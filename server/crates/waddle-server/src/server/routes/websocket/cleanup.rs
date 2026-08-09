use super::*;
use super::{
    replay::{
        drain_outbound_into_replay, drain_outbound_into_terminal_recovery, PendingRowDrainPolicy,
        TerminalDrainContext,
    },
    state::WsConnState,
    stream_management::sm_show_from_name,
};
use waddle_xmpp::muc::room_actor::{LeaveOutcome, SealGuard};
use waddle_xmpp::muc::room_registry_actor::{RoomAcquisition, RoomRegistryError};
use waddle_xmpp::muc::RoomRegistry;
use waddle_xmpp::xep::xep0272::Muji;
use waddle_xmpp::xep::xep0421::OccupantIdentity;

#[cfg(feature = "clustering")]
async fn unregister_remote_user_resource_if_owner(
    state: &WebSocketState,
    jid: &FullJid,
    owner: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    if let Some(bridge) = state
        .deps
        .app_state
        .clustering_claims
        .ordered_relay_delivery_bridge
        .as_ref()
    {
        bridge
            .unregister_remote_user_resource_if_owner(jid, owner)
            .await;
    }
}

#[cfg(not(feature = "clustering"))]
async fn unregister_remote_user_resource_if_owner(
    _state: &WebSocketState,
    _jid: &FullJid,
    _owner: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
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
pub(crate) async fn broadcast_muc_leave_to_remaining(
    state: &WebSocketState,
    room_jid: &BareJid,
    sender_jid: &FullJid,
    outcome: &LeaveOutcome,
) {
    if !outcome.removed_last_session {
        return;
    }
    let from_jid = room_jid
        .clone()
        .with_resource_str(&outcome.nick)
        .unwrap_or_else(|_| sender_jid.clone());
    let sender_bare = sender_jid.to_bare();
    for occupant_jid in &outcome.remaining_occupants {
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
    }
}

/// Broadcast canonical available room/nick presence after one
/// resource's Muji state changes while another same-nick session
/// remains. Sibling Muji state is emitted under the exact full JID
/// that owns it so resource-scoped XEP-0272 preparing state stays
/// attributable.
pub(crate) async fn broadcast_muc_muji_clear_to_remaining(
    state: &WebSocketState,
    room_jid: &BareJid,
    leaving_real_jid: &FullJid,
    outcome: &LeaveOutcome,
) {
    if outcome.removed_last_session || !outcome.cleared_muji_state {
        return;
    }
    let from_jid = room_jid
        .clone()
        .with_resource_str(&outcome.nick)
        .unwrap_or_else(|_| {
            outcome
                .remaining_nick_real_jid
                .clone()
                .unwrap_or_else(|| outcome.leaving_room_jid.clone())
        });
    let mut entries = Vec::with_capacity(outcome.remaining_muji_sessions.len() + 1);
    entries.push((leaving_real_jid.clone(), Muji::default()));
    entries.extend(outcome.remaining_muji_sessions.iter().cloned());
    entries.sort_by_key(|(owner_jid, muji)| (muji_reflection_rank(muji), owner_jid.to_string()));
    for occupant_jid in &outcome.remaining_occupants {
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

/// Same cleanup path, but with explicit ordered-relay origin provenance for
/// clustered remote MUC leaves.
#[cfg(feature = "clustering")]
pub async fn cleanup_muc_presence_for_jid_with_origin(
    state: &WebSocketState,
    jid: &FullJid,
    origin: crate::server::routes::interpret::OrderedRelayRouteOrigin,
) -> MucCleanupOutcome {
    if cleanup_muc_presence_with_origin(state, jid, Some(&origin)).await {
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
    if cleanup_muc_presence_with_origin(state, jid, None).await {
        MucCleanupOutcome::Completed
    } else {
        MucCleanupOutcome::Failed
    }
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
    if !(outcome.removed_last_session && outcome.occupant_count == 0 && !outcome.is_persistent) {
        return true;
    }
    // #1108: revision-guarded destroy. The registry asks the room actor
    // to seal itself only if it is still empty at the occupancy revision
    // captured by this leave — a join admitted after the leave bumps the
    // revision and the destroy refuses instead of orphaning the fresh
    // occupant.
    let destroyed = match RoomRegistry::wrap(state.deps.protocol.room_registry.clone())
        .destroy_room_if_inactive(
            room_jid.clone(),
            outcome.occupancy_revision,
            SealGuard::EmptyNonPersistent,
        )
        .await
    {
        Ok(destroyed) => destroyed,
        Err(error) => {
            warn!(room = %room_jid, error = %error, "Failed guarded destroy of empty room");
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
    // A superseded ordinary connection must not touch the replacement's
    // registry or MUC state. Terminal SM recovery is different: it only
    // promotes this task's already accepted delivery and leaves those shared
    // resources alone.
    if superseded && !conn.sm_recovery_required {
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
            let principal = match conn.authenticated_session.as_ref() {
                Some(session) => match session.authenticated_principal_ref() {
                    Ok(principal) => principal,
                    Err(error) => {
                        // Pre-v11 rows do not carry an auth context. They
                        // cannot prove a durable resume identity, so fall
                        // through to ordinary non-resumable cleanup below.
                        warn!(jid = %jid, %error, "Refusing SM detach without a durable principal");
                        return refuse_detach_without_principal(
                            state,
                            outbound_rx,
                            &jid,
                            conn,
                            vec![detached],
                            false,
                            TerminalRouteRemoval::NotAttempted,
                        )
                        .await;
                    }
                },
                None => {
                    warn!(jid = %jid, "Refusing SM detach without an authenticated principal");
                    return refuse_detach_without_principal(
                        state,
                        outbound_rx,
                        &jid,
                        conn,
                        vec![detached],
                        false,
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
                                crate::sm_promotion::promote_displaced_sessions(
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
                    crate::server::dual_registration::mirror_unregister(
                        &state.deps.protocol.user_registry,
                        &jid,
                        Some(std::sync::Arc::clone(owner)),
                    )
                    .await;
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
                    let detach_fail_removed = state
                        .deps
                        .protocol
                        .connection_registry
                        .unregister_if_owner(&jid, owner)
                        .is_some();
                    let cleanup_origin = clustered_cleanup_origin(state, &jid, owner).await;
                    if detach_fail_removed {
                        cleanup_muc_presence_with_origin(state, &jid, cleanup_origin.as_ref())
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
        cleanup_muc_presence_with_origin(state, &jid, cleanup_origin.as_ref()).await;
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
    ConnectionShutdownOutcome::NotPersisted
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
    let released_terminal_rows =
        release_terminally_promoted_pending_rows(state, &mut detached).await;
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
    let recovery = terminal_recovery_session(
        state,
        outbound_rx,
        conn,
        detached,
        detached_snapshot,
        terminal_route_removed.is_some(),
    )
    .await;
    let released_terminal_rows = released_terminal_rows || recovery.released_pending_rows;
    refuse_detach_without_principal(
        state,
        outbound_rx,
        jid,
        conn,
        vec![recovery.session],
        released_terminal_rows,
        TerminalRouteRemoval::Attempted(terminal_route_removed),
    )
    .await
}

/// A live SM queue stores only replay XML, while pending-delivery stores the
/// durable `(stream_id, outbound_sequence)` ownership binding. Terminal
/// recovery must discard that replay copy after releasing the durable row;
/// otherwise promotion treats it as a new message and can collide with an
/// existing Archived payload's uniqueness key.
async fn release_terminally_promoted_pending_rows(
    state: &WebSocketState,
    detached: &mut waddle_xmpp::stream_management::DetachedSession,
) -> bool {
    let sequences: std::collections::HashSet<u32> = detached
        .unacked_stanzas
        .iter()
        .map(|entry| entry.sequence)
        .collect::<std::collections::HashSet<u32>>();
    let session_id = waddle_xmpp::pending_delivery::SmSessionId::new(detached.stream_id.clone());
    let sequence_bound_rows = match state
        .deps
        .protocol
        .pending_delivery_storage
        .list(&detached.jid.to_bare())
        .await
    {
        Ok(rows) => rows
            .into_iter()
            .filter(|row| {
                row.flushed_in_session.as_ref() == Some(&session_id)
                    && row
                        .outbound_sequence
                        .is_some_and(|sequence| sequences.contains(&sequence))
            })
            .filter_map(|row| row.outbound_sequence)
            .collect::<std::collections::HashSet<u32>>(),
        Err(error) => {
            warn!(
                stream_id = %detached.stream_id,
                jid = %detached.jid,
                %error,
                "terminal recovery could not preflight sequence-bound pending rows; release failures may still leave replay entries promotable"
            );
            std::collections::HashSet::new()
        }
    };
    let outcome = state
        .deps
        .protocol
        .pending_delivery_storage
        .release_rows_for_outbound_sequences(&detached.jid.to_bare(), &session_id, &sequences)
        .await;
    let mut replay_sequences_to_strip = outcome.released.clone();
    if outcome.error.is_some() {
        replay_sequences_to_strip.extend(sequence_bound_rows.iter().copied());
    }
    detached
        .unacked_stanzas
        .retain(|entry| !replay_sequences_to_strip.contains(&entry.sequence));
    if let Some(error) = outcome.error {
        if outcome.released.is_empty() {
            warn!(
                stream_id = %detached.stream_id,
                jid = %detached.jid,
                %error,
                "terminal recovery could not release sequence-bound pending rows; stripped any row-backed replay copies so cleanup can only re-drive the durable rows after claim cleanup"
            );
        } else {
            warn!(
                stream_id = %detached.stream_id,
                jid = %detached.jid,
                released = outcome.released.len(),
                %error,
                "terminal recovery partially released sequence-bound pending rows; stripped every matching replay copy so unreleased durable rows wait for claim cleanup instead of being promoted again"
            );
        }
    }
    !replay_sequences_to_strip.is_empty()
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
            .map(|entry| LivePendingFlushTarget {
                resource,
                owner: std::sync::Arc::clone(&entry.carbons_enabled),
                sm_session: entry.sm_stream_id(),
            })
    })
}

/// Terminal recovery can release sequence-bound `pending_delivery` rows back
/// to bare-JID storage after a newer live resource has already spent its own
/// once-per-session offline-flush claim. Re-drive those rows directly to the
/// current best live resource here so they do not wait for a future presence
/// update that may never arrive.
///
/// This intentionally does NOT touch RFC 6121 pending-subscribe delivery. That
/// queue is bare-JID scoped and persists until approval/denial; fresh sessions
/// already deliver it on their own initial available presence, and live
/// subscription traffic bypasses this cleanup path entirely.
async fn reflush_terminally_released_pending_rows_to_live_resource(
    state: &WebSocketState,
    recipient: &BareJid,
) {
    let blocking_storage: &std::sync::Arc<dyn waddle_xmpp::xep::xep0191::BlockingStorage> =
        &state.deps.protocol.blocking_storage;
    // A same-FullJID replacement can race the owner-gated send below. Retry
    // one fresh target selection in that case: the successor may already have
    // consumed its initial offline-flush CAS, which is exactly why terminal
    // recovery drives this path directly. The bound prevents cleanup from
    // chasing an unbounded stream of reconnects.
    for _ in 0..2 {
        let Some(target) = preferred_live_pending_flush_target(state, recipient) else {
            return;
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
        if target_still_current && terminal_reflush_left_retryable_rows(state, recipient).await {
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
            return;
        }
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
    append_terminal_recovery_backlog(conn, &mut detached, detached_snapshot);
    // Preserve offline FIFO ordering: the live/unacked prefix must reach its
    // Q6 sink before any subsequently accepted channel frame. Once that
    // prefix is settled, channel work is promoted one frame at a time rather
    // than being allowed to grow another terminal replay queue.
    let promote_incrementally = match (can_promote_incrementally, blocklist.as_ref()) {
        (false, _) => false,
        (true, Some(blocklist)) => {
            promote_terminal_recovery_prefix(state, &mut detached, blocklist, &recent_tombstones)
                .await
        }
        (true, None) => false,
    };
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
        detached.unacked_stanzas.push(stanza);
    }
    detached.outbound_count = detached
        .outbound_count
        .wrapping_add(u32::try_from(retained_len).unwrap_or(u32::MAX));
    TerminalRecoverySessionResult {
        session: detached,
        released_pending_rows: drain_outcome.released_pending_rows,
    }
}

fn append_terminal_recovery_backlog(
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
            detached.unacked_stanzas.push(stanza);
        }
        detached.outbound_count = detached
            .outbound_count
            .wrapping_add(u32::try_from(backlog_len).unwrap_or(u32::MAX));
    }
}

async fn promote_terminal_recovery_prefix(
    state: &WebSocketState,
    detached: &mut waddle_xmpp::stream_management::DetachedSession,
    blocklist: &waddle_xmpp::protocol::session_state::Blocklist,
    recent_tombstones: &[waddle_xmpp::stream_management::RecentTombstoneRecord],
) -> bool {
    if detached.unacked_stanzas.is_empty() {
        return true;
    }
    let summary = crate::sm_promotion::promote_session_unacked(
        detached,
        &state.deps.protocol.connection_registry,
        &state.deps.protocol.user_registry,
        &state.deps.protocol.pending_delivery_storage,
        blocklist,
        state.deps.auth_state.xmpp_domain.as_str(),
        recent_tombstones,
    )
    .await;
    crate::sm_promotion::scrub_pending_for_tombstones_recorded_during_promotion(
        &state.deps.protocol.sm_session_registry,
        &state.deps.protocol.pending_delivery_storage,
        recent_tombstones,
        "terminal recovery prefix promotion",
    )
    .await;
    if summary.has_storage_failure() {
        let completed: std::collections::HashSet<u32> =
            summary.promoted_sequences.into_iter().collect();
        detached
            .unacked_stanzas
            .retain(|entry| !completed.contains(&entry.sequence));
        return false;
    }
    detached.unacked_stanzas.clear();
    true
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
    released_terminal_rows: bool,
    terminal_route_removal: TerminalRouteRemoval,
) -> ConnectionShutdownOutcome {
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
    if released_terminal_rows {
        // Preserve FIFO when terminal recovery released an earlier
        // pending_delivery row but still has later unacked traffic to
        // promote: drive the released prefix back onto any live replacement
        // before promotion can enqueue the tail.
        reflush_terminally_released_pending_rows_to_live_resource(state, &jid.to_bare()).await;
    }
    // Keep this stream's claim live throughout every promotion. The claim
    // janitor uses SM liveness/fences, and deferring it before the locally
    // held batch is registered would let a sweep release the fence midway.
    let retrying = if detached_sessions.is_empty() {
        false
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
        conn.sm_state
            .stream_id
            .as_deref()
            .is_some_and(|stream_id| outcome.is_retrying(stream_id))
    };
    if !retrying {
        super::stream_management::defer_superseded_sm_claim(state, &conn.sm_state);
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
            cleanup_muc_presence_with_origin(state, jid, cleanup_origin.as_ref()).await;
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
    cleanup_muc_presence_with_origin(state, jid, None).await
}

async fn cleanup_muc_presence_with_origin(
    state: &WebSocketState,
    jid: &FullJid,
    origin: Option<&crate::server::routes::interpret::OrderedRelayRouteOrigin>,
) -> bool {
    let mut completed = cleanup_remote_muc_presence(state, jid, origin).await;

    let room_jids = match RoomRegistry::wrap(state.deps.protocol.room_registry.clone())
        .list_rooms()
        .await
    {
        Ok(room_jids) => room_jids,
        Err(error) => {
            completed = false;
            warn!(error = %error, "Failed to list room actors");
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
                continue;
            }
        };
        let leave_result = room_actor
            .ask(LeaveByRealJid {
                sender_jid: jid.clone(),
            })
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
        super::muc_call_sfu::unregister_participant_from_room(state, &room_jid, jid);
        match leave_result {
            Ok(Some(outcome)) => {
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
                broadcast_muc_leave_to_remaining(state, &room_jid, jid, &outcome).await;
                broadcast_muc_muji_clear_to_remaining(state, &room_jid, jid, &outcome).await;
                if !maybe_evict_empty_room(state, &room_jid, &outcome).await {
                    completed = false;
                }
            }
            Ok(None) => {}
            Err(error) => {
                completed = false;
                warn!(
                    room = %room_jid,
                    jid = %jid,
                    error = ?error,
                    "Failed to remove disconnected user from MUC room"
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
) -> bool {
    let memberships = state
        .deps
        .protocol
        .remote_muc_memberships
        .take_for_occupant(jid);
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
        MucProxyRouteDecision::Attempted(OrderedRelayMucProxyOutcome::Delivered(_)) => {
            RemoteMucCleanupDisposition::Converged
        }
        MucProxyRouteDecision::Attempted(OrderedRelayMucProxyOutcome::MaybeCommitted)
        | MucProxyRouteDecision::Attempted(OrderedRelayMucProxyOutcome::JoinMaybeCommitted) => {
            RemoteMucCleanupDisposition::UncertainCommit
        }
        MucProxyRouteDecision::Attempted(OrderedRelayMucProxyOutcome::Unavailable)
        | MucProxyRouteDecision::Attempted(OrderedRelayMucProxyOutcome::Dropped)
        | MucProxyRouteDecision::RoomClaimUnavailable
        | MucProxyRouteDecision::OriginUnavailable => RemoteMucCleanupDisposition::RetryableFailure,
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
    match state
        .deps
        .protocol
        .user_registry
        .ask(waddle_xmpp::registry::GetOrCreateUser {
            bare_jid: bare_jid.clone(),
        })
        .await
    {
        Ok(_) => Some(crate::server::routes::interpret::OrderedRelayRouteOrigin {
            kind: crate::server::routes::interpret::OrderedRelayRouteOriginKind::Entity(
                entity.clone(),
            ),
            sender_entity: entity,
            inbound_sequence: 0,
            handoff: None,
        }),
        Err(error) => {
            warn!(
                jid = %jid,
                error = ?error,
                "failed to acquire UserActor claim for remote MUC cleanup"
            );
            None
        }
    }
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
) -> bool {
    true
}

pub(super) async fn cleanup_invalidated_detached_session(
    state: &WebSocketState,
    detached: waddle_xmpp::stream_management::DetachedSession,
    replacement_owner: Option<&std::sync::Arc<std::sync::atomic::AtomicBool>>,
) {
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
    RoomRegistry::wrap(state.deps.protocol.room_registry.clone())
        .get_room(room_jid.clone())
        .await
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
            MucProxyRouteDecision, OrderedRelayMucProxyOutcome::Delivered,
        };

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
                Delivered(Vec::new())
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

        let outcome = room_actor
            .ask(LeaveByRealJid { sender_jid: alice })
            .await
            .expect("leave")
            .expect("outcome present");
        assert!(outcome.removed_last_session);
        assert_eq!(outcome.occupant_count, 0);
        assert!(!outcome.is_persistent);

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

        let outcome = room_actor
            .ask(LeaveByRealJid { sender_jid: alice })
            .await
            .expect("leave")
            .expect("outcome present");
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

        let outcome = room_actor
            .ask(LeaveByRealJid { sender_jid: alice })
            .await
            .expect("leave")
            .expect("outcome present");
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
    use crate::clustering::route_bridge::{MucProxyRouteDecision, OrderedRelayMucProxyOutcome};

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
            MucProxyRouteDecision::Attempted(OrderedRelayMucProxyOutcome::Unavailable),
            MucProxyRouteDecision::Attempted(OrderedRelayMucProxyOutcome::Dropped),
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
            remote_muc_cleanup_disposition(&MucProxyRouteDecision::Attempted(
                OrderedRelayMucProxyOutcome::Delivered(Vec::new()),
            )),
            RemoteMucCleanupDisposition::Converged
        );
        for outcome in [
            OrderedRelayMucProxyOutcome::MaybeCommitted,
            OrderedRelayMucProxyOutcome::JoinMaybeCommitted,
        ] {
            assert_eq!(
                remote_muc_cleanup_disposition(&MucProxyRouteDecision::Attempted(outcome)),
                RemoteMucCleanupDisposition::UncertainCommit
            );
        }
    }
}
