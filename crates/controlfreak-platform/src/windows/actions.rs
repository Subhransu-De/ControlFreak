use super::capture::SettleWatch;
use super::input::{BUTTON_DWELL, Placement};
use super::settle::SettlePolicy;
use super::{
    Backend, Duration, HWND, KeyChordRequest, KeyboardActionResult, KeyboardBackend,
    MAX_SCROLL_DELTA, MAX_TEXT_UTF16_UNITS, MouseClickRequest, MouseDragRequest, MouseMoveRequest,
    MousePosition, MouseScrollRequest, MutationControl, ObservationOptions, POINT,
    POST_FOCUS_SETTLE_MS, POST_INPUT_SETTLE_MS, PlatformError, PointerActionResult,
    PointerActionSpec, PointerBackend, TextInputRequest, current_cursor_position,
    ensure_dpi_awareness, ensure_interactive_input_desktop, find_display_at_point,
    foreground_window_handle, move_cursor, observe_display, observe_foreground, post_action_error,
    privilege, send_click, send_drag_press, send_drag_release, send_key_chord, send_scroll,
    send_unicode_text, validate_click_request, validate_duration, validate_key_chord,
    validate_modifiers, validate_observation, window_info,
};
use controlfreak_core::{ElementActionRequest, ElementActionResult, SettleOutcome, SettleReport};

impl PointerBackend for Backend {
    fn move_mouse_controlled(
        &self,
        request: &MouseMoveRequest,
        control: &MutationControl,
    ) -> Result<PointerActionResult, PlatformError> {
        self.perform_pointer_action(
            "move_mouse",
            PointerActionSpec {
                display_id: &request.display_id,
                x: request.x,
                y: request.y,
                duration_ms: request.duration_ms,
                observation: &request.observation,
                input_generation: None,
            },
            control,
            None::<fn(&MutationControl, POINT) -> Result<SettleReport, PlatformError>>,
        )
    }

    fn click_mouse_controlled(
        &self,
        request: &MouseClickRequest,
        control: &MutationControl,
    ) -> Result<PointerActionResult, PlatformError> {
        self.click_mouse_at_generation(request, control, None)
    }

    fn drag_mouse_controlled(
        &self,
        request: &MouseDragRequest,
        control: &MutationControl,
    ) -> Result<PointerActionResult, PlatformError> {
        ensure_dpi_awareness()?;
        validate_duration(request.duration_ms)?;
        validate_observation(&request.observation)?;
        validate_modifiers(&request.modifiers)?;
        super::target::validate(control, "drag_mouse", None)?;
        let start_display = super::target::display(control, &request.start_display_id)?;
        let end_display = super::target::display(control, &request.end_display_id)?;
        let (start_x, start_y) = start_display
            .bounds
            .to_virtual(request.start_x, request.start_y)?;
        let (end_x, end_y) = end_display
            .bounds
            .to_virtual(request.end_x, request.end_y)?;
        let start = POINT {
            x: start_x,
            y: start_y,
        };
        let end = POINT { x: end_x, y: end_y };
        let _guard = self.lock_input(control)?;
        let foreground_before = foreground_window_handle();
        ensure_interactive_input_desktop("drag_mouse")?;
        control.check("drag_mouse")?;
        move_cursor(
            "drag_mouse",
            current_cursor_position()?,
            start,
            0,
            Placement::Verified,
            control,
            |point| {
                super::target::validate(control, "drag_mouse", Some(point))?;
                super::input::ensure_pointer_idle("drag_mouse")?;
                // Only before the press: the travel below holds the drag's own modifiers.
                super::input::ensure_no_modifier_held("drag_mouse")
            },
        )?;
        control.check("drag_mouse")?;
        let watch = settle_watch(control, &request.observation);
        send_drag_press(request.button, &request.modifiers, control, || {
            ensure_pointer_target(control, "drag_mouse", start)
        })?;
        control.cleanup_status(controlfreak_core::CleanupStatus::Unknown);
        // The button is held from here to the release below; a failure or cancellation in the
        // dwell or the travel is reported after the release cleanup has run. The release always
        // happens where the pointer is, so a travel that Windows stopped short still lets go.
        let movement = control
            .wait("drag_mouse", BUTTON_DWELL)
            .and_then(|()| {
                move_cursor(
                    "drag_mouse",
                    start,
                    end,
                    effective_drag_duration_ms(request.duration_ms),
                    Placement::Verified,
                    control,
                    |point| super::target::validate(control, "drag_mouse", Some(point)),
                )
            })
            .and_then(|()| control.wait("drag_mouse", BUTTON_DWELL));
        let release = send_drag_release(
            request.button,
            &request.modifiers,
            control,
            movement.is_err(),
            || ensure_pointer_target(control, "drag_mouse", end),
        );
        movement?;
        release?;
        let settled = settle(
            "drag_mouse",
            watch,
            &request.observation,
            POST_INPUT_SETTLE_MS,
            control,
        )?;
        super::target::evidence(control);
        Self::pointer_action_result(
            "drag_mouse",
            foreground_before,
            &request.observation,
            settled,
            control,
        )
    }

