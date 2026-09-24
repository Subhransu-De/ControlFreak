use controlfreak_core::{
    FindTextRequest, Key, MouseButton, ObservationMode, ObservationOptions, ObservationRegion,
    OcrRegionRequest, VirtualDesktopDirection,
};
use rmcp::{ErrorData, model::JsonObject};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NoArguments {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BeginControlSessionInput {
    pub(super) target_ref: String,
    pub(super) expected_seconds: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CaptureDisplayInput {
    pub(super) display_id: String,
    pub(super) max_width: Option<u32>,
    #[serde(default = "default_true")]
    pub(super) include_cursor: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CaptureRegionInput {
    pub(super) display_id: String,
    pub(super) x: u32,
    pub(super) y: u32,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) max_width: Option<u32>,
    #[serde(default = "default_true")]
    pub(super) include_cursor: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WaitForVisualChangeInput {
    pub(super) display_id: String,
    pub(super) x: u32,
    pub(super) y: u32,
    pub(super) width: u32,
    pub(super) height: u32,
    #[serde(default = "default_wait_timeout_ms")]
    pub(super) timeout_ms: u32,
    #[serde(default = "default_stable_ms")]
    pub(super) stable_ms: u32,
    #[serde(default = "default_difference_threshold")]
    pub(super) difference_threshold: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct VisualBaselineInput {
    pub(super) display_id: String,
    pub(super) x: u32,
    pub(super) y: u32,
    pub(super) width: u32,
    pub(super) height: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WaitForChangeSinceInput {
    pub(super) baseline_id: String,
    #[serde(default = "default_wait_timeout_ms")]
    pub(super) timeout_ms: u32,
    #[serde(default = "default_stable_ms")]
    pub(super) stable_ms: u32,
    #[serde(default = "default_difference_threshold")]
    pub(super) difference_threshold: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OcrRegionInput {
    display_id: String,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    language: Option<String>,
}

impl OcrRegionInput {
    pub(super) fn into_request(self) -> OcrRegionRequest {
        OcrRegionRequest {
            display_id: self.display_id,
            x: self.x,
            y: self.y,
            width: self.width,
            height: self.height,
            language: self.language,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FindTextInput {
    display_id: String,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    language: Option<String>,
    query: String,
    #[serde(default)]
    case_sensitive: bool,
    #[serde(default = "default_max_ocr_results")]
    max_results: u32,
    #[serde(default)]
    match_mode: controlfreak_core::TextMatchMode,
    #[serde(default)]
    ocr_confusions: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ClickTextInput {
    pub(super) target_ref: String,
    pub(super) display_id: String,
    pub(super) x: u32,
    pub(super) y: u32,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) language: Option<String>,
    pub(super) query: String,
    #[serde(default)]
    pub(super) case_sensitive: bool,
    #[serde(default = "default_true")]
    pub(super) exact_match: bool,
    #[serde(default = "default_left_button")]
    pub(super) button: MouseButton,
    #[serde(default = "default_click_count")]
    pub(super) click_count: u8,
    #[serde(default)]
    pub(super) modifiers: Vec<Key>,
    #[serde(default)]
    pub(super) duration_ms: u32,
    #[serde(default)]
    pub(super) observation: ObservationInput,
}

impl FindTextInput {
    pub(super) fn into_request(self) -> FindTextRequest {
        FindTextRequest {
            region: OcrRegionRequest {
                display_id: self.display_id,
                x: self.x,
                y: self.y,
                width: self.width,
                height: self.height,
                language: self.language,
            },
            query: self.query,
            case_sensitive: self.case_sensitive,
            max_results: self.max_results,
            match_mode: self.match_mode,
            ocr_confusions: self.ocr_confusions,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ObservationInput {
    #[serde(default)]
    mode: ObservationMode,
    max_width: Option<u32>,
    #[serde(default = "default_true")]
    include_cursor: bool,
    pub(super) region: Option<ObservationRegionInput>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ObservationRegionInput {
    display_id: String,
    x: u32,
    y: u32,
    pub(super) width: u32,
    height: u32,
}

impl Default for ObservationInput {
    fn default() -> Self {
        Self {
            mode: ObservationMode::Screenshot,
            max_width: None,
            include_cursor: true,
            region: None,
        }
    }
}

impl From<ObservationInput> for ObservationOptions {
    fn from(input: ObservationInput) -> Self {
        Self {
            mode: input.mode,
            max_width: input.max_width,
            include_cursor: input.include_cursor,
            region: input.region.map(|region| ObservationRegion {
                display_id: region.display_id,
                x: region.x,
                y: region.y,
                width: region.width,
                height: region.height,
            }),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MoveMouseInput {
    pub(super) target_ref: String,
    pub(super) display_id: String,
    pub(super) x: u32,
    pub(super) y: u32,
    #[serde(default)]
    pub(super) duration_ms: u32,
    #[serde(default)]
    pub(super) observation: ObservationInput,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ClickMouseInput {
    pub(super) target_ref: String,
    pub(super) display_id: String,
    pub(super) x: u32,
    pub(super) y: u32,
    pub(super) button: MouseButton,
    #[serde(default = "default_click_count")]
    pub(super) click_count: u8,
    #[serde(default)]
    pub(super) modifiers: Vec<Key>,
    #[serde(default)]
    pub(super) duration_ms: u32,
    #[serde(default)]
    pub(super) observation: ObservationInput,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DragMouseInput {
    pub(super) target_ref: String,
    pub(super) start_display_id: String,
    pub(super) start_x: u32,
    pub(super) start_y: u32,
    pub(super) end_display_id: String,
    pub(super) end_x: u32,
    pub(super) end_y: u32,
    pub(super) button: MouseButton,
    #[serde(default)]
    pub(super) modifiers: Vec<Key>,
    #[serde(default)]
    pub(super) duration_ms: u32,
    #[serde(default)]
    pub(super) observation: ObservationInput,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ScrollMouseInput {
    pub(super) target_ref: String,
    pub(super) display_id: String,
    pub(super) x: u32,
    pub(super) y: u32,
    #[serde(default)]
    pub(super) delta_x: i32,
    pub(super) delta_y: i32,
    #[serde(default)]
    pub(super) duration_ms: u32,
    #[serde(default)]
    pub(super) observation: ObservationInput,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FocusWindowInput {
    #[serde(default = "default_focus_timeout_ms")]
    pub(super) timeout_ms: u32,
    pub(super) window_id: String,
    #[serde(default)]
    pub(super) observation: ObservationInput,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SwitchVirtualDesktopInput {
    pub(super) target_ref: String,
    pub(super) direction: VirtualDesktopDirection,
    #[serde(default = "default_desktop_steps")]
    pub(super) steps: u8,
    #[serde(default)]
    pub(super) observation: ObservationInput,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CaptureWindowInput {
    pub(super) window_id: String,
    pub(super) max_width: Option<u32>,
    #[serde(default = "default_true")]
    pub(super) include_cursor: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WaitForWindowInput {
    pub(super) window_id: Option<String>,
    pub(super) title_contains: Option<String>,
    pub(super) class_name: Option<String>,
    pub(super) process_id: Option<u32>,
    pub(super) is_foreground: Option<bool>,
    #[serde(default = "default_wait_timeout_ms")]
    pub(super) timeout_ms: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PressKeysInput {
    pub(super) target_ref: String,
    pub(super) keys: Vec<Key>,
    #[serde(default)]
    pub(super) observation: ObservationInput,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TypeTextInput {
    pub(super) target_ref: String,
    pub(super) text: String,
    #[serde(default)]
    pub(super) observation: ObservationInput,
}

const fn default_true() -> bool {
    true
}

const fn default_click_count() -> u8 {
    1
}

const fn default_left_button() -> MouseButton {
    MouseButton::Left
}

const fn default_desktop_steps() -> u8 {
    1
}

const fn default_wait_timeout_ms() -> u32 {
    5_000
}

const fn default_stable_ms() -> u32 {
    250
}

const fn default_difference_threshold() -> f64 {
    0.01
}

const fn default_max_ocr_results() -> u32 {
    20
}

pub(super) fn parse_arguments<T>(arguments: Option<JsonObject>) -> Result<T, ErrorData>
where
    T: for<'de> Deserialize<'de>,
{
    serde_json::from_value(Value::Object(arguments.unwrap_or_default())).map_err(|error| {
        ErrorData::invalid_params(
            format!("invalid tool arguments: {error}"),
            Some(json!({ "reason": error.to_string() })),
        )
    })
}

const fn default_focus_timeout_ms() -> u32 {
    1_000
}
