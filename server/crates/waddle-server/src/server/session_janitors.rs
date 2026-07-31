use crate::room_policy::RoomRegistryActorPolicy;
use crate::server::routes;
use crate::server::routes::websocket::WebSocketState;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, error, info, warn, Instrument};
use waddle_xmpp::telemetry::attributes::{Janitor, MetricAttribute, SweepOutcome};

/// Named root span for one janitor sweep tick (#1483).
///
/// Janitor loops run with no active span, so the actor.handle_message
/// spans their asks mint would root the trace — exactly the shape the
/// #1438 span-noise sampler drops. One root per sweep (never per actor
/// message) keeps the sweep's actor work parented and traceable.
/// `parent: None` guarantees root semantics even if a caller is
/// instrumented. The span name is documented in `telemetry::span_noise`
/// and must never be added to its suppression lists.
fn janitor_sweep_span(janitor: Janitor) -> tracing::Span {
    tracing::info_span!(parent: None, "janitor.sweep", janitor = janitor.value())
}

/// The active span's OTel context, captured at work-enqueue time so a
/// later attempt can *link* back to the sweep that found the work.
#[cfg(feature = "clustering")]
fn current_sweep_context() -> opentelemetry::trace::SpanContext {
    use opentelemetry::trace::TraceContextExt;
    use tracing_opentelemetry::OpenTelemetrySpanExt;
    tracing::Span::current()
        .context()
        .span()
        .span_context()
        .clone()
}

/// Root span for one orphan-reaper work-item attempt (#1483).
///
/// Worker attempts must NOT run under the enqueuing sweep's live span:
/// retry queues and pending inventories would hold that root open
/// indefinitely — unbounded duration, lost on crash, the very
/// `actor.lifecycle` pathology the #1438 sampler kills. Each attempt
/// gets its own short-lived root instead, linked (not parented) to the
/// enqueuing sweep when its context is valid. The span name is
/// documented in `telemetry::span_noise` and must never be added to its
/// suppression lists.
#[cfg(feature = "clustering")]
fn orphan_work_span(
    lane: &'static str,
    sweep_context: &opentelemetry::trace::SpanContext,
) -> tracing::Span {
    use tracing_opentelemetry::OpenTelemetrySpanExt;
    let span = tracing::info_span!(
        parent: None,
        "janitor.orphan_work",
        janitor = Janitor::OrphanReaper.value(),
        lane,
    );
    if sweep_context.is_valid() {
        span.add_link(sweep_context.clone());
    }
    span
}

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

/// Per-sweep cap for the stale-node watchdog that runs before the orphaned
/// SM-session claim scan. This bounds the raw-heartbeat discovery pass; each
/// candidate still has to pass `NodeLeaseStore::expire`'s CAS before any
/// claim can be stolen.
#[cfg(feature = "clustering")]
const STALE_NODE_WATCHDOG_CANDIDATE_LIMIT: usize = 64;

/// Per-sweep bound for proactive `RoomActor` orphan reconciliation. Room
/// claims are cheap rows but adoption can hydrate durable state, so the
/// janitor deliberately amortizes a large node loss across sweeps.
#[cfg(feature = "clustering")]
const ORPHANED_ROOM_CANDIDATE_LIMIT: usize = 64;

#[cfg(feature = "clustering")]
const ORPHANED_ROOM_RELEASE_TIMEOUT: Duration = Duration::from_secs(1);

#[cfg(feature = "clustering")]
const ORPHANED_SM_SCAN_TIMEOUT: Duration = Duration::from_secs(5);

#[cfg(feature = "clustering")]
const ORPHAN_WORK_QUEUE_CAPACITY: usize = 128;

#[cfg(feature = "clustering")]
const ORPHAN_WORK_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(5);

/// Hard wall-clock bound for one outer orphan-reconciliation pass. Individual
/// hydration and release jobs have their own tighter deadlines, but the sweep
/// also awaits lease-store and RoomRegistry operations. If any of those
/// dependencies wedges after a claim steal, continuing to advertise this
/// node's lease could strand authority with no retained actor responsibility.
/// Self-fence on expiry so another node can recover the exact claim.
#[cfg(feature = "clustering")]
const ORPHAN_REAPER_SWEEP_TIMEOUT: Duration = Duration::from_secs(60);
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
            run_sm_expiry_sweep(&state).await;
        }
    });
}

async fn run_sm_expiry_sweep(state: &Arc<WebSocketState>) {
    async {
        let mut sweep_failed = false;
        let drained: Vec<waddle_xmpp::stream_management::DetachedSession> = match state
            .deps
            .protocol
            .sm_session_registry
            .drain_expired()
            .await
        {
            Ok(sessions) => sessions,
            Err(err) => {
                sweep_failed = true;
                warn!(error = %err, "SM janitor: drain_expired failed");
                Vec::new()
            }
        };
        if !drained.is_empty() {
            info!(
                count = drained.len(),
                "SM janitor: cleaning up expired detached sessions"
            );
        }
        let mut promotion_batch = crate::sm_promotion::PromotionBatchGuard::new(
            &state.deps.protocol.sm_session_registry,
            drained,
        );
        while let Some(pending_session) = promotion_batch.pop() {
            let mut promotion_guard = crate::sm_promotion::PromotionSessionGuard::new(
                &state.deps.protocol.sm_session_registry,
                pending_session,
            );
            let session = promotion_guard.session().clone();
            let blocklist = match state
                .deps
                .protocol
                .blocking_storage
                .list_blocked_jid_entries(&session.jid.to_bare())
                .await
            {
                Ok(jids) => waddle_xmpp::protocol::session_state::Blocklist::new(jids),
                Err(error) => {
                    sweep_failed = true;
                    waddle_xmpp::telemetry::reliability::increment_sm_promotion_blocklist_failed();
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
                            if crate::sm_promotion::reinsert_failed_session_for_retry(
                                &state.deps.protocol.sm_session_registry,
                                session.clone(),
                            )
                            .await
                            {
                                promotion_guard.complete();
                            }
                            continue;
                        }
                    };
                    if attempts >= max_promotion_attempts_from_env() {
                        waddle_xmpp::telemetry::reliability::increment_sm_promotion_dead_lettered();
                        error!(
                            jid = %session.jid,
                            stream_id = %session.stream_id,
                            attempts,
                            error = %error,
                            "SM janitor: blocklist load has repeatedly failed; \
                             dead-lettering the durable row to break the retry loop"
                        );
                        if state
                            .deps
                            .protocol
                            .sm_session_registry
                            .confirm_drained(&session.stream_id)
                            .await
                        {
                            promotion_guard.complete();
                        }
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
                    if crate::sm_promotion::reinsert_failed_session_for_retry(
                        &state.deps.protocol.sm_session_registry,
                        session.clone(),
                    )
                    .await
                    {
                        promotion_guard.complete();
                    }
                    continue;
                }
            };
            // Round-2 review R2 + round-3 finding 1: retractions
            // racing this drain window are invisible to the registry
            // scrub (the sessions are off both maps); fetch the
            // recent-tombstone record PER SESSION, immediately
            // before this session's promotion, so even a retraction
            // landing mid-batch is still seen.
            let recent_tombstones = match crate::sm_promotion::recent_tombstones_for_promotion(
                &state.deps.protocol.sm_session_registry,
                "SM janitor",
            ) {
                Ok(records) => records,
                Err(_) => {
                    sweep_failed = true;
                    Vec::new()
                }
            };
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
            if crate::sm_promotion::scrub_pending_for_tombstones_recorded_during_promotion(
                &state.deps.protocol.sm_session_registry,
                &state.deps.protocol.pending_delivery_storage,
                &recent_tombstones,
                "SM janitor",
            )
            .await
                == crate::sm_promotion::PromotionScrubOutcome::Failed
            {
                sweep_failed = true;
            }
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
                sweep_failed = true;
                waddle_xmpp::telemetry::reliability::add_sm_promotion_storage_failed(
                    u64::from(summary.storage_failed),
                );
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
                        if crate::sm_promotion::prune_promoted_then_reinsert_for_retry(
                            &state.deps.protocol.sm_session_registry,
                            session.clone(),
                            &summary,
                        )
                        .await
                        {
                            promotion_guard.complete();
                        }
                        continue;
                    }
                };
                if attempts >= max_promotion_attempts_from_env() {
                    waddle_xmpp::telemetry::reliability::increment_sm_promotion_dead_lettered();
                    error!(
                        jid = %session.jid,
                        stream_id = %session.stream_id,
                        attempts,
                        storage_failed = summary.storage_failed,
                        "SM janitor: Q6 promotion repeatedly failed; \
                         dead-lettering the durable row to break the retry loop"
                    );
                    if state
                        .deps
                        .protocol
                        .sm_session_registry
                        .confirm_drained(&session.stream_id)
                        .await
                    {
                        promotion_guard.complete();
                    }
                    continue;
                }
                warn!(
                    jid = %session.jid,
                    attempts,
                    storage_failed = summary.storage_failed,
                    "SM janitor: promotion had storage failures; \
                     preserving session state for retry"
                );
                if crate::sm_promotion::prune_promoted_then_reinsert_for_retry(
                    &state.deps.protocol.sm_session_registry,
                    session.clone(),
                    &summary,
                )
                .await
                {
                    promotion_guard.complete();
                }
                continue;
            }
            if state
                .deps
                .protocol
                .sm_session_registry
                .confirm_drained(&session.stream_id)
                .await
            {
                promotion_guard.complete();
            } else {
                sweep_failed = true;
                continue;
            }
            let session_id =
                waddle_xmpp::pending_delivery::SmSessionId::new(session.stream_id.clone());

            // Same replacement re-check as the unclean-disconnect
            // path: a fresh bind that superseded this expired
            // detached session broadcasts its own presence, so a
            // late unavailable would pin subscribers on offline for
            // an online JID.
            if routes::websocket::broadcast_unavailable_if_no_replacement(
                state,
                &session.jid,
                session.presence_available,
            )
            .await
                == routes::websocket::handlers::presence::TerminatedPresenceBroadcastOutcome::Failed
            {
                sweep_failed = true;
            }
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
                if crate::server::dual_registration::mirror_unregister(
                    &state.deps.protocol.user_registry,
                    &session.jid,
                    Some(std::sync::Arc::clone(&entry.carbons_enabled)),
                )
                .await
                    == crate::server::dual_registration::MirrorUnregisterOutcome::Failed
                {
                    sweep_failed = true;
                }
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
                    if routes::websocket::cleanup_muc_presence_for_jid_with_origin(
                        state,
                        &session.jid,
                        cleanup_origin,
                    )
                    .await
                        == routes::websocket::MucCleanupOutcome::Failed
                    {
                        sweep_failed = true;
                    }
                }
                #[cfg(not(feature = "clustering"))]
                {
                    if routes::websocket::cleanup_muc_presence_for_jid(&state, &session.jid)
                        .await
                        == routes::websocket::MucCleanupOutcome::Failed
                    {
                        sweep_failed = true;
                    }
                }
            }
            if let Err(error) = state
                .deps
                .protocol
                .pending_delivery_storage
                .release_claim(&session_id)
                .await
            {
                sweep_failed = true;
                warn!(
                    jid = %session.jid,
                    stream_id = %session.stream_id,
                    error = %error,
                    "SM janitor: pending_delivery release_claim failed; \
                     rows remain claimed and will be released by claim-expiry janitor"
                );
            }
        }
        if !retry_pending_sm_ownership(state).await {
            sweep_failed = true;
        }
        waddle_xmpp::telemetry::reliability::record_janitor_sweep(
            Janitor::SmExpiry,
            if sweep_failed {
                SweepOutcome::Failed
            } else {
                SweepOutcome::Completed
            },
        );
    }
    .instrument(janitor_sweep_span(Janitor::SmExpiry))
    .await;
}

async fn retry_pending_sm_ownership(state: &WebSocketState) -> bool {
    const COMBINED_RETRY_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);
    let registry = &state.deps.protocol.sm_session_registry;
    let hydration = async {
        registry.retry_pending_reclaimed_hydrations(64).await;
    };
    let releases = async {
        registry.retry_pending_claim_releases(64).await;
    };
    let completed = run_sm_retries_with_budget(COMBINED_RETRY_BUDGET, hydration, releases).await;
    if !completed {
        warn!(
            budget = ?COMBINED_RETRY_BUDGET,
            "SM janitor: ownership reconciliation exhausted its post-expiry tick budget"
        );
    }
    completed
}

async fn run_sm_retries_with_budget<H, R>(
    budget: std::time::Duration,
    hydration: H,
    releases: R,
) -> bool
where
    H: std::future::Future<Output = ()>,
    R: std::future::Future<Output = ()>,
{
    // Both inventories retain responsibility until their individual item
    // succeeds or ownership is disproved, so cancelling this joined tail at
    // the tick deadline is safe. Polling concurrently prevents a degraded
    // hydration backend from starving exact terminal releases every tick.
    tokio::time::timeout(budget, async {
        tokio::join!(hydration, releases);
    })
    .await
    .is_ok()
}

#[cfg(test)]
mod sm_retry_budget_tests {
    use super::run_sm_retries_with_budget;

