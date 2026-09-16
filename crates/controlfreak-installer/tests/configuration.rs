use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};
use tempfile::TempDir;

struct Fixture {
    root: TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self {
            root: tempfile::tempdir().unwrap(),
        }
    }
    fn path(&self, path: &str) -> PathBuf {
        self.root.path().join(path)
    }
    fn write(&self, path: &str, text: &str) {
        let path = self.path(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_controlfreak-installer"));
        // Tests must never discover or edit the developer's actual client profiles.
        for name in [
            "CODEX_HOME",
            "CLAUDE_CONFIG_DIR",
            "PI_CODING_AGENT_DIR",
            "OPENCODE_CONFIG",
            "OPENCODE_CONFIG_DIR",
            "XDG_CONFIG_HOME",
        ] {
            cmd.env_remove(name);
        }
        cmd.env("USERPROFILE", self.path("user"))
            .env("LOCALAPPDATA", self.path("local"))
            .env("APPDATA", self.path("roaming"))
            .env("PATH", "");
        cmd
    }
    fn configure(&self, id: &str, policy: &str) -> Output {
        self.command()
            .arg("configure")
            .arg(id)
            .arg(self.path("Apps with spaces/日本語/controlfreak.exe"))
            .arg(self.path("state"))
            .arg(self.path("result.ini"))
            .arg(policy)
            .output()
            .unwrap()
    }
    fn remove(&self) -> Output {
        self.command()
            .arg("remove")
            .arg(self.path("state"))
            .arg(self.path("result.ini"))
            .output()
            .unwrap()
    }
    fn report(&self) -> String {
        fs::read_to_string(self.path("result.ini")).unwrap()
    }
    fn json(&self, path: &str) -> Value {
        serde_json::from_str(&fs::read_to_string(self.path(path)).unwrap()).unwrap()
    }
}

const CLIENTS: [(&str, &str, &str); 5] = [
    ("codex", "user/.codex/config.toml", "mcp_servers"),
    ("claude-code", "user/.claude.json", "mcpServers"),
    (
        "claude-desktop",
        "roaming/Claude/claude_desktop_config.json",
        "mcpServers",
    ),
    ("pi", "user/.pi/agent/mcp.json", "mcpServers"),
    ("opencode", "user/.config/opencode/opencode.json", "mcp"),
];

#[test]
fn all_clients_install_reinstall_and_remove_in_synthetic_profiles() {
    for (id, path, group) in CLIENTS {
        let f = Fixture::new();
        assert!(f.configure(id, "keep").status.success(), "{}", f.report());
        let installed = fs::read_to_string(f.path(path)).unwrap();
        assert!(installed.contains("controlfreak"));
        assert!(installed.contains("日本語"));
        assert!(f.configure(id, "keep").status.success());
        assert_eq!(fs::read_to_string(f.path(path)).unwrap(), installed);
        assert!(f.remove().status.success(), "{}", f.report());
        if id == "codex" {
            let doc: toml_edit::DocumentMut =
                fs::read_to_string(f.path(path)).unwrap().parse().unwrap();
            assert!(doc[group].get("controlfreak").is_none());
        } else {
            assert!(f.json(path)[group].get("controlfreak").is_none());
        }
        assert!(f.remove().status.success());
    }
}

#[test]
fn json_edits_preserve_other_servers_preferences_and_create_exact_backup() {
    let f = Fixture::new();
    let original = "{\n  \"preferences\": {\"theme\":\"dark\"},\n  \"mcpServers\": {\"synthetic\": {\"command\":\"fixture.exe\",\"env\":{\"FIXTURE_TOKEN\":\"synthetic-only\"}}}\n}\n";
    let path = "roaming/Claude/claude_desktop_config.json";
    f.write(path, original);
    assert!(f.configure("claude-desktop", "keep").status.success());
    let expected: Value = serde_json::from_str(original).unwrap();
    let result = f.json(path);
    assert_eq!(result["preferences"], expected["preferences"]);
    assert_eq!(
        result["mcpServers"]["synthetic"],
        expected["mcpServers"]["synthetic"]
    );
    let backups: Vec<_> = fs::read_dir(f.path("roaming/Claude"))
        .unwrap()
        .flatten()
        .filter(|p| p.path().extension().is_some_and(|v| v == "bak"))
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(fs::read_to_string(backups[0].path()).unwrap(), original);
}

