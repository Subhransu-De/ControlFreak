# Changelog

All notable changes to ControlFreak are documented in this file.

The project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Changes are collected under Unreleased. The release workflow moves them into a dated,
versioned section and uses that section for the corresponding GitHub Release notes.

## Unreleased

### Added

- Every tool has a `title`, and annotations are consistent: every input tool is `destructiveHint: true` and `openWorldHint: true`. `stop_desktop_work` is now annotated destructive, because it closes input permanently, and is listed last. Its description tells the model to use `end_control_session` to finish normally.
- `focus_window`, `capture_window` and `wait_for_window` accept `target_ref` as an alias of `window_id`. Giving both is refused unless they are equal.
- `switch_virtual_desktop` takes `target_ref` as an optional field. Without it the session's approved window, or the foreground window, is used. The switch still runs the input-desktop and foreground checks and ends the session.
- `display_id` is optional wherever a tool took it, defaults to the primary display, and accepts `"primary"`. Explicit captures accept `native: true` to keep full resolution.
- `run_actions` accepts a `wait` step (`wait_ms` from 1 to 5000, at most 15000 per call, never the last step).
- Every error has `error.retryable`, and `platform_operation_failed` has a stable `error.reason` (`pointer_moved`, `key_held`, `input_interleaved`, `cancelled`, `ocr_unavailable`, `input_blocked`, `desktop_work_stopped`, `worker_failed`, `os_call_failed`, `backend_unavailable`). `target_invalidated` also has a `reason` (`not_foreground`, `foreground_changed`, `foreign_destination`, `destination_unidentified`, `own_indicator`, `target_changed`) and a truthful `retryable`, so a recoverable refusal can be told from a fatal change without reading the message. Reasons come from typed causes set where the failure happens, not from message text. Error codes are unchanged.
- `press_keys` accepts punctuation (`comma`, `period`, `slash`, `semicolon`, `quote`, `left_bracket`, `right_bracket`, `backslash`, `minus`, `equals`, `backtick`, or the character itself), numpad keys, `print_screen`, `caps_lock`, `scroll_lock`, `num_lock`, `pause`, `menu`, volume and media keys, and the aliases `up`, `down`, `left`, `right`. An unknown key is refused with the nearest valid name.
- `list_windows` lists visible owned windows such as dialogs, with an `owner` field holding the owner's target reference. `focus_window` and `capture_window` accept them.
- The taskbar, secondary taskbars, the desktop and the Start and Search surfaces can be approved as targets. They are recognized by window class and by the verified program that owns them, and treated as present on every virtual desktop. There is still no tool that launches applications.
- `capture_window` reports `capture_method` `windows_print_window` when the window rendered itself and `windows_gdi` when its screen rectangle was copied.
- The session tab gets a **Stop agent** button that works with a mouse, touch, pen and accessibility tools (UI Automation Invoke). Injected input, which is all the agent can send, cannot press it. Physical Esc still stops a session.

### Changed

