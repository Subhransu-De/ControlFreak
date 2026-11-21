use serde::{Deserialize, Serialize};

use crate::{
    BackendIdentity, BuildInfo, CapabilityDescriptor, ForbiddenFamily, HardLimits, HelperReadiness,
    OcrReadiness, PermissionDescriptor, Readiness, RuntimeReadiness, SecurityContext,
};

const CAPABILITY_REPORT_SCHEMA_VERSION: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityReport {
    schema_version: u32,
    pub backend: BackendIdentity,
    pub capabilities: Vec<CapabilityDescriptor>,
    permissions: Vec<PermissionDescriptor>,
    security_context: SecurityContext,
    /// OCR readiness and installed languages, from the backend's bounded probe.
    ocr: OcrReadiness,
    helpers: HelperReadiness,
    limits: HardLimits,
    /// Tool families the server never offers; see [`ForbiddenFamily`].
    forbidden_families: Vec<ForbiddenFamily>,
    /// The executable's build identity. A backend does not know it; the server adds it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    build: Option<BuildInfo>,
}

impl CapabilityReport {
    pub(crate) fn new(
        backend: BackendIdentity,
        capabilities: Vec<CapabilityDescriptor>,
        permissions: Vec<PermissionDescriptor>,
        security_context: SecurityContext,
        readiness: RuntimeReadiness,
    ) -> Self {
        Self {
            schema_version: CAPABILITY_REPORT_SCHEMA_VERSION,
            backend,
            capabilities,
            permissions,
            security_context,
            ocr: readiness.ocr,
            helpers: readiness.helpers,
            limits: HardLimits::CURRENT,
            forbidden_families: ForbiddenFamily::ALL.to_vec(),
            build: None,
        }
    }

    #[must_use]
    pub fn with_build(mut self, build: BuildInfo) -> Self {
        self.build = Some(build);
        self
    }

    /// The session lamp is started by the server, not the backend, so the server reports it.
    #[must_use]
    pub const fn with_lamp(mut self, lamp: Readiness) -> Self {
        self.helpers.lamp = lamp;
        self
    }
}
