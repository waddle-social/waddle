//! XEP-0372: References
//!
//! Provides helpers for detecting, parsing, and building reference elements
//! within XMPP message stanzas. References mark spans of text as pointing
//! to entities (users, URIs, etc.), enabling structured @mentions.
//!
//! ## XML Format
//!
//! ```xml
//! <message type='groupchat' to='room@muc.example.com'>
//!   <body>Hello @alice, meet @bob</body>
//!   <reference xmlns='urn:xmpp:reference:0' type='mention'
//!              begin='6' end='12' uri='xmpp:alice@example.com'/>
//!   <reference xmlns='urn:xmpp:reference:0' type='mention'
//!              begin='19' end='23' uri='xmpp:bob@example.com'/>
//! </message>
//! ```
//!
//! ## Reference Types
//!
//! - **mention**: An @mention of a user or entity.
//! - **data**: A reference to a data source (file, media).
//!
//! ## Server Behavior
//!
//! The server transparently routes reference elements. It may use
//! mention references for notification decisions (e.g., pushing
//! notifications to mentioned users even if they have notifications muted).

use minidom::Element;
use thiserror::Error;
use xmpp_parsers::message::Message;

/// Namespace for XEP-0372 References.
pub const NS_REFERENCE: &str = "urn:xmpp:reference:0";

/// Errors that can occur when parsing reference elements.
#[derive(Debug, Error)]
pub enum ReferenceError {
    /// Missing required `type` attribute.
    #[error("reference missing type attribute")]
    MissingType,
    /// Invalid begin/end range.
    #[error("invalid reference range: begin={begin}, end={end}")]
    InvalidRange { begin: usize, end: usize },
}

/// The type of a reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReferenceType {
    /// An @mention of a user or entity.
    Mention,
    /// A reference to data (file, media).
    Data,
}

impl ReferenceType {
    /// Parse from a string attribute value.
    pub fn from_str_attr(s: &str) -> Option<Self> {
        match s {
            "mention" => Some(Self::Mention),
            "data" => Some(Self::Data),
            _ => None,
        }
    }

    /// Convert to the attribute string value.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Mention => "mention",
            Self::Data => "data",
        }
    }
}

impl std::fmt::Display for ReferenceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A reference annotation on a message body span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    /// The type of reference (mention, data).
    pub ref_type: ReferenceType,
    /// The start index (UTF-8 code units) in the body text.
    pub begin: Option<usize>,
    /// The end index (exclusive) in the body text.
    pub end: Option<usize>,
    /// The URI of the referenced entity (e.g., `xmpp:alice@example.com`).
    pub uri: Option<String>,
    /// Optional anchor text (for display if body range unavailable).
    pub anchor: Option<String>,
}

impl Reference {
    /// Create a new mention reference.
    pub fn mention(uri: impl Into<String>) -> Self {
        Self {
            ref_type: ReferenceType::Mention,
            begin: None,
            end: None,
            uri: Some(uri.into()),
            anchor: None,
        }
    }

    /// Create a mention reference with body position.
    pub fn mention_at(begin: usize, end: usize, uri: impl Into<String>) -> Self {
        Self {
            ref_type: ReferenceType::Mention,
            begin: Some(begin),
            end: Some(end),
            uri: Some(uri.into()),
            anchor: None,
        }
    }

    /// Create a data reference.
    pub fn data(uri: impl Into<String>) -> Self {
        Self {
            ref_type: ReferenceType::Data,
            begin: None,
            end: None,
            uri: Some(uri.into()),
            anchor: None,
        }
    }

    /// Set the anchor text.
    pub fn with_anchor(mut self, anchor: impl Into<String>) -> Self {
        self.anchor = Some(anchor.into());
        self
    }

    /// Returns `true` if this is a mention reference.
    pub fn is_mention(&self) -> bool {
        self.ref_type == ReferenceType::Mention
    }

    /// Extract the bare JID from the URI (strips `xmpp:` prefix).
    pub fn bare_jid(&self) -> Option<&str> {
        self.uri.as_deref().and_then(|u| u.strip_prefix("xmpp:"))
    }
}

/// Trait for types that can carry reference elements.
pub trait ReferenceCarrier {
    /// Extract all references from this carrier.
    fn references(&self) -> Vec<Reference>;

    /// Extract only mention references.
    fn mentions(&self) -> Vec<Reference> {
        self.references()
            .into_iter()
            .filter(|r| r.is_mention())
            .collect()
    }

    /// Returns `true` if this carrier mentions a specific JID.
    fn mentions_jid(&self, jid: &str) -> bool {
        let xmpp_uri = format!("xmpp:{jid}");
        self.references()
            .iter()
            .any(|r| r.is_mention() && r.uri.as_deref() == Some(&xmpp_uri))
    }

