use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::IntegrityLevel;

#[derive(Debug, Error, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlatformError {
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
    pub(crate) fn unsupported(operation: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Unsupported {
            operation: operation.into(),
            reason: reason.into(),
        }
    }
}
