//! XEP-0198/XEP-0334: transient full-JID copies are never rebuilt by ingress recovery.
use super::*;
use crate::server::routes::interpret::plan_message_dispatch;
use waddle_xmpp::protocol::{StanzaDispatcher, XmppStateMachine};
use waddle_xmpp::xep::xep0334::{add_hint, Hint};

#[derive(Clone, Copy)]
enum RecipientState {
    Resumable,
    Gone,
    GoneThenLive,
    Blocked,
    AlreadyDelivered,
    BareGone,
    #[cfg(feature = "clustering")]
    GoneWithClaim(ClaimState),
}

#[cfg(feature = "clustering")]
#[derive(Clone, Copy)]
enum ClaimState {
    Local,
    Foreign,
    Missing,
    Stale,
}

async fn prepared_no_store_recovery(fixture: IngressFixture, recipient_state: RecipientState) {
    #[cfg(feature = "clustering")]
    let fixture = if let RecipientState::GoneWithClaim(claim) = recipient_state {
        let mut fixture = fixture;
        let fence = fixture
            .room_fence(&"policy-test@muc.example.com".parse().expect("room"))
            .await;
        let owner = if matches!(claim, ClaimState::Foreign) {
            waddle_xmpp::ownership::NodeIdentity::new("remote-owner", "remote-incarnation")
        } else {
            fence.owner
        };
        fixture
            .execute(
                "INSERT INTO clustering_nodes (node_id, node_epoch, expired) VALUES (?, ?, ? <> 0)",
                crate::db_params![
                    owner.node_id.clone(),
                    owner.node_epoch.clone(),
                    matches!(claim, ClaimState::Stale)
                ],
            )
            .await;
        if !matches!(claim, ClaimState::Missing) {
            fixture.execute("INSERT INTO clustering_claims (entity, entity_type, node_id, node_epoch, claim_epoch) VALUES (?, ?, ?, ?, ?)",
                crate::db_params!["user_actor:juliet@example.com", "user_actor", owner.node_id, owner.node_epoch, 23_i64]).await;
        }
        fixture
    } else {
        fixture
    };
    let sm = persistent_sm(&fixture).await;
    let target: jid::FullJid = "juliet@example.com/phone".parse().expect("target");
    store_detached(&sm, &target).await;
    let mut state = state_for(&fixture, sm.clone()).await;
    let blocking = Arc::new(waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new());
    let protocol = &mut Arc::get_mut(&mut state)
        .expect("unique state")
        .deps
        .protocol;
    protocol.blocking_storage = blocking.clone();
    protocol.pending_delivery_storage = Arc::new(
        crate::pending_delivery::DatabasePendingDeliveryStorage::from_database(
            fixture.db.clone(),
            waddle_xmpp::pending_delivery::QuotaPolicy::Unlimited,
        )
        .await
        .expect("pending storage"),
    );
    socket_tests::create_test_session(&state, "juliet").await;
    let mut submission = fixture.submission(Some("prepared-no-store"), "frozen transient copy");
    retarget(
        &mut submission,
        if matches!(recipient_state, RecipientState::BareGone) {
            NormalizedTarget::Bare(target.to_bare())
        } else {
            NormalizedTarget::Full(target.clone())
        },
        xmpp_parsers::message::MessageType::Chat,
    );
    let mut message = submission.plan.sanitized_message.clone();
    add_hint(
        &mut message,
        if matches!(
            recipient_state,
            RecipientState::AlreadyDelivered | RecipientState::Blocked
        ) {
            Hint::NoPermanentStore
        } else {
            Hint::NoStore
        },
    );
    if !matches!(
        recipient_state,
        RecipientState::AlreadyDelivered | RecipientState::BareGone | RecipientState::Blocked
    ) {
        message
            .subjects
            .insert(Default::default(), "transient subject".into());
    }
    let offered = message.clone();
    let mut dispatcher = StanzaDispatcher::new();
    waddle_xmpp::protocol::handlers::register_default_message_handlers(&mut dispatcher);
    let mut machine = XmppStateMachine::new("example.com", dispatcher);
    machine.transition_to_ready(submission.sender.clone(), false);
    submission.plan =
        plan_message_dispatch(&mut machine, message, &build_interpret_deps(&state, None)).await;
    assert!(submission.plan.failure.is_none());
    assert!(submission.plan.rejection.is_none());
    let expected = submission
        .plan
        .plan
        .iter()
        .find_map(|planned| match &planned.effect {
            Effect::External(ExternalEffect::Delivery(ExternalDeliveryEffect::QueueDetached {
                resources,
                stanza,
                ..
            })) if resources == std::slice::from_ref(&target) => Some(stanza.clone()),
            _ => None,
        })
        .expect("production planner captured the prepared target copy");
    let decision = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("accept");
    let key = decision.message_key.expect("canonical key");
    assert!(decision.class.advances());
    assert_eq!(
        append_count(&sm, &target).await,
        0,
        "commit has no delivery effect"
    );
    assert_eq!(
        fixture.count("mam_messages").await,
        0,
        "no-store forbids MAM"
    );
    let transient = !matches!(
        recipient_state,
        RecipientState::AlreadyDelivered | RecipientState::BareGone | RecipientState::Blocked
    );
    let mut tx = fixture.uow.begin().await.expect("committed authority");
    let received_at = CanonicalMessageRepository::created_at(&mut tx, key)
        .await
        .expect("original receipt time");
    if transient {
        assert!(CanonicalMessageRepository::is_terminal(&mut tx, key)
            .await
            .expect("terminal"));
        assert!(EffectReceiptRepository::receipts_complete(&mut tx, key)
            .await
            .expect("receipts"));
        let envelope = CanonicalMessageRepository::load_envelope(&mut tx, key)
            .await
            .expect("envelope read")
            .expect("header envelope");
        assert!(
            envelope.message().bodies.is_empty(),
            "canonical ledger must not retain the transient body"
        );
        assert!(
            envelope.message().subjects.is_empty(),
            "canonical ledger must not retain transient subjects"
        );
        assert!(waddle_xmpp::xep::xep0334::has_hint(
            envelope.message(),
            Hint::NoStore
        ));
        let intents = crate::ingress_uow::EffectIntentRepository::load(&mut tx, key)
            .await
            .expect("recorded intents");
        let route = intents
            .iter()
            .find(|intent| {
                matches!(intent,
                    IngressEffectIntent::RouteDirect { fanout, .. } if fanout.as_slice() == std::slice::from_ref(&target)
                )
            })
            .expect("recorded exact target");
        assert!(matches!(
            route,
            IngressEffectIntent::RouteDirect { prepared: None, .. }
        ));
        for intent in &intents {
            intent
                .with_encoded_v1(|_, payload| {
                    for secret in [
                        b"frozen transient copy".as_slice(),
                        b"transient subject".as_slice(),
                    ] {
                        assert!(
                            !payload.windows(secret.len()).any(|part| part == secret),
                            "intent codec retained transient content"
                        );
                    }
                })
                .expect("inspect stored codec");
        }
    }
    tx.commit().await.expect("authority read");
    if transient {
        assert_eq!(fixture.count("ingress_effect_receipts WHERE policy_discard_reason = 'storage_hint_forbids_handoff'").await, 1);
        if matches!(recipient_state, RecipientState::Resumable) {
            // Initial execution can be delayed after acceptance. Ordinary SM
            // custody still owns the original receipt time, not this enqueue.
            tokio::time::sleep(Duration::from_millis(25)).await;
            let deps = build_interpret_deps(&state, None);
            let report = execute_effects(
                &fixture.uow,
                &fixture.db,
                &decision,
                &ImmediateSink,
                &deps,
                Duration::from_secs(5),
            )
            .await;
            assert!(report.receipt_failures.is_empty());
            let retained = sm
                .peek_session(&target.to_string())
                .await
                .expect("live SM read")
                .expect("live SM custody");
            assert_eq!(retained.unacked_stanzas[0].original_receipt_at, received_at);
            assert_eq!(
                append_count(&sm, &target).await,
                usize::from(matches!(recipient_state, RecipientState::Resumable))
            );
        }
        // A sender retry can contain another fresh recipient stamp, but the
        // terminal ledger must not manufacture another execution opportunity.
        submission.plan =
            plan_message_dispatch(&mut machine, offered, &build_interpret_deps(&state, None)).await;
        let retry = commit_submission(&fixture.uow, &submission, 5)
            .await
            .expect("retry acceptance");
        assert_eq!(retry.message_key, Some(key));
        assert!(!retry.external.iter().any(|effect| matches!(effect,
            ExternalEffect::Delivery(ExternalDeliveryEffect::QueueDetached { resources, .. }) if resources.contains(&target)
        )), "retry must not resend a terminal transient route");
        drop(retry);
    } else {
        let progress = decision
            .route_progress
            .iter()
            .find(|progress| progress.fanout == [target.clone()])
            .expect("frozen direct route");
        let context = crate::server::routes::interpret::SmIngressAppendContext {
            message_key: key,
            receipt: progress.receipt.clone(),
            received_at: progress.received_at,
            archive_positions: vec![],
            dispatch_stream: None,
            authority: crate::ingress::append_authority::AppendAuthority::Verified,
        };
        let authority = fixture.authority().await;
        for change_stamp in [false, true] {
            let mut forged = expected.as_ref().clone();
            let Stanza::Message(message) = &mut forged else {
                panic!("message")
            };
            if change_stamp {
                waddle_xmpp_core::xep0359::add_stanza_id(
                    message,
                    &waddle_xmpp_core::xep0359::StanzaId::new(
                        "forged-recipient-stamp",
                        target.to_bare().into(),
                    ),
                );
            } else {
                message
                    .bodies
                    .insert(Default::default(), "forged body".into());
            }
            assert_eq!(
                authority
                    .accept_live_delivery(&context, &target, &forged, || panic!(
                        "forged copy reached sink"
                    ))
                    .await,
                crate::server::routes::interpret::FullJidDeliveryOutcome::MaybeCommitted
            );
            let relayed = crate::ingress::identity::IngressAppendObligationRef::for_message(
                Some(&context),
                &forged,
            )
            .expect("relayed identity");
            assert!(
                crate::ingress::append_authority::check_canonical_obligation(
                    &fixture.db,
                    &forged,
                    &relayed
                )
                .await
                .is_err()
            );
        }
        assert_eq!(fixture.count("ingress_send_attempts").await, 0);
        submission.plan =
            plan_message_dispatch(&mut machine, offered, &build_interpret_deps(&state, None)).await;
        let retry = commit_submission(&fixture.uow, &submission, 5)
            .await
            .expect("retry prepared route");
        assert_eq!(retry.message_key, Some(key));
        let retried_copy = retry
            .external
            .iter()
            .find_map(|effect| match effect {
                ExternalEffect::Delivery(ExternalDeliveryEffect::QueueDetached {
                    resources,
                    stanza,
                    ..
                }) if resources == std::slice::from_ref(&target) => Some(stanza),
                _ => None,
            })
            .expect("unfinished stored route remains executable");
        assert_eq!(retried_copy.to_element(), expected.to_element());
        drop(retry);
        if matches!(recipient_state, RecipientState::Blocked) {
            blocking.set_blocklist(target.to_bare(), vec![submission.sender.to_bare()]);
        } else {
            sm.take_session(&target.to_string())
                .await
                .expect("session ended");
            let mut tx = fixture.uow.begin().await.expect("resource evidence");
            CanonicalMessageRepository::lock(&mut tx, key)
                .await
                .expect("canonical lock");
            if matches!(recipient_state, RecipientState::AlreadyDelivered) {
                crate::ingress_uow::DeliveryProgressRepository::record(
                    &mut tx,
                    key,
                    &progress.receipt,
                    std::slice::from_ref(&target),
                )
                .await
                .expect("record direct proof");
            } else {
                let obligation = crate::ingress_uow::SendObligation {
                    message: key,
                    receipt: progress.receipt.clone(),
                    recipient: target.clone(),
                };
                let crate::ingress_uow::SendClaim::Acquired(lease) =
                    crate::ingress_uow::SendAttemptRepository::claim(
                        &mut tx,
                        &obligation,
                        &waddle_xmpp::ownership::NodeIdentity::local(),
                        Duration::from_secs(5),
                    )
                    .await
                    .expect("lease")
                else {
                    panic!("new claim")
                };
                assert!(
                    crate::ingress_uow::SendAttemptRepository::start(&mut tx, &lease)
                        .await
                        .expect("start")
                );
            }
            tx.commit().await.expect("evidence commit");
            fixture
                .execute("UPDATE ingress_send_attempts SET expires_at_ms = 0", ())
                .await;
        }
    }
    // Crash after commit: maintenance can only see terminal route metadata,
    // never the process-local copy owned by the initial decision.
    drop(decision);
    let disappeared = matches!(
        recipient_state,
        RecipientState::Gone | RecipientState::GoneThenLive
    );
    #[cfg(feature = "clustering")]
    let disappeared = disappeared || matches!(recipient_state, RecipientState::GoneWithClaim(_));
    if disappeared {
        sm.take_session(&target.to_string())
            .await
            .expect("original session ended");
    }
    let newcomer: jid::FullJid = "juliet@example.com/newcomer".parse().expect("new resource");
    store_detached(&sm, &newcomer).await;
    let env: Arc<dyn RecoveryEnvironment> = Arc::new(StateEnvironment(state.clone()));
    for _ in 0..2 {
        assert_eq!(
            pass(&fixture, &env, &MaintenanceCursor::default()).await,
            MaintenanceOutcome::Complete
        );
        let mut tx = fixture.uow.begin().await.expect("maintenance result");
        assert!(CanonicalMessageRepository::is_terminal(&mut tx, key)
            .await
            .expect("terminal read"));
        tx.commit().await.expect("result read");
    }
    if matches!(recipient_state, RecipientState::Resumable) {
        // Reconstitute only the ordinary XEP-0198 session. No keyed ingress
        // append or recovery sweep may create another copy.
        let restarted = persistent_sm(&fixture).await;
        restarted
            .restore_from_persistence()
            .await
            .expect("restore native SM custody");
        let session = restarted
            .peek_session(&target.to_string())
            .await
            .expect("restored SM")
            .expect("session");
        assert_eq!(session.unacked_stanzas.len(), 1);
        // Native sm_unacked stores original_receipt_at_ms, not microseconds.
        assert_eq!(
            session.unacked_stanzas[0]
                .original_receipt_at
                .timestamp_millis(),
            received_at.timestamp_millis()
        );
        let replay: minidom::Element = session.unacked_stanzas[0]
            .stanza_xml
            .parse()
            .expect("replay XML");
        assert_eq!(replay, expected.to_element());
        assert_eq!(append_count(&sm, &target).await, 1);
    }
    if matches!(recipient_state, RecipientState::Gone) {
        store_detached(&sm, &target).await;
        assert_eq!(
            pass(&fixture, &env, &MaintenanceCursor::default()).await,
            MaintenanceOutcome::Complete
        );
        assert_eq!(
            append_count(&sm, &target).await,
            0,
            "new session must not receive a prior transient copy"
        );
    }
    let rebind_live = matches!(recipient_state, RecipientState::GoneThenLive);
    #[cfg(feature = "clustering")]
    let rebind_live = rebind_live || matches!(recipient_state, RecipientState::GoneWithClaim(_));
    if rebind_live {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(8);
        socket_tests::register_test_connection(&state, &target, sender).await;
        assert_eq!(
            pass(&fixture, &env, &MaintenanceCursor::default()).await,
            MaintenanceOutcome::Complete
        );
        assert!(
            receiver.try_recv().is_err(),
            "same-owner rebind cannot resurrect a terminal transient route"
        );
    }
    assert_eq!(
        fixture.count("sm_ingress_appends").await,
        0,
        "ordinary SM replay is the only payload custody"
    );
    assert_eq!(fixture.count("mam_messages").await, 0);
    assert_eq!(fixture.count("pending_delivery").await, 0);
    assert_eq!(append_count(&sm, &newcomer).await, 0);
    assert_eq!(
        fixture.count("ingress_delivery_receipts").await,
        i64::from(matches!(recipient_state, RecipientState::AlreadyDelivered))
    );
    assert_eq!(fixture.count("ingress_effect_receipts WHERE policy_discard_reason = 'storage_hint_forbids_handoff'").await, i64::from(transient));
    assert_eq!(
        fixture
            .count("ingress_effect_receipts WHERE policy_discard_reason = 'recipient_blocked'")
            .await,
        i64::from(matches!(recipient_state, RecipientState::Blocked))
    );
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_xep0198_xep0334_prepared_full_no_store_replays_only_native_sm_custody() {
    prepared_no_store_recovery(IngressFixture::sqlite().await, RecipientState::Resumable).await;
}

#[tokio::test]
async fn postgres_xep0198_xep0334_prepared_full_no_store_replays_only_native_sm_custody() {
    if let Some(fixture) = IngressFixture::postgres("prp_nostore").await {
        prepared_no_store_recovery(fixture, RecipientState::Resumable).await;
    }
}

#[tokio::test]
async fn sqlite_xep0334_prepared_full_no_store_gone_target_is_terminal_before_recovery() {
    prepared_no_store_recovery(IngressFixture::sqlite().await, RecipientState::Gone).await;
}

#[tokio::test]
async fn postgres_xep0334_prepared_full_no_store_gone_target_is_terminal_before_recovery() {
    if let Some(fixture) = IngressFixture::postgres("prp_gone").await {
        prepared_no_store_recovery(fixture, RecipientState::Gone).await;
    }
}

#[tokio::test]
async fn sqlite_prepared_full_no_store_never_delivers_after_later_live_rebind() {
    prepared_no_store_recovery(IngressFixture::sqlite().await, RecipientState::GoneThenLive).await;
}

#[tokio::test]
async fn postgres_prepared_full_no_store_never_delivers_after_later_live_rebind() {
    if let Some(fixture) = IngressFixture::postgres("prp_rebind").await {
        prepared_no_store_recovery(fixture, RecipientState::GoneThenLive).await;
    }
}

#[tokio::test]
async fn sqlite_xep0191_prepared_full_temporary_storage_blocked_target_discards() {
    prepared_no_store_recovery(IngressFixture::sqlite().await, RecipientState::Blocked).await;
}

#[tokio::test]
async fn postgres_xep0191_prepared_full_temporary_storage_blocked_target_discards() {
    if let Some(fixture) = IngressFixture::postgres("prp_block").await {
        prepared_no_store_recovery(fixture, RecipientState::Blocked).await;
    }
}

async fn oversized_prepared_copy(fixture: IngressFixture) {
    use crate::ingress::IngressStreamIdentity;
    use crate::ingress_uow::{IngressUowError, SmIngressStreamRepository};
    use waddle_xmpp::ingress::{
        EffectIntentCodecError, WireHandledCount, MAX_EFFECT_INTENT_PAYLOAD_BYTES,
    };
    let sm = persistent_sm(&fixture).await;
    let target: jid::FullJid = "juliet@example.com/phone".parse().expect("target");
    store_detached(&sm, &target).await;
    let state = state_for(&fixture, sm.clone()).await;
    socket_tests::create_test_session(&state, "juliet").await;
    let mut submission = fixture.submission(
        Some("oversized-prepared"),
        &"x".repeat(MAX_EFFECT_INTENT_PAYLOAD_BYTES),
    );
    retarget(
        &mut submission,
        NormalizedTarget::Full(target.clone()),
        xmpp_parsers::message::MessageType::Chat,
    );
    let mut offered = submission.plan.sanitized_message.clone();
    add_hint(&mut offered, Hint::NoStore);
    let mut dispatcher = StanzaDispatcher::new();
    waddle_xmpp::protocol::handlers::register_default_message_handlers(&mut dispatcher);
    let mut machine = XmppStateMachine::new("example.com", dispatcher);
    machine.transition_to_ready(submission.sender.clone(), false);
    submission.plan =
        plan_message_dispatch(&mut machine, offered, &build_interpret_deps(&state, None)).await;
    assert!(submission.plan.failure.is_none());
    let stream_id = waddle_xmpp::pending_delivery::SmSessionId::new("oversized-prepared");
    let mut tx = fixture.uow.begin().await.expect("stream enrollment");
    let sm_ingress_id = SmIngressStreamRepository::mint(&mut tx, &stream_id)
        .await
        .expect("stream");
    tx.commit().await.expect("enroll stream");
    submission.identity = IngressStreamIdentity::Resumable {
        stream_id,
        sm_ingress_id,
        #[cfg(feature = "clustering")]
        owner: waddle_xmpp::ownership::NodeIdentity::new("unused", "single-node"),
        #[cfg(feature = "clustering")]
        claim_epoch: waddle_xmpp::ownership::ClaimEpoch(1),
        reserved_wire_position: WireHandledCount::new(1),
        checkpoint_h: WireHandledCount::new(1),
    };
    let failure = match commit_submission(&fixture.uow, &submission, 1).await {
        Err(failure) => failure,
        Ok(_) => panic!("oversized intent must refuse responsibility"),
    };
    assert!(!failure.class.advances());
    assert!(matches!(
        failure.source,
        IngressUowError::EffectIntentCodec(EffectIntentCodecError::PayloadTooLarge)
    ));
    assert_eq!(fixture.count("ingress_messages").await, 0);
    assert_eq!(fixture.count("ingress_effect_intents").await, 0);
    assert_eq!(fixture.count("ingress_sm_refs").await, 0);
    assert_eq!(
        fixture
            .count("ingress_sm_streams WHERE handled_ordinal = 0 AND checkpoint_h = 0")
            .await,
        1
    );
    assert_eq!(append_count(&sm, &target).await, 0);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_prepared_full_no_store_oversized_payload_never_advances_h() {
    oversized_prepared_copy(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_prepared_full_no_store_oversized_payload_never_advances_h() {
    if let Some(fixture) = IngressFixture::postgres("prp_big").await {
        oversized_prepared_copy(fixture).await;
    }
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn postgres_prepared_full_no_store_terminal_policy_ignores_later_claim_and_rebind() {
    for claim in [
        ClaimState::Local,
        ClaimState::Foreign,
        ClaimState::Missing,
        ClaimState::Stale,
    ] {
        if let Some(fixture) = IngressFixture::postgres("prp_claim").await {
            prepared_no_store_recovery(fixture, RecipientState::GoneWithClaim(claim)).await;
        }
    }
}

#[tokio::test]
async fn sqlite_prepared_full_temporary_storage_existing_resource_proof_repairs_aggregate() {
    prepared_no_store_recovery(
        IngressFixture::sqlite().await,
        RecipientState::AlreadyDelivered,
    )
    .await;
}

#[tokio::test]
async fn postgres_prepared_full_temporary_storage_existing_resource_proof_repairs_aggregate() {
    if let Some(fixture) = IngressFixture::postgres("prp_proof").await {
        prepared_no_store_recovery(fixture, RecipientState::AlreadyDelivered).await;
    }
}

#[tokio::test]
async fn sqlite_prepared_bare_no_store_preserves_expired_attempt_policy() {
    prepared_no_store_recovery(IngressFixture::sqlite().await, RecipientState::BareGone).await;
}

#[tokio::test]
async fn postgres_prepared_bare_no_store_preserves_expired_attempt_policy() {
    if let Some(fixture) = IngressFixture::postgres("prp_bare").await {
        prepared_no_store_recovery(fixture, RecipientState::BareGone).await;
    }
}

#[derive(Clone, Copy)]
enum InitialTarget {
    Exact,
    Sibling,
    Offline,
    Blocked,
    LiveThenDetached,
}

async fn transient_live_carbons_or_offline(fixture: IngressFixture, initial: InitialTarget) {
    let initially_live = matches!(initial, InitialTarget::Exact | InitialTarget::Sibling);
    let sm = persistent_sm(&fixture).await;
    let mut state = state_for(&fixture, sm.clone()).await;
    let blocking = Arc::new(waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new());
    let protocol = &mut Arc::get_mut(&mut state)
        .expect("unique state")
        .deps
        .protocol;
    protocol.blocking_storage = blocking.clone();
    protocol.pending_delivery_storage = Arc::new(
        crate::pending_delivery::DatabasePendingDeliveryStorage::from_database(
            fixture.db.clone(),
            waddle_xmpp::pending_delivery::QuotaPolicy::Unlimited,
        )
        .await
        .expect("pending storage"),
    );
    socket_tests::create_test_session(&state, "juliet").await;
    let target: jid::FullJid = "juliet@example.com/phone".parse().expect("target");
    let mut submission = fixture.submission(Some("transient-copies"), "one transient live copy");
    retarget(
        &mut submission,
        NormalizedTarget::Full(target.clone()),
        xmpp_parsers::message::MessageType::Chat,
    );
    let sender_sibling = submission
        .sender
        .to_bare()
        .with_resource_str("carbon")
        .expect("sender sibling");
    let recipient_sibling = target
        .to_bare()
        .with_resource_str("carbon")
        .expect("recipient sibling");
    let (target_tx, mut target_rx) = tokio::sync::mpsc::channel(8);
    let mut target_tx = Some(target_tx);
    let (sender_tx, mut sender_rx) = tokio::sync::mpsc::channel(8);
    let (recipient_tx, mut recipient_rx) = tokio::sync::mpsc::channel(8);
    let (bounce_tx, mut bounce_rx) = tokio::sync::mpsc::channel(8);
    if initially_live {
        if matches!(initial, InitialTarget::Exact) {
            socket_tests::register_test_connection(
                &state,
                &target,
                target_tx.take().expect("target channel"),
            )
            .await;
        }
        socket_tests::register_test_connection(&state, &sender_sibling, sender_tx).await;
        socket_tests::register_test_connection(&state, &recipient_sibling, recipient_tx).await;
        for sibling in [&sender_sibling, &recipient_sibling] {
            assert!(state
                .deps
                .protocol
                .connection_registry
                .set_carbons_enabled(sibling, true));
        }
    }
    if matches!(
        initial,
        InitialTarget::Blocked | InitialTarget::LiveThenDetached
    ) {
        socket_tests::register_test_connection(
            &state,
            &target,
            target_tx.take().expect("target channel"),
        )
        .await;
        if matches!(initial, InitialTarget::Blocked) {
            socket_tests::register_test_connection(&state, &submission.sender, bounce_tx).await;
            blocking.set_blocklist(target.to_bare(), vec![submission.sender.to_bare()]);
        }
    }
    let mut offered = submission.plan.sanitized_message.clone();
    add_hint(&mut offered, Hint::NoStore);
    let mut dispatcher = StanzaDispatcher::new();
    waddle_xmpp::protocol::handlers::register_default_message_handlers(&mut dispatcher);
    let mut machine = XmppStateMachine::new("example.com", dispatcher);
    machine.transition_to_ready(submission.sender.clone(), false);
    let deps = build_interpret_deps(&state, None);
    submission.plan = plan_message_dispatch(&mut machine, offered.clone(), &deps).await;
    assert!(submission.plan.failure.is_none());
    if !matches!(initial, InitialTarget::Blocked) {
        assert!(submission.plan.rejection.is_none());
    }
    let first = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("transient acceptance");
    assert!(first.class.advances());
    let key = first.message_key.expect("canonical key");
    let mut tx = fixture.uow.begin().await.expect("transient authority");
    let received_at = CanonicalMessageRepository::created_at(&mut tx, key)
        .await
        .expect("canonical receipt time");
    assert!(CanonicalMessageRepository::is_terminal(&mut tx, key)
        .await
        .expect("terminal at commit"));
    assert!(EffectReceiptRepository::receipts_complete(&mut tx, key)
        .await
        .expect("no receipt backlog"));
    let envelope = CanonicalMessageRepository::load_envelope(&mut tx, key)
        .await
        .expect("envelope")
        .expect("header metadata");
    assert!(envelope.message().bodies.is_empty());
    assert!(waddle_xmpp::xep::xep0334::has_hint(
        envelope.message(),
        Hint::NoStore
    ));
    let intents = crate::ingress_uow::EffectIntentRepository::load(&mut tx, key)
        .await
        .expect("intents");
    for intent in &intents {
        assert!(!matches!(
            intent,
            IngressEffectIntent::RouteDirect {
                prepared: Some(_),
                ..
            }
        ));
        intent
            .with_encoded_v1(|_, bytes| {
                let body = b"one transient live copy";
                assert!(!bytes.windows(body.len()).any(|part| part == body));
            })
            .expect("intent codec");
    }
    tx.commit().await.expect("authority read");
    if matches!(initial, InitialTarget::LiveThenDetached) {
        assert!(first.external.iter().any(|effect| matches!(effect,
            ExternalEffect::Delivery(ExternalDeliveryEffect::RouteToPeer { jid, .. }) if jid == &target
        )), "capture selected a live peer before detach");
        state.deps.protocol.connection_registry.unregister(&target);
        state
            .deps
            .protocol
            .user_registry
            .ask(waddle_xmpp::registry::UnregisterUserResource {
                jid: target.clone(),
                owner: None,
            })
            .await
            .expect("unregister departed socket");
        store_detached(&sm, &target).await;
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &first,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert!(report.receipt_failures.is_empty());
    if matches!(initial, InitialTarget::LiveThenDetached) {
        let retained = sm
            .peek_session(&target.to_string())
            .await
            .expect("live SM read")
            .expect("live SM custody");
        assert_eq!(retained.unacked_stanzas[0].original_receipt_at, received_at);
    }
    if matches!(initial, InitialTarget::Blocked) {
        let Stanza::Message(bounce) = bounce_rx.try_recv().expect("blocked sender error").stanza
        else {
            panic!("message error")
        };
        assert_eq!(bounce.type_, xmpp_parsers::message::MessageType::Error);
        assert!(
            target_rx.try_recv().is_err(),
            "blocked recipient receives no original copy"
        );
    }
    if initially_live {
        let delivered = if matches!(initial, InitialTarget::Exact) {
            target_rx.try_recv().expect("original direct copy")
        } else {
            recipient_rx
                .try_recv()
                .expect("original same-bare fallback copy")
        };
        let Stanza::Message(delivered) = delivered.stanza else {
            panic!("message")
        };
        assert_eq!(delivered.bodies, offered.bodies);
        let mut carbon_receivers = vec![(&mut sender_rx, "sent")];
        if matches!(initial, InitialTarget::Exact) {
            carbon_receivers.push((&mut recipient_rx, "received"));
        }
        for (receiver, direction) in carbon_receivers {
            let Stanza::Message(carbon) = receiver.try_recv().expect("original carbon copy").stanza
            else {
                panic!("carbon message")
            };
            let wrapper = carbon
                .payloads
                .iter()
                .find(|payload| payload.is(direction, waddle_xmpp::carbons::CARBONS_NS))
                .expect("carbon direction");
            let forwarded = wrapper
                .get_child("forwarded", waddle_xmpp::carbons::FORWARDED_NS)
                .expect("forwarded original");
            let original = forwarded
                .get_child("message", waddle_xmpp::parser::ns::JABBER_CLIENT)
                .expect("forwarded message");
            let original = xmpp_parsers::message::Message::try_from(original.clone())
                .expect("typed carbon original");
            assert_eq!(original.bodies, offered.bodies);
        }
    }
    if matches!(initial, InitialTarget::Offline | InitialTarget::Sibling) {
        // A target absent at capture never acquires a replayable ingress copy,
        // even if the exact resource appears before the next sender attempt.
        let (late_tx, late_rx) = tokio::sync::mpsc::channel(8);
        socket_tests::register_test_connection(&state, &target, late_tx).await;
        target_rx = late_rx;
    }
    submission.plan = plan_message_dispatch(&mut machine, offered, &deps).await;
    let alias = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("terminal alias retry");
    assert_eq!(alias.message_key, Some(key));
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &alias,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert!(report.receipt_failures.is_empty());
    let env: Arc<dyn RecoveryEnvironment> = Arc::new(StateEnvironment(state.clone()));
    for _ in 0..2 {
        assert_eq!(
            pass(&fixture, &env, &MaintenanceCursor::default()).await,
            MaintenanceOutcome::Complete
        );
    }
    assert!(
        target_rx.try_recv().is_err(),
        "alias/recovery cannot resend the original copy"
    );
    assert!(
        sender_rx.try_recv().is_err(),
        "alias/recovery cannot resend sender carbons"
    );
    assert!(
        recipient_rx.try_recv().is_err(),
        "alias/recovery cannot resend recipient carbons"
    );
    if matches!(initial, InitialTarget::LiveThenDetached) {
        let restored = persistent_sm(&fixture).await;
        restored
            .restore_from_persistence()
            .await
            .expect("restore raced SM custody");
        let session = restored
            .peek_session(&target.to_string())
            .await
            .expect("restored session read")
            .expect("restored session");
        assert_eq!(
            session.unacked_stanzas.len(),
            1,
            "initial live-to-detached fallback appends once, alias and maintenance do not append"
        );
        // Native sm_unacked stores original_receipt_at_ms, not microseconds.
        assert_eq!(
            session.unacked_stanzas[0]
                .original_receipt_at
                .timestamp_millis(),
            received_at.timestamp_millis()
        );
        let replay: minidom::Element = session.unacked_stanzas[0]
            .stanza_xml
            .parse()
            .expect("restored copy");
        let replay = xmpp_parsers::message::Message::try_from(replay).expect("replayed message");
        assert_eq!(
            replay.bodies.get(&xmpp_parsers::message::Lang::default()),
            Some(&"one transient live copy".into())
        );
    }
    for table in [
        "mam_messages",
        "pending_delivery",
        "sm_ingress_appends",
        "ingress_send_attempts",
    ] {
        assert_eq!(
            fixture.count(table).await,
            0,
            "transient copy created durable work in {table}"
        );
    }
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_prepared_full_no_store_original_live_and_carbons_are_one_shot() {
    transient_live_carbons_or_offline(IngressFixture::sqlite().await, InitialTarget::Exact).await;
}

#[tokio::test]
async fn postgres_prepared_full_no_store_original_live_and_carbons_are_one_shot() {
    if let Some(fixture) = IngressFixture::postgres("transient_carbons").await {
        transient_live_carbons_or_offline(fixture, InitialTarget::Exact).await;
    }
}

#[tokio::test]
async fn sqlite_prepared_full_no_store_originally_offline_has_no_recovery_backlog() {
    transient_live_carbons_or_offline(IngressFixture::sqlite().await, InitialTarget::Offline).await;
}

#[tokio::test]
async fn postgres_prepared_full_no_store_originally_offline_has_no_recovery_backlog() {
    if let Some(fixture) = IngressFixture::postgres("transient_offline").await {
        transient_live_carbons_or_offline(fixture, InitialTarget::Offline).await;
    }
}

#[tokio::test]
async fn sqlite_prepared_full_no_store_original_sibling_fallback_is_one_shot() {
    transient_live_carbons_or_offline(IngressFixture::sqlite().await, InitialTarget::Sibling).await;
}

#[tokio::test]
async fn postgres_prepared_full_no_store_original_sibling_fallback_is_one_shot() {
    if let Some(fixture) = IngressFixture::postgres("transient_fallback").await {
        transient_live_carbons_or_offline(fixture, InitialTarget::Sibling).await;
    }
}

#[tokio::test]
async fn sqlite_prepared_full_no_store_blocked_before_planning_never_delivers() {
    transient_live_carbons_or_offline(IngressFixture::sqlite().await, InitialTarget::Blocked).await;
}

#[tokio::test]
async fn postgres_prepared_full_no_store_blocked_before_planning_never_delivers() {
    if let Some(fixture) = IngressFixture::postgres("transient_blocked").await {
        transient_live_carbons_or_offline(fixture, InitialTarget::Blocked).await;
    }
}

#[tokio::test]
async fn sqlite_prepared_full_no_store_live_detach_preserves_original_receipt_time() {
    transient_live_carbons_or_offline(
        IngressFixture::sqlite().await,
        InitialTarget::LiveThenDetached,
    )
    .await;
}

#[tokio::test]
async fn postgres_prepared_full_no_store_live_detach_preserves_original_receipt_time() {
    if let Some(fixture) = IngressFixture::postgres("transient_live_detach").await {
        transient_live_carbons_or_offline(fixture, InitialTarget::LiveThenDetached).await;
    }
}
