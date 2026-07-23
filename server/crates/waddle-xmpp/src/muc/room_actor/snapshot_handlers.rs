use jid::{BareJid, FullJid};
use kameo::message::Context;
use xmpp_parsers::message::Message;

use super::{RoomActor, RoomActorError};
use crate::muc::{OutboundMucMessage, RoomClaimFenceContext, RoomConfig};
use crate::types::{Affiliation, Role};

#[derive(Debug, Clone)]
pub struct GroupchatBroadcastResult {
    pub sender_nick: String,
    pub messages: Vec<OutboundMucMessage>,
    pub occupant_bare_jids: Vec<String>,
    /// Per-XEP-0308 §3 occupancy generation for the sender's nickname
    /// at the moment this broadcast was built. Stored alongside the
    /// archive row so that later corrections can verify the sender is
    /// still in the same occupancy session (i.e. the nickname has not
    /// been left and re-claimed in the meantime).
    pub sender_nickname_generation: u64,
}

/// Frozen-at-dispatch-start snapshot of the room state needed by the
/// sans-I/O room handler chain (#229 PR18).
///
/// Returned by [`GetRoomSnapshot`] in a single round-trip so the
/// [`crate::protocol::event::OutboundEvent::DispatchToRoom`] interpreter
/// arm can build a [`crate::protocol::room::RoomContext`] without N+1
/// actor queries. Mirrors the data the legacy
/// `BuildGroupchatBroadcast` flow accessed inline (occupants list,
/// sender's role/affiliation/nick, sender's nickname-generation,
/// room config).
#[derive(Debug, Clone)]
pub struct RoomChainSnapshot {
    /// Exact durable-ownership proof retained by this actor incarnation.
    /// Dispatch and archive paths must use this immutable context rather
    /// than looking up the room JID in the registry's mutable successor
    /// cache, which could transplant a later actor's authority onto this
    /// frozen snapshot.
    pub claim_fence: Option<RoomClaimFenceContext>,
    /// One entry per active occupant session — same `nick` may appear
    /// multiple times when an occupant has joined under multiple
    /// resources.
    pub occupants: Vec<RoomChainOccupant>,
    /// The sender's nickname when present in the room, or `None` when
    /// the sender is not currently joined (XEP-0045 §7.4 trigger:
    /// `OccupancyValidationHandler` will halt with `<not-acceptable/>`).
    pub sender_nick: Option<String>,
    /// Sender's role at dispatch start (visitor/participant/moderator),
    /// or `None` when the sender is not joined.
    pub sender_role: Option<Role>,
    /// Sender's affiliation at dispatch start, or `None` when not joined.
    pub sender_affiliation: Option<Affiliation>,
    /// Per-XEP-0308 §3 occupancy generation for the sender's nickname.
    /// `None` when the sender is not joined or the nickname has never
    /// been observed.
    pub sender_nickname_generation: Option<u64>,
    /// Snapshot of the room config (members_only, moderated, etc.).
    pub config: RoomConfig,
    /// Durable affiliation-derived recipients at dispatch start.
    pub durable_recipient_bare_jids: Vec<BareJid>,
    /// Admission revision at dispatch start for stale-join rejection.
    pub admission_revision: u64,
}

/// One occupant session in a [`RoomChainSnapshot`].
///
/// Distinct from [`super::OccupantInfo`] / [`crate::muc::Occupant`] because the
/// chain needs *one entry per active session* (an occupant joined from multiple
/// resources gets one snapshot per resource), whereas `OccupantInfo` collapses
/// sessions to one row per nickname.
#[derive(Debug, Clone)]
pub struct RoomChainOccupant {
    pub full_jid: FullJid,
    pub nick: String,
    pub role: Role,
    pub affiliation: Affiliation,
}

/// Read the room state into a [`RoomChainSnapshot`] tailored for the
/// sans-I/O room handler chain (#229 PR18). Single round-trip so the
/// interpreter can build a [`crate::protocol::room::RoomContext`] in
/// one actor call.
pub struct GetRoomSnapshot {
    /// Full JID of the connection driving this dispatch — used to
    /// resolve the sender's nick, role, affiliation, and
    /// nickname-generation in one pass.
    pub sender_jid: FullJid,
}

impl kameo::message::Message<GetRoomSnapshot> for RoomActor {
    type Reply = Result<RoomChainSnapshot, RoomActorError>;

