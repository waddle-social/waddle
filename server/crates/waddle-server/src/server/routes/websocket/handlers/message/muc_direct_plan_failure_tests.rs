//! XEP-0045 prerequisite outages must leave the original ingress identity retryable.
use crate::ingress::{
    commit::commit_submission, test_support::IngressFixture, IngressDecisionClass,
    IngressStreamIdentity, IngressSubmission,
};
use crate::ingress_uow::SmIngressStreamRepository;
use crate::server::routes::interpret::{effects::PlanFailure, plan_message_dispatch};
use crate::server::routes::websocket::{
    tests::{create_test_websocket_state, create_test_websocket_state_with_db_pool_and_ingress},
    WebSocketState,
};
use jid::{BareJid, FullJid};
use std::sync::Arc;
use waddle_xmpp::ingress::{
    DigestContext, DigestInput, IngressEffectIntent, NormalizedTarget, WireHandledCount,
};
use waddle_xmpp::muc::{room_actor::Join, room_registry_actor::CreateRoom};
use xmpp_parsers::message::MessageType;

async fn state(fixture: &IngressFixture) -> Arc<WebSocketState> {
    let pool = crate::db::DatabasePool::new(
        crate::db::DatabaseConfig::new(fixture.db.driver(), fixture.db.database_url()),
        crate::db::PoolConfig,
    )
    .await
    .expect("database pool");
    let standalone = create_test_websocket_state().await;
    create_test_websocket_state_with_db_pool_and_ingress(
        Arc::new(pool),
        standalone.deps.protocol.ingress.clone(),
    )
    .await
}

async fn submission(fixture: &IngressFixture, decline: bool) -> IngressSubmission {
    let room: BareJid = "direct-failure@muc.example.com".parse().expect("room");
    let mut submission = fixture.submission(Some("muc-direct-prerequisite-retry"), "");
    let target: jid::Jid = if decline {
        room.clone().into()
    } else {
        room.with_resource_str("juliet").expect("occupant").into()
    };
    submission.target = if decline {
        NormalizedTarget::Bare(room)
    } else {
        NormalizedTarget::Full(target.clone().try_into_full().expect("full target"))
    };
    let message = &mut submission.plan.sanitized_message;
    message.to = Some(target);
    message.bodies.clear();
    message.type_ = if decline {
        MessageType::Normal
    } else {
        MessageType::Chat
    };
    if decline {
        message.payloads.push(
            minidom::Element::builder("x", waddle_xmpp::muc::presence::NS_MUC_USER)
                .append(
                    minidom::Element::builder("decline", waddle_xmpp::muc::presence::NS_MUC_USER)
                        .build(),
                )
                .build(),
        );
    } else {
        message.payloads.push(
            minidom::Element::builder("no-copy", waddle_xmpp::xep::xep0334::NS_HINTS).build(),
        );
    }
    submission.digest_input = DigestInput::from_parsed(
        message,
        &DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![],
            stanza_lang: None,
        },
    )
    .expect("digest");
    let stream_id = waddle_xmpp::pending_delivery::SmSessionId::new("muc-direct-failure-stream");
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

async fn plan(submission: &mut IngressSubmission, state: &WebSocketState) {
    let mut machine = waddle_xmpp::protocol::XmppStateMachine::new(
        "example.com",
        waddle_xmpp::protocol::StanzaDispatcher::new(),
    );
    machine.transition_to_ready(submission.sender.clone(), false);
    let deps = crate::server::routes::websocket::interpret_loop::build_interpret_deps(state, None);
    submission.plan = plan_message_dispatch(
        &mut machine,
        submission.plan.sanitized_message.clone(),
        &deps,
    )
    .await;
}

