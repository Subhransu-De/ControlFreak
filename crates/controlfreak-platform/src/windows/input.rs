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
    let _timer = controlfreak_core::timing::start("move_cursor");
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
    send_releasing_native(operation, &inputs, control)
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
    for segment in text_segments(utf16) {
        match segment {
            TextSegment::Text(units) => {
                for batch in unicode_batches(units) {
                    control.check("type_text")?;
                    let mut inputs = Vec::with_capacity(batch.len() * 2);
                    for unit in batch {
                        inputs.push(unicode_input(*unit, false));
                        inputs.push(unicode_input(*unit, true));
                    }
                    validate_target()?;
                    send_releasing("type_text", &inputs, control, &mut send)?;
                    if batch.len() + 1 >= UNITS_PER_TEXT_BATCH {
                        thread::sleep(Duration::from_millis(2));
                    }
                }
            }
            TextSegment::Key(key) => {
                control.check("type_text")?;
                let inputs = [keyboard_input(key, false), keyboard_input(key, true)];
                validate_target()?;
                send_releasing("type_text", &inputs, control, &mut send)?;
            }
        }
    }
    Ok(())
}

/// One run of `type_text` input: characters sent as Unicode packets, or a key that must be
/// pressed as a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextSegment<'a> {
    Text(&'a [u16]),
    Key(Key),
}

/// Splits text so line breaks and tabs become Enter and Tab key presses. A Unicode packet for
/// U+000A, U+000D or U+0009 reaches a control as a character, and many controls ignore it
/// instead of submitting a form, inserting a line or moving focus. `\r\n` is one Enter; a lone
/// `\r` or `\n` is also one Enter.
fn text_segments(utf16: &[u16]) -> Vec<TextSegment<'_>> {
    const TAB: u16 = 0x09;
    const LINE_FEED: u16 = 0x0A;
    const CARRIAGE_RETURN: u16 = 0x0D;
    let mut segments = Vec::new();
    let mut start = 0;
    let mut index = 0;
    while index < utf16.len() {
        let key = match utf16[index] {
            TAB => Key::Tab,
            LINE_FEED | CARRIAGE_RETURN => Key::Enter,
            _ => {
                index += 1;
                continue;
            }
        };
        if start < index {
            segments.push(TextSegment::Text(&utf16[start..index]));
        }
        segments.push(TextSegment::Key(key));
        index += if utf16[index] == CARRIAGE_RETURN && utf16.get(index + 1) == Some(&LINE_FEED) {
            2
        } else {
            1
        };
        start = index;
    }
    if start < utf16.len() {
        segments.push(TextSegment::Text(&utf16[start..]));
    }
    segments
}

const UNITS_PER_TEXT_BATCH: usize = 50;

// Batches end before a high surrogate so a refusal between batches cannot leave half a character.
fn unicode_batches(utf16: &[u16]) -> impl Iterator<Item = &[u16]> {
    let mut rest = utf16;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let mut end = rest.len().min(UNITS_PER_TEXT_BATCH);
        if end < rest.len() && (0xD800..=0xDBFF).contains(&rest[end - 1]) {
            end -= 1;
        }
        let (batch, remaining) = rest.split_at(end);
        rest = remaining;
        Some(batch)
    })
}

/// The virtual key for `key` under the foreground window's keyboard layout.
fn virtual_key(key: Key) -> (VIRTUAL_KEY, bool) {
    virtual_key_with(key, native_layout_virtual_key)
}

/// The character a layout-dependent OEM key types without modifiers. Comma, period, minus and
/// plus keep one virtual key on every layout, so they need no lookup.
const fn oem_character(key: Key) -> Option<char> {
    Some(match key {
        Key::Semicolon => ';',
        Key::Slash => '/',
        Key::Backtick => '`',
        Key::LeftBracket => '[',
        Key::Backslash => '\\',
        Key::RightBracket => ']',
        Key::Quote => '\'',
        _ => return None,
    })
}

