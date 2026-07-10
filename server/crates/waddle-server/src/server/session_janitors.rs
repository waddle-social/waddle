use crate::room_policy::RoomRegistryActorPolicy;
use crate::server::routes;
use crate::server::routes::websocket::WebSocketState;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, error, info, warn};

/// Default interval for the auth-state TTL janitor.
const AUTH_JANITOR_INTERVAL: Duration = Duration::from_secs(60);

/// Default interval for the persistent-room dormancy janitor.
const ROOM_DORMANCY_JANITOR_INTERVAL: Duration = Duration::from_secs(300);

/// Default interval for the empty-`UserActor` reaper (ADR-0017 Phase 1
/// Slice 2). Matches the room dormancy cadence: orphaned empty actors are
/// harmless between sweeps (they route to `NotConnected`/detached), so a
/// 5-minute reap keeps `UserRegistryActor.users` bounded without hot-looping.
const USER_ACTOR_REAPER_INTERVAL: Duration = Duration::from_secs(300);

/// Upper bound on each `UserRegistryActor` ask the reaper makes (`ListUsers`,
/// per-user `ReapUserIfEmpty`, `UserCount`). A wedged or backed-up registry
/// must never hang the janitor task indefinitely (Copilot review on PR #1177):
/// on timeout the sweep logs and moves on (or ends), retrying next interval.
///
/// Sized to *strictly exceed* the registry's internal per-child ask bound
/// (`user_registry::CHILD_ACTOR_TIMEOUT`, 2s): `ReapUserIfEmpty` itself asks the
/// child `UserActor` for its `ResourceCount` bounded at that inner timeout, so a
/// janitor timeout equal to the inner one could fire simultaneously and abandon
/// a reap the registry actually completes — undercounting
/// `waddle_user_actor_reaped_total` (Greptile review on PR #1177). The extra
/// headroom lets the outer ask observe the reply instead of racing it; it is
/// harmless for the child-less `ListUsers`/`UserCount` reads, which reply fast.
const REAPER_ASK_TIMEOUT: Duration = Duration::from_secs(5);

/// Council-adjudicated FIX 4 (ADR-0017 Phase 3 Slice 8): TTL for a
/// `clustering_isr_tokens` row that has never been consumed. A token is
/// minted per `<isr-enable/>` and only ever reaped by an ordinary
/// `consume` (match or mismatch); one that's issued and never resumed
/// (client never reconnects, or the SM session is later expired/reaped by
/// [`run_orphan_reaper_sweep`] itself) would otherwise sit forever. 24h is
/// generous relative to any realistic resume window, while still bounding
/// the table.
#[cfg(feature = "clustering")]
const ISR_TOKEN_SWEEP_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Upper bound on the ISR token sweep's own DB call (FIX 4) — a wedged
/// Postgres connection must never hang the orphan-reaper sweep this rides
/// alongside, mirroring [`REAPER_ASK_TIMEOUT`]'s own bounded-ask discipline.
#[cfg(feature = "clustering")]
const ISR_TOKEN_SWEEP_TIMEOUT: Duration = Duration::from_secs(5);

/// Per-sweep cap for the stale-node watchdog that runs before the orphaned
/// SM-session claim scan. This bounds the raw-heartbeat discovery pass; each
/// candidate still has to pass `NodeLeaseStore::expire`'s CAS before any
/// claim can be stolen.
#[cfg(feature = "clustering")]
const STALE_NODE_WATCHDOG_CANDIDATE_LIMIT: usize = 64;

fn pending_delivery_max_age_days_from_env() -> u32 {
    const DEFAULT_DAYS: u32 = 30;
    const MIN_DAYS: u32 = 1;
    const MAX_DAYS: u32 = 365;
    std::env::var("WADDLE_PENDING_DELIVERY_MAX_AGE_DAYS")
        .ok()
        .and_then(|raw| raw.parse::<u32>().ok())
        .map(|v| v.clamp(MIN_DAYS, MAX_DAYS))
        .unwrap_or(DEFAULT_DAYS)
}

fn max_promotion_attempts_from_env() -> u32 {
    const DEFAULT_ATTEMPTS: u32 = 5;
    const MIN_ATTEMPTS: u32 = 2;
    const MAX_ATTEMPTS: u32 = 1024;
    std::env::var("WADDLE_SM_PROMOTION_MAX_ATTEMPTS")
        .ok()
        .and_then(|raw| raw.parse::<u32>().ok())
        .map(|v| v.clamp(MIN_ATTEMPTS, MAX_ATTEMPTS))
        .unwrap_or(DEFAULT_ATTEMPTS)
}

fn max_drain_duration_from_env() -> std::time::Duration {
    const DEFAULT_SECS: u64 = 30;
    const MIN_SECS: u64 = 1;
    const MAX_SECS: u64 = 600;
    let secs = std::env::var("WADDLE_DRAIN_TIMEOUT_SECS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(|v| v.clamp(MIN_SECS, MAX_SECS))
        .unwrap_or(DEFAULT_SECS);
    std::time::Duration::from_secs(secs)
}

fn notification_outbox_retention_days_from_env() -> u32 {
    const DEFAULT_DAYS: u32 = 30;
    const MIN_DAYS: u32 = 1;
    const MAX_DAYS: u32 = 365;
    std::env::var("WADDLE_NOTIFICATION_OUTBOX_RETENTION_DAYS")
        .ok()
        .and_then(|raw| raw.parse::<u32>().ok())
        .map(|v| v.clamp(MIN_DAYS, MAX_DAYS))
        .unwrap_or(DEFAULT_DAYS)
}

fn notification_outbox_prune_batch_from_env() -> usize {
    const DEFAULT_BATCH: usize = 1_000;
    const MIN_BATCH: usize = 1;
    const MAX_BATCH: usize = 10_000;
    std::env::var("WADDLE_NOTIFICATION_OUTBOX_PRUNE_BATCH")
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .map(|v| v.clamp(MIN_BATCH, MAX_BATCH))
        .unwrap_or(DEFAULT_BATCH)
}

pub(crate) fn spawn_sm_expiry_janitor(websocket_state: &Arc<WebSocketState>) {
    // XEP-0198 expired-session janitor. Without this, detached SM sessions
    // whose resume window elapses leave MUC occupants in their rooms forever
    // and the `resumable_sessions` sidecar grows unbounded.
    let weak_state = Arc::downgrade(websocket_state);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(60));
        // Skip the first tick (immediate) so we don't sweep before the
        // server has accepted any connections.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let Some(state) = weak_state.upgrade() else {
                break;
            };
            let drained: Vec<waddle_xmpp::stream_management::DetachedSession> = match state
                .deps
                .protocol
                .sm_session_registry
                .drain_expired()
                .await
            {
                Ok(sessions) => sessions,
                Err(err) => {
                    warn!(error = %err, "SM janitor: drain_expired failed");
                    continue;
                }
            };
            if drained.is_empty() {
                continue;
            }
            info!(
                count = drained.len(),
                "SM janitor: cleaning up expired detached sessions"
            );
            for session in drained {
                let blocklist = match state
                    .deps
                    .protocol
                    .blocking_storage
                    .list_blocked_jid_entries(&session.jid.to_bare())
                    .await
                {
                    Ok(jids) => waddle_xmpp::protocol::session_state::Blocklist::new(jids),
                    Err(error) => {
                        waddle_xmpp::prometheus::increment_sm_promotion_blocklist_failed();
                        let attempts = match state
                            .deps
                            .protocol
                            .sm_session_registry
                            .record_promotion_failure(&session.stream_id)
                            .await
                        {
                            Ok(n) => n,
                            Err(record_error) => {
                                warn!(
                                    jid = %session.jid,
                                    error = %error,
                                    record_error = %record_error,
                                    "SM janitor: blocklist load failed and \
                                     record_promotion_failure also failed; preserving \
                                     session state for retry"
                                );
                                crate::sm_promotion::reinsert_failed_session_for_retry(
                                    &state.deps.protocol.sm_session_registry,
                                    session,
                                )
                                .await;
                                continue;
                            }
                        };
                        if attempts >= max_promotion_attempts_from_env() {
                            waddle_xmpp::prometheus::increment_sm_promotion_dead_lettered();
                            error!(
                                jid = %session.jid,
                                stream_id = %session.stream_id,
                                attempts,
                                error = %error,
                                "SM janitor: blocklist load has repeatedly failed; \
                                 dead-lettering the durable row to break the retry loop"
                            );
                            state
                                .deps
                                .protocol
                                .sm_session_registry
                                .confirm_drained(&session.stream_id)
                                .await;
                            continue;
                        }
                        warn!(
                            jid = %session.jid,
                            attempts,
                            error = %error,
                            "SM janitor: blocklist load failed; SKIPPING promotion to \
                             preserve fail-closed XEP-0191 policy. Durable SM row will \
                             be retried on the next janitor pass."
                        );
                        // Make the retry promise true: drain_expired scans only
                        // memory, so the drained session must go back into the
                        // map for the next tick to see it.
                        crate::sm_promotion::reinsert_failed_session_for_retry(
                            &state.deps.protocol.sm_session_registry,
                            session,
                        )
                        .await;
                        continue;
                    }
                };
                // Round-2 review R2 + round-3 finding 1: retractions
                // racing this drain window are invisible to the registry
                // scrub (the sessions are off both maps); fetch the
                // recent-tombstone record PER SESSION, immediately
                // before this session's promotion, so even a retraction
                // landing mid-batch is still seen.
                let recent_tombstones = crate::sm_promotion::recent_tombstones_for_promotion(
                    &state.deps.protocol.sm_session_registry,
                    "SM janitor",
                );
                let summary = crate::sm_promotion::promote_session_unacked(
                    &session,
                    &state.deps.protocol.connection_registry,
                    &state.deps.protocol.user_registry,
                    &state.deps.protocol.pending_delivery_storage,
                    &blocklist,
                    state.deps.auth_state.xmpp_domain.as_str(),
                    &recent_tombstones,
                )
                .await;
                // Finding B (retraction-vs-promotion TOCTOU): a
                // retraction recorded after the snapshot above raced
                // this session's promotion — re-scrub the pending rows
                // it may have just inserted BEFORE confirm_drained.
                crate::sm_promotion::scrub_pending_for_tombstones_recorded_during_promotion(
                    &state.deps.protocol.sm_session_registry,
                    &state.deps.protocol.pending_delivery_storage,
                    &recent_tombstones,
                    "SM janitor",
                )
                .await;
                if summary.queued + summary.redelivered + summary.bounced + summary.not_promotable
                    > 0
                    || summary.storage_failed > 0
                {
                    info!(
                        jid = %session.jid,
                        redelivered = summary.redelivered,
                        queued = summary.queued,
                        bounced = summary.bounced,
                        dropped = summary.dropped,
                        not_promotable = summary.not_promotable,
                        unparseable = summary.unparseable,
                        scrubbed = summary.scrubbed,
                        storage_failed = summary.storage_failed,
                        "SM janitor: Q6 promotion completed"
                    );
                }
                if summary.has_storage_failure() {
                    waddle_xmpp::prometheus::add_sm_promotion_storage_failed(u64::from(
                        summary.storage_failed,
                    ));
                    let attempts = match state
                        .deps
                        .protocol
                        .sm_session_registry
                        .record_promotion_failure(&session.stream_id)
                        .await
                    {
                        Ok(n) => n,
                        Err(error) => {
                            warn!(
                                jid = %session.jid,
                                %error,
                                "SM janitor: record_promotion_failure failed; \
                                 preserving session state for retry"
                            );
                            crate::sm_promotion::prune_promoted_then_reinsert_for_retry(
                                &state.deps.protocol.sm_session_registry,
                                session,
                                &summary,
                            )
                            .await;
                            continue;
                        }
                    };
                    if attempts >= max_promotion_attempts_from_env() {
                        waddle_xmpp::prometheus::increment_sm_promotion_dead_lettered();
                        error!(
                            jid = %session.jid,
                            stream_id = %session.stream_id,
                            attempts,
                            storage_failed = summary.storage_failed,
                            "SM janitor: Q6 promotion repeatedly failed; \
                             dead-lettering the durable row to break the retry loop"
                        );
                        state
                            .deps
                            .protocol
                            .sm_session_registry
                            .confirm_drained(&session.stream_id)
                            .await;
                        continue;
                    }
                    warn!(
                        jid = %session.jid,
                        attempts,
                        storage_failed = summary.storage_failed,
                        "SM janitor: promotion had storage failures; \
                         preserving session state for retry"
                    );
                    crate::sm_promotion::prune_promoted_then_reinsert_for_retry(
                        &state.deps.protocol.sm_session_registry,
                        session,
                        &summary,
                    )
                    .await;
                    continue;
                }
                state
                    .deps
                    .protocol
                    .sm_session_registry
                    .confirm_drained(&session.stream_id)
                    .await;
                let session_id =
                    waddle_xmpp::pending_delivery::SmSessionId::new(session.stream_id.clone());

                // Same replacement re-check as the unclean-disconnect
                // path: a fresh bind that superseded this expired
                // detached session broadcasts its own presence, so a
                // late unavailable would pin subscribers on offline for
                // an online JID.
                routes::websocket::broadcast_unavailable_if_no_replacement(
                    &state,
                    &session.jid,
                    session.presence_available,
                )
                .await;
                state
                    .deps
                    .protocol
                    .resumable_sessions
                    .remove(&session.stream_id);
                // ADR-0017 Phase 1 (Greptile P1 on PR #1177): gate the DashMap
                // removal on the EXPIRED session's own SM stream id, not a plain
                // `unregister`. A plain unregister removes whatever currently
                // holds the full JID — which is a live REPLACEMENT session S2 if
                // it rebound the same resource after this session (S1) detached.
                // The removed entry's `carbons_enabled` would then be S2's token,
                // so the actor mirror below would evict S2's actor-tree entry
                // too — and under Slice 1 the actor tree is the bare-JID
                // selection source, so S2 would silently stop receiving
                // messages. `unregister_if_sm_stream_id` removes only when the
                // current entry is genuinely S1's (matching published stream id),
                // so S2 is left untouched.
                //
                // For the common case, S1 already had its DashMap + actor
                // entries pruned at detach time (`cleanup_connection_shutdown`),
                // so this returns `None` and the mirror is skipped — the actor
                // entry is already gone, not leaked.
                let removed_entry = state
                    .deps
                    .protocol
                    .connection_registry
                    .unregister_if_sm_stream_id(
                        &session.jid,
                        &waddle_xmpp::pending_delivery::SmSessionId::new(session.stream_id.clone()),
                    );
                // Mirror the (S1-gated) unregister into the actor tree. The
                // removed entry is guaranteed to be S1's, so its token cannot
                // evict a replacement.
                if let Some(entry) = removed_entry {
                    crate::server::dual_registration::mirror_unregister(
                        &state.deps.protocol.user_registry,
                        &session.jid,
                        Some(std::sync::Arc::clone(&entry.carbons_enabled)),
                    )
                    .await;
                }
                // PR #438 review (Qodo issue #2): the periodic SM expiry
                // janitor takes its own cleanup path; without this drop
                // it leaks `resource_to_ver` and `pending` entries for
                // every detached session that times out. The other
                // disconnect/expiry paths already call this; mirror it
                // here so the caps state is bounded across all five
                // tear-down code paths.
                state
                    .deps
                    .protocol
                    .caps_resolver
                    .drop_resource(&session.jid);
                // MUC occupancy is keyed by FULL JID: a live same-JID
                // replacement session (fresh bind after this expired
                // session detached) shares the room occupancies, so
                // evicting them here would kick the replacement out of
                // its rooms. Skip room cleanup whenever any live
                // registry entry exists for the JID (same guard as
                // cleanup_invalidated_detached_session).
                if state
                    .deps
                    .protocol
                    .connection_registry
                    .get_entry(&session.jid)
                    .is_none()
                {
                    #[cfg(feature = "clustering")]
                    {
                        let cleanup_origin =
                            crate::server::routes::interpret::OrderedRelayRouteOrigin {
                                kind: crate::server::routes::interpret::OrderedRelayRouteOriginKind::SmSession(
                                    session_id.clone(),
                                ),
                                sender_entity: waddle_xmpp::ownership::Entity::new(
                                    waddle_xmpp::ownership::EntityType::UserActor,
                                    session.jid.to_bare().to_string(),
                                ),
                                inbound_sequence: 0,
                                handoff: None,
                            };
                        routes::websocket::cleanup_muc_presence_for_jid_with_origin(
                            &state,
                            &session.jid,
                            cleanup_origin,
                        )
                        .await;
                    }
                    #[cfg(not(feature = "clustering"))]
                    {
                        routes::websocket::cleanup_muc_presence_for_jid(&state, &session.jid).await;
                    }
                }
                if let Err(error) = state
                    .deps
                    .protocol
                    .pending_delivery_storage
                    .release_claim(&session_id)
                    .await
                {
                    warn!(
                        jid = %session.jid,
                        stream_id = %session.stream_id,
                        error = %error,
                        "SM janitor: pending_delivery release_claim failed; \
                         rows remain claimed and will be released by claim-expiry janitor"
                    );
                }
            }
        }
    });
}

