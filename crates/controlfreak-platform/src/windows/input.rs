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
    mut validate_target: F,
) -> Result<(), PlatformError>
where
    F: FnMut(POINT) -> Result<(), PlatformError>,
{
    control.check(operation)?;
    if duration_ms == 0 {
        validate_target(target)?;
        // SAFETY: SetCursorPos accepts any pair of i32 virtual-screen coordinates and retains no
        // pointers or references.
        return unsafe { SetCursorPos(target.x, target.y) }
            .map_err(|error| win32_error("SetCursorPos", &error));
    }

    let steps = u64::from(duration_ms).div_ceil(MOVE_FRAME_MS).max(1);
    let started = Instant::now();
    for step in 1..=steps {
        control.check(operation)?;
        let target_elapsed = Duration::from_millis(u64::from(duration_ms) * step / steps);
        if let Some(remaining) = target_elapsed.checked_sub(started.elapsed()) {
            thread::sleep(remaining);
        }
        let x = interpolate(start.x, target.x, step, steps);
        let y = interpolate(start.y, target.y, step, steps);
        validate_target(POINT { x, y })?;
        // SAFETY: SetCursorPos accepts any pair of i32 virtual-screen coordinates and retains no
        // pointers or references.
        unsafe { SetCursorPos(x, y) }.map_err(|error| win32_error("SetCursorPos", &error))?;
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
    let inserted = send_inputs(&inputs)?;
    if inserted == inputs.len() {
        return Ok(());
    }

    let releases: Vec<INPUT> = keys
        .iter()
        .rev()
        .copied()
        .map(|key| keyboard_input(key, true))
        .collect();
    let _release_attempt = send_inputs(&releases);
    Err(incomplete_input_error("press_keys", inserted, inputs.len()))
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
    mut validate_target: F,
) -> Result<(), PlatformError>
where
    F: FnMut() -> Result<(), PlatformError>,
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
        let inserted = send_inputs(&inputs)?;
        if inserted != inputs.len() {
            let releases: Vec<INPUT> = batch
                .iter()
                .copied()
                .map(|unit| unicode_input(unit, true))
                .collect();
            let _release_attempt = send_inputs(&releases);
            return Err(incomplete_input_error("type_text", inserted, inputs.len()));
        }
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
    mut validate_target: F,
) -> Result<(), PlatformError>
where
    F: FnMut() -> Result<(), PlatformError>,
{
    let inputs = click_inputs(button, click_count, modifiers);
    validate_target()?;
    let inserted = send_inputs(&inputs)?;
    if inserted == inputs.len() {
        return Ok(());
    }

    // Always attempt the matching release after a partial insertion so a failed
    // click cannot leave the logical mouse button held down.
    let releases = pointer_release_inputs(button, modifiers);
    let _release_attempt = send_inputs(&releases);
    Err(incomplete_input_error(
        "click_mouse",
        inserted,
        inputs.len(),
    ))
}

pub(super) fn send_drag_press<F>(
    button: MouseButton,
    modifiers: &[Key],
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
    let inserted = send_inputs(&inputs)?;
    if inserted == inputs.len() {
        Ok(())
    } else {
        let releases = pointer_release_inputs(button, modifiers);
        let _release_attempt = send_inputs(&releases);
        Err(incomplete_input_error("drag_mouse", inserted, inputs.len()))
    }
}

pub(super) fn send_drag_release<F>(
    button: MouseButton,
    modifiers: &[Key],
    mut validate_target: F,
) -> Result<(), PlatformError>
where
    F: FnMut() -> Result<(), PlatformError>,
{
    let releases = pointer_release_inputs(button, modifiers);
    if let Err(error) = validate_target() {
        // Releasing an already-held button is bounded cleanup, not a new action. Attempt it even
        // when revalidation fails so a desktop transition cannot strand a logical button-down.
        let _release_attempt = send_inputs(&releases);
        return Err(error);
    }
    let inserted = send_inputs(&releases)?;
    if inserted == releases.len() {
        Ok(())
    } else {
        let _release_attempt = send_inputs(&releases[inserted.min(releases.len())..]);
        Err(incomplete_input_error(
            "drag_mouse",
            inserted,
            releases.len(),
        ))
    }
}

pub(super) fn send_scroll<F>(
    delta_x: i32,
    delta_y: i32,
    mut validate_target: F,
) -> Result<(), PlatformError>
where
    F: FnMut() -> Result<(), PlatformError>,
{
    let inputs = scroll_inputs(delta_x, delta_y);
    validate_target()?;
    let inserted = send_inputs(&inputs)?;
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

fn send_inputs(inputs: &[INPUT]) -> Result<usize, PlatformError> {
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
