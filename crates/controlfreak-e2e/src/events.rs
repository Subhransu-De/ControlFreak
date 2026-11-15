use std::collections::HashMap;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Regions(HashMap<String, [i32; 4]>);

impl Regions {
    pub fn get(&self, name: &str) -> Option<[i32; 4]> {
        self.0.get(name).copied()
    }

    pub fn center(&self, name: &str) -> Option<(i32, i32)> {
        let [left, top, right, bottom] = self.get(name)?;
        Some((left.midpoint(right), top.midpoint(bottom)))
    }

    pub fn contains(&self, name: &str, point: (i32, i32)) -> bool {
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

impl Event {
    pub fn parse(line: &str) -> Self {
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
            [role, "char", unit] => unit
                .strip_prefix("U+")
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .and_then(char::from_u32)
                .map(|ch| Self::Char {
                    role: (*role).to_owned(),
                    ch,
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

    pub fn is_input(&self) -> bool {
        matches!(
            self,
            Self::KeyDown { .. } | Self::KeyUp { .. } | Self::Char { .. } | Self::Mouse { .. }
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

pub fn parse_log(text: &str) -> Vec<Event> {
    text.lines().map(Event::parse).collect()
}

pub fn ready(events: &[Event]) -> Option<&Regions> {
    events.iter().find_map(|event| match event {
        Event::Ready(regions) => Some(regions),
        _ => None,
    })
}

pub fn typed(events: &[Event]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            Event::KeyDown { role, vk: 0x0D } if role == "main" => Some('\u{23CE}'),
            Event::KeyDown { role, vk: 0x09 } if role == "main" => Some('\u{21E5}'),
            Event::Char { role, ch } if role == "main" && !ch.is_control() => Some(*ch),
            _ => None,
        })
        .collect()
}

pub fn left_downs(events: &[Event], role: &str) -> Vec<(i32, i32)> {
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

pub fn has_key_down(events: &[Event], vk: u16) -> bool {
    events
        .iter()
        .any(|event| matches!(event, Event::KeyDown { role, vk: v } if role == "main" && *v == vk))
}

pub fn button_pressed(events: &[Event], name: &str) -> bool {
    events
        .iter()
        .any(|event| matches!(event, Event::Button { name: n, .. } if n == name))
}

pub fn activated(events: &[Event]) -> bool {
    events
        .iter()
        .any(|event| matches!(event, Event::Activate { role, state: 1 | 2 } if role == "main"))
}

pub fn near(actual: (i32, i32), wanted: (i32, i32)) -> bool {
    (actual.0 - wanted.0).abs() <= 1 && (actual.1 - wanted.1).abs() <= 1
}

#[cfg(test)]
mod tests {
    use super::*;

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
            "main char U+D800",
            "main mouse left_down x y",
            "main activate yes",
        ] {
            let event = Event::parse(line);
            assert!(!event.is_input(), "{line:?} parsed as input: {event:?}");
        }
        assert_eq!(
            ready(&parse_log("ready edit=1,2,3")),
            Some(&Regions::default())
        );
    }
}
