#![cfg(windows)]
// Tool arguments are built inline with `json!`; taking them by value keeps the tests readable.
#![allow(clippy::needless_pass_by_value)]

//! Live end-to-end tests: the real `controlfreak` binary over STDIO against disposable fixture
//! windows (`controlfreak-fixture`), proving real effects from the fixture's own event log.
//!
//! These tests inject real input, activate windows, change the foreground and capture the
//! screen. They must only run on a disposable, unlocked Windows desktop such as the CI
//! `live-desktop` job. Every test is `#[ignore]` and also refuses to run unless
//! `CONTROLFREAK_LIVE_DESKTOP=1` is set, so `cargo test --workspace` never touches a desktop and
//! `--ignored` alone fails fast with an explanation instead of driving a developer's machine.
//!
//! ```text
//! cargo build -p controlfreak-server --locked
//! $env:CONTROLFREAK_LIVE_DESKTOP = "1"
//! cargo test -p controlfreak-live-tests --test live --locked -- --ignored --test-threads=1
//! ```
//!
//! One test at a time: they share the desktop, the foreground and the single-owner arbitration.
//! The taskbar test and the OCR test skip with a printed reason when the environment cannot
//! provide what they need (a foregroundable taskbar, a Windows OCR language pack).

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader, Cursor, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const GATE: &str = "CONTROLFREAK_LIVE_DESKTOP";
const FIXTURE: &str = env!("CARGO_BIN_EXE_controlfreak-fixture");
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(90);
const EVENT_TIMEOUT: Duration = Duration::from_secs(10);
/// Where the first fixture sits: below the session tab, clear of the second fixture.
const LEFT: (i32, i32) = (10, 140);
const RIGHT: (i32, i32) = (410, 140);
const GREEN: (u8, u8, u8) = (0x2E, 0x8B, 0x57);
const MAGENTA: (u8, u8, u8) = (0xFF, 0x00, 0xFF);

fn require_live_desktop() {
    assert!(
        std::env::var(GATE).as_deref() == Ok("1"),
        "refusing to run: these tests inject real input and change the foreground window. \
         Run them only on a disposable desktop (the CI live-desktop job) with {GATE}=1."
    );
}

fn server_path() -> PathBuf {
    let path = Path::new(FIXTURE).with_file_name("controlfreak.exe");
    assert!(
        path.exists(),
        "{} not found: run `cargo build -p controlfreak-server --locked` first",
        path.display()
    );
    path
}

fn unique() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{}-{nanos}", std::process::id())
}

/// The real server on a pipe. A reader thread lets every wait have a deadline.
struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<Value>,
    next_id: u64,
}

