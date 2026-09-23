#![allow(unsafe_code)]

use super::{
    Duration, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, Instant, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, Key, KeyChordRequest,
    MAX_CHORD_KEYS, MAX_CLICK_COUNT, MOUSE_EVENT_FLAGS, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN,
    MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL, MOUSEINPUT, MOVE_FRAME_MS, MouseButton,
    MouseClickRequest, MutationControl, POINT, PlatformError, SendInput, SetCursorPos, VIRTUAL_KEY,
    size_of, thread, win32_error,
};

pub(super) fn move_cursor<F>(
    operation: &str,
    start: POINT,
    target: POINT,
    duration_ms: u32,
    control: &MutationControl,
    validate_target: F,
) -> Result<(), PlatformError>
where
    F: FnMut(POINT) -> Result<(), PlatformError>,
{
    move_cursor_with(
        operation,
        start,
        target,
        duration_ms,
        control,
        validate_target,
        |point| {
            // SAFETY: SetCursorPos takes signed screen coordinates and retains no references.
            unsafe { SetCursorPos(point.x, point.y) }
                .map_err(|error| win32_error("SetCursorPos", &error))
        },
    )
}

fn move_cursor_with(
    operation: &str,
    start: POINT,
    target: POINT,
    duration_ms: u32,
    control: &MutationControl,
    mut validate_target: impl FnMut(POINT) -> Result<(), PlatformError>,
    mut set_position: impl FnMut(POINT) -> Result<(), PlatformError>,
) -> Result<(), PlatformError> {
    control.check(operation)?;
    if duration_ms == 0 {
        validate_target(target)?;
        control.check(operation)?;
        control.dispatch_started();
        set_position(target)?;
        control.dispatch_accepted(0);
        return Ok(());
    }

    let steps = u64::from(duration_ms).div_ceil(MOVE_FRAME_MS).max(1);
    let started = Instant::now();
    for step in 1..=steps {
        control.check(operation)?;
        let target_elapsed = Duration::from_millis(u64::from(duration_ms) * step / steps);
        if let Some(remaining) = target_elapsed.checked_sub(started.elapsed()) {
            control.wait(operation, remaining)?;
        }
        let x = interpolate(start.x, target.x, step, steps);
        let y = interpolate(start.y, target.y, step, steps);
        validate_target(POINT { x, y })?;
        control.check(operation)?;
        control.dispatch_started();
        set_position(POINT { x, y })?;
        control.dispatch_accepted(0);
    }
    Ok(())
}

fn mouse_input(flags: MOUSE_EVENT_FLAGS, data: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                mouseData: u32::from_ne_bytes(data.to_ne_bytes()),
                dwFlags: flags,
                ..Default::default()
            },
        },
    }
}

pub(super) fn validate_key_chord(request: &KeyChordRequest) -> Result<(), PlatformError> {
    if request.keys.is_empty() || request.keys.len() > MAX_CHORD_KEYS {
        return Err(PlatformError::InvalidArgument {
            argument: "keys".to_owned(),
            reason: format!("must contain between 1 and {MAX_CHORD_KEYS} keys"),
        });
    }
    let unique: std::collections::HashSet<Key> = request.keys.iter().copied().collect();
    if unique.len() != request.keys.len() {
        return Err(PlatformError::InvalidArgument {
            argument: "keys".to_owned(),
            reason: "must not contain duplicate keys".to_owned(),
        });
    }
    Ok(())
}

pub(super) fn validate_modifiers(modifiers: &[Key]) -> Result<(), PlatformError> {
    if modifiers.len() > 4
        || modifiers
            .iter()
            .any(|key| !matches!(key, Key::Ctrl | Key::Alt | Key::Shift | Key::Win))
    {
        return Err(PlatformError::InvalidArgument {
            argument: "modifiers".to_owned(),
            reason: "may contain only ctrl, alt, shift, and win".to_owned(),
        });
    }
    let unique: std::collections::HashSet<Key> = modifiers.iter().copied().collect();
    if unique.len() != modifiers.len() {
        return Err(PlatformError::InvalidArgument {
            argument: "modifiers".to_owned(),
            reason: "must not contain duplicate keys".to_owned(),
        });
    }
    Ok(())
}

