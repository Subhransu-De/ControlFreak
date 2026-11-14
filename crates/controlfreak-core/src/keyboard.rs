use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use crate::{ActionObservation, ObservationOptions};

/// Declares every key once, so the enum, the canonical names and `Key::ALL` cannot drift apart.
macro_rules! keys {
    ($($variant:ident => $name:literal),* $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Key {
            $($variant,)*
        }

        impl Key {
            /// Every key, in the order documented to clients.
            pub const ALL: &'static [Self] = &[$(Self::$variant,)*];

            /// The stable `snake_case` name used in schemas and serialized results.
            #[must_use]
            pub const fn canonical_name(self) -> &'static str {
                match self {
                    $(Self::$variant => $name,)*
                }
            }
        }
    };
}

keys! {
    Ctrl => "ctrl",
    Alt => "alt",
    Shift => "shift",
    Win => "win",
    Enter => "enter",
    Tab => "tab",
    Escape => "escape",
    Space => "space",
    Backspace => "backspace",
    Delete => "delete",
    Insert => "insert",
    Home => "home",
    End => "end",
    PageUp => "page_up",
    PageDown => "page_down",
    ArrowUp => "arrow_up",
    ArrowDown => "arrow_down",
    ArrowLeft => "arrow_left",
    ArrowRight => "arrow_right",
    A => "a",
    B => "b",
    C => "c",
    D => "d",
    E => "e",
    F => "f",
    G => "g",
    H => "h",
    I => "i",
    J => "j",
    K => "k",
    L => "l",
    M => "m",
    N => "n",
    O => "o",
    P => "p",
    Q => "q",
    R => "r",
    S => "s",
    T => "t",
    U => "u",
    V => "v",
    W => "w",
    X => "x",
    Y => "y",
    Z => "z",
    Digit0 => "0",
    Digit1 => "1",
    Digit2 => "2",
    Digit3 => "3",
    Digit4 => "4",
    Digit5 => "5",
    Digit6 => "6",
    Digit7 => "7",
    Digit8 => "8",
    Digit9 => "9",
    F1 => "f1",
    F2 => "f2",
    F3 => "f3",
    F4 => "f4",
    F5 => "f5",
    F6 => "f6",
    F7 => "f7",
    F8 => "f8",
    F9 => "f9",
    F10 => "f10",
    F11 => "f11",
    F12 => "f12",
    Comma => "comma",
    Period => "period",
    Slash => "slash",
    Semicolon => "semicolon",
    Quote => "quote",
    LeftBracket => "left_bracket",
    RightBracket => "right_bracket",
    Backslash => "backslash",
    Minus => "minus",
    Equals => "equals",
    Backtick => "backtick",
    Numpad0 => "numpad_0",
    Numpad1 => "numpad_1",
    Numpad2 => "numpad_2",
    Numpad3 => "numpad_3",
    Numpad4 => "numpad_4",
    Numpad5 => "numpad_5",
    Numpad6 => "numpad_6",
    Numpad7 => "numpad_7",
    Numpad8 => "numpad_8",
    Numpad9 => "numpad_9",
    NumpadMultiply => "numpad_multiply",
    NumpadAdd => "numpad_add",
    NumpadSubtract => "numpad_subtract",
    NumpadDecimal => "numpad_decimal",
    NumpadDivide => "numpad_divide",
    NumLock => "num_lock",
    PrintScreen => "print_screen",
    CapsLock => "caps_lock",
    ScrollLock => "scroll_lock",
    Pause => "pause",
    Menu => "menu",
    VolumeMute => "volume_mute",
    VolumeDown => "volume_down",
    VolumeUp => "volume_up",
    MediaNext => "media_next",
    MediaPrevious => "media_previous",
    MediaStop => "media_stop",
    MediaPlayPause => "media_play_pause",
}

/// Strips separators and case, so `Page-Up`, `page_up` and `PAGEUP` compare equal.
fn normalize(value: &str) -> String {
    value
        .chars()
        .filter(|character| !matches!(character, '_' | '-' | ' '))
        .flat_map(char::to_lowercase)
        .collect()
}

