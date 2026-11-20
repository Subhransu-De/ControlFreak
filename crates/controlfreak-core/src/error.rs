use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::IntegrityLevel;

#[derive(Debug, Error, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlatformError {
    /// The target changed identity, display layout or desktop, or the session is poisoned. The
    /// session cannot continue; end it and observe again.
    #[error("approved target invalidated: {reason}")]
    TargetInvalidated { reason: String },
    /// This action was refused, but the target is unchanged and the session keeps it. The next
    /// action checks again, so the agent can recover (for example with `focus_window`).
    #[error("approved target cannot take this action right now: {reason}")]
    TargetRefused { cause: RefusalCause, reason: String },
    #[error(
        "foreground activation failed: {reason} after {attempts} attempts and {elapsed_ms} ms; Windows can refuse focus changes requested by a background process, so ask the user to click the target window, then try again"
    )]
    ActivationFailed {
        reason: String,
        attempts: u32,
        elapsed_ms: u64,
    },
    #[error("no OCR line matched; refine the query or change the match mode")]
    OcrNoMatch { details: crate::OcrMatchDetails },
    #[error("multiple OCR lines matched; narrow the region or refine the query")]
    OcrAmbiguousMatch { details: crate::OcrMatchDetails },
    #[error("operation '{operation}' is unsupported: {reason}")]
    Unsupported { operation: String, reason: String },
    #[error("permission '{permission}' was denied")]
    PermissionDenied { permission: String },
    #[error(
        "operation '{operation}' refused input into process {process_id} because its integrity level ({target_integrity}) is higher than ControlFreak ({server_integrity})"
    )]
    HigherIntegrityTarget {
        operation: String,
        process_id: u32,
        server_integrity: IntegrityLevel,
        target_integrity: IntegrityLevel,
    },
    #[error(
        "operation '{operation}' refused input because the target process integrity could not be verified: {reason}"
    )]
    TargetIntegrityUnavailable { operation: String, reason: String },
    #[error("invalid argument '{argument}': {reason}")]
    InvalidArgument { argument: String, reason: String },
    /// A `baseline_id` that cannot be compared against. It has the wire code of
    /// `InvalidArgument`, with the cause as its reason.
    #[error("invalid argument 'baseline_id': {}", cause.message())]
    InvalidBaseline { cause: BaselineFailure },
    #[error("operation '{operation}' failed: {reason}")]
    OperationFailed { operation: String, reason: String },
    /// A failure the agent can act on, with its cause as data rather than inferred from text.
    /// It has the same wire code as `OperationFailed`.
    #[error("operation '{operation}' failed: {reason}")]
    Failed {
        cause: FailureCause,
        operation: String,
        reason: String,
    },
    #[error(
        "operation '{operation}' completed, but post-action observation failed: {reason}; do not repeat the action solely because its screenshot failed"
    )]
    PostActionObservationFailed { operation: String, reason: String },
    #[error("platform backend is unavailable: {reason}")]
    Unavailable { reason: String },
}

/// Why an action was refused while the session's target stayed valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalCause {
    /// The target (or a window of its process) is not in the foreground. `focus_window` fixes it.
    NotForeground,
    /// The foreground changed while the action was being validated.
    ForegroundChanged,
    /// The screen read for a text click was taken while the foreground or the window set changed,
    /// so the text may belong to another window. Reading and clicking again is safe.
    ObservationStale,
    /// The point or keyboard focus is in a window of another process.
    ForeignDestination,
    /// The point is under the server's own indicator or Stop button.
    OwnIndicator,
    /// The window that would receive the input cannot be identified.
    DestinationUnidentified,
    /// The approved window is the same window, but it moved or was resized since the last check.
    /// The session keeps it with its new bounds; observe again so that coordinates match where
    /// it is now, then act.
    TargetMoved,
}

impl RefusalCause {
    #[must_use]
    const fn code(self) -> &'static str {
        match self {
            Self::NotForeground => "not_foreground",
            Self::ForegroundChanged => "foreground_changed",
            Self::ObservationStale => "observation_stale",
            Self::ForeignDestination => "foreign_destination",
            Self::OwnIndicator => "own_indicator",
            Self::DestinationUnidentified => "destination_unidentified",
            Self::TargetMoved => "target_moved",
        }
    }

    /// Whether the same request can succeed after the recovery its message names. Pointing at
    /// The server's own Stop button is never retryable: the request itself must change.
    #[must_use]
    const fn retryable(self) -> bool {
        !matches!(self, Self::OwnIndicator)
    }
}

/// Why a visual baseline cannot be used. Every cause needs a new `capture_visual_baseline`, but
/// the cause tells the agent what went wrong: waiting too long, capturing too many, a changed
/// display, or an ID this server never issued.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BaselineFailure {
    /// The baseline reached the end of its lifetime and its pixels were removed.
    Expired,
    /// The store was full, so a newer baseline replaced it before its lifetime ended.
    Evicted,
    /// This server instance did not issue the ID, or no longer remembers it. A restarted server
    /// knows no ID from before the restart.
    Unknown,
    /// The baseline's display moved, resized or disappeared since capture.
    DisplayChanged,
}

