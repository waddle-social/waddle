//! XEP-0338 Jingle Grouping constants and builders.

use minidom::Element;
use std::fmt;

/// Namespace for Jingle grouping payloads.
pub const NS_JINGLE_GROUPING: &str = xmpp_parsers::ns::JINGLE_GROUPING;
/// Disco feature for RTP BUNDLE/grouping support per XEP-0338.
pub const FEATURE_RFC5888_GROUPING: &str = "urn:ietf:rfc:5888";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupSemantics {
    Bundle,
}

impl GroupSemantics {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bundle => "BUNDLE",
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ContentName(String);

impl ContentName {
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (!value.trim().is_empty()).then_some(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ContentName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ContentName").field(&self.0).finish()
    }
}

/// Build a grouping element such as BUNDLE.
pub fn build_group(semantics: GroupSemantics, content_names: &[ContentName]) -> Element {
    let mut elem = Element::builder("group", NS_JINGLE_GROUPING)
        .attr("semantics", semantics.as_str())
        .build();
    for name in content_names {
        elem.append_child(
            Element::builder("content", NS_JINGLE_GROUPING)
                .attr("name", name.as_str())
                .build(),
        );
    }
    elem
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_bundle_group() {
        let names = [
            ContentName::new("audio").expect("content name"),
            ContentName::new("video").expect("content name"),
        ];
        let elem = build_group(GroupSemantics::Bundle, &names);
        assert_eq!(elem.attr("semantics"), Some("BUNDLE"));
        assert_eq!(elem.children().count(), 2);
        assert_eq!(
            elem.children().next().and_then(|c| c.attr("name")),
            Some("audio")
        );
    }
}
