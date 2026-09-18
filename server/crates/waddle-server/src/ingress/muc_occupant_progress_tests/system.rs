//! Pin system broadcasts freeze the archived payload for each pending occupant.
use super::*;
use crate::ingress::execute_uow::STALL_DELIVERY_RESOURCE;
use crate::ingress_substrate::MessageEnvelope;
use crate::ingress_uow::EffectIntentRepository;
use crate::server::routes::interpret::DeliveryExecutionContext;
use std::sync::{atomic::AtomicBool, Arc};
use waddle_xmpp::{
    mam::{MamStorage, SqlxMamStorage},
    muc::pin::PinChangeRequest,
};
use waddle_xmpp_core::xep0359::StanzaId;

async fn partial_pin_broadcast(fixture: IngressFixture) {
    let state = socket_tests::create_test_websocket_state().await;
    let room: jid::BareJid = "pins@muc.example.com".parse().expect("room");
    let a: jid::FullJid = "alice@example.com/phone".parse().expect("A");
    let b: jid::FullJid = "ben@example.com/phone".parse().expect("B");
    let actor = state
        .deps
        .protocol
        .room_registry
        .ask(CreateRoom {
            room_jid: room.clone(),
            waddle_id: "pin-progress".into(),
            channel_id: "pin-progress".into(),
            config: Default::default(),
        })
        .await
        .expect("room actor");
    let mut receivers = Vec::new();
    for (resource, nick) in [(&a, "alice"), (&b, "ben")] {
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
    let mam: Arc<dyn MamStorage> = Arc::new(
        SqlxMamStorage::open(fixture.db.database_url())
            .await
            .expect("MAM"),
    );
    let target_id = StanzaId::new("pin-target", room.clone().into());
    let mut target = waddle_xmpp::mam::ArchivedMessage::for_test(
        room.with_resource_str("romeo").expect("nick").into(),
        room.clone().into(),
    );
    target.id = target_id.id.clone();
    target.stanza_id = Some(target_id.clone());
    target.body = Some("target".into());
    target.message_type = xmpp_parsers::message::MessageType::Groupchat;
    mam.store_message(&room, &target)
        .await
        .expect("target archive");
    let mut submission = fixture.submission(Some("partial-pin"), "triggering command");
    submission.target = NormalizedTarget::Bare(room.clone());
    submission.plan.sanitized_message.to = Some(room.clone().into());
    submission.plan.sanitized_message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    submission.digest_input = DigestInput::from_parsed(
        &submission.plan.sanitized_message,
        &DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![room.clone()],
            stanza_lang: None,
        },
    )
    .expect("digest");
    let sink = PlanSink::new();
    let capture = IngressEffectCapture::new();
    let mut deps = build_interpret_deps(&state, None);
    deps.inbox_storage = None;
    deps.mam_storage = Some(&mam);
    deps.effects = &sink;
    deps.ingress_effect_capture = Some(capture.clone());
    interpret(
        vec![OutboundEvent::ApplyPinChange {
            room: room.clone(),
            request: PinChangeRequest::Pin {
                target_stanza_id: target_id,
                pinner_jid: submission.sender.to_bare(),
                pinner_nick: "romeo".into(),
                pinned_at: chrono::Utc::now(),
            },
        }],
        &deps,
    )
    .await;
    submission.plan.room_canonical_message = sink.room_canonical_message();
    let (plan, execution) = sink.take();
    submission.plan.plan = plan;
    // Snapshot occupants are a HashMap: make the fault occur after A while
    // retaining the real planner's effects and their prerequisite ordering.
    submission.plan.plan.sort_by_key(|planned| matches!(
        &planned.effect,
        effects::Effect::External(ExternalEffect::Delivery(ExternalDeliveryEffect::RouteToPeer { jid, .. })) if jid == &b
    ));
    submission.plan.room_execution = execution;
    submission.plan.intents = capture.snapshot().intents;
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit pin");
    let key = decision.message_key.expect("key");
    let mut tx = fixture.uow.begin().await.expect("inspect frozen source");
    let intents = EffectIntentRepository::load(&mut tx, key)
        .await
        .expect("intents");
    tx.commit().await.expect("read commit");
    let intent = intents
        .iter()
        .find(|intent| matches!(intent, IngressEffectIntent::RouteMucSystemBroadcast { .. }))
        .expect("system obligation");
    let receipt = receipt_key(intent).expect("MUC receipt");
    let trigger = MessageEnvelope::new(submission.plan.sanitized_message.clone());
    let frozen =
        crate::ingress::room_canonical::source(&trigger, intent).expect("frozen system source");
    assert_eq!(frozen.from, Some(room.clone().into()));
    let archive = decision
        .external
        .iter()
        .find_map(|effect| match effect {
            ExternalEffect::Room(effects::room::ExternalRoomEffect::ArchiveAfterPin {
                message,
                ..
            }) => Some(message),
            _ => None,
        })
        .expect("pin archive");
    let archived_message = waddle_xmpp::parser::message_from_string(
        archive.stanza_xml.as_deref().expect("archived XML"),
    )
    .expect("typed archived message");
    assert_eq!(
        frozen, &archived_message,
        "frozen source is the exact archived system payload"
    );
    let copy = crate::ingress::room_canonical::occupant_copy_message(frozen, &b, &intents);
    assert_eq!(copy.from, Some(room.clone().into()));
    assert_eq!(copy.to, Some(b.clone().into()));
    assert_eq!(copy.bodies, frozen.bodies);
    deps.effects = &ImmediateSink;
    let entered = Arc::new(AtomicBool::new(false));
    let report = STALL_DELIVERY_RESOURCE
        .scope(
            (b.clone(), entered.clone()),
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
    assert!(
        entered.load(std::sync::atomic::Ordering::SeqCst),
        "B reached progress arm: {report:?}"
    );
    assert!(
        receivers[0].try_recv().is_ok(),
        "A received pin system copy"
    );
    assert!(receivers[1].try_recv().is_err(), "B remains undelivered");
    let mut tx = fixture.uow.begin().await.expect("inspect progress");
    assert_eq!(
        DeliveryProgressRepository::load(&mut tx, key, &receipt)
            .await
            .expect("progress"),
        vec![a]
    );
    assert!(!EffectReceiptRepository::contains(
        &mut tx,
        key,
        receipt.kind,
        &receipt.semantic_identity_hash
    )
    .await
    .expect("aggregate"));
    tx.commit().await.expect("read commit");
    assert!(
        !terminalize_if_complete(&fixture.uow, key, DeliveryExecutionContext::Live.into())
            .await
            .expect("pending")
    );
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_muc_occupant_progress_partial_pin_frozen_payload() {
    partial_pin_broadcast(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_muc_occupant_progress_partial_pin_frozen_payload() {
    if let Some(fixture) = IngressFixture::postgres("muc_pin_progress").await {
        partial_pin_broadcast(fixture).await;
    }
}
