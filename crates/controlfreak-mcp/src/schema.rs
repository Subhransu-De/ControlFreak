use super::{
    Arc, BEGIN_CONTROL_SESSION, CAPTURE_DISPLAY, CAPTURE_REGION, CAPTURE_VISUAL_BASELINE,
    CAPTURE_WINDOW, CLICK_MOUSE, CLICK_TEXT, DRAG_MOUSE, END_CONTROL_SESSION, FIND_TEXT_ON_SCREEN,
    FOCUS_WINDOW, GET_SERVER_STATUS, JsonObject, LIST_DISPLAYS, LIST_VIRTUAL_DESKTOPS,
    LIST_WINDOWS, MOVE_MOUSE, PRESS_KEYS, READ_TEXT_IN_REGION, RUN_ACTIONS, SCROLL_MOUSE,
    STOP_DESKTOP_WORK, SWITCH_VIRTUAL_DESKTOP, TYPE_TEXT, Tool, ToolAnnotations, Value,
    WAIT_FOR_CHANGE_SINCE, WAIT_FOR_TEXT, WAIT_FOR_VISUAL_CHANGE, WAIT_FOR_WINDOW, json,
    requests::MAX_RUN_ACTION_STEPS,
};
use controlfreak_core::limits::{
    DEFAULT_CAPTURE_WIDTH, MAX_CAPTURE_WIDTH, MAX_MOVE_DURATION_MS, MAX_RUN_ACTIONS_WAIT_MS,
    MAX_SCROLL_DELTA, MAX_TEXT_UTF16_UNITS, MAX_WAIT_MS, MIN_WAIT_MS,
};

/// How a tool touches the desktop. It decides the annotations, which every client reads.
#[derive(Clone, Copy)]
enum Kind {
    /// Reads desktop or server state only.
    Observe { idempotent: bool },
    /// Sends keyboard, mouse or focus input to the desktop.
    Input { idempotent: bool },
    /// Changes the control session without sending input.
    Session,
    /// Permanently closes input until the server restarts.
    Stop,
}

impl Kind {
    fn annotations(self, title: &str) -> ToolAnnotations {
        let (read_only, destructive, idempotent, open_world) = match self {
            Self::Observe { idempotent } => (true, false, idempotent, false),
            Self::Input { idempotent } => (false, true, idempotent, true),
            Self::Session => (false, false, true, false),
            Self::Stop => (false, true, true, false),
        };
        ToolAnnotations::with_title(title)
            .read_only(read_only)
            .destructive(destructive)
            .idempotent(idempotent)
            .open_world(open_world)
    }
}

fn tool(
    name: &'static str,
    title: &str,
    kind: Kind,
    description: &str,
    input: JsonObject,
    output: Arc<JsonObject>,
) -> Tool {
    Tool::new(name, description.to_owned(), input)
        .with_title(title)
        .with_raw_output_schema(output)
        .with_annotations(kind.annotations(title))
}

/// Tool descriptions and schemas used by both `tools/list` and offline contract export.
///
/// The list starts with the tools an agent needs first and ends with the emergency stop.
pub fn tools() -> Vec<Tool> {
    let mut tools = vec![
        get_server_status_tool(),
        begin_control_session_tool(),
        end_control_session_tool(),
        list_displays_tool(),
        capture_display_tool(),
        capture_region_tool(),
        wait_for_visual_change_tool(),
        capture_visual_baseline_tool(),
        wait_for_change_since_tool(),
        read_text_in_region_tool(),
        find_text_on_screen_tool(),
        wait_for_text_tool(),
        click_text_tool(),
        list_virtual_desktops_tool(),
        switch_virtual_desktop_tool(),
        list_windows_tool(),
        focus_window_tool(),
        capture_window_tool(),
        wait_for_window_tool(),
        move_mouse_tool(),
        click_mouse_tool(),
        drag_mouse_tool(),
        scroll_mouse_tool(),
        press_keys_tool(),
        type_text_tool(),
        run_actions_tool(),
        stop_desktop_work_tool(),
    ];
    for tool in &mut tools {
        if super::is_mutating_tool(&tool.name) && tool.name != FOCUS_WINDOW
            || tool.name == BEGIN_CONTROL_SESSION
        {
            let schema = Arc::make_mut(&mut tool.input_schema);
            schema
                .get_mut("properties")
                .and_then(Value::as_object_mut)
                .expect("targeted tool schemas define a properties object")
                .insert(
                    "target_ref".into(),
                    json!({"type": "string", "minLength": 1,
                    "description": "Approved window, from list_windows or a screenshot."}),
                );
            // Switching desktops may omit the target: it defaults to the session's window.
            if tool.name != SWITCH_VIRTUAL_DESKTOP {
                schema
                    .get_mut("required")
                    .and_then(Value::as_array_mut)
                    .expect("targeted tool schemas define a required array")
                    .push(json!("target_ref"));
            }
        }
    }
    tools
}

fn stop_desktop_work_tool() -> Tool {
    tool(
        STOP_DESKTOP_WORK,
        "Emergency stop",
        Kind::Stop,
        "To finish normally, call end_control_session instead. This emergency stop cancels all desktop work and closes input permanently until the user restarts the server. Do not call it to clean up. It cannot undo input that was already sent.",
        object_schema(JsonObject::new(), &[]),
        output_schema(
            vec![json!({
                "type": "object",
                "properties": {"status": {"enum": ["ready", "draining", "stopped", "cleanup_failed"]}},
                "required": ["status"],
                "additionalProperties": false
            })],
            false,
        ),
    )
}

fn begin_control_session_tool() -> Tool {
    let mut properties = JsonObject::new();
    properties.insert(
        "expected_seconds".to_owned(),
        json!({
            "type": "integer",
            "minimum": 1,
            "description": "Idle hold in seconds. Default 45. The server caps it, at 120 unless the operator changed the cap."
        }),
    );
    tool(
        BEGIN_CONTROL_SESSION,
        "Begin control session",
        Kind::Session,
        "Start here before any input. Reserves desktop control for ONE window (target_ref from list_windows or a screenshot) and shows the user the control indicator. Input tools work only while that window is in the foreground, so call focus_window next if it is not. Dialogs and menus of the same application are accepted (for File Explorer, only its own dialogs and menus, not the taskbar or other folder windows). A window from another process needs end_control_session and a new session. Error desktop_in_use is retryable: another ControlFreak server owns the desktop.",
        object_schema(properties, &[]),
        session_change_output_schema("armed"),
    )
}

