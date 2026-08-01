use super::*;

// #229 PR12 — RouteToConnection delivers as PeerStanza
// -----------------------------------------------------------------

#[tokio::test]
async fn route_to_connection_full_jid_queues_peer_stanza_kind() {
    // Locks in the staged-cutover contract: full-JID
    // RouteToConnection events queue an OutboundStanza tagged
    // PeerStanza so the destination's main loop runs the
    // recipient pass before any wire write.
    use waddle_xmpp::registry::{DeliveryKind, UserRegistryActor};
    let registry = ConnectionRegistry::new();
    let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
    let bob: jid::FullJid = "bob@example.com/desk".parse().expect("jid");
    let (bob_tx, mut bob_rx) = tokio::sync::mpsc::channel(8);
    // ADR-0017 Slice 3: full-JID delivery now routes exclusively through the
    // authoritative actor (`deliver_peer_to_full` no longer has a DashMap
    // path), so register into both tiers and drive it with a `Some`
    // user_registry.
    register_into_both_tiers(&registry, &user_registry, &bob, bob_tx).await;

    let msg = chat_msg(jid("alice@example.com/web"), jid("bob@example.com"), "hi");
    let events = vec![OutboundEvent::RouteToConnection {
        jid: jid::Jid::from(bob.clone()),
        stanza: Box::new(Stanza::Message(msg)),
        call_setup: None,
    }];
    let _outcome = interpret(
        events,
        &Deps::registry_with_user_registry(&registry, &user_registry),
    )
    .await;

    let queued = drain_inbound(&mut bob_rx);
    assert_eq!(queued.len(), 1, "delivered to bob's queue exactly once");
    assert_eq!(
        queued[0].kind,
        DeliveryKind::PeerStanza,
        "RouteToConnection MUST tag PeerStanza so the destination main \
         loop runs the recipient pass; got {:?}",
        queued[0].kind
    );
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
        &Deps::registry_with_user_registry(&registry, &user_registry),
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
    // fallback, read from the same authoritative actor).

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

    let delivered = drain_inbound(&mut desk_rx);
    assert_eq!(
        delivered.len(),
        1,
        "no presence -> still delivered to connected resource as a legacy fallback"
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

/// ADR-0017 Phase 3 Slice 9 — the fifth unit test the DashMap-selection
/// retirement was deferred behind (this replaces the Slice-1 "filter drops the
/// stale extra" guard). With the transitional DashMap-liveness intersection
/// filter retired, a *sole stale extra* — a resource still present in the actor
/// whose underlying channel has already closed at teardown — is no longer
/// filtered out of selection. It SELF-HEALS instead: the bare-JID delivery
/// selects it, the actor's `TrySend*` hits `DroppedClosed`, `try_deliver`
/// evicts it from the actor, and the message is NOT lost — the shared fan-out
/// recipient pass persists it to the recipient's MAM independently of the
/// live-send outcome, and the recipient catches up via MAM. This is exactly
/// the "self-healing via `TrySendPeer` → `DroppedClosed` eviction" the Phase 1
/// completion note deferred to this slice.
#[tokio::test]
async fn route_to_connection_bare_jid_sole_stale_extra_self_heals_via_dropped_closed() {
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
    // Real teardown closes the resource's channel (the connection task's
    // receiver drops). The actor still holds the entry in the brief
    // lagging-unregister window — this is the exact "stale extra" the retired
    // filter used to pre-empt; now it self-heals at delivery time. Presence
    // stays "available" in the actor (teardown does not flip the atomic), so
    // selection still picks it.
    drop(desk_rx);

    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
    let blocking: Arc<dyn BlockingStorage> = Arc::new(InMemoryBlockingStorage::new());
    let dispatcher = pipelined_dispatcher();
    let deps = offline_pass_deps_with_user_registry(
        &registry,
        &user_registry,
        &mam,
        &inbox,
        &blocking,
        &dispatcher,
    );

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

    // No message loss: the recipient pass persisted the DM under bob's MAM even
    // though the sole live target's channel was dead.
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

    // Self-heal: the dead resource was evicted from the actor on the failed
    // send (DroppedClosed), so a subsequent selection sees no stale extra.
    let remaining = waddle_xmpp::registry::get_resources_for_user(&user_registry, &bob_bare).await;
    assert!(
        remaining.is_empty(),
        "the stale extra must be evicted from the actor by the DroppedClosed \
         eviction, not linger (self-healing replaces the retired filter)"
    );
}

/// ADR-0017 Phase 3 Slice 9: with the Slice-1 liveness filter retired, a stale
/// extra holding a UNIQUE top priority whose channel has closed is still
/// SELECTED (the actor ranks it top — there is no DashMap intersection to drop
/// it before the max-priority collapse), but its dead channel self-heals: the
/// `DroppedClosed` send evicts it, and the message is persisted (no loss) via
/// the recipient pass. Because selection collapsed to the ghost's priority tie
/// set, the live lower-priority resources are NOT live-delivered on that first
/// attempt (they catch up via MAM) — this is the intended, accepted behaviour
/// change from retiring the exact-parity filter, NOT a regression. On the NEXT
/// bare-JID delivery — after the ghost has been evicted — routing correctly
/// reaches the true live top-priority resource, proving convergence.
#[tokio::test]
async fn route_to_connection_bare_jid_stale_top_priority_extra_self_heals_to_live_lower() {
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
    // Real teardown of the top-priority resource: its channel closes, but the
    // actor still holds the entry (presence atomic not flipped) in the
    // lagging-unregister window.
    drop(stale_rx);

    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
    let blocking: Arc<dyn BlockingStorage> = Arc::new(InMemoryBlockingStorage::new());
    let dispatcher = pipelined_dispatcher();
    let deps = offline_pass_deps_with_user_registry(
        &registry,
        &user_registry,
        &mam,
        &inbox,
        &blocking,
        &dispatcher,
    );

    // First delivery: the ghost (pri 5) dominates selection; its dead channel
    // self-heals (DroppedClosed → evicted) and the message is persisted, but
    // the live pri-3/pri-1 resources are NOT live-delivered this round.
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
        "while the top-priority ghost dominates selection, the live pri-3 \
         resource is not live-delivered on the first attempt (catches up via MAM)"
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
    // The ghost was evicted by the DroppedClosed send, leaving the two live
    // resources.
    let mut remaining =
        waddle_xmpp::registry::get_resources_for_user(&user_registry, &bob_bare).await;
    remaining.sort_by_key(|j| j.to_string());
    assert_eq!(
        remaining,
        vec![bob_low.clone(), bob_mid.clone()],
        "the stale top-priority extra must be evicted (self-heal), leaving the \
         two live resources"
    );

    // Second delivery: with the ghost gone, selection now reaches the true live
    // top-priority resource (pri 3) — convergence, no filter required.
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
        "after the ghost is evicted, the true live top-priority resource (pri 3) \
         receives the next bare-JID delivery live"
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
