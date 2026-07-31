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

    let msg = chat_msg("alice@example.com/web", "bob@example.com", "hi");
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

    let msg = chat_msg("alice@example.com/web", "bob@example.com", "hi bare");
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

    let msg = chat_msg("alice@example.com/web", "bob@example.com", "hi");
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

    let msg = chat_msg("alice@example.com/web", "bob@example.com", "hi");
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

    let msg = chat_msg("alice@example.com/web", "bob@example.com", "no-loss");
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
            "alice@example.com/web",
            "bob@example.com",
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
            "alice@example.com/web",
            "bob@example.com",
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

    let msg = chat_msg("alice@example.com/web", "bob@example.com", "hi");
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

// -----------------------------------------------------------------
// #1106 — shared fan-out recipient pass: blocklist-load failure
// -----------------------------------------------------------------

/// BlockingStorage stub whose reads always fail, simulating a
/// transient storage outage during the shared fan-out pass.
struct FailingBlockingStorage;

#[async_trait::async_trait]
impl waddle_xmpp::xep::xep0191::BlockingStorage for FailingBlockingStorage {
    async fn list_blocked_jids(
        &self,
        _user: &jid::BareJid,
    ) -> Result<Vec<jid::BareJid>, waddle_xmpp::xep::xep0191::BlockingStorageError> {
        Err(waddle_xmpp::xep::xep0191::BlockingStorageError::new(
            std::io::Error::other("storage down"),
        ))
    }

    async fn list_blocked_jid_entries(
        &self,
        _user: &jid::BareJid,
    ) -> Result<Vec<jid::Jid>, waddle_xmpp::xep::xep0191::BlockingStorageError> {
        Err(waddle_xmpp::xep::xep0191::BlockingStorageError::new(
            std::io::Error::other("storage down"),
        ))
    }
}

#[tokio::test]
async fn fanout_pass_blocklist_failure_falls_back_to_legacy_per_resource_delivery() {
    // A transient blocklist-storage error must not drop a DM to LIVE
    // recipients: the legacy per-resource PeerStanza path still runs
    // each recipient connection's own state machine, whose bind-time
    // blocklist snapshot keeps XEP-0191 enforcement intact.
    use waddle_xmpp::registry::DeliveryKind;

    let registry = ConnectionRegistry::new();
    let user_registry = waddle_xmpp::registry::UserRegistryActor::spawn(
        waddle_xmpp::registry::UserRegistryActor::new(),
    );
    let bob: jid::FullJid = "bob@example.com/web".parse().expect("bob jid");
    let (bob_tx, mut bob_rx) = tokio::sync::mpsc::channel(8);
    // ADR-0017 Slice 1: bare-JID selection reads the actor tree, so register
    // bob into both tiers. bob sends no presence, so tier-2 `GetResources`
    // (the bound-without-presence fallback) resolves him as the live target.
    register_into_both_tiers(&registry, &user_registry, &bob, bob_tx).await;

    let blocking: Arc<dyn waddle_xmpp::xep::xep0191::BlockingStorage> =
        Arc::new(FailingBlockingStorage);
    let dispatcher = pipelined_dispatcher();
    let deps = Deps {
        connection_registry: &registry,
        user_registry: Some(&user_registry),
        sm_session_registry: None,
        mam_storage: None,
        inbox_storage: None,
        extension_manager: None,
        room_registry: None,
        web_socket_state: None,
        authenticated_session: None,
        local_domain: "example.com",
        blocking_storage: Some(&blocking),
        message_dispatcher: Some(&dispatcher),
        pending_delivery_storage: None,
        ordered_relay_origin: None,
        sfu: None,
    };

    let msg = chat_msg("alice@example.com/web", "bob@example.com", "must arrive");
    let events = vec![OutboundEvent::RouteToConnection {
        jid: "bob@example.com".parse::<jid::Jid>().expect("bare"),
        stanza: Box::new(Stanza::Message(msg)),
        call_setup: None,
    }];
    let _ = interpret(events, &deps).await;

    let delivered = tokio::time::timeout(std::time::Duration::from_secs(2), bob_rx.recv())
        .await
        .expect("delivery must not time out")
        .expect("channel open");
    assert_eq!(
        delivered.kind,
        DeliveryKind::PeerStanza,
        "fallback delivers via the legacy per-resource recipient pass"
    );
    let Stanza::Message(delivered_msg) = delivered.stanza else {
        panic!("expected message stanza");
    };
    assert_eq!(
        delivered_msg.bodies.values().next().map(|b| b.as_str()),
        Some("must arrive")
    );
}

