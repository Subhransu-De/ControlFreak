//! Invokes official Claude Code and Codex CLIs to register or remove `ControlFreak`.
//! Never reads, parses, or writes third-party configuration files directly.
use std::{
    env,
    ffi::OsString,
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

pub const CLIENTS: [&str; 2] = ["claude-code", "codex"];
pub const LEGACY_RECEIPT_CLIENTS: [&str; 5] =
    ["claude-code", "codex", "claude-desktop", "pi", "opencode"];
const CLI_TIMEOUT: Duration = Duration::from_secs(15);
const POST_EXIT_PIPE_GRACE: Duration = Duration::from_millis(500);
const MAX_CLI_OUTPUT_BYTES: u64 = 64 * 1024;

pub type Result<T> = std::result::Result<T, &'static str>;

enum ResolvedCli {
    Exe(PathBuf),
    Batch(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ManagedMarker {
    exe: String,
    profile_override: String,
    userprofile: String,
}

pub fn is_elevated() -> bool {
    // Fail closed if process elevation cannot be verified.
    match controlfreak_platform::server_security_context(true) {
        Ok(ctx) => ctx.elevated,
        Err(_) => true,
    }
}

fn ensure_not_elevated(allow_elevated: bool) -> Result<()> {
    if !allow_elevated && is_elevated() {
        return Err(
            "Automatic CLI registration is disabled when Setup runs as Administrator; copy and run the command from a normal PowerShell terminal.",
        );
    }
    Ok(())
}

fn cli_binary_name(id: &str) -> Result<&'static str> {
    match id {
        "claude-code" => Ok("claude"),
        "codex" => Ok("codex"),
        _ => Err("Unknown MCP client."),
    }
}

fn resolve_on_path(name: &str) -> Option<ResolvedCli> {
    let paths = env::var_os("PATH")?;
    for dir in env::split_paths(&paths) {
        // Ignore empty or relative PATH entries so cwd cannot hijack CLI resolution.
        if dir.as_os_str().is_empty() || !dir.is_absolute() {
            continue;
        }
        let exe = dir.join(format!("{name}.exe"));
        if exe.is_file() {
            return Some(ResolvedCli::Exe(exe));
        }
        for ext in ["cmd", "bat"] {
            let batch = dir.join(format!("{name}.{ext}"));
            if batch.is_file() {
                return Some(ResolvedCli::Batch(batch));
            }
        }
    }
    None
}

pub fn cli_detected(id: &str) -> bool {
    cli_binary_name(id).is_ok_and(|name| resolve_on_path(name).is_some())
}

fn validate_executable(path: &Path) -> Result<String> {
    if !path.is_absolute() {
        return Err("The installed executable path must be absolute.");
    }
    let text = path
        .to_str()
        .ok_or("The executable path must be Unicode.")?;
    if text.chars().any(|c| matches!(c, '\r' | '\n' | '\0')) {
        return Err("The executable path contains invalid control characters.");
    }
    Ok(text.to_owned())
}

fn has_batch_metacharacters(value: &str) -> bool {
    value.chars().any(|c| {
        matches!(
            c,
            '&' | '|' | '<' | '>' | '^' | '%' | '!' | '"' | '\r' | '\n'
        )
    })
}

struct CliOutput {
    success: bool,
    stdout: String,
    combined: String,
}

fn drain_bounded_channel<R: Read + Send + 'static>(mut reader: R) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = reader
            .by_ref()
            .take(MAX_CLI_OUTPUT_BYTES)
            .read_to_end(&mut buf);
        // Drain remaining bytes without retaining them so the child cannot block on a full pipe.
        let _ = std::io::copy(&mut reader, &mut std::io::sink());
        let _ = tx.send(buf);
    });
    rx
}