pub(super) fn validate_click_request(request: &MouseClickRequest) -> Result<(), PlatformError> {
    if request.click_count == 0 || request.click_count > MAX_CLICK_COUNT {
        return Err(PlatformError::InvalidArgument {
            argument: "click_count".to_owned(),
            reason: format!("must be between 1 and {MAX_CLICK_COUNT}"),
        });
    }
    validate_modifiers(&request.modifiers)
}

fn keyboard_input(key: Key, key_up: bool) -> INPUT {
    let (virtual_key, extended) = virtual_key(key);
    let mut flags = if key_up {
        KEYEVENTF_KEYUP
    } else {
        KEYBD_EVENT_FLAGS::default()
    };
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: virtual_key,
                dwFlags: flags,
                ..Default::default()
            },
        },
    }
}

fn unicode_input(unit: u16, key_up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: unit,
                dwFlags: KEYEVENTF_UNICODE
                    | if key_up {
                        KEYEVENTF_KEYUP
                    } else {
                        KEYBD_EVENT_FLAGS::default()
                    },
                ..Default::default()
            },
        },
    }
}

pub(super) fn send_key_chord<F>(
    operation: &str,
    keys: &[Key],
    control: &MutationControl,
    mut validate_target: F,
) -> Result<(), PlatformError>
where
    F: FnMut() -> Result<(), PlatformError>,
{
    control.check(operation)?;
    let inputs = key_chord_inputs(keys);
    validate_target()?;
    send_releasing(operation, &inputs, control, native_send_inputs)
}

pub(super) fn key_chord_inputs(keys: &[Key]) -> Vec<INPUT> {
    let mut inputs = Vec::with_capacity(keys.len() * 2);
    inputs.extend(keys.iter().copied().map(|key| keyboard_input(key, false)));
    inputs.extend(
        keys.iter()
            .rev()
            .copied()
            .map(|key| keyboard_input(key, true)),
    );
    inputs
}

pub(super) fn send_unicode_text<F>(
    utf16: &[u16],
    control: &MutationControl,
    validate_target: F,
) -> Result<(), PlatformError>
where
    F: FnMut() -> Result<(), PlatformError>,
{
    send_unicode_text_with(utf16, control, validate_target, native_send_inputs)
}

fn send_unicode_text_with<F, S>(
    utf16: &[u16],
    control: &MutationControl,
    mut validate_target: F,
    mut send: S,
) -> Result<(), PlatformError>
where
    F: FnMut() -> Result<(), PlatformError>,
    S: FnMut(&[INPUT]) -> Result<usize, PlatformError>,
{
    const UNITS_PER_BATCH: usize = 50;
    for batch in utf16.chunks(UNITS_PER_BATCH) {
        control.check("type_text")?;
        let mut inputs = Vec::with_capacity(batch.len() * 2);
        for unit in batch {
            inputs.push(unicode_input(*unit, false));
            inputs.push(unicode_input(*unit, true));
        }
        validate_target()?;
        send_releasing("type_text", &inputs, control, &mut send)?;
        if batch.len() == UNITS_PER_BATCH {
            thread::sleep(Duration::from_millis(2));
        }
    }
    Ok(())
}

