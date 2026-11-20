use serde::{Deserialize, Serialize};

use crate::{Key, PlatformError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayBounds {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

impl DisplayBounds {
    const fn contains_local(self, x: u32, y: u32) -> bool {
        x < self.width && y < self.height
    }

    pub fn to_virtual(self, x: u32, y: u32) -> Result<(i32, i32), PlatformError> {
        if !self.contains_local(x, y) {
            return Err(PlatformError::InvalidArgument {
                argument: "coordinates".to_owned(),
                reason: format!(
                    "local point ({x}, {y}) is outside {}x{} display bounds",
                    self.width, self.height
                ),
            });
        }

        let x = i32::try_from(x).map_err(|_| PlatformError::InvalidArgument {
            argument: "x".to_owned(),
            reason: "coordinate exceeds the supported range".to_owned(),
        })?;
        let y = i32::try_from(y).map_err(|_| PlatformError::InvalidArgument {
            argument: "y".to_owned(),
            reason: "coordinate exceeds the supported range".to_owned(),
        })?;

        let virtual_x = self
            .left
            .checked_add(x)
            .ok_or_else(|| PlatformError::InvalidArgument {
                argument: "x".to_owned(),
                reason: "virtual coordinate overflow".to_owned(),
            })?;
        let virtual_y = self
            .top
            .checked_add(y)
            .ok_or_else(|| PlatformError::InvalidArgument {
                argument: "y".to_owned(),
                reason: "virtual coordinate overflow".to_owned(),
            })?;

        Ok((virtual_x, virtual_y))
    }
}

/// The image size of a capture of a `source_width` x `source_height` rectangle under an optional
/// `max_width` budget: never wider than the source, the height rounded to keep the aspect ratio,
/// and at least one pixel high. Image points map back through the source bounds, so this is the
/// size every observation is registered with.
#[must_use]
pub fn scaled_dimensions(
    source_width: u32,
    source_height: u32,
    max_width: Option<u32>,
) -> (u32, u32) {
    let image_width = max_width.map_or(source_width, |width| width.min(source_width));
    let image_height = u32::try_from(
        (u64::from(source_height) * u64::from(image_width) + u64::from(source_width / 2))
            / u64::from(source_width),
    )
    .unwrap_or(source_height)
    .max(1);
    (image_width, image_height)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayInfo {
    pub id: String,
    pub name: String,
    pub bounds: DisplayBounds,
    pub is_primary: bool,
}

/// The active displays by ID and bounds, and nothing else: enough to tell that a display was
/// added, removed or moved since a capture. The order a backend lists displays in does not
/// matter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayLayout(Vec<(String, DisplayBounds)>);

impl DisplayLayout {
    #[must_use]
    pub fn from_displays(displays: &[DisplayInfo]) -> Self {
        let mut layout: Vec<_> = displays
            .iter()
            .map(|display| (display.id.clone(), display.bounds))
            .collect();
        layout.sort_by(|left, right| left.0.cmp(&right.0));
        Self(layout)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayScreenshot {
    pub target_ref: Option<String>,
    pub display: DisplayInfo,
    pub source_bounds: DisplayBounds,
    /// The display layout the source bounds belong to, read after the pixels were.
    pub display_layout: DisplayLayout,
    pub png: Vec<u8>,
    pub image_width: u32,
    pub image_height: u32,
    pub cursor_marker: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureDisplayRequest {
    pub display_id: String,
    pub max_width: Option<u32>,
    pub include_cursor: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ObservationMode {
    None,
    Metadata,
    #[default]
    Screenshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationOptions {
    pub mode: ObservationMode,
    pub max_width: Option<u32>,
    pub include_cursor: bool,
    pub region: Option<ObservationRegion>,
    /// Longest wait, in milliseconds, for the approved window to settle after input and before
    /// the evidence is read. `None` uses the default for the mode; `Some(0)` keeps the short
    /// fixed wait.
    pub settle_ms: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationRegion {
    pub display_id: String,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Default for ObservationOptions {
    fn default() -> Self {
        Self {
            mode: ObservationMode::Screenshot,
            max_width: None,
            include_cursor: true,
            region: None,
            settle_ms: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MouseMoveRequest {
    pub display_id: String,
    pub x: u32,
    pub y: u32,
    pub duration_ms: u32,
    pub observation: ObservationOptions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MouseClickRequest {
    pub display_id: String,
    pub x: u32,
    pub y: u32,
    pub button: MouseButton,
    pub click_count: u8,
    pub modifiers: Vec<Key>,
    pub duration_ms: u32,
    pub observation: ObservationOptions,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MouseDragRequest {
    pub start_display_id: String,
    pub start_x: u32,
    pub start_y: u32,
    pub end_display_id: String,
    pub end_x: u32,
    pub end_y: u32,
    pub button: MouseButton,
    pub modifiers: Vec<Key>,
    pub duration_ms: u32,
    pub observation: ObservationOptions,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureRegionRequest {
    pub display_id: String,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub max_width: Option<u32>,
    pub include_cursor: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WaitForVisualChangeRequest {
    pub display_id: String,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub timeout_ms: u32,
    pub stable_ms: u32,
    pub difference_threshold: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VisualBaselineRequest {
    pub display_id: String,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VisualBaseline {
    pub baseline_id: String,
    pub display: DisplayInfo,
    pub source_bounds: DisplayBounds,
    pub fingerprint: String,
    pub expires_in_ms: u32,
    /// How many baselines the server holds, this one included.
    pub baseline_count: u32,
    /// The pixel bytes all held baselines take together, this one included.
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WaitForChangeSinceRequest {
    pub baseline_id: String,
    pub timeout_ms: u32,
    pub stable_ms: u32,
    pub difference_threshold: f64,
}

/// How a visual-change wait ended. The wait samples the region, so a change that comes and goes
/// between two polls is not seen, and a visible change does not show that an application did
/// what was asked.
#[derive(Debug, Clone, PartialEq)]
pub struct VisualChangeResult {
    /// A polled frame differed from the baseline by at least the threshold at some point,
    /// even if the region later returned to how the baseline looks.
    pub changed_once: bool,
    /// The last polled frame, the one `difference` describes, differs from the baseline by at
    /// least the threshold.
    pub currently_changed: bool,
    /// After the first change, consecutive frames stayed still for `stable_ms`.
    pub settled: bool,
    /// The last polled frame's difference from the baseline, from 0 to 1.
    pub difference: f64,
    pub elapsed_ms: u64,
    pub screenshot: DisplayScreenshot,
}

impl VisualChangeResult {
    /// The published `changed` field, kept for compatibility. It means `changed_once`.
    #[must_use]
    pub const fn changed(&self) -> bool {
        self.changed_once
    }

    /// The wait reached its deadline before the region settled.
    #[must_use]
    pub const fn timed_out(&self) -> bool {
        !self.settled
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MouseScrollRequest {
    pub display_id: String,
    pub x: u32,
    pub y: u32,
    pub delta_x: i32,
    pub delta_y: i32,
    pub duration_ms: u32,
    pub observation: ObservationOptions,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MousePosition {
    pub display_id: String,
    pub local_x: u32,
    pub local_y: u32,
    pub virtual_x: i32,
    pub virtual_y: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PointerActionResult {
    pub position: MousePosition,
    pub observation: ActionObservation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionObservation {
    /// `None` only when no window holds the foreground.
    pub foreground_window: Option<crate::ForegroundWindow>,
    pub screenshot: Option<DisplayScreenshot>,
    /// How the wait between the input and this evidence ended, for input actions.
    pub settle: Option<SettleReport>,
}

/// How an input action's wait for the approved window to settle ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettleOutcome {
    /// The window changed and then stayed unchanged; the evidence shows the response.
    Settled,
    /// The window did not visibly change in time; the evidence may predate the response.
    Unchanged,
    /// The window was still changing when the wait ended; the evidence may be mid-change.
    StillChanging,
    /// The window was not watched, because `settle_ms` was 0 or the window is unknown or too
    /// large to read quickly; a short fixed wait was used instead.
    NotWatched,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettleReport {
    pub outcome: SettleOutcome,
    pub elapsed_ms: u32,
}
