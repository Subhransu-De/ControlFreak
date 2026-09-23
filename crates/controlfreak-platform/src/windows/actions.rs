use super::{
    Backend, Duration, HWND, KeyChordRequest, KeyboardActionResult, KeyboardBackend,
    MAX_SCROLL_DELTA, MAX_TEXT_UTF16_UNITS, MouseClickRequest, MouseDragRequest, MouseMoveRequest,
    MousePosition, MouseScrollRequest, MutationControl, ObservationOptions, POINT,
    POST_FOCUS_SETTLE_MS, POST_INPUT_SETTLE_MS, PlatformError, PointerActionResult,
    PointerActionSpec, PointerBackend, TextInputRequest, current_cursor_position,
    ensure_dpi_awareness, ensure_interactive_input_desktop, find_display, find_display_at_point,
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
        validate_click_request(request)?;
        self.perform_pointer_action(
            "click_mouse",
            PointerActionSpec {
                display_id: &request.display_id,
                x: request.x,
                y: request.y,
                duration_ms: request.duration_ms,
                observation: &request.observation,
            },
            control,
            |control, target| {
                control.check("click_mouse")?;
                send_click(
                    request.button,
                    request.click_count,
                    &request.modifiers,
                    control,
                    || Self::ensure_point_input_target("click_mouse", target),
                )?;
                thread::sleep(Duration::from_millis(POST_INPUT_SETTLE_MS));
                Ok(())
            },
        )
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
        let start_display = find_display(&request.start_display_id)?;
        let end_display = find_display(&request.end_display_id)?;
        let (start_x, start_y) = start_display
            .bounds
            .to_virtual(request.start_x, request.start_y)?;
        let (end_x, end_y) = end_display
            .bounds
            .to_virtual(request.end_x, request.end_y)?;
        let _guard = self.lock_input();
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
            |point| Self::ensure_point_input_target("drag_mouse", point),
        )?;
        control.check("drag_mouse")?;
        send_drag_press(request.button, &request.modifiers, control, || {
            Self::ensure_point_input_target("drag_mouse", current_cursor_position()?)
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
            |point| Self::ensure_point_input_target("drag_mouse", point),
        );
        let release = send_drag_release(
            request.button,
            &request.modifiers,
            control,
            movement.is_err(),
            || Self::ensure_point_input_target("drag_mouse", current_cursor_position()?),
        );
        movement?;
        release?;
        thread::sleep(Duration::from_millis(POST_INPUT_SETTLE_MS));
        control.input_complete();
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
            },
            control,
            |control, target| {
                control.check("scroll_mouse")?;
                send_scroll(request.delta_x, request.delta_y, control, || {
                    Self::ensure_point_input_target("scroll_mouse", target)
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
        let _guard = self.lock_input();
        ensure_interactive_input_desktop("press_keys")?;
        send_key_chord("press_keys", &request.keys, control, || {
            Self::ensure_foreground_input_target("press_keys")
        })?;
        thread::sleep(Duration::from_millis(POST_FOCUS_SETTLE_MS));
        control.input_complete();
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

        let _guard = self.lock_input();
        ensure_interactive_input_desktop("type_text")?;
        send_unicode_text(&utf16, control, || {
            Self::ensure_foreground_input_target("type_text")
        })?;
        thread::sleep(Duration::from_millis(POST_FOCUS_SETTLE_MS));
        control.input_complete();
        Ok(KeyboardActionResult {
            observation: observe_foreground(&request.observation)
                .map_err(|error| post_action_error("type_text", error.to_string()))?,
        })
    }
}

impl Backend {
    pub(super) fn lock_input(&self) -> std::sync::MutexGuard<'_, ()> {
        self.input_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(super) fn ensure_window_input_target(
        operation: &str,
        hwnd: HWND,
    ) -> Result<(), PlatformError> {
        ensure_interactive_input_desktop(operation)?;
        privilege::ensure_window_integrity(operation, hwnd)
    }

    pub(super) fn ensure_foreground_input_target(operation: &str) -> Result<(), PlatformError> {
        Self::ensure_window_input_target(operation, foreground_window_handle())
    }

    fn ensure_point_input_target(operation: &str, point: POINT) -> Result<(), PlatformError> {
        ensure_interactive_input_desktop(operation)?;
        privilege::ensure_point_integrity(operation, point)
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

        let display = find_display(spec.display_id)?;
        let (target_x, target_y) = display.bounds.to_virtual(spec.x, spec.y)?;
        let _guard = self.lock_input();
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
            |point| Self::ensure_point_input_target(operation, point),
        )?;
        // A move-only action is already dispatched. Its next cursor read is observation,
        // whereas clicks and scrolls still need this read to validate their input target.
        if operation == "move_mouse" {
            control.input_complete();
            return Self::pointer_action_result(operation, foreground_before, spec.observation);
        }
        let actual_target = current_cursor_position()?;
        action(control, actual_target)?;

        control.input_complete();
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
