use super::{
    Arc, BEGIN_CONTROL_SESSION, CAPTURE_DISPLAY, CAPTURE_REGION, CAPTURE_VISUAL_BASELINE,
    CAPTURE_WINDOW, CLICK_MOUSE, CLICK_TEXT, DRAG_MOUSE, END_CONTROL_SESSION, FIND_TEXT_ON_SCREEN,
    FOCUS_WINDOW, GET_SERVER_STATUS, JsonObject, LIST_DISPLAYS, LIST_VIRTUAL_DESKTOPS,
    LIST_WINDOWS, MAX_CAPTURE_WIDTH, MAX_MOVE_DURATION_MS, MAX_SCROLL_DELTA, MAX_TEXT_UTF16_UNITS,
    MAX_WAIT_MS, MOVE_MOUSE, PRESS_KEYS, READ_TEXT_IN_REGION, RUN_ACTIONS, SCROLL_MOUSE,
    SWITCH_VIRTUAL_DESKTOP, TYPE_TEXT, Tool, ToolAnnotations, Value, WAIT_FOR_CHANGE_SINCE,
    WAIT_FOR_TEXT, WAIT_FOR_VISUAL_CHANGE, WAIT_FOR_WINDOW, json, requests::MAX_RUN_ACTION_STEPS,
};

/// Tool descriptions and schemas used by both `tools/list` and offline contract export.
pub fn tools() -> Vec<Tool> {
    let mut tools = vec![
        Tool::new(
            "stop_desktop_work",
            "Cancel active desktop work and close admission until the user restarts the server. Cleanup may still be draining. Cannot undo dispatched application operations.",
            object_schema(JsonObject::new(), &[]),
        )
        .with_raw_output_schema(object_output_schema(json!({
            "type": "object",
            "properties": {"status": {"enum": ["ready", "draining", "stopped", "cleanup_failed"]}},
            "required": ["status"],
            "additionalProperties": false
        })))
        .with_annotations(ToolAnnotations::default().read_only(false).destructive(false).idempotent(true).open_world(false)),
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
    ];
    for tool in &mut tools {
        if super::is_mutating_tool(&tool.name) && tool.name != FOCUS_WINDOW
            || tool.name == BEGIN_CONTROL_SESSION
        {
            let schema = Arc::make_mut(&mut tool.input_schema);
            schema.get_mut("properties").and_then(Value::as_object_mut).unwrap().insert(
                "target_ref".into(), json!({"type": "string", "minLength": 1,
                "description": "Server-issued target reference from a current window or screenshot observation. Must match the session's approved target. End the session before approving another target."}));
            schema
                .get_mut("required")
                .and_then(Value::as_array_mut)
                .unwrap()
                .push(json!("target_ref"));
        }
    }
    tools
}

fn begin_control_session_tool() -> Tool {
    let mut properties = JsonObject::new();
    properties.insert(
        "expected_seconds".to_owned(),
        json!({
            "type": "integer",
            "minimum": 1,
            "description": "Requested reservation time. The server clamps it to CONTROLFREAK_GLOW_MAX_HOLD_MS."
        }),
    );
    Tool::new(
        BEGIN_CONTROL_SESSION,
        "Reserve desktop control. Returns a retryable failure if another ControlFreak server owns it.",
        object_schema(properties, &[]),
    )
    .with_raw_output_schema(session_change_output_schema("armed"))
    .with_annotations(
        ToolAnnotations::default()
            .read_only(false)
            .destructive(false)
            .idempotent(true)
            .open_world(false),
    )
}

fn end_control_session_tool() -> Tool {
    Tool::new(
        END_CONTROL_SESSION,
        "Stop accepting new input actions and release desktop control after pending actions finish.",
        object_schema(JsonObject::new(), &[]),
    )
    .with_raw_output_schema(session_change_output_schema("closing"))
    .with_annotations(
        ToolAnnotations::default()
            .read_only(false)
            .destructive(false)
            .idempotent(true)
            .open_world(false),
    )
}

fn get_server_status_tool() -> Tool {
    Tool::new(
        GET_SERVER_STATUS,
        "Return server identity, version, uptime, privileges, operation diagnostics, stop_state and any user stop_reason. A user_stop requires the user's permission before restarting or resuming control.",
        object_schema(JsonObject::new(), &[]),
    )
    .with_raw_output_schema(server_status_output_schema())
    .with_annotations(
        ToolAnnotations::default()
            .read_only(true)
            .destructive(false)
            .idempotent(true)
            .open_world(false),
    )
}

fn list_displays_tool() -> Tool {
    Tool::new(
        LIST_DISPLAYS,
        "List active displays with IDs, primary status, and bounds in physical pixels.",
        object_schema(JsonObject::new(), &[]),
    )
    .with_raw_output_schema(Arc::new(rmcp::object!({
        "type": "object",
        "properties": {
            "count": { "type": "integer", "minimum": 0 },
            "displays": {
                "type": "array",
                "items": display_schema(),
            }
        },
        "required": ["count", "displays"],
        "additionalProperties": false
    })))
    .with_annotations(
        ToolAnnotations::default()
            .read_only(true)
            .destructive(false)
            .idempotent(true)
            .open_world(false),
    )
}

