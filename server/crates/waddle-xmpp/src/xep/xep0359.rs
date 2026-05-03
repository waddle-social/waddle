//! XEP-0359: Unique and Stable Stanza IDs
//!
//! Provides helpers for detecting, parsing, and building stanza ID elements.
//! These ensure messages have stable, server-assigned identifiers for use
//! in archives (MAM), corrections, reactions, and retractions.
//!
//! ## Elements
//!
//! - **`<stanza-id id='...' by='...'/>`**: Server-assigned stable ID.
//!   The `by` attribute identifies the assigning entity (server or MUC).
//! - **`<origin-id id='...'/>`**: Client-assigned unique ID, preserved
//!   through MUC reflection where the `id` attribute may be changed.
//!
//! ## XML Format
//!
//! ```xml
//! <message from='room@muc.example.com/nick' id='server-id'>
//!   <body>Hello</body>
//!   <stanza-id xmlns='urn:xmpp:sid:0' id='archive-uuid' by='room@muc.example.com'/>
//!   <origin-id xmlns='urn:xmpp:sid:0' id='client-uuid'/>
//! </message>
//! ```
//!
//! ## Server Behavior
//!
//! - The server SHOULD add a `<stanza-id/>` to archived messages.
//! - The server MUST NOT strip `<origin-id/>` from client messages.
//! - Clients MUST NOT trust `<stanza-id/>` from other clients; only the
//!   `by` entity that matches the server/MUC JID is authoritative.

use jid::BareJid;
use minidom::Element;
use xmpp_parsers::message::Message;

/// Namespace for XEP-0359 Unique and Stable Stanza IDs.
pub const NS_SID: &str = "urn:xmpp:sid:0";

/// A server-assigned stable stanza ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StanzaId {
    /// The stable ID assigned by the server/service.
    pub id: String,
    /// The JID of the entity that assigned this ID.
    pub by: String,
}

impl StanzaId {
    /// Create a new stanza ID.
    pub fn new(id: impl Into<String>, by: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            by: by.into(),
        }
    }
}

fn stanza_id_by_matches(existing: &str, by: &str) -> bool {
    match (existing.parse::<BareJid>().ok(), by.parse::<BareJid>().ok()) {
        (Some(existing), Some(expected)) => existing == expected,
        _ => existing == by,
    }
}

fn stanza_id_matches_by(elem: &Element, by: &str) -> bool {
    if !is_stanza_id_element(elem) {
        return false;
    }

    elem.attr("by")
        .is_some_and(|existing| stanza_id_by_matches(existing, by))
}

/// A client-assigned origin ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginId {
    /// The unique ID assigned by the originating client.
    pub id: String,
}

impl OriginId {
    /// Create a new origin ID.
    pub fn new(id: impl Into<String>) -> Self {
        Self { id: id.into() }
    }
}

/// Trait for types that can carry stanza ID elements.
pub trait StanzaIdCarrier {
    /// Extract all stanza IDs from this carrier.
    fn stanza_ids(&self) -> Vec<StanzaId>;

    /// Extract the stanza ID assigned by a specific entity.
    fn stanza_id_by(&self, by: &str) -> Option<String> {
        self.stanza_ids()
            .into_iter()
            .find(|sid| stanza_id_by_matches(&sid.by, by))
            .map(|sid| sid.id)
    }

    /// Extract the origin ID from this carrier.
    fn origin_id(&self) -> Option<OriginId>;

    /// Returns `true` if this carrier has any stanza ID.
    fn has_stanza_id(&self) -> bool {
        !self.stanza_ids().is_empty()
    }
}

impl StanzaIdCarrier for Message {
    fn stanza_ids(&self) -> Vec<StanzaId> {
        extract_stanza_ids(self)
    }

    fn origin_id(&self) -> Option<OriginId> {
        extract_origin_id(self)
    }
}

// ── Detection ────────────────────────────────────────────────────────

