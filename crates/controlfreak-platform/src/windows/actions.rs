use super::{
    Backend, Duration, HWND, KeyChordRequest, KeyboardActionResult, KeyboardBackend,
    MAX_SCROLL_DELTA, MAX_TEXT_UTF16_UNITS, MouseClickRequest, MouseDragRequest, MouseMoveRequest,
    MousePosition, MouseScrollRequest, MutationControl, ObservationOptions, POINT,
    POST_FOCUS_SETTLE_MS, POST_INPUT_SETTLE_MS, PlatformError, PointerActionResult,
    PointerActionSpec, PointerBackend, TextInputRequest, current_cursor_position,
    ensure_dpi_awareness, ensure_interactive_input_desktop, find_display_at_point,
    foreground_window_handle, move_cursor, observe_display, observe_foreground, post_action_error,
    privilege, send_click, send_drag_press, send_drag_release, send_key_chord, send_scroll,
    send_unicode_text, thread, validate_click_request, validate_duration, validate_key_chord,
    validate_modifiers, validate_observation, window_info,
};

impl PointerBackend for Backend {
    fn move_mouse(&self, request: &MouseMoveRequest) -> Result<PointerActionResult, PlatformError> {
        self.move_mouse_controlled(request, &MutationControl::default())
    }

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
            |_, _| Ok(()),
        )
    }

    fn click_mouse(
        &self,
        request: &MouseClickRequest,
    ) -> Result<PointerActionResult, PlatformError> {
        self.click_mouse_controlled(request, &MutationControl::default())
    }

    fn click_mouse_controlled(
        &self,
        request: &MouseClickRequest,
        control: &MutationControl,
    ) -> Result<PointerActionResult, PlatformError> {
        self.click_mouse_at_generation(request, control, None)
    }

    fn drag_mouse(&self, request: &MouseDragRequest) -> Result<PointerActionResult, PlatformError> {
        self.drag_mouse_controlled(request, &MutationControl::default())
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
        let _guard = self.lock_input(control)?;
        let foreground_before = foreground_window_handle();
        ensure_interactive_input_desktop("drag_mouse")?;
        control.check("drag_mouse")?;
        move_cursor(
            "drag_mouse",
            current_cursor_position()?,
            POINT {
                x: start_x,
                y: start_y,
            },
            0,
            control,
            |point| {
                super::target::validate(control, "drag_mouse", Some(point))?;
                super::input::ensure_pointer_idle("drag_mouse")
            },
        )?;
        control.check("drag_mouse")?;
        send_drag_press(request.button, &request.modifiers, control, || {
            super::target::validate(control, "drag_mouse", Some(current_cursor_position()?))
        })?;
        control.cleanup_status(controlfreak_core::CleanupStatus::Unknown);
        let movement = move_cursor(
            "drag_mouse",
            POINT {
                x: start_x,
                y: start_y,
            },
            POINT { x: end_x, y: end_y },
            request.duration_ms,
            control,
            |point| super::target::validate(control, "drag_mouse", Some(point)),
        );
        let release = send_drag_release(
            request.button,
            &request.modifiers,
            control,
            movement.is_err(),
            || super::target::validate(control, "drag_mouse", Some(current_cursor_position()?)),
        );
        movement?;
        release?;
        thread::sleep(Duration::from_millis(POST_INPUT_SETTLE_MS));
        control.input_complete();
        super::target::evidence(control);
        Self::pointer_action_result("drag_mouse", foreground_before, &request.observation)
    }

    fn scroll_mouse(
        &self,
        request: &MouseScrollRequest,
    ) -> Result<PointerActionResult, PlatformError> {
        self.scroll_mouse_controlled(request, &MutationControl::default())
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
            |control, target| {
                control.check("scroll_mouse")?;
                send_scroll(request.delta_x, request.delta_y, control, || {
                    super::target::validate(control, "scroll_mouse", Some(target))?;
                    ensure_cursor_at("scroll_mouse", target, current_cursor_position()?)
                })?;
                thread::sleep(Duration::from_millis(POST_INPUT_SETTLE_MS));
                Ok(())
            },
        )
    }
}

