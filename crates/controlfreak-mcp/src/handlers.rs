use controlfreak_core::{
    ActionObservation, InputOutcome, KeyboardActionResult, MutationControl, MutationProgress,
    ObservationMode, ObservationOptions, PlatformError,
};

use super::requests::{ActionStep, RunActionsInput};

use super::{
    Arc, CallToolResponse, CallToolResult, CaptureDisplayInput, CaptureDisplayRequest,
    CaptureRegionInput, CaptureRegionRequest, CaptureWindowInput, CaptureWindowRequest, CarriesPng,
    ClickMouseInput, ClickTextInput, ClickTextRequest, Diagnostics, DragMouseInput, Encoded,
    ErrorData, FindTextInput, FocusWindowInput, FocusWindowRequest, IndicatorRuntime, JsonObject,
    KeyChordRequest, MouseClickRequest, MouseDragRequest, MouseMoveRequest, MouseScrollRequest,
    MoveMouseInput, NoArguments, OcrRegionInput, OcrRegionRequest, OperationLease, Ordering,
    PlatformBackend, PressKeysInput, SafetyIndicator, ScrollMouseInput, SwitchVirtualDesktopInput,
    TextInputRequest, TypeTextInput, Value, VirtualDesktopSwitchRequest, VisualBaselineInput,
    VisualBaselineRequest, WaitForChangeSinceInput, WaitForChangeSinceRequest, WaitForTextInput,
    WaitForVisualChangeInput, WaitForVisualChangeRequest, WaitForWindowInput, WaitForWindowRequest,
    click_text_result, join_error, json, keyboard_result, parse_arguments, pointer_result,
    screenshot_result, tool_error, virtual_desktop_list_result, virtual_desktop_switch_result,
    visual_baseline_result, visual_change_result, window_focus_result, window_screenshot_result,
    window_wait_result,
};

pub(super) async fn run_platform_operation<T, F>(
    operation_lease: Option<OperationLease>,
    operation: F,
) -> Result<Result<T, PlatformError>, ErrorData>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, PlatformError> + Send + 'static,
{
    run_platform_operation_then(operation_lease, operation, |value| value).await
}

/// Runs an image-producing observation and base64-encodes its PNG on the blocking worker,
/// after the last cancellation check, so a cancelled request never pays for encoding.
pub(super) async fn run_image_operation<T, F>(
    operation_lease: Option<OperationLease>,
    operation: F,
) -> Result<Result<Encoded<T>, PlatformError>, ErrorData>
where
    T: CarriesPng + Send + 'static,
    F: FnOnce() -> Result<T, PlatformError> + Send + 'static,
{
    run_platform_operation_then(operation_lease, operation, Encoded::new).await
}

/// `finish` runs on the blocking worker only when the operation succeeded and the request was
/// not cancelled, and its time is recorded with the operation's stages.
async fn run_platform_operation_then<T, U, F>(
    operation_lease: Option<OperationLease>,
    operation: F,
    finish: impl FnOnce(T) -> U + Send + 'static,
) -> Result<Result<U, PlatformError>, ErrorData>
where
    T: Send + 'static,
    U: Send + 'static,
    F: FnOnce() -> Result<T, PlatformError> + Send + 'static,
{
    let work = super::stop::current();
    tokio::task::spawn_blocking(move || {
        let _operation_lease = operation_lease;
        let control = work.as_ref().map(|work| &work.control);
        if let Some(control) = control {
            control.check("observation_worker")?;
        }
        controlfreak_core::timing::reset();
        let result = operation();
        let cancelled = control.map_or(Ok(()), |control| control.check("observation_worker"));
        let result = match cancelled {
            Ok(()) => result.map(finish),
            Err(error) => Err(error),
        };
        let taken = controlfreak_core::timing::take();
        if let Some(work) = &work {
            work.record_stages(taken);
        }
        result
    })
    .await
    .map_err(|error| join_error(&error))
}

/// Runs a mutation whose result may carry a screenshot, encoding it inside the blocking worker.
pub(super) async fn run_encoded_mutation<T, F>(
    operation_lease: Option<OperationLease>,
    target_ref: String,
    backend: Arc<dyn PlatformBackend>,
    operation: F,
) -> (Result<Encoded<T>, PlatformError>, MutationProgress)
where
    T: CarriesPng + Send + 'static,
    F: FnOnce(&MutationControl) -> Result<T, PlatformError> + Send + 'static,
{
    run_mutation_operation(operation_lease, target_ref, backend, move |control| {
        operation(control).map(Encoded::new)
    })
    .await
}

