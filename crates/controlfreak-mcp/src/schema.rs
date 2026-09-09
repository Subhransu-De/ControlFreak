use super::{
    Arc, BEGIN_CONTROL_SESSION, CAPTURE_DISPLAY, CAPTURE_REGION, CAPTURE_VISUAL_BASELINE,
    CAPTURE_WINDOW, CLICK_MOUSE, CLICK_TEXT, DRAG_MOUSE, END_CONTROL_SESSION, FIND_TEXT_ON_SCREEN,
    FOCUS_WINDOW, GET_SERVER_STATUS, JsonObject, LIST_DISPLAYS, LIST_VIRTUAL_DESKTOPS,
    LIST_WINDOWS, MAX_CAPTURE_WIDTH, MAX_MOVE_DURATION_MS, MAX_SCROLL_DELTA, MAX_TEXT_UTF16_UNITS,
    MAX_WAIT_MS, MOVE_MOUSE, PRESS_KEYS, READ_TEXT_IN_REGION, SCROLL_MOUSE, SWITCH_VIRTUAL_DESKTOP,
    TYPE_TEXT, Tool, ToolAnnotations, Value, WAIT_FOR_CHANGE_SINCE, WAIT_FOR_VISUAL_CHANGE,
    WAIT_FOR_WINDOW, json,
};

