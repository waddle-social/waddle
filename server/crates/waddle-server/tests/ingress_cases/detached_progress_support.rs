//! Real Phase B/C detached-delivery fixtures shared with the XEP-0198 suite.
use std::{sync::Arc, time::Duration};

use jid::FullJid;
use waddle_server::{
    ingress::{
        commit::commit_submission,
        effects::{delivery::ExternalDeliveryEffect, Effect, PlanSuppressionPolicy},
        execute::execute_effects,
        Deps, ExternalEffect, ImmediateSink, IngressDecision, IngressDecisionClass,
        IngressSubmission, PlannedEffect,
    },
    sm_persistence::DatabaseSmPersistence,
};
use waddle_xmpp::{
    ingress::{EffectMessageIdentity, IngressEffectIntent},
    registry::ConnectionRegistry,
    stream_management::{DetachedSession, InMemorySmSessionRegistry, SmSessionRegistry},
    Stanza,
};

use crate::ingress_support::IngressFixture;

pub fn resources() -> [FullJid; 3] {
    [
        "juliet@example.com/a",
        "juliet@example.com/b",
        "juliet@example.com/c",
    ]
    .map(|jid| jid.parse().expect("resource JID"))
}

pub async fn registry(fixture: &IngressFixture) -> Arc<InMemorySmSessionRegistry> {
    let persistence = Arc::new(
        DatabaseSmPersistence::open(Some(fixture.db.database_url()))
            .await
            .expect("SM persistence"),
    );
    Arc::new(InMemorySmSessionRegistry::new().with_persistence(persistence))
}

pub async fn attach(sm: &InMemorySmSessionRegistry, jid: &FullJid) {
    sm.store_session(DetachedSession {
        stream_id: jid.to_string(),
        user_id: jid.to_bare().to_string(),
        jid: jid.clone(),
        occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
        inbound_count: 0,
        outbound_count: 0,
        last_acked: 0,
        replay_gap_through: None,
        unacked_stanzas: Vec::new(),
        max_resume_time: Some(300),
        detached_at: std::time::Instant::now(),
        carbons_enabled: false,
        roster_interested: false,
        blocklist_interested: false,
        presence_available: false,
        presence_show: None,
        presence_status: None,
        presence_priority: 0,
        presence_payloads: Vec::new(),
        pending_subscribes_flushed: false,
    })
    .await
    .expect("store detached session");
}

pub fn route(submission: &mut IngressSubmission, fanout: &[FullJid], ordinal: u64) {
    let identity = EffectMessageIdentity::capture_ordinal(ordinal);
    submission
        .plan
        .intents
        .push(IngressEffectIntent::RouteDirect {
            recipient: fanout[0].to_bare(),
            fanout: fanout.to_vec(),
            route_identity: identity.clone(),
        });
    submission.plan.plan.push(detached_effect(
        fanout,
        identity,
        submission.plan.sanitized_message.clone(),
    ));
}

pub fn detached_effect(
    targets: &[FullJid],
    identity: EffectMessageIdentity,
    message: xmpp_parsers::message::Message,
) -> PlannedEffect {
    PlannedEffect::new(Effect::External(ExternalEffect::Delivery(
        ExternalDeliveryEffect::QueueDetached {
            route_identity: Some(identity),
            call_setup: None,
            bare: targets[0].to_bare(),
            resources: targets.to_vec(),
            stanza: Box::new(Stanza::Message(message)),
        },
    )))
    .with_suppression(PlanSuppressionPolicy::SenderOnly)
}

pub async fn execute(
    fixture: &IngressFixture,
    decision: &IngressDecision,
    registry: &ConnectionRegistry,
    sm: &Arc<InMemorySmSessionRegistry>,
) {
    let mut deps = Deps::new(registry, "example.com");
    deps.sm_session_registry = Some(sm);
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert!(report.receipt_failures.is_empty(), "{report:?}");
    assert!(report.terminalization_failure.is_none(), "{report:?}");
}

pub async fn queued(sm: &InMemorySmSessionRegistry, jid: &FullJid) -> DetachedSession {
    sm.peek_session(&jid.to_string())
        .await
        .expect("peek session")
        .expect("detached session exists")
}