#[tokio::test]
async fn fanout_pass_applies_archive_id_rewrite_to_the_delivered_stanza() {
    // XEP-0359 live/MAM id parity under origin-id retry: when the
    // recipient archive dedupes the store to an EXISTING row (same
    // origin-id already archived), the resulting ArchiveIdRewrite must
    // reach the wire copy the shared fan-out pass delivers — otherwise
    // live resources see a recipient <stanza-id/> that no archive row
    // carries, breaking client-side live/MAM dedupe.
    use waddle_xmpp::registry::DeliveryKind;
    use waddle_xmpp_core::xep0359::{build_origin_id_element, extract_stanza_id_by};

    let registry = ConnectionRegistry::new();
    let user_registry = waddle_xmpp::registry::UserRegistryActor::spawn(
        waddle_xmpp::registry::UserRegistryActor::new(),
    );
    let bob: jid::FullJid = "bob@example.com/web".parse().expect("bob jid");
    let (bob_tx, mut bob_rx) = tokio::sync::mpsc::channel(8);
    // ADR-0017 Slice 1: bare-JID selection reads the actor tree; bob is live
    // (bound without presence), resolved via tier-2 `GetResources`.
    register_into_both_tiers(&registry, &user_registry, &bob, bob_tx).await;

    let mam: Arc<dyn MamStorage> =
        Arc::new(waddle_xmpp::mam::storage::InMemoryMamStorage::default());
    let inbox: Arc<dyn InboxStorage> =
        Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
    let blocking: Arc<dyn waddle_xmpp::xep::xep0191::BlockingStorage> =
        Arc::new(waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new());
    let dispatcher = pipelined_dispatcher();
    let deps = offline_pass_deps_with_user_registry(
        &registry,
        &user_registry,
        &mam,
        &inbox,
        &blocking,
        &dispatcher,
    );

    let dm = || {
        let mut m = chat_msg("alice@example.com/web", "bob@example.com", "retry me");
        m.payloads.push(build_origin_id_element("origin-retry-1"));
        m
    };

    // First delivery: archives a row under bob's recipient stamp.
    let _ = interpret(
        vec![OutboundEvent::RouteToConnection {
            jid: "bob@example.com".parse::<jid::Jid>().expect("bare"),
            stanza: Box::new(Stanza::Message(dm())),
            call_setup: None,
        }],
        &deps,
    )
    .await;
    // Drain the first delivery.
    while bob_rx.try_recv().is_ok() {}

    // Retry with the same origin-id: the archive store dedupes to the
    // existing row and reports its id via ArchiveIdRewrite.
    let _ = interpret(
        vec![OutboundEvent::RouteToConnection {
            jid: "bob@example.com".parse::<jid::Jid>().expect("bare"),
            stanza: Box::new(Stanza::Message(dm())),
            call_setup: None,
        }],
        &deps,
    )
    .await;
    let delivered = tokio::time::timeout(std::time::Duration::from_secs(2), bob_rx.recv())
        .await
        .expect("second delivery must not time out")
        .expect("channel open");
    assert_eq!(
        delivered.kind,
        DeliveryKind::DirectFrame,
        "shared fan-out pass delivers the processed stanza directly"
    );
    let Stanza::Message(delivered_msg) = delivered.stanza else {
        panic!("expected message stanza");
    };

    let bob_bare: jid::BareJid = "bob@example.com".parse().expect("bare");
    let archive = mam
        .query_messages(
            &bob_bare,
            waddle_xmpp::mam::MamArchiveKind::Personal,
            &Default::default(),
        )
        .await
        .expect("query bob archive");
    assert_eq!(
        archive.messages.len(),
        1,
        "origin-id retry dedupes to one row"
    );
    let archived_id = archive.messages[0].id.clone();

    let delivered_stanza_id = extract_stanza_id_by(&delivered_msg, &jid::Jid::from(bob_bare));
    assert_eq!(
        delivered_stanza_id.as_deref(),
        Some(archived_id.as_str()),
        "the delivered recipient <stanza-id/> must match the deduped archive row"
    );
}

