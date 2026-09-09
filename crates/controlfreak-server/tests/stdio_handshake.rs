use std::{
    io::Write,
    process::{Command, Stdio},
};

#[cfg(target_os = "windows")]
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::Value;
#[cfg(target_os = "windows")]
use std::{
    io::{BufRead, BufReader},
    process::Child,
};

#[test]
fn stdio_handshake_reports_complete_windows_tool_surface() {
    let mut child = test_server_command()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn controlfreak server");

    let requests = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"integration-test","version":"0.1.0"}}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_displays","arguments":{}}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"get_server_status","arguments":{}}}"#,
        "\n",
    );

    child
        .stdin
        .take()
        .expect("open server stdin")
        .write_all(requests.as_bytes())
        .expect("write MCP requests");

    let output = child.wait_with_output().expect("wait for server");
    assert!(
        output.status.success(),
        "server failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let responses: Vec<Value> = String::from_utf8(output.stdout)
        .expect("MCP output is UTF-8")
        .lines()
        .map(|line| serde_json::from_str(line).expect("valid JSON-RPC response"))
        .collect();

    let initialize = responses
        .iter()
        .find(|response| response["id"] == 1)
        .expect("initialize response");
    assert_eq!(initialize["result"]["serverInfo"]["name"], "controlfreak");

    let tools = responses
        .iter()
        .find(|response| response["id"] == 2)
        .expect("tools/list response");
    let tool_names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name"))
        .collect();
    assert_eq!(
        tool_names,
        [
            "get_server_status",
            "begin_control_session",
            "end_control_session",
            "list_displays",
            "capture_display",
            "capture_region",
            "wait_for_visual_change",
            "capture_visual_baseline",
            "wait_for_change_since",
            "read_text_in_region",
            "find_text_on_screen",
            "click_text",
            "list_virtual_desktops",
            "switch_virtual_desktop",
            "list_windows",
            "focus_window",
            "capture_window",
            "wait_for_window",
            "move_mouse",
            "click_mouse",
            "drag_mouse",
            "scroll_mouse",
            "press_keys",
            "type_text"
        ]
    );

    let display_result = responses
        .iter()
        .find(|response| response["id"] == 3)
        .expect("list_displays response");
    if cfg!(target_os = "windows") {
        assert_eq!(display_result["result"]["isError"], false);
        assert!(
            display_result["result"]["structuredContent"]["count"]
                .as_u64()
                .is_some_and(|count| count >= 1)
        );
    } else {
        assert_eq!(display_result["result"]["isError"], true);
    }
    assert!(display_result["result"]["_meta"]["controlfreak"]["operation_id"].is_u64());

    let status = responses
        .iter()
        .find(|response| response["id"] == 4)
        .expect("server status response");
    assert_server_status(status);
}

fn assert_server_status(status: &Value) {
    let content = &status["result"]["structuredContent"];
    assert_eq!(content["status"], "ready");
    assert!(content["instance_id"].is_string());
    assert!(content["recent_operations"].is_array());
    assert!(content["server_elevated"].is_boolean());
    assert!(content["windows_integrity_level"].is_string());
    assert!(content["elevated_operation_allowed"].is_boolean());
}

#[test]
fn capability_report_matches_the_target_platform() {
    let output = Command::new(env!("CARGO_BIN_EXE_controlfreak"))
        .arg("--print-capabilities")
        .output()
        .expect("run capability report");

    assert!(output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).expect("valid capability JSON");
    assert_eq!(report["schema_version"], 2);
    assert!(report["security_context"]["elevated"].is_boolean());
    assert!(report["security_context"]["windows_integrity_level"].is_string());
    let supported_count = report["capabilities"]
        .as_array()
        .expect("capability array")
        .iter()
        .filter(|capability| capability["state"] == "supported")
        .count();
    assert_eq!(
        supported_count,
        usize::from(cfg!(target_os = "windows")) * 4
    );
}

#[cfg(target_os = "windows")]
#[test]
fn stdio_capture_display_returns_png_image_content() {
    let mut child = spawn_server();
    let stdout = child.stdout.take().expect("open server stdout");
    let mut reader = BufReader::new(stdout);

    send_request(
        &mut child,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "capture-test", "version": "0.1.0" }
            }
        }),
    );
    let _initialize = read_response(&mut reader);
    send_request(
        &mut child,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }),
    );
    let display_id = assert_indicator_lifecycle_and_list_displays(&mut child, &mut reader);

    send_request(
        &mut child,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {
                "name": "capture_display",
                "arguments": { "display_id": display_id, "max_width": 321, "include_cursor": false }
            }
        }),
    );
    let capture = read_response(&mut reader);
    assert_eq!(capture["result"]["isError"], false);
    let encoded = capture["result"]["content"]
        .as_array()
        .expect("content array")
        .iter()
        .find(|content| content["type"] == "image")
        .and_then(|content| content["data"].as_str())
        .expect("PNG image content");
    let png = STANDARD.decode(encoded).expect("valid base64 image");
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    let metadata = &capture["result"]["structuredContent"];
    let source_width = metadata["source_bounds"]["width"].as_u64().unwrap();
    let expected_width = source_width.min(321);
    assert_eq!(metadata["image_width"], expected_width);
    assert!(matches!(
        metadata["resize_method"].as_str(),
        Some("native" | "bilinear")
    ));

    assert_region_tools(&mut child, &mut reader, &display_id);
    assert_argument_errors_are_structured(&mut child, &mut reader, &display_id);
    assert_ocr_helper_errors_are_structured(&mut child, &mut reader, &display_id);

    drop(child.stdin.take());
    assert!(child.wait().expect("wait for server").success());
}

