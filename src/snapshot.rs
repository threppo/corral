//! Corral-only snapshot storage; no live restore.
//! Caller supplies an existing private directory, Unix-millisecond timestamp and local day
//! ordinal (days since 0001-01-01). No clock, default path or app state is consulted.
//! Daily payloads and rotation metadata share one atomic publication, avoiding a two-file commit.
//! One writer at a time: callers must serialize store mutations (including across processes).
#![allow(dead_code)]

use crate::{
    model::{LAYOUT_VERSION, Layout},
    settings::Settings,
};
use serde::{Deserialize, Serialize};
use std::os::windows::{
    fs::{MetadataExt, OpenOptionsExt},
    io::AsRawHandle,
};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
};
use windows::{
    Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle, MOVEFILE_REPLACE_EXISTING,
            MOVEFILE_WRITE_THROUGH, MoveFileExW,
        },
        System::Com::CoCreateGuid,
    },
    core::PCWSTR,
};

pub const VERSION: u32 = 1;
/// Longest age of a settings-file request (its nonce is a Unix-ms timestamp) that the app still
/// runs: the settings window's longest wait (60 s) plus a margin.
pub const REQUEST_MAX_AGE_MS: u64 = 90_000;
pub const DAILY_SLOTS: u8 = 7;
const RING_FILE: &str = "daily-ring.json";
const JOURNAL_FILE: &str = "restore-journal.json";
const LAST_ERROR_FILE: &str = "last-error.json";
const REPARSE: u32 = 0x400;
const OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const BACKUP_SEMANTICS: u32 = 0x0200_0000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Kind {
    Manual,
    /// Safeguard taken by Corral itself before a risky change, e.g. an applied "Sort the desktop
    /// now" (round-up-spec 8, question 3). Stored and managed like a manual backup.
    Automatic,
    Daily { slot: u8 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub version: u32,
    pub id: String,
    pub title: String,
    pub created_ms: u64,
    pub kind: Kind,
    pub layout: Layout,
    pub settings: Settings,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RingState {
    pub day: u32,
    pub slot: u8,
}

/// Initial slot 0 advances to 1; same day overwrites, missed days advance once.
/// Clock rollback is treated as a changed captured day.
pub fn daily_decision(previous: RingState, day: u32) -> io::Result<RingState> {
    if previous.slot >= DAILY_SLOTS {
        return Err(invalid("invalid daily slot"));
    }
    Ok(RingState {
        day,
        slot: if previous.day == day {
            previous.slot
        } else {
            (previous.slot + 1) % DAILY_SLOTS
        },
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Journal {
    pub version: u32,
    pub target_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LastError {
    message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ring {
    version: u32,
    state: RingState,
    entries: [Option<Snapshot>; DAILY_SLOTS as usize],
}
impl Default for Ring {
    fn default() -> Self {
        Self {
            version: VERSION,
            state: RingState::default(),
            entries: Default::default(),
        }
    }
}

/// One backup action requested by the settings window. Corral protocol: the
/// settings process never mutates the store (one writer at a time, module notes); it writes a
/// `Request` into the settings file and the running app executes it on its own thread.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    /// "Save backup": `title` None = automatic name; Some = the name from the naming dialog.
    Capture { title: Option<String> },
    /// "Re-capture...": same id and title, current data and time.
    Recapture { id: String },
    /// "Rename...": metadata only, the id stays.
    Rename { id: String, title: String },
    /// "Delete Backup", after the settings window's confirmation.
    Delete { id: String },
    /// "Restore Backup": `backup_first` = "Take a new backup before restoring"; if that
    /// capture fails the restore must not run.
    Restore { id: String, backup_first: bool },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    /// Chosen by the settings window (e.g. a millisecond timestamp); echoed in the result.
    pub nonce: u64,
    #[serde(flatten)]
    pub action: Action,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestResult {
    pub nonce: u64,
    pub ok: bool,
    /// English text for the settings window, e.g. "Backup captured" / "Backup deleted" or the
    /// error texts of the backup dialogs.
    pub message: String,
    /// The snapshot the action produced or changed, if any.
    pub id: Option<String>,
}

/// The snapshot store of a layout: `<layout folder>\backups`. Desktop mode therefore uses
/// `%APPDATA%\Corral\backups`; a preview with a QA `--layout` gets a scratch store next to it,
/// never the desktop store. The caller creates the directory before `Store::open`.
pub fn store_dir_for(layout_path: &Path) -> PathBuf {
    layout_path.parent().unwrap_or_else(|| Path::new(".")).join("backups")
}

/// The settings to put into a snapshot: without the request/result fields, so a restore can't
/// replay a backup action.
pub fn for_capture(settings: &Settings) -> Settings {
    let mut s = settings.clone();
    s.snapshot_request = None;
    s.snapshot_result = None;
    s.style_request = None;
    s.style_result = None;
    s.desk_setup_request = None;
    s.desk_setup_result = None;
    s
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn check_id(id: &str) -> io::Result<()> {
    if valid_id(id) {
        Ok(())
    } else {
        Err(invalid("invalid snapshot id"))
    }
}
fn check_title(title: &str) -> io::Result<()> {
    // Approved Corral redesign: up to 255 Unicode characters; preserve the entered title.
    if title.trim().is_empty() || title.chars().count() > 255 {
        Err(invalid("invalid snapshot title"))
    } else {
        Ok(())
    }
}
fn generated_id() -> io::Result<String> {
    let guid = unsafe { CoCreateGuid() }.map_err(io::Error::other)?;
    Ok(format!("{guid:?}")
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .collect::<String>()
        .to_ascii_lowercase())
}
impl Snapshot {
    pub fn validate(&self) -> io::Result<()> {
        check_id(&self.id)?;
        check_title(&self.title)?;
        if self.version != VERSION || self.layout.version != LAYOUT_VERSION {
            return Err(invalid("unsupported snapshot or layout version"));
        }
        if matches!(self.kind, Kind::Daily { slot } if slot >= DAILY_SLOTS) {
            return Err(invalid("invalid daily slot"));
        }
        Ok(())
    }
}

/// Explicit existing directory only. Ancestor handles deny rename/deletion for the lifetime
/// of the store, and OPEN_REPARSE_POINT inspects links themselves, never their targets.
/// Reject all reparse points, including junctions and cloud placeholders; do not canonicalize
/// first (canonicalization would hide the very link that must be rejected).
pub struct Store {
    directory: PathBuf,
    _ancestors: Vec<File>,
}
impl Store {
    pub fn open(directory: &Path) -> io::Result<Self> {
        if !directory.is_absolute()
            || directory
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
        {
            return Err(invalid(
                "store requires an absolute path without dot components",
            ));
        }
        // Only ordinary drive paths. No device namespace, UNC, ADS or prefix aliases.
        let text = directory
            .to_str()
            .ok_or_else(|| invalid("non-Unicode store path"))?;
        let bytes = text.as_bytes();
        if bytes.len() < 3
            || !bytes[0].is_ascii_alphabetic()
            || bytes[1] != b':'
            || !matches!(bytes[2], b'\\' | b'/')
            || text[2..].contains(':')
            || text.contains('/')
            || text.split('\\').skip(1).any(|part| {
                part.ends_with('.') || part.ends_with(' ') || part == ".." || part == "."
            })
        {
            return Err(invalid("unsupported store path"));
        }
        let mut ancestors = Vec::new();
        let mut path = PathBuf::new();
        for component in directory.components() {
            path.push(component);
            if !path.has_root() {
                continue;
            }
            let handle = OpenOptions::new()
                .read(true)
                .share_mode(3)
                .custom_flags(OPEN_REPARSE_POINT | BACKUP_SEMANTICS)
                .open(&path)?;
            let metadata = handle.metadata()?;
            if !metadata.is_dir() || metadata.file_attributes() & REPARSE != 0 {
                return Err(invalid("reparse or non-directory store ancestor"));
            }
            ancestors.push(handle);
        }
        Ok(Self {
            directory: directory.to_owned(),
            _ancestors: ancestors,
        })
    }

    fn manual_name(id: &str) -> io::Result<String> {
        check_id(id)?;
        Ok(format!("manual-{id}.json"))
    }
    fn read<T: for<'de> Deserialize<'de>>(&self, name: &str) -> io::Result<T> {
        let mut file = self.open_regular(name)?;
        let mut text = String::new();
        file.read_to_string(&mut text)?;
        serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(io::Error::other)
    }
    fn open_regular(&self, name: &str) -> io::Result<File> {
        let file = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .custom_flags(OPEN_REPARSE_POINT)
            .open(self.directory.join(name))?;
        check_regular(&file)?;
        Ok(file)
    }
    fn ring(&self) -> io::Result<Ring> {
        let ring: Ring = match self.read(RING_FILE) {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ring::default(),
            Err(error) => return Err(error),
        };
        if ring.version != VERSION || ring.state.slot >= DAILY_SLOTS {
            return Err(invalid("unsupported ring version or slot"));
        }
        let mut ids = std::collections::BTreeSet::new();
        for (slot, entry) in ring.entries.iter().enumerate() {
            if let Some(snapshot) = entry {
                snapshot.validate()?;
                if snapshot.kind != (Kind::Daily { slot: slot as u8 }) || !ids.insert(&snapshot.id)
                {
                    return Err(invalid("inconsistent daily ring"));
                }
            }
        }
        Ok(ring)
    }

    /// Atomic replacement, no copy into .bak and no truncation of an existing file.
    /// The random temporary name is create_new; only that owned file is cleaned up on error.
    fn publish<T: Serialize>(&self, name: &str, value: &T) -> io::Result<()> {
        let data = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
        let destination = self.directory.join(name);
        match self.open_regular(name) {
            Ok(file) => drop(file),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
        }
        let temporary = self
            .directory
            .join(format!("pending-{}.tmp", generated_id()?));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(0)
            .open(&temporary)?;
        let result = file.write_all(&data).and_then(|_| file.sync_all());
        drop(file);
        let result = result.and_then(|_| {
            use std::os::windows::ffi::OsStrExt;
            let source: Vec<u16> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
            let target: Vec<u16> = destination
                .as_os_str()
                .encode_wide()
                .chain(Some(0))
                .collect();
            // No COPY_ALLOWED: this is a same-directory rename. Replacing a link replaces
            // the directory entry, never follows it; ancestor handles prevent path redirection.
            unsafe {
                MoveFileExW(
                    PCWSTR(source.as_ptr()),
                    PCWSTR(target.as_ptr()),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            }
            .map_err(io::Error::other)
        });
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    pub fn capture(
        &self,
        title: &str,
        created_ms: u64,
        layout: &Layout,
        settings: &Settings,
    ) -> io::Result<Snapshot> {
        let snapshot = Snapshot {
            version: VERSION,
            id: generated_id()?,
            title: title.into(),
            created_ms,
            kind: Kind::Manual,
            layout: layout.clone(),
            settings: settings.clone(),
        };
        snapshot.validate()?;
        self.publish(&Self::manual_name(&snapshot.id)?, &snapshot)?;
        Ok(snapshot)
    }
    /// Round-up-spec 4: a durable safeguard that exists before membership changes are applied.
    pub fn capture_automatic(
        &self,
        title: &str,
        created_ms: u64,
        layout: &Layout,
        settings: &Settings,
    ) -> io::Result<Snapshot> {
        let snapshot = Snapshot {
            version: VERSION,
            id: generated_id()?,
            title: title.into(),
            created_ms,
            kind: Kind::Automatic,
            layout: layout.clone(),
            settings: for_capture(settings),
        };
        snapshot.validate()?;
        self.publish(&Self::manual_name(&snapshot.id)?, &snapshot)?;
        Ok(snapshot)
    }
    /// Keep the `keep` newest Automatic snapshots and delete the older ones (this kind only; manual
    /// and daily backups are never touched). Returns how many were deleted.
    pub fn prune_automatic(&self, keep: usize) -> io::Result<usize> {
        let old: Vec<String> = self
            .list()?
            .into_iter()
            .filter(|s| s.kind == Kind::Automatic)
            .skip(keep)
            .map(|s| s.id)
            .collect();
        for id in &old {
            self.delete(id)?;
        }
        Ok(old.len())
    }
    pub fn capture_daily(
        &self,
        day: u32,
        title: &str,
        created_ms: u64,
        layout: &Layout,
        settings: &Settings,
    ) -> io::Result<Snapshot> {
        let mut ring = self.ring()?;
        let state = daily_decision(ring.state, day)?;
        let id = match &ring.entries[state.slot as usize] {
            Some(old) => old.id.clone(),
            None => generated_id()?,
        };
        let snapshot = Snapshot {
            version: VERSION,
            id,
            title: title.into(),
            created_ms,
            kind: Kind::Daily { slot: state.slot },
            layout: layout.clone(),
            settings: settings.clone(),
        };
        snapshot.validate()?;
        ring.entries[state.slot as usize] = Some(snapshot.clone());
        ring.state = state;
        self.publish(RING_FILE, &ring)?;
        Ok(snapshot)
    }
    /// Newest first, stable ID tie-breaker. Invalid/foreign files are omitted, not modified.
    /// A corrupt ring is omitted in listing, but mutation/load reports its error.
    pub fn list(&self) -> io::Result<Vec<Snapshot>> {
        let mut snapshots = Vec::new();
        for entry in fs::read_dir(&self.directory)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(id) = name
                .strip_prefix("manual-")
                .and_then(|n| n.strip_suffix(".json"))
            else {
                continue;
            };
            if !valid_id(id) {
                continue;
            }
            if let Ok(snapshot) = self.read::<Snapshot>(name) {
                if snapshot.validate().is_ok()
                    && matches!(snapshot.kind, Kind::Manual | Kind::Automatic)
                    && snapshot.id == id
                {
                    snapshots.push(snapshot);
                }
            }
        }
        if let Ok(ring) = self.ring() {
            snapshots.extend(ring.entries.into_iter().flatten());
        }
        snapshots.sort_by(|a, b| {
            b.created_ms
                .cmp(&a.created_ms)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(snapshots)
    }
    pub fn load(&self, id: &str) -> io::Result<Snapshot> {
        let name = Self::manual_name(id)?;
        match self.read::<Snapshot>(&name) {
            Ok(snapshot) => {
                snapshot.validate()?;
                if snapshot.id != id || !matches!(snapshot.kind, Kind::Manual | Kind::Automatic) {
                    return Err(invalid("snapshot identity mismatch"));
                }
                Ok(snapshot)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => self
                .ring()?
                .entries
                .into_iter()
                .flatten()
                .find(|s| s.id == id)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "snapshot not found")),
            Err(error) => Err(error),
        }
    }
    /// Metadata only: keeps ID, kind, timestamp and both complete payloads.
    pub fn rename(&self, id: &str, title: &str) -> io::Result<Snapshot> {
        check_title(title)?;
        let mut snapshot = self.load(id)?;
        snapshot.title = title.into();
        match snapshot.kind {
            Kind::Manual | Kind::Automatic => self.publish(&Self::manual_name(id)?, &snapshot)?,
            Kind::Daily { slot } => {
                let mut ring = self.ring()?;
                ring.entries[slot as usize] = Some(snapshot.clone());
                self.publish(RING_FILE, &ring)?;
            }
        }
        Ok(snapshot)
    }
    /// "Re-capture..." (section 4.1): same id, title, kind and daily slot; new payload and time.
    pub fn recapture(
        &self,
        id: &str,
        created_ms: u64,
        layout: &Layout,
        settings: &Settings,
    ) -> io::Result<Snapshot> {
        let mut snapshot = self.load(id)?;
        snapshot.created_ms = created_ms;
        snapshot.layout = layout.clone();
        snapshot.settings = settings.clone();
        snapshot.validate()?;
        match snapshot.kind {
            Kind::Manual | Kind::Automatic => self.publish(&Self::manual_name(id)?, &snapshot)?,
            Kind::Daily { slot } => {
                let mut ring = self.ring()?;
                ring.entries[slot as usize] = Some(snapshot.clone());
                self.publish(RING_FILE, &ring)?;
            }
        }
        Ok(snapshot)
    }

    /// Restore recovery journal: written (atomically) before the
    /// layout/settings pair is replaced, deleted after both are in place. A journal found at
    /// start-up means the pair may be half replaced; the target is re-applied (forward recovery).
    pub fn write_journal(&self, target_id: &str) -> io::Result<()> {
        check_id(target_id)?;
        self.publish(
            JOURNAL_FILE,
            &Journal {
                version: VERSION,
                target_id: target_id.into(),
            },
        )
    }
    /// `Ok(None)` = no journal. A present but unreadable journal is an error (caller logs it).
    pub fn read_journal(&self) -> io::Result<Option<Journal>> {
        match self.read::<Journal>(JOURNAL_FILE) {
            Ok(journal) => {
                check_id(&journal.target_id)?;
                if journal.version != VERSION {
                    return Err(invalid("unsupported journal version"));
                }
                Ok(Some(journal))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }
    /// Removes exactly the journal file (a regular, non-reparse file); absent is fine.
    pub fn clear_journal(&self) -> io::Result<()> {
        match self.open_regular(JOURNAL_FILE) {
            Ok(file) => drop(file),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        }
        fs::remove_file(self.directory.join(JOURNAL_FILE))
    }

    /// The last failed automatic (daily) capture, for the settings window; `None` clears it.
    pub fn set_last_error(&self, message: Option<&str>) -> io::Result<()> {
        match message {
            Some(message) => self.publish(
                LAST_ERROR_FILE,
                &LastError {
                    message: message.into(),
                },
            ),
            None => match self.open_regular(LAST_ERROR_FILE) {
                Ok(file) => {
                    drop(file);
                    fs::remove_file(self.directory.join(LAST_ERROR_FILE))
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error),
            },
        }
    }
    pub fn last_error(&self) -> Option<String> {
        self.read::<LastError>(LAST_ERROR_FILE).ok().map(|e| e.message)
    }
    pub fn delete(&self, id: &str) -> io::Result<()> {
        let snapshot = self.load(id)?;
        match snapshot.kind {
            Kind::Manual | Kind::Automatic => {
                let name = Self::manual_name(id)?;
                // Validate immediately before unlinking; never recurse, derive paths from
                // payload references/titles, or remove an image companion.
                drop(self.open_regular(&name)?);
                fs::remove_file(self.directory.join(name))
            }
            Kind::Daily { slot } => {
                let mut ring = self.ring()?;
                ring.entries[slot as usize] = None;
                self.publish(RING_FILE, &ring)
            }
        }
    }
}

fn check_regular(file: &File) -> io::Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.file_attributes() & REPARSE != 0 {
        return Err(invalid("snapshot is not a regular non-reparse file"));
    }
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }
        .map_err(io::Error::other)?;
    if info.nNumberOfLinks != 1 {
        return Err(invalid("snapshot hard links are rejected"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trips_through_settings_and_is_dropped_from_captures() {
        let mut s = Settings::default();
        s.snapshot_request = Some(Request { nonce: 7, action: Action::Restore { id: "a".repeat(32), backup_first: true } });
        s.snapshot_result = Some(RequestResult { nonce: 6, ok: true, message: "Backup captured".into(), id: None });
        let text = serde_json::to_string(&s).unwrap();
        assert!(text.contains(r#""SnapshotRequest":{"nonce":7,"action":"restore""#), "{text}");
        assert_eq!(serde_json::from_str::<Settings>(&text).unwrap(), s);
        let captured = for_capture(&s);
        assert!(captured.snapshot_request.is_none() && captured.snapshot_result.is_none());
        let plain = serde_json::to_string(&Settings::default()).unwrap();
        assert!(!plain.contains("SnapshotRequest") && !plain.contains("SnapshotResult"));
    }

    #[test]
    fn store_dir_is_next_to_the_layout() {
        assert_eq!(store_dir_for(Path::new(r"C:\a\Corral\layout.json")), PathBuf::from(r"C:\a\Corral\backups"));
    }

    struct Scratch {
        root: PathBuf,
        directory: PathBuf,
    }
    impl Scratch {
        fn new() -> Self {
            let root = std::env::current_dir()
                .unwrap()
                .join("target\\qa\\snapshot-tests");
            fs::create_dir_all(&root).unwrap();
            let directory = root.join(format!("test-{}", generated_id().unwrap()));
            fs::create_dir(&directory).unwrap();
            Self {
                root: fs::canonicalize(root).unwrap(),
                directory,
            }
        }
        fn store(&self) -> Store {
            Store::open(&self.directory).unwrap()
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            // Only this test's unique scratch directory. Check resolved absolute boundary
            // before recursive cleanup; never traverse a linked root or use a shell delete.
            let resolved = fs::canonicalize(&self.directory).unwrap();
            assert_eq!(resolved.parent(), Some(self.root.as_path()));
            assert_eq!(
                fs::symlink_metadata(&self.directory)
                    .unwrap()
                    .file_attributes()
                    & REPARSE,
                0
            );
            fs::remove_dir_all(&self.directory).unwrap();
        }
    }
    fn capture(store: &Store, time: u64) -> Snapshot {
        store
            .capture("Backup", time, &Layout::new(), &Settings::default())
            .unwrap()
    }
    fn daily(store: &Store, day: u32, time: u64) -> Snapshot {
        store
            .capture_daily(
                day,
                "Daily Backup",
                time,
                &Layout::new(),
                &Settings::default(),
            )
            .unwrap()
    }

    #[test]
    fn automatic_backup_is_listed_loaded_renamed_recaptured_and_deleted_like_a_manual_one() {
        let scratch = Scratch::new();
        let store = scratch.store();
        let mut settings = Settings::default();
        settings.snapshot_request = Some(Request {
            nonce: 6,
            action: Action::Capture { title: None },
        });
        let layout = Layout::new();
        let backup = store
            .capture_automatic("Before Round-up", 99, &layout, &settings)
            .unwrap();
        assert_eq!(backup.kind, Kind::Automatic);
        assert!(backup.settings.snapshot_request.is_none());
        assert_eq!(store.load(&backup.id).unwrap(), backup);
        assert_eq!(store.list().unwrap(), vec![backup.clone()]);
        let renamed = store.rename(&backup.id, "Before rearranging").unwrap();
        assert_eq!(renamed.kind, Kind::Automatic);
        let again = store.recapture(&backup.id, 123, &layout, &settings).unwrap();
        assert_eq!((again.kind, again.created_ms), (Kind::Automatic, 123));
        store.delete(&backup.id).unwrap();
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn only_the_newest_automatic_backups_are_kept_and_other_kinds_are_left_alone() {
        let scratch = Scratch::new();
        let store = scratch.store();
        let layout = Layout::new();
        let settings = Settings::default();
        let manual = capture(&store, 1);
        let daily_one = daily(&store, 100, 2);
        let automatic: Vec<_> = (10..18)
            .map(|t| store.capture_automatic("Before Round-up", t, &layout, &settings).unwrap().id)
            .collect();
        assert_eq!(store.prune_automatic(5).unwrap(), 3);
        let left: Vec<_> = store.list().unwrap().into_iter().map(|s| s.id).collect();
        for id in &automatic[..3] {
            assert!(!left.contains(id), "oldest automatic backups are gone");
        }
        for id in &automatic[3..] {
            assert!(left.contains(id));
        }
        assert!(left.contains(&manual.id) && left.contains(&daily_one.id));
        assert_eq!(store.prune_automatic(5).unwrap(), 0);
    }

    #[test]
    fn a_snapshot_from_before_round_up_restores_with_its_rules_converted_and_kept() {
        use crate::settings as settings_store;
        let scratch = Scratch::new();
        let store = scratch.store();
        let made = capture(&store, 5);
        // Rewrite the payload the way an older build wrote it: no RoundUp block, an old Rules list.
        let path = scratch.directory.join(Store::manual_name(&made.id).unwrap());
        let mut value: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let settings = value["settings"].as_object_mut().unwrap();
        settings.remove("RoundUp");
        settings.insert(
            "Rules".into(),
            serde_json::json!([
                {"Data": "Invoices|<type>:=:0:pdf|", "Target": "1|3|1|0"},
                {"Data": "Names|<name>:=:0:x|", "Target": "1|3|1|0"}
            ]),
        );
        fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();
        let loaded = store.load(&made.id).unwrap();
        let block = &loaded.settings.round_up;
        assert_eq!(block.rules[0].name, "Invoices");
        assert_eq!(block.needs_review.len(), 1);
        assert!(block.active(), "a restored snapshot must not leave Round-up waiting for a migration");
        // what a restore writes
        let target = scratch.directory.join("restored-settings.json");
        settings_store::save(&target, &loaded.settings).unwrap();
        let written: serde_json::Value = serde_json::from_str(&fs::read_to_string(&target).unwrap()).unwrap();
        assert_eq!(written["RoundUp"]["rules"][0]["name"], "Invoices");
        assert_eq!(written["RoundUp"]["needs_review"].as_array().unwrap().len(), 1);
        assert_eq!(written["Rules"].as_array().unwrap().len(), 2, "the old rules stay while review items exist");
    }

    #[test]
    fn full_payload_roundtrip_without_referenced_file_access() {
        let scratch = Scratch::new();
        let store = scratch.store();
        let mut layout = Layout::new();
        // Populate through JSON so the fixture remains a complete model payload, not a
        // second manually maintained snapshot schema.
        layout.groups = serde_json::from_value(serde_json::json!([{
            "id": 17, "title": "Portal", "monitor": "screen", "rect": {"left": 10, "top": 20, "right": 310, "bottom": 220},
            "content": {"kind": "portal", "path": "Z:\\nonexistent\\portal"},
            "nav_history": ["Z:\\nonexistent\\portal", "Z:\\older"], "view_flags": 1,
            "swipe_index": 2, "all_pages": true, "style": {"color": 123, "font_name": "Test font"},
            "roll": "Manual", "scroll": 19, "opacity": 51
        }])).unwrap();
        layout.icon_pages.insert("z:\\missing.txt".into(), 3);
        layout.loose.insert(
            "z:\\missing.txt".into(),
            crate::model::LoosePosition {
                monitor: "screen".into(),
                x: 40,
                y: 60,
            },
        );
        layout.view_states.insert(
            "z:\\nonexistent\\portal".into(),
            crate::view_state::ViewState {
                view_mode: 4,
                icon_size: 32,
                sort_mode: -2,
                view_flags: 34,
                set: true,
                col_info: Some(vec![crate::view_state::Column {
                    fmtid: crate::view_state::NAME_FMTID.into(),
                    pid: 10,
                    width: 123,
                }]),
                ..Default::default()
            },
        );
        layout.mark_seen("Z:\\missing.txt");
        let mut settings = Settings::default();
        settings.color = 789;
        settings.rules = serde_json::from_value(
            serde_json::json!([{"Data": "Text|2:0:0:*.txt|", "Target": "1|17|1|0"}]),
        )
        .unwrap();
        let snapshot = store
            .capture(" Complete backup ", 500, &layout, &settings)
            .unwrap();
        assert_eq!(store.load(&snapshot.id).unwrap(), snapshot);
        assert_eq!(snapshot.layout, layout);
        assert_eq!(snapshot.settings, settings);
        assert_eq!(store.list().unwrap(), vec![snapshot]);
    }

    #[test]
    fn generated_ids_are_unique_and_safe() {
        let scratch = Scratch::new();
        let store = scratch.store();
        let a = capture(&store, 1);
        let b = capture(&store, 1);
        assert!(valid_id(&a.id));
        assert!(valid_id(&b.id));
        assert_ne!(a.id, b.id);
    }
    #[test]
    fn daily_pure_decision_same_day_wrap_missed_days_and_rollback() {
        assert_eq!(
            daily_decision(RingState::default(), 100).unwrap(),
            RingState { day: 100, slot: 1 }
        );
        assert_eq!(
            daily_decision(RingState { day: 100, slot: 1 }, 100)
                .unwrap()
                .slot,
            1
        );
        assert_eq!(
            daily_decision(RingState { day: 100, slot: 6 }, 500)
                .unwrap()
                .slot,
            0
        );
        assert_eq!(
            daily_decision(RingState { day: 100, slot: 1 }, 99)
                .unwrap()
                .slot,
            2
        );
        assert!(daily_decision(RingState { day: 100, slot: 7 }, 101).is_err());
    }
    #[test]
    fn daily_overwrites_same_slot_persists_wraps_and_does_not_backfill() {
        let scratch = Scratch::new();
        let first;
        {
            let store = scratch.store();
            first = daily(&store, 100, 1);
        }
        let store = scratch.store();
        let same = daily(&store, 100, 2);
        assert_eq!(same.id, first.id);
        assert_eq!(store.list().unwrap().len(), 1);
        assert_eq!(daily(&store, 200, 3).kind, Kind::Daily { slot: 2 });
        for day in 201..207 {
            daily(&store, day, day as u64);
        }
        assert_eq!(store.list().unwrap().len(), 7);
        assert_eq!(store.ring().unwrap().state, RingState { day: 206, slot: 1 });
        assert_eq!(store.load(&first.id).unwrap().created_ms, 206);
    }
    #[test]
    fn manual_snapshots_are_not_pruned_by_daily_ring() {
        let scratch = Scratch::new();
        let store = scratch.store();
        let manual = capture(&store, 1);
        for day in 100..110 {
            daily(&store, day, day as u64);
        }
        assert_eq!(store.list().unwrap().len(), 8);
        assert_eq!(store.load(&manual.id).unwrap(), manual);
    }
    #[test]
    fn rename_preserves_identity_timestamp_kind_and_payload() {
        let scratch = Scratch::new();
        let store = scratch.store();
        for mut before in [capture(&store, 1), daily(&store, 100, 2)] {
            let after = store.rename(&before.id, " New title ").unwrap();
            before.title = " New title ".into();
            assert_eq!(after, before);
            assert_eq!(store.load(&before.id).unwrap(), before);
        }
    }
    #[test]
    fn title_validation_matches_corral_character_limit_and_preserves_raw_title() {
        let scratch = Scratch::new();
        let store = scratch.store();
        for title in [
            "".to_owned(),
            " \t\n".into(),
            "a".repeat(256),
            "😀".repeat(256),
        ] {
            assert!(
                store
                    .capture(&title, 1, &Layout::new(), &Settings::default())
                    .is_err()
            );
        }
        let snapshot = store
            .capture(&"😀".repeat(255), 1, &Layout::new(), &Settings::default())
            .unwrap();
        assert!(store.rename(&snapshot.id, " ").is_err());
        assert_eq!(store.load(&snapshot.id).unwrap(), snapshot);
    }
    #[test]
    fn list_is_sorted_and_ignores_foreign_corrupt_and_mismatched_files() {
        let scratch = Scratch::new();
        let store = scratch.store();
        let old = capture(&store, 1);
        let new = capture(&store, 4);
        let middle = daily(&store, 100, 2);
        fs::write(scratch.directory.join("foreign.json"), "foreign data").unwrap();
        fs::write(
            scratch
                .directory
                .join("manual-00000000000000000000000000000000.json"),
            "{",
        )
        .unwrap();
        fs::write(
            scratch
                .directory
                .join("manual-11111111111111111111111111111111.json"),
            serde_json::to_vec(&old).unwrap(),
        )
        .unwrap();
        fs::write(scratch.directory.join("pending-unused.tmp"), "incomplete").unwrap();
        assert_eq!(store.list().unwrap(), vec![new, middle, old]);
        assert!(scratch.directory.join("foreign.json").exists());
    }
    #[test]
    fn rejects_corruption_version_mismatch_and_missing_payload() {
        let scratch = Scratch::new();
        let store = scratch.store();
        let snapshot = capture(&store, 1);
        let path = scratch
            .directory
            .join(Store::manual_name(&snapshot.id).unwrap());
        for change in ["version", "layout_version", "settings", "id"] {
            let mut value = serde_json::to_value(&snapshot).unwrap();
            match change {
                "version" => value["version"] = 999.into(),
                "layout_version" => value["layout"]["version"] = 999.into(),
                "settings" => {
                    value.as_object_mut().unwrap().remove("settings");
                }
                _ => value["id"] = "../../outside".into(),
            }
            fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
            assert!(store.load(&snapshot.id).is_err());
            assert!(store.list().unwrap().is_empty());
        }
        fs::write(&path, "{").unwrap();
        assert!(store.load(&snapshot.id).is_err());
    }
    #[test]
    fn rejects_path_escape_for_all_id_operations_and_store_paths() {
        let scratch = Scratch::new();
        let store = scratch.store();
        for id in [
            "..",
            "../x",
            "a/b",
            "a\\b",
            "C:\\outside",
            "\\\\server\\share",
            "x:stream",
            "",
            "CON",
            "a.",
        ] {
            assert!(store.load(id).is_err());
            assert!(store.rename(id, "Name").is_err());
            assert!(store.delete(id).is_err());
        }
        for path in [
            PathBuf::from("relative"),
            scratch.directory.join(".."),
            scratch.directory.join("x:stream"),
            scratch.directory.join("x."),
        ] {
            assert!(Store::open(&path).is_err());
        }
        assert!(store.list().unwrap().is_empty());
    }
    #[test]
    fn deletes_only_selected_manual_or_daily_payload() {
        let scratch = Scratch::new();
        let store = scratch.store();
        let manual = capture(&store, 1);
        let automatic = daily(&store, 100, 2);
        fs::write(scratch.directory.join("unrelated.txt"), "keep").unwrap();
        store.delete(&manual.id).unwrap();
        assert!(store.load(&manual.id).is_err());
        let state = store.ring().unwrap().state;
        store.delete(&automatic.id).unwrap();
        assert!(store.load(&automatic.id).is_err());
        assert_eq!(store.ring().unwrap().state, state);
        assert_eq!(
            fs::read_to_string(scratch.directory.join("unrelated.txt")).unwrap(),
            "keep"
        );
        assert!(store.list().unwrap().is_empty());
    }
    #[test]
    fn failed_replacement_preserves_manual_payload_and_cleans_owned_temp() {
        let scratch = Scratch::new();
        let store = scratch.store();
        let snapshot = capture(&store, 1);
        let locked = store
            .open_regular(&Store::manual_name(&snapshot.id).unwrap())
            .unwrap();
        // A real Windows sharing failure at the publication boundary, after temp is flushed.
        assert!(store.rename(&snapshot.id, "Replacement").is_err());
        drop(locked);
        assert_eq!(store.load(&snapshot.id).unwrap(), snapshot);
        assert_eq!(fs::read_dir(&scratch.directory).unwrap().count(), 1);
    }
    #[test]
    fn failed_daily_publication_preserves_payload_and_rotation_metadata() {
        let scratch = Scratch::new();
        let store = scratch.store();
        let snapshot = daily(&store, 100, 1);
        let bytes = fs::read(scratch.directory.join(RING_FILE)).unwrap();
        let locked = store.open_regular(RING_FILE).unwrap();
        assert!(
            store
                .capture_daily(101, "New day", 2, &Layout::new(), &Settings::default())
                .is_err()
        );
        drop(locked);
        assert_eq!(fs::read(scratch.directory.join(RING_FILE)).unwrap(), bytes);
        assert_eq!(store.load(&snapshot.id).unwrap(), snapshot);
        assert_eq!(daily(&store, 101, 3).kind, Kind::Daily { slot: 2 });
        assert_eq!(fs::read_dir(&scratch.directory).unwrap().count(), 1);
    }
    #[test]
    fn corrupt_ring_never_resets_or_overwrites_rotation() {
        let scratch = Scratch::new();
        let store = scratch.store();
        capture(&store, 1);
        fs::write(scratch.directory.join(RING_FILE), "bad ring").unwrap();
        assert_eq!(store.list().unwrap().len(), 1);
        assert!(
            store
                .capture_daily(100, "Daily", 1, &Layout::new(), &Settings::default())
                .is_err()
        );
        assert_eq!(
            fs::read_to_string(scratch.directory.join(RING_FILE)).unwrap(),
            "bad ring"
        );
    }
    #[test]
    fn hardlinked_snapshot_is_never_read_renamed_or_deleted() {
        let scratch = Scratch::new();
        let store = scratch.store();
        let snapshot = capture(&store, 1);
        let path = scratch
            .directory
            .join(Store::manual_name(&snapshot.id).unwrap());
        let alias = scratch.directory.join("protected.txt");
        fs::hard_link(&path, &alias).unwrap();
        let bytes = fs::read(&alias).unwrap();
        assert!(store.load(&snapshot.id).is_err());
        assert!(store.rename(&snapshot.id, "New").is_err());
        assert!(store.delete(&snapshot.id).is_err());
        assert!(store.list().unwrap().is_empty());
        assert_eq!(fs::read(alias).unwrap(), bytes);
    }
    #[test]
    fn junction_store_and_linked_snapshot_are_rejected() {
        let scratch = Scratch::new();
        let target = scratch.directory.join("target");
        fs::create_dir(&target).unwrap();
        let junction = scratch.directory.join("junction");
        // Junction creation needs no symlink privilege. All paths are this test's scratch.
        let output = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&junction)
            .arg(&target)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(Store::open(&junction).is_err());
        let nested = junction.join("nested");
        fs::create_dir(&nested).unwrap();
        assert!(Store::open(&nested).is_err());
        let store = scratch.store();
        let id = "00000000000000000000000000000000";
        let link = scratch.directory.join(Store::manual_name(id).unwrap());
        let output = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(store.load(id).is_err());
        assert!(store.delete(id).is_err());
        assert!(store.list().unwrap().is_empty());
        drop(store);
        // Remove only junction entries (nonrecursive); the target remains intact.
        fs::remove_dir(&link).unwrap();
        fs::remove_dir(&junction).unwrap();
        assert!(target.exists());
    }
    #[test]
    fn ancestor_handles_prevent_store_redirection() {
        let scratch = Scratch::new();
        let store = scratch.store();
        assert!(
            fs::rename(
                &scratch.directory,
                scratch.directory.with_extension("moved")
            )
            .is_err()
        );
        let snapshot = capture(&store, 1);
        assert_eq!(store.load(&snapshot.id).unwrap(), snapshot);
    }
}
