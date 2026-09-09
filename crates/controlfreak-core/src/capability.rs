use serde::{Deserialize, Serialize};

use crate::PermissionId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    ScreenCapture,
    WindowEnumeration,
    AccessibilityTree,
    PointerMovement,
    InputInjection,
    Elevation,
}

impl Capability {
    pub const ALL: [Self; 6] = [
        Self::ScreenCapture,
        Self::WindowEnumeration,
        Self::AccessibilityTree,
        Self::PointerMovement,
        Self::InputInjection,
        Self::Elevation,
    ];
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CapabilityState {
    Supported,
    Degraded { reason: String },
    RequiresPermission { permission: PermissionId },
    Unsupported { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityDescriptor {
    pub capability: Capability,
    #[serde(flatten)]
    pub state: CapabilityState,
    pub required_permissions: Vec<PermissionId>,
}
