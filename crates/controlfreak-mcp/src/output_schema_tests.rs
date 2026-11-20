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
        display_layout: controlfreak_core::DisplayLayout::from_displays(&[display()]),
        png: vec![1, 2, 3],
        image_width: 5,
        image_height: 5,
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
        owner: None,
    }
}

/// A region that changed at some point. `reverted` means it then returned to the baseline and
/// settled there; otherwise the wait timed out while it still differed.
fn visual_change(reverted: bool) -> controlfreak_core::VisualChangeResult {
    controlfreak_core::VisualChangeResult {
        changed_once: true,
        currently_changed: !reverted,
        settled: reverted,
        difference: if reverted { 0.0 } else { 0.25 },
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

pub(super) struct ObservationBackend;

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
    fn list_virtual_desktops(&self) -> Result<VirtualDesktopList, PlatformError> {
        Ok(VirtualDesktopList::from_windows(
            self.list_windows()?,
            None,
            |_| Some(("desktop".into(), true)),
        ))
    }
    fn capture_window(
        &self,
        _: &CaptureWindowRequest,
    ) -> Result<controlfreak_core::WindowScreenshot, PlatformError> {
        Ok(controlfreak_core::WindowScreenshot {
            window: window(),
            source_bounds: window().bounds,
            display_layout: controlfreak_core::DisplayLayout::from_displays(&[display()]),
            png: vec![1, 2, 3],
            image_width: 5,
            image_height: 5,
            cursor_marker: true,
            capture_method: "windows_print_window",
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

/// The output schema of `name` in the full contract that `--print-tools` exports.
pub(super) fn full_validator(name: &str) -> jsonschema::Validator {
    let tool = full_tool_definitions()
        .into_iter()
        .find(|tool| tool.name == name)
        .unwrap();
    jsonschema::validator_for(&Value::Object((*tool.output_schema.unwrap()).clone())).unwrap()
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

/// `tools/list` differs from the full contract only in its open, shallow success branches: the
/// error branch, names, descriptions, input schemas and annotations are the same byte for byte.
#[test]
fn published_schemas_differ_from_the_full_contract_only_in_success_branches() {
    let compact = tools();
    let full = full_tool_definitions();
    assert_eq!(compact.len(), full.len());
    let output = |tool: &Tool| Value::Object((*tool.output_schema.clone().unwrap()).clone());
    for (compact, full) in compact.iter().zip(&full) {
        let name = &compact.name;
        assert_eq!(compact.name, full.name);
        assert_eq!(compact.title, full.title, "{name}");
        assert_eq!(compact.description, full.description, "{name}");
        assert_eq!(compact.input_schema, full.input_schema, "{name}");
        assert_eq!(compact.annotations, full.annotations, "{name}");
        let (published, complete) = (output(compact), output(full));
        let branches = |schema: &Value| schema["anyOf"].as_array().unwrap().clone();
        let (published_error, published_success) = branches(&published)
            .split_last()
            .map(|(e, s)| (e.clone(), s.to_vec()))
            .unwrap();
        let (complete_error, complete_success) = branches(&complete)
            .split_last()
            .map(|(e, s)| (e.clone(), s.to_vec()))
            .unwrap();
        assert_eq!(published_error, complete_error, "{name}");
        assert_eq!(published_error["additionalProperties"], false, "{name}");
        assert_eq!(published_success.len(), complete_success.len(), "{name}");
        let complete_schema = &complete;
        for (published, complete) in published_success.iter().zip(&complete_success) {
            let complete = complete["$ref"].as_str().map_or(complete, |reference| {
                &complete_schema["$defs"][reference.trim_start_matches("#/$defs/")]
            });
            assert_eq!(published["additionalProperties"], true, "{name}");
            assert_eq!(complete["additionalProperties"], false, "{name}");
            assert_eq!(published["required"], complete["required"], "{name}");
            let names = |branch: &Value| {
                branch["properties"]
                    .as_object()
                    .unwrap()
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
            };
            assert_eq!(names(published), names(complete), "{name}");
        }
        assert!(
            published.to_string().len() < complete.to_string().len(),
            "{name} publishes no smaller schema"
        );
    }
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
    // The full contract pins every field; the published one keeps its top-level enums.
    let validator = full_validator(GET_SERVER_STATUS);
    let published = tools()
        .into_iter()
        .find(|tool| tool.name == GET_SERVER_STATUS)
        .unwrap();
    let published =
        jsonschema::validator_for(&Value::Object((*published.output_schema.unwrap()).clone()))
            .unwrap();
    let base = json!({
        "status": "ready",
        "safety_indicator": {"status": "dormant", "reason": null},
        "session": null,
        "version": "1",
        "build": {"schema_version": 1, "version": "1", "revision": "unknown", "dirty": null, "target": "unknown"},
        "instance_id": "i", "process_id": 1,
        "server_elevated": false, "windows_integrity_level": "medium",
        "elevated_operation_allowed": false, "dpi_awareness": null, "uptime_ms": 1,
        "operations_started": 0, "operations_completed": 0, "operations_failed": 0,
        "recent_operations": [],
        "backend": {"platform": "windows", "display_server": "dwm", "backend_id": "b"},
        "input_admission": "open",
        "recovery": null,
        "stop_state": "ready",
        "profile": {"name": "full", "tool_allowlist": null}
    });
    assert!(validator.is_valid(&base));
    let observe = with(
        base.clone(),
        json!({"profile": {"name": "observe", "tool_allowlist": ["list_windows"]}}),
    );
    assert!(validator.is_valid(&observe), "{observe}");
    let busy = with(
        base.clone(),
        json!({"session": {"state": "busy", "draining": false, "stop_state": "ready"}}),
    );
    assert!(validator.is_valid(&busy), "{busy}");
    for (stop_state, actor, action) in [
        ("draining", "agent", "wait_for_drain"),
        ("stopped", "user", "allow_again"),
        ("stopped", "user", "restart_client"),
        ("cleanup_failed", "user", "resolve_held_input"),
    ] {
        let stopped = with(
            base.clone(),
            json!({
                "stop_state": stop_state, "stop_reason": "user_stop", "input_admission": "closed",
                "recovery": {"actor": actor, "action": action, "message": "m"}
            }),
        );
        assert!(validator.is_valid(&stopped), "{stopped}");
    }
    for awareness in [
        "per_monitor_v2",
        "per_monitor",
        "system_aware",
        "unaware",
        "unknown",
    ] {
        let reported = with(
            base.clone(),
            json!({"dpi_awareness": {"awareness": awareness, "reason": null}}),
        );
        assert!(validator.is_valid(&reported), "{reported}");
    }
    let refused = with(
        base.clone(),
        json!({"status": "degraded", "dpi_awareness": {"awareness": "unaware", "reason": "r"}}),
    );
    assert!(validator.is_valid(&refused), "{refused}");
    let built = with(
        base.clone(),
        json!({"build": {"schema_version": 1, "version": "1", "revision": "a".repeat(40), "dirty": false, "target": "x86_64-pc-windows-msvc"}}),
    );
    assert!(validator.is_valid(&built), "{built}");
    for build in [
        json!({"schema_version": 1, "version": "1", "revision": r"C:\src", "dirty": null, "target": "unknown"}),
        json!({"schema_version": 1, "version": "1", "revision": "unknown", "dirty": null, "target": "unknown", "built_at": "t"}),
        json!({"schema_version": 1, "version": "1", "revision": "unknown", "target": "unknown"}),
    ] {
        assert!(
            !validator.is_valid(&with(base.clone(), json!({"build": build}))),
            "{build}"
        );
    }
    assert!(!validator.is_valid(&with(
        base.clone(),
        json!({"dpi_awareness": {"awareness": "per_monitor_v3", "reason": null}})
    )));
    assert!(!validator.is_valid(&with(base.clone(), json!({"status": "bogus"}))));
    assert!(
        !validator.is_valid(&with(
            base.clone(),
            json!({"recovery": {"actor": "user", "action": "rearm", "message": "m"}})
        )),
        "no recovery action reopens input from MCP"
    );
    assert!(published.is_valid(&base));
    assert!(!published.is_valid(&with(base.clone(), json!({"status": "bogus"}))));
    let mut no_build = base.clone();
    no_build.as_object_mut().unwrap().remove("build");
    assert!(!validator.is_valid(&no_build), "build is required");
    assert!(!published.is_valid(&no_build), "build is required");
    let mut missing = base;
    missing.as_object_mut().unwrap().remove("input_admission");
    assert!(!validator.is_valid(&missing), "input_admission is required");
    assert!(!published.is_valid(&missing), "input_admission is required");
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
            json!({"target_ref": "target-00000000-0000-0000-0000-000000000003"}),
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
        (LIST_VIRTUAL_DESKTOPS, json!({}), false),
        (
            CAPTURE_WINDOW,
            json!({"window_id":"target-00000000-0000-0000-0000-000000000003"}),
            true,
        ),
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
        assert!(
            full_validator(name).is_valid(content),
            "{name} result does not match the full contract: {content}"
        );
        let has_image_block = result["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(|block| block["type"] == "image");
        assert_eq!(has_image_block, has_image, "{name}");
        if name == WAIT_FOR_VISUAL_CHANGE {
            // Changed once, changed back and settled: the result tells the two apart.
            assert_eq!(content["changed"], true);
            assert_eq!(content["changed_once"], true);
            assert_eq!(content["currently_changed"], false);
            assert_eq!(content["settled"], true);
            assert_eq!(content["timed_out"], false);
        }
    }
    harness.finish().await;
}
