use crate::{
    Capability, Ctx, Failure, Suite,
    events::{self, Event},
    workflow::{ensure, task},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::json;
use std::{
    io::Cursor,
    thread,
    time::{Duration, Instant},
};

const GREEN: (u8, u8, u8) = (0x2E, 0x8B, 0x57);
const MAGENTA: (u8, u8, u8) = (0xFF, 0x00, 0xFF);

pub(crate) fn suite() -> Suite {
    Suite {
        name: "smoke",
        about: "One tool behavior per task on one disposable window",
        tasks: vec![
            task("focus-window", focus_window).scene("single"),
            task("click-pixel-and-button", click_pixel_and_button).scene("single"),
            task("click-outside-pointer-clip", click_outside_pointer_clip).scene("clipped"),
            task("type-unicode-enter-tab", type_unicode).scene("single"),
            task("type-into-busy-app", type_into_busy_app).scene("busy-app"),
            task("press-punctuation-keys", press_punctuation).scene("single"),
            task("capture-occluded-window", capture_occluded).scene("covered"),
            task("click-text-by-ocr", click_text_by_ocr)
                .scene("single")
                .requires(&[Capability::Ocr]),
            task("owned-dialog-accepted", owned_dialog).scene("with-dialog"),
            task("foreign-window-refused", foreign_refused).scene("two-processes"),
            task("focus-after-user-input", focus_after_user_input).scene("two-processes"),
            task("taskbar-target-ref", taskbar_target).requires(&[Capability::TaskbarForeground]),
            task("injected-stop-click-ignored", injected_stop_click)
                .scene("single")
                .budget_secs(40),
            task("held-modifier-refused", held_modifier_refused).scene("single"),
        ],
    }
}

fn control(cx: &mut Ctx<'_>, key: &str) -> Result<String, Failure> {
    let id = cx.target(key)?;
    cx.begin(&id)?;
    cx.focus(&id)?;
    Ok(id)
}

fn focus_window(cx: &mut Ctx<'_>) -> Result<(), Failure> {
    let id = control(cx, "app")?;
    cx.expect("app", "an activation", events::activated)?;
    let windows = cx.ok("list_windows", json!({}))?;
    let listed = windows["windows"]
        .as_array()
        .and_then(|all| all.iter().find(|window| window["id"] == id.as_str()))
        .ok_or_else(|| Failure::assertion(format!("the fixture is not listed: {windows}")))?;
    ensure!(listed["is_foreground"] == true, "not foreground: {listed}");
    cx.end()
}

fn click_pixel_and_button(cx: &mut Ctx<'_>) -> Result<(), Failure> {
    let id = control(cx, "app")?;
    let edit = cx.center("app", "edit")?;
    let result = cx.click(&id, edit)?;
    ensure!(
        !result.is_error && result.structured["status"] == "completed",
        "{}",
        result.structured
    );
    let events = cx.expect("app", "the click on the edit surface", |e| {
        !events::left_downs(e, "main").is_empty()
    })?;
    let down = events::left_downs(&events, "main")[0];
    ensure!(
        events::near(down, edit),
        "clicked {edit:?} but the fixture saw {down:?}"
    );
    let button = cx.center("app", "button")?;
    let result = cx.click(&id, button)?;
    ensure!(!result.is_error, "{}", result.structured);
    cx.expect("app", "the button press", |e| {
        events::button_pressed(e, "ping")
    })?;
    cx.end()
}

/// A program that confines the pointer with `ClipCursor` makes `SetCursorPos` succeed but leave
/// the pointer on the clip's edge. A click asked for outside the clip must be refused instead of
/// landing on whatever is at the edge, and a click inside it must still land. The harness frees
/// the pointer when it closes the fixture, whatever happens here.
fn click_outside_pointer_clip(cx: &mut Ctx<'_>) -> Result<(), Failure> {
    let id = control(cx, "app")?;
    cx.expect("app", "the pointer clip", |e| {
        e.iter()
            .any(|event| matches!(event, Event::Other(line) if line == "main clip on"))
    })?;
    let regions = cx.window("app")?.regions.clone();
    let clip = regions
        .get("clip")
        .ok_or_else(|| Failure::host("the fixture reported no clip region"))?;

    let button = cx.center("app", "button")?;
    ensure!(button.0 < clip[2], "the button is outside the clip");
    ensure_clip_in_force(cx, clip)?;
    let inside = cx.click(&id, button)?;
    ensure!(
        !inside.is_error,
        "a click inside the clip was refused: {}",
        inside.structured
    );
    cx.expect("app", "the button press", |e| {
        events::button_pressed(e, "ping")
    })?;

    let [_, edit_top, edit_right, edit_bottom] = regions
        .get("edit")
        .ok_or_else(|| Failure::host("the fixture reported no edit region"))?;
    let outside = (edit_right - 20, edit_top.midpoint(edit_bottom));
    ensure!(outside.0 >= clip[2], "the edit point is inside the clip");
    ensure_clip_in_force(cx, clip)?;
    let downs_before = events::left_downs(&cx.window("app")?.events(), "main").len();
    // Only the pointer moved, so the result is `partially_sent` with the error beside it, not
    // an MCP error: a result whose input began never sets `isError`.
    let refused = cx.call(
        "click_mouse",
        json!({"target_ref": id, "x": outside.0, "y": outside.1, "observation": {"mode": "none"}}),
    )?;
    ensure!(
        refused.structured["status"] == "partially_sent"
            && refused.error_code() == Some("platform_operation_failed")
            && refused.error_reason() == Some("pointer_not_placed")
            && refused.structured["error"]["retryable"] == false
            && refused.structured["input"]["sent_events"] == 0,
        "wrong refusal: {}",
        refused.structured
    );
    thread::sleep(Duration::from_millis(300));
    let downs_after = events::left_downs(&cx.window("app")?.events(), "main").len();
    ensure!(
        downs_after == downs_before,
        "the refused click reached the fixture at the clip edge"
    );
    cx.end()
}

/// Reads the pointer's clip back from Windows. A clip lost since the fixture set it would let
/// the clicks pass or fail for the wrong reason, so that is a harness failure, not a finding.
fn ensure_clip_in_force(cx: &mut Ctx<'_>, clip: [i32; 4]) -> Result<(), Failure> {
    let output = cx.helper(&["--clip-rect"])?;
    let current: Vec<i32> = String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .filter_map(|n| n.parse().ok())
        .collect();
    if current != clip {
        return Err(Failure::host(format!(
            "the pointer clip {clip:?} is not in force: Windows reports {current:?}"
        )));
    }
    Ok(())
}

fn type_unicode(cx: &mut Ctx<'_>) -> Result<(), Failure> {
    let id = control(cx, "app")?;
    // U+1F980 is outside the Basic Multilingual Plane: it arrives as two UTF-16 halves.
    cx.type_text(&id, "Hi, \u{e9}\u{65e5}\u{1f980}\r\n\tz")?;
    let wanted = "Hi, \u{e9}\u{65e5}\u{1f980}\u{23CE}\u{21E5}z";
    let events = cx.expect("app", "all typed text", |e| {
        events::typed(e).chars().count() >= wanted.chars().count()
    })?;
    ensure_typed(&events, wanted)?;
    cx.end()
}

/// The main window received exactly `wanted`, with every surrogate pair whole.
fn ensure_typed(events: &[Event], wanted: &str) -> Result<(), Failure> {
    let unpaired = events::unpaired(events);
    ensure!(
        unpaired.is_empty(),
        "the window received surrogate halves without their partner: {unpaired:04X?}"
    );
    let typed = events::typed(events);
    ensure!(typed == wanted, "typed {typed:?}, wanted {wanted:?}");
    Ok(())
}

/// An application that takes several queued keystrokes before translating them turns a burst of
/// Unicode packets into copies of the last character ("Hello,!!!!!!!!!!!" in Notepad). Every
/// character must arrive as typed, including both halves of a surrogate pair, which such an
/// application could otherwise take together and receive as two low halves.
fn type_into_busy_app(cx: &mut Ctx<'_>) -> Result<(), Failure> {
    let id = control(cx, "app")?;
    let text = "Hello, world! \u{e9}\u{65e5}\u{1f980} 0123456789\tz";
    cx.type_text(&id, text)?;
    let wanted = text.replace('\t', "\u{21E5}");
    let events = cx.expect("app", "all typed text", |e| {
        events::typed(e).chars().count() >= wanted.chars().count()
    })?;
    ensure_typed(&events, &wanted)?;
    cx.end()
}

fn press_punctuation(cx: &mut Ctx<'_>) -> Result<(), Failure> {
    let id = control(cx, "app")?;
    let keys: [(&str, u16, char); 9] = [
        ("comma", 0xBC, ','),
        ("period", 0xBE, '.'),
        ("slash", 0xBF, '/'),
        ("semicolon", 0xBA, ';'),
        ("quote", 0xDE, '\''),
        ("left_bracket", 0xDB, '['),
        ("minus", 0xBD, '-'),
        ("equals", 0xBB, '='),
        ("backtick", 0xC0, '`'),
    ];
    for (name, _, _) in keys {
        cx.press(&id, &[name])?;
    }
    cx.press(&id, &["enter"])?;
    cx.press(&id, &["tab"])?;
    let wanted: String = keys
        .iter()
        .map(|(_, _, c)| *c)
        .chain(['\u{23CE}', '\u{21E5}'])
        .collect();
    let events = cx.expect("app", "every key", |e| {
        events::typed(e).chars().count() >= wanted.chars().count()
    })?;
    let typed = events::typed(&events);
    ensure!(typed == wanted, "typed {typed:?}, wanted {wanted:?}");
    for (name, vk, _) in keys {
        ensure!(
            events::has_key_down(&events, vk),
            "{name} did not arrive as virtual key {vk:#04X}"
        );
    }
    cx.end()
}

fn capture_occluded(cx: &mut Ctx<'_>) -> Result<(), Failure> {
    let id = cx.target("target")?;
    let cover = cx.target("cover")?;
    ensure!(id != cover, "the cover and the target share an id");
    let result = cx.call(
        "capture_window",
        json!({"window_id": id, "native": true, "include_cursor": false}),
    )?;
    ensure!(!result.is_error, "{}", result.structured);
    ensure!(
        result.structured["capture_method"] == "windows_print_window",
        "{}",
        result.structured
    );
    let data = result
        .image_data()
        .ok_or_else(|| Failure::assertion("capture_window returned no image"))?;
    let png = STANDARD
        .decode(data)
        .map_err(|error| Failure::assertion(format!("image is not base64: {error}")))?;
    let (distinct, green, magenta) = count_pixels(&png)?;
    ensure!(distinct >= 3, "the image is uniform: {distinct} colors");
    ensure!(green > 0, "the target's own pixels are missing");
    ensure!(magenta == 0, "the covering window leaked into the capture");
    Ok(())
}

fn count_pixels(bytes: &[u8]) -> Result<(usize, usize, usize), Failure> {
    let bad = |error: &dyn std::fmt::Display| Failure::assertion(format!("bad PNG: {error}"));
    let mut reader = png::Decoder::new(Cursor::new(bytes))
        .read_info()
        .map_err(|e| bad(&e))?;
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| Failure::assertion("PNG too large"))?;
    let mut buffer = vec![0; size];
    let info = reader.next_frame(&mut buffer).map_err(|e| bad(&e))?;
    let step = match info.color_type {
        png::ColorType::Rgba => 4,
        png::ColorType::Rgb => 3,
        other => {
            return Err(Failure::assertion(format!(
                "unexpected PNG color type {other:?}"
            )));
        }
    };
    let mut distinct = std::collections::HashSet::new();
    let (mut green, mut magenta) = (0, 0);
    for pixel in buffer[..info.buffer_size()].chunks_exact(step) {
        let color = (pixel[0], pixel[1], pixel[2]);
        distinct.insert(color);
        green += usize::from(color == GREEN);
        magenta += usize::from(color == MAGENTA);
    }
    Ok((distinct.len(), green, magenta))
}

