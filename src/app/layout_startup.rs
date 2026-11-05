//! Preserve unreadable startup files before the empty in-memory layout can be saved.
use crate::model::{self, Layout, LoadError};
use std::{
    fs, io,
    path::{Path, PathBuf},
    time::Duration,
};
use windows::{Win32::Storage::FileSystem::MoveFileW, core::HSTRING};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Loaded,
    FirstRun,
    Temporary,
    Preserve { primary: bool, backup: bool },
}

fn temporary(result: &Result<Layout, LoadError>) -> bool {
    matches!(result, Err(LoadError::Io(e)) if e.kind() != io::ErrorKind::NotFound)
}

fn corrupt(result: &Result<Layout, LoadError>) -> bool {
    matches!(
        result,
        Err(LoadError::Parse(_) | LoadError::UnsupportedVersion(_) | LoadError::InvalidTabs(_))
    )
}

/// A good file wins; otherwise neither file may be moved until both could be read
/// (or are known missing). An I/O error is never evidence of corrupt content.
fn decision(primary: &Result<Layout, LoadError>, backup: &Result<Layout, LoadError>) -> Action {
    if primary.is_ok() || backup.is_ok() {
        Action::Loaded
    } else if temporary(primary) || temporary(backup) {
        Action::Temporary
    } else if corrupt(primary) || corrupt(backup) {
        Action::Preserve {
            primary: corrupt(primary),
            backup: corrupt(backup),
        }
    } else {
        Action::FirstRun
    }
}

const RETRIES: usize = 3;
const RETRY_WAIT: Duration = Duration::from_millis(300);
const TEMPORARY_NOTICE: &str = "The layout could not be opened right now. The layout files have not been changed. Nothing will be saved unless a backup is successfully restored from Settings. Restart Corral to try opening the layout again.";

#[derive(Default)]
pub(super) struct Recovery {
    pub saving_disabled: bool,
    pub notice: Option<String>,
    pub first_run: super::first_run::Offer,
}

impl Recovery {
    /// Only a completed restore proves the replacement pair is good. Drop the
    /// pending startup notice too, in case its posted message has not run yet.
    pub fn after_restore(&mut self, finish: &super::snapshot_glue::Finish) {
        if matches!(finish, super::snapshot_glue::Finish::Done) {
            *self = Self::default();
        }
    }

    pub fn pending_load() -> Self {
        Self {
            saving_disabled: true,
            notice: None,
            ..Self::default()
        }
    }

    /// Also used by write-through tab and colour edits, which bypass App::save.
    pub fn save(&self, path: &Path, layout: &Layout) -> io::Result<()> {
        if self.saving_disabled {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Layout saving is off until a backup is successfully restored or Corral restarts.",
            ));
        }
        // Startup and quit autosaves must not answer the first-run question. Anything the user has
        // arranged is saved as usual, whatever the state of the question.
        if self.first_run.keeps_question_open(layout) {
            return Ok(());
        }
        model::save(path, layout)
    }
}

#[derive(Default)]
struct Preservation {
    renamed: Vec<(PathBuf, PathBuf)>,
    errors: Vec<String>,
}

fn unreadable_path(path: &Path, stamp: &str, number: u32) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".unreadable-{stamp}"));
    if number > 0 {
        name.push(format!("-{number}"));
    }
    PathBuf::from(name)
}