fn end_control_session_tool() -> Tool {
    tool(
        END_CONTROL_SESSION,
        "End control session",
        Kind::Session,
        "The normal way to finish. Stops accepting input and releases desktop control after pending actions finish. Call it when the task is done, or before approving another window. Safe to call twice.",
        object_schema(JsonObject::new(), &[]),
        session_change_output_schema("closing"),
    )
}

fn get_server_status_tool() -> Tool {
    tool(
        GET_SERVER_STATUS,
        "Server status",
        Kind::Observe { idempotent: true },
        "Return version, uptime, privileges, indicator health, session state, stop_state and recent operations. stop_reason=user_stop means the user ended control: do not resume without their permission.",
        object_schema(JsonObject::new(), &[]),
        server_status_output_schema(),
    )
}

fn list_displays_tool() -> Tool {
    tool(
        LIST_DISPLAYS,
        "List displays",
        Kind::Observe { idempotent: true },
        "List active displays: ID, name, primary flag and bounds in physical pixels (left and top can be negative). Every display_id parameter defaults to the primary display, so call this only on multi-display setups.",
        object_schema(JsonObject::new(), &[]),
        output_schema(
            vec![json!({
                "type": "object",
                "properties": {
                    "count": { "type": "integer", "minimum": 0 },
                    "displays": { "type": "array", "items": def_ref("display") }
                },
                "required": ["count", "displays"],
                "additionalProperties": false
            })],
            false,
        ),
    )
}

fn capture_display_tool() -> Tool {
    let mut properties = JsonObject::new();
    properties.insert("display_id".to_owned(), display_id_property());
    add_capture_options(&mut properties);
    tool(
        CAPTURE_DISPLAY,
        "Capture display",
        Kind::Observe { idempotent: false },
        "Capture a whole display as PNG, 1280 px wide by default (max_width changes it, native=true keeps full resolution). The result's observation_id maps image points to screen pixels for pointer tools. Protected or HDR content may render incorrectly.",
        object_schema(properties, &[]),
        output_schema(vec![screenshot_schema()], false),
    )
}

fn capture_region_tool() -> Tool {
    let mut properties = region_properties();
    add_capture_options(&mut properties);
    tool(
        CAPTURE_REGION,
        "Capture region",
        Kind::Observe { idempotent: false },
        "Capture a rectangle as PNG, cheaper than a full capture when one area matters. x, y, width, height are display-local physical pixels. Width defaults to 1280 (max_width changes it, native=true keeps full resolution).",
        object_schema(properties, &["x", "y", "width", "height"]),
        output_schema(vec![screenshot_schema()], false),
    )
}

fn wait_properties() -> JsonObject {
    let mut properties = JsonObject::new();
    properties.insert("timeout_ms".to_owned(), timeout_property());
    properties.insert(
        "stable_ms".to_owned(),
        json!({ "type": "integer", "minimum": 0, "maximum": MAX_WAIT_MS, "default": 250 }),
    );
    properties.insert(
        "difference_threshold".to_owned(),
        json!({ "type": "number", "exclusiveMinimum": 0, "maximum": 1, "default": 0.01 }),
    );
    properties
}

fn timeout_property() -> Value {
    json!({ "type": "integer", "minimum": MIN_WAIT_MS, "maximum": MAX_WAIT_MS, "default": 5000 })
}

fn wait_for_visual_change_tool() -> Tool {
    let mut properties = region_properties();
    properties.extend(wait_properties());
    tool(
        WAIT_FOR_VISUAL_CHANGE,
        "Wait for visual change",
        Kind::Observe { idempotent: false },
        "Wait until a region differs from how it looks at call time and then stays stable for stable_ms, and return its screenshot. A timeout (default 5000 ms, at most 30000; 0 checks once) is a normal result with timed_out=true, not an error. To compare against a moment before an action, use capture_visual_baseline and wait_for_change_since.",
        object_schema(properties, &["x", "y", "width", "height"]),
        visual_change_output_schema(),
    )
}

fn capture_visual_baseline_tool() -> Tool {
    tool(
        CAPTURE_VISUAL_BASELINE,
        "Save visual baseline",
        Kind::Observe { idempotent: false },
        "Save a region's appearance without returning an image. Call it BEFORE an action, then call wait_for_change_since afterwards. Baselines last 5 minutes and at most 16 are kept.",
        object_schema(region_properties(), &["x", "y", "width", "height"]),
        output_schema(
            vec![json!({
                "type": "object",
                "properties": {
                    "baseline_id": { "type": "string", "pattern": "^vb-[0-9a-f]{32}-[0-9a-f]{16}$" },
                    "display": def_ref("display"),
                    "source_bounds": def_ref("bounds"),
                    "fingerprint": { "type": "string" },
                    "expires_in_ms": { "type": "integer", "minimum": 0 }
                },
                "required": ["baseline_id", "display", "source_bounds", "fingerprint", "expires_in_ms"],
                "additionalProperties": false
            })],
            false,
        ),
    )
}

fn wait_for_change_since_tool() -> Tool {
    let mut properties = wait_properties();
    properties.insert(
        "baseline_id".to_owned(),
        json!({
            "type": "string", "pattern": "^vb-[0-9a-f]{32}-[0-9a-f]{16}$",
            "description": "From capture_visual_baseline."
        }),
    );
    tool(
        WAIT_FOR_CHANGE_SINCE,
        "Wait for change since baseline",
        Kind::Observe { idempotent: false },
        "Wait until a region differs from its saved baseline and is stable, then return a screenshot. A timeout (default 5000 ms, at most 30000) is a normal result. The baseline can be reused until it expires after 5 minutes.",
        object_schema(properties, &["baseline_id"]),
        visual_change_output_schema(),
    )
}

fn read_text_in_region_tool() -> Tool {
    let mut properties = region_properties();
    add_ocr_language(&mut properties);
    tool(
        READ_TEXT_IN_REGION,
        "Read text (OCR)",
        Kind::Observe { idempotent: false },
        "Read the text in a region with OCR. Returns lines and words with display-local pixel bounds, no image and no confidence scores. Needs a Windows OCR language pack.",
        object_schema(properties, &["x", "y", "width", "height"]),
        read_text_output_schema(),
    )
}

fn text_query_properties() -> JsonObject {
    let mut properties = region_properties();
    add_ocr_language(&mut properties);
    properties.insert(
        "query".to_owned(),
        json!({ "type": "string", "minLength": 1 }),
    );
    properties.insert(
        "case_sensitive".to_owned(),
        json!({ "type": "boolean", "default": false }),
    );
    properties.insert(
        "max_results".to_owned(),
        json!({ "type": "integer", "minimum": 1, "maximum": 100, "default": 20 }),
    );
    properties.insert("match_mode".into(), json!({"type": "string", "enum": ["exact", "substring", "tolerant"], "default": "substring", "description": "tolerant tries normalization, two adjacent lines, then optional OCR confusions; the first non-empty tier wins."}));
    properties.insert(
        "ocr_confusions".into(),
        json!({"type": "boolean", "default": false, "description": "Tolerant only: allow one 0/O/o or 1/I/i/l/| substitution in a 4-64 character label."}),
    );
    properties
}

