use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn helper() -> &'static str {
    env!("CARGO_BIN_EXE_controlfreak-installer")
}

fn system_path_prefix(bin_dir: &Path) -> std::ffi::OsString {
    let sys_root = env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    let sys_root = PathBuf::from(sys_root);
    env::join_paths([bin_dir.to_path_buf(), sys_root.join("System32"), sys_root])
        .expect("join synthetic PATH")
}

fn running_elevated() -> bool {
    controlfreak_platform::server_security_context(true).map_or(true, |ctx| ctx.elevated)
}

fn run_helper_raw(bin_dir: &Path, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(helper());
    cmd.args(args)
        .env("PATH", system_path_prefix(bin_dir))
        .env_remove("CODEX_HOME")
        .env_remove("CLAUDE_CONFIG_DIR");
    for (key, value) in extra_env {
        cmd.env(key, value);
    }
    cmd.output().expect("run controlfreak-installer helper")
}

fn run_helper_with_env(bin_dir: &Path, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
    if running_elevated()
        && let Some((cmd_name, rest)) = args.split_first()
        && matches!(*cmd_name, "register-cli" | "remove-cli")
    {
        let mut adjusted = Vec::with_capacity(args.len() + 1);
        adjusted.push(*cmd_name);
        adjusted.push("--allow-elevated");
        adjusted.extend_from_slice(rest);
        return run_helper_raw(bin_dir, &adjusted, extra_env);
    }
    run_helper_raw(bin_dir, args, extra_env)
}

fn run_helper(bin_dir: &Path, args: &[&str]) -> Output {
    run_helper_with_env(bin_dir, args, &[])
}

fn read_utf16_ini(path: &Path) -> String {
    let bytes = fs::read(path).expect("read UTF-16 INI report");
    assert!(bytes.starts_with(&[0xff, 0xfe]), "expected UTF-16 LE BOM");
    let words: Vec<u16> = bytes[2..]
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();
    String::from_utf16(&words).expect("decode UTF-16 INI report")
}

fn write_fake_claude(bin_dir: &Path, state_file: &Path, log_file: &Path) {
    let script = format!(
        "@echo off\r\n\
chcp 65001 >nul\r\n\
echo %*>>\"{log}\"\r\n\
if \"%1 %2\"==\"mcp add\" goto do_add\r\n\
if \"%1 %2\"==\"mcp remove\" goto do_remove\r\n\
exit /b 1\r\n\
:do_add\r\n\
if exist \"{state}\" goto already\r\n\
echo registered>\"{state}\"\r\n\
exit /b 0\r\n\
:already\r\n\
echo MCP server controlfreak already exists in user config 1>&2\r\n\
exit /b 1\r\n\
:do_remove\r\n\
if not exist \"{state}\" exit /b 1\r\n\
del /f /q \"{state}\"\r\n\
exit /b 0\r\n",
        log = log_file.display(),
        state = state_file.display()
    );
    fs::write(bin_dir.join("claude.cmd"), script).expect("write fake claude.cmd");
}

fn write_fake_codex(bin_dir: &Path, state_file: &Path, log_file: &Path) {
    let script = format!(
        "@echo off\r\n\
chcp 65001 >nul\r\n\
echo %*>>\"{log}\"\r\n\
if \"%1 %2\"==\"mcp get\" goto do_get\r\n\
if \"%1 %2\"==\"mcp add\" goto do_add\r\n\
if \"%1 %2\"==\"mcp remove\" goto do_remove\r\n\
exit /b 1\r\n\
:do_get\r\n\
if exist \"{state}\" (\r\n\
  type \"{state}\"\r\n\
  exit /b 0\r\n\
)\r\n\
echo Error: No MCP server named 'controlfreak' found. 1>&2\r\n\
exit /b 1\r\n\
:do_add\r\n\
set \"EXE=%~5\"\r\n\
set \"EXE=%EXE:\\=\\\\%\"\r\n\
echo {{\"name\":\"controlfreak\",\"enabled\":true,\"transport\":{{\"type\":\"stdio\",\"command\":\"%EXE%\",\"args\":[],\"env\":null,\"env_vars\":[],\"cwd\":null}}}}>\"{state}\"\r\n\
exit /b 0\r\n\
:do_remove\r\n\
if not exist \"{state}\" exit /b 1\r\n\
del /f /q \"{state}\"\r\n\
exit /b 0\r\n",
        log = log_file.display(),
        state = state_file.display()
    );
    fs::write(bin_dir.join("codex.cmd"), script).expect("write fake codex.cmd");
}

