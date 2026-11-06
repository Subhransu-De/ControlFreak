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
- Observation tools never start a session.

## Indicator

While a session is active, a steady turquoise lamp runs along the top edge of every display, and
a tab hangs from it in the middle of each display. Turquoise is the color SAE J3134 recommends for
lamps that show an automated system is driving a car.

| State | Lamp | Tab |
| --- | --- | --- |
| Working | Turquoise | "An agent is using this PC", "ControlFreak is working", "Press Esc to stop" |
| Waiting | Turquoise | The same, with "ControlFreak is waiting" once no action has run for 1 second |
| Stopping | Unchanged | "Stopping desktop control", "ControlFreak is releasing held keys and mouse buttons" |
| Cleanup failed | Amber double line | "A key or mouse button may still be held" and steps to release it |
| Session ended | Off | "This PC is yours again" for 2.5 seconds |

- For the first 3 seconds of a session the tab shows two lines. It then shrinks to one row:
  "An agent is using this PC", "ControlFreak is working" or "ControlFreak is waiting", and
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
- Both are click-through and never take the focus.
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

## Stopping

Control stops when:

- the user presses the physical Esc key during a session. Injected Esc is ignored, and the key
  press does not reach the application.
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
