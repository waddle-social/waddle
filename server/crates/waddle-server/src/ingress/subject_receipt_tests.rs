use super::*;
use crate::server::routes::interpret::{effects::PlanSink, interpret};
use waddle_xmpp::{muc::RoomSubjectTexts, protocol::OutboundEvent, registry::ConnectionRegistry};

async fn rejected_subject_receipt(fixture: test_support::IngressFixture) {
    let registry = ConnectionRegistry::new();
    let capture = IngressEffectCapture::new();
    let sink = PlanSink::new();
    let mut deps = Deps::registry_only(&registry);
    deps.ingress_effect_capture = Some(capture.clone());
    deps.effects = &sink;
    let mut submission = fixture.submission(Some("subject-bounce-receipt"), "");
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
    let mut message = submission.plan.sanitized_message.clone();
    message.to = Some(room.clone().into());
    message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    message.bodies.clear();
    message
        .subjects
        .insert(xmpp_parsers::message::Lang::new(), "subject".to_owned());
    let outcome = interpret(
        vec![OutboundEvent::PersistRoomSubject {
            room,
            claim_fence: None,
            texts: RoomSubjectTexts::from_iter([(String::new(), "subject".to_owned())]),
            setter: submission.sender.to_bare(),
            sender: submission.sender.clone(),
            message: Box::new(message),
            setter_nick: "romeo".to_owned(),
            set_at: chrono::Utc::now(),
        }],
        &deps,
    )
    .await;
    assert!(outcome.frames.is_empty(), "planning defers the bounce");
    submission.plan.intents = capture.snapshot().intents;
    submission.plan.plan = sink.take().0;
    assert!(matches!(
        submission.plan.intents.as_slice(),
        [waddle_xmpp::ingress::IngressEffectIntent::ErrorReply { .. }]
    ));
    let decision = commit::commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit subject rejection");
    assert_eq!(decision.external_receipts[0].len(), 1);
    deps.effects = &ImmediateSink;
    let mut report = execute::execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(report.outcomes[0].1, ExternalOutcome::AwaitingFrameDelivery);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    assert_eq!(report.frame_obligations.len(), 1);
    for frame in &report.frame_obligations[0].frames {
        let waddle_xmpp::Stanza::Message(message) = frame else {
            panic!("subject bounce must be a message");
        };
        let element = minidom::Element::from(message.clone());
        let mut wire = Vec::new();
        element
            .write_to(&mut wire)
            .expect("write bounce to transport");
        assert!(!wire.is_empty());
    }
    assert!(report
        .complete_frame_obligations(&fixture.uow, &fixture.db, Duration::from_secs(5))
        .await
        .expect("receipt written bounce and terminalize"));
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_subject_failure_bounce_receipts_only_after_frame_delivery() {
    rejected_subject_receipt(test_support::IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_subject_failure_bounce_receipts_only_after_frame_delivery() {
    if let Some(fixture) = test_support::IngressFixture::postgres("subject_bounce_receipt").await {
        rejected_subject_receipt(fixture).await;
    }
}

/// The real room actor accepts planning reads but refuses its Phase-C subject write.
#[derive(Default)]
struct FailingSubjectStore {
    succeed: std::sync::atomic::AtomicBool,
    attempts: std::sync::atomic::AtomicUsize,
}

impl waddle_xmpp::muc::durable::MucDurableStore for FailingSubjectStore {
    fn load_room_state_fenced<'a>(
        &'a self,
        _room: &'a jid::BareJid,
        _fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
    ) -> waddle_xmpp::muc::durable::MucDurableFuture<
        'a,
        Option<waddle_xmpp::muc::durable::DurableRoomState>,
    > {
        Box::pin(async { Ok(None) })
    }

    fn commit_room_mutation<'a>(
        &'a self,
        _room: &'a jid::BareJid,
        _fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
        intent: waddle_xmpp::muc::RoomDurableMutation,
        _effects: waddle_xmpp::muc::RoomMutationEffects,
    ) -> waddle_xmpp::muc::RoomCommitFuture<'a> {
        Box::pin(async move {
            assert!(matches!(
                intent,
                waddle_xmpp::muc::RoomDurableMutation::Subject(_)
            ));
            self.attempts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.succeed.load(std::sync::atomic::Ordering::SeqCst) {
                Ok(waddle_xmpp::muc::RoomCommitOutcome {
                    coordinates: waddle_xmpp::muc::RoomCommittedCoordinates {
                        lifecycle: waddle_xmpp::muc::RoomLifecycleId::generate(),
                        revision: waddle_xmpp::muc::RoomRevision::initial(),
                    },
                    reservation: None,
                })
            } else {
                Err(waddle_xmpp::muc::RoomCommitError::OwnershipUnavailable)
            }
        })
    }

    fn check_exact_claim_fence<'a>(
        &'a self,
        _room: &'a jid::BareJid,
        _fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
    ) -> waddle_xmpp::muc::durable::MucDurableFuture<'a, bool> {
        Box::pin(async { Ok(true) })
    }
}