/// Single punctuation characters name the physical key that types them on a US layout. They are
/// matched before normalization, which would otherwise erase `-`.
fn symbol_key(value: &str) -> Option<Key> {
    Some(match value {
        " " => Key::Space,
        "," => Key::Comma,
        "." => Key::Period,
        "/" => Key::Slash,
        ";" => Key::Semicolon,
        "'" => Key::Quote,
        "[" => Key::LeftBracket,
        "]" => Key::RightBracket,
        "\\" => Key::Backslash,
        "-" => Key::Minus,
        "=" => Key::Equals,
        "`" => Key::Backtick,
        _ => return None,
    })
}

/// Alternative spellings. `normalized` has no separators and is lowercase.
fn alias_key(normalized: &str) -> Option<Key> {
    Some(match normalized {
        "control" => Key::Ctrl,
        "windows" | "meta" | "super" | "cmd" => Key::Win,
        "return" => Key::Enter,
        "esc" => Key::Escape,
        "spacebar" => Key::Space,
        "bksp" => Key::Backspace,
        "del" => Key::Delete,
        "ins" => Key::Insert,
        "pgup" => Key::PageUp,
        "pgdn" => Key::PageDown,
        "up" => Key::ArrowUp,
        "down" => Key::ArrowDown,
        "left" => Key::ArrowLeft,
        "right" => Key::ArrowRight,
        "dot" | "fullstop" => Key::Period,
        "forwardslash" => Key::Slash,
        "apostrophe" | "singlequote" => Key::Quote,
        "openbracket" | "leftsquarebracket" => Key::LeftBracket,
        "closebracket" | "rightsquarebracket" => Key::RightBracket,
        "dash" | "hyphen" => Key::Minus,
        "equal" => Key::Equals,
        "grave" | "backquote" | "tilde" => Key::Backtick,
        "numpadplus" => Key::NumpadAdd,
        "numpadminus" => Key::NumpadSubtract,
        "numpadstar" | "numpadasterisk" => Key::NumpadMultiply,
        "numpaddot" => Key::NumpadDecimal,
        "numpadslash" => Key::NumpadDivide,
        "numlk" => Key::NumLock,
        "printscr" | "prtsc" | "prtscn" => Key::PrintScreen,
        "capslk" | "capital" => Key::CapsLock,
        "scrolllk" => Key::ScrollLock,
        "break" => Key::Pause,
        "apps" | "contextmenu" | "application" => Key::Menu,
        "mute" => Key::VolumeMute,
        "mediaplay" | "playpause" => Key::MediaPlayPause,
        "nexttrack" => Key::MediaNext,
        "previoustrack" | "prevtrack" | "mediaprev" => Key::MediaPrevious,
        _ => return None,
    })
}

impl Key {
    fn from_name(value: &str) -> Option<Self> {
        let trimmed = value.trim();
        if let Some(key) = symbol_key(value).or_else(|| symbol_key(trimmed)) {
            return Some(key);
        }
        let normalized = normalize(trimmed);
        Self::ALL
            .iter()
            .copied()
            .find(|key| normalize(key.canonical_name()) == normalized)
            .or_else(|| alias_key(&normalized))
    }

    /// The canonical name closest to an unrecognized spelling, if any is plausibly a typo.
    fn nearest_name(value: &str) -> Option<&'static str> {
        let normalized = normalize(value.trim());
        if normalized.is_empty() {
            return None;
        }
        Self::ALL
            .iter()
            .filter_map(|key| {
                let name = key.canonical_name();
                let candidate = normalize(name);
                // One-letter names are typo targets for nothing but themselves.
                if candidate.len() < 2 {
                    return None;
                }
                let distance = edit_distance(&normalized, &candidate);
                let contained = normalized.len() >= 3
                    && (candidate.contains(&normalized) || normalized.contains(&candidate));
                let limit = if candidate.len() <= 3 { 1 } else { 2 };
                (distance <= limit || contained).then_some((distance, name))
            })
            .min_by_key(|(distance, _)| *distance)
            .map(|(_, name)| name)
    }
}

fn edit_distance(left: &str, right: &str) -> usize {
    let right: Vec<char> = right.chars().collect();
    let mut row: Vec<usize> = (0..=right.len()).collect();
    for (index, left_char) in left.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = index + 1;
        for (column, right_char) in right.iter().enumerate() {
            let above = row[column + 1];
            row[column + 1] = (diagonal + usize::from(left_char != *right_char))
                .min(above + 1)
                .min(row[column] + 1);
            diagonal = above;
        }
    }
    row[right.len()]
}

impl Serialize for Key {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.canonical_name())
    }
}