fn virtual_key(key: Key) -> (VIRTUAL_KEY, bool) {
    let code = match key {
        Key::Backspace => 0x08,
        Key::Tab => 0x09,
        Key::Enter => 0x0D,
        Key::Shift => 0x10,
        Key::Ctrl => 0x11,
        Key::Alt => 0x12,
        Key::Escape => 0x1B,
        Key::Space => 0x20,
        Key::PageUp => 0x21,
        Key::PageDown => 0x22,
        Key::End => 0x23,
        Key::Home => 0x24,
        Key::ArrowLeft => 0x25,
        Key::ArrowUp => 0x26,
        Key::ArrowRight => 0x27,
        Key::ArrowDown => 0x28,
        Key::Insert => 0x2D,
        Key::Delete => 0x2E,
        Key::Digit0 => 0x30,
        Key::Digit1 => 0x31,
        Key::Digit2 => 0x32,
        Key::Digit3 => 0x33,
        Key::Digit4 => 0x34,
        Key::Digit5 => 0x35,
        Key::Digit6 => 0x36,
        Key::Digit7 => 0x37,
        Key::Digit8 => 0x38,
        Key::Digit9 => 0x39,
        Key::A => 0x41,
        Key::B => 0x42,
        Key::C => 0x43,
        Key::D => 0x44,
        Key::E => 0x45,
        Key::F => 0x46,
        Key::G => 0x47,
        Key::H => 0x48,
        Key::I => 0x49,
        Key::J => 0x4A,
        Key::K => 0x4B,
        Key::L => 0x4C,
        Key::M => 0x4D,
        Key::N => 0x4E,
        Key::O => 0x4F,
        Key::P => 0x50,
        Key::Q => 0x51,
        Key::R => 0x52,
        Key::S => 0x53,
        Key::T => 0x54,
        Key::U => 0x55,
        Key::V => 0x56,
        Key::W => 0x57,
        Key::X => 0x58,
        Key::Y => 0x59,
        Key::Z => 0x5A,
        Key::Win => 0x5B,
        Key::F1 => 0x70,
        Key::F2 => 0x71,
        Key::F3 => 0x72,
        Key::F4 => 0x73,
        Key::F5 => 0x74,
        Key::F6 => 0x75,
        Key::F7 => 0x76,
        Key::F8 => 0x77,
        Key::F9 => 0x78,
        Key::F10 => 0x79,
        Key::F11 => 0x7A,
        Key::F12 => 0x7B,
    };
    let extended = matches!(
        key,
        Key::Win
            | Key::Insert
            | Key::Delete
            | Key::Home
            | Key::End
            | Key::PageUp
            | Key::PageDown
            | Key::ArrowUp
            | Key::ArrowDown
            | Key::ArrowLeft
            | Key::ArrowRight
    );
    (VIRTUAL_KEY(code), extended)
}

fn mouse_button_flags(button: MouseButton) -> (MOUSE_EVENT_FLAGS, MOUSE_EVENT_FLAGS) {
    match button {
        MouseButton::Left => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
        MouseButton::Right => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
        MouseButton::Middle => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
    }
}

pub(super) fn click_inputs(button: MouseButton, click_count: u8, modifiers: &[Key]) -> Vec<INPUT> {
    let (down, up) = mouse_button_flags(button);
    let mut inputs = Vec::with_capacity(modifiers.len() * 2 + usize::from(click_count) * 2);
    inputs.extend(
        modifiers
            .iter()
            .copied()
            .map(|key| keyboard_input(key, false)),
    );
    for _ in 0..click_count {
        inputs.push(mouse_input(down, 0));
        inputs.push(mouse_input(up, 0));
    }
    inputs.extend(
        modifiers
            .iter()
            .rev()
            .copied()
            .map(|key| keyboard_input(key, true)),
    );
    inputs
}

fn pointer_release_inputs(button: MouseButton, modifiers: &[Key]) -> Vec<INPUT> {
    let (_, up) = mouse_button_flags(button);
    let mut inputs = Vec::with_capacity(modifiers.len() + 1);
    inputs.push(mouse_input(up, 0));
    inputs.extend(
        modifiers
            .iter()
            .rev()
            .copied()
            .map(|key| keyboard_input(key, true)),
    );
    inputs
}

pub(super) fn scroll_inputs(delta_x: i32, delta_y: i32) -> Vec<INPUT> {
    let mut inputs = Vec::with_capacity(2);
    if delta_y != 0 {
        inputs.push(mouse_input(MOUSEEVENTF_WHEEL, delta_y));
    }
    if delta_x != 0 {
        inputs.push(mouse_input(MOUSEEVENTF_HWHEEL, delta_x));
    }
    inputs
}

pub(super) fn send_click<F>(
    button: MouseButton,
    click_count: u8,
    modifiers: &[Key],
    control: &MutationControl,
    mut validate_target: F,
) -> Result<(), PlatformError>
where
    F: FnMut() -> Result<(), PlatformError>,
{
    let inputs = click_inputs(button, click_count, modifiers);
    validate_target()?;
    send_releasing("click_mouse", &inputs, control, native_send_inputs)
}

pub(super) fn send_drag_press<F>(
    button: MouseButton,
    modifiers: &[Key],
    control: &MutationControl,
    mut validate_target: F,
) -> Result<(), PlatformError>
where
    F: FnMut() -> Result<(), PlatformError>,
{
    let (down, _) = mouse_button_flags(button);
    let mut inputs = Vec::with_capacity(modifiers.len() + 1);
    inputs.extend(
        modifiers
            .iter()
            .copied()
            .map(|key| keyboard_input(key, false)),
    );
    inputs.push(mouse_input(down, 0));
    validate_target()?;
    send_releasing("drag_mouse", &inputs, control, native_send_inputs)
}

