# Install and configure

## Requirements

- Windows 10 version 2004 (build 19041) or later, or Windows 11, on x86-64.
- A Windows OCR language pack for the text tools.

## Installer

Download `ControlFreak-<version>.exe` from [GitHub Releases](https://github.com/Subhransu-De/ControlFreak/releases) and run it as your normal user.

- It installs to `%LOCALAPPDATA%\Programs\ControlFreak` for the current user. No administrator rights.
- On the optional **Connect MCP clients** step, it shows the exact CLI registration command and an individual **Copy** button for **Claude Code** and **Codex**. Check a client to let setup run its command after installing files, or leave it unchecked to copy and run it yourself. It does not install the client.
- Restart configured clients afterwards.
- The finish page and `setup-results.txt` in the install folder show the result for each client.

### What setup changes

| Client | Registration command (PowerShell syntax) | Removal command on uninstall |
| --- | --- | --- |
| Claude Code | `claude mcp add --scope user controlfreak -- '<install-dir>\controlfreak.exe'` | `claude mcp remove --scope user controlfreak` |
| Codex | `codex mcp add controlfreak -- '<install-dir>\controlfreak.exe'` | `codex mcp remove controlfreak` |

- Setup never reads, parses, edits or backs up third-party client configuration files directly.
- Existing `controlfreak` entries are kept untouched: for Codex, setup checks `codex mcp get --json controlfreak` outside any project directory and only calls `codex mcp add` on the exact not-found response (Codex has no atomic add-if-absent command); for Claude Code, an existing entry is preserved. Note that if you copy and run `codex mcp add controlfreak -- ...` manually in PowerShell, Codex replaces any existing `controlfreak` entry. If your install path contains `cmd.exe` metacharacters (`&|<>^%!"`) and your client CLI is a `.cmd`/`.bat` shim, install to a path without those characters or use the native `.exe` CLI.
- For each client that setup registers, it writes a marker file (`claude-code.managed` or `codex.managed`) under `installer-state` in the install folder recording the executable path and active profile environment (`USERPROFILE` plus `CLAUDE_CONFIG_DIR` or `CODEX_HOME`).
- Automatic CLI registration and removal are disabled by default when setup runs as Administrator (and always disabled in interactive setup); copy and run the PowerShell commands from a normal user terminal instead. Silent automation on elevated runners can explicitly opt in with `/ALLOWELEVATEDCLI=1`.
- Project or managed client configuration can override user-scoped entries.
- Setup does not change `PATH`, start the server, add a service, enable `--allow-elevated` or install an updater.
- For Claude Desktop, OpenCode and Pi, use the manual setup instructions below.

### Upgrade

- Close the MCP clients that use ControlFreak, then run the new installer.
- If a file is locked, setup shows a parent-child process tree marking the exact `holds file` PIDs separately from their parent app. Close the parent app or end only the `holds file` PIDs and select **Try Again**.
- Setup refuses downgrades and reports the existing folder if you select a different directory.
- If you are upgrading from v0.3.0 or earlier, client entries written by the earlier direct-edit installer (including Claude Desktop, OpenCode and Pi) and any `*.controlfreak-backup-*.bak` files are left untouched.
- Restart running clients to load the new version.

### Uninstall

- Uninstall from Windows Settings, or run `unins000.exe` in the install folder.
- If files are in use, uninstall shows the same process tree and stops without killing any process.
- Uninstall offers to remove the `controlfreak` MCP entries that setup registered, only when the active profile environment matches the recorded marker. For Codex, setup verifies via `codex mcp get --json controlfreak` that the entry still points to the installed executable with no custom flags before calling `codex mcp remove controlfreak`. Because `claude mcp get` connects to the server rather than reading offline config, confirming uninstall runs `claude mcp remove --scope user controlfreak` in the matching profile without pre-connecting.
- Entries that existed before setup, customized Codex entries, and entries written by v0.3.0 or earlier (along with any old `*.controlfreak-backup-*.bak` files, which may contain credentials) are left untouched and must be removed manually.

### Silent install

```powershell
ControlFreak-<version>.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /CLIENTS=claude-code,codex
```

- `/DIR="C:\Tools\ControlFreak"` sets the install folder.
- Supported `/CLIENTS` values are `claude-code` and `codex`. Without `/CLIENTS`, no client is registered.
- Exit code `10` means ControlFreak installed but at least one CLI command failed (for example, the CLI was not on `PATH`). See `setup-results.txt`.
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
