use super::{execute, LOSE_COMMIT_ACK};
use crate::ingress::commit::commit_submission;
use crate::ingress::decision::IngressDecision;
use crate::ingress::test_support::IngressFixture;
use crate::ingress_uow::{CanonicalMessageRepository, EffectReceiptRepository};
use crate::notification_outbox::{direct_candidate_from_envelope, NotificationOutboxStore};
use crate::server::routes::interpret::effects::delivery::ExternalDeliveryEffect;
use crate::server::routes::interpret::effects::{
    Effect, EffectOutcome, ExternalEffect, PlannedEffect, SettledCompletion,
};
use jid::BareJid;
use waddle_xmpp::ingress::{
    DigestContext, DigestInput, IngressEffectIntent, NotificationActivityMutation,
    NotificationCandidateOutcome,
};
use waddle_xmpp::xep::xep0353::{build_propose, CallOffer};
use waddle_xmpp_core::xep0359::{add_stanza_id, StanzaId};
use xmpp_parsers::jingle::SessionId;

async fn call_decision(fixture: &IngressFixture) -> (IngressDecision, IngressEffectIntent) {
    let mut submission = fixture.submission(Some("live-call-effect-replay"), "");
    let recipient: BareJid = "juliet@example.com".parse().expect("recipient");
    let message = &mut submission.plan.sanitized_message;
    message.bodies.clear();
    message.payloads.push(build_propose(
        SessionId("live-call-effect-replay".into()),
        CallOffer::audio_only(),
    ));
    let stamp = StanzaId::new("live-call-effect-archive", recipient.clone().into());
    let candidate = direct_candidate_from_envelope(
        message,
        &recipient,
        message.from.as_ref().expect("sender"),
        &stamp,
    )
    .expect("call candidate");
    submission.digest_input = DigestInput::from_parsed(
        message,
        &DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![fixture.principal.bare_jid().clone(), recipient.clone()],
            stanza_lang: None,
        },
    )
    .expect("call digest");
    add_stanza_id(message, &stamp);
    let intent = IngressEffectIntent::NotificationActivityPreview {
        owner: recipient.clone(),
        mutation: NotificationActivityMutation::NotificationCandidate {
            conversation: recipient,
            archive_stanza_id: stamp,
            outcome: NotificationCandidateOutcome::Inserted,
        },
    };
    submission.plan.intents = vec![intent.clone()];
    submission.plan.plan = vec![PlannedEffect::new(Effect::External(
        ExternalEffect::Delivery(ExternalDeliveryEffect::NotificationCandidate {
            candidate: Box::new(candidate),
        }),
    ))];
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit frozen call obligation");
    assert_eq!(decision.external.len(), 1);
    assert_eq!(decision.external_receipts[0].len(), 1);
    assert_eq!(decision.arm_owned_receipts, decision.external_receipts[0]);
    (decision, intent)
}

