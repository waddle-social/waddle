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

/// Single choke point for a failed post-auth session initialization (#1454).
///
/// Every `SessionInitializationFailed` return routes through here so the
/// client-visible stream `<internal-server-error/>` is never server-silent
/// again: one `error!` with the bare JID, the XEP-0198 stream id when one
/// exists (fresh binds enable SM only after bind, so it is empty there),
/// the typed reason, and the failure detail; the
/// `waddle.session.init.failed` counter keyed by that reason so the rate is
/// alertable; and a dedicated error-status span. The span is created here —
/// not recorded on `Span::current()` — because registration runs after
/// `handle_xmpp_frame` returns, outside `xmpp.stanza.dispatch`, where a
/// current-span mark would be a silent no-op; a short point-in-time root
/// span is exactly the #1428 "internal failures export as error spans"
/// shape. The bare JID lives on the log and span only — never as a metric
/// attribute.
pub(super) fn record_session_init_failure(
    reason: waddle_xmpp::telemetry::attributes::SessionInitFailureReason,
    jid: &FullJid,
    stream_id: Option<&str>,
    detail: Option<String>,
) {
    use waddle_xmpp::telemetry::attributes::MetricAttribute;
    error!(
        user = %jid.to_bare(),
        resource = %jid.resource(),
        // Logged as an Option so a fresh bind (no SM yet → None) stays
        // distinguishable from a genuinely empty id.
        stream_id = ?stream_id,
        reason = reason.value(),
        detail = detail.as_deref().unwrap_or(""),
        "session initialization failed; sending stream internal-server-error and closing"
    );
    waddle_xmpp::counter_add!(
        "waddle.session.init.failed",
        "1",
        "Post-auth session initialization failures by reason; each one sent a \
         client a stream-level internal-server-error.",
        1,
        reason,
    );
    let span = tracing::info_span!(
        "xmpp.session.init",
        user = %jid.to_bare(),
        reason = reason.value(),
    );
    let _enter = span.enter();
    crate::telemetry::mark_span_error("session initialization failed");
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
                // Fail the bind rather than run the session with an empty
                // blocklist (a session-long XEP-0191 fail-open). The choke
                // point below is the single log line for this failure —
                // #1175 log hygiene: no second free-standing error!.
                record_session_init_failure(
                    waddle_xmpp::telemetry::attributes::SessionInitFailureReason::BlocklistLoad,
                    &jid,
                    conn.sm_state.stream_id.as_deref(),
                    Some(format!("XEP-0191 blocklist load failed: {error}")),
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
        let mirror_outcome = crate::server::dual_registration::mirror_register_outcome(
            &state.deps.protocol.user_registry,
            jid.clone(),
            entry.clone(),
        )
        .await;
        let registered = match mirror_outcome {
            crate::server::dual_registration::MirrorRegisterOutcome::Registered => true,
            crate::server::dual_registration::MirrorRegisterOutcome::ForeignOwner => {
                register_remote_clustered_resource(state, &jid, entry, owner.clone()).await
            }
            crate::server::dual_registration::MirrorRegisterOutcome::Failed => false,
        };
        if !registered {
            // Rollback also clears the presence_states published above.
            state
                .deps
                .protocol
                .connection_registry
                .unregister_if_owner(&jid, &owner);
            // kameo's reply_timeout does not cancel an already-enqueued
            // handler, so a register that *timed out* may still land in the
            // actor tree after this rollback. Reap it with an owner-gated
            // unregister: the `UserRegistryActor` mailbox is FIFO, so this is
            // ordered after the register ask and prunes the phantom if it ran
            // late; owner-gating leaves a racing replacement untouched.
            crate::server::dual_registration::mirror_unregister(
                &state.deps.protocol.user_registry,
                &jid,
                Some(owner.clone()),
            )
            .await;
            conn.registry_owner = None;
            // This was the server-silent path from #1454: the rollback ran
            // and the client got a stream error with no server-side trace.
            record_session_init_failure(
                waddle_xmpp::telemetry::attributes::SessionInitFailureReason::AuthoritativeRegistration,
                &jid,
                conn.sm_state.stream_id.as_deref(),
                None,
            );
            return RegistrationAfterFrame::SessionInitializationFailed;
        }
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

#[cfg(feature = "clustering")]
async fn register_remote_clustered_resource(
    state: &WebSocketState,
    jid: &FullJid,
    entry: waddle_xmpp::registry::ConnectionEntry,
    owner: std::sync::Arc<std::sync::atomic::AtomicBool>,
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
    match bridge
        .try_register_remote_user_resource(jid, entry, owner)
        .await
    {
        crate::clustering::route_bridge::RemoteResourceRegisterOutcome::Registered => true,
        crate::clustering::route_bridge::RemoteResourceRegisterOutcome::NotRemote
        | crate::clustering::route_bridge::RemoteResourceRegisterOutcome::Failed => false,
    }
}

#[cfg(not(feature = "clustering"))]
async fn register_remote_clustered_resource(
    _state: &WebSocketState,
    _jid: &FullJid,
    _entry: waddle_xmpp::registry::ConnectionEntry,
    _owner: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> bool {
    false
}
