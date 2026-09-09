use super::*;
use crate::ingress::{
    commit::commit_submission,
    execute::{execute_effects, terminalize_if_complete},
    test_support::IngressFixture,
    IngressDecisionClass, IngressStreamIdentity, IngressSubmission,
};
use crate::ingress_uow::{EffectReceiptRepository, SmIngressStreamRepository};
use crate::server::routes::interpret::effects::{EffectSink, ImmediateSink, PlanSink};
use crate::server::routes::websocket::tests::{
    create_test_websocket_state, register_test_connection,
};
use std::{sync::Arc, time::Duration};
use waddle_xmpp::{
    ingress::{DigestContext, DigestInput, WireHandledCount},
    mam::{ArchivedMessage, SqlxMamStorage},
    pending_delivery::SmSessionId,
};

async fn state_for(fixture: &IngressFixture) -> Arc<WebSocketState> {
    let mut state = create_test_websocket_state().await;
    Arc::get_mut(&mut state)
        .expect("exclusive fixture state")
        .deps
        .protocol
        .mam_storage = Arc::new(
        SqlxMamStorage::open(fixture.db.database_url())
            .await
            .expect("MAM"),
    );
    state
}

async fn seed_target(state: &WebSocketState, submission: &IngressSubmission) -> StanzaId {
    let target = StanzaId::new("pin-target", submission.sender.to_bare().into());
    let peer = submission
        .plan
        .sanitized_message
        .to
        .as_ref()
        .expect("peer")
        .to_bare();
    for (archive, id) in [
        (&submission.sender.to_bare(), "sender-copy"),
        (&peer, "peer-copy"),
    ] {
        state
            .deps
            .protocol
            .mam_storage
            .store_message(
                archive,
                &ArchivedMessage {
                    id: id.to_owned(),
                    body: Some("important message".into()),
                    stanza_id: Some(target.clone()),
                    message_type: xmpp_parsers::message::MessageType::Chat,
                    ..ArchivedMessage::for_test(
                        submission.sender.clone().into(),
                        peer.clone().into(),
                    )
                },
            )
            .await
            .expect("archive target");
    }
    // The lookup prefers the peer's archive assigning authority.
    StanzaId::new(target.id, peer.into())
}

fn set_marker(submission: &mut IngressSubmission, target: &StanzaId, unpin: bool, cascade: bool) {
    let message = &mut submission.plan.sanitized_message;
    message.payloads.push(if cascade {
        waddle_xmpp::xep::build_retract_element(&target.id)
    } else if unpin {
        waddle_xmpp::xep::xep_waddle_pin::build_unpinned_element(target)
    } else {
        waddle_xmpp::xep::build_pinned_message_element(target)
    });
    submission.digest_input = DigestInput::from_parsed(
        message,
        &DigestContext {
            target: submission.target.clone(),
            server_authorities: vec![submission.sender.to_bare()],
            stanza_lang: None,
        },
    )
    .expect("pin digest");
}

async fn plan(state: &WebSocketState, submission: &mut IngressSubmission, cascade: bool) {
    let sink = PlanSink::new();
    sink.observe_sender(&submission.sender);
    let capture = IngressEffectCapture::new();
    let mut deps =
        crate::server::routes::websocket::interpret_loop::build_interpret_deps(state, None);
    deps.effects = &sink;
    deps.ingress_effect_capture = Some(capture.clone());
    if cascade {
        handle_dm_pin_retraction_cascade(
            &submission.plan.sanitized_message,
            state,
            &submission.sender,
            &deps,
        )
        .await;
    } else {
        handle_dm_pin_message(
            &submission.plan.sanitized_message,
            state,
            &submission.sender,
            &deps,
        )
        .await
        .expect("pin handled");
    }
    submission.plan.failure = sink.failure();
    submission.plan.rejection = sink.rejection();
    submission.plan.plan = sink.take().0;
    submission.plan.intents = capture.snapshot().intents;
}

async fn assert_receipted(fixture: &IngressFixture, decision: &crate::ingress::IngressDecision) {
    let key = decision.message_key.expect("canonical key");
    let mut tx = fixture.uow.begin().await.expect("receipt tx");
    for receipt in decision.external_receipts.iter().flatten() {
        assert!(EffectReceiptRepository::contains(
            &mut tx,
            key,
            receipt.kind,
            &receipt.semantic_identity_hash
        )
        .await
        .expect("receipt"));
    }
    tx.commit().await.expect("receipt read");
    assert!(terminalize_if_complete(&fixture.uow, key)
        .await
        .expect("terminalize"));
}

