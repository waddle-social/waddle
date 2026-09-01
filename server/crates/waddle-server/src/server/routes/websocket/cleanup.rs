use super::*;
use super::{
    frame::ResponseBatch,
    replay::{
        drain_outbound_into_replay, drain_outbound_into_terminal_recovery, PendingRowDrainPolicy,
        TerminalDrainContext,
    },
    state::WsConnState,
    stream_management::sm_show_from_name,
    LocalDepartureItem,
};
use std::collections::HashSet;
use waddle_xmpp::muc::durable::OccupancyLeaveCause;
use waddle_xmpp::muc::room_actor::{
    LeaveDisposition, LeaveOutcome, LeaveSessionSelector, SealGuard,
};
use waddle_xmpp::muc::room_registry_actor::{RoomAcquisition, RoomRegistryError};
use waddle_xmpp::muc::RoomRegistry;
use waddle_xmpp::xep::xep0272::Muji;
use waddle_xmpp::xep::xep0421::OccupantIdentity;

/// A force-detach may race a short, in-flight operation on the old
/// `UserActor`.  Keep the resume handoff on its synchronous path for those
/// brief windows rather than immediately handing it to the janitor.
const FORCE_DETACH_BUSY_UNREGISTER_ATTEMPTS: usize = 3;

fn force_detach_busy_unregister_backoff(attempt: usize) -> std::time::Duration {
    std::time::Duration::from_millis(50 * (attempt as u64 + 1))
}

pub(super) async fn retry_force_detach_busy_unregister<F, Fut, E>(
    mut unregister: F,
) -> Result<waddle_xmpp::registry::UnregisterAndReleaseOutcome, E>
where
    F: FnMut() -> Fut,
    Fut:
        std::future::Future<Output = Result<waddle_xmpp::registry::UnregisterAndReleaseOutcome, E>>,
{
    for attempt in 0..FORCE_DETACH_BUSY_UNREGISTER_ATTEMPTS {
        let outcome = unregister().await;
        if matches!(
            &outcome,
            Ok(
                waddle_xmpp::registry::UnregisterAndReleaseOutcome::RetryableFailure(
                    waddle_xmpp::registry::user_registry::UnregisterAndReleaseRetryableFailure::UserActorBusy
                )
            )
        ) && attempt + 1 < FORCE_DETACH_BUSY_UNREGISTER_ATTEMPTS
        {
            tokio::time::sleep(force_detach_busy_unregister_backoff(attempt)).await;
            continue;
        }
        return outcome;
    }
    unreachable!("force-detach busy retry loop always returns")
}

#[cfg(feature = "clustering")]
async fn unregister_remote_user_resource_if_owner(
    state: &WebSocketState,
    jid: &FullJid,
    owner: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> crate::clustering::route_bridge::RemoteResourceUnregisterOutcome {
    if let Some(bridge) = state
        .deps
        .app_state
        .clustering_claims
        .ordered_relay_delivery_bridge
        .as_ref()
    {
        return bridge
            .unregister_remote_user_resource_if_owner(jid, owner)
            .await;
    }
    crate::clustering::route_bridge::RemoteResourceUnregisterOutcome::NotRegistered
}

#[cfg(not(feature = "clustering"))]
async fn unregister_remote_user_resource_if_owner(
    _state: &WebSocketState,
    _jid: &FullJid,
    _owner: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> () {
}

fn muji_reflection_rank(muji: &Muji) -> u8 {
    if muji.is_empty() {
        0
    } else if muji.is_active() {
        2
    } else {
        1
    }
}

/// Broadcast a `<presence type='unavailable'/>` from the leaving
/// occupant's room-nick JID to every remaining occupant when their
/// LAST session for that nick departs.
///
/// XEP-0045 §7.14: the room is responsible for telling remaining
/// occupants that an occupant has left. The wire shape (room/nick
/// `from`, hat-less, `<x xmlns='muc#user'>` with role/affiliation,
/// XEP-0421 `<occupant-id/>`) is produced by the typed
/// `waddle_xmpp::muc::build_leave_presence` builder.
///
/// Used by both the explicit-leave path (`handle_muc_leave`) and the
/// unclean-disconnect path (`cleanup_muc_presence`) so the two cannot
/// drift. Prior to this helper, `cleanup_muc_presence` skipped the
/// broadcast entirely, which manifested as the "N in call" chip
/// staying lit forever after a tab close: other occupants never
/// received the leave signal, so their client-side
/// `$mucCallParticipants` never cleared the stale nick.
/// Why a bounded `LeaveByRealJid` ask produced no disposition.
#[derive(Debug)]
pub(crate) enum LeaveAskFailure {
    /// The whole ask (mailbox admission + reply) exceeded `LEAVE_ASK_TIMEOUT`.
    /// The actor may or may not have completed the departure: the retry
    /// carries the same attempt id so a completed one replays its receipt.
    Timeout,
    /// The actor answered with a typed error (sealed / ownership).
    Handler(waddle_xmpp::muc::room_actor::RoomActorError),
    /// The actor is not running or stopped before replying.
    Transport,
}

/// One bounded `LeaveByRealJid` ask: mailbox admission and the reply share a
/// single `LEAVE_ASK_TIMEOUT` deadline, so a wedged room costs a caller at
/// most that once.
pub(crate) async fn ask_leave_bounded(
    actor: &kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor>,
    message: LeaveByRealJid,
) -> Result<LeaveDisposition, LeaveAskFailure> {
    match tokio::time::timeout(
        super::LEAVE_ASK_TIMEOUT,
        actor.ask(message).mailbox_timeout(super::LEAVE_ASK_TIMEOUT),
    )
    .await
    {
        Err(_elapsed) => Err(LeaveAskFailure::Timeout),
        Ok(Ok(disposition)) => Ok(disposition),
        Ok(Err(kameo::error::SendError::Timeout(_))) => Err(LeaveAskFailure::Timeout),
        Ok(Err(kameo::error::SendError::HandlerError(error))) => {
            Err(LeaveAskFailure::Handler(error))
        }
        Ok(Err(_)) => Err(LeaveAskFailure::Transport),
    }
}

/// Queue an acknowledgement for a delivered `LeaveByRealJid` reply so the
/// actor drops the departure receipt it retained for lost-reply replay.
///
/// Awaiting mailbox admission is deliberate: dropping a full-mailbox
/// `try_send` would leave a normal-path receipt replayable. The wait is
/// bounded by one leave deadline (a wedged room must not stall the caller),
/// and an acknowledgement that could not be handed over in time becomes a
/// retained [`LocalDepartureItem::AckReceipt`] the janitor retries: the
/// effects already ran, so the receipt MUST go, or a later gone-JID leave of
/// the same cause would replay them.
pub(crate) async fn ack_departure_receipt(
    pending: &PendingLocalMucDepartures,
    actor: &kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor>,
    room: &BareJid,
    jid: &FullJid,
    attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId,
) {
    if try_ack_departure_receipt(actor, attempt).await {
        return;
    }
    pending.record(LocalDepartureItem::AckReceipt {
        room: room.clone(),
        jid: jid.clone(),
        attempt,
        absent_sweeps: 0,
    });
}

/// A live task's effects ran: hand its write-ahead entry over to the owed
/// acknowledgement (atomically, before awaiting the actor), then deliver the
/// acknowledgement; the owed entry is cleared only on an authoritative answer.
pub(crate) fn acknowledge_in_flight(
    pending: &Arc<PendingLocalMucDepartures>,
    actor: &kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor>,
    in_flight: &LocalDepartureItem,
    acknowledge: waddle_xmpp::muc::room_actor::LeaveAttemptId,
) {
    let LocalDepartureItem::InFlight { room, jid, .. } = in_flight else {
        return;
    };
    // Synchronous handover first: from here the only responsibility is the
    // acknowledgement, which is delivered by a detached task so the caller
    // (a WS handler under the frame backstop, a cleanup task) returns its own
    // response/effects immediately and cannot be cancelled mid-ack.
    pending.convert_in_flight_to_ack(in_flight, acknowledge);
    let pending = Arc::clone(pending);
    let actor = actor.clone();
    let (room, jid) = (room.clone(), jid.clone());
    tokio::spawn(async move {
        if try_ack_departure_receipt(&actor, acknowledge).await {
            pending.complete_ack(&room, &jid, acknowledge);
        }
    });
}

/// One bounded attempt to deliver the acknowledgement; `true` only once the
/// authoritative actor answered that the receipt is dropped. A timeout, a
/// dead actor, or an actor that lost ownership (its ledger may be on its way
/// to a successor) all leave the acknowledgement owed.
pub(crate) async fn try_ack_departure_receipt(
    actor: &kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor>,
    attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId,
) -> bool {
    matches!(
        tokio::time::timeout(
            super::LEAVE_ASK_TIMEOUT,
            actor
                .ask(waddle_xmpp::muc::room_actor::AckDepartureReceipt { attempt })
                .mailbox_timeout(super::LEAVE_ASK_TIMEOUT)
                .reply_timeout(super::LEAVE_ASK_TIMEOUT),
        )
        .await,
        Ok(Ok(
            waddle_xmpp::muc::room_actor::AckDepartureOutcome::Acknowledged
        ))
    )
}

/// Deliver the XEP-0045 §7.14 self-presence (`<status code='110'/>`) of a
/// completed departure to the leaver's own session, for departures whose
/// reply was lost and that the janitor replayed later.
pub(crate) async fn echo_muc_self_unavailable(
    state: &WebSocketState,
    room_jid: &BareJid,
    sender_jid: &FullJid,
    leaving_room_jid: &FullJid,
    affiliation: waddle_xmpp_core::Affiliation,
) {
    let from_jid = leaving_room_jid.clone();
    let sender_bare = sender_jid.to_bare();
    let identity = OccupantIdentity {
        bare_jid: &sender_bare,
        real_jid: Some(sender_jid),
        secret: &state.deps.occupant_id_secret,
    };
    let presence = waddle_xmpp::muc::build_leave_presence(
        &from_jid,
        sender_jid,
        affiliation,
        waddle_xmpp::muc::MucPresenceStatus::new(true, false),
        &identity,
    );
    super::handlers::presence::route_room_presence_to_occupant(
        state,
        room_jid,
        sender_jid,
        Stanza::Presence(presence),
    )
    .await;
}

/// Per-recipient progress of a departure's fan-out, recorded on the
/// retained inventory entry so a task that dies mid fan-out is resumed from
/// the first recipient it did NOT reach (never announcing the departure twice
/// to the ones it did).
pub(crate) struct LeaveFanOutProgress<'a> {
    /// Recipients already notified by an earlier (died) attempt at this
    /// departure: skipped.
    pub skip: &'a HashSet<FullJid>,
    /// Where to record progress — the live task's own inventory entry. `None`
    /// for the janitor, whose item is drained from the inventory while it
    /// runs (its own fan-out is not resumable; it only consumes `skip`).
    pub record: Option<(&'a PendingLocalMucDepartures, &'a LocalDepartureItem)>,
}

impl LeaveFanOutProgress<'_> {
    fn already_notified(&self, recipient: &FullJid) -> bool {
        self.skip.contains(recipient)
    }

    fn notified(&self, recipient: &FullJid) {
        if let Some((pending, item)) = self.record {
            pending.note_notified(item, recipient);
        }
    }
}

pub(crate) static NO_SKIP: std::sync::LazyLock<HashSet<FullJid>> =
    std::sync::LazyLock::new(HashSet::new);

pub(crate) async fn broadcast_muc_leave_to_remaining_resumable(
    state: &WebSocketState,
    room_jid: &BareJid,
    sender_jid: &FullJid,
    outcome: &LeaveOutcome,
    progress: Option<LeaveFanOutProgress<'_>>,
) {
    if !outcome.removed_last_session {
        return;
    }
    let from_jid = outcome.leaving_room_jid.clone();
    let sender_bare = sender_jid.to_bare();
    for occupant_jid in &outcome.remaining_occupants {
        if progress
            .as_ref()
            .is_some_and(|progress| progress.already_notified(occupant_jid))
        {
            continue;
        }
        let identity = OccupantIdentity {
            bare_jid: &sender_bare,
            real_jid: Some(sender_jid),
            secret: &state.deps.occupant_id_secret,
        };
        let presence = waddle_xmpp::muc::build_leave_presence(
            &from_jid,
            occupant_jid,
            outcome.affiliation,
            waddle_xmpp::muc::MucPresenceStatus::new(false, false),
            &identity,
        );
        super::handlers::presence::route_room_presence_to_occupant(
            state,
            room_jid,
            occupant_jid,
            Stanza::Presence(presence),
        )
        .await;
        if let Some(progress) = progress.as_ref() {
            progress.notified(occupant_jid);
        }
    }
}

/// The leave fan-out (last session) and the Muji-clear fan-out (siblings
/// remain) are mutually exclusive for one departure, so one progress list
/// serves whichever runs.
pub(crate) async fn broadcast_muc_muji_clear_to_remaining_resumable(
    state: &WebSocketState,
    room_jid: &BareJid,
    leaving_real_jid: &FullJid,
    outcome: &LeaveOutcome,
    progress: Option<LeaveFanOutProgress<'_>>,
) {
    if outcome.removed_last_session || !outcome.cleared_muji_state {
        return;
    }
    let from_jid = outcome.leaving_room_jid.clone();
    let mut entries = Vec::with_capacity(outcome.remaining_muji_sessions.len() + 1);
    entries.push((leaving_real_jid.clone(), Muji::default()));
    entries.extend(outcome.remaining_muji_sessions.iter().cloned());
    entries.sort_by_key(|(owner_jid, muji)| (muji_reflection_rank(muji), owner_jid.to_string()));
    for occupant_jid in &outcome.remaining_occupants {
        if progress
            .as_ref()
            .is_some_and(|progress| progress.already_notified(occupant_jid))
        {
            continue;
        }
        for (owner_jid, muji) in &entries {
            let owner_bare = owner_jid.to_bare();
            let identity = OccupantIdentity {
                bare_jid: &owner_bare,
                real_jid: Some(owner_jid),
                secret: &state.deps.occupant_id_secret,
            };
            let is_self = occupant_jid.to_bare() == owner_bare;
            let mut presence = waddle_xmpp::muc::build_occupant_presence(
                &from_jid,
                occupant_jid,
                outcome.affiliation,
                outcome.role,
                waddle_xmpp::muc::MucPresenceStatus::new(is_self, false),
                &identity,
            );
            if !muji.is_empty() {
                presence.payloads.push(muji.to_element());
            }
            super::handlers::presence::route_room_presence_to_occupant(
                state,
                room_jid,
                occupant_jid,
                Stanza::Presence(presence),
            )
            .await;
        }
        if let Some(progress) = progress.as_ref() {
            progress.notified(occupant_jid);
        }
    }
}

/// Clean up MUC room presence when a connection disconnects
/// Public alias for the MUC-presence cleanup used by the SM expired-session
/// janitor in `server::mod`. Thin passthrough so the janitor doesn't need
/// to reimplement the room traversal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MucCleanupOutcome {
    Completed,
    Failed,
}

#[cfg(any(not(feature = "clustering"), test))]
pub async fn cleanup_muc_presence_for_jid(
    state: &WebSocketState,
    jid: &FullJid,
) -> MucCleanupOutcome {
    if cleanup_muc_presence(state, jid).await {
        MucCleanupOutcome::Completed
    } else {
        MucCleanupOutcome::Failed
    }
}

/// Re-drive the local room traversal after a retained departure becomes due.
pub(crate) async fn redrive_local_muc_cleanup(
    state: &WebSocketState,
    jid: &FullJid,
    attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId,
    remote_ceiling: u64,
) -> MucCleanupOutcome {
    // The redrive re-uses the ORIGINAL sweep's attempt as its occupancy-order
    // ceiling: sessions that (re)joined since are classified `Superseded` by
    // the actor instead of being evicted by a late retry (#1647). Failures
    // do not re-record the sweep — the janitor requeues it with backoff.
    if cleanup_muc_presence_with_origin(
        state,
        jid,
        None,
        attempt,
        SweepFailureRecording::JanitorRequeues { remote_ceiling },
    )
    .await
    {
        MucCleanupOutcome::Completed
    } else {
        MucCleanupOutcome::Failed
    }
}

/// Same cleanup path, but with explicit ordered-relay origin provenance for
/// clustered remote MUC leaves.
#[cfg(feature = "clustering")]
pub async fn cleanup_muc_presence_for_jid_with_origin(
    state: &WebSocketState,
    jid: &FullJid,
    origin: crate::server::routes::interpret::OrderedRelayRouteOrigin,
) -> MucCleanupOutcome {
    if cleanup_muc_presence_with_origin(
        state,
        jid,
        Some(&origin),
        waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
        SweepFailureRecording::RecordSweep,
    )
    .await
    {
        MucCleanupOutcome::Completed
    } else {
        MucCleanupOutcome::Failed
    }
}

/// #1249: re-drive the MUC cleanup for a session that no longer exists
/// locally. Called by the reconciliation janitor for occupants whose
/// earlier disconnect cleanup could not reach the remote room owner
/// (node unreachable, claim lookup failure, origin claim held
/// elsewhere) — `cleanup_remote_muc_presence` re-takes the restored
/// membership snapshots and retries the relay, so occupancy converges
/// instead of ghosting until the next same-JID disconnect.
///
/// Runs the FULL cleanup (remote relay pass + local room sweep), not
/// just the remote pass: a room claim that migrated to THIS node
/// between the failed relay and the re-drive classifies as
/// `LocalRoom` / `NoRemoteOccupancy` (membership forgotten), and only
/// the local `LeaveByRealJid` loop can then remove the occupancy from
/// the now-local `RoomActor` (codex review P1 on PR #1277).
///
/// Residual (documented, #1195): when the user's `UserActor` claim is
/// held by another node (second device online there) AND the
/// disconnect-time remote-resource relay failed (e.g. partition), the
/// re-drive's `Entity(UserActor)` origin stays `OriginUnavailable`
/// until that claim is released — the ghost heals when the other
/// device disconnects or the claim expires, not before.
#[cfg(feature = "clustering")]
pub(crate) async fn redrive_remote_muc_cleanup(
    state: &WebSocketState,
    jid: &FullJid,
) -> MucCleanupOutcome {
    if cleanup_muc_presence_with_origin(
        state,
        jid,
        None,
        waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
        SweepFailureRecording::RecordSweep,
    )
    .await
    {
        MucCleanupOutcome::Completed
    } else {
        MucCleanupOutcome::Failed
    }
}

/// Response-path variant of [`maybe_evict_empty_room`]: run the guarded
/// destroy on a detached task so a handler under the frame backstop returns
/// its §7.14 self-presence without waiting on the registry. The detached ask
/// keeps the registry handle's bounded mailbox/reply timeouts, so transient
/// registry saturation resolves by waiting rather than by dropping the
/// request. The destroy stays revision-guarded.
pub(crate) fn evict_empty_room_detached(
    state: &WebSocketState,
    room_jid: &BareJid,
    outcome: &LeaveOutcome,
) {
    if !(outcome.removed_last_session && outcome.occupant_count == 0 && !outcome.is_persistent) {
        return;
    }
    let registry = state.deps.protocol.room_registry.clone();
    let pending = Arc::clone(&state.deps.protocol.pending_local_muc_departures);
    let room_jid = room_jid.clone();
    let occupancy_revision = outcome.occupancy_revision;
    tokio::spawn(async move {
        match RoomRegistry::wrap(registry)
            .destroy_room_if_inactive(
                room_jid.clone(),
                occupancy_revision,
                SealGuard::EmptyNonPersistent,
            )
            .await
        {
            Ok(outcome) if outcome.is_definitive() => {}
            // Retained by the registry, or the ask failed: the destroy stays
            // owed — retain it for the janitor, which retries the bounded ask
            // until the registry answers definitively.
            outcome => {
                warn!(room = %room_jid, ?outcome, "Detached guarded destroy of empty room not definitive; retained for retry");
                pending.record(LocalDepartureItem::EvictEmptyRoom {
                    room: room_jid,
                    occupancy_revision,
                });
            }
        }
    });
}

