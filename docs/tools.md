# Tools

ControlFreak exposes 27 MCP tools. Every tool publishes a title, an input schema, an `outputSchema` and annotations. Export the full contract with `controlfreak --print-tools`.

- Tools are listed in the order an agent needs them. `stop_desktop_work` is last.
- Every input tool is annotated `destructiveHint: true` and `openWorldHint: true`. `stop_desktop_work` is also destructive, because it closes input permanently. Use `end_control_session` to finish normally.
- Every `outputSchema` accepts both the success result and the error result of its tool, so clients that validate `structuredContent` (such as the official TypeScript SDK) can read errors. Refused input actions carry their progress fields next to `error`.

## Tool list

Observation tools read the desktop and never need a control session. Input tools change the
desktop and run inside a [control session](sessions.md).

| Tool | Kind | What it does |
| --- | --- | --- |
| `list_displays` | Observe | Active displays with IDs, primary flag and bounds in physical pixels. |
| `capture_display` | Observe | Screenshot of a display. |
| `capture_region` | Observe | Screenshot of a rectangle, 1280 px wide unless `max_width` or `native` is set. |
| `capture_window` | Observe | Screenshot of a window's visible frame, without focusing it. The window renders itself (`capture_method` `windows_print_window`), so windows covering it do not appear; if it cannot, the screen is copied (`windows_gdi`). |
| `list_windows` | Observe | Visible titled windows, including owned dialogs (with an `owner` field), foreground first. At most 200. |
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
| `stop_desktop_work` | Session | Emergency stop: cancel all desktop work and close input until the server restarts. |
| `focus_window` | Input | Restore and focus a window, then verify it is in the foreground. |
| `move_mouse` | Input | Move the pointer. |
| `click_mouse` | Input | Click, with optional modifiers. |
| `drag_mouse` | Input | Press, move and release in one call. The pointer travels in several steps over at least 120 ms, holding the button for 20 ms before and after the move. |
| `scroll_mouse` | Input | Scroll vertically or horizontally at a point. |
| `press_keys` | Input | Send a key or key chord. Letters, digits, F1 to F12, punctuation, the numpad, system keys and media keys; see Keys below. |
| `type_text` | Input | Type Unicode text without the clipboard. Line breaks and tabs are pressed as Enter and Tab. |
| `click_text` | Input | Click the unique OCR text that matches, at the matched words. |
| `switch_virtual_desktop` | Input | Switch one virtual desktop left or right. |
| `run_actions` | Input | Up to 16 clicks, scrolls, keys, text and wait steps in one call. |

## Targets

- Window IDs are opaque and issued by the server. Do not build them from HWNDs or PIDs.
- Get an ID from `list_windows` or a screenshot result, and pass it as `target_ref` to
  `begin_control_session` and to every input tool. `focus_window`, `capture_window` and `wait_for_window`
  take it as `window_id`, and also accept `target_ref` as an alias. Giving both is fine only when they
  are equal.
- `switch_virtual_desktop` takes `target_ref` as an optional field. When it is missing, the session's approved window is used, or the foreground window when no session has one. The switch ends the session, because the approved window is no longer on screen. Begin a new session on a window of the new desktop.
- One session approves one target. End the session before approving another.
- A screenshot or OCR result includes `target_ref` only when the foreground window stayed the
  same during the capture. A null `target_ref` does not approve input.
- References expire 5 minutes after the last matching observation.
- Before each input, the server checks that the target is the same process and window, is on the
  current desktop, has the same bounds and display layout, and does not run at a higher integrity
  level, and that the target or a window of its own process is in the foreground.
- The target's own dialogs, popup menus and drop-downs belong to the same process, so they may
  hold the foreground and receive pointer input without a new session. A window of another
  process never does. For `explorer.exe` targets (File Explorer, the taskbar, the desktop) only
  the target's own dialogs and popup menus count, not the rest of the shell.
- If the foreground or the pointer destination belongs to another process, that action is
  refused and the session keeps its target. Bring the target back with `focus_window` and try
  again; nothing else needs to be observed.
- If the window itself changes (a different window or process, a move or resize, another virtual
  desktop, destruction), the action is refused and the session target is cleared. Observe again
  and approve the new target. Do not repeat the input on your own.
- Moving, restoring or switching desktops invalidates a target. Capture again and approve the new
  reference.
- `list_windows` also lists visible owned windows such as dialogs. Their `owner` field is the
  target reference of the window that owns them. `focus_window` and `capture_window` accept them,
  and a dialog can be approved as a target of its own.
- The taskbar, secondary taskbars, the desktop and the Start and Search surfaces can be approved
  too, even though Windows reports no virtual desktop for them: they are treated as present on
  every desktop. They are recognized by window class and by the program that owns them, never by
  class alone. A screenshot taken while one of them is in front returns its `target_ref`.

`focus_window`:

- Call it after `begin_control_session`, and again when a window from another process took the foreground.
- `timeout_ms` defaults to 1000 and accepts 100 to 5000.
- It never sends keyboard or mouse input to get focus.
- Errors: `activation_refused`, `settle_timeout`, `target_invalidated`, and the integrity errors.

## Coordinates