pub(super) fn send_drag_release<F>(
    button: MouseButton,
    modifiers: &[Key],
    control: &MutationControl,
    cleanup_only: bool,
    mut validate_target: F,
) -> Result<(), PlatformError>
where
    F: FnMut() -> Result<(), PlatformError>,
{
    let releases = pointer_release_inputs(button, modifiers);
    if cleanup_only {
        send_cleanup(&releases, control, native_send_inputs);
        return Ok(());
    }
    if let Err(error) = validate_target() {
        // Bounded release cleanup must still run after revalidation fails.
        send_cleanup(&releases, control, native_send_inputs);
        return Err(error);
    }
    release_inputs(&releases, control, native_send_inputs)
}

pub(super) fn send_scroll<F>(
    delta_x: i32,
    delta_y: i32,
    control: &MutationControl,
    mut validate_target: F,
) -> Result<(), PlatformError>
where
    F: FnMut() -> Result<(), PlatformError>,
{
    let inputs = scroll_inputs(delta_x, delta_y);
    validate_target()?;
    control.check("scroll_mouse")?;
    let inserted = dispatch_inputs(&inputs, control, native_send_inputs)?;
    if inserted == inputs.len() {
        Ok(())
    } else {
        Err(incomplete_input_error(
            "scroll_mouse",
            inserted,
            inputs.len(),
        ))
    }
}

fn any_requested_input_held(inputs: &[INPUT], mut is_down: impl FnMut(i32) -> bool) -> bool {
    for input in inputs {
        // SAFETY: Each INPUT is locally constructed with its matching union tag;
        // The query receives a virtual-key code and no native references.
        let held = unsafe {
            if input.r#type == INPUT_KEYBOARD {
                let key = input.Anonymous.ki;
                !key.dwFlags.contains(KEYEVENTF_KEYUP)
                    && key.wVk.0 != 0
                    && is_down(i32::from(key.wVk.0))
            } else if input.r#type == INPUT_MOUSE {
                let flags = input.Anonymous.mi.dwFlags;
                [
                    (MOUSEEVENTF_LEFTDOWN, 1),
                    (MOUSEEVENTF_RIGHTDOWN, 2),
                    (MOUSEEVENTF_MIDDLEDOWN, 4),
                ]
                .into_iter()
                .any(|(down, key)| flags.contains(down) && is_down(key))
            } else {
                false
            }
        };
        if held {
            return true;
        }
    }
    false
}

// Use only before ControlFreak presses a drag button. Recheck on every approach frame.
pub(super) fn ensure_pointer_idle(operation: &str) -> Result<(), PlatformError> {
    ensure_pointer_idle_with(operation, |key| {
        // SAFETY: GetAsyncKeyState takes a virtual-key code and retains no references.
        unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(key) < 0 }
    })
}

fn ensure_pointer_idle_with(
    operation: &str,
    mut is_down: impl FnMut(i32) -> bool,
) -> Result<(), PlatformError> {
    if [1, 2, 4, 5, 6].into_iter().any(&mut is_down) {
        return Err(PlatformError::OperationFailed {
            operation: operation.to_owned(),
            reason: "a mouse button is already held; refusing to move the user's drag".to_owned(),
        });
    }
    Ok(())
}

fn native_send_inputs(inputs: &[INPUT]) -> Result<usize, PlatformError> {
    use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    if any_requested_input_held(inputs, |key| {
        // SAFETY: GetAsyncKeyState takes a virtual-key code and retains no references.
        unsafe { GetAsyncKeyState(key) < 0 }
    }) {
        return Ok(0);
    }
    let input_size =
        i32::try_from(size_of::<INPUT>()).map_err(|_| PlatformError::OperationFailed {
            operation: "SendInput".to_owned(),
            reason: "INPUT size exceeds the Win32 parameter width".to_owned(),
        })?;
    // SAFETY: `inputs` is a valid contiguous INPUT slice for the duration of the synchronous call,
    // and `input_size` is the checked platform size of INPUT.
    let inserted = unsafe { SendInput(inputs, input_size) };
    usize::try_from(inserted).map_err(|_| PlatformError::OperationFailed {
        operation: "SendInput".to_owned(),
        reason: "Windows returned an invalid inserted-input count".to_owned(),
    })
}

