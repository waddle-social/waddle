#[cfg(feature = "clustering")]
use crate::clustering::claims::OrphanReaperCursorUpdate;
#[cfg(feature = "clustering")]
use crate::clustering::claims::OrphanedSmSessionClaimPage;
#[cfg(feature = "clustering")]
use crate::clustering::claims::RoomOrphanScanCursor;
#[cfg(feature = "clustering")]
use crate::clustering::claims::SmOrphanScanCursor;
#[cfg(feature = "clustering")]
use crate::clustering::metrics::OrphanTerminalCleanupFailureReason;
#[cfg(feature = "clustering")]
use crate::clustering::metrics::OrphanTerminalCleanupLane;
#[cfg(feature = "clustering")]
use crate::clustering::metrics::OrphanWorkQueue;
#[cfg(feature = "clustering")]
use crate::clustering::metrics::OrphanWorker;
#[cfg(feature = "clustering")]
use crate::clustering::metrics::OrphanWorkerFailureReason;
#[cfg(feature = "clustering")]
use crate::clustering::metrics::RoomOrphanReconciliationOutcome;
use crate::clustering::CriticalNodeFailure;
use crate::clustering::NodeLifecycle;
use crate::db::actor::DbExecute;
use crate::metrics::LocalDepartureRetryOutcome;
use crate::notification_outbox::NotificationDrainDeps;
use crate::pending_delivery::PendingRecovery;
use crate::server::dual_registration::MirrorUnregisterOutcome;
#[cfg(feature = "clustering")]
use crate::server::routes::interpret::OrderedRelayRouteOrigin;
#[cfg(feature = "clustering")]
use crate::server::routes::interpret::OrderedRelayRouteOriginKind;
#[cfg(feature = "clustering")]
use crate::server::routes::websocket::MembershipGenerationFilter;
use crate::sm_promotion::PromotionBatchGuard;
use crate::sm_promotion::PromotionScrubOutcome;
use crate::sm_promotion::PromotionSessionGuard;
use crate::sm_promotion::TerminalOverflowPromotionDeps;
use std::collections::HashMap;
use std::collections::HashSet;
#[cfg(feature = "clustering")]
use std::collections::VecDeque;
mod ingress_custody;
pub(crate) use ingress_custody::run_ingress_custody_sweep;
#[cfg(feature = "clustering")]
mod remote_owner;
#[cfg(all(test, feature = "clustering"))]
pub(crate) use remote_owner::remote_owner_mirror_ticker;
#[cfg(feature = "clustering")]
pub(crate) use remote_owner::spawn_remote_owner_mirror_janitor;

use crate::room_policy::RoomRegistryActorPolicy;
use crate::server::routes;
use crate::server::routes::websocket::WebSocketState;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, error, info, warn, Instrument};
use waddle_xmpp::muc::room_actor::LeaveSessionSelector;

use waddle_xmpp::telemetry::attributes::{Janitor, MetricAttribute, SweepOutcome};

