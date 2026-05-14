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
    let mut remove_resumable_sidecar = true;
    let finalization = match completion {
        Ok(Some(SmClaimCompletion::Resumed(detached))) => match resume_h {
            Some(h) => {
                conn.sm_state.restore_from_session(&detached);
                conn.sm_state.acknowledge(h);
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
            reset_registered_resume_attempt(state, conn, jid, owner);
            remove_resumable_sidecar = false;
            SmRegistrationFinalization::ReplaceWithFailed(SmFailed::resume_failed(
                "resource-constraint",
                detached.inbound_count,
            ))
        }
        Ok(Some(SmClaimCompletion::Expired(detached))) => {
            warn!(stream_id = %stream_id, jid = %jid, "SM resume claim expired before completion");
            cleanup_invalidated_detached_session(state, detached, Some(owner)).await;
            close_registered_resume_attempt(state, conn, jid, owner);
            SmRegistrationFinalization::ReplaceWithFailed(SmFailed::with_condition(
                "item-not-found",
            ))
        }
        Ok(None) => {
            warn!(stream_id = %stream_id, jid = %jid, "SM resume claim disappeared before completion");
            close_registered_resume_attempt(state, conn, jid, owner);
            SmRegistrationFinalization::ReplaceWithFailed(SmFailed::with_condition(
                "item-not-found",
            ))
        }
        Err(error) => {
            warn!(stream_id = %stream_id, jid = %jid, error = %error, "Failed to complete SM resume claim");
            close_registered_resume_attempt(state, conn, jid, owner);
            SmRegistrationFinalization::ReplaceWithFailed(SmFailed::with_condition(
                "internal-server-error",
            ))
        }
    };
    if remove_resumable_sidecar {
        state.deps.protocol.resumable_sessions.remove(&stream_id);
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
            for detached in removed {
                cleanup_invalidated_detached_session(state, detached, Some(owner)).await;
            }
        }
        Err(error) => {
            warn!(jid = %jid, error = %error, "Failed to invalidate older detached SM sessions for fresh bind");
        }
    }
}

fn reset_registered_resume_attempt(
    state: &WebSocketState,
    conn: &mut WsConnState,
    jid: &FullJid,
    owner: &Arc<std::sync::atomic::AtomicBool>,
) {
    let _ = state
        .deps
        .protocol
        .connection_registry
        .unregister_if_owner(jid, owner);
    conn.registry_owner = None;
    conn.phase = ConnectionPhase::authenticated(jid);
    conn.state_machine = None;
    conn.sm_state = StreamManagementState::new();
    conn.suppress_sm_record_next_batch = false;
}

fn close_registered_resume_attempt(
    state: &WebSocketState,
    conn: &mut WsConnState,
    jid: &FullJid,
    owner: &Arc<std::sync::atomic::AtomicBool>,
) {
    let _ = state
        .deps
        .protocol
        .connection_registry
        .unregister_if_owner(jid, owner);
    conn.registry_owner = None;
    conn.phase = ConnectionPhase::closing(Some(jid.clone()));
}
