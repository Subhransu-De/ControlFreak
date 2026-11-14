# Security

ControlFreak is security-sensitive because it observes and controls the Windows desktop.

[PRIVACY.md](PRIVACY.md) describes what desktop data the server reads, returns to the MCP client,
and keeps, and what the setup program changes.

The MCP can capture displays, regions, and windows; run local Windows OCR; retain up to sixteen
short-lived visual baselines in memory; move/click/drag/scroll the pointer; focus existing windows;
and inject keyboard input. Mouse clicks and drags are bounded atomic operations: the server
does not expose raw button-down/button-up tools and attempts release cleanup after partial failures.
The semantic text-click tool rejects zero or ambiguous OCR matches before injecting input.
These are powerful local operations. Run it only for MCP clients you trust, and
report suspected security problems privately through GitHub's security advisory interface instead
of a public issue.

ControlFreak deliberately exposes no application-launch, shell, filesystem, clipboard, registry,
network, or elevation tool. That is a tool-surface boundary rather than a security sandbox: keyboard
input can still reach functionality exposed by an already-running desktop application.

## Process integrity and elevation

The optional Windows setup program and its separate `controlfreak-installer.exe` utility edit only
the explicitly selected clients' MCP configuration. These are installation features, not MCP tools.
Setup creates private configuration backups alongside existing files; those backups may contain
credentials and are intentionally retained after uninstall. Backups preserve access permissions;
EFS-encrypted source configurations are refused before backup creation. Optional uninstall cleanup
retains data it cannot safely change and reports failed rollback as requiring backup recovery. The setup utility reports fixed error
messages rather than configuration contents. It never enables `--allow-elevated` in generated client
entries, starts a server, or changes the server's desktop-control checks. Run setup as the intended
non-elevated user so configuration changes apply to that user.

ControlFreak inherits the Windows access token of the MCP client that starts it. It never requests
elevation itself, but an elevated client would otherwise make ControlFreak elevated too. The server
therefore inspects its token before opening the MCP transport and refuses an elevated token by
default. The refusal is a JSON error on stderr with code `elevated_operation_requires_opt_in`.

Only start an elevated server when the task genuinely requires it:

```powershell
controlfreak --allow-elevated
```

This option is a deliberate trust decision for the whole server lifetime, not permission to cross
Windows integrity boundaries silently. Before every state-changing action, and again immediately
before each input batch, ControlFreak verifies the unlocked interactive `Default` input desktop and
the current target process integrity. A target above the server's integrity is refused with
`higher_integrity_target`; an unverifiable target is refused with `target_integrity_unavailable`.
UAC secure desktop, the lock screen, disconnected sessions, and non-default input desktops remain
blocked even when `--allow-elevated` is present.

Mutating sessions bind one server-issued opaque target reference. Each input batch validates
that window's process creation time, ownership, desktop, foreground and integrity, except as
described below for `switch_virtual_desktop`. Pointer
batches also check the effective top-level window under the mapped point. References retain
window bounds and display layout from observation; incompatible changes invalidate input.
A bounded five-minute cache and Windows destroy-event watcher prevent raw HWND/PID reconstruction
and retire destroyed-window references, including same-process handle reuse. Watcher failure
refuses target resolution. Desktop-switch events retire existing references.

The approved target stands in for its own process, not for the desktop. The foreground window, the keyboard-focus window and the window that would receive a pointer event are each accepted if they are the target itself or belong to the target's process, so the dialogs, popup menus and drop-downs that an approved application opens keep working. A window of any other process is refused, however it is classed or owned. Integrity is checked on the actual window that holds the foreground or receives the event, not only on the target, and a pointer destination must also be a window of the target's process at the child level, so a foreign window embedded in the target's window is refused. A multi-window application's other top-level windows are part of the same process and are accepted. `explorer.exe` hosts the taskbar, the desktop and every File Explorer window, and `ApplicationFrameHost.exe` in System32 hosts the frame of every packaged app, so a target in either accepts only windows in its own owner chain and popup menus, not the taskbar, another folder window or another app's frame. The one addition is the content window that a packaged app hosts inside its own `ApplicationFrameHost.exe` frame: its process differs from the frame's, so a pointer or keyboard destination is accepted as part of the target if it is a window inside the approved frame that belongs to the process of the frame's hosted `Windows.UI.Core.CoreWindow`, with the integrity check on the window actually hit. Top-level windows of that process are not accepted. That content window is recognized by its class and its position as a direct child of the frame, not by package identity, so a process of the same user that parents a window of that class into the frame becomes the accepted process; that is not a privilege boundary, because such a process can inject input itself and the integrity check still applies. A target whose program file cannot be read is treated the same way: failing to know narrows the rule and never widens it.