    fn scroll_mouse_controlled(
        &self,
        request: &MouseScrollRequest,
        control: &MutationControl,
    ) -> Result<PointerActionResult, PlatformError> {
        if request.delta_x == 0 && request.delta_y == 0 {
            return Err(PlatformError::InvalidArgument {
                argument: "delta_x/delta_y".to_owned(),
                reason: "at least one scroll delta must be nonzero".to_owned(),
            });
        }
        if request.delta_x.unsigned_abs() > MAX_SCROLL_DELTA
            || request.delta_y.unsigned_abs() > MAX_SCROLL_DELTA
        {
            return Err(PlatformError::InvalidArgument {
                argument: "delta_x/delta_y".to_owned(),
                reason: format!(
                    "each scroll delta must be between -{MAX_SCROLL_DELTA} and {MAX_SCROLL_DELTA}"
                ),
            });
        }

        self.perform_pointer_action(
            "scroll_mouse",
            PointerActionSpec {
                display_id: &request.display_id,
                x: request.x,
                y: request.y,
                duration_ms: request.duration_ms,
                observation: &request.observation,
                input_generation: None,
            },
            control,
            Some(|control: &MutationControl, target| {
                control.check("scroll_mouse")?;
                let watch = settle_watch(control, &request.observation);
                send_scroll(request.delta_x, request.delta_y, control, || {
                    ensure_pointer_target(control, "scroll_mouse", target)
                })?;
                settle(
                    "scroll_mouse",
                    watch,
                    &request.observation,
                    POST_INPUT_SETTLE_MS,
                    control,
                )
            }),
        )
    }
}

impl KeyboardBackend for Backend {
    fn press_keys_controlled(
        &self,
        request: &KeyChordRequest,
        control: &MutationControl,
    ) -> Result<KeyboardActionResult, PlatformError> {
        ensure_dpi_awareness()?;
        validate_key_chord(request)?;
        validate_observation(&request.observation)?;
        let _guard = self.lock_input(control)?;
        super::target::validate(control, "press_keys", None)?;
        let watch = settle_watch(control, &request.observation);
        send_key_chord("press_keys", &request.keys, control, || {
            super::target::validate(control, "press_keys", None)
        })?;
        let settled = settle(
            "press_keys",
            watch,
            &request.observation,
            POST_FOCUS_SETTLE_MS,
            control,
        )?;
        super::target::evidence(control);
        let mut observation = observe_foreground(&request.observation, control)
            .map_err(|error| post_action_error("press_keys", error.to_string()))?;
        observation.settle = Some(settled);
        Ok(KeyboardActionResult { observation })
    }

