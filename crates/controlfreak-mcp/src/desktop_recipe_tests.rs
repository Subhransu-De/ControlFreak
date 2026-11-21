//! Recovery recipes an agent depends on, driven through the real tools against a stateful fake
//! desktop. Each test asserts what the tools returned and what the desktop received.

use super::fake_desktop::{FakeDesktop, Received};
use super::session_error_tests::{Harness, call, harness_with_backend};
use super::*;
use controlfreak_core::{ActivationHelperOutcome, WindowBounds};

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

/// The reference of the window `list_windows` reports in the foreground.
async fn foreground(harness: &mut Harness) -> String {
    let windows = call(harness, LIST_WINDOWS, json!({})).await;
    let listed = windows["structuredContent"]["windows"].as_array().unwrap();
    let front: Vec<&Value> = listed
        .iter()
        .filter(|window| window["is_foreground"] == true)
        .collect();
    assert_eq!(front.len(), 1, "{windows}");
    front[0]["id"].as_str().unwrap().to_owned()
}

/// The foreground lock, without a desktop: Windows refuses the server's own request after the
/// user worked in another application, and the activation helper's answer decides the result.
#[tokio::test]
async fn the_activation_helper_answer_decides_a_refused_focus() {
    let focus = |target: &str| json!({"window_id": target, "observation": {"mode": "none"}});

    // Own call refused, helper accepted: the window is in front and takes input.
    let desktop = FakeDesktop::new();
    let app = desktop.open("Editor", 10);
    let target = desktop.id(app);
    let mut harness = start(&desktop).await;
    begin(&mut harness, &target).await;
    desktop.open("Chat notification", 20);
    desktop.refuse_focus(1);
    desktop.answer_helper(ActivationHelperOutcome::Accepted);
    let focused = call(&mut harness, FOCUS_WINDOW, focus(&target)).await;
    accepted(&focused);
    let input = &focused["structuredContent"]["input"];
    assert_eq!(input["activation_helper"], "accepted", "{focused}");
    assert_eq!(foreground(&mut harness).await, target);
    accepted(&click(&mut harness, &target).await);
    assert_eq!(
        desktop.received(),
        [Received::Focus { window: app }, clicked(app)]
    );
    harness.finish().await;

    // Own call and helper refused: `activation_refused`, nothing sent, the session kept.
    let desktop = FakeDesktop::new();
    let app = desktop.open("Editor", 10);
    let target = desktop.id(app);
    let mut harness = start(&desktop).await;
    begin(&mut harness, &target).await;
    let chat = desktop.open("Chat notification", 20);
    desktop.refuse_focus(1);
    desktop.answer_helper(ActivationHelperOutcome::Refused);
    let refused = call(&mut harness, FOCUS_WINDOW, focus(&target)).await;
    assert_eq!(refused["isError"], true, "{refused}");
    let structured = &refused["structuredContent"];
    assert_eq!(
        structured["error"]["code"], "activation_refused",
        "{refused}"
    );
    assert_eq!(structured["error"]["retryable"], true, "{refused}");
    assert_eq!(
        structured["input"]["activation_helper"], "refused",
        "{refused}"
    );
    assert_eq!(foreground(&mut harness).await, desktop.id(chat));
    assert!(desktop.received().is_empty());
    accepted(&call(&mut harness, FOCUS_WINDOW, focus(&target)).await);
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

fn assert_moved(result: &Value) {
    assert_eq!(refused_code(result), "target_invalidated");
    let error = &result["structuredContent"]["error"];
    assert_eq!(error["reason"], "target_moved", "{result}");
    assert_eq!(error["retryable"], true, "{result}");
}

#[tokio::test]
async fn a_resized_window_keeps_its_session_after_one_refused_action() {
    let desktop = FakeDesktop::new();
    let app = desktop.open("Editor", 10);
    let target = desktop.id(app);
    let mut harness = start(&desktop).await;
    begin(&mut harness, &target).await;
    accepted(&click(&mut harness, &target).await);

    // Maximize, restore or a drag: the same window, so the same reference.
    desktop.resize(app);
    assert_eq!(desktop.id(app), target);
    assert_moved(&click(&mut harness, &target).await);
    // Refused once: the next action, with coordinates for the new bounds, needs no new session.
    accepted(&click(&mut harness, &target).await);
    assert_eq!(desktop.received(), [clicked(app), clicked(app)]);

    // Every later move is refused once again.
    desktop.resize(app);
    assert_moved(
        &call(
            &mut harness,
            TYPE_TEXT,
            json!({"target_ref": target, "text": "x", "observation": {"mode": "none"}}),
        )
        .await,
    );
    accepted(&click(&mut harness, &target).await);
    assert_eq!(desktop.received().len(), 3, "the refused text sent nothing");
    harness.finish().await;
}

#[tokio::test]
async fn an_image_of_the_approved_window_from_before_a_move_never_maps_points() {
    let desktop = FakeDesktop::new();
    let app = desktop.open("Editor", 10);
    let target = desktop.id(app);
    let mut harness = start(&desktop).await;
    begin(&mut harness, &target).await;
    let point = |capture: &Value| {
        json!({
            "target_ref": target,
            "observation_id": capture["structuredContent"]["observation_id"],
            "image_x": 10,
            "image_y": 10,
            "observation": {"mode": "none"},
        })
    };
    let before = call(&mut harness, CAPTURE_WINDOW, json!({"window_id": target})).await;
    accepted(&before);

    desktop.resize(app);
    let stale = call(&mut harness, CLICK_MOUSE, point(&before)).await;
    assert_eq!(refused_code(&stale), "invalid_argument");
    assert!(
        stale.to_string().contains("moved, resized or closed"),
        "{stale}"
    );
    // Display coordinates are not refused by the image check, but the move is.
    assert_moved(&click(&mut harness, &target).await);
    // The reference took the new bounds, so it is unchanged; the old image is still refused.
    let still_stale = call(&mut harness, CLICK_MOUSE, point(&before)).await;
    assert_eq!(refused_code(&still_stale), "invalid_argument");
    assert!(desktop.received().is_empty(), "nothing was sent");

    let after = call(&mut harness, CAPTURE_WINDOW, json!({"window_id": target})).await;
    accepted(&after);
    assert_eq!(
        after["structuredContent"]["window"]["id"], target,
        "an observation returns the session's reference"
    );
    accepted(&call(&mut harness, CLICK_MOUSE, point(&after)).await);
    // The new image maps through the new bounds: the window starts at 100, 100.
    assert_eq!(
        desktop.received(),
        [Received::Click {
            window: app,
            x: 110,
            y: 110
        }]
    );
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

/// The result's `observation.foreground_window`, after checking the result against the tool's
/// full output schema.
fn foreground_window(result: &Value, tool: &str) -> Value {
    accepted(result);
    let structured = &result["structuredContent"];
    let validator = super::output_schema_tests::full_validator(tool);
    if let Err(error) = validator.validate(structured) {
        panic!("{tool} result does not match its schema: {error}: {structured}");
    }
    structured["observation"]["foreground_window"].clone()
}

#[tokio::test]
async fn an_untitled_popup_in_front_is_described_by_class_and_process_only() {
    let desktop = FakeDesktop::new();
    let app = desktop.open("Editor", 10);
    let target = desktop.id(app);
    let mut harness = start(&desktop).await;
    begin(&mut harness, &target).await;

    // A popup menu of the target's application has no title, so it gets no reference.
    desktop.open_owned(app, "");
    let result = call(
        &mut harness,
        PRESS_KEYS,
        json!({"target_ref": target, "keys": ["down"], "observation": {"mode": "none"}}),
    )
    .await;
    let window = foreground_window(&result, PRESS_KEYS);
    assert_ne!(
        window,
        Value::Null,
        "a popup in front is not 'no foreground'"
    );
    assert_eq!(
        window,
        json!({"id": null, "class_name": "FakePopup", "process_id": 10, "same_process_as_target": true})
    );
    assert_eq!(
        result["structuredContent"]["input"]["target_remained_foreground"], true,
        "{result}"
    );
    harness.finish().await;
}

#[tokio::test]
async fn a_titled_dialog_in_front_is_described_as_list_windows_lists_it() {
    let desktop = FakeDesktop::new();
    let app = desktop.open("Editor", 10);
    let target = desktop.id(app);
    let mut harness = start(&desktop).await;
    begin(&mut harness, &target).await;

    let dialog = desktop.open_owned(app, "Save as");
    let window = foreground_window(&click(&mut harness, &target).await, CLICK_MOUSE);
    assert_eq!(window["id"], desktop.id(dialog), "{window}");
    assert_eq!(window["owner"], target, "{window}");
    let listed = call(&mut harness, LIST_WINDOWS, json!({})).await;
    let entry = listed["structuredContent"]["windows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == window["id"])
        .cloned()
        .unwrap();
    assert_eq!(window, entry, "the same description as list_windows");
    harness.finish().await;
}

#[tokio::test]
async fn every_run_actions_input_step_reports_the_window_in_front() {
    let desktop = FakeDesktop::new();
    let app = desktop.open("Editor", 10);
    let target = desktop.id(app);
    let mut harness = start(&desktop).await;
    begin(&mut harness, &target).await;

    desktop.open_owned(app, "");
    let result = call(
        &mut harness,
        RUN_ACTIONS,
        json!({"target_ref": target, "steps": [
            {"action": "press_keys", "keys": ["down"]},
            {"action": "wait", "wait_ms": 1},
            {"action": "press_keys", "keys": ["enter"]}
        ], "observation": {"mode": "none"}}),
    )
    .await;
    let popup = json!({"id": null, "class_name": "FakePopup", "process_id": 10, "same_process_as_target": true});
    assert_eq!(foreground_window(&result, RUN_ACTIONS), popup);
    let steps = result["structuredContent"]["action"]["steps"]
        .as_array()
        .unwrap();
    assert_eq!(steps.len(), 3, "{result}");
    assert_eq!(steps[0]["foreground_window"], popup, "{result}");
    // A wait sends no input and reads nothing; the step after it reports the window in front.
    assert!(steps[1].get("foreground_window").is_none(), "{result}");
    assert_eq!(steps[2]["foreground_window"], popup, "{result}");
    harness.finish().await;
}

fn window_bounds(left: i32, top: i32, width: u32, height: u32) -> WindowBounds {
    WindowBounds {
        left,
        top,
        width,
        height,
    }
}

/// A capture's structured result after checking it against the tool's full output schema.
fn window_capture(result: &Value) -> Value {
    accepted(result);
    let structured = result["structuredContent"].clone();
    let validator = super::output_schema_tests::full_validator(CAPTURE_WINDOW);
    if let Err(error) = validator.validate(&structured) {
        panic!("capture_window result does not match its schema: {error}: {structured}");
    }
    structured
}

#[tokio::test]
async fn a_capture_with_popups_covers_a_same_process_popup_outside_the_window_and_maps_into_it() {
    let desktop = FakeDesktop::new();
    // The editor is at (100, 100) and 800 x 600, so it ends at x = 900.
    let app = desktop.open("Editor", 10);
    let target = desktop.id(app);
    let mut harness = start(&desktop).await;
    begin(&mut harness, &target).await;
    // A window of another process that claims the editor as its owner is never included.
    desktop.open_popup(app, 20, window_bounds(40, 760, 100, 100));
    // A menu that opens past the editor's right edge.
    let menu = desktop.open_popup(app, 10, window_bounds(850, 300, 200, 150));

    let capture = window_capture(
        &call(
            &mut harness,
            CAPTURE_WINDOW,
            json!({"window_id": target, "include_popups": true}),
        )
        .await,
    );
    assert_eq!(
        capture["source_bounds"],
        json!({"left": 100, "top": 100, "width": 950, "height": 600}),
        "{capture}"
    );
    assert_eq!(
        capture["included_windows"],
        json!([{
            "class_name": "FakePopup",
            "process_id": 10,
            "bounds": {"left": 850, "top": 300, "width": 200, "height": 150},
            "capture_method": "windows_print_window",
        }]),
        "{capture}"
    );
    assert_eq!(capture["window"]["id"], target);
    let point = |image_x: u32, image_y: u32| {
        json!({
            "target_ref": target,
            "observation_id": capture["observation_id"],
            "image_x": image_x,
            "image_y": image_y,
            "observation": {"mode": "none"},
        })
    };
    // (1000, 350) on the screen is inside the menu and outside the editor.
    accepted(&call(&mut harness, CLICK_MOUSE, point(900, 250)).await);
    assert_eq!(
        desktop.received(),
        [Received::Click {
            window: menu,
            x: 1000,
            y: 350
        }]
    );

    desktop.close(menu);
    let closed = call(&mut harness, CLICK_MOUSE, point(900, 250)).await;
    assert_eq!(refused_code(&closed), "invalid_argument");
    assert!(
        closed
            .to_string()
            .contains("popup at this image point closed"),
        "{closed}"
    );
    // Another menu of the same class, process and bounds is not the one in the image.
    let replacement = desktop.open_popup(app, 10, window_bounds(850, 300, 200, 150));
    let replaced = call(&mut harness, CLICK_MOUSE, point(900, 250)).await;
    assert_eq!(refused_code(&replaced), "invalid_argument");
    desktop.close(replacement);
    // A point on the editor does not depend on the menu, so it still maps.
    accepted(&call(&mut harness, CLICK_MOUSE, point(10, 10)).await);
    assert_eq!(
        desktop.received()[1..],
        [Received::Click {
            window: app,
            x: 110,
            y: 110
        }]
    );
    harness.finish().await;
}

#[tokio::test]
async fn a_capture_without_include_popups_is_unchanged_while_a_popup_is_open() {
    let desktop = FakeDesktop::new();
    let app = desktop.open("Editor", 10);
    let target = desktop.id(app);
    desktop.open_popup(app, 10, window_bounds(850, 300, 200, 150));
    let mut harness = start(&desktop).await;
    // The image and the structured result without its per-call observation_id.
    let essence = |result: &Value| {
        let mut structured = window_capture(result);
        let id = structured.as_object_mut().unwrap().remove("observation_id");
        assert!(id.is_some(), "{result}");
        (result["content"][0].to_string(), structured.to_string())
    };
    let default = call(&mut harness, CAPTURE_WINDOW, json!({"window_id": target})).await;
    let structured = window_capture(&default);
    let mut fields: Vec<_> = structured.as_object().unwrap().keys().cloned().collect();
    fields.sort();
    assert_eq!(
        fields,
        [
            "byte_length",
            "capture_method",
            "cursor_marker",
            "image_height",
            "image_width",
            "mime_type",
            "observation_id",
            "source_bounds",
            "window"
        ],
        "the result has exactly the fields it had before include_popups"
    );
    assert_eq!(
        structured["source_bounds"],
        json!({"left": 100, "top": 100, "width": 800, "height": 600})
    );
    let explicit = call(
        &mut harness,
        CAPTURE_WINDOW,
        json!({"window_id": target, "include_popups": false}),
    )
    .await;
    assert_eq!(essence(&explicit), essence(&default));
    harness.finish().await;
}
