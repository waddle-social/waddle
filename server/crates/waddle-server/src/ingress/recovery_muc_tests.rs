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
    planned_room_with_origin(f, state, case, resources, "muc-recovery").await
}

/// The same planning pass under a caller-chosen XEP-0359 origin id, so one
/// test can commit SEVERAL distinct canonical rows for the same room and the
/// same occupants — the shape one ghost pinning several rows takes (#1803).
async fn planned_room_with_origin(
    f: &IngressFixture,
    state: &WebSocketState,
    case: Case,
    resources: &[jid::FullJid],
    origin: &str,
) -> IngressSubmission {
    let room: jid::BareJid = "recovery@muc.example.com".parse().expect("room");
    let mut submission = f.submission(Some(origin), "frozen original content");
    // Get-or-create: a second obligation for the SAME room reuses the live
    // incarnation, exactly as a second client message would.
    let existing = state
        .deps
        .protocol
        .room_registry
        .ask(waddle_xmpp::muc::room_registry_actor::GetRoom {
            room_jid: room.clone(),
        })
        .await
        .expect("registry lookup");
    let actor = match existing {
        Some(actor) => actor,
        None => state
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
            .expect("room"),
    };
    for (resource, nick) in resources
        .iter()
        .map(|r| (r, r.node().expect("node").as_str()))
        .chain(std::iter::once((&submission.sender, "romeo")))
    {
        // A reused room already seats these occupants; re-joining them would
        // only collide on the nick.
        if actor
            .ask(waddle_xmpp::muc::room_actor::GetOccupantByJid {
                jid: resource.clone(),
            })
            .await
            .expect("occupancy probe")
            .is_some()
        {
            continue;
        }
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
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
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
    let before_unrecoverable = metrics
        .counter_sum(
            "ingress.maintenance.unrecoverable_obligations",
            &[("kind", "route_muc")],
        )
        .unwrap_or(0);
    let cursor = MaintenanceCursor::default();
    for _ in 0..2 {
        assert_eq!(pass(&f, &env, &cursor).await, MaintenanceOutcome::Complete);
    }
    assert!(
        super::super::attempt_count(key) > 0,
        "P3: kind-2 row is attempted"
    );
    if matches!(case, Case::OldEnvelope | Case::SubjectPending) {
        assert_eq!(
            super::super::attempt_count(key),
            2,
            // #1803: both rooms still SEAT the frozen occupants, so the route
            // still owes them a copy after the settlement pass and the row is
            // never cached as unsupported — the settlement's evidence is
            // time-varying and only a later attempt can change the receipt
            // counts the cache keys on. The ordinary stall accounting bounds
            // the retries (three samples, then a cooldown parking).
            "a row with owed groupchat occupants is re-attempted, not cached"
        );
        assert_eq!(
            metrics
                .counter_sum(
                    "ingress.maintenance.unrecoverable_obligations",
                    &[("kind", "route_muc")],
                )
                .unwrap_or(0),
            before_unrecoverable + 2,
            // The counter is per EVALUATION, not per row: one tick per attempt.
            "source and prerequisite failures count once per attempt"
        );
        let (_, attribute_counts) = metrics
            .counter_shape("ingress.maintenance.unrecoverable_obligations")
            .expect("exported counter");
        assert!(
            attribute_counts.iter().all(|count| *count == 2),
            "classification exports the kind and reason labels"
        );
    }
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

/// #1815: the post-execution cache decision must read the receipts as they
/// stand AFTER the rebuilt effects ran, never the pre-execution settlement
/// answer.
///
/// One row can carry both a REBUILDABLE frozen groupchat copy and a
/// warning-only `room_observer`. The copy is still owed when the departed-copy
/// settlement looks at it — its occupant is seated, so nothing may be settled —
/// and the rebuilt `QueueDetached` then delivers it in the SAME attempt,
/// landing the fanout's aggregate receipt. Nothing is owed by the time the row
/// is classified: the only obligation left is the warning-only observer, whose
/// reply belongs to a connection that no longer exists, so no later attempt can
/// settle it and the row must be cached. Trusting the pre-execution
/// `still_owed` left it uncached, and the next scan re-invoked the
/// at-least-once observer plugin.
async fn delivered_copy_still_caches_a_warning_only_observer(f: IngressFixture) {
    use crate::ingress::{
        effects::{room::ExternalRoomEffect, PlanSuppressionPolicy},
        PlannedEffect,
    };
    use waddle_extensions::{
        observer_test_support::{ObserverTestBehavior, ObserverTestPlugin},
        ExtensionManager, PluginId,
    };

    let sm = persistent_sm(&f).await;
    let occupant: jid::FullJid = "alice@example.com/phone".parse().expect("occupant");
    // A detached session is what makes the frozen copy REBUILDABLE and
    // deliverable inside the same attempt.
    store_detached(&sm, &occupant).await;
    let plugin_id = PluginId::new("muc-recovery-observer").expect("plugin id");
    let plugin = ObserverTestPlugin::new(plugin_id.clone(), ObserverTestBehavior::Warning);
    let mut state = state_for(&f, sm.clone()).await;
    Arc::get_mut(&mut state)
        .expect("unique state")
        .deps
        .protocol
        .extension_manager =
        Arc::new(ExtensionManager::with_observer_test_plugins(vec![plugin.clone()]).await);

    // `Case::Partial` differs from `Case::Lost` only in keeping the body, which
    // the observer hook carries.
    let mut submission =
        planned_room(&f, &state, Case::Partial, std::slice::from_ref(&occupant)).await;
    let muc = submission
        .plan
        .intents
        .iter()
        .find(|intent| matches!(intent, IngressEffectIntent::RouteMucGroupchat { .. }))
        .cloned()
        .expect("MUC intent");
    let receipt = receipt_key(&muc).expect("MUC receipt");
    let room: jid::BareJid = "recovery@muc.example.com".parse().expect("room");
    let observed = submission
        .plan
        .room_canonical_message
        .clone()
        .expect("room canonical message");
    submission
        .plan
        .intents
        .push(IngressEffectIntent::RoomObserver {
            room: room.clone(),
            plugin: plugin_id.clone(),
            requester: submission.sender.to_bare(),
            sender: submission.sender.clone(),
        });
    // The planned effect is what carries the observer's error request into the
    // stored envelope at commit; recovery rebuilds the effect from the intent.
    submission.plan.plan.push(
        PlannedEffect::new(Effect::External(ExternalEffect::Room(
            ExternalRoomEffect::ObserveRoomMessage {
                room,
                plugin: plugin_id,
                requester: submission.sender.to_bare(),
                sender: submission.sender.clone(),
                message: observed,
                error_request: Box::new(submission.plan.sanitized_message.clone()),
            },
        )))
        .with_suppression(PlanSuppressionPolicy::Always),
    );
    let decision = commit_submission(&f.uow, &submission, 1)
        .await
        .expect("commit");
    let key = decision.message_key.expect("key");
    assert!(
        plugin.invocations().is_empty(),
        "commit records the obligation without running the observer"
    );

    let env: Arc<dyn RecoveryEnvironment> = Arc::new(StateEnvironment(state.clone()));
    let cursor = MaintenanceCursor::default();
    for pass_number in 1..=2 {
        assert_eq!(pass(&f, &env, &cursor).await, MaintenanceOutcome::Complete);
        cursor.wait_for_recovery_accounting().await;
        assert_eq!(
            plugin.invocations().len(),
            1,
            "pass {pass_number}: the at-least-once observer is not re-invoked"
        );
        assert_eq!(
            super::super::attempt_count(key),
            1,
            "pass {pass_number}: the row is cached once its last frozen copy has landed"
        );
        assert_eq!(
            append_count(&sm, &occupant).await,
            1,
            "pass {pass_number}: the frozen copy is delivered exactly once"
        );
    }

    let mut tx = f.uow.begin().await.expect("inspect cached row");
    assert_eq!(
        DeliveryProgressRepository::load(&mut tx, key, &receipt)
            .await
            .expect("delivery progress"),
        vec![occupant.clone()],
        "the copy was owed at settlement time and delivered by the rebuild"
    );
    assert!(
        EffectReceiptRepository::contains(
            &mut tx,
            key,
            receipt.kind,
            &receipt.semantic_identity_hash
        )
        .await
        .expect("aggregate receipt"),
        "the rebuilt copy completed the frozen fanout in the same attempt"
    );
    assert!(
        !CanonicalMessageRepository::is_terminal(&mut tx, key)
            .await
            .expect("terminal"),
        "the warning-only observer keeps the row pending"
    );
    tx.commit().await.expect("read commit");
    f.close().await;
}

#[tokio::test]
async fn sqlite_delivered_copy_still_caches_a_warning_only_observer() {
    delivered_copy_still_caches_a_warning_only_observer(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_delivered_copy_still_caches_a_warning_only_observer() {
    if let Some(f) = IngressFixture::postgres("muc_recovery_delivered_copy_observer").await {
        delivered_copy_still_caches_a_warning_only_observer(f).await;
    }
}

/// A maintenance pass whose per-row deadline is `recovery_row` rather than the
/// generous one the rest of these tests run under: the #1803 probe loops are
/// bounded by exactly that deadline, so a test that measures them has to use
/// the production value.
async fn pass_with_row_deadline(
    f: &IngressFixture,
    env: &Arc<dyn RecoveryEnvironment>,
    cursor: &MaintenanceCursor,
    recovery_row: Duration,
) -> MaintenanceOutcome {
    run_maintenance_pass_with_cursor(
        &f.db,
        &f.uow,
        MaintenanceBudget {
            recovery_row,
            ..immediate_recovery_budget()
        },
        cursor,
        Some(env.clone()),
    )
    .await
}

#[path = "recovery_muc_pin_tests.rs"]
mod pin;

#[path = "recovery_departed_tests.rs"]
mod departed;

#[path = "recovery_ghosts_tests.rs"]
mod ghosts;

#[cfg(feature = "clustering")]
#[path = "recovery_muc_remote_tests.rs"]
mod remote;

paired!(
    sqlite_muc_recovery_subject_receipted,
    postgres_muc_recovery_subject_receipted,
    Case::SubjectReady
);

#[tokio::test]
async fn sqlite_muc_occupant_progress_resets_streak_and_parked_copy_recovers_after_cooldown() {
    let fixture = IngressFixture::sqlite().await;
    let sm = persistent_sm(&fixture).await;
    let state = state_for(&fixture, sm.clone()).await;
    let resources: Vec<jid::FullJid> = ["alice@example.com/phone", "ben@example.com/phone"]
        .into_iter()
        .map(|resource| resource.parse().expect("occupant"))
        .collect();
    // Both occupants keep a resumable session in the SHARED DURABLE store and
    // none in this node's memory, so the row stalls on delivery alone: a
    // seated occupant with no session anywhere would be evicted as an
    // XEP-0045 ghost (#1803) and the row would never reach its cooldown.
    let elsewhere = persistent_sm(&fixture).await;
    for resource in &resources {
        store_detached(&elsewhere, resource).await;
    }
    let submission = planned_room(&fixture, &state, Case::Unavailable, &resources).await;
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit room fanout");
    let key = decision.message_key.expect("key");
    let receipt = submission
        .plan
        .intents
        .iter()
        .find(|intent| matches!(intent, IngressEffectIntent::RouteMucGroupchat { .. }))
        .map(|intent| receipt_key(intent).expect("MUC receipt"))
        .expect("MUC route");
    let env: Arc<dyn RecoveryEnvironment> = Arc::new(StateEnvironment(state));
    let cursor = MaintenanceCursor::default();
    for attempt in 1..=2 {
        assert_eq!(
            pass(&fixture, &env, &cursor).await,
            MaintenanceOutcome::Complete
        );
        cursor.wait_for_recovery_accounting().await;
        assert_eq!(super::super::attempt_count(key), attempt);
    }
    // One occupant becomes reachable, while the aggregate MUC receipt remains pending.
    store_detached(&sm, &resources[0]).await;
    assert_eq!(
        pass(&fixture, &env, &cursor).await,
        MaintenanceOutcome::Complete
    );
    cursor.wait_for_recovery_accounting().await;
    assert_eq!(super::super::attempt_count(key), 3);
    assert_eq!(append_count(&sm, &resources[0]).await, 1);
    let mut tx = fixture.uow.begin().await.expect("inspect partial progress");
    assert_eq!(
        DeliveryProgressRepository::load(&mut tx, key, &receipt)
            .await
            .expect("progress"),
        vec![resources[0].clone()],
    );
    assert!(!EffectReceiptRepository::contains(
        &mut tx,
        key,
        receipt.kind,
        &receipt.semantic_identity_hash
    )
    .await
    .expect("aggregate remains pending"));
    tx.commit().await.expect("inspection commit");
    // The partial delivery reset the two prior stalls: three new attempts are required.
    for attempt in 4..=6 {
        assert_eq!(
            pass(&fixture, &env, &cursor).await,
            MaintenanceOutcome::Complete
        );
        cursor.wait_for_recovery_accounting().await;
        assert_eq!(super::super::attempt_count(key), attempt);
    }
    store_detached(&sm, &resources[1]).await;
    assert_eq!(
        pass(&fixture, &env, &cursor).await,
        MaintenanceOutcome::Complete
    );
    cursor.wait_for_recovery_accounting().await;
    assert_eq!(super::super::attempt_count(key), 6, "parked until cooldown");
    assert_eq!(append_count(&sm, &resources[1]).await, 0);
    tokio::time::pause();
    tokio::time::advance(MaintenanceBudget::DEFAULT.recovery_stall_cooldown).await;
    tokio::time::resume();
    assert_eq!(
        pass(&fixture, &env, &cursor).await,
        MaintenanceOutcome::Complete
    );
    cursor.wait_for_recovery_accounting().await;
    assert_eq!(
        super::super::attempt_count(key),
        7,
        "one attempt after expiry"
    );
    assert_eq!(
        append_count(&sm, &resources[0]).await,
        1,
        "first occupant is not duplicated"
    );
    assert_eq!(append_count(&sm, &resources[1]).await, 1);
    assert_eq!(
        fixture.count("sm_ingress_appends").await,
        2,
        "one keyed allocation per occupant"
    );
    let mut tx = fixture.uow.begin().await.expect("inspect completion");
    assert!(EffectReceiptRepository::contains(
        &mut tx,
        key,
        receipt.kind,
        &receipt.semantic_identity_hash
    )
    .await
    .expect("aggregate receipt"));
    tx.commit().await.expect("inspection commit");
    fixture.close().await;
}
