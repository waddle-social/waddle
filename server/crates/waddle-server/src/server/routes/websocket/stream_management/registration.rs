use jid::FullJid;
use std::sync::Arc;
use tracing::warn;
use waddle_xmpp::protocol::ConnectionPhase;
use waddle_xmpp::stream_management::{
    SmClaimCompletion, SmFailed, SmResumed, StreamManagementState,
};

use super::super::{
    cleanup::cleanup_invalidated_detached_session, state::WsConnState, WebSocketState,
};

#[cfg(feature = "clustering")]
async fn unregister_remote_user_resource_if_owner(
    state: &WebSocketState,
    jid: &FullJid,
    owner: &Arc<std::sync::atomic::AtomicBool>,
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
    _owner: &Arc<std::sync::atomic::AtomicBool>,
) {
}

/// Finish the XEP-0198 side effects that become safe only after the resumed
/// or freshly-bound resource has been published to the connection registry.
///
/// `handle_sm_resume` claims the detached session before bind so the old
/// stream cannot be resumed twice, but detached fanout can still append
/// stanzas during the claim-to-registration handoff. This boundary completes
/// that claim under the stream registry lock, returns the typed final
/// XEP-0198 outcome, and invalidates superseded detached sessions for fresh
/// binds.
pub(in crate::server::routes::websocket) async fn finalize_sm_after_registry_registration(
    state: &WebSocketState,
    conn: &mut WsConnState,
    jid: &FullJid,
    owner: &Arc<std::sync::atomic::AtomicBool>,
) -> SmRegistrationFinalization {
    if let Some(stream_id) = conn.pending_resume_stream_id.take() {
        return complete_pending_resume_claim(state, conn, jid, owner, stream_id).await;
    }

    if !conn.phase.is_resumed() {
        invalidate_older_detached_sessions(state, jid, owner).await;
    }

    SmRegistrationFinalization::KeepExistingResponses
}

pub(in crate::server::routes::websocket) enum SmRegistrationFinalization {
    KeepExistingResponses,
    ReplaceWithResumed {
        resumed: SmResumed,
        replay_after_h: u32,
    },
    ReplaceWithFailed(SmFailed),
    ReplaceWithHandledCountTooHigh {
        acknowledged: u32,
        send_count: u32,
    },
}