- Every `outputSchema` accepts the error result as well as the success result, so clients that validate `structuredContent` can read errors. A test validates a real error for every tool against its published schema.
- `tools/list` is 14.1% smaller, with the same accepted and returned shapes: 115,810 bytes before, 99,427 bytes after (`--print-tools`, compact JSON; output schemas 75,205 to 61,965 bytes). The guard test allows 102,400 bytes. Descriptions lead with preconditions and recovery, and the server instructions are shorter. They now state the foreground rule, that dialogs of the approved application are accepted, that `stop_desktop_work` is permanent and that display IDs default to the primary display.
- Explicit captures (`capture_display`, `capture_region`, `capture_window`) are 1280 px wide by default, like action screenshots, instead of native resolution. Pass `native: true` for full resolution. `max_width` and `native` cannot be combined.
- All wait tools accept `timeout_ms` from 0.
- The six read-only observation tools (`read_text_in_region`, `find_text_on_screen`, `wait_for_text`, `wait_for_window`, `wait_for_visual_change`, `wait_for_change_since`) stay available after a stop. A stop cancels a wait that is already running.
- An `observation_id` from a window capture maps points only while that window is unchanged. If the window moved, resized or closed, the point is refused with `invalid_argument`.
- A failed foreground activation tells the agent that the user may need to click the window first.
- Installer configuration parse errors give the line and column, never the content.
- A control session is admitted only after the Esc keyboard hook is confirmed installed (bounded wait of 2 seconds). If the hook cannot be installed the session is refused with the reason, instead of running without a working Esc stop.
- A window of the approved target's own process (its dialogs, popup menus and drop-downs) may hold the foreground and receive pointer input, with the integrity check applied to that window. A window of another process is still refused. For `explorer.exe` targets only the target's own dialogs and popup menus count.
- A wrong foreground window, keyboard-focus window or pointer destination (another process, a higher-integrity window, an unreadable window, or no foreground at that instant) now refuses only that action. The reference and the session target are kept, and the next action checks the live foreground again. Changes to the target itself still end the session's approval.
- A failed `focus_window` (refused or timed out) no longer ends the session, so the retry its error asks for works. Inside a session `focus_window` accepts a window of the approved application's process, such as a dialog, and a dialog taking the foreground during activation is no longer treated as a change of owner.
- A point under the Stop agent button is refused with reason `own_indicator` and a message that names the button, instead of the generic other-process message. The Stop button accepts only input from hardware, so posted messages and unreadable origins no longer press it.
- Targets in `ApplicationFrameHost.exe`, and targets whose program file cannot be read, use the same owner-chain rule as `explorer.exe` targets.
- `press_keys` refuses a named punctuation key that the active keyboard layout types only with Shift or AltGr, with an error that suggests `type_text`, instead of sending the US key.
- Double and triple clicks validate the target again before every press, not only before the first.
- `capture_window` renders on a watchdog thread with a 2-second deadline, skips windows that are flagged as hung and falls back to the screen copy (`windows_gdi`), so a hung window cannot block a capture.
- `list_windows` says in its description that owned dialogs are listed.
- `press_keys` and `type_text` handle held keys the same way: an Enter or Tab segment of `type_text` is refused with `key_held` if the person already holds that key, before anything of that segment is sent. Text sent earlier stays sent and is reported.
- `wait_for_text` reuses the previous pass only when a fresh read of the region matches a fingerprint of the exact frame that recognition read, and the foreground, window set and desktop are unchanged. Recognition reports that fingerprint, so a transient frame can no longer leave a stale answer in place until the timeout, and an unreadable comparison always recognizes again.
- `focus_window` no longer reports a fatal-looking `target_changed` when the window in front cannot be given a target reference (for example a shell surface or no foreground at that instant). It keeps the raw handle of that window and carries on.
- The `focus_window` description says it accepts the approved window or, inside a session, a dialog of the same application. `docs/tools.md` now separates the retryable foreground refusals (`target_invalidated` with a `reason`, recover with `focus_window`) from the higher-integrity ones (`higher_integrity_target`, `target_integrity_unavailable`, no `reason`, `focus_window` cannot recover them).
- A click batch that only releases the button and modifiers is no longer validated, so a pointer nudge or foreground change during the 20 ms hold cannot turn a completed press into an error. Every batch that presses is still validated.
- A packaged (UWP) app target, whose frame belongs to `ApplicationFrameHost.exe` and whose content window belongs to the app, now accepts input to the content window hosted inside its own frame. The integrity check still runs on the window that is actually hit or focused, and a hosted pid never widens the rule for another kind of target.
- A render that panics no longer leaves the single `capture_window` render slot taken, which would have made every later window capture copy the screen. The chord form of the layout refusal tells the agent to name the layout's own key combination, and the first-read listener removes itself once the tab is up.
- `type_text` presses Enter for `\n`, `\r\n` and `\r` and Tab for `\t`, instead of sending them as Unicode characters.
- `click_text` matches words inside OCR lines and clicks the centre of the matched words instead of the centre of the whole line. `find_text_on_screen` reports the matched-word box. Two matches in one line are ambiguous.
- `capture_window` renders the window itself, so a covered window is captured as itself, and falls back to a screen copy.
- Mouse clicks hold the button for 20 ms. `drag_mouse` always travels in several steps over at least 120 ms.
- Target validation reuses identity and integrity results for 250 ms within one input lock, the settle watcher and `wait_for_text` reuse their GDI objects and pixel buffers, screenshots are encoded without a second full-frame RGB copy, and `wait_for_text` skips OCR while the region and foreground are unchanged.
- The low-level keyboard hook for Esc is installed only while a session is owned, on a dedicated thread, and removed when the session ends. The tab says "Press Esc to stop" only after the hook is confirmed installed.
- The session tab and Stop button start with the first screen read or the first session, and the Esc monitor with the first session. If they cannot start, the server still runs, read-only tools work, and sessions are refused with the reason. A tab failure after start-up still fails closed, and the reason is now kept in refusals, `get_server_status` and standard error.
- Captures and OCR show a brief "ControlFreak is reading the screen" state on the tab without claiming a session or installing a keyboard hook, even before any session has run. A tab that cannot start never breaks a read. The waiting tab says "ControlFreak is waiting for the next action", and the tab returns to dormant 1 second after a normal session end.
- The indicator no longer polls while idle: the tab thread, the lamp helper and the stop notification wait for events, the session worker polls only while a session is owned, and an idle lamp-helper actor polls twice a second. Lock and desktop-switch detection during a hold is unchanged.
- Failed helper shutdowns are retried with a growing delay and abandoned after about a minute, with the reason on standard error.
- Apps & Features and `winget list` now show the app as `ControlFreak` instead of `ControlFreak version <version>`. The version stays in its own column.
- The control indicator is redesigned. The 230-pixel blue glow around every display and the navy bar are replaced by a steady 4-pixel turquoise lamp along the top of every display, and a tab hanging from it on every display that reads "An agent is using this PC", names ControlFreak and shows "Press Esc to stop". The tab says whether the agent is working or waiting, keeps an "Administrator" label for elevated servers, says when keys are being released, and confirms "This PC is yours again" when a session ends. After a cleanup failure the lamp turns into a double amber line, which High Contrast and colour-blind users can tell apart by shape, and the tab explains how to release a held key or mouse button. Unlike the old bar, the tab follows virtual desktop switches, and screen readers can read its message from the window name, with system alerts when a session starts, stops or ends. The tab uses the Inter typeface, embedded in the executable; its license is in THIRD-PARTY-NOTICES.md, which is now installed with ControlFreak. The glow helper protocol gains a `TONE` command for the amber lamp.