/// If `outcome` represents the final occupant leaving a
/// non-persistent (instant-style) MUC room, dispatch `DestroyRoom`
/// to the room registry so the per-room `RoomActor` is reaped and
/// its entry is removed from `RoomRegistryActor.rooms`.
///
/// Persistent rooms (`RoomConfig.persistent == true`, the default
/// and the shape Waddle channels use) are intentionally left in
/// place: the in-memory actor holds the authoritative caches for
/// affiliations, pin list, and room subject, and a separate
/// re-hydration audit is required before they can be safely
/// evicted. Empty persistent rooms therefore continue to retain
/// their actor across this PR; the residual growth they cause is
/// the next state-inventory-driven follow-up.
///
/// Errors here are logged and returned to sweep callers. Interactive leave
/// paths intentionally ignore the status because the leave itself has already
/// succeeded on the wire; failing that response would be worse than letting
/// the registry janitor catch the room on its next dead-actor sweep.
pub(crate) async fn maybe_evict_empty_room(
    state: &WebSocketState,
    room_jid: &BareJid,
    outcome: &LeaveOutcome,
) -> bool {
    let outcome_revision = outcome.occupancy_revision;
    if !(outcome.removed_last_session && outcome.occupant_count == 0 && !outcome.is_persistent) {
        return true;
    }
    // #1108: revision-guarded destroy. The registry asks the room actor
    // to seal itself only if it is still empty at the occupancy revision
    // captured by this leave — a join admitted after the leave bumps the
    // revision and the destroy refuses instead of orphaning the fresh
    // occupant.
    // Bounded: a stalled guarded destroy must not keep the leave task (and
    // its write-ahead retention) open indefinitely; an empty room left behind
    // is reaped by the registry's inactivity sweep.
    let destroyed = match tokio::time::timeout(
        2 * super::LEAVE_ASK_TIMEOUT,
        RoomRegistry::wrap(state.deps.protocol.room_registry.clone()).destroy_room_if_inactive(
            room_jid.clone(),
            outcome.occupancy_revision,
            SealGuard::EmptyNonPersistent,
        ),
    )
    .await
    {
        Ok(Ok(outcome)) if outcome.is_definitive() => outcome.destroyed(),
        Ok(Ok(outcome)) => {
            // Retained by the registry: hand the owed destroy to the janitor.
            warn!(room = %room_jid, ?outcome, "Guarded destroy of empty room retained; requeued");
            state.deps.protocol.pending_local_muc_departures.record(
                LocalDepartureItem::EvictEmptyRoom {
                    room: room_jid.clone(),
                    occupancy_revision: outcome_revision,
                },
            );
            return true;
        }
        Ok(Err(error)) => {
            warn!(room = %room_jid, error = %error, "Failed guarded destroy of empty room; retained for retry");
            state.deps.protocol.pending_local_muc_departures.record(
                LocalDepartureItem::EvictEmptyRoom {
                    room: room_jid.clone(),
                    occupancy_revision: outcome_revision,
                },
            );
            return false;
        }
        Err(_) => {
            warn!(room = %room_jid, "Guarded destroy of empty room timed out; retained for retry");
            state.deps.protocol.pending_local_muc_departures.record(
                LocalDepartureItem::EvictEmptyRoom {
                    room: room_jid.clone(),
                    occupancy_revision: outcome_revision,
                },
            );
            return false;
        }
    };
    if destroyed {
        debug!(
            room = %room_jid,
            "Evicted empty non-persistent MUC room from registry"
        );
    } else {
        // Either the room was already absent (race with another leave path) or
        // a new occupant was admitted after this leave. Either way we don't
        // want to fail the user's leave on this.
        debug!(
            room = %room_jid,
            "Guarded eviction returned false; room re-admitted an occupant or was already cleared"
        );
    }
    true
}

/// Whether [`cleanup_connection_shutdown`] actually persisted a detached,
/// resumable XEP-0198 snapshot (council-adjudicated FIX 4: "ack only what
/// actually happened"). The cross-node resume force-detach ack
/// (`connection.rs`) maps [`Self::Detached`] onto
/// [`waddle_xmpp::registry::ForceDetachOutcome::Detached`] — the only
/// outcome that authorizes the remote asker's `steal_for_resume` — and
/// every other exit path (superseded, no cleanup-eligible JID, not
/// resumable, no registry ownership, non-owned entry, `store_session`
/// failure, ownership-moved-during-detach) onto
/// [`waddle_xmpp::registry::ForceDetachOutcome::NotPersisted`], which the
/// bridge/asker treat identically to "not live locally" (re-check
/// persistence, retry).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "the cross-node resume force-detach ack must reflect whether a snapshot was actually persisted"]
pub(super) enum ConnectionShutdownOutcome {
    /// A detach-for-resume snapshot was persisted (`store_session`
    /// succeeded).
    Detached,
    /// No detach-for-resume snapshot was persisted by this call.
    NotPersisted,
}

pub(super) async fn cleanup_connection_shutdown(
    state: &WebSocketState,
    outbound_rx: &mut mpsc::Receiver<OutboundStanza>,
    conn: &mut WsConnState,
    superseded: bool,
) -> ConnectionShutdownOutcome {
    cleanup_connection_shutdown_inner(state, outbound_rx, conn, superseded, None).await
}

/// Force-detach cleanup has a stricter completion boundary than ordinary
/// teardown: a cross-node resume waiter is acknowledged only after the actor
/// registry has synchronously observed the resource removal. Stale-actor
/// retirement is the exception: its already-running registry handler owns the
/// removal and waits for this cleanup acknowledgement.
pub(super) async fn cleanup_force_detach_connection_shutdown(
    state: &WebSocketState,
    outbound_rx: &mut mpsc::Receiver<OutboundStanza>,
    conn: &mut WsConnState,
    superseded: bool,
    origin: waddle_xmpp::registry::ForceDetachOrigin,
) -> ConnectionShutdownOutcome {
    cleanup_connection_shutdown_inner(state, outbound_rx, conn, superseded, Some(origin)).await
}

async fn cleanup_connection_shutdown_inner(
    state: &WebSocketState,
    outbound_rx: &mut mpsc::Receiver<OutboundStanza>,
    conn: &mut WsConnState,
    superseded: bool,
    force_detach_origin: Option<waddle_xmpp::registry::ForceDetachOrigin>,
) -> ConnectionShutdownOutcome {
    let force_detach = force_detach_origin.is_some();
    // A superseded ordinary connection must not touch the replacement's
    // registry or MUC state. Terminal SM recovery is different: it only
    // promotes this task's already accepted delivery and leaves those shared
    // resources alone.
    if superseded && !conn.sm_recovery_required {
        forget_terminal_shadow_stream_and_release_claim(state, &conn.sm_state).await;
        return ConnectionShutdownOutcome::NotPersisted;
    }
    // Note: we deliberately do NOT mirror `conn.phase` Closing into
    // the SM here — the drain loops below need the SM in `Ready`
    // phase so they can run the recipient pass on queued
    // `DeliveryKind::PeerStanza` values. Any explicit Closing
    // transition that needs to lock out late peer dispatches has
    // already happened via the post-`handle_xmpp_frame` mirror in
    // the main loop.

    let Some(jid) = conn.phase.cleanup_jid().cloned() else {
        return ConnectionShutdownOutcome::NotPersisted;
    };

    if superseded {
        return promote_terminal_recovery(state, outbound_rx, &jid, conn).await;
    }

    // Claim-loss owner-managed retirement (an attached SM claim was demoted
    // or terminally released, so the live fence is already gone): a resumable
    // detach is doomed here — the fenced snapshot write fails `NotOwner`
    // after `store_session` has inserted the detached session in memory,
    // leaving the unacked queue neither durably resumable nor promoted.
    // Route it through terminal XEP-0198 §5 promotion instead. Owner-managed
    // retirements that still hold a live fence keep the ordinary
    // detach-for-resume path.
    if matches!(
        force_detach_origin,
        Some(waddle_xmpp::registry::ForceDetachOrigin::OwnerManagedRetirement)
    ) && conn.sm_state.is_resumable()
        && !conn.sm_recovery_required
        && conn.sm_state.stream_id.as_deref().is_some_and(|stream_id| {
            state
                .deps
                .protocol
                .sm_session_registry
                .current_sm_claim_fence(stream_id)
                .is_none()
        })
    {
        conn.begin_terminal_sm_recovery();
        let _ = forget_terminal_shadow_stream_and_wait(state, &conn.sm_state).await;
        return promote_terminal_recovery(state, outbound_rx, &jid, conn).await;
    }

    let should_detach_for_resume = (conn.sm_state.is_resumable()
        && !matches!(conn.phase, ConnectionPhase::Closing { .. }))
        || conn.sm_recovery_required;

    if should_detach_for_resume {
        if conn.registry_owner.is_none() {
            if conn.sm_recovery_required {
                return promote_terminal_recovery(state, outbound_rx, &jid, conn).await;
            }
            conn.pending_resume_stream_id = None;
            conn.pending_resume_h = None;
            drop(conn.pending_resume_claim.take());
        }
        let Some(owner) = conn.registry_owner.as_ref() else {
            debug!(jid = %jid, "Skipped SM detach for connection without registry ownership");
            forget_terminal_shadow_stream_and_release_claim(state, &conn.sm_state).await;
            return ConnectionShutdownOutcome::NotPersisted;
        };
        let presence_state = state
            .deps
            .protocol
            .connection_registry
            .get_presence_state(&jid);
        let Some(entry) = state
            .deps
            .protocol
            .connection_registry
            .entry_if_owner(&jid, owner)
        else {
            if conn.sm_recovery_required {
                return promote_terminal_recovery(state, outbound_rx, &jid, conn).await;
            }
            super::stream_management::defer_superseded_sm_claim(state, &conn.sm_state);
            debug!(jid = %jid, "Skipped SM detach for non-owned registry entry");
            forget_terminal_shadow_stream_and_release_claim(state, &conn.sm_state).await;
            return ConnectionShutdownOutcome::NotPersisted;
        };

        let carbons_enabled = conn.carbons_enabled;
        let presence_available = entry.is_presence_available();
        if !conn.sm_recovery_required {
            // First detach drain: snapshot whatever's already in the
            // channel into the unacked queue. No detached_stream_id yet
            // because we haven't decided to store the detached session.
            drain_outbound_into_replay(
                state,
                conn.state_machine.as_mut(),
                &mut conn.sm_state,
                conn.authenticated_session.as_ref(),
                outbound_rx,
                None,
                PendingRowDrainPolicy::PreserveForReplay,
            )
            .await;
        }
        let user_id = conn
            .authenticated_session
            .as_ref()
            .map(|session| session.user_jid.to_string())
            .unwrap_or_else(|| jid.to_bare().to_string());
        let detached_snapshot = waddle_xmpp::stream_management::DetachedSessionSnapshot {
            user_id,
            jid: jid.clone(),
            carbons_enabled,
            roster_interested: conn.roster_interested,
            blocklist_interested: conn.blocklist_interested,
            presence_available,
            presence_show: presence_state
                .as_ref()
                .and_then(|state| state.show.as_deref())
                .and_then(sm_show_from_name),
            presence_status: presence_state
                .as_ref()
                .and_then(|state| state.status.clone()),
            presence_priority: presence_state
                .as_ref()
                .map(|state| state.priority)
                .unwrap_or_else(|| entry.presence_priority()),
            presence_payloads: presence_state
                .map(|state| state.payloads)
                .unwrap_or_default(),
            // The once-per-session claim state travels with the
            // detached session (not inferred from presence): a
            // session that went available then unavailable before
            // detaching keeps its consumed claim across resume.
            pending_subscribes_flushed: entry
                .pending_subscribes_flushed
                .load(std::sync::atomic::Ordering::Acquire),
        };
        if let Some(stream_id) = conn.sm_state.stream_id.as_deref() {
            let stream_id = waddle_xmpp::pending_delivery::SmSessionId::new(stream_id);
            if !state
                .deps
                .protocol
                .ingress_shadow
                .wait_for_stream_idle(&stream_id, std::time::Duration::from_secs(30))
                .await
            {
                warn!(
                    jid = %jid,
                    %stream_id,
                    "refusing resumable SM handoff with unfinished ingress shadow work"
                );
                return promote_terminal_recovery(state, outbound_rx, &jid, conn).await;
            }
        }
        if let Some(detached) = conn.sm_state.to_detached_session(detached_snapshot.clone()) {
            if conn.sm_recovery_required {
                // The batch writer stopped before accepting the unwritten
                // tail into the capped SM queue.  Persisting a resumable
                // snapshot here would let a later `<resume/>` claim a stream
                // whose response tail was never recorded.  Promote only the
                // already-recorded queue through the established recovery
                // chain, then close as non-resumable so that resume fails
                // deliberately instead of silently omitting that tail.
                warn!(
                    jid = %jid,
                    stream_id = %detached.stream_id,
                    recorded = detached.unacked_stanzas.len(),
                    "SM send-window deferred capacity exhausted; promoting recorded queue and invalidating resume"
                );
                // Stop accepting new routed work, then recover everything
                // already accepted by the bounded channel. The paused batch
                // suffix never entered that channel and remains deliberately
                // excluded; dropping accepted routed work here would be a
                // separate silent-loss path.
                return promote_terminal_recovery(state, outbound_rx, &jid, conn).await;
            }
            let stream_id = detached.stream_id.clone();
            let Some(session) = conn.authenticated_session.as_ref() else {
                warn!(jid = %jid, "Refusing SM detach without an authenticated principal");
                return refuse_detach_without_principal(
                    state,
                    outbound_rx,
                    &jid,
                    conn,
                    vec![detached],
                    TerminalRowRecovery::default(),
                    TerminalRouteRemoval::NotAttempted,
                )
                .await;
            };
            let principal = match session.authenticated_principal_ref() {
                Ok(principal) => principal,
                Err(error) => {
                    warn!(jid = %jid, %error, "Refusing SM detach with an invalid persisted principal");
                    return refuse_detach_without_principal(
                        state,
                        outbound_rx,
                        &jid,
                        conn,
                        vec![detached],
                        TerminalRowRecovery::default(),
                        TerminalRouteRemoval::NotAttempted,
                    )
                    .await;
                }
            };
            match state
                .deps
                .protocol
                .sm_session_registry
                .store_session_with_principal(detached, principal)
                .await
            {
                Ok(displaced) => {
                    // Issue #1097: sessions the registry displaced to make
                    // room (max_sessions overflow, or a stale detached
                    // stream for this same JID) carry unacked queues that
                    // must run the XEP-0198 §5 promote → confirm chain
                    // instead of being dropped. Promote before anything
                    // else in this arm — the early-return below must not
                    // skip it.
                    if !displaced.is_empty() {
                        crate::sm_promotion::promote_displaced_sessions(
                            displaced.clone(),
                            crate::sm_promotion::DisplacedPromotionDeps {
                                sm_registry: &state.deps.protocol.sm_session_registry,
                                connection_registry: &state.deps.protocol.connection_registry,
                                user_registry: &state.deps.protocol.user_registry,
                                pending_storage: &state.deps.protocol.pending_delivery_storage,
                                blocking_storage: state.deps.protocol.blocking_storage.as_ref(),
                                server_domain: state.deps.auth_state.xmpp_domain.as_str(),
                            },
                        )
                        .await;
                        for dead in displaced {
                            cleanup_invalidated_detached_session(state, dead, None).await;
                        }
                    }
                    if state
                        .deps
                        .protocol
                        .connection_registry
                        .unregister_if_owner(&jid, owner)
                        .is_none()
                    {
                        // Ownership moved: the same client fresh-bound on a
                        // newer connection before this cleanup stored its
                        // detached session, so the newer bind's
                        // invalidation pass never saw this stream. The
                        // just-stored session still carries the unacked
                        // queue — run the XEP-0198 §5 promote →
                        // confirm_drained chain on it (issue #1097
                        // displaced contract) instead of erasing it, or
                        // the queue is lost with no promotion.
                        debug!(
                            jid = %jid,
                            stream_id = %stream_id,
                            "detached SM session lost ownership race; promoting its queue"
                        );
                        match state
                            .deps
                            .protocol
                            .sm_session_registry
                            .displace_stored_session_if_unclaimed(&stream_id)
                            .await
                        {
                            Ok(Some(displaced)) => {
                                let outcome = crate::sm_promotion::promote_displaced_sessions(
                                    vec![displaced],
                                    crate::sm_promotion::DisplacedPromotionDeps {
                                        sm_registry: &state.deps.protocol.sm_session_registry,
                                        connection_registry: &state
                                            .deps
                                            .protocol
                                            .connection_registry,
                                        user_registry: &state.deps.protocol.user_registry,
                                        pending_storage: &state
                                            .deps
                                            .protocol
                                            .pending_delivery_storage,
                                        blocking_storage: state
                                            .deps
                                            .protocol
                                            .blocking_storage
                                            .as_ref(),
                                        server_domain: state.deps.auth_state.xmpp_domain.as_str(),
                                    },
                                )
                                .await;
                                for completed in outcome.completed_stream_ids() {
                                    state.deps.protocol.ingress_shadow.forget_stream(
                                        &waddle_xmpp::pending_delivery::SmSessionId::new(completed),
                                    );
                                }
                            }
                            Ok(None) => {}
                            Err(error) => {
                                warn!(
                                    jid = %jid,
                                    stream_id = %stream_id,
                                    error = %error,
                                    "ownership-moved detach: displace failed; durable rows \
                                     remain for janitor/restart retry"
                                );
                            }
                        }
                        return ConnectionShutdownOutcome::NotPersisted;
                    }
                    // ADR-0017 Phase 1: prune the actor-tree entry at detach
                    // (Greptile review on PR #1177). We just removed the DashMap
                    // routing entry and are about to close this resource's
                    // channel; the actor entry is no longer live-routable.
                    // Keeping it would LEAK: a later SM-expiry cannot converge
                    // it — the janitor's `unregister` returns `None` (the
                    // DashMap entry is already gone) so its mirror never fires.
                    // Detached delivery is sourced from the SM session registry
                    // (not the actor), and a successful resume re-registers a
                    // fresh entry via `register_bound_connection_after_frame` →
                    // `mirror_register`, so pruning here does not affect resume.
                    // Owner-gated so a superseding newcomer is untouched.
                    if !force_detach {
                        crate::server::dual_registration::mirror_unregister(
                            &state.deps.protocol.user_registry,
                            &jid,
                            Some(std::sync::Arc::clone(owner)),
                        )
                        .await;
                    }
                    #[cfg(feature = "clustering")]
                    let remote_unregister_outcome =
                        unregister_remote_user_resource_if_owner(state, &jid, owner).await;
                    #[cfg(not(feature = "clustering"))]
                    unregister_remote_user_resource_if_owner(state, &jid, owner).await;
                    outbound_rx.close();
                    // Second detach drain: anything that arrived
                    // between the first drain and the registry
                    // unregister. With the detached session stored,
                    // we record both into the per-connection unacked
                    // queue AND the detached stream's replay buffer.
                    drain_outbound_into_replay(
                        state,
                        conn.state_machine.as_mut(),
                        &mut conn.sm_state,
                        conn.authenticated_session.as_ref(),
                        outbound_rx,
                        Some(&stream_id),
                        PendingRowDrainPolicy::PreserveForReplay,
                    )
                    .await;
                    if force_detach_origin.is_some_and(force_detach_requires_actor_unregister) {
                        let outcome = retry_force_detach_busy_unregister(|| async {
                            state
                                .deps
                                .protocol
                                .user_registry
                                .ask(
                                    waddle_xmpp::registry::user_registry::UnregisterAndReleaseIfEmptyWithoutPendingRecord {
                                        jid: jid.clone(),
                                        owner: Some(std::sync::Arc::clone(owner)),
                                    },
                                )
                                .mailbox_timeout(std::time::Duration::from_secs(2))
                                .reply_timeout(std::time::Duration::from_secs(2))
                                .await
                        })
                        .await;
                        match outcome {
                            Ok(waddle_xmpp::registry::UnregisterAndReleaseOutcome::Released
                            | waddle_xmpp::registry::UnregisterAndReleaseOutcome::RetainedLiveResources
                            | waddle_xmpp::registry::UnregisterAndReleaseOutcome::AlreadyAbsent) => {}
                            Ok(waddle_xmpp::registry::UnregisterAndReleaseOutcome::RetryableFailure(reason)) => {
                                let recorded = state
                                    .deps
                                    .protocol
                                    .user_registry
                                    .ask(waddle_xmpp::registry::RecordPendingUserUnregister {
                                        jid: jid.clone(),
                                        owner: Some(std::sync::Arc::clone(owner)),
                                    })
                                    .mailbox_timeout(std::time::Duration::from_secs(2))
                                    .reply_timeout(std::time::Duration::from_secs(2))
                                    .await;
                                match recorded {
                                    Ok(()) => warn!(?reason, "force-detach UserActor unregister remained busy after bounded retries; recorded janitor retry"),
                                    Err(record_error) => {
                                        warn!(?reason, ?record_error, "force-detach UserActor unregister retry could not be recorded");
                                        return ConnectionShutdownOutcome::NotPersisted;
                                    }
                                }
                            }
                            Err(error) => {
                                // The ask may have timed out before entering the actor OR after
                                // its handler committed. Submit a second, ordered actor ask so
                                // either case leaves an exact pending-unregister record before
                                // this force-detach cleanup returns and its ack can be sent.
                                let recorded = state
                                    .deps
                                    .protocol
                                    .user_registry
                                    .ask(waddle_xmpp::registry::RecordPendingUserUnregister {
                                        jid: jid.clone(),
                                        owner: Some(std::sync::Arc::clone(owner)),
                                    })
                                    .mailbox_timeout(std::time::Duration::from_secs(2))
                                    .reply_timeout(std::time::Duration::from_secs(2))
                                    .await;
                                match recorded {
                                    Ok(()) => warn!(?error, "force-detach UserActor unregister ask outcome was ambiguous; recorded janitor retry"),
                                    Err(record_error) => {
                                        // Do not claim a successful detach to the remote resume
                                        // waiter when this actor is unavailable to retain the
                                        // convergence obligation. The persisted snapshot remains
                                        // available to its normal retry path.
                                        warn!(?error, ?record_error, "force-detach UserActor unregister retry could not be recorded");
                                        return ConnectionShutdownOutcome::NotPersisted;
                                    }
                                }
                            }
                        }
                        #[cfg(feature = "clustering")]
                        if !remote_unregister_outcome.permits_detached_force_ack() {
                            warn!(
                                jid = %jid,
                                stream_id = %stream_id,
                                ?remote_unregister_outcome,
                                "force-detach remote owner unregister lacked cleanup proof"
                            );
                            return ConnectionShutdownOutcome::NotPersisted;
                        }
                    }
                    // Remove the routing entry only — the MUC occupant
                    // slot stays. On a successful resume we'll re-register
                    // the same FullJid and presence is preserved.
                    info!(
                        jid = %jid,
                        stream_id = %stream_id,
                        "SM session detached; awaiting resume"
                    );
                    return ConnectionShutdownOutcome::Detached;
                }
                Err(err) => {
                    warn!(jid = %jid, error = %err, "Failed to detach SM session; falling back to full cleanup");
                    forget_terminal_shadow_stream_and_release_claim(state, &conn.sm_state).await;
                    let detach_fail_removed = state
                        .deps
                        .protocol
                        .connection_registry
                        .unregister_if_owner(&jid, owner)
                        .is_some();
                    let cleanup_origin = clustered_cleanup_origin(state, &jid, owner).await;
                    if detach_fail_removed {
                        cleanup_muc_presence_with_origin(
                            state,
                            &jid,
                            cleanup_origin.as_ref(),
                            waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
                            SweepFailureRecording::RecordSweep,
                        )
                        .await;
                        // ADR-0017 Phase 1: mirror the unregister into the
                        // actor tree on the SM-detach-failure fallback. This
                        // session is never stored, so no SM-expiry janitor
                        // will ever converge it — without this mirror the
                        // resource leaks in the actor tree forever. Owner-gated
                        // (the same token that owned the DashMap slot) so a
                        // superseding newcomer's actor-tree entry is not clobbered.
                        crate::server::dual_registration::mirror_unregister(
                            &state.deps.protocol.user_registry,
                            &jid,
                            Some(std::sync::Arc::clone(owner)),
                        )
                        .await;
                        unregister_remote_user_resource_if_owner(state, &jid, owner).await;
                    }
                    // PR #438 review (Copilot): when SM detachment
                    // fails we fall back to a full unregister, so the
                    // caps resource→ver mapping AND any pending
                    // disco#info resolution must be cleared too —
                    // otherwise stale state lingers indefinitely.
                    state.deps.protocol.caps_resolver.drop_resource(&jid);
                    if !detach_fail_removed {
                        cleanup_muc_presence(state, &jid).await;
                    }
                    return ConnectionShutdownOutcome::NotPersisted;
                }
            }
        }
    }

    let removed = conn.registry_owner.as_ref().and_then(|owner| {
        state
            .deps
            .protocol
            .connection_registry
            .unregister_if_owner(&jid, owner)
            .map(|entry| (entry, std::sync::Arc::clone(owner)))
    });
    if let Some((removed_entry, owner)) = removed {
        // Capture presence availability from the entry we just owned and
        // removed — NOT from a fresh registry lookup, which after the
        // unregister could only observe a replacement connection's state.
        // The flag is only true if this session actually sent initial
        // available presence (RFC 6121 §4.2.2) and did not retract it.
        let was_presence_available = removed_entry.is_presence_available();
        let cleanup_origin = clustered_cleanup_origin(state, &jid, &owner).await;
        // XEP-0115 §6: drop the per-resource caps mapping for this
        // resource. The hash-keyed `CapsCache` itself stays warm so
        // a future session reusing the same `(hash, ver)` short-
        // circuits the disco#info round-trip.
        state.deps.protocol.caps_resolver.drop_resource(&jid);
        info!(jid = %jid, "WebSocket connection unregistered");
        broadcast_unavailable_if_no_replacement(state, &jid, was_presence_available).await;
        cleanup_muc_presence_with_origin(
            state,
            &jid,
            cleanup_origin.as_ref(),
            waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
            SweepFailureRecording::RecordSweep,
        )
        .await;
        // ADR-0017 Phase 1: mirror the unregister into the actor tree on
        // the dominant disconnect teardown path. Owner-gated (the same token
        // that owned the DashMap entry) so a superseding newcomer that
        // already replaced this FullJid's actor-tree entry is not clobbered.
        crate::server::dual_registration::mirror_unregister(
            &state.deps.protocol.user_registry,
            &jid,
            Some(std::sync::Arc::clone(&owner)),
        )
        .await;
        unregister_remote_user_resource_if_owner(state, &jid, &owner).await;
    } else {
        debug!(jid = %jid, "Skipped websocket cleanup for non-owned registry entry");
    }
    // Every path reaching here is a non-detach (full-cleanup or no-op)
    // teardown — never a persisted resumable snapshot.
    forget_terminal_shadow_stream_and_release_claim(state, &conn.sm_state).await;
    ConnectionShutdownOutcome::NotPersisted
}