/// Check if an element is a `<stanza-id/>`.
pub fn is_stanza_id_element(elem: &Element) -> bool {
    elem.ns() == NS_SID && elem.name() == "stanza-id"
}

/// Check if an element is an `<origin-id/>`.
pub fn is_origin_id_element(elem: &Element) -> bool {
    elem.ns() == NS_SID && elem.name() == "origin-id"
}

/// Check if a message has any `<stanza-id/>`.
pub fn has_stanza_id(msg: &Message) -> bool {
    msg.payloads.iter().any(is_stanza_id_element)
}

/// Check if a message has an `<origin-id/>`.
pub fn has_origin_id(msg: &Message) -> bool {
    msg.payloads.iter().any(is_origin_id_element)
}

// ── Extraction ───────────────────────────────────────────────────────

/// Extract all stanza IDs from a message.
pub fn extract_stanza_ids(msg: &Message) -> Vec<StanzaId> {
    msg.payloads
        .iter()
        .filter(|e| is_stanza_id_element(e))
        .filter_map(|e| {
            let id = e.attr("id").filter(|s| !s.is_empty())?;
            let by = e.attr("by").filter(|s| !s.is_empty())?;
            Some(StanzaId::new(id, by))
        })
        .collect()
}

/// Extract the stanza ID assigned by a specific entity.
pub fn extract_stanza_id_by(msg: &Message, by: &str) -> Option<String> {
    extract_stanza_ids(msg)
        .into_iter()
        .find(|sid| stanza_id_by_matches(&sid.by, by))
        .map(|sid| sid.id)
}

/// Extract the origin ID from a message.
pub fn extract_origin_id(msg: &Message) -> Option<OriginId> {
    msg.payloads
        .iter()
        .find(|e| is_origin_id_element(e))
        .and_then(|e| e.attr("id"))
        .filter(|id| !id.is_empty())
        .map(OriginId::new)
}

/// Extract the origin ID string from a message.
pub fn extract_origin_id_str(msg: &Message) -> Option<String> {
    extract_origin_id(msg).map(|o| o.id)
}

// ── Building ─────────────────────────────────────────────────────────

/// Build a `<stanza-id xmlns='urn:xmpp:sid:0' id='...' by='...'/>` element.
pub fn build_stanza_id_element(id: &str, by: &str) -> Element {
    Element::builder("stanza-id", NS_SID)
        .attr("id", id)
        .attr("by", by)
        .build()
}

/// Build an `<origin-id xmlns='urn:xmpp:sid:0' id='...'/>` element.
pub fn build_origin_id_element(id: &str) -> Element {
    Element::builder("origin-id", NS_SID).attr("id", id).build()
}

// ── Mutation ─────────────────────────────────────────────────────────

/// Add a `<stanza-id/>` to a message.
///
/// This is the primary function used by the server when archiving messages.
/// Multiple stanza-ids from different entities may coexist.
pub fn add_stanza_id(msg: &mut Message, id: &str, by: &str) {
    remove_stanza_ids_by(msg, by);
    msg.payloads.push(build_stanza_id_element(id, by));
}

/// Add an `<origin-id/>` to a message (no-op if already present).
pub fn add_origin_id(msg: &mut Message, id: &str) {
    if !has_origin_id(msg) {
        msg.payloads.push(build_origin_id_element(id));
    }
}

/// Remove all stanza-id elements assigned by a specific entity.
pub fn remove_stanza_ids_by(msg: &mut Message, by: &str) {
    msg.payloads.retain(|e| !stanza_id_matches_by(e, by));
}

/// Strip all stanza-id and origin-id elements from a message.
pub fn strip_all_ids(msg: &mut Message) {
    msg.payloads.retain(|e| e.ns() != NS_SID);
}

// ── Conversion ───────────────────────────────────────────────────────

impl From<xmpp_parsers::stanza_id::StanzaId> for StanzaId {
    fn from(sid: xmpp_parsers::stanza_id::StanzaId) -> Self {
        Self::new(sid.id, sid.by.to_string())
    }
}