/// Virtual keys of the punctuation, numpad, system and media keys; zero for any other key, which
/// `every_key_has_its_own_virtual_key` rules out. The punctuation codes are the US ones.
const fn symbol_and_system_code(key: Key) -> u16 {
    match key {
        Key::Semicolon => 0xBA,
        Key::Slash => 0xBF,
        Key::Backtick => 0xC0,
        Key::LeftBracket => 0xDB,
        Key::Backslash => 0xDC,
        Key::RightBracket => 0xDD,
        Key::Quote => 0xDE,
        Key::Comma => 0xBC,
        Key::Period => 0xBE,
        Key::Minus => 0xBD,
        Key::Equals => 0xBB,
        Key::Numpad0 => 0x60,
        Key::Numpad1 => 0x61,
        Key::Numpad2 => 0x62,
        Key::Numpad3 => 0x63,
        Key::Numpad4 => 0x64,
        Key::Numpad5 => 0x65,
        Key::Numpad6 => 0x66,
        Key::Numpad7 => 0x67,
        Key::Numpad8 => 0x68,
        Key::Numpad9 => 0x69,
        Key::NumpadMultiply => 0x6A,
        Key::NumpadAdd => 0x6B,
        Key::NumpadSubtract => 0x6D,
        Key::NumpadDecimal => 0x6E,
        Key::NumpadDivide => 0x6F,
        Key::NumLock => 0x90,
        Key::PrintScreen => 0x2C,
        Key::CapsLock => 0x14,
        Key::ScrollLock => 0x91,
        Key::Pause => 0x13,
        Key::Menu => 0x5D,
        Key::VolumeMute => 0xAD,
        Key::VolumeDown => 0xAE,
        Key::VolumeUp => 0xAF,
        Key::MediaNext => 0xB0,
        Key::MediaPrevious => 0xB1,
        Key::MediaStop => 0xB2,
        Key::MediaPlayPause => 0xB3,
        _ => 0,
    }
}

/// Maps a key to a virtual key. `layout` answers which unshifted virtual key types a character on
/// the active layout. A named punctuation key means "the key that types this character", so a
/// layout that types it unshifted decides the code. A layout that cannot (it needs Shift or
/// `AltGr`, or lacks the character) falls back to the US virtual key, which is the same physical
/// key position, rather than sending a different symbol.
fn virtual_key_with(key: Key, layout: impl FnOnce(char) -> Option<u16>) -> (VIRTUAL_KEY, bool) {
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
        other => symbol_and_system_code(other),
    };
    let code = oem_character(key).and_then(layout).unwrap_or(code);
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
            | Key::NumpadDivide
            | Key::NumLock
            | Key::PrintScreen
            | Key::Menu
    );
    (VIRTUAL_KEY(code), extended)
}

/// The unshifted virtual key that types `character` on the foreground thread's layout, or `None`
/// if that layout needs a modifier for it or cannot type it.
fn native_layout_virtual_key(character: char) -> Option<u16> {
    use windows::Win32::UI::{
        Input::KeyboardAndMouse::{GetKeyboardLayout, VkKeyScanExW},
        WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId},
    };
    let unit = u16::try_from(u32::from(character)).ok()?;
    // SAFETY: These queries take a borrowed window handle and a value, and retain nothing. A null
    // foreground handle or a zero thread selects the calling thread's layout.
    let scan = unsafe {
        let thread = GetWindowThreadProcessId(GetForegroundWindow(), None);
        VkKeyScanExW(unit, GetKeyboardLayout(thread))
    };
    unshifted_virtual_key(scan)
}

/// Decodes a `VkKeyScan` result: -1 means no key; the high byte holds the Shift, Ctrl and Alt
/// state the character needs.
fn unshifted_virtual_key(scan: i16) -> Option<u16> {
    let [low, high] = scan.to_le_bytes();
    (scan != -1 && high == 0).then_some(u16::from(low))
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
    send_with_button_dwell_native("click_mouse", &inputs, control, validate_target)
}

/// How long a mouse button stays down. Events sent in one `SendInput` call carry the same
/// timestamp, and applications that act on the press (menus, drag sources, custom buttons)
/// can miss a button that is released in the same instant.
pub(super) const BUTTON_DWELL: Duration = Duration::from_millis(20);

