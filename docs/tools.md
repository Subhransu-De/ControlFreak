# Tools

ControlFreak exposes 27 MCP tools. Every tool publishes a title, an input schema, an `outputSchema` and annotations. Export the full contract with `controlfreak --print-tools`.

- Tools are listed in the order an agent needs them. `stop_desktop_work` is last.
- Every input tool is annotated `destructiveHint: true` and `openWorldHint: true`. `stop_desktop_work` is also destructive, because it closes input permanently. Use `end_control_session` to finish normally.
- Every `outputSchema` accepts both the success result and the error result of its tool, so clients that validate `structuredContent` (such as the official TypeScript SDK) can read errors. Refused input actions carry their progress fields next to `error`.
- `tools/list` publishes a compact `outputSchema`. Each success branch names its top-level fields with their JSON types (or their fixed values) and allows other fields. The error branch is complete and allows no other fields. `controlfreak --print-tools` prints the same tools with the full result contract, which describes every field and allows no others.

## Tool list

Observation tools read the desktop and never need a control session. Input tools change the
desktop and run inside a [control session](sessions.md).

| Tool | Kind | What it does |
| --- | --- | --- |
| `list_displays` | Observe | Active displays with IDs, primary flag and bounds in physical pixels. |
| `capture_display` | Observe | Screenshot of a display. |
| `capture_region` | Observe | Screenshot of a rectangle, 1280 px wide unless `max_width` or `native` is set. |
| `capture_window` | Observe | Screenshot of a window's visible frame, without focusing it. The window renders itself (`capture_method` `windows_print_window`), so windows covering it do not appear. The render is bounded: a window that Windows flags as hung is not asked, and one that does not answer within 2 seconds is given up on. The screen rectangle is then copied (`windows_gdi`), which shows whatever covers the window, so `capture_method` says which one you got. Menus, popups and dropdowns are separate windows and never appear in it; use an action's screenshot or `capture_display` to see them. Only one render runs at a time: a second `capture_window` call made meanwhile waits for it while it is still rendering, within the same 2 seconds, and copies the screen if that time runs out; if that render was already given up on, the screen is copied at once. Every screenshot result reports `capture_method` as one of `windows_print_window` or `windows_gdi`; display and region captures always report `windows_gdi`. |
| `list_windows` | Observe | Visible titled windows, including owned dialogs (with an `owner` field), foreground first. At most 200. |
| `wait_for_window` | Observe | Wait for a window that matches title, class or process. |
| `list_virtual_desktops` | Observe | Windows grouped by virtual desktop, with the current one marked. |
| `read_text_in_region` | Observe | OCR lines and words with bounds. |
| `find_text_on_screen` | Observe | Find text with OCR. Discovery only. |
| `wait_for_text` | Observe | Repeat OCR until text appears or disappears. |
| `capture_visual_baseline` | Observe | Save how a region looks now, for a later wait. |
| `wait_for_change_since` | Observe | Wait until a region differs from its baseline and settles. |
| `wait_for_visual_change` | Observe | Wait until a region changes from how it looks now and settles. |
| `get_server_status` | Observe | Version, privileges, DPI awareness, session and stop state, input admission and the recovery step after a stop, recent operations. |
| `begin_control_session` | Session | Reserve the desktop for one approved window. |
| `end_control_session` | Session | Release the desktop after pending actions finish. Reports `closed`, `closing` or `not_owned`. |
| `stop_desktop_work` | Session | Emergency stop: cancel all desktop work and close input until the server restarts. |
| `focus_window` | Input | Restore and focus a window, then verify it is in the foreground. |
| `move_mouse` | Input | Move the pointer. |
| `click_mouse` | Input | Click, with optional modifiers. |
| `drag_mouse` | Input | Press, move and release in one call. The pointer travels in several steps over at least 120 ms, holding the button for 20 ms before and after the move. |
| `scroll_mouse` | Input | Scroll vertically or horizontally at a point. |
| `press_keys` | Input | Send a key or key chord. Letters, digits, F1 to F12, punctuation, the numpad, system keys and media keys; see Keys below. |
| `type_text` | Input | Type Unicode text without the clipboard. Line breaks and tabs are pressed as Enter and Tab. |
| `click_text` | Input | Click the unique OCR text that matches, at the matched words. |
| `switch_virtual_desktop` | Input | Switch one or more virtual desktops (`steps`) left or right. |
| `run_actions` | Input | Up to 16 clicks, scrolls, keys, text and wait steps in one call. |

