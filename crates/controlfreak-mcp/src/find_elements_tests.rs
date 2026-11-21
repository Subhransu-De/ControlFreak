//! `find_elements` over the transport: a read-only observation that claims no session, reports
//! provider failures as errors rather than empty lists, and hands out bounds the pointer tools
//! accept.

use super::fake_desktop::{FakeDesktop, Received};
use super::session_error_tests::{call, harness_with_backend};
use super::*;

fn elements(result: &Value) -> &Vec<Value> {
    assert_ne!(result["isError"], true, "{result}");
    result["structuredContent"]["elements"].as_array().unwrap()
}

#[tokio::test]
async fn finding_elements_claims_no_session_and_its_bounds_drive_a_normal_click() {
    let desktop = FakeDesktop::new();
    let app = desktop.open("Orders", 10);
    let target = desktop.id(app);
    let owned = Arc::new(AtomicBool::new(false));
    let mut harness = harness_with_backend(desktop.clone(), Arc::clone(&owned)).await;

    let found = call(
        &mut harness,
        FIND_ELEMENTS,
        json!({"target_ref": target, "role": "button", "name": "PING"}),
    )
    .await;
    let found_elements = elements(&found);
    assert_eq!(found_elements.len(), 1);
    let button = &found_elements[0];
    assert_eq!(button["role"], "button");
    assert_eq!(button["name"], "Ping");
    assert_eq!(button["patterns"], json!(["invoke"]));
    assert_eq!(found["structuredContent"]["status"], "complete");
    assert!(
        !owned.load(Ordering::SeqCst),
        "an observation reserved the desktop"
    );
    let status = call(&mut harness, GET_SERVER_STATUS, json!({})).await;
    assert_eq!(
        status["structuredContent"]["session"]["approved_target_ref"],
        Value::Null
    );
    assert!(desktop.received().is_empty(), "a query sends no input");

    // The agent clicks through the pointer tools, inside a session, at the element's bounds.
    let bounds = &button["bounds"];
    let x = bounds["x"].as_u64().unwrap() + bounds["width"].as_u64().unwrap() / 2;
    let y = bounds["y"].as_u64().unwrap() + bounds["height"].as_u64().unwrap() / 2;
    let begun = call(
        &mut harness,
        BEGIN_CONTROL_SESSION,
        json!({"target_ref": target}),
    )
    .await;
    assert_ne!(begun["isError"], true, "{begun}");
    let clicked = call(
        &mut harness,
        CLICK_MOUSE,
        json!({"target_ref": target, "display_id": bounds["display_id"], "x": x, "y": y,
               "observation": {"mode": "none"}}),
    )
    .await;
    assert_ne!(clicked["isError"], true, "{clicked}");
    assert_eq!(
        desktop.received(),
        [Received::Click {
            window: app,
            x: u32::try_from(x).unwrap(),
            y: u32::try_from(y).unwrap(),
        }]
    );
    harness.finish().await;
}

#[tokio::test]
async fn an_unanswering_provider_is_an_error_not_an_empty_list() {
    let desktop = FakeDesktop::new();
    let target = desktop.id(desktop.open("Busy", 10));
    let mut harness = harness_with_backend(desktop.clone(), Arc::new(AtomicBool::new(false))).await;
    desktop.stop_accessibility();
    let result = call(&mut harness, FIND_ELEMENTS, json!({"target_ref": target})).await;
    assert_eq!(result["isError"], true, "{result}");
    let error = &result["structuredContent"]["error"];
    assert_eq!(error["code"], "platform_operation_failed");
    assert_eq!(error["reason"], "accessibility_unavailable");
    assert_eq!(error["retryable"], false);
    assert!(result["structuredContent"].get("elements").is_none());
    harness.finish().await;
}

#[tokio::test]
async fn a_closed_window_and_bad_arguments_are_refused() {
    let desktop = FakeDesktop::new();
    let app = desktop.open("Orders", 10);
    let target = desktop.id(app);
    let mut harness = harness_with_backend(desktop.clone(), Arc::new(AtomicBool::new(false))).await;
    for (arguments, code) in [
        (
            json!({"target_ref": target, "max_elements": 0}),
            "invalid_argument",
        ),
        (
            json!({"target_ref": target, "timeout_ms": 50}),
            "invalid_argument",
        ),
        (
            json!({"target_ref": target, "name": ""}),
            "invalid_argument",
        ),
        (
            json!({"target_ref": target, "role": "slider_thing"}),
            "invalid_arguments",
        ),
        (
            json!({"target_ref": target, "value": "x"}),
            "invalid_arguments",
        ),
        (json!({}), "invalid_arguments"),
    ] {
        let result = call(&mut harness, FIND_ELEMENTS, arguments.clone()).await;
        assert_eq!(result["isError"], true, "{arguments}: {result}");
        assert_eq!(
            result["structuredContent"]["error"]["code"], code,
            "{arguments}: {result}"
        );
    }
    desktop.close(app);
    let closed = call(&mut harness, FIND_ELEMENTS, json!({"target_ref": target})).await;
    assert_eq!(closed["isError"], true, "{closed}");
    assert_eq!(
        closed["structuredContent"]["error"]["code"],
        "target_invalidated"
    );
    harness.finish().await;
}

#[tokio::test]
async fn finding_elements_still_works_after_an_emergency_stop() {
    let desktop = FakeDesktop::new();
    let target = desktop.id(desktop.open("Orders", 10));
    let mut harness = harness_with_backend(desktop.clone(), Arc::new(AtomicBool::new(false))).await;
    let stopped = call(&mut harness, STOP_DESKTOP_WORK, json!({})).await;
    assert_ne!(stopped["isError"], true, "{stopped}");
    let found = call(&mut harness, FIND_ELEMENTS, json!({"target_ref": target})).await;
    assert_eq!(elements(&found).len(), 2);
    harness.finish().await;
}

#[test]
fn find_elements_is_published_as_a_read_only_observation() {
    let tool = tools()
        .into_iter()
        .find(|tool| tool.name == FIND_ELEMENTS)
        .unwrap();
    let annotations = tool.annotations.unwrap();
    assert_eq!(annotations.read_only_hint, Some(true));
    assert_eq!(annotations.destructive_hint, Some(false));
    assert_eq!(annotations.open_world_hint, Some(false));
    // A read-only observation, so the observe profile (#36) serves it.
    assert!(Profile::observe().serves_tool(FIND_ELEMENTS));
    let spec = catalog::spec(FIND_ELEMENTS).unwrap();
    assert!(!spec.is_mutating());
    assert_eq!(spec.documented_kind(), "Observe");
    assert_eq!(tool.input_schema["required"], json!(["target_ref"]));
    let defaults: Vec<_> = ["max_depth", "max_elements", "timeout_ms"]
        .iter()
        .map(|name| tool.input_schema["properties"][name]["default"].clone())
        .collect();
    assert_eq!(defaults, [json!(32), json!(100), json!(3000)]);
    // No field accepts a value to write: the tool only reads.
    assert!(tool.input_schema["properties"].get("value").is_none());
}
