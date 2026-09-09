use serde::{Deserialize, Serialize};

use crate::{ActionObservation, ObservationOptions};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowBounds {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowInfo {
    pub id: String,
    pub title: String,
    pub class_name: String,
    pub process_id: u32,
    pub bounds: WindowBounds,
    pub display_id: String,
    pub is_foreground: bool,
    pub is_minimized: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowFocusResult {
    pub window: WindowInfo,
    pub observation: ActionObservation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FocusWindowRequest {
    pub window_id: String,
    pub observation: ObservationOptions,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureWindowRequest {
    pub window_id: String,
    pub max_width: Option<u32>,
    pub include_cursor: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowScreenshot {
    pub window: WindowInfo,
    pub source_bounds: WindowBounds,
    pub png: Vec<u8>,
    pub image_width: u32,
    pub image_height: u32,
    pub downscale_factor: u32,
    pub cursor_marker: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitForWindowRequest {
    pub window_id: Option<String>,
    pub title_contains: Option<String>,
    pub class_name: Option<String>,
    pub process_id: Option<u32>,
    pub is_foreground: Option<bool>,
    pub timeout_ms: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowWaitResult {
    pub matched: bool,
    pub timed_out: bool,
    pub elapsed_ms: u64,
    pub window: Option<WindowInfo>,
}
