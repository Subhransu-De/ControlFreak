use controlfreak_e2e::{
    Capability, Failure, Suite,
    mcp::McpClient,
    report,
    runner::{self, Environment, Outcome, Plan},
    workflow::{FailureKind, task},
};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    thread,
    time::Duration,
};

fn fake_server(reader: std::io::PipeReader, mut writer: std::io::PipeWriter) {
    for line in BufReader::new(reader).lines().map_while(Result::ok) {
        let message: Value = serde_json::from_str(&line).expect("client sent JSON");
        let Some(id) = message.get("id").cloned() else {
            continue;
        };
        let result = match message["method"].as_str() {
            Some("initialize") => json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "controlfreak", "version": "test"}
            }),
            Some("tools/list") => tools(&message["params"]),
            Some("tools/call") => {
                let name = message["params"]["name"].as_str().unwrap_or_default();
                if name == "hang" {
                    continue;
                }
                let note =
                    json!({"jsonrpc": "2.0", "method": "notifications/progress", "params": {}});
                writeln!(writer, "{note}").expect("write notification");
                call(name)
            }
            _ => json!({}),
        };
        let reply = json!({"jsonrpc": "2.0", "id": id, "result": result});
        if writeln!(writer, "{reply}")
            .and_then(|()| writer.flush())
            .is_err()
        {
            return;
        }
    }
}

fn tools(params: &Value) -> Value {
    let status_schema = json!({
        "type": "object",
        "properties": {"stop_state": {"enum": ["ready", "stopped"]}},
        "required": ["stop_state"]
    });
    if params.get("cursor").is_none() {
        return json!({
            "tools": [
                {"name": "get_server_status", "inputSchema": {"type": "object"}, "outputSchema": status_schema},
                {"name": "find_text_on_screen", "inputSchema": {"type": "object"}, "outputSchema": {"type": "object"}}
            ],
            "nextCursor": "page-2"
        });
    }
    json!({"tools": [
        {"name": "broken", "inputSchema": {"type": "object"}, "outputSchema": status_schema},
        {"name": "bad_error", "inputSchema": {"type": "object"}},
        {"name": "hang", "inputSchema": {"type": "object"}}
    ]})
}

fn call(name: &str) -> Value {
    match name {
        "get_server_status" => json!({
            "content": [], "isError": false, "structuredContent": {"stop_state": "ready"}
        }),
        "find_text_on_screen" => json!({
            "content": [], "isError": true,
            "structuredContent": {"error": {
                "code": "platform_operation_failed", "reason": "ocr_unavailable",
                "message": "no OCR language", "retryable": false
            }}
        }),
        "broken" => json!({
            "content": [], "isError": false, "structuredContent": {"stop_state": "sideways"}
        }),
        "bad_error" => json!({
            "content": [], "isError": true, "structuredContent": {"oops": true}
        }),
        _ => json!({"content": [], "isError": false, "structuredContent": {}}),
    }
}

fn connect() -> Result<McpClient, String> {
    let (client_reads, server_writes) = std::io::pipe().map_err(|e| e.to_string())?;
    let (server_reads, client_writes) = std::io::pipe().map_err(|e| e.to_string())?;
    thread::spawn(move || fake_server(server_reads, server_writes));
    McpClient::connect(
        client_reads,
        client_writes,
        None,
        Duration::from_millis(500),
    )
    .map_err(|error| error.to_string())
}

fn environment() -> Environment {
    Environment {
        launch_server: Box::new(|_log| connect()),
        open_window: Box::new(|spec, _, _| Err(format!("no desktop in tests: {}", spec.key))),
        helper: Box::new(|args| Err(format!("no fixture in tests: {args:?}"))),
    }
}

fn suites() -> Vec<Suite> {
    vec![Suite {
        name: "fake",
        about: "Runner behavior against a fake server",
        tasks: vec![
            task("status-ready", |cx| {
                let status = cx.ok("get_server_status", json!({}))?;
                if status["stop_state"] == "ready" {
                    Ok(())
                } else {
                    Err(Failure::assertion(format!("{status}")))
                }
            }),
            task("contract-broken", |cx| {
                cx.call("broken", json!({})).map(drop)
            }),
            task("error-envelope-broken", |cx| {
                cx.call("bad_error", json!({})).map(drop)
            }),
            task("needs-ocr", |_| Ok(())).requires(&[Capability::Ocr]),
            task("panics", |_| panic!("boom")),
            task("hangs", |cx| cx.call("hang", json!({})).map(drop)),
            task("needs-window", |cx| cx.target("app").map(drop)).scene("single"),
        ],
    }]
}

