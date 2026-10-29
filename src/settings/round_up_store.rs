//! Reading and writing the Round-up parts of the settings file (round-up-spec 6).
//!
//! * A missing `RoundUp` block means "older file": the typed block is migrated from the old
//!   `Rules` strings. The migration is a pure in-memory step; nothing is saved before the
//!   configuration copies exist.
//! * A block with a newer `version`, or one this build cannot read, is kept byte for byte and
//!   Round-up stays paused.
//! * The old `Rules` value is written back untouched while items need review, and dropped after.
//! * Nothing here touches a file the settings do not own: only Corral's own settings and layout
//!   file are copied, and only inside their own folder.

use std::fs;
use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};

use super::Settings;
use crate::round_up::{RoundUp, StoredRule};

/// Newest `RoundUp.version` this build understands.
pub const BLOCK_VERSION: u32 = 1;

/// Parse a settings file's text. Unknown keys are ignored like everywhere else; the Round-up
/// parts get the handling described in the module comment.
pub fn decode(text: &str) -> serde_json::Result<Settings> {
    let mut value: serde_json::Value = serde_json::from_str(text)?;
    let mut keep = super::RoundUpKeep::default();
    let mut paused = None;
    let mut absent = true;
    if let Some(block) = value.get("RoundUp").cloned() {
        absent = false;
        let newer = block
            .get("version")
            .and_then(|v| v.as_u64())
            .is_some_and(|v| v > u64::from(BLOCK_VERSION));
        let readable = !newer && serde_json::from_value::<RoundUp>(block).is_ok();
        if !readable {
            keep.block = top_level_value(text, "RoundUp").map(|span| text[span].to_owned());
            // A harmless stand-in keeps `Settings::deserialize` from migrating over the kept block.
            if let Some(object) = value.as_object_mut() {
                object.insert(
                    "RoundUp".into(),
                    serde_json::to_value(RoundUp::default()).expect("default block serializes"),
                );
            }
            paused = Some(if newer {
                "Round-up is paused: its settings were saved by a newer version of Corral."
            } else {
                "Round-up is paused: its saved settings could not be read."
            });
        }
    }
    // Malformed old entries become review text; they must not make the rest of the settings
    // disappear through the whole-file fallback to defaults.
    if let Some(old) = value.get("Rules").cloned() {
        let convert = |entry: &serde_json::Value| StoredRule {
            data: entry
                .get("Data")
                .and_then(|v| v.as_str())
                .map_or_else(|| entry.to_string(), str::to_owned),
            target: entry
                .get("Target")
                .and_then(|v| v.as_str())
                .map(str::to_owned)
                .filter(|_| entry.get("Data").is_some_and(|d| d.is_string()))
                .unwrap_or_default(),
        };
        let rules: Vec<StoredRule> = match old.as_array() {
            Some(entries) => entries.iter().map(convert).collect(),
            None => vec![convert(&old)],
        };
        if let Some(object) = value.as_object_mut() {
            object.insert(
                "Rules".into(),
                serde_json::to_value(rules).expect("old entries serialize"),
            );
        }
    }
    let mut settings: Settings = serde_json::from_value(value)?;
    if absent {
        // `Settings::deserialize` already converted the old rules; saving needs the copies first.
        settings.round_up.migration_pending = true;
    }
    if keep.block.is_some() {
        settings.round_up.paused_reason = paused.map(str::to_owned);
    }
    if keep.block.is_some() || !settings.round_up.needs_review.is_empty() {
        keep.legacy_rules = top_level_value(text, "Rules").map(|span| text[span].to_owned());
    }
    settings.round_up_keep = keep;
    Ok(settings)
}

/// The text `save` writes. Field order stays that of the struct, `version` stays first inside
/// the block, and kept raw values are pasted back unchanged.
pub fn encode(settings: &Settings) -> io::Result<String> {
    let keep_legacy =
        !settings.round_up.needs_review.is_empty() || settings.round_up_keep.block.is_some();
    let mut copy = settings.clone();
    if !keep_legacy {
        copy.rules.clear();
    }
    let mut text = serde_json::to_string_pretty(&copy).map_err(io::Error::other)?;
    let raw_values = [
        ("RoundUp", settings.round_up_keep.block.as_ref()),
        (
            "Rules",
            settings
                .round_up_keep
                .legacy_rules
                .as_ref()
                .filter(|_| keep_legacy),
        ),
    ];
    for (key, raw) in raw_values {
        if let (Some(raw), Some(span)) = (raw, top_level_value(&text, key)) {
            text.replace_range(span, raw);
        }
    }
    Ok(text)
}

