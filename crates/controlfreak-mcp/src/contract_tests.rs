//! The published tool contract: output schemas that accept every real error, annotations,
//! aliases, defaults and the recovery rules an agent relies on.

use super::output_schema_tests::ObservationBackend;
use super::session_error_tests::{call, harness_with_backend};
use super::*;
use controlfreak_core::{
    BackendMetadata, DisplayBackend, DisplayBounds, DisplayInfo, DisplayScreenshot,
    KeyboardBackend, MouseButton, MutationProgress, OcrBackend, PointerBackend, WindowBackend,
    WindowBounds, WindowInfo,
};

fn bounds(left: i32, width: u32) -> DisplayBounds {
    DisplayBounds {
        left,
        top: 0,
        width,
        height: 600,
    }
}

fn display(id: &str, is_primary: bool, left: i32) -> DisplayInfo {
    DisplayInfo {
        id: id.into(),
        name: id.into(),
        bounds: bounds(left, 800),
        is_primary,
    }
}

fn window(id: &str) -> WindowInfo {
    WindowInfo {
        id: id.into(),
        title: id.into(),
        class_name: "fixture".into(),
        process_id: 7,
        bounds: WindowBounds {
            left: 0,
            top: 0,
            width: 400,
            height: 300,
        },
        display_id: "main".into(),
        is_foreground: id == "target-00000000-0000-0000-0000-000000000001",
        is_minimized: false,
        owner: None,
    }
}

#[derive(Default)]
struct Recorded {
    captures: Vec<CaptureDisplayRequest>,
    regions: Vec<CaptureRegionRequest>,
    visual_waits: Vec<WaitForVisualChangeRequest>,
    clicks: Vec<MouseClickRequest>,
    key_chords: Vec<KeyChordRequest>,
    focused: Vec<FocusWindowRequest>,
    validated: Vec<String>,
    switches: usize,
}

/// Two displays, the primary one listed second, and a window list the test can change.
struct ContractBackend {
    recorded: Mutex<Recorded>,
    windows: Mutex<Vec<WindowInfo>>,
    click_error: Mutex<Option<PlatformError>>,
    /// Truncates `list_windows` like the real backend's cap, without hiding windows from
    /// `wait_for_window`.
    list_cap: Mutex<Option<usize>>,
}

impl ContractBackend {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            recorded: Mutex::default(),
            windows: Mutex::new(vec![
                window("target-00000000-0000-0000-0000-000000000001"),
                window("target-00000000-0000-0000-0000-000000000002"),
            ]),
            click_error: Mutex::new(None),
            list_cap: Mutex::new(None),
        })
    }

    fn recorded<T>(&self, read: impl FnOnce(&Recorded) -> T) -> T {
        read(&self.recorded.lock().unwrap())
    }

    #[allow(clippy::unused_self, clippy::unnecessary_wraps)] // mirrors the other fixture backends
    fn act<T>(&self, control: &MutationControl, value: T) -> Result<T, PlatformError> {
        control.dispatch_started();
        control.dispatch_accepted(2);
        control.input_complete();
        Ok(value)
    }
}

fn observation() -> ActionObservation {
    ActionObservation {
        foreground_window: None,
        screenshot: None,
    }
}

fn screenshot(request_width: Option<u32>) -> DisplayScreenshot {
    let width = request_width.unwrap_or(800).min(800);
    DisplayScreenshot {
        target_ref: None,
        display: display("main", true, 0),
        source_bounds: bounds(0, 800),
        png: vec![1, 2, 3],
        image_width: width,
        image_height: 600,
        downscale_factor: 1,
        cursor_marker: false,
    }
}

impl BackendMetadata for ContractBackend {
    fn validate_target_reference(&self, target: &str) -> Result<(), PlatformError> {
        self.recorded
            .lock()
            .unwrap()
            .validated
            .push(target.to_owned());
        if target.starts_with("target-") {
            Ok(())
        } else {
            Err(PlatformError::TargetInvalidated {
                reason: "unknown fixture target".into(),
            })
        }
    }
    fn identity(&self) -> controlfreak_core::BackendIdentity {
        controlfreak_core::BackendIdentity {
            platform: controlfreak_core::Platform::Windows,
            display_server: controlfreak_core::DisplayServer::Dwm,
            backend_id: "contract".into(),
        }
    }
    fn capabilities(&self) -> Vec<controlfreak_core::CapabilityDescriptor> {
        vec![]
    }
    fn permissions(&self) -> Vec<controlfreak_core::PermissionDescriptor> {
        vec![]
    }
}

impl DisplayBackend for ContractBackend {
    fn list_displays(&self) -> Result<Vec<DisplayInfo>, PlatformError> {
        Ok(vec![
            display("second", false, -800),
            display("main", true, 0),
        ])
    }
    fn capture_display(
        &self,
        request: &CaptureDisplayRequest,
    ) -> Result<DisplayScreenshot, PlatformError> {
        self.recorded.lock().unwrap().captures.push(request.clone());
        Ok(screenshot(request.max_width))
    }
    fn capture_region(
        &self,
        request: &CaptureRegionRequest,
    ) -> Result<DisplayScreenshot, PlatformError> {
        self.recorded.lock().unwrap().regions.push(request.clone());
        Ok(screenshot(request.max_width))
    }
    fn wait_for_visual_change(
        &self,
        request: &WaitForVisualChangeRequest,
    ) -> Result<controlfreak_core::VisualChangeResult, PlatformError> {
        self.recorded
            .lock()
            .unwrap()
            .visual_waits
            .push(request.clone());
        Ok(controlfreak_core::VisualChangeResult {
            changed: false,
            timed_out: true,
            difference: 0.0,
            elapsed_ms: 1,
            screenshot: screenshot(None),
        })
    }
}

