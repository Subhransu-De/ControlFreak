use crate::{Result, package, process, verify};
use controlfreak_platform::packaging_tests::{self as native, Registration};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::process::CommandExt;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

const CLIENT_PATHS: [&str; 5] = [
    ".codex/config.toml",
    ".claude.json",
    "roaming/Claude/claude_desktop_config.json",
    ".pi/agent/mcp.json",
    ".config/opencode/opencode.json",
];

struct Fixture {
    output: PathBuf,
    profile: PathBuf,
    install: PathBuf,
    registration: Registration,
}
impl Fixture {
    fn command(&self, executable: &Path) -> Command {
        let mut command = Command::new(executable);
        for name in [
            "CODEX_HOME",
            "CLAUDE_CONFIG_DIR",
            "PI_CODING_AGENT_DIR",
            "OPENCODE_CONFIG",
            "OPENCODE_CONFIG_DIR",
            "XDG_CONFIG_HOME",
        ] {
            command.env_remove(name);
        }
        command
            .env("USERPROFILE", &self.profile)
            .env("LOCALAPPDATA", self.profile.join("local"))
            .env("APPDATA", self.profile.join("roaming"));
        command
    }
    fn setup(&self, executable: &Path, configure: bool) -> Result<i32> {
        let mut command = self.command(executable);
        command.args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"]);
        if configure {
            command
                .arg(format!("/DIR={}", self.install.display()))
                .arg("/CLIENTS=codex,claude-code,claude-desktop,pi,opencode");
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
        install: output.join("Apps with spaces 日本語/ControlFreak"),
        registration,
        output,
    };
    exercise(&fixture, &installer, directory, version)?;
    // Inno's uninstaller can finish deleting its old files after its launcher
    // exits. A second scenario must not reinstall into that same directory.
    fixture.install = fixture.output.join("Missing helper/ControlFreak");
    verify_missing_helper_uninstall(&fixture, &installer)?;
    fixture.install = fixture.output.join("Cleanup unavailable/ControlFreak");
    fixture.profile = fixture.output.join("cleanup-unavailable-profile");
    verify_failed_cleanup_uninstall(&fixture, &installer)?;
    fixture.install = fixture.output.join("Cleanup warning/ControlFreak");
    fixture.profile = fixture.output.join("cleanup-warning-profile");
    verify_retained_cleanup_uninstall(&fixture, &installer)?;
    println!(
        "Installer lifecycle: client configuration, permissions, upgrades, refusals, partial failure and uninstall passed."
    );
    Ok(())
}

fn exercise(fixture: &Fixture, installer: &Path, directory: &Path, version: &str) -> Result<()> {
    fs::create_dir_all(&fixture.profile)?;
    let config = fixture.profile.join(".claude.json");
    fs::write(
        &config,
        r#"{"preferences":{"synthetic":true},"mcpServers":{"controlfreak":{"command":"old-synthetic.exe"},"other":{"command":"other-synthetic.exe"}}}"#,
    )?;
    native::restrict_fixture_permissions(&config)?;
    let permissions = native::configuration_permissions(&config)?;
    if fixture.setup(installer, true)? != 0 {
        return Err("Fresh installation failed".into());
    }
    if fixture.registration.location()?.as_deref() != Some(fixture.install.as_path()) {
        return Err("Wrong uninstall registration".into());
    }
    if native::configuration_permissions(&config)? != permissions {
        return Err("Configuration permissions changed".into());
    }
    let backups = fs::read_dir(&fixture.profile)?
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(".claude.json.controlfreak-backup-")
        })
        .collect::<Vec<_>>();
    if backups.len() != 1 || native::configuration_permissions(&backups[0].path())? != permissions {
        return Err("Backup permissions changed".into());
    }
    verify_updated_entry(fixture, &config)?;
    verify_blocker_pid(fixture)?;
    let portable = fixture.output.join("portable comparison");
    verify::extract(&directory.join(package::archive_name(version)), &portable)?;
    for binary in package::BINARIES {
        if package::hash(&fixture.install.join(binary))? != package::hash(&portable.join(binary))? {
            return Err("Installer and ZIP binaries differ".into());
        }
    }
    verify::server(&fixture.install.join("controlfreak.exe"), version)?;
    for path in CLIENT_PATHS {
        if !fs::read_to_string(fixture.profile.join(path))?.contains("controlfreak") {
            return Err("Selected client configuration missing".into());
        }
    }
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
    fs::write(&config, "synthetic invalid JSON")?;
    if fixture.setup(installer, true)? != 10 {
        return Err("Partial configuration failure must return exit 10".into());
    }
    fs::write(&config, "{}")?;
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
    for path in CLIENT_PATHS {
        if fs::read_to_string(fixture.profile.join(path))?.contains("controlfreak") {
            return Err("Installer-created configuration remains".into());
        }
    }
    Ok(())
}