/// ADR-0017 Phase 3 Slice 5, element 9 (quoted verbatim): *"any node may
/// steal such [orphaned] claims (fenced CAS) and then expire or promote
/// them, after first committing the expire CAS on the owner's `nodes`
/// row."* A no-op build/runtime configuration (clustering disabled, or this
/// binary built without the `clustering` Cargo feature) does nothing —
/// [`run_orphan_reaper_sweep`] itself is feature-gated, and the outer
/// function here is always callable so its call site in `http.rs` needs no
/// `#[cfg]` of its own, mirroring the other eight janitor spawns.
///
/// `interval` is `ClusteringConfig::orphan_reaper_interval`
/// (`WADDLE_CLUSTERING_ORPHAN_REAPER_INTERVAL_MS`, default 120s) — ADR-0017
/// Phase 3 Slice 11 corrigenda (deviation 111) made this cadence env-
/// overridable, the same way every sibling cluster timer already is, so the
/// multi-process harness's kill-one hydration capstone does not have to
/// wait out the full 120s production default in real wall-clock time.
pub(crate) fn spawn_orphan_reaper_janitor(
    websocket_state: &Arc<WebSocketState>,
    interval: std::time::Duration,
) {
    #[cfg(feature = "clustering")]
    {
        let weak_state = Arc::downgrade(websocket_state);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            // Skip the first (immediate) tick, mirroring the other janitors
            // — no need to sweep before the node-lease loop has even
            // registered this node.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                let Some(state) = weak_state.upgrade() else {
                    break;
                };
                run_orphan_reaper_sweep(&state).await;
            }
        });
    }
    #[cfg(not(feature = "clustering"))]
    {
        let _ = websocket_state;
        let _ = interval;
    }
}

#[cfg(feature = "clustering")]
async fn orphan_reaper_self_lease_is_fresh(
    node_lease: &dyn crate::clustering::claims::NodeLeaseStore,
    me: &waddle_xmpp::ownership::NodeIdentity,
    lease_ttl: Duration,
    phase: &'static str,
) -> bool {
    match node_lease.is_fresh(me, lease_ttl).await {
        Ok(true) => true,
        Ok(false) => {
            debug!(
                node_id = %me.node_id,
                node_epoch = %me.node_epoch,
                phase,
                "orphan reaper: skipping sweep because this node's own lease is not fresh"
            );
            false
        }
        Err(error) => {
            warn!(
                node_id = %me.node_id,
                node_epoch = %me.node_epoch,
                phase,
                %error,
                "orphan reaper: failed to prove this node's own lease freshness; skipping sweep"
            );
            false
        }
    }
}

/// One orphan-reaper sweep: first run the bounded stale-node watchdog that
/// commits `expired = true` through `NodeLeaseStore::expire` for
/// heartbeat-stale nodes, then scan for `sm_session` claims owned by a
/// committed-stale node, steal what this node can, and targeted-hydrate
/// exactly the entities this sweep just won
/// (FIX 2, council-adjudicated ADR-0017 Phase 3 Slice 5 corrigenda) via
/// [`waddle_xmpp::stream_management::InMemorySmSessionRegistry::hydrate_reclaimed`].
///
/// **Not `restore_from_persistence`**: that method is a startup-time-only,
/// unscoped table scan (see its own doc comment) — re-running it here, on a
/// server that is already serving live traffic, on every successful sweep
/// would re-scan every row this node already holds on every tick and can
/// observe a row a live session concurrently completes/re-claims mid-scan
/// (the **live restore hazard** this fix closes). `hydrate_reclaimed`
/// instead takes each entity's own stream-shard lock and re-checks
/// in-memory absence before loading/inserting, so it is safe to call from a
/// running server — it hydrates exactly (and only) the entities this sweep
/// just reclaimed, without needing this function to duplicate the
/// codec/expiry logic `hydrate_reclaimed` already owns.
#[cfg(feature = "clustering")]
async fn run_orphan_reaper_sweep(state: &Arc<WebSocketState>) {
    use waddle_xmpp::ownership::{ClaimError, Entity};

    let clustering = &state.deps.app_state.clustering_claims;
    let Some((_claim_store, identity_handle)) = clustering.claim_pair() else {
        return;
    };
    let Some(node_lease) = clustering.node_lease.clone() else {
        return;
    };
    let Some(lease_ttl) = clustering.lease_ttl else {
        return;
    };

    let me = identity_handle.current();
    if !orphan_reaper_self_lease_is_fresh(node_lease.as_ref(), &me, lease_ttl, "start").await {
        return;
    }

    match node_lease
        .list_heartbeat_stale_nodes(lease_ttl, STALE_NODE_WATCHDOG_CANDIDATE_LIMIT)
        .await
    {
        Ok(stale_nodes) => {
            let candidate_count = stale_nodes.len();
            let mut expired_nodes = 0usize;
            let mut renewed_nodes = 0usize;
            let mut failed_nodes = 0usize;
            for stale_node in stale_nodes {
                if stale_node == me {
                    debug!(
                        node_id = %me.node_id,
                        node_epoch = %me.node_epoch,
                        "orphan reaper: stale-node watchdog found this node's own heartbeat stale; aborting sweep"
                    );
                    return;
                }
                match node_lease.expire(&stale_node, lease_ttl).await {
                    Ok(true) => expired_nodes += 1,
                    Ok(false) => renewed_nodes += 1,
                    Err(error) => {
                        failed_nodes += 1;
                        warn!(
                            node_id = %stale_node.node_id,
                            node_epoch = %stale_node.node_epoch,
                            %error,
                            "orphan reaper: stale-node watchdog expire failed; retrying next sweep"
                        );
                    }
                }
            }
            if expired_nodes > 0 {
                info!(
                    expired_nodes,
                    candidate_count,
                    limit = STALE_NODE_WATCHDOG_CANDIDATE_LIMIT,
                    "orphan reaper: stale-node watchdog committed expired nodes"
                );
            }
            if renewed_nodes > 0 {
                debug!(
                    renewed_nodes,
                    candidate_count,
                    "orphan reaper: stale-node watchdog candidates renewed before expire"
                );
            }
            if failed_nodes > 0 {
                warn!(
                    failed_nodes,
                    candidate_count,
                    "orphan reaper: stale-node watchdog failed to expire some candidates"
                );
            }
        }
        Err(error) => {
            warn!(%error, "orphan reaper: stale-node watchdog candidate scan failed");
        }
    }

    if !orphan_reaper_self_lease_is_fresh(node_lease.as_ref(), &me, lease_ttl, "post-watchdog")
        .await
    {
        return;
    }

    let candidates = match node_lease.list_orphaned_sm_session_claims().await {
        Ok(candidates) => candidates,
        Err(error) => {
            warn!(%error, "orphan reaper: list_orphaned_sm_session_claims failed");
            return;
        }
    };
    if candidates.is_empty() {
        return;
    }

    // ADR-0017 Phase 3 Slice 10 (Q5's rollout-aware placement rule): an
    // old-generation node backs off before racing a matching/newer
    // -generation node for a just-orphaned claim, so each entity moves
    // approximately once per deploy instead of up to N times. Resolved
    // once per sweep (not per candidate) — the current generation cannot
    // meaningfully change mid-sweep, and this keeps the hot per-candidate
    // loop below to a single extra comparison, no extra Postgres round
    // trip per candidate.
    let backoff_delay = match node_lease.current_generation().await {
        Ok(current_generation) => crate::clustering::drain::rollout_backoff_delay(
            clustering.pod_template_hash.as_deref(),
            current_generation.as_deref(),
        ),
        Err(error) => {
            debug!(%error, "orphan reaper: current_generation lookup failed; proceeding without backoff");
            std::time::Duration::ZERO
        }
    };

    let mut reclaimed: Vec<(Entity, waddle_xmpp::ownership::ClaimEpoch)> = Vec::new();
    for candidate in candidates {
        if !orphan_reaper_self_lease_is_fresh(node_lease.as_ref(), &me, lease_ttl, "pre-candidate")
            .await
        {
            return;
        }
        // Element 9's ordering requirement: commit the expire CAS on the
        // dead owner's row FIRST. Idempotent/best-effort — a failure here
        // just means the steal below is also likely to lose (the owner
        // row is not yet committed-expired), retried next sweep.
        if let Err(error) = node_lease.expire(&candidate.owner, lease_ttl).await {
            debug!(
                entity_id = %candidate.entity.id,
                %error,
                "orphan reaper: expire on the dead owner's row failed; retrying next sweep"
            );
            continue;
        }
        // ADR-0017 Phase 3 Slice 10: a placement heuristic only — never
        // affects correctness. The reaper-specific stale-owner epoch CAS
        // remains the sole authority over who actually wins; this only
        // decides who tries first.
        if !backoff_delay.is_zero() {
            tokio::time::sleep(backoff_delay).await;
        }
        if !orphan_reaper_self_lease_is_fresh(node_lease.as_ref(), &me, lease_ttl, "pre-steal")
            .await
        {
            return;
        }
        match node_lease
            .steal_orphaned_sm_session_claim(&candidate.entity, candidate.epoch, &me, lease_ttl)
            .await
        {
            Ok(new_epoch) => reclaimed.push((candidate.entity, new_epoch)),
            Err(ClaimError::Conflict) => {
                // Another node (or this same node's own re-registration
                // reacquisition step, ADR-0017 Phase 3 plan deviation #19)
                // already reclaimed it, or the "dead" owner actually
                // renewed concurrently — safe, no-op.
            }
            Err(error) => {
                warn!(
                    entity_id = %candidate.entity.id,
                    %error,
                    "orphan reaper: steal_orphaned_sm_session_claim failed"
                );
            }
        }
    }
    if !reclaimed.is_empty() {
        let stolen = reclaimed.len();
        info!(
            stolen,
            "orphan reaper: reclaimed orphaned SM-session claims"
        );
        match state
            .deps
            .protocol
            .sm_session_registry
            .hydrate_reclaimed(&reclaimed)
            .await
        {
            Ok(hydrated) => {
                info!(
                    stolen,
                    hydrated, "orphan reaper: targeted hydration of reclaimed claims complete"
                );
            }
            Err(error) => {
                warn!(
                    %error,
                    "orphan reaper: hydrate_reclaimed (post-steal targeted hydrate) failed"
                );
            }
        }
    }

    // Council-adjudicated FIX 4 (ADR-0017 Phase 3 Slice 8): a
    // `clustering_isr_tokens` row is never reaped by the ordinary
    // `consume` path alone (a token issued but never resumed, or whose SM
    // session this very sweep just reaped above, leaves an otherwise
    // -permanent orphan). No cascade hook exists from the SM session
    // claim's own release/reap paths — the SM session registry
    // (`waddle-xmpp`) has no reason to depend on ISR (`waddle-server`
    // -local, Postgres-only) at all, the same crate separation
    // `ClaimStore`/`IsrTokenStore` already keep — so this rides the same
    // janitor cadence as a bounded, deadline-armed TTL sweep instead.
    if let Some(isr_token_store) = clustering.isr_token_store() {
        match tokio::time::timeout(
            ISR_TOKEN_SWEEP_TIMEOUT,
            isr_token_store.sweep_expired(ISR_TOKEN_SWEEP_MAX_AGE),
        )
        .await
        {
            Ok(Ok(deleted)) if deleted > 0 => {
                info!(deleted, "orphan reaper: swept expired ISR tokens");
            }
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                warn!(%error, "orphan reaper: ISR token sweep failed");
            }
            Err(_timeout) => {
                warn!(
                    timeout = ?ISR_TOKEN_SWEEP_TIMEOUT,
                    "orphan reaper: ISR token sweep timed out"
                );
            }
        }
    }
}

