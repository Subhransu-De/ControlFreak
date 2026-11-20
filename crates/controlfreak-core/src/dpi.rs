use std::fmt;

use serde::{Deserialize, Serialize};

/// How Windows scales coordinates for the server process. Tool coordinates are physical pixels,
/// which window, pointer and display APIs return only to a per-monitor aware process; for an
/// unaware or system-aware process Windows virtualizes some of them on a scaled display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DpiAwareness {
    PerMonitorV2,
    /// Per-monitor v1. Coordinates are physical as with v2; only non-client scaling differs.
    PerMonitor,
    SystemAware,
    Unaware,
    /// Windows reported no awareness the server recognizes.
    Unknown,
}

impl DpiAwareness {
    /// Whether coordinates are physical pixels on every display.
    #[must_use]
    pub const fn is_per_monitor(self) -> bool {
        matches!(self, Self::PerMonitorV2 | Self::PerMonitor)
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::PerMonitorV2 => "per_monitor_v2",
            Self::PerMonitor => "per_monitor",
            Self::SystemAware => "system_aware",
            Self::Unaware => "unaware",
            Self::Unknown => "unknown",
        }
    }
}

impl fmt::Display for DpiAwareness {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

/// The awareness the server runs with, as `get_server_status` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DpiAwarenessStatus {
    pub awareness: DpiAwareness,
    /// Why coordinates cannot be trusted, or `None` when they can.
    pub reason: Option<String>,
}

impl DpiAwarenessStatus {
    #[must_use]
    pub const fn is_usable(&self) -> bool {
        self.reason.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::DpiAwareness;

    #[test]
    fn only_per_monitor_modes_keep_physical_coordinates() {
        assert!(DpiAwareness::PerMonitorV2.is_per_monitor());
        assert!(DpiAwareness::PerMonitor.is_per_monitor());
        for virtualized in [
            DpiAwareness::SystemAware,
            DpiAwareness::Unaware,
            DpiAwareness::Unknown,
        ] {
            assert!(!virtualized.is_per_monitor(), "{virtualized}");
        }
        assert_eq!(
            serde_json::to_value(DpiAwareness::SystemAware).unwrap(),
            DpiAwareness::SystemAware.code()
        );
    }
}