    #[tokio::test(start_paused = true)]
    async fn a_stalled_hydration_retry_does_not_starve_release_retry() {
        let release_polled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let release_observer = release_polled.clone();
        let completed = run_sm_retries_with_budget(
            std::time::Duration::from_secs(5),
            std::future::pending(),
            async move {
                release_observer.store(true, std::sync::atomic::Ordering::SeqCst);
            },
        )
        .await;

        assert!(!completed);
        assert!(release_polled.load(std::sync::atomic::Ordering::SeqCst));
    }
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
        let registry = websocket_state.deps.protocol.sm_session_registry.clone();
        let Some(stop) = websocket_state
            .deps
            .app_state
            .clustering_claims
            .stop_token
            .clone()
        else {
            // The binary may include clustering support while runtime
            // clustering is disabled. In that configuration no clustering
            // supervisor or registry-lifetime watcher may be attached to the
            // process-wide shutdown token.
            return;
        };
        let Some(fatal_fence) = websocket_state
            .deps
            .app_state
            .clustering_claims
            .fatal_fence
            .clone()
        else {
            return;
        };
        let room_registry_actor = websocket_state.deps.protocol.room_registry.clone();
        let node_lifecycle = websocket_state.deps.app_state.node_lifecycle.clone();
        tokio::spawn(async move {
            let terminal_room_registry =
                waddle_xmpp::muc::RoomRegistry::wrap(room_registry_actor.clone());
            let mut supervisor = OrphanReaperSupervisor::new_with_fatal_fence(
                registry.clone(),
                stop.clone(),
                fatal_fence,
                node_lifecycle,
            );
            let mut ticker = tokio::time::interval(interval);
            // Skip the first (immediate) tick, mirroring the other janitors
            // — no need to sweep before the node-lease loop has even
            // registered this node.
            ticker.tick().await;
            loop {
                tokio::select! {
                    _ = stop.cancelled() => break,
                    _ = supervisor.workers.fatal_fence.cancelled() => break,
                    _ = ticker.tick() => {}
                }
                if !supervisor.is_healthy() {
                    error!("orphan reaper worker exited; restarting workers with retained work");
                    supervisor = supervisor.restarted(registry.clone(), stop.clone()).await;
                    if supervisor.workers.fatal_fence.is_cancelled() {
                        waddle_xmpp::telemetry::reliability::record_janitor_sweep(
                            Janitor::OrphanReaper,
                            SweepOutcome::Failed,
                        );
                        break;
                    }
                }
                let Some(state) = weak_state.upgrade() else {
                    break;
                };
                let sweep_outcome = supervise_orphan_reaper_sweep(
                    &supervisor.workers.cancel,
                    &supervisor.workers.fatal_fence,
                    &supervisor.workers.node_lifecycle,
                    ORPHAN_REAPER_SWEEP_TIMEOUT,
                    run_orphan_reaper_sweep_with_workers(&state, &supervisor.workers),
                )
                .await;
                let mut workers_healthy = supervisor.is_healthy();
                let mut stop_after_sweep = false;
                match sweep_outcome {
                    OrphanSweepOutcome::Completed | OrphanSweepOutcome::Failed => {}
                    OrphanSweepOutcome::Cancelled => {
                        stop_after_sweep = true;
                    }
                    OrphanSweepOutcome::TimedOut => {
                        error!(
                            timeout = ?ORPHAN_REAPER_SWEEP_TIMEOUT,
                            "orphan reaper sweep timed out; self-fencing node because claim handoff state may be uncertain"
                        );
                        stop_after_sweep = true;
                    }
                }
                if !stop_after_sweep && !workers_healthy {
                    error!("orphan reaper worker exited during sweep; restarting workers with retained work");
                    supervisor = supervisor.restarted(registry.clone(), stop.clone()).await;
                    if supervisor.workers.fatal_fence.is_cancelled() {
                        stop_after_sweep = true;
                    }
                    workers_healthy = false;
                }
                // A shutdown-cancelled sweep is neither completed nor failed;
                // skipping the heartbeat keeps routine deploys from
                // registering failed sweeps (an absent heartbeat during
                // shutdown is the honest signal).
                if sweep_outcome != OrphanSweepOutcome::Cancelled {
                    waddle_xmpp::telemetry::reliability::record_janitor_sweep(
                        Janitor::OrphanReaper,
                        orphan_sweep_heartbeat_outcome(sweep_outcome, workers_healthy),
                    );
                }
                if stop_after_sweep {
                    break;
                }
            }
            // Snapshot and stop SM/release workers before any other terminal
            // await. A fatal fence must not leave a live worker able to start
            // another ownership CAS while RoomRegistry cleanup is in flight.
            let pending_room_handoffs = supervisor.shutdown_terminal().await;
            match terminal_room_registry
                .drain_room_ownership_for_shutdown(pending_room_handoffs)
                .await
            {
                Ok(outcome) if outcome.retained > 0 => {
                    error!(released = outcome.released, preserved_live = outcome.preserved_live, retained = outcome.retained, "orphan reaper: terminal RoomRegistry claim drain retained exact fences for node-expiry recovery");
                }
                Ok(outcome) => {
                    debug!(
                        released = outcome.released,
                        preserved_live = outcome.preserved_live,
                        "orphan reaper: terminal RoomRegistry claim drain completed"
                    );
                }
                Err(error) => {
                    error!(%error, "orphan reaper: terminal RoomRegistry claim drain failed; node-expiry recovery remains authoritative");
                }
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OrphanSweepOutcome {
    Completed,
    Failed,
    Cancelled,
    TimedOut,
}

#[cfg(feature = "clustering")]
fn orphan_sweep_heartbeat_outcome(
    outcome: OrphanSweepOutcome,
    workers_healthy: bool,
) -> SweepOutcome {
    if outcome == OrphanSweepOutcome::Completed && workers_healthy {
        SweepOutcome::Completed
    } else {
        SweepOutcome::Failed
    }
}

/// Bound and cancellation-arm the complete outer sweep, not only its worker
/// jobs. A timeout can occur after a fenced steal but before the typed actor
/// handoff returns; in that uncertain state the only safe generic recovery is
/// to stop this node's lease lifecycle and let another incarnation reclaim it.
#[cfg(feature = "clustering")]
async fn supervise_orphan_reaper_sweep<F>(
    cancel: &tokio_util::sync::CancellationToken,
    fatal_fence: &tokio_util::sync::CancellationToken,
    node_lifecycle: &crate::clustering::NodeLifecycle,
    timeout: Duration,
    sweep: F,
) -> OrphanSweepOutcome
where
    F: std::future::Future<Output = bool>,
{
    tokio::select! {
        biased;
        _ = cancel.cancelled() => OrphanSweepOutcome::Cancelled,
        _ = fatal_fence.cancelled() => OrphanSweepOutcome::Cancelled,
        result = tokio::time::timeout(timeout, sweep) => match result {
            Ok(true) => OrphanSweepOutcome::Completed,
            // The sweep observed the cancellation token itself before the
            // biased select arms fired — that's a shutdown, not a failure.
            Ok(false) if cancel.is_cancelled() || fatal_fence.is_cancelled() => {
                OrphanSweepOutcome::Cancelled
            }
            Ok(false) => OrphanSweepOutcome::Failed,
            Err(_) => {
                node_lifecycle.begin_fenced_recovery();
                fatal_fence.cancel();
                crate::clustering::metrics::record_orphan_worker_failure("sweep", "timeout");
                OrphanSweepOutcome::TimedOut
            }
        }
    }
}

#[cfg(all(test, feature = "clustering"))]
mod orphan_sweep_supervision_tests {
    use super::*;

    #[test]
    fn heartbeat_fails_for_aborted_or_unhealthy_sweeps() {
        assert_eq!(
            orphan_sweep_heartbeat_outcome(OrphanSweepOutcome::Failed, true),
            SweepOutcome::Failed
        );
        assert_eq!(
            orphan_sweep_heartbeat_outcome(OrphanSweepOutcome::Completed, true),
            SweepOutcome::Completed
        );
        assert_eq!(
            orphan_sweep_heartbeat_outcome(OrphanSweepOutcome::Completed, false),
            SweepOutcome::Failed
        );
        assert_eq!(
            orphan_sweep_heartbeat_outcome(OrphanSweepOutcome::Cancelled, true),
            SweepOutcome::Failed
        );
        assert_eq!(
            orphan_sweep_heartbeat_outcome(OrphanSweepOutcome::TimedOut, true),
            SweepOutcome::Failed
        );
    }

    #[tokio::test]
    async fn timed_out_outer_sweep_self_fences_uncertain_claim_handoffs() {
        let cancel = tokio_util::sync::CancellationToken::new();
        let node_lifecycle = crate::clustering::NodeLifecycle::new();
        let fatal_fence = node_lifecycle.fatal_fence_token();
        let admitted = node_lifecycle
            .admit()
            .expect("serving permit before ambiguity");

        let outcome = supervise_orphan_reaper_sweep(
            &cancel,
            &fatal_fence,
            &node_lifecycle,
            Duration::from_millis(1),
            std::future::pending(),
        )
        .await;

        assert_eq!(outcome, OrphanSweepOutcome::TimedOut);
        assert!(fatal_fence.is_cancelled());
        assert_eq!(
            node_lifecycle.admission(),
            crate::clustering::NodeAdmission::FencedRecovering
        );
        assert!(node_lifecycle.admit().is_err());
        assert!(admitted.revalidate().is_err());
    }

    #[tokio::test]
    async fn ordinary_sweep_cancellation_does_not_invent_a_new_self_fence() {
        let cancel = tokio_util::sync::CancellationToken::new();
        let node_lifecycle = crate::clustering::NodeLifecycle::new();
        let fatal_fence = node_lifecycle.fatal_fence_token();
        cancel.cancel();

        let outcome = supervise_orphan_reaper_sweep(
            &cancel,
            &fatal_fence,
            &node_lifecycle,
            Duration::from_secs(1),
            std::future::pending(),
        )
        .await;

        assert_eq!(outcome, OrphanSweepOutcome::Cancelled);
        assert!(!fatal_fence.is_cancelled());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_unpolled_sweep_does_not_export_a_phantom_span() {
        let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
        let worker_cancel = tokio_util::sync::CancellationToken::new();
        let supervisor = OrphanReaperSupervisor::new(
            state.deps.protocol.sm_session_registry.clone(),
            worker_cancel.clone(),
        );
        let cancel = tokio_util::sync::CancellationToken::new();
        let node_lifecycle = crate::clustering::NodeLifecycle::new();
        let fatal_fence = node_lifecycle.fatal_fence_token();
        cancel.cancel();
        let spans = waddle_xmpp::telemetry::test_support::acquire_spans();

        let outcome = supervise_orphan_reaper_sweep(
            &cancel,
            &fatal_fence,
            &node_lifecycle,
            Duration::from_secs(1),
            run_orphan_reaper_sweep_with_workers(&state, &supervisor.workers),
        )
        .await;

        assert_eq!(outcome, OrphanSweepOutcome::Cancelled);
        assert_eq!(
            spans.recorded_field("janitor.sweep", "janitor"),
            None,
            "a sweep that loses the biased cancellation race must never create a span"
        );
        assert!(
            spans
                .exported()
                .iter()
                .all(|span| span.name != "janitor.sweep"),
            "a cancelled-before-poll sweep must not export a phantom root"
        );

        worker_cancel.cancel();
        supervisor.shutdown().await;
    }

    /// Documents WHY the worker loops wrap each attempt in an `async`
    /// block inside their `select!` arms: `select!` evaluates branch
    /// expressions eagerly, and only the block's lazy body keeps a
    /// cancel-beaten attempt from exporting a phantom root. This test
    /// pins that language mechanism on a hand-built future — it does NOT
    /// guard the three worker select sites themselves (no deterministic
    /// seam exists: a pre-cancelled token exits the loop before the
    /// attempt select is ever reached). The load-bearing guard is the
    /// `async` block + comment at each site; do not "simplify" those
    /// blocks away.
    #[tokio::test(flavor = "current_thread")]
    async fn dropped_unpolled_attempt_exports_no_phantom_span() {
        use tracing::Instrument;
        let spans = waddle_xmpp::telemetry::test_support::acquire_spans();

        let sweep_context = opentelemetry::trace::SpanContext::empty_context();
        let attempt = async {
            std::future::ready(())
                .instrument(orphan_work_span("sm_hydration", &sweep_context))
                .await
        };
        drop(attempt);

        assert_eq!(
            spans.recorded_field("janitor.orphan_work", "lane"),
            None,
            "an attempt future dropped before its first poll must never create a span"
        );
        assert!(
            spans
                .exported()
                .iter()
                .all(|span| span.name != "janitor.orphan_work"),
            "a dropped-unpolled attempt must not export a phantom root"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn executed_orphan_sweep_records_its_lazy_span() {
        let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
        let worker_cancel = tokio_util::sync::CancellationToken::new();
        let supervisor = OrphanReaperSupervisor::new(
            state.deps.protocol.sm_session_registry.clone(),
            worker_cancel.clone(),
        );
        let spans = waddle_xmpp::telemetry::test_support::acquire_spans();

        assert!(run_orphan_reaper_sweep_with_workers(&state, &supervisor.workers).await);
        assert_eq!(
            spans.recorded_field("janitor.sweep", "janitor").as_deref(),
            Some("orphan_reaper"),
        );

        worker_cancel.cancel();
        supervisor.shutdown().await;
    }

    #[tokio::test]
    async fn fatal_fence_cancels_a_sweep_before_it_can_make_progress() {
        let cancel = tokio_util::sync::CancellationToken::new();
        let node_lifecycle = crate::clustering::NodeLifecycle::new();
        let fatal_fence = node_lifecycle.fatal_fence_token();
        fatal_fence.cancel();
        let entered = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let entered_by_sweep = Arc::clone(&entered);

        let outcome = supervise_orphan_reaper_sweep(
            &cancel,
            &fatal_fence,
            &node_lifecycle,
            Duration::from_secs(1),
            async move {
                entered_by_sweep.store(true, std::sync::atomic::Ordering::SeqCst);
                true
            },
        )
        .await;

        assert_eq!(outcome, OrphanSweepOutcome::Cancelled);
        assert!(!entered.load(std::sync::atomic::Ordering::SeqCst));
    }
}

#[cfg(all(test, feature = "clustering"))]
async fn clustered_test_state_for_critical_registry_watch() -> Arc<WebSocketState> {
    let handles = crate::clustering::ClusteringHandles {
        stop_token: Some(tokio_util::sync::CancellationToken::new()),
        ..Default::default()
    };
    crate::server::routes::websocket::tests::create_test_websocket_state_with_clustering(
        handles,
        Arc::new(waddle_xmpp::stream_management::InMemorySmSessionRegistry::new()),
    )
    .await
}

#[cfg(test)]
async fn assert_room_registry_death_is_critical(state: Arc<WebSocketState>) {
    let process_stop = state.deps.shutdown.stop_token();
    spawn_critical_registry_supervisor(&state).await;
    state.deps.protocol.room_registry.kill();
    state.deps.protocol.room_registry.wait_for_shutdown().await;
    tokio::task::yield_now().await;

    assert_eq!(
        state.deps.app_state.node_lifecycle.critical_failure(),
        Some(crate::clustering::CriticalNodeFailure::RoomRegistryTerminated)
    );
    assert!(process_stop.is_cancelled());
    assert!(state.deps.app_state.node_lifecycle.admit().is_err());
}

#[cfg(test)]
async fn assert_user_registry_death_is_critical(state: Arc<WebSocketState>) {
    let process_stop = state.deps.shutdown.stop_token();
    spawn_critical_registry_supervisor(&state).await;
    state.deps.protocol.user_registry.kill();
    state.deps.protocol.user_registry.wait_for_shutdown().await;
    tokio::task::yield_now().await;

    assert_eq!(
        state.deps.app_state.node_lifecycle.critical_failure(),
        Some(crate::clustering::CriticalNodeFailure::UserRegistryTerminated)
    );
    assert!(process_stop.is_cancelled());
    assert!(state.deps.app_state.node_lifecycle.admit().is_err());
}

#[cfg(test)]
async fn assert_ordinary_shutdown_is_not_critical(state: Arc<WebSocketState>) {
    let process_stop = state.deps.shutdown.stop_token();
    spawn_critical_registry_supervisor(&state).await;
    process_stop.cancel();
    tokio::task::yield_now().await;

    assert_eq!(state.deps.app_state.node_lifecycle.critical_failure(), None);
}

#[cfg(test)]
#[tokio::test]
async fn single_node_room_registry_death_is_critical() {
    assert_room_registry_death_is_critical(
        crate::server::routes::websocket::tests::create_test_websocket_state().await,
    )
    .await;
}

#[cfg(test)]
#[tokio::test]
async fn single_node_user_registry_death_is_critical() {
    assert_user_registry_death_is_critical(
        crate::server::routes::websocket::tests::create_test_websocket_state().await,
    )
    .await;
}

#[cfg(test)]
#[tokio::test]
async fn single_node_ordinary_shutdown_is_not_critical() {
    assert_ordinary_shutdown_is_not_critical(
        crate::server::routes::websocket::tests::create_test_websocket_state().await,
    )
    .await;
}

#[cfg(test)]
#[tokio::test]
async fn already_dead_room_registry_blocks_startup_promotion() {
    let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    let lifecycle = crate::clustering::NodeLifecycle::starting();
    let stop = tokio_util::sync::CancellationToken::new();
    state.deps.protocol.room_registry.kill();
    state.deps.protocol.room_registry.wait_for_shutdown().await;

    let outcome = spawn_room_registry_lifetime_watch(
        state.deps.protocol.room_registry.clone(),
        lifecycle.clone(),
        stop.clone(),
    )
    .await
    .expect("room watcher arm outcome");

    assert_eq!(
        outcome,
        CriticalRegistryArm::Failed(crate::clustering::CriticalNodeFailure::RoomRegistryTerminated)
    );
    assert!(stop.is_cancelled());
    assert!(matches!(
        lifecycle.finish_startup(),
        crate::clustering::StartupServingTransition::Blocked(
            crate::clustering::NodeAdmission::Failed(
                crate::clustering::CriticalNodeFailure::RoomRegistryTerminated
            )
        )
    ));
}

#[cfg(test)]
#[tokio::test]
async fn already_dead_user_registry_blocks_startup_promotion() {
    let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    let lifecycle = crate::clustering::NodeLifecycle::starting();
    let stop = tokio_util::sync::CancellationToken::new();
    state.deps.protocol.user_registry.kill();
    state.deps.protocol.user_registry.wait_for_shutdown().await;

    let outcome = spawn_user_registry_lifetime_watch(
        state.deps.protocol.user_registry.clone(),
        lifecycle.clone(),
        stop.clone(),
    )
    .await
    .expect("user watcher arm outcome");

    assert_eq!(
        outcome,
        CriticalRegistryArm::Failed(crate::clustering::CriticalNodeFailure::UserRegistryTerminated)
    );
    assert!(stop.is_cancelled());
    assert!(matches!(
        lifecycle.finish_startup(),
        crate::clustering::StartupServingTransition::Blocked(
            crate::clustering::NodeAdmission::Failed(
                crate::clustering::CriticalNodeFailure::UserRegistryTerminated
            )
        )
    ));
}

#[cfg(all(test, feature = "clustering"))]
#[tokio::test]
async fn clustered_room_registry_death_is_critical() {
    assert_room_registry_death_is_critical(
        clustered_test_state_for_critical_registry_watch().await,
    )
    .await;
}

#[cfg(all(test, feature = "clustering"))]
#[tokio::test]
async fn clustered_user_registry_death_is_critical() {
    assert_user_registry_death_is_critical(
        clustered_test_state_for_critical_registry_watch().await,
    )
    .await;
}

#[cfg(all(test, feature = "clustering"))]
#[tokio::test]
async fn clustered_ordinary_shutdown_is_not_critical() {
    assert_ordinary_shutdown_is_not_critical(
        clustered_test_state_for_critical_registry_watch().await,
    )
    .await;
}

pub(crate) async fn spawn_critical_registry_supervisor(websocket_state: &Arc<WebSocketState>) {
    let node_lifecycle = websocket_state.deps.app_state.node_lifecycle.clone();
    let process_stop = websocket_state.deps.shutdown.stop_token();
    let room_registry = websocket_state.deps.protocol.room_registry.clone();
    let user_registry = websocket_state.deps.protocol.user_registry.clone();
    let room_registry_liveness = room_registry.clone();
    let user_registry_liveness = user_registry.clone();
    let room_armed = spawn_room_registry_lifetime_watch(
        room_registry,
        node_lifecycle.clone(),
        process_stop.clone(),
    );
    let user_armed =
        spawn_user_registry_lifetime_watch(user_registry, node_lifecycle, process_stop);
    let (room_arm, user_arm) = tokio::join!(room_armed, user_armed);
    for outcome in [
        room_arm.unwrap_or(CriticalRegistryArm::Failed(
            crate::clustering::CriticalNodeFailure::RoomRegistryTerminated,
        )),
        user_arm.unwrap_or(CriticalRegistryArm::Failed(
            crate::clustering::CriticalNodeFailure::UserRegistryTerminated,
        )),
    ] {
        if let CriticalRegistryArm::Failed(failure) = outcome {
            websocket_state.deps.app_state.node_lifecycle.fail(failure);
            websocket_state.deps.shutdown.stop_token().cancel();
        }
    }
    // Close the arm-to-promotion gap synchronously. A registry that died
    // after its shutdown future was first polled but before this barrier is
    // failed before `finish_startup` can linearize Starting -> Serving.
    if !room_registry_liveness.is_alive() {
        websocket_state
            .deps
            .app_state
            .node_lifecycle
            .fail(crate::clustering::CriticalNodeFailure::RoomRegistryTerminated);
        websocket_state.deps.shutdown.stop_token().cancel();
    }
    if !user_registry_liveness.is_alive() {
        websocket_state
            .deps
            .app_state
            .node_lifecycle
            .fail(crate::clustering::CriticalNodeFailure::UserRegistryTerminated);
        websocket_state.deps.shutdown.stop_token().cancel();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CriticalRegistryArm {
    Armed,
    Failed(crate::clustering::CriticalNodeFailure),
}

fn spawn_room_registry_lifetime_watch(
    room_registry: kameo::actor::ActorRef<waddle_xmpp::muc::room_registry_actor::RoomRegistryActor>,
    node_lifecycle: crate::clustering::NodeLifecycle,
    process_stop: tokio_util::sync::CancellationToken,
) -> tokio::sync::oneshot::Receiver<CriticalRegistryArm> {
    let (armed_tx, armed_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let shutdown = room_registry.wait_for_shutdown();
        tokio::pin!(shutdown);
        if futures::poll!(shutdown.as_mut()).is_ready() {
            node_lifecycle.fail(crate::clustering::CriticalNodeFailure::RoomRegistryTerminated);
            process_stop.cancel();
            let _ = armed_tx.send(CriticalRegistryArm::Failed(
                crate::clustering::CriticalNodeFailure::RoomRegistryTerminated,
            ));
            return;
        }
        let _ = armed_tx.send(CriticalRegistryArm::Armed);
        tokio::select! {
            biased;
            // Ordered process shutdown is the sole non-fatal exit. Keeping it
            // first means a registry that terminates as part of that shutdown
            // cannot race into a terminal failure latch.
            _ = process_stop.cancelled() => {}
            _ = shutdown => {
                // Room ownership retry state is actor-local. Losing the
                // registry while this node's lease remains fresh can strand
                // every retained exact fence, even while idle, so registry
                // lifetime is clustering-critical.
                node_lifecycle.fail(crate::clustering::CriticalNodeFailure::RoomRegistryTerminated);
                process_stop.cancel();
            }
        }
    });
    armed_rx
}

fn spawn_user_registry_lifetime_watch(
    user_registry: kameo::actor::ActorRef<waddle_xmpp::registry::UserRegistryActor>,
    node_lifecycle: crate::clustering::NodeLifecycle,
    process_stop: tokio_util::sync::CancellationToken,
) -> tokio::sync::oneshot::Receiver<CriticalRegistryArm> {
    let (armed_tx, armed_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let shutdown = user_registry.wait_for_shutdown();
        tokio::pin!(shutdown);
        if futures::poll!(shutdown.as_mut()).is_ready() {
            node_lifecycle.fail(crate::clustering::CriticalNodeFailure::UserRegistryTerminated);
            process_stop.cancel();
            let _ = armed_tx.send(CriticalRegistryArm::Failed(
                crate::clustering::CriticalNodeFailure::UserRegistryTerminated,
            ));
            return;
        }
        let _ = armed_tx.send(CriticalRegistryArm::Armed);
        tokio::select! {
            biased;
            _ = process_stop.cancelled() => {}
            _ = shutdown => {
                // UserActor claim/retry state is registry-local. Continuing
                // to advertise this node after it dies would accept sockets
                // whose logical owners cannot be activated safely.
                node_lifecycle.fail(crate::clustering::CriticalNodeFailure::UserRegistryTerminated);
                process_stop.cancel();
            }
        }
    });
    armed_rx
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
#[cfg(all(test, feature = "clustering"))]
fn orphaned_sm_candidates_or_empty(
    result: Result<
        Vec<crate::clustering::claims::OrphanedSmSessionClaim>,
        waddle_xmpp::ownership::ClaimError,
    >,
) -> Vec<crate::clustering::claims::OrphanedSmSessionClaim> {
    result
        .inspect_err(|error| {
            debug!(%error, "orphan reaper: SM-session candidate scan failed; continuing other orphan lanes")
        })
        .unwrap_or_default()
}

#[cfg(feature = "clustering")]
#[derive(Clone)]
struct SmHydrationWork {
    entity: waddle_xmpp::ownership::Entity,
    fence: waddle_xmpp::stream_management::persistence::SmClaimFence,
    reservation: waddle_xmpp::stream_management::ReclaimedClaimReservation,
    /// The enqueuing sweep's span context, carried for a span *link* on
    /// each attempt — never a live `Span`, which retry queues and pending
    /// inventories would hold open indefinitely.
    sweep_context: opentelemetry::trace::SpanContext,
}

#[cfg(feature = "clustering")]
#[derive(Clone)]
enum PendingSmHydration {
    Reserved {
        entity: waddle_xmpp::ownership::Entity,
        attempted_owner: waddle_xmpp::ownership::NodeIdentity,
        reservation: waddle_xmpp::stream_management::ReclaimedClaimReservation,
    },
    Won(SmHydrationWork),
}

#[cfg(feature = "clustering")]
#[derive(Clone)]
struct ExactReleaseWork {
    claim_store: Arc<dyn waddle_xmpp::ownership::ClaimStore>,
    entity: waddle_xmpp::ownership::Entity,
    owner: waddle_xmpp::ownership::NodeIdentity,
    epoch: waddle_xmpp::ownership::ClaimEpoch,
    /// See [`SmHydrationWork::sweep_context`].
    sweep_context: opentelemetry::trace::SpanContext,
}

#[cfg(feature = "clustering")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ExactReleaseKey {
    entity: waddle_xmpp::ownership::Entity,
    owner: waddle_xmpp::ownership::NodeIdentity,
    epoch: waddle_xmpp::ownership::ClaimEpoch,
}

#[cfg(feature = "clustering")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkEnqueueOutcome {
    Enqueued,
    AlreadyTracked,
    /// Receiver exited, but the exact work remains in the supervisor's
    /// restart inventory and will be requeued by the replacement worker.
    RetainedForRestart,
    /// The bounded channel was full. The exact work remains in the pending
    /// inventory and a bounded background sender is actively redriving it.
    RetainedForRedrive,
    Rejected,
}

#[cfg(feature = "clustering")]
impl WorkEnqueueOutcome {
    const fn is_accepted(self) -> bool {
        matches!(
            self,
            Self::Enqueued
                | Self::AlreadyTracked
                | Self::RetainedForRestart
                | Self::RetainedForRedrive
        )
    }
}

#[cfg(feature = "clustering")]
#[derive(Clone)]
struct OrphanReaperWorkers {
    hydration_tx: tokio::sync::mpsc::Sender<SmHydrationWork>,
    release_tx: tokio::sync::mpsc::Sender<ExactReleaseWork>,
    hydration_pending: Arc<std::sync::Mutex<std::collections::HashMap<String, PendingSmHydration>>>,
    release_pending:
        Arc<std::sync::Mutex<std::collections::HashMap<ExactReleaseKey, ExactReleaseWork>>>,
    /// Exact RoomActor epochs observed as won but not yet accepted into the
    /// registry actor's pending inventory. This synchronous bridge closes the
    /// cancellation gap between the steal CAS returning and the first
    /// subsequent mailbox await.
    room_handoff_pending: Arc<
        std::sync::Mutex<
            std::collections::HashMap<
                (jid::BareJid, waddle_xmpp::muc::RoomClaimFenceContext),
                waddle_xmpp::muc::room_registry_actor::PendingReclaimedRoom,
            >,
        >,
    >,
    room_cursor: Arc<std::sync::Mutex<Option<crate::clustering::claims::RoomOrphanScanCursor>>>,
    sm_cursor: Arc<std::sync::Mutex<Option<crate::clustering::claims::SmOrphanScanCursor>>>,
    cancel: tokio_util::sync::CancellationToken,
    fatal_fence: tokio_util::sync::CancellationToken,
    node_lifecycle: crate::clustering::NodeLifecycle,
}

#[cfg(feature = "clustering")]
struct OrphanReaperSupervisor {
    registry: Arc<waddle_xmpp::stream_management::InMemorySmSessionRegistry>,
    workers: OrphanReaperWorkers,
    cancel: tokio_util::sync::CancellationToken,
    tasks: Vec<(&'static str, tokio::task::JoinHandle<()>)>,
}

#[cfg(feature = "clustering")]
impl OrphanReaperWorkers {
    fn hydration_key(work: &SmHydrationWork) -> String {
        work.entity.id.clone()
    }

    fn reserve_hydration(
        &self,
        entity: &waddle_xmpp::ownership::Entity,
        attempted_owner: &waddle_xmpp::ownership::NodeIdentity,
        reservation: waddle_xmpp::stream_management::ReclaimedClaimReservation,
    ) -> bool {
        let mut pending = self
            .hydration_pending
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if pending.contains_key(&entity.id) {
            return false;
        }
        if pending.len() >= ORPHAN_WORK_QUEUE_CAPACITY {
            crate::clustering::metrics::record_orphan_work_queue_backpressure("sm_hydration");
            return false;
        }
        pending.insert(
            entity.id.clone(),
            PendingSmHydration::Reserved {
                entity: entity.clone(),
                attempted_owner: attempted_owner.clone(),
                reservation,
            },
        );
        crate::clustering::metrics::record_orphan_work_queue_depth("sm_hydration", pending.len());
        true
    }

    fn cancel_hydration_reservation(&self, entity: &waddle_xmpp::ownership::Entity) {
        let mut pending = self
            .hydration_pending
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        pending.remove(&entity.id);
        crate::clustering::metrics::record_orphan_work_queue_depth("sm_hydration", pending.len());
    }

    fn enqueue_reserved_hydration(
        &self,
        entity: waddle_xmpp::ownership::Entity,
        fence: waddle_xmpp::stream_management::persistence::SmClaimFence,
        reservation: waddle_xmpp::stream_management::ReclaimedClaimReservation,
    ) -> WorkEnqueueOutcome {
        let work = SmHydrationWork {
            entity,
            fence,
            reservation,
            sweep_context: current_sweep_context(),
        };
        let key = Self::hydration_key(&work);
        self.hydration_pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.clone(), PendingSmHydration::Won(work.clone()));
        match self.hydration_tx.try_send(work) {
            Ok(()) => WorkEnqueueOutcome::Enqueued,
            Err(tokio::sync::mpsc::error::TrySendError::Full(work)) => {
                crate::clustering::metrics::record_orphan_work_queue_backpressure("sm_hydration");
                let tx = self.hydration_tx.clone();
                tokio::spawn(async move {
                    let _ = tx.send(work).await;
                });
                WorkEnqueueOutcome::RetainedForRedrive
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                // The supervisor snapshots this map when it replaces a dead
                // worker. Retain the exact work so restart can requeue it.
                crate::clustering::metrics::record_orphan_work_queue_backpressure("sm_hydration");
                WorkEnqueueOutcome::RetainedForRestart
            }
        }
    }

    fn release_key(work: &ExactReleaseWork) -> ExactReleaseKey {
        ExactReleaseKey {
            entity: work.entity.clone(),
            owner: work.owner.clone(),
            epoch: work.epoch,
        }
    }

    fn has_release_capacity(&self) -> bool {
        self.release_pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
            < ORPHAN_WORK_QUEUE_CAPACITY
    }

    fn enqueue_hydration(
        &self,
        entity: waddle_xmpp::ownership::Entity,
        fence: waddle_xmpp::stream_management::persistence::SmClaimFence,
        reservation: waddle_xmpp::stream_management::ReclaimedClaimReservation,
    ) -> WorkEnqueueOutcome {
        let work = SmHydrationWork {
            entity,
            fence,
            reservation,
            sweep_context: current_sweep_context(),
        };
        if self
            .hydration_pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&Self::hydration_key(&work))
        {
            return WorkEnqueueOutcome::AlreadyTracked;
        }
        if !self.reserve_hydration(&work.entity, work.fence.owner(), work.reservation) {
            return WorkEnqueueOutcome::Rejected;
        }
        self.enqueue_reserved_hydration(work.entity, work.fence, work.reservation)
    }

    fn enqueue_release(&self, work: ExactReleaseWork) -> WorkEnqueueOutcome {
        let key = Self::release_key(&work);
        let mut pending = self
            .release_pending
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if pending.contains_key(&key) {
            return WorkEnqueueOutcome::AlreadyTracked;
        }
        if pending.len() >= ORPHAN_WORK_QUEUE_CAPACITY {
            crate::clustering::metrics::record_orphan_work_queue_backpressure("room_release");
            return WorkEnqueueOutcome::Rejected;
        }
        pending.insert(key.clone(), work.clone());
        match self.release_tx.try_send(work) {
            Ok(()) => {
                crate::clustering::metrics::record_orphan_work_queue_depth(
                    "room_release",
                    pending.len(),
                );
                WorkEnqueueOutcome::Enqueued
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(work)) => {
                crate::clustering::metrics::record_orphan_work_queue_backpressure("room_release");
                let tx = self.release_tx.clone();
                tokio::spawn(async move {
                    let _ = tx.send(work).await;
                });
                WorkEnqueueOutcome::RetainedForRedrive
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                // A closed receiver means the supervisor must restart the
                // worker. Keep this exact fence in its restart inventory.
                crate::clustering::metrics::record_orphan_work_queue_backpressure("room_release");
                WorkEnqueueOutcome::RetainedForRestart
            }
        }
    }

    fn pending_hydrations(&self) -> Vec<SmHydrationWork> {
        self.hydration_pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter_map(|pending| match pending {
                PendingSmHydration::Won(work) => Some(work.clone()),
                PendingSmHydration::Reserved { .. } => None,
            })
            .collect()
    }

    fn pending_unwon_hydration_reservations(
        &self,
    ) -> Vec<(
        waddle_xmpp::ownership::Entity,
        waddle_xmpp::ownership::NodeIdentity,
        waddle_xmpp::stream_management::ReclaimedClaimReservation,
    )> {
        self.hydration_pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter_map(|pending| match pending {
                PendingSmHydration::Reserved {
                    entity,
                    attempted_owner,
                    reservation,
                } => Some((entity.clone(), attempted_owner.clone(), *reservation)),
                PendingSmHydration::Won(_) => None,
            })
            .collect()
    }

    fn pending_releases(&self) -> Vec<ExactReleaseWork> {
        self.release_pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect()
    }

    /// Restore exact known-won work only after both worker tasks have
    /// stopped. The stopped supervisor becomes a terminal carrier when an
    /// unrelated uncertain reservation cannot be retired; no receiver may
    /// clear these maps again before `shutdown_terminal` snapshots them.
    fn restore_captured_terminal_work(
        &self,
        hydrations: Vec<SmHydrationWork>,
        releases: Vec<ExactReleaseWork>,
    ) {
        let mut hydration_pending = self
            .hydration_pending
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for work in hydrations {
            hydration_pending.insert(Self::hydration_key(&work), PendingSmHydration::Won(work));
        }
        debug_assert!(hydration_pending.len() <= ORPHAN_WORK_QUEUE_CAPACITY);
        crate::clustering::metrics::record_orphan_work_queue_depth(
            "sm_hydration",
            hydration_pending.len(),
        );
        drop(hydration_pending);

        let mut release_pending = self
            .release_pending
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for work in releases {
            release_pending.insert(Self::release_key(&work), work);
        }
        debug_assert!(release_pending.len() <= ORPHAN_WORK_QUEUE_CAPACITY);
        crate::clustering::metrics::record_orphan_work_queue_depth(
            "room_release",
            release_pending.len(),
        );
    }

    fn remember_room_handoff(
        &self,
        pending: waddle_xmpp::muc::room_registry_actor::PendingReclaimedRoom,
    ) {
        let key = (pending.room_jid.clone(), pending.claim_fence.clone());
        let mut handoffs = self
            .room_handoff_pending
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if handoffs.contains_key(&key) {
            return;
        }
        // Every entry consumes a RoomRegistry reservation admitted under the
        // same bound before the steal. Keep the post-CAS transition
        // infallible: once ownership is observed as won, dropping the exact
        // fence would be worse than violating a defensive debug assertion.
        debug_assert!(handoffs.len() < ORPHAN_WORK_QUEUE_CAPACITY);
        handoffs.insert(key, pending);
        crate::clustering::metrics::record_orphan_work_queue_depth("room_handoff", handoffs.len());
    }

    fn forget_room_handoff(
        &self,
        room_jid: &jid::BareJid,
        claim_fence: &waddle_xmpp::muc::RoomClaimFenceContext,
    ) {
        let mut handoffs = self
            .room_handoff_pending
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        handoffs.remove(&(room_jid.clone(), claim_fence.clone()));
        crate::clustering::metrics::record_orphan_work_queue_depth("room_handoff", handoffs.len());
    }

    fn pending_room_handoffs(
        &self,
    ) -> Vec<waddle_xmpp::muc::room_registry_actor::PendingReclaimedRoom> {
        self.room_handoff_pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect()
    }

    fn sm_cursor(&self) -> Option<crate::clustering::claims::SmOrphanScanCursor> {
        self.sm_cursor
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn set_sm_cursor(&self, cursor: Option<crate::clustering::claims::SmOrphanScanCursor>) {
        *self.sm_cursor.lock().unwrap_or_else(|e| e.into_inner()) = cursor;
    }

    fn room_cursor(&self) -> Option<crate::clustering::claims::RoomOrphanScanCursor> {
        self.room_cursor
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn set_room_cursor(&self, cursor: Option<crate::clustering::claims::RoomOrphanScanCursor>) {
        *self.room_cursor.lock().unwrap_or_else(|e| e.into_inner()) = cursor;
    }
}

#[cfg(feature = "clustering")]
impl OrphanReaperSupervisor {
    #[cfg(test)]
    fn new(
        registry: Arc<waddle_xmpp::stream_management::InMemorySmSessionRegistry>,
        parent_cancel: tokio_util::sync::CancellationToken,
    ) -> Self {
        let node_lifecycle = crate::clustering::NodeLifecycle::new();
        let fatal_fence = node_lifecycle.fatal_fence_token();
        Self::new_with_fatal_fence(registry, parent_cancel, fatal_fence, node_lifecycle)
    }

    fn new_with_fatal_fence(
        registry: Arc<waddle_xmpp::stream_management::InMemorySmSessionRegistry>,
        parent_cancel: tokio_util::sync::CancellationToken,
        fatal_fence: tokio_util::sync::CancellationToken,
        node_lifecycle: crate::clustering::NodeLifecycle,
    ) -> Self {
        let cancel = parent_cancel.child_token();
        let (hydration_tx, mut hydration_rx) =
            tokio::sync::mpsc::channel(ORPHAN_WORK_QUEUE_CAPACITY);
        let (release_tx, mut release_rx) = tokio::sync::mpsc::channel(ORPHAN_WORK_QUEUE_CAPACITY);
        let hydration_pending = Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
        let release_pending = Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
        let room_handoff_pending =
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
        let workers = OrphanReaperWorkers {
            hydration_tx,
            release_tx,
            hydration_pending: hydration_pending.clone(),
            release_pending: release_pending.clone(),
            room_handoff_pending,
            room_cursor: Arc::new(std::sync::Mutex::new(None)),
            sm_cursor: Arc::new(std::sync::Mutex::new(None)),
            cancel: cancel.clone(),
            fatal_fence,
            node_lifecycle,
        };

        let hydration_cancel = cancel.clone();
        let hydration_registry = registry.clone();
        let hydration_task = tokio::spawn(async move {
            let mut queue = std::collections::VecDeque::new();
            loop {
                if queue.is_empty() {
                    tokio::select! {
                        _ = hydration_cancel.cancelled() => break,
                        work = hydration_rx.recv() => match work { Some(work) => queue.push_back(work), None => break },
                    }
                }
                while let Ok(work) = hydration_rx.try_recv() {
                    queue.push_back(work);
                }
                let Some(work) = queue.pop_front() else {
                    continue;
                };
                // The async block defers span creation to first poll:
                // select! evaluates every branch expression eagerly, and a
                // cancel-arm win would otherwise export a phantom attempt
                // root for work that never ran.
                let result = tokio::select! {
                    _ = hydration_cancel.cancelled() => break,
                    result = async {
                        tokio::time::timeout(
                            ORPHAN_WORK_ATTEMPT_TIMEOUT,
                            hydration_registry.hydrate_reclaimed_typed(
                                &work.entity,
                                &work.fence,
                                work.reservation,
                            ),
                        )
                        .instrument(orphan_work_span("sm_hydration", &work.sweep_context))
                        .await
                    } => result,
                };
                match result {
                    Ok(Ok(
                        waddle_xmpp::stream_management::ReclaimedHydrationOutcome::Hydrated
                        | waddle_xmpp::stream_management::ReclaimedHydrationOutcome::AlreadyPresent
                        | waddle_xmpp::stream_management::ReclaimedHydrationOutcome::LostClaim,
                    )) => {
                        let key = OrphanReaperWorkers::hydration_key(&work);
                        let mut pending =
                            hydration_pending.lock().unwrap_or_else(|e| e.into_inner());
                        pending.remove(&key);
                        crate::clustering::metrics::record_orphan_work_queue_depth(
                            "sm_hydration",
                            pending.len(),
                        );
                        info!("orphan reaper: targeted hydration work complete");
                    }
                    Ok(Ok(
                        waddle_xmpp::stream_management::ReclaimedHydrationOutcome::MissingDurable
                        | waddle_xmpp::stream_management::ReclaimedHydrationOutcome::PoisonReleased,
                    )) => {
                        let cleanup = tokio::select! {
                            _ = hydration_cancel.cancelled() => break,
                            result = async {
                                tokio::time::timeout(
                                    ORPHAN_WORK_ATTEMPT_TIMEOUT,
                                    hydration_registry.release_reclaimed_claim(
                                        &work.entity,
                                        &work.fence,
                                        work.reservation,
                                    ),
                                )
                                .instrument(orphan_work_span("sm_hydration", &work.sweep_context))
                                .await
                            } => result,
                        };
                        match cleanup {
                            Ok(Ok(_)) => {
                                let key = OrphanReaperWorkers::hydration_key(&work);
                                let mut pending =
                                    hydration_pending.lock().unwrap_or_else(|e| e.into_inner());
                                pending.remove(&key);
                                crate::clustering::metrics::record_orphan_work_queue_depth(
                                    "sm_hydration",
                                    pending.len(),
                                );
                            }
                            Ok(Err(error)) => {
                                debug!(%error, "orphan reaper: missing/poison SM exact release failed; retrying");
                                queue.push_back(work);
                            }
                            Err(_) => queue.push_back(work),
                        }
                    }
                    Ok(Ok(
                        waddle_xmpp::stream_management::ReclaimedHydrationOutcome::TransientFailure,
                    )) => {
                        queue.push_back(work);
                    }
                    Ok(Ok(
                        waddle_xmpp::stream_management::ReclaimedHydrationOutcome::StaleIdentity,
                    )) => {
                        let cleanup = tokio::time::timeout(
                            ORPHAN_WORK_ATTEMPT_TIMEOUT,
                            hydration_registry.release_reclaimed_claim(
                                &work.entity,
                                &work.fence,
                                work.reservation,
                            ),
                        )
                        .instrument(orphan_work_span("sm_hydration", &work.sweep_context))
                        .await;
                        match cleanup {
                            Ok(Ok(_)) => {
                                let key = OrphanReaperWorkers::hydration_key(&work);
                                let mut pending =
                                    hydration_pending.lock().unwrap_or_else(|e| e.into_inner());
                                pending.remove(&key);
                                crate::clustering::metrics::record_orphan_work_queue_depth(
                                    "sm_hydration",
                                    pending.len(),
                                );
                            }
                            Ok(Err(_)) | Err(_) => queue.push_back(work),
                        }
                    }
                    Ok(Err(error)) => {
                        debug!(%error, "orphan reaper: targeted hydration failed; retaining bounded retry");
                        queue.push_back(work);
                    }
                    Err(_) => {
                        debug!(
                            "orphan reaper: targeted hydration timed out; retaining bounded retry"
                        );
                        queue.push_back(work);
                    }
                }
                if !queue.is_empty() {
                    tokio::select! {
                        _ = hydration_cancel.cancelled() => break,
                        _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                    }
                }
            }
            hydration_pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
            crate::clustering::metrics::record_orphan_work_queue_depth("sm_hydration", 0);
        });

        let release_cancel = cancel.clone();
        let release_task = tokio::spawn(async move {
            let mut queue = std::collections::VecDeque::new();
            loop {
                if queue.is_empty() {
                    tokio::select! {
                        _ = release_cancel.cancelled() => break,
                        work = release_rx.recv() => match work { Some(work) => queue.push_back(work), None => break },
                    }
                }
                while let Ok(work) = release_rx.try_recv() {
                    queue.push_back(work);
                }
                let Some(work) = queue.pop_front() else {
                    continue;
                };
                // async block: see the hydration lane — select! would
                // otherwise create (and export) the span even when the
                // cancel arm wins before this branch is ever polled.
                let result = tokio::select! {
                    _ = release_cancel.cancelled() => break,
                    result = async {
                        tokio::time::timeout(
                            ORPHANED_ROOM_RELEASE_TIMEOUT,
                            work.claim_store.release_exact(&work.entity, &work.owner, work.epoch),
                        )
                        .instrument(orphan_work_span("room_release", &work.sweep_context))
                        .await
                    } => result,
                };
                match result {
                    Ok(Ok(_)) => {
                        let key = OrphanReaperWorkers::release_key(&work);
                        let mut pending = release_pending.lock().unwrap_or_else(|e| e.into_inner());
                        pending.remove(&key);
                        crate::clustering::metrics::record_orphan_work_queue_depth(
                            "room_release",
                            pending.len(),
                        );
                    }
                    Ok(Err(error)) => {
                        debug!(%error, entity_id = %work.entity.id, "orphan reaper: exact cleanup retry failed");
                        queue.push_back(work);
                    }
                    Err(_) => {
                        debug!(entity_id = %work.entity.id, "orphan reaper: exact cleanup retry timed out");
                        queue.push_back(work);
                    }
                }
                if !queue.is_empty() {
                    tokio::select! {
                        _ = release_cancel.cancelled() => break,
                        _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                    }
                }
            }
            release_pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
            crate::clustering::metrics::record_orphan_work_queue_depth("room_release", 0);
        });
        Self {
            registry,
            workers,
            cancel,
            tasks: vec![
                ("sm_hydration", hydration_task),
                ("room_release", release_task),
            ],
        }
    }

    fn is_healthy(&self) -> bool {
        self.tasks.iter().all(|(_, task)| !task.is_finished())
    }

    async fn restarted(
        self,
        registry: Arc<waddle_xmpp::stream_management::InMemorySmSessionRegistry>,
        parent_cancel: tokio_util::sync::CancellationToken,
    ) -> Self {
        let hydration = self.workers.pending_hydrations();
        let unwon_reservations = self.workers.pending_unwon_hydration_reservations();
        let releases = self.workers.pending_releases();
        let room_handoffs = self.workers.pending_room_handoffs();
        let room_cursor = self.workers.room_cursor();
        let sm_cursor = self.workers.sm_cursor();
        let stopped = self.shutdown_for_restart().await;
        let fatal_fence = stopped.workers.fatal_fence.clone();
        let previous_registry = stopped.registry.clone();
        let retirement_results = futures::future::join_all(unwon_reservations.into_iter().map(
            |(entity, attempted_owner, reservation)| {
                let registry = previous_registry.clone();
                async move {
                    let entity_id = entity.id.clone();
                    let result = registry
                        .retire_uncertain_reclaimed_claim(&entity, &attempted_owner, reservation)
                        .await;
                    (entity_id, result)
                }
            },
        ))
        .await;
        let mut retirement_failed = false;
        for (entity_id, result) in retirement_results {
            if let Err(error) = result {
                error!(%error, %entity_id, "orphan reaper: uncertain SM claim could not be retired during worker restart; self-fencing node");
                retirement_failed = true;
            }
        }
        if retirement_failed {
            stopped
                .workers
                .restore_captured_terminal_work(hydration, releases);
            stopped.workers.node_lifecycle.begin_fenced_recovery();
            fatal_fence.cancel();
            return stopped;
        }
        let next = Self::new_with_fatal_fence(
            registry,
            parent_cancel,
            fatal_fence,
            stopped.workers.node_lifecycle.clone(),
        );
        next.workers.set_room_cursor(room_cursor);
        next.workers.set_sm_cursor(sm_cursor);
        for handoff in room_handoffs {
            next.workers.remember_room_handoff(handoff);
        }
        for work in hydration {
            // Re-enqueueing re-captures the (empty) ambient context, so a
            // restarted item loses its sweep link; its attempts still get
            // their own `janitor.orphan_work` roots, so the work stays
            // traced.
            if !next
                .workers
                .enqueue_hydration(work.entity, work.fence, work.reservation)
                .is_accepted()
            {
                error!("orphan reaper: failed to requeue hydration after worker restart");
            }
        }
        for work in releases {
            if !next.workers.enqueue_release(work).is_accepted() {
                error!("orphan reaper: failed to requeue exact release after worker restart");
            }
        }
        next
    }

    async fn shutdown_for_restart(mut self) -> Self {
        self.cancel.cancel();
        for (worker, task) in self.tasks.drain(..) {
            if let Err(error) = task.await {
                let reason = if error.is_panic() {
                    "panic"
                } else {
                    "cancelled"
                };
                crate::clustering::metrics::record_orphan_worker_failure(worker, reason);
                error!(worker, %error, "orphan reaper worker failed");
            }
        }
        self
    }

    async fn shutdown_terminal(
        mut self,
    ) -> Vec<waddle_xmpp::muc::room_registry_actor::PendingReclaimedRoom> {
        let hydrations = self.workers.pending_hydrations();
        let unwon_reservations = self.workers.pending_unwon_hydration_reservations();
        let releases = self.workers.pending_releases();
        let room_handoffs = self.workers.pending_room_handoffs();
        self.cancel.cancel();
        for (worker, task) in self.tasks.drain(..) {
            if let Err(error) = task.await {
                let reason = if error.is_panic() {
                    "panic"
                } else {
                    "cancelled"
                };
                crate::clustering::metrics::record_orphan_worker_failure(worker, reason);
                error!(worker, %error, "orphan reaper worker failed");
            }
        }
        let retirement_results = futures::future::join_all(unwon_reservations.into_iter().map(
            |(entity, attempted_owner, reservation)| {
                let registry = self.registry.clone();
                async move {
                    let entity_id = entity.id.clone();
                    let result = registry
                        .retire_uncertain_reclaimed_claim(&entity, &attempted_owner, reservation)
                        .await;
                    (entity_id, result)
                }
            },
        ))
        .await;
        for (entity_id, result) in retirement_results {
            if let Err(error) = result {
                error!(%error, %entity_id, "orphan reaper: terminal uncertain SM claim cleanup failed; retaining capacity reservation until process teardown");
            }
        }
        futures::future::join_all(hydrations.into_iter().map(|work| {
            let registry = self.registry.clone();
            let span = orphan_work_span("sm_hydration", &work.sweep_context);
            async move {
                match tokio::time::timeout(
                    ORPHAN_WORK_ATTEMPT_TIMEOUT,
                    registry.release_reclaimed_claim(
                        &work.entity,
                        &work.fence,
                        work.reservation,
                    ),
                )
                .await
                {
                    Ok(Ok(_)) => {}
                    Ok(Err(error)) => {
                        crate::clustering::metrics::record_orphan_terminal_cleanup_failure(
                            "sm_hydration",
                            "error",
                        );
                        debug!(%error, entity_id = %work.entity.id, "orphan reaper: terminal SM cleanup failed");
                    }
                    Err(_) => {
                        crate::clustering::metrics::record_orphan_terminal_cleanup_failure(
                            "sm_hydration",
                            "timeout",
                        );
                        debug!(entity_id = %work.entity.id, "orphan reaper: terminal SM cleanup timed out");
                    }
                }
            }
            .instrument(span)
        }))
        .await;
        crate::clustering::metrics::record_orphan_work_queue_depth("sm_hydration", 0);
        futures::future::join_all(releases.into_iter().map(|work| {
            let span = orphan_work_span("room_release", &work.sweep_context);
            async move {
                match tokio::time::timeout(
                    ORPHANED_ROOM_RELEASE_TIMEOUT,
                    work.claim_store
                        .release_exact(&work.entity, &work.owner, work.epoch),
                )
                .await
                {
                    Ok(Ok(_)) => {}
                    Ok(Err(error)) => {
                        crate::clustering::metrics::record_orphan_terminal_cleanup_failure(
                            "room_release",
                            "error",
                        );
                        debug!(%error, entity_id = %work.entity.id, "orphan reaper: terminal exact cleanup failed");
                    }
                    Err(_) => {
                        crate::clustering::metrics::record_orphan_terminal_cleanup_failure(
                            "room_release",
                            "timeout",
                        );
                        debug!(entity_id = %work.entity.id, "orphan reaper: terminal exact cleanup timed out");
                    }
                }
            }
            .instrument(span)
        }))
        .await;
        crate::clustering::metrics::record_orphan_work_queue_depth("room_release", 0);
        crate::clustering::metrics::record_orphan_work_queue_depth("room_handoff", 0);
        room_handoffs
    }

    #[cfg(test)]
    async fn shutdown(self) {
        self.shutdown_terminal().await;
    }
}

#[cfg(feature = "clustering")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReclaimedRegistration {
    Registered,
    Released,
    LostRace,
    CleanupScheduled,
}

#[cfg(feature = "clustering")]
struct ReclaimedRegistrationContext<'a> {
    workers: &'a OrphanReaperWorkers,
    room_registry: &'a waddle_xmpp::muc::RoomRegistry,
    claim_store: &'a Arc<dyn waddle_xmpp::ownership::ClaimStore>,
    me: &'a waddle_xmpp::ownership::NodeIdentity,
}

#[cfg(feature = "clustering")]
async fn register_reclaimed_epoch_or_cleanup(
    context: ReclaimedRegistrationContext<'_>,
    pending: waddle_xmpp::muc::room_registry_actor::PendingReclaimedRoom,
) -> ReclaimedRegistration {
    let room_jid = &pending.room_jid;
    let claim_fence = &pending.claim_fence;
    loop {
        let registration = context
            .room_registry
            .remember_pending_reclaimed_room(
                room_jid.clone(),
                claim_fence.clone(),
                pending.previous_owner.clone(),
            )
            .await;
        if registration.is_ok() {
            return ReclaimedRegistration::Registered;
        }
        if !context.room_registry.actor_ref().is_alive() {
            break;
        }
        // The registry reservation was established before the steal, so
        // demand remains serialized while mailbox backpressure clears. Do
        // not cancel it or release outside the actor while the actor is live.
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // A stopped registry cannot create or publish a replacement actor, so
    // exact cleanup can no longer race demand on this registry. Release the
    // won generation now; retain failures in the bounded supervised worker.
    let release = tokio::time::timeout(
        ORPHANED_ROOM_RELEASE_TIMEOUT,
        context
            .claim_store
            .release_exact(&claim_fence.entity, context.me, claim_fence.epoch),
    )
    .await;
    match release {
        Ok(Ok(waddle_xmpp::ownership::ExactReleaseOutcome::Released)) => {
            context.workers.forget_room_handoff(room_jid, claim_fence);
            ReclaimedRegistration::Released
        }
        Ok(Ok(waddle_xmpp::ownership::ExactReleaseOutcome::NotOwned)) => {
            context.workers.forget_room_handoff(room_jid, claim_fence);
            ReclaimedRegistration::LostRace
        }
        Ok(Err(error)) => {
            debug!(room = %room_jid, %error, "orphan reaper: stopped-registry exact cleanup failed; retaining supervised retry");
            loop {
                if context
                    .workers
                    .enqueue_release(ExactReleaseWork {
                        claim_store: context.claim_store.clone(),
                        entity: claim_fence.entity.clone(),
                        owner: context.me.clone(),
                        epoch: claim_fence.epoch,
                        sweep_context: current_sweep_context(),
                    })
                    .is_accepted()
                {
                    context.workers.forget_room_handoff(room_jid, claim_fence);
                    break ReclaimedRegistration::CleanupScheduled;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
        Err(_) => loop {
            if context
                .workers
                .enqueue_release(ExactReleaseWork {
                    claim_store: context.claim_store.clone(),
                    entity: claim_fence.entity.clone(),
                    owner: context.me.clone(),
                    epoch: claim_fence.epoch,
                    sweep_context: current_sweep_context(),
                })
                .is_accepted()
            {
                context.workers.forget_room_handoff(room_jid, claim_fence);
                break ReclaimedRegistration::CleanupScheduled;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        },
    }
}

#[cfg(feature = "clustering")]
async fn reconcile_registered_room_or_self_fence(
    workers: &OrphanReaperWorkers,
    room_registry: &waddle_xmpp::muc::RoomRegistry,
    room_jid: jid::BareJid,
    claim_fence: waddle_xmpp::muc::RoomClaimFenceContext,
    previous_owner: waddle_xmpp::ownership::NodeIdentity,
) -> Result<
    waddle_xmpp::muc::room_registry_actor::ReclaimedRoomOutcome,
    waddle_xmpp::muc::RoomRegistryError,
> {
    let result = room_registry
        .reconcile_reclaimed_room(room_jid.clone(), claim_fence.clone(), previous_owner)
        .await;
    match &result {
        Ok(_) => workers.forget_room_handoff(&room_jid, &claim_fence),
        Err(_) => {
            // Mailbox acceptance is not handler completion. If this uncertain
            // handoff cannot produce a typed outcome, retain the supervisor's
            // exact fence for terminal transfer and stop this node's lease
            // lifecycle so another incarnation can recover after expiry.
            workers.node_lifecycle.begin_fenced_recovery();
            workers.fatal_fence.cancel();
        }
    }
    result
}

#[cfg(all(test, feature = "clustering"))]
#[tokio::test]
async fn stopped_registry_after_steal_exactly_releases_the_won_claim() {
    use waddle_xmpp::ownership::{ClaimStore, Entity, EntityType, InProcessClaimStore};

    let registry = waddle_xmpp::muc::RoomRegistry::spawn(
        "muc.example.com".to_string(),
        waddle_xmpp::xep::xep0421::OccupantIdSecret::new(
            b"test-occupant-id-secret-32-bytes-long".to_vec(),
        )
        .expect("secret"),
        None,
    );
    registry.actor_ref().kill();
    registry.actor_ref().wait_for_shutdown().await;
    let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
    let me = waddle_xmpp::ownership::NodeIdentity::new("sweeper", "incarnation");
    let previous = waddle_xmpp::ownership::NodeIdentity::new("dead", "old");
    let room_jid: jid::BareJid = "stopped-registry@muc.example.com".parse().expect("room");
    let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
    let epoch = claim_store.acquire(&entity, &me).await.expect("won epoch");
    let supervisor = OrphanReaperSupervisor::new(
        Arc::new(waddle_xmpp::stream_management::InMemorySmSessionRegistry::new()),
        tokio_util::sync::CancellationToken::new(),
    );
    let pending = waddle_xmpp::muc::room_registry_actor::PendingReclaimedRoom {
        room_jid: room_jid.clone(),
        claim_fence: waddle_xmpp::muc::RoomClaimFenceContext::new(
            entity.clone(),
            me.clone(),
            epoch,
        ),
        previous_owner: previous.clone(),
    };
    supervisor.workers.remember_room_handoff(pending.clone());
    let outcome = register_reclaimed_epoch_or_cleanup(
        ReclaimedRegistrationContext {
            workers: &supervisor.workers,
            room_registry: &registry,
            claim_store: &claim_store,
            me: &me,
        },
        pending,
    )
    .await;
    assert_eq!(outcome, ReclaimedRegistration::Released);
    assert!(
        claim_store
            .current_claim(&entity)
            .await
            .expect("claim")
            .is_none(),
        "a stopped registry cannot adopt the won claim, so exact cleanup must not strand it"
    );
    supervisor.shutdown().await;
}

#[cfg(all(test, feature = "clustering"))]
#[tokio::test]
async fn terminal_shutdown_transfers_a_post_cas_room_handoff_into_registry_drain() {
    use waddle_xmpp::ownership::{ClaimStore, Entity, EntityType, InProcessClaimStore};

    let registry = waddle_xmpp::muc::RoomRegistry::spawn(
        "muc.example.com".to_string(),
        waddle_xmpp::xep::xep0421::OccupantIdSecret::new(
            b"test-occupant-id-secret-32-bytes-long".to_vec(),
        )
        .expect("secret"),
        None,
    );
    let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
    let me = waddle_xmpp::ownership::NodeIdentity::new("sweeper", "incarnation");
    let previous = waddle_xmpp::ownership::NodeIdentity::new("dead", "old");
    registry
        .actor_ref()
        .ask(
            waddle_xmpp::muc::room_registry_actor::WireClusteringClaims {
                claim_store: claim_store.clone(),
                node_identity: waddle_xmpp::ownership::SharedNodeIdentity::new(me.clone()),
                durable_store: None,
                rollout_backoff: None,
            },
        )
        .await
        .expect("wire registry claim store");
    let room_jid: jid::BareJid = "cancelled-before-remember@muc.example.com"
        .parse()
        .expect("room");
    assert!(registry
        .reserve_pending_reclaimed_room(room_jid.clone())
        .await
        .expect("reserve adoption"));
    let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
    let epoch = claim_store.acquire(&entity, &me).await.expect("won epoch");
    let supervisor = OrphanReaperSupervisor::new(
        Arc::new(waddle_xmpp::stream_management::InMemorySmSessionRegistry::new()),
        tokio_util::sync::CancellationToken::new(),
    );
    supervisor.workers.remember_room_handoff(
        waddle_xmpp::muc::room_registry_actor::PendingReclaimedRoom {
            room_jid: room_jid.clone(),
            claim_fence: waddle_xmpp::muc::RoomClaimFenceContext::new(entity.clone(), me, epoch),
            previous_owner: previous,
        },
    );

    let handoffs = supervisor.shutdown_terminal().await;
    assert_eq!(handoffs.len(), 1);
    let outcome = registry
        .drain_room_ownership_for_shutdown(handoffs)
        .await
        .expect("drain transferred handoff");

    assert_eq!(outcome.released, 1);
    assert_eq!(outcome.retained, 0);
    assert!(claim_store
        .current_claim(&entity)
        .await
        .expect("claim")
        .is_none());

    registry.actor_ref().kill();
    registry.actor_ref().wait_for_shutdown().await;
}

#[cfg(all(test, feature = "clustering"))]
#[tokio::test]
async fn cancelled_after_known_steal_still_registers_the_exact_room_fence() {
    use waddle_xmpp::ownership::{ClaimStore, Entity, EntityType, InProcessClaimStore};

    let registry = waddle_xmpp::muc::RoomRegistry::spawn(
        "muc.example.com".to_string(),
        waddle_xmpp::xep::xep0421::OccupantIdSecret::new(
            b"test-occupant-id-secret-32-bytes-long".to_vec(),
        )
        .expect("secret"),
        None,
    );
    let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
    let me = waddle_xmpp::ownership::NodeIdentity::new("sweeper", "incarnation");
    let previous = waddle_xmpp::ownership::NodeIdentity::new("dead", "old");
    registry
        .actor_ref()
        .ask(
            waddle_xmpp::muc::room_registry_actor::WireClusteringClaims {
                claim_store: claim_store.clone(),
                node_identity: waddle_xmpp::ownership::SharedNodeIdentity::new(me.clone()),
                durable_store: None,
                rollout_backoff: None,
            },
        )
        .await
        .expect("wire registry claim store");
    let room_jid: jid::BareJid = "cancelled-registration@muc.example.com"
        .parse()
        .expect("room");
    let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
    let epoch = claim_store.acquire(&entity, &me).await.expect("won epoch");
    let cancel = tokio_util::sync::CancellationToken::new();
    let supervisor = OrphanReaperSupervisor::new(
        Arc::new(waddle_xmpp::stream_management::InMemorySmSessionRegistry::new()),
        cancel.clone(),
    );
    cancel.cancel();

    let pending = waddle_xmpp::muc::room_registry_actor::PendingReclaimedRoom {
        room_jid: room_jid.clone(),
        claim_fence: waddle_xmpp::muc::RoomClaimFenceContext::new(
            entity.clone(),
            me.clone(),
            epoch,
        ),
        previous_owner: previous.clone(),
    };
    supervisor.workers.remember_room_handoff(pending.clone());

    let outcome = register_reclaimed_epoch_or_cleanup(
        ReclaimedRegistrationContext {
            workers: &supervisor.workers,
            room_registry: &registry,
            claim_store: &claim_store,
            me: &me,
        },
        pending,
    )
    .await;
    assert_eq!(outcome, ReclaimedRegistration::Registered);
    let pending = registry
        .list_pending_reclaimed_rooms(1)
        .await
        .expect("list retained exact room fence");
    assert_eq!(pending.len(), 1);
    assert!(
        claim_store
            .current_claim(&entity)
            .await
            .expect("claim")
            .is_some(),
        "known post-CAS ownership must stay actor-serialized while the registry is live"
    );

    assert_eq!(
        supervisor.workers.pending_room_handoffs().len(),
        1,
        "mailbox acceptance alone must not retire supervisor ownership"
    );
    let reconciliation = reconcile_registered_room_or_self_fence(
        &supervisor.workers,
        &registry,
        room_jid.clone(),
        waddle_xmpp::muc::RoomClaimFenceContext::new(entity.clone(), me, epoch),
        previous,
    )
    .await
    .expect("typed reconciliation");
    assert_eq!(
        reconciliation,
        waddle_xmpp::muc::room_registry_actor::ReclaimedRoomOutcome::Released
    );
    assert!(supervisor.workers.pending_room_handoffs().is_empty());
    assert!(claim_store
        .current_claim(&entity)
        .await
        .expect("claim after typed reconciliation")
        .is_none());

    registry.actor_ref().kill();
    registry.actor_ref().wait_for_shutdown().await;
    supervisor.shutdown().await;
}

#[cfg(all(test, feature = "clustering"))]
#[tokio::test]
async fn registry_death_after_mailbox_acceptance_self_fences_the_node() {
    use waddle_xmpp::ownership::{ClaimStore, Entity, EntityType, InProcessClaimStore};

    let registry = waddle_xmpp::muc::RoomRegistry::spawn(
        "muc.example.com".to_string(),
        waddle_xmpp::xep::xep0421::OccupantIdSecret::new(
            b"test-occupant-id-secret-32-bytes-long".to_vec(),
        )
        .expect("secret"),
        None,
    );
    let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
    let me = waddle_xmpp::ownership::NodeIdentity::new("sweeper", "incarnation");
    let previous = waddle_xmpp::ownership::NodeIdentity::new("dead", "old");
    let room_jid: jid::BareJid = "accepted-then-dead@muc.example.com".parse().expect("room");
    let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
    let epoch = claim_store.acquire(&entity, &me).await.expect("won epoch");
    assert!(registry
        .reserve_pending_reclaimed_room(room_jid.clone())
        .await
        .expect("reserve adoption"));
    let node_lifecycle = crate::clustering::NodeLifecycle::new();
    let fatal_fence = node_lifecycle.fatal_fence_token();
    let supervisor = OrphanReaperSupervisor::new_with_fatal_fence(
        Arc::new(waddle_xmpp::stream_management::InMemorySmSessionRegistry::new()),
        tokio_util::sync::CancellationToken::new(),
        fatal_fence.clone(),
        node_lifecycle,
    );
    let pending = waddle_xmpp::muc::room_registry_actor::PendingReclaimedRoom {
        room_jid: room_jid.clone(),
        claim_fence: waddle_xmpp::muc::RoomClaimFenceContext::new(
            entity.clone(),
            me.clone(),
            epoch,
        ),
        previous_owner: previous.clone(),
    };
    supervisor.workers.remember_room_handoff(pending.clone());

    assert_eq!(
        register_reclaimed_epoch_or_cleanup(
            ReclaimedRegistrationContext {
                workers: &supervisor.workers,
                room_registry: &registry,
                claim_store: &claim_store,
                me: &me,
            },
            pending,
        )
        .await,
        ReclaimedRegistration::Registered
    );
    registry.actor_ref().kill();
    registry.actor_ref().wait_for_shutdown().await;

    let result = reconcile_registered_room_or_self_fence(
        &supervisor.workers,
        &registry,
        room_jid,
        waddle_xmpp::muc::RoomClaimFenceContext::new(entity.clone(), me, epoch),
        previous,
    )
    .await;
    assert!(result.is_err());
    assert!(fatal_fence.is_cancelled());
    assert!(
        claim_store
            .current_claim(&entity)
            .await
            .expect("claim")
            .is_some(),
        "uncertain post-accept handoff must self-fence, not guess that release is safe"
    );
    let handoffs = supervisor.shutdown_terminal().await;
    assert_eq!(
        handoffs.len(),
        1,
        "failed reconciliation must retain the exact supervisor handoff"
    );
}

#[cfg(all(test, feature = "clustering"))]
#[tokio::test]
async fn idle_room_registry_death_self_fences_the_node() {
    let registry = waddle_xmpp::muc::RoomRegistry::spawn(
        "muc.example.com".to_string(),
        waddle_xmpp::xep::xep0421::OccupantIdSecret::new(
            b"test-occupant-id-secret-32-bytes-long".to_vec(),
        )
        .expect("secret"),
        None,
    );
    let node_lifecycle = crate::clustering::NodeLifecycle::new();
    let process_stop = tokio_util::sync::CancellationToken::new();
    let armed = spawn_room_registry_lifetime_watch(
        registry.actor_ref().clone(),
        node_lifecycle.clone(),
        process_stop.clone(),
    );
    armed.await.expect("registry lifetime watcher armed");

    registry.actor_ref().kill();
    registry.actor_ref().wait_for_shutdown().await;
    tokio::time::timeout(Duration::from_secs(1), process_stop.cancelled())
        .await
        .expect("registry lifetime watcher must cancel the node promptly");
    assert_eq!(
        node_lifecycle.critical_failure(),
        Some(crate::clustering::CriticalNodeFailure::RoomRegistryTerminated)
    );
}

#[cfg(all(test, feature = "clustering"))]
#[derive(Default)]
struct HangingSmReadPersistence {
    inner: waddle_xmpp::stream_management::persistence::InMemorySmPersistence,
    read_started: tokio::sync::Notify,
}

#[cfg(all(test, feature = "clustering"))]
#[async_trait::async_trait]
impl waddle_xmpp::stream_management::persistence::SmPersistenceStorage
    for HangingSmReadPersistence
{
    async fn upsert_session(
        &self,
        session: waddle_xmpp::stream_management::persistence::PersistedSession,
    ) -> Result<(), waddle_xmpp::stream_management::persistence::SmPersistenceError> {
        self.inner.upsert_session(session).await
    }

    async fn get_session(
        &self,
        _stream_id: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<
        Option<waddle_xmpp::stream_management::persistence::PersistedSession>,
        waddle_xmpp::stream_management::persistence::SmPersistenceError,
    > {
        drop(tracing::info_span!("orphan.worker.hydration.test"));
        self.read_started.notify_one();
        std::future::pending().await
    }

    async fn delete_session(
        &self,
        stream_id: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<(), waddle_xmpp::stream_management::persistence::SmPersistenceError> {
        self.inner.delete_session(stream_id).await
    }

    async fn append_unacked(
        &self,
        stanza: waddle_xmpp::stream_management::persistence::PersistedUnackedStanza,
    ) -> Result<(), waddle_xmpp::stream_management::persistence::SmPersistenceError> {
        self.inner.append_unacked(stanza).await
    }

    async fn ack_through(
        &self,
        stream_id: &waddle_xmpp::pending_delivery::SmSessionId,
        up_to_sequence: u32,
    ) -> Result<u64, waddle_xmpp::stream_management::persistence::SmPersistenceError> {
        self.inner.ack_through(stream_id, up_to_sequence).await
    }

    async fn delete_unacked(
        &self,
        stream_id: &waddle_xmpp::pending_delivery::SmSessionId,
        sequences: &[u32],
    ) -> Result<u64, waddle_xmpp::stream_management::persistence::SmPersistenceError> {
        self.inner.delete_unacked(stream_id, sequences).await
    }

    async fn list_unacked(
        &self,
        stream_id: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<
        Vec<waddle_xmpp::stream_management::persistence::PersistedUnackedStanza>,
        waddle_xmpp::stream_management::persistence::SmPersistenceError,
    > {
        self.inner.list_unacked(stream_id).await
    }

    async fn list_expired_sessions(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<
        Vec<waddle_xmpp::stream_management::persistence::PersistedSession>,
        waddle_xmpp::stream_management::persistence::SmPersistenceError,
    > {
        self.inner.list_expired_sessions(now).await
    }

    async fn list_all_sessions(
        &self,
    ) -> Result<
        Vec<waddle_xmpp::stream_management::persistence::PersistedSession>,
        waddle_xmpp::stream_management::persistence::SmPersistenceError,
    > {
        self.inner.list_all_sessions().await
    }
}

#[cfg(all(test, feature = "clustering"))]
struct HangingExactReleaseStore {
    inner: waddle_xmpp::ownership::InProcessClaimStore,
    release_started: tokio::sync::Notify,
}

#[cfg(all(test, feature = "clustering"))]
#[async_trait::async_trait]
impl waddle_xmpp::ownership::ClaimStore for HangingExactReleaseStore {
    async fn ensure_schema(&self) -> Result<(), waddle_xmpp::ownership::ClaimError> {
        self.inner.ensure_schema().await
    }
    async fn acquire(
        &self,
        entity: &waddle_xmpp::ownership::Entity,
        me: &waddle_xmpp::ownership::NodeIdentity,
    ) -> Result<waddle_xmpp::ownership::ClaimEpoch, waddle_xmpp::ownership::ClaimError> {
        self.inner.acquire(entity, me).await
    }
    async fn ensure_claimed(
        &self,
        entity: &waddle_xmpp::ownership::Entity,
        me: &waddle_xmpp::ownership::NodeIdentity,
    ) -> Result<waddle_xmpp::ownership::ClaimEpoch, waddle_xmpp::ownership::ClaimError> {
        self.inner.ensure_claimed(entity, me).await
    }
    async fn steal_stale(
        &self,
        entity: &waddle_xmpp::ownership::Entity,
        observed: waddle_xmpp::ownership::ClaimEpoch,
        staleness: waddle_xmpp::ownership::StalePredicate,
        me: &waddle_xmpp::ownership::NodeIdentity,
    ) -> Result<waddle_xmpp::ownership::ClaimEpoch, waddle_xmpp::ownership::ClaimError> {
        self.inner
            .steal_stale(entity, observed, staleness, me)
            .await
    }
    async fn steal_for_resume(
        &self,
        entity: &waddle_xmpp::ownership::Entity,
        observed: waddle_xmpp::ownership::ClaimEpoch,
        witness: waddle_xmpp::ownership::ResumeIdentityProof,
        me: &waddle_xmpp::ownership::NodeIdentity,
    ) -> Result<waddle_xmpp::ownership::ClaimEpoch, waddle_xmpp::ownership::ClaimError> {
        self.inner
            .steal_for_resume(entity, observed, witness, me)
            .await
    }
    async fn current_claim(
        &self,
        entity: &waddle_xmpp::ownership::Entity,
    ) -> Result<Option<waddle_xmpp::ownership::ClaimSnapshot>, waddle_xmpp::ownership::ClaimError>
    {
        self.inner.current_claim(entity).await
    }
    async fn fence(
        &self,
        entity: &waddle_xmpp::ownership::Entity,
        me: &waddle_xmpp::ownership::NodeIdentity,
        mine: waddle_xmpp::ownership::ClaimEpoch,
    ) -> Result<bool, waddle_xmpp::ownership::ClaimError> {
        self.inner.fence(entity, me, mine).await
    }
    async fn release(
        &self,
        entity: &waddle_xmpp::ownership::Entity,
        me: &waddle_xmpp::ownership::NodeIdentity,
        mine: waddle_xmpp::ownership::ClaimEpoch,
    ) -> Result<(), waddle_xmpp::ownership::ClaimError> {
        self.inner.release(entity, me, mine).await
    }
    async fn release_exact(
        &self,
        _entity: &waddle_xmpp::ownership::Entity,
        _me: &waddle_xmpp::ownership::NodeIdentity,
        _mine: waddle_xmpp::ownership::ClaimEpoch,
    ) -> Result<waddle_xmpp::ownership::ExactReleaseOutcome, waddle_xmpp::ownership::ClaimError>
    {
        drop(tracing::info_span!("orphan.worker.release.test"));
        self.release_started.notify_one();
        std::future::pending().await
    }
    async fn release_many(
        &self,
        entities: &[waddle_xmpp::ownership::Entity],
        me: &waddle_xmpp::ownership::NodeIdentity,
    ) -> Result<(), waddle_xmpp::ownership::ClaimError> {
        self.inner.release_many(entities, me).await
    }
}

#[cfg(all(test, feature = "clustering"))]
#[tokio::test]
async fn hung_sm_hydration_does_not_block_completed_room_lane() {
    use waddle_xmpp::ownership::{
        ClaimStore, Entity, EntityType, InProcessClaimStore, NodeIdentity, SharedNodeIdentity,
    };

    let storage = Arc::new(HangingSmReadPersistence::default());
    let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
    let me = NodeIdentity::new("sweeper", "incarnation");
    let registry = Arc::new(
        waddle_xmpp::stream_management::InMemorySmSessionRegistry::new()
            .with_persistence(storage.clone())
            .with_claim_store(claim_store.clone(), SharedNodeIdentity::new(me.clone())),
    );
    let entity = Entity::new(EntityType::SmSession, "hung-session");
    let epoch = claim_store.acquire(&entity, &me).await.expect("claim");

    // Production completes the bounded RoomActor lane before scheduling
    // this detached SM hydration. A storage read that never returns must
    // therefore neither undo nor delay that room progress.
    let room_lane_completed = true;
    let reservation = registry
        .reserve_reclaimed_claim_capacity(&entity)
        .expect("hung hydration reservation");
    let cancel = tokio_util::sync::CancellationToken::new();
    let supervisor = OrphanReaperSupervisor::new(registry, cancel.clone());
    supervisor.workers.enqueue_hydration(
        entity,
        waddle_xmpp::stream_management::persistence::SmClaimFence::new(me.clone(), epoch),
        reservation,
    );

    tokio::time::timeout(Duration::from_secs(1), storage.read_started.notified())
        .await
        .expect("detached hydration should reach the deliberately hung SM read");
    assert!(room_lane_completed);
    for index in 1..ORPHAN_WORK_QUEUE_CAPACITY {
        assert!(supervisor
            .workers
            .enqueue_hydration(
                Entity::new(EntityType::SmSession, format!("queued-{index}")),
                waddle_xmpp::stream_management::persistence::SmClaimFence::new(
                    me.clone(),
                    waddle_xmpp::ownership::ClaimEpoch(index as i64),
                ),
                waddle_xmpp::stream_management::ReclaimedClaimReservation::from_generation(
                    index as u64 + 1,
                ),
            )
            .is_accepted());
    }
    assert_eq!(
        supervisor.workers.enqueue_hydration(
            Entity::new(EntityType::SmSession, "queue-overflow"),
            waddle_xmpp::stream_management::persistence::SmClaimFence::new(
                me,
                waddle_xmpp::ownership::ClaimEpoch(999),
            ),
            waddle_xmpp::stream_management::ReclaimedClaimReservation::from_generation(999),
        ),
        WorkEnqueueOutcome::Rejected
    );
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(1), supervisor.shutdown())
        .await
        .expect("hung hydration worker must cancel and join");
}

/// #1483 verify round: worker attempts run under their own short-lived
/// `janitor.orphan_work` root (never the sweep's live span, which retry
/// queues would pin open) and carry a *link* back to the enqueuing sweep.
#[cfg(all(test, feature = "clustering"))]
fn assert_linked_orphan_work_attempt(
    spans: &waddle_xmpp::telemetry::test_support::SpanTestGuard,
    sweep_id: opentelemetry::trace::SpanId,
    lane: &str,
    work_marker: &str,
) {
    let exported = spans.exported();
    let attempt = exported
        .iter()
        .find(|span| {
            span.name == "janitor.orphan_work"
                && span
                    .attributes
                    .iter()
                    .any(|kv| kv.key.as_str() == "lane" && kv.value.as_str() == lane)
        })
        .unwrap_or_else(|| panic!("a {lane} attempt span must export"));
    assert_eq!(
        attempt.parent_span_id,
        opentelemetry::trace::SpanId::INVALID,
        "attempt spans must be roots, never children of the sweep"
    );
    assert!(
        attempt
            .links
            .iter()
            .any(|link| link.span_context.span_id() == sweep_id),
        "the attempt must link back to the sweep that enqueued it"
    );
    // The marker span the hanging store mints proves the store call ran
    // INSIDE an attempt span — not merely that an attempt span exists.
    let marker = exported
        .iter()
        .find(|span| span.name == work_marker)
        .unwrap_or_else(|| panic!("the {work_marker} marker span must export"));
    assert!(
        exported
            .iter()
            .any(|span| span.name == "janitor.orphan_work"
                && span.span_context.span_id() == marker.parent_span_id),
        "the store call must run inside an attempt span"
    );
}

#[cfg(all(test, feature = "clustering"))]
#[tokio::test(flavor = "current_thread")]
async fn hydration_worker_attempts_link_to_the_enqueuing_sweep() {
    use waddle_xmpp::ownership::{
        ClaimStore, Entity, EntityType, InProcessClaimStore, NodeIdentity, SharedNodeIdentity,
    };

    let storage = Arc::new(HangingSmReadPersistence::default());
    let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
    let me = NodeIdentity::new("sweeper", "trace-incarnation");
    let registry = Arc::new(
        waddle_xmpp::stream_management::InMemorySmSessionRegistry::new()
            .with_persistence(storage.clone())
            .with_claim_store(claim_store.clone(), SharedNodeIdentity::new(me.clone())),
    );
    let entity = Entity::new(EntityType::SmSession, "traced-hydration");
    let epoch = claim_store.acquire(&entity, &me).await.expect("claim");
    let reservation = registry
        .reserve_reclaimed_claim_capacity(&entity)
        .expect("hydration reservation");
    let cancel = tokio_util::sync::CancellationToken::new();
    let supervisor = OrphanReaperSupervisor::new(registry, cancel.clone());
    let spans = waddle_xmpp::telemetry::test_support::acquire_spans();
    let sweep = janitor_sweep_span(Janitor::OrphanReaper);
    let sweep_id = sweep.in_scope(current_sweep_context).span_id();

    assert!(sweep
        .in_scope(|| supervisor.workers.enqueue_hydration(
            entity,
            waddle_xmpp::stream_management::persistence::SmClaimFence::new(me, epoch),
            reservation,
        ))
        .is_accepted());
    drop(sweep);
    tokio::time::timeout(Duration::from_secs(5), storage.read_started.notified())
        .await
        .expect("hydration worker started");
    cancel.cancel();
    supervisor.shutdown().await;

    assert_linked_orphan_work_attempt(
        &spans,
        sweep_id,
        "sm_hydration",
        "orphan.worker.hydration.test",
    );
}

#[cfg(all(test, feature = "clustering"))]
#[tokio::test]
async fn terminal_shutdown_retains_an_unobserved_uncertain_sm_reservation() {
    use waddle_xmpp::ownership::{Entity, EntityType, NodeIdentity};

    let registry =
        Arc::new(waddle_xmpp::stream_management::InMemorySmSessionRegistry::with_capacity(1));
    let entity = Entity::new(EntityType::SmSession, "reserved-before-steal");
    let replacement = Entity::new(EntityType::SmSession, "replacement-after-fence");
    let reservation = registry
        .reserve_reclaimed_claim_capacity(&entity)
        .expect("initial reservation");
    let supervisor =
        OrphanReaperSupervisor::new(registry.clone(), tokio_util::sync::CancellationToken::new());
    assert!(supervisor.workers.reserve_hydration(
        &entity,
        &NodeIdentity::new("sweeper", "incarnation"),
        reservation,
    ));
    assert!(registry
        .reserve_reclaimed_claim_capacity(&replacement)
        .is_none());

    supervisor.shutdown_terminal().await;

    assert!(registry
        .reserve_reclaimed_claim_capacity(&replacement)
        .is_none());
}

#[cfg(all(test, feature = "clustering"))]
#[tokio::test]
async fn worker_restart_self_fences_when_an_uncertain_sm_claim_is_unobserved() {
    use waddle_xmpp::ownership::{
        ClaimStore, Entity, EntityType, InProcessClaimStore, NodeIdentity, SharedNodeIdentity,
    };

    let registry =
        Arc::new(waddle_xmpp::stream_management::InMemorySmSessionRegistry::with_capacity(1));
    let entity = Entity::new(EntityType::SmSession, "unobserved-during-restart");
    let replacement = Entity::new(EntityType::SmSession, "replacement-after-restart");
    let reservation = registry
        .reserve_reclaimed_claim_capacity(&entity)
        .expect("initial reservation");
    let parent_cancel = tokio_util::sync::CancellationToken::new();
    let supervisor = OrphanReaperSupervisor::new(registry.clone(), parent_cancel.clone());
    let fatal_fence = supervisor.workers.fatal_fence.clone();
    assert!(supervisor.workers.reserve_hydration(
        &entity,
        &NodeIdentity::new("sweeper", "incarnation"),
        reservation,
    ));
    let room_claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
    let room_owner = NodeIdentity::new("sweeper", "room-incarnation");
    let previous_room_owner = NodeIdentity::new("dead", "room-incarnation");
    let room_jid: jid::BareJid = "handoff-across-failed-restart@muc.example.com"
        .parse()
        .expect("room");
    let room_entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
    let room_epoch = room_claim_store
        .acquire(&room_entity, &room_owner)
        .await
        .expect("room claim");
    supervisor.workers.remember_room_handoff(
        waddle_xmpp::muc::room_registry_actor::PendingReclaimedRoom {
            room_jid: room_jid.clone(),
            claim_fence: waddle_xmpp::muc::RoomClaimFenceContext::new(
                room_entity.clone(),
                room_owner.clone(),
                room_epoch,
            ),
            previous_owner: previous_room_owner,
        },
    );

    let restarted = supervisor.restarted(registry.clone(), parent_cancel).await;

    assert!(fatal_fence.is_cancelled());
    assert!(restarted.workers.cancel.is_cancelled());
    assert!(registry
        .reserve_reclaimed_claim_capacity(&replacement)
        .is_none());
    let room_handoffs = restarted.shutdown_terminal().await;
    assert_eq!(room_handoffs.len(), 1);

    let room_registry = waddle_xmpp::muc::RoomRegistry::spawn(
        "muc.example.com".to_string(),
        waddle_xmpp::xep::xep0421::OccupantIdSecret::new(
            b"test-occupant-id-secret-32-bytes-long".to_vec(),
        )
        .expect("secret"),
        None,
    );
    room_registry
        .actor_ref()
        .ask(
            waddle_xmpp::muc::room_registry_actor::WireClusteringClaims {
                claim_store: room_claim_store.clone(),
                node_identity: SharedNodeIdentity::new(room_owner),
                durable_store: None,
                rollout_backoff: None,
            },
        )
        .await
        .expect("wire room registry");
    room_registry
        .drain_room_ownership_for_shutdown(room_handoffs)
        .await
        .expect("drain handoff after failed restart");
    assert!(room_claim_store
        .current_claim(&room_entity)
        .await
        .expect("room claim after drain")
        .is_none());
    room_registry.actor_ref().kill();
    room_registry.actor_ref().wait_for_shutdown().await;
}

#[cfg(all(test, feature = "clustering"))]
#[tokio::test]
async fn failed_worker_restart_retains_captured_won_work_for_terminal_cleanup() {
    use waddle_xmpp::ownership::{
        ClaimStore, Entity, EntityType, InProcessClaimStore, NodeIdentity, SharedNodeIdentity,
    };

    let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
    let me = NodeIdentity::new("sweeper", "failed-restart-incarnation");
    let registry = Arc::new(
        waddle_xmpp::stream_management::InMemorySmSessionRegistry::with_capacity(2)
            .with_claim_store(claim_store.clone(), SharedNodeIdentity::new(me.clone())),
    );
    let parent_cancel = tokio_util::sync::CancellationToken::new();
    let supervisor = OrphanReaperSupervisor::new(registry.clone(), parent_cancel.clone());
    let fatal_fence = supervisor.workers.fatal_fence.clone();

    let won_sm = Entity::new(EntityType::SmSession, "won-before-failed-restart");
    let won_epoch = claim_store
        .acquire(&won_sm, &me)
        .await
        .expect("won SM claim");
    let won_reservation = registry
        .reserve_reclaimed_claim_capacity(&won_sm)
        .expect("won SM reservation");
    let won_work = SmHydrationWork {
        entity: won_sm.clone(),
        fence: waddle_xmpp::stream_management::persistence::SmClaimFence::new(
            me.clone(),
            won_epoch,
        ),
        reservation: won_reservation,
        sweep_context: opentelemetry::trace::SpanContext::empty_context(),
    };
    supervisor
        .workers
        .hydration_pending
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(won_sm.id.clone(), PendingSmHydration::Won(won_work));

    let uncertain = Entity::new(EntityType::SmSession, "unobserved-during-failed-restart");
    let uncertain_reservation = registry
        .reserve_reclaimed_claim_capacity(&uncertain)
        .expect("uncertain SM reservation");
    assert!(supervisor
        .workers
        .reserve_hydration(&uncertain, &me, uncertain_reservation,));

    let room_entity = Entity::new(
        EntityType::RoomActor,
        "release-before-failed-restart@muc.example.com",
    );
    let room_epoch = claim_store
        .acquire(&room_entity, &me)
        .await
        .expect("room claim");
    let release_work = ExactReleaseWork {
        claim_store: claim_store.clone(),
        entity: room_entity.clone(),
        owner: me.clone(),
        epoch: room_epoch,
        sweep_context: current_sweep_context(),
    };
    supervisor
        .workers
        .release_pending
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(
            OrphanReaperWorkers::release_key(&release_work),
            release_work,
        );

    let restarted = supervisor.restarted(registry.clone(), parent_cancel).await;

    assert!(fatal_fence.is_cancelled());
    assert!(restarted.workers.cancel.is_cancelled());
    assert!(
        restarted.tasks.is_empty(),
        "failed restart must return the stopped supervisor as a terminal carrier"
    );

    restarted.shutdown_terminal().await;

    assert!(claim_store
        .current_claim(&won_sm)
        .await
        .expect("won SM claim after terminal cleanup")
        .is_none());
    assert!(claim_store
        .current_claim(&room_entity)
        .await
        .expect("room claim after terminal cleanup")
        .is_none());
    let first_replacement = Entity::new(EntityType::SmSession, "replacement-after-won-cleanup");
    let second_replacement = Entity::new(EntityType::SmSession, "replacement-after-uncertain");
    assert!(registry
        .reserve_reclaimed_claim_capacity(&first_replacement)
        .is_some());
    assert!(registry
        .reserve_reclaimed_claim_capacity(&second_replacement)
        .is_none());
}

#[cfg(all(test, feature = "clustering"))]
#[tokio::test]
async fn terminal_shutdown_reconciles_an_unknown_sm_steal_result_read_only() {
    use waddle_xmpp::ownership::{
        ClaimStore, Entity, EntityType, InProcessClaimStore, NodeIdentity, SharedNodeIdentity,
    };

    let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
    let attempted_owner = NodeIdentity::new("sweeper", "uncertain-incarnation");
    let registry = Arc::new(
        waddle_xmpp::stream_management::InMemorySmSessionRegistry::with_capacity(1)
            .with_claim_store(
                claim_store.clone(),
                SharedNodeIdentity::new(attempted_owner.clone()),
            ),
    );
    let entity = Entity::new(EntityType::SmSession, "uncertain-steal");
    let replacement = Entity::new(EntityType::SmSession, "replacement-after-reconcile");
    let reservation = registry
        .reserve_reclaimed_claim_capacity(&entity)
        .expect("initial reservation");
    let supervisor =
        OrphanReaperSupervisor::new(registry.clone(), tokio_util::sync::CancellationToken::new());
    assert!(supervisor
        .workers
        .reserve_hydration(&entity, &attempted_owner, reservation,));
    claim_store
        .acquire(&entity, &attempted_owner)
        .await
        .expect("simulate an ambiguously committed steal");

    supervisor.shutdown_terminal().await;

    assert!(
        claim_store
            .current_claim(&entity)
            .await
            .expect("read reconciled claim")
            .is_none(),
        "read-only reconciliation must discover and terminally release the won claim"
    );
    assert!(registry
        .reserve_reclaimed_claim_capacity(&replacement)
        .is_some());
}

#[cfg(all(test, feature = "clustering"))]
#[tokio::test]
async fn worker_restart_preserves_hung_sm_reservation_until_terminal_shutdown() {
    use waddle_xmpp::ownership::{
        ClaimStore, Entity, EntityType, InProcessClaimStore, NodeIdentity, SharedNodeIdentity,
    };

    let storage = Arc::new(HangingSmReadPersistence::default());
    let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
    let me = NodeIdentity::new("sweeper", "restart-incarnation");
    let registry = Arc::new(
        waddle_xmpp::stream_management::InMemorySmSessionRegistry::with_capacity(1)
            .with_persistence(storage.clone())
            .with_claim_store(claim_store.clone(), SharedNodeIdentity::new(me.clone())),
    );
    let entity = Entity::new(EntityType::SmSession, "hung-across-restart");
    let replacement = Entity::new(EntityType::SmSession, "replacement-after-shutdown");
    let epoch = claim_store.acquire(&entity, &me).await.expect("claim");
    let reservation = registry
        .reserve_reclaimed_claim_capacity(&entity)
        .expect("initial reservation");
    let parent_cancel = tokio_util::sync::CancellationToken::new();
    let supervisor = OrphanReaperSupervisor::new(registry.clone(), parent_cancel.clone());
    assert!(supervisor
        .workers
        .enqueue_hydration(
            entity,
            waddle_xmpp::stream_management::persistence::SmClaimFence::new(me, epoch),
            reservation,
        )
        .is_accepted());
    tokio::time::timeout(Duration::from_secs(1), storage.read_started.notified())
        .await
        .expect("first worker reaches the hung read");

    let restarted = supervisor.restarted(registry.clone(), parent_cancel).await;
    assert!(
        registry
            .reserve_reclaimed_claim_capacity(&replacement)
            .is_none(),
        "worker restart must retain the exact existing reservation"
    );

    restarted.shutdown_terminal().await;
    assert!(registry
        .reserve_reclaimed_claim_capacity(&replacement)
        .is_some());
}

#[cfg(all(test, feature = "clustering"))]
#[tokio::test]
async fn sm_rotation_before_hydration_worker_releases_the_exact_old_fence() {
    use waddle_xmpp::ownership::{
        ClaimStore, Entity, EntityType, InProcessClaimStore, NodeIdentity, SharedNodeIdentity,
    };

    let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
    let old_owner = NodeIdentity::new("sweeper", "old-incarnation");
    let new_owner = NodeIdentity::new("sweeper", "new-incarnation");
    let identity = SharedNodeIdentity::new(old_owner.clone());
    let entity = Entity::new(EntityType::SmSession, "rotate-before-worker");
    let epoch = claim_store
        .acquire(&entity, &old_owner)
        .await
        .expect("seed old claim");
    let registry = Arc::new(
        waddle_xmpp::stream_management::InMemorySmSessionRegistry::new()
            .with_claim_store(claim_store.clone(), identity.clone()),
    );
    let reservation = registry
        .reserve_reclaimed_claim_capacity(&entity)
        .expect("stale hydration reservation");
    let cancel = tokio_util::sync::CancellationToken::new();
    let supervisor = OrphanReaperSupervisor::new(registry, cancel.clone());

    identity.rotate(new_owner).await;
    assert!(supervisor
        .workers
        .enqueue_hydration(
            entity.clone(),
            waddle_xmpp::stream_management::persistence::SmClaimFence::new(old_owner, epoch),
            reservation,
        )
        .is_accepted());

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if claim_store
                .current_claim(&entity)
                .await
                .expect("read claim")
                .is_none()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("stale worker must release the exact old fence");
    assert!(supervisor.workers.pending_hydrations().is_empty());

    cancel.cancel();
    supervisor.shutdown().await;
}

#[cfg(all(test, feature = "clustering"))]
#[tokio::test(start_paused = true)]
async fn sm_rotation_between_hydration_retries_releases_the_exact_old_fence() {
    use waddle_xmpp::ownership::{
        ClaimStore, Entity, EntityType, InProcessClaimStore, NodeIdentity, SharedNodeIdentity,
    };

    let storage = Arc::new(HangingSmReadPersistence::default());
    let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
    let old_owner = NodeIdentity::new("sweeper", "old-incarnation");
    let new_owner = NodeIdentity::new("sweeper", "new-incarnation");
    let identity = SharedNodeIdentity::new(old_owner.clone());
    let entity = Entity::new(EntityType::SmSession, "rotate-between-retries");
    let epoch = claim_store
        .acquire(&entity, &old_owner)
        .await
        .expect("seed old claim");
    let registry = Arc::new(
        waddle_xmpp::stream_management::InMemorySmSessionRegistry::new()
            .with_persistence(storage.clone())
            .with_claim_store(claim_store.clone(), identity.clone()),
    );
    let reservation = registry
        .reserve_reclaimed_claim_capacity(&entity)
        .expect("retry hydration reservation");
    let cancel = tokio_util::sync::CancellationToken::new();
    let supervisor = OrphanReaperSupervisor::new(registry, cancel.clone());
    assert!(supervisor
        .workers
        .enqueue_hydration(
            entity.clone(),
            waddle_xmpp::stream_management::persistence::SmClaimFence::new(old_owner, epoch),
            reservation,
        )
        .is_accepted());

    storage.read_started.notified().await;
    identity.rotate(new_owner).await;
    tokio::time::advance(ORPHAN_WORK_ATTEMPT_TIMEOUT).await;
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(1)).await;
    for _ in 0..100 {
        if claim_store
            .current_claim(&entity)
            .await
            .expect("read claim")
            .is_none()
        {
            break;
        }
        tokio::task::yield_now().await;
    }

    assert!(claim_store
        .current_claim(&entity)
        .await
        .expect("read final claim")
        .is_none());
    assert!(supervisor.workers.pending_hydrations().is_empty());

    cancel.cancel();
    supervisor.shutdown().await;
}

#[cfg(all(test, feature = "clustering"))]
#[tokio::test]
async fn hung_exact_release_worker_is_cancelled_and_joined() {
    use waddle_xmpp::ownership::{Entity, EntityType, NodeIdentity};

    let store = Arc::new(HangingExactReleaseStore {
        inner: waddle_xmpp::ownership::InProcessClaimStore::new(),
        release_started: tokio::sync::Notify::new(),
    });
    let cancel = tokio_util::sync::CancellationToken::new();
    let supervisor = OrphanReaperSupervisor::new(
        Arc::new(waddle_xmpp::stream_management::InMemorySmSessionRegistry::new()),
        cancel.clone(),
    );
    supervisor.workers.enqueue_release(ExactReleaseWork {
        claim_store: store.clone(),
        entity: Entity::new(EntityType::RoomActor, "cancel@muc.example.com"),
        owner: NodeIdentity::new("sweeper", "incarnation"),
        epoch: waddle_xmpp::ownership::ClaimEpoch(1),
        sweep_context: current_sweep_context(),
    });
    tokio::time::timeout(Duration::from_secs(1), store.release_started.notified())
        .await
        .expect("release worker started");
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(1), supervisor.shutdown())
        .await
        .expect("hung exact-release worker must cancel and join");
}

#[cfg(all(test, feature = "clustering"))]
#[tokio::test(flavor = "current_thread")]
async fn exact_release_worker_attempts_link_to_the_enqueuing_sweep() {
    use waddle_xmpp::ownership::{Entity, EntityType, NodeIdentity};

    let store = Arc::new(HangingExactReleaseStore {
        inner: waddle_xmpp::ownership::InProcessClaimStore::new(),
        release_started: tokio::sync::Notify::new(),
    });
    let cancel = tokio_util::sync::CancellationToken::new();
    let supervisor = OrphanReaperSupervisor::new(
        Arc::new(waddle_xmpp::stream_management::InMemorySmSessionRegistry::new()),
        cancel.clone(),
    );
    let spans = waddle_xmpp::telemetry::test_support::acquire_spans();
    let sweep = janitor_sweep_span(Janitor::OrphanReaper);
    let sweep_id = sweep.in_scope(current_sweep_context).span_id();

    assert!(sweep
        .in_scope(|| supervisor.workers.enqueue_release(ExactReleaseWork {
            claim_store: store.clone(),
            entity: Entity::new(EntityType::RoomActor, "traced-release@muc.example.com"),
            owner: NodeIdentity::new("sweeper", "trace-incarnation"),
            epoch: waddle_xmpp::ownership::ClaimEpoch(1),
            sweep_context: current_sweep_context(),
        }))
        .is_accepted());
    drop(sweep);
    tokio::time::timeout(Duration::from_secs(5), store.release_started.notified())
        .await
        .expect("release worker started");
    cancel.cancel();
    supervisor.shutdown().await;

    assert_linked_orphan_work_attempt(
        &spans,
        sweep_id,
        "room_release",
        "orphan.worker.release.test",
    );
}

#[cfg(all(test, feature = "clustering"))]
#[test]
fn hydration_reservations_do_not_overcommit_a_127_of_128_queue() {
    use waddle_xmpp::ownership::{Entity, EntityType, NodeIdentity};
    let (hydration_tx, _hydration_rx) = tokio::sync::mpsc::channel(ORPHAN_WORK_QUEUE_CAPACITY);
    let (release_tx, _release_rx) = tokio::sync::mpsc::channel(ORPHAN_WORK_QUEUE_CAPACITY);
    let workers = OrphanReaperWorkers {
        hydration_tx,
        release_tx,
        hydration_pending: Arc::new(std::sync::Mutex::new(
            (0..127)
                .map(|index| {
                    let entity =
                        Entity::new(EntityType::SmSession, format!("existing-{index}"));
                    (
                        entity.id.clone(),
                        PendingSmHydration::Reserved {
                            entity,
                            attempted_owner: NodeIdentity::new("sweeper", "incarnation"),
                            reservation: waddle_xmpp::stream_management::ReclaimedClaimReservation::from_generation(index + 1),
                        },
                    )
                })
                .collect(),
        )),
        release_pending: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        room_handoff_pending: Arc::new(std::sync::Mutex::new(
            std::collections::HashMap::new(),
        )),
        room_cursor: Arc::new(std::sync::Mutex::new(None)),
        sm_cursor: Arc::new(std::sync::Mutex::new(None)),
        cancel: tokio_util::sync::CancellationToken::new(),
        fatal_fence: tokio_util::sync::CancellationToken::new(),
        node_lifecycle: crate::clustering::NodeLifecycle::new(),
    };
    let reserved = (0..64)
        .filter(|index| {
            workers.reserve_hydration(
                &Entity::new(EntityType::SmSession, format!("candidate-{index}")),
                &NodeIdentity::new("sweeper", "incarnation"),
                waddle_xmpp::stream_management::ReclaimedClaimReservation::from_generation(
                    index + 128,
                ),
            )
        })
        .count();
    assert_eq!(reserved, 1);
}

#[cfg(all(test, feature = "clustering"))]
#[tokio::test]
async fn full_hydration_channel_retains_and_redrives_pending_work() {
    use waddle_xmpp::ownership::{ClaimEpoch, Entity, EntityType, NodeIdentity};
    use waddle_xmpp::stream_management::persistence::SmClaimFence;

    let (hydration_tx, mut hydration_rx) = tokio::sync::mpsc::channel(1);
    let (release_tx, _release_rx) = tokio::sync::mpsc::channel(1);
    let existing = SmHydrationWork {
        entity: Entity::new(EntityType::SmSession, "existing"),
        fence: SmClaimFence::new(NodeIdentity::new("node", "incarnation"), ClaimEpoch(1)),
        reservation: waddle_xmpp::stream_management::ReclaimedClaimReservation::from_generation(1),
        sweep_context: opentelemetry::trace::SpanContext::empty_context(),
    };
    hydration_tx
        .try_send(existing.clone())
        .expect("prefill hydration channel");
    let workers = OrphanReaperWorkers {
        hydration_tx,
        release_tx,
        hydration_pending: Arc::new(std::sync::Mutex::new(
            [("existing".to_string(), PendingSmHydration::Won(existing))].into(),
        )),
        release_pending: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        room_handoff_pending: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        room_cursor: Arc::new(std::sync::Mutex::new(None)),
        sm_cursor: Arc::new(std::sync::Mutex::new(None)),
        cancel: tokio_util::sync::CancellationToken::new(),
        fatal_fence: tokio_util::sync::CancellationToken::new(),
        node_lifecycle: crate::clustering::NodeLifecycle::new(),
    };
    let candidate = Entity::new(EntityType::SmSession, "candidate");

    assert_eq!(
        workers.enqueue_hydration(
            candidate.clone(),
            SmClaimFence::new(NodeIdentity::new("node", "incarnation"), ClaimEpoch(2)),
            waddle_xmpp::stream_management::ReclaimedClaimReservation::from_generation(2),
        ),
        WorkEnqueueOutcome::RetainedForRedrive
    );
    assert!(workers
        .hydration_pending
        .lock()
        .expect("hydration pending")
        .contains_key(&candidate.id));
    assert_eq!(
        hydration_rx
            .recv()
            .await
            .expect("existing hydration")
            .entity
            .id,
        "existing"
    );
    assert_eq!(
        hydration_rx
            .recv()
            .await
            .expect("redriven hydration")
            .entity
            .id,
        candidate.id
    );
}

#[cfg(all(test, feature = "clustering"))]
#[test]
fn closed_hydration_channel_retains_restart_inventory() {
    use waddle_xmpp::ownership::{ClaimEpoch, Entity, EntityType, NodeIdentity};
    use waddle_xmpp::stream_management::persistence::SmClaimFence;

    let (hydration_tx, hydration_rx) = tokio::sync::mpsc::channel(1);
    drop(hydration_rx);
    let (release_tx, _release_rx) = tokio::sync::mpsc::channel(1);
    let workers = OrphanReaperWorkers {
        hydration_tx,
        release_tx,
        hydration_pending: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        release_pending: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        room_handoff_pending: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        room_cursor: Arc::new(std::sync::Mutex::new(None)),
        sm_cursor: Arc::new(std::sync::Mutex::new(None)),
        cancel: tokio_util::sync::CancellationToken::new(),
        fatal_fence: tokio_util::sync::CancellationToken::new(),
        node_lifecycle: crate::clustering::NodeLifecycle::new(),
    };
    let candidate = Entity::new(EntityType::SmSession, "candidate");

    assert_eq!(
        workers.enqueue_hydration(
            candidate.clone(),
            SmClaimFence::new(NodeIdentity::new("node", "incarnation"), ClaimEpoch(2)),
            waddle_xmpp::stream_management::ReclaimedClaimReservation::from_generation(2),
        ),
        WorkEnqueueOutcome::RetainedForRestart
    );
    assert!(workers
        .hydration_pending
        .lock()
        .expect("hydration pending")
        .contains_key(&candidate.id));
}

#[cfg(all(test, feature = "clustering"))]
#[test]
fn exact_release_keys_are_structural_when_components_contain_colons() {
    use waddle_xmpp::ownership::{
        ClaimEpoch, Entity, EntityType, InProcessClaimStore, NodeIdentity,
    };

    let left = ExactReleaseWork {
        claim_store: Arc::new(InProcessClaimStore::new()),
        entity: Entity::new(EntityType::RoomActor, "room:part"),
        owner: NodeIdentity::new("node", "epoch"),
        epoch: ClaimEpoch(1),
        sweep_context: opentelemetry::trace::SpanContext::empty_context(),
    };
    let right = ExactReleaseWork {
        claim_store: Arc::new(InProcessClaimStore::new()),
        entity: Entity::new(EntityType::RoomActor, "room"),
        owner: NodeIdentity::new("part:node", "epoch"),
        epoch: ClaimEpoch(1),
        sweep_context: opentelemetry::trace::SpanContext::empty_context(),
    };

    assert_ne!(
        OrphanReaperWorkers::release_key(&left),
        OrphanReaperWorkers::release_key(&right),
        "field boundaries must remain part of the deduplication identity"
    );
}

#[cfg(all(test, feature = "clustering"))]
#[tokio::test]
async fn full_release_channel_retains_and_redrives_pending_work() {
    use waddle_xmpp::ownership::{
        ClaimEpoch, Entity, EntityType, InProcessClaimStore, NodeIdentity,
    };

    let (hydration_tx, _hydration_rx) = tokio::sync::mpsc::channel(1);
    let (release_tx, mut release_rx) = tokio::sync::mpsc::channel(1);
    let owner = NodeIdentity::new("node", "incarnation");
    let existing = ExactReleaseWork {
        claim_store: Arc::new(InProcessClaimStore::new()),
        entity: Entity::new(EntityType::RoomActor, "existing@muc.example.com"),
        owner: owner.clone(),
        epoch: ClaimEpoch(1),
        sweep_context: opentelemetry::trace::SpanContext::empty_context(),
    };
    release_tx
        .try_send(existing.clone())
        .expect("prefill release channel");
    let existing_key = OrphanReaperWorkers::release_key(&existing);
    let workers = OrphanReaperWorkers {
        hydration_tx,
        release_tx,
        hydration_pending: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        release_pending: Arc::new(std::sync::Mutex::new(
            [(existing_key.clone(), existing)].into(),
        )),
        room_handoff_pending: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        room_cursor: Arc::new(std::sync::Mutex::new(None)),
        sm_cursor: Arc::new(std::sync::Mutex::new(None)),
        cancel: tokio_util::sync::CancellationToken::new(),
        fatal_fence: tokio_util::sync::CancellationToken::new(),
        node_lifecycle: crate::clustering::NodeLifecycle::new(),
    };
    let candidate = ExactReleaseWork {
        claim_store: Arc::new(InProcessClaimStore::new()),
        entity: Entity::new(EntityType::RoomActor, "candidate@muc.example.com"),
        owner,
        epoch: ClaimEpoch(2),
        sweep_context: opentelemetry::trace::SpanContext::empty_context(),
    };
    let candidate_key = OrphanReaperWorkers::release_key(&candidate);

    assert_eq!(
        workers.enqueue_release(candidate),
        WorkEnqueueOutcome::RetainedForRedrive
    );
    assert!(workers
        .release_pending
        .lock()
        .expect("release pending")
        .contains_key(&candidate_key));
    assert_eq!(
        OrphanReaperWorkers::release_key(&release_rx.recv().await.expect("existing release")),
        existing_key
    );
    assert_eq!(
        OrphanReaperWorkers::release_key(&release_rx.recv().await.expect("redriven release")),
        candidate_key
    );
}

#[cfg(all(test, feature = "clustering"))]
#[test]
fn closed_release_channel_retains_restart_inventory() {
    use waddle_xmpp::ownership::{
        ClaimEpoch, Entity, EntityType, InProcessClaimStore, NodeIdentity,
    };

    let (hydration_tx, _hydration_rx) = tokio::sync::mpsc::channel(1);
    let (release_tx, release_rx) = tokio::sync::mpsc::channel(1);
    drop(release_rx);
    let workers = OrphanReaperWorkers {
        hydration_tx,
        release_tx,
        hydration_pending: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        release_pending: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        room_handoff_pending: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        room_cursor: Arc::new(std::sync::Mutex::new(None)),
        sm_cursor: Arc::new(std::sync::Mutex::new(None)),
        cancel: tokio_util::sync::CancellationToken::new(),
        fatal_fence: tokio_util::sync::CancellationToken::new(),
        node_lifecycle: crate::clustering::NodeLifecycle::new(),
    };
    let candidate = ExactReleaseWork {
        claim_store: Arc::new(InProcessClaimStore::new()),
        entity: Entity::new(EntityType::RoomActor, "candidate@muc.example.com"),
        owner: NodeIdentity::new("node", "incarnation"),
        epoch: ClaimEpoch(2),
        sweep_context: opentelemetry::trace::SpanContext::empty_context(),
    };
    let candidate_key = OrphanReaperWorkers::release_key(&candidate);

    assert_eq!(
        workers.enqueue_release(candidate),
        WorkEnqueueOutcome::RetainedForRestart
    );
    assert!(workers
        .release_pending
        .lock()
        .expect("release pending")
        .contains_key(&candidate_key));
}

#[cfg(all(test, feature = "clustering"))]
#[tokio::test]
async fn supervisor_restart_preserves_orphan_scan_cursors() {
    let parent_cancel = tokio_util::sync::CancellationToken::new();
    let supervisor = OrphanReaperSupervisor::new(
        Arc::new(waddle_xmpp::stream_management::InMemorySmSessionRegistry::new()),
        parent_cancel.clone(),
    );
    supervisor.workers.set_sm_cursor(Some(
        crate::clustering::claims::SmOrphanScanCursor::from_raw("sm_session:page-064".to_string()),
    ));
    supervisor.workers.set_room_cursor(Some(
        crate::clustering::claims::RoomOrphanScanCursor::from_raw(
            "room_actor:page-064@muc.example.com".to_string(),
        ),
    ));

    let restarted = supervisor
        .restarted(
            Arc::new(waddle_xmpp::stream_management::InMemorySmSessionRegistry::new()),
            parent_cancel,
        )
        .await;

    assert_eq!(
        restarted
            .workers
            .sm_cursor()
            .as_ref()
            .map(|cursor| cursor.as_raw()),
        Some("sm_session:page-064")
    );
    assert_eq!(
        restarted
            .workers
            .room_cursor()
            .as_ref()
            .map(|cursor| cursor.as_raw()),
        Some("room_actor:page-064@muc.example.com")
    );
    restarted.shutdown().await;
}

#[cfg(all(test, feature = "clustering"))]
#[tokio::test]
async fn supervisor_detects_and_reports_a_panicked_worker() {
    let mut supervisor = OrphanReaperSupervisor::new(
        Arc::new(waddle_xmpp::stream_management::InMemorySmSessionRegistry::new()),
        tokio_util::sync::CancellationToken::new(),
    );
    supervisor.tasks.push((
        "panic-regression",
        tokio::spawn(async { panic!("injected worker panic") }),
    ));
    tokio::task::yield_now().await;
    assert!(!supervisor.is_healthy());
    tokio::time::timeout(Duration::from_secs(1), supervisor.shutdown())
        .await
        .expect("shutdown observes the panicked JoinHandle");
}

#[cfg(all(test, feature = "clustering"))]
#[test]
fn failed_sm_candidate_scan_is_an_empty_batch_not_a_sweep_abort() {
    let candidates = orphaned_sm_candidates_or_empty(Err(
        waddle_xmpp::ownership::ClaimError::Backend("scan failed".to_string()),
    ));
    assert!(candidates.is_empty());

    // The caller continues directly into the RoomActor lane after iterating
    // this empty batch; preserve that control-flow contract explicitly.
    let room_lane_ran = {
        for _ in candidates {}
        true
    };
    assert!(room_lane_ran);
}

#[cfg(feature = "clustering")]
async fn run_orphan_reaper_sweep_with_workers(
    state: &Arc<WebSocketState>,
    workers: &OrphanReaperWorkers,
) -> bool {
    async {
        use waddle_xmpp::ownership::ClaimError;

        let mut sweep_failed = false;

        let clustering = &state.deps.app_state.clustering_claims;
        let Some((claim_store, identity_handle)) = clustering.claim_pair() else {
            return true;
        };
        let Some(node_lease) = clustering.node_lease.clone() else {
            return true;
        };
        let Some(lease_ttl) = clustering.lease_ttl else {
            return true;
        };

        let me = identity_handle.current();
        if !orphan_reaper_self_lease_is_fresh(node_lease.as_ref(), &me, lease_ttl, "start").await {
            return false;
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
                        return false;
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
                    sweep_failed = true;
                    warn!(
                        failed_nodes,
                        candidate_count,
                        "orphan reaper: stale-node watchdog failed to expire some candidates"
                    );
                }
            }
            Err(error) => {
                sweep_failed = true;
                warn!(%error, "orphan reaper: stale-node watchdog candidate scan failed");
            }
        }

        if !orphan_reaper_self_lease_is_fresh(node_lease.as_ref(), &me, lease_ttl, "post-watchdog")
            .await
        {
            return false;
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
                sweep_failed = true;
                debug!(%error, "orphan reaper: current_generation lookup failed; proceeding without backoff");
                std::time::Duration::ZERO
            }
        };

        let room_registry =
            waddle_xmpp::muc::RoomRegistry::wrap(state.deps.protocol.room_registry.clone());
        let mut room_hydrated = 0u64;
        let mut room_released = 0u64;
        let mut room_already_live = 0u64;
        let mut room_pending_retry = 0u64;
        let mut room_lost_race = 0u64;
        let mut room_failed = 0u64;

        // Ordinary destroy/dead-actor/eviction releases use the same orphan
        // reaper cadence, but retry one exact fence per sweep so a slow ownership
        // backend cannot monopolize the registry mailbox.
        if let Err(error) = room_registry.retry_pending_room_releases(1).await {
            sweep_failed = true;
            debug!(%error, "orphan reaper: pending ordinary RoomActor release retry failed");
        }

        // Retry epochs won on an earlier sweep before discovering new work.
        // Each retry is its own bounded registry ask, so a slow store operation
        // cannot monopolize the registry actor as one large batch handler.
        match room_registry
            .list_pending_reclaimed_rooms(ORPHANED_ROOM_CANDIDATE_LIMIT)
            .await
        {
            Ok(pending) => {
                for pending_room in pending {
                    match reconcile_registered_room_or_self_fence(
                        workers,
                        &room_registry,
                        pending_room.room_jid,
                        pending_room.claim_fence,
                        pending_room.previous_owner,
                    )
                    .await
                    {
                        Ok(waddle_xmpp::muc::room_registry_actor::ReclaimedRoomOutcome::Hydrated) => {
                            room_hydrated += 1;
                        }
                        Ok(waddle_xmpp::muc::room_registry_actor::ReclaimedRoomOutcome::Released) => {
                            room_released += 1;
                        }
                        Ok(
                            waddle_xmpp::muc::room_registry_actor::ReclaimedRoomOutcome::AlreadyLive,
                        ) => room_already_live += 1,
                        Ok(
                            waddle_xmpp::muc::room_registry_actor::ReclaimedRoomOutcome::PendingRetry,
                        ) => {
                            room_pending_retry += 1;
                        }
                        Ok(waddle_xmpp::muc::room_registry_actor::ReclaimedRoomOutcome::LostRace) => {
                            room_lost_race += 1;
                        }
                        Err(error) => {
                            debug!(%error, "orphan reaper: pending RoomActor retry ask failed");
                            return false;
                        }
                    }
                }
            }
            Err(error) => {
                debug!(%error, "orphan reaper: pending RoomActor listing failed");
                workers.node_lifecycle.begin_fenced_recovery();
                workers.fatal_fence.cancel();
                return false;
            }
        }
        if let Ok(mut backlog) = room_registry.pending_reclaimed_room_backlog().await {
            if let Ok(releases) = room_registry.pending_room_release_backlog().await {
                backlog.depth = backlog.depth.saturating_add(releases.depth);
                backlog.oldest_age_ms = backlog.oldest_age_ms.max(releases.oldest_age_ms);
            }
            crate::clustering::metrics::record_room_orphan_pending_backlog(
                backlog.depth,
                backlog.oldest_age_ms,
            );
        }

        // RoomActor counterpart: proactively move a bounded set of claims off
        // committed-dead owners. Unlike detached SM sessions, a room claim may
        // have no durable state at all (for example an ephemeral instant room).
        // The serialized registry adoption below therefore either hydrates the
        // exact won epoch, observes demand-side creation already did so, or
        // releases the unusable claim.
        let (room_candidates, room_page_next_cursor, room_page_has_more, room_scan_succeeded) =
            match node_lease
                .list_orphaned_room_actor_claims_page(
                    workers.room_cursor(),
                    ORPHANED_ROOM_CANDIDATE_LIMIT,
                )
                .await
            {
                Ok(page) => {
                    if page.quarantined > 0 {
                        debug!(
                            quarantined = page.quarantined,
                            "orphan reaper: quarantined malformed stale RoomActor claims"
                        );
                    }
                    (page.candidates, page.next_cursor, page.has_more, true)
                }
                Err(error) => {
                    warn!(%error, "orphan reaper: list_orphaned_room_actor_claims_page failed");
                    (Vec::new(), workers.room_cursor(), false, false)
                }
            };
        let mut room_page_processed = true;
        for candidate in room_candidates {
            if workers.cancel.is_cancelled() {
                return false;
            }
            if !workers.has_release_capacity() {
                room_page_processed = false;
                crate::clustering::metrics::record_orphan_work_queue_backpressure("room_release");
                warn!("orphan reaper: pausing RoomActor steals because exact-release cleanup capacity is exhausted");
                break;
            }
            let Ok(room_jid) = candidate.entity.id.parse::<jid::BareJid>() else {
                room_page_processed = false;
                room_failed += 1;
                debug!(
                    entity_id = %candidate.entity.id,
                    "orphan reaper: RoomActor claim id is not a bare JID; leaving stale claim for repair"
                );
                continue;
            };
            match room_registry
                .reserve_pending_reclaimed_room(room_jid.clone())
                .await
            {
                Ok(true) => {}
                Ok(false) => {
                    room_page_processed = false;
                    crate::clustering::metrics::record_orphan_work_queue_backpressure("room_adoption");
                    warn!("orphan reaper: pausing RoomActor steals because adoption capacity is exhausted");
                    break;
                }
                Err(error) => {
                    room_page_processed = false;
                    room_failed += 1;
                    debug!(room = %room_jid, %error, "orphan reaper: room adoption reservation failed");
                    break;
                }
            }
            if !orphan_reaper_self_lease_is_fresh(node_lease.as_ref(), &me, lease_ttl, "pre-room").await
            {
                let _ = room_registry
                    .cancel_pending_reclaimed_room_reservation(room_jid.clone())
                    .await;
                return false;
            }
            if let Err(error) = node_lease.expire(&candidate.owner, lease_ttl).await {
                room_page_processed = false;
                let _ = room_registry
                    .cancel_pending_reclaimed_room_reservation(room_jid.clone())
                    .await;
                room_failed += 1;
                debug!(
                    entity_id = %candidate.entity.id,
                    %error,
                    "orphan reaper: room owner's expire CAS failed"
                );
                continue;
            }
            if !backoff_delay.is_zero() {
                tokio::time::sleep(backoff_delay).await;
            }
            if workers.cancel.is_cancelled() {
                let _ = room_registry
                    .cancel_pending_reclaimed_room_reservation(room_jid.clone())
                    .await;
                return false;
            }
            match node_lease
                .steal_orphaned_room_actor_claim(&candidate.entity, candidate.epoch, &me, lease_ttl)
                .await
            {
                Ok(new_epoch) => {
                    let pending_handoff = waddle_xmpp::muc::room_registry_actor::PendingReclaimedRoom {
                        room_jid: room_jid.clone(),
                        claim_fence: waddle_xmpp::muc::RoomClaimFenceContext::new(
                            candidate.entity.clone(),
                            me.clone(),
                            new_epoch,
                        ),
                        previous_owner: candidate.owner.clone(),
                    };
                    // This must remain the first operation after observing the
                    // steal CAS succeed. It is synchronous, so cancellation of
                    // the outer sweep cannot drop the exact won epoch before
                    // terminal shutdown can transfer it into RoomRegistry.
                    workers.remember_room_handoff(pending_handoff.clone());
                    match register_reclaimed_epoch_or_cleanup(
                        ReclaimedRegistrationContext {
                            workers,
                            room_registry: &room_registry,
                            claim_store: &claim_store,
                            me: &me,
                        },
                        pending_handoff.clone(),
                    )
                    .await
                    {
                        ReclaimedRegistration::Registered => {}
                        ReclaimedRegistration::Released => {
                            room_released += 1;
                            continue;
                        }
                        ReclaimedRegistration::LostRace => {
                            room_lost_race += 1;
                            continue;
                        }
                        ReclaimedRegistration::CleanupScheduled => {
                            room_failed += 1;
                            continue;
                        }
                    }
                    let reconciliation = reconcile_registered_room_or_self_fence(
                        workers,
                        &room_registry,
                        room_jid.clone(),
                        pending_handoff.claim_fence,
                        candidate.owner.clone(),
                    )
                    .await;
                    let notify_previous_owner = !matches!(
                        reconciliation,
                        Ok(waddle_xmpp::muc::room_registry_actor::ReclaimedRoomOutcome::LostRace)
                            | Err(_)
                    );
                    if notify_previous_owner {
                        if let Some(store) = clustering.muc_durable_store.as_ref() {
                            if let Err(error) = store
                                .notify_previous_owner_demoted(
                                    &room_jid,
                                    &candidate.owner.node_id,
                                    &candidate.owner.node_epoch,
                                    new_epoch,
                                )
                                .await
                            {
                                sweep_failed = true;
                                debug!(
                                    room = %room_jid,
                                    %error,
                                    "orphan reaper: best-effort room demotion notification failed"
                                );
                            }
                        }
                    }
                    match reconciliation {
                        Ok(waddle_xmpp::muc::room_registry_actor::ReclaimedRoomOutcome::Hydrated) => {
                            room_hydrated += 1;
                        }
                        Ok(waddle_xmpp::muc::room_registry_actor::ReclaimedRoomOutcome::Released) => {
                            room_released += 1;
                        }
                        Ok(
                            waddle_xmpp::muc::room_registry_actor::ReclaimedRoomOutcome::AlreadyLive,
                        ) => room_already_live += 1,
                        Ok(
                            waddle_xmpp::muc::room_registry_actor::ReclaimedRoomOutcome::PendingRetry,
                        ) => room_pending_retry += 1,
                        Ok(waddle_xmpp::muc::room_registry_actor::ReclaimedRoomOutcome::LostRace) => {
                            room_lost_race += 1;
                        }
                        Err(error) => {
                            debug!(
                                room = %room_jid,
                                %error,
                                "orphan reaper: reclaimed-room registry adoption ask failed; node self-fenced"
                            );
                            return false;
                        }
                    }
                }
                Err(ClaimError::Conflict) => {
                    room_lost_race += 1;
                    if let Err(error) = room_registry
                        .cancel_pending_reclaimed_room_reservation(room_jid.clone())
                        .await
                    {
                        sweep_failed = true;
                        debug!(room = %room_jid, %error, "orphan reaper: failed to cancel lost-race room adoption reservation");
                    }
                }
                Err(error) => {
                    room_page_processed = false;
                    room_failed += 1;
                    let _ = room_registry
                        .cancel_pending_reclaimed_room_reservation(room_jid.clone())
                        .await;
                    debug!(
                        entity_id = %candidate.entity.id,
                        %error,
                        "orphan reaper: RoomActor claim steal failed"
                    );
                }
            }
        }
        if room_scan_succeeded && room_page_processed {
            let committed_cursor = if room_page_has_more {
                room_page_next_cursor
            } else {
                None
            };
            workers.set_room_cursor(committed_cursor.clone());
            if let Err(error) = node_lease
                .persist_orphan_reaper_cursor(
                    crate::clustering::claims::OrphanReaperCursorUpdate::RoomActor(committed_cursor),
                )
                .await
            {
                sweep_failed = true;
                warn!(%error, "orphan reaper: failed to persist RoomActor scan cursor");
            }
        }
        if !room_scan_succeeded || room_failed > 0 {
            sweep_failed = true;
        }
        for (outcome, count) in [
            ("hydrated", room_hydrated),
            ("released", room_released),
            ("already_live", room_already_live),
            ("pending_retry", room_pending_retry),
            ("lost_race", room_lost_race),
            ("failed", room_failed),
        ] {
            if count > 0 {
                crate::clustering::metrics::record_room_orphan_reconciliation(outcome, count);
            }
        }
        let room_reconciled = room_hydrated + room_released + room_already_live;
        if room_pending_retry > 0 || room_failed > 0 {
            warn!(
                hydrated = room_hydrated,
                released = room_released,
                already_live = room_already_live,
                pending_retry = room_pending_retry,
                lost_race = room_lost_race,
                failed = room_failed,
                limit = ORPHANED_ROOM_CANDIDATE_LIMIT,
                "orphan reaper: RoomActor reconciliation completed with pending work"
            );
        } else if room_reconciled > 0 {
            info!(
                hydrated = room_hydrated,
                released = room_released,
                already_live = room_already_live,
                pending_retry = room_pending_retry,
                lost_race = room_lost_race,
                failed = room_failed,
                limit = ORPHANED_ROOM_CANDIDATE_LIMIT,
                "orphan reaper: reconciled orphaned RoomActor claims"
            );
        }
        let (page, scan_succeeded) = match tokio::time::timeout(
            ORPHANED_SM_SCAN_TIMEOUT,
            node_lease.list_orphaned_sm_session_claims_page(
                workers.sm_cursor(),
                STALE_NODE_WATCHDOG_CANDIDATE_LIMIT,
            ),
        )
        .await
        {
            Ok(Ok(page)) => (page, true),
            Ok(Err(error)) => {
                debug!(%error, "orphan reaper: SM-session candidate page failed; room lane already completed");
                (
                    crate::clustering::claims::OrphanedSmSessionClaimPage {
                        candidates: Vec::new(),
                        next_cursor: workers.sm_cursor(),
                        has_more: false,
                        quarantined: 0,
                    },
                    false,
                )
            }
            Err(_) => {
                debug!(
                    "orphan reaper: SM-session candidate scan timed out; room lane already completed"
                );
                (
                    crate::clustering::claims::OrphanedSmSessionClaimPage {
                        candidates: Vec::new(),
                        next_cursor: workers.sm_cursor(),
                        has_more: false,
                        quarantined: 0,
                    },
                    false,
                )
            }
        };
        crate::clustering::metrics::record_sm_orphan_candidate_page(
            page.candidates.len(),
            page.has_more,
            page.quarantined,
        );
        let page_has_more = page.has_more;
        let page_next_cursor = page.next_cursor.clone();
        let candidates = page.candidates;
        let mut stolen = 0usize;
        let mut page_processed = true;
        for candidate in candidates {
            let Some(reservation) = state
                .deps
                .protocol
                .sm_session_registry
                .reserve_reclaimed_claim_capacity(&candidate.entity)
            else {
                page_processed = false;
                break;
            };
            if !workers.reserve_hydration(&candidate.entity, &me, reservation) {
                state
                    .deps
                    .protocol
                    .sm_session_registry
                    .cancel_reclaimed_claim_capacity(&candidate.entity, reservation);
                warn!("orphan reaper: pausing SM steals because hydration capacity is exhausted");
                page_processed = false;
                break;
            }
            if !orphan_reaper_self_lease_is_fresh(node_lease.as_ref(), &me, lease_ttl, "pre-candidate")
                .await
            {
                workers.cancel_hydration_reservation(&candidate.entity);
                state
                    .deps
                    .protocol
                    .sm_session_registry
                    .cancel_reclaimed_claim_capacity(&candidate.entity, reservation);
                return false;
            }
            // Element 9's ordering requirement: commit the expire CAS on the
            // dead owner's row FIRST. Idempotent/best-effort — a failure here
            // just means the steal below is also likely to lose (the owner
            // row is not yet committed-expired), retried next sweep.
            if let Err(error) = node_lease.expire(&candidate.owner, lease_ttl).await {
                sweep_failed = true;
                page_processed = false;
                workers.cancel_hydration_reservation(&candidate.entity);
                debug!(
                    entity_id = %candidate.entity.id,
                    %error,
                    "orphan reaper: expire on the dead owner's row failed; retrying next sweep"
                );
                state
                    .deps
                    .protocol
                    .sm_session_registry
                    .cancel_reclaimed_claim_capacity(&candidate.entity, reservation);
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
                workers.cancel_hydration_reservation(&candidate.entity);
                state
                    .deps
                    .protocol
                    .sm_session_registry
                    .cancel_reclaimed_claim_capacity(&candidate.entity, reservation);
                return false;
            }
            match node_lease
                .steal_orphaned_sm_session_claim(&candidate.entity, candidate.epoch, &me, lease_ttl)
                .await
            {
                Ok(new_epoch) => {
                    stolen += 1;
                    match workers.enqueue_reserved_hydration(
                        candidate.entity,
                        waddle_xmpp::stream_management::persistence::SmClaimFence::new(
                            me.clone(),
                            new_epoch,
                        ),
                        reservation,
                    ) {
                        WorkEnqueueOutcome::Enqueued | WorkEnqueueOutcome::AlreadyTracked => {}
                        WorkEnqueueOutcome::RetainedForRestart => debug!("orphan reaper: hydration worker stopped after steal; responsibility retained for supervisor restart"),
                        WorkEnqueueOutcome::RetainedForRedrive => debug!("orphan reaper: hydration channel full after steal; responsibility retained for active redrive"),
                        WorkEnqueueOutcome::Rejected => debug!("orphan reaper: reserved hydration channel rejected work after steal; claim remains fenced until node expiry"),
                    }
                }
                Err(ClaimError::Conflict) => {
                    workers.cancel_hydration_reservation(&candidate.entity);
                    state
                        .deps
                        .protocol
                        .sm_session_registry
                        .cancel_reclaimed_claim_capacity(&candidate.entity, reservation);
                    // Another node (or this same node's own re-registration
                    // reacquisition step, ADR-0017 Phase 3 plan deviation #19)
                    // already reclaimed it, or the "dead" owner actually
                    // renewed concurrently — safe, no-op.
                }
                Err(error) => {
                    sweep_failed = true;
                    page_processed = false;
                    workers.cancel_hydration_reservation(&candidate.entity);
                    state
                        .deps
                        .protocol
                        .sm_session_registry
                        .cancel_reclaimed_claim_capacity(&candidate.entity, reservation);
                    warn!(
                        entity_id = %candidate.entity.id,
                        %error,
                        "orphan reaper: steal_orphaned_sm_session_claim failed"
                    );
                }
            }
        }
        if scan_succeeded && page_processed {
            let committed_cursor = if page_has_more {
                page_next_cursor
            } else {
                None
            };
            workers.set_sm_cursor(committed_cursor.clone());
            if let Err(error) = node_lease
                .persist_orphan_reaper_cursor(
                    crate::clustering::claims::OrphanReaperCursorUpdate::SmSession(committed_cursor),
                )
                .await
            {
                sweep_failed = true;
                warn!(%error, "orphan reaper: failed to persist SM-session scan cursor");
            }
        }
        if !scan_succeeded {
            sweep_failed = true;
        }
        if stolen > 0 {
            info!(
                stolen,
                "orphan reaper: reclaimed orphaned SM-session claims"
            );
        }

        !sweep_failed
    }
    .instrument(janitor_sweep_span(Janitor::OrphanReaper))
    .await
}

#[cfg(all(test, feature = "clustering"))]
async fn run_orphan_reaper_sweep(state: &Arc<WebSocketState>) {
    let supervisor = OrphanReaperSupervisor::new(
        state.deps.protocol.sm_session_registry.clone(),
        tokio_util::sync::CancellationToken::new(),
    );
    run_orphan_reaper_sweep_with_workers(state, &supervisor.workers).await;
    let workers = supervisor.workers.clone();
    let _ = tokio::time::timeout(Duration::from_secs(10), async move {
        loop {
            let hydration_empty = workers
                .hydration_pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_empty();
            let release_empty = workers
                .release_pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_empty();
            if hydration_empty && release_empty {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    supervisor.shutdown().await;
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
    use crate::muc_durable::PostgresMucRoomStore;
    use crate::server::routes::websocket::tests::create_test_websocket_state_with_clustering;
    use crate::sm_persistence_fenced::PostgresFencedSmPersistence;
    use chrono::{TimeZone, Utc};
    use waddle_xmpp::muc::room_actor::GetConfig;
    use waddle_xmpp::muc::room_registry_actor::WireClusteringClaims;
    use waddle_xmpp::muc::{MucDurableStore, RoomConfig, RoomRegistry};
    use waddle_xmpp::ownership::{
        ClaimStore, Entity, EntityType, NodeIdentity, SharedNodeIdentity,
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
        let muc_store = Arc::new(
            PostgresMucRoomStore::open(
                db.clone(),
                tokio_util::sync::CancellationToken::new(),
                sweeper_identity_handle.clone(),
            )
            .await
            .expect("open fenced MUC durable store"),
        );
        {
            let conn = db.guard().await.expect("guard");
            for statement in [
                "DELETE FROM clustering_muc_room_affiliations",
                "DELETE FROM clustering_muc_rooms",
            ] {
                conn.execute(statement, ())
                    .await
                    .expect("clean MUC durable table");
            }
        }
        let muc_store_dyn: Arc<dyn MucDurableStore> = muc_store.clone();

        let clustering = ClusteringHandles {
            claim_store: Some(Arc::clone(&sweeper_claim_store)),
            node_identity: Some(sweeper_identity_handle.clone()),
            local_claims: None,
            room_local_claims: None,
            user_local_claims: None,
            muc_durable_store: Some(Arc::clone(&muc_store_dyn)),
            node_lease: Some(sweeper_node_lease),
            lease_ttl: Some(lease_ttl),
            pod_template_hash: None,
            resume_bridge: None,
            ordered_relay_delivery_bridge: None,
            stop_token: None,
            fatal_fence: None,
            resume_handshake_timeout: None,
        };

        let state = create_test_websocket_state_with_clustering(
            clustering,
            Arc::clone(&sm_session_registry),
        )
        .await;
        state
            .deps
            .protocol
            .room_registry
            .tell(WireClusteringClaims {
                claim_store: Arc::clone(&sweeper_claim_store),
                node_identity: sweeper_identity_handle.clone(),
                durable_store: Some(muc_store_dyn),
                rollout_backoff: None,
            })
            .await
            .expect("wire test room registry to clustering stores");

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
        let reclaimed = claim_store
            .current_claim(&entity)
            .await
            .expect("claim lookup")
            .expect("reclaimed claim remains present");
        assert!(
            reclaimed.owner == sweeper_identity && reclaimed.claim_epoch.0 > orphan_epoch.0,
            "steal must land under the sweeping node at a fresh monotonic generation"
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
        let reclaimed_once = claim_store
            .current_claim(&entity_2)
            .await
            .expect("claim lookup")
            .expect("reclaimed claim remains present");
        assert!(
            reclaimed_once.owner == sweeper_identity
                && reclaimed_once.claim_epoch.0 > orphan_epoch_2.0,
            "exactly one concurrent sweep must win the observed-epoch CAS and leave the \
             claim under the live sweeper at a fresh monotonic generation"
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

        // ============ Leg 3: durable RoomActor hydrate + ephemeral release ==========
        let dead_room_owner = node_identity();
        register_and_backdate_dead_owner(&claim_store, &db, &dead_room_owner).await;
        let durable_room: jid::BareJid =
            "durable-orphan@muc.example.com".parse().expect("room JID");
        let durable_room_entity = Entity::new(EntityType::RoomActor, durable_room.to_string());
        let durable_room_epoch = claim_store
            .acquire(&durable_room_entity, &dead_room_owner)
            .await
            .expect("dead node owns durable room claim");
        let restored_config = RoomConfig {
            name: "restored by room orphan reaper".to_string(),
            persistent: true,
            ..RoomConfig::default()
        };
        {
            let conn = db.guard().await.expect("guard");
            conn.execute(
                r#"
                INSERT INTO clustering_muc_rooms
                    (room_jid, waddle_id, channel_id, config_json, subject_json)
                VALUES (?, ?, ?, ?, NULL)
                ON CONFLICT (room_jid) DO UPDATE SET
                    waddle_id = EXCLUDED.waddle_id,
                    channel_id = EXCLUDED.channel_id,
                    config_json = EXCLUDED.config_json,
                    subject_json = NULL
                "#,
                crate::db_params![
                    durable_room.to_string(),
                    "w-reclaimed".to_string(),
                    "c-reclaimed".to_string(),
                    serde_json::to_string(&restored_config).expect("serialize config"),
                ],
            )
            .await
            .expect("seed durable room state");
        }

        let ephemeral_room: jid::BareJid = "ephemeral-orphan@muc.example.com"
            .parse()
            .expect("room JID");
        let ephemeral_room_entity = Entity::new(EntityType::RoomActor, ephemeral_room.to_string());
        claim_store
            .acquire(&ephemeral_room_entity, &dead_room_owner)
            .await
            .expect("dead node owns ephemeral room claim");

        run_orphan_reaper_sweep(&state).await;

        let durable_room_claim = claim_store
            .current_claim(&durable_room_entity)
            .await
            .expect("durable room claim lookup")
            .expect("durable room remains claimed after recovery");
        assert_eq!(durable_room_claim.owner, sweeper_identity);
        assert_ne!(durable_room_claim.claim_epoch, durable_room_epoch);
        let room_registry = RoomRegistry::wrap(state.deps.protocol.room_registry.clone());
        let restored_actor = room_registry
            .get_room(durable_room.clone())
            .await
            .expect("registry lookup")
            .expect("durable room proactively hydrated");
        assert_eq!(
            restored_actor.ask(GetConfig).await.expect("config").name,
            restored_config.name
        );
        assert!(
            claim_store
                .current_claim(&ephemeral_room_entity)
                .await
                .expect("ephemeral claim lookup")
                .is_none(),
            "a room with no durable state must be released for demand-side recreation"
        );
        assert!(
            room_registry
                .get_room(ephemeral_room)
                .await
                .expect("registry lookup")
                .is_none(),
            "the reaper must not invent a RoomActor with default state"
        );

        // ============ Leg 4: a heartbeat-stale sweeper cannot steal or hydrate ============
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

        let unchanged = claim_store
            .current_claim(&entity_3)
            .await
            .expect("claim lookup")
            .expect("dead-owner claim remains present");
        assert_eq!(unchanged.owner, dead_owner_3);
        assert_eq!(unchanged.claim_epoch, orphan_epoch_3);
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
            run_pending_delivery_claim_sweep(&state, interval_secs).await;
        }
    });
}

async fn run_pending_delivery_claim_sweep(state: &WebSocketState, interval_secs: u64) {
    async {
        let mut sweep_failed = false;
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
                waddle_xmpp::telemetry::reliability::record_janitor_sweep(
                    Janitor::PendingDeliveryClaim,
                    SweepOutcome::Failed,
                );
                return;
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
                sweep_failed = true;
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
                            sweep_failed = true;
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
                waddle_xmpp::telemetry::reliability::add_pending_delivery_orphan_claims_released(
                    released,
                );
            }
            Ok(_) => {}
            Err(error) => {
                sweep_failed = true;
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
                waddle_xmpp::telemetry::reliability::add_pending_delivery_aged_out(removed);
                info!(
                    removed,
                    cutoff = %cutoff,
                    max_age_days,
                    "pending_delivery aging janitor: dropped expired rows"
                );
            }
            Err(error) => {
                sweep_failed = true;
                warn!(
                    %error,
                    "pending_delivery aging janitor: delete_older_than failed; \
                     will retry on next sweep"
                );
            }
        }
        waddle_xmpp::telemetry::reliability::record_janitor_sweep(
            Janitor::PendingDeliveryClaim,
            if sweep_failed {
                SweepOutcome::Failed
            } else {
                SweepOutcome::Completed
            },
        );
    }
    .instrument(janitor_sweep_span(Janitor::PendingDeliveryClaim))
    .await;
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
            run_push_service_publish_job_sweep(&state, batch_size).await;
        }
    });
}

async fn run_push_service_publish_job_sweep(state: &WebSocketState, batch_size: usize) {
    async {
        let outcome = match state
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
                SweepOutcome::Completed
            }
            Ok(_) => SweepOutcome::Completed,
            Err(error) => {
                warn!(
                    error = %error,
                    "Push Service publish-job janitor failed; queued jobs remain durable"
                );
                SweepOutcome::Failed
            }
        };
        waddle_xmpp::telemetry::reliability::record_janitor_sweep(Janitor::PushPublishJob, outcome);
    }
    .instrument(janitor_sweep_span(Janitor::PushPublishJob))
    .await;
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
            run_notification_outbox_sweep(&state, batch_size, retention_days, prune_batch_size)
                .await;
        }
    });
}

async fn run_notification_outbox_sweep(
    state: &WebSocketState,
    batch_size: usize,
    retention_days: u32,
    prune_batch_size: usize,
) {
    async {
        let mut sweep_failed = false;
        let first_party_service_jid = match state.deps.service_domains.push.parse() {
            Ok(jid) => jid,
            Err(error) => {
                warn!(
                    error = %error,
                    push_service = %state.deps.service_domains.push,
                    "Notification outbox janitor cannot parse first-party Push Service JID"
                );
                waddle_xmpp::telemetry::reliability::record_janitor_sweep(
                    Janitor::NotificationOutbox,
                    SweepOutcome::Failed,
                );
                return;
            }
        };
        let recovered =
            routes::interpret::reconcile_xep0357_notification_candidates_for_sweep(
                state, batch_size,
            )
            .await;
        sweep_failed |= recovered.had_failure;
        if recovered.completed > 0 {
            debug!(
                recovered = recovered.completed,
                "Notification outbox janitor recovered XEP-0357 candidates from pending_delivery"
            );
        }
        let recovered_groupchat =
            routes::interpret::reconcile_groupchat_notification_candidates_for_sweep(
                state, batch_size,
            )
            .await;
        sweep_failed |= recovered_groupchat.had_failure;
        if recovered_groupchat.completed > 0 {
            debug!(
                recovered = recovered_groupchat.completed,
                "Notification outbox janitor recovered XEP-0357 groupchat candidates from inbox projections"
            );
        }
        let room_policy = RoomRegistryActorPolicy::new(state.deps.protocol.room_registry.clone());
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
                sweep_failed = true;
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
                sweep_failed = true;
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
                sweep_failed = true;
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
                sweep_failed = true;
                warn!(
                    error = %error,
                    "Notification outbox janitor failed to prune completed groupchat notification recovery rows"
                );
            }
        }
        waddle_xmpp::telemetry::reliability::record_janitor_sweep(
            Janitor::NotificationOutbox,
            if sweep_failed {
                SweepOutcome::Failed
            } else {
                SweepOutcome::Completed
            },
        );
    }
    .instrument(janitor_sweep_span(Janitor::NotificationOutbox))
    .await;
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
                waddle_xmpp::telemetry::reliability::increment_sm_drain_timeout();
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
            let release_retry_budget =
                drain_deadline.saturating_duration_since(std::time::Instant::now());
            if tokio::time::timeout(
                release_retry_budget,
                websocket_state
                    .deps
                    .protocol
                    .sm_session_registry
                    .retry_pending_claim_releases(64),
            )
            .await
            .is_err()
            {
                // Re-enter at the deadline check so timeout telemetry and
                // abandonment accounting stay centralized above.
                continue;
            }
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
            let mut promotion_batch = crate::sm_promotion::PromotionBatchGuard::new(
                &websocket_state.deps.protocol.sm_session_registry,
                drained,
            );
            while let Some(session) = promotion_batch.pop() {
                let mut promotion_guard = crate::sm_promotion::PromotionSessionGuard::new(
                    &websocket_state.deps.protocol.sm_session_registry,
                    session,
                );
                let session = promotion_guard.session();
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
                let mut recent_tombstones = Vec::new();
                if let Ok(records) = crate::sm_promotion::recent_tombstones_for_promotion(
                    &websocket_state.deps.protocol.sm_session_registry,
                    "Graceful shutdown",
                ) {
                    recent_tombstones = records;
                }
                let summary = crate::sm_promotion::promote_session_unacked(
                    session,
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
                let _ =
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
                    if crate::sm_promotion::prune_promoted_then_reinsert_for_retry(
                        &websocket_state.deps.protocol.sm_session_registry,
                        session.clone(),
                        &summary,
                    )
                    .await
                    {
                        promotion_guard.complete();
                    }
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
                if !confirmed {
                    warn!(
                        jid = %session.jid,
                        stream_id = %session.stream_id,
                        "Graceful shutdown: durable SM confirmation failed; retaining \
                         promotion ownership and pending-delivery claim for retry"
                    );
                    if crate::sm_promotion::prune_promoted_then_reinsert_for_retry(
                        &websocket_state.deps.protocol.sm_session_registry,
                        session.clone(),
                        &summary,
                    )
                    .await
                    {
                        promotion_guard.complete();
                    }
                    continue;
                }
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
                promotion_guard.complete();
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

/// Sweep expired entries from the durable auth handshake tables
/// (`pending_auth`, `device_auth`, `xmpp_auth_codes`).
///
/// These tables grow on every started OAuth / device / XMPP-OAuth flow
/// and are removed only on the success path. Abandoned flows (network
/// flake, tab close, user typo) leave entries behind. The OAuth specs
/// already bound the validity window (10 minutes for
/// `PendingAuthorization` / `XmppAuthCode`, `device_auth.expires_at`
/// for `DeviceAuthorization`); each row carries that deadline as
/// `expires_at_ms`, so the sweep is a plain SQL delete and any replica
/// may run it (see `AuthHandshakeStore::sweep_expired`).
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
            run_auth_state_sweep(&state).await;
        }
    });
}

