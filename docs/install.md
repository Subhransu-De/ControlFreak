# Install and configure

## Requirements

- Windows 10 version 2004 (build 19041) or later, or Windows 11, on x86-64.
- A Windows OCR language pack for the text tools.

## Installer

Download `ControlFreak-<version>.exe` from
[GitHub Releases](https://github.com/Subhransu-De/ControlFreak/releases) and run it as your
normal user.

- It installs to `%LOCALAPPDATA%\Programs\ControlFreak` for the current user. No administrator
  rights.
- It offers a checkbox for each supported MCP client. For each one you select, it adds a
  `controlfreak` entry to that client's configuration. It does not install the client.
- Restart the selected clients afterwards.
- The finish page and `setup-results.txt` in the install folder show the result for each client.

### What setup changes

| Client | File | Override |
| --- | --- | --- |
| Codex | `%USERPROFILE%\.codex\config.toml` | `CODEX_HOME` |
| Claude Code | `%USERPROFILE%\.claude.json` | `CLAUDE_CONFIG_DIR` |
| Claude Desktop | `%APPDATA%\Claude\claude_desktop_config.json` | `APPDATA` |
| OpenCode | `%USERPROFILE%\.config\opencode\opencode.json` (or `.jsonc`) | `XDG_CONFIG_HOME`, `OPENCODE_CONFIG_DIR`, `OPENCODE_CONFIG` |
| Pi | `%USERPROFILE%\.pi\agent\mcp.json` | `PI_CODING_AGENT_DIR` |

- Other servers, settings and comments in these files are kept.
- Before it changes a file, setup saves a backup next to it named
  `<file>.controlfreak-backup-*.bak`. Backups can contain credentials from the original file.
  Keep them private.
- Files that are malformed, encrypted (EFS) or changed during setup are left alone and reported.
- Pi also needs `pi install npm:pi-mcp-adapter`. Setup does not install it.
- Project or managed client configuration can override these user files.
- Setup does not change `PATH`, start the server, add a service, enable `--allow-elevated` or
  install an updater.

### Upgrade

- Close the MCP clients that use ControlFreak, then run the new installer.
- If a file is locked, setup lists the processes that hold it. Close them and select Try Again.
- Setup refuses downgrades.
- Restart running clients to load the new version.

### Uninstall

- Uninstall from Windows Settings, or run `unins000.exe` in the install folder.
- Uninstall offers to remove the `controlfreak` entries that setup added. It removes an entry only
  if it is unchanged.
- Other entries and the backups are kept. To restore an earlier configuration, copy its backup
  back.

### Silent install

```powershell
ControlFreak-<version>.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /CLIENTS=codex,claude-code,claude-desktop,pi,opencode
```

- `/DIR="C:\Tools\ControlFreak"` sets the install folder.
- Without `/CLIENTS`, no client is configured.
- Exit code `10` means ControlFreak installed but at least one client failed. See
  `setup-results.txt`.
- Silent uninstall keeps client entries unless you add `/REMOVECONFIG=1`.

## Portable ZIP

Extract `controlfreak-v<version>-x86_64-pc-windows-msvc.zip` to a folder that will not move, then
add `controlfreak.exe` to your client yourself. `controlfreak-installer.exe` is the setup helper,
not the MCP server.

## Verify a download

Every release file has a `.sha256` checksum. From 0.1.0, the installer, the ZIP and both
executables inside the ZIP also have GitHub build provenance attestations:

```powershell
gh attestation verify ControlFreak-<version>.exe --repo Subhransu-De/ControlFreak
```

`controlfreak --version` prints the version, then the Git commit it was built from, whether the source had uncommitted changes (`dirty`) and the target. A release reports its tagged commit and `dirty: false`.

The binaries are not code-signed, so Windows SmartScreen can show a warning.

## Configure a client by hand

Use the full path to `controlfreak.exe`. The installer path is
`C:\Users\<username>\AppData\Local\Programs\ControlFreak\controlfreak.exe`.

### Codex

`%USERPROFILE%\.codex\config.toml`

```toml
[mcp_servers.controlfreak]
command = 'C:\Users\<username>\AppData\Local\Programs\ControlFreak\controlfreak.exe'
```

### Claude Code

```powershell
claude mcp add --scope user controlfreak -- 'C:\Users\<username>\AppData\Local\Programs\ControlFreak\controlfreak.exe'
```

### Claude Desktop

`%APPDATA%\Claude\claude_desktop_config.json`

```json
{
  "mcpServers": {
    "controlfreak": {
      "command": "C:\\Users\\<username>\\AppData\\Local\\Programs\\ControlFreak\\controlfreak.exe",
      "args": []
    }
  }
}
```

### OpenCode

`%USERPROFILE%\.config\opencode\opencode.json`

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "controlfreak": {
      "type": "local",
      "command": ["C:\\Users\\<username>\\AppData\\Local\\Programs\\ControlFreak\\controlfreak.exe"],
      "enabled": true
    }
  }
}
```

### Pi

Run `pi install npm:pi-mcp-adapter`, then edit `%USERPROFILE%\.pi\agent\mcp.json`

```json
{
  "mcpServers": {
    "controlfreak": {
      "command": "C:\\Users\\<username>\\AppData\\Local\\Programs\\ControlFreak\\controlfreak.exe"
    }
  }
}
```

## Restrict the tools

Two server flags narrow what an agent can call. Without either flag, the server serves every tool, as before.

- `--profile observe` serves only tools that read the desktop. It hides `begin_control_session`, `focus_window`, `switch_virtual_desktop` and every input tool.
- `--tools a,b,c` serves only the named tools. Names are case-sensitive and come from [the tool list](tools.md#tool-list) or `controlfreak --print-tools`.
- Both flags together serve the listed tools, and every listed tool must be one that `--profile observe` serves.
- `get_server_status`, `end_control_session` and `stop_desktop_work` are served in every profile, even if `--tools` leaves them out.
- A hidden tool is refused like a tool that does not exist. `run_actions` refuses a whole batch, before any step runs, if a step needs a hidden tool: a click step needs `click_mouse`, a scroll step `scroll_mouse`, a type step `type_text` and a key step `press_keys`.
- A typo, an empty entry or a repeated name in `--tools`, an unknown profile, or `--tools` naming an input tool with `--profile observe` stops the server at startup with an error on stderr. It never falls back to serving every tool. The client then shows the server as failed; its MCP log has the reason.
- `get_server_status` reports the active profile in `profile`.

Setup never adds these flags. Add them to the client entry by hand, after the executable path. In the examples below, replace `<path>` with the folder of `controlfreak.exe`.

Codex, in `%USERPROFILE%\.codex\config.toml`:

```toml
[mcp_servers.controlfreak]
command = '<path>\controlfreak.exe'
args = ["--profile", "observe"]
```

To serve a chosen set of tools instead:

```toml
args = ["--tools", "list_windows,capture_window,read_text_in_region"]
```

Claude Code. Remove the existing entry first, then add it again with the flag after the path:

```powershell
claude mcp remove controlfreak --scope user
claude mcp add --scope user controlfreak -- '<path>\controlfreak.exe' --profile observe
```

Claude Desktop and Pi take the flags in `args`:

```json
"args": ["--profile", "observe"]
```

OpenCode takes them in the `command` array:

```json
"command": ["<path>\\controlfreak.exe", "--profile", "observe"]
```

You can register two entries side by side, such as `controlfreak-observe` with `--profile observe` and `controlfreak` with no flags, and enable the full one only for tasks that need input. A profile narrows the MCP tool surface only. It is not a sandbox: see [SECURITY.md](../SECURITY.md#tool-profiles).

## Build from source

Needs Rust 1.97 or later with the `x86_64-pc-windows-msvc` toolchain and the Microsoft C++ Build
Tools.

```powershell
cargo install --path crates/controlfreak-server --locked
```

This installs `controlfreak.exe` to `%USERPROFILE%\.cargo\bin`.
