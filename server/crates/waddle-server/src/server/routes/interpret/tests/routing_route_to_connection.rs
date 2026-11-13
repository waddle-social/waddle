use super::{
    chat_msg, coordinator_for, drain_inbound, interpret, jid, offline_pass_deps_with_user_registry,
    pipelined_dispatcher, register_into_both_tiers, Arc, BlockingStorage, ConnectionRegistry, Deps,
    InboxStorage, IngressEffectIntent, MamStorage, OutboundEvent, Stanza,
};
use crate::ingress::IngressEffectCapture;
use kameo::actor::Spawn;

// Live full-JID messages prepare recipient effects before enqueue.
#[tokio::test]
async fn route_to_connection_full_jid_prepares_once_then_queues_direct_frame() {
    use crate::server::routes::interpret::effects::delivery::{
        ExternalDeliveryEffect, PeerDeliveryKind,
    };
    use crate::server::routes::interpret::effects::{Effect, ExternalEffect, PlanSink};
    use waddle_xmpp::registry::{DeliveryKind, UserRegistryActor};
    let registry = ConnectionRegistry::new();
    let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
    let bob: jid::FullJid = "bob@example.com/desk".parse().expect("jid");
    let (bob_tx, mut bob_rx) = tokio::sync::mpsc::channel(8);
    register_into_both_tiers(&registry, &user_registry, &bob, bob_tx).await;
    let coordinator = coordinator_for(&registry).await;
    let sink = PlanSink::new();
    let dispatcher = pipelined_dispatcher();
    let deps = Deps {
        effects: &sink,
        message_dispatcher: Some(&dispatcher),
        local_domain: "example.com",
        web_socket_state: Some(&coordinator),
        ..Deps::registry_with_user_registry(&registry, &user_registry)
    };
    let message = chat_msg(jid("alice@example.com/web"), bob.clone().into(), "hi");
    interpret(
        vec![OutboundEvent::RouteToConnection {
            jid: bob.clone().into(),
            stanza: Box::new(Stanza::Message(message)),
            call_setup: None,
        }],
        &deps,
    )
    .await;
    assert!(bob_rx.try_recv().is_err(), "preparation must not enqueue");
    let plan = sink.snapshot();
    let processed = plan
        .iter()
        .find_map(|effect| match &effect.effect {
            Effect::External(ExternalEffect::Delivery(ExternalDeliveryEffect::RouteToPeer {
                jid,
                stanza,
                kind: PeerDeliveryKind::DirectFrame,
                ..
            })) if jid == &bob => Some(stanza.as_ref()),
            _ => None,
        })
        .expect("processed original");
    let Stanza::Message(message) = processed else {
        panic!("message");
    };
    assert!(
        waddle_xmpp_core::xep0359::extract_stanza_id_by(message, &bob.to_bare().into()).is_some()
    );
    let immediate = Deps {
        effects: &crate::server::routes::interpret::effects::ImmediateSink,
        ..deps.clone()
    };
    assert_eq!(
        super::super::route_to_connection::deliver_direct_to_full_with_registered_remote(
            &immediate, &bob, processed
        )
        .await,
        super::super::FullJidDeliveryOutcome::Delivered
    );
    let queued = drain_inbound(&mut bob_rx);
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].kind, DeliveryKind::DirectFrame);
}

#[tokio::test]
async fn route_to_connection_full_jid_live_dm_records_route_direct_intent() {
    use waddle_xmpp::registry::UserRegistryActor;

    let registry = ConnectionRegistry::new();
    let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
    let bob: jid::FullJid = "bob@example.com/desk".parse().expect("jid");
    let (bob_tx, mut bob_rx) = tokio::sync::mpsc::channel(8);
    register_into_both_tiers(&registry, &user_registry, &bob, bob_tx).await;

    let capture = IngressEffectCapture::new();
    let deps = Deps::registry_with_user_registry(&registry, &user_registry)
        .with_ingress_effect_capture(Some(capture.clone()));

    let msg = chat_msg(jid("alice@example.com/web"), jid("bob@example.com"), "hi");
    let _ = interpret(
        vec![OutboundEvent::RouteToConnection {
            jid: jid::Jid::from(bob.clone()),
            stanza: Box::new(Stanza::Message(msg)),
            call_setup: None,
        }],
        &deps,
    )
    .await;

    let queued = drain_inbound(&mut bob_rx);
    assert_eq!(queued.len(), 1, "delivered to bob's queue exactly once");
    assert!(capture.snapshot().intents.iter().any(|intent| matches!(intent, IngressEffectIntent::RouteDirect { recipient, fanout, .. } if *recipient == "bob@example.com".parse::<jid::BareJid>().expect("bare") && *fanout == vec![bob.clone()])));
}

