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

/// Publish the SM stream id onto the freshly-registered entry so the
/// offline-flush path keys claims by the XEP-0198 session id, not the
/// resource JID. For a fresh bind without SM enabled, sm_state.stream_id is
/// None and the flush path falls back to delete-on-push for non-SM sessions.
///
/// This — and the presence publication below — run BEFORE the authoritative
/// mirror `ask`, not after (concurrency review, Slice 0): the mirror is a
/// blocking `ask` bounded at 2s, and leaving it between the DashMap
/// `register` and these mutations would leave the just-bound resource
/// registered-but-unavailable (and stream-id-less) on the authoritative
/// routing map for the whole ask window. On an SM resume that window would
/// hide a live resource from RFC 6121 §8.5.2.1.1 bare-JID selection. Setting
/// presence/stream-id first closes it; the actor shares this same
/// `Arc`-backed entry, so it still observes these atomics.
pub(super) fn publish_stream_id_and_presence(
    state: &WebSocketState,
    jid: &FullJid,
    owner: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    conn: &WsConnState,
) {
    // Owner-gated: a racing same-JID replacement can take the slot between
    // `register_with_stream_state` and this publication; a stale-owner write
    // would stamp OUR stream id and restored presence onto the replacement's
    // entry / the JID-keyed presence map.
    if let Some(entry) = state
        .deps
        .protocol
        .connection_registry
        .entry_if_owner(jid, owner)
    {
        entry.set_sm_stream_id(
            conn.sm_state
                .stream_id
                .clone()
                .map(waddle_xmpp::pending_delivery::SmSessionId::new),
        );
    }

    // Each write re-verifies ownership INSIDE the registry call: a separate
    // `entry_if_owner(...).is_some()` check ahead of ungated writes would let
    // a same-JID replacement that registers between check and write inherit
    // OUR availability/presence. The owner-gated variants hold the entry
    // guard across check and write, so a refused write means the replacement
    // owns the slot and its own initial presence supersedes ours.
    if conn.presence_available {
        let registry = &state.deps.protocol.connection_registry;
        registry.update_presence_if_owner(jid, owner, true, conn.presence_priority);
        registry.update_presence_state_if_owner(
            jid,
            owner,
            conn.presence_show
                .as_ref()
                .map(sm_show_name)
                .map(str::to_string),
            conn.presence_status.clone(),
            conn.presence_priority,
            // XEP-0198 resume restores the full last presence,
            // extension payloads included (XEP-0115 caps, XEP-0319
            // idle) — RFC 6121 §4.3.2 requires probe responses to
            // reproduce the complete stanza, and the client sends no
            // new presence after <resumed/> (#1103 follow-up).
            conn.presence_payloads.clone(),
        );
    }
}

pub(super) fn rollback_registration_if_self_fenced(
    state: &WebSocketState,
    jid: &FullJid,
    owner: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    admission: Option<crate::clustering::ClusteringAdmissionToken>,
) -> bool {
    if admission.is_some_and(|token| state.deps.app_state.clustering_readiness.admits(token)) {
        return false;
    }
    state
        .deps
        .protocol
        .connection_registry
        .unregister_if_owner(jid, owner);
    true
}

fn clustering_admission_is_current(state: &WebSocketState, conn: &WsConnState) -> bool {
    conn.clustering_admission
        .is_some_and(|token| state.deps.app_state.clustering_readiness.admits(token))
}

