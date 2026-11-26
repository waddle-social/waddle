//! Optional display names alongside the XEP-0357 summary form.

use minidom::Element;

pub const NS_PUSH_NAMES: &str = "urn:waddle:push:names:0";

const MAX_NAME_CHARS: usize = 64;

/// Display text safe to put in the push extension and read back from it.
#[derive(Clone, PartialEq, Eq)]
pub struct DisplayName(String);

impl DisplayName {
    pub fn parse(raw: &str) -> Option<Self> {
        let mut name = String::new();
        let mut pending_space = false;
        let mut count = 0;
        for c in raw.chars() {
            // U+FFFE and U+FFFF cannot be serialized in XML 1.0.
            if c.is_control()
                || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{fffe}' | '\u{ffff}')
            {
                continue;
            }
            if c.is_whitespace() {
                pending_space = !name.is_empty();
                continue;
            }
            if pending_space {
                if count + 1 == MAX_NAME_CHARS {
                    break;
                }
                name.push(' ');
                count += 1;
                pending_space = false;
            }
            name.push(c);
            count += 1;
            if count == MAX_NAME_CHARS {
                break;
            }
        }
        (!name.is_empty()).then_some(Self(name))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct PushNames {
    pub sender: Option<DisplayName>,
    pub room: Option<DisplayName>,
}

impl PushNames {
    pub fn to_element(&self) -> Option<Element> {
        if self.sender.is_none() && self.room.is_none() {
            return None;
        }
        let mut names = Element::builder("names", NS_PUSH_NAMES);
        for (tag, name) in [("sender", &self.sender), ("room", &self.room)] {
            if let Some(name) = name {
                names = names.append(
                    Element::builder(tag, NS_PUSH_NAMES)
                        .append(name.as_str())
                        .build(),
                );
            }
        }
        Some(names.build())
    }

    /// Parse a notification's optional names, validating every accepted value.
    pub fn parse(notification: &Element, include_room: bool) -> Self {
        let Some(names) = notification.get_child("names", NS_PUSH_NAMES) else {
            return Self::default();
        };
        let name = |tag: &str| {
            names
                .get_child(tag, NS_PUSH_NAMES)
                .and_then(|child| DisplayName::parse(&child.text()))
        };
        Self {
            sender: name("sender"),
            room: if include_room { name("room") } else { None },
        }
    }
}