/// Postgres-gated end-to-end test for [`run_orphan_reaper_sweep`]
/// (ADR-0017 Phase 3 Slice 5 corrigenda, council-adjudicated review): seeds
/// a stale-owner `sm_session` claim plus its durably persisted session,
/// runs the sweep, and asserts every step of element 9's ordering contract
/// actually lands — the expire CAS, the steal, and the targeted hydration
/// — then repeats under two concurrent sweeps to prove the steal is
/// exactly-once.
///
/// Skipped (not failed) when `WADDLE_TEST_POSTGRES_URL` is unset, mirroring
/// every other Postgres-gated test in this crate (`clustering::claims`,
/// `clustering::self_fence`, `sm_persistence_fenced::tests`).
#[cfg(all(test, feature = "clustering"))]
mod orphan_reaper_sweep_tests {
    use super::*;
    use crate::clustering::claims::{
        clustering_control_plane_table_lock, NodeLeaseStore, PostgresClaimStore,
    };
    use crate::clustering::ClusteringHandles;
    use crate::db::{Database, DatabaseConfig, DatabaseDriver, DEFAULT_CONTROL_PLANE_POOL_SIZE};
    use crate::server::routes::websocket::tests::create_test_websocket_state_with_clustering;
    use crate::sm_persistence_fenced::PostgresFencedSmPersistence;
    use chrono::{TimeZone, Utc};
    use waddle_xmpp::ownership::{
        ClaimEpoch, ClaimStore, Entity, EntityType, NodeIdentity, SharedNodeIdentity,
    };
    use waddle_xmpp::pending_delivery::SmSessionId;
    use waddle_xmpp::stream_management::persistence::{PersistedSession, SmPersistenceStorage};
    use waddle_xmpp::stream_management::{InMemorySmSessionRegistry, SmSessionRegistry as _};
    use xmpp_parsers::presence::Show;

    fn node_identity() -> NodeIdentity {
        NodeIdentity::new(
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
        )
    }

    fn sm_entity(id: &str) -> Entity {
        Entity::new(EntityType::SmSession, id.to_string())
    }

    fn full(s: &str) -> jid::FullJid {
        s.parse().expect("valid full JID fixture")
    }

    /// Deliberately in the past: [`PostgresFencedSmPersistence`] always
    /// overwrites `detached_at` with the database's own `now()` at write
    /// time (mirrors `sm_persistence_fenced::tests`'s identical fixture
    /// helper), so the exact value here is never asserted against.
    fn stale_caller_supplied_time() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0)
            .single()
            .expect("valid fixture timestamp")
    }

    fn fixture_session(stream_id: &str) -> PersistedSession {
        PersistedSession {
            stream_id: SmSessionId::new(stream_id),
            user_id: "alice".to_string(),
            jid: full("alice@example.com/web"),
            inbound_count: 7,
            outbound_count: 12,
            last_acked: 10,
            replay_gap_through: Some(9),
            max_resume_time: Some(60),
            detached_at: stale_caller_supplied_time(),
            max_resume_duration: Duration::from_secs(60),
            carbons_enabled: true,
            roster_interested: true,
            blocklist_interested: true,
            presence_available: true,
            presence_show: Some(Show::Chat),
            presence_status: Some("at the keyboard".to_string()),
            presence_priority: 5,
            presence_payloads: Vec::new(),
        }
    }

    async fn expired_flag(db: &Database, node_id: &str) -> bool {
        let conn = db.guard().await.expect("guard");
        let mut rows = conn
            .query(
                "SELECT expired FROM clustering_nodes WHERE node_id = ?",
                crate::db_params![node_id.to_string()],
            )
            .await
            .expect("query expired flag");
        rows.next()
            .await
            .expect("row present")
            .expect("row present")
            .get::<bool>(0)
            .expect("expired column")
    }

    async fn backdate_heartbeat(db: &Database, node_id: &str) {
        let conn = db.guard().await.expect("guard");
        conn.execute(
            "UPDATE clustering_nodes SET heartbeat = now() - interval '1 hour' WHERE node_id = ?",
            crate::db_params![node_id.to_string()],
        )
        .await
        .expect("backdate heartbeat");
    }

    /// Seed a persisted SM session row for `stream_id` as its current
    /// claim-holding `owner` — [`PostgresFencedSmPersistence::upsert_session`]
    /// fences every write through `assert_fenced` (a `FOR SHARE` probe
    /// against `clustering_claims` keyed on *this handle's own* node
    /// identity — see that method's doc comment), so seeding the row
    /// through the sweeper's own persistence handle (which does not hold
    /// the claim yet — that is the whole point of this test) would be
    /// rejected with `NotOwner`. A short-lived, throwaway
    /// `PostgresFencedSmPersistence` scoped to `owner`'s identity is the
    /// correct stand-in for "the dead node itself persisted this session
    /// before it died," mirroring how the real dead node would have
    /// written it under its own fenced handle.
    async fn seed_persisted_session_as_owner(db: &Database, owner: &NodeIdentity, stream_id: &str) {
        let owner_claim_store: Arc<dyn ClaimStore> = Arc::new(PostgresClaimStore::new(db.clone()));
        let owner_fenced = PostgresFencedSmPersistence::open(
            db.clone(),
            owner_claim_store,
            SharedNodeIdentity::new(owner.clone()),
        )
        .await
        .expect("open fenced SM persistence scoped to the claim-holding owner");
        owner_fenced
            .upsert_session(fixture_session(stream_id))
            .await
            .expect("seed persisted session as its current claim-holding owner");
    }

    /// Register `dead_owner`, then backdate its heartbeat while leaving
    /// `expired = false`. The sweep's stale-node watchdog must discover
    /// this row, commit `NodeLeaseStore::expire`, and only then make its
    /// SM-session claims visible to `list_orphaned_sm_session_claims`.
    async fn register_and_backdate_dead_owner(
        claim_store: &PostgresClaimStore,
        db: &Database,
        dead_owner: &NodeIdentity,
    ) {
        claim_store
            .register(dead_owner, None)
            .await
            .expect("register dead owner");
        assert!(
            !expired_flag(db, &dead_owner.node_id).await,
            "freshly registered node must start non-expired"
        );
        backdate_heartbeat(db, &dead_owner.node_id).await;
        assert!(
            !expired_flag(db, &dead_owner.node_id).await,
            "the fixture must leave expiry for run_orphan_reaper_sweep's watchdog"
        );
    }

    #[tokio::test]
    async fn run_orphan_reaper_sweep_steals_hydrates_and_is_exactly_once_under_concurrency() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Ok(url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
            eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set");
            return;
        };
        let db = Database::from_config(
            "orphan-reaper-sweep-test",
            &DatabaseConfig::new(DatabaseDriver::Postgres, url)
                .with_control_plane_pool(DEFAULT_CONTROL_PLANE_POOL_SIZE),
        )
        .await
        .expect("open test postgres");

        let claim_store = PostgresClaimStore::new(db.clone());
        claim_store
            .ensure_schema()
            .await
            .expect("ensure claims schema");
        {
            let conn = db.guard().await.expect("guard");
            for stmt in [
                "DELETE FROM clustering_claims",
                "DELETE FROM clustering_nodes",
                "DELETE FROM clustering_steal_intents",
            ] {
                conn.execute(stmt, ()).await.expect("clean table");
            }
        }

        let lease_ttl = Duration::from_secs(30);

        // The sweeping node's own identity, shared verbatim between
        // `ClusteringHandles` and the SM-session registry's claim binding —
        // mirrors `server/http.rs::create_sm_session_registry`'s production
        // wiring, never two independently-tracked identities for the same
        // node.
        let sweeper_identity = node_identity();
        let sweeper_identity_handle = SharedNodeIdentity::new(sweeper_identity.clone());
        let sweeper_claim_store: Arc<dyn ClaimStore> =
            Arc::new(PostgresClaimStore::new(db.clone()));
        let sweeper_node_lease: Arc<dyn NodeLeaseStore> =
            Arc::new(PostgresClaimStore::new(db.clone()));
        claim_store
            .register(&sweeper_identity, None)
            .await
            .expect("register the sweeping node's own lease row");

        // Fenced SM persistence, co-located in the same Postgres database
        // as the claims tables (FIX 4's co-location invariant in
        // `sm_persistence_fenced.rs`).
        let fenced = PostgresFencedSmPersistence::open(
            db.clone(),
            Arc::clone(&sweeper_claim_store),
            sweeper_identity_handle.clone(),
        )
        .await
        .expect("open fenced SM persistence");
        {
            let conn = db.guard().await.expect("guard");
            for stmt in ["DELETE FROM sm_unacked", "DELETE FROM sm_sessions"] {
                conn.execute(stmt, ()).await.expect("clean table");
            }
        }
        let fenced_arc: Arc<dyn SmPersistenceStorage> = Arc::new(fenced);

        let sm_session_registry = Arc::new(
            InMemorySmSessionRegistry::new()
                .with_persistence(Arc::clone(&fenced_arc))
                .with_claim_store(
                    Arc::clone(&sweeper_claim_store),
                    sweeper_identity_handle.clone(),
                ),
        );

        let clustering = ClusteringHandles {
            claim_store: Some(Arc::clone(&sweeper_claim_store)),
            node_identity: Some(sweeper_identity_handle.clone()),
            local_claims: None,
            room_local_claims: None,
            user_local_claims: None,
            muc_durable_store: None,
            isr_token_store: None,
            node_lease: Some(sweeper_node_lease),
            lease_ttl: Some(lease_ttl),
            pod_template_hash: None,
            resume_bridge: None,
            ordered_relay_delivery_bridge: None,
            stop_token: None,
            resume_handshake_timeout: None,
        };

        let state = create_test_websocket_state_with_clustering(
            clustering,
            Arc::clone(&sm_session_registry),
        )
        .await;

        // ============ Leg 1: single-sweep steal + targeted hydrate ============
        let dead_owner = node_identity();
        register_and_backdate_dead_owner(&claim_store, &db, &dead_owner).await;

        let stream_id = "orphan-reaper-stream-1";
        let entity = sm_entity(stream_id);
        let orphan_epoch = claim_store
            .acquire(&entity, &dead_owner)
            .await
            .expect("acquire claim under dead owner");
        seed_persisted_session_as_owner(&db, &dead_owner, stream_id).await;

        run_orphan_reaper_sweep(&state).await;

        assert!(
            expired_flag(&db, &dead_owner.node_id).await,
            "the stale-node watchdog must commit dead owner's clustering_nodes row expired"
        );
        let steal_landed = claim_store
            .fence(&entity, &sweeper_identity, ClaimEpoch(orphan_epoch.0 + 1))
            .await
            .expect("fence call");
        assert!(
            steal_landed,
            "steal must land: the sweeping node must now own the entity at the \
             bumped epoch"
        );
        let hydrated = sm_session_registry
            .peek_session(stream_id)
            .await
            .expect("peek_session call");
        assert!(
            hydrated.is_some(),
            "targeted hydration must land the reclaimed session in memory"
        );

        // ============ Leg 2: two concurrent sweeps, exactly-once steal ============
        let dead_owner_2 = node_identity();
        register_and_backdate_dead_owner(&claim_store, &db, &dead_owner_2).await;

        let stream_id_2 = "orphan-reaper-stream-2";
        let entity_2 = sm_entity(stream_id_2);
        let orphan_epoch_2 = claim_store
            .acquire(&entity_2, &dead_owner_2)
            .await
            .expect("acquire claim under dead owner 2");
        seed_persisted_session_as_owner(&db, &dead_owner_2, stream_id_2).await;

        let state_a = Arc::clone(&state);
        let state_b = Arc::clone(&state);
        let task_a = tokio::spawn(async move { run_orphan_reaper_sweep(&state_a).await });
        let task_b = tokio::spawn(async move { run_orphan_reaper_sweep(&state_b).await });
        let (result_a, result_b) = tokio::join!(task_a, task_b);
        result_a.expect("sweep task a must not panic");
        result_b.expect("sweep task b must not panic");

        assert!(
            expired_flag(&db, &dead_owner_2.node_id).await,
            "dead owner 2's clustering_nodes row must be committed-expired"
        );
        let steal_landed_exactly_once = claim_store
            .fence(
                &entity_2,
                &sweeper_identity,
                ClaimEpoch(orphan_epoch_2.0 + 1),
            )
            .await
            .expect("fence call");
        assert!(
            steal_landed_exactly_once,
            "exactly one of the two concurrent sweeps must have won the steal — the \
             epoch must be bumped by exactly 1, not 2 (a double-steal would fence at \
             +1 as false, since the epoch would actually be +2)"
        );
        let hydrated_2 = sm_session_registry
            .peek_session(stream_id_2)
            .await
            .expect("peek_session call");
        assert!(
            hydrated_2.is_some(),
            "the winning sweep's targeted hydration must land session 2 in memory"
        );

        // Sanity: exactly the two legs' sessions are in memory — no
        // duplicate or lost hydration across the concurrent pair.
        let total = sm_session_registry.session_count().await;
        assert_eq!(
            total, 2,
            "exactly one hydrated session per leg; no duplicates from the \
             concurrent sweeps"
        );

        // ============ Leg 3: a heartbeat-stale sweeper cannot steal or hydrate ============
        {
            let conn = db.guard().await.expect("guard");
            conn.execute(
                "UPDATE clustering_nodes \
                 SET heartbeat = now() - interval '1 hour', expired = false \
                 WHERE node_id = ?",
                crate::db_params![sweeper_identity.node_id.clone()],
            )
            .await
            .expect("backdate sweeper heartbeat");
        }

        let dead_owner_3 = node_identity();
        register_and_backdate_dead_owner(&claim_store, &db, &dead_owner_3).await;
        let stream_id_3 = "orphan-reaper-stream-stale-sweeper";
        let entity_3 = sm_entity(stream_id_3);
        let orphan_epoch_3 = claim_store
            .acquire(&entity_3, &dead_owner_3)
            .await
            .expect("acquire claim under dead owner 3");
        seed_persisted_session_as_owner(&db, &dead_owner_3, stream_id_3).await;

        run_orphan_reaper_sweep(&state).await;

        assert!(
            claim_store
                .fence(&entity_3, &dead_owner_3, orphan_epoch_3)
                .await
                .expect("fence dead-owner claim after heartbeat-stale sweeper"),
            "a heartbeat-stale sweeping node must not steal the orphaned claim"
        );
        assert!(
            !claim_store
                .fence(
                    &entity_3,
                    &sweeper_identity,
                    ClaimEpoch(orphan_epoch_3.0 + 1)
                )
                .await
                .expect("fence heartbeat-stale sweeper after failed steal"),
            "the heartbeat-stale sweeping node must not own the bumped claim epoch"
        );
        let not_hydrated = sm_session_registry
            .peek_session(stream_id_3)
            .await
            .expect("peek_session call");
        assert!(
            not_hydrated.is_none(),
            "a heartbeat-stale sweeping node must not hydrate a session it could not steal"
        );
    }
}

