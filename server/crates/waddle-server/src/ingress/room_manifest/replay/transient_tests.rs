//! XEP-0045 sender echoes retain XEP-0334's one live attempt.
use super::*;
use crate::ingress::uow::{
    CanonicalMessageRepository, EffectIntentRepository, EffectReceiptRepository,
};
use crate::ingress::{
    commit::commit_submission, execute::execute_effects, test_support::IngressFixture,
    tests::xep0045_room_manifest::room_submission, IngressSubmission, RoomManifestSnapshot,
};
use crate::server::routes::interpret::{
    effects::{
        delivery::{ExternalDeliveryEffect, PeerDeliveryKind},
        Effect, ExternalEffect, ImmediateSink, PlannedEffect, RoomExecutionPath,
    },
    DeliveryExecutionContext, Deps,
};
use std::sync::Arc;
use waddle_xmpp::{
    ingress::{DigestContext, DigestInput, IngressEffectIntent},
    muc::{
        room_actor::{Join, LeaveAttemptId, LeaveByRealJid, LeaveOrigin, LeaveSessionSelector},
        room_registry_actor::CreateRoom,
    },
    xep::xep0334::{add_hint, Hint},
};

struct Case {
    fixture: IngressFixture,
    state: Arc<crate::server::routes::websocket::WebSocketState>,
    actor: ActorRef<RoomActor>,
    submission: IngressSubmission,
}

impl Case {
    async fn new(hint: Hint) -> Self {
        let fixture = IngressFixture::sqlite().await;
        let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
        let mut submission = room_submission(&fixture);
        let waddle_xmpp::ingress::NormalizedTarget::Bare(room) = &submission.target else {
            panic!("room target");
        };
        let room = room.clone();
        let actor = state
            .deps
            .protocol
            .room_registry
            .ask(CreateRoom {
                room_jid: room.clone(),
                waddle_id: "transient-reflection".into(),
                channel_id: "transient-reflection".into(),
                config: Default::default(),
            })
            .await
            .expect("room");
        for (jid, nick) in [
            (submission.sender.clone(), "romeo"),
            ("alice@example.com/phone".parse().expect("peer"), "alice"),
        ] {
            actor
                .ask(Join {
                    session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
                    nick: nick.into(),
                    real_jid: jid,
                    role: waddle_xmpp::Role::Participant,
                    affiliation: waddle_xmpp::Affiliation::Member,
                })
                .await
                .expect("joined");
        }
        let snapshot = actor
            .ask(GetRoomSnapshot {
                sender_jid: submission.sender.clone(),
            })
            .await
            .expect("snapshot");
        if let RoomExecutionPath::Local {
            manifest_snapshot,
            snapshot_generation,
            ..
        } = &mut submission.plan.room_execution
        {
            *manifest_snapshot = Some(RoomManifestSnapshot::from(&snapshot));
            *snapshot_generation = snapshot.admission_revision;
        }
        add_hint(&mut submission.plan.sanitized_message, hint);
        add_hint(
            submission
                .plan
                .room_canonical_message
                .as_mut()
                .expect("canonical"),
            hint,
        );
        submission.digest_input = DigestInput::from_parsed(
            &submission.plan.sanitized_message,
            &DigestContext {
                target: submission.target.clone(),
                server_authorities: vec![room],
                stanza_lang: None,
            },
        )
        .expect("hinted digest");
        let mut echo = submission
            .plan
            .room_canonical_message
            .as_ref()
            .expect("canonical")
            .as_ref()
            .clone();
        echo.to = Some(submission.sender.clone().into());
        let mut frame = PlannedEffect::new(Effect::External(ExternalEffect::Frame(Box::new(
            Stanza::Message(echo.clone()),
        ))));
        frame.reflection_delivery = Some(Box::new(ExternalDeliveryEffect::RouteToPeer {
            route_identity: None,
            jid: submission.sender.clone(),
            stanza: Box::new(Stanza::Message(echo)),
            kind: PeerDeliveryKind::PeerStanza,
            call_setup: None,
        }));
        submission.plan.plan = vec![frame];
        Self {
            fixture,
            state,
            actor,
            submission,
        }
    }

    fn deps(&self) -> Deps<'_> {
        let mut deps = Deps::new(&self.state.deps.protocol.connection_registry, "example.com");
        deps.room_registry = Some(&self.state.deps.protocol.room_registry);
        deps.web_socket_state = Some(&self.state);
        deps
    }
}