fn click_text_by_ocr(cx: &mut Ctx<'_>) -> Result<(), Failure> {
    let label = cx
        .window("app")?
        .regions
        .get("label")
        .ok_or_else(|| Failure::host("no label region"))?;
    let [left, top, right, bottom] = label;
    let id = control(cx, "app")?;
    let result = cx.call(
        "click_text",
        json!({
            "target_ref": id, "query": "Banana",
            "x": left, "y": top, "width": right - left, "height": bottom - top,
            "observation": {"mode": "none"}
        }),
    )?;
    ensure!(!result.is_error, "click_text failed: {}", result.structured);
    let events = cx.expect("app", "the OCR click", |e| {
        !events::left_downs(e, "main").is_empty()
    })?;
    let down = events::left_downs(&events, "main")[0];
    ensure!(
        cx.window("app")?.regions.contains("label", down),
        "the OCR click {down:?} missed the label {label:?}"
    );
    cx.end()
}

fn owned_dialog(cx: &mut Ctx<'_>) -> Result<(), Failure> {
    let id = cx.target("owner")?;
    let windows = cx.ok("list_windows", json!({}))?;
    let dialog = windows["windows"]
        .as_array()
        .and_then(|all| all.iter().find(|window| window["owner"] == id.as_str()))
        .ok_or_else(|| Failure::assertion(format!("no window owned by the fixture: {windows}")))?;
    let pid = cx.window("owner")?.process_id();
    ensure!(
        dialog["process_id"] == pid,
        "the dialog is another process: {dialog}"
    );
    cx.begin(&id)?;
    cx.focus(&id)?;
    let point = cx.center("owner", "dialog")?;
    let result = cx.click(&id, point)?;
    ensure!(
        !result.is_error,
        "the dialog click was refused: {}",
        result.structured
    );
    let events = cx.expect("owner", "the dialog click", |e| {
        !events::left_downs(e, "dialog").is_empty()
    })?;
    ensure!(
        events::near(events::left_downs(&events, "dialog")[0], point),
        "missed the dialog"
    );
    let button = cx.center("owner", "button")?;
    let result = cx.click(&id, button)?;
    ensure!(!result.is_error, "{}", result.structured);
    cx.expect("owner", "the button press", |e| {
        events::button_pressed(e, "ping")
    })?;
    cx.end()
}