fn dispatch_inputs(
    inputs: &[INPUT],
    control: &MutationControl,
    send: impl FnOnce(&[INPUT]) -> Result<usize, PlatformError>,
) -> Result<usize, PlatformError> {
    let previous = control.dispatch_started();
    let inserted = send(inputs)?;
    if inserted == 0 {
        control.dispatch_rejected(previous);
    } else {
        control.dispatch_accepted(inserted as u64);
    }
    Ok(inserted)
}

fn send_releasing(
    operation: &str,
    inputs: &[INPUT],
    control: &MutationControl,
    mut send: impl FnMut(&[INPUT]) -> Result<usize, PlatformError>,
) -> Result<(), PlatformError> {
    control.check(operation)?;
    // If dispatch unwinds, release state is unknown until cleanup has acknowledged it.
    control.cleanup_status(controlfreak_core::CleanupStatus::Unknown);
    let result = dispatch_inputs(inputs, control, &mut send);
    match result {
        Ok(inserted) if inserted == inputs.len() => {
            control.cleanup_status(controlfreak_core::CleanupStatus::NotNeeded);
            Ok(())
        }
        Ok(inserted) => {
            let owned = owned_releases(&inputs[..inserted.min(inputs.len())]);
            if owned.is_empty() {
                control.cleanup_status(controlfreak_core::CleanupStatus::NotNeeded);
            } else {
                send_cleanup(&owned, control, send);
            }
            Err(incomplete_input_error(operation, inserted, inputs.len()))
        }
        Err(error) => Err(error),
    }
}

// Derive compensating releases only from acknowledged downs without matching ups.
// This is also the cleanup hook for a future managed hold.
fn owned_releases(inputs: &[INPUT]) -> Vec<INPUT> {
    let mut held: Vec<((u32, u16, u16), INPUT)> = Vec::new();
    for input in inputs {
        // SAFETY: INPUT is constructed locally and the union member is selected by r#type.
        let event = unsafe {
            if input.r#type == INPUT_KEYBOARD {
                let key = input.Anonymous.ki;
                let identity = (0, key.wVk.0, key.wScan);
                let mut release = *input;
                release.Anonymous.ki.dwFlags |= KEYEVENTF_KEYUP;
                Some((identity, !key.dwFlags.contains(KEYEVENTF_KEYUP), release))
            } else if input.r#type == INPUT_MOUSE {
                let flags = input.Anonymous.mi.dwFlags;
                [MouseButton::Left, MouseButton::Right, MouseButton::Middle]
                    .into_iter()
                    .find_map(|button| {
                        let (down, up) = mouse_button_flags(button);
                        if flags.contains(down) || flags.contains(up) {
                            Some(((down.0, 0, 0), flags.contains(down), mouse_input(up, 0)))
                        } else {
                            None
                        }
                    })
            } else {
                None
            }
        };
        if let Some((identity, down, release)) = event {
            if down {
                if !held.iter().any(|(key, _)| *key == identity) {
                    held.push((identity, release));
                }
            } else {
                held.retain(|(key, _)| *key != identity);
            }
        }
    }
    held.into_iter().rev().map(|(_, release)| release).collect()
}

fn release_inputs(
    releases: &[INPUT],
    control: &MutationControl,
    mut send: impl FnMut(&[INPUT]) -> Result<usize, PlatformError>,
) -> Result<(), PlatformError> {
    let inserted = match dispatch_inputs(releases, control, &mut send) {
        Ok(inserted) => inserted,
        Err(error) => {
            send_cleanup(releases, control, &mut send);
            return Err(error);
        }
    };
    if inserted == releases.len() {
        control.cleanup_status(controlfreak_core::CleanupStatus::NotNeeded);
        Ok(())
    } else {
        // Preserve the original release order without replaying already accepted key-ups.
        send_cleanup(
            &releases[inserted.min(releases.len())..],
            control,
            &mut send,
        );
        Err(incomplete_input_error(
            "drag_mouse",
            inserted,
            releases.len(),
        ))
    }
}