fn capture_display_tool() -> Tool {
    let mut properties = JsonObject::new();
    properties.insert(
        "display_id".to_owned(),
        json!({
            "type": "string",
            "minLength": 1,
            "description": "Display ID returned by list_displays."
        }),
    );
    add_capture_options(&mut properties);
    Tool::new(
        CAPTURE_DISPLAY,
        "Capture a display as PNG. Protected or HDR content may render incorrectly.",
        object_schema(properties, &["display_id"]),
    )
    .with_raw_output_schema(object_output_schema(screenshot_schema()))
    .with_annotations(
        ToolAnnotations::default()
            .read_only(true)
            .destructive(false)
            .idempotent(false)
            .open_world(false),
    )
}

fn capture_region_tool() -> Tool {
    let mut properties = region_properties();
    add_capture_options(&mut properties);
    Tool::new(
        CAPTURE_REGION,
        "Capture a rectangular display region as PNG, at native resolution unless max_width is supplied.",
        object_schema(
            properties,
            &["display_id", "x", "y", "width", "height"],
        ),
    )
    .with_raw_output_schema(object_output_schema(screenshot_schema()))
    .with_annotations(
        ToolAnnotations::default()
            .read_only(true)
            .destructive(false)
            .idempotent(false)
            .open_world(false),
    )
}

fn wait_for_visual_change_tool() -> Tool {
    let mut properties = region_properties();
    properties.insert(
        "timeout_ms".to_owned(),
        json!({
            "type": "integer", "minimum": 1, "maximum": MAX_WAIT_MS, "default": 5000,
            "description": "Maximum server-side wait. A timeout is returned as a successful result, not an error."
        }),
    );
    properties.insert(
        "stable_ms".to_owned(),
        json!({
            "type": "integer", "minimum": 0, "maximum": MAX_WAIT_MS, "default": 250,
            "description": "How long the changed region must remain visually stable before returning."
        }),
    );
    properties.insert(
        "difference_threshold".to_owned(),
        json!({
            "type": "number", "exclusiveMinimum": 0, "maximum": 1, "default": 0.01,
            "description": "Normalized mean RGB difference required to count as changed."
        }),
    );
    Tool::new(
        WAIT_FOR_VISUAL_CHANGE,
        "Wait for a region to change from its appearance at call time and settle. Return the final screenshot; timeout is a normal result.",
        object_schema(
            properties,
            &["display_id", "x", "y", "width", "height"],
        ),
    )
    .with_raw_output_schema(visual_change_output_schema())
    .with_annotations(
        ToolAnnotations::default()
            .read_only(true)
            .destructive(false)
            .idempotent(false)
            .open_world(false),
    )
}

fn capture_visual_baseline_tool() -> Tool {
    Tool::new(
        CAPTURE_VISUAL_BASELINE,
        "Save a region's visual baseline without returning an image. Capture before an action, then use wait_for_change_since afterward to wait for its effects.",
        object_schema(region_properties(), &["display_id", "x", "y", "width", "height"]),
    )
    .with_raw_output_schema(visual_baseline_output_schema())
    .with_annotations(
        ToolAnnotations::default()
            .read_only(true)
            .destructive(false)
            .idempotent(false)
            .open_world(false),
    )
}

fn wait_for_change_since_tool() -> Tool {
    let mut properties = wait_properties();
    properties.insert(
        "baseline_id".to_owned(),
        json!({
            "type": "string", "pattern": "^vb-[0-9a-f]{32}-[0-9a-f]{16}$",
            "description": "Unexpired baseline ID returned by capture_visual_baseline."
        }),
    );
    Tool::new(
        WAIT_FOR_CHANGE_SINCE,
        "Wait for a region to differ from its saved baseline and settle, then return a screenshot. Timeout is normal. Baselines expire after five minutes and can be reused while retained.",
        object_schema(properties, &["baseline_id"]),
    )
    .with_raw_output_schema(visual_change_output_schema())
    .with_annotations(
        ToolAnnotations::default()
            .read_only(true)
            .destructive(false)
            .idempotent(false)
            .open_world(false),
    )
}

fn read_text_in_region_tool() -> Tool {
    let mut properties = region_properties();
    add_ocr_language(&mut properties);
    Tool::new(
        READ_TEXT_IN_REGION,
        "Read text in a region using OCR. Return lines and words with bounds in display-local physical pixels. No image or confidence scores.",
        object_schema(properties, &["display_id", "x", "y", "width", "height"]),
    )
    .with_raw_output_schema(read_text_output_schema())
    .with_annotations(
        ToolAnnotations::default()
            .read_only(true)
            .destructive(false)
            .idempotent(false)
            .open_world(false),
    )
}

fn find_text_on_screen_tool() -> Tool {
    let mut properties = region_properties();
    add_ocr_language(&mut properties);
    properties.insert("query".to_owned(), json!({ "type": "string", "minLength": 1, "description": "Text substring to find within each OCR line." }));
    properties.insert(
        "case_sensitive".to_owned(),
        json!({ "type": "boolean", "default": false }),
    );
    properties.insert(
        "max_results".to_owned(),
        json!({ "type": "integer", "minimum": 1, "maximum": 100, "default": 20 }),
    );
    properties.insert("match_mode".into(), json!({"type": "string", "enum": ["exact", "substring", "tolerant"], "default": "substring", "description": "Discovery only. Tolerant tries trimmed exact, NFKC/whitespace/punctuation normalization, two adjacent lines, then optional OCR confusions. The first nonempty tier wins even if ambiguous. No confidence scores or automatic input."}));
    properties.insert("ocr_confusions".into(), json!({"type": "boolean", "default": false, "description": "Tolerant discovery only: allow one 0/O/o or 1/I/i/l/| substitution in a 4-64 character label."}));
    Tool::new(
        FIND_TEXT_ON_SCREEN,
        "Discover OCR text within a region without authorizing input. Returns display-local bounds, match_tier, full candidate_count, and truncation flags. At most max_results candidates and 256 Unicode characters per candidate; ambiguity is never hidden by the limit.",
        object_schema(properties, &["display_id", "x", "y", "width", "height", "query"]),
    )
    .with_raw_output_schema(find_text_output_schema())
    .with_annotations(
        ToolAnnotations::default()
            .read_only(true)
            .destructive(false)
            .idempotent(false)
            .open_world(false),
    )
}