async fn complete_pending_resume_claim(
    state: &WebSocketState,
    conn: &mut WsConnState,
    jid: &FullJid,
    owner: &Arc<std::sync::atomic::AtomicBool>,
    stream_id: String,
) -> SmRegistrationFinalization {
    let resume_h = conn.pending_resume_h.take();
    let completion = match resume_h {
        Some(h) => {
            state
                .deps
                .protocol
                .sm_session_registry
                .complete_claim_if_resumable(&stream_id, h)
                .await
        }
        None => {
            state
                .deps
                .protocol
                .sm_session_registry
                .complete_claim(&stream_id)
                .await
        }
    };
    let completion_reached_terminal_boundary = completion.is_ok();
    let finalization = match completion {
        Ok(Some(SmClaimCompletion::Resumed(detached))) => match resume_h {
            Some(h) => {
                conn.sm_state.restore_from_session(&detached);
                // The resume `h` acknowledges the mod-2^32 window
                // (detached.last_acked, h] — purge the pending_delivery
                // rows this SM session claimed in exactly that window
                // (review F4). Before the ack window went wrap-aware
                // these rows were swept up by the NEXT live ack's
                // numeric `<= h` delete; a windowed delete anchored at
                // the post-resume `last_acked` would skip them forever,
                // stranding them claimed until the claim-expiry janitor
                // re-released them as duplicates.
                let acked_from_exclusive = detached.last_acked;
                conn.sm_state.acknowledge(h);
                if acked_from_exclusive != h {
                    let session_id =
                        waddle_xmpp::pending_delivery::SmSessionId::new(stream_id.clone());
                    if let Err(_error) = state
                        .deps
                        .protocol
                        .pending_delivery_storage
                        .delete_acked_in_window(&session_id, acked_from_exclusive, h)
                        .await
                    {
                        warn!(
                            session = %session_id,
                            from = acked_from_exclusive,
                            h,
                            failure = "storage",
                            "pending_delivery delete_acked_in_window failed on resume; \
                             rows will be retried on next session via release_claim"
                        );
                    }
                }
                super::observe::observe_sm_resume_finalized(
                    super::observe::SmResumeOutcome::Resumed,
                );
                SmRegistrationFinalization::ReplaceWithResumed {
                    resumed: SmResumed::new(stream_id.clone(), conn.sm_state.get_inbound_count()),
                    replay_after_h: h,
                }
            }
            None => SmRegistrationFinalization::KeepExistingResponses,
        },
        Ok(Some(SmClaimCompletion::ReplayWindowTruncated(detached))) => {
            warn!(
                stream_id = %stream_id,
                jid = %jid,
                client_h = ?resume_h,
                replay_gap_through = ?detached.replay_gap_through,
                "SM resume claim gained a replay gap before completion"
            );
            reset_registered_resume_attempt(state, conn, jid, owner).await;
            super::observe::observe_sm_resume_finalized(super::observe::SmResumeOutcome::ReplayGap);
            SmRegistrationFinalization::ReplaceWithFailed(SmFailed::resume_failed(
                "resource-constraint",
                detached.inbound_count,
            ))
        }
        Ok(Some(SmClaimCompletion::HandledCountTooHigh(detached))) => {
            let acknowledged = resume_h.unwrap_or_default();
            warn!(
                stream_id = %stream_id,
                jid = %jid,
                client_h = acknowledged,
                send_count = detached.outbound_count,
                "SM resume claim completed with handled count too high"
            );
            close_registered_resume_attempt(state, conn, jid, owner).await;
            super::observe::observe_sm_resume_finalized(
                super::observe::SmResumeOutcome::HandledTooHigh,
            );
            SmRegistrationFinalization::ReplaceWithHandledCountTooHigh {
                acknowledged,
                send_count: detached.outbound_count,
            }
        }
        Ok(Some(SmClaimCompletion::Expired(detached))) => {
            warn!(stream_id = %stream_id, jid = %jid, "SM resume claim expired before completion");
            cleanup_invalidated_detached_session(state, detached, Some(owner)).await;
            close_registered_resume_attempt(state, conn, jid, owner).await;
            super::observe::observe_sm_resume_finalized(super::observe::SmResumeOutcome::NotFound);
            SmRegistrationFinalization::ReplaceWithFailed(SmFailed::with_condition(
                "item-not-found",
            ))
        }
        Ok(None) => {
            warn!(stream_id = %stream_id, jid = %jid, "SM resume claim disappeared before completion");
            close_registered_resume_attempt(state, conn, jid, owner).await;
            super::observe::observe_sm_resume_finalized(super::observe::SmResumeOutcome::NotFound);
            SmRegistrationFinalization::ReplaceWithFailed(SmFailed::with_condition(
                "item-not-found",
            ))
        }
        Err(_error) => {
            warn!(stream_id = %stream_id, jid = %jid, failure = "storage", "Failed to complete SM resume claim");
            close_registered_resume_attempt(state, conn, jid, owner).await;
            super::observe::observe_sm_resume_finalized(super::observe::SmResumeOutcome::Storage);
            SmRegistrationFinalization::ReplaceWithFailed(SmFailed::with_condition(
                "internal-server-error",
            ))
        }
    };
    if let Some(guard) = conn.pending_resume_claim.take() {
        if completion_reached_terminal_boundary {
            guard.commit();
        } else {
            drop(guard);
        }
    }
    finalization
}