    async fn handle(
        &mut self,
        msg: GetRoomSnapshot,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        // This is the dispatch-chain admission snapshot, not a diagnostic
        // read: returning it authorizes archive writes and room fan-out.
        self.gate_serving_activity()?;
        let occupants: Vec<RoomChainOccupant> = self
            .room
            .occupants
            .values()
            .flat_map(|occupant| {
                let nick = occupant.nick.clone();
                let role = occupant.role;
                let affiliation = occupant.affiliation;
                self.room
                    .get_occupant_sessions(&occupant.nick)
                    .into_iter()
                    .map(move |full_jid| RoomChainOccupant {
                        full_jid,
                        nick: nick.clone(),
                        role,
                        affiliation,
                    })
            })
            .collect();

        let sender_occupant = self.room.find_occupant_by_real_jid(&msg.sender_jid);
        let sender_nick = sender_occupant.map(|o| o.nick.clone());
        let sender_role = sender_occupant.map(|o| o.role);
        let sender_affiliation = sender_occupant.map(|o| o.affiliation);
        let sender_nickname_generation = sender_nick
            .as_deref()
            .and_then(|nick| self.room.current_nickname_generation(nick));
        // Durable recipients = session-observed Member+ affiliations ∪
        // the spawn-time hydrated durable membership (#1135). The
        // hydrated set covers offline members who never joined this
        // actor incarnation; the affiliation-list side keeps runtime
        // grants visible immediately. A runtime demotion to Outcast
        // wins over the hydrated mirror so banned members drop out of
        // inbox fan-out without waiting for a respawn.
        let mut durable_recipient_bare_jids = self
            .room
            .get_all_affiliations()
            .into_iter()
            .filter(|entry| entry.affiliation >= Affiliation::Member)
            .map(|entry| entry.jid)
            .collect::<Vec<_>>();
        durable_recipient_bare_jids.extend(
            self.durable_member_recipients
                .iter()
                .filter(|jid| self.room.get_affiliation(jid) != Affiliation::Outcast)
                .cloned(),
        );
        durable_recipient_bare_jids.sort();
        durable_recipient_bare_jids.dedup();

        Ok(RoomChainSnapshot {
            claim_fence: self.durable_claim_fence.clone(),
            occupants,
            sender_nick,
            sender_role,
            sender_affiliation,
            sender_nickname_generation,
            config: self.room.config.clone(),
            durable_recipient_bare_jids,
            admission_revision: self.admission_revision,
        })
    }
}

pub struct BuildGroupchatBroadcast {
    pub sender_jid: FullJid,
    pub message: Message,
}

impl kameo::message::Message<BuildGroupchatBroadcast> for RoomActor {
    type Reply = Result<GroupchatBroadcastResult, RoomActorError>;

    async fn handle(
        &mut self,
        msg: BuildGroupchatBroadcast,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.gate_serving_activity()?;
        let sender_occupant = self
            .room
            .find_occupant_by_real_jid(&msg.sender_jid)
            .ok_or_else(|| RoomActorError::SenderNotOccupant(msg.sender_jid.clone()))?;
        let sender_nick = sender_occupant.nick.clone();

        if self.room.config.moderated && sender_occupant.role == Role::Visitor {
            return Err(RoomActorError::VisitorMayNotSpeak(msg.sender_jid.clone()));
        }

        let messages = self
            .room
            .broadcast_message(&sender_nick, &msg.message)
            .map_err(|error| RoomActorError::BroadcastFailed(error.to_string()))?;

        let sender_jid_for_filter = msg.sender_jid;
        let occupant_bare_jids: Vec<String> = self
            .room
            .occupants
            .values()
            .flat_map(|o| {
                self.room
                    .get_occupant_sessions(&o.nick)
                    .into_iter()
                    .filter(|jid| *jid != sender_jid_for_filter)
            })
            .map(|jid| jid.to_bare().to_string())
            .collect();

        let sender_nickname_generation = self
            .room
            .current_nickname_generation(&sender_nick)
            .unwrap_or(0);

        Ok(GroupchatBroadcastResult {
            sender_nick,
            messages,
            occupant_bare_jids,
            sender_nickname_generation,
        })
    }
}

/// Query the current per-nickname occupancy generation. Returns 0 when
/// the nickname has never been observed by this actor (e.g. after
/// server restart, which closes the correction window for prior
/// archive entries per XEP-0308 §3).
pub struct GetNicknameGeneration {
    pub nick: String,
}

impl kameo::message::Message<GetNicknameGeneration> for RoomActor {
    type Reply = Result<u64, RoomActorError>;

    async fn handle(
        &mut self,
        msg: GetNicknameGeneration,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.gate_serving_activity()?;
        Ok(self
            .room
            .current_nickname_generation(&msg.nick)
            .unwrap_or(0))
    }
}
