use serde::{Deserialize, Serialize};

use crate::{ActionObservation, ObservationOptions, WindowInfo, WindowList};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VirtualDesktopInfo {
    pub id: String,
    pub is_current: bool,
    pub windows: Vec<WindowInfo>,
}

/// Windows grouped by desktop. Desktops without a listed window (except the current one),
/// desktop names and desktop order are never known, so the result reports them as unavailable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VirtualDesktopList {
    pub desktops: Vec<VirtualDesktopInfo>,
    /// The window list was cut at its cap, so some windows, and possibly whole desktops, are
    /// missing.
    pub truncated: bool,
}

impl VirtualDesktopList {
    /// Groups listed windows by desktop, in desktop-id order, and carries the list's truncation.
    ///
    /// `locate` returns a window's desktop id and whether that desktop is current, or `None` to
    /// leave the window out (it closed, or the shell reports no desktop for it). When no listed
    /// window is on the current desktop, `current_id` names it, so an empty current desktop is
    /// still listed and marked.
    pub fn from_windows(
        list: WindowList,
        current_id: Option<&str>,
        mut locate: impl FnMut(&WindowInfo) -> Option<(String, bool)>,
    ) -> Self {
        let mut current_ids = std::collections::BTreeSet::<String>::new();
        let mut grouped = std::collections::BTreeMap::<String, Vec<WindowInfo>>::new();
        for window in list.windows {
            let Some((id, is_current)) = locate(&window) else {
                continue;
            };
            if is_current {
                current_ids.insert(id.clone());
            }
            grouped.entry(id).or_default().push(window);
        }
        if current_ids.is_empty()
            && let Some(current_id) = current_id
        {
            grouped.entry(current_id.to_owned()).or_default();
        }
        let desktops = grouped
            .into_iter()
            .map(|(id, windows)| VirtualDesktopInfo {
                is_current: if current_ids.is_empty() {
                    current_id == Some(id.as_str())
                } else {
                    current_ids.contains(&id)
                },
                id,
                windows,
            })
            .collect();
        Self {
            desktops,
            truncated: list.truncated,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VirtualDesktopDirection {
    Left,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VirtualDesktopSwitchRequest {
    pub direction: VirtualDesktopDirection,
    pub steps: u8,
    pub observation: ObservationOptions,
}

/// What two desktop readings, one before and one after a switch, establish about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VirtualDesktopTransition {
    ConfirmedChanged,
    ConfirmedUnchanged,
    Unknown,
}

impl VirtualDesktopTransition {
    /// Two known readings confirm the result. A reading is unknown when no window holds the
    /// foreground or the shell reports no desktop for it. After a switch to an empty desktop the
    /// shell's own desktop window holds the foreground and reads that way, so a known desktop
    /// followed by an unknown one is `ConfirmedChanged` only when `before_left` confirms it: the
    /// window that was in front before the switch is still on `before`, and that desktop is no
    /// longer current. Anything else with a missing reading is `Unknown`.
    #[must_use]
    pub fn classify(before: Option<&str>, after: Option<&str>, before_left: bool) -> Self {
        match (before, after) {
            (Some(before), Some(after)) if before == after => Self::ConfirmedUnchanged,
            (Some(_), Some(_)) => Self::ConfirmedChanged,
            (Some(_), None) if before_left => Self::ConfirmedChanged,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtualDesktopSwitchResult {
    pub direction: VirtualDesktopDirection,
    pub requested_steps: u8,
    /// The raw readings differ, kept for compatibility: it is `true` for a known desktop followed
    /// by an unknown one, and `false` for two unknown readings. Trust `transition`.
    pub changed: bool,
    /// The foreground window's desktop before the first shortcut, `None` when unknown.
    pub before_id: Option<String>,
    /// The foreground window's desktop after the last shortcut and its settle delay.
    pub after_id: Option<String>,
    pub transition: VirtualDesktopTransition,
    pub observation: ActionObservation,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WindowBounds;

    fn window(id: &str, process_id: u32) -> WindowInfo {
        WindowInfo {
            id: id.to_owned(),
            title: id.to_owned(),
            class_name: "Synthetic".to_owned(),
            process_id,
            bounds: WindowBounds {
                left: 0,
                top: 0,
                width: 10,
                height: 10,
            },
            display_id: "display".to_owned(),
            is_foreground: false,
            is_minimized: false,
            owner: None,
        }
    }

    #[test]
    fn grouping_keeps_the_window_list_truncation_and_marks_the_current_desktop() {
        let windows = (0..5)
            .map(|index| window(&format!("w{index}"), index))
            .collect();
        let list = VirtualDesktopList::from_windows(
            WindowList {
                windows,
                truncated: true,
            },
            Some("A"),
            |window| match window.id.as_str() {
                "w0" | "w1" => Some(("B".to_owned(), true)),
                "w4" => None,
                _ => Some(("C".to_owned(), false)),
            },
        );
        assert!(list.truncated);
        let summary: Vec<_> = list
            .desktops
            .iter()
            .map(|desktop| {
                (
                    desktop.id.as_str(),
                    desktop.is_current,
                    desktop.windows.len(),
                )
            })
            .collect();
        // A window on the current desktop outranks the shell's current id, so "A" is not added.
        assert_eq!(summary, [("B", true, 2), ("C", false, 2)]);
    }

    #[test]
    fn an_empty_current_desktop_is_still_listed() {
        let list = VirtualDesktopList::from_windows(
            WindowList {
                windows: vec![window("w0", 1)],
                truncated: false,
            },
            Some("A"),
            |_| Some(("B".to_owned(), false)),
        );
        assert!(!list.truncated);
        let summary: Vec<_> = list
            .desktops
            .iter()
            .map(|desktop| {
                (
                    desktop.id.as_str(),
                    desktop.is_current,
                    desktop.windows.len(),
                )
            })
            .collect();
        assert_eq!(summary, [("A", true, 0), ("B", false, 1)]);
    }

    #[test]
    fn a_missing_reading_confirms_a_change_only_when_the_start_desktop_was_left() {
        let a = Some("A");
        let b = Some("B");
        for (before, after, before_left, expected) in [
            (None, None, false, VirtualDesktopTransition::Unknown),
            (None, None, true, VirtualDesktopTransition::Unknown),
            (a, None, false, VirtualDesktopTransition::Unknown),
            // An empty destination desktop: the shell's desktop window is in front.
            (a, None, true, VirtualDesktopTransition::ConfirmedChanged),
            (None, a, false, VirtualDesktopTransition::Unknown),
            (None, a, true, VirtualDesktopTransition::Unknown),
            (a, a, false, VirtualDesktopTransition::ConfirmedUnchanged),
            (a, b, false, VirtualDesktopTransition::ConfirmedChanged),
            (a, b, true, VirtualDesktopTransition::ConfirmedChanged),
        ] {
            assert_eq!(
                VirtualDesktopTransition::classify(before, after, before_left),
                expected,
                "{before:?} -> {after:?}, before_left={before_left}"
            );
        }
    }
}