## Targets

- Window IDs are opaque and issued by the server. Do not build them from HWNDs or PIDs.
- Get an ID from `list_windows` or a screenshot result, and pass it as `target_ref` to
  `begin_control_session` and to every input tool. `focus_window`, `capture_window` and `wait_for_window`
  take it as `window_id`, and also accept `target_ref` as an alias. Giving both is fine only when they
  are equal.
- `switch_virtual_desktop` takes `target_ref` as an optional field. When it is missing, the session's approved window is used, or the foreground window when no session has one. The switch ends the session, because the approved window is no longer on screen. Begin a new session on a window of the new desktop. With `steps` above 1, the first shortcut is checked like any other input, and later shortcuts are checked only for what still applies once the desktop has changed: cancellation, the unlocked Default input desktop, the approved window still being the same live window with an unchanged display layout and unchanged bounds (a move refuses the shortcut with `target_moved`, as for any input), and the integrity of the window that now holds the foreground and keyboard focus. The approved window is not required to be on the new desktop or in the foreground. A failure or cancellation part-way reports the shortcuts already sent as partial input.
- `switch_virtual_desktop` reports `action.before_id` and `action.after_id`, the desktop of the foreground window before the first shortcut and after the last one, and `action.transition`: `confirmed_changed` or `confirmed_unchanged` when both are known, otherwise `unknown`. A desktop is unknown when no window holds the foreground or Windows reports no desktop for it, which may be the normal result of switching to an empty desktop; that case is not yet verified on Windows. The input is still reported as sent. `action.changed` compares the two raw values and is kept for compatibility, so it is `true` for a known desktop followed by an unknown one. Use `transition`.
- One session approves one target. End the session before approving another.
- A screenshot or OCR result includes `target_ref` only when the foreground window stayed the
  same during the capture. A null `target_ref` does not approve input.
- References expire 5 minutes after the last matching observation.
- Before each input, the server checks that the target is the same process and window, is on the current desktop, has the same display layout and the same bounds, and does not run at a higher integrity level, and that the target or a window of its own process is in the foreground.
- The target's own dialogs, popup menus and drop-downs belong to the same process, so they may
  hold the foreground and receive pointer input without a new session. A window of another
  process never does. For `explorer.exe` targets (File Explorer, the taskbar, the desktop) only
  the target's own dialogs and popup menus count, not the rest of the shell.
- If the foreground, the keyboard focus or the pointer destination belongs to another process, cannot be identified, or does not exist at that instant, that action is refused and the session keeps its target. The error is `target_invalidated` with a `reason` that tells it from a fatal change (see Errors). Bring the target back with `focus_window` and try again; nothing else needs to be observed.
- If that window runs at a higher integrity level, or its integrity cannot be read, the action is refused with `higher_integrity_target` or `target_integrity_unavailable`, there is no `reason`, `retryable` is false, and the session keeps its target. `focus_window` is refused for such a window too, so it cannot take the foreground back; the user has to close or leave that window first.
- A point under the ControlFreak session tab's **Stop agent** button is refused with reason `own_indicator`. Screenshots do not show the button, which sits under the tab at the top centre of every display (104 by 32 device-independent pixels, a small gap below the tab). Pick another point, or use the keyboard.
- A packaged app's content window belongs to the app, not to the `ApplicationFrameHost.exe` frame that holds it. For a target that is such a frame, a window inside that very frame that belongs to the process of the frame's hosted content window counts as the target for pointer and keyboard destinations. Its integrity is checked like any other window's. Top-level windows of that process, and windows inside any other frame, are not accepted this way.
- `explorer.exe`, `ApplicationFrameHost.exe` (the frame of every packaged app) and any process whose program file cannot be read host unrelated windows, so for those targets only the target's own dialogs and popup menus count as the same application.
- If the target is moved, resized, maximized or restored, it is still the same window and keeps its session. The next action is refused once with reason `target_moved`, sends nothing, and the session's reference takes the window's new bounds. Observe again and act with coordinates from the new screenshot, passing the same `target_ref`; no new session is needed. A window that only moved keeps its reference, so `list_windows` and screenshots still return that `target_ref`. An action that started before the move was found, for example one waiting behind another action, is refused the same way, because it may hold coordinates for the old position. Every later move is refused once in the same way, including one during a long action such as a drag or a long text, which then stops as partial input. Display-local `x` and `y` are not changed by the move: they still name the same point on the display, which may now be a different part of the window, so take them from a screenshot taken after the move. An `observation_id` from before the move stays refused (see Coordinates).
- If the window itself changes (a different window or process, another virtual desktop, destruction), or the display layout changes (a display added, removed, moved or resized), the action is refused and the session target is cleared. A move that comes with such a change is cleared too. Observe again and approve the new target. Do not repeat the input on your own.
- Switching desktops invalidates a target. Capture again and approve the new reference.
- `list_windows` also lists visible owned windows such as dialogs. Their `owner` field is the
  target reference of the window that owns them. `focus_window` and `capture_window` accept them,
  and a dialog can be approved as a target of its own.