impl OcrBackend for ContractBackend {}

impl PointerBackend for ContractBackend {
    fn move_mouse_controlled(
        &self,
        request: &MouseMoveRequest,
        control: &MutationControl,
    ) -> Result<PointerActionResult, PlatformError> {
        self.act(control, pointer(&request.display_id))
    }
    fn click_mouse_controlled(
        &self,
        request: &MouseClickRequest,
        control: &MutationControl,
    ) -> Result<PointerActionResult, PlatformError> {
        self.recorded.lock().unwrap().clicks.push(request.clone());
        if let Some(error) = self.click_error.lock().unwrap().clone() {
            return Err(error);
        }
        self.act(control, pointer(&request.display_id))
    }
}

fn pointer(display_id: &str) -> PointerActionResult {
    PointerActionResult {
        position: controlfreak_core::MousePosition {
            display_id: display_id.into(),
            local_x: 1,
            local_y: 1,
            virtual_x: 1,
            virtual_y: 1,
        },
        observation: observation(),
    }
}

impl KeyboardBackend for ContractBackend {
    fn press_keys_controlled(
        &self,
        request: &KeyChordRequest,
        control: &MutationControl,
    ) -> Result<KeyboardActionResult, PlatformError> {
        self.recorded
            .lock()
            .unwrap()
            .key_chords
            .push(request.clone());
        self.act(
            control,
            KeyboardActionResult {
                observation: observation(),
            },
        )
    }
}

impl WindowBackend for ContractBackend {
    fn list_windows(&self) -> Result<controlfreak_core::WindowList, PlatformError> {
        let mut windows = self.windows.lock().unwrap().clone();
        let cap = *self.list_cap.lock().unwrap();
        let truncated = cap.is_some_and(|cap| windows.len() > cap);
        if let Some(cap) = cap {
            windows.truncate(cap);
        }
        Ok(controlfreak_core::WindowList { windows, truncated })
    }
    fn wait_for_window(
        &self,
        request: &controlfreak_core::WaitForWindowRequest,
    ) -> Result<controlfreak_core::WindowWaitResult, PlatformError> {
        let window = self
            .windows
            .lock()
            .unwrap()
            .iter()
            .find(|window| request.window_id.as_ref().is_none_or(|id| *id == window.id))
            .cloned();
        Ok(controlfreak_core::WindowWaitResult {
            matched: window.is_some(),
            timed_out: window.is_none(),
            elapsed_ms: 0,
            window,
        })
    }
    fn capture_window(
        &self,
        request: &CaptureWindowRequest,
    ) -> Result<controlfreak_core::WindowScreenshot, PlatformError> {
        let window = self
            .windows
            .lock()
            .unwrap()
            .iter()
            .find(|window| window.id == request.window_id)
            .cloned()
            .ok_or_else(|| PlatformError::TargetInvalidated {
                reason: "unknown window".into(),
            })?;
        Ok(controlfreak_core::WindowScreenshot {
            source_bounds: window.bounds,
            window,
            png: vec![1, 2, 3],
            image_width: request.max_width.map_or(400, |width| width.min(400)),
            image_height: 300,
            downscale_factor: 1,
            cursor_marker: false,
            capture_method: "windows_print_window",
        })
    }
    fn focus_window_controlled(
        &self,
        request: &FocusWindowRequest,
        control: &MutationControl,
    ) -> Result<WindowFocusResult, PlatformError> {
        self.recorded.lock().unwrap().focused.push(request.clone());
        self.act(
            control,
            WindowFocusResult {
                window: window(&request.window_id),
                observation: observation(),
                attempts: 1,
                elapsed_ms: 0,
            },
        )
    }
    fn switch_virtual_desktop_controlled(
        &self,
        request: &VirtualDesktopSwitchRequest,
        control: &MutationControl,
    ) -> Result<VirtualDesktopSwitchResult, PlatformError> {
        self.recorded.lock().unwrap().switches += 1;
        self.act(
            control,
            VirtualDesktopSwitchResult {
                direction: request.direction,
                requested_steps: request.steps,
                changed: true,
                observation: observation(),
            },
        )
    }
}

async fn harness(backend: Arc<dyn PlatformBackend>) -> super::session_error_tests::Harness {
    harness_with_backend(backend, Arc::new(AtomicBool::new(false))).await
}

fn tool_named(name: &str) -> Tool {
    tools().into_iter().find(|tool| tool.name == name).unwrap()
}

fn validator_for(name: &str) -> jsonschema::Validator {
    let schema = Value::Object((*tool_named(name).output_schema.unwrap()).clone());
    jsonschema::validator_for(&schema).unwrap()
}

fn structured(result: &CallToolResult) -> Value {
    result.structured_content.clone().expect("structured")
}