fn find_text_on_screen_tool() -> Tool {
    tool(
        FIND_TEXT_ON_SCREEN,
        "Find text (OCR)",
        Kind::Observe { idempotent: false },
        "Find text in a region with OCR. Discovery only: it never sends input. Returns display-local bounds, match_tier, the full candidate_count and truncation flags. Use it to pick a query and region before click_text.",
        object_schema(
            text_query_properties(),
            &["x", "y", "width", "height", "query"],
        ),
        find_text_output_schema(),
    )
}

fn wait_for_text_tool() -> Tool {
    let mut properties = text_query_properties();
    properties.insert(
        "condition".to_owned(),
        json!({ "type": "string", "enum": ["appears", "disappears"], "default": "appears" }),
    );
    properties.insert("timeout_ms".to_owned(), timeout_property());
    tool(
        WAIT_FOR_TEXT,
        "Wait for text (OCR)",
        Kind::Observe { idempotent: false },
        "Repeat OCR over a region until query appears or disappears, or timeout_ms (default 5000, at most 30000) elapses. Returns satisfied, timed_out, attempts and the last matches. Each pass is a full recognition, so keep the region small. Never sends input.",
        object_schema(properties, &["x", "y", "width", "height", "query"]),
        wait_for_text_output_schema(),
    )
}

fn click_text_tool() -> Tool {
    let mut properties = text_query_properties();
    properties.remove("max_results");
    properties.remove("match_mode");
    properties.remove("ocr_confusions");
    properties.insert(
        "exact_match".to_owned(),
        json!({
            "type": "boolean", "default": true,
            "description": "false accepts a unique substring."
        }),
    );
    properties.insert("button".to_owned(), button_property(true));
    properties.insert("click_count".to_owned(), click_count_property());
    properties.insert("modifiers".to_owned(), modifier_schema());
    properties.insert("duration_ms".to_owned(), duration_property());
    add_observation(&mut properties);
    tool(
        CLICK_TEXT,
        "Click text (OCR)",
        Kind::Input { idempotent: false },
        "Click the unique visible text match inside a region. Needs a session whose window is in the foreground. Zero or several matches return ocr_no_match or ocr_ambiguous_match with candidates and send no input: narrow the region or look with find_text_on_screen. Success reports matched_text and match_tier.",
        object_schema(properties, &["x", "y", "width", "height", "query"]),
        action_output_schema(true),
    )
}

fn list_virtual_desktops_tool() -> Tool {
    tool(
        LIST_VIRTUAL_DESKTOPS,
        "List virtual desktops",
        Kind::Observe { idempotent: true },
        "Group the discoverable titled windows by virtual desktop and mark the current one. Empty desktops, names and desktop order are not available.",
        object_schema(JsonObject::new(), &[]),
        output_schema(
            vec![json!({
                "type": "object",
                "properties": {
                    "count": { "type": "integer", "minimum": 0 },
                    "desktops": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string" },
                                "is_current": { "type": "boolean" },
                                "application_count": { "type": "integer", "minimum": 0 },
                                "windows": { "type": "array", "items": def_ref("window") }
                            },
                            "required": ["id", "is_current", "application_count", "windows"],
                            "additionalProperties": false
                        }
                    },
                    "includes_empty_desktops": { "const": false },
                    "order_available": { "const": false },
                    "names_available": { "const": false }
                },
                "required": ["count", "desktops", "includes_empty_desktops", "order_available", "names_available"],
                "additionalProperties": false
            })],
            false,
        ),
    )
}

fn switch_virtual_desktop_tool() -> Tool {
    let mut properties = JsonObject::new();
    properties.insert(
        "direction".to_owned(),
        json!({ "type": "string", "enum": ["left", "right"] }),
    );
    properties.insert(
        "steps".to_owned(),
        json!({ "type": "integer", "minimum": 1, "maximum": 10, "default": 1, "description": "Windows stops at the last desktop." }),
    );
    add_observation(&mut properties);
    tool(
        SWITCH_VIRTUAL_DESKTOP,
        "Switch virtual desktop",
        Kind::Input { idempotent: false },
        "Switch left or right across existing virtual desktops. Needs the session's window in the foreground. The switch ends the control session, because that window is no longer on screen: begin a new session on a window of the new desktop. target_ref is optional and defaults to the session's window, or to the foreground window when no session is active. changed=false means no switch was detected.",
        object_schema(properties, &["direction"]),
        action_output_schema(false),
    )
}

fn list_windows_tool() -> Tool {
    tool(
        LIST_WINDOWS,
        "List windows",
        Kind::Observe { idempotent: true },
        "List visible titled top-level windows, foreground first, then by title. Dialogs and other owned windows are listed too, with an owner field naming the window they belong to. Each id is a target_ref for begin_control_session, focus_window and capture_window. Returns at most 200; truncated=true means more exist, so use wait_for_window with title_contains, class_name or process_id to reach them. Refresh ids after windows open, close or change owner.",
        object_schema(JsonObject::new(), &[]),
        output_schema(
            vec![json!({
                "type": "object",
                "properties": {
                    "count": { "type": "integer", "minimum": 0 },
                    "truncated": { "type": "boolean" },
                    "windows": { "type": "array", "items": def_ref("window") }
                },
                "required": ["count", "truncated", "windows"],
                "additionalProperties": false
            })],
            false,
        ),
    )
}

fn focus_window_tool() -> Tool {
    let mut properties = JsonObject::new();
    properties.insert(
        "timeout_ms".into(),
        json!({ "type": "integer", "minimum": 100, "maximum": 5000, "default": 1000 }),
    );
    add_window_id(&mut properties);
    add_observation(&mut properties);
    tool(
        FOCUS_WINDOW,
        "Focus window",
        Kind::Input { idempotent: true },
        "Bring the session's approved window to the foreground, restoring it if minimized, and verify it. Call it right after begin_control_session, and again whenever a window from another process took the foreground. It never sends keys or clicks to get focus: if Windows refuses, ask the user to click the window. window_id (or its alias target_ref) must be the approved window.",
        object_schema(properties, &[]),
        action_output_schema(false),
    )
}

