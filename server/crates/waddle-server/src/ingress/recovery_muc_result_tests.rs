//! A saved, pin-free room result must recover only its missing frozen occupant copy.

use super::*;
use crate::ingress_uow::{
    initialize_room_observations, CapturedRoomSource, EffectIntentRepository,
    RoomObservationRepository,
};
use chrono::Utc;
use waddle_extensions::{
    ConfiguredRoomObserver, ExtensionPayload, ObservationGeneration, PluginId,
    RoomObservationOutcome, RoomObservationResult, RoomObservationScope,
    RoomObservationSubscription, Sha256Digest, XmlAttribute, XmlElement, XmlNode,
};
use waddle_xmpp::ingress::SemanticDigest;
use waddle_xmpp_core::xep0359::{add_origin_id, add_stanza_id, StanzaId};
use xmpp_parsers::message::{Id, Lang, Message, MessageType};

fn score_payload() -> ExtensionPayload {
    let namespace =
        waddle_extensions::PayloadNamespace::new("urn:waddle:safety-scores:1").expect("namespace");
    let attribute = |local_name: &str, value: &str| XmlAttribute {
        namespace: None,
        local_name: local_name.to_string(),
        value: value.to_string(),
    };
    let score = XmlElement::new(
        namespace.clone(),
        "score",
        vec![
            attribute("category", "safety:spam"),
            attribute("probability", "0.9"),
            attribute("taxonomy-version", "1"),
        ],
        vec![],
    )
    .expect("score");
    ExtensionPayload::new(
        namespace.clone(),
        XmlElement::new(
            namespace,
            "safety-scores",
            vec![attribute("model-version", "test-model")],
            vec![XmlNode::Element(score)],
        )
        .expect("scores"),
    )
    .expect("payload")
}

async fn saved_result(
    fixture: &IngressFixture,
    room: &jid::BareJid,
) -> crate::ingress_uow::RoomPublication {
    initialize_room_observations(&fixture.db)
        .await
        .expect("observation schema");
    let observer = ConfiguredRoomObserver {
        plugin: PluginId::new("recovery-result-test").expect("plugin"),
        generation: ObservationGeneration::new(1).expect("generation"),
        identity: Sha256Digest::new("a".repeat(64)).expect("identity"),
        scope: RoomObservationScope::Rooms(vec![room.clone()]),
        max_concurrent: 1,
    };
    let subscription = RoomObservationSubscription {
        plugin: observer.plugin.clone(),
        generation: observer.generation,
        identity: observer.identity.clone(),
        room: room.clone(),
    };
    let sender: jid::BareJid = "romeo@example.com".parse().expect("sender");
    let mut message = Message::new(Some(room.clone().into()));
    message.type_ = MessageType::Groupchat;
    message.from = Some(room.with_resource_str("romeo").expect("nick").into());
    message.id = Some(Id("source-wire".to_string()));
    message
        .bodies
        .insert(Lang::new(), "original source".to_string());
    add_origin_id(&mut message, "source-origin");
    add_stanza_id(
        &mut message,
        &StanzaId::new("source-room-stanza", room.clone().into()),
    );
    let source_key = MessageKey::new();
    let intent = IngressEffectIntent::RoomObserver {
        room: room.clone(),
        requester: sender.clone(),
        sender: room.with_resource_str("romeo").expect("nick"),
        plugin: observer.plugin.clone(),
        generation: observer.generation,
        identity: observer.identity.clone(),
        correction_target: None,
    };
    let mut tx = fixture.uow.begin().await.expect("source transaction");
    CanonicalMessageRepository::record_message(
        &mut tx,
        source_key,
        &SemanticDigest::from_storage(1, [7; 32]).expect("digest"),
        None,
    )
    .await
    .expect("canonical source");
    EffectIntentRepository::reconcile(&mut tx, source_key, std::slice::from_ref(&intent), false)
        .await
        .expect("observer intent");
    RoomObservationRepository::sync_configured(&mut tx, std::slice::from_ref(&observer))
        .await
        .expect("observer config");
    RoomObservationRepository::capture(
        &mut tx,
        CapturedRoomSource {
            key: source_key,
            room,
            message: &message,
            sender: &sender,
            intents: &[intent],
            observed_at: Utc::now(),
            correction_target: None,
        },
    )
    .await
    .expect("source capture");
    tx.commit().await.expect("source commit");

    let mut tx = fixture.uow.begin().await.expect("claim transaction");
    let work =
        RoomObservationRepository::claim(&mut tx, &subscription, Utc::now().timestamp_millis())
            .await
            .expect("claim")
            .expect("source work");
    tx.commit().await.expect("claim commit");
    let mut tx = fixture.uow.begin().await.expect("finish transaction");
    assert!(RoomObservationRepository::finish(
        &mut tx,
        &work,
        &RoomObservationOutcome::Completed(RoomObservationResult {
            payloads: vec![score_payload()],
            usage: None,
        }),
        Utc::now().timestamp_millis(),
    )
    .await
    .expect("finish"));
    tx.commit().await.expect("finish commit");
    let mut tx = fixture.uow.begin().await.expect("publication transaction");
    let publication = RoomObservationRepository::publication(&mut tx, &subscription)
        .await
        .expect("publication")
        .expect("saved publication");
    tx.commit().await.expect("publication read");
    publication
}

