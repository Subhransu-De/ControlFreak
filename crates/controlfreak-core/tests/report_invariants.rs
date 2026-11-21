use controlfreak_core::{
    BackendIdentity, BackendMetadata, BuildInfo, Capability, CapabilityDescriptor,
    CapabilityReport, CapabilityState, DisplayBackend, DisplayBounds, DisplayServer,
    KeyboardBackend, OcrBackend, Platform, PlatformBackend, PointerBackend, WindowBackend,
};

fn assert_object_safe(_: &dyn PlatformBackend) {}

struct TestBackend;

impl BackendMetadata for TestBackend {
    fn identity(&self) -> BackendIdentity {
        BackendIdentity {
            platform: Platform::Windows,
            display_server: DisplayServer::Dwm,
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
impl controlfreak_core::AccessibilityBackend for TestBackend {}

#[test]
fn platform_backend_is_object_safe() {
    assert_object_safe(&TestBackend);
}

#[test]
fn capability_report_round_trips_with_stable_field_names() {
    let report = TestBackend.report();
    let json = serde_json::to_value(&report).expect("serialize report");

    assert_eq!(json["schema_version"], 3);
    assert!(json["security_context"]["elevated"].is_boolean());
    assert!(json["security_context"]["windows_integrity_level"].is_string());
    assert!(json.get("backend").is_some());
    assert!(json.get("capabilities").is_some());
    assert!(json.get("permissions").is_some());
    // A backend without a probe says so instead of claiming OCR or helpers work.
    assert_eq!(
        json["ocr"],
        serde_json::json!({"state": "unsupported", "reason": "not_implemented", "languages": [], "default_language": null})
    );
    assert_eq!(json["helpers"]["ocr"]["state"], "unsupported");
    assert_eq!(json["limits"]["max_wait_ms"], 30_000);
    assert_eq!(
        json["forbidden_families"],
        serde_json::json!([
            "application_launch",
            "shell",
            "filesystem",
            "registry",
            "network",
            "clipboard",
            "browser_dom"
        ])
    );

    // A backend does not know the executable's build; only the server adds it.
    assert!(json.get("build").is_none());

    let decoded: CapabilityReport = serde_json::from_value(json).expect("deserialize report");
    assert_eq!(decoded, report);

    let built = report.with_build(BuildInfo::new("0.2.3", "unknown", None, "unknown"));
    let json = serde_json::to_value(&built).expect("serialize report");
    assert_eq!(json["schema_version"], 3);
    assert_eq!(json["build"]["revision"], "unknown");
    let decoded: CapabilityReport = serde_json::from_value(json).expect("deserialize report");
    assert_eq!(decoded, built);
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
