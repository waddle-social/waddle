//! Session-bounded state carried in [`super::message_context::MessageContext`].
//!
//! Per Q5 of the sans-I/O migration design (issue #229), session-bounded
//! state is read **synchronously** by message handlers via a snapshot in
//! [`super::message_context::MessageContext`], rather than fetched async
//! through callback events. Three concerns fall in that bucket:
//!
//! - [`Blocklist`]   — XEP-0191 block list, mutated by IQ handlers.
//! - [`CarbonsState`] — XEP-0280 per-connection carbons-enabled flag.
//! - [`MucOccupancy`] — XEP-0045 currently-joined-rooms-with-nick map.
//!
//! Query-shaped state (MAM lookups, archived-message-by-id) stays on the
//! two-phase callback path in [`super::event::OutboundEvent`].
//!
//! Locality classification ([`Locality`]) sits alongside this state since
//! it is derived once at dispatch start from the connection's bound JID
//! and the inbound message's `from`/`to`.

use jid::{BareJid, FullJid, Jid};
use std::collections::{BTreeSet, HashMap};
use xmpp_parsers::message::Message;

/// True when `candidate` denotes the same full JID as `local` — i.e. it
/// is itself a full JID and equal to `local`.
fn jid_equals_full(candidate: &Jid, local: &FullJid) -> bool {
    candidate
        .resource()
        .map(|_| {
            // Full JID: exact equality.
            candidate.to_string() == local.to_string()
        })
        .unwrap_or(false)
}

/// True when `to` addresses `local`: a full `to` requires exact match;
/// a bare `to` matches any resource of `local`'s bare JID.
fn jid_addresses_full(to: &Jid, local: &FullJid) -> bool {
    if to.resource().is_some() {
        // Full JID: must exactly select this resource.
        to.to_string() == local.to_string()
    } else {
        // Bare JID: server may deliver to any of the user's resources.
        to.to_bare() == local.to_bare()
    }
}

/// XEP-0191 blocklist for the connection's bound user.
///
/// Stored as a [`BTreeSet`] because the read path is "is this peer in the
/// list" and the typical list size is in the dozens, not millions —
/// `O(log n)` is fine and the tree gives us a stable iteration order for
/// snapshots and tests.
#[derive(Debug, Default, Clone)]
pub struct Blocklist {
    entries: BTreeSet<BareJid>,
}

impl Blocklist {
    /// Build a blocklist from its bare JID entries.
    pub fn new(entries: impl IntoIterator<Item = BareJid>) -> Self {
        Self {
            entries: entries.into_iter().collect(),
        }
    }

    /// Empty blocklist — used as the `MessageContext` default before any
    /// XEP-0191 entries are loaded.
    pub fn empty() -> Self {
        Self::default()
    }

    /// True when the bare JID of `peer` matches any entry.
    ///
    /// XEP-0191 entries can be full JIDs, bare JIDs, or domain JIDs; for
    /// the message-pipeline read path we currently match on the peer's
    /// bare JID only. Domain-JID and full-JID matching land alongside the
    /// XEP-0191 IQ-set handler in a later PR.
    pub fn contains(&self, peer: &BareJid) -> bool {
        self.entries.contains(peer)
    }

    /// Iterate over the entries in the blocklist.
    pub fn iter(&self) -> impl Iterator<Item = &BareJid> {
        self.entries.iter()
    }
}

/// Per-connection XEP-0280 carbons enable/disable state.
///
/// Carbons default to `Disabled` until the client sends an
/// `<enable xmlns='urn:xmpp:carbons:2'/>` IQ-set.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum CarbonsState {
    /// XEP-0280 carbons are not active for this connection.
    #[default]
    Disabled,
    /// XEP-0280 carbons have been enabled for this connection.
    Enabled,
}

impl CarbonsState {
    /// True when carbons are active and fan-out should run.
    pub fn is_enabled(self) -> bool {
        matches!(self, CarbonsState::Enabled)
    }
}

/// Snapshot of one occupant's identity within a single MUC room.
#[derive(Debug, Clone)]
pub struct OccupancyEntry {
    /// The nickname under which the user is currently joined.
    pub nick: String,
    /// Generation counter incremented on every join/leave; lets handlers
    /// distinguish a stale presence from the current one without a
    /// timestamp.
    pub generation: u64,
}

/// XEP-0045 occupancy snapshot: which rooms is this connection currently
/// joined to, with what nick and generation.
///
/// The `RouteHandler`'s groupchat branch reads this to enforce the
/// "non-occupant cannot send a groupchat message" rule (XEP-0045 §7.4).
#[derive(Debug, Default, Clone)]
pub struct MucOccupancy {
    rooms: HashMap<BareJid, OccupancyEntry>,
}