/// #1124: how many janitor intervals a claim must age before the
/// claim-expiry janitor may release it. Three intervals (180s at the
/// default 60s cadence) comfortably outlasts an in-flight flush batch
/// while keeping post-crash orphan recovery prompt.
const PENDING_CLAIM_RELEASE_FLOOR_INTERVALS: i64 = 3;

pub(crate) fn spawn_pending_delivery_claim_janitor(websocket_state: &Arc<WebSocketState>) {
    // pending_delivery claim-expiry janitor (issue #209 slice (d)
    // phase 6 / PR #360): catches claims whose session no longer exists.
    let weak_state = Arc::downgrade(websocket_state);
    let interval_secs = std::env::var("WADDLE_PENDING_DELIVERY_JANITOR_INTERVAL")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(|v| v.max(1))
        .unwrap_or(60);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
        // Skip the first tick (immediate) so we don't sweep before
        // any flush has had a chance to run.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let Some(state) = weak_state.upgrade() else {
                break;
            };
            let detached_live: Option<Vec<waddle_xmpp::pending_delivery::SmSessionId>> = state
                .deps
                .protocol
                .sm_session_registry
                .live_session_ids()
                .map(|ids| {
                    ids.into_iter()
                        .map(waddle_xmpp::pending_delivery::SmSessionId::new)
                        .collect()
                });
            let detached_live = match detached_live {
                Some(v) => v,
                None => {
                    warn!(
                        "claim-expiry janitor: SM session registry locks poisoned; \
                         skipping sweep to avoid mass-release"
                    );
                    continue;
                }
            };
            let active_live = state
                .deps
                .protocol
                .connection_registry
                .active_sm_stream_ids();
            let mut live_sessions = detached_live;
            live_sessions.extend(active_live);
            // #1124 claim recency floor: only release claims older
            // than several janitor intervals. Non-SM flushes claim
            // rows under a synthetic `transient:` session id that is
            // never in the live-set; without the floor, a janitor
            // pass overlapping an in-flight flush releases its claims
            // mid-flight and a second resource re-pushes the same
            // offline messages. Fresh claims are skipped this pass and
            // re-examined on later sweeps, so genuinely orphaned
            // (post-crash) claims are still released — just a few
            // intervals later.
            let claim_release_floor_ms = i64::try_from(interval_secs)
                .unwrap_or(i64::MAX)
                .saturating_mul(1_000)
                .saturating_mul(PENDING_CLAIM_RELEASE_FLOOR_INTERVALS);
            let now_ms = chrono::Utc::now().timestamp_millis();
            let claimed_before_ms = now_ms.saturating_sub(claim_release_floor_ms);
            // Adopt claims written without a recency stamp (a
            // pre-#1124 binary during a rolling deploy) so they age
            // into release-eligibility instead of being skipped
            // forever — `list_orphaned_claims` ignores unstamped rows.
            match state
                .deps
                .protocol
                .pending_delivery_storage
                .stamp_unstamped_claims(now_ms)
                .await
            {
                Ok(adopted) if adopted > 0 => {
                    debug!(
                        adopted,
                        "claim-expiry janitor: adopted unstamped pending_delivery claims"
                    );
                }
                Ok(_) => {}
                Err(error) => {
                    warn!(error = %error, "claim-expiry janitor: stamp_unstamped_claims failed");
                }
            }
            match state
                .deps
                .protocol
                .pending_delivery_storage
                .list_orphaned_claims(&live_sessions, claimed_before_ms)
                .await
            {
                Ok(orphans) if !orphans.is_empty() => {
                    let candidate_count = orphans.len();
                    let mut released = 0u64;
                    let mut skipped_reclaimed = 0u64;
                    for (row_id, session) in orphans {
                        match state
                            .deps
                            .protocol
                            .pending_delivery_storage
                            .release_row_if_session(&row_id, &session)
                            .await
                        {
                            Ok(0) => skipped_reclaimed += 1,
                            Ok(_) => released += 1,
                            Err(error) => {
                                warn!(
                                    row_id = %row_id,
                                    session = %session,
                                    error = %error,
                                    "claim-expiry janitor: release_row_if_session failed; row stays \
                                     claimed and will be retried next sweep"
                                );
                            }
                        }
                    }
                    info!(
                        candidate_count,
                        released,
                        skipped_reclaimed,
                        "claim-expiry janitor: released orphaned pending_delivery claims"
                    );
                    waddle_xmpp::prometheus::add_pending_delivery_orphan_claims_released(released);
                }
                Ok(_) => {}
                Err(error) => {
                    warn!(error = %error, "claim-expiry janitor: list_orphaned_claims failed");
                }
            }

            let swept = state
                .deps
                .protocol
                .pending_delivery_storage
                .sweep_internal_bookkeeping();
            if swept > 0 {
                debug!(
                    swept,
                    "claim-expiry janitor: pruned idle per-recipient insert locks"
                );
            }

            let max_age_days = i64::from(pending_delivery_max_age_days_from_env());
            let cutoff = chrono::Utc::now() - chrono::Duration::days(max_age_days);
            match state
                .deps
                .protocol
                .pending_delivery_storage
                .delete_older_than(cutoff)
                .await
            {
                Ok(0) => {}
                Ok(removed) => {
                    waddle_xmpp::prometheus::add_pending_delivery_aged_out(removed);
                    info!(
                        removed,
                        cutoff = %cutoff,
                        max_age_days,
                        "pending_delivery aging janitor: dropped expired rows"
                    );
                }
                Err(error) => {
                    warn!(
                        %error,
                        "pending_delivery aging janitor: delete_older_than failed; \
                         will retry on next sweep"
                    );
                }
            }
        }
    });
}

pub(crate) fn spawn_push_service_publish_job_janitor(websocket_state: &Arc<WebSocketState>) {
    let weak_state = Arc::downgrade(websocket_state);
    let interval_secs = std::env::var("WADDLE_PUSH_SERVICE_JOB_JANITOR_INTERVAL")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(|v| v.max(1))
        .unwrap_or(10);
    let batch_size = std::env::var("WADDLE_PUSH_SERVICE_JOB_JANITOR_BATCH")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .map(|v| v.clamp(1, 1_000))
        .unwrap_or(128);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let Some(state) = weak_state.upgrade() else {
                break;
            };
            match state
                .deps
                .protocol
                .push_service
                .drain_queued_notification_publish_jobs(batch_size)
                .await
            {
                Ok(results) if !results.is_empty() => {
                    debug!(
                        drained = results.len(),
                        "Push Service publish-job janitor drained queued XEP-0357 jobs"
                    );
                }
                Ok(_) => {}
                Err(error) => {
                    warn!(
                        error = %error,
                        "Push Service publish-job janitor failed; queued jobs remain durable"
                    );
                }
            }
        }
    });
}