// ---------------------------------------------------------------------
// #1244 — RFC 6121 §8.5.3.2.1: full-JID DM with no matching resource
// falls back to bare-JID delivery semantics instead of dropping.
// ---------------------------------------------------------------------

#[tokio::test]
async fn route_full_jid_dm_offline_resource_falls_back_to_other_live_resource() {
    // Alice keeps replying to bob@x/old-resource after Bob reconnected
    // under /desk. RFC 6121 §8.5.3.2.1: with no resource matching the
    // full JID, treat the stanza as addressed to the bare JID — /desk
    // must receive it (previously: silent drop).
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    use waddle_xmpp::registry::{DeliveryKind, UserRegistryActor};
    use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

    let registry = ConnectionRegistry::new();
    let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
    let bob_desk: jid::FullJid = "bob@example.com/desk".parse().expect("jid");
    let (desk_tx, mut desk_rx) = tokio::sync::mpsc::channel(8);
    register_into_both_tiers(&registry, &user_registry, &bob_desk, desk_tx).await;
    registry.update_presence(&bob_desk, true, 0);

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

    let msg = chat_msg("alice@example.com/web", "bob@example.com/gone", "hi bob");
    let outcome = interpret(
        vec![OutboundEvent::RouteToConnection {
            jid: "bob@example.com/gone".parse::<jid::Jid>().expect("full"),
            stanza: Box::new(Stanza::Message(msg)),
            call_setup: None,
        }],
        &deps,
    )
    .await;
    assert!(
        outcome.frames.is_empty(),
        "fallback delivery must not synthesize an error to the sender"
    );

    let delivered = drain_inbound(&mut desk_rx);
    assert_eq!(
        delivered.len(),
        1,
        "RFC 6121 §8.5.3.2.1: bare-JID fallback delivers to bob's live resource"
    );
    assert_eq!(
        delivered[0].kind,
        DeliveryKind::DirectFrame,
        "fallback goes through the shared recipient pass (processed copy)"
    );

    let bob_bare: jid::BareJid = "bob@example.com".parse().expect("bare");
    let bob_archive = mam
        .query_messages(
            &bob_bare,
            waddle_xmpp::mam::MamArchiveKind::Personal,
            &Default::default(),
        )
        .await
        .expect("query bob");
    assert_eq!(
        bob_archive.messages.len(),
        1,
        "recipient pass ran exactly once for the fallback delivery"
    );
}

#[tokio::test]
async fn route_full_jid_dm_no_resources_stores_offline() {
    // Full-JID DM, recipient has no resources at all: §8.5.3.2.1 →
    // §8.5.2 → offline handling (headless recipient pass persists
    // archive + inbox). Previously the message vanished.
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
    let blocking: Arc<dyn BlockingStorage> = Arc::new(InMemoryBlockingStorage::new());
    let dispatcher = pipelined_dispatcher();
    let deps = offline_pass_deps(&registry, &mam, &inbox, &blocking, &dispatcher);

    let msg = chat_msg("alice@example.com/web", "bob@example.com/gone", "offline?");
    let _ = interpret(
        vec![OutboundEvent::RouteToConnection {
            jid: "bob@example.com/gone".parse::<jid::Jid>().expect("full"),
            stanza: Box::new(Stanza::Message(msg)),
            call_setup: None,
        }],
        &deps,
    )
    .await;

    let bob_bare: jid::BareJid = "bob@example.com".parse().expect("bare");
    let bob_archive = mam
        .query_messages(
            &bob_bare,
            waddle_xmpp::mam::MamArchiveKind::Personal,
            &Default::default(),
        )
        .await
        .expect("query bob");
    assert_eq!(
        bob_archive.messages.len(),
        1,
        "full-JID DM to a fully-offline user must be stored, not dropped"
    );
}

