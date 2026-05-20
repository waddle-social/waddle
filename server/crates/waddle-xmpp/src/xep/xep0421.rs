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

use hmac::{Hmac, KeyInit, Mac};
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
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id.as_str())
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
mod tests;