fn format_batch_command_line(cmd_exe: &Path, batch: &Path, args: &[&str]) -> Result<OsString> {
    let cmd_str = cmd_exe
        .to_str()
        .ok_or("Windows system cmd.exe path is not Unicode.")?;
    let batch_str = batch
        .to_str()
        .ok_or("Client CLI path must be Unicode; run the command manually.")?;
    if batch_str
        .chars()
        .any(|c| matches!(c, '"' | '%' | '!' | '\r' | '\n' | '\0'))
    {
        return Err("Client CLI path contains unsupported characters; run the command manually.");
    }
    if args.iter().any(|arg| has_batch_metacharacters(arg)) {
        return Err("Install path contains shell metacharacters; run the command manually.");
    }
    // `cmd /s /c` unconditionally strips one outer pair of quotes, preserving inner quotes on
    // both the batch shim path and executable paths containing spaces.
    let mut inner = format!("\"{batch_str}\"");
    for arg in args {
        inner.push(' ');
        if arg.contains([' ', '\t']) {
            inner.push('"');
            inner.push_str(arg);
            inner.push('"');
        } else {
            inner.push_str(arg);
        }
    }
    Ok(OsString::from(format!(
        "\"{cmd_str}\" /d /s /c \"{inner}\""
    )))
}

fn collect_pipe_within(rx: Option<mpsc::Receiver<Vec<u8>>>, deadline: Instant) -> Option<Vec<u8>> {
    let rx = rx?;
    let remaining = deadline.saturating_duration_since(Instant::now());
    rx.recv_timeout(remaining).ok()
}

fn run_cli(resolved: &ResolvedCli, args: &[&str]) -> Result<CliOutput> {
    run_cli_with_timeout(resolved, args, CLI_TIMEOUT)
}

fn run_cli_with_timeout(
    resolved: &ResolvedCli,
    args: &[&str],
    timeout: Duration,
) -> Result<CliOutput> {
    // Spawn through controlfreak-platform's required job-contained launcher so timeouts and
    // pipe-holding descendants terminate the entire process tree atomically (no uncontained fallback).
    let mut child = match resolved {
        ResolvedCli::Exe(exe) => {
            let os_args: Vec<OsString> = args.iter().map(OsString::from).collect();
            controlfreak_platform::spawn_contained_process(exe, &os_args)
                .map_err(|_| "Could not start the client CLI.")?
        }
        ResolvedCli::Batch(batch) => {
            let cmd_exe = controlfreak_platform::system_cmd_exe()
                .map_err(|_| "Windows system cmd.exe was not found.")?;
            let command_line = format_batch_command_line(&cmd_exe, batch, args)?;
            controlfreak_platform::spawn_contained_command_line(&cmd_exe, &command_line)
                .map_err(|_| "Could not start the client CLI.")?
        }
    };
    // Close stdin immediately so an interactive prompt cannot hang waiting for input.
    drop(child.take_stdin());

    let stdout_rx = child.take_stdout().map(drain_bounded_channel);
    let stderr_rx = child.take_stderr().map(drain_bounded_channel);

    let deadline = Instant::now() + timeout;
    let remaining = deadline.saturating_duration_since(Instant::now());
    let status = match child.wait_exit_within(remaining) {
        Ok(Some(status)) => status,
        Ok(None) => {
            let _ = child.terminate();
            return Err("Client CLI timed out; run the command manually.");
        }
        Err(_) => {
            let _ = child.terminate();
            return Err("Could not wait for the client CLI.");
        }
    };

    // Wait briefly for pipe EOF within the same overall deadline. If a detached descendant kept
    // stdout/stderr open after the launcher exited, terminate the job tree to release the pipes.
    let pipe_deadline = deadline.min(Instant::now() + POST_EXIT_PIPE_GRACE);
    let mut stdout = collect_pipe_within(stdout_rx, pipe_deadline);
    let mut stderr = collect_pipe_within(stderr_rx, pipe_deadline);
    if stdout.is_none() || stderr.is_none() {
        let _ = child.terminate();
        if stdout.is_none() {
            stdout = Some(Vec::new());
        }
        if stderr.is_none() {
            stderr = Some(Vec::new());
        }
    }
    let stdout_text = String::from_utf8_lossy(&stdout.unwrap_or_default()).into_owned();
    let stderr_text = String::from_utf8_lossy(&stderr.unwrap_or_default()).into_owned();
    let mut combined = stdout_text.clone();
    if !stderr_text.is_empty() {
        combined.push('\n');
        combined.push_str(&stderr_text);
    }
    Ok(CliOutput {
        success: status.success(),
        stdout: stdout_text,
        combined,
    })
}