async fn forget_terminal_shadow_stream_and_release_claim(
    state: &WebSocketState,
    sm_state: &waddle_xmpp::stream_management::StreamManagementState,
) {
    let Some(stream_id) = forget_terminal_shadow_stream_and_wait(state, sm_state).await else {
        return;
    };
    state
        .deps
        .protocol
        .sm_session_registry
        .release_attached_session_claim(&stream_id)
        .await;
}

async fn forget_terminal_shadow_stream_and_wait(
    state: &WebSocketState,
    sm_state: &waddle_xmpp::stream_management::StreamManagementState,
) -> Option<String> {
    // Retire first to close admission, then wait for every previously
    // admitted submission task to drain before ending the fence those tasks
    // captured at observation time. Enroll/Retire tasks are not counted in
    // per-stream pending activity; the retirement task fences on claim
    // absence itself and self-reschedules (`DeferredClaim`) when it runs
    // before the release below.
    forget_terminal_shadow_stream(state, sm_state);
    let stream_id = sm_state.stream_id.clone()?;
    let session_id = waddle_xmpp::pending_delivery::SmSessionId::new(stream_id.clone());
    while !state
        .deps
        .protocol
        .ingress_shadow
        .wait_for_stream_idle(&session_id, std::time::Duration::from_secs(30))
        .await
    {
        warn!(
            %session_id,
            "terminal SM cleanup is still waiting for admitted ingress shadow work to drain"
        );
    }
    Some(stream_id)
}

fn forget_terminal_shadow_stream(
    state: &WebSocketState,
    sm_state: &waddle_xmpp::stream_management::StreamManagementState,
) {
    let Some(stream_id) = sm_state.stream_id.as_deref() else {
        return;
    };
    state
        .deps
        .protocol
        .ingress_shadow
        .forget_stream(&waddle_xmpp::pending_delivery::SmSessionId::new(stream_id));
}

/// Promote terminal recovery without creating a resumable snapshot. This is
/// also safe after an ownership race because pending delivery is bare-JID
/// keyed and [`refuse_detach_without_principal`] owner-gates all live-entry
/// teardown.
enum TerminalRouteRemoval {
    NotAttempted,
    Attempted(Option<(bool, std::sync::Arc<std::sync::atomic::AtomicBool>)>),
}

struct TerminalRecoverySessionResult {
    session: waddle_xmpp::stream_management::DetachedSession,
    released_pending_rows: bool,
    queued_pending_rows: bool,
    release_failed_pending_rows: bool,
    prefix_redrive_aborted: bool,
}

/// Row-recovery facts a terminal cleanup carries into
/// [`refuse_detach_without_principal`] so the promotion aftermath can
/// re-drive or retain durable rows correctly.
#[derive(Default)]
struct TerminalRowRecovery {
    /// Sequence-bound rows were released back to bare-JID pending
    /// delivery (or row-backed channel entries released individually).
    released_rows: bool,
    /// Known sequence-bound rows failed to release and stay claimed by
    /// the dead stream until promotion's `release_claim` frees them.
    release_failed_known_rows: bool,
    /// Row ownership could not be discovered at all; the session's
    /// queue must stay out of promotion until a retry re-discovers it.
    ownership_unknown: bool,
    /// Promotion inserted fresh `pending_delivery` rows whose
    /// online-resource snapshot may have missed a concurrently binding
    /// replacement.
    queued_pending_rows: bool,
    /// A re-drive inside the recovery session aborted with entries retained
    /// behind the stuck row — final promotion must defer too.
    redrive_aborted: bool,
}

async fn promote_terminal_recovery(
    state: &WebSocketState,
    outbound_rx: &mut mpsc::Receiver<OutboundStanza>,
    jid: &FullJid,
    conn: &mut WsConnState,
) -> ConnectionShutdownOutcome {
    let detached_snapshot = terminal_recovery_snapshot(jid, conn);
    let Some(mut detached) = conn.sm_state.to_detached_session(detached_snapshot.clone()) else {
        super::stream_management::defer_superseded_sm_claim(state, &conn.sm_state);
        return ConnectionShutdownOutcome::NotPersisted;
    };
    let row_release = crate::sm_promotion::release_row_backed_replay_copies(
        &state.deps.protocol.sm_session_registry,
        &state.deps.protocol.pending_delivery_storage,
        &mut detached,
    )
    .await;
    // Overflow promotion can route directly to another resource. Remove this
    // terminal connection first, otherwise an exact-FullJID route can select
    // its already-closed channel and prevent the RFC 6121 bare-JID fallback.
    // The owner token keeps a same-FullJID replacement untouched.
    let terminal_route_removed = conn.registry_owner.as_ref().and_then(|owner| {
        conn.sm_recovery_required.then(|| {
            state
                .deps
                .protocol
                .connection_registry
                .unregister_if_owner(jid, owner)
                .map(|entry| (entry.is_presence_available(), std::sync::Arc::clone(owner)))
        })?
    });
    // Settle released rows BEFORE any incremental promotion can run: the
    // prefix path awaits blocklist/tombstone reads during which a
    // replacement can bind, and a later stanza delivered live there would
    // overtake the released earlier row. An aborted re-drive suppresses the
    // incremental path the same way ownership_unknown does.
    let released_redrive_aborted = row_release.released_rows
        && redrive_terminal_pending_rows_to_live_resource(state, &jid.to_bare()).await
            == TerminalRedriveOutcome::Aborted;
    // With row ownership unknown, neither the prefix nor the per-item
    // overflow promotion may run: unidentified row-backed replay copies
    // would be promoted as fresh work. The drain still releases channel
    // entries that carry their own explicit row id (ownership known).
    let recovery = terminal_recovery_session(
        state,
        outbound_rx,
        conn,
        detached,
        detached_snapshot,
        terminal_route_removed.is_some()
            && !row_release.ownership_unknown
            && !released_redrive_aborted
            // A known row whose release failed stays claimed until the
            // settled promotion's release_claim; incremental promotion would
            // deliver later entries to a live replacement ahead of it, and
            // the post-claim re-drive cannot repair that inversion.
            && !row_release.release_failed_known_rows,
    )
    .await;
    refuse_detach_without_principal(
        state,
        outbound_rx,
        jid,
        conn,
        vec![recovery.session],
        TerminalRowRecovery {
            released_rows: row_release.released_rows || recovery.released_pending_rows,
            release_failed_known_rows: row_release.release_failed_known_rows
                || recovery.release_failed_pending_rows,
            ownership_unknown: row_release.ownership_unknown,
            queued_pending_rows: recovery.queued_pending_rows,
            redrive_aborted: recovery.prefix_redrive_aborted || released_redrive_aborted,
        },
        TerminalRouteRemoval::Attempted(terminal_route_removed),
    )
    .await
}

#[derive(Clone)]
struct LivePendingFlushTarget {
    resource: FullJid,
    owner: std::sync::Arc<std::sync::atomic::AtomicBool>,
    sm_session: Option<waddle_xmpp::pending_delivery::SmSessionId>,
}

fn preferred_live_pending_flush_target(
    state: &WebSocketState,
    recipient: &BareJid,
) -> Option<LivePendingFlushTarget> {
    let mut resources = state
        .deps
        .protocol
        .connection_registry
        .get_available_resources_for_user(recipient)
        .into_iter()
        // Match the XEP-0160/RFC 6121 gate in the normal initial-presence
        // flush: a negative-priority resource is available for presence, but
        // must not receive bare-JID/offline delivery.
        .filter(|(_, priority)| *priority >= 0)
        .collect::<Vec<_>>();
    resources.sort_by(|(left_jid, left_priority), (right_jid, right_priority)| {
        right_priority
            .cmp(left_priority)
            .then_with(|| left_jid.to_string().cmp(&right_jid.to_string()))
    });
    resources.into_iter().find_map(|(resource, _)| {
        state
            .deps
            .protocol
            .connection_registry
            .get_entry(&resource)
            .and_then(|entry| {
                // The availability snapshot above and this entry lookup are
                // two reads: the resource can go unavailable/negative or a
                // silent same-FullJID replacement can register in between.
                // Revalidate on the entry actually adopted, otherwise the
                // flush would owner-gate successfully against a resource
                // that RFC 6121/XEP-0160 exclude from offline delivery.
                if !entry.is_presence_available() || entry.presence_priority() < 0 {
                    return None;
                }
                Some(LivePendingFlushTarget {
                    resource,
                    owner: std::sync::Arc::clone(&entry.carbons_enabled),
                    sm_session: entry.sm_stream_id(),
                })
            })
    })
}

/// Terminal recovery can release sequence-bound `pending_delivery` rows back
/// to bare-JID storage — and promotion can insert fresh rows — after a newer
/// live resource has already spent its own once-per-session offline-flush
/// claim. Re-drive those rows directly to the current best live resource here
/// so they do not wait for a future presence update that may never arrive.
///
/// This intentionally does NOT touch RFC 6121 pending-subscribe delivery. That
/// queue is bare-JID scoped and persists until approval/denial; fresh sessions
/// already deliver it on their own initial available presence, and live
/// subscription traffic bypasses this cleanup path entirely.
/// Result of a terminal re-drive attempt, so callers can keep FIFO: while an
/// earlier released/queued row could NOT be enqueued to a live target, later
/// recovery traffic must not be promoted directly to that same target.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TerminalRedriveOutcome {
    /// Rows were flushed (or none remained) — later promotion may proceed.
    Settled,
    /// No eligible live resource exists; later promotion also lands in
    /// durable storage behind the earlier rows, preserving order.
    NoLiveTarget,
    /// A live target exists but unclaimed rows remain (flush aborted or
    /// deferred) — promoting later traffic directly would overtake them.
    Aborted,
}

pub(crate) async fn redrive_terminal_pending_rows_to_live_resource(
    state: &WebSocketState,
    recipient: &BareJid,
) -> TerminalRedriveOutcome {
    let blocking_storage: &std::sync::Arc<dyn waddle_xmpp::xep::xep0191::BlockingStorage> =
        &state.deps.protocol.blocking_storage;
    // A same-FullJID replacement can race the owner-gated send below. Retry
    // one fresh target selection in that case: the successor may already have
    // consumed its initial offline-flush CAS, which is exactly why terminal
    // recovery drives this path directly. The bound prevents cleanup from
    // chasing an unbounded stream of reconnects.
    let mut last_attempt_left_rows = false;
    for _ in 0..2 {
        let Some(target) = preferred_live_pending_flush_target(state, recipient) else {
            return TerminalRedriveOutcome::NoLiveTarget;
        };
        let resolver = crate::pending_delivery::MamArchiveResolver {
            mam_storage: std::sync::Arc::clone(&state.deps.protocol.mam_storage),
        };
        let outcome = crate::pending_delivery::flush_for_resource(
            &state.deps.protocol.pending_delivery_storage,
            &state.deps.protocol.connection_registry,
            recipient,
            &target.resource,
            crate::pending_delivery::FlushContext {
                server_domain: state.deps.auth_state.xmpp_domain.as_str(),
                sm_session: target.sm_session.as_ref(),
                blocking_storage: Some(blocking_storage),
                owner: Some(&target.owner),
                archive_resolver: &resolver,
            },
        )
        .await;
        let target_still_current = state
            .deps
            .protocol
            .connection_registry
            .entry_if_owner(&target.resource, &target.owner)
            .is_some();
        if !target_still_current && (outcome.claimed > 0 || outcome.pushed > 0) {
            // Rows were claimed/pushed into a session that has since been
            // superseded: they sit in that session's channel until ITS
            // cleanup releases and re-drives them. Reporting Settled here
            // would let the caller promote later traffic to the successor
            // ahead of those rows — treat the attempt as aborted instead.
            return TerminalRedriveOutcome::Aborted;
        }
        last_attempt_left_rows = terminal_reflush_left_retryable_rows(state, recipient).await;
        if target_still_current && last_attempt_left_rows {
            if let Some(entry) = state
                .deps
                .protocol
                .connection_registry
                .entry_if_owner(&target.resource, &target.owner)
            {
                entry.reset_offline_flush();
            }
        }
        if outcome.claimed > 0 {
            debug!(
                recipient = %recipient,
                resource = %target.resource,
                claimed = outcome.claimed,
                pushed = outcome.pushed,
                deferred_transient = outcome.deferred_transient,
                "terminal cleanup re-drove pending_delivery rows onto a live resource"
            );
        }
        if target_still_current {
            return if last_attempt_left_rows {
                TerminalRedriveOutcome::Aborted
            } else {
                TerminalRedriveOutcome::Settled
            };
        }
    }
    if last_attempt_left_rows {
        TerminalRedriveOutcome::Aborted
    } else {
        TerminalRedriveOutcome::Settled
    }
}

/// Terminal reflush must re-open the replacement session's once-only offline
/// flush gate whenever retryable rows remain unclaimed. This catches not only
/// transient archive failures (`deferred_transient > 0`) but also early aborts
/// before a row is ever claimed (e.g. blocklist/claim-storage failures), which
/// otherwise leave a live replacement with a spent CAS and no janitor that can
/// flush the bare-JID backlog.
async fn terminal_reflush_left_retryable_rows(state: &WebSocketState, recipient: &BareJid) -> bool {
    match state
        .deps
        .protocol
        .pending_delivery_storage
        .list(recipient)
        .await
    {
        Ok(rows) => rows.iter().any(|row| row.flushed_in_session.is_none()),
        Err(error) => {
            warn!(
                recipient = %recipient,
                error = %error,
                "terminal cleanup could not inspect pending_delivery after live reflush; rearming replacement flush conservatively"
            );
            true
        }
    }
}

