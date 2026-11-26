//! Optional display names alongside the XEP-0357 summary form.

use minidom::Element;

use super::envelope::NotificationClass;

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
            if c.is_control() || is_invisible(c) {
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

fn is_invisible(c: char) -> bool {
    // Unicode 17.0 General_Category=Cf and Default_Ignorable_Code_Point,
    // plus blank Braille and the two code points excluded by XML 1.0.
    // Strip joiners and variation selectors even inside emoji sequences.
    matches!(
        c,
        '\u{00ad}'
            | '\u{034f}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061c}'
            | '\u{06dd}'
            | '\u{070f}'
            | '\u{0890}'..='\u{0891}'
            | '\u{08e2}'
            | '\u{115f}'..='\u{1160}'
            | '\u{17b4}'..='\u{17b5}'
            | '\u{180b}'..='\u{180f}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}'
            | '\u{2800}'
            | '\u{3164}'
            | '\u{fe00}'..='\u{fe0f}'
            | '\u{feff}'
            | '\u{ffa0}'
            | '\u{fff0}'..='\u{fffb}'
            | '\u{fffe}'..='\u{ffff}'
            | '\u{110bd}'
            | '\u{110cd}'
            | '\u{13430}'..='\u{1343f}'
            | '\u{1bca0}'..='\u{1bca3}'
            | '\u{1d173}'..='\u{1d17a}'
            | '\u{e0000}'..='\u{e0fff}'
    )
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
    pub fn parse(notification: &Element, class: NotificationClass) -> Self {
        if !class.allows_display_names() {
            return Self::default();
        }
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
            room: if class.is_room() { name("room") } else { None },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DisplayName;

    #[test]
    fn invisible_display_name_falls_back() {
        for c in [
            '\u{00ad}',
            '\u{061c}',
            '\u{034f}',
            '\u{200b}',
            '\u{200c}',
            '\u{200d}',
            '\u{200e}',
            '\u{200f}',
            '\u{202e}',
            '\u{2060}',
            '\u{2064}',
            '\u{2069}',
            '\u{fe0f}',
            '\u{feff}',
            '\u{1343f}',
            '\u{e0001}',
            '\u{e007f}',
            '\u{e0100}',
        ] {
            assert!(DisplayName::parse(&format!(" {c} ")).is_none(), "{c:?}");
        }
        let name = DisplayName::parse(" \u{200b}Émilie \u{2060} 🥨\u{fe0f} ").expect("name");
        assert_eq!(name.as_str(), "Émilie 🥨");
    }
}