fn assert_pin_notification(
    outbound: waddle_xmpp::registry::OutboundStanza,
    recipient: &jid::BareJid,
    recorded: &[IngressEffectIntent],
    request: &xmpp_parsers::message::Message,
    target: &StanzaId,
    action: DmPinAction,
    cascade: bool,
) {
    let Stanza::Message(message) = outbound.stanza else {
        panic!("pin notification must be a message");
    };
    let event = message
        .payloads
        .iter()
        .find(|payload| {
            payload.name() == "pin-event" && payload.ns() == waddle_xmpp::xep::NS_WADDLE_PIN_V0
        })
        .expect("recipient receives the synthetic pin event, not the canonical request");
    assert_eq!(event.attr("action"), Some(action.as_attr()));
    assert_eq!(event.attr("target"), Some(target.id.as_str()));
    assert_eq!(event.attr("reason"), cascade.then_some("retracted"));
    let identity = recorded
        .iter()
        .find_map(|intent| match intent {
            IngressEffectIntent::RouteDirect {
                recipient: saved,
                route_identity: EffectMessageIdentity::StanzaId(stanza_id),
                ..
            } if saved == recipient => Some(stanza_id),
            _ => None,
        })
        .expect("recorded pin notification identity");
    assert_eq!(
        waddle_xmpp_core::xep0359::extract_stanza_ids(&message),
        vec![identity.clone()],
        "notification retains its recorded identity on replay"
    );
    assert_ne!(
        message.bodies, request.bodies,
        "request body is not delivered"
    );
    for payload in &request.payloads {
        assert!(
            !message.payloads.contains(payload),
            "canonical request payload must not replace or leak into the pin event"
        );
    }
}

