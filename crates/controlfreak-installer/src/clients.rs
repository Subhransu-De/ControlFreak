use crate::config::Result;
use std::{
    env,
    path::{Path, PathBuf},
};

pub const CLIENTS: [&str; 5] = ["codex", "claude-code", "claude-desktop", "pi", "opencode"];

pub struct Client {
    pub id: String,
    pub path: PathBuf,
    pub detected: bool,
    pub note: &'static str,
}

fn directory(name: &str, fallback: PathBuf) -> Result<PathBuf> {
    let value = env::var_os(name).map_or(fallback, PathBuf::from);
    if !value.is_absolute() {
        return Err("A client directory override is relative; use an absolute path.");
    }
    Ok(value)
}

fn on_path(name: &str) -> bool {
    env::var_os("PATH").is_some_and(|paths| {
        env::split_paths(&paths).any(|dir| {
            ["exe", "cmd", "bat"]
                .iter()
                .any(|ext| dir.join(format!("{name}.{ext}")).is_file())
        })
    })
}

pub fn resolve(id: &str) -> Result<Client> {
    let home = directory("USERPROFILE", PathBuf::new())?;
    let (path, detected, note) = match id {
        "codex" => {
            let dir = directory("CODEX_HOME", home.join(".codex"))?;
            let detected = dir.exists() || on_path("codex");
            (
                dir.join("config.toml"),
                detected,
                "Restart Codex after installation.",
            )
        }
        "claude-code" => {
            let path = if env::var_os("CLAUDE_CONFIG_DIR").is_some() {
                directory("CLAUDE_CONFIG_DIR", home.join(".claude"))?.join(".claude.json")
            } else {
                home.join(".claude.json")
            };
            let detected = path.exists() || on_path("claude");
            (
                path,
                detected,
                "Restart Claude Code; project overrides may take precedence.",
            )
        }
        "claude-desktop" => {
            let dir = directory("APPDATA", home.join("AppData/Roaming"))?.join("Claude");
            let detected = dir.exists();
            (
                dir.join("claude_desktop_config.json"),
                detected,
                "Quit and reopen Claude Desktop.",
            )
        }
        "pi" => {
            let dir = directory("PI_CODING_AGENT_DIR", home.join(".pi/agent"))?;
            let detected = dir.exists() || on_path("pi");
            (
                dir.join("mcp.json"),
                detected,
                "Requires pi-mcp-adapter: run pi install npm:pi-mcp-adapter, then restart Pi. Setup does not install the adapter.",
            )
        }
        "opencode" => {
            // Explicit custom configuration files win. Directory overrides may contain
            // higher-precedence config; don't silently edit a lower-precedence file.
            let base = directory("XDG_CONFIG_HOME", home.join(".config"))?.join("opencode");
            let dir = directory("OPENCODE_CONFIG_DIR", base)?;
            let path = if env::var_os("OPENCODE_CONFIG").is_some() {
                directory("OPENCODE_CONFIG", dir.join("opencode.json"))?
            } else {
                let json = dir.join("opencode.json");
                let jsonc = dir.join("opencode.jsonc");
                if json.exists() && jsonc.exists() {
                    return Err(
                        "Both OpenCode JSON and JSONC configurations exist; configure ControlFreak manually.",
                    );
                }
                if jsonc.exists() { jsonc } else { json }
            };
            let detected = path.exists() || on_path("opencode");
            (
                path,
                detected,
                "Restart OpenCode; project or managed configuration may override this entry.",
            )
        }
        _ => return Err("Unknown MCP client."),
    };
    Ok(Client {
        id: id.to_owned(),
        path,
        detected,
        note,
    })
}

pub fn group(id: &str) -> &'static str {
    match id {
        "codex" => "mcp_servers",
        "opencode" => "mcp",
        _ => "mcpServers",
    }
}

pub fn executable(path: &Path) -> Result<String> {
    if !path.is_absolute() {
        return Err("The installed executable path must be absolute.");
    }
    let value = path
        .to_str()
        .ok_or("The executable path must be Unicode.")?;
    // Claude/OpenCode expand these sequences even inside JSON strings.
    if value.contains("${") || value.contains("{env:") || value.contains("{file:") {
        return Err("Choose an installation path without client interpolation expressions.");
    }
    Ok(value.to_owned())
}
