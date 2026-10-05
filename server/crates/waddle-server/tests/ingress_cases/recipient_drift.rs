//! RFC 0018 §3 recorded-wins and XEP-0198: resource drift cannot revoke acceptance.
use super::*;
use waddle_server::{
    ingress::{
        effects::delivery::{ExternalDeliveryEffect, PeerDeliveryKind},
        ExternalEffect, IngressStreamIdentity, PlanSuppressionPolicy,
    },
    ingress_uow::{EffectIntentRepository, SmIngressStreamRepository},
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
    let sender_archive_effects = submission.plan.plan.clone();
    if missing_sender {
        submission.plan.intents.clear();
        submission.plan.plan.clear();
    }
    // Both archive authorities belong to the original ingress transaction,
    // before an already-processed frame is offered to the live recipient.
    let recipient_id = StanzaId::new("live-recipient-id", recipient.clone().into());
    let mut recipient_message =
        ArchivedMessage::for_test(submission.sender.clone().into(), full.clone().into());
    recipient_message.id = recipient_id.id.clone();
    recipient_message.stanza_id = Some(recipient_id.clone());
    recipient_message.body = Some("hello".to_owned());
    recipient_message.message_type = xmpp_parsers::message::MessageType::Chat;
    let recipient_authority = IngressEffectIntent::ArchiveAuthoritative {
        archive: recipient.clone(),
        by: recipient.clone(),
        stanza_id: recipient_id.clone(),
        archived_at: recipient_message.timestamp,
        ordinal: None,
    };
    submission.plan.intents.push(recipient_authority.clone());
    submission
        .plan
        .plan
        .push(PlannedEffect::new(Effect::Durable(DurableEffect::Direct(
            DurableDirectEffect::ArchiveDirect {
                archive: recipient.clone(),
                message: Box::new(recipient_message.clone()),
                archive_expectation: ArchiveExpectation::Fresh,
            },
        ))));
    let route = IngressEffectIntent::RouteDirect {
        prepared: None,
        recipient: recipient.clone(),
        fanout: vec![full.clone()],
        route_identity: route_identity.clone(),
    };
    submission.plan.intents.push(route.clone());
    let mut delivered_message = submission.plan.sanitized_message.clone();
    waddle_xmpp_core::xep0359::add_stanza_id(&mut delivered_message, &recipient_id);
    submission.plan.plan.push(
        PlannedEffect::new(Effect::External(ExternalEffect::Delivery(
            ExternalDeliveryEffect::RouteToPeer {
                route_identity: Some(route_identity.clone()),
                jid: full.clone(),
                stanza: Box::new(waddle_xmpp::Stanza::Message(delivered_message)),
                kind: PeerDeliveryKind::DirectFrame,
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
    assert!(first
        .archive_ids
        .contains(&(recipient.clone(), recipient_id.clone())));
    let mut tx = fixture.uow.begin().await.expect("inspect original commit");
    let recorded = EffectIntentRepository::load(&mut tx, first.message_key.expect("key"))
        .await
        .expect("recorded live obligations");
    assert!(recorded.contains(&route));
    assert!(recorded.iter().any(|intent| matches!(intent,
        IngressEffectIntent::ArchiveAuthoritative {
            archive, by, stanza_id, archived_at, ordinal: Some(_),
        } if archive == &recipient && by == &recipient && stanza_id == &recipient_id
            && *archived_at == recipient_message.timestamp
    )));
    tx.commit().await.expect("original commit inspected");
    let archive_count = fixture.count("mam_messages").await;
    assert_eq!(archive_count, if missing_sender { 1 } else { 2 });
    let intent_count = fixture.count("ingress_effect_intents").await;

    // The addressed resource disconnects. Replanning now captures the detached
    // recipient pass with a new provisional archive identity and queue effect.
    submission.plan.plan.retain(|planned| {
        matches!(&planned.effect,
            Effect::Durable(DurableEffect::Direct(DurableDirectEffect::ArchiveDirect {
                archive, ..
            })) if archive != &recipient)
    });
    submission
        .plan
        .intents
        .retain(|intent| {
            !matches!(intent, IngressEffectIntent::RouteDirect { .. })
                && !matches!(intent,
                    IngressEffectIntent::ArchiveAuthoritative { archive, .. } if archive == &recipient)
        });
    if missing_sender {
        submission.plan.intents.extend(sender_archive_intents);
        submission.plan.plan.extend(sender_archive_effects);
    }
    let new_id = StanzaId::new("detached-recipient-id", recipient.clone().into());
    recipient_message.id = new_id.id.clone();
    recipient_message.stanza_id = Some(new_id.clone());
    submission
        .plan
        .intents
        .push(IngressEffectIntent::ArchiveAuthoritative {
            archive: recipient.clone(),
            by: recipient.clone(),
            stanza_id: new_id.clone(),
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
    let mut detached_message = submission.plan.sanitized_message.clone();
    waddle_xmpp_core::xep0359::add_stanza_id(&mut detached_message, &new_id);
    submission.plan.plan.push(
        PlannedEffect::new(Effect::External(ExternalEffect::Delivery(
            ExternalDeliveryEffect::QueueDetached {
                route_identity: Some(route_identity),
                call_setup: None,
                bare: recipient.clone(),
                resources: vec![full.clone()],
                stanza: Box::new(waddle_xmpp::Stanza::Message(detached_message)),
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
        assert_eq!(duplicate.archive_ids, first.archive_ids);
        let stamps = waddle_xmpp_core::xep0359::extract_stanza_ids(copy);
        assert!(
            stamps.contains(&recipient_id),
            "recorded recipient authority"
        );
        assert!(
            !stamps.contains(&new_id),
            "provisional recipient ID discarded"
        );
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
