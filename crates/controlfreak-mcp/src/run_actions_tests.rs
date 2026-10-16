use super::*;
use controlfreak_core::{
    BackendIdentity, BackendMetadata, CapabilityDescriptor, DisplayBackend, DisplayBounds,
    DisplayInfo, DisplayServer, KeyboardBackend, MousePosition, OcrBackend, PermissionDescriptor,
    Platform, PointerBackend, WindowBackend,
};
use tokio::io::{AsyncWriteExt, BufReader};

/// Records each dispatched step and fails the step whose zero-based number is `fail_at`.
struct Scripted {
    fail_at: Option<usize>,
    calls: Mutex<Vec<String>>,
}

impl Scripted {
    fn step(&self, label: String, control: &MutationControl) -> Result<(), PlatformError> {
        let mut calls = self.calls.lock().unwrap();
        let index = calls.len();
        calls.push(label);
        drop(calls);
        if self.fail_at == Some(index) {
            return Err(PlatformError::TargetInvalidated {
                reason: "synthetic focus theft before dispatch".into(),
            });
        }
        control.dispatch_started();
        control.dispatch_accepted(2);
        control.input_complete();
        Ok(())
    }
}

fn observation() -> ActionObservation {
    ActionObservation {
        foreground_window: None,
        screenshot: None,
    }
}

fn pointer() -> PointerActionResult {
    PointerActionResult {
        position: MousePosition {
            display_id: "fixture".into(),
            local_x: 0,
            local_y: 0,
            virtual_x: 0,
            virtual_y: 0,
        },
        observation: observation(),
    }
}

impl BackendMetadata for Scripted {
    fn validate_target_reference(&self, _: &str) -> Result<(), PlatformError> {
        Ok(())
    }
    fn identity(&self) -> BackendIdentity {
        BackendIdentity {
            platform: Platform::Windows,
            display_server: DisplayServer::Dwm,
            backend_id: "run-actions-fixture".into(),
        }
    }
    fn capabilities(&self) -> Vec<CapabilityDescriptor> {
        vec![]
    }
    fn permissions(&self) -> Vec<PermissionDescriptor> {
        vec![]
    }
}

impl DisplayBackend for Scripted {
    fn list_displays(&self) -> Result<Vec<DisplayInfo>, PlatformError> {
        Ok(vec![DisplayInfo {
            id: "fixture".into(),
            name: "fixture".into(),
            bounds: DisplayBounds {
                left: -1920,
                top: 0,
                width: 1920,
                height: 1080,
            },
            is_primary: true,
        }])
    }
}

impl PointerBackend for Scripted {
    fn click_mouse_controlled(
        &self,
        request: &MouseClickRequest,
        control: &MutationControl,
    ) -> Result<PointerActionResult, PlatformError> {
        self.step(
            format!("click {} {} {}", request.display_id, request.x, request.y),
            control,
        )
        .map(|()| pointer())
    }
    fn scroll_mouse_controlled(
        &self,
        request: &MouseScrollRequest,
        control: &MutationControl,
    ) -> Result<PointerActionResult, PlatformError> {
        self.step(format!("scroll {}", request.delta_y), control)
            .map(|()| pointer())
    }
}

impl KeyboardBackend for Scripted {
    fn type_text_controlled(
        &self,
        request: &TextInputRequest,
        control: &MutationControl,
    ) -> Result<KeyboardActionResult, PlatformError> {
        self.step(
            format!("type {} {:?}", request.text, request.observation.mode),
            control,
        )
        .map(|()| KeyboardActionResult {
            observation: observation(),
        })
    }
    fn press_keys_controlled(
        &self,
        request: &KeyChordRequest,
        control: &MutationControl,
    ) -> Result<KeyboardActionResult, PlatformError> {
        self.step(
            format!("keys {:?} {:?}", request.keys, request.observation.mode),
            control,
        )
        .map(|()| KeyboardActionResult {
            observation: observation(),
        })
    }
}

impl OcrBackend for Scripted {}
impl WindowBackend for Scripted {}

async fn run(fail_at: Option<usize>, arguments: Value) -> (Value, Vec<String>) {
    let backend = Arc::new(Scripted {
        fail_at,
        calls: Mutex::new(Vec::new()),
    });
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
    let exchange = super::outcome_tests::exchange;
    exchange(&mut client, json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"run-actions","version":"1"}}})).await;
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
    let tool = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == RUN_ACTIONS)
        .unwrap()
        .clone();
    let response = exchange(
        &mut client,
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":RUN_ACTIONS,"arguments":arguments}}),
    )
    .await;
    let content = response["result"]["structuredContent"].clone();
    assert!(
        jsonschema::validator_for(&tool["outputSchema"])
            .unwrap()
            .is_valid(&content),
        "{content}"
    );
    drop(client);
    tokio::time::timeout(Duration::from_secs(10), serving)
        .await
        .unwrap()
        .unwrap();
    let calls = backend.calls.lock().unwrap().clone();
    (content, calls)
}

fn steps() -> Value {
    json!([
        {"action": "click", "display_id": "fixture", "x": 10, "y": 20},
        {"action": "type_text", "text": "Ada"},
        {"action": "press_keys", "keys": ["tab"]},
        {"action": "scroll", "display_id": "fixture", "x": 10, "y": 20, "delta_y": -120}
    ])
}

#[tokio::test]
async fn steps_run_in_order_with_one_final_observation() {
    let (content, calls) = run(
        None,
        json!({"target_ref": "fixture", "steps": steps(), "observation": {"mode": "metadata"}}),
    )
    .await;
    assert_eq!(content["status"], "completed", "{content}");
    assert_eq!(content["input"]["sent_events"], 8);
    assert_eq!(
        calls,
        [
            "click fixture 10 20",
            "type Ada None",
            "keys [Tab] None",
            "scroll -120"
        ]
    );
    let reported: Vec<_> = content["action"]["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|step| (step["action"].clone(), step["status"].clone()))
        .collect();
    assert_eq!(reported.len(), 4);
    assert!(reported.iter().all(|(_, status)| status == "completed"));
}

#[tokio::test]
async fn a_failed_step_stops_the_batch_and_reports_partial_delivery() {
    let (content, calls) = run(Some(2), json!({"target_ref": "fixture", "steps": steps()})).await;
    assert_eq!(content["status"], "partially_sent", "{content}");
    assert_eq!(content["input"]["sent_events"], 4);
    assert_eq!(calls.len(), 3);
    let statuses: Vec<_> = content["error"]["details"]["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|step| step["status"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(statuses, ["completed", "completed", "failed", "skipped"]);
    assert_eq!(content["error"]["code"], "target_invalidated");
}

#[tokio::test]
async fn invalid_later_steps_are_refused_before_any_input() {
    for bad in [
        json!({"action": "type_text", "text": ""}),
        json!({"action": "scroll", "display_id": "fixture", "x": 1, "y": 1}),
        json!({"action": "click", "display_id": "fixture", "x": 1}),
        json!({"action": "click", "observation_id": "obs-unknown", "image_x": 1, "image_y": 1}),
        json!({"action": "click", "display_id": "fixture", "x": 1920, "y": 1}),
        json!({"action": "scroll", "display_id": "missing", "x": 1, "y": 1, "delta_y": 120}),
    ] {
        let mut all = steps();
        all.as_array_mut().unwrap().push(bad.clone());
        let (content, calls) = run(None, json!({"target_ref": "fixture", "steps": all})).await;
        assert_eq!(content["status"], "not_started", "{bad}: {content}");
        assert!(calls.is_empty(), "{bad}: {calls:?}");
    }
}
