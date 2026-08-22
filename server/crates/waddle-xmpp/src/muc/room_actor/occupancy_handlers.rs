use std::convert::Infallible;

use jid::{BareJid, FullJid};
use kameo::message::Context;

use super::{
    affiliation_overflows_full_room, JoinDenialReason, JoinExistingOccupant, JoinOutcome,
    LeaveOutcome, OccupancyWatermark, PresenceUpdateOutcome, ProjectionGate, RoomActor,
    RoomActorError,
};
use crate::muc::{
    durable::{
        ChannelId, OccupancyLeaveCause, RoomDurableMutation, RoomProjection, RoomProjectionKind,
        WaddleId,
    },
    RoomConfig,
};
use crate::types::Affiliation;

/// The affiliation a join request carries into the room, typed by
/// where it came from (#1110/#1134).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinAffiliationGrant {
    /// The joiner brings no affiliation of their own; whatever the
    /// room's affiliation list already stores applies.
    Unaffiliated,
    /// Derived by the join-path authz resolver from the managed
    /// channel / space graph. Reconstructible on the next join, so it
    /// is stored with [`AffiliationProvenance::ResolverDerived`] and
    /// never blocks room dormancy.
    ///
    /// [`AffiliationProvenance::ResolverDerived`]: crate::muc::affiliation::AffiliationProvenance::ResolverDerived
    Resolver(Affiliation),
    /// XEP-0045 §10.1.1: this join created the room, so the joiner is
    /// the room creator and receives Owner. Stored as an explicit
    /// grant — it is not reconstructible from any resolver.
    CreatorOwner,
}

pub struct JoinWithAffiliation {
    pub sender_jid: FullJid,
    pub nick: String,
    pub affiliation_grant: JoinAffiliationGrant,
    pub local_domain: String,
    pub admission_revision: u64,
}

impl kameo::message::Message<JoinWithAffiliation> for RoomActor {
    type Reply = Result<JoinOutcome, RoomActorError>;

