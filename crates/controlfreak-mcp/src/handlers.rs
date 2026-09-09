use super::{
    Arc, CallToolResponse, CallToolResult, CaptureDisplayInput, CaptureDisplayRequest,
    CaptureRegionInput, CaptureRegionRequest, CaptureWindowInput, CaptureWindowRequest,
    ClickMouseInput, ClickTextInput, ClickTextRequest, Diagnostics, DragMouseInput, ErrorData,
    FindTextInput, FocusWindowInput, FocusWindowRequest, IndicatorRuntime, JsonObject,
    KeyChordRequest, MouseClickRequest, MouseDragRequest, MouseMoveRequest, MouseScrollRequest,
    MoveMouseInput, NoArguments, OcrRegionInput, OcrRegionRequest, OperationLease, Ordering,
    PlatformBackend, PressKeysInput, SafetyIndicator, ScrollMouseInput, SwitchVirtualDesktopInput,
    TextInputRequest, TypeTextInput, Value, VirtualDesktopSwitchRequest, VisualBaselineInput,
    VisualBaselineRequest, WaitForChangeSinceInput, WaitForChangeSinceRequest,
    WaitForVisualChangeInput, WaitForVisualChangeRequest, WaitForWindowInput, WaitForWindowRequest,
    click_text_result, join_error, json, keyboard_result, parse_arguments, pointer_result,
    screenshot_result, tool_error, virtual_desktop_list_result, virtual_desktop_switch_result,
    visual_baseline_result, visual_change_result, window_focus_result, window_screenshot_result,
    window_wait_result,
};

pub(super) async fn run_platform_operation<T, F>(
    operation_lease: Option<OperationLease>,
    operation: F,
) -> Result<T, ErrorData>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let _operation_lease = operation_lease;
        operation()
    })
    .await
    .map_err(|error| join_error(&error))
}

async fn run_mutation_operation<T, F>(
    operation_lease: Option<OperationLease>,
    operation: F,
) -> Result<T, ErrorData>
where
    T: Send + 'static,
    F: FnOnce(&controlfreak_core::MutationControl) -> T + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let control = operation_lease.as_ref().map_or_else(
            controlfreak_core::MutationControl::default,
            OperationLease::mutation_control,
        );
        let _operation_lease = operation_lease;
        operation(&control)
    })
    .await
    .map_err(|error| join_error(&error))
}

pub(super) fn call_get_server_status(
    backend: &Arc<dyn PlatformBackend>,
    diagnostics: &Diagnostics,
    safety_indicator: &SafetyIndicator,
    indicator_runtime: Option<&Arc<IndicatorRuntime>>,
    arguments: Option<JsonObject>,
) -> Result<CallToolResponse, ErrorData> {
    parse_arguments::<NoArguments>(arguments)?;
    let recent_operations: Vec<Value> = diagnostics
        .recent_operations
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .map(|operation| {
            json!({
                "operation_id": operation.operation_id,
                "tool": operation.tool,
                "status": if operation.failed { "failed" } else { "completed" },
                "elapsed_ms": operation.elapsed_ms,
                "gap_ms": operation.gap_ms,
            })
        })
        .collect();
    let security_context = backend.security_context();
    Ok(CallToolResult::structured(json!({
        "status": if safety_indicator.is_failed() { "degraded" } else { "ready" },
        "safety_indicator": {
            "status": safety_indicator.status(),
            "reason": safety_indicator.failure_reason(),
        },
        "session": indicator_runtime.map(|runtime| runtime.status()),
        "version": env!("CARGO_PKG_VERSION"),
        "instance_id": diagnostics.instance_id,
        "process_id": std::process::id(),
        "server_elevated": security_context.elevated,
        "windows_integrity_level": security_context.windows_integrity_level,
        "elevated_operation_allowed": security_context.elevated_operation_allowed,
        "uptime_ms": u64::try_from(diagnostics.started.elapsed().as_millis()).unwrap_or(u64::MAX),
        "operations_started": diagnostics.next_operation_id.load(Ordering::Relaxed).saturating_sub(1),
        "operations_completed": diagnostics.completed.load(Ordering::Relaxed),
        "operations_failed": diagnostics.failed.load(Ordering::Relaxed),
        "recent_operations": recent_operations,
        "backend": backend.identity(),
    }))
    .into())
}