fn capture_window_tool() -> Tool {
    let mut properties = JsonObject::new();
    add_window_id(&mut properties);
    add_capture_options(&mut properties);
    tool(
        CAPTURE_WINDOW,
        "Capture window",
        Kind::Observe { idempotent: false },
        "Capture one window as PNG without focusing it. window_id (or its alias target_ref) comes from list_windows, and must be fresh. Restore minimized windows first. Width defaults to 1280 (max_width changes it, native=true keeps full resolution).",
        object_schema(properties, &[]),
        output_schema(vec![window_screenshot_schema()], false),
    )
}

fn wait_for_window_tool() -> Tool {
    let mut properties = JsonObject::new();
    add_window_id(&mut properties);
    properties.insert(
        "title_contains".to_owned(),
        json!({ "type": "string", "minLength": 1, "description": "Case-insensitive substring." }),
    );
    properties.insert(
        "class_name".to_owned(),
        json!({ "type": "string", "minLength": 1, "description": "Case-insensitive exact Win32 class." }),
    );
    properties.insert(
        "process_id".to_owned(),
        json!({ "type": "integer", "minimum": 1 }),
    );
    properties.insert("is_foreground".to_owned(), json!({ "type": "boolean" }));
    properties.insert("timeout_ms".to_owned(), timeout_property());
    tool(
        WAIT_FOR_WINDOW,
        "Wait for window",
        Kind::Observe { idempotent: false },
        "Wait for a visible top-level window that matches every criterion given: window_id (or target_ref), title_contains, class_name, process_id, is_foreground. At least one is required. A timeout (default 5000 ms, at most 30000) returns matched=false and timed_out=true.",
        object_schema(properties, &[]),
        output_schema(
            vec![json!({
                "type": "object",
                "properties": {
                    "matched": { "type": "boolean" },
                    "timed_out": { "type": "boolean" },
                    "elapsed_ms": { "type": "integer", "minimum": 0 },
                    "window": { "oneOf": [def_ref("window"), { "type": "null" }] }
                },
                "required": ["matched", "timed_out", "elapsed_ms", "window"],
                "additionalProperties": false
            })],
            false,
        ),
    )
}

fn move_mouse_tool() -> Tool {
    tool(
        MOVE_MOUSE,
        "Move mouse",
        Kind::Input { idempotent: true },
        "Move the pointer onto the approved window. Needs the window in the foreground. Give x and y (display-local physical pixels; display_id defaults to primary), or observation_id with image_x and image_y from a screenshot.",
        object_schema(pointer_properties(), &[]),
        action_output_schema(true),
    )
}

fn click_mouse_tool() -> Tool {
    let mut properties = pointer_properties();
    properties.insert("button".to_owned(), button_property(true));
    properties.insert("click_count".to_owned(), click_count_property());
    properties.insert("modifiers".to_owned(), modifier_schema());
    tool(
        CLICK_MOUSE,
        "Click mouse",
        Kind::Input { idempotent: false },
        "Click on the approved window. Needs the window in the foreground. Give x and y (display-local physical pixels; display_id defaults to primary), or observation_id with image_x and image_y from a screenshot. Modifiers are held for the click and always released. button defaults to left.",
        object_schema(properties, &[]),
        action_output_schema(true),
    )
}

fn drag_mouse_tool() -> Tool {
    let mut properties = JsonObject::new();
    for prefix in ["start", "end"] {
        properties.insert(
            format!("{prefix}_display_id"),
            json!({ "type": "string", "minLength": 1 }),
        );
        properties.insert(
            format!("{prefix}_x"),
            json!({ "type": "integer", "minimum": 0 }),
        );
        properties.insert(
            format!("{prefix}_y"),
            json!({ "type": "integer", "minimum": 0 }),
        );
    }
    properties.insert(
        "observation_id".to_owned(),
        json!({ "type": "string", "minLength": 1 }),
    );
    for prefix in ["start", "end"] {
        for axis in ["x", "y"] {
            properties.insert(
                format!("{prefix}_image_{axis}"),
                json!({ "type": "integer", "minimum": 0 }),
            );
        }
    }
    properties.insert("button".to_owned(), button_property(true));
    properties.insert("modifiers".to_owned(), modifier_schema());
    properties.insert("duration_ms".to_owned(), duration_property());
    add_observation(&mut properties);
    tool(
        DRAG_MOUSE,
        "Drag mouse",
        Kind::Input { idempotent: false },
        "Press, move and release in one call. Needs the approved window in the foreground. Give each end as start_x/start_y or end_x/end_y (start_display_id and end_display_id default to primary), or as start_image_x/start_image_y and end_image_x/end_image_y in one screenshot's observation_id. The button and modifiers are always released.",
        object_schema(properties, &[]),
        action_output_schema(true),
    )
}

fn scroll_mouse_tool() -> Tool {
    let mut properties = pointer_properties();
    properties.insert(
        "delta_x".to_owned(),
        json!({
            "type": "integer",
            "minimum": -i64::from(MAX_SCROLL_DELTA),
            "maximum": MAX_SCROLL_DELTA,
            "default": 0
        }),
    );
    properties.insert(
        "delta_y".to_owned(),
        json!({
            "type": "integer",
            "minimum": -i64::from(MAX_SCROLL_DELTA),
            "maximum": MAX_SCROLL_DELTA,
            "description": "Positive scrolls up, negative down. 120 is one wheel detent."
        }),
    );
    tool(
        SCROLL_MOUSE,
        "Scroll mouse",
        Kind::Input { idempotent: false },
        "Move to a point on the approved window, then scroll. Needs the window in the foreground. Give x and y (display_id defaults to primary) or observation_id with image_x and image_y. delta_x scrolls right when positive.",
        object_schema(properties, &["delta_y"]),
        action_output_schema(true),
    )
}

/// Every canonical key name `press_keys` accepts, straight from the core key table.
pub(super) fn all_key_names() -> Vec<String> {
    controlfreak_core::Key::ALL
        .iter()
        .map(|key| key.canonical_name().to_owned())
        .collect()
}

fn key_names_description() -> String {
    // Letters, digits and F-keys are summarised by range to keep tools/list small.
    let named: Vec<String> = all_key_names()
        .into_iter()
        .filter(|name| {
            name.len() > 1 && !(name.starts_with('f') && name[1..].parse::<u8>().is_ok())
        })
        .collect();
    format!(
        "One chord pressed together and released in reverse order. Case-insensitive names: {}, a-z, 0-9, f1-f12. Common aliases (esc, del, return, up, down, left, right) and the symbols themselves also work. Use type_text for text.",
        named.join(", ")
    )
}