impl<'de> Deserialize<'de> for Key {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::from_name(&value).ok_or_else(|| {
            let hint = Self::nearest_name(&value).map_or_else(
                || {
                    let names: Vec<&str> =
                        Self::ALL.iter().map(|key| key.canonical_name()).collect();
                    format!("valid names: {}", names.join(", "))
                },
                |name| format!("did you mean '{name}'?"),
            );
            de::Error::custom(format!(
                "unknown key '{value}'; key names are case-insensitive; {hint}"
            ))
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyChordRequest {
    pub keys: Vec<Key>,
    pub observation: ObservationOptions,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextInputRequest {
    pub text: String,
    pub observation: ObservationOptions,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyboardActionResult {
    pub observation: ActionObservation,
}

#[cfg(test)]
mod tests {
    use super::Key;

    fn parse(name: &str) -> Option<Key> {
        serde_json::from_value(serde_json::Value::String(name.to_owned())).ok()
    }

    #[test]
    fn key_names_are_case_insensitive_and_accept_common_aliases() {
        assert_eq!(serde_json::from_str::<Key>("\"CTRL\"").unwrap(), Key::Ctrl);
        assert_eq!(
            serde_json::from_str::<Key>("\"Page-Up\"").unwrap(),
            Key::PageUp
        );
        assert_eq!(serde_json::from_str::<Key>("\"f12\"").unwrap(), Key::F12);
        assert!(serde_json::from_str::<Key>("\"launch_browser\"").is_err());
    }

    #[test]
    fn every_canonical_name_round_trips_and_is_unique() {
        let mut seen = std::collections::HashSet::new();
        for key in Key::ALL {
            let name = key.canonical_name();
            assert!(seen.insert(name), "duplicate canonical name {name}");
            assert_eq!(parse(name), Some(*key), "{name}");
            assert_eq!(parse(&name.to_uppercase()), Some(*key), "{name}");
            assert_eq!(
                serde_json::to_value(key).unwrap(),
                serde_json::Value::String(name.to_owned())
            );
        }
    }

    #[test]
    fn punctuation_symbols_and_names_select_the_same_keys() {
        for (symbol, name, key) in [
            (",", "comma", Key::Comma),
            (".", "period", Key::Period),
            ("/", "slash", Key::Slash),
            (";", "semicolon", Key::Semicolon),
            ("'", "quote", Key::Quote),
            ("[", "left_bracket", Key::LeftBracket),
            ("]", "right_bracket", Key::RightBracket),
            ("\\", "backslash", Key::Backslash),
            ("-", "minus", Key::Minus),
            ("=", "equals", Key::Equals),
            ("`", "backtick", Key::Backtick),
        ] {
            assert_eq!(parse(symbol), Some(key), "{symbol}");
            assert_eq!(parse(name), Some(key), "{name}");
        }
        assert_eq!(parse(" "), Some(Key::Space));
        assert_eq!(parse("+"), None);
    }

    #[test]
    fn system_numpad_and_media_keys_and_direction_aliases_parse() {
        for (name, key) in [
            ("numpad_0", Key::Numpad0),
            ("Numpad9", Key::Numpad9),
            ("numpad-add", Key::NumpadAdd),
            ("print_screen", Key::PrintScreen),
            ("PrtSc", Key::PrintScreen),
            ("caps_lock", Key::CapsLock),
            ("scroll_lock", Key::ScrollLock),
            ("pause", Key::Pause),
            ("menu", Key::Menu),
            ("apps", Key::Menu),
            ("volume_up", Key::VolumeUp),
            ("media_play_pause", Key::MediaPlayPause),
            ("up", Key::ArrowUp),
            ("Down", Key::ArrowDown),
            ("left", Key::ArrowLeft),
            ("right", Key::ArrowRight),
            ("esc", Key::Escape),
            ("del", Key::Delete),
            ("return", Key::Enter),
        ] {
            assert_eq!(parse(name), Some(key), "{name}");
        }
    }

    #[test]
    fn unknown_key_errors_suggest_the_nearest_name_or_list_the_valid_ones() {
        let error = serde_json::from_str::<Key>("\"pagedwn\"")
            .unwrap_err()
            .to_string();
        assert!(error.contains("did you mean 'page_down'"), "{error}");
        let error = serde_json::from_str::<Key>("\"launch_browser\"")
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("valid names:") && error.contains("print_screen"),
            "{error}"
        );
    }
}