/// One SM-expiry sweep pass. `pub(crate)` so regression tests can drive the
/// janitor's retry convergence directly instead of waiting on the interval.
#[cfg(test)]
pub(crate) async fn run_sm_expiry_sweep(state: &Arc<WebSocketState>) {
    run_sm_expiry_sweep_with_custody_cursor(state, &mut None).await;
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

#[cfg(test)]
mod sm_expiry_tests {
    use super::run_sm_expiry_sweep;
    use crate::server::routes::websocket::tests::{
        create_test_websocket_state, register_test_connection, snapshot_room,
    };
    use jid::{BareJid, FullJid};
    use tokio::sync::mpsc;
    use waddle_xmpp::muc::room_actor::{GetSnapshot, JoinAffiliationGrant, JoinWithAffiliation};
    use waddle_xmpp::muc::room_registry_actor::CreateRoom;
    use waddle_xmpp::muc::RoomConfig;
    use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
    use waddle_xmpp::Affiliation;
    use waddle_xmpp_core::OccupancySessionGeneration;

    fn room(local: &str) -> BareJid {
        format!("{local}@muc.example.com")
            .parse()
            .expect("room jid")
    }

    fn full_jid(value: &str) -> FullJid {
        value.parse().expect("full jid")
    }

    fn expired_detached_session(
        stream_id: &str,
        jid: FullJid,
        occupancy_session: OccupancySessionGeneration,
    ) -> DetachedSession {
        DetachedSession {
            promotion_custody:
                waddle_xmpp::stream_management::PromotionCaptureCustody::never_claimed(),
            stream_id: stream_id.to_string(),
            user_id: jid.to_bare().to_string(),
            jid,
            occupancy_session,
            inbound_count: 0,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(0),
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

    async fn join_member_with_generation(
        state: &crate::server::routes::websocket::WebSocketState,
        room_jid: &BareJid,
        jid: &FullJid,
        nick: &str,
        generation: OccupancySessionGeneration,
    ) {
        let actor = match state
            .deps
            .protocol
            .room_registry
            .ask(waddle_xmpp::muc::room_registry_actor::GetRoom {
                room_jid: room_jid.clone(),
            })
            .await
            .expect("get room")
        {
            Some(actor) => actor,
            None => state
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
                .expect("create room"),
        };
        let admission_revision = actor
            .ask(GetSnapshot)
            .await
            .expect("snapshot before join")
            .admission_revision;
        actor
            .ask(JoinWithAffiliation {
                sender_jid: jid.clone(),
                nick: nick.to_string(),
                affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
                local_domain: "example.com".to_string(),
                admission_revision,
                session: generation,
            })
            .await
            .expect("join occupant");
    }

    #[tokio::test]
    async fn retirement_timeout_does_not_skip_unattempted_page_suffix() {
        let state = create_test_websocket_state().await;
        let first = waddle_xmpp::pending_delivery::SmSessionId::new("a-blocked-retirement");
        let second = waddle_xmpp::pending_delivery::SmSessionId::new("b-ready-retirement");
        for stream in [&first, &second] {
            state
                .deps
                .protocol
                .ingress
                .enroll_stream(stream)
                .await
                .expect("enroll");
        }
        let stalled = state.deps.protocol.ingress.block_test_stream(&first).await;
        assert!(!super::run_ingress_retirement_sweep(&state).await);
        // The first retirement still cannot progress, but the untouched suffix can.
        assert!(super::run_ingress_retirement_sweep(&state).await);
        assert!(state
            .deps
            .protocol
            .ingress
            .lookup_stream(&second)
            .await
            .expect("second retired")
            .is_none());
        assert!(state
            .deps
            .protocol
            .ingress
            .lookup_stream(&first)
            .await
            .expect("first retained")
            .is_some());
        drop(stalled);
        assert!(super::run_ingress_retirement_sweep(&state).await);
        assert!(state
            .deps
            .protocol
            .ingress
            .lookup_stream(&first)
            .await
            .expect("wrapped retry retired")
            .is_none());
    }

    #[tokio::test]
    async fn orphan_scan_preserves_persisted_sessions_missing_from_local_restore() {
        use waddle_xmpp::stream_management::persistence::{
            InMemorySmPersistence, SmPersistenceStorage,
        };
        use waddle_xmpp::stream_management::InMemorySmSessionRegistry;
        let persistence = std::sync::Arc::new(InMemorySmPersistence::new());
        let original = InMemorySmSessionRegistry::new().with_persistence(persistence.clone());
        let stream = waddle_xmpp::pending_delivery::SmSessionId::new("unhydrated-ingress");
        original
            .store_session(expired_detached_session(
                stream.as_str(),
                full_jid("alice@example.com/web"),
                OccupancySessionGeneration::mint(),
            ))
            .await
            .expect("persist detached session");
        let unhydrated = std::sync::Arc::new(
            InMemorySmSessionRegistry::new().with_persistence(persistence.clone()),
        );
        let state =
            crate::server::routes::websocket::tests::create_test_websocket_state_with_sm_registry(
                unhydrated,
            )
            .await;
        state
            .deps
            .protocol
            .ingress
            .enroll_stream(&stream)
            .await
            .expect("enroll");
        assert!(super::run_ingress_retirement_sweep(&state).await);
        assert!(state
            .deps
            .protocol
            .ingress
            .lookup_stream(&stream)
            .await
            .expect("persisted stream protected")
            .is_some());
        persistence
            .delete_session(&stream)
            .await
            .expect("terminal durable cleanup");
        assert!(super::run_ingress_retirement_sweep(&state).await);
        assert!(state
            .deps
            .protocol
            .ingress
            .lookup_stream(&stream)
            .await
            .expect("retired stream")
            .is_none());
    }

    #[tokio::test]
    async fn orphan_scan_preserves_live_enrollment_then_retires_released_claim() {
        let state = create_test_websocket_state().await;
        let stream = waddle_xmpp::pending_delivery::SmSessionId::new("orphan-enrollment");
        let registry = &state.deps.protocol.sm_session_registry;
        let publication = registry
            .ensure_session_claim(stream.as_str())
            .await
            .expect("acquire enable claim");
        state
            .deps
            .protocol
            .ingress
            .enroll_stream(&stream)
            .await
            .expect("enroll");
        assert!(super::run_ingress_retirement_sweep(&state).await);
        assert!(state
            .deps
            .protocol
            .ingress
            .lookup_stream(&stream)
            .await
            .expect("live stream")
            .is_some());
        drop(publication);
        assert!(registry.defer_unpublished_enabled_claim_release(stream.as_str()));
        assert!(super::run_ingress_retirement_sweep(&state).await);
        assert!(state
            .deps
            .protocol
            .ingress
            .lookup_stream(&stream)
            .await
            .expect("claim release pending")
            .is_some());
        registry.retry_pending_claim_releases(64).await;
        assert!(super::run_ingress_retirement_sweep(&state).await);
        assert!(state
            .deps
            .protocol
            .ingress
            .lookup_stream(&stream)
            .await
            .expect("retired stream")
            .is_none());
    }

    #[tokio::test]
    async fn orphan_scan_retries_failed_retirement_from_durable_stream_row() {
        let state = create_test_websocket_state().await;
        let stream = waddle_xmpp::pending_delivery::SmSessionId::new("retirement-retry");
        state
            .deps
            .protocol
            .ingress
            .enroll_stream(&stream)
            .await
            .expect("enroll");
        let db = state.deps.app_state.db_pool.global();
        db.execute("CREATE TRIGGER block_ingress_retirement BEFORE DELETE ON ingress_sm_streams BEGIN SELECT RAISE(ABORT, 'retirement test failure'); END").await.expect("block retirement");
        assert!(!super::run_ingress_retirement_sweep(&state).await);
        assert!(state
            .deps
            .protocol
            .ingress
            .lookup_stream(&stream)
            .await
            .expect("retained retry row")
            .is_some());
        db.execute("DROP TRIGGER block_ingress_retirement")
            .await
            .expect("unblock retirement");
        assert!(super::run_ingress_retirement_sweep(&state).await);
        assert!(state
            .deps
            .protocol
            .ingress
            .lookup_stream(&stream)
            .await
            .expect("retired on retry")
            .is_none());
    }

    #[tokio::test]
    async fn expired_session_retires_ingress_after_successful_confirmation() {
        let state = create_test_websocket_state().await;
        let stream = waddle_xmpp::pending_delivery::SmSessionId::new("expiry-ingress");
        state
            .deps
            .protocol
            .ingress
            .enroll_stream(&stream)
            .await
            .expect("enroll");
        state
            .deps
            .protocol
            .sm_session_registry
            .store_session(expired_detached_session(
                stream.as_str(),
                full_jid("alice@example.com/web"),
                OccupancySessionGeneration::mint(),
            ))
            .await
            .expect("store expired session");
        run_sm_expiry_sweep(&state).await;
        assert!(state
            .deps
            .protocol
            .ingress
            .lookup_stream(&stream)
            .await
            .expect("lookup retired stream")
            .is_none());
    }

    #[tokio::test]
    async fn sm_expiry_generation_keeps_a_same_full_jid_replacement_occupancy() {
        let state = create_test_websocket_state().await;
        let room_jid = room("sm-expiry-replacement");
        let jid = full_jid("alice@example.com/web");
        let first_generation = OccupancySessionGeneration::mint();
        let second_generation = OccupancySessionGeneration::mint();

        join_member_with_generation(state.as_ref(), &room_jid, &jid, "alice", first_generation)
            .await;
        let (tx, _rx) = mpsc::channel(4);
        register_test_connection(state.as_ref(), &jid, tx).await;
        join_member_with_generation(state.as_ref(), &room_jid, &jid, "alice", second_generation)
            .await;

        state
            .deps
            .protocol
            .sm_session_registry
            .store_session(expired_detached_session(
                "expired-g1",
                jid.clone(),
                first_generation,
            ))
            .await
            .expect("store detached session");

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        run_sm_expiry_sweep(&state).await;

        let snapshot = snapshot_room(state.as_ref(), &room_jid).await;
        assert!(
            snapshot.room.find_occupant_by_real_jid(&jid).is_some(),
            "the replacement occupant must remain after expiring the detached g1 session"
        );
        assert!(
            state.deps.protocol.connection_registry.is_connected(&jid),
            "the replacement connection must remain registered"
        );
    }

    #[tokio::test]
    async fn sm_expiry_generation_without_replacement_removes_the_old_occupancy() {
        let state = create_test_websocket_state().await;
        let room_jid = room("sm-expiry-no-replacement");
        let jid = full_jid("alice@example.com/web");
        let generation = OccupancySessionGeneration::mint();

        join_member_with_generation(state.as_ref(), &room_jid, &jid, "alice", generation).await;
        state
            .deps
            .protocol
            .sm_session_registry
            .store_session(expired_detached_session(
                "expired-no-replacement",
                jid.clone(),
                generation,
            ))
            .await
            .expect("store detached session");

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        run_sm_expiry_sweep(&state).await;

        let snapshot = snapshot_room(state.as_ref(), &room_jid).await;
        assert!(
            snapshot.room.find_occupant_by_real_jid(&jid).is_none(),
            "without a replacement the expired detached session must be cleaned from the room"
        );
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

#[cfg(all(test, feature = "clustering"))]
#[tokio::test]
async fn stopped_registry_after_steal_exactly_releases_the_won_claim() {
    use waddle_xmpp::ownership::{ClaimStore, Entity, EntityType, InProcessClaimStore};

    let registry = waddle_xmpp::muc::RoomRegistry::spawn(
        "muc.example.com".to_string(),
        waddle_xmpp::xep::xep0421::OccupantIdSecret::try_new(
            b"test-occupant-id-secret-32-bytes-long".to_vec(),
        )
        .expect("secret"),
        waddle_xmpp::xep::xep0317::ServerHats::default(),
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
        waddle_xmpp::xep::xep0421::OccupantIdSecret::try_new(
            b"test-occupant-id-secret-32-bytes-long".to_vec(),
        )
        .expect("secret"),
        waddle_xmpp::xep::xep0317::ServerHats::default(),
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
        waddle_xmpp::xep::xep0421::OccupantIdSecret::try_new(
            b"test-occupant-id-secret-32-bytes-long".to_vec(),
        )
        .expect("secret"),
        waddle_xmpp::xep::xep0317::ServerHats::default(),
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
        waddle_xmpp::xep::xep0421::OccupantIdSecret::try_new(
            b"test-occupant-id-secret-32-bytes-long".to_vec(),
        )
        .expect("secret"),
        waddle_xmpp::xep::xep0317::ServerHats::default(),
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
        waddle_xmpp::xep::xep0421::OccupantIdSecret::try_new(
            b"test-occupant-id-secret-32-bytes-long".to_vec(),
        )
        .expect("secret"),
        waddle_xmpp::xep::xep0317::ServerHats::default(),
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
    allow_reads: std::sync::atomic::AtomicBool,
    delete_failures_remaining: std::sync::atomic::AtomicUsize,
    delete_attempts: std::sync::atomic::AtomicUsize,
    delete_times: std::sync::Mutex<Vec<std::time::Instant>>,
}

#[cfg(all(test, feature = "clustering"))]
#[async_trait::async_trait]
impl waddle_xmpp::stream_management::persistence::SmPersistenceStorage
    for HangingSmReadPersistence
{
    async fn acknowledge_claimed_session(
        &self,
        stream: &waddle_xmpp::pending_delivery::SmSessionId,
        h: u32,
        fence: &waddle_xmpp::stream_management::persistence::SmClaimFence,
        authority: Option<&waddle_xmpp::ownership::CurrentNodeIdentityGuard>,
    ) -> Result<(), waddle_xmpp::stream_management::persistence::SmPersistenceError> {
        self.inner
            .acknowledge_claimed_session(stream, h, fence, authority)
            .await
    }

    async fn pending_acknowledgements(
        &self,
        stream: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<
        Vec<waddle_xmpp::stream_management::persistence::SmAckWindow>,
        waddle_xmpp::stream_management::persistence::SmPersistenceError,
    > {
        self.inner.pending_acknowledgements(stream).await
    }

    async fn confirm_acknowledgement(
        &self,
        stream: &waddle_xmpp::pending_delivery::SmSessionId,
        window: waddle_xmpp::stream_management::persistence::SmAckWindow,
        fence: &waddle_xmpp::stream_management::persistence::SmClaimFence,
        authority: Option<&waddle_xmpp::ownership::CurrentNodeIdentityGuard>,
    ) -> Result<(), waddle_xmpp::stream_management::persistence::SmPersistenceError> {
        self.inner
            .confirm_acknowledgement(stream, window, fence, authority)
            .await
    }

    async fn pending_claim_protected_streams(
        &self,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::SmSessionId>,
        waddle_xmpp::stream_management::persistence::SmPersistenceError,
    > {
        self.inner.pending_claim_protected_streams().await
    }

    async fn mark_session_nonresumable(
        &self,
        stream_id: &waddle_xmpp::pending_delivery::SmSessionId,
        acknowledged_h: u32,
        expected_fence: &waddle_xmpp::stream_management::persistence::SmClaimFence,
        authority: Option<&waddle_xmpp::ownership::CurrentNodeIdentityGuard>,
    ) -> Result<(), waddle_xmpp::stream_management::persistence::SmPersistenceError> {
        self.inner
            .mark_session_nonresumable(stream_id, acknowledged_h, expected_fence, authority)
            .await
    }

    async fn upsert_session(
        &self,
        session: waddle_xmpp::stream_management::persistence::PersistedSession,
    ) -> Result<(), waddle_xmpp::stream_management::persistence::SmPersistenceError> {
        self.inner.upsert_session(session).await
    }

    async fn get_session(
        &self,
        stream_id: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<
        Option<waddle_xmpp::stream_management::persistence::PersistedSession>,
        waddle_xmpp::stream_management::persistence::SmPersistenceError,
    > {
        if self.allow_reads.load(std::sync::atomic::Ordering::SeqCst) {
            return self.inner.get_session(stream_id).await;
        }
        drop(tracing::info_span!("orphan.worker.hydration.test"));
        self.read_started.notify_one();
        std::future::pending().await
    }

    async fn delete_session(
        &self,
        stream_id: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<(), waddle_xmpp::stream_management::persistence::SmPersistenceError> {
        self.delete_attempts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.delete_times
            .lock()
            .expect("record delete attempt")
            .push(std::time::Instant::now());
        if self
            .delete_failures_remaining
            .fetch_update(
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
                |remaining| remaining.checked_sub(1),
            )
            .is_ok()
        {
            return Err(
                waddle_xmpp::stream_management::persistence::SmPersistenceError::Other(
                    "injected transient delete failure".to_string(),
                ),
            );
        }
        self.inner.delete_session(stream_id).await
    }

    async fn append_unacked(
        &self,
        stanza: waddle_xmpp::stream_management::persistence::PersistedUnackedStanza,
    ) -> Result<(), waddle_xmpp::stream_management::persistence::SmPersistenceError> {
        self.inner.append_unacked(stanza).await
    }

    async fn append_promotion_stanza(
        &self,
        stanza: waddle_xmpp::stream_management::persistence::PersistedUnackedStanza,
    ) -> Result<
        waddle_xmpp::stream_management::persistence::PromotionAppendOutcome,
        waddle_xmpp::stream_management::persistence::SmPersistenceError,
    > {
        self.inner.append_promotion_stanza(stanza).await
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

    async fn store_session_atomic_with_principal(
        &self,
        principal: &waddle_xmpp::auth::AuthenticatedPrincipalRef,
        session: waddle_xmpp::stream_management::persistence::PersistedSession,
        unacked: Vec<waddle_xmpp::stream_management::persistence::PersistedUnackedStanza>,
    ) -> Result<(), waddle_xmpp::stream_management::persistence::SmPersistenceError> {
        self.inner
            .store_session_atomic_with_principal(principal, session, unacked)
            .await
    }

    async fn get_session_principal(
        &self,
        stream_id: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<
        Option<waddle_xmpp::auth::AuthenticatedPrincipalRef>,
        waddle_xmpp::stream_management::persistence::SmPersistenceError,
    > {
        self.inner.get_session_principal(stream_id).await
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

    async fn store_session_atomic_with_ingress_append(
        &self,
        session: waddle_xmpp::stream_management::persistence::PersistedSession,
        unacked: Vec<waddle_xmpp::stream_management::persistence::PersistedUnackedStanza>,
        append: waddle_xmpp::stream_management::persistence::PersistedIngressAppend,
    ) -> Result<
        waddle_xmpp::stream_management::persistence::KeyedSnapshotOutcome,
        waddle_xmpp::stream_management::persistence::SmPersistenceError,
    > {
        self.inner
            .store_session_atomic_with_ingress_append(session, unacked, append)
            .await
    }

    async fn store_session_atomic_with_principal_and_ingress_appends(
        &self,
        principal: &waddle_xmpp::auth::AuthenticatedPrincipalRef,
        session: waddle_xmpp::stream_management::persistence::PersistedSession,
        unacked: Vec<waddle_xmpp::stream_management::persistence::PersistedUnackedStanza>,
        appends: Vec<waddle_xmpp::stream_management::persistence::PersistedIngressAppend>,
    ) -> Result<
        Vec<waddle_xmpp::stream_management::SmIngressAppendKey>,
        waddle_xmpp::stream_management::persistence::SmPersistenceError,
    > {
        self.inner
            .store_session_atomic_with_principal_and_ingress_appends(
                principal, session, unacked, appends,
            )
            .await
    }

    async fn get_ingress_append(
        &self,
        key: &waddle_xmpp::stream_management::SmIngressAppendKey,
    ) -> Result<
        Option<waddle_xmpp::stream_management::persistence::PersistedIngressAppend>,
        waddle_xmpp::stream_management::persistence::SmPersistenceError,
    > {
        self.inner.get_ingress_append(key).await
    }

    async fn list_pending_ingress_appends_after(
        &self,
        after: Option<&waddle_xmpp::stream_management::SmIngressAppendKey>,
        limit: usize,
    ) -> Result<
        Vec<waddle_xmpp::stream_management::persistence::PersistedIngressAppend>,
        waddle_xmpp::stream_management::persistence::SmPersistenceError,
    > {
        self.inner
            .list_pending_ingress_appends_after(after, limit)
            .await
    }

    async fn scrub_ingress_custody(
        &self,
        target: &waddle_xmpp::tombstone::TombstoneTarget,
        through: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), waddle_xmpp::stream_management::persistence::SmPersistenceError> {
        self.inner.scrub_ingress_custody(target, through).await
    }

    async fn delete_tombstoned_unacked(
        &self,
        stream: &waddle_xmpp::pending_delivery::SmSessionId,
        sequences: &[u32],
    ) -> Result<u64, waddle_xmpp::stream_management::persistence::SmPersistenceError> {
        self.inner
            .delete_tombstoned_unacked(stream, sequences)
            .await
    }

    async fn get_ingress_appends_for_sequence(
        &self,
        stream: &waddle_xmpp::pending_delivery::SmSessionId,
        sequence: u32,
    ) -> Result<
        Vec<waddle_xmpp::stream_management::persistence::PersistedIngressAppend>,
        waddle_xmpp::stream_management::persistence::SmPersistenceError,
    > {
        self.inner
            .get_ingress_appends_for_sequence(stream, sequence)
            .await
    }

    async fn list_pending_ingress_appends(
        &self,
        limit: usize,
    ) -> Result<
        Vec<waddle_xmpp::stream_management::persistence::PersistedIngressAppend>,
        waddle_xmpp::stream_management::persistence::SmPersistenceError,
    > {
        self.inner.list_pending_ingress_appends(limit).await
    }

    async fn complete_ingress_append(
        &self,
        key: &waddle_xmpp::stream_management::SmIngressAppendKey,
        stream: &waddle_xmpp::pending_delivery::SmSessionId,
        sequence: u32,
        disposition: waddle_xmpp::stream_management::persistence::IngressCustodyDisposition,
    ) -> Result<bool, waddle_xmpp::stream_management::persistence::SmPersistenceError> {
        self.inner
            .complete_ingress_append(key, stream, sequence, disposition)
            .await
    }

    async fn complete_ingress_appends_through(
        &self,
        stream: &waddle_xmpp::pending_delivery::SmSessionId,
        from_exclusive: u32,
        h: u32,
    ) -> Result<(), waddle_xmpp::stream_management::persistence::SmPersistenceError> {
        self.inner
            .complete_ingress_appends_through(stream, from_exclusive, h)
            .await
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
        waddle_xmpp::xep::xep0421::OccupantIdSecret::try_new(
            b"test-occupant-id-secret-32-bytes-long".to_vec(),
        )
        .expect("secret"),
        waddle_xmpp::xep::xep0317::ServerHats::default(),
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
        .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
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
        .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
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
#[tokio::test(start_paused = true)]
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
    let shutdown_started = tokio::time::Instant::now();
    // Terminal shutdown joins the cancelled worker, then gives its captured
    // release one final bounded attempt. Allow a timer tick beyond that attempt
    // instead of racing the same deadline with a wall-clock timeout.
    let shutdown_budget = ORPHANED_ROOM_RELEASE_TIMEOUT + Duration::from_millis(10);
    cancel.cancel();
    tokio::time::timeout(shutdown_budget, supervisor.shutdown())
        .await
        .expect("hung exact-release worker must cancel and join");
    let elapsed = shutdown_started.elapsed();
    assert!(elapsed >= ORPHANED_ROOM_RELEASE_TIMEOUT);
    assert!(elapsed <= shutdown_budget);
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

#[cfg(all(test, unix, feature = "clustering"))]
#[test]
fn poisoned_orphan_inventory_aborts_instead_of_losing_the_supervisor() {
    use std::os::unix::process::ExitStatusExt;

    const CHILD_ENV: &str = "WADDLE_POISONED_ORPHAN_INVENTORY_CHILD";
    if std::env::var_os(CHILD_ENV).is_some() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime")
            .block_on(async {
                let registry =
                    Arc::new(waddle_xmpp::stream_management::InMemorySmSessionRegistry::new());
                let cancel = tokio_util::sync::CancellationToken::new();
                let supervisor = OrphanReaperSupervisor::new(registry.clone(), cancel.clone());
                let pending = supervisor.workers.hydration_pending.clone();
                std::thread::spawn(move || {
                    let _pending = pending.lock().expect("unpoisoned inventory");
                    panic!("writer failed while changing exact ownership inventory");
                })
                .join()
                .expect_err("writer panicked while holding the pending-map lock");

                // Restart snapshots the poisoned map before shutting workers
                // down. A panic contained by Tokio must not strand live node
                // authority after killing only the supervisor task.
                let _ = tokio::spawn(async move {
                    let restarted = supervisor.restarted(registry, cancel).await;
                    restarted.shutdown().await;
                })
                .await;
            });
        return;
    }

    let output = std::process::Command::new(
        std::env::current_exe().expect("current test binary"),
    )
    .args([
        "--exact",
        "server::session_janitors::poisoned_orphan_inventory_aborts_instead_of_losing_the_supervisor",
        "--nocapture",
    ])
    .env(CHILD_ENV, "1")
    .output()
    .expect("isolated poisoned supervisor process");
    assert_eq!(
        output.status.signal(),
        Some(libc::SIGABRT),
        "poisoned restart inventory must abort the process, not disappear with its supervisor task: {}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
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
                .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
                .is_empty();
            let release_empty = workers
                .release_pending
                .lock()
                .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
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
            occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
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
            resource_presence: None,
            cluster_membership: None,
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

#[cfg(test)]
mod pending_delivery_retention_tests {
    use super::{run_pending_delivery_claim_sweep, run_pending_promotion_receipt_sweep};
    use crate::pending_delivery::DatabasePendingDeliveryStorage;
    use crate::server::routes::websocket::tests::create_test_websocket_state_with_sm_registry_and_pending_storage;
    use std::sync::Arc;
    use waddle_xmpp::pending_delivery::{
        InsertOutcome, PendingPayload, PendingRow, PendingRowId, QuotaPolicy, SmSessionId,
    };
    use waddle_xmpp::stream_management::persistence::{
        InMemorySmPersistence, PersistedSession, SmPersistenceStorage,
    };
    use waddle_xmpp::stream_management::InMemorySmSessionRegistry;

    use waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage;

    fn pending_row() -> PendingRow {
        let recipient: jid::BareJid = "offline@example.com".parse().expect("recipient");
        let mut message = xmpp_parsers::message::Message::new(Some(recipient.clone().into()));
        message.from = Some("sender@example.com/web".parse().expect("sender"));
        message.bodies.insert(
            xmpp_parsers::message::Lang::new(),
            "pending message".to_owned(),
        );
        PendingRow {
            id: PendingRowId::fresh(),
            recipient,
            original_receipt_at: chrono::Utc::now(),
            payload: PendingPayload::Transient(Box::new(message)),
            flushed_in_session: None,
            outbound_sequence: None,
        }
    }

    #[tokio::test]
    async fn real_pending_claim_janitor_ages_out_rows_restores_quota_and_exports_metric() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let storage = Arc::new(
            DatabasePendingDeliveryStorage::open(None, QuotaPolicy::CountCap { max_rows: 1 })
                .await
                .expect("database pending storage"),
        );
        let state = create_test_websocket_state_with_sm_registry_and_pending_storage(
            Arc::new(InMemorySmSessionRegistry::new()),
            storage.clone(),
        )
        .await;
        let mut expired = pending_row();
        expired.original_receipt_at -= chrono::Duration::days(366);
        assert_eq!(
            storage.insert(expired).await.expect("old row"),
            InsertOutcome::Inserted
        );
        let new_row = pending_row();
        assert_eq!(
            storage.insert(new_row.clone()).await.expect("full quota"),
            InsertOutcome::QuotaExceeded
        );

        let mut recovery = crate::pending_delivery::PendingRecovery::default();
        run_pending_delivery_claim_sweep(&state, 60, &mut recovery).await;

        assert_eq!(
            metrics.counter_sum("xmpp.pending_delivery.aged_out", &[]),
            Some(1)
        );
        assert_eq!(
            storage
                .insert(new_row.clone())
                .await
                .expect("capacity after real sweep"),
            InsertOutcome::Inserted
        );
        let retained = storage.list(&new_row.recipient).await.expect("new queue");
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].id, new_row.id);
        assert_eq!(
            metrics.counter_sum(
                "waddle.janitor.sweeps",
                &[
                    ("janitor", "pending_delivery_claim"),
                    ("outcome", "completed")
                ]
            ),
            Some(1)
        );
    }

    fn live_source(stream_id: &str) -> PersistedSession {
        PersistedSession {
            stream_id: SmSessionId::new(stream_id),
            user_id: "offline@example.com".to_owned(),
            jid: "offline@example.com/web".parse().expect("source JID"),
            occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            inbound_count: 0,
            outbound_count: 1,
            last_acked: 0,
            replay_gap_through: None,
            max_resume_time: Some(0),
            detached_at: chrono::Utc::now(),
            max_resume_duration: std::time::Duration::ZERO,
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
        }
    }

    #[tokio::test]
    async fn orphan_claim_sweep_protects_durable_sm_custody_before_and_after_resume_ack() {
        use waddle_xmpp::ownership::{ClaimEpoch, NodeIdentity};
        use waddle_xmpp::stream_management::persistence::{SmAckWindow, SmClaimFence};
        let pending = Arc::new(
            DatabasePendingDeliveryStorage::open(None, QuotaPolicy::Unlimited)
                .await
                .expect("pending database"),
        );
        let row = pending_row();
        let recipient = row.recipient.clone();
        let stream = SmSessionId::new("restart-with-unsettled-resume-ack");
        pending.insert(row).await.expect("pending row");
        let claimed = pending
            .claim_for_session(&recipient, &stream)
            .await
            .expect("claim");
        pending
            .record_pushed_at(&claimed[0].id, 1)
            .await
            .expect("offered sequence");
        let source = Arc::new(InMemorySmPersistence::new());
        source
            .upsert_session(live_source(stream.as_str()))
            .await
            .expect("source");
        let fence = SmClaimFence::new(
            NodeIdentity::new("test-node", "test-incarnation"),
            ClaimEpoch(1),
        );
        // Simulate a restart before this source has been rehydrated locally.
        let registry = Arc::new(InMemorySmSessionRegistry::new().with_persistence(source.clone()));
        assert!(registry.live_session_ids().expect("local IDs").is_empty());
        let state = create_test_websocket_state_with_sm_registry_and_pending_storage(
            registry,
            pending.clone(),
        )
        .await;
        let mut recovery = crate::pending_delivery::PendingRecovery::default();
        run_pending_delivery_claim_sweep(&state, 0, &mut recovery).await;
        let retained = pending
            .list(&recipient)
            .await
            .expect("retained pending row");
        assert_eq!(retained[0].flushed_in_session, Some(stream.clone()));
        assert_eq!(retained[0].outbound_sequence, Some(1));
        source
            .acknowledge_claimed_session(&stream, 1, &fence, None)
            .await
            .expect("durable ACK");
        run_pending_delivery_claim_sweep(&state, 0, &mut recovery).await;
        let retained = pending
            .list(&recipient)
            .await
            .expect("ACK remains protected");
        assert_eq!(retained[0].flushed_in_session, Some(stream.clone()));
        // Remove only the protection in this control: it proves claim age and
        // the ordinary orphan predicate would otherwise release this row.
        source
            .confirm_acknowledgement(
                &stream,
                SmAckWindow {
                    from_exclusive: 0,
                    to_inclusive: 1,
                },
                &fence,
                None,
            )
            .await
            .expect("clear control protection");
        source
            .delete_session(&stream)
            .await
            .expect("retire control source");
        run_pending_delivery_claim_sweep(&state, 0, &mut recovery).await;
        let released = pending
            .list(&recipient)
            .await
            .expect("ordinary orphan release");
        assert!(released[0].flushed_in_session.is_none());
        assert!(released[0].outbound_sequence.is_none());
    }

    #[tokio::test]
    async fn orphan_receipt_scan_advances_past_a_full_page_of_live_sources() {
        let storage = Arc::new(
            DatabasePendingDeliveryStorage::open(None, QuotaPolicy::Unlimited)
                .await
                .expect("receipt database"),
        );
        let sources = Arc::new(InMemorySmPersistence::new());
        let registry = Arc::new(InMemorySmSessionRegistry::new().with_persistence(sources.clone()));
        let state = create_test_websocket_state_with_sm_registry_and_pending_storage(
            registry,
            storage.clone(),
        )
        .await;
        for index in 0..129 {
            let source = format!("source-{index:03}");
            if index < 128 {
                sources
                    .upsert_session(live_source(&source))
                    .await
                    .expect("live canonical source");
            }
            assert_eq!(
                storage
                    .insert_fenced_and_prune_unacked(pending_row(), &source, 1)
                    .await
                    .expect("portable receipt"),
                InsertOutcome::Inserted
            );
        }
        let mut cursor = None;
        assert!(!run_pending_promotion_receipt_sweep(&state, i64::MAX, &mut cursor).await);
        assert_eq!(cursor.as_ref().map(SmSessionId::as_str), Some("source-127"));
        assert_eq!(
            storage
                .promotion_receipt_sources_before(i64::MAX, None, 200)
                .await
                .expect("receipts retained")
                .len(),
            129
        );

        assert!(!run_pending_promotion_receipt_sweep(&state, i64::MAX, &mut cursor).await);
        assert!(cursor.is_none(), "short final page wraps the next tick");
        let remaining = storage
            .promotion_receipt_sources_before(i64::MAX, None, 200)
            .await
            .expect("verified receipt cleanup");
        assert_eq!(
            remaining.len(),
            128,
            "live sources survive while the later orphan is retired"
        );
        assert!(!remaining.contains(&SmSessionId::new("source-128")));
        assert_eq!(
            storage
                .count(&pending_row().recipient)
                .await
                .expect("pending custody"),
            129,
            "receipt GC does not delete delivered-custody rows"
        );
    }
}

#[cfg(test)]
mod room_dormancy_tests {
    use super::{run_room_dormancy_sweep, sweep_dormant_rooms_once};
    use crate::server::routes::websocket::tests::create_test_websocket_state;
    use waddle_xmpp::muc::room_actor::{
        ChangeAffiliation, GetOccupantByJid, Join, JoinAffiliationGrant, JoinWithAffiliation,
        LeaveByRealJid,
    };
    use waddle_xmpp::muc::room_registry_actor::{CreateRoom, GetOrCreateRoom, RoomCount};
    use waddle_xmpp::muc::RoomConfig;
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
                session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
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
                session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
                nick: "alice".to_string(),
                real_jid: alice.clone(),
                role: Role::Participant,
                affiliation: Affiliation::Member,
            })
            .await
            .expect("join");
        let waddle_xmpp::muc::room_actor::LeaveDisposition::Left(outcome) = actor
            .ask(LeaveByRealJid {
                sender_jid: alice,
                cause: waddle_xmpp::muc::durable::OccupancyLeaveCause::Disconnect,
                session: waddle_xmpp::muc::room_actor::LeaveSessionSelector::Any,
                attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
                origin: waddle_xmpp::muc::room_actor::LeaveOrigin::Fresh,
            })
            .await
            .expect("leave")
        else {
            panic!("occupant leaves");
        };
        // #1647: acknowledge the departure receipt — an unacknowledged
        // receipt now legitimately vetoes dormancy/eviction.
        assert_eq!(
            actor
                .ask(waddle_xmpp::muc::room_actor::AckDepartureReceipt {
                    attempt: outcome.acknowledge,
                })
                .await
                .expect("ack ask"),
            waddle_xmpp::muc::room_actor::AckDepartureOutcome::Acknowledged
        );

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
                session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            })
            .await
            .expect("resolver-derived member join");
        let waddle_xmpp::muc::room_actor::LeaveDisposition::Left(outcome) = actor
            .ask(LeaveByRealJid {
                sender_jid: alice,
                cause: waddle_xmpp::muc::durable::OccupancyLeaveCause::Disconnect,
                session: waddle_xmpp::muc::room_actor::LeaveSessionSelector::Any,
                attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
                origin: waddle_xmpp::muc::room_actor::LeaveOrigin::Fresh,
            })
            .await
            .expect("leave")
        else {
            panic!("occupant leaves");
        };
        // #1647: acknowledge the departure receipt — an unacknowledged
        // receipt now legitimately vetoes dormancy/eviction.
        assert_eq!(
            actor
                .ask(waddle_xmpp::muc::room_actor::AckDepartureReceipt {
                    attempt: outcome.acknowledge,
                })
                .await
                .expect("ack ask"),
            waddle_xmpp::muc::room_actor::AckDepartureOutcome::Acknowledged
        );

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
                session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            })
            .await
            .expect("first join");
        let waddle_xmpp::muc::room_actor::LeaveDisposition::Left(outcome) = actor
            .ask(LeaveByRealJid {
                sender_jid: alice.clone(),
                cause: waddle_xmpp::muc::durable::OccupancyLeaveCause::Disconnect,
                session: waddle_xmpp::muc::room_actor::LeaveSessionSelector::Any,
                attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
                origin: waddle_xmpp::muc::room_actor::LeaveOrigin::Fresh,
            })
            .await
            .expect("leave")
        else {
            panic!("occupant leaves");
        };
        // #1647: acknowledge the departure receipt — an unacknowledged
        // receipt now legitimately vetoes dormancy/eviction.
        assert_eq!(
            actor
                .ask(waddle_xmpp::muc::room_actor::AckDepartureReceipt {
                    attempt: outcome.acknowledge,
                })
                .await
                .expect("ack ask"),
            waddle_xmpp::muc::room_actor::AckDepartureOutcome::Acknowledged
        );
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
                session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
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
    use std::sync::Arc;
    use waddle_xmpp::registry::{
        ConnectionEntry, GetUser, RegisterUserResource, TrySendPeer, UserCount,
        WireUserClusteringClaims,
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

    struct HoldReaperUser {
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }

    impl kameo::message::Message<HoldReaperUser> for waddle_xmpp::registry::user_actor::UserActor {
        type Reply = ();

        async fn handle(
            &mut self,
            msg: HoldReaperUser,
            _ctx: &mut kameo::message::Context<Self, Self::Reply>,
        ) {
            msg.entered.notify_one();
            msg.release.notified().await;
        }
    }

    #[tokio::test]
    async fn busy_user_unregister_is_deferred_without_failed_heartbeat() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let state = create_test_websocket_state().await;
        let registry = &state.deps.protocol.user_registry;
        let jid = full_jid("busy-reaper@example.com/web");
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        registry
            .ask(RegisterUserResource {
                jid: jid.clone(),
                entry: ConnectionEntry::new(tx),
            })
            .await
            .expect("register resource awaiting cleanup");
        let actor = registry
            .ask(GetUser {
                bare_jid: jid.to_bare(),
            })
            .await
            .expect("look up registered user")
            .expect("registered actor exists");
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        actor
            .tell(HoldReaperUser {
                entered: entered.clone(),
                release: release.clone(),
            })
            .await
            .expect("hold user actor busy");
        entered.notified().await;
        registry
            .ask(waddle_xmpp::registry::RecordPendingUserUnregister {
                jid: jid.clone(),
                owner: None,
            })
            .await
            .expect("record owed resource cleanup");

        // Database setup needs real time; only the gated actor retry is virtual.
        tokio::time::pause();
        run_user_actor_reaper_sweep(&state).await;
        assert_eq!(
            metrics.counter_sum(
                "waddle.janitor.sweeps",
                &[("janitor", "user_actor_reaper"), ("outcome", "deferred"),]
            ),
            Some(1),
            "busy child actors defer work without failing the reaper"
        );
        assert_eq!(
            metrics.counter_sum(
                "waddle.janitor.sweeps",
                &[("janitor", "user_actor_reaper"), ("outcome", "failed"),]
            ),
            Some(0)
        );

        release.notify_one();
        actor
            .ask(waddle_xmpp::registry::user_actor::ResourceCount)
            .await
            .expect("drain prior child asks after releasing the gate");
        let recovered = sweep_empty_user_actors_once(&state).await;
        assert_eq!(recovered.outcome(), super::SweepOutcome::Completed);
        assert_eq!(recovered.remaining, 0);
    }

    pub(super) struct CountingReleaseFailureClaimStore {
        pub(super) inner: waddle_xmpp::ownership::InProcessClaimStore,
        pub(super) fail_release: std::sync::atomic::AtomicBool,
        pub(super) release_calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl waddle_xmpp::ownership::ClaimStore for CountingReleaseFailureClaimStore {
        async fn ensure_schema(&self) -> Result<(), waddle_xmpp::ownership::ClaimError> {
            self.inner.ensure_schema().await
        }

        async fn acquire(
            &self,
            entity: &waddle_xmpp::ownership::Entity,
            me: &waddle_xmpp::ownership::NodeIdentity,
        ) -> Result<waddle_xmpp::ownership::ClaimEpoch, waddle_xmpp::ownership::ClaimError>
        {
            self.inner.acquire(entity, me).await
        }

        async fn ensure_claimed(
            &self,
            entity: &waddle_xmpp::ownership::Entity,
            me: &waddle_xmpp::ownership::NodeIdentity,
        ) -> Result<waddle_xmpp::ownership::ClaimEpoch, waddle_xmpp::ownership::ClaimError>
        {
            self.inner.ensure_claimed(entity, me).await
        }

        async fn steal_stale(
            &self,
            entity: &waddle_xmpp::ownership::Entity,
            observed: waddle_xmpp::ownership::ClaimEpoch,
            staleness: waddle_xmpp::ownership::StalePredicate,
            me: &waddle_xmpp::ownership::NodeIdentity,
        ) -> Result<waddle_xmpp::ownership::ClaimEpoch, waddle_xmpp::ownership::ClaimError>
        {
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
        ) -> Result<waddle_xmpp::ownership::ClaimEpoch, waddle_xmpp::ownership::ClaimError>
        {
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
            self.release_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.fail_release.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(waddle_xmpp::ownership::ClaimError::Backend(
                    "injected user-claim release failure".to_string(),
                ));
            }
            self.inner.release(entity, me, mine).await
        }

        async fn release_exact(
            &self,
            entity: &waddle_xmpp::ownership::Entity,
            me: &waddle_xmpp::ownership::NodeIdentity,
            mine: waddle_xmpp::ownership::ClaimEpoch,
        ) -> Result<waddle_xmpp::ownership::ExactReleaseOutcome, waddle_xmpp::ownership::ClaimError>
        {
            self.inner.release_exact(entity, me, mine).await
        }

        async fn release_many(
            &self,
            entities: &[waddle_xmpp::ownership::Entity],
            me: &waddle_xmpp::ownership::NodeIdentity,
        ) -> Result<(), waddle_xmpp::ownership::ClaimError> {
            self.inner.release_many(entities, me).await
        }
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
                ingress_append: None,
                jid: jid.clone(),
                stanza: sample_stanza(&jid),
                original_receipt_at: None,
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

    #[tokio::test]
    async fn sweep_retries_terminal_claim_release_recorded_during_reap() {
        let _metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let state = create_test_websocket_state().await;
        let jid = full_jid("carol@example.com/web");
        let user_registry = &state.deps.protocol.user_registry;
        let claim_store = Arc::new(CountingReleaseFailureClaimStore {
            inner: waddle_xmpp::ownership::InProcessClaimStore::new(),
            fail_release: std::sync::atomic::AtomicBool::new(true),
            release_calls: std::sync::atomic::AtomicUsize::new(0),
        });
        user_registry
            .ask(WireUserClusteringClaims {
                claim_store: claim_store.clone(),
                node_identity: waddle_xmpp::ownership::SharedNodeIdentity::new(
                    waddle_xmpp::ownership::NodeIdentity::new("user-reaper-node", "test"),
                ),
            })
            .await
            .expect("wire custom claim store");

        let (tx, rx) = tokio::sync::mpsc::channel(16);
        user_registry
            .ask(RegisterUserResource {
                jid: jid.clone(),
                entry: ConnectionEntry::new(tx),
            })
            .await
            .expect("register");

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
                ingress_append: None,
                jid: jid.clone(),
                stanza: sample_stanza(&jid),
                original_receipt_at: None,
            })
            .await
            .expect("try send");

        let counts = sweep_empty_user_actors_once(&state).await;
        assert_eq!(counts.examined, 1);
        assert_eq!(counts.reaped, 1);
        assert_eq!(counts.remaining, 0);
        assert!(
            counts.failed,
            "a reap-time claim-release failure must keep the sweep non-complete"
        );
        assert_eq!(
            claim_store
                .release_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            2,
            "the janitor must retry terminal release work again after the reap loop"
        );

        claim_store
            .fail_release
            .store(false, std::sync::atomic::Ordering::SeqCst);
        let retry_counts = sweep_empty_user_actors_once(&state).await;
        assert_eq!(retry_counts.reaped, 0);
        assert_eq!(retry_counts.remaining, 0);
        assert!(
            !retry_counts.failed,
            "once the release backend recovers, the next sweep should fully converge"
        );
        assert_eq!(
            claim_store
                .release_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            3,
            "the deferred terminal release should clear on the next sweep"
        );
    }
}

#[cfg(all(test, feature = "clustering"))]
mod local_muc_departure_tests {
    use super::run_local_muc_departure_sweep;
    use crate::clustering::ClusteringHandles;
    use crate::server::routes::websocket::tests::{
        create_test_websocket_state, create_test_websocket_state_with_clustering,
        create_test_websocket_state_with_sfu, register_test_connection, snapshot_room,
        RecordingSfu,
    };
    use jid::{BareJid, FullJid};
    use std::collections::HashMap;
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::Instant;
    use tokio::sync::mpsc;
    use waddle_sfu::SfuService;
    use waddle_xmpp::muc::durable::{
        ChannelId, DurableRoomState, MucDurableFuture, MucDurableStore, OccupancyLeaveCause,
        RoomClaimFenceContext, WaddleId,
    };
    use waddle_xmpp::muc::room_actor::{
        GetSnapshot, Join, LeaveByRealJid, LeaveDisposition, LeaveSessionSelector,
        OccupancyWatermark, RestoreLiveRoster, SealForDestroy, SealGuard, SealIfInactive,
        SealIfInactiveOutcome, UnsealInactive,
    };
    use waddle_xmpp::muc::room_registry_actor::{
        CreateRoom, GetOrCreateRoomWithLiveRoster, ReapSealedRoom, WireClusteringClaims,
    };
    use waddle_xmpp::muc::{
        RoomCommitError, RoomCommitOutcome, RoomCommittedCoordinates, RoomConfig, RoomLifecycleId,
        RoomMutationEffects, RoomRegistry, RoomRevision,
    };
    use waddle_xmpp::ownership::{
        ClaimStore, InProcessClaimStore, NodeIdentity, SharedNodeIdentity,
    };
    use waddle_xmpp::stream_management::InMemorySmSessionRegistry;
    use waddle_xmpp_core::{Affiliation, OccupancySessionGeneration, Role};

    fn room_jid(local: &str) -> BareJid {
        format!("{local}@muc.example.com")
            .parse()
            .expect("room jid")
    }

    fn full_jid(value: &str) -> FullJid {
        value.parse().expect("full jid")
    }

    async fn create_room(
        state: &crate::server::routes::websocket::WebSocketState,
        room_jid: &BareJid,
    ) -> kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor> {
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
            .expect("create room")
    }

    async fn join_member(
        actor: &kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor>,
        jid: &FullJid,
        nick: &str,
    ) {
        actor
            .ask(Join {
                session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
                nick: nick.to_string(),
                real_jid: jid.clone(),
                role: Role::Participant,
                affiliation: Affiliation::Member,
            })
            .await
            .expect("join member");
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum LeaveProjectionMode {
        Succeed,
        OwnershipUnavailable,
        NotOwner,
        Hang,
        Delay(std::time::Duration),
    }

    struct JanitorProjectionStore {
        leave_mode: Mutex<LeaveProjectionMode>,
        established_fences: Mutex<HashMap<BareJid, RoomClaimFenceContext>>,
        lifecycle: OnceLock<RoomLifecycleId>,
        next_revision: AtomicUsize,
    }

    impl JanitorProjectionStore {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                leave_mode: Mutex::new(LeaveProjectionMode::Succeed),
                established_fences: Mutex::new(HashMap::new()),
                lifecycle: OnceLock::new(),
                next_revision: AtomicUsize::new(0),
            })
        }

        fn set_leave_mode(&self, mode: LeaveProjectionMode) {
            *self.leave_mode.lock().expect("leave mode lock") = mode;
        }

        fn next_commit_coordinates(&self) -> RoomCommittedCoordinates {
            let lifecycle = *self.lifecycle.get_or_init(RoomLifecycleId::generate);
            let revision = self.next_revision.fetch_add(1, Ordering::SeqCst) + 1;
            RoomCommittedCoordinates {
                lifecycle,
                revision: RoomRevision::from_stored(revision as i64).expect("positive revision"),
            }
        }
    }

    impl MucDurableStore for JanitorProjectionStore {
        fn load_room_state_fenced<'a>(
            &'a self,
            room_jid: &'a BareJid,
            fence: &'a RoomClaimFenceContext,
        ) -> MucDurableFuture<'a, Option<DurableRoomState>> {
            let exact = self
                .established_fences
                .lock()
                .expect("fence lock")
                .get(room_jid)
                .cloned();
            let given = fence.clone();
            Box::pin(async move {
                if exact.as_ref() == Some(&given) {
                    Ok(None)
                } else {
                    Err(waddle_xmpp::XmppError::internal(
                        "unexpected room claim fence",
                    ))
                }
            })
        }

        fn commit_room_mutation<'a>(
            &'a self,
            room_jid: &'a BareJid,
            fence: &'a RoomClaimFenceContext,
            intent: waddle_xmpp::muc::RoomDurableMutation,
            _effects: RoomMutationEffects,
        ) -> waddle_xmpp::muc::RoomCommitFuture<'a> {
            let exact = self
                .established_fences
                .lock()
                .expect("fence lock")
                .get(room_jid)
                .cloned();
            let given = fence.clone();
            let leave_mode = *self.leave_mode.lock().expect("leave mode lock");
            let coordinates = self.next_commit_coordinates();
            Box::pin(async move {
                if exact.as_ref() != Some(&given) {
                    return Err(RoomCommitError::OwnershipUnavailable);
                }
                match intent {
                    waddle_xmpp::muc::RoomDurableMutation::Projection(
                        waddle_xmpp::muc::durable::RoomProjection::OccupancyLeave { .. },
                    ) => match leave_mode {
                        LeaveProjectionMode::Succeed => Ok(RoomCommitOutcome {
                            coordinates,
                            reservation: None,
                        }),
                        LeaveProjectionMode::OwnershipUnavailable => {
                            Err(RoomCommitError::OwnershipUnavailable)
                        }
                        LeaveProjectionMode::NotOwner => Err(RoomCommitError::NotOwner),
                        LeaveProjectionMode::Hang => {
                            std::future::pending::<Result<RoomCommitOutcome, RoomCommitError>>()
                                .await
                        }
                        LeaveProjectionMode::Delay(delay) => {
                            tokio::time::sleep(delay).await;
                            Ok(RoomCommitOutcome {
                                coordinates,
                                reservation: None,
                            })
                        }
                    },
                    _ => Ok(RoomCommitOutcome {
                        coordinates,
                        reservation: None,
                    }),
                }
            })
        }

        fn establish_claim_fence(&self, room_jid: &BareJid, fence: RoomClaimFenceContext) {
            self.established_fences
                .lock()
                .expect("fence lock")
                .insert(room_jid.clone(), fence);
        }

        fn check_exact_claim_fence<'a>(
            &'a self,
            room_jid: &'a BareJid,
            fence: &'a RoomClaimFenceContext,
        ) -> MucDurableFuture<'a, bool> {
            let exact = self
                .established_fences
                .lock()
                .expect("fence lock")
                .get(room_jid)
                .cloned();
            let given = fence.clone();
            Box::pin(async move { Ok(exact.as_ref() == Some(&given)) })
        }
    }

    async fn clustered_state_with_store(
        store: Arc<JanitorProjectionStore>,
    ) -> Arc<crate::server::routes::websocket::WebSocketState> {
        let claim_store = Arc::new(InProcessClaimStore::new());
        let node_identity = SharedNodeIdentity::new(NodeIdentity::local());
        let durable_store: Arc<dyn MucDurableStore> = store.clone();
        let state = create_test_websocket_state_with_clustering(
            ClusteringHandles {
                claim_store: Some(claim_store.clone() as Arc<dyn ClaimStore>),
                node_identity: Some(node_identity.clone()),
                muc_durable_store: Some(durable_store.clone()),
                ..ClusteringHandles::default()
            },
            Arc::new(InMemorySmSessionRegistry::new()),
        )
        .await;
        state
            .deps
            .protocol
            .room_registry
            .ask(WireClusteringClaims {
                claim_store: claim_store as Arc<dyn ClaimStore>,
                node_identity,
                durable_store: Some(durable_store),
                rollout_backoff: None,
            })
            .await
            .expect("wire clustering claims");
        state
    }

    async fn assert_disconnect_departure_retry_fans_out_once(room_name: &str) {
        let store = JanitorProjectionStore::new();
        let state = clustered_state_with_store(store.clone()).await;
        let room = room_jid(room_name);
        let alice = full_jid("alice@example.com/web");
        let bob = full_jid("bob@example.com/phone");
        let actor = state
            .deps
            .protocol
            .room_registry
            .ask(CreateRoom {
                room_jid: room.clone(),
                waddle_id: "w".to_string(),
                channel_id: "c".to_string(),
                config: RoomConfig {
                    persistent: false,
                    ..RoomConfig::default()
                },
            })
            .await
            .expect("create inactive target room");
        join_member(&actor, &alice, "alice").await;
        join_member(&actor, &bob, "bob").await;
        let (bob_tx, mut bob_rx) = mpsc::channel(8);
        register_test_connection(state.as_ref(), &bob, bob_tx).await;
        while bob_rx.try_recv().is_ok() {}
        store.set_leave_mode(LeaveProjectionMode::OwnershipUnavailable);
        state.deps.protocol.pending_local_muc_departures.record(
            crate::server::routes::websocket::LocalDepartureItem::RoomDeparture {
                room: room.clone(),
                jid: alice.clone(),
                cause: OccupancyLeaveCause::Disconnect,
                selector: LeaveSessionSelector::Any,
                attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
                notified: std::collections::HashSet::new(),
                removal: waddle_xmpp::muc::MucRemovalCause::Voluntary,
            },
        );

        run_local_muc_departure_sweep(&state).await;

        assert!(
            bob_rx.try_recv().is_err(),
            "the deferred retry must not broadcast unavailable yet"
        );
        assert_eq!(
            state.deps.protocol.pending_local_muc_departures.len(),
            1,
            "the deferred retry stays in the local inventory"
        );

        store.set_leave_mode(LeaveProjectionMode::Succeed);
        // The requeue backoff is 2s plus up to 25% jitter: wait past the maximum.
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        run_local_muc_departure_sweep(&state).await;

        let unavailable = bob_rx.try_recv().expect("exactly one unavailable");
        let waddle_xmpp::Stanza::Presence(presence) = unavailable.stanza else {
            panic!("expected presence broadcast");
        };
        assert_eq!(presence.type_, xmpp_parsers::presence::Type::Unavailable);
        assert!(
            bob_rx.try_recv().is_err(),
            "the successful retry must fan out exactly once"
        );
        assert_eq!(
            state.deps.protocol.pending_local_muc_departures.len(),
            0,
            "the retained room departure must converge once the leave commits"
        );
    }

    #[tokio::test]
    async fn local_departure_sweep_deferred_then_left_fans_out_once() {
        assert_disconnect_departure_retry_fans_out_once("janitor-deferred-then-left").await;
    }

    #[tokio::test]
    async fn janitor_replays_departure_completed_after_reply_timeout() {
        let store = JanitorProjectionStore::new();
        let state = clustered_state_with_store(store.clone()).await;
        let room = room_jid("janitor-reply-timeout-replay");
        let alice = full_jid("alice@example.com/web");
        let bob = full_jid("bob@example.com/phone");
        let actor = create_room(state.as_ref(), &room).await;
        join_member(&actor, &alice, "alice").await;
        join_member(&actor, &bob, "bob").await;
        let (bob_tx, mut bob_rx) = mpsc::channel(8);
        register_test_connection(state.as_ref(), &bob, bob_tx).await;
        while bob_rx.try_recv().is_ok() {}

        let attempt = waddle_xmpp::muc::room_actor::LeaveAttemptId::generate();
        store.set_leave_mode(LeaveProjectionMode::Delay(std::time::Duration::from_secs(
            6,
        )));
        state.deps.protocol.pending_local_muc_departures.record(
            crate::server::routes::websocket::LocalDepartureItem::RoomDeparture {
                room: room.clone(),
                jid: alice.clone(),
                cause: OccupancyLeaveCause::Disconnect,
                selector: LeaveSessionSelector::Any,
                attempt,
                notified: std::collections::HashSet::new(),
                removal: waddle_xmpp::muc::MucRemovalCause::Voluntary,
            },
        );

        run_local_muc_departure_sweep(&state).await;

        assert!(
            bob_rx.try_recv().is_err(),
            "the timed-out first ask must not fan out yet"
        );
        assert_eq!(
            state.deps.protocol.pending_local_muc_departures.len(),
            1,
            "the timed-out departure must stay retained for replay"
        );

        tokio::time::sleep(std::time::Duration::from_secs(7)).await;
        run_local_muc_departure_sweep(&state).await;

        let unavailable = bob_rx.try_recv().expect("exactly one replayed unavailable");
        let waddle_xmpp::Stanza::Presence(presence) = unavailable.stanza else {
            panic!("expected presence broadcast");
        };
        assert_eq!(presence.type_, xmpp_parsers::presence::Type::Unavailable);
        assert!(
            bob_rx.try_recv().is_err(),
            "the same-attempt replay receipt must fan out exactly once"
        );
        assert_eq!(
            state.deps.protocol.pending_local_muc_departures.len(),
            0,
            "the retained departure must converge once the receipt is replayed"
        );
    }

    #[tokio::test]
    async fn disconnect_cleanup_transient_leave_converges_via_janitor_with_exactly_one_unavailable()
    {
        assert_disconnect_departure_retry_fans_out_once("disconnect-janitor-converges").await;
    }

    #[tokio::test]
    async fn janitor_leave_ask_timeout_requeues_departure_not_confirm_retired() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let store = JanitorProjectionStore::new();
        let state = clustered_state_with_store(store.clone()).await;
        let room = room_jid("janitor-leave-timeout");
        let alice = full_jid("alice@example.com/web");
        let bob = full_jid("bob@example.com/phone");
        let actor = create_room(state.as_ref(), &room).await;
        join_member(&actor, &alice, "alice").await;
        join_member(&actor, &bob, "bob").await;
        store.set_leave_mode(LeaveProjectionMode::Hang);
        state.deps.protocol.pending_local_muc_departures.record(
            crate::server::routes::websocket::LocalDepartureItem::RoomDeparture {
                room: room.clone(),
                jid: alice.clone(),
                cause: OccupancyLeaveCause::Disconnect,
                selector: LeaveSessionSelector::Any,
                attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
                notified: std::collections::HashSet::new(),
                removal: waddle_xmpp::muc::MucRemovalCause::Voluntary,
            },
        );

        run_local_muc_departure_sweep(&state).await;

        let retained = state
            .deps
            .protocol
            .pending_local_muc_departures
            .take_due(Instant::now() + std::time::Duration::from_secs(120));
        assert_eq!(
            retained.len(),
            1,
            "the timed-out departure must be retained"
        );
        assert_eq!(
            retained[0].attempts, 1,
            "the timed-out departure backs off once"
        );
        assert!(matches!(
            &retained[0].item,
            crate::server::routes::websocket::LocalDepartureItem::RoomDeparture {
                room: retained_room,
                jid: retained_jid,
                cause: OccupancyLeaveCause::Disconnect,
                selector: LeaveSessionSelector::Any,
                ..
            } if retained_room == &room && retained_jid == &alice
        ));
        assert!(
            metrics
                .counter_sum(
                    "waddle.muc.local_departure_retry",
                    &[("outcome", "requeued")],
                )
                .is_some_and(|count| count >= 1),
            "a timed-out janitor leave must record a requeue outcome"
        );
    }

    #[tokio::test]
    async fn local_departure_sweep_replacement_rejoin_is_superseded_without_sfu_teardown() {
        let recorder = Arc::new(RecordingSfu::default());
        let state = create_test_websocket_state_with_sfu(recorder.clone()).await;
        let room = room_jid("superseded-rejoin");
        let jid = full_jid("alice@example.com/web");
        let actor = create_room(state.as_ref(), &room).await;
        join_member(&actor, &jid, "alice").await;
        let before = actor.ask(GetSnapshot).await.expect("snapshot");
        let watermark = OccupancyWatermark::from_revision(before.occupancy_revision);
        // A replacement connection binds the same full JID and re-joins under
        // the same nick: the existing-session rejoin path advances the
        // session watermark past the deferred attempt's.
        actor
            .ask(waddle_xmpp::muc::room_actor::JoinWithAffiliation {
                sender_jid: jid.clone(),
                nick: "alice".to_string(),
                affiliation_grant: waddle_xmpp::muc::room_actor::JoinAffiliationGrant::Resolver(
                    Affiliation::Member,
                ),
                local_domain: "example.com".to_string(),
                admission_revision: before.admission_revision,
                session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            })
            .await
            .expect("replacement rejoin");
        state.deps.protocol.pending_local_muc_departures.record(
            crate::server::routes::websocket::LocalDepartureItem::RoomDeparture {
                room: room.clone(),
                jid: jid.clone(),
                cause: OccupancyLeaveCause::Disconnect,
                selector: LeaveSessionSelector::JoinedAtOrBefore(watermark),
                attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
                notified: std::collections::HashSet::new(),
                removal: waddle_xmpp::muc::MucRemovalCause::Voluntary,
            },
        );

        run_local_muc_departure_sweep(&state).await;

        assert!(
            recorder.snapshot().is_empty(),
            "superseded retry must not unregister the SFU participant"
        );
        let snapshot = actor.ask(GetSnapshot).await.expect("snapshot after sweep");
        assert!(
            snapshot.room.find_occupant_by_real_jid(&jid).is_some(),
            "the replacement session must remain present after a superseded retry"
        );
        assert_eq!(
            state.deps.protocol.pending_local_muc_departures.len(),
            0,
            "a superseded retry converges and drops the retained item"
        );
    }

    #[tokio::test]
    async fn stale_generation_explicit_retry_superseded_by_replacement_unregisters_the_old_sfu_session(
    ) {
        let recorder = Arc::new(RecordingSfu::default());
        let state = create_test_websocket_state_with_sfu(recorder.clone()).await;
        let room = room_jid("superseded-explicit-generation");
        let jid = full_jid("alice@example.com/web");
        let first_generation = OccupancySessionGeneration::mint();
        let second_generation = OccupancySessionGeneration::mint();

        let actor = create_room(state.as_ref(), &room).await;
        let admission_revision = actor
            .ask(GetSnapshot)
            .await
            .expect("initial snapshot")
            .admission_revision;
        actor
            .ask(waddle_xmpp::muc::room_actor::JoinWithAffiliation {
                sender_jid: jid.clone(),
                nick: "alice".to_string(),
                affiliation_grant: waddle_xmpp::muc::room_actor::JoinAffiliationGrant::Resolver(
                    Affiliation::Member,
                ),
                local_domain: "example.com".to_string(),
                admission_revision,
                session: first_generation,
            })
            .await
            .expect("first join");
        let call_id = waddle_sfu::CallId::try_new(room.to_string()).expect("call id");
        let identity = waddle_sfu::Identity::from_jid(jid.clone());
        recorder.register_call_participant_with_session(
            &call_id,
            &identity,
            &waddle_sfu::SessionBinding::try_new("superseded-explicit-g1").expect("sid"),
            first_generation,
        );

        let admission_revision = actor
            .ask(GetSnapshot)
            .await
            .expect("replacement snapshot")
            .admission_revision;
        actor
            .ask(waddle_xmpp::muc::room_actor::JoinWithAffiliation {
                sender_jid: jid.clone(),
                nick: "alice".to_string(),
                affiliation_grant: waddle_xmpp::muc::room_actor::JoinAffiliationGrant::Resolver(
                    Affiliation::Member,
                ),
                local_domain: "example.com".to_string(),
                admission_revision,
                session: second_generation,
            })
            .await
            .expect("replacement join");
        state.deps.protocol.pending_local_muc_departures.record(
            crate::server::routes::websocket::LocalDepartureItem::RoomDeparture {
                room: room.clone(),
                jid: jid.clone(),
                cause: OccupancyLeaveCause::Explicit,
                selector: LeaveSessionSelector::Generation(first_generation),
                attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
                notified: std::collections::HashSet::new(),
                removal: waddle_xmpp::muc::MucRemovalCause::Voluntary,
            },
        );

        run_local_muc_departure_sweep(&state).await;

        let unregisters = recorder.snapshot();
        assert_eq!(
            unregisters.len(),
            1,
            "the superseded explicit retry must tear down the stale generation's SFU registration"
        );
        assert!(
            !recorder.has_call_participant(&call_id, &identity),
            "the stale generation's SFU registration must be removed"
        );
        let snapshot = actor.ask(GetSnapshot).await.expect("snapshot after sweep");
        assert!(
            snapshot.room.find_occupant_by_real_jid(&jid).is_some(),
            "the replacement occupancy must remain after the stale explicit retry"
        );
        assert_eq!(
            state.deps.protocol.pending_local_muc_departures.len(),
            0,
            "the superseded explicit retry must converge"
        );
    }

    #[tokio::test]
    async fn superseded_departure_without_occupancy_retires_only_matching_media() {
        for matching_media in [Some(true), Some(false), None] {
            let recorder = Arc::new(RecordingSfu::default());
            let state = create_test_websocket_state_with_sfu(recorder.clone()).await;
            let room = room_jid("superseded-no-occupancy");
            let jid = full_jid("alice@example.com/web");
            let generation = OccupancySessionGeneration::mint();
            let actor = create_room(state.as_ref(), &room).await;
            let mut attempts = Vec::new();
            let mut successors = Vec::new();
            // One retained item coalesces many nick departures. After the
            // preflight below, 17 unreplayable receipts remain: enough to hit
            // the bounded janitor drain before it can reach NotOccupant.
            for index in 0..18 {
                let nick = format!("shared-nick-{index}");
                actor
                    .ask(Join {
                        session: generation,
                        nick: nick.clone(),
                        real_jid: jid.clone(),
                        role: Role::Participant,
                        affiliation: Affiliation::Member,
                    })
                    .await
                    .expect("join departed generation");
                let attempt = waddle_xmpp::muc::room_actor::LeaveAttemptId::generate();
                assert!(matches!(
                    actor
                        .ask(LeaveByRealJid {
                            sender_jid: jid.clone(),
                            cause: OccupancyLeaveCause::Disconnect,
                            session: LeaveSessionSelector::Generation(generation),
                            attempt,
                            origin: waddle_xmpp::muc::room_actor::LeaveOrigin::Fresh,
                        })
                        .await
                        .expect("commit departure with lost reply"),
                    LeaveDisposition::Left(_)
                ));
                attempts.push(attempt);
                let successor = full_jid(&format!("bob{index}@example.com/web"));
                join_member(&actor, &successor, &nick).await;
                successors.push(successor);
            }
            let attempt = *attempts.last().expect("retained departures");
            assert!(matches!(
                actor
                    .ask(LeaveByRealJid {
                        sender_jid: jid.clone(),
                        cause: OccupancyLeaveCause::Disconnect,
                        session: LeaveSessionSelector::Generation(generation),
                        attempt,
                        origin: waddle_xmpp::muc::room_actor::LeaveOrigin::RetainedRetry,
                    })
                    .await
                    .expect("consume superseded departure receipt"),
                LeaveDisposition::Superseded {
                    current_generation: None
                }
            ));

            let call = waddle_sfu::CallId::try_new(room.to_string()).expect("call id");
            let identity = waddle_sfu::Identity::from_jid(jid.clone());
            match matching_media {
                Some(matching) => recorder.register_call_participant_with_session(
                    &call,
                    &identity,
                    &waddle_sfu::SessionBinding::try_new("retained-media")
                        .expect("session binding"),
                    if matching {
                        generation
                    } else {
                        OccupancySessionGeneration::mint()
                    },
                ),
                None => recorder.register_call_participant(&call, &identity),
            }
            state.deps.protocol.pending_local_muc_departures.record(
                crate::server::routes::websocket::LocalDepartureItem::RoomDeparture {
                    room: room.clone(),
                    jid: jid.clone(),
                    cause: OccupancyLeaveCause::Disconnect,
                    selector: LeaveSessionSelector::Generation(generation),
                    attempt,
                    notified: HashSet::new(),
                    removal: waddle_xmpp::muc::MucRemovalCause::Voluntary,
                },
            );

            run_local_muc_departure_sweep(&state).await;

            assert_eq!(
                recorder.has_call_participant(&call, &identity),
                matching_media != Some(true)
            );
            assert_eq!(
                recorder.snapshot().len(),
                usize::from(matching_media == Some(true))
            );
            let snapshot = actor.ask(GetSnapshot).await.expect("current roster");
            for successor in &successors {
                assert!(snapshot.room.find_occupant_by_real_jid(successor).is_some());
            }
            assert_eq!(state.deps.protocol.pending_local_muc_departures.len(), 0);
        }
    }

    #[tokio::test]
    async fn same_generation_rejoin_supersedes_receipt_without_unregistering_media() {
        let recorder = Arc::new(RecordingSfu::default());
        let state = create_test_websocket_state_with_sfu(recorder.clone()).await;
        let room = room_jid("superseded-same-generation");
        let jid = full_jid("alice@example.com/web");
        let generation = OccupancySessionGeneration::mint();

        let actor = create_room(state.as_ref(), &room).await;
        let admission_revision = actor
            .ask(GetSnapshot)
            .await
            .expect("initial snapshot")
            .admission_revision;
        actor
            .ask(waddle_xmpp::muc::room_actor::JoinWithAffiliation {
                sender_jid: jid.clone(),
                nick: "alice".to_string(),
                affiliation_grant: waddle_xmpp::muc::room_actor::JoinAffiliationGrant::Resolver(
                    Affiliation::Member,
                ),
                local_domain: "example.com".to_string(),
                admission_revision,
                session: generation,
            })
            .await
            .expect("first join");
        let call_id = waddle_sfu::CallId::try_new(room.to_string()).expect("call id");
        let identity = waddle_sfu::Identity::from_jid(jid.clone());

        let attempt = waddle_xmpp::muc::room_actor::LeaveAttemptId::generate();
        assert!(matches!(
            actor
                .ask(LeaveByRealJid {
                    sender_jid: jid.clone(),
                    cause: OccupancyLeaveCause::Explicit,
                    session: LeaveSessionSelector::Generation(generation),
                    attempt,
                    origin: waddle_xmpp::muc::room_actor::LeaveOrigin::Fresh,
                })
                .await
                .expect("committed leave whose response was lost"),
            LeaveDisposition::Left(_)
        ));
        let admission_revision = actor
            .ask(GetSnapshot)
            .await
            .expect("replacement snapshot")
            .admission_revision;
        actor
            .ask(waddle_xmpp::muc::room_actor::JoinWithAffiliation {
                sender_jid: jid.clone(),
                nick: "alice".to_string(),
                affiliation_grant: waddle_xmpp::muc::room_actor::JoinAffiliationGrant::Resolver(
                    Affiliation::Member,
                ),
                local_domain: "example.com".to_string(),
                admission_revision,
                session: generation,
            })
            .await
            .expect("replacement join");
        recorder.register_call_participant_with_session(
            &call_id,
            &identity,
            &waddle_sfu::SessionBinding::try_new("superseded-explicit-g1").expect("sid"),
            generation,
        );
        state.deps.protocol.pending_local_muc_departures.record(
            crate::server::routes::websocket::LocalDepartureItem::RoomDeparture {
                room: room.clone(),
                jid: jid.clone(),
                cause: OccupancyLeaveCause::Explicit,
                selector: LeaveSessionSelector::Generation(generation),
                attempt,
                notified: std::collections::HashSet::new(),
                removal: waddle_xmpp::muc::MucRemovalCause::Voluntary,
            },
        );

        run_local_muc_departure_sweep(&state).await;

        let unregisters = recorder.snapshot();
        assert_eq!(
            unregisters.len(),
            0,
            "same-generation receipt supersession must preserve rejoined media"
        );
        assert!(
            recorder.has_call_participant(&call_id, &identity),
            "same-generation media remains registered"
        );
        let snapshot = actor.ask(GetSnapshot).await.expect("snapshot after sweep");
        assert!(
            snapshot.room.find_occupant_by_real_jid(&jid).is_some(),
            "the replacement occupancy must remain after the stale explicit retry"
        );
        assert_eq!(
            state.deps.protocol.pending_local_muc_departures.len(),
            0,
            "the superseded explicit retry must converge"
        );
    }

    /// A retained retry the room answers with a LIVE `Left` (the session was
    /// removed now, under the generation fence) is evidence about the current
    /// SFU registration: an unbound (restored) one is torn down. Only a
    /// replayed receipt keeps it (`unbound_policy_for_left`).
    #[tokio::test]
    async fn retained_retry_live_left_tears_down_an_unbound_sfu_registration() {
        let recorder = Arc::new(RecordingSfu::default());
        let state = create_test_websocket_state_with_sfu(recorder.clone()).await;
        let room = room_jid("retry-left-unbound");
        let jid = full_jid("alice@example.com/web");
        let generation = OccupancySessionGeneration::mint();

        let actor = create_room(state.as_ref(), &room).await;
        let admission_revision = actor
            .ask(GetSnapshot)
            .await
            .expect("initial snapshot")
            .admission_revision;
        actor
            .ask(waddle_xmpp::muc::room_actor::JoinWithAffiliation {
                sender_jid: jid.clone(),
                nick: "alice".to_string(),
                affiliation_grant: waddle_xmpp::muc::room_actor::JoinAffiliationGrant::Resolver(
                    Affiliation::Member,
                ),
                local_domain: "example.com".to_string(),
                admission_revision,
                session: generation,
            })
            .await
            .expect("join");
        let call_id = waddle_sfu::CallId::try_new(room.to_string()).expect("call id");
        let identity = waddle_sfu::Identity::from_jid(jid.clone());
        recorder.register_call_participant(&call_id, &identity);
        state.deps.protocol.pending_local_muc_departures.record(
            crate::server::routes::websocket::LocalDepartureItem::RoomDeparture {
                room: room.clone(),
                jid: jid.clone(),
                cause: OccupancyLeaveCause::Explicit,
                selector: LeaveSessionSelector::Generation(generation),
                attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
                notified: std::collections::HashSet::new(),
                removal: waddle_xmpp::muc::MucRemovalCause::Voluntary,
            },
        );

        run_local_muc_departure_sweep(&state).await;

        let snapshot = actor.ask(GetSnapshot).await.expect("snapshot after sweep");
        assert!(
            snapshot.room.find_occupant_by_real_jid(&jid).is_none(),
            "the retained retry removes the occupancy"
        );
        assert!(
            !recorder.has_call_participant(&call_id, &identity),
            "a live Left on retry removes the session now, so the restored registration is torn down"
        );
    }

    #[tokio::test]
    async fn failed_receipt_acknowledgement_is_retained_and_retried_by_the_janitor() {
        use crate::server::routes::websocket::{ack_departure_receipt, LocalDepartureItem};
        let store = JanitorProjectionStore::new();
        let state = clustered_state_with_store(store.clone()).await;
        let room = room_jid("ack-retry-janitor");
        let alice = full_jid("alice@example.com/web");
        let bob = full_jid("bob@example.com/phone");
        let actor = create_room(state.as_ref(), &room).await;
        join_member(&actor, &alice, "alice").await;
        join_member(&actor, &bob, "bob").await;
        let (bob_tx, mut bob_rx) = mpsc::channel(8);
        register_test_connection(state.as_ref(), &bob, bob_tx).await;
        while bob_rx.try_recv().is_ok() {}

        // The departure completes and its effects run on the caller side, but
        // the acknowledgement cannot be handed to the actor: a stopped actor
        // stands in for a mailbox that stays saturated past the ack bound.
        let attempt = waddle_xmpp::muc::room_actor::LeaveAttemptId::generate();
        let disposition = actor
            .ask(LeaveByRealJid {
                sender_jid: alice.clone(),
                cause: OccupancyLeaveCause::Explicit,
                session: LeaveSessionSelector::Any,
                attempt,
                origin: waddle_xmpp::muc::room_actor::LeaveOrigin::Fresh,
            })
            .await
            .expect("leave");
        assert!(matches!(disposition, LeaveDisposition::Left(_)));
        let stopped = {
            let stopped = create_room(state.as_ref(), &room_jid("ack-retry-stopped")).await;
            stopped.stop_gracefully().await.expect("stop");
            stopped.wait_for_shutdown().await;
            stopped
        };
        ack_departure_receipt(
            &state.deps.protocol.pending_local_muc_departures,
            &stopped,
            &room,
            &alice,
            attempt,
        )
        .await;
        assert_eq!(
            state.deps.protocol.pending_local_muc_departures.len(),
            1,
            "an undeliverable acknowledgement is retained, never dropped"
        );
        assert!(
            state
                .deps
                .protocol
                .pending_local_muc_departures
                .contains_for_test(&LocalDepartureItem::AckReceipt {
                    room: room.clone(),
                    jid: alice.clone(),
                    attempt,
                    absent_sweeps: 0,
                }),
            "the retained item is the acknowledgement itself, not another leave"
        );
        assert_eq!(
            actor
                .ask(waddle_xmpp::muc::room_actor::GetSnapshot)
                .await
                .expect("snapshot")
                .departures
                .receipts
                .len(),
            1,
            "the real actor still holds the un-acknowledged receipt"
        );

        run_local_muc_departure_sweep(&state).await;

        assert_eq!(
            state.deps.protocol.pending_local_muc_departures.len(),
            0,
            "the janitor hands the acknowledgement to the live actor"
        );
        assert!(
            actor
                .ask(waddle_xmpp::muc::room_actor::GetSnapshot)
                .await
                .expect("snapshot")
                .departures
                .receipts
                .is_empty(),
            "the receipt is dropped once acknowledged"
        );
        // An unrelated later leave of the same gone JID and cause finds nothing
        // to replay: the handled departure's effects are never emitted twice.
        let later = actor
            .ask(LeaveByRealJid {
                sender_jid: alice.clone(),
                cause: OccupancyLeaveCause::Explicit,
                session: LeaveSessionSelector::Any,
                attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
                origin: waddle_xmpp::muc::room_actor::LeaveOrigin::Fresh,
            })
            .await
            .expect("later leave");
        assert!(
            matches!(later, LeaveDisposition::NotOccupant),
            "got {later:?}"
        );
        assert!(
            bob_rx.try_recv().is_err(),
            "nothing was broadcast by the acknowledgement path"
        );
    }

    #[tokio::test]
    async fn owed_acknowledgement_is_delivered_before_a_later_retained_departure_can_replay() {
        use crate::server::routes::websocket::{
            ack_departure_receipt, LocalDepartureItem, PendingLocalDeparture,
        };
        let store = JanitorProjectionStore::new();
        let state = clustered_state_with_store(store.clone()).await;
        let room = room_jid("ack-barrier");
        let alice = full_jid("alice@example.com/web");
        let bob = full_jid("bob@example.com/phone");
        let actor = create_room(state.as_ref(), &room).await;
        join_member(&actor, &alice, "alice").await;
        join_member(&actor, &bob, "bob").await;
        let (bob_tx, mut bob_rx) = mpsc::channel(8);
        register_test_connection(state.as_ref(), &bob, bob_tx).await;
        while bob_rx.try_recv().is_ok() {}

        // Departure A: reply delivered, effects ran, acknowledgement failed
        // (stopped actor stands in for a saturated mailbox) and is retained.
        let attempt_a = waddle_xmpp::muc::room_actor::LeaveAttemptId::generate();
        let first = actor
            .ask(LeaveByRealJid {
                sender_jid: alice.clone(),
                cause: OccupancyLeaveCause::Explicit,
                session: LeaveSessionSelector::Any,
                attempt: attempt_a,
                origin: waddle_xmpp::muc::room_actor::LeaveOrigin::Fresh,
            })
            .await
            .expect("leave A");
        assert!(matches!(first, LeaveDisposition::Left(_)));
        let stopped = create_room(state.as_ref(), &room_jid("ack-barrier-stopped")).await;
        stopped.stop_gracefully().await.expect("stop");
        stopped.wait_for_shutdown().await;
        ack_departure_receipt(
            &state.deps.protocol.pending_local_muc_departures,
            &stopped,
            &room,
            &alice,
            attempt_a,
        )
        .await;
        // Departure B: a later same-cause leave whose ask timed out is
        // retained and due now. Its deadline is EARLIER than the
        // acknowledgement's, so by deadline order it would be processed first
        // in this pass — the pass must still deliver the owed acknowledgement
        // ahead of it.
        let ack_item = LocalDepartureItem::AckReceipt {
            room: room.clone(),
            jid: alice.clone(),
            attempt: attempt_a,
            absent_sweeps: 0,
        };
        let retained_ack = state
            .deps
            .protocol
            .pending_local_muc_departures
            .take_for_test(&ack_item)
            .expect("retained ack");
        state
            .deps
            .protocol
            .pending_local_muc_departures
            .record_pending_for_test(PendingLocalDeparture {
                item: LocalDepartureItem::RoomDeparture {
                    room: room.clone(),
                    jid: alice.clone(),
                    cause: OccupancyLeaveCause::Explicit,
                    selector: LeaveSessionSelector::Any,
                    attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
                    notified: std::collections::HashSet::new(),
                    removal: waddle_xmpp::muc::MucRemovalCause::Voluntary,
                },
                attempts: 1,
                not_before: std::time::Instant::now() - std::time::Duration::from_secs(5),
            });
        state
            .deps
            .protocol
            .pending_local_muc_departures
            .record_pending_for_test(PendingLocalDeparture {
                item: retained_ack.item,
                attempts: retained_ack.attempts,
                not_before: std::time::Instant::now(),
            });
        assert_eq!(state.deps.protocol.pending_local_muc_departures.len(), 2);

        run_local_muc_departure_sweep(&state).await;

        assert!(
            bob_rx.try_recv().is_err(),
            "A's already-emitted effects must not be replayed through B's retry"
        );
        assert_eq!(
            state.deps.protocol.pending_local_muc_departures.len(),
            0,
            "the acknowledgement was delivered first and B converged as NotOccupant"
        );
        assert!(actor
            .ask(waddle_xmpp::muc::room_actor::GetSnapshot)
            .await
            .expect("snapshot")
            .departures
            .receipts
            .is_empty());
    }

    #[tokio::test]
    async fn barrier_delivers_a_backed_off_acknowledgement_before_a_due_departure_retry() {
        use crate::server::routes::websocket::{
            ack_departure_receipt, LocalDepartureItem, PendingLocalDeparture,
        };
        let store = JanitorProjectionStore::new();
        let state = clustered_state_with_store(store.clone()).await;
        let room = room_jid("ack-barrier-backed-off");
        let alice = full_jid("alice@example.com/web");
        let bob = full_jid("bob@example.com/phone");
        let actor = create_room(state.as_ref(), &room).await;
        join_member(&actor, &alice, "alice").await;
        join_member(&actor, &bob, "bob").await;
        let (bob_tx, mut bob_rx) = mpsc::channel(8);
        register_test_connection(state.as_ref(), &bob, bob_tx).await;
        while bob_rx.try_recv().is_ok() {}

        let attempt_a = waddle_xmpp::muc::room_actor::LeaveAttemptId::generate();
        let first = actor
            .ask(LeaveByRealJid {
                sender_jid: alice.clone(),
                cause: OccupancyLeaveCause::Explicit,
                session: LeaveSessionSelector::Any,
                attempt: attempt_a,
                origin: waddle_xmpp::muc::room_actor::LeaveOrigin::Fresh,
            })
            .await
            .expect("leave A");
        assert!(matches!(first, LeaveDisposition::Left(_)));
        let stopped =
            create_room(state.as_ref(), &room_jid("ack-barrier-backed-off-stopped")).await;
        stopped.stop_gracefully().await.expect("stop");
        stopped.wait_for_shutdown().await;
        ack_departure_receipt(
            &state.deps.protocol.pending_local_muc_departures,
            &stopped,
            &room,
            &alice,
            attempt_a,
        )
        .await;
        // The acknowledgement is backed off (not due); only the departure
        // retry is due. The barrier alone must deliver the acknowledgement.
        let ack_item = LocalDepartureItem::AckReceipt {
            room: room.clone(),
            jid: alice.clone(),
            attempt: attempt_a,
            absent_sweeps: 0,
        };
        let retained_ack = state
            .deps
            .protocol
            .pending_local_muc_departures
            .take_for_test(&ack_item)
            .expect("retained ack");
        state
            .deps
            .protocol
            .pending_local_muc_departures
            .record_pending_for_test(PendingLocalDeparture {
                item: retained_ack.item,
                attempts: retained_ack.attempts,
                not_before: std::time::Instant::now() + std::time::Duration::from_secs(3600),
            });
        state.deps.protocol.pending_local_muc_departures.record(
            LocalDepartureItem::RoomDeparture {
                room: room.clone(),
                jid: alice.clone(),
                cause: OccupancyLeaveCause::Explicit,
                selector: LeaveSessionSelector::Any,
                attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
                notified: std::collections::HashSet::new(),
                removal: waddle_xmpp::muc::MucRemovalCause::Voluntary,
            },
        );

        run_local_muc_departure_sweep(&state).await;

        assert!(
            bob_rx.try_recv().is_err(),
            "the departure retry must not replay A's effects"
        );
        assert_eq!(
            state.deps.protocol.pending_local_muc_departures.len(),
            0,
            "the barrier delivered the backed-off acknowledgement and B converged"
        );
        assert!(actor
            .ask(waddle_xmpp::muc::room_actor::GetSnapshot)
            .await
            .expect("snapshot")
            .departures
            .receipts
            .is_empty());
    }

    #[tokio::test]
    async fn barrier_clears_every_owed_acknowledgement_not_just_one() {
        use crate::server::routes::websocket::{
            ack_departure_receipt, LocalDepartureItem, PendingLocalDeparture,
        };
        let store = JanitorProjectionStore::new();
        let state = clustered_state_with_store(store.clone()).await;
        let room = room_jid("ack-barrier-multi");
        let alice = full_jid("alice@example.com/web");
        let bob = full_jid("bob@example.com/phone");
        let actor = create_room(state.as_ref(), &room).await;
        join_member(&actor, &alice, "alice").await;
        join_member(&actor, &bob, "bob").await;
        let (bob_tx, mut bob_rx) = mpsc::channel(8);
        register_test_connection(state.as_ref(), &bob, bob_tx).await;
        while bob_rx.try_recv().is_ok() {}

        let stopped = create_room(state.as_ref(), &room_jid("ack-barrier-multi-stopped")).await;
        stopped.stop_gracefully().await.expect("stop");
        stopped.wait_for_shutdown().await;
        // Two departures of the same JID (leave, rejoin, leave), both acks
        // retained: the actor keeps only the newer receipt, so the stale ack
        // is a no-op and the live one must still be delivered before any
        // departure retry.
        let mut retained_acks = Vec::new();
        for _ in 0..2 {
            let attempt = waddle_xmpp::muc::room_actor::LeaveAttemptId::generate();
            let left = actor
                .ask(LeaveByRealJid {
                    sender_jid: alice.clone(),
                    cause: OccupancyLeaveCause::Explicit,
                    session: LeaveSessionSelector::Any,
                    attempt,
                    origin: waddle_xmpp::muc::room_actor::LeaveOrigin::Fresh,
                })
                .await
                .expect("leave");
            assert!(matches!(left, LeaveDisposition::Left(_)));
            ack_departure_receipt(
                &state.deps.protocol.pending_local_muc_departures,
                &stopped,
                &room,
                &alice,
                attempt,
            )
            .await;
            let ack_item = LocalDepartureItem::AckReceipt {
                room: room.clone(),
                jid: alice.clone(),
                attempt,
                absent_sweeps: 0,
            };
            retained_acks.push(
                state
                    .deps
                    .protocol
                    .pending_local_muc_departures
                    .take_for_test(&ack_item)
                    .expect("retained ack"),
            );
            if retained_acks.len() == 1 {
                join_member(&actor, &alice, "alice").await;
                while bob_rx.try_recv().is_ok() {}
            }
        }
        for retained in retained_acks {
            state
                .deps
                .protocol
                .pending_local_muc_departures
                .record_pending_for_test(PendingLocalDeparture {
                    item: retained.item,
                    attempts: retained.attempts,
                    not_before: std::time::Instant::now() + std::time::Duration::from_secs(3600),
                });
        }
        state.deps.protocol.pending_local_muc_departures.record(
            LocalDepartureItem::RoomDeparture {
                room: room.clone(),
                jid: alice.clone(),
                cause: OccupancyLeaveCause::Explicit,
                selector: LeaveSessionSelector::Any,
                attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
                notified: std::collections::HashSet::new(),
                removal: waddle_xmpp::muc::MucRemovalCause::Voluntary,
            },
        );
        assert_eq!(state.deps.protocol.pending_local_muc_departures.len(), 3);

        run_local_muc_departure_sweep(&state).await;

        assert!(bob_rx.try_recv().is_err(), "no replayed departure effects");
        assert_eq!(
            state.deps.protocol.pending_local_muc_departures.len(),
            0,
            "both acknowledgements delivered, departure converged"
        );
        assert!(actor
            .ask(waddle_xmpp::muc::room_actor::GetSnapshot)
            .await
            .expect("snapshot")
            .departures
            .receipts
            .is_empty());
    }

    #[tokio::test]
    async fn owed_acknowledgement_for_an_absent_room_is_retried_before_being_dropped() {
        use crate::server::routes::websocket::{
            LocalDepartureItem, PendingLocalDeparture, ACK_ABSENT_ROOM_RETRIES,
        };
        let store = JanitorProjectionStore::new();
        let state = clustered_state_with_store(store.clone()).await;
        let room = room_jid("ack-absent-room");
        let alice = full_jid("alice@example.com/web");
        let attempt = waddle_xmpp::muc::room_actor::LeaveAttemptId::generate();
        let ack = || LocalDepartureItem::AckReceipt {
            room: room.clone(),
            jid: alice.clone(),
            attempt,
            absent_sweeps: 0,
        };
        // Fresh ack, room not registered (handoff window): retained.
        state
            .deps
            .protocol
            .pending_local_muc_departures
            .record(ack());
        run_local_muc_departure_sweep(&state).await;
        assert_eq!(
            state.deps.protocol.pending_local_muc_departures.len(),
            1,
            "an absent room does not drop a fresh acknowledgement"
        );
        // Past the absent-room budget: dropped (the room is gone for good).
        state
            .deps
            .protocol
            .pending_local_muc_departures
            .take_for_test(&ack())
            .expect("retained ack");
        // Generic retry attempts (ask timeouts, NotAuthoritative answers) do
        // not consume the absent-room budget.
        state
            .deps
            .protocol
            .pending_local_muc_departures
            .record_pending_for_test(PendingLocalDeparture {
                item: ack(),
                attempts: ACK_ABSENT_ROOM_RETRIES + 5,
                not_before: std::time::Instant::now(),
            });
        run_local_muc_departure_sweep(&state).await;
        assert_eq!(state.deps.protocol.pending_local_muc_departures.len(), 1);
        state
            .deps
            .protocol
            .pending_local_muc_departures
            .take_for_test(&ack())
            .expect("retained ack");
        state
            .deps
            .protocol
            .pending_local_muc_departures
            .record_pending_for_test(PendingLocalDeparture {
                item: LocalDepartureItem::AckReceipt {
                    room: room.clone(),
                    jid: alice.clone(),
                    attempt,
                    absent_sweeps: ACK_ABSENT_ROOM_RETRIES,
                },
                attempts: 0,
                not_before: std::time::Instant::now(),
            });
        run_local_muc_departure_sweep(&state).await;
        assert_eq!(state.deps.protocol.pending_local_muc_departures.len(), 0);
    }

    #[tokio::test]
    async fn write_ahead_departure_is_replayed_when_the_live_task_dies_before_its_effects() {
        use crate::server::routes::websocket::{LocalDepartureItem, PendingLocalDeparture};
        let store = JanitorProjectionStore::new();
        let state = clustered_state_with_store(store.clone()).await;
        let room = room_jid("write-ahead-replay");
        let alice = full_jid("alice@example.com/web");
        let bob = full_jid("bob@example.com/phone");
        let actor = create_room(state.as_ref(), &room).await;
        join_member(&actor, &alice, "alice").await;
        join_member(&actor, &bob, "bob").await;
        let (bob_tx, mut bob_rx) = mpsc::channel(8);
        register_test_connection(state.as_ref(), &bob, bob_tx).await;
        while bob_rx.try_recv().is_ok() {}

        // The explicit-leave task: write-ahead, ask, then it is cancelled
        // before fan-out and acknowledgement.
        let attempt = waddle_xmpp::muc::room_actor::LeaveAttemptId::generate();
        let in_flight = LocalDepartureItem::InFlight {
            room: room.clone(),
            jid: alice.clone(),
            cause: OccupancyLeaveCause::Explicit,
            selector: LeaveSessionSelector::Any,
            attempt,
            notified: std::collections::HashSet::new(),
            removal: waddle_xmpp::muc::MucRemovalCause::Voluntary,
        };
        state
            .deps
            .protocol
            .pending_local_muc_departures
            .record_in_flight(in_flight.clone());
        let left = actor
            .ask(LeaveByRealJid {
                sender_jid: alice.clone(),
                cause: OccupancyLeaveCause::Explicit,
                session: LeaveSessionSelector::Any,
                attempt,
                origin: waddle_xmpp::muc::room_actor::LeaveOrigin::Fresh,
            })
            .await
            .expect("leave");
        assert!(matches!(left, LeaveDisposition::Left(_)));
        assert!(
            bob_rx.try_recv().is_err(),
            "no effects ran: the task died before fan-out"
        );
        // Not due yet (the live task normally completes it first).
        run_local_muc_departure_sweep(&state).await;
        assert_eq!(state.deps.protocol.pending_local_muc_departures.len(), 1);
        assert!(bob_rx.try_recv().is_err());
        // Make it due: the janitor replays the retained outcome.
        let retained = state
            .deps
            .protocol
            .pending_local_muc_departures
            .take_for_test(&in_flight)
            .expect("write-ahead entry");
        state
            .deps
            .protocol
            .pending_local_muc_departures
            .record_pending_for_test(PendingLocalDeparture {
                item: retained.item,
                attempts: retained.attempts,
                not_before: std::time::Instant::now(),
            });
        run_local_muc_departure_sweep(&state).await;

        let unavailable = bob_rx.try_recv().expect("replayed unavailable reaches bob");
        let waddle_xmpp::Stanza::Presence(presence) = unavailable.stanza else {
            panic!("expected presence broadcast");
        };
        assert_eq!(presence.type_, xmpp_parsers::presence::Type::Unavailable);
        assert_eq!(state.deps.protocol.pending_local_muc_departures.len(), 0);
        assert!(actor
            .ask(waddle_xmpp::muc::room_actor::GetSnapshot)
            .await
            .expect("snapshot")
            .departures
            .receipts
            .is_empty());
    }

    #[tokio::test]
    async fn completed_explicit_leave_leaves_no_write_ahead_entry() {
        use crate::server::routes::websocket::LocalDepartureItem;
        let store = JanitorProjectionStore::new();
        let state = clustered_state_with_store(store.clone()).await;
        let room = room_jid("write-ahead-complete");
        let alice = full_jid("alice@example.com/web");
        let actor = create_room(state.as_ref(), &room).await;
        join_member(&actor, &alice, "alice").await;
        let attempt = waddle_xmpp::muc::room_actor::LeaveAttemptId::generate();
        let in_flight = LocalDepartureItem::InFlight {
            room: room.clone(),
            jid: alice.clone(),
            cause: OccupancyLeaveCause::Explicit,
            selector: LeaveSessionSelector::Any,
            attempt,
            notified: std::collections::HashSet::new(),
            removal: waddle_xmpp::muc::MucRemovalCause::Voluntary,
        };
        state
            .deps
            .protocol
            .pending_local_muc_departures
            .record_in_flight(in_flight.clone());
        state
            .deps
            .protocol
            .pending_local_muc_departures
            .complete_in_flight(&in_flight);
        assert_eq!(state.deps.protocol.pending_local_muc_departures.len(), 0);
        // A newer attempt merged under the same key survives completion of
        // the older one.
        state
            .deps
            .protocol
            .pending_local_muc_departures
            .record_in_flight(in_flight.clone());
        state
            .deps
            .protocol
            .pending_local_muc_departures
            .record_in_flight(LocalDepartureItem::InFlight {
                room: room.clone(),
                jid: alice.clone(),
                cause: OccupancyLeaveCause::Explicit,
                selector: LeaveSessionSelector::Any,
                attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
                notified: std::collections::HashSet::new(),
                removal: waddle_xmpp::muc::MucRemovalCause::Voluntary,
            });
        state
            .deps
            .protocol
            .pending_local_muc_departures
            .complete_in_flight(&in_flight);
        assert_eq!(state.deps.protocol.pending_local_muc_departures.len(), 1);
        // A retained departure under the ROOM-scoped key is never touched by
        // in-flight bookkeeping.
        state.deps.protocol.pending_local_muc_departures.record(
            LocalDepartureItem::RoomDeparture {
                room: room.clone(),
                jid: alice.clone(),
                cause: OccupancyLeaveCause::Explicit,
                selector: LeaveSessionSelector::Any,
                attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
                notified: std::collections::HashSet::new(),
                removal: waddle_xmpp::muc::MucRemovalCause::Voluntary,
            },
        );
        assert_eq!(state.deps.protocol.pending_local_muc_departures.len(), 2);
        let _ = actor;
    }

    #[tokio::test]
    async fn task_cancelled_after_effects_leaves_only_an_acknowledgement_to_deliver() {
        use crate::server::routes::websocket::{LocalDepartureItem, PendingLocalDeparture};
        let store = JanitorProjectionStore::new();
        let state = clustered_state_with_store(store.clone()).await;
        let room = room_jid("cancel-after-effects");
        let alice = full_jid("alice@example.com/web");
        let bob = full_jid("bob@example.com/phone");
        let actor = create_room(state.as_ref(), &room).await;
        join_member(&actor, &alice, "alice").await;
        join_member(&actor, &bob, "bob").await;
        let (bob_tx, mut bob_rx) = mpsc::channel(8);
        register_test_connection(state.as_ref(), &bob, bob_tx).await;
        while bob_rx.try_recv().is_ok() {}

        // Live task: write-ahead, ask, effects ran (simulated), then it
        // converts its entry into the owed ack and dies before awaiting it.
        let attempt = waddle_xmpp::muc::room_actor::LeaveAttemptId::generate();
        let in_flight = LocalDepartureItem::InFlight {
            room: room.clone(),
            jid: alice.clone(),
            cause: OccupancyLeaveCause::Explicit,
            selector: LeaveSessionSelector::Any,
            attempt,
            notified: std::collections::HashSet::new(),
            removal: waddle_xmpp::muc::MucRemovalCause::Voluntary,
        };
        state
            .deps
            .protocol
            .pending_local_muc_departures
            .record_in_flight(in_flight.clone());
        let LeaveDisposition::Left(outcome) = actor
            .ask(LeaveByRealJid {
                sender_jid: alice.clone(),
                cause: OccupancyLeaveCause::Explicit,
                session: LeaveSessionSelector::Any,
                attempt,
                origin: waddle_xmpp::muc::room_actor::LeaveOrigin::Fresh,
            })
            .await
            .expect("leave")
        else {
            panic!("alice leaves");
        };
        state
            .deps
            .protocol
            .pending_local_muc_departures
            .convert_in_flight_to_ack(&in_flight, outcome.acknowledge);

        // Nothing is due yet; then make the owed ack due and sweep.
        run_local_muc_departure_sweep(&state).await;
        assert_eq!(state.deps.protocol.pending_local_muc_departures.len(), 1);
        let ack_item = LocalDepartureItem::AckReceipt {
            room: room.clone(),
            jid: alice.clone(),
            attempt: outcome.acknowledge,
            absent_sweeps: 0,
        };
        let retained = state
            .deps
            .protocol
            .pending_local_muc_departures
            .take_for_test(&ack_item)
            .expect("owed ack");
        state
            .deps
            .protocol
            .pending_local_muc_departures
            .record_pending_for_test(PendingLocalDeparture {
                item: retained.item,
                attempts: retained.attempts,
                not_before: std::time::Instant::now(),
            });
        run_local_muc_departure_sweep(&state).await;

        assert!(
            bob_rx.try_recv().is_err(),
            "the completed departure is acknowledged, never replayed"
        );
        assert_eq!(state.deps.protocol.pending_local_muc_departures.len(), 0);
        assert!(actor
            .ask(waddle_xmpp::muc::room_actor::GetSnapshot)
            .await
            .expect("snapshot")
            .departures
            .receipts
            .is_empty());
    }

    #[tokio::test]
    async fn resumed_departure_skips_recipients_the_dead_task_already_notified() {
        use crate::server::routes::websocket::{LocalDepartureItem, PendingLocalDeparture};
        let store = JanitorProjectionStore::new();
        let state = clustered_state_with_store(store.clone()).await;
        let room = room_jid("resume-fan-out");
        let alice = full_jid("alice@example.com/web");
        let bob = full_jid("bob@example.com/phone");
        let carol = full_jid("carol@example.com/tablet");
        let actor = create_room(state.as_ref(), &room).await;
        join_member(&actor, &alice, "alice").await;
        join_member(&actor, &bob, "bob").await;
        join_member(&actor, &carol, "carol").await;
        let (bob_tx, mut bob_rx) = mpsc::channel(8);
        let (carol_tx, mut carol_rx) = mpsc::channel(8);
        register_test_connection(state.as_ref(), &bob, bob_tx).await;
        register_test_connection(state.as_ref(), &carol, carol_tx).await;
        while bob_rx.try_recv().is_ok() {}
        while carol_rx.try_recv().is_ok() {}

        // The live task committed the departure and notified bob, then died.
        let attempt = waddle_xmpp::muc::room_actor::LeaveAttemptId::generate();
        let in_flight = LocalDepartureItem::InFlight {
            room: room.clone(),
            jid: alice.clone(),
            cause: OccupancyLeaveCause::Disconnect,
            selector: LeaveSessionSelector::Any,
            attempt,
            notified: std::collections::HashSet::new(),
            removal: waddle_xmpp::muc::MucRemovalCause::Voluntary,
        };
        state
            .deps
            .protocol
            .pending_local_muc_departures
            .record_in_flight(in_flight.clone());
        let LeaveDisposition::Left(outcome) = actor
            .ask(LeaveByRealJid {
                sender_jid: alice.clone(),
                cause: OccupancyLeaveCause::Disconnect,
                session: LeaveSessionSelector::Any,
                attempt,
                origin: waddle_xmpp::muc::room_actor::LeaveOrigin::Fresh,
            })
            .await
            .expect("leave")
        else {
            panic!("alice leaves");
        };
        // The live task's fan-out reaches bob through the production helper
        // (recording bob on its entry) and dies before carol: simulated by
        // skipping carol.
        let skip_carol = HashSet::from([carol.clone()]);
        crate::server::routes::websocket::broadcast_muc_leave_to_remaining_resumable(
            &state,
            &room,
            &alice,
            &outcome,
            waddle_xmpp::muc::MucRemovalCause::Voluntary,
            Some(crate::server::routes::websocket::LeaveFanOutProgress {
                skip: &skip_carol,
                record: Some((
                    &state.deps.protocol.pending_local_muc_departures,
                    &in_flight,
                )),
            }),
        )
        .await;
        let first = bob_rx
            .try_recv()
            .expect("bob got the live task's unavailable");
        assert!(matches!(first.stanza, waddle_xmpp::Stanza::Presence(_)));
        assert!(
            carol_rx.try_recv().is_err(),
            "the dead task never reached carol"
        );
        // Make the write-ahead entry due (lease gone).
        let retained = state
            .deps
            .protocol
            .pending_local_muc_departures
            .take_for_test(&in_flight)
            .expect("in-flight entry");
        assert!(matches!(
            &retained.item,
            LocalDepartureItem::InFlight { notified, .. } if notified == &HashSet::from([bob.clone()])
        ));
        state
            .deps
            .protocol
            .pending_local_muc_departures
            .record_pending_for_test(PendingLocalDeparture {
                item: retained.item,
                attempts: retained.attempts,
                not_before: std::time::Instant::now(),
            });

        run_local_muc_departure_sweep(&state).await;

        assert!(
            bob_rx.try_recv().is_err(),
            "bob was already notified by the dead task: no second unavailable"
        );
        let unavailable = carol_rx
            .try_recv()
            .expect("carol gets the resumed unavailable");
        let waddle_xmpp::Stanza::Presence(presence) = unavailable.stanza else {
            panic!("expected presence");
        };
        assert_eq!(presence.type_, xmpp_parsers::presence::Type::Unavailable);
        assert_eq!(state.deps.protocol.pending_local_muc_departures.len(), 0);
    }

    #[tokio::test]
    async fn retained_empty_room_eviction_actually_destroys_the_live_empty_room() {
        use crate::server::routes::websocket::LocalDepartureItem;
        let state = create_test_websocket_state().await;
        let room = room_jid("evict-live-empty");
        let actor = state
            .deps
            .protocol
            .room_registry
            .ask(CreateRoom {
                room_jid: room.clone(),
                waddle_id: "w".to_string(),
                channel_id: "c".to_string(),
                config: RoomConfig {
                    persistent: false,
                    ..RoomConfig::default()
                },
            })
            .await
            .expect("create room");
        let occupancy_revision = actor
            .ask(GetSnapshot)
            .await
            .expect("snapshot")
            .occupancy_revision;
        state.deps.protocol.pending_local_muc_departures.record(
            LocalDepartureItem::EvictEmptyRoom {
                room: room.clone(),
                occupancy_revision,
            },
        );

        run_local_muc_departure_sweep(&state).await;

        assert_eq!(state.deps.protocol.pending_local_muc_departures.len(), 0);
        assert!(
            state
                .deps
                .protocol
                .room_registry
                .ask(waddle_xmpp::muc::room_registry_actor::GetRoom {
                    room_jid: room.clone(),
                })
                .await
                .expect("registry lookup")
                .is_none(),
            "the retained eviction must actually destroy the empty room"
        );
    }

    #[tokio::test]
    async fn retained_empty_room_eviction_converges_when_the_registry_answers() {
        use crate::server::routes::websocket::LocalDepartureItem;
        let state = create_test_websocket_state().await;
        let room = room_jid("evict-retained");
        state.deps.protocol.pending_local_muc_departures.record(
            LocalDepartureItem::EvictEmptyRoom {
                room,
                occupancy_revision: 1,
            },
        );
        assert_eq!(state.deps.protocol.pending_local_muc_departures.len(), 1);
        run_local_muc_departure_sweep(&state).await;
        assert_eq!(
            state.deps.protocol.pending_local_muc_departures.len(),
            0,
            "an absent room is a definitive answer: the owed destroy converges"
        );
    }

    #[tokio::test]
    async fn fresh_leave_never_consumes_an_unacknowledged_receipt_but_a_retained_retry_does() {
        // While an acknowledgement is still in flight (or retained for the
        // janitor), an unrelated fresh leave of the same gone JID and cause
        // must find nothing to replay; only a retained retry may consume it.
        let store = JanitorProjectionStore::new();
        let state = clustered_state_with_store(store.clone()).await;
        let room = room_jid("ack-window");
        let alice = full_jid("alice@example.com/web");
        let actor = create_room(state.as_ref(), &room).await;
        join_member(&actor, &alice, "alice").await;
        let attempt = waddle_xmpp::muc::room_actor::LeaveAttemptId::generate();
        let first = actor
            .ask(LeaveByRealJid {
                sender_jid: alice.clone(),
                cause: OccupancyLeaveCause::Explicit,
                session: LeaveSessionSelector::Any,
                attempt,
                origin: waddle_xmpp::muc::room_actor::LeaveOrigin::Fresh,
            })
            .await
            .expect("leave");
        assert!(matches!(first, LeaveDisposition::Left(_)));
        let fresh_later = actor
            .ask(LeaveByRealJid {
                sender_jid: alice.clone(),
                cause: OccupancyLeaveCause::Explicit,
                session: LeaveSessionSelector::Any,
                attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
                origin: waddle_xmpp::muc::room_actor::LeaveOrigin::Fresh,
            })
            .await
            .expect("later fresh leave");
        assert!(
            matches!(fresh_later, LeaveDisposition::NotOccupant),
            "got {fresh_later:?}"
        );
        let retry = actor
            .ask(LeaveByRealJid {
                sender_jid: alice.clone(),
                cause: OccupancyLeaveCause::Explicit,
                session: LeaveSessionSelector::Any,
                attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
                origin: waddle_xmpp::muc::room_actor::LeaveOrigin::RetainedRetry,
            })
            .await
            .expect("retained retry");
        assert!(matches!(retry, LeaveDisposition::Left(_)), "got {retry:?}");
    }

    #[tokio::test]
    async fn janitor_suppressed_explicit_replay_unregisters_sfu_exactly_once() {
        let recorder = Arc::new(RecordingSfu::default());
        let state = create_test_websocket_state_with_sfu(recorder.clone()).await;
        let room = room_jid("suppressed-explicit-janitor");
        let alice = full_jid("alice@example.com/web");
        let bob = full_jid("bob@example.com/phone");
        let actor = create_room(state.as_ref(), &room).await;
        join_member(&actor, &alice, "alice").await;
        join_member(&actor, &bob, "bob").await;

        let (alice_tx, mut alice_rx) = mpsc::channel(8);
        let (bob_tx, mut bob_rx) = mpsc::channel(8);
        register_test_connection(state.as_ref(), &alice, alice_tx).await;
        register_test_connection(state.as_ref(), &bob, bob_tx).await;
        while alice_rx.try_recv().is_ok() {}
        while bob_rx.try_recv().is_ok() {}

        actor
            .ask(SealForDestroy {
                attempt: waddle_xmpp::muc::DestroyAttemptId::generate(),
            })
            .await
            .expect("seal destroying");
        let attempt = waddle_xmpp::muc::room_actor::LeaveAttemptId::generate();
        assert!(matches!(
            actor
                .ask(LeaveByRealJid {
                    sender_jid: alice.clone(),
                    cause: OccupancyLeaveCause::Explicit,
                    session: LeaveSessionSelector::Any,
                    attempt,
                    origin: waddle_xmpp::muc::room_actor::LeaveOrigin::Fresh,
                })
                .await
                .expect("suppressed explicit leave"),
            LeaveDisposition::Suppressed {
                ref nick,
                affiliation: Affiliation::Member,
                ..
            } if nick.as_str() == "alice"
        ));
        state.deps.protocol.pending_local_muc_departures.record(
            crate::server::routes::websocket::LocalDepartureItem::RoomDeparture {
                room: room.clone(),
                jid: alice.clone(),
                cause: OccupancyLeaveCause::Explicit,
                selector: LeaveSessionSelector::Any,
                attempt,
                notified: std::collections::HashSet::new(),
                removal: waddle_xmpp::muc::MucRemovalCause::Voluntary,
            },
        );

        run_local_muc_departure_sweep(&state).await;

        let alice_unavailable = alice_rx.try_recv().expect("self unavailable");
        let xml = crate::server::routes::websocket::stanza_to_xml(&alice_unavailable.stanza);
        assert!(xml.contains("type='unavailable'"), "{xml}");
        assert!(xml.contains("status code='110'"), "{xml}");
        assert!(xml.contains("affiliation='member'"), "{xml}");
        assert!(
            alice_rx.try_recv().is_err(),
            "the replayed receipt must self-echo exactly once"
        );
        assert!(
            bob_rx.try_recv().is_err(),
            "suppressed replay must not fan out to other occupants"
        );
        assert_eq!(
            state.deps.protocol.pending_local_muc_departures.len(),
            0,
            "the replayed suppressed receipt must be consumed"
        );
        let unregisters = recorder.snapshot();
        assert_eq!(unregisters.len(), 1, "suppressed replay unregisters once");
        assert_eq!(unregisters[0].0.as_str(), room.as_str());
        assert_eq!(unregisters[0].1.as_livekit_identity(), alice.as_str());
    }

    #[tokio::test]
    async fn second_disconnect_after_rejoin_is_not_superseded_by_the_older_watermark() {
        let store = JanitorProjectionStore::new();
        let state = clustered_state_with_store(store.clone()).await;
        let room = room_jid("second-disconnect-rejoin");
        let alice = full_jid("alice@example.com/web");
        let bob = full_jid("bob@example.com/phone");
        let actor = create_room(state.as_ref(), &room).await;
        join_member(&actor, &alice, "alice").await;
        join_member(&actor, &bob, "bob").await;
        let (bob_tx, mut bob_rx) = mpsc::channel(8);
        register_test_connection(state.as_ref(), &bob, bob_tx).await;
        while bob_rx.try_recv().is_ok() {}

        store.set_leave_mode(LeaveProjectionMode::OwnershipUnavailable);
        let first = match actor
            .ask(LeaveByRealJid {
                sender_jid: alice.clone(),
                cause: OccupancyLeaveCause::Disconnect,
                session: LeaveSessionSelector::Any,
                attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
                origin: waddle_xmpp::muc::room_actor::LeaveOrigin::Fresh,
            })
            .await
            .expect("first disconnect")
        {
            LeaveDisposition::Deferred { watermark } => watermark,
            other => panic!("expected first deferred leave, got {other:?}"),
        };
        state.deps.protocol.pending_local_muc_departures.record(
            crate::server::routes::websocket::LocalDepartureItem::RoomDeparture {
                room: room.clone(),
                jid: alice.clone(),
                cause: OccupancyLeaveCause::Disconnect,
                selector: LeaveSessionSelector::JoinedAtOrBefore(first),
                attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
                notified: std::collections::HashSet::new(),
                removal: waddle_xmpp::muc::MucRemovalCause::Voluntary,
            },
        );
        actor
            .ask(waddle_xmpp::muc::room_actor::JoinWithAffiliation {
                sender_jid: alice.clone(),
                nick: "alice".to_string(),
                affiliation_grant: waddle_xmpp::muc::room_actor::JoinAffiliationGrant::Resolver(
                    Affiliation::Member,
                ),
                local_domain: "example.com".to_string(),
                admission_revision: actor
                    .ask(GetSnapshot)
                    .await
                    .expect("snapshot before rejoin")
                    .admission_revision,
                session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            })
            .await
            .expect("replacement rejoin");
        let second = match actor
            .ask(LeaveByRealJid {
                sender_jid: alice.clone(),
                cause: OccupancyLeaveCause::Disconnect,
                session: LeaveSessionSelector::Any,
                attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
                origin: waddle_xmpp::muc::room_actor::LeaveOrigin::Fresh,
            })
            .await
            .expect("second disconnect")
        {
            LeaveDisposition::Deferred { watermark } => watermark,
            other => panic!("expected second deferred leave, got {other:?}"),
        };
        assert!(
            second > first,
            "a replacement join must advance its watermark"
        );
        state.deps.protocol.pending_local_muc_departures.record(
            crate::server::routes::websocket::LocalDepartureItem::RoomDeparture {
                room: room.clone(),
                jid: alice.clone(),
                cause: OccupancyLeaveCause::Disconnect,
                selector: LeaveSessionSelector::JoinedAtOrBefore(second),
                attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
                notified: std::collections::HashSet::new(),
                removal: waddle_xmpp::muc::MucRemovalCause::Voluntary,
            },
        );

        store.set_leave_mode(LeaveProjectionMode::Succeed);
        run_local_muc_departure_sweep(&state).await;

        assert!(
            bob_rx.try_recv().is_ok(),
            "the newest retry must fan out once"
        );
        assert!(
            bob_rx.try_recv().is_err(),
            "the departure must fan out exactly once"
        );
        assert!(
            actor
                .ask(GetSnapshot)
                .await
                .expect("snapshot")
                .room
                .find_occupant_by_real_jid(&alice)
                .is_none(),
            "the newest watermark must remove the rejoined session"
        );
        assert_eq!(state.deps.protocol.pending_local_muc_departures.len(), 0);
    }

    #[tokio::test]
    async fn deferred_generation_retry_preserves_the_occupant_selector() {
        let store = JanitorProjectionStore::new();
        let state = clustered_state_with_store(store.clone()).await;
        let room = room_jid("deferred-generation-selector");
        let alice = full_jid("alice@example.com/web");
        let actor = create_room(state.as_ref(), &room).await;
        let generation = waddle_xmpp_core::OccupancySessionGeneration::mint();
        let admission_revision = actor
            .ask(GetSnapshot)
            .await
            .expect("snapshot before join")
            .admission_revision;
        actor
            .ask(waddle_xmpp::muc::room_actor::JoinWithAffiliation {
                sender_jid: alice.clone(),
                nick: "alice".to_string(),
                affiliation_grant: waddle_xmpp::muc::room_actor::JoinAffiliationGrant::Resolver(
                    Affiliation::Member,
                ),
                local_domain: "example.com".to_string(),
                admission_revision,
                session: generation,
            })
            .await
            .expect("join occupant");

        store.set_leave_mode(LeaveProjectionMode::OwnershipUnavailable);
        state.deps.protocol.pending_local_muc_departures.record(
            crate::server::routes::websocket::LocalDepartureItem::RoomDeparture {
                room,
                jid: alice,
                cause: OccupancyLeaveCause::Disconnect,
                selector: LeaveSessionSelector::Generation(generation),
                attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
                notified: std::collections::HashSet::new(),
                removal: waddle_xmpp::muc::MucRemovalCause::Voluntary,
            },
        );

        run_local_muc_departure_sweep(&state).await;

        let retained = state
            .deps
            .protocol
            .pending_local_muc_departures
            .take_due(std::time::Instant::now() + std::time::Duration::from_secs(60));
        assert_eq!(retained.len(), 1);
        assert!(matches!(
            retained[0].item,
            crate::server::routes::websocket::LocalDepartureItem::RoomDeparture {
                selector: LeaveSessionSelector::Generation(found),
                ..
            } if found == generation
        ));
    }

    /// #1647 (codex round 27): one inventory entry can stand for several owed
    /// receipts (the actor retains them per (JID, nick); the inventory
    /// coalesces per (room, JID, cause)). The janitor drains every matching
    /// receipt instead of acknowledging only the newest and stranding the
    /// older nick's fan-out behind a permanent dormancy veto.
    #[tokio::test]
    async fn janitor_entry_drains_every_owed_receipt_for_its_jid_and_cause() {
        let state = create_test_websocket_state().await;
        let room = room_jid("drain-owed-receipts");
        let jid = full_jid("alice@example.com/web");
        let actor = create_room(state.as_ref(), &room).await;
        join_member(&actor, &jid, "old-nick").await;
        let old_attempt = waddle_xmpp::muc::room_actor::LeaveAttemptId::generate();
        assert!(matches!(
            actor
                .ask(waddle_xmpp::muc::room_actor::LeaveByRealJid {
                    sender_jid: jid.clone(),
                    cause: OccupancyLeaveCause::Explicit,
                    session: LeaveSessionSelector::Any,
                    attempt: old_attempt,
                    origin: waddle_xmpp::muc::room_actor::LeaveOrigin::Fresh,
                })
                .await
                .expect("old-nick leave"),
            waddle_xmpp::muc::room_actor::LeaveDisposition::Left(_)
        ));
        join_member(&actor, &jid, "new-nick").await;
        let new_attempt = waddle_xmpp::muc::room_actor::LeaveAttemptId::generate();
        assert!(matches!(
            actor
                .ask(waddle_xmpp::muc::room_actor::LeaveByRealJid {
                    sender_jid: jid.clone(),
                    cause: OccupancyLeaveCause::Explicit,
                    session: LeaveSessionSelector::Any,
                    attempt: new_attempt,
                    origin: waddle_xmpp::muc::room_actor::LeaveOrigin::Fresh,
                })
                .await
                .expect("new-nick leave"),
            waddle_xmpp::muc::room_actor::LeaveDisposition::Left(_)
        ));
        join_member(&actor, &jid, "third-nick").await;
        let third_attempt = waddle_xmpp::muc::room_actor::LeaveAttemptId::generate();
        assert!(matches!(
            actor
                .ask(waddle_xmpp::muc::room_actor::LeaveByRealJid {
                    sender_jid: jid.clone(),
                    cause: OccupancyLeaveCause::Explicit,
                    session: LeaveSessionSelector::Any,
                    attempt: third_attempt,
                    origin: waddle_xmpp::muc::room_actor::LeaveOrigin::Fresh,
                })
                .await
                .expect("third-nick leave"),
            waddle_xmpp::muc::room_actor::LeaveDisposition::Left(_)
        ));
        // Another account re-takes the OLDEST nick before the retry: its
        // receipt is unreplayable (Superseded consumes it), and the drain
        // must continue past it to the still-owed receipts behind it.
        let bob = full_jid("bob@example.com/web");
        join_member(&actor, &bob, "old-nick").await;
        // Three replies were lost; the coalescing inventory holds ONE entry
        // carrying the newest attempt.
        state.deps.protocol.pending_local_muc_departures.record(
            crate::server::routes::websocket::LocalDepartureItem::RoomDeparture {
                room: room.clone(),
                jid: jid.clone(),
                cause: OccupancyLeaveCause::Explicit,
                selector: LeaveSessionSelector::Any,
                attempt: third_attempt,
                notified: HashSet::new(),
                removal: waddle_xmpp::muc::MucRemovalCause::Voluntary,
            },
        );

        run_local_muc_departure_sweep(&state).await;

        assert!(
            actor
                .ask(GetSnapshot)
                .await
                .expect("snapshot")
                .departures
                .receipts
                .is_empty(),
            "every owed receipt is replayed (or consumed as stale) and none is stranded"
        );
        assert_eq!(
            state.deps.protocol.pending_local_muc_departures.len(),
            0,
            "the drained entry fully converges"
        );
    }

    /// #1647 (codex round 23): a `FullJidSweep` carries the ORIGINAL
    /// cleanup's attempt as an occupancy-order ceiling. A replacement
    /// connection's session that joined after the ceiling is `Superseded`
    /// (kept), while occupancies the terminated session left behind are
    /// reaped — a live registration is proof of neither.
    #[tokio::test]
    async fn full_jid_sweep_fence_keeps_replacement_joins_and_reaps_stale_occupancies() {
        let store = JanitorProjectionStore::new();
        let state = clustered_state_with_store(store.clone()).await;
        let jid = full_jid("alice@example.com/web");
        let stale_room = room_jid("sweep-fence-stale");
        let rejoined_room = room_jid("sweep-fence-rejoined");
        let stale_actor = create_room(state.as_ref(), &stale_room).await;
        let rejoined_actor = create_room(state.as_ref(), &rejoined_room).await;
        // The terminated session occupied only the stale room.
        join_member(&stale_actor, &jid, "alice").await;
        // The original disconnect cleanup mints the sweep ceiling, then fails
        // room enumeration (simulated by recording the sweep directly).
        let attempt = waddle_xmpp::muc::room_actor::LeaveAttemptId::generate();
        // A replacement connection claims the same full JID and joins a room
        // AFTER the ceiling.
        let (tx, _rx) = mpsc::channel(4);
        register_test_connection(state.as_ref(), &jid, tx).await;
        join_member(&rejoined_actor, &jid, "alice").await;
        state.deps.protocol.pending_local_muc_departures.record(
            crate::server::routes::websocket::LocalDepartureItem::FullJidSweep {
                jid: jid.clone(),
                selector: LeaveSessionSelector::Any,
                attempt,
                remote_ceiling: u64::MAX,
                removal: waddle_xmpp::muc::MucRemovalCause::Voluntary,
            },
        );

        run_local_muc_departure_sweep(&state).await;

        assert!(
            rejoined_actor
                .ask(GetSnapshot)
                .await
                .expect("rejoined snapshot")
                .room
                .find_occupant_by_real_jid(&jid)
                .is_some(),
            "a session that joined after the sweep ceiling is not the sweep's target"
        );
        assert!(
            stale_actor
                .ask(GetSnapshot)
                .await
                .expect("stale snapshot")
                .room
                .find_occupant_by_real_jid(&jid)
                .is_none(),
            "the terminated session's occupancy is reaped despite the live registration"
        );
    }

    /// #1647 (codex P1): a live connection registration must NOT
    /// short-circuit a retained disconnect departure. The janitor hands the
    /// retry to the actor, whose fences classify it: a rejoin newer than the
    /// attempt is `Superseded` (covered by
    /// `local_departure_sweep_replacement_rejoin_is_superseded_without_sfu_teardown`),
    /// while a live registration WITHOUT a rejoin still owes the departure
    /// of the old session — dropping it would leave a ghost occupant.
    #[tokio::test]
    async fn local_departure_sweep_delivers_disconnect_despite_live_registration() {
        let store = JanitorProjectionStore::new();
        let state = clustered_state_with_store(store.clone()).await;
        let room = room_jid("live-not-skipped");
        let jid = full_jid("alice@example.com/web");
        let actor = create_room(state.as_ref(), &room).await;
        join_member(&actor, &jid, "alice").await;
        // The full JID is live-registered (a reconnect), but it never
        // rejoined the room after the retained attempt was minted.
        let (tx, _rx) = mpsc::channel(4);
        register_test_connection(state.as_ref(), &jid, tx).await;
        state.deps.protocol.pending_local_muc_departures.record(
            crate::server::routes::websocket::LocalDepartureItem::RoomDeparture {
                room: room.clone(),
                jid: jid.clone(),
                cause: OccupancyLeaveCause::Disconnect,
                selector: LeaveSessionSelector::Any,
                attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
                notified: std::collections::HashSet::new(),
                removal: waddle_xmpp::muc::MucRemovalCause::Voluntary,
            },
        );

        run_local_muc_departure_sweep(&state).await;

        assert!(
            actor
                .ask(GetSnapshot)
                .await
                .expect("snapshot")
                .room
                .find_occupant_by_real_jid(&jid)
                .is_none(),
            "the departed session is removed even though the JID is live-registered"
        );
        assert_eq!(
            state.deps.protocol.pending_local_muc_departures.len(),
            0,
            "the delivered departure (and its acknowledgement) fully converges"
        );
    }

    #[tokio::test]
    async fn confirm_retired_waits_for_reap_then_converges() {
        let state = create_test_websocket_state().await;
        let room = room_jid("confirm-retired");
        let jid = full_jid("alice@example.com/web");
        let actor = create_room(state.as_ref(), &room).await;
        let actor_id = actor.id();
        let attempt = waddle_xmpp::muc::room_actor::LeaveAttemptId::generate();
        let notified_bob: HashSet<FullJid> = HashSet::from([full_jid("bob@example.com/phone")]);
        state.deps.protocol.pending_local_muc_departures.record(
            crate::server::routes::websocket::LocalDepartureItem::ConfirmRetired {
                room: room.clone(),
                jid: jid.clone(),
                actor: actor_id,
                cause: OccupancyLeaveCause::Disconnect,
                selector: LeaveSessionSelector::Any,
                attempt,
                notified: notified_bob.clone(),
                removal: waddle_xmpp::muc::MucRemovalCause::Voluntary,
            },
        );

        run_local_muc_departure_sweep(&state).await;
        assert_eq!(
            state.deps.protocol.pending_local_muc_departures.len(),
            1,
            "a live actor with the same id must be requeued while waiting for reap"
        );
        assert!(
            state
                .deps
                .protocol
                .pending_local_muc_departures
                .contains_for_test(
                    &crate::server::routes::websocket::LocalDepartureItem::ConfirmRetired {
                        room: room.clone(),
                        jid: jid.clone(),
                        actor: actor_id,
                        cause: OccupancyLeaveCause::Disconnect,
                        selector: LeaveSessionSelector::Any,
                        attempt,
                        notified: notified_bob,
                        removal: waddle_xmpp::muc::MucRemovalCause::Voluntary,
                    }
                ),
            "the awaiting-reap requeue carries the fan-out progress"
        );

        let destroyed = RoomRegistry::wrap(state.deps.protocol.room_registry.clone())
            .destroy_room_if_inactive(
                room.clone(),
                actor
                    .ask(GetSnapshot)
                    .await
                    .expect("snapshot")
                    .occupancy_revision,
                waddle_xmpp::muc::room_actor::SealGuard::Dormant,
            )
            .await
            .expect("destroy dormant room");
        assert!(destroyed.destroyed(), "the test room should be reapable");

        // The registry retires the actor asynchronously and a requeued item
        // re-arms with a 2s backoff, so converge with a bounded poll.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            run_local_muc_departure_sweep(&state).await;
            if state.deps.protocol.pending_local_muc_departures.len() == 0
                || std::time::Instant::now() > deadline
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        assert_eq!(
            state.deps.protocol.pending_local_muc_departures.len(),
            0,
            "the retained confirm-retired item must converge once the actor disappears"
        );
    }

    #[tokio::test]
    async fn confirm_retired_with_successor_converts_to_departure_and_leaves_transplanted_roster() {
        let store = JanitorProjectionStore::new();
        let state = clustered_state_with_store(store.clone()).await;
        let room = room_jid("confirm-retired-successor");
        let alice = full_jid("alice@example.com/web");
        let bob = full_jid("bob@example.com/phone");
        let actor = create_room(state.as_ref(), &room).await;
        join_member(&actor, &alice, "alice").await;
        join_member(&actor, &bob, "bob").await;
        let (bob_tx, mut bob_rx) = mpsc::channel(8);
        register_test_connection(state.as_ref(), &bob, bob_tx).await;
        while bob_rx.try_recv().is_ok() {}
        let predecessor_snapshot = actor.ask(GetSnapshot).await.expect("predecessor snapshot");

        store.set_leave_mode(LeaveProjectionMode::NotOwner);
        assert!(matches!(
            actor
                .ask(LeaveByRealJid {
                    sender_jid: alice.clone(),
                    cause: OccupancyLeaveCause::Disconnect,
                    session: LeaveSessionSelector::Any,
                    attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
                    origin: waddle_xmpp::muc::room_actor::LeaveOrigin::Fresh,
                })
                .await,
            Err(kameo::error::SendError::HandlerError(
                waddle_xmpp::muc::room_actor::RoomActorError::RoomSealed
            ))
        ));

        let reaped = state
            .deps
            .protocol
            .room_registry
            .ask(ReapSealedRoom {
                room_jid: room.clone(),
            })
            .await
            .expect("reap sealed predecessor");
        assert!(reaped, "the sealed predecessor must be purgeable");

        store.set_leave_mode(LeaveProjectionMode::Succeed);
        let successor = state
            .deps
            .protocol
            .room_registry
            .ask(GetOrCreateRoomWithLiveRoster {
                room_jid: room.clone(),
                waddle_id: WaddleId::new("w".to_string()),
                channel_id: ChannelId::new("c".to_string()),
                config: RoomConfig::default(),
                live_room_restore: predecessor_snapshot.room,
                occupancy_revision: predecessor_snapshot.occupancy_revision,
                departures: Default::default(),
                live_roster_restore_attempt: predecessor_snapshot.live_roster_restore_attempt,
                occupancy_incarnation: predecessor_snapshot.occupancy_incarnation,
                pending_affiliation_departures: Default::default(),
                pending_admin_projection: None,
                admin_mutation_resolutions: Vec::new(),
                demote_first: None,
            })
            .await
            .expect("spawn successor with live roster")
            .actor_ref;
        assert_ne!(
            successor.id(),
            actor.id(),
            "the registry must now point at a successor actor"
        );

        state.deps.protocol.pending_local_muc_departures.record(
            crate::server::routes::websocket::LocalDepartureItem::ConfirmRetired {
                room: room.clone(),
                jid: alice.clone(),
                actor: actor.id(),
                cause: OccupancyLeaveCause::Disconnect,
                selector: LeaveSessionSelector::Any,
                attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
                // The dead task had already notified bob before the retirement
                // watch: the successor's resumed fan-out must skip bob.
                notified: HashSet::from([bob.clone()]),
                removal: waddle_xmpp::muc::MucRemovalCause::Voluntary,
            },
        );

        run_local_muc_departure_sweep(&state).await;

        assert_eq!(
            state.deps.protocol.pending_local_muc_departures.len(),
            0,
            "the successor conversion must leave the transplanted roster in one sweep"
        );
        let snapshot = successor
            .ask(GetSnapshot)
            .await
            .expect("successor snapshot");
        assert!(
            snapshot.room.find_occupant_by_real_jid(&alice).is_none(),
            "the converted retry must remove the transplanted departed session"
        );
        assert!(
            snapshot.room.find_occupant_by_real_jid(&bob).is_some(),
            "the converted retry must preserve the rest of the transplanted roster"
        );
        assert!(
            bob_rx.try_recv().is_err(),
            "progress carried through ConfirmRetired: bob is not notified twice"
        );
    }

    #[tokio::test]
    async fn inactive_seal_deferred_then_unseal_inactive_retry_leaves() {
        // Durable-store rooms defer under an inactivity seal (store-less rooms
        // keep today's suppressed departure instead).
        let store = JanitorProjectionStore::new();
        let state = clustered_state_with_store(store).await;
        let room = room_jid("inactive-seal-retry");
        let source_room = room_jid("inactive-source");
        let alice = full_jid("alice@example.com/web");
        let source_actor = create_room(state.as_ref(), &source_room).await;
        join_member(&source_actor, &alice, "alice").await;
        let source_snapshot = source_actor
            .ask(GetSnapshot)
            .await
            .expect("source snapshot");

        let actor = create_room(state.as_ref(), &room).await;
        assert!(matches!(
            actor
                .ask(SealIfInactive {
                    expected_occupancy_revision: 0,
                    guard: SealGuard::Dormant,
                })
                .await
                .expect("seal inactive"),
            SealIfInactiveOutcome::Inactive
        ));
        actor
            .ask(RestoreLiveRoster {
                room: source_snapshot.room,
                occupancy_revision: source_snapshot.occupancy_revision,
                departures: Default::default(),
                live_roster_restore_attempt: source_snapshot.live_roster_restore_attempt,
                occupancy_incarnation: source_snapshot.occupancy_incarnation,
                pending_affiliation_departures: source_snapshot.pending_affiliation_departures,
                pending_admin_projection: source_snapshot.pending_admin_projection,
                admin_mutation_resolutions: source_snapshot.admin_mutation_resolutions,
            })
            .await
            .expect("restore live roster into inactive actor");
        state.deps.protocol.pending_local_muc_departures.record(
            crate::server::routes::websocket::LocalDepartureItem::RoomDeparture {
                room: room.clone(),
                jid: alice.clone(),
                cause: OccupancyLeaveCause::Disconnect,
                selector: LeaveSessionSelector::Any,
                attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
                notified: std::collections::HashSet::new(),
                removal: waddle_xmpp::muc::MucRemovalCause::Voluntary,
            },
        );

        run_local_muc_departure_sweep(&state).await;

        assert_eq!(
            state.deps.protocol.pending_local_muc_departures.len(),
            1,
            "the inactive seal must defer the janitor retry"
        );
        assert!(actor.ask(UnsealInactive).await.expect("unseal inactive"));

        // The deferred item re-armed with a 2s backoff: poll until it is due.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            run_local_muc_departure_sweep(&state).await;
            if state.deps.protocol.pending_local_muc_departures.len() == 0
                || std::time::Instant::now() > deadline
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }

        assert_eq!(
            state.deps.protocol.pending_local_muc_departures.len(),
            0,
            "the retried leave must converge after the inactive seal is lifted"
        );
        let snapshot = actor.ask(GetSnapshot).await.expect("post-retry snapshot");
        assert!(
            snapshot.room.find_occupant_by_real_jid(&alice).is_none(),
            "the retried inactive leave must remove the occupant"
        );
    }

    /// #1647 (codex round 23): the sweep no longer short-circuits on a live
    /// registration — the ceiling attempt fences instead. A session that
    /// joined after the ceiling is `Superseded`, the queue converges.
    #[tokio::test]
    async fn full_jid_sweep_converges_without_evicting_a_join_newer_than_its_ceiling() {
        let state = create_test_websocket_state().await;
        let room = room_jid("full-jid-sweep");
        let jid = full_jid("alice@example.com/web");
        let actor = create_room(state.as_ref(), &room).await;
        // The ceiling predates the replacement's join.
        let attempt = waddle_xmpp::muc::room_actor::LeaveAttemptId::generate();
        let (tx, _rx) = mpsc::channel(4);
        register_test_connection(state.as_ref(), &jid, tx).await;
        join_member(&actor, &jid, "alice").await;
        state.deps.protocol.pending_local_muc_departures.record(
            crate::server::routes::websocket::LocalDepartureItem::FullJidSweep {
                jid: jid.clone(),
                selector: LeaveSessionSelector::Any,
                attempt,
                remote_ceiling: u64::MAX,
                removal: waddle_xmpp::muc::MucRemovalCause::Voluntary,
            },
        );

        run_local_muc_departure_sweep(&state).await;

        assert_eq!(
            state.deps.protocol.pending_local_muc_departures.len(),
            0,
            "the superseded sweep converges without a retained retry"
        );
        let snapshot = snapshot_room(state.as_ref(), &room).await.room;
        assert!(
            snapshot.find_occupant_by_real_jid(&jid).is_some(),
            "a join newer than the sweep ceiling must be left intact"
        );
    }

    #[tokio::test]
    async fn full_jid_sweep_redrive_failure_requeues_with_backoff_then_converges() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let failed_state = create_test_websocket_state().await;
        let failed_room = room_jid("full-jid-sweep-redrive-failed");
        let jid = full_jid("alice@example.com/web");
        let failed_actor = create_room(failed_state.as_ref(), &failed_room).await;
        join_member(&failed_actor, &jid, "alice").await;

        // `WebSocketState` owns a concrete RoomRegistry actor reference, so
        // killing it is the available enumeration-failure injection. Create a
        // fresh fixture below to model enumeration becoming available again.
        failed_state.deps.protocol.room_registry.kill();
        failed_state
            .deps
            .protocol
            .room_registry
            .wait_for_shutdown()
            .await;
        failed_state
            .deps
            .protocol
            .pending_local_muc_departures
            .record(
                crate::server::routes::websocket::LocalDepartureItem::FullJidSweep {
                    jid: jid.clone(),
                    selector: LeaveSessionSelector::Any,
                    attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
                    remote_ceiling: u64::MAX,
                    removal: waddle_xmpp::muc::MucRemovalCause::Voluntary,
                },
            );

        run_local_muc_departure_sweep(&failed_state).await;

        let mut requeued = failed_state
            .deps
            .protocol
            .pending_local_muc_departures
            .take_due(Instant::now() + std::time::Duration::from_secs(120));
        assert_eq!(requeued.len(), 1, "the failed redrive remains retained");
        assert_eq!(requeued[0].attempts, 1, "the failed redrive backs off once");
        // The recorder is process-global: sibling tests may add to this
        // counter concurrently, so assert the outcome was recorded at least once.
        assert!(
            metrics
                .counter_sum(
                    "waddle.muc.local_departure_retry",
                    &[("outcome", "requeued")],
                )
                .is_some_and(|count| count >= 1),
            "the failed redrive records its requeue outcome"
        );

        let restored_state = create_test_websocket_state().await;
        let restored_room = room_jid("full-jid-sweep-redrive-restored");
        let restored_actor = create_room(restored_state.as_ref(), &restored_room).await;
        join_member(&restored_actor, &jid, "alice").await;
        let requeued = requeued.pop().expect("retained full-JID sweep");
        restored_state
            .deps
            .protocol
            .pending_local_muc_departures
            .record_pending_for_test(crate::server::routes::websocket::PendingLocalDeparture {
                // The restored fixture's join happened just above (a fixture
                // artifact — production sweeps are minted after the dead
                // session's joins), so carry the sweep forward with a ceiling
                // that postdates it.
                item: crate::server::routes::websocket::LocalDepartureItem::FullJidSweep {
                    jid: jid.clone(),
                    selector: LeaveSessionSelector::Any,
                    attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
                    remote_ceiling: u64::MAX,
                    removal: waddle_xmpp::muc::MucRemovalCause::Voluntary,
                },
                attempts: requeued.attempts,
                // Model the janitor's next pass after the retained item's
                // backoff has elapsed in the recovered registry fixture.
                not_before: Instant::now(),
            });

        run_local_muc_departure_sweep(&restored_state).await;

        let snapshot = restored_actor
            .ask(GetSnapshot)
            .await
            .expect("restored cleanup snapshot");
        assert!(
            snapshot.room.find_occupant_by_real_jid(&jid).is_none(),
            "the re-driven full-JID cleanup removes the disconnected occupant"
        );
        assert_eq!(
            restored_state
                .deps
                .protocol
                .pending_local_muc_departures
                .len(),
            0,
            "the completed redrive leaves no retained sweep"
        );
        assert!(
            metrics
                .counter_sum(
                    "waddle.muc.local_departure_retry",
                    &[("outcome", "completed")],
                )
                .is_some_and(|count| count >= 1),
            "the successful redrive records its completion outcome"
        );
    }

    #[tokio::test]
    async fn stuck_item_emits_metric_and_is_still_retried() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let state = create_test_websocket_state().await;
        let room = room_jid("stuck-confirm-retired");
        let actor = create_room(state.as_ref(), &room).await;
        state
            .deps
            .protocol
            .pending_local_muc_departures
            .record_pending_for_test(crate::server::routes::websocket::PendingLocalDeparture {
                item: crate::server::routes::websocket::LocalDepartureItem::ConfirmRetired {
                    room,
                    jid: full_jid("alice@example.com/web"),
                    actor: actor.id(),
                    cause: OccupancyLeaveCause::Disconnect,
                    selector: LeaveSessionSelector::Any,
                    attempt: waddle_xmpp::muc::room_actor::LeaveAttemptId::generate(),
                    notified: std::collections::HashSet::new(),
                    removal: waddle_xmpp::muc::MucRemovalCause::Voluntary,
                },
                attempts: 10,
                not_before: Instant::now(),
            });

        run_local_muc_departure_sweep(&state).await;

        assert_eq!(
            metrics.counter_sum("waddle.muc.local_departure_retry", &[("outcome", "stuck")]),
            Some(1),
            "the sweep's own requeue of a repeatedly pending item must emit the stuck metric"
        );
        assert_eq!(
            state.deps.protocol.pending_local_muc_departures.len(),
            1,
            "the stuck item must still be retained for another retry"
        );
    }
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
            promotion_custody:
                waddle_xmpp::stream_management::PromotionCaptureCustody::never_claimed(),
            stream_id: stream_id.to_string(),
            user_id: jid.to_bare().to_string(),
            jid,
            occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
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
            waddle_xmpp_core::OccupancySessionGeneration::mint(),
        );

        let cleanup = crate::server::routes::websocket::redrive_remote_muc_cleanup(
            &state,
            &occupant,
            crate::server::routes::websocket::MembershipGenerationFilter::Any,
        )
        .await;

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
    /// #1703 (codex round 7): a live same-full-JID replacement owns its own
    /// memberships, but a snapshot recorded by a displaced generation is
    /// still a redrive candidate while the replacement is bound.
    #[tokio::test]
    async fn reconciler_redrives_foreign_generation_snapshots_behind_a_live_replacement() {
        let state = create_test_websocket_state().await;
        let memberships = &state.deps.protocol.remote_muc_memberships;
        let alice: jid::FullJid = "alice@example.com/web".parse().unwrap();
        let old_generation = waddle_xmpp_core::OccupancySessionGeneration::mint();
        let live_generation = waddle_xmpp_core::OccupancySessionGeneration::mint();

        // The live replacement publishes its generation on its registry entry.
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let owner = state
            .deps
            .protocol
            .connection_registry
            .register(alice.clone(), tx);
        assert!(state
            .deps
            .protocol
            .connection_registry
            .set_occupancy_session_if_owner(&alice, &owner, live_generation));

        // Only the live generation's own snapshot: nothing to redrive.
        memberships.record_join(&alice, &room("live-only"), "alice", live_generation);
        let candidates = collect_remote_muc_reconcile_candidates(&state).await;
        assert!(
            candidates.occupants.is_empty(),
            "a live connection's own memberships are its own"
        );

        // A displaced generation's snapshot behind the live replacement IS owed.
        memberships.record_join(&alice, &room("orphaned-remote"), "alice", old_generation);
        let candidates = collect_remote_muc_reconcile_candidates(&state).await;
        assert_eq!(candidates.occupants, vec![(alice, old_generation)]);
    }

    #[tokio::test]
    async fn resumable_replacement_exempts_only_its_own_membership_generation() {
        let state = create_test_websocket_state().await;
        let occupant: jid::FullJid = "alice@example.com/web".parse().unwrap();
        let old_generation = waddle_xmpp_core::OccupancySessionGeneration::mint();
        let replacement = detached_session("replacement", occupant.clone());
        let memberships = &state.deps.protocol.remote_muc_memberships;
        memberships.record_join(&occupant, &room("old"), "alice", old_generation);
        memberships.record_join(
            &occupant,
            &room("resumable"),
            "alice",
            replacement.occupancy_session,
        );
        state
            .deps
            .protocol
            .sm_session_registry
            .store_session(replacement)
            .await
            .unwrap();

        let candidates = collect_remote_muc_reconcile_candidates(&state).await;
        assert!(!candidates.had_failure);
        assert_eq!(
            candidates.occupants,
            vec![(occupant, old_generation)],
            "a resumable replacement must not hide the old generation"
        );
    }

    #[tokio::test]
    async fn reconciler_targets_only_fully_departed_occupants() {
        let state = create_test_websocket_state().await;
        let memberships = &state.deps.protocol.remote_muc_memberships;

        // Fully departed: candidate.
        let ghost: jid::FullJid = "ghost@example.com/web".parse().unwrap();
        let ghost_generation = waddle_xmpp_core::OccupancySessionGeneration::mint();
        memberships.record_join(&ghost, &room("ghost-room"), "ghost", ghost_generation);

        // Live connection: not a candidate.
        let live: jid::FullJid = "live@example.com/web".parse().unwrap();
        let live_generation = waddle_xmpp_core::OccupancySessionGeneration::mint();
        memberships.record_join(&live, &room("live-room"), "live", live_generation);
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let owner = state
            .deps
            .protocol
            .connection_registry
            .register(live.clone(), tx);
        // A bound connection publishes its generation (#1703): only ITS
        // memberships are exempt from the reconciler.
        assert!(state
            .deps
            .protocol
            .connection_registry
            .set_occupancy_session_if_owner(&live, &owner, live_generation));

        // Detached-but-resumable session: not a candidate.
        let detached: jid::FullJid = "detached@example.com/web".parse().unwrap();
        let detached_session = detached_session("stream-detached", detached.clone());
        memberships.record_join(
            &detached,
            &room("detached-room"),
            "detached",
            detached_session.occupancy_session,
        );
        state
            .deps
            .protocol
            .sm_session_registry
            .store_session(detached_session)
            .await
            .expect("store detached session");

        // Tombstoned membership (cleanup already in flight): no ACTIVE
        // entry, so not a candidate.
        let tombstoned: jid::FullJid = "tombstoned@example.com/web".parse().unwrap();
        memberships.record_join(
            &tombstoned,
            &room("tomb-room"),
            "tombstoned",
            waddle_xmpp_core::OccupancySessionGeneration::mint(),
        );
        let taken = memberships.take_for_occupant(&tombstoned);
        assert_eq!(taken.len(), 1);

        let candidates = collect_remote_muc_reconcile_candidates(&state).await;
        assert!(!candidates.had_failure);
        assert_eq!(
            candidates.occupants,
            vec![(ghost, ghost_generation)],
            "only the fully departed occupant is re-driven"
        );
    }
}