async fn run_auth_state_sweep(state: &WebSocketState) {
    async {
        let counts = match state
            .deps
            .auth_state
            .auth_handshake
            .sweep_expired(chrono::Utc::now())
            .await
        {
            Ok(counts) => counts,
            Err(error) => {
                warn!(error = %error, "auth janitor: sweep failed; will retry next tick");
                waddle_xmpp::telemetry::reliability::record_janitor_sweep(
                    Janitor::AuthState,
                    SweepOutcome::Failed,
                );
                return;
            }
        };
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
        waddle_xmpp::telemetry::reliability::record_janitor_sweep(
            Janitor::AuthState,
            SweepOutcome::Completed,
        );
    }
    .instrument(janitor_sweep_span(Janitor::AuthState))
    .await;
}

/// Per-sweep counts returned by [`sweep_dormant_rooms_once`].
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct DormancySweepCounts {
    pub evicted: usize,
    pub examined: usize,
    pub remaining: usize,
    failed: bool,
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
            counts.failed = true;
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
                counts.failed = true;
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
                counts.failed = true;
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
                counts.failed = true;
                warn!(
                    room = %room_jid,
                    error = ?error,
                    "room dormancy janitor: DestroyRoomIfInactive ask failed; will retry next pass"
                );
            }
        }
    }
    counts.remaining = match websocket_state
        .deps
        .protocol
        .room_registry
        .ask(RoomCount)
        .await
    {
        Ok(remaining) => remaining,
        Err(error) => {
            counts.failed = true;
            warn!(error = ?error, "room dormancy janitor: RoomCount ask failed");
            0
        }
    };
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
            run_room_dormancy_sweep(&state).await;
        }
    });
}