#[cfg(target_os = "windows")]
fn assert_ocr_helper_errors_are_structured(
    child: &mut Child,
    reader: &mut impl BufRead,
    display_id: &str,
) {
    send_request(
        child,
        &serde_json::json!({
            "jsonrpc": "2.0", "id": 12, "method": "tools/call",
            "params": { "name": "read_text_in_region", "arguments": {
                "display_id": "controlfreak-missing-display", "x": 0, "y": 0,
                "width": 1, "height": 1
            }}
        }),
    );
    let invalid_display = read_response(reader);
    assert_eq!(invalid_display["result"]["isError"], true);
    assert_eq!(
        invalid_display["result"]["structuredContent"]["error"]["code"],
        "invalid_argument"
    );
    assert!(
        invalid_display["result"]["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("display_id"))
    );

    send_request(
        child,
        &serde_json::json!({
            "jsonrpc": "2.0", "id": 13, "method": "tools/call",
            "params": { "name": "read_text_in_region", "arguments": {
                "display_id": display_id, "x": 0, "y": 0, "width": 0, "height": 1
            }}
        }),
    );
    let invalid_region = read_response(reader);
    assert_eq!(invalid_region["result"]["isError"], true);
    assert_eq!(
        invalid_region["result"]["structuredContent"]["error"]["code"],
        "invalid_argument"
    );
    assert!(
        invalid_region["result"]["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("width/height"))
    );

    send_request(
        child,
        &serde_json::json!({
            "jsonrpc": "2.0", "id": 14, "method": "tools/call",
            "params": { "name": "read_text_in_region", "arguments": {
                "display_id": display_id, "x": 0, "y": 0, "width": 1, "height": 1,
                "language": "x-controlfreak-unsupported"
            }}
        }),
    );
    let unavailable = read_response(reader);
    assert_eq!(unavailable["result"]["isError"], true);
    assert_eq!(
        unavailable["result"]["structuredContent"]["error"]["code"],
        "platform_operation_failed"
    );
    assert!(
        unavailable["result"]["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("platform backend is unavailable"))
    );

    send_request(
        child,
        &serde_json::json!({
            "jsonrpc": "2.0", "id": 15, "method": "tools/call",
            "params": { "name": "get_server_status", "arguments": {} }
        }),
    );
    let status = read_response(reader);
    assert_eq!(status["result"]["isError"], false);
    assert_eq!(status["result"]["structuredContent"]["status"], "ready");
}

#[cfg(target_os = "windows")]
fn assert_indicator_lifecycle_and_list_displays(
    child: &mut Child,
    reader: &mut impl BufRead,
) -> String {
    send_request(
        child,
        &serde_json::json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": { "name": "get_server_status", "arguments": {} }
        }),
    );
    let dormant_status = read_response(reader);
    assert_server_status(&dormant_status);
    assert_eq!(
        dormant_status["result"]["structuredContent"]["safety_indicator"]["status"],
        "dormant"
    );

    send_request(
        child,
        &serde_json::json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": { "name": "list_displays", "arguments": {} }
        }),
    );
    let displays = read_response(reader);
    let display_id = displays["result"]["structuredContent"]["displays"]
        .as_array()
        .and_then(|items| items.first())
        .and_then(|display| display["id"].as_str())
        .expect("first active display ID")
        .to_owned();

    send_request(
        child,
        &serde_json::json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": { "name": "get_server_status", "arguments": {} }
        }),
    );
    let active_status = read_response(reader);
    assert_server_status(&active_status);
    assert_eq!(
        active_status["result"]["structuredContent"]["safety_indicator"]["status"],
        "dormant"
    );

    display_id
}

#[cfg(target_os = "windows")]
fn assert_argument_errors_are_structured(
    child: &mut Child,
    reader: &mut impl BufRead,
    display_id: &str,
) {
    send_request(
        child,
        &serde_json::json!({
            "jsonrpc": "2.0", "id": 11, "method": "tools/call",
            "params": { "name": "capture_display", "arguments": {
                "display_id": display_id, "unknown_option": true
            }}
        }),
    );
    let rejected = read_response(reader);
    assert_eq!(rejected["result"]["isError"], true);
    assert_eq!(
        rejected["result"]["structuredContent"]["error"]["code"],
        "invalid_arguments"
    );
    assert!(rejected["result"]["content"][0]["text"].is_string());
}