async fn dm_pin_receipts(fixture: IngressFixture, replay: bool, cascade: bool) {
    let state = state_for(&fixture).await;
    let mut submission = fixture.submission(Some("pin-receipt"), "pin request");
    let target = seed_target(&state, &submission).await;
    let peer: jid::FullJid = "juliet@example.com/phone".parse().expect("peer");
    let (sender_tx, mut sender_rx) = tokio::sync::mpsc::channel(8);
    let (peer_tx, mut peer_rx) = tokio::sync::mpsc::channel(8);
    register_test_connection(&state, &submission.sender, sender_tx).await;
    register_test_connection(&state, &peer, peer_tx).await;
    let pair = crate::server::routes::websocket::DmPairKey::new(
        submission.sender.to_bare(),
        peer.to_bare(),
    );
    if replay {
        state.deps.protocol.dm_pin_store.apply_pin(
            pair.clone(),
            waddle_xmpp::muc::PinnedEntry {
                target_stanza_id: target.clone(),
                pinner_jid: submission.sender.to_bare(),
                pinned_at: chrono::Utc::now(),
                preview: waddle_xmpp::muc::PinPreview::new(
                    submission.sender.to_bare(),
                    None,
                    "important message",
                    chrono::Utc::now(),
                ),
            },
        );
    }
    set_marker(&mut submission, &target, replay, cascade);
    plan(&state, &mut submission, cascade).await;
    if cascade {
        for (archive, archive_id) in [
            (submission.sender.to_bare(), "sender-copy"),
            (peer.to_bare(), "peer-copy"),
        ] {
            let target_stanza_id = StanzaId::new(archive_id, archive.clone().into());
            submission.plan.plan.push(PlannedEffect::new(Effect::Durable(
                crate::server::routes::interpret::effects::DurableEffect::Direct(
                    crate::server::routes::interpret::effects::direct::DurableDirectEffect::RetractionTombstone {
                        archive: archive.clone(),
                        target: target_stanza_id.clone(),
                        tombstone: waddle_xmpp_core::mam::ArchivedTombstone {
                            retraction_id: None,
                            stamp: chrono::Utc::now(),
                            moderation: None,
                            sender_scope: None,
                        },
                    },
                ),
            )));
            submission
                .plan
                .intents
                .push(IngressEffectIntent::RetractionTombstone {
                    mutation: waddle_xmpp::ingress::RetractionTombstoneMutation {
                        archive: archive.clone(),
                        target_stanza_id,
                        retraction_stanza_id: StanzaId::new("pin-retraction", archive.into()),
                    },
                });
        }
    }
    let recorded = submission.plan.intents.clone();
    assert_eq!(
        recorded
            .iter()
            .filter(|intent| matches!(intent, IngressEffectIntent::RouteDirect { .. }))
            .count(),
        2
    );
    let mut decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit pin");
    assert!(sender_rx.try_recv().is_err());
    assert!(peer_rx.try_recv().is_err());
    let deps = crate::server::routes::websocket::interpret_loop::build_interpret_deps(&state, None);
    if replay {
        if cascade {
            for archive in [submission.sender.to_bare(), peer.to_bare()] {
                let archived = state
                    .deps
                    .protocol
                    .mam_storage
                    .get_message_by_archive_or_stanza_id(&archive, &target.id)
                    .await
                    .expect("read Phase B tombstone")
                    .expect("target retained");
                assert!(
                    is_tombstoned_archive_row(&archived),
                    "Phase B durably tombstoned both copies before cancellation"
                );
            }
        } else {
            let mutation = submission
                .plan
                .plan
                .iter()
                .find_map(|effect| match &effect.effect {
                    Effect::External(ExternalEffect::DmPinMutation(mutation)) => {
                        Some(mutation.clone())
                    }
                    _ => None,
                })
                .expect("committed unpin");
            assert!(matches!(
                execute_dm_pin(mutation, &deps).await,
                EffectOutcome::Completed
            ));
            assert!(!state.deps.protocol.dm_pin_store.contains(&pair, &target));
        }
        assert!(
            !terminalize_if_complete(&fixture.uow, decision.message_key.expect("key"))
                .await
                .expect("pending receipt")
        );
        plan(&state, &mut submission, cascade).await;
        assert!(
            submission.plan.plan.is_empty(),
            "live lookup cannot reconstruct committed work"
        );
        // The commit hook restores recorded authority after today's empty plan.
        decision = commit_submission(&fixture.uow, &submission, 1)
            .await
            .expect("replay committed pin");
        assert!(
            decision
                .external
                .iter()
                .any(|effect| matches!(effect, ExternalEffect::DmPinMutation(_))),
            "replay reconstructs mutation"
        );
    }
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
    for (recipient, outbound) in [
        (
            submission.sender.to_bare(),
            sender_rx.try_recv().expect("sender notification"),
        ),
        (
            peer.to_bare(),
            peer_rx.try_recv().expect("peer notification"),
        ),
    ] {
        assert_pin_notification(
            outbound,
            &recipient,
            &recorded,
            &submission.plan.sanitized_message,
            &target,
            if replay {
                DmPinAction::Unpinned
            } else {
                DmPinAction::Pinned
            },
            cascade,
        );
    }
    assert!(
        sender_rx.try_recv().is_err(),
        "only one sender notification"
    );
    assert!(peer_rx.try_recv().is_err(), "only one peer notification");
    assert_eq!(
        state.deps.protocol.dm_pin_store.contains(&pair, &target),
        !replay,
        "confirmed pin state"
    );
    assert_receipted(&fixture, &decision).await;
    fixture.close().await;
}