#[tokio::test]
async fn route_full_jid_dm_to_detached_resource_runs_recipient_pipeline() {
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    use waddle_xmpp::stream_management::SmSessionRegistry;
    use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

    let registry = ConnectionRegistry::new();
    let bob_phone: jid::FullJid = "bob@example.com/phone".parse().expect("jid");
    let sm = Arc::new(InMemorySmSessionRegistry::new());
    sm.store_session(detached_dm_session("bob-phone-stream", &bob_phone))
        .await
        .expect("store detached session");

    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
    let blocking: Arc<dyn BlockingStorage> = Arc::new(InMemoryBlockingStorage::new());
    let dispatcher = pipelined_dispatcher();
    let deps = Deps {
        sm_session_registry: Some(&sm),
        ..offline_pass_deps(&registry, &mam, &inbox, &blocking, &dispatcher)
    };

    let msg = chat_msg(
        "alice@example.com/web",
        "bob@example.com/phone",
        "resume me",
    );
    let _ = interpret(
        vec![OutboundEvent::RouteToConnection {
            jid: "bob@example.com/phone".parse::<jid::Jid>().expect("full"),
            stanza: Box::new(Stanza::Message(msg)),
            call_setup: None,
        }],
        &deps,
    )
    .await;

    // XEP-0313 §6.1: the recipient archive captured the message.
    let bob_bare: jid::BareJid = "bob@example.com".parse().expect("bare");
    let bob_archive = mam
        .query_messages(
            &bob_bare,
            waddle_xmpp::mam::MamArchiveKind::Personal,
            &Default::default(),
        )
        .await
        .expect("query bob");
    assert_eq!(
        bob_archive.messages.len(),
        1,
        "detached full-JID DM must land in the recipient's archive"
    );

    // XEP-0359 §5: the queued replay copy is the PROCESSED stanza and
    // carries the recipient <stanza-id by='bob@example.com'/>.
    let session = sm
        .peek_session("bob-phone-stream")
        .await
        .expect("peek ok")
        .expect("session present");
    assert_eq!(
        session.unacked_stanzas.len(),
        1,
        "processed DM queued for XEP-0198 replay"
    );
    let queued_element: Element = session.unacked_stanzas[0]
        .stanza_xml
        .parse()
        .expect("queued stanza XML parses");
    let queued =
        xmpp_parsers::message::Message::try_from(queued_element).expect("queued message parses");
    let by: jid::Jid = "bob@example.com".parse().expect("jid");
    let recipient_stanza_id = waddle_xmpp_core::xep0359::extract_stanza_id_by(&queued, &by);
    assert!(
        recipient_stanza_id.is_some(),
        "replay copy must carry the recipient-side stanza-id (XEP-0359 §3); \
         payloads: {:?}",
        queued.payloads
    );
    assert_eq!(
        recipient_stanza_id.as_deref(),
        Some(bob_archive.messages[0].id.as_str()),
        "wire stanza-id and archive row id must agree"
    );
}

// ---------------------------------------------------------------------
// #1246 — RFC 6121 §8.5.1: message to a nonexistent local account is
// bounced with <service-unavailable/>, never persisted.
// ---------------------------------------------------------------------

#[tokio::test]
async fn route_bare_jid_message_to_nonexistent_local_user_bounces() {
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

    let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
    let blocking: Arc<dyn BlockingStorage> = Arc::new(InMemoryBlockingStorage::new());
    let dispatcher = pipelined_dispatcher();
    let deps = Deps {
        web_socket_state: Some(&state),
        ..offline_pass_deps(&registry, &mam, &inbox, &blocking, &dispatcher)
    };

    let msg = chat_msg("alice@example.com/web", "typo@example.com", "anyone?");
    let outcome = interpret(
        vec![OutboundEvent::RouteToConnection {
            jid: "typo@example.com".parse::<jid::Jid>().expect("bare"),
            stanza: Box::new(Stanza::Message(msg)),
            call_setup: None,
        }],
        &deps,
    )
    .await;

    assert_eq!(
        outcome.frames.len(),
        1,
        "sender must receive a bounce for a nonexistent local account"
    );
    assert!(
        outcome.frames[0].contains("service-unavailable"),
        "RFC 6121 §8.5.1: the bounce is <service-unavailable/>; got {}",
        outcome.frames[0]
    );
    assert!(
        outcome.frames[0].contains("type=\"error\"") || outcome.frames[0].contains("type='error'"),
        "bounce is a message of type error; got {}",
        outcome.frames[0]
    );

    let typo_bare: jid::BareJid = "typo@example.com".parse().expect("bare");
    let typo_archive = mam
        .query_messages(
            &typo_bare,
            waddle_xmpp::mam::MamArchiveKind::Personal,
            &Default::default(),
        )
        .await
        .expect("query typo");
    assert!(
        typo_archive.messages.is_empty(),
        "no MAM rows may be created for a nonexistent account"
    );
}