impl MucOccupancy {
    /// Build occupancy from `(room, entry)` pairs.
    pub fn new(rooms: impl IntoIterator<Item = (BareJid, OccupancyEntry)>) -> Self {
        Self {
            rooms: rooms.into_iter().collect(),
        }
    }

    /// Empty occupancy — used as the default before any MUC presence has
    /// been processed.
    pub fn empty() -> Self {
        Self::default()
    }

    /// True when this connection is currently joined to `room`.
    pub fn is_occupant(&self, room: &BareJid) -> bool {
        self.rooms.contains_key(room)
    }

    /// Look up the occupancy entry for `room`.
    pub fn get(&self, room: &BareJid) -> Option<&OccupancyEntry> {
        self.rooms.get(room)
    }
}

/// Per-message classification of the local user's role.
///
/// Derived once per dispatch from `(ctx.full_jid, message.from, message.to)`
/// and passed into [`super::message_context::MessageContext`] so handlers
/// don't re-derive on every read.
///
/// In Waddle's single-server model a local-to-local message is processed
/// twice: first on the sender's connection (locality = `Sender`), then on
/// the recipient's connection (locality = `Recipient`) after the
/// interpreter feeds [`super::event::OutboundEvent::RouteToConnection`]
/// to the destination machine as [`super::event::InboundEvent::StanzaFromPeer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locality {
    /// The local user is the sender of this message.
    Sender,
    /// The local user is the recipient of this message.
    Recipient,
    /// The local user is both sender and recipient — typically a
    /// self-message between two of their own resources.
    Both,
    /// Neither sender nor recipient matches the local user — should not
    /// happen in normal C2S; emitted as a diagnostic case.
    Neither,
}

impl Locality {
    /// Classify the message against the local connection's bound JID.
    ///
    /// Matching is **asymmetric** by design:
    ///
    /// - `from` MUST match the local full JID exactly. After
    ///   authentication every outbound stanza carries the originating
    ///   resource in `from`, so a bare-only match would mis-classify a
    ///   stanza coming from `alice@x/phone` as "sender" on
    ///   `alice@x/web`'s connection — duplicating sender-side side
    ///   effects across resources.
    /// - `to` matches a bare JID by bare equality (the typical XMPP
    ///   recipient address) and a full JID by full equality. This
    ///   ensures `alice/web -> alice/phone` is **only** Recipient on
    ///   alice/phone's connection, not on alice/web's.
    ///
    /// Without this asymmetry, a self-multi-resource flow would be
    /// classified `Locality::Both` on every connection, and PR2's
    /// blocking / archive / inbox / carbons handlers would fire
    /// duplicated side effects across resources.
    pub fn derive(local: &FullJid, message: &Message) -> Self {
        let from_matches = message
            .from
            .as_ref()
            .map(|j| jid_equals_full(j, local))
            .unwrap_or(false);
        let to_matches = message
            .to
            .as_ref()
            .map(|j| jid_addresses_full(j, local))
            .unwrap_or(false);

        match (from_matches, to_matches) {
            (true, true) => Locality::Both,
            (true, false) => Locality::Sender,
            (false, true) => Locality::Recipient,
            (false, false) => Locality::Neither,
        }
    }

    /// True when the local user is the sender (or both ends).
    pub fn is_sender(self) -> bool {
        matches!(self, Locality::Sender | Locality::Both)
    }