- The taskbar, secondary taskbars, the desktop and the Start and Search surfaces can be approved
  too, even though Windows reports no virtual desktop for them: they are treated as present on
  every desktop. They are recognized by window class and by the program that owns them, never by
  class alone. A screenshot taken while one of them is in front returns its `target_ref`.

`focus_window`:

- Call it after `begin_control_session`, and again when a window from another process took the foreground.
- Inside a session it accepts the approved window or a window of the same process, such as one of its dialogs. Anything else needs a new session.
- A refused or timed-out activation (`activation_refused`, `settle_timeout`) leaves the session intact and is retryable, as its `retryable` says. Only a change of the approved window itself ends the session.
- `timeout_ms` defaults to 1000 and accepts 100 to 5000.
- It never sends keyboard or mouse input to get focus.
- Windows lets a process take the foreground only under the conditions in Microsoft's [`SetForegroundWindow` documentation](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setforegroundwindow). The calling process must be a desktop app, the foreground process must not have locked foreground changes with `LockSetForegroundWindow`, and no menu may be active. In addition, one of these must hold: the foreground-lock timeout has expired, the caller is the foreground process, was started by it or received the last input event, there is no foreground window, or the caller or the foreground process is being debugged. Windows can refuse even then. The server starts before the user types a request in another application, so Windows usually refuses the server's own request, and sometimes grants it. When it refuses, `focus_window` asks once more through a short-lived copy of the server (`controlfreak --activation-helper`), started for this request only, which makes the same `SetForegroundWindow` call for the already validated window and exits. It may use only what is left of `timeout_ms`, at most 1.5 seconds, and a stop or cancellation terminates it at once. `input.activation_helper` reports it: `accepted`, `refused` or `failed` (it could not start, did not answer in time, was stopped, or found the window owned by another process). When its answer was lost after it may have made the call, `input.input_outcome` stays `unknown`. The field is absent when the helper was not needed.
- Errors: `activation_refused`, `settle_timeout`, `target_invalidated`, and the integrity errors.

## Coordinates

- Display coordinates are display-local physical pixels. `display_id` is optional everywhere. It defaults to the primary display, and `"primary"` names it explicitly. Call `list_displays` only to reach another display.
- Explicit captures (`capture_display`, `capture_region`, `capture_window`) are 1280 px wide by default, like action screenshots. Pass `max_width` for another width, or `native: true` for full resolution. The two cannot be combined.
- Every screenshot has an `observation_id`. Pointer tools accept `observation_id` with `image_x`
  and `image_y` in that screenshot's pixels. The server maps the point through the crop and
  resize. Give one form per point.