async fn run_room_dormancy_sweep(state: &WebSocketState) {
    async {
        let counts = sweep_dormant_rooms_once(state).await;
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
        waddle_xmpp::telemetry::reliability::record_janitor_sweep(
            Janitor::RoomDormancy,
            if counts.failed {
                SweepOutcome::Failed
            } else {
                SweepOutcome::Completed
            },
        );
    }
    .instrument(janitor_sweep_span(Janitor::RoomDormancy))
    .await;
}

/// Per-sweep counts returned by [`sweep_empty_user_actors_once`].
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct UserReaperSweepCounts {
    pub reaped: usize,
    pub examined: usize,
    pub remaining: usize,
    failed: bool,
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
            counts.failed = true;
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
                waddle_xmpp::telemetry::reliability::increment_user_actor_reaped();
                debug!(jid = %bare_jid, "user actor reaper: reaped empty UserActor");
            }
            Ok(false) => {}
            Err(error) => {
                counts.failed = true;
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
            counts.failed = true;
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
            run_user_actor_reaper_sweep(&state).await;
        }
    });
}

async fn run_user_actor_reaper_sweep(state: &WebSocketState) {
    async {
        let counts = sweep_empty_user_actors_once(state).await;
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
        waddle_xmpp::telemetry::reliability::record_janitor_sweep(
            Janitor::UserActorReaper,
            if counts.failed {
                SweepOutcome::Failed
            } else {
                SweepOutcome::Completed
            },
        );
    }
    .instrument(janitor_sweep_span(Janitor::UserActorReaper))
    .await;
}