pub(super) fn tools() -> Vec<Tool> {
    vec![
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
    ]
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
        "Reserve desktop control before a multi-step task and show the ARMED safety glow. The call fails retryably when another ControlFreak server owns the desktop.",
        object_schema(properties, &[]),
    )
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
        "Close mutation admission after the last desktop step. The glow fades and arbitration is released after any in-flight mutation finishes.",
        object_schema(JsonObject::new(), &[]),
    )
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
        "Return this MCP process instance ID, version, uptime, backend identity, Windows integrity level, elevation/opt-in state, and bounded operation counters. Use it to confirm that the same server instance and privilege context are still active after a suspected transport or tool failure.",
        object_schema(JsonObject::new(), &[]),
    )
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
        "List active displays and return their count, stable Windows device IDs, primary flag, and physical-pixel virtual-desktop bounds.",
        object_schema(JsonObject::new(), &[]),
    )
    .with_raw_output_schema(Arc::new(json_object(json!({
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
    }))))
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
        "Capture one active Windows display and return PNG image content. Use max_width to avoid unnecessary full-resolution images; input tools already return post-action evidence. Protected or HDR content may not reproduce exactly with GDI.",
        object_schema(properties, &["display_id"]),
    )
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
        "Capture a display-local rectangular region. Returns native resolution by default, explicit virtual-desktop source bounds, and an optional cursor marker; use this to inspect small controls or text without recapturing an entire display.",
        object_schema(
            properties,
            &["display_id", "x", "y", "width", "height"],
        ),
    )
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
        "Wait for a display region to change and settle, then return the final screenshot. This replaces guessed sleeps and repeated capture polling; timed_out indicates no settled change before the deadline.",
        object_schema(
            properties,
            &["display_id", "x", "y", "width", "height"],
        ),
    )
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
        "Capture and retain a lightweight visual fingerprint for a display region without returning an image. Use its baseline_id before an action, then call wait_for_change_since so fast changes that occur during the action are not missed.",
        object_schema(region_properties(), &["display_id", "x", "y", "width", "height"]),
    )
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
        "Compare an earlier retained baseline with its region until it has changed and settled, then return one final screenshot. Baselines expire after five minutes and can be reused while valid.",
        object_schema(properties, &["baseline_id"]),
    )
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
        "Read visible text locally with the installed Windows OCR engine. Returns lines and words with display-local physical-pixel bounds and no image data. Windows OCR does not expose confidence scores.",
        object_schema(properties, &["display_id", "x", "y", "width", "height"]),
    )
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
    Tool::new(
        FIND_TEXT_ON_SCREEN,
        "Find visible text locally in a display region and return matching OCR lines with display-local bounds suitable for mouse actions. No screenshot or cloud service is used.",
        object_schema(properties, &["display_id", "x", "y", "width", "height", "query"]),
    )
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
            "description": "OCR text to click. The action is rejected unless exactly one matching line remains."
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
        "Find one unique OCR text line inside a bounded region, click its center, and return post-action evidence. Zero or ambiguous matches fail without injecting input, making this safer than reusing stale coordinates.",
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
        "Group discoverable titled application windows by their stable Windows virtual-desktop GUID and mark the current group. The supported Windows API cannot reveal empty desktops, desktop names, or desktop order; the result states these limitations explicitly.",
        object_schema(JsonObject::new(), &[]),
    )
    .with_raw_output_schema(Arc::new(json_object(json!({
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
    }))))
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
        "Switch left or right across existing Windows virtual desktops using the stable native shortcut, then return the resulting foreground screenshot. changed=false means the requested direction was already at an edge or Windows did not switch.",
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
        "List visible titled top-level Windows windows. IDs are ephemeral: refresh this list before focusing after windows open, close, or change ownership.",
        object_schema(JsonObject::new(), &[]),
    )
    .with_raw_output_schema(Arc::new(json_object(json!({
        "type": "object",
        "properties": {
            "count": { "type": "integer", "minimum": 0 },
            "windows": { "type": "array", "items": window_schema() }
        },
        "required": ["count", "windows"],
        "additionalProperties": false
    }))))
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
    properties.insert(
        "window_id".to_owned(),
        json!({
            "type": "string",
            "pattern": "^0[xX][0-9A-Fa-f]+:[0-9A-Fa-f]+$",
            "description": "Fresh ephemeral window ID returned by list_windows."
        }),
    );
    add_observation(&mut properties);
    Tool::new(
        FOCUS_WINDOW,
        "Restore and focus one existing visible window without launching applications, verify that it became foreground, then return a bounded screenshot of its display.",
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
        "Capture the current visible frame bounds of one existing window without focusing it or launching anything. Refresh list_windows first; minimized windows must be restored before capture.",
        object_schema(properties, &["window_id"]),
    )
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
        "Wait until a visible top-level window matches all supplied criteria. Provide at least one criterion. Returns matched=false and timed_out=true normally when the deadline expires.",
        object_schema(properties, &[]),
    )
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
        "Move the pointer to display-local physical-pixel coordinates, then return a screenshot of that display. Supports smooth movement within or between displays; never clicks or types.",
        object_schema(pointer_properties(), &["display_id", "x", "y"]),
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
        "Move to display-local physical-pixel coordinates, atomically perform one to three left, right, or middle clicks with optional modifiers, then return a post-action screenshot.",
        object_schema(properties, &["display_id", "x", "y", "button"]),
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
    properties.insert(
        "button".to_owned(),
        json!({ "type": "string", "enum": ["left", "right", "middle"] }),
    );
    properties.insert("modifiers".to_owned(), modifier_schema());
    properties.insert("duration_ms".to_owned(), json!({ "type": "integer", "minimum": 0, "maximum": MAX_MOVE_DURATION_MS, "default": 0, "description": "Smooth drag duration from start to end." }));
    add_observation(&mut properties);
    Tool::new(
        DRAG_MOUSE,
        "Atomically move to a start point, hold the selected mouse button and optional modifiers, drag within or between displays, release all held inputs, then return a post-action screenshot. No raw button-down state is exposed.",
        object_schema(properties, &["start_display_id", "start_x", "start_y", "end_display_id", "end_x", "end_y", "button"]),
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
        "Move to display-local physical-pixel coordinates, scroll vertically and optionally horizontally, wait briefly for the UI to update, then return a screenshot of that display.",
        object_schema(properties, &["display_id", "x", "y", "delta_y"]),
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
        "Send one explicit key or keyboard chord to the current foreground window, then return a bounded screenshot of the resulting foreground display. Call focus_window first when the target is uncertain.",
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
        "Type Unicode text into the current foreground window without using the clipboard, then return a bounded screenshot of the foreground display. Call focus_window first when the target is uncertain.",
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
            "pattern": "^0[xX][0-9A-Fa-f]+:[0-9A-Fa-f]+$",
            "description": "Fresh ephemeral window ID returned by list_windows."
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
    Arc::new(json_object(schema))
}

fn screenshot_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "display": display_schema(),
            "source_bounds": bounds_schema(),
            "mime_type": { "const": "image/png" },
            "byte_length": { "type": "integer", "minimum": 1 },
            "capture_method": { "type": "string" },
            "image_width": { "type": "integer", "minimum": 1 },
            "image_height": { "type": "integer", "minimum": 1 },
            "downscale_factor": { "type": "integer", "minimum": 1, "description": "Backward-compatible ceiling ratio. For exact mapping use source_bounds and image dimensions." },
            "resize_method": { "type": "string", "enum": ["native", "bilinear"] },
            "cursor_marker": { "type": "boolean" }
        },
        "required": ["display", "source_bounds", "mime_type", "byte_length", "capture_method", "image_width", "image_height", "downscale_factor", "resize_method", "cursor_marker"],
        "additionalProperties": false
    })
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

pub(super) fn json_object(value: Value) -> JsonObject {
    match value {
        Value::Object(object) => object,
        _ => unreachable!("tool schemas are always JSON objects"),
    }
}