- Observation IDs keep geometry only, expire after 5 minutes, and at most 64 are kept.
- An image of one window (`capture_window`) maps points only while that window is unchanged: the same reference with the same bounds. A window keeps its reference when it moves, so its bounds decide: if it moved, resized or closed, the point is refused with `invalid_argument`. Capture the window again.
- Every screenshot also records the display layout it was captured on: the ID and bounds of each display. If a display was added, removed or moved since then, an image point is refused with `invalid_argument` and nothing is sent, because the old rectangle may now lie on another display. Capture again. A screenshot taken while the layout changes fails instead of recording a layout its bounds do not belong to.
- An image pixel maps to the source pixel under its centre: `source_bounds.left + floor((2 * image_x + 1) * source_bounds.width / (2 * image_width))`, and the same for `y` with `top` and `height`, each axis on its own. A source pixel taken to the image pixel that shows it and back lands within one source pixel of where it started when the image is downscaled less than 2×. An unresized image maps one to one.
- Screenshots are downscaled by area averaging. An image is resized when `image_width` and `image_height` differ from the size of `source_bounds`.
- Physical pixels need a per-monitor DPI aware server. `controlfreak.exe` declares per-monitor v2 in its manifest; per-monitor v1 is also accepted. If Windows keeps the server DPI unaware or system aware, for example through a high DPI compatibility override on the executable, it would scale coordinates on a scaled display. The server then refuses captures and input with `dpi_awareness_unsupported`, and `get_server_status` reports `status: "degraded"` and `dpi_awareness` with the awareness in effect (`per_monitor_v2`, `per_monitor`, `system_aware`, `unaware` or `unknown`) and the reason. Remove the override and restart the server.

## Waiting for the window to settle

Every input action (`move_mouse`, `click_mouse`, `click_text`, `scroll_mouse`, `drag_mouse`, `press_keys`, `type_text` and each `run_actions` step) watches the approved window after it sends input and reads its evidence once the window has visibly responded and then stayed unchanged for 150 ms. This keeps an action from returning the screen as it was before the application reacted, for example a menu that is still opening or a page that is still loading.

- A window that does not visibly change within 500 ms (150 ms for an action without a screenshot) is taken as not responding, and the action returns.
- `observation.settle_ms` bounds the whole wait: 2000 ms by default, 500 ms for an action without a screenshot (`mode` `none` or `metadata`, and every `run_actions` step before the last), at most 10000. `0` keeps a short fixed wait (75 ms after pointer input, 150 ms after keys) and watches nothing.
- `observation.settle` reports how the wait ended, with `elapsed_ms`: `settled` (the evidence shows the response), `unchanged` (no visible response, so the evidence may predate it), `still_changing` (the limit was reached, for example on a page with animation or video) or `not_watched` (`settle_ms` was 0, or the window is unknown or larger than 2560×1600 physical pixels, so the fixed wait was used). In `run_actions`, each step reports its own `settle`.
- A stop during the wait ends it at once. The input was already sent, so the action reports `completed_unverified`.
- Only the approved window's rectangle is watched. A menu or dialog that opens outside it reads as `unchanged`. `focus_window` and `switch_virtual_desktop` have their own waits and refuse `settle_ms`.

## Text (OCR)

OCR uses the Windows OCR engine on the computer. It needs a Windows OCR language pack. It gives
no confidence scores.

Without `language`, OCR uses the first language in the Windows language list that has an OCR pack. A change to that list applies from the next OCR call. A newly installed or removed OCR language pack can take up to 100 calls, or one minute without OCR, to apply.

| Tool | Matching | Gives input approval |
| --- | --- | --- |
| `click_text` | `match_mode`: `exact` (default) or `substring` | Yes, only for one unique match |
| `find_text_on_screen` | `match_mode`: `substring` (default), `exact`, `tolerant` | No |
| `wait_for_text` | Same as `find_text_on_screen`, until `appears` or `disappears` | No |

`click_text`:

- Matching works on the words inside each OCR line, so a label in the middle of a line can be
  clicked. `exact` needs the query's words to equal consecutive whole words of a line
  (`Edit` matches in `File Edit View`, `Edi` does not). `substring` needs the query to
  occur in the line's words and selects every word it touches. The click lands on the centre of
  the box that covers the matched words, not on the centre of the line. A line without word
  boxes is matched as a whole line.
- `find_text_on_screen` returns the same matched-word box for each match, in display-local pixels.
- `match_mode` `tolerant` is refused with `invalid_argument`, because a click needs a strict match. Use `find_text_on_screen` with `tolerant` to pick the query.
- `exact_match` is the older name: `true` is `exact` and `false` is `substring`. It can repeat `match_mode`, but a value that contradicts it is refused with `invalid_argument`.
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

