use serde::Deserialize;
use std::collections::HashSet;

const BUILTIN: &[(&str, &str)] = &[
    ("single", include_str!("../scenes/single.toml")),
    ("with-dialog", include_str!("../scenes/with-dialog.toml")),
    ("covered", include_str!("../scenes/covered.toml")),
    (
        "two-processes",
        include_str!("../scenes/two-processes.toml"),
    ),
    ("busy-app", include_str!("../scenes/busy-app.toml")),
    ("clipped", include_str!("../scenes/clipped.toml")),
    ("with-popup", include_str!("../scenes/with-popup.toml")),
    (
        "empty-desktop",
        include_str!("../scenes/empty-desktop.toml"),
    ),
];

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Scene {
    description: String,
    #[serde(rename = "window")]
    pub(crate) windows: Vec<WindowSpec>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent fixture switch of the scene file, not a state"
)]
pub struct WindowSpec {
    pub key: String,
    #[serde(default = "default_color")]
    pub(crate) color: String,
    #[serde(default = "default_position")]
    pub(crate) position: [i32; 2],
    #[serde(default)]
    pub(crate) topmost: bool,
    #[serde(default)]
    pub(crate) dialog: bool,
    #[serde(default = "default_lifetime")]
    pub(crate) lifetime_secs: u32,
    /// Nonzero: the fixture stays busy this long when input arrives, then takes every queued
    /// keystroke before translating any, like a busy UI framework.
    #[serde(default)]
    pub(crate) busy_drain_ms: u32,
    /// The fixture confines the pointer to the left half of its client area while it is active.
    #[serde(default)]
    pub(crate) clip_cursor: bool,
    /// A press of the fixture's button opens an untitled popup that takes the foreground.
    #[serde(default)]
    pub(crate) popup_on_ping: bool,
}

fn default_color() -> String {
    "2E8B57".into()
}

const fn default_position() -> [i32; 2] {
    [10, 140]
}

const fn default_lifetime() -> u32 {
    240
}

impl Scene {
    fn parse(text: &str) -> Result<Self, String> {
        let scene: Self = toml::from_str(text).map_err(|error| error.to_string())?;
        scene.validate()?;
        Ok(scene)
    }

    pub(crate) fn builtin(name: &str) -> Result<Self, String> {
        let (_, text) = BUILTIN
            .iter()
            .find(|(known, _)| *known == name)
            .ok_or_else(|| format!("no scene named {name:?}"))?;
        Self::parse(text).map_err(|error| format!("scene {name}: {error}"))
    }

    #[cfg(test)]
    fn names() -> impl Iterator<Item = &'static str> {
        BUILTIN.iter().map(|(name, _)| *name)
    }

    fn validate(&self) -> Result<(), String> {
        if self.windows.is_empty() {
            return Err("a scene needs at least one [[window]]".into());
        }
        let mut keys = HashSet::new();
        for window in &self.windows {
            let valid_key = !window.key.is_empty()
                && window
                    .key
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
            if !valid_key {
                return Err(format!(
                    "window key {:?} must be lowercase letters, digits and dashes",
                    window.key
                ));
            }
            if !keys.insert(window.key.as_str()) {
                return Err(format!("window key {:?} is used twice", window.key));
            }
            if window.color.len() != 6 || !window.color.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(format!(
                    "window {}: color {:?} is not RRGGBB",
                    window.key, window.color
                ));
            }
            if !(10..=600).contains(&window.lifetime_secs) {
                return Err(format!(
                    "window {}: lifetime_secs must be 10..=600",
                    window.key
                ));
            }
            if window.busy_drain_ms > 1_000 {
                return Err(format!(
                    "window {}: busy_drain_ms must be at most 1000",
                    window.key
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_scene_parses_and_is_valid() {
        for name in Scene::names() {
            let scene = Scene::builtin(name).unwrap_or_else(|error| panic!("{error}"));
            assert!(!scene.description.is_empty(), "{name}");
        }
    }

    #[test]
    fn defaults_fill_missing_fields() {
        let scene = Scene::parse("description = \"d\"\n[[window]]\nkey = \"a\"\n").unwrap();
        let window = &scene.windows[0];
        assert_eq!(window.color, "2E8B57");
        assert_eq!(window.position, [10, 140]);
        assert!(!window.topmost && !window.dialog);
        assert_eq!(window.lifetime_secs, 240);
        assert_eq!(window.busy_drain_ms, 0);
        assert!(!window.clip_cursor);
        assert!(!window.popup_on_ping);
    }

    #[test]
    fn invalid_scenes_are_refused_with_a_reason() {
        let cases = [
            ("description = \"d\"\nwindow = []\n", "at least one"),
            (
                "description = \"d\"\n[[window]]\nkey = \"a\"\n[[window]]\nkey = \"a\"\n",
                "used twice",
            ),
            (
                "description = \"d\"\n[[window]]\nkey = \"Ledger\"\n",
                "lowercase",
            ),
            (
                "description = \"d\"\n[[window]]\nkey = \"a\"\ncolor = \"green\"\n",
                "RRGGBB",
            ),
            (
                "description = \"d\"\n[[window]]\nkey = \"a\"\nlifetime_secs = 0\n",
                "lifetime",
            ),
            (
                "description = \"d\"\n[[window]]\nkey = \"a\"\nbusy_drain_ms = 5000\n",
                "busy_drain_ms",
            ),
            (
                "description = \"d\"\n[[window]]\nkey = \"a\"\nsize = 3\n",
                "unknown field",
            ),
        ];
        for (text, wanted) in cases {
            let error = Scene::parse(text).unwrap_err();
            assert!(
                error.contains(wanted),
                "{error:?} should mention {wanted:?}"
            );
        }
        assert!(Scene::builtin("missing").is_err());
    }
}
