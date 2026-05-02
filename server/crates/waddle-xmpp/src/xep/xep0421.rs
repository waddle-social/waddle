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
use std::sync::Arc;
use xmpp_parsers::message::Message;
use xmpp_parsers::presence::Presence;

/// Namespace for XEP-0421 Occupant Identifiers.
pub const NS_OCCUPANT_ID: &str = "urn:xmpp:occupant-id:0";

/// Minimum byte length of the deployment-keyed occupant-id secret.
///
/// XEP-0421 §3 specifies that the derivation be keyed by a per-deployment
/// secret to avoid cross-deployment linkability. The 32-byte floor is the
/// project's chosen entropy minimum; values below this are rejected at
/// `OccupantIdSecret` construction.
pub const OCCUPANT_ID_SECRET_MIN_BYTES: usize = 32;

type HmacSha256 = Hmac<Sha256>;

/// Per-deployment HMAC key used to derive XEP-0421 occupant identifiers.
///
/// The inner allocation is shared via `Arc<[u8]>` so the same secret can
/// flow through `WebSocketDeps`, `RoomRegistryActor`, every spawned
/// `RoomActor`, and `RoomContext` without copying the bytes.
///
/// `Debug` is hand-implemented to redact the value — never print, log, or
/// derive trace fields from this type. The bytes are accessible only via
/// `key()` for HMAC consumption.
#[derive(Clone)]
pub struct OccupantIdSecret(Arc<[u8]>);

impl OccupantIdSecret {
    /// Validate and wrap a deployment secret. Rejects values shorter than
    /// `OCCUPANT_ID_SECRET_MIN_BYTES`.
    pub fn new(bytes: impl Into<Arc<[u8]>>) -> Result<Self, OccupantIdSecretError> {
        let bytes: Arc<[u8]> = bytes.into();
        if bytes.len() < OCCUPANT_ID_SECRET_MIN_BYTES {
            return Err(OccupantIdSecretError::TooShort {
                got: bytes.len(),
                min: OCCUPANT_ID_SECRET_MIN_BYTES,
            });
        }
        Ok(Self(bytes))
    }

    /// Borrow the raw HMAC key bytes. The only legitimate consumer is
    /// `generate_occupant_id` (or its kameo/test equivalents).
    pub fn key(&self) -> &[u8] {
        &self.0
    }

    /// Test-only constructor that bypasses length validation. Lets unit
    /// tests use short labels like `b"test-secret"` without coupling them
    /// to the production length floor.
    #[cfg(test)]
    pub(crate) fn for_testing(bytes: impl Into<Arc<[u8]>>) -> Self {
        Self(bytes.into())
    }
}

impl std::fmt::Debug for OccupantIdSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("OccupantIdSecret")
            .field(&format_args!("<redacted, {} bytes>", self.0.len()))
            .finish()
    }
}

/// Failure reasons for `OccupantIdSecret::new`.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum OccupantIdSecretError {
    #[error("occupant-id secret must be at least {min} bytes, got {got}")]
    TooShort { got: usize, min: usize },
}