async fn checkpoint(fixture: &IngressFixture, submission: &IngressSubmission, expected: u32) {
    let IngressStreamIdentity::Resumable { sm_ingress_id, .. } = &submission.identity else {
        panic!("resumable")
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

async fn refused(fixture: &IngressFixture, submission: &IngressSubmission, expected: PlanFailure) {
    assert_eq!(submission.plan.failure, Some(expected));
    let error = commit_submission(&fixture.uow, submission, 1)
        .await
        .expect_err("incomplete plan");
    assert_eq!(error.class(), IngressDecisionClass::Storage);
    assert!(!error.class().advances());
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
    checkpoint(fixture, submission, 0).await;
}

async fn accepted(fixture: &IngressFixture, submission: &IngressSubmission) {
    assert_eq!(submission.plan.failure, None);
    assert!(submission.plan.error_reply.is_none());
    let decision = commit_submission(&fixture.uow, submission, 3)
        .await
        .expect("healthy same-origin retry");
    assert_eq!(decision.class, IngressDecisionClass::Accepted);
    assert_eq!(fixture.count("ingress_origin_aliases").await, 1);
    checkpoint(fixture, submission, 1).await;
}

async fn room(
    state: &WebSocketState,
    sender: &FullJid,
) -> kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor> {
    let actor = state
        .deps
        .protocol
        .room_registry
        .ask(CreateRoom {
            room_jid: "direct-failure@muc.example.com".parse().expect("room"),
            waddle_id: "direct-failure".into(),
            channel_id: "direct-failure".into(),
            config: Default::default(),
        })
        .await
        .expect("create room");
    for (nick, real_jid) in [
        ("romeo", sender.clone()),
        (
            "juliet",
            "juliet@example.com/phone".parse().expect("recipient"),
        ),
    ] {
        actor
            .ask(Join {
                nick: nick.into(),
                real_jid,
                role: waddle_xmpp::Role::Participant,
                affiliation: waddle_xmpp::Affiliation::Member,
            })
            .await
            .expect("join");
    }
    actor
}

async fn actor_failure(fixture: IngressFixture, snapshot: bool) {
    let broken = state(&fixture).await;
    let mut submission = submission(&fixture, false).await;
    let actor = room(&broken, &submission.sender).await;
    let store = Arc::new(BlockedRestore::default());
    let restore = if snapshot {
        let blocked_actor = actor.clone();
        let blocked_store = store.clone();
        let room: BareJid = "direct-failure@muc.example.com".parse().expect("room");
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
        broken
            .deps
            .protocol
            .room_registry
            .stop_gracefully()
            .await
            .expect("stop registry");
        broken.deps.protocol.room_registry.wait_for_shutdown().await;
        None
    };
    plan(&mut submission, &broken).await;
    refused(&fixture, &submission, PlanFailure::RoomSnapshotUnavailable).await;
    if let Some(restore) = restore {
        store.release.notify_one();
        restore.await.expect("restore task");
    }
    let healthy = state(&fixture).await;
    let healthy_actor = room(&healthy, &submission.sender).await;
    plan(&mut submission, &healthy).await;
    assert!(submission
        .plan
        .intents
        .iter()
        .any(|intent| matches!(intent, IngressEffectIntent::RouteOccupantPm { .. })));
    accepted(&fixture, &submission).await;
    actor.kill();
    healthy_actor.kill();
    broken.deps.protocol.room_registry.kill();
    healthy.deps.protocol.room_registry.kill();
    drop(broken);
    drop(healthy);
    fixture.close().await;
}

async fn ledger_failure(fixture: IngressFixture) {
    use crate::server::routes::websocket::muc_invites::{
        list_invites, record_invite, OutstandingInvite,
    };
    let state = state(&fixture).await;
    let mut submission = submission(&fixture, true).await;
    let invite = OutstandingInvite {
        room: "direct-failure@muc.example.com".parse().expect("room"),
        invitee: submission.sender.to_bare(),
        inviter: "juliet@example.com".parse().expect("inviter"),
    };
    let actor = state.deps.app_state.db_pool.global_actor().clone();
    record_invite(actor.clone(), &invite)
        .await
        .expect("seed invite");
    fixture
        .execute(
            "ALTER TABLE muc_pending_invites RENAME TO unavailable_muc_pending_invites",
            (),
        )
        .await;
    plan(&mut submission, &state).await;
    refused(&fixture, &submission, PlanFailure::InvitePrerequisiteRead).await;
    assert_eq!(fixture.count("unavailable_muc_pending_invites").await, 1);
    fixture
        .execute(
            "ALTER TABLE unavailable_muc_pending_invites RENAME TO muc_pending_invites",
            (),
        )
        .await;
    plan(&mut submission, &state).await;
    assert!(submission.plan.intents.iter().any(|intent| matches!(intent, IngressEffectIntent::MucInviteLedger { mutation } if mutation.action == waddle_xmpp::ingress::MucInviteLedgerAction::Claimed)));
    accepted(&fixture, &submission).await;
    assert_eq!(
        list_invites(actor, &invite.room, &invite.invitee)
            .await
            .expect("ledger remains until execution"),
        vec![invite]
    );
    drop(state);
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
async fn ingress_muc_direct_registry_failure_retry_sqlite() {
    actor_failure(IngressFixture::sqlite().await, false).await;
}
#[tokio::test]
async fn ingress_muc_direct_registry_failure_retry_postgres() {
    if let Some(fixture) = IngressFixture::postgres("muc_direct_registry").await {
        actor_failure(fixture, false).await;
    }
}
#[tokio::test]
async fn ingress_muc_direct_snapshot_failure_retry_sqlite() {
    actor_failure(IngressFixture::sqlite().await, true).await;
}
#[tokio::test]
async fn ingress_muc_direct_snapshot_failure_retry_postgres() {
    if let Some(fixture) = IngressFixture::postgres("muc_direct_snapshot").await {
        actor_failure(fixture, true).await;
    }
}
#[tokio::test]
async fn ingress_muc_direct_decline_ledger_failure_retry_sqlite() {
    ledger_failure(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn ingress_muc_direct_decline_ledger_failure_retry_postgres() {
    if let Some(fixture) = IngressFixture::postgres("muc_direct_decline").await {
        ledger_failure(fixture).await;
    }
}

async fn missing_room(fixture: IngressFixture) {
    let state = state(&fixture).await;
    let mut submission = submission(&fixture, false).await;
    plan(&mut submission, &state).await;
    assert_eq!(submission.plan.failure, None);
    let reply = submission
        .plan
        .error_reply
        .as_ref()
        .expect("missing room error");
    let waddle_xmpp::Stanza::Message(reply) = reply else {
        panic!("message error")
    };
    assert!(reply.payloads.iter().any(|payload| payload
        .children()
        .any(|child| child.name() == "item-not-found")));
    let decision = commit_submission(&fixture.uow, &submission, 3)
        .await
        .expect("authoritative missing room denial");
    assert_eq!(decision.class, IngressDecisionClass::PolicyDenied);
    assert!(decision.class.advances());
    assert_eq!(fixture.count("ingress_messages").await, 1);
    assert_eq!(fixture.count("ingress_sm_refs").await, 1);
    checkpoint(&fixture, &submission, 1).await;
    drop(state);
    fixture.close().await;
}

#[tokio::test]
async fn ingress_muc_direct_missing_room_commits_denial_sqlite() {
    missing_room(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn ingress_muc_direct_missing_room_commits_denial_postgres() {
    if let Some(fixture) = IngressFixture::postgres("muc_direct_missing").await {
        missing_room(fixture).await;
    }
}
