//! After a user stop, every surface an agent reads tells it the same recovery step, over one
//! connection: the refusal, `get_server_status`, the end result and the stop notifications.

use super::session_error_tests::{Harness, harness_with_backend};
use super::*;
use controlfreak_core::{
    BackendMetadata, CleanupStatus, DisplayBackend, KeyboardBackend, OcrBackend, PointerBackend,
    WindowBackend,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

const TARGET: &str = "target-00000000-0000-0000-0000-000000000003";
const NOTIFICATION: &str = "notifications/controlfreak/session_stopped";

/// Lists one window and holds `type_text` until the test releases it, so the stop drains for as
/// long as the test needs. With `fail_cleanup`, the stopped action cannot confirm its release.
struct HeldTyping {
    fail_cleanup: bool,
    started: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
}

impl BackendMetadata for HeldTyping {
    fn validate_target_reference(&self, _: &str) -> Result<(), PlatformError> {
        Ok(())
    }
    fn identity(&self) -> controlfreak_core::BackendIdentity {
        controlfreak_core::BackendIdentity {
            platform: controlfreak_core::Platform::Windows,
            display_server: controlfreak_core::DisplayServer::Dwm,
            backend_id: "stop-recovery-fixture".into(),
        }
    }
    fn capabilities(&self) -> Vec<controlfreak_core::CapabilityDescriptor> {
        vec![]
    }
    fn permissions(&self) -> Vec<controlfreak_core::PermissionDescriptor> {
        vec![]
    }
}

impl KeyboardBackend for HeldTyping {
    fn type_text_controlled(
        &self,
        _: &TextInputRequest,
        control: &MutationControl,
    ) -> Result<KeyboardActionResult, PlatformError> {
        if let Some(started) = self.started.lock().unwrap().take() {
            let _ = started.send(());
        }
        let _ = self
            .release
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(10));
        if self.fail_cleanup {
            control.dispatch_started();
            control.cleanup_status(CleanupStatus::Unknown);
        }
        control.check("synthetic_typing")?;
        Err(PlatformError::OperationFailed {
            operation: "synthetic_typing".into(),
            reason: "the test expected a stop first".into(),
        })
    }
}

impl WindowBackend for HeldTyping {
    fn list_windows(&self) -> Result<controlfreak_core::WindowList, PlatformError> {
        Ok(controlfreak_core::WindowList {
            windows: vec![controlfreak_core::WindowInfo {
                id: TARGET.into(),
                title: "synthetic".into(),
                class_name: "synthetic".into(),
                process_id: 1,
                bounds: controlfreak_core::WindowBounds {
                    left: 0,
                    top: 0,
                    width: 10,
                    height: 10,
                },
                display_id: "synthetic".into(),
                is_foreground: true,
                is_minimized: false,
                owner: None,
            }],
            truncated: false,
        })
    }
}

impl DisplayBackend for HeldTyping {}
impl OcrBackend for HeldTyping {}
impl PointerBackend for HeldTyping {}

/// Reads the connection, keeping every stop notification, until the reply to `id` arrives.
struct Connection {
    harness: Harness,
    notifications: Vec<Value>,
    schemas: Vec<Value>,
    next_id: u64,
}

impl Connection {
    async fn open(backend: Arc<HeldTyping>) -> Self {
        let mut harness = harness_with_backend(backend, Arc::new(AtomicBool::new(false))).await;
        let listed = outcome_tests::exchange(
            &mut harness.client,
            json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}),
        )
        .await;
        Self {
            harness,
            notifications: Vec::new(),
            schemas: listed["result"]["tools"].as_array().unwrap().clone(),
            next_id: 100,
        }
    }

    async fn send(&mut self, name: &str, arguments: Value) -> u64 {
        self.next_id += 1;
        let request = json!({"jsonrpc":"2.0","id":self.next_id,"method":"tools/call",
            "params":{"name":name,"arguments":arguments}});
        self.harness
            .client
            .get_mut()
            .write_all(format!("{request}\n").as_bytes())
            .await
            .unwrap();
        self.next_id
    }

    async fn read(&mut self) -> Value {
        let mut line = String::new();
        let read = tokio::time::timeout(
            Duration::from_secs(10),
            self.harness.client.read_line(&mut line),
        )
        .await
        .expect("the server answers in time")
        .unwrap();
        assert_ne!(read, 0, "the server closed the connection");
        let value: Value = serde_json::from_str(&line).unwrap();
        if value["method"] == NOTIFICATION {
            self.notifications.push(value["params"].clone());
        }
        value
    }

    async fn reply(&mut self, id: u64) -> Value {
        loop {
            let value = self.read().await;
            if value["id"] == id {
                return value["result"].clone();
            }
        }
    }

    /// Calls a tool, waits for its reply and checks it against the tool's published schema.
    async fn call(&mut self, name: &str, arguments: Value) -> Value {
        let id = self.send(name, arguments).await;
        let result = self.reply(id).await;
        let schema = &self
            .schemas
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap()["outputSchema"];
        assert!(
            jsonschema::validator_for(schema)
                .unwrap()
                .is_valid(&result["structuredContent"]),
            "{name} result does not match its outputSchema: {result}"
        );
        result
    }

    async fn notification(&mut self, count: usize) -> Value {
        while self.notifications.len() < count {
            self.read().await;
        }
        self.notifications[count - 1].clone()
    }
}

fn recovery_action(value: &Value) -> &str {
    value["action"].as_str().unwrap_or_default()
}