#[test]
fn probe_clients_reports_path_presence_without_invoking_clis() {
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let sentinel = root.path().join("invoked.txt");
    fs::write(
        bin.join("claude.cmd"),
        format!(
            "@echo off\r\necho ran>\"{}\"\r\nexit /b 0\r\n",
            sentinel.display()
        ),
    )
    .unwrap();

    let report = root.path().join("probe.ini");
    let out = run_helper(&bin, &["probe-clients", report.to_str().unwrap()]);
    assert!(out.status.success());
    assert!(
        !sentinel.exists(),
        "probe-clients must not execute client CLIs"
    );
    let text = read_utf16_ini(&report);
    if running_elevated() {
        assert!(text.contains("elevated=1"));
        assert!(text.contains("claude-code=CLI disabled while elevated"));
    } else {
        assert!(text.contains("elevated=0"));
        assert!(text.contains("claude-code=CLI found"));
        assert!(text.contains("claude-code-found=1"));
        assert!(text.contains("codex=CLI not found on PATH"));
        assert!(text.contains("codex-found=0"));
    }
}

#[test]
fn elevated_process_refuses_cli_registration_without_explicit_opt_in() {
    if !running_elevated() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("bin");
    let state = root.path().join("installer-state");
    fs::create_dir_all(&bin).unwrap();
    let invoked = root.path().join("elevated-invoked.txt");
    fs::write(
        bin.join("claude.cmd"),
        format!(
            "@echo off\r\necho ran>\"{}\"\r\nexit /b 0\r\n",
            invoked.display()
        ),
    )
    .unwrap();
    let report = root.path().join("elevated.ini");
    let exe = root.path().join("controlfreak.exe");
    let out = run_helper_raw(
        &bin,
        &[
            "register-cli",
            "claude-code",
            exe.to_str().unwrap(),
            state.to_str().unwrap(),
            report.to_str().unwrap(),
        ],
        &[],
    );
    assert!(!out.status.success());
    assert!(!invoked.exists());
    let text = fs::read_to_string(&report).unwrap();
    assert!(text.contains("Administrator"));
}

#[test]
fn cli_resolution_ignores_relative_and_empty_path_entries() {
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("rel-bin");
    let state = root.path().join("installer-state");
    fs::create_dir_all(&bin).unwrap();
    let invoked = root.path().join("rel-invoked.txt");
    fs::write(
        bin.join("claude.cmd"),
        format!(
            "@echo off\r\necho ran>\"{}\"\r\nexit /b 0\r\n",
            invoked.display()
        ),
    )
    .unwrap();

    let sys_root = env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    let sys_root = PathBuf::from(sys_root);
    let path_with_rel = format!(
        ";rel-bin;.;{};{}",
        sys_root.join("System32").display(),
        sys_root.display()
    );
    let report = root.path().join("reg-rel.ini");
    let exe = root.path().join("controlfreak.exe");
    let mut cmd = Command::new(helper());
    cmd.current_dir(root.path()).arg("register-cli");
    if running_elevated() {
        cmd.arg("--allow-elevated");
    }
    let out = cmd
        .args([
            "claude-code",
            exe.to_str().unwrap(),
            state.to_str().unwrap(),
            report.to_str().unwrap(),
        ])
        .env("PATH", &path_with_rel)
        .output()
        .expect("run register-cli with relative PATH");
    assert!(!out.status.success());
    assert!(!invoked.exists());
    let text = fs::read_to_string(&report).unwrap();
    assert!(text.contains("claude CLI was not found on PATH."));

    if !running_elevated() {
        let probe_report = root.path().join("probe-rel.ini");
        let probe_out = Command::new(helper())
            .current_dir(root.path())
            .args(["probe-clients", probe_report.to_str().unwrap()])
            .env("PATH", &path_with_rel)
            .output()
            .expect("run probe-clients with relative PATH");
        assert!(probe_out.status.success());
        let probe_text = read_utf16_ini(&probe_report);
        assert!(probe_text.contains("claude-code=CLI not found on PATH"));
    }
}

