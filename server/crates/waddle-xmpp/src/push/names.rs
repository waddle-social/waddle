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
        let mut has_visible_char = false;
        let mut count = 0;
        for c in raw.chars() {
            if always_remove(c) {
                continue;
            }
            if c.is_whitespace() {
                pending_space = !name.is_empty();
                continue;
            }
            if is_name_suffix(c) && (!has_visible_char || pending_space) {
                continue;
            }
            if count + usize::from(pending_space) >= MAX_NAME_CHARS {
                trim_dangling_joiners(&mut name);
                break;
            }
            if pending_space {
                name.push(' ');
                count += 1;
                pending_space = false;
            }
            name.push(c);
            count += 1;
            has_visible_char |= is_visible(c);
        }
        has_visible_char.then_some(Self(name))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn always_remove(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00ad}' | '\u{061c}' | '\u{200b}' | '\u{200e}'..='\u{200f}'
                | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}'
                | '\u{feff}' | '\u{fffe}'..='\u{ffff}'
        )
}

fn is_joiner(c: char) -> bool {
    matches!(c, '\u{200c}' | '\u{200d}')
}

fn is_name_suffix(c: char) -> bool {
    is_joiner(c)
        || matches!(
            c,
            '\u{180b}'..='\u{180d}' | '\u{180f}' | '\u{fe00}'..='\u{fe0f}'
                | '\u{e0020}'..='\u{e007f}' | '\u{e0100}'..='\u{e01ef}'
        )
}

fn is_visible(c: char) -> bool {
    // These characters can affect rendering in context but cannot supply
    // display text on their own. The caller has already handled whitespace
    // and characters that are always removed.
    !is_name_suffix(c)
        && !matches!(
            c,
            '\u{034f}' | '\u{115f}'..='\u{1160}' | '\u{17b4}'..='\u{17b5}' | '\u{180e}'
                | '\u{2065}' | '\u{206a}'..='\u{206f}' | '\u{2800}' | '\u{3164}' | '\u{ffa0}'
                | '\u{fff0}'..='\u{fffb}' | '\u{13430}'..='\u{1343f}' | '\u{1bca0}'..='\u{1bca3}'
                | '\u{1d173}'..='\u{1d17a}' | '\u{e0000}'..='\u{e0fff}'
        )
}

fn trim_dangling_joiners(name: &mut String) {
    let mut end = name.len();
    for (index, c) in name.char_indices().rev() {
        if c.is_whitespace() || is_visible(c) {
            break;
        }
        if is_joiner(c) {
            end = index;
        }
    }
    name.truncate(end);
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
        assert_eq!(name.as_str(), "Émilie 🥨\u{fe0f}");
    }

    #[test]
    fn display_names_preserve_script_shaping_and_emoji_sequences() {
        for original in [
            "علی\u{200c}رضا",
            "ශ්\u{200d}රී",
            "👨\u{200d}👩\u{200d}👧",
            "🏳\u{fe0f}\u{200d}🌈",
            "❤\u{fe0f}",
            "🏴\u{e0067}\u{e0062}\u{e0073}\u{e0063}\u{e0074}\u{e007f}",
            "ᠮ\u{180b}ᠣ\u{180c}ᠩ\u{180d}ᠭ\u{180f}ᠣᠯ",
            "葛\u{e0100}",
        ] {
            let name = DisplayName::parse(original).expect("visible name");
            assert_eq!(name.as_str(), original);
        }
    }

    #[test]
    fn visible_format_marks_count_as_visible() {
        for c in ('\u{0600}'..='\u{0605}').chain(['\u{06dd}', '\u{070f}', '\u{08e2}', '\u{110bd}'])
        {
            let original = c.to_string();
            let name = DisplayName::parse(&original).expect("visible format mark");
            assert_eq!(name.as_str(), original);
        }
    }

    #[test]
    fn unsafe_characters_are_removed_inside_a_name() {
        for c in [
            '\u{0000}'..='\u{001f}',
            '\u{007f}'..='\u{009f}',
            '\u{202a}'..='\u{202e}',
            '\u{2066}'..='\u{2069}',
            '\u{2060}'..='\u{2064}',
        ]
        .into_iter()
        .flatten()
        .chain([
            '\u{00ad}', '\u{061c}', '\u{200b}', '\u{200e}', '\u{200f}', '\u{feff}', '\u{fffe}',
            '\u{ffff}',
        ]) {
            let name = DisplayName::parse(&format!("A{c}B")).expect("visible name");
            assert_eq!(name.as_str(), "AB");
        }
    }

    #[test]
    fn suffix_characters_require_a_preceding_base() {
        let suffixes = "\u{200c}\u{200d}\u{fe0f}\u{e0100}\u{180b}\u{180f}\u{e0067}\u{e007f}";
        assert!(DisplayName::parse(suffixes).is_none());
        let name = DisplayName::parse(&format!(" {suffixes}Alice  {suffixes}  Bob "))
            .expect("visible name");
        assert_eq!(name.as_str(), "Alice Bob");
    }

    #[test]
    fn character_cap_preserves_complete_sequences_and_drops_dangling_joiners() {
        let prefix = "é".repeat(62);
        let rainbow = DisplayName::parse(&format!("{prefix}🏳\u{fe0f}\u{200d}🌈")).expect("name");
        assert_eq!(rainbow.as_str(), format!("{prefix}🏳\u{fe0f}"));
        let family = DisplayName::parse(&format!("{prefix}👨\u{200d}👩\u{200d}👧")).expect("name");
        assert_eq!(family.as_str(), format!("{prefix}👨"));
        let family = DisplayName::parse(&format!("{}👨\u{200d}👩!", "é".repeat(61))).expect("name");
        assert_eq!(family.as_str(), format!("{}👨\u{200d}👩", "é".repeat(61)));
    }
}
