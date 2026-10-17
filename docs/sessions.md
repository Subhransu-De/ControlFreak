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

While a session is active:

- A blue glow surrounds every display. It is bright while an action runs and dim between actions.
- A navy bar at the top of the primary display reads "ControlFreak is controlling your system
  (Use Esc to stop)".
- Both are excluded from screen capture, so screenshots never show them.
- Both are click-through and never take the focus.
- With Windows animation effects off, the glow does not pulse.
- With a High Contrast theme on, the bar uses the system highlight colors.
- An elevated server shows a red glow instead of a blue one.

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