pub(super) async fn rollback_authoritative_registration(
    state: &WebSocketState,
    jid: &FullJid,
    owner: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    state
        .deps
        .protocol
        .connection_registry
        .unregister_if_owner(jid, owner);
    tokio::join!(
        crate::server::dual_registration::mirror_unregister(
            &state.deps.protocol.user_registry,
            jid,
            Some(owner.clone()),
        ),
        super::cleanup::unregister_remote_user_resource_if_owner(state, jid, owner),
    );
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
    if !clustering_admission_is_current(state, conn) {
        warn!(
            jid = %jid,
            "Refusing XMPP resource registration while the cluster node is self-fenced"
        );
        return RegistrationAfterFrame::SessionInitializationFailed;
    }
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

    // Construct the physical entry before publishing it. In cluster mode the
    // bridge first reserves the one durable admission epoch shared by local-
    // owner and remote-owner sockets; only then may either registry observe
    // this full JID.
    let entry = waddle_xmpp::registry::ConnectionEntry::new(tx);
    entry
        .carbons_enabled
        .store(conn.carbons_enabled, std::sync::atomic::Ordering::Relaxed);
    entry
        .roster_interested
        .store(conn.roster_interested, std::sync::atomic::Ordering::Relaxed);
    entry.blocklist_interested.store(
        conn.blocklist_interested,
        std::sync::atomic::Ordering::Relaxed,
    );
    let owner = entry.carbons_handle();
    if let Some(retirement_handle) = conn.retirement_handle.clone() {
        if !entry.install_retirement_handle(retirement_handle) {
            error!(jid = %jid, "Connection retirement handle was already installed");
            return RegistrationAfterFrame::SessionInitializationFailed;
        }
    }

    #[cfg(feature = "clustering")]
    let mut physical_guard = {
        let bridge = state
            .deps
            .app_state
            .clustering_claims
            .ordered_relay_delivery_bridge
            .as_ref();
        match bridge {
            Some(bridge) => match bridge
                .begin_physical_user_resource(&jid, owner.clone())
                .await
            {
                Ok(guard) => Some(guard),
                Err(_) => return RegistrationAfterFrame::SessionInitializationFailed,
            },
            None => None,
        }
    };

    let published_owner = state
        .deps
        .protocol
        .connection_registry
        .register_entry(jid.clone(), entry.clone());
    debug_assert!(std::sync::Arc::ptr_eq(&published_owner, &owner));
    conn.registry_owner = Some(owner.clone());

    // Close the race where fencing begins after the pre-registration check
    // but before publication. Since readiness flips before the terminal
    // ConnectionRegistry snapshot, either that snapshot sees this entry or
    // this owner-gated rollback removes it here.
    if rollback_registration_if_self_fenced(state, &jid, &owner, conn.clustering_admission) {
        #[cfg(feature = "clustering")]
        abort_physical_registration(state, &mut physical_guard).await;
        conn.registry_owner = None;
        warn!(
            jid = %jid,
            "Rolled back XMPP resource registration because the cluster node self-fenced"
        );
        return RegistrationAfterFrame::SessionInitializationFailed;
    }

    // Publish the SM stream id + restored presence onto the
    // freshly-registered entry, owner-gated against a racing same-JID
    // replacement (#1139 resume-claim clobber fix — see
    // `publish_stream_id_and_presence`'s doc comment for the full
    // "run before the authoritative mirror ask" rationale).
    publish_stream_id_and_presence(state, &jid, &owner, conn);

    if resumed && conn.pending_subscribes_flushed {
        // XEP-0198 §5: a resumed stream is the SAME session. The
        // detached session recorded that its once-per-session
        // pending-subscribe delivery already fired (RFC 6121 §3.1.3),
        // so consume the fresh entry's claim — a presence flip after
        // resume must not re-prompt (#1104 follow-up). Gated on the
        // carried flag, NOT on current presence: a session that went
        // available (claim consumed) then unavailable before detaching
        // must not re-arm the claim on resume. Subscribes that arrived
        // during the detached window were fanned out to the detached
        // session and reach the client via SM replay instead.
        // Owner-gated: a racing same-JID replacement must not have ITS
        // once-per-session claim consumed by this resume's pre-claim.
        if let Some(entry) = state
            .deps
            .protocol
            .connection_registry
            .entry_if_owner(&jid, &owner)
        {
            let _ = entry.claim_pending_subscribes_flush();
        }
    }

    // ADR-0017 Phase 1 completion: registration into the actor tree is
    // authoritative and fail-closed. We share the SAME `Arc`-backed
    // `ConnectionEntry` we just registered (obtained owner-gated via
    // `entry_if_owner`, now carrying the presence/stream-id set above) so the
    // actor sees live presence/carbons updates without per-site mirroring. If
    // the `UserActor` cannot confirm the resource, roll the DashMap
    // registration back (owner-gated, so a racing replacement is left intact)
    // and fail the bind: a lagging register is a *silent false-negative* — a
    // bare-JID selection that misses this live resource looks like a complete
    // set and can never fall back — so the two views must never disagree in the
    // miss-a-resource direction. The client simply reconnects. See
    // docs/adrs/0017-phase1-completion-authoritative-registration.md.
    //
    // The `None` arm (our slot was already superseded by a racing
    // reconnect on the same resource) preserves prior behaviour: the
    // replacement owns the slot and performs its own authoritative register,
    // so there is nothing for this session to mirror.
    if let Some(entry) = state
        .deps
        .protocol
        .connection_registry
        .entry_if_owner(&jid, &owner)
    {
        #[cfg(feature = "clustering")]
        let registered = if physical_guard.is_some() {
            publish_clustered_physical_resource(state, physical_guard.as_ref(), entry.clone()).await
        } else {
            matches!(
                crate::server::dual_registration::mirror_register_outcome(
                    &state.deps.protocol.user_registry,
                    jid.clone(),
                    entry.clone(),
                )
                .await,
                crate::server::dual_registration::MirrorRegisterOutcome::Registered
            )
        };
        #[cfg(not(feature = "clustering"))]
        let registered = matches!(
            crate::server::dual_registration::mirror_register_outcome(
                &state.deps.protocol.user_registry,
                jid.clone(),
                entry.clone(),
            )
            .await,
            crate::server::dual_registration::MirrorRegisterOutcome::Registered
        );
        if !registered {
            // kameo's reply_timeout does not cancel an already-enqueued
            // handler, so a register that *timed out* may still land in the
            // actor tree after this rollback. Reap it with an owner-gated
            // unregister: the `UserRegistryActor` mailbox is FIFO, so this is
            // ordered after the register ask and prunes the phantom if it ran
            // late; owner-gating leaves a racing replacement untouched.
            #[cfg(feature = "clustering")]
            abort_physical_registration(state, &mut physical_guard).await;
            rollback_authoritative_registration(state, &jid, &owner).await;
            conn.registry_owner = None;
            return RegistrationAfterFrame::SessionInitializationFailed;
        }
    }

    if !clustering_admission_is_current(state, conn) {
        #[cfg(feature = "clustering")]
        abort_physical_registration(state, &mut physical_guard).await;
        rollback_authoritative_registration(state, &jid, &owner).await;
        conn.registry_owner = None;
        warn!(jid = %jid, "Rolled back authoritative registration after node self-fence");
        return RegistrationAfterFrame::SessionInitializationFailed;
    }

    let sm_finalization = finalize_sm_after_registry_registration(state, conn, &jid, &owner).await;
    if !clustering_admission_is_current(state, conn) {
        #[cfg(feature = "clustering")]
        abort_physical_registration(state, &mut physical_guard).await;
        rollback_authoritative_registration(state, &jid, &owner).await;
        conn.registry_owner = None;
        warn!(jid = %jid, "Rolled back finalized session after node self-fence");
        return RegistrationAfterFrame::SessionInitializationFailed;
    }
    #[cfg(feature = "clustering")]
    if let Some(guard) = physical_guard.take() {
        let Some(bridge) = state
            .deps
            .app_state
            .clustering_claims
            .ordered_relay_delivery_bridge
            .as_ref()
        else {
            rollback_authoritative_registration(state, &jid, &owner).await;
            conn.registry_owner = None;
            return RegistrationAfterFrame::SessionInitializationFailed;
        };
        let Some(token) = bridge.finalize_physical_user_resource(guard).await else {
            rollback_authoritative_registration(state, &jid, &owner).await;
            conn.registry_owner = None;
            return RegistrationAfterFrame::SessionInitializationFailed;
        };
        conn.physical_resource_admission = Some(token);
    }
    info!(
        jid = %jid,
        resumed = conn.phase.is_resumed(),
        carbons_enabled = conn.carbons_enabled,
        "WebSocket connection registered"
    );

    RegistrationAfterFrame::Registered(sm_finalization)
}