pub(super) async fn call_list_displays(
    backend: Arc<dyn PlatformBackend>,
    arguments: Option<JsonObject>,
    operation_lease: Option<OperationLease>,
) -> Result<CallToolResponse, ErrorData> {
    parse_arguments::<NoArguments>(arguments)?;
    let result = run_platform_operation(operation_lease, move || backend.list_displays()).await?;
    match result {
        Ok(displays) => Ok(CallToolResult::structured(json!({
            "count": displays.len(),
            "displays": displays,
        }))
        .into()),
        Err(error) => Ok(tool_error(&error).into()),
    }
}

pub(super) async fn call_capture_display(
    backend: Arc<dyn PlatformBackend>,
    arguments: Option<JsonObject>,
    operation_lease: Option<OperationLease>,
) -> Result<CallToolResponse, ErrorData> {
    let input = parse_arguments::<CaptureDisplayInput>(arguments)?;
    let request = CaptureDisplayRequest {
        display_id: input.display_id,
        max_width: input.max_width,
        include_cursor: input.include_cursor,
    };
    let result =
        run_platform_operation(operation_lease, move || backend.capture_display(&request)).await?;
    match result {
        Ok(screenshot) => Ok(screenshot_result(&screenshot).into()),
        Err(error) => Ok(tool_error(&error).into()),
    }
}

pub(super) async fn call_capture_region(
    backend: Arc<dyn PlatformBackend>,
    arguments: Option<JsonObject>,
    operation_lease: Option<OperationLease>,
) -> Result<CallToolResponse, ErrorData> {
    let input = parse_arguments::<CaptureRegionInput>(arguments)?;
    let request = CaptureRegionRequest {
        display_id: input.display_id,
        x: input.x,
        y: input.y,
        width: input.width,
        height: input.height,
        max_width: input.max_width,
        include_cursor: input.include_cursor,
    };
    let result =
        run_platform_operation(operation_lease, move || backend.capture_region(&request)).await?;
    match result {
        Ok(screenshot) => Ok(screenshot_result(&screenshot).into()),
        Err(error) => Ok(tool_error(&error).into()),
    }
}

pub(super) async fn call_wait_for_visual_change(
    backend: Arc<dyn PlatformBackend>,
    arguments: Option<JsonObject>,
    operation_lease: Option<OperationLease>,
) -> Result<CallToolResponse, ErrorData> {
    let input = parse_arguments::<WaitForVisualChangeInput>(arguments)?;
    let request = WaitForVisualChangeRequest {
        display_id: input.display_id,
        x: input.x,
        y: input.y,
        width: input.width,
        height: input.height,
        timeout_ms: input.timeout_ms,
        stable_ms: input.stable_ms,
        difference_threshold: input.difference_threshold,
    };
    let result = run_platform_operation(operation_lease, move || {
        backend.wait_for_visual_change(&request)
    })
    .await?;
    match result {
        Ok(result) => Ok(visual_change_result(&result).into()),
        Err(error) => Ok(tool_error(&error).into()),
    }
}

pub(super) async fn call_capture_visual_baseline(
    backend: Arc<dyn PlatformBackend>,
    arguments: Option<JsonObject>,
    operation_lease: Option<OperationLease>,
) -> Result<CallToolResponse, ErrorData> {
    let input = parse_arguments::<VisualBaselineInput>(arguments)?;
    let request = VisualBaselineRequest {
        display_id: input.display_id,
        x: input.x,
        y: input.y,
        width: input.width,
        height: input.height,
    };
    let result = run_platform_operation(operation_lease, move || {
        backend.capture_visual_baseline(&request)
    })
    .await?;
    match result {
        Ok(result) => Ok(visual_baseline_result(&result).into()),
        Err(error) => Ok(tool_error(&error).into()),
    }
}

pub(super) async fn call_wait_for_change_since(
    backend: Arc<dyn PlatformBackend>,
    arguments: Option<JsonObject>,
    operation_lease: Option<OperationLease>,
) -> Result<CallToolResponse, ErrorData> {
    let input = parse_arguments::<WaitForChangeSinceInput>(arguments)?;
    let request = WaitForChangeSinceRequest {
        baseline_id: input.baseline_id,
        timeout_ms: input.timeout_ms,
        stable_ms: input.stable_ms,
        difference_threshold: input.difference_threshold,
    };
    let result = run_platform_operation(operation_lease, move || {
        backend.wait_for_change_since(&request)
    })
    .await?;
    match result {
        Ok(result) => Ok(visual_change_result(&result).into()),
        Err(error) => Ok(tool_error(&error).into()),
    }
}