impl KeyboardBackend for Backend {
    fn press_keys(&self, request: &KeyChordRequest) -> Result<KeyboardActionResult, PlatformError> {
        self.press_keys_controlled(request, &MutationControl::default())
    }

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
        send_key_chord("press_keys", &request.keys, control, || {
            super::target::validate(control, "press_keys", None)
        })?;
        thread::sleep(Duration::from_millis(POST_FOCUS_SETTLE_MS));
        control.input_complete();
        super::target::evidence(control);
        Ok(KeyboardActionResult {
            observation: observe_foreground(&request.observation)
                .map_err(|error| post_action_error("press_keys", error.to_string()))?,
        })
    }

    fn type_text(&self, request: &TextInputRequest) -> Result<KeyboardActionResult, PlatformError> {
        self.type_text_controlled(request, &MutationControl::default())
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

        let _guard = self.lock_input(control)?;
        super::target::validate(control, "type_text", None)?;
        send_unicode_text(&utf16, control, || {
            super::target::validate(control, "type_text", None)
        })?;
        thread::sleep(Duration::from_millis(POST_FOCUS_SETTLE_MS));
        control.input_complete();
        super::target::evidence(control);
        Ok(KeyboardActionResult {
            observation: observe_foreground(&request.observation)
                .map_err(|error| post_action_error("type_text", error.to_string()))?,
        })
    }
}

/// Holds the input lock; releasing it records that input may have changed the desktop.
pub(super) struct InputGuard<'a> {
    _lock: std::sync::MutexGuard<'a, ()>,
    generation: &'a std::sync::atomic::AtomicU64,
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
            |control, target| {
                control.check("click_mouse")?;
                send_click(
                    request.button,
                    request.click_count,
                    &request.modifiers,
                    control,
                    || {
                        super::target::validate(control, "click_mouse", Some(target))?;
                        ensure_cursor_at("click_mouse", target, current_cursor_position()?)
                    },
                )?;
                thread::sleep(Duration::from_millis(POST_INPUT_SETTLE_MS));
                Ok(())
            },
        )
    }

    pub(super) fn lock_input(
        &self,
        control: &MutationControl,
    ) -> Result<InputGuard<'_>, PlatformError> {
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
        action: F,
    ) -> Result<PointerActionResult, PlatformError>
    where
        F: FnOnce(&MutationControl, POINT) -> Result<(), PlatformError>,
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

        move_cursor(
            operation,
            start,
            POINT {
                x: target_x,
                y: target_y,
            },
            spec.duration_ms,
            control,
            |point| {
                super::target::validate(control, operation, Some(point))?;
                super::input::ensure_pointer_idle(operation)
            },
        )?;
        // A move-only action is already dispatched. Its next cursor read is observation,
        // whereas clicks and scrolls still need this read to validate their input target.
        if operation == "move_mouse" {
            control.input_complete();
            super::target::evidence(control);
            return Self::pointer_action_result(operation, foreground_before, spec.observation);
        }
        let actual_target = current_cursor_position()?;
        action(control, actual_target)?;

        control.input_complete();
        super::target::evidence(control);
        Self::pointer_action_result(operation, foreground_before, spec.observation)
    }

    fn pointer_action_result(
        operation: &str,
        foreground_before: HWND,
        options: &ObservationOptions,
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
        let observation = observe_display(&observation_display_id, options)
            .map_err(|error| post_action_error(operation, error.to_string()))?;

        Ok(PointerActionResult {
            position,
            observation,
        })
    }
}

/// Button and wheel events fire at the live cursor, so refuse if it left the validated point.
fn ensure_cursor_at(
    operation: &str,
    validated: POINT,
    current: POINT,
) -> Result<(), PlatformError> {
    if current == validated {
        Ok(())
    } else {
        Err(PlatformError::OperationFailed {
            operation: operation.to_owned(),
            reason: "the pointer moved after target validation; observe again before retrying"
                .to_owned(),
        })
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
        Err(PlatformError::OperationFailed {
            operation: operation.to_owned(),
            reason: "another input action ran during text recognition; find the text again before clicking".to_owned(),
        })
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
    fn pointer_drift_after_validation_refuses_dispatch() {
        let validated = POINT { x: -100, y: 20 };
        assert!(ensure_cursor_at("click_mouse", validated, validated).is_ok());
        let error =
            ensure_cursor_at("click_mouse", validated, POINT { x: -99, y: 20 }).unwrap_err();
        assert!(error.to_string().contains("pointer moved"));
    }
}
