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
/// Shortest `timeout_ms` a `type_text` call or step accepts. Zero would type nothing.
pub const MIN_TEXT_TIMEOUT_MS: u32 = 1;
/// Longest `timeout_ms` a `type_text` call or step accepts, in milliseconds. Above the wait
/// tools' limit, because the longest text takes about a minute at the paced typing rate.
pub const MAX_TEXT_TIMEOUT_MS: u32 = 300_000;
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
/// Longest wait for the window to settle after input when the action returns a screenshot and
/// the caller gives no `settle_ms`.
pub const DEFAULT_SETTLE_MS: u32 = 2_000;
/// Longest wait for the window to settle after input when the action returns no screenshot,
/// such as a `run_actions` step before the last, and the caller gives no `settle_ms`.
pub const DEFAULT_QUIET_SETTLE_MS: u32 = 500;
/// Largest `settle_ms` an action accepts.
pub const MAX_SETTLE_MS: u32 = 10_000;
/// Most windows `list_windows` returns, and so the most `list_virtual_desktops` groups.
pub const MAX_WINDOWS: usize = 200;
/// Most OCR lines one `read_text_in_region` result returns.
pub const MAX_READ_TEXT_LINES: usize = 200;
/// Most characters (Unicode scalar values) of line text one `read_text_in_region` result returns.
pub const MAX_READ_TEXT_CHARS: usize = 8_000;
/// Most bytes the serialized `lines` of one `read_text_in_region` result take. Each word repeats
/// its text with a box, so this bounds the structured result the characters do not.
pub const MAX_READ_TEXT_BYTES: usize = 128 * 1024;