fn wait_for_text_tool() -> Tool {
    let mut properties = region_properties();
    add_ocr_language(&mut properties);
    properties.insert("query".to_owned(), json!({ "type": "string", "minLength": 1, "description": "Text to wait for, matched as in find_text_on_screen." }));
    properties.insert("condition".to_owned(), json!({ "type": "string", "enum": ["appears", "disappears"], "default": "appears", "description": "appears: wait until at least one match is visible. disappears: wait until none is." }));
    properties.insert(
        "timeout_ms".to_owned(),
        json!({ "type": "integer", "minimum": 0, "maximum": MAX_WAIT_MS, "default": 5000 }),
    );
    properties.insert(
        "case_sensitive".to_owned(),
        json!({ "type": "boolean", "default": false }),
    );
    properties.insert(
        "max_results".to_owned(),
        json!({ "type": "integer", "minimum": 1, "maximum": 100, "default": 20 }),
    );
    properties.insert("match_mode".into(), json!({"type": "string", "enum": ["exact", "substring", "tolerant"], "default": "substring"}));
    properties.insert(
        "ocr_confusions".into(),
        json!({"type": "boolean", "default": false, "description": "Tolerant matching only."}),
    );
    Tool::new(
        WAIT_FOR_TEXT,
        "Repeat OCR discovery in a region until query appears or disappears, or timeout_ms elapses. Returns satisfied, timed_out, attempts, and the final pass's matches with display-local bounds. Each pass is a full recognition; prefer a small region. Discovery only: it never authorizes input.",
        object_schema(properties, &["display_id", "x", "y", "width", "height", "query"]),
    )
    .with_raw_output_schema(wait_for_text_output_schema())
    .with_annotations(
        ToolAnnotations::default()
            .read_only(true)
            .destructive(false)
            .idempotent(false)
            .open_world(false),
    )
}

fn click_text_tool() -> Tool {
    let mut properties = region_properties();
    add_ocr_language(&mut properties);
    properties.insert(
        "query".to_owned(),
        json!({
            "type": "string", "minLength": 1,
            "description": "OCR text to click. Surrounding whitespace is ignored. The action is rejected unless exactly one matching line exists in the entire region."
        }),
    );
    properties.insert(
        "case_sensitive".to_owned(),
        json!({ "type": "boolean", "default": false }),
    );
    properties.insert(
        "exact_match".to_owned(),
        json!({
            "type": "boolean", "default": true,
            "description": "Require the trimmed OCR line to equal query. Set false only when a unique substring is sufficient."
        }),
    );
    properties.insert(
        "button".to_owned(),
        json!({ "type": "string", "enum": ["left", "right", "middle"], "default": "left" }),
    );
    properties.insert(
        "click_count".to_owned(),
        json!({ "type": "integer", "minimum": 1, "maximum": 3, "default": 1 }),
    );
    properties.insert("modifiers".to_owned(), modifier_schema());
    properties.insert(
        "duration_ms".to_owned(),
        json!({ "type": "integer", "minimum": 0, "maximum": MAX_MOVE_DURATION_MS, "default": 0 }),
    );
    add_observation(&mut properties);
    Tool::new(
        CLICK_TEXT,
        "Click the center of a unique exact or substring OCR line in a region. No tolerant fallback. Zero/multiple matches return ocr_no_match/ocr_ambiguous_match with recovery guidance and at most 20 candidates of 256 Unicode characters, with truncation flags. No input occurs on either outcome. Success reports matched_text bounds and match_tier.",
        object_schema(
            properties,
            &["display_id", "x", "y", "width", "height", "query"],
        ),
    )
    .with_raw_output_schema(pointer_action_output_schema())
    .with_annotations(
        ToolAnnotations::default()
            .read_only(false)
            .destructive(true)
            .idempotent(false)
            .open_world(false),
    )
}

fn list_virtual_desktops_tool() -> Tool {
    Tool::new(
        LIST_VIRTUAL_DESKTOPS,
        "Group discoverable titled windows by desktop ID and identify the current group. Empty desktops, names, and desktop order are unavailable.",
        object_schema(JsonObject::new(), &[]),
    )
    .with_raw_output_schema(Arc::new(rmcp::object!({
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
                        "windows": { "type": "array", "items": window_schema() }
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
    })))
    .with_annotations(
        ToolAnnotations::default()
            .read_only(true)
            .destructive(false)
            .idempotent(true)
            .open_world(false),
    )
}

fn switch_virtual_desktop_tool() -> Tool {
    let mut properties = JsonObject::new();
    properties.insert(
        "direction".to_owned(),
        json!({
            "type": "string", "enum": ["left", "right"],
            "description": "Relative direction in the Windows virtual-desktop strip."
        }),
    );
    add_observation(&mut properties);
    properties.insert(
        "steps".to_owned(),
        json!({
            "type": "integer", "minimum": 1, "maximum": 10, "default": 1,
            "description": "Number of relative desktop switches to request. Windows stops at an edge."
        }),
    );
    Tool::new(
        SWITCH_VIRTUAL_DESKTOP,
        "Switch left or right across existing virtual desktops. changed=false means no switch was detected.",
        object_schema(properties, &["direction"]),
    )
    .with_raw_output_schema(action_output_schema(false))
    .with_annotations(
        ToolAnnotations::default()
            .read_only(false)
            .destructive(false)
            .idempotent(false)
            .open_world(false),
    )
}

