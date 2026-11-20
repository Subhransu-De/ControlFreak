use super::*;
use controlfreak_core::{
    BackendIdentity, BackendMetadata, CapabilityDescriptor, DisplayBackend, DisplayBounds,
    DisplayInfo, DisplayServer, KeyboardBackend, MousePosition, OcrBackend, PermissionDescriptor,
    Platform, PointerBackend, WindowBackend,
};
use tokio::io::{AsyncWriteExt, BufReader};

/// Records each dispatched step and fails the step whose zero-based number is `fail_at` with
/// `failure`, before it dispatches anything.
struct Scripted {
    fail_at: Option<usize>,
    failure: PlatformError,
    /// Step whose input is delivered but whose post-action observation then fails.
    observation_fails_at: Option<usize>,
    /// The failing step first sends the high half of a surrogate pair, as typing that stops
    /// between the halves does.
    splits_pair: bool,
    calls: Mutex<Vec<String>>,
}

impl Scripted {
    fn step(&self, label: String, control: &MutationControl) -> Result<(), PlatformError> {
        let mut calls = self.calls.lock().unwrap();
        let index = calls.len();
        calls.push(label);
        drop(calls);
        if self.fail_at == Some(index) {
            if self.splits_pair {
                control.dispatch_started();
                control.dispatch_accepted(2);
                control.split_character();
            }
            return Err(self.failure.clone());
        }
        control.dispatch_started();
        control.dispatch_accepted(2);
        control.input_complete();
        if self.observation_fails_at == Some(index) {
            return Err(PlatformError::PostActionObservationFailed {
                operation: "synthetic".into(),
                reason: "display topology changed".into(),
            });
        }
        Ok(())
    }
}

fn observation() -> ActionObservation {
    ActionObservation {
        foreground_window: None,
        screenshot: None,
        settle: Some(controlfreak_core::SettleReport {
            outcome: controlfreak_core::SettleOutcome::Settled,
            elapsed_ms: 180,
        }),
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
        // The last step of the shared batch; it records the settle limit it was given.
        let settle = request
            .observation
            .settle_ms
            .map_or_else(String::new, |settle_ms| format!(" settle={settle_ms}"));
        self.step(format!("scroll {}{settle}", request.delta_y), control)
            .map(|()| pointer())
    }
}

impl controlfreak_core::AccessibilityBackend for Scripted {}