fn press_keys_tool() -> Tool {
    let mut properties = JsonObject::new();
    properties.insert(
        "keys".to_owned(),
        json!({
            "type": "array",
            "minItems": 1,
            "maxItems": 8,
            "uniqueItems": true,
            "description": key_names_description(),
            "items": { "type": "string", "minLength": 1 }
        }),
    );
    add_observation(&mut properties);
    tool(
        PRESS_KEYS,
        "Press keys",
        Kind::Input { idempotent: false },
        "Send a key or chord to the approved window. Needs the window in the foreground; if it is not, call focus_window first. Refused if another process took the foreground.",
        object_schema(properties, &["keys"]),
        action_output_schema(false),
    )
}

fn run_actions_tool() -> Tool {
    let mut step = pointer_properties();
    step.remove("duration_ms");
    step.remove("observation");
    step.insert(
        "action".to_owned(),
        json!({ "type": "string", "enum": ["click", "scroll", "type_text", "press_keys", "wait"] }),
    );
    step.insert("button".to_owned(), button_property(true));
    step.insert("click_count".to_owned(), click_count_property());
    step.insert("modifiers".to_owned(), modifier_schema());
    step.insert(
        "delta_x".to_owned(),
        json!({ "type": "integer", "minimum": -i64::from(MAX_SCROLL_DELTA), "maximum": MAX_SCROLL_DELTA, "default": 0 }),
    );
    step.insert(
        "delta_y".to_owned(),
        json!({ "type": "integer", "minimum": -i64::from(MAX_SCROLL_DELTA), "maximum": MAX_SCROLL_DELTA, "default": 0 }),
    );
    step.insert(
        "text".to_owned(),
        json!({ "type": "string", "minLength": 1, "maxLength": MAX_TEXT_UTF16_UNITS }),
    );
    step.insert(
        "keys".to_owned(),
        json!({ "type": "array", "minItems": 1, "maxItems": 8, "uniqueItems": true, "items": { "type": "string", "minLength": 1 } }),
    );
    step.insert(
        "wait_ms".to_owned(),
        json!({ "type": "integer", "minimum": 1, "maximum": MAX_RUN_ACTIONS_WAIT_MS }),
    );
    let mut properties = JsonObject::new();
    properties.insert(
        "steps".to_owned(),
        json!({
            "type": "array",
            "minItems": 1,
            "maxItems": MAX_RUN_ACTION_STEPS,
            "description": "click and scroll take x/y or observation_id/image_x/image_y; type_text takes text; press_keys takes keys as in press_keys; wait takes wait_ms.",
            "items": {
                "type": "object",
                "properties": step,
                "required": ["action"],
                "additionalProperties": false
            }
        }),
    );
    add_observation(&mut properties);
    tool(
        RUN_ACTIONS,
        "Run action batch",
        Kind::Input { idempotent: false },
        "Run up to 16 click, scroll, type_text, press_keys and wait steps in order on the approved window, with one final screenshot. Needs the window in the foreground. Every step is checked before the first runs. The batch stops at the first failure and reports each step as completed, failed or skipped. A wait step pauses up to 5 s and cannot be the last step. Use it for a known sequence such as filling a form, and observe between steps when the next one depends on the result.",
        object_schema(properties, &["steps"]),
        action_output_schema(false),
    )
}

fn type_text_tool() -> Tool {
    let mut properties = JsonObject::new();
    properties.insert(
        "text".to_owned(),
        json!({
            "type": "string",
            "minLength": 1,
            "maxLength": MAX_TEXT_UTF16_UNITS,
        }),
    );
    add_observation(&mut properties);
    tool(
        TYPE_TEXT,
        "Type text",
        Kind::Input { idempotent: false },
        "Type Unicode text into the approved window without the clipboard. Needs the window in the foreground and revalidates it for each batch of characters. Use press_keys for shortcuts and special keys.",
        object_schema(properties, &["text"]),
        action_output_schema(false),
    )
}

fn display_id_property() -> Value {
    json!({
        "type": "string",
        "minLength": 1,
        "description": "Display ID from list_displays, or \"primary\" (default)."
    })
}

fn add_window_id(properties: &mut JsonObject) {
    properties.insert(
        "window_id".to_owned(),
        json!({
            "type": "string",
            "minLength": 1,
            "description": "Window ID from list_windows or a screenshot. Alias: target_ref."
        }),
    );
    properties.insert(
        "target_ref".to_owned(),
        json!({ "type": "string", "minLength": 1, "description": "Alias of window_id." }),
    );
}

fn add_capture_options(properties: &mut JsonObject) {
    properties.insert(
        "max_width".to_owned(),
        json!({
            "type": "integer", "minimum": 1, "maximum": MAX_CAPTURE_WIDTH, "default": DEFAULT_CAPTURE_WIDTH,
            "description": "Maximum output width; aspect ratio is kept. Use source_bounds and the image size to map coordinates."
        }),
    );
    properties.insert(
        "native".to_owned(),
        json!({
            "type": "boolean", "default": false,
            "description": "Return full resolution. Large images cost many tokens. Cannot be combined with max_width."
        }),
    );
    properties.insert(
        "include_cursor".to_owned(),
        json!({ "type": "boolean", "default": true, "description": "Draw the magenta cursor marker." }),
    );
}

fn add_ocr_language(properties: &mut JsonObject) {
    properties.insert(
        "language".to_owned(),
        json!({
            "type": "string", "minLength": 2,
            "description": "Installed BCP-47 OCR language such as en-US. Default: the Windows profile languages."
        }),
    );
}

fn add_observation(properties: &mut JsonObject) {
    properties.insert(
        "observation".to_owned(),
        json!({
            "type": "object",
            "description": "Default: 1280 px screenshot. mode none or metadata sends no image; region crops.",
            "properties": {
                "mode": { "type": "string", "enum": ["none", "metadata", "screenshot"], "default": "screenshot" },
                "max_width": { "type": "integer", "minimum": 1, "maximum": MAX_CAPTURE_WIDTH },
                "include_cursor": { "type": "boolean", "default": true },
                "region": {
                    "type": "object",
                    "properties": {
                        "display_id": { "type": "string", "minLength": 1 },
                        "x": { "type": "integer", "minimum": 0 },
                        "y": { "type": "integer", "minimum": 0 },
                        "width": { "type": "integer", "minimum": 1 },
                        "height": { "type": "integer", "minimum": 1 }
                    },
                    "required": ["x", "y", "width", "height"],
                    "additionalProperties": false
                }
            },
            "additionalProperties": false
        }),
    );
}

fn region_properties() -> JsonObject {
    let mut properties = JsonObject::new();
    properties.insert("display_id".to_owned(), display_id_property());
    for (name, minimum) in [("x", 0), ("y", 0), ("width", 1), ("height", 1)] {
        properties.insert(
            name.to_owned(),
            json!({ "type": "integer", "minimum": minimum }),
        );
    }
    properties
}

