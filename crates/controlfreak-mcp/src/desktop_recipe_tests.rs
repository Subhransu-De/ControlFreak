//! Recovery recipes an agent depends on, driven through the real tools against a stateful fake
//! desktop. Each test asserts what the tools returned and what the desktop received.

use super::fake_desktop::{FakeDesktop, Received};
use super::session_error_tests::{Harness, call, harness_with_backend};
use super::*;

async fn start(desktop: &Arc<FakeDesktop>) -> Harness {
    harness_with_backend(desktop.clone(), Arc::new(AtomicBool::new(false))).await
}

fn refused_code(result: &Value) -> &str {
    assert_eq!(result["isError"], true, "{result}");
    assert_eq!(
        result["structuredContent"]["status"], "not_started",
        "a refused action sends nothing: {result}"
    );
    result["structuredContent"]["error"]["code"]
        .as_str()
        .unwrap()
}

fn accepted(result: &Value) {
    assert_ne!(result["isError"], true, "{result}");
}

async fn begin(harness: &mut Harness, target: &str) {
    accepted(
        &call(
            harness,
            BEGIN_CONTROL_SESSION,
            json!({"target_ref": target}),
        )
        .await,
    );
}

async fn click(harness: &mut Harness, target: &str) -> Value {
    call(
        harness,
        CLICK_MOUSE,
        json!({"target_ref": target, "x": 5, "y": 6, "observation": {"mode": "none"}}),
    )
    .await
}

fn clicked(window: u32) -> Received {
    Received::Click { window, x: 5, y: 6 }
}

#[tokio::test]
async fn a_dialog_of_the_same_process_is_accepted() {
    let desktop = FakeDesktop::new();
    let app = desktop.open("Editor", 10);
    let target = desktop.id(app);
    let mut harness = start(&desktop).await;
    begin(&mut harness, &target).await;

    let dialog = desktop.open_owned(app, "Save as");
    accepted(
        &call(
            &mut harness,
            TYPE_TEXT,
            json!({"target_ref": target, "text": "notes.txt", "observation": {"mode": "none"}}),
        )
        .await,
    );
    accepted(&click(&mut harness, &target).await);
    assert_eq!(
        desktop.received(),
        [
            Received::Text {
                window: dialog,
                text: "notes.txt".into()
            },
            clicked(dialog),
        ],
        "input reaches the dialog that holds the foreground"
    );

    // The dialog is listed with its owner so a client can tell what it is.
    let listed = call(&mut harness, LIST_WINDOWS, json!({})).await;
    let windows = listed["structuredContent"]["windows"].as_array().unwrap();
    let entry = windows.iter().find(|w| w["title"] == "Save as").unwrap();
    assert_eq!(entry["owner"], target);
    harness.finish().await;
}

#[tokio::test]
async fn a_popup_menu_takes_the_click_and_closing_it_returns_to_the_window() {
    let desktop = FakeDesktop::new();
    let app = desktop.open("Editor", 10);
    let target = desktop.id(app);
    let mut harness = start(&desktop).await;
    begin(&mut harness, &target).await;

    let popup = desktop.open_owned(app, "Context menu");
    accepted(&click(&mut harness, &target).await);
    desktop.close(popup);
    accepted(&click(&mut harness, &target).await);
    assert_eq!(desktop.received(), [clicked(popup), clicked(app)]);
    harness.finish().await;
}

#[tokio::test]
async fn a_foreign_foreground_is_refused_and_focus_window_recovers_the_session() {
    let desktop = FakeDesktop::new();
    let app = desktop.open("Editor", 10);
    let target = desktop.id(app);
    let mut harness = start(&desktop).await;
    begin(&mut harness, &target).await;

    desktop.open("Chat notification", 20);
    let refused = click(&mut harness, &target).await;
    assert_eq!(refused_code(&refused), "target_invalidated");
    // The agent can tell this recoverable refusal from a fatal change without reading prose.
    let error = &refused["structuredContent"]["error"];
    assert_eq!(error["reason"], "not_foreground", "{refused}");
    assert_eq!(error["retryable"], true, "{refused}");
    assert!(desktop.received().is_empty(), "nothing reached any window");

    // The refusal did not poison the session: focusing the target makes the next action work.
    accepted(
        &call(
            &mut harness,
            FOCUS_WINDOW,
            json!({"window_id": target, "observation": {"mode": "none"}}),
        )
        .await,
    );
    accepted(&click(&mut harness, &target).await);
    assert_eq!(
        desktop.received(),
        [Received::Focus { window: app }, clicked(app)]
    );
    harness.finish().await;
}

