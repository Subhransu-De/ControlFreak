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