#[cfg(feature = "clustering")]
async fn publish_clustered_physical_resource(
    state: &WebSocketState,
    guard: Option<&crate::clustering::route_bridge::PhysicalResourceRegistrationGuard>,
    entry: waddle_xmpp::registry::ConnectionEntry,
) -> bool {
    let Some(bridge) = state
        .deps
        .app_state
        .clustering_claims
        .ordered_relay_delivery_bridge
        .as_ref()
    else {
        return false;
    };
    let Some(guard) = guard else {
        return false;
    };
    match bridge.publish_physical_user_resource(guard, entry).await {
        crate::clustering::route_bridge::RemoteResourceRegisterOutcome::Registered => true,
        crate::clustering::route_bridge::RemoteResourceRegisterOutcome::NotRemote
        | crate::clustering::route_bridge::RemoteResourceRegisterOutcome::Failed => false,
    }
}

#[cfg(feature = "clustering")]
async fn abort_physical_registration(
    state: &WebSocketState,
    guard: &mut Option<crate::clustering::route_bridge::PhysicalResourceRegistrationGuard>,
) {
    let Some(guard) = guard.take() else {
        return;
    };
    if let Some(bridge) = state
        .deps
        .app_state
        .clustering_claims
        .ordered_relay_delivery_bridge
        .as_ref()
    {
        bridge.abort_physical_user_resource(guard).await;
    }
}
