use serde::{Deserialize, Serialize};

/// Dispatch describes delivery, never whether the application achieved the requested effect.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputOutcome {
    #[default]
    NotStarted,
    InputSent,
    PartiallySent,
    Unknown,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupStatus {
    #[default]
    NotNeeded,
    Succeeded,
    Unknown,
}

/// What the short-lived activation helper did during `focus_window`. Windows refuses
/// `SetForegroundWindow` from a background process unless one of the conditions Microsoft
/// documents holds, so a long-running server is usually refused once the user has worked in
/// another application. The helper is a fresh process that asks once.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivationHelperOutcome {
    /// The server's own request was accepted, or the helper is not configured.
    #[default]
    NotUsed,
    /// The server's own request was refused and the helper's was accepted.
    Accepted,
    /// The server's own request and the helper's were both refused.
    Refused,
    /// The helper could not start, did not finish in time, or found the window changed.
    Failed,
}

impl ActivationHelperOutcome {
    #[allow(
        clippy::trivially_copy_pass_by_ref,
        reason = "serde's skip_serializing_if passes the field by reference"
    )]
    fn is_not_used(&self) -> bool {
        *self == Self::NotUsed
    }
}

/// Known cumulative input progress for one operation. Counts exclude cleanup events and
/// cursor/window API calls. An event is not a character or proof of an application effect.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutationProgress {
    pub input_outcome: InputOutcome,
    pub sent_events: u64,
    pub cleanup: CleanupStatus,
    pub(crate) target_remained_foreground: Option<bool>,
    pub(crate) activation_attempts: u32,
    pub(crate) activation_elapsed_ms: u64,
    #[serde(default, skip_serializing_if = "ActivationHelperOutcome::is_not_used")]
    pub(crate) activation_helper: ActivationHelperOutcome,
}
