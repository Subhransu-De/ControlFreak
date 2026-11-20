use super::*;
use controlfreak_core::{OcrLine, OcrResult};
use serde::Serialize;

/// Results that may carry a PNG.
pub(super) trait CarriesPng {
    fn png(&self) -> Option<&[u8]>;
}

/// A result with its PNG already base64-encoded. Build it on a blocking worker: encoding a
/// large capture is CPU work that must not run on a Tokio worker.
pub(super) struct Encoded<T> {
    value: T,
    base64: Option<String>,
}

impl<T: CarriesPng> Encoded<T> {
    pub(super) fn new(value: T) -> Self {
        let base64 = value
            .png()
            .map(|png| controlfreak_core::timing::time("encode_base64", || STANDARD.encode(png)));
        Self { value, base64 }
    }
}

impl CarriesPng for DisplayScreenshot {
    fn png(&self) -> Option<&[u8]> {
        Some(&self.png)
    }
}

impl CarriesPng for WindowScreenshot {
    fn png(&self) -> Option<&[u8]> {
        Some(&self.png)
    }
}

impl CarriesPng for VisualChangeResult {
    fn png(&self) -> Option<&[u8]> {
        self.screenshot.png()
    }
}

impl CarriesPng for ActionObservation {
    fn png(&self) -> Option<&[u8]> {
        self.screenshot.as_ref().and_then(CarriesPng::png)
    }
}

macro_rules! observed_result {
    ($($result:ty),+) => {$(
        impl CarriesPng for $result {
            fn png(&self) -> Option<&[u8]> {
                self.observation.png()
            }
        }
    )+};
}

observed_result!(
    PointerActionResult,
    KeyboardActionResult,
    ClickTextResult,
    WindowFocusResult,
    VirtualDesktopSwitchResult
);

fn screenshot_metadata(screenshot: &DisplayScreenshot) -> Value {
    json!({
        "display": screenshot.display,
        "target_ref": screenshot.target_ref,
        "source_bounds": screenshot.source_bounds,
        "mime_type": "image/png",
        "byte_length": screenshot.png.len(),
        "capture_method": "windows_gdi",
        "image_width": screenshot.image_width,
        "image_height": screenshot.image_height,
        "cursor_marker": screenshot.cursor_marker,
        "observation_id": super::observations::register(
            screenshot.source_bounds,
            screenshot.image_width,
            screenshot.image_height,
            screenshot.display_layout.clone(),
            None,
        ),
    })
}

