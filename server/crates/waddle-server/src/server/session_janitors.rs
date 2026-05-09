use crate::server::routes;
use crate::server::routes::websocket::WebSocketState;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

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
                    .list_blocked_jids(&session.jid.to_bare())
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
                if summary.queued + summary.redelivered + summary.bounced > 0
                    || summary.storage_failed > 0
                {
                    info!(
                        jid = %session.jid,
                        redelivered = summary.redelivered,
                        queued = summary.queued,
                        bounced = summary.bounced,
                        dropped = summary.dropped,
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
                state
                    .deps
                    .protocol
                    .connection_registry
                    .unregister(&session.jid);
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
        info!("Graceful shutdown: starting SM session Q6 drain");
        const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);
        const QUIET_WINDOW_PASSES: u32 = 8;
        let drain_deadline = std::time::Instant::now() + max_drain_duration_from_env();
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
                    .list_blocked_jids(&session.jid.to_bare())
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
    });
}
