//! Saved observation output follows the ordinary room ingress authority.
use super::groupchat_ingress::GroupchatFixture;
use crate::{
    ingress::{
        nested::{NestedContinuation, NestedOutcome},
        test_support::IngressFixture,
    },
    ingress_uow::{
        initialize_room_observations, CanonicalMessageRepository, CapturedRoomSource,
        EffectIntentRepository, RoomObservationRepository,
    },
    server::routes::{
        interpret::{
            effects::{
                room::ExternalRoomEffect, Effect, ExternalEffect, PlanSuppressionPolicy,
                PlannedEffect,
            },
            plan_room_result,
        },
        websocket::interpret_loop::build_interpret_deps,
    },
};
use chrono::Utc;
use waddle_extensions::{
    ConfiguredRoomObserver, ExtensionPayload, ObservationGeneration, PluginId,
    RoomObservationOutcome, RoomObservationResult, RoomObservationScope,
    RoomObservationSubscription, Sha256Digest, XmlAttribute, XmlElement, XmlNode,
};
use waddle_xmpp::{
    ingress::{
        DigestContext, DigestInput, IngressEffectIntent, MessageKey, NormalizedTarget,
        SemanticDigest,
    },
    protocol::{handlers::register_default_message_handlers, StanzaDispatcher, XmppStateMachine},
    Stanza,
};
use waddle_xmpp_core::xep0359::{add_origin_id, add_stanza_id, StanzaId};
use xmpp_parsers::message::{Id, Lang, Message, MessageType};

fn observer(room: &jid::BareJid) -> ConfiguredRoomObserver {
    ConfiguredRoomObserver {
        plugin: PluginId::new("jev-result-fixture").expect("plugin"),
        generation: ObservationGeneration::new(1).expect("generation"),
        identity: Sha256Digest::new("a".repeat(64)).expect("identity"),
        scope: RoomObservationScope::Rooms(vec![room.clone()]),
        max_concurrent: 1,
    }
}

fn subscription(
    configured: &ConfiguredRoomObserver,
    room: &jid::BareJid,
) -> RoomObservationSubscription {
    RoomObservationSubscription {
        plugin: configured.plugin.clone(),
        generation: configured.generation,
        identity: configured.identity.clone(),
        room: room.clone(),
    }
}

fn source_message(room: &jid::BareJid, source_id: &str, origin_id: &str) -> Message {
    let mut message = Message::new(Some(room.clone().into()));
    message.type_ = MessageType::Groupchat;
    message.from = Some(
        room.clone()
            .with_resource_str("romeo")
            .expect("occupant")
            .into(),
    );
    message.id = Some(Id(format!("client-{source_id}")));
    message
        .bodies
        .insert(Lang::new(), "message to score".to_string());
    add_origin_id(&mut message, origin_id);
    add_stanza_id(&mut message, &StanzaId::new(source_id, room.clone().into()));
    message
}

fn score_payload() -> ExtensionPayload {
    let ns = waddle_extensions::PayloadNamespace::new("urn:waddle:safety-scores:1").expect("ns");
    let attr = |local_name: &str, value: &str| XmlAttribute {
        namespace: None,
        local_name: local_name.to_string(),
        value: value.to_string(),
    };
    let score = XmlElement::new(
        ns.clone(),
        "score",
        vec![
            attr("category", "spam"),
            attr("probability", "0.9"),
            attr("taxonomy-version", "1"),
        ],
        vec![],
    )
    .expect("score");
    ExtensionPayload::new(
        ns.clone(),
        XmlElement::new(
            ns,
            "safety-scores",
            vec![attr("model-version", "test-model")],
            vec![XmlNode::Element(score)],
        )
        .expect("root"),
    )
    .expect("payload")
}

async fn saved_publication(
    f: &IngressFixture,
    room: &jid::BareJid,
    source_id: &str,
    origin_id: &str,
) -> crate::ingress_uow::RoomPublication {
    let configured = observer(room);
    let subscription = subscription(&configured, room);
    let source = source_message(room, source_id, origin_id);
    let key = MessageKey::new();
    let sender: jid::BareJid = "romeo@example.com".parse().expect("real sender");
    let intent = IngressEffectIntent::RoomObserver {
        room: room.clone(),
        requester: sender.clone(),
        sender: room.clone().with_resource_str("romeo").expect("occupant"),
        plugin: configured.plugin.clone(),
        generation: configured.generation,
        identity: configured.identity.clone(),
        correction_target: None,
    };
    let mut tx = f.uow.begin().await.expect("source transaction");
    CanonicalMessageRepository::record_message(
        &mut tx,
        key,
        &SemanticDigest::from_storage(1, [7; 32]).expect("digest"),
        None,
    )
    .await
    .expect("canonical source");
    EffectIntentRepository::reconcile(&mut tx, key, std::slice::from_ref(&intent), false)
        .await
        .expect("frozen observer intent");
    RoomObservationRepository::sync_configured(&mut tx, &[configured])
        .await
        .expect("observer config");
    RoomObservationRepository::capture(
        &mut tx,
        CapturedRoomSource {
            key,
            room,
            message: &source,
            sender: &sender,
            intents: &[intent],
            observed_at: Utc::now(),
            correction_target: None,
        },
    )
    .await
    .expect("source capture");
    tx.commit().await.expect("source commit");
    let mut tx = f.uow.begin().await.expect("claim transaction");
    let work =
        RoomObservationRepository::claim(&mut tx, &subscription, Utc::now().timestamp_millis())
            .await
            .expect("claim")
            .expect("work");
    tx.commit().await.expect("claim commit");
    let mut tx = f.uow.begin().await.expect("finish transaction");
    let outcome = RoomObservationOutcome::Completed(RoomObservationResult {
        payloads: vec![score_payload()],
        usage: None,
    });
    assert!(RoomObservationRepository::finish(
        &mut tx,
        &work,
        &outcome,
        Utc::now().timestamp_millis()
    )
    .await
    .expect("finish"));
    tx.commit().await.expect("finish commit");
    let mut tx = f.uow.begin().await.expect("publication transaction");
    let publication = RoomObservationRepository::publication(&mut tx, &subscription)
        .await
        .expect("publication")
        .expect("saved publication");
    tx.commit().await.expect("publication read");
    publication
}

