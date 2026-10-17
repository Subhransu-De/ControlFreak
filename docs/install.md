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

## Build from source

Needs Rust 1.97 or later with the `x86_64-pc-windows-msvc` toolchain and the Microsoft C++ Build
Tools.

```powershell
cargo install --path crates/controlfreak-server --locked
```

This installs `controlfreak.exe` to `%USERPROFILE%\.cargo\bin`.
