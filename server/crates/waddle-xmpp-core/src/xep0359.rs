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

use minidom::Element;
use serde::{Deserialize, Serialize};
use xmpp_parsers::message::Message;

/// Namespace for XEP-0359 Unique and Stable Stanza IDs.
pub const NS_SID: &str = "urn:xmpp:sid:0";

/// A server-assigned stable stanza ID.
///
/// This is the canonical workspace shape for an XEP-0359 stanza-id at every
/// boundary above the wire — handler outputs, archive payloads, inbox
/// projections, pending-delivery references — so the `id` cannot drift
/// from its assigning `by` archive across crate boundaries.
///
/// Refs:
/// - XEP-0359 §3 (`<stanza-id/>` element)
/// - issue #329 (consolidation of duplicate StanzaId-ish newtypes)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StanzaId {
    /// The stable ID assigned by the server/service.
    pub id: String,
    /// The JID of the entity that assigned this ID.
    pub by: jid::Jid,
}

impl StanzaId {
    /// Create a new stanza ID.
    pub fn new(id: impl Into<String>, by: jid::Jid) -> Self {
        Self { id: id.into(), by }
    }

    /// Borrow the opaque id portion as a string slice.
    pub fn as_str(&self) -> &str {
        &self.id
    }
}

/// A client-assigned origin ID.
///
/// Per XEP-0359 §3 the origin-id is a client-stamped opaque identifier with
/// no `by` context (the originating entity is implied by the message's
/// `from`). This is the canonical workspace shape; the protocol-layer
/// `OriginIdValue` newtype was removed in the #329 consolidation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OriginId {
    /// The unique ID assigned by the originating client.
    pub id: String,
}

impl OriginId {
    /// Create a new origin ID.
    pub fn new(id: impl Into<String>) -> Self {
        Self { id: id.into() }
    }

    /// Borrow the opaque id portion as a string slice.
    pub fn as_str(&self) -> &str {
        &self.id
    }
}

impl std::fmt::Display for StanzaId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.id)
    }
}

impl std::fmt::Display for OriginId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.id)
    }
}

/// Trait for types that can carry stanza ID elements.
pub trait StanzaIdCarrier {
    /// Extract all stanza IDs from this carrier.
    fn stanza_ids(&self) -> Vec<StanzaId>;

    /// Extract the stanza ID assigned by a specific entity.
    fn stanza_id_by(&self, by: &jid::Jid) -> Option<String> {
        self.stanza_ids()
            .into_iter()
            .find(|sid| &sid.by == by)
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
///
/// Stanza-id elements whose `by` attribute is missing, empty, or fails to
/// parse as a JID are silently skipped, consistent with the typed-payloads
/// rule (untyped input is dropped at the boundary).
pub fn extract_stanza_ids(msg: &Message) -> Vec<StanzaId> {
    msg.payloads
        .iter()
        .filter(|e| is_stanza_id_element(e))
        .filter_map(|e| {
            let id = e.attr("id").filter(|s| !s.is_empty())?;
            let by_raw = e.attr("by").filter(|s| !s.is_empty())?;
            let by = by_raw.parse::<jid::Jid>().ok()?;
            Some(StanzaId::new(id, by))
        })
        .collect()
}

/// Extract the stanza ID assigned by a specific entity.
pub fn extract_stanza_id_by(msg: &Message, by: &jid::Jid) -> Option<String> {
    extract_stanza_ids(msg)
        .into_iter()
        .find(|sid| &sid.by == by)
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
pub fn build_stanza_id_element(id: &str, by: &jid::Jid) -> Element {
    Element::builder("stanza-id", NS_SID)
        .attr("id", id)
        .attr("by", by.to_string())
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
///
/// Takes a typed [`StanzaId`] so callers cannot accidentally emit a
/// `<stanza-id id='...'/>` element without the XEP-0359 §3 REQUIRED
/// `by` attribute — the typed value carries both fields together.
///
/// Pre-existing `<stanza-id/>` elements with the same `by` are removed
/// first so a new authoritative stamp from `by` overwrites any
/// spoofed value the upstream sender may have included (XEP-0359 §3:
/// clients MUST NOT trust foreign-stamped `<stanza-id/>` elements).
pub fn add_stanza_id(msg: &mut Message, sid: &StanzaId) {
    remove_stanza_ids_by(msg, &sid.by);
    msg.payloads.push(build_stanza_id_element(&sid.id, &sid.by));
}

/// Add an `<origin-id/>` to a message (no-op if already present).
pub fn add_origin_id(msg: &mut Message, id: &str) {
    if !has_origin_id(msg) {
        msg.payloads.push(build_origin_id_element(id));
    }
}

/// Remove all stanza-id elements assigned by a specific entity.
///
/// Compares the element's `by` attribute as a JID where possible (so
/// `room@muc.example.COM` matches `room@muc.example.com` per RFC 7622
/// case-folding rules). Falls back to byte-exact comparison for
/// elements whose `by` attribute does not parse as a JID.
pub fn remove_stanza_ids_by(msg: &mut Message, by: &jid::Jid) {
    msg.payloads.retain(|e| !stanza_id_matches_by(e, by));
}

fn stanza_id_matches_by(elem: &Element, by: &jid::Jid) -> bool {
    if !is_stanza_id_element(elem) {
        return false;
    }
    let Some(existing) = elem.attr("by") else {
        return false;
    };
    match existing.parse::<jid::Jid>() {
        Ok(existing_jid) => &existing_jid == by,
        Err(_) => existing == by.to_string(),
    }
}

/// Strip all stanza-id and origin-id elements from a message.
pub fn strip_all_ids(msg: &mut Message) {
    msg.payloads.retain(|e| e.ns() != NS_SID);
}

// ── Conversion ───────────────────────────────────────────────────────

impl From<xmpp_parsers::stanza_id::StanzaId> for StanzaId {
    fn from(sid: xmpp_parsers::stanza_id::StanzaId) -> Self {
        Self::new(sid.id, sid.by)
    }
}

impl From<xmpp_parsers::stanza_id::OriginId> for OriginId {
    fn from(oid: xmpp_parsers::stanza_id::OriginId) -> Self {
        Self::new(oid.id)
    }
}

#[cfg(test)]
mod tests;