impl Server {
    fn start() -> Self {
        require_live_desktop();
        let mut child = Command::new(server_path())
            // Hosted runners run as an elevated administrator; the opt-in is documented policy.
            .arg("--allow-elevated")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn controlfreak server");
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("server stdout");
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(value) = serde_json::from_str::<Value>(&line)
                    && sender.send(value).is_err()
                {
                    break;
                }
            }
        });
        let mut server = Self {
            child,
            stdin,
            lines,
            next_id: 1,
        };
        let initialize = server.request(
            "initialize",
            json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "controlfreak-live-tests", "version": "0.1.0" }
            }),
        );
        assert_eq!(
            initialize["result"]["serverInfo"]["name"], "controlfreak",
            "initialize failed: {initialize}"
        );
        server.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        server
    }

    fn send(&mut self, message: &Value) {
        let stdin = self.stdin.as_mut().expect("server stdin is open");
        writeln!(stdin, "{message}").expect("write MCP message");
        stdin.flush().expect("flush MCP message");
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let deadline = Instant::now() + RESPONSE_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let message = self
                .lines
                .recv_timeout(remaining)
                .unwrap_or_else(|_| panic!("no response to {method} within {RESPONSE_TIMEOUT:?}"));
            // Notifications (such as session_stopped) carry no id; skip them.
            if message["id"] == id {
                return message;
            }
        }
    }

    /// Calls a tool and returns its `result`. A protocol-level error fails the test.
    fn call(&mut self, tool: &str, arguments: Value) -> Value {
        let response = self.request("tools/call", json!({"name": tool, "arguments": arguments}));
        assert!(
            response.get("error").is_none(),
            "{tool} was rejected by the protocol: {response}"
        );
        response["result"].clone()
    }

    /// Calls a tool that must succeed and returns its `structuredContent`.
    fn ok(&mut self, tool: &str, arguments: Value) -> Value {
        let result = self.call(tool, arguments);
        assert_eq!(result["isError"], false, "{tool} failed: {result}");
        result["structuredContent"].clone()
    }

    fn window_id(&mut self, fixture: &Fixture) -> String {
        let found = self.ok(
            "wait_for_window",
            json!({"title_contains": fixture.title, "timeout_ms": 10_000}),
        );
        assert_eq!(found["matched"], true, "fixture window not found: {found}");
        found["window"]["id"]
            .as_str()
            .expect("window id")
            .to_owned()
    }

    fn begin(&mut self, target: &str) {
        self.ok(
            "begin_control_session",
            json!({"target_ref": target, "expected_seconds": 60}),
        );
    }

    fn focus(&mut self, target: &str) {
        self.ok(
            "focus_window",
            json!({"window_id": target, "observation": {"mode": "none"}}),
        );
    }

    fn end(&mut self) {
        self.ok("end_control_session", json!({}));
    }

    fn click(&mut self, target: &str, point: (i32, i32)) -> Value {
        self.call(
            "click_mouse",
            json!({
                "target_ref": target, "x": point.0, "y": point.1,
                "observation": {"mode": "none"}
            }),
        )
    }

    fn press(&mut self, target: &str, keys: &[&str]) {
        self.ok(
            "press_keys",
            json!({"target_ref": target, "keys": keys, "observation": {"mode": "none"}}),
        );
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // EOF is the documented way to end a client; it also closes any open session.
        drop(self.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// One fixture window process. Killed through its owned handle on drop.
struct Fixture {
    child: Child,
    log: PathBuf,
    title: String,
    regions: HashMap<String, [i32; 4]>,
}

impl Fixture {
    fn spawn(label: &str, color: &str, position: (i32, i32), extra: &[&str]) -> Self {
        require_live_desktop();
        let unique = unique();
        let title = format!("CFLive {label} {unique}");
        let directory = std::env::temp_dir().join("controlfreak-live-tests");
        fs::create_dir_all(&directory).expect("create fixture log directory");
        let log = directory.join(format!("{label}-{unique}.log"));
        let child = Command::new(FIXTURE)
            .args([
                "--title",
                &title,
                "--color",
                color,
                "--lifetime-secs",
                "240",
            ])
            .args(["--pos", &format!("{},{}", position.0, position.1)])
            .arg("--log")
            .arg(&log)
            .args(extra)
            .stdout(Stdio::null())
            .spawn()
            .expect("spawn fixture window");
        let mut fixture = Self {
            child,
            log,
            title,
            regions: HashMap::new(),
        };
        let lines = fixture.wait_for("the ready line", |events| {
            events.iter().any(|e| e.starts_with("ready "))
        });
        let ready = lines
            .iter()
            .find(|line| line.starts_with("ready "))
            .expect("ready line");
        for field in ready.split_whitespace().skip(1) {
            let (name, values) = field.split_once('=').expect("region field");
            let numbers: Vec<i32> = values.split(',').filter_map(|v| v.parse().ok()).collect();
            fixture
                .regions
                .insert(name.to_owned(), numbers.try_into().expect("four numbers"));
        }
        fixture
    }

    fn events(&self) -> Vec<String> {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// Waits until the log satisfies `done`, and fails with the whole log when it never does.
    fn wait_for(&self, what: &str, done: impl Fn(&[String]) -> bool) -> Vec<String> {
        let deadline = Instant::now() + EVENT_TIMEOUT;
        loop {
            let events = self.events();
            if done(&events) {
                return events;
            }
            assert!(
                Instant::now() < deadline,
                "fixture {} never logged {what}. Log so far:\n{}",
                self.title,
                events.join("\n")
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn center(&self, region: &str) -> (i32, i32) {
        let [left, top, right, bottom] = self.regions[region];
        (left.midpoint(right), top.midpoint(bottom))
    }

    /// Mouse events of one role: `(x, y)` of every `left_down`.
    fn left_downs(events: &[String], role: &str) -> Vec<(i32, i32)> {
        events
            .iter()
            .filter_map(|line| {
                let parts: Vec<&str> = line.split_whitespace().collect();
                match parts.as_slice() {
                    [r, "mouse", "left_down", x, y] if *r == role => {
                        Some((x.parse().ok()?, y.parse().ok()?))
                    }
                    _ => None,
                }
            })
            .collect()
    }

    /// Typed output as one string: characters as themselves, Enter and Tab key presses as
    /// marks. Control characters that follow a key press are the same event and are skipped.
    fn typed(events: &[String]) -> String {
        events
            .iter()
            .filter_map(|line| {
                let parts: Vec<&str> = line.split_whitespace().collect();
                match parts.as_slice() {
                    ["main", "keydown", "0x0D"] => Some('\u{23CE}'),
                    ["main", "keydown", "0x09"] => Some('\u{21E5}'),
                    ["main", "char", unit] => {
                        let code = u32::from_str_radix(unit.strip_prefix("U+")?, 16).ok()?;
                        char::from_u32(code).filter(|c| !c.is_control())
                    }
                    _ => None,
                }
            })
            .collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_file(&self.log);
    }
}

fn near(actual: (i32, i32), wanted: (i32, i32)) -> bool {
    (actual.0 - wanted.0).abs() <= 1 && (actual.1 - wanted.1).abs() <= 1
}

fn display_local(point: (i32, i32)) -> (u32, u32) {
    (
        u32::try_from(point.0).expect("primary-display x"),
        u32::try_from(point.1).expect("primary-display y"),
    )
}

#[test]
#[ignore = "live desktop: set CONTROLFREAK_LIVE_DESKTOP=1 on a disposable desktop"]
fn focus_window_brings_the_fixture_to_the_foreground() {
    let mut server = Server::start();
    let fixture = Fixture::spawn("focus", "2E8B57", LEFT, &[]);
    let id = server.window_id(&fixture);
    server.begin(&id);
    server.focus(&id);
    fixture.wait_for("an activation", |events| {
        events
            .iter()
            .any(|e| e == "main activate 1" || e == "main activate 2")
    });
    let windows = server.ok("list_windows", json!({}));
    let listed = windows["windows"]
        .as_array()
        .expect("windows")
        .iter()
        .find(|window| window["id"] == id)
        .expect("fixture is listed");
    assert_eq!(listed["is_foreground"], true, "{listed}");
    server.end();
}

#[test]
#[ignore = "live desktop: set CONTROLFREAK_LIVE_DESKTOP=1 on a disposable desktop"]
fn click_mouse_lands_on_the_requested_pixel_and_button() {
    let mut server = Server::start();
    let fixture = Fixture::spawn("click", "2E8B57", LEFT, &[]);
    let id = server.window_id(&fixture);
    server.begin(&id);
    server.focus(&id);

    let edit = fixture.center("edit");
    let result = server.click(&id, edit);
    assert_eq!(result["isError"], false, "{result}");
    assert_eq!(result["structuredContent"]["status"], "completed");
    let events = fixture.wait_for("the click on the edit surface", |events| {
        !Fixture::left_downs(events, "main").is_empty()
    });
    let downs = Fixture::left_downs(&events, "main");
    assert!(
        near(downs[0], edit),
        "clicked {edit:?} but the fixture saw {:?}",
        downs[0]
    );

    let result = server.click(&id, fixture.center("button"));
    assert_eq!(result["isError"], false, "{result}");
    fixture.wait_for("the button press", |events| {
        events.iter().any(|e| e == "main button ping")
    });
    server.end();
}

#[test]
#[ignore = "live desktop: set CONTROLFREAK_LIVE_DESKTOP=1 on a disposable desktop"]
fn type_text_delivers_unicode_enter_and_tab() {
    let mut server = Server::start();
    let fixture = Fixture::spawn("type", "2E8B57", LEFT, &[]);
    let id = server.window_id(&fixture);
    server.begin(&id);
    server.focus(&id);

    server.ok(
        "type_text",
        json!({
            "target_ref": id,
            "text": "Hi, \u{e9}\u{65e5}\r\n\tz",
            "observation": {"mode": "none"}
        }),
    );
    // A CRLF is one Enter, not two.
    let wanted = "Hi, \u{e9}\u{65e5}\u{23CE}\u{21E5}z";
    let events = fixture.wait_for("all typed text", |events| {
        Fixture::typed(events).chars().count() >= wanted.chars().count()
    });
    assert_eq!(
        Fixture::typed(&events),
        wanted,
        "log:\n{}",
        events.join("\n")
    );
    server.end();
}

#[test]
#[ignore = "live desktop: set CONTROLFREAK_LIVE_DESKTOP=1 on a disposable desktop"]
fn press_keys_sends_punctuation_enter_and_tab_keys() {
    let mut server = Server::start();
    let fixture = Fixture::spawn("keys", "2E8B57", LEFT, &[]);
    let id = server.window_id(&fixture);
    server.begin(&id);
    server.focus(&id);

    // Name, virtual-key code and the character a US layout types without Shift.
    let keys = [
        ("comma", "0xBC", ','),
        ("period", "0xBE", '.'),
        ("slash", "0xBF", '/'),
        ("semicolon", "0xBA", ';'),
        ("quote", "0xDE", '\''),
        ("left_bracket", "0xDB", '['),
        ("minus", "0xBD", '-'),
        ("equals", "0xBB", '='),
        ("backtick", "0xC0", '`'),
    ];
    for (name, _, _) in keys {
        server.press(&id, &[name]);
    }
    server.press(&id, &["enter"]);
    server.press(&id, &["tab"]);

    let wanted: String = keys
        .iter()
        .map(|(_, _, c)| *c)
        .chain(['\u{23CE}', '\u{21E5}'])
        .collect();
    let events = fixture.wait_for("every key", |events| {
        Fixture::typed(events).chars().count() >= wanted.chars().count()
    });
    assert_eq!(
        Fixture::typed(&events),
        wanted,
        "log:\n{}",
        events.join("\n")
    );
    for (name, vk, _) in keys {
        assert!(
            events.contains(&format!("main keydown {vk}")),
            "{name} did not arrive as virtual key {vk}. Log:\n{}",
            events.join("\n")
        );
    }
    server.end();
}

fn count_pixels(png_bytes: &[u8]) -> (usize, usize, usize) {
    let decoder = png::Decoder::new(Cursor::new(png_bytes));
    let mut reader = decoder.read_info().expect("PNG header");
    let mut buffer = vec![0; reader.output_buffer_size().expect("PNG size")];
    let info = reader.next_frame(&mut buffer).expect("PNG pixels");
    let step = match info.color_type {
        png::ColorType::Rgba => 4,
        png::ColorType::Rgb => 3,
        other => panic!("unexpected PNG color type {other:?}"),
    };
    let mut distinct = std::collections::HashSet::new();
    let (mut green, mut magenta) = (0, 0);
    for pixel in buffer[..info.buffer_size()].chunks_exact(step) {
        let color = (pixel[0], pixel[1], pixel[2]);
        distinct.insert(color);
        green += usize::from(color == GREEN);
        magenta += usize::from(color == MAGENTA);
    }
    (distinct.len(), green, magenta)
}

#[test]
#[ignore = "live desktop: set CONTROLFREAK_LIVE_DESKTOP=1 on a disposable desktop"]
fn capture_window_sees_an_occluded_fixture_through_print_window() {
    let mut server = Server::start();
    let target = Fixture::spawn("covered", "2E8B57", LEFT, &[]);
    let id = server.window_id(&target);
    // A topmost window of another process sits exactly over the target.
    let cover = Fixture::spawn("cover", "FF00FF", LEFT, &["--topmost"]);
    let cover_id = server.window_id(&cover);
    assert_ne!(id, cover_id);

    // No cursor marker: it is drawn in the same magenta as the covering window, so a cursor
    // left inside the window by an earlier test would look like a leak.
    let result = server.call(
        "capture_window",
        json!({"window_id": id, "native": true, "include_cursor": false}),
    );
    assert_eq!(result["isError"], false, "{result}");
    let metadata = &result["structuredContent"];
    assert_eq!(
        metadata["capture_method"], "windows_print_window",
        "{metadata}"
    );
    assert_eq!(metadata["resize_method"], "native");
    let encoded = result["content"]
        .as_array()
        .expect("content")
        .iter()
        .find(|c| c["type"] == "image")
        .and_then(|c| c["data"].as_str())
        .expect("PNG image content");
    let png = STANDARD.decode(encoded).expect("base64 PNG");
    let (distinct, green, magenta) = count_pixels(&png);
    assert!(distinct >= 3, "image is uniform: {distinct} colors");
    assert!(green > 0, "the target's own pixels are missing");
    assert_eq!(magenta, 0, "the covering window leaked into the capture");
}

#[test]
#[ignore = "live desktop: set CONTROLFREAK_LIVE_DESKTOP=1 on a disposable desktop"]
fn click_text_clicks_the_unique_ocr_match_when_a_language_pack_exists() {
    let mut server = Server::start();
    let fixture = Fixture::spawn("ocr", "2E8B57", LEFT, &[]);
    let id = server.window_id(&fixture);
    let [left, top, right, bottom] = fixture.regions["label"];
    let region = json!({
        "x": left, "y": top, "width": right - left, "height": bottom - top
    });
    // Preflight with an observation tool, which needs no session.
    let mut read = region.clone();
    read["query"] = json!("Banana");
    let probe = server.call("find_text_on_screen", read);
    if probe["isError"] == true {
        eprintln!(
            "SKIPPED click_text: Windows OCR is unavailable here (install an OCR language pack): {}",
            probe["structuredContent"]["error"]
        );
        return;
    }
    server.begin(&id);
    server.focus(&id);
    let mut arguments = region;
    arguments["target_ref"] = json!(id);
    arguments["query"] = json!("Banana");
    arguments["observation"] = json!({"mode": "none"});
    let result = server.call("click_text", arguments);
    assert_eq!(result["isError"], false, "click_text failed: {result}");
    let events = fixture.wait_for("the OCR click", |events| {
        !Fixture::left_downs(events, "main").is_empty()
    });
    let down = Fixture::left_downs(&events, "main")[0];
    assert!(
        down.0 >= left && down.0 < right && down.1 >= top && down.1 < bottom,
        "OCR click {down:?} missed the label {:?}",
        fixture.regions["label"]
    );
    server.end();
}

#[test]
#[ignore = "live desktop: set CONTROLFREAK_LIVE_DESKTOP=1 on a disposable desktop"]
fn an_owned_dialog_of_the_same_process_is_accepted() {
    let mut server = Server::start();
    let fixture = Fixture::spawn("owner", "2E8B57", LEFT, &["--with-dialog"]);
    let id = server.window_id(&fixture);
    let windows = server.ok("list_windows", json!({}));
    let dialog = windows["windows"]
        .as_array()
        .expect("windows")
        .iter()
        .find(|window| window["owner"] == id)
        .unwrap_or_else(|| panic!("no window owned by the fixture in {windows}"));
    assert_eq!(dialog["process_id"], fixture.child.id());

    server.begin(&id);
    server.focus(&id);
    let point = fixture.center("dialog");
    let result = server.click(&id, point);
    assert_eq!(result["isError"], false, "dialog click refused: {result}");
    let events = fixture.wait_for("the dialog click", |events| {
        !Fixture::left_downs(events, "dialog").is_empty()
    });
    assert!(near(Fixture::left_downs(&events, "dialog")[0], point));

    // The dialog now holds the foreground; the main window is still the same process.
    let result = server.click(&id, fixture.center("button"));
    assert_eq!(result["isError"], false, "{result}");
    fixture.wait_for("the button press", |events| {
        events.iter().any(|e| e == "main button ping")
    });
    server.end();
}

#[test]
#[ignore = "live desktop: set CONTROLFREAK_LIVE_DESKTOP=1 on a disposable desktop"]
fn a_foreign_process_window_is_refused_without_poisoning_the_session() {
    let mut server = Server::start();
    let target = Fixture::spawn("target", "2E8B57", LEFT, &[]);
    let foreign = Fixture::spawn("foreign", "FF00FF", RIGHT, &[]);
    let id = server.window_id(&target);
    server.begin(&id);
    server.focus(&id);

    let refused = server.click(&id, foreign.center("edit"));
    assert_eq!(refused["isError"], true, "{refused}");
    let error = &refused["structuredContent"]["error"];
    assert_eq!(error["code"], "target_invalidated", "{refused}");
    // The agent can tell this recoverable refusal from a fatal one without reading prose.
    assert_eq!(error["reason"], "foreign_destination", "{refused}");
    assert_eq!(error["retryable"], true, "{refused}");
    assert_eq!(
        refused["structuredContent"]["input"]["input_outcome"],
        "not_started"
    );

    // The session still has its target: the next action on it succeeds.
    let edit = target.center("edit");
    let accepted = server.click(&id, edit);
    assert_eq!(
        accepted["isError"], false,
        "session was poisoned: {accepted}"
    );
    let events = target.wait_for("the click on the target", |events| {
        !Fixture::left_downs(events, "main").is_empty()
    });
    assert!(near(Fixture::left_downs(&events, "main")[0], edit));
    let foreign_events = foreign.events();
    assert!(
        foreign_events
            .iter()
            .all(|e| !e.contains("mouse") && !e.contains("keydown") && !e.contains("char")),
        "the foreign window received input: {foreign_events:?}"
    );
    server.end();
}

#[test]
#[ignore = "live desktop: set CONTROLFREAK_LIVE_DESKTOP=1 on a disposable desktop"]
fn the_taskbar_gets_a_target_ref_while_it_is_in_front() {
    let mut server = Server::start();
    // The taskbar is a tool window, so list_windows never shows it. A screenshot taken while it
    // is the foreground window carries its reference.
    let status = Command::new(FIXTURE)
        .arg("--activate-taskbar")
        .status()
        .expect("run the fixture's taskbar helper");
    if !status.success() {
        eprintln!("SKIPPED taskbar: Windows would not bring the taskbar to the foreground here");
        return;
    }
    let capture = server.ok(
        "capture_display",
        json!({"max_width": 320, "include_cursor": false}),
    );
    let target = capture["target_ref"]
        .as_str()
        .unwrap_or_else(|| panic!("no target_ref for the foreground taskbar: {capture}"))
        .to_owned();
    server.begin(&target);
    server.end();
}

#[test]
#[ignore = "live desktop: set CONTROLFREAK_LIVE_DESKTOP=1 on a disposable desktop"]
fn an_injected_click_on_the_stop_button_does_not_stop_control() {
    let mut server = Server::start();
    let fixture = Fixture::spawn("stop", "2E8B57", LEFT, &[]);
    let id = server.window_id(&fixture);
    server.begin(&id);
    server.focus(&id);

    // The button hangs under the session tab once the tab is steady.
    let deadline = Instant::now() + Duration::from_secs(20);
    let rect = loop {
        let output = Command::new(FIXTURE)
            .arg("--stop-button-rect")
            .output()
            .expect("run the fixture's button finder");
        if output.status.success() {
            let numbers: Vec<i32> = String::from_utf8_lossy(&output.stdout)
                .split_whitespace()
                .filter_map(|n| n.parse().ok())
                .collect();
            break numbers;
        }
        assert!(
            Instant::now() < deadline,
            "the Stop agent button never appeared during a session"
        );
        thread::sleep(Duration::from_millis(250));
    };
    let center = (rect[0].midpoint(rect[2]), rect[1].midpoint(rect[3]));
    let (x, y) = display_local(center);
    let result = server.click(&id, (i32::try_from(x).unwrap(), i32::try_from(y).unwrap()));
    // The server refuses this itself: the window under the point is its own Stop button, not
    // the approved target. This is the boundary check; it never reaches the button's filter.
    assert_eq!(result["isError"], true, "{result}");
    let error = &result["structuredContent"]["error"];
    assert_eq!(error["reason"], "own_indicator", "{result}");
    assert!(
        error["message"]
            .as_str()
            .unwrap_or_default()
            .contains("Stop button"),
        "{result}"
    );

    // The filter itself: real injected input, sent by the fixture straight to the button and
    // never through an MCP tool. SendInput reports an injected origin, so the button must ignore
    // it and control must continue.
    let status = Command::new(FIXTURE)
        .args(["--inject-click", &format!("{},{}", center.0, center.1)])
        .status()
        .expect("run the fixture's injected click");
    assert!(status.success(), "the fixture could not inject the click");
    thread::sleep(Duration::from_millis(500));

    let status = server.ok("get_server_status", json!({}));
    assert_eq!(status["stop_state"], "ready", "{status}");
    assert!(status.get("stop_reason").is_none(), "{status}");
    let edit = fixture.center("edit");
    let accepted = server.click(&id, edit);
    assert_eq!(
        accepted["isError"], false,
        "control was stopped: {accepted}"
    );
    server.end();
}
