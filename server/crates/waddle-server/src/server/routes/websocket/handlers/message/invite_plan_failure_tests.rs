//! Invitation prerequisite failures preserve the resumable ingress position.
use super::*;
use crate::ingress::{
    commit::commit_submission, test_support::IngressFixture, IngressDecisionClass,
    IngressStreamIdentity, IngressSubmission,
};
use crate::ingress_uow::SmIngressStreamRepository;
use crate::server::routes::interpret::effects::{EffectSink, IngressPlan, PlanSink};
use std::sync::Arc;
use waddle_xmpp::ingress::{DigestContext, DigestInput, NormalizedTarget, WireHandledCount};

async fn submission(fixture: &IngressFixture, plan: IngressPlan) -> IngressSubmission {
    let mut submission = fixture.submission(Some("invite-prerequisite-retry"), "");
    submission.target = NormalizedTarget::Bare(
        plan.sanitized_message
            .to
            .as_ref()
            .expect("target")
            .to_bare(),
    );
    submission.plan = plan;
    submission.digest_input = DigestInput::from_parsed(
        &submission.plan.sanitized_message,
        &DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![submission.sender.to_bare()],
            stanza_lang: None,
        },
    )
    .expect("digest");
    let stream_id = waddle_xmpp::pending_delivery::SmSessionId::new("invite-prerequisite-stream");
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
async fn checkpoint(fixture: &IngressFixture, submission: &IngressSubmission, expected: u32) {
    let IngressStreamIdentity::Resumable { sm_ingress_id, .. } = submission.identity else {
        panic!("resumable")
    };
    let mut tx = fixture.uow.begin().await.expect("checkpoint transaction");
    assert_eq!(
        SmIngressStreamRepository::load_stream_checkpoint(&mut tx, sm_ingress_id)
            .await
            .expect("checkpoint"),
        Some(WireHandledCount::new(expected))
    );
    tx.commit().await.expect("checkpoint read");
}
async fn refused(fixture: &IngressFixture, submission: &IngressSubmission, expected: PlanFailure) {
    assert_eq!(submission.plan.failure, Some(expected));
    let failure = commit_submission(&fixture.uow, submission, 1)
        .await
        .expect_err("incomplete plan");
    assert_eq!(failure.class(), IngressDecisionClass::Storage);
    assert!(!failure.class().advances());
    for table in [
        "ingress_messages",
        "ingress_origin_aliases",
        "ingress_effect_intents",
        "ingress_effect_receipts",
        "ingress_sm_refs",
    ] {
        assert_eq!(fixture.count(table).await, 0, "no writes to {table}");
    }
    checkpoint(fixture, submission, 0).await;
}
async fn recovered(
    fixture: &IngressFixture,
    submission: &IngressSubmission,
) -> crate::ingress::IngressDecision {
    assert_eq!(submission.plan.failure, None);
    let decision = commit_submission(&fixture.uow, submission, 3)
        .await
        .expect("healthy retry");
    assert_eq!(decision.class, IngressDecisionClass::Accepted);
    assert_eq!(fixture.count("ingress_origin_aliases").await, 1);
    checkpoint(fixture, submission, 1).await;
    decision
}
async fn deliver_recovered(
    fixture: &IngressFixture,
    submission: &IngressSubmission,
    state: &WebSocketState,
) {
    let decision = recovered(fixture, submission).await;
    let deps = crate::server::routes::websocket::interpret_loop::build_interpret_deps(state, None);
    let report = crate::ingress::execute::execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &crate::server::routes::interpret::effects::ImmediateSink,
        &deps,
        std::time::Duration::from_secs(5),
    )
    .await;
    assert!(report.receipt_failures.is_empty(), "{report:?}");
    let recipient = "juliet@example.com".parse().expect("recipient");
    assert_eq!(
        state
            .deps
            .protocol
            .pending_delivery_storage
            .list(&recipient)
            .await
            .expect("delivered invitation")
            .len(),
        1
    );
}