pub(super) async fn run_mutation_operation<T, F>(
    operation_lease: Option<OperationLease>,
    target_ref: String,
    backend: Arc<dyn PlatformBackend>,
    operation: F,
) -> (Result<T, PlatformError>, MutationProgress)
where
    T: Send + 'static,
    F: FnOnce(&MutationControl) -> Result<T, PlatformError> + Send + 'static,
{
    let work = super::stop::current();
    let request_control = work
        .as_ref()
        .map_or_else(MutationControl::default, |work| work.control.clone());
    let control = request_control.with_cancellation(
        operation_lease
            .as_ref()
            .map_or_else(MutationControl::default, OperationLease::mutation_control),
    );
    let worker_control = control.clone();
    let result = tokio::task::spawn_blocking(move || {
        let registered_work = work;
        let _operation_lease = operation_lease;
        let _cleanup = super::stop::CleanupOnDrop(registered_work.clone());
        worker_control.check("mutation_worker")?;
        controlfreak_core::timing::reset();
        let result = (|| {
            backend.validate_target_reference(&target_ref)?;
            worker_control.bind_target(&target_ref)?;
            let result = operation(&worker_control);
            backend.record_target_evidence(&worker_control);
            result
        })();
        if let Some(work) = &registered_work {
            work.record_stages(controlfreak_core::timing::take());
        }
        result
    })
    .await;
    let result = result.unwrap_or_else(|_| {
        // A worker can fail while a native/provider call is in flight. Never replay it.
        if control.progress().input_outcome == InputOutcome::NotStarted {
            control.dispatch_started();
        }
        Err(PlatformError::OperationFailed {
            operation: "mutation_worker".to_owned(),
            reason: "mutation worker failed; observe the desktop before recovery".to_owned(),
        })
    });
    (result, control.progress())
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
                "status": operation.status,
                "elapsed_ms": operation.elapsed_ms,
                "gap_ms": operation.gap_ms,
                "stages_ms": operation.stages_ms,
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
        "session": indicator_runtime.map(|runtime| runtime.try_status()),
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
        run_image_operation(operation_lease, move || backend.capture_display(&request)).await?;
    match result {
        Ok(screenshot) => Ok(screenshot_result(screenshot).into()),
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
        run_image_operation(operation_lease, move || backend.capture_region(&request)).await?;
    match result {
        Ok(screenshot) => Ok(screenshot_result(screenshot).into()),
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
    let control =
        super::stop::current().map_or_else(MutationControl::default, |work| work.control.clone());
    let result = run_image_operation(operation_lease, move || {
        backend.wait_for_visual_change_controlled(&request, &control)
    })
    .await?;
    match result {
        Ok(result) => Ok(visual_change_result(result).into()),
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
    let control =
        super::stop::current().map_or_else(MutationControl::default, |work| work.control.clone());
    let result = run_image_operation(operation_lease, move || {
        backend.wait_for_change_since_controlled(&request, &control)
    })
    .await?;
    match result {
        Ok(result) => Ok(visual_change_result(result).into()),
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
    let control =
        super::stop::current().map_or_else(MutationControl::default, |work| work.control.clone());
    let result = run_platform_operation(operation_lease, move || {
        backend.read_text_in_region_controlled(&request, &control)
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
    let control =
        super::stop::current().map_or_else(MutationControl::default, |work| work.control.clone());
    let result = run_platform_operation(operation_lease, move || {
        backend.find_text_on_screen_controlled(&request, &control)
    })
    .await?;
    match result {
        Ok(result) => Ok(CallToolResult::structured(json!({
            "count": result.matches.len(),
            "target_ref": result.target_ref,
            "query": result.query,
            "display": result.display,
            "source_bounds": result.source_bounds,
            "language": result.language,
            "matches": result.matches,
            "match_tier": result.details.match_tier,
            "candidate_count": result.details.candidate_count,
            "candidates_truncated": result.details.candidates_truncated,
            "text_truncated": result.details.text_truncated,
            "recovery": result.details.recovery,
        }))
        .into()),
        Err(error) => Ok(tool_error(&error).into()),
    }
}

pub(super) async fn call_wait_for_text(
    backend: Arc<dyn PlatformBackend>,
    arguments: Option<JsonObject>,
    operation_lease: Option<OperationLease>,
) -> Result<CallToolResponse, ErrorData> {
    let input = parse_arguments::<WaitForTextInput>(arguments)?;
    let request = input.into_request();
    let control =
        super::stop::current().map_or_else(MutationControl::default, |work| work.control.clone());
    let result = run_platform_operation(operation_lease, move || {
        backend.wait_for_text_controlled(&request, &control)
    })
    .await?;
    match result {
        Ok(result) => Ok(CallToolResult::structured(json!({
            "condition": result.condition,
            "satisfied": result.satisfied,
            "timed_out": result.timed_out,
            "elapsed_ms": result.elapsed_ms,
            "attempts": result.attempts,
            "count": result.last.matches.len(),
            "target_ref": result.last.target_ref,
            "query": result.last.query,
            "display": result.last.display,
            "source_bounds": result.last.source_bounds,
            "language": result.last.language,
            "matches": result.last.matches,
            "match_tier": result.last.details.match_tier,
            "candidate_count": result.last.details.candidate_count,
            "candidates_truncated": result.last.details.candidates_truncated,
            "text_truncated": result.last.details.text_truncated,
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
    let (result, progress) = run_encoded_mutation(
        operation_lease,
        input.target_ref,
        Arc::clone(&backend),
        move |control| backend.click_text_controlled(&request, control),
    )
    .await;
    match result {
        Ok(result) => Ok(click_text_result(result, &action).into()),
        Err(error) => Ok(tool_error(&error).into()),
    }
    .map(|response| super::results::with_progress(response, progress))
}

pub(super) async fn call_move_mouse(
    backend: Arc<dyn PlatformBackend>,
    arguments: Option<JsonObject>,
    operation_lease: Option<OperationLease>,
) -> Result<CallToolResponse, ErrorData> {
    let input = parse_arguments::<MoveMouseInput>(arguments)?;
    let target = match input.pointer_target() {
        Ok(target) => target,
        Err(error) => return Ok(not_started(&error)),
    };
    let duration_ms = input.duration_ms;
    let observation = input.observation.into();
    let (result, progress) = run_encoded_mutation(
        operation_lease,
        input.target_ref,
        Arc::clone(&backend),
        move |control| {
            let (display_id, x, y) = target.resolve(backend.as_ref())?;
            let request = MouseMoveRequest {
                display_id,
                x,
                y,
                duration_ms,
                observation,
            };
            backend.move_mouse_controlled(&request, control)
        },
    )
    .await;
    match result {
        Ok(result) => Ok(pointer_result(result, &json!({ "kind": "move" })).into()),
        Err(error) => Ok(tool_error(&error).into()),
    }
    .map(|response| super::results::with_progress(response, progress))
}

pub(super) async fn call_click_mouse(
    backend: Arc<dyn PlatformBackend>,
    arguments: Option<JsonObject>,
    operation_lease: Option<OperationLease>,
) -> Result<CallToolResponse, ErrorData> {
    let input = parse_arguments::<ClickMouseInput>(arguments)?;
    let target = match input.pointer_target() {
        Ok(target) => target,
        Err(error) => return Ok(not_started(&error)),
    };
    let button = input.button;
    let click_count = input.click_count;
    let modifiers = input.modifiers.clone();
    let duration_ms = input.duration_ms;
    let observation = input.observation.into();
    let (result, progress) = run_encoded_mutation(
        operation_lease,
        input.target_ref,
        Arc::clone(&backend),
        move |control| {
            let (display_id, x, y) = target.resolve(backend.as_ref())?;
            let click_request = MouseClickRequest {
                display_id,
                x,
                y,
                button,
                click_count,
                modifiers,
                duration_ms,
                observation,
            };
            backend.click_mouse_controlled(&click_request, control)
        },
    )
    .await;
    match result {
        Ok(result) => Ok(pointer_result(
            result,
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
    .map(|response| super::results::with_progress(response, progress))
}

pub(super) async fn call_drag_mouse(
    backend: Arc<dyn PlatformBackend>,
    arguments: Option<JsonObject>,
    operation_lease: Option<OperationLease>,
) -> Result<CallToolResponse, ErrorData> {
    let input = parse_arguments::<DragMouseInput>(arguments)?;
    let (start, end) = match input.pointer_targets() {
        Ok(targets) => targets,
        Err(error) => return Ok(not_started(&error)),
    };
    let action = json!({
        "kind": "drag",
        "button": input.button,
        "modifiers": input.modifiers,
        "start": start.to_json(),
        "end": end.to_json(),
    });
    let button = input.button;
    let modifiers = input.modifiers;
    let duration_ms = input.duration_ms;
    let observation = input.observation.into();
    let (result, progress) = run_encoded_mutation(
        operation_lease,
        input.target_ref,
        Arc::clone(&backend),
        move |control| {
            let (start_display_id, start_x, start_y) = start.resolve(backend.as_ref())?;
            let (end_display_id, end_x, end_y) = end.resolve(backend.as_ref())?;
            let request = MouseDragRequest {
                start_display_id,
                start_x,
                start_y,
                end_display_id,
                end_x,
                end_y,
                button,
                modifiers,
                duration_ms,
                observation,
            };
            backend.drag_mouse_controlled(&request, control)
        },
    )
    .await;
    match result {
        Ok(result) => Ok(pointer_result(result, &action).into()),
        Err(error) => Ok(tool_error(&error).into()),
    }
    .map(|response| super::results::with_progress(response, progress))
}

pub(super) async fn call_list_windows(
    backend: Arc<dyn PlatformBackend>,
    arguments: Option<JsonObject>,
    operation_lease: Option<OperationLease>,
) -> Result<CallToolResponse, ErrorData> {
    parse_arguments::<NoArguments>(arguments)?;
    let result = run_platform_operation(operation_lease, move || backend.list_windows()).await?;
    match result {
        Ok(list) => Ok(CallToolResult::structured(json!({
            "count": list.windows.len(),
            "truncated": list.truncated,
            "windows": list.windows,
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
    let (result, progress) = run_encoded_mutation(
        operation_lease,
        input.target_ref,
        Arc::clone(&backend),
        move |control| backend.switch_virtual_desktop_controlled(&request, control),
    )
    .await;
    match result {
        Ok(result) => Ok(virtual_desktop_switch_result(result).into()),
        Err(error) => Ok(tool_error(&error).into()),
    }
    .map(|response| super::results::with_progress(response, progress))
}

pub(super) async fn call_focus_window(
    backend: Arc<dyn PlatformBackend>,
    arguments: Option<JsonObject>,
    operation_lease: Option<OperationLease>,
) -> Result<CallToolResponse, ErrorData> {
    let input = parse_arguments::<FocusWindowInput>(arguments)?;
    let request = FocusWindowRequest {
        timeout_ms: input.timeout_ms,
        window_id: input.window_id,
        observation: input.observation.into(),
    };
    let (result, progress) = run_encoded_mutation(
        operation_lease,
        request.window_id.clone(),
        Arc::clone(&backend),
        move |control| backend.focus_window_controlled(&request, control),
    )
    .await;
    match result {
        Ok(result) => Ok(window_focus_result(result).into()),
        Err(error) => Ok(tool_error(&error).into()),
    }
    .map(|response| super::results::with_progress(response, progress))
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
        run_image_operation(operation_lease, move || backend.capture_window(&request)).await?;
    match result {
        Ok(result) => Ok(window_screenshot_result(result).into()),
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
    let control =
        super::stop::current().map_or_else(MutationControl::default, |work| work.control.clone());
    let result = run_platform_operation(operation_lease, move || {
        backend.wait_for_window_controlled(&request, &control)
    })
    .await?;
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
    let (result, progress) = run_encoded_mutation(
        operation_lease,
        input.target_ref,
        Arc::clone(&backend),
        move |control| backend.press_keys_controlled(&request, control),
    )
    .await;
    match result {
        Ok(result) => Ok(keyboard_result(
            result,
            &json!({ "kind": "key_chord", "keys": action_keys }),
        )
        .into()),
        Err(error) => Ok(tool_error(&error).into()),
    }
    .map(|response| super::results::with_progress(response, progress))
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
    let (result, progress) = run_encoded_mutation(
        operation_lease,
        input.target_ref,
        Arc::clone(&backend),
        move |control| backend.type_text_controlled(&request, control),
    )
    .await;
    match result {
        Ok(result) => Ok(keyboard_result(
            result,
            &json!({ "kind": "text", "requested_utf16_units": text_length }),
        )
        .into()),
        Err(error) => Ok(tool_error(&error).into()),
    }
    .map(|response| super::results::with_progress(response, progress))
}

pub(super) async fn call_scroll_mouse(
    backend: Arc<dyn PlatformBackend>,
    arguments: Option<JsonObject>,
    operation_lease: Option<OperationLease>,
) -> Result<CallToolResponse, ErrorData> {
    let input = parse_arguments::<ScrollMouseInput>(arguments)?;
    let target = match input.pointer_target() {
        Ok(target) => target,
        Err(error) => return Ok(not_started(&error)),
    };
    let delta_x = input.delta_x;
    let delta_y = input.delta_y;
    let duration_ms = input.duration_ms;
    let observation = input.observation.into();
    let (result, progress) = run_encoded_mutation(
        operation_lease,
        input.target_ref,
        Arc::clone(&backend),
        move |control| {
            let (display_id, x, y) = target.resolve(backend.as_ref())?;
            let scroll_request = MouseScrollRequest {
                display_id,
                x,
                y,
                delta_x,
                delta_y,
                duration_ms,
                observation,
            };
            backend.scroll_mouse_controlled(&scroll_request, control)
        },
    )
    .await;
    match result {
        Ok(result) => Ok(pointer_result(
            result,
            &json!({
                "kind": "scroll",
                "delta_x": delta_x,
                "delta_y": delta_y,
            }),
        )
        .into()),
        Err(error) => Ok(tool_error(&error).into()),
    }
    .map(|response| super::results::with_progress(response, progress))
}

/// A tool error for a request rejected before any desktop work, with not-started progress.
fn not_started(error: &PlatformError) -> CallToolResponse {
    super::results::with_progress(tool_error(error).into(), MutationProgress::default())
}

/// Runs validated steps in order on one target, stopping at the first failure. Each step uses
/// the same backend path, target checks, and cleanup as the matching single-action tool;
/// only the last step captures the requested observation.
pub(super) async fn call_run_actions(
    backend: Arc<dyn PlatformBackend>,
    arguments: Option<JsonObject>,
    operation_lease: Option<OperationLease>,
) -> Result<CallToolResponse, ErrorData> {
    let input = parse_arguments::<RunActionsInput>(arguments)?;
    let steps = match input.validated_steps() {
        Ok(steps) => steps,
        Err(error) => return Ok(not_started(&error)),
    };
    let final_observation: ObservationOptions = input.observation.into();
    let reports = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let worker_reports = Arc::clone(&reports);
    let (result, progress) = run_encoded_mutation(
        operation_lease,
        input.target_ref,
        Arc::clone(&backend),
        move |control| {
            run_steps(
                backend.as_ref(),
                &steps,
                &final_observation,
                control,
                &worker_reports,
            )
            .map(|observation| KeyboardActionResult { observation })
        },
    )
    .await;
    let steps = std::mem::take(
        &mut *reports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
    let action = json!({ "kind": "run_actions", "steps": steps });
    match result {
        Ok(result) => Ok(keyboard_result(result, &action).into()),
        Err(error) => {
            let mut response = tool_error(&error);
            if let Some(content) = response.structured_content.as_mut() {
                if content.get("error").is_some() {
                    content["error"]["details"]["steps"] = action["steps"].clone();
                } else {
                    // The completed-but-unverified variant carries a warning, not an error.
                    content["details"] = json!({ "steps": action["steps"] });
                }
            }
            Ok(response.into())
        }
    }
    .map(|response| super::results::with_progress(response, progress))
}

fn run_steps(
    backend: &dyn PlatformBackend,
    steps: &[ActionStep],
    final_observation: &ObservationOptions,
    control: &MutationControl,
    reports: &std::sync::Mutex<Vec<Value>>,
) -> Result<ActionObservation, PlatformError> {
    let report = |value: Value| {
        reports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(value);
    };
    // Resolve every image point before the first step sends input.
    let resolved = steps
        .iter()
        .map(|step| match step {
            ActionStep::Click { target, .. } | ActionStep::Scroll { target, .. } => {
                target.resolve(backend).map(Some)
            }
            ActionStep::TypeText { .. } | ActionStep::PressKeys { .. } => Ok(None),
        })
        .collect::<Result<Vec<_>, _>>()?;
    // Direct coordinates are only echoed by resolution, so check every point against the
    // current displays now; a bad later point must not strand input from earlier steps.
    let displays = backend.list_displays()?;
    for (index, point) in resolved.iter().enumerate() {
        let Some((display_id, x, y)) = point else {
            continue;
        };
        let display = displays
            .iter()
            .find(|display| display.id.eq_ignore_ascii_case(display_id))
            .ok_or_else(|| PlatformError::InvalidArgument {
                argument: format!("steps[{index}]"),
                reason: format!("display '{display_id}' is not active; call list_displays again"),
            })?;
        if *x >= display.bounds.width || *y >= display.bounds.height {
            return Err(PlatformError::InvalidArgument {
                argument: format!("steps[{index}]"),
                reason: format!(
                    "point ({x}, {y}) lies outside the {}x{} display",
                    display.bounds.width, display.bounds.height
                ),
            });
        }
    }
    let quiet = ObservationOptions {
        mode: ObservationMode::None,
        ..ObservationOptions::default()
    };
    let mut observation = None;
    for (index, (step, point)) in steps.iter().zip(resolved).enumerate() {
        let options = if index + 1 == steps.len() {
            final_observation.clone()
        } else {
            quiet.clone()
        };
        let sent_before = control.progress().sent_events;
        let outcome = control
            .check("run_actions")
            .and_then(|()| run_step(backend, step, point, options, control));
        let sent_events = control.progress().sent_events.saturating_sub(sent_before);
        match outcome {
            Ok(step_observation) => {
                report(
                    json!({ "index": index, "action": step.kind(), "status": "completed", "sent_events": sent_events }),
                );
                observation = Some(step_observation);
            }
            Err(error) => {
                let observation_failed =
                    matches!(error, PlatformError::PostActionObservationFailed { .. });
                if observation_failed && index + 1 == steps.len() {
                    // Every step's input was sent; only the final evidence is missing.
                    report(
                        json!({ "index": index, "action": step.kind(), "status": "completed", "sent_events": sent_events, "error": error.to_string() }),
                    );
                    return Err(error);
                }
                report(
                    json!({ "index": index, "action": step.kind(), "status": "failed", "sent_events": sent_events, "error": error.to_string() }),
                );
                for (skipped, step) in steps.iter().enumerate().skip(index + 1) {
                    report(
                        json!({ "index": skipped, "action": step.kind(), "status": "skipped", "sent_events": 0 }),
                    );
                }
                if index > 0 || observation_failed {
                    control.steps_incomplete();
                }
                // A non-final step that sent input but failed its post-action check still
                // stopped the batch, so report it as a failure, not as unverified completion.
                return Err(match error {
                    PlatformError::PostActionObservationFailed { operation, reason } => {
                        PlatformError::OperationFailed {
                            operation: "run_actions".to_owned(),
                            reason: format!(
                                "step {index} ({operation}) sent its input but its post-action check failed: {reason}; later steps were skipped"
                            ),
                        }
                    }
                    other => other,
                });
            }
        }
    }
    observation.ok_or_else(|| PlatformError::InvalidArgument {
        argument: "steps".to_owned(),
        reason: "at least one step is required".to_owned(),
    })
}

fn run_step(
    backend: &dyn PlatformBackend,
    step: &ActionStep,
    point: Option<(String, u32, u32)>,
    observation: ObservationOptions,
    control: &MutationControl,
) -> Result<ActionObservation, PlatformError> {
    let missing_point = || PlatformError::InvalidArgument {
        argument: "point".to_owned(),
        reason: "the step's point was not resolved".to_owned(),
    };
    match step {
        ActionStep::Click {
            button,
            click_count,
            modifiers,
            ..
        } => {
            let (display_id, x, y) = point.ok_or_else(missing_point)?;
            backend
                .click_mouse_controlled(
                    &MouseClickRequest {
                        display_id,
                        x,
                        y,
                        button: *button,
                        click_count: *click_count,
                        modifiers: modifiers.clone(),
                        duration_ms: 0,
                        observation,
                    },
                    control,
                )
                .map(|result| result.observation)
        }
        ActionStep::Scroll {
            delta_x, delta_y, ..
        } => {
            let (display_id, x, y) = point.ok_or_else(missing_point)?;
            backend
                .scroll_mouse_controlled(
                    &MouseScrollRequest {
                        display_id,
                        x,
                        y,
                        delta_x: *delta_x,
                        delta_y: *delta_y,
                        duration_ms: 0,
                        observation,
                    },
                    control,
                )
                .map(|result| result.observation)
        }
        ActionStep::TypeText { text } => backend
            .type_text_controlled(
                &TextInputRequest {
                    text: text.clone(),
                    observation,
                },
                control,
            )
            .map(|result| result.observation),
        ActionStep::PressKeys { keys } => backend
            .press_keys_controlled(
                &KeyChordRequest {
                    keys: keys.clone(),
                    observation,
                },
                control,
            )
            .map(|result| result.observation),
    }
}
