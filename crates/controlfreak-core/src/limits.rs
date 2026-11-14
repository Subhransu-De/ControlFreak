//! Request limits shared by the MCP schemas, request validation and the platform backends.
//!
//! Keeping each limit in one place stops the published schema, the request parser and the
//! backend from drifting apart.

/// Longest smooth pointer movement, in milliseconds.
pub const MAX_MOVE_DURATION_MS: u32 = 10_000;
/// Largest wheel delta in either direction. 120 is one wheel detent.
pub const MAX_SCROLL_DELTA: u32 = 12_000;
/// Longest text one `type_text` call or step accepts, in UTF-16 code units.
pub const MAX_TEXT_UTF16_UNITS: usize = 4_000;
/// Widest image an explicit capture may request.
pub const MAX_CAPTURE_WIDTH: u32 = 7_680;
/// Width of a captured image when the caller gives no `max_width`.
pub const DEFAULT_CAPTURE_WIDTH: u32 = 1_280;
/// Longest server-side wait, in milliseconds.
pub const MAX_WAIT_MS: u32 = 30_000;
/// Smallest `timeout_ms` that every wait tool accepts. Zero checks once and returns.
pub const MIN_WAIT_MS: u32 = 0;
/// Longest pause one `run_actions` wait step may request, in milliseconds.
pub const MAX_RUN_ACTIONS_WAIT_MS: u32 = 5_000;
/// Longest total pause across all wait steps of one `run_actions` call, in milliseconds.
pub const MAX_RUN_ACTIONS_TOTAL_WAIT_MS: u32 = 15_000;
