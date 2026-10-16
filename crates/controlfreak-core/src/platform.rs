use serde::{Deserialize, Serialize};

use crate::{
    CapabilityDescriptor, CapabilityReport, CaptureDisplayRequest, CaptureRegionRequest,
    CaptureWindowRequest, ClickTextRequest, ClickTextResult, DisplayInfo, DisplayScreenshot,
    FindTextRequest, FindTextResult, FocusWindowRequest, KeyChordRequest, KeyboardActionResult,
    MouseClickRequest, MouseDragRequest, MouseMoveRequest, MouseScrollRequest, MutationControl,
    OcrRegionRequest, OcrResult, PermissionDescriptor, PlatformError, PointerActionResult,
    SecurityContext, TextInputRequest, VirtualDesktopList, VirtualDesktopSwitchRequest,
    VirtualDesktopSwitchResult, VisualBaseline, VisualBaselineRequest, VisualChangeResult,
    WaitForChangeSinceRequest, WaitForTextRequest, WaitForTextResult, WaitForVisualChangeRequest,
    WaitForWindowRequest, WindowFocusResult, WindowList, WindowScreenshot, WindowWaitResult,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Windows,
    MacOs,
    Linux,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayServer {
    Dwm,
    Quartz,
    X11,
    Wayland,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendIdentity {
    pub platform: Platform,
    pub display_server: DisplayServer,
    pub backend_id: String,
}

/// Identifies a backend and reports the capabilities it exposes.
pub trait BackendMetadata: Send + Sync + 'static {
    fn identity(&self) -> BackendIdentity;
    fn capabilities(&self) -> Vec<CapabilityDescriptor>;
    fn permissions(&self) -> Vec<PermissionDescriptor>;

    /// Validate that mutation admission is safe in the current desktop environment.
    fn check_control_environment(&self) -> Result<(), PlatformError> {
        Ok(())
    }

    fn validate_target_reference(&self, _target: &str) -> Result<(), PlatformError> {
        Err(PlatformError::unsupported(
            "approve_target",
            "the backend cannot validate target identities",
        ))
    }

    fn record_target_evidence(&self, _control: &MutationControl) {}

    fn security_context(&self) -> SecurityContext {
        SecurityContext::default()
    }

    fn report(&self) -> CapabilityReport {
        CapabilityReport::new(
            self.identity(),
            self.capabilities(),
            self.permissions(),
            self.security_context(),
        )
    }
}

/// Captures displays and tracks visual changes.
pub trait DisplayBackend {
    fn list_displays(&self) -> Result<Vec<DisplayInfo>, PlatformError> {
        Err(PlatformError::unsupported(
            "list_displays",
            "the active platform backend has no display implementation",
        ))
    }

    fn capture_display(
        &self,
        _request: &CaptureDisplayRequest,
    ) -> Result<DisplayScreenshot, PlatformError> {
        Err(PlatformError::unsupported(
            "capture_display",
            "the active platform backend has no screen-capture implementation",
        ))
    }

    fn capture_region(
        &self,
        _request: &CaptureRegionRequest,
    ) -> Result<DisplayScreenshot, PlatformError> {
        Err(PlatformError::unsupported(
            "capture_region",
            "the active platform backend has no region-capture implementation",
        ))
    }

    fn wait_for_visual_change_controlled(
        &self,
        request: &WaitForVisualChangeRequest,
        control: &MutationControl,
    ) -> Result<VisualChangeResult, PlatformError> {
        control.check("wait_for_visual_change")?;
        let result = self.wait_for_visual_change(request)?;
        control.check("wait_for_visual_change")?;
        Ok(result)
    }

    fn wait_for_visual_change(
        &self,
        _request: &WaitForVisualChangeRequest,
    ) -> Result<VisualChangeResult, PlatformError> {
        Err(PlatformError::unsupported(
            "wait_for_visual_change",
            "the active platform backend has no visual-wait implementation",
        ))
    }

    fn capture_visual_baseline(
        &self,
        _request: &VisualBaselineRequest,
    ) -> Result<VisualBaseline, PlatformError> {
        Err(PlatformError::unsupported(
            "capture_visual_baseline",
            "the active platform backend has no visual-baseline implementation",
        ))
    }

    fn wait_for_change_since_controlled(
        &self,
        request: &WaitForChangeSinceRequest,
        control: &MutationControl,
    ) -> Result<VisualChangeResult, PlatformError> {
        control.check("wait_for_change_since")?;
        let result = self.wait_for_change_since(request)?;
        control.check("wait_for_change_since")?;
        Ok(result)
    }

    fn wait_for_change_since(
        &self,
        _request: &WaitForChangeSinceRequest,
    ) -> Result<VisualChangeResult, PlatformError> {
        Err(PlatformError::unsupported(
            "wait_for_change_since",
            "the active platform backend has no visual-baseline implementation",
        ))
    }
}

/// Performs local OCR and semantic text interaction.
pub trait OcrBackend {
    fn read_text_in_region_controlled(
        &self,
        request: &OcrRegionRequest,
        control: &MutationControl,
    ) -> Result<OcrResult, PlatformError> {
        control.check("read_text_in_region")?;
        let result = self.read_text_in_region(request)?;
        control.check("read_text_in_region")?;
        Ok(result)
    }

    fn read_text_in_region(&self, _request: &OcrRegionRequest) -> Result<OcrResult, PlatformError> {
        Err(PlatformError::unsupported(
            "read_text_in_region",
            "the active platform backend has no OCR implementation",
        ))
    }

    fn find_text_on_screen_controlled(
        &self,
        request: &FindTextRequest,
        control: &MutationControl,
    ) -> Result<FindTextResult, PlatformError> {
        control.check("find_text_on_screen")?;
        let result = self.find_text_on_screen(request)?;
        control.check("find_text_on_screen")?;
        Ok(result)
    }

    /// Repeats discovery until a match appears or disappears, or the timeout elapses. Each pass is
    /// a full recognition; the final pass is returned so callers act on current text.
    fn wait_for_text_controlled(
        &self,
        request: &WaitForTextRequest,
        control: &MutationControl,
    ) -> Result<WaitForTextResult, PlatformError> {
        const POLL_GAP: std::time::Duration = std::time::Duration::from_millis(100);
        if request.timeout_ms > crate::MAX_TEXT_WAIT_MS {
            return Err(PlatformError::InvalidArgument {
                argument: "timeout_ms".to_owned(),
                reason: format!("must be at most {}", crate::MAX_TEXT_WAIT_MS),
            });
        }
        let started = std::time::Instant::now();
        let deadline = started + std::time::Duration::from_millis(u64::from(request.timeout_ms));
        let mut attempts = 0_u32;
        loop {
            let last = self.find_text_on_screen_controlled(&request.find, control)?;
            attempts = attempts.saturating_add(1);
            let visible = !last.matches.is_empty();
            let satisfied = match request.condition {
                crate::TextCondition::Appears => visible,
                crate::TextCondition::Disappears => !visible,
            };
            let now = std::time::Instant::now();
            if satisfied || now >= deadline {
                return Ok(WaitForTextResult {
                    condition: request.condition,
                    satisfied,
                    timed_out: !satisfied,
                    elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                    attempts,
                    last,
                });
            }
            control.wait(
                "wait_for_text",
                POLL_GAP.min(deadline.saturating_duration_since(now)),
            )?;
        }
    }

    fn find_text_on_screen(
        &self,
        _request: &FindTextRequest,
    ) -> Result<FindTextResult, PlatformError> {
        Err(PlatformError::unsupported(
            "find_text_on_screen",
            "the active platform backend has no OCR implementation",
        ))
    }

    fn click_text(&self, _request: &ClickTextRequest) -> Result<ClickTextResult, PlatformError> {
        Err(PlatformError::unsupported(
            "click_text",
            "the active platform backend has no OCR interaction implementation",
        ))
    }

    fn click_text_controlled(
        &self,
        request: &ClickTextRequest,
        control: &MutationControl,
    ) -> Result<ClickTextResult, PlatformError> {
        control.check("click_text")?;
        legacy_mutation(control, || self.click_text(request))
    }
}

/// Controls the system pointer.
pub trait PointerBackend {
    fn move_mouse(
        &self,
        _request: &MouseMoveRequest,
    ) -> Result<PointerActionResult, PlatformError> {
        Err(PlatformError::unsupported(
            "move_mouse",
            "the active platform backend has no pointer-movement implementation",
        ))
    }

    fn move_mouse_controlled(
        &self,
        request: &MouseMoveRequest,
        control: &MutationControl,
    ) -> Result<PointerActionResult, PlatformError> {
        control.check("move_mouse")?;
        legacy_mutation(control, || self.move_mouse(request))
    }

    fn click_mouse(
        &self,
        _request: &MouseClickRequest,
    ) -> Result<PointerActionResult, PlatformError> {
        Err(PlatformError::unsupported(
            "click_mouse",
            "the active platform backend has no pointer-click implementation",
        ))
    }

    fn click_mouse_controlled(
        &self,
        request: &MouseClickRequest,
        control: &MutationControl,
    ) -> Result<PointerActionResult, PlatformError> {
        control.check("click_mouse")?;
        legacy_mutation(control, || self.click_mouse(request))
    }

    fn drag_mouse(
        &self,
        _request: &MouseDragRequest,
    ) -> Result<PointerActionResult, PlatformError> {
        Err(PlatformError::unsupported(
            "drag_mouse",
            "the active platform backend has no pointer-drag implementation",
        ))
    }

    fn drag_mouse_controlled(
        &self,
        request: &MouseDragRequest,
        control: &MutationControl,
    ) -> Result<PointerActionResult, PlatformError> {
        control.check("drag_mouse")?;
        legacy_mutation(control, || self.drag_mouse(request))
    }

    fn scroll_mouse(
        &self,
        _request: &MouseScrollRequest,
    ) -> Result<PointerActionResult, PlatformError> {
        Err(PlatformError::unsupported(
            "scroll_mouse",
            "the active platform backend has no pointer-scroll implementation",
        ))
    }

    fn scroll_mouse_controlled(
        &self,
        request: &MouseScrollRequest,
        control: &MutationControl,
    ) -> Result<PointerActionResult, PlatformError> {
        control.check("scroll_mouse")?;
        legacy_mutation(control, || self.scroll_mouse(request))
    }
}

/// Enumerates, observes, and focuses windows and virtual desktops.
pub trait WindowBackend {
    fn list_windows(&self) -> Result<WindowList, PlatformError> {
        Err(PlatformError::unsupported(
            "list_windows",
            "the active platform backend has no window-enumeration implementation",
        ))
    }

    fn list_virtual_desktops(&self) -> Result<VirtualDesktopList, PlatformError> {
        Err(PlatformError::unsupported(
            "list_virtual_desktops",
            "the active platform backend has no virtual-desktop discovery implementation",
        ))
    }

    fn switch_virtual_desktop(
        &self,
        _request: &VirtualDesktopSwitchRequest,
    ) -> Result<VirtualDesktopSwitchResult, PlatformError> {
        Err(PlatformError::unsupported(
            "switch_virtual_desktop",
            "the active platform backend has no virtual-desktop switching implementation",
        ))
    }

    fn switch_virtual_desktop_controlled(
        &self,
        request: &VirtualDesktopSwitchRequest,
        control: &MutationControl,
    ) -> Result<VirtualDesktopSwitchResult, PlatformError> {
        control.check("switch_virtual_desktop")?;
        legacy_mutation(control, || self.switch_virtual_desktop(request))
    }

    fn focus_window(
        &self,
        _request: &FocusWindowRequest,
    ) -> Result<WindowFocusResult, PlatformError> {
        Err(PlatformError::unsupported(
            "focus_window",
            "the active platform backend has no window-focus implementation",
        ))
    }

    fn focus_window_controlled(
        &self,
        request: &FocusWindowRequest,
        control: &MutationControl,
    ) -> Result<WindowFocusResult, PlatformError> {
        control.check("focus_window")?;
        legacy_mutation(control, || self.focus_window(request))
    }

    fn capture_window(
        &self,
        _request: &CaptureWindowRequest,
    ) -> Result<WindowScreenshot, PlatformError> {
        Err(PlatformError::unsupported(
            "capture_window",
            "the active platform backend has no window-capture implementation",
        ))
    }

    fn wait_for_window_controlled(
        &self,
        request: &WaitForWindowRequest,
        control: &MutationControl,
    ) -> Result<WindowWaitResult, PlatformError> {
        control.check("wait_for_window")?;
        let result = self.wait_for_window(request)?;
        control.check("wait_for_window")?;
        Ok(result)
    }

    fn wait_for_window(
        &self,
        _request: &WaitForWindowRequest,
    ) -> Result<WindowWaitResult, PlatformError> {
        Err(PlatformError::unsupported(
            "wait_for_window",
            "the active platform backend has no window-wait implementation",
        ))
    }
}

/// Sends keyboard shortcuts and Unicode text.
pub trait KeyboardBackend {
    fn press_keys(
        &self,
        _request: &KeyChordRequest,
    ) -> Result<KeyboardActionResult, PlatformError> {
        Err(PlatformError::unsupported(
            "press_keys",
            "the active platform backend has no keyboard implementation",
        ))
    }

    fn press_keys_controlled(
        &self,
        request: &KeyChordRequest,
        control: &MutationControl,
    ) -> Result<KeyboardActionResult, PlatformError> {
        control.check("press_keys")?;
        legacy_mutation(control, || self.press_keys(request))
    }

    fn type_text(
        &self,
        _request: &TextInputRequest,
    ) -> Result<KeyboardActionResult, PlatformError> {
        Err(PlatformError::unsupported(
            "type_text",
            "the active platform backend has no text-input implementation",
        ))
    }

    fn type_text_controlled(
        &self,
        request: &TextInputRequest,
        control: &MutationControl,
    ) -> Result<KeyboardActionResult, PlatformError> {
        control.check("type_text")?;
        legacy_mutation(control, || self.type_text(request))
    }
}

/// Complete backend contract consumed by the MCP adapter.
pub trait PlatformBackend:
    BackendMetadata + DisplayBackend + OcrBackend + PointerBackend + WindowBackend + KeyboardBackend
{
}

impl<T> PlatformBackend for T where
    T: BackendMetadata
        + DisplayBackend
        + OcrBackend
        + PointerBackend
        + WindowBackend
        + KeyboardBackend
{
}

// Legacy implementations cannot expose dispatch progress. Treat their failures as uncertain,
// except explicit unsupported responses, which promise that no action was available.
fn legacy_mutation<T>(
    control: &MutationControl,
    operation: impl FnOnce() -> Result<T, PlatformError>,
) -> Result<T, PlatformError> {
    let previous_cleanup = control.progress().cleanup;
    let previous = control.dispatch_started();
    // Legacy implementations cannot acknowledge release cleanup, including during unwinding.
    control.cleanup_status(crate::CleanupStatus::Unknown);
    let result = operation();
    let unsupported = matches!(result, Err(PlatformError::Unsupported { .. }));
    if unsupported {
        control.dispatch_rejected(previous);
    }
    if result.is_ok() || unsupported {
        control.cleanup_status(previous_cleanup);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CleanupStatus, InputOutcome};

    #[test]
    fn legacy_results_preserve_only_known_delivery_and_cleanup() {
        for (result, expected_input, expected_cleanup) in [
            (Ok(()), InputOutcome::Unknown, CleanupStatus::NotNeeded),
            (
                Err(PlatformError::unsupported("synthetic", "unsupported")),
                InputOutcome::NotStarted,
                CleanupStatus::NotNeeded,
            ),
            (
                Err(PlatformError::Unavailable {
                    reason: "provider disconnected".into(),
                }),
                InputOutcome::Unknown,
                CleanupStatus::Unknown,
            ),
        ] {
            let control = MutationControl::default();
            let expected_result = result.clone();
            assert_eq!(legacy_mutation(&control, || result), expected_result);
            assert_eq!(control.progress().input_outcome, expected_input);
            assert_eq!(control.progress().cleanup, expected_cleanup);
        }
    }

    #[test]
    fn legacy_panic_leaves_delivery_and_cleanup_unknown() {
        let control = MutationControl::default();
        let result = std::panic::catch_unwind(|| {
            legacy_mutation::<()>(&control, || panic!("injected provider panic"))
        });
        assert!(result.is_err());
        assert_eq!(control.progress().input_outcome, InputOutcome::Unknown);
        assert_eq!(control.progress().cleanup, CleanupStatus::Unknown);
    }

    /// Reports the query as visible on passes `visible_from..visible_until`.
    struct ScriptedText {
        passes: std::sync::atomic::AtomicU32,
        visible: std::ops::Range<u32>,
    }

    impl OcrBackend for ScriptedText {
        fn find_text_on_screen_controlled(
            &self,
            request: &crate::FindTextRequest,
            _: &MutationControl,
        ) -> Result<crate::FindTextResult, PlatformError> {
            let pass = self
                .passes
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let bounds = crate::DisplayBounds {
                left: 0,
                top: 0,
                width: 10,
                height: 10,
            };
            let matches = if self.visible.contains(&pass) {
                vec![crate::TextMatch {
                    text: request.query.clone(),
                    x: 1,
                    y: 2,
                    width: 3,
                    height: 4,
                }]
            } else {
                Vec::new()
            };
            Ok(crate::FindTextResult {
                target_ref: None,
                query: request.query.clone(),
                display: crate::DisplayInfo {
                    id: "fixture".into(),
                    name: "fixture".into(),
                    bounds,
                    is_primary: true,
                },
                source_bounds: bounds,
                language: "en-US".into(),
                details: crate::OcrMatchDetails {
                    candidates: matches.clone(),
                    candidate_count: matches.len(),
                    candidates_truncated: false,
                    text_truncated: false,
                    match_tier: None,
                    recovery: String::new(),
                },
                matches,
            })
        }
    }

    fn wait_request(condition: crate::TextCondition, timeout_ms: u32) -> crate::WaitForTextRequest {
        crate::WaitForTextRequest {
            find: crate::FindTextRequest {
                region: OcrRegionRequest {
                    display_id: "fixture".into(),
                    x: 0,
                    y: 0,
                    width: 10,
                    height: 10,
                    language: None,
                },
                query: "Saved".into(),
                case_sensitive: false,
                max_results: 20,
                match_mode: crate::TextMatchMode::default(),
                ocr_confusions: false,
            },
            condition,
            timeout_ms,
        }
    }

    #[test]
    fn text_waits_repeat_recognition_until_the_condition_holds() {
        let control = MutationControl::default();
        let appearing = ScriptedText {
            passes: 0.into(),
            visible: 2..u32::MAX,
        };
        let result = appearing
            .wait_for_text_controlled(
                &wait_request(crate::TextCondition::Appears, 5_000),
                &control,
            )
            .unwrap();
        assert!(result.satisfied && !result.timed_out);
        assert_eq!(result.attempts, 3);
        assert_eq!(result.last.matches.len(), 1);

        let disappearing = ScriptedText {
            passes: 0.into(),
            visible: 0..2,
        };
        let result = disappearing
            .wait_for_text_controlled(
                &wait_request(crate::TextCondition::Disappears, 5_000),
                &control,
            )
            .unwrap();
        assert!(result.satisfied);
        assert_eq!(result.attempts, 3);
        assert!(result.last.matches.is_empty());
    }

    #[test]
    fn text_waits_time_out_with_the_final_pass_and_reject_long_timeouts() {
        let control = MutationControl::default();
        let never = ScriptedText {
            passes: 0.into(),
            visible: 0..0,
        };
        let result = never
            .wait_for_text_controlled(&wait_request(crate::TextCondition::Appears, 0), &control)
            .unwrap();
        assert!(!result.satisfied && result.timed_out);
        assert_eq!(result.attempts, 1);
        assert!(matches!(
            never.wait_for_text_controlled(
                &wait_request(crate::TextCondition::Appears, crate::MAX_TEXT_WAIT_MS + 1),
                &control,
            ),
            Err(PlatformError::InvalidArgument { .. })
        ));
        control.cancel();
        assert!(
            never
                .wait_for_text_controlled(
                    &wait_request(crate::TextCondition::Appears, 5_000),
                    &control
                )
                .is_err()
        );
    }
}
