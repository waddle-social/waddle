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
                        continue;
                    }
                };
                let summary = crate::sm_promotion::promote_session_unacked(
                    &session,
                    &state.deps.protocol.connection_registry,
                    &state.deps.protocol.pending_delivery_storage,
                    &blocklist,
                    state.deps.auth_state.xmpp_domain.as_str(),
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

                if session.presence_available {
                    routes::websocket::handlers::presence::broadcast_unavailable_for_expired_detached_session(
                        &state,
                        &session.jid,
                    )
                    .await;
                }
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
                routes::websocket::cleanup_muc_presence_for_jid(&state, &session.jid).await;
            }
        }
    });
}

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
            match state
                .deps
                .protocol
                .pending_delivery_storage
                .list_orphaned_claims(&live_sessions)
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
                warn!(
                    total_drained,
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
                        continue;
                    }
                };
                let summary = crate::sm_promotion::promote_session_unacked(
                    &session,
                    &websocket_state.deps.protocol.connection_registry,
                    &websocket_state.deps.protocol.pending_delivery_storage,
                    &blocklist,
                    websocket_state.deps.auth_state.xmpp_domain.as_str(),
                )
                .await;
                info!(
                    jid = %session.jid,
                    redelivered = summary.redelivered,
                    queued = summary.queued,
                    bounced = summary.bounced,
                    dropped = summary.dropped,
                    unparseable = summary.unparseable,
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
                    continue;
                }
                websocket_state
                    .deps
                    .protocol
                    .sm_session_registry
                    .confirm_drained(&session.stream_id)
                    .await;
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
    use waddle_xmpp::muc::room_actor::IsDormant;
    use waddle_xmpp::muc::room_registry_actor::{DestroyRoom, ListRooms, RoomCount};
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
        let is_dormant = match actor.ask(IsDormant).await {
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
        if !is_dormant {
            continue;
        }
        match websocket_state
            .deps
            .protocol
            .room_registry
            .ask(DestroyRoom {
                room_jid: room_jid.clone(),
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
                    "room dormancy janitor: DestroyRoom ask failed; will retry next pass"
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
        room_actor::{ChangeAffiliation, Join, LeaveByRealJid},
        room_registry_actor::{CreateRoom, RoomCount},
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
