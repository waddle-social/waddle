//! XEP-0424 obligations cross the real plan, commit and execution boundaries.
use super::*;
use crate::ingress::{
    commit::commit_submission,
    execute::{execute_effects, ExternalOutcome},
    test_support::IngressFixture,
    IngressDecisionClass, IngressEffectCapture,
};
use crate::server::routes::interpret::{
    effects::{ImmediateSink, PlanFailure, PlanSink},
    message_plan::finish_plan,
};
use std::time::Duration;
use waddle_xmpp::{
    mam::SqlxMamStorage,
    pending_delivery::{
        storage::PendingDeliveryStorage, PendingPayload, PendingRow, PendingRowId, QuotaPolicy,
    },
};

async fn seed(
    fixture: &IngressFixture,
) -> (
    Arc<dyn MamStorage>,
    BareJid,
    waddle_xmpp::mam::ArchivedMessage,
) {
    let mam: Arc<dyn MamStorage> = Arc::new(
        SqlxMamStorage::open(fixture.db.database_url())
            .await
            .expect("MAM"),
    );
    let room: BareJid = "retract@muc.example.com".parse().expect("room");
    let mut original = waddle_xmpp::mam::ArchivedMessage::for_test(
        fixture.principal.bare_jid().clone().into(),
        room.clone().into(),
    );
    original.id = "retraction-original".into();
    original.stanza_id = Some(Xep0359StanzaId::new("wire-original", room.clone().into()));
    original.body = Some("original remains available".into());
    original.message_type = XmppMessageType::Groupchat;
    mam.store_message(&room, &original)
        .await
        .expect("seed target");
    (mam, room, original)
}

async fn read_failure(fixture: IngressFixture, direct: bool) {
    let (mam, room, original) = seed(&fixture).await;
    // The validation read succeeded. The subsequent Phase-A read fails.
    assert!(mam
        .get_message(&original.id)
        .await
        .expect("validation")
        .is_some());
    let registry = ConnectionRegistry::new();
    let sink = PlanSink::new();
    let capture = IngressEffectCapture::new();
    let deps = Deps {
        mam_storage: Some(&mam),
        effects: &sink,
        ..Deps::registry_only(&registry)
    }
    .with_ingress_effect_capture(Some(capture.clone()));
    let mut submission = fixture.submission(None, "retraction");
    submission.plan.sanitized_message.id = Some(xmpp_parsers::message::Id("retraction".into()));
    waddle_xmpp_core::xep0359::add_stanza_id(
        &mut submission.plan.sanitized_message,
        &Xep0359StanzaId::new("retraction", room.clone().into()),
    );
    fixture
        .execute(
            "ALTER TABLE mam_messages RENAME TO unavailable_mam_messages",
            (),
        )
        .await;
    if direct {
        assert!(super::direct_retraction::apply_retraction_tombstone(
            &deps,
            &room,
            "wire-original",
            &submission.plan.sanitized_message
        )
        .await
        .is_none());
    } else {
        super::interpret(
            vec![OutboundEvent::ApplyGroupchatRetractionTombstone {
                room: room.clone(),
                target_message_id: original.id.clone(),
                retraction_message: Box::new(submission.plan.sanitized_message.clone()),
            }],
            &deps,
        )
        .await;
    }
    submission.plan = finish_plan(
        &sink,
        &capture,
        submission.plan.sanitized_message.clone(),
        Some(submission.sender.clone()),
    );
    assert_eq!(
        submission.plan.failure,
        Some(PlanFailure::RetractionTargetRead)
    );
    assert!(
        submission.plan.plan.is_empty(),
        "failed retraction cannot plan a broadcast"
    );
    fixture
        .execute(
            "ALTER TABLE unavailable_mam_messages RENAME TO mam_messages",
            (),
        )
        .await;
    let failure = commit_submission(&fixture.uow, &submission, 3)
        .await
        .expect_err("sticky read failure");
    assert_eq!(failure.class(), IngressDecisionClass::Storage);
    assert!(!failure.class().advances());
    assert_eq!(fixture.count("ingress_messages").await, 0);
    assert_eq!(fixture.count("ingress_effect_intents").await, 0);
    assert_eq!(
        mam.get_message(&original.id)
            .await
            .expect("read")
            .expect("original")
            .body,
        original.body
    );
    drop(mam);
    fixture.close().await;
}