fn foreign_refused(cx: &mut Ctx<'_>) -> Result<(), Failure> {
    let id = control(cx, "ledger")?;
    let foreign = cx.center("journal", "edit")?;
    let refused = cx.refused(
        "click_mouse",
        json!({"target_ref": id, "x": foreign.0, "y": foreign.1, "observation": {"mode": "none"}}),
    )?;
    ensure!(
        refused.error_code() == Some("target_invalidated")
            && refused.error_reason() == Some("foreign_destination")
            && refused.structured["error"]["retryable"] == true,
        "wrong refusal: {}",
        refused.structured
    );
    ensure!(
        refused.structured["input"]["input_outcome"] == "not_started",
        "{}",
        refused.structured
    );
    let edit = cx.center("ledger", "edit")?;
    let accepted = cx.click(&id, edit)?;
    ensure!(
        !accepted.is_error,
        "the session was poisoned: {}",
        accepted.structured
    );
    let events = cx.expect("ledger", "the click on the target", |e| {
        !events::left_downs(e, "main").is_empty()
    })?;
    ensure!(
        events::near(events::left_downs(&events, "main")[0], edit),
        "missed the target"
    );
    let leaked: Vec<Event> = cx
        .window("journal")?
        .events()
        .into_iter()
        .filter(Event::is_input)
        .collect();
    ensure!(
        leaked.is_empty(),
        "the foreign window received input: {leaked:?}"
    );
    cx.end()
}