- Display coordinates are display-local physical pixels. `display_id` is optional everywhere. It defaults to the primary display, and `"primary"` names it explicitly. Call `list_displays` only to reach another display.
- Explicit captures (`capture_display`, `capture_region`, `capture_window`) are 1280 px wide by default, like action screenshots. Pass `max_width` for another width, or `native: true` for full resolution. The two cannot be combined.
- Every screenshot has an `observation_id`. Pointer tools accept `observation_id` with `image_x`
  and `image_y` in that screenshot's pixels. The server maps the point through the crop and
  resize. Give one form per point.
- Observation IDs keep geometry only, expire after 5 minutes, and at most 64 are kept.
- An image of one window (`capture_window`) maps points only while that window is unchanged. If it moved, resized or closed, its reference changed, and the point is refused with `invalid_argument`. Capture the window again.
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

- Matching works on the words inside each OCR line, so a label in the middle of a line can be
  clicked. `exact` needs the query's words to equal consecutive whole words of a line
  (`Edit` matches in `File Edit View`, `Edi` does not). `exact_match=false` needs the query to
  occur in the line's words and selects every word it touches. The click lands on the centre of
  the box that covers the matched words, not on the centre of the line. A line without word
  boxes is matched as a whole line.
- `find_text_on_screen` returns the same matched-word box for each match, in display-local pixels.
- Zero matches return `ocr_no_match`. More than one returns `ocr_ambiguous_match`, including two
  matches inside one line. Neither sends input or falls back to coordinates.
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

- `timeout_ms` defaults to 5000. The maximum is 30000. All wait tools accept 0 as the lowest value, which checks once.
- Returns `satisfied`, `timed_out`, `attempts`, `elapsed_ms` and the last matches.
- Each pass is a full OCR pass, so a smaller region is faster. If every pixel of the region and
  the foreground are unchanged since the previous pass, that pass is reused and OCR is skipped.

OCR text and checkbox glyphs do not prove the state of a control.

## Keys

`press_keys` names are case-insensitive, and `-`, `_` and spaces are ignored (`Page-Up` is
`page_up`). Common aliases work: `esc`, `del`, `return`, `up`, `down`, `left`, `right`, `pgup`.

- Punctuation: `comma`, `period`, `slash`, `semicolon`, `quote`, `left_bracket`, `right_bracket`,
  `backslash`, `minus`, `equals`, `backtick`, or the character itself (`,` `.` `/` `;` `'` `[` `]`
  `\` `-` `=` and the backtick). These name the key that types that character without Shift. The
  active keyboard layout of the foreground window decides which key that is; on a layout that
  needs Shift or AltGr for it, the US key position is used.
- Numpad: `numpad_0` to `numpad_9`, `numpad_add`, `numpad_subtract`, `numpad_multiply`,
  `numpad_divide`, `numpad_decimal`, `num_lock`.
- System: `print_screen`, `caps_lock`, `scroll_lock`, `pause`, `menu` (also `apps`).
- Media: `volume_mute`, `volume_down`, `volume_up`, `media_next`, `media_previous`, `media_stop`,
  `media_play_pause`.

An unknown name is refused with the nearest valid name, or the full list.

`type_text` sends text as Unicode characters, except that `\n`, `\r\n`, a lone `\r` and `\t` are
pressed as Enter and Tab keys, because a control that ignores those characters still reacts to
the keys. Each Enter or Tab is checked against the target like a text batch.

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
- A `wait` step (`wait_ms` from 1 to 5000, at most 15000 in total) pauses between steps. It sends no input and cannot be the last step.

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

Every error also has `error.retryable`. It says whether repeating the same request can succeed after the stated recovery. A result with `retry_action: false` always wins over `retryable`. `platform_operation_failed` adds `error.reason`, a stable sub-code:

| `reason` | Meaning | `retryable` |
| --- | --- | --- |
| `pointer_moved` | The pointer moved after validation. Observe again. | Yes |
| `key_held` | A requested key or button is already held. Release it. | Yes |
| `input_interleaved` | Another input ran during text recognition. Find the text again. | Yes |
| `cancelled` | A stop, client cancellation or closing session cancelled the work. | No |
| `ocr_unavailable` | The OCR helper failed. | No |
| `input_blocked` | Windows accepted only part of the input, for example for a higher-integrity window. | No |
| `desktop_work_stopped` | Admission is closed. | No |
| `worker_failed` | A worker failed. Observe before recovery. | No |
| `os_call_failed` | Any other Windows call failure. | No |
| `backend_unavailable` | The platform backend is missing. | No |

A failed foreground activation (`focus_window`) is retryable. Ask the user to click the window first, because Windows can refuse focus changes from a background process.

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

After a stop, observation tools keep working: `list_displays`, `list_windows`, `list_virtual_desktops`, the captures, `read_text_in_region`, `find_text_on_screen`, `wait_for_text`, `wait_for_window`, `wait_for_visual_change`, `wait_for_change_since` and `get_server_status`. A wait that is already running is cancelled by the stop. Input tools stay refused.

## Limits

- Windows 10 version 2004 or later, or Windows 11, on x86-64.
- No UI Automation. The agent works from screenshots, OCR and coordinates.
- Protected video and HDR content may not capture correctly.
- No tools for shell commands, launching applications, files, clipboard, registry, network or
  elevation.