fn plan(strict: bool, out: &std::path::Path) -> Plan {
    Plan {
        strict,
        suites: Vec::new(),
        out_dir: out.to_path_buf(),
    }
}

fn outcome<'r>(report: &'r runner::RunReport, task: &str) -> &'r Outcome {
    &report
        .records
        .iter()
        .find(|record| record.task == task)
        .unwrap_or_else(|| panic!("{task} did not run"))
        .outcome
}

fn failure_kind(outcome: &Outcome) -> Option<FailureKind> {
    match outcome {
        Outcome::Failed(failure) => Some(failure.kind),
        _ => None,
    }
}

#[test]
fn every_outcome_is_classified_and_reported() {
    let out = tempfile::tempdir().expect("temp dir");
    let mut seen = Vec::new();
    let report = runner::run(
        &suites(),
        &plan(false, out.path()),
        &mut environment(),
        &mut |record| seen.push(record.task),
    )
    .expect("run");

    assert_eq!(seen.len(), 7, "every task runs: {seen:?}");
    assert_eq!(outcome(&report, "status-ready"), &Outcome::Passed);
    assert_eq!(
        failure_kind(outcome(&report, "contract-broken")),
        Some(FailureKind::Contract)
    );
    assert_eq!(
        failure_kind(outcome(&report, "error-envelope-broken")),
        Some(FailureKind::Contract)
    );
    assert_eq!(
        outcome(&report, "needs-ocr"),
        &Outcome::Blocked(vec![Capability::Ocr])
    );
    let Outcome::Failed(panicked) = outcome(&report, "panics") else {
        panic!("a panic must fail the task");
    };
    assert!(panicked.message.contains("boom"), "{panicked}");
    assert_eq!(
        failure_kind(outcome(&report, "hangs")),
        Some(FailureKind::Timeout)
    );
    assert_eq!(
        failure_kind(outcome(&report, "needs-window")),
        Some(FailureKind::Host)
    );

    let status = report
        .records
        .iter()
        .find(|r| r.task == "status-ready")
        .unwrap();
    assert_eq!(status.steps.len(), 1);

    report::write(&report, out.path()).expect("write reports");
    let junit = std::fs::read_to_string(out.path().join("junit.xml")).expect("junit");
    assert!(junit.contains("tests=\"7\""), "{junit}");
    assert!(
        junit.contains("<skipped message=\"BLOCKED: missing ocr\"/>"),
        "{junit}"
    );
    let timeline = std::fs::read_to_string(out.path().join("timeline.jsonl")).expect("timeline");
    assert!(
        timeline
            .lines()
            .all(|line| serde_json::from_str::<Value>(line).is_ok())
    );
    assert!(
        timeline.contains("\"tool\":\"get_server_status\""),
        "{timeline}"
    );
    assert!(!report.succeeded());
}

#[test]
fn strict_turns_blocked_into_failure() {
    let out = tempfile::tempdir().expect("temp dir");
    let only_blocked = vec![Suite {
        name: "fake",
        about: "",
        tasks: vec![task("needs-ocr", |_| Ok(())).requires(&[Capability::Ocr])],
    }];
    let mut ignore = |_: &runner::TaskRecord| {};
    let relaxed = runner::run(
        &only_blocked,
        &plan(false, out.path()),
        &mut environment(),
        &mut ignore,
    )
    .expect("run");
    assert_eq!(relaxed.records.len(), 1);
    assert!(relaxed.succeeded(), "BLOCKED alone passes without --strict");
    let strict = runner::run(
        &only_blocked,
        &plan(true, out.path()),
        &mut environment(),
        &mut ignore,
    )
    .expect("run");
    assert!(!strict.succeeded(), "BLOCKED fails under --strict");
}

#[test]
fn an_unknown_suite_is_refused_before_anything_runs() {
    let out = tempfile::tempdir().expect("temp dir");
    let mut plan = plan(false, out.path());
    plan.suites = vec!["nope".into()];
    let error = runner::run(&suites(), &plan, &mut environment(), &mut |_| {
        panic!("nothing may run")
    })
    .unwrap_err();
    assert!(
        error.contains("no suite named \"nope\"") && error.contains("fake"),
        "{error}"
    );
}

#[test]
fn the_built_in_suites_list_without_a_desktop() {
    let all = controlfreak_e2e::suites::all();
    let out = tempfile::tempdir().expect("temp dir");
    let selected = runner::select(&all, &plan(false, out.path())).expect("select");
    let count: usize = selected.iter().map(|(_, tasks)| tasks.len()).sum();
    assert_eq!(count, 21);
}