    fn type_text_controlled(
        &self,
        request: &TextInputRequest,
        control: &MutationControl,
    ) -> Result<KeyboardActionResult, PlatformError> {
        ensure_dpi_awareness()?;
        validate_observation(&request.observation)?;
        let utf16: Vec<u16> = request.text.encode_utf16().collect();
        if utf16.is_empty() {
            return Err(PlatformError::InvalidArgument {
                argument: "text".to_owned(),
                reason: "must not be empty".to_owned(),
            });
        }
        if utf16.len() > MAX_TEXT_UTF16_UNITS {
            return Err(PlatformError::InvalidArgument {
                argument: "text".to_owned(),
                reason: format!("must contain at most {MAX_TEXT_UTF16_UNITS} UTF-16 code units"),
            });
        }
        controlfreak_core::validate_text_timeout(request.timeout_ms)?;
        let timeout = request
            .timeout_ms
            .map(|timeout_ms| Duration::from_millis(timeout_ms.into()));

        let _guard = self.lock_input(control)?;
        super::target::validate(control, "type_text", None)?;
        let watch = settle_watch(control, &request.observation);
        // The deadline starts here, after the input lock and the first target check, so it
        // bounds the typing itself; the settle wait and screenshot after it have their own limits.
        send_unicode_text(&utf16, timeout, control, || {
            super::target::validate(control, "type_text", None)
        })?;
        let settled = settle(
            "type_text",
            watch,
            &request.observation,
            POST_FOCUS_SETTLE_MS,
            control,
        )?;
        super::target::evidence(control);
        let mut observation = observe_foreground(&request.observation, control)
            .map_err(|error| post_action_error("type_text", error.to_string()))?;
        observation.settle = Some(settled);
        Ok(KeyboardActionResult { observation })
    }
}

/// Holds the input lock; releasing it records that input may have changed the desktop.
pub(super) struct InputGuard<'a> {
    _lock: std::sync::MutexGuard<'a, ()>,
    generation: &'a std::sync::atomic::AtomicU64,
    /// Lets target validation reuse identity and integrity results while this lock is held.
    _validation: super::target::ValidationScope,
}

impl Drop for InputGuard<'_> {
    fn drop(&mut self) {
        self.generation
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }
}

impl Backend {
    pub(super) fn click_mouse_at_generation(
        &self,
        request: &MouseClickRequest,
        control: &MutationControl,
        input_generation: Option<u64>,
    ) -> Result<PointerActionResult, PlatformError> {
        validate_click_request(request)?;
        self.perform_pointer_action(
            "click_mouse",
            PointerActionSpec {
                display_id: &request.display_id,
                x: request.x,
                y: request.y,
                duration_ms: request.duration_ms,
                observation: &request.observation,
                input_generation,
            },
            control,
            Some(|control: &MutationControl, target| {
                control.check("click_mouse")?;
                let watch = settle_watch(control, &request.observation);
                send_click(
                    request.button,
                    request.click_count,
                    &request.modifiers,
                    control,
                    || ensure_pointer_target(control, "click_mouse", target),
                )?;
                settle(
                    "click_mouse",
                    watch,
                    &request.observation,
                    POST_INPUT_SETTLE_MS,
                    control,
                )
            }),
        )
    }

    /// A semantic action on an element of the session's window, under the input lock like any
    /// input: the target is checked, the window is watched to settle, the helper acts, and the
    /// foreground evidence is read as after keyboard input.
    pub(super) fn perform_element_action(
        &self,
        request: &ElementActionRequest,
        control: &MutationControl,
        environment: &impl super::uia::ActionEnvironment,
    ) -> Result<ElementActionResult, PlatformError> {
        const OPERATION: &str = "act_on_element";
        ensure_dpi_awareness()?;
        validate_observation(&request.observation)?;
        request.validate()?;
        let _guard = self.lock_input(control)?;
        // Checked here so a refused action reads no settle baseline, and again by the action
        // itself right before the request goes to the helper.
        environment.validate(control)?;
        let watch = settle_watch(control, &request.observation);
        super::uia::act_on_element(request, control, environment, &self.element_refs)?;
        let settled = settle(
            OPERATION,
            watch,
            &request.observation,
            POST_INPUT_SETTLE_MS,
            control,
        )?;
        super::target::evidence(control);
        let mut observation = observe_foreground(&request.observation, control)
            .map_err(|error| post_action_error(OPERATION, error.to_string()))?;
        observation.settle = Some(settled);
        Ok(ElementActionResult { observation })
    }

