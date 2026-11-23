use crate::{Result, package, process, verify};
use controlfreak_platform::packaging_tests::{self as native, Registration};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::process::CommandExt;
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

struct Fixture {
    output: PathBuf,
    profile: PathBuf,
    bin: PathBuf,
    install: PathBuf,
    registration: Registration,
}

impl Fixture {
    fn command(&self, executable: &Path) -> Command {
        let mut command = Command::new(executable);
        for name in ["CODEX_HOME", "CLAUDE_CONFIG_DIR"] {
            command.env_remove(name);
        }
        let sys_root = env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
        let sys_root = PathBuf::from(sys_root);
        let path = env::join_paths([self.bin.clone(), sys_root.join("System32"), sys_root])
            .expect("join synthetic PATH");
        command
            .env("PATH", path)
            .env("USERPROFILE", &self.profile)
            .env("LOCALAPPDATA", self.profile.join("local"))
            .env("APPDATA", self.profile.join("roaming"));
        command
    }

    fn setup_with_flags(
        &self,
        executable: &Path,
        configure: bool,
        allow_elevated: bool,
    ) -> Result<i32> {
        let mut command = self.command(executable);
        command.args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"]);
        if allow_elevated {
            command.arg("/ALLOWELEVATEDCLI=1");
        }
        if configure {
            command
                .arg(format!("/DIR={}", self.install.display()))
                .arg("/CLIENTS=claude-code,codex");
        } else {
            command.arg("/REMOVECONFIG=1").arg(format!(
                "/LOG={}",
                self.output.join("uninstall.log").display()
            ));
        }
        let result = process::run(&mut command, "", Duration::from_mins(1))?;
        result
            .status
            .code()
            .ok_or_else(|| "Installer terminated without an exit code".into())
    }

    fn setup(&self, executable: &Path, configure: bool) -> Result<i32> {
        let elevated =
            controlfreak_platform::server_security_context(true).map_or(true, |ctx| ctx.elevated);
        self.setup_with_flags(executable, configure, elevated)
    }

    fn write_synthetic_clis(&self, claude_present: bool, codex_present: bool) -> Result<()> {
        if self.bin.exists() {
            fs::remove_dir_all(&self.bin)?;
        }
        fs::create_dir_all(&self.bin)?;
        if claude_present {
            let state = self.profile.join("claude.registered");
            let script = format!(
                "@echo off\r\n\
chcp 65001 >nul\r\n\
if \"%1 %2\"==\"mcp add\" goto do_add\r\n\
if \"%1 %2\"==\"mcp remove\" goto do_remove\r\n\
exit /b 1\r\n\
:do_add\r\n\
if exist \"{s}\" goto already\r\n\
echo %*>\"{s}\"\r\n\
exit /b 0\r\n\
:already\r\n\
echo MCP server controlfreak already exists in user config 1>&2\r\n\
exit /b 1\r\n\
:do_remove\r\n\
if not exist \"{s}\" exit /b 1\r\n\
del /f /q \"{s}\"\r\n\
exit /b 0\r\n",
                s = state.display()
            );
            fs::write(self.bin.join("claude.cmd"), script)?;
        }
        if codex_present {
            let state = self.profile.join("codex.registered");
            let script = format!(
                "@echo off\r\n\
chcp 65001 >nul\r\n\
if \"%1 %2\"==\"mcp get\" goto do_get\r\n\
if \"%1 %2\"==\"mcp add\" goto do_add\r\n\
if \"%1 %2\"==\"mcp remove\" goto do_remove\r\n\
exit /b 1\r\n\
:do_get\r\n\
if exist \"{s}\" (\r\n\
  type \"{s}\"\r\n\
  exit /b 0\r\n\
)\r\n\
echo Error: No MCP server named 'controlfreak' found. 1>&2\r\n\
exit /b 1\r\n\
:do_add\r\n\
set \"EXE=%~5\"\r\n\
set \"EXE=%EXE:\\=\\\\%\"\r\n\
echo {{\"name\":\"controlfreak\",\"enabled\":true,\"transport\":{{\"type\":\"stdio\",\"command\":\"%EXE%\",\"args\":[],\"env\":null,\"env_vars\":[],\"cwd\":null}}}}>\"{s}\"\r\n\
exit /b 0\r\n\
:do_remove\r\n\
if not exist \"{s}\" exit /b 1\r\n\
del /f /q \"{s}\"\r\n\
exit /b 0\r\n",
                s = state.display()
            );
            fs::write(self.bin.join("codex.cmd"), script)?;
        }
        Ok(())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // A pre-existing registration was excluded before constructing this guard.
        // Clean only this owned install location; never a changed registration.
        if self.registration.location().ok().flatten().as_deref() == Some(self.install.as_path()) {
            let uninstaller = self.install.join("unins000.exe");
            let mut command = self.command(&uninstaller);
            command.args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"]);
            if process::checked(&mut command, Duration::from_mins(1)).is_err() {
                eprintln!(
                    "Synthetic installer cleanup failed; inspect the test directory before retrying."
                );
            }
        }
    }
}

