use std::collections::HashMap;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Regions(HashMap<String, [i32; 4]>);

impl Regions {
    pub(crate) fn get(&self, name: &str) -> Option<[i32; 4]> {
        self.0.get(name).copied()
    }

    pub(crate) fn center(&self, name: &str) -> Option<(i32, i32)> {
        let [left, top, right, bottom] = self.get(name)?;
        Some((left.midpoint(right), top.midpoint(bottom)))
    }

    pub(crate) fn contains(&self, name: &str, point: (i32, i32)) -> bool {
        self.get(name).is_some_and(|[left, top, right, bottom]| {
            point.0 >= left && point.0 < right && point.1 >= top && point.1 < bottom
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Ready(Regions),
    Activate {
        role: String,
        state: u32,
    },
    KeyDown {
        role: String,
        vk: u16,
    },
    KeyUp {
        role: String,
        vk: u16,
    },
    Char {
        role: String,
        ch: char,
    },
    /// A UTF-16 surrogate that did not arrive with its other half: a high surrogate followed by
    /// another character, or the end of the log, or a low surrogate with no high one before it.
    /// `typed` shows it as U+FFFD, so a split or dropped half never disappears from the text.
    Unpaired {
        role: String,
        unit: u16,
    },
    Mouse {
        role: String,
        action: String,
        x: i32,
        y: i32,
    },
    Button {
        role: String,
        name: String,
    },
    Other(String),
}

/// One log line. A `char` line holds one UTF-16 unit of a `WM_CHAR`, so it becomes an event only
/// once `parse_log` has seen whether a surrogate has its other half.
enum Line {
    Event(Event),
    Unit { role: String, unit: u16 },
}

impl Line {
    fn parse(line: &str) -> Self {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if let [role, "char", unit] = parts.as_slice()
            && let Some(unit) = unit
                .strip_prefix("U+")
                .and_then(|hex| u16::from_str_radix(hex, 16).ok())
        {
            return Self::Unit {
                role: (*role).to_owned(),
                unit,
            };
        }
        Self::Event(Event::parse(line))
    }
}

impl Event {
    fn parse(line: &str) -> Self {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let parsed = match parts.as_slice() {
            ["ready", fields @ ..] => Some(Self::Ready(parse_regions(fields))),
            [role, "activate", state] => state.parse().ok().map(|state| Self::Activate {
                role: (*role).to_owned(),
                state,
            }),
            [role, "keydown", vk] => parse_vk(vk).map(|vk| Self::KeyDown {
                role: (*role).to_owned(),
                vk,
            }),
            [role, "keyup", vk] => parse_vk(vk).map(|vk| Self::KeyUp {
                role: (*role).to_owned(),
                vk,
            }),
            [role, "mouse", action, x, y] => match (x.parse(), y.parse()) {
                (Ok(x), Ok(y)) => Some(Self::Mouse {
                    role: (*role).to_owned(),
                    action: (*action).to_owned(),
                    x,
                    y,
                }),
                _ => None,
            },
            [role, "button", name] => Some(Self::Button {
                role: (*role).to_owned(),
                name: (*name).to_owned(),
            }),
            _ => None,
        };
        parsed.unwrap_or_else(|| Self::Other(line.to_owned()))
    }

    pub(crate) fn is_input(&self) -> bool {
        matches!(
            self,
            Self::KeyDown { .. }
                | Self::KeyUp { .. }
                | Self::Char { .. }
                | Self::Unpaired { .. }
                | Self::Mouse { .. }
        )
    }
}

fn parse_vk(text: &str) -> Option<u16> {
    u16::from_str_radix(text.strip_prefix("0x")?, 16).ok()
}

fn parse_regions(fields: &[&str]) -> Regions {
    let mut regions = HashMap::new();
    for field in fields {
        let Some((name, values)) = field.split_once('=') else {
            continue;
        };
        let numbers: Vec<i32> = values.split(',').filter_map(|v| v.parse().ok()).collect();
        if let Ok(rect) = <[i32; 4]>::try_from(numbers) {
            regions.insert(name.to_owned(), rect);
        }
    }
    Regions(regions)
}

const HIGH_SURROGATE: std::ops::RangeInclusive<u16> = 0xD800..=0xDBFF;
const LOW_SURROGATE: std::ops::RangeInclusive<u16> = 0xDC00..=0xDFFF;

/// Parses a fixture log. Windows delivers a character outside the Basic Multilingual Plane as
/// two `WM_CHAR` messages, one per UTF-16 surrogate, so a high surrogate waits for the next
/// character of the same window: a low surrogate completes it, anything else, or the end of the
/// log, leaves it `Unpaired`. Key and mouse lines between the halves do not break the pair.
pub(crate) fn parse_log(text: &str) -> Vec<Event> {
    let mut events = Vec::new();
    // The high surrogate each receiving window is waiting to complete; a scene has few windows.
    let mut pending: Vec<(String, u16)> = Vec::new();
    for line in text.lines() {
        let (role, unit) = match Line::parse(line) {
            Line::Event(event) => {
                events.push(event);
                continue;
            }
            Line::Unit { role, unit } => (role, unit),
        };
        let high = pending
            .iter()
            .position(|(waiting, _)| *waiting == role)
            .map(|index| pending.swap_remove(index).1);
        if let Some(high) = high {
            if LOW_SURROGATE.contains(&unit) {
                if let Some(Ok(ch)) = char::decode_utf16([high, unit]).next() {
                    events.push(Event::Char { role, ch });
                }
                continue;
            }
            events.push(Event::Unpaired {
                role: role.clone(),
                unit: high,
            });
        }
        if HIGH_SURROGATE.contains(&unit) {
            pending.push((role, unit));
        } else if let Some(ch) = char::from_u32(u32::from(unit)) {
            events.push(Event::Char { role, ch });
        } else {
            events.push(Event::Unpaired { role, unit });
        }
    }
    events.extend(
        pending
            .into_iter()
            .map(|(role, unit)| Event::Unpaired { role, unit }),
    );
    events
}

pub(crate) fn ready(events: &[Event]) -> Option<&Regions> {
    events.iter().find_map(|event| match event {
        Event::Ready(regions) => Some(regions),
        _ => None,
    })
}

pub(crate) fn typed(events: &[Event]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            Event::KeyDown { role, vk: 0x0D } if role == "main" => Some('\u{23CE}'),
            Event::KeyDown { role, vk: 0x09 } if role == "main" => Some('\u{21E5}'),
            Event::Char { role, ch } if role == "main" && !ch.is_control() => Some(*ch),
            Event::Unpaired { role, .. } if role == "main" => Some(char::REPLACEMENT_CHARACTER),
            _ => None,
        })
        .collect()
}

/// The surrogates the main window received without their other half.
pub(crate) fn unpaired(events: &[Event]) -> Vec<u16> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Unpaired { role, unit } if role == "main" => Some(*unit),
            _ => None,
        })
        .collect()
}

