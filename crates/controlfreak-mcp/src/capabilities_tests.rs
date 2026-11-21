//! #23 and #9: `get_server_status.capabilities` reports OCR and helper readiness, the hard
//! limits and the forbidden tool families without any input, and `wait_for_window` selects no
//! window when several match.

use controlfreak_core::{
    HelperReadiness, OcrReadiness, Readiness, ReadinessReason, RuntimeReadiness,
};

use super::fake_desktop::FakeDesktop;
use super::session_error_tests::{Harness, call, harness_with_backend};
use super::*;

async fn start(desktop: &Arc<FakeDesktop>) -> Harness {
    harness_with_backend(desktop.clone(), Arc::new(AtomicBool::new(false))).await
}

fn ready_readiness() -> RuntimeReadiness {
    RuntimeReadiness {
        ocr: OcrReadiness::from_languages(
            vec!["en-US".into(), "de-DE".into()],
            Some("en-US".into()),
        ),
        helpers: HelperReadiness {
            ocr: Readiness::Ready,
            activation: Readiness::Ready,
            lamp: Readiness::Unsupported {
                reason: ReadinessReason::NotConfigured,
            },
            ui_automation: Readiness::Unsupported {
                reason: ReadinessReason::NotConfigured,
            },
        },
    }
}

/// Checks `status` against the full contract and the published `outputSchema`.
fn assert_valid_status(status: &Value) {
    assert!(
        output_schema_tests::full_validator(GET_SERVER_STATUS).is_valid(status),
        "{status}"
    );
    let published = tools()
        .into_iter()
        .find(|tool| tool.name == GET_SERVER_STATUS)
        .unwrap();
    let published =
        jsonschema::validator_for(&Value::Object((*published.output_schema.unwrap()).clone()))
            .unwrap();
    assert!(published.is_valid(status), "{status}");
}

async fn capabilities(harness: &mut Harness) -> Value {
    let status = call(harness, GET_SERVER_STATUS, json!({})).await;
    assert_ne!(status["isError"], true, "{status}");
    let content = status["structuredContent"].clone();
    assert_valid_status(&content);
    content["capabilities"].clone()
}

#[tokio::test]
async fn an_mcp_client_learns_ocr_readiness_and_languages_without_input() {
    let desktop = FakeDesktop::new();
    desktop.open("Editor", 10);
    desktop.set_readiness(ready_readiness());
    let mut harness = start(&desktop).await;

    let reported = capabilities(&mut harness).await;
    assert_eq!(
        reported["ocr"],
        json!({"state": "ready", "languages": ["en-US", "de-DE"], "default_language": "en-US"})
    );
    assert_eq!(reported["helpers"]["ocr"], json!({"state": "ready"}));
    assert_eq!(reported["helpers"]["activation"], json!({"state": "ready"}));
    // The harness runs sessions, so the server reports the lamp, not the backend.
    assert_eq!(reported["helpers"]["lamp"], json!({"state": "ready"}));
    // The helper list is pinned: every helper the server can start is reported.
    let helpers: Vec<&String> = reported["helpers"].as_object().unwrap().keys().collect();
    assert_eq!(helpers, ["activation", "lamp", "ocr", "ui_automation"]);
    assert_eq!(
        reported["helpers"]["ui_automation"],
        json!({"state": "unsupported", "reason": "not_configured"}),
        "a backend without the UI Automation helper says so"
    );
    assert_eq!(reported["limits"]["max_wait_ms"], 30_000);
    assert_eq!(reported["limits"]["max_window_wait_candidates"], 20);
    assert_eq!(reported["hidden_tools"], json!([]));

    // A second call reuses the probe instead of starting another one.
    capabilities(&mut harness).await;
    assert_eq!(desktop.probes(), 1);
    assert!(desktop.received().is_empty(), "the report sent no input");
    let session =
        call(&mut harness, GET_SERVER_STATUS, json!({})).await["structuredContent"]["session"]
            .clone();
    assert_eq!(
        session["state"], "dormant",
        "the report claims no session: {session}"
    );
    harness.finish().await;
}