fn rename_unreadable(path: &Path, stamp: &str) -> io::Result<PathBuf> {
    for number in 0..=u32::MAX {
        let target = unreadable_path(path, stamp, number);
        match fs::symlink_metadata(&target) {
            Ok(_) => continue,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        // Unlike fs::rename, MoveFileW never replaces an existing destination, even if
        // another writer creates it between the presence check and this call.
        match unsafe {
            MoveFileW(
                &HSTRING::from(path.as_os_str()),
                &HSTRING::from(target.as_os_str()),
            )
        } {
            Ok(()) => return Ok(target),
            Err(e) => {
                let error = io::Error::from_raw_os_error(e.code().0 & 0xffff);
                if error.kind() != io::ErrorKind::AlreadyExists {
                    return Err(error);
                }
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "No unused unreadable layout name",
    ))
}

/// Only renames the supplied files, in their own folders. A partial success is kept;
/// any failure disables saving rather than attempting a destructive rollback.
fn set_aside(paths: &[&Path], stamp: &str) -> Preservation {
    let mut result = Preservation::default();
    for path in paths {
        match fs::symlink_metadata(path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Ok(m) if m.is_dir() => {
                result
                    .errors
                    .push(format!("{} is a directory", path.display()));
                continue;
            }
            _ => {}
        }
        match rename_unreadable(path, stamp) {
            Ok(target) => result.renamed.push((path.to_path_buf(), target)),
            Err(e) => result.errors.push(format!("{}: {e}", path.display())),
        }
    }
    result
}

pub(super) fn load(path: &Path, stamp: &str) -> (Layout, Recovery) {
    load_with_retry(path, stamp, RETRIES, || std::thread::sleep(RETRY_WAIT))
}

/// Retry the pair, preserving primary-first fallback semantics. The count and wait
/// are injected so a test can release a real file lock without sleeping.
fn load_with_retry(
    path: &Path,
    stamp: &str,
    retries: usize,
    mut wait: impl FnMut(),
) -> (Layout, Recovery) {
    let backup = model::backup_path(path);
    let mut attempt = 0;
    let (primary_result, backup_result, action) = loop {
        let primary_result = model::load(path);
        // Just as in load_with_backup, a good primary needs no backup read.
        if let Ok(layout) = primary_result {
            return (layout, Recovery::default());
        }
        let backup_result = model::load(&backup);
        let action = decision(&primary_result, &backup_result);
        if action != Action::Temporary || attempt == retries {
            break (primary_result, backup_result, action);
        }
        attempt += 1;
        super::log(&format!(
            "layout startup: retry {attempt}/{retries} after I/O error; primary: {}; backup: {}",
            primary_result.as_ref().unwrap_err(),
            backup_result.as_ref().unwrap_err()
        ));
        wait();
    };
    match action {
        Action::Loaded => (
            primary_result.or(backup_result).unwrap(),
            Recovery::default(),
        ),
        Action::FirstRun => (Layout::new(), Recovery {
            first_run: super::first_run::Offer::Pending,
            ..Recovery::default()
        }),
        Action::Temporary => {
            super::log(&format!(
                "layout startup: still unavailable after {attempt} retries; primary: {}; backup: {}; files unchanged; starting empty with saving disabled",
                primary_result.unwrap_err(),
                backup_result.unwrap_err()
            ));
            (
                Layout::new(),
                Recovery {
                    saving_disabled: true,
                    notice: Some(TEMPORARY_NOTICE.into()),
                    ..Recovery::default()
                },
            )
        }
        Action::Preserve {
            primary,
            backup: bad_backup,
        } => {
            let mut paths = Vec::new();
            if primary {
                paths.push(path);
            }
            if bad_backup {
                paths.push(backup.as_path());
            }
            let kept = set_aside(&paths, stamp);
            let saving_disabled = !kept.errors.is_empty();
            super::log(&format!(
                "layout startup: primary: {}; backup: {}; starting empty; saving disabled={saving_disabled}",
                primary_result.unwrap_err(),
                backup_result.unwrap_err()
            ));
            for (source, target) in &kept.renamed {
                super::log(&format!(
                    "layout startup: kept {} as {}",
                    source.display(),
                    target.display()
                ));
            }
            for error in &kept.errors {
                super::log(&format!("layout startup: could not set aside {error}"));
            }
            let names = kept
                .renamed
                .iter()
                .map(|(_, p)| p.file_name().unwrap_or_default().to_string_lossy())
                .collect::<Vec<_>>()
                .join("\n");
            let folder = path.parent().unwrap_or_else(|| Path::new("."));
            let mut notice = "The layout could not be read.".to_owned();
            if !names.is_empty() {
                notice.push_str(&format!(" The unreadable files were kept under new names in the Corral settings folder:\n\n{names}\n\n{}", folder.display()));
            }
            if saving_disabled {
                notice.push_str("\n\nA file could not be renamed and was left in place. Layout saving is off until a backup is successfully restored or Corral restarts.");
            }
            notice.push_str("\n\nBackups can be restored from Settings.");
            (
                Layout::new(),
                Recovery {
                    saving_disabled,
                    notice: Some(notice),
                    ..Recovery::default()
                },
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    const STAMP: &str = "20261104-153012";
    const BAD_PRIMARY: &[u8] = b"{hand edited primary\r\n";
    const BAD_BACKUP: &[u8] = b"{broken backup\n";
    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join("corral-layout-startup-tests");
            fs::create_dir_all(&root).unwrap();
            loop {
                let path = root.join(format!(
                    "corral-unreadable-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(e) => panic!("temporary folder: {e}"),
                }
            }
        }
        fn primary(&self) -> PathBuf {
            self.0.join("layout.json")
        }
        fn backup(&self) -> PathBuf {
            model::backup_path(&self.primary())
        }
        fn start(&self) -> (Layout, Recovery) {
            load_with_retry(&self.primary(), STAMP, RETRIES, || {})
        }
        fn entries(&self) -> usize {
            fs::read_dir(&self.0).unwrap().count()
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            // Every entry was created in this test's exclusively created folder; no recursion.
            for entry in fs::read_dir(&self.0).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() {
                    fs::remove_dir(entry.path()).unwrap();
                } else {
                    fs::remove_file(entry.path()).unwrap();
                }
            }
            fs::remove_dir(&self.0).unwrap();
        }
    }

    #[test]
    fn decision_classifies_each_file_and_all_combinations() {
        use Action::{FirstRun as F, Loaded as L, Temporary as T};
        const P: Action = Action::Preserve {
            primary: true,
            backup: false,
        };
        const B: Action = Action::Preserve {
            primary: false,
            backup: true,
        };
        const BOTH: Action = Action::Preserve {
            primary: true,
            backup: true,
        };
        let results = [
            Ok(Layout::new()),
            Err(LoadError::Io(io::Error::from(io::ErrorKind::NotFound))),
            Err(LoadError::Io(io::Error::from(
                io::ErrorKind::PermissionDenied,
            ))),
            Err(LoadError::Parse(
                serde_json::from_slice::<Layout>(BAD_PRIMARY).unwrap_err(),
            )),
            Err(LoadError::UnsupportedVersion(u32::MAX)),
            Err(LoadError::InvalidTabs(crate::tabs::TabError::Invalid(
                "duplicate id",
            ))),
        ];
        // Rows are primary, columns backup: good, missing, I/O, parse, version, tabs.
        let expected = [
            [L, L, L, L, L, L],
            [L, F, T, B, B, B],
            [L, T, T, T, T, T],
            [L, P, T, BOTH, BOTH, BOTH],
            [L, P, T, BOTH, BOTH, BOTH],
            [L, P, T, BOTH, BOTH, BOTH],
        ];
        for (p, primary) in results.iter().enumerate() {
            for (b, backup) in results.iter().enumerate() {
                assert_eq!(
                    decision(primary, backup),
                    expected[p][b],
                    "primary={p}, backup={b}"
                );
            }
        }
    }

    #[test]
    fn exclusive_lock_released_on_retry_is_a_normal_start() {
        use std::os::windows::fs::OpenOptionsExt;
        for lock_backup in [false, true] {
            let dir = Scratch::new();
            let mut expected = Layout::new();
            expected.next_id = 42;
            let good = serde_json::to_vec(&expected).unwrap();
            let (locked_path, other) = if lock_backup {
                (dir.backup(), dir.primary())
            } else {
                (dir.primary(), dir.backup())
            };
            fs::write(&locked_path, &good).unwrap();
            fs::write(&other, BAD_PRIMARY).unwrap();
            let mut lock = Some(
                fs::OpenOptions::new()
                    .read(true)
                    .share_mode(0)
                    .open(&locked_path)
                    .unwrap(),
            );
            let mut waits = 0;
            let (layout, recovery) = load_with_retry(&dir.primary(), STAMP, RETRIES, || {
                waits += 1;
                // Exercise the last permitted retry, without a real sleep.
                if waits == RETRIES {
                    drop(lock.take());
                }
            });
            assert_eq!(waits, RETRIES);
            assert_eq!(layout, expected);
            assert!(!recovery.saving_disabled);
            assert!(recovery.notice.is_none());
            assert_eq!(dir.entries(), 2);
            assert_eq!(fs::read(&locked_path).unwrap(), good);
            assert_eq!(fs::read(&other).unwrap(), BAD_PRIMARY);
        }
    }

    #[test]
    fn persistent_exclusive_lock_leaves_both_files_untouched_and_saving_off() {
        use std::os::windows::fs::OpenOptionsExt;
        for lock_backup in [false, true] {
            for other_missing in [false, true] {
                let dir = Scratch::new();
                let (locked_path, other) = if lock_backup {
                    (dir.backup(), dir.primary())
                } else {
                    (dir.primary(), dir.backup())
                };
                let good = serde_json::to_vec(&Layout::new()).unwrap();
                fs::write(&locked_path, &good).unwrap();
                if !other_missing {
                    fs::write(&other, BAD_PRIMARY).unwrap();
                }
                let lock = fs::OpenOptions::new()
                    .read(true)
                    .share_mode(0)
                    .open(&locked_path)
                    .unwrap();
                let mut waits = 0;
                let (layout, recovery) =
                    load_with_retry(&dir.primary(), STAMP, RETRIES, || waits += 1);
                assert_eq!(waits, RETRIES);
                assert_eq!(layout, Layout::new());
                assert!(recovery.saving_disabled);
                assert_eq!(recovery.notice.as_deref(), Some(TEMPORARY_NOTICE));
                assert_eq!(dir.entries(), if other_missing { 1 } else { 2 });
                drop(lock);
                assert_eq!(
                    recovery.save(&dir.primary(), &layout).unwrap_err().kind(),
                    io::ErrorKind::PermissionDenied
                );
                assert_eq!(fs::read(&locked_path).unwrap(), good);
                if other_missing {
                    assert!(!other.exists());
                } else {
                    assert_eq!(fs::read(&other).unwrap(), BAD_PRIMARY);
                }
            }
        }
    }

    #[test]
    fn a_good_file_wins_even_when_the_other_is_locked() {
        use std::os::windows::fs::OpenOptionsExt;
        for lock_backup in [false, true] {
            let dir = Scratch::new();
            let mut expected = Layout::new();
            expected.next_id = 42;
            let good = serde_json::to_vec(&expected).unwrap();
            fs::write(dir.primary(), &good).unwrap();
            fs::write(dir.backup(), &good).unwrap();
            let locked_path = if lock_backup {
                dir.backup()
            } else {
                dir.primary()
            };
            let lock = fs::OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(&locked_path)
                .unwrap();
            let (layout, recovery) = load_with_retry(&dir.primary(), STAMP, RETRIES, || {
                panic!("good file needs no retry")
            });
            assert_eq!(layout, expected);
            assert!(!recovery.saving_disabled);
            assert!(recovery.notice.is_none());
            drop(lock);
            assert_eq!(fs::read(dir.primary()).unwrap(), good);
            assert_eq!(fs::read(dir.backup()).unwrap(), good);
            assert_eq!(dir.entries(), 2);
        }
    }

    #[test]
    fn restore_reenables_saving_and_clears_pending_notice_only_after_success() {
        use super::super::snapshot_glue::{Finish, finish_restore, restore_to_disk};
        use crate::{settings::Settings, snapshot::Store};
        for outcome in ["success", "rolled back", "incomplete"] {
            let dir = Scratch::new();
            let store = Store::open(&dir.0).unwrap();
            let settings = Settings::default();
            let mut target_layout = Layout::new();
            target_layout.next_id = 42;
            let target = store
                .capture("Recovery test", 1, &target_layout, &settings)
                .unwrap();
            let settings_path = dir.0.join("settings.json");
            if outcome == "incomplete" {
                fs::create_dir(&settings_path).unwrap();
            }
            store.write_journal(&target.id).unwrap();
            let memory = Layout::new();
            let finish = finish_restore(
                &store,
                &target,
                (&memory, &settings),
                (&dir.primary(), &settings_path),
                |snapshot, layout_path, settings_path| {
                    if outcome == "rolled back" {
                        model::save(layout_path, &snapshot.layout)?;
                        Err(io::Error::other("injected settings write failure"))
                    } else {
                        restore_to_disk(snapshot, layout_path, settings_path)
                    }
                },
            );
            assert!(matches!(
                (&finish, outcome),
                (Finish::Done, "success")
                    | (Finish::RolledBack(_), "rolled back")
                    | (Finish::Incomplete(_), "incomplete")
            ));
            let mut recovery = Recovery::pending_load();
            recovery.notice = Some(TEMPORARY_NOTICE.into());
            recovery.after_restore(&finish);
            if outcome == "success" {
                assert!(!recovery.saving_disabled);
                assert!(recovery.notice.is_none());
                assert_eq!(model::load(&dir.primary()).unwrap(), target_layout);
                target_layout.next_id = 43;
                recovery.save(&dir.primary(), &target_layout).unwrap();
                assert_eq!(model::load(&dir.primary()).unwrap().next_id, 43);
            } else {
                assert!(recovery.saving_disabled);
                assert_eq!(recovery.notice.as_deref(), Some(TEMPORARY_NOTICE));
                assert_eq!(
                    recovery.save(&dir.primary(), &memory).unwrap_err().kind(),
                    io::ErrorKind::PermissionDenied
                );
            }
        }
    }

    #[test]
    fn saves_are_held_until_the_startup_load_has_decided() {
        let dir = Scratch::new();
        let pending = Recovery::pending_load();
        assert!(pending.save(&dir.primary(), &Layout::new()).is_err());
        assert_eq!(dir.entries(), 0);
        let (layout, ready) = dir.start();
        ready.save(&dir.primary(), &layout).unwrap();
    }

    #[test]
    fn first_run_has_no_rename_flag_or_notice() {
        let dir = Scratch::new();
        let (layout, mut recovery) = dir.start();
        assert_eq!(layout, Layout::new());
        assert!(!recovery.saving_disabled);
        assert!(recovery.notice.is_none());
        assert_eq!(dir.entries(), 0);
        recovery.save(&dir.primary(), &layout).unwrap();
        assert_eq!(dir.entries(), 0, "an unanswered offer cannot persist an empty layout");
        recovery.first_run = super::super::first_run::Offer::No;
        recovery.save(&dir.primary(), &layout).unwrap();
        assert!(model::load(&dir.primary()).is_ok());
    }

    #[test]
    fn bad_primary_good_backup_loads_without_renaming_or_notice() {
        let dir = Scratch::new();
        let mut expected = Layout::new();
        expected.next_id = 42;
        let bytes = serde_json::to_vec(&expected).unwrap();
        fs::write(dir.primary(), BAD_PRIMARY).unwrap();
        fs::write(dir.backup(), &bytes).unwrap();
        let (layout, recovery) = dir.start();
        assert_eq!(layout, expected);
        assert!(!recovery.saving_disabled);
        assert!(recovery.notice.is_none());
        assert_eq!(dir.entries(), 2);
        assert_eq!(fs::read(dir.primary()).unwrap(), BAD_PRIMARY);
        assert_eq!(fs::read(dir.backup()).unwrap(), bytes);
    }

    #[test]
    fn both_bad_survive_byte_identically_after_two_blank_saves() {
        let dir = Scratch::new();
        fs::write(dir.primary(), BAD_PRIMARY).unwrap();
        fs::write(dir.backup(), BAD_BACKUP).unwrap();
        let (layout, recovery) = dir.start();
        assert_eq!(layout, Layout::new());
        assert!(!recovery.saving_disabled);
        assert!(
            recovery
                .notice
                .as_ref()
                .unwrap()
                .contains("Backups can be restored from Settings")
        );
        assert!(!dir.primary().exists());
        assert!(!dir.backup().exists());
        for _ in 0..2 {
            recovery.save(&dir.primary(), &layout).unwrap();
            assert_eq!(
                fs::read(unreadable_path(&dir.primary(), STAMP, 0)).unwrap(),
                BAD_PRIMARY
            );
            assert_eq!(
                fs::read(unreadable_path(&dir.backup(), STAMP, 0)).unwrap(),
                BAD_BACKUP
            );
        }
        assert_eq!(model::load(&dir.primary()).unwrap(), Layout::new());
        assert_eq!(model::load(&dir.backup()).unwrap(), Layout::new());
    }

    #[test]
    fn newer_versions_are_preserved_before_two_saves() {
        let dir = Scratch::new();
        let newer = br#"{"version":4294967295,"next_id":1,"groups":[]}"#;
        fs::write(dir.primary(), newer).unwrap();
        fs::write(dir.backup(), newer).unwrap();
        assert!(matches!(
            model::load_with_backup(&dir.primary()),
            Err(LoadError::UnsupportedVersion(_))
        ));
        let (layout, recovery) = dir.start();
        assert!(!recovery.saving_disabled);
        assert!(recovery.notice.is_some());
        for _ in 0..2 {
            recovery.save(&dir.primary(), &layout).unwrap();
            for path in [dir.primary(), dir.backup()] {
                assert_eq!(fs::read(unreadable_path(&path, STAMP, 0)).unwrap(), newer);
            }
        }
    }

    #[test]
    fn invalid_tabs_are_preserved_too() {
        let dir = Scratch::new();
        let mut bad = Layout::new();
        bad.add_group(
            "A".into(),
            "test".into(),
            crate::geometry::Rect::new(0, 0, 100, 100),
            model::GroupContent::Items { items: vec![] },
        );
        bad.groups.push(bad.groups[0].clone());
        let bytes = serde_json::to_vec(&bad).unwrap();
        fs::write(dir.primary(), &bytes).unwrap();
        assert!(matches!(
            model::load_with_backup(&dir.primary()),
            Err(LoadError::InvalidTabs(_))
        ));
        let (_, recovery) = dir.start();
        assert!(!recovery.saving_disabled);
        assert_eq!(
            fs::read(unreadable_path(&dir.primary(), STAMP, 0)).unwrap(),
            bytes
        );
    }

    #[test]
    fn either_unreadable_file_alone_is_preserved() {
        for backup_only in [false, true] {
            let dir = Scratch::new();
            let path = if backup_only {
                dir.backup()
            } else {
                dir.primary()
            };
            fs::write(&path, BAD_PRIMARY).unwrap();
            let (layout, recovery) = dir.start();
            assert!(!recovery.saving_disabled);
            assert!(recovery.notice.is_some());
            assert_eq!(dir.entries(), 1);
            recovery.save(&dir.primary(), &layout).unwrap();
            recovery.save(&dir.primary(), &layout).unwrap();
            assert_eq!(
                fs::read(unreadable_path(&path, STAMP, 0)).unwrap(),
                BAD_PRIMARY
            );
        }
    }

    #[test]
    fn occupied_names_are_numbered_and_never_overwritten() {
        let dir = Scratch::new();
        fs::write(dir.primary(), BAD_PRIMARY).unwrap();
        fs::write(dir.backup(), BAD_BACKUP).unwrap();
        for path in [dir.primary(), dir.backup()] {
            fs::write(unreadable_path(&path, STAMP, 0), b"earlier copy").unwrap();
            fs::write(unreadable_path(&path, STAMP, 1), b"another copy").unwrap();
        }
        let kept = set_aside(&[&dir.primary(), &dir.backup()], STAMP);
        assert!(kept.errors.is_empty());
        assert_eq!(kept.renamed.len(), 2);
        for (path, bytes) in [(dir.primary(), BAD_PRIMARY), (dir.backup(), BAD_BACKUP)] {
            assert_eq!(
                fs::read(unreadable_path(&path, STAMP, 0)).unwrap(),
                b"earlier copy"
            );
            assert_eq!(
                fs::read(unreadable_path(&path, STAMP, 1)).unwrap(),
                b"another copy"
            );
            assert_eq!(fs::read(unreadable_path(&path, STAMP, 2)).unwrap(), bytes);
        }
    }

    #[test]
    fn rename_failure_keeps_saving_off_even_after_the_lock_is_released() {
        use std::os::windows::fs::OpenOptionsExt;
        // Test both an immediate failure and partial preservation (backup rename fails).
        for lock_backup in [false, true] {
            let dir = Scratch::new();
            fs::write(dir.primary(), BAD_PRIMARY).unwrap();
            fs::write(dir.backup(), BAD_BACKUP).unwrap();
            let locked_path = if lock_backup {
                dir.backup()
            } else {
                dir.primary()
            };
            // Permit reads, deny delete sharing: loading fails on the bytes, renaming on the lock.
            let lock = fs::OpenOptions::new()
                .read(true)
                .share_mode(3)
                .open(&locked_path)
                .unwrap();
            let (layout, recovery) = dir.start();
            assert_eq!(layout, Layout::new());
            assert!(recovery.saving_disabled);
            assert!(recovery.notice.as_ref().unwrap().contains("saving is off"));
            assert_eq!(
                fs::read(&locked_path).unwrap(),
                if lock_backup { BAD_BACKUP } else { BAD_PRIMARY }
            );
            drop(lock);
            for _ in 0..2 {
                assert_eq!(
                    recovery.save(&dir.primary(), &layout).unwrap_err().kind(),
                    io::ErrorKind::PermissionDenied
                );
                assert_eq!(
                    fs::read(&locked_path).unwrap(),
                    if lock_backup { BAD_BACKUP } else { BAD_PRIMARY }
                );
            }
            let (other, bytes) = if lock_backup {
                (dir.primary(), BAD_PRIMARY)
            } else {
                (dir.backup(), BAD_BACKUP)
            };
            assert!(!other.exists());
            assert_eq!(fs::read(unreadable_path(&other, STAMP, 0)).unwrap(), bytes);
        }
    }

    #[test]
    fn io_error_is_not_first_run_and_directories_are_not_moved() {
        let dir = Scratch::new();
        fs::create_dir(dir.primary()).unwrap();
        let (layout, recovery) = dir.start();
        assert!(recovery.saving_disabled);
        assert!(dir.primary().is_dir());
        assert!(recovery.save(&dir.primary(), &layout).is_err());
        assert_eq!(dir.entries(), 1);
    }
}