fn all_platform_errors() -> Vec<PlatformError> {
    let details = controlfreak_core::OcrMatchDetails {
        candidates: vec![],
        candidate_count: 0,
        candidates_truncated: false,
        text_truncated: false,
        match_tier: None,
        recovery: "narrow the region".into(),
    };
    vec![
        PlatformError::TargetInvalidated {
            reason: "moved".into(),
        },
        PlatformError::ActivationFailed {
            reason: "foreground_activation_failed".into(),
            attempts: 3,
            elapsed_ms: 400,
        },
        PlatformError::OcrNoMatch {
            details: details.clone(),
        },
        PlatformError::OcrAmbiguousMatch { details },
        PlatformError::Unsupported {
            operation: "x".into(),
            reason: "y".into(),
        },
        PlatformError::PermissionDenied {
            permission: "p".into(),
        },
        PlatformError::HigherIntegrityTarget {
            operation: "x".into(),
            process_id: 1,
            server_integrity: controlfreak_core::IntegrityLevel::Medium,
            target_integrity: controlfreak_core::IntegrityLevel::High,
        },
        PlatformError::TargetIntegrityUnavailable {
            operation: "x".into(),
            reason: "y".into(),
        },
        PlatformError::InvalidArgument {
            argument: "a".into(),
            reason: "b".into(),
        },
        PlatformError::OperationFailed {
            operation: "x".into(),
            reason: "the pointer moved after target validation".into(),
        },
        PlatformError::failed(controlfreak_core::FailureCause::KeyHeld, "x", "held"),
        PlatformError::refused(controlfreak_core::RefusalCause::NotForeground, "x"),
        PlatformError::refused(controlfreak_core::RefusalCause::OwnIndicator, "x"),
        PlatformError::Unavailable { reason: "r".into() },
    ]
}

fn progress_variants() -> Vec<MutationProgress> {
    let untouched = MutationControl::default();
    let started = MutationControl::default();
    started.dispatch_started();
    let partial = MutationControl::default();
    partial.dispatch_started();
    partial.dispatch_accepted(1);
    partial.steps_incomplete();
    let sent = MutationControl::default();
    sent.dispatch_started();
    sent.dispatch_accepted(2);
    sent.input_complete();
    [untouched, started, partial, sent]
        .iter()
        .map(MutationControl::progress)
        .collect()
}

/// F-12: a client that validates `structuredContent` against `outputSchema` must accept the
/// error results the server really returns, for every tool.
#[test]
fn every_error_the_server_builds_validates_against_every_tool_schema() {
    let session_errors = || {
        let stop = controlfreak_core::StopController::default();
        vec![
            SessionError::DesktopInUse {
                owner_instance_id: Some("other".into()),
                retry_after_ms: 25,
            },
            SessionError::ArbitrationUnavailable { reason: "r".into() },
            SessionError::ShuttingDown,
            SessionError::StillClosing,
            SessionError::stopped(&stop),
            SessionError::Environment("locked".into()),
            SessionError::Indicator("dead".into()),
            SessionError::Worker("join".into()),
        ]
    };
    let indicator = SafetyIndicator::dormant();
    let mut checked = 0;
    for tool in tools() {
        let validator = validator_for(&tool.name);
        let mutating = is_mutating_tool(&tool.name);
        let mut results: Vec<CallToolResult> = Vec::new();
        for error in all_platform_errors() {
            results.push(tool_error(&error));
        }
        for error in session_errors() {
            results.push(error.into_tool_result(&indicator));
        }
        results.push(tool_execution_error(&ErrorData::invalid_params(
            "bad",
            Some(json!({"reason": "x"})),
        )));
        results.push(tool_execution_error(&ErrorData::internal_error(
            "boom", None,
        )));
        for result in results {
            let responses = if mutating {
                progress_variants()
                    .into_iter()
                    .map(|progress| results::with_progress(result.clone().into(), progress))
                    .collect()
            } else {
                vec![CallToolResponse::from(result)]
            };
            for response in responses {
                let CallToolResponse::Complete(result) = response else {
                    panic!("complete result expected");
                };
                let content = structured(&result);
                assert!(
                    validator.is_valid(&content),
                    "{} error does not match its outputSchema: {content}\n{:?}",
                    tool.name,
                    validator.iter_errors(&content).collect::<Vec<_>>()
                );
                checked += 1;
            }
        }
    }
    assert!(checked > 27 * 15, "{checked}");
}

/// The same check over the real transport, so the wire form and the schema agree.
#[tokio::test]
async fn real_error_results_over_the_transport_validate_for_all_tools() {
    let mut harness = harness(ContractBackend::new()).await;
    for tool in tools() {
        let result = call(&mut harness, &tool.name, json!({"unknown_option": true})).await;
        assert_eq!(result["isError"], true, "{}: {result}", tool.name);
        let content = &result["structuredContent"];
        assert_eq!(
            content["error"]["code"], "invalid_arguments",
            "{}",
            tool.name
        );
        assert_eq!(content["error"]["retryable"], false, "{}", tool.name);
        let validator = validator_for(&tool.name);
        assert!(
            validator.is_valid(content),
            "{} error does not validate: {content}",
            tool.name
        );
        if is_mutating_tool(&tool.name) {
            assert_eq!(content["status"], "not_started", "{}", tool.name);
        }
    }
    harness.finish().await;
}

