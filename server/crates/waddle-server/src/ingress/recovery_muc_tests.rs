//! Real room planning followed by maintenance, without client retransmission.
use super::*;
use crate::ingress::{receipt_key, IngressEffectCapture};
use crate::ingress_uow::DeliveryProgressRepository;
use waddle_xmpp::{
    muc::{room_actor::Join, room_registry_actor::CreateRoom},
    protocol::OutboundEvent,
};

#[derive(Clone, Copy)]
enum Case {
    Lost,
    Partial,
    Inbox,
    SenderOnly,
    OldEnvelope,
    SubjectPending,
    SubjectReady,
    Unavailable,
    OccupantPm,
}

async fn planned_room(
    f: &IngressFixture,
    state: &WebSocketState,
    case: Case,
    resources: &[jid::FullJid],
) -> IngressSubmission {
    let room: jid::BareJid = "recovery@muc.example.com".parse().expect("room");
    let mut submission = f.submission(Some("muc-recovery"), "frozen original content");
    let actor = state
        .deps
        .protocol
        .room_registry
        .ask(CreateRoom {
            room_jid: room.clone(),
            waddle_id: "recovery".into(),
            channel_id: "recovery".into(),
            config: Default::default(),
        })
        .await
        .expect("room");
    for (resource, nick) in resources
        .iter()
        .map(|r| (r, r.node().expect("node").as_str()))
        .chain(std::iter::once((&submission.sender, "romeo")))
    {
        actor
            .ask(Join {
                nick: nick.into(),
                real_jid: resource.clone(),
                role: if matches!(case, Case::SubjectPending | Case::SubjectReady)
                    && resource == &submission.sender
                {
                    waddle_xmpp::Role::Moderator
                } else {
                    waddle_xmpp::Role::Participant
                },
                affiliation: if matches!(case, Case::SubjectPending | Case::SubjectReady)
                    && resource == &submission.sender
                {
                    waddle_xmpp::Affiliation::Owner
                } else {
                    waddle_xmpp::Affiliation::Member
                },
            })
            .await
            .expect("join");
    }
    retarget(
        &mut submission,
        NormalizedTarget::Bare(room.clone()),
        xmpp_parsers::message::MessageType::Groupchat,
    );
    // Archive-free keeps P3's kind-2-only half free of supported siblings.
    let message = &mut submission.plan.sanitized_message;
    if !matches!(case, Case::Partial | Case::Inbox) {
        message.bodies.clear();
    }
    message
        .payloads
        .push(waddle_xmpp::xep::xep0085::build_chat_state_element(
            waddle_xmpp::xep::xep0085::ChatState::Composing,
        ));
    if matches!(case, Case::SubjectPending | Case::SubjectReady) {
        message
            .subjects
            .insert(Default::default(), "frozen subject".into());
    }
    let sink = PlanSink::new();
    let capture = IngressEffectCapture::new();
    let mut deps = build_interpret_deps(state, None);
    deps.inbox_storage = None;
    deps.effects = &sink;
    deps.ingress_effect_capture = Some(capture.clone());
    crate::server::routes::interpret::interpret(
        vec![OutboundEvent::DispatchToRoom {
            room,
            message: Box::new(message.clone()),
        }],
        &deps,
    )
    .await;
    submission.plan.room_canonical_message = sink.room_canonical_message();
    let (effects, execution) = sink.take();
    submission.plan.plan = effects;
    submission.plan.room_execution = execution;
    submission.plan.intents = capture.snapshot().intents;
    submission
}

