//! Input arguments are parsed and statically checked before a request takes the desktop, so a
//! malformed request never claims arbitration, arms the idle hold or shows the indicator.

use super::fake_desktop::FakeDesktop;
use super::session_error_tests::{Harness, call, serve_with_runtime};
use super::tests::{ExclusiveArbitrator, RecordingIndicator};
use super::*;

/// One server's view of a desktop that other servers may share.
struct CountingArbitrator {
    shared: ExclusiveArbitrator,
    acquisitions: Arc<AtomicUsize>,
}

impl ActivityArbitrator for CountingArbitrator {
    fn try_acquire(&self) -> Result<(), ArbitrationError> {
        self.acquisitions.fetch_add(1, Ordering::SeqCst);
        self.shared.try_acquire()
    }

    fn release(&self) {
        self.shared.release();
    }
}

struct Server {
    harness: Harness,
    indicator_starts: Arc<AtomicUsize>,
    indicator_events: Arc<Mutex<Vec<&'static str>>>,
    acquisitions: Arc<AtomicUsize>,
}

impl Server {
    /// A server on `desktop`, sharing the exclusive desktop flag `owned` with other servers. The
    /// hold outlasts the test, so a session that a request did take is still visible afterwards.
    async fn start(desktop: &Arc<FakeDesktop>, owned: &Arc<AtomicBool>) -> Self {
        let indicator_starts = Arc::new(AtomicUsize::new(0));
        let indicator_events = Arc::new(Mutex::new(Vec::new()));
        let acquisitions = Arc::new(AtomicUsize::new(0));
        let hold = Duration::from_secs(30);
        let runtime = Arc::new(SessionRuntime::new_with_timing(
            SafetyIndicator::dormant(),
            {
                let starts = Arc::clone(&indicator_starts);
                let events = Arc::clone(&indicator_events);
                move || {
                    starts.fetch_add(1, Ordering::SeqCst);
                    Ok(RecordingIndicator {
                        events: Arc::clone(&events),
                        fail_hide: false,
                    })
                }
            },
            Arc::new(CountingArbitrator {
                shared: ExclusiveArbitrator {
                    owned: Arc::clone(owned),
                },
                acquisitions: Arc::clone(&acquisitions),
            }),
            GlowTiming {
                short_hold: hold,
                session_hold: hold,
                max_hold: hold,
            },
        ));
        Self {
            harness: serve_with_runtime(desktop.clone(), runtime).await,
            indicator_starts,
            indicator_events,
            acquisitions,
        }
    }

    async fn call(&mut self, name: &str, arguments: Value) -> Value {
        call(&mut self.harness, name, arguments).await
    }

    /// The `session` object of `get_server_status`.
    async fn session(&mut self) -> Value {
        self.call(GET_SERVER_STATUS, json!({})).await["structuredContent"]["session"].clone()
    }

    fn acting_shown(&self) -> usize {
        self.indicator_events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| **event == "acting")
            .count()
    }
}

/// The static refusals named by the contract, with the error code each one returns.
fn malformed(target: &str) -> Vec<(&'static str, Value, &'static str)> {
    vec![
        (TYPE_TEXT, json!({}), "invalid_arguments"),
        (
            TYPE_TEXT,
            json!({"target_ref": target, "text": "a", "timeout_ms": 0}),
            "invalid_argument",
        ),
        (
            RUN_ACTIONS,
            json!({"target_ref": target, "steps": [
                {"action": "type_text", "text": "a", "timeout_ms": 300_001},
            ]}),
            "invalid_argument",
        ),
        (
            CLICK_MOUSE,
            json!({"target_ref": target}),
            "invalid_argument",
        ),
        (
            FOCUS_WINDOW,
            json!({"window_id": target, "target_ref": "target-00000000-0000-0000-0000-0000000000ff"}),
            "invalid_argument",
        ),
        (
            SWITCH_VIRTUAL_DESKTOP,
            json!({"direction": "right", "observation": {"settle_ms": 100}}),
            "invalid_argument",
        ),
        (
            RUN_ACTIONS,
            json!({"target_ref": target, "steps": [
                {"action": "type_text", "text": "a"},
                {"action": "click", "x": 1, "y": 1, "click_count": 4},
            ]}),
            "invalid_argument",
        ),
        (
            CLICK_TEXT,
            json!({"target_ref": target, "x": 0, "y": 0, "width": 10, "height": 10, "query": "Save",
                   "match_mode": "tolerant"}),
            "invalid_argument",
        ),
        (
            CLICK_TEXT,
            json!({"target_ref": target, "x": 0, "y": 0, "width": 10, "height": 10, "query": "Save",
                   "match_mode": "substring", "exact_match": true}),
            "invalid_argument",
        ),
    ]
}

