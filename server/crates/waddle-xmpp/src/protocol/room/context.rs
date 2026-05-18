//! Room-locality context for the MUC handler chain.
//!
//! Mirrors [`super::super::message_context::MessageContext`] but for
//! groupchat dispatch: instead of describing a single connection's
//! session-bounded state, [`RoomContext`] describes the **room's** state
//! at the moment a sender-pass groupchat message enters the chain. Per
//! Q5 of the #229 design, the snapshot is *frozen at dispatch start* —
//! every handler in the chain sees the same occupant set, sender nick,
//! and managed-room flags throughout one dispatch.
//!
//! The interpreter's [`super::super::event::OutboundEvent::DispatchToRoom`]
//! arm constructs a [`RoomContext`] by querying the per-room actor for its
//! occupancy + nickname + role data and threading the connection's
//! authenticated session (for the managed-room owner check), then runs
//! the chain against the resulting snapshot.

use super::super::id_gen::IdGenerator;
use crate::types::{Affiliation, Role};
use crate::xep::xep0421::OccupantIdSecret;
use jid::{BareJid, FullJid};

/// Snapshot of one occupant of the room, frozen at dispatch start.
///
/// Carries the typed identity needed by the chain handlers:
/// the occupant's bare JID for XEP-0421 occupant-id derivation,
/// the occupant's full JID(s) for per-resource fan-out, and the
/// occupant's nickname for XEP-0045 `from='room/nick'` rewriting.
#[derive(Debug, Clone)]
pub struct OccupantSnapshot {
    /// The full JID this snapshot represents (one entry per session if
    /// an occupant has multiple resources joined under the same nick).
    pub full_jid: FullJid,
    /// The MUC nickname this occupant is joined under.
    pub nick: String,
    /// XEP-0045 affiliation (owner/admin/member/none/outcast).
    pub affiliation: Affiliation,
    /// XEP-0045 role (moderator/participant/visitor/none).
    pub role: Role,
}

impl OccupantSnapshot {
    /// The bare JID of this occupant (the user's account, stripped of
    /// resource). Used for XEP-0421 occupant-id derivation — same user
    /// across multiple resources gets the same occupant id.
    pub fn bare_jid(&self) -> BareJid {
        self.full_jid.to_bare()
    }
}