/// F-11: every error says whether repeating can help, and the shared wire code has a reason.
#[tokio::test]
async fn platform_failures_carry_retryable_and_a_stable_reason() {
    let backend = ContractBackend::new();
    *backend.click_error.lock().unwrap() = Some(PlatformError::failed(
        controlfreak_core::FailureCause::PointerMoved,
        "click_mouse",
        "the pointer moved after target validation; observe again",
    ));
    let mut harness = harness(backend).await;
    let result = call(
        &mut harness,
        CLICK_MOUSE,
        json!({"target_ref": "target-00000000-0000-0000-0000-000000000001", "x": 1, "y": 1}),
    )
    .await;
    let error = &result["structuredContent"]["error"];
    assert_eq!(error["code"], "platform_operation_failed", "{result}");
    assert_eq!(error["reason"], "pointer_moved");
    assert_eq!(error["retryable"], true);
    // retry_action still tells the agent not to replay the action automatically.
    assert_eq!(result["structuredContent"]["retry_action"], false);
    let unsupported = call(
        &mut harness,
        FIND_TEXT_ON_SCREEN,
        json!({"x":0,"y":0,"width":5,"height":5,"query":"a"}),
    )
    .await;
    let error = &unsupported["structuredContent"]["error"];
    assert_eq!(error["code"], "unsupported_platform_operation");
    assert_eq!(error["retryable"], false);
    assert!(error.get("reason").is_none());
    harness.finish().await;
}

/// F-03, F-08: titles, honest annotations, and the emergency stop at the end.
#[test]
fn every_tool_has_a_title_and_honest_annotations() {
    let all = tools();
    assert_eq!(all.len(), 27);
    assert_eq!(all.last().unwrap().name, STOP_DESKTOP_WORK);
    for tool in &all {
        let title = tool.title.as_deref().unwrap_or_default();
        assert!(!title.is_empty(), "{} needs a title", tool.name);
        let annotations = tool.annotations.as_ref().expect("annotations");
        assert_eq!(annotations.title.as_deref(), Some(title), "{}", tool.name);
        for hint in [
            annotations.read_only_hint,
            annotations.destructive_hint,
            annotations.idempotent_hint,
            annotations.open_world_hint,
        ] {
            assert!(hint.is_some(), "{} states every hint", tool.name);
        }
        if is_mutating_tool(&tool.name) {
            assert_eq!(annotations.read_only_hint, Some(false), "{}", tool.name);
            assert_eq!(annotations.destructive_hint, Some(true), "{}", tool.name);
            assert_eq!(annotations.open_world_hint, Some(true), "{}", tool.name);
        } else if annotations.read_only_hint == Some(true) {
            assert_eq!(annotations.destructive_hint, Some(false), "{}", tool.name);
        }
    }
    let stop = all.last().unwrap();
    assert_eq!(
        stop.annotations.as_ref().unwrap().destructive_hint,
        Some(true)
    );
    let description = stop.description.as_deref().unwrap();
    let first_sentence = description.split('.').next().unwrap();
    assert!(
        first_sentence.contains("end_control_session"),
        "the first sentence must point to the normal way to finish: {description}"
    );
    assert!(description.contains("permanently") || description.contains("closes input"));
}

/// F-54: the route table and the published tool list name exactly the same tools.
#[test]
fn routes_and_published_tools_agree() {
    let mut published: Vec<String> = tools().iter().map(|tool| tool.name.to_string()).collect();
    let mut routed: Vec<String> = ROUTES.iter().map(|(name, _)| (*name).to_owned()).collect();
    published.sort();
    routed.sort();
    assert_eq!(published, routed);
    assert!(route("not_a_tool").is_none());
    assert!(!is_mutating_tool(CAPTURE_DISPLAY));
    assert!(is_mutating_tool(CLICK_TEXT));
}