async fn subject_broadcast_after_actor_commit(
    fixture: test_support::IngressFixture,
    fail: bool,
    retry: bool,
) {
    use crate::server::routes::interpret::effects::{
        room::{ExternalRoomEffect, RoomActorMutation},
        Effect, EffectSink, PlanEffectDependency,
    };
    use kameo::actor::Spawn;
    use waddle_xmpp::{
        ingress::IngressEffectIntent,
        muc::{
            room_actor::{GetSnapshot, Join, RestoreDurableRoomState, SetSubject},
            room_registry_actor::{CreateRoom, RoomRegistryActor},
        },
        registry::{ConnectionEntry, RegisterUserResource, UserRegistryActor},
        xep::xep0421::OccupantIdSecret,
        Stanza,
    };
    let room: jid::BareJid = "subject@muc.example.com".parse().expect("room");
    let mut submission = fixture.submission(Some("subject-actor-commit"), "");
    let sender = submission.sender.clone();
    let occupant: jid::FullJid = "juliet@example.com/phone".parse().expect("occupant");
    let registry = ConnectionRegistry::new();
    let users = UserRegistryActor::spawn(UserRegistryActor::new());
    let mut receivers = Vec::new();
    for jid in [&sender, &occupant] {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        registry.register(jid.clone(), tx.clone());
        users
            .ask(RegisterUserResource {
                jid: jid.clone(),
                entry: ConnectionEntry::new(tx),
            })
            .await
            .expect("register occupant resource");
        receivers.push(rx);
    }
    let rooms = RoomRegistryActor::spawn(RoomRegistryActor::new(
        "muc.example.com".into(),
        OccupantIdSecret::new(vec![b's'; 32]).expect("secret"),
    ));
    let actor = rooms
        .ask(CreateRoom {
            room_jid: room.clone(),
            waddle_id: "waddle".into(),
            channel_id: "subject".into(),
            config: Default::default(),
        })
        .await
        .expect("room actor");
    for (jid, nick) in [(&sender, "romeo"), (&occupant, "juliet")] {
        actor
            .ask(Join {
                nick: nick.into(),
                real_jid: jid.clone(),
                role: waddle_xmpp::Role::Moderator,
                affiliation: waddle_xmpp::Affiliation::Owner,
            })
            .await
            .expect("join occupant");
    }
    actor
        .ask(SetSubject {
            texts: RoomSubjectTexts::from_iter([(String::new(), "old subject".into())]),
            setter: sender.to_bare(),
            setter_nick: "romeo".into(),
            set_at: chrono::Utc::now() - chrono::Duration::minutes(1),
        })
        .await
        .expect("initial subject");
    let previous = actor
        .ask(GetSnapshot)
        .await
        .expect("initial snapshot")
        .room
        .subject;
    let subject_store = std::sync::Arc::new(FailingSubjectStore::default());
    let fence = if fail {
        let fence = waddle_xmpp::muc::RoomClaimFenceContext::new(
            waddle_xmpp::ownership::Entity::new(
                waddle_xmpp::ownership::EntityType::RoomActor,
                room.to_string(),
            ),
            waddle_xmpp::ownership::NodeIdentity::local(),
            waddle_xmpp::ownership::ClaimEpoch(1),
        );
        actor
            .ask(RestoreDurableRoomState {
                store: subject_store.clone(),
                claim_fence: fence.clone(),
            })
            .await
            .expect("install failing subject store");
        Some(fence)
    } else {
        None
    };
    submission.target = waddle_xmpp::ingress::NormalizedTarget::Bare(room.clone());
    let message = &mut submission.plan.sanitized_message;
    message.to = Some(room.clone().into());
    message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    message.bodies.clear();
    message
        .subjects
        .insert(xmpp_parsers::message::Lang::new(), "new subject".into());
    submission.digest_input = waddle_xmpp::ingress::DigestInput::from_parsed(
        message,
        &waddle_xmpp::ingress::DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![room.clone()],
            stanza_lang: None,
        },
    )
    .expect("subject digest");
    // The room canonicalizer stamps this before subject persistence/reflection.
    let room_stanza_id =
        waddle_xmpp_core::xep0359::StanzaId::new("subject-reflection", room.clone().into());
    waddle_xmpp_core::xep0359::add_stanza_id(message, &room_stanza_id);
    let mut events = vec![OutboundEvent::PersistRoomSubject {
        room: room.clone(),
        claim_fence: fence,
        texts: RoomSubjectTexts::from_message_subjects(&message.subjects),
        setter: sender.to_bare(),
        sender: sender.clone(),
        message: Box::new(message.clone()),
        setter_nick: "romeo".into(),
        set_at: chrono::Utc::now(),
    }];
    for recipient in [&sender, &occupant] {
        let mut reflected = message.clone();
        reflected.from = Some(room.with_resource_str("romeo").expect("nick").into());
        reflected.to = Some(recipient.clone().into());
        events.push(OutboundEvent::RouteToConnection {
            jid: recipient.clone().into(),
            stanza: Box::new(Stanza::Message(reflected)),
            call_setup: None,
        });
    }
    let capture = IngressEffectCapture::new();
    let sink = PlanSink::new();
    sink.observe_sender(&sender);
    let mut deps = Deps::registry_only(&registry);
    deps.room_registry = Some(&rooms);
    deps.user_registry = Some(&users);
    deps.ingress_effect_capture = Some(capture.clone());
    deps.effects = &sink;
    assert!(interpret(events, &deps).await.frames.is_empty());
    // dispatch_to_room records the complete reflector audience after the nested
    // interpreter pass; retain the same obligation at this actor seam.
    capture.record_intent(IngressEffectIntent::RouteMucGroupchat {
        room: room.clone(),
        occupants: vec![sender.clone(), occupant.clone()],
        reflection: sender.clone(),
        room_generation: waddle_xmpp::ingress::EntityGeneration::INITIAL,
        route_identity: waddle_xmpp::ingress::EffectMessageIdentity::stanza(room_stanza_id),
    });
    submission.plan.intents = capture.snapshot().intents;
    submission.plan.plan = sink.take().0;
    assert_eq!(
        submission
            .plan
            .plan
            .iter()
            .filter(
                |effect| effect.dependencies.iter().any(|dependency| matches!(
                    dependency,
                    PlanEffectDependency::AfterRoomSubject { .. }
                ))
            )
            .count(),
        2,
        "both occupant reflections depend on the subject commit"
    );
    assert_eq!(
        actor
            .ask(GetSnapshot)
            .await
            .expect("planned snapshot")
            .room
            .subject,
        previous
    );
    let decision = commit::commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit subject plan");
    assert!(receivers.iter_mut().all(|rx| rx.try_recv().is_err()));
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    assert!(submission
        .plan
        .intents
        .iter()
        .any(|intent| matches!(intent, IngressEffectIntent::RoomSubjectMutation { .. })));
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("read recorded subject intents");
    let recorded = crate::ingress_uow::EffectIntentRepository::load(
        &mut tx,
        decision.message_key.expect("canonical subject key"),
    )
    .await
    .expect("load payload-complete subject intents");
    drop(tx);
    let bounce_intent = recorded
        .iter()
        .find(|intent| matches!(intent, IngressEffectIntent::ErrorReply { .. }))
        .expect("Phase-C subject rejection is durably planned");
    let bounce_key = durable::receipt_key(bounce_intent).expect("bounce receipt identity");
    assert!(decision.external_receipts[0].contains(&bounce_key));
    assert_eq!(decision.external_receipts[0].len(), 2);
    assert_eq!(decision.external_receipts.len(), 3);
    assert!(
        decision
            .external_receipts
            .iter()
            .all(|receipts| !receipts.is_empty()),
        "every broadcast has durable receipt obligations"
    );
    deps.effects = &ImmediateSink;
    let mut report = execute::execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    if fail {
        assert_eq!(
            subject_store
                .attempts
                .load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the real actor attempted SetSubject persistence"
        );
        assert!(
            receivers.iter_mut().all(|rx| rx.try_recv().is_err()),
            "no occupant sees an uncommitted subject"
        );
        assert_eq!(
            actor
                .ask(GetSnapshot)
                .await
                .expect("failed snapshot")
                .room
                .subject,
            previous
        );
        assert_eq!(report.outcomes[0].1, ExternalOutcome::AwaitingFrameDelivery);
        assert!(report.outcomes[1..]
            .iter()
            .all(|(_, outcome)| { *outcome == ExternalOutcome::Failed }));
        assert_eq!(
            report.frame_obligations.len(),
            1,
            "only the bounce is emitted"
        );
        let bounce = &report.frame_obligations[0];
        assert_eq!(
            bounce.receipt_keys,
            vec![bounce_key.clone()],
            "bounce receipts only its recorded ErrorReply"
        );
        assert_eq!(
            fixture.count("ingress_effect_receipts").await,
            0,
            "frame write has not completed"
        );
        let [Stanza::Message(reply)] = bounce.frames.as_slice() else {
            panic!("one message bounce");
        };
        let ExternalEffect::Room(ExternalRoomEffect::RoomActorMutation {
            mutation:
                RoomActorMutation::SetSubject {
                    rejection_reply, ..
                },
            ..
        }) = &decision.external[0]
        else {
            panic!("subject mutation");
        };
        assert_eq!(
            reply,
            rejection_reply.as_ref(),
            "emit the exact planned bounce stanza"
        );
        assert_eq!(reply.type_, xmpp_parsers::message::MessageType::Error);
        assert_eq!(reply.to, Some(sender.clone().into()));
        let error = reply
            .payloads
            .iter()
            .find_map(|payload| {
                xmpp_parsers::stanza_error::StanzaError::try_from(payload.clone()).ok()
            })
            .expect("typed stanza error");
        let IngressEffectIntent::ErrorReply {
            recipient,
            error: recorded_error,
        } = bounce_intent
        else {
            panic!("recorded bounce");
        };
        assert_eq!(recipient, &sender);
        assert_eq!(
            recorded_error.to_xmpp(),
            error,
            "recorded error exactly matches the emitted stanza error"
        );
        let mut wire = Vec::new();
        minidom::Element::from(reply.clone())
            .write_to(&mut wire)
            .expect("complete bounce frame write");
        let mut reconstructed = reply.clone();
        for payload in &mut reconstructed.payloads {
            if xmpp_parsers::stanza_error::StanzaError::try_from(payload.clone()).is_ok() {
                *payload = minidom::Element::from(recorded_error.to_xmpp());
            }
        }
        let mut reconstructed_wire = Vec::new();
        minidom::Element::from(reconstructed)
            .write_to(&mut reconstructed_wire)
            .expect("serialize recorded bounce reconstruction");
        assert_eq!(
            wire, reconstructed_wire,
            "durable error reconstruction writes exactly the emitted bytes"
        );
        assert_eq!(
            error.defined_condition,
            xmpp_parsers::stanza_error::DefinedCondition::ResourceConstraint
        );
        assert!(!report
            .complete_frame_obligations(&fixture.uow, &fixture.db, Duration::from_secs(5))
            .await
            .expect("bounce delivered"));
        assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
        let pending = commit::commit_submission(&fixture.uow, &submission, 1)
            .await
            .expect("same-origin retry retains subject mutation");
        assert_eq!(pending.message_key, decision.message_key);
        assert!(!pending.receipts_pending.contains(&bounce_key));
        assert_eq!(
            i64::try_from(pending.receipts_pending.len()).expect("pending count"),
            fixture.count("ingress_effect_intents").await - 1
        );
        assert_eq!(
            fixture
                .count("ingress_messages WHERE terminal_at IS NOT NULL")
                .await,
            0
        );
    } else {
        for (rx, recipient) in receivers.iter_mut().zip([&sender, &occupant]) {
            let frame = rx.try_recv().expect("occupant receives committed subject");
            let Stanza::Message(reflected) = frame.stanza else {
                panic!("subject message");
            };
            assert_eq!(
                reflected.subjects,
                submission.plan.sanitized_message.subjects
            );
            assert_eq!(
                reflected.type_,
                xmpp_parsers::message::MessageType::Groupchat
            );
            assert_eq!(reflected.to, Some(recipient.clone().into()));
            assert_eq!(
                reflected.from,
                Some(room.with_resource_str("romeo").expect("nick").into())
            );
            assert!(rx.try_recv().is_err(), "one reflection per occupant");
        }
        assert!(report.frame_obligations.is_empty());
        assert!(report
            .outcomes
            .iter()
            .all(|(_, outcome)| *outcome == ExternalOutcome::Done));
        assert_eq!(
            actor
                .ask(GetSnapshot)
                .await
                .expect("committed snapshot")
                .room
                .subject
                .expect("subject")
                .texts,
            RoomSubjectTexts::from_message_subjects(&submission.plan.sanitized_message.subjects)
        );
        assert_eq!(
            fixture.count("ingress_effect_receipts").await,
            fixture.count("ingress_effect_intents").await
        );
        assert_eq!(
            fixture
                .count("ingress_messages WHERE terminal_at IS NOT NULL")
                .await,
            1
        );
        let completed = commit::commit_submission(&fixture.uow, &submission, 1)
            .await
            .expect("success leaves no pending obligation");
        assert!(completed.receipts_pending.is_empty());
    }
    if retry {
        let Effect::External(ExternalEffect::Room(ExternalRoomEffect::RoomActorMutation {
            mutation:
                RoomActorMutation::SetSubject {
                    rejection_reply, ..
                },
            ..
        })) = &mut submission.plan.plan[0].effect
        else {
            panic!("subject mutation");
        };
        // The retry's transient stanza is not authoritative: restore from the row.
        rejection_reply.payloads.retain(|payload| {
            xmpp_parsers::stanza_error::StanzaError::try_from(payload.clone()).is_err()
        });
        subject_store
            .succeed
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let retried = commit::commit_submission(&fixture.uow, &submission, 1)
            .await
            .expect("retry subject after actor recovery");
        assert_eq!(retried.message_key, decision.message_key);
        let ExternalEffect::Room(ExternalRoomEffect::RoomActorMutation {
            mutation:
                RoomActorMutation::SetSubject {
                    rejection_reply: restored,
                    ..
                },
            ..
        }) = &retried.external[0]
        else {
            panic!("restored subject mutation");
        };
        let ExternalEffect::Room(ExternalRoomEffect::RoomActorMutation {
            mutation:
                RoomActorMutation::SetSubject {
                    rejection_reply: original,
                    ..
                },
            ..
        }) = &decision.external[0]
        else {
            panic!("original subject mutation");
        };
        assert_eq!(
            restored, original,
            "durable row reconstructs the identical bounce"
        );
        let report = execute::execute_effects(
            &fixture.uow,
            &fixture.db,
            &retried,
            &ImmediateSink,
            &deps,
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(
            subject_store
                .attempts
                .load(std::sync::atomic::Ordering::SeqCst),
            2,
            "same-origin retry re-executes SetSubject exactly once"
        );
        assert!(
            report.frame_obligations.is_empty(),
            "successful retry emits no second bounce"
        );
        assert!(report.receipt_failures.is_empty());
        assert_eq!(
            fixture.count("ingress_effect_receipts").await,
            fixture.count("ingress_effect_intents").await
        );
        assert_eq!(
            fixture
                .count("ingress_messages WHERE terminal_at IS NOT NULL")
                .await,
            1
        );
        let completed = commit::commit_submission(&fixture.uow, &submission, 1)
            .await
            .expect("completed same-origin retry");
        assert!(completed.receipts_pending.is_empty());
    }
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_subject_broadcast_waits_for_actor_persistence_failure() {
    subject_broadcast_after_actor_commit(test_support::IngressFixture::sqlite().await, true, false)
        .await;
}

#[tokio::test]
async fn postgres_subject_broadcast_waits_for_actor_persistence_failure() {
    if let Some(fixture) = test_support::IngressFixture::postgres("subject_actor_failure").await {
        subject_broadcast_after_actor_commit(fixture, true, false).await;
    }
}

#[tokio::test]
async fn sqlite_subject_broadcast_follows_actor_persistence_success() {
    subject_broadcast_after_actor_commit(
        test_support::IngressFixture::sqlite().await,
        false,
        false,
    )
    .await;
}

#[tokio::test]
async fn postgres_subject_broadcast_follows_actor_persistence_success() {
    if let Some(fixture) = test_support::IngressFixture::postgres("subject_actor_success").await {
        subject_broadcast_after_actor_commit(fixture, false, false).await;
    }
}

#[tokio::test]
async fn sqlite_subject_failure_same_origin_retry_terminalizes_without_second_bounce() {
    subject_broadcast_after_actor_commit(test_support::IngressFixture::sqlite().await, true, true)
        .await;
}

#[tokio::test]
async fn postgres_subject_failure_same_origin_retry_terminalizes_without_second_bounce() {
    if let Some(fixture) = test_support::IngressFixture::postgres("subject_actor_retry").await {
        subject_broadcast_after_actor_commit(fixture, true, true).await;
    }
}