### Fixed

- Clients that validate `structuredContent` against `outputSchema`, such as the official TypeScript SDK, no longer throw on an error result.
- The lamp helper no longer exits when the foreground window has no virtual desktop, such as the taskbar, the desktop or Start. It stays on its current desktop, so shell windows in front no longer cancel input or block new sessions.
- GitHub release notes no longer break each changelog entry mid-sentence. Wrapped entries are joined into single lines when the notes are generated.
- A session can no longer be admitted on an Esc hook that belongs to the previous session and is about to be removed. The hook is confirmed per session.
- An `observation_id` for a window image is checked against that window directly, so a window missing from the 200-entry `list_windows` cap is no longer treated as closed.

### Known limitations

- The Windows executables are not code signed, because that needs a certificate.
- The lamp helper and the session tab still run as separate processes.
- There is no human approval step before a session starts. Run ControlFreak only for MCP clients you trust.
- Same-process binding is by process, not by window: any window of the approved application's process counts while a session lasts.
- Shared host processes other than `explorer.exe` and `ApplicationFrameHost.exe` are not narrowed.
- Input to packaged (UWP) apps relies on the frame's hosted `Windows.UI.Core.CoreWindow` content window. It is covered by synthetic facts only and has not been tried against a real packaged app.
- `PrintWindow` cannot be cancelled once started. After a timeout one abandoned render stays parked until the program answers, and window captures copy the screen meanwhile.
- The new input, capture, key mapping and Stop button behavior is covered by fakes and synthetic fixtures. It is not yet verified on a real desktop with several displays, mixed DPI, non-US keyboard layouts or physical touch and pen input.