A foreground window, keyboard-focus window or pointer destination that is wrong refuses that one action. This covers another process, a window of higher integrity, a window whose integrity or identity cannot be read, and a moment with no foreground window. It does not retire the reference and does not end the session, because every action reads the live foreground again; a window that gives the foreground back is usable at once. The refusal is still a refusal: nothing is sent, and a higher-integrity window is still reported as `higher_integrity_target`. Every such refusal carries `retryable`, and a `target_invalidated` refusal also carries a `reason`, so the agent need not parse text; the integrity refusals keep their own error codes and have no `reason`. Changes to the target itself still end the session's approval: a different window, process, owner or desktop, changed bounds or display layout, a destroyed window, an unverifiable integrity level of the target, or a locked or non-default input desktop. A failed `focus_window` (refused or timed out) leaves the target as it was and does not end the session.

One exception to the desktop, foreground and same-process rules is `switch_virtual_desktop` with `steps` above 1. The first shortcut is validated like every other input batch. After it, the approved window is on another desktop and a window of another application, or none, is in front, so the later Ctrl+Win+Arrow shortcuts are sent without the current-desktop, foreground and same-process checks. They still require that the work is not cancelled, the session's target is not invalidated, the unlocked Default input desktop, the approved window being the same live window with unchanged bounds and display layout at an acceptable integrity level, and a readable, acceptable integrity level for the foreground window and the keyboard-focus window, checked again after the other queries; when nothing holds the foreground the shortcut goes to the shell and nothing more is checked. A failed check refuses before that shortcut is sent and keeps the shortcuts already sent as partial input. The keys are fixed by the server and cannot be chosen by the client, and the session ends with the switch.

A double or triple click is sent as separate batches with a 20 ms dwell after each press. The target, foreground, input desktop, integrity, hit window and cursor position are validated again immediately before every batch that carries new input, and a refusal releases only the input that the system acknowledged; cleanup never sends a new press.

Window capture renders the window itself with `PrintWindow`. That call can wait for the target program, so it runs on a watchdog thread with a 2-second deadline, is skipped for windows that Windows flags as hung, and runs one render at a time. A second capture waits for the slot while its render is live, within the same deadline; after a timeout, or behind a render that was already given up on, the screen is copied at once and `capture_method` says `windows_gdi`.

The taskbar, secondary taskbars, the desktop and the Start and Search surfaces can be approved as
targets. Windows does not report a virtual desktop for them because they are on every desktop, so
they are treated as present on every desktop. This exemption needs both a shell window class and a
verified owning image: `explorer.exe` in the Windows directory for the taskbars and desktop, or
`StartMenuExperienceHost.exe`, `SearchHost.exe` or `SearchApp.exe` in their package folder under
`SystemApps` in the Windows directory. The class alone never qualifies, any other window that
lacks a desktop identity is still refused, and every other check, including integrity, still
applies to the window. ControlFreak still has no tool that launches applications.

Focus acquisition uses only documented restoration and foreground APIs with a bounded retry
budget. It validates on every poll and stops on competing foreground ownership. A mismatch
invalidates session approval. Release-only cleanup remains permitted after earlier dispatch.
These checks reduce OS races; out-of-context event notifications and input delivery are not
atomic with validation. Post-action foreground evidence is a sample, not a continuous guarantee.
Clicks and scrolls refuse dispatch if the cursor left the validated point after validation, but
button and wheel events still fire at the live cursor, so a move in the final microseconds is not
excluded. `click_text` refuses if any other input action completed while its OCR ran; changes made
by the application or the user meanwhile are not detected. A window handle reused within the same
process before its destroy event is processed cannot be told apart from the original; this needs
the same full handle value, thread, class, bounds and foreground, and is accepted as residual risk.
Batched actions validate every step's arguments before the first step and recheck the target for
each step exactly as separate calls would.