/// Windows refuses `SetForegroundWindow` from a process that is not in front once the user has
/// worked in another application, unless one of the conditions Microsoft documents holds, such
/// as an expired foreground-lock timeout. The server starts before the user types the request,
/// so on a real desktop its own call is usually refused. This task recreates that: the harness
/// starts the server, then a click is injected into `journal`, the "app the user was working
/// in". `ledger` must then come to the front, by the server's own call when Windows grants it or
/// through the activation helper when it does not, and take a click. A direct grant means the
/// lock was not reproduced; the task records that in a note.
fn focus_after_user_input(cx: &mut Ctx<'_>) -> Result<(), Failure> {
    let ledger = cx.target("ledger")?;
    let journal = cx.center("journal", "edit")?;
    let output = cx.helper(&["--inject-click", &format!("{},{}", journal.0, journal.1)])?;
    ensure!(
        output.status.success(),
        "the fixture could not inject the click"
    );
    cx.expect("journal", "the injected click", |e| {
        !events::left_downs(e, "main").is_empty()
    })?;
    let foreground = cx.ok("list_windows", json!({}))?;
    let journal_id = cx.target("journal")?;
    ensure!(
        foreground["windows"][0]["id"] == journal_id.as_str()
            && foreground["windows"][0]["is_foreground"] == true,
        "the click did not leave journal in front: {foreground}"
    );
    let activations_before = events::activations(&cx.window("ledger")?.events());
    cx.begin(&ledger)?;
    let focused = cx.ok(
        "focus_window",
        json!({"window_id": ledger, "observation": {"mode": "none"}}),
    )?;
    match foreground_path(&focused["input"]) {
        Ok(ForegroundPath::Helper) => {}
        Ok(ForegroundPath::Direct) => cx.note(
            "Windows granted the server's own foreground request, so the foreground lock was not reproduced and the activation helper was not needed",
        ),
        Err(reason) => return Err(Failure::assertion(format!("{reason}: {focused}"))),
    }
    cx.expect("ledger", "an activation after the click", |e| {
        events::activations(e) > activations_before
    })?;
    let windows = cx.ok("list_windows", json!({}))?;
    ensure!(
        windows["windows"][0]["id"] == ledger.as_str()
            && windows["windows"][0]["is_foreground"] == true,
        "ledger is not in front: {windows}"
    );
    let edit = cx.center("ledger", "edit")?;
    let clicked = cx.click(&ledger, edit)?;
    ensure!(
        !clicked.is_error,
        "input after the helper's activation was refused: {}",
        clicked.structured
    );
    cx.expect("ledger", "the click after activation", |e| {
        !events::left_downs(e, "main").is_empty()
    })?;
    cx.end()
}