#[test]
fn toml_comments_and_other_tables_survive() {
    let f = Fixture::new();
    let original = "# preserve this comment\nmodel = 'synthetic-model'\n[mcp_servers.other]\ncommand = 'fixture.exe' # preserve inline\n";
    f.write("user/.codex/config.toml", original);
    assert!(f.configure("codex", "keep").status.success());
    let result = fs::read_to_string(f.path("user/.codex/config.toml")).unwrap();
    assert!(result.contains(original));
    assert!(f.remove().status.success());
    assert_eq!(
        fs::read_to_string(f.path("user/.codex/config.toml")).unwrap(),
        original
    );
}

#[test]
fn jsonc_comments_trailing_commas_and_other_settings_survive() {
    let f = Fixture::new();
    let path = "user/.config/opencode/opencode.jsonc";
    f.write(path, "{\n  // preserve\n  \"theme\": \"dark\",\n  \"mcp\": {\"other\": {\"type\":\"local\",\"command\":[\"fixture.exe\"],},},\n}\n");
    assert!(
        f.configure("opencode", "keep").status.success(),
        "{}",
        f.report()
    );
    let text = fs::read_to_string(f.path(path)).unwrap();
    assert!(text.contains("// preserve"));
    assert!(text.contains("\"theme\": \"dark\""));
    assert!(!f.path("user/.config/opencode/opencode.json").exists());
    assert!(f.remove().status.success());
    assert!(
        fs::read_to_string(f.path(path))
            .unwrap()
            .contains("// preserve")
    );
}

#[test]
fn conflicts_are_kept_unless_explicitly_replaced() {
    let f = Fixture::new();
    let path = "user/.claude.json";
    let original = r#"{"mcpServers":{"controlfreak":{"command":"old-fixture.exe","args":["--allow-elevated"]}}}"#;
    f.write(path, original);
    assert!(f.configure("claude-code", "keep").status.success());
    assert_eq!(fs::read_to_string(f.path(path)).unwrap(), original);
    assert!(!f.path("state/claude-code.json").exists());
    assert!(f.configure("claude-code", "replace").status.success());
    assert_eq!(
        f.json(path)["mcpServers"]["controlfreak"]["args"],
        serde_json::json!([])
    );
    assert!(f.remove().status.success());
}

#[test]
fn uninstall_keeps_user_modified_entries_and_unrelated_content() {
    let f = Fixture::new();
    assert!(f.configure("claude-code", "keep").status.success());
    let changed =
        r#"{"mcpServers":{"controlfreak":{"command":"changed.exe"}},"preferences":{"keep":true}}"#;
    f.write("user/.claude.json", changed);
    assert!(f.remove().status.success());
    assert_eq!(
        fs::read_to_string(f.path("user/.claude.json")).unwrap(),
        changed
    );
}

#[test]
fn malformed_ambiguous_or_wrong_type_configs_are_not_overwritten_or_logged() {
    for original in [
        "not json synthetic-secret",
        "[]",
        r#"{"mcpServers":null}"#,
        r#"{"mcpServers":{},"mcpServers":{}}"#,
        r#"{"mcpServers":{"same":{},"same":{}}}"#,
        "{\"a\":1 \"b\":2}",
    ] {
        let f = Fixture::new();
        f.write("user/.claude.json", original);
        let result = f.configure("claude-code", "replace");
        assert!(!result.status.success());
        assert_eq!(
            fs::read_to_string(f.path("user/.claude.json")).unwrap(),
            original
        );
        assert!(!String::from_utf8_lossy(&result.stderr).contains("synthetic-secret"));
        assert!(!f.report().contains("synthetic-secret"));
    }
}