pub async fn assert_pending(fixture: &IngressFixture, progress: i64, receipts: i64) {
    assert_eq!(fixture.count("ingress_delivery_receipts").await, progress);
    assert_eq!(fixture.count("ingress_effect_receipts").await, receipts);
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        0
    );
}

pub async fn retry_decision(
    fixture: &IngressFixture,
    submission: &IngressSubmission,
) -> IngressDecision {
    let decision = commit_submission(&fixture.uow, submission, 5)
        .await
        .expect("ordinary duplicate commit");
    assert!(decision.class.advances(), "{decision:?}");
    assert_eq!(
        decision.alias,
        waddle_server::ingress::AliasOutcomeClass::Existing
    );
    assert_ne!(decision.class, IngressDecisionClass::Accepted);
    decision
}

/// A process restart must preserve both the SM append and its ingress progress;
/// XEP-0198 retransmission never authorizes a second append to the finished stream.
pub async fn restart_and_retry(fixture: IngressFixture) {
    let [a, b, _] = resources();
    let connections = ConnectionRegistry::new();
    let sm = registry(&fixture).await;
    attach(&sm, &a).await;
    let mut submission = fixture.submission(Some("detached-restart"), "canonical body");
    route(&mut submission, &[a.clone(), b.clone()], 1);
    let first = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("first commit");
    execute(&fixture, &first, &connections, &sm).await;
    assert_pending(&fixture, 1, 0).await;
    assert_eq!(queued(&sm, &a).await.unacked_stanzas.len(), 1);
    drop(sm);

    let sm = registry(&fixture).await;
    assert_eq!(
        sm.restore_from_persistence()
            .await
            .expect("restore SM on restart"),
        1
    );
    attach(&sm, &b).await;
    // The authenticated digest is unchanged; today's provisional routed stanza
    // can drift independently and must never replace the committed envelope.
    let mut provisional = submission.plan.sanitized_message.clone();
    provisional.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "policy drift".to_owned(),
    );
    submission.plan.plan = vec![detached_effect(
        &[a.clone(), b.clone()],
        EffectMessageIdentity::capture_ordinal(1),
        provisional,
    )];
    let retry = retry_decision(&fixture, &submission).await;
    assert_eq!(retry.message_key, first.message_key);
    assert_eq!(retry.external.len(), 1);
    let ExternalEffect::Delivery(ExternalDeliveryEffect::QueueDetached { resources, .. }) =
        &retry.external[0]
    else {
        panic!("detached retry");
    };
    assert_eq!(
        resources,
        std::slice::from_ref(&b),
        "ordinary replay trims the committed A progress"
    );
    execute(&fixture, &retry, &connections, &sm).await;
    assert_eq!(queued(&sm, &a).await.unacked_stanzas.len(), 1);
    let b_session = queued(&sm, &b).await;
    assert_eq!(b_session.unacked_stanzas.len(), 1);
    let stanza: minidom::Element = b_session.unacked_stanzas[0]
        .stanza_xml
        .parse()
        .expect("SM wire XML");
    let delivered = xmpp_parsers::message::Message::try_from(stanza).expect("SM message");
    let delivered: minidom::Element = delivered.into();
    let canonical: minidom::Element = submission.plan.sanitized_message.clone().into();
    assert_eq!(
        delivered, canonical,
        "SM replay carries exactly the canonical envelope"
    );
    assert_eq!(fixture.count("ingress_delivery_receipts").await, 2);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
    drop(sm);
    fixture.close().await;
}