async fn muc_recovery(f: IngressFixture, case: Case) {
    let sm = persistent_sm(&f).await;
    let a: jid::FullJid = "alice@example.com/phone".parse().expect("A");
    let b: jid::FullJid = "ben@example.com/phone".parse().expect("B");
    let c: jid::FullJid = "claire@example.com/phone".parse().expect("C");
    let resources = if matches!(case, Case::SenderOnly) {
        vec![]
    } else {
        vec![a.clone(), b.clone()]
    };
    if !matches!(case, Case::Unavailable) {
        for r in &resources {
            store_detached(&sm, r).await;
        }
    }
    let state = state_for(&f, sm.clone()).await;
    let mut submission = planned_room(&f, &state, case, &resources).await;
    let mut muc = submission
        .plan
        .intents
        .iter()
        .find(|i| matches!(i, IngressEffectIntent::RouteMucGroupchat { .. }))
        .expect("MUC intent")
        .clone();
    if let IngressEffectIntent::RouteMucGroupchat { occupants, .. } = &mut muc {
        occupants.sort();
        occupants.dedup();
    }
    let receipt = receipt_key(&muc).expect("MUC receipt");
    if matches!(case, Case::Inbox) {
        submission
            .plan
            .intents
            .push(IngressEffectIntent::RouteDirect {
                recipient: b.to_bare(),
                fanout: vec![b.clone()],
                route_identity: EffectMessageIdentity::capture_ordinal(999),
            });
    }
    if matches!(case, Case::OccupantPm) {
        submission
            .plan
            .intents
            .push(IngressEffectIntent::RouteOccupantPm {
                recipient: b.clone(),
                sender: submission.sender.clone(),
            });
    }
    let decision = commit_submission(&f.uow, &submission, 1)
        .await
        .expect("commit");
    let key = decision.message_key.expect("key");
    let mut tx = f.uow.begin().await.expect("frozen source");
    let frozen = CanonicalMessageRepository::load_envelope(&mut tx, key)
        .await
        .expect("envelope")
        .expect("canonical source");
    tx.commit().await.expect("read");
    if matches!(case, Case::OldEnvelope) {
        let mut tx = f.uow.begin().await.expect("old row");
        CanonicalMessageRepository::lock(&mut tx, key)
            .await
            .expect("lock");
        CanonicalMessageRepository::record_room_canonical_envelope(
            &mut tx,
            key,
            &crate::ingress_substrate::MessageEnvelope::new(
                submission.plan.sanitized_message.clone(),
            ),
        )
        .await
        .expect("old real-sender envelope");
        tx.commit().await.expect("old row commit");
    }
    let env: Arc<dyn RecoveryEnvironment> = Arc::new(StateEnvironment(state.clone()));
    if matches!(case, Case::SubjectReady | Case::Lost | Case::SenderOnly) {
        let mut mutations = decision.clone();
        let indices: Vec<_> = decision
            .external
            .iter()
            .enumerate()
            .filter_map(|(i, e)| (!matches!(e, ExternalEffect::Delivery(_))).then_some(i))
            .collect();
        mutations.external = indices
            .iter()
            .map(|i| decision.external[*i].clone())
            .collect();
        mutations.external_dependencies = indices
            .iter()
            .map(|i| decision.external_dependencies[*i].clone())
            .collect();
        mutations.external_receipts = indices
            .iter()
            .map(|i| decision.external_receipts[*i].clone())
            .collect();
        let deps = env.recovery_deps();
        let report = execute_effects(
            &f.uow,
            &f.db,
            &mutations,
            &ImmediateSink,
            &deps,
            Duration::from_secs(5),
        )
        .await;
        assert!(
            report.receipt_failures.is_empty(),
            "non-delivery effects persisted: {report:?}"
        );
    }
    if matches!(case, Case::SenderOnly) {
        f.execute("DELETE FROM ingress_effect_receipts WHERE kind = 2", ())
            .await;
        f.execute("UPDATE ingress_messages SET terminal_at = NULL", ())
            .await;
    }
    if matches!(case, Case::Lost | Case::SenderOnly) {
        let mut tx = f.uow.begin().await.expect("pending kinds");
        let recorded = crate::ingress_uow::EffectIntentRepository::load(&mut tx, key)
            .await
            .expect("recorded");
        let keys = EffectReceiptRepository::keys(&mut tx, key)
            .await
            .expect("receipts");
        let pending: Vec<_> = recorded
            .into_iter()
            .filter(|intent| !keys.contains(&receipt_key(intent).expect("receipt key")))
            .collect();
        assert_eq!(
            pending,
            vec![muc.clone()],
            "kind-2-only pending prerequisite"
        );
        tx.commit().await.expect("read");
    }
    if matches!(case, Case::Partial | Case::Inbox) {
        let deps = env.recovery_deps();
        let entered = Arc::new(AtomicBool::new(false));
        let mut ordered = decision.clone();
        // There is one detached effect per occupant; preserve all original dependencies.
        let b_index = ordered.external.iter().position(|e| matches!(e, ExternalEffect::Delivery(ExternalDeliveryEffect::QueueDetached { resources, .. }) if resources == std::slice::from_ref(&b))).expect("B effect");
        let last = ordered.external.len() - 1;
        ordered.external.swap(b_index, last);
        ordered.external_dependencies.swap(b_index, last);
        ordered.external_receipts.swap(b_index, last);
        let report = STALL_DELIVERY_RESOURCE
            .scope(
                (b.clone(), entered.clone()),
                execute_effects(
                    &f.uow,
                    &f.db,
                    &ordered,
                    &ImmediateSink,
                    &deps,
                    Duration::from_millis(100),
                ),
            )
            .await;
        assert!(entered.load(Ordering::SeqCst), "B stalled: {report:?}");
        assert_eq!(append_count(&sm, &a).await, 1);
        assert_eq!(append_count(&sm, &b).await, 0);
    }
    store_detached(&sm, &c).await;
    let room = "recovery@muc.example.com".parse().expect("room");
    let actor = state
        .deps
        .protocol
        .room_registry
        .ask(waddle_xmpp::muc::room_registry_actor::GetRoom { room_jid: room })
        .await
        .expect("lookup")
        .expect("actor");
    actor
        .ask(Join {
            nick: "late".into(),
            real_jid: c.clone(),
            role: waddle_xmpp::Role::Participant,
            affiliation: waddle_xmpp::Affiliation::Member,
        })
        .await
        .expect("late join");
    let archives_before_recovery = f.count("mam_messages").await;
    let cursor = MaintenanceCursor::default();
    for _ in 0..2 {
        assert_eq!(pass(&f, &env, &cursor).await, MaintenanceOutcome::Complete);
    }
    assert!(
        super::super::attempt_count(key) > 0,
        "P3: kind-2 row is attempted"
    );
    let blocked = matches!(
        case,
        Case::OldEnvelope | Case::SubjectPending | Case::Unavailable
    );
    let mut tx = f.uow.begin().await.expect("inspect");
    assert_eq!(
        EffectReceiptRepository::contains(
            &mut tx,
            key,
            receipt.kind,
            &receipt.semantic_identity_hash
        )
        .await
        .expect("MUC receipt"),
        !blocked
    );
    let progress = DeliveryProgressRepository::load(&mut tx, key, &receipt)
        .await
        .expect("progress");
    assert_eq!(progress, if blocked { vec![] } else { resources.clone() });
    assert_eq!(
        CanonicalMessageRepository::is_terminal(&mut tx, key)
            .await
            .expect("terminal"),
        !blocked && !matches!(case, Case::Inbox | Case::OccupantPm)
    );
    tx.commit().await.expect("read");
    assert_eq!(append_count(&sm, &c).await, 0, "audience never widens");
    if !matches!(case, Case::Unavailable) {
        for resource in &resources {
            let session = sm
                .peek_session(&resource.to_string())
                .await
                .expect("SM")
                .expect("session");
            assert_eq!(session.unacked_stanzas.len(), usize::from(!blocked));
            if !blocked {
                let message = &waddle_xmpp::parser::message_from_string(
                    &session.unacked_stanzas[0].stanza_xml,
                )
                .expect("wire message");
                assert_eq!(message.from, frozen.message().from);
                assert_eq!(message.bodies, frozen.message().bodies);
                assert_eq!(message.subjects, frozen.message().subjects);
                assert_eq!(message.to, Some(resource.clone().into()));
                assert_eq!(
                    waddle_xmpp::xep::extract_stanza_ids(message),
                    waddle_xmpp::xep::extract_stanza_ids(frozen.message())
                );
                assert_eq!(
                    waddle_xmpp::xep::xep0421::extract_occupant_id_from_message(message),
                    waddle_xmpp::xep::xep0421::extract_occupant_id_from_message(frozen.message())
                );
            }
        }
    }
    assert_eq!(
        f.count("mam_messages").await,
        archives_before_recovery,
        "recovery never archives"
    );
    f.close().await;
}

