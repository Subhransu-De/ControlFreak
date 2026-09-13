//! Same-directory atomic replacement, bounded reads and recoverable private backups.
use crate::config::Result;
use std::{
    fs::{self, File},
    io::{Read, Write},
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
    let mut text = String::new();
    file.take(MAX_CONFIG + 1)
        .read_to_string(&mut text)
        .map_err(|_| "Cannot read UTF-8 configuration.")?;
    if text.len() as u64 > MAX_CONFIG {
        return Err("Configuration exceeds the 8 MiB safety limit.");
    }
    Ok(Some(text))
}

pub fn write(path: &Path, original: Option<&str>, updated: &str, backup: bool) -> Result<()> {
    let parent = path
        .parent()
        .ok_or("Configuration has no parent directory.")?;
    fs::create_dir_all(parent).map_err(|_| "Cannot create configuration directory.")?;
    let mut replacement =
        NamedTempFile::new_in(parent).map_err(|_| "Cannot stage configuration update.")?;
    if original.is_some() {
        controlfreak_platform::copy_configuration_permissions(path, replacement.path())
            .map_err(|_| "Cannot preserve configuration permissions.")?;
    }
    if original.is_some_and(|text| text.starts_with('\u{feff}')) {
        replacement
            .write_all("\u{feff}".as_bytes())
            .map_err(|_| "Cannot write encoding marker.")?;
    }
    replacement
        .write_all(updated.as_bytes())
        .map_err(|_| "Cannot write staged configuration.")?;
    replacement
        .as_file()
        .sync_all()
        .map_err(|_| "Cannot flush staged configuration.")?;
    if backup && let Some(original) = original {
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
    }
    if read(path)?.as_deref() != original {
        return Err("Configuration changed during setup; close the client and retry.");
    }
    replacement
        .persist(path)
        .map_err(|_| "Cannot replace configuration; close the client and retry.")?;
    Ok(())
}
