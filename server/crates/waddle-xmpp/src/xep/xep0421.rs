//! XEP-0421: Occupant Identifiers for Semi-Anonymous MUCs
//!
//! Provides stable, opaque identifiers for MUC room occupants. These IDs
//! remain consistent even when a user changes their nickname, allowing
//! clients to track the same person across nick changes.
//!
//! ## XML Format
//!
//! Added by the MUC service to every message and presence:
//! ```xml
//! <message from='room@muc.example.com/nick' type='groupchat'>
//!   <body>Hello!</body>
//!   <occupant-id xmlns='urn:xmpp:occupant-id:0' id='opaque-stable-id'/>
//! </message>
//! ```
//!
//! ## ID Generation
//!
//! The occupant ID is derived from the user's real bare JID and the room JID
//! using HMAC-SHA-256, producing a stable but opaque identifier:
//! - Same user in same room → same ID (even across sessions)
//! - Same user in different room → different ID
//! - Different users → different IDs
//!
//! ## Server Behavior
//!
//! The MUC service MUST:
//! - Add `<occupant-id/>` to every groupchat message and MUC presence
//! - Strip any `<occupant-id/>` received from clients (prevent spoofing)
//! - Use a consistent generation algorithm

use hmac::{Hmac, Mac};
use minidom::Element;
use sha2::Sha256;
use xmpp_parsers::message::Message;
use xmpp_parsers::presence::Presence;

/// Namespace for XEP-0421 Occupant Identifiers.
pub const NS_OCCUPANT_ID: &str = "urn:xmpp:occupant-id:0";

type HmacSha256 = Hmac<Sha256>;

/// An opaque occupant identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OccupantId(pub String);

impl OccupantId {
    /// Create an occupant ID from a pre-computed string.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Get the ID string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for OccupantId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Trait for types that can carry an occupant identifier.
pub trait OccupantIdCarrier {
    /// Extract the occupant ID from this carrier, if present.
    fn occupant_id(&self) -> Option<OccupantId>;

