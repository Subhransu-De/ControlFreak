use serde::{Deserialize, Serialize};

use crate::{ActionObservation, ObservationOptions, WindowInfo};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VirtualDesktopInfo {
    pub id: String,
    pub is_current: bool,
    pub windows: Vec<WindowInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VirtualDesktopList {
    pub desktops: Vec<VirtualDesktopInfo>,
    pub includes_empty_desktops: bool,
    pub order_available: bool,
    pub names_available: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VirtualDesktopDirection {
    Left,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VirtualDesktopSwitchRequest {
    pub direction: VirtualDesktopDirection,
    pub steps: u8,
    pub observation: ObservationOptions,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtualDesktopSwitchResult {
    pub direction: VirtualDesktopDirection,
    pub requested_steps: u8,
    pub changed: bool,
    pub observation: ActionObservation,
}
