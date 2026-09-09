use std::fmt;

use serde::{Deserialize, Serialize};

/// Windows mandatory integrity level associated with a process token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrityLevel {
    Untrusted,
    Low,
    Medium,
    MediumPlus,
    High,
    System,
    Protected,
    Unknown,
}

impl fmt::Display for IntegrityLevel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Untrusted => "untrusted",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::MediumPlus => "medium_plus",
            Self::High => "high",
            Self::System => "system",
            Self::Protected => "protected",
            Self::Unknown => "unknown",
        };
        formatter.write_str(value)
    }
}

/// Privilege state inherited by the `ControlFreak` server process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecurityContext {
    pub elevated: bool,
    pub windows_integrity_level: IntegrityLevel,
    pub elevated_operation_allowed: bool,
}

impl Default for SecurityContext {
    fn default() -> Self {
        Self {
            elevated: false,
            windows_integrity_level: IntegrityLevel::Unknown,
            elevated_operation_allowed: false,
        }
    }
}
