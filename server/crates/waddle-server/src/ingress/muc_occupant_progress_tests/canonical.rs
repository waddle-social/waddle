use super::*;
use crate::ingress_uow::CanonicalMessageRepository;
use crate::server::routes::interpret::effects::Effect;
use crate::server::routes::interpret::DeliveryExecutionContext;

async fn canonical_owner(fixture: IngressFixture, relayed: bool, available: bool) {
    #[cfg(feature = "clustering")]
    let mut fixture = fixture;
    let state = socket_tests::create_test_websocket_state().await;
    let room: jid::BareJid = "canonical@muc.example.com".parse().expect("room");
    let mut submission = fixture.submission(Some("canonical-owner"), "frozen content");
    let actor = state
        .deps
        .protocol
        .room_registry
        .ask(CreateRoom {
            room_jid: room.clone(),
            waddle_id: "canonical".into(),
            channel_id: "canonical".into(),
            config: Default::default(),
        })
        .await
        .expect("room");
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    if available {
        socket_tests::register_test_connection(&state, &submission.sender, tx).await;
    }
    actor
        .ask(Join {
            nick: "original-nick".into(),
            real_jid: submission.sender.clone(),
            role: waddle_xmpp::Role::Participant,
            affiliation: waddle_xmpp::Affiliation::Member,
        })
        .await
        .expect("join");
    if !available {
        actor
            .ask(Join {
                nick: "unavailable".into(),
                real_jid: "juliet@example.com/unavailable".parse().expect("occupant"),
                role: waddle_xmpp::Role::Participant,
                affiliation: waddle_xmpp::Affiliation::Member,
            })
            .await
            .expect("join unavailable occupant");
    }
    let mut message = submission.plan.sanitized_message.clone();
    message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    message.to = Some(room.clone().into());
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
    submission.plan.intents.clear();
    submission.plan.plan.clear();
    if relayed {
        let relay_target =
            waddle_xmpp::ingress::RelayTargetIdentity::owner_node("room-owner", "owner-epoch");
        submission
            .plan
            .intents
            .push(IngressEffectIntent::DispatchToRoomRemote {
                room: room.clone(),
                relay_target: relay_target.clone(),
            });
        submission.plan.room_execution = effects::RoomExecutionPath::Remote {
            room: room.clone(),
            relay_target,
        };
    }
    let origin = if relayed {
        Some(
            commit_submission(&fixture.uow, &submission, 1)
                .await
                .expect("origin commit"),
        )
    } else {
        None
    };
    let sink = PlanSink::new();
    let capture = IngressEffectCapture::new();
    let mut deps = build_interpret_deps(&state, None);
    deps.inbox_storage = None;
    deps.effects = &sink;
    deps.ingress_effect_capture = Some(capture.clone());
    interpret(
        vec![OutboundEvent::DispatchToRoom {
            room: room.clone(),
            message: Box::new(message),
        }],
        &deps,
    )
    .await;
    submission.plan.room_canonical_message = sink.room_canonical_message();
    let (plan, execution) = sink.take();
    submission.plan.plan = plan;
    submission.plan.room_execution = execution;
    submission.plan.intents = capture.snapshot().intents;
    assert!(!submission
        .plan
        .intents
        .iter()
        .any(|i| matches!(i, IngressEffectIntent::RoomObserver { .. })));
    if let Some(origin) = origin {
        #[cfg(feature = "clustering")]
        let room_fence = fixture.room_fence(&room).await;
        #[cfg(feature = "clustering")]
        if let effects::RoomExecutionPath::Local { fence, .. } = &mut submission.plan.room_execution
        {
            *fence = effects::room::RoomFenceRequirement::Guarded(room_fence.clone());
        }
        #[cfg(feature = "clustering")]
        for planned in &mut submission.plan.plan {
            if let Effect::Durable(effects::DurableEffect::Room(
                effects::room::DurableRoomEffect::ArchiveGroupchat { fence, .. },
            )) = &mut planned.effect
            {
                *fence = effects::room::RoomFenceRequirement::Guarded(room_fence.clone());
            }
        }
        submission.identity = IngressStreamIdentity::Relayed {
            canonical: IngressCanonicalRef {
                message_key: origin.message_key.expect("key"),
                sender_bare: submission.sender.to_bare(),
                origin_id: submission.digest_input.origin().cloned(),
            },
            room: room.clone(),
            #[cfg(feature = "clustering")]
            room_fence,
        };
    }
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("owner commit");
    let key = decision.message_key.expect("key");
    let mut tx = fixture.uow.begin().await.expect("inspect");
    let envelope = CanonicalMessageRepository::load_envelope(&mut tx, key)
        .await
        .expect("load")
        .expect("envelope");
    tx.commit().await.expect("read commit");
    assert_eq!(
        envelope.message().to,
        None,
        "persist exact reflector working message"
    );
    assert_eq!(
        envelope.message().from,
        Some(
            room.with_resource_str("original-nick")
                .expect("occupant")
                .into()
        ),
        "zero-plugin owner must freeze room canonical sender"
    );
    assert!(
        waddle_xmpp::xep::xep0421::extract_occupant_id_from_message(envelope.message()).is_some()
    );
    assert!(waddle_xmpp::xep::extract_stanza_ids(envelope.message())
        .iter()
        .any(|id| id.by == room));
    let muc_intent = submission
        .plan
        .intents
        .iter()
        .find(|intent| matches!(intent, IngressEffectIntent::RouteMucGroupchat { .. }))
        .expect("MUC obligation");
    assert!(
        crate::ingress::room_canonical::source(&envelope, muc_intent).is_ok(),
        "unavailable fanout still freezes recoverable provenance"
    );
    let mut available_receiver = if available {
        None
    } else {
        let occupant = "juliet@example.com/unavailable".parse().expect("occupant");
        let (tx, receiver) = tokio::sync::mpsc::channel(16);
        socket_tests::register_test_connection(&state, &occupant, tx).await;
        let request = submission.plan.sanitized_message.clone();
        plan_broadcast(&mut submission, &room, &request, &deps).await;
        Some(receiver)
    };
    for planned in &mut submission.plan.plan {
        if let Effect::External(ExternalEffect::Delivery(ExternalDeliveryEffect::RouteToPeer {
            stanza,
            ..
        })) = &mut planned.effect
        {
            if let waddle_xmpp::Stanza::Message(copy) = stanza.as_mut() {
                copy.from = Some(room.with_resource_str("changed-nick").expect("nick").into());
                copy.bodies
                    .insert(Default::default(), "changed retry content".into());
            }
        }
    }
    let retry = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("owner retry");
    if let Some(receiver) = &mut available_receiver {
        let copy = retry
            .external
            .iter()
            .find_map(|effect| match effect {
                ExternalEffect::Delivery(ExternalDeliveryEffect::RouteToPeer {
                    stanza, ..
                }) => match stanza.as_ref() {
                    waddle_xmpp::Stanza::Message(message) => Some(message),
                    _ => None,
                },
                _ => None,
            })
            .expect("newly available frozen occupant is restored through the provenance gate");
        let mut expected = envelope.message().clone();
        expected.to = Some("juliet@example.com/unavailable".parse().expect("occupant"));
        assert_eq!(
            waddle_xmpp::xep::extract_stanza_ids(copy),
            waddle_xmpp::xep::extract_stanza_ids(&expected),
            "retry preserves the frozen stanza identities"
        );
        let mut restored = copy.clone();
        for message in [&mut restored, &mut expected] {
            message
                .payloads
                .retain(|payload| !waddle_xmpp_core::xep0359::is_stanza_id_element(payload));
        }
        assert_eq!(
            restored, expected,
            "retry retains the other frozen room fields"
        );
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
        assert!(
            receiver.try_recv().is_ok(),
            "retry delivers to the newly available occupant"
        );
        assert!(
            !terminalize_if_complete(&fixture.uow, key, DeliveryExecutionContext::Live.into())
                .await
                .expect("original reflection remains owed to its unavailable sender")
        );
    }
    let mut tx = fixture.uow.begin().await.expect("inspect retry");
    assert_eq!(
        CanonicalMessageRepository::load_envelope(&mut tx, key)
            .await
            .expect("frozen envelope"),
        Some(envelope),
        "retries cannot replace canonical room content"
    );
    tx.commit().await.expect("read commit");
    fixture.close().await;
}

