# Changelog

All notable changes to ControlFreak are documented in this file.

The project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

No version has been released yet. There is no tag and no published artifact. Everything below is
unreleased and describes the current state of `main`. Tool contracts and installation details may
change before the first release.

## Unreleased

### Added

- Native Windows STDIO MCP server with twenty-four computer-use tools.
- Multi-display discovery, proportional screenshots, bounded region and window capture, and cursor
  markers.
- Mouse movement across displays, left/right/middle clicking, dragging, and horizontal or vertical
  scrolling.
- Unicode text input and case-insensitive keyboard chords without clipboard access.
- Window discovery, focus, capture, waits, and window-backed virtual desktop discovery and
  switching.
- Local Windows OCR, text search, and fail-closed unique text clicking. The OCR engine runs in an
  isolated child process with a bounded lifetime.
- Visual baselines and server-side visual-change waits for reliable post-action verification.
- Cropped, metadata-only, or disabled post-action observations to control image overhead.
- Explicit control sessions through `begin_control_session` and `end_control_session`, with an
  optional `expected_seconds` hint and an inactivity timeout as the backstop when a client forgets
  to end the session.
- Click-through animated desktop-edge glow that shows when a client owns the desktop. It stays at a
  dim `ARMED` level between tool calls and a bright `ACTING` level during a mutation, so model
  thinking time no longer makes it blink. The glow is excluded from captures.
- Glow hold tuning through `CONTROLFREAK_GLOW_HOLD_MS` and `CONTROLFREAK_GLOW_MAX_HOLD_MS`, and an
  opt-in indicator troubleshooting log through `CONTROLFREAK_GLOW_ERROR_LOG`.
- Desktop arbitration across clients, displays, and virtual desktops. A second ControlFreak server
  is refused while the first owns the desktop.
- The indicator runs as a native helper mode of the same executable. It does not launch PowerShell
  and does not write executable scripts to disk. A per-helper Windows job object contains it when
  one can be created; if that fails the helper still starts, with cooperative cleanup only, and
  `get_server_status` reports whether containment is active.
- Opt-in elevated operation. The server refuses an inherited elevated token unless it is started
  with `--allow-elevated`, and an elevated session renders the glow in red instead of blue.
- Process and operation diagnostics with payload-free lifecycle events, per-call `gap_ms`, and
  consistent structured errors.
- `--print-capabilities` and `--version` command-line options, and a `get_server_status` tool that
  reports session state, hold, indicator health, helper restart count, and security context.
- Install-from-source instructions and MCP client setup for Codex, Claude Code, Pi, and OpenCode.
- MIT `LICENSE` file, a `license` field on every workspace crate, and the license text in the
  packaged release archive.
- Daily Dependabot checks for Cargo and GitHub Actions.
- Automatic assignment of every opened, reopened, or review-ready pull request to the repository
  owner.

### Security

- No application-launch, shell, filesystem, clipboard, registry, network, or elevation tool. This is
  a tool-surface boundary, not a sandbox: keyboard input can still reach anything an already-running
  application exposes.
- Every state-changing action revalidates the unlocked interactive `Default` input desktop and the
  target process integrity immediately before injection. A target above the server's integrity is
  refused with `higher_integrity_target`, and an unverifiable target with
  `target_integrity_unavailable`.
- The UAC secure desktop, the lock screen, disconnected sessions, and non-default input desktops
  stay blocked even when `--allow-elevated` is present.
- Input tools fail closed when the indicator cannot be shown: the operation is refused admission
  rather than run dark. Indicator failure part-way through an operation is delivered as an
  asynchronous cancellation, so a timed pointer move can still complete the frame already in
  flight.
- Atomic click and drag sequences always attempt to release held inputs.
- Missing or ambiguous OCR targets fail before input is injected.
- Diagnostic events exclude screenshots, typed text, OCR contents, tool arguments, and window
  titles. Nothing is written to disk by default. Setting `CONTROLFREAK_GLOW_ERROR_LOG` opts into an
  append-only log at the given path that records indicator helper lifecycle, helper stderr, and
  error text; it still carries no desktop content.

### Known limitations

- Windows x86-64 is the only supported platform.
- Input is not yet bound to an approved window. The guards check the input desktop and the target
  process integrity immediately before injection, but not target identity, so a focus change between
  observation and mutation can send input to the wrong window.
- There is no emergency stop.
- Operation diagnostics are held in memory and there is no sanitized export. The only disk output is
  the opt-in `CONTROLFREAK_GLOW_ERROR_LOG` indicator log.
- UI Automation accessibility-tree inspection is not implemented; OCR and coordinates are used.
- GDI capture may not reproduce protected video or HDR content accurately.
- The supported Windows virtual desktop API cannot enumerate empty desktops or expose desktop names
  and ordering.
