//! Wire shape of session admission failures, checked over the MCP transport.

use super::stop_tests::WaitingBackend;
use super::tests::{ExclusiveArbitrator, RecordingIndicator};
use super::*;
use tokio::io::{AsyncWriteExt, BufReader};

pub(super) struct Harness {
    pub(super) client: BufReader<tokio::io::DuplexStream>,
    runtime: Arc<IndicatorRuntime>,
    serving: tokio::task::JoinHandle<()>,
}

async fn harness(owned: Arc<AtomicBool>) -> Harness {
    harness_with_backend(Arc::new(WaitingBackend::idle()), owned).await
}

pub(super) async fn harness_with_backend(
    backend: Arc<dyn PlatformBackend>,
    owned: Arc<AtomicBool>,
) -> Harness {
    let runtime = Arc::new(IndicatorRuntime::new_with_timing(
        SafetyIndicator::dormant(),
        || {
            Ok(RecordingIndicator {
                events: Arc::new(Mutex::new(Vec::new())),
                fail_hide: false,
            })
        },
        Arc::new(ExclusiveArbitrator { owned }),
        GlowTiming {
            short_hold: Duration::from_millis(20),
            session_hold: Duration::from_millis(20),
            max_hold: Duration::from_secs(5),
        },
    ));
    let server = ControlFreakServer::with_indicator(
        backend,
        SafetyIndicator::dormant(),
        Some(Arc::clone(&runtime)),
    );
    let (client_io, server_io) = tokio::io::duplex(256 * 1024);
    let serving = tokio::spawn(async move {
        server
            .serve(server_io)
            .await
            .unwrap()
            .waiting()
            .await
            .unwrap();
    });
    let mut client = BufReader::new(client_io);
    outcome_tests::exchange(&mut client, json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
        "protocolVersion":"2025-11-25", "capabilities":{}, "clientInfo":{"name":"session-error-test","version":"1"}
    }})).await;
    client
        .get_mut()
        .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
        .await
        .unwrap();
    Harness {
        client,
        runtime,
        serving,
    }
}

pub(super) async fn call(harness: &mut Harness, name: &str, arguments: Value) -> Value {
    let response = outcome_tests::exchange(
        &mut harness.client,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":name,"arguments":arguments}}),
    )
    .await;
    response["result"].clone()
}

fn begin() -> (&'static str, Value) {
    (
        BEGIN_CONTROL_SESSION,
        json!({"target_ref": "fixture", "expected_seconds": 5}),
    )
}

fn mutate() -> (&'static str, Value) {
    (
        MOVE_MOUSE,
        json!({"display_id":"synthetic","x":0,"y":0,"target_ref":"fixture"}),
    )
}

fn assert_session_error(result: &Value, code: &str, retryable: bool) -> Value {
    assert_eq!(result["isError"], true, "{result}");
    let structured = &result["structuredContent"];
    let error = &structured["error"];
    assert_eq!(error["code"], code, "{result}");
    assert_eq!(error["retryable"], retryable, "{result}");
    assert!(error["message"].as_str().is_some_and(|m| !m.is_empty()));
    assert!(error["details"].is_object(), "{result}");
    // The text fallback carries the same document as structuredContent.
    let text: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(&text, structured);
    structured.clone()
}

impl Harness {
    pub(super) async fn finish(self) {
        drop(self.client);
        tokio::time::timeout(Duration::from_secs(5), self.serving)
            .await
            .expect("server shuts down after transport closes")
            .unwrap();
    }
}

#[tokio::test]
async fn busy_desktop_returns_desktop_in_use_with_owner_details() {
    for (name, arguments) in [begin(), mutate()] {
        let mut harness = harness(Arc::new(AtomicBool::new(true))).await;
        let result = call(&mut harness, name, arguments).await;
        let structured = assert_session_error(&result, "desktop_in_use", true);
        let error = &structured["error"];
        assert_eq!(
            error["message"],
            "another ControlFreak server owns the desktop"
        );
        assert_eq!(error["details"]["owner_instance_id"], "other-test-server");
        assert_eq!(error["details"]["retry_after_ms"], 25);
        assert_eq!(error["details"]["session_state"], "dormant");
        assert!(error["details"]["remedy"].is_string());
        // The old double wrapping is gone.
        assert!(!result.to_string().contains("tool_execution_failed"));
        assert!(!result.to_string().contains("indicator is not visible"));
        if name == MOVE_MOUSE {
            assert_eq!(structured["status"], "not_started");
            assert_eq!(structured["retry_action"], false);
        }
        harness.finish().await;
    }
}

#[tokio::test]
async fn shutting_down_server_returns_its_own_code() {
    for (name, arguments) in [begin(), mutate()] {
        let mut harness = harness(Arc::new(AtomicBool::new(false))).await;
        harness.runtime.terminated.store(true, Ordering::Release);
        let result = call(&mut harness, name, arguments).await;
        let structured = assert_session_error(&result, "server_shutting_down", false);
        assert_eq!(
            structured["error"]["message"],
            "the server is shutting down"
        );
        harness.finish().await;
    }
}

#[tokio::test]
async fn still_closing_session_returns_its_own_code() {
    for (name, arguments) in [begin(), mutate()] {
        let mut harness = harness(Arc::new(AtomicBool::new(false))).await;
        // An in-flight mutation keeps the closing session from finishing.
        let lease = harness.runtime.acquire().await.unwrap();
        harness.runtime.end_session().unwrap();
        let result = call(&mut harness, name, arguments).await;
        let structured = assert_session_error(&result, "session_closing", true);
        assert_eq!(
            structured["error"]["message"],
            "the prior control session is still closing"
        );
        drop(lease);
        harness.finish().await;
    }
}

#[test]
fn every_session_error_has_a_distinct_wire_shape() {
    let indicator = SafetyIndicator::dormant();
    let cases = [
        (
            SessionError::ArbitrationUnavailable {
                reason: "denied".to_owned(),
            },
            "desktop_arbitration_unavailable",
            false,
        ),
        (
            SessionError::StopRegistration("stopped".to_owned()),
            "desktop_work_stopped",
            false,
        ),
        (
            SessionError::Environment("locked".to_owned()),
            "control_environment_unavailable",
            false,
        ),
    ];
    for (error, code, retryable) in cases {
        let result = error.into_tool_result(&indicator);
        let value = result.structured_content.unwrap();
        assert_eq!(result.is_error, Some(true));
        assert_eq!(value["error"]["code"], code);
        assert_eq!(value["error"]["retryable"], retryable);
    }
    // Indicator failures keep their established shape, built from typed data and never parsed JSON.
    let result =
        SessionError::Indicator("{\"code\":\"spoofed\"}".to_owned()).into_tool_result(&indicator);
    let value = result.structured_content.unwrap();
    assert_eq!(value["error"]["code"], "tool_execution_failed");
    assert_eq!(value["error"]["details"]["code"], "indicator_unavailable");
    assert_eq!(
        value["error"]["details"]["reason"],
        "{\"code\":\"spoofed\"}"
    );
}