#[tokio::test]
async fn route_to_connection_bare_jid_selects_highest_priority_available_resources() {
    // RFC 6121 §8.5.2.1 resource selection: deliver to every
    // resource tied at the highest available priority. A
    // bare-JID `to` from the sender pass (handlers/route.rs
    // emits `message.to` verbatim) lands here; without selection
    // the cutover would silently drop bare-targeted 1:1 traffic.
    use waddle_xmpp::registry::{DeliveryKind, UserRegistryActor};
    let registry = ConnectionRegistry::new();
    let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
    let bob_desk: jid::FullJid = "bob@example.com/desk".parse().expect("jid");
    let bob_phone: jid::FullJid = "bob@example.com/phone".parse().expect("jid");
    let bob_tablet: jid::FullJid = "bob@example.com/tablet".parse().expect("jid");
    let (desk_tx, mut desk_rx) = tokio::sync::mpsc::channel(8);
    let (phone_tx, mut phone_rx) = tokio::sync::mpsc::channel(8);
    let (tablet_tx, mut tablet_rx) = tokio::sync::mpsc::channel(8);
    register_into_both_tiers(&registry, &user_registry, &bob_desk, desk_tx).await;
    register_into_both_tiers(&registry, &user_registry, &bob_phone, phone_tx).await;
    register_into_both_tiers(&registry, &user_registry, &bob_tablet, tablet_tx).await;
    // desk + phone available at priority 5 (tied); tablet at
    // lower priority 1. Tablet must NOT receive. `update_presence`
    // mutates the shared `Arc` atomics, so the actor sees these too.
    registry.update_presence(&bob_desk, true, 5);
    registry.update_presence(&bob_phone, true, 5);
    registry.update_presence(&bob_tablet, true, 1);
    let coordinator = coordinator_for(&registry).await;

    let msg = chat_msg(
        jid("alice@example.com/web"),
        jid("bob@example.com"),
        "hi bare",
    );
    let events = vec![OutboundEvent::RouteToConnection {
        jid: "bob@example.com".parse::<jid::Jid>().expect("bare jid"),
        stanza: Box::new(Stanza::Message(msg)),
        call_setup: None,
    }];
    let _outcome = interpret(
        events,
        &Deps {
            web_socket_state: Some(&coordinator),
            ..Deps::registry_with_user_registry(&registry, &user_registry)
        },
    )
    .await;

    let desk_q = drain_inbound(&mut desk_rx);
    let phone_q = drain_inbound(&mut phone_rx);
    let tablet_q = drain_inbound(&mut tablet_rx);
    assert_eq!(
        desk_q.len(),
        1,
        "desk (tied at max priority) gets the message"
    );
    assert_eq!(
        phone_q.len(),
        1,
        "phone (tied at max priority) gets the message"
    );
    assert!(
        tablet_q.is_empty(),
        "tablet (lower priority) is excluded by RFC 6121 §8.5.2.1.2"
    );
    for q in [&desk_q, &phone_q] {
        assert_eq!(q[0].kind, DeliveryKind::PeerStanza);
    }
}

#[tokio::test]
async fn route_to_connection_bare_jid_falls_back_to_connected_resources_without_presence() {
    // RFC 6121 §8.5.2.1.1 prefers presence-available resources
    // for bare-JID delivery, but Waddle falls back to *any*
    // connected resource when no resource has emitted
    // `<presence/>` yet (matching legacy `handle_message`
    // behaviour and unblocking integration tests where clients
    // bind without sending presence). This test pins that
    // fall-back: a bare-JID DM addressed to a user with one
    // registered-but-not-presence-available resource is delivered
    // to that resource instead of falling through to the offline
    // headless pass.
    use waddle_xmpp::registry::UserRegistryActor;
    let registry = ConnectionRegistry::new();
    let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
    let bob_desk: jid::FullJid = "bob@example.com/desk".parse().expect("jid");
    let (desk_tx, mut desk_rx) = tokio::sync::mpsc::channel(8);
    register_into_both_tiers(&registry, &user_registry, &bob_desk, desk_tx).await;
    // Registered but presence NOT made available — legacy
    // routing still delivers to this resource (tier-2 `GetResources`
    // fallback, read from the account coordinator).
    let coordinator = coordinator_for(&registry).await;

    let msg = chat_msg(jid("alice@example.com/web"), jid("bob@example.com"), "hi");
    let events = vec![OutboundEvent::RouteToConnection {
        jid: "bob@example.com".parse::<jid::Jid>().expect("bare jid"),
        stanza: Box::new(Stanza::Message(msg)),
        call_setup: None,
    }];
    let _outcome = interpret(
        events,
        &Deps {
            web_socket_state: Some(&coordinator),
            ..Deps::registry_with_user_registry(&registry, &user_registry)
        },
    )
    .await;

    let delivered = drain_inbound(&mut desk_rx);
    assert_eq!(
        delivered.len(),
        1,
        "no presence -> still delivered to connected resource as a legacy fallback"
    );
}

