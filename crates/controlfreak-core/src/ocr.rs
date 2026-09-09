use serde::{Deserialize, Serialize};

use crate::{
    ActionObservation, DisplayBounds, DisplayInfo, Key, MouseButton, MousePosition,
    ObservationOptions,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OcrRegionRequest {
    pub display_id: String,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub language: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OcrWord {
    pub text: String,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OcrLine {
    pub text: String,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub words: Vec<OcrWord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OcrResult {
    pub display: DisplayInfo,
    pub source_bounds: DisplayBounds,
    pub language: String,
    pub text: String,
    pub lines: Vec<OcrLine>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindTextRequest {
    pub region: OcrRegionRequest,
    pub query: String,
    pub case_sensitive: bool,
    pub max_results: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextMatch {
    pub text: String,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindTextResult {
    pub query: String,
    pub display: DisplayInfo,
    pub source_bounds: DisplayBounds,
    pub language: String,
    pub matches: Vec<TextMatch>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClickTextRequest {
    pub region: OcrRegionRequest,
    pub query: String,
    pub case_sensitive: bool,
    pub exact_match: bool,
    pub button: MouseButton,
    pub click_count: u8,
    pub modifiers: Vec<Key>,
    pub duration_ms: u32,
    pub observation: ObservationOptions,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClickTextResult {
    pub matched_text: TextMatch,
    pub position: MousePosition,
    pub observation: ActionObservation,
}