fn button_property(default_left: bool) -> Value {
    let mut schema = json!({ "type": "string", "enum": ["left", "right", "middle"] });
    if default_left {
        schema["default"] = json!("left");
    }
    schema
}

fn click_count_property() -> Value {
    json!({ "type": "integer", "minimum": 1, "maximum": 3, "default": 1 })
}

fn duration_property() -> Value {
    json!({ "type": "integer", "minimum": 0, "maximum": MAX_MOVE_DURATION_MS, "default": 0, "description": "Smooth movement time in ms." })
}

fn modifier_schema() -> Value {
    json!({
        "type": "array",
        "maxItems": 4,
        "uniqueItems": true,
        "default": [],
        "items": { "type": "string", "minLength": 1 },
        "description": "ctrl, alt, shift or win, held for the whole action and always released."
    })
}

fn pointer_properties() -> JsonObject {
    let mut properties = JsonObject::new();
    properties.insert(
        "observation_id".to_owned(),
        json!({
            "type": "string",
            "minLength": 1,
            "description": "From a screenshot. Use with image_x and image_y instead of x and y."
        }),
    );
    for axis in ["x", "y"] {
        properties.insert(
            format!("image_{axis}"),
            json!({ "type": "integer", "minimum": 0 }),
        );
    }
    properties.insert("display_id".to_owned(), display_id_property());
    for (axis, text) in [("x", "Physical-pixel X"), ("y", "Physical-pixel Y")] {
        properties.insert(
            axis.to_owned(),
            json!({ "type": "integer", "minimum": 0, "description": format!("{text} local to the display.") }),
        );
    }
    properties.insert("duration_ms".to_owned(), duration_property());
    add_observation(&mut properties);
    properties
}

/// Wraps tool-specific success schemas with the error envelope the server really returns, so a
/// client that validates `structuredContent` against `outputSchema` never rejects an error.
/// Shared sub-schemas move into `$defs`, so each is written once per tool.
fn output_schema(mut branches: Vec<Value>, mutating: bool) -> Arc<JsonObject> {
    branches.push(error_schema(mutating));
    let mut schema = json!({ "type": "object", "anyOf": branches });
    let text = schema.to_string();
    let mut needed: Vec<&'static str> = Vec::new();
    let mut pending: Vec<&'static str> = DEFS
        .iter()
        .copied()
        .filter(|name| text.contains(&format!("#/$defs/{name}\"")))
        .collect();
    while let Some(name) = pending.pop() {
        if needed.contains(&name) {
            continue;
        }
        needed.push(name);
        let body = definition(name).to_string();
        pending.extend(
            DEFS.iter()
                .copied()
                .filter(|other| body.contains(&format!("#/$defs/{other}\""))),
        );
    }
    if !needed.is_empty() {
        let defs: serde_json::Map<String, Value> = DEFS
            .iter()
            .copied()
            .filter(|name| needed.contains(name))
            .map(|name| (name.to_owned(), definition(name)))
            .collect();
        schema["$defs"] = Value::Object(defs);
    }
    strip_lower_bounds(&mut schema);
    strip_enum_types(&mut schema);
    match schema {
        Value::Object(schema) => Arc::new(schema),
        _ => Arc::default(),
    }
}

/// `{"type":"string","enum":[...]}` and `{"enum":[...]}` accept exactly the same values when every
/// member is a string, so the `type` only costs bytes in every `tools/list`. Input schemas keep
/// it: some clients rebuild their own tool declarations from `type`.
fn strip_enum_types(schema: &mut Value) {
    match schema {
        Value::Object(map) => {
            let all_strings = map
                .get("enum")
                .and_then(Value::as_array)
                .is_some_and(|members| members.iter().all(Value::is_string));
            if all_strings && map.get("type") == Some(&json!("string")) {
                map.remove("type");
            }
            map.values_mut().for_each(strip_enum_types);
        }
        Value::Array(items) => items.iter_mut().for_each(strip_enum_types),
        _ => {}
    }
}

/// Counters, sizes and times in results are produced by the server and never negative, so a
/// `minimum` on them only costs bytes in every `tools/list`. Input schemas keep their bounds.
fn strip_lower_bounds(schema: &mut Value) {
    match schema {
        Value::Object(map) => {
            map.remove("minimum");
            map.values_mut().for_each(strip_lower_bounds);
        }
        Value::Array(items) => items.iter_mut().for_each(strip_lower_bounds),
        _ => {}
    }
}

const DEFS: [&str; 7] = [
    "bounds",
    "display",
    "window",
    "screenshot",
    "window_screenshot",
    "session",
    "input",
];

fn def_ref(name: &str) -> Value {
    json!({ "$ref": format!("#/$defs/{name}") })
}

fn definition(name: &str) -> Value {
    match name {
        "bounds" => bounds_schema(),
        "display" => display_schema(),
        "window" => window_schema(),
        "screenshot" => screenshot_body(),
        "window_screenshot" => window_screenshot_body(),
        "session" => session_status_schema(),
        "input" => input_progress_schema(),
        other => unreachable!("unknown schema definition {other}"),
    }
}

/// The error envelope every tool can return. `details` is free-form and a refused input action
/// also carries its progress fields, so extra properties stay allowed.
fn error_schema(mutating: bool) -> Value {
    let mut properties = json!({
        "error": {
            "type": "object",
            "properties": {
                "code": { "type": "string" },
                "message": { "type": "string" },
                "retryable": { "type": "boolean" },
                "reason": { "type": "string" },
                "details": {}
            },
            "required": ["code", "message"]
        }
    });
    if mutating {
        // A refused or interrupted input action reports its progress next to the error. Input
        // that was sent while only the evidence failed has a warning instead of an error.
        properties["operation"] = json!({"type": "string"});
        properties["warning"] = json!({"type": "string"});
        properties["details"] = json!({"type": "object"});
        properties["status"] =
            json!({"enum": ["completed_unverified", "not_started", "partially_sent", "unknown"]});
        properties["input"] = def_ref("input");
        properties["observation_status"] = json!({"enum": ["failed", "not_attempted"]});
        properties["effect_verification"] = json!({"const": "unverified"});
        properties["retry_action"] = json!({"const": false});
        return json!({
            "type": "object",
            "properties": properties,
            "required": ["status", "input", "observation_status", "effect_verification", "retry_action"],
            "additionalProperties": false,
            "anyOf": [{"required": ["error"]}, {"required": ["operation", "warning"]}]
        });
    }
    json!({ "type": "object", "properties": properties, "required": ["error"], "additionalProperties": false })
}

