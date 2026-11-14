# Control sessions

Input tools run inside a control session. A session reserves the desktop for one approved window
and shows the user that an agent is in control.

## Lifecycle

- The first input tool starts a session on its own. A one-shot action closes it after 8 seconds.
- For a longer task, call `begin_control_session` with the `target_ref` before the first action,
  and `end_control_session` after the last one. `expected_seconds` sets the hold, up to
  `CONTROLFREAK_GLOW_MAX_HOLD_MS`.
- The hold is a backstop. If a client forgets to end the session, it closes when the hold ends.
- Only one ControlFreak server can own the desktop. A second server gets `desktop_in_use`.
- Observation tools never start a session. Tools that read the screen (captures, OCR and the waits) make the tab show "ControlFreak is reading the screen" for about 1.5 seconds. That state owns nothing: no lamp, no Esc, no Stop button, and it never hides an owned session or a warning.
- The session tab, its Stop button and the Esc monitor start with the first session, not with the server. If they cannot start, the server still runs and read-only tools work; the session is refused with `indicator_unavailable` and the reason, and the next session tries again.
- If the tab fails after it started, admission closes for the rest of the server's life. The reason is kept: refused actions include it, `get_server_status` reports it in `safety_indicator.reason` with `status` `degraded`, and it is logged to standard error as `session_tab_failed`.

## Indicator

While a session is active, a steady turquoise lamp runs along the top edge of every display, and
a tab hangs from it in the middle of each display. Turquoise is the color SAE J3134 recommends for
lamps that show an automated system is driving a car.

| State | Lamp | Tab |
| --- | --- | --- |
| Working | Turquoise | "An agent is using this PC", "ControlFreak is working", "Press Esc to stop" |
| Waiting | Turquoise | The same, with "ControlFreak is waiting for the next action" once no action has run for 1 second |
| Reading the screen | Off | "ControlFreak is reading the screen", "It is not using the keyboard or mouse", for about 1.5 seconds after a capture or OCR call, only when no session is owned |
| Stopping | Unchanged | "Stopping desktop control", "ControlFreak is releasing held keys and mouse buttons" |
| Cleanup failed | Amber double line | "A key or mouse button may still be held" and steps to release it |
| Session ended | Off | "This PC is yours again" for 1 second after `end_control_session` or a hold ending, 2.5 seconds after a stop |

- For the first 3 seconds of a session the tab shows two lines. It then shrinks to one row:
  "An agent is using this PC", "ControlFreak is working" or "ControlFreak is waiting for the next action", and
  "Esc to stop". A session that starts within a minute of the last one ending, such as a run of
  one-shot actions, starts as one row. Every change of message, including from two lines to one
  row, appears at once at full strength; only the tab appearing and disappearing fades.
- After a normal end, "This PC is yours again" is one row. After Esc or a client stop it has a
  second line: "Restart your agent app to use ControlFreak".
- An elevated server shows a red lamp and an "Administrator" label on the tab. The label stays
  while stopping and after a cleanup failure, when the lamp becomes the amber double line.
- After a cleanup failure, the lamp becomes a double amber line and the tab stays, unchanged,
  until the server exits. It says "ControlFreak stopped but could not confirm it released
  everything", then three steps: "Press and release any key or mouse button that seems stuck",
  "If typing or the pointer still misbehaves, sign out or restart Windows" and "Restart your
  agent app to use ControlFreak again". On a display too short for all of it, the tab drops the
  first sentence, then the last step, without rewording the rest.
- Stopping and a possible held key appear at once at full strength, even mid-fade.
- The tab follows the virtual desktop of the foreground window, like the lamp.
- Each tab window is named with the words the tab shows, so screen readers can read it; an
  elevated server's name says "Running as administrator", and after a cleanup failure the name
  keeps the whole warning, even on a short display. A session that introduces itself,
  stopping, a possible held key, and the handback after a stop also raise a system alert once the
  tab is fully on screen. Working and waiting, and quick returns within a minute, only rename the
  window.
- On a narrow display, or with a large Windows text size, the stop key moves under the message
  instead of being cut off. If a tab still does not fit its display, its type steps down from the
  Windows text size until it does, never below 100%. A cleanup-failure warning first gives up
  its first sentence and last step, keeping the type size.