/// Build the detached payload needed solely for terminal recovery when the
/// connection registry slot may already belong to a same-JID replacement.
/// These fields are never made resumable: promotion uses the typed JID and
/// unacked queue, while the replacement retains all registry/MUC ownership.
fn terminal_recovery_snapshot(
    jid: &FullJid,
    conn: &WsConnState,
) -> waddle_xmpp::stream_management::DetachedSessionSnapshot {
    waddle_xmpp::stream_management::DetachedSessionSnapshot {
        user_id: conn
            .authenticated_session
            .as_ref()
            .map(|session| session.user_jid.to_string())
            .unwrap_or_else(|| jid.to_bare().to_string()),
        jid: jid.clone(),
        carbons_enabled: conn.carbons_enabled,
        roster_interested: conn.roster_interested,
        blocklist_interested: conn.blocklist_interested,
        presence_available: conn.presence_available,
        presence_show: conn.presence_show.clone(),
        presence_status: conn.presence_status.clone(),
        presence_priority: conn.presence_priority,
        presence_payloads: conn.presence_payloads.clone(),
        pending_subscribes_flushed: conn.pending_subscribes_flushed,
    }
}

/// Combine replies recorded after cap exhaustion with the accepted outbound
/// receiver backlog into one synthetic detached session. Both buffers are
/// promoted rather than made resumable; terminal recovery itself is bounded,
/// keeping the recorded prefix and dropping later replayable stanzas once the
/// terminal cap is full. Keeping one session per stream preserves the
/// promotion helper's retry identity invariant.
async fn terminal_recovery_session(
    state: &WebSocketState,
    outbound_rx: &mut mpsc::Receiver<OutboundStanza>,
    conn: &mut WsConnState,
    mut detached: waddle_xmpp::stream_management::DetachedSession,
    detached_snapshot: waddle_xmpp::stream_management::DetachedSessionSnapshot,
    can_promote_incrementally: bool,
) -> TerminalRecoverySessionResult {
    outbound_rx.close();
    conn.ensure_terminal_sm_recovery();
    let blocklist = match super::replay::load_terminal_recovery_blocklist(
        state,
        &detached.jid.to_bare(),
    )
    .await
    {
        Ok(blocklist) => Some(blocklist),
        Err(error) => {
            warn!(
                stream_id = %detached.stream_id,
                jid = %detached.jid,
                %error,
                "terminal recovery could not load blocklist; retaining terminal overflow for retry"
            );
            None
        }
    };
    let recent_tombstones = crate::sm_promotion::recent_tombstones_for_promotion(
        &state.deps.protocol.sm_session_registry,
        "terminal recovery overflow promotion",
    )
    .unwrap_or_default();
    append_terminal_recovery_backlog(state, conn, &mut detached, detached_snapshot).await;
    // Preserve offline FIFO ordering: the live/unacked prefix must reach its
    // Q6 sink before any subsequently accepted channel frame. Once that
    // prefix is settled, channel work is promoted one frame at a time rather
    // than being allowed to grow another terminal replay queue.
    let (promote_incrementally, mut queued_pending_rows, prefix_redrive_aborted) =
        match (can_promote_incrementally, blocklist.as_ref()) {
            (false, _) => (false, false, false),
            (true, Some(blocklist)) => {
                let prefix = promote_terminal_recovery_prefix(
                    state,
                    &mut detached,
                    blocklist,
                    &recent_tombstones,
                )
                .await;
                (prefix.settled, prefix.queued_rows, prefix.redrive_aborted)
            }
            (true, None) => (false, false, false),
        };
    // Per-item re-drives inside the prefix promotion already covered freshly
    // queued rows (and halted the prefix on an aborted re-drive, which also
    // clears `promote_incrementally` via `settled`).
    let drain_outcome = drain_outbound_into_terminal_recovery(
        state,
        conn,
        outbound_rx,
        PendingRowDrainPolicy::ReleaseForTerminalRecovery,
        TerminalDrainContext {
            session: &detached,
            blocklist: blocklist.as_ref(),
            recent_tombstones: &recent_tombstones,
            promote_incrementally,
        },
    )
    .await;
    let retained_len = drain_outcome.retained_overflow.len();
    for (offset, mut stanza) in drain_outcome.retained_overflow.into_iter().enumerate() {
        stanza.sequence = detached
            .outbound_count
            .wrapping_add(u32::try_from(offset + 1).unwrap_or(u32::MAX));
        persist_terminal_suffix_entry(state, &detached.stream_id, &stanza).await;
        detached.unacked_stanzas.push(stanza);
    }
    detached.outbound_count = detached
        .outbound_count
        .wrapping_add(u32::try_from(retained_len).unwrap_or(u32::MAX));
    queued_pending_rows |= drain_outcome.queued_pending_rows;
    TerminalRecoverySessionResult {
        session: detached,
        released_pending_rows: drain_outcome.released_pending_rows,
        queued_pending_rows,
        release_failed_pending_rows: drain_outcome.release_failed_pending_rows,
        prefix_redrive_aborted: prefix_redrive_aborted || drain_outcome.redrive_aborted,
    }
}

/// Persist a synthetic terminal-recovery entry into the stream's durable
/// `sm_unacked` rows. Retry retention goes through
/// `reinsert_for_retry`, whose reconciliation keeps only sequences present
/// durably whenever the durable session row exists — without this write an
/// ownership/blocklist/storage failure would silently drop exactly the
/// terminal backlog and accepted channel tail before the janitor's retry.
async fn persist_terminal_suffix_entry(
    state: &WebSocketState,
    stream_id: &str,
    stanza: &waddle_xmpp::stream_management::DetachedUnackedStanza,
) {
    if let Err(error) = state
        .deps
        .protocol
        .sm_session_registry
        .record_outbound_for_detached_stream_at(
            stream_id,
            stanza.sequence,
            stanza.stanza_xml.clone(),
            stanza.original_receipt_at,
        )
        .await
    {
        warn!(
            stream_id = %stream_id,
            sequence = stanza.sequence,
            %error,
            "terminal recovery could not persist a synthetic suffix entry; a retry \
             after reinsertion may drop it during durable reconciliation"
        );
    }
}

async fn append_terminal_recovery_backlog(
    state: &WebSocketState,
    conn: &mut WsConnState,
    detached: &mut waddle_xmpp::stream_management::DetachedSession,
    detached_snapshot: waddle_xmpp::stream_management::DetachedSessionSnapshot,
) {
    if let Some(backlog) = conn
        .terminal_sm_recovery
        .to_detached_session(detached_snapshot)
    {
        let backlog_len = backlog.unacked_stanzas.len();
        for (offset, mut stanza) in backlog.unacked_stanzas.into_iter().enumerate() {
            stanza.sequence = detached
                .outbound_count
                .wrapping_add(u32::try_from(offset + 1).unwrap_or(u32::MAX));
            persist_terminal_suffix_entry(state, &detached.stream_id, &stanza).await;
            detached.unacked_stanzas.push(stanza);
        }
        detached.outbound_count = detached
            .outbound_count
            .wrapping_add(u32::try_from(backlog_len).unwrap_or(u32::MAX));
    }
}

struct TerminalPrefixPromotion {
    /// The prefix settled completely; overflow may promote incrementally.
    settled: bool,
    /// The prefix inserted fresh `pending_delivery` rows whose
    /// online-resource snapshot may have missed a replacement binding
    /// concurrently — the caller must re-drive them before promoting
    /// later traffic.
    queued_rows: bool,
    /// A queued row's re-drive aborted mid-prefix: the retained remainder
    /// must not be promoted at all (not even by the final displaced
    /// promotion) until the stuck row settles via the janitor.
    redrive_aborted: bool,
}

/// Promote the recorded prefix ONE entry at a time, stopping at the first
/// storage failure so every later entry stays queued BEHIND the failed one —
/// batch promotion retained only the failed entry, and the queued-row
/// re-drive then delivered a later success before the retry could re-promote
/// the earlier failure, inverting the stream's accepted FIFO order. Per-item
/// promotion also re-checks tombstones per entry and re-drives each freshly
/// queued row before the next entry can be delivered live, mirroring the
/// overflow drain's contract.
async fn promote_terminal_recovery_prefix(
    state: &WebSocketState,
    detached: &mut waddle_xmpp::stream_management::DetachedSession,
    _blocklist: &waddle_xmpp::protocol::session_state::Blocklist,
    recent_tombstones: &[waddle_xmpp::stream_management::RecentTombstoneRecord],
) -> TerminalPrefixPromotion {
    if detached.unacked_stanzas.is_empty() {
        return TerminalPrefixPromotion {
            settled: true,
            queued_rows: false,
            redrive_aborted: false,
        };
    }
    let entries = std::mem::take(&mut detached.unacked_stanzas);
    let mut retained = Vec::new();
    let mut queued_rows = false;
    let mut halted = false;
    let mut redrive_aborted = false;
    for entry in entries {
        if halted {
            retained.push(entry);
            continue;
        }
        let blocklist =
            match super::replay::load_terminal_recovery_blocklist(state, &detached.jid.to_bare())
                .await
            {
                Ok(blocklist) => blocklist,
                Err(error) => {
                    warn!(
                        stream_id = %detached.stream_id,
                        jid = %detached.jid,
                        %error,
                        "terminal recovery prefix could not refresh blocklist; retaining the \
                         remaining prefix for retry"
                    );
                    retained.push(entry);
                    halted = true;
                    continue;
                }
            };
        let item_tombstones = crate::sm_promotion::recent_tombstones_for_promotion(
            &state.deps.protocol.sm_session_registry,
            "terminal recovery prefix promotion",
        )
        .unwrap_or_else(|_| recent_tombstones.to_vec());
        let summary = crate::sm_promotion::promote_terminal_overflow_entry(
            detached,
            entry.clone(),
            crate::sm_promotion::TerminalOverflowPromotionDeps {
                registry: &state.deps.protocol.connection_registry,
                user_registry: &state.deps.protocol.user_registry,
                pending_storage: &state.deps.protocol.pending_delivery_storage,
                blocklist: &blocklist,
                server_domain: state.deps.auth_state.xmpp_domain.as_str(),
                recent_tombstones: &item_tombstones,
            },
        )
        .await;
        crate::sm_promotion::scrub_pending_for_tombstones_recorded_during_promotion(
            &state.deps.protocol.sm_session_registry,
            &state.deps.protocol.pending_delivery_storage,
            &item_tombstones,
            "terminal recovery prefix promotion",
        )
        .await;
        if summary.has_storage_failure() {
            retained.push(entry);
            halted = true;
            continue;
        }
        if summary.queued > 0 {
            queued_rows = true;
            if redrive_terminal_pending_rows_to_live_resource(state, &detached.jid.to_bare()).await
                == TerminalRedriveOutcome::Aborted
            {
                // The queued row could not reach the live target; promoting
                // later entries directly would overtake it — including via
                // the final displaced promotion, so the abort propagates.
                halted = true;
                redrive_aborted = true;
            }
        }
    }
    detached.unacked_stanzas = retained;
    TerminalPrefixPromotion {
        settled: !halted && detached.unacked_stanzas.is_empty(),
        queued_rows,
        redrive_aborted,
    }
}

fn force_detach_requires_actor_unregister(
    origin: waddle_xmpp::registry::ForceDetachOrigin,
) -> bool {
    matches!(
        origin,
        waddle_xmpp::registry::ForceDetachOrigin::CrossNodeResume
    )
}

#[cfg(test)]
mod force_detach_tests {
    use super::*;

    #[test]
    fn only_cross_node_resume_reenters_the_registry() {
        assert!(!force_detach_requires_actor_unregister(
            waddle_xmpp::registry::ForceDetachOrigin::RegistryStaleActorRetirement
        ));
        assert!(!force_detach_requires_actor_unregister(
            waddle_xmpp::registry::ForceDetachOrigin::OwnerManagedRetirement
        ));
        assert!(force_detach_requires_actor_unregister(
            waddle_xmpp::registry::ForceDetachOrigin::CrossNodeResume
        ));
    }
}

/// A session that enabled resumable SM but cannot prove a durable resume
/// identity (pre-v11 row, or no authenticated session) still owns two
/// things that must not vanish with it: the `<enabled/>` path published a
/// durable ClaimStore claim for its stream id, and `to_detached_session`
/// already captured its unacked server stanzas. Move the claim into
/// terminal-release inventory and run the captured queues through the
/// XEP-0198 §5 promote → confirm chain before falling back to ordinary
/// non-resumable cleanup.
async fn refuse_detach_without_principal(
    state: &WebSocketState,
    outbound_rx: &mut mpsc::Receiver<OutboundStanza>,
    jid: &FullJid,
    conn: &mut WsConnState,
    detached_sessions: Vec<waddle_xmpp::stream_management::DetachedSession>,
    row_recovery: TerminalRowRecovery,
    terminal_route_removal: TerminalRouteRemoval,
) -> ConnectionShutdownOutcome {
    let _ = forget_terminal_shadow_stream_and_wait(state, &conn.sm_state).await;
    // A terminal session must disappear from the exact-FullJID routing table
    // before promotion. Otherwise `send_to` can successfully target this
    // closed channel and drop a <no-store/> stanza instead of taking the
    // pending-delivery fallback. Owner gating leaves a replacement untouched.
    let terminal_removed = match terminal_route_removal {
        TerminalRouteRemoval::NotAttempted => conn.registry_owner.as_ref().and_then(|owner| {
            conn.sm_recovery_required.then(|| {
                state
                    .deps
                    .protocol
                    .connection_registry
                    .unregister_if_owner(jid, owner)
                    .map(|entry| (entry.is_presence_available(), std::sync::Arc::clone(owner)))
            })?
        }),
        TerminalRouteRemoval::Attempted(removed) => removed,
    };
    let pre_promotion_redrive_aborted = if row_recovery.released_rows {
        // Preserve FIFO when terminal recovery released an earlier
        // pending_delivery row but still has later unacked traffic to
        // promote: drive the released prefix back onto any live replacement
        // before promotion can enqueue the tail. An abort means the earlier
        // row is still pending at a live replacement — promoting the tail
        // now would overtake it, so the session defers to the janitor.
        redrive_terminal_pending_rows_to_live_resource(state, &jid.to_bare()).await
            == TerminalRedriveOutcome::Aborted
    } else {
        false
    };
    // Keep this stream's claim live throughout every promotion. The claim
    // janitor uses SM liveness/fences, and deferring it before the locally
    // held batch is registered would let a sweep release the fence midway.
    let (retrying, promotion_queued_rows) = if detached_sessions.is_empty() {
        (false, false)
    } else if row_recovery.ownership_unknown
        || row_recovery.redrive_aborted
        || pre_promotion_redrive_aborted
    {
        // Two deferral reasons share this arm: row ownership could not be
        // discovered (promoting could duplicate still-claimed durable rows),
        // or an earlier released row is stuck at a live replacement
        // (promoting the tail would overtake it). Keep the whole queue out
        // of promotion and hand it to the SM-expiry janitor, whose retry
        // pass re-runs discovery and the re-drive before promoting.
        let mut conn_stream_retrying = false;
        for session in detached_sessions {
            let is_conn_stream = conn
                .sm_state
                .stream_id
                .as_deref()
                .is_some_and(|stream_id| stream_id == session.stream_id);
            if crate::sm_promotion::reinsert_failed_session_for_retry(
                &state.deps.protocol.sm_session_registry,
                session,
            )
            .await
                && is_conn_stream
            {
                conn_stream_retrying = true;
            }
        }
        (conn_stream_retrying, false)
    } else {
        let outcome = crate::sm_promotion::promote_displaced_sessions(
            detached_sessions,
            crate::sm_promotion::DisplacedPromotionDeps {
                sm_registry: &state.deps.protocol.sm_session_registry,
                connection_registry: &state.deps.protocol.connection_registry,
                user_registry: &state.deps.protocol.user_registry,
                pending_storage: &state.deps.protocol.pending_delivery_storage,
                blocking_storage: state.deps.protocol.blocking_storage.as_ref(),
                server_domain: state.deps.auth_state.xmpp_domain.as_str(),
            },
        )
        .await;
        for completed in outcome.completed_stream_ids() {
            state
                .deps
                .protocol
                .ingress_shadow
                .forget_stream(&waddle_xmpp::pending_delivery::SmSessionId::new(completed));
        }
        (
            conn.sm_state
                .stream_id
                .as_deref()
                .is_some_and(|stream_id| outcome.is_retrying(stream_id)),
            outcome.queued_pending_rows(),
        )
    };
    if row_recovery.queued_pending_rows || promotion_queued_rows {
        // Rows promotion freshly queued while a replacement bound mid-await
        // (spending its once-only offline flush) are already durable and are
        // NOT re-reported by a promotion retry — re-drive them regardless of
        // retry status, before a later retry can deliver the remaining tail
        // ahead of them.
        redrive_terminal_pending_rows_to_live_resource(state, &jid.to_bare()).await;
    }
    if !retrying {
        super::stream_management::defer_superseded_sm_claim(state, &conn.sm_state);
        if row_recovery.release_failed_known_rows {
            // Known row-backed replay copies were stripped while their
            // release failed; the settled promotion's `release_claim` has
            // since freed those rows and nothing else re-drives them.
            redrive_terminal_pending_rows_to_live_resource(state, &jid.to_bare()).await;
        }
    }
    conn.sm_state = waddle_xmpp::stream_management::StreamManagementState::new();
    if let Some((was_presence_available, owner)) = terminal_removed {
        // The promotion await above can overlap a same-FullJID replacement
        // binding and rejoining rooms. At this point the old route is already
        // gone, so any live registry entry is necessarily that replacement and
        // owns the FullJID-keyed resources below.
        let replacement_took_over = state
            .deps
            .protocol
            .connection_registry
            .get_entry(jid)
            .is_some();
        if !replacement_took_over {
            let cleanup_origin = clustered_cleanup_origin(state, jid, &owner).await;
            state.deps.protocol.caps_resolver.drop_resource(jid);
            cleanup_muc_presence_with_origin(
                state,
                jid,
                cleanup_origin.as_ref(),
                waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
                SweepFailureRecording::RecordSweep,
            )
            .await;
            unregister_remote_user_resource_if_owner(state, jid, &owner).await;
        } else {
            debug!(
                jid = %jid,
                "Skipped terminal shared-resource cleanup: a replacement connection retook the FullJid during promotion"
            );
        }
        // Replacement ownership protects FullJID-scoped teardown, but does
        // not mean that the replacement has advertised presence. The helper
        // performs the required presence-aware suppression check.
        broadcast_unavailable_if_no_replacement(state, jid, was_presence_available).await;
        crate::server::dual_registration::mirror_unregister(
            &state.deps.protocol.user_registry,
            jid,
            Some(std::sync::Arc::clone(&owner)),
        )
        .await;
        outbound_rx.close();
        return ConnectionShutdownOutcome::NotPersisted;
    }
    conn.sm_recovery_required = false;
    cleanup_without_resumable_snapshot(state, outbound_rx, jid, conn).await
}

async fn cleanup_without_resumable_snapshot(
    state: &WebSocketState,
    outbound_rx: &mut mpsc::Receiver<OutboundStanza>,
    jid: &FullJid,
    conn: &mut WsConnState,
) -> ConnectionShutdownOutcome {
    conn.phase = ConnectionPhase::closing(Some(jid.clone()));
    Box::pin(cleanup_connection_shutdown(state, outbound_rx, conn, false)).await
}