    /// Returns `true` if this carrier has any references.
    fn has_references(&self) -> bool {
        !self.references().is_empty()
    }

    /// Returns `true` if this carrier has any mention references.
    fn has_mentions(&self) -> bool {
        self.references().iter().any(|r| r.is_mention())
    }
}

impl ReferenceCarrier for Message {
    fn references(&self) -> Vec<Reference> {
        extract_references_from_message(self)
    }
}

// ── Detection ────────────────────────────────────────────────────────

/// Check if an element is a `<reference/>` element.
pub fn is_reference_element(elem: &Element) -> bool {
    elem.ns() == NS_REFERENCE && elem.name() == "reference"
}

/// Check if a message has any reference elements.
pub fn has_references(msg: &Message) -> bool {
    msg.payloads.iter().any(|e| is_reference_element(e))
}

// ── Extraction ───────────────────────────────────────────────────────

/// Extract all references from a message.
pub fn extract_references_from_message(msg: &Message) -> Vec<Reference> {
    msg.payloads
        .iter()
        .filter(|e| is_reference_element(e))
        .filter_map(|e| parse_reference_element(e).ok())
        .collect()
}

/// Parse a single `<reference/>` element.
pub fn parse_reference_element(elem: &Element) -> Result<Reference, ReferenceError> {
    let ref_type = elem
        .attr("type")
        .and_then(ReferenceType::from_str_attr)
        .ok_or(ReferenceError::MissingType)?;

    let begin = elem.attr("begin").and_then(|s| s.parse().ok());
    let end = elem.attr("end").and_then(|s| s.parse().ok());
    let uri = elem
        .attr("uri")
        .filter(|s| !s.is_empty())
        .map(|s| s.to_owned());
    let anchor = elem
        .attr("anchor")
        .filter(|s| !s.is_empty())
        .map(|s| s.to_owned());

    Ok(Reference {
        ref_type,
        begin,
        end,
        uri,
        anchor,
    })
}

/// Extract mention URIs from a message.
pub fn extract_mention_uris(msg: &Message) -> Vec<String> {
    extract_references_from_message(msg)
        .into_iter()
        .filter(|r| r.is_mention())
        .filter_map(|r| r.uri)
        .collect()
}

/// Extract mentioned bare JIDs from a message (strips `xmpp:` prefix).
pub fn extract_mentioned_jids(msg: &Message) -> Vec<String> {
    extract_references_from_message(msg)
        .into_iter()
        .filter(|r| r.is_mention())
        .filter_map(|r| r.bare_jid().map(|s| s.to_owned()))
        .collect()
}

// ── Building ─────────────────────────────────────────────────────────

/// Build a `<reference/>` element from a Reference.
pub fn build_reference_element(reference: &Reference) -> Element {
    let mut builder = Element::builder("reference", NS_REFERENCE)
        .attr("type", reference.ref_type.as_str());

    if let Some(begin) = reference.begin {
        builder = builder.attr("begin", begin.to_string());
    }
    if let Some(end) = reference.end {
        builder = builder.attr("end", end.to_string());
    }
    if let Some(ref uri) = reference.uri {
        builder = builder.attr("uri", uri.as_str());
    }
    if let Some(ref anchor) = reference.anchor {
        builder = builder.attr("anchor", anchor.as_str());
    }

    builder.build()
}

// ── Mutation ─────────────────────────────────────────────────────────

/// Add a reference to a message.
pub fn add_reference(msg: &mut Message, reference: &Reference) {
    msg.payloads.push(build_reference_element(reference));
}