## [0.1.0] - 2026-10-18

### Added

- Release files have GitHub build provenance attestations. `gh attestation verify <file> --repo
  Subhransu-De/ControlFreak` confirms that a download, or the `controlfreak.exe` inside the ZIP,
  was built by this repository's release workflow. The recorded source commit is the `main`
  commit the release started from, not the release commit that adds the version number.
- PRIVACY.md describes what the server reads, returns to the MCP client and keeps in memory, and
  what the setup program changes. It is installed with ControlFreak.
- Every tool now publishes an `outputSchema`. The 14 tools that lacked one (`stop_desktop_work`,
  `get_server_status`, `begin_control_session`, `end_control_session`, `capture_display`,
  `capture_region`, `capture_window`, `wait_for_visual_change`, `wait_for_change_since`,
  `capture_visual_baseline`, `read_text_in_region`, `find_text_on_screen`, `wait_for_text`, and
  `wait_for_window`) describe their `structuredContent`. Screenshot and wait results share one
  image-metadata definition. Error results keep the common `error` envelope and are not covered by
  these schemas.

### Changed

- The README is now a short overview. Tool behavior, control sessions and full setup details
  moved to the `docs/` folder.
- The Esc monitor thread now blocks on a message-aware wait (100 ms poll) instead of waking
  about 100 times per second, while still answering keyboard hook callbacks immediately.
- The desktop glow holds a steady opacity instead of breathing when Windows animation effects are
  off; level changes still fade, and the setting is re-read during a session. The session control
  bar paints with the system highlight colors when High Contrast is on.
- Session admission failures now carry their own top-level `error.code` with `error.retryable` and
  `error.details`, instead of `tool_execution_failed` with the raw JSON repeated in a reason. A
  busy desktop is `desktop_in_use` (details: `owner_instance_id`, `retry_after_ms`,
  `session_state`, `remedy`). A shutting-down server is `server_shutting_down`, and a session
  that is still closing is `session_closing`. After Esc or `stop_desktop_work`, every request
  that needs admission returns `desktop_work_stopped` instead of `platform_operation_failed`. An
  unusable control environment is `control_environment_unavailable`, and a broken arbitration
  mechanism is `desktop_arbitration_unavailable`. Indicator failures keep `tool_execution_failed`
  with `details.code` `indicator_unavailable`.

### Fixed

- The manual Pi setup path is `%USERPROFILE%\.pi\agent\mcp.json`. The README previously gave
  `%USERPROFILE%\.config\mcp\mcp.json`, which Pi does not read.
- The session control bar no longer blocks input. Pointer actions aimed at the strip it covers
  now reach the window beneath instead of being refused as a different window, physical clicks
  pass through it, and it never takes focus. It stays excluded from screen capture.
- Base64 encoding of screenshot data now runs on the blocking worker, after the last cancellation
  check, instead of on a Tokio worker. A cancelled request is no longer encoded. The cost appears
  as the `encode_base64` stage in `stages_ms`. PNG output is unchanged: 8-bit RGB.

## [0.1.0-alpha.2] - 2026-10-16

### Added

- `run_actions` runs up to 16 click, scroll, `type_text`, and `press_keys` steps on one approved
  target with one final observation. All step arguments are validated before the first step, the
  batch stops at the first failure, and each step reports `completed`, `failed`, or `skipped`.
