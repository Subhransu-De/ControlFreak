use serde::{Deserialize, Serialize};

use crate::{ActionObservation, ObservationOptions, WindowInfo};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VirtualDesktopInfo {
    pub id: String,
    pub is_current: bool,
    pub windows: Vec<WindowInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VirtualDesktopList {
    pub desktops: Vec<VirtualDesktopInfo>,
    pub includes_empty_desktops: bool,
    pub order_available: bool,
    pub names_available: bool,
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
    /// Only two known readings confirm anything. A reading is unknown when no window holds the
    /// foreground or the shell reports no desktop for it, and an empty destination desktop may
    /// read that way after a successful switch, so a missing reading on either side is `Unknown`.
    #[must_use]
    pub fn classify(before: Option<&str>, after: Option<&str>) -> Self {
        match (before, after) {
            (Some(before), Some(after)) if before == after => Self::ConfirmedUnchanged,
            (Some(_), Some(_)) => Self::ConfirmedChanged,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtualDesktopSwitchResult {
    pub direction: VirtualDesktopDirection,
    pub requested_steps: u8,
    /// `before_id != after_id`, kept for compatibility: it is `true` for a known desktop
    /// followed by an unknown one, and `false` for two unknown readings. Trust `transition`.
    pub changed: bool,
    /// The foreground window's desktop, read with the input lock held, before the first shortcut.
    pub before_id: Option<String>,
    /// The foreground window's desktop after the last shortcut and its settle delay.
    pub after_id: Option<String>,
    pub transition: VirtualDesktopTransition,
    pub observation: ActionObservation,
}

impl VirtualDesktopSwitchResult {
    /// Derives `changed` and `transition` from the same two readings.
    #[must_use]
    pub fn from_readings(
        request: &VirtualDesktopSwitchRequest,
        before_id: Option<String>,
        after_id: Option<String>,
        observation: ActionObservation,
    ) -> Self {
        Self {
            direction: request.direction,
            requested_steps: request.steps,
            changed: before_id != after_id,
            transition: VirtualDesktopTransition::classify(
                before_id.as_deref(),
                after_id.as_deref(),
            ),
            before_id,
            after_id,
            observation,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_two_known_readings_confirm_a_transition() {
        let a = Some("A");
        let b = Some("B");
        for (before, after, expected) in [
            (None, None, VirtualDesktopTransition::Unknown),
            (a, None, VirtualDesktopTransition::Unknown),
            (None, a, VirtualDesktopTransition::Unknown),
            (a, a, VirtualDesktopTransition::ConfirmedUnchanged),
            (a, b, VirtualDesktopTransition::ConfirmedChanged),
        ] {
            assert_eq!(
                VirtualDesktopTransition::classify(before, after),
                expected,
                "{before:?} -> {after:?}"
            );
        }
    }

    #[test]
    fn changed_keeps_its_raw_comparison_beside_the_transition() {
        let request = VirtualDesktopSwitchRequest {
            direction: VirtualDesktopDirection::Right,
            steps: 2,
            observation: ObservationOptions::default(),
        };
        let observation = ActionObservation {
            foreground_window: None,
            screenshot: None,
            settle: None,
        };
        let known = || Some("A".to_owned());
        let result = |before, after| {
            let result = VirtualDesktopSwitchResult::from_readings(
                &request,
                before,
                after,
                observation.clone(),
            );
            (result.changed, result.transition)
        };
        // A known desktop followed by none still reads as changed, but is not confirmed.
        assert_eq!(
            result(known(), None),
            (true, VirtualDesktopTransition::Unknown)
        );
        assert_eq!(
            result(None, None),
            (false, VirtualDesktopTransition::Unknown)
        );
        assert_eq!(
            result(known(), known()),
            (false, VirtualDesktopTransition::ConfirmedUnchanged)
        );
        let switched = VirtualDesktopSwitchResult::from_readings(
            &request,
            known(),
            Some("B".to_owned()),
            observation,
        );
        assert_eq!(switched.direction, VirtualDesktopDirection::Right);
        assert_eq!(switched.requested_steps, 2);
        assert_eq!(switched.before_id.as_deref(), Some("A"));
        assert_eq!(switched.after_id.as_deref(), Some("B"));
        assert_eq!(
            switched.transition,
            VirtualDesktopTransition::ConfirmedChanged
        );
    }
}
