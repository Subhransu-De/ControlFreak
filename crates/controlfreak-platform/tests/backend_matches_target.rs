use controlfreak_core::{Capability, CapabilityState, OcrRegionRequest, Platform, PlatformError};

#[test]
fn backend_matches_compilation_target() {
    let report = controlfreak_platform::backend().report();

    let expected = if cfg!(target_os = "windows") {
        Platform::Windows
    } else if cfg!(target_os = "macos") {
        Platform::MacOs
    } else if cfg!(target_os = "linux") {
        Platform::Linux
    } else {
        Platform::Unsupported
    };

    assert_eq!(report.backend.platform, expected);
    let supported: Vec<Capability> = report
        .capabilities
        .iter()
        .filter(|descriptor| matches!(descriptor.state, CapabilityState::Supported))
        .map(|descriptor| descriptor.capability)
        .collect();
    if cfg!(target_os = "windows") {
        assert_eq!(
            supported,
            [
                Capability::ScreenCapture,
                Capability::WindowEnumeration,
                Capability::PointerMovement,
                Capability::InputInjection,
            ]
        );
    } else {
        assert!(supported.is_empty());
    }
}

#[cfg(target_os = "windows")]
#[test]
fn default_backend_does_not_require_an_ocr_helper_argument() {
    let backend = controlfreak_platform::backend();
    let error = backend
        .read_text_in_region(&OcrRegionRequest {
            display_id: "controlfreak-missing-display".to_owned(),
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            language: None,
        })
        .expect_err("reject an unknown display without relaunching the test executable");

    assert!(matches!(
        error,
        PlatformError::InvalidArgument { argument, .. } if argument == "display_id"
    ));
}
