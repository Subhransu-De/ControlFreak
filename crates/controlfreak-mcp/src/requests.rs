use super::observations::PointerTarget;
use controlfreak_core::{
    FindTextRequest, Key, MouseButton, ObservationMode, ObservationOptions, ObservationRegion,
    OcrRegionRequest, PlatformError, VirtualDesktopDirection,
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WaitForTextInput {
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
    #[serde(default)]
    pub(super) display_id: Option<String>,
    #[serde(default)]
    pub(super) x: Option<u32>,
    #[serde(default)]
    pub(super) y: Option<u32>,
    #[serde(default)]
    pub(super) observation_id: Option<String>,
    #[serde(default)]
    pub(super) image_x: Option<u32>,
    #[serde(default)]
    pub(super) image_y: Option<u32>,
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
    pub(super) display_id: Option<String>,
    #[serde(default)]
    pub(super) x: Option<u32>,
    #[serde(default)]
    pub(super) y: Option<u32>,
    #[serde(default)]
    pub(super) observation_id: Option<String>,
    #[serde(default)]
    pub(super) image_x: Option<u32>,
    #[serde(default)]
    pub(super) image_y: Option<u32>,
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
    pub(super) start_display_id: Option<String>,
    #[serde(default)]
    pub(super) start_x: Option<u32>,
    #[serde(default)]
    pub(super) start_y: Option<u32>,
    #[serde(default)]
    pub(super) end_display_id: Option<String>,
    #[serde(default)]
    pub(super) end_x: Option<u32>,
    #[serde(default)]
    pub(super) end_y: Option<u32>,
    #[serde(default)]
    pub(super) observation_id: Option<String>,
    #[serde(default)]
    pub(super) start_image_x: Option<u32>,
    #[serde(default)]
    pub(super) start_image_y: Option<u32>,
    #[serde(default)]
    pub(super) end_image_x: Option<u32>,
    #[serde(default)]
    pub(super) end_image_y: Option<u32>,
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
    pub(super) display_id: Option<String>,
    #[serde(default)]
    pub(super) x: Option<u32>,
    #[serde(default)]
    pub(super) y: Option<u32>,
    #[serde(default)]
    pub(super) observation_id: Option<String>,
    #[serde(default)]
    pub(super) image_x: Option<u32>,
    #[serde(default)]
    pub(super) image_y: Option<u32>,
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
    pub(super) steps: Vec<ActionStepInput>,
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
}

impl ActionStep {
    pub(super) const fn kind(&self) -> &'static str {
        match self {
            Self::Click { .. } => "click",
            Self::Scroll { .. } => "scroll",
            Self::TypeText { .. } => "type_text",
            Self::PressKeys { .. } => "press_keys",
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
                let limit = super::MAX_SCROLL_DELTA.unsigned_abs();
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
                if units == 0 || units > super::MAX_TEXT_UTF16_UNITS {
                    return Err(format!(
                        "text must contain 1 to {} UTF-16 code units",
                        super::MAX_TEXT_UTF16_UNITS
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
        self.steps
            .iter()
            .enumerate()
            .map(|(index, step)| {
                step.validate()
                    .map_err(|reason| PlatformError::InvalidArgument {
                        argument: format!("steps[{index}]"),
                        reason,
                    })
            })
            .collect()
    }
}