impl From<xmpp_parsers::stanza_id::OriginId> for OriginId {
    fn from(oid: xmpp_parsers::stanza_id::OriginId) -> Self {
        Self::new(oid.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmpp_parsers::message::Message;

    #[test]
    fn test_is_stanza_id_element() {
        let elem = Element::builder("stanza-id", NS_SID)
            .attr("id", "abc")
            .attr("by", "room@muc.example.com")
            .build();
        assert!(is_stanza_id_element(&elem));

        let wrong = Element::builder("origin-id", NS_SID).build();
        assert!(!is_stanza_id_element(&wrong));
    }

    #[test]
    fn test_is_origin_id_element() {
        let elem = Element::builder("origin-id", NS_SID)
            .attr("id", "abc")
            .build();
        assert!(is_origin_id_element(&elem));

        let wrong = Element::builder("stanza-id", NS_SID).build();
        assert!(!is_origin_id_element(&wrong));
    }

    #[test]
    fn test_extract_stanza_ids() {
        let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <body>Hello</body>\
                    <stanza-id xmlns='urn:xmpp:sid:0' id='archive-1' by='room@muc.example.com'/>\
                    <stanza-id xmlns='urn:xmpp:sid:0' id='archive-2' by='example.com'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        let ids = extract_stanza_ids(&msg);
        assert_eq!(ids.len(), 2);
        assert_eq!(ids[0], StanzaId::new("archive-1", "room@muc.example.com"));
        assert_eq!(ids[1], StanzaId::new("archive-2", "example.com"));
    }

    #[test]
    fn test_extract_stanza_id_by() {
        let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <body>Hello</body>\
                    <stanza-id xmlns='urn:xmpp:sid:0' id='arc-1' by='room@muc.example.com'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        assert_eq!(
            extract_stanza_id_by(&msg, "room@muc.example.com"),
            Some("arc-1".to_owned())
        );
        assert_eq!(extract_stanza_id_by(&msg, "other@example.com"), None);
    }

    #[test]
    fn test_extract_stanza_id_by_matches_case_folded_bare_jid() {
        let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <body>Hello</body>\
                    <stanza-id xmlns='urn:xmpp:sid:0' id='arc-1' by='room@muc.example.COM'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        assert_eq!(
            extract_stanza_id_by(&msg, "room@muc.example.com"),
            Some("arc-1".to_owned())
        );
        assert_eq!(
            msg.stanza_id_by("room@muc.example.com"),
            Some("arc-1".to_owned())
        );
    }

    #[test]
    fn test_extract_origin_id() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <body>Hello</body>\
                    <origin-id xmlns='urn:xmpp:sid:0' id='client-uuid-1'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        let oid = extract_origin_id(&msg).expect("has origin-id");
        assert_eq!(oid.id, "client-uuid-1");
        assert_eq!(
            extract_origin_id_str(&msg),
            Some("client-uuid-1".to_owned())
        );
    }

    #[test]
    fn test_extract_origin_id_absent() {
        let msg = Message::new(None::<jid::Jid>);
        assert!(extract_origin_id(&msg).is_none());
    }

    #[test]
    fn test_extract_stanza_id_empty_attrs_ignored() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <stanza-id xmlns='urn:xmpp:sid:0' id='' by='example.com'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
        assert!(extract_stanza_ids(&msg).is_empty());
    }

    #[test]
    fn test_build_stanza_id_element() {
        let elem = build_stanza_id_element("arc-99", "room@muc.example.com");
        assert_eq!(elem.name(), "stanza-id");
        assert_eq!(elem.ns(), NS_SID);
        assert_eq!(elem.attr("id"), Some("arc-99"));
        assert_eq!(elem.attr("by"), Some("room@muc.example.com"));
    }

