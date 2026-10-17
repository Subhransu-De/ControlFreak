# Tools

ControlFreak exposes 27 MCP tools. Every tool publishes an input schema and an `outputSchema`.
Export the full contract with `controlfreak --print-tools`.

## Tool list

Observation tools read the desktop and never need a control session. Input tools change the
desktop and run inside a [control session](sessions.md).

| Tool | Kind | What it does |
| --- | --- | --- |
| `list_displays` | Observe | Active displays with IDs, primary flag and bounds in physical pixels. |
| `capture_display` | Observe | Screenshot of a display. |
| `capture_region` | Observe | Screenshot of a rectangle, at native resolution unless `max_width` is set. |
| `capture_window` | Observe | Screenshot of a window's visible frame, without focusing it. |
| `list_windows` | Observe | Visible titled top-level windows, foreground first. At most 200. |
| `wait_for_window` | Observe | Wait for a window that matches title, class or process. |
| `list_virtual_desktops` | Observe | Windows grouped by virtual desktop, with the current one marked. |
| `read_text_in_region` | Observe | OCR lines and words with bounds. |
| `find_text_on_screen` | Observe | Find text with OCR. Discovery only. |
| `wait_for_text` | Observe | Repeat OCR until text appears or disappears. |
| `capture_visual_baseline` | Observe | Save how a region looks now, for a later wait. |
| `wait_for_change_since` | Observe | Wait until a region differs from its baseline and settles. |
| `wait_for_visual_change` | Observe | Wait until a region changes from how it looks now and settles. |
| `get_server_status` | Observe | Version, privileges, session and stop state, recent operations. |
| `begin_control_session` | Session | Reserve the desktop for one approved window. |
| `end_control_session` | Session | Release the desktop after pending actions finish. |
| `stop_desktop_work` | Session | Cancel all desktop work until the server restarts. |
| `focus_window` | Input | Restore and focus a window, then verify it is in the foreground. |
| `move_mouse` | Input | Move the pointer. |
| `click_mouse` | Input | Click, with optional modifiers. |
| `drag_mouse` | Input | Press, move and release in one call. |
| `scroll_mouse` | Input | Scroll vertically or horizontally at a point. |
| `press_keys` | Input | Send a key or key chord. |
| `type_text` | Input | Type Unicode text without the clipboard. |
| `click_text` | Input | Click a unique line of OCR text. |
| `switch_virtual_desktop` | Input | Switch one virtual desktop left or right. |
| `run_actions` | Input | Up to 16 clicks, scrolls, keys and text steps in one call. |

## Targets

- Window IDs are opaque and issued by the server. Do not build them from HWNDs or PIDs.
- Get an ID from `list_windows` or a screenshot result, and pass it as `target_ref` to
  `begin_control_session` and to every input tool. `focus_window` takes it as `window_id`.
- One session approves one target. End the session before approving another.
- A screenshot or OCR result includes `target_ref` only when the foreground window stayed the
  same during the capture. A null `target_ref` does not approve input.
- References expire 5 minutes after the last matching observation.
- Before each input, the server checks that the target is the same process and window, is in the
  foreground, is on the current desktop, has the same bounds and display layout, and does not
  run at a higher integrity level. Pointer input must land on the approved window. Owned popups
  are not approved automatically.
- If a check fails, the action is refused and the session target is cleared. Observe again and
  approve the new target. Do not repeat the input or refocus on your own.
- Moving, restoring or switching desktops invalidates a target. Capture again and approve the new
  reference.

`focus_window`:

- `timeout_ms` defaults to 1000 and accepts 100 to 5000.
- It never sends keyboard or mouse input to get focus.
- Errors: `activation_refused`, `settle_timeout`, `target_invalidated`, and the integrity errors.

## Coordinates

- Display coordinates are display-local physical pixels with a `display_id`.
- Every screenshot has an `observation_id`. Pointer tools accept `observation_id` with `image_x`
  and `image_y` in that screenshot's pixels. The server maps the point through the crop and
  resize. Give one form per point.
- Observation IDs keep geometry only, expire after 5 minutes, and at most 64 are kept.
- Screenshots are downscaled by area averaging (`resize_method: area_average`).