#[tokio::test]
async fn degraded_unavailable_and_unsupported_are_distinct_reported_states() {
    let degraded = RuntimeReadiness {
        ocr: OcrReadiness::from_languages(vec!["de-DE".into()], None),
        ..ready_readiness()
    };
    let unavailable = RuntimeReadiness {
        ocr: OcrReadiness::without_languages(Readiness::Unavailable {
            reason: ReadinessReason::ProbeTimedOut,
        }),
        helpers: HelperReadiness {
            ocr: Readiness::Unavailable {
                reason: ReadinessReason::ProbeTimedOut,
            },
            ..ready_readiness().helpers
        },
    };
    let cases = [
        (
            Some(degraded),
            json!({"state": "degraded", "reason": "no_default_ocr_language", "languages": ["de-DE"], "default_language": null}),
        ),
        (
            Some(unavailable),
            json!({"state": "unavailable", "reason": "probe_timed_out", "languages": [], "default_language": null}),
        ),
        // A backend without a probe.
        (
            None,
            json!({"state": "unsupported", "reason": "not_implemented", "languages": [], "default_language": null}),
        ),
    ];
    for (readiness, expected) in cases {
        let desktop = FakeDesktop::new();
        if let Some(readiness) = readiness {
            desktop.set_readiness(readiness);
        }
        let mut harness = start(&desktop).await;
        let reported = capabilities(&mut harness).await;
        assert_eq!(reported["ocr"], expected);
        harness.finish().await;
    }
}

#[test]
fn the_published_reason_codes_are_exactly_the_readiness_reasons() {
    let every = [
        ReadinessReason::NotImplemented,
        ReadinessReason::NotConfigured,
        ReadinessReason::NotVerified,
        ReadinessReason::NoOcrLanguage,
        ReadinessReason::NoDefaultOcrLanguage,
        ReadinessReason::OcrRuntimeFailed,
        ReadinessReason::HelperFailed,
        ReadinessReason::ProbeTimedOut,
        ReadinessReason::IndicatorFailed,
    ];
    assert_eq!(json!(every), json!(schema::READINESS_REASONS));
}

/// #9: the families the server never offers are pinned, apart from unsupported features.
#[tokio::test]
async fn the_capability_report_pins_the_forbidden_families() {
    let desktop = FakeDesktop::new();
    let mut harness = start(&desktop).await;
    let reported = capabilities(&mut harness).await;
    assert_eq!(
        reported["forbidden_families"],
        json!([
            "application_launch",
            "shell",
            "filesystem",
            "registry",
            "network",
            "clipboard",
            "browser_dom"
        ])
    );
    harness.finish().await;
}

/// #9: no tool name offers to launch, run, read or write anything outside the desktop.
#[test]
fn no_tool_launches_applications_runs_commands_or_reaches_files_or_the_clipboard() {
    let forbidden = [
        "launch",
        "start_app",
        "open",
        "spawn",
        "exec",
        "shell",
        "command",
        "script",
        "process",
        "file",
        "folder",
        "path",
        "clipboard",
        "copy",
        "paste",
        "registry",
        "network",
        "http",
        "url",
        "browser",
        "dom",
        "elevat",
    ];
    let names: Vec<String> = full_tool_definitions()
        .into_iter()
        .map(|tool| tool.name.to_string())
        .collect();
    assert_eq!(names.len(), 28);
    for name in &names {
        for fragment in forbidden {
            assert!(!name.contains(fragment), "{name} contains {fragment}");
        }
    }
}

/// #23: the report holds no path, user name or raw environment, even when Windows returns
/// something other than a language tag.
#[tokio::test]
async fn the_capability_report_carries_no_paths_user_names_or_environment() {
    let environment: Vec<String> = std::env::vars()
        .map(|(_, value)| value)
        .filter(|value| value.len() >= 4)
        .collect();
    let user = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "synthetic-user".to_owned());
    let mut readiness = ready_readiness();
    readiness.ocr = OcrReadiness::from_languages(
        vec![
            "en-US".into(),
            format!(r"C:\Users\{user}"),
            std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_owned()),
        ],
        Some(format!("/home/{user}")),
    );
    let desktop = FakeDesktop::new();
    desktop.set_readiness(readiness);
    let mut harness = start(&desktop).await;
    let reported = capabilities(&mut harness).await;
    assert_eq!(reported["ocr"]["languages"], json!(["en-US"]));
    assert_eq!(reported["ocr"]["default_language"], Value::Null);

    let mut strings = Vec::new();
    collect_strings(&reported, &mut strings);
    for text in strings {
        assert!(
            !text.contains('\\') && !text.contains('/') && !text.contains(':'),
            "looks like a path: {text}"
        );
        assert!(!text.contains(&user), "names the user: {text}");
        for value in &environment {
            assert_ne!(&text, value, "repeats an environment variable");
        }
    }
    harness.finish().await;
}