    pub(super) fn lock_input(
        &self,
        control: &MutationControl,
    ) -> Result<InputGuard<'_>, PlatformError> {
        let _timer = controlfreak_core::timing::start("input_queue");
        loop {
            control.check("input_queue")?;
            let lock = match self.input_lock.try_lock() {
                Ok(guard) => guard,
                Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => {
                    control.wait("input_queue", Duration::from_millis(10))?;
                    continue;
                }
            };
            return Ok(InputGuard {
                _lock: lock,
                generation: &self.input_generation,
                _validation: super::target::ValidationScope::enter(),
            });
        }
    }

    pub(super) fn ensure_window_input_target(
        operation: &str,
        hwnd: HWND,
    ) -> Result<(), PlatformError> {
        ensure_interactive_input_desktop(operation)?;
        privilege::ensure_window_integrity(operation, hwnd)
    }

    fn perform_pointer_action<F>(
        &self,
        operation: &str,
        spec: PointerActionSpec<'_>,
        control: &MutationControl,
        action: Option<F>,
    ) -> Result<PointerActionResult, PlatformError>
    where
        F: FnOnce(&MutationControl, POINT) -> Result<SettleReport, PlatformError>,
    {
        ensure_dpi_awareness()?;
        validate_duration(spec.duration_ms)?;
        validate_observation(spec.observation)?;

        super::target::validate(control, operation, None)?;
        let display = super::target::display(control, spec.display_id)?;
        let (target_x, target_y) = display.bounds.to_virtual(spec.x, spec.y)?;
        let _guard = self.lock_input(control)?;
        ensure_input_generation(
            operation,
            spec.input_generation,
            self.input_generation
                .load(std::sync::atomic::Ordering::Acquire),
        )?;
        ensure_interactive_input_desktop(operation)?;
        control.check(operation)?;
        let foreground_before = foreground_window_handle();

        let start = current_cursor_position()?;
        // Hover effects are a move's response. A click or scroll is watched from its own
        // dispatch, so the hover effects of its approach are not taken for its response.
        let move_watch = if action.is_none() {
            settle_watch(control, spec.observation)
        } else {
            None
        };

        let requested = POINT {
            x: target_x,
            y: target_y,
        };
        let presses = action.is_some();
        // A plain move reports wherever the pointer ended up. A click or scroll fires at the
        // live pointer, so its approach must end exactly on the requested point.
        let placement = if presses {
            Placement::Verified
        } else {
            Placement::Unchecked
        };
        move_cursor(
            operation,
            start,
            requested,
            spec.duration_ms,
            placement,
            control,
            |point| {
                super::target::validate(control, operation, Some(point))?;
                super::input::ensure_pointer_idle(operation)?;
                // A click or scroll under a held modifier is refused before the pointer moves;
                // the check before its dispatch covers a modifier pressed during the approach.
                if presses {
                    super::input::ensure_no_modifier_held(operation)
                } else {
                    Ok(())
                }
            },
        )?;
        let settled = if let Some(action) = action {
            action(control, requested)?
        } else {
            settle(
                operation,
                move_watch,
                spec.observation,
                POST_INPUT_SETTLE_MS,
                control,
            )?
        };
        super::target::evidence(control);
        Self::pointer_action_result(
            operation,
            foreground_before,
            spec.observation,
            settled,
            control,
        )
    }

    fn pointer_action_result(
        operation: &str,
        foreground_before: HWND,
        options: &ObservationOptions,
        settled: SettleReport,
        control: &MutationControl,
    ) -> Result<PointerActionResult, PlatformError> {
        let actual = current_cursor_position()
            .map_err(|error| post_action_error(operation, error.to_string()))?;
        let actual_display = find_display_at_point(actual).ok_or_else(|| {
            post_action_error(
                operation,
                format!(
                    "Windows reported cursor position ({}, {}) outside every active display",
                    actual.x, actual.y
                ),
            )
        })?;
        let local_x = u32::try_from(actual.x - actual_display.bounds.left).unwrap_or(0);
        let local_y = u32::try_from(actual.y - actual_display.bounds.top).unwrap_or(0);
        let position = MousePosition {
            display_id: actual_display.id.clone(),
            local_x,
            local_y,
            virtual_x: actual.x,
            virtual_y: actual.y,
        };
        let foreground_after = foreground_window_handle();
        let observation_display_id =
            if foreground_after != foreground_before && !foreground_after.is_invalid() {
                window_info(foreground_after)
                    .map_or_else(|_| actual_display.id.clone(), |window| window.display_id)
            } else {
                actual_display.id.clone()
            };
        let mut observation = observe_display(&observation_display_id, options, control)
            .map_err(|error| post_action_error(operation, error.to_string()))?;
        observation.settle = Some(settled);

        Ok(PointerActionResult {
            position,
            observation,
        })
    }
}

