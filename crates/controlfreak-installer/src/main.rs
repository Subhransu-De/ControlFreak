#![forbid(unsafe_code)]
//! Private setup utility, separate from the MCP executable and protocol.
mod cli_clients;

use cli_clients::Result;
use std::{env, fmt::Write as _, fs, path::Path, process::ExitCode};

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
        [command, report] if command == "probe-clients" => probe_clients_report(Path::new(report)),
        [command, directory, report] if command == "blockers" => {
            blockers_report(Path::new(directory), Path::new(report), false)
        }
        [command, directory, report, mode]
            if command == "blockers" && matches!(mode.as_str(), "install" | "uninstall") =>
        {
            blockers_report(Path::new(directory), Path::new(report), mode == "uninstall")
        }
        [command, id, executable, state, report] if command == "register-cli" => report_result(
            Path::new(report),
            cli_clients::register(id, Path::new(executable), Path::new(state), false),
        ),
        [command, flag, id, executable, state, report]
            if command == "register-cli" && flag == "--allow-elevated" =>
        {
            report_result(
                Path::new(report),
                cli_clients::register(id, Path::new(executable), Path::new(state), true),
            )
        }
        [command, state, report] if command == "remove-cli" => {
            report_cli_cleanup(Path::new(report), Path::new(state), false)
        }
        [command, flag, state, report] if command == "remove-cli" && flag == "--allow-elevated" => {
            report_cli_cleanup(Path::new(report), Path::new(state), true)
        }
        _ => Err("Invalid installer helper arguments."),
    }
}

fn write_utf16_ini(path: &Path, text: &str, error_message: &'static str) -> Result<()> {
    // Windows' INI reader expects UTF-16 with BOM for non-ASCII application names.
    let bytes: Vec<_> = std::iter::once(0xfeff_u16)
        .chain(text.encode_utf16())
        .flat_map(u16::to_le_bytes)
        .collect();
    fs::write(path, bytes).map_err(|_| error_message)
}

fn probe_clients_report(report: &Path) -> Result<()> {
    let elevated = cli_clients::is_elevated();
    let mut text = String::from("[clients]\r\n");
    write!(text, "elevated={}\r\n", i32::from(elevated))
        .map_err(|_| "Cannot format client status.")?;
    for id in cli_clients::CLIENTS {
        let detected = !elevated && cli_clients::cli_detected(id);
        let status = if elevated {
            "CLI disabled while elevated"
        } else if detected {
            "CLI found"
        } else {
            "CLI not found on PATH"
        };
        let flag = i32::from(detected);
        write!(text, "{id}={status}\r\n{id}-found={flag}\r\n")
            .map_err(|_| "Cannot format client status.")?;
    }
    write_utf16_ini(report, &text, "Cannot write client status.")
}

fn report_cli_cleanup(path: &Path, state: &Path, allow_elevated: bool) -> Result<()> {
    let outcome = cli_clients::remove_managed(state, allow_elevated);
    let mut parts = Vec::new();
    let mut warning = false;

    if outcome.elevated_refused {
        warning = true;
        parts.push(format!(
            "Uninstall is running as Administrator; automatic CLI removal is disabled. Remove ControlFreak manually from a normal PowerShell terminal for: {}.",
            outcome.retained.join(", ")
        ));
    } else if !outcome.retained.is_empty() {
        warning = true;
        parts.push(format!(
            "Could not verify or remove MCP entry for: {}. Remove it manually with `claude mcp remove --scope user controlfreak` or `codex mcp remove controlfreak`.",
            outcome.retained.join(", ")
        ));
    } else if !outcome.removed.is_empty() {
        parts.push(format!(
            "Removed ControlFreak MCP entry for: {}.",
            outcome.removed.join(", ")
        ));
    }

    if !outcome.legacy_clients.is_empty() {
        warning = true;
        parts.push(format!(
            "Earlier Setup versions configured: {}. Remove those MCP entries manually and delete any old *.controlfreak-backup-*.bak files (which may contain credentials) once no longer needed.",
            outcome.legacy_clients.join(", ")
        ));
    }

    if parts.is_empty() {
        parts.push("No installer-registered MCP entries were present.".to_owned());
    }
    let status = if warning { "warning" } else { "ok" };
    let message = parts.join(" ");
    fs::write(
        path,
        format!("[result]\r\nstatus={status}\r\nmessage={message}\r\n"),
    )
    .map_err(|_| "Cannot write setup result.")
}

