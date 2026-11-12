//! Teardown, terminal custody and resource replacement for sessions the
//! durable XEP-0198 authority owns.
//!
//! A connection that owns a durable session ends it through the authority:
//! it detaches the session with its resource snapshot, closes it, or leaves
//! it to the generation that superseded it. A terminal session's retained
//! frames are transferred to offline delivery and its custody is released
//! here, by the janitor and by the paths that make a session terminal.

use std::time::Duration;

use waddle_xmpp::ownership::SmSessionKey;
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::persistence::PersistedSession;
use waddle_xmpp::stream_management::{
    DetachedSession, DetachedUnackedStanza, PromotionCaptureCustody,
};

use super::super::replay::load_terminal_recovery_blocklist;
use super::super::stream_management::{selected_store, DurableSmOwner, OwnerRelease};
use super::{
    cleanup_invalidated_detached_session, debug, drain_outbound_into_replay, finish_detached_route,
    mpsc, redrive_terminal_pending_rows_to_live_resource, retire_binding, retire_ingress_stream,
    retire_remote_binding, sm_show_from_name, warn, withdraw_binding, ConnectionPhase,
    ConnectionShutdownOutcome, DetachedRoute, FullJid, OutboundStanza, PendingRowDrainPolicy,
    ReplayDrainSink, WebSocketState, WsConnState,
};
use crate::sm_authority::{DurableSmError, DurableSmStore, LifecycleError, LifecycleRejection};