#[test]
fn register_and_remove_cli_respects_existing_entries_profiles_and_customizations() {
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("bin");
    let state = root.path().join("installer-state");
    fs::create_dir_all(&bin).unwrap();
    let exe = root.path().join("ControlFreak").join("controlfreak.exe");

    let claude_entry = root.path().join("claude.entry");
    let claude_log = root.path().join("claude.log");
    let codex_entry = root.path().join("codex.entry");
    let codex_log = root.path().join("codex.log");
    write_fake_claude(&bin, &claude_entry, &claude_log);
    write_fake_codex(&bin, &codex_entry, &codex_log);

    // Register both freshly.
    let claude_report = root.path().join("claude-reg.ini");
    let out = run_helper(
        &bin,
        &[
            "register-cli",
            "claude-code",
            exe.to_str().unwrap(),
            state.to_str().unwrap(),
            claude_report.to_str().unwrap(),
        ],
    );
    assert!(out.status.success());
    assert!(claude_entry.is_file());
    assert!(state.join("claude-code.managed").is_file());

    let codex_report = root.path().join("codex-reg.ini");
    let out = run_helper(
        &bin,
        &[
            "register-cli",
            "codex",
            exe.to_str().unwrap(),
            state.to_str().unwrap(),
            codex_report.to_str().unwrap(),
        ],
    );
    assert!(out.status.success());
    assert!(codex_entry.is_file());
    assert!(state.join("codex.managed").is_file());

    // Changing CODEX_HOME / CLAUDE_CONFIG_DIR must prevent uninstall from removing entries in another profile.
    let wrong_profile_report = root.path().join("remove-wrong-profile.ini");
    let out = run_helper_with_env(
        &bin,
        &[
            "remove-cli",
            state.to_str().unwrap(),
            wrong_profile_report.to_str().unwrap(),
        ],
        &[
            ("CODEX_HOME", r"C:\OtherCodexProfile"),
            ("CLAUDE_CONFIG_DIR", r"C:\OtherClaudeProfile"),
        ],
    );
    assert!(out.status.success());
    assert!(claude_entry.is_file());
    assert!(codex_entry.is_file());
    assert!(
        fs::read_to_string(&wrong_profile_report)
            .unwrap()
            .contains("status=warning")
    );

    // Customizing Codex entry (e.g. adding --profile observe) must prevent uninstall from deleting it.
    let escaped_exe = exe.display().to_string().replace('\\', r"\\");
    fs::write(
        &codex_entry,
        format!(
            r#"{{"name":"controlfreak","enabled":true,"transport":{{"type":"stdio","command":"{escaped_exe}","args":["--profile","observe"],"env":null,"env_vars":[],"cwd":null}}}}"#
        ),
    )
    .unwrap();

    let remove_report = root.path().join("remove.ini");
    let out = run_helper(
        &bin,
        &[
            "remove-cli",
            state.to_str().unwrap(),
            remove_report.to_str().unwrap(),
        ],
    );
    assert!(out.status.success());
    assert!(
        !claude_entry.exists(),
        "matching Claude entry should be removed"
    );
    assert!(
        codex_entry.is_file(),
        "customized Codex entry must be retained"
    );
    let remove_text = fs::read_to_string(&remove_report).unwrap();
    assert!(remove_text.contains("status=warning"));
    assert!(remove_text.contains("codex"));
}

#[test]
fn registering_second_profile_retains_earlier_profile_for_uninstall_warning() {
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("bin");
    let state = root.path().join("installer-state");
    fs::create_dir_all(&bin).unwrap();
    let exe = root.path().join("ControlFreak").join("controlfreak.exe");

    let claude_entry_a = root.path().join("claude-a.entry");
    let claude_entry_b = root.path().join("claude-b.entry");
    let claude_log = root.path().join("claude.log");
    // Profile-aware fake claude shim storing separate state files per CLAUDE_CONFIG_DIR.
    fs::write(
        bin.join("claude.cmd"),
        format!(
            "@echo off\r\n\
echo %CLAUDE_CONFIG_DIR% %*>>\"{log}\"\r\n\
if \"%1 %2\"==\"mcp add\" goto do_add\r\n\
if \"%1 %2\"==\"mcp remove\" goto do_remove\r\n\
exit /b 1\r\n\
:do_add\r\n\
if \"%CLAUDE_CONFIG_DIR%\"==\"A\" echo reg>\"{a}\"\r\n\
if \"%CLAUDE_CONFIG_DIR%\"==\"B\" echo reg>\"{b}\"\r\n\
exit /b 0\r\n\
:do_remove\r\n\
if \"%CLAUDE_CONFIG_DIR%\"==\"B\" del /f /q \"{b}\"\r\n\
exit /b 0\r\n",
            log = claude_log.display(),
            a = claude_entry_a.display(),
            b = claude_entry_b.display()
        ),
    )
    .unwrap();

    let report_a = root.path().join("reg-a.ini");
    let out_a = run_helper_with_env(
        &bin,
        &[
            "register-cli",
            "claude-code",
            exe.to_str().unwrap(),
            state.to_str().unwrap(),
            report_a.to_str().unwrap(),
        ],
        &[("CLAUDE_CONFIG_DIR", "A")],
    );
    assert!(out_a.status.success());

    let report_b = root.path().join("reg-b.ini");
    let out_b = run_helper_with_env(
        &bin,
        &[
            "register-cli",
            "claude-code",
            exe.to_str().unwrap(),
            state.to_str().unwrap(),
            report_b.to_str().unwrap(),
        ],
        &[("CLAUDE_CONFIG_DIR", "B")],
    );
    assert!(out_b.status.success());

    // Uninstalling under profile B removes B, keeps profile A's marker record, and warns about it.
    let remove_report = root.path().join("remove-b.ini");
    let out_rm = run_helper_with_env(
        &bin,
        &[
            "remove-cli",
            state.to_str().unwrap(),
            remove_report.to_str().unwrap(),
        ],
        &[("CLAUDE_CONFIG_DIR", "B")],
    );
    assert!(out_rm.status.success());
    assert!(!claude_entry_b.exists());
    assert!(claude_entry_a.is_file());
    assert!(state.join("claude-code.managed").is_file());
    let text = fs::read_to_string(&remove_report).unwrap();
    assert!(text.contains("status=warning"));
    assert!(text.contains("claude-code"));
}