fn verify_missing_helper_uninstall(fixture: &Fixture, installer: &Path) -> Result<()> {
    // A deleted/quarantined helper must not prevent removing the application.
    if fixture.setup(installer, true)? != 0 {
        return Err("Missing-helper fixture installation failed".into());
    }
    let configs = CLIENT_PATHS
        .iter()
        .map(|path| fs::read(fixture.profile.join(path)))
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|_| "Missing-helper fixture configuration was not created")?;
    fs::remove_file(fixture.install.join("controlfreak-installer.exe"))
        .map_err(|_| "Missing-helper fixture executable was not installed")?;
    if fixture.setup(&fixture.install.join("unins000.exe"), false)? != 0
        || fixture.install.join("controlfreak.exe").exists()
        || fixture.registration.location()?.is_some()
    {
        return Err("Missing cleanup helper prevented uninstall".into());
    }
    for (path, original) in CLIENT_PATHS.iter().zip(configs) {
        if fs::read(fixture.profile.join(path))? != original {
            return Err("Missing-helper uninstall changed client configuration".into());
        }
    }
    Ok(())
}

fn verify_failed_cleanup_uninstall(fixture: &Fixture, installer: &Path) -> Result<()> {
    if fixture.setup(installer, true)? != 0 {
        return Err("Cleanup-failure fixture installation failed".into());
    }
    let configs = CLIENT_PATHS
        .iter()
        .map(|path| fs::read(fixture.profile.join(path)))
        .collect::<std::io::Result<Vec<_>>>()?;
    // Make the real helper return an error before touching any configuration.
    // This checks the installer's failure policy, not only helper exit codes.
    let guard = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(fixture.profile.join("local/ControlFreak/installer.lock"))?;
    guard.try_lock()?;
    if fixture.setup(&fixture.install.join("unins000.exe"), false)? != 0
        || fixture.install.join("controlfreak.exe").exists()
        || fixture.registration.location()?.is_some()
    {
        return Err("Optional configuration cleanup failure prevented uninstall".into());
    }
    for (path, original) in CLIENT_PATHS.iter().zip(configs) {
        if fs::read(fixture.profile.join(path))? != original {
            return Err("Unavailable cleanup modified client configuration".into());
        }
    }
    Ok(())
}

fn verify_retained_cleanup_uninstall(fixture: &Fixture, installer: &Path) -> Result<()> {
    if fixture.setup(installer, true)? != 0 {
        return Err("Cleanup-warning fixture installation failed".into());
    }
    let config = fixture.profile.join(CLIENT_PATHS[0]);
    let original = fs::read(&config)?;
    let _guard = fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&config)?;
    if fixture.setup(&fixture.install.join("unins000.exe"), false)? != 0
        || fixture.install.join("controlfreak.exe").exists()
        || fixture.registration.location()?.is_some()
    {
        return Err("Retained configuration prevented uninstall".into());
    }
    if fs::read(&config)? != original
        || !fixture.install.join("installer-state/codex.json").exists()
    {
        return Err("Cleanup warning lost retained configuration or receipt".into());
    }
    for path in CLIENT_PATHS.iter().skip(1) {
        if fs::read_to_string(fixture.profile.join(path))?.contains("controlfreak") {
            return Err("Retained configuration prevented other client cleanup".into());
        }
    }
    let bytes = fs::read(fixture.output.join("uninstall.log"))?;
    let log = if bytes.starts_with(&[0xff, 0xfe]) {
        String::from_utf16(
            &bytes[2..]
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect::<Vec<_>>(),
        )?
    } else {
        String::from_utf8(bytes)?
    };
    if !log.contains("Cleanup incomplete for: codex") {
        return Err("Successful helper exit concealed a cleanup warning".into());
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
        if !text.contains(&format!("PID: {}", child.id())) {
            return Err("Blocker diagnostics did not identify the owned server PID".into());
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

fn verify_updated_entry(fixture: &Fixture, config: &Path) -> Result<()> {
    let updated: serde_json::Value = serde_json::from_str(&fs::read_to_string(config)?)?;
    let command = updated["mcpServers"]["controlfreak"]["command"]
        .as_str()
        .ok_or("Updated client command is missing")?;
    if Path::new(command).canonicalize()?
        != fixture.install.join("controlfreak.exe").canonicalize()?
        || updated["mcpServers"]["other"]["command"] != "other-synthetic.exe"
        || updated["preferences"]["synthetic"] != true
    {
        return Err(
            "Install did not update the existing entry while preserving other settings".into(),
        );
    }
    Ok(())
}