fn list_windows_tool() -> Tool {
    Tool::new(
        LIST_WINDOWS,
        "List visible titled top-level windows, foreground first, then by title. Returns at most 200; truncated=true means more exist, so use wait_for_window with title_contains, class_name, or process_id to reach them. Refresh IDs after windows open, close, or change ownership.",
        object_schema(JsonObject::new(), &[]),
    )
    .with_raw_output_schema(Arc::new(rmcp::object!({
        "type": "object",
        "properties": {
            "count": { "type": "integer", "minimum": 0 },
            "truncated": { "type": "boolean", "description": "More eligible windows existed than were returned." },
            "windows": { "type": "array", "items": window_schema() }
        },
        "required": ["count", "truncated", "windows"],
        "additionalProperties": false
    })))
    .with_annotations(
        ToolAnnotations::default()
            .read_only(true)
            .destructive(false)
            .idempotent(true)
            .open_world(false),
    )
}

fn focus_window_tool() -> Tool {
    let mut properties = JsonObject::new();
    properties.insert("timeout_ms".into(), json!({ "type": "integer", "minimum": 100, "maximum": 5000, "default": 1000, "description": "Total activation retry and settle budget in milliseconds." }));
    add_window_id(&mut properties);
    add_observation(&mut properties);
    Tool::new(
        FOCUS_WINDOW,
        "Restore and focus an existing window, then verify it became foreground.",
        object_schema(properties, &["window_id"]),
    )
    .with_raw_output_schema(action_output_schema(false))
    .with_annotations(
        ToolAnnotations::default()
            .read_only(false)
            .destructive(false)
            .idempotent(true)
            .open_world(false),
    )
}

fn capture_window_tool() -> Tool {
    let mut properties = JsonObject::new();
    add_window_id(&mut properties);
    add_capture_options(&mut properties);
    Tool::new(
        CAPTURE_WINDOW,
        "Capture a window's visible frame without focusing it. Use a fresh window ID and restore minimized windows first.",
        object_schema(properties, &["window_id"]),
    )
    .with_raw_output_schema(object_output_schema(window_screenshot_schema()))
    .with_annotations(
        ToolAnnotations::default()
            .read_only(true)
            .destructive(false)
            .idempotent(false)
            .open_world(false),
    )
}

fn wait_for_window_tool() -> Tool {
    let mut properties = JsonObject::new();
    add_window_id(&mut properties);
    properties.insert("title_contains".to_owned(), json!({ "type": "string", "minLength": 1, "description": "Case-insensitive title substring." }));
    properties.insert("class_name".to_owned(), json!({ "type": "string", "minLength": 1, "description": "Case-insensitive exact Win32 class name." }));
    properties.insert(
        "process_id".to_owned(),
        json!({ "type": "integer", "minimum": 1 }),
    );
    properties.insert("is_foreground".to_owned(), json!({ "type": "boolean" }));
    properties.insert(
        "timeout_ms".to_owned(),
        json!({ "type": "integer", "minimum": 0, "maximum": MAX_WAIT_MS, "default": 5000 }),
    );
    Tool::new(
        WAIT_FOR_WINDOW,
        "Wait for a visible top-level window matching all supplied criteria. Requires at least one criterion. Timeout returns matched=false and timed_out=true.",
        object_schema(properties, &[]),
    )
    .with_raw_output_schema(wait_for_window_output_schema())
    .with_annotations(
        ToolAnnotations::default()
            .read_only(true)
            .destructive(false)
            .idempotent(false)
            .open_world(false),
    )
}

fn move_mouse_tool() -> Tool {
    Tool::new(
        MOVE_MOUSE,
        "Move the pointer within or between displays. Give display_id with display-local physical x/y, or observation_id with image_x/image_y from a returned screenshot.",
        object_schema(pointer_properties(), &[]),
    )
    .with_raw_output_schema(pointer_action_output_schema())
    .with_annotations(
        ToolAnnotations::default()
            .read_only(false)
            .destructive(false)
            .idempotent(true)
            .open_world(false),
    )
}

fn click_mouse_tool() -> Tool {
    let mut properties = pointer_properties();
    properties.insert(
        "button".to_owned(),
        json!({
            "type": "string",
            "enum": ["left", "right", "middle"],
            "description": "Mouse button to click. Middle-click commonly opens links in a background tab."
        }),
    );
    properties.insert("click_count".to_owned(), json!({ "type": "integer", "minimum": 1, "maximum": 3, "default": 1, "description": "Number of clicks performed as one atomic input sequence." }));
    properties.insert("modifiers".to_owned(), modifier_schema());
    Tool::new(
        CLICK_MOUSE,
        "Click at display_id with display-local physical x/y, or at observation_id with image_x/image_y from a returned screenshot. Clicks and modifiers execute as one input sequence.",
        object_schema(properties, &["button"]),
    )
    .with_raw_output_schema(pointer_action_output_schema())
    .with_annotations(
        ToolAnnotations::default()
            .read_only(false)
            .destructive(true)
            .idempotent(false)
            .open_world(false),
    )
}

