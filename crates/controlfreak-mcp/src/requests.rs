use super::observations::{PRIMARY_DISPLAY, PointerTarget};
use controlfreak_core::{
    FindTextRequest, Key, MouseButton, ObservationMode, ObservationOptions, ObservationRegion,
    OcrRegionRequest, PlatformError, VirtualDesktopDirection,
    limits::{
        DEFAULT_CAPTURE_WIDTH, MAX_RUN_ACTIONS_TOTAL_WAIT_MS, MAX_RUN_ACTIONS_WAIT_MS,
        MAX_SCROLL_DELTA, MAX_TEXT_UTF16_UNITS,
    },
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
    display_id: Option<String>,
    max_width: Option<u32>,
    #[serde(default)]
    native: bool,
    #[serde(default = "default_true")]
    include_cursor: bool,
}

impl CaptureDisplayInput {
    pub(super) fn into_request(
        self,
    ) -> Result<controlfreak_core::CaptureDisplayRequest, PlatformError> {
        Ok(controlfreak_core::CaptureDisplayRequest {
            display_id: display_or_primary(self.display_id),
            max_width: capture_width(self.max_width, self.native)?,
            include_cursor: self.include_cursor,
        })
    }
}

/// The width an explicit capture is returned at: the caller's `max_width`, source resolution
/// when `native` is set, and otherwise the same default width as an action screenshot.
pub(super) fn capture_width(
    max_width: Option<u32>,
    native: bool,
) -> Result<Option<u32>, PlatformError> {
    match (max_width, native) {
        (Some(_), true) => Err(PlatformError::InvalidArgument {
            argument: "native".to_owned(),
            reason: "native=true returns full resolution; leave max_width out".to_owned(),
        }),
        (None, true) => Ok(None),
        (Some(width), false) => Ok(Some(width)),
        (None, false) => Ok(Some(DEFAULT_CAPTURE_WIDTH)),
    }
}