    async fn handle(
        &mut self,
        msg: JoinWithAffiliation,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let _handler_timer = crate::metrics::MucOccupancyHandlerTimer::start("join");
        // #1108: a caller holding a stale ActorRef must retry through the
        // registry. For an inactivity seal this also re-checks ownership so
        // the seal can strengthen to OwnershipLost before the reaper runs.
        self.reject_sealed_join().await?;
        // ADR-0017 Phase 3 Slice 7 FIX 4 (council-adjudicated): refuse to
        // admit a join while this incarnation's durable restore is
        // unresolved (a genuine backend failure, not a legitimate brand
        // -new room) — see `RoomActor::ensure_restored_before_join`.
        self.ensure_restored_before_join().await?;
        self.gate_join_ownership().await?;
        if self.invite_rollback_pending(&msg.sender_jid.to_bare()) {
            return Err(RoomActorError::InviteRollbackPending);
        }
        let joining_bare_jid = msg.sender_jid.to_bare();
        if !self.admission_revision_is_current(&joining_bare_jid, msg.admission_revision) {
            return Err(RoomActorError::StaleAdmissionRevision);
        }

        let mut resolver_join_advanced_admission_revision = false;
        match msg.affiliation_grant {
            JoinAffiliationGrant::Unaffiliated => {}
            JoinAffiliationGrant::Resolver(affiliation) => {
                // Resolver-derived affiliations are reconstructible from the
                // channel/space graph, so they intentionally remain memory-only.
                // Applied unconditionally, including `Affiliation::None`:
                // a resolver revocation must clear any stale
                // resolver-derived Member/Admin entry so the revoked
                // user no longer passes members-only admission below.
                // `set_with_provenance` keeps this safe — it refuses
                // resolver writes over explicit grants (bans survive)
                // and removes the map entry on a None write.
                //
                // An applied change bumps the admission revision immediately
                // (like `ChangeAffiliation`). If the resolver value is already
                // identical, a successful admission bumps it below: a delayed
                // `SyncResolverAffiliation` from an earlier rejected
                // join of this user carries the pre-change revision and
                // must be refused, or it would clear the affiliation
                // this join just re-derived.
                if self
                    .room
                    .update_affiliation_from_resolver(msg.sender_jid.to_bare(), affiliation)
                    .is_some()
                {
                    self.invalidate_invite_grant(&msg.sender_jid.to_bare());
                    self.advance_member_admission_revision(&joining_bare_jid);
                    resolver_join_advanced_admission_revision = true;
                }
            }
            JoinAffiliationGrant::CreatorOwner => {
                // #1134 defense-in-depth on top of the registry's
                // created-bit: XEP-0045 §10.1.1 gives Owner to the
                // creator only, so the grant applies only while no
                // owner exists. If two racing first-joins both claim
                // creatorship, the actor's serialized mailbox makes
                // exactly one of them the owner.
                if !self.room.has_owner() {
                    self.commit_durable(
                        RoomDurableMutation::Affiliation(
                            crate::muc::durable::AffiliationEntry::new(
                                msg.sender_jid.to_bare(),
                                Some(Affiliation::Owner),
                            ),
                        ),
                        crate::muc::RoomMutationEffects::none(),
                    )
                    .await
                    .map_err(|error| match error {
                        super::DurablePersistError::NotOwner
                        | super::DurablePersistError::CommitOutcomeUnknown => {
                            RoomActorError::RoomSealed
                        }
                        super::DurablePersistError::OwnershipUnavailable
                        | super::DurablePersistError::PersistFailed => {
                            RoomActorError::OwnershipUnavailable
                        }
                    })?;
                    self.room
                        .set_affiliation(msg.sender_jid.to_bare(), Affiliation::Owner);
                    self.invalidate_invite_grant(&msg.sender_jid.to_bare());
                }
            }
        }

        if !self.room.can_user_join(&msg.sender_jid.to_bare()) {
            // XEP-0045 §7.2.8: a ban (outcast) is <forbidden/> even in a
            // members-only room — never <registration-required/>, which
            // would invite the banned user to apply for membership
            // (#1265 item 1).
            let reason =
                if self.room.get_affiliation(&msg.sender_jid.to_bare()) == Affiliation::Outcast {
                    JoinDenialReason::Banned
                } else {
                    JoinDenialReason::MembersOnly
                };
            return Err(RoomActorError::JoinForbidden { reason });
        }
        // #1107: the same FULL JID must never hold two occupancies.
        // Sibling sessions of the same BARE jid legitimately share one
        // nick (multi-session join below), but this exact session
        // joining under a second nick would create a ghost occupancy
        // that leave-cleanup misses. Nicknames are locked to identity,
        // so refuse per XEP-0045 §7.6 (`<not-acceptable/>`).
        if let Some(current_nick) = self.room.find_nick_by_real_jid(&msg.sender_jid) {
            if current_nick != msg.nick {
                return Err(RoomActorError::OccupantAlreadyJoinedUnderDifferentNick {
                    current_nick: current_nick.to_owned(),
                    requested_nick: msg.nick,
                });
            }
        }
        let mut is_same_bare_multi_session_join = false;
        let is_existing_session_rejoin = self
            .room
            .get_occupant_sessions(&msg.nick)
            .iter()
            .any(|session| session == &msg.sender_jid);
        if let Some(existing) = self.room.get_occupant(&msg.nick) {
            if existing.real_jid != msg.sender_jid {
                if existing.real_jid.to_bare() == msg.sender_jid.to_bare() {
                    is_same_bare_multi_session_join = true;
                } else {
                    return Err(RoomActorError::NickAlreadyInUse(msg.nick));
                }
            }
        }
        let joining_affiliation = self.room.get_affiliation(&msg.sender_jid.to_bare());
        if self.room.is_full()
            && !is_existing_session_rejoin
            && !is_same_bare_multi_session_join
            && !affiliation_overflows_full_room(joining_affiliation)
        {
            return Err(RoomActorError::RoomFull);
        }

        let existing_occupants: Vec<JoinExistingOccupant> = self
            .room
            .occupants
            .values()
            .flat_map(|o| {
                self.room
                    .get_occupant_sessions(&o.nick)
                    .into_iter()
                    .map(|jid| {
                        let muji = self.room.muji_for_session(&o.nick, &jid);
                        let in_call = self.room.in_call_state_for_session(&o.nick, &jid);
                        JoinExistingOccupant {
                            jid,
                            nick: o.nick.clone(),
                            affiliation: o.affiliation,
                            role: o.role,
                            muji,
                            in_call,
                        }
                    })
            })
            .collect();

        // Resolver cache updates deliberately precede this commit so a
        // revocation is visible to the admission checks even during outage.
        let Some(durable_nick) = crate::muc::durable::MucOccupantNick::new(msg.nick.clone()) else {
            return Err(RoomActorError::NickAlreadyInUse(msg.nick));
        };
        let gate = self
            .commit_projection(RoomProjection::OccupancyJoin {
                occupant: msg.sender_jid.clone(),
                nick: durable_nick,
            })
            .await
            .map_err(Self::map_projection_commit_error)?;
        self.project(gate, RoomProjectionKind::OccupancyJoin, |actor| {
            let occupant_count_before = actor.room.occupant_count();
            let joined_at =
                OccupancyWatermark::from_revision(actor.occupancy_revision.saturating_add(1));
            let joined_jid = msg.sender_jid.clone();
            let new_occupant = actor
                .room
                .add_occupant_with_affiliation(
                    msg.sender_jid,
                    msg.nick.clone(),
                    Some(msg.local_domain.as_str()),
                    joined_at,
                )
                .clone();
            actor.note_session_joined(&joined_jid);
            let new_occupant_affiliation = new_occupant.affiliation;
            let new_occupant_role = new_occupant.role;
            let occupant_count = actor.room.occupant_count();
            crate::metrics::record_muc_presence("join");
            crate::metrics::adjust_muc_occupant_total(
                occupant_count as i64 - occupant_count_before as i64,
            );
            let room_jid = actor.room.room_jid.clone();
            if matches!(msg.affiliation_grant, JoinAffiliationGrant::Resolver(_))
                && !resolver_join_advanced_admission_revision
            {
                actor.advance_member_admission_revision(&joining_bare_jid);
            }
            actor.occupancy_revision = actor.occupancy_revision.saturating_add(1);
            JoinOutcome {
                existing_occupants,
                new_occupant_affiliation,
                new_occupant_role,
                occupant_count,
                room_jid,
                is_same_bare_multi_session_join,
                is_existing_session_rejoin,
                subject_state: actor.room.subject.clone(),
            }
        })
        .map_err(Self::map_projection_refusal)
    }
}