#[derive(Debug, thiserror::Error)]
#[error("injected invitation blocklist read failure")]
struct ReadFailure;
struct FailedBlocklist;
#[async_trait::async_trait]
impl waddle_xmpp::xep::xep0191::BlockingStorage for FailedBlocklist {
    async fn list_blocked_jids(
        &self,
        _: &jid::BareJid,
    ) -> Result<Vec<jid::BareJid>, waddle_xmpp::xep::xep0191::BlockingStorageError> {
        Err(waddle_xmpp::xep::xep0191::BlockingStorageError::new(
            ReadFailure,
        ))
    }
}
async fn blocklist_failure(fixture: IngressFixture, group_dm: bool) {
    let plan = super::tests::invite_plan_with_blocking(Arc::new(FailedBlocklist), group_dm).await;
    let mut submission = submission(&fixture, plan).await;
    refused(&fixture, &submission, PlanFailure::InvitePrerequisiteRead).await;
    submission.plan = super::tests::invite_plan_with_blocking(
        Arc::new(waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new()),
        group_dm,
    )
    .await;
    recovered(&fixture, &submission).await;
    fixture.close().await;
}

#[tokio::test]
async fn ingress_muc_invite_blocklist_failure_retry_sqlite() {
    blocklist_failure(IngressFixture::sqlite().await, false).await;
}

#[tokio::test]
async fn ingress_group_dm_invite_blocklist_failure_retry_sqlite() {
    blocklist_failure(IngressFixture::sqlite().await, true).await;
}

#[tokio::test]
async fn ingress_muc_invite_blocklist_failure_retry_postgres() {
    if let Some(fixture) = IngressFixture::postgres("muc_invite_blocklist").await {
        blocklist_failure(fixture, false).await;
    }
}

#[tokio::test]
async fn ingress_group_dm_invite_blocklist_failure_retry_postgres() {
    if let Some(fixture) = IngressFixture::postgres("group_dm_invite_blocklist").await {
        blocklist_failure(fixture, true).await;
    }
}

async fn invite_plan(
    state: &WebSocketState,
    message: &xmpp_parsers::message::Message,
    group_dm: bool,
) -> IngressPlan {
    let sink = PlanSink::new();
    let capture = crate::ingress::IngressEffectCapture::new();
    let mut deps =
        crate::server::routes::websocket::interpret_loop::build_interpret_deps(state, None)
            .with_ingress_effect_capture(Some(capture.clone()));
    deps.effects = &sink;
    let sender = "romeo@example.com/phone".parse().expect("sender");
    let session = crate::auth::Session::new("romeo@example.com", "romeo", "romeo");
    let frames = if group_dm {
        handle_group_dm_mediated_invite(message, state, &sender, Some(&session), &deps).await
    } else {
        super::super::muc_invite::handle_muc_mediated_invite(
            message,
            state,
            &sender,
            Some(&session),
            &deps,
        )
        .await
    }
    .expect("invite consumed");
    let (plan, room_execution) = sink.take();
    IngressPlan {
        failure: sink.failure(),
        plan,
        room_execution,
        intents: capture.snapshot().intents,
        sanitized_message: message.clone(),
        rejection: sink.rejection(),
        error_reply: frames.into_iter().next(),
    }
}

