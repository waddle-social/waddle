//! RFC 0018 §3 recorded-wins and XEP-0198: resource drift cannot revoke acceptance.
use super::*;
use waddle_server::{
    ingress::{
        effects::delivery::{ExternalDeliveryEffect, PeerDeliveryKind},
        ExternalEffect, IngressStreamIdentity, PlanSuppressionPolicy,
    },
    ingress_uow::{EffectIntentRepository, MamArchiveRepository, SmIngressStreamRepository},
};
use waddle_xmpp::ingress::{
    DigestContext, DigestInput, EffectMessageIdentity, NormalizedTarget, WireHandledCount,
};

async fn recipient_plan_drift(fixture: IngressFixture, missing_sender: bool) {
    let full: jid::FullJid = "juliet@example.com/phone".parse().expect("recipient");
    let recipient = full.to_bare();
    let route_identity = EffectMessageIdentity::capture_ordinal(0);
    let mut submission = archive_plan(&fixture, Some("live-origin"), "hello", "sender-id");
    submission.target = NormalizedTarget::Full(full.clone());
    submission.plan.sanitized_message.to = Some(full.clone().into());
    submission.digest_input = DigestInput::from_parsed(
        &submission.plan.sanitized_message,
        &DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![fixture.principal.bare_jid().clone()],
            stanza_lang: None,
        },
    )
    .expect("full-JID digest");
    submission.identity = super::replay::resumable_identity(&fixture, "live-drift", 1).await;
    let sender_archive_intents = submission.plan.intents.clone();
    if missing_sender {
        submission.plan.intents.clear();
        submission.plan.plan.clear();
    }
    let route = IngressEffectIntent::RouteDirect {
        recipient: recipient.clone(),
        fanout: vec![full.clone()],
        route_identity: route_identity.clone(),
    };
    // The live route records delivery, while its recipient archive belongs to
    // the destination connection's separate recipient pass.
    submission.plan.intents.push(route.clone());
    submission.plan.plan.push(
        PlannedEffect::new(Effect::External(ExternalEffect::Delivery(
            ExternalDeliveryEffect::RouteToPeer {
                route_identity: Some(route_identity.clone()),
                jid: full.clone(),
                stanza: Box::new(waddle_xmpp::Stanza::Message(
                    submission.plan.sanitized_message.clone(),
                )),
                kind: PeerDeliveryKind::PeerStanza,
                call_setup: None,
            },
        )))
        .with_suppression(PlanSuppressionPolicy::SenderOnly),
    );
    let first = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("live commit");
    assert_eq!(first.class, IngressDecisionClass::Accepted);
    assert_eq!(first.external.len(), 1);
    let mut recipient_message =
        ArchivedMessage::for_test(submission.sender.clone().into(), full.clone().into());
    recipient_message.id = "live-recipient-id".to_owned();
    recipient_message.stanza_id =
        Some(StanzaId::new("live-recipient-id", recipient.clone().into()));
    recipient_message.body = Some("hello".to_owned());
    recipient_message.message_type = xmpp_parsers::message::MessageType::Chat;
    let mut tx = fixture.uow.begin().await.expect("recipient pipeline");
    MamArchiveRepository::store(
        &mut tx,
        &recipient,
        &recipient_message,
        ArchiveExpectation::Fresh,
    )
    .await
    .expect("live recipient archive");
    let recorded = EffectIntentRepository::load(&mut tx, first.message_key.expect("key"))
        .await
        .expect("recorded live obligations");
    assert!(recorded.contains(&route));
    assert!(!recorded.iter().any(|intent| matches!(intent,
        IngressEffectIntent::ArchiveAuthoritative { archive, .. } if archive == &recipient)));
    tx.commit().await.expect("recipient pipeline commit");
    let archive_count = fixture.count("mam_messages").await;
    let intent_count = fixture.count("ingress_effect_intents").await;

    // The addressed resource disconnects. Replanning now captures the detached
    // recipient pass with a new provisional archive identity and queue effect.
    submission
        .plan
        .plan
        .retain(|planned| matches!(planned.effect, Effect::Durable(_)));
    submission
        .plan
        .intents
        .retain(|intent| !matches!(intent, IngressEffectIntent::RouteDirect { .. }));
    submission.plan.intents.extend(
        sender_archive_intents
            .into_iter()
            .filter(|intent| !recorded.contains(intent)),
    );
    let new_id = StanzaId::new("detached-recipient-id", recipient.clone().into());
    recipient_message.id = new_id.id.clone();
    recipient_message.stanza_id = Some(new_id.clone());
    submission
        .plan
        .intents
        .push(IngressEffectIntent::ArchiveAuthoritative {
            archive: recipient.clone(),
            by: recipient.clone(),
            stanza_id: new_id,
            archived_at: recipient_message.timestamp,
            ordinal: None,
        });
    submission
        .plan
        .plan
        .push(PlannedEffect::new(Effect::Durable(DurableEffect::Direct(
            DurableDirectEffect::ArchiveDirect {
                archive: recipient.clone(),
                message: Box::new(recipient_message),
                archive_expectation: ArchiveExpectation::Fresh,
            },
        ))));
    submission.plan.intents.push(route.clone());
    submission.plan.plan.push(
        PlannedEffect::new(Effect::External(ExternalEffect::Delivery(
            ExternalDeliveryEffect::QueueDetached {
                route_identity: Some(route_identity),
                call_setup: None,
                bare: recipient.clone(),
                resources: vec![full.clone()],
                stanza: Box::new(waddle_xmpp::Stanza::Message(
                    submission.plan.sanitized_message.clone(),
                )),
            },
        )))
        .with_suppression(PlanSuppressionPolicy::SenderOnly),
    );
    let IngressStreamIdentity::Resumable {
        reserved_wire_position,
        checkpoint_h,
        sm_ingress_id,
        ..
    } = &mut submission.identity
    else {
        panic!("resumable")
    };
    *reserved_wire_position = WireHandledCount::from_storage(2);
    *checkpoint_h = WireHandledCount::from_storage(2);
    let stream = *sm_ingress_id;
    let retry = commit_submission(&fixture.uow, &submission, 5).await;
    if missing_sender {
        let failure = retry.expect_err("missing original sender authority must refuse");
        assert_eq!(failure.class(), IngressDecisionClass::Storage);
        assert!(!failure.class().advances());
    } else {
        let duplicate = retry.expect("recipient drift advances");
        assert!(duplicate.class.advances());
        assert!(matches!(
            duplicate.class,
            IngressDecisionClass::ExistingConsistent | IngressDecisionClass::ExistingDivergent
        ));
        assert_eq!(duplicate.message_key, first.message_key);
        // #1739: the recorded live route was never receipted, so the replay
        // retries exactly the unfinished recorded resource with the canonical
        // payload instead of suppressing it as a stated limitation.
        assert_eq!(
            duplicate.external.len(),
            1,
            "one unfinished recorded resource"
        );
        let ExternalEffect::Delivery(ExternalDeliveryEffect::QueueDetached {
            resources,
            stanza,
            ..
        }) = &duplicate.external[0]
        else {
            panic!("detached retry for the unfinished recorded resource");
        };
        assert_eq!(resources.as_slice(), std::slice::from_ref(&full));
        let waddle_xmpp::Stanza::Message(copy) = stanza.as_ref() else {
            panic!("message copy");
        };
        assert_eq!(
            copy.bodies.values().next().map(String::as_str),
            Some("hello"),
            "canonical payload"
        );
        assert_eq!(duplicate.route_progress.len(), 1);
        assert!(duplicate.route_progress[0].completed.is_empty());
        assert!(!duplicate
            .archive_ids
            .iter()
            .any(|(archive, _)| archive == &recipient));
    }
    assert_eq!(fixture.count("mam_messages").await, archive_count);
    {
        let database = fixture.db.guard().await.expect("archive database");
        let mut rows = database
            .query(
                "SELECT stanza_id, body FROM mam_messages WHERE id = ?",
                waddle_server::db_params!["live-recipient-id".to_owned()],
            )
            .await
            .expect("original recipient row");
        let row = rows
            .next()
            .await
            .expect("row")
            .expect("live recipient retained");
        assert_eq!(
            row.get::<String>(0).expect("stanza id"),
            "live-recipient-id"
        );
        assert_eq!(row.get::<String>(1).expect("body"), "hello");
    }
    assert_eq!(fixture.count("ingress_effect_intents").await, intent_count);
    let mut tx = fixture.uow.begin().await.expect("inspect retry");
    assert_eq!(
        EffectIntentRepository::load(&mut tx, first.message_key.expect("key"))
            .await
            .expect("intents"),
        recorded
    );
    assert_eq!(
        SmIngressStreamRepository::load_stream_checkpoint(&mut tx, stream)
            .await
            .expect("h"),
        Some(WireHandledCount::from_storage(if missing_sender {
            1
        } else {
            2
        }))
    );
    tx.commit().await.expect("read commit");
    assert_eq!(
        fixture.count("ingress_sm_refs").await,
        if missing_sender { 1 } else { 2 }
    );
    fixture.close().await;
}

#[tokio::test]
async fn ingress_live_full_jid_recipient_drift_sqlite() {
    recipient_plan_drift(IngressFixture::sqlite().await, false).await;
}
#[tokio::test]
async fn ingress_live_full_jid_recipient_drift_postgres() {
    if let Some(fixture) = IngressFixture::postgres("live_recipient_drift").await {
        recipient_plan_drift(fixture, false).await;
    }
}
#[tokio::test]
async fn ingress_live_full_jid_missing_sender_authority_sqlite() {
    recipient_plan_drift(IngressFixture::sqlite().await, true).await;
}
#[tokio::test]
async fn ingress_live_full_jid_missing_sender_authority_postgres() {
    if let Some(fixture) = IngressFixture::postgres("live_missing_sender").await {
        recipient_plan_drift(fixture, true).await;
    }
}
