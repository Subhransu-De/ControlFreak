# E2E scenes

The live end-to-end suites in `crates/controlfreak-e2e` run the real `controlfreak` server against disposable `controlfreak-fixture` windows. A scene is a TOML file in `crates/controlfreak-e2e/scenes/` that says which fixture windows a task needs, where they sit and how they look. The harness opens the scene's windows before the task starts and closes them when it ends. Each task also gets a fresh server process.

The suites inject real input and change the foreground window. Run them only on a disposable desktop, such as the CI runner or a test VM:

```powershell
$env:CONTROLFREAK_LIVE_DESKTOP = "1"
cargo xtask e2e --strict
```

`cargo run -p controlfreak-e2e -- list` prints every task with its scene and required capabilities, and touches nothing.

## The fixture window

Every scene window is one `controlfreak-fixture` process. It is a 360 × 260 pixel client area with a label reading `Banana` at the top, an edit surface in the middle that paints the text typed into it, and a `Ping` button below it. With `dialog = true` it also opens a small owned dialog of the same process next to it.

The fixture writes every key, character, click and activation it receives to its own event log, and the tasks check that log, not the server's reply. Its first line, `ready`, reports the screen rectangles of the label, edit surface, button and dialog. Tasks use those rectangles to know where to click. A fixture closes itself after `lifetime_secs` (240 seconds by default), so a crashed run never leaves a window behind.

## Scene fields

| Field | Default | Meaning |
| --- | --- | --- |
| `description` | required | One line about the scene. |
| `window.key` | required | The name tasks use for the window. Lowercase letters, digits and dashes, unique in the scene. It also starts the window title. |
| `window.color` | `2E8B57` | Background color as `RRGGBB`. |
| `window.position` | `[10, 140]` | Top-left corner on the primary display, in physical pixels. The default keeps the window below the session indicator. |
| `window.topmost` | `false` | Keeps the window above all non-topmost windows. |
| `window.dialog` | `false` | Opens an owned dialog of the same process. |
| `window.lifetime_secs` | `240` | Seconds until the fixture closes itself, from 10 to 600. |
| `window.busy_drain_ms` | `0` | When above 0, at most 1000: the fixture stays busy this many milliseconds whenever a message arrives, then takes every queued keystroke before translating any, like a busy UI framework. |

Scenes are compiled into the harness, so a run does not depend on the working directory. Unknown fields, duplicate keys and invalid colors are refused when the scene loads.

## single

One green window, `app`, at `[10, 140]`.

This is the plain "one application" desk. Every task here starts a control session on `app`, focuses it, does one thing and checks the result in the fixture's log.

| Task | What happens |
| --- | --- |
| `smoke/focus-window` | Focuses the window, waits for its activation event, and checks that `list_windows` reports it in the foreground. |
| `smoke/click-pixel-and-button` | Clicks the center of the edit surface and checks the click landed within one pixel. Then clicks the button and waits for its press event. |
| `smoke/type-unicode-enter-tab` | Types `Hi, é日` followed by CRLF, Tab and `z`. Checks the window received exactly those characters, with CRLF as one Enter. |
| `smoke/press-punctuation-keys` | Presses nine punctuation keys by name, then Enter and Tab. Checks each arrived as the right virtual key and typed the US-layout character. |
| `smoke/click-text-by-ocr` | Needs OCR. Clicks the word `Banana` by OCR and checks the click landed inside the label. |
| `smoke/injected-stop-click-ignored` | Finds the session's Stop button. Checks the server refuses a click on it with `own_indicator`. Then sends a real injected click straight to the button through the fixture and checks control did not stop. |

## busy-app

One green window, `app`, with `busy_drain_ms = 30`.

This is the "application that is busy when input arrives" desk. Windows hands a Unicode keystroke's character to the application from the keystroke it took last, so an application that takes several queued keystrokes before translating them receives them all as one character. Windows 11 Notepad did this while busy, and `type_text` used to send text in bursts of 50 characters.