impl BaselineFailure {
    #[must_use]
    const fn code(self) -> &'static str {
        match self {
            Self::Expired => "expired",
            Self::Evicted => "evicted",
            Self::Unknown => "unknown",
            Self::DisplayChanged => "display_changed",
        }
    }

    const fn message(self) -> &'static str {
        match self {
            Self::Expired => "the baseline expired; capture a new baseline",
            Self::Evicted => {
                "newer baselines replaced this one because the store was full; capture a new baseline"
            }
            Self::Unknown => {
                "this server did not issue the baseline or no longer remembers it, for example after a restart; capture a new baseline"
            }
            Self::DisplayChanged => {
                "the display layout changed since this baseline was captured; capture a new baseline"
            }
        }
    }
}

/// The actionable causes of a `platform_operation_failed` result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureCause {
    Cancelled,
    PointerMoved,
    /// Windows left the pointer somewhere other than the requested point, usually because
    /// another program confines it with `ClipCursor`. Repeating the same request fails the same
    /// way, so it is not retryable.
    PointerNotPlaced,
    KeyHeld,
    InputInterleaved,
    OcrUnavailable,
    DesktopWorkStopped,
    WorkerFailed,
    InputBlocked,
    /// The window with keyboard focus stopped answering while text was typed, so typing stopped
    /// instead of queueing keystrokes the application could mistranslate.
    TargetNotResponding,
    /// Typing reached the request's `timeout_ms` before the text was finished, so it stopped
    /// between two keystrokes. Part of the text may have been sent, so repeating it is unsafe.
    DeadlineReached,
    /// The server process is not per-monitor DPI aware, so Windows would virtualize window and
    /// pointer coordinates on a scaled display. Process awareness is fixed for its lifetime.
    DpiAwarenessUnsupported,
}

impl FailureCause {
    #[must_use]
    const fn code(self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::PointerMoved => "pointer_moved",
            Self::PointerNotPlaced => "pointer_not_placed",
            Self::KeyHeld => "key_held",
            Self::InputInterleaved => "input_interleaved",
            Self::OcrUnavailable => "ocr_unavailable",
            Self::DesktopWorkStopped => "desktop_work_stopped",
            Self::WorkerFailed => "worker_failed",
            Self::InputBlocked => "input_blocked",
            Self::TargetNotResponding => "target_not_responding",
            Self::DeadlineReached => "deadline_reached",
            Self::DpiAwarenessUnsupported => "dpi_awareness_unsupported",
        }
    }

    #[must_use]
    const fn retryable(self) -> bool {
        matches!(
            self,
            Self::PointerMoved | Self::KeyHeld | Self::InputInterleaved
        )
    }
}

impl PlatformError {
    /// An actionable failure; see [`FailureCause`].
    pub fn failed(
        cause: FailureCause,
        operation: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self::Failed {
            cause,
            operation: operation.into(),
            reason: reason.into(),
        }
    }

    /// A refusal that leaves the session's target valid; see [`RefusalCause`].
    pub fn refused(cause: RefusalCause, reason: impl Into<String>) -> Self {
        Self::TargetRefused {
            cause,
            reason: reason.into(),
        }
    }

    /// Whether repeating the same request can succeed once the stated recovery is done.
    ///
    /// This is advice about the failure, not permission to repeat input: a result that carries
    /// `retry_action=false` always wins over `retryable`.
    #[must_use]
    pub fn retryable(&self) -> bool {
        match self {
            Self::ActivationFailed { .. } => true,
            Self::TargetRefused { cause, .. } => cause.retryable(),
            Self::Failed { cause, .. } => cause.retryable(),
            _ => false,
        }
    }

