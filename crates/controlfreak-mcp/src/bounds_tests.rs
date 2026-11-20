//! Result bounds over the transport: `list_virtual_desktops` reports the window cap, and
//! `read_text_in_region` stops at a whole line under its line, character and byte limits.

use super::output_schema_tests::full_validator;
use super::session_error_tests::{call, harness_with_backend};
use super::*;
use controlfreak_core::limits::{
    MAX_READ_TEXT_BYTES, MAX_READ_TEXT_CHARS, MAX_READ_TEXT_LINES, MAX_WINDOWS,
};
use controlfreak_core::{
    BackendMetadata, DisplayBackend, DisplayBounds, DisplayInfo, KeyboardBackend, OcrBackend,
    OcrLine, OcrResult, OcrWord, PointerBackend, WindowBackend, WindowBounds, WindowInfo,
    WindowList,
};

/// The separator the fake engine puts between lines in `text`.
const SEPARATOR: &str = " ";

fn display() -> DisplayInfo {
    DisplayInfo {
        id: "synthetic".into(),
        name: "synthetic".into(),
        bounds: DisplayBounds {
            left: -1920,
            top: 0,
            width: 1920,
            height: 1080,
        },
        is_primary: true,
    }
}

fn window(index: usize) -> WindowInfo {
    WindowInfo {
        id: format!("target-{index:08}"),
        title: format!("window {index}"),
        class_name: "synthetic".into(),
        process_id: u32::try_from(index % 7).unwrap(),
        bounds: WindowBounds {
            left: 0,
            top: 0,
            width: 10,
            height: 10,
        },
        display_id: "synthetic".into(),
        is_foreground: index == 0,
        is_minimized: false,
        owner: None,
    }
}

/// A line of `words` copies of `word`, one row below the previous line.
fn line(row: usize, word: &str, words: usize) -> OcrLine {
    let y = u32::try_from(row).unwrap() * 20;
    let words: Vec<OcrWord> = (0..words)
        .map(|index| OcrWord {
            text: word.to_owned(),
            x: u32::try_from(index).unwrap() * 10,
            y,
            width: 9,
            height: 16,
        })
        .collect();
    OcrLine {
        text: words
            .iter()
            .map(|word| word.text.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        x: 0,
        y,
        width: u32::try_from(words.len()).unwrap() * 10,
        height: 16,
        words,
    }
}

struct BoundsBackend {
    windows: usize,
    lines: Vec<OcrLine>,
    /// `None` joins the lines with [`SEPARATOR`], as the engine's own text would.
    text: Option<String>,
}

impl BoundsBackend {
    fn with_lines(lines: Vec<OcrLine>) -> Self {
        Self {
            windows: 1,
            lines,
            text: None,
        }
    }
}

impl BackendMetadata for BoundsBackend {
    fn validate_target_reference(&self, _: &str) -> Result<(), PlatformError> {
        Ok(())
    }
    fn identity(&self) -> controlfreak_core::BackendIdentity {
        controlfreak_core::BackendIdentity {
            platform: controlfreak_core::Platform::Windows,
            display_server: controlfreak_core::DisplayServer::Dwm,
            backend_id: "bounds-fixture".into(),
        }
    }
    fn capabilities(&self) -> Vec<controlfreak_core::CapabilityDescriptor> {
        vec![]
    }
    fn permissions(&self) -> Vec<controlfreak_core::PermissionDescriptor> {
        vec![]
    }
}

impl DisplayBackend for BoundsBackend {
    fn list_displays(&self) -> Result<Vec<DisplayInfo>, PlatformError> {
        Ok(vec![display()])
    }
}

impl OcrBackend for BoundsBackend {
    fn read_text_in_region(&self, _: &OcrRegionRequest) -> Result<OcrResult, PlatformError> {
        let text = self.text.clone().unwrap_or_else(|| {
            self.lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>()
                .join(SEPARATOR)
        });
        Ok(OcrResult {
            target_ref: None,
            display: display(),
            source_bounds: display().bounds,
            language: "en-US".into(),
            text,
            lines: self.lines.clone(),
        })
    }
}

impl WindowBackend for BoundsBackend {
    /// Cut at the real cap, like the Windows backend.
    fn list_windows(&self) -> Result<WindowList, PlatformError> {
        Ok(WindowList {
            windows: (0..self.windows.min(MAX_WINDOWS)).map(window).collect(),
            truncated: self.windows > MAX_WINDOWS,
        })
    }
    /// Two desktops by window parity, grouped by the same code as the Windows backend.
    fn list_virtual_desktops(&self) -> Result<VirtualDesktopList, PlatformError> {
        Ok(VirtualDesktopList::from_windows(
            self.list_windows()?,
            Some("desktop-even"),
            |window| {
                let even = window.id.ends_with(['0', '2', '4', '6', '8']);
                let id = if even { "desktop-even" } else { "desktop-odd" };
                Some((id.to_owned(), even))
            },
        ))
    }
}

impl PointerBackend for BoundsBackend {}
impl KeyboardBackend for BoundsBackend {}

/// Calls `name` and checks the result against both published schemas and its own text block.
async fn checked_call(backend: BoundsBackend, name: &str, arguments: Value) -> Value {
    let mut harness =
        harness_with_backend(Arc::new(backend), Arc::new(AtomicBool::new(false))).await;
    let listed = outcome_tests::exchange(
        &mut harness.client,
        json!({"jsonrpc":"2.0","id":9,"method":"tools/list","params":{}}),
    )
    .await;
    let result = call(&mut harness, name, arguments).await;
    harness.finish().await;
    assert_ne!(result["isError"], true, "{name}: {result}");
    let content = result["structuredContent"].clone();
    let schema = &listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == name)
        .unwrap()["outputSchema"];
    assert!(
        jsonschema::validator_for(schema)
            .unwrap()
            .is_valid(&content),
        "{name} result does not match its outputSchema"
    );
    assert!(
        full_validator(name).is_valid(&content),
        "{name} result does not match the full contract"
    );
    let text_block: Value =
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(
        text_block, content,
        "the text block and structured content differ"
    );
    content
}

