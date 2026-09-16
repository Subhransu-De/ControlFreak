//! Bounded reads, exclusive updates and recoverable private backups.
use crate::config::Result;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::windows::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
};
use tempfile::NamedTempFile;

const MAX_CONFIG: u64 = 8 * 1024 * 1024;

pub fn read(path: &Path) -> Result<Option<String>> {
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err("Symbolic-link configurations require manual setup.");
    }
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("Cannot read configuration; check access permissions."),
    };
    bounded_text(file).map(Some)
}

fn bounded_text(file: impl Read) -> Result<String> {
    let mut text = String::new();
    file.take(MAX_CONFIG + 1)
        .read_to_string(&mut text)
        .map_err(|_| "Cannot read UTF-8 configuration.")?;
    if text.len() as u64 > MAX_CONFIG {
        return Err("Configuration exceeds the 8 MiB safety limit.");
    }
    Ok(text)
}

fn lock_existing(path: &Path, original: &str) -> Result<File> {
    // Deny readers, writers and delete/rename handles until the update is flushed.
    // OPEN_REPARSE_POINT lets us reject symlinks rather than follow a raced link.
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .custom_flags(0x0020_0000)
        .open(path)
        .map_err(|_| "Cannot lock configuration; close the client and retry.")?;
    if file
        .metadata()
        .map_err(|_| "Cannot inspect configuration.")?
        .file_type()
        .is_symlink()
    {
        return Err("Symbolic-link configurations require manual setup.");
    }
    if bounded_text(&mut file)? != original {
        return Err("Configuration changed during setup; close the client and retry.");
    }
    Ok(file)
}

pub fn write(path: &Path, original: Option<&str>, updated: &str, backup: bool) -> Result<()> {
    write_checked(path, original, updated, backup, false).map(|_| ())
}

/// A conflict before any write leaves the client configuration untouched.
pub fn remove_if_unchanged(path: &Path, original: Option<&str>, updated: &str) -> Result<bool> {
    if original.is_none() {
        return Ok(false);
    }
    write_checked(path, original, updated, true, true)
}

fn write_checked(
    path: &Path,
    original: Option<&str>,
    updated: &str,
    backup: bool,
    retain_on_conflict: bool,
) -> Result<bool> {
    let parent = path
        .parent()
        .ok_or("Configuration has no parent directory.")?;
    fs::create_dir_all(parent).map_err(|_| "Cannot create configuration directory.")?;
    let mut file = match original.map(|text| lock_existing(path, text)).transpose() {
        Ok(file) => file,
        Err(_) if retain_on_conflict => return Ok(false),
        Err(error) => return Err(error),
    };
    let updated = if original.is_some_and(|text| text.starts_with('\u{feff}')) {
        format!("\u{feff}{updated}")
    } else {
        updated.to_owned()
    };
    if let (Some(file), Some(original)) = (file.as_mut(), original) {
        if backup {
            // Check the locked source before creating or writing any backup.
            let protection = validate_backup_attributes(
                file.metadata()
                    .map_err(|_| "Cannot inspect configuration.")?
                    .file_attributes(),
            );
            if retain_on_conflict && protection.is_err() {
                return Ok(false);
            }
            protection?;
            save_backup(path, original)?;
        }
        // A path-based atomic replace cannot retain an exclusive Windows handle
        // on its destination. Write through the verified handle instead: no
        // intervening client write/rename can invalidate the comparison.
        if overwrite(file, updated.as_bytes()).is_err() {
            if overwrite(file, original.as_bytes()).is_err() {
                return Err(
                    "Configuration update and rollback failed; restore the private backup.",
                );
            }
            return Err("Configuration update failed; original content was restored.");
        }
    } else {
        let mut replacement =
            NamedTempFile::new_in(parent).map_err(|_| "Cannot stage configuration.")?;
        replacement
            .write_all(updated.as_bytes())
            .map_err(|_| "Cannot write staged configuration.")?;
        replacement
            .as_file()
            .sync_all()
            .map_err(|_| "Cannot flush staged configuration.")?;
        // If a client created the previously missing path, keep its content.
        replacement
            .persist_noclobber(path)
            .map_err(|_| "Configuration appeared during setup; close the client and retry.")?;
    }
    Ok(true)
}

fn validate_backup_attributes(attributes: u32) -> Result<()> {
    const FILE_ATTRIBUTE_ENCRYPTED: u32 = 0x4000;
    if attributes & FILE_ATTRIBUTE_ENCRYPTED != 0 {
        return Err("Encrypted configurations require manual setup; no backup was created.");
    }
    Ok(())
}

fn overwrite(file: &mut File, bytes: &[u8]) -> std::io::Result<()> {
    file.seek(SeekFrom::Start(0))?;
    file.write_all(bytes)?;
    file.set_len(bytes.len() as u64)?;
    file.sync_all()
}

fn save_backup(path: &Path, original: &str) -> Result<()> {
    let parent = path
        .parent()
        .ok_or("Configuration has no parent directory.")?;
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or("Invalid configuration filename.")?;
    let mut saved = tempfile::Builder::new()
        .prefix(&format!("{name}.controlfreak-backup-"))
        .suffix(".bak")
        .tempfile_in(parent)
        .map_err(|_| "Cannot create configuration backup.")?;
    controlfreak_platform::copy_configuration_permissions(path, saved.path())
        .map_err(|_| "Cannot preserve backup permissions.")?;
    saved
        .write_all(original.as_bytes())
        .map_err(|_| "Cannot write configuration backup.")?;
    saved
        .as_file()
        .sync_all()
        .map_err(|_| "Cannot flush configuration backup.")?;
    saved
        .keep()
        .map_err(|_| "Cannot retain configuration backup.")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn encrypted_configuration_backup_is_refused() {
        assert!(validate_backup_attributes(0x4020).is_err());
        assert!(validate_backup_attributes(0x20).is_ok());
    }

    #[test]
    fn uninstall_retains_a_changed_or_busy_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, "newer").unwrap();
        assert!(!remove_if_unchanged(&path, Some("original"), "removed").unwrap());
        let guard = lock_existing(&path, "newer").unwrap();
        assert!(!remove_if_unchanged(&path, Some("newer"), "removed").unwrap());
        drop(guard);
        assert_eq!(fs::read_to_string(&path).unwrap(), "newer");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
        assert!(remove_if_unchanged(&path, Some("newer"), "removed").unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap(), "removed");
    }

    #[test]
    fn verified_handle_excludes_competing_writes_and_replacements() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, "original").unwrap();
        let mut guard = lock_existing(&path, "original").unwrap();
        assert!(fs::write(&path, "competing write").is_err());
        let replacement = dir.path().join("competing.json");
        fs::write(&replacement, "competing replacement").unwrap();
        assert!(fs::rename(&replacement, &path).is_err());
        overwrite(&mut guard, b"updated").unwrap();
        drop(guard);
        assert_eq!(fs::read_to_string(&path).unwrap(), "updated");
    }
    #[test]
    fn stale_snapshot_and_newly_created_path_are_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, "newer").unwrap();
        assert!(write(&path, Some("original"), "installer", true).is_err());
        assert!(write(&path, None, "installer", true).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "newer");
    }
}