    /// A stable sub-code for the errors whose wire code does not say enough: the causes of
    /// `platform_operation_failed`, of `target_invalidated` and of a refused `baseline_id`. Other errors have their own wire
    /// code. The code comes from data set where the error is built, never from its message.
    #[must_use]
    pub fn reason_code(&self) -> Option<&'static str> {
        match self {
            Self::Unavailable { .. } => Some("backend_unavailable"),
            Self::OperationFailed { .. } => Some("os_call_failed"),
            Self::Failed { cause, .. } => Some(cause.code()),
            Self::TargetRefused { cause, .. } => Some(cause.code()),
            Self::TargetInvalidated { .. } => Some("target_changed"),
            Self::InvalidBaseline { cause } => Some(cause.code()),
            _ => None,
        }
    }

    pub(crate) fn unsupported(operation: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Unsupported {
            operation: operation.into(),
            reason: reason.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BaselineFailure, FailureCause, PlatformError, RefusalCause};
    use crate::MutationControl;

    fn failed(operation: &str, reason: &str) -> PlatformError {
        PlatformError::OperationFailed {
            operation: operation.to_owned(),
            reason: reason.to_owned(),
        }
    }

    #[test]
    fn a_pointer_windows_did_not_place_is_its_own_non_retryable_reason() {
        let error = PlatformError::failed(FailureCause::PointerNotPlaced, "click_mouse", "x");
        assert_eq!(error.reason_code(), Some("pointer_not_placed"));
        assert!(!error.retryable(), "the same request is clamped again");
    }

    #[test]
    fn a_reached_typing_deadline_is_its_own_non_retryable_reason() {
        let error = PlatformError::failed(FailureCause::DeadlineReached, "type_text", "x");
        assert_eq!(error.reason_code(), Some("deadline_reached"));
        assert!(!error.retryable(), "part of the text may have been sent");
    }

    #[test]
    fn a_server_without_per_monitor_dpi_awareness_has_its_own_non_retryable_reason() {
        let error = PlatformError::failed(FailureCause::DpiAwarenessUnsupported, "op", "x");
        assert_eq!(error.reason_code(), Some("dpi_awareness_unsupported"));
        assert!(!error.retryable(), "process awareness cannot change");
    }

    #[test]
    fn shared_wire_code_failures_get_distinct_reasons() {
        let control = MutationControl::default();
        control.cancel();
        let cancelled = control.check("op").unwrap_err();
        // Wording never changes the reason: only the typed cause does.
        let cases = [
            (cancelled, "cancelled", false),
            (
                PlatformError::failed(FailureCause::PointerMoved, "op", "x"),
                "pointer_moved",
                true,
            ),
            (
                PlatformError::failed(FailureCause::KeyHeld, "op", "x"),
                "key_held",
                true,
            ),
            (
                PlatformError::failed(FailureCause::InputInterleaved, "op", "x"),
                "input_interleaved",
                true,
            ),
            (
                PlatformError::failed(FailureCause::OcrUnavailable, "windows_ocr_helper", "x"),
                "ocr_unavailable",
                false,
            ),
            (
                PlatformError::failed(FailureCause::DesktopWorkStopped, "admission", "x"),
                "desktop_work_stopped",
                false,
            ),
            (
                PlatformError::failed(FailureCause::WorkerFailed, "mutation_worker", "x"),
                "worker_failed",
                false,
            ),
            (
                PlatformError::failed(FailureCause::InputBlocked, "SendInput", "x"),
                "input_blocked",
                false,
            ),
            (
                PlatformError::failed(FailureCause::TargetNotResponding, "type_text", "x"),
                "target_not_responding",
                false,
            ),
            (
                failed("op", "the pointer moved after target validation"),
                "os_call_failed",
                false,
            ),
            (failed("BitBlt", "access denied"), "os_call_failed", false),
            (
                PlatformError::Unavailable {
                    reason: "no backend".to_owned(),
                },
                "backend_unavailable",
                false,
            ),
            (
                PlatformError::TargetInvalidated {
                    reason: "gone".to_owned(),
                },
                "target_changed",
                false,
            ),
        ];
        for (error, reason, retryable) in cases {
            assert_eq!(error.reason_code(), Some(reason), "{error}");
            assert_eq!(error.retryable(), retryable, "{error}");
        }
    }

    #[test]
    fn every_refusal_cause_has_its_own_reason() {
        for (cause, reason, retryable) in [
            (RefusalCause::NotForeground, "not_foreground", true),
            (RefusalCause::ForegroundChanged, "foreground_changed", true),
            (RefusalCause::ObservationStale, "observation_stale", true),
            (
                RefusalCause::ForeignDestination,
                "foreign_destination",
                true,
            ),
            (
                RefusalCause::DestinationUnidentified,
                "destination_unidentified",
                true,
            ),
            (RefusalCause::OwnIndicator, "own_indicator", false),
            (RefusalCause::TargetMoved, "target_moved", true),
        ] {
            let error = PlatformError::refused(cause, "x");
            assert_eq!(error.reason_code(), Some(reason), "{error}");
            assert_eq!(error.retryable(), retryable, "{error}");
        }
    }

    #[test]
    fn every_baseline_failure_has_its_own_reason_and_names_the_argument() {
        for (cause, reason) in [
            (BaselineFailure::Expired, "expired"),
            (BaselineFailure::Evicted, "evicted"),
            (BaselineFailure::Unknown, "unknown"),
            (BaselineFailure::DisplayChanged, "display_changed"),
        ] {
            let error = PlatformError::InvalidBaseline { cause };
            assert_eq!(error.reason_code(), Some(reason), "{error}");
            assert!(!error.retryable(), "the same ID fails again: {error}");
            let message = error.to_string();
            assert!(message.starts_with("invalid argument 'baseline_id': "));
            assert!(message.ends_with("capture a new baseline"));
        }
    }

    #[test]
    fn other_errors_have_no_reason_and_activation_is_retryable() {
        let invalid = PlatformError::InvalidArgument {
            argument: "x".to_owned(),
            reason: "bad".to_owned(),
        };
        assert_eq!(invalid.reason_code(), None);
        assert!(!invalid.retryable());
        let activation = PlatformError::ActivationFailed {
            reason: "foreground_activation_failed".to_owned(),
            attempts: 3,
            elapsed_ms: 400,
        };
        assert!(activation.retryable());
        assert!(activation.to_string().contains("click the target window"));
    }
}
