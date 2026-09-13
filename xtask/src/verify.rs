use crate::{Result, package, process};
use serde_json::Value;
use std::{
    fs::{self, File},
    path::Path,
    process::Command,
    time::Duration,
};

pub fn package(directory: &Path, output: &Path, version: &str) -> Result<()> {
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
    server(&output.join("controlfreak.exe"), version)
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

pub fn server(executable: &Path, version: &str) -> Result<()> {
    let executable = std::path::absolute(executable)?;
    let result = invoke(&executable, &["--version"], "")?;
    if !result.status.success() || result.stdout.trim() != format!("controlfreak {version}") {
        return Err("Packaged executable version mismatch".into());
    }
    let result = invoke(&executable, &["--print-capabilities"], "")?;
    if !result.status.success() {
        return Err("Packaged capabilities command failed".into());
    }
    let capabilities: Value =
        serde_json::from_str(&result.stdout).map_err(|_| "Invalid capabilities JSON")?;
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
    validate_responses(&result.stdout, version)?;
    println!(
        "Packaged executable: version, capabilities, MCP discovery/status and disconnect passed."
    );
    Ok(())
}

fn validate_responses(output: &str, version: &str) -> Result<()> {
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn protocol_noise_and_missing_responses_fail() {
        assert!(validate_responses("debug output\n", "1.0.0").is_err());
        assert!(validate_responses("{\"jsonrpc\":\"2.0\",\"id\":1}\n", "1.0.0").is_err());
    }
}
