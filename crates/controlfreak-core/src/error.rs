use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::IntegrityLevel;

#[derive(Debug, Error, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlatformError {
    #[error("approved target invalidated: {reason}")]
    TargetInvalidated { reason: String },
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
    #[error(
        "operation '{operation}' completed, but post-action observation failed: {reason}; do not repeat the action solely because its screenshot failed"
    )]
    PostActionObservationFailed { operation: String, reason: String },
    #[error("platform backend is unavailable: {reason}")]
    Unavailable { reason: String },
}

impl PlatformError {
    /// Whether repeating the same request can succeed once the stated recovery is done.
    ///
    /// This is advice about the failure, not permission to repeat input: a result that carries
    /// `retry_action=false` always wins over `retryable`.
    #[must_use]
    pub fn retryable(&self) -> bool {
        match self {
            Self::ActivationFailed { .. } => true,
            Self::OperationFailed { .. } => matches!(
                self.reason_code(),
                Some("pointer_moved" | "key_held" | "input_interleaved")
            ),
            _ => false,
        }
    }

    /// A stable sub-code that tells apart the failures that share the wire code
    /// `platform_operation_failed`. Other errors already have their own wire code.
    ///
    /// Platform failures are free text, so this matches the phrases the backends use for the
    /// failures an agent can act on. Each phrase is covered by a test, and an unmatched failure
    /// reports `os_call_failed`.
    #[must_use]
    pub fn reason_code(&self) -> Option<&'static str> {
        match self {
            Self::Unavailable { .. } => Some("backend_unavailable"),
            Self::OperationFailed { operation, reason } => {
                Some(if reason.contains("was cancelled") {
                    "cancelled"
                } else if reason.contains("pointer moved") {
                    "pointer_moved"
                } else if reason.contains("already held") {
                    "key_held"
                } else if reason.contains("another input action ran") {
                    "input_interleaved"
                } else if operation == "windows_ocr_helper" {
                    "ocr_unavailable"
                } else if operation == "admission" {
                    "desktop_work_stopped"
                } else if operation == "mutation_worker" && reason.contains("worker failed") {
                    "worker_failed"
                } else if reason.contains("SendInput inserted") {
                    "input_blocked"
                } else {
                    "os_call_failed"
                })
            }
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
    use super::PlatformError;
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
        let cases = [
            (cancelled, "cancelled", false),
            (
                failed(
                    "op",
                    "the pointer moved after target validation; observe again",
                ),
                "pointer_moved",
                true,
            ),
            (
                failed(
                    "op",
                    "a requested key or mouse button is already held; release it",
                ),
                "key_held",
                true,
            ),
            (
                failed("op", "a mouse button is already held; refusing to move"),
                "key_held",
                true,
            ),
            (
                failed("op", "another input action ran during text recognition"),
                "input_interleaved",
                true,
            ),
            (
                failed("windows_ocr_helper", "the helper exited"),
                "ocr_unavailable",
                false,
            ),
            (
                failed("admission", "desktop work is stopped"),
                "desktop_work_stopped",
                false,
            ),
            (
                failed("mutation_worker", "mutation worker failed; observe"),
                "worker_failed",
                false,
            ),
            (
                failed("SendInput", "SendInput inserted 1 of 4 events"),
                "input_blocked",
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
