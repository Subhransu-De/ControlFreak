use serde::{Deserialize, Deserializer, Serialize, de};

use crate::{ActionObservation, ObservationOptions};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Key {
    Ctrl,
    Alt,
    Shift,
    Win,
    Enter,
    Tab,
    Escape,
    Space,
    Backspace,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
    #[serde(rename = "0")]
    Digit0,
    #[serde(rename = "1")]
    Digit1,
    #[serde(rename = "2")]
    Digit2,
    #[serde(rename = "3")]
    Digit3,
    #[serde(rename = "4")]
    Digit4,
    #[serde(rename = "5")]
    Digit5,
    #[serde(rename = "6")]
    Digit6,
    #[serde(rename = "7")]
    Digit7,
    #[serde(rename = "8")]
    Digit8,
    #[serde(rename = "9")]
    Digit9,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
}

impl Key {
    fn from_name(value: &str) -> Option<Self> {
        let normalized: String = value
            .chars()
            .filter(|character| !matches!(character, '_' | '-' | ' '))
            .flat_map(char::to_lowercase)
            .collect();
        Some(match normalized.as_str() {
            "ctrl" | "control" => Self::Ctrl,
            "alt" => Self::Alt,
            "shift" => Self::Shift,
            "win" | "windows" => Self::Win,
            "enter" | "return" => Self::Enter,
            "tab" => Self::Tab,
            "escape" | "esc" => Self::Escape,
            "space" => Self::Space,
            "backspace" => Self::Backspace,
            "delete" | "del" => Self::Delete,
            "insert" | "ins" => Self::Insert,
            "home" => Self::Home,
            "end" => Self::End,
            "pageup" | "pgup" => Self::PageUp,
            "pagedown" | "pgdn" => Self::PageDown,
            "arrowup" => Self::ArrowUp,
            "arrowdown" => Self::ArrowDown,
            "arrowleft" => Self::ArrowLeft,
            "arrowright" => Self::ArrowRight,
            "a" => Self::A,
            "b" => Self::B,
            "c" => Self::C,
            "d" => Self::D,
            "e" => Self::E,
            "f" => Self::F,
            "g" => Self::G,
            "h" => Self::H,
            "i" => Self::I,
            "j" => Self::J,
            "k" => Self::K,
            "l" => Self::L,
            "m" => Self::M,
            "n" => Self::N,
            "o" => Self::O,
            "p" => Self::P,
            "q" => Self::Q,
            "r" => Self::R,
            "s" => Self::S,
            "t" => Self::T,
            "u" => Self::U,
            "v" => Self::V,
            "w" => Self::W,
            "x" => Self::X,
            "y" => Self::Y,
            "z" => Self::Z,
            "0" => Self::Digit0,
            "1" => Self::Digit1,
            "2" => Self::Digit2,
            "3" => Self::Digit3,
            "4" => Self::Digit4,
            "5" => Self::Digit5,
            "6" => Self::Digit6,
            "7" => Self::Digit7,
            "8" => Self::Digit8,
            "9" => Self::Digit9,
            "f1" => Self::F1,
            "f2" => Self::F2,
            "f3" => Self::F3,
            "f4" => Self::F4,
            "f5" => Self::F5,
            "f6" => Self::F6,
            "f7" => Self::F7,
            "f8" => Self::F8,
            "f9" => Self::F9,
            "f10" => Self::F10,
            "f11" => Self::F11,
            "f12" => Self::F12,
            _ => return None,
        })
    }
}

impl<'de> Deserialize<'de> for Key {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::from_name(&value).ok_or_else(|| {
            de::Error::custom(format!(
                "unknown key '{value}'; key names are case-insensitive"
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
}