#[test]
fn ambiguous_opencode_files_are_refused() {
    let f = Fixture::new();
    f.write("user/.config/opencode/opencode.json", "{}");
    f.write("user/.config/opencode/opencode.jsonc", "{}");
    assert!(!f.configure("opencode", "replace").status.success());
    assert_eq!(
        f.json("user/.config/opencode/opencode.json"),
        serde_json::json!({})
    );
}

#[test]
fn overrides_are_respected_and_relative_overrides_refused() {
    for (id, variable, suffix) in [
        ("codex", "CODEX_HOME", "config.toml"),
        ("claude-code", "CLAUDE_CONFIG_DIR", ".claude.json"),
        ("pi", "PI_CODING_AGENT_DIR", "mcp.json"),
        ("opencode", "OPENCODE_CONFIG_DIR", "opencode.json"),
    ] {
        let f = Fixture::new();
        let mut command = f.command();
        command.env(variable, f.path("custom"));
        let result = command
            .arg("configure")
            .arg(id)
            .arg(f.path("app/controlfreak.exe"))
            .arg(f.path("state"))
            .arg(f.path("result.ini"))
            .arg("keep")
            .output()
            .unwrap();
        assert!(result.status.success(), "{}", f.report());
        assert!(f.path("custom").join(suffix).exists());
        let result = f
            .command()
            .env(variable, "relative")
            .arg("probe")
            .arg(id)
            .arg(f.path("app/controlfreak.exe"))
            .arg(f.path("result.ini"))
            .output()
            .unwrap();
        assert!(!result.status.success());
    }
}

#[test]
fn pi_prerequisite_is_reported_without_installing_packages() {
    let f = Fixture::new();
    let output = f
        .command()
        .args(["probe", "pi"])
        .arg(f.path("app/controlfreak.exe"))
        .arg(f.path("result.ini"))
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(f.report().contains("pi install npm:pi-mcp-adapter"));
    assert!(!f.path("user").exists());
}