fn archived_submission(fixture: &IngressFixture, stamp: &str) -> IngressSubmission {
    use waddle_server::ingress::{effects::direct::DurableDirectEffect, DurableEffect};
    use waddle_xmpp::mam::{ArchiveExpectation, ArchivedMessage};
    use waddle_xmpp_core::xep0359::{add_stanza_id, StanzaId};

    let mut submission = fixture.submission(Some("detached-archive-drift"), "archived canonical");
    let recipient = resources()[0].to_bare();
    for archive in [fixture.principal.bare_jid().clone(), recipient.clone()] {
        let id = format!("{stamp}-{archive}");
        let stanza_id = StanzaId::new(&id, archive.clone().into());
        let mut message =
            ArchivedMessage::for_test(submission.sender.clone().into(), recipient.clone().into());
        message.id = id;
        message.body = Some("archived canonical".to_owned());
        message.message_type = xmpp_parsers::message::MessageType::Chat;
        message.origin_id = submission.digest_input.origin().cloned();
        message.stanza_id = Some(stanza_id.clone());
        submission
            .plan
            .intents
            .push(IngressEffectIntent::ArchiveAuthoritative {
                archive: archive.clone(),
                stanza_id: stanza_id.clone(),
                by: archive.clone(),
                archived_at: message.timestamp,
            });
        submission
            .plan
            .plan
            .push(PlannedEffect::new(Effect::Durable(DurableEffect::Direct(
                DurableDirectEffect::ArchiveDirect {
                    archive: archive.clone(),
                    message: Box::new(message),
                    archive_expectation: ArchiveExpectation::Fresh,
                },
            ))));
        if archive == *fixture.principal.bare_jid() {
            add_stanza_id(&mut submission.plan.sanitized_message, &stanza_id);
        }
    }
    let [a, b, _] = resources();
    route(&mut submission, &[a, b], 1);
    let Some(planned) = submission.plan.plan.last_mut() else {
        panic!("route effect")
    };
    let Effect::External(ExternalEffect::Delivery(ExternalDeliveryEffect::QueueDetached {
        stanza,
        ..
    })) = &mut planned.effect
    else {
        panic!("detached effect")
    };
    let Stanza::Message(message) = stanza.as_mut() else {
        panic!("message")
    };
    add_stanza_id(
        message,
        &StanzaId::new(format!("{stamp}-{recipient}"), recipient.into()),
    );
    submission
}

/// Provisional sender/recipient archive identities differ on a duplicate;
/// the retry must retain both assigning authorities' original XEP-0359 IDs.
pub async fn cross_archive_retry(fixture: IngressFixture) {
    use waddle_xmpp_core::xep0359::{extract_stanza_ids, StanzaId};
    let [a, b, _] = resources();
    let connections = ConnectionRegistry::new();
    let sm = registry(&fixture).await;
    attach(&sm, &a).await;
    let first_submission = archived_submission(&fixture, "recorded-stamp");
    let first = commit_submission(&fixture.uow, &first_submission, 5)
        .await
        .expect("archive commit");
    execute(&fixture, &first, &connections, &sm).await;
    assert_pending(&fixture, 1, 2).await;
    attach(&sm, &b).await;
    let retry_submission = archived_submission(&fixture, "discarded-provisional-stamp");
    let retry = commit_submission(&fixture.uow, &retry_submission, 5)
        .await
        .expect("cross-archive duplicate");
    assert!(retry.class.advances(), "{retry:?}");
    assert_ne!(retry.class, IngressDecisionClass::Accepted);
    assert_eq!(retry.message_key, first.message_key);
    assert_eq!(retry.archive_ids, first.archive_ids);
    execute(&fixture, &retry, &connections, &sm).await;
    assert_eq!(queued(&sm, &a).await.unacked_stanzas.len(), 1);
    let session = queued(&sm, &b).await;
    assert_eq!(session.unacked_stanzas.len(), 1);
    let element: minidom::Element = session.unacked_stanzas[0]
        .stanza_xml
        .parse()
        .expect("SM XML");
    let delivered = xmpp_parsers::message::Message::try_from(element).expect("delivered message");
    let stamps = extract_stanza_ids(&delivered);
    assert_eq!(stamps.len(), 2);
    assert!(stamps.contains(&StanzaId::new(
        "recorded-stamp-romeo@example.com",
        fixture.principal.bare_jid().clone().into()
    )));
    assert!(stamps.contains(&StanzaId::new(
        "recorded-stamp-juliet@example.com",
        b.to_bare().into()
    )));
    assert_eq!(fixture.count("mam_messages").await, 2);
    assert_eq!(
        fixture
            .count("mam_messages WHERE id LIKE 'discarded-provisional-stamp%'")
            .await,
        0
    );
    assert_eq!(fixture.count("ingress_delivery_receipts").await, 2);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 3);
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
    drop(sm);
    fixture.close().await;
}