async fn admit_result(
    fixture: &GroupchatFixture,
    publication: crate::ingress_uow::RoomPublication,
) -> NestedOutcome {
    let state = &fixture.adapter.state;
    let deps = build_interpret_deps(state, None);
    let submission = plan_room_result(&deps, publication)
        .await
        .expect("room result plan");
    let continuation = NestedContinuation::new(state.clone(), None, submission.sender.clone());
    state
        .deps
        .protocol
        .ingress
        .try_begin_nested()
        .expect("admission")
        .commit_and_continue(submission, continuation)
        .await
}

async fn result_archive_and_stale_gate(f: IngressFixture) {
    initialize_room_observations(&f.db)
        .await
        .expect("observation schema");
    let mut fixture = GroupchatFixture::new(&f).await;
    let publication =
        saved_publication(&f, &fixture.room, "source-room-stanza", "source-origin").await;
    let source_room_id = publication.source.stanza_id.clone();
    let source_revision_id = publication.source.revision_stanza_id.clone();
    let state = &fixture.adapter.state;
    let deps = build_interpret_deps(state, None);
    let submission = plan_room_result(&deps, publication.clone())
        .await
        .expect("room result plan");
    let apply_to = submission
        .plan
        .sanitized_message
        .payloads
        .iter()
        .find(|element| element.name() == "apply-to")
        .expect("fastening");
    assert_eq!(apply_to.attr("id"), Some("source-origin"));
    let scores = apply_to
        .get_child("safety-scores", "urn:waddle:safety-scores:1")
        .expect("scores");
    assert_eq!(
        scores.attr("target-stanza-id"),
        Some(source_room_id.as_str())
    );
    assert_eq!(
        scores.attr("target-stanza-by"),
        Some(fixture.room.to_string().as_str())
    );
    assert_eq!(
        scores.attr("source-revision-id"),
        Some(source_revision_id.as_str())
    );
    let continuation = NestedContinuation::new(state.clone(), None, submission.sender.clone());
    let outcome = state
        .deps
        .protocol
        .ingress
        .try_begin_nested()
        .expect("admission")
        .commit_and_continue(submission, continuation)
        .await;
    let NestedOutcome::Committed { settlement, .. } = outcome else {
        panic!("saved result must commit")
    };
    let settled = settlement.await.expect("settlement task");
    assert!(settled.terminal.expect("terminal persistence"));
    assert!(settled.rejection.is_none());
    assert_eq!(f.count("mam_messages").await, 1);
    assert_eq!(
        f.count("extension_room_publications WHERE status = 'published'")
            .await,
        1
    );
    assert_eq!(
        fixture
            .drain()
            .iter()
            .filter(
                |s| matches!(s, Stanza::Message(message) if message.type_ == MessageType::Groupchat)
            )
            .count(),
        1
    );

    if let NestedOutcome::Committed { settlement, .. } = admit_result(&fixture, publication).await {
        let settled = settlement.await.expect("replay settlement task");
        assert!(settled.terminal.expect("replay terminal persistence"));
    }
    assert_eq!(f.count("mam_messages").await, 1);
    assert!(fixture.drain().is_empty());

    let stale = saved_publication(&f, &fixture.room, "later-source", "later-origin").await;
    let deps = build_interpret_deps(&fixture.adapter.state, None);
    let submission = plan_room_result(&deps, stale).await.expect("stale plan");
    let mut tx = f.uow.begin().await.expect("retract transaction");
    RoomObservationRepository::retract(
        &mut tx,
        &fixture.room,
        &StanzaId::new("later-source", fixture.room.clone().into()),
    )
    .await
    .expect("retract");
    tx.commit().await.expect("retraction commit");
    let continuation = NestedContinuation::new(
        fixture.adapter.state.clone(),
        None,
        submission.sender.clone(),
    );
    let refused = fixture
        .adapter
        .state
        .deps
        .protocol
        .ingress
        .try_begin_nested()
        .expect("admission")
        .commit_and_continue(submission, continuation)
        .await;
    assert!(matches!(refused, NestedOutcome::Refused(_)));
    assert_eq!(f.count("mam_messages").await, 1);
    assert!(fixture.drain().is_empty());
    assert_eq!(
        f.count("extension_room_publications WHERE status = 'pending'")
            .await,
        0
    );
    assert_eq!(
        f.count("extension_room_publications WHERE status = 'stale'")
            .await,
        1
    );
    fixture.close(f).await;
}