- Punctuation: `comma`, `period`, `slash`, `semicolon`, `quote`, `left_bracket`, `right_bracket`, `backslash`, `minus`, `equals`, `backtick`, or the character itself (`,` `.` `/` `;` `'` `[` `]` `\` `-` `=` and the backtick). `comma`, `period`, `minus` and `equals` are the fixed OEM keys (`VK_OEM_COMMA`, `VK_OEM_PERIOD`, `VK_OEM_MINUS`, `VK_OEM_PLUS`) on every layout. The others name the key that types that character without Shift on the foreground window's keyboard layout. If that layout types the character only with Shift or AltGr, the action is refused before any input with an error that suggests `type_text`; if the layout cannot say, the US key position is used.
- Numpad: `numpad_0` to `numpad_9`, `numpad_add`, `numpad_subtract`, `numpad_multiply`,
  `numpad_divide`, `numpad_decimal`, `num_lock`.
- System: `print_screen`, `caps_lock`, `scroll_lock`, `pause`, `menu` (also `apps`).
- Media: `volume_mute`, `volume_down`, `volume_up`, `media_next`, `media_previous`, `media_stop`,
  `media_play_pause`.

An unknown name is refused with the nearest valid name, or the full list.

Windows adds a modifier the user is holding to every key, click and wheel turn the server sends: Delete becomes Shift+Delete, Tab becomes Alt+Tab, L becomes Win+L, a click becomes a Ctrl-click. So while the user holds Shift, Ctrl, Alt or Win, `press_keys`, `type_text`, `click_mouse`, `click_text`, `drag_mouse`, `scroll_mouse`, `switch_virtual_desktop` and the matching `run_actions` steps are refused with reason `key_held` before anything is sent. A modifier named in the request's own `keys` or `modifiers` is refused too, because the server presses it itself. A click, drag or scroll is refused before the pointer moves if the key is already down when it starts. If the key goes down during a timed approach, the action stops before its button or wheel event, as `partially_sent` with no events, because the pointer already moved. If it goes down between the clicks of a double or triple click, the click stops before its next press and releases the button. `move_mouse` is not refused. Release the key and retry. The server's own modifiers never cause a refusal, and releases and cleanup are never held back.

`type_text` sends text as Unicode characters, except that `\n`, `\r\n`, a lone `\r` and `\t` are pressed as Enter and Tab keys, because a control that ignores those characters still reacts to the keys. Each character, Enter and Tab is checked against the target before it is sent, and is refused if Shift, Ctrl, Alt or Win is held at that moment, or, for Enter and Tab, if that key itself is held. A held Alt could turn characters into menu shortcuts. If the user presses a modifier part-way through, typing stops before the next character with `key_held`, and `input.sent_events` shows how much was sent.

`type_text` sends one keystroke at a time and checks the target before every keystroke. Before each keystroke it waits until the window with keyboard focus answers a message, pauses 10 ms, and waits for a second answer. A Unicode keystroke does not carry its character in its window message: Windows keeps the character of the keystroke the application took last, so an application that takes several queued keystrokes before translating them, as a busy one can, would turn them all into the last character. Pacing keeps keystrokes from piling up while the application is busy. It cannot prove the application took or translated a keystroke, so an application that answers messages without taking keyboard input, or handles one keystroke for longer than the pause, can still receive wrong characters, and so can any application when a keyboard hook delays keystrokes for longer than the pause; check the result before relying on it. Typing runs at roughly 70 characters a second, slower while the application is busy, and the 4,000-unit maximum can take about a minute. If the window does not answer for 5 seconds, typing stops with `target_not_responding`, and `input.sent_events` shows how much was sent. Each wait is bounded, but without `timeout_ms` a whole run has no overall limit: a window that answers just under 5 seconds every time keeps typing slowly until it is finished, stopped or cancelled.

`timeout_ms` (1 to 300000, optional) limits how long typing may take. It is higher than the 30000 limit of the wait tools because the longest text takes about a minute, and 0 is refused because it would type nothing. The time counts from when typing starts, after the target is first checked; the settle wait and the screenshot after typing are not included. Typing stops between two keystrokes once the time has passed, also while it waits for a busy window to answer, so it can run over by at most about 100 ms plus one keystroke. The result is `platform_operation_failed` with reason `deadline_reached` and `retryable: false`, `status` is `partially_sent` with the exact `input.sent_events`, or `not_started` when time ran out before the first keystroke. Observe the text before typing the rest: sending the whole text again would type the beginning twice. Without `timeout_ms`, typing behaves as before.

A character outside the Basic Multilingual Plane, such as an emoji, is two UTF-16 units, a surrogate pair, and each half is checked and sent as its own keystroke. If typing stops between the halves, the second half is never sent: a target refusal because it could reach another window, and a held modifier, a reached `timeout_ms`, a user stop or a cancellation because a stop means no more input. The application then holds half a character, and the result says so with `input.split_character: true`; in `run_actions` the failed `type_text` step carries `split_character: true` too. Observe the text and delete the stray half before typing again. The field is absent when no pair was split. If Windows did not confirm whether the second half was sent, `input.input_outcome` is `unknown` and `split_character` is absent.

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
- Only the last step returns a screenshot. Each earlier step waits up to 500 ms for the window to settle (see [Waiting for the window to settle](#waiting-for-the-window-to-settle)). Add a `wait` step before a step that depends on a slower load.
- A `wait` step (`wait_ms` from 1 to 5000, at most 15000 in total) pauses between steps. It sends no input and cannot be the last step.
- A `type_text` step takes an optional `timeout_ms` with the same range and meaning as the single tool (see [Keys](#keys)). It limits that step only. A step that runs out of time fails with `deadline_reached`, and the later steps are skipped. The batch as a whole has no overall limit.

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
| `input.activation_helper` | `focus_window` only, when Windows refused the server: `accepted`, `refused` or `failed` |
| `input.split_character` | `type_text` and `run_actions` only: `true` when typing stopped between the two halves of a surrogate pair, otherwise absent |
| `observation_status` | `succeeded`, `failed`, `not_attempted` |
| `retry_action` | `false` on every status except `completed`, which omits it |

Every error also has `error.retryable`. It says whether repeating the same request can succeed after the stated recovery. A result with `retry_action: false` always wins over `retryable`. `platform_operation_failed` adds `error.reason`, a stable sub-code:

| `reason` | Meaning | `retryable` |
| --- | --- | --- |
| `pointer_moved` | The pointer moved away from the requested point before a button or wheel event, for example because the user moved the mouse. For `drag_mouse` this can be found at the end point, after the drag happened: the button was released where the pointer was, so observe before repeating. | Yes |
| `pointer_not_placed` | Windows did not put the pointer on the requested point, usually because another program confines it (`ClipCursor`). Nothing was pressed there; a drag already in progress is released where the pointer stopped. Observe before continuing. | No |
| `key_held` | A requested key or button is already held, or the user holds Shift, Ctrl, Alt or Win (see [Keys](#keys)). Nothing of the refused key, character or click was sent. Release it. | Yes |
| `input_interleaved` | Another input ran during text recognition. Find the text again. | Yes |
| `cancelled` | A stop, client cancellation or closing session cancelled the work. | No |
| `ocr_unavailable` | The OCR helper failed. | No |
| `input_blocked` | Windows accepted only part of the input, for example for a higher-integrity window. | No |
| `target_not_responding` | The window with keyboard focus stopped answering while text was typed, so typing stopped. Observe it before typing the rest. | No |
| `deadline_reached` | `type_text` or a `run_actions` `type_text` step reached its `timeout_ms` and stopped between two keystrokes. `input.sent_events` shows how much was sent. Observe the text before typing the rest. | No |
| `desktop_work_stopped` | Admission is closed. | No |
| `worker_failed` | A worker failed. Observe before recovery. | No |
| `dpi_awareness_unsupported` | Windows runs the server DPI unaware or system aware, so it would scale coordinates. The message names the awareness. Remove the high DPI compatibility override from `controlfreak.exe` and restart the server (see [Coordinates](#coordinates)). | No |
| `os_call_failed` | Any other Windows call failure. | No |
| `backend_unavailable` | The platform backend is missing. | No |

`target_invalidated` also has a `reason`, so an agent can tell a recoverable refusal from a fatal change without reading the message:

| `reason` | Meaning | `retryable` |
| --- | --- | --- |
| `not_foreground` | The target (or a window of its process) is not in front, or nothing is. Call `focus_window`. The session is kept. | Yes |
| `foreground_changed` | The foreground changed while the action was checked. The session is kept. | Yes |
| `observation_stale` | The foreground or the window set changed while `click_text` read the screen, or the window in front cannot be identified, so the text may belong to another window. Call `click_text` again. The session is kept. | Yes |
| `foreign_destination` | The point or the keyboard focus is in a window of another process. The session is kept. | Yes |
| `destination_unidentified` | The window that would get the input cannot be identified. The session is kept. | Yes |
| `own_indicator` | The point is under ControlFreak's own Stop button. Choose another point. The session is kept. | No |
| `target_moved` | The target moved or was resized since the last check. Input stopped before its next event; `status` and `input` say what was already sent, which is nothing for an action that had not started. Observe again and use coordinates from the new screenshot. The session is kept with the window's new bounds, so an action that starts after this refusal is not refused for this move. | Yes |
| `target_changed` | The target changed (window, process, display layout or desktop), or an earlier change ended the approval. End the session and observe again. | No |

`higher_integrity_target` and `target_integrity_unavailable` have no `reason` and are not retryable. When the code is about the foreground window, the keyboard focus or the pointer destination, the session is kept, and retrying cannot help until the user changes which window is in front. When it is about the target window itself, the session ends.

A failed foreground activation (`focus_window`) is retryable and keeps the session. The activation helper has then been refused too, so ask the user to click the window first, because Windows can refuse focus changes from a background process.

The reason for `platform_operation_failed` is set by the code that detects the failure, not read from the message text, so rewording a message never changes it.

- Partial and uncertain delivery are not errors. Read `status`, not only `isError`.
- Accepted input does not prove the application did what was asked. Check the screenshot.
- After partial or unknown delivery, observe before you act again. Do not replay the whole
  action.

## Argument errors

A request that does not fit the tool's input schema fails with `error.code` `invalid_arguments`: a missing or unknown field, a wrong JSON type, a name that is not in an enum, or a number outside its integer type. A request that parses but breaks a rule, such as a published range or a combination of fields, fails with `invalid_argument`. Both are found before any desktop work, so an input tool reports `status: "not_started"` and takes no session. No argument error repeats a value you sent, so typed text never appears in an error.

`invalid_arguments` explains the field in `error.details`:

| Field | Meaning |
| --- | --- |
| `path` | The field, such as `text`, `observation.region.x`, `keys[1]` or `steps[2].text`. |
| `problem` | `missing_field`, `unknown_field`, `invalid_type`, `invalid_value`, `unknown_variant` or `invalid_length`. |
| `expected` | What the field takes, when known: `string`, `boolean`, `array`, `object`, a range such as `integer from 0 to 255`, or a list such as `one of: left, right, middle`. |
| `received_kind` | The JSON kind that was sent: `string`, `number`, `boolean`, `null`, `array` or `object`, or `missing`. |
| `valid_fields` | `unknown_field` only: the fields that object takes, at most 32. |
| `reason` | The same explanation as one sentence, also used in `error.message`. |

For example, `type_text` with `{"text": 5}` gives `path: "text"`, `problem: "invalid_type"`, `expected: "string"` and `received_kind: "number"`. An unknown key name in `press_keys` keeps its hint (`did you mean 'ctrl'?`, or the valid names) in `reason`.

## Session errors

When an input tool cannot start a session, `error.code` says why:

| Code | Meaning | Retryable |
| --- | --- | --- |
| `desktop_in_use` | Another ControlFreak server owns the desktop. `details` has `retry_after_ms`. | Yes |
| `session_closing` | The previous session is still closing. | Yes |
| `server_shutting_down` | The server is shutting down. | No |
| `desktop_work_stopped` | The user stopped control, or a client called `stop_desktop_work`. `details.recovery` says what allows input again: `wait_for_drain`, `restart_client` or `resolve_held_input`. Only the user restarting the server from the MCP client reopens input. See [Getting input back](sessions.md#getting-input-back). | No |
| `control_environment_unavailable` | The desktop cannot be controlled, for example while it is locked. | No |
| `desktop_arbitration_unavailable` | Desktop ownership could not be checked. | No |

After a stop, observation tools keep working: `list_displays`, `list_windows`, `list_virtual_desktops`, the captures, `read_text_in_region`, `find_text_on_screen`, `wait_for_text`, `wait_for_window`, `wait_for_visual_change`, `wait_for_change_since` and `get_server_status`. A wait that is already running is cancelled by the stop. Input tools stay refused.

## Limits

- Windows 10 version 2004 or later, or Windows 11, on x86-64.
- No UI Automation. The agent works from screenshots, OCR and coordinates.
- Protected video and HDR content may not capture correctly.
- No tools for shell commands, launching applications, files, clipboard, registry, network or
  elevation.