/// Sessions one janitor sweep expires, and terminal sessions it settles.
const SWEEP_LIMIT: u32 = 128;
/// How long a released terminal session's row is kept before it is purged.
/// It outlasts any operation that could still be resolved against the row.
const RELEASED_TOMBSTONE_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);
/// How long a detach waits for admitted ingress work on the session's stream.
const INGRESS_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Why a terminal session's custody could not be read.
#[derive(Debug, thiserror::Error)]
enum CustodyReadError {
    #[error(transparent)]
    Store(#[from] DurableSmError),
    #[error(transparent)]
    Lifecycle(#[from] LifecycleError),
}

/// What the durable authority left for the caller's own cleanup.
pub(super) enum OwnedSessionTeardown {
    /// The connection's teardown is complete.
    Finished(ConnectionShutdownOutcome),
    /// The connection owns no durable session (any more). The caller cleans
    /// it up as a connection whose stream-management session has ended.
    Continue,
}

/// End this connection's hold on the durable session it owns.
///
/// A resumable session whose transport was lost is detached with its
/// resource snapshot, after every frame the connection recorded without a
/// transport is admitted. A session that ends deliberately, was replaced, or
/// cannot be detached is closed and its retained frames are transferred to
/// offline delivery. A session another generation owns is left alone: this
/// binding removes only its own route.
pub(super) async fn settle_owned_session(
    state: &WebSocketState,
    outbound_rx: &mut mpsc::Receiver<OutboundStanza>,
    conn: &mut WsConnState,
    superseded: bool,
    force_detach_origin: Option<waddle_xmpp::registry::ForceDetachOrigin>,
) -> OwnedSessionTeardown {
    conn.durable_sm.release_unresolved_enable(state).await;
    if conn.durable_sm.is_superseded() {
        conn.durable_sm = Default::default();
        return OwnedSessionTeardown::Finished(leave_session_to_its_owner(state, conn).await);
    }
    let Some(mut owner) = conn.durable_sm.take_owner() else {
        return OwnedSessionTeardown::Continue;
    };
    let replaced_by_fresh_bind =
        force_detach_origin == Some(waddle_xmpp::registry::ForceDetachOrigin::FreshBindReplacement);
    let detachable = conn.sm_state.enabled
        && conn.sm_state.is_resumable()
        && !matches!(conn.phase, ConnectionPhase::Closing { .. })
        && !conn.sm_recovery_required
        && !superseded
        && !replaced_by_fresh_bind;
    let route = conn.phase.cleanup_jid().cloned().and_then(|jid| {
        let owner_token = conn.registry_owner?;
        let entry = state
            .deps
            .protocol
            .connection_registry
            .entry_if_owner(&jid, &owner_token)?;
        Some((jid, owner_token, entry))
    });
    let (Some((jid, owner_token, entry)), true) = (route, detachable) else {
        return end_session(state, conn, &owner).await;
    };

    let presence_state = state.deps.protocol.connection_registry.presence_state(&jid);
    let resource = PersistedSession {
        // The authority stores its own stream id, JID, counters and detach
        // time; these carry the connection's view for completeness.
        stream_id: SmSessionId::new(owner.session().as_uuid().to_string()),
        user_id: conn
            .authenticated_session
            .as_ref()
            .map(|session| session.user_jid.to_string())
            .unwrap_or_else(|| jid.to_bare().to_string()),
        jid: jid.clone(),
        occupancy_session: conn.occupancy_session,
        inbound_count: conn.sm_state.inbound_count(),
        outbound_count: conn.sm_state.outbound_count,
        last_acked: conn.sm_state.last_acked,
        replay_gap_through: None,
        max_resume_time: conn.sm_state.max_resume_time,
        detached_at: chrono::Utc::now(),
        max_resume_duration: Duration::from_secs(u64::from(
            conn.sm_state.max_resume_time.unwrap_or_default(),
        )),
        carbons_enabled: conn.carbons_enabled,
        roster_interested: conn.roster_interested,
        blocklist_interested: conn.blocklist_interested,
        presence_available: entry.is_presence_available(),
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
    };

    // Whatever the endpoint already accepted joins the projection, then the
    // authority's custody, before the session is detached. The relayed
    // ingress obligations these frames discharge are not proven with the
    // detach; their canonical rows stay non-terminal and are redelivered.
    drain_outbound_into_replay(
        state,
        conn.state_machine.as_mut(),
        &mut conn.sm_state,
        conn.authenticated_session.as_ref(),
        outbound_rx,
        ReplayDrainSink {
            detached_stream_id: None,
            bound_resource: Some(&jid),
            pending_row_policy: PendingRowDrainPolicy::PreserveForReplay,
            drained_appends: &mut Vec::new(),
        },
    )
    .await;
    let stream_id = SmSessionId::new(owner.session().as_uuid().to_string());
    if !state
        .deps
        .protocol
        .ingress
        .wait_for_stream_idle(&stream_id, INGRESS_IDLE_TIMEOUT)
        .await
    {
        warn!(
            jid = %jid,
            %stream_id,
            "refusing resumable SM handoff with unfinished ingress authority work"
        );
        return end_session(state, conn, &owner).await;
    }
    let tail_admitted = owner.admit_unwritten_tail(&conn.sm_state).await;
    match owner.detach(&conn.sm_state, resource).await {
        OwnerRelease::Committed => {
            if withdraw_binding(state, &jid, &owner_token).is_none() {
                // A newer bind of the resource holds the route. It replaces
                // the resource's sessions, this one included.
                debug!(jid = %jid, %stream_id, "detached durable SM session lost its route to a newer bind");
                return OwnedSessionTeardown::Finished(ConnectionShutdownOutcome::NotPersisted);
            }
            let outcome = finish_detached_route(
                state,
                conn,
                outbound_rx,
                DetachedRoute {
                    jid: jid.clone(),
                    owner: &owner_token,
                    force_detach_origin,
                    detached_stream_id: None,
                    stream_label: stream_id.as_str(),
                },
            )
            .await;
            // Frames the endpoint accepted between the detach and its
            // removal from the registry are retained on the detached
            // authority for the next resume.
            let late_admitted = owner.admit_detached_tail(&conn.sm_state).await;
            OwnedSessionTeardown::Finished(if tail_admitted && late_admitted {
                outcome
            } else {
                // Some recorded frames are in no custody. Their producers
                // must keep their own retry.
                warn!(jid = %jid, %stream_id, "durable SM session detached without every recorded frame in custody");
                ConnectionShutdownOutcome::NotPersisted
            })
        }
        OwnerRelease::Superseded => {
            OwnedSessionTeardown::Finished(leave_session_to_its_owner(state, conn).await)
        }
        OwnerRelease::Ended | OwnerRelease::Unavailable => {
            // The session cannot be handed to a resume from here. The
            // connection is cleaned up as one that closed its stream.
            hand_projection_to_caller(conn, &owner);
            conn.phase = ConnectionPhase::closing(Some(jid));
            OwnedSessionTeardown::Continue
        }
    }
}

/// Close the owner's session and transfer what it retained.
async fn end_session(
    state: &WebSocketState,
    conn: &mut WsConnState,
    owner: &DurableSmOwner,
) -> OwnedSessionTeardown {
    match owner.close().await {
        OwnerRelease::Committed => {
            // Transferred now so that a binding replacing this resource
            // finds the frames in offline delivery when it flushes. The
            // settlement retires the session's ingress stream and claim, so
            // it waits for admitted ingress work; a stream that stays busy
            // is settled by the janitor instead.
            let stream_id = SmSessionId::new(owner.session().as_uuid().to_string());
            if state
                .deps
                .protocol
                .ingress
                .wait_for_stream_idle(&stream_id, INGRESS_IDLE_TIMEOUT)
                .await
            {
                settle_terminal_session(state, owner.store(), owner.session()).await;
            }
        }
        OwnerRelease::Superseded => {
            return OwnedSessionTeardown::Finished(leave_session_to_its_owner(state, conn).await);
        }
        // Terminal already: the janitor, or the bind that replaced the
        // resource, settles its custody.
        OwnerRelease::Ended => {}
        OwnerRelease::Unavailable => {
            warn!(session = ?owner.session(), "durable SM session could not be closed at teardown; it keeps a departed owner");
        }
    }
    hand_projection_to_caller(conn, owner);
    OwnedSessionTeardown::Continue
}

/// Drop from the connection's queue the frames the authority retains, so
/// that the caller's cleanup handles only frames that were never admitted.
fn hand_projection_to_caller(conn: &mut WsConnState, owner: &DurableSmOwner) {
    conn.sm_state.acknowledge(owner.admitted_through());
}

/// Remove this binding's own route and nothing else. The session, its
/// occupancy, its ingress stream and its claim belong to the generation
/// that owns it now.
async fn leave_session_to_its_owner(
    state: &WebSocketState,
    conn: &mut WsConnState,
) -> ConnectionShutdownOutcome {
    if let (Some(jid), Some(owner)) = (conn.phase.cleanup_jid().cloned(), conn.registry_owner) {
        if withdraw_binding(state, &jid, &owner).is_some() {
            retire_binding(state, &jid, &owner).await;
            retire_remote_binding(state, &jid, &owner).await;
        }
    }
    ConnectionShutdownOutcome::NotPersisted
}

/// Transfer a terminal session's retained frames to offline delivery, clean
/// up the resource it left detached, and release its custody.
///
/// The frames go through the same promotion chain an expired in-memory
/// session uses. Custody is released only when every frame was handled, so a
/// failed transfer is repeated by the next sweep; a transfer repeated after a
/// failed release may deliver a frame twice. Returns whether the session's
/// custody was released.
pub(super) async fn settle_terminal_session(
    state: &WebSocketState,
    store: &DurableSmStore,
    session: SmSessionKey,
) -> bool {
    let stream_id = SmSessionId::new(session.as_uuid().to_string());
    let claim = state
        .deps
        .protocol
        .sm_session_registry
        .current_sm_claim_fence(stream_id.as_str());
    if store.is_clustered() && claim.is_none() {
        // Another node holds the session's claim and settles it. A transfer
        // here could not be followed by the release that ends it.
        return false;
    }
    let custody = async {
        let Some(record) = store.session(session).await? else {
            return Ok(None);
        };
        let resource = store.terminal_resource(session).await?;
        let frames = store.terminal_frames(session).await?;
        Ok::<_, CustodyReadError>(Some((record, resource, frames)))
    };
    let (record, resource, frames) = match custody.await {
        Ok(Some(custody)) => custody,
        Ok(None) => return false,
        Err(error) => {
            warn!(%stream_id, %error, "terminal durable SM session custody could not be read");
            return false;
        }
    };
    let detached = DetachedSession {
        promotion_custody: PromotionCaptureCustody::never_claimed(),
        stream_id: stream_id.to_string(),
        user_id: record.full_jid.to_bare().to_string(),
        jid: record.full_jid.clone(),
        occupancy_session: resource.as_ref().map_or_else(
            waddle_xmpp_core::OccupancySessionGeneration::mint,
            |resource| resource.occupancy_session,
        ),
        inbound_count: record.inbound_handled as u32,
        outbound_count: record.window.appended().wire(),
        last_acked: record.window.acked().wire(),
        replay_gap_through: None,
        unacked_stanzas: frames
            .into_iter()
            .map(|retained| DetachedUnackedStanza {
                provenance: retained.frame.provenance,
                ingress_receipts: retained.frame.ingress_receipts,
                sequence: retained.frame.sequence,
                stanza_xml: retained.frame.stanza.wire().to_owned(),
                original_receipt_at: retained.frame.original_receipt_at,
            })
            .collect(),
        max_resume_time: Some(0),
        detached_at: std::time::Instant::now(),
        carbons_enabled: resource
            .as_ref()
            .is_some_and(|resource| resource.carbons_enabled),
        roster_interested: resource
            .as_ref()
            .is_some_and(|resource| resource.roster_interested),
        blocklist_interested: resource
            .as_ref()
            .is_some_and(|resource| resource.blocklist_interested),
        presence_available: resource
            .as_ref()
            .is_some_and(|resource| resource.presence_available),
        presence_show: resource
            .as_ref()
            .and_then(|resource| resource.presence_show.clone()),
        presence_status: resource
            .as_ref()
            .and_then(|resource| resource.presence_status.clone()),
        presence_priority: resource
            .as_ref()
            .map_or(0, |resource| resource.presence_priority),
        presence_payloads: resource
            .as_ref()
            .map(|resource| resource.presence_payloads.clone())
            .unwrap_or_default(),
        pending_subscribes_flushed: false,
    };

    if !detached.unacked_stanzas.is_empty() {
        let recipient = record.full_jid.to_bare();
        let blocklist = match load_terminal_recovery_blocklist(state, &recipient).await {
            Ok(blocklist) => blocklist,
            Err(error) => {
                warn!(%stream_id, %error, "terminal durable SM transfer deferred: blocklist unavailable");
                return false;
            }
        };
        let Ok(tombstones) = crate::sm_promotion::recent_tombstones_for_promotion(
            &state.deps.protocol.sm_session_registry,
            "durable SM terminal transfer",
        ) else {
            return false;
        };
        let summary = crate::sm_promotion::promote_session_unacked(
            &detached,
            &state.deps.protocol.connection_registry,
            &state.deps.protocol.user_registry,
            &state.deps.protocol.pending_delivery_storage,
            &blocklist,
            state.deps.auth_state.xmpp_domain.as_str(),
            &tombstones,
        )
        .await;
        if summary.storage_failed > 0 {
            warn!(
                %stream_id,
                failed = summary.storage_failed,
                "terminal durable SM transfer incomplete; custody retained for the next sweep"
            );
            return false;
        }
        if summary.queued > 0 {
            // A replacement that bound during the transfer may have spent
            // its offline flush before these rows existed.
            redrive_terminal_pending_rows_to_live_resource(state, &recipient).await;
        }
    }

    if let Err(error) = store
        .release_terminal(session, claim.as_ref(), chrono::Utc::now())
        .await
    {
        warn!(%stream_id, %error, "terminal durable SM custody could not be released");
        return false;
    }
    if let Err(error) = state
        .deps
        .protocol
        .pending_delivery_storage
        .confirm_promotion(&stream_id)
        .await
    {
        warn!(%stream_id, %error, "terminal durable SM transfer receipts were not confirmed");
    }
    if resource.is_some() {
        // The session ended while detached: its resource is still shown as
        // present and still occupies its rooms.
        cleanup_invalidated_detached_session(state, detached, None).await;
    } else {
        retire_ingress_stream(state, &stream_id).await;
    }
    state
        .deps
        .protocol
        .sm_session_registry
        .release_attached_session_claim(&stream_id)
        .await;
    true
}

/// One pass of the durable session janitor: expire detached sessions whose
/// resume window elapsed, transfer and release every terminal session that
/// still holds custody, and purge released tombstones. Does nothing when the
/// durable authority is not selected.
pub(crate) async fn run_durable_sm_sweep(state: &WebSocketState) {
    let Some(store) = selected_store(state) else {
        return;
    };
    let now = chrono::Utc::now();
    match store.due_for_expiry(now, SWEEP_LIMIT).await {
        Ok(due) => {
            for session in due {
                let claim = state
                    .deps
                    .protocol
                    .sm_session_registry
                    .current_sm_claim_fence(&session.as_uuid().to_string());
                match store.expire(session, claim.as_ref(), now).await {
                    Ok(_) => {}
                    // A resume committed first; the session is not due.
                    Err(LifecycleError::Rejected(
                        LifecycleRejection::NotDue | LifecycleRejection::Transition(_),
                    )) => {}
                    Err(error) => {
                        warn!(session = ?session, %error, "durable SM session could not be expired");
                    }
                }
            }
        }
        Err(error) => warn!(%error, "durable SM expiry scan failed"),
    }
    match store.terminal_with_custody(SWEEP_LIMIT).await {
        Ok(sessions) => {
            for session in sessions {
                settle_terminal_session(state, store, session).await;
            }
        }
        Err(error) => warn!(%error, "durable SM terminal custody scan failed"),
    }
    let Some(purge_before) = chrono::Duration::from_std(RELEASED_TOMBSTONE_RETENTION)
        .ok()
        .and_then(|retention| now.checked_sub_signed(retention))
    else {
        return;
    };
    if let Err(error) = store.purge_terminal(purge_before).await {
        warn!(%error, "durable SM tombstone purge failed");
    }
}

/// End every durable session of `resource` because the resource is being
/// bound again, and transfer what those sessions retained so the new binding
/// receives it from offline delivery.
///
/// A clustered store refuses the replacement: the binder holds the account's
/// claim, not each session's. The sessions are then left as they are, which
/// is the behaviour without a durable authority for a session this node does
/// not hold: the occupancy authority retires their generation at this bind,
/// a later resume of one is refused for that reason, and each expires through
/// its own resume window.
pub(crate) async fn replace_durable_resource_sessions(state: &WebSocketState, resource: &FullJid) {
    let Some(store) = selected_store(state) else {
        return;
    };
    match store
        .replace_resource_sessions(resource, chrono::Utc::now())
        .await
    {
        Ok(replaced) => {
            for terminal in replaced {
                settle_terminal_session(state, store, terminal.session).await;
            }
        }
        Err(LifecycleError::Rejected(LifecycleRejection::Store(DurableSmError::Unsupported {
            operation,
        }))) => {
            debug!(
                jid = %resource,
                operation,
                "durable SM sessions of a rebound resource are left to expire"
            );
        }
        Err(error) => {
            warn!(jid = %resource, %error, "durable SM sessions of a rebound resource were not replaced");
        }
    }
}