async fn real_client_room_archive_captures_observer_source(f: IngressFixture) {
    initialize_room_observations(&f.db)
        .await
        .expect("observation schema");
    f.uow.enable_room_observations();
    let fixture = GroupchatFixture::new(&f).await;
    let configured = observer(&fixture.room);
    let mut tx = f.uow.begin().await.expect("observer config");
    RoomObservationRepository::sync_configured(&mut tx, &[configured.clone()])
        .await
        .expect("sync observer");
    tx.commit().await.expect("config commit");

    let sender: jid::FullJid = "romeo@example.com/web".parse().expect("joined sender");
    let mut submission = f.submission(Some("client-source-origin"), "real source body");
    submission.sender = sender.clone();
    submission.target = NormalizedTarget::Bare(fixture.room.clone());
    submission.plan.sanitized_message.from = Some(sender.clone().into());
    submission.plan.sanitized_message.to = Some(fixture.room.clone().into());
    submission.plan.sanitized_message.type_ = MessageType::Groupchat;
    submission.digest_input = DigestInput::from_parsed(
        &submission.plan.sanitized_message,
        &DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![fixture.room.clone()],
            stanza_lang: None,
        },
    )
    .expect("source digest");
    let mut dispatcher = StanzaDispatcher::new();
    register_default_message_handlers(&mut dispatcher);
    let mut machine = XmppStateMachine::new("example.com", dispatcher);
    machine.transition_to_ready(sender.clone(), false);
    let deps = build_interpret_deps(&fixture.adapter.state, None);
    submission.plan = crate::server::plan_message_dispatch(
        &mut machine,
        submission.plan.sanitized_message,
        &deps,
    )
    .await;
    assert!(submission.plan.failure.is_none());
    assert!(submission.plan.rejection.is_none());
    let canonical = submission
        .plan
        .room_canonical_message
        .as_deref()
        .expect("canonical room message")
        .clone();
    let room_stanza_id =
        waddle_xmpp_core::xep0359::extract_stanza_id_by(&canonical, &fixture.room.clone().into())
            .expect("canonical room stanza id");
    submission
        .plan
        .intents
        .push(IngressEffectIntent::RoomObserver {
            room: fixture.room.clone(),
            requester: sender.to_bare(),
            sender: fixture
                .room
                .clone()
                .with_resource_str("romeo")
                .expect("room sender"),
            plugin: configured.plugin.clone(),
            generation: configured.generation,
            identity: configured.identity.clone(),
            correction_target: None,
        });
    submission.plan.plan.push(
        PlannedEffect::new(Effect::External(ExternalEffect::Room(
            ExternalRoomEffect::ObserveRoomMessage {
                room: fixture.room.clone(),
                plugin: configured.plugin.clone(),
                message: Box::new(canonical.clone()),
                requester: sender.to_bare(),
                sender: sender.clone(),
                error_request: Box::new(submission.plan.sanitized_message.clone()),
            },
        )))
        .with_suppression(PlanSuppressionPolicy::Always),
    );
    let decision = crate::ingress::commit::commit_submission(&f.uow, &submission, 5)
        .await
        .expect("actual room archive commit");
    assert!(decision.class.advances());
    assert_eq!(f.count("mam_messages").await, 1);
    assert_eq!(f.count("extension_room_sources").await, 1);
    assert_eq!(f.count("extension_room_observation_work").await, 1);
    let mut tx = f.uow.begin().await.expect("durable claim");
    let work = RoomObservationRepository::claim(
        &mut tx,
        &subscription(&configured, &fixture.room),
        Utc::now().timestamp_millis(),
    )
    .await
    .expect("claim")
    .expect("source work");
    assert_eq!(work.source.stanza_id.as_str(), room_stanza_id);
    assert_eq!(
        work.source.origin_id.as_ref().map(|id| id.as_str()),
        Some("client-source-origin")
    );
    assert_eq!(work.body.as_str(), "real source body");
    tx.commit().await.expect("claim commit");
    fixture.close(f).await;
}

#[tokio::test]
async fn real_client_room_archive_captures_observer_source_sqlite() {
    real_client_room_archive_captures_observer_source(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn real_client_room_archive_captures_observer_source_postgres() {
    if let Some(fixture) = IngressFixture::postgres("client_observer_source").await {
        real_client_room_archive_captures_observer_source(fixture).await;
    }
}

#[tokio::test]
async fn saved_room_result_archive_and_stale_gate_sqlite() {
    result_archive_and_stale_gate(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn saved_room_result_archive_and_stale_gate_postgres() {
    if let Some(fixture) = IngressFixture::postgres("room_result_archive").await {
        result_archive_and_stale_gate(fixture).await;
    }
}