#[tokio::test]
async fn xep0045_transient_initial_local_sender_delivery_uses_one_guarded_frame() {
    for hint in [Hint::NoStore, Hint::NoPermanentStore] {
        let mut case = Case::new(hint).await;
        let planned = &mut case.submission.plan.plan[0];
        let delivery = planned
            .reflection_delivery
            .take()
            .expect("local sender resource resolution");
        planned.effect = Effect::External(ExternalEffect::Delivery(*delivery));
        let decision = commit_submission(&case.fixture.uow, &case.submission, 1)
            .await
            .expect("initial local acceptance");
        let [ExternalEffect::Frame(original)] = decision.external.as_slice() else {
            panic!("local sender sink becomes the one initial response Frame");
        };
        assert!(decision.external_receipts[0].is_empty());
        let report = execute_effects(
            &case.fixture.uow,
            &case.fixture.db,
            &decision,
            &ImmediateSink,
            &case.deps(),
            Duration::from_secs(2),
        )
        .await;
        assert_eq!(report.frame_obligations.len(), 1);
        assert_eq!(report.frame_obligations[0].frames.len(), 1);
        let (Stanza::Message(actual), Stanza::Message(expected)) =
            (&report.frame_obligations[0].frames[0], original.as_ref())
        else {
            panic!("canonical initial reflection")
        };
        assert_eq!(actual, expected);
        assert!(report.frame_obligations[0].receipt_keys.is_empty());
        assert_eq!(case.fixture.count("sm_ingress_appends").await, 0);
        assert_eq!(case.fixture.count("ingress_send_attempts").await, 0);
        assert_eq!(case.fixture.count("ingress_effect_receipts WHERE policy_discard_reason = 'storage_hint_forbids_handoff'").await, 1);
        let retry = commit_submission(&case.fixture.uow, &case.submission, 1)
            .await
            .expect("local retry");
        assert!(
            retry.external.is_empty(),
            "settled sender metadata has no historical replay"
        );
        case.fixture.close().await;
    }
}

#[tokio::test]
async fn xep0045_transient_initial_no_store_and_no_permanent_store_echo_once_without_keyed_append()
{
    for hint in [Hint::NoStore, Hint::NoPermanentStore] {
        let case = Case::new(hint).await;
        let decision = commit_submission(&case.fixture.uow, &case.submission, 1)
            .await
            .expect("initial acceptance");
        assert_eq!(
            decision.external.len(),
            1,
            "initial transient frame remains"
        );
        assert!(
            decision.external_receipts[0].is_empty(),
            "policy disposition is not append authority"
        );
        let report = execute_effects(
            &case.fixture.uow,
            &case.fixture.db,
            &decision,
            &ImmediateSink,
            &case.deps(),
            Duration::from_secs(2),
        )
        .await;
        assert_eq!(report.frame_obligations.len(), 1);
        let [Stanza::Message(message)] = report.frame_obligations[0].frames.as_slice() else {
            panic!("one typed room echo");
        };
        let mut expected = case
            .submission
            .plan
            .room_canonical_message
            .as_ref()
            .expect("canonical")
            .as_ref()
            .clone();
        expected.to = Some(case.submission.sender.clone().into());
        assert_eq!(message, &expected);
        assert!(report.frame_obligations[0].receipt_keys.is_empty());
        assert_eq!(case.fixture.count("ingress_send_attempts").await, 0);
        assert_eq!(case.fixture.count("sm_ingress_appends").await, 0);
        assert_eq!(case.fixture.count("ingress_effect_receipts WHERE policy_discard_reason = 'storage_hint_forbids_handoff'").await, 1);
        let key = decision.message_key.expect("key");
        let mut tx = case.fixture.uow.begin().await.expect("metadata");
        let original = CanonicalMessageRepository::load_envelope(&mut tx, key)
            .await
            .expect("envelope")
            .expect("canonical");
        let intents = EffectIntentRepository::load(&mut tx, key)
            .await
            .expect("intents");
        assert!(intents.iter().any(|intent| matches!(intent, IngressEffectIntent::RouteDirect { prepared: None, fanout, .. }
            if fanout.as_slice() == [case.submission.sender.clone()])));
        tx.commit().await.expect("metadata commit");
        let retry = commit_submission(&case.fixture.uow, &case.submission, 1)
            .await
            .expect("alias retry");
        assert!(
            retry
                .external
                .iter()
                .all(|effect| !matches!(effect, ExternalEffect::Frame(_))),
            "no second echo"
        );
        let mut sibling = case.submission.clone();
        sibling.sender = sibling
            .sender
            .to_bare()
            .with_resource_str("mobile")
            .expect("sibling");
        if let Effect::External(ExternalEffect::Frame(stanza)) = &mut sibling.plan.plan[0].effect {
            let Stanza::Message(message) = stanza.as_mut() else {
                panic!("frame");
            };
            message.to = Some(sibling.sender.clone().into());
        }
        let sibling_retry = commit_submission(&case.fixture.uow, &sibling, 1)
            .await
            .expect("sibling retry");
        assert!(
            sibling_retry
                .external
                .iter()
                .all(|effect| !matches!(effect, ExternalEffect::Frame(_))),
            "no sibling echo"
        );
        let authority = case.fixture.authority().await;
        let mut deps = case.deps();
        deps.delivery_execution_context = DeliveryExecutionContext::MaintenanceRecovery;
        let recovered = execute_effects(
            &case.fixture.uow,
            &case.fixture.db,
            &decision,
            &ImmediateSink,
            &deps,
            Duration::from_secs(2),
        )
        .await;
        assert!(
            recovered.frame_obligations.is_empty(),
            "restart/recovery cannot release retained initial decision"
        );
        authority.drain_and_join(Duration::from_secs(1)).await;
        let mut tx = case.fixture.uow.begin().await.expect("stable metadata");
        let restored = CanonicalMessageRepository::load_envelope(&mut tx, key)
            .await
            .expect("envelope")
            .expect("canonical");
        assert_eq!(restored.room_manifest(), original.room_manifest());
        tx.commit().await.expect("metadata commit");
        case.fixture.close().await;
    }
}