fn read_uninstall_log(path: &Path) -> Result<String> {
    let bytes = fs::read(path)?;
    if bytes.starts_with(&[0xff, 0xfe]) {
        Ok(String::from_utf16(
            &bytes[2..]
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect::<Vec<_>>(),
        )?)
    } else {
        Ok(String::from_utf8(bytes)?)
    }
}

pub fn run(directory: &Path, output: &Path, version: &str, production: bool) -> Result<()> {
    let installer = std::path::absolute(directory.join(package::installer_name(version)))?;
    let expected = if production {
        "ControlFreak setup"
    } else {
        "ControlFreak installer test"
    };
    if production && std::env::var("GITHUB_ACTIONS").as_deref() != Ok("true") {
        return Err("Production installer tests require a disposable GitHub Actions runner".into());
    }
    if native::file_description(&installer)?.trim() != expected {
        return Err("Installer identity mismatch; local tests require --test-setup".into());
    }
    let registration = Registration::new(production);
    if registration.location()?.is_some() {
        return Err("An installation is already registered; refusing test".into());
    }
    let output = package::fresh(output)?;
    let mut fixture = Fixture {
        profile: output.join("synthetic-profile"),
        bin: output.join("synthetic-bin"),
        install: output.join("Apps with spaces 日本語/ControlFreak"),
        registration,
        output,
    };
    exercise(&fixture, &installer, directory, version)?;
    // Inno's uninstaller can finish deleting its old files after its launcher
    // exits. A second scenario must not reinstall into that same directory.
    fixture.install = fixture.output.join("Missing helper/ControlFreak");
    fixture.profile = fixture.output.join("missing-helper-profile");
    verify_missing_helper_uninstall(&fixture, &installer)?;
    fixture.install = fixture.output.join("Missing cli cleanup/ControlFreak");
    fixture.profile = fixture.output.join("missing-cli-profile");
    verify_missing_cli_cleanup_uninstall(&fixture, &installer)?;
    fixture.install = fixture.output.join("Legacy receipt upgrade/ControlFreak");
    fixture.profile = fixture.output.join("legacy-receipt-profile");
    verify_legacy_v030_receipt_uninstall(&fixture, &installer)?;
    println!(
        "Installer lifecycle: CLI registration, UTF-8 results, upgrades, legacy receipt notice, refusals, partial failure and uninstall passed."
    );
    Ok(())
}