fn collect_strings(value: &Value, into: &mut Vec<String>) {
    match value {
        Value::String(text) => into.push(text.clone()),
        Value::Array(items) => items.iter().for_each(|item| collect_strings(item, into)),
        Value::Object(map) => map.iter().for_each(|(key, item)| {
            into.push(key.clone());
            collect_strings(item, into);
        }),
        _ => {}
    }
}

/// #23: the whole status result, with its capability report, stays small.
#[tokio::test]
async fn the_status_result_with_capabilities_stays_bounded() {
    let mut readiness = ready_readiness();
    readiness.ocr = OcrReadiness::from_languages(
        (0..200).map(|index| format!("x-lang-{index:04}")).collect(),
        Some("x-lang-0000".into()),
    );
    let desktop = FakeDesktop::new();
    desktop.set_readiness(readiness);
    let mut harness = start(&desktop).await;
    let status = call(&mut harness, GET_SERVER_STATUS, json!({})).await;
    let content = &status["structuredContent"];
    assert_valid_status(content);
    assert_eq!(
        content["capabilities"]["ocr"]["languages"]
            .as_array()
            .unwrap()
            .len(),
        controlfreak_core::MAX_REPORTED_OCR_LANGUAGES
    );
    let bytes = content["capabilities"].to_string().len();
    // Measured at about 2,300 bytes with 64 languages.
    assert!(bytes < 4_096, "capabilities are {bytes} bytes");
    harness.finish().await;
}

/// #9: two matching windows are both returned and neither is selected; one match is unchanged.
#[tokio::test]
async fn wait_for_window_with_two_matches_returns_both_and_selects_none() {
    let desktop = FakeDesktop::new();
    let editor = desktop.open("Report - Editor", 10);
    let viewer = desktop.open("Report - Viewer", 11);
    let mut harness = start(&desktop).await;
    let validator = output_schema_tests::full_validator(WAIT_FOR_WINDOW);

    let ambiguous = call(
        &mut harness,
        WAIT_FOR_WINDOW,
        json!({"title_contains": "report", "timeout_ms": 0}),
    )
    .await;
    let content = &ambiguous["structuredContent"];
    assert_ne!(ambiguous["isError"], true, "{ambiguous}");
    assert!(validator.is_valid(content), "{content}");
    assert_eq!(content["matched"], false);
    assert_eq!(content["timed_out"], false);
    assert_eq!(content["window"], Value::Null);
    assert_eq!(content["match_count"], 2);
    let mut ids: Vec<&str> = content["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|window| window["id"].as_str().unwrap())
        .collect();
    ids.sort_unstable();
    let mut expected = vec![desktop.id(editor), desktop.id(viewer)];
    expected.sort_unstable();
    assert_eq!(ids, expected);

    let single = call(
        &mut harness,
        WAIT_FOR_WINDOW,
        json!({"title_contains": "viewer", "timeout_ms": 0}),
    )
    .await;
    let content = &single["structuredContent"];
    assert!(validator.is_valid(content), "{content}");
    assert_eq!(content["matched"], true);
    assert_eq!(content["timed_out"], false);
    assert_eq!(content["window"]["id"], desktop.id(viewer));
    assert_eq!(content["match_count"], 1);
    assert_eq!(content["candidates"], json!([]));

    let none = call(
        &mut harness,
        WAIT_FOR_WINDOW,
        json!({"title_contains": "absent", "timeout_ms": 0}),
    )
    .await;
    let content = &none["structuredContent"];
    assert_eq!(content["matched"], false);
    assert_eq!(content["timed_out"], true);
    assert_eq!(content["match_count"], 0);
    assert!(desktop.received().is_empty());
    harness.finish().await;
}