#[tokio::test]
async fn xep0045_transient_initial_echo_rejects_leave_rejoin_and_success_receipt_without_policy() {
    let case = Case::new(Hint::NoStore).await;
    let decision = commit_submission(&case.fixture.uow, &case.submission, 1)
        .await
        .expect("initial acceptance");
    case.actor
        .ask(LeaveByRealJid {
            sender_jid: case.submission.sender.clone(),
            cause: waddle_xmpp::muc::durable::OccupancyLeaveCause::Explicit,
            session: LeaveSessionSelector::Any,
            attempt: LeaveAttemptId::generate(),
            origin: LeaveOrigin::Fresh,
        })
        .await
        .expect("leave");
    case.actor
        .ask(Join {
            session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            nick: "new-romeo".into(),
            real_jid: case.submission.sender.clone(),
            role: waddle_xmpp::Role::Participant,
            affiliation: waddle_xmpp::Affiliation::Member,
        })
        .await
        .expect("rejoin");
    let denied = execute_effects(
        &case.fixture.uow,
        &case.fixture.db,
        &decision,
        &ImmediateSink,
        &case.deps(),
        Duration::from_secs(2),
    )
    .await;
    assert!(
        denied.frame_obligations.is_empty(),
        "old occupancy cannot release initial frame"
    );
    case.fixture.close().await;

    let case = Case::new(Hint::NoPermanentStore).await;
    let decision = commit_submission(&case.fixture.uow, &case.submission, 1)
        .await
        .expect("initial acceptance");
    case.fixture.execute("UPDATE ingress_effect_receipts SET policy_discard_reason = NULL WHERE policy_discard_reason = 'storage_hint_forbids_handoff'", ()).await;
    let denied = execute_effects(
        &case.fixture.uow,
        &case.fixture.db,
        &decision,
        &ImmediateSink,
        &case.deps(),
        Duration::from_secs(2),
    )
    .await;
    assert!(
        denied.frame_obligations.is_empty(),
        "a completed receipt is not storage-policy authority"
    );
    let mut tx = case.fixture.uow.begin().await.expect("inspect receipt");
    let key = decision.message_key.expect("key");
    let envelope = CanonicalMessageRepository::load_envelope(&mut tx, key)
        .await
        .expect("envelope")
        .expect("canonical");
    let reflection = envelope
        .room_manifest()
        .expect("manifest")
        .recorded_intents()
        .iter()
        .find(|intent| matches!(intent, IngressEffectIntent::RouteDirect { .. }))
        .expect("reflection");
    let receipt = crate::ingress::receipt_key(reflection).expect("receipt");
    assert!(EffectReceiptRepository::contains(
        &mut tx,
        key,
        receipt.kind,
        &receipt.semantic_identity_hash
    )
    .await
    .expect("success receipt remains"));
    tx.commit().await.expect("inspect commit");
    case.fixture.close().await;
}