/// The display a request names, or the primary display when it names none.
fn display_or_primary(display_id: Option<String>) -> String {
    display_id.unwrap_or_else(|| PRIMARY_DISPLAY.to_owned())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CaptureRegionInput {
    display_id: Option<String>,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    max_width: Option<u32>,
    #[serde(default)]
    native: bool,
    #[serde(default = "default_true")]
    include_cursor: bool,
}

impl CaptureRegionInput {
    pub(super) fn into_request(
        self,
    ) -> Result<controlfreak_core::CaptureRegionRequest, PlatformError> {
        Ok(controlfreak_core::CaptureRegionRequest {
            display_id: display_or_primary(self.display_id),
            x: self.x,
            y: self.y,
            width: self.width,
            height: self.height,
            max_width: capture_width(self.max_width, self.native)?,
            include_cursor: self.include_cursor,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WaitForVisualChangeInput {
    display_id: Option<String>,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    #[serde(default = "default_wait_timeout_ms")]
    timeout_ms: u32,
    stable_ms: Option<u32>,
    #[serde(default = "default_difference_threshold")]
    difference_threshold: f64,
}

impl WaitForVisualChangeInput {
    pub(super) fn into_request(self) -> controlfreak_core::WaitForVisualChangeRequest {
        let (timeout_ms, stable_ms) = visual_wait_values(self.timeout_ms, self.stable_ms);
        controlfreak_core::WaitForVisualChangeRequest {
            display_id: display_or_primary(self.display_id),
            x: self.x,
            y: self.y,
            width: self.width,
            height: self.height,
            timeout_ms,
            stable_ms,
            difference_threshold: self.difference_threshold,
        }
    }
}

/// Every wait tool publishes the same lower bound of 0 ms, but the visual waits compare at least
/// once over 1 ms. A zero timeout means one comparison, which needs no stability period. A
/// stability period that was not given never exceeds the timeout, so a short timeout works alone.
pub(super) fn visual_wait_values(timeout_ms: u32, stable_ms: Option<u32>) -> (u32, u32) {
    if timeout_ms == 0 {
        (1, 0)
    } else {
        (
            timeout_ms,
            stable_ms.unwrap_or_else(|| default_stable_ms().min(timeout_ms)),
        )
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct VisualBaselineInput {
    display_id: Option<String>,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

impl VisualBaselineInput {
    pub(super) fn into_request(self) -> controlfreak_core::VisualBaselineRequest {
        controlfreak_core::VisualBaselineRequest {
            display_id: display_or_primary(self.display_id),
            x: self.x,
            y: self.y,
            width: self.width,
            height: self.height,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WaitForChangeSinceInput {
    baseline_id: String,
    #[serde(default = "default_wait_timeout_ms")]
    timeout_ms: u32,
    stable_ms: Option<u32>,
    #[serde(default = "default_difference_threshold")]
    difference_threshold: f64,
}

impl WaitForChangeSinceInput {
    pub(super) fn into_request(self) -> controlfreak_core::WaitForChangeSinceRequest {
        let (timeout_ms, stable_ms) = visual_wait_values(self.timeout_ms, self.stable_ms);
        controlfreak_core::WaitForChangeSinceRequest {
            baseline_id: self.baseline_id,
            timeout_ms,
            stable_ms,
            difference_threshold: self.difference_threshold,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OcrRegionInput {
    display_id: Option<String>,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    language: Option<String>,
}

impl OcrRegionInput {
    pub(super) fn into_request(self) -> OcrRegionRequest {
        OcrRegionRequest {
            display_id: display_or_primary(self.display_id),
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
    display_id: Option<String>,
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
    display_id: Option<String>,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    language: Option<String>,
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

impl ClickTextInput {
    /// The OCR region, with an omitted display left as the primary-display alias.
    pub(super) fn region(&self) -> OcrRegionRequest {
        OcrRegionRequest {
            display_id: display_or_primary(self.display_id.clone()),
            x: self.x,
            y: self.y,
            width: self.width,
            height: self.height,
            language: self.language.clone(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WaitForTextInput {
    display_id: Option<String>,
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
    #[serde(default)]
    condition: controlfreak_core::TextCondition,
    #[serde(default = "default_text_wait_ms")]
    timeout_ms: u32,
}

const fn default_text_wait_ms() -> u32 {
    5_000
}

impl WaitForTextInput {
    pub(super) fn into_request(self) -> controlfreak_core::WaitForTextRequest {
        controlfreak_core::WaitForTextRequest {
            find: FindTextInput {
                display_id: self.display_id,
                x: self.x,
                y: self.y,
                width: self.width,
                height: self.height,
                language: self.language,
                query: self.query,
                case_sensitive: self.case_sensitive,
                max_results: self.max_results,
                match_mode: self.match_mode,
                ocr_confusions: self.ocr_confusions,
            }
            .into_request(),
            condition: self.condition,
            timeout_ms: self.timeout_ms,
        }
    }
}

impl FindTextInput {
    pub(super) fn into_request(self) -> FindTextRequest {
        FindTextRequest {
            region: OcrRegionRequest {
                display_id: display_or_primary(self.display_id),
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
    display_id: Option<String>,
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
                display_id: display_or_primary(region.display_id),
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
    #[serde(default)]
    display_id: Option<String>,
    #[serde(default)]
    x: Option<u32>,
    #[serde(default)]
    y: Option<u32>,
    #[serde(default)]
    observation_id: Option<String>,
    #[serde(default)]
    image_x: Option<u32>,
    #[serde(default)]
    image_y: Option<u32>,
    #[serde(default)]
    pub(super) duration_ms: u32,
    #[serde(default)]
    pub(super) observation: ObservationInput,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ClickMouseInput {
    pub(super) target_ref: String,
    #[serde(default)]
    display_id: Option<String>,
    #[serde(default)]
    x: Option<u32>,
    #[serde(default)]
    y: Option<u32>,
    #[serde(default)]
    observation_id: Option<String>,
    #[serde(default)]
    image_x: Option<u32>,
    #[serde(default)]
    image_y: Option<u32>,
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DragMouseInput {
    pub(super) target_ref: String,
    #[serde(default)]
    start_display_id: Option<String>,
    #[serde(default)]
    start_x: Option<u32>,
    #[serde(default)]
    start_y: Option<u32>,
    #[serde(default)]
    end_display_id: Option<String>,
    #[serde(default)]
    end_x: Option<u32>,
    #[serde(default)]
    end_y: Option<u32>,
    #[serde(default)]
    observation_id: Option<String>,
    #[serde(default)]
    start_image_x: Option<u32>,
    #[serde(default)]
    start_image_y: Option<u32>,
    #[serde(default)]
    end_image_x: Option<u32>,
    #[serde(default)]
    end_image_y: Option<u32>,
    #[serde(default = "default_left_button")]
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
    #[serde(default)]
    display_id: Option<String>,
    #[serde(default)]
    x: Option<u32>,
    #[serde(default)]
    y: Option<u32>,
    #[serde(default)]
    observation_id: Option<String>,
    #[serde(default)]
    image_x: Option<u32>,
    #[serde(default)]
    image_y: Option<u32>,
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
    window_id: Option<String>,
    target_ref: Option<String>,
    #[serde(default)]
    pub(super) observation: ObservationInput,
}

impl FocusWindowInput {
    pub(super) fn window_id(&self) -> Result<String, PlatformError> {
        window_reference(self.window_id.as_deref(), self.target_ref.as_deref())?
            .ok_or_else(missing_window_reference)
    }
}

/// `target_ref` is an alias of `window_id`. Giving both is fine only when they agree.
pub(super) fn window_reference(
    window_id: Option<&str>,
    target_ref: Option<&str>,
) -> Result<Option<String>, PlatformError> {
    match (window_id, target_ref) {
        (Some(window_id), Some(target_ref)) if window_id != target_ref => {
            Err(PlatformError::InvalidArgument {
                argument: "target_ref".to_owned(),
                reason:
                    "target_ref is an alias of window_id and the two values differ; give only one"
                        .to_owned(),
            })
        }
        (Some(reference), _) | (None, Some(reference)) => Ok(Some(reference.to_owned())),
        (None, None) => Ok(None),
    }
}

fn missing_window_reference() -> PlatformError {
    PlatformError::InvalidArgument {
        argument: "window_id".to_owned(),
        reason: "give window_id (alias target_ref) from list_windows or a screenshot".to_owned(),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SwitchVirtualDesktopInput {
    pub(super) target_ref: Option<String>,
    pub(super) direction: VirtualDesktopDirection,
    #[serde(default = "default_desktop_steps")]
    pub(super) steps: u8,
    #[serde(default)]
    pub(super) observation: ObservationInput,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CaptureWindowInput {
    window_id: Option<String>,
    target_ref: Option<String>,
    max_width: Option<u32>,
    #[serde(default)]
    native: bool,
    #[serde(default = "default_true")]
    include_cursor: bool,
}

impl CaptureWindowInput {
    pub(super) fn into_request(
        self,
    ) -> Result<controlfreak_core::CaptureWindowRequest, PlatformError> {
        let window_id = window_reference(self.window_id.as_deref(), self.target_ref.as_deref())?
            .ok_or_else(missing_window_reference)?;
        Ok(controlfreak_core::CaptureWindowRequest {
            window_id,
            max_width: capture_width(self.max_width, self.native)?,
            include_cursor: self.include_cursor,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WaitForWindowInput {
    window_id: Option<String>,
    target_ref: Option<String>,
    title_contains: Option<String>,
    class_name: Option<String>,
    process_id: Option<u32>,
    is_foreground: Option<bool>,
    #[serde(default = "default_wait_timeout_ms")]
    timeout_ms: u32,
}

impl WaitForWindowInput {
    pub(super) fn into_request(
        self,
    ) -> Result<controlfreak_core::WaitForWindowRequest, PlatformError> {
        Ok(controlfreak_core::WaitForWindowRequest {
            window_id: window_reference(self.window_id.as_deref(), self.target_ref.as_deref())?,
            title_contains: self.title_contains,
            class_name: self.class_name,
            process_id: self.process_id,
            is_foreground: self.is_foreground,
            timeout_ms: self.timeout_ms,
        })
    }
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
        let reason = error.to_string();
        ErrorData::invalid_params(
            format!("invalid tool arguments: {reason}"),
            Some(json!({ "reason": reason })),
        )
    })
}

const fn default_focus_timeout_ms() -> u32 {
    1_000
}

macro_rules! pointer_target {
    ($input:ty) => {
        impl $input {
            /// The requested position, given either in display pixels or as an image point.
            pub(super) fn pointer_target(&self) -> Result<PointerTarget, PlatformError> {
                PointerTarget::from_arguments(
                    (self.display_id.clone(), self.x, self.y),
                    (self.observation_id.as_deref(), self.image_x, self.image_y),
                    "display_id/x/y or observation_id/image_x/image_y",
                )
            }
        }
    };
}

pointer_target!(MoveMouseInput);
pointer_target!(ClickMouseInput);
pointer_target!(ScrollMouseInput);

impl DragMouseInput {
    /// Start and end positions, each in display pixels or as points in one observation.
    pub(super) fn pointer_targets(&self) -> Result<(PointerTarget, PointerTarget), PlatformError> {
        let uses_image = |x: Option<u32>, y: Option<u32>| x.is_some() || y.is_some();
        let start_image = uses_image(self.start_image_x, self.start_image_y);
        let end_image = uses_image(self.end_image_x, self.end_image_y);
        if self.observation_id.is_some() && !start_image && !end_image {
            return Err(PlatformError::InvalidArgument {
                argument: "observation_id".to_owned(),
                reason: "give start_image_x/start_image_y or end_image_x/end_image_y with it"
                    .to_owned(),
            });
        }
        let observation = |used: bool| used.then_some(self.observation_id.as_deref()).flatten();
        Ok((
            PointerTarget::from_arguments(
                (self.start_display_id.clone(), self.start_x, self.start_y),
                (
                    observation(start_image),
                    self.start_image_x,
                    self.start_image_y,
                ),
                "start_display_id/start_x/start_y or observation_id/start_image_x/start_image_y",
            )?,
            PointerTarget::from_arguments(
                (self.end_display_id.clone(), self.end_x, self.end_y),
                (observation(end_image), self.end_image_x, self.end_image_y),
                "end_display_id/end_x/end_y or observation_id/end_image_x/end_image_y",
            )?,
        ))
    }
}

/// Most steps one `run_actions` call accepts.
pub(super) const MAX_RUN_ACTION_STEPS: usize = 16;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RunActionsInput {
    pub(super) target_ref: String,
    steps: Vec<ActionStepInput>,
    #[serde(default)]
    pub(super) observation: ObservationInput,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum ActionStepInput {
    Click {
        #[serde(default)]
        display_id: Option<String>,
        #[serde(default)]
        x: Option<u32>,
        #[serde(default)]
        y: Option<u32>,
        #[serde(default)]
        observation_id: Option<String>,
        #[serde(default)]
        image_x: Option<u32>,
        #[serde(default)]
        image_y: Option<u32>,
        #[serde(default = "default_left_button")]
        button: MouseButton,
        #[serde(default = "default_click_count")]
        click_count: u8,
        #[serde(default)]
        modifiers: Vec<Key>,
    },
    Scroll {
        #[serde(default)]
        display_id: Option<String>,
        #[serde(default)]
        x: Option<u32>,
        #[serde(default)]
        y: Option<u32>,
        #[serde(default)]
        observation_id: Option<String>,
        #[serde(default)]
        image_x: Option<u32>,
        #[serde(default)]
        image_y: Option<u32>,
        #[serde(default)]
        delta_x: i32,
        #[serde(default)]
        delta_y: i32,
    },
    TypeText {
        text: String,
    },
    PressKeys {
        keys: Vec<Key>,
    },
    Wait {
        wait_ms: u32,
    },
}

/// A batch step checked before any step runs. Image points are resolved later, on the worker,
/// but still before the first step.
#[derive(Debug, Clone)]
pub(super) enum ActionStep {
    Click {
        target: PointerTarget,
        button: MouseButton,
        click_count: u8,
        modifiers: Vec<Key>,
    },
    Scroll {
        target: PointerTarget,
        delta_x: i32,
        delta_y: i32,
    },
    TypeText {
        text: String,
    },
    PressKeys {
        keys: Vec<Key>,
    },
    Wait {
        wait_ms: u32,
    },
}

impl ActionStep {
    pub(super) const fn kind(&self) -> &'static str {
        match self {
            Self::Click { .. } => "click",
            Self::Scroll { .. } => "scroll",
            Self::TypeText { .. } => "type_text",
            Self::PressKeys { .. } => "press_keys",
            Self::Wait { .. } => "wait",
        }
    }
}

fn point(
    display: (Option<String>, Option<u32>, Option<u32>),
    image: (Option<&str>, Option<u32>, Option<u32>),
) -> Result<PointerTarget, String> {
    PointerTarget::from_arguments(display, image, "point").map_err(|error| match error {
        PlatformError::InvalidArgument { reason, .. } => reason,
        other => other.to_string(),
    })
}

fn validate_modifiers(modifiers: &[Key]) -> Result<(), String> {
    let unique: std::collections::HashSet<_> = modifiers.iter().collect();
    if modifiers.len() > 4
        || unique.len() != modifiers.len()
        || modifiers
            .iter()
            .any(|key| !matches!(key, Key::Ctrl | Key::Alt | Key::Shift | Key::Win))
    {
        return Err("modifiers may contain only distinct ctrl, alt, shift, and win".to_owned());
    }
    Ok(())
}

impl ActionStepInput {
    /// Checks one step with the same limits as its single-action tool.
    fn validate(&self) -> Result<ActionStep, String> {
        match self {
            Self::Click {
                display_id,
                x,
                y,
                observation_id,
                image_x,
                image_y,
                button,
                click_count,
                modifiers,
            } => {
                if !(1..=3).contains(click_count) {
                    return Err("click_count must be between 1 and 3".to_owned());
                }
                validate_modifiers(modifiers)?;
                Ok(ActionStep::Click {
                    target: point(
                        (display_id.clone(), *x, *y),
                        (observation_id.as_deref(), *image_x, *image_y),
                    )?,
                    button: *button,
                    click_count: *click_count,
                    modifiers: modifiers.clone(),
                })
            }
            Self::Scroll {
                display_id,
                x,
                y,
                observation_id,
                image_x,
                image_y,
                delta_x,
                delta_y,
            } => {
                let limit = MAX_SCROLL_DELTA;
                if (*delta_x == 0 && *delta_y == 0)
                    || delta_x.unsigned_abs() > limit
                    || delta_y.unsigned_abs() > limit
                {
                    return Err(format!(
                        "scroll needs a nonzero delta within -{limit} to {limit}"
                    ));
                }
                Ok(ActionStep::Scroll {
                    target: point(
                        (display_id.clone(), *x, *y),
                        (observation_id.as_deref(), *image_x, *image_y),
                    )?,
                    delta_x: *delta_x,
                    delta_y: *delta_y,
                })
            }
            Self::TypeText { text } => {
                let units = text.encode_utf16().count();
                if units == 0 || units > MAX_TEXT_UTF16_UNITS {
                    return Err(format!(
                        "text must contain 1 to {MAX_TEXT_UTF16_UNITS} UTF-16 code units"
                    ));
                }
                Ok(ActionStep::TypeText { text: text.clone() })
            }
            Self::PressKeys { keys } => {
                let unique: std::collections::HashSet<_> = keys.iter().collect();
                if keys.is_empty() || keys.len() > 8 || unique.len() != keys.len() {
                    return Err("keys must be 1 to 8 distinct keys".to_owned());
                }
                Ok(ActionStep::PressKeys { keys: keys.clone() })
            }
            Self::Wait { wait_ms } => {
                if !(1..=MAX_RUN_ACTIONS_WAIT_MS).contains(wait_ms) {
                    return Err(format!(
                        "wait_ms must be between 1 and {MAX_RUN_ACTIONS_WAIT_MS}"
                    ));
                }
                Ok(ActionStep::Wait { wait_ms: *wait_ms })
            }
        }
    }
}

impl RunActionsInput {
    /// Validates every step's arguments, so a malformed later step cannot strand earlier input.
    pub(super) fn validated_steps(&self) -> Result<Vec<ActionStep>, PlatformError> {
        if self.steps.is_empty() || self.steps.len() > MAX_RUN_ACTION_STEPS {
            return Err(PlatformError::InvalidArgument {
                argument: "steps".to_owned(),
                reason: format!("must contain between 1 and {MAX_RUN_ACTION_STEPS} steps"),
            });
        }
        let steps = self
            .steps
            .iter()
            .enumerate()
            .map(|(index, step)| {
                step.validate()
                    .map_err(|reason| PlatformError::InvalidArgument {
                        argument: format!("steps[{index}]"),
                        reason,
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        // The final step supplies the observation, and only an input step can.
        if matches!(steps.last(), Some(ActionStep::Wait { .. })) {
            return Err(PlatformError::InvalidArgument {
                argument: format!("steps[{}]", steps.len() - 1),
                reason: "the last step cannot be a wait; end with an input step".to_owned(),
            });
        }
        let total_wait: u32 = steps
            .iter()
            .filter_map(|step| match step {
                ActionStep::Wait { wait_ms } => Some(*wait_ms),
                _ => None,
            })
            .sum();
        if total_wait > MAX_RUN_ACTIONS_TOTAL_WAIT_MS {
            return Err(PlatformError::InvalidArgument {
                argument: "steps".to_owned(),
                reason: format!(
                    "wait steps add up to {total_wait} ms; the total may not exceed {MAX_RUN_ACTIONS_TOTAL_WAIT_MS} ms"
                ),
            });
        }
        Ok(steps)
    }
}