pub(crate) fn spawn_notification_outbox_janitor(websocket_state: &Arc<WebSocketState>) {
    let weak_state = Arc::downgrade(websocket_state);
    let interval_secs = std::env::var("WADDLE_NOTIFICATION_OUTBOX_JANITOR_INTERVAL")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(|v| v.max(1))
        .unwrap_or(5);
    let batch_size = std::env::var("WADDLE_NOTIFICATION_OUTBOX_JANITOR_BATCH")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .map(|v| v.clamp(1, 1_000))
        .unwrap_or(128);
    let retention_days = notification_outbox_retention_days_from_env();
    let prune_batch_size = notification_outbox_prune_batch_from_env();
    // Operability: DND suppression now reads the durable
    // `urn:waddle:dnd:0` PEP projection (#367). The
    // `waddle_push_suppressed_total{reason="waddle_dnd"}` counter
    // reflects real recipient state — flat = no users actively in
    // DND, not a wiring placeholder.
    info!("Notification outbox janitor: DND suppression backed by urn:waddle:dnd:0 PEP projection");
    // Slice 2b operability: log the effective XEP-0513 `<active/>`
    // TTL once at startup so operators can read the clamped value
    // without grepping the codebase or re-deriving the env var
    // resolution chain.
    let active_mention_ttl_ms = crate::notification_outbox::active_mention_ttl_ms_from_env();
    let active_mention_ttl_secs = active_mention_ttl_ms / 1_000;
    info!(
        "Push active-mention TTL: {active_mention_ttl_secs}s \
         (default {}s, env WADDLE_PUSH_ACTIVE_MENTION_TTL_SECONDS, \
         clamp [{}s, {}s])",
        crate::notification_outbox::DEFAULT_ACTIVE_MENTION_TTL_SECONDS,
        crate::notification_outbox::MIN_ACTIVE_MENTION_TTL_SECONDS,
        crate::notification_outbox::MAX_ACTIVE_MENTION_TTL_SECONDS,
    );
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let Some(state) = weak_state.upgrade() else {
                break;
            };
            let first_party_service_jid = match state.deps.service_domains.push.parse() {
                Ok(jid) => jid,
                Err(error) => {
                    warn!(
                        error = %error,
                        push_service = %state.deps.service_domains.push,
                        "Notification outbox janitor cannot parse first-party Push Service JID"
                    );
                    continue;
                }
            };
            let recovered = routes::interpret::reconcile_xep0357_notification_candidates(
                state.as_ref(),
                batch_size,
            )
            .await;
            if recovered > 0 {
                debug!(
                    recovered,
                    "Notification outbox janitor recovered XEP-0357 candidates from pending_delivery"
                );
            }
            let recovered_groupchat =
                routes::interpret::reconcile_groupchat_notification_candidates(
                    state.as_ref(),
                    batch_size,
                )
                .await;
            if recovered_groupchat > 0 {
                debug!(
                    recovered = recovered_groupchat,
                    "Notification outbox janitor recovered XEP-0357 groupchat candidates from inbox projections"
                );
            }
            let room_policy =
                RoomRegistryActorPolicy::new(state.deps.protocol.room_registry.clone());
            let dnd_reader = state.deps.protocol.dnd_reader.as_ref();
            let activity_reader = state.deps.protocol.notification_activity.as_ref();
            let deps = crate::notification_outbox::NotificationDrainDeps::new(
                &room_policy,
                dnd_reader,
                activity_reader,
            );
            match state
                .deps
                .protocol
                .notification_outbox
                .drain_pending_candidates_into_outbox(
                    state.deps.protocol.push_store.as_ref(),
                    state.deps.protocol.blocking_storage.as_ref(),
                    state
                        .deps
                        .protocol
                        .notification_settings_projection
                        .as_ref(),
                    deps,
                    &first_party_service_jid,
                    batch_size,
                )
                .await
            {
                Ok(processed) if processed > 0 => {
                    debug!(
                        processed,
                        "Notification outbox janitor expanded XEP-0357 candidates into outbox jobs"
                    );
                }
                Ok(_) => {}
                Err(error) => {
                    warn!(
                        error = %error,
                        "Notification outbox janitor failed to process candidates; candidates remain durable"
                    );
                }
            }
            match state
                .deps
                .protocol
                .notification_outbox
                .drain_due_outbox_jobs(
                    state.deps.protocol.push_service.as_ref(),
                    state.deps.protocol.push_store.as_ref(),
                    state.deps.protocol.inbox_storage.as_ref(),
                    state.deps.protocol.blocking_storage.as_ref(),
                    &first_party_service_jid,
                    batch_size,
                )
                .await
            {
                Ok(results) if !results.is_empty() => {
                    debug!(
                        drained = results.len(),
                        "Notification outbox janitor emitted durable XEP-0357 PubSub publish jobs"
                    );
                }
                Ok(_) => {}
                Err(error) => {
                    warn!(
                        error = %error,
                        "Notification outbox janitor failed; outbox jobs remain durable"
                    );
                }
            }
            let cutoff_ms = crate::time::now_ms()
                .saturating_sub(i64::from(retention_days) * 24 * 60 * 60 * 1_000);
            match state
                .deps
                .protocol
                .notification_outbox
                .prune_completed_before(cutoff_ms, prune_batch_size)
                .await
            {
                Ok(outcome) if outcome.total_deleted() > 0 => {
                    debug!(
                        candidates_deleted = outcome.candidates_deleted,
                        jobs_deleted = outcome.jobs_deleted,
                        "Notification outbox janitor pruned completed rows"
                    );
                }
                Ok(_) => {}
                Err(error) => {
                    warn!(
                        error = %error,
                        "Notification outbox janitor prune failed; completed rows remain durable"
                    );
                }
            }
            match state
                .deps
                .protocol
                .inbox_storage
                .prune_completed_groupchat_notification_recoveries(cutoff_ms, prune_batch_size)
                .await
            {
                Ok(deleted) if deleted > 0 => {
                    debug!(
                        deleted,
                        "Notification outbox janitor pruned completed groupchat notification recovery rows"
                    );
                }
                Ok(_) => {}
                Err(error) => {
                    warn!(
                        error = %error,
                        "Notification outbox janitor failed to prune completed groupchat notification recovery rows"
                    );
                }
            }
        }
    });
}

/// ADR-0017 Phase 3 Slice 10: feed the SM-session Q6 drain's outcomes into
/// the SAME `claims_released_on_drain`/`claims_abandoned_on_drain` counters
/// the generic per-entity room drain uses (`clustering::drain::
/// run_shutdown_drain`) — one shared observability surface for "how much of
/// this node's owned state made it out cleanly," regardless of which of
/// the two independent drain mechanisms (SM sessions vs. rooms) actually
/// drove a given entity. A no-op on a non-`clustering`-feature build: the
/// Q6 drain itself is unconditionally compiled (it works with or without
/// clustering, via `InProcessClaimStore`), but the Slice-10 metrics module
/// only exists behind the `clustering` Cargo feature.
fn record_sm_drain_outcome(released: bool) {
    #[cfg(feature = "clustering")]
    {
        if released {
            crate::clustering::metrics::record_claims_released_on_drain(1);
        } else {
            crate::clustering::metrics::record_claims_abandoned_on_drain(1);
        }
    }
    #[cfg(not(feature = "clustering"))]
    {
        let _ = released;
    }
}

pub(crate) fn spawn_graceful_shutdown_drain(
    websocket_state: Arc<WebSocketState>,
    drain_token: tokio_util::sync::CancellationToken,
    drain_notify: Arc<tokio::sync::Notify>,
) {
    tokio::spawn(async move {
        // Always notify_one on exit (success or early-return) so
        // the runtime's awaiting code never blocks indefinitely
        // on drain completion.
        struct NotifyOnDrop(Arc<tokio::sync::Notify>);
        impl Drop for NotifyOnDrop {
            fn drop(&mut self) {
                self.0.notify_one();
            }
        }
        let _notify_guard = NotifyOnDrop(drain_notify);

        drain_token.cancelled().await;
        // ADR-0017 Phase 3 Slice 10: this task's own start-of-drain
        // timestamp, fed into the SAME `drain_duration_ms` histogram the
        // generic per-entity room drain records into (below, once the Q6
        // portion completes) — one shared observability surface across
        // both independent drain mechanisms.
        #[cfg(feature = "clustering")]
        let sm_drain_started = std::time::Instant::now();
        // Issue #1091: live sessions observe the same stop token, send
        // <system-shutdown/> and detach into the SmSessionRegistry.
        // Wait for every connection guard to drop before the Q6 passes
        // below, so live sessions' unacked queues are in the registry
        // when drain_all_for_shutdown runs — otherwise the quiet window
        // could conclude before a slow session finishes detaching.
        //
        // One deadline covers BOTH phases (connection drain + Q6
        // promotion): the pod's terminationGracePeriodSeconds is sized
        // for a single WADDLE_DRAIN_TIMEOUT_SECS budget, so serializing
        // two full budgets would let SIGKILL truncate Q6 promotion.
        // The connection wait gets at most HALF the budget: a single
        // stuck peer must not consume the whole deadline and starve Q6
        // promotion for sessions that already detached cleanly — the
        // stuck session itself falls back to its durable SM row on
        // next startup either way.
        let total_budget = max_drain_duration_from_env();
        let drain_deadline = std::time::Instant::now() + total_budget;
        info!(
            active_connections = websocket_state.deps.shutdown.active_connections(),
            "Graceful shutdown: waiting for live sessions to close and detach"
        );
        if !websocket_state
            .deps
            .shutdown
            .wait_for_connections_drained_for(total_budget / 2)
            .await
        {
            warn!(
                remaining_connections = websocket_state.deps.shutdown.active_connections(),
                "Graceful shutdown: connection drain timed out; promoting \
                 whatever detached in time. Remaining sessions keep their \
                 durable SM rows for next-startup retry."
            );
        }
        info!("Graceful shutdown: starting SM session Q6 drain");
        const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);
        const QUIET_WINDOW_PASSES: u32 = 8;
        let mut empty_passes = 0u32;
        let mut total_drained = 0usize;
        loop {
            if std::time::Instant::now() >= drain_deadline {
                waddle_xmpp::prometheus::increment_sm_drain_timeout();
                // ADR-0017 Phase 3 Slice 10: whatever this node still
                // believes it owns at the timeout never even reached
                // `drain_all_for_shutdown` this pass — abandoned, same as
                // the generic per-entity drain's own budget-overrun path.
                let remaining = websocket_state
                    .deps
                    .protocol
                    .sm_session_registry
                    .live_session_ids()
                    .map(|ids| ids.len())
                    .unwrap_or(0);
                if remaining > 0 {
                    #[cfg(feature = "clustering")]
                    crate::clustering::metrics::record_claims_abandoned_on_drain(remaining as u64);
                }
                warn!(
                    total_drained,
                    remaining,
                    "Graceful shutdown: drain timeout reached. Remaining sessions \
                     keep their durable SM rows and will be retried on next startup \
                     via restore_from_persistence + Q6 expiry."
                );
                break;
            }
            let drained = match websocket_state
                .deps
                .protocol
                .sm_session_registry
                .drain_all_for_shutdown()
                .await
            {
                Ok(s) => s,
                Err(error) => {
                    warn!(error = %error, "Graceful shutdown: drain_all_for_shutdown failed");
                    break;
                }
            };
            if drained.is_empty() {
                empty_passes += 1;
                if empty_passes >= QUIET_WINDOW_PASSES {
                    break;
                }
                tokio::time::sleep(POLL_INTERVAL).await;
                continue;
            }
            empty_passes = 0;
            total_drained += drained.len();
            info!(
                count = drained.len(),
                "Graceful shutdown: promoting unacked queues for detached SM sessions"
            );
            for session in drained {
                let blocklist = match websocket_state
                    .deps
                    .protocol
                    .blocking_storage
                    .list_blocked_jid_entries(&session.jid.to_bare())
                    .await
                {
                    Ok(jids) => waddle_xmpp::protocol::session_state::Blocklist::new(jids),
                    Err(error) => {
                        warn!(
                            jid = %session.jid,
                            error = %error,
                            "Graceful shutdown: blocklist load failed; SKIPPING \
                             promotion to preserve fail-closed XEP-0191 policy. \
                             Durable SM row will be retried on next startup."
                        );
                        record_sm_drain_outcome(false);
                        continue;
                    }
                };
                // Round-2 review R2 + round-3 finding 1: per-session
                // recent-tombstone fetch so a retraction landing
                // mid-batch during the shutdown drain is still seen.
                let recent_tombstones = crate::sm_promotion::recent_tombstones_for_promotion(
                    &websocket_state.deps.protocol.sm_session_registry,
                    "Graceful shutdown",
                );
                let summary = crate::sm_promotion::promote_session_unacked(
                    &session,
                    &websocket_state.deps.protocol.connection_registry,
                    &websocket_state.deps.protocol.user_registry,
                    &websocket_state.deps.protocol.pending_delivery_storage,
                    &blocklist,
                    websocket_state.deps.auth_state.xmpp_domain.as_str(),
                    &recent_tombstones,
                )
                .await;
                // Finding B: same TOCTOU close-out as the SM janitor —
                // re-scrub pending rows for tombstones recorded during
                // this session's promotion window before confirming.
                crate::sm_promotion::scrub_pending_for_tombstones_recorded_during_promotion(
                    &websocket_state.deps.protocol.sm_session_registry,
                    &websocket_state.deps.protocol.pending_delivery_storage,
                    &recent_tombstones,
                    "Graceful shutdown",
                )
                .await;
                info!(
                    jid = %session.jid,
                    redelivered = summary.redelivered,
                    queued = summary.queued,
                    bounced = summary.bounced,
                    dropped = summary.dropped,
                    unparseable = summary.unparseable,
                    scrubbed = summary.scrubbed,
                    storage_failed = summary.storage_failed,
                    "Graceful shutdown: Q6 promotion completed for session"
                );
                if summary.has_storage_failure() {
                    warn!(
                        jid = %session.jid,
                        storage_failed = summary.storage_failed,
                        "Graceful shutdown: promotion had storage failures; \
                         preserving durable SM row for restart-time retry"
                    );
                    record_sm_drain_outcome(false);
                    continue;
                }
                let confirmed = websocket_state
                    .deps
                    .protocol
                    .sm_session_registry
                    .confirm_drained(&session.stream_id)
                    .await;
                // ADR-0017 Phase 3 Slice 10: this session's own "final
                // fenced write, then release" sequence — `confirm_drained`
                // deletes the durable row and releases the `ClaimStore`
                // claim only on success (see that method's own doc
                // comment).
                record_sm_drain_outcome(confirmed);
                let session_id =
                    waddle_xmpp::pending_delivery::SmSessionId::new(session.stream_id.clone());
                if let Err(error) = websocket_state
                    .deps
                    .protocol
                    .pending_delivery_storage
                    .release_claim(&session_id)
                    .await
                {
                    warn!(
                        jid = %session.jid,
                        stream_id = %session.stream_id,
                        error = %error,
                        "Graceful shutdown: pending_delivery release_claim failed; \
                         rows remain claimed and will be released by next-startup \
                         claim-expiry janitor"
                    );
                }
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        info!(
            total_drained,
            "Graceful shutdown: SM Q6 drain complete (iterative)"
        );
        #[cfg(feature = "clustering")]
        crate::clustering::metrics::record_drain_duration_ms(
            sm_drain_started.elapsed().as_secs_f64() * 1000.0,
        );

        // Drain the OIDC profile-publish tracker before notifying
        // shutdown complete. Each in-flight `ensure_pep_profile_published`
        // call is bounded by the fetcher's 25s timeout budget +
        // storage latency, so the wait is naturally bounded; calling
        // `close()` first prevents new publishes from racing in
        // during the drain. Without this wait the runtime can tear
        // down mid-step and leave the avatar in a split state
        // (empty `<metadata/>` published but vcard-temp PHOTO not
        // yet stripped — exactly the inconsistency XEP-0398 §3
        // forbids).
        let publish_tracker = websocket_state
            .deps
            .protocol
            .profile_publish_tracker
            .clone();
        publish_tracker.close();
        if !publish_tracker.is_empty() {
            info!(
                in_flight = publish_tracker.len(),
                "Graceful shutdown: awaiting in-flight OIDC profile publishes"
            );
        }
        publish_tracker.wait().await;
        info!("Graceful shutdown: OIDC profile-publish drain complete");

        // Drain in-flight provider webhook dispatch tasks so the ledger
        // status update (`mark_provider_delivery`) lands before teardown.
        // Rows that never reach dispatch (process kill between insert and
        // task start) still stay 'queued' — V1 has no sweep job.
        let dispatch_tracker = websocket_state.deps.provider_dispatch_tasks.clone();
        dispatch_tracker.close();
        if !dispatch_tracker.is_empty() {
            info!(
                in_flight = dispatch_tracker.len(),
                "Graceful shutdown: awaiting in-flight provider webhook dispatches"
            );
        }
        dispatch_tracker.wait().await;
        info!("Graceful shutdown: provider webhook dispatch drain complete");
    });
}

/// Per-sweep counts returned by [`sweep_auth_state_once`]. Exposed for
/// the unit tests that exercise the sweep without the live tokio
/// ticker.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct AuthSweepCounts {
    pub pending_pruned: usize,
    pub device_pruned: usize,
    pub xmpp_pruned: usize,
    pub pending_remaining: usize,
    pub device_remaining: usize,
    pub xmpp_remaining: usize,
}