#[cfg(all(test, feature = "clustering"))]
mod graceful_shutdown_drain_tests {
    use super::user_reaper_tests::CountingReleaseFailureClaimStore;
    use super::{run_graceful_shutdown_drain, run_sm_expiry_sweep, HangingSmReadPersistence};
    use crate::server::routes::websocket::tests::{
        create_test_websocket_state_with_clustering, create_test_websocket_state_with_sm_registry,
        create_test_websocket_state_with_sm_registry_and_pending_storage,
        create_test_websocket_state_with_sm_registry_pending_and_blocking,
        register_test_connection,
    };
    use crate::shutdown_deadline::{ShutdownBudget, ShutdownDeadline};
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use tokio::sync::Notify;
    use waddle_xmpp::ownership::{
        ClaimStore, Entity, EntityType, InProcessClaimStore, NodeIdentity, SharedNodeIdentity,
    };
    use waddle_xmpp::pending_delivery::storage::{
        InMemoryPendingDeliveryStorage, PendingDeliveryStorage,
    };
    use waddle_xmpp::pending_delivery::SmSessionId;
    use waddle_xmpp::pending_delivery::{PendingPayload, PendingRow, PendingRowId};
    use waddle_xmpp::stream_management::persistence::SmPersistenceStorage;
    use waddle_xmpp::stream_management::{
        DetachedSession, DetachedUnackedStanza, InMemorySmSessionRegistry, SmSessionRegistry,
    };
    use waddle_xmpp::Stanza;
    use xmpp_parsers::message::{Message, MessageType};

