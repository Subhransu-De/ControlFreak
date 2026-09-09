use serde::{Deserialize, Serialize};

use crate::{BackendIdentity, CapabilityDescriptor, PermissionDescriptor, SecurityContext};

const CAPABILITY_REPORT_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityReport {
    schema_version: u32,
    pub backend: BackendIdentity,
    pub capabilities: Vec<CapabilityDescriptor>,
    permissions: Vec<PermissionDescriptor>,
    security_context: SecurityContext,
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
        }
    }
}
