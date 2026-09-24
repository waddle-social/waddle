//! A retry round cannot repeat the accepted half of a direct resource fanout.
use super::*;
use crate::ingress::DispatchProbeBudget;
use crate::ingress_uow::DeliveryProgressRepository;
use crate::server::routes::interpret::effects::{
    direct::DurableDirectEffect, DurableEffect, Effect,
};
use waddle_xmpp::ingress::IngressEffectIntent;
use waddle_xmpp::mam::{ArchiveExpectation, ArchivedMessage};

#[tokio::test]
async fn sqlite_partial_detached_fanout_rechecks_preserve_accepted_resource() {
    let fixture = IngressFixture::sqlite().await;
    let persistence = Arc::new(
        crate::sm_persistence::DatabaseSmPersistence::open(Some(fixture.db.database_url()))
            .await
            .expect("SM persistence"),
    );
    let sm = Arc::new(InMemorySmSessionRegistry::new().with_persistence(persistence));
    let first: jid::FullJid = "juliet@example.com/phone".parse().expect("first");
    let second: jid::FullJid = "juliet@example.com/laptop".parse().expect("second");
    for resource in [&first, &second] {
        sm.store_session(DetachedSession {
            stream_id: resource.to_string(),
            user_id: resource.to_bare().to_string(),
            jid: resource.clone(),
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
        .expect("detached session");
    }
    let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    let mut deps = Deps::new(&state.deps.protocol.connection_registry, "example.com");
    deps.user_registry = Some(&state.deps.protocol.user_registry);
    deps.sm_session_registry = Some(&sm);
    let mut decisions = Vec::new();
    for (id, resources) in [
        ("detached-round-A", vec![second.clone()]),
        ("detached-round-B", vec![first.clone(), second.clone()]),
    ] {
        let mut submission = fixture.submission(Some(id), id);
        assert_eq!(
            submission.plan.sanitized_message.to,
            Some(first.to_bare().into())
        );
        let identity = EffectMessageIdentity::capture_ordinal(1);
        let stamp = waddle_xmpp_core::xep0359::StanzaId::new(id, first.to_bare().into());
        waddle_xmpp_core::xep0359::add_stanza_id(&mut submission.plan.sanitized_message, &stamp);
        let archived_at = chrono::Utc::now();
        submission.plan.intents = vec![
            IngressEffectIntent::ArchiveAuthoritative {
                ordinal: None,
                archive: first.to_bare(),
                by: first.to_bare(),
                stanza_id: stamp.clone(),
                archived_at,
            },
            IngressEffectIntent::RouteDirect {
                recipient: first.to_bare(),
                fanout: resources.clone(),
                route_identity: identity.clone(),
            },
        ];
        submission.plan.plan = capture_delivery(
            &deps,
            ExternalDeliveryEffect::QueueDetached {
                route_identity: Some(identity),
                call_setup: None,
                bare: first.to_bare(),
                resources,
                stanza: Box::new(Stanza::Message(submission.plan.sanitized_message.clone())),
            },
        );
        let mut archived =
            ArchivedMessage::for_test(submission.sender.clone().into(), first.to_bare().into());
        archived.id = id.to_owned();
        archived.timestamp = archived_at;
        archived.body = Some(id.to_owned());
        archived.message_type = xmpp_parsers::message::MessageType::Chat;
        archived.stanza_id = Some(stamp);
        archived.origin_id = submission.digest_input.origin().cloned();
        submission
            .plan
            .plan
            .push(PlannedEffect::new(Effect::Durable(DurableEffect::Direct(
                DurableDirectEffect::ArchiveDirect {
                    archive: first.to_bare(),
                    message: Box::new(archived),
                    archive_expectation: ArchiveExpectation::Fresh,
                },
            ))));
        let decision = commit_submission(&fixture.uow, &submission, 1)
            .await
            .expect("commit archived direct fanout");
        assert!(!decision.archive_ids.is_empty());
        decisions.push(decision);
    }
    // The predecessor's frozen audience contains only laptop. Phone can accept
    // B immediately, even though its sibling keeps their shared effect pending.
    let successor = &decisions[1];
    let budget = DispatchProbeBudget::default();
    deps.dispatch_probe_budget = Some(budget.clone());
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        successor,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert!(report.receipt_failures.is_empty(), "{report:?}");
    assert!(report.frame_obligations.is_empty(), "{report:?}");
    assert_eq!(report.outcomes.len(), 1);
    assert_eq!(report.outcomes[0].1, ExternalOutcome::AwaitingPredecessor);
    assert_eq!(
        budget.consumed_backoffs(),
        4,
        "all shared rounds were exercised"
    );
    let first_session = sm
        .peek_session(&first.to_string())
        .await
        .expect("first session")
        .expect("retained session");
    assert_eq!(
        first_session.unacked_stanzas.len(),
        1,
        "rechecking a partially completed effect must not append phone again"
    );
    let xml: minidom::Element = first_session.unacked_stanzas[0]
        .stanza_xml
        .parse()
        .expect("queued XML");
    let message = xmpp_parsers::message::Message::try_from(xml).expect("queued message");
    assert_eq!(message.to, Some(first.to_bare().into()));
    assert_eq!(
        message.bodies.values().next().expect("body"),
        "detached-round-B"
    );
    assert!(sm
        .peek_session(&second.to_string())
        .await
        .expect("second session")
        .expect("retained blocked session")
        .unacked_stanzas
        .is_empty());
    let mut tx = fixture.uow.begin().await.expect("inspect progress");
    let key = successor.message_key.expect("successor key");
    let progress = &successor.route_progress[0];
    assert_eq!(
        DeliveryProgressRepository::load(&mut tx, key, &progress.receipt)
            .await
            .expect("accepted resources"),
        vec![first],
        "only the accepted sibling has durable delivery progress"
    );
    assert!(!EffectReceiptRepository::receipts_complete(&mut tx, key)
        .await
        .expect("partial receipt remains pending"));
    tx.commit().await.expect("inspection commit");
    fixture.close().await;
}
