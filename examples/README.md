# Agent recipes

[recipes.json](recipes.json) contains executable sequences of MCP `tools/call` parameters.
Each `call` is sent unchanged to the server. Each `expect` maps a JSON Pointer in
`structuredContent` to its expected value. These expectations describe the synthetic fixture,
not values to assume on a real desktop.

Run every recipe, including input-schema validation and available output-schema validation:

```powershell
cargo test -p controlfreak-mcp --locked published_recipes_validate_and_run_on_a_synthetic_desktop
```

The runner uses an in-memory MCP transport, a synthetic Windows backend, and a fake indicator.
It never captures the real desktop or injects input. It checks state changes, partial delivery,
session release, and that observation-only calls leave control dormant. The fixture has one
secondary display at virtual origin `-1920, -200`, a crop, a button, and a text field.
[fixture.png](fixture.png) is a generated 200 by 100 pixel image with a blue target centered
at image pixel `50, 25`. Synthetic OCR and input simulate the application's state; these tests
do not validate native capture, OCR accuracy, timing, DPI awareness, or input delivery.

## Observe, act, verify

The first recipe observes `Ready`, captures the target, begins a bounded control session,
captures a visual baseline, clicks, waits for a change, and reads `Saved` before ending control.
On a real desktop, discover display and window IDs first and choose targets from current
observations. Replace the fixture IDs, coordinates, baseline ID, and expected text with the
values observed during that run. Reuse action screenshots when they answer the next question.

A completed action reports accepted input. Its `effect_verification` remains `unverified`.
A visual change also does not prove success. The final observation must establish the intended
application state. If any step fails, stop the sequence, inspect the result, and end the control
session after pending actions finish. An agent should use its equivalent of `finally` for cleanup.

## Map a resized crop

Input coordinates use display-local physical pixels. Screenshot `source_bounds` use signed
virtual-desktop coordinates. For an image point `image_x, image_y`, map each axis independently:

```text
local_x = source_bounds.left - display.bounds.left
          + floor(image_x * source_bounds.width / image_width)
local_y = source_bounds.top - display.bounds.top
          + floor(image_y * source_bounds.height / image_height)
```

Use actual image dimensions because resizing can round either axis. Validate that the image
point lies inside the image and the mapped point lies inside the display. Do not multiply
by Windows DPI scaling or use the resized image coordinates directly as input coordinates.

The fixture crop starts at display-local `100, 60` and covers 400 by 200 physical pixels.
Its returned image is 200 by 100 pixels. Image point `50, 25` maps to display-local `200, 110`,
or virtual point `-1720, -90`. The first recipe asserts both capture metadata and input position.

## Bounded waits

Capture a baseline before the action, then pass its returned ID to `wait_for_change_since`.
The first recipe covers a changed result. The second covers `changed=false, timed_out=true`
without acquiring control. A timeout is an observation outcome, not permission to replay input.
Baseline IDs belong to the running server and expire; capture a new baseline after expiration.
`wait_for_visual_change` starts its comparison when called, so it can miss a change that already
happened before the call.

## Partial-input recovery

The third recipe attempts to type `ab`. The fixture accepts the first character and fails,
returning `status=partially_sent`, two accepted events, and `retry_action=false`.
The next call reads the field and sees `a`. The recipe stops and releases control without replaying
the action. On a real desktop, inspect the observed field and choose a new, deliberate correction.
Accepted events are not character counts, particularly for Unicode input. Also inspect
`input.cleanup` when recovering from failed input.

The action-result transport tests separately cover `not_started`, `unknown`, and
`completed_unverified`, including observation failure and worker failure. All require reading
the structured result instead of relying only on `isError`.

## Drag lifecycle and target limits

The fourth recipe uses the shipped atomic `drag_mouse`, then verifies the fixture's `Dropped`
state and ends control. The drag performs its press, motion, and release within one call and
attempts release cleanup on failure. `begin_control_session` reserves control; it does not hold
a mouse button down.

Managed drags across calls and caller-specified target guards are not shipped. No recipe assumes
those contracts. Focusing a window does not pin later keyboard input to that window. Refresh
observations before acting and stop if the intended target has changed. Existing integrity and
input-desktop checks remain enforced, but do not prove the intended application has focus.
Add recipes for future contracts with their implementations.

## Export the tool contract

```powershell
cargo run -p controlfreak-server --locked -- --print-tools
```

This standalone command prints a JSON array containing the same descriptions, input schemas,
output schemas where declared, and annotations as MCP `tools/list`. It does not open the MCP
transport or start desktop helpers. Generate the contract from the version being used instead
of maintaining a second schema copy. The recipe test compares the export with `tools/list`.

ControlFreak's implemented desktop backend is Windows only. Platform-neutral domain types,
synthetic fixtures, and tool wording do not imply a Linux or macOS desktop backend. Application
launch remains outside the tool contract; open the intended application through the user or a
separately authorized launcher before discovering its window.

## Tolerant OCR discovery

The tolerant discovery recipe first tries exact text, then recovers a nonbreaking-space label
using `match_mode=tolerant`. It checks the matching tier and full candidate count without
acquiring a control session. Discovery does not authorize input. Inspect the returned text and
bounds before choosing a fresh strict click query, and stop if the full count is ambiguous.
