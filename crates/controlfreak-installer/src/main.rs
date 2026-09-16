#![forbid(unsafe_code)]
//! Private setup utility, separate from the MCP executable and protocol.
mod clients;
mod config;
mod storage;

use config::{Document, Result};
use serde::{Deserialize, Serialize};
use std::{
    env,
    fmt::Write as _,
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    process::ExitCode,
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    client: String,
    path: PathBuf,
    entry: String,
    #[serde(default = "legacy_receipt_committed")]
    committed: bool,
}

fn legacy_receipt_committed() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ReceiptFile {
    Legacy(Receipt),
    Multiple(Vec<Receipt>),
}

fn receipts(text: &str, id: &str) -> Result<Vec<Receipt>> {
    let stored: ReceiptFile =
        serde_json::from_str(text).map_err(|_| "Invalid setup receipt; configuration retained.")?;
    let receipts = match stored {
        ReceiptFile::Legacy(receipt) => vec![receipt],
        ReceiptFile::Multiple(receipts) => receipts,
    };
    if receipts
        .iter()
        .any(|receipt| receipt.client != id || !receipt.path.is_absolute())
    {
        return Err("Invalid setup receipt; configuration retained.");
    }
    Ok(receipts)
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            // Fixed messages only: never echo private configuration contents.
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<()> {
    match args {
        [command, installed] if command == "check-version" => {
            let installed = semver::Version::parse(installed.trim())
                .map_err(|_| "Cannot determine the installed version; uninstall it first.")?;
            let current = semver::Version::parse(env!("CARGO_PKG_VERSION"))
                .map_err(|_| "Invalid setup version.")?;
            if installed > current {
                return Err(
                    "A newer ControlFreak version is installed. Downgrades are not supported.",
                );
            }
            Ok(())
        }
        [command, id, executable, report] if command == "probe" => {
            report_result(Path::new(report), probe(id, Path::new(executable)))
        }
        [command, directory, report] if command == "blockers" => {
            blockers_report(Path::new(directory), Path::new(report))
        }
        [command, id, executable, state, report, policy] if command == "configure" => {
            report_result(
                Path::new(report),
                configure(id, Path::new(executable), Path::new(state), policy),
            )
        }
        [command, state, report] if command == "remove" => {
            report_cleanup(Path::new(report), remove(Path::new(state)))
        }
        _ => Err("Invalid installer helper arguments."),
    }
}

fn blockers_report(directory: &Path, report: &Path) -> Result<()> {
    if !directory.is_absolute() {
        return Err("Installation directory must be absolute.");
    }
    let files: Vec<_> = ["controlfreak.exe", "controlfreak-installer.exe"]
        .iter()
        .map(|name| directory.join(name))
        .filter(|path| path.is_file())
        .collect();
    let (message, action, names) = match controlfreak_platform::installer_blockers(&files) {
        Ok(blockers) => {
            let names: Vec<_> = blockers
                .into_iter()
                .filter(|item| item.pid != std::process::id())
                .map(|item| format!("{} - PID: {}", item.name, item.pid))
                .collect();
            if names.is_empty() {
                (
                    "Setup cannot replace the ControlFreak files.",
                    "Close clients using ControlFreak and check that you can write to the installation folder. Windows could not identify a blocking process.",
                    names,
                )
            } else {
                (
                    "ControlFreak is in use. Setup cannot update it while these processes are running.",
                    "Close the clients using ControlFreak, or open Task Manager > Details and end the processes listed below.",
                    names,
                )
            }
        }
        Err(_) => (
            "Setup cannot replace the ControlFreak files.",
            "Close clients using ControlFreak and check that you can write to the installation folder. Windows could not identify the blocking processes.",
            Vec::new(),
        ),
    };
    // Windows' INI reader expects UTF-16 with BOM for non-ASCII application names.
    // Keep each process on its own key: INI values cannot contain line breaks.
    // This private temporary report is read by Setup, never printed or uploaded.
    let mut text = format!(
        "[result]\r\nmessage={message}\r\naction={action}\r\ncount={}\r\n",
        names.len()
    );
    for (index, name) in names.iter().enumerate() {
        write!(text, "process{index}={name}\r\n")
            .map_err(|_| "Cannot format blocker diagnostics.")?;
    }
    let bytes: Vec<_> = std::iter::once(0xfeff_u16)
        .chain(text.encode_utf16())
        .flat_map(u16::to_le_bytes)
        .collect();
    fs::write(report, bytes).map_err(|_| "Cannot write blocker diagnostics.")
}

fn report_result(path: &Path, result: Result<String>) -> Result<()> {
    let (status, message) = match result {
        Ok(message) => ("ok", message),
        Err(e) => ("error", e.to_owned()),
    };
    let message = message.replace(['\r', '\n'], " ");
    let text = format!("[result]\r\nstatus={status}\r\nmessage={message}\r\n");
    fs::write(path, text).map_err(|_| "Cannot write setup result.")?;
    if status == "error" {
        Err("Client configuration was not completed; see the setup result.")
    } else {
        Ok(())
    }
}

fn load_document(client: &clients::Client) -> Result<(Option<String>, Document)> {
    let original = storage::read(&client.path)?;
    let empty = if client.id == "codex" { "" } else { "{}" };
    let document = Document::parse(
        original.as_deref().unwrap_or(empty),
        client.id == "codex",
        matches!(client.id.as_str(), "opencode" | "pi"),
    )?;
    Ok((original, document))
}

fn probe(id: &str, executable: &Path) -> Result<String> {
    clients::executable(executable)?;
    let client = clients::resolve(id)?;
    let (_, document) = load_document(&client)?;
    let entry = document.entry(clients::group(id))?;
    let detection = if client.detected {
        "Client files or command detected"
    } else {
        "Client not detected; configuration can still be created"
    };
    let conflict = if entry.is_some() {
        " Existing ControlFreak entry: kept unless replacement is selected."
    } else {
        ""
    };
    Ok(format!("{detection}.{conflict} {}", client.note))
}

fn lock() -> Result<fs::File> {
    let local = env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA is required.")?;
    let dir = PathBuf::from(local).join("ControlFreak");
    if !dir.is_absolute() {
        return Err("LOCALAPPDATA must be absolute.");
    }
    fs::create_dir_all(&dir).map_err(|_| "Cannot create setup lock directory.")?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("installer.lock"))
        .map_err(|_| "Cannot open setup lock.")?;
    file.try_lock().map_err(
        |_| "Another ControlFreak setup is updating configuration; retry after it finishes.",
    )?;
    Ok(file)
}

