//! Shared atomic file replacement for layout and settings bytes.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".bak");
    PathBuf::from(name)
}

/// Write and sync `<path>.tmp`, close it, copy the old target to `.bak`, then rename.
/// The serialization result is checked after opening the temp file to preserve layout save's
/// error precedence and temp-file side effects. Callers that return serialization errors before
/// filesystem work must check those errors themselves and pass `Ok(bytes)`.
pub fn write_atomic_with_backup(path: &Path, bytes: io::Result<Vec<u8>>) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut temp = path.as_os_str().to_owned();
    temp.push(".tmp");
    let temp = PathBuf::from(temp);
    {
        let mut file = fs::File::create(&temp)?;
        file.write_all(&bytes?)?;
        file.sync_all()?;
    }
    if path.exists() {
        fs::copy(path, backup_path(path))?;
    }
    fs::rename(&temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "corral-persistence-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
        ));
        fs::create_dir(&dir).unwrap();
        dir
    }

    #[test]
    fn first_write_creates_parents_and_target_without_backup() {
        let dir = temp_dir("first");
        let path = dir.join("nested").join("data.json");
        let bytes = b"{\n  \"value\": 1\n}";
        write_atomic_with_backup(&path, Ok(bytes.to_vec())).unwrap();
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert!(!backup_path(&path).exists());
        assert!(!path.with_file_name("data.json.tmp").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn second_write_backs_up_previous_bytes_and_replaces_target() {
        let dir = temp_dir("second");
        let path = dir.join("data.json");
        let old = b"previous\r\nbytes\x00";
        let new = b"new\nbytes";
        write_atomic_with_backup(&path, Ok(old.to_vec())).unwrap();
        write_atomic_with_backup(&path, Ok(new.to_vec())).unwrap();
        assert_eq!(fs::read(backup_path(&path)).unwrap(), old);
        assert_eq!(fs::read(&path).unwrap(), new);
        assert!(!dir.join("data.json.tmp").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn blocked_temp_leaves_target_and_backup_untouched() {
        let dir = temp_dir("blocked");
        let path = dir.join("data.json");
        let temp = dir.join("data.json.tmp");
        fs::write(&path, b"target").unwrap();
        fs::write(backup_path(&path), b"backup").unwrap();
        fs::create_dir(&temp).unwrap();
        let expected = fs::File::create(&temp).unwrap_err().kind();
        let error = write_atomic_with_backup(&path, Ok(b"new".to_vec())).unwrap_err();
        assert_eq!(error.kind(), expected);
        assert_eq!(fs::read(&path).unwrap(), b"target");
        assert_eq!(fs::read(backup_path(&path)).unwrap(), b"backup");
        assert!(temp.is_dir());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn serialization_error_leaves_truncated_temp_and_preserves_files() {
        let dir = temp_dir("serialize");
        let path = dir.join("data.json");
        let temp = dir.join("data.json.tmp");
        fs::write(&path, b"target").unwrap();
        fs::write(backup_path(&path), b"backup").unwrap();
        fs::write(&temp, b"leftover").unwrap();
        let error = write_atomic_with_backup(&path, Err(io::Error::other("serialization"))).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(error.to_string(), "serialization");
        assert_eq!(fs::read(&temp).unwrap(), b"");
        assert_eq!(fs::read(&path).unwrap(), b"target");
        assert_eq!(fs::read(backup_path(&path)).unwrap(), b"backup");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn temp_open_error_precedes_serialization_error() {
        let dir = temp_dir("precedence");
        let path = dir.join("data.json");
        let temp = dir.join("data.json.tmp");
        fs::create_dir(&temp).unwrap();
        let expected = fs::File::create(&temp).unwrap_err().kind();
        let error = write_atomic_with_backup(&path, Err(io::Error::other("serialization"))).unwrap_err();
        assert_eq!(error.kind(), expected);
        assert!(!path.exists());
        assert!(!backup_path(&path).exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn backup_failure_leaves_complete_temp_and_target_untouched() {
        let dir = temp_dir("backup");
        let path = dir.join("data.json");
        fs::write(&path, b"target").unwrap();
        fs::create_dir(backup_path(&path)).unwrap();
        let expected = fs::copy(&path, backup_path(&path)).unwrap_err().kind();
        let error = write_atomic_with_backup(&path, Ok(b"new bytes".to_vec())).unwrap_err();
        assert_eq!(error.kind(), expected);
        assert_eq!(fs::read(&path).unwrap(), b"target");
        assert_eq!(fs::read(dir.join("data.json.tmp")).unwrap(), b"new bytes");
        assert!(backup_path(&path).is_dir());
        fs::remove_dir_all(dir).unwrap();
    }
}
