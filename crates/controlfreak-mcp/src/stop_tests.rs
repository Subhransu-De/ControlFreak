use super::*;
use controlfreak_core::{
    BackendMetadata, DisplayBackend, KeyboardBackend, OcrBackend, PointerBackend, WindowBackend,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub(super) struct WaitingBackend {
    started: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    cancelled: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}

impl WaitingBackend {
    pub(super) fn idle() -> Self {
        Self {
            started: Mutex::new(None),
            cancelled: Mutex::new(None),
        }
    }

    fn wait<T>(&self, control: &MutationControl) -> Result<T, PlatformError> {
        self.started
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .send(())
            .unwrap();
        let error = control
            .wait("synthetic_provider", Duration::from_secs(30))
            .unwrap_err();
        self.cancelled
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .send(())
            .unwrap();
        Err(error)
    }
}

impl BackendMetadata for WaitingBackend {
    fn validate_target_reference(&self, target: &str) -> Result<(), PlatformError> {
        assert_eq!(target, "fixture");
        Ok(())
    }

    fn identity(&self) -> controlfreak_core::BackendIdentity {
        controlfreak_core::BackendIdentity {
            platform: controlfreak_core::Platform::Windows,
            display_server: controlfreak_core::DisplayServer::Dwm,
            backend_id: "stop-fixture".to_owned(),
        }
    }
    fn capabilities(&self) -> Vec<controlfreak_core::CapabilityDescriptor> {
        vec![]
    }
    fn permissions(&self) -> Vec<controlfreak_core::PermissionDescriptor> {
        vec![]
    }
}

impl PointerBackend for WaitingBackend {
    fn move_mouse_controlled(
        &self,
        _: &MouseMoveRequest,
        control: &MutationControl,
    ) -> Result<PointerActionResult, PlatformError> {
        self.wait(control)
    }
}
impl KeyboardBackend for WaitingBackend {
    fn type_text_controlled(
        &self,
        _: &TextInputRequest,
        control: &MutationControl,
    ) -> Result<KeyboardActionResult, PlatformError> {
        self.wait(control)
    }
}
impl DisplayBackend for WaitingBackend {
    fn list_displays(&self) -> Result<Vec<controlfreak_core::DisplayInfo>, PlatformError> {
        Ok(vec![])
    }

    fn wait_for_visual_change_controlled(
        &self,
        _: &WaitForVisualChangeRequest,
        control: &MutationControl,
    ) -> Result<VisualChangeResult, PlatformError> {
        self.wait(control)
    }
}
impl OcrBackend for WaitingBackend {
    fn find_text_on_screen_controlled(
        &self,
        _: &controlfreak_core::FindTextRequest,
        control: &MutationControl,
    ) -> Result<controlfreak_core::FindTextResult, PlatformError> {
        self.wait(control)
    }

    fn read_text_in_region_controlled(
        &self,
        _: &OcrRegionRequest,
        control: &MutationControl,
    ) -> Result<controlfreak_core::OcrResult, PlatformError> {
        self.wait(control)
    }
}
impl WindowBackend for WaitingBackend {}

#[tokio::test]
async fn human_stop_notifies_idle_client_once_and_retains_reason() {
    let backend = Arc::new(WaitingBackend {
        started: Mutex::new(None),
        cancelled: Mutex::new(None),
    });
    let server = ControlFreakServer::with_indicator(backend, SafetyIndicator::dormant(), None);
    let stop = server.stop.clone();
    let (client_io, server_io) = tokio::io::duplex(65536);
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
    let initialized = outcome_tests::exchange(
        &mut client,
        json!({
            "jsonrpc":"2.0", "id":1, "method":"initialize", "params":{
                "protocolVersion":"2025-11-25", "capabilities":{},
                "clientInfo":{"name":"human-stop-test","version":"1"}
            }
        }),
    )
    .await;
    assert!(
        initialized["result"]["capabilities"]["experimental"]["controlfreak/user-stop"].is_object()
    );
    client
        .get_mut()
        .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
        .await
        .unwrap();
    stop.set_session_active(true);
    assert!(stop.stop_by_user());
    assert!(!stop.stop_by_user());
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(5), client.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    let notification: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(
        notification["method"],
        "notifications/controlfreak/session_stopped"
    );

    assert_eq!(notification["params"]["event"], "user_stopped_session");
    assert_eq!(notification["params"]["reason"], "user_stop");
    assert_eq!(notification["params"]["retry_action"], false);
    stop.set_session_active(false);
    let status = outcome_tests::exchange(
        &mut client,
        json!({
            "jsonrpc":"2.0", "id":2, "method":"tools/call",
            "params":{"name":GET_SERVER_STATUS,"arguments":{}}
        }),
    )
    .await;
    assert_eq!(
        status["id"], 2,
        "a repeated stop must not emit a second notification"
    );
    assert_eq!(
        status["result"]["structuredContent"]["stop_reason"],
        "user_stop"
    );
    let refused = outcome_tests::exchange(
        &mut client,
        json!({
            "jsonrpc":"2.0", "id":3, "method":"tools/call",
            "params":{"name":TYPE_TEXT,"arguments":{"text":"synthetic"}}
        }),
    )
    .await;
    assert_eq!(
        refused["result"]["structuredContent"]["retry_action"],
        false
    );
    assert!(refused.to_string().contains("the user stopped"));
    drop(client);
    tokio::time::timeout(Duration::from_secs(5), serving)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn transport_stop_and_client_cancellation_reach_every_worker_kind() {
    for cancellation_mode in ["request", "tool", "human"] {
        let user_stop = cancellation_mode != "request";
        for (name, arguments) in worker_requests() {
            let (started, ready) = tokio::sync::oneshot::channel();
            let (cancelled, finished) = tokio::sync::oneshot::channel();
            let backend = Arc::new(WaitingBackend {
                started: Mutex::new(Some(started)),
                cancelled: Mutex::new(Some(cancelled)),
            });
            let server =
                ControlFreakServer::with_indicator(backend, SafetyIndicator::dormant(), None);
            let stop = server.stop.clone();
            let (client_io, server_io) = tokio::io::duplex(65536);
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
            outcome_tests::exchange(&mut client, json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"stop-test","version":"1"}
            }})).await;
            client
                .get_mut()
                .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
                .await
                .unwrap();
            let request = json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":name,"arguments":arguments}});
            client
                .get_mut()
                .write_all(format!("{request}\n").as_bytes())
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(5), ready)
                .await
                .unwrap()
                .unwrap();
            let cancellation = if user_stop {
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"stop_desktop_work","arguments":{}}})
            } else {
                json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":2}})
            };
            if cancellation_mode == "human" {
                stop.set_session_active(true);
                assert!(stop.stop_by_user());
            } else {
                client
                    .get_mut()
                    .write_all(format!("{cancellation}\n").as_bytes())
                    .await
                    .unwrap();
            }
            tokio::time::timeout(Duration::from_secs(2), finished)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(stop.is_stopped(), user_stop);
            // Status stays reachable after a stop, even with other responses in flight.
            let status = json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":GET_SERVER_STATUS,"arguments":{}}});
            client
                .get_mut()
                .write_all(format!("{status}\n").as_bytes())
                .await
                .unwrap();
            tokio::time::timeout(
                Duration::from_secs(5),
                receive_stop_status(&mut client, cancellation_mode == "human"),
            )
            .await
            .unwrap();
            assert_observation_contract(&mut client).await;
            drop(client);
            tokio::time::timeout(Duration::from_secs(5), serving)
                .await
                .unwrap()
                .unwrap();
        }
    }
}