async fn dm_pin_lookup_failure(fixture: IngressFixture, unpin: bool) {
    let state = state_for(&fixture).await;
    let mut submission = fixture.submission(Some("pin-storage-retry"), "pin request");
    let target = seed_target(&state, &submission).await;
    if unpin {
        let pair = crate::server::routes::websocket::DmPairKey::new(
            submission.sender.to_bare(),
            "juliet@example.com".parse().expect("peer"),
        );
        state.deps.protocol.dm_pin_store.apply_pin(
            pair,
            waddle_xmpp::muc::PinnedEntry {
                target_stanza_id: target.clone(),
                pinner_jid: submission.sender.to_bare(),
                pinned_at: chrono::Utc::now(),
                preview: waddle_xmpp::muc::PinPreview::new(
                    submission.sender.to_bare(),
                    None,
                    "body",
                    chrono::Utc::now(),
                ),
            },
        );
    }
    set_marker(&mut submission, &target, unpin, false);
    let mut tx = fixture.uow.begin().await.expect("stream tx");
    let stream_id = SmSessionId::new("pin-storage-stream");
    let sm_ingress_id = SmIngressStreamRepository::mint(&mut tx, &stream_id)
        .await
        .expect("mint stream");
    tx.commit().await.expect("stream commit");
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
    fixture
        .execute(
            "ALTER TABLE mam_messages RENAME TO unavailable_mam_messages",
            (),
        )
        .await;
    plan(&state, &mut submission, false).await;
    assert_eq!(
        submission.plan.failure,
        Some(PlanFailure::DmPinTargetLookup)
    );
    let error = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect_err("lookup failure");
    assert_eq!(error.class(), IngressDecisionClass::Storage);
    assert!(!error.class().advances());
    for table in [
        "ingress_messages",
        "ingress_origin_aliases",
        "ingress_effect_intents",
        "ingress_effect_receipts",
        "ingress_sm_refs",
    ] {
        assert_eq!(fixture.count(table).await, 0, "no writes to {table}");
    }
    assert_eq!(
        fixture
            .count("ingress_sm_streams WHERE handled_ordinal = 0 AND checkpoint_h = 0")
            .await,
        1
    );
    fixture
        .execute(
            "ALTER TABLE unavailable_mam_messages RENAME TO mam_messages",
            (),
        )
        .await;
    plan(&state, &mut submission, false).await;
    assert_eq!(submission.plan.failure, None);
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("healthy retry");
    assert!(decision.class.advances());
    let deps = crate::server::routes::websocket::interpret_loop::build_interpret_deps(&state, None);
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
    let pair = crate::server::routes::websocket::DmPairKey::new(
        submission.sender.to_bare(),
        "juliet@example.com".parse().expect("peer"),
    );
    assert_eq!(
        state.deps.protocol.dm_pin_store.contains(&pair, &target),
        !unpin,
        "healthy retry applies mutation"
    );
    assert_receipted(&fixture, &decision).await;
    assert_eq!(fixture.count("ingress_origin_aliases").await, 1);
    assert_eq!(
        fixture
            .count("ingress_sm_streams WHERE handled_ordinal = 1 AND checkpoint_h = 1")
            .await,
        1
    );
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_dm_pin_delivery_receipts_terminalize() {
    dm_pin_receipts(IngressFixture::sqlite().await, false, false).await;
}
#[tokio::test]
async fn postgres_dm_pin_delivery_receipts_terminalize() {
    if let Some(f) = IngressFixture::postgres("dm_pin_receipts").await {
        dm_pin_receipts(f, false, false).await;
    }
}
#[tokio::test]
async fn sqlite_committed_unpin_replay_receipts_absent_target() {
    dm_pin_receipts(IngressFixture::sqlite().await, true, false).await;
}
#[tokio::test]
async fn postgres_committed_unpin_replay_receipts_absent_target() {
    if let Some(f) = IngressFixture::postgres("dm_unpin_replay").await {
        dm_pin_receipts(f, true, false).await;
    }
}
#[tokio::test]
async fn sqlite_retraction_cascade_replay_unpins_tombstones() {
    dm_pin_receipts(IngressFixture::sqlite().await, true, true).await;
}
#[tokio::test]
async fn postgres_retraction_cascade_replay_unpins_tombstones() {
    if let Some(f) = IngressFixture::postgres("dm_cascade_replay").await {
        dm_pin_receipts(f, true, true).await;
    }
}
#[tokio::test]
async fn sqlite_dm_pin_lookup_failure_is_nonadvancing_and_retries() {
    dm_pin_lookup_failure(IngressFixture::sqlite().await, false).await;
}
#[tokio::test]
async fn postgres_dm_pin_lookup_failure_is_nonadvancing_and_retries() {
    if let Some(f) = IngressFixture::postgres("dm_pin_lookup").await {
        dm_pin_lookup_failure(f, false).await;
    }
}
#[tokio::test]
async fn sqlite_dm_unpin_lookup_failure_is_nonadvancing_and_retries() {
    dm_pin_lookup_failure(IngressFixture::sqlite().await, true).await;
}
#[tokio::test]
async fn postgres_dm_unpin_lookup_failure_is_nonadvancing_and_retries() {
    if let Some(f) = IngressFixture::postgres("dm_unpin_lookup").await {
        dm_pin_lookup_failure(f, true).await;
    }
}