    /// True when the local user is the recipient (or both ends).
    pub fn is_recipient(self) -> bool {
        matches!(self, Locality::Recipient | Locality::Both)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmpp_parsers::message::{Message, MessageType};

    fn bare(s: &str) -> BareJid {
        s.parse().expect("valid bare jid")
    }

    fn full(s: &str) -> FullJid {
        s.parse().expect("valid full jid")
    }

    fn msg_from_to(from: Option<&str>, to: Option<&str>) -> Message {
        let mut m = Message::new(to.map(|t| t.parse().expect("jid")));
        m.from = from.map(|f| f.parse().expect("jid"));
        m.type_ = MessageType::Chat;
        m
    }

    #[test]
    fn blocklist_contains_matches_bare_jid() {
        let bl = Blocklist::new([bare("blocked@example.com")]);
        assert!(bl.contains(&bare("blocked@example.com")));
        assert!(!bl.contains(&bare("ok@example.com")));
    }

    #[test]
    fn carbons_state_default_is_disabled() {
        assert_eq!(CarbonsState::default(), CarbonsState::Disabled);
        assert!(!CarbonsState::Disabled.is_enabled());
        assert!(CarbonsState::Enabled.is_enabled());
    }

    #[test]
    fn muc_occupancy_lookup_by_room() {
        let occ = MucOccupancy::new([(
            bare("room@conf.example"),
            OccupancyEntry {
                nick: "alice".to_string(),
                generation: 1,
            },
        )]);
        assert!(occ.is_occupant(&bare("room@conf.example")));
        assert!(!occ.is_occupant(&bare("other@conf.example")));
        assert_eq!(
            occ.get(&bare("room@conf.example")).map(|e| e.nick.as_str()),
            Some("alice")
        );
    }

    #[test]
    fn locality_derive_sender_when_from_matches_local() {
        let local = full("alice@example.com/web");
        let m = msg_from_to(Some("alice@example.com/web"), Some("bob@example.com"));
        assert_eq!(Locality::derive(&local, &m), Locality::Sender);
    }

    #[test]
    fn locality_derive_recipient_when_to_matches_local() {
        let local = full("bob@example.com/desk");
        let m = msg_from_to(Some("alice@example.com/web"), Some("bob@example.com"));
        assert_eq!(Locality::derive(&local, &m), Locality::Recipient);
    }

    #[test]
    fn locality_derive_both_only_when_self_message_targets_same_full_jid() {
        // Sending alice/phone -> alice/web: on alice/web the local
        // user is *only* recipient (the sender resource is different).
        // Bare-only matching would classify this as Both and duplicate
        // sender-side side effects on alice/web — which is the bug
        // the asymmetric derive fixes.
        let alice_web = full("alice@example.com/web");
        let cross_resource = msg_from_to(
            Some("alice@example.com/phone"),
            Some("alice@example.com/web"),
        );
        assert_eq!(
            Locality::derive(&alice_web, &cross_resource),
            Locality::Recipient
        );

        // True self-loop where from and to are both `alice/web`: this is
        // the only legitimate `Both` case at the wire level.
        let true_self_loop =
            msg_from_to(Some("alice@example.com/web"), Some("alice@example.com/web"));
        assert_eq!(
            Locality::derive(&alice_web, &true_self_loop),
            Locality::Both
        );
    }

    #[test]
    fn locality_derive_recipient_when_to_is_bare_jid_of_local_user() {
        // Typical case: `alice/web -> bob` with `to` as a bare JID;
        // bob/desk's connection delivers via bare-match.
        let bob_desk = full("bob@example.com/desk");
        let m = msg_from_to(Some("alice@example.com/web"), Some("bob@example.com"));
        assert_eq!(Locality::derive(&bob_desk, &m), Locality::Recipient);
    }

    #[test]
    fn locality_derive_recipient_only_for_addressed_resource_when_to_is_full() {
        // alice -> bob/desk: only bob/desk is the recipient; bob/web
        // and bob/laptop are NOT.
        let bob_desk = full("bob@example.com/desk");
        let bob_web = full("bob@example.com/web");
        let m = msg_from_to(Some("alice@example.com/web"), Some("bob@example.com/desk"));
        assert_eq!(Locality::derive(&bob_desk, &m), Locality::Recipient);
        assert_eq!(Locality::derive(&bob_web, &m), Locality::Neither);
    }

    #[test]
    fn locality_derive_sender_requires_full_jid_match_on_from() {
        // alice/phone -> bob: on alice/web's connection, this is NOT
        // the sender (different resource) — it's Neither.
        let alice_web = full("alice@example.com/web");
        let m = msg_from_to(Some("alice@example.com/phone"), Some("bob@example.com"));
        assert_eq!(Locality::derive(&alice_web, &m), Locality::Neither);
    }

    #[test]
    fn locality_derive_neither_when_third_party() {
        let local = full("eve@example.com/web");
        let m = msg_from_to(Some("alice@example.com/web"), Some("bob@example.com"));
        assert_eq!(Locality::derive(&local, &m), Locality::Neither);
    }

    #[test]
    fn locality_helpers_classify_combined_role() {
        assert!(Locality::Sender.is_sender());
        assert!(Locality::Both.is_sender());
        assert!(Locality::Both.is_recipient());
        assert!(!Locality::Recipient.is_sender());
        assert!(!Locality::Sender.is_recipient());
        assert!(!Locality::Neither.is_sender());
        assert!(!Locality::Neither.is_recipient());
    }
}
