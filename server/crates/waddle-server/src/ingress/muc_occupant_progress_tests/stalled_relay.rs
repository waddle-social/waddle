use super::*;
use crate::server::routes::interpret::DeliveryExecutionContext;
use crate::server::routes::interpret::{
    ControlledMucRelay, FullJidDeliveryOutcome, CONTROLLED_MUC_RELAY,
};
use std::sync::{Arc, Mutex};

#[tokio::test]
async fn sqlite_stalled_remote_first_preserves_independent_local_muc_progress() {
    let fixture = IngressFixture::sqlite().await;
    let state = socket_tests::create_test_websocket_state().await;
    let room: jid::BareJid = "stalled@muc.example.com".parse().unwrap();
    let local: jid::FullJid = "alice@example.com/phone".parse().unwrap();
    let remote: jid::FullJid = "juliet@example.com/phone".parse().unwrap();
    let mut submission = fixture.submission(Some("stalled-relay-progress"), "room content");
    let actor = state
        .deps
        .protocol
        .room_registry
        .ask(CreateRoom {
            room_jid: room.clone(),
            waddle_id: "stalled".into(),
            channel_id: "stalled".into(),
            config: Default::default(),
        })
        .await
        .expect("room");
    let mut receivers = Vec::new();
    for (resource, nick) in [
        (&local, "alice"),
        (&remote, "juliet"),
        (&submission.sender, "romeo"),
    ] {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        socket_tests::register_test_connection(&state, resource, tx).await;
        receivers.push(rx);
        actor
            .ask(Join {
                nick: nick.into(),
                real_jid: resource.clone(),
                role: waddle_xmpp::Role::Participant,
                affiliation: waddle_xmpp::Affiliation::Member,
            })
            .await
            .expect("join");
    }
    let mut message = submission.plan.sanitized_message.clone();
    message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    message.to = Some(room.clone().into());
    waddle_xmpp::xep::xep0334::add_hint(&mut message, waddle_xmpp::xep::xep0334::Hint::NoStore);
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
    let receipt = submission
        .plan
        .intents
        .iter()
        .find(|intent| matches!(intent, IngressEffectIntent::RouteMucGroupchat { .. }))
        .map(receipt_key)
        .expect("MUC intent")
        .expect("MUC receipt");
    super::relay::select_remote_copy(&mut submission, &remote);
    // Room fanout iteration can put the remote copy first. Reorder entire planned
    // effects before commit so their real policies and dependency metadata survive.
    submission.plan.plan.sort_by_key(|planned| {
        !matches!(
            &planned.effect,
            crate::server::routes::interpret::effects::Effect::External(
                ExternalEffect::Delivery(ExternalDeliveryEffect::RelayFullJid { target, .. })
            ) if target == &remote
        )
    });
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit");
    let key = decision.message_key.expect("message key");
    assert!(matches!(
        decision.external.first(),
        Some(ExternalEffect::Delivery(ExternalDeliveryEffect::RelayFullJid { target, .. }))
            if target == &remote
    ));
    let targets = Arc::new(Mutex::new(Vec::new()));
    let report = CONTROLLED_MUC_RELAY
        .scope(
            (ControlledMucRelay::Pending, targets.clone()),
            execute_effects(
                &fixture.uow,
                &fixture.db,
                &decision,
                &ImmediateSink,
                &deps,
                Duration::from_secs(1),
            ),
        )
        .await;
    assert!(report.receipt_failures.is_empty(), "{report:?}");
    assert_eq!(*targets.lock().unwrap(), vec![remote.clone()]);
    assert!(
        receivers[0].try_recv().is_ok(),
        "independent local occupant must receive its copy even when remote relay stalls first"
    );
    assert!(receivers[1].try_recv().is_err(), "remote remains pending");
    let mut tx = fixture.uow.begin().await.expect("inspect progress");
    assert_eq!(
        DeliveryProgressRepository::load(&mut tx, key, &receipt)
            .await
            .unwrap(),
        vec![local.clone()],
        "successful local delivery must have durable per-resource progress"
    );
    assert!(
        !EffectReceiptRepository::contains(
            &mut tx,
            key,
            receipt.kind,
            &receipt.semantic_identity_hash
        )
        .await
        .unwrap(),
        "stalled remote prevents aggregate completion"
    );
    tx.commit().await.unwrap();
    assert!(
        !terminalize_if_complete(&fixture.uow, key, DeliveryExecutionContext::Live.into())
            .await
            .unwrap()
    );

    let retry = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("retry");
    assert_eq!(retry.message_key, Some(key));
    assert!(!retry.external.iter().any(|effect| matches!(effect,
        ExternalEffect::Delivery(ExternalDeliveryEffect::RouteToPeer { jid, .. }) if jid == &local
    )), "durably completed local occupant must be filtered on retry");
    targets.lock().unwrap().clear();
    let report = CONTROLLED_MUC_RELAY
        .scope(
            (
                ControlledMucRelay::Outcome(Some(FullJidDeliveryOutcome::Delivered)),
                targets.clone(),
            ),
            execute_effects(
                &fixture.uow,
                &fixture.db,
                &retry,
                &ImmediateSink,
                &deps,
                Duration::from_secs(5),
            ),
        )
        .await;
    assert!(report.receipt_failures.is_empty(), "{report:?}");
    assert_eq!(*targets.lock().unwrap(), vec![remote.clone()]);
    assert!(
        receivers[0].try_recv().is_err(),
        "retry must not duplicate local delivery"
    );
    let mut tx = fixture.uow.begin().await.unwrap();
    assert_eq!(
        DeliveryProgressRepository::load(&mut tx, key, &receipt)
            .await
            .unwrap(),
        vec![local, remote]
    );
    assert!(EffectReceiptRepository::contains(
        &mut tx,
        key,
        receipt.kind,
        &receipt.semantic_identity_hash
    )
    .await
    .unwrap());
    tx.commit().await.unwrap();
    assert!(
        terminalize_if_complete(&fixture.uow, key, DeliveryExecutionContext::Live.into())
            .await
            .unwrap()
    );
    fixture.close().await;
}