#[tokio::test]
async fn route_to_connection_capture_skips_blocked_bare_jid_delivery() {
    use crate::ingress::IngressEffectCapture;
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    use waddle_xmpp::registry::UserRegistryActor;
    use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

    let registry = ConnectionRegistry::new();
    let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
    let bob_desk: jid::FullJid = "bob@example.com/desk".parse().expect("jid");
    let (desk_tx, mut desk_rx) = tokio::sync::mpsc::channel(8);
    register_into_both_tiers(&registry, &user_registry, &bob_desk, desk_tx).await;
    registry.update_presence(&bob_desk, true, 5);

    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
    let blocking_concrete = Arc::new(InMemoryBlockingStorage::new());
    blocking_concrete.set_blocklist(
        "bob@example.com".parse().expect("bob bare"),
        vec!["alice@example.com".parse().expect("alice bare")],
    );
    let blocking: Arc<dyn BlockingStorage> = blocking_concrete;
    let dispatcher = pipelined_dispatcher();
    let capture = IngressEffectCapture::new();
    let deps = offline_pass_deps_with_user_registry(
        &registry,
        &user_registry,
        &mam,
        &inbox,
        &blocking,
        &dispatcher,
    )
    .with_ingress_effect_capture(Some(capture.clone()));

    let _ = interpret(
        vec![OutboundEvent::RouteToConnection {
            jid: "bob@example.com".parse::<jid::Jid>().expect("bare"),
            stanza: Box::new(Stanza::Message(chat_msg(
                jid("alice@example.com/web"),
                jid("bob@example.com"),
                "blocked",
            ))),
            call_setup: None,
        }],
        &deps,
    )
    .await;

    assert!(
        drain_inbound(&mut desk_rx).is_empty(),
        "blocked fan-out must not deliver a live copy"
    );
    assert!(
        !capture.snapshot().intents.iter().any(|intent| {
            matches!(
                intent,
                IngressEffectIntent::RouteDirect { recipient, .. }
                    if recipient == &"bob@example.com".parse::<jid::BareJid>().expect("bare")
            )
        }),
        "blocked bare-JID delivery must not record RouteDirect"
    );
}

/// Slice 1 cutover proof: the candidate set is sourced from the
/// actor-authoritative `UserActor`, NOT the DashMap. A resource present +
/// presence-available in the DashMap but never mirrored into the
/// `UserRegistryActor` (so `GetUser` returns `Ok(None)`) must NOT be selected —
/// if selection still read the DashMap this would deliver. Pins that the actor
/// gates the candidate set.
#[tokio::test]
async fn route_to_connection_bare_jid_ignores_resource_absent_from_actor() {
    use waddle_xmpp::registry::UserRegistryActor;
    let registry = ConnectionRegistry::new();
    let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
    let bob_desk: jid::FullJid = "bob@example.com/desk".parse().expect("jid");
    let (desk_tx, mut desk_rx) = tokio::sync::mpsc::channel(8);
    // DashMap ONLY — never mirrored into the actor tree.
    registry.register_with_carbons(bob_desk.clone(), desk_tx, false);
    registry.update_presence(&bob_desk, true, 5);

    let msg = chat_msg(jid("alice@example.com/web"), jid("bob@example.com"), "hi");
    let events = vec![OutboundEvent::RouteToConnection {
        jid: "bob@example.com".parse::<jid::Jid>().expect("bare jid"),
        stanza: Box::new(Stanza::Message(msg)),
        call_setup: None,
    }];
    let _outcome = interpret(
        events,
        &Deps::registry_with_user_registry(&registry, &user_registry),
    )
    .await;

    assert!(
        drain_inbound(&mut desk_rx).is_empty(),
        "a DashMap-only resource (absent from the actor) must NOT be selected; \
         the actor is the authoritative candidate source"
    );
}

