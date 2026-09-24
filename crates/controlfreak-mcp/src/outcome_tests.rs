use super::*;
use controlfreak_core::{
    BackendMetadata, DisplayBackend, KeyboardBackend, OcrBackend, PointerBackend,
    VirtualDesktopDirection, WindowBackend,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

struct SyntheticBackend(AtomicU8);

impl SyntheticBackend {
    fn act<T>(&self, control: &MutationControl, value: T) -> Result<T, PlatformError> {
        let failure = || PlatformError::OperationFailed {
            operation: "synthetic".into(),
            reason: "injected failure".into(),
        };
        if self.0.load(Ordering::Relaxed) == 4 {
            return Err(failure());
        }
        control.dispatch_started();
        control.dispatch_accepted(100);
        match self.0.load(Ordering::Relaxed) {
            2 => return Err(failure()),
            3 => {
                control.dispatch_started();
                panic!("injected worker failure");
            }
            _ => {}
        }
        control.input_complete();
        if self.0.load(Ordering::Relaxed) == 1 {
            return Err(PlatformError::PostActionObservationFailed {
                operation: "synthetic".into(),
                reason: "injected capture failure".into(),
            });
        }
        Ok(value)
    }
}

impl BackendMetadata for SyntheticBackend {
    fn validate_target_reference(&self, target: &str) -> Result<(), PlatformError> {
        if target == "synthetic" {
            Ok(())
        } else {
            Err(PlatformError::TargetInvalidated {
                reason: "synthetic stale target".into(),
            })
        }
    }
    fn identity(&self) -> controlfreak_core::BackendIdentity {
        controlfreak_core::BackendIdentity {
            platform: controlfreak_core::Platform::Windows,
            display_server: controlfreak_core::DisplayServer::Dwm,
            backend_id: "synthetic".into(),
        }
    }
    fn capabilities(&self) -> Vec<controlfreak_core::CapabilityDescriptor> {
        vec![]
    }
    fn permissions(&self) -> Vec<controlfreak_core::PermissionDescriptor> {
        vec![]
    }
}
impl DisplayBackend for SyntheticBackend {}

fn observation() -> ActionObservation {
    ActionObservation {
        foreground_window: None,
        screenshot: None,
    }
}
fn pointer() -> PointerActionResult {
    PointerActionResult {
        position: controlfreak_core::MousePosition {
            display_id: "synthetic".into(),
            local_x: 0,
            local_y: 0,
            virtual_x: -100,
            virtual_y: 0,
        },
        observation: observation(),
    }
}
macro_rules! pointer_method {
    ($name:ident, $request:ty) => {
        fn $name(
            &self,
            _: &$request,
            control: &MutationControl,
        ) -> Result<PointerActionResult, PlatformError> {
            self.act(control, pointer())
        }
    };
}
impl PointerBackend for SyntheticBackend {
    pointer_method!(move_mouse_controlled, MouseMoveRequest);
    pointer_method!(click_mouse_controlled, MouseClickRequest);
    pointer_method!(drag_mouse_controlled, MouseDragRequest);
    pointer_method!(scroll_mouse_controlled, MouseScrollRequest);
}
impl KeyboardBackend for SyntheticBackend {
    fn press_keys_controlled(
        &self,
        _: &KeyChordRequest,
        control: &MutationControl,
    ) -> Result<KeyboardActionResult, PlatformError> {
        self.act(
            control,
            KeyboardActionResult {
                observation: observation(),
            },
        )
    }
    fn type_text_controlled(
        &self,
        _: &TextInputRequest,
        control: &MutationControl,
    ) -> Result<KeyboardActionResult, PlatformError> {
        self.act(
            control,
            KeyboardActionResult {
                observation: observation(),
            },
        )
    }
}
impl OcrBackend for SyntheticBackend {
    fn click_text_controlled(
        &self,
        _: &ClickTextRequest,
        control: &MutationControl,
    ) -> Result<ClickTextResult, PlatformError> {
        let mode = self.0.load(Ordering::Relaxed);
        if mode >= 5 {
            let region = controlfreak_core::OcrRegionRequest {
                display_id: "synthetic".into(),
                x: 0,
                y: 0,
                width: 10,
                height: 10,
                language: None,
            };
            let line = controlfreak_core::OcrLine {
                text: "synthetic".into(),
                x: 1,
                y: 1,
                width: 2,
                height: 2,
                words: vec![],
            };
            let lines = if mode == 5 {
                vec![]
            } else {
                vec![line.clone(), line]
            };
            controlfreak_core::select_text_candidate(&lines, &region, "synthetic", false, true)?;
        }
        self.act(
            control,
            ClickTextResult {
                position: pointer().position,
                observation: observation(),
                matched_text: controlfreak_core::TextMatch {
                    text: "synthetic".into(),
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                },
            },
        )
    }
}
impl WindowBackend for SyntheticBackend {
    fn focus_window_controlled(
        &self,
        _: &FocusWindowRequest,
        control: &MutationControl,
    ) -> Result<WindowFocusResult, PlatformError> {
        self.act(
            control,
            WindowFocusResult {
                attempts: 1,
                elapsed_ms: 0,
                window: controlfreak_core::WindowInfo {
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
                },
                observation: observation(),
            },
        )
    }
    fn switch_virtual_desktop_controlled(
        &self,
        _: &VirtualDesktopSwitchRequest,
        control: &MutationControl,
    ) -> Result<VirtualDesktopSwitchResult, PlatformError> {
        self.act(
            control,
            VirtualDesktopSwitchResult {
                direction: VirtualDesktopDirection::Right,
                requested_steps: 1,
                changed: false,
                observation: observation(),
            },
        )
    }
}

pub(super) async fn exchange(
    client: &mut BufReader<tokio::io::DuplexStream>,
    request: Value,
) -> Value {
    client
        .get_mut()
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(10), client.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    serde_json::from_str(&line).unwrap()
}

#[tokio::test]
async fn every_action_result_validates_against_tools_list_over_transport() {
    let backend = Arc::new(SyntheticBackend(AtomicU8::new(0)));
    let server =
        ControlFreakServer::with_indicator(backend.clone(), SafetyIndicator::dormant(), None);
    let (client_io, server_io) = tokio::io::duplex(256 * 1024);
    let serving = tokio::spawn(async move {
        server
            .serve(server_io)
            .await
            .unwrap()
            .waiting()
            .await
            .unwrap()
    });
    let mut client = BufReader::new(client_io);
    exchange(&mut client, json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
        "protocolVersion":"2025-11-25", "capabilities":{}, "clientInfo":{"name":"schema-test","version":"1"}
    }})).await;
    client
        .get_mut()
        .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
        .await
        .unwrap();
    let listed = exchange(
        &mut client,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
    )
    .await;
    let tools = listed["result"]["tools"].as_array().unwrap();
    assert_window_input_schemas(tools);
    let actions = action_requests();
    for (mode, status) in [
        "completed",
        "completed_unverified",
        "partially_sent",
        "unknown",
        "not_started",
    ]
    .into_iter()
    .enumerate()
    {
        backend
            .0
            .store(u8::try_from(mode).unwrap(), Ordering::Relaxed);
        for (name, arguments) in &actions {
            let schema = &tools.iter().find(|tool| tool["name"] == *name).unwrap()["outputSchema"];
            let validator = jsonschema::validator_for(schema).unwrap();
            let response = exchange(&mut client, json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":name,"arguments":arguments}})).await;
            validate_action_response(&response, &validator, name, status, mode);
        }
        let diagnostics = exchange(&mut client, json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":GET_SERVER_STATUS,"arguments":{}}})).await;
        let recent = diagnostics["result"]["structuredContent"]["recent_operations"]
            .as_array()
            .unwrap();
        assert_eq!(recent.last().unwrap()["status"], status);
    }
    for (mode, code, count) in [(5, "ocr_no_match", 0), (6, "ocr_ambiguous_match", 2)] {
        backend.0.store(mode, Ordering::Relaxed);
        let schema = &tools
            .iter()
            .find(|tool| tool["name"] == CLICK_TEXT)
            .unwrap()["outputSchema"];
        let validator = jsonschema::validator_for(schema).unwrap();
        let arguments = &actions
            .iter()
            .find(|(name, _)| *name == CLICK_TEXT)
            .unwrap()
            .1;
        let response = exchange(&mut client, json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":CLICK_TEXT,"arguments":arguments}})).await;
        validate_action_response(&response, &validator, CLICK_TEXT, "not_started", 4);
        let content = &response["result"]["structuredContent"];
        assert_eq!(content["error"]["code"], code);
        assert_eq!(content["error"]["details"]["candidate_count"], count);
        assert_eq!(content["input"]["input_outcome"], "not_started");
    }
    backend.0.store(0, Ordering::Relaxed);
    assert_unapproved_actions_rejected(&mut client, &actions).await;
    drop(client);
    tokio::time::timeout(Duration::from_secs(10), serving)
        .await
        .unwrap()
        .unwrap();
}

fn assert_window_input_schemas(tools: &[Value]) {
    let reference = "target-12345678-1234-1234-1234-123456789ABC";
    for name in [FOCUS_WINDOW, CAPTURE_WINDOW, WAIT_FOR_WINDOW] {
        let schema = &tools.iter().find(|tool| tool["name"] == name).unwrap()["inputSchema"];
        let validator = jsonschema::validator_for(schema).unwrap();
        assert!(
            validator.is_valid(&json!({"window_id": reference})),
            "{name}"
        );
        assert!(!validator.is_valid(&json!({"window_id": ""})), "{name}");
    }
}

async fn assert_unapproved_actions_rejected(
    client: &mut BufReader<tokio::io::DuplexStream>,
    actions: &[(&str, Value)],
) {
    for (name, arguments) in actions {
        for supplied in [None, Some("0X10:20"), Some("expired-target")] {
            let mut arguments = arguments.clone();
            let field = if *name == FOCUS_WINDOW {
                "window_id"
            } else {
                "target_ref"
            };
            arguments.as_object_mut().unwrap().remove(field);
            if let Some(target) = supplied {
                arguments[field] = json!(target);
            }
            let response = exchange(client, json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":name,"arguments":arguments}})).await;
            let content = &response["result"]["structuredContent"];
            assert_eq!(
                content["input"]["input_outcome"], "not_started",
                "{name}: {response}"
            );
            assert_eq!(content["input"]["sent_events"], 0, "{name}: {response}");
            assert_eq!(response["result"]["isError"], true);
        }
    }
}

fn action_requests() -> [(&'static str, Value); 9] {
    [
        (MOVE_MOUSE, json!({"display_id":"synthetic","x":0,"y":0})),
        (
            CLICK_MOUSE,
            json!({"display_id":"synthetic","x":0,"y":0,"button":"left"}),
        ),
        (
            SCROLL_MOUSE,
            json!({"display_id":"synthetic","x":0,"y":0,"delta_y":120}),
        ),
        (
            DRAG_MOUSE,
            json!({"start_display_id":"synthetic","start_x":0,"start_y":0,"end_display_id":"synthetic","end_x":1,"end_y":1,"button":"left"}),
        ),
        (
            CLICK_TEXT,
            json!({"display_id":"synthetic","x":0,"y":0,"width":10,"height":10,"query":"synthetic"}),
        ),
        (PRESS_KEYS, json!({"keys":["enter"]})),
        (TYPE_TEXT, json!({"text":"synthetic"})),
        (FOCUS_WINDOW, json!({"window_id":"synthetic"})),
        (SWITCH_VIRTUAL_DESKTOP, json!({"direction":"right"})),
    ].map(|(name, mut arguments)| {
        if name != FOCUS_WINDOW { arguments["target_ref"] = json!("synthetic"); }
        (name, arguments)
    })
}

fn validate_action_response(
    response: &Value,
    validator: &jsonschema::Validator,
    name: &str,
    status: &str,
    mode: usize,
) {
    let result = &response["result"];
    let content = &result["structuredContent"];
    assert_eq!(content["status"], status, "{name}: {response}");
    assert!(
        validator.is_valid(content),
        "{name}: {content}: {:?}",
        validator.iter_errors(content).collect::<Vec<_>>()
    );
    if mode == 0 && name == CLICK_TEXT {
        assert_eq!(content["action"]["match_tier"], "exact");
        assert_eq!(content["action"]["matched_text"]["width"], 1);
        assert_eq!(content["action"]["matched_text"]["height"], 1);
    }
    assert_eq!(content["retry_action"], false);
    assert_eq!(content["effect_verification"], "unverified");
    assert_eq!(
        content["input"]["sent_events"],
        if mode == 4 { 0 } else { 100 }
    );
    assert_eq!(result["isError"] == true, mode == 4);
    let text = result["content"]
        .as_array()
        .unwrap()
        .iter()
        .find(|block| block["type"] == "text")
        .unwrap()["text"]
        .as_str()
        .unwrap();
    assert_eq!(serde_json::from_str::<Value>(text).unwrap(), *content);
    let mut invalid = content.clone();
    invalid["unexpected"] = json!(true);
    assert!(!validator.is_valid(&invalid));
    invalid = content.clone();
    invalid.as_object_mut().unwrap().remove("input");
    assert!(!validator.is_valid(&invalid));
}