/// Walk the three auth-state DashMaps and remove every entry whose
/// `is_expired()` reports `true`. Pure helper so the long-running
/// janitor in [`spawn_auth_state_janitor`] and the unit tests share
/// the same eviction logic.
pub(crate) fn sweep_auth_state_once(
    pending_auth: &dashmap::DashMap<String, crate::server::routes::auth::PendingAuthorization>,
    device_auth: &dashmap::DashMap<String, crate::server::routes::auth::DeviceAuthorization>,
    xmpp_auth_codes: &dashmap::DashMap<String, crate::server::routes::auth::XmppAuthCode>,
) -> AuthSweepCounts {
    let mut counts = AuthSweepCounts::default();
    pending_auth.retain(|_, entry| {
        if entry.is_expired() {
            counts.pending_pruned += 1;
            false
        } else {
            true
        }
    });
    device_auth.retain(|_, entry| {
        if entry.is_expired() {
            counts.device_pruned += 1;
            false
        } else {
            true
        }
    });
    xmpp_auth_codes.retain(|_, entry| {
        if entry.is_expired() {
            counts.xmpp_pruned += 1;
            false
        } else {
            true
        }
    });
    counts.pending_remaining = pending_auth.len();
    counts.device_remaining = device_auth.len();
    counts.xmpp_remaining = xmpp_auth_codes.len();
    counts
}

/// Sweep expired entries from the auth-state DashMaps (`pending_auth`,
/// `device_auth`, `xmpp_auth_codes`).
///
/// These maps grow on every started OAuth / device / XMPP-OAuth flow
/// and are removed only on the success path. Abandoned flows (network
/// flake, tab close, user typo) leave entries behind. Each carries an
/// `is_expired()` and the OAuth specs already bound the validity
/// window (10 minutes for `PendingAuthorization` / `XmppAuthCode`,
/// `device_auth.expires_at` for `DeviceAuthorization`), so the
/// janitor just consults that and removes anything past its window.
///
/// Runs on a 60 s ticker. Skips the first immediate tick so a fresh
/// process doesn't sweep before any auth flow has started.
pub(crate) fn spawn_auth_state_janitor(websocket_state: &Arc<WebSocketState>) {
    let weak_state = Arc::downgrade(websocket_state);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(AUTH_JANITOR_INTERVAL);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let Some(state) = weak_state.upgrade() else {
                break;
            };
            let auth = &state.deps.auth_state;
            let counts =
                sweep_auth_state_once(&auth.pending_auth, &auth.device_auth, &auth.xmpp_auth_codes);
            let total = counts.pending_pruned + counts.device_pruned + counts.xmpp_pruned;
            if total > 0 {
                info!(
                    pending_auth_pruned = counts.pending_pruned,
                    device_auth_pruned = counts.device_pruned,
                    xmpp_auth_codes_pruned = counts.xmpp_pruned,
                    pending_auth_remaining = counts.pending_remaining,
                    device_auth_remaining = counts.device_remaining,
                    xmpp_auth_codes_remaining = counts.xmpp_remaining,
                    "auth janitor: pruned expired entries"
                );
            } else {
                debug!(
                    pending_auth_remaining = counts.pending_remaining,
                    device_auth_remaining = counts.device_remaining,
                    xmpp_auth_codes_remaining = counts.xmpp_remaining,
                    "auth janitor: no expired entries"
                );
            }
        }
    });
}

/// Per-sweep counts returned by [`sweep_dormant_rooms_once`].
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct DormancySweepCounts {
    pub evicted: usize,
    pub examined: usize,
    pub remaining: usize,
}

/// Walk every registered MUC room and destroy the ones that report
/// dormant. Pure helper so the live janitor and unit tests share the
/// same logic. Each room ask is bounded — a hung room actor only
/// stalls its own check, never the sweep.
pub(crate) async fn sweep_dormant_rooms_once(
    websocket_state: &WebSocketState,
) -> DormancySweepCounts {
    use waddle_xmpp::muc::room_actor::{IsDormant, SealGuard};
    use waddle_xmpp::muc::room_registry_actor::{DestroyRoomIfInactive, ListRooms, RoomCount};
    let mut counts = DormancySweepCounts::default();
    let rooms = match websocket_state
        .deps
        .protocol
        .room_registry
        .ask(ListRooms)
        .await
    {
        Ok(list) => list,
        Err(error) => {
            warn!(error = ?error, "room dormancy janitor: ListRooms ask failed");
            return counts;
        }
    };
    counts.examined = rooms.len();
    for room_jid in rooms {
        let actor = match websocket_state
            .deps
            .protocol
            .room_registry
            .ask(waddle_xmpp::muc::room_registry_actor::GetRoom {
                room_jid: room_jid.clone(),
            })
            .await
        {
            Ok(Some(actor)) => actor,
            Ok(None) => continue,
            Err(error) => {
                warn!(
                    room = %room_jid,
                    error = ?error,
                    "room dormancy janitor: GetRoom ask failed; skipping"
                );
                continue;
            }
        };
        // Bounded ask: a hung room actor (e.g. wedged on hydration) must
        // only skip its own check — timeout lands in the existing
        // warn-and-skip arm, treating the room as not-dormant (never reap
        // on uncertainty).
        let status = match actor.ask(IsDormant).reply_timeout(REAPER_ASK_TIMEOUT).await {
            Ok(value) => value,
            Err(error) => {
                warn!(
                    room = %room_jid,
                    error = ?error,
                    "room dormancy janitor: IsDormant ask failed; skipping"
                );
                continue;
            }
        };
        if !status.dormant {
            continue;
        }
        // #1108: destruction is revision-guarded — the registry asks the
        // room actor to seal itself only if it is still dormant at the
        // probed occupancy revision (checked inside the room actor's
        // serialized mailbox), so a join that landed after the probe
        // above refuses the destroy instead of being orphaned.
        match websocket_state
            .deps
            .protocol
            .room_registry
            .ask(DestroyRoomIfInactive {
                room_jid: room_jid.clone(),
                expected_occupancy_revision: status.occupancy_revision,
                guard: SealGuard::Dormant,
            })
            .await
        {
            Ok(true) => {
                counts.evicted += 1;
                debug!(room = %room_jid, "room dormancy janitor: evicted dormant room");
            }
            Ok(false) => {}
            Err(error) => {
                warn!(
                    room = %room_jid,
                    error = ?error,
                    "room dormancy janitor: DestroyRoomIfInactive ask failed; will retry next pass"
                );
            }
        }
    }
    counts.remaining = websocket_state
        .deps
        .protocol
        .room_registry
        .ask(RoomCount)
        .await
        .unwrap_or(0);
    counts
}

/// Periodically evict fully-dormant MUC rooms (no occupants AND no
/// subject AND no pins AND no in-memory affiliations) so the
/// `RoomRegistryActor.rooms` map shrinks back to current
/// working-set rather than growing to the lifetime room set.
///
/// Eviction is safe for dormant rooms because re-entry through
/// `GetOrCreateRoom` spawns a fresh `RoomActor` with identical
/// initial state — see [`waddle_xmpp::muc::MucRoom::is_dormant`].
/// Rooms with subject text, pinned entries, or explicit affiliation
/// grants are intentionally NOT evicted here: those caches are
/// in-memory only and dropping them would lose user-visible state.
///
/// Runs on a 5-minute ticker. Skips the first immediate tick so the
/// process doesn't sweep before any room has been touched.
pub(crate) fn spawn_room_dormancy_janitor(websocket_state: &Arc<WebSocketState>) {
    let weak_state = Arc::downgrade(websocket_state);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(ROOM_DORMANCY_JANITOR_INTERVAL);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let Some(state) = weak_state.upgrade() else {
                break;
            };
            let counts = sweep_dormant_rooms_once(&state).await;
            if counts.evicted > 0 {
                info!(
                    examined = counts.examined,
                    evicted = counts.evicted,
                    remaining = counts.remaining,
                    "room dormancy janitor: evicted dormant rooms"
                );
            } else {
                debug!(
                    examined = counts.examined,
                    remaining = counts.remaining,
                    "room dormancy janitor: no dormant rooms"
                );
            }
        }
    });
}