fn current_profile(id: &str) -> (String, String) {
    let override_var = match id {
        "claude-code" => "CLAUDE_CONFIG_DIR",
        "codex" => "CODEX_HOME",
        _ => "",
    };
    let profile_override = if override_var.is_empty() {
        String::new()
    } else {
        env::var(override_var).unwrap_or_default()
    };
    let userprofile = env::var("USERPROFILE").unwrap_or_default();
    (profile_override, userprofile)
}

fn marker_path(state: &Path, id: &str) -> PathBuf {
    state.join(format!("{id}.managed"))
}

fn parse_marker_object(value: &serde_json::Value) -> Option<ManagedMarker> {
    let obj = value.as_object()?;
    if obj.get("version").and_then(serde_json::Value::as_u64) != Some(1) {
        return None;
    }
    let exe = obj.get("exe").and_then(serde_json::Value::as_str)?;
    if exe.is_empty() {
        return None;
    }
    Some(ManagedMarker {
        exe: exe.to_owned(),
        profile_override: obj
            .get("profile_override")
            .and_then(serde_json::Value::as_str)?
            .to_owned(),
        userprofile: obj
            .get("userprofile")
            .and_then(serde_json::Value::as_str)?
            .to_owned(),
    })
}

fn read_markers(path: &Path) -> Option<Vec<ManagedMarker>> {
    let text = fs::read_to_string(path).ok()?;
    let trimmed = text.trim();
    if trimmed.starts_with('[') {
        let items = serde_json::from_str::<Vec<serde_json::Value>>(trimmed).ok()?;
        let mut parsed = Vec::with_capacity(items.len());
        for item in &items {
            parsed.push(parse_marker_object(item)?);
        }
        return (!parsed.is_empty()).then_some(parsed);
    }
    // Support legacy single-record key=value marker format.
    let mut version = None;
    let mut exe = None;
    let mut profile_override = None;
    let mut userprofile = None;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("version=") {
            version = Some(value.trim());
        } else if let Some(value) = line.strip_prefix("exe=") {
            exe = Some(value.to_owned());
        } else if let Some(value) = line.strip_prefix("profile_override=") {
            profile_override = Some(value.to_owned());
        } else if let Some(value) = line.strip_prefix("userprofile=") {
            userprofile = Some(value.to_owned());
        }
    }
    if version != Some("1") {
        return None;
    }
    let exe = exe?;
    if exe.is_empty() {
        return None;
    }
    Some(vec![ManagedMarker {
        exe,
        profile_override: profile_override?,
        userprofile: userprofile?,
    }])
}

fn write_markers_list(path: &Path, markers: &[ManagedMarker]) -> std::io::Result<()> {
    let json: Vec<serde_json::Value> = markers
        .iter()
        .map(|marker| {
            serde_json::json!({
                "version": 1,
                "exe": marker.exe,
                "profile_override": marker.profile_override,
                "userprofile": marker.userprofile,
            })
        })
        .collect();
    let encoded = serde_json::to_string(&json).map_err(std::io::Error::other)?;
    fs::write(path, encoded)
}

fn write_marker(state: &Path, id: &str, exe: &str) -> std::io::Result<()> {
    let (profile_override, userprofile) = current_profile(id);
    fs::create_dir_all(state)?;
    let path = marker_path(state, id);
    let mut markers = if path.is_file() {
        read_markers(&path).unwrap_or_default()
    } else {
        Vec::new()
    };
    // Preserve earlier registered profiles while updating any record for the active profile.
    markers.retain(|existing| {
        existing.profile_override != profile_override || existing.userprofile != userprofile
    });
    markers.push(ManagedMarker {
        exe: exe.to_owned(),
        profile_override,
        userprofile,
    });
    write_markers_list(&path, &markers)
}

fn enter_non_project_dir() -> Result<()> {
    // Use Windows' System32 directory so project-scoped `.codex/config.toml` files in any
    // workspace (including one containing TMP) cannot shadow global Codex configuration.
    let cmd_exe = controlfreak_platform::system_cmd_exe()
        .map_err(|_| "Windows system directory was not found.")?;
    let system_dir = cmd_exe
        .parent()
        .ok_or("Windows system directory was not found.")?;
    env::set_current_dir(system_dir)
        .map_err(|_| "Could not isolate Codex lookup outside project directories.")
}

