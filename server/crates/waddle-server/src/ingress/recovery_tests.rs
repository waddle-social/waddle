use crate as waddle_server;
use crate::ingress::test_support::IngressFixture;
use std::time::Duration;
use waddle_server::ingress::{
    commit::commit_submission, effects::Effect, DurableEffect, IngressSubmission, PlannedEffect,
};
use waddle_server::{
    inbox::DatabaseInboxStorage,
    ingress::{
        effects::room::{
            DurableRoomEffect, ExternalRoomEffect, PlannedGroupchatNotificationRecovery,
        },
        execute::execute_effects,
        Deps, ExternalEffect, ExternalOutcome, ImmediateSink,
    },
    notification_outbox::{
        NotificationCandidate, NotificationClass, NotificationOutboxStore, NotificationThreadId,
    },
};
use waddle_xmpp::inbox::storage::InboxStorage;
use waddle_xmpp::ingress::IngressEffectIntent;
use waddle_xmpp::{
    inbox::{storage::GroupchatNotificationRecoveryKey, ConversationKind, InboxEntry},
    ingress::{
        GroupchatNotificationRecoveryAction, GroupchatNotificationRecoveryMutation,
        InboxProjectionMutation, NotificationActivityMutation, NotificationCandidateOutcome,
    },
    registry::ConnectionRegistry,
};
use waddle_xmpp_core::xep0359::StanzaId;

pub(crate) fn recovery_plan(fixture: &IngressFixture) -> IngressSubmission {
    let mut submission = fixture.submission(Some("recovery-receipts"), "frozen canonical body");
    let owner = "juliet@example.com".parse().expect("recipient");
    let room: jid::BareJid = "room@conference.example.com".parse().expect("room");
    submission.target = waddle_xmpp::ingress::NormalizedTarget::Bare(room.clone());
    submission.plan.sanitized_message.to = Some(room.clone().into());
    submission.plan.sanitized_message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    submission.digest_input = waddle_xmpp::ingress::DigestInput::from_parsed(
        &submission.plan.sanitized_message,
        &waddle_xmpp::ingress::DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![fixture.principal.bare_jid().clone(), room.clone()],
            stanza_lang: None,
        },
    )
    .expect("groupchat digest");
    let stamp = StanzaId::new("recovery-archive", room.clone().into());
    let recovery = PlannedGroupchatNotificationRecovery {
        key: GroupchatNotificationRecoveryKey {
            recipient: owner,
            room: room.clone(),
            thread_id: None,
            archive_stanza_id: stamp.clone(),
        },
        sender_jid: "room@conference.example.com/romeo"
            .parse()
            .expect("room sender"),
        is_live_occupant: false,
        room_members_only: false,
        sender_can_broadcast_channel_mention: false,
        created_at_ms: 42,
    };
    let owner = recovery.key.recipient.clone();
    let candidate = NotificationCandidate::groupchat(
        owner.clone(),
        room.clone(),
        room.with_resource_str("romeo").expect("occupant").into(),
        NotificationThreadId::root(),
        stamp.clone(),
        NotificationClass::NotifyAll,
    )
    .expect("candidate")
    .with_last_message_body(Some("frozen canonical body".to_owned()));
    submission
        .plan
        .intents
        .push(IngressEffectIntent::InboxProject {
            owner: owner.clone(),
            mutation: InboxProjectionMutation::GroupchatChannel {
                room: room.clone(),
                increment_unread: true,
            },
        });
    let mutation = GroupchatNotificationRecoveryMutation {
        recipient: owner.clone(),
        room: room.clone(),
        thread_id: None,
        archive_stanza_id: stamp.clone(),
        sender: recovery.sender_jid.clone(),
        is_live_occupant: false,
        room_members_only: false,
        sender_can_broadcast_channel_mention: false,
        created_at_ms: 42,
        action: GroupchatNotificationRecoveryAction::Recorded,
    };
    submission
        .plan
        .intents
        .push(IngressEffectIntent::GroupchatNotificationRecovery {
            mutation: mutation.clone(),
        });
    submission
        .plan
        .intents
        .push(IngressEffectIntent::GroupchatNotificationRecovery {
            mutation: GroupchatNotificationRecoveryMutation {
                action: GroupchatNotificationRecoveryAction::Completed,
                ..mutation
            },
        });
    submission
        .plan
        .intents
        .push(IngressEffectIntent::NotificationActivityPreview {
            owner: owner.clone(),
            mutation: NotificationActivityMutation::NotificationCandidate {
                conversation: room.clone(),
                archive_stanza_id: stamp.clone(),
                outcome: NotificationCandidateOutcome::Inserted,
            },
        });
    submission
        .plan
        .plan
        .push(PlannedEffect::new(Effect::Durable(DurableEffect::Room(
            DurableRoomEffect::ProjectGroupchatInbox {
                archive_stanza_id: stamp.clone(),
                owner: owner.clone(),
                entry: Box::new(InboxEntry::new(
                    room.clone(),
                    ConversationKind::MucRoom,
                    &stamp.id,
                    42,
                )),
                is_recipient: true,
                recovery: Some(recovery.clone()),
            },
        ))));
    submission
        .plan
        .plan
        .push(PlannedEffect::new(Effect::External(ExternalEffect::Room(
            ExternalRoomEffect::NotificationCandidate {
                owner,
                room,
                archive_stanza_id: stamp,
                candidate: Some(Box::new(candidate)),
                recovery: Some(recovery),
            },
        ))));
    submission
}

async fn rollback(fixture: IngressFixture) {
    NotificationOutboxStore::new(fixture.db.clone())
        .await
        .expect("outbox schema");
    let inbox = DatabaseInboxStorage::from_database(fixture.db.clone())
        .await
        .expect("inbox");
    let decision = commit_submission(&fixture.uow, &recovery_plan(&fixture), 5)
        .await
        .expect("commit recovery plan");
    super::execute_uow::fail_after_recovery_update(decision.message_key.expect("canonical"));
    let registry = ConnectionRegistry::new();
    let deps = Deps::new(&registry, "example.com");
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(report.outcomes[0].1, ExternalOutcome::Failed);
    assert_eq!(fixture.count("notification_candidates").await, 0);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 2);
    assert_eq!(
        inbox
            .list_pending_groupchat_notification_recoveries(10)
            .await
            .expect("pending recovery")
            .len(),
        1
    );
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        0
    );
    let retry = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(retry.outcomes[0].1, ExternalOutcome::Done);
    assert_eq!(fixture.count("notification_candidates").await, 1);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 4);
    assert!(inbox
        .list_pending_groupchat_notification_recoveries(10)
        .await
        .expect("recovered")
        .is_empty());
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
    fixture.close().await;
}

#[tokio::test]
async fn recovery_atomic_rollback_sqlite() {
    rollback(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn recovery_atomic_rollback_postgres() {
    if let Some(fixture) = IngressFixture::postgres("recovery_rollback").await {
        rollback(fixture).await;
    }
}