pub(super) async fn call_read_text_in_region(
    backend: Arc<dyn PlatformBackend>,
    arguments: Option<JsonObject>,
    operation_lease: Option<OperationLease>,
) -> Result<CallToolResponse, ErrorData> {
    let input = parse_arguments::<OcrRegionInput>(arguments)?;
    let request = input.into_request();
    let result = run_platform_operation(operation_lease, move || {
        backend.read_text_in_region(&request)
    })
    .await?;
    match result {
        Ok(result) => Ok(CallToolResult::structured(json!(result)).into()),
        Err(error) => Ok(tool_error(&error).into()),
    }
}

pub(super) async fn call_find_text_on_screen(
    backend: Arc<dyn PlatformBackend>,
    arguments: Option<JsonObject>,
    operation_lease: Option<OperationLease>,
) -> Result<CallToolResponse, ErrorData> {
    let input = parse_arguments::<FindTextInput>(arguments)?;
    let request = input.into_request();
    let result = run_platform_operation(operation_lease, move || {
        backend.find_text_on_screen(&request)
    })
    .await?;
    match result {
        Ok(result) => Ok(CallToolResult::structured(json!({
            "count": result.matches.len(),
            "query": result.query,
            "display": result.display,
            "source_bounds": result.source_bounds,
            "language": result.language,
            "matches": result.matches,
        }))
        .into()),
        Err(error) => Ok(tool_error(&error).into()),
    }
}

pub(super) async fn call_click_text(
    backend: Arc<dyn PlatformBackend>,
    arguments: Option<JsonObject>,
    operation_lease: Option<OperationLease>,
) -> Result<CallToolResponse, ErrorData> {
    let input = parse_arguments::<ClickTextInput>(arguments)?;
    let action = json!({
        "kind": "click_text",
        "query": input.query,
        "exact_match": input.exact_match,
        "button": input.button,
        "click_count": input.click_count,
        "modifiers": input.modifiers,
    });
    let request = ClickTextRequest {
        region: OcrRegionRequest {
            display_id: input.display_id,
            x: input.x,
            y: input.y,
            width: input.width,
            height: input.height,
            language: input.language,
        },
        query: input.query,
        case_sensitive: input.case_sensitive,
        exact_match: input.exact_match,
        button: input.button,
        click_count: input.click_count,
        modifiers: input.modifiers,
        duration_ms: input.duration_ms,
        observation: input.observation.into(),
    };
    let result = run_mutation_operation(operation_lease, move |control| {
        backend.click_text_controlled(&request, control)
    })
    .await?;
    match result {
        Ok(result) => Ok(click_text_result(&result, &action).into()),
        Err(error) => Ok(tool_error(&error).into()),
    }
}

pub(super) async fn call_move_mouse(
    backend: Arc<dyn PlatformBackend>,
    arguments: Option<JsonObject>,
    operation_lease: Option<OperationLease>,
) -> Result<CallToolResponse, ErrorData> {
    let input = parse_arguments::<MoveMouseInput>(arguments)?;
    let request = MouseMoveRequest {
        display_id: input.display_id,
        x: input.x,
        y: input.y,
        duration_ms: input.duration_ms,
        observation: input.observation.into(),
    };
    let result = run_mutation_operation(operation_lease, move |control| {
        backend.move_mouse_controlled(&request, control)
    })
    .await?;
    match result {
        Ok(result) => Ok(pointer_result(&result, &json!({ "kind": "move" })).into()),
        Err(error) => Ok(tool_error(&error).into()),
    }
}

pub(super) async fn call_click_mouse(
    backend: Arc<dyn PlatformBackend>,
    arguments: Option<JsonObject>,
    operation_lease: Option<OperationLease>,
) -> Result<CallToolResponse, ErrorData> {
    let input = parse_arguments::<ClickMouseInput>(arguments)?;
    let button = input.button;
    let click_request = MouseClickRequest {
        display_id: input.display_id,
        x: input.x,
        y: input.y,
        button,
        click_count: input.click_count,
        modifiers: input.modifiers.clone(),
        duration_ms: input.duration_ms,
        observation: input.observation.into(),
    };
    let result = run_mutation_operation(operation_lease, move |control| {
        backend.click_mouse_controlled(&click_request, control)
    })
    .await?;
    match result {
        Ok(result) => Ok(pointer_result(
            &result,
            &json!({
                "kind": "click",
                "button": button,
                "click_count": input.click_count,
                "modifiers": input.modifiers,
            }),
        )
        .into()),
        Err(error) => Ok(tool_error(&error).into()),
    }
}