#[tokio::test]
async fn route_bare_jid_message_to_existing_oidc_user_persists_offline() {
    // Two-table identity: an OIDC-provisioned account exists only in
    // `users` (no native_users row). The existence gate must accept it
    // and run the normal offline/headless persistence.
    use crate::db::actor::DbExecute;
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

    let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .ask(DbExecute {
            sql: "INSERT INTO users \
                  (jid, username, xmpp_localpart, display_name, avatar_url, primary_email, created_at, updated_at) \
                  VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
                .to_string(),
            params: vec![
                "bob@example.com".into(),
                "bob".into(),
                "bob".into(),
                "Bob".into(),
                crate::db::Value::NullText,
                crate::db::Value::NullText,
                "2026-01-01T00:00:00Z".into(),
                "2026-01-01T00:00:00Z".into(),
            ],
        })
        .await
        .expect("seed oidc user");

    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
    let blocking: Arc<dyn BlockingStorage> = Arc::new(InMemoryBlockingStorage::new());
    let dispatcher = pipelined_dispatcher();
    let deps = Deps {
        web_socket_state: Some(&state),
        ..offline_pass_deps(&registry, &mam, &inbox, &blocking, &dispatcher)
    };

    let msg = chat_msg("alice@example.com/web", "bob@example.com", "hello bob");
    let outcome = interpret(
        vec![OutboundEvent::RouteToConnection {
            jid: "bob@example.com".parse::<jid::Jid>().expect("bare"),
            stanza: Box::new(Stanza::Message(msg)),
            call_setup: None,
        }],
        &deps,
    )
    .await;
    assert!(
        outcome.frames.is_empty(),
        "existing OIDC account must not be bounced"
    );

    let bob_bare: jid::BareJid = "bob@example.com".parse().expect("bare");
    let bob_archive = mam
        .query_messages(
            &bob_bare,
            waddle_xmpp::mam::MamArchiveKind::Personal,
            &Default::default(),
        )
        .await
        .expect("query bob");
    assert_eq!(
        bob_archive.messages.len(),
        1,
        "offline persistence runs for the OIDC-only account"
    );
}

// ---------------------------------------------------------------------
// #1266 item 4 — RFC 6121 §8.5.2.1.1: bare-JID delivery MUST NOT reach
// resources that advertised a negative presence priority.
// ---------------------------------------------------------------------

#[tokio::test]
async fn route_to_connection_bare_jid_skips_negative_priority_resources() {
    use waddle_xmpp::registry::UserRegistryActor;
    let registry = ConnectionRegistry::new();
    let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
    let bob_desk: jid::FullJid = "bob@example.com/desk".parse().expect("jid");
    let bob_phone: jid::FullJid = "bob@example.com/phone".parse().expect("jid");
    let (desk_tx, mut desk_rx) = tokio::sync::mpsc::channel(8);
    let (phone_tx, mut phone_rx) = tokio::sync::mpsc::channel(8);
    register_into_both_tiers(&registry, &user_registry, &bob_desk, desk_tx).await;
    register_into_both_tiers(&registry, &user_registry, &bob_phone, phone_tx).await;
    // desk explicitly opts out of bare-JID delivery (priority -1);
    // phone is connected but has not sent presence (tier-2 fallback
    // territory).
    registry.update_presence(&bob_desk, true, -1);

    let msg = chat_msg("alice@example.com/web", "bob@example.com", "hi bare");
    let _ = interpret(
        vec![OutboundEvent::RouteToConnection {
            jid: "bob@example.com".parse::<jid::Jid>().expect("bare"),
            stanza: Box::new(Stanza::Message(msg)),
            call_setup: None,
        }],
        &Deps::registry_with_user_registry(&registry, &user_registry),
    )
    .await;

    assert!(
        drain_inbound(&mut desk_rx).is_empty(),
        "RFC 6121 §8.5.2.1.1: negative-priority resource must not receive \
         bare-JID delivery"
    );
    assert_eq!(
        drain_inbound(&mut phone_rx).len(),
        1,
        "presence-deferred sibling still receives via the tier-2 fallback"
    );
}

