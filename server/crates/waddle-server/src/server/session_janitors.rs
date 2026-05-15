use crate::server::routes;
use crate::server::routes::websocket::WebSocketState;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, error, info, warn};

/// Default interval for the auth-state TTL janitor.
const AUTH_JANITOR_INTERVAL: Duration = Duration::from_secs(60);

/// Default interval for the persistent-room dormancy janitor.
const ROOM_DORMANCY_JANITOR_INTERVAL: Duration = Duration::from_secs(300);

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