    /// A shutdown deadline whose drain stage is `drain`; these tests run
    /// without a clustering room drain.
    fn drain_deadline(drain: Duration) -> ShutdownDeadline {
        ShutdownDeadline::new(ShutdownBudget::new(drain, Duration::ZERO))
    }

    fn detached_session(stream_id: &str, jid: jid::FullJid) -> DetachedSession {
        DetachedSession {
            promotion_custody:
                waddle_xmpp::stream_management::PromotionCaptureCustody::never_claimed(),
            stream_id: stream_id.to_string(),
            user_id: jid.to_bare().to_string(),
            jid,
            occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            inbound_count: 0,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(120),
            detached_at: Instant::now(),
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

    fn message_xml(message: &Message) -> String {
        let element = Stanza::Message(message.clone()).to_element();
        let mut buffer = Vec::new();
        element.write_to(&mut buffer).expect("serialize message");
        String::from_utf8(buffer).expect("message XML is UTF-8")
    }

    fn transient_message(to: &jid::BareJid, body: &str) -> Message {
        let mut message = Message::new(Some(jid::Jid::from(to.clone())));
        message.from = Some(
            "juliet@example.com/balcony"
                .parse()
                .expect("peer sender JID"),
        );
        message.type_ = MessageType::Chat;
        message
            .bodies
            .insert(xmpp_parsers::message::Lang::new(), body.to_string());
        message
    }

    struct UnavailableFenceAfterLoss {
        inner: InProcessClaimStore,
        lost_stream: SmSessionId,
        lost_stream_fence_calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl ClaimStore for UnavailableFenceAfterLoss {
        async fn ensure_schema(&self) -> Result<(), waddle_xmpp::ownership::ClaimError> {
            self.inner.ensure_schema().await
        }
        async fn acquire(
            &self,
            entity: &Entity,
            me: &NodeIdentity,
        ) -> Result<waddle_xmpp::ownership::ClaimEpoch, waddle_xmpp::ownership::ClaimError>
        {
            self.inner.acquire(entity, me).await
        }
        async fn ensure_claimed(
            &self,
            entity: &Entity,
            me: &NodeIdentity,
        ) -> Result<waddle_xmpp::ownership::ClaimEpoch, waddle_xmpp::ownership::ClaimError>
        {
            self.inner.ensure_claimed(entity, me).await
        }
        async fn steal_stale(
            &self,
            entity: &Entity,
            observed: waddle_xmpp::ownership::ClaimEpoch,
            staleness: waddle_xmpp::ownership::StalePredicate,
            me: &NodeIdentity,
        ) -> Result<waddle_xmpp::ownership::ClaimEpoch, waddle_xmpp::ownership::ClaimError>
        {
            self.inner
                .steal_stale(entity, observed, staleness, me)
                .await
        }
        async fn steal_for_resume(
            &self,
            entity: &Entity,
            observed: waddle_xmpp::ownership::ClaimEpoch,
            witness: waddle_xmpp::ownership::ResumeIdentityProof,
            me: &NodeIdentity,
        ) -> Result<waddle_xmpp::ownership::ClaimEpoch, waddle_xmpp::ownership::ClaimError>
        {
            self.inner
                .steal_for_resume(entity, observed, witness, me)
                .await
        }
        async fn current_claim(
            &self,
            entity: &Entity,
        ) -> Result<Option<waddle_xmpp::ownership::ClaimSnapshot>, waddle_xmpp::ownership::ClaimError>
        {
            self.inner.current_claim(entity).await
        }
        async fn fence(
            &self,
            entity: &Entity,
            me: &NodeIdentity,
            mine: waddle_xmpp::ownership::ClaimEpoch,
        ) -> Result<bool, waddle_xmpp::ownership::ClaimError> {
            if entity.id == self.lost_stream.as_str() {
                let call = self
                    .lost_stream_fence_calls
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if call == 0 {
                    return Ok(false);
                }
                return Err(waddle_xmpp::ownership::ClaimError::Backend(
                    "authority lookup unavailable after proven loss".to_owned(),
                ));
            }
            self.inner.fence(entity, me, mine).await
        }
        async fn release(
            &self,
            entity: &Entity,
            me: &NodeIdentity,
            mine: waddle_xmpp::ownership::ClaimEpoch,
        ) -> Result<(), waddle_xmpp::ownership::ClaimError> {
            self.inner.release(entity, me, mine).await
        }
        async fn release_exact(
            &self,
            entity: &Entity,
            me: &NodeIdentity,
            mine: waddle_xmpp::ownership::ClaimEpoch,
        ) -> Result<waddle_xmpp::ownership::ExactReleaseOutcome, waddle_xmpp::ownership::ClaimError>
        {
            self.inner.release_exact(entity, me, mine).await
        }
        async fn release_many(
            &self,
            entities: &[Entity],
            me: &NodeIdentity,
        ) -> Result<(), waddle_xmpp::ownership::ClaimError> {
            self.inner.release_many(entities, me).await
        }
    }

    #[tokio::test]
    async fn graceful_shutdown_preserves_proven_loss_when_later_authority_reads_fail() {
        let persistence =
            Arc::new(waddle_xmpp::stream_management::persistence::InMemorySmPersistence::new());
        let lost_stream = SmSessionId::new("shutdown-proven-loss");
        let claims = Arc::new(UnavailableFenceAfterLoss {
            inner: InProcessClaimStore::new(),
            lost_stream: lost_stream.clone(),
            lost_stream_fence_calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let registry = Arc::new(
            InMemorySmSessionRegistry::new()
                .with_persistence(persistence.clone())
                .with_claim_store(
                    claims.clone(),
                    SharedNodeIdentity::new(NodeIdentity::new("shutdown-node", "live")),
                ),
        );
        let state = create_test_websocket_state_with_sm_registry(registry.clone()).await;
        for (stream, jid) in [
            (lost_stream.as_str(), "romeo@example.com/phone"),
            ("shutdown-proof-healthy", "benvolio@example.com/web"),
        ] {
            let mut session = detached_session(stream, jid.parse().unwrap());
            session.outbound_count = 1;
            session.unacked_stanzas.push(DetachedUnackedStanza {
                ingress_receipts: Vec::new(),
                sequence: 1,
                stanza_xml: message_xml(&transient_message(
                    &session.jid.to_bare(),
                    "preserve custody",
                )),
                original_receipt_at: chrono::Utc::now(),
            });
            registry.store_session(session).await.expect("store source");
        }
        let stop = tokio_util::sync::CancellationToken::new();
        stop.cancel();
        run_graceful_shutdown_drain(
            state.clone(),
            stop,
            Arc::new(Notify::new()),
            tokio_util::sync::CancellationToken::new(),
            drain_deadline(Duration::from_secs(5)),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await;
        assert_eq!(
            claims
                .lost_stream_fence_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            1,
            "proven loss must not depend on a second successful lookup"
        );
        assert!(registry
            .live_session_ids()
            .expect("local inventory")
            .is_empty());
        assert!(persistence
            .get_session(&lost_stream)
            .await
            .expect("lost durable source")
            .is_some());
        assert_eq!(
            persistence
                .list_unacked(&lost_stream)
                .await
                .expect("lost queue")
                .len(),
            1
        );
        assert!(state
            .deps
            .protocol
            .pending_delivery_storage
            .list(&"romeo@example.com".parse().unwrap())
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            state
                .deps
                .protocol
                .pending_delivery_storage
                .list(&"benvolio@example.com".parse().unwrap())
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn sm_maintenance_promotes_nonexpired_sessions_without_current_occupancy_authority() {
        for replaced in [false, true] {
            let sm_registry = Arc::new(InMemorySmSessionRegistry::new());
            let pending = Arc::new(InMemoryPendingDeliveryStorage::unlimited());
            let state = create_test_websocket_state_with_sm_registry_and_pending_storage(
                sm_registry.clone(),
                pending.clone(),
            )
            .await;
            let jid: jid::FullJid = "romeo@example.com/legacy".parse().unwrap();
            let recipient = jid.to_bare();
            let mut session = detached_session("legacy-authority", jid.clone());
            session.outbound_count = 1;
            session.unacked_stanzas.push(DetachedUnackedStanza {
                ingress_receipts: Vec::new(),
                sequence: 1,
                stanza_xml: message_xml(&transient_message(&recipient, "recover now")),
                original_receipt_at: chrono::Utc::now(),
            });
            assert!(!session.is_expired());
            if replaced {
                crate::occupancy_authority::publish(
                    state.deps.app_state.db_pool.global(),
                    &jid,
                    waddle_xmpp_core::OccupancySessionGeneration::mint(),
                )
                .await
                .unwrap();
            }
            sm_registry.store_session(session).await.unwrap();

            run_sm_expiry_sweep(&state).await;

            assert!(sm_registry
                .peek_session("legacy-authority")
                .await
                .unwrap()
                .is_none());
            assert_eq!(pending.list(&recipient).await.unwrap().len(), 1);
            run_sm_expiry_sweep(&state).await;
            assert_eq!(
                pending.list(&recipient).await.unwrap().len(),
                1,
                "promote once"
            );
        }
    }

    #[tokio::test]
    async fn sm_maintenance_preserves_nonexpired_current_occupancy_authority() {
        let registry = Arc::new(InMemorySmSessionRegistry::new());
        let state = create_test_websocket_state_with_sm_registry(registry.clone()).await;
        let session = detached_session(
            "current-authority",
            "romeo@example.com/current".parse().unwrap(),
        );
        crate::occupancy_authority::publish(
            state.deps.app_state.db_pool.global(),
            &session.jid,
            session.occupancy_session,
        )
        .await
        .unwrap();
        registry.store_session(session).await.unwrap();
        run_sm_expiry_sweep(&state).await;
        assert!(registry
            .peek_session("current-authority")
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn sm_maintenance_defers_nonexpired_session_when_authority_lookup_fails() {
        let registry = Arc::new(InMemorySmSessionRegistry::new());
        let state = create_test_websocket_state_with_sm_registry(registry.clone()).await;
        registry
            .store_session(detached_session(
                "authority-unavailable",
                "romeo@example.com/current".parse().unwrap(),
            ))
            .await
            .unwrap();
        state
            .deps
            .app_state
            .db_pool
            .global()
            .guard()
            .await
            .unwrap()
            .execute("DROP TABLE xmpp_occupancy_authority", crate::db_params![])
            .await
            .unwrap();

        run_sm_expiry_sweep(&state).await;

        assert!(registry
            .peek_session("authority-unavailable")
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn sm_maintenance_preserves_foreign_owned_legacy_snapshot_and_queue() {
        let persistence =
            Arc::new(waddle_xmpp::stream_management::persistence::InMemorySmPersistence::new());
        let claims = Arc::new(InProcessClaimStore::new());
        let local = NodeIdentity::new("local", "legacy");
        let foreign = NodeIdentity::new("foreign", "current");
        let registry = Arc::new(
            InMemorySmSessionRegistry::new()
                .with_persistence(persistence.clone())
                .with_claim_store(claims.clone(), SharedNodeIdentity::new(local.clone())),
        );
        let pending = Arc::new(InMemoryPendingDeliveryStorage::unlimited());
        let state = create_test_websocket_state_with_sm_registry_and_pending_storage(
            registry.clone(),
            pending.clone(),
        )
        .await;
        let jid: jid::FullJid = "romeo@example.com/foreign".parse().unwrap();
        let mut session = detached_session("foreign-legacy-authority", jid.clone());
        session.outbound_count = 1;
        session.unacked_stanzas.push(DetachedUnackedStanza {
            ingress_receipts: Vec::new(),
            sequence: 1,
            stanza_xml: message_xml(&transient_message(&jid.to_bare(), "foreign custody")),
            original_receipt_at: chrono::Utc::now(),
        });
        registry.store_session(session).await.unwrap();
        let entity = Entity::new(EntityType::SmSession, "foreign-legacy-authority");
        let epoch = claims
            .current_claim(&entity)
            .await
            .unwrap()
            .unwrap()
            .claim_epoch;
        claims.release(&entity, &local, epoch).await.unwrap();
        let foreign_epoch = claims.acquire(&entity, &foreign).await.unwrap();

        run_sm_expiry_sweep(&state).await;

        let stream = SmSessionId::new("foreign-legacy-authority");
        assert!(persistence.get_session(&stream).await.unwrap().is_some());
        assert_eq!(persistence.list_unacked(&stream).await.unwrap().len(), 1);
        assert!(pending.list(&jid.to_bare()).await.unwrap().is_empty());
        assert!(claims
            .fence(&entity, &foreign, foreign_epoch)
            .await
            .unwrap());
    }

    struct HangingBlockingStorage {
        started: Notify,
    }

    struct BlockAfterFirstStorage {
        calls: std::sync::atomic::AtomicUsize,
        blocked: Notify,
    }

    #[async_trait::async_trait]
    impl waddle_xmpp::xep::xep0191::BlockingStorage for BlockAfterFirstStorage {
        async fn list_blocked_jids(
            &self,
            _user: &jid::BareJid,
        ) -> Result<Vec<jid::BareJid>, waddle_xmpp::xep::xep0191::BlockingStorageError> {
            if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                return Ok(Vec::new());
            }
            self.blocked.notify_one();
            std::future::pending().await
        }
    }

    #[async_trait::async_trait]
    impl waddle_xmpp::xep::xep0191::BlockingStorage for HangingBlockingStorage {
        async fn list_blocked_jids(
            &self,
            _user: &jid::BareJid,
        ) -> Result<Vec<jid::BareJid>, waddle_xmpp::xep::xep0191::BlockingStorageError> {
            self.started.notify_one();
            std::future::pending().await
        }
    }

    #[tokio::test]
    async fn lost_claim_stops_promotion_after_concurrent_local_demotion() {
        let identity = SharedNodeIdentity::new(NodeIdentity::new("sm-node", "demoted"));
        let registry = InMemorySmSessionRegistry::new()
            .with_claim_store(Arc::new(InProcessClaimStore::new()), identity.clone());
        let stream_id = "shutdown-already-demoted";
        registry
            .store_session(detached_session(
                stream_id,
                "romeo@example.com/phone".parse().expect("full jid"),
            ))
            .await
            .expect("store detached session");
        let session = registry
            .drain_all_for_shutdown()
            .await
            .expect("drain session")
            .sessions
            .pop()
            .expect("one session");
        let mut guard = crate::sm_promotion::PromotionSessionGuard::new(&registry, session);
        identity.disable().await;
        assert!(registry.forget_claim_locally(stream_id).await);

        assert!(
            super::abandon_shutdown_session_if_claim_lost(&registry, &mut guard)
                .await
                .expect("claim result")
        );
        drop(guard);
        assert!(registry
            .live_session_ids()
            .expect("live sessions")
            .is_empty());
    }

    #[tokio::test]
    async fn graceful_shutdown_distinguishes_socket_closure_from_q6_settlement() {
        let state = create_test_websocket_state_with_sm_registry(Arc::new(
            InMemorySmSessionRegistry::new(),
        ))
        .await;
        let stop = tokio_util::sync::CancellationToken::new();
        stop.cancel();
        let proof = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let completion = tokio_util::sync::CancellationToken::new();
        let held_connection = state.deps.shutdown.connection_guard();
        run_graceful_shutdown_drain(
            state.clone(),
            stop.clone(),
            Arc::new(Notify::new()),
            completion.clone(),
            drain_deadline(Duration::from_millis(40)),
            proof.clone(),
        )
        .await;
        assert!(
            completion.is_cancelled(),
            "Q6 must settle even after timeout"
        );
        assert!(
            !proof.load(std::sync::atomic::Ordering::Acquire),
            "settlement must not prove closure while a socket guard remains"
        );

        drop(held_connection);
        run_graceful_shutdown_drain(
            state,
            stop,
            Arc::new(Notify::new()),
            tokio_util::sync::CancellationToken::new(),
            drain_deadline(Duration::from_millis(40)),
            proof.clone(),
        )
        .await;
        assert!(
            proof.load(std::sync::atomic::Ordering::Acquire),
            "successful socket drain must publish closure proof"
        );
    }

    #[tokio::test]
    async fn graceful_shutdown_poll_wait_respects_remaining_budget() {
        let state = create_test_websocket_state_with_sm_registry(Arc::new(
            InMemorySmSessionRegistry::new(),
        ))
        .await;
        let stop = tokio_util::sync::CancellationToken::new();
        stop.cancel();

        tokio::time::timeout(
            Duration::from_millis(230),
            run_graceful_shutdown_drain(
                state,
                stop,
                Arc::new(Notify::new()),
                tokio_util::sync::CancellationToken::new(),
                drain_deadline(Duration::from_millis(80)),
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
            ),
        )
        .await
        .expect("an empty Q6 pass must not sleep beyond its shutdown deadline");
    }

    #[tokio::test]
    async fn graceful_shutdown_late_q6_start_does_not_restart_connection_wait_budget() {
        let registry = Arc::new(InMemorySmSessionRegistry::new());
        let state = create_test_websocket_state_with_sm_registry(registry).await;
        let held_connection = state.deps.shutdown.connection_guard();
        let stop = tokio_util::sync::CancellationToken::new();
        stop.cancel();
        let completion = tokio_util::sync::CancellationToken::new();
        let started = drain_deadline(Duration::from_secs(2));
        started.start_at(tokio::time::Instant::now() - Duration::from_secs(3));

        tokio::time::timeout(
            Duration::from_millis(500),
            run_graceful_shutdown_drain(
                state,
                stop,
                Arc::new(Notify::new()),
                completion.clone(),
                started,
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
            ),
        )
        .await
        .expect("late Q6 start must use the expired shared connection budget");
        assert!(completion.is_cancelled());
        drop(held_connection);
    }

    #[tokio::test]
    async fn graceful_shutdown_abandons_terminally_disabled_identity_without_deleting_durable_row()
    {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let persistence = Arc::new(HangingSmReadPersistence::default());
        persistence
            .allow_reads
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let claims = Arc::new(InProcessClaimStore::new());
        let identity = SharedNodeIdentity::new(NodeIdentity::new("sm-node", "disabled"));
        let registry = Arc::new(
            InMemorySmSessionRegistry::new()
                .with_persistence(persistence.clone())
                .with_claim_store(claims, identity.clone()),
        );
        let state = create_test_websocket_state_with_sm_registry(registry.clone()).await;
        let stream_id = "shutdown-disabled-identity";
        registry
            .store_session(detached_session(
                stream_id,
                "romeo@example.com/phone".parse().expect("full jid"),
            ))
            .await
            .expect("store detached session");
        persistence
            .delete_attempts
            .store(0, std::sync::atomic::Ordering::SeqCst);
        identity.disable().await;

        let token = tokio_util::sync::CancellationToken::new();
        token.cancel();
        let started = Instant::now();
        run_graceful_shutdown_drain(
            state,
            token,
            Arc::new(Notify::new()),
            tokio_util::sync::CancellationToken::new(),
            drain_deadline(Duration::from_secs(5)),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await;

        assert!(started.elapsed() < Duration::from_secs(4));
        assert_eq!(
            registry.live_session_ids().expect("live inventory"),
            Vec::<String>::new()
        );
        assert!(persistence
            .get_session(&SmSessionId::new(stream_id))
            .await
            .expect("durable session read")
            .is_some());
        assert_eq!(
            persistence
                .delete_attempts
                .load(std::sync::atomic::Ordering::SeqCst),
            0,
            "terminal loss must skip durable confirmation entirely"
        );
        assert_eq!(
            metrics.counter_sum("waddle.clustering.claims_abandoned_on_drain", &[]),
            Some(1)
        );
        assert_eq!(
            metrics
                .counter_sum("xmpp.sm.drain_timeout", &[])
                .unwrap_or(0),
            0
        );
    }

    #[tokio::test]
    async fn graceful_shutdown_abandons_session_taken_over_by_successor() {
        let persistence =
            Arc::new(waddle_xmpp::stream_management::persistence::InMemorySmPersistence::new());
        let claims = Arc::new(InProcessClaimStore::new());
        let local = NodeIdentity::new("sm-node", "old");
        let successor = NodeIdentity::new("sm-node", "successor");
        let registry = Arc::new(
            InMemorySmSessionRegistry::new()
                .with_persistence(persistence.clone())
                .with_claim_store(claims.clone(), SharedNodeIdentity::new(local.clone())),
        );
        let state = create_test_websocket_state_with_sm_registry(registry.clone()).await;
        let stream_id = "shutdown-successor-takeover";
        registry
            .store_session(detached_session(
                stream_id,
                "romeo@example.com/phone".parse().expect("full jid"),
            ))
            .await
            .expect("store detached session");
        let healthy_jid: jid::FullJid =
            "benvolio@example.com/healthy".parse().expect("healthy jid");
        let healthy_recipient = healthy_jid.to_bare();
        let healthy_stream = "shutdown-successor-healthy-peer";
        let mut healthy = detached_session(healthy_stream, healthy_jid);
        healthy.outbound_count = 1;
        healthy.unacked_stanzas.push(DetachedUnackedStanza {
            ingress_receipts: Vec::new(),
            sequence: 1,
            stanza_xml: message_xml(&transient_message(
                &healthy_recipient,
                "healthy peer still drains",
            )),
            original_receipt_at: chrono::Utc::now(),
        });
        registry
            .store_session(healthy)
            .await
            .expect("store healthy peer");
        let entity = Entity::new(EntityType::SmSession, stream_id);
        let local_epoch = claims
            .current_claim(&entity)
            .await
            .expect("current claim")
            .expect("local claim")
            .claim_epoch;
        claims
            .release(&entity, &local, local_epoch)
            .await
            .expect("release local claim");
        let successor_epoch = claims
            .acquire(&entity, &successor)
            .await
            .expect("successor claim");

        let token = tokio_util::sync::CancellationToken::new();
        token.cancel();
        run_graceful_shutdown_drain(
            state.clone(),
            token,
            Arc::new(Notify::new()),
            tokio_util::sync::CancellationToken::new(),
            drain_deadline(Duration::from_secs(5)),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await;

        assert!(registry
            .live_session_ids()
            .expect("live inventory")
            .is_empty());
        assert!(persistence
            .get_session(&SmSessionId::new(stream_id))
            .await
            .expect("durable session read")
            .is_some());
        assert!(claims
            .fence(&entity, &successor, successor_epoch)
            .await
            .expect("successor fence"));
        assert!(persistence
            .get_session(&SmSessionId::new(healthy_stream))
            .await
            .expect("healthy durable row")
            .is_none());
        assert_eq!(
            state
                .deps
                .protocol
                .pending_delivery_storage
                .list(&healthy_recipient)
                .await
                .expect("healthy pending queue")
                .len(),
            1,
            "a successor-owned session must not prevent healthy shutdown promotion"
        );
    }

    #[tokio::test]
    async fn graceful_shutdown_retries_transient_confirmation_after_backoff() {
        let persistence = Arc::new(HangingSmReadPersistence::default());
        persistence
            .allow_reads
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let registry =
            Arc::new(InMemorySmSessionRegistry::new().with_persistence(persistence.clone()));
        let state = create_test_websocket_state_with_sm_registry(registry.clone()).await;
        let stream_id = "shutdown-transient-confirmation";
        registry
            .store_session(detached_session(
                stream_id,
                "romeo@example.com/phone".parse().expect("full jid"),
            ))
            .await
            .expect("store detached session");
        persistence
            .delete_failures_remaining
            .store(1, std::sync::atomic::Ordering::SeqCst);
        persistence
            .delete_attempts
            .store(0, std::sync::atomic::Ordering::SeqCst);
        persistence
            .delete_times
            .lock()
            .expect("delete attempt times")
            .clear();

        let token = tokio_util::sync::CancellationToken::new();
        token.cancel();
        run_graceful_shutdown_drain(
            state,
            token,
            Arc::new(Notify::new()),
            tokio_util::sync::CancellationToken::new(),
            drain_deadline(Duration::from_secs(5)),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await;

        let delete_times = persistence
            .delete_times
            .lock()
            .expect("delete attempt times")
            .clone();
        assert_eq!(delete_times.len(), 2);
        assert!(
            delete_times[1].duration_since(delete_times[0]) >= Duration::from_millis(250),
            "non-empty retry pass must pause before the next confirmation"
        );
        assert_eq!(
            persistence
                .delete_attempts
                .load(std::sync::atomic::Ordering::SeqCst),
            2,
            "the transient confirmation failure should retry once"
        );
        assert!(registry
            .live_session_ids()
            .expect("live inventory")
            .is_empty());
        assert!(persistence
            .get_session(&SmSessionId::new(stream_id))
            .await
            .expect("durable session read")
            .is_none());
    }

    #[tokio::test]
    async fn graceful_shutdown_counts_unreleased_confirmed_claim_as_abandoned() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let persistence =
            Arc::new(waddle_xmpp::stream_management::persistence::InMemorySmPersistence::new());
        let claims = Arc::new(CountingReleaseFailureClaimStore {
            inner: InProcessClaimStore::new(),
            fail_release: std::sync::atomic::AtomicBool::new(false),
            release_calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let registry = Arc::new(
            InMemorySmSessionRegistry::new()
                .with_persistence(persistence.clone())
                .with_claim_store(
                    claims.clone(),
                    SharedNodeIdentity::new(NodeIdentity::new("sm-node", "release-failure")),
                ),
        );
        let state = create_test_websocket_state_with_sm_registry(registry.clone()).await;
        let stream_id = "shutdown-release-failure";
        registry
            .store_session(detached_session(
                stream_id,
                "romeo@example.com/phone".parse().expect("full jid"),
            ))
            .await
            .expect("store detached session");
        claims
            .fail_release
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let stop = tokio_util::sync::CancellationToken::new();
        stop.cancel();

        run_graceful_shutdown_drain(
            state,
            stop,
            Arc::new(Notify::new()),
            tokio_util::sync::CancellationToken::new(),
            drain_deadline(Duration::from_secs(4)),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await;

        assert!(persistence
            .get_session(&SmSessionId::new(stream_id))
            .await
            .expect("durable session read")
            .is_none());
        assert!(registry
            .live_session_ids()
            .expect("live sessions")
            .is_empty());
        assert_eq!(registry.pending_claim_release_count(), 1);
        assert_eq!(
            metrics.counter_sum("waddle.clustering.claims_abandoned_on_drain", &[]),
            Some(1)
        );
        assert_eq!(
            metrics
                .counter_sum("waddle.clustering.claims_released_on_drain", &[])
                .unwrap_or(0),
            0
        );
    }

    #[tokio::test]
    async fn clustered_shutdown_parks_deferred_replay_and_restores_suffix_after_restart() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let persistence =
            Arc::new(waddle_xmpp::stream_management::persistence::InMemorySmPersistence::new());
        let claims = Arc::new(InProcessClaimStore::new());
        let old = NodeIdentity::new("sm-node", "deferred-old");
        let identity = SharedNodeIdentity::new(old.clone());
        let registry = Arc::new(
            InMemorySmSessionRegistry::new()
                .with_persistence(persistence.clone())
                .with_claim_store(claims.clone(), identity.clone()),
        );
        let state = create_test_websocket_state_with_clustering(
            crate::clustering::ClusteringHandles {
                claim_store: Some(claims.clone()),
                node_identity: Some(identity),
                ..Default::default()
            },
            registry.clone(),
        )
        .await;
        let stream = SmSessionId::new("shutdown-deferred-iq");
        let jid: jid::FullJid = "romeo@example.com/deferred".parse().unwrap();
        let recipient = jid.to_bare();
        let mut session = detached_session(stream.as_str(), jid.clone());
        session.max_resume_time = Some(0);
        session.outbound_count = 3;
        let iq = Stanza::Iq(Box::new(xmpp_parsers::iq::Iq::empty_result(
            jid.clone().into(),
            "deferred-iq",
        )));
        let mut iq_xml = Vec::new();
        iq.to_element().write_to(&mut iq_xml).unwrap();
        let received_at = chrono::Utc::now() - chrono::Duration::minutes(2);
        for (sequence, stanza_xml) in [
            (
                1,
                message_xml(&transient_message(&recipient, "promoted prefix")),
            ),
            (2, String::from_utf8(iq_xml).unwrap()),
            (
                3,
                message_xml(&transient_message(&recipient, "retained suffix")),
            ),
        ] {
            session.unacked_stanzas.push(DetachedUnackedStanza {
                ingress_receipts: Vec::new(),
                sequence,
                stanza_xml,
                original_receipt_at: received_at,
            });
        }
        registry.store_session(session).await.unwrap();
        registry
            .store_session(detached_session(
                "shutdown-healthy-peer",
                "juliet@example.com/healthy".parse().unwrap(),
            ))
            .await
            .unwrap();
        let pending = state.deps.protocol.pending_delivery_storage.clone();
        let stop = tokio_util::sync::CancellationToken::new();
        stop.cancel();
        tokio::time::timeout(
            Duration::from_secs(4),
            run_graceful_shutdown_drain(
                state.clone(),
                stop,
                Arc::new(Notify::new()),
                tokio_util::sync::CancellationToken::new(),
                drain_deadline(Duration::from_secs(8)),
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
            ),
        )
        .await
        .expect("policy deferral must let shutdown finish before the drain deadline");
        assert_eq!(
            metrics
                .counter_sum("xmpp.sm.drain_timeout", &[])
                .unwrap_or(0),
            0,
        );
        assert!(persistence.get_session(&stream).await.unwrap().is_some());
        assert!(persistence
            .get_session(&SmSessionId::new("shutdown-healthy-peer"))
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            persistence
                .list_unacked(&stream)
                .await
                .unwrap()
                .iter()
                .map(|entry| entry.sequence)
                .collect::<Vec<_>>(),
            vec![2, 3],
            "only the unpromoted suffix survives the drain",
        );
        assert_eq!(pending.list(&recipient).await.unwrap().len(), 1);
        let entity = Entity::new(EntityType::SmSession, stream.as_str());
        let claim = claims.current_claim(&entity).await.unwrap().unwrap();
        assert_eq!(claim.owner, old, "parking retains claim authority");
        // Model old-node expiry/reaping before successor startup. Restore
        // must not hydrate a second owner while the old claim is live.
        let successor = Arc::new(
            InMemorySmSessionRegistry::new()
                .with_persistence(persistence.clone())
                .with_claim_store(
                    claims.clone(),
                    SharedNodeIdentity::new(NodeIdentity::new("sm-node", "deferred-new")),
                ),
        );
        assert_eq!(successor.restore_from_persistence().await.unwrap(), 0);
        claims
            .release(&entity, &old, claim.claim_epoch)
            .await
            .unwrap();
        assert_eq!(successor.restore_from_persistence().await.unwrap(), 1);
        let restored = create_test_websocket_state_with_sm_registry_and_pending_storage(
            successor,
            pending.clone(),
        )
        .await;
        run_sm_expiry_sweep(&restored).await;
        assert!(persistence.get_session(&stream).await.unwrap().is_none());
        let rows = pending.list(&recipient).await.unwrap();
        assert!(rows
            .iter()
            .all(|row| row.original_receipt_at == received_at));
        let bodies: Vec<_> = rows
            .iter()
            .map(|row| match &row.payload {
                PendingPayload::Transient(message) => message
                    .bodies
                    .get(&xmpp_parsers::message::Lang::new())
                    .expect("stored message body")
                    .as_str(),
                PendingPayload::Archived(_) => panic!("fixture has no archive reference"),
            })
            .collect();
        assert_eq!(bodies, vec!["promoted prefix", "retained suffix"]);
        run_sm_expiry_sweep(&restored).await;
        assert_eq!(
            pending.list(&recipient).await.unwrap().len(),
            2,
            "restart promotes the suffix without duplicating the prefix"
        );
    }

    #[tokio::test]
    async fn clustered_shutdown_redrives_queued_replay_through_pending_custody() {
        let persistence =
            Arc::new(waddle_xmpp::stream_management::persistence::InMemorySmPersistence::new());
        let claims: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
        let identity = SharedNodeIdentity::new(NodeIdentity::new("sm-node", "q6-handoff"));
        let registry = Arc::new(
            InMemorySmSessionRegistry::new()
                .with_persistence(persistence.clone())
                .with_claim_store(claims.clone(), identity.clone()),
        );
        let state = create_test_websocket_state_with_clustering(
            crate::clustering::ClusteringHandles {
                claim_store: Some(claims),
                node_identity: Some(identity),
                ..Default::default()
            },
            registry.clone(),
        )
        .await;
        let alternate: jid::FullJid = "romeo@example.com/web".parse().expect("full jid");
        let (sender, mut receiver) = tokio::sync::mpsc::channel(8);
        register_test_connection(&state, &alternate, sender).await;
        state
            .deps
            .protocol
            .connection_registry
            .update_presence(&alternate, true, 1);
        let stream_id = "clustered-q6-live-eligible";
        let recipient = alternate.to_bare();
        let mut session = detached_session(
            stream_id,
            "romeo@example.com/phone".parse().expect("full jid"),
        );
        session.outbound_count = 1;
        session.unacked_stanzas.push(DetachedUnackedStanza {
            ingress_receipts: Vec::new(),
            sequence: 1,
            stanza_xml: message_xml(&transient_message(&recipient, "durable before live")),
            original_receipt_at: chrono::Utc::now(),
        });
        registry
            .store_session(session)
            .await
            .expect("store detached session");

        let stop = tokio_util::sync::CancellationToken::new();
        stop.cancel();
        run_graceful_shutdown_drain(
            state.clone(),
            stop,
            Arc::new(Notify::new()),
            tokio_util::sync::CancellationToken::new(),
            drain_deadline(Duration::from_secs(4)),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await;

        let delivered = receiver
            .try_recv()
            .expect("queued replay must be re-driven to the available resource");
        assert!(
            matches!(delivered.stanza, Stanza::Message(_)),
            "pending re-drive must deliver the queued message"
        );
        assert!(receiver.try_recv().is_err(), "no duplicate live delivery");
        assert!(persistence
            .get_session(&SmSessionId::new(stream_id))
            .await
            .expect("durable session")
            .is_none());
    }

    #[tokio::test]
    async fn clustered_shutdown_retains_session_when_remote_pending_prefix_is_deferred() {
        let persistence =
            Arc::new(waddle_xmpp::stream_management::persistence::InMemorySmPersistence::new());
        let claims: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
        let identity = SharedNodeIdentity::new(NodeIdentity::new("sm-node", "remote-prefix"));
        let registry = Arc::new(
            InMemorySmSessionRegistry::new()
                .with_persistence(persistence.clone())
                .with_claim_store(claims.clone(), identity.clone()),
        );
        let state = create_test_websocket_state_with_clustering(
            crate::clustering::ClusteringHandles {
                claim_store: Some(claims),
                node_identity: Some(identity),
                ..Default::default()
            },
            registry.clone(),
        )
        .await;
        let remote: jid::FullJid = "romeo@example.com/remote".parse().expect("full jid");
        let (sender, mut receiver) = tokio::sync::mpsc::channel(8);
        state.deps.protocol.connection_registry.register_entry(
            remote.clone(),
            waddle_xmpp::registry::ConnectionEntry::remote_hosted(sender),
        );
        state
            .deps
            .protocol
            .connection_registry
            .update_presence(&remote, true, 10);
        let stream_id = "clustered-q6-remote-prefix";
        let recipient = remote.to_bare();
        let mut session = detached_session(
            stream_id,
            "romeo@example.com/phone".parse().expect("full jid"),
        );
        session.outbound_count = 1;
        session.unacked_stanzas.push(DetachedUnackedStanza {
            ingress_receipts: Vec::new(),
            sequence: 1,
            stanza_xml: message_xml(&transient_message(&recipient, "ordered prefix")),
            original_receipt_at: chrono::Utc::now(),
        });
        registry
            .store_session(session)
            .await
            .expect("store detached session");

        let stop = tokio_util::sync::CancellationToken::new();
        stop.cancel();
        run_graceful_shutdown_drain(
            state.clone(),
            stop,
            Arc::new(Notify::new()),
            tokio_util::sync::CancellationToken::new(),
            drain_deadline(Duration::from_millis(500)),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await;

        assert!(
            receiver.try_recv().is_err(),
            "remote mirror cannot take a local pending offer"
        );
        let rows = state
            .deps
            .protocol
            .pending_delivery_storage
            .list(&recipient)
            .await
            .expect("pending prefix");
        assert_eq!(rows.len(), 1);
        assert!(rows[0].flushed_in_session.is_none());
        assert!(rows[0].outbound_sequence.is_none());
        assert!(persistence
            .get_session(&SmSessionId::new(stream_id))
            .await
            .expect("durable session")
            .is_some());
    }

    #[tokio::test]
    async fn fatal_fence_records_earlier_confirmed_claims() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let persistence =
            Arc::new(waddle_xmpp::stream_management::persistence::InMemorySmPersistence::new());
        let registry =
            Arc::new(InMemorySmSessionRegistry::new().with_persistence(persistence.clone()));
        let blocking = Arc::new(BlockAfterFirstStorage {
            calls: std::sync::atomic::AtomicUsize::new(0),
            blocked: Notify::new(),
        });
        let state = create_test_websocket_state_with_sm_registry_pending_and_blocking(
            registry.clone(),
            Arc::new(InMemoryPendingDeliveryStorage::unlimited()),
            blocking.clone(),
        )
        .await;
        let first = "shutdown-confirmed-before-fatal";
        registry
            .store_session(detached_session(
                first,
                "romeo@example.com/first".parse().expect("full jid"),
            ))
            .await
            .expect("store first session");
        let stop = tokio_util::sync::CancellationToken::new();
        let task = tokio::spawn(run_graceful_shutdown_drain(
            state.clone(),
            stop.clone(),
            Arc::new(Notify::new()),
            tokio_util::sync::CancellationToken::new(),
            drain_deadline(Duration::from_secs(5)),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        ));
        stop.cancel();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if persistence
                    .get_session(&SmSessionId::new(first))
                    .await
                    .expect("first durable row")
                    .is_none()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("first session confirms before the quiet window ends");

        registry
            .store_session(detached_session(
                "shutdown-held-after-confirmation",
                "romeo@example.com/second".parse().expect("full jid"),
            ))
            .await
            .expect("store second session");
        tokio::time::timeout(Duration::from_secs(2), blocking.blocked.notified())
            .await
            .expect("second session reaches blocked promotion");
        state
            .deps
            .app_state
            .node_lifecycle
            .fatal_fence_token()
            .cancel();
        tokio::time::timeout(Duration::from_millis(500), task)
            .await
            .expect("fatal fence preempts Q6")
            .expect("Q6 task completes");
        assert_eq!(
            metrics.counter_sum("waddle.clustering.claims_released_on_drain", &[]),
            Some(1),
        );
        assert_eq!(
            metrics.counter_sum("waddle.clustering.claims_abandoned_on_drain", &[]),
            Some(1),
            "the later guard-restored session is counted on fatal exit",
        );
    }

    #[tokio::test]
    async fn fatal_abandonment_count_survives_concurrent_local_demotion() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let registry = InMemorySmSessionRegistry::new();
        let stream_id = "fatal-metric-demoted-before-q6-exit";
        let session = detached_session(
            stream_id,
            "romeo@example.com/phone".parse().expect("full jid"),
        );
        registry
            .store_session(session.clone())
            .await
            .expect("store detached session");
        let fatal_fence = tokio_util::sync::CancellationToken::new();
        let mut outcomes = super::RemainingSmDrainOutcomes {
            registry: &registry,
            fatal_fence: fatal_fence.clone(),
            observed: std::collections::HashSet::new(),
            accounted: std::collections::HashSet::new(),
            recorded: false,
        };
        outcomes.observe_drained(&[session]);
        fatal_fence.cancel();
        registry.forget_claim_locally(stream_id).await;
        assert!(registry.live_session_ids().expect("live IDs").is_empty());
        outcomes.record();
        drop(outcomes);
        assert_eq!(
            metrics.counter_sum("waddle.clustering.claims_abandoned_on_drain", &[]),
            Some(1),
        );
    }

    #[tokio::test]
    async fn graceful_shutdown_successful_confirm_drains_and_retires_ingress_stream() {
        let sm_registry = Arc::new(InMemorySmSessionRegistry::new());
        let state = create_test_websocket_state_with_sm_registry(Arc::clone(&sm_registry)).await;
        let stream_id = "shutdown-retire-stream";
        state
            .deps
            .protocol
            .ingress
            .enroll_stream(&SmSessionId::new(stream_id))
            .await
            .expect("enroll ingress");
        sm_registry
            .store_session(detached_session(
                stream_id,
                "romeo@example.com/phone".parse().expect("full jid"),
            ))
            .await
            .expect("store detached session");
        let drain_token = tokio_util::sync::CancellationToken::new();
        let drain_notify = Arc::new(Notify::new());
        let drain_task = tokio::spawn(run_graceful_shutdown_drain(
            Arc::clone(&state),
            drain_token.clone(),
            Arc::clone(&drain_notify),
            tokio_util::sync::CancellationToken::new(),
            drain_deadline(Duration::from_millis(400)),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        ));

        drain_token.cancel();
        tokio::time::timeout(Duration::from_secs(2), drain_notify.notified())
            .await
            .expect("graceful shutdown drain should finish");
        drain_task.await.expect("graceful drain task");
        let connection = state
            .deps
            .app_state
            .db_pool
            .global()
            .guard()
            .await
            .expect("database connection");
        let mut rows = connection
            .query(
                "SELECT 1 FROM ingress_sm_streams WHERE stream_id = ?",
                crate::db_params![stream_id],
            )
            .await
            .expect("query retired ingress stream");
        assert!(rows
            .next()
            .await
            .expect("read retired ingress stream")
            .is_none());
    }

    #[tokio::test]
    async fn graceful_shutdown_releases_row_backed_replay_without_duplicate_promotion() {
        let sm_registry = Arc::new(InMemorySmSessionRegistry::new());
        let pending = Arc::new(InMemoryPendingDeliveryStorage::unlimited());
        let pending_storage: Arc<dyn PendingDeliveryStorage> = pending.clone();
        let state = create_test_websocket_state_with_sm_registry_and_pending_storage(
            Arc::clone(&sm_registry),
            pending_storage,
        )
        .await;
        let stream_id = "shutdown-row-backed-replay";
        let recipient: jid::BareJid = "romeo@example.com".parse().expect("bare jid");
        let message = transient_message(&recipient, "already pending");
        pending
            .insert(PendingRow {
                id: PendingRowId::fresh(),
                recipient: recipient.clone(),
                original_receipt_at: chrono::Utc::now(),
                payload: PendingPayload::Transient(Box::new(message.clone())),
                flushed_in_session: None,
                outbound_sequence: None,
            })
            .await
            .expect("insert pending row");
        let session_id = SmSessionId::new(stream_id);
        let claimed = pending
            .claim_for_session(&recipient, &session_id)
            .await
            .expect("claim pending row");
        pending
            .record_pushed_at(&claimed[0].id, 11)
            .await
            .expect("bind pending row to SM sequence");

        let mut session = detached_session(
            stream_id,
            "romeo@example.com/phone".parse().expect("full jid"),
        );
        session.unacked_stanzas.push(DetachedUnackedStanza {
            ingress_receipts: Vec::new(),
            sequence: 11,
            stanza_xml: message_xml(&message),
            original_receipt_at: chrono::Utc::now(),
        });
        sm_registry
            .store_session(session)
            .await
            .expect("store detached session");

        let drain_token = tokio_util::sync::CancellationToken::new();
        let drain_notify = Arc::new(Notify::new());
        let drain_task = tokio::spawn(run_graceful_shutdown_drain(
            Arc::clone(&state),
            drain_token.clone(),
            Arc::clone(&drain_notify),
            tokio_util::sync::CancellationToken::new(),
            drain_deadline(Duration::from_millis(400)),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        ));
        drain_token.cancel();
        tokio::time::timeout(Duration::from_secs(2), drain_notify.notified())
            .await
            .expect("graceful shutdown drain should finish");
        drain_task.await.expect("graceful drain task");

        let rows = pending.list(&recipient).await.expect("list pending rows");
        assert_eq!(rows.len(), 1, "the replay copy must not be promoted twice");
        assert!(rows[0].flushed_in_session.is_none());
        assert!(rows[0].outbound_sequence.is_none());
    }

    #[tokio::test]
    async fn graceful_shutdown_bounds_stalled_per_session_promotion_and_restores_session() {
        let persistence =
            Arc::new(waddle_xmpp::stream_management::persistence::InMemorySmPersistence::new());
        let storage: Arc<dyn waddle_xmpp::stream_management::persistence::SmPersistenceStorage> =
            persistence.clone();
        let claim_store = Arc::new(InProcessClaimStore::new());
        let claims: Arc<dyn ClaimStore> = claim_store.clone();
        let sm_registry = Arc::new(
            InMemorySmSessionRegistry::new()
                .with_persistence(storage)
                .with_claim_store(
                    claims,
                    SharedNodeIdentity::new(NodeIdentity::new("sm-node", "shutdown-timeout")),
                ),
        );
        let pending: Arc<dyn PendingDeliveryStorage> =
            Arc::new(InMemoryPendingDeliveryStorage::unlimited());
        let blocking = Arc::new(HangingBlockingStorage {
            started: Notify::new(),
        });
        let state = create_test_websocket_state_with_sm_registry_pending_and_blocking(
            Arc::clone(&sm_registry),
            pending,
            blocking.clone(),
        )
        .await;
        let stream_id = "shutdown-stalled-session-promotion";
        let recipient: jid::BareJid = "romeo@example.com".parse().expect("bare jid");
        let message = transient_message(&recipient, "must survive timeout");
        let mut session = detached_session(
            stream_id,
            "romeo@example.com/phone".parse().expect("full jid"),
        );
        session.outbound_count = 11;
        session.unacked_stanzas.push(DetachedUnackedStanza {
            ingress_receipts: Vec::new(),
            sequence: 11,
            stanza_xml: message_xml(&message),
            original_receipt_at: chrono::Utc::now(),
        });
        sm_registry
            .store_session(session)
            .await
            .expect("store detached session");

        let drain_token = tokio_util::sync::CancellationToken::new();
        let drain_notify = Arc::new(Notify::new());
        let started = Instant::now();
        let drain_task = tokio::spawn(run_graceful_shutdown_drain(
            Arc::clone(&state),
            drain_token.clone(),
            Arc::clone(&drain_notify),
            tokio_util::sync::CancellationToken::new(),
            drain_deadline(Duration::from_millis(75)),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        ));
        drain_token.cancel();
        tokio::time::timeout(Duration::from_secs(1), blocking.started.notified())
            .await
            .expect("per-session blocklist read should start");
        tokio::time::timeout(Duration::from_millis(400), drain_notify.notified())
            .await
            .expect("shutdown should finish within its budget");
        drain_task.await.expect("graceful drain task");

        assert!(started.elapsed() < Duration::from_millis(300));
        assert_eq!(
            sm_registry.live_session_ids().expect("live inventory"),
            vec![stream_id.to_string()],
            "timed-out promotion must restore the session with its claim retained"
        );
        assert!(claim_store
            .current_claim(&Entity::new(EntityType::SmSession, stream_id))
            .await
            .expect("current claim")
            .is_some());
        assert!(persistence
            .get_session(&SmSessionId::new(stream_id))
            .await
            .expect("durable session read")
            .is_some());
        assert_eq!(
            persistence
                .list_unacked(&SmSessionId::new(stream_id))
                .await
                .expect("durable queue read")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn fatal_fence_cancels_stalled_q6_without_promoting_or_losing_replay() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let persistence =
            Arc::new(waddle_xmpp::stream_management::persistence::InMemorySmPersistence::new());
        let claim_store = Arc::new(InProcessClaimStore::new());
        let sm_registry = Arc::new(
            InMemorySmSessionRegistry::new()
                .with_persistence(persistence.clone())
                .with_claim_store(
                    claim_store.clone(),
                    SharedNodeIdentity::new(NodeIdentity::new("sm-node", "fatal-q6")),
                ),
        );
        let pending = Arc::new(InMemoryPendingDeliveryStorage::unlimited());
        let blocking = Arc::new(HangingBlockingStorage {
            started: Notify::new(),
        });
        let state = create_test_websocket_state_with_sm_registry_pending_and_blocking(
            Arc::clone(&sm_registry),
            pending.clone(),
            blocking.clone(),
        )
        .await;
        let fatal_fence = state.deps.app_state.node_lifecycle.fatal_fence_token();
        let stream_id = "fatal-q6-stalled-promotion";
        let recipient: jid::BareJid = "romeo@example.com".parse().expect("bare jid");
        let mut session = detached_session(
            stream_id,
            "romeo@example.com/phone".parse().expect("full jid"),
        );
        session.outbound_count = 11;
        session.unacked_stanzas.push(DetachedUnackedStanza {
            ingress_receipts: Vec::new(),
            sequence: 11,
            stanza_xml: message_xml(&transient_message(&recipient, "successor retains replay")),
            original_receipt_at: chrono::Utc::now(),
        });
        sm_registry
            .store_session(session)
            .await
            .expect("store detached session");

        let drain_token = tokio_util::sync::CancellationToken::new();
        let completion = tokio_util::sync::CancellationToken::new();
        let drain_task = tokio::spawn(run_graceful_shutdown_drain(
            Arc::clone(&state),
            drain_token.clone(),
            Arc::new(Notify::new()),
            completion.clone(),
            drain_deadline(Duration::from_secs(5)),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        ));
        drain_token.cancel();
        tokio::time::timeout(Duration::from_secs(1), blocking.started.notified())
            .await
            .expect("Q6 must reach the blocked promotion");
        fatal_fence.cancel();
        tokio::time::timeout(Duration::from_millis(400), drain_task)
            .await
            .expect("fatal fence must preempt Q6 promotion")
            .expect("Q6 task completes");

        assert!(completion.is_cancelled());
        assert_eq!(
            sm_registry.live_session_ids().expect("live inventory"),
            vec![stream_id.to_string()]
        );
        assert!(claim_store
            .current_claim(&Entity::new(EntityType::SmSession, stream_id))
            .await
            .expect("current claim")
            .is_some());
        assert!(persistence
            .get_session(&SmSessionId::new(stream_id))
            .await
            .expect("durable session read")
            .is_some());
        assert_eq!(
            persistence
                .list_unacked(&SmSessionId::new(stream_id))
                .await
                .expect("durable queue read")
                .len(),
            1
        );
        assert!(pending
            .list(&recipient)
            .await
            .expect("pending rows")
            .is_empty());
        assert_eq!(
            metrics.counter_sum("waddle.clustering.claims_abandoned_on_drain", &[]),
            Some(1),
        );
    }

    #[tokio::test]
    async fn graceful_shutdown_completes_after_ingress_budget_timeout() {
        let state = create_test_websocket_state_with_sm_registry(Arc::new(
            InMemorySmSessionRegistry::new(),
        ))
        .await;
        let held = state
            .deps
            .protocol
            .ingress
            .hold_test_commit(&SmSessionId::new("ingress-held"))
            .await;
        let drain_token = tokio_util::sync::CancellationToken::new();
        let drain_notify = Arc::new(Notify::new());
        let drain_task = tokio::spawn(run_graceful_shutdown_drain(
            Arc::clone(&state),
            drain_token.clone(),
            Arc::clone(&drain_notify),
            tokio_util::sync::CancellationToken::new(),
            drain_deadline(Duration::from_millis(50)),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        ));

        drain_token.cancel();
        tokio::time::timeout(Duration::from_millis(400), drain_notify.notified())
            .await
            .expect("shutdown should complete once the ingress drain budget expires");
        drain_task.await.expect("graceful drain task");

        drop(held);
        assert!(
            state
                .deps
                .protocol
                .ingress
                .drain_and_join(Duration::from_secs(1))
                .await
        );
    }

    #[tokio::test]
    async fn graceful_shutdown_times_out_stalled_promotion_retry_and_restores_it() {
        use waddle_xmpp::ownership::{ClaimStore, InProcessClaimStore, SharedNodeIdentity};

        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let persistence = Arc::new(HangingSmReadPersistence::default());
        let storage: Arc<dyn waddle_xmpp::stream_management::persistence::SmPersistenceStorage> =
            persistence.clone();
        let claim_store = Arc::new(InProcessClaimStore::new());
        let claims: Arc<dyn ClaimStore> = claim_store.clone();
        let sm_registry = Arc::new(
            InMemorySmSessionRegistry::new()
                .with_persistence(storage)
                .with_claim_store(
                    claims,
                    SharedNodeIdentity::new(NodeIdentity::new("sm-node", "shutdown-timeout")),
                ),
        );
        let state = create_test_websocket_state_with_sm_registry(Arc::clone(&sm_registry)).await;
        let stream_id = "shutdown-hung-promotion-retry";
        let mut session = detached_session(
            stream_id,
            "romeo@example.com/phone".parse().expect("full jid"),
        );
        session.max_resume_time = Some(1);
        sm_registry
            .store_session(session)
            .await
            .expect("store detached session");
        sm_registry
            .claim_session(stream_id)
            .await
            .expect("claim session")
            .expect("session exists");
        tokio::time::sleep(Duration::from_secs(2)).await;
        sm_registry
            .release_claim(stream_id)
            .await
            .expect("release expired claim into promotion retry");

        let drain_token = tokio_util::sync::CancellationToken::new();
        let drain_notify = Arc::new(Notify::new());
        let started = Instant::now();
        let drain_task = tokio::spawn(run_graceful_shutdown_drain(
            Arc::clone(&state),
            drain_token.clone(),
            Arc::clone(&drain_notify),
            tokio_util::sync::CancellationToken::new(),
            drain_deadline(Duration::from_millis(75)),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        ));

        drain_token.cancel();
        tokio::time::timeout(Duration::from_secs(1), persistence.read_started.notified())
            .await
            .expect("stalled promotion retry should start a persistence read");
        tokio::time::timeout(Duration::from_millis(400), drain_notify.notified())
            .await
            .expect("shutdown drain should stop once the SM drain budget expires");
        drain_task.await.expect("graceful drain task");

        assert!(
            started.elapsed() < Duration::from_millis(300),
            "the shutdown drain must re-check its deadline instead of hanging on stalled promotion retry IO"
        );
        assert_eq!(metrics.counter_sum("xmpp.sm.drain_timeout", &[]), Some(1));
        assert_eq!(
            metrics.counter_sum("waddle.clustering.claims_abandoned_on_drain", &[]),
            Some(1),
            "the stalled session is counted once when the deadline expires"
        );
        assert_eq!(
            sm_registry
                .live_session_ids()
                .expect("live inventory after timeout"),
            vec![stream_id.to_string()],
            "dropping the timed-out drain future must restore the parked promotion retry"
        );
    }
}

mod implementation;
#[cfg(test)]
#[cfg(feature = "clustering")]
use implementation::abandon_shutdown_session_if_claim_lost;
#[cfg(feature = "clustering")]
#[cfg(test)]
use implementation::collect_remote_muc_reconcile_candidates;
#[cfg(feature = "clustering")]
#[cfg(test)]
use implementation::current_sweep_context;
#[cfg(any(feature = "clustering", test))]
use implementation::janitor_sweep_span;
pub(crate) use implementation::max_drain_duration_from_env;

#[cfg(feature = "clustering")]
#[cfg(test)]
use implementation::orphan_sweep_heartbeat_outcome;
#[cfg(feature = "clustering")]
#[cfg(test)]
use implementation::orphan_work_span;
#[cfg(feature = "clustering")]
#[cfg(test)]
use implementation::reconcile_registered_room_or_self_fence;
#[cfg(feature = "clustering")]
#[cfg(test)]
use implementation::register_reclaimed_epoch_or_cleanup;
#[cfg(feature = "clustering")]
#[cfg(test)]
use implementation::remote_muc_sweep_outcome;
pub(crate) use implementation::run_graceful_shutdown_drain;
pub(crate) use implementation::run_ingress_retirement_sweep;
pub(crate) use implementation::run_local_muc_departure_sweep;
pub(crate) use implementation::run_notification_outbox_sweep;
#[cfg(feature = "clustering")]
#[cfg(test)]
use implementation::run_orphan_reaper_sweep_with_workers;
pub(crate) use implementation::run_pending_delivery_claim_sweep;
#[cfg(test)]
use implementation::run_pending_promotion_receipt_sweep;

#[cfg(test)]
use implementation::run_room_dormancy_sweep;
#[cfg(test)]
use implementation::run_sm_expiry_sweep_with_custody_cursor;
#[cfg(test)]
use implementation::run_sm_retries_with_budget;
#[cfg(test)]
use implementation::run_user_actor_reaper_sweep;
pub(crate) use implementation::spawn_auth_state_janitor;
pub(crate) use implementation::spawn_call_teardown_outbox_janitor;
pub(crate) use implementation::spawn_critical_registry_supervisor;
pub(crate) use implementation::spawn_destroy_completion_janitor;
pub(crate) use implementation::spawn_graceful_shutdown_drain;
pub(crate) use implementation::spawn_local_muc_departure_janitor;
pub(crate) use implementation::spawn_notification_outbox_janitor;
pub(crate) use implementation::spawn_orphan_reaper_janitor;
pub(crate) use implementation::spawn_pending_delivery_claim_janitor;
pub(crate) use implementation::spawn_push_service_publish_job_janitor;
#[cfg(feature = "clustering")]
pub(crate) use implementation::spawn_remote_muc_membership_reconciler;
pub(crate) use implementation::spawn_room_dormancy_janitor;
pub(crate) use implementation::spawn_room_effect_outbox_janitor;
#[cfg(test)]
use implementation::spawn_room_registry_lifetime_watch;
pub(crate) use implementation::spawn_sm_expiry_janitor;
pub(crate) use implementation::spawn_user_actor_reaper;
#[cfg(test)]
use implementation::spawn_user_registry_lifetime_watch;
#[cfg(feature = "clustering")]
#[cfg(test)]
use implementation::supervise_orphan_reaper_sweep;
pub(crate) use implementation::sweep_dormant_rooms_once;
pub(crate) use implementation::sweep_empty_user_actors_once;
#[cfg(test)]
use implementation::CriticalRegistryArm;
pub(crate) use implementation::DormancySweepCounts;

#[cfg(feature = "clustering")]
#[cfg(test)]
use implementation::ExactReleaseWork;
#[cfg(feature = "clustering")]
#[cfg(test)]
use implementation::OrphanReaperSupervisor;
#[cfg(feature = "clustering")]
#[cfg(test)]
use implementation::OrphanReaperWorkers;
#[cfg(feature = "clustering")]
#[cfg(test)]
use implementation::OrphanSweepOutcome;
#[cfg(feature = "clustering")]
#[cfg(test)]
use implementation::PendingSmHydration;
#[cfg(feature = "clustering")]
#[cfg(test)]
use implementation::ReclaimedRegistration;
#[cfg(feature = "clustering")]
#[cfg(test)]
use implementation::ReclaimedRegistrationContext;
#[cfg(test)]
#[cfg(feature = "clustering")]
use implementation::RemainingSmDrainOutcomes;

#[cfg(feature = "clustering")]
#[cfg(test)]
use implementation::SmHydrationWork;
pub(crate) use implementation::UserReaperSweepCounts;
#[cfg(feature = "clustering")]
#[cfg(test)]
use implementation::WorkEnqueueOutcome;

#[cfg(feature = "clustering")]
#[cfg(test)]
use implementation::ORPHANED_ROOM_RELEASE_TIMEOUT;

#[cfg(feature = "clustering")]
#[cfg(test)]
use implementation::ORPHAN_WORK_ATTEMPT_TIMEOUT;
#[cfg(feature = "clustering")]
#[cfg(test)]
use implementation::ORPHAN_WORK_QUEUE_CAPACITY;
