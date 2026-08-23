use super::*;
use super::{
    session_init::load_blocklist_for_bind,
    state::WsConnState,
    stream_management::{
        finalize_sm_after_registry_registration, sm_show_name, SmRegistrationFinalization,
    },
};

/// A resume can briefly observe the previous node's `UserActor` while its
/// force-detach cleanup drains.  Bound retries keep that handoff from turning
/// an otherwise-valid XEP-0198 resume into a terminal session-init failure.
const RESUME_REGISTRATION_BUSY_ATTEMPTS: usize = 3;

fn resume_registration_busy_backoff(attempt: usize) -> std::time::Duration {
    std::time::Duration::from_millis(50 * (attempt as u64 + 1))
}

pub(super) async fn retry_resumed_registration_busy<F, Fut>(
    mut register: F,
) -> crate::server::dual_registration::MirrorRegisterOutcome
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = crate::server::dual_registration::MirrorRegisterOutcome>,
{
    for attempt in 0..RESUME_REGISTRATION_BUSY_ATTEMPTS {
        let outcome = register().await;
        if outcome == crate::server::dual_registration::MirrorRegisterOutcome::Busy
            && attempt + 1 < RESUME_REGISTRATION_BUSY_ATTEMPTS
        {
            tokio::time::sleep(resume_registration_busy_backoff(attempt)).await;
            continue;
        }
        return outcome;
    }
    unreachable!("resume registration busy retry loop always returns")
}

pub(super) enum RegistrationAfterFrame {
    Unchanged,
    Registered(SmRegistrationFinalization),
    SessionInitializationFailed,
    AuthorityRevoked,
    /// Resume finalization has moved the durable detached snapshot into the
    /// live connection. Retain registry ownership so normal shutdown cleanup
    /// can store that exact live SM state before unregistering.
    AuthorityRevokedAfterSmFinalization,
}

fn registration_authoritative(
    permit: &crate::clustering::NodeAdmissionPermit,
    shutdown: &tokio_util::sync::CancellationToken,
) -> bool {
    !shutdown.is_cancelled() && permit.revalidate().is_ok()
}

async fn rollback_registered_connection(
    state: &WebSocketState,
    jid: &FullJid,
    owner: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    conn: &mut WsConnState,
) {
    state
        .deps
        .protocol
        .connection_registry
        .unregister_if_owner(jid, owner);
    crate::server::dual_registration::mirror_unregister(
        &state.deps.protocol.user_registry,
        jid,
        Some(owner.clone()),
    )
    .await;
    unregister_remote_clustered_resource_if_owner(state, jid, owner).await;
    conn.registry_owner = None;
}