fn is_codex_not_found(output: &str) -> bool {
    output.lines().any(|line| {
        let trimmed = line.trim();
        trimmed.eq_ignore_ascii_case("Error: No MCP server named 'controlfreak' found.")
            || trimmed.eq_ignore_ascii_case("No MCP server named 'controlfreak' found.")
    })
}

fn codex_entry_matches_managed(stdout: &str, expected_exe: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(stdout.trim()) else {
        return false;
    };
    let Some(root) = value.as_object() else {
        return false;
    };
    if root.get("enabled").and_then(serde_json::Value::as_bool) != Some(true) {
        return false;
    }
    for key in [
        "disabled_reason",
        "enabled_tools",
        "disabled_tools",
        "startup_timeout_sec",
        "tool_timeout_sec",
    ] {
        if !root.get(key).is_none_or(serde_json::Value::is_null) {
            return false;
        }
    }
    let Some(transport) = root.get("transport").and_then(serde_json::Value::as_object) else {
        return false;
    };
    if transport.get("type").and_then(serde_json::Value::as_str) != Some("stdio") {
        return false;
    }
    let Some(command) = transport.get("command").and_then(serde_json::Value::as_str) else {
        return false;
    };
    let args_empty = transport
        .get("args")
        .is_none_or(|v| v.is_null() || v.as_array().is_some_and(Vec::is_empty));
    let env_empty = transport
        .get("env")
        .is_none_or(|v| v.is_null() || v.as_object().is_some_and(serde_json::Map::is_empty));
    let env_vars_empty = transport
        .get("env_vars")
        .is_none_or(|v| v.is_null() || v.as_array().is_some_and(Vec::is_empty));
    let cwd_empty = transport.get("cwd").is_none_or(serde_json::Value::is_null);
    if !args_empty || !env_empty || !env_vars_empty || !cwd_empty {
        return false;
    }
    let normalized_cmd = command.trim_matches('"');
    let normalized_expected = expected_exe.trim_matches('"');
    normalized_cmd.eq_ignore_ascii_case(normalized_expected)
}

pub fn legacy_receipt_clients(state: &Path) -> Vec<&'static str> {
    if !state.is_absolute() {
        return Vec::new();
    }
    LEGACY_RECEIPT_CLIENTS
        .iter()
        .copied()
        .filter(|id| state.join(format!("{id}.json")).is_file())
        .collect()
}

pub fn register(id: &str, executable: &Path, state: &Path, allow_elevated: bool) -> Result<String> {
    ensure_not_elevated(allow_elevated)?;
    let exe = validate_executable(executable)?;
    if !state.is_absolute() {
        return Err("Installer state directory must be absolute.");
    }
    let cli_name = cli_binary_name(id)?;
    let Some(resolved) = resolve_on_path(cli_name) else {
        return Err(match id {
            "claude-code" => "claude CLI was not found on PATH.",
            "codex" => "codex CLI was not found on PATH.",
            _ => "Client CLI was not found on PATH.",
        });
    };
    if matches!(resolved, ResolvedCli::Batch(_)) && has_batch_metacharacters(&exe) {
        return Err("Install path contains shell metacharacters; run the command manually.");
    }

    match id {
        "claude-code" => {
            let output = run_cli(
                &resolved,
                &["mcp", "add", "--scope", "user", "controlfreak", "--", &exe],
            )?;
            if output.success {
                return if write_marker(state, id, &exe).is_ok() {
                    Ok("Registered via claude mcp add.".to_owned())
                } else {
                    Ok("Registered via claude mcp add (could not save uninstall marker; remove manually on uninstall).".to_owned())
                };
            }
            if output
                .combined
                .to_ascii_lowercase()
                .contains("already exists")
            {
                return Ok("Already configured (existing entry kept).".to_owned());
            }
            Err("claude mcp add failed; copy the command and run it in PowerShell.")
        }
        "codex" => {
            enter_non_project_dir()?;
            // Check existing registration first and only proceed to `codex mcp add` on an
            // exact absence line, so customized entries (e.g. --profile observe) are never
            // silently overwritten on a lookup error or corrupt config.
            let check = run_cli(&resolved, &["mcp", "get", "--json", "controlfreak"])?;
            if check.success {
                return Ok("Already configured (existing entry kept).".to_owned());
            }
            if !is_codex_not_found(&check.combined) {
                return Err(
                    "Could not verify existing Codex configuration; copy the command and run it in PowerShell.",
                );
            }
            let output = run_cli(&resolved, &["mcp", "add", "controlfreak", "--", &exe])?;
            if output.success {
                return if write_marker(state, id, &exe).is_ok() {
                    Ok("Registered via codex mcp add.".to_owned())
                } else {
                    Ok("Registered via codex mcp add (could not save uninstall marker; remove manually on uninstall).".to_owned())
                };
            }
            Err("codex mcp add failed; copy the command and run it in PowerShell.")
        }
        _ => Err("Unknown MCP client."),
    }
}

