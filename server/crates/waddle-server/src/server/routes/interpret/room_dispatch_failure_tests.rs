//! Phase-A room infrastructure failures must leave the origin-id retryable.
use super::super::effects::{PlanFailure, PlanSink};
use super::*;
use crate::ingress::{
    commit::commit_submission, test_support::IngressFixture, IngressDecisionClass,
    IngressStreamIdentity, IngressSubmission,
};
use crate::ingress_uow::SmIngressStreamRepository;
use crate::server::routes::websocket::tests::create_test_websocket_state;
use kameo::actor::Spawn;
use waddle_xmpp::ingress::{DigestContext, DigestInput, NormalizedTarget, WireHandledCount};
use waddle_xmpp::muc::{
    room_actor::{ChangeAffiliation, Join},
    room_registry_actor::{CreateRoom, RoomRegistryActor},
};

async fn room_submission(fixture: &IngressFixture, room: &BareJid) -> IngressSubmission {
    let mut submission = fixture.submission(Some("dispatch-infrastructure-retry"), "room body");
    submission.target = NormalizedTarget::Bare(room.clone());
    submission.plan.sanitized_message.to = Some(room.clone().into());
    submission.plan.sanitized_message.type_ = XmppMessageType::Groupchat;
    submission.digest_input = DigestInput::from_parsed(
        &submission.plan.sanitized_message,
        &DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![room.clone()],
            stanza_lang: None,
        },
    )
    .expect("digest");
    let stream_id = waddle_xmpp::pending_delivery::SmSessionId::new("dispatch-failure-stream");
    let mut tx = fixture.uow.begin().await.expect("stream transaction");
    let sm_ingress_id = SmIngressStreamRepository::mint(&mut tx, &stream_id)
        .await
        .expect("stream");
    tx.commit().await.expect("stream commit");
    submission.identity = IngressStreamIdentity::Resumable {
        stream_id,
        sm_ingress_id,
        #[cfg(feature = "clustering")]
        owner: waddle_xmpp::ownership::NodeIdentity::new("unused", "single-node"),
        #[cfg(feature = "clustering")]
        claim_epoch: waddle_xmpp::ownership::ClaimEpoch(1),
        reserved_wire_position: WireHandledCount::new(1),
        checkpoint_h: WireHandledCount::new(1),
    };
    submission
}

async fn plan(submission: &mut IngressSubmission, deps: &Deps<'_>, room: &BareJid) {
    let sink = PlanSink::new();
    let capture = crate::ingress::IngressEffectCapture::new();
    let mut deps = deps.clone();
    deps.effects = &sink;
    deps.ingress_effect_capture = Some(capture.clone());
    let result = dispatch_to_room(
        &deps,
        room.clone(),
        submission.plan.sanitized_message.clone(),
        0,
    )
    .await;
    assert!(result.frames.is_empty(), "Phase A does not emit a reply");
    submission.plan = super::super::message_plan::finish_plan(
        &sink,
        &capture,
        submission.plan.sanitized_message.clone(),
        Some(submission.sender.clone()),
    );
}

async fn assert_refused(
    fixture: &IngressFixture,
    submission: &IngressSubmission,
    expected: PlanFailure,
    expected_class: IngressDecisionClass,
) {
    assert_eq!(submission.plan.failure, Some(expected));
    let failure = commit_submission(&fixture.uow, submission, 1)
        .await
        .expect_err("incomplete plan cannot commit");
    assert_eq!(failure.class(), expected_class);
    assert!(!failure.class().advances());
    for table in [
        "ingress_messages",
        "ingress_origin_aliases",
        "ingress_effect_intents",
        "ingress_effect_receipts",
        "ingress_sm_refs",
        "ingress_deliveries",
        "mam_messages",
        "inbox_entries",
    ] {
        assert_eq!(fixture.count(table).await, 0, "no writes to {table}");
    }
    assert_checkpoint(fixture, submission, 0).await;
}

async fn assert_checkpoint(
    fixture: &IngressFixture,
    submission: &IngressSubmission,
    expected: u32,
) {
    let IngressStreamIdentity::Resumable { sm_ingress_id, .. } = &submission.identity else {
        panic!("resumable");
    };
    let mut tx = fixture.uow.begin().await.expect("checkpoint transaction");
    assert_eq!(
        SmIngressStreamRepository::load_stream_checkpoint(&mut tx, *sm_ingress_id)
            .await
            .expect("checkpoint"),
        Some(WireHandledCount::new(expected))
    );
    tx.commit().await.expect("checkpoint read");
}

