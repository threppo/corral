//! Read-only facts about desktop items, the input of Round-up (round-up-spec 1, 2).
//!
//! Nothing here moves, copies, renames, deletes or writes a file or folder. The only file system
//! access is reading metadata and the saved target of a shortcut, and it goes through
//! [`FactReader`], a trait that has no write operation at all.

use crate::shell::ShellItem;
use serde::{Deserialize, Serialize};

/// Exactly one type per item (round-up-spec 2, "Item type").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemType {
    Folder,
    File,
    Shortcut,
    SystemItem,
}

/// What a shortcut points to. A web address is none of program, folder or file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetClass {
    Program,
    Folder,
    File,
    Web,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetFacts {
    pub path: String,
    pub class: TargetClass,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemFacts {
    /// The layout's identity of the item: its parsing name.
    pub name: String,
    /// Real file name with extension (the Shell label may hide the extension); the display name
    /// for system items.
    pub file_name: String,
    pub kind: ItemType,
    /// File size in bytes; `None` when it does not apply or could not be read.
    pub size: Option<u64>,
    /// Last-write time as a UTC instant in Unix milliseconds; `None` when unset or unreadable.
    pub modified_ms: Option<i64>,
    /// Shortcuts only: `None` when the target could not be read or does not exist.
    pub target: Option<TargetFacts>,
}

#[derive(Clone, Debug)]
pub struct FileMetadata {
    pub is_folder: bool,
    pub size: u64,
    pub modified_ms: Option<i64>,
}

/// The only capability fact gathering has: reading (round-up-spec 1).
pub trait FactReader {
    fn metadata(&self, path: &str) -> Option<FileMetadata>;
    fn shortcut_target(&self, path: &str) -> Option<String>;
}

/// Facts of one desktop item from the real file system.
pub fn facts_for(item: &ShellItem) -> ItemFacts {
    collect_with(item, &ReadOnlyFacts)
}

/// Facts of every item on the real desktop, for the editor's live caption. Errors are reported
/// instead of showing a misleading zero.
pub fn desktop_facts() -> Result<Vec<ItemFacts>, String> {
    ensure_com();
    let items = crate::shell::enumerate_desktop()
        .map_err(|_| "The desktop icons could not be read.".to_string())?;
    Ok(items.iter().map(facts_for).collect())
}

/// Shortcut targets are read through COM; the settings window thread may not have started it.
fn ensure_com() {
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
    thread_local!(static DONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) });
    DONE.with(|done| {
        if !done.replace(true) {
            // S_FALSE (already initialised) and a changed mode are both fine here.
            let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        }
    });
}

const PROGRAM_EXTENSIONS: [&str; 5] = ["exe", "bat", "cmd", "dll", "jar"];

pub fn collect_with(item: &ShellItem, reader: &dyn FactReader) -> ItemFacts {
    let path = item.parsing_name.as_str();
    let system = path.starts_with("::");
    let file_name = path.rsplit(['\\', '/']).next().unwrap_or(path).to_string();
    let extension = file_name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    let metadata = (!system).then(|| reader.metadata(path)).flatten();
    let folder = item.is_folder || metadata.as_ref().is_some_and(|m| m.is_folder);
    let shortcut =
        !folder && (item.is_link || matches!(extension.as_str(), "lnk" | "url" | "website"));
    let kind = if system {
        ItemType::SystemItem
    } else if folder {
        ItemType::Folder
    } else if shortcut {
        ItemType::Shortcut
    } else {
        ItemType::File
    };
    let target = (kind == ItemType::Shortcut)
        .then(|| reader.shortcut_target(path))
        .flatten()
        .and_then(|target| classify_target(target, reader));
    ItemFacts {
        name: path.to_string(),
        file_name: if system {
            item.display_name.clone()
        } else {
            file_name
        },
        kind,
        size: matches!(kind, ItemType::File | ItemType::Shortcut)
            .then(|| metadata.as_ref().map(|m| m.size))
            .flatten(),
        modified_ms: metadata.as_ref().and_then(|m| m.modified_ms),
        target,
    }
}

/// One attribute lookup of the target path (round-up-spec 2, "Shortcut points to"). A target that
/// cannot be found gives `None`: the fact should exist but could not be obtained.
fn classify_target(path: String, reader: &dyn FactReader) -> Option<TargetFacts> {
    if path.is_empty() {
        return None;
    }
    let class = if path.contains("://") {
        TargetClass::Web
    } else if path.starts_with("::") {
        TargetClass::Program
    } else {
        let metadata = reader.metadata(&path)?;
        let extension = path
            .rsplit(['\\', '/'])
            .next()
            .and_then(|name| name.rsplit_once('.'))
            .map(|(_, e)| e.to_ascii_lowercase())
            .unwrap_or_default();
        if metadata.is_folder {
            TargetClass::Folder
        } else if PROGRAM_EXTENSIONS.contains(&extension.as_str()) {
            TargetClass::Program
        } else {
            TargetClass::File
        }
    };
    Some(TargetFacts { path, class })
}