/// F-04: `target_ref` is an alias of `window_id`, and disagreement is refused.
#[tokio::test]
async fn target_ref_is_an_alias_of_window_id() {
    for name in [FOCUS_WINDOW, CAPTURE_WINDOW, WAIT_FOR_WINDOW] {
        let schema = &tool_named(name).input_schema;
        assert!(schema["properties"]["window_id"].is_object(), "{name}");
        assert!(schema["properties"]["target_ref"].is_object(), "{name}");
        assert_eq!(schema["required"], json!([]), "{name}");
    }
    let backend = ContractBackend::new();
    let mut harness = harness(backend.clone()).await;
    // Begin a session on the window so focus_window may act on it.
    let begun = call(
        &mut harness,
        BEGIN_CONTROL_SESSION,
        json!({"target_ref": "target-00000000-0000-0000-0000-000000000001"}),
    )
    .await;
    assert_ne!(begun["isError"], true, "{begun}");
    let by_alias = call(
        &mut harness,
        FOCUS_WINDOW,
        json!({"target_ref": "target-00000000-0000-0000-0000-000000000001"}),
    )
    .await;
    assert_ne!(by_alias["isError"], true, "{by_alias}");
    assert_eq!(
        backend.recorded(|r| r.focused[0].window_id.clone()),
        "target-00000000-0000-0000-0000-000000000001"
    );
    let both = call(
        &mut harness,
        FOCUS_WINDOW,
        json!({"window_id": "target-00000000-0000-0000-0000-000000000001", "target_ref": "target-00000000-0000-0000-0000-000000000001"}),
    )
    .await;
    assert_ne!(both["isError"], true, "{both}");
    for arguments in [
        json!({"window_id": "target-00000000-0000-0000-0000-000000000001", "target_ref": "target-00000000-0000-0000-0000-000000000002"}),
        json!({}),
    ] {
        let refused = call(&mut harness, FOCUS_WINDOW, arguments).await;
        assert_eq!(refused["isError"], true, "{refused}");
        assert_eq!(
            refused["structuredContent"]["error"]["code"],
            "invalid_argument"
        );
        assert_eq!(refused["structuredContent"]["status"], "not_started");
    }
    let capture = call(
        &mut harness,
        CAPTURE_WINDOW,
        json!({"target_ref": "target-00000000-0000-0000-0000-000000000002"}),
    )
    .await;
    assert_ne!(capture["isError"], true, "{capture}");
    assert_eq!(
        capture["structuredContent"]["window"]["id"],
        "target-00000000-0000-0000-0000-000000000002"
    );
    let clash = call(
        &mut harness,
        CAPTURE_WINDOW,
        json!({"window_id": "target-00000000-0000-0000-0000-000000000001", "target_ref": "target-00000000-0000-0000-0000-000000000002"}),
    )
    .await;
    assert_eq!(clash["isError"], true, "{clash}");
    assert!(clash.to_string().contains("alias of window_id"));
    let waited = call(
        &mut harness,
        WAIT_FOR_WINDOW,
        json!({"target_ref": "target-00000000-0000-0000-0000-000000000002"}),
    )
    .await;
    assert_ne!(waited["isError"], true, "{waited}");
    assert_eq!(
        waited["structuredContent"]["window"]["id"],
        "target-00000000-0000-0000-0000-000000000002"
    );
    harness.finish().await;
}

/// F-07: switching desktops needs no target, and it ends the session it ran in.
#[tokio::test]
async fn switch_virtual_desktop_defaults_its_target_and_ends_the_session() {
    let schema = &tool_named(SWITCH_VIRTUAL_DESKTOP).input_schema;
    assert_eq!(schema["required"], json!(["direction"]));
    assert!(schema["properties"]["target_ref"].is_object());
    let backend = ContractBackend::new();
    let mut harness = harness(backend.clone()).await;
    // No session yet: the foreground window is the target.
    let switched = call(
        &mut harness,
        SWITCH_VIRTUAL_DESKTOP,
        json!({"direction": "right"}),
    )
    .await;
    assert_ne!(switched["isError"], true, "{switched}");
    assert_eq!(
        backend.recorded(|r| r.validated.clone()),
        ["target-00000000-0000-0000-0000-000000000001"]
    );
    assert_eq!(backend.recorded(|r| r.switches), 1);
    let status = call(&mut harness, GET_SERVER_STATUS, json!({})).await;
    let session_state = status["structuredContent"]["session"]["state"].clone();
    assert!(
        session_state == "closing" || session_state == "dormant",
        "the switch must end the session: {status}"
    );
    // A session on another window supplies the target, and an explicit one is still validated.
    call(
        &mut harness,
        BEGIN_CONTROL_SESSION,
        json!({"target_ref": "target-00000000-0000-0000-0000-000000000002"}),
    )
    .await;
    let again = call(
        &mut harness,
        SWITCH_VIRTUAL_DESKTOP,
        json!({"direction": "left"}),
    )
    .await;
    assert_ne!(again["isError"], true, "{again}");
    assert_eq!(
        backend.recorded(|r| r.validated.last().cloned()).as_deref(),
        Some("target-00000000-0000-0000-0000-000000000002")
    );
    let stale = call(
        &mut harness,
        SWITCH_VIRTUAL_DESKTOP,
        json!({"direction": "left", "target_ref": "expired"}),
    )
    .await;
    assert_eq!(stale["isError"], true, "{stale}");
    assert_eq!(stale["structuredContent"]["status"], "not_started");
    assert_eq!(backend.recorded(|r| r.switches), 2);
    harness.finish().await;
}