#[tokio::test]
async fn sqlite_live_call_effect_replay_keeps_one_candidate_and_original_receipt_time() {
    let fixture = IngressFixture::sqlite().await;
    let store = NotificationOutboxStore::try_new(fixture.db.clone())
        .await
        .expect("notification schema");
    let (decision, intent) = call_decision(&fixture).await;
    let key = decision.message_key.expect("canonical key");
    let original_receipt = chrono::Utc::now() - chrono::Duration::seconds(20);
    fixture
        .execute(
            "UPDATE ingress_messages SET created_at = ? WHERE message_key = ?",
            crate::db_params![original_receipt.to_rfc3339(), key.to_storage().to_string()],
        )
        .await;
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("read canonical receipt time");
    assert!(CanonicalMessageRepository::lock(&mut tx, key)
        .await
        .expect("canonical custody"));
    let original_receipt = CanonicalMessageRepository::created_at(&mut tx, key)
        .await
        .expect("canonical receipt time");
    tx.commit().await.expect("read commit");
    let ExternalEffect::Delivery(effect) = &decision.external[0] else {
        panic!("call candidate delivery effect");
    };

    for attempt in 0..2 {
        let EffectOutcome::Settled(outcome) = execute(&fixture.uow, &decision, 0, effect).await
        else {
            panic!("call candidate effect must settle");
        };
        assert_eq!(outcome.completion, SettledCompletion::Complete);
        assert_eq!(
            outcome.persisted,
            if attempt == 0 {
                vec![intent.clone()]
            } else {
                Vec::new()
            }
        );
        assert_eq!(
            fixture
                .count("notification_candidates WHERE class = 'call'")
                .await,
            1
        );
        assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
        assert_eq!(fixture.count("ingress_effect_descendants").await, 1);
    }
    let candidates = store.pending_candidates(10).await.expect("pending calls");
    assert_eq!(candidates.len(), 1);
    assert_eq!(
        fixture
            .count("ingress_effect_descendants AS descendant JOIN notification_candidates AS candidate ON descendant.descendant_key = candidate.delivery_id")
            .await,
        1,
        "the recorded obligation owns the candidate's scheduling identity"
    );
    assert_eq!(
        candidates[0].call().expect("call payload").session_id,
        SessionId("live-call-effect-replay".into())
    );
    let connection = fixture.db.guard().await.expect("candidate query");
    let mut rows = connection
        .query("SELECT created_at_ms FROM notification_candidates", ())
        .await
        .expect("candidate receipt time");
    let created_at: i64 = rows
        .next()
        .await
        .expect("candidate row")
        .expect("candidate exists")
        .get(0)
        .expect("candidate timestamp");
    assert_eq!(created_at, original_receipt.timestamp_millis());
    drop(rows);
    drop(connection);
    let receipt = &decision.external_receipts[0][0];
    let mut tx = fixture.uow.begin().await.expect("receipt query");
    assert!(EffectReceiptRepository::contains(
        &mut tx,
        key,
        receipt.kind,
        &receipt.semantic_identity_hash,
    )
    .await
    .expect("recorded receipt"));
    tx.commit().await.expect("receipt read commit");

    // Model candidate retention after dispatch: a stale copy of the frozen
    // effect has no authority to recreate already-settled scheduling work.
    fixture
        .execute("DELETE FROM notification_candidates", ())
        .await;
    let EffectOutcome::Settled(replayed) = execute(&fixture.uow, &decision, 0, effect).await else {
        panic!("settled candidate replay must complete");
    };
    assert_eq!(replayed.completion, SettledCompletion::Complete);
    assert!(replayed.persisted.is_empty());
    assert_eq!(fixture.count("notification_candidates").await, 0);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    assert_eq!(fixture.count("ingress_effect_descendants").await, 1);
    drop(store);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_live_call_lost_commit_ack_is_uncertain_and_replay_does_not_duplicate() {
    let fixture = IngressFixture::sqlite().await;
    let store = NotificationOutboxStore::try_new(fixture.db.clone())
        .await
        .expect("notification schema");
    let (decision, _) = call_decision(&fixture).await;
    let ExternalEffect::Delivery(effect) = &decision.external[0] else {
        panic!("call candidate delivery effect");
    };

    let outcome = LOSE_COMMIT_ACK
        .scope(true, execute(&fixture.uow, &decision, 0, effect))
        .await;
    let EffectOutcome::Settled(uncertain) = outcome else {
        panic!("a lost commit acknowledgement must retain settlement uncertainty");
    };
    assert_eq!(uncertain.completion, SettledCompletion::Uncertain);
    assert!(uncertain.refusal.is_none());
    assert!(uncertain.persisted.is_empty());
    assert!(uncertain.detached.is_none());
    assert_eq!(fixture.count("notification_candidates").await, 1);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    assert_eq!(fixture.count("ingress_effect_descendants").await, 1);

    let EffectOutcome::Settled(replayed) = execute(&fixture.uow, &decision, 0, effect).await else {
        panic!("replay reconciles the committed receipt");
    };
    assert_eq!(replayed.completion, SettledCompletion::Complete);
    assert!(replayed.persisted.is_empty());
    assert_eq!(fixture.count("notification_candidates").await, 1);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    assert_eq!(fixture.count("ingress_effect_descendants").await, 1);
    drop(store);
    fixture.close().await;
}
