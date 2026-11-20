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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayInfo {
    pub id: String,
    pub name: String,
    pub bounds: DisplayBounds,
    pub is_primary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayScreenshot {
    pub target_ref: Option<String>,
    pub display: DisplayInfo,
    pub source_bounds: DisplayBounds,
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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WaitForChangeSinceRequest {
    pub baseline_id: String,
    pub timeout_ms: u32,
    pub stable_ms: u32,
    pub difference_threshold: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VisualChangeResult {
    pub changed: bool,
    pub timed_out: bool,
    pub difference: f64,
    pub elapsed_ms: u64,
    pub screenshot: DisplayScreenshot,
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
    pub foreground_window: Option<crate::WindowInfo>,
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