fn send_cleanup(
    inputs: &[INPUT],
    control: &MutationControl,
    send: impl FnOnce(&[INPUT]) -> Result<usize, PlatformError>,
) {
    use controlfreak_core::CleanupStatus;
    control.cleanup_status(CleanupStatus::Unknown);
    if send(inputs).is_ok_and(|sent| sent == inputs.len()) {
        control.cleanup_status(CleanupStatus::Succeeded);
    }
}

fn incomplete_input_error(operation: &str, inserted: usize, requested: usize) -> PlatformError {
    PlatformError::OperationFailed {
        operation: operation.to_owned(),
        reason: format!(
            "SendInput inserted {inserted} of {requested} events; Windows may block input targeting a higher-integrity application"
        ),
    }
}

pub(super) fn post_action_error(operation: &str, reason: String) -> PlatformError {
    PlatformError::PostActionObservationFailed {
        operation: operation.to_owned(),
        reason,
    }
}

pub(super) fn interpolate(start: i32, end: i32, step: u64, steps: u64) -> i32 {
    let start = i64::from(start);
    let distance = i64::from(end) - start;
    let step = i64::try_from(step).unwrap_or(i64::MAX);
    let steps = i64::try_from(steps).unwrap_or(i64::MAX);
    let value = start + distance * step / steps;
    i32::try_from(value).unwrap_or(if value.is_negative() {
        i32::MIN
    } else {
        i32::MAX
    })
}

#[cfg(test)]
mod progress_tests {
    use super::*;
    use controlfreak_core::{CleanupStatus, InputOutcome};

    fn failure() -> PlatformError {
        PlatformError::OperationFailed {
            operation: "synthetic".into(),
            reason: "injected".into(),
        }
    }

    #[test]
    fn held_buttons_refuse_cursor_dispatch_before_click_or_drag() {
        for button in [1, 2, 4, 5, 6] {
            for duration in [0, 20] {
                let control = MutationControl::default();
                let mut dispatched = false;
                let result = move_cursor_with(
                    "approach",
                    POINT::default(),
                    POINT { x: 100, y: 100 },
                    duration,
                    &control,
                    |_| ensure_pointer_idle_with("approach", |key| key == button),
                    |_| {
                        dispatched = true;
                        Ok(())
                    },
                );
                assert!(result.is_err());
                assert!(!dispatched);
                assert_eq!(control.progress(), MutationControl::default().progress());
            }
        }
        assert!(ensure_pointer_idle_with("approach", |_| false).is_ok());
    }

    #[test]
    fn cancellation_after_target_validation_does_not_dispatch_or_release_unowned_keys() {
        let control = MutationControl::default();
        let result = send_unicode_text_with(
            &[65, 66],
            &control,
            || {
                control.cancel();
                Ok(())
            },
            |_| panic!("cancelled batch must not dispatch or clean up unowned keys"),
        );
        assert!(result.is_err());
        assert_eq!(control.progress().sent_events, 0);
        assert_eq!(control.progress().cleanup, CleanupStatus::NotNeeded);
    }

    #[test]
    fn partial_chords_release_only_acknowledged_unmatched_downs() {
        let inputs = key_chord_inputs(&[Key::Ctrl, Key::Shift, Key::A]);
        for (accepted, expected) in [(0, 0), (1, 1), (2, 2), (3, 3), (4, 2), (5, 1), (6, 0)] {
            assert_eq!(owned_releases(&inputs[..accepted]).len(), expected);
        }
        let clicks = click_inputs(MouseButton::Left, 2, &[Key::Ctrl]);
        for accepted in 0..=clicks.len() {
            let releases = owned_releases(&clicks[..accepted]);
            assert!(releases.len() <= 2);
        }
        assert!(owned_releases(&clicks).is_empty());
    }

    #[test]
    fn overlapping_user_modifiers_are_refused_but_owned_releases_remain_available() {
        let chord = key_chord_inputs(&[Key::Ctrl, Key::A]);
        assert!(any_requested_input_held(&chord, |key| key == 0x11));
        assert!(!any_requested_input_held(&chord, |_| false));
        let releases = owned_releases(&chord[..2]);
        assert!(!any_requested_input_held(&releases, |_| true));
        let click = click_inputs(MouseButton::Left, 1, &[]);
        assert!(any_requested_input_held(&click, |key| key == 1));
        assert!(!any_requested_input_held(
            &owned_releases(&click[..1]),
            |_| true
        ));
    }