- The lamp and the tab are excluded from screen capture, so screenshots never show them.
- Both are click-through and never take the focus, except for the Stop button described below.
- While the tab says "working" or "waiting", a **Stop agent** button hangs under it. It is a native push button, so a mouse, a finger, a pen and accessibility tools (UI Automation Invoke, which on-screen keyboards, voice control and switch access use) can press it. It never takes focus, follows the display's DPI and the Windows text size, uses the system colors in High Contrast, and is excluded from screen capture. It appears only with a steady tab and never during stopping, a held-key warning, a handback or reading.
- The agent cannot press the button. Mouse and key messages that come from injected input (`SendInput` and its relatives) are dropped before the button sees them, and a pointer action on the button is refused anyway because the window under the point belongs to ControlFreak, not to the approved target. Pressing it is a user stop, with the same effects as Esc.
- The tab says "Press Esc to stop" only while the keyboard hook is confirmed installed. If the hook cannot be installed, the tab leaves Esc out and the Stop button remains.
- Nothing pulses. With Windows animation effects off, the tab and lamp also appear and disappear
  without fading.
- With a High Contrast theme on, both use the theme's colors, and an elevated server's lamp is
  thicker instead of red.
- The tab sits at the top center of each display, where some apps put their own bars, such as the
  Remote Desktop connection bar or a meeting app's sharing bar. Clicks pass through the tab to
  them, but the tab can hide them while a session holds the PC.

| Setting | Effect |
| --- | --- |
| `CONTROLFREAK_GLOW_HOLD_MS` | Overrides the default session holds. |
| `CONTROLFREAK_GLOW_MAX_HOLD_MS` | Upper bound for every hold. |
| `CONTROLFREAK_GLOW_ERROR_LOG` | Appends indicator errors to this file. Off by default. |

The lamp follows the virtual desktop of the foreground window. Windows reports no desktop for the taskbar, the desktop and the Start surfaces, which belong to every desktop. The lamp helper and the tab then stay where they are instead of failing, so a shell window in front never ends the helper or blocks a session.

The indicator is event driven. An idle server has no polling loop for it: the tab thread, the Esc monitor and the stop notification sleep until a session, stop, observation or Windows message wakes them, with a slow safety check. While a session is owned, the session worker looks at the lock state and the input desktop every 50 ms, because ending the session on a lock or desktop switch during a hold is a safety invariant. A helper that cannot be stopped is retried with a growing delay and abandoned after about a minute, with the reason on standard error.

## Stopping

Control stops when:

- the user presses the physical Esc key during a session. Injected Esc is ignored, and the key
  press does not reach the application. The keyboard hook exists only while a session is owned, on a thread of its own that does nothing else; it is removed when the session ends, and a server that never starts a session never installs one.
- the user presses the **Stop agent** button on the tab with a mouse, touch or pen, or through an accessibility tool. Injected clicks are ignored.
- a client calls `stop_desktop_work`.

After a stop:

- New input is refused with `desktop_work_stopped` for the rest of the server's life.
  `begin_control_session` and `end_control_session` cannot clear it.
- Observation tools, `get_server_status` and session cleanup still work.
- Input already sent to an application cannot be undone.
- On a user stop, the server sends `notifications/controlfreak/session_stopped` with
  `reason="user_stop"` and `retry_action=false`. It advertises this as
  `experimental.controlfreak/user-stop`. `get_server_status` reports `stop_reason="user_stop"`.
- Agents must not restart the server or resume desktop actions without the user's permission.

`get_server_status.stop_state`:

| Value | Meaning |
| --- | --- |
| `ready` | Accepting work. |
| `draining` | Stopped. In-flight work is finishing. |
| `stopped` | Stopped. Nothing is running. |
| `cleanup_failed` | Held keys or buttons could not be released for certain. The desktop stays reserved. |

To cancel a single request without stopping the server, send the MCP `notifications/cancelled`
notification.

## Elevated mode

ControlFreak runs with the Windows token of the MCP client that starts it. If that token is
elevated, the server refuses to start and reports `elevated_operation_requires_opt_in` on
standard error.

To allow it, add `--allow-elevated` to the server command in the client configuration:

```json
{ "command": "C:\\path\\to\\controlfreak.exe", "args": ["--allow-elevated"] }
```

Even with this option, ControlFreak never acts on the lock screen, the UAC prompt, another input
desktop, or a window with a higher integrity level. See [SECURITY.md](../SECURITY.md).
