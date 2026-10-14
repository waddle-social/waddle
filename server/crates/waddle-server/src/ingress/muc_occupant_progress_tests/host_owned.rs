use super::*;
use crate::server::routes::interpret::DeliveryExecutionContext;

fn sorted(mut resources: Vec<jid::FullJid>) -> Vec<jid::FullJid> {
    resources.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    resources
}

async fn host_owned_progress(fixture: IngressFixture) {
    let state = socket_tests::create_test_websocket_state().await;
    let room: jid::BareJid = "host-owned-progress@muc.example.com".parse().expect("room");
    let a: jid::FullJid = "alice@example.com/phone".parse().expect("A");
    let b: jid::FullJid = "ben@example.com/phone".parse().expect("B");
    let bot_bare: jid::BareJid = format!("observer@{}", state.deps.service_domains.extensions)
        .parse()
        .expect("configured extension actor");
    let bot = bot_bare.with_resource_str("bot").expect("bot resource");
    let mut submission = fixture.submission(Some("muc-host-owned-progress"), "room content");
    let sender = submission.sender.clone();
    let actor = state
        .deps
        .protocol
        .room_registry
        .ask(CreateRoom {
            room_jid: room.clone(),
            waddle_id: "host-owned-progress".into(),
            channel_id: "host-owned-progress".into(),
            config: Default::default(),
        })
        .await
        .expect("room actor");

    let (a_tx, mut a_rx) = tokio::sync::mpsc::channel(16);
    socket_tests::register_test_connection(&state, &a, a_tx).await;
    let (b_tx, mut b_rx) = tokio::sync::mpsc::channel(16);
    socket_tests::register_test_connection(&state, &b, b_tx).await;
    let (sender_tx, mut sender_rx) = tokio::sync::mpsc::channel(16);
    socket_tests::register_test_connection(&state, &sender, sender_tx).await;
    for (resource, nick) in [
        (&bot, "observer"),
        (&a, "alice"),
        (&b, "ben"),
        (&sender, "romeo"),
    ] {
        actor
            .ask(Join {
                session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
                nick: nick.into(),
                real_jid: resource.clone(),
                role: waddle_xmpp::Role::Participant,
                affiliation: waddle_xmpp::Affiliation::Member,
            })
            .await
            .expect("join");
    }

    let mut message = submission.plan.sanitized_message.clone();
    message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    message.to = Some(room.clone().into());
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
    let deps = {
        let mut deps = build_interpret_deps(&state, None);
        deps.inbox_storage = None;
        deps
    };
    plan_broadcast(&mut submission, &room, &message, &deps).await;

    assert!(
        submission.plan.plan.iter().any(|planned| matches!(
            &planned.effect,
            crate::server::routes::interpret::effects::Effect::External(
                ExternalEffect::Delivery(ExternalDeliveryEffect::HostOwnedCopy { target, .. })
            ) if target == &bot
        )),
        "the configured extension occupant must use HostOwnedCopy"
    );
    submission.plan.plan.sort_by_key(|planned| {
        matches!(
            &planned.effect,
            crate::server::routes::interpret::effects::Effect::External(
                ExternalEffect::Delivery(ExternalDeliveryEffect::RouteToPeer { jid, .. })
            ) if jid == &b
        )
    });
    let intent = submission
        .plan
        .intents
        .iter()
        .find(|intent| matches!(intent, IngressEffectIntent::RouteMucGroupchat { .. }))
        .expect("real room fanout");
    let receipt = receipt_key(intent).expect("kind-2 receipt");
    let first = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit");
    let key = first.message_key.expect("key");
    assert_eq!(
        sorted(
            first
                .route_progress
                .iter()
                .find(|progress| progress.receipt == receipt)
                .expect("occupant progress")
                .fanout
                .clone()
        ),
        sorted(vec![bot.clone(), a.clone(), b.clone()]),
        "the frozen non-reflection audience is exactly bot, A and B"
    );
    let mut tx = fixture.uow.begin().await.expect("inspect frozen route");
    let envelope = crate::ingress_uow::CanonicalMessageRepository::load_envelope(&mut tx, key)
        .await
        .expect("load envelope")
        .expect("canonical envelope");
    let manifest = envelope.room_manifest().expect("sealed room audience");
    let host_child = manifest
        .children
        .iter()
        .find(|child| child.receipt == receipt && child.target.as_ref() == Some(&bot))
        .expect("frozen host endpoint");
    assert_eq!(
        host_child.disposition,
        crate::ingress::RoomManifestChildDisposition::HostRoomCopyRequiresNoTransport,
        "trusted pre-admission classification durably resolves this endpoint without transport"
    );
    let human_child = manifest
        .children
        .iter()
        .find(|child| child.receipt == receipt && child.target.as_ref() == Some(&b))
        .expect("frozen human endpoint");
    assert_eq!(
        human_child.disposition,
        crate::ingress::RoomManifestChildDisposition::Delivery,
        "host policy cannot resolve a disconnected human recipient"
    );
    tx.commit().await.expect("inspection commit");
    let recovered =
        super::super::recovery_rebuild::rebuild(super::super::recovery_rebuild::RecoveryInput {
            key,
            envelope: &envelope,
            created_at: chrono::Utc::now(),
            recorded: std::slice::from_ref(intent),
            unreceipted: std::slice::from_ref(intent),
            route_progress: first.route_progress.clone(),
            host_owned_resources: vec![bot.clone()],
            departed_occupants: vec![],
            blocked_recipients: &[],
        })
        .expect("rebuild host-owned route");
    assert!(
        recovered.decision.external.iter().all(|effect| !matches!(
            effect,
            ExternalEffect::Delivery(ExternalDeliveryEffect::HostOwnedCopy { target, .. })
                if target == &bot
        )),
        "maintenance does not rebuild a policy-resolved host endpoint"
    );
    assert!(
        recovered.decision.external.iter().all(|effect| !matches!(
            effect,
            ExternalEffect::Delivery(ExternalDeliveryEffect::QueueDetached { resources, .. })
                if resources.contains(&bot)
        )),
        "maintenance never rebuilds a host-owned bot as detached transport work"
    );

    let stalled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
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
    assert!(
        stalled.load(std::sync::atomic::Ordering::SeqCst),
        "B reached the stalled delivery arm"
    );
    assert!(
        report
            .outcomes
            .iter()
            .any(|(_, outcome)| *outcome == ExternalOutcome::Uncertain),
        "B's timeout leaves the aggregate pending: {report:?}"
    );
    assert!(report.receipt_failures.is_empty(), "{report:?}");
    assert!(a_rx.try_recv().is_ok(), "A delivered");
    assert!(b_rx.try_recv().is_err(), "B failed before delivery");
    assert!(sender_rx.try_recv().is_ok(), "sender reflected");

    let mut tx = fixture.uow.begin().await.expect("inspect partial progress");
    assert_eq!(
        DeliveryProgressRepository::load(&mut tx, key, &receipt)
            .await
            .expect("partial progress"),
        sorted(vec![bot.clone(), a.clone()]),
        "the accepted no-transport bot disposition and delivered A are both settled"
    );
    assert!(!EffectReceiptRepository::contains(
        &mut tx,
        key,
        receipt.kind,
        &receipt.semantic_identity_hash,
    )
    .await
    .expect("aggregate receipt"));
    assert_eq!(
        crate::ingress_uow::SendAttemptRepository::status(
            &mut tx,
            &crate::ingress_uow::SendObligation {
                message: key,
                receipt: receipt.clone(),
                recipient: bot.clone(),
            },
        )
        .await
        .expect("inspect host send attempt"),
        None,
        "logical no-transport settlement cannot fabricate an endpoint send attempt"
    );
    tx.commit().await.expect("inspection commit");
    assert!(
        !terminalize_if_complete(&fixture.uow, key, DeliveryExecutionContext::Live.into())
            .await
            .expect("partial fanout stays pending")
    );

    plan_broadcast(&mut submission, &room, &message, &deps).await;
    if let effects::RoomExecutionPath::Local {
        manifest_snapshot: Some(snapshot),
        ..
    } = &mut submission.plan.room_execution
    {
        snapshot.host_no_transport.clear();
    }
    let retry = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("duplicate");
    let pending_occupant_targets = retry
        .external
        .iter()
        .filter_map(|effect| match effect {
            ExternalEffect::Delivery(ExternalDeliveryEffect::HostOwnedCopy { target, .. })
            | ExternalEffect::Delivery(ExternalDeliveryEffect::RouteToPeer {
                jid: target, ..
            }) if target != &sender => Some(target.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        pending_occupant_targets,
        vec![b.clone()],
        "retry preserves the original host disposition even if today's classification changes"
    );

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
    assert!(b_rx.try_recv().is_ok(), "B delivers on retry");
    assert!(a_rx.try_recv().is_err(), "A is not delivered twice");
    assert!(
        sender_rx.try_recv().is_err(),
        "the archived original reflection was already delivered"
    );

    let mut tx = fixture.uow.begin().await.expect("inspect final progress");
    assert_eq!(
        DeliveryProgressRepository::load(&mut tx, key, &receipt)
            .await
            .expect("final progress"),
        sorted(vec![bot.clone(), a, b]),
        "every frozen occupant is durably settled"
    );
    assert!(EffectReceiptRepository::contains(
        &mut tx,
        key,
        receipt.kind,
        &receipt.semantic_identity_hash,
    )
    .await
    .expect("aggregate receipt"));
    assert_eq!(
        crate::ingress_uow::SendAttemptRepository::status(
            &mut tx,
            &crate::ingress_uow::SendObligation {
                message: key,
                receipt: receipt.clone(),
                recipient: bot.clone(),
            },
        )
        .await
        .expect("inspect final host send attempt"),
        None,
        "terminalization preserves the bot's no-transport disposition without synthetic delivery"
    );
    let guard = fixture
        .db
        .guard()
        .await
        .expect("inspect physical host receipts");
    let mut rows = guard
        .query(
            "SELECT COUNT(*) FROM ingress_delivery_receipts WHERE resource = ?",
            crate::db_params![bot.to_string()],
        )
        .await
        .expect("inspect physical host receipts");
    let physical_receipts: i64 = rows
        .next()
        .await
        .expect("host receipt row")
        .expect("host receipt count")
        .get(0)
        .expect("host receipt count integer");
    assert_eq!(
        physical_receipts, 0,
        "logical policy settlement cannot fabricate a transport delivery receipt"
    );
    drop(rows);
    drop(guard);
    tx.commit().await.expect("inspection commit");
    assert!(
        terminalize_if_complete(&fixture.uow, key, DeliveryExecutionContext::Live.into())
            .await
            .expect("terminal")
    );
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_muc_occupant_progress_host_owned_copy() {
    host_owned_progress(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_muc_occupant_progress_host_owned_copy() {
    if let Some(fixture) = IngressFixture::postgres("muc_host_owned_progress").await {
        host_owned_progress(fixture).await;
    }
}