/// F-23, F-26: `display_id` is optional everywhere and means the primary display.
#[tokio::test]
async fn omitted_display_id_means_the_primary_display() {
    for name in [
        CAPTURE_DISPLAY,
        CAPTURE_REGION,
        WAIT_FOR_VISUAL_CHANGE,
        CAPTURE_VISUAL_BASELINE,
        READ_TEXT_IN_REGION,
        FIND_TEXT_ON_SCREEN,
        WAIT_FOR_TEXT,
        CLICK_TEXT,
        MOVE_MOUSE,
        CLICK_MOUSE,
        SCROLL_MOUSE,
    ] {
        let schema = &tool_named(name).input_schema;
        assert!(
            !schema["required"].to_string().contains("display_id"),
            "{name} must not require display_id"
        );
    }
    let backend = ContractBackend::new();
    let mut harness = harness(backend.clone()).await;
    call(&mut harness, CAPTURE_DISPLAY, json!({})).await;
    call(
        &mut harness,
        CAPTURE_DISPLAY,
        json!({"display_id": "PRIMARY"}),
    )
    .await;
    call(
        &mut harness,
        CAPTURE_DISPLAY,
        json!({"display_id": "second"}),
    )
    .await;
    call(
        &mut harness,
        CAPTURE_REGION,
        json!({"x": 0, "y": 0, "width": 10, "height": 10}),
    )
    .await;
    call(
        &mut harness,
        BEGIN_CONTROL_SESSION,
        json!({"target_ref": "target-00000000-0000-0000-0000-000000000001"}),
    )
    .await;
    let clicked = call(
        &mut harness,
        CLICK_MOUSE,
        json!({"target_ref": "target-00000000-0000-0000-0000-000000000001", "x": 3, "y": 4,
               "observation": {"mode": "screenshot", "region": {"x": 0, "y": 0, "width": 5, "height": 5}}}),
    )
    .await;
    assert_ne!(clicked["isError"], true, "{clicked}");
    let ids = backend.recorded(|r| {
        r.captures
            .iter()
            .map(|request| request.display_id.clone())
            .collect::<Vec<_>>()
    });
    assert_eq!(ids, ["main", "main", "second"]);
    assert_eq!(
        backend.recorded(|r| r.regions[0].display_id.clone()),
        "main"
    );
    let click = backend.recorded(|r| r.clicks[0].clone());
    assert_eq!(click.display_id, "main");
    assert_eq!(click.button, MouseButton::Left);
    assert_eq!(click.observation.region.unwrap().display_id, "main");
    harness.finish().await;
}

/// F-24: explicit captures default to the action-screenshot width, with native as an opt-in.
#[tokio::test]
async fn explicit_captures_default_to_a_model_friendly_width() {
    let backend = ContractBackend::new();
    let mut harness = harness(backend.clone()).await;
    call(&mut harness, CAPTURE_DISPLAY, json!({})).await;
    call(&mut harness, CAPTURE_DISPLAY, json!({"native": true})).await;
    call(&mut harness, CAPTURE_DISPLAY, json!({"max_width": 640})).await;
    let both = call(
        &mut harness,
        CAPTURE_DISPLAY,
        json!({"native": true, "max_width": 640}),
    )
    .await;
    assert_eq!(both["isError"], true, "{both}");
    let widths = backend.recorded(|r| {
        r.captures
            .iter()
            .map(|request| request.max_width)
            .collect::<Vec<_>>()
    });
    assert_eq!(widths, [Some(1280), None, Some(640)]);
    let window = call(
        &mut harness,
        CAPTURE_WINDOW,
        json!({"window_id": "target-00000000-0000-0000-0000-000000000001"}),
    )
    .await;
    assert_eq!(window["structuredContent"]["image_width"], 400);
    assert_eq!(
        controlfreak_core::limits::DEFAULT_CAPTURE_WIDTH,
        1280,
        "the default matches action screenshots"
    );
    harness.finish().await;
}

/// F-56: every wait tool shares one lower bound for `timeout_ms`, and zero is accepted.
#[tokio::test]
async fn every_wait_tool_accepts_the_same_timeout_range() {
    for name in [
        WAIT_FOR_VISUAL_CHANGE,
        WAIT_FOR_CHANGE_SINCE,
        WAIT_FOR_TEXT,
        WAIT_FOR_WINDOW,
    ] {
        let timeout = &tool_named(name).input_schema["properties"]["timeout_ms"];
        assert_eq!(
            timeout["minimum"],
            controlfreak_core::limits::MIN_WAIT_MS,
            "{name}"
        );
        assert_eq!(
            timeout["maximum"],
            controlfreak_core::limits::MAX_WAIT_MS,
            "{name}"
        );
    }
    let backend = ContractBackend::new();
    let mut harness = harness(backend.clone()).await;
    call(
        &mut harness,
        WAIT_FOR_VISUAL_CHANGE,
        json!({"x": 0, "y": 0, "width": 5, "height": 5, "timeout_ms": 0}),
    )
    .await;
    let request = backend.recorded(|r| r.visual_waits[0].clone());
    assert_eq!((request.timeout_ms, request.stable_ms), (1, 0));
    // A short timeout works without also shortening the default stability period.
    call(
        &mut harness,
        WAIT_FOR_VISUAL_CHANGE,
        json!({"x": 0, "y": 0, "width": 5, "height": 5, "timeout_ms": 100}),
    )
    .await;
    let request = backend.recorded(|r| r.visual_waits[1].clone());
    assert_eq!((request.timeout_ms, request.stable_ms), (100, 100));
    assert_eq!(request.display_id, "main");
    harness.finish().await;
}