async fn receive_stop_status(client: &mut BufReader<tokio::io::DuplexStream>, expect_event: bool) {
    let mut status_received = false;
    let mut event_received = !expect_event;
    while !status_received || !event_received {
        let mut line = String::new();
        assert_ne!(client.read_line(&mut line).await.unwrap(), 0);
        let value: Value = serde_json::from_str(&line).unwrap();
        if value["id"] == 4 {
            assert!(value["result"]["structuredContent"]["stop_state"].is_string());
            status_received = true;
        }
        if value["method"] == "notifications/controlfreak/session_stopped" {
            assert_eq!(value["params"]["event"], "user_stopped_session");
            event_received = true;
        }
    }
}

async fn assert_observation_contract(client: &mut BufReader<tokio::io::DuplexStream>) {
    // Every admission path preserves structured argument errors, including after stop.
    for tool in [
        CAPTURE_DISPLAY,
        GET_SERVER_STATUS,
        END_CONTROL_SESSION,
        "stop_desktop_work",
    ] {
        let mut response = outcome_tests::exchange(
            client,
            json!({
                "jsonrpc":"2.0", "id":5, "method":"tools/call",
                "params":{"name":tool,"arguments":{"unknown_option":true}}
            }),
        )
        .await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while response["id"] != 5 {
                let mut line = String::new();
                assert_ne!(client.read_line(&mut line).await.unwrap(), 0);
                response = serde_json::from_str(&line).unwrap();
            }
        })
        .await
        .unwrap();
        assert_eq!(response["result"]["isError"], true, "{response}");
        assert_eq!(
            response["result"]["structuredContent"]["error"]["code"],
            "invalid_arguments"
        );
    }
    let listed = outcome_tests::exchange(
        client,
        json!({
            "jsonrpc":"2.0", "id":6, "method":"tools/call",
            "params":{"name":LIST_DISPLAYS,"arguments":{}}
        }),
    )
    .await;
    assert_eq!(listed["result"]["structuredContent"]["count"], 0);
}

#[tokio::test]
async fn observation_cancellation_refuses_queued_work_and_discards_late_results() {
    for queued in [true, false] {
        let work = stop::Work::default();
        let control = work.control.clone();
        if queued {
            control.cancel();
        }
        let result = stop::WORK
            .scope(
                work,
                handlers::run_platform_operation(None, move || {
                    assert!(
                        !queued,
                        "cancelled queued observation must not reach its provider"
                    );
                    control.cancel();
                    Ok(())
                }),
            )
            .await
            .unwrap();
        assert!(
            result.is_err(),
            "cancelled observation must not return success"
        );
    }
}

fn worker_requests() -> [(&'static str, Value); 5] {
    [
        (
            MOVE_MOUSE,
            json!({"target_ref":"fixture","display_id":"fixture","x":0,"y":0}),
        ),
        (
            TYPE_TEXT,
            json!({"target_ref":"fixture","text":"synthetic"}),
        ),
        (
            WAIT_FOR_VISUAL_CHANGE,
            json!({"display_id":"fixture","x":0,"y":0,"width":1,"height":1}),
        ),
        (
            FIND_TEXT_ON_SCREEN,
            json!({"display_id":"fixture","x":0,"y":0,"width":1,"height":1,
                    "query":"synthetic", "match_mode":"tolerant", "ocr_confusions":true}),
        ),
        (
            READ_TEXT_IN_REGION,
            json!({"display_id":"fixture","x":0,"y":0,"width":1,"height":1}),
        ),
    ]
}
