use super::*;
use crate::ingress_uow::EffectIntentRepository;
use waddle_xmpp::{
    mam::{MamStorage, SqlxMamStorage},
    muc::pin::PinChangeRequest,
};
use waddle_xmpp_core::xep0359::StanzaId;

#[derive(Clone, Copy)]
enum PinCase {
    Pending,
    ArchivePending,
    ArchiveFailure,
    Complete,
    MissingPayload,
}

async fn pin_recovery(f: IngressFixture, case: PinCase) {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let sm = persistent_sm(&f).await;
    let resource: jid::FullJid = "alice@example.com/phone".parse().expect("occupant");
    store_detached(&sm, &resource).await;
    let state = state_for(&f, sm.clone()).await;
    let mut submission =
        planned_room(&f, &state, Case::Lost, std::slice::from_ref(&resource)).await;
    store_detached(&sm, &submission.sender).await;
    let room: jid::BareJid = "recovery@muc.example.com".parse().expect("room");
    let mam: Arc<dyn MamStorage> = Arc::new(
        SqlxMamStorage::open(f.db.database_url())
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
    mam.store_message(&room, &target).await.expect("target");
    let sink = PlanSink::new();
    let capture = IngressEffectCapture::new();
    let mut deps = build_interpret_deps(&state, None);
    deps.inbox_storage = None;
    deps.mam_storage = Some(&mam);
    deps.effects = &sink;
    deps.ingress_effect_capture = Some(capture.clone());
    crate::server::routes::interpret::interpret(
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
    let (effects, execution) = sink.take();
    submission.plan.plan = effects;
    submission.plan.room_execution = execution;
    submission.plan.room_canonical_message = None;
    submission.plan.intents = capture.snapshot().intents;
    if matches!(case, PinCase::MissingPayload) {
        for intent in &mut submission.plan.intents {
            if let IngressEffectIntent::RouteMucSystemBroadcast { system_message, .. } = intent {
                *system_message = None;
            }
        }
    }
    let unrelated_archive = if matches!(case, PinCase::ArchivePending) {
        let mut other = submission
            .plan
            .intents
            .iter()
            .find(|i| matches!(i, IngressEffectIntent::SystemMessageArchive { .. }))
            .expect("system archive")
            .clone();
        if let IngressEffectIntent::SystemMessageArchive {
            sequence,
            stanza_id,
            ..
        } = &mut other
        {
            *sequence += 1;
            *stanza_id = StanzaId::new("different-system-broadcast", room.clone().into());
        }
        submission.plan.intents.push(other.clone());
        Some(other)
    } else {
        None
    };
    let decision = commit_submission(&f.uow, &submission, 1)
        .await
        .expect("pin commit");
    let key = decision.message_key.expect("key");
    if let Some(other) = unrelated_archive {
        let mut tx = f.uow.begin().await.expect("unrelated archive receipt");
        CanonicalMessageRepository::lock(&mut tx, key)
            .await
            .expect("lock");
        crate::ingress_uow::settle_recorded(&mut tx, key, &[other])
            .await
            .expect("other archive complete");
        tx.commit().await.expect("other receipt");
    }
    let mut tx = f.uow.begin().await.expect("intents");
    let intents = EffectIntentRepository::load(&mut tx, key)
        .await
        .expect("intents");
    tx.commit().await.expect("read");
    let muc = intents
        .iter()
        .find(|i| matches!(i, IngressEffectIntent::RouteMucSystemBroadcast { .. }))
        .expect("MUC");
    let receipt = receipt_key(muc).expect("MUC key");
    let archive = intents.iter().find(|i| matches!(i, IngressEffectIntent::SystemMessageArchive { stanza_id, .. } if matches!(muc, IngressEffectIntent::RouteMucSystemBroadcast { route_identity: EffectMessageIdentity::StanzaId(id), .. } if id == stanza_id))).expect("correlated archive");
    let archive_key = receipt_key(archive).expect("archive key");
    if matches!(case, PinCase::ArchiveFailure) {
        let hash = hex::encode(archive_key.semantic_identity_hash);
        match f.db.driver() {
            crate::db::DatabaseDriver::Sqlite => f.execute(&format!("CREATE TRIGGER fail_muc_archive BEFORE INSERT ON ingress_effect_receipts WHEN NEW.semantic_identity_hash = X'{hash}' BEGIN SELECT RAISE(FAIL, 'injected archive failure'); END"), ()).await,
            crate::db::DatabaseDriver::Postgres => {
                f.execute(&format!("CREATE FUNCTION fail_muc_archive() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.semantic_identity_hash = decode('{hash}', 'hex') THEN RAISE EXCEPTION 'injected archive failure'; END IF; RETURN NEW; END $$"), ()).await;
                f.execute("CREATE TRIGGER fail_muc_archive BEFORE INSERT ON ingress_effect_receipts FOR EACH ROW EXECUTE FUNCTION fail_muc_archive()", ()).await;
            }
        }
    }
    deps.effects = &ImmediateSink;
    if !matches!(case, PinCase::Pending | PinCase::MissingPayload) {
        let mut before_delivery = decision.clone();
        let indices: Vec<_> = decision.external.iter().enumerate().filter_map(|(i,e)| {
            let retain = !matches!(e, ExternalEffect::Delivery(_)) && (!matches!(case, PinCase::ArchivePending) || !matches!(e, ExternalEffect::Room(crate::ingress::effects::room::ExternalRoomEffect::ArchiveAfterPin { .. })));
            retain.then_some(i)
        }).collect();
        before_delivery.external = indices
            .iter()
            .map(|i| decision.external[*i].clone())
            .collect();
        before_delivery.external_dependencies = indices
            .iter()
            .map(|i| decision.external_dependencies[*i].clone())
            .collect();
        before_delivery.external_receipts = indices
            .iter()
            .map(|i| decision.external_receipts[*i].clone())
            .collect();
        let report = execute_effects(
            &f.uow,
            &f.db,
            &before_delivery,
            &ImmediateSink,
            &deps,
            Duration::from_secs(5),
        )
        .await;
        if matches!(case, PinCase::ArchiveFailure) {
            assert!(
                report
                    .outcomes
                    .iter()
                    .any(|(_, o)| *o == crate::ingress::ExternalOutcome::Failed),
                "archive failed: {report:?}"
            );
        }
    }
    assert_eq!(append_count(&sm, &resource).await, 0);
    let env: Arc<dyn RecoveryEnvironment> = Arc::new(StateEnvironment(state.clone()));
    let before_unrecoverable = metrics
        .counter_sum(
            "ingress.maintenance.unrecoverable_obligations",
            &[("kind", "route_muc")],
        )
        .unwrap_or(0);
    assert_eq!(
        pass(&f, &env, &MaintenanceCursor::default()).await,
        MaintenanceOutcome::Complete
    );
    assert!(
        super::super::super::attempt_count(key) > 0,
        "maintenance attempted pin row"
    );
    if matches!(case, PinCase::MissingPayload) {
        assert_eq!(
            metrics
                .counter_sum(
                    "ingress.maintenance.unrecoverable_obligations",
                    &[("kind", "route_muc")],
                )
                .unwrap_or(0),
            before_unrecoverable + 1,
            "missing payload counts under the kind-only label"
        );
        let (_, attribute_counts) = metrics
            .counter_shape("ingress.maintenance.unrecoverable_obligations")
            .expect("exported counter");
        assert!(
            attribute_counts.iter().all(|count| *count == 1),
            "reason remains a log field; kind is the only metric label"
        );
    }
    let complete = matches!(case, PinCase::Complete);
    assert_eq!(append_count(&sm, &resource).await, usize::from(complete));
    let mut tx = f.uow.begin().await.expect("inspect");
    assert_eq!(
        EffectReceiptRepository::contains(
            &mut tx,
            key,
            receipt.kind,
            &receipt.semantic_identity_hash
        )
        .await
        .expect("receipt"),
        complete
    );
    assert_eq!(
        EffectReceiptRepository::contains(
            &mut tx,
            key,
            archive_key.kind,
            &archive_key.semantic_identity_hash
        )
        .await
        .expect("archive receipt"),
        complete
    );
    tx.commit().await.expect("read");
    assert_eq!(
        f.count("mam_messages").await,
        if complete { 2 } else { 1 },
        "maintenance never replays archive"
    );
    if complete {
        let session = sm
            .peek_session(&resource.to_string())
            .await
            .expect("SM")
            .expect("session");
        let copy = waddle_xmpp::parser::message_from_string(&session.unacked_stanzas[0].stanza_xml)
            .expect("wire");
        assert_eq!(copy.from, Some(room.into()));
        assert_eq!(copy.to, Some(resource.into()));
    }
    f.close().await;
}
macro_rules! paired_pin {
    ($sqlite:ident, $postgres:ident, $case:expr) => {
        #[tokio::test]
        async fn $sqlite() {
            pin_recovery(IngressFixture::sqlite().await, $case).await;
        }
        #[tokio::test]
        async fn $postgres() {
            if let Some(f) = IngressFixture::postgres(stringify!($postgres)).await {
                pin_recovery(f, $case).await;
            }
        }
    };
}
paired_pin!(
    sqlite_muc_recovery_pin_pending,
    postgres_muc_recovery_pin_pending,
    PinCase::Pending
);
paired_pin!(
    sqlite_muc_recovery_archive_pending,
    postgres_muc_recovery_archive_pending,
    PinCase::ArchivePending
);
paired_pin!(
    sqlite_muc_recovery_archive_failure,
    postgres_muc_recovery_archive_failure,
    PinCase::ArchiveFailure
);
paired_pin!(
    sqlite_muc_recovery_system_payload,
    postgres_muc_recovery_system_payload,
    PinCase::Complete
);

paired_pin!(
    sqlite_muc_recovery_missing_system_payload,
    postgres_muc_recovery_missing_system_payload,
    PinCase::MissingPayload
);