/// RFC 6121 §4.5.2: an ungraceful session end (connection drop with no
/// self-sent unavailable) requires the SERVER to broadcast `<presence
/// type='unavailable'/>` from this full JID to the user's presence
/// subscribers (#1105).
///
/// Gated twice. `was_presence_available` comes from the entry the caller
/// just owned and removed — a bound-but-silent resource advertised
/// nothing, so there is nothing to retract. And a fresh registry lookup
/// must find no PRESENCE-AVAILABLE replacement for this full JID: the
/// owner-gated unregister only protects the map slot, not ordering
/// against a replacement's presence — broadcasting after the newcomer's
/// available would leave subscribers on a stale unavailable for a JID
/// that is online (round-2 concurrency review). A replacement that is
/// merely REGISTERED has broadcast nothing yet, so suppression would be
/// wrong there: if it never sends presence, subscribers keep the
/// dropped session's stale available forever. Broadcasting is correct
/// in every interleaving of that case — the replacement's future
/// available lands after our unavailable and wins (round-3 review).
pub(crate) async fn broadcast_unavailable_if_no_replacement(
    state: &WebSocketState,
    jid: &FullJid,
    was_presence_available: bool,
) -> handlers::presence::TerminatedPresenceBroadcastOutcome {
    if !was_presence_available {
        return handlers::presence::TerminatedPresenceBroadcastOutcome::Completed;
    }
    if state
        .deps
        .protocol
        .connection_registry
        .get_entry(jid)
        .is_some_and(|entry| entry.is_presence_available())
    {
        debug!(
            jid = %jid,
            "Suppressed terminated-session unavailable: an available replacement connection is live"
        );
        return handlers::presence::TerminatedPresenceBroadcastOutcome::Completed;
    }
    handlers::presence::broadcast_unavailable_for_terminated_session(state, jid).await
}

async fn cleanup_muc_presence(state: &WebSocketState, jid: &FullJid) -> bool {
    cleanup_muc_presence_with_origin(
        state,
        jid,
        None,
        waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
        SweepFailureRecording::RecordSweep,
    )
    .await
}

/// Whether an enumeration/lookup failure records a `FullJidSweep`. The
/// janitor's redrive passes `JanitorRequeues`: it already holds the drained
/// sweep and requeues it WITH BACKOFF on failure — re-recording from inside
/// the redrive would merge an immediate deadline over that backoff and spin
/// the sweep on every janitor tick during a registry outage (#1647).
#[derive(Clone, Copy, PartialEq, Eq)]
enum SweepFailureRecording {
    /// A fresh cleanup pass: mint the remote-membership ceiling now and
    /// record a `FullJidSweep` carrying it on enumeration/lookup failure.
    RecordSweep,
    /// The janitor's redrive of a retained sweep: reuse the ORIGINAL pass's
    /// remote ceiling and let the janitor requeue on failure.
    JanitorRequeues { remote_ceiling: u64 },
}

async fn cleanup_muc_presence_with_origin(
    state: &WebSocketState,
    jid: &FullJid,
    origin: Option<&crate::server::routes::interpret::OrderedRelayRouteOrigin>,
    sweep_attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId,
    sweep_recording: SweepFailureRecording,
) -> bool {
    let remote_ceiling = match sweep_recording {
        SweepFailureRecording::RecordSweep => state
            .deps
            .protocol
            .remote_muc_memberships
            .generation_watermark(),
        SweepFailureRecording::JanitorRequeues { remote_ceiling } => remote_ceiling,
    };
    let mut completed = cleanup_remote_muc_presence(state, jid, origin, remote_ceiling).await;

    let room_jids = match RoomRegistry::wrap(state.deps.protocol.room_registry.clone())
        .list_rooms()
        .await
    {
        Ok(room_jids) => room_jids,
        Err(error) => {
            completed = false;
            warn!(error = %error, "Failed to list room actors");
            if sweep_recording == SweepFailureRecording::RecordSweep {
                state.deps.protocol.pending_local_muc_departures.record(
                    LocalDepartureItem::FullJidSweep {
                        jid: jid.clone(),
                        attempt: sweep_attempt,
                        remote_ceiling,
                    },
                );
            }
            Vec::new()
        }
    };
    for room_jid in room_jids {
        let room_actor = match get_room_actor_result(state, &room_jid).await {
            Ok(Some(room_actor)) => room_actor,
            Ok(None) => continue,
            Err(error) => {
                completed = false;
                warn!(room = %room_jid, error = %error, "Failed to get room actor");
                if sweep_recording == SweepFailureRecording::RecordSweep {
                    state.deps.protocol.pending_local_muc_departures.record(
                        LocalDepartureItem::FullJidSweep {
                            jid: jid.clone(),
                            attempt: sweep_attempt,
                            remote_ceiling,
                        },
                    );
                }
                continue;
            }
        };
        // Bounded: one wedged room (stuck projection transaction or mailbox)
        // must not stall the sweep of every other room this JID occupies.
        // Every room shares the sweep's attempt: it was minted when THIS
        // cleanup pass (or the original one, on a redrive) started, so a
        // session that (re)joined since is `Superseded` by the order fence.
        let attempt = sweep_attempt;
        let in_flight = LocalDepartureItem::InFlight {
            room: room_jid.clone(),
            jid: jid.clone(),
            cause: OccupancyLeaveCause::Disconnect,
            attempt,
            notified: HashSet::new(),
        };
        state
            .deps
            .protocol
            .pending_local_muc_departures
            .record_in_flight(in_flight.clone());
        let _in_flight_lease = InFlightLease::hold(
            Arc::clone(&state.deps.protocol.pending_local_muc_departures),
            in_flight.clone(),
        );
        let leave_result = ask_leave_bounded(
            &room_actor,
            LeaveByRealJid {
                sender_jid: jid.clone(),
                cause: OccupancyLeaveCause::Disconnect,
                session: LeaveSessionSelector::Any,
                attempt,
                origin: waddle_xmpp::muc::room_actor::LeaveOrigin::Fresh,
            },
        )
        .await;
        // SFU teardown runs *after* the room actor has dropped the
        // session so the MUC's view of the world is the leading edge
        // (the membership gate immediately reports the user as a
        // non-occupant) and the SFU's view is the trailing edge —
        // matches `handle_muc_leave`'s "XMPP says they left, then
        // notify SFU" semantics. Tab close / SM-expiry: the client
        // never sends a graceful `request-leave` on
        // `urn:waddle:muc-call:0`, so without this call the SFU
        // would otherwise hold the participant slot until its own
        // (long) timeout. Idempotent on the SFU side — calling for
        // rooms where the user was never in a call is a no-op.
        // SFU teardown is keyed by (room, full JID) and idempotent: the
        // connection is physically gone, so it runs on the first attempt
        // regardless of how the room answered (today's semantics; #1703 tracks
        // the generation-safe variant).
        super::muc_call_sfu::unregister_participant_from_room(state, &room_jid, jid);
        match leave_result {
            Ok(LeaveDisposition::Left(outcome)) => {
                debug!(
                    room = %room_jid,
                    nick = %outcome.nick,
                    removed_last_session = outcome.removed_last_session,
                    "Removed user from MUC room on disconnect"
                );
                // Tell remaining occupants the user is gone. Without
                // this fan-out, a tab-close / SM-expiry / panic-shed
                // disconnect on a participant who had advertised the
                // `<call xmlns='urn:waddle:muc-call:0'/>` extension
                // leaves the "N in call" chip lit on every other
                // occupant's client. The explicit-leave path
                // (`handle_muc_leave`) calls the same helper, so the
                // wire shape is identical regardless of how the
                // session ended.
                broadcast_muc_leave_to_remaining_resumable(
                    state,
                    &room_jid,
                    jid,
                    &outcome,
                    Some(LeaveFanOutProgress {
                        skip: &NO_SKIP,
                        record: Some((
                            &state.deps.protocol.pending_local_muc_departures,
                            &in_flight,
                        )),
                    }),
                )
                .await;
                broadcast_muc_muji_clear_to_remaining_resumable(
                    state,
                    &room_jid,
                    jid,
                    &outcome,
                    Some(LeaveFanOutProgress {
                        skip: &NO_SKIP,
                        record: Some((
                            &state.deps.protocol.pending_local_muc_departures,
                            &in_flight,
                        )),
                    }),
                )
                .await;
                if !maybe_evict_empty_room(state, &room_jid, &outcome).await {
                    completed = false;
                }
                acknowledge_in_flight(
                    &state.deps.protocol.pending_local_muc_departures,
                    &room_actor,
                    &in_flight,
                    outcome.acknowledge,
                );
            }
            Ok(LeaveDisposition::Deferred { watermark }) => {
                completed = false;
                // Replace the write-ahead entry (selector `Any`) with the
                // watermark-bound deferral the actor answered with, so the
                // retry targets exactly the sessions that existed now.
                state
                    .deps
                    .protocol
                    .pending_local_muc_departures
                    .complete_in_flight(&in_flight);
                state.deps.protocol.pending_local_muc_departures.record(
                    LocalDepartureItem::RoomDeparture {
                        room: room_jid,
                        jid: jid.clone(),
                        cause: OccupancyLeaveCause::Disconnect,
                        selector: LeaveSessionSelector::JoinedAtOrBefore(watermark),
                        attempt,
                        notified: HashSet::new(),
                    },
                );
            }
            Ok(LeaveDisposition::Suppressed {
                attempt: acknowledge,
                ..
            }) => {
                acknowledge_in_flight(
                    &state.deps.protocol.pending_local_muc_departures,
                    &room_actor,
                    &in_flight,
                    acknowledge,
                );
            }
            Ok(LeaveDisposition::NotOccupant | LeaveDisposition::Superseded) => {
                state
                    .deps
                    .protocol
                    .pending_local_muc_departures
                    .complete_in_flight(&in_flight);
            }
            Err(LeaveAskFailure::Timeout) => {
                // Never enqueued, or enqueued with the reply lost: the retry
                // carries the same attempt id so a completed departure replays
                // its retained outcome instead of `NotOccupant`. A timeout
                // proves nothing about the seal: retain the departure itself,
                // not a retirement watch.
                completed = false;
                state
                    .deps
                    .protocol
                    .pending_local_muc_departures
                    .complete_in_flight(&in_flight);
                state.deps.protocol.pending_local_muc_departures.record(
                    LocalDepartureItem::RoomDeparture {
                        room: room_jid.clone(),
                        jid: jid.clone(),
                        cause: OccupancyLeaveCause::Disconnect,
                        selector: LeaveSessionSelector::Any,
                        attempt,
                        notified: HashSet::new(),
                    },
                );
                warn!(room = %room_jid, jid = %jid, "MUC leave ask timed out on disconnect; retained for retry");
            }
            Err(error @ (LeaveAskFailure::Handler(_) | LeaveAskFailure::Transport)) => {
                completed = false;
                state
                    .deps
                    .protocol
                    .pending_local_muc_departures
                    .complete_in_flight(&in_flight);
                state.deps.protocol.pending_local_muc_departures.record(
                    LocalDepartureItem::ConfirmRetired {
                        room: room_jid.clone(),
                        jid: jid.clone(),
                        actor: room_actor.id(),
                        cause: OccupancyLeaveCause::Disconnect,
                        selector: LeaveSessionSelector::Any,
                        attempt,
                        notified: HashSet::new(),
                    },
                );
                warn!(
                    room = %room_jid,
                    jid = %jid,
                    error = ?error,
                    "Failed to remove disconnected user from MUC room; waiting for actor retirement"
                );
            }
        }
    }
    completed
}

#[cfg(feature = "clustering")]
async fn cleanup_remote_muc_presence(
    state: &WebSocketState,
    jid: &FullJid,
    cleanup_origin: Option<&crate::server::routes::interpret::OrderedRelayRouteOrigin>,
    remote_ceiling: u64,
) -> bool {
    let memberships = state
        .deps
        .protocol
        .remote_muc_memberships
        .take_for_occupant_below(jid, remote_ceiling);
    if memberships.is_empty() {
        return true;
    }
    let Some(bridge) = state
        .deps
        .app_state
        .clustering_claims
        .ordered_relay_delivery_bridge
        .as_ref()
    else {
        for membership in &memberships {
            state
                .deps
                .protocol
                .remote_muc_memberships
                .restore_snapshot_if_current(membership);
        }
        return false;
    };
    let acquired_user_actor_origin = cleanup_origin.is_none();
    let origin = match cleanup_origin.cloned() {
        Some(origin) => origin,
        None => {
            let Some(origin) = acquire_remote_muc_cleanup_origin(state, jid).await else {
                for membership in &memberships {
                    state
                        .deps
                        .protocol
                        .remote_muc_memberships
                        .restore_snapshot_if_current(membership);
                }
                return false;
            };
            origin
        }
    };
    let mut completed = true;
    for membership in memberships {
        let room_jid = membership.room().clone();
        let nick = membership.nick().to_string();
        let Some(to) = room_jid
            .clone()
            .with_resource_str(&nick)
            .ok()
            .map(jid::Jid::from)
        else {
            completed = false;
            state
                .deps
                .protocol
                .remote_muc_memberships
                .restore_snapshot_if_current(&membership);
            continue;
        };
        let _remote_muc_membership_guard = state
            .deps
            .protocol
            .remote_muc_memberships
            .lock_snapshot(&membership)
            .await;
        if !state
            .deps
            .protocol
            .remote_muc_memberships
            .snapshot_is_current_tombstone(&membership)
        {
            debug!(
                room = %room_jid,
                nick = %nick,
                jid = %jid,
                "skipped stale remote MUC unavailable cleanup after newer membership generation"
            );
            continue;
        }
        super::muc_call_sfu::unregister_participant_from_room(state, &room_jid, jid);
        let mut presence =
            xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::Unavailable);
        presence.from = Some(jid::Jid::from(jid.clone()));
        presence.to = Some(to);
        let stanza = Stanza::Presence(presence);
        let decision = bridge
            .try_proxy_muc_remote_decision(
                &room_jid,
                &stanza,
                crate::clustering::ordered_relay::OrderedRelayMucProxyKind::OccupantPresence,
                &origin,
            )
            .await;
        match remote_muc_cleanup_disposition(&decision) {
            RemoteMucCleanupDisposition::Converged => {
                debug!(
                    room = %room_jid,
                    nick = %nick,
                    jid = %jid,
                    "remote MUC unavailable relayed; membership cleaned up"
                );
                state
                    .deps
                    .protocol
                    .remote_muc_memberships
                    .forget_snapshot_if_current(&membership);
            }
            // #1249: the previously-warned benign cases. A locally-owned
            // room claim means the local `LeaveByRealJid` loop that runs
            // right after this pass converges the occupancy; an unclaimed
            // room has no live RoomActor anywhere, so there is no remote
            // occupancy left to clean. Both forget the membership so the
            // recurring un-actionable warn is gone AND the entry stops
            // resurrecting.
            RemoteMucCleanupDisposition::NoRemoteOccupancy => {
                debug!(
                    room = %room_jid,
                    nick = %nick,
                    jid = %jid,
                    decision = ?decision,
                    "remote MUC membership has no remote occupancy (room local or unclaimed); \
                     local cleanup path is authoritative"
                );
                state
                    .deps
                    .protocol
                    .remote_muc_memberships
                    .forget_snapshot_if_current(&membership);
            }
            RemoteMucCleanupDisposition::UncertainCommit => {
                completed = false;
                debug!(
                    room = %room_jid,
                    nick = %nick,
                    jid = %jid,
                    decision = ?decision,
                    "remote MUC unavailable cleanup commit uncertain; keeping retry provenance"
                );
                state
                    .deps
                    .protocol
                    .remote_muc_memberships
                    .restore_snapshot_if_current(&membership);
            }
            // #1249: the harmful case. Restore the membership so the
            // reconciliation janitor re-drives the relay until the remote
            // node/claim recovers — the cleanup is now convergent instead
            // of one-shot.
            RemoteMucCleanupDisposition::RetryableFailure => {
                completed = false;
                // Log-level split (race review P2 on PR #1277):
                // `OriginUnavailable` is the EXPECTED steady state while
                // the user's other device holds the `UserActor` claim on
                // another node — the janitor re-drives every 30s and the
                // relay converges when that claim releases, so a warn per
                // attempt would just recreate the recurring-noise problem
                // this fix retires. Genuine relay failures stay at warn.
                if matches!(
                    decision,
                    crate::clustering::route_bridge::MucProxyRouteDecision::OriginUnavailable
                ) {
                    info!(
                        room = %room_jid,
                        nick = %nick,
                        jid = %jid,
                        "remote MUC unavailable cleanup deferred: origin claim held \
                         elsewhere; membership kept for janitor re-drive"
                    );
                } else {
                    warn!(
                        room = %room_jid,
                        nick = %nick,
                        jid = %jid,
                        decision = ?decision,
                        "failed to relay remote MUC unavailable during disconnect cleanup; \
                         membership kept for janitor re-drive"
                    );
                }
                state
                    .deps
                    .protocol
                    .remote_muc_memberships
                    .restore_snapshot_if_current(&membership);
            }
        }
    }
    if acquired_user_actor_origin {
        completed &= reap_remote_muc_cleanup_origin_if_empty(state, jid).await;
    }
    completed
}

/// How the disconnect-cleanup pass converges one remote MUC membership
/// after a relay decision (#1249).
#[cfg(feature = "clustering")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteMucCleanupDisposition {
    /// The unavailable was delivered remotely — forget the membership.
    Converged,
    /// No remote occupancy can exist (room claim locally owned, or no
    /// claim row at all) — forget the membership; the local room path
    /// is authoritative.
    NoRemoteOccupancy,
    /// The relay may have committed remotely; keep the membership so a
    /// re-drive can settle it, but don't warn — this is the expected
    /// ambiguous-network case.
    UncertainCommit,
    /// The relay definitively did not run (remote unreachable, claim
    /// lookup failure, origin claim held elsewhere). Keep the
    /// membership; the reconciliation janitor re-drives it.
    RetryableFailure,
}

#[cfg(feature = "clustering")]
fn remote_muc_cleanup_disposition(
    decision: &crate::clustering::route_bridge::MucProxyRouteDecision,
) -> RemoteMucCleanupDisposition {
    use crate::clustering::route_bridge::{MucProxyRouteDecision, OrderedRelayMucProxyOutcome};
    match decision {
        MucProxyRouteDecision::Attempted(attempt) => match &attempt.outcome {
            OrderedRelayMucProxyOutcome::Delivered(_) => RemoteMucCleanupDisposition::Converged,
            OrderedRelayMucProxyOutcome::MaybeCommitted
            | OrderedRelayMucProxyOutcome::JoinMaybeCommitted => {
                RemoteMucCleanupDisposition::UncertainCommit
            }
            OrderedRelayMucProxyOutcome::Unavailable | OrderedRelayMucProxyOutcome::Dropped => {
                RemoteMucCleanupDisposition::RetryableFailure
            }
        },
        MucProxyRouteDecision::RoomClaimUnavailable | MucProxyRouteDecision::OriginUnavailable => {
            RemoteMucCleanupDisposition::RetryableFailure
        }
        MucProxyRouteDecision::LocalRoom | MucProxyRouteDecision::RoomUnclaimed => {
            RemoteMucCleanupDisposition::NoRemoteOccupancy
        }
    }
}

#[cfg(feature = "clustering")]
fn clustered_user_actor_cleanup_origin(
    jid: &FullJid,
) -> Option<crate::server::routes::interpret::OrderedRelayRouteOrigin> {
    let entity = waddle_xmpp::ownership::Entity::new(
        waddle_xmpp::ownership::EntityType::UserActor,
        jid.to_bare().to_string(),
    );
    Some(crate::server::routes::interpret::OrderedRelayRouteOrigin {
        kind: crate::server::routes::interpret::OrderedRelayRouteOriginKind::Entity(entity.clone()),
        sender_entity: entity,
        inbound_sequence: 0,
        handoff: None,
    })
}

