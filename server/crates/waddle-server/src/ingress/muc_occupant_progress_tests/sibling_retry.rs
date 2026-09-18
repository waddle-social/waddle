use super::*;
use crate::server::routes::interpret::effects::Effect;
use crate::server::routes::interpret::DeliveryExecutionContext;
use waddle_xmpp::Stanza;
use xmpp_parsers::message::{Lang, MessageType};

async fn sibling_retry(fixture: IngressFixture, subject: bool, canonical: bool) {
    let state = socket_tests::create_test_websocket_state().await;
    let room: jid::BareJid = "sibling@muc.example.com".parse().expect("room");
    let mut submission = fixture.submission(Some("sibling-retry"), "original body");
    let sender = submission.sender.clone();
    let sibling = sender
        .to_bare()
        .with_resource_str("mobile")
        .expect("sibling");
    let actor = state
        .deps
        .protocol
        .room_registry
        .ask(CreateRoom {
            room_jid: room.clone(),
            waddle_id: "sibling".into(),
            channel_id: "sibling".into(),
            config: Default::default(),
        })
        .await
        .expect("room");
    let mut receivers = Vec::new();
    for (resource, nick) in [(&sender, "original"), (&sibling, "mobile")] {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        socket_tests::register_test_connection(&state, resource, tx).await;
        receivers.push(rx);
        actor
            .ask(Join {
                nick: nick.into(),
                real_jid: resource.clone(),
                role: waddle_xmpp::Role::Moderator,
                affiliation: waddle_xmpp::Affiliation::Owner,
            })
            .await
            .expect("join");
    }
    let mut message = submission.plan.sanitized_message.clone();
    message.type_ = MessageType::Groupchat;
    message.to = Some(room.clone().into());
    if subject {
        message.bodies.clear();
        message
            .subjects
            .insert(Lang::new(), "original subject".into());
    }
    submission.target = NormalizedTarget::Bare(room.clone());
    submission.digest_input = DigestInput::from_parsed(
        &message,
        &DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![room.clone()],
            stanza_lang: None,
        },
    )
    .expect("digest");
    submission.plan.sanitized_message = message.clone();
    let mut deps = build_interpret_deps(&state, None);
    deps.inbox_storage = None;
    plan_broadcast(&mut submission, &room, &message, &deps).await;
    submission
        .plan
        .plan
        .sort_by_key(|planned| match &planned.effect {
            Effect::External(effect) => {
                super::super::recorded::single_target(effect) == Some(&sibling)
            }
            _ => false,
        });
    let intent = submission
        .plan
        .intents
        .iter()
        .find(|intent| matches!(intent, IngressEffectIntent::RouteMucGroupchat { .. }))
        .expect("MUC obligation")
        .clone();
    let receipt = receipt_key(&intent).expect("MUC receipt");
    let first = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit");
    let key = first.message_key.expect("key");
    let stalled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let report = STALL_DELIVERY_RESOURCE
        .scope(
            (sibling.clone(), stalled.clone()),
            execute_effects(
                &fixture.uow,
                &fixture.db,
                &first,
                &ImmediateSink,
                &deps,
                Duration::from_secs(1),
            ),
        )
        .await;
    assert!(stalled.load(std::sync::atomic::Ordering::SeqCst));
    assert!(report.receipt_failures.is_empty(), "{report:?}");
    assert!(receivers[0].try_recv().is_ok(), "original sender reflected");
    assert!(
        receivers[1].try_recv().is_err(),
        "sibling remains undelivered"
    );
    assert!(
        !terminalize_if_complete(&fixture.uow, key, DeliveryExecutionContext::Live.into())
            .await
            .expect("pending")
    );
    let mut tx = fixture.uow.begin().await.expect("inspect source");
    assert!(DeliveryProgressRepository::load(&mut tx, key, &receipt)
        .await
        .expect("progress")
        .is_empty());
    let frozen = crate::ingress_uow::CanonicalMessageRepository::load_envelope(&mut tx, key)
        .await
        .expect("source")
        .expect("envelope");
    let frozen_source =
        super::super::room_canonical::source(&frozen, &intent).expect("canonical source");
    if !canonical {
        crate::ingress_uow::CanonicalMessageRepository::record_room_canonical_envelope(
            &mut tx,
            key,
            &crate::ingress_substrate::MessageEnvelope::new(message.clone()),
        )
        .await
        .expect("old real-sender envelope");
    }
    tx.commit().await.expect("source commit");

    submission.sender = sibling.clone();
    message.from = Some(sibling.clone().into());
    plan_broadcast(&mut submission, &room, &message, &deps).await;
    // The production planner exposes the room/nick prototype, including for subjects.
    submission.plan.sanitized_message = submission
        .plan
        .room_canonical_message
        .as_deref()
        .expect("room prototype")
        .clone();
    let retry = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("retry");
    let sibling_effects: Vec<_> = retry
        .external
        .iter()
        .enumerate()
        .filter(|(_, effect)| super::super::recorded::single_target(effect) == Some(&sibling))
        .collect();
    let reflections: Vec<_> = sibling_effects
        .iter()
        .filter(|(index, _)| !retry.external_receipts[*index].contains(&receipt))
        .collect();
    assert_eq!(
        reflections.len(),
        1,
        "current-attempt reflection has no kind-2 ownership"
    );
    assert!(
        !super::super::execute_uow::owns(reflections[0].1, &retry.route_progress),
        "fresh reflection never proves historical delivery"
    );
    let repairs: Vec<_> = sibling_effects
        .iter()
        .filter(|(index, _)| retry.external_receipts[*index].contains(&receipt))
        .collect();
    assert_eq!(
        repairs.len(),
        usize::from(canonical),
        "historical copy needs validated frozen content"
    );
    for (_, effect) in repairs {
        assert!(super::super::execute_uow::owns(
            effect,
            &retry.route_progress
        ));
        let ExternalEffect::Delivery(ExternalDeliveryEffect::RouteToPeer { stanza, .. }) = effect
        else {
            panic!("live historical repair");
        };
        let Stanza::Message(copy) = stanza.as_ref() else {
            panic!("message repair")
        };
        assert_eq!(copy.from, frozen_source.from);
        let IngressEffectIntent::RouteMucGroupchat { route_identity, .. } = &intent else {
            panic!("MUC intent");
        };
        assert!(super::super::receipts::routing::message_identity(
            copy,
            route_identity
        ));
        assert_eq!(copy.bodies, frozen_source.bodies);
        assert_eq!(copy.subjects, frozen_source.subjects);
        assert_eq!(
            waddle_xmpp::xep::xep0421::extract_occupant_id_from_message(copy),
            waddle_xmpp::xep::xep0421::extract_occupant_id_from_message(frozen_source)
        );
    }
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &retry,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert!(report.receipt_failures.is_empty(), "{report:?}");
    let mut delivered = Vec::new();
    while let Ok(outbound) = receivers[1].try_recv() {
        let Stanza::Message(copy) = outbound.stanza else {
            panic!("message delivery")
        };
        delivered.push(copy);
    }
    assert_eq!(
        delivered.len(),
        1 + usize::from(canonical),
        "fresh reflection plus optional frozen repair"
    );
    assert!(
        delivered
            .iter()
            .any(|copy| copy.from == Some(room.with_resource_str("mobile").expect("nick").into())),
        "the retry sender receives its fresh reflection"
    );
    if canonical {
        assert!(
            delivered.iter().any(|copy| copy.from == frozen_source.from
                && copy.bodies == frozen_source.bodies
                && copy.subjects == frozen_source.subjects),
            "historical delivery uses frozen source"
        );
    }
    let mut tx = fixture.uow.begin().await.expect("final progress");
    let completed = DeliveryProgressRepository::load(&mut tx, key, &receipt)
        .await
        .expect("progress");
    assert_eq!(
        completed,
        if canonical { vec![sibling] } else { Vec::new() }
    );
    assert_eq!(
        EffectReceiptRepository::contains(
            &mut tx,
            key,
            receipt.kind,
            &receipt.semantic_identity_hash
        )
        .await
        .expect("aggregate"),
        canonical
    );
    if !canonical {
        let envelope = crate::ingress_uow::CanonicalMessageRepository::load_envelope(&mut tx, key)
            .await
            .expect("load")
            .expect("envelope");
        assert_eq!(
            super::super::room_canonical::source(&envelope, &intent),
            Err(super::super::room_canonical::CanonicalSourceError::MissingCanonicalProvenance)
        );
    }
    tx.commit().await.expect("read commit");
    assert_eq!(
        terminalize_if_complete(&fixture.uow, key, DeliveryExecutionContext::Live.into())
            .await
            .expect("terminal"),
        canonical
    );
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_muc_occupant_progress_sibling_retry_canonical() {
    sibling_retry(IngressFixture::sqlite().await, false, true).await;
}
#[tokio::test]
async fn postgres_muc_occupant_progress_sibling_retry_canonical() {
    if let Some(fixture) = IngressFixture::postgres("sibling_retry_canonical").await {
        sibling_retry(fixture, false, true).await;
    }
}
#[tokio::test]
async fn sqlite_muc_occupant_progress_sibling_retry_old_source() {
    sibling_retry(IngressFixture::sqlite().await, false, false).await;
}
#[tokio::test]
async fn postgres_muc_occupant_progress_sibling_retry_old_source() {
    if let Some(fixture) = IngressFixture::postgres("sibling_retry_old_source").await {
        sibling_retry(fixture, false, false).await;
    }
}
#[tokio::test]
async fn sqlite_muc_occupant_progress_sibling_retry_subject_canonical() {
    sibling_retry(IngressFixture::sqlite().await, true, true).await;
}
#[tokio::test]
async fn postgres_muc_occupant_progress_sibling_retry_subject_canonical() {
    if let Some(fixture) = IngressFixture::postgres("sibling_retry_subject_canonical").await {
        sibling_retry(fixture, true, true).await;
    }
}
#[tokio::test]
async fn sqlite_muc_occupant_progress_sibling_retry_subject_old_source() {
    sibling_retry(IngressFixture::sqlite().await, true, false).await;
}
#[tokio::test]
async fn postgres_muc_occupant_progress_sibling_retry_subject_old_source() {
    if let Some(fixture) = IngressFixture::postgres("sibling_retry_subject_old_source").await {
        sibling_retry(fixture, true, false).await;
    }
}
