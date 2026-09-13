use crate::{Result, package, process, verify};
use controlfreak_platform::packaging_tests::{self as native, Registration};
use std::os::windows::fs::OpenOptionsExt;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
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
            command.arg("/REMOVECONFIG=1");
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
    let fixture = Fixture {
        profile: output.join("synthetic-profile"),
        install: output.join("Apps with spaces 日本語/ControlFreak"),
        registration,
        output,
    };
    exercise(&fixture, &installer, directory, version)
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
    println!(
        "Installer lifecycle: client configuration, permissions, upgrades, refusals, partial failure and uninstall passed."
    );
    Ok(())
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