#[tokio::test]
async fn list_virtual_desktops_reports_a_window_list_cut_at_its_cap() {
    for (windows, truncated) in [(MAX_WINDOWS + 50, true), (MAX_WINDOWS, false), (3, false)] {
        let backend = BoundsBackend {
            windows,
            lines: vec![],
            text: None,
        };
        let content = checked_call(backend, LIST_VIRTUAL_DESKTOPS, json!({})).await;
        assert_eq!(content["truncated"], truncated, "{windows} windows");
        let grouped: usize = content["desktops"]
            .as_array()
            .unwrap()
            .iter()
            .map(|desktop| desktop["windows"].as_array().unwrap().len())
            .sum();
        assert_eq!(grouped, windows.min(MAX_WINDOWS), "{windows} windows");
        assert_eq!(content["count"], 2);
    }
}

fn read_region() -> Value {
    json!({"display_id": "synthetic", "x": 0, "y": 0, "width": 1920, "height": 1080})
}

fn kept_lines(content: &Value) -> Vec<OcrLine> {
    serde_json::from_value(content["lines"].clone()).unwrap()
}

fn serialized_len(lines: &[OcrLine]) -> usize {
    serde_json::to_vec(lines).unwrap().len()
}

/// The kept lines are a prefix of the recognized ones, and `text` holds exactly them.
fn assert_whole_line_prefix(content: &Value, all: &[OcrLine]) -> Vec<OcrLine> {
    let kept = kept_lines(content);
    assert_eq!(kept.as_slice(), &all[..kept.len()]);
    let expected = kept
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join(SEPARATOR);
    assert_eq!(content["text"], expected);
    kept
}