/// Shortest time a drag spends travelling from press to release. Applications that start a drag
/// only after the pointer has moved while the button is down need intermediate moves; a drag
/// that jumps in one step is a click at a different place to them. The travel is stepped at
/// `MOVE_FRAME_MS` intervals by `move_cursor`.
const MIN_DRAG_DURATION_MS: u32 = 120;

/// The travel time actually used for a requested `duration_ms`, which can be zero.
const fn effective_drag_duration_ms(requested: u32) -> u32 {
    if requested < MIN_DRAG_DURATION_MS {
        MIN_DRAG_DURATION_MS
    } else {
        requested
    }
}

/// Waits for the target application to process sent input before evidence is read, and reports
/// how the wait ended. The input is recorded as sent first, and a stop during the wait is
/// reported as missing evidence, so the action is completed but unverified and a batch counts
/// the step as delivered. With a watch the wait ends once the approved window has
/// responded and stopped changing, or at the limits of `options`; without one it is the short
/// fixed `fixed_ms` pause.
fn settle(
    operation: &str,
    watch: Option<SettleWatch>,
    options: &ObservationOptions,
    fixed_ms: u64,
    control: &MutationControl,
) -> Result<SettleReport, PlatformError> {
    control.input_complete();
    let started = std::time::Instant::now();
    let policy = SettlePolicy::for_observation(options);
    let outcome = if let (Some(watch), Some(policy)) = (watch, policy) {
        watch.wait(operation, policy, control)
    } else {
        control
            .wait(operation, Duration::from_millis(fixed_ms))
            .map(|()| SettleOutcome::NotWatched)
    }
    .map_err(|error| post_action_error(operation, error.to_string()))?;
    let elapsed = started.elapsed();
    let stage = match outcome {
        SettleOutcome::Settled => "settle_responded",
        SettleOutcome::Unchanged => "settle_unchanged",
        SettleOutcome::StillChanging => "settle_capped",
        SettleOutcome::NotWatched => "settle_fixed",
    };
    controlfreak_core::timing::record(stage, elapsed);
    controlfreak_core::timing::record("settle", elapsed);
    Ok(SettleReport {
        outcome,
        elapsed_ms: u32::try_from(elapsed.as_millis()).unwrap_or(u32::MAX),
    })
}

/// Reads the approved window before input, unless `settle_ms` is 0 and nothing will be watched.
fn settle_watch(control: &MutationControl, options: &ObservationOptions) -> Option<SettleWatch> {
    SettlePolicy::for_observation(options)?;
    SettleWatch::begin(super::target::approved_window_bounds(control))
}

