use crate::ingress_support::IngressFixture;
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
        Deps, ExternalEffect, ExternalOutcome, ImmediateSink, IngressDecision,
    },
    notification_outbox::{
        NotificationCandidate, NotificationClass, NotificationOutboxStore, NotificationThreadId,
    },
};
use waddle_xmpp::inbox::storage::InboxStorage;
use waddle_xmpp::ingress::IngressEffectIntent;
use waddle_xmpp::{
    inbox::{
        storage::{GroupchatNotificationRecovery, GroupchatNotificationRecoveryKey},
        ConversationKind, InboxEntry,
    },
    ingress::{
        GroupchatNotificationRecoveryAction, GroupchatNotificationRecoveryMutation,
        InboxProjectionMutation, NotificationActivityMutation, NotificationCandidateOutcome,
    },
    registry::ConnectionRegistry,
};
use waddle_xmpp_core::xep0359::StanzaId;

fn recovery_plan(fixture: &IngressFixture) -> IngressSubmission {
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

async fn committed(
    fixture: &IngressFixture,
) -> (
    IngressDecision,
    DatabaseInboxStorage,
    NotificationOutboxStore,
    GroupchatNotificationRecovery,
) {
    let store = NotificationOutboxStore::new(fixture.db.clone())
        .await
        .expect("outbox schema");
    let inbox = DatabaseInboxStorage::from_database(fixture.db.clone())
        .await
        .expect("inbox");
    let decision = commit_submission(&fixture.uow, &recovery_plan(fixture), 5)
        .await
        .expect("commit recovery plan");
    assert_eq!(decision.external_receipts[0].len(), 2);
    assert_eq!(decision.arm_owned_receipts.len(), 2);
    let recovery = inbox
        .list_pending_groupchat_notification_recoveries(10)
        .await
        .expect("recovery rows")
        .pop()
        .expect("committed recovery");
    assert_eq!(
        recovery.message_key,
        decision.message_key.expect("canonical")
    );
    (decision, inbox, store, recovery)
}

async fn execute(fixture: &IngressFixture, decision: &IngressDecision) -> ExternalOutcome {
    let registry = ConnectionRegistry::new();
    let deps = Deps::new(&registry, "example.com");
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert!(report.receipt_failures.is_empty());
    assert!(report.terminalization_failure.is_none());
    report.outcomes[0].1
}

async fn assert_complete(fixture: &IngressFixture) {
    assert_eq!(fixture.count("notification_candidates").await, 1);
    assert_eq!(
        fixture
            .count("groupchat_notification_recovery WHERE completed_at_ms IS NOT NULL")
            .await,
        1
    );
    assert_eq!(fixture.count("ingress_effect_receipts").await, 4);
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
}

async fn success(fixture: IngressFixture) {
    let (decision, _, _, _) = committed(&fixture).await;
    assert_eq!(execute(&fixture, &decision).await, ExternalOutcome::Done);
    assert_complete(&fixture).await;
    assert_eq!(execute(&fixture, &decision).await, ExternalOutcome::Done);
    assert_complete(&fixture).await;
    fixture.close().await;
}

async fn duplicate(fixture: IngressFixture, cross_archive: bool) {
    let (decision, _, store, _) = committed(&fixture).await;
    let ExternalEffect::Room(ExternalRoomEffect::NotificationCandidate {
        candidate: Some(candidate),
        ..
    }) = &decision.external[0]
    else {
        panic!("candidate effect")
    };
    store
        .insert_candidate(candidate)
        .await
        .expect("earlier candidate insert");
    if cross_archive {
        fixture
            .execute(
                "UPDATE notification_candidates SET stanza_id_by = ?",
                waddle_server::db_params!["older-archive@conference.example.com"],
            )
            .await;
    }
    assert_eq!(execute(&fixture, &decision).await, ExternalOutcome::Done);
    assert_complete(&fixture).await;
    fixture.close().await;
}

async fn missing_identity(fixture: IngressFixture) {
    let (decision, _, _, _) = committed(&fixture).await;
    fixture
        .execute(
            "UPDATE groupchat_notification_recovery SET stanza_id = ?",
            waddle_server::db_params!["different-archive"],
        )
        .await;
    assert_eq!(execute(&fixture, &decision).await, ExternalOutcome::Failed);
    assert_eq!(
        fixture.count("notification_candidates").await,
        0,
        "failed completion rolls candidate back"
    );
    assert_eq!(fixture.count("ingress_effect_receipts").await, 2);
    assert_eq!(
        fixture
            .count("groupchat_notification_recovery WHERE completed_at_ms IS NULL")
            .await,
        1
    );
    fixture.close().await;
}

macro_rules! dialect_tests {
    ($sqlite:ident, $postgres:ident, $test:ident $(, $argument:expr)?) => {
        #[tokio::test]
        async fn $sqlite() { $test(IngressFixture::sqlite().await $(, $argument)?).await; }
        #[tokio::test]
        async fn $postgres() {
            if let Some(fixture) = IngressFixture::postgres(stringify!($test)).await {
                $test(fixture $(, $argument)?).await;
            }
        }
    };
}

dialect_tests!(recovery_success_sqlite, recovery_success_postgres, success);
dialect_tests!(
    recovery_exact_duplicate_sqlite,
    recovery_exact_duplicate_postgres,
    duplicate,
    false
);
dialect_tests!(
    recovery_cross_archive_duplicate_sqlite,
    recovery_cross_archive_duplicate_postgres,
    duplicate,
    true
);
dialect_tests!(
    recovery_missing_identity_sqlite,
    recovery_missing_identity_postgres,
    missing_identity
);

fn rebuild(
    envelope: &waddle_server::ingress_substrate::MessageEnvelope,
    recovery: &GroupchatNotificationRecovery,
) -> NotificationCandidate {
    waddle_server::notification_outbox::candidate_from_envelope(
        envelope.message(),
        waddle_server::notification_outbox::GroupchatCandidateIdentity {
            owner: &recovery.key.recipient,
            room: &recovery.key.room,
            sender: &recovery.sender_jid,
            thread_id: NotificationThreadId::root(),
            archive_stanza_id: &recovery.key.archive_stanza_id,
            is_live_occupant: recovery.is_live_occupant,
            sender_can_broadcast_channel_mention: recovery.sender_can_broadcast_channel_mention,
        },
        &waddle_xmpp::xep::xep0421::OccupantIdSecret::new(vec![7_u8; 32]).expect("occupant secret"),
    )
    .expect("canonical candidate")
}

async fn lost_phase_c(fixture: IngressFixture) {
    use waddle_server::ingress::{
        RecoveryPolicyDecision, RecoveryPreparation, RecoverySweepOutcome,
    };
    let (_, _, _, recovery) = committed(&fixture).await;
    use waddle_xmpp::mam::{ArchivedMessage, MamStorage, SqlxMamStorage};
    let mam = SqlxMamStorage::open(fixture.db.database_url())
        .await
        .expect("MAM storage");
    let mut archived = ArchivedMessage::for_test(
        recovery.sender_jid.clone(),
        recovery.key.room.clone().into(),
    );
    archived.id.clone_from(&recovery.key.archive_stanza_id.id);
    archived.stanza_id = Some(recovery.key.archive_stanza_id.clone());
    archived.message_type = xmpp_parsers::message::MessageType::Groupchat;
    archived.body = Some("frozen canonical body".to_owned());
    mam.store_message(&recovery.key.room, &archived)
        .await
        .expect("original MAM row");
    assert!(mam
        .get_message(&archived.id)
        .await
        .expect("stored MAM read")
        .is_some());
    assert_eq!(
        mam.delete_before(
            &recovery.key.room,
            chrono::Utc::now() + chrono::Duration::seconds(1)
        )
        .await
        .expect("MAM retention deletion"),
        1
    );
    assert!(mam
        .get_message(&archived.id)
        .await
        .expect("deleted MAM read")
        .is_none());
    let authority = fixture.authority().await;
    let RecoveryPreparation::Ready {
        envelope,
        deferred_policy,
        candidate_required,
        completed,
    } = authority
        .prepare_notification_recovery(&recovery)
        .await
        .expect("prepare")
    else {
        panic!("canonical exists")
    };
    assert!(!deferred_policy, "decided work cannot reevaluate policy");
    assert!(candidate_required);
    assert!(!completed);
    let candidate = rebuild(&envelope, &recovery);
    assert_eq!(candidate.last_message_body(), Some("frozen canonical body"));
    assert_eq!(
        authority
            .settle_notification_recovery(&recovery, RecoveryPolicyDecision::Suppressed)
            .await
            .expect("policy drift"),
        RecoverySweepOutcome::Pending
    );
    assert_eq!(
        authority
            .settle_notification_recovery(
                &recovery,
                RecoveryPolicyDecision::Deliver(Box::new(candidate))
            )
            .await
            .expect("recover without MAM"),
        RecoverySweepOutcome::Completed
    );
    assert_complete(&fixture).await;
    assert!(authority.drain_and_join(Duration::from_secs(5)).await);
    fixture.close().await;
}

async fn completed_orphan(fixture: IngressFixture) {
    use waddle_server::ingress::{RecoveryPolicyDecision, RecoverySweepOutcome};
    let (decision, inbox, _, recovery) = committed(&fixture).await;
    // Reproduce historical effect completion whose receipt commit was lost.
    inbox
        .mark_groupchat_notification_recovery_completed(&recovery.key)
        .await
        .expect("old completion");
    assert_eq!(
        inbox
            .prune_completed_groupchat_notification_recoveries(i64::MAX, 10)
            .await
            .expect("prune"),
        0
    );
    assert_eq!(
        inbox
            .list_completed_unreceipted_groupchat_notification_recoveries(10)
            .await
            .expect("orphans")
            .len(),
        1
    );
    let authority = fixture.authority().await;
    assert_eq!(
        authority
            .settle_notification_recovery(&recovery, RecoveryPolicyDecision::AlreadyCompleted)
            .await
            .expect("orphan receipt"),
        RecoverySweepOutcome::Completed
    );
    assert_eq!(
        fixture.count("notification_candidates").await,
        0,
        "AlreadyCompleted only proves recovery"
    );
    assert_eq!(fixture.count("ingress_effect_receipts").await, 3);
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        0
    );
    assert!(inbox
        .list_completed_unreceipted_groupchat_notification_recoveries(10)
        .await
        .expect("no orphan")
        .is_empty());
    assert_eq!(
        inbox
            .prune_completed_groupchat_notification_recoveries(i64::MAX, 10)
            .await
            .expect("unresolved candidate retained"),
        0
    );
    assert_eq!(execute(&fixture, &decision).await, ExternalOutcome::Done);
    assert_complete(&fixture).await;
    assert_eq!(
        inbox
            .prune_completed_groupchat_notification_recoveries(i64::MAX, 10)
            .await
            .expect("terminal prune"),
        1
    );
    assert!(authority.drain_and_join(Duration::from_secs(5)).await);
    fixture.close().await;
}

