//! XEP-0198/XEP-0334: recover prepared full-JID copies without offline storage.
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
    add_hint(&mut message, Hint::NoStore);
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
    let mut tx = fixture.uow.begin().await.expect("receipt time");
    let received_at = CanonicalMessageRepository::created_at(&mut tx, key)
        .await
        .expect("time");
    tx.commit().await.expect("read commit");
    // A genuine retry mints a different recipient stamp. Canonical authority
    // must replace that fresh copy before either execution or later recovery.
    let replanned =
        plan_message_dispatch(&mut machine, offered, &build_interpret_deps(&state, None)).await;
    let original_stamp = match expected.as_ref() {
        Stanza::Message(message) => {
            waddle_xmpp_core::xep0359::extract_stanza_id_by(message, &target.to_bare().into())
        }
        _ => None,
    }
    .expect("original recipient stamp");
    let fresh_stamp = replanned
        .plan
        .iter()
        .find_map(|planned| match &planned.effect {
            Effect::External(ExternalEffect::Delivery(ExternalDeliveryEffect::QueueDetached {
                stanza,
                resources,
                ..
            })) if resources == std::slice::from_ref(&target) => match stanza.as_ref() {
                Stanza::Message(message) => waddle_xmpp_core::xep0359::extract_stanza_id_by(
                    message,
                    &target.to_bare().into(),
                ),
                _ => None,
            },
            _ => None,
        })
        .expect("fresh recipient stamp");
    assert_ne!(original_stamp, fresh_stamp);
    submission.plan = replanned;
    let retry = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("retry acceptance");
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
        .expect("unfinished frozen copy remains executable");
    assert_eq!(retried_copy.to_element(), expected.to_element());
    drop(retry);
    // Discard the execution decision: maintenance has only committed authority.
    drop(decision);
    match recipient_state {
        RecipientState::Resumable => {}
        RecipientState::Gone | RecipientState::GoneThenLive => {
            sm.take_session(&target.to_string())
                .await
                .expect("session ended");
        }
        RecipientState::AlreadyDelivered => {
            sm.take_session(&target.to_string())
                .await
                .expect("session ended");
            let mut tx = fixture.uow.begin().await.expect("retained resource proof");
            CanonicalMessageRepository::lock(&mut tx, key)
                .await
                .expect("lock authority");
            crate::ingress_uow::DeliveryProgressRepository::record(
                &mut tx,
                key,
                &context.receipt,
                std::slice::from_ref(&target),
            )
            .await
            .expect("delivery proof survived");
            tx.commit().await.expect("resource proof");
        }
        RecipientState::BareGone => {
            sm.take_session(&target.to_string())
                .await
                .expect("session ended");
            let mut tx = fixture.uow.begin().await.expect("unknown send");
            let obligation = crate::ingress_uow::SendObligation {
                message: key,
                receipt: context.receipt.clone(),
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
            tx.commit().await.expect("unknown start");
            fixture
                .execute("UPDATE ingress_send_attempts SET expires_at_ms = 0", ())
                .await;
        }
        #[cfg(feature = "clustering")]
        RecipientState::GoneWithClaim(_) => {
            sm.take_session(&target.to_string())
                .await
                .expect("session ended");
        }
        RecipientState::Blocked => {
            blocking.set_blocklist(target.to_bare(), vec![submission.sender.to_bare()]);
        }
    }
    let newcomer: jid::FullJid = "juliet@example.com/newcomer".parse().expect("new target");
    store_detached(&sm, &newcomer).await;
    let env: Arc<dyn RecoveryEnvironment> = Arc::new(StateEnvironment(state.clone()));
    for _ in 0..2 {
        let cursor = MaintenanceCursor::default();
        assert_eq!(
            pass(&fixture, &env, &cursor).await,
            MaintenanceOutcome::Complete
        );
        if matches!(
            recipient_state,
            RecipientState::Gone | RecipientState::GoneThenLive
        ) {
            assert_pending(&fixture, key).await;
            assert_eq!(
                fixture
                    .count("ingress_effect_receipts WHERE policy_discard_reason IS NOT NULL")
                    .await,
                0,
                "absence cannot prove the prepared copy was permanently undeliverable"
            );
            assert_eq!(fixture.count("ingress_delivery_receipts").await, 0);
            assert_eq!(fixture.count("sm_ingress_appends").await, 0);
            assert_eq!(fixture.count("pending_delivery").await, 0);
            continue;
        }
        #[cfg(feature = "clustering")]
        if matches!(recipient_state, RecipientState::GoneWithClaim(_)) {
            assert_pending(&fixture, key).await;
            assert_eq!(
                fixture
                    .count("ingress_effect_receipts WHERE policy_discard_reason IS NOT NULL")
                    .await,
                0
            );
            assert_eq!(fixture.count("ingress_delivery_receipts").await, 0);
            assert_eq!(fixture.count("sm_ingress_appends").await, 0);
            assert_eq!(fixture.count("pending_delivery").await, 0);
            assert_eq!(append_count(&sm, &newcomer).await, 0);
            continue;
        }
        if matches!(
            recipient_state,
            RecipientState::AlreadyDelivered | RecipientState::BareGone
        ) {
            let mut tx = fixture.uow.begin().await.expect("proof repair");
            assert!(
                CanonicalMessageRepository::is_terminal(&mut tx, key)
                    .await
                    .expect("terminal read"),
                "existing proof or the original bare-JID fallback resolves the aggregate"
            );
            tx.commit().await.expect("repair read");
            assert_eq!(
                fixture
                    .count("ingress_effect_receipts WHERE policy_discard_reason IS NOT NULL")
                    .await,
                0
            );
            assert_eq!(
                fixture.count("ingress_delivery_receipts").await,
                i64::from(matches!(recipient_state, RecipientState::AlreadyDelivered))
            );
            assert_eq!(fixture.count("sm_ingress_appends").await, 0);
            assert_eq!(fixture.count("pending_delivery").await, 0);
            continue;
        }
        if matches!(recipient_state, RecipientState::Blocked) {
            let mut tx = fixture.uow.begin().await.expect("settlement");
            assert!(
                CanonicalMessageRepository::is_terminal(&mut tx, key)
                    .await
                    .expect("terminal read"),
                "policy must resolve the undeliverable prepared route"
            );
            tx.commit().await.expect("settlement read");
            assert_eq!(
                fixture
                    .count(
                        "ingress_effect_receipts WHERE policy_discard_reason = 'recipient_blocked'"
                    )
                    .await,
                1
            );
            assert_eq!(
                fixture.count("ingress_delivery_receipts").await,
                0,
                "discard is not a successful delivery"
            );
            assert_eq!(fixture.count("sm_ingress_appends").await, 0);
            assert_eq!(fixture.count("pending_delivery").await, 0);
            assert_eq!(append_count(&sm, &newcomer).await, 0);
            let mut tx = fixture.uow.begin().await.expect("late ordinary settlement");
            CanonicalMessageRepository::lock(&mut tx, key)
                .await
                .expect("lock authority");
            EffectReceiptRepository::record_receipt(
                &mut tx,
                key,
                context.receipt.kind,
                &context.receipt.semantic_identity_hash,
            )
            .await
            .expect("ordinary receipt retry");
            tx.commit().await.expect("receipt retry");
            assert_eq!(
                fixture
                    .count(
                        "ingress_effect_receipts WHERE policy_discard_reason = 'recipient_blocked'"
                    )
                    .await,
                1,
                "ordinary retries preserve the original discard reason"
            );
            continue;
        }
        let session = sm
            .peek_session(&target.to_string())
            .await
            .expect("read SM")
            .expect("session");
        assert_eq!(
            session.unacked_stanzas.len(),
            1,
            "one keyed resumable allocation"
        );
        let recovered: minidom::Element = session.unacked_stanzas[0]
            .stanza_xml
            .parse()
            .expect("queued XML");
        assert_eq!(recovered, expected.to_element());
        assert_eq!(session.unacked_stanzas[0].original_receipt_at, received_at);
        assert_eq!(
            append_count(&sm, &newcomer).await,
            0,
            "frozen audience excludes new resources"
        );
        assert_eq!(fixture.count("sm_ingress_appends").await, 1);
        assert_eq!(fixture.count("mam_messages").await, 0);
        assert_eq!(fixture.count("pending_delivery").await, 0);
        let mut tx = fixture.uow.begin().await.expect("late policy settlement");
        CanonicalMessageRepository::lock(&mut tx, key)
            .await
            .expect("lock authority");
        EffectReceiptRepository::record_policy_discard(
            &mut tx,
            key,
            &context.receipt,
            crate::ingress_uow::PolicyDiscardReason::RecipientBlocked,
        )
        .await
        .expect("late policy");
        tx.commit().await.expect("late policy commit");
        assert_eq!(
            fixture
                .count("ingress_effect_receipts WHERE policy_discard_reason IS NOT NULL")
                .await,
            0,
            "a completed custody receipt cannot be relabeled as policy discard"
        );
    }
    if matches!(recipient_state, RecipientState::Gone) {
        // Reappearance after maintenance observed absence must still receive
        // the owed exact copy; an absence-based receipt would lose it forever.
        store_detached(&sm, &target).await;
        assert_eq!(
            pass(&fixture, &env, &MaintenanceCursor::default()).await,
            MaintenanceOutcome::Complete
        );
        assert_eq!(append_count(&sm, &target).await, 1);
        let session = sm
            .peek_session(&target.to_string())
            .await
            .expect("read rebound SM")
            .expect("rebound session");
        let recovered: minidom::Element = session.unacked_stanzas[0]
            .stanza_xml
            .parse()
            .expect("rebound XML");
        assert_eq!(recovered, expected.to_element());
        assert_eq!(fixture.count("pending_delivery").await, 0);
        assert_eq!(append_count(&sm, &newcomer).await, 0);
        assert_eq!(
            fixture
                .count("ingress_effect_receipts WHERE policy_discard_reason IS NOT NULL")
                .await,
            0
        );
    }
    let reconnect_live = matches!(recipient_state, RecipientState::GoneThenLive);
    #[cfg(feature = "clustering")]
    let reconnect_live = reconnect_live
        || matches!(
            recipient_state,
            RecipientState::GoneWithClaim(ClaimState::Local)
        );
    if reconnect_live {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(8);
        socket_tests::register_test_connection(&state, &target, sender).await;
        for _ in 0..2 {
            assert_eq!(
                pass(&fixture, &env, &MaintenanceCursor::default()).await,
                MaintenanceOutcome::Complete
            );
        }
        let delivered = receiver
            .try_recv()
            .expect("owed copy reaches rebound socket");
        assert_eq!(delivered.stanza.to_element(), expected.to_element());
        assert!(
            receiver.try_recv().is_err(),
            "repeated recovery sends only once"
        );
        assert_eq!(fixture.count("ingress_delivery_receipts").await, 1);
        assert_eq!(fixture.count("sm_ingress_appends").await, 0);
        assert_eq!(fixture.count("pending_delivery").await, 0);
        assert_eq!(append_count(&sm, &newcomer).await, 0);
        assert_eq!(
            fixture
                .count("ingress_effect_receipts WHERE policy_discard_reason IS NOT NULL")
                .await,
            0
        );
    }
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_xep0198_xep0334_prepared_full_no_store_recovers_once() {
    prepared_no_store_recovery(IngressFixture::sqlite().await, RecipientState::Resumable).await;
}

#[tokio::test]
async fn postgres_xep0198_xep0334_prepared_full_no_store_recovers_once() {
    if let Some(fixture) = IngressFixture::postgres("prp_nostore").await {
        prepared_no_store_recovery(fixture, RecipientState::Resumable).await;
    }
}

#[tokio::test]
async fn sqlite_xep0334_prepared_full_no_store_gone_target_remains_recoverable() {
    prepared_no_store_recovery(IngressFixture::sqlite().await, RecipientState::Gone).await;
}

#[tokio::test]
async fn postgres_xep0334_prepared_full_no_store_gone_target_remains_recoverable() {
    if let Some(fixture) = IngressFixture::postgres("prp_gone").await {
        prepared_no_store_recovery(fixture, RecipientState::Gone).await;
    }
}

#[tokio::test]
async fn sqlite_prepared_full_no_store_gone_target_delivers_after_live_rebind() {
    prepared_no_store_recovery(IngressFixture::sqlite().await, RecipientState::GoneThenLive).await;
}

#[tokio::test]
async fn postgres_prepared_full_no_store_gone_target_delivers_after_live_rebind() {
    if let Some(fixture) = IngressFixture::postgres("prp_rebind").await {
        prepared_no_store_recovery(fixture, RecipientState::GoneThenLive).await;
    }
}

#[tokio::test]
async fn sqlite_xep0191_prepared_full_no_store_blocked_target_discards() {
    prepared_no_store_recovery(IngressFixture::sqlite().await, RecipientState::Blocked).await;
}

#[tokio::test]
async fn postgres_xep0191_prepared_full_no_store_blocked_target_discards() {
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
    let failure = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect_err("oversized intent refuses responsibility");
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
async fn postgres_prepared_full_no_store_absence_remains_pending_regardless_of_claim() {
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
async fn sqlite_prepared_full_no_store_existing_resource_proof_repairs_aggregate() {
    prepared_no_store_recovery(
        IngressFixture::sqlite().await,
        RecipientState::AlreadyDelivered,
    )
    .await;
}

#[tokio::test]
async fn postgres_prepared_full_no_store_existing_resource_proof_repairs_aggregate() {
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
