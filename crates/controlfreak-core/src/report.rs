use serde::{Deserialize, Serialize};

use crate::{
    BackendIdentity, BuildInfo, CapabilityDescriptor, PermissionDescriptor, SecurityContext,
};

const CAPABILITY_REPORT_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityReport {
    schema_version: u32,
    pub backend: BackendIdentity,
    pub capabilities: Vec<CapabilityDescriptor>,
    permissions: Vec<PermissionDescriptor>,
    security_context: SecurityContext,
    /// The executable's build identity. A backend does not know it; the server adds it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<BuildInfo>,
}

impl CapabilityReport {
    pub(crate) fn new(
        backend: BackendIdentity,
        capabilities: Vec<CapabilityDescriptor>,
        permissions: Vec<PermissionDescriptor>,
        security_context: SecurityContext,
    ) -> Self {
        Self {
            schema_version: CAPABILITY_REPORT_SCHEMA_VERSION,
            backend,
            capabilities,
            permissions,
            security_context,
            build: None,
        }
    }

    #[must_use]
    pub fn with_build(mut self, build: BuildInfo) -> Self {
        self.build = Some(build);
        self
    }
}