`get_server_status` and `--print-capabilities` report `server_elevated`, the Windows integrity level,
and whether elevated operation was explicitly allowed. Elevated mutation sessions turn the
turquoise lamp red and label the session tab "Administrator" in every state, including stopping
and cleanup failure; in High Contrast the lamp is thicker instead of red. All other indicator
behavior is unchanged. The capture-excluded lamp runs as a native Rust helper
mode of the same executable, without launching PowerShell or materializing executable scripts. A
per-helper Windows job object contains it when one can be created; if that fails the helper still
starts with cooperative cleanup only, and `get_server_status` reports whether containment is active.

Future platform capabilities must declare their permission requirements and preserve these explicit
boundaries.

For transport diagnosis, the server emits process lifecycle plus tool name, operation ID, status,
duration, and named per-stage durations to stderr. These events deliberately exclude tool arguments, typed text, OCR contents,
window titles, screenshots, and image data. ControlFreak writes no diagnostics to disk by default.
Setting `CONTROLFREAK_GLOW_ERROR_LOG` opts into an append-only log at the given path, recording
indicator helper lifecycle, helper stderr, and error text. That log carries no desktop content.

## User stop and cancellation

A native session tab on every display, a **Stop agent** button on it and a physical Esc latch stop independently
of MCP transport and provider progress. The tab is visible while this server owns
the desktop, including cleanup, then says briefly that control was handed
back. After a cleanup failure it stays, unchanged and with recovery steps,
until the server exits. The tab itself never takes focus or receives clicks, follows
the foreground virtual desktop, and is excluded from screen capture like the
lamp. Only the Stop button takes clicks. It is a standard native button on its own window, so a mouse, touch, pen and accessibility tools (UI Automation Invoke) can press it. The agent cannot: its input tools are injected input, and the button's window procedure accepts mouse and key messages only when their origin is input hardware (`IMO_HARDWARE`): injected input, messages posted by another process and messages whose origin cannot be read are dropped before the button sees them. An accessibility tool that invokes the button through UI Automation sends a click message that needs no origin. A pointer action over the button is also refused, with reason `own_indicator`, because the window under the point belongs to ControlFreak and not to the approved target. The button is invisible in screenshots, so an agent can aim at that spot; the refusal says what it hit. A press is a user stop with the same effects as Esc. The button appears only while the tab says an agent is working or waiting.

The keyboard hook exists only while this server owns a session. It runs on a dedicated thread that does nothing but wait for session changes and service the hook, is installed when a session begins and removed when it ends, and a server that never starts a session never installs one. The tab says "Press Esc to stop" only after the hook is confirmed installed; session admission waits, for at most 2 seconds, until the hook is confirmed, and a session whose hook cannot be installed is refused with the reason (also logged to standard error) instead of running without the Esc stop. The hook examines key identity
and injection flags only to recognize physical Esc. It retains no typed values
or keyboard history, ignores injected Esc, and consumes the stop key's press and
release. Esc outside an owned session is passed through. A user-stop notification
is sent over MCP and its reason remains available in status and refused actions.

The tab and Stop button start with the first screen read or the first session; the Esc keyboard hook only with a session. If they cannot start, sessions are refused with the reason and read-only tools keep working. If the tab fails after it started, the server fails closed: admission closes for the rest of the server's life and the reason is kept in refusals, in `get_server_status` (`safety_indicator.reason`) and on standard error.

Observation tools that read the screen show a brief "reading the screen" state on the tab, which the first screen read starts if no session has. It claims no session, installs no keyboard hook, takes no keys and offers no stop control, because there is nothing to stop. If the tab cannot start for a read, the read still works and the failure is logged to standard error.
Clients may also stop all desktop work or cancel
one request. No MCP call clears the stop latch. User-controlled server restart is
the re-arming procedure, after draining and resolving any cleanup failure.

Cancellation is cooperative. An in-flight native call may finish after stop.
Cleanup bypasses ordinary mutation admission but releases only acknowledged,
unmatched owned inputs. It does not release every key named by a failed request.
An existing held key or button is refused before new input dispatch. Unknown
cleanup closes admission and retains the ownership lease and indicator. Status
reports draining or cleanup failure without claiming to undo application effects.
Stopping does not kill applications, lock Windows, or shut it down. The private
OCR helper retains its pre-existing timeout and owned-process cleanup policy.
