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
}