/// Read-only context handed to every [`super::traits::RoomHandler`] in a
/// single MUC dispatch.
///
/// The struct is borrow-only — its lifetime is the dispatch call. The
/// interpreter owns the underlying values and constructs a fresh
/// `RoomContext<'_>` per dispatch.
pub struct RoomContext<'a> {
    /// The room's bare JID (e.g. `team@conference.example.com`). Used
    /// for the XEP-0359 `<stanza-id by='room'/>` stamp, the
    /// `from='room/nick'` rewrite, and the XEP-0421 occupant-id
    /// derivation.
    pub room: &'a BareJid,
    /// The full JID of the sending occupant — the connection that
    /// emitted the groupchat message. Used to resolve the sender's
    /// occupancy snapshot for the XEP-0045 §7.4 occupancy check and
    /// for the `from='room/nick'` rewrite.
    pub sender_full: &'a FullJid,
    /// The room's occupancy at dispatch start (Q5 frozen-snapshot
    /// semantic). Indexed iteration only — handlers don't need a
    /// hash lookup at this scale (typical room ≤ 200 occupants).
    pub occupants: &'a [OccupantSnapshot],
    /// Durable bare-JID recipients derived from persistent room
    /// affiliations/membership at dispatch start. The inbox projection
    /// uses this as the authoritative recipient set for durable
    /// per-user rows; current occupancy is only a live-delivery and
    /// active-mention filter.
    pub durable_recipient_bare_jids: &'a [BareJid],
    /// True when the room is the managed `announcements` room and the
    /// sender is NOT a server owner.
    ///
    /// Pre-derived by the interpreter so the
    /// [`super::occupancy_validation::OccupancyValidationHandler`]
    /// can short-circuit with a typed `<forbidden/>` reply without
    /// awaiting an async permission check.
    pub managed_room_forbidden: bool,
    /// True when the room is configured as moderated (XEP-0045 §5.1.4
    /// `muc#roomconfig_moderatedroom`). Combined with the sender's
    /// snapshot role, this lets
    /// [`super::occupancy_validation::OccupancyValidationHandler`]
    /// enforce XEP-0045 §7.5 (visitors may not send messages in
    /// moderated rooms) — the chain emits a typed
    /// `<error type='auth'><forbidden/></error>` reply. Closes a
    /// regression introduced by PR18: the legacy
    /// `RoomActor::BuildGroupchatBroadcast` path enforced this; the
    /// chain didn't, until this field landed (Copilot review on
    /// PR #279).
    pub room_moderated: bool,
    /// True when the room is configured as members-only. This is
    /// carried into projection events so the server-side notification
    /// gate can apply XEP-0492's private/public group defaults from the
    /// same frozen room snapshot.
    pub room_members_only: bool,
    /// XEP-0513 mention permission snapshot. The inbox projection carries
    /// this into server-side push classification so retries do not re-read
    /// mutable room config after dispatch.
    pub mention_permissions: crate::xep::xep0513::MentionPermissions,
    /// #415: per-room pin permission policy (`urn:waddle:roomconfig:pinpermission`).
    /// Frozen at dispatch start from `RoomConfig.pin_permission`; the
    /// `MucPinHandler` reads this to decide whether non-admin members
    /// may pin/unpin.
    pub pin_permission: crate::muc::PinPermission,
    /// Source of fresh, opaque XEP-0359 stanza-id values for the
    /// canonical `<stanza-id by='room'/>` stamp the
    /// [`super::canonicalize::MucCanonicalizeHandler`] applies.
    pub id_gen: &'a dyn IdGenerator,
    /// Server-side secret used to derive the XEP-0421 stable occupant-id
    /// (per-(room, bare-jid) HMAC). Borrowed from the deployment config
    /// so tests can substitute a fixture without mutating global state.
    pub occupant_id_secret: &'a OccupantIdSecret,
    /// Sender's per-room nickname generation (XEP-0308 §3 correction
    /// window). Provided here so the
    /// [`super::archive::MucArchiveHandler`] can include it directly
    /// in [`super::super::event::OutboundEvent::ArchiveGroupchat`]
    /// without the interpreter having to issue a second
    /// `RoomActor::GetRoomSnapshot` query at archive time (Copilot
    /// review on PR #279).
    pub sender_nickname_generation: u64,
    /// Whether the sender should receive the sender-owned inbox
    /// projection. Real occupant sends do; synthetic server-authored
    /// sends can disable this while still using the sender snapshot
    /// for MUC canonicalization.
    pub project_sender_inbox: bool,
    /// Single dispatch timestamp (Unix epoch seconds) shared across
    /// every groupchat inbox
    /// [`super::super::event::OutboundEvent::ProjectGroupchatInbox`]
    /// projection emitted by [`super::inbox::MucInboxHandler`].
    /// Captured by the interpreter at dispatch start so projections
    /// don't drift across a second-boundary (Copilot review on
    /// PR #279).
    pub dispatch_timestamp: i64,
}

impl<'a> RoomContext<'a> {
    /// Look up the sender's occupancy snapshot in the frozen list.
    ///
    /// Returns `None` when the sender is not currently joined to the
    /// room — the
    /// [`super::occupancy_validation::OccupancyValidationHandler`] uses
    /// this to enforce the XEP-0045 §7.4 sender-occupancy gate.
    pub fn sender_snapshot(&self) -> Option<&'a OccupantSnapshot> {
        self.occupants
            .iter()
            .find(|o| &o.full_jid == self.sender_full)
    }

    /// Occupants that should receive live delivery.
    ///
    /// Normal user-originated dispatches use the full occupant set.
    /// Server-authored synthetic dispatches may include a sender-only
    /// snapshot so canonicalization can produce `room/nick` without
    /// treating that synthetic sender as a live delivery recipient.
    pub fn recipient_occupants(&'a self) -> Box<dyn Iterator<Item = &'a OccupantSnapshot> + 'a> {
        if self.project_sender_inbox {
            Box::new(self.occupants.iter())
        } else {
            Box::new(
                self.occupants
                    .iter()
                    .filter(|occupant| &occupant.full_jid != self.sender_full),
            )
        }
    }
}
