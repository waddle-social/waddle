use super::*;
use super::{
    session_init::load_blocklist_for_bind,
    state::WsConnState,
    stream_management::{
        finalize_sm_after_registry_registration, sm_show_name, SmRegistrationFinalization,
    },
};

pub(super) enum RegistrationAfterFrame {
    Unchanged,
    Registered(SmRegistrationFinalization),
    SessionInitializationFailed,
}

pub(super) async fn register_bound_connection_after_frame(
    state: &WebSocketState,
    domain: &str,
    conn: &mut WsConnState,
    pending_tx: &mut Option<mpsc::Sender<OutboundStanza>>,
) -> RegistrationAfterFrame {
    let Some(jid) = conn.phase.bound_jid().cloned() else {
        return RegistrationAfterFrame::Unchanged;
    };
    let Some(tx) = pending_tx.take() else {
        return RegistrationAfterFrame::Unchanged;
    };

    // Mirror the bind transition into the per-connection state machine (#229
    // PR11). The SM stays `None` until here so unauthenticated traffic can't
    // reach `on_peer_stanza`. We detect SM-resume vs fresh bind from whether
    // `pending_resume_stream_id` is waiting to be consumed below.
    let resumed = conn.pending_resume_stream_id.is_some();

    let blocklist = if resumed {
        // XEP-0198 resume: the previous session was detached/dropped, but we
        // deliberately do NOT re-read from DB. Re-reading would let blocklist
        // mutations from other resources during the detach window leak into
        // the resumed stream, contradicting the snapshot semantic. The resumed
        // session starts with an empty snapshot; subsequent XEP-0191 IQ-set
        // traffic on the resumed stream re-populates it via the SM's internal
        // blocklist mutators.
        Blocklist::empty()
    } else {
        match load_blocklist_for_bind(&state.deps.app_state.db_pool, &jid).await {
            Ok(blocklist) => blocklist,
            Err(error) => {
                error!(
                    jid = %jid,
                    %error,
                    "Failed to load XEP-0191 blocklist at bind; failing the bind to avoid a \
                     session-long fail-open. Client should reconnect."
                );
                return RegistrationAfterFrame::SessionInitializationFailed;
            }
        }
    };

    conn.ensure_state_machine(
        domain,
        &state.deps.protocol.dispatcher,
        jid.clone(),
        resumed,
        blocklist,
    );

    // ADR-0017 Phase 1 dual-registration: mirror this resource into the
    // actor-backed `user_registry` before the sender is moved into the
    // DashMap registry below. Clone the sender for the actor mirror.
    let tx_for_actor = tx.clone();

    let owner = state
        .deps
        .protocol
        .connection_registry
        .register_with_stream_state(
            jid.clone(),
            tx,
            conn.carbons_enabled,
            conn.roster_interested,
            conn.blocklist_interested,
        );
    conn.registry_owner = Some(owner.clone());

    // Best-effort mirror into the actor tree (nothing reads it for delivery
    // yet; the DashMap registration above is authoritative).
    crate::server::dual_registration::mirror_register(
        &state.deps.protocol.user_registry,
        jid.clone(),
        tx_for_actor,
        conn.carbons_enabled,
    )
    .await;

    // Publish the SM stream id onto the freshly-registered entry so the
    // offline-flush path keys claims by the XEP-0198 session id, not the
    // resource JID. For a fresh bind without SM enabled, sm_state.stream_id is
    // None and the flush path falls back to delete-on-push for non-SM sessions.
    if let Some(entry) = state.deps.protocol.connection_registry.get_entry(&jid) {
        entry.set_sm_stream_id(
            conn.sm_state
                .stream_id
                .clone()
                .map(waddle_xmpp::pending_delivery::SmSessionId::new),
        );
    }

    if conn.presence_available {
        state
            .deps
            .protocol
            .connection_registry
            .update_presence(&jid, true, conn.presence_priority);
        state
            .deps
            .protocol
            .connection_registry
            .update_presence_state(
                &jid,
                conn.presence_show
                    .as_ref()
                    .map(sm_show_name)
                    .map(str::to_string),
                conn.presence_status.clone(),
                conn.presence_priority,
                // XEP-0198 resume restores show/status/priority; idle is not
                // carried in SM session state yet (deferred), so a probe during
                // a detached window omits the stamp until the next live update.
                None,
            );
    }

    let sm_finalization = finalize_sm_after_registry_registration(state, conn, &jid, &owner).await;
    info!(
        jid = %jid,
        resumed = conn.phase.is_resumed(),
        carbons_enabled = conn.carbons_enabled,
        "WebSocket connection registered"
    );

    RegistrationAfterFrame::Registered(sm_finalization)
}
