use super::*;

fn screenshot_metadata(screenshot: &DisplayScreenshot) -> Value {
    json!({
        "display": screenshot.display,
        "source_bounds": screenshot.source_bounds,
        "mime_type": "image/png",
        "byte_length": screenshot.png.len(),
        "capture_method": "windows_gdi",
        "image_width": screenshot.image_width,
        "image_height": screenshot.image_height,
        "downscale_factor": screenshot.downscale_factor,
        "resize_method": if screenshot.image_width == screenshot.source_bounds.width
            && screenshot.image_height == screenshot.source_bounds.height {
            "native"
        } else {
            "bilinear"
        },
        "cursor_marker": screenshot.cursor_marker,
    })
}

pub(super) fn visual_change_result(result: &VisualChangeResult) -> CallToolResult {
    let encoded = STANDARD.encode(&result.screenshot.png);
    image_result(
        encoded,
        json!({
            "changed": result.changed,
            "timed_out": result.timed_out,
            "difference": result.difference,
            "elapsed_ms": result.elapsed_ms,
            "screenshot": screenshot_metadata(&result.screenshot),
        }),
    )
}

pub(super) fn visual_baseline_result(result: &VisualBaseline) -> CallToolResult {
    CallToolResult::structured(json!({
        "baseline_id": result.baseline_id,
        "display": result.display,
        "source_bounds": result.source_bounds,
        "fingerprint": result.fingerprint,
        "expires_in_ms": result.expires_in_ms,
    }))
}

pub(super) fn window_screenshot_result(screenshot: &WindowScreenshot) -> CallToolResult {
    let encoded = STANDARD.encode(&screenshot.png);
    image_result(
        encoded,
        json!({
            "window": screenshot.window,
            "source_bounds": screenshot.source_bounds,
            "mime_type": "image/png",
            "byte_length": screenshot.png.len(),
            "capture_method": "windows_gdi",
            "image_width": screenshot.image_width,
            "image_height": screenshot.image_height,
            "downscale_factor": screenshot.downscale_factor,
            "resize_method": if screenshot.image_width == screenshot.source_bounds.width
                && screenshot.image_height == screenshot.source_bounds.height {
                "native"
            } else {
                "bilinear"
            },
            "cursor_marker": screenshot.cursor_marker,
        }),
    )
}

pub(super) fn window_wait_result(result: &WindowWaitResult) -> CallToolResult {
    CallToolResult::structured(json!({
        "matched": result.matched,
        "timed_out": result.timed_out,
        "elapsed_ms": result.elapsed_ms,
        "window": result.window,
    }))
}

pub(super) fn virtual_desktop_list_result(result: &VirtualDesktopList) -> CallToolResult {
    let desktops: Vec<Value> = result
        .desktops
        .iter()
        .map(|desktop| {
            let application_count = desktop
                .windows
                .iter()
                .map(|window| window.process_id)
                .collect::<std::collections::HashSet<_>>()
                .len();
            json!({
                "id": desktop.id,
                "is_current": desktop.is_current,
                "application_count": application_count,
                "windows": desktop.windows,
            })
        })
        .collect();
    CallToolResult::structured(json!({
        "count": desktops.len(),
        "desktops": desktops,
        "includes_empty_desktops": result.includes_empty_desktops,
        "order_available": result.order_available,
        "names_available": result.names_available,
    }))
}

pub(super) fn virtual_desktop_switch_result(result: &VirtualDesktopSwitchResult) -> CallToolResult {
    action_result(
        &result.observation,
        &json!({
            "kind": "switch_virtual_desktop",
            "direction": result.direction,
            "requested_steps": result.requested_steps,
            "changed": result.changed,
        }),
        None,
    )
}

pub(super) fn screenshot_result(screenshot: &DisplayScreenshot) -> CallToolResult {
    let encoded = STANDARD.encode(&screenshot.png);
    let metadata = screenshot_metadata(screenshot);
    image_result(encoded, metadata)
}

pub(super) fn pointer_result(result: &PointerActionResult, action: &Value) -> CallToolResult {
    action_result(&result.observation, action, Some(json!(result.position)))
}

pub(super) fn click_text_result(result: &ClickTextResult, action: &Value) -> CallToolResult {
    let mut action = action.clone();
    action["matched_text"] = json!(result.matched_text);
    action_result(&result.observation, &action, Some(json!(result.position)))
}

pub(super) fn window_focus_result(result: &WindowFocusResult) -> CallToolResult {
    let action = json!({ "kind": "focus_window", "window": result.window });
    action_result(&result.observation, &action, None)
}

pub(super) fn keyboard_result(result: &KeyboardActionResult, action: &Value) -> CallToolResult {
    action_result(&result.observation, action, None)
}

fn action_result(
    observation: &ActionObservation,
    action: &Value,
    position: Option<Value>,
) -> CallToolResult {
    let mut metadata = json!({
        "action": action,
        "observation": {
            "foreground_window": observation.foreground_window,
            "screenshot": observation.screenshot.as_ref().map(screenshot_metadata),
        },
    });
    if let Some(position) = position {
        metadata["position"] = position;
    }
    if let Some(screenshot) = &observation.screenshot {
        image_result(STANDARD.encode(&screenshot.png), metadata)
    } else {
        CallToolResult::structured(metadata)
    }
}

fn image_result(encoded: String, metadata: Value) -> CallToolResult {
    let mut tool_result = CallToolResult::success(vec![
        ContentBlock::image(encoded, "image/png"),
        ContentBlock::text(metadata.to_string()),
    ]);
    tool_result.structured_content = Some(metadata);
    tool_result
}

pub(super) fn tool_error(error: &PlatformError) -> CallToolResult {
    if let PlatformError::PostActionObservationFailed { operation, reason } = error {
        return CallToolResult::structured(json!({
            "status": "completed_unverified",
            "operation": operation,
            "warning": reason,
            "retry_action": false,
        }));
    }
    let code = match error {
        PlatformError::Unsupported { .. } => "unsupported_platform_operation",
        PlatformError::PermissionDenied { .. } => "permission_denied",
        PlatformError::HigherIntegrityTarget { .. } => "higher_integrity_target",
        PlatformError::TargetIntegrityUnavailable { .. } => "target_integrity_unavailable",
        PlatformError::InvalidArgument { .. } => "invalid_argument",
        PlatformError::PostActionObservationFailed { .. } => unreachable!(),
        PlatformError::OperationFailed { .. } | PlatformError::Unavailable { .. } => {
            "platform_operation_failed"
        }
    };
    CallToolResult::structured_error(json!({
        "error": {
            "code": code,
            "message": error.to_string(),
        }
    }))
}

pub(super) fn tool_execution_error(error: &ErrorData) -> CallToolResult {
    let code = if error.code == ErrorCode::INVALID_PARAMS {
        "invalid_arguments"
    } else {
        "tool_execution_failed"
    };
    CallToolResult::structured_error(json!({
        "error": {
            "code": code,
            "message": error.message,
            "details": error.data,
        }
    }))
}