| Task | What happens |
| --- | --- |
| `smoke/type-into-busy-app` | Types `Hello, world! é日 0123456789`, Tab and `z`. Checks the window received exactly those characters. Before keystrokes were paced, it received most of them as copies of one character. |

## with-dialog

One window, `owner`, that also owns a dialog of the same process.

This is the "application with a modal-style dialog" desk. It shows that a session approved for an application also covers the dialogs that application opens.

| Task | What happens |
| --- | --- |
| `smoke/owned-dialog-accepted` | Checks `list_windows` lists the dialog with `owner` set to the main window and the same process ID. Clicks inside the dialog, then clicks the main window's button. Both clicks must be accepted and both must reach the fixture. |

## covered

Two windows of two processes at the same position. `target` is green. `cover` is magenta and topmost, so it hides `target` completely.

This is the "window behind another window" desk. It checks that a window capture shows the window's own pixels, not whatever covers it.

| Task | What happens |
| --- | --- |
| `smoke/capture-occluded-window` | Captures `target` with `capture_window` without a cursor marker. Checks the result used `windows_print_window`, contains the target's green and no magenta from the cover. |

## two-processes

Two windows of two different processes side by side: `ledger` (green) at `[10, 140]` and `journal` (blue) at `[410, 140]`.

This is the "copy something from one application into another" desk. A control session owns one application, so moving to the other window always ends one session and begins the next.

| Task | What happens |
| --- | --- |
| `smoke/focus-after-user-input` | Injects a click into `journal` after the server has started, as a user typing in another app does, then focuses `ledger`. Checks that Windows refused the server's own request and the activation helper's was accepted (`input.activation_helper` is `accepted`), that `ledger` logged an activation and is in front, and that a click on it is accepted. |
| `smoke/foreign-window-refused` | With a session on `ledger`, clicks at the center of `journal`. Checks the refusal is `target_invalidated` with reason `foreign_destination`, is retryable and sent no input. Then checks the session still works on `ledger` and `journal` received nothing. |
| `handoff/sessions-hand-over-cleanly` | Types `A1` into `ledger`, `B2` into `journal` and `C3` into `ledger`, each in its own session. Checks `ledger` received exactly `A1C3` and `journal` exactly `B2`. |
| `handoff/other-window-refused-in-session` | With a session on `ledger`, tries to type into `journal`. Checks the call is refused, `journal` received no input, and typing into `ledger` still works afterwards. |
| `handoff/carry-value-read-by-ocr` | Needs OCR. Types `INV 4471 1280` into `ledger`, reads it back from the screen with `read_text_in_region`, ends the session, and types what it read into `journal`. Checks `journal` received the value and `ledger` received nothing after its session ended. |
| `handoff/ten-round-trips` | Ten handovers in a row, alternating between the windows. Checks every window's text after each round, and that no round takes longer than 3 seconds. |

## No scene

`smoke/taskbar-target-ref` opens no fixture window. It needs the `taskbar-foreground` capability: the probe brings the shell taskbar to the foreground. The task then checks that a display capture reports the taskbar's target reference and that a session can begin on it.

## Capabilities and BLOCKED

A task that needs a capability the runner lacks is reported as BLOCKED, not skipped. With `--strict`, which CI uses, BLOCKED fails the run.

| Capability | How it is probed |
| --- | --- |
| `ocr` | `find_text_on_screen` on a small region. Only an `ocr_unavailable` error counts as missing. |
| `taskbar-foreground` | The fixture's `--activate-taskbar` helper must succeed. |

## Adding a scene

1. Add a TOML file to `crates/controlfreak-e2e/scenes/`.
2. Add it to the `BUILTIN` list in `crates/controlfreak-e2e/src/scene.rs`.
3. Name it from a task with `.scene("name")`.
4. Describe it in this file.

`cargo test -p controlfreak-e2e` checks that every built-in scene loads and that every scene a task names exists.