pub(super) async fn call_drag_mouse(
    backend: Arc<dyn PlatformBackend>,
    arguments: Option<JsonObject>,
    operation_lease: Option<OperationLease>,
) -> Result<CallToolResponse, ErrorData> {
    let input = parse_arguments::<DragMouseInput>(arguments)?;
    let action = json!({
        "kind": "drag",
        "button": input.button,
        "modifiers": input.modifiers,
        "start": {
            "display_id": input.start_display_id,
            "x": input.start_x,
            "y": input.start_y,
        },
        "end": {
            "display_id": input.end_display_id,
            "x": input.end_x,
            "y": input.end_y,
        },
    });
    let request = MouseDragRequest {
        start_display_id: input.start_display_id,
        start_x: input.start_x,
        start_y: input.start_y,
        end_display_id: input.end_display_id,
        end_x: input.end_x,
        end_y: input.end_y,
        button: input.button,
        modifiers: input.modifiers,
        duration_ms: input.duration_ms,
        observation: input.observation.into(),
    };
    let result = run_mutation_operation(operation_lease, move |control| {
        backend.drag_mouse_controlled(&request, control)
    })
    .await?;
    match result {
        Ok(result) => Ok(pointer_result(&result, &action).into()),
        Err(error) => Ok(tool_error(&error).into()),
    }
}

pub(super) async fn call_list_windows(
    backend: Arc<dyn PlatformBackend>,
    arguments: Option<JsonObject>,
    operation_lease: Option<OperationLease>,
) -> Result<CallToolResponse, ErrorData> {
    parse_arguments::<NoArguments>(arguments)?;
    let result = run_platform_operation(operation_lease, move || backend.list_windows()).await?;
    match result {
        Ok(windows) => Ok(CallToolResult::structured(json!({
            "count": windows.len(),
            "windows": windows,
        }))
        .into()),
        Err(error) => Ok(tool_error(&error).into()),
    }
}

pub(super) async fn call_list_virtual_desktops(
    backend: Arc<dyn PlatformBackend>,
    arguments: Option<JsonObject>,
    operation_lease: Option<OperationLease>,
) -> Result<CallToolResponse, ErrorData> {
    parse_arguments::<NoArguments>(arguments)?;
    let result =
        run_platform_operation(operation_lease, move || backend.list_virtual_desktops()).await?;
    match result {
        Ok(result) => Ok(virtual_desktop_list_result(&result).into()),
        Err(error) => Ok(tool_error(&error).into()),
    }
}

pub(super) async fn call_switch_virtual_desktop(
    backend: Arc<dyn PlatformBackend>,
    arguments: Option<JsonObject>,
    operation_lease: Option<OperationLease>,
) -> Result<CallToolResponse, ErrorData> {
    let input = parse_arguments::<SwitchVirtualDesktopInput>(arguments)?;
    let request = VirtualDesktopSwitchRequest {
        direction: input.direction,
        steps: input.steps,
        observation: input.observation.into(),
    };
    let result = run_mutation_operation(operation_lease, move |control| {
        backend.switch_virtual_desktop_controlled(&request, control)
    })
    .await?;
    match result {
        Ok(result) => Ok(virtual_desktop_switch_result(&result).into()),
        Err(error) => Ok(tool_error(&error).into()),
    }
}

pub(super) async fn call_focus_window(
    backend: Arc<dyn PlatformBackend>,
    arguments: Option<JsonObject>,
    operation_lease: Option<OperationLease>,
) -> Result<CallToolResponse, ErrorData> {
    let input = parse_arguments::<FocusWindowInput>(arguments)?;
    let request = FocusWindowRequest {
        window_id: input.window_id,
        observation: input.observation.into(),
    };
    let result = run_mutation_operation(operation_lease, move |control| {
        backend.focus_window_controlled(&request, control)
    })
    .await?;
    match result {
        Ok(result) => Ok(window_focus_result(&result).into()),
        Err(error) => Ok(tool_error(&error).into()),
    }
}

