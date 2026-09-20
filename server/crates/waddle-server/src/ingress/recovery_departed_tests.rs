//! XEP-0045 ghost users: maintenance settles copies the room no longer owes.
use super::*;
use crate::ingress::IngressDecision;
use kameo::actor::Spawn;
use waddle_xmpp::{
    muc::{
        durable::OccupancyLeaveCause,
        room_actor::{LeaveAttemptId, LeaveByRealJid, LeaveOrigin, LeaveSessionSelector},
        room_registry_actor::{GetRoom, RoomRegistryActor},
    },
    xep::xep0421::OccupantIdSecret,
};

const DEPARTED_COPIES: &str = "ingress.maintenance.departed_occupant_copies";

#[derive(Clone, Copy, PartialEq, Eq)]
enum DepartedCase {
    /// The local room actor answers and no longer lists the occupant.
    Departed,
    /// The local room actor still lists the occupant.
    StillJoined,
    /// No local actor hosts the room: absence is unproven.
    NoLocalRoom,
    /// One departed occupant beside one still-deliverable detached occupant.
    Mixed,
}

/// Recovery against the planning process, optionally with a registry that does
/// not host the room, so "no local actor" is exercised without a second state.
struct RoomOverrideEnvironment {
    state: Arc<WebSocketState>,
    registry: Option<kameo::actor::ActorRef<RoomRegistryActor>>,
}

impl RecoveryEnvironment for RoomOverrideEnvironment {
    fn recovery_deps(&self) -> Deps<'_> {
        let mut deps = build_interpret_deps(&self.state, None);
        if let Some(registry) = self.registry.as_ref() {
            deps.room_registry = Some(registry);
        }
        deps
    }
}

async fn departed_recovery(f: IngressFixture, case: DepartedCase) {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let before = metrics.counter_sum(DEPARTED_COPIES, &[]).unwrap_or(0);
    let sm = persistent_sm(&f).await;
    let state = state_for(&f, sm.clone()).await;
    let ghost: jid::FullJid = "alice@example.com/phone".parse().expect("ghost");
    let live: jid::FullJid = "ben@example.com/phone".parse().expect("live occupant");
    let resources = if case == DepartedCase::Mixed {
        vec![ghost.clone(), live.clone()]
    } else {
        vec![ghost.clone()]
    };
    // A resumable session makes every frozen copy deliverable, so only room
    // occupancy can decide whether it is still owed.
    let resumable = matches!(case, DepartedCase::Departed | DepartedCase::Mixed);
    if resumable {
        for resource in &resources {
            store_detached(&sm, resource).await;
        }
    }
    let submission = planned_room(&f, &state, Case::Lost, &resources).await;
    let muc = submission
        .plan
        .intents
        .iter()
        .find(|intent| matches!(intent, IngressEffectIntent::RouteMucGroupchat { .. }))
        .cloned()
        .expect("room fanout intent");
    let receipt = receipt_key(&muc).expect("MUC receipt");
    let decision = commit_submission(&f.uow, &submission, 1)
        .await
        .expect("room commit");
    let key = decision.message_key.expect("key");
    settle_non_delivery_effects(&f, &state, &decision).await;
    if case != DepartedCase::StillJoined {
        depart(&state, &ghost).await;
    }
    let registry = (case == DepartedCase::NoLocalRoom).then(|| {
        RoomRegistryActor::spawn(RoomRegistryActor::new(
            "muc.example.com".into(),
            OccupantIdSecret::new(vec![b'd'; 32]).expect("occupant-id secret"),
        ))
    });
    let env: Arc<dyn RecoveryEnvironment> = Arc::new(RoomOverrideEnvironment {
        state: state.clone(),
        registry,
    });
    assert_eq!(
        pass(&f, &env, &MaintenanceCursor::default()).await,
        MaintenanceOutcome::Complete
    );
    let settled = matches!(case, DepartedCase::Departed | DepartedCase::Mixed);
    let mut tx = f.uow.begin().await.expect("inspect recovered row");
    let mut progress = DeliveryProgressRepository::load(&mut tx, key, &receipt)
        .await
        .expect("delivery progress");
    progress.sort();
    assert_eq!(
        progress,
        if settled { resources.clone() } else { vec![] },
        "departed copies are durable progress"
    );
    assert_eq!(
        EffectReceiptRepository::contains(
            &mut tx,
            key,
            receipt.kind,
            &receipt.semantic_identity_hash
        )
        .await
        .expect("aggregate receipt"),
        settled
    );
    assert_eq!(
        CanonicalMessageRepository::is_terminal(&mut tx, key)
            .await
            .expect("terminal"),
        settled
    );
    tx.commit().await.expect("read commit");
    assert_eq!(
        f.count("sm_ingress_appends").await,
        i64::from(case == DepartedCase::Mixed),
        "only a live occupant's copy is appended"
    );
    if resumable {
        assert_eq!(
            append_count(&sm, &ghost).await,
            0,
            "XEP-0045: a departed occupant is owed no groupchat copy"
        );
    }
    if case == DepartedCase::Mixed {
        assert_eq!(append_count(&sm, &live).await, 1);
    }
    assert_eq!(
        metrics.counter_sum(DEPARTED_COPIES, &[]).unwrap_or(0),
        before + u64::from(settled),
        "one departed copy per settled occupant"
    );
    f.close().await;
}

