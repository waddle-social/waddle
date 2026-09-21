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
    /// The local room actor answers, no longer lists the occupant, and this
    /// node cannot reach it either: the copy is owed to nobody.
    Departed,
    /// The local room actor still lists the occupant.
    StillJoined,
    /// No local actor hosts the room: absence is unproven.
    NoLocalRoom,
    /// One departed occupant beside one still-deliverable detached occupant.
    Mixed,
    /// The roster says the occupant left, but this node can still hand it the
    /// copy (#1803 H1). Rosters are memory-only, so a room-host restart makes
    /// every frozen pre-restart occupant read "absent" — settling on that
    /// would permanently drop copies for users sitting right here. The copy
    /// is DELIVERED instead, in the same pass.
    ReachableDeparted,
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
    // H1: a copy is only ever SETTLED for an occupant this node cannot reach.
    // Whoever keeps a resumable session here gets the copy delivered instead,
    // whatever the roster says.
    let attached: Vec<jid::FullJid> = match case {
        DepartedCase::Mixed => vec![live.clone()],
        DepartedCase::ReachableDeparted => vec![ghost.clone()],
        _ => Vec::new(),
    };
    for resource in &attached {
        store_detached(&sm, resource).await;
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
    // Copies this attempt SETTLED (dropped as owed to nobody).
    let settled_copies = u64::from(matches!(case, DepartedCase::Departed | DepartedCase::Mixed));
    // Whether the frozen fanout is complete — by settlement, by delivery, or
    // by both.
    let settled = matches!(
        case,
        DepartedCase::Departed | DepartedCase::Mixed | DepartedCase::ReachableDeparted
    );
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
        i64::try_from(attached.len()).expect("append count"),
        "exactly the reachable occupants' copies are appended"
    );
    if case == DepartedCase::ReachableDeparted {
        assert_eq!(
            append_count(&sm, &ghost).await,
            1,
            "#1803 H1: a memory-only roster must not permanently drop a copy \
             for an occupant this node can still hand it to"
        );
    }
    if case == DepartedCase::Mixed {
        assert_eq!(append_count(&sm, &live).await, 1);
    }
    assert_eq!(
        metrics.counter_sum(DEPARTED_COPIES, &[]).unwrap_or(0),
        before + settled_copies,
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

/// #1803 review: the reachability probes can spend the whole 500 ms fan-out
/// budget, and the web client reuses its `web-<uuid>` resource across
/// reconnects within a page — so the same full JID can rejoin between the
/// probes and the write. The roster is asked once more immediately before the
/// write, and a rejoined resource keeps its copy: the rejoin is a NEW
/// occupancy, and the frozen copy is left to the ordinary rebuild.
#[tokio::test]
async fn sqlite_rejoin_before_the_settlement_write_keeps_its_copy() {
    use crate::ingress::recovery_departed::{hook_settlement_write_window, SettlementWriteWindow};

    /// Rejoins the departed occupant's full JID exactly once, from inside the
    /// window between the reachability probes and the settlement write.
    struct RejoinOnce {
        room: kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor>,
        occupant: jid::FullJid,
        done: std::sync::atomic::AtomicBool,
        ran: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    #[async_trait::async_trait]
    impl SettlementWriteWindow for RejoinOnce {
        async fn enter(&self, occupants: &[jid::FullJid]) {
            if !occupants.contains(&self.occupant)
                || self.done.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                return;
            }
            self.ran.store(true, std::sync::atomic::Ordering::SeqCst);
            let admission_revision = self
                .room
                .ask(waddle_xmpp::muc::room_actor::GetSnapshot)
                .await
                .expect("room snapshot")
                .admission_revision;
            self.room
                .ask(waddle_xmpp::muc::room_actor::JoinWithAffiliation {
                    sender_jid: self.occupant.clone(),
                    nick: "alice".to_owned(),
                    affiliation_grant: waddle_xmpp::muc::room_actor::JoinAffiliationGrant::Resolver(
                        waddle_xmpp::Affiliation::Member,
                    ),
                    local_domain: "example.com".to_owned(),
                    admission_revision,
                    session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
                })
                .await
                .expect("the client rebinds the same resource and rejoins");
        }
    }

    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let before = metrics.counter_sum(DEPARTED_COPIES, &[]).unwrap_or(0);
    let f = IngressFixture::sqlite().await;
    let sm = persistent_sm(&f).await;
    let state = state_for(&f, sm.clone()).await;
    let occupant: jid::FullJid = "alice@example.com/phone".parse().expect("occupant");
    let submission = planned_room(&f, &state, Case::Lost, std::slice::from_ref(&occupant)).await;
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
    depart(&state, &occupant).await;

    let room = state
        .deps
        .protocol
        .room_registry
        .ask(GetRoom {
            room_jid: "recovery@muc.example.com".parse().expect("room"),
        })
        .await
        .expect("registry lookup")
        .expect("local room actor");
    let hook_ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    hook_settlement_write_window(
        key,
        std::sync::Arc::new(RejoinOnce {
            room,
            occupant: occupant.clone(),
            done: std::sync::atomic::AtomicBool::new(false),
            ran: std::sync::Arc::clone(&hook_ran),
        }) as std::sync::Arc<dyn SettlementWriteWindow>,
    );

    let env: Arc<dyn RecoveryEnvironment> = Arc::new(RoomOverrideEnvironment {
        state: state.clone(),
        registry: None,
    });
    assert_eq!(
        pass(&f, &env, &MaintenanceCursor::default()).await,
        MaintenanceOutcome::Complete
    );

    assert!(
        hook_ran.load(std::sync::atomic::Ordering::SeqCst),
        "the settlement-write hook must have run"
    );
    let mut tx = f.uow.begin().await.expect("inspect recovered row");
    assert!(
        DeliveryProgressRepository::load(&mut tx, key, &receipt)
            .await
            .expect("delivery progress")
            .is_empty(),
        "a resource that rejoined before the write is owed its copy"
    );
    assert!(
        !EffectReceiptRepository::contains(
            &mut tx,
            key,
            receipt.kind,
            &receipt.semantic_identity_hash
        )
        .await
        .expect("aggregate receipt"),
        "the frozen fanout is not complete"
    );
    assert!(!CanonicalMessageRepository::is_terminal(&mut tx, key)
        .await
        .expect("terminal"));
    tx.commit().await.expect("read commit");
    assert_eq!(
        metrics.counter_sum(DEPARTED_COPIES, &[]).unwrap_or(0),
        before,
        "nothing was settled"
    );
    f.close().await;
}

/// #1803 review: the occupants of one route are probed in bounded chunks, and
/// each chunk's proven departures are written BEFORE the next chunk is probed.
/// A row deadline that cuts a later chunk can then only lose that chunk — the
/// prefix this attempt already proved is durable, so the next pass starts from
/// it instead of re-proving everything from scratch.
#[tokio::test]
async fn sqlite_a_settled_chunk_survives_a_deadline_that_cuts_the_next_one() {
    use crate::ingress::recovery_departed::{
        hook_settlement_write_window, SettlementWriteWindow, PROBE_CONCURRENCY,
    };

    /// Records every chunk that reaches the settlement write, and stalls the
    /// SECOND one past the row deadline.
    struct StallTheSecondChunk {
        chunks: std::sync::Mutex<Vec<Vec<jid::FullJid>>>,
    }

    #[async_trait::async_trait]
    impl SettlementWriteWindow for StallTheSecondChunk {
        async fn enter(&self, occupants: &[jid::FullJid]) {
            let seen = {
                let mut chunks = self.chunks.lock().expect("chunk log");
                chunks.push(occupants.to_vec());
                chunks.len()
            };
            if seen > 1 {
                // Far beyond the row deadline: this attempt is cancelled here,
                // with the first chunk already committed.
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        }
    }

    let f = IngressFixture::sqlite().await;
    let sm = persistent_sm(&f).await;
    let state = state_for(&f, sm.clone()).await;
    // One more occupant than a single chunk can hold, so the settlement has to
    // come back for a second one.
    let occupants: Vec<jid::FullJid> = (0..=PROBE_CONCURRENCY)
        .map(|index| {
            format!("ghost{index:02}@example.com/phone")
                .parse()
                .expect("occupant")
        })
        .collect();
    let submission = planned_room(&f, &state, Case::Lost, &occupants).await;
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
    for occupant in &occupants {
        depart(&state, occupant).await;
    }

    let hook = std::sync::Arc::new(StallTheSecondChunk {
        chunks: std::sync::Mutex::new(Vec::new()),
    });
    hook_settlement_write_window(
        key,
        std::sync::Arc::clone(&hook) as std::sync::Arc<dyn SettlementWriteWindow>,
    );
    let env: Arc<dyn RecoveryEnvironment> = Arc::new(RoomOverrideEnvironment {
        state: state.clone(),
        registry: None,
    });
    pass_with_row_deadline(
        &f,
        &env,
        &MaintenanceCursor::default(),
        Duration::from_secs(1),
    )
    .await;

    let chunks = hook.chunks.lock().expect("chunk log").clone();
    assert_eq!(
        chunks.len(),
        2,
        "the occupants must be probed in bounded chunks: {chunks:?}"
    );
    assert_eq!(chunks[0].len(), PROBE_CONCURRENCY);
    let mut first_chunk = chunks[0].clone();
    first_chunk.sort();
    let mut tx = f.uow.begin().await.expect("inspect recovered row");
    let mut progress = DeliveryProgressRepository::load(&mut tx, key, &receipt)
        .await
        .expect("delivery progress");
    progress.sort();
    assert_eq!(
        progress, first_chunk,
        "a cancelled chunk must not take the proven prefix with it"
    );
    assert!(
        !CanonicalMessageRepository::is_terminal(&mut tx, key)
            .await
            .expect("terminal"),
        "the occupant the deadline cut is still owed its copy"
    );
    tx.commit().await.expect("read commit");
    f.close().await;
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

/// #1803 H1: a memory-only roster must not permanently drop a copy for an
/// occupant this node can still hand it to. The copy is delivered by the
/// ordinary rebuild in the same pass, so the row still terminalizes — it just
/// does not terminalize by dropping a message.
#[tokio::test]
async fn sqlite_departed_but_reachable_occupant_is_delivered_not_settled() {
    departed_recovery(
        IngressFixture::sqlite().await,
        DepartedCase::ReachableDeparted,
    )
    .await;
}
#[tokio::test]
async fn postgres_departed_but_reachable_occupant_is_delivered_not_settled() {
    if let Some(f) = IngressFixture::postgres("departed_reachable").await {
        departed_recovery(f, DepartedCase::ReachableDeparted).await;
    }
}

/// Durable room ownership — and therefore room claim fences — exist only
/// behind the `clustering` feature, so the proof these cases exercise has no
/// meaning without it.
#[cfg(feature = "clustering")]
mod fenced_authority {
    use super::*;

    /// What the durable store says about the incarnation's retained claim fence
    /// when the settlement asks (#1803 F2).
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum FenceCase {
        /// The exact `(entity, epoch, node)` tuple is still on file.
        Current,
        /// A steal committed: this incarnation is demoted or sealed
        /// `OwnershipLost` and is serving a roster from before it lost the room.
        /// `RoomActor::durable_claim_fence` is assigned once and never cleared,
        /// so the snapshot still reports `Some(fence)` — only the store can tell.
        Superseded,
        /// The store cannot answer, so nothing is proven.
        Unreadable,
        /// The incarnation never held a fence at all.
        NoFence,
    }

    /// Durable room store that answers exactly one question: is this
    /// incarnation's fence still the room's?
    struct FenceProofStore(FenceCase);

    impl waddle_xmpp::muc::durable::MucDurableStore for FenceProofStore {
        fn load_room_state_fenced<'a>(
            &'a self,
            _room: &'a jid::BareJid,
            _fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
        ) -> waddle_xmpp::muc::durable::MucDurableFuture<
            'a,
            Option<waddle_xmpp::muc::durable::DurableRoomState>,
        > {
            Box::pin(async { Ok(None) })
        }

        fn commit_room_mutation<'a>(
            &'a self,
            _room: &'a jid::BareJid,
            _fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
            _intent: waddle_xmpp::muc::RoomDurableMutation,
            _effects: waddle_xmpp::muc::RoomMutationEffects,
        ) -> waddle_xmpp::muc::RoomCommitFuture<'a> {
            Box::pin(async { Err(waddle_xmpp::muc::RoomCommitError::OwnershipUnavailable) })
        }

        fn check_exact_claim_fence<'a>(
            &'a self,
            _room: &'a jid::BareJid,
            _fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
        ) -> waddle_xmpp::muc::durable::MucDurableFuture<'a, bool> {
            let case = self.0;
            Box::pin(async move {
                match case {
                    FenceCase::Current => Ok(true),
                    FenceCase::Superseded => Ok(false),
                    FenceCase::Unreadable | FenceCase::NoFence => {
                        Err(waddle_xmpp::XmppError::Internal("store unreadable".into()))
                    }
                }
            })
        }
    }

    /// #1803 F2: with durable room ownership configured, a roster answer only
    /// settles a copy while the store still holds this incarnation's EXACT claim.
    /// A demoted or `OwnershipLost` incarnation keeps reporting a fence and keeps
    /// serving the roster it had, so trusting the snapshot alone would let it
    /// permanently drop a copy for an occupant who joined on the new owner.
    async fn fenced_departed_recovery(f: IngressFixture, case: FenceCase) {
        use waddle_xmpp::muc::room_actor::RestoreDurableRoomState;

        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let before = metrics.counter_sum(DEPARTED_COPIES, &[]).unwrap_or(0);
        let sm = persistent_sm(&f).await;
        let planning_state = state_for(&f, sm.clone()).await;
        let ghost: jid::FullJid = "alice@example.com/phone".parse().expect("ghost");
        let submission = planned_room(
            &f,
            &planning_state,
            Case::Lost,
            std::slice::from_ref(&ghost),
        )
        .await;
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
        settle_non_delivery_effects(&f, &planning_state, &decision).await;

        let store = Arc::new(FenceProofStore(case));
        let state = socket_tests::create_test_websocket_state_with_clustering(
            crate::clustering::ClusteringHandles {
                muc_durable_store: Some(store.clone()),
                ..Default::default()
            },
            sm.clone(),
        )
        .await;
        let room: jid::BareJid = "recovery@muc.example.com".parse().expect("room");
        let actor = state
            .deps
            .protocol
            .room_registry
            .ask(waddle_xmpp::muc::room_registry_actor::CreateRoom {
                room_jid: room.clone(),
                waddle_id: "recovery".into(),
                channel_id: "recovery".into(),
                config: Default::default(),
            })
            .await
            .expect("room");
        // The room is hosted here and does NOT list the frozen occupant, and the
        // occupant has no session anywhere: only the fence proof can decide.
        if case != FenceCase::NoFence {
            actor
                .ask(RestoreDurableRoomState {
                    store: store.clone() as Arc<dyn waddle_xmpp::muc::durable::MucDurableStore>,
                    claim_fence: waddle_xmpp::muc::RoomClaimFenceContext::new(
                        waddle_xmpp::ownership::Entity::new(
                            waddle_xmpp::ownership::EntityType::RoomActor,
                            room.to_string(),
                        ),
                        waddle_xmpp::ownership::NodeIdentity::local(),
                        waddle_xmpp::ownership::ClaimEpoch(1),
                    ),
                })
                .await
                .expect("install the durable room store");
        }
        assert_eq!(
            actor
                .ask(waddle_xmpp::muc::room_actor::GetSnapshot)
                .await
                .expect("room snapshot")
                .claim_fence
                .is_some(),
            case != FenceCase::NoFence,
            "the actor's own snapshot is exactly the evidence that is not enough"
        );

        let env: Arc<dyn RecoveryEnvironment> = Arc::new(StateEnvironment(state.clone()));
        assert_eq!(
            pass(&f, &env, &MaintenanceCursor::default()).await,
            MaintenanceOutcome::Complete
        );

        let settled = case == FenceCase::Current;
        let mut tx = f.uow.begin().await.expect("inspect recovered row");
        assert_eq!(
            DeliveryProgressRepository::load(&mut tx, key, &receipt)
                .await
                .expect("delivery progress"),
            if settled { vec![ghost.clone()] } else { vec![] },
            "only a proven-current fence may settle a copy"
        );
        assert_eq!(
            CanonicalMessageRepository::is_terminal(&mut tx, key)
                .await
                .expect("terminal"),
            settled
        );
        tx.commit().await.expect("read commit");
        assert_eq!(
            metrics.counter_sum(DEPARTED_COPIES, &[]).unwrap_or(0),
            before + u64::from(settled)
        );
        f.close().await;
    }

    #[tokio::test]
    async fn sqlite_fenced_room_with_a_current_claim_settles_the_departed_copy() {
        fenced_departed_recovery(IngressFixture::sqlite().await, FenceCase::Current).await;
    }

    /// The F2 regression: a superseded incarnation serving a stale roster must not
    /// permanently drop a copy for an occupant who joined on the new owner.
    #[tokio::test]
    async fn sqlite_superseded_room_incarnation_settles_nothing() {
        fenced_departed_recovery(IngressFixture::sqlite().await, FenceCase::Superseded).await;
    }

    #[tokio::test]
    async fn sqlite_unreadable_room_fence_settles_nothing() {
        fenced_departed_recovery(IngressFixture::sqlite().await, FenceCase::Unreadable).await;
    }

    #[tokio::test]
    async fn sqlite_unfenced_room_incarnation_settles_nothing() {
        fenced_departed_recovery(IngressFixture::sqlite().await, FenceCase::NoFence).await;
    }

    /// #1803 F2, in-transaction half (R2-2): the settlement asserts the EXACT
    /// room claim under `FOR SHARE` inside its own transaction, so a steal
    /// that commits between the roster probe and the write rolls the
    /// settlement back instead of dropping a copy for an occupant who joined
    /// on the new owner.
    ///
    /// Postgres-only by construction: `assert_room_claim` needs a unit of work
    /// opened with a bound node identity, and
    /// `IngressUnitOfWork::open_with_node_identity` refuses any other driver.
    async fn clustered_settlement_asserts_the_room_claim(
        mut f: IngressFixture,
        steal_the_claim: bool,
    ) {
        use crate::server::routes::interpret::effects::{self, Effect};
        use waddle_xmpp::muc::room_actor::RestoreDurableRoomState;

        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let before = metrics.counter_sum(DEPARTED_COPIES, &[]).unwrap_or(0);
        let sm = persistent_sm(&f).await;
        let planning_state = state_for(&f, sm.clone()).await;
        let ghost: jid::FullJid = "alice@example.com/phone".parse().expect("ghost");
        let room: jid::BareJid = "recovery@muc.example.com".parse().expect("room");
        // Seeds the real `clustering_claims` row AND reopens the fixture's
        // unit of work with that node identity bound.
        let room_fence = f.room_fence(&room).await;
        let mut submission = planned_room(
            &f,
            &planning_state,
            Case::Lost,
            std::slice::from_ref(&ghost),
        )
        .await;
        // A clustered unit of work refuses an unfenced room commit, so the
        // plan has to carry the same fence the settlement will re-assert.
        if let effects::RoomExecutionPath::Local { fence, .. } = &mut submission.plan.room_execution
        {
            *fence = effects::room::RoomFenceRequirement::Guarded(room_fence.clone());
        }
        for planned in &mut submission.plan.plan {
            if let Effect::Durable(effects::DurableEffect::Room(
                effects::room::DurableRoomEffect::ArchiveGroupchat { fence, .. },
            )) = &mut planned.effect
            {
                *fence = effects::room::RoomFenceRequirement::Guarded(room_fence.clone());
            }
        }
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
        settle_non_delivery_effects(&f, &planning_state, &decision).await;

        // The recovering incarnation hosts the room, carries the same fence,
        // and the store agrees it is current — so ONLY the in-transaction
        // assertion can still stop the settlement.
        let store = Arc::new(FenceProofStore(FenceCase::Current));
        let state = socket_tests::create_test_websocket_state_with_clustering(
            crate::clustering::ClusteringHandles {
                muc_durable_store: Some(store.clone()),
                ..Default::default()
            },
            sm.clone(),
        )
        .await;
        let actor = state
            .deps
            .protocol
            .room_registry
            .ask(waddle_xmpp::muc::room_registry_actor::CreateRoom {
                room_jid: room.clone(),
                waddle_id: "recovery".into(),
                channel_id: "recovery".into(),
                config: Default::default(),
            })
            .await
            .expect("room");
        actor
            .ask(RestoreDurableRoomState {
                store: store.clone() as Arc<dyn waddle_xmpp::muc::durable::MucDurableStore>,
                claim_fence: room_fence.clone(),
            })
            .await
            .expect("install the durable room store");
        if steal_the_claim {
            // Another node won the room between the roster probe and the
            // write. The actor has not noticed and still serves its roster.
            f.execute(
                "UPDATE clustering_claims SET claim_epoch = claim_epoch + 1 WHERE entity = ?",
                crate::db_params![format!("room_actor:{room}")],
            )
            .await;
        }

        let env: Arc<dyn RecoveryEnvironment> = Arc::new(StateEnvironment(state.clone()));
        assert_eq!(
            pass(&f, &env, &MaintenanceCursor::default()).await,
            MaintenanceOutcome::Complete
        );

        let settled = !steal_the_claim;
        let mut tx = f.uow.begin().await.expect("inspect recovered row");
        assert_eq!(
            DeliveryProgressRepository::load(&mut tx, key, &receipt)
                .await
                .expect("delivery progress"),
            if settled { vec![ghost.clone()] } else { vec![] },
            "a settlement that lost the claim rolls back and drops nothing"
        );
        assert_eq!(
            CanonicalMessageRepository::is_terminal(&mut tx, key)
                .await
                .expect("terminal"),
            settled
        );
        tx.commit().await.expect("read commit");
        assert_eq!(
            metrics.counter_sum(DEPARTED_COPIES, &[]).unwrap_or(0),
            before + u64::from(settled)
        );
        f.close().await;
    }

    #[tokio::test]
    async fn postgres_clustered_settlement_commits_under_a_matching_room_claim() {
        if let Some(f) = IngressFixture::postgres("settle_fenced_ok").await {
            clustered_settlement_asserts_the_room_claim(f, false).await;
        }
    }

    /// The probe-to-commit TOCTOU: the store still vouched for the fence when
    /// the roster was read, and the claim moved before the write landed.
    #[tokio::test]
    async fn postgres_clustered_settlement_rolls_back_when_the_room_claim_moves() {
        if let Some(f) = IngressFixture::postgres("settle_fenced_stolen").await {
            clustered_settlement_asserts_the_room_claim(f, true).await;
        }
    }
}