fn exercise(fixture: &Fixture, installer: &Path, directory: &Path, version: &str) -> Result<()> {
    fs::create_dir_all(&fixture.profile)?;
    fixture.write_synthetic_clis(true, true)?;
    let claude_state = fixture.profile.join("claude.registered");
    let codex_state = fixture.profile.join("codex.registered");
    // Pre-populate Codex so we verify Setup keeps pre-existing entries untouched and does not remove them on uninstall.
    let preexisting_codex = r#"{"name":"controlfreak","enabled":true,"transport":{"type":"stdio","command":"C:\\Existing\\controlfreak.exe","args":["--profile","observe"],"env":null,"env_vars":[],"cwd":null}}"#;
    fs::write(&codex_state, preexisting_codex)?;

    let elevated =
        controlfreak_platform::server_security_context(true).map_or(true, |ctx| ctx.elevated);
    if elevated && fixture.setup_with_flags(installer, true, false)? != 10 {
        return Err("Elevated setup must refuse automatic CLI registration without opt-in".into());
    }
    if fixture.setup(installer, true)? != 0 {
        return Err("Fresh installation failed".into());
    }
    if fixture.registration.location()?.as_deref() != Some(fixture.install.as_path()) {
        return Err("Wrong uninstall registration".into());
    }
    let results_txt = fs::read_to_string(fixture.install.join("setup-results.txt"))?;
    if !results_txt.contains("Apps with spaces 日本語") {
        return Err("setup-results.txt did not preserve the Unicode install path as UTF-8".into());
    }
    if !claude_state.is_file()
        || !fixture
            .install
            .join("installer-state/claude-code.managed")
            .is_file()
    {
        return Err("Claude Code CLI registration or managed marker missing".into());
    }
    if fs::read_to_string(&codex_state)? != preexisting_codex
        || fixture
            .install
            .join("installer-state/codex.managed")
            .exists()
    {
        return Err("Pre-existing Codex entry was modified or wrongly claimed".into());
    }
    verify_blocker_pid(fixture)?;
    let portable = fixture.output.join("portable comparison");
    verify::extract(&directory.join(package::archive_name(version)), &portable)?;
    for binary in package::BINARIES {
        if package::hash(&fixture.install.join(binary))? != package::hash(&portable.join(binary))? {
            return Err("Installer and ZIP binaries differ".into());
        }
    }
    verify::server(&fixture.install.join("controlfreak.exe"), version, None)?;

    if fixture.setup(installer, true)? != 0 {
        return Err("Reinstallation failed".into());
    }
    fixture.registration.set_version("0.0.1")?;
    if fixture.setup(installer, true)? != 0 {
        return Err("Upgrade failed".into());
    }
    fixture.registration.set_version("999.0.0")?;
    if fixture.setup(installer, true)? == 0 {
        return Err("Downgrade was not refused".into());
    }
    fixture.registration.set_version(version)?;
    {
        let _locked = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(fixture.install.join("controlfreak.exe"))?;
        if fixture.setup(installer, true)? == 0 {
            return Err("Setup replaced an in-use executable".into());
        }
    }

    // Missing Claude CLI while selected must still install files and return exit code 10.
    fixture.write_synthetic_clis(false, true)?;
    if fixture.setup(installer, true)? != 10 {
        return Err("Partial CLI registration failure must return exit 10".into());
    }
    fixture.write_synthetic_clis(true, true)?;

    fs::write(
        fixture.install.join("unrelated-fixture.txt"),
        "retain this synthetic file",
    )?;
    if fixture.setup(&fixture.install.join("unins000.exe"), false)? != 0 {
        return Err("Uninstall failed".into());
    }
    if fixture.install.join("controlfreak.exe").exists()
        || fixture.registration.location()?.is_some()
    {
        return Err("Uninstall left executable or registration".into());
    }
    if !fixture.install.join("unrelated-fixture.txt").is_file() {
        return Err("Uninstall removed unrelated content".into());
    }
    if claude_state.exists() {
        return Err("Installer-managed Claude Code registration remains after uninstall".into());
    }
    if fs::read_to_string(&codex_state)? != preexisting_codex {
        return Err("Uninstall modified pre-existing Codex registration".into());
    }
    Ok(())
}

fn verify_missing_helper_uninstall(fixture: &Fixture, installer: &Path) -> Result<()> {
    // A deleted/quarantined helper must not prevent removing the application.
    fs::create_dir_all(&fixture.profile)?;
    fixture.write_synthetic_clis(true, true)?;
    if fixture.setup(installer, true)? != 0 {
        return Err("Missing-helper fixture installation failed".into());
    }
    let claude_state = fixture.profile.join("claude.registered");
    let codex_state = fixture.profile.join("codex.registered");
    if !claude_state.is_file() || !codex_state.is_file() {
        return Err("Missing-helper fixture registrations were not created".into());
    }
    fs::remove_file(fixture.install.join("controlfreak-installer.exe"))
        .map_err(|_| "Missing-helper fixture executable was not installed")?;
    if fixture.setup(&fixture.install.join("unins000.exe"), false)? != 0
        || fixture.install.join("controlfreak.exe").exists()
        || fixture.registration.location()?.is_some()
    {
        return Err("Missing cleanup helper prevented uninstall".into());
    }
    if !claude_state.is_file() || !codex_state.is_file() {
        return Err("Missing-helper uninstall unexpectedly changed registrations".into());
    }
    Ok(())
}