    /// Returns `true` if this carrier has an occupant ID.
    fn has_occupant_id(&self) -> bool {
        self.occupant_id().is_some()
    }
}

impl OccupantIdCarrier for Message {
    fn occupant_id(&self) -> Option<OccupantId> {
        extract_occupant_id_from_payloads(&self.payloads)
    }
}

impl OccupantIdCarrier for Presence {
    fn occupant_id(&self) -> Option<OccupantId> {
        extract_occupant_id_from_payloads(&self.payloads)
    }
}

// ── Generation ───────────────────────────────────────────────────────

/// Generate a stable occupant ID from a user's bare JID and room JID.
///
/// Uses HMAC-SHA-256 with the deployment server-secret as key and the
/// room + user JID as message, producing a hex-encoded opaque
/// identifier. The same inputs always produce the same output.
///
/// Takes typed JIDs (typed-payloads hard rule) — the canonical JID
/// string form is generated at the HMAC boundary only, never carried
/// across function boundaries as `String`.
pub fn generate_occupant_id(
    user_bare_jid: &jid::BareJid,
    room_jid: &jid::BareJid,
    server_secret: &[u8],
) -> OccupantId {
    let mut mac = HmacSha256::new_from_slice(server_secret).expect("HMAC accepts any key length");
    // `BareJid::as_str` returns the canonical string form; we hash
    // bytes at the I/O boundary only.
    mac.update(room_jid.as_str().as_bytes());
    mac.update(b":");
    mac.update(user_bare_jid.as_str().as_bytes());
    let result = mac.finalize().into_bytes();

    // Use first 16 bytes (128 bits) for a shorter but still unique ID
    let hex: String = result[..16].iter().map(|b| format!("{b:02x}")).collect();
    OccupantId(hex)
}

// ── Detection ────────────────────────────────────────────────────────

/// Check if an element is an `<occupant-id/>` element.
pub fn is_occupant_id_element(elem: &Element) -> bool {
    elem.ns() == NS_OCCUPANT_ID && elem.name() == "occupant-id"
}

// ── Extraction ───────────────────────────────────────────────────────

fn extract_occupant_id_from_payloads(payloads: &[Element]) -> Option<OccupantId> {
    payloads
        .iter()
        .find(|e| is_occupant_id_element(e))
        .and_then(|e| e.attr("id"))
        .filter(|id| !id.is_empty())
        .map(OccupantId::new)
}

/// Extract occupant ID from a message.
pub fn extract_occupant_id_from_message(msg: &Message) -> Option<OccupantId> {
    extract_occupant_id_from_payloads(&msg.payloads)
}

/// Extract occupant ID from a presence.
pub fn extract_occupant_id_from_presence(presence: &Presence) -> Option<OccupantId> {
    extract_occupant_id_from_payloads(&presence.payloads)
}

// ── Building ─────────────────────────────────────────────────────────

/// Build an `<occupant-id xmlns='urn:xmpp:occupant-id:0' id='...'/>` element.
pub fn build_occupant_id_element(id: &OccupantId) -> Element {
    Element::builder("occupant-id", NS_OCCUPANT_ID)
        .attr("id", id.as_str())
        .build()
}

// ── Mutation ─────────────────────────────────────────────────────────

/// Add or replace occupant ID on a message.
///
/// The MUC service should call this before broadcasting messages.
pub fn set_occupant_id_on_message(msg: &mut Message, id: &OccupantId) {
    strip_occupant_id_from_message(msg);
    msg.payloads.push(build_occupant_id_element(id));
}

/// Add or replace occupant ID on a presence.
pub fn set_occupant_id_on_presence(presence: &mut Presence, id: &OccupantId) {
    strip_occupant_id_from_presence(presence);
    presence.payloads.push(build_occupant_id_element(id));
}

/// Strip any client-provided occupant ID from a message (anti-spoofing).
pub fn strip_occupant_id_from_message(msg: &mut Message) {
    msg.payloads.retain(|e| e.ns() != NS_OCCUPANT_ID);
}

/// Strip any client-provided occupant ID from a presence (anti-spoofing).
pub fn strip_occupant_id_from_presence(presence: &mut Presence) {
    presence.payloads.retain(|e| e.ns() != NS_OCCUPANT_ID);
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmpp_parsers::message::MessageType;

    const TEST_SECRET: &[u8] = b"waddle-test-secret-key-for-occupant-ids";

    fn alice() -> jid::BareJid {
        "alice@example.com".parse().expect("bare")
    }
    fn bob() -> jid::BareJid {
        "bob@example.com".parse().expect("bare")
    }
    fn room() -> jid::BareJid {
        "room@muc.example.com".parse().expect("bare")
    }
    fn room1() -> jid::BareJid {
        "room1@muc.example.com".parse().expect("bare")
    }
    fn room2() -> jid::BareJid {
        "room2@muc.example.com".parse().expect("bare")
    }

    #[test]
    fn test_generate_occupant_id_deterministic() {
        let id1 = generate_occupant_id(&alice(), &room(), TEST_SECRET);
        let id2 = generate_occupant_id(&alice(), &room(), TEST_SECRET);
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_generate_different_users() {
        let alice_id = generate_occupant_id(&alice(), &room(), TEST_SECRET);
        let bob_id = generate_occupant_id(&bob(), &room(), TEST_SECRET);
        assert_ne!(alice_id, bob_id);
    }

    #[test]
    fn test_generate_different_rooms() {
        let r1 = generate_occupant_id(&alice(), &room1(), TEST_SECRET);
        let r2 = generate_occupant_id(&alice(), &room2(), TEST_SECRET);
        assert_ne!(r1, r2);
    }

    #[test]
    fn test_generate_id_length() {
        let id = generate_occupant_id(&alice(), &room(), TEST_SECRET);
        // 16 bytes = 32 hex chars
        assert_eq!(id.0.len(), 32);
    }

    #[test]
    fn test_is_occupant_id_element() {
        let elem = Element::builder("occupant-id", NS_OCCUPANT_ID)
            .attr("id", "abc123")
            .build();
        assert!(is_occupant_id_element(&elem));

        let wrong = Element::builder("occupant-id", "jabber:client").build();
        assert!(!is_occupant_id_element(&wrong));
    }

    #[test]
    fn test_extract_from_message() {
        let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <body>Hello</body>\
                    <occupant-id xmlns='urn:xmpp:occupant-id:0' id='abc123def456'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        let oid = extract_occupant_id_from_message(&msg).expect("has occupant-id");
        assert_eq!(oid.as_str(), "abc123def456");
    }

    #[test]
    fn test_extract_from_presence() {
        let xml = "<presence xmlns='jabber:client'>\
                    <occupant-id xmlns='urn:xmpp:occupant-id:0' id='xyz789'/>\
                    </presence>";
        let presence =
            Presence::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid presence");

        let oid = extract_occupant_id_from_presence(&presence).expect("has occupant-id");
        assert_eq!(oid.as_str(), "xyz789");
    }

    #[test]
    fn test_extract_absent() {
        let msg = Message::new(None::<jid::Jid>);
        assert!(extract_occupant_id_from_message(&msg).is_none());
    }

    #[test]
    fn test_extract_empty_id_ignored() {
        let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <occupant-id xmlns='urn:xmpp:occupant-id:0' id=''/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
        assert!(extract_occupant_id_from_message(&msg).is_none());
    }

    #[test]
    fn test_build_occupant_id_element() {
        let id = OccupantId::new("abc123");
        let elem = build_occupant_id_element(&id);

        assert_eq!(elem.name(), "occupant-id");
        assert_eq!(elem.ns(), NS_OCCUPANT_ID);
        assert_eq!(elem.attr("id"), Some("abc123"));
    }

    #[test]
    fn test_set_occupant_id_on_message() {
        let mut msg = Message::new(None::<jid::Jid>);
        msg.type_ = MessageType::Groupchat;
        let id = OccupantId::new("test-id");

        set_occupant_id_on_message(&mut msg, &id);
        assert_eq!(
            extract_occupant_id_from_message(&msg),
            Some(OccupantId::new("test-id"))
        );

        // Replace
        let id2 = OccupantId::new("new-id");
        set_occupant_id_on_message(&mut msg, &id2);
        assert_eq!(
            extract_occupant_id_from_message(&msg),
            Some(OccupantId::new("new-id"))
        );
        assert_eq!(
            msg.payloads
                .iter()
                .filter(|e| e.ns() == NS_OCCUPANT_ID)
                .count(),
            1
        );
    }

    #[test]
    fn test_strip_occupant_id_anti_spoofing() {
        let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <body>Hello</body>\
                    <occupant-id xmlns='urn:xmpp:occupant-id:0' id='spoofed'/>\
                    </message>";
        let mut msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        strip_occupant_id_from_message(&mut msg);
        assert!(extract_occupant_id_from_message(&msg).is_none());
        assert!(!msg.bodies.is_empty()); // body preserved
    }

    #[test]
    fn test_occupant_id_carrier_trait_message() {
        let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <occupant-id xmlns='urn:xmpp:occupant-id:0' id='trait-test'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        assert!(msg.has_occupant_id());
        assert_eq!(msg.occupant_id(), Some(OccupantId::new("trait-test")));
    }

    #[test]
    fn test_occupant_id_carrier_trait_presence() {
        let xml = "<presence xmlns='jabber:client'>\
                    <occupant-id xmlns='urn:xmpp:occupant-id:0' id='pres-test'/>\
                    </presence>";
        let presence =
            Presence::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid presence");

        assert!(presence.has_occupant_id());
        assert_eq!(presence.occupant_id(), Some(OccupantId::new("pres-test")));
    }

    #[test]
    fn test_occupant_id_display() {
        let id = OccupantId::new("display-test");
        assert_eq!(id.to_string(), "display-test");
    }
}