/// How `focus_window` brought the window to the front, by its `input.activation_helper`.
#[derive(Debug, PartialEq, Eq)]
enum ForegroundPath {
    /// Windows granted the server's own request; the field is absent.
    Direct,
    /// Windows refused the server and the helper's request was accepted.
    Helper,
}

fn foreground_path(input: &serde_json::Value) -> Result<ForegroundPath, String> {
    match &input["activation_helper"] {
        serde_json::Value::Null => Ok(ForegroundPath::Direct),
        helper if helper == "accepted" => Ok(ForegroundPath::Helper),
        helper => Err(format!(
            "the activation helper answered {helper} although focus_window succeeded"
        )),
    }
}

fn taskbar_target(cx: &mut Ctx<'_>) -> Result<(), Failure> {
    let capture = cx.ok(
        "capture_display",
        json!({"max_width": 320, "include_cursor": false}),
    )?;
    let target = capture["target_ref"]
        .as_str()
        .ok_or_else(|| Failure::assertion(format!("no target_ref for the taskbar: {capture}")))?
        .to_owned();
    cx.begin(&target)?;
    cx.end()
}

fn injected_stop_click(cx: &mut Ctx<'_>) -> Result<(), Failure> {
    let id = control(cx, "app")?;
    let deadline = Instant::now() + Duration::from_secs(20);
    let rect: Vec<i32> = loop {
        let output = cx.helper(&["--stop-button-rect"])?;
        if output.status.success() {
            break String::from_utf8_lossy(&output.stdout)
                .split_whitespace()
                .filter_map(|n| n.parse().ok())
                .collect();
        }
        ensure!(
            Instant::now() < deadline,
            "the Stop agent button never appeared during a session"
        );
        thread::sleep(Duration::from_millis(250));
    };
    let [left, top, right, bottom] = <[i32; 4]>::try_from(rect.as_slice())
        .map_err(|_| Failure::assertion(format!("bad button rectangle {rect:?}")))?;
    let center = (left.midpoint(right), top.midpoint(bottom));
    let refused = cx.refused(
        "click_mouse",
        json!({"target_ref": id, "x": center.0, "y": center.1, "observation": {"mode": "none"}}),
    )?;
    ensure!(
        refused.error_reason() == Some("own_indicator")
            && refused.error_message().contains("Stop button"),
        "wrong refusal: {}",
        refused.structured
    );
    let output = cx.helper(&["--inject-click", &format!("{},{}", center.0, center.1)])?;
    ensure!(
        output.status.success(),
        "the fixture could not inject the click"
    );
    thread::sleep(Duration::from_millis(500));
    let status = cx.ok("get_server_status", json!({}))?;
    ensure!(
        status["stop_state"] == "ready" && status.get("stop_reason").is_none(),
        "control was stopped: {status}"
    );
    let edit = cx.center("app", "edit")?;
    let accepted = cx.click(&id, edit)?;
    ensure!(
        !accepted.is_error,
        "control was stopped: {}",
        accepted.structured
    );
    cx.end()
}