fn input_progress_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "input_outcome": {"enum": ["not_started", "input_sent", "partially_sent", "unknown"]},
            "sent_events": {"type": "integer", "minimum": 0},
            "target_remained_foreground": { "type": ["boolean", "null"] },
            "activation_attempts": { "type": "integer", "minimum": 0 },
            "activation_elapsed_ms": { "type": "integer", "minimum": 0 },
            "cleanup": {"enum": ["not_needed", "succeeded", "unknown"]}
        },
        "required": ["input_outcome", "sent_events", "cleanup"],
        "additionalProperties": false
    })
}

fn action_output_schema(include_position: bool) -> Arc<JsonObject> {
    let mut success = json!({
        "type": "object",
        "properties": {
            "action": { "type": "object" },
            "observation": {
                "type": "object",
                "properties": {
                    "foreground_window": { "oneOf": [def_ref("window"), { "type": "null" }] },
                    "screenshot": { "oneOf": [def_ref("screenshot"), { "type": "null" }] }
                },
                "required": ["foreground_window", "screenshot"],
                "additionalProperties": false
            },
            "status": {"const": "completed"},
            "input": {"$ref": "#/$defs/input", "properties": {"input_outcome": {"const": "input_sent"}}},
            "observation_status": {"const": "succeeded"},
            "effect_verification": {"const": "unverified"},
            "retry_action": {"const": false}
        },
        "required": ["action", "observation", "status", "input", "observation_status", "effect_verification", "retry_action"],
        "additionalProperties": false
    });
    if include_position {
        success["properties"]["position"] = position_schema();
        success["required"]
            .as_array_mut()
            .expect("required is an array")
            .push(json!("position"));
    }
    output_schema(vec![success], true)
}

/// Image metadata shared by every screenshot-bearing result. `subject` adds the
/// fields that say what was captured, as `(name, schema, required)`.
fn image_metadata_schema(subject: Vec<(&str, Value, bool)>) -> Value {
    let mut properties = json!({
        "mime_type": { "const": "image/png" },
        "byte_length": { "type": "integer", "minimum": 1 },
        "capture_method": { "type": "string", "enum": ["windows_print_window", "windows_gdi"] },
        "image_width": { "type": "integer", "minimum": 1 },
        "image_height": { "type": "integer", "minimum": 1 },
        "downscale_factor": { "type": "integer", "minimum": 1 },
        "resize_method": { "type": "string", "enum": ["native", "area_average"] },
        "cursor_marker": { "type": "boolean" },
        "observation_id": { "type": "string" }
    });
    let mut required: Vec<&str> = vec![
        "mime_type",
        "byte_length",
        "capture_method",
        "image_width",
        "image_height",
        "downscale_factor",
        "resize_method",
        "cursor_marker",
    ];
    for (name, schema, is_required) in subject {
        properties[name] = schema;
        if is_required {
            required.push(name);
        }
    }
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

fn screenshot_body() -> Value {
    image_metadata_schema(vec![
        ("target_ref", json!({"type": ["string", "null"]}), false),
        ("display", def_ref("display"), true),
        ("source_bounds", def_ref("bounds"), true),
    ])
}

fn window_screenshot_body() -> Value {
    image_metadata_schema(vec![
        ("window", def_ref("window"), true),
        ("source_bounds", def_ref("bounds"), true),
    ])
}

fn screenshot_schema() -> Value {
    def_ref("screenshot")
}

fn window_screenshot_schema() -> Value {
    def_ref("window_screenshot")
}

/// Session snapshot from `SessionRuntime::status` or the busy fallback of `try_status`.
fn session_status_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "state": { "enum": ["dormant", "armed", "acting", "closing", "busy"] },
            "last_cleanup_reason": { "type": ["string", "null"] },
            "draining": { "type": "boolean" },
            "stop_state": { "type": "string" },
            "call_count": { "type": "integer", "minimum": 0 },
            "approved_target_ref": { "type": ["string", "null"] },
            "active_mutations": { "type": "integer", "minimum": 0 },
            "hold_ms": { "type": "integer", "minimum": 0 },
            "time_until_close_ms": { "type": ["integer", "null"], "minimum": 0 },
            "owns_arbitration": { "type": "boolean" },
            "restart_count": { "type": "integer", "minimum": 0 },
            "containment": { "type": "string" },
            "containment_reason": { "type": ["string", "null"] }
        },
        "required": ["state", "draining"],
        "additionalProperties": false
    })
}

fn session_change_output_schema(status: &str) -> Arc<JsonObject> {
    output_schema(
        vec![json!({
            "type": "object",
            "properties": {
                "status": { "const": status },
                "session": def_ref("session")
            },
            "required": ["status", "session"],
            "additionalProperties": false
        })],
        false,
    )
}

fn server_status_output_schema() -> Arc<JsonObject> {
    output_schema(
        vec![json!({
            "type": "object",
            "properties": {
                "status": { "enum": ["ready", "degraded"] },
                "safety_indicator": {
                    "type": "object",
                    "properties": {
                        "status": { "type": "string" },
                        "reason": { "type": ["string", "null"] }
                    },
                    "required": ["status", "reason"],
                    "additionalProperties": false
                },
                "session": { "oneOf": [def_ref("session"), { "type": "null" }] },
                "version": { "type": "string" },
                "instance_id": { "type": "string" },
                "process_id": { "type": "integer", "minimum": 0 },
                "server_elevated": { "type": "boolean" },
                "windows_integrity_level": { "enum": ["untrusted", "low", "medium", "medium_plus", "high", "system", "protected", "unknown"] },
                "elevated_operation_allowed": { "type": "boolean" },
                "uptime_ms": { "type": "integer", "minimum": 0 },
                "operations_started": { "type": "integer", "minimum": 0 },
                "operations_completed": { "type": "integer", "minimum": 0 },
                "operations_failed": { "type": "integer", "minimum": 0 },
                "recent_operations": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "operation_id": { "type": "integer", "minimum": 0 },
                            "tool": { "type": "string" },
                            "status": { "type": "string" },
                            "elapsed_ms": { "type": "integer", "minimum": 0 },
                            "gap_ms": { "type": ["integer", "null"], "minimum": 0 },
                            "stages_ms": { "type": "object", "additionalProperties": { "type": "number" } }
                        },
                        "required": ["operation_id", "tool", "status", "elapsed_ms", "gap_ms", "stages_ms"],
                        "additionalProperties": false
                    }
                },
                "backend": {
                    "type": "object",
                    "properties": {
                        "platform": { "type": "string" },
                        "display_server": { "type": "string" },
                        "backend_id": { "type": "string" }
                    },
                    "required": ["platform", "display_server", "backend_id"],
                    "additionalProperties": false
                },
                "stop_state": { "type": "string" },
                "stop_reason": { "const": "user_stop" }
            },
            "required": [
                "status", "safety_indicator", "session", "version", "instance_id", "process_id",
                "server_elevated", "windows_integrity_level", "elevated_operation_allowed", "uptime_ms",
                "operations_started", "operations_completed", "operations_failed", "recent_operations",
                "backend", "stop_state"
            ],
            "additionalProperties": false
        })],
        false,
    )
}