struct ReadOnlyFacts;

impl FactReader for ReadOnlyFacts {
    fn metadata(&self, path: &str) -> Option<FileMetadata> {
        let meta = std::fs::metadata(path).ok()?;
        let modified_ms = meta.modified().ok().and_then(|time| {
            let value = match time.duration_since(std::time::UNIX_EPOCH) {
                Ok(delta) => i128::try_from(delta.as_millis()).ok()?,
                Err(error) => -i128::try_from(error.duration().as_millis()).ok()?,
            };
            i64::try_from(value).ok()
        });
        Some(FileMetadata {
            is_folder: meta.is_dir(),
            size: meta.len(),
            modified_ms,
        })
    }

    fn shortcut_target(&self, path: &str) -> Option<String> {
        let is_lnk = path
            .rsplit_once('.')
            .is_some_and(|(_, e)| e.eq_ignore_ascii_case("lnk"));
        if is_lnk {
            resolve_lnk(path)
        } else {
            read_internet_shortcut(path)
        }
    }
}

/// `URL=` of an Internet shortcut; UTF-16LE or UTF-8, with an optional byte order mark.
fn read_internet_shortcut(path: &str) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let text = if bytes.starts_with(&[0xff, 0xfe]) {
        String::from_utf16_lossy(
            &bytes[2..]
                .chunks_exact(2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
                .collect::<Vec<_>>(),
        )
    } else {
        String::from_utf8_lossy(&bytes).into_owned()
    };
    text.trim_start_matches('\u{feff}')
        .lines()
        .find_map(|line| {
            let (key, value) = line.trim().split_once('=')?;
            (key.eq_ignore_ascii_case("URL") && !value.trim().is_empty())
                .then(|| value.trim().to_string())
        })
}

fn resolve_lnk(path: &str) -> Option<String> {
    use windows::Win32::Storage::FileSystem::WIN32_FIND_DATAW;
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, CoCreateInstance, IPersistFile, STGM_READ,
    };
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    use windows::core::{HSTRING, Interface};
    // SAFETY: plain COM calls on locals; the buffer outlives the call that fills it.
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let file: IPersistFile = link.cast().ok()?;
        file.Load(&HSTRING::from(path), STGM_READ).ok()?;
        let mut buffer = [0u16; 32768];
        let mut data = WIN32_FIND_DATAW::default();
        link.GetPath(&mut buffer, &mut data, 0).ok()?;
        let len = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
        (len > 0).then(|| String::from_utf16_lossy(&buffer[..len]))
    }
}