fn assert_refused(name: &str, result: &Value, code: &str) {
    assert_eq!(result["isError"], true, "{name}: {result}");
    let content = &result["structuredContent"];
    assert_eq!(content["error"]["code"], code, "{name}: {result}");
    assert_eq!(content["status"], "not_started", "{name}: {result}");
    assert_eq!(content["input"]["sent_events"], 0, "{name}: {result}");
}

fn assert_untouched(session: &Value) {
    assert_eq!(session["owns_arbitration"], false, "{session}");
    assert_eq!(session["call_count"], 0, "{session}");
    assert_eq!(session["state"], "dormant", "{session}");
    assert!(
        session["time_until_close_ms"].is_null(),
        "no hold is armed: {session}"
    );
}

#[tokio::test]
async fn a_malformed_input_request_is_refused_before_it_takes_the_desktop() {
    let desktop = FakeDesktop::new();
    let target = desktop.id(desktop.open("Editor", 10));
    let owned = Arc::new(AtomicBool::new(false));
    let mut server = Server::start(&desktop, &owned).await;

    for (name, arguments, code) in malformed(&target) {
        assert_refused(name, &server.call(name, arguments).await, code);
        assert_untouched(&server.session().await);
    }
    // Every input tool called with no arguments fails parsing, or a static check when it has no
    // required field.
    for spec in catalog::CATALOG {
        if !spec.is_mutating() {
            continue;
        }
        let name = spec.name;
        let result = server.call(name, json!({})).await;
        let code = result["structuredContent"]["error"]["code"]
            .as_str()
            .unwrap_or_default();
        assert!(
            ["invalid_arguments", "invalid_argument"].contains(&code),
            "{name}: {result}"
        );
        assert_refused(name, &result, code);
        assert_untouched(&server.session().await);
    }

    assert_eq!(server.acquisitions.load(Ordering::SeqCst), 0);
    assert_eq!(server.indicator_starts.load(Ordering::SeqCst), 0);
    assert!(server.indicator_events.lock().unwrap().is_empty());
    assert!(!owned.load(Ordering::SeqCst));
    assert!(desktop.received().is_empty());
    server.harness.finish().await;
}

#[tokio::test]
async fn a_malformed_request_on_one_server_leaves_the_desktop_free_for_another() {
    let desktop = FakeDesktop::new();
    let target = desktop.id(desktop.open("Editor", 10));
    let owned = Arc::new(AtomicBool::new(false));
    let mut first = Server::start(&desktop, &owned).await;
    let mut second = Server::start(&desktop, &owned).await;

    for (name, arguments, code) in malformed(&target) {
        assert_refused(name, &first.call(name, arguments).await, code);
    }
    let begun = second
        .call(BEGIN_CONTROL_SESSION, json!({"target_ref": target}))
        .await;
    assert_ne!(begun["isError"], true, "{begun}");
    assert_eq!(begun["structuredContent"]["status"], "armed", "{begun}");

    // The two servers do share one desktop: a valid action on the first is now refused.
    let busy = first
        .call(
            TYPE_TEXT,
            json!({"target_ref": target, "text": "a", "observation": {"mode": "none"}}),
        )
        .await;
    assert_eq!(
        busy["structuredContent"]["error"]["code"], "desktop_in_use",
        "{busy}"
    );
    first.harness.finish().await;
    second.harness.finish().await;
}

