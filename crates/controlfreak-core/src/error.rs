use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::IntegrityLevel;

#[derive(Debug, Error, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlatformError {
    /// The target changed identity, geometry or desktop, or the session is poisoned. The
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
}

impl RefusalCause {
    #[must_use]
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::NotForeground => "not_foreground",
            Self::ForegroundChanged => "foreground_changed",
            Self::ObservationStale => "observation_stale",
            Self::ForeignDestination => "foreign_destination",
            Self::OwnIndicator => "own_indicator",
            Self::DestinationUnidentified => "destination_unidentified",
        }
    }

    /// Whether the same request can succeed after the recovery its message names. Pointing at
    /// The server's own Stop button is never retryable: the request itself must change.
    #[must_use]
    pub(crate) const fn retryable(self) -> bool {
        !matches!(self, Self::OwnIndicator)
    }
}

/// The actionable causes of a `platform_operation_failed` result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureCause {
    Cancelled,
    PointerMoved,
    KeyHeld,
    InputInterleaved,
    OcrUnavailable,
    DesktopWorkStopped,
    WorkerFailed,
    InputBlocked,
}

impl FailureCause {
    #[must_use]
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::PointerMoved => "pointer_moved",
            Self::KeyHeld => "key_held",
            Self::InputInterleaved => "input_interleaved",
            Self::OcrUnavailable => "ocr_unavailable",
            Self::DesktopWorkStopped => "desktop_work_stopped",
            Self::WorkerFailed => "worker_failed",
            Self::InputBlocked => "input_blocked",
        }
    }

    #[must_use]
    pub(crate) const fn retryable(self) -> bool {
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
    /// `platform_operation_failed` and of `target_invalidated`. Other errors have their own wire
    /// code. The code comes from data set where the error is built, never from its message.
    #[must_use]
    pub fn reason_code(&self) -> Option<&'static str> {
        match self {
            Self::Unavailable { .. } => Some("backend_unavailable"),
            Self::OperationFailed { .. } => Some("os_call_failed"),
            Self::Failed { cause, .. } => Some(cause.code()),
            Self::TargetRefused { cause, .. } => Some(cause.code()),
            Self::TargetInvalidated { .. } => Some("target_changed"),
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
    use super::{FailureCause, PlatformError, RefusalCause};
    use crate::MutationControl;

    fn failed(operation: &str, reason: &str) -> PlatformError {
        PlatformError::OperationFailed {
            operation: operation.to_owned(),
            reason: reason.to_owned(),
        }
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
            (
                PlatformError::refused(RefusalCause::NotForeground, "x"),
                "not_foreground",
                true,
            ),
            (
                PlatformError::refused(RefusalCause::ForegroundChanged, "x"),
                "foreground_changed",
                true,
            ),
            (
                PlatformError::refused(RefusalCause::ObservationStale, "x"),
                "observation_stale",
                true,
            ),
            (
                PlatformError::refused(RefusalCause::ForeignDestination, "x"),
                "foreign_destination",
                true,
            ),
            (
                PlatformError::refused(RefusalCause::DestinationUnidentified, "x"),
                "destination_unidentified",
                true,
            ),
            (
                PlatformError::refused(RefusalCause::OwnIndicator, "x"),
                "own_indicator",
                false,
            ),
        ];
        for (error, reason, retryable) in cases {
            assert_eq!(error.reason_code(), Some(reason), "{error}");
            assert_eq!(error.retryable(), retryable, "{error}");
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