/// Leave the row exactly as an interrupted acceptance would: every recorded
/// obligation but the frozen occupant copies already receipted.
pub(super) async fn settle_non_delivery_effects(
    f: &IngressFixture,
    state: &Arc<WebSocketState>,
    decision: &IngressDecision,
) {
    let mut mutations = decision.clone();
    let indices: Vec<_> = decision
        .external
        .iter()
        .enumerate()
        .filter_map(|(index, effect)| {
            (!matches!(effect, ExternalEffect::Delivery(_))).then_some(index)
        })
        .collect();
    mutations.external = indices
        .iter()
        .map(|index| decision.external[*index].clone())
        .collect();
    mutations.external_dependencies = indices
        .iter()
        .map(|index| decision.external_dependencies[*index].clone())
        .collect();
    mutations.external_receipts = indices
        .iter()
        .map(|index| decision.external_receipts[*index].clone())
        .collect();
    let deps = build_interpret_deps(state, None);
    let report = execute_effects(
        &f.uow,
        &f.db,
        &mutations,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert!(
        report.receipt_failures.is_empty(),
        "non-delivery effects persisted: {report:?}"
    );
}

async fn depart(state: &Arc<WebSocketState>, occupant: &jid::FullJid) {
    let room: jid::BareJid = "recovery@muc.example.com".parse().expect("room");
    let actor = state
        .deps
        .protocol
        .room_registry
        .ask(GetRoom { room_jid: room })
        .await
        .expect("registry lookup")
        .expect("local room actor");
    actor
        .ask(LeaveByRealJid {
            sender_jid: occupant.clone(),
            cause: OccupancyLeaveCause::Disconnect,
            session: LeaveSessionSelector::Any,
            attempt: LeaveAttemptId::generate(),
            origin: LeaveOrigin::Fresh,
        })
        .await
        .expect("occupant leaves the room");
}

#[tokio::test]
async fn sqlite_departed_occupant_copy_settles() {
    departed_recovery(IngressFixture::sqlite().await, DepartedCase::Departed).await;
}
#[tokio::test]
async fn postgres_departed_occupant_copy_settles() {
    if let Some(f) = IngressFixture::postgres("departed_settle").await {
        departed_recovery(f, DepartedCase::Departed).await;
    }
}

#[tokio::test]
async fn sqlite_departed_joined_occupant_stays_pending() {
    departed_recovery(IngressFixture::sqlite().await, DepartedCase::StillJoined).await;
}
#[tokio::test]
async fn postgres_departed_joined_occupant_stays_pending() {
    if let Some(f) = IngressFixture::postgres("departed_joined").await {
        departed_recovery(f, DepartedCase::StillJoined).await;
    }
}

#[tokio::test]
async fn sqlite_departed_without_local_room_stays_pending() {
    departed_recovery(IngressFixture::sqlite().await, DepartedCase::NoLocalRoom).await;
}
#[tokio::test]
async fn postgres_departed_without_local_room_stays_pending() {
    if let Some(f) = IngressFixture::postgres("departed_no_room").await {
        departed_recovery(f, DepartedCase::NoLocalRoom).await;
    }
}

/// An eviction writes a delivery-progress row, which is durable progress: it
/// must restart the stall streak exactly as a delivered copy does, so a row
/// whose evidence just changed is never parked for a cooldown.
#[tokio::test]
async fn sqlite_departed_copy_resets_the_maintenance_stall_streak() {
    let f = IngressFixture::sqlite().await;
    let sm = persistent_sm(&f).await;
    let state = state_for(&f, sm.clone()).await;
    let ghost: jid::FullJid = "alice@example.com/phone".parse().expect("ghost");
    let live: jid::FullJid = "ben@example.com/phone".parse().expect("live occupant");
    // Neither occupant is reachable, so only the settlement can make progress.
    let resources = vec![ghost.clone(), live.clone()];
    // The occupant that stays seated keeps a resumable session in the SHARED
    // DURABLE store and none in this node's memory: its copy is undeliverable
    // here, but it is not an XEP-0045 ghost, so the streak this test measures
    // runs to its parking rather than ending in an eviction (#1803).
    let elsewhere = persistent_sm(&f).await;
    store_detached(&elsewhere, &live).await;
    let submission = planned_room(&f, &state, Case::Lost, &resources).await;
    let decision = commit_submission(&f.uow, &submission, 1)
        .await
        .expect("room commit");
    let key = decision.message_key.expect("key");
    settle_non_delivery_effects(&f, &state, &decision).await;
    let env: Arc<dyn RecoveryEnvironment> = Arc::new(RoomOverrideEnvironment {
        state: state.clone(),
        registry: None,
    });
    let cursor = MaintenanceCursor::default();
    for attempt in 1..=2 {
        assert_eq!(pass(&f, &env, &cursor).await, MaintenanceOutcome::Complete);
        cursor.wait_for_recovery_accounting().await;
        assert_eq!(super::super::super::attempt_count(key), attempt);
    }
    depart(&state, &ghost).await;
    for attempt in 3..=6 {
        assert_eq!(pass(&f, &env, &cursor).await, MaintenanceOutcome::Complete);
        cursor.wait_for_recovery_accounting().await;
        assert_eq!(
            super::super::super::attempt_count(key),
            attempt,
            "the settled departed copy restarted the streak"
        );
    }
    assert_eq!(pass(&f, &env, &cursor).await, MaintenanceOutcome::Complete);
    cursor.wait_for_recovery_accounting().await;
    assert_eq!(
        super::super::super::attempt_count(key),
        6,
        "parked only after three stalls with unchanged evidence"
    );
    f.close().await;
}

#[tokio::test]
async fn sqlite_departed_mixed_fanout_completes() {
    departed_recovery(IngressFixture::sqlite().await, DepartedCase::Mixed).await;
}
#[tokio::test]
async fn postgres_departed_mixed_fanout_completes() {
    if let Some(f) = IngressFixture::postgres("departed_mixed").await {
        departed_recovery(f, DepartedCase::Mixed).await;
    }
}