pub struct LeaveByRealJid {
    pub sender_jid: FullJid,
    pub cause: OccupancyLeaveCause,
    pub session: LeaveSessionSelector,
    /// Idempotency key of this logical departure. A caller whose reply timed
    /// out after the message was enqueued retries with the SAME id: if the
    /// actor already completed the departure it replays the retained
    /// [`LeaveOutcome`] (exactly once) instead of answering `NotOccupant`, so
    /// the departure's effects are never lost.
    pub attempt: LeaveAttemptId,
    /// Whether this ask is a retained retry of an earlier departure. Only a
    /// retry may consume the full JID's unacknowledged receipt by JID
    /// fallback (its own attempt may have been coalesced away); a fresh ask
    /// for a gone session is simply `NotOccupant`, so a receipt whose
    /// acknowledgement is still in flight can never be replayed by an
    /// unrelated later leave.
    pub origin: LeaveOrigin,
}

/// Provenance of a `LeaveByRealJid` ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaveOrigin {
    /// A first attempt (explicit leave, disconnect cleanup, admin removal).
    Fresh,
    /// A retry of a retained departure responsibility (janitor).
    RetainedRetry,
}

/// Idempotency key for one logical `LeaveByRealJid` departure, minted from
/// the process-wide occupancy order so the actor can tell whether a session
/// existed when the attempt was first issued: a session that joined AFTER
/// the attempt was minted is never the attempt's target (`Superseded`),
/// which makes late retries safe against any number of rejoins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LeaveAttemptId(OccupancyOrder);

impl LeaveAttemptId {
    pub fn generate() -> Self {
        Self(next_occupancy_order())
    }

    /// Position of this attempt in the process-wide occupancy order.
    pub fn order(self) -> OccupancyOrder {
        self.0
    }
}

/// A position in the process-wide monotonic occupancy order shared by session
/// joins and leave attempts. Unlike the room's occupancy watermark it advances
/// on every join in every room (memory-only rooms included), so comparing two
/// positions always tells which event came first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OccupancyOrder(u64);

impl OccupancyOrder {
    #[cfg(test)]
    pub const fn from_raw(order: u64) -> Self {
        Self(order)
    }
}