    #[test]
    fn test_build_origin_id_element() {
        let elem = build_origin_id_element("client-1");
        assert_eq!(elem.name(), "origin-id");
        assert_eq!(elem.ns(), NS_SID);
        assert_eq!(elem.attr("id"), Some("client-1"));
    }

    #[test]
    fn test_add_stanza_id() {
        let mut msg = Message::new(None::<jid::Jid>);
        add_stanza_id(&mut msg, "arc-1", "room@muc.example.com");
        add_stanza_id(&mut msg, "arc-2", "example.com");

        let ids = extract_stanza_ids(&msg);
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn test_add_stanza_id_replaces_existing_same_by() {
        let mut msg = Message::new(None::<jid::Jid>);
        msg.payloads
            .push(build_stanza_id_element("spoofed", "alice@example.com"));

        add_stanza_id(&mut msg, "fresh", "alice@example.com");

        let ids = extract_stanza_ids(&msg);
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0], StanzaId::new("fresh", "alice@example.com"));
    }

    #[test]
    fn test_add_origin_id() {
        let mut msg = Message::new(None::<jid::Jid>);
        add_origin_id(&mut msg, "client-1");
        assert!(has_origin_id(&msg));

        // Adding again is no-op
        add_origin_id(&mut msg, "client-2");
        assert_eq!(
            msg.payloads
                .iter()
                .filter(|e| is_origin_id_element(e))
                .count(),
            1
        );
    }

    #[test]
    fn test_remove_stanza_ids_by() {
        let mut msg = Message::new(None::<jid::Jid>);
        add_stanza_id(&mut msg, "arc-1", "room@muc.example.com");
        add_stanza_id(&mut msg, "arc-2", "example.com");

        remove_stanza_ids_by(&mut msg, "room@muc.example.com");
        let ids = extract_stanza_ids(&msg);
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0].by, "example.com");
    }

    #[test]
    fn test_remove_stanza_ids_by_matches_case_folded_bare_jid() {
        let mut msg = Message::new(None::<jid::Jid>);
        msg.payloads
            .push(build_stanza_id_element("arc-1", "room@muc.example.COM"));
        msg.payloads
            .push(build_stanza_id_element("arc-2", "example.com"));

        remove_stanza_ids_by(&mut msg, "room@muc.example.com");
        let ids = extract_stanza_ids(&msg);
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0].by, "example.com");
    }

    #[test]
    fn test_strip_all_ids() {
        let mut msg = Message::new(None::<jid::Jid>);
        add_stanza_id(&mut msg, "arc-1", "example.com");
        add_origin_id(&mut msg, "client-1");
        msg.payloads
            .push(Element::builder("body", "jabber:client").build());

        strip_all_ids(&mut msg);
        assert!(!has_stanza_id(&msg));
        assert!(!has_origin_id(&msg));
        // Non-SID payloads preserved
        assert_eq!(msg.payloads.len(), 1);
    }

    #[test]
    fn test_stanza_id_carrier_trait() {
        let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <body>Test</body>\
                    <stanza-id xmlns='urn:xmpp:sid:0' id='arc-1' by='room@muc.example.com'/>\
                    <origin-id xmlns='urn:xmpp:sid:0' id='client-1'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        assert!(msg.has_stanza_id());
        assert_eq!(
            msg.stanza_id_by("room@muc.example.com"),
            Some("arc-1".to_owned())
        );
        assert_eq!(msg.origin_id(), Some(OriginId::new("client-1")));
    }

    #[test]
    fn test_conversion_from_xmpp_parsers() {
        let sid: StanzaId = xmpp_parsers::stanza_id::StanzaId {
            id: "abc".to_owned(),
            by: "room@muc.example.com".parse().expect("valid jid"),
        }
        .into();
        assert_eq!(sid.id, "abc");
        assert_eq!(sid.by, "room@muc.example.com");

        let oid: OriginId = xmpp_parsers::stanza_id::OriginId {
            id: "def".to_owned(),
        }
        .into();
        assert_eq!(oid.id, "def");
    }
}