async fn shared_state(fixture: &IngressFixture) -> Arc<WebSocketState> {
    let standalone = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    let pool = crate::db::DatabasePool::new(
        crate::db::DatabaseConfig::new(fixture.db.driver(), fixture.db.database_url()),
        crate::db::PoolConfig,
    )
    .await
    .expect("shared pool");
    crate::server::routes::websocket::tests::create_test_websocket_state_with_db_pool_and_ingress(
        Arc::new(pool),
        standalone.deps.protocol.ingress.clone(),
    )
    .await
}
async fn invite_room(
    state: &WebSocketState,
    group_dm: bool,
) -> (
    xmpp_parsers::message::Message,
    kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor>,
) {
    let room = "invite-retry@muc.example.com".parse().expect("room");
    let sender: jid::FullJid = "romeo@example.com/phone".parse().expect("sender");
    crate::server::routes::websocket::tests::seed_local_account(state, "juliet").await;
    let actor = if group_dm {
        super::tests::create_group_dm_room(state, &room, "invite-retry").await
    } else {
        state
            .deps
            .protocol
            .room_registry
            .ask(waddle_xmpp::muc::room_registry_actor::CreateRoom {
                room_jid: room.clone(),
                waddle_id: "test".into(),
                channel_id: "invite-retry".into(),
                config: Default::default(),
            })
            .await
            .expect("room")
    };
    actor
        .ask(waddle_xmpp::muc::room_actor::Join {
            nick: "romeo".into(),
            real_jid: sender.clone(),
            role: waddle_xmpp::Role::Moderator,
            affiliation: waddle_xmpp::Affiliation::Admin,
        })
        .await
        .expect("join");
    actor
        .ask(ChangeAffiliation {
            jid: sender.to_bare(),
            affiliation: waddle_xmpp::Affiliation::Admin,
        })
        .await
        .expect("admin");
    let mut message = super::tests::group_dm_invite_message(&room, &sender, "juliet@example.com");
    waddle_xmpp_core::xep0359::add_origin_id(&mut message, "invite-prerequisite-retry");
    (message, actor)
}

async fn directory_failure(fixture: IngressFixture, group_dm: bool) {
    let state = shared_state(&fixture).await;
    let (message, actor) = invite_room(&state, group_dm).await;
    fixture
        .execute(
            "ALTER TABLE native_users RENAME TO temporarily_unavailable_native_users",
            (),
        )
        .await;
    let mut submission = submission(&fixture, invite_plan(&state, &message, group_dm).await).await;
    refused(&fixture, &submission, PlanFailure::InvitePrerequisiteRead).await;
    fixture
        .execute(
            "ALTER TABLE temporarily_unavailable_native_users RENAME TO native_users",
            (),
        )
        .await;
    submission.plan = invite_plan(&state, &message, group_dm).await;
    deliver_recovered(&fixture, &submission, &state).await;
    actor.kill();
    drop(state);
    fixture.close().await;
}
#[tokio::test]
async fn ingress_muc_invite_directory_failure_retry_sqlite() {
    directory_failure(IngressFixture::sqlite().await, false).await;
}
#[tokio::test]
async fn ingress_muc_invite_directory_failure_retry_postgres() {
    if let Some(fixture) = IngressFixture::postgres("muc_invite_directory").await {
        directory_failure(fixture, false).await;
    }
}

use jid::BareJid;
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

async fn snapshot_failure(fixture: IngressFixture, group_dm: bool) {
    let state = shared_state(&fixture).await;
    let (message, actor) = invite_room(&state, group_dm).await;
    let store = Arc::new(BlockedRestore::default());
    let blocked_actor = actor.clone();
    let blocked_store = store.clone();
    let room = message.to.as_ref().expect("room").to_bare();
    let fence = waddle_xmpp::muc::RoomClaimFenceContext::new(
        waddle_xmpp::ownership::Entity::new(
            waddle_xmpp::ownership::EntityType::RoomActor,
            room.to_string(),
        ),
        waddle_xmpp::ownership::NodeIdentity::local(),
        waddle_xmpp::ownership::ClaimEpoch(1),
    );
    let restore = tokio::spawn(async move {
        blocked_actor
            .ask(waddle_xmpp::muc::room_actor::RestoreDurableRoomState {
                store: blocked_store,
                claim_fence: fence,
            })
            .await
            .expect("restore");
    });
    store.entered.notified().await;
    let mut submission = submission(&fixture, invite_plan(&state, &message, group_dm).await).await;
    refused(&fixture, &submission, PlanFailure::RoomSnapshotUnavailable).await;
    store.release.notify_one();
    restore.await.expect("restore task");
    submission.plan = invite_plan(&state, &message, group_dm).await;
    recovered(&fixture, &submission).await;
    actor.kill();
    drop(state);
    fixture.close().await;
}