#[cfg(not(feature = "clustering"))]
#[tokio::test]
async fn sqlite_muc_canonical_owner_zero_plugins() {
    canonical_owner(IngressFixture::sqlite().await, true, true).await;
}
#[tokio::test]
async fn postgres_muc_canonical_owner_zero_plugins() {
    if let Some(fixture) = IngressFixture::postgres("muc_canonical_owner").await {
        canonical_owner(fixture, true, true).await;
    }
}
#[tokio::test]
async fn sqlite_muc_canonical_local_zero_plugins() {
    canonical_owner(IngressFixture::sqlite().await, false, true).await;
}
#[tokio::test]
async fn postgres_muc_canonical_local_zero_plugins() {
    if let Some(fixture) = IngressFixture::postgres("muc_canonical_local").await {
        canonical_owner(fixture, false, true).await;
    }
}

async fn old_sender_envelope_stays_pending(fixture: IngressFixture) {
    use crate::ingress::room_canonical::{source, CanonicalSourceError};
    use waddle_xmpp::ingress::{EffectMessageIdentity, EntityGeneration};
    let room: jid::BareJid = "old@muc.example.com".parse().expect("room");
    let occupant: jid::FullJid = "juliet@example.com/laptop".parse().expect("occupant");
    let stamp = waddle_xmpp_core::xep0359::StanzaId::new("old-room-id", room.clone().into());
    let mut submission = fixture.submission(Some("old-room-provenance"), "original content");
    submission.target = NormalizedTarget::Bare(room.clone());
    submission.plan.sanitized_message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    submission.plan.sanitized_message.to = Some(room.clone().into());
    submission.digest_input = DigestInput::from_parsed(
        &submission.plan.sanitized_message,
        &DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![room.clone()],
            stanza_lang: None,
        },
    )
    .expect("digest");
    let intent = IngressEffectIntent::RouteMucGroupchat {
        room: room.clone(),
        occupants: vec![occupant.clone(), submission.sender.clone()],
        reflection: submission.sender.clone(),
        room_generation: EntityGeneration::INITIAL,
        route_identity: EffectMessageIdentity::StanzaId(stamp.clone()),
    };
    submission.plan.intents = vec![intent.clone()];
    // Model a pre-fix row: it retained room authority but only the origin envelope.
    submission.plan.plan.clear();
    let accepted = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("old row");
    let key = accepted.message_key.expect("key");
    let mut tx = fixture.uow.begin().await.expect("read old row");
    let envelope = CanonicalMessageRepository::load_envelope(&mut tx, key)
        .await
        .expect("load")
        .expect("envelope");
    tx.commit().await.expect("read commit");
    assert_eq!(
        source(&envelope, &intent),
        Err(CanonicalSourceError::MissingCanonicalProvenance)
    );

    let mut fresh = submission.plan.sanitized_message.clone();
    fresh.from = Some(room.with_resource_str("new-nick").expect("nick").into());
    fresh.to = Some(occupant.clone().into());
    fresh
        .bodies
        .insert(Default::default(), "new retry content".into());
    waddle_xmpp_core::xep0359::add_stanza_id(&mut fresh, &stamp);
    waddle_xmpp::xep::xep0421::set_occupant_id_on_message(
        &mut fresh,
        &waddle_xmpp::xep::xep0421::OccupantId("fresh-occupant".into()),
    );
    submission.plan.plan.push(
        effects::PlannedEffect::new(Effect::External(ExternalEffect::Delivery(
            ExternalDeliveryEffect::QueueDetached {
                route_identity: None,
                call_setup: None,
                bare: occupant.to_bare(),
                resources: vec![occupant],
                stanza: Box::new(waddle_xmpp::Stanza::Message(fresh)),
            },
        )))
        .with_suppression(effects::PlanSuppressionPolicy::SenderOnly),
    );
    let retry = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("retry old row");
    assert!(
        retry.external.is_empty(),
        "missing canonical provenance cannot borrow today's sender or content"
    );
    assert!(!crate::ingress::execute::terminalize_if_complete(
        &fixture.uow,
        key,
        DeliveryExecutionContext::Live.into()
    )
    .await
    .expect("pending old obligation"));
    let mut tx = fixture.uow.begin().await.expect("inspect frozen old row");
    assert_eq!(
        CanonicalMessageRepository::load_envelope(&mut tx, key)
            .await
            .expect("load"),
        Some(envelope)
    );
    tx.commit().await.expect("read commit");
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_muc_canonical_old_sender_envelope_never_rebuilds() {
    old_sender_envelope_stays_pending(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn postgres_muc_canonical_old_sender_envelope_never_rebuilds() {
    if let Some(fixture) = IngressFixture::postgres("muc_old_provenance").await {
        old_sender_envelope_stays_pending(fixture).await;
    }
}

async fn system_sources_keep_separate_payloads(fixture: IngressFixture) {
    use crate::ingress::room_canonical::{occupant_copy_message, source, CanonicalSourceError};
    use crate::ingress_uow::EffectIntentRepository;
    use waddle_xmpp::ingress::{EffectMessageIdentity, EntityGeneration, StoredMessagePayload};
    let room: jid::BareJid = "system@muc.example.com".parse().expect("room");
    let occupant: jid::FullJid = "juliet@example.com/laptop".parse().expect("occupant");
    let mut submission =
        fixture.submission(Some("separate-system-sources"), "triggering pin command");
    submission.plan.plan.clear();
    submission.plan.intents.clear();
    for (id, body) in [
        ("system-one", Some("first pin event")),
        ("system-two", Some("second pin event")),
        ("old-system", None),
    ] {
        let stamp = waddle_xmpp_core::xep0359::StanzaId::new(id, room.clone().into());
        let system_message = body.map(|body| {
            let mut message =
                xmpp_parsers::message::Message::new(Some(jid::Jid::from(room.clone())));
            message.type_ = xmpp_parsers::message::MessageType::Groupchat;
            message.from = Some(room.clone().into());
            message.bodies.insert(Default::default(), body.into());
            waddle_xmpp_core::xep0359::add_stanza_id(&mut message, &stamp);
            StoredMessagePayload::new(message).expect("system payload")
        });
        submission
            .plan
            .intents
            .push(IngressEffectIntent::RouteMucSystemBroadcast {
                room: room.clone(),
                occupants: vec![occupant.clone()],
                room_generation: EntityGeneration::INITIAL,
                route_identity: EffectMessageIdentity::StanzaId(stamp),
                system_message,
            });
    }
    let accepted = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit system sources");
    let mut tx = fixture.uow.begin().await.expect("read sources");
    let key = accepted.message_key.expect("key");
    let recorded = EffectIntentRepository::load(&mut tx, key)
        .await
        .expect("recorded system payloads");
    let envelope = CanonicalMessageRepository::load_envelope(&mut tx, key)
        .await
        .expect("load")
        .expect("envelope");
    tx.commit().await.expect("read commit");
    assert_eq!(recorded.len(), submission.plan.intents.len());
    assert!(
        recorded
            .iter()
            .all(|intent| submission.plan.intents.contains(intent)),
        "both payloads survive database codec round trip"
    );
    for intent in &recorded {
        let IngressEffectIntent::RouteMucSystemBroadcast {
            system_message,
            route_identity,
            ..
        } = intent
        else {
            panic!("system source");
        };
        match system_message {
            Some(payload) => {
                let copy = occupant_copy_message(
                    source(&envelope, intent).expect("frozen source"),
                    &occupant,
                    &recorded,
                );
                assert_eq!(copy.from, Some(room.clone().into()));
                assert_eq!(copy.to, Some(occupant.clone().into()));
                assert_eq!(copy.bodies, payload.message().bodies);
                assert_ne!(
                    copy.bodies,
                    envelope.message().bodies,
                    "never rebuild from triggering command"
                );
                let EffectMessageIdentity::StanzaId(stamp) = route_identity else {
                    panic!("room identity");
                };
                assert_eq!(
                    waddle_xmpp::xep::extract_stanza_ids(&copy),
                    vec![stamp.clone()]
                );
            }
            None => assert_eq!(
                source(&envelope, intent),
                Err(CanonicalSourceError::MissingPayload)
            ),
        }
    }
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_muc_canonical_system_sources_are_frozen_and_distinct() {
    system_sources_keep_separate_payloads(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn postgres_muc_canonical_system_sources_are_frozen_and_distinct() {
    if let Some(fixture) = IngressFixture::postgres("muc_system_sources").await {
        system_sources_keep_separate_payloads(fixture).await;
    }
}

async fn observer_source_without_deliverable_copy(fixture: IngressFixture, late_observer: bool) {
    use waddle_xmpp::ingress::{EffectMessageIdentity, EntityGeneration};
    let room: jid::BareJid = "observer@muc.example.com".parse().expect("room");
    let occupant: jid::FullJid = "juliet@example.com/absent".parse().expect("occupant");
    let plugin = waddle_extensions::PluginId::new("message-hook-fixture").expect("plugin");
    let stamp = waddle_xmpp_core::xep0359::StanzaId::new("observer-id", room.clone().into());
    let mut submission = fixture.submission(Some("observer-no-live-copy"), "observed content");
    let request = submission.plan.sanitized_message.clone();
    let mut message = request.clone();
    message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    message.from = Some(room.with_resource_str("nick").expect("nick").into());
    message.to = Some(room.clone().into());
    waddle_xmpp_core::xep0359::add_stanza_id(&mut message, &stamp);
    waddle_xmpp::xep::xep0421::set_occupant_id_on_message(
        &mut message,
        &waddle_xmpp::xep::xep0421::OccupantId("observer-occupant".into()),
    );
    submission.plan.intents = vec![
        IngressEffectIntent::RouteMucGroupchat {
            room: room.clone(),
            occupants: vec![occupant, submission.sender.clone()],
            reflection: submission.sender.clone(),
            room_generation: EntityGeneration::INITIAL,
            route_identity: EffectMessageIdentity::StanzaId(stamp),
        },
        IngressEffectIntent::RoomObserver {
            room: room.clone(),
            requester: submission.sender.to_bare(),
            sender: submission.sender.clone(),
            plugin: plugin.clone(),
        },
    ];
    submission.plan.room_canonical_message = Some(Box::new(message.clone()));
    submission.plan.plan = vec![effects::PlannedEffect::new(Effect::External(
        ExternalEffect::Room(effects::room::ExternalRoomEffect::ObserveRoomMessage {
            room,
            plugin,
            message: Box::new(message.clone()),
            requester: submission.sender.to_bare(),
            sender: submission.sender.clone(),
            error_request: Box::new(request.clone()),
        }),
    ))];
    if late_observer {
        let observer = submission.plan.intents.pop().expect("observer intent");
        let effect = submission.plan.plan.pop().expect("observer effect");
        // Archive-free relayed acceptance retains pending owner authority, so
        // reconciliation permits its subsequently eligible observer omission.
        // Ordinary completed room authority intentionally freezes membership.
        submission
            .plan
            .intents
            .push(IngressEffectIntent::DispatchToRoomRemote {
                room: "observer@muc.example.com".parse().expect("room"),
                relay_target: waddle_xmpp::ingress::RelayTargetIdentity::owner_node(
                    "room-owner",
                    "owner-epoch",
                ),
            });
        let first = commit_submission(&fixture.uow, &submission, 1)
            .await
            .expect("first acceptance without eligible observer");
        let mut tx = fixture.uow.begin().await.expect("initial envelope");
        let envelope =
            CanonicalMessageRepository::load_envelope(&mut tx, first.message_key.expect("key"))
                .await
                .expect("load")
                .expect("envelope");
        assert!(envelope.room_observer_request().is_none());
        assert_eq!(envelope.message(), &message);
        tx.commit().await.expect("read commit");
        submission.plan.intents.push(observer);
        submission.plan.plan.push(effect);
        submission
            .plan
            .room_canonical_message
            .as_mut()
            .expect("canonical prototype")
            .bodies
            .insert(Default::default(), "changed retry prototype".into());
    }
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("observer source remains available when delivery planner emits no copies");
    let mut tx = fixture.uow.begin().await.expect("read observer source");
    let envelope =
        CanonicalMessageRepository::load_envelope(&mut tx, decision.message_key.expect("key"))
            .await
            .expect("load")
            .expect("envelope");
    assert_eq!(envelope.message(), &message);
    assert_eq!(envelope.room_observer_request(), Some(request.clone()));
    tx.commit().await.expect("read commit");
    if late_observer {
        submission.plan.plan.clear();
        let replay = commit_submission(&fixture.uow, &submission, 1)
            .await
            .expect("restore absent observer on replay");
        let observer = submission.plan.intents.last().expect("observer intent");
        let recovered = crate::ingress::recovery_rebuild::rebuild(
            crate::ingress::recovery_rebuild::RecoveryInput {
                key: decision.message_key.expect("key"),
                envelope: &envelope,
                created_at: chrono::Utc::now(),
                recorded: &submission.plan.intents,
                unreceipted: std::slice::from_ref(observer),
                route_progress: vec![],
                host_owned_resources: vec![],
                departed_occupants: vec![],
                blocked_recipients: &[],
            },
        )
        .expect("rebuild observer");
        for restored in [&replay, &recovered.decision] {
            let (observed, restored_request) = restored
                .external
                .iter()
                .find_map(|effect| match effect {
                    ExternalEffect::Room(
                        effects::room::ExternalRoomEffect::ObserveRoomMessage {
                            message,
                            error_request,
                            ..
                        },
                    ) => Some((message.as_ref(), error_request.as_ref())),
                    _ => None,
                })
                .expect("restored observer");
            assert_eq!(observed, &message, "frozen canonical content");
            assert_eq!(restored_request, &request, "persisted error request");
        }
    }
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_muc_canonical_observer_without_deliverable_copy() {
    observer_source_without_deliverable_copy(IngressFixture::sqlite().await, false).await;
}
#[tokio::test]
async fn postgres_muc_canonical_observer_without_deliverable_copy() {
    if let Some(fixture) = IngressFixture::postgres("muc_observer_no_delivery").await {
        observer_source_without_deliverable_copy(fixture, false).await;
    }
}

#[tokio::test]
async fn sqlite_muc_canonical_zero_plugins_all_unavailable() {
    canonical_owner(IngressFixture::sqlite().await, false, false).await;
}
#[tokio::test]
async fn postgres_muc_canonical_zero_plugins_all_unavailable() {
    if let Some(fixture) = IngressFixture::postgres("muc_canonical_unavailable").await {
        canonical_owner(fixture, false, false).await;
    }
}

#[tokio::test]
async fn sqlite_muc_occupant_progress_late_observer_request_survives_replay_recovery() {
    observer_source_without_deliverable_copy(IngressFixture::sqlite().await, true).await;
}
#[tokio::test]
async fn postgres_muc_occupant_progress_late_observer_request_survives_replay_recovery() {
    if let Some(fixture) = IngressFixture::postgres("muc_late_observer").await {
        observer_source_without_deliverable_copy(fixture, true).await;
    }
}
