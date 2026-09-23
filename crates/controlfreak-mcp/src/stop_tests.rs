use super::*;
use controlfreak_core::{
    BackendMetadata, DisplayBackend, KeyboardBackend, OcrBackend, PointerBackend, WindowBackend,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

struct WaitingBackend {
    started: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    cancelled: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}

impl WaitingBackend {
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
    fn wait_for_visual_change_controlled(
        &self,
        _: &WaitForVisualChangeRequest,
        control: &MutationControl,
    ) -> Result<VisualChangeResult, PlatformError> {
        self.wait(control)
    }
}
impl OcrBackend for WaitingBackend {
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
async fn transport_stop_and_client_cancellation_reach_every_worker_kind() {
    for user_stop in [false, true] {
        for (name, arguments) in [
            (MOVE_MOUSE, json!({"display_id":"fixture","x":0,"y":0})),
            (TYPE_TEXT, json!({"text":"synthetic"})),
            (
                WAIT_FOR_VISUAL_CHANGE,
                json!({"display_id":"fixture","x":0,"y":0,"width":1,"height":1}),
            ),
            (
                READ_TEXT_IN_REGION,
                json!({"display_id":"fixture","x":0,"y":0,"width":1,"height":1}),
            ),
        ] {
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
            client
                .get_mut()
                .write_all(format!("{cancellation}\n").as_bytes())
                .await
                .unwrap();
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
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let mut line = String::new();
                    assert_ne!(client.read_line(&mut line).await.unwrap(), 0);
                    let value: Value = serde_json::from_str(&line).unwrap();
                    if value["id"] == 4 {
                        assert!(value["result"]["structuredContent"]["stop_state"].is_string());
                        break;
                    }
                }
            })
            .await
            .unwrap();
            assert_argument_errors(&mut client).await;
            drop(client);
            tokio::time::timeout(Duration::from_secs(5), serving)
                .await
                .unwrap()
                .unwrap();
        }
    }
}

async fn assert_argument_errors(client: &mut BufReader<tokio::io::DuplexStream>) {
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
}