/// Begin, a user stop while an action is in flight, the drain, then end, begin, status and a
/// read, all on one connection.
async fn stop_and_recover(fail_cleanup: bool) {
    let (started, started_signal) = tokio::sync::oneshot::channel();
    let (release, release_signal) = std::sync::mpsc::channel();
    let backend = Arc::new(HeldTyping {
        fail_cleanup,
        started: Mutex::new(Some(started)),
        release: Mutex::new(release_signal),
    });
    let mut connection = Connection::open(backend).await;
    let stop = connection.harness.runtime.stop.clone();

    let begun = connection
        .call(BEGIN_CONTROL_SESSION, json!({"target_ref": TARGET}))
        .await;
    assert_ne!(begun["isError"], true, "{begun}");
    assert_eq!(
        begun["structuredContent"]["session"]["approved_target_ref"],
        TARGET
    );

    // An action is in flight when the user stops, so the stop drains.
    let typing = connection
        .send(
            TYPE_TEXT,
            json!({"target_ref": TARGET, "text": "synthetic"}),
        )
        .await;
    tokio::time::timeout(Duration::from_secs(5), started_signal)
        .await
        .unwrap()
        .unwrap();
    assert!(stop.stop_by_user(), "the user stops the owned session");

    let first = connection.notification(1).await;
    assert_eq!(first["event"], "user_stopped_session");
    assert_eq!(first["stop_state"], "draining");
    assert_eq!(recovery_action(&first["recovery"]), "wait_for_drain");
    assert_eq!(first["retry_action"], false);

    let draining = connection.call(GET_SERVER_STATUS, json!({})).await;
    let draining = &draining["structuredContent"];
    assert_eq!(draining["input_admission"], "closed");
    assert_eq!(draining["stop_state"], "draining");
    assert_eq!(draining["recovery"], first["recovery"]);

    // The in-flight action finishes and the drain ends with a second notification.
    release.send(()).unwrap();
    let typed = connection.reply(typing).await;
    assert_eq!(typed["structuredContent"]["retry_action"], false, "{typed}");
    after_the_drain(&mut connection, fail_cleanup).await;
    assert!(stop.is_stopped(), "no call reopened the latch");
    assert_eq!(connection.notifications.len(), 2, "one per stop state");
    connection.harness.finish().await;
}

/// End, begin, an input tool, status and a read once the drain has ended.
async fn after_the_drain(connection: &mut Connection, fail_cleanup: bool) {
    let second = connection.notification(2).await;
    assert_eq!(second["event"], "stop_drained");
    let (stop_state, expected) = if fail_cleanup {
        ("cleanup_failed", "resolve_held_input")
    } else {
        ("stopped", "restart_client")
    };
    assert_eq!(second["stop_state"], stop_state);
    assert_eq!(recovery_action(&second["recovery"]), expected);
    assert_eq!(second["recovery"]["actor"], "user");

    let ended = connection.call(END_CONTROL_SESSION, json!({})).await;
    if fail_cleanup {
        // The release could not be confirmed, so the desktop stays reserved.
        assert_eq!(ended["isError"], true, "{ended}");
        assert_eq!(
            recovery_action(&ended["structuredContent"]["error"]["details"]["recovery"]),
            expected
        );
    } else {
        let ended = &ended["structuredContent"];
        assert_eq!(ended["status"], "not_owned", "the stop already closed it");
        assert_eq!(ended["session"]["approved_target_ref"], Value::Null);
        assert_eq!(ended["session"]["owns_arbitration"], false);
        assert_eq!(recovery_action(&ended["recovery"]), expected);
    }

    let refused = connection
        .call(BEGIN_CONTROL_SESSION, json!({"target_ref": TARGET}))
        .await;
    assert_eq!(refused["isError"], true, "{refused}");
    let error = &refused["structuredContent"]["error"];
    assert_eq!(error["code"], "desktop_work_stopped");
    assert_eq!(error["retryable"], false);
    let message = error["message"].as_str().unwrap();
    assert!(message.contains("server restart control"), "{message}");
    assert!(message.contains("quit and reopen the client"), "{message}");
    let recovery = error["details"]["recovery"].clone();
    assert_eq!(recovery_action(&recovery), expected);

    let input = connection
        .call(
            TYPE_TEXT,
            json!({"target_ref": TARGET, "text": "synthetic"}),
        )
        .await;
    assert_eq!(input["structuredContent"]["status"], "not_started");
    assert_eq!(
        input["structuredContent"]["error"]["details"]["recovery"],
        recovery
    );

    let status = connection.call(GET_SERVER_STATUS, json!({})).await;
    let status = &status["structuredContent"];
    assert_eq!(status["input_admission"], "closed");
    assert_eq!(status["stop_state"], stop_state);
    assert_eq!(status["stop_reason"], "user_stop");
    assert_eq!(status["recovery"], recovery, "status and refusal agree");
    if !fail_cleanup {
        assert_eq!(status["session"]["approved_target_ref"], Value::Null);
    }

    let windows = connection.call(LIST_WINDOWS, json!({})).await;
    assert_ne!(windows["isError"], true, "reads still work: {windows}");
    assert_eq!(windows["structuredContent"]["windows"][0]["id"], TARGET);
}

#[tokio::test]
async fn a_user_stop_reports_one_recovery_step_on_every_surface() {
    tokio::time::timeout(Duration::from_secs(30), stop_and_recover(false))
        .await
        .expect("the stop sequence finishes in time");
}

#[tokio::test]
async fn a_failed_release_after_a_user_stop_asks_for_held_input_first() {
    tokio::time::timeout(Duration::from_secs(30), stop_and_recover(true))
        .await
        .expect("the stop sequence finishes in time");
}