/// Seed the historical persisted shape: accepted room routing authority, no
/// room manifest, and no sender reflection obligation. The synthetic metadata
/// removal is fixture setup; retries and recovery use their real interfaces.
async fn seed_legacy_archive_free(case: &Case) -> waddle_xmpp::ingress::MessageKey {
    let mut legacy = case.submission.clone();
    legacy.plan.room_execution = RoomExecutionPath::None;
    let accepted = commit_submission(&case.fixture.uow, &legacy, 1)
        .await
        .expect("legacy acceptance");
    let key = accepted.message_key.expect("legacy key");
    let kind = waddle_xmpp::ingress::IngressEffectKind::RouteDirect.storage_tag();
    case.fixture
        .execute(
            "DELETE FROM ingress_effect_receipts WHERE message_key = ? AND kind = ?",
            crate::db_params![key.to_storage().to_string(), kind],
        )
        .await;
    case.fixture
        .execute(
            "DELETE FROM ingress_effect_intents WHERE message_key = ? AND kind = ?",
            crate::db_params![key.to_storage().to_string(), kind],
        )
        .await;
    let mut tx = case.fixture.uow.begin().await.expect("legacy shape");
    let envelope = CanonicalMessageRepository::load_envelope(&mut tx, key)
        .await
        .expect("legacy envelope")
        .expect("legacy canonical");
    assert!(envelope.room_manifest().is_none());
    let recorded = EffectIntentRepository::load(&mut tx, key)
        .await
        .expect("legacy intents");
    assert_eq!(recorded.len(), 1);
    assert!(matches!(
        recorded[0],
        IngressEffectIntent::RouteMucGroupchat { .. }
    ));
    tx.commit().await.expect("legacy shape commit");
    key
}

#[tokio::test]
async fn xep0045_legacy_archive_free_storage_hints_never_acquire_replayable_reflection() {
    for hint in [Hint::NoStore, Hint::NoPermanentStore] {
        let case = Case::new(hint).await;
        let key = seed_legacy_archive_free(&case).await;
        for sibling in [false, true] {
            let mut retry = case.submission.clone();
            if sibling {
                retry.sender = retry
                    .sender
                    .to_bare()
                    .with_resource_str("mobile")
                    .expect("sibling");
                let Effect::External(ExternalEffect::Frame(stanza)) =
                    &mut retry.plan.plan[0].effect
                else {
                    panic!("current sender frame");
                };
                let Stanza::Message(message) = stanza.as_mut() else {
                    panic!("room frame");
                };
                message.to = Some(retry.sender.clone().into());
            }
            let decision = commit_submission(&case.fixture.uow, &retry, 1)
                .await
                .expect("legacy retry");
            assert_eq!(decision.message_key, Some(key));
            assert!(
                decision
                    .route_progress
                    .iter()
                    .all(|progress| !progress.is_direct()),
                "retry cannot invent a persistent sender obligation"
            );
            let mut tx = case.fixture.uow.begin().await.expect("retry authority");
            let envelope = CanonicalMessageRepository::load_envelope(&mut tx, key)
                .await
                .expect("envelope")
                .expect("canonical");
            assert!(
                envelope.room_manifest().is_none(),
                "legacy authority is never retrofitted"
            );
            let recorded = EffectIntentRepository::load(&mut tx, key)
                .await
                .expect("intents");
            let receipts = EffectReceiptRepository::keys(&mut tx, key)
                .await
                .expect("receipt evidence");
            let pending = recorded
                .iter()
                .filter(|intent| {
                    !receipts.contains(&crate::ingress::receipt_key(intent).expect("receipt key"))
                })
                .cloned()
                .collect::<Vec<_>>();
            let created_at = CanonicalMessageRepository::created_at(&mut tx, key)
                .await
                .expect("acceptance time");
            assert_eq!(
                recorded.len(),
                1,
                "no synthetic RouteDirect on same or sibling retry"
            );
            assert!(recorded
                .iter()
                .all(|intent| !matches!(intent, IngressEffectIntent::RouteDirect { .. })));
            tx.commit().await.expect("retry authority commit");
            let recovery = crate::ingress::recovery::rebuild::rebuild(
                crate::ingress::recovery::rebuild::RecoveryInput {
                    key,
                    envelope: &envelope,
                    created_at,
                    recorded: &recorded,
                    unreceipted: &pending,
                    route_progress: decision.route_progress,
                    host_owned_resources: Vec::new(),
                    departed_occupants: Vec::new(),
                    blocked_recipients: &[],
                },
            )
            .expect("maintenance rebuild");
            assert!(
                recovery.decision.external.iter().all(|effect| {
                    !matches!(effect, ExternalEffect::Frame(_))
                        && crate::ingress::recorded::single_target(effect)
                            != Some(&case.submission.sender)
                        && crate::ingress::recorded::single_target(effect) != Some(&retry.sender)
                }),
                "maintenance cannot reconstruct a historical sender echo"
            );
            let mut deps = case.deps();
            deps.delivery_execution_context = DeliveryExecutionContext::MaintenanceRecovery;
            let report = execute_effects(
                &case.fixture.uow,
                &case.fixture.db,
                &recovery.decision,
                &ImmediateSink,
                &deps,
                Duration::from_secs(2),
            )
            .await;
            assert!(report.frame_obligations.is_empty());
        }
        case.fixture.close().await;
    }
}