async fn healthy_room(
    rooms: &kameo::actor::ActorRef<RoomRegistryActor>,
    room: &BareJid,
    sender: &FullJid,
) -> kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor> {
    let actor = rooms
        .ask(CreateRoom {
            room_jid: room.clone(),
            waddle_id: "dispatch".into(),
            channel_id: "dispatch".into(),
            config: Default::default(),
        })
        .await
        .expect("create room");
    actor
        .ask(ChangeAffiliation {
            jid: sender.to_bare(),
            affiliation: waddle_xmpp::Affiliation::Member,
        })
        .await
        .expect("membership");
    actor
        .ask(Join {
            nick: "romeo".into(),
            real_jid: sender.clone(),
            role: waddle_xmpp::Role::Participant,
            affiliation: waddle_xmpp::Affiliation::Member,
        })
        .await
        .expect("join");
    actor
}

fn room_registry() -> kameo::actor::ActorRef<RoomRegistryActor> {
    RoomRegistryActor::spawn(RoomRegistryActor::new(
        "muc.example.com".into(),
        waddle_xmpp::xep::xep0421::OccupantIdSecret::new(vec![b'd'; 32]).expect("secret"),
    ))
}

async fn actor_failure(fixture: IngressFixture, snapshot: bool) {
    let state = create_test_websocket_state().await;
    let room: BareJid = "dispatch@muc.example.com".parse().expect("room");
    let mut submission = room_submission(&fixture, &room).await;
    let rooms = room_registry();
    let actor = healthy_room(&rooms, &room, &submission.sender).await;
    let store = Arc::new(BlockedRestore::default());
    let restore = if snapshot {
        let blocked_actor = actor.clone();
        let blocked_store = store.clone();
        let fence = waddle_xmpp::muc::RoomClaimFenceContext::new(
            waddle_xmpp::ownership::Entity::new(
                waddle_xmpp::ownership::EntityType::RoomActor,
                room.to_string(),
            ),
            waddle_xmpp::ownership::NodeIdentity::local(),
            waddle_xmpp::ownership::ClaimEpoch(1),
        );
        let task = tokio::spawn(async move {
            blocked_actor
                .ask(waddle_xmpp::muc::room_actor::RestoreDurableRoomState {
                    store: blocked_store,
                    claim_fence: fence,
                })
                .await
                .expect("restore");
        });
        store.entered.notified().await;
        Some(task)
    } else {
        rooms.stop_gracefully().await.expect("stop registry");
        rooms.wait_for_shutdown().await;
        None
    };
    let mut deps = crate::server::routes::websocket::interpret_loop::build_interpret_deps(
        state.as_ref(),
        None,
    );
    deps.room_registry = Some(&rooms);
    plan(&mut submission, &deps, &room).await;
    assert_refused(
        &fixture,
        &submission,
        PlanFailure::RoomSnapshotUnavailable,
        IngressDecisionClass::Storage,
    )
    .await;
    if let Some(restore) = restore {
        store.release.notify_one();
        restore.await.expect("restore task");
    }
    let healthy_rooms = room_registry();
    let healthy_actor = healthy_room(&healthy_rooms, &room, &submission.sender).await;
    deps.room_registry = Some(&healthy_rooms);
    plan(&mut submission, &deps, &room).await;
    assert_eq!(submission.plan.failure, None);
    assert!(submission
        .plan
        .intents
        .iter()
        .any(|intent| matches!(intent, IngressEffectIntent::ArchiveAuthoritative { .. })));
    let decision = commit_submission(&fixture.uow, &submission, 3)
        .await
        .expect("healthy same-origin retry");
    assert_eq!(decision.class, IngressDecisionClass::Accepted);
    assert_eq!(fixture.count("ingress_origin_aliases").await, 1);
    assert_checkpoint(&fixture, &submission, 1).await;
    actor.kill();
    rooms.kill();
    healthy_actor.kill();
    healthy_rooms.kill();
    fixture.close().await;
}

#[derive(Default)]
struct BlockedRestore {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
impl waddle_xmpp::muc::durable::MucDurableStore for BlockedRestore {
    fn load_room_state_fenced<'a>(
        &'a self,
        _room: &'a BareJid,
        _fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
    ) -> waddle_xmpp::muc::durable::MucDurableFuture<
        'a,
        Option<waddle_xmpp::muc::durable::DurableRoomState>,
    > {
        Box::pin(async move {
            self.entered.notify_one();
            self.release.notified().await;
            Ok(None)
        })
    }
    fn commit_room_mutation<'a>(
        &'a self,
        _room: &'a BareJid,
        _fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
        _intent: waddle_xmpp::muc::RoomDurableMutation,
        _effects: waddle_xmpp::muc::RoomMutationEffects,
    ) -> waddle_xmpp::muc::RoomCommitFuture<'a> {
        Box::pin(async { panic!("planning must not mutate room") })
    }
    fn check_exact_claim_fence<'a>(
        &'a self,
        _room: &'a BareJid,
        _fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
    ) -> waddle_xmpp::muc::durable::MucDurableFuture<'a, bool> {
        Box::pin(async { Ok(true) })
    }
}

