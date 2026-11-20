//! #36: a profile hides catalog rows from both `tools/list` and `tools/call`, and a hidden tool
//! is refused like a tool that does not exist, before admission, arbitration or a lease.

use std::collections::BTreeSet;

use super::fake_desktop::FakeDesktop;
use super::prepare_tests::{Server, assert_refused, assert_untouched};
use super::*;

/// One profile of each kind, with the tools each one must serve.
fn profiles() -> Vec<(&'static str, Profile)> {
    vec![
        ("full", Profile::full()),
        ("observe", Profile::observe()),
        (
            "allowlist",
            Profile::full()
                .restrict_to("capture_display, type_text,run_actions,press_keys")
                .unwrap(),
        ),
        (
            "observe allowlist",
            Profile::observe()
                .restrict_to("list_windows,capture_window")
                .unwrap(),
        ),
    ]
}

/// The tools a server with `name`'s profile must list, in catalog order.
fn expected(name: &str) -> Vec<&'static str> {
    let always = [GET_SERVER_STATUS, END_CONTROL_SESSION, STOP_DESKTOP_WORK];
    let restricted: &[&str] = match name {
        "full" => return CATALOG_NAMES.to_vec(),
        "observe" => {
            return catalog::CATALOG
                .iter()
                .filter(|spec| !spec.is_mutating() && !matches!(spec.route, Route::BeginSession))
                .map(|spec| spec.name)
                .collect();
        }
        "allowlist" => &[CAPTURE_DISPLAY, TYPE_TEXT, RUN_ACTIONS, PRESS_KEYS],
        "observe allowlist" => &[LIST_WINDOWS, CAPTURE_WINDOW],
        other => panic!("no profile {other}"),
    };
    catalog::CATALOG
        .iter()
        .map(|spec| spec.name)
        .filter(|name| always.contains(name) || restricted.contains(name))
        .collect()
}

const CATALOG_NAMES: [&str; 27] = [
    GET_SERVER_STATUS,
    BEGIN_CONTROL_SESSION,
    END_CONTROL_SESSION,
    LIST_DISPLAYS,
    CAPTURE_DISPLAY,
    CAPTURE_REGION,
    WAIT_FOR_VISUAL_CHANGE,
    CAPTURE_VISUAL_BASELINE,
    WAIT_FOR_CHANGE_SINCE,
    READ_TEXT_IN_REGION,
    FIND_TEXT_ON_SCREEN,
    WAIT_FOR_TEXT,
    CLICK_TEXT,
    LIST_VIRTUAL_DESKTOPS,
    SWITCH_VIRTUAL_DESKTOP,
    LIST_WINDOWS,
    FOCUS_WINDOW,
    CAPTURE_WINDOW,
    WAIT_FOR_WINDOW,
    MOVE_MOUSE,
    CLICK_MOUSE,
    DRAG_MOUSE,
    SCROLL_MOUSE,
    PRESS_KEYS,
    TYPE_TEXT,
    RUN_ACTIONS,
    STOP_DESKTOP_WORK,
];