pub(crate) fn left_downs(events: &[Event], role: &str) -> Vec<(i32, i32)> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Mouse {
                role: r,
                action,
                x,
                y,
            } if r == role && action == "left_down" => Some((*x, *y)),
            _ => None,
        })
        .collect()
}

pub(crate) fn has_key_down(events: &[Event], vk: u16) -> bool {
    events
        .iter()
        .any(|event| matches!(event, Event::KeyDown { role, vk: v } if role == "main" && *v == vk))
}

pub(crate) fn button_pressed(events: &[Event], name: &str) -> bool {
    events
        .iter()
        .any(|event| matches!(event, Event::Button { name: n, .. } if n == name))
}

pub(crate) fn activated(events: &[Event]) -> bool {
    activations(events) > 0
}

/// How often the main window was activated, by click or by `SetForegroundWindow`.
pub(crate) fn activations(events: &[Event]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, Event::Activate { role, state: 1 | 2 } if role == "main"))
        .count()
}

pub(crate) fn near(actual: (i32, i32), wanted: (i32, i32)) -> bool {
    (actual.0 - wanted.0).abs() <= 1 && (actual.1 - wanted.1).abs() <= 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;

    const LOG: &str = "ready client=10,140,370,400 label=10,140,370,204 edit=26,216,354,296 \
                       button=26,316,146,356\n\
                       main activate 1\n\
                       main mouse left_down 190 256\n\
                       main mouse left_up 190 256\n\
                       main char U+0048\n\
                       main char U+00E9\n\
                       main keydown 0x0D\n\
                       main char U+000D\n\
                       main keydown 0x09\n\
                       main char U+0009\n\
                       main char U+65E5\n\
                       dialog mouse left_down 500 300\n\
                       main button ping\n\
                       something new from a later fixture";

    #[test]
    fn a_fixture_log_parses_into_typed_events() {
        let events = parse_log(LOG);
        let regions = ready(&events).expect("ready line");
        assert_eq!(regions.get("edit"), Some([26, 216, 354, 296]));
        assert_eq!(regions.center("button"), Some((86, 336)));
        assert!(regions.contains("label", (10, 140)));
        assert!(
            !regions.contains("label", (370, 140)),
            "right edge is outside"
        );
        assert!(activated(&events));
        assert_eq!(left_downs(&events, "main"), vec![(190, 256)]);
        assert_eq!(left_downs(&events, "dialog"), vec![(500, 300)]);
        assert!(button_pressed(&events, "ping"));
        assert!(has_key_down(&events, 0x0D));
        assert_eq!(
            events.last(),
            Some(&Event::Other("something new from a later fixture".into()))
        );
    }

    #[test]
    fn typed_text_marks_enter_and_tab_once() {
        assert_eq!(typed(&parse_log(LOG)), "H\u{e9}\u{23CE}\u{21E5}\u{65e5}");
    }

    #[test]
    fn malformed_lines_never_panic() {
        for line in [
            "",
            "ready",
            "ready edit=1,2,3",
            "main keydown 13",
            "main char U+ZZZZ",
            "main char U+1F980",
            "main mouse left_down x y",
            "main activate yes",
        ] {
            let events = parse_log(line);
            assert!(
                events.iter().all(|event| !event.is_input()),
                "{line:?} parsed as input: {events:?}"
            );
        }
        assert_eq!(
            ready(&parse_log("ready edit=1,2,3")),
            Some(&Regions::default())
        );
    }

    /// The log lines a window writes for `text`: one `char` line per UTF-16 unit, as `WM_CHAR`
    /// delivers it.
    fn units(role: &str, text: &str) -> String {
        text.encode_utf16().fold(String::new(), |mut log, unit| {
            let _ = writeln!(log, "{role} char U+{unit:04X}");
            log
        })
    }

    #[test]
    fn a_surrogate_pair_is_one_character() {
        let events = parse_log(&units("main", "A\u{1F980}B"));
        assert_eq!(typed(&events), "A\u{1F980}B");
        assert!(unpaired(&events).is_empty());
        assert_eq!(
            events.iter().filter(|event| event.is_input()).count(),
            3,
            "{events:?}"
        );
    }

    #[test]
    fn key_lines_between_the_halves_keep_the_pair() {
        // SendInput delivers each half as its own VK_PACKET keystroke.
        let log = "main keydown 0xE7\nmain char U+D83E\nmain keyup 0xE7\n\
                   main keydown 0xE7\nmain char U+DD80\nmain keyup 0xE7\n";
        assert_eq!(typed(&parse_log(log)), "\u{1F980}");
    }

    #[test]
    fn each_window_keeps_its_own_pending_half() {
        let events = parse_log("main char U+D83E\ndialog char U+0078\nmain char U+DD80\n");
        assert_eq!(typed(&events), "\u{1F980}");
        assert!(events.contains(&Event::Char {
            role: "dialog".into(),
            ch: 'x'
        }));
        assert!(unpaired(&events).is_empty());
    }

    #[test]
    fn a_lone_half_is_reported_unpaired() {
        // A high surrogate followed by another character, a high surrogate at the end of the log,
        // a low surrogate with no high one, and a high surrogate followed by another high one.
        for (log, wanted, half) in [
            ("main char U+D83E\nmain char U+0042\n", "\u{FFFD}B", 0xD83E),
            ("main char U+0041\nmain char U+D83E\n", "A\u{FFFD}", 0xD83E),
            ("main char U+DD80\nmain char U+0042\n", "\u{FFFD}B", 0xDD80),
            (
                "main char U+D83E\nmain char U+D83E\nmain char U+DD80\n",
                "\u{FFFD}\u{1F980}",
                0xD83E,
            ),
        ] {
            let events = parse_log(log);
            assert_eq!(typed(&events), wanted, "{log:?}");
            assert_eq!(unpaired(&events), [half], "{log:?}");
            assert!(
                events.contains(&Event::Unpaired {
                    role: "main".into(),
                    unit: half
                }),
                "{log:?}: {events:?}"
            );
        }
    }

    #[test]
    fn an_unpaired_half_of_another_window_stays_out_of_the_main_text() {
        let events = parse_log("dialog char U+D83E\nmain char U+0041\n");
        assert_eq!(typed(&events), "A");
        assert!(unpaired(&events).is_empty());
        assert!(events.contains(&Event::Unpaired {
            role: "dialog".into(),
            unit: 0xD83E
        }));
    }
}