impl KeyboardBackend for Scripted {
    fn type_text_controlled(
        &self,
        request: &TextInputRequest,
        control: &MutationControl,
    ) -> Result<KeyboardActionResult, PlatformError> {
        let timeout = request
            .timeout_ms
            .map_or_else(String::new, |timeout_ms| format!(" timeout={timeout_ms}"));
        self.step(
            format!(
                "type {} {:?}{timeout}",
                request.text, request.observation.mode
            ),
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
    run_scripted(fail_at, None, arguments).await
}

async fn run_scripted(
    fail_at: Option<usize>,
    observation_fails_at: Option<usize>,
    arguments: Value,
) -> (Value, Vec<String>) {
    let failure = PlatformError::TargetInvalidated {
        reason: "synthetic focus theft before dispatch".into(),
    };
    run_failing(fail_at, failure, observation_fails_at, arguments).await
}

async fn run_failing(
    fail_at: Option<usize>,
    failure: PlatformError,
    observation_fails_at: Option<usize>,
    arguments: Value,
) -> (Value, Vec<String>) {
    let backend = Scripted {
        fail_at,
        failure,
        observation_fails_at,
        splits_pair: false,
        calls: Mutex::new(Vec::new()),
    };
    call(backend, RUN_ACTIONS, arguments).await
}

/// Calls `tool` once on `backend` and checks the result against the tool's output schema.
async fn call(backend: Scripted, tool: &str, arguments: Value) -> (Value, Vec<String>) {
    let backend = Arc::new(backend);
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
    let listed_tool = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|listed| listed["name"] == tool)
        .unwrap()
        .clone();
    let response = exchange(
        &mut client,
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":tool,"arguments":arguments}}),
    )
    .await;
    let content = response["result"]["structuredContent"].clone();
    assert!(
        jsonschema::validator_for(&listed_tool["outputSchema"])
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
        json!({"target_ref": "target-00000000-0000-0000-0000-000000000003", "steps": steps(), "observation": {"mode": "metadata"}}),
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
async fn each_step_reports_its_settle_and_only_the_last_gets_settle_ms() {
    let (content, calls) = run(
        None,
        json!({"target_ref": "target-00000000-0000-0000-0000-000000000003", "steps": steps(), "observation": {"mode": "metadata", "settle_ms": 1_500}}),
    )
    .await;
    assert_eq!(content["status"], "completed", "{content}");
    // Earlier steps take no screenshot and keep the short default limit.
    assert_eq!(
        calls,
        [
            "click fixture 10 20",
            "type Ada None",
            "keys [Tab] None",
            "scroll -120 settle=1500"
        ]
    );
    for step in content["action"]["steps"].as_array().unwrap() {
        assert_eq!(step["settle"]["outcome"], "settled", "{step}");
    }
    assert_eq!(content["observation"]["settle"]["elapsed_ms"], 180);
}

#[tokio::test]
async fn a_failed_step_stops_the_batch_and_reports_partial_delivery() {
    let (content, calls) = run(
        Some(2),
        json!({"target_ref": "target-00000000-0000-0000-0000-000000000003", "steps": steps()}),
    )
    .await;
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
async fn a_held_modifier_refusal_stops_the_batch_at_its_step() {
    let held = PlatformError::failed(
        controlfreak_core::FailureCause::KeyHeld,
        "press_keys",
        "a modifier key (Shift, Ctrl, Alt or Win) is held and would change this input; \
         release it before retrying",
    );
    for (fail_at, expected) in [
        (0, ["failed", "skipped", "skipped", "skipped"]),
        (2, ["completed", "completed", "failed", "skipped"]),
    ] {
        let (content, calls) = run_failing(
            Some(fail_at),
            held.clone(),
            None,
            json!({"target_ref": "target-00000000-0000-0000-0000-000000000003", "steps": steps()}),
        )
        .await;
        assert_eq!(calls.len(), fail_at + 1, "{content}");
        let error = &content["error"];
        assert_eq!(error["code"], "platform_operation_failed", "{content}");
        assert_eq!(error["reason"], "key_held");
        let steps = error["details"]["steps"].as_array().unwrap();
        let statuses: Vec<_> = steps
            .iter()
            .map(|step| step["status"].as_str().unwrap())
            .collect();
        assert_eq!(statuses, expected);
        // The refused step and every skipped step sent nothing.
        assert!(
            steps[fail_at..].iter().all(|step| step["sent_events"] == 0),
            "{content}"
        );
        let (status, sent) = if fail_at == 0 {
            ("not_started", 0)
        } else {
            ("partially_sent", 4)
        };
        assert_eq!(content["status"], status, "{content}");
        assert_eq!(content["input"]["sent_events"], sent);
    }
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
        let (content, calls) = run(
            None,
            json!({"target_ref": "target-00000000-0000-0000-0000-000000000003", "steps": all}),
        )
        .await;
        assert_eq!(content["status"], "not_started", "{bad}: {content}");
        assert!(calls.is_empty(), "{bad}: {calls:?}");
    }
}

#[tokio::test]
async fn final_observation_limits_are_refused_before_any_input() {
    for observation in [
        json!({"settle_ms": 10_001}),
        json!({"max_width": 0}),
        json!({"max_width": 7_681}),
    ] {
        let (content, calls) = run(
            None,
            json!({"target_ref": "target-00000000-0000-0000-0000-000000000003", "steps": steps(), "observation": observation}),
        )
        .await;
        assert_eq!(content["status"], "not_started", "{observation}: {content}");
        assert!(calls.is_empty(), "{observation}: {calls:?}");
    }
}

#[tokio::test]
async fn observation_failures_report_steps_and_true_delivery() {
    let statuses = |steps: &Value| -> Vec<String> {
        steps
            .as_array()
            .unwrap()
            .iter()
            .map(|step| step["status"].as_str().unwrap().to_owned())
            .collect()
    };
    // A middle step sent its input, then its check failed: later steps never ran.
    let (content, calls) = run_scripted(
        None,
        Some(1),
        json!({"target_ref": "target-00000000-0000-0000-0000-000000000003", "steps": steps()}),
    )
    .await;
    assert_eq!(content["status"], "partially_sent", "{content}");
    assert_eq!(calls.len(), 2);
    assert_eq!(
        statuses(&content["error"]["details"]["steps"]),
        ["completed", "failed", "skipped", "skipped"]
    );
    // Only the final evidence failed: every step was delivered.
    let (content, calls) = run_scripted(
        None,
        Some(3),
        json!({"target_ref": "target-00000000-0000-0000-0000-000000000003", "steps": steps()}),
    )
    .await;
    assert_eq!(content["status"], "completed_unverified", "{content}");
    assert_eq!(calls.len(), 4);
    assert_eq!(
        statuses(&content["details"]["steps"]),
        ["completed", "completed", "completed", "completed"]
    );
}

#[tokio::test]
async fn typing_that_stops_inside_a_surrogate_pair_reports_the_split_character() {
    let stopped = || {
        PlatformError::failed(
            controlfreak_core::FailureCause::Cancelled,
            "type_text",
            "the desktop operation was cancelled",
        )
    };
    let scripted = |fail_at| Scripted {
        fail_at: Some(fail_at),
        failure: stopped(),
        observation_fails_at: None,
        splits_pair: true,
        calls: Mutex::new(Vec::new()),
    };
    let target = "target-00000000-0000-0000-0000-000000000003";
    let (content, _) = call(
        scripted(0),
        TYPE_TEXT,
        json!({"target_ref": target, "text": "\u{1f980}"}),
    )
    .await;
    assert_eq!(content["status"], "partially_sent", "{content}");
    assert_eq!(content["input"]["split_character"], true, "{content}");
    assert_eq!(content["error"]["reason"], "cancelled", "{content}");

    let (content, calls) = call(
        scripted(1),
        RUN_ACTIONS,
        json!({"target_ref": target, "steps": steps()}),
    )
    .await;
    assert_eq!(calls.len(), 2, "{content}");
    assert_eq!(content["status"], "partially_sent", "{content}");
    assert_eq!(content["input"]["split_character"], true, "{content}");
    let reported = content["error"]["details"]["steps"].as_array().unwrap();
    assert_eq!(reported[1]["status"], "failed", "{content}");
    assert_eq!(reported[1]["split_character"], true, "{content}");
    // Only the step that split the pair says so.
    for index in [0, 2, 3] {
        assert!(
            reported[index].get("split_character").is_none(),
            "{content}"
        );
    }

    // Typing that stops elsewhere carries no split flag.
    let (content, _) = run_failing(
        Some(1),
        stopped(),
        None,
        json!({"target_ref": target, "steps": steps()}),
    )
    .await;
    assert!(
        content["input"].get("split_character").is_none(),
        "{content}"
    );
}

fn deadline_reached() -> PlatformError {
    PlatformError::failed(
        controlfreak_core::FailureCause::DeadlineReached,
        "type_text",
        "typing reached its timeout_ms before the text was finished",
    )
}

#[tokio::test]
async fn timeout_ms_reaches_the_backend_and_is_absent_by_default() {
    let target = "target-00000000-0000-0000-0000-000000000003";
    let scripted = || Scripted {
        fail_at: None,
        failure: deadline_reached(),
        observation_fails_at: None,
        splits_pair: false,
        calls: Mutex::new(Vec::new()),
    };
    let (content, calls) = call(
        scripted(),
        TYPE_TEXT,
        json!({"target_ref": target, "text": "Ada", "timeout_ms": 2_500, "observation": {"mode": "none"}}),
    )
    .await;
    assert_eq!(content["status"], "completed", "{content}");
    assert_eq!(calls, ["type Ada None timeout=2500"]);

    let mut timed = steps();
    timed[1]["timeout_ms"] = json!(300_000);
    let (content, calls) = call(
        scripted(),
        RUN_ACTIONS,
        json!({"target_ref": target, "steps": timed}),
    )
    .await;
    assert_eq!(content["status"], "completed", "{content}");
    assert_eq!(calls[1], "type Ada None timeout=300000");

    // Without timeout_ms the request carries no limit.
    let (_, calls) = run(None, json!({"target_ref": target, "steps": steps()})).await;
    assert_eq!(calls[1], "type Ada None");
}

#[tokio::test]
async fn a_typing_deadline_reports_partial_delivery_and_is_not_retryable() {
    let target = "target-00000000-0000-0000-0000-000000000003";
    // The deadline fell between the halves of a pair: the high half was sent.
    let (content, _) = call(
        Scripted {
            fail_at: Some(0),
            failure: deadline_reached(),
            observation_fails_at: None,
            splits_pair: true,
            calls: Mutex::new(Vec::new()),
        },
        TYPE_TEXT,
        json!({"target_ref": target, "text": "a\u{1f980}", "timeout_ms": 20}),
    )
    .await;
    assert_eq!(content["status"], "partially_sent", "{content}");
    assert_eq!(content["retry_action"], false, "{content}");
    assert_eq!(content["input"]["sent_events"], 2, "{content}");
    assert_eq!(content["input"]["split_character"], true, "{content}");
    let error = &content["error"];
    assert_eq!(error["code"], "platform_operation_failed", "{content}");
    assert_eq!(error["reason"], "deadline_reached", "{content}");
    assert_eq!(error["retryable"], false, "{content}");

    // In a batch the step that ran out of time fails and every later step is skipped.
    let mut timed = steps();
    timed[1]["timeout_ms"] = json!(20);
    let (content, calls) = run_failing(
        Some(1),
        deadline_reached(),
        None,
        json!({"target_ref": target, "steps": timed}),
    )
    .await;
    assert_eq!(calls, ["click fixture 10 20", "type Ada None timeout=20"]);
    assert_eq!(content["status"], "partially_sent", "{content}");
    assert_eq!(content["input"]["sent_events"], 2, "{content}");
    let error = &content["error"];
    assert_eq!(error["reason"], "deadline_reached", "{content}");
    assert_eq!(error["retryable"], false, "{content}");
    let statuses: Vec<_> = error["details"]["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|step| step["status"].as_str().unwrap())
        .collect();
    assert_eq!(statuses, ["completed", "failed", "skipped", "skipped"]);
}

#[tokio::test]
async fn an_out_of_range_timeout_is_refused_before_any_input() {
    let target = "target-00000000-0000-0000-0000-000000000003";
    for timeout_ms in [0, 300_001] {
        let scripted = Scripted {
            fail_at: None,
            failure: deadline_reached(),
            observation_fails_at: None,
            splits_pair: false,
            calls: Mutex::new(Vec::new()),
        };
        let (content, calls) = call(
            scripted,
            TYPE_TEXT,
            json!({"target_ref": target, "text": "Ada", "timeout_ms": timeout_ms}),
        )
        .await;
        assert_eq!(content["status"], "not_started", "{content}");
        assert_eq!(content["error"]["code"], "invalid_argument", "{content}");
        assert!(calls.is_empty(), "{calls:?}");

        let mut timed = steps();
        timed[1]["timeout_ms"] = json!(timeout_ms);
        let (content, calls) = run(None, json!({"target_ref": target, "steps": timed})).await;
        assert_eq!(content["status"], "not_started", "{content}");
        assert!(
            content
                .to_string()
                .contains("timeout_ms must be between 1 and 300000"),
            "{content}"
        );
        assert!(calls.is_empty(), "{calls:?}");
    }
}