async fn invalidate_older_detached_sessions(
    state: &WebSocketState,
    jid: &FullJid,
    owner: &Arc<std::sync::atomic::AtomicBool>,
) {
    match state
        .deps
        .protocol
        .sm_session_registry
        .invalidate_sessions_for_jid(jid)
        .await
    {
        Ok(removed) => {
            if removed.is_empty() {
                return;
            }
            // Issue #1097: the superseded sessions' unacked queues run
            // the XEP-0198 §5 promote → confirm chain instead of being
            // dropped. This runs AFTER the fresh bind registered its
            // connection, so the promotion chain's alt-resource step
            // naturally live-delivers to the newly bound resource;
            // otherwise stanzas land in pending delivery storage.
            // Durable SM rows are erased only after promotion succeeds
            // (confirm_drained inside the helper).
            crate::sm_promotion::promote_displaced_sessions(
                removed.clone(),
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
            for detached in removed {
                cleanup_invalidated_detached_session(state, detached, Some(owner)).await;
            }
        }
        Err(_error) => {
            warn!(jid = %jid, failure = "storage", "Failed to invalidate older detached SM sessions for fresh bind");
        }
    }
}

async fn reset_registered_resume_attempt(
    state: &WebSocketState,
    conn: &mut WsConnState,
    jid: &FullJid,
    owner: &Arc<std::sync::atomic::AtomicBool>,
) {
    let removed = state
        .deps
        .protocol
        .connection_registry
        .unregister_if_owner(jid, owner)
        .is_some();
    if removed {
        // ADR-0017 Phase 1: the fresh/resumed bind already mirror-registered
        // this resource into the actor tree, so a resume-rollback that
        // unregisters it from the DashMap must mirror the unregister too or
        // the actor-tree resource leaks. Owner-gated on the same token so a
        // superseding newcomer is not clobbered.
        crate::server::dual_registration::mirror_unregister(
            &state.deps.protocol.user_registry,
            jid,
            Some(Arc::clone(owner)),
        )
        .await;
        unregister_remote_user_resource_if_owner(state, jid, owner).await;
    }
    conn.registry_owner = None;
    conn.phase = ConnectionPhase::authenticated(jid);
    // Replace (never null) the per-connection state machine: the
    // connection loop's keepalive timer arm feeds `Tick` into
    // `conn.state_machine`, and the policy re-arms the adapter's timer
    // wheel on every tick. Dropping the machine to `None` here would
    // let the next tick hit the loop's no-machine guard, which cannot
    // re-arm — permanently disarming dead-peer detection (issue #1090)
    // for whatever remains of this connection. A fresh pre-bind
    // machine restores the session-free semantics this reset wants
    // (an `Unauthenticated` machine drops peer stanzas with a WARN,
    // matching the old `None` guard) while keeping the keepalive
    // clock chain unbroken.
    let domain = state.deps.auth_state.xmpp_domain.clone();
    let keepalive = conn.keepalive_config;
    conn.init_prebind_state_machine(&domain, &state.deps.protocol.dispatcher, keepalive);
    conn.sm_state = StreamManagementState::new();
    conn.blocklist_interested = false;
    conn.suppress_sm_record_next_batch = false;
}

async fn close_registered_resume_attempt(
    state: &WebSocketState,
    conn: &mut WsConnState,
    jid: &FullJid,
    owner: &Arc<std::sync::atomic::AtomicBool>,
) {
    let removed = state
        .deps
        .protocol
        .connection_registry
        .unregister_if_owner(jid, owner)
        .is_some();
    if removed {
        // ADR-0017 Phase 1: mirror the DashMap unregister into the actor
        // tree so the resource the bind mirror-registered does not leak when
        // a resume attempt is closed out. Owner-gated on the same token so a
        // superseding newcomer is not clobbered.
        crate::server::dual_registration::mirror_unregister(
            &state.deps.protocol.user_registry,
            jid,
            Some(Arc::clone(owner)),
        )
        .await;
        unregister_remote_user_resource_if_owner(state, jid, owner).await;
    }
    conn.registry_owner = None;
    conn.phase = ConnectionPhase::closing(Some(jid.clone()));
}