/// Splits an input sequence after every mouse-button press, so the caller can pause with the
/// button held. Sequences with no press come back whole.
fn split_after_button_down(inputs: &[INPUT]) -> Vec<&[INPUT]> {
    let is_button_down = |input: &INPUT| {
        // SAFETY: INPUT values in these sequences are built locally with a matching union tag.
        input.r#type == INPUT_MOUSE
            && unsafe { input.Anonymous.mi.dwFlags }.0
                & (MOUSEEVENTF_LEFTDOWN.0 | MOUSEEVENTF_RIGHTDOWN.0 | MOUSEEVENTF_MIDDLEDOWN.0)
                != 0
    };
    let mut steps = Vec::new();
    let mut start = 0;
    for (index, input) in inputs.iter().enumerate() {
        if is_button_down(input) {
            steps.push(&inputs[start..=index]);
            start = index + 1;
        }
    }
    if start < inputs.len() {
        steps.push(&inputs[start..]);
    }
    steps
}

fn send_with_button_dwell_native(
    operation: &str,
    inputs: &[INPUT],
    control: &MutationControl,
    validate: impl FnMut() -> Result<(), PlatformError>,
) -> Result<(), PlatformError> {
    use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    send_with_button_dwell_if_idle(
        operation,
        inputs,
        control,
        |key| {
            // SAFETY: GetAsyncKeyState takes a virtual-key code and retains no references.
            unsafe { GetAsyncKeyState(key) < 0 }
        },
        validate,
        native_send_inputs,
        || control.wait(operation, BUTTON_DWELL),
    )
}

fn send_with_button_dwell_if_idle(
    operation: &str,
    inputs: &[INPUT],
    control: &MutationControl,
    is_down: impl FnMut(i32) -> bool,
    validate: impl FnMut() -> Result<(), PlatformError>,
    send: impl FnMut(&[INPUT]) -> Result<usize, PlatformError>,
    dwell: impl FnMut() -> Result<(), PlatformError>,
) -> Result<(), PlatformError> {
    control.check(operation)?;
    if any_requested_input_held(inputs, is_down) {
        return Err(PlatformError::OperationFailed {
            operation: operation.to_owned(),
            reason: "a requested key or mouse button is already held; release it before retrying"
                .to_owned(),
        });
    }
    send_with_button_dwell(operation, inputs, control, validate, send, dwell)
}

/// Sends `inputs` in steps with a pause after each button press. The caller validated the
/// target before the first step; `validate` runs again after every pause, immediately before
/// the next step, because that step holds new input (a second press of a double click) and the
/// foreground, destination or cursor may have changed during the dwell. Whatever the sent steps
/// left held (the button, and modifiers pressed before it) is released if a later step fails,
/// validation refuses or the pause is cancelled; that cleanup never contains a new press.
fn send_with_button_dwell(
    operation: &str,
    inputs: &[INPUT],
    control: &MutationControl,
    mut validate: impl FnMut() -> Result<(), PlatformError>,
    mut send: impl FnMut(&[INPUT]) -> Result<usize, PlatformError>,
    mut dwell: impl FnMut() -> Result<(), PlatformError>,
) -> Result<(), PlatformError> {
    control.check(operation)?;
    // If dispatch unwinds, release state is unknown until cleanup has acknowledged it.
    control.cleanup_status(controlfreak_core::CleanupStatus::Unknown);
    let steps = split_after_button_down(inputs);
    let mut accepted = 0;
    for (index, step) in steps.iter().enumerate() {
        let delivered = match dispatch_inputs(step, control, &mut send) {
            Ok(count) => count,
            Err(error) => {
                if accepted > 0 {
                    release_sent(&inputs[..accepted], control, &mut send);
                }
                return Err(error);
            }
        };
        accepted += delivered;
        if delivered != step.len() {
            release_sent(&inputs[..accepted], control, &mut send);
            return Err(incomplete_input_error(operation, accepted, inputs.len()));
        }
        if index + 1 < steps.len()
            && let Err(error) = control
                .check(operation)
                .and_then(|()| dwell())
                .and_then(|()| validate())
        {
            release_sent(&inputs[..accepted], control, &mut send);
            return Err(error);
        }
    }
    control.cleanup_status(controlfreak_core::CleanupStatus::NotNeeded);
    Ok(())
}