- `wait_for_text` repeats OCR discovery in a region until text appears or disappears, bounded to
  30 seconds, and returns the final pass.
- Every screenshot carries an `observation_id`. `click_mouse`, `move_mouse`, `scroll_mouse`, and
  `drag_mouse` accept it with `image_x`/`image_y` as an alternative to display coordinates, and
  the server maps the point through the screenshot's crop and resize.
- Per-stage timings (`stages_ms`) in the `tool_completed` stderr event, `get_server_status`
  recent operations, and response `_meta`, plus a synthetic capture benchmark.

### Changed

- Key and text actions return once the approved window has visibly responded and then stayed
  unchanged for 60 ms, instead of always waiting 150 ms. Windows that never visibly respond keep
  the previous wait. Pointer actions keep their fixed 75 ms wait, which a safe stillness window
  could not shorten.
- One OCR helper process is kept warm between requests and recycled after errors, timeouts,
  cancellation, 100 requests, or a minute idle. `read_text_in_region` takes about half as long.
- Target checks share one virtual-desktop manager instead of activating one per check, and glow
  indicator commands are delivered without poll delays. A click without a screenshot takes about
  a third of its previous time on a live test window.
- Screenshots are downscaled by exact area averaging, which keeps thin text strokes, and encoded
  as RGB PNG at a faster compression level. `resize_method` reports `area_average`.
- `list_windows` returns `truncated` when more than 200 windows exist and issues references only
  for returned windows. `wait_for_window` searches every eligible window.
- A broken desktop-arbitration mutex is reported as `desktop_arbitration_unavailable` without a
  retry hint, instead of as a busy desktop.

### Fixed

- The stop-key hook no longer delays every keystroke on the system by up to 10 ms while a server
  runs.
- `click_text` refuses if another input action ran while its OCR was in progress, and clicks and
  scrolls refuse if the cursor moved after validation.
- An armed session target is no longer evicted from the reference cache by observation churn.
- `wait_for_change_since` refuses a baseline after its display moved, resized, or disappeared.
- `type_text` never ends an input batch on half of a surrogate pair.
- OCR helper output is read with a size limit.

## [0.1.0-alpha.1] - 2026-10-16

### Changed

- MCP discovery probes preserve subsequent classic initialization. Modern inline clients receive
  conservative tool-list cache hints through rmcp 3.5.1.

- Input actions and control sessions now require an opaque `target_ref`; `focus_window` uses
  `window_id`. Sessions refuse target changes, stale window/process identities, incompatible
  observation geometry, desktop changes, focus theft, and pointer hits on other windows.
- Foreground activation now retries within a configurable 100 to 5000 ms budget, reports attempts
  and elapsed time, and distinguishes refusal, settle timeout, invalidation, and integrity errors.

### Added

- Add a navy bar centered near the top of the primary display with white control-session text and physical Esc to stop. Notify the MCP client when the user stops control. Keep the client stop tool, request cancellation and ownership through cleanup; require a user restart to re-arm.
- Cancel queued input and visual waits, track OCR draining, and release only acknowledged held inputs after partial dispatch.

- Structured OCR no-match and ambiguity errors with bounded candidate metadata and recovery
  guidance. Successful text clicks report the matching tier and selected bounds.
- Opt-in tolerant OCR discovery with Unicode, whitespace and punctuation normalization,
  bounded adjacent-line joining, and optional single-character OCR-confusion matching.
  Strict click authorization remains separate from discovery.

- Fixture-tested agent recipes for observation, crop mapping, verification, visual waits,
  partial-input recovery, and atomic dragging. Export published tool descriptions and schemas
  with `--print-tools`.

### Fixed

- Preserve JSONC line comments and line endings during setup and cleanup,
  including lone carriage returns, with jsonc-parser 0.34.0. Keep JSON5
  extensions disabled.
- Report held keys and buttons as input refusals with release guidance, without implying Windows blocked a dispatch.
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