fn configure(id: &str, executable: &Path, state: &Path, policy: &str) -> Result<String> {
    if !matches!(policy, "keep" | "replace") {
        return Err("Invalid conflict policy.");
    }
    let executable = clients::executable(executable)?;
    let _lock = lock()?;
    let client = clients::resolve(id)?;
    let (original, mut document) = load_document(&client)?;
    let group = clients::group(id);
    let existing = document.entry(group)?;
    let desired = config::desired(&executable, id == "codex", id == "opencode");
    if existing
        .as_deref()
        .is_some_and(|entry| config::equivalent(entry, &desired, id == "codex"))
    {
        return Ok(format!("Already configured. {}", client.note));
    }
    if existing.is_some() && policy == "keep" {
        return Ok(
            "Existing ControlFreak entry kept. Select replacement during setup to change it."
                .to_owned(),
        );
    }
    document.set(group, Some(&desired))?;
    let receipt = Receipt {
        client: id.to_owned(),
        path: client.path.clone(),
        entry: document
            .entry(group)?
            .ok_or("Cannot verify configured entry.")?,
        committed: false,
    };
    let receipt_path = state.join(format!("{id}.json"));
    let old_receipt = storage::read(&receipt_path)?;
    let mut history = old_receipt
        .as_deref()
        .map(|text| receipts(text, id))
        .transpose()?
        .unwrap_or_default();
    // Journal the attempted write without claiming ownership. An interrupted or
    // failed configuration write must not authorize deleting a later manual entry.
    history.retain(|old| old.committed);
    history.push(receipt);
    let pending = serde_json::to_string(&history).map_err(|_| "Cannot encode setup receipt.")?;
    storage::write(&receipt_path, old_receipt.as_deref(), &pending, false)?;
    storage::write(&client.path, original.as_deref(), &document.render(), true)?;
    let mut written = history.pop().ok_or("Missing pending receipt.")?;
    written.committed = true;
    // Only the latest successful write owns this path. Older profile paths
    // remain tracked, but a user reverting an entry must not match stale history.
    history.retain(|old| old.path != written.path);
    history.push(written);
    let committed = serde_json::to_string(&history).map_err(|_| "Cannot encode setup receipt.")?;
    storage::write(&receipt_path, Some(&pending), &committed, false)?;
    Ok(format!(
        "Configured. Existing file backed up beside the configuration when present. {}",
        client.note
    ))
}