#[tokio::test]
async fn read_text_in_region_returns_everything_under_the_bounds() {
    let lines: Vec<OcrLine> = (0..3).map(|row| line(row, "word", 4)).collect();
    let content = checked_call(
        BoundsBackend::with_lines(lines.clone()),
        READ_TEXT_IN_REGION,
        read_region(),
    )
    .await;
    assert_eq!(content["truncated"], false);
    assert_eq!(content["limit"], Value::Null);
    assert_eq!(assert_whole_line_prefix(&content, &lines), lines);
}

#[tokio::test]
async fn read_text_in_region_stops_at_the_line_limit() {
    let lines: Vec<OcrLine> = (0..MAX_READ_TEXT_LINES + 50)
        .map(|row| line(row, "ab", 1))
        .collect();
    let content = checked_call(
        BoundsBackend::with_lines(lines.clone()),
        READ_TEXT_IN_REGION,
        read_region(),
    )
    .await;
    assert_eq!(content["truncated"], true);
    assert_eq!(content["limit"], "lines");
    assert_eq!(
        assert_whole_line_prefix(&content, &lines).len(),
        MAX_READ_TEXT_LINES
    );
}

#[tokio::test]
async fn read_text_in_region_stops_at_the_character_limit_on_a_whole_line() {
    // 999 characters a line, so the limit falls inside a line, which is left out whole.
    let lines: Vec<OcrLine> = (0..20).map(|row| line(row, &"é".repeat(999), 1)).collect();
    let content = checked_call(
        BoundsBackend::with_lines(lines.clone()),
        READ_TEXT_IN_REGION,
        read_region(),
    )
    .await;
    assert_eq!(content["truncated"], true);
    assert_eq!(content["limit"], "characters");
    let kept = assert_whole_line_prefix(&content, &lines);
    assert_eq!(kept.len(), MAX_READ_TEXT_CHARS / 999);
}

#[tokio::test]
async fn read_text_in_region_stops_at_the_byte_limit_on_a_whole_line() {
    // Few characters but many word boxes, so the serialized lines reach the byte limit first.
    let lines: Vec<OcrLine> = (0..80).map(|row| line(row, "a", 120)).collect();
    let content = checked_call(
        BoundsBackend::with_lines(lines.clone()),
        READ_TEXT_IN_REGION,
        read_region(),
    )
    .await;
    assert_eq!(content["truncated"], true);
    assert_eq!(content["limit"], "bytes");
    let kept = assert_whole_line_prefix(&content, &lines);
    assert!(!kept.is_empty());
    assert!(serialized_len(&kept) <= MAX_READ_TEXT_BYTES);
    assert!(serialized_len(&lines[..=kept.len()]) > MAX_READ_TEXT_BYTES);
}

#[tokio::test]
async fn a_cut_text_that_cannot_be_matched_to_its_lines_is_rebuilt_from_them() {
    let lines: Vec<OcrLine> = (0..=MAX_READ_TEXT_LINES)
        .map(|row| line(row, "ab", 1))
        .collect();
    let backend = BoundsBackend {
        windows: 1,
        lines,
        text: Some("text that does not hold the lines".into()),
    };
    let content = checked_call(backend, READ_TEXT_IN_REGION, read_region()).await;
    assert_eq!(content["limit"], "lines");
    let kept = kept_lines(&content);
    let joined = kept
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(content["text"], joined);
}

#[test]
fn the_published_descriptions_state_the_limits() {
    let description = |name: &str| {
        tools()
            .into_iter()
            .find(|tool| tool.name == name)
            .and_then(|tool| tool.description)
            .unwrap()
            .to_string()
    };
    let read = description(READ_TEXT_IN_REGION);
    for limit in [
        format!("{MAX_READ_TEXT_LINES} lines"),
        format!("{MAX_READ_TEXT_CHARS} characters"),
        format!("{} KiB", MAX_READ_TEXT_BYTES / 1024),
    ] {
        assert!(read.contains(&limit), "{limit}: {read}");
    }
    for name in [LIST_WINDOWS, LIST_VIRTUAL_DESKTOPS] {
        assert!(
            description(name).contains(&format!("at most {MAX_WINDOWS}")),
            "{name}"
        );
    }
}