#[tokio::test]
async fn route_to_connection_bare_jid_all_negative_priority_goes_offline() {
    // A user whose only resources advertise negative priority is
    // treated as offline for bare-JID delivery (§8.5.2.1.1 →
    // "SHOULD store offline"): the headless pass persists instead of
    // delivering.
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    use waddle_xmpp::registry::UserRegistryActor;
    use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

    let registry = ConnectionRegistry::new();
    let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
    let bob_desk: jid::FullJid = "bob@example.com/desk".parse().expect("jid");
    let (desk_tx, mut desk_rx) = tokio::sync::mpsc::channel(8);
    register_into_both_tiers(&registry, &user_registry, &bob_desk, desk_tx).await;
    registry.update_presence(&bob_desk, true, -1);

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

    let msg = chat_msg("alice@example.com/web", "bob@example.com", "store me");
    let _ = interpret(
        vec![OutboundEvent::RouteToConnection {
            jid: "bob@example.com".parse::<jid::Jid>().expect("bare"),
            stanza: Box::new(Stanza::Message(msg)),
            call_setup: None,
        }],
        &deps,
    )
    .await;

    assert!(
        drain_inbound(&mut desk_rx).is_empty(),
        "negative-priority resource must not receive the message"
    );
    let bob_bare: jid::BareJid = "bob@example.com".parse().expect("bare");
    let bob_archive = mam
        .query_messages(
            &bob_bare,
            waddle_xmpp::mam::MamArchiveKind::Personal,
            &Default::default(),
        )
        .await
        .expect("query bob");
    assert_eq!(
        bob_archive.messages.len(),
        1,
        "message stored offline instead of delivered to the negative resource"
    );
}

// ---------------------------------------------------------------------
// XEP-0191 fail-closed: a blocklist load failure must never let the
// raw (unfiltered) stanza into a detached XEP-0198 replay buffer —
// replay writes stored XML verbatim with no recipient pass.
// ---------------------------------------------------------------------

#[tokio::test]
async fn route_full_jid_dm_to_detached_drops_when_blocklist_load_fails() {
    use async_trait::async_trait;
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    use waddle_xmpp::stream_management::SmSessionRegistry;
    use waddle_xmpp::xep::xep0191::{BlockingStorage, BlockingStorageError};

    #[derive(Debug, thiserror::Error)]
    #[error("simulated blocking storage failure")]
    struct SimulatedFailure;

    struct FailingBlocking;
    #[async_trait]
    impl BlockingStorage for FailingBlocking {
        async fn list_blocked_jids(
            &self,
            _: &jid::BareJid,
        ) -> Result<Vec<jid::BareJid>, BlockingStorageError> {
            Err(BlockingStorageError::new(SimulatedFailure))
        }
    }

    let registry = ConnectionRegistry::new();
    let bob_phone: jid::FullJid = "bob@example.com/phone".parse().expect("jid");
    let sm = Arc::new(InMemorySmSessionRegistry::new());
    sm.store_session(detached_dm_session("bob-blocked-stream", &bob_phone))
        .await
        .expect("store detached session");

    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
    let blocking: Arc<dyn BlockingStorage> = Arc::new(FailingBlocking);
    let dispatcher = pipelined_dispatcher();
    let deps = Deps {
        sm_session_registry: Some(&sm),
        ..offline_pass_deps(&registry, &mam, &inbox, &blocking, &dispatcher)
    };

    let msg = chat_msg(
        "alice@example.com/web",
        "bob@example.com/phone",
        "maybe blocked",
    );
    let _ = interpret(
        vec![OutboundEvent::RouteToConnection {
            jid: "bob@example.com/phone".parse::<jid::Jid>().expect("full"),
            stanza: Box::new(Stanza::Message(msg)),
            call_setup: None,
        }],
        &deps,
    )
    .await;

    let session = sm
        .peek_session("bob-blocked-stream")
        .await
        .expect("peek ok")
        .expect("session present");
    assert!(
        session.unacked_stanzas.is_empty(),
        "blocklist load failure must fail closed: no raw stanza may be \
         queued for XEP-0198 replay"
    );
}

