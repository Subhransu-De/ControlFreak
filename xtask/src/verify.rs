use crate::{Result, package, process};
use serde_json::Value;
use std::{
    fs::{self, File},
    path::Path,
    process::Command,
    time::Duration,
};

pub fn package(
    directory: &Path,
    output: &Path,
    version: &str,
    expected_revision: Option<&str>,
) -> Result<()> {
    for name in [
        package::archive_name(version),
        package::installer_name(version),
    ] {
        let expected = fs::read_to_string(directory.join(format!("{name}.sha256")))?;
        if expected.trim() != format!("{}  {name}", package::hash(&directory.join(&name))?) {
            return Err("Release artifact checksum mismatch".into());
        }
    }
    let output = package::fresh(output)?;
    extract(&directory.join(package::archive_name(version)), &output)?;
    for name in package::BINARIES
        .into_iter()
        .chain(package::DOCUMENTS)
        .chain(["version.txt"])
    {
        if !output.join(name).is_file() {
            return Err("Required payload file missing".into());
        }
    }
    if fs::read_to_string(output.join("version.txt"))?.trim() != version {
        return Err("Package version mismatch".into());
    }
    server(&output.join("controlfreak.exe"), version, expected_revision)
}

pub fn extract(archive: &Path, destination: &Path) -> Result<()> {
    zip::ZipArchive::new(File::open(archive)?)?.extract(destination)?;
    Ok(())
}

fn invoke(executable: &Path, arguments: &[&str], input: &str) -> Result<process::Output> {
    process::run(
        Command::new(executable)
            .args(arguments)
            .current_dir(executable.parent().ok_or("Missing executable directory")?)
            .env("PATH", "")
            .env_remove("CONTROLFREAK_GLOW_ERROR_LOG"),
        input,
        Duration::from_secs(20),
    )
}

pub fn server(executable: &Path, version: &str, expected_revision: Option<&str>) -> Result<()> {
    let executable = std::path::absolute(executable)?;
    let manifest = controlfreak_platform::packaging_tests::embedded_manifest(&executable)?
        .ok_or("Packaged executable has no application manifest")?;
    if !declares_per_monitor_v2(&manifest) {
        return Err(
            "Packaged executable manifest does not declare PerMonitorV2 DPI awareness".into(),
        );
    }
    let result = invoke(&executable, &["--version"], "")?;
    if !result.status.success()
        || result.stdout.lines().next() != Some(&format!("controlfreak {version}"))
    {
        return Err("Packaged executable version mismatch".into());
    }
    let version_text = result.stdout;
    let result = invoke(&executable, &["--print-capabilities"], "")?;
    if !result.status.success() {
        return Err("Packaged capabilities command failed".into());
    }
    let capabilities: Value =
        serde_json::from_str(&result.stdout).map_err(|_| "Invalid capabilities JSON")?;
    let build = &capabilities["build"];
    check_build(build, version, &version_text, expected_revision)?;
    let elevated = capabilities["security_context"]["elevated"]
        .as_bool()
        .ok_or("Missing security context")?;
    let mut args = Vec::new();
    if elevated {
        let refusal = invoke(&executable, &[], "")?;
        let failure: Value =
            serde_json::from_str(&refusal.stderr).map_err(|_| "Invalid startup refusal JSON")?;
        if refusal.status.success()
            || !refusal.stdout.is_empty()
            || failure["error"]["code"] != "elevated_operation_requires_opt_in"
        {
            return Err("Elevated startup was not refused cleanly".into());
        }
        args.push("--allow-elevated");
    }
    // Harmless observations only. No desktop capture or mutation; never print responses.
    let requests = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{},\"clientInfo\":{\"name\":\"synthetic-package-check\",\"version\":\"1.0.0\"}}}\n",
        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"get_server_status\",\"arguments\":{}}}\n"
    );
    let result = invoke(&executable, &args, requests)?;
    if !result.status.success() {
        return Err("Packaged MCP startup or disconnect failed".into());
    }
    validate_responses(&result.stdout, version, build)?;
    println!(
        "Packaged executable: DPI manifest, version, build identity, capabilities, MCP discovery/status and disconnect passed."
    );
    Ok(())
}

/// The build object of `--print-capabilities` must have the documented format and agree with
/// `--version`. With an expected revision, as in a release, it must name that commit with a clean
/// tree: a dirty marker of `true` or `null` fails.
fn check_build(
    build: &Value,
    version: &str,
    version_text: &str,
    expected_revision: Option<&str>,
) -> Result<()> {
    let info: controlfreak_core::BuildInfo =
        serde_json::from_value(build.clone()).map_err(|_| "Missing or invalid build identity")?;
    if !info.is_well_formed() || info.version != version {
        return Err("Packaged build identity is malformed or names another version".into());
    }
    let dirty = info
        .dirty
        .map_or("unknown", |dirty| if dirty { "true" } else { "false" });
    let expected_text = format!(
        "controlfreak {version}\nrevision: {}\ndirty: {dirty}\ntarget: {}",
        info.revision, info.target
    );
    if version_text.trim_end() != expected_text {
        return Err("--version and --print-capabilities report different builds".into());
    }
    if let Some(expected) = expected_revision {
        if info.revision != expected.trim().to_ascii_lowercase() {
            return Err(format!(
                "Packaged executable reports revision {}, expected {expected}",
                info.revision
            )
            .into());
        }
        if info.dirty != Some(false) {
            return Err(format!(
                "Packaged executable reports dirty: {dirty}; release builds need a clean checkout"
            )
            .into());
        }
    }
    Ok(())
}