#[test]
fn failed_codex_lookup_never_overwrites_existing_entry() {
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("bin");
    let state = root.path().join("installer-state");
    fs::create_dir_all(&bin).unwrap();
    let exe = root.path().join("ControlFreak").join("controlfreak.exe");
    let add_called = root.path().join("add-called.txt");

    // Simulate a broken/corrupt config error on `codex mcp get` (NOT "No MCP server named").
    fs::write(
        bin.join("codex.cmd"),
        format!(
            "@echo off\r\nif \"%1 %2\"==\"mcp get\" (\r\n  echo Error: failed to parse config.toml 1>&2\r\n  exit /b 1\r\n)\r\necho added>\"{}\"\r\nexit /b 0\r\n",
            add_called.display()
        ),
    )
    .unwrap();

    let report = root.path().join("codex-err.ini");
    let out = run_helper(
        &bin,
        &[
            "register-cli",
            "codex",
            exe.to_str().unwrap(),
            state.to_str().unwrap(),
            report.to_str().unwrap(),
        ],
    );
    assert!(!out.status.success());
    assert!(
        !add_called.exists(),
        "codex mcp add must not run after an ambiguous mcp get failure"
    );
    let text = fs::read_to_string(&report).unwrap();
    assert!(text.contains("status=error"));
    assert!(!text.contains("config.toml"));
}

#[test]
fn register_cli_reports_missing_and_failing_clis_without_leaking_output() {
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("bin");
    let state = root.path().join("installer-state");
    fs::create_dir_all(&bin).unwrap();
    let exe = root.path().join("controlfreak.exe");

    let report = root.path().join("missing.ini");
    let out = run_helper(
        &bin,
        &[
            "register-cli",
            "claude-code",
            exe.to_str().unwrap(),
            state.to_str().unwrap(),
            report.to_str().unwrap(),
        ],
    );
    assert!(!out.status.success());
    let text = fs::read_to_string(&report).unwrap();
    assert!(text.contains("status=error"));
    assert!(text.contains("claude CLI was not found on PATH."));

    fs::write(
        bin.join("claude.cmd"),
        "@echo off\r\necho SECRET_TOKEN_12345 1>&2\r\nexit /b 1\r\n",
    )
    .unwrap();
    let fail_report = root.path().join("fail.ini");
    let out = run_helper(
        &bin,
        &[
            "register-cli",
            "claude-code",
            exe.to_str().unwrap(),
            state.to_str().unwrap(),
            fail_report.to_str().unwrap(),
        ],
    );
    assert!(!out.status.success());
    let text = fs::read_to_string(&fail_report).unwrap();
    assert!(text.contains("status=error"));
    assert!(!text.contains("SECRET_TOKEN_12345"));
    assert!(!String::from_utf8_lossy(&out.stderr).contains("SECRET_TOKEN_12345"));
}

