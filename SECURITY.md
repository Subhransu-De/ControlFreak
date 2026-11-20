# Security

ControlFreak is security-sensitive because it observes and controls the Windows desktop.

[PRIVACY.md](PRIVACY.md) describes what desktop data the server reads, returns to the MCP client,
and keeps, and what the setup program changes.

The MCP can capture displays, regions, and windows; run local Windows OCR; retain up to sixteen
short-lived visual baselines in memory; move/click/drag/scroll the pointer; focus existing windows;
and inject keyboard input. Mouse clicks and drags are bounded atomic operations: the server
does not expose raw button-down/button-up tools and attempts release cleanup after partial failures.
The semantic text-click tool rejects zero or ambiguous OCR matches before injecting input.
The MCP can also read the UI Automation controls of one window; that tool is read-only and is
described below.
These are powerful local operations. Run it only for MCP clients you trust, and
report suspected security problems privately through GitHub's security advisory interface instead
of a public issue.

ControlFreak deliberately exposes no application-launch, shell, filesystem, clipboard, registry,
network, or elevation tool. That is a tool-surface boundary rather than a security sandbox: keyboard
input can still reach functionality exposed by an already-running desktop application.

## Tool profiles

`--profile observe` and `--tools` narrow the tools a server serves (see [docs/install.md](docs/install.md#restrict-the-tools)). Discovery, dispatch and `run_actions` steps all check the same profile, so a hidden tool cannot be called by name or through a batch, and is refused before any session, desktop lock or indicator. A malformed profile flag stops the server at startup; it never serves every tool instead. No tool changes the profile: only the client's launch arguments do.

A profile narrows the MCP tool surface. It is not a sandbox. The observe profile still captures the screen and runs OCR, so an agent can read anything visible, with the same privacy rules as any other capture (see [PRIVACY.md](PRIVACY.md)). A profile that serves `press_keys` or `type_text` can still operate any application that the approved window can reach, and the profile does not limit other tools that the MCP client offers. Anyone who can edit the client's configuration can remove the flags.

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
batches also check the effective top-level window under the mapped point. References retain the display layout from observation and the window bounds from observation or from the last input check. A changed display layout invalidates input. Changed window bounds alone do not, and a window that only moved keeps its reference: see the moved-target rule below.
A bounded five-minute cache and Windows destroy-event watcher prevent raw HWND/PID reconstruction
and retire destroyed-window references, including same-process handle reuse. Watcher failure
refuses target resolution. Desktop-switch events retire existing references.

The approved target stands in for its own process, not for the desktop. The foreground window, the keyboard-focus window and the window that would receive a pointer event are each accepted if they are the target itself or belong to the target's process, so the dialogs, popup menus and drop-downs that an approved application opens keep working. A window of any other process is refused, however it is classed or owned. Integrity is checked on the actual window that holds the foreground or receives the event, not only on the target, and a pointer destination must also be a window of the target's process at the child level, so a foreign window embedded in the target's window is refused. A multi-window application's other top-level windows are part of the same process and are accepted. `explorer.exe` hosts the taskbar, the desktop and every File Explorer window, and `ApplicationFrameHost.exe` in System32 hosts the frame of every packaged app, so a target in either accepts only windows in its own owner chain and popup menus, not the taskbar, another folder window or another app's frame. The one addition is the content window that a packaged app hosts inside its own `ApplicationFrameHost.exe` frame: its process differs from the frame's, so a pointer or keyboard destination is accepted as part of the target if it is a window inside the approved frame that belongs to the process of the frame's hosted `Windows.UI.Core.CoreWindow`, with the integrity check on the window actually hit. Top-level windows of that process are not accepted. That content window is recognized by its class and its position as a direct child of the frame, not by package identity, so a process of the same user that parents a window of that class into the frame becomes the accepted process; that is not a privilege boundary, because such a process can inject input itself and the integrity check still applies. A target whose program file cannot be read is treated the same way: failing to know narrows the rule and never widens it.

A foreground window, keyboard-focus window or pointer destination that is wrong refuses that one action. This covers another process, a window of higher integrity, a window whose integrity or identity cannot be read, and a moment with no foreground window. It does not retire the reference and does not end the session, because every action reads the live foreground again; a window that gives the foreground back is usable at once. The refusal is still a refusal: nothing is sent, and a higher-integrity window is still reported as `higher_integrity_target`. Every such refusal carries `retryable`, and a `target_invalidated` refusal also carries a `reason`, so the agent need not parse text; the integrity refusals keep their own error codes and have no `reason`. Changes to the target itself still end the session's approval: a different window, process, owner or desktop, a changed display layout, a destroyed window, an unverifiable integrity level of the target, or a locked or non-default input desktop. A failed `focus_window` (refused or timed out) leaves the target as it was and does not end the session.

A target that only moved or was resized keeps its approval. When the window handle, process ID, process creation time, thread, owner, class and virtual desktop are all unchanged, the display layout is unchanged and the integrity checks pass, but the window's bounds differ, that one action is refused with reason `target_moved` and nothing is sent. The reference then takes the new bounds, so an action that starts after the refusal is checked against where the window is now, and every later move is refused once again. An action that started before the new bounds were taken, such as one queued behind the action that found the move, is refused as well, because it may hold coordinates for the old position. The refusal makes the agent observe again before it acts: coordinates chosen for the old position are not sent for that action, and an image of the window taken before the move no longer maps points, because its recorded window bounds differ. This does not widen what may receive input: every action still checks the identity, the foreground, the destination under the point and integrity as above. A move that comes with any other change of the target, or with a changed display layout, still ends the approval.

One exception to the desktop, foreground and same-process rules is `switch_virtual_desktop` with `steps` above 1. The first shortcut is validated like every other input batch. After it, the approved window is on another desktop and a window of another application, or none, is in front, so the later Ctrl+Win+Arrow shortcuts are sent without the current-desktop, foreground and same-process checks. They still require that the work is not cancelled, the session's target is not invalidated, the unlocked Default input desktop, the approved window being the same live window with an unchanged display layout at an acceptable integrity level (a move refuses the shortcut with `target_moved`, as above), and a readable, acceptable integrity level for the foreground window and the keyboard-focus window, checked again after the other queries; when nothing holds the foreground the shortcut goes to the shell and nothing more is checked. A failed check refuses before that shortcut is sent and keeps the shortcuts already sent as partial input. The keys are fixed by the server and cannot be chosen by the client, and the session ends with the switch.

A double or triple click is sent as separate batches with a 20 ms dwell after each press. The target, foreground, input desktop, integrity, hit window and cursor position are validated again immediately before every batch that carries new input, and a refusal releases only the input that the system acknowledged; cleanup never sends a new press.

`find_elements` reads the UI Automation tree of one window and changes nothing: it invokes no pattern, reads no value and claims no session, so the boundaries above are unchanged. Clicking a control it found is an ordinary pointer action with every check above. Its element references are accepted only as the root of a later query and by no input tool. Semantic actions (invoke, toggle, set value) are not offered; they would bypass the foreground and hit-test model above and need their own validation rule first. The query reads only the subtree of the window that its `target_ref` resolves to, after the same identity checks as any reference: it never walks to the window's parent or siblings, and it skips elements of any process other than the window's own (and, for a packaged app's frame, the app's content process), with their subtrees. An element reference is bound to that `target_ref`, the process creation time and the element's UI Automation runtime ID, expires after 2 minutes, and is refused when any of these changed. The window is resolved again after the walk, and a window that closed or changed meanwhile fails the query. Windows does not let a process read the UI Automation tree of a higher-integrity process, so such a query fails or reads little. Provider calls are cross-process calls into the application, so they run in a helper process of the server's own executable (`--uia-helper`) on one dedicated thread, never on the server's async workers. The helper stops at the request's deadline, answers with what it read and exits if a provider call does not return, and the server kills a helper that has not answered 3 seconds after the deadline. A cut-short query is reported as partial or as `accessibility_unavailable`, never as an empty tree, and ending the helper is not evidence that the application recovered. A password field's name is withheld, and so is the name of an edit control whose provider does not say whether it is one. Two residual risks are accepted: a provider that moves an element to another window of the same process between two calls of one walk can make the walk continue among that window's elements, and a provider can give a new element the runtime ID of a destroyed one, so a reference can then name the new element. Both stay inside the target's process, which is already one trust domain for input, and neither can send input.

Window capture renders the window itself with `PrintWindow`. That call can wait for the target program, so it runs on a watchdog thread with a 2-second deadline, is skipped for windows that Windows flags as hung, and runs one render at a time. A second capture waits for the slot while its render is live, within the same deadline; after a timeout, or behind a render that was already given up on, the screen is copied at once and `capture_method` says `windows_gdi`.

The taskbar, secondary taskbars, the desktop and the Start and Search surfaces can be approved as
targets. Windows does not report a virtual desktop for them because they are on every desktop, so
they are treated as present on every desktop. This exemption needs both a shell window class and a
verified owning image: `explorer.exe` in the Windows directory for the taskbars and desktop, or
`StartMenuExperienceHost.exe`, `SearchHost.exe` or `SearchApp.exe` in their package folder under
`SystemApps` in the Windows directory. The class alone never qualifies, any other window that
lacks a desktop identity is still refused, and every other check, including integrity, still
applies to the window. ControlFreak still has no tool that launches applications.

Focus acquisition uses only documented restoration and foreground APIs with a bounded retry budget. It validates on every poll and stops on competing foreground ownership. When Windows refuses the server's own `SetForegroundWindow`, because user input arrived after the server started, the server starts its own executable once per `focus_window` as a helper process in a kill-on-close job. The helper receives only the window handle and the owning process ID that the server validated, refuses the call if the handle now belongs to another process, makes the same call, and exits with its answer. The server waits only for what is left of the request's `timeout_ms`, at most 1.5 seconds, and terminates the helper at once on cancellation or a user stop; a helper whose answer is lost leaves delivery reported as unknown. It sends no input, writes nothing to stdout and has no other mode. All target, desktop and integrity checks stay in the server and run again on every poll after the helper's answer. A mismatch invalidates session approval. Release-only cleanup remains permitted after earlier dispatch. These checks reduce OS races; out-of-context event notifications and input delivery are not atomic with validation. Post-action foreground evidence is a sample, not a continuous guarantee. Clicks, scrolls and drags compare the live cursor with the point the client requested, not with where the cursor happened to be after the move: every approach step is read back after `SetCursorPos`, because Windows keeps the cursor inside another program's `ClipCursor` rectangle and still reports success, and a cursor it left elsewhere refuses the action with `pointer_not_placed` before anything is pressed. A cursor that moves away afterwards refuses each button or wheel batch with `pointer_moved`, and a drag checks both its start before the press and its end before the release; a drag that is refused after the press still releases the button where the cursor is. Button and wheel events still fire at the live cursor, so a move in the final microseconds is not excluded. `click_text` refuses if any other input action completed while its OCR ran; changes made by the application or the user meanwhile are not detected. A window handle reused within the same process before its destroy event is processed cannot be told apart from the original; this needs the same full handle value, thread, class, bounds and foreground, and is accepted as residual risk. Batched actions validate every step's arguments before the first step and recheck the target for each step exactly as separate calls would.

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
UI Automation names, window titles, screenshots, and image data. ControlFreak writes no diagnostics to disk by default.
Setting `CONTROLFREAK_GLOW_ERROR_LOG` opts into an append-only log at the given path, recording
indicator helper lifecycle, helper stderr, and error text. That log carries no desktop content.

## User stop and cancellation

A native session tab on every display, a **Stop agent** button on it and a physical Esc latch stop independently
of MCP transport and provider progress. The tab is visible while this server owns
the desktop, including cleanup, then says briefly that control was handed
back. After the user's own stop it stays with an **Allow again** control instead (see below). After a cleanup failure it stays, unchanged and with recovery steps,
until the server exits. The tab itself never takes focus or receives clicks, follows
the foreground virtual desktop, and is excluded from screen capture like the
lamp. Only the Stop button and Allow again take clicks. The Stop button is a standard native button on its own window, so a mouse, touch, pen and accessibility tools (UI Automation Invoke) can press it. The agent cannot: its input tools are injected input, and the button's window procedure accepts mouse and key messages only when their origin is input hardware (`IMO_HARDWARE`): injected input, messages posted by another process and messages whose origin cannot be read are dropped before the button sees them. A press is tracked by that same window procedure, after the origin check: it starts on the button and counts when the pointer is released on it. An accessibility tool that invokes the button through UI Automation sends a click message that needs no origin. A pointer action over the button is also refused, with reason `own_indicator`, because the window under the point belongs to ControlFreak and not to the approved target. The button is invisible in screenshots, so an agent can aim at that spot; the refusal says what it hit. A press is a user stop with the same effects as Esc. The button appears only while the tab says an agent is working or waiting.

The keyboard hook exists only while this server owns a session. It runs on a dedicated thread that does nothing but wait for session changes and service the hook, is installed when a session begins and removed when it ends, and a server that never starts a session never installs one. The tab says "Press Esc to stop" only after the hook is confirmed installed and only while Esc stops; session admission waits, for at most 2 seconds, until the hook is confirmed, and a session whose hook cannot be installed is refused with the reason (also logged to standard error) instead of running without the Esc stop. The hook examines key identity
and injection flags only to recognize physical Esc. It retains no typed values
or keyboard history, ignores injected Esc, and consumes the stop key's press and
release while the agent is working: from the moment an action's indicator level is acknowledged, before any of its input is sent, and for as long as the tab still says "Esc to stop" after it. While the tab says the agent is waiting for the next action, a physical Esc is passed through to the application and does not stop; the Stop button still does. Esc outside an owned session is passed through. A user-stop notification
is sent over MCP and its reason remains available in status and refused actions.

The tab and Stop button start with the first screen read or the first session; the Esc keyboard hook only with a session. If they cannot start, sessions are refused with the reason and read-only tools keep working. If the tab fails after it started, the server fails closed: admission closes for the rest of the server's life and the reason is kept in refusals, in `get_server_status` (`safety_indicator.reason`) and on standard error. One reading is an exception while no session is owned: Windows reporting no display, or a display with no area, only makes the tab and the hidden lamp wait for their next check. During a session it still fails closed, and the lamp never confirms it is visible without a display.

Observation tools that read the screen show a brief "reading the screen" state on the tab, which the first screen read starts if no session has. It claims no session, installs no keyboard hook, takes no keys and offers no stop control, because there is nothing to stop. If the tab cannot start for a read, the read still works and the failure is logged to standard error.
Clients may also stop all desktop work or cancel
one request. No MCP call clears the stop latch.

### Allowing input again

Only the user re-arms input, in one of two ways. Restarting the server from the MCP client works after every stop, once any cleanup failure is resolved. After the user's own stop (physical Esc or the Stop button) the session tab also offers **Allow again**. Pressing it reopens admission only when the stop was the user's, everything admitted before it has finished and its session is released, no cleanup failed and the server did not close admission for a failure of its own, such as a failed session tab. A lamp helper that failed while the stop drained does not block it, as it does not close admission without a stop either: the next session starts and checks the indicator like any other. A client stop (`stop_desktop_work`, even one that follows the user's stop), a shutdown, a failure of the tab and a possibly held key stay permanent until the restart. A re-arm starts a new stop epoch with no registration left from before the stop, and resumes nothing: the next action is admitted as a new session with the lamp, the tab and the confirmed Esc hook.

Neither control can be closed by another process: both ignore `WM_CLOSE`. Allow again uses a stricter origin rule than the Stop button, because it gives control back instead of taking it away. It is a window of ControlFreak's own class, not a `BUTTON`, so UI Automation offers no Invoke pattern for it and the accessibility default action does nothing. It ignores `BM_CLICK`, which any process on the desktop can send, including a terminal or application the agent drives, and it ignores keys; it never has the focus. It counts a left-button press and release on itself only when Windows reports the origin of both as input hardware (`IMO_HARDWARE`) and the device as a mouse, touch screen, pen or touchpad, which a person's press gives. Injected input, messages sent or posted by another process and messages with an unreadable origin are dropped. A pointer action of the agent on it is refused as `own_indicator`, like one on the Stop button. The consequence is that screen-reader, voice-control and switch-access users cannot use Allow again; the tab tells them, in its words, window name and system alert, to restart the agent app instead. Windows also reports a hardware origin for input that an application with the UIAccess manifest attribute injects. Such applications must be signed and installed in a protected folder by an administrator, and are assistive technology the user chose; the device-type check refuses that input when Windows names no pointing device for it, which ControlFreak cannot verify for every such application. A device driver that emulates input hardware would report a hardware origin too; installing one needs administrator rights, and the agent cannot approve an elevation prompt: it appears on the secure desktop or at a higher integrity level, where ControlFreak refuses input.

Cancellation is cooperative. An in-flight native call may finish after stop.
Cleanup bypasses ordinary mutation admission but releases only acknowledged,
unmatched owned inputs. It does not release every key named by a failed request.
An existing held key or button is refused before new input dispatch. A held Shift, Ctrl, Alt or Win key that the request does not name is refused the same way (`key_held`), for every key press, typed character, click, drag and scroll, because Windows would combine it with the injected input (Shift+Delete, Alt+Tab, a Ctrl-click). Releases and cleanup are never refused, and the server's own held modifiers do not trigger the check.
Unknown cleanup closes admission and retains the ownership lease and indicator. Status
reports draining or cleanup failure without claiming to undo application effects.
Stopping does not kill applications, lock Windows, or shut it down. The private
OCR helper retains its pre-existing timeout and owned-process cleanup policy.