/// F-26: `run_actions` has a bounded wait step.
#[tokio::test]
async fn run_actions_waits_are_bounded_and_never_last() {
    let backend = ContractBackend::new();
    let mut harness = harness(backend.clone()).await;
    let run = |steps: Value| json!({"target_ref": "target-00000000-0000-0000-0000-000000000001", "steps": steps});
    let ok = call(
        &mut harness,
        RUN_ACTIONS,
        run(json!([
            {"action": "press_keys", "keys": ["a"]},
            {"action": "wait", "wait_ms": 30},
            {"action": "press_keys", "keys": ["b"]}
        ])),
    )
    .await;
    assert_ne!(ok["isError"], true, "{ok}");
    let steps = ok["structuredContent"]["action"]["steps"]
        .as_array()
        .unwrap()
        .clone();
    let kinds: Vec<_> = steps
        .iter()
        .map(|step| step["action"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["press_keys", "wait", "press_keys"]);
    assert!(steps.iter().all(|step| step["status"] == "completed"));
    assert_eq!(backend.recorded(|r| r.key_chords.len()), 2);
    for (steps, reason) in [
        (
            json!([{"action": "wait", "wait_ms": 10}]),
            "cannot be a wait",
        ),
        (
            json!([{"action": "wait", "wait_ms": 5001}, {"action": "press_keys", "keys": ["a"]}]),
            "wait_ms must be between",
        ),
        (
            json!([{"action": "wait", "wait_ms": 0}, {"action": "press_keys", "keys": ["a"]}]),
            "wait_ms must be between",
        ),
        (
            json!([
                {"action": "wait", "wait_ms": 5000}, {"action": "wait", "wait_ms": 5000},
                {"action": "wait", "wait_ms": 5000}, {"action": "wait", "wait_ms": 1},
                {"action": "press_keys", "keys": ["a"]}
            ]),
            "may not exceed",
        ),
    ] {
        let before = backend.recorded(|r| r.key_chords.len());
        let refused = call(&mut harness, RUN_ACTIONS, run(steps)).await;
        assert_eq!(refused["isError"], true, "{refused}");
        assert!(refused.to_string().contains(reason), "{refused}");
        assert_eq!(refused["structuredContent"]["status"], "not_started");
        assert_eq!(backend.recorded(|r| r.key_chords.len()), before);
    }
    harness.finish().await;
}

/// F-43: observation tools keep working after a stop, and input stays refused.
#[tokio::test]
async fn observation_tools_stay_available_after_a_stop() {
    let mut harness = harness(Arc::new(ObservationBackend)).await;
    let stopped = call(&mut harness, STOP_DESKTOP_WORK, json!({})).await;
    assert_ne!(stopped["isError"], true, "{stopped}");
    let region = json!({"display_id": "synthetic", "x": 0, "y": 0, "width": 10, "height": 10});
    let mut find = region.clone();
    find["query"] = json!("synthetic");
    let calls = [
        (READ_TEXT_IN_REGION, region.clone()),
        (FIND_TEXT_ON_SCREEN, find.clone()),
        (WAIT_FOR_TEXT, find),
        (WAIT_FOR_WINDOW, json!({"title_contains": "synthetic"})),
        (WAIT_FOR_VISUAL_CHANGE, region),
        (
            WAIT_FOR_CHANGE_SINCE,
            json!({"baseline_id": "vb-00000000000000000000000000000000-0000000000000000"}),
        ),
        (LIST_DISPLAYS, json!({})),
        (CAPTURE_DISPLAY, json!({"display_id": "synthetic"})),
    ];
    for (name, arguments) in calls {
        let result = call(&mut harness, name, arguments).await;
        assert_ne!(
            result["isError"], true,
            "{name} must stay available: {result}"
        );
    }
    let refused = call(
        &mut harness,
        CLICK_MOUSE,
        json!({"target_ref": "target-00000000-0000-0000-0000-000000000004", "x": 0, "y": 0}),
    )
    .await;
    assert_eq!(refused["isError"], true, "{refused}");
    assert_eq!(
        refused["structuredContent"]["error"]["code"],
        "desktop_work_stopped"
    );
    harness.finish().await;
}

/// F-47: an image of one window maps points only while that window is unchanged.
#[tokio::test]
async fn a_window_image_stops_mapping_points_once_its_window_changed() {
    let backend = ContractBackend::new();
    let mut harness = harness(backend.clone()).await;
    call(
        &mut harness,
        BEGIN_CONTROL_SESSION,
        json!({"target_ref": "target-00000000-0000-0000-0000-000000000001"}),
    )
    .await;
    let capture = call(
        &mut harness,
        CAPTURE_WINDOW,
        json!({"window_id": "target-00000000-0000-0000-0000-000000000001"}),
    )
    .await;
    let observation_id = capture["structuredContent"]["observation_id"].clone();
    let click = json!({"target_ref": "target-00000000-0000-0000-0000-000000000001", "observation_id": observation_id, "image_x": 10, "image_y": 10});
    let first = call(&mut harness, CLICK_MOUSE, click.clone()).await;
    assert_ne!(first["isError"], true, "{first}");
    // The window moves: its reference changes, so the old one is no longer listed.
    backend.windows.lock().unwrap()[0].id = "w1-moved".into();
    let stale = call(&mut harness, CLICK_MOUSE, click).await;
    assert_eq!(stale["isError"], true, "{stale}");
    assert!(
        stale.to_string().contains("moved, resized or closed"),
        "{stale}"
    );
    assert_eq!(stale["structuredContent"]["status"], "not_started");
    assert_eq!(backend.recorded(|r| r.clicks.len()), 1);
    harness.finish().await;
}

/// The window check does not depend on the capped `list_windows`.
#[tokio::test]
async fn a_window_image_is_checked_without_the_capped_window_list() {
    let backend = ContractBackend::new();
    *backend.list_cap.lock().unwrap() = Some(0);
    let mut harness = harness(backend.clone()).await;
    let target = "target-00000000-0000-0000-0000-000000000001";
    call(
        &mut harness,
        BEGIN_CONTROL_SESSION,
        json!({"target_ref": target}),
    )
    .await;
    let capture = call(&mut harness, CAPTURE_WINDOW, json!({"window_id": target})).await;
    let click = json!({"target_ref": target, "observation_id": capture["structuredContent"]["observation_id"], "image_x": 10, "image_y": 10});
    let result = call(&mut harness, CLICK_MOUSE, click).await;
    assert_ne!(result["isError"], true, "{result}");
    harness.finish().await;
}

/// F-20: an unknown key names the closest key and the valid ones, and every advertised key parses.
#[test]
fn unknown_key_errors_suggest_a_name_and_advertised_keys_parse() {
    let error = parse_arguments::<PressKeysInput>(Some(rmcp::object!({
        "target_ref": "target-00000000-0000-0000-0000-000000000001", "keys": ["ctlr"]
    })))
    .unwrap_err();
    assert!(
        error.message.contains("did you mean 'ctrl'?"),
        "{}",
        error.message
    );
    assert_eq!(
        error.message.matches("unknown key").count(),
        1,
        "{}",
        error.message
    );
    let far = parse_arguments::<PressKeysInput>(Some(rmcp::object!({
        "target_ref": "target-00000000-0000-0000-0000-000000000001", "keys": ["launch_browser"]
    })))
    .unwrap_err();
    assert!(!far.message.contains("did you mean"), "{}", far.message);
    assert!(far.message.contains("valid names:") && far.message.contains("page_down"));
    assert_eq!(
        far.message.matches("unknown key").count(),
        1,
        "{}",
        far.message
    );
    for name in schema::all_key_names() {
        assert!(
            serde_json::from_value::<controlfreak_core::Key>(json!(name)).is_ok(),
            "advertised key {name} must parse"
        );
    }
    let description = tool_named(PRESS_KEYS).input_schema["properties"]["keys"]["description"]
        .as_str()
        .unwrap()
        .to_owned();
    for name in [
        "ctrl",
        "page_up",
        "arrow_left",
        "f1-f12",
        "a-z",
        "comma",
        "numpad_0",
        "volume_up",
        "print_screen",
    ] {
        assert!(description.contains(name), "{description}");
    }
}

/// F-05, F-06, F-13: the instructions and descriptions lead with preconditions and recovery.
#[test]
fn guidance_states_the_foreground_rule_dialogs_and_the_permanent_stop() {
    assert!(
        SERVER_INSTRUCTIONS.len() < 2_000,
        "{}",
        SERVER_INSTRUCTIONS.len()
    );
    for needed in [
        "foreground",
        "Dialogs and menus of the same application",
        "new session",
        "primary display",
        "stop_desktop_work is an emergency stop that closes input permanently",
        "click the window",
        "1280",
    ] {
        assert!(SERVER_INSTRUCTIONS.contains(needed), "missing: {needed}");
    }
    let begin = tool_named(BEGIN_CONTROL_SESSION);
    let description = begin.description.as_deref().unwrap();
    assert!(description.contains("ONE window") && description.contains("foreground"));
    let hold = &begin.input_schema["properties"]["expected_seconds"]["description"];
    assert!(hold.as_str().unwrap().contains("45") && hold.as_str().unwrap().contains("120"));
    for name in [PRESS_KEYS, CLICK_MOUSE, TYPE_TEXT, CLICK_TEXT] {
        let description = tool_named(name).description.unwrap();
        assert!(description.contains("foreground"), "{name}: {description}");
    }
}

/// F-56: `click_mouse` defaults to the left button like `click_text` and `run_actions`.
#[test]
fn click_mouse_defaults_to_the_left_button() {
    let input = parse_arguments::<ClickMouseInput>(Some(rmcp::object!({
        "target_ref": "target-00000000-0000-0000-0000-000000000001", "x": 1, "y": 1
    })))
    .unwrap();
    assert_eq!(input.button, MouseButton::Left);
    let schema = &tool_named(CLICK_MOUSE).input_schema;
    assert!(!schema["required"].to_string().contains("button"));
    assert_eq!(schema["properties"]["button"]["default"], "left");
    let drag = &tool_named(DRAG_MOUSE).input_schema;
    assert_eq!(drag["properties"]["button"]["default"], "left");
}

/// F-01: the whole list stays small enough to send on every session.
#[test]
fn tool_list_stays_compact() {
    let list = serde_json::to_string(&tools()).unwrap();
    // Measured with `--print-tools`: 115,810 bytes before the compaction, 99,477 after. The
    // limit leaves about 3% of headroom, so a reworded description does not fail this test but
    // a schema that is pasted into every tool does.
    assert!(list.len() < 102_400, "tools/list is {} bytes", list.len());
    for tool in tools() {
        let output = Value::Object((*tool.output_schema.unwrap()).clone()).to_string();
        assert!(
            !output.contains("\"description\""),
            "{} output schema carries prose",
            tool.name
        );
    }
}
