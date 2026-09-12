use super::*;
use crate::{
    ingress::{commit::commit_submission, execute::execute_effects, test_support::IngressFixture},
    ingress_uow::CanonicalMessageRepository,
    server::routes::interpret::{
        effects::{
            room::{ExternalRoomEffect, RoomFenceRequirement},
            Effect, ExternalEffect, ImmediateSink, PlannedEffect,
        },
        Deps,
    },
};
use waddle_xmpp::{mam::ArchivedMessage, registry::ConnectionRegistry};
use waddle_xmpp_core::xep0359::StanzaId;

fn candidate(
    owner: &str,
    conversation: &str,
    archive: &str,
    outcome: NotificationCandidateOutcome,
) -> IngressEffectIntent {
    let conversation: jid::BareJid = conversation.parse().expect("conversation");
    IngressEffectIntent::NotificationActivityPreview {
        owner: owner.parse().expect("owner"),
        mutation: NotificationActivityMutation::NotificationCandidate {
            conversation: conversation.clone(),
            archive_stanza_id: StanzaId::new(archive, conversation.into()),
            outcome,
        },
    }
}

#[test]
fn typed_discharge_requires_exact_identity_and_one_way_coalescence() {
    use NotificationCandidateOutcome::{Duplicate, Inserted};
    let recorded = candidate(
        "juliet@example.com",
        "room@muc.example.com",
        "archive",
        Inserted,
    );
    let duplicate = candidate(
        "juliet@example.com",
        "room@muc.example.com",
        "archive",
        Duplicate,
    );
    assert!(discharges(&recorded, &recorded));
    assert!(discharges(&recorded, &duplicate));
    assert!(!discharges(&duplicate, &recorded));
    for evidence in [
        candidate(
            "other@example.com",
            "room@muc.example.com",
            "archive",
            Duplicate,
        ),
        candidate(
            "juliet@example.com",
            "other@muc.example.com",
            "archive",
            Duplicate,
        ),
        candidate(
            "juliet@example.com",
            "room@muc.example.com",
            "other",
            Duplicate,
        ),
    ] {
        assert!(!discharges(&recorded, &evidence));
    }
}