macro_rules! paired {
    ($sqlite:ident, $postgres:ident, $case:expr) => {
        #[tokio::test]
        async fn $sqlite() {
            muc_recovery(IngressFixture::sqlite().await, $case).await;
        }
        #[tokio::test]
        async fn $postgres() {
            if let Some(f) = IngressFixture::postgres(stringify!($postgres)).await {
                muc_recovery(f, $case).await;
            }
        }
    };
}
paired!(
    sqlite_muc_recovery_kind2_only,
    postgres_muc_recovery_kind2_only,
    Case::Lost
);
paired!(
    sqlite_muc_recovery_timeout_frozen_audience,
    postgres_muc_recovery_timeout_frozen_audience,
    Case::Partial
);
paired!(
    sqlite_muc_recovery_supported_inbox_sibling,
    postgres_muc_recovery_supported_inbox_sibling,
    Case::Inbox
);
paired!(
    sqlite_muc_recovery_sender_only_old_row,
    postgres_muc_recovery_sender_only_old_row,
    Case::SenderOnly
);
paired!(
    sqlite_muc_recovery_old_provenance,
    postgres_muc_recovery_old_provenance,
    Case::OldEnvelope
);
paired!(
    sqlite_muc_recovery_subject_pending,
    postgres_muc_recovery_subject_pending,
    Case::SubjectPending
);
paired!(
    sqlite_muc_recovery_unavailable,
    postgres_muc_recovery_unavailable,
    Case::Unavailable
);
paired!(
    sqlite_muc_recovery_occupant_pm_untouched,
    postgres_muc_recovery_occupant_pm_untouched,
    Case::OccupantPm
);

#[path = "recovery_muc_pin_tests.rs"]
mod pin;

#[cfg(feature = "clustering")]
#[path = "recovery_muc_remote_tests.rs"]
mod remote;

paired!(
    sqlite_muc_recovery_subject_receipted,
    postgres_muc_recovery_subject_receipted,
    Case::SubjectReady
);