fn is_controlfreak_exe(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == "controlfreak.exe" || lower == "controlfreak-installer.exe" || lower == "controlfreak"
}

fn is_generic_wrapper(exe_name: &str) -> bool {
    matches!(
        exe_name.to_ascii_lowercase().as_str(),
        "cmd.exe"
            | "powershell.exe"
            | "pwsh.exe"
            | "bash.exe"
            | "sh.exe"
            | "zsh.exe"
            | "wsl.exe"
            | "wslhost.exe"
            | "conhost.exe"
            | "openconsole.exe"
            | "node.exe"
            | "bun.exe"
            | "deno.exe"
            | "python.exe"
            | "python3.exe"
            | "py.exe"
            | "uv.exe"
            | "uvx.exe"
            | "npx.exe"
            | "npm.exe"
            | "pnpm.exe"
            | "yarn.exe"
            | "cargo.exe"
    )
}

fn is_terminal_host(exe_name: &str) -> bool {
    matches!(
        exe_name.to_ascii_lowercase().as_str(),
        "windowsterminal.exe"
            | "wt.exe"
            | "alacritty.exe"
            | "wezterm-gui.exe"
            | "mintty.exe"
            | "hyper.exe"
            | "tabby.exe"
    )
}

fn format_ancestor_node(
    ancestor: &controlfreak_platform::ProcessAncestor,
    include_friendly: bool,
) -> String {
    if include_friendly
        && let Some(friendly) = ancestor.friendly_name.as_deref()
        && !friendly.is_empty()
        && !friendly.eq_ignore_ascii_case(&ancestor.exe_name)
    {
        let stem = ancestor
            .exe_name
            .strip_suffix(".exe")
            .or_else(|| ancestor.exe_name.strip_suffix(".EXE"))
            .unwrap_or(&ancestor.exe_name);
        if friendly.eq_ignore_ascii_case(stem) {
            return friendly.to_owned();
        }
        return format!("{friendly} ({})", ancestor.exe_name);
    }
    ancestor.exe_name.clone()
}

#[derive(Debug, Clone)]
struct TreeNode {
    pid: u32,
    label: String,
    children: Vec<Self>,
}

impl TreeNode {
    fn insert(&mut self, path: &[(u32, String)]) {
        let Some(((pid, label), rest)) = path.split_first() else {
            return;
        };
        if let Some(child) = self.children.iter_mut().find(|node| node.pid == *pid) {
            if label.contains("holds file") && !child.label.contains("holds file") {
                child.label.clone_from(label);
            }
            child.insert(rest);
        } else {
            let mut node = Self {
                pid: *pid,
                label: label.clone(),
                children: Vec::new(),
            };
            node.insert(rest);
            self.children.push(node);
        }
    }

    fn sort_recursive(&mut self) {
        self.children
            .sort_by(|a, b| a.pid.cmp(&b.pid).then_with(|| a.label.cmp(&b.label)));
        for child in &mut self.children {
            child.sort_recursive();
        }
    }

    fn render_children(&self, prefix: &str, out: &mut Vec<String>) {
        let last_index = self.children.len().saturating_sub(1);
        for (index, child) in self.children.iter().enumerate() {
            let is_last = index == last_index;
            let branch = if is_last {
                "\u{2514}\u{2500} "
            } else {
                "\u{251c}\u{2500} "
            };
            out.push(format!("{prefix}{branch}{}", child.label));
            let next_prefix = if is_last {
                format!("{prefix}   ")
            } else {
                format!("{prefix}\u{2502}  ")
            };
            child.render_children(&next_prefix, out);
        }
    }
}