#[test]
fn downgrade_and_invalid_version_are_refused() {
    let f = Fixture::new();
    for version in ["999.0.0", "invalid"] {
        assert!(
            !f.command()
                .args(["check-version", version])
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    for version in ["0.0.1", env!("CARGO_PKG_VERSION")] {
        assert!(
            f.command()
                .args(["check-version", version])
                .output()
                .unwrap()
                .status
                .success()
        );
    }
}

#[test]
fn client_interpolation_in_install_path_is_refused() {
    let f = Fixture::new();
    for directory in ["${VARIABLE}", "{env:VARIABLE}", "{file:fixture}"] {
        let result = f
            .command()
            .args(["configure", "claude-code"])
            .arg(f.path(directory).join("controlfreak.exe"))
            .arg(f.path("state"))
            .arg(f.path("result.ini"))
            .arg("replace")
            .output()
            .unwrap();
        assert!(!result.status.success());
    }
}

#[test]
fn uninstall_uses_receipt_path_after_environment_override_changes() {
    let f = Fixture::new();
    assert!(f.configure("claude-code", "keep").status.success());
    let result = f
        .command()
        .env("CLAUDE_CONFIG_DIR", f.path("different"))
        .arg("remove")
        .arg(f.path("state"))
        .arg(f.path("result.ini"))
        .output()
        .unwrap();
    assert!(result.status.success());
    assert!(
        f.json("user/.claude.json")["mcpServers"]
            .get("controlfreak")
            .is_none()
    );
    assert!(!f.path("different").exists());
}

#[test]
fn inaccessible_config_is_a_partial_failure_without_affecting_other_clients() {
    let f = Fixture::new();
    fs::create_dir_all(f.path("user/.claude.json")).unwrap();
    assert!(!f.configure("claude-code", "keep").status.success());
    assert!(f.configure("codex", "keep").status.success());
    assert!(Path::new(&f.path("user/.claude.json")).is_dir());
}

#[test]
fn concurrent_setup_is_refused_without_changing_configuration() {
    let f = Fixture::new();
    fs::create_dir_all(f.path("local/ControlFreak")).unwrap();
    let lock = fs::File::create(f.path("local/ControlFreak/installer.lock")).unwrap();
    lock.lock().unwrap();
    assert!(!f.configure("codex", "keep").status.success());
    assert!(f.report().contains("Another ControlFreak setup"));
    assert!(!f.path("user/.codex/config.toml").exists());
}

#[test]
fn bom_and_large_config_limits_are_preserved() {
    let f = Fixture::new();
    f.write("user/.claude.json", "\u{feff}{}");
    assert!(f.configure("claude-code", "keep").status.success());
    assert!(
        fs::read_to_string(f.path("user/.claude.json"))
            .unwrap()
            .starts_with('\u{feff}')
    );
    let large = " ".repeat(8 * 1024 * 1024 + 1);
    f.write("user/.claude.json", &large);
    assert!(!f.configure("claude-code", "replace").status.success());
    assert_eq!(
        fs::metadata(f.path("user/.claude.json")).unwrap().len(),
        large.len() as u64
    );
}

#[test]
fn unowned_identical_entry_is_not_claimed_or_removed() {
    let f = Fixture::new();
    let original = serde_json::json!({"mcpServers":{"controlfreak":{"command":f.path("Apps with spaces/日本語/controlfreak.exe"),"args":[]}}}).to_string();
    f.write("user/.claude.json", &original);
    assert!(f.configure("claude-code", "replace").status.success());
    assert!(!f.path("state/claude-code.json").exists());
    assert!(f.remove().status.success());
    assert_eq!(
        fs::read_to_string(f.path("user/.claude.json")).unwrap(),
        original
    );
}

#[test]
fn reconfiguration_preserves_legacy_receipts_at_previous_paths() {
    let f = Fixture::new();
    assert!(f.configure("codex", "replace").status.success());
    // Emulate the single-receipt format written by older installers.
    let mut first = f.json("state/codex.json")[0].clone();
    first.as_object_mut().unwrap().remove("committed");
    f.write("state/codex.json", &first.to_string());
    let result = f
        .command()
        .env("CODEX_HOME", f.path("other-codex"))
        .arg("configure")
        .arg("codex")
        .arg(f.path("Apps/controlfreak.exe"))
        .arg(f.path("state"))
        .arg(f.path("result.ini"))
        .arg("replace")
        .output()
        .unwrap();
    assert!(result.status.success(), "{}", f.report());
    assert_eq!(f.json("state/codex.json").as_array().unwrap().len(), 2);
    assert!(f.remove().status.success(), "{}", f.report());
    for path in ["user/.codex/config.toml", "other-codex/config.toml"] {
        assert!(
            !fs::read_to_string(f.path(path))
                .unwrap()
                .contains("controlfreak")
        );
    }
}

#[test]
fn uninstall_retains_malformed_configs_and_cleans_other_clients() {
    for (id, path, _) in CLIENTS {
        let f = Fixture::new();
        assert!(f.configure(id, "replace").status.success());
        let other = if id == "codex" {
            "claude-code"
        } else {
            "codex"
        };
        assert!(f.configure(other, "replace").status.success());
        let malformed = "{ synthetic broken configuration [";
        f.write(path, malformed);
        assert!(f.remove().status.success(), "{}", f.report());
        assert_eq!(fs::read_to_string(f.path(path)).unwrap(), malformed);
        let other_path = CLIENTS
            .iter()
            .find(|(client, _, _)| *client == other)
            .unwrap()
            .1;
        assert!(
            !fs::read_to_string(f.path(other_path))
                .unwrap()
                .contains("controlfreak")
        );
    }
}

#[test]
fn failed_write_does_not_claim_a_later_matching_manual_entry() {
    use std::os::windows::fs::OpenOptionsExt;
    let f = Fixture::new();
    f.write("user/.claude.json", "{}");
    // Permit the initial read but deny the helper's later write handle.
    let locked = fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(f.path("user/.claude.json"))
        .unwrap();
    assert!(!f.configure("claude-code", "replace").status.success());
    drop(locked);
    assert_eq!(
        fs::read_to_string(f.path("user/.claude.json")).unwrap(),
        "{}"
    );
    let pending = f.json("state/claude-code.json")[0].clone();
    assert_eq!(pending["committed"], false);
    let entry: Value = serde_json::from_str(pending["entry"].as_str().unwrap()).unwrap();
    let manually_added = serde_json::json!({"mcpServers":{"controlfreak":entry}}).to_string();
    f.write("user/.claude.json", &manually_added);
    assert!(f.remove().status.success(), "{}", f.report());
    assert_eq!(
        fs::read_to_string(f.path("user/.claude.json")).unwrap(),
        manually_added
    );
}

#[test]
fn successful_update_replaces_ownership_only_for_the_same_path() {
    let f = Fixture::new();
    assert!(f.configure("claude-code", "replace").status.success());
    let first = fs::read_to_string(f.path("user/.claude.json")).unwrap();
    let result = f
        .command()
        .arg("configure")
        .arg("claude-code")
        .arg(f.path("Different/controlfreak.exe"))
        .arg(f.path("state"))
        .arg(f.path("result.ini"))
        .arg("replace")
        .output()
        .unwrap();
    assert!(result.status.success(), "{}", f.report());
    assert_eq!(
        f.json("state/claude-code.json").as_array().unwrap().len(),
        1
    );
    f.write("user/.claude.json", &first);
    assert!(f.remove().status.success());
    assert_eq!(
        fs::read_to_string(f.path("user/.claude.json")).unwrap(),
        first
    );
}

#[test]
fn damaged_receipts_retain_their_config_without_blocking_other_cleanup() {
    for damage in ["truncated", "invalid-utf8", "unreadable"] {
        let f = Fixture::new();
        assert!(f.configure("codex", "replace").status.success());
        assert!(f.configure("claude-code", "replace").status.success());
        let original = fs::read(f.path("user/.codex/config.toml")).unwrap();
        match damage {
            "truncated" => f.write("state/codex.json", "{ broken synthetic receipt"),
            "invalid-utf8" => fs::write(f.path("state/codex.json"), [0xff, 0xfe]).unwrap(),
            _ => {
                fs::remove_file(f.path("state/codex.json")).unwrap();
                fs::create_dir(f.path("state/codex.json")).unwrap();
            }
        }
        assert!(f.remove().status.success(), "{}", f.report());
        assert_eq!(
            fs::read(f.path("user/.codex/config.toml")).unwrap(),
            original
        );
        assert!(f.path("state/codex.json").exists());
        assert!(
            f.json("user/.claude.json")["mcpServers"]
                .get("controlfreak")
                .is_none()
        );
    }
}

#[test]
fn unavailable_receipt_retains_its_config_without_blocking_other_cleanup() {
    use std::os::windows::fs::OpenOptionsExt;
    let f = Fixture::new();
    assert!(f.configure("codex", "replace").status.success());
    assert!(f.configure("claude-code", "replace").status.success());
    let receipt = f.path("state/codex.json");
    // Allow reads but deny deletion until the synthetic guard is released.
    let guard = fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&receipt)
        .unwrap();
    assert!(f.remove().status.success(), "{}", f.report());
    assert!(receipt.exists());
    assert!(!f.path("state/claude-code.json").exists());
    assert!(
        fs::read_to_string(f.path("user/.codex/config.toml"))
            .unwrap()
            .contains("controlfreak")
    );
    assert!(
        f.json("user/.claude.json")["mcpServers"]
            .get("controlfreak")
            .is_none()
    );
    drop(guard);
}

#[test]
fn backup_preparation_failure_retains_config_and_continues_other_clients() {
    let f = Fixture::new();
    // A valid source filename whose added backup suffix exceeds NTFS's
    // component limit forces backup creation failure without changing ACLs.
    let config = f.path(&format!("user/{}.json", "x".repeat(235)));
    assert!(f.configure("opencode", "replace").status.success());
    fs::rename(f.path("user/.config/opencode/opencode.json"), &config).unwrap();
    let mut receipt = f.json("state/opencode.json");
    receipt[0]["path"] = serde_json::to_value(&config).unwrap();
    f.write("state/opencode.json", &receipt.to_string());
    let original = fs::read(&config).unwrap();
    assert!(f.configure("codex", "replace").status.success());
    assert!(f.remove().status.success(), "{}", f.report());
    assert_eq!(fs::read(&config).unwrap(), original);
    assert!(f.path("state/opencode.json").exists());
    assert!(!f.path("state/codex.json").exists());
    assert!(f.report().contains("status=warning"));
    assert!(f.report().contains("opencode"));
    assert!(
        !fs::read_to_string(f.path("user/.codex/config.toml"))
            .unwrap()
            .contains("controlfreak")
    );
}

#[test]
fn busy_configuration_is_reported_and_can_be_retried_from_retained_receipt() {
    use std::os::windows::fs::OpenOptionsExt;
    let f = Fixture::new();
    assert!(f.configure("codex", "replace").status.success());
    assert!(f.configure("claude-code", "replace").status.success());
    let path = f.path("user/.codex/config.toml");
    let original = fs::read(&path).unwrap();
    let guard = fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&path)
        .unwrap();
    assert!(f.remove().status.success(), "{}", f.report());
    assert_eq!(fs::read(&path).unwrap(), original);
    assert!(f.path("state/codex.json").exists());
    assert!(!f.path("state/claude-code.json").exists());
    assert!(f.report().contains("status=warning"));
    drop(guard);
    assert!(f.remove().status.success(), "{}", f.report());
    assert!(!f.path("state/codex.json").exists());
    assert!(f.report().contains("status=ok"));
    assert!(!fs::read_to_string(path).unwrap().contains("controlfreak"));
}