async fn settlement_contract(fixture: IngressFixture) {
    use NotificationCandidateOutcome::{Duplicate, Inserted};
    let recorded = candidate(
        "juliet@example.com",
        "room@muc.example.com",
        "archive",
        Inserted,
    );
    let mut submission = fixture.submission(None, "candidate settlement");
    submission.plan.intents = vec![recorded.clone()];
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit obligation");
    let key = decision.message_key.expect("canonical key");

    let mut tx = fixture.uow.begin().await.expect("mismatch transaction");
    assert!(CanonicalMessageRepository::lock(&mut tx, key)
        .await
        .expect("canonical lock"));
    let mismatch = candidate(
        "juliet@example.com",
        "room@muc.example.com",
        "wrong-archive",
        Duplicate,
    );
    assert!(settle_recorded(&mut tx, key, &[mismatch])
        .await
        .expect("unmatched evidence must not violate FK")
        .is_empty());
    tx.commit().await.expect("commit mismatch");
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);

    let mut tx = fixture.uow.begin().await.expect("rollback transaction");
    assert!(CanonicalMessageRepository::lock(&mut tx, key)
        .await
        .expect("canonical lock"));
    assert_eq!(
        settle_recorded(&mut tx, key, std::slice::from_ref(&recorded))
            .await
            .expect("exact settlement"),
        vec![recorded.clone()]
    );
    assert!(EffectReceiptRepository::receipts_complete(&mut tx, key)
        .await
        .expect("receipt visible in transaction"));
    drop(tx);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);

    let mut tx = fixture.uow.begin().await.expect("coalescence transaction");
    assert!(CanonicalMessageRepository::lock(&mut tx, key)
        .await
        .expect("canonical lock"));
    let duplicate = candidate(
        "juliet@example.com",
        "room@muc.example.com",
        "archive",
        Duplicate,
    );
    assert_eq!(
        settle_recorded(&mut tx, key, &[duplicate])
            .await
            .expect("duplicate proves insertion"),
        vec![recorded.clone()]
    );
    tx.commit().await.expect("commit settlement");
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);

    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("already-settled transaction");
    assert!(CanonicalMessageRepository::lock(&mut tx, key)
        .await
        .expect("canonical lock"));
    assert_eq!(
        settle_recorded(&mut tx, key, std::slice::from_ref(&recorded))
            .await
            .expect("already settled"),
        vec![recorded]
    );
    tx.commit().await.expect("commit repeated settlement");
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_settlement_contract() {
    settlement_contract(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_settlement_contract() {
    if let Some(fixture) = IngressFixture::postgres("settlement_contract").await {
        settlement_contract(fixture).await;
    }
}

async fn settled_archive_avoids_pooled_receipt_transactions(fixture: IngressFixture) {
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
    let mut archive = ArchivedMessage::for_test(
        room.with_resource_str("romeo").expect("nick").into(),
        room.clone().into(),
    );
    archive.id = "settled-system-message".into();
    archive.stanza_id = Some(StanzaId::new(&archive.id, room.clone().into()));
    let mut submission = fixture.submission(None, "system archive settlement");
    submission.plan.intents = vec![IngressEffectIntent::SystemMessageArchive {
        ordinal: None,
        sequence: 0,
        archive: room.clone(),
        stanza_id: archive.stanza_id.clone().expect("stanza id"),
        by: room.clone(),
        archived_at: archive.timestamp,
    }];
    submission.plan.plan = vec![PlannedEffect::new(Effect::External(ExternalEffect::Room(
        ExternalRoomEffect::ArchiveAfterPin {
            room,
            message: Box::new(archive),
            fence: RoomFenceRequirement::Unfenced,
        },
    )))];
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit archive obligation");
    assert_eq!(decision.arm_owned_receipts, decision.external_receipts[0]);
    assert_eq!(decision.arm_owned_receipts.len(), 1);
    let registry = ConnectionRegistry::new();
    let deps = Deps::registry_only(&registry);
    POOLED_RECEIPT_WRITES
        .scope(std::cell::Cell::new(0), async {
            let report = execute_effects(
                &fixture.uow,
                &fixture.db,
                &decision,
                &ImmediateSink,
                &deps,
                std::time::Duration::from_secs(5),
            )
            .await;
            assert_eq!(
                report.outcomes[0].1,
                crate::ingress::execute::ExternalOutcome::Done
            );
            assert!(report.receipt_failures.is_empty());
            assert!(report.terminalization_failure.is_none());
            assert_eq!(
                POOLED_RECEIPT_WRITES.with(std::cell::Cell::get),
                0,
                "Settled must not open an additional receipt transaction"
            );
            let receipt = &decision.arm_owned_receipts[0];
            EffectReceiptRepository::record_receipt_pooled(
                &fixture.db,
                decision.message_key.expect("message key"),
                receipt.kind,
                &receipt.semantic_identity_hash,
            )
            .await
            .expect("counter control write");
            assert_eq!(
                POOLED_RECEIPT_WRITES.with(std::cell::Cell::get),
                1,
                "the hook counts a real pooled invocation even on conflict"
            );
        })
        .await;
    assert_eq!(fixture.count("mam_messages").await, 1);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_settled_archive_avoids_pooled_receipt_transactions() {
    settled_archive_avoids_pooled_receipt_transactions(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_settled_archive_avoids_pooled_receipt_transactions() {
    if let Some(fixture) = IngressFixture::postgres("settled_archive").await {
        settled_archive_avoids_pooled_receipt_transactions(fixture).await;
    }
}

#[test]
fn deferred_policy_requires_completed_evidence_with_identical_frozen_fields() {
    use waddle_xmpp::ingress::{
        GroupchatNotificationRecoveryAction as Action, GroupchatNotificationRecoveryMutation,
    };
    let mutation = GroupchatNotificationRecoveryMutation {
        recipient: "juliet@example.com".parse().expect("recipient"),
        room: "room@muc.example.com".parse().expect("room"),
        thread_id: None,
        archive_stanza_id: StanzaId::new(
            "archive",
            "room@muc.example.com".parse().expect("archive"),
        ),
        sender: "romeo@example.com/phone".parse().expect("sender"),
        is_live_occupant: false,
        room_members_only: true,
        sender_can_broadcast_channel_mention: false,
        created_at_ms: 123,
        action: Action::DeferredPolicy,
    };
    let recorded = IngressEffectIntent::GroupchatNotificationRecovery {
        mutation: mutation.clone(),
    };
    let mut completed = mutation;
    completed.action = Action::Completed;
    let evidence = IngressEffectIntent::GroupchatNotificationRecovery {
        mutation: completed.clone(),
    };
    assert!(discharges(&recorded, &evidence));
    assert!(!discharges(&evidence, &recorded));
    assert_ne!(
        crate::ingress::receipt_key(&recorded).expect("deferred receipt identity"),
        crate::ingress::receipt_key(&evidence).expect("completion receipt identity")
    );
    for changed in 0..10 {
        let mut mismatch = completed.clone();
        match changed {
            0 => mismatch.recipient = "other@example.com".parse().expect("recipient"),
            1 => mismatch.room = "other@muc.example.com".parse().expect("room"),
            2 => {
                mismatch.thread_id =
                    Some(waddle_xmpp_core::mam::ThreadId::new("thread").expect("thread"))
            }
            3 => mismatch.archive_stanza_id.id = "other".into(),
            4 => mismatch.sender = "other@example.com/phone".parse().expect("sender"),
            5 => mismatch.is_live_occupant = true,
            6 => mismatch.room_members_only = false,
            7 => mismatch.sender_can_broadcast_channel_mention = true,
            8 => mismatch.created_at_ms += 1,
            _ => mismatch.action = Action::Recorded,
        }
        assert!(!discharges(
            &recorded,
            &IngressEffectIntent::GroupchatNotificationRecovery { mutation: mismatch }
        ));
    }
}