/// Locate a top-level value of an already valid JSON object without touching its whitespace.
/// The scanner only finds spans; serde has done all syntax and type validation.
pub fn top_level_value(text: &str, wanted: &str) -> Option<Range<usize>> {
    let bytes = text.as_bytes();
    fn skip_space(bytes: &[u8], i: &mut usize) {
        while bytes.get(*i).is_some_and(u8::is_ascii_whitespace) {
            *i += 1;
        }
    }
    fn quoted(bytes: &[u8], i: &mut usize) -> Option<()> {
        if bytes.get(*i) != Some(&b'"') {
            return None;
        }
        *i += 1;
        while let Some(&b) = bytes.get(*i) {
            *i += 1;
            if b == b'\\' {
                *i += 1;
            } else if b == b'"' {
                return Some(());
            }
        }
        None
    }
    let mut i = 0;
    skip_space(bytes, &mut i);
    if bytes.get(i) != Some(&b'{') {
        return None;
    }
    i += 1;
    loop {
        skip_space(bytes, &mut i);
        let key_start = i;
        quoted(bytes, &mut i)?;
        let key: String = serde_json::from_str(&text[key_start..i]).ok()?;
        skip_space(bytes, &mut i);
        if bytes.get(i) != Some(&b':') {
            return None;
        }
        i += 1;
        skip_space(bytes, &mut i);
        let start = i;
        let mut depth = 0usize;
        while let Some(&b) = bytes.get(i) {
            match b {
                b'"' => {
                    quoted(bytes, &mut i)?;
                    continue;
                }
                b'[' | b'{' => depth += 1,
                b']' | b'}' if depth > 0 => depth -= 1,
                b',' | b'}' if depth == 0 => break,
                _ => {}
            }
            i += 1;
        }
        let mut end = i;
        while end > start && bytes[end - 1].is_ascii_whitespace() {
            end -= 1;
        }
        if key == wanted {
            return Some(start..end);
        }
        if bytes.get(i) != Some(&b',') {
            return None;
        }
        i += 1;
    }
}