/// Bundled occupant-identity inputs for XEP-0045/0421 presence stamping.
///
/// Three fields that always travel together when a presence-builder needs
/// to stamp an `<occupant-id/>`:
///
/// - `bare_jid` — the user's bare JID. **Always required.** The room
///   service knows who the occupant is regardless of whether the real
///   JID is disclosed in the MUC `<item jid='…'>`. Used as the HMAC
///   message input.
/// - `real_jid` — whether to disclose the user's full JID in
///   `<item jid='…'>`. `Some` for non-anonymous and semi-anonymous
///   rooms; `None` for fully-anonymous rooms (XEP-0045 §15.6.4). The
///   choice is independent of `bare_jid` — a fully-anonymous room
///   still stamps occupant-id (XEP-0421 §"Business Rules"), it just
///   doesn't expose the real JID.
/// - `secret` — the per-deployment HMAC key.
///
/// Bundling these three drops public `build_*_presence` argument counts
/// below clippy's `too_many_arguments` threshold without an `#[allow]`,
/// and is the typed-payloads-rule-conformant shape for "occupant
/// identity context" crossing the legacy presence builders' boundary.
pub struct OccupantIdentity<'a> {
    pub bare_jid: &'a jid::BareJid,
    pub real_jid: Option<&'a jid::FullJid>,
    pub secret: &'a OccupantIdSecret,
}

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
///
/// Inputs are joined with a `0x00` byte (per XEP-0421 §3 example) which
/// cannot appear in a bare JID per RFC 7622 / PRECIS, so the boundary is
/// unambiguous. Earlier revisions used `:` as a separator, which is a
/// valid JID-localpart character and produced collisions like
/// `room=a@x` + `user=b:c@y` ↔ `room=a@x:b` + `user=c@y`.
pub fn generate_occupant_id(
    user_bare_jid: &jid::BareJid,
    room_jid: &jid::BareJid,
    server_secret: &OccupantIdSecret,
) -> OccupantId {
    let mut mac =
        HmacSha256::new_from_slice(server_secret.key()).expect("HMAC accepts any key length");
    // `BareJid::as_str` returns the canonical string form; we hash
    // bytes at the I/O boundary only.
    mac.update(room_jid.as_str().as_bytes());
    mac.update(&[0u8]);
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

    fn test_secret() -> OccupantIdSecret {
        OccupantIdSecret::new(b"waddle-test-secret-key-for-occupant-ids".to_vec())
            .expect("test secret meets length floor")
    }

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
        let secret = test_secret();
        let id1 = generate_occupant_id(&alice(), &room(), &secret);
        let id2 = generate_occupant_id(&alice(), &room(), &secret);
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_generate_different_users() {
        let secret = test_secret();
        let alice_id = generate_occupant_id(&alice(), &room(), &secret);
        let bob_id = generate_occupant_id(&bob(), &room(), &secret);
        assert_ne!(alice_id, bob_id);
    }

    #[test]
    fn test_generate_different_rooms() {
        let secret = test_secret();
        let r1 = generate_occupant_id(&alice(), &room1(), &secret);
        let r2 = generate_occupant_id(&alice(), &room2(), &secret);
        assert_ne!(r1, r2);
    }

    #[test]
    fn test_generate_different_secrets_produce_different_ids() {
        // XEP-0421 §3: the deployment-keyed derivation MUST make occupant-ids
        // unlinkable across deployments using different secrets. Same (user,
        // room) inputs with different keys must yield different ids.
        let secret_a = OccupantIdSecret::new(b"deployment-a-secret-32-bytes-long".to_vec())
            .expect("≥32 bytes");
        let secret_b = OccupantIdSecret::new(b"deployment-b-secret-32-bytes-long".to_vec())
            .expect("≥32 bytes");
        let id_a = generate_occupant_id(&alice(), &room(), &secret_a);
        let id_b = generate_occupant_id(&alice(), &room(), &secret_b);
        assert_ne!(id_a, id_b);
    }

    #[test]
    fn test_generate_id_length() {
        let secret = test_secret();
        let id = generate_occupant_id(&alice(), &room(), &secret);
        // 16 bytes = 32 hex chars
        assert_eq!(id.0.len(), 32);
    }

    #[test]
    fn test_secret_rejects_short_input() {
        let result = OccupantIdSecret::new(b"too-short".to_vec());
        assert_eq!(
            result.unwrap_err(),
            OccupantIdSecretError::TooShort {
                got: 9,
                min: OCCUPANT_ID_SECRET_MIN_BYTES,
            }
        );
    }

    #[test]
    fn test_secret_accepts_minimum_length() {
        let bytes = vec![0x42u8; OCCUPANT_ID_SECRET_MIN_BYTES];
        OccupantIdSecret::new(bytes).expect("32 bytes meets floor");
    }

    #[test]
    fn test_secret_debug_redacts_bytes() {
        // Build the secret from a known marker substring. The `Debug`
        // impl MUST redact the bytes; we then check the rendered form
        // contains "redacted" and does NOT contain the marker. We
        // deliberately do NOT interpolate the rendered string into any
        // panic / assertion message — doing so would taint-flow the
        // secret-derived value into a panic-log sink (false-positive
        // for CodeQL `rust/cleartext-logging`, but also pointless: if
        // redaction failed, the panic message would itself leak the
        // bytes during test failure).
        const MARKER: &str = "do-not-leak-this-byte-string-32b!!!!!";
        let secret = OccupantIdSecret::new(MARKER.as_bytes().to_vec()).expect("≥32 bytes");
        let rendered = format!("{secret:?}");
        let redacted = rendered.contains("redacted");
        let leaked = rendered.contains(MARKER);
        assert!(redacted, "Debug output should contain 'redacted'");
        assert!(!leaked, "Debug must not leak secret bytes");
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

    // ── XEP-0421 §3 occupant-id on the XEP-0045 §7.2.15 subject message
    //
    // The acceptance criterion in #304 is that the subject-message
    // emission carries an `<occupant-id>` whose id equals the
    // deterministic HMAC for the setter. Tested at the builder
    // boundary (`muc::messages::build_subject_message`) which is the
    // single emission site for the historical join-time subject.

    #[test]
    fn xep_0421_subject_message_stamps_occupant_id_for_setter() {
        use crate::muc::messages::build_subject_message;
        use crate::muc::SubjectState;
        use chrono::TimeZone;
        use jid::{BareJid, FullJid};

        let room: BareJid = "team@muc.example.com".parse().expect("valid bare jid");
        let to: FullJid = "joiner@example.com/web".parse().expect("valid full jid");
        let secret = OccupantIdSecret::for_testing(b"xep0421-subject-test".to_vec());
        let setter: BareJid = "alice@example.com".parse().expect("valid bare jid");
        let mut texts = std::collections::BTreeMap::new();
        texts.insert(String::new(), "topic".to_string());
        let state = SubjectState {
            texts,
            setter: setter.clone(),
            setter_nick: "alice-nick".to_string(),
            set_at: chrono::Utc.with_ymd_and_hms(2026, 5, 2, 12, 0, 0).unwrap(),
        };

        let msg = build_subject_message(&room, &to, Some(&state), &secret);
        let id = extract_occupant_id_from_message(&msg).expect("occupant-id MUST be present");
        let expected = generate_occupant_id(&setter, &room, &secret);
        assert_eq!(id, expected, "id is the deterministic HMAC of the setter");
    }

    #[test]
    fn xep_0421_subject_message_omits_occupant_id_when_no_setter_is_known() {
        // Pins the documented spec-gap: never-set rooms emit empty
        // <subject/> with no occupant-id, matching established servers.
        // A future "always stamp" change would silently violate the
        // unlinkability semantics of XEP-0421 §3 by fabricating input.
        use crate::muc::messages::build_subject_message;
        use jid::{BareJid, FullJid};

        let room: BareJid = "team@muc.example.com".parse().expect("valid bare jid");
        let to: FullJid = "joiner@example.com/web".parse().expect("valid full jid");
        let secret = OccupantIdSecret::for_testing(b"xep0421-subject-test".to_vec());
        let msg = build_subject_message(&room, &to, None, &secret);
        assert!(
            extract_occupant_id_from_message(&msg).is_none(),
            "never-set room MUST omit occupant-id (no setter input)"
        );
    }
}
