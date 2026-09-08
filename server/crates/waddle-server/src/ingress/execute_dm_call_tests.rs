use super::*;
use crate::ingress::{commit::commit_submission, test_support::IngressFixture};
use crate::server::routes::interpret::effects::PlanSuppressionPolicy;
use waddle_xmpp::ingress::{
    DmCallThreadKey, IngressEffectIntent, PendingDmCallOffer, PlannedDmCallState,
};

async fn cancelled_call_offer_replays(fixture: IngressFixture) {
    let mut submission = fixture.submission(Some("call-offer-retry"), "stored call offer");
    let key = DmCallThreadKey::new(
        submission.sender.to_bare(),
        "bob@example.com".parse().expect("peer"),
        xmpp_parsers::jingle::SessionId("call-offer".into()),
    );
    let state = PlannedDmCallState {
        key: key.clone(),
        pending: Some(PendingDmCallOffer {
            media: waddle_xmpp::xep::CallThreadMedia::audio_only(),
            initiator: submission.sender.to_bare(),
            started: chrono::Utc::now(),
        }),
        active: None,
        projected: Default::default(),
    };
    let intent = IngressEffectIntent::DmCallThreadState {
        sequence: 0,
        state: Box::new(state.clone()),
    };
    submission.plan.intents = vec![intent.clone()];
    submission.plan.plan = vec![PlannedEffect::new(Effect::External(ExternalEffect::Direct(
        ExternalDirectEffect::DmCallThreadState {
            state: Box::new(state.clone()),
            receipt: Some(Box::new(intent)),
        },
    )))
    .with_suppression(PlanSuppressionPolicy::Always)];
    let first = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit offer");
    let canonical = first.message_key.expect("canonical");
    assert_eq!(fixture.count("ingress_effect_intents").await, 1);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    assert!(!terminalize_if_complete(&fixture.uow, canonical)
        .await
        .expect("offer pending"));
    // Phase C never starts. Replanning no longer observes the stored propose,
    // so replay must reconstruct the saved transition without mutable lookups.
    submission.plan.intents.clear();
    submission.plan.plan.clear();
    let retry = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("retry offer");
    assert_eq!(retry.external.len(), 1);
    assert_eq!(retry.external_receipts[0].len(), 1);
    let socket = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    let registry = waddle_xmpp::registry::ConnectionRegistry::new();
    let mut deps = Deps::new(&registry, "example.com");
    deps.web_socket_state = Some(socket.as_ref());
    let completed = execute_effects(
        &fixture.uow,
        &fixture.db,
        &retry,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(completed.outcomes[0].1, ExternalOutcome::Done);
    assert!(completed.receipt_failures.is_empty());
    assert_eq!(
        socket
            .deps
            .protocol
            .pending_dm_call_offers
            .get(&key)
            .expect("restored offer")
            .value(),
        state.pending.as_ref().expect("pending")
    );
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    assert!(terminalize_if_complete(&fixture.uow, canonical)
        .await
        .expect("complete"));
    let duplicate = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("receipted retry");
    let unavailable = Deps::new(&registry, "example.com");
    let skipped = execute_effects(
        &fixture.uow,
        &fixture.db,
        &duplicate,
        &ImmediateSink,
        &unavailable,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(skipped.outcomes[0].1, ExternalOutcome::Done);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_ingress_cancelled_dm_call_offer_replays_and_receipts_once() {
    cancelled_call_offer_replays(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_ingress_cancelled_dm_call_offer_replays_and_receipts_once() {
    if let Some(fixture) = IngressFixture::postgres("dm_call_retry").await {
        cancelled_call_offer_replays(fixture).await;
    }
}

async fn fail_call_receipt(fixture: &IngressFixture, receipt: &EffectReceiptKey) {
    // SQL literals encode storage bytes, never an XMPP payload.
    let hash = hex::encode(receipt.semantic_identity_hash);
    match fixture.db.driver() {
        crate::db::DatabaseDriver::Sqlite => {
            fixture.execute(&format!("CREATE TRIGGER fail_call_receipt BEFORE INSERT ON ingress_effect_receipts WHEN NEW.semantic_identity_hash = X'{hash}' BEGIN SELECT RAISE(FAIL, 'injected call receipt failure'); END"), ()).await;
        }
        crate::db::DatabaseDriver::Postgres => {
            fixture.execute(&format!("CREATE FUNCTION fail_call_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.semantic_identity_hash = decode('{hash}', 'hex') THEN RAISE EXCEPTION 'injected call receipt failure'; END IF; RETURN NEW; END $$"), ()).await;
            fixture.execute("CREATE TRIGGER fail_call_receipt BEFORE INSERT ON ingress_effect_receipts FOR EACH ROW EXECUTE FUNCTION fail_call_receipt()", ()).await;
        }
    }
}

async fn allow_call_receipt(fixture: &IngressFixture) {
    let sql = match fixture.db.driver() {
        crate::db::DatabaseDriver::Sqlite => "DROP TRIGGER fail_call_receipt",
        crate::db::DatabaseDriver::Postgres => {
            "DROP TRIGGER fail_call_receipt ON ingress_effect_receipts"
        }
    };
    fixture.execute(sql, ()).await;
}

async fn confirmed_proceed_state_supersedes_pending_transition(fixture: IngressFixture) {
    use crate::server::routes::interpret::{effects::PlanSink, interpret};
    use waddle_xmpp::protocol::OutboundEvent;

    let mut submission = fixture.submission(Some("call-proceed-retry"), "stored proceed");
    let sender = submission.sender.to_bare();
    let peer = submission
        .plan
        .sanitized_message
        .to
        .as_ref()
        .expect("peer")
        .to_bare();
    let key = DmCallThreadKey::new(
        sender.clone(),
        peer.clone(),
        xmpp_parsers::jingle::SessionId("call-proceed".into()),
    );
    let socket = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    let protocol = &socket.deps.protocol;
    protocol.pending_dm_call_offers.insert(
        key.clone(),
        PendingDmCallOffer {
            media: waddle_xmpp::xep::CallThreadMedia::audio_only(),
            initiator: peer.clone(),
            started: chrono::Utc::now(),
        },
    );
    let sink = PlanSink::new();
    let capture = crate::ingress::IngressEffectCapture::new();
    let mut deps = Deps::test_with_storage(
        &protocol.connection_registry,
        &protocol.mam_storage,
        &protocol.inbox_storage,
    );
    deps.web_socket_state = Some(socket.as_ref());
    deps.effects = &sink;
    deps.ingress_effect_capture = Some(capture.clone());
    let message = &mut submission.plan.sanitized_message;
    waddle_xmpp::xep::xep0334::add_hint(message, waddle_xmpp::xep::xep0334::Hint::Store);
    message.payloads.push(
        minidom::Element::builder("proceed", waddle_xmpp::xep::xep0353::NS_JINGLE_MESSAGE)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "call-proceed")
            .build(),
    );
    for archive in [&sender, &peer] {
        waddle_xmpp_core::xep0359::add_stanza_id(
            message,
            &waddle_xmpp_core::xep0359::StanzaId::new(
                if archive == &sender {
                    "sender-call-anchor"
                } else {
                    "peer-call-anchor"
                },
                archive.clone().into(),
            ),
        );
    }
    // Exercise the real stored JMI sender and recipient archive passes: activation,
    // first archive anchor/projection, then the complete two-peer projection.
    interpret(
        [&sender, &peer]
            .into_iter()
            .map(|archive| OutboundEvent::ArchiveDirect {
                archive_jid: archive.clone(),
                from: submission.sender.clone().into(),
                to: peer.clone().into(),
                message: Box::new(message.clone()),
            })
            .collect(),
        &deps,
    )
    .await;
    submission.plan.intents = capture
        .snapshot()
        .intents
        .into_iter()
        .filter(|intent| {
            matches!(
                intent,
                IngressEffectIntent::DmCallThreadState { .. }
                    | IngressEffectIntent::ArchiveAuthoritative { .. }
            )
        })
        .collect();
    submission.plan.plan = sink.take().0.into_iter().filter(|planned| {
        matches!(planned.effect, Effect::External(ExternalEffect::Direct(ExternalDirectEffect::DmCallThreadState { .. })) | Effect::Durable(crate::server::routes::interpret::effects::DurableEffect::Direct(crate::server::routes::interpret::effects::direct::DurableDirectEffect::ArchiveDirect { .. })))
    }).collect();
    assert_eq!(
        submission
            .plan
            .intents
            .iter()
            .filter(|intent| matches!(intent, IngressEffectIntent::DmCallThreadState { .. }))
            .count(),
        3,
        "real proceed captures three snapshots"
    );
    let IngressEffectIntent::DmCallThreadState {
        state: final_state, ..
    } = submission.plan.intents.last().expect("final state")
    else {
        panic!("call state intent");
    };
    let expected = final_state.clone();
    assert!(expected.pending.is_none());
    assert_eq!(expected.projected.len(), 2);
    assert!(expected.active.as_ref().expect("active").anchor.is_some());
    assert!(protocol.dm_call_threads.is_empty(), "planning is read-only");
    let first = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit proceed");
    let canonical = first.message_key.expect("canonical");
    let first_receipt = &first.external_receipts[0][0];
    fail_call_receipt(&fixture, first_receipt).await;
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &first,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(report.receipt_failures.len(), 1);
    assert_eq!(report.receipt_failures[0].0, *first_receipt);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 4);
    assert!(!terminalize_if_complete(&fixture.uow, canonical)
        .await
        .expect("earliest receipt pending"));
    allow_call_receipt(&fixture).await;
    submission.plan.intents.clear();
    submission.plan.plan.clear();
    let retry = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("alias replay");
    assert_eq!(retry.message_key, Some(canonical));
    assert_eq!(retry.receipts_pending.len(), 1);
    assert_eq!(retry.external.len(), 3);
    // No execution dependency is supplied: every old transition must be discharged
    // from the later confirmed state, rather than reinstalling any snapshot.
    let unavailable = Deps::new(&protocol.connection_registry, "example.com");
    let replay = execute_effects(
        &fixture.uow,
        &fixture.db,
        &retry,
        &ImmediateSink,
        &unavailable,
        Duration::from_secs(5),
    )
    .await;
    assert!(replay
        .outcomes
        .iter()
        .all(|(_, outcome)| *outcome == ExternalOutcome::Done));
    assert!(replay.receipt_failures.is_empty());
    let active = protocol
        .dm_call_threads
        .get(&key)
        .expect("active call preserved");
    let expected_active = expected.active.as_ref().expect("expected active");
    assert_eq!(
        active.anchor_origin_id,
        expected_active.anchor.as_ref().expect("anchor").id
    );
    assert_eq!(active.thread_id, expected_active.thread.as_str());
    drop(active);
    assert!(!protocol.pending_dm_call_offers.contains_key(&key));
    for archive in &expected.projected {
        assert!(protocol
            .dm_call_thread_projections
            .contains(&(archive.clone(), key.clone())));
    }
    assert_eq!(fixture.count("ingress_effect_receipts").await, 5);
    assert!(terminalize_if_complete(&fixture.uow, canonical)
        .await
        .expect("terminalized"));
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_ingress_proceed_replay_preserves_later_receipted_call_state() {
    confirmed_proceed_state_supersedes_pending_transition(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_ingress_proceed_replay_preserves_later_receipted_call_state() {
    if let Some(fixture) = IngressFixture::postgres("call_supersession").await {
        confirmed_proceed_state_supersedes_pending_transition(fixture).await;
    }
}