fn drag_mouse_tool() -> Tool {
    let mut properties = JsonObject::new();
    for prefix in ["start", "end"] {
        properties.insert(format!("{prefix}_display_id"), json!({ "type": "string", "minLength": 1, "description": format!("{prefix} display ID returned by list_displays.") }));
        properties.insert(format!("{prefix}_x"), json!({ "type": "integer", "minimum": 0, "description": format!("{prefix} physical-pixel X coordinate local to its display.") }));
        properties.insert(format!("{prefix}_y"), json!({ "type": "integer", "minimum": 0, "description": format!("{prefix} physical-pixel Y coordinate local to its display.") }));
    }
    properties.insert("observation_id".to_owned(), json!({ "type": "string", "minLength": 1, "description": "Screenshot observation_id for start/end image points." }));
    for prefix in ["start", "end"] {
        for axis in ["x", "y"] {
            properties.insert(format!("{prefix}_image_{axis}"), json!({ "type": "integer", "minimum": 0, "description": format!("{prefix} {axis} in the observation_id image's own pixels; replaces {prefix}_display_id/{prefix}_x/{prefix}_y.") }));
        }
    }
    properties.insert(
        "button".to_owned(),
        json!({ "type": "string", "enum": ["left", "right", "middle"] }),
    );
    properties.insert("modifiers".to_owned(), modifier_schema());
    properties.insert("duration_ms".to_owned(), json!({ "type": "integer", "minimum": 0, "maximum": MAX_MOVE_DURATION_MS, "default": 0, "description": "Smooth drag duration from start to end." }));
    add_observation(&mut properties);
    Tool::new(
        DRAG_MOUSE,
        "Drag within or between displays. Give each end as display_id with display-local x/y, or as image points in one observation_id. Hold the button and modifiers for the drag, then release all held inputs.",
        object_schema(properties, &["button"]),
    )
    .with_raw_output_schema(pointer_action_output_schema())
    .with_annotations(
        ToolAnnotations::default()
            .read_only(false)
            .destructive(true)
            .idempotent(false)
            .open_world(false),
    )
}

fn scroll_mouse_tool() -> Tool {
    let mut properties = pointer_properties();
    properties.insert(
        "delta_x".to_owned(),
        json!({
            "type": "integer",
            "minimum": -MAX_SCROLL_DELTA,
            "maximum": MAX_SCROLL_DELTA,
            "default": 0,
            "description": "Horizontal wheel delta: positive scrolls right and negative scrolls left. 120 is one wheel detent."
        }),
    );
    properties.insert(
        "delta_y".to_owned(),
        json!({
            "type": "integer",
            "minimum": -MAX_SCROLL_DELTA,
            "maximum": MAX_SCROLL_DELTA,
            "description": "Vertical wheel delta: positive scrolls up and negative scrolls down. 120 is one wheel detent."
        }),
    );
    Tool::new(
        SCROLL_MOUSE,
        "Move to display_id with display-local physical x/y, or observation_id with image_x/image_y, then scroll vertically or horizontally.",
        object_schema(properties, &["delta_y"]),
    )
    .with_raw_output_schema(pointer_action_output_schema())
    .with_annotations(
        ToolAnnotations::default()
            .read_only(false)
            .destructive(false)
            .idempotent(false)
            .open_world(false),
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
            "description": format!("Keys pressed together as one chord, then released in reverse order. Names are case-insensitive. Canonical names: {}. Use type_text for text.", key_names().join(", ")),
            "items": { "type": "string", "minLength": 1 }
        }),
    );
    add_observation(&mut properties);
    Tool::new(
        PRESS_KEYS,
        "Send a key or chord only while the approved target remains foreground. Observe and explicitly approve a target before input.",
        object_schema(properties, &["keys"]),
    )
    .with_raw_output_schema(action_output_schema(false))
    .with_annotations(
        ToolAnnotations::default()
            .read_only(false)
            .destructive(true)
            .idempotent(false)
            .open_world(false),
    )
}

fn run_actions_tool() -> Tool {
    let mut step = pointer_properties();
    step.remove("duration_ms");
    step.remove("observation");
    step.insert(
        "action".to_owned(),
        json!({ "type": "string", "enum": ["click", "scroll", "type_text", "press_keys"] }),
    );
    step.insert("button".to_owned(), json!({ "type": "string", "enum": ["left", "right", "middle"], "default": "left", "description": "click only." }));
    step.insert("click_count".to_owned(), json!({ "type": "integer", "minimum": 1, "maximum": 3, "default": 1, "description": "click only." }));
    step.insert("modifiers".to_owned(), modifier_schema());
    step.insert("delta_x".to_owned(), json!({ "type": "integer", "minimum": -MAX_SCROLL_DELTA, "maximum": MAX_SCROLL_DELTA, "default": 0, "description": "scroll only." }));
    step.insert("delta_y".to_owned(), json!({ "type": "integer", "minimum": -MAX_SCROLL_DELTA, "maximum": MAX_SCROLL_DELTA, "default": 0, "description": "scroll only. Positive scrolls up." }));
    step.insert("text".to_owned(), json!({ "type": "string", "minLength": 1, "maxLength": MAX_TEXT_UTF16_UNITS, "description": "type_text only." }));
    step.insert("keys".to_owned(), json!({ "type": "array", "minItems": 1, "maxItems": 8, "uniqueItems": true, "items": { "type": "string", "minLength": 1 }, "description": "press_keys only: one chord, as in press_keys." }));
    let mut properties = JsonObject::new();
    properties.insert(
        "steps".to_owned(),
        json!({
            "type": "array",
            "minItems": 1,
            "maxItems": MAX_RUN_ACTION_STEPS,
            "description": "Ordered steps. click and scroll take display_id/x/y or observation_id/image_x/image_y.",
            "items": {
                "type": "object",
                "properties": step,
                "required": ["action"],
                "additionalProperties": false
            }
        }),
    );
    add_observation(&mut properties);
    Tool::new(
        RUN_ACTIONS,
        "Run up to 16 click, scroll, type_text, and press_keys steps in order on the approved target, with one final observation. All step arguments are checked before the first step runs. Each step keeps the single-action target checks and waits for the window to settle; the batch stops at the first failure and reports each step as completed, failed, or skipped. Use it for a known sequence, such as filling a form; observe between steps when the next step depends on the result.",
        object_schema(properties, &["steps"]),
    )
    .with_raw_output_schema(action_output_schema(false))
    .with_annotations(
        ToolAnnotations::default()
            .read_only(false)
            .destructive(true)
            .idempotent(false)
            .open_world(false),
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
            "description": "Unicode text to type into the current foreground window. This does not use the clipboard."
        }),
    );
    add_observation(&mut properties);
    Tool::new(
        TYPE_TEXT,
        "Type Unicode text into the approved foreground target without using the clipboard. Each text batch revalidates target ownership.",
        object_schema(properties, &["text"]),
    )
    .with_raw_output_schema(action_output_schema(false))
    .with_annotations(
        ToolAnnotations::default()
            .read_only(false)
            .destructive(true)
            .idempotent(false)
            .open_world(false),
    )
}