fn visual_change_output_schema() -> Arc<JsonObject> {
    output_schema(
        vec![json!({
            "type": "object",
            "properties": {
                "changed": { "type": "boolean" },
                "timed_out": { "type": "boolean" },
                "difference": { "type": "number", "minimum": 0 },
                "elapsed_ms": { "type": "integer", "minimum": 0 },
                "screenshot": screenshot_schema()
            },
            "required": ["changed", "timed_out", "difference", "elapsed_ms", "screenshot"],
            "additionalProperties": false
        })],
        false,
    )
}

fn text_box_schema(extra: Value, extra_required: &[&str]) -> Value {
    let mut properties = json!({
        "text": { "type": "string" },
        "x": { "type": "integer", "minimum": 0 },
        "y": { "type": "integer", "minimum": 0 },
        "width": { "type": "integer", "minimum": 0 },
        "height": { "type": "integer", "minimum": 0 }
    });
    if let (Some(base), Value::Object(extra)) = (properties.as_object_mut(), extra) {
        base.extend(extra);
    }
    let mut required = vec!["text", "x", "y", "width", "height"];
    required.extend_from_slice(extra_required);
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

fn read_text_output_schema() -> Arc<JsonObject> {
    let word = text_box_schema(json!({}), &[]);
    let line = text_box_schema(
        json!({"words": {"type": "array", "items": word}}),
        &["words"],
    );
    output_schema(
        vec![json!({
            "type": "object",
            "properties": {
                "target_ref": { "type": ["string", "null"] },
                "display": def_ref("display"),
                "source_bounds": def_ref("bounds"),
                "language": { "type": "string" },
                "text": { "type": "string" },
                "lines": { "type": "array", "items": line }
            },
            "required": ["display", "source_bounds", "language", "text", "lines"],
            "additionalProperties": false
        })],
        false,
    )
}

/// Fields shared by `find_text_on_screen` and `wait_for_text` discovery passes.
fn text_discovery_properties() -> Value {
    json!({
        "count": { "type": "integer", "minimum": 0 },
        "target_ref": { "type": ["string", "null"] },
        "query": { "type": "string" },
        "display": def_ref("display"),
        "source_bounds": def_ref("bounds"),
        "language": { "type": "string" },
        "matches": { "type": "array", "items": text_box_schema(json!({}), &[]) },
        "match_tier": { "enum": ["exact", "substring", "normalized", "joined", "ocr_confusion", null] },
        "candidate_count": { "type": "integer", "minimum": 0 },
        "candidates_truncated": { "type": "boolean" },
        "text_truncated": { "type": "boolean" }
    })
}

const TEXT_DISCOVERY_REQUIRED: [&str; 11] = [
    "count",
    "target_ref",
    "query",
    "display",
    "source_bounds",
    "language",
    "matches",
    "match_tier",
    "candidate_count",
    "candidates_truncated",
    "text_truncated",
];

fn find_text_output_schema() -> Arc<JsonObject> {
    let mut properties = text_discovery_properties();
    properties["recovery"] = json!({ "type": "string" });
    let mut required = TEXT_DISCOVERY_REQUIRED.to_vec();
    required.push("recovery");
    output_schema(
        vec![json!({
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false
        })],
        false,
    )
}

fn wait_for_text_output_schema() -> Arc<JsonObject> {
    let mut properties = text_discovery_properties();
    properties["condition"] = json!({ "enum": ["appears", "disappears"] });
    properties["satisfied"] = json!({ "type": "boolean" });
    properties["timed_out"] = json!({ "type": "boolean" });
    properties["elapsed_ms"] = json!({ "type": "integer", "minimum": 0 });
    properties["attempts"] = json!({ "type": "integer", "minimum": 0 });
    let mut required = TEXT_DISCOVERY_REQUIRED.to_vec();
    required.extend([
        "condition",
        "satisfied",
        "timed_out",
        "elapsed_ms",
        "attempts",
    ]);
    output_schema(
        vec![json!({
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false
        })],
        false,
    )
}

fn bounds_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "left": { "type": "integer" },
            "top": { "type": "integer" },
            "width": { "type": "integer", "minimum": 1 },
            "height": { "type": "integer", "minimum": 1 }
        },
        "required": ["left", "top", "width", "height"],
        "additionalProperties": false
    })
}

fn position_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "display_id": { "type": "string" },
            "local_x": { "type": "integer", "minimum": 0 },
            "local_y": { "type": "integer", "minimum": 0 },
            "virtual_x": { "type": "integer" },
            "virtual_y": { "type": "integer" }
        },
        "required": ["display_id", "local_x", "local_y", "virtual_x", "virtual_y"],
        "additionalProperties": false
    })
}

fn object_schema(properties: JsonObject, required: &[&str]) -> JsonObject {
    let mut schema = JsonObject::new();
    schema.insert("type".to_owned(), Value::String("object".to_owned()));
    schema.insert("properties".to_owned(), Value::Object(properties));
    schema.insert(
        "required".to_owned(),
        Value::Array(
            required
                .iter()
                .map(|name| Value::String((*name).to_owned()))
                .collect(),
        ),
    );
    schema.insert("additionalProperties".to_owned(), Value::Bool(false));
    schema
}

fn display_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "id": { "type": "string" },
            "name": { "type": "string" },
            "bounds": def_ref("bounds"),
            "is_primary": { "type": "boolean" }
        },
        "required": ["id", "name", "bounds", "is_primary"],
        "additionalProperties": false
    })
}

fn window_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "id": { "type": "string" },
            "title": { "type": "string" },
            "class_name": { "type": "string" },
            "process_id": { "type": "integer", "minimum": 0 },
            "bounds": def_ref("bounds"),
            "display_id": { "type": "string" },
            "is_foreground": { "type": "boolean" },
            "is_minimized": { "type": "boolean" },
            "owner": { "type": "string" }
        },
        "required": ["id", "title", "class_name", "process_id", "bounds", "display_id", "is_foreground", "is_minimized"],
        "additionalProperties": false
    })
}