/// A sole bound resource whose channel has closed (its connection is tearing
/// down and has not retired its binding yet) is still the coordinator's
/// destination for a bare-JID message. The live send fails, and the message
/// is not lost: the recipient pass archives it to the recipient's MAM
/// independently of the live-send outcome. Delivery does not retire the
/// binding; the connection's teardown does (`2138-account-coordinator.md`,
/// "Delivery").
#[tokio::test]
async fn route_to_connection_bare_jid_sole_closed_binding_is_archived_without_loss() {
    use waddle_xmpp::inbox::storage::{InMemoryInboxStorage, InboxStorage};
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    use waddle_xmpp::registry::UserRegistryActor;
    use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

    let registry = ConnectionRegistry::new();
    let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
    let bob_desk: jid::FullJid = "bob@example.com/desk".parse().expect("jid");
    let (desk_tx, desk_rx) = tokio::sync::mpsc::channel(8);
    register_into_both_tiers(&registry, &user_registry, &bob_desk, desk_tx).await;
    registry.update_presence(&bob_desk, true, 5);
    let coordinator = coordinator_for(&registry).await;
    // Teardown closes the resource's channel before the connection retires
    // its binding.
    drop(desk_rx);

    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
    let blocking: Arc<dyn BlockingStorage> = Arc::new(InMemoryBlockingStorage::new());
    let dispatcher = pipelined_dispatcher();
    let deps = Deps {
        web_socket_state: Some(&coordinator),
        ..offline_pass_deps_with_user_registry(
            &registry,
            &user_registry,
            &mam,
            &inbox,
            &blocking,
            &dispatcher,
        )
    };

    let msg = chat_msg(
        jid("alice@example.com/web"),
        jid("bob@example.com"),
        "no-loss",
    );
    let events = vec![OutboundEvent::RouteToConnection {
        jid: "bob@example.com".parse::<jid::Jid>().expect("bare jid"),
        stanza: Box::new(Stanza::Message(msg)),
        call_setup: None,
    }];
    let _ = interpret(events, &deps).await;

    let bob_bare: jid::BareJid = "bob@example.com".parse().expect("bare");
    let archive = mam
        .query_messages(
            &bob_bare,
            waddle_xmpp::mam::MamArchiveKind::Personal,
            &Default::default(),
        )
        .await
        .expect("query bob");
    assert_eq!(
        archive.messages.len(),
        1,
        "the DM must be persisted to the recipient's MAM (no loss) despite the \
         sole selected resource having a dead channel"
    );
    assert_eq!(archive.messages[0].body.as_deref(), Some("no-loss"));
    assert_eq!(
        coordinator
            .deps
            .app_state
            .account_authority()
            .resources(&bob_bare)
            .await
            .expect("coordinator resource read"),
        vec![bob_desk],
        "a failed send does not retire the binding; its connection's teardown does"
    );
}