fn add_window_id(properties: &mut JsonObject) {
    properties.insert(
        "window_id".to_owned(),
        json!({
            "type": "string",
            "minLength": 1,
            "description": "Opaque server-issued target reference returned by list_windows or an observation."
        }),
    );
}

fn add_capture_options(properties: &mut JsonObject) {
    properties.insert(
        "max_width".to_owned(),
        json!({
            "type": "integer", "minimum": 1, "maximum": MAX_CAPTURE_WIDTH,
            "description": "Optional exact maximum output width. Resizing preserves aspect ratio; use source_bounds and output dimensions for exact coordinate mapping."
        }),
    );
    properties.insert(
        "include_cursor".to_owned(),
        json!({
            "type": "boolean", "default": true,
            "description": "Draw the magenta ControlFreak cursor marker when the pointer lies inside the capture bounds."
        }),
    );
}

fn add_ocr_language(properties: &mut JsonObject) {
    properties.insert(
        "language".to_owned(),
        json!({
            "type": "string", "minLength": 2,
            "description": "Optional installed BCP-47 OCR language such as en-US. Omit to use Windows profile languages."
        }),
    );
}

fn add_observation(properties: &mut JsonObject) {
    properties.insert(
        "observation".to_owned(),
        json!({
            "type": "object",
            "description": "Post-action evidence. Omit for the compatible bounded screenshot default; metadata and none avoid returning image data.",
            "properties": {
                "mode": { "type": "string", "enum": ["none", "metadata", "screenshot"], "default": "screenshot" },
                "max_width": { "type": "integer", "minimum": 1, "maximum": MAX_CAPTURE_WIDTH },
                "include_cursor": { "type": "boolean", "default": true },
                "region": {
                    "type": "object",
                    "description": "Optional display-local crop for screenshot mode. Use this to return only the UI area affected by the action.",
                    "properties": {
                        "display_id": { "type": "string", "minLength": 1 },
                        "x": { "type": "integer", "minimum": 0 },
                        "y": { "type": "integer", "minimum": 0 },
                        "width": { "type": "integer", "minimum": 1 },
                        "height": { "type": "integer", "minimum": 1 }
                    },
                    "required": ["display_id", "x", "y", "width", "height"],
                    "additionalProperties": false
                }
            },
            "additionalProperties": false
        }),
    );
}

