use super::*;
use crate::ingress::{commit::commit_submission, test_support::IngressFixture};
use std::sync::atomic::{AtomicUsize, Ordering};
use waddle_xmpp::ingress::EffectMessageIdentity;

async fn recorded(fixture: &IngressFixture) -> (SmIngressAppendContext, FullJid, Stanza) {
    let target: FullJid = "juliet@example.com/phone".parse().expect("target");
    let intent = IngressEffectIntent::RouteDirect {
        recipient: target.to_bare(),
        fanout: vec![target.clone()],
        route_identity: EffectMessageIdentity::capture_ordinal(1),
    };
    let mut submission = fixture.submission(None, "one live delivery");
    submission.plan.intents = vec![intent.clone()];
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit");
    (
        SmIngressAppendContext {
            message_key: decision.message_key.expect("key"),
            receipt: crate::ingress::receipt_key(&intent).expect("receipt"),
            received_at: None,
            archive_positions: vec![],
            dispatch_stream: None,
        },
        target,
        Stanza::Message(submission.plan.sanitized_message),
    )
}

async fn concurrent_and_receipt_repair(fixture: IngressFixture) {
    let authority = fixture.authority().await;
    let (context, target, stanza) = recorded(&fixture).await;
    let calls = AtomicUsize::new(0);
    let enqueue = || {
        calls.fetch_add(1, Ordering::SeqCst);
        BroadcastOutcome::Delivered
    };
    let (first, second) = tokio::join!(
        authority.accept_live_delivery(&context, &target, &stanza, enqueue),
        authority.accept_live_delivery(&context, &target, &stanza, enqueue),
    );
    assert!(matches!(
        first,
        FullJidDeliveryOutcome::Delivered | FullJidDeliveryOutcome::MaybeCommitted
    ));
    assert!(matches!(
        second,
        FullJidDeliveryOutcome::Delivered | FullJidDeliveryOutcome::MaybeCommitted
    ));
    assert!(
        first == FullJidDeliveryOutcome::Delivered || second == FullJidDeliveryOutcome::Delivered
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    assert_eq!(fixture.count("ingress_delivery_receipts").await, 0);
    assert_eq!(
        authority
            .live_delivery_status(&context, &target)
            .await
            .expect("status"),
        Some(FullJidDeliveryOutcome::Delivered)
    );
    assert_eq!(
        authority
            .accept_live_delivery(&context, &target, &stanza, || panic!(
                "completed retry must not enqueue"
            ))
            .await,
        FullJidDeliveryOutcome::Delivered
    );
    authority.drain_and_join(Duration::from_secs(1)).await;
    drop(authority);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_concurrent_live_sends_and_missing_receipt_repair_enqueue_once() {
    concurrent_and_receipt_repair(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_concurrent_live_sends_and_missing_receipt_repair_enqueue_once() {
    if let Some(fixture) = IngressFixture::postgres("live_sends").await {
        concurrent_and_receipt_repair(fixture).await;
    }
}

#[tokio::test]
async fn definite_queue_failure_releases_but_failed_completion_never_retries() {
    let fixture = IngressFixture::sqlite().await;
    let authority = fixture.authority().await;
    let (context, target, stanza) = recorded(&fixture).await;
    for outcome in [
        BroadcastOutcome::NotConnected,
        BroadcastOutcome::DroppedClosed,
        BroadcastOutcome::DroppedFull,
    ] {
        let observed = authority
            .accept_live_delivery(&context, &target, &stanza, || outcome)
            .await;
        assert!(matches!(
            observed,
            FullJidDeliveryOutcome::Unavailable | FullJidDeliveryOutcome::Dropped
        ));
        assert_eq!(
            authority
                .live_delivery_status(&context, &target)
                .await
                .expect("status"),
            None
        );
    }
    fixture.execute("CREATE TRIGGER fail_send_completion BEFORE UPDATE ON ingress_send_attempts WHEN NEW.state = 2 BEGIN SELECT RAISE(ABORT, 'lost completion'); END", ()).await;
    let calls = AtomicUsize::new(0);
    assert_eq!(
        authority
            .accept_live_delivery(&context, &target, &stanza, || {
                calls.fetch_add(1, Ordering::SeqCst);
                BroadcastOutcome::Delivered
            })
            .await,
        FullJidDeliveryOutcome::MaybeCommitted
    );
    fixture
        .execute("UPDATE ingress_send_attempts SET expires_at_ms = 0", ())
        .await;
    assert_eq!(
        authority
            .live_delivery_status(&context, &target)
            .await
            .expect("status"),
        Some(FullJidDeliveryOutcome::MaybeCommitted)
    );
    assert_eq!(
        authority
            .accept_live_delivery(&context, &target, &stanza, || panic!(
                "ambiguous started attempt must not enqueue"
            ))
            .await,
        FullJidDeliveryOutcome::MaybeCommitted
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    authority.drain_and_join(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn wrong_resource_receipt_and_stanza_cannot_allocate_send_lease() {
    let fixture = IngressFixture::sqlite().await;
    let authority = fixture.authority().await;
    let (context, target, stanza) = recorded(&fixture).await;
    let other = "juliet@example.com/not-frozen".parse().expect("resource");
    assert!(authority
        .live_delivery_status(&context, &other)
        .await
        .is_err());
    assert_eq!(
        authority
            .accept_live_delivery(&context, &other, &stanza, || panic!(
                "unauthorized resource"
            ))
            .await,
        FullJidDeliveryOutcome::MaybeCommitted
    );
    let mut forged = context.clone();
    forged.receipt.semantic_identity_hash = [42; 32];
    assert!(authority
        .live_delivery_status(&forged, &target)
        .await
        .is_err());
    assert_eq!(
        authority
            .accept_live_delivery(&forged, &target, &stanza, || panic!("unauthorized receipt"))
            .await,
        FullJidDeliveryOutcome::MaybeCommitted
    );
    let Stanza::Message(mut tampered) = stanza else {
        panic!("message");
    };
    tampered.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "substituted payload".into(),
    );
    assert_eq!(
        authority
            .accept_live_delivery(&context, &target, &Stanza::Message(tampered), || panic!(
                "unauthorized payload"
            ))
            .await,
        FullJidDeliveryOutcome::MaybeCommitted
    );
    assert_eq!(fixture.count("ingress_send_attempts").await, 0);
    authority.drain_and_join(Duration::from_secs(1)).await;
}

async fn detached_custody_wins_before_live_claim(fixture: IngressFixture) {
    let authority = fixture.authority().await;
    let (context, target, stanza) = recorded(&fixture).await;
    let postgres = fixture.db.driver() == crate::db::DatabaseDriver::Postgres;
    let mut custody = if postgres {
        fixture.db.begin().await
    } else {
        fixture.db.begin_immediate().await
    }
    .expect("custody transaction");
    let sql = if postgres {
        "SELECT message_key FROM ingress_messages WHERE message_key = ?::uuid FOR UPDATE"
    } else {
        "SELECT message_key FROM ingress_messages WHERE message_key = ?"
    };
    let mut locked = custody
        .query(
            sql,
            crate::db_params![context.message_key.to_storage().to_string()],
        )
        .await
        .expect("canonical custody lock");
    assert!(locked.next().await.expect("canonical row").is_some());
    drop(locked);
    custody.execute("INSERT INTO sm_ingress_appends (message_key, receipt_kind, semantic_identity_hash, resource, accepting_stream_id, sequence, appended_at_ms, custody_payload, original_receipt_at_ms, disposition) VALUES (?, ?, ?, ?, ?, 1, 0, ?, 0, 0)", crate::db_params![context.message_key.to_storage().to_string(), context.receipt.kind.to_storage(), context.receipt.semantic_identity_hash.to_vec(), target.to_string(), "former-stream", "fixture"]).await.expect("custody allocation");
    let mut attempt = Box::pin(
        authority.accept_live_delivery(&context, &target, &stanza, || {
            panic!("custody must prevent live enqueue")
        }),
    );
    assert!(
        futures::poll!(attempt.as_mut()).is_pending(),
        "live acceptance cannot pass an uncommitted custody transaction"
    );
    custody.commit().await.expect("custody commit");
    assert_eq!(attempt.await, FullJidDeliveryOutcome::QueuedDetached);
    assert_eq!(
        authority
            .live_delivery_status(&context, &target)
            .await
            .expect("status"),
        Some(FullJidDeliveryOutcome::QueuedDetached)
    );
    assert_eq!(fixture.count("ingress_send_attempts").await, 0);
    authority.drain_and_join(Duration::from_secs(1)).await;
    drop(authority);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_detached_custody_prevents_live_send_even_without_progress() {
    detached_custody_wins_before_live_claim(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_detached_custody_prevents_live_send_even_without_progress() {
    if let Some(fixture) = IngressFixture::postgres("live_custody_interlock").await {
        detached_custody_wins_before_live_claim(fixture).await;
    }
}

#[tokio::test]
async fn shutdown_authority_does_not_claim_or_touch_sink() {
    let fixture = IngressFixture::sqlite().await;
    let authority = fixture.authority().await;
    let (context, target, stanza) = recorded(&fixture).await;
    authority.drain_and_join(Duration::from_secs(1)).await;
    assert_eq!(
        authority
            .accept_live_delivery(&context, &target, &stanza, || panic!("stopped authority"))
            .await,
        FullJidDeliveryOutcome::MaybeCommitted
    );
    assert_eq!(fixture.count("ingress_send_attempts").await, 0);
}

#[tokio::test]
async fn cancellation_before_start_can_expire_but_after_start_remains_ambiguous() {
    let fixture = IngressFixture::sqlite().await;
    let authority = fixture.authority().await;
    let (context, target, stanza) = recorded(&fixture).await;
    let gate = test_hooks::pause_after_claim(context.message_key, target.clone());
    let mut attempt = Box::pin(
        authority.accept_live_delivery(&context, &target, &stanza, || {
            panic!("cancelled before sink")
        }),
    );
    tokio::select! {
        _ = gate.wait_until_reached() => {},
        outcome = &mut attempt => panic!("attempt completed before claim gate: {outcome:?}"),
    }
    drop(attempt);
    assert_eq!(
        authority
            .live_delivery_status(&context, &target)
            .await
            .expect("reserved status"),
        Some(FullJidDeliveryOutcome::MaybeCommitted)
    );
    fixture
        .execute("UPDATE ingress_send_attempts SET expires_at_ms = 0", ())
        .await;
    assert_eq!(
        authority
            .live_delivery_status(&context, &target)
            .await
            .expect("expired status"),
        None
    );
    let gate = test_hooks::pause_after_start(context.message_key, target.clone());
    let mut attempt = Box::pin(
        authority.accept_live_delivery(&context, &target, &stanza, || {
            panic!("cancelled before sink")
        }),
    );
    tokio::select! {
        _ = gate.wait_until_reached() => {},
        outcome = &mut attempt => panic!("attempt completed before start gate: {outcome:?}"),
    }
    drop(attempt);
    fixture
        .execute("UPDATE ingress_send_attempts SET expires_at_ms = 0", ())
        .await;
    assert_eq!(
        authority
            .accept_live_delivery(&context, &target, &stanza, || panic!(
                "started cancellation cannot retry"
            ))
            .await,
        FullJidDeliveryOutcome::MaybeCommitted
    );
    authority.drain_and_join(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn replaced_unstarted_lease_cannot_reach_socket_after_wait() {
    let fixture = IngressFixture::sqlite().await;
    let authority = fixture.authority().await;
    let (context, target, stanza) = recorded(&fixture).await;
    let gate = test_hooks::pause_after_claim(context.message_key, target.clone());
    let mut attempt = Box::pin(
        authority.accept_live_delivery(&context, &target, &stanza, || {
            panic!("stale lease must not reach sink")
        }),
    );
    tokio::select! {
        _ = gate.wait_until_reached() => {},
        outcome = &mut attempt => panic!("attempt completed before claim gate: {outcome:?}"),
    }
    fixture
        .execute("UPDATE ingress_send_attempts SET expires_at_ms = 0", ())
        .await;
    let mut tx = fixture.uow.begin().await.expect("replacement tx");
    assert!(matches!(
        SendAttemptRepository::claim(
            &mut tx,
            &obligation(&context, &target),
            &NodeIdentity::new("replacement", "new-incarnation"),
            LEASE_DURATION
        )
        .await
        .expect("replacement lease"),
        SendClaim::Acquired(_)
    ));
    tx.commit().await.expect("replacement commit");
    gate.release();
    assert_eq!(attempt.await, FullJidDeliveryOutcome::MaybeCommitted);
    authority.drain_and_join(Duration::from_secs(1)).await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn postgres_rotation_revokes_unstarted_claim_and_waits_for_started_sink() {
    use waddle_xmpp::ownership::SharedNodeIdentity;
    let Some(fixture) = IngressFixture::postgres("live_node_fence").await else {
        return;
    };
    let mut authority = fixture.authority().await;
    let old = NodeIdentity::new("live-node", "first");
    let current = NodeIdentity::new("live-node", "second");
    let shared = SharedNodeIdentity::new(old);
    let deployment = fixture
        .optional_text("SELECT deployment_uuid FROM _lineage")
        .await
        .expect("deployment");
    authority.uow = IngressUnitOfWork::open_with_node_identity(
        fixture.db.clone(),
        crate::config::LineageConfig {
            deployment_uuid: Some(crate::db::lineage::DeploymentUuid(
                deployment.parse().expect("deployment UUID"),
            )),
            action: None,
        },
        shared.clone(),
    )
    .expect("node-bound UOW");
    let (context, target, stanza) = recorded(&fixture).await;
    let gate = test_hooks::pause_after_claim(context.message_key, target.clone());
    let mut attempt = Box::pin(
        authority.accept_live_delivery(&context, &target, &stanza, || {
            panic!("rotated node cannot enqueue")
        }),
    );
    tokio::select! {
        _ = gate.wait_until_reached() => {},
        outcome = &mut attempt => panic!("attempt completed before claim gate: {outcome:?}"),
    }
    shared.rotate(current.clone()).await;
    gate.release();
    assert_eq!(attempt.await, FullJidDeliveryOutcome::MaybeCommitted);
    fixture
        .execute("UPDATE ingress_send_attempts SET expires_at_ms = 0", ())
        .await;
    let gate = test_hooks::pause_after_start(context.message_key, target.clone());
    let mut attempt = Box::pin(
        authority.accept_live_delivery(&context, &target, &stanza, || {
            assert_eq!(
                shared.current(),
                current,
                "start guard must retain node authority through enqueue"
            );
            BroadcastOutcome::Delivered
        }),
    );
    tokio::select! {
        _ = gate.wait_until_reached() => {},
        outcome = &mut attempt => panic!("attempt completed before start gate: {outcome:?}"),
    }
    let mut rotation = Box::pin(shared.rotate(NodeIdentity::new("live-node", "third")));
    assert!(
        futures::poll!(rotation.as_mut()).is_pending(),
        "node rotation must wait for sink authority guard"
    );
    gate.release();
    let (outcome, ()) = tokio::join!(attempt, rotation);
    assert_eq!(outcome, FullJidDeliveryOutcome::Delivered);
    authority.drain_and_join(Duration::from_secs(1)).await;
    drop(authority);
    fixture.close().await;
}

#[tokio::test]
async fn frozen_pin_notification_allows_sender_copy_but_rejects_substituted_payload() {
    use waddle_xmpp::ingress::DmPinMutationAction;
    use waddle_xmpp_core::xep0359::StanzaId;
    use xmpp_parsers::message::{Lang, Message, MessageType};
    let fixture = IngressFixture::sqlite().await;
    let authority = fixture.authority().await;
    let mut submission = fixture.submission(None, "pin request");
    let target = submission.sender.clone();
    let peer = "juliet@example.com".parse::<jid::BareJid>().expect("peer");
    let stamp = StanzaId::new("pin-notification", target.to_bare().into());
    let route = IngressEffectIntent::RouteDirect {
        recipient: target.to_bare(),
        fanout: vec![target.clone()],
        route_identity: EffectMessageIdentity::stanza(stamp.clone()),
    };
    let pair = crate::server::routes::websocket::DmPairKey::new(target.to_bare(), peer.clone());
    submission.plan.intents = vec![
        IngressEffectIntent::DmPinMutation {
            pair: (pair.low_peer, pair.high_peer),
            target_stanza_id: StanzaId::new("pinned-message", peer.clone().into()),
            action: DmPinMutationAction::Unpin,
        },
        route.clone(),
    ];
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("pin authority");
    let context = SmIngressAppendContext {
        message_key: decision.message_key.expect("key"),
        receipt: crate::ingress::receipt_key(&route).expect("route receipt"),
        received_at: None,
        archive_positions: vec![],
        dispatch_stream: None,
    };
    // This notification stays addressed to the peer even on the sender's own
    // frozen resource, as specified by the pin fanout planner.
    let mut message = Message::new(Some(peer.into()));
    message.from = Some(target.to_bare().into());
    message.type_ = MessageType::Chat;
    message
        .bodies
        .insert(Lang::new(), "romeo@example.com unpinned a message".into());
    message.payloads.push(
        minidom::Element::builder("pin-event", waddle_xmpp::xep::NS_WADDLE_PIN_V0)
            .attr(minidom::rxml::xml_ncname!("action").to_owned(), "unpinned")
            .attr(
                minidom::rxml::xml_ncname!("target").to_owned(),
                "pinned-message",
            )
            .attr(
                minidom::rxml::xml_ncname!("by").to_owned(),
                "romeo@example.com",
            )
            .build(),
    );
    waddle_xmpp_core::xep0359::add_stanza_id(&mut message, &stamp);
    let mut forged = message.clone();
    forged
        .bodies
        .insert(Lang::new(), "unrelated injected content".into());
    assert_eq!(
        authority
            .accept_live_delivery(&context, &target, &Stanza::Message(forged), || panic!(
                "wrong pin payload"
            ))
            .await,
        FullJidDeliveryOutcome::MaybeCommitted
    );
    assert_eq!(fixture.count("ingress_send_attempts").await, 0);
    assert_eq!(
        authority
            .accept_live_delivery(&context, &target, &Stanza::Message(message), || {
                BroadcastOutcome::Delivered
            })
            .await,
        FullJidDeliveryOutcome::Delivered
    );
    authority.drain_and_join(Duration::from_secs(1)).await;
}

async fn unarchived_recipient_stamp(hint: Option<waddle_xmpp::xep::xep0334::Hint>) {
    use waddle_xmpp::xep::xep0353::{build_propose, CallOffer};
    use waddle_xmpp_core::xep0359::{add_stanza_id, StanzaId};
    let fixture = IngressFixture::sqlite().await;
    let authority = fixture.authority().await;
    let target: FullJid = "juliet@example.com/phone".parse().expect("target");
    let route = IngressEffectIntent::RouteDirect {
        recipient: target.to_bare(),
        fanout: vec![target.clone()],
        route_identity: EffectMessageIdentity::capture_ordinal(1),
    };
    let mut submission = fixture.submission(None, "nonarchived signal body");
    let message = &mut submission.plan.sanitized_message;
    if hint.is_none() {
        message.bodies.clear();
    }
    message.type_ = xmpp_parsers::message::MessageType::Normal;
    message.payloads.push(build_propose(
        xmpp_parsers::jingle::SessionId("call-offer".into()),
        CallOffer::audio_video(),
    ));
    add_stanza_id(
        message,
        &StanzaId::new("sender-signal", submission.sender.to_bare().into()),
    );
    if let Some(hint) = hint {
        waddle_xmpp::xep::xep0334::add_hint(message, hint);
    }
    submission.plan.intents = vec![route.clone()];
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("signal authority");
    let context = SmIngressAppendContext {
        message_key: decision.message_key.expect("key"),
        receipt: crate::ingress::receipt_key(&route).expect("route receipt"),
        received_at: None,
        archive_positions: vec![],
        dispatch_stream: None,
    };
    let mut delivered = submission.plan.sanitized_message.clone();
    add_stanza_id(
        &mut delivered,
        &StanzaId::new("recipient-signal", target.to_bare().into()),
    );
    let mut changed_offer = delivered.clone();
    changed_offer.payloads[0] = build_propose(
        xmpp_parsers::jingle::SessionId("other-call".into()),
        CallOffer::audio_only(),
    );
    let mut changed_sender_stamp = delivered.clone();
    add_stanza_id(
        &mut changed_sender_stamp,
        &StanzaId::new("forged-sender-id", submission.sender.to_bare().into()),
    );
    let mut wrong_stamp_owner = submission.plan.sanitized_message.clone();
    add_stanza_id(
        &mut wrong_stamp_owner,
        &StanzaId::new(
            "foreign-signal",
            "mallory@example.com".parse().expect("foreign authority"),
        ),
    );
    let mut duplicate_recipient_stamp = delivered.clone();
    duplicate_recipient_stamp
        .payloads
        .push(waddle_xmpp_core::xep0359::build_stanza_id_element(
            "duplicate",
            &target.to_bare().into(),
        ));
    for forged in [
        changed_offer,
        changed_sender_stamp,
        wrong_stamp_owner,
        duplicate_recipient_stamp,
    ] {
        assert_eq!(
            authority
                .accept_live_delivery(&context, &target, &Stanza::Message(forged), || panic!(
                    "unauthorized signal change"
                ))
                .await,
            FullJidDeliveryOutcome::MaybeCommitted
        );
    }
    assert_eq!(fixture.count("ingress_send_attempts").await, 0);
    assert_eq!(
        authority
            .accept_live_delivery(&context, &target, &Stanza::Message(delivered), || {
                BroadcastOutcome::Delivered
            })
            .await,
        FullJidDeliveryOutcome::Delivered
    );
    authority.drain_and_join(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn unarchived_jmi_accepts_recipient_stamp_but_rejects_payload_and_other_stamp_changes() {
    use waddle_xmpp::xep::xep0334::Hint;
    for hint in [None, Some(Hint::NoStore), Some(Hint::NoPermanentStore)] {
        unarchived_recipient_stamp(hint).await;
    }
}