#[test]
fn partial_profile_cleanup_drops_completed_ownership_and_retains_retry_state() {
    use std::os::windows::fs::OpenOptionsExt;
    let f = Fixture::new();
    assert!(f.configure("codex", "replace").status.success());
    let second = f.path("user/second-codex");
    let result = f
        .command()
        .env("CODEX_HOME", &second)
        .arg("configure")
        .arg("codex")
        .arg(f.path("Apps/controlfreak.exe"))
        .arg(f.path("state"))
        .arg(f.path("result.ini"))
        .arg("replace")
        .output()
        .unwrap();
    assert!(result.status.success(), "{}", f.report());
    let second_config = second.join("config.toml");
    let original = fs::read(&second_config).unwrap();
    let guard = fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(f.path("user/.codex/config.toml"))
        .unwrap();
    assert!(f.remove().status.success(), "{}", f.report());
    assert_eq!(f.json("state/codex.json").as_array().unwrap().len(), 1);
    assert!(
        !fs::read_to_string(&second_config)
            .unwrap()
            .contains("controlfreak")
    );
    // A manually recreated entry in the completed profile is no longer owned.
    fs::write(&second_config, &original).unwrap();
    drop(guard);
    assert!(f.remove().status.success(), "{}", f.report());
    assert_eq!(fs::read(second_config).unwrap(), original);
    assert!(!f.path("state/codex.json").exists());
}