#[tokio::test]
async fn ingress_room_dispatch_registry_failure_retry_sqlite() {
    actor_failure(IngressFixture::sqlite().await, false).await;
}
#[tokio::test]
async fn ingress_room_dispatch_registry_failure_retry_postgres() {
    if let Some(fixture) = IngressFixture::postgres("dispatch_registry").await {
        actor_failure(fixture, false).await;
    }
}
#[tokio::test]
async fn ingress_room_dispatch_snapshot_failure_retry_sqlite() {
    actor_failure(IngressFixture::sqlite().await, true).await;
}
#[tokio::test]
async fn ingress_room_dispatch_snapshot_failure_retry_postgres() {
    if let Some(fixture) = IngressFixture::postgres("dispatch_snapshot").await {
        actor_failure(fixture, true).await;
    }
}

#[cfg(feature = "clustering")]
async fn claim_failure(fixture: IngressFixture, stale: bool) {
    use super::super::message_plan::ownership_plan_tests::PlanningClaims;
    use waddle_xmpp::ownership::{Entity, EntityType, NodeIdentity, SharedNodeIdentity};
    let room: BareJid = "remote-dispatch@muc.example.com".parse().expect("room");
    let mut submission = room_submission(&fixture, &room).await;
    let claims = Arc::new(PlanningClaims::new(NodeIdentity::new("remote", "epoch")));
    claims.fail_reads(!stale);
    claims.set_stale(stale);
    let state =
        crate::server::routes::websocket::tests::create_test_websocket_state_with_clustering(
            crate::clustering::ClusteringHandles {
                claim_store: Some(claims.clone()),
                node_identity: Some(SharedNodeIdentity::new(NodeIdentity::new("local", "epoch"))),
                ..Default::default()
            },
            Arc::new(InMemorySmSessionRegistry::new()),
        )
        .await;
    let mut deps = Deps::registry_only(&state.deps.protocol.connection_registry);
    deps.web_socket_state = Some(state.as_ref());
    deps.room_registry = Some(&state.deps.protocol.room_registry);
    let sender_entity = Entity::new(
        EntityType::UserActor,
        submission.sender.to_bare().to_string(),
    );
    deps.ordered_relay_origin = Some(OrderedRelayRouteOrigin {
        kind: OrderedRelayRouteOriginKind::Entity(sender_entity.clone()),
        sender_entity,
        inbound_sequence: 1,
        handoff: None,
    });
    plan(&mut submission, &deps, &room).await;
    let (expected_failure, expected_class) = if stale {
        (
            PlanFailure::RoomClaimStale,
            IngressDecisionClass::RoomGenerationStale,
        )
    } else {
        (PlanFailure::OwnershipLookup, IngressDecisionClass::Storage)
    };
    assert_refused(&fixture, &submission, expected_failure, expected_class).await;
    assert!(!submission
        .plan
        .intents
        .iter()
        .any(|intent| matches!(intent, IngressEffectIntent::DispatchToRoomRemote { .. })));
    claims.fail_reads(false);
    claims.set_stale(false);
    plan(&mut submission, &deps, &room).await;
    assert_eq!(submission.plan.failure, None);
    assert!(submission.plan.intents.iter().any(|intent| matches!(intent, IngressEffectIntent::DispatchToRoomRemote { room: target, .. } if target == &room)));
    let decision = commit_submission(&fixture.uow, &submission, 3)
        .await
        .expect("healthy same-origin retry");
    assert_eq!(decision.class, IngressDecisionClass::Accepted);
    assert_eq!(fixture.count("ingress_origin_aliases").await, 1);
    assert_checkpoint(&fixture, &submission, 1).await;
    fixture.close().await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn ingress_room_dispatch_claim_failure_retry_sqlite() {
    claim_failure(IngressFixture::sqlite().await, false).await;
}
#[cfg(feature = "clustering")]
#[tokio::test]
async fn ingress_room_dispatch_claim_failure_retry_postgres() {
    if let Some(fixture) = IngressFixture::postgres("dispatch_claim").await {
        claim_failure(fixture, false).await;
    }
}
#[cfg(feature = "clustering")]
#[tokio::test]
async fn ingress_room_dispatch_stale_claim_retry_sqlite() {
    claim_failure(IngressFixture::sqlite().await, true).await;
}
#[cfg(feature = "clustering")]
#[tokio::test]
async fn ingress_room_dispatch_stale_claim_retry_postgres() {
    if let Some(fixture) = IngressFixture::postgres("dispatch_stale").await {
        claim_failure(fixture, true).await;
    }
}