pub(super) async fn call_capture_window(
    backend: Arc<dyn PlatformBackend>,
    arguments: Option<JsonObject>,
    operation_lease: Option<OperationLease>,
) -> Result<CallToolResponse, ErrorData> {
    let input = parse_arguments::<CaptureWindowInput>(arguments)?;
    let request = CaptureWindowRequest {
        window_id: input.window_id,
        max_width: input.max_width,
        include_cursor: input.include_cursor,
    };
    let result =
        run_platform_operation(operation_lease, move || backend.capture_window(&request)).await?;
    match result {
        Ok(result) => Ok(window_screenshot_result(&result).into()),
        Err(error) => Ok(tool_error(&error).into()),
    }
}

pub(super) async fn call_wait_for_window(
    backend: Arc<dyn PlatformBackend>,
    arguments: Option<JsonObject>,
    operation_lease: Option<OperationLease>,
) -> Result<CallToolResponse, ErrorData> {
    let input = parse_arguments::<WaitForWindowInput>(arguments)?;
    let request = WaitForWindowRequest {
        window_id: input.window_id,
        title_contains: input.title_contains,
        class_name: input.class_name,
        process_id: input.process_id,
        is_foreground: input.is_foreground,
        timeout_ms: input.timeout_ms,
    };
    let result =
        run_platform_operation(operation_lease, move || backend.wait_for_window(&request)).await?;
    match result {
        Ok(result) => Ok(window_wait_result(&result).into()),
        Err(error) => Ok(tool_error(&error).into()),
    }
}

pub(super) async fn call_press_keys(
    backend: Arc<dyn PlatformBackend>,
    arguments: Option<JsonObject>,
    operation_lease: Option<OperationLease>,
) -> Result<CallToolResponse, ErrorData> {
    let input = parse_arguments::<PressKeysInput>(arguments)?;
    let action_keys = input.keys.clone();
    let request = KeyChordRequest {
        keys: input.keys,
        observation: input.observation.into(),
    };
    let result = run_mutation_operation(operation_lease, move |control| {
        backend.press_keys_controlled(&request, control)
    })
    .await?;
    match result {
        Ok(result) => Ok(keyboard_result(
            &result,
            &json!({ "kind": "key_chord", "keys": action_keys }),
        )
        .into()),
        Err(error) => Ok(tool_error(&error).into()),
    }
}

pub(super) async fn call_type_text(
    backend: Arc<dyn PlatformBackend>,
    arguments: Option<JsonObject>,
    operation_lease: Option<OperationLease>,
) -> Result<CallToolResponse, ErrorData> {
    let input = parse_arguments::<TypeTextInput>(arguments)?;
    let text_length = input.text.encode_utf16().count();
    let request = TextInputRequest {
        text: input.text,
        observation: input.observation.into(),
    };
    let result = run_mutation_operation(operation_lease, move |control| {
        backend.type_text_controlled(&request, control)
    })
    .await?;
    match result {
        Ok(result) => Ok(keyboard_result(
            &result,
            &json!({ "kind": "text", "requested_utf16_units": text_length }),
        )
        .into()),
        Err(error) => Ok(tool_error(&error).into()),
    }
}

pub(super) async fn call_scroll_mouse(
    backend: Arc<dyn PlatformBackend>,
    arguments: Option<JsonObject>,
    operation_lease: Option<OperationLease>,
) -> Result<CallToolResponse, ErrorData> {
    let input = parse_arguments::<ScrollMouseInput>(arguments)?;
    let delta_x = input.delta_x;
    let delta_y = input.delta_y;
    let scroll_request = MouseScrollRequest {
        display_id: input.display_id,
        x: input.x,
        y: input.y,
        delta_x,
        delta_y,
        duration_ms: input.duration_ms,
        observation: input.observation.into(),
    };
    let result = run_mutation_operation(operation_lease, move |control| {
        backend.scroll_mouse_controlled(&scroll_request, control)
    })
    .await?;
    match result {
        Ok(result) => Ok(pointer_result(
            &result,
            &json!({
                "kind": "scroll",
                "delta_x": delta_x,
                "delta_y": delta_y,
            }),
        )
        .into()),
        Err(error) => Ok(tool_error(&error).into()),
    }
}
