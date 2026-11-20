//! The executable must carry the manifest that `build.rs` embeds.
#![cfg(target_os = "windows")]

use std::path::Path;

#[test]
fn the_executable_declares_per_monitor_v2_dpi_awareness_once() {
    let manifest = controlfreak_platform::packaging_tests::embedded_manifest(Path::new(env!(
        "CARGO_BIN_EXE_controlfreak"
    )))
    .expect("read the executable's resources")
    .expect("controlfreak.exe has an application manifest");
    assert!(
        manifest.contains(">PerMonitorV2</dpiAwareness>"),
        "{manifest}"
    );
    assert!(manifest.contains(">true/pm</dpiAware>"), "{manifest}");
    assert_eq!(
        manifest.matches("<requestedExecutionLevel").count(),
        1,
        "the linker must not add a second execution level: {manifest}"
    );
    assert!(manifest.contains(r#"level="asInvoker""#), "{manifest}");
}
