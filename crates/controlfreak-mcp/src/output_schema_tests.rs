//! Every tool publishes an output schema, and real results conform to it over the transport.

use super::session_error_tests::{call, harness_with_backend};
use super::*;
use controlfreak_core::{
    BackendMetadata, DisplayBackend, DisplayBounds, DisplayInfo, DisplayScreenshot,
    KeyboardBackend, OcrBackend, PointerBackend, WindowBackend,
};

fn display() -> DisplayInfo {
    DisplayInfo {
        id: "synthetic".into(),
        name: "synthetic".into(),
        bounds: bounds(),
        is_primary: true,
    }
}

fn bounds() -> DisplayBounds {
    DisplayBounds {
        left: -10,
        top: 0,
        width: 10,
        height: 10,
    }
}

fn screenshot() -> DisplayScreenshot {
    DisplayScreenshot {
        target_ref: Some("target-synthetic".into()),
        display: display(),
        source_bounds: bounds(),
        png: vec![1, 2, 3],
        image_width: 5,
        image_height: 5,
        downscale_factor: 2,
        cursor_marker: false,
    }
}

fn window() -> controlfreak_core::WindowInfo {
    controlfreak_core::WindowInfo {
        id: "synthetic".into(),
        title: "synthetic".into(),
        class_name: "synthetic".into(),
        process_id: 1,
        bounds: controlfreak_core::WindowBounds {
            left: -100,
            top: 0,
            width: 10,
            height: 10,
        },
        display_id: "synthetic".into(),
        is_foreground: true,
        is_minimized: false,
    }
}

fn visual_change(changed: bool) -> controlfreak_core::VisualChangeResult {
    controlfreak_core::VisualChangeResult {
        changed,
        timed_out: !changed,
        difference: 0.25,
        elapsed_ms: 3,
        screenshot: screenshot(),
    }
}

fn text_match() -> controlfreak_core::TextMatch {
    controlfreak_core::TextMatch {
        text: "synthetic".into(),
        x: 1,
        y: 2,
        width: 3,
        height: 4,
    }
}

fn find_result(found: bool) -> controlfreak_core::FindTextResult {
    controlfreak_core::FindTextResult {
        target_ref: None,
        query: "synthetic".into(),
        display: display(),
        source_bounds: bounds(),
        language: "en-US".into(),
        matches: if found { vec![text_match()] } else { vec![] },
        details: controlfreak_core::OcrMatchDetails {
            candidates: vec![],
            candidate_count: usize::from(found),
            candidates_truncated: false,
            text_truncated: false,
            match_tier: found.then_some(controlfreak_core::TextMatchTier::Exact),
            recovery: String::new(),
        },
    }
}

struct ObservationBackend;

impl BackendMetadata for ObservationBackend {
    fn validate_target_reference(&self, _: &str) -> Result<(), PlatformError> {
        Ok(())
    }
    fn identity(&self) -> controlfreak_core::BackendIdentity {
        controlfreak_core::BackendIdentity {
            platform: controlfreak_core::Platform::Windows,
            display_server: controlfreak_core::DisplayServer::Dwm,
            backend_id: "schema-fixture".into(),
        }
    }
    fn capabilities(&self) -> Vec<controlfreak_core::CapabilityDescriptor> {
        vec![]
    }
    fn permissions(&self) -> Vec<controlfreak_core::PermissionDescriptor> {
        vec![]
    }
}

impl DisplayBackend for ObservationBackend {
    fn list_displays(&self) -> Result<Vec<DisplayInfo>, PlatformError> {
        Ok(vec![display()])
    }
    fn capture_display(
        &self,
        _: &CaptureDisplayRequest,
    ) -> Result<DisplayScreenshot, PlatformError> {
        Ok(screenshot())
    }
    fn capture_region(&self, _: &CaptureRegionRequest) -> Result<DisplayScreenshot, PlatformError> {
        Ok(screenshot())
    }
    fn wait_for_visual_change(
        &self,
        _: &WaitForVisualChangeRequest,
    ) -> Result<controlfreak_core::VisualChangeResult, PlatformError> {
        Ok(visual_change(true))
    }
    fn capture_visual_baseline(
        &self,
        _: &VisualBaselineRequest,
    ) -> Result<VisualBaseline, PlatformError> {
        Ok(VisualBaseline {
            baseline_id: BASELINE_ID.into(),
            display: display(),
            source_bounds: bounds(),
            fingerprint: "00ff".into(),
            expires_in_ms: 300_000,
        })
    }
    fn wait_for_change_since(
        &self,
        _: &WaitForChangeSinceRequest,
    ) -> Result<controlfreak_core::VisualChangeResult, PlatformError> {
        // A timeout is a normal result and carries a screenshot too.
        Ok(visual_change(false))
    }
}