/// Windows adds a modifier the user holds to injected input, so Delete would arrive as
/// Shift+Delete and a click as a Shift-click. The fixture holds Shift the way a person does, with
/// a real key-down that stays down. Every input tool must be refused with `key_held` before
/// anything reaches the window, and work again once Shift is up. Shift is released whatever the
/// refusals did, so a failure never leaves it down for later tasks.
fn held_modifier_refused(cx: &mut Ctx<'_>) -> Result<(), Failure> {
    let id = control(cx, "app")?;
    let refusals = refused_while_shift_is_held(cx, &id);
    let released = release_key(cx, "0x10");
    if let (Err(refused), Err(stuck)) = (&refusals, &released) {
        return Err(Failure::new(stuck.kind, format!("{refused}; then {stuck}")));
    }
    refusals?;
    released?;
    // The release is queued input: wait until it arrived before expecting plain Delete.
    cx.expect("app", "Shift going up", |e| {
        e.iter()
            .any(|event| matches!(event, Event::KeyUp { vk: 0x10, .. }))
    })?;
    cx.press(&id, &["delete"])?;
    cx.expect("app", "Delete once Shift was released", |e| {
        e.iter()
            .any(|event| matches!(event, Event::KeyDown { role, vk: 0x2E } if role == "main"))
    })?;
    note_unicode_under_alt(cx)?;
    cx.end()
}

fn refused_while_shift_is_held(cx: &mut Ctx<'_>, id: &str) -> Result<(), Failure> {
    let held = cx.helper(&["--hold-key", "0x10"])?;
    ensure!(held.status.success(), "the fixture could not hold Shift");
    let events = cx.expect("app", "Shift going down", |e| {
        e.iter()
            .any(|event| matches!(event, Event::KeyDown { vk: 0x10, .. }))
    })?;
    let before = events.len();
    let edit = cx.center("app", "edit")?;
    let none = json!({"mode": "none"});
    for (tool, arguments) in [
        (
            "press_keys",
            json!({"target_ref": id, "keys": ["delete"], "observation": none}),
        ),
        (
            "type_text",
            json!({"target_ref": id, "text": "a", "observation": none}),
        ),
        (
            "click_mouse",
            json!({"target_ref": id, "x": edit.0, "y": edit.1, "observation": none}),
        ),
        (
            "scroll_mouse",
            json!({"target_ref": id, "x": edit.0, "y": edit.1, "delta_y": -120, "observation": none}),
        ),
    ] {
        let refused = cx.refused(tool, arguments)?;
        ensure!(
            refused.error_reason() == Some("key_held")
                && refused.structured["status"] == "not_started"
                && refused.structured["input"]["sent_events"] == 0,
            "{tool} was not refused before sending anything: {}",
            refused.structured
        );
    }
    thread::sleep(Duration::from_millis(300));
    let leaked: Vec<Event> = cx.window("app")?.events()[before..]
        .iter()
        .filter(|event| event.is_input())
        .cloned()
        .collect();
    ensure!(
        leaked.is_empty(),
        "input reached the window while Shift was held: {leaked:?}"
    );
    Ok(())
}

/// Records, without judging, how a Unicode packet typed under a held Alt reaches a window: as a
/// character, or as a menu mnemonic (`syschar`). The server refuses typing under Alt either way,
/// so this documents why rather than testing the server. Alt is released whatever happens.
fn note_unicode_under_alt(cx: &mut Ctx<'_>) -> Result<(), Failure> {
    let before = cx.window("app")?.events().len();
    let held = cx.helper(&["--hold-key", "0x12"]);
    let typed = match &held {
        Ok(output) if output.status.success() => cx.helper(&["--inject-unicode", "U+0061"]),
        _ => Err(Failure::host("Alt was not held, so nothing was typed")),
    };
    // Released even if holding or typing failed, before any of their errors is reported.
    release_key(cx, "0x12")?;
    ensure!(
        held?.status.success() && typed?.status.success(),
        "the fixture could not type under Alt"
    );
    thread::sleep(Duration::from_millis(300));
    let arrived: Vec<Event> = cx.window("app")?.events()[before..].to_vec();
    cx.note(format!(
        "a Unicode packet under a held Alt arrived as {arrived:?}"
    ));
    Ok(())
}

/// Releases a key the fixture held with `--hold-key`. Windows keeps an injected key down until
/// its key-up, across processes and later tasks, so a failure here is reported, never ignored.
fn release_key(cx: &mut Ctx<'_>, key: &str) -> Result<(), Failure> {
    let output = cx.helper(&["--release-key", key])?;
    ensure!(
        output.status.success(),
        "the fixture could not release key {key}; it may still be held"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_foreground_may_come_from_a_direct_grant_or_the_helper() {
        assert_eq!(
            foreground_path(&json!({"input_outcome": "delivered"})),
            Ok(ForegroundPath::Direct)
        );
        assert_eq!(
            foreground_path(&json!({"activation_helper": "accepted"})),
            Ok(ForegroundPath::Helper)
        );
        for helper in ["refused", "failed"] {
            assert!(foreground_path(&json!({ "activation_helper": helper })).is_err());
        }
    }
}
