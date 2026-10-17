<p align="center">
  <img src="assets/logo.png" alt="ControlFreak mascot: a blue octopus holding two app windows, pressing a key and clicking a mouse cursor" width="240">
</p>

<h1 align="center">ControlFreak</h1>

<p align="center">Let your agents work for you.</p>

<p align="center">
  <a href="https://github.com/Subhransu-De/ControlFreak/releases"><img src="https://img.shields.io/github/v/release/Subhransu-De/ControlFreak?include_prereleases&amp;label=Release&amp;style=flat" alt="Latest release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/Subhransu-De/ControlFreak?label=License&amp;style=flat" alt="MIT License"></a>
  <a href="https://github.com/Subhransu-De/ControlFreak/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/Subhransu-De/ControlFreak/ci.yml?branch=main&amp;label=Build&amp;style=flat" alt="Build status"></a>
</p>

ControlFreak lets an AI agent use your Windows desktop. It is a computer-use
[MCP](https://modelcontextprotocol.io) server written in Rust. Your MCP client, such as Claude
Code, Codex, Claude Desktop, OpenCode or Pi, starts it on your computer and talks to it over
STDIO. The agent can take screenshots, read text with the Windows OCR engine on your computer,
and click, scroll and type in one window that it chose.

[Install](#install-on-windows) · [Connect a client](#configure-an-mcp-client) ·
[Privacy](PRIVACY.md) · [Security](SECURITY.md) · [Changelog](CHANGELOG.md)

## What it can do

- Capture displays, regions, and windows.
- Move, click, drag, and scroll the mouse across multiple displays.
- Send keyboard shortcuts and type Unicode text without using the clipboard.
- List, focus, capture, and wait for application windows.
- Read, find, wait for, and click on-screen text using the local Windows OCR engine.
- Run up to 16 clicks, scrolls and keystrokes on one window in a single call.
- Inspect and switch between discoverable Windows virtual desktops.

Run `controlfreak --print-tools` to see all 27 tools with their input and output schemas.

## What it will not do

- It has no tools to run commands, launch applications, open files, read the clipboard, edit
  the registry, use the network, or elevate privileges.
- Every click and keystroke is checked against the one window the agent approved. If another
  window has the focus or lies under the pointer, the action is refused instead of sent.
- It refuses to start with administrator rights unless you add `--allow-elevated`. Even then, it
  never acts on the lock screen, the UAC prompt, or a window with higher integrity.

This is a limit on the tools, not a sandbox. Keyboard input can still use whatever an approved
application can do, so only let an agent control applications you would let it use.

## When the agent is in control

A blue glow appears around the edges of every display while an agent controls the desktop, and
a navy bar at the top of the primary display reads "ControlFreak is controlling your system
(Use Esc to stop)". Press **Esc** to stop the agent. Screenshots, OCR and window listing do not
show the glow, because they do not change anything.

The glow and the bar are excluded from screen capture, so the agent never sees them. Mouse
clicks pass through both, and neither takes the focus. When Windows animation effects are off,
the glow does not breathe. When a High Contrast theme is on, the bar uses the system highlight
colors.

The glow is bright while an action runs and dimmer between actions of the same session, so a
model's thinking time does not make it blink. A one-shot action closes its session after 8
seconds. Multi-step sessions use a longer bounded hold based on recent gaps.

The glow fades inward across 230 physical pixels on each display. It uses 42% base opacity, a
soft bright core spanning 39% of that width at 26% core opacity, and an 18% breathing modulation
over 4.7 seconds. The dimmer level uses 48% of the full strength. Display scaling does not
enlarge the physical-pixel width.

For a long task, call `begin_control_session` before the first action. It reserves the desktop and
shows the dim level; `expected_seconds` is always clamped by `CONTROLFREAK_GLOW_MAX_HOLD_MS`. Call
`end_control_session` after the final desktop action. The timeout remains a backstop when a client
forgets to end the session. A second ControlFreak server is refused while the first owns the desktop.

`CONTROLFREAK_GLOW_HOLD_MS` overrides the default holds, and
`CONTROLFREAK_GLOW_MAX_HOLD_MS` sets their upper bound. The helper stays hidden for reuse and closes
when the server exits. `get_server_status` reports the session state, active mutation count, hold,
restart count, indicator health, whether a Windows job is enforcing helper containment, and the
server's elevation and Windows integrity level.

See [Stop desktop work](#stop-desktop-work) for exactly what Esc stops and what it cannot undo.

## Install on Windows

Download `ControlFreak-<version>.exe` from [GitHub Releases](https://github.com/Subhransu-De/ControlFreak/releases).
The installer supports Windows 10 version 2004 (build 19041) or newer and Windows 11 on x86-64.
Rust and Microsoft C++ Build Tools are not needed for packaged releases. OCR requires an installed
Windows OCR language pack. The setup executable and portable binaries are currently **unsigned**.

Setup installs for the current user and does not request administrator privileges. Run it from a
normal, non-elevated session. The wizard asks for an installation directory, then offers checkboxes
for Codex, Claude Code, Claude Desktop, Pi, and OpenCode before installing. The default directory is
`%LOCALAPPDATA%\Programs\ControlFreak`. Selecting a client configures its MCP connection; it does not
install that client. Selecting no clients installs only ControlFreak.

Pi additionally requires `pi install npm:pi-mcp-adapter`; setup does not download or install the
adapter. Quit and reopen selected clients afterward.
Project-specific or managed client configuration can take precedence over these user settings.

Setup preserves other settings and servers, including TOML and JSONC comments. For each selected
client, setup installs or updates its ControlFreak entry automatically. Before changing an
existing file, setup saves a uniquely named `<filename>.controlfreak-backup-*.bak` beside it.
EFS-encrypted configuration files require manual setup; setup refuses to create a plaintext backup.
Treat these backups as private: they can contain credentials from the original configuration. Malformed,
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

### Verify a download

Each release file has a `.sha256` checksum file. The installer, the ZIP, and the
`controlfreak.exe` and `controlfreak-installer.exe` inside the ZIP also have GitHub build
provenance attestations. With the [GitHub CLI](https://cli.github.com), check that a file was
built by this repository's release workflow:

```powershell
gh attestation verify ControlFreak-<version>.exe --repo Subhransu-De/ControlFreak
```

Attestations start with 0.1.0. The binaries are not code-signed yet, so Windows SmartScreen can
show a warning when you run the installer.

### Upgrade and uninstall

Stop MCP clients using the installed executable before upgrading or uninstalling. Setup retains
the installation directory and refuses downgrades, including prerelease downgrades. To move an
installation, uninstall it first. Locked files cause setup to stop and show process names and PIDs
reported by Windows Restart Manager. Stop those processes or disconnect their MCP clients, then
click Try Again to recheck the files and continue installation. Each process is listed separately
with its PID. If Windows cannot identify a blocker, check folder permissions and close clients
using this installation. Setup does not terminate clients or schedule executable
replacement at reboot. A running server must be restarted to use the new version.

Optional MCP configuration cleanup never blocks application removal. Each client is handled
independently. Malformed, unreadable, busy, concurrently changed or unverified configurations are
retained, as are files whose backups cannot be safely created. Unfinished profiles keep their receipts
for retry or recovery. Cleanup invalidates recorded ownership before touching a configuration;
if that cannot be saved, the configuration is retained. Interrupted cleanup can leave uncommitted
receipts that require manual cleanup. Cleanup warnings identify affected clients; if a write and its rollback both
fail, restore that client's configuration from its adjacent `.controlfreak-backup-*.bak` file.
A missing or failing helper leaves cleanup for manual inspection. Interactive uninstall displays
warnings; unattended uninstall continues and records available warnings in its Inno `/LOG` output.
Reconfiguring a client at a new profile path preserves the previous installation receipt so
uninstall can clean unchanged installer-owned entries at both paths. Receipts claim ownership only
after a successful configuration write; interrupted or failed setup may require manual entry cleanup.

Uninstall from Windows Settings or run `unins000.exe` in the install directory. Uninstall offers to
remove the MCP entries written by setup, and removes an entry only if its complete value still
matches the recorded value. User-modified entries, other servers, unrelated files, and configuration
backups are retained. Replaced pre-install entries can be recovered from the backup; uninstall does
not automatically restore them.

For unattended installation, use `/VERYSILENT /SUPPRESSMSGBOXES /NORESTART`, optionally followed by
`/DIR="C:\Tools\ControlFreak"` and `/CLIENTS=codex,claude-code,claude-desktop,pi,opencode`.
Selected clients are updated automatically. Omitting `/CLIENTS` configures no clients.
Exit code `10` means the application
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

Add this server to `%USERPROFILE%\.pi\agent\mcp.json`, then restart Pi:

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

## Privacy

Screenshots, OCR text and window details go to your MCP client, which usually sends them to an
AI model. ControlFreak itself has no network code and keeps data only in memory. See
[PRIVACY.md](PRIVACY.md) for what it reads and keeps, and what setup changes.

## How it works in detail

The sections below describe exact tool behavior for client and agent authors.

### Approved targets

Window IDs are opaque references issued by this server. Do not construct IDs from HWNDs or PIDs.
Choose an ID from `list_windows` or a current observation. Pass it as `target_ref` to
`begin_control_session` and every input action. `focus_window` uses its `window_id` as the
approved reference. A session permits one reference; end it before approving a different one.

Screenshot and OCR results include `target_ref` when the foreground target and observation
state remained compatible across capture. A null reference does not authorize input. References
retain their source window bounds and display layout, expire five minutes after the latest compatible observation, and use a
bounded server cache. Their opaque IDs identify the stored observation generation. Input checks
process creation time, window identity and ownership, desktop, foreground, integrity, bounds,
and display layout before every batch, including later text batches and drag movement.
Pointer destinations must belong to the approved top-level window. Owned popups are not implicitly
approved. A mismatch invalidates the session target and sends no further action input.
Release-only cleanup still runs when an earlier batch left keys or buttons held.

Use `focus_window` to acquire the chosen target. Its `timeout_ms` defaults to 1000 and accepts
100 through 5000. Refused activation retries after 50 ms, then 100 ms, then at most every 200 ms.
The server polls at 10 ms intervals and waits without retrying once Windows accepts activation.
It restores minimized windows using `ShowWindowAsync` and activates with `SetForegroundWindow`.
It never generates keyboard or mouse input to acquire focus. A third foreground window stops
activation. Errors distinguish `activation_refused`, `settle_timeout`, `target_invalidated`,
and the existing integrity refusals. Results include attempt count and elapsed milliseconds.

Restoring or moving a window can invalidate its old bounds. End that session, capture the restored
window, and approve its new reference before coordinate input. Desktop switching also invalidates
the previous target; multi-step switching stops when the next batch cannot validate it.

`input.target_remained_foreground` reports the post-action identity/foreground check, or null
when unavailable. It is sampled evidence, not proof of uninterrupted ownership or application
success. Validation narrows Windows event-delivery races; it cannot make external input atomic.
After refusal or uncertain delivery, end the session and observe again. Do not replay input or
silently refocus after a mismatch.

### OCR matching and recovery

`click_text` keeps strict trimmed equality by default, with optional Unicode lowercase comparison.
`exact_match=false` selects substring matching. It checks every fully contained, nonempty OCR
line in the requested region before input. Duplicate and overlapping matches remain ambiguous.
Success includes `action.matched_text` with the original text and display-local physical-pixel
bounds used for the click, plus `action.match_tier` as `exact` or `substring`.

Failures use `ocr_no_match` or `ocr_ambiguous_match`. `error.details` contains up to 20 candidates,
with at most 256 Unicode scalar values per text. `candidate_count` counts all eligible candidates;
`candidates_truncated` and `text_truncated` report omitted candidates and shortened returned text.
For no match, candidates are recognized lines from the requested region, not matches. For ambiguity,
they are matching lines. Candidate limits never limit the uniqueness check. Narrow the region for
repeated labels, refine the query using recognized text, or change the discovery mode when strict
matching misses a label. Neither failure injects input or falls back to coordinates.

`find_text_on_screen` is discovery only. Its explicit `match_mode` values are `substring`, the
default, `exact`, and `tolerant`. Exact and substring ignore surrounding query whitespace and use
`case_sensitive` consistently. Discovery returns `match_tier`, the full `candidate_count`,
truncation flags, and up to `max_results` matches, each limited to 256 Unicode scalar values.
A limited response containing one candidate does not establish uniqueness.

`wait_for_text` repeats the same discovery in a region until the query `appears` (the default)
or `disappears`, or until `timeout_ms` (default 5000, at most 30000) elapses. It returns
`satisfied`, `timed_out`, `attempts`, `elapsed_ms`, and the final pass's matches, so a client
can wait for a status message or a closing dialog in one call instead of a capture-and-read
loop. Each pass is a full recognition, so a small region is faster. Like discovery, it never
authorizes input.

Tolerant discovery tries these tiers in order and stops at the first nonempty tier, even when
that tier is ambiguous:

1. Trimmed exact equality with the requested case handling.
2. Unicode NFKC normalization, Unicode whitespace collapsed to one space, curly single/double
   quotes folded to ASCII, and U+2010 through U+2014 and U+2212 folded to ASCII hyphen.
   NFKC also folds compatibility characters such as full-width letters and ellipsis.
3. Normalized equality across two consecutive lines sorted by Y, X, dimensions, and text.
   Lines must not overlap vertically, their vertical gap must not exceed the smaller line height,
   and their left edges must differ by at most half that height. Joined text is limited to 256
   Unicode scalar values. The returned bounds enclose both lines.
4. Only with `ocr_confusions=true`, one substitution within `0/O/o` or `1/I/i/l/|` in a
   normalized label of 4 to 64 Unicode scalar values. No insertions, deletions, or closest-label ranking.

`ocr_confusions` is rejected outside tolerant discovery. Tolerant results never authorize a
`click_text` action; inspect them, then choose a fresh strict query and region. Matching tiers
are deterministic rules, not recognition confidence. Windows OCR supplies no confidence scores.
No contrast adjustment or dark-theme inversion is enabled. Enabling preprocessing would first
require OCR evaluation on synthetic light/dark, low-contrast, punctuation, Unicode, and checkbox images.

Prefer capability-supported UI Automation patterns for checkbox, selection, and value operations
when available through a client or another tool. OCR text and checkbox glyphs do not prove control
state. ControlFreak does not currently implement UI Automation.

### Image-space points

Every returned screenshot includes an `observation_id`. Pointer tools accept that ID with
`image_x` and `image_y` in the screenshot's own pixels, instead of `display_id` with display-local
`x` and `y`, and the server maps the point through the screenshot's crop and resize. This avoids
DPI and resize arithmetic in the client. Give exactly one form per point. IDs keep only geometry,
expire after five minutes, and at most 64 are retained. Mapped points pass the same target checks
as direct coordinates. Screenshots are downscaled by area averaging (`resize_method:
area_average`), which preserves thin text strokes better than sampling.

### Bounded action batches

`run_actions` runs up to 16 `click`, `scroll`, `type_text`, and `press_keys` steps in order on one
approved target and returns one final observation. Every step's arguments, including image
points and whether each point lies on an active display, are checked before the first step runs,
so a malformed later step refuses the whole batch without input. Live checks that depend on
desktop state, such as foreground ownership and the window under a point, run per step. Each step uses the same target checks, settle wait, and release cleanup as its
single-action tool, and only the last step captures the requested observation. The batch stops at
the first failure: `action.steps` (`error.details.steps` on failure, or `details.steps` when every
step was sent but the final observation failed) reports each step as
`completed`, `failed`, or `skipped` with its accepted event count, and `status` is
`partially_sent` when earlier steps already ran. Use it for a known sequence such as filling a
form; observe between calls when a later step depends on what an earlier one shows.

### Action results

Mouse, keyboard, OCR-click, window-focus, and desktop-switch results report input dispatch
separately from observation. `input.input_outcome` is `not_started`, `input_sent`,
`partially_sent`, or `unknown`. `input.sent_events` counts known accepted input events across
all batches, excluding cleanup and cursor/window API calls. It is not a character count or
proof that text reached an application.

`observation_status` is `succeeded`, `failed`, or `not_attempted`. A successful observation
can omit the screenshot when requested. `effect_verification` is currently always `unverified`:
input acceptance and a screenshot do not establish that a submission succeeded or a checkbox changed.
`input.cleanup` reports `not_needed`, `succeeded`, or `unknown` for release cleanup.

Normal results retain `action`, `observation`, and the pointer `position` where applicable.
Incomplete results retain the error or warning and report `status` as `completed_unverified`,
`partially_sent`, `unknown`, or `not_started`. All variants conform to the tool's published
output schema. Partial and uncertain delivery are non-error results so clients must read
`status`, not just `isError`. Diagnostics preserve these statuses without recording input contents.

Action results set `retry_action=false`. After incomplete or uncertain delivery, observe the
desktop before choosing a recovery action. Do not replay the entire action or switch input
methods merely because observation or delivery verification failed.

### Session errors

When an action cannot start a control session, `error.code` says why, with `error.retryable` and
`error.details`:

| Code | Meaning |
| --- | --- |
| `desktop_in_use` | Another ControlFreak server owns the desktop. `details` has the owner and `retry_after_ms`. Retryable. |
| `session_closing` | The previous session of this server is still closing. Retryable. |
| `server_shutting_down` | The server is shutting down. Not retryable. |
| `desktop_work_stopped` | The user pressed Esc or a client called `stop_desktop_work`. Not retryable. |
| `control_environment_unavailable` | The input desktop cannot be controlled, for example while the workstation is locked. Not retryable. |
| `desktop_arbitration_unavailable` | The server could not check which server owns the desktop. Restart the server. Not retryable. |

Every tool publishes an `outputSchema` for its successful and partial results. Errors use the
common `error` envelope.

### Stop desktop work

A navy blue bar with white text appears at the top center of the primary display,
16 display-scaled pixels below its top edge, while ControlFreak owns a control session.
It reads "ControlFreak is controlling your system (Use Esc to stop)". Press **Esc**
to stop. The bar does not take focus and remains visible with cleanup status until
ownership is released. It stays hidden during observation-only use.
The bar and Esc monitor run independently of MCP requests, OCR and the glow helper.
Only physical Esc stops the session; injected keys are ignored. The monitor does
not retain typed text or keyboard history. The stop key press and release are
consumed so they do not also dismiss a dialog in the controlled application.
Clients can call `stop_desktop_work` or cancel an individual request with the MCP
`notifications/cancelled` notification. Request cancellation affects that request;
Esc and the stop tool close admission for the server's lifetime.
`begin_control_session` and `end_control_session` cannot clear a stop.

A user stop sends a custom MCP `notifications/controlfreak/session_stopped`
notification with `params.event="user_stopped_session"`,
`reason="user_stop"`, `stop_state`, and `retry_action=false`. The MCP connection
stays open. The server advertises this as `experimental.controlfreak/user-stop`.
Client support determines whether this notification is shown to the
agent. Status also retains `stop_reason="user_stop"`, and subsequent action
requests report that the user stopped control. Agents must not restart the server
or resume desktop actions without the user's permission.

`get_server_status.stop_state` reports `ready`, `draining`, `stopped`, or
`cleanup_failed`. Read-only captures, lists, status and session cleanup remain available after stop. Queued
mutations check cancellation before dispatch. Captures and lists also honor request cancellation
before queued work starts and after an in-flight native call returns. Movement and visual waits use 10 ms cancellation polls between native calls; typing checks between bounded batches.
Already-dispatched application operations cannot be undone. A blocked native call,
including OCR, can still be draining. The private OCR helper forwards cancellation
to WinRT recognition and retains its existing
30-second deadline; stopping does not kill applications or lock the workstation.

Cleanup releases only acknowledged, unmatched ControlFreak key and button downs.
Input refuses a requested key or button that is already held. Desktop ownership
and the glow remain until mutation workers finish cleanup. If release cleanup is
uncertain, ownership stays reserved and status reports `cleanup_failed`.
Only the user should restart the server to re-arm it, after checking that work has
drained and resolving any held input. There is no MCP re-arm operation.

### Elevated operation

ControlFreak inherits its MCP client's Windows token. An elevated server is refused by default with
the structured error code `elevated_operation_requires_opt_in`. If elevated control is intentional,
add the explicit option to the configured command:

```powershell
controlfreak --allow-elevated
```

For JSON client configurations, add `"--allow-elevated"` after the executable path; for clients with
a separate arguments field, add it there. Elevated sessions replace the standard blue glow and
its bright core with red counterparts; all other indicator behavior and styling remains the same. The
option does not bypass the lock screen, UAC secure desktop, non-default input desktops, or
higher-integrity target checks. See [SECURITY.md](SECURITY.md) for the full privilege-inheritance
policy. The capture-excluded indicator is rendered by a native Rust helper mode in the ControlFreak
executable; it does not launch PowerShell or compile UI code at runtime.

## Development

See the [tested agent recipes](examples/README.md) for observation, crop-coordinate mapping,
verification, waits, partial-input recovery, and atomic dragging. These run against a synthetic
fixture without interacting with the desktop. Export the current tool descriptions and schemas
with `cargo run -p controlfreak-server --locked -- --print-tools`.

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

- Windows 10 version 2004 or newer and Windows 11, on x86-64 only.
- The binaries are not code-signed.
- UI Automation (the accessibility tree) is not used. The agent works from screenshots, OCR and
  coordinates.
- An already-elevated token is accepted only with `--allow-elevated`. There is no elevation tool.
- Protected video and HDR content may not capture accurately through the current GDI backend.

## License

ControlFreak is released under the [MIT License](LICENSE).