pub(super) fn visual_change_result(result: Encoded<VisualChangeResult>) -> CallToolResult {
    let Encoded {
        value: result,
        base64,
    } = result;
    image_result(
        base64,
        json!({
            "changed": result.changed(),
            "changed_once": result.changed_once,
            "currently_changed": result.currently_changed,
            "settled": result.settled,
            "timed_out": result.timed_out(),
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

pub(super) fn window_screenshot_result(screenshot: Encoded<WindowScreenshot>) -> CallToolResult {
    let Encoded {
        value: screenshot,
        base64,
    } = screenshot;
    image_result(
        base64,
        json!({
            "window": screenshot.window,
            "source_bounds": screenshot.source_bounds,
            "mime_type": "image/png",
            "byte_length": screenshot.png.len(),
            "capture_method": screenshot.capture_method,
            "image_width": screenshot.image_width,
            "image_height": screenshot.image_height,
            "cursor_marker": screenshot.cursor_marker,
            "observation_id": super::observations::register(
                controlfreak_core::DisplayBounds {
                    left: screenshot.source_bounds.left,
                    top: screenshot.source_bounds.top,
                    width: screenshot.source_bounds.width,
                    height: screenshot.source_bounds.height,
                },
                screenshot.image_width,
                screenshot.image_height,
                screenshot.display_layout.clone(),
                Some(super::observations::ShownWindow {
                    id: screenshot.window.id.clone(),
                    bounds: screenshot.window.bounds,
                }),
            ),
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
        "includes_empty_desktops": false,
        "order_available": false,
        "names_available": false,
        "truncated": result.truncated,
    }))
}

/// Which bound cut a `read_text_in_region` result short.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ReadTextLimit {
    Lines,
    Characters,
    Bytes,
}

/// How many leading lines fit every `read_text_in_region` bound, and the bound that stopped the
/// rest. Lines are kept whole, in reading order, so a client can read on below the last one.
fn read_text_budget(lines: &[OcrLine]) -> (usize, Option<ReadTextLimit>) {
    use controlfreak_core::limits::{
        MAX_READ_TEXT_BYTES, MAX_READ_TEXT_CHARS, MAX_READ_TEXT_LINES,
    };
    let mut characters = 0_usize;
    // The brackets of the `lines` array.
    let mut bytes = 2_usize;
    for (index, line) in lines.iter().enumerate() {
        if index == MAX_READ_TEXT_LINES {
            return (index, Some(ReadTextLimit::Lines));
        }
        characters += line.text.chars().count();
        if characters > MAX_READ_TEXT_CHARS {
            return (index, Some(ReadTextLimit::Characters));
        }
        // The comma before every line but the first. A line that cannot be measured is too big.
        let line_bytes = serde_json::to_vec(line).map_or(usize::MAX, |json| json.len());
        bytes = bytes
            .saturating_add(line_bytes)
            .saturating_add(usize::from(index > 0));
        if bytes > MAX_READ_TEXT_BYTES {
            return (index, Some(ReadTextLimit::Bytes));
        }
    }
    (lines.len(), None)
}

/// The recognized text up to the end of the last kept line. The engine's text separates lines
/// its own way, so the cut is found in it; if a kept line is not found there in order, the kept
/// lines are joined with line breaks instead.
fn text_through(text: &str, kept: &[OcrLine]) -> String {
    let mut end = 0;
    for line in kept {
        let Some(offset) = text[end..].find(line.text.as_str()) else {
            return kept
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>()
                .join("\n");
        };
        end += offset + line.text.len();
    }
    text[..end].to_owned()
}

/// A `read_text_in_region` result bounded by lines, characters and serialized bytes. When a
/// bound is hit, `text` and `lines` both stop at the same whole line.
pub(super) fn read_text_result(result: OcrResult) -> CallToolResult {
    let OcrResult {
        target_ref,
        display,
        source_bounds,
        language,
        mut text,
        mut lines,
    } = result;
    let (kept, limit) = read_text_budget(&lines);
    if limit.is_some() {
        lines.truncate(kept);
        text = text_through(&text, &lines);
    }
    CallToolResult::structured(json!({
        "target_ref": target_ref,
        "display": display,
        "source_bounds": source_bounds,
        "language": language,
        "text": text,
        "lines": lines,
        "truncated": limit.is_some(),
        "limit": limit,
    }))
}

pub(super) fn virtual_desktop_switch_result(
    result: Encoded<VirtualDesktopSwitchResult>,
) -> CallToolResult {
    let Encoded {
        value: result,
        base64,
    } = result;
    action_result(
        &result.observation,
        base64,
        &json!({
            "kind": "switch_virtual_desktop",
            "direction": result.direction,
            "requested_steps": result.requested_steps,
            "changed": result.changed,
            "before_id": result.before_id,
            "after_id": result.after_id,
            "transition": result.transition,
        }),
        None,
    )
}

pub(super) fn screenshot_result(screenshot: Encoded<DisplayScreenshot>) -> CallToolResult {
    let Encoded {
        value: screenshot,
        base64,
    } = screenshot;
    let metadata = screenshot_metadata(&screenshot);
    image_result(base64, metadata)
}

pub(super) fn pointer_result(
    result: Encoded<PointerActionResult>,
    action: &Value,
) -> CallToolResult {
    let Encoded {
        value: result,
        base64,
    } = result;
    action_result(
        &result.observation,
        base64,
        action,
        Some(json!(result.position)),
    )
}

pub(super) fn click_text_result(
    result: Encoded<ClickTextResult>,
    action: &Value,
) -> CallToolResult {
    let Encoded {
        value: result,
        base64,
    } = result;
    let mut action = action.clone();
    action["matched_text"] = json!(result.matched_text);
    action["match_tier"] = json!(if action["exact_match"] == true {
        "exact"
    } else {
        "substring"
    });
    action_result(
        &result.observation,
        base64,
        &action,
        Some(json!(result.position)),
    )
}

pub(super) fn window_focus_result(result: Encoded<WindowFocusResult>) -> CallToolResult {
    let Encoded {
        value: result,
        base64,
    } = result;
    let action = json!({ "kind": "focus_window", "window": result.window, "attempts": result.attempts, "elapsed_ms": result.elapsed_ms });
    action_result(&result.observation, base64, &action, None)
}

pub(super) fn keyboard_result(
    result: Encoded<KeyboardActionResult>,
    action: &Value,
) -> CallToolResult {
    let Encoded {
        value: result,
        base64,
    } = result;
    action_result(&result.observation, base64, action, None)
}

fn action_result(
    observation: &ActionObservation,
    encoded: Option<String>,
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
    if let Some(settle) = observation.settle {
        metadata["observation"]["settle"] = json!(settle);
    }
    if let Some(position) = position {
        metadata["position"] = position;
    }
    image_result(encoded, metadata)
}

/// Builds the result; `encoded` is `None` only when the result has no image.
fn image_result(encoded: Option<String>, metadata: Value) -> CallToolResult {
    let Some(encoded) = encoded else {
        return CallToolResult::structured(metadata);
    };
    let mut tool_result = CallToolResult::success(vec![
        ContentBlock::image(encoded, "image/png"),
        ContentBlock::text(metadata.to_string()),
    ]);
    tool_result.structured_content = Some(metadata);
    tool_result
}

pub(super) fn tool_error(error: &PlatformError) -> CallToolResult {
    let code = match error {
        PlatformError::PostActionObservationFailed { operation, reason } => {
            return CallToolResult::structured(json!({
                "status": "completed_unverified",
                "operation": operation,
                "warning": reason,
                "retry_action": false,
            }));
        }
        PlatformError::TargetInvalidated { .. } | PlatformError::TargetRefused { .. } => {
            "target_invalidated"
        }
        PlatformError::ActivationFailed { reason, .. } => reason.as_str(),
        PlatformError::OcrNoMatch { .. } => "ocr_no_match",
        PlatformError::OcrAmbiguousMatch { .. } => "ocr_ambiguous_match",
        PlatformError::Unsupported { .. } => "unsupported_platform_operation",
        PlatformError::PermissionDenied { .. } => "permission_denied",
        PlatformError::HigherIntegrityTarget { .. } => "higher_integrity_target",
        PlatformError::TargetIntegrityUnavailable { .. } => "target_integrity_unavailable",
        PlatformError::InvalidArgument { .. } => "invalid_argument",
        PlatformError::OperationFailed { .. }
        | PlatformError::Failed { .. }
        | PlatformError::Unavailable { .. } => "platform_operation_failed",
    };
    let mut value = json!({"error": {
        "code": code,
        "message": error.to_string(),
        "retryable": error.retryable(),
    }});
    if let Some(reason) = error.reason_code() {
        value["error"]["reason"] = json!(reason);
    }
    if let PlatformError::ActivationFailed {
        attempts,
        elapsed_ms,
        ..
    } = error
    {
        value["error"]["details"] = json!({"attempts": attempts, "elapsed_ms": elapsed_ms});
    }
    if let PlatformError::OcrNoMatch { details } | PlatformError::OcrAmbiguousMatch { details } =
        error
    {
        value["error"]["details"] = json!(details);
    }
    CallToolResult::structured_error(value)
}

pub(super) fn with_progress(
    response: super::CallToolResponse,
    mut progress: controlfreak_core::MutationProgress,
) -> super::CallToolResponse {
    use controlfreak_core::InputOutcome;
    let super::CallToolResponse::Complete(mut result) = response else {
        return response;
    };
    let Some(metadata) = result.structured_content.as_mut() else {
        return super::CallToolResponse::Complete(result);
    };
    let observation_failed = metadata["status"] == "completed_unverified";
    if result.is_error != Some(true) || observation_failed {
        progress.input_outcome = InputOutcome::InputSent;
    }
    let status = match progress.input_outcome {
        InputOutcome::NotStarted => "not_started",
        InputOutcome::PartiallySent => "partially_sent",
        InputOutcome::Unknown => "unknown",
        InputOutcome::InputSent if result.is_error == Some(true) || observation_failed => {
            "completed_unverified"
        }
        InputOutcome::InputSent => "completed",
    };
    metadata["status"] = json!(status);
    metadata["input"] = json!(progress);
    metadata["observation_status"] = json!(if status == "completed" {
        "succeeded"
    } else if status == "completed_unverified" {
        "failed"
    } else {
        "not_attempted"
    });
    // Even a refusal needs a new decision, not an automatic retry loop. A completed action has
    // nothing to retry, so only the other statuses carry the guidance.
    if status == "completed" {
        if let Some(metadata) = metadata.as_object_mut() {
            metadata.remove("retry_action");
        }
    } else {
        metadata["retry_action"] = json!(false);
    }
    if progress.input_outcome != InputOutcome::NotStarted {
        result.is_error = Some(false);
    }
    // Keep the text fallback and structured content identical, retaining any image blocks.
    result
        .content
        .retain(|block| !matches!(block, ContentBlock::Text(_)));
    result
        .content
        .push(ContentBlock::text(metadata.to_string()));
    super::CallToolResponse::Complete(result)
}

/// Whether an input tool's response says any input was sent, fully or in part.
pub(super) fn input_was_sent(response: &super::CallToolResponse) -> bool {
    let super::CallToolResponse::Complete(result) = response else {
        return false;
    };
    result
        .structured_content
        .as_ref()
        .and_then(|content| content["status"].as_str())
        .is_some_and(|status| status != "not_started")
}

pub(super) fn tool_execution_error(error: &ErrorData) -> CallToolResult {
    let code = if error.code == ErrorCode::INVALID_PARAMS {
        "invalid_arguments"
    } else {
        "tool_execution_failed"
    };
    // A detail that says whether repeating helps (the indicator's `starting` or `failed` states
    // are transient) is the envelope's answer too, so the two never disagree.
    let retryable = error
        .data
        .as_ref()
        .and_then(|data| data["retryable"].as_bool())
        .unwrap_or(false);
    CallToolResult::structured_error(json!({
        "error": {
            "code": code,
            "message": error.message,
            "retryable": retryable,
            "details": error.data,
        }
    }))
}