/// Preferred ordered-relay origin for a disconnecting session's MUC
/// cleanup (#1249). When this socket was registered as a REMOTE-owned
/// resource (its `UserActor` claim is held by the node hosting the
/// user's other device — the exact case that used to fail with
/// `failed to relay remote MUC unavailable during disconnect cleanup`),
/// the cleanup must relay through the remote-resource origin path: the
/// user-owning node holds the origin claim and forwards the unavailable
/// to the room owner. Falls back to the local `UserActor` entity origin
/// when the socket is not remote-registered.
#[cfg(feature = "clustering")]
async fn clustered_cleanup_origin(
    state: &WebSocketState,
    jid: &FullJid,
    owner: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Option<crate::server::routes::interpret::OrderedRelayRouteOrigin> {
    if let Some(bridge) = state
        .deps
        .app_state
        .clustering_claims
        .ordered_relay_delivery_bridge
        .as_ref()
    {
        if let Some(remote) = bridge.remote_resource_origin_if_owner(jid, owner).await {
            return Some(crate::server::routes::interpret::OrderedRelayRouteOrigin {
                kind: crate::server::routes::interpret::OrderedRelayRouteOriginKind::RemoteResource(
                    remote,
                ),
                sender_entity: waddle_xmpp::ownership::Entity::new(
                    waddle_xmpp::ownership::EntityType::UserActor,
                    jid.to_bare().to_string(),
                ),
                inbound_sequence: 0,
                handoff: None,
            });
        }
    }
    clustered_user_actor_cleanup_origin(jid)
}

#[cfg(not(feature = "clustering"))]
async fn clustered_cleanup_origin(
    _state: &WebSocketState,
    _jid: &FullJid,
    _owner: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Option<crate::server::routes::interpret::OrderedRelayRouteOrigin> {
    None
}

#[cfg(feature = "clustering")]
async fn acquire_remote_muc_cleanup_origin(
    state: &WebSocketState,
    jid: &FullJid,
) -> Option<crate::server::routes::interpret::OrderedRelayRouteOrigin> {
    let bare_jid = jid.to_bare();
    let entity = waddle_xmpp::ownership::Entity::new(
        waddle_xmpp::ownership::EntityType::UserActor,
        bare_jid.to_string(),
    );
    // A resumed resource may still be registered under the old node while
    // that node drains its force-detach cleanup.  Foreign ownership is a
    // legitimate transient state, not a terminal one-shot failure.
    for attempt in 0..3 {
        match state
            .deps
            .protocol
            .user_registry
            .ask(waddle_xmpp::registry::GetOrCreateUser {
                bare_jid: bare_jid.clone(),
            })
            .mailbox_timeout(std::time::Duration::from_secs(2))
            .reply_timeout(std::time::Duration::from_secs(2))
            .await
        {
            Ok(_) => {
                return Some(crate::server::routes::interpret::OrderedRelayRouteOrigin {
                    kind: crate::server::routes::interpret::OrderedRelayRouteOriginKind::Entity(
                        entity.clone(),
                    ),
                    sender_entity: entity,
                    inbound_sequence: 0,
                    handoff: None,
                });
            }
            Err(kameo::error::SendError::HandlerError(
                waddle_xmpp::registry::UserRegistryError::ClaimHeldByAnotherNode(_),
            )) if attempt < 2 => {
                tokio::time::sleep(std::time::Duration::from_millis(25 * (attempt + 1))).await;
            }
            Err(error) => {
                warn!(
                    jid = %jid,
                    error = ?error,
                    "failed to acquire UserActor claim for remote MUC cleanup"
                );
                return None;
            }
        }
    }
    None
}

#[cfg(feature = "clustering")]
async fn reap_remote_muc_cleanup_origin_if_empty(state: &WebSocketState, jid: &FullJid) -> bool {
    let bare_jid = jid.to_bare();
    match state
        .deps
        .protocol
        .user_registry
        .ask(waddle_xmpp::registry::ReapUserIfEmpty { bare_jid })
        .await
    {
        Ok(_) => true,
        Err(error) => {
            warn!(
                jid = %jid,
                error = ?error,
                "failed to reap empty UserActor after remote MUC cleanup"
            );
            false
        }
    }
}

#[cfg(not(feature = "clustering"))]
async fn cleanup_remote_muc_presence(
    _state: &WebSocketState,
    _jid: &FullJid,
    _cleanup_origin: Option<&crate::server::routes::interpret::OrderedRelayRouteOrigin>,
    _remote_ceiling: u64,
) -> bool {
    true
}

pub(super) async fn cleanup_invalidated_detached_session(
    state: &WebSocketState,
    detached: waddle_xmpp::stream_management::DetachedSession,
    replacement_owner: Option<&std::sync::Arc<std::sync::atomic::AtomicBool>>,
) {
    // Terminal invalidation: this stream can never resume, so its ingress-
    // shadow enrollment gate must not outlive it (a fresh bind mints a new
    // stream id, and the expiry janitor never sees an invalidated session).
    state.deps.protocol.ingress_shadow.forget_stream(
        &waddle_xmpp::pending_delivery::SmSessionId::new(detached.stream_id.clone()),
    );
    // `entry_if_owner` is a READ-ONLY ownership check — it does NOT remove the
    // DashMap entry. It gates whether we attempt cleanup at all: if the
    // replacement (the freshly-bound session that triggered this invalidation)
    // already owns the slot, there is nothing of the old session's to clean up.
    let replacement_is_current_owner = replacement_owner.is_some_and(|owner| {
        state
            .deps
            .protocol
            .connection_registry
            .entry_if_owner(&detached.jid, owner)
            .is_some()
    });
    if !replacement_is_current_owner {
        // Remove the DashMap entry ONLY if it is still this exact invalidated
        // session's, gated on its own SM stream id (Greptile P1 on PR #1177) —
        // NOT a plain `unregister`. A plain unregister removes whatever holds
        // the full JID, which can be an UNRELATED live session S3 that bound the
        // same resource concurrently; the mirror below (keyed on the removed
        // entry's token) would then evict S3 from the actor tree too, and under
        // Slice 1 that silently drops S3 from bare-JID routing on both paths.
        // The invalidated session was normally already pruned at detach, so this
        // returns `None` and the mirror is skipped; if it somehow lingered, only
        // its own entry (matching stream id) is removed.
        let removed_entry = state
            .deps
            .protocol
            .connection_registry
            .unregister_if_sm_stream_id(
                &detached.jid,
                &waddle_xmpp::pending_delivery::SmSessionId::new(detached.stream_id.clone()),
            );
        if let Some(entry) = removed_entry {
            crate::server::dual_registration::mirror_unregister(
                &state.deps.protocol.user_registry,
                &detached.jid,
                Some(std::sync::Arc::clone(&entry.carbons_enabled)),
            )
            .await;
            unregister_remote_user_resource_if_owner(state, &detached.jid, &entry.carbons_enabled)
                .await;
        }
        // XEP-0115 §6: clear the resource→ver mapping AND any stuck
        // pending disco#info resolution for this resource so an
        // unresumed detached session doesn't leak indefinitely.
        state
            .deps
            .protocol
            .caps_resolver
            .drop_resource(&detached.jid);
    }
    // Same rule as the unclean-disconnect path: the helper suppresses
    // the broadcast ONLY when the current registry entry is
    // presence-AVAILABLE. A replacement that merely OWNS the slot but
    // never sent presence has broadcast nothing yet — suppressing here
    // would pin subscribers on this detached session's stale available
    // forever if the replacement stays silent. So we pass the detached
    // session's own availability unchanged and let the helper decide.
    broadcast_unavailable_if_no_replacement(state, &detached.jid, detached.presence_available)
        .await;
    // MUC occupancy is keyed by FULL JID, so a live same-JID session
    // shares the room occupancies this stale detached session would
    // evict. Two cases (codex P1 on PR #1207):
    //  - The live entry IS the invalidating caller (fresh bind / failed
    //    resume registering right now): its new stream cannot have
    //    joined any rooms yet, so the occupancies are certainly the
    //    dead session's — clean them, or the fresh connection inherits
    //    room fan-out without ever joining.
    //  - A FOREIGN live entry (third-party replacement, janitor-driven
    //    invalidation): it may have legitimately re-joined rooms under
    //    the shared full JID — skip cleanup; its own disconnect path
    //    cleans up when it ends.
    let foreign_live_entry = !replacement_is_current_owner
        && state
            .deps
            .protocol
            .connection_registry
            .get_entry(&detached.jid)
            .is_some();
    if !foreign_live_entry {
        cleanup_muc_presence(state, &detached.jid).await;
    }
}

pub(crate) async fn get_room_actor(
    state: &WebSocketState,
    room_jid: &BareJid,
) -> Option<ActorRef<RoomActor>> {
    match get_room_actor_result(state, room_jid).await {
        Ok(actor) => actor,
        Err(error) => {
            warn!(room = %room_jid, error = %error, "Failed to get room actor");
            None
        }
    }
}

/// Typed room lookup for protocol paths where an unpublished restore is not
/// equivalent to an absent room. Callers must map reconciliation to a
/// wait-class response or coalesce through `GetOrCreateRoom`; they must never
/// run room-creation authorization based on that transient state.
pub(crate) async fn get_room_actor_result(
    state: &WebSocketState,
    room_jid: &BareJid,
) -> Result<Option<ActorRef<RoomActor>>, RoomRegistryError> {
    let registry = RoomRegistry::wrap(state.deps.protocol.room_registry.clone());
    registry.get_room(room_jid.clone()).await
}

/// Register owner-IQ cleanup before starting its destroy. If the registry
/// reply is lost after the durable commit, reconciliation carries this same
/// typed snapshot into [`drain_destroy_completions`].
pub(crate) async fn register_destroy_completion(
    state: &WebSocketState,
    completion: waddle_xmpp::muc::room_registry_actor::DestroyCompletion,
) -> Result<(), RoomRegistryError> {
    RoomRegistry::wrap(state.deps.protocol.room_registry.clone())
        .register_destroy_completion(completion)
        .await
}

/// Forget owner-IQ work after a destroy was conclusively refused before the
/// registry could attach it to a retained attempt.
pub(crate) async fn cancel_destroy_completion_attempt(
    state: &WebSocketState,
    attempt: waddle_xmpp::muc::DestroyAttemptId,
) {
    if let Err(error) = RoomRegistry::wrap(state.deps.protocol.room_registry.clone())
        .cancel_destroy_completion_attempt(attempt)
        .await
    {
        warn!(%error, "Failed to discard refused MUC destroy completion");
    }
}

/// Run every registry-completed owner destroy at the server boundary. A
/// normal owner IQ supplies its own session so its final presence can be
/// returned inline; reconciliation sends all presence through the connection
/// registry because the original frame already received a retryable error.
pub(crate) async fn drain_destroy_completions(
    state: &WebSocketState,
    inline_session: Option<&FullJid>,
) -> ResponseBatch {
    let completions = match RoomRegistry::wrap(state.deps.protocol.room_registry.clone())
        .take_destroy_completions()
        .await
    {
        Ok(completions) => completions,
        Err(error) => {
            warn!(error = %error, "Failed to drain completed MUC destroy work");
            return ResponseBatch::default();
        }
    };
    let mut batch = ResponseBatch::default();
    for completion in completions {
        let attempt = completion.attempt;
        #[cfg(feature = "clustering")]
        let clustered = state
            .deps
            .app_state
            .clustering_claims
            .muc_durable_store
            .is_some();
        #[cfg(not(feature = "clustering"))]
        let clustered = false;
        // A local completion is proof that the non-clustered registry
        // committed its destroy. Do not let the persisted janitor infer that
        // proof from an inert row: it cannot distinguish a committed destroy
        // from an in-flight or refused intent.
        if !clustered
            && !super::handlers::iq::muc_owner_moderation::arm_destroy_completion_recovery(
                state, attempt,
            )
            .await
        {
            if let Err(error) = RoomRegistry::wrap(state.deps.protocol.room_registry.clone())
                .requeue_destroy_completion(attempt)
                .await
            {
                warn!(%error, "Failed to retain MUC destroy completion after non-clustered arming failure");
            }
            continue;
        }
        let Some(lease) =
            super::handlers::iq::muc_owner_moderation::claim_persisted_destroy_completion(
                state, attempt,
            )
            .await
        else {
            match super::handlers::iq::muc_owner_moderation::persisted_destroy_completion_exists(
                state, attempt,
            )
            .await
            {
                Some(false) => {
                    if let Err(error) =
                        RoomRegistry::wrap(state.deps.protocol.room_registry.clone())
                            .ack_destroy_completion(attempt)
                            .await
                    {
                        warn!(%error, "Failed to acknowledge MUC destroy completion already finalized by another node");
                    }
                }
                Some(true) | None => {
                    if let Err(error) =
                        RoomRegistry::wrap(state.deps.protocol.room_registry.clone())
                            .requeue_destroy_completion(attempt)
                            .await
                    {
                        warn!(%error, "Failed to retain MUC destroy completion without a durable lease");
                    }
                }
            }
            continue;
        };
        match super::handlers::iq::muc_owner_moderation::complete_leased_destroy_completion(
            state,
            &lease,
            inline_session,
        )
        .await
        {
            Ok(completion_frames) => {
                let persisted_acknowledged = super::handlers::iq::muc_owner_moderation::finalize_persisted_destroy_completion(state, &lease, true).await;
                if persisted_acknowledged {
                    if let Err(error) =
                        RoomRegistry::wrap(state.deps.protocol.room_registry.clone())
                            .ack_destroy_completion(attempt)
                            .await
                    {
                        warn!(%error, "Failed to acknowledge completed MUC destroy cleanup");
                    }
                } else {
                    if let Err(error) =
                        RoomRegistry::wrap(state.deps.protocol.room_registry.clone())
                            .requeue_destroy_completion(attempt)
                            .await
                    {
                        warn!(%error, "Failed to retain MUC destroy completion after durable acknowledgement failure");
                    }
                }
                batch.append_batch(completion_frames);
            }
            Err(()) => {
                let _ = super::handlers::iq::muc_owner_moderation::finalize_persisted_destroy_completion(state, &lease, false).await;
                if let Err(error) = RoomRegistry::wrap(state.deps.protocol.room_registry.clone())
                    .requeue_destroy_completion(attempt)
                    .await
                {
                    warn!(%error, "Failed to requeue incomplete MUC destroy cleanup");
                }
            }
        }
    }
    super::handlers::iq::muc_owner_moderation::drain_persisted_destroy_completions(state).await;
    batch
}

pub(crate) async fn drain_destroy_completion_attempt(
    state: &WebSocketState,
    attempt: waddle_xmpp::muc::DestroyAttemptId,
    inline_session: Option<&FullJid>,
) -> Result<ResponseBatch, ()> {
    let local_completion = match RoomRegistry::wrap(state.deps.protocol.room_registry.clone())
        .take_destroy_completion_attempt(attempt)
        .await
    {
        Ok(completion) => completion,
        Err(error) => {
            warn!(
                attempt = %attempt.as_uuid(),
                error = %error,
                "Failed to lease exact completed MUC destroy work"
            );
            None
        }
    };
    if local_completion.is_some() {
        #[cfg(feature = "clustering")]
        let clustered = state
            .deps
            .app_state
            .clustering_claims
            .muc_durable_store
            .is_some();
        #[cfg(not(feature = "clustering"))]
        let clustered = false;
        if !clustered
            && !super::handlers::iq::muc_owner_moderation::arm_destroy_completion_recovery(
                state, attempt,
            )
            .await
        {
            if let Err(error) = RoomRegistry::wrap(state.deps.protocol.room_registry.clone())
                .requeue_destroy_completion(attempt)
                .await
            {
                warn!(%error, "Failed to retain exact MUC destroy completion after non-clustered arming failure");
            }
            return Err(());
        }
    }
    let Some(lease) =
        super::handlers::iq::muc_owner_moderation::claim_persisted_destroy_completion(
            state, attempt,
        )
        .await
    else {
        if local_completion.is_some() {
            match super::handlers::iq::muc_owner_moderation::persisted_destroy_completion_exists(
                state, attempt,
            )
            .await
            {
                Some(false) => {
                    if let Err(error) =
                        RoomRegistry::wrap(state.deps.protocol.room_registry.clone())
                            .ack_destroy_completion(attempt)
                            .await
                    {
                        warn!(%error, "Failed to acknowledge exact MUC destroy completion already finalized by another node");
                    }
                }
                Some(true) | None => {
                    if let Err(error) =
                        RoomRegistry::wrap(state.deps.protocol.room_registry.clone())
                            .requeue_destroy_completion(attempt)
                            .await
                    {
                        warn!(%error, "Failed to retain exact MUC destroy completion without a durable lease");
                    }
                }
            }
        }
        return Err(());
    };
    match super::handlers::iq::muc_owner_moderation::complete_leased_destroy_completion(
        state,
        &lease,
        inline_session,
    )
    .await
    {
        Ok(frames) => {
            let persisted_acknowledged =
                super::handlers::iq::muc_owner_moderation::finalize_persisted_destroy_completion(
                    state, &lease, true,
                )
                .await;
            if local_completion.is_some() {
                if persisted_acknowledged {
                    if let Err(error) =
                        RoomRegistry::wrap(state.deps.protocol.room_registry.clone())
                            .ack_destroy_completion(attempt)
                            .await
                    {
                        warn!(%error, "Failed to acknowledge exact completed MUC destroy cleanup");
                    }
                } else if let Err(error) =
                    RoomRegistry::wrap(state.deps.protocol.room_registry.clone())
                        .requeue_destroy_completion(attempt)
                        .await
                {
                    warn!(%error, "Failed to retain exact MUC destroy completion after durable acknowledgement failure");
                }
            }
            Ok(frames)
        }
        Err(()) => {
            let _ =
                super::handlers::iq::muc_owner_moderation::finalize_persisted_destroy_completion(
                    state, &lease, false,
                )
                .await;
            if local_completion.is_some() {
                if let Err(error) = RoomRegistry::wrap(state.deps.protocol.room_registry.clone())
                    .requeue_destroy_completion(attempt)
                    .await
                {
                    warn!(%error, "Failed to requeue exact incomplete MUC destroy cleanup");
                }
            }
            Err(())
        }
    }
}

/// Get or create the room via the registry. The returned
/// [`RoomAcquisition`] carries the registry-authoritative created-bit
/// (#1134): only the caller that observes `RoomCreation::Created`
/// actually created the room and may grant itself the XEP-0045
/// §10.1.1 creator Owner.
///
/// ADR-0017 Phase 3 Slice 7 FIX 6 (council-adjudicated): returns the
/// registry's typed `Err` rather than collapsing every failure (including
/// `RoomRegistryError::ClaimHeldByAnotherNode`) into `None` — a caller that
/// only sees `None` cannot distinguish "another node genuinely owns this
/// room right now" (a conformant, recoverable `<resource-constraint/>`
/// bounce per XEP-0045) from any other registry failure, and the MUC join
/// path's previous `let Some(actor) = ... else { return vec![] }` silently
/// dropped the join with NO presence reply at all in every such case.
pub(crate) async fn get_or_create_room_actor(
    state: &WebSocketState,
    room_jid: &BareJid,
    config: RoomConfig,
    waddle_id: String,
    channel_id: String,
) -> Result<RoomAcquisition, RoomRegistryError> {
    RoomRegistry::wrap(state.deps.protocol.room_registry.clone())
        .get_or_create_room(room_jid.clone(), waddle_id, channel_id, config)
        .await
        .inspect_err(|error| {
            if matches!(
                error,
                RoomRegistryError::ClaimHeldByAnotherNode(_)
                    | RoomRegistryError::OwnershipReconciliationPending(_)
            ) {
                debug!(
                    room = %room_jid,
                    %error,
                    "Room actor ownership is not locally available"
                );
            } else {
                warn!(room = %room_jid, %error, "Failed to get or create room actor");
            }
        })
}

pub(crate) async fn is_muc_room_jid(state: &WebSocketState, room_jid: &BareJid) -> bool {
    match RoomRegistry::wrap(state.deps.protocol.room_registry.clone())
        .is_muc_jid(room_jid.clone())
        .await
    {
        Ok(is_muc_jid) => is_muc_jid,
        Err(error) => {
            warn!(room = %room_jid, error = %error, "Failed to validate MUC JID");
            false
        }
    }
}

/// Atomically destroy a room via the registry `DestroyRoom` handler
/// (#1261, #1276): it removes the in-memory registry entry AND wipes the
/// clustering durable rows (config/subject/affiliations incl. bans)
/// under one claim fence. On a durable-delete failure it restores the
/// entry and reports [`DestroyRoomOutcome::DurableWipeFailed`], so the
/// room is either fully destroyed or fully intact — never split, and
/// with no separate pre-wipe to roll back (no fail-open window).
///
/// A transport-level ask failure (registry mailbox unavailable,
/// timeout) is surfaced as `Err`, NOT coerced to `NotRegistered`: the
/// atomic handler either fully applied or not at all, so the caller must
/// answer with a retryable wait-class error rather than `item-not-found`.
#[cfg(test)]
pub(crate) async fn destroy_room_actor(
    state: &WebSocketState,
    room_jid: &BareJid,
) -> Result<
    waddle_xmpp::muc::room_registry_actor::DestroyRoomOutcome,
    waddle_xmpp::muc::room_registry_actor::RoomRegistryError,
> {
    RoomRegistry::wrap(state.deps.protocol.room_registry.clone())
        .destroy_room(room_jid.clone())
        .await
}

#[cfg(test)]
mod destroy_completion_tests {
    use super::super::tests::create_test_websocket_state;
    use super::*;
    use waddle_xmpp::muc::{
        room_actor::GetSnapshot,
        room_registry_actor::{CreateInstantRoom, DestroyCompletion},
        DestroyRequest,
    };

    fn room_jid(local: &str) -> BareJid {
        format!("{local}@muc.example.com")
            .parse()
            .expect("bare jid")
    }

    async fn queue_destroy_completion(
        state: &WebSocketState,
        room_jid: &BareJid,
    ) -> waddle_xmpp::muc::DestroyAttemptId {
        let actor = state
            .deps
            .protocol
            .room_registry
            .ask(CreateInstantRoom {
                room_jid: room_jid.clone(),
            })
            .await
            .expect("create instant room")
            .actor_ref;
        let attempt = waddle_xmpp::muc::DestroyAttemptId::generate();
        let snapshot = actor.ask(GetSnapshot).await.expect("snapshot");
        let completion = DestroyCompletion {
            attempt,
            room_jid: room_jid.clone(),
            room: snapshot.room,
            departures: snapshot.departures,
            request: DestroyRequest::default(),
        };
        super::super::handlers::iq::muc_owner_moderation::persist_destroy_completion(
            state,
            &completion,
        )
        .await
        .expect("persist destroy completion");
        register_destroy_completion(state, completion)
            .await
            .expect("register destroy completion");
        assert_eq!(
            RoomRegistry::wrap(state.deps.protocol.room_registry.clone())
                .destroy_room_with_attempt(room_jid.clone(), attempt)
                .await
                .expect("destroy with attempt"),
            waddle_xmpp::muc::room_registry_actor::DestroyRoomOutcome::Destroyed,
        );
        attempt
    }

    #[tokio::test]
    async fn exact_inline_destroy_drain_leaves_unrelated_completions_queued() {
        let state = create_test_websocket_state().await;
        let first_room = room_jid("inline-destroy-first");
        let second_room = room_jid("inline-destroy-second");
        let first_attempt = queue_destroy_completion(state.as_ref(), &first_room).await;
        let second_attempt = queue_destroy_completion(state.as_ref(), &second_room).await;

        let frames = drain_destroy_completion_attempt(state.as_ref(), first_attempt, None)
            .await
            .expect("drain exact destroy completion");
        assert!(
            frames.frames.is_empty(),
            "no occupant frames are expected for empty-room destroy cleanup"
        );

        let remaining = RoomRegistry::wrap(state.deps.protocol.room_registry.clone())
            .take_destroy_completion_attempt(second_attempt)
            .await
            .expect("lease exact remaining completion");
        assert!(
            remaining.is_some(),
            "draining one attempt inline must leave unrelated queued cleanup untouched"
        );
    }
}

#[cfg(test)]
mod eviction_tests {
    use super::super::tests::create_test_websocket_state;
    use super::*;
    use waddle_xmpp::muc::{
        room_actor::{Join, LeaveByRealJid},
        room_registry_actor::{CreateInstantRoom, CreateRoom, RoomCount},
        RoomConfig,
    };
    use waddle_xmpp_core::{Affiliation, Role};

    fn full_jid(s: &str) -> FullJid {
        s.parse().expect("valid full jid")
    }

    fn room_bare_jid(local: &str) -> BareJid {
        format!("{local}@muc.example.com")
            .parse()
            .expect("bare jid")
    }

    #[cfg(feature = "clustering")]
    #[test]
    fn remote_muc_cleanup_retry_restore_does_not_overwrite_fresh_join() {
        let memberships = crate::server::routes::websocket::state::RemoteMucMemberships::default();
        let occupant = full_jid("alice@example.com/web");
        let room = room_bare_jid("race");

        memberships.record_join(&occupant, &room, "old-nick");
        let snapshot = memberships.take_for_occupant(&occupant);
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].room(), &room);
        assert_eq!(snapshot[0].nick(), "old-nick");

        memberships.record_join(&occupant, &room, "fresh-nick");
        memberships.restore_snapshot_if_current(&snapshot[0]);

        assert_eq!(
            memberships.nick_for(&occupant, &room).as_deref(),
            Some("fresh-nick")
        );
    }

    #[cfg(feature = "clustering")]
    #[test]
    fn remote_muc_cleanup_success_leaves_fresh_join_untouched() {
        use crate::clustering::route_bridge::{
            MucProxyRouteAttempt, MucProxyRouteDecision, OrderedRelayMucProxyOutcome::Delivered,
        };
        use waddle_xmpp::ingress::RelayTargetIdentity;

        let memberships = crate::server::routes::websocket::state::RemoteMucMemberships::default();
        let occupant = full_jid("alice@example.com/web");
        let room = room_bare_jid("race-success");

        memberships.record_join(&occupant, &room, "old-nick");
        let snapshot = memberships.take_for_occupant(&occupant);
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].room(), &room);
        assert_eq!(snapshot[0].nick(), "old-nick");

        memberships.record_join(&occupant, &room, "fresh-nick");
        assert_eq!(
            remote_muc_cleanup_disposition(&MucProxyRouteDecision::Attempted(
                MucProxyRouteAttempt {
                    relay_target: Some(RelayTargetIdentity::relay_node("relay-node")),
                    room_fence: None,
                    outcome: Delivered(Vec::new()),
                }
            )),
            RemoteMucCleanupDisposition::Converged
        );
        memberships.forget_snapshot_if_current(&snapshot[0]);

        assert_eq!(
            memberships.nick_for(&occupant, &room).as_deref(),
            Some("fresh-nick")
        );
    }

    #[tokio::test]
    async fn evicts_empty_instant_room_after_last_leave() {
        let state = create_test_websocket_state().await;
        let room_jid = room_bare_jid("evict-me");
        let room_actor = state
            .deps
            .protocol
            .room_registry
            .ask(CreateInstantRoom {
                room_jid: room_jid.clone(),
            })
            .await
            .expect("create instant room")
            .actor_ref;

        let alice = full_jid("alice@example.com/r1");
        room_actor
            .ask(Join {
                nick: "alice".to_string(),
                real_jid: alice.clone(),
                role: Role::Participant,
                affiliation: Affiliation::Member,
            })
            .await
            .expect("alice joins");

        let count_before: usize = state
            .deps
            .protocol
            .room_registry
            .ask(RoomCount)
            .await
            .expect("room count");
        assert_eq!(count_before, 1);

        let waddle_xmpp::muc::room_actor::LeaveDisposition::Left(outcome) = room_actor
            .ask(LeaveByRealJid {
                sender_jid: alice,
                cause: waddle_xmpp::muc::durable::OccupancyLeaveCause::Disconnect,
                session: waddle_xmpp::muc::room_actor::LeaveSessionSelector::Any,
                attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
                origin: waddle_xmpp::muc::room_actor::LeaveOrigin::Fresh,
            })
            .await
            .expect("leave")
        else {
            panic!("occupant leaves");
        };
        assert!(outcome.removed_last_session);
        assert_eq!(outcome.occupant_count, 0);
        assert!(!outcome.is_persistent);
        // #1647: acknowledge the departure receipt — an unacknowledged
        // receipt now legitimately vetoes the guarded destroy (EffectsOwed).
        assert_eq!(
            room_actor
                .ask(waddle_xmpp::muc::room_actor::AckDepartureReceipt {
                    attempt: outcome.acknowledge,
                })
                .await
                .expect("ack ask"),
            waddle_xmpp::muc::room_actor::AckDepartureOutcome::Acknowledged
        );

        maybe_evict_empty_room(&state, &room_jid, &outcome).await;

        let count_after: usize = state
            .deps
            .protocol
            .room_registry
            .ask(RoomCount)
            .await
            .expect("room count");
        assert_eq!(
            count_after, 0,
            "empty non-persistent room must be evicted from the registry"
        );
    }

    #[tokio::test]
    async fn does_not_evict_empty_persistent_room() {
        let state = create_test_websocket_state().await;
        let room_jid = room_bare_jid("keep-me");
        let room_actor = state
            .deps
            .protocol
            .room_registry
            .ask(CreateRoom {
                room_jid: room_jid.clone(),
                waddle_id: "w".to_string(),
                channel_id: "c".to_string(),
                config: RoomConfig::default(), // persistent: true
            })
            .await
            .expect("create persistent room");

        let alice = full_jid("alice@example.com/r1");
        room_actor
            .ask(Join {
                nick: "alice".to_string(),
                real_jid: alice.clone(),
                role: Role::Participant,
                affiliation: Affiliation::Member,
            })
            .await
            .expect("alice joins");

        let waddle_xmpp::muc::room_actor::LeaveDisposition::Left(outcome) = room_actor
            .ask(LeaveByRealJid {
                sender_jid: alice,
                cause: waddle_xmpp::muc::durable::OccupancyLeaveCause::Disconnect,
                session: waddle_xmpp::muc::room_actor::LeaveSessionSelector::Any,
                attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
                origin: waddle_xmpp::muc::room_actor::LeaveOrigin::Fresh,
            })
            .await
            .expect("leave")
        else {
            panic!("occupant leaves");
        };
        assert!(outcome.removed_last_session);
        assert_eq!(outcome.occupant_count, 0);
        assert!(
            outcome.is_persistent,
            "default RoomConfig is persistent — outcome must say so"
        );

        maybe_evict_empty_room(&state, &room_jid, &outcome).await;

        let count_after: usize = state
            .deps
            .protocol
            .room_registry
            .ask(RoomCount)
            .await
            .expect("room count");
        assert_eq!(
            count_after, 1,
            "persistent rooms (Waddle channels) must NOT be evicted on empty"
        );
    }

    #[tokio::test]
    async fn does_not_evict_when_other_occupants_remain() {
        let state = create_test_websocket_state().await;
        let room_jid = room_bare_jid("crowded");
        let room_actor = state
            .deps
            .protocol
            .room_registry
            .ask(CreateInstantRoom {
                room_jid: room_jid.clone(),
            })
            .await
            .expect("create instant room")
            .actor_ref;

        let alice = full_jid("alice@example.com/r1");
        let bob = full_jid("bob@example.com/r1");
        for (nick, jid) in [("alice", &alice), ("bob", &bob)] {
            room_actor
                .ask(Join {
                    nick: nick.to_string(),
                    real_jid: jid.clone(),
                    role: Role::Participant,
                    affiliation: Affiliation::Member,
                })
                .await
                .expect("join");
        }

        let waddle_xmpp::muc::room_actor::LeaveDisposition::Left(outcome) = room_actor
            .ask(LeaveByRealJid {
                sender_jid: alice,
                cause: waddle_xmpp::muc::durable::OccupancyLeaveCause::Disconnect,
                session: waddle_xmpp::muc::room_actor::LeaveSessionSelector::Any,
                attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
                origin: waddle_xmpp::muc::room_actor::LeaveOrigin::Fresh,
            })
            .await
            .expect("leave")
        else {
            panic!("occupant leaves");
        };
        assert_eq!(outcome.occupant_count, 1);

        maybe_evict_empty_room(&state, &room_jid, &outcome).await;

        let count_after: usize = state
            .deps
            .protocol
            .room_registry
            .ask(RoomCount)
            .await
            .expect("room count");
        assert_eq!(
            count_after, 1,
            "room must remain registered while at least one occupant is present"
        );
    }

    /// #1276 Greptile P1 ("Ask Failure Misclassified"): a transport-level
    /// registry ask failure (mailbox gone / timeout) must NOT be coerced
    /// to `Ok(NotRegistered)` — that would answer the owner
    /// `item-not-found` and stop, while the atomic destroy may or may not
    /// have applied. It must surface as `Err` so the caller returns a
    /// retryable wait-class error instead.
    #[tokio::test]
    async fn destroy_room_actor_transport_failure_is_err_not_not_registered() {
        let state = create_test_websocket_state().await;
        let room_jid = room_bare_jid("dead-registry-destroy");

        // Hard-kill the registry so the destroy ask fails at the transport
        // level rather than returning a genuine reply.
        state.deps.protocol.room_registry.kill();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        while state.deps.protocol.room_registry.is_alive() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "registry actor did not die in time"
            );
            tokio::task::yield_now().await;
        }

        let outcome = destroy_room_actor(&state, &room_jid).await;
        assert!(
            outcome.is_err(),
            "a transport-level registry ask failure must surface as Err (→ retryable \
             internal-server-error), never be coerced to Ok(NotRegistered) → \
             item-not-found (#1276 P1-B); got {outcome:?}"
        );
    }
}