#[test]
fn register_cli_handles_spaces_and_unicode_in_both_batch_shim_and_install_path() {
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("bin with spaces");
    let state = root.path().join("installer-state");
    fs::create_dir_all(&bin).unwrap();
    let exe = root
        .path()
        .join("Apps with spaces 日本語")
        .join("ControlFreak")
        .join("controlfreak.exe");

    let claude_entry = root.path().join("claude.entry");
    let claude_log = root.path().join("claude.log");
    write_fake_claude(&bin, &claude_entry, &claude_log);

    let report = root.path().join("spaced.ini");
    let out = run_helper(
        &bin,
        &[
            "register-cli",
            "claude-code",
            exe.to_str().unwrap(),
            state.to_str().unwrap(),
            report.to_str().unwrap(),
        ],
    );
    assert!(out.status.success());
    assert!(claude_entry.is_file());
    let logged = fs::read_to_string(&claude_log).unwrap();
    assert!(logged.contains("mcp add --scope user controlfreak --"));
    assert!(
        logged.contains(exe.to_str().unwrap()),
        "expected full Unicode path in log, got: {logged}"
    );
}

#[test]
fn register_cli_rejects_shell_metacharacters_for_batch_shims() {
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("bin");
    let state = root.path().join("installer-state");
    fs::create_dir_all(&bin).unwrap();
    let invoked = root.path().join("shim-invoked.txt");
    fs::write(
        bin.join("codex.cmd"),
        format!(
            "@echo off\r\necho invoked>\"{}\"\r\nexit /b 1\r\n",
            invoked.display()
        ),
    )
    .unwrap();

    let unsafe_exe = root.path().join("Apps&More").join("controlfreak.exe");
    let report = root.path().join("meta.ini");
    let out = run_helper(
        &bin,
        &[
            "register-cli",
            "codex",
            unsafe_exe.to_str().unwrap(),
            state.to_str().unwrap(),
            report.to_str().unwrap(),
        ],
    );
    assert!(!out.status.success());
    assert!(
        !invoked.exists(),
        "batch shim must never be executed when install path contains shell metacharacters"
    );
    let text = fs::read_to_string(&report).unwrap();
    assert!(text.contains("status=error"));
    assert!(text.contains("metacharacters"));
}

#[test]
fn remove_cli_reports_warning_for_missing_cli_and_legacy_receipts() {
    let root = tempfile::tempdir().unwrap();
    let empty_bin = root.path().join("empty-bin");
    let state = root.path().join("installer-state");
    fs::create_dir_all(&empty_bin).unwrap();
    fs::create_dir_all(&state).unwrap();
    let userprofile = env::var("USERPROFILE").unwrap_or_default();
    fs::write(
        state.join("codex.managed"),
        format!(
            "version=1\nexe=C:\\ControlFreak\\controlfreak.exe\nprofile_override=\nuserprofile={userprofile}\n"
        ),
    )
    .unwrap();
    fs::write(state.join("claude-desktop.json"), "[]").unwrap();

    let report = root.path().join("remove-warn.ini");
    let out = run_helper(
        &empty_bin,
        &[
            "remove-cli",
            state.to_str().unwrap(),
            report.to_str().unwrap(),
        ],
    );
    assert!(out.status.success());
    let text = fs::read_to_string(&report).unwrap();
    assert!(text.contains("status=warning"));
    assert!(text.contains("codex"));
    assert!(text.contains("claude-desktop"));
    assert!(text.contains(".controlfreak-backup-*.bak"));
}

#[test]
fn check_version_and_blockers_modes_work() {
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();

    assert!(
        run_helper(&bin, &["check-version", "0.0.1"])
            .status
            .success()
    );
    assert!(
        run_helper(&bin, &["check-version", env!("CARGO_PKG_VERSION")])
            .status
            .success()
    );
    assert!(
        !run_helper(&bin, &["check-version", "999.0.0"])
            .status
            .success()
    );
    assert!(
        !run_helper(&bin, &["check-version", "not-a-version"])
            .status
            .success()
    );

    let install_dir = root.path().join("install");
    fs::create_dir_all(&install_dir).unwrap();
    let install_report = root.path().join("blockers-install.ini");
    let uninstall_report = root.path().join("blockers-uninstall.ini");
    assert!(
        run_helper(
            &bin,
            &[
                "blockers",
                install_dir.to_str().unwrap(),
                install_report.to_str().unwrap(),
                "install",
            ],
        )
        .status
        .success()
    );
    assert!(
        run_helper(
            &bin,
            &[
                "blockers",
                install_dir.to_str().unwrap(),
                uninstall_report.to_str().unwrap(),
                "uninstall",
            ],
        )
        .status
        .success()
    );
    assert!(read_utf16_ini(&install_report).contains("Setup cannot replace"));
    assert!(read_utf16_ini(&uninstall_report).contains("Uninstall cannot remove"));
}