#[tokio::test]
async fn a_refused_focus_is_retryable_and_does_not_poison_the_session() {
    let desktop = FakeDesktop::new();
    let app = desktop.open("Editor", 10);
    let target = desktop.id(app);
    let mut harness = start(&desktop).await;
    begin(&mut harness, &target).await;
    desktop.open("Chat notification", 20);
    desktop.refuse_focus(1);

    let focus = || json!({"window_id": target, "observation": {"mode": "none"}});
    let refused = call(&mut harness, FOCUS_WINDOW, focus()).await;
    assert_eq!(refused["isError"], true, "{refused}");
    let error = &refused["structuredContent"]["error"];
    assert_eq!(error["code"], "activation_refused", "{refused}");
    assert_eq!(error["retryable"], true, "{refused}");

    // The retry the error asks for works in the same session, and so does input afterwards.
    accepted(&call(&mut harness, FOCUS_WINDOW, focus()).await);
    accepted(&click(&mut harness, &target).await);
    assert_eq!(
        desktop.received(),
        [Received::Focus { window: app }, clicked(app)]
    );
    harness.finish().await;
}

#[tokio::test]
async fn focus_window_can_bring_back_a_dialog_of_the_approved_application() {
    let desktop = FakeDesktop::new();
    let app = desktop.open("Editor", 10);
    let target = desktop.id(app);
    let mut harness = start(&desktop).await;
    begin(&mut harness, &target).await;
    let dialog = desktop.open_owned(app, "Save as");
    desktop.open("Chat notification", 20);

    // The session stays on the editor; the dialog of the same process can be focused in it.
    accepted(
        &call(
            &mut harness,
            FOCUS_WINDOW,
            json!({"window_id": desktop.id(dialog), "observation": {"mode": "none"}}),
        )
        .await,
    );
    accepted(&click(&mut harness, &target).await);
    assert_eq!(
        desktop.received(),
        [Received::Focus { window: dialog }, clicked(dialog)]
    );

    // A window of another process still needs its own session.
    let stranger = desktop.open("Other app", 30);
    let refused = call(
        &mut harness,
        FOCUS_WINDOW,
        json!({"window_id": desktop.id(stranger), "observation": {"mode": "none"}}),
    )
    .await;
    assert_eq!(refused["isError"], true, "{refused}");
    harness.finish().await;
}

#[tokio::test]
async fn a_resized_window_loses_its_reference_and_needs_a_new_session() {
    let desktop = FakeDesktop::new();
    let app = desktop.open("Editor", 10);
    let old = desktop.id(app);
    let mut harness = start(&desktop).await;
    begin(&mut harness, &old).await;
    accepted(&click(&mut harness, &old).await);

    desktop.resize(app);
    let new = desktop.id(app);
    assert_ne!(old, new);
    assert_eq!(
        refused_code(&click(&mut harness, &old).await),
        "target_invalidated"
    );
    // The session cannot adopt the new reference: the target changed under it.
    assert_eq!(
        refused_code(&click(&mut harness, &new).await),
        "target_invalidated"
    );

    accepted(&call(&mut harness, END_CONTROL_SESSION, json!({})).await);
    begin(&mut harness, &new).await;
    accepted(&click(&mut harness, &new).await);
    assert_eq!(desktop.received(), [clicked(app), clicked(app)]);
    harness.finish().await;
}

#[tokio::test]
async fn a_screenshot_of_a_resized_window_no_longer_maps_points() {
    let desktop = FakeDesktop::new();
    let other = desktop.open("Reference", 10);
    let app = desktop.open("Editor", 10);
    let target = desktop.id(app);
    let mut harness = start(&desktop).await;
    begin(&mut harness, &target).await;
    // The image shows a second window of the same process; the session stays on the editor.
    let shown = desktop.id(other);
    let capture = call(&mut harness, CAPTURE_WINDOW, json!({"window_id": shown})).await;
    accepted(&capture);
    let point = json!({
        "target_ref": target,
        "observation_id": capture["structuredContent"]["observation_id"],
        "image_x": 10,
        "image_y": 10,
        "observation": {"mode": "none"},
    });
    accepted(&call(&mut harness, CLICK_MOUSE, point.clone()).await);

    desktop.resize(other);
    let stale = call(&mut harness, CLICK_MOUSE, point).await;
    assert_eq!(stale["isError"], true, "{stale}");
    assert!(
        stale.to_string().contains("moved, resized or closed"),
        "{stale}"
    );
    assert_eq!(desktop.received().len(), 1, "the stale image sent nothing");
    harness.finish().await;
}

#[tokio::test]
async fn a_closed_window_ends_the_approval() {
    let desktop = FakeDesktop::new();
    let app = desktop.open("Editor", 10);
    let target = desktop.id(app);
    let mut harness = start(&desktop).await;
    begin(&mut harness, &target).await;
    desktop.close(app);
    assert_eq!(
        refused_code(&click(&mut harness, &target).await),
        "target_invalidated"
    );
    harness.finish().await;
}