#[cfg(all(test, feature = "clustering"))]
mod remote_muc_cleanup_disposition_tests {
    use super::{remote_muc_cleanup_disposition, RemoteMucCleanupDisposition};
    use crate::clustering::route_bridge::{
        MucProxyRouteAttempt, MucProxyRouteDecision, OrderedRelayMucProxyOutcome,
    };
    use waddle_xmpp::ingress::RelayTargetIdentity;

    fn attempted(outcome: OrderedRelayMucProxyOutcome) -> MucProxyRouteDecision {
        MucProxyRouteDecision::Attempted(MucProxyRouteAttempt {
            relay_target: Some(RelayTargetIdentity::relay_node("relay-node")),
            room_fence: None,
            outcome,
        })
    }

    /// #1249: the benign "room claim locally owned" and definitive
    /// "room unclaimed anywhere" cases converge by FORGETTING the
    /// membership (no warn, no ghost resurrection) — pre-fix they were
    /// classified `DefiniteNoEffect` and warned + restored forever.
    #[test]
    fn benign_local_and_unclaimed_rooms_forget_membership() {
        assert_eq!(
            remote_muc_cleanup_disposition(&MucProxyRouteDecision::LocalRoom),
            RemoteMucCleanupDisposition::NoRemoteOccupancy
        );
        assert_eq!(
            remote_muc_cleanup_disposition(&MucProxyRouteDecision::RoomUnclaimed),
            RemoteMucCleanupDisposition::NoRemoteOccupancy
        );
    }

    /// #1249: every failure that could leave a ghost occupant on a
    /// remote node is RETRYABLE — the membership is kept so the
    /// reconciliation janitor re-drives the relay.
    #[test]
    fn harmful_failures_are_retryable() {
        for decision in [
            MucProxyRouteDecision::OriginUnavailable,
            MucProxyRouteDecision::RoomClaimUnavailable,
            attempted(OrderedRelayMucProxyOutcome::Unavailable),
            attempted(OrderedRelayMucProxyOutcome::Dropped),
        ] {
            assert_eq!(
                remote_muc_cleanup_disposition(&decision),
                RemoteMucCleanupDisposition::RetryableFailure,
                "{decision:?} must keep the membership for janitor re-drive"
            );
        }
    }

    #[test]
    fn delivered_converges_and_uncertain_commit_retries_quietly() {
        assert_eq!(
            remote_muc_cleanup_disposition(&attempted(OrderedRelayMucProxyOutcome::Delivered(
                Vec::new(),
            ))),
            RemoteMucCleanupDisposition::Converged
        );
        for outcome in [
            OrderedRelayMucProxyOutcome::MaybeCommitted,
            OrderedRelayMucProxyOutcome::JoinMaybeCommitted,
        ] {
            assert_eq!(
                remote_muc_cleanup_disposition(&attempted(outcome)),
                RemoteMucCleanupDisposition::UncertainCommit
            );
        }
    }
}