async fn deferred(fixture: IngressFixture, deliver: bool) {
    use waddle_server::ingress::{
        RecoveryPolicyDecision, RecoveryPreparation, RecoverySweepOutcome,
    };
    let store = NotificationOutboxStore::new(fixture.db.clone())
        .await
        .expect("outbox");
    let inbox = DatabaseInboxStorage::from_database(fixture.db.clone())
        .await
        .expect("inbox");
    let mut submission = recovery_plan(&fixture);
    submission
        .plan
        .plan
        .retain(|effect| matches!(effect.effect, Effect::Durable(_)));
    submission.plan.intents.retain(|intent| {
        !matches!(
            intent,
            IngressEffectIntent::NotificationActivityPreview { .. }
        )
    });
    for intent in &mut submission.plan.intents {
        if let IngressEffectIntent::GroupchatNotificationRecovery { mutation } = intent {
            if mutation.action == GroupchatNotificationRecoveryAction::Completed {
                mutation.action = GroupchatNotificationRecoveryAction::DeferredPolicy;
            }
        }
    }
    commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("deferred commit");
    let recovery = inbox
        .list_pending_groupchat_notification_recoveries(10)
        .await
        .expect("pending")
        .pop()
        .expect("recovery");
    let authority = fixture.authority().await;
    for _ in 0..2 {
        assert_eq!(
            authority
                .settle_notification_recovery(&recovery, RecoveryPolicyDecision::RetryLater)
                .await
                .expect("repeat error"),
            RecoverySweepOutcome::Pending
        );
        assert_eq!(fixture.count("ingress_effect_receipts").await, 2);
        assert_eq!(
            inbox
                .list_pending_groupchat_notification_recoveries(10)
                .await
                .expect("still pending")
                .len(),
            1
        );
    }
    let RecoveryPreparation::Ready {
        envelope,
        deferred_policy,
        candidate_required,
        ..
    } = authority
        .prepare_notification_recovery(&recovery)
        .await
        .expect("prepare deferred")
    else {
        panic!("canonical")
    };
    assert!(deferred_policy);
    assert!(!candidate_required);
    let policy = if deliver {
        RecoveryPolicyDecision::Deliver(Box::new(rebuild(&envelope, &recovery)))
    } else {
        RecoveryPolicyDecision::Suppressed
    };
    assert_eq!(
        authority
            .settle_notification_recovery(&recovery, policy)
            .await
            .expect("deferred decision"),
        RecoverySweepOutcome::Completed
    );
    assert_eq!(
        store.count_all_candidates().await.expect("candidates"),
        i64::from(deliver)
    );
    assert_eq!(
        fixture.count("ingress_effect_receipts").await,
        3,
        "no invented candidate obligation"
    );
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
    commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("replay settled deferred plan");
    assert!(inbox
        .list_pending_groupchat_notification_recoveries(10)
        .await
        .expect("no reset after replay")
        .is_empty());
    assert!(authority.drain_and_join(Duration::from_secs(5)).await);
    fixture.close().await;
}