#[cfg(feature = "clustering")]
async fn unregister_remote_clustered_resource_if_owner(
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
async fn unregister_remote_clustered_resource_if_owner(
    _state: &WebSocketState,
    _jid: &FullJid,
    _owner: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
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
    // `error_span!` (not `info_span!`): the registry-level `EnvFilter` gates
    // span export too, so an operator running `RUST_LOG=warn`/`error` must
    // not silently lose the failure span while keeping the log line.
    let span = tracing::error_span!(
        "xmpp.session.init",
        user = %jid.to_bare(),
        reason = reason.value(),
    );
    let _enter = span.enter();
    // Inside the span scope so the OTLP log record carries this span's
    // trace/span id — the alertable line pivots straight to the Tempo
    // error span.
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

#[cfg(test)]
pub(super) async fn register_bound_connection_after_frame(
    state: &WebSocketState,
    domain: &str,
    conn: &mut WsConnState,
    pending_tx: &mut Option<mpsc::Sender<OutboundStanza>>,
) -> RegistrationAfterFrame {
    let lifecycle = crate::clustering::NodeLifecycle::new();
    let permit = lifecycle.admit().expect("fresh serving lifecycle");
    let shutdown = tokio_util::sync::CancellationToken::new();
    register_bound_connection_after_frame_with_admission(
        state, domain, conn, pending_tx, &permit, &shutdown,
    )
    .await
}

pub(super) async fn register_bound_connection_after_frame_with_admission(
    state: &WebSocketState,
    domain: &str,
    conn: &mut WsConnState,
    pending_tx: &mut Option<mpsc::Sender<OutboundStanza>>,
    permit: &crate::clustering::NodeAdmissionPermit,
    shutdown: &tokio_util::sync::CancellationToken,
) -> RegistrationAfterFrame {
    if !registration_authoritative(permit, shutdown) {
        return RegistrationAfterFrame::AuthorityRevoked;
    }
    let Some(jid) = conn.phase.bound_jid().cloned() else {
        return RegistrationAfterFrame::Unchanged;
    };
    if pending_tx.is_none() {
        return RegistrationAfterFrame::Unchanged;
    }

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
                if !registration_authoritative(permit, shutdown) {
                    return RegistrationAfterFrame::AuthorityRevoked;
                }
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

    if !registration_authoritative(permit, shutdown) {
        return RegistrationAfterFrame::AuthorityRevoked;
    }

    conn.ensure_state_machine(
        domain,
        &state.deps.protocol.dispatcher,
        jid.clone(),
        resumed,
        blocklist,
    );

    let Some(tx) = pending_tx.take() else {
        return RegistrationAfterFrame::Unchanged;
    };

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

    if !registration_authoritative(permit, shutdown) {
        rollback_registered_connection(state, &jid, &owner, conn).await;
        return RegistrationAfterFrame::AuthorityRevoked;
    }

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
        let mirror_outcome = if resumed {
            retry_resumed_registration_busy(|| {
                crate::server::dual_registration::mirror_register_outcome(
                    &state.deps.protocol.user_registry,
                    jid.clone(),
                    entry.clone(),
                )
            })
            .await
        } else {
            crate::server::dual_registration::mirror_register_outcome(
                &state.deps.protocol.user_registry,
                jid.clone(),
                entry.clone(),
            )
            .await
        };
        let registered = match mirror_outcome {
            crate::server::dual_registration::MirrorRegisterOutcome::Registered => true,
            crate::server::dual_registration::MirrorRegisterOutcome::ForeignOwner => {
                register_remote_clustered_resource(state, &jid, entry, owner.clone()).await
            }
            crate::server::dual_registration::MirrorRegisterOutcome::Busy => false,
            crate::server::dual_registration::MirrorRegisterOutcome::Failed => false,
        };
        if !registration_authoritative(permit, shutdown) {
            rollback_registered_connection(state, &jid, &owner, conn).await;
            return RegistrationAfterFrame::AuthorityRevoked;
        }
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

    let sm_report = finalize_sm_after_registry_registration(state, conn, &jid, &owner).await;
    #[cfg(test)]
    if let Some((reached, release)) = conn.post_sm_finalization_test_hook.take() {
        reached.notify_one();
        release.notified().await;
    }
    if !registration_authoritative(permit, shutdown) {
        // `complete_pending_resume_claim` persist-deletes the detached
        // snapshot after restoring it into `conn.sm_state`. Clearing the
        // registry owner here would make outer shutdown cleanup skip detach
        // and lose that recovered queue. Preserve the owner and let the
        // normal cleanup path re-store the live session before unregister.
        // The finalized resume result is deliberately NOT recorded on this
        // path: the client never receives the finalized response.
        return RegistrationAfterFrame::AuthorityRevokedAfterSmFinalization;
    }
    if let Some(outcome) = sm_report.resume_outcome {
        super::stream_management::observe_sm_resume_finalized(outcome);
    }
    info!(
        jid = %jid,
        resumed = conn.phase.is_resumed(),
        carbons_enabled = conn.carbons_enabled,
        "WebSocket connection registered"
    );

    RegistrationAfterFrame::Registered(sm_report.finalization)
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