    #[test]
    fn text_batches_preserve_cumulative_progress_and_cleanup() {
        let text: Vec<u16> = "a\u{1f980}".repeat(40).encode_utf16().collect();
        for failed_batch in [0, 1, 2] {
            for accepted in [0, 1, 7] {
                for cleanup_succeeds in [false, true] {
                    let control = MutationControl::default();
                    let mut batch = 0;
                    let result = send_unicode_text_with(
                        &text,
                        &control,
                        || Ok(()),
                        |inputs| {
                            let current = batch;
                            batch += 1;
                            if current == failed_batch {
                                Ok(accepted)
                            } else if current > failed_batch && !cleanup_succeeds {
                                Err(failure())
                            } else {
                                Ok(inputs.len())
                            }
                        },
                    );
                    assert!(result.is_err());
                    let progress = control.progress();
                    assert_eq!(progress.sent_events, failed_batch * 100 + accepted as u64);
                    assert_eq!(
                        progress.input_outcome,
                        if failed_batch == 0 && accepted == 0 {
                            InputOutcome::NotStarted
                        } else {
                            InputOutcome::PartiallySent
                        }
                    );
                    assert_eq!(
                        progress.cleanup,
                        if accepted == 0 {
                            CleanupStatus::NotNeeded
                        } else if cleanup_succeeds {
                            CleanupStatus::Succeeded
                        } else {
                            CleanupStatus::Unknown
                        }
                    );
                }
            }
        }
    }

    #[test]
    fn later_validation_cancellation_and_provider_failure_keep_earlier_batches() {
        for mode in 0..3 {
            let control = MutationControl::default();
            let mut validations = 0;
            let mut batches = 0;
            let result = send_unicode_text_with(
                &[65; 120],
                &control,
                || {
                    validations += 1;
                    if mode == 0 && validations == 2 {
                        Err(failure())
                    } else {
                        Ok(())
                    }
                },
                |inputs| {
                    batches += 1;
                    if mode == 2 && batches == 2 {
                        return Err(failure());
                    }
                    if mode == 1 {
                        control.cancel();
                    }
                    Ok(inputs.len())
                },
            );
            assert!(result.is_err());
            assert_eq!(control.progress().sent_events, 100);
            assert_eq!(
                control.progress().input_outcome,
                if mode == 2 {
                    InputOutcome::Unknown
                } else {
                    InputOutcome::PartiallySent
                }
            );
        }
    }

    #[test]
    fn drag_cleanup_sends_only_unaccepted_releases() {
        let releases = pointer_release_inputs(MouseButton::Left, &[Key::Ctrl, Key::Shift]);
        for accepted in 0..=releases.len() {
            let control = MutationControl::default();
            control.dispatch_accepted(3);
            control.cancel();
            let mut calls = Vec::new();
            let result = release_inputs(&releases, &control, |inputs| {
                calls.push(inputs.len());
                Ok(if calls.len() == 1 {
                    accepted
                } else {
                    inputs.len()
                })
            });
            assert_eq!(result.is_ok(), accepted == releases.len());
            assert_eq!(control.progress().sent_events, 3 + accepted as u64);
            if accepted == releases.len() {
                assert_eq!(calls, [3]);
                assert_eq!(control.progress().cleanup, CleanupStatus::NotNeeded);
            } else {
                assert_eq!(calls, [3, 3 - accepted]);
                assert_eq!(control.progress().cleanup, CleanupStatus::Succeeded);
            }
        }
    }

    #[test]
    fn complete_unicode_dispatch_counts_events_without_claiming_application_effect() {
        let control = MutationControl::default();
        let text: Vec<u16> = "\u{65e5}\u{672c}\u{8a9e}\u{1f980}"
            .repeat(30)
            .encode_utf16()
            .collect();
        send_unicode_text_with(&text, &control, || Ok(()), |inputs| Ok(inputs.len())).unwrap();
        control.input_complete();
        assert_eq!(control.progress().sent_events, text.len() as u64 * 2);
        assert_eq!(control.progress().input_outcome, InputOutcome::InputSent);
        assert_eq!(control.progress().cleanup, CleanupStatus::NotNeeded);
        let next = control.for_operation();
        assert_eq!(
            next.progress(),
            controlfreak_core::MutationProgress::default()
        );
        assert_ne!(control.progress(), next.progress());
    }
}