/// Checks the window at the requested point, then that the pointer is still on that point.
/// Button and wheel events fire at the live pointer, not at the requested point, so the
/// comparison is against the point the client asked for: a pointer that drifted after the
/// approach (the user's hand, or an application moving it) must not carry the input with it.
fn ensure_pointer_target(
    control: &MutationControl,
    operation: &str,
    requested: POINT,
) -> Result<(), PlatformError> {
    super::target::validate(control, operation, Some(requested))?;
    ensure_pointer_not_moved(operation, requested, current_cursor_position()?)
}

/// Refuses dispatch when the live pointer is not on the requested point.
fn ensure_pointer_not_moved(
    operation: &str,
    requested: POINT,
    current: POINT,
) -> Result<(), PlatformError> {
    if current == requested {
        Ok(())
    } else {
        Err(PlatformError::failed(
            controlfreak_core::FailureCause::PointerMoved,
            operation,
            "the pointer moved away from the requested point; observe again before retrying",
        ))
    }
}

/// Refuses a click prepared from an observation that other input may have made stale.
fn ensure_input_generation(
    operation: &str,
    expected: Option<u64>,
    current: u64,
) -> Result<(), PlatformError> {
    if expected.is_none_or(|expected| expected == current) {
        Ok(())
    } else {
        Err(PlatformError::failed(
            controlfreak_core::FailureCause::InputInterleaved,
            operation,
            "another input action ran during text recognition; find the text again before clicking",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn released_input_lock_advances_the_generation() {
        let backend = Backend::new();
        let control = MutationControl::default();
        let before = backend
            .input_generation
            .load(std::sync::atomic::Ordering::Acquire);
        drop(backend.lock_input(&control).unwrap());
        let after = backend
            .input_generation
            .load(std::sync::atomic::Ordering::Acquire);
        assert_eq!(after, before + 1);
        assert!(ensure_input_generation("click_text", Some(before), after).is_err());
        assert!(ensure_input_generation("click_text", Some(after), after).is_ok());
        assert!(ensure_input_generation("click_mouse", None, after).is_ok());
    }

    #[test]
    fn a_stop_during_the_settle_wait_reports_sent_input_without_evidence() {
        let control = MutationControl::default();
        control.cancel();
        let error = settle(
            "click_mouse",
            None,
            &ObservationOptions::default(),
            POST_INPUT_SETTLE_MS,
            &control,
        )
        .unwrap_err();
        assert!(
            matches!(error, PlatformError::PostActionObservationFailed { .. }),
            "{error:?}"
        );
        assert_eq!(
            control.progress().input_outcome,
            controlfreak_core::InputOutcome::InputSent
        );
    }

    #[test]
    fn drags_always_travel_in_several_steps() {
        assert_eq!(effective_drag_duration_ms(0), MIN_DRAG_DURATION_MS);
        assert_eq!(effective_drag_duration_ms(1), MIN_DRAG_DURATION_MS);
        assert_eq!(
            effective_drag_duration_ms(MIN_DRAG_DURATION_MS),
            MIN_DRAG_DURATION_MS
        );
        assert_eq!(effective_drag_duration_ms(2_000), 2_000);
        // `move_cursor` makes one move per frame, so the default travel is a real path.
        assert!(u64::from(MIN_DRAG_DURATION_MS).div_ceil(super::super::MOVE_FRAME_MS) >= 4);
    }

    #[test]
    fn pointer_drift_from_the_requested_point_refuses_dispatch() {
        let requested = POINT { x: -100, y: 20 };
        assert!(ensure_pointer_not_moved("click_mouse", requested, requested).is_ok());
        let error = ensure_pointer_not_moved("click_mouse", requested, POINT { x: -99, y: 20 })
            .unwrap_err();
        assert_eq!(error.reason_code(), Some("pointer_moved"));
        assert!(error.retryable());
    }
}