/// A closed binding at the unique top priority is still selected for a
/// bare-JID message until its connection retires it, so the live
/// lower-priority resources are not live-delivered on that attempt (they
/// catch up through MAM, which receives the message). Once the binding is
/// retired, the next bare-JID delivery reaches the live top-priority
/// resource.
#[tokio::test]
async fn route_to_connection_bare_jid_closed_top_priority_binding_routes_until_retired() {
    use waddle_xmpp::inbox::storage::{InMemoryInboxStorage, InboxStorage};
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    use waddle_xmpp::registry::UserRegistryActor;
    use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

    let registry = ConnectionRegistry::new();
    let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
    let bob_mid: jid::FullJid = "bob@example.com/mid".parse().expect("jid");
    let bob_low: jid::FullJid = "bob@example.com/low".parse().expect("jid");
    let bob_stale: jid::FullJid = "bob@example.com/stale".parse().expect("jid");
    let (mid_tx, mut mid_rx) = tokio::sync::mpsc::channel(8);
    let (low_tx, mut low_rx) = tokio::sync::mpsc::channel(8);
    let (stale_tx, stale_rx) = tokio::sync::mpsc::channel(8);
    register_into_both_tiers(&registry, &user_registry, &bob_mid, mid_tx).await;
    register_into_both_tiers(&registry, &user_registry, &bob_low, low_tx).await;
    register_into_both_tiers(&registry, &user_registry, &bob_stale, stale_tx).await;
    registry.update_presence(&bob_mid, true, 3);
    registry.update_presence(&bob_low, true, 1);
    registry.update_presence(&bob_stale, true, 5);
    let coordinator = coordinator_for(&registry).await;
    // Teardown of the top-priority resource closes its channel before its
    // connection retires the binding.
    drop(stale_rx);

    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
    let blocking: Arc<dyn BlockingStorage> = Arc::new(InMemoryBlockingStorage::new());
    let dispatcher = pipelined_dispatcher();
    let deps = Deps {
        web_socket_state: Some(&coordinator),
        ..offline_pass_deps_with_user_registry(
            &registry,
            &user_registry,
            &mam,
            &inbox,
            &blocking,
            &dispatcher,
        )
    };

    let events = vec![OutboundEvent::RouteToConnection {
        jid: "bob@example.com".parse::<jid::Jid>().expect("bare jid"),
        stanza: Box::new(Stanza::Message(chat_msg(
            jid("alice@example.com/web"),
            jid("bob@example.com"),
            "first",
        ))),
        call_setup: None,
    }];
    let _ = interpret(events, &deps).await;

    assert!(
        drain_inbound(&mut mid_rx).is_empty(),
        "while the closed top-priority binding is active, the live pri-3 \
         resource is not live-delivered (catches up via MAM)"
    );
    assert!(drain_inbound(&mut low_rx).is_empty());
    let bob_bare: jid::BareJid = "bob@example.com".parse().expect("bare");
    assert_eq!(
        mam.query_messages(
            &bob_bare,
            waddle_xmpp::mam::MamArchiveKind::Personal,
            &Default::default()
        )
        .await
        .expect("query bob")
        .messages
        .len(),
        1,
        "no message loss: the first DM is persisted to the recipient's MAM"
    );
    assert_eq!(
        coordinator
            .deps
            .app_state
            .account_authority()
            .resources(&bob_bare)
            .await
            .expect("coordinator resource read"),
        vec![bob_low.clone(), bob_mid.clone(), bob_stale.clone()],
        "a failed send does not retire the binding; its connection's teardown does"
    );

    // The connection's teardown retires its binding.
    registry.unregister(&bob_stale);
    crate::server::routes::websocket::test_bindings::sync_registry_bindings(
        &coordinator,
        &registry,
    )
    .await;

    let events = vec![OutboundEvent::RouteToConnection {
        jid: "bob@example.com".parse::<jid::Jid>().expect("bare jid"),
        stanza: Box::new(Stanza::Message(chat_msg(
            jid("alice@example.com/web"),
            jid("bob@example.com"),
            "second",
        ))),
        call_setup: None,
    }];
    let _ = interpret(events, &deps).await;

    assert_eq!(
        drain_inbound(&mut mid_rx).len(),
        1,
        "after the closed binding is retired, the live top-priority resource \
         (pri 3) receives the next bare-JID delivery live"
    );
    assert!(
        drain_inbound(&mut low_rx).is_empty(),
        "the live pri-1 resource is still not a top-priority destination"
    );
}

/// Slice 1 degradation path: when the `UserRegistryActor` is dead (crashed /
/// poisoned), `GetUser` errors and selection degrades to empty — the caller
/// runs the offline/headless pass rather than delivering live. No live
/// delivery reaches any DashMap resource.
#[tokio::test]
async fn route_to_connection_bare_jid_degrades_to_offline_on_dead_user_registry() {
    use waddle_xmpp::registry::UserRegistryActor;
    let registry = ConnectionRegistry::new();
    let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
    let bob_desk: jid::FullJid = "bob@example.com/desk".parse().expect("jid");
    let (desk_tx, mut desk_rx) = tokio::sync::mpsc::channel(8);
    register_into_both_tiers(&registry, &user_registry, &bob_desk, desk_tx).await;
    registry.update_presence(&bob_desk, true, 5);
    // Kill the registry actor so the GetUser ask errors.
    user_registry.kill();
    tokio::task::yield_now().await;

    let msg = chat_msg(jid("alice@example.com/web"), jid("bob@example.com"), "hi");
    let events = vec![OutboundEvent::RouteToConnection {
        jid: "bob@example.com".parse::<jid::Jid>().expect("bare jid"),
        stanza: Box::new(Stanza::Message(msg)),
        call_setup: None,
    }];
    let _outcome = interpret(
        events,
        &Deps::registry_with_user_registry(&registry, &user_registry),
    )
    .await;

    assert!(
        drain_inbound(&mut desk_rx).is_empty(),
        "a dead UserRegistryActor must degrade selection to offline, not \
         deliver live"
    );
}