#[tokio::test]
async fn route_bare_jid_dm_to_detached_only_recipient_runs_recipient_pipeline() {
    // Qodo review on PR #1272: a bare-JID DM whose recipient has ONLY
    // detached XEP-0198 resources must run the shared recipient pass
    // (recipient MAM row + stamped replay copy), not queue the raw
    // pre-pass stanza.
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    use waddle_xmpp::stream_management::SmSessionRegistry;
    use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

    let registry = ConnectionRegistry::new();
    let bob_phone: jid::FullJid = "bob@example.com/phone".parse().expect("jid");
    let sm = Arc::new(InMemorySmSessionRegistry::new());
    sm.store_session(detached_dm_session("bob-bare-detached", &bob_phone))
        .await
        .expect("store detached session");

    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
    let blocking: Arc<dyn BlockingStorage> = Arc::new(InMemoryBlockingStorage::new());
    let dispatcher = pipelined_dispatcher();
    let deps = Deps {
        sm_session_registry: Some(&sm),
        ..offline_pass_deps(&registry, &mam, &inbox, &blocking, &dispatcher)
    };

    let msg = chat_msg("alice@example.com/web", "bob@example.com", "bare detached");
    let _ = interpret(
        vec![OutboundEvent::RouteToConnection {
            jid: "bob@example.com".parse::<jid::Jid>().expect("bare"),
            stanza: Box::new(Stanza::Message(msg)),
            call_setup: None,
        }],
        &deps,
    )
    .await;

    let bob_bare: jid::BareJid = "bob@example.com".parse().expect("bare");
    let bob_archive = mam
        .query_messages(
            &bob_bare,
            waddle_xmpp::mam::MamArchiveKind::Personal,
            &Default::default(),
        )
        .await
        .expect("query bob");
    assert_eq!(
        bob_archive.messages.len(),
        1,
        "detached-only bare-JID DM must land in the recipient's archive"
    );

    let session = sm
        .peek_session("bob-bare-detached")
        .await
        .expect("peek ok")
        .expect("session present");
    assert_eq!(session.unacked_stanzas.len(), 1);
    let queued_element: Element = session.unacked_stanzas[0]
        .stanza_xml
        .parse()
        .expect("queued stanza XML parses");
    let queued =
        xmpp_parsers::message::Message::try_from(queued_element).expect("queued message parses");
    let by: jid::Jid = "bob@example.com".parse().expect("jid");
    assert!(
        waddle_xmpp_core::xep0359::extract_stanza_id_by(&queued, &by).is_some(),
        "detached-only replay copy must be the PROCESSED (stamped) stanza"
    );
}

#[tokio::test]
async fn route_bare_jid_dm_from_blocked_sender_to_detached_only_recipient_is_filtered() {
    // The recipient (only detached) has blocked the sender: the shared
    // pass must halt the message (nothing queued for replay) and bounce
    // <service-unavailable/> to the sender per XEP-0191.
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    use waddle_xmpp::stream_management::SmSessionRegistry;
    use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

    let registry = ConnectionRegistry::new();
    let bob_phone: jid::FullJid = "bob@example.com/phone".parse().expect("jid");
    let sm = Arc::new(InMemorySmSessionRegistry::new());
    sm.store_session(detached_dm_session("bob-blocked-bare", &bob_phone))
        .await
        .expect("store detached session");

    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
    let blocking_concrete = Arc::new(InMemoryBlockingStorage::new());
    blocking_concrete.set_blocklist(
        "bob@example.com".parse::<jid::BareJid>().expect("bare"),
        vec!["alice@example.com".parse::<jid::BareJid>().expect("bare")],
    );
    let blocking: Arc<dyn BlockingStorage> = blocking_concrete;
    let dispatcher = pipelined_dispatcher();
    let deps = Deps {
        sm_session_registry: Some(&sm),
        ..offline_pass_deps(&registry, &mam, &inbox, &blocking, &dispatcher)
    };

    let msg = chat_msg(
        "alice@example.com/web",
        "bob@example.com",
        "should not pass",
    );
    let _ = interpret(
        vec![OutboundEvent::RouteToConnection {
            jid: "bob@example.com".parse::<jid::Jid>().expect("bare"),
            stanza: Box::new(Stanza::Message(msg)),
            call_setup: None,
        }],
        &deps,
    )
    .await;

    let session = sm
        .peek_session("bob-blocked-bare")
        .await
        .expect("peek ok")
        .expect("session present");
    assert!(
        session.unacked_stanzas.is_empty(),
        "blocked sender's message must not reach the detached replay buffer"
    );
    let bob_bare: jid::BareJid = "bob@example.com".parse().expect("bare");
    let bob_archive = mam
        .query_messages(
            &bob_bare,
            waddle_xmpp::mam::MamArchiveKind::Personal,
            &Default::default(),
        )
        .await
        .expect("query bob");
    assert!(
        bob_archive.messages.is_empty(),
        "blocked sender's message must not be archived for the recipient"
    );
}