/// Per-sweep counts returned by [`sweep_empty_user_actors_once`].
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct UserReaperSweepCounts {
    pub reaped: usize,
    pub examined: usize,
    pub remaining: usize,
}

/// Walk every registered `UserActor` and reap the ones that report zero
/// connected resources (ADR-0017 Phase 1 Slice 2, Copilot review on PR #1177).
///
/// Each user is reaped via the single atomic [`ReapUserIfEmpty`] registry
/// message — the emptiness check and the removal happen in one registry handler
/// so a concurrent re-registration cannot slip a resource in between (see that
/// message's docs). The janitor only drives the per-user asks; it never reads
/// then removes across two asks, which would reintroduce that race.
///
/// Every registry ask — `ListUsers`, each per-user `ReapUserIfEmpty`, and the
/// closing `UserCount` — is bounded by [`REAPER_ASK_TIMEOUT`], so a wedged or
/// backed-up registry stalls only that ask (logged, skipped) rather than
/// hanging the whole janitor task; the sweep resumes on the next interval.
///
/// Pure helper so the live reaper and unit tests share the same logic.
pub(crate) async fn sweep_empty_user_actors_once(
    websocket_state: &WebSocketState,
) -> UserReaperSweepCounts {
    use waddle_xmpp::registry::{ListUsers, ReapUserIfEmpty, UserCount};
    let mut counts = UserReaperSweepCounts::default();
    let user_registry = &websocket_state.deps.protocol.user_registry;
    let users = match user_registry
        .ask(ListUsers)
        .mailbox_timeout(REAPER_ASK_TIMEOUT)
        .reply_timeout(REAPER_ASK_TIMEOUT)
        .await
    {
        Ok(list) => list,
        Err(error) => {
            warn!(error = ?error, "user actor reaper: ListUsers ask failed");
            return counts;
        }
    };
    counts.examined = users.len();
    for bare_jid in users {
        match user_registry
            .ask(ReapUserIfEmpty {
                bare_jid: bare_jid.clone(),
            })
            .mailbox_timeout(REAPER_ASK_TIMEOUT)
            .reply_timeout(REAPER_ASK_TIMEOUT)
            .await
        {
            Ok(true) => {
                counts.reaped += 1;
                waddle_xmpp::prometheus::increment_user_actor_reaped();
                debug!(jid = %bare_jid, "user actor reaper: reaped empty UserActor");
            }
            Ok(false) => {}
            Err(error) => {
                warn!(
                    jid = %bare_jid,
                    error = ?error,
                    "user actor reaper: ReapUserIfEmpty ask failed; will retry next pass"
                );
            }
        }
    }
    counts.remaining = match user_registry
        .ask(UserCount)
        .mailbox_timeout(REAPER_ASK_TIMEOUT)
        .reply_timeout(REAPER_ASK_TIMEOUT)
        .await
    {
        Ok(count) => count,
        Err(error) => {
            // The closing count is a log-only diagnostic, so a failure is not
            // fatal — but surface it rather than reporting a misleading `0
            // remaining` that reads as "everything was reaped".
            warn!(error = ?error, "user actor reaper: UserCount ask failed; remaining is unknown");
            counts.examined.saturating_sub(counts.reaped)
        }
    };
    counts
}

/// Periodically reap empty `UserActor`s so `UserRegistryActor.users` shrinks
/// back to the current working set rather than growing to the lifetime user
/// set.
///
/// Required by the Slice 2 delivery cutover: production delivery now runs
/// through the actor's `TrySend*`, whose `try_deliver` evicts a closed-channel
/// resource. When that eviction removes a `UserActor`'s last resource without
/// the explicit `UnregisterConnectionAndReportEmpty` prune path running (a
/// dropped best-effort `mirror_unregister`), the empty actor would otherwise
/// linger forever. The `UserActor` deliberately does NOT self-prune on empty
/// (that races an in-flight re-registration and trips the crashed-actor poison
/// path); this out-of-band reaper is the safe alternative.
///
/// Runs on a 5-minute ticker. Skips the first immediate tick so the process
/// doesn't sweep before any resource has been registered.
pub(crate) fn spawn_user_actor_reaper(websocket_state: &Arc<WebSocketState>) {
    let weak_state = Arc::downgrade(websocket_state);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(USER_ACTOR_REAPER_INTERVAL);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let Some(state) = weak_state.upgrade() else {
                break;
            };
            let counts = sweep_empty_user_actors_once(&state).await;
            if counts.reaped > 0 {
                info!(
                    examined = counts.examined,
                    reaped = counts.reaped,
                    remaining = counts.remaining,
                    "user actor reaper: reaped empty UserActors"
                );
            } else {
                debug!(
                    examined = counts.examined,
                    remaining = counts.remaining,
                    "user actor reaper: no empty UserActors"
                );
            }
        }
    });
}

#[cfg(test)]
mod auth_janitor_tests {
    use super::sweep_auth_state_once;
    use crate::auth::providers::AuthProviderTokenEndpointAuthMethod;
    use crate::server::routes::auth::{
        BrowserSessionTransport, DeviceAuthStatus, DeviceAuthorization, PendingAuthorization,
        PendingFlow, XmppAuthCode,
    };
    use chrono::{Duration, Utc};
    use dashmap::DashMap;

    fn make_pending(state: &str, created_minutes_ago: i64) -> PendingAuthorization {
        PendingAuthorization {
            state: state.to_string(),
            provider_id: "p".to_string(),
            nonce: "n".to_string(),
            code_verifier: "cv".to_string(),
            redirect_uri: "https://example.test/cb".to_string(),
            client_id: "cid".to_string(),
            client_secret: String::new(),
            token_endpoint_auth_method: AuthProviderTokenEndpointAuthMethod::NoAuthentication,
            require_dpop: false,
            flow: PendingFlow::Browser {
                next: None,
                session_transport: BrowserSessionTransport::Cookie,
            },
            created_at: Utc::now() - Duration::minutes(created_minutes_ago),
        }
    }

    fn make_device(code: &str, expires_minutes_from_now: i64) -> DeviceAuthorization {
        DeviceAuthorization {
            device_code: code.to_string(),
            user_code: "user".to_string(),
            provider_id: "p".to_string(),
            expires_at: Utc::now() + Duration::minutes(expires_minutes_from_now),
            status: DeviceAuthStatus::Pending,
            session_id: None,
        }
    }

    fn make_xmpp_code(session_id: &str, created_minutes_ago: i64) -> XmppAuthCode {
        XmppAuthCode {
            session_id: session_id.to_string(),
            redirect_uri: "xmpp://example.test/cb".to_string(),
            code_challenge: None,
            created_at: Utc::now() - Duration::minutes(created_minutes_ago),
        }
    }

    #[test]
    fn sweep_removes_only_expired_entries() {
        let pending: DashMap<String, PendingAuthorization> = DashMap::new();
        // PendingAuthorization expires 10 minutes after creation.
        pending.insert("fresh".to_string(), make_pending("fresh", 1));
        pending.insert("stale".to_string(), make_pending("stale", 30));

        let device: DashMap<String, DeviceAuthorization> = DashMap::new();
        device.insert("live".to_string(), make_device("live", 5));
        device.insert("dead".to_string(), make_device("dead", -1));

        let xmpp: DashMap<String, XmppAuthCode> = DashMap::new();
        // XmppAuthCode expires 10 minutes after creation.
        xmpp.insert("fresh".to_string(), make_xmpp_code("fresh", 2));
        xmpp.insert("stale".to_string(), make_xmpp_code("stale", 20));

        let counts = sweep_auth_state_once(&pending, &device, &xmpp);

        assert_eq!(counts.pending_pruned, 1);
        assert_eq!(counts.device_pruned, 1);
        assert_eq!(counts.xmpp_pruned, 1);
        assert_eq!(counts.pending_remaining, 1);
        assert_eq!(counts.device_remaining, 1);
        assert_eq!(counts.xmpp_remaining, 1);

        assert!(pending.contains_key("fresh"));
        assert!(!pending.contains_key("stale"));
        assert!(device.contains_key("live"));
        assert!(!device.contains_key("dead"));
        assert!(xmpp.contains_key("fresh"));
        assert!(!xmpp.contains_key("stale"));
    }

    #[test]
    fn sweep_is_noop_when_all_entries_are_fresh() {
        let pending: DashMap<String, PendingAuthorization> = DashMap::new();
        pending.insert("a".to_string(), make_pending("a", 0));
        let device: DashMap<String, DeviceAuthorization> = DashMap::new();
        device.insert("b".to_string(), make_device("b", 30));
        let xmpp: DashMap<String, XmppAuthCode> = DashMap::new();
        xmpp.insert("c".to_string(), make_xmpp_code("c", 0));

        let counts = sweep_auth_state_once(&pending, &device, &xmpp);

        assert_eq!(counts.pending_pruned, 0);
        assert_eq!(counts.device_pruned, 0);
        assert_eq!(counts.xmpp_pruned, 0);
        assert_eq!(counts.pending_remaining, 1);
        assert_eq!(counts.device_remaining, 1);
        assert_eq!(counts.xmpp_remaining, 1);
    }
}

#[cfg(test)]
mod room_dormancy_tests {
    use super::sweep_dormant_rooms_once;
    use crate::server::routes::websocket::tests::create_test_websocket_state;
    use waddle_xmpp::muc::{
        room_actor::{
            ChangeAffiliation, GetOccupantByJid, Join, JoinAffiliationGrant, JoinWithAffiliation,
            LeaveByRealJid,
        },
        room_registry_actor::{CreateRoom, GetOrCreateRoom, RoomCount},
        RoomConfig,
    };
    use waddle_xmpp_core::{Affiliation, Role};

    fn full_jid(s: &str) -> jid::FullJid {
        s.parse().expect("valid full jid")
    }
    fn room_bare_jid(local: &str) -> jid::BareJid {
        format!("{local}@muc.example.com")
            .parse()
            .expect("bare jid")
    }

    #[tokio::test]
    async fn sweep_evicts_dormant_persistent_rooms() {
        let state = create_test_websocket_state().await;
        let room_jid = room_bare_jid("dormant");
        state
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
            .expect("create");

        let before: usize = state
            .deps
            .protocol
            .room_registry
            .ask(RoomCount)
            .await
            .expect("count");
        assert_eq!(before, 1);

        let counts = sweep_dormant_rooms_once(&state).await;
        assert_eq!(counts.examined, 1);
        assert_eq!(counts.evicted, 1);
        assert_eq!(counts.remaining, 0);
    }

    #[tokio::test]
    async fn sweep_skips_room_with_occupant() {
        let state = create_test_websocket_state().await;
        let room_jid = room_bare_jid("busy");
        let actor = state
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
            .expect("create");
        actor
            .ask(Join {
                nick: "alice".to_string(),
                real_jid: full_jid("alice@example.com/r1"),
                role: Role::Participant,
                affiliation: Affiliation::Member,
            })
            .await
            .expect("join");

        let counts = sweep_dormant_rooms_once(&state).await;
        assert_eq!(counts.examined, 1);
        assert_eq!(counts.evicted, 0);
        assert_eq!(counts.remaining, 1);
    }

    #[tokio::test]
    async fn sweep_skips_room_with_affiliation_grant() {
        let state = create_test_websocket_state().await;
        let room_jid = room_bare_jid("graced");
        let actor = state
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
            .expect("create");
        actor
            .ask(ChangeAffiliation {
                jid: "alice@example.com".parse().expect("bare jid"),
                affiliation: Affiliation::Admin,
            })
            .await
            .expect("change affiliation");

        let counts = sweep_dormant_rooms_once(&state).await;
        assert_eq!(counts.evicted, 0);
        assert_eq!(counts.remaining, 1);
    }

    #[tokio::test]
    async fn sweep_evicts_room_after_last_occupant_leaves() {
        let state = create_test_websocket_state().await;
        let room_jid = room_bare_jid("emptied");
        let actor = state
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
            .expect("create");
        let alice = full_jid("alice@example.com/r1");
        actor
            .ask(Join {
                nick: "alice".to_string(),
                real_jid: alice.clone(),
                role: Role::Participant,
                affiliation: Affiliation::Member,
            })
            .await
            .expect("join");
        actor
            .ask(LeaveByRealJid { sender_jid: alice })
            .await
            .expect("leave")
            .expect("outcome");

        let counts = sweep_dormant_rooms_once(&state).await;
        assert_eq!(counts.evicted, 1);
        assert_eq!(counts.remaining, 0);
    }

