//! `act_on_element` over the transport: a semantic action is input. It takes one operation lease
//! and shows the indicator like pointer input, refuses a window that is not itself in front and
//! a stale or foreign element before anything is sent, and reports a provider that may have acted
//! as `unknown`, never as success.

use super::fake_desktop::{ActionProvider, FakeDesktop, Received};
use super::prepare_tests::{Server, assert_refused, assert_untouched};
use super::session_error_tests::{call, harness_with_backend};
use super::*;
use controlfreak_core::ElementAction;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

/// The `element_ref` of the window's `Ping` button, from `find_elements`.
async fn ping_button(server: &mut Server, target: &str) -> String {
    let found = server
        .call(
            FIND_ELEMENTS,
            json!({"target_ref": target, "role": "button", "name": "ping"}),
        )
        .await;
    assert_ne!(found["isError"], true, "{found}");
    found["structuredContent"]["elements"][0]["element_ref"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn invoke(target: &str, element: &str) -> Value {
    json!({"target_ref": target, "element_ref": element, "action": "invoke",
           "observation": {"mode": "none"}})
}

#[tokio::test]
async fn an_action_takes_one_lease_shows_the_indicator_and_reaches_the_element() {
    let desktop = FakeDesktop::new();
    let app = desktop.open("Orders", 10);
    let target = desktop.id(app);
    let owned = Arc::new(AtomicBool::new(false));
    let mut server = Server::start(&desktop, &owned).await;
    let element = ping_button(&mut server, &target).await;
    assert_untouched(&server.session().await);

    let acted = server.call(ACT_ON_ELEMENT, invoke(&target, &element)).await;
    assert_ne!(acted["isError"], true, "{acted}");
    let content = &acted["structuredContent"];
    assert_eq!(content["status"], "completed", "{acted}");
    assert_eq!(content["input"]["input_outcome"], "input_sent", "{acted}");
    assert_eq!(
        content["action"],
        json!({"kind": "element", "action": "invoke"})
    );
    assert_eq!(
        content["observation"]["foreground_window"]["id"],
        target.as_str()
    );
    assert_eq!(
        desktop.received(),
        [Received::Element {
            window: app,
            action: ElementAction::Invoke,
        }]
    );
    let session = server.session().await;
    assert_eq!(session["call_count"], 1, "{session}");
    assert_eq!(session["owns_arbitration"], true, "{session}");
    assert_eq!(session["approved_target_ref"], target.as_str(), "{session}");
    assert_eq!(server.acquisitions.load(Ordering::SeqCst), 1);
    assert_eq!(server.indicator_starts.load(Ordering::SeqCst), 1);
    assert_eq!(server.acting_shown(), 1);
    server.harness.finish().await;
}

#[tokio::test]
async fn malformed_actions_are_refused_before_the_desktop_is_taken() {
    let desktop = FakeDesktop::new();
    let target = desktop.id(desktop.open("Orders", 10));
    let owned = Arc::new(AtomicBool::new(false));
    let mut server = Server::start(&desktop, &owned).await;
    for (arguments, code) in [
        (
            json!({"target_ref": target, "action": "invoke"}),
            "invalid_arguments",
        ),
        (
            json!({"target_ref": target, "element_ref": "element-1", "action": "set_value"}),
            "invalid_arguments",
        ),
        (
            json!({"target_ref": target, "element_ref": "element-1", "action": "invoke",
                   "value": "x"}),
            "invalid_arguments",
        ),
        (
            json!({"target_ref": target, "element_ref": "", "action": "invoke"}),
            "invalid_argument",
        ),
    ] {
        assert_refused(
            ACT_ON_ELEMENT,
            &server.call(ACT_ON_ELEMENT, arguments).await,
            code,
        );
        assert_untouched(&server.session().await);
    }
    assert_eq!(server.acquisitions.load(Ordering::SeqCst), 0);
    assert_eq!(server.indicator_starts.load(Ordering::SeqCst), 0);
    assert!(desktop.received().is_empty());
    server.harness.finish().await;
}

#[tokio::test]
async fn an_action_is_refused_unless_the_window_itself_is_in_front() {
    let desktop = FakeDesktop::new();
    let app = desktop.open("Orders", 10);
    let target = desktop.id(app);
    let owned = Arc::new(AtomicBool::new(false));
    let mut server = Server::start(&desktop, &owned).await;
    let element = ping_button(&mut server, &target).await;
    let begun = server
        .call(BEGIN_CONTROL_SESSION, json!({"target_ref": target}))
        .await;
    assert_ne!(begun["isError"], true, "{begun}");

    let assert_not_in_front = |refused: &Value| {
        assert_refused(ACT_ON_ELEMENT, refused, "target_invalidated");
        let error = &refused["structuredContent"]["error"];
        assert_eq!(error["reason"], "not_foreground", "{refused}");
        assert_eq!(error["retryable"], true, "{refused}");
        assert_eq!(refused["structuredContent"]["retry_action"], false);
    };

    // Another process in front: refused, nothing sent, the session is kept.
    desktop.open("Mail", 20);
    assert_not_in_front(&server.call(ACT_ON_ELEMENT, invoke(&target, &element)).await);
    let focused = server
        .call(
            FOCUS_WINDOW,
            json!({"window_id": target, "observation": {"mode": "none"}}),
        )
        .await;
    assert_ne!(focused["isError"], true, "{focused}");

    // A dialog of the same process in front, which a click would accept, is refused too.
    let dialog = desktop.open_owned(app, "Confirm");
    assert_not_in_front(&server.call(ACT_ON_ELEMENT, invoke(&target, &element)).await);
    assert!(
        desktop
            .received()
            .iter()
            .all(|received| !matches!(received, Received::Element { .. }))
    );
    assert_eq!(
        server.session().await["approved_target_ref"],
        target.as_str()
    );

    // With the window itself in front again, the same reference works in the same session.
    desktop.close(dialog);
    let acted = server.call(ACT_ON_ELEMENT, invoke(&target, &element)).await;
    assert_eq!(acted["structuredContent"]["status"], "completed", "{acted}");
    server.harness.finish().await;
}

#[tokio::test]
async fn an_element_of_another_window_or_a_stale_one_is_refused_without_input() {
    let desktop = FakeDesktop::new();
    let other = desktop.open("Mail", 20);
    let other_target = desktop.id(other);
    let app = desktop.open("Orders", 10);
    let target = desktop.id(app);
    let owned = Arc::new(AtomicBool::new(false));
    let mut server = Server::start(&desktop, &owned).await;
    let foreign = ping_button(&mut server, &other_target).await;
    for element in [foreign.as_str(), "element-made-up"] {
        let refused = server.call(ACT_ON_ELEMENT, invoke(&target, element)).await;
        assert_refused(ACT_ON_ELEMENT, &refused, "invalid_argument");
    }
    // An element of the window, but asked for a window of another session target.
    let own = ping_button(&mut server, &target).await;
    let elsewhere = server
        .call(ACT_ON_ELEMENT, invoke(&other_target, &own))
        .await;
    assert_refused(ACT_ON_ELEMENT, &elsewhere, "target_invalidated");
    // An action the element does not support.
    let toggle = server
        .call(
            ACT_ON_ELEMENT,
            json!({"target_ref": target, "element_ref": own, "action": "toggle"}),
        )
        .await;
    assert_refused(ACT_ON_ELEMENT, &toggle, "invalid_argument");
    assert!(desktop.received().is_empty());
    server.harness.finish().await;
}

#[tokio::test]
async fn a_provider_that_may_have_acted_is_unknown_and_never_retried() {
    let desktop = FakeDesktop::new();
    let target = desktop.id(desktop.open("Orders", 10));
    let owned = Arc::new(AtomicBool::new(false));
    let mut server = Server::start(&desktop, &owned).await;
    let element = ping_button(&mut server, &target).await;
    desktop.set_action_provider(ActionProvider::Hangs);
    let hung = server.call(ACT_ON_ELEMENT, invoke(&target, &element)).await;
    // Uncertain delivery is not a tool error; `status` says it.
    let content = &hung["structuredContent"];
    assert_eq!(content["status"], "unknown", "{hung}");
    assert_eq!(content["input"]["input_outcome"], "unknown", "{hung}");
    assert_eq!(content["retry_action"], false, "{hung}");
    assert_eq!(content["error"]["reason"], "accessibility_unavailable");
    assert_eq!(content["error"]["retryable"], false);
    server.harness.finish().await;
}

#[tokio::test]
async fn a_stop_during_the_action_reports_unknown_delivery() {
    let desktop = FakeDesktop::new();
    let target = desktop.id(desktop.open("Orders", 10));
    let mut harness = harness_with_backend(desktop.clone(), Arc::new(AtomicBool::new(false))).await;
    let found = call(
        &mut harness,
        FIND_ELEMENTS,
        json!({"target_ref": target, "role": "button"}),
    )
    .await;
    let element = found["structuredContent"]["elements"][0]["element_ref"]
        .as_str()
        .unwrap()
        .to_owned();
    desktop.set_action_provider(ActionProvider::BlocksUntilStopped);
    let act = json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{
        "name": ACT_ON_ELEMENT, "arguments": invoke(&target, &element)}});
    harness
        .client
        .get_mut()
        .write_all(format!("{act}\n").as_bytes())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let stop = json!({"jsonrpc":"2.0","id":8,"method":"tools/call","params":{
        "name": STOP_DESKTOP_WORK, "arguments": {}}});
    harness
        .client
        .get_mut()
        .write_all(format!("{stop}\n").as_bytes())
        .await
        .unwrap();
    let acted = loop {
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(10), harness.client.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        let message: Value = serde_json::from_str(&line).unwrap();
        if message["id"] == 7 {
            break message["result"].clone();
        }
    };
    let content = &acted["structuredContent"];
    assert_eq!(content["status"], "unknown", "{acted}");
    assert_eq!(content["retry_action"], false, "{acted}");
    assert_eq!(content["error"]["reason"], "cancelled", "{acted}");
    assert!(desktop.received().is_empty());
    let after = call(&mut harness, ACT_ON_ELEMENT, invoke(&target, &element)).await;
    assert_eq!(
        after["structuredContent"]["error"]["code"], "desktop_work_stopped",
        "{after}"
    );
    harness.finish().await;
}

#[test]
fn act_on_element_is_published_as_input() {
    let tool = tools()
        .into_iter()
        .find(|tool| tool.name == ACT_ON_ELEMENT)
        .unwrap();
    let annotations = tool.annotations.unwrap();
    assert_eq!(annotations.read_only_hint, Some(false));
    assert_eq!(annotations.destructive_hint, Some(true));
    assert_eq!(annotations.open_world_hint, Some(true));
    let spec = catalog::spec(ACT_ON_ELEMENT).unwrap();
    assert!(spec.is_mutating());
    assert_eq!(spec.documented_kind(), "Input");
    assert!(!Profile::observe().serves_tool(ACT_ON_ELEMENT));
    assert_eq!(
        tool.input_schema["required"],
        json!(["element_ref", "action", "target_ref"])
    );
    assert_eq!(
        tool.input_schema["properties"]["action"]["enum"],
        json!(["invoke", "toggle", "expand", "collapse"])
    );
    // Setting a value is a separate decision (#166): no field takes one.
    assert!(tool.input_schema["properties"].get("value").is_none());
}