#[cfg(target_os = "windows")]
fn assert_region_tools(child: &mut Child, reader: &mut impl BufRead, display_id: &str) {
    send_request(
        child,
        &serde_json::json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": { "name": "capture_region", "arguments": {
                "display_id": display_id, "x": 0, "y": 0, "width": 1, "height": 1,
                "include_cursor": false
            }}
        }),
    );
    let region = read_response(reader);
    assert_eq!(region["result"]["isError"], false);
    assert_eq!(
        region["result"]["structuredContent"]["source_bounds"]["width"],
        1
    );

    send_request(
        child,
        &serde_json::json!({
            "jsonrpc": "2.0", "id": 5, "method": "tools/call",
            "params": { "name": "wait_for_visual_change", "arguments": {
                "display_id": display_id, "x": 0, "y": 0, "width": 1, "height": 1,
                "timeout_ms": 1, "stable_ms": 0, "difference_threshold": 1.0
            }}
        }),
    );
    let visual_wait = read_response(reader);
    assert_eq!(visual_wait["result"]["isError"], false);
    assert_eq!(
        visual_wait["result"]["structuredContent"]["timed_out"],
        true
    );
    assert_eq!(
        visual_wait["result"]["structuredContent"]["screenshot"]["source_bounds"]["width"],
        1
    );

    send_request(
        child,
        &serde_json::json!({
            "jsonrpc": "2.0", "id": 6, "method": "tools/call",
            "params": { "name": "capture_visual_baseline", "arguments": {
                "display_id": display_id, "x": 0, "y": 0, "width": 1, "height": 1
            }}
        }),
    );
    let baseline = read_response(reader);
    assert_eq!(baseline["result"]["isError"], false);
    let baseline_id = baseline["result"]["structuredContent"]["baseline_id"]
        .as_str()
        .expect("baseline ID");

    send_request(
        child,
        &serde_json::json!({
            "jsonrpc": "2.0", "id": 7, "method": "tools/call",
            "params": { "name": "wait_for_change_since", "arguments": {
                "baseline_id": baseline_id, "timeout_ms": 1, "stable_ms": 0,
                "difference_threshold": 1.0
            }}
        }),
    );
    let baseline_wait = read_response(reader);
    assert_eq!(baseline_wait["result"]["isError"], false);
    assert_eq!(
        baseline_wait["result"]["structuredContent"]["timed_out"],
        true
    );

    send_request(
        child,
        &serde_json::json!({
            "jsonrpc": "2.0", "id": 8, "method": "tools/call",
            "params": { "name": "read_text_in_region", "arguments": {
                "display_id": display_id, "x": 0, "y": 0, "width": 1, "height": 1
            }}
        }),
    );
    let ocr = read_response(reader);
    assert_eq!(ocr["result"]["isError"], false, "OCR failed: {ocr}");
    assert!(ocr["result"]["structuredContent"]["text"].is_string());

    send_request(
        child,
        &serde_json::json!({
            "jsonrpc": "2.0", "id": 9, "method": "tools/call",
            "params": { "name": "find_text_on_screen", "arguments": {
                "display_id": display_id, "x": 0, "y": 0, "width": 1, "height": 1,
                "query": "controlfreak-ocr-no-match"
            }}
        }),
    );
    let text_match = read_response(reader);
    assert_eq!(text_match["result"]["isError"], false);
    assert_eq!(text_match["result"]["structuredContent"]["count"], 0);

    assert_click_text_rejects_no_match(child, reader, display_id);
}

#[cfg(target_os = "windows")]
fn assert_click_text_rejects_no_match(
    child: &mut Child,
    reader: &mut impl BufRead,
    display_id: &str,
) {
    send_request(
        child,
        &serde_json::json!({
            "jsonrpc": "2.0", "id": 10, "method": "tools/call",
            "params": { "name": "click_text", "arguments": {
                "display_id": display_id, "x": 0, "y": 0, "width": 1, "height": 1,
                "query": "controlfreak-ocr-no-match", "observation": { "mode": "none" }
            }}
        }),
    );
    let rejected_click = read_response(reader);
    assert_eq!(rejected_click["result"]["isError"], true);
    assert_eq!(
        rejected_click["result"]["structuredContent"]["error"]["code"],
        "platform_operation_failed"
    );
}

#[cfg(target_os = "windows")]
fn spawn_server() -> Child {
    test_server_command()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn controlfreak server")
}

fn test_server_command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_controlfreak"));
    command.arg("--allow-elevated");
    command.env("CONTROLFREAK_DISABLE_DESKTOP_GLOW_FOR_TESTS", "1");
    command
}

#[cfg(target_os = "windows")]
fn send_request(child: &mut Child, request: &Value) {
    let stdin = child.stdin.as_mut().expect("open server stdin");
    writeln!(stdin, "{request}").expect("write MCP request");
    stdin.flush().expect("flush MCP request");
}

#[cfg(target_os = "windows")]
fn read_response(reader: &mut impl BufRead) -> Value {
    let mut line = String::new();
    reader.read_line(&mut line).expect("read MCP response");
    serde_json::from_str(line.trim_end()).expect("valid JSON-RPC response")
}
