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
            report_result(Path::new(report), remove(Path::new(state)))
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

fn remove(state: &Path) -> Result<String> {
    let _lock = lock()?;
    let mut retained = false;
    for id in clients::CLIENTS {
        let path = state.join(format!("{id}.json"));
        if let Some(text) = storage::read(&path)? {
            for receipt in receipts(&text, id)? {
                if !receipt.committed {
                    retained = true;
                    continue;
                }
                let client = clients::Client {
                    id: id.to_owned(),
                    path: receipt.path,
                    detected: true,
                    note: "",
                };
                let Ok((original, mut document)) = load_document(&client) else {
                    retained = true;
                    continue;
                };
                let Ok(entry) = document.entry(clients::group(id)) else {
                    retained = true;
                    continue;
                };
                if entry
                    .as_deref()
                    .is_some_and(|entry| config::equivalent(entry, &receipt.entry, id == "codex"))
                {
                    document.set(clients::group(id), None)?;
                    storage::write(&client.path, original.as_deref(), &document.render(), true)?;
                } else {
                    retained = true;
                }
            }
            fs::remove_file(path).map_err(|_| "Cannot remove setup receipt.")?;
        }
    }
    if retained {
        Ok(
            "Modified, unverified, unreadable or missing entries were left unchanged. Configuration backups were retained."
                .to_owned(),
        )
    } else {
        Ok(
            "Installer-created MCP entries removed. Configuration backups were retained."
                .to_owned(),
        )
    }
}