    /// #1110: a resolver-derived member affiliation (written by every
    /// managed-channel join) must NOT pin the room in the registry —
    /// once the last member leaves, the sweep evicts the room. Before
    /// the fix this leaked one room actor per ever-visited channel.
    #[tokio::test]
    async fn sweep_evicts_room_with_only_resolver_derived_affiliation() {
        let state = create_test_websocket_state().await;
        let room_jid = room_bare_jid("resolver-graced");
        let actor = state
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
            .expect("create");
        let alice = full_jid("alice@example.com/r1");
        actor
            .ask(JoinWithAffiliation {
                sender_jid: alice.clone(),
                nick: "alice".to_string(),
                affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
                local_domain: "example.com".to_string(),
                admission_revision: 0,
            })
            .await
            .expect("resolver-derived member join");
        actor
            .ask(LeaveByRealJid { sender_jid: alice })
            .await
            .expect("leave")
            .expect("outcome");

        let counts = sweep_dormant_rooms_once(&state).await;
        assert_eq!(
            counts.evicted, 1,
            "resolver-derived affiliations are re-derived on the next \
             join and must not block dormancy eviction (#1110)"
        );
        assert_eq!(counts.remaining, 0);
    }

    /// #1110: eviction of a resolver-affiliated room is lossless — a
    /// rejoin re-spawns the room through the registry and the resolver
    /// re-applies the member affiliation.
    #[tokio::test]
    async fn rejoin_after_dormancy_eviction_re_resolves_affiliation() {
        let state = create_test_websocket_state().await;
        let room_jid = room_bare_jid("re-resolved");
        let actor = state
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
            .expect("create");
        let alice = full_jid("alice@example.com/r1");
        actor
            .ask(JoinWithAffiliation {
                sender_jid: alice.clone(),
                nick: "alice".to_string(),
                affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
                local_domain: "example.com".to_string(),
                admission_revision: 0,
            })
            .await
            .expect("first join");
        actor
            .ask(LeaveByRealJid {
                sender_jid: alice.clone(),
            })
            .await
            .expect("leave")
            .expect("outcome");
        let counts = sweep_dormant_rooms_once(&state).await;
        assert_eq!(counts.evicted, 1, "room evicted while dormant");

        // Rejoin: the registry re-spawns a fresh actor and the join
        // path re-applies the resolver-derived affiliation.
        let respawned = state
            .deps
            .protocol
            .room_registry
            .ask(GetOrCreateRoom {
                room_jid: room_jid.clone(),
                waddle_id: "w".to_string(),
                channel_id: "c".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("respawn room")
            .actor_ref;
        respawned
            .ask(JoinWithAffiliation {
                sender_jid: alice.clone(),
                nick: "alice".to_string(),
                affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
                local_domain: "example.com".to_string(),
                admission_revision: 0,
            })
            .await
            .expect("rejoin after eviction");
        let occupant = respawned
            .ask(GetOccupantByJid { jid: alice })
            .await
            .expect("occupant lookup")
            .expect("alice is an occupant again");
        assert_eq!(
            occupant.affiliation,
            Affiliation::Member,
            "the resolver-derived member affiliation is re-applied on rejoin"
        );
    }
}

#[cfg(test)]
mod user_reaper_tests {
    use super::sweep_empty_user_actors_once;
    use crate::server::routes::websocket::tests::create_test_websocket_state;
    use waddle_xmpp::registry::{
        ConnectionEntry, GetUser, RegisterUserResource, TrySendPeer, UserCount,
    };

    fn full_jid(s: &str) -> jid::FullJid {
        s.parse().expect("valid full jid")
    }

    fn sample_stanza(to: &jid::FullJid) -> waddle_xmpp::Stanza {
        let mut msg = xmpp_parsers::message::Message::new(Some(jid::Jid::from(to.clone())));
        msg.type_ = xmpp_parsers::message::MessageType::Chat;
        msg.bodies
            .insert(xmpp_parsers::message::Lang::new(), "hi".to_string());
        waddle_xmpp::Stanza::Message(msg)
    }

    /// End-to-end sweep over the actor tree: register a resource, force the
    /// production closed-channel eviction so the actor is empty-but-registered,
    /// then assert the sweep reaps it, its accounting is correct, and the
    /// `waddle_user_actor_reaped_total` metric fires.
    #[tokio::test]
    async fn sweep_reaps_orphaned_empty_user_actor() {
        // Hold the metrics lock for the whole check: the reaped counter is a
        // process-wide global, so serialise against other metric-asserting
        // tests and reset first for a clean baseline.
        let _guard = waddle_xmpp::prometheus::metrics_test_lock().lock().await;
        waddle_xmpp::prometheus::reset_metrics_for_test();

        let state = create_test_websocket_state().await;
        let jid = full_jid("alice@example.com/web");
        let user_registry = &state.deps.protocol.user_registry;

        let (tx, rx) = tokio::sync::mpsc::channel(16);
        user_registry
            .ask(RegisterUserResource {
                jid: jid.clone(),
                entry: ConnectionEntry::new(tx),
            })
            .await
            .expect("register");

        // Close the channel and drive one delivery so try_deliver evicts the
        // last resource — the actor is now empty but still registered.
        drop(rx);
        let actor = user_registry
            .ask(GetUser {
                bare_jid: jid.to_bare(),
            })
            .await
            .expect("get user")
            .expect("actor exists");
        actor
            .ask(TrySendPeer {
                jid: jid.clone(),
                stanza: sample_stanza(&jid),
            })
            .await
            .expect("try send");
        assert_eq!(user_registry.ask(UserCount).await.expect("count"), 1);

        let counts = sweep_empty_user_actors_once(&state).await;
        assert_eq!(counts.examined, 1);
        assert_eq!(counts.reaped, 1);
        assert_eq!(counts.remaining, 0);

        assert!(
            waddle_xmpp::prometheus::render_metrics().contains("waddle_user_actor_reaped_total 1"),
            "the reaper must increment waddle_user_actor_reaped_total"
        );
    }

    /// A user with a live resource is examined but never reaped.
    #[tokio::test]
    async fn sweep_keeps_user_with_live_resource() {
        let state = create_test_websocket_state().await;
        let jid = full_jid("bob@example.com/web");
        let user_registry = &state.deps.protocol.user_registry;

        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        user_registry
            .ask(RegisterUserResource {
                jid: jid.clone(),
                entry: ConnectionEntry::new(tx),
            })
            .await
            .expect("register");

        let counts = sweep_empty_user_actors_once(&state).await;
        assert_eq!(counts.examined, 1);
        assert_eq!(counts.reaped, 0);
        assert_eq!(counts.remaining, 1);
    }
}

/// Interval for the remote-MUC-membership reconciliation janitor
/// (#1249). 30s bounds how long a ghost occupant survives a failed
/// disconnect-cleanup relay while keeping the sweep trivially cheap
/// (an in-memory DashMap scan; the relay only runs for entries whose
/// occupant has no local presence at all).
#[cfg(feature = "clustering")]
const REMOTE_MUC_MEMBERSHIP_RECONCILE_INTERVAL: Duration = Duration::from_secs(30);

/// Collect the occupants whose remote MUC memberships need a cleanup
/// re-drive (#1249): an ACTIVE membership entry whose occupant full JID
/// has neither a live connection-registry entry nor a detached
/// XEP-0198 session on this node. Such an entry can only be the residue
/// of a failed (or missed) disconnect cleanup — the join path records
/// memberships strictly while the connection is registered, and both
/// graceful-leave and successful cleanup forget them.
///
/// A detached-but-resumable session keeps its occupancy on purpose
/// (XEP-0198 resume re-attaches to the same room state), so it is NOT a
/// candidate; SM expiry runs its own cleanup pass which restores the
/// membership on failure and thereby feeds this janitor.
#[cfg(feature = "clustering")]
async fn collect_remote_muc_reconcile_candidates(state: &WebSocketState) -> Vec<jid::FullJid> {
    let mut candidates = Vec::new();
    for occupant in state
        .deps
        .protocol
        .remote_muc_memberships
        .occupants_with_active_memberships()
    {
        if state
            .deps
            .protocol
            .connection_registry
            .get_entry(&occupant)
            .is_some()
        {
            continue;
        }
        match state
            .deps
            .protocol
            .sm_session_registry
            .detached_resources_for_user(&occupant.to_bare())
            .await
        {
            Ok(detached) if detached.contains(&occupant) => continue,
            Ok(_) => {}
            // Fail-closed: on a registry read error, skip this occupant
            // for this sweep rather than risk evicting a resumable
            // session's occupancy.
            Err(error) => {
                warn!(
                    jid = %occupant,
                    %error,
                    "remote MUC reconciler: detached-session lookup failed; skipping"
                );
                continue;
            }
        }
        candidates.push(occupant);
    }
    candidates
}

/// #1249: periodically re-drive remote MUC unavailable relays whose
/// disconnect-time attempt failed (remote node unreachable, claim
/// lookup failure, origin `UserActor` claim held by another node).
/// `cleanup_remote_muc_presence` restores the membership snapshot on
/// every failed relay, so this janitor retries until the remote side
/// recovers — making cross-node occupancy cleanup convergent instead of
/// one-shot (the root cause of the recurring production error
/// `failed to relay remote MUC unavailable during disconnect cleanup`).
#[cfg(feature = "clustering")]
pub(crate) fn spawn_remote_muc_membership_reconciler(websocket_state: &Arc<WebSocketState>) {
    let weak_state = Arc::downgrade(websocket_state);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(REMOTE_MUC_MEMBERSHIP_RECONCILE_INTERVAL);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let Some(state) = weak_state.upgrade() else {
                break;
            };
            let candidates = collect_remote_muc_reconcile_candidates(&state).await;
            if candidates.is_empty() {
                continue;
            }
            info!(
                candidates = candidates.len(),
                "remote MUC reconciler: re-driving unavailable relays for departed occupants"
            );
            for occupant in candidates {
                routes::websocket::redrive_remote_muc_cleanup(&state, &occupant).await;
            }
        }
    });
}

#[cfg(all(test, feature = "clustering"))]
mod remote_muc_reconciler_tests {
    use super::collect_remote_muc_reconcile_candidates;
    use crate::server::routes::websocket::tests::create_test_websocket_state;
    use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};

    fn room(local: &str) -> jid::BareJid {
        format!("{local}@muc.example.com")
            .parse()
            .expect("room jid")
    }

    fn detached_session(stream_id: &str, jid: jid::FullJid) -> DetachedSession {
        DetachedSession {
            stream_id: stream_id.to_string(),
            user_id: jid.to_bare().to_string(),
            jid,
            inbound_count: 0,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(120),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        }
    }

    /// #1249: an ACTIVE remote membership whose occupant has neither a
    /// live connection nor a detached SM session is a re-drive
    /// candidate; live and detached occupants are skipped (their
    /// occupancy is legitimate), and a tombstoned membership (cleanup
    /// in flight) is not re-driven.
    #[tokio::test]
    async fn reconciler_targets_only_fully_departed_occupants() {
        let state = create_test_websocket_state().await;
        let memberships = &state.deps.protocol.remote_muc_memberships;

        // Fully departed: candidate.
        let ghost: jid::FullJid = "ghost@example.com/web".parse().unwrap();
        memberships.record_join(&ghost, &room("ghost-room"), "ghost");

        // Live connection: not a candidate.
        let live: jid::FullJid = "live@example.com/web".parse().unwrap();
        memberships.record_join(&live, &room("live-room"), "live");
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let _owner = state
            .deps
            .protocol
            .connection_registry
            .register(live.clone(), tx);

        // Detached-but-resumable session: not a candidate.
        let detached: jid::FullJid = "detached@example.com/web".parse().unwrap();
        memberships.record_join(&detached, &room("detached-room"), "detached");
        state
            .deps
            .protocol
            .sm_session_registry
            .store_session(detached_session("stream-detached", detached.clone()))
            .await
            .expect("store detached session");

        // Tombstoned membership (cleanup already in flight): no ACTIVE
        // entry, so not a candidate.
        let tombstoned: jid::FullJid = "tombstoned@example.com/web".parse().unwrap();
        memberships.record_join(&tombstoned, &room("tomb-room"), "tombstoned");
        let taken = memberships.take_for_occupant(&tombstoned);
        assert_eq!(taken.len(), 1);

        let candidates = collect_remote_muc_reconcile_candidates(&state).await;
        assert_eq!(
            candidates,
            vec![ghost],
            "only the fully departed occupant is re-driven"
        );
    }
}
