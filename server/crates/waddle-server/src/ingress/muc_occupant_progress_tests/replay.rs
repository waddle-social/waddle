use super::*;
use crate::server::routes::interpret::effects::{Effect, PlanEffectDependency};
use crate::server::routes::interpret::DeliveryExecutionContext;
use std::sync::Arc;
use waddle_xmpp::{stream_management::InMemorySmSessionRegistry, Stanza};
use xmpp_parsers::message::{Lang, MessageType};

#[derive(Clone, Copy)]
enum ReplayCase {
    LiveSubject,
    DetachedSubject,
    RelayBody,
    MissingProvenanceSubject,
}

impl ReplayCase {
    fn subject(self) -> bool {
        !matches!(self, Self::RelayBody)
    }
}

async fn partial_replay(fixture: IngressFixture, case: ReplayCase) {
    let state = socket_tests::create_test_websocket_state().await;
    let room: jid::BareJid = "replay@muc.example.com".parse().expect("room");
    let a: jid::FullJid = "alice@example.com/phone".parse().expect("A");
    let b: jid::FullJid = "ben@example.com/phone".parse().expect("B");
    let c: jid::FullJid = "claire@example.com/phone".parse().expect("C");
    let mut submission = fixture.submission(Some("muc-replay"), "original body");
    let sender = submission.sender.clone();
    let actor = state
        .deps
        .protocol
        .room_registry
        .ask(CreateRoom {
            room_jid: room.clone(),
            waddle_id: "replay".into(),
            channel_id: "replay".into(),
            config: Default::default(),
        })
        .await
        .expect("room");
    let persistence = Arc::new(
        crate::sm_persistence::DatabaseSmPersistence::open(Some(fixture.db.database_url()))
            .await
            .expect("SM persistence"),
    );
    let sm = Arc::new(InMemorySmSessionRegistry::new().with_persistence(persistence));
    let mut receivers = Vec::new();
    for (resource, nick) in [(&a, "alice"), (&b, "ben"), (&sender, "romeo")] {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        if resource == &a && matches!(case, ReplayCase::DetachedSubject) {
            super::local::store_detached(&sm, resource).await;
        } else {
            socket_tests::register_test_connection(&state, resource, tx).await;
        }
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
    if case.subject() {
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
    deps.sm_session_registry = Some(&sm);
    plan_broadcast(&mut submission, &room, &message, &deps).await;
    submission
        .plan
        .plan
        .sort_by_key(|planned| match &planned.effect {
            Effect::External(effect) => super::super::recorded::single_target(effect) == Some(&b),
            _ => false,
        });
    let intent = submission
        .plan
        .intents
        .iter()
        .find(|intent| matches!(intent, IngressEffectIntent::RouteMucGroupchat { .. }))
        .expect("MUC obligation")
        .clone();
    let receipt = receipt_key(&intent).expect("MUC key");
    let first = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit");
    let key = first.message_key.expect("key");
    let stalled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let report = STALL_DELIVERY_RESOURCE
        .scope(
            (b.clone(), stalled.clone()),
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
    assert!(receivers[1].try_recv().is_err());
    assert!(receivers[2].try_recv().is_ok());
    if matches!(case, ReplayCase::DetachedSubject) {
        assert_eq!(super::local::append_count(&sm, &a).await, 1);
    } else {
        assert!(receivers[0].try_recv().is_ok());
    }
    let mut tx = fixture.uow.begin().await.expect("progress");
    assert_eq!(
        DeliveryProgressRepository::load(&mut tx, key, &receipt)
            .await
            .expect("load"),
        vec![a.clone()]
    );
    tx.commit().await.expect("read commit");
    let (tx, mut c_rx) = tokio::sync::mpsc::channel(16);
    socket_tests::register_test_connection(&state, &c, tx).await;
    actor
        .ask(Join {
            nick: "claire".into(),
            real_jid: c.clone(),
            role: waddle_xmpp::Role::Participant,
            affiliation: waddle_xmpp::Affiliation::Member,
        })
        .await
        .expect("new occupant");
    if matches!(case, ReplayCase::MissingProvenanceSubject) {
        let mut tx = fixture.uow.begin().await.expect("old subject row");
        crate::ingress_uow::CanonicalMessageRepository::record_room_canonical_envelope(
            &mut tx,
            key,
            &crate::ingress_substrate::MessageEnvelope::new(message.clone()),
        )
        .await
        .expect("pre-fix source");
        tx.commit().await.expect("old source commit");
    }
    plan_broadcast(&mut submission, &room, &message, &deps).await;
    // Fresh delivery availability can select relay, while its canonical row
    // still owns the same frozen non-sender audience. Exercise commit filtering
    // without executing a relay (the relay progress arm is a separate task).
    if matches!(case, ReplayCase::RelayBody) {
        for planned in &mut submission.plan.plan {
            if let Effect::External(ExternalEffect::Delivery(
                ExternalDeliveryEffect::RouteToPeer {
                    jid,
                    stanza,
                    route_identity,
                    call_setup,
                    ..
                },
            )) = &planned.effect
            {
                let mut changed = stanza.clone();
                if let Stanza::Message(message) = changed.as_mut() {
                    message.from =
                        Some(room.with_resource_str("changed-nick").expect("nick").into());
                    message.bodies.insert(Lang::new(), "changed body".into());
                }
                planned.effect = Effect::External(ExternalEffect::Delivery(
                    ExternalDeliveryEffect::RelayFullJid {
                        route_identity: route_identity.clone(),
                        origin: None,
                        target: jid.clone(),
                        stanza: changed,
                        call_setup: call_setup.clone(),
                    },
                ));
            }
        }
    }
    let retry = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("duplicate");
    let targets: Vec<_> = retry
        .external
        .iter()
        .filter_map(super::super::recorded::single_target)
        .cloned()
        .collect();
    assert_eq!(
        targets.contains(&b),
        !matches!(case, ReplayCase::MissingProvenanceSubject),
        "pending B needs frozen provenance: {targets:?}"
    );
    assert!(targets.contains(&sender), "reflection retained");
    if matches!(case, ReplayCase::RelayBody) {
        assert!(
            !targets.contains(&a),
            "completed relay A suppressed: {targets:?}"
        );
        assert!(!targets.contains(&c), "new relay C suppressed: {targets:?}");
        let copy = retry
            .external
            .iter()
            .find_map(|effect| match effect {
                ExternalEffect::Delivery(ExternalDeliveryEffect::RelayFullJid {
                    target,
                    stanza,
                    ..
                }) if target == &b => match stanza.as_ref() {
                    Stanza::Message(message) => Some(message),
                    _ => None,
                },
                _ => None,
            })
            .expect("B relay copy");
        assert_eq!(
            copy.from,
            Some(room.with_resource_str("romeo").expect("nick").into())
        );
        assert_eq!(
            copy.bodies.get(&Lang::new()).map(String::as_str),
            Some("original body")
        );
        assert!(waddle_xmpp::xep::xep0421::extract_occupant_id_from_message(copy).is_some());
        let IngressEffectIntent::RouteMucGroupchat { route_identity, .. } = &intent else {
            panic!("MUC")
        };
        assert!(super::super::receipts::routing::message_identity(
            copy,
            route_identity
        ));
    } else {
        assert!(
            targets.contains(&a),
            "completed A gets generic subject reapplication: {targets:?}"
        );
        assert!(
            targets.contains(&c),
            "new C gets generic subject reapplication: {targets:?}"
        );
        for (index, effect) in retry.external.iter().enumerate() {
            let Some(target) = super::super::recorded::single_target(effect) else {
                continue;
            };
            assert!(
                retry.external_dependencies[index]
                    .iter()
                    .any(|dependency| matches!(
                        dependency,
                        PlanEffectDependency::AfterRoomSubject { .. }
                    )),
                "subject dependency retained"
            );
            if target == &a || target == &c {
                assert!(!super::super::execute_uow::owns(
                    effect,
                    &retry.route_progress
                ));
                assert!(!retry.external_receipts[index].contains(&receipt));
            }
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
        assert_eq!(
            receivers[1].try_recv().is_ok(),
            !matches!(case, ReplayCase::MissingProvenanceSubject),
            "only proven pending copies deliver"
        );
        assert!(
            receivers[2].try_recv().is_ok(),
            "fresh reflection survives provenance failure"
        );
        assert!(c_rx.try_recv().is_ok(), "C receives subject reapplication");
        if matches!(case, ReplayCase::DetachedSubject) {
            assert_eq!(
                super::local::append_count(&sm, &a).await,
                2,
                "generic reapplication allocates a new frame"
            );
            assert_eq!(
                fixture.count("sm_ingress_appends").await,
                1,
                "A reapplication carries no MUC append context"
            );
        } else {
            assert!(
                receivers[0].try_recv().is_ok(),
                "A receives subject reapplication"
            );
        }
        if matches!(case, ReplayCase::MissingProvenanceSubject) {
            assert!(!terminalize_if_complete(
                &fixture.uow,
                key,
                DeliveryExecutionContext::Live.into()
            )
            .await
            .expect("pending"));
            let mut tx = fixture.uow.begin().await.expect("unchanged progress");
            assert_eq!(
                DeliveryProgressRepository::load(&mut tx, key, &receipt)
                    .await
                    .expect("progress"),
                vec![a]
            );
            tx.commit().await.expect("read");
            fixture.close().await;
            return;
        }
        let mut tx = fixture.uow.begin().await.expect("final progress");
        assert_eq!(
            DeliveryProgressRepository::load(&mut tx, key, &receipt)
                .await
                .expect("progress"),
            vec![a, b]
        );
        assert!(EffectReceiptRepository::contains(
            &mut tx,
            key,
            receipt.kind,
            &receipt.semantic_identity_hash
        )
        .await
        .expect("aggregate"));
        tx.commit().await.expect("read commit");
        assert!(
            terminalize_if_complete(&fixture.uow, key, DeliveryExecutionContext::Live.into())
                .await
                .expect("terminal")
        );
    }
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_muc_replay_live_subject() {
    partial_replay(IngressFixture::sqlite().await, ReplayCase::LiveSubject).await;
}
#[tokio::test]
async fn postgres_muc_replay_live_subject() {
    if let Some(fixture) = IngressFixture::postgres("muc_replay_live_subject").await {
        partial_replay(fixture, ReplayCase::LiveSubject).await;
    }
}
#[tokio::test]
async fn sqlite_muc_replay_detached_subject() {
    partial_replay(IngressFixture::sqlite().await, ReplayCase::DetachedSubject).await;
}
#[tokio::test]
async fn postgres_muc_replay_detached_subject() {
    if let Some(fixture) = IngressFixture::postgres("muc_replay_detached_subject").await {
        partial_replay(fixture, ReplayCase::DetachedSubject).await;
    }
}
#[tokio::test]
async fn sqlite_muc_replay_relay_filter() {
    partial_replay(IngressFixture::sqlite().await, ReplayCase::RelayBody).await;
}
#[tokio::test]
async fn postgres_muc_replay_relay_filter() {
    if let Some(fixture) = IngressFixture::postgres("muc_replay_relay_filter").await {
        partial_replay(fixture, ReplayCase::RelayBody).await;
    }
}

#[tokio::test]
async fn sqlite_muc_occupant_progress_old_subject_rebroadcast() {
    partial_replay(
        IngressFixture::sqlite().await,
        ReplayCase::MissingProvenanceSubject,
    )
    .await;
}
#[tokio::test]
async fn postgres_muc_occupant_progress_old_subject_rebroadcast() {
    if let Some(fixture) = IngressFixture::postgres("muc_old_subject").await {
        partial_replay(fixture, ReplayCase::MissingProvenanceSubject).await;
    }
}
