# Changelog

All notable changes to ControlFreak are documented in this file.

The project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Changes are collected under Unreleased. The release workflow moves them into a dated,
versioned section and uses that section for the corresponding GitHub Release notes.

## Unreleased

### Changed

- Input actions and control sessions now require an opaque `target_ref`; `focus_window` uses
  `window_id`. Sessions refuse target changes, stale window/process identities, incompatible
  observation geometry, desktop changes, focus theft, and pointer hits on other windows.
- Foreground activation now retries within a configurable 100 to 5000 ms budget, reports attempts
  and elapsed time, and distinguishes refusal, settle timeout, invalidation, and integrity errors.


### Added

- Structured OCR no-match and ambiguity errors with bounded candidate metadata and recovery
  guidance. Successful text clicks report the matching tier and selected bounds.
- Opt-in tolerant OCR discovery with Unicode, whitespace and punctuation normalization,
  bounded adjacent-line joining, and optional single-character OCR-confusion matching.
  Strict click authorization remains separate from discovery.

- Fixture-tested agent recipes for observation, crop mapping, verification, visual waits,
  partial-input recovery, and atomic dragging. Export published tool descriptions and schemas
  with `--print-tools`.

### Fixed

- Check text-click uniqueness across all recognized lines before clicking, so search result
  limits cannot hide duplicate or unique exact matches. Apply case and surrounding-whitespace
  handling consistently when selecting click targets.
- Preserve cumulative input delivery and release-cleanup status across action failures,
  cancellation, and worker failure. Report post-action observation failures separately and
  prevent automatic replay of partial or uncertain input.
- Make normal, partial, and unverified action results conform to published MCP output schemas,
  with client-side schema validation over a synthetic MCP transport.

### Changed

- Replace the desktop-edge particles with a continuous, inward-fading glow spanning 230 physical
  pixels, with a soft bright core and a 4.7-second breathing cycle. Between-action sessions retain
  the glow at 48% strength; elevated sessions retain red styling.
- Upgrade the MCP SDK to 3.4.0 for lifecycle-aware cancellation and first-request
  dispatch fixes, and migrate server configuration to its supported API name.

## [0.1.0-alpha] - 2026-09-16

### Added

- Optional uninstall configuration cleanup handles each client independently and never blocks
  application removal. Unchanged files are retained when preparation fails; failed writes attempt
  rollback, with explicit recovery warnings if rollback fails. Ownership is invalidated before
  cleanup so leftover receipts cannot claim recreated manual entries. Unfinished profiles retain receipts.
- Configuration backups refuse EFS-encrypted source files rather than exposing plaintext.
- Configuration ownership is preserved across client profile path changes and committed only
  after a successful write. Malformed configurations and damaged receipts remain untouched.

- Manual release workflow that prepares version files and dated changelog entries, verifies
  Windows packages, and creates the release commit, Git tag, and GitHub Release.

- Unsigned per-user Windows x64 setup executable with location selection, optional MCP configuration
  for Codex, Claude Code, Claude Desktop, Pi, and OpenCode, and per-client installation results.
- Configuration backups, automatic updates for selected clients, upgrade/downgrade checks, silent setup,
  and conditional removal of installer-created MCP entries during uninstall.
- Installer and portable release verification before publication, including checksums, executable
  version identity, piped MCP startup, synthetic client profiles, and installation lifecycle checks.
- Statically linked MSVC runtime for Windows packages, avoiding a separate VC runtime installation.
- Rust `cargo xtask` commands for compiler setup, packaging, verification, and release publication.
- Blocking process names and PIDs in separate bullet points, clear file-in-use instructions,
  and a Try Again button that repeats installation checks. Diagnostics use read-only Windows
  Restart Manager queries.

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