/// A fake file tree for tests (round-up-spec 7): it can only be read, which is all that fact
/// gathering is allowed to do.
#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    #[derive(Default)]
    pub struct FakeTree {
        pub entries: BTreeMap<String, (FileMetadata, Option<String>)>,
        pub reads: RefCell<Vec<String>>,
    }

    impl FakeTree {
        pub fn file(mut self, path: &str, size: u64, modified_ms: Option<i64>) -> Self {
            let meta = FileMetadata {
                is_folder: false,
                size,
                modified_ms,
            };
            self.entries.insert(path.to_lowercase(), (meta, None));
            self
        }
        pub fn folder(mut self, path: &str) -> Self {
            let meta = FileMetadata {
                is_folder: true,
                size: 0,
                modified_ms: Some(1),
            };
            self.entries.insert(path.to_lowercase(), (meta, None));
            self
        }
        pub fn link(mut self, path: &str, target: &str) -> Self {
            let meta = FileMetadata {
                is_folder: false,
                size: 1,
                modified_ms: Some(1),
            };
            self.entries
                .insert(path.to_lowercase(), (meta, Some(target.to_string())));
            self
        }
        /// Everything the tree holds, as text: equal before and after means nothing changed.
        pub fn fingerprint(&self) -> String {
            format!("{:?}", self.entries)
        }
    }

    impl FactReader for FakeTree {
        fn metadata(&self, path: &str) -> Option<FileMetadata> {
            self.reads.borrow_mut().push(path.to_string());
            self.entries
                .get(&path.to_lowercase())
                .map(|(m, _)| m.clone())
        }
        fn shortcut_target(&self, path: &str) -> Option<String> {
            self.entries
                .get(&path.to_lowercase())
                .and_then(|(_, t)| t.clone())
        }
    }

    pub fn item(path: &str) -> ShellItem {
        ShellItem {
            parsing_name: path.into(),
            display_name: "hidden extension".into(),
            is_folder: false,
            is_link: false,
            attributes: 0,
            modified_unix: None,
            size: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{FakeTree, item};
    use super::*;

    #[test]
    fn real_file_name_is_used_not_the_extension_hiding_label() {
        let tree = FakeTree::default().file("C:\\desk\\report.pdf", 5, Some(9));
        let facts = collect_with(&item("C:\\desk\\report.pdf"), &tree);
        assert_eq!(
            (facts.file_name.as_str(), facts.kind),
            ("report.pdf", ItemType::File)
        );
        assert_eq!((facts.size, facts.modified_ms), (Some(5), Some(9)));
    }

    #[test]
    fn folder_target_costs_one_attribute_lookup_and_is_classified() {
        let tree = FakeTree::default()
            .link("C:\\desk\\docs.lnk", "C:\\work")
            .folder("C:\\work");
        let facts = collect_with(&item("C:\\desk\\docs.lnk"), &tree);
        assert_eq!(facts.kind, ItemType::Shortcut);
        let target = facts.target.unwrap();
        assert_eq!(
            (target.path.as_str(), target.class),
            ("C:\\work", TargetClass::Folder)
        );
        assert_eq!(
            tree.reads.borrow().as_slice(),
            ["C:\\desk\\docs.lnk", "C:\\work"]
        );
    }

    #[test]
    fn target_classes_program_file_web_and_shell() {
        let tree = FakeTree::default()
            .link("a.lnk", "C:\\Apps\\Tool.EXE")
            .file("C:\\Apps\\Tool.EXE", 1, Some(1))
            .link("b.lnk", "C:\\Docs\\a.txt")
            .file("C:\\Docs\\a.txt", 1, Some(1))
            .link("c.url", "https://example.org/page")
            .link("d.lnk", "::{26EE0668-A00A-44D7-9371-BEB064C98683}");
        let class = |p: &str| collect_with(&item(p), &tree).target.map(|t| t.class);
        assert_eq!(class("a.lnk"), Some(TargetClass::Program));
        assert_eq!(class("b.lnk"), Some(TargetClass::File));
        assert_eq!(class("c.url"), Some(TargetClass::Web));
        assert_eq!(class("d.lnk"), Some(TargetClass::Program));
    }

    #[test]
    fn missing_or_unreadable_target_is_unknown_not_a_guess() {
        let tree = FakeTree::default()
            .link("gone.lnk", "C:\\missing\\x.exe")
            .link("empty.lnk", "");
        assert_eq!(collect_with(&item("gone.lnk"), &tree).target, None);
        assert_eq!(collect_with(&item("empty.lnk"), &tree).target, None);
        assert_eq!(collect_with(&item("unknown.lnk"), &tree).target, None);
    }

    #[test]
    fn system_items_are_never_read_from_the_file_system() {
        let tree = FakeTree::default();
        let facts = collect_with(&item("::{645FF040-5081-101B-9F08-00AA002F954E}"), &tree);
        assert_eq!(facts.kind, ItemType::SystemItem);
        assert_eq!(
            (facts.size, facts.modified_ms, facts.file_name.as_str()),
            (None, None, "hidden extension")
        );
        assert!(tree.reads.borrow().is_empty());
    }

    #[test]
    fn unreadable_size_and_date_stay_unknown_even_when_the_shell_cached_values() {
        let mut cached = item("missing.pdf");
        cached.size = Some(1024);
        cached.modified_unix = Some(1_700_000_000);
        let facts = collect_with(&cached, &FakeTree::default());
        assert_eq!(
            (facts.kind, facts.size, facts.modified_ms),
            (ItemType::File, None, None)
        );
    }

    #[test]
    fn folders_and_internet_shortcuts_get_their_own_type() {
        let tree = FakeTree::default()
            .folder("C:\\desk\\Projects")
            .link("C:\\desk\\w.url", "https://a.b/");
        assert_eq!(
            collect_with(&item("C:\\desk\\Projects"), &tree).kind,
            ItemType::Folder
        );
        assert_eq!(
            collect_with(&item("C:\\desk\\w.url"), &tree).kind,
            ItemType::Shortcut
        );
        let mut flagged = item("C:\\desk\\x.lnk");
        flagged.is_link = true;
        assert_eq!(collect_with(&flagged, &tree).kind, ItemType::Shortcut);
    }

    #[test]
    fn fact_gathering_has_no_write_capability() {
        // The reader trait offers two read functions and nothing else; this keeps a reviewer from
        // adding a third without noticing (round-up-spec 7, "Never a file operation").
        let source = include_str!("item_facts.rs");
        let production = source.split("#[cfg(test)]").next().unwrap();
        for forbidden in [
            "fs::write",
            "fs::remove",
            "fs::rename",
            "fs::copy",
            "fs::create",
            "File::create",
            "OpenOptions",
            "set_permissions",
            "MoveFile",
            "DeleteFile",
            "CopyFile",
            "SHFileOperation",
            "IFileOperation",
            "std::process",
        ] {
            assert!(!production.contains(forbidden), "{forbidden}");
        }
    }
}