fn blocker_tree_path(blocker: &controlfreak_platform::InstallerBlocker) -> Vec<(u32, String)> {
    // Separate leading ControlFreak helper->server ancestors from the host/client chain above them.
    let cf_ancestor_count = blocker
        .ancestors
        .iter()
        .take_while(|ancestor| is_controlfreak_exe(&ancestor.exe_name))
        .count();
    let cf_ancestors = &blocker.ancestors[..cf_ancestor_count];
    let host_ancestors = &blocker.ancestors[cf_ancestor_count..];

    // Reverse host ancestors to outermost->innermost first so dedup retains the outermost PID
    // (merging multi-process Electron trees like Code.exe/T3 Code under one root node).
    let mut host_chain: Vec<&controlfreak_platform::ProcessAncestor> =
        host_ancestors.iter().rev().collect();
    host_chain.dedup_by(|a, b| a.exe_name.eq_ignore_ascii_case(&b.exe_name));

    let meaningful: Vec<&controlfreak_platform::ProcessAncestor> = host_chain
        .iter()
        .copied()
        .filter(|ancestor| !is_generic_wrapper(&ancestor.exe_name))
        .collect();

    let selected_hosts: Vec<&controlfreak_platform::ProcessAncestor> = match meaningful.as_slice() {
        [] => {
            if host_chain.len() <= 2 {
                host_chain
            } else {
                vec![host_chain[0], host_chain[host_chain.len() - 1]]
            }
        }
        [only] => {
            if is_terminal_host(&only.exe_name)
                && let Some(last) = host_chain.last()
                && !last.exe_name.eq_ignore_ascii_case(&only.exe_name)
            {
                vec![*only, *last]
            } else {
                vec![*only]
            }
        }
        [first, .., last] => vec![*first, *last],
    };

    let mut path = Vec::new();
    for (index, ancestor) in selected_hosts.iter().enumerate() {
        let name = format_ancestor_node(ancestor, index == 0);
        path.push((ancestor.pid, format!("{name} (PID: {})", ancestor.pid)));
    }

    let has_host = !selected_hosts.is_empty();
    for (index, ancestor) in cf_ancestors.iter().rev().enumerate() {
        let label = if !has_host && index == 0 {
            format!(
                "{} (PID: {}, no parent client found)",
                ancestor.exe_name, ancestor.pid
            )
        } else {
            format!("{} (PID: {})", ancestor.exe_name, ancestor.pid)
        };
        path.push((ancestor.pid, label));
    }

    let role_tag = if blocker.identity_verified {
        "holds file"
    } else {
        "identity unverified"
    };
    let self_label = if !has_host && cf_ancestors.is_empty() && is_controlfreak_exe(&blocker.name) {
        format!(
            "{} (PID: {}, {role_tag}, no parent client found)",
            blocker.name, blocker.pid
        )
    } else {
        format!("{} (PID: {}, {role_tag})", blocker.name, blocker.pid)
    };
    path.push((blocker.pid, self_label));
    path
}

fn format_blockers(
    blockers: Vec<controlfreak_platform::InstallerBlocker>,
    current_pid: u32,
) -> Vec<String> {
    let mut roots: Vec<TreeNode> = Vec::new();
    for item in blockers.into_iter().filter(|item| item.pid != current_pid) {
        let path = blocker_tree_path(&item);
        let Some(((root_pid, root_label), rest)) = path.split_first() else {
            continue;
        };
        if let Some(root) = roots.iter_mut().find(|node| node.pid == *root_pid) {
            // Upgrade label if a later blocker confirms this root itself directly holds a file.
            if root_label.contains("holds file") && !root.label.contains("holds file") {
                root.label.clone_from(root_label);
            }
            root.insert(rest);
        } else {
            let mut node = TreeNode {
                pid: *root_pid,
                label: root_label.clone(),
                children: Vec::new(),
            };
            node.insert(rest);
            roots.push(node);
        }
    }

    // Show identified client/IDE trees before orphaned processes without a parent client.
    roots.sort_by(|a, b| {
        let a_orphan = a.label.contains("no parent client found");
        let b_orphan = b.label.contains("no parent client found");
        a_orphan
            .cmp(&b_orphan)
            .then_with(|| a.pid.cmp(&b.pid))
            .then_with(|| a.label.cmp(&b.label))
    });
    for root in &mut roots {
        root.sort_recursive();
    }

    let mut lines = Vec::new();
    for root in &roots {
        lines.push(root.label.clone());
        root.render_children("", &mut lines);
    }
    lines
}