fn verify_missing_cli_cleanup_uninstall(fixture: &Fixture, installer: &Path) -> Result<()> {
    fs::create_dir_all(&fixture.profile)?;
    fixture.write_synthetic_clis(true, true)?;
    if fixture.setup(installer, true)? != 0 {
        return Err("Missing-CLI cleanup fixture installation failed".into());
    }
    // Remove Codex CLI before uninstall; Claude should be removed while Codex reports a warning without blocking uninstall.
    fixture.write_synthetic_clis(true, false)?;
    if fixture.setup(&fixture.install.join("unins000.exe"), false)? != 0
        || fixture.install.join("controlfreak.exe").exists()
        || fixture.registration.location()?.is_some()
    {
        return Err("Missing CLI during cleanup prevented uninstall".into());
    }
    if fixture.profile.join("claude.registered").exists() {
        return Err("Missing Codex CLI prevented Claude Code cleanup".into());
    }
    if !fixture.profile.join("codex.registered").is_file() {
        return Err("Codex entry disappeared without CLI".into());
    }
    let log = read_uninstall_log(&fixture.output.join("uninstall.log"))?;
    if !log.contains("Could not verify or remove MCP entry for: codex") {
        return Err("Uninstall log did not record the CLI cleanup warning".into());
    }
    Ok(())
}

fn verify_legacy_v030_receipt_uninstall(fixture: &Fixture, installer: &Path) -> Result<()> {
    // Seed a v0.3.0-format JSON receipt in installer-state before upgrading in place without
    // re-selecting clients, then verify uninstall leaves the legacy receipt untouched and logs
    // manual cleanup guidance.
    fs::create_dir_all(&fixture.profile)?;
    let state_dir = fixture.install.join("installer-state");
    fs::create_dir_all(&state_dir)?;
    let legacy_receipt = state_dir.join("claude-desktop.json");
    let receipt_body = r#"[{"client":"claude-desktop","path":"C:\\Synthetic\\claude_desktop_config.json","entry":"{}","committed":true}]"#;
    fs::write(&legacy_receipt, receipt_body)?;
    fixture.write_synthetic_clis(true, true)?;

    // Install with no /CLIENTS selected (files-only upgrade over v0.3.0 state).
    let mut install_cmd = fixture.command(installer);
    install_cmd
        .args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"])
        .arg(format!("/DIR={}", fixture.install.display()));
    let install_code = process::run(&mut install_cmd, "", Duration::from_mins(1))?
        .status
        .code();
    if install_code != Some(0) {
        return Err("Legacy-receipt upgrade installation failed".into());
    }

    if fixture.setup(&fixture.install.join("unins000.exe"), false)? != 0
        || fixture.install.join("controlfreak.exe").exists()
        || fixture.registration.location()?.is_some()
    {
        return Err("Legacy receipt prevented uninstall".into());
    }
    if fs::read_to_string(&legacy_receipt)? != receipt_body {
        return Err("Uninstall modified or removed a legacy v0.3.0 receipt".into());
    }
    let log = read_uninstall_log(&fixture.output.join("uninstall.log"))?;
    if !log.contains("Legacy MCP receipts detected") || !log.contains(".controlfreak-backup-*.bak")
    {
        return Err(
            "Uninstall log did not record manual cleanup guidance for v0.3.0 receipts".into(),
        );
    }
    Ok(())
}

fn verify_blocker_pid(fixture: &Fixture) -> Result<()> {
    // This owned server waits for STDIO requests. No capture or mutation is sent.
    let mut child = fixture
        .command(&fixture.install.join("controlfreak.exe"))
        .arg("--allow-elevated")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(0x0800_0000)
        .spawn()?;
    let result = (|| -> Result<()> {
        let report = fixture.output.join("blockers.ini");
        process::checked(
            fixture
                .command(&fixture.install.join("controlfreak-installer.exe"))
                .arg("blockers")
                .arg(&fixture.install)
                .arg(&report),
            Duration::from_secs(20),
        )?;
        let bytes = fs::read(report)?;
        let words: Vec<_> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let text = String::from_utf16(&words)?;
        if !text.contains(&format!("PID: {}, holds file", child.id())) {
            return Err(
                "Blocker diagnostics did not identify the owned server PID as holding the file"
                    .into(),
            );
        }
        if !text
            .lines()
            .any(|line| line.starts_with("process0=") && line.contains("PID: "))
            || !text.contains("\r\naction=")
        {
            return Err("Blocker report must separate instructions and process entries".into());
        }
        if child.try_wait()?.is_some() {
            return Err("Blocker inspection stopped the server".into());
        }
        Ok(())
    })();
    // Only terminate the server this fixture just started, retaining its handle
    // through termination and reaping. Never terminate a process by name.
    let _ = child.kill();
    let _ = child.wait();
    result
}
