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
    /// Missing required `uri` attribute.
    #[error("reference missing uri attribute")]
    MissingUri,
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
    /// The required URI of the referenced entity (e.g., `xmpp:alice@example.com`).
    pub uri: String,
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
            uri: uri.into(),
            anchor: None,
        }
    }

    /// Create a mention reference with body position.
    pub fn mention_at(begin: usize, end: usize, uri: impl Into<String>) -> Self {
        Self {
            ref_type: ReferenceType::Mention,
            begin: Some(begin),
            end: Some(end),
            uri: uri.into(),
            anchor: None,
        }
    }

    /// Create a data reference.
    pub fn data(uri: impl Into<String>) -> Self {
        Self {
            ref_type: ReferenceType::Data,
            begin: None,
            end: None,
            uri: uri.into(),
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
        self.uri.strip_prefix("xmpp:")
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
            .any(|r| r.is_mention() && r.uri == xmpp_uri)
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
    msg.payloads.iter().any(is_reference_element)
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
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_owned())
        .ok_or(ReferenceError::MissingUri)?;
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
        .map(|r| r.uri)
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
    let mut builder =
        Element::builder("reference", NS_REFERENCE).attr("type", reference.ref_type.as_str());

    if let Some(begin) = reference.begin {
        builder = builder.attr("begin", begin.to_string());
    }
    if let Some(end) = reference.end {
        builder = builder.attr("end", end.to_string());
    }
    builder = builder.attr("uri", reference.uri.as_str());
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
mod tests;