#[tokio::test]
async fn ingress_muc_invite_snapshot_failure_retry_sqlite() {
    snapshot_failure(IngressFixture::sqlite().await, false).await;
}

#[tokio::test]
async fn ingress_group_dm_invite_snapshot_failure_retry_sqlite() {
    snapshot_failure(IngressFixture::sqlite().await, true).await;
}

#[tokio::test]
async fn ingress_muc_invite_snapshot_failure_retry_postgres() {
    if let Some(fixture) = IngressFixture::postgres("muc_invite_snapshot").await {
        snapshot_failure(fixture, false).await;
    }
}

#[tokio::test]
async fn ingress_group_dm_invite_snapshot_failure_retry_postgres() {
    if let Some(fixture) = IngressFixture::postgres("group_dm_invite_snapshot").await {
        snapshot_failure(fixture, true).await;
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CancelAfter {
    Membership,
    LedgerReceipt,
    LedgerWithoutReceipt,
}
async fn membership_cancel_replay(fixture: IngressFixture, cancel_after: CancelAfter) {
    use crate::server::routes::interpret::effects::{early::RoomMembershipMutation, ImmediateSink};
    let state = shared_state(&fixture).await;
    let (message, actor) = invite_room(&state, true).await;
    let recipient: jid::BareJid = "juliet@example.com".parse().expect("recipient");
    let resource: jid::FullJid = "juliet@example.com/phone".parse().expect("resource");
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    crate::server::routes::websocket::tests::register_test_connection(&state, &resource, tx).await;
    let mut submission = submission(&fixture, invite_plan(&state, &message, true).await).await;
    let accepted = commit_submission(&fixture.uow, &submission, 3)
        .await
        .expect("accepted invitation");
    let mutation = accepted
        .external
        .iter()
        .find_map(|effect| match effect {
            ExternalEffect::RoomMembershipMutation(RoomMembershipMutation::GroupDm(mutation)) => {
                Some((**mutation).clone())
            }
            _ => None,
        })
        .expect("membership");
    let deps = crate::server::routes::websocket::interpret_loop::build_interpret_deps(&state, None);
    assert!(matches!(
        execute_group_dm_membership(mutation, &deps).await,
        EffectOutcome::Membership(_)
    ));
    // Cancellation here has not executed the ledger or any invitation delivery.
    assert!(crate::server::routes::websocket::muc_invites::list_invites(
        state.deps.app_state.db_pool.global_actor().clone(),
        &message.to.as_ref().expect("room").to_bare(),
        &recipient
    )
    .await
    .expect("ledger")
    .is_empty());
    if cancel_after != CancelAfter::Membership {
        let (index, ledger) = accepted
            .external
            .iter()
            .enumerate()
            .find_map(|(index, effect)| match effect {
                ExternalEffect::InviteLedger(ledger) => Some((index, ledger.clone())),
                _ => None,
            })
            .expect("ledger effect");
        assert!(matches!(
            super::super::muc_invite::execute_invite_ledger(ledger, &deps).await,
            EffectOutcome::InviteLedger(Ok(
                super::super::muc_invite::InviteLedgerOutcome::Recorded(
                    crate::server::routes::websocket::muc_invites::RecordOutcome::New { .. }
                )
            ))
        ));
        for receipt in accepted.external_receipts[index]
            .iter()
            .filter(|_| cancel_after == CancelAfter::LedgerReceipt)
        {
            crate::ingress_uow::EffectReceiptRepository::record_receipt_pooled(
                &fixture.db,
                accepted.message_key.expect("canonical"),
                receipt.kind,
                &receipt.semantic_identity_hash,
            )
            .await
            .expect("confirmed ledger receipt");
        }
    }
    // Bookmark updates from membership are distinct from the outstanding invite.
    while rx.try_recv().is_ok() {}
    submission.plan = invite_plan(&state, &message, true).await;
    assert!(
        submission.plan.rejection.is_some(),
        "fresh already-member invitation remains a conflict"
    );
    let replay = commit_submission(&fixture.uow, &submission, 3)
        .await
        .expect("replay accepted authority");
    assert_eq!(replay.message_key, accepted.message_key);
    assert_eq!(
        replay
            .external
            .iter()
            .any(|effect| matches!(effect, ExternalEffect::InviteLedger(_))),
        cancel_after != CancelAfter::LedgerReceipt,
        "only pending ledger reconstructed"
    );
    assert!(
        replay
            .external
            .iter()
            .any(|effect| matches!(effect, ExternalEffect::RouteToPeer(_))),
        "pending route reconstructed"
    );
    let report = crate::ingress::execute::execute_effects(
        &fixture.uow,
        &fixture.db,
        &replay,
        &ImmediateSink,
        &deps,
        std::time::Duration::from_secs(5),
    )
    .await;
    assert!(report.receipt_failures.is_empty(), "{report:?}");
    assert!(rx.try_recv().is_ok(), "missed invitation delivered");
    assert!(crate::ingress::execute::terminalize_if_complete(
        &fixture.uow,
        replay.message_key.expect("canonical key")
    )
    .await
    .expect("terminalize"));
    assert_eq!(
        fixture.count("ingress_effect_receipts").await,
        fixture.count("ingress_effect_intents").await
    );
    actor.kill();
    drop(state);
    fixture.close().await;
}
#[tokio::test]
async fn ingress_group_dm_invite_membership_cancel_replay_sqlite() {
    membership_cancel_replay(IngressFixture::sqlite().await, CancelAfter::Membership).await;
}
#[tokio::test]
async fn ingress_group_dm_invite_membership_cancel_replay_postgres() {
    if let Some(fixture) = IngressFixture::postgres("group_dm_invite_membership_replay").await {
        membership_cancel_replay(fixture, CancelAfter::Membership).await;
    }
}

#[tokio::test]
async fn ingress_group_dm_invite_ledger_cancel_replay_sqlite() {
    membership_cancel_replay(IngressFixture::sqlite().await, CancelAfter::LedgerReceipt).await;
}
#[tokio::test]
async fn ingress_group_dm_invite_ledger_cancel_replay_postgres() {
    if let Some(fixture) = IngressFixture::postgres("group_dm_invite_ledger_replay").await {
        membership_cancel_replay(fixture, CancelAfter::LedgerReceipt).await;
    }
}

#[derive(Default)]
struct StopInviteeContext {
    actor:
        std::sync::Mutex<Option<kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor>>>,
}
#[async_trait::async_trait]
impl waddle_xmpp::xep::xep0191::BlockingStorage for StopInviteeContext {
    async fn list_blocked_jids(
        &self,
        _: &jid::BareJid,
    ) -> Result<Vec<jid::BareJid>, waddle_xmpp::xep::xep0191::BlockingStorageError> {
        let actor = self.actor.lock().expect("actor lock").take();
        if let Some(actor) = actor {
            actor.kill();
            actor.wait_for_shutdown().await;
        }
        Ok(Vec::new())
    }
}
async fn invitee_context_failure(fixture: IngressFixture) {
    let blocking = Arc::new(StopInviteeContext::default());
    let state = crate::server::routes::websocket::tests::create_test_websocket_state_with_sm_registry_pending_and_blocking(
        Arc::new(waddle_xmpp::stream_management::InMemorySmSessionRegistry::new()),
        Arc::new(waddle_xmpp::pending_delivery::storage::InMemoryPendingDeliveryStorage::unlimited()),
        blocking.clone(),
    ).await;
    let (message, actor) = invite_room(&state, true).await;
    *blocking.actor.lock().expect("actor lock") = Some(actor);
    // The first admin context succeeds; blocklist lookup stops the actor before
    // the invitee's separate GetAdminContext ask.
    let mut submission = submission(&fixture, invite_plan(&state, &message, true).await).await;
    refused(&fixture, &submission, PlanFailure::RoomSnapshotUnavailable).await;
    let healthy = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    let (message, actor) = invite_room(&healthy, true).await;
    submission.plan = invite_plan(&healthy, &message, true).await;
    recovered(&fixture, &submission).await;
    actor.kill();
    fixture.close().await;
}
#[tokio::test]
async fn ingress_group_dm_invite_invitee_context_failure_retry_sqlite() {
    invitee_context_failure(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn ingress_group_dm_invite_invitee_context_failure_retry_postgres() {
    if let Some(fixture) = IngressFixture::postgres("group_dm_invitee_context").await {
        invitee_context_failure(fixture).await;
    }
}

#[tokio::test]
async fn ingress_group_dm_invite_ledger_without_receipt_replay_sqlite() {
    membership_cancel_replay(
        IngressFixture::sqlite().await,
        CancelAfter::LedgerWithoutReceipt,
    )
    .await;
}
#[tokio::test]
async fn ingress_group_dm_invite_ledger_without_receipt_replay_postgres() {
    if let Some(fixture) = IngressFixture::postgres("group_dm_invite_unreceipted_ledger").await {
        membership_cancel_replay(fixture, CancelAfter::LedgerWithoutReceipt).await;
    }
}

#[path = "invite_recorded_policy_tests.rs"]
mod recorded_policy_tests;

async fn fallback_receipt_replay(fixture: IngressFixture, partial: bool) {
    use crate::server::routes::interpret::effects::ImmediateSink;
    let state = shared_state(&fixture).await;
    let (message, actor) = invite_room(&state, true).await;
    let resource: jid::FullJid = "juliet@example.com/phone".parse().expect("resource");
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    crate::server::routes::websocket::tests::register_test_connection(&state, &resource, tx).await;
    let mut submission = submission(&fixture, invite_plan(&state, &message, true).await).await;
    let accepted = commit_submission(&fixture.uow, &submission, 3)
        .await
        .expect("accept");
    assert!(accepted.external.iter().any(|effect| matches!(effect,
        ExternalEffect::RouteToPeer(route) if route.resources.contains(&resource))));
    drop(rx); // The recorded live resource refuses the invitation at execution.
    let deps = crate::server::routes::websocket::interpret_loop::build_interpret_deps(&state, None);
    let report = crate::ingress::execute::execute_effects(
        &fixture.uow,
        &fixture.db,
        &accepted,
        &ImmediateSink,
        &deps,
        std::time::Duration::from_secs(5),
    )
    .await;
    assert!(report.receipt_failures.is_empty(), "{report:?}");
    assert_eq!(
        fixture.count("ingress_effect_receipts").await,
        fixture.count("ingress_effect_intents").await,
        "fallback discharges live route"
    );
    let queued = state
        .deps
        .protocol
        .pending_delivery_storage
        .list(&resource.to_bare())
        .await
        .expect("queued invitation");
    assert_eq!(queued.len(), 1);
    if partial {
        // Model a crash after the fallback receipt, before its route receipt.
        fixture
            .execute(
                "DELETE FROM ingress_effect_receipts WHERE kind = ?1",
                crate::db_params![submission
                    .plan
                    .intents
                    .iter()
                    .find(|intent| matches!(intent, IngressEffectIntent::RouteDirect { .. }))
                    .expect("route intent")
                    .with_encoded_v1(|kind, _| kind)
                    .expect("route kind")],
            )
            .await;
    }
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    crate::server::routes::websocket::tests::register_test_connection(&state, &resource, tx).await;
    submission.plan = invite_plan(&state, &message, true).await;
    if let IngressStreamIdentity::Resumable {
        reserved_wire_position,
        checkpoint_h,
        ..
    } = &mut submission.identity
    {
        *reserved_wire_position = WireHandledCount::new(2);
        *checkpoint_h = WireHandledCount::new(2);
    }
    let replay = commit_submission(&fixture.uow, &submission, 3)
        .await
        .expect("alias replay");
    assert_eq!(replay.message_key, accepted.message_key);
    assert!(!replay.external.iter().any(|effect| matches!(
        effect,
        ExternalEffect::RouteToPeer(_) | ExternalEffect::QueueOfflineDelivery(_)
    )));
    let report = crate::ingress::execute::execute_effects(
        &fixture.uow,
        &fixture.db,
        &replay,
        &ImmediateSink,
        &deps,
        std::time::Duration::from_secs(5),
    )
    .await;
    assert!(report.receipt_failures.is_empty(), "{report:?}");
    assert!(
        rx.try_recv().is_err(),
        "receipted fallback must not redeliver live"
    );
    let after = state
        .deps
        .protocol
        .pending_delivery_storage
        .list(&resource.to_bare())
        .await
        .expect("pending rows after replay");
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].id, queued[0].id);
    assert!(crate::ingress::execute::terminalize_if_complete(
        &fixture.uow,
        replay.message_key.expect("canonical"),
    )
    .await
    .expect("terminalize"));
    actor.kill();
    drop(state);
    fixture.close().await;
}

#[tokio::test]
async fn ingress_group_dm_invite_fallback_receipt_replay_sqlite() {
    fallback_receipt_replay(IngressFixture::sqlite().await, false).await;
}
#[tokio::test]
async fn ingress_group_dm_invite_fallback_receipt_replay_postgres() {
    if let Some(fixture) = IngressFixture::postgres("invite_fallback_receipt").await {
        fallback_receipt_replay(fixture, false).await;
    }
}
#[tokio::test]
async fn ingress_group_dm_invite_partial_fallback_receipt_replay_sqlite() {
    fallback_receipt_replay(IngressFixture::sqlite().await, true).await;
}
#[tokio::test]
async fn ingress_group_dm_invite_partial_fallback_receipt_replay_postgres() {
    if let Some(fixture) = IngressFixture::postgres("invite_partial_fallback_receipt").await {
        fallback_receipt_replay(fixture, true).await;
    }
}

async fn full_history_timestamp_replay(fixture: IngressFixture) {
    let state = shared_state(&fixture).await;
    let (message, actor) = invite_room(&state, true).await;
    let offered = invite_plan(&state, &message, true).await;
    let recorded_at =
        chrono::DateTime::from_timestamp(1_700_000_000, 123_000_000).expect("recorded timestamp");
    let mut recorded = offered.intents.clone();
    for intent in &mut recorded {
        match intent {
            IngressEffectIntent::GroupDmMembershipGrant { grant }
            | IngressEffectIntent::GroupDmInviteLedger { grant } => {
                grant.history_visibility = GroupDmHistoryVisibility::Full;
            }
            IngressEffectIntent::MucInviteLedger { mutation } => {
                mutation.recorded_at = Some(recorded_at);
            }
            _ => {}
        }
    }
    let mut restored = offered.clone();
    assert!(restore_recorded_group_dm_invite(
        &mut restored,
        &offered,
        &recorded,
        &recorded,
        &crate::ingress_substrate::MessageEnvelope::new(message),
    )
    .expect("restore full history invitation"));
    assert!(restored.plan.iter().any(|effect| matches!(&effect.effect,
        Effect::External(ExternalEffect::InviteLedger(
            super::super::muc_invite::InviteLedgerMutation::Record { recorded_at: actual, .. }
        )) if actual == &recorded_at)));
    assert!(restored.plan.iter().any(|effect| matches!(&effect.effect,
        Effect::External(ExternalEffect::RouteToPeer(route) | ExternalEffect::QueueOfflineDelivery(route))
            if route.fallback.original_receipt_at == recorded_at)));
    actor.kill();
    drop(state);
    fixture.close().await;
}

#[tokio::test]
async fn ingress_group_dm_invite_full_history_timestamp_replay_sqlite() {
    full_history_timestamp_replay(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn ingress_group_dm_invite_full_history_timestamp_replay_postgres() {
    if let Some(fixture) = IngressFixture::postgres("invite_full_timestamp").await {
        full_history_timestamp_replay(fixture).await;
    }
}

#[tokio::test]
async fn ingress_group_dm_invite_directory_failure_retry_sqlite() {
    directory_failure(IngressFixture::sqlite().await, true).await;
}
#[tokio::test]
async fn ingress_group_dm_invite_directory_failure_retry_postgres() {
    if let Some(fixture) = IngressFixture::postgres("group_invite_directory").await {
        directory_failure(fixture, true).await;
    }
}
async fn registry_failure(fixture: IngressFixture, group_dm: bool) {
    let state = shared_state(&fixture).await;
    let (message, actor) = invite_room(&state, group_dm).await;
    state.deps.protocol.room_registry.kill();
    state.deps.protocol.room_registry.wait_for_shutdown().await;
    let mut submission = submission(&fixture, invite_plan(&state, &message, group_dm).await).await;
    refused(&fixture, &submission, PlanFailure::RoomSnapshotUnavailable).await;
    let healthy = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    let (message, healthy_actor) = invite_room(&healthy, group_dm).await;
    submission.plan = invite_plan(&healthy, &message, group_dm).await;
    deliver_recovered(&fixture, &submission, &healthy).await;
    actor.kill();
    healthy_actor.kill();
    drop(state);
    fixture.close().await;
}
#[tokio::test]
async fn ingress_muc_invite_registry_failure_retry_sqlite() {
    registry_failure(IngressFixture::sqlite().await, false).await;
}
#[tokio::test]
async fn ingress_group_dm_invite_registry_failure_retry_sqlite() {
    registry_failure(IngressFixture::sqlite().await, true).await;
}
#[tokio::test]
async fn ingress_muc_invite_registry_failure_retry_postgres() {
    if let Some(fixture) = IngressFixture::postgres("muc_registry_failure").await {
        registry_failure(fixture, false).await;
    }
}
#[tokio::test]
async fn ingress_group_dm_invite_registry_failure_retry_postgres() {
    if let Some(fixture) = IngressFixture::postgres("group_registry_failure").await {
        registry_failure(fixture, true).await;
    }
}

async fn invite_directory_semantic_denial(fixture: IngressFixture, group_dm: bool) {
    let state = shared_state(&fixture).await;
    let (message, actor) = invite_room(&state, group_dm).await;
    let sender = "romeo@example.com/phone".parse().expect("sender");
    let mut message = super::tests::group_dm_invite_message(
        &message.to.expect("room").to_bare(),
        &sender,
        "missing@example.com",
    );
    waddle_xmpp_core::xep0359::add_origin_id(&mut message, "invite-prerequisite-retry");
    let submission = submission(&fixture, invite_plan(&state, &message, group_dm).await).await;
    assert_eq!(submission.plan.failure, None);
    assert!(submission.plan.rejection.is_some());
    let decision = commit_submission(&fixture.uow, &submission, 3)
        .await
        .expect("semantic denial commits");
    assert!(decision.class.advances());
    checkpoint(&fixture, &submission, 1).await;
    actor.kill();
    drop(state);
    fixture.close().await;
}
#[tokio::test]
async fn ingress_muc_invite_missing_account_denial_sqlite() {
    invite_directory_semantic_denial(IngressFixture::sqlite().await, false).await;
}
#[tokio::test]
async fn ingress_group_dm_invite_missing_account_denial_sqlite() {
    invite_directory_semantic_denial(IngressFixture::sqlite().await, true).await;
}
#[tokio::test]
async fn ingress_muc_invite_missing_account_denial_postgres() {
    if let Some(fixture) = IngressFixture::postgres("muc_missing_account").await {
        invite_directory_semantic_denial(fixture, false).await;
    }
}
#[tokio::test]
async fn ingress_group_dm_invite_missing_account_denial_postgres() {
    if let Some(fixture) = IngressFixture::postgres("group_missing_account").await {
        invite_directory_semantic_denial(fixture, true).await;
    }
}