#[cfg(test)]
mod room_dormancy_tests {
    use super::{run_room_dormancy_sweep, sweep_dormant_rooms_once};
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
    async fn zero_work_room_dormancy_sweep_records_completed_heartbeat() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let state = create_test_websocket_state().await;
        run_room_dormancy_sweep(&state).await;

        assert_eq!(
            metrics.counter_sum(
                "waddle.janitor.sweeps",
                &[("janitor", "room_dormancy"), ("outcome", "completed")],
            ),
            Some(1),
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn room_dormancy_sweep_records_the_janitor_span() {
        let _metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let spans = waddle_xmpp::telemetry::test_support::acquire_spans();
        let state = create_test_websocket_state().await;

        run_room_dormancy_sweep(&state).await;

        assert_eq!(
            spans.recorded_field("janitor.sweep", "janitor").as_deref(),
            Some("room_dormancy"),
        );
    }

    /// #1483: `parent: None` is the load-bearing property — a sweep span
    /// that inherited an active local span could be dropped along with it
    /// by the #1438 sampler. Pin that the production constructor starts a
    /// fresh root even when a span is active.
    #[tokio::test(flavor = "current_thread")]
    async fn janitor_sweep_span_is_a_root_even_inside_an_active_span() {
        let spans = waddle_xmpp::telemetry::test_support::acquire_spans();
        let outer = tracing::info_span!("actor.handle_message");
        let sweep = outer.in_scope(|| super::janitor_sweep_span(super::Janitor::RoomDormancy));
        drop(sweep);
        drop(outer);

        let exported = spans.exported();
        let sweep = exported
            .iter()
            .find(|span| span.name == "janitor.sweep")
            .expect("sweep span must export");
        assert_eq!(
            sweep.parent_span_id,
            opentelemetry::trace::SpanId::INVALID,
            "the sweep span must root a fresh trace, not inherit the \
             active span as its parent"
        );
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
    use super::{run_user_actor_reaper_sweep, sweep_empty_user_actors_once};
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

    #[tokio::test(flavor = "current_thread")]
    async fn user_actor_reaper_sweep_records_the_janitor_span() {
        let _metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let spans = waddle_xmpp::telemetry::test_support::acquire_spans();
        let state = create_test_websocket_state().await;

        run_user_actor_reaper_sweep(&state).await;

        assert_eq!(
            spans.recorded_field("janitor.sweep", "janitor").as_deref(),
            Some("user_actor_reaper"),
        );
    }

    /// #1483 acceptance, production path: the registry asks a real sweep
    /// makes must export `actor.handle_message` spans parented under the
    /// exported `janitor.sweep` root — the property the sampler needs to
    /// keep sweep work traceable. This is the test that fails if the
    /// sweep body escapes the instrumented scope (e.g. a dropped
    /// `.instrument` or a wrapper around the wrong future).
    #[tokio::test(flavor = "current_thread")]
    async fn user_actor_reaper_actor_children_are_parented_under_the_sweep_root() {
        let _metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let spans = waddle_xmpp::telemetry::test_support::acquire_spans();
        let state = create_test_websocket_state().await;

        run_user_actor_reaper_sweep(&state).await;

        let exported = spans.exported();
        let root = exported
            .iter()
            .find(|span| span.name == "janitor.sweep")
            .expect("the sweep root must export once the sweep completes");
        assert!(
            exported
                .iter()
                .any(|span| span.name == "actor.handle_message"
                    && span.parent_span_id == root.span_context.span_id()),
            "the sweep's registry asks must export actor.handle_message \
             spans parented under the janitor.sweep root; exported: {:?}",
            exported
                .iter()
                .map(|span| span.name.as_ref())
                .collect::<Vec<&str>>()
        );
    }

    /// End-to-end sweep over the actor tree: register a resource, force the
    /// production closed-channel eviction so the actor is empty-but-registered,
    /// then assert the sweep reaps it, its accounting is correct, and the
    /// `waddle_user_actor_reaped_total` metric fires.
    #[tokio::test]
    async fn sweep_reaps_orphaned_empty_user_actor() {
        // Hold the metric-reader guard for the whole check: it serialises
        // against other metric-asserting tests and drains prior samples.
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;

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

        assert_eq!(
            metrics.counter_sum("xmpp.user_actor.reaped", &[]),
            Some(1),
            "the reaper must increment the reaped counter"
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

#[cfg(feature = "clustering")]
fn remote_muc_sweep_outcome(had_failure: bool) -> SweepOutcome {
    if had_failure {
        SweepOutcome::Failed
    } else {
        SweepOutcome::Completed
    }
}

/// Collect the occupants whose remote MUC memberships need a cleanup
/// re-drive (#1249): an ACTIVE membership entry whose occupant full JID
/// has no live connection-registry entry here and no resumable
/// XEP-0198 session anywhere (this node's memory OR the shared durable
/// store). Such an entry can only be the residue
/// of a failed (or missed) disconnect cleanup — the join path records
/// memberships strictly while the connection is registered, and both
/// graceful-leave and successful cleanup forget them.
///
/// A detached-but-resumable session keeps its occupancy on purpose
/// (XEP-0198 resume re-attaches to the same room state), so it is NOT a
/// candidate; SM expiry runs its own cleanup pass which restores the
/// membership on failure and thereby feeds this janitor.
#[cfg(feature = "clustering")]
struct RemoteMucReconcileCandidates {
    occupants: Vec<jid::FullJid>,
    had_failure: bool,
}

#[cfg(feature = "clustering")]
async fn collect_remote_muc_reconcile_candidates(
    state: &WebSocketState,
) -> RemoteMucReconcileCandidates {
    let mut candidates = Vec::new();
    let mut had_failure = false;
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
        // Cross-node guard (SM review P1 on PR #1277): a session that
        // detached HERE and was resume-stolen by another node leaves no
        // local trace, but its durable row (now owned by the stealing
        // node) proves the occupancy is still legitimately resumable.
        // The probe checks this node's memory AND the shared durable
        // store, and fails closed on read errors.
        match state
            .deps
            .protocol
            .sm_session_registry
            .probe_resumable_session_for_full_jid(&occupant)
            .await
        {
            waddle_xmpp::stream_management::ResumableSessionProbe::Present => continue,
            waddle_xmpp::stream_management::ResumableSessionProbe::Absent => {}
            waddle_xmpp::stream_management::ResumableSessionProbe::Failed => {
                had_failure = true;
                continue;
            }
        }
        candidates.push(occupant);
    }
    RemoteMucReconcileCandidates {
        occupants: candidates,
        had_failure,
    }
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
        // A sweep slower than the interval (serial relays against an
        // unreachable node) must not burst-fire missed ticks into a
        // continuous retry loop (race review P2 on PR #1277).
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let Some(state) = weak_state.upgrade() else {
                break;
            };
            run_remote_muc_membership_sweep(&state).await;
        }
    });
}

#[cfg(feature = "clustering")]
async fn run_remote_muc_membership_sweep(state: &WebSocketState) {
    async {
        let candidates = collect_remote_muc_reconcile_candidates(state).await;
        if candidates.occupants.is_empty() {
            waddle_xmpp::telemetry::reliability::record_janitor_sweep(
                Janitor::RemoteMucMembership,
                remote_muc_sweep_outcome(candidates.had_failure),
            );
            return;
        }
        info!(
            candidates = candidates.occupants.len(),
            "remote MUC reconciler: re-driving unavailable relays for departed occupants"
        );
        let mut sweep_failed = candidates.had_failure;
        for occupant in candidates.occupants {
            // Re-check liveness IMMEDIATELY before the re-drive
            // (codex review P1 on PR #1277): the candidate list was
            // collected before earlier awaited re-drives, and the
            // same full JID may have reconnected in that gap. A
            // registered connection means the occupancy is
            // legitimate again; the membership-generation guards
            // inside the cleanup protect the map but cannot undo a
            // relayed remote leave.
            if state
                .deps
                .protocol
                .connection_registry
                .get_entry(&occupant)
                .is_some()
            {
                continue;
            }
            if routes::websocket::redrive_remote_muc_cleanup(state, &occupant).await
                == routes::websocket::MucCleanupOutcome::Failed
            {
                sweep_failed = true;
            }
        }
        waddle_xmpp::telemetry::reliability::record_janitor_sweep(
            Janitor::RemoteMucMembership,
            remote_muc_sweep_outcome(sweep_failed),
        );
    }
    .instrument(janitor_sweep_span(Janitor::RemoteMucMembership))
    .await;
}

#[cfg(all(test, feature = "clustering"))]
mod remote_muc_reconciler_tests {
    use super::{collect_remote_muc_reconcile_candidates, remote_muc_sweep_outcome};
    use crate::server::routes::websocket::tests::create_test_websocket_state;
    use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
    use waddle_xmpp::telemetry::attributes::SweepOutcome;

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

    #[tokio::test]
    async fn missing_relay_bridge_restores_membership_and_fails_the_sweep() {
        let state = create_test_websocket_state().await;
        let occupant: jid::FullJid = "ghost@example.com/web".parse().expect("occupant");
        state.deps.protocol.remote_muc_memberships.record_join(
            &occupant,
            &room("bridge-missing"),
            "ghost",
        );

        let cleanup =
            crate::server::routes::websocket::redrive_remote_muc_cleanup(&state, &occupant).await;

        assert_eq!(
            cleanup,
            crate::server::routes::websocket::MucCleanupOutcome::Failed
        );
        assert!(state
            .deps
            .protocol
            .remote_muc_memberships
            .occupants_with_active_memberships()
            .contains(&occupant));
        assert_eq!(remote_muc_sweep_outcome(true), SweepOutcome::Failed);
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
        assert!(!candidates.had_failure);
        assert_eq!(
            candidates.occupants,
            vec![ghost],
            "only the fully departed occupant is re-driven"
        );
    }
}