fn release_sent(
    acknowledged: &[INPUT],
    control: &MutationControl,
    send: impl FnOnce(&[INPUT]) -> Result<usize, PlatformError>,
) {
    let owned = owned_releases(acknowledged);
    if owned.is_empty() {
        control.cleanup_status(controlfreak_core::CleanupStatus::NotNeeded);
    } else {
        send_cleanup(&owned, control, send);
    }
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
    send_releasing_native("drag_mouse", &inputs, control)
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

fn send_releasing_native(
    operation: &str,
    inputs: &[INPUT],
    control: &MutationControl,
) -> Result<(), PlatformError> {
    use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    send_releasing_if_idle(
        operation,
        inputs,
        control,
        |key| {
            // SAFETY: GetAsyncKeyState takes a virtual-key code and retains no references.
            unsafe { GetAsyncKeyState(key) < 0 }
        },
        native_send_inputs,
    )
}

fn send_releasing_if_idle(
    operation: &str,
    inputs: &[INPUT],
    control: &MutationControl,
    is_down: impl FnMut(i32) -> bool,
    send: impl FnMut(&[INPUT]) -> Result<usize, PlatformError>,
) -> Result<(), PlatformError> {
    control.check(operation)?;
    if any_requested_input_held(inputs, is_down) {
        return Err(PlatformError::OperationFailed {
            operation: operation.to_owned(),
            reason: "a requested key or mouse button is already held; release it before retrying"
                .to_owned(),
        });
    }
    send_releasing(operation, inputs, control, send)
}

fn native_send_inputs(inputs: &[INPUT]) -> Result<usize, PlatformError> {
    let _timer = controlfreak_core::timing::start("send_input");
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
    fn held_input_refusal_reports_no_dispatch_or_cleanup() {
        for inputs in [
            key_chord_inputs(&[Key::Ctrl, Key::C]),
            click_inputs(MouseButton::Left, 1, &[]),
        ] {
            let control = MutationControl::default();
            let error = send_releasing_if_idle(
                "test",
                &inputs,
                &control,
                |_| true,
                |_| panic!("held input must not dispatch or release user input"),
            )
            .unwrap_err();
            assert!(error.to_string().contains("release it before retrying"));
            assert!(!error.to_string().contains("integrity"));
            assert_eq!(control.progress(), MutationControl::default().progress());
        }
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
                    // The first batch stops one unit early instead of ending on a high surrogate.
                    let earlier_events = [0, 98, 198][usize::try_from(failed_batch).unwrap()];
                    assert_eq!(progress.sent_events, earlier_events + accepted as u64);
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
    fn text_batches_never_split_a_surrogate_pair() {
        for prefix in 0..60 {
            let text: Vec<u16> = format!("{}\u{1f980}\u{1f980}", "a".repeat(prefix))
                .encode_utf16()
                .collect();
            let batches: Vec<&[u16]> = unicode_batches(&text).collect();
            assert_eq!(batches.concat(), text);
            assert!(batches.iter().all(|batch| {
                !batch.is_empty()
                    && batch.len() <= UNITS_PER_TEXT_BATCH
                    && !(0xD800..=0xDBFF).contains(batch.last().unwrap())
            }));
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

    fn utf16(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    #[test]
    fn line_breaks_and_tabs_become_enter_and_tab_key_presses() {
        let text = utf16("a\r\nb\n\tc\r");
        let segments = text_segments(&text);
        assert_eq!(
            segments,
            [
                TextSegment::Text(&text[0..1]),
                TextSegment::Key(Key::Enter),
                TextSegment::Text(&text[3..4]),
                TextSegment::Key(Key::Enter),
                TextSegment::Key(Key::Tab),
                TextSegment::Text(&text[6..7]),
                TextSegment::Key(Key::Enter),
            ]
        );
        assert!(text_segments(&[]).is_empty());
        let plain = utf16("no controls \u{1f980}");
        assert_eq!(text_segments(&plain), [TextSegment::Text(&plain)]);
        // Consecutive breaks are separate presses, and a CR LF pair is one.
        let blank = utf16("\n\n\r\n\r\r");
        assert_eq!(text_segments(&blank), [TextSegment::Key(Key::Enter); 5]);
    }

    #[test]
    fn typed_text_sends_return_and_tab_as_virtual_keys_between_unicode_packets() {
        let control = MutationControl::default();
        let mut sent: Vec<Vec<(u16, u16, bool)>> = Vec::new();
        let mut validations = 0;
        send_unicode_text_with(
            &utf16("ab\r\n\tc"),
            &control,
            || {
                validations += 1;
                Ok(())
            },
            |inputs| {
                sent.push(
                    inputs
                        .iter()
                        .map(|input| {
                            // SAFETY: Every INPUT here was built by this module as a keyboard event.
                            let key = unsafe { input.Anonymous.ki };
                            (key.wVk.0, key.wScan, key.dwFlags.contains(KEYEVENTF_KEYUP))
                        })
                        .collect(),
                );
                Ok(inputs.len())
            },
        )
        .unwrap();
        // Text, Enter, Tab, text: each is validated and sent as its own batch.
        assert_eq!(validations, 4);
        assert_eq!(sent.len(), 4);
        assert_eq!(
            sent[0],
            [(0, 97, false), (0, 97, true), (0, 98, false), (0, 98, true)]
        );
        assert_eq!(sent[1], [(0x0D, 0, false), (0x0D, 0, true)]);
        assert_eq!(sent[2], [(0x09, 0, false), (0x09, 0, true)]);
        assert_eq!(sent[3], [(0, 99, false), (0, 99, true)]);
        assert_eq!(control.progress().sent_events, 10);
    }

    #[test]
    fn a_refused_revalidation_before_a_line_break_keeps_the_earlier_text() {
        let control = MutationControl::default();
        let mut validations = 0;
        let mut batches = 0;
        let result = send_unicode_text_with(
            &utf16("ab\ncd"),
            &control,
            || {
                validations += 1;
                if validations == 2 {
                    Err(failure())
                } else {
                    Ok(())
                }
            },
            |inputs| {
                batches += 1;
                Ok(inputs.len())
            },
        );
        assert!(result.is_err());
        assert_eq!(batches, 1);
        assert_eq!(control.progress().sent_events, 4);
    }

    #[test]
    fn punctuation_keys_use_the_layout_when_it_types_them_unshifted() {
        for (key, character, us) in [
            (Key::Semicolon, ';', 0xBA),
            (Key::Slash, '/', 0xBF),
            (Key::Backtick, '`', 0xC0),
            (Key::LeftBracket, '[', 0xDB),
            (Key::Backslash, '\\', 0xDC),
            (Key::RightBracket, ']', 0xDD),
            (Key::Quote, '\'', 0xDE),
        ] {
            // Without a layout answer the US key position is used.
            assert_eq!(virtual_key_with(key, |_| None), (VIRTUAL_KEY(us), false));
            // A layout that moves the character to another key wins.
            let (vk, extended) = virtual_key_with(key, |asked| {
                assert_eq!(asked, character);
                Some(0x41)
            });
            assert_eq!((vk.0, extended), (0x41, false));
        }
        // Keys that never move do not consult the layout at all.
        for (key, vk) in [
            (Key::Comma, 0xBC),
            (Key::Period, 0xBE),
            (Key::Minus, 0xBD),
            (Key::Equals, 0xBB),
        ] {
            assert_eq!(
                virtual_key_with(key, |_| panic!("layout lookup is not needed")),
                (VIRTUAL_KEY(vk), false)
            );
        }
    }

    #[test]
    fn layout_answers_that_need_a_modifier_or_no_key_are_not_used() {
        assert_eq!(unshifted_virtual_key(0x00BA), Some(0xBA));
        assert_eq!(unshifted_virtual_key(0x01BA), None, "needs Shift");
        assert_eq!(unshifted_virtual_key(0x06BA), None, "needs AltGr");
        assert_eq!(unshifted_virtual_key(-1), None, "not on this layout");
    }

    #[test]
    fn numpad_system_and_media_keys_map_to_their_virtual_keys() {
        for (key, vk, extended) in [
            (Key::Numpad0, 0x60, false),
            (Key::Numpad9, 0x69, false),
            (Key::NumpadMultiply, 0x6A, false),
            (Key::NumpadAdd, 0x6B, false),
            (Key::NumpadSubtract, 0x6D, false),
            (Key::NumpadDecimal, 0x6E, false),
            (Key::NumpadDivide, 0x6F, true),
            (Key::NumLock, 0x90, true),
            (Key::PrintScreen, 0x2C, true),
            (Key::CapsLock, 0x14, false),
            (Key::ScrollLock, 0x91, false),
            (Key::Pause, 0x13, false),
            (Key::Menu, 0x5D, true),
            (Key::VolumeMute, 0xAD, false),
            (Key::VolumeDown, 0xAE, false),
            (Key::VolumeUp, 0xAF, false),
            (Key::MediaNext, 0xB0, false),
            (Key::MediaPrevious, 0xB1, false),
            (Key::MediaStop, 0xB2, false),
            (Key::MediaPlayPause, 0xB3, false),
        ] {
            assert_eq!(
                virtual_key_with(key, |_| panic!("not a layout-dependent key")),
                (VIRTUAL_KEY(vk), extended),
                "{key:?}"
            );
        }
    }

    #[test]
    fn every_key_has_its_own_virtual_key() {
        let mut seen = std::collections::HashMap::new();
        for key in Key::ALL.iter().copied() {
            let (vk, _) = virtual_key_with(key, |_| None);
            assert_ne!(vk.0, 0, "{key:?}");
            if let Some(previous) = seen.insert(vk.0, key) {
                panic!("{previous:?} and {key:?} share virtual key {:#x}", vk.0);
            }
        }
    }

    fn event_kinds(inputs: &[INPUT]) -> Vec<&'static str> {
        inputs
            .iter()
            .map(|input| {
                if input.r#type == INPUT_MOUSE {
                    // SAFETY: Every INPUT here was built by this module with a matching tag.
                    let flags = unsafe { input.Anonymous.mi.dwFlags };
                    if flags.contains(MOUSEEVENTF_LEFTDOWN) {
                        "down"
                    } else {
                        "up"
                    }
                } else {
                    // SAFETY: As above, for a keyboard event.
                    let up = unsafe { input.Anonymous.ki.dwFlags }.contains(KEYEVENTF_KEYUP);
                    if up { "key-up" } else { "key-down" }
                }
            })
            .collect()
    }

    #[test]
    fn clicks_pause_with_the_button_held_and_not_after_the_last_event() {
        let inputs = click_inputs(MouseButton::Left, 2, &[Key::Ctrl]);
        let steps = split_after_button_down(&inputs);
        assert_eq!(
            steps
                .iter()
                .map(|step| event_kinds(step))
                .collect::<Vec<_>>(),
            [
                vec!["key-down", "down"],
                vec!["up", "down"],
                vec!["up", "key-up"]
            ]
        );
        let control = MutationControl::default();
        let events_cell = std::cell::RefCell::new(Vec::<String>::new());
        send_with_button_dwell(
            "click_mouse",
            &inputs,
            &control,
            || Ok(()),
            |batch| {
                events_cell
                    .borrow_mut()
                    .push(format!("send:{}", batch.len()));
                Ok(batch.len())
            },
            || {
                events_cell.borrow_mut().push("dwell".to_owned());
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            *events_cell.borrow(),
            ["send:2", "dwell", "send:2", "dwell", "send:2"]
        );
        assert_eq!(control.progress().cleanup, CleanupStatus::NotNeeded);
        // No press, no pause.
        let keys = key_chord_inputs(&[Key::Ctrl, Key::C]);
        assert_eq!(split_after_button_down(&keys).len(), 1);
    }

    #[test]
    fn a_failed_or_cancelled_pause_releases_the_button_and_modifiers_that_were_sent() {
        let inputs = click_inputs(MouseButton::Left, 1, &[Key::Ctrl]);
        let control = MutationControl::default();
        let mut sent = Vec::new();
        let result = send_with_button_dwell(
            "click_mouse",
            &inputs,
            &control,
            || Ok(()),
            |batch| {
                sent.push(event_kinds(batch));
                Ok(batch.len())
            },
            || Err(failure()),
        );
        assert!(result.is_err());
        // The release of everything still held follows the first step, in reverse order.
        assert_eq!(sent, [vec!["key-down", "down"], vec!["up", "key-up"]]);
        assert_eq!(control.progress().cleanup, CleanupStatus::Succeeded);

        // A click cancelled before anything was sent has nothing to release.
        let control = MutationControl::default();
        control.cancel();
        assert!(
            send_with_button_dwell(
                "click_mouse",
                &inputs,
                &control,
                || Ok(()),
                |_| panic!("cancelled click must not dispatch"),
                || Ok(()),
            )
            .is_err()
        );
    }

    #[test]
    fn a_partially_accepted_click_step_releases_only_what_was_accepted() {
        let inputs = click_inputs(MouseButton::Left, 1, &[Key::Ctrl]);
        let control = MutationControl::default();
        let mut calls = Vec::new();
        let result = send_with_button_dwell(
            "click_mouse",
            &inputs,
            &control,
            || Ok(()),
            |batch| {
                calls.push(event_kinds(batch));
                // The first call accepts only the modifier press.
                Ok(if calls.len() == 1 { 1 } else { batch.len() })
            },
            || Ok(()),
        );
        assert!(result.is_err());
        assert_eq!(calls, [vec!["key-down", "down"], vec!["key-up"]]);
        assert_eq!(control.progress().cleanup, CleanupStatus::Succeeded);
    }

    #[test]
    fn held_buttons_refuse_a_click_before_any_dispatch() {
        let inputs = click_inputs(MouseButton::Left, 1, &[]);
        let control = MutationControl::default();
        let error = send_with_button_dwell_if_idle(
            "click_mouse",
            &inputs,
            &control,
            |_| true,
            || Ok(()),
            |_| panic!("held button must not dispatch"),
            || Ok(()),
        )
        .unwrap_err();
        assert!(error.to_string().contains("already held"));
    }

    #[test]
    fn a_target_that_stops_being_valid_during_the_dwell_gets_no_second_press() {
        // A double click is three batches. Validation turns false during the first dwell.
        let inputs = click_inputs(MouseButton::Left, 2, &[Key::Ctrl]);
        let control = MutationControl::default();
        let mut sent = Vec::new();
        let mut validations = 0;
        let result = send_with_button_dwell(
            "click_mouse",
            &inputs,
            &control,
            || {
                validations += 1;
                Err(PlatformError::TargetInvalidated {
                    reason: "the foreground changed during the dwell".into(),
                })
            },
            |batch| {
                sent.push(event_kinds(batch));
                Ok(batch.len())
            },
            || Ok(()),
        );
        assert!(matches!(
            result,
            Err(PlatformError::TargetInvalidated { .. })
        ));
        assert_eq!(validations, 1);
        // Only the acknowledged press and modifier are released; no new press follows.
        assert_eq!(sent, [vec!["key-down", "down"], vec!["up", "key-up"]]);
        assert_eq!(control.progress().cleanup, CleanupStatus::Succeeded);
    }

    #[test]
    fn every_batch_with_new_input_is_validated_right_before_it_is_sent() {
        let inputs = click_inputs(MouseButton::Left, 3, &[]);
        let control = MutationControl::default();
        let log = std::cell::RefCell::new(Vec::<&str>::new());
        send_with_button_dwell(
            "click_mouse",
            &inputs,
            &control,
            || {
                log.borrow_mut().push("validate");
                Ok(())
            },
            |batch| {
                log.borrow_mut().push("send");
                Ok(batch.len())
            },
            || {
                log.borrow_mut().push("dwell");
                Ok(())
            },
        )
        .unwrap();
        // down | up down | up down | up (four batches): the caller validated the first batch itself.
        assert_eq!(
            *log.borrow(),
            [
                "send", "dwell", "validate", "send", "dwell", "validate", "send", "dwell",
                "validate", "send"
            ]
        );
    }
}