#[tokio::test]
async fn each_valid_input_request_takes_exactly_one_lease() {
    let desktop = FakeDesktop::new();
    let target = desktop.id(desktop.open("Editor", 10));
    let owned = Arc::new(AtomicBool::new(false));
    let mut server = Server::start(&desktop, &owned).await;

    let typed = server
        .call(
            TYPE_TEXT,
            json!({"target_ref": target, "text": "a", "observation": {"mode": "none"}}),
        )
        .await;
    assert_eq!(typed["structuredContent"]["status"], "completed", "{typed}");
    let session = server.session().await;
    assert_eq!(session["call_count"], 1, "{session}");
    assert_eq!(session["owns_arbitration"], true, "{session}");
    assert_eq!(server.acting_shown(), 1);

    // A malformed request inside the session takes no lease and leaves the session as it was.
    let (name, arguments, code) = malformed(&target).remove(4);
    assert_refused(name, &server.call(name, arguments).await, code);
    assert_eq!(server.session().await["call_count"], 1);

    let batch = server
        .call(
            RUN_ACTIONS,
            json!({"target_ref": target, "steps": [
                {"action": "type_text", "text": "b"},
                {"action": "press_keys", "keys": ["enter"]},
            ], "observation": {"mode": "none"}}),
        )
        .await;
    assert_eq!(batch["structuredContent"]["status"], "completed", "{batch}");
    assert_eq!(server.session().await["call_count"], 2);
    assert_eq!(server.acquisitions.load(Ordering::SeqCst), 1);
    assert_eq!(server.indicator_starts.load(Ordering::SeqCst), 1);
    server.harness.finish().await;
}

/// #5: an argument error names the field's path, what it expects and the JSON kind it got,
/// never the value, and like any malformed request it leaves the desktop alone.
#[tokio::test]
async fn an_argument_error_explains_the_field_without_the_value() {
    let desktop = FakeDesktop::new();
    let target = desktop.id(desktop.open("Editor", 10));
    let owned = Arc::new(AtomicBool::new(false));
    let mut server = Server::start(&desktop, &owned).await;

    let typed = server
        .call(TYPE_TEXT, json!({"target_ref": target, "text": 5}))
        .await;
    assert_refused(TYPE_TEXT, &typed, "invalid_arguments");
    let details = &typed["structuredContent"]["error"]["details"];
    assert_eq!(details["path"], "text", "{typed}");
    assert_eq!(details["expected"], "string", "{typed}");
    assert_eq!(details["received_kind"], "number", "{typed}");

    let typo = server
        .call(
            TYPE_TEXT,
            json!({"target_ref": target, "text": "private words", "txt": "more private words"}),
        )
        .await;
    assert_refused(TYPE_TEXT, &typo, "invalid_arguments");
    let details = &typo["structuredContent"]["error"]["details"];
    assert_eq!(details["problem"], "unknown_field", "{typo}");
    assert_eq!(details["path"], "txt", "{typo}");
    let valid = details["valid_fields"].as_array().unwrap();
    assert!(
        valid.contains(&json!("text")) && valid.len() <= 32,
        "{typo}"
    );

    let batch = server
        .call(
            RUN_ACTIONS,
            json!({"target_ref": target, "steps": [
                {"action": "type_text", "text": "private words"},
                {"action": "type_text", "text": ["private words"]},
            ]}),
        )
        .await;
    assert_refused(RUN_ACTIONS, &batch, "invalid_arguments");
    let details = &batch["structuredContent"]["error"]["details"];
    assert_eq!(details["path"], "steps[1].text", "{batch}");
    assert_eq!(details["received_kind"], "array", "{batch}");

    let capture = server
        .call(
            CAPTURE_REGION,
            json!({"x": "private words", "y": 0, "width": 1, "height": 1}),
        )
        .await;
    assert_eq!(capture["isError"], true, "{capture}");
    let error = &capture["structuredContent"]["error"];
    assert_eq!(error["code"], "invalid_arguments", "{capture}");
    assert_eq!(error["details"]["path"], "x", "{capture}");
    assert_eq!(error["details"]["received_kind"], "string", "{capture}");

    for result in [&typed, &typo, &batch, &capture] {
        assert!(!result.to_string().contains("private"), "{result}");
    }
    assert_untouched(&server.session().await);
    assert_eq!(server.acquisitions.load(Ordering::SeqCst), 0);
    assert!(desktop.received().is_empty());
    server.harness.finish().await;
}