fn declares_per_monitor_v2(manifest: &str) -> bool {
    manifest.contains(">PerMonitorV2</dpiAwareness>") && manifest.contains(">true/pm</dpiAware>")
}

fn validate_responses(output: &str, version: &str, build: &Value) -> Result<()> {
    let responses = output
        .lines()
        .filter(|line| !line.is_empty())
        .map(serde_json::from_str::<Value>)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| "Non-protocol stdout")?;
    if responses.iter().any(|r| r["jsonrpc"] != "2.0") {
        return Err("Non-protocol stdout".into());
    }
    let response = |id| -> Result<&Value> {
        let matching: Vec<_> = responses.iter().filter(|r| r["id"] == id).collect();
        if matching.len() != 1 {
            return Err("Missing or duplicate MCP response".into());
        }
        Ok(matching[0])
    };
    let initialize = response(1)?;
    if initialize["result"]["serverInfo"]["version"] != version
        || initialize["result"]["serverInfo"]["name"] != "controlfreak"
    {
        return Err("MCP build identity mismatch".into());
    }
    let tools = response(2)?["result"]["tools"]
        .as_array()
        .ok_or("Missing tool list")?;
    if !tools.iter().any(|tool| tool["name"] == "get_server_status") {
        return Err("MCP tool discovery failed".into());
    }
    let status = response(3)?;
    if status["result"]["isError"] == true
        || status["result"]["structuredContent"]["status"] != "ready"
    {
        return Err("Packaged server did not report ready".into());
    }
    if status["result"]["structuredContent"]["dpi_awareness"]["awareness"] != "per_monitor_v2" {
        return Err("Packaged server is not per-monitor v2 DPI aware".into());
    }
    if status["result"]["structuredContent"]["build"] != *build {
        return Err("get_server_status and --print-capabilities report different builds".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn protocol_noise_and_missing_responses_fail() {
        assert!(validate_responses("debug output\n", "1.0.0", &Value::Null).is_err());
    }

    #[test]
    fn only_a_per_monitor_v2_manifest_passes() {
        let manifest =
            include_str!("../../crates/controlfreak-server/windows/controlfreak.exe.manifest");
        assert!(declares_per_monitor_v2(manifest));
        assert!(!declares_per_monitor_v2(
            &manifest.replace("PerMonitorV2", "PerMonitor")
        ));
        assert!(!declares_per_monitor_v2(
            &manifest.replace("true/pm", "true")
        ));
        assert!(
            validate_responses("{\"jsonrpc\":\"2.0\",\"id\":1}\n", "1.0.0", &Value::Null).is_err()
        );
    }

    const COMMIT: &str = "aa56f634712aaca50879d5ca0aeb3675ada9a773";

    fn build(revision: &str, dirty: Option<bool>) -> Value {
        serde_json::json!({
            "schema_version": 1,
            "version": "1.0.0",
            "revision": revision,
            "dirty": dirty,
            "target": "x86_64-pc-windows-msvc",
        })
    }

    fn version_text(revision: &str, dirty: &str) -> String {
        format!(
            "controlfreak 1.0.0
revision: {revision}
dirty: {dirty}
target: x86_64-pc-windows-msvc
"
        )
    }

    #[test]
    fn a_clean_build_of_the_released_commit_passes() {
        let clean = build(COMMIT, Some(false));
        let text = version_text(COMMIT, "false");
        assert!(check_build(&clean, "1.0.0", &text, Some(COMMIT)).is_ok());
        assert!(check_build(&clean, "1.0.0", &text, Some(&COMMIT.to_uppercase())).is_ok());
        assert!(check_build(&clean, "1.0.0", &text, None).is_ok());
    }

    #[test]
    fn a_release_fails_on_a_dirty_unknown_or_other_build() {
        let dirty = build(COMMIT, Some(true));
        let text = version_text(COMMIT, "true");
        assert!(check_build(&dirty, "1.0.0", &text, None).is_ok());
        assert!(check_build(&dirty, "1.0.0", &text, Some(COMMIT)).is_err());
        let other = "b".repeat(40);
        let clean = build(COMMIT, Some(false));
        assert!(
            check_build(
                &clean,
                "1.0.0",
                &version_text(COMMIT, "false"),
                Some(&other)
            )
            .is_err()
        );
        let archive = build("unknown", None);
        let text = version_text("unknown", "unknown");
        assert!(check_build(&archive, "1.0.0", &text, None).is_ok());
        assert!(check_build(&archive, "1.0.0", &text, Some(COMMIT)).is_err());
    }

    #[test]
    fn every_surface_must_report_the_same_well_formed_build() {
        let clean = build(COMMIT, Some(false));
        let text = version_text(COMMIT, "false");
        assert!(check_build(&Value::Null, "1.0.0", &text, None).is_err());
        assert!(check_build(&clean, "2.0.0", &text, None).is_err());
        assert!(check_build(&clean, "1.0.0", &version_text(COMMIT, "true"), None).is_err());
        assert!(
            check_build(
                &clean,
                "1.0.0",
                "controlfreak 1.0.0
",
                None
            )
            .is_err()
        );
        let path = build(r"C:\Users\someone", None);
        assert!(
            check_build(
                &path,
                "1.0.0",
                &version_text(r"C:\Users\someone", "unknown"),
                None
            )
            .is_err()
        );
    }
}
