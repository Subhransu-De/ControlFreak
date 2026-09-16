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

/// Distinguishes completed work, retained data and failed recovery.
#[derive(Debug, PartialEq, Eq)]
pub enum CleanupOutcome {
    Complete,
    Retained,
    RecoveryRequired,
}

pub fn write(path: &Path, original: Option<&str>, updated: &str, backup: bool) -> Result<()> {
    if let Some(original) = original {
        let mut file = prepare_existing(path, original, backup)?;
        return match update_existing(&mut file, original, updated, overwrite) {
            CleanupOutcome::Complete => Ok(()),
            CleanupOutcome::Retained => {
                Err("Configuration update failed; original content was restored.")
            }
            CleanupOutcome::RecoveryRequired if backup => {
                Err("Configuration update and rollback failed; restore the private backup.")
            }
            CleanupOutcome::RecoveryRequired => Err(
                "Setup receipt update and rollback failed; client entries may require manual cleanup.",
            ),
        };
    }
    let parent = path
        .parent()
        .ok_or("Configuration has no parent directory.")?;
    fs::create_dir_all(parent).map_err(|_| "Cannot create configuration directory.")?;
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
    Ok(())
}

/// Every preparation failure is non-mutating. Optional cleanup retains that file.
/// A failed write is separately distinguished from a failed rollback.
pub fn remove_if_unchanged(path: &Path, original: Option<&str>, updated: &str) -> CleanupOutcome {
    let Some(original) = original else {
        return CleanupOutcome::Retained;
    };
    let Ok(mut file) = prepare_existing(path, original, true) else {
        return CleanupOutcome::Retained;
    };
    update_existing(&mut file, original, updated, overwrite)
}

fn prepare_existing(path: &Path, original: &str, backup: bool) -> Result<File> {
    let file = lock_existing(path, original)?;
    if backup {
        // Inspect the locked source before creating or writing any backup.
        validate_backup_attributes(
            file.metadata()
                .map_err(|_| "Cannot inspect configuration.")?
                .file_attributes(),
        )?;
        save_backup(path, original)?;
    }
    Ok(file)
}

fn update_existing(
    file: &mut File,
    original: &str,
    updated: &str,
    mut write: impl FnMut(&mut File, &[u8]) -> std::io::Result<()>,
) -> CleanupOutcome {
    let updated = if original.starts_with('\u{feff}') {
        format!("\u{feff}{updated}")
    } else {
        updated.to_owned()
    };
    // Retain the exclusive handle through update and rollback. No competing
    // writer or rename can invalidate the snapshot in either operation.
    if write(file, updated.as_bytes()).is_ok() {
        CleanupOutcome::Complete
    } else if write(file, original.as_bytes()).is_ok() {
        CleanupOutcome::Retained
    } else {
        CleanupOutcome::RecoveryRequired
    }
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
        assert_eq!(
            remove_if_unchanged(&path, Some("original"), "removed"),
            CleanupOutcome::Retained
        );
        let guard = lock_existing(&path, "newer").unwrap();
        assert_eq!(
            remove_if_unchanged(&path, Some("newer"), "removed"),
            CleanupOutcome::Retained
        );
        drop(guard);
        assert_eq!(fs::read_to_string(&path).unwrap(), "newer");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
        assert_eq!(
            remove_if_unchanged(&path, Some("newer"), "removed"),
            CleanupOutcome::Complete
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "removed");
    }

    #[test]
    fn backup_failure_happens_before_any_configuration_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("{}.json", "x".repeat(235)));
        fs::write(&path, "original").unwrap();
        assert!(lock_existing(&path, "original").is_ok());
        assert_eq!(
            prepare_existing(&path, "original", true).unwrap_err(),
            "Cannot create configuration backup."
        );
        assert_eq!(
            remove_if_unchanged(&path, Some("original"), "updated"),
            CleanupOutcome::Retained
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "original");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn failed_update_distinguishes_restored_content_from_recovery() {
        for fail_rollback in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("config.json");
            fs::write(&path, "original").unwrap();
            let mut file = prepare_existing(&path, "original", true).unwrap();
            let mut attempts = 0;
            let outcome = update_existing(&mut file, "original", "updated", |file, bytes| {
                attempts += 1;
                if attempts == 1 || fail_rollback {
                    overwrite(file, b"partial")?;
                    return Err(std::io::Error::other("synthetic write failure"));
                }
                overwrite(file, bytes)
            });
            drop(file);
            assert_eq!(attempts, 2);
            assert_eq!(
                outcome,
                if fail_rollback {
                    CleanupOutcome::RecoveryRequired
                } else {
                    CleanupOutcome::Retained
                }
            );
            assert_eq!(
                fs::read_to_string(&path).unwrap(),
                if fail_rollback { "partial" } else { "original" }
            );
            let backup = fs::read_dir(dir.path())
                .unwrap()
                .map(|item| item.unwrap().path())
                .find(|item| item.extension().is_some_and(|ext| ext == "bak"))
                .unwrap();
            assert_eq!(fs::read_to_string(backup).unwrap(), "original");
        }
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
