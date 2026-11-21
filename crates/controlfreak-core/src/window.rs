use serde::{Deserialize, Serialize};

use crate::{ActionObservation, DisplayLayout, ObservationOptions};

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
    /// Target reference of the window that owns this one, set only for an owned window such as a
    /// dialog. Absent for ordinary top-level windows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
}

/// The window in the foreground when an action read its evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForegroundWindow {
    /// A window the server issued a target reference for, described as `list_windows` does.
    Listed(WindowInfo),
    /// A window the server cannot issue a reference for (untitled, a tool window or cloaked), such
    /// as a popup menu. Only its class and process are reported, never its title.
    Unlisted(UnlistedWindow),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnlistedWindow {
    pub class_name: String,
    pub process_id: u32,
    /// Whether the window belongs to the process of the action's approved window; `None` when the
    /// approved window is no longer known. The same process does not by itself mean that input
    /// is accepted there: `input.target_remained_foreground` reports that.
    pub same_process_as_target: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowFocusResult {
    pub window: WindowInfo,
    pub observation: ActionObservation,
    pub attempts: u32,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FocusWindowRequest {
    pub window_id: String,
    pub observation: ObservationOptions,
    pub timeout_ms: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureWindowRequest {
    pub window_id: String,
    pub max_width: Option<u32>,
    pub include_cursor: bool,
    /// Also capture the window's own visible menus, drop-down lists and untitled popups.
    #[serde(default)]
    pub include_popups: bool,
}

/// A popup of a captured window's application: a menu, a drop-down list or an untitled popup.
/// Popups have no target reference and their titles are never reported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PopupWindow {
    pub class_name: String,
    pub process_id: u32,
    pub bounds: WindowBounds,
    /// Tells this popup from a later one of the same class, process and bounds, such as another
    /// menu opened in the same place. Opaque and backend-defined; never reported to clients.
    #[serde(skip)]
    pub instance: u64,
}

/// A popup shown in a window screenshot, with how its pixels were produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncludedWindow {
    pub window: PopupWindow,
    /// `windows_print_window` when the popup rendered itself, `windows_gdi` when its screen
    /// rectangle was copied.
    pub capture_method: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowScreenshot {
    pub window: WindowInfo,
    pub source_bounds: WindowBounds,
    /// The display layout the window was captured on, read after the pixels were.
    pub display_layout: DisplayLayout,
    pub png: Vec<u8>,
    pub image_width: u32,
    pub image_height: u32,
    pub cursor_marker: bool,
    /// How the pixels were produced, for example `windows_print_window` when the window rendered
    /// itself or `windows_gdi` when its screen rectangle was copied.
    pub capture_method: &'static str,
    /// The popups in the image, topmost last, when the request included popups; `None` otherwise.
    /// `source_bounds` then covers the window and these popups.
    pub included_windows: Option<Vec<IncludedWindow>>,
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

impl WaitForWindowRequest {
    /// Whether `window` meets every criterion except `window_id`, which names a handle and is
    /// resolved by the backend.
    #[must_use]
    pub fn matches(&self, window: &WindowInfo) -> bool {
        self.title_contains
            .as_ref()
            .is_none_or(|title| window.title.to_lowercase().contains(&title.to_lowercase()))
            && self
                .class_name
                .as_ref()
                .is_none_or(|class| window.class_name.eq_ignore_ascii_case(class))
            && self
                .process_id
                .is_none_or(|process_id| window.process_id == process_id)
            && self
                .is_foreground
                .is_none_or(|foreground| window.is_foreground == foreground)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowWaitResult {
    /// One window matched and is `window`.
    pub matched: bool,
    pub timed_out: bool,
    pub elapsed_ms: u64,
    pub window: Option<WindowInfo>,
    /// How many windows matched on the last check.
    pub match_count: usize,
    /// When more than one window matched: the first of them, at most
    /// [`crate::limits::MAX_WINDOW_WAIT_CANDIDATES`], and none is selected. Empty otherwise.
    pub candidates: Vec<WindowInfo>,
}

impl WindowWaitResult {
    /// The result of a check that found `match_count` matching windows, of which `issued` are the
    /// first ones with a reference, in order. `None` when nothing matched, so the wait goes on.
    /// More than one match selects none: the caller picks a candidate.
    #[must_use]
    pub fn from_matches(
        mut issued: Vec<WindowInfo>,
        match_count: usize,
        elapsed_ms: u64,
    ) -> Option<Self> {
        let match_count = match_count.max(issued.len());
        match match_count {
            0 => None,
            1 => Some(Self {
                matched: true,
                timed_out: false,
                elapsed_ms,
                window: Some(issued.pop()?),
                match_count,
                candidates: Vec::new(),
            }),
            _ => {
                issued.truncate(crate::limits::MAX_WINDOW_WAIT_CANDIDATES);
                Some(Self {
                    matched: false,
                    timed_out: false,
                    elapsed_ms,
                    window: None,
                    match_count,
                    candidates: issued,
                })
            }
        }
    }

    /// No window matched before the deadline.
    #[must_use]
    pub const fn timed_out(elapsed_ms: u64) -> Self {
        Self {
            matched: false,
            timed_out: true,
            elapsed_ms,
            window: None,
            match_count: 0,
            candidates: Vec::new(),
        }
    }
}

/// Visible windows in foreground-then-title order, capped by the backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowList {
    pub windows: Vec<WindowInfo>,
    /// More eligible windows existed than the backend returned.
    pub truncated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(id: &str) -> WindowInfo {
        WindowInfo {
            id: id.to_owned(),
            title: "Untitled - Notepad".to_owned(),
            class_name: "Notepad".to_owned(),
            process_id: 7,
            bounds: WindowBounds {
                left: 0,
                top: 0,
                width: 10,
                height: 10,
            },
            display_id: "d".to_owned(),
            is_foreground: false,
            is_minimized: false,
            owner: None,
        }
    }

    #[test]
    fn one_match_is_selected_and_several_select_none() {
        assert_eq!(WindowWaitResult::from_matches(Vec::new(), 0, 1), None);

        let single = WindowWaitResult::from_matches(vec![window("a")], 1, 1).unwrap();
        assert!(single.matched && !single.timed_out);
        assert_eq!(single.window, Some(window("a")));
        assert_eq!(single.match_count, 1);
        assert!(single.candidates.is_empty());

        let two = WindowWaitResult::from_matches(vec![window("a"), window("b")], 2, 1).unwrap();
        assert!(!two.matched && !two.timed_out);
        assert_eq!(two.window, None);
        assert_eq!(two.match_count, 2);
        assert_eq!(two.candidates, vec![window("a"), window("b")]);
    }

    #[test]
    fn ambiguous_candidates_are_bounded_but_the_count_is_not() {
        let many: Vec<_> = (0..30).map(|index| window(&index.to_string())).collect();
        let result = WindowWaitResult::from_matches(many, 45, 1).unwrap();
        assert_eq!(result.match_count, 45);
        assert_eq!(
            result.candidates.len(),
            crate::limits::MAX_WINDOW_WAIT_CANDIDATES
        );
    }
}