async fn partial_result_recovers(fixture: IngressFixture) {
    let _metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let sm = persistent_sm(&fixture).await;
    let web: jid::FullJid = "web@example.com/browser".parse().expect("web occupant");
    let ios: jid::FullJid = "ios@example.com/phone".parse().expect("iOS occupant");
    for occupant in [&web, &ios] {
        store_detached(&sm, occupant).await;
    }
    let state = state_for(&fixture, sm.clone()).await;
    // Seat both recipients in a real room before planning the room-authored result.
    let room_setup = planned_room(&fixture, &state, Case::Lost, &[web.clone(), ios.clone()]).await;
    let room: jid::BareJid = "recovery@muc.example.com".parse().expect("room");
    // The setup helper also seats its client sender. A room-authored result has
    // only the two intended recipients in its frozen audience.
    use waddle_xmpp::muc::room_actor::{
        LeaveAttemptId, LeaveByRealJid, LeaveOrigin, LeaveSessionSelector,
    };
    let actor = state
        .deps
        .protocol
        .room_registry
        .ask(waddle_xmpp::muc::room_registry_actor::GetRoom {
            room_jid: room.clone(),
        })
        .await
        .expect("room lookup")
        .expect("room actor");
    actor
        .ask(LeaveByRealJid {
            sender_jid: room_setup.sender,
            cause: waddle_xmpp::muc::durable::OccupancyLeaveCause::Disconnect,
            session: LeaveSessionSelector::Any,
            attempt: LeaveAttemptId::generate(),
            origin: LeaveOrigin::Fresh,
        })
        .await
        .expect("setup sender leaves");
    let publication = saved_result(&fixture, &room).await;
    let deps = build_interpret_deps(&state, None);
    let submission = crate::server::routes::interpret::plan_room_result(&deps, publication)
        .await
        .expect("room result plan");
    assert!(!submission
        .plan
        .intents
        .iter()
        .any(|intent| matches!(intent, IngressEffectIntent::Pin { .. })));
    let route = submission
        .plan
        .intents
        .iter()
        .find(|intent| matches!(intent, IngressEffectIntent::RouteMucSystemBroadcast { .. }))
        .expect("system route")
        .clone();
    let IngressEffectIntent::RouteMucSystemBroadcast { occupants, .. } = &route else {
        unreachable!("selected system route")
    };
    let mut audience = occupants.clone();
    audience.sort();
    let mut expected_audience = vec![web.clone(), ios.clone()];
    expected_audience.sort();
    assert_eq!(audience, expected_audience, "frozen two-recipient audience");
    let archive = submission
        .plan
        .intents
        .iter()
        .find(|intent| matches!(intent, IngressEffectIntent::SystemMessageArchive { .. }))
        .expect("system archive")
        .clone();
    let route_receipt = receipt_key(&route).expect("route receipt");
    let archive_receipt = receipt_key(&archive).expect("archive receipt");
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("result archive commit");
    let key = decision.message_key.expect("result key");
    let mut tx = fixture.uow.begin().await.expect("inspect archive");
    assert!(EffectReceiptRepository::contains(
        &mut tx,
        key,
        archive_receipt.kind,
        &archive_receipt.semantic_identity_hash,
    )
    .await
    .expect("archive receipt present"));
    tx.commit().await.expect("inspection commit");

    // Simulate the process stopping after the web copy but before the iOS copy.
    assert!(
        decision.external.iter().any(|effect| {
            matches!(effect,
            ExternalEffect::Delivery(ExternalDeliveryEffect::QueueDetached { resources, .. })
                if resources == std::slice::from_ref(&web))
        }),
        "web delivery"
    );
    let ios_index = decision
        .external
        .iter()
        .position(|effect| {
            matches!(effect,
            ExternalEffect::Delivery(ExternalDeliveryEffect::QueueDetached { resources, .. })
                if resources == std::slice::from_ref(&ios))
        })
        .expect("iOS delivery");
    let mut before_gap = decision.clone();
    let indices: Vec<_> = (0..decision.external.len())
        .filter(|index| *index != ios_index)
        .collect();
    before_gap.external = indices
        .iter()
        .map(|index| decision.external[*index].clone())
        .collect();
    before_gap.external_dependencies = indices
        .iter()
        .map(|index| decision.external_dependencies[*index].clone())
        .collect();
    before_gap.external_receipts = indices
        .iter()
        .map(|index| decision.external_receipts[*index].clone())
        .collect();
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &before_gap,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert!(
        report.receipt_failures.is_empty(),
        "web delivery: {report:?}"
    );
    assert_eq!(append_count(&sm, &web).await, 1);
    assert_eq!(append_count(&sm, &ios).await, 0);
    let mut tx = fixture.uow.begin().await.expect("inspect partial route");
    assert!(!EffectReceiptRepository::contains(
        &mut tx,
        key,
        route_receipt.kind,
        &route_receipt.semantic_identity_hash,
    )
    .await
    .expect("route pending"));
    assert_eq!(
        DeliveryProgressRepository::load(&mut tx, key, &route_receipt)
            .await
            .expect("recipient progress"),
        vec![web.clone()],
    );
    tx.commit().await.expect("inspection commit");

    let environment: Arc<dyn RecoveryEnvironment> = Arc::new(StateEnvironment(state));
    let cursor = MaintenanceCursor::default();
    assert_eq!(
        pass(&fixture, &environment, &cursor).await,
        MaintenanceOutcome::Complete
    );
    assert!(
        crate::ingress::recovery_executor::attempt_count(key) > 0,
        "result entered maintenance"
    );
    assert_eq!(append_count(&sm, &web).await, 1, "web copy is not repeated");
    assert_eq!(
        append_count(&sm, &ios).await,
        1,
        "missing iOS copy is recovered"
    );
    assert_recovered(&fixture, key, 2).await;
    let mut tx = fixture.uow.begin().await.expect("inspect completed route");
    assert_eq!(
        DeliveryProgressRepository::load(&mut tx, key, &route_receipt)
            .await
            .expect("completed progress"),
        vec![ios.clone(), web.clone()],
    );
    tx.commit().await.expect("inspection commit");
    assert_eq!(
        fixture.count("mam_messages").await,
        1,
        "recovery does not rearchive"
    );
    assert_eq!(
        pass(&fixture, &environment, &cursor).await,
        MaintenanceOutcome::Complete
    );
    assert_eq!(append_count(&sm, &web).await, 1);
    assert_eq!(append_count(&sm, &ios).await, 1);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_partial_room_result_recovers_missing_occupant() {
    partial_result_recovers(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_partial_room_result_recovers_missing_occupant() {
    if let Some(fixture) = IngressFixture::postgres("partial_room_result_recovery").await {
        partial_result_recovers(fixture).await;
    }
}
