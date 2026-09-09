use controlfreak_core::{
    BackendIdentity, BackendMetadata, Capability, CapabilityDescriptor, CapabilityReport,
    CapabilityState, DisplayBackend, DisplayBounds, DisplayServer, KeyboardBackend, OcrBackend,
    Platform, PlatformBackend, PointerBackend, WindowBackend,
};

fn assert_object_safe(_: &dyn PlatformBackend) {}

struct TestBackend;

impl BackendMetadata for TestBackend {
    fn identity(&self) -> BackendIdentity {
        BackendIdentity {
            platform: Platform::Unsupported,
            display_server: DisplayServer::Unknown,
            backend_id: "test".to_owned(),
        }
    }

    fn capabilities(&self) -> Vec<CapabilityDescriptor> {
        Vec::new()
    }

    fn permissions(&self) -> Vec<controlfreak_core::PermissionDescriptor> {
        Vec::new()
    }
}

impl DisplayBackend for TestBackend {}
impl OcrBackend for TestBackend {}
impl PointerBackend for TestBackend {}
impl WindowBackend for TestBackend {}
impl KeyboardBackend for TestBackend {}

#[test]
fn platform_backend_is_object_safe() {
    assert_object_safe(&TestBackend);
}

#[test]
fn capability_report_round_trips_with_stable_field_names() {
    let report = TestBackend.report();
    let json = serde_json::to_value(&report).expect("serialize report");

    assert_eq!(json["schema_version"], 2);
    assert!(json["security_context"]["elevated"].is_boolean());
    assert!(json["security_context"]["windows_integrity_level"].is_string());
    assert!(json.get("backend").is_some());
    assert!(json.get("capabilities").is_some());
    assert!(json.get("permissions").is_some());

    let decoded: CapabilityReport = serde_json::from_value(json).expect("deserialize report");
    assert_eq!(decoded, report);
}

#[test]
fn skeleton_capabilities_are_not_supported() {
    let descriptor = CapabilityDescriptor {
        capability: Capability::ScreenCapture,
        state: CapabilityState::Unsupported {
            reason: "skeleton".to_owned(),
        },
        required_permissions: Vec::new(),
    };

    assert!(!matches!(descriptor.state, CapabilityState::Supported));
}

#[test]
fn display_local_coordinates_map_to_negative_virtual_space() {
    let bounds = DisplayBounds {
        left: -1920,
        top: 0,
        width: 1920,
        height: 1080,
    };

    assert_eq!(
        bounds.to_virtual(100, 50).expect("valid point"),
        (-1820, 50)
    );
    assert!(bounds.to_virtual(1920, 50).is_err());
}