async fn listed_names(server: &mut Server) -> Vec<String> {
    let listed = outcome_tests::exchange(
        &mut server.harness.client,
        json!({"jsonrpc":"2.0", "id":3, "method":"tools/list", "params":{}}),
    )
    .await;
    listed["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("{listed}"))
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect()
}

/// The result of a call to a tool that does not exist, with `tool` in place of its name.
fn unknown_tool_result(unknown: &Value, unknown_name: &str, tool: &str) -> Value {
    serde_json::from_str(&unknown.to_string().replace(unknown_name, tool)).unwrap()
}

fn without_meta(mut result: Value) -> Value {
    result.as_object_mut().unwrap().remove("_meta");
    result
}

#[tokio::test]
async fn for_each_profile_the_listed_tools_are_exactly_the_callable_tools() {
    let catalog_names: Vec<&str> = catalog::CATALOG.iter().map(|spec| spec.name).collect();
    assert_eq!(
        catalog_names, CATALOG_NAMES,
        "the fixture follows the catalog"
    );
    let unknown_name = "no_such_controlfreak_tool";
    for (name, profile) in profiles() {
        let desktop = FakeDesktop::new();
        let owned = Arc::new(AtomicBool::new(false));
        let mut server = Server::start_with_profile(&desktop, &owned, profile.clone()).await;

        let listed = listed_names(&mut server).await;
        assert_eq!(listed, expected(name), "{name}");
        for always in [GET_SERVER_STATUS, END_CONTROL_SESSION, STOP_DESKTOP_WORK] {
            assert!(listed.iter().any(|tool| tool == always), "{name}: {always}");
        }

        // An unknown field is refused by every tool's argument parser before it does any work,
        // so the call shows whether dispatch knows the tool without changing anything.
        let probe = json!({"profile_probe_field": true});
        let unknown = without_meta(server.call(unknown_name, probe.clone()).await);
        assert_eq!(
            unknown["structuredContent"]["error"]["code"], "invalid_arguments",
            "{unknown}"
        );
        let mut accepted = Vec::new();
        for tool in CATALOG_NAMES {
            let result = without_meta(server.call(tool, probe.clone()).await);
            let hidden = unknown_tool_result(&unknown, unknown_name, tool);
            if result == hidden {
                continue;
            }
            assert_eq!(result["isError"], true, "{name} {tool}: {result}");
            assert_eq!(
                result["structuredContent"]["error"]["details"]["problem"], "unknown_field",
                "{name} {tool}: {result}"
            );
            accepted.push(tool.to_owned());
        }
        assert_eq!(
            accepted, listed,
            "{name}: tools/call accepts what tools/list lists"
        );

        let status = server.call(GET_SERVER_STATUS, json!({})).await;
        assert_eq!(
            status["structuredContent"]["profile"],
            profile.status_json(),
            "{name}: {status}"
        );
        assert_eq!(server.acquisitions.load(Ordering::SeqCst), 0, "{name}");
        assert!(desktop.received().is_empty(), "{name}");
        server.harness.finish().await;
    }
}

/// Valid arguments for every tool that changes or reserves the desktop.
fn mutating_calls(target: &str) -> Vec<(&'static str, Value)> {
    let none = json!({"mode": "none"});
    vec![
        (
            BEGIN_CONTROL_SESSION,
            json!({"target_ref": target, "expected_seconds": 5}),
        ),
        (FOCUS_WINDOW, json!({"window_id": target})),
        (
            SWITCH_VIRTUAL_DESKTOP,
            json!({"target_ref": target, "direction": "right", "observation": none}),
        ),
        (
            MOVE_MOUSE,
            json!({"target_ref": target, "x": 5, "y": 5, "observation": none}),
        ),
        (
            CLICK_MOUSE,
            json!({"target_ref": target, "x": 5, "y": 5, "observation": none}),
        ),
        (
            DRAG_MOUSE,
            json!({"target_ref": target, "start_x": 5, "start_y": 5, "end_x": 9, "end_y": 9,
                   "observation": none}),
        ),
        (
            SCROLL_MOUSE,
            json!({"target_ref": target, "x": 5, "y": 5, "delta_y": -1, "observation": none}),
        ),
        (
            PRESS_KEYS,
            json!({"target_ref": target, "keys": ["enter"], "observation": none}),
        ),
        (
            TYPE_TEXT,
            json!({"target_ref": target, "text": "a", "observation": none}),
        ),
        (
            CLICK_TEXT,
            json!({"target_ref": target, "x": 0, "y": 0, "width": 10, "height": 10,
                   "query": "Save", "observation": none}),
        ),
        (
            RUN_ACTIONS,
            json!({"target_ref": target, "steps": [{"action": "press_keys", "keys": ["enter"]}],
                   "observation": none}),
        ),
    ]
}

#[tokio::test]
async fn observe_refuses_every_mutating_tool_before_it_takes_the_desktop() {
    let desktop = FakeDesktop::new();
    let target = desktop.id(desktop.open("Editor", 10));
    let calls = mutating_calls(&target);
    let named: BTreeSet<&str> = calls.iter().map(|(name, _)| *name).collect();
    let mutating: BTreeSet<&str> = catalog::CATALOG
        .iter()
        .filter(|spec| spec.is_mutating() || matches!(spec.route, Route::BeginSession))
        .map(|spec| spec.name)
        .collect();
    assert_eq!(named, mutating, "every tool observe hides is called");

    // The arguments are valid: a full server takes each call past its argument checks.
    let owned = Arc::new(AtomicBool::new(false));
    let mut full = Server::start(&desktop, &owned).await;
    for (name, arguments) in &calls {
        let result = full.call(name, arguments.clone()).await;
        let code = result["structuredContent"]["error"]["code"]
            .as_str()
            .unwrap_or_default();
        assert!(
            !["invalid_arguments", "invalid_argument"].contains(&code),
            "{name}: {result}"
        );
    }
    full.call(END_CONTROL_SESSION, json!({})).await;
    full.harness.finish().await;
    assert!(
        !owned.load(Ordering::SeqCst),
        "the full server released the desktop"
    );

    let desktop = FakeDesktop::new();
    let target = desktop.id(desktop.open("Editor", 10));
    let owned = Arc::new(AtomicBool::new(false));
    let mut observe = Server::start_with_profile(&desktop, &owned, Profile::observe()).await;
    let unknown = without_meta(observe.call("no_such_controlfreak_tool", json!({})).await);
    for (name, arguments) in mutating_calls(&target) {
        let result = without_meta(observe.call(name, arguments).await);
        assert_eq!(
            result,
            unknown_tool_result(&unknown, "no_such_controlfreak_tool", name),
            "{name}: a hidden tool is refused like an unknown tool"
        );
        assert!(
            result["structuredContent"].get("status").is_none(),
            "{name}: {result}"
        );
        assert_untouched(&observe.session().await);
    }
    assert_eq!(observe.acquisitions.load(Ordering::SeqCst), 0);
    assert_eq!(observe.indicator_starts.load(Ordering::SeqCst), 0);
    assert!(!owned.load(Ordering::SeqCst));
    assert!(desktop.received().is_empty());

    // Observation and the always-served tools still work.
    let windows = observe.call(LIST_WINDOWS, json!({})).await;
    assert_ne!(windows["isError"], true, "{windows}");
    let ended = observe.call(END_CONTROL_SESSION, json!({})).await;
    assert_eq!(ended["structuredContent"]["status"], "not_owned", "{ended}");
    let stopped = observe.call(STOP_DESKTOP_WORK, json!({})).await;
    assert_ne!(stopped["isError"], true, "{stopped}");
    observe.harness.finish().await;
}

#[tokio::test]
async fn an_allowlist_without_type_text_refuses_a_batch_with_a_type_step_before_any_input() {
    let desktop = FakeDesktop::new();
    let target = desktop.id(desktop.open("Editor", 10));
    let owned = Arc::new(AtomicBool::new(false));
    let profile = Profile::full()
        .restrict_to("run_actions,click_mouse,press_keys")
        .unwrap();
    let mut server = Server::start_with_profile(&desktop, &owned, profile).await;

    let batch = server
        .call(
            RUN_ACTIONS,
            json!({"target_ref": target, "steps": [
                {"action": "press_keys", "keys": ["enter"]},
                {"action": "wait", "wait_ms": 1},
                {"action": "type_text", "text": "private words"},
                {"action": "click", "x": 5, "y": 5},
            ], "observation": {"mode": "none"}}),
        )
        .await;
    assert_refused(RUN_ACTIONS, &batch, "invalid_argument");
    let message = batch["structuredContent"]["error"]["message"]
        .as_str()
        .unwrap();
    assert!(message.contains("steps[2].action"), "{batch}");
    assert!(message.contains("type_text"), "{batch}");
    assert!(!batch.to_string().contains("private"), "{batch}");
    assert_untouched(&server.session().await);
    assert_eq!(server.acquisitions.load(Ordering::SeqCst), 0);
    assert!(desktop.received().is_empty());

    // A scroll step needs scroll_mouse, which this list leaves out too.
    let scroll = server
        .call(
            RUN_ACTIONS,
            json!({"target_ref": target, "steps": [
                {"action": "scroll", "x": 5, "y": 5, "delta_y": -1},
            ]}),
        )
        .await;
    assert_refused(RUN_ACTIONS, &scroll, "invalid_argument");
    assert!(desktop.received().is_empty());

    // The tool itself is hidden, and a batch of allowed steps runs.
    let typed = server
        .call(TYPE_TEXT, json!({"target_ref": target, "text": "a"}))
        .await;
    assert_eq!(
        typed["structuredContent"]["error"]["message"], "unknown ControlFreak tool 'type_text'",
        "{typed}"
    );
    let allowed = server
        .call(
            RUN_ACTIONS,
            json!({"target_ref": target, "steps": [
                {"action": "wait", "wait_ms": 1},
                {"action": "press_keys", "keys": ["enter"]},
            ], "observation": {"mode": "none"}}),
        )
        .await;
    assert_eq!(
        allowed["structuredContent"]["status"], "completed",
        "{allowed}"
    );
    assert!(!desktop.received().is_empty());
    server.harness.finish().await;
}

#[test]
fn a_tool_list_is_parsed_strictly() {
    let full = Profile::full;
    assert_eq!(full().restrict_to(""), Err(ProfileError::EmptyList));
    assert_eq!(full().restrict_to(" "), Err(ProfileError::EmptyList));
    for list in [
        ",",
        "type_text,",
        ",type_text",
        "type_text,,press_keys",
        "type_text, ",
    ] {
        assert_eq!(
            full().restrict_to(list),
            Err(ProfileError::EmptyEntry),
            "{list}"
        );
    }
    assert_eq!(
        full().restrict_to("type_text,typo_text"),
        Err(ProfileError::UnknownTool("typo_text".to_owned()))
    );
    assert_eq!(
        full().restrict_to("Type_Text"),
        Err(ProfileError::UnknownTool("Type_Text".to_owned())),
        "names are case-sensitive"
    );
    assert_eq!(
        full().restrict_to("type_text,press_keys,type_text"),
        Err(ProfileError::Duplicate(TYPE_TEXT))
    );
    assert_eq!(
        Profile::observe().restrict_to("list_windows,type_text"),
        Err(ProfileError::NotServedByObserve(TYPE_TEXT))
    );
    assert_eq!(
        Profile::observe().restrict_to("begin_control_session"),
        Err(ProfileError::NotServedByObserve(BEGIN_CONTROL_SESSION))
    );

    // An error repeats at most 64 characters of what it was given.
    let long = "x".repeat(10_000);
    let error = full().restrict_to(&long).unwrap_err();
    assert_eq!(error, ProfileError::UnknownTool("x".repeat(64)));
    assert!(error.to_string().len() < 1024, "{error}");
    assert!(error.to_string().contains("list_windows"), "{error}");

    // Spaces around names are ignored, the order is the catalog's, and the tools every profile
    // keeps may be named.
    let profile = full()
        .restrict_to(" type_text , get_server_status,list_windows")
        .unwrap();
    assert_eq!(
        profile.status_json(),
        json!({"name": "full", "tool_allowlist": [GET_SERVER_STATUS, LIST_WINDOWS, TYPE_TEXT]})
    );
    let names: Vec<&str> = profile.specs().map(|spec| spec.name).collect();
    assert_eq!(
        names,
        [
            GET_SERVER_STATUS,
            END_CONTROL_SESSION,
            LIST_WINDOWS,
            TYPE_TEXT,
            STOP_DESKTOP_WORK
        ]
    );
    assert!(!profile.is_full());
    assert!(Profile::full().is_full());
    assert_eq!(Profile::default(), Profile::full());
    assert_eq!(
        Profile::observe().status_json(),
        json!({"name": "observe", "tool_allowlist": null})
    );
}

#[test]
fn a_restricted_server_tells_the_agent_that_tools_are_missing() {
    let backend: Arc<dyn PlatformBackend> = Arc::new(stop_tests::WaitingBackend::idle());
    let instructions = |profile: Profile| {
        ControlFreakServer::with_indicator(Arc::clone(&backend), SafetyIndicator::dormant(), None)
            .with_profile(profile)
            .get_info()
            .instructions
            .unwrap()
    };
    assert_eq!(instructions(Profile::full()), SERVER_INSTRUCTIONS);
    for profile in [
        Profile::observe(),
        Profile::full().restrict_to("list_windows").unwrap(),
    ] {
        let restricted = instructions(profile);
        assert!(restricted.starts_with(SERVER_INSTRUCTIONS));
        assert!(restricted.ends_with(RESTRICTED_PROFILE_INSTRUCTIONS));
    }
}