fn blockers_report(directory: &Path, report: &Path, uninstall: bool) -> Result<()> {
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
            let names = format_blockers(blockers, std::process::id());
            if names.is_empty() {
                (
                    if uninstall {
                        "Uninstall cannot remove the ControlFreak files."
                    } else {
                        "Setup cannot replace the ControlFreak files."
                    },
                    "Close clients using ControlFreak and check that you can write to the installation folder. Windows could not identify a blocking process.",
                    names,
                )
            } else {
                (
                    if uninstall {
                        "ControlFreak is in use. Uninstall cannot remove it while these processes are running."
                    } else {
                        "ControlFreak is in use. Setup cannot update it while these processes are running."
                    },
                    "Close the parent app at the top of each tree, or end only the \"holds file\" PIDs in Task Manager > Details.",
                    names,
                )
            }
        }
        Err(_) => (
            if uninstall {
                "Uninstall cannot remove the ControlFreak files."
            } else {
                "Setup cannot replace the ControlFreak files."
            },
            "Close clients using ControlFreak and check that you can write to the installation folder. Windows could not identify the blocking processes.",
            Vec::new(),
        ),
    };
    // Keep each process group on its own key: INI values cannot contain line breaks.
    // This private temporary report is read by Setup, never printed or uploaded.
    let mut text = format!(
        "[result]\r\nmessage={message}\r\naction={action}\r\ncount={}\r\n",
        names.len()
    );
    for (index, name) in names.iter().enumerate() {
        // Quote INI values so Windows' GetPrivateProfileStringW preserves leading tree indentation.
        let safe = name.replace('"', "'");
        write!(text, "process{index}=\"{safe}\"\r\n")
            .map_err(|_| "Cannot format blocker diagnostics.")?;
    }
    write_utf16_ini(report, &text, "Cannot write blocker diagnostics.")
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

#[cfg(test)]
mod blocker_tests {
    use super::*;

    #[test]
    fn blockers_group_controlfreak_helpers_under_their_parent_client() {
        use controlfreak_platform::{InstallerBlocker, ProcessAncestor};
        let code = ProcessAncestor {
            pid: 500,
            exe_name: "Code.exe".into(),
            friendly_name: Some("Visual Studio Code".into()),
        };
        let node = ProcessAncestor {
            pid: 600,
            exe_name: "node.exe".into(),
            friendly_name: Some("Node.js".into()),
        };
        let server = InstallerBlocker {
            pid: 1000,
            name: "controlfreak.exe".into(),
            identity_verified: true,
            ancestors: vec![node.clone(), code.clone(), code.clone()],
        };
        let ocr_helper = InstallerBlocker {
            pid: 900, // deliberately lower PID than server to test root-first ordering
            name: "controlfreak.exe".into(),
            identity_verified: true,
            ancestors: vec![
                ProcessAncestor {
                    pid: 1000,
                    exe_name: "controlfreak.exe".into(),
                    friendly_name: None,
                },
                node,
                code,
            ],
        };
        let orphan = InstallerBlocker {
            pid: 2000,
            name: "controlfreak.exe".into(),
            identity_verified: true,
            ancestors: Vec::new(),
        };
        let unverified = InstallerBlocker {
            pid: 2500,
            name: "other.exe".into(),
            identity_verified: false,
            ancestors: Vec::new(),
        };
        let formatted = format_blockers(vec![ocr_helper, orphan, unverified, server], 9999);
        assert_eq!(
            formatted,
            vec![
                "Visual Studio Code (Code.exe) (PID: 500)",
                "\u{2514}\u{2500} controlfreak.exe (PID: 1000, holds file)",
                "   \u{2514}\u{2500} controlfreak.exe (PID: 900, holds file)",
                "other.exe (PID: 2500, identity unverified)",
                "controlfreak.exe (PID: 2000, holds file, no parent client found)",
            ]
        );
    }

    #[test]
    fn blockers_surface_ide_and_agent_while_skipping_shell_wrappers() {
        use controlfreak_platform::{InstallerBlocker, ProcessAncestor};
        let blocker = InstallerBlocker {
            pid: 4000,
            name: "controlfreak.exe".into(),
            identity_verified: true,
            ancestors: vec![
                ProcessAncestor {
                    pid: 3000,
                    exe_name: "cmd.exe".into(),
                    friendly_name: Some("Windows Command Processor".into()),
                },
                ProcessAncestor {
                    pid: 2500,
                    exe_name: "pwsh.exe".into(),
                    friendly_name: Some("PowerShell".into()),
                },
                ProcessAncestor {
                    pid: 2000,
                    exe_name: "agy.exe".into(),
                    friendly_name: None,
                },
                ProcessAncestor {
                    pid: 1000,
                    exe_name: "T3 Code.exe".into(),
                    friendly_name: Some("T3 Code".into()),
                },
            ],
        };
        assert_eq!(
            format_blockers(vec![blocker], 9999),
            vec![
                "T3 Code (PID: 1000)",
                "\u{2514}\u{2500} agy.exe (PID: 2000)",
                "   \u{2514}\u{2500} controlfreak.exe (PID: 4000, holds file)",
            ]
        );
    }
}