static OCCUPANCY_ORDER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Mint the next position in the process-wide occupancy order.
pub fn next_occupancy_order() -> OccupancyOrder {
    OccupancyOrder(OCCUPANCY_ORDER.fetch_add(1, std::sync::atomic::Ordering::SeqCst))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaveSessionSelector {
    Any,
    JoinedAtOrBefore(OccupancyWatermark),
}

#[derive(Debug, kameo::Reply)]
pub enum LeaveDisposition {
    Left(Box<LeaveOutcome>),
    NotOccupant,
    Superseded,
    Deferred {
        watermark: OccupancyWatermark,
    },
    /// The departure changed local state but must not emit leave effects.
    /// Store-less room with a destroy/dormancy in flight: the departure was
    /// recorded but nothing fans out. Carries the leaver's affiliation so the
    /// XEP-0045 §7.14 self-presence echo can report it.
    Suppressed {
        nick: crate::muc::MucOccupantNick,
        affiliation: crate::types::Affiliation,
        /// The attempt to acknowledge (see [`super::LeaveOutcome::acknowledge`]).
        attempt: LeaveAttemptId,
    },
}

impl kameo::message::Message<LeaveByRealJid> for RoomActor {
    type Reply = Result<LeaveDisposition, RoomActorError>;

    async fn handle(
        &mut self,
        msg: LeaveByRealJid,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let _handler_timer = crate::metrics::MucOccupancyHandlerTimer::start("leave");
        if self.departure_attempt_is_superseded(&msg.sender_jid, msg.attempt) {
            return Ok(LeaveDisposition::Superseded);
        }
        match self.replay_departure_receipt(msg.attempt) {
            // Replay only while the departure is still the latest truth: a
            // newer generation of this full JID (re-entered, even if it left
            // or was removed since) or a re-taken nick makes the retained
            // outcome stale; replaying it would evict a live occupant from
            // every client's roster or repeat a removal already announced.
            Some(super::RetainedDeparture::Stale) => return Ok(LeaveDisposition::Superseded),
            Some(super::RetainedDeparture::Current(receipt)) => {
                if self.room.session_watermark(&msg.sender_jid).is_some()
                    || self.nick_retaken(&receipt)
                {
                    return Ok(LeaveDisposition::Superseded);
                }
                return Ok(receipt_disposition(receipt));
            }
            None => {}
        }
        // Resolve occupancy before consulting the seal: a non-occupant's leave
        // is a pure memory read with nothing to project, and every disconnect
        // asks every local room — a sealed room must not mint retained
        // departures for sessions it never held.
        // #1107: collect EVERY nick this full JID occupies. Post-#1107
        // the join path refuses a second nick for the same full JID, so
        // this is normally a single entry — but pre-existing ghost
        // states (or direct state manipulation) must still converge on
        // disconnect, or the ghost lives forever: wrong occupant count,
        // fan-out to a dead session, room never dormant. Sorted for
        // deterministic primary-nick selection.
        let mut nicks: Vec<String> = self
            .room
            .occupants
            .values()
            .filter(|occupant| {
                self.room
                    .get_occupant_sessions(&occupant.nick)
                    .iter()
                    .any(|session| session == &msg.sender_jid)
            })
            .map(|occupant| occupant.nick.clone())
            .collect();
        nicks.sort();
        let Some(nick) = nicks.first().cloned() else {
            // The session is gone. If a departure of this full JID completed
            // but its reply was lost (unacknowledged receipt) — possibly under
            // a different attempt id that a coalesced retry no longer carries —
            // replay it now: the departure's effects are owed to someone and
            // the receipt is consumed exactly once.
            if msg.origin == LeaveOrigin::RetainedRetry {
                match self.replay_departure_receipt_for_jid(&msg.sender_jid, msg.cause) {
                    Some(super::RetainedDeparture::Stale) => {
                        return Ok(LeaveDisposition::Superseded);
                    }
                    Some(super::RetainedDeparture::Current(receipt)) => {
                        if self.nick_retaken(&receipt) {
                            return Ok(LeaveDisposition::Superseded);
                        }
                        return Ok(receipt_disposition(receipt));
                    }
                    None => {}
                }
            }
            return Ok(LeaveDisposition::NotOccupant);
        };
        let Some(occupant) = self.room.get_occupant(&nick) else {
            return Ok(LeaveDisposition::NotOccupant);
        };
        // A session that joined after this attempt was minted is not the
        // attempt's target: a late retry (lost reply, then rejoin) must never
        // evict the live successor session, whatever selector it carries.
        if self
            .room
            .session_order(&msg.sender_jid)
            .is_some_and(|joined_order| joined_order > msg.attempt.order())
        {
            return Ok(LeaveDisposition::Superseded);
        }
        let departing_generation = self
            .room
            .session_order(&msg.sender_jid)
            .unwrap_or_else(next_occupancy_order);
        let departing_nick_generation = self.room.current_nickname_generation(&nick);
        let current_watermark = OccupancyWatermark::from_revision(self.occupancy_revision);
        let suppress_effects = match self.seal_state {
            super::RoomSealState::Inactive | super::RoomSealState::Destroying { .. }
                if self.durable_store.is_none() =>
            {
                true
            }
            super::RoomSealState::Inactive | super::RoomSealState::Destroying { .. } => {
                return Ok(LeaveDisposition::Deferred {
                    watermark: current_watermark,
                });
            }
            super::RoomSealState::OwnershipLost => return Err(RoomActorError::RoomSealed),
            super::RoomSealState::Open => false,
        };
        if matches!(
            msg.session,
            LeaveSessionSelector::JoinedAtOrBefore(watermark)
                if self
                    .room
                    .session_watermark(&msg.sender_jid)
                    .is_some_and(|current| current > watermark)
        ) {
            return Ok(LeaveDisposition::Superseded);
        };
        let affiliation = occupant.affiliation;
        let role = occupant.role;
        let Ok(leaving_room_jid) = self.room.room_jid.clone().with_resource_str(&nick) else {
            tracing::warn!(room = %self.room.room_jid, %nick, "occupant state contains an invalid nickname");
            return Ok(LeaveDisposition::NotOccupant);
        };
        let remaining_occupants: Vec<FullJid> = self
            .room
            .occupants
            .values()
            .flat_map(|o| self.room.get_occupant_sessions(&o.nick))
            .filter(|jid| *jid != msg.sender_jid)
            .collect();
        let cleared_muji_state = self
            .room
            .muji_state
            .get(&nick)
            .is_some_and(|entries| entries.contains_key(&msg.sender_jid));
        let Some(durable_nick) = crate::muc::durable::MucOccupantNick::new(nick.clone()) else {
            tracing::warn!(room = %self.room.room_jid, %nick, "occupant state contains a non-durable nickname");
            return Ok(LeaveDisposition::NotOccupant);
        };
        let gate = if suppress_effects {
            ProjectionGate::Unfenced
        } else {
            match self
                .commit_projection(RoomProjection::OccupancyLeave {
                    occupant: msg.sender_jid.clone(),
                    nick: durable_nick.clone(),
                    cause: msg.cause,
                })
                .await
            {
                Ok(gate) => gate,
                Err(
                    super::DurablePersistError::OwnershipUnavailable
                    | super::DurablePersistError::PersistFailed,
                ) => {
                    return Ok(LeaveDisposition::Deferred {
                        watermark: current_watermark,
                    });
                }
                Err(
                    super::DurablePersistError::NotOwner
                    | super::DurablePersistError::CommitOutcomeUnknown,
                ) => {
                    return Err(RoomActorError::RoomSealed);
                }
            }
        };
        let occupant_count_before = self.room.occupant_count();
        self.project(gate, RoomProjectionKind::OccupancyLeave, |actor| {
            let removed_last_session = actor
                .room
                .remove_occupant_session(&nick, &msg.sender_jid)
                .unwrap_or(false);
            for ghost_nick in nicks.iter().skip(1) {
                actor
                    .room
                    .remove_occupant_session(ghost_nick, &msg.sender_jid);
            }
            let remaining_muji = if removed_last_session {
                None
            } else {
                actor.room.muji_for_nick(&nick)
            };
            let remaining_muji_sessions = if removed_last_session {
                Vec::new()
            } else {
                actor.room.muji_sessions_for_nick(&nick)
            };
            let remaining_nick_real_jid = if removed_last_session {
                None
            } else {
                actor
                    .room
                    .get_occupant(&nick)
                    .map(|occupant| occupant.real_jid.clone())
            };
            let occupant_count = actor.room.occupant_count();
            crate::metrics::record_muc_presence("leave");
            crate::metrics::adjust_muc_occupant_total(
                occupant_count as i64 - occupant_count_before as i64,
            );
            LeaveDisposition::Left(Box::new(LeaveOutcome {
                acknowledge: msg.attempt,
                nick,
                affiliation,
                role,
                leaving_room_jid,
                remaining_occupants,
                removed_last_session,
                cleared_muji_state,
                remaining_muji,
                remaining_muji_sessions,
                remaining_nick_real_jid,
                occupant_count,
                is_persistent: actor.room.config.persistent,
                occupancy_revision: actor.occupancy_revision,
            }))
        })
        .map(|disposition| match disposition {
            LeaveDisposition::Left(outcome) if suppress_effects => {
                self.retain_departure_receipt(super::DepartureReceipt {
                    attempt: msg.attempt,
                    jid: msg.sender_jid.clone(),
                    cause: msg.cause,
                    generation: departing_generation,
                    nick_generation: departing_nick_generation,
                    outcome: super::DepartureReceiptOutcome::Suppressed {
                        nick: durable_nick.clone(),
                        affiliation: outcome.affiliation,
                    },
                });
                LeaveDisposition::Suppressed {
                    nick: durable_nick,
                    affiliation: outcome.affiliation,
                    attempt: msg.attempt,
                }
            }
            LeaveDisposition::Left(outcome) => {
                self.retain_departure_receipt(super::DepartureReceipt {
                    attempt: msg.attempt,
                    jid: msg.sender_jid.clone(),
                    cause: msg.cause,
                    generation: departing_generation,
                    nick_generation: departing_nick_generation,
                    outcome: super::DepartureReceiptOutcome::Left(outcome.clone()),
                });
                LeaveDisposition::Left(outcome)
            }
            other => other,
        })
        .map_err(Self::map_projection_refusal)
    }
}

pub struct PresenceUpdateData {
    pub sender_jid: FullJid,
}

impl kameo::message::Message<PresenceUpdateData> for RoomActor {
    type Reply = Result<Option<PresenceUpdateOutcome>, Infallible>;

    async fn handle(
        &mut self,
        msg: PresenceUpdateData,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if !self.effectful_work_is_permitted().await {
            return Ok(None);
        }
        let Some(sender_occupant) = self.room.find_occupant_by_real_jid(&msg.sender_jid) else {
            return Ok(None);
        };
        let sender_nick = sender_occupant.nick.clone();
        let sender_real_jid = sender_occupant.real_jid.clone();
        let sender_role = sender_occupant.role;
        let sender_affiliation = sender_occupant.affiliation;
        let room_jid = self.room.room_jid.clone();
        let recipients = self
            .room
            .occupants
            .values()
            .flat_map(|o| self.room.get_occupant_sessions(&o.nick))
            .collect();
        Ok(Some(PresenceUpdateOutcome {
            sender_nick,
            sender_real_jid,
            sender_role,
            sender_affiliation,
            room_jid,
            recipients,
        }))
    }
}

/// Upsert the `<muji xmlns='urn:xmpp:jingle:muji:0'/>` advertised
/// state (XEP-0272) for the calling session's nick. Returns a
/// presence-update outcome (occupant identity + room recipients) and
/// the post-update Muji state to embed in the broadcast.
///
/// XEP-0045 §5.1.3 / §7.1: the room is responsible for reflecting
/// in-room presence to every occupant. Sender authentication —
/// "the session is actually an occupant of this room" — happens
/// here via `find_occupant_by_real_jid`; if the sender isn't an
/// occupant, the actor returns `Ok(None)` and the caller falls
/// back to the regular join path.
pub struct UpsertMujiPresence {
    pub sender_jid: FullJid,
    pub muji: crate::xep::xep0272::Muji,
}

pub struct ClearMujiPresence {
    pub sender_jid: FullJid,
}

/// Read the full JIDs of occupant sessions currently advertising active
/// XEP-0272 Muji contents. This narrow query lets room-scoped convergence
/// enumerate actor-owned state without exposing the actor's complete room
/// snapshot or relying on a node-local SFU registry.
pub struct GetActiveMujiSessions;

#[derive(Debug, Clone)]
pub struct MujiPresenceUpdateOutcome {
    pub update: PresenceUpdateOutcome,
    /// Exact Muji payload to reflect to the sending session. This
    /// preserves the XEP-0272 preparing echo even when another
    /// same-nick resource already has active contents.
    pub sender_muji: Option<crate::xep::xep0272::Muji>,
    /// Aggregate Muji payload for the occupant nick. Other occupants
    /// receive this authoritative state so one resource's preparing
    /// update cannot hide a sibling resource's active call.
    pub active_muji: Option<crate::xep::xep0272::Muji>,
    /// Exact per-session Muji payloads still advertised for this nick
    /// after the update.
    pub session_mujis: Vec<(FullJid, crate::xep::xep0272::Muji)>,
    /// True when this update starts the room's active call state.
    pub active_call_started: bool,
}

impl kameo::message::Message<UpsertMujiPresence> for RoomActor {
    type Reply = Result<Option<MujiPresenceUpdateOutcome>, Infallible>;

    async fn handle(
        &mut self,
        msg: UpsertMujiPresence,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if !self.effectful_work_is_permitted().await {
            return Ok(None);
        }
        let Some(sender_occupant) = self.room.find_occupant_by_real_jid(&msg.sender_jid) else {
            return Ok(None);
        };
        let sender_nick = sender_occupant.nick.clone();
        let sender_real_jid = sender_occupant.real_jid.clone();
        let sender_role = sender_occupant.role;
        let sender_affiliation = sender_occupant.affiliation;
        let room_jid = self.room.room_jid.clone();
        let recipients = self
            .room
            .occupants
            .values()
            .flat_map(|o| self.room.get_occupant_sessions(&o.nick))
            .collect();
        // Bind the call advertisement to the specific session that
        // emitted it so a partial-session leave (one resource of a
        // multi-resource occupant) clears the chip even when the
        // user's other sessions remain in the room.
        let muji_state =
            self.room
                .upsert_muji_presence(&sender_nick, msg.sender_jid.clone(), msg.muji);
        Ok(Some(MujiPresenceUpdateOutcome {
            update: PresenceUpdateOutcome {
                sender_nick,
                sender_real_jid,
                sender_role,
                sender_affiliation,
                room_jid,
                recipients,
            },
            sender_muji: muji_state.sender_muji,
            active_muji: muji_state.room_muji,
            session_mujis: muji_state.session_mujis,
            active_call_started: muji_state.active_call_started,
        }))
    }
}

impl kameo::message::Message<ClearMujiPresence> for RoomActor {
    type Reply = Result<Option<MujiPresenceUpdateOutcome>, Infallible>;

    async fn handle(
        &mut self,
        msg: ClearMujiPresence,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if !self.effectful_work_is_permitted().await {
            return Ok(None);
        }
        let Some(sender_occupant) = self.room.find_occupant_by_real_jid(&msg.sender_jid) else {
            return Ok(None);
        };
        let sender_nick = sender_occupant.nick.clone();
        let sender_real_jid = sender_occupant.real_jid.clone();
        let sender_role = sender_occupant.role;
        let sender_affiliation = sender_occupant.affiliation;
        let muji_state = self.room.clear_muji_presence(&sender_nick, &msg.sender_jid);
        let room_jid = self.room.room_jid.clone();
        let recipients = self
            .room
            .occupants
            .values()
            .flat_map(|o| self.room.get_occupant_sessions(&o.nick))
            .collect();
        Ok(Some(MujiPresenceUpdateOutcome {
            update: PresenceUpdateOutcome {
                sender_nick,
                sender_real_jid,
                sender_role,
                sender_affiliation,
                room_jid,
                recipients,
            },
            sender_muji: muji_state.sender_muji,
            active_muji: muji_state.room_muji,
            session_mujis: muji_state.session_mujis,
            active_call_started: muji_state.active_call_started,
        }))
    }
}

impl kameo::message::Message<GetActiveMujiSessions> for RoomActor {
    type Reply = Result<Vec<FullJid>, Infallible>;

    async fn handle(
        &mut self,
        _msg: GetActiveMujiSessions,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let mut sessions: Vec<FullJid> = self
            .room
            .muji_state
            .values()
            .flat_map(|entries| entries.iter())
            .filter(|(_, muji)| muji.is_active())
            .map(|(session, _)| session.clone())
            .collect();
        sessions.sort_by_key(ToString::to_string);
        Ok(sessions)
    }
}

/// Apply the calling session's `<in-call xmlns='urn:waddle:in-call:0'>`
/// presence state (#1029 raised hand / #1030 mute). Carried on the same
/// MUC presence as the XEP-0272 `<muji/>` advertisement but tracked
/// independently so muji handling stays single-purpose. Like
/// [`UpsertMujiPresence`] this authenticates the sender as a current
/// occupant; a non-occupant yields `Ok(None)` and the caller falls back
/// to the join path.
pub struct UpsertInCallState {
    pub sender_jid: FullJid,
    pub state: crate::xep::InCallPresenceState,
}

#[derive(Debug, Clone)]
pub struct InCallPresenceUpdateOutcome {
    /// Resolved nick of the sending occupant (room-authoritative).
    pub sender_nick: String,
    /// Per-session in-call states for the nick after the update (only
    /// sessions advertising a non-empty state). The presence broadcaster
    /// decorates each reflected per-session presence with its owner's
    /// state from this list.
    pub in_call_sessions: Vec<(FullJid, crate::xep::InCallPresenceState)>,
}

impl kameo::message::Message<UpsertInCallState> for RoomActor {
    type Reply = Result<Option<InCallPresenceUpdateOutcome>, Infallible>;

    async fn handle(
        &mut self,
        msg: UpsertInCallState,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if !self.effectful_work_is_permitted().await {
            return Ok(None);
        }
        let Some(sender_occupant) = self.room.find_occupant_by_real_jid(&msg.sender_jid) else {
            return Ok(None);
        };
        let sender_nick = sender_occupant.nick.clone();
        self.room
            .upsert_in_call_state(&sender_nick, msg.sender_jid.clone(), msg.state);
        let in_call_sessions = self.room.in_call_sessions_for_nick(&sender_nick);
        Ok(Some(InCallPresenceUpdateOutcome {
            sender_nick,
            in_call_sessions,
        }))
    }
}

pub struct PingSelfCheck {
    pub nick: String,
    pub sender_jid: FullJid,
}

impl kameo::message::Message<PingSelfCheck> for RoomActor {
    type Reply = Result<(), RoomActorError>;

    async fn handle(
        &mut self,
        msg: PingSelfCheck,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if self.room.get_occupant(&msg.nick).is_none() {
            return Err(RoomActorError::OccupantNotFound(msg.nick.clone()));
        }
        // XEP-0410 server optimization: the self-ping succeeds iff THIS
        // session is joined under the pinged nick. A multi-session nick
        // (XEP-0045 multi-session join) keeps `occupant.real_jid` as the
        // FIRST session's full JID and appends later resources to
        // `occupant_sessions`, so the check must consult the full session
        // set — comparing only `real_jid` made every secondary device's
        // periodic self-ping fail with `not-acceptable` and rejoin-loop
        // (#1253).
        if !self
            .room
            .get_occupant_sessions(&msg.nick)
            .iter()
            .any(|session| session == &msg.sender_jid)
        {
            return Err(RoomActorError::OccupantNotFound(
                "Self-ping only allowed for own occupant".to_string(),
            ));
        }
        Ok(())
    }
}

pub struct ReconcileChannelBackedRoom {
    pub room_jid: BareJid,
    pub waddle_id: WaddleId,
    pub channel_id: ChannelId,
    pub desired_config: RoomConfig,
}

impl kameo::message::Message<ReconcileChannelBackedRoom> for RoomActor {
    type Reply = Result<(), super::RoomMutationError>;

    async fn handle(
        &mut self,
        msg: ReconcileChannelBackedRoom,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let ReconcileChannelBackedRoom {
            room_jid,
            waddle_id,
            channel_id,
            desired_config,
        } = msg;
        // ADR-0017 Phase 3 Slice 7 FIX 2: verify ownership BEFORE mutating.
        self.gate_mutation().await?;
        let instant_name = room_jid.node().map(|node| node.to_string());
        let mut desired_config = desired_config;
        desired_config.description = self.room.config.description.clone();
        if !self.room.config.name.is_empty()
            && instant_name.as_deref() != Some(self.room.config.name.as_str())
        {
            desired_config.name = self.room.config.name.clone();
        }
        let desired_config = desired_config.normalized();
        self.commit_durable(
            RoomDurableMutation::Config {
                config: desired_config.clone(),
                waddle_id: waddle_id.clone(),
                channel_id: channel_id.clone(),
            },
            crate::muc::RoomMutationEffects::none(),
        )
        .await?;
        self.room.waddle_id = waddle_id.into_string();
        self.room.channel_id = channel_id.into_string();
        self.replace_config(desired_config);
        self.config_revision = self.config_revision.saturating_add(1);
        self.advance_room_admission_revision();
        Ok(())
    }
}

/// Best-effort resolver-affiliation sync for joins the presence handler
/// rejects BEFORE any actor message (members-only registration-required
/// and resolver Outcast → forbidden). Without it, a stale
/// resolver-derived affiliation inside a live room actor (written
/// before the revocation) lingers on the room's affiliation list —
/// visible via admin queries and XEP-0045 §7.x member lists — until the
/// room is evicted. Provenance-aware via
/// `MucRoom::update_affiliation_from_resolver`: explicit grants (bans,
/// creator Owner) are never touched, and `Affiliation::None` removes
/// resolver-derived entries.
pub struct SyncResolverAffiliation {
    pub jid: BareJid,
    pub affiliation: Affiliation,
    /// The `admission_revision` the caller's rejection decision was
    /// computed against (the room snapshot of that same join attempt).
    /// The sync applies only while neither room-wide admission policy nor
    /// this JID's admission/affiliation state has changed. A later successful
    /// join of the re-granted user therefore invalidates this delayed sync,
    /// while an unrelated occupant's join does not strand the repair.
    pub expected_admission_revision: u64,
}

/// Typed outcome of a [`SyncResolverAffiliation`] request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, kameo::Reply)]
pub enum ResolverAffiliationSyncOutcome {
    /// The resolver verdict was applied (or was already in effect).
    Applied {
        /// Exact actor revision after this repair. A single ordered worker
        /// may use it for a newer verdict computed from the same original
        /// snapshot. Scope-aware actor watermarks decide whether a follow-up
        /// remains fresh when another member mutates in between.
        admission_revision: u64,
    },
    /// The room's admission state changed since the caller snapshotted
    /// it; the stale sync was refused.
    StaleAdmissionRevision,
    /// The actor is sealed pending destruction (#1108); nothing to sync.
    RoomSealed,
    /// Exact room ownership could not be proven, so the resolver result was
    /// not allowed to mutate this actor's affiliation memory.
    OwnershipUnavailable,
    /// An invite compensation owns the invitee affiliation until its
    /// terminal result is acknowledged.
    InviteRollbackPending,
}

impl kameo::message::Message<SyncResolverAffiliation> for RoomActor {
    type Reply = ResolverAffiliationSyncOutcome;

    async fn handle(
        &mut self,
        msg: SyncResolverAffiliation,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if self.reject_sealed_join().await.is_err() {
            return ResolverAffiliationSyncOutcome::RoomSealed;
        }
        match self.gate_join_ownership().await {
            Ok(()) => {}
            Err(RoomActorError::RoomSealed) => {
                return ResolverAffiliationSyncOutcome::RoomSealed;
            }
            Err(_) => return ResolverAffiliationSyncOutcome::OwnershipUnavailable,
        }
        if self.invite_rollback_pending(&msg.jid) {
            return ResolverAffiliationSyncOutcome::InviteRollbackPending;
        }
        if !self.admission_revision_is_current(&msg.jid, msg.expected_admission_revision) {
            return ResolverAffiliationSyncOutcome::StaleAdmissionRevision;
        }
        // An applied change is itself an admission-relevant affiliation
        // change, so it bumps the revision like `ChangeAffiliation` —
        // any join admitted against the pre-sync snapshot retries.
        if self
            .room
            .update_affiliation_from_resolver(msg.jid.clone(), msg.affiliation)
            .is_some()
        {
            self.invalidate_invite_grant(&msg.jid);
            self.advance_member_admission_revision(&msg.jid);
        }
        ResolverAffiliationSyncOutcome::Applied {
            admission_revision: self.admission_revision,
        }
    }
}

impl RoomActor {
    /// The departed nick is held by someone the departure did not account
    /// for: a different bare JID, or ANY occupant when the departure freed
    /// the nick (`removed_last_session`) — e.g. the same account back on a
    /// new resource. Replaying would announce a live occupant's departure.
    /// Sibling sessions of the same bare JID that still held the nick when
    /// the departure completed (`removed_last_session == false`) are the
    /// normal multi-resource case — but only while the nick is still that
    /// same generation: once the siblings left too and the nick was retaken
    /// (even by the same account), the captured roster/Muji state is stale.
    fn nick_retaken(&self, receipt: &super::DepartureReceipt) -> bool {
        let nick = receipt_nick(receipt);
        let holder = self.room.get_occupant(nick);
        let same_generation_and_account = |occupant: &crate::muc::room::Occupant| {
            occupant.real_jid.to_bare() == receipt.jid.to_bare()
                && self.room.current_nickname_generation(nick) == receipt.nick_generation
        };
        match &receipt.outcome {
            // A non-final departure captured its siblings' roster/Muji state:
            // replayable only while those siblings (same account, same nick
            // generation) still hold the nick — an absent nick means they
            // left too and the captured state would resurrect it.
            super::DepartureReceiptOutcome::Left(outcome) if !outcome.removed_last_session => {
                !holder.is_some_and(same_generation_and_account)
            }
            // The departure freed the nick: anyone holding it now is a newer
            // generation (the same account on a new resource included).
            super::DepartureReceiptOutcome::Left(_) => holder.is_some(),
            super::DepartureReceiptOutcome::Suppressed { .. } => {
                holder.is_some_and(|occupant| !same_generation_and_account(occupant))
            }
        }
    }
}

fn receipt_nick(receipt: &super::DepartureReceipt) -> &str {
    match &receipt.outcome {
        super::DepartureReceiptOutcome::Left(outcome) => outcome.nick.as_str(),
        super::DepartureReceiptOutcome::Suppressed { nick, .. } => nick.as_str(),
    }
}

fn receipt_disposition(receipt: super::DepartureReceipt) -> LeaveDisposition {
    let attempt = receipt.attempt;
    match receipt.outcome {
        super::DepartureReceiptOutcome::Left(mut outcome) => {
            outcome.acknowledge = attempt;
            LeaveDisposition::Left(outcome)
        }
        super::DepartureReceiptOutcome::Suppressed { nick, affiliation } => {
            LeaveDisposition::Suppressed {
                nick,
                affiliation,
                attempt,
            }
        }
    }
}