/// `<name>.before-round-up.<UTC stamp>.json` next to `source`, created exclusively (an existing
/// copy is never overwritten) and synced. `Ok(None)` when `source` does not exist.
pub fn backup_copy(source: &Path) -> io::Result<Option<PathBuf>> {
    let mut input = match fs::File::open(source) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !input.metadata()?.is_file() {
        return Err(io::Error::other("configuration is not a file"));
    }
    let stem = source
        .file_stem()
        .ok_or_else(|| io::Error::other("configuration path has no name"))?
        .to_string_lossy()
        .into_owned();
    let stamp = crate::round_up_time::utc_stamp(crate::round_up_time::Clock::now().now_ms);
    for attempt in 0..1000 {
        let suffix = if attempt == 0 {
            String::new()
        } else {
            format!("-{attempt}")
        };
        let destination =
            source.with_file_name(format!("{stem}.before-round-up.{stamp}{suffix}.json"));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
        {
            Ok(mut output) => {
                let copied = io::copy(&mut input, &mut output).and_then(|_| output.sync_all());
                if let Err(error) = copied {
                    drop(output);
                    let _ = fs::remove_file(&destination);
                    return Err(error);
                }
                return Ok(Some(destination));
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::other("could not allocate a unique backup name"))
}

/// Copy the settings file about to be replaced by its first migrated version. Nothing to do when
/// the file is missing or already holds a `RoundUp` block (round-up-spec 6.6).
pub fn backup_settings_before_migration(path: &Path) -> io::Result<()> {
    let Ok(text) = fs::read_to_string(path) else {
        return if path.exists() {
            backup_copy(path).map(|_| ())
        } else {
            Ok(())
        };
    };
    if top_level_value(text.trim_start_matches('\u{feff}'), "RoundUp").is_some() {
        return Ok(());
    }
    backup_copy(path).map(|_| ())
}

/// Round-up-spec 6.6: before the first migrated save both configuration files are copied. On
/// success the migration may be saved; on failure Round-up stays paused with a visible reason
/// and `settings.round_up.migration_pending` stays set (the app does not save the migration).
pub fn prepare_migration(
    settings_path: &Path,
    layout_path: &Path,
    settings: &mut Settings,
) -> io::Result<()> {
    if !settings.round_up.migration_pending {
        return Ok(());
    }
    let result = backup_settings_before_migration(settings_path)
        .and_then(|()| backup_copy(layout_path).map(|_| ()));
    match result {
        Ok(()) => {
            settings.round_up.migration_pending = false;
            settings.round_up.paused_reason = None;
            Ok(())
        }
        Err(error) => {
            settings.round_up.paused_reason = Some(format!(
                "Round-up is paused: the settings and layout could not be backed up. {error}"
            ));
            Err(error)
        }
    }
}

/// Write `text` exactly where `save` writes, used by the tests of this module.
#[cfg(test)]
fn write(path: &Path, text: &str) {
    use std::io::Write;
    let mut file = fs::File::create(path).unwrap();
    file.write_all(text.as_bytes()).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "corral-round-up-store-{name}-{}-{}",
            std::process::id(),
            crate::round_up_time::Clock::now().now_ms
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn newer_block_is_kept_byte_for_byte_and_pauses_round_up() {
        let raw = "{ \"version\" : 300, \"future\" : [1, {\"text\":\"}, \\\"RoundUp\\\": [\"}], \"rules\": null }";
        let text =
            format!("{{\"RoundUp\":{raw},\"Rules\": [],\"DefaultCorral\":-1,\"Disabled\":false}}");
        let mut settings = decode(&text).unwrap();
        assert!(settings.round_up.paused_reason.is_some());
        assert!(!settings.round_up.migration_pending);
        settings.disabled = true;
        let saved = encode(&settings).unwrap();
        assert_eq!(&saved[top_level_value(&saved, "RoundUp").unwrap()], raw);
        assert!(
            serde_json::from_str::<serde_json::Value>(&saved).unwrap()["Disabled"]
                .as_bool()
                .unwrap()
        );
        assert_eq!(
            decode(&saved).unwrap().round_up_keep.block.as_deref(),
            Some(raw)
        );
    }

    #[test]
    fn unreadable_block_of_a_known_version_is_kept_and_does_not_reset_the_settings() {
        let raw = "{\"version\":1,\"rules\":\"not a list\"}";
        let settings = decode(&format!("{{\"Disabled\":true,\"RoundUp\":{raw}}}")).unwrap();
        assert!(settings.disabled);
        assert!(settings.round_up.paused_reason.is_some());
        let saved = encode(&settings).unwrap();
        assert_eq!(&saved[top_level_value(&saved, "RoundUp").unwrap()], raw);
    }

    #[test]
    fn review_items_keep_the_old_rules_untouched_and_clearing_them_drops_only_rules() {
        let rules = "[ {\"Data\":\"unconvertible text\", \"Target\":\"-1\"} ]";
        let text = format!("{{\"Rules\":{rules},\"DefaultCorral\": -7}}");
        let mut settings = decode(&text).unwrap();
        assert!(!settings.round_up.needs_review.is_empty());
        let saved = encode(&settings).unwrap();
        assert_eq!(&saved[top_level_value(&saved, "Rules").unwrap()], rules);
        assert_eq!(
            &saved[top_level_value(&saved, "DefaultCorral").unwrap()],
            "-7"
        );
        settings.round_up.needs_review.clear();
        let saved = encode(&settings).unwrap();
        assert!(top_level_value(&saved, "Rules").is_none());
        assert_eq!(
            &saved[top_level_value(&saved, "DefaultCorral").unwrap()],
            "-7"
        );
    }

    #[test]
    fn malformed_old_entries_are_reviewed_without_resetting_the_settings() {
        for rules in [
            r#"[{"Data":"unfinished"}]"#,
            r#"[42,null,{"Data":false,"Target":-1}]"#,
            r#"{"broken":"outer shape"}"#,
        ] {
            let input = format!("{{\"Disabled\":true,\"Rules\":{rules}}}");
            let settings = decode(&input).unwrap();
            assert!(settings.disabled);
            assert!(!settings.round_up.needs_review.is_empty(), "{rules}");
            let output = encode(&settings).unwrap();
            assert_eq!(&output[top_level_value(&output, "Rules").unwrap()], rules);
        }
    }

    #[test]
    fn typed_block_roundtrips_with_version_first_and_is_not_migrated_again() {
        let settings = decode("{}").unwrap();
        assert!(settings.round_up.migration_pending);
        let mut settings = settings;
        settings.round_up.migration_pending = false;
        let saved = encode(&settings).unwrap();
        let block = &saved[top_level_value(&saved, "RoundUp").unwrap()];
        assert!(
            block
                .trim_start_matches(['{', ' ', '\n', '\r'])
                .starts_with("\"version\"")
        );
        let again = decode(&saved).unwrap();
        assert!(!again.round_up.migration_pending);
        assert_eq!(again.round_up, settings.round_up);
        assert_eq!(encode(&again).unwrap(), saved);
        assert!(top_level_value(&saved, "Rules").is_none());
    }

    #[test]
    fn backup_precedes_the_migrated_save_is_exclusive_and_idempotent() {
        let dir = scratch("backup");
        let settings_path = dir.join("settings.json");
        let layout_path = dir.join("layout.json");
        let original = "{\"DefaultCorral\":-1,\"Rules\":[]}";
        let layout = "{\"version\":1,\"unread\":\"exact bytes\"}";
        write(&settings_path, original);
        write(&layout_path, layout);
        let mut settings = super::super::load_file(&settings_path).unwrap();
        assert!(settings.round_up.migration_pending);
        prepare_migration(&settings_path, &layout_path, &mut settings).unwrap();
        assert!(!settings.round_up.migration_pending);
        let copies = |needle: &str| -> Vec<String> {
            fs::read_dir(&dir)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|n| n.contains(".before-round-up.") && n.starts_with(needle))
                .collect()
        };
        assert_eq!(copies("settings").len(), 1);
        assert_eq!(copies("layout").len(), 1);
        for name in copies("settings") {
            assert_eq!(fs::read_to_string(dir.join(name)).unwrap(), original);
        }
        for name in copies("layout") {
            assert_eq!(fs::read_to_string(dir.join(name)).unwrap(), layout);
        }
        // Not pending any more: a second call copies nothing.
        prepare_migration(&settings_path, &layout_path, &mut settings).unwrap();
        assert_eq!(copies("settings").len() + copies("layout").len(), 2);
        // A second backup in the same second is a new file, never an overwrite.
        let second = backup_copy(&settings_path).unwrap().unwrap();
        assert_eq!(fs::read_to_string(second).unwrap(), original);
        assert_eq!(copies("settings").len(), 2);
        super::super::save(&settings_path, &settings).unwrap();
        let reloaded = super::super::load_file(&settings_path).unwrap();
        assert!(!reloaded.round_up.migration_pending);
        // Once the file holds the block no further copy is taken.
        backup_settings_before_migration(&settings_path).unwrap();
        assert_eq!(copies("settings").len(), 2);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_backup_blocks_the_migration_and_leaves_the_sources_alone() {
        let dir = scratch("failed");
        let settings_path = dir.join("settings.json");
        let original = "{\"Rules\":[]}";
        write(&settings_path, original);
        // The layout path is a directory: it exists but cannot be copied as a file.
        let layout_path = dir.join("layout.json");
        fs::create_dir(&layout_path).unwrap();
        let mut settings = super::super::load_file(&settings_path).unwrap();
        assert!(prepare_migration(&settings_path, &layout_path, &mut settings).is_err());
        assert!(settings.round_up.migration_pending);
        assert!(
            settings
                .round_up
                .paused_reason
                .as_deref()
                .unwrap()
                .contains("could not be backed up")
        );
        assert!(!settings.round_up.active());
        assert_eq!(fs::read_to_string(&settings_path).unwrap(), original);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn saving_a_pending_migration_without_the_engine_copies_the_old_file_first() {
        let dir = scratch("direct-save");
        let path = dir.join("settings.json");
        let original = "{\"Disabled\":true,\"Rules\":[]}";
        write(&path, original);
        let settings = super::super::load_file(&path).unwrap();
        super::super::save(&path, &settings).unwrap();
        let copies: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.to_string_lossy().contains(".before-round-up."))
            .collect();
        assert_eq!(copies.len(), 1);
        assert_eq!(fs::read_to_string(&copies[0]).unwrap(), original);
        fs::remove_dir_all(dir).unwrap();
    }
}