pub struct RemoveOutcome {
    pub removed: Vec<&'static str>,
    pub retained: Vec<&'static str>,
    pub elevated_refused: bool,
    pub legacy_clients: Vec<&'static str>,
}

pub fn remove_managed(state: &Path, allow_elevated: bool) -> RemoveOutcome {
    let mut outcome = RemoveOutcome {
        removed: Vec::new(),
        retained: Vec::new(),
        elevated_refused: false,
        legacy_clients: legacy_receipt_clients(state),
    };
    if !state.is_absolute() {
        return outcome;
    }
    if !allow_elevated && is_elevated() {
        for id in CLIENTS {
            if marker_path(state, id).is_file() {
                outcome.retained.push(id);
                outcome.elevated_refused = true;
            }
        }
        return outcome;
    }

    for id in CLIENTS {
        let marker_file = marker_path(state, id);
        if !marker_file.is_file() {
            continue;
        }
        let Some(markers) = read_markers(&marker_file) else {
            outcome.retained.push(id);
            continue;
        };
        let (profile_override, userprofile) = current_profile(id);
        let mut active_marker = None;
        let mut other_profiles = Vec::new();
        for marker in markers {
            if marker.profile_override == profile_override && marker.userprofile == userprofile {
                active_marker = Some(marker);
            } else {
                other_profiles.push(marker);
            }
        }
        let Some(marker) = active_marker else {
            outcome.retained.push(id);
            continue;
        };
        let Ok(cli_name) = cli_binary_name(id) else {
            outcome.retained.push(id);
            continue;
        };
        let Some(resolved) = resolve_on_path(cli_name) else {
            outcome.retained.push(id);
            continue;
        };

        let mut active_removed = false;
        if id == "codex" {
            if enter_non_project_dir().is_err() {
                outcome.retained.push(id);
                continue;
            }
            // Verify Codex still holds the exact unmodified entry Setup registered before removing.
            match run_cli(&resolved, &["mcp", "get", "--json", "controlfreak"]) {
                Ok(check) if check.success => {
                    if !codex_entry_matches_managed(&check.stdout, &marker.exe) {
                        outcome.retained.push(id);
                        continue;
                    }
                }
                Ok(check) if is_codex_not_found(&check.combined) => {
                    active_removed = true;
                }
                _ => {
                    outcome.retained.push(id);
                    continue;
                }
            }
        }

        if !active_removed {
            let args: &[&str] = match id {
                "claude-code" => &["mcp", "remove", "--scope", "user", "controlfreak"],
                "codex" => &["mcp", "remove", "controlfreak"],
                _ => continue,
            };
            if let Ok(output) = run_cli(&resolved, args)
                && output.success
            {
                active_removed = true;
            }
        }

        if active_removed {
            outcome.removed.push(id);
            if other_profiles.is_empty() {
                let _ = fs::remove_file(&marker_file);
            } else {
                // Keep records for other profiles and warn so earlier registrations are not lost silently.
                let _ = write_markers_list(&marker_file, &other_profiles);
                outcome.retained.push(id);
            }
        } else {
            outcome.retained.push(id);
        }
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_metacharacters_are_detected() {
        assert!(has_batch_metacharacters(r"C:\Apps&More\controlfreak.exe"));
        assert!(has_batch_metacharacters(r"C:\Apps%TEMP%\controlfreak.exe"));
        assert!(!has_batch_metacharacters(
            r"C:\Users\Alex\AppData\Local\Programs\ControlFreak\controlfreak.exe"
        ));
    }

    #[test]
    fn codex_entry_matching_rejects_customized_args_or_different_path() {
        let exact = r#"{"name":"controlfreak","enabled":true,"transport":{"type":"stdio","command":"C:\\Apps\\ControlFreak\\controlfreak.exe","args":[],"env":null,"env_vars":[],"cwd":null}}"#;
        assert!(codex_entry_matches_managed(
            exact,
            r"C:\Apps\ControlFreak\controlfreak.exe"
        ));
        assert!(codex_entry_matches_managed(
            exact,
            r"c:\apps\controlfreak\controlfreak.exe"
        ));
        assert!(!codex_entry_matches_managed(
            exact,
            r"C:\Other\controlfreak.exe"
        ));

        let customized = r#"{"name":"controlfreak","enabled":true,"transport":{"type":"stdio","command":"C:\\Apps\\ControlFreak\\controlfreak.exe","args":["--profile","observe"],"env":null,"env_vars":[],"cwd":null}}"#;
        assert!(!codex_entry_matches_managed(
            customized,
            r"C:\Apps\ControlFreak\controlfreak.exe"
        ));

        let disabled = r#"{"name":"controlfreak","enabled":false,"transport":{"type":"stdio","command":"C:\\Apps\\ControlFreak\\controlfreak.exe","args":[],"env":null,"env_vars":[],"cwd":null}}"#;
        assert!(!codex_entry_matches_managed(
            disabled,
            r"C:\Apps\ControlFreak\controlfreak.exe"
        ));

        let filtered = r#"{"name":"controlfreak","enabled":true,"enabled_tools":["list_windows"],"transport":{"type":"stdio","command":"C:\\Apps\\ControlFreak\\controlfreak.exe","args":[],"env":null,"env_vars":[],"cwd":null}}"#;
        assert!(!codex_entry_matches_managed(
            filtered,
            r"C:\Apps\ControlFreak\controlfreak.exe"
        ));
    }

    #[test]
    fn codex_not_found_requires_exact_absence_line() {
        assert!(is_codex_not_found(
            "WARNING: unrelated\nError: No MCP server named 'controlfreak' found.\n"
        ));
        assert!(!is_codex_not_found(
            "Error: failed to read controlfreak because no mcp server named X was valid"
        ));
    }

    #[test]
    fn run_cli_times_out_and_terminates_descendant_tree() {
        let dir = tempfile::tempdir().unwrap();
        let batch = dir.path().join("hang.cmd");
        let marker = dir.path().join("descendant-ran-late.txt");
        // Spawn a PowerShell descendant that writes `marker` after 1.0s if left alive.
        fs::write(
            &batch,
            format!(
                "@echo off\r\npowershell.exe -NoProfile -NonInteractive -Command \"Start-Sleep -Milliseconds 1000; Set-Content -Path '{}' -Value 'late'\"\r\n",
                marker.display()
            ),
        )
        .unwrap();
        let start = Instant::now();
        let result =
            run_cli_with_timeout(&ResolvedCli::Batch(batch), &[], Duration::from_millis(350));
        assert!(result.is_err());
        assert!(start.elapsed() < Duration::from_secs(2));
        // Wait well past the descendant's 1.0s scheduled write to confirm the Job Object killed it.
        thread::sleep(Duration::from_millis(1200));
        assert!(!marker.exists());
    }

    #[test]
    fn run_cli_does_not_hang_when_launcher_exits_with_inherited_pipe() {
        let dir = tempfile::tempdir().unwrap();
        let batch = dir.path().join("detach.cmd");
        // `start /b` launches a child that inherits stdout/stderr while the batch exits immediately.
        fs::write(
            &batch,
            "@echo off\r\necho hello\r\nstart /b ping -n 10 127.0.0.1 >nul\r\nexit /b 0\r\n",
        )
        .unwrap();
        let start = Instant::now();
        let result = run_cli_with_timeout(&ResolvedCli::Batch(batch), &[], Duration::from_secs(5));
        assert!(result.is_ok());
        assert!(start.elapsed() < Duration::from_secs(3));
    }
}