## Text (OCR)

OCR uses the Windows OCR engine on the computer. It needs a Windows OCR language pack. It gives
no confidence scores.

| Tool | Matching | Gives input approval |
| --- | --- | --- |
| `click_text` | `exact` (default) or substring with `exact_match=false` | Yes, only for one unique match |
| `find_text_on_screen` | `match_mode`: `substring` (default), `exact`, `tolerant` | No |
| `wait_for_text` | Same as `find_text_on_screen`, until `appears` or `disappears` | No |

`click_text`:

- Zero matches return `ocr_no_match`. More than one returns `ocr_ambiguous_match`. Neither sends
  input or falls back to coordinates.
- `error.details` lists up to 20 candidates of up to 256 characters each, with `candidate_count`
  and truncation flags.
- To recover, narrow the region, use the recognized text as the query, or use discovery.

`tolerant` discovery tries these tiers in order and stops at the first tier with results:

1. Trimmed exact match.
2. Normalized match: NFKC, collapsed whitespace, straight quotes, ASCII hyphens.
3. Normalized match across two consecutive lines.
4. With `ocr_confusions=true` only: one `0/O/o` or `1/I/i/l/|` substitution in a 4 to 64
   character label.

Tolerant results never approve a click. Use them to pick a strict query and region.

`wait_for_text`:

- `timeout_ms` defaults to 5000. The maximum is 30000.
- Returns `satisfied`, `timed_out`, `attempts`, `elapsed_ms` and the last matches.
- Each pass is a full OCR pass, so a smaller region is faster.

OCR text and checkbox glyphs do not prove the state of a control.

## Waiting for changes

- `capture_visual_baseline` then `wait_for_change_since`: save a region before an action and wait
  for its effect afterwards. Up to 16 baselines, each kept for 5 minutes.
- `wait_for_visual_change`: wait for a region to change from how it looks at call time.
- A timeout is a normal result, not an error.

## Action batches

`run_actions` runs up to 16 `click`, `scroll`, `type_text` and `press_keys` steps on one target:

- All step arguments are checked before the first step. One bad step refuses the whole batch.
- Each step gets the same target checks as the single tool.
- The batch stops at the first failure. `action.steps` reports each step as `completed`,
  `failed` or `skipped`.
- Only the last step returns a screenshot.

Use it for a known sequence, such as filling a form. Observe between calls when a later step
depends on what an earlier step shows.

## Action results

| Field | Values |
| --- | --- |
| `status` | `completed`, `completed_unverified` (input sent, screenshot failed), `partially_sent`, `unknown`, `not_started` |
| `input.input_outcome` | `not_started`, `input_sent`, `partially_sent`, `unknown` |
| `input.sent_events` | Count of input events Windows accepted |
| `input.cleanup` | `not_needed`, `succeeded`, `unknown` |
| `input.target_remained_foreground` | `true`, `false` or `null` after the action |
| `observation_status` | `succeeded`, `failed`, `not_attempted` |
| `effect_verification` | Always `unverified` |
| `retry_action` | Always `false` |

- Partial and uncertain delivery are not errors. Read `status`, not only `isError`.
- Accepted input does not prove the application did what was asked. Check the screenshot.
- After partial or unknown delivery, observe before you act again. Do not replay the whole
  action.

## Session errors

When an input tool cannot start a session, `error.code` says why:

| Code | Meaning | Retryable |
| --- | --- | --- |
| `desktop_in_use` | Another ControlFreak server owns the desktop. `details` has `retry_after_ms`. | Yes |
| `session_closing` | The previous session is still closing. | Yes |
| `server_shutting_down` | The server is shutting down. | No |
| `desktop_work_stopped` | The user stopped control, or a client called `stop_desktop_work`. | No |
| `control_environment_unavailable` | The desktop cannot be controlled, for example while it is locked. | No |
| `desktop_arbitration_unavailable` | Desktop ownership could not be checked. | No |

## Limits

- Windows 10 version 2004 or later, or Windows 11, on x86-64.
- No UI Automation. The agent works from screenshots, OCR and coordinates.
- Protected video and HDR content may not capture correctly.
- No tools for shell commands, launching applications, files, clipboard, registry, network or
  elevation.
