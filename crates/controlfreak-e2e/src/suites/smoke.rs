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

fn type_unicode(cx: &mut Ctx<'_>) -> Result<(), Failure> {
    let id = control(cx, "app")?;
    cx.type_text(&id, "Hi, \u{e9}\u{65e5}\r\n\tz")?;
    let wanted = "Hi, \u{e9}\u{65e5}\u{23CE}\u{21E5}z";
    let events = cx.expect("app", "all typed text", |e| {
        events::typed(e).chars().count() >= wanted.chars().count()
    })?;
    let typed = events::typed(&events);
    ensure!(typed == wanted, "typed {typed:?}, wanted {wanted:?}");
    cx.end()
}

/// An application that takes several queued keystrokes before translating them turns a burst of
/// Unicode packets into copies of the last character ("Hello,!!!!!!!!!!!" in Notepad). Every
/// character must arrive as typed.
fn type_into_busy_app(cx: &mut Ctx<'_>) -> Result<(), Failure> {
    let id = control(cx, "app")?;
    let text = "Hello, world! \u{e9}\u{65e5} 0123456789\tz";
    cx.type_text(&id, text)?;
    let wanted = text.replace('\t', "\u{21E5}");
    let events = cx.expect("app", "all typed text", |e| {
        events::typed(e).chars().count() >= wanted.chars().count()
    })?;
    let typed = events::typed(&events);
    ensure!(typed == wanted, "typed {typed:?}, wanted {wanted:?}");
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

/// Windows lets a process take the foreground only while no input has arrived since it started.
/// The server starts before the user types the request, so on a real desktop its own
/// `SetForegroundWindow` is refused from then on. This task recreates that: the harness starts
/// the server, then a click is injected into `journal`, the "app the user was working in". The
/// server's own request for `ledger` must now be refused, and the activation helper must still
/// bring `ledger` to the front.
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
    ensure!(
        focused["input"]["activation_helper"] == "accepted",
        "the foreground lock was not reproduced or not overcome: the server's own request must be refused and the helper's accepted: {focused}"
    );
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