async fn concurrent_execute_sweep(fixture: IngressFixture) {
    use waddle_server::ingress::{RecoveryPolicyDecision, RecoveryPreparation};
    let (decision, _, _, recovery) = committed(&fixture).await;
    let authority = fixture.authority().await;
    let RecoveryPreparation::Ready { envelope, .. } = authority
        .prepare_notification_recovery(&recovery)
        .await
        .expect("prepare")
    else {
        panic!("canonical")
    };
    let candidate = rebuild(&envelope, &recovery);
    let (execution, sweep) = tokio::join!(
        execute(&fixture, &decision),
        authority.settle_notification_recovery(
            &recovery,
            RecoveryPolicyDecision::Deliver(Box::new(candidate))
        )
    );
    assert_eq!(execution, ExternalOutcome::Done);
    sweep.expect("concurrent settlement");
    assert_complete(&fixture).await;
    assert!(authority.drain_and_join(Duration::from_secs(5)).await);
    fixture.close().await;
}

dialect_tests!(
    recovery_phase_c_lost_sqlite,
    recovery_phase_c_lost_postgres,
    lost_phase_c
);
dialect_tests!(
    recovery_completed_orphan_sqlite,
    recovery_completed_orphan_postgres,
    completed_orphan
);
dialect_tests!(
    recovery_deferred_deliver_sqlite,
    recovery_deferred_deliver_postgres,
    deferred,
    true
);
dialect_tests!(
    recovery_deferred_suppressed_sqlite,
    recovery_deferred_suppressed_postgres,
    deferred,
    false
);
dialect_tests!(
    recovery_concurrent_sqlite,
    recovery_concurrent_postgres,
    concurrent_execute_sweep
);

async fn canonical_gone(fixture: IngressFixture) {
    use waddle_server::ingress::{RecoveryPolicyDecision, RecoverySweepOutcome};
    let (_, _, _, mut recovery) = committed(&fixture).await;
    let absent = waddle_xmpp::ingress::MessageKey::new();
    fixture
        .execute(
            "UPDATE groupchat_notification_recovery SET message_key = ?",
            waddle_server::db_params![absent.to_storage().to_string()],
        )
        .await;
    recovery.message_key = absent;
    let authority = fixture.authority().await;
    assert_eq!(
        authority
            .settle_notification_recovery(&recovery, RecoveryPolicyDecision::AlreadyCompleted)
            .await
            .expect("discard canonical orphan"),
        RecoverySweepOutcome::CanonicalGone
    );
    assert_eq!(fixture.count("groupchat_notification_recovery").await, 0);
    assert_eq!(fixture.count("notification_candidates").await, 0);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 2);
    assert!(authority.drain_and_join(Duration::from_secs(5)).await);
    fixture.close().await;
}

dialect_tests!(
    recovery_canonical_gone_sqlite,
    recovery_canonical_gone_postgres,
    canonical_gone
);