#[cfg(all(test, feature = "clustering"))]
mod local_departure_cleanup_tests {
    use super::super::tests::{
        create_test_websocket_state, create_test_websocket_state_with_clustering,
        create_test_websocket_state_with_sfu_and_clustering, register_test_connection,
        snapshot_room, RecordingSfu,
    };
    use super::*;
    use crate::clustering::ClusteringHandles;
    use jid::{BareJid, FullJid};
    use kameo::actor::ActorRef;
    use std::collections::HashMap;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex, OnceLock,
    };
    use std::time::{Duration, Instant};
    use tokio::sync::mpsc;
    use waddle_xmpp::muc::durable::{
        DurableRoomState, MucDurableFuture, MucDurableStore, RoomClaimFenceContext,
    };
    use waddle_xmpp::muc::room_actor::{
        GetSnapshot, JoinAffiliationGrant, JoinWithAffiliation, RoomActor,
    };
    use waddle_xmpp::muc::room_registry_actor::{CreateRoom, ListRooms, WireClusteringClaims};
    use waddle_xmpp::muc::{
        RoomCommitError, RoomCommitOutcome, RoomCommittedCoordinates, RoomConfig, RoomLifecycleId,
        RoomMutationEffects, RoomRevision,
    };
    use waddle_xmpp::ownership::{
        ClaimStore, InProcessClaimStore, NodeIdentity, SharedNodeIdentity,
    };
    use waddle_xmpp::Affiliation;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum LeaveProjectionMode {
        Succeed,
        OwnershipUnavailable,
        NotOwner,
        Hang,
        Delay(Duration),
    }

    struct CleanupProjectionStore {
        leave_mode: Mutex<LeaveProjectionMode>,
        room_leave_modes: Mutex<HashMap<BareJid, LeaveProjectionMode>>,
        established_fences: Mutex<HashMap<BareJid, RoomClaimFenceContext>>,
        lifecycle: OnceLock<RoomLifecycleId>,
        next_revision: AtomicUsize,
    }

    impl CleanupProjectionStore {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                leave_mode: Mutex::new(LeaveProjectionMode::Succeed),
                room_leave_modes: Mutex::new(HashMap::new()),
                established_fences: Mutex::new(HashMap::new()),
                lifecycle: OnceLock::new(),
                next_revision: AtomicUsize::new(0),
            })
        }

        fn set_leave_mode(&self, mode: LeaveProjectionMode) {
            *self.leave_mode.lock().expect("leave mode lock") = mode;
        }

        fn set_leave_mode_for_room(&self, room: &BareJid, mode: LeaveProjectionMode) {
            self.room_leave_modes
                .lock()
                .expect("room leave modes lock")
                .insert(room.clone(), mode);
        }

        fn next_commit_coordinates(&self) -> RoomCommittedCoordinates {
            let lifecycle = *self.lifecycle.get_or_init(RoomLifecycleId::generate);
            let revision = self.next_revision.fetch_add(1, Ordering::SeqCst) + 1;
            RoomCommittedCoordinates {
                lifecycle,
                revision: RoomRevision::from_stored(revision as i64).expect("positive revision"),
            }
        }
    }

    impl MucDurableStore for CleanupProjectionStore {
        fn load_room_state_fenced<'a>(
            &'a self,
            room_jid: &'a BareJid,
            fence: &'a RoomClaimFenceContext,
        ) -> MucDurableFuture<'a, Option<DurableRoomState>> {
            let exact = self
                .established_fences
                .lock()
                .expect("fence lock")
                .get(room_jid)
                .cloned();
            let given = fence.clone();
            Box::pin(async move {
                if exact.as_ref() == Some(&given) {
                    Ok(None)
                } else {
                    Err(waddle_xmpp::XmppError::internal(
                        "unexpected room claim fence",
                    ))
                }
            })
        }

        fn commit_room_mutation<'a>(
            &'a self,
            room_jid: &'a BareJid,
            fence: &'a RoomClaimFenceContext,
            intent: waddle_xmpp::muc::RoomDurableMutation,
            _effects: RoomMutationEffects,
        ) -> waddle_xmpp::muc::RoomCommitFuture<'a> {
            let exact = self
                .established_fences
                .lock()
                .expect("fence lock")
                .get(room_jid)
                .cloned();
            let given = fence.clone();
            let leave_mode = self
                .room_leave_modes
                .lock()
                .expect("room leave modes lock")
                .get(room_jid)
                .copied()
                .unwrap_or_else(|| *self.leave_mode.lock().expect("leave mode lock"));
            let coordinates = self.next_commit_coordinates();
            Box::pin(async move {
                if exact.as_ref() != Some(&given) {
                    return Err(RoomCommitError::OwnershipUnavailable);
                }
                match intent {
                    waddle_xmpp::muc::RoomDurableMutation::Projection(
                        waddle_xmpp::muc::durable::RoomProjection::OccupancyLeave { .. },
                    ) => match leave_mode {
                        LeaveProjectionMode::Succeed => Ok(RoomCommitOutcome {
                            coordinates,
                            reservation: None,
                        }),
                        LeaveProjectionMode::OwnershipUnavailable => {
                            Err(RoomCommitError::OwnershipUnavailable)
                        }
                        LeaveProjectionMode::NotOwner => Err(RoomCommitError::NotOwner),
                        LeaveProjectionMode::Hang => {
                            std::future::pending::<Result<RoomCommitOutcome, RoomCommitError>>()
                                .await
                        }
                        LeaveProjectionMode::Delay(delay) => {
                            tokio::time::sleep(delay).await;
                            Ok(RoomCommitOutcome {
                                coordinates,
                                reservation: None,
                            })
                        }
                    },
                    _ => Ok(RoomCommitOutcome {
                        coordinates,
                        reservation: None,
                    }),
                }
            })
        }

        fn establish_claim_fence(&self, room_jid: &BareJid, fence: RoomClaimFenceContext) {
            self.established_fences
                .lock()
                .expect("fence lock")
                .insert(room_jid.clone(), fence);
        }

        fn check_exact_claim_fence<'a>(
            &'a self,
            room_jid: &'a BareJid,
            fence: &'a RoomClaimFenceContext,
        ) -> MucDurableFuture<'a, bool> {
            let exact = self
                .established_fences
                .lock()
                .expect("fence lock")
                .get(room_jid)
                .cloned();
            let given = fence.clone();
            Box::pin(async move { Ok(exact.as_ref() == Some(&given)) })
        }
    }

    fn room_jid(local: &str) -> BareJid {
        format!("{local}@muc.example.com")
            .parse()
            .expect("room jid")
    }

    fn full_jid(value: &str) -> FullJid {
        value.parse().expect("full jid")
    }

    async fn clustered_state_with_store(store: Arc<CleanupProjectionStore>) -> Arc<WebSocketState> {
        let claim_store = Arc::new(InProcessClaimStore::new());
        let node_identity = SharedNodeIdentity::new(NodeIdentity::local());
        let durable_store: Arc<dyn MucDurableStore> = store.clone();
        let state = create_test_websocket_state_with_clustering(
            ClusteringHandles {
                claim_store: Some(claim_store.clone() as Arc<dyn ClaimStore>),
                node_identity: Some(node_identity.clone()),
                muc_durable_store: Some(durable_store.clone()),
                ..ClusteringHandles::default()
            },
            Arc::new(waddle_xmpp::stream_management::InMemorySmSessionRegistry::new()),
        )
        .await;
        state
            .deps
            .protocol
            .room_registry
            .ask(WireClusteringClaims {
                claim_store: claim_store as Arc<dyn ClaimStore>,
                node_identity,
                durable_store: Some(durable_store),
                rollout_backoff: None,
            })
            .await
            .expect("wire clustering claims");
        state
    }

    async fn clustered_state_with_sfu_and_store(
        store: Arc<CleanupProjectionStore>,
        sfu: Arc<RecordingSfu>,
    ) -> Arc<WebSocketState> {
        let claim_store = Arc::new(InProcessClaimStore::new());
        let node_identity = SharedNodeIdentity::new(NodeIdentity::local());
        let durable_store: Arc<dyn MucDurableStore> = store.clone();
        let state = create_test_websocket_state_with_sfu_and_clustering(
            sfu,
            ClusteringHandles {
                claim_store: Some(claim_store.clone() as Arc<dyn ClaimStore>),
                node_identity: Some(node_identity.clone()),
                muc_durable_store: Some(durable_store.clone()),
                ..ClusteringHandles::default()
            },
        )
        .await;
        state
            .deps
            .protocol
            .room_registry
            .ask(WireClusteringClaims {
                claim_store: claim_store as Arc<dyn ClaimStore>,
                node_identity,
                durable_store: Some(durable_store),
                rollout_backoff: None,
            })
            .await
            .expect("wire clustering claims");
        state
    }

    async fn join_member(room_actor: &ActorRef<RoomActor>, jid: &FullJid, nick: &str) {
        room_actor
            .ask(JoinWithAffiliation {
                sender_jid: jid.clone(),
                nick: nick.to_string(),
                affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
                local_domain: "example.com".to_string(),
                admission_revision: 0,
            })
            .await
            .expect("join occupant");
    }

    #[tokio::test]
    async fn disconnect_cleanup_deferred_leave_records_room_departure_and_does_not_broadcast() {
        let store = CleanupProjectionStore::new();
        let state = clustered_state_with_store(store.clone()).await;
        let room_jid = room_jid("disconnect-deferred");
        let room_actor = state
            .deps
            .protocol
            .room_registry
            .ask(CreateRoom {
                room_jid: room_jid.clone(),
                waddle_id: "w".to_string(),
                channel_id: "c".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("create room");
        let alice = full_jid("alice@example.com/web");
        let bob = full_jid("bob@example.com/phone");
        join_member(&room_actor, &alice, "alice").await;
        join_member(&room_actor, &bob, "bob").await;

        let (bob_tx, mut bob_rx) = mpsc::channel(8);
        register_test_connection(state.as_ref(), &bob, bob_tx).await;
        let watermark = snapshot_room(state.as_ref(), &room_jid)
            .await
            .occupancy_revision;

        store.set_leave_mode(LeaveProjectionMode::OwnershipUnavailable);
        assert_eq!(
            cleanup_muc_presence_for_jid(state.as_ref(), &alice).await,
            MucCleanupOutcome::Failed
        );

        let due = state
            .deps
            .protocol
            .pending_local_muc_departures
            .take_due(Instant::now());
        assert_eq!(due.len(), 1, "one deferred room departure is retained");
        assert!(
            tokio::time::timeout(Duration::from_millis(100), bob_rx.recv())
                .await
                .is_err()
        );
        match &due[0].item {
            LocalDepartureItem::RoomDeparture {
                room,
                jid,
                cause,
                selector,
                ..
            } => {
                assert_eq!(room, &room_jid);
                assert_eq!(jid, &alice);
                assert_eq!(*cause, OccupancyLeaveCause::Disconnect);
                assert_eq!(
                    *selector,
                    LeaveSessionSelector::JoinedAtOrBefore(
                        waddle_xmpp::muc::room_actor::OccupancyWatermark::from_revision(watermark)
                    )
                );
            }
            other => panic!("expected retained room departure, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn disconnect_cleanup_enumeration_failure_records_full_jid_sweep() {
        let state = create_test_websocket_state().await;
        state.deps.protocol.room_registry.kill();
        state.deps.protocol.room_registry.wait_for_shutdown().await;

        let jid = full_jid("alice@example.com/web");
        assert_eq!(
            cleanup_muc_presence_for_jid(state.as_ref(), &jid).await,
            MucCleanupOutcome::Failed
        );

        let due = state
            .deps
            .protocol
            .pending_local_muc_departures
            .take_due(Instant::now());
        assert_eq!(due.len(), 1);
        assert!(
            matches!(
                &due[0].item,
                LocalDepartureItem::FullJidSweep { jid: sweep_jid, .. } if sweep_jid == &jid
            ),
            "enumeration failure retains a full-JID sweep: {:?}",
            due[0].item
        );
    }

    #[tokio::test]
    async fn disconnect_cleanup_room_sealed_records_confirm_retired() {
        let store = CleanupProjectionStore::new();
        let state = clustered_state_with_store(store.clone()).await;
        let room_jid = room_jid("disconnect-sealed");
        let room_actor = state
            .deps
            .protocol
            .room_registry
            .ask(CreateRoom {
                room_jid: room_jid.clone(),
                waddle_id: "w".to_string(),
                channel_id: "c".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("create room");
        let alice = full_jid("alice@example.com/web");
        join_member(&room_actor, &alice, "alice").await;

        store.set_leave_mode(LeaveProjectionMode::NotOwner);
        assert_eq!(
            cleanup_muc_presence_for_jid(state.as_ref(), &alice).await,
            MucCleanupOutcome::Failed
        );

        let due = state
            .deps
            .protocol
            .pending_local_muc_departures
            .take_due(Instant::now());
        assert_eq!(due.len(), 1, "sealed actor responsibility is retained");
        match &due[0].item {
            LocalDepartureItem::ConfirmRetired {
                room,
                jid,
                actor,
                cause,
                selector,
                ..
            } => {
                assert_eq!(room, &room_jid);
                assert_eq!(jid, &alice);
                assert_eq!(*actor, room_actor.id());
                assert_eq!(*cause, OccupancyLeaveCause::Disconnect);
                assert_eq!(*selector, LeaveSessionSelector::Any);
            }
            other => panic!("expected confirm-retired item, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn disconnect_cleanup_room_sealed_still_unregisters_sfu_participant() {
        let store = CleanupProjectionStore::new();
        let recorder = Arc::new(RecordingSfu::default());
        let state = clustered_state_with_sfu_and_store(store.clone(), recorder.clone()).await;
        let room_jid = room_jid("disconnect-sealed-sfu");
        let room_actor = state
            .deps
            .protocol
            .room_registry
            .ask(CreateRoom {
                room_jid: room_jid.clone(),
                waddle_id: "w".to_string(),
                channel_id: "c".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("create room");
        let alice = full_jid("alice@example.com/web");
        join_member(&room_actor, &alice, "alice").await;
        store.set_leave_mode(LeaveProjectionMode::NotOwner);

        assert_eq!(
            cleanup_muc_presence_for_jid(state.as_ref(), &alice).await,
            MucCleanupOutcome::Failed
        );
        assert_eq!(
            recorder.snapshot().len(),
            1,
            "the first attempt unregisters SFU"
        );
        assert_eq!(state.deps.protocol.pending_local_muc_departures.len(), 1);
    }

    #[tokio::test]
    async fn suppressed_leave_is_acknowledged_and_leaves_no_receipt() {
        let state = create_test_websocket_state().await;
        let room_jid = room_jid("disconnect-suppressed-ack");
        let room_actor = state
            .deps
            .protocol
            .room_registry
            .ask(CreateRoom {
                room_jid: room_jid.clone(),
                waddle_id: "w".to_string(),
                channel_id: "c".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("create store-less room");
        let alice = full_jid("alice@example.com/web");
        join_member(&room_actor, &alice, "alice").await;
        room_actor
            .ask(waddle_xmpp::muc::room_actor::SealForDestroy {
                attempt: waddle_xmpp::muc::DestroyAttemptId::generate(),
            })
            .await
            .expect("seal room for destroy");

        assert_eq!(
            cleanup_muc_presence_for_jid(state.as_ref(), &alice).await,
            MucCleanupOutcome::Completed
        );
        // The acknowledgement is delivered by a detached task after the
        // cleanup returned: wait for it.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let receipts = room_actor
                .ask(GetSnapshot)
                .await
                .expect("snapshot after suppressed cleanup")
                .departures
                .receipts
                .len();
            if receipts == 0 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "disconnect cleanup acknowledges the suppressed receipt"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn disconnect_cleanup_mailbox_saturation_retains_departure_and_continues() {
        let store = CleanupProjectionStore::new();
        let state = clustered_state_with_store(store.clone()).await;
        let first_room = room_jid("disconnect-timeout-first");
        let second_room = room_jid("disconnect-timeout-second");
        let first_actor = state
            .deps
            .protocol
            .room_registry
            .ask(CreateRoom {
                room_jid: first_room.clone(),
                waddle_id: "w-first".to_string(),
                channel_id: "c-first".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("create first room");
        let second_actor = state
            .deps
            .protocol
            .room_registry
            .ask(CreateRoom {
                room_jid: second_room.clone(),
                waddle_id: "w-second".to_string(),
                channel_id: "c-second".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("create second room");
        let alice = full_jid("alice@example.com/web");
        let bob = full_jid("bob@example.com/phone");
        join_member(&first_actor, &alice, "alice").await;
        join_member(&first_actor, &bob, "bob").await;
        join_member(&second_actor, &alice, "alice").await;
        join_member(&second_actor, &bob, "bob").await;
        // `ListRooms` exposes the exact HashMap iteration order that cleanup
        // will use. Hang that first room so the second one proves the loop
        // continues after the five-second ask timeout.
        let room_order = state
            .deps
            .protocol
            .room_registry
            .ask(ListRooms)
            .await
            .expect("list rooms");
        assert_eq!(room_order.len(), 2, "both test rooms must be listed");
        let hanging_room = room_order[0].clone();
        let succeeding_room = room_order[1].clone();
        store.set_leave_mode_for_room(&hanging_room, LeaveProjectionMode::Hang);
        let hanging_actor = if hanging_room == first_room {
            first_actor.clone()
        } else {
            second_actor.clone()
        };

        let wedging_leave = tokio::spawn({
            let actor = hanging_actor.clone();
            let alice = alice.clone();
            async move {
                actor
                    .ask(LeaveByRealJid {
                        sender_jid: alice,
                        cause: OccupancyLeaveCause::Disconnect,
                        session: LeaveSessionSelector::Any,
                        attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
                        origin: waddle_xmpp::muc::room_actor::LeaveOrigin::Fresh,
                    })
                    .mailbox_timeout(Duration::from_secs(30))
                    .reply_timeout(Duration::from_secs(30))
                    .await
            }
        });
        tokio::time::sleep(Duration::from_millis(100)).await;

        let mut mailbox_fillers = Vec::new();
        for _ in 0..80 {
            let actor = hanging_actor.clone();
            let alice = alice.clone();
            mailbox_fillers.push(tokio::spawn(async move {
                actor
                    .ask(LeaveByRealJid {
                        sender_jid: alice,
                        cause: OccupancyLeaveCause::Disconnect,
                        session: LeaveSessionSelector::Any,
                        attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
                        origin: waddle_xmpp::muc::room_actor::LeaveOrigin::Fresh,
                    })
                    .mailbox_timeout(Duration::from_secs(30))
                    .reply_timeout(Duration::from_secs(30))
                    .await
            }));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert_eq!(
            cleanup_muc_presence_for_jid(state.as_ref(), &alice).await,
            MucCleanupOutcome::Failed
        );

        let retained = state
            .deps
            .protocol
            .pending_local_muc_departures
            .take_due(Instant::now());
        assert_eq!(
            retained.len(),
            1,
            "the mailbox-saturated room must be retained for retry"
        );
        assert!(matches!(
            &retained[0].item,
            LocalDepartureItem::RoomDeparture {
                room,
                jid,
                cause: OccupancyLeaveCause::Disconnect,
                selector: LeaveSessionSelector::Any,
                ..
            } if room == &hanging_room && jid == &alice
        ));
        assert!(
            (if succeeding_room == first_room {
                &first_actor
            } else {
                &second_actor
            })
            .ask(GetSnapshot)
            .await
            .expect("succeeding room snapshot")
            .room
            .find_occupant_by_real_jid(&alice)
            .is_none(),
            "a saturated mailbox in one room must not block cleanup of another room"
        );

        hanging_actor.kill();
        wedging_leave.abort();
        for handle in mailbox_fillers {
            handle.abort();
        }
    }

    #[tokio::test]
    async fn ask_leave_bounded_applies_one_deadline_to_mailbox_and_reply() {
        let store = CleanupProjectionStore::new();
        let state = clustered_state_with_store(store.clone()).await;
        let room_jid = room_jid("ask-leave-bounded-deadline");
        let room_actor = state
            .deps
            .protocol
            .room_registry
            .ask(CreateRoom {
                room_jid: room_jid.clone(),
                waddle_id: "w".to_string(),
                channel_id: "c".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("create room");
        store.set_leave_mode(LeaveProjectionMode::Delay(Duration::from_secs(3)));

        let mut occupants = Vec::new();
        for index in 0..66_u32 {
            let jid = full_jid(&format!("user{index}@example.com/web"));
            join_member(&room_actor, &jid, &format!("user{index}")).await;
            occupants.push(jid);
        }

        let wedging_leave = tokio::spawn({
            let actor = room_actor.clone();
            let sender_jid = occupants[0].clone();
            async move {
                actor
                    .ask(LeaveByRealJid {
                        sender_jid,
                        cause: OccupancyLeaveCause::Disconnect,
                        session: LeaveSessionSelector::Any,
                        attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
                        origin: waddle_xmpp::muc::room_actor::LeaveOrigin::Fresh,
                    })
                    .mailbox_timeout(Duration::from_secs(30))
                    .reply_timeout(Duration::from_secs(30))
                    .await
            }
        });
        tokio::time::sleep(Duration::from_millis(100)).await;

        let mut mailbox_fillers = Vec::new();
        for sender_jid in occupants.iter().skip(1).take(64).cloned() {
            let actor = room_actor.clone();
            mailbox_fillers.push(tokio::spawn(async move {
                actor
                    .ask(LeaveByRealJid {
                        sender_jid,
                        cause: OccupancyLeaveCause::Disconnect,
                        session: LeaveSessionSelector::Any,
                        attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
                        origin: waddle_xmpp::muc::room_actor::LeaveOrigin::Fresh,
                    })
                    .mailbox_timeout(Duration::from_secs(30))
                    .reply_timeout(Duration::from_secs(30))
                    .await
            }));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;

        let started = std::time::Instant::now();
        let result = ask_leave_bounded(
            &room_actor,
            LeaveByRealJid {
                sender_jid: occupants[65].clone(),
                cause: OccupancyLeaveCause::Disconnect,
                session: LeaveSessionSelector::Any,
                attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
                origin: waddle_xmpp::muc::room_actor::LeaveOrigin::Fresh,
            },
        )
        .await;
        let elapsed = started.elapsed();

        assert!(matches!(result, Err(LeaveAskFailure::Timeout)));
        assert!(
            elapsed < Duration::from_secs(6),
            "the combined ask must time out once, not after stacked mailbox+reply waits: {elapsed:?}"
        );

        room_actor.kill();
        room_actor.wait_for_shutdown().await;
        wedging_leave.abort();
        for handle in mailbox_fillers {
            handle.abort();
        }
    }

    #[tokio::test]
    async fn ask_leave_bounded_maps_handler_errors() {
        let store = CleanupProjectionStore::new();
        let state = clustered_state_with_store(store.clone()).await;
        let room_jid = room_jid("ask-leave-bounded-room-sealed");
        let room_actor = state
            .deps
            .protocol
            .room_registry
            .ask(CreateRoom {
                room_jid: room_jid.clone(),
                waddle_id: "w".to_string(),
                channel_id: "c".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("create room");
        let alice = full_jid("alice@example.com/web");
        join_member(&room_actor, &alice, "alice").await;
        store.set_leave_mode(LeaveProjectionMode::NotOwner);

        let result = ask_leave_bounded(
            &room_actor,
            LeaveByRealJid {
                sender_jid: alice,
                cause: OccupancyLeaveCause::Disconnect,
                session: LeaveSessionSelector::Any,
                attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
                origin: waddle_xmpp::muc::room_actor::LeaveOrigin::Fresh,
            },
        )
        .await;

        assert!(matches!(
            result,
            Err(LeaveAskFailure::Handler(
                waddle_xmpp::muc::room_actor::RoomActorError::RoomSealed
            ))
        ));
    }

    #[tokio::test]
    async fn ask_leave_bounded_maps_stopped_actor_to_transport() {
        let state = create_test_websocket_state().await;
        let room_jid = room_jid("ask-leave-bounded-transport");
        let room_actor = state
            .deps
            .protocol
            .room_registry
            .ask(CreateRoom {
                room_jid,
                waddle_id: "w".to_string(),
                channel_id: "c".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("create room");
        room_actor.kill();
        room_actor.wait_for_shutdown().await;

        let result = ask_leave_bounded(
            &room_actor,
            LeaveByRealJid {
                sender_jid: full_jid("alice@example.com/web"),
                cause: OccupancyLeaveCause::Disconnect,
                session: LeaveSessionSelector::Any,
                attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
                origin: waddle_xmpp::muc::room_actor::LeaveOrigin::Fresh,
            },
        )
        .await;

        assert!(matches!(result, Err(LeaveAskFailure::Transport)));
    }
}