async fn replay_scrub(fixture: IngressFixture, fail: bool) {
    let (mam, room, original) = seed(&fixture).await;
    let pending: Arc<dyn PendingDeliveryStorage> = Arc::new(
        crate::pending_delivery::DatabasePendingDeliveryStorage::open(
            Some(fixture.db.database_url()),
            QuotaPolicy::Unlimited,
        )
        .await
        .expect("pending"),
    );
    let queued = PendingRow {
        id: PendingRowId::fresh(),
        recipient: fixture.principal.bare_jid().clone(),
        original_receipt_at: chrono::Utc::now(),
        payload: PendingPayload::Archived(Xep0359StanzaId::new(&original.id, room.clone().into())),
        flushed_in_session: None,
        outbound_sequence: None,
    };
    pending.insert(queued.clone()).await.expect("queued target");
    let registry = ConnectionRegistry::new();
    let deps = Deps {
        mam_storage: Some(&mam),
        pending_delivery_storage: Some(&pending),
        ..Deps::registry_only(&registry)
    };
    let sink = PlanSink::new();
    let capture = IngressEffectCapture::new();
    let planned = build_plan_deps(&deps, &sink).with_ingress_effect_capture(Some(capture.clone()));
    let mut submission = fixture.submission(None, "retraction");
    submission.plan.sanitized_message.id = Some(xmpp_parsers::message::Id("retraction".into()));
    waddle_xmpp_core::xep0359::add_stanza_id(
        &mut submission.plan.sanitized_message,
        &Xep0359StanzaId::new("retraction", room.clone().into()),
    );
    super::interpret(
        vec![OutboundEvent::ApplyGroupchatRetractionTombstone {
            room: room.clone(),
            target_message_id: original.id.clone(),
            retraction_message: Box::new(submission.plan.sanitized_message.clone()),
        }],
        &planned,
    )
    .await;
    submission.plan = finish_plan(
        &sink,
        &capture,
        submission.plan.sanitized_message.clone(),
        Some(submission.sender.clone()),
    );
    assert_eq!(submission.plan.failure, None);
    assert!(submission.plan.intents.iter().any(|intent| matches!(intent,
        IngressEffectIntent::TombstoneReplayDeletion { pending_rows, .. } if pending_rows == &vec![queued.id.clone()]
    )), "payload-complete intent exists before commit");
    assert_eq!(
        pending
            .count(&queued.recipient)
            .await
            .expect("pending before commit"),
        1
    );
    let decision = commit_submission(&fixture.uow, &submission, 3)
        .await
        .expect("commit tombstone");
    assert!(mam
        .get_message(&original.id)
        .await
        .expect("lookup")
        .expect("tombstone")
        .rich
        .expect("rich")
        .is_tombstoned());
    if fail {
        fixture
            .execute(
                "ALTER TABLE pending_delivery RENAME TO unavailable_pending_delivery",
                (),
            )
            .await;
    }
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    if fail {
        assert!(report
            .outcomes
            .iter()
            .any(|(_, outcome)| *outcome == ExternalOutcome::Failed));
        assert_eq!(
            fixture
                .count("ingress_effect_receipts WHERE kind = 23")
                .await,
            0
        );
        assert_eq!(
            fixture
                .count("ingress_messages WHERE terminal_at IS NOT NULL")
                .await,
            0
        );
        fixture
            .execute(
                "ALTER TABLE unavailable_pending_delivery RENAME TO pending_delivery",
                (),
            )
            .await;
        assert_eq!(
            pending
                .count(&queued.recipient)
                .await
                .expect("preserved replay"),
            1
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
        assert!(retry
            .outcomes
            .iter()
            .all(|(_, outcome)| *outcome == ExternalOutcome::Done));
    } else {
        assert!(report
            .outcomes
            .iter()
            .all(|(_, outcome)| *outcome == ExternalOutcome::Done));
    }
    assert_eq!(pending.count(&queued.recipient).await.expect("scrubbed"), 0);
    assert_eq!(
        fixture.count("ingress_effect_intents").await,
        fixture.count("ingress_effect_receipts").await
    );
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
    drop(pending);
    drop(mam);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_groupchat_retraction_target_read_failure_is_nonadvancing_storage() {
    read_failure(IngressFixture::sqlite().await, false).await;
}
#[tokio::test]
async fn sqlite_direct_retraction_target_read_failure_is_nonadvancing_storage() {
    read_failure(IngressFixture::sqlite().await, true).await;
}
#[tokio::test]
async fn postgres_groupchat_retraction_target_read_failure_is_nonadvancing_storage() {
    if let Some(f) = IngressFixture::postgres("retract_read").await {
        read_failure(f, false).await;
    }
}
#[tokio::test]
async fn postgres_direct_retraction_target_read_failure_is_nonadvancing_storage() {
    if let Some(f) = IngressFixture::postgres("direct_retract_read").await {
        read_failure(f, true).await;
    }
}
#[tokio::test]
async fn sqlite_tombstone_replay_scrub_failure_keeps_intent_unreceipted() {
    replay_scrub(IngressFixture::sqlite().await, true).await;
}
#[tokio::test]
async fn sqlite_tombstone_replay_scrub_success_receipts_intent() {
    replay_scrub(IngressFixture::sqlite().await, false).await;
}
#[tokio::test]
async fn postgres_tombstone_replay_scrub_failure_keeps_intent_unreceipted() {
    if let Some(f) = IngressFixture::postgres("scrub_failure").await {
        replay_scrub(f, true).await;
    }
}
#[tokio::test]
async fn postgres_tombstone_replay_scrub_success_receipts_intent() {
    if let Some(f) = IngressFixture::postgres("scrub_success").await {
        replay_scrub(f, false).await;
    }
}