/// Remove all reference elements from a message.
pub fn strip_references(msg: &mut Message) {
    msg.payloads.retain(|e| e.ns() != NS_REFERENCE);
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmpp_parsers::message::Message;

    #[test]
    fn test_is_reference_element() {
        let elem = Element::builder("reference", NS_REFERENCE)
            .attr("type", "mention")
            .build();
        assert!(is_reference_element(&elem));

        let wrong_ns = Element::builder("reference", "jabber:client").build();
        assert!(!is_reference_element(&wrong_ns));
    }

    #[test]
    fn test_extract_mentions() {
        let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <body>Hello @alice and @bob</body>\
                    <reference xmlns='urn:xmpp:reference:0' type='mention' begin='6' end='12' uri='xmpp:alice@example.com'/>\
                    <reference xmlns='urn:xmpp:reference:0' type='mention' begin='17' end='21' uri='xmpp:bob@example.com'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        let refs = extract_references_from_message(&msg);
        assert_eq!(refs.len(), 2);
        assert!(refs[0].is_mention());
        assert_eq!(refs[0].begin, Some(6));
        assert_eq!(refs[0].end, Some(12));
        assert_eq!(refs[0].uri.as_deref(), Some("xmpp:alice@example.com"));
        assert_eq!(refs[0].bare_jid(), Some("alice@example.com"));
    }

    #[test]
    fn test_extract_mentioned_jids() {
        let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <body>@alice @bob</body>\
                    <reference xmlns='urn:xmpp:reference:0' type='mention' uri='xmpp:alice@example.com'/>\
                    <reference xmlns='urn:xmpp:reference:0' type='mention' uri='xmpp:bob@example.com'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        let jids = extract_mentioned_jids(&msg);
        assert_eq!(jids, vec!["alice@example.com", "bob@example.com"]);
    }

    #[test]
    fn test_extract_no_references() {
        let msg = Message::new(None::<jid::Jid>);
        assert!(extract_references_from_message(&msg).is_empty());
    }

    #[test]
    fn test_reference_missing_type_skipped() {
        let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <body>Hello</body>\
                    <reference xmlns='urn:xmpp:reference:0' uri='xmpp:alice@example.com'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
        // Missing type → skipped (not an error, just ignored)
        assert!(extract_references_from_message(&msg).is_empty());
    }

    #[test]
    fn test_reference_data_type() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <body>See the file</body>\
                    <reference xmlns='urn:xmpp:reference:0' type='data' uri='https://files.example.com/cat.jpg'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        let refs = extract_references_from_message(&msg);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].ref_type, ReferenceType::Data);
        assert!(!refs[0].is_mention());
    }

    #[test]
    fn test_build_reference_mention() {
        let r = Reference::mention_at(6, 12, "xmpp:alice@example.com");
        let elem = build_reference_element(&r);

        assert_eq!(elem.name(), "reference");
        assert_eq!(elem.ns(), NS_REFERENCE);
        assert_eq!(elem.attr("type"), Some("mention"));
        assert_eq!(elem.attr("begin"), Some("6"));
        assert_eq!(elem.attr("end"), Some("12"));
        assert_eq!(elem.attr("uri"), Some("xmpp:alice@example.com"));
    }

    #[test]
    fn test_build_reference_no_position() {
        let r = Reference::mention("xmpp:bob@example.com");
        let elem = build_reference_element(&r);

        assert_eq!(elem.attr("type"), Some("mention"));
        assert_eq!(elem.attr("begin"), None);
        assert_eq!(elem.attr("end"), None);
        assert_eq!(elem.attr("uri"), Some("xmpp:bob@example.com"));
    }

    #[test]
    fn test_build_reference_with_anchor() {
        let r = Reference::mention("xmpp:alice@example.com").with_anchor("@alice");
        let elem = build_reference_element(&r);

        assert_eq!(elem.attr("anchor"), Some("@alice"));
    }

    #[test]
    fn test_add_reference() {
        let mut msg = Message::new(None::<jid::Jid>);
        add_reference(&mut msg, &Reference::mention("xmpp:alice@example.com"));
        add_reference(&mut msg, &Reference::mention("xmpp:bob@example.com"));

        assert_eq!(extract_references_from_message(&msg).len(), 2);
    }

    #[test]
    fn test_strip_references() {
        let mut msg = Message::new(None::<jid::Jid>);
        add_reference(&mut msg, &Reference::mention("xmpp:alice@example.com"));

        strip_references(&mut msg);
        assert!(!has_references(&msg));
    }

    #[test]
    fn test_reference_carrier_trait() {
        let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <body>@alice hello</body>\
                    <reference xmlns='urn:xmpp:reference:0' type='mention' uri='xmpp:alice@example.com'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        assert!(msg.has_references());
        assert!(msg.has_mentions());
        assert!(msg.mentions_jid("alice@example.com"));
        assert!(!msg.mentions_jid("bob@example.com"));
        assert_eq!(msg.mentions().len(), 1);
    }

    #[test]
    fn test_reference_type_display() {
        assert_eq!(ReferenceType::Mention.to_string(), "mention");
        assert_eq!(ReferenceType::Data.to_string(), "data");
    }

    #[test]
    fn test_reference_new_helpers() {
        let m = Reference::mention("xmpp:a@b.com");
        assert!(m.is_mention());
        assert_eq!(m.bare_jid(), Some("a@b.com"));

        let d = Reference::data("https://example.com/file.png");
        assert!(!d.is_mention());
        assert_eq!(d.ref_type, ReferenceType::Data);
    }
}
