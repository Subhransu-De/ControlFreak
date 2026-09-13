# ControlFreak

ControlFreak is a native Windows computer-use MCP server. It runs locally and communicates with
MCP clients over STDIO.

## Capabilities

- Capture displays, regions, and windows.
- Move, click, drag, and scroll the mouse across multiple displays.
- Send keyboard shortcuts and type Unicode text without using the clipboard.
- List, focus, capture, and wait for application windows.
- Read and click on-screen text using the local Windows OCR engine.
- Inspect and switch between discoverable Windows virtual desktops.
- Show a click-through animated desktop-edge glow while a client owns a mutating control session.

## Install on Windows

Download `ControlFreak-<version>-Setup.exe` from [GitHub Releases](https://github.com/Subhransu-De/ControlFreak/releases).
The installer supports Windows 10 version 2004 (build 19041) or newer and Windows 11 on x86-64.
Rust and Microsoft C++ Build Tools are not needed for packaged releases. OCR requires an installed
Windows OCR language pack. The setup executable and portable binaries are currently **unsigned**.

Setup installs for the current user and does not request administrator privileges. Run it from a
normal, non-elevated session. The wizard asks for an installation directory, then offers checkboxes
for Codex, Claude Code, Claude Desktop, Pi, and OpenCode before installing. The default directory is
`%LOCALAPPDATA%\Programs\ControlFreak`. Selecting a client configures its MCP connection; it does not
install that client. Selecting no clients installs only ControlFreak.

Pi additionally requires `pi install npm:pi-mcp-adapter`; setup does not download or install the
adapter. Client detection uses existing configuration files/directories and commands on PATH as
hints, not proof that a client or adapter is installed. Quit and reopen selected clients afterward.
Project-specific or managed client configuration can take precedence over these user settings.

Setup preserves other settings and servers, including TOML and JSONC comments. Existing ControlFreak
entries are kept unless **Replace existing ControlFreak entries** is selected. Before changing an
existing file, setup saves a uniquely named `<filename>.controlfreak-backup-*.bak` beside it. Treat
these backups as private: they can contain credentials from the original configuration. Malformed,
duplicate-key, unsupported, or concurrently changed configurations are left for manual recovery.
Existing files are updated through an exclusive Windows handle after their backup is flushed.
This blocks competing writes and renames during the update. A write failure attempts rollback;
an interrupted process or power loss can require restoring the backup manually.
The finish page and `setup-results.txt` in the install directory report each client's result.
If a client fails to configure, the application remains installed; fix the reported problem and
rerun setup, or configure the client manually using the executable's full path.

| Client | Default file edited by setup | Supported environment override |
| --- | --- | --- |
| Codex | `%USERPROFILE%\.codex\config.toml` | `CODEX_HOME` |
| Claude Code | `%USERPROFILE%\.claude.json` | `CLAUDE_CONFIG_DIR` (uses `.claude.json` inside it) |
| Claude Desktop | `%APPDATA%\Claude\claude_desktop_config.json` | `APPDATA` |
| Pi | `%USERPROFILE%\.pi\agent\mcp.json` | `PI_CODING_AGENT_DIR` |
| OpenCode | `%USERPROFILE%\.config\opencode\opencode.json` or existing `.jsonc` | `XDG_CONFIG_HOME`, `OPENCODE_CONFIG_DIR`, `OPENCODE_CONFIG` |

Directory/file overrides must be absolute. If both OpenCode JSON and JSONC files exist, setup refuses
to guess which to edit. Pi configuration is written to its own override file, not the shared MCP
configuration used by other applications. Setup does not alter PATH, start the server, add a
service, enable elevated operation, or install an auto-updater.

### Upgrade and uninstall

Stop MCP clients using the installed executable before upgrading or uninstalling. Setup retains
the installation directory and refuses downgrades, including prerelease downgrades. To move an
installation, uninstall it first. Locked files cause setup to stop for a retry; it does not terminate
clients or schedule executable replacement at reboot.

Uninstall from Windows Settings or run `unins000.exe` in the install directory. Uninstall offers to
remove the MCP entries written by setup, and removes an entry only if its complete value still
matches the recorded value. User-modified entries, other servers, unrelated files, and configuration
backups are retained. Replaced pre-install entries can be recovered from the backup; uninstall does
not automatically restore them.

For unattended installation, use `/VERYSILENT /SUPPRESSMSGBOXES /NORESTART`, optionally followed by
`/DIR="C:\Tools\ControlFreak"`, `/CLIENTS=codex,claude-code,claude-desktop,pi,opencode`, and
`/REPLACECLIENTS=1`. Omitting `/CLIENTS` configures no clients. Exit code `10` means the application
was installed but at least one client configuration failed; see `setup-results.txt`. Other setup
failures use Inno Setup's standard nonzero exit codes. Silent uninstall retains client entries unless
`/REMOVECONFIG=1` is supplied.

### Portable ZIP

Alternatively, extract the Windows ZIP from the same release into a stable folder and configure the
MCP client with the full path to `controlfreak.exe`. Both downloads have `.sha256` checksum files and
contain license information and SBOMs. The separate `controlfreak-installer.exe` is a private setup
utility, not an MCP server; clients must launch `controlfreak.exe`.

## Install from source

Requirements:

- Windows 10 version 2004 (build 19041) or newer, or Windows 11, on x86-64.
- Rust 1.97 or newer using the `x86_64-pc-windows-msvc` toolchain.
- Microsoft C++ Build Tools for the MSVC linker.
- A Windows OCR language pack if you want to use the OCR tools.

From the repository root:

```powershell
cargo install --path crates/controlfreak-server --locked
controlfreak --version
```

Cargo installs `controlfreak.exe` in `%USERPROFILE%\.cargo\bin` by default.
The glow remains dormant for observation-only tools such as capture, OCR, and window listing. The
first state-changing tool reserves the desktop and shows the bright `ACTING` level. Between actions,
the same session stays visible at the lower `ARMED` level, so a model's thinking time no longer makes
the glow blink. A one-shot action closes after 8 seconds. Multi-step sessions use a longer bounded
hold based on recent gaps.

For a long task, call `begin_control_session` before the first action. It reserves the desktop and
shows `ARMED`; `expected_seconds` is always clamped by `CONTROLFREAK_GLOW_MAX_HOLD_MS`. Call
`end_control_session` after the final desktop action. The timeout remains a backstop when a client
forgets to end the session. A second ControlFreak server is refused while the first owns the desktop.

`CONTROLFREAK_GLOW_HOLD_MS` overrides the default holds, and
`CONTROLFREAK_GLOW_MAX_HOLD_MS` sets their upper bound. The helper stays hidden for reuse and closes
when the server exits. `get_server_status` reports the session state, active mutation count, hold,
restart count, indicator health, whether a Windows job is enforcing helper containment, and the
server's elevation and Windows integrity level.

## Elevated operation

ControlFreak inherits its MCP client's Windows token. An elevated server is refused by default with
the structured error code `elevated_operation_requires_opt_in`. If elevated control is intentional,
add the explicit option to the configured command:

```powershell
controlfreak --allow-elevated
```

For JSON client configurations, add `"--allow-elevated"` after the executable path; for clients with
a separate arguments field, add it there. Elevated sessions replace the standard blue particles and
their glow with red counterparts; all other indicator behavior and styling remains the same. The
option does not bypass the lock screen, UAC secure desktop, non-default input desktops, or
higher-integrity target checks. See [SECURITY.md](SECURITY.md) for the full privilege-inheritance
policy. The capture-excluded indicator is rendered by a native Rust helper mode in the ControlFreak
executable; it does not launch PowerShell or compile UI code at runtime.

## Configure an MCP client

Replace `<username>` with your Windows username and restart the client after adding ControlFreak.
The examples below use the Cargo installation path. For setup or portable installations, substitute
the actual installed executable path, such as
`C:\Users\<username>\AppData\Local\Programs\ControlFreak\controlfreak.exe`.

<details>
<summary><strong>Codex</strong></summary>

Add this to `%USERPROFILE%\.codex\config.toml`:

```toml
[mcp_servers.controlfreak]
command = 'C:\Users\<username>\.cargo\bin\controlfreak.exe'
```

</details>

<details>
<summary><strong>Claude Code</strong></summary>

Run:

```powershell
claude mcp add --scope user controlfreak -- 'C:\Users\<username>\.cargo\bin\controlfreak.exe'
```

</details>

<details>
<summary><strong>Claude Desktop</strong></summary>

Add this server to `%APPDATA%\Claude\claude_desktop_config.json`, preserving existing servers and
preferences, then quit and reopen Claude Desktop:

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

</details>

<details>
<summary><strong>Pi</strong></summary>

Pi requires the MCP adapter extension:

```powershell
pi install npm:pi-mcp-adapter
```

Add this server to `%USERPROFILE%\.config\mcp\mcp.json`, then restart Pi:

```json
{
  "mcpServers": {
    "controlfreak": {
      "command": "C:\\Users\\<username>\\.cargo\\bin\\controlfreak.exe"
    }
  }
}
```

</details>

<details>
<summary><strong>OpenCode</strong></summary>

Add this to `%USERPROFILE%\.config\opencode\opencode.json`:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "controlfreak": {
      "type": "local",
      "command": ["C:\\Users\\<username>\\.cargo\\bin\\controlfreak.exe"],
      "enabled": true
    }
  }
}
```

</details>

## Development

The workspace separates domain logic, native Windows integration, the MCP adapter, and the server
executable into dedicated crates. With GNU Make installed, run the standard local checks:

```powershell
make check
```

Individual gates are available as `make fmt-check`, `make lint`, `make test`, and `make release`.
Generate API documentation with `cargo doc --workspace --no-deps --locked`.

Run the server locally with `cargo run -p controlfreak-server`. Inspect its declared capabilities
with `cargo run -p controlfreak-server -- --print-capabilities`.

See [CONTRIBUTING.md](CONTRIBUTING.md) for the crate boundaries, pull-request expectations, and the
complete CI policy.

## Current limitations

- Windows only.
- Accessibility automation and an elevation tool are not implemented; an already-elevated token is
  accepted only with `--allow-elevated`.
- Protected video and HDR content may not capture accurately through the current GDI backend.

## License

ControlFreak is released under the [MIT License](LICENSE).