impl OcrBackend for ObservationBackend {
    fn read_text_in_region(
        &self,
        _: &OcrRegionRequest,
    ) -> Result<controlfreak_core::OcrResult, PlatformError> {
        Ok(controlfreak_core::OcrResult {
            target_ref: None,
            display: display(),
            source_bounds: bounds(),
            language: "en-US".into(),
            text: "synthetic".into(),
            lines: vec![controlfreak_core::OcrLine {
                text: "synthetic".into(),
                x: 0,
                y: 0,
                width: 9,
                height: 3,
                words: vec![controlfreak_core::OcrWord {
                    text: "synthetic".into(),
                    x: 0,
                    y: 0,
                    width: 9,
                    height: 3,
                }],
            }],
        })
    }
    fn find_text_on_screen_controlled(
        &self,
        _: &controlfreak_core::FindTextRequest,
        _: &MutationControl,
    ) -> Result<controlfreak_core::FindTextResult, PlatformError> {
        Ok(find_result(true))
    }
    fn wait_for_text_controlled(
        &self,
        _: &controlfreak_core::WaitForTextRequest,
        _: &MutationControl,
    ) -> Result<controlfreak_core::WaitForTextResult, PlatformError> {
        Ok(controlfreak_core::WaitForTextResult {
            condition: controlfreak_core::TextCondition::Disappears,
            satisfied: true,
            timed_out: false,
            elapsed_ms: 4,
            attempts: 2,
            last: find_result(false),
        })
    }
}

impl WindowBackend for ObservationBackend {
    fn list_windows(&self) -> Result<controlfreak_core::WindowList, PlatformError> {
        Ok(controlfreak_core::WindowList {
            windows: vec![window()],
            truncated: false,
        })
    }
    fn capture_window(
        &self,
        _: &CaptureWindowRequest,
    ) -> Result<controlfreak_core::WindowScreenshot, PlatformError> {
        Ok(controlfreak_core::WindowScreenshot {
            window: window(),
            source_bounds: window().bounds,
            png: vec![1, 2, 3],
            image_width: 5,
            image_height: 5,
            downscale_factor: 2,
            cursor_marker: true,
        })
    }
    fn wait_for_window(
        &self,
        request: &WaitForWindowRequest,
    ) -> Result<WindowWaitResult, PlatformError> {
        let matched = request.title_contains.as_deref() != Some("absent");
        Ok(WindowWaitResult {
            matched,
            timed_out: !matched,
            elapsed_ms: 1,
            window: matched.then(window),
        })
    }
}

impl PointerBackend for ObservationBackend {}
impl KeyboardBackend for ObservationBackend {}

const BASELINE_ID: &str = "vb-00000000000000000000000000000000-0000000000000000";

fn region() -> Value {
    json!({"display_id": "synthetic", "x": 0, "y": 0, "width": 10, "height": 10})
}

fn with(mut base: Value, extra: Value) -> Value {
    if let (Some(base), Value::Object(extra)) = (base.as_object_mut(), extra) {
        base.extend(extra);
    }
    base
}

#[test]
fn every_tool_publishes_an_object_output_schema() {
    let missing: Vec<_> = tools()
        .iter()
        .filter(|tool| {
            tool.output_schema
                .as_ref()
                .is_none_or(|schema| schema.get("type") != Some(&json!("object")))
        })
        .map(|tool| tool.name.to_string())
        .collect();
    assert!(
        missing.is_empty(),
        "tools without an outputSchema: {missing:?}"
    );
    assert_eq!(tools().len(), 27);
}

#[test]
fn screenshot_and_wait_schemas_share_one_image_metadata_definition() {
    let by_name = |name: &str| {
        tools()
            .into_iter()
            .find(|tool| tool.name == name)
            .and_then(|tool| tool.output_schema)
            .map(|schema| Value::Object((*schema).clone()))
            .unwrap()
    };
    let image_fields = ["mime_type", "byte_length", "image_width", "observation_id"];
    let display = by_name(CAPTURE_DISPLAY);
    let region = by_name(CAPTURE_REGION);
    let window = by_name(CAPTURE_WINDOW);
    assert_eq!(display, region);
    for field in image_fields {
        assert_eq!(
            display["properties"][field], window["properties"][field],
            "{field}"
        );
        for wait in [WAIT_FOR_VISUAL_CHANGE, WAIT_FOR_CHANGE_SINCE] {
            assert_eq!(
                by_name(wait)["properties"]["screenshot"]["properties"][field],
                display["properties"][field],
                "{wait} {field}"
            );
        }
    }
}