fn wait_properties() -> JsonObject {
    let mut properties = JsonObject::new();
    properties.insert(
        "timeout_ms".to_owned(),
        json!({ "type": "integer", "minimum": 1, "maximum": MAX_WAIT_MS, "default": 5000 }),
    );
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

fn region_properties() -> JsonObject {
    let mut properties = JsonObject::new();
    properties.insert("display_id".to_owned(), json!({ "type": "string", "minLength": 1, "description": "Display ID returned by list_displays." }));
    properties.insert("x".to_owned(), json!({ "type": "integer", "minimum": 0, "description": "Region left edge in display-local physical pixels." }));
    properties.insert("y".to_owned(), json!({ "type": "integer", "minimum": 0, "description": "Region top edge in display-local physical pixels." }));
    properties.insert("width".to_owned(), json!({ "type": "integer", "minimum": 1, "description": "Region width in physical pixels." }));
    properties.insert("height".to_owned(), json!({ "type": "integer", "minimum": 1, "description": "Region height in physical pixels." }));
    properties
}

fn modifier_schema() -> Value {
    json!({
        "type": "array",
        "maxItems": 4,
        "uniqueItems": true,
        "default": [],
        "items": { "type": "string", "minLength": 1 },
        "description": "Case-insensitive modifier names ctrl, alt, shift, or win, held for the complete atomic mouse action and always released afterward."
    })
}

fn pointer_properties() -> JsonObject {
    let mut properties = JsonObject::new();
    properties.insert(
        "observation_id".to_owned(),
        json!({
            "type": "string",
            "minLength": 1,
            "description": "observation_id of a returned screenshot. Use with image_x/image_y instead of display_id/x/y."
        }),
    );
    properties.insert(
        "image_x".to_owned(),
        json!({
            "type": "integer",
            "minimum": 0,
            "description": "X in that screenshot's own pixels; the server maps it through the crop and scale."
        }),
    );
    properties.insert(
        "image_y".to_owned(),
        json!({
            "type": "integer",
            "minimum": 0,
            "description": "Y in that screenshot's own pixels."
        }),
    );
    properties.insert(
        "display_id".to_owned(),
        json!({
            "type": "string",
            "minLength": 1,
            "description": "Target display ID returned by list_displays."
        }),
    );
    properties.insert(
        "x".to_owned(),
        json!({
            "type": "integer",
            "minimum": 0,
            "description": "Physical-pixel X coordinate local to the target display."
        }),
    );
    properties.insert(
        "y".to_owned(),
        json!({
            "type": "integer",
            "minimum": 0,
            "description": "Physical-pixel Y coordinate local to the target display."
        }),
    );
    properties.insert(
        "duration_ms".to_owned(),
        json!({
            "type": "integer",
            "minimum": 0,
            "maximum": MAX_MOVE_DURATION_MS,
            "default": 0,
            "description": "Optional movement duration before the action. A nonzero value moves smoothly between displays."
        }),
    );
    add_observation(&mut properties);
    properties
}

fn pointer_action_output_schema() -> Arc<JsonObject> {
    action_output_schema(true)
}

fn action_output_schema(include_position: bool) -> Arc<JsonObject> {
    let mut schema = json!({
        "type": "object",
        "properties": {
            "action": { "type": "object" },
            "observation": {
                "type": "object",
                "properties": {
                    "foreground_window": {
                        "oneOf": [window_schema(), { "type": "null" }]
                    },
                    "screenshot": {
                        "oneOf": [screenshot_schema(), { "type": "null" }]
                    }
                },
                "required": ["foreground_window", "screenshot"],
                "additionalProperties": false
            }
        },
        "required": ["action", "observation"],
        "additionalProperties": false
    });
    if include_position {
        schema["properties"]["position"] = position_schema();
        schema["required"]
            .as_array_mut()
            .expect("required is an array")
            .push(Value::String("position".to_owned()));
    }
    let progress_properties = mutation_progress_properties();
    let mut incomplete = json!({
        "type": "object",
        "properties": {
            "operation": {"type": "string"},
            "warning": {"type": "string"},
            "details": {"type": "object", "description": "run_actions: per-step reports when the final observation failed."},
            "error": {
                "type": "object",
                "properties": {"code": {"type": "string"}, "message": {"type": "string"}, "details": {}},
                "required": ["code", "message"],
                "additionalProperties": false
            }
        },
        "required": [],
        "additionalProperties": false,
        "anyOf": [{"required": ["error"]}, {"required": ["operation", "warning"]}]
    });
    for variant in [&mut schema, &mut incomplete] {
        for (name, property) in progress_properties.as_object().expect("properties object") {
            variant["properties"][name] = property.clone();
            variant["required"]
                .as_array_mut()
                .expect("required array")
                .push(json!(name));
        }
    }
    schema["properties"]["status"] = json!({"const": "completed"});
    schema["properties"]["observation_status"] = json!({"const": "succeeded"});
    schema["properties"]["input"]["properties"]["input_outcome"] = json!({"const": "input_sent"});
    incomplete["properties"]["status"] =
        json!({"enum": ["completed_unverified", "not_started", "partially_sent", "unknown"]});
    incomplete["properties"]["observation_status"] = json!({"enum": ["failed", "not_attempted"]});
    incomplete["oneOf"] = json!(
        [
            ("completed_unverified", "input_sent", "failed"),
            ("not_started", "not_started", "not_attempted"),
            ("partially_sent", "partially_sent", "not_attempted"),
            ("unknown", "unknown", "not_attempted"),
        ]
        .map(|(status, input, observation)| json!({"properties": {
            "status": {"const": status},
            "input": {"properties": {"input_outcome": {"const": input}}},
            "observation_status": {"const": observation}
        }}))
    );
    Arc::new(rmcp::object!({"type": "object", "oneOf": [schema, incomplete]}))
}

fn mutation_progress_properties() -> Value {
    json!({
        "status": {"enum": ["completed", "completed_unverified", "not_started", "partially_sent", "unknown"]},
        "input": {
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
        },
        "observation_status": {"enum": ["succeeded", "failed", "not_attempted"]},
        "effect_verification": {"const": "unverified"},
        "retry_action": {"const": false}
    })
}

/// Image metadata shared by every screenshot-bearing result. `subject` adds the
/// fields that say what was captured, as `(name, schema, required)`.
fn image_metadata_schema(subject: Vec<(&str, Value, bool)>) -> Value {
    let mut properties = json!({
        "mime_type": { "const": "image/png" },
        "byte_length": { "type": "integer", "minimum": 1 },
        "capture_method": { "type": "string" },
        "image_width": { "type": "integer", "minimum": 1 },
        "image_height": { "type": "integer", "minimum": 1 },
        "downscale_factor": { "type": "integer", "minimum": 1, "description": "Backward-compatible ceiling ratio. For exact mapping use source_bounds and image dimensions." },
        "resize_method": { "type": "string", "enum": ["native", "area_average"] },
        "cursor_marker": { "type": "boolean" },
        "observation_id": { "type": "string", "description": "Pass with image_x/image_y to pointer tools to act on a point in this image." }
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

fn screenshot_schema() -> Value {
    image_metadata_schema(vec![
        ("target_ref", json!({"type": ["string", "null"]}), false),
        ("display", display_schema(), true),
        ("source_bounds", bounds_schema(), true),
    ])
}

fn window_screenshot_schema() -> Value {
    image_metadata_schema(vec![
        ("window", window_schema(), true),
        ("source_bounds", bounds_schema(), true),
    ])
}

fn object_output_schema(schema: Value) -> Arc<JsonObject> {
    // Every caller passes an object literal; a test requires each tool to publish one.
    Arc::new(schema.as_object().cloned().unwrap_or_default())
}

/// Session snapshot from `IndicatorRuntime::status` or the busy fallback of `try_status`.
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
    object_output_schema(json!({
        "type": "object",
        "properties": {
            "status": { "const": status },
            "session": session_status_schema()
        },
        "required": ["status", "session"],
        "additionalProperties": false
    }))
}

fn server_status_output_schema() -> Arc<JsonObject> {
    object_output_schema(json!({
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
            "session": { "oneOf": [session_status_schema(), { "type": "null" }] },
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
    }))
}

fn visual_change_output_schema() -> Arc<JsonObject> {
    let mut schema = json!({
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
    });
    schema["properties"]["screenshot"]["description"] =
        json!("The final image; the PNG itself is the image content block.");
    object_output_schema(schema)
}

fn visual_baseline_output_schema() -> Arc<JsonObject> {
    object_output_schema(json!({
        "type": "object",
        "properties": {
            "baseline_id": { "type": "string", "pattern": "^vb-[0-9a-f]{32}-[0-9a-f]{16}$" },
            "display": display_schema(),
            "source_bounds": bounds_schema(),
            "fingerprint": { "type": "string" },
            "expires_in_ms": { "type": "integer", "minimum": 0 }
        },
        "required": ["baseline_id", "display", "source_bounds", "fingerprint", "expires_in_ms"],
        "additionalProperties": false
    }))
}

fn wait_for_window_output_schema() -> Arc<JsonObject> {
    object_output_schema(json!({
        "type": "object",
        "properties": {
            "matched": { "type": "boolean" },
            "timed_out": { "type": "boolean" },
            "elapsed_ms": { "type": "integer", "minimum": 0 },
            "window": { "oneOf": [window_schema(), { "type": "null" }] }
        },
        "required": ["matched", "timed_out", "elapsed_ms", "window"],
        "additionalProperties": false
    }))
}

fn text_box_schema(extra: Value, extra_required: &[&str]) -> Value {
    let mut properties = json!({
        "text": { "type": "string" },
        "x": { "type": "integer", "minimum": 0 },
        "y": { "type": "integer", "minimum": 0 },
        "width": { "type": "integer", "minimum": 0 },
        "height": { "type": "integer", "minimum": 0 }
    });
    if let (Some(base), Some(extra)) = (properties.as_object_mut(), extra.as_object()) {
        base.extend(extra.clone());
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
    object_output_schema(json!({
        "type": "object",
        "properties": {
            "target_ref": { "type": ["string", "null"] },
            "display": display_schema(),
            "source_bounds": bounds_schema(),
            "language": { "type": "string" },
            "text": { "type": "string" },
            "lines": { "type": "array", "items": line }
        },
        "required": ["display", "source_bounds", "language", "text", "lines"],
        "additionalProperties": false
    }))
}

/// Fields shared by `find_text_on_screen` and `wait_for_text` discovery passes.
fn text_discovery_properties() -> Value {
    json!({
        "count": { "type": "integer", "minimum": 0 },
        "target_ref": { "type": ["string", "null"] },
        "query": { "type": "string" },
        "display": display_schema(),
        "source_bounds": bounds_schema(),
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
    object_output_schema(json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    }))
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
    object_output_schema(json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    }))
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
            "bounds": {
                "type": "object",
                "properties": {
                    "left": { "type": "integer" },
                    "top": { "type": "integer" },
                    "width": { "type": "integer", "minimum": 1 },
                    "height": { "type": "integer", "minimum": 1 }
                },
                "required": ["left", "top", "width", "height"],
                "additionalProperties": false
            },
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
            "bounds": {
                "type": "object",
                "properties": {
                    "left": { "type": "integer" },
                    "top": { "type": "integer" },
                    "width": { "type": "integer", "minimum": 1 },
                    "height": { "type": "integer", "minimum": 1 }
                },
                "required": ["left", "top", "width", "height"],
                "additionalProperties": false
            },
            "display_id": { "type": "string" },
            "is_foreground": { "type": "boolean" },
            "is_minimized": { "type": "boolean" }
        },
        "required": ["id", "title", "class_name", "process_id", "bounds", "display_id", "is_foreground", "is_minimized"],
        "additionalProperties": false
    })
}

fn key_names() -> Vec<&'static str> {
    vec![
        "ctrl",
        "alt",
        "shift",
        "win",
        "enter",
        "tab",
        "escape",
        "space",
        "backspace",
        "delete",
        "insert",
        "home",
        "end",
        "page_up",
        "page_down",
        "arrow_up",
        "arrow_down",
        "arrow_left",
        "arrow_right",
        "a",
        "b",
        "c",
        "d",
        "e",
        "f",
        "g",
        "h",
        "i",
        "j",
        "k",
        "l",
        "m",
        "n",
        "o",
        "p",
        "q",
        "r",
        "s",
        "t",
        "u",
        "v",
        "w",
        "x",
        "y",
        "z",
        "0",
        "1",
        "2",
        "3",
        "4",
        "5",
        "6",
        "7",
        "8",
        "9",
        "f1",
        "f2",
        "f3",
        "f4",
        "f5",
        "f6",
        "f7",
        "f8",
        "f9",
        "f10",
        "f11",
        "f12",
    ]
}