#[derive(Default)]
struct CleanupReport {
    retained: Vec<&'static str>,
    recovery: Vec<&'static str>,
}

fn report_cleanup(path: &Path, result: Result<CleanupReport>) -> Result<()> {
    let report = match result {
        Ok(report) => report,
        Err(error) => return report_result(path, Err(error)),
    };
    let mut message = String::new();
    if !report.retained.is_empty() {
        write!(message, "Cleanup incomplete for: {}. Entries or setup receipts were retained; remove remaining ControlFreak entries manually. ", report.retained.join(", ")).map_err(|_| "Cannot format cleanup result.")?;
    }
    if !report.recovery.is_empty() {
        write!(message, "Configuration write and rollback failed for: {}. Restore the configuration from its adjacent .controlfreak-backup-*.bak file before using that client. ", report.recovery.join(", ")).map_err(|_| "Cannot format cleanup result.")?;
    }
    let status = if message.is_empty() {
        message.push_str("Installer-created MCP entries removed. ");
        "ok"
    } else {
        "warning"
    };
    message.push_str("Configuration backups were retained.");
    fs::write(
        path,
        format!("[result]\r\nstatus={status}\r\nmessage={message}\r\n"),
    )
    .map_err(|_| "Cannot write setup result.")?;
    Ok(())
}

fn remove(state: &Path) -> Result<CleanupReport> {
    let _lock = lock()?;
    let mut report = CleanupReport::default();
    for id in clients::CLIENTS {
        let path = state.join(format!("{id}.json"));
        let Ok(stored) = storage::read(&path) else {
            report.retained.push(id);
            continue;
        };
        let Some(text) = stored else {
            continue;
        };
        let Ok(receipts) = receipts(&text, id) else {
            report.retained.push(id);
            continue;
        };
        let mut retained = false;
        let mut recovery = false;
        let total = receipts.len();
        let mut remaining = Vec::new();
        for receipt in receipts {
            match remove_entry(&receipt) {
                storage::CleanupOutcome::Complete => (),
                storage::CleanupOutcome::Retained => {
                    retained = true;
                    remaining.push(receipt);
                }
                storage::CleanupOutcome::RecoveryRequired => {
                    recovery = true;
                    remaining.push(receipt);
                }
            }
        }
        // Keep unfinished profiles, dropping completed ownership records where
        // possible. Receipt maintenance is optional and never stops other clients.
        if remaining.is_empty() {
            retained |= fs::remove_file(path).is_err();
        } else if remaining.len() != total {
            match serde_json::to_string(&remaining) {
                Ok(updated) => {
                    retained |= storage::write(&path, Some(&text), &updated, false).is_err();
                }
                Err(_) => retained = true,
            }
        }
        if retained {
            report.retained.push(id);
        }
        if recovery {
            report.recovery.push(id);
        }
    }
    Ok(report)
}

fn remove_entry(receipt: &Receipt) -> storage::CleanupOutcome {
    use storage::CleanupOutcome;
    if !receipt.committed {
        return CleanupOutcome::Retained;
    }
    let client = clients::Client {
        id: receipt.client.clone(),
        path: receipt.path.clone(),
        detected: true,
        note: "",
    };
    let Ok((original, mut document)) = load_document(&client) else {
        return CleanupOutcome::Retained;
    };
    let group = clients::group(&receipt.client);
    let Ok(entry) = document.entry(group) else {
        return CleanupOutcome::Retained;
    };
    let Some(entry) = entry else {
        return CleanupOutcome::Complete;
    };
    if !config::equivalent(&entry, &receipt.entry, receipt.client == "codex")
        || document.set(group, None).is_err()
    {
        return CleanupOutcome::Retained;
    }
    storage::remove_if_unchanged(&client.path, original.as_deref(), &document.render())
}

#[cfg(test)]
mod cleanup_tests {
    use super::*;

    #[test]
    fn recovery_report_identifies_client_and_backup_without_exposing_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("result.ini");
        let report = CleanupReport {
            retained: vec!["codex"],
            recovery: vec!["claude-code"],
        };
        assert!(report_cleanup(&path, Ok(report)).is_ok());
        let report = fs::read_to_string(path).unwrap();
        assert!(report.contains("status=warning"));
        assert!(report.contains("Cleanup incomplete for: codex"));
        assert!(report.contains("rollback failed for: claude-code"));
        assert!(report.contains(".controlfreak-backup-*.bak"));
    }
}