#[test]
fn schemas_describe_idle_busy_and_stopped_server_status() {
    let tool = tools()
        .into_iter()
        .find(|tool| tool.name == GET_SERVER_STATUS)
        .unwrap();
    let schema = Value::Object((*tool.output_schema.unwrap()).clone());
    let validator = jsonschema::validator_for(&schema).unwrap();
    let base = json!({
        "status": "ready",
        "safety_indicator": {"status": "dormant", "reason": null},
        "session": null,
        "version": "1", "instance_id": "i", "process_id": 1,
        "server_elevated": false, "windows_integrity_level": "medium",
        "elevated_operation_allowed": false, "uptime_ms": 1,
        "operations_started": 0, "operations_completed": 0, "operations_failed": 0,
        "recent_operations": [],
        "backend": {"platform": "windows", "display_server": "dwm", "backend_id": "b"},
        "stop_state": "ready"
    });
    assert!(validator.is_valid(&base));
    let busy = with(
        base.clone(),
        json!({"session": {"state": "busy", "draining": false, "stop_state": "ready"}}),
    );
    assert!(validator.is_valid(&busy), "{busy}");
    let stopped = with(
        base.clone(),
        json!({"stop_state": "stopped", "stop_reason": "user_stop"}),
    );
    assert!(validator.is_valid(&stopped));
    assert!(!validator.is_valid(&with(base, json!({"status": "bogus"}))));
}

#[tokio::test]
async fn every_observation_and_session_result_validates_against_tools_list() {
    let mut harness = harness_with_backend(
        Arc::new(ObservationBackend),
        Arc::new(AtomicBool::new(false)),
    )
    .await;
    let listed = outcome_tests::exchange(
        &mut harness.client,
        json!({"jsonrpc":"2.0","id":9,"method":"tools/list","params":{}}),
    )
    .await;
    let listed = listed["result"]["tools"].as_array().unwrap().clone();
    assert_eq!(listed.len(), 27);
    assert!(
        listed
            .iter()
            .all(|tool| tool["outputSchema"]["type"] == "object"),
        "every listed tool needs an outputSchema"
    );

    let calls = [
        (
            BEGIN_CONTROL_SESSION,
            json!({"target_ref": "synthetic"}),
            false,
        ),
        (GET_SERVER_STATUS, json!({}), false),
        (LIST_DISPLAYS, json!({}), false),
        (CAPTURE_DISPLAY, json!({"display_id": "synthetic"}), true),
        (CAPTURE_REGION, region(), true),
        (WAIT_FOR_VISUAL_CHANGE, region(), true),
        (CAPTURE_VISUAL_BASELINE, region(), false),
        (
            WAIT_FOR_CHANGE_SINCE,
            json!({"baseline_id": BASELINE_ID}),
            true,
        ),
        (READ_TEXT_IN_REGION, region(), false),
        (
            FIND_TEXT_ON_SCREEN,
            with(region(), json!({"query": "synthetic"})),
            false,
        ),
        (
            WAIT_FOR_TEXT,
            with(
                region(),
                json!({"query": "synthetic", "condition": "disappears"}),
            ),
            false,
        ),
        (LIST_WINDOWS, json!({}), false),
        (CAPTURE_WINDOW, json!({"window_id": "synthetic"}), true),
        (
            WAIT_FOR_WINDOW,
            json!({"title_contains": "synthetic"}),
            false,
        ),
        (WAIT_FOR_WINDOW, json!({"title_contains": "absent"}), false),
        (END_CONTROL_SESSION, json!({}), false),
        ("stop_desktop_work", json!({}), false),
    ];
    for (name, arguments, has_image) in calls {
        let result = call(&mut harness, name, arguments).await;
        assert_ne!(result["isError"], true, "{name}: {result}");
        let schema = &listed.iter().find(|tool| tool["name"] == name).unwrap()["outputSchema"];
        let content = &result["structuredContent"];
        let validator = jsonschema::validator_for(schema).unwrap();
        assert!(
            validator.is_valid(content),
            "{name} result does not match its outputSchema: {content}"
        );
        let has_image_block = result["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(|block| block["type"] == "image");
        assert_eq!(has_image_block, has_image, "{name}");
    }
    harness.finish().await;
}
