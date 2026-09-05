use super::*;

#[tokio::test]
async fn try_send_to_returns_dropped_full_on_backpressured_channel() {
    // A size-1 channel lets us prove try_send does not block when the
    // receiver isn't draining: the second send must report DroppedFull
    // immediately instead of awaiting capacity. Callers rely on this
    // variant to count silent drops for observability.
    let registry = ConnectionRegistry::new();
    let jid: FullJid = "user@example.com/res".parse().expect("jid");
    let (tx, _rx) = mpsc::channel::<OutboundStanza>(1);
    registry.register(jid.clone(), tx);

    let stanza_a = Stanza::Presence(xmpp_parsers::presence::Presence::new(
        xmpp_parsers::presence::Type::None,
    ));
    let stanza_b = Stanza::Presence(xmpp_parsers::presence::Presence::new(
        xmpp_parsers::presence::Type::None,
    ));

    assert_eq!(
        registry.try_send_to(&jid, stanza_a),
        BroadcastOutcome::Delivered
    );
    assert_eq!(
        registry.try_send_to(&jid, stanza_b),
        BroadcastOutcome::DroppedFull
    );
}

#[tokio::test]
async fn try_send_to_returns_dropped_closed_and_unregisters() {
    let registry = ConnectionRegistry::new();
    let jid: FullJid = "gone@example.com/res".parse().expect("jid");
    let (tx, rx) = mpsc::channel::<OutboundStanza>(4);
    registry.register(jid.clone(), tx);
    drop(rx); // close the channel so try_send sees Closed

    let stanza = Stanza::Presence(xmpp_parsers::presence::Presence::new(
        xmpp_parsers::presence::Type::None,
    ));
    assert_eq!(
        registry.try_send_to(&jid, stanza),
        BroadcastOutcome::DroppedClosed
    );
    assert!(!registry.is_connected(&jid));
}

#[tokio::test]
async fn try_send_to_returns_not_connected_when_unregistered() {
    let registry = ConnectionRegistry::new();
    let jid: FullJid = "nobody@example.com/res".parse().expect("jid");
    let stanza = Stanza::Presence(xmpp_parsers::presence::Presence::new(
        xmpp_parsers::presence::Type::None,
    ));
    assert_eq!(
        registry.try_send_to(&jid, stanza),
        BroadcastOutcome::NotConnected
    );
}

#[tokio::test]
async fn try_send_to_does_not_unregister_replacement_entry() {
    // Simulate the replacement race: connection A is registered, its
    // receiver is dropped so the sender is closed, connection B takes
    // over the same JID with a live sender, then something (e.g. a MUC
    // broadcast task that still holds a clone of A's sender) tries to
    // send. The try_send would see Closed on A's cloned sender — but
    // the entry in the registry is now B's live one and must NOT be
    // evicted.
    let registry = ConnectionRegistry::new();
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");

    let (tx_a, rx_a) = mpsc::channel::<OutboundStanza>(4);
    registry.register(jid.clone(), tx_a);
    drop(rx_a); // A's sender is now closed

    // B takes over. The register() call replaces A's entry; only B's
    // (live) sender is now in the registry.
    let (tx_b, _rx_b) = mpsc::channel::<OutboundStanza>(4);
    registry.register(jid.clone(), tx_b);

    // A broadcast path now tries to send. From its perspective, it sees
    // whatever sender is currently in the registry — which is B's live
    // one — so try_send_to returns Delivered. Either way, the entry
    // must remain in the registry.
    let stanza = Stanza::Presence(xmpp_parsers::presence::Presence::new(
        xmpp_parsers::presence::Type::None,
    ));
    let _outcome = registry.try_send_to(&jid, stanza);
    assert!(
        registry.is_connected(&jid),
        "replacement entry must still be registered after a try_send_to that races with eviction"
    );
}

/// Round-2 concurrency review on #1105, sharpened by the round-3
/// review: the terminated-session unavailable is suppressed ONLY when
/// the replacement connection is already PRESENCE-AVAILABLE (its
/// available superseded ours; a late unavailable would pin subscribers
/// on a stale offline). A replacement that is merely registered has
/// broadcast nothing yet — if it never sends presence, subscribers
/// would keep the dropped session's stale available forever — so the
/// unavailable must still go out; the replacement's future available
/// lands after it and wins in every interleaving.
#[tokio::test]
async fn terminated_session_unavailable_is_suppressed_when_a_replacement_is_live() {
    let state = create_test_websocket_state().await;
    let dropped: FullJid = "alice@example.com/web".parse().expect("jid");
    let sibling: FullJid = "alice@example.com/other".parse().expect("jid");

    // A live available sibling resource observes the (bare-JID) leg of
    // the terminated-session broadcast.
    let (sib_tx, mut sib_rx) = mpsc::channel::<OutboundStanza>(8);
    state
        .deps
        .protocol
        .connection_registry
        .register(sibling.clone(), sib_tx);
    state
        .deps
        .protocol
        .connection_registry
        .update_presence(&sibling, true, 0);

    // No replacement: the broadcast goes out and the sibling sees the
    // unavailable from the dropped resource.
    super::super::cleanup::broadcast_unavailable_if_no_replacement(state.as_ref(), &dropped, true)
        .await;
    let received = tokio::time::timeout(std::time::Duration::from_millis(500), sib_rx.recv())
        .await
        .expect("sibling must receive the unavailable broadcast")
        .expect("channel open");
    let frame = stanza_to_xml(&received.stanza);
    assert!(
        frame.contains("unavailable") && frame.contains("alice@example.com/web"),
        "expected unavailable from the dropped resource, got {frame}"
    );

    // Replacement registered for the SAME full JID but NOT yet
    // available: the unavailable must still be broadcast.
    let (repl_tx, _repl_rx) = mpsc::channel::<OutboundStanza>(8);
    state
        .deps
        .protocol
        .connection_registry
        .register(dropped.clone(), repl_tx);
    super::super::cleanup::broadcast_unavailable_if_no_replacement(state.as_ref(), &dropped, true)
        .await;
    let received = tokio::time::timeout(std::time::Duration::from_millis(500), sib_rx.recv())
        .await
        .expect("a registered-but-silent replacement must not suppress the unavailable")
        .expect("channel open");
    let frame = stanza_to_xml(&received.stanza);
    assert!(
        frame.contains("unavailable") && frame.contains("alice@example.com/web"),
        "expected unavailable despite the silent replacement, got {frame}"
    );

    // Replacement goes presence-available: NOW the broadcast must be
    // suppressed entirely.
    state
        .deps
        .protocol
        .connection_registry
        .update_presence(&dropped, true, 0);
    super::super::cleanup::broadcast_unavailable_if_no_replacement(state.as_ref(), &dropped, true)
        .await;
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(300), sib_rx.recv())
            .await
            .is_err(),
        "no unavailable may be broadcast while an available replacement is live"
    );

    // And a session that never sent available broadcasts nothing.
    state.deps.protocol.connection_registry.unregister(&dropped);
    super::super::cleanup::broadcast_unavailable_if_no_replacement(state.as_ref(), &dropped, false)
        .await;
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(300), sib_rx.recv())
            .await
            .is_err(),
        "a never-available session must not broadcast unavailable"
    );
}

fn make_detached_session(
    jid: &FullJid,
    presence_available: bool,
) -> waddle_xmpp::stream_management::DetachedSession {
    waddle_xmpp::stream_management::DetachedSession {
        stream_id: "stale-stream".to_string(),
        user_id: jid.to_bare().to_string(),
        jid: jid.clone(),
        occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
        inbound_count: 0,
        shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
        outbound_count: 0,
        last_acked: 0,
        replay_gap_through: None,
        unacked_stanzas: vec![],
        max_resume_time: Some(300),
        detached_at: std::time::Instant::now(),
        carbons_enabled: false,
        roster_interested: false,
        blocklist_interested: false,
        presence_available,
        presence_show: None,
        presence_status: None,
        presence_priority: 0,
        presence_payloads: Vec::new(),
        pending_subscribes_flushed: false,
    }
}

/// Detached-invalidation must apply the same presence-aware suppression
/// rule as the unclean-disconnect path: a same-JID replacement that is
/// merely REGISTERED (owner of the slot) but has never gone
/// presence-available has broadcast nothing yet — if the detached
/// session was available, subscribers still hold its stale available,
/// so the unavailable MUST go out. Suppression is correct only once the
/// replacement is itself presence-available.
#[tokio::test]
async fn detached_invalidation_broadcasts_unavailable_past_a_silent_replacement_owner() {
    let state = create_test_websocket_state().await;
    let dropped: FullJid = "alice@example.com/web".parse().expect("jid");
    let sibling: FullJid = "alice@example.com/other".parse().expect("jid");

    // A live available sibling resource observes the broadcast.
    let (sib_tx, mut sib_rx) = mpsc::channel::<OutboundStanza>(8);
    state
        .deps
        .protocol
        .connection_registry
        .register(sibling.clone(), sib_tx);
    state
        .deps
        .protocol
        .connection_registry
        .update_presence(&sibling, true, 0);

    // The replacement is registered (and owns the slot) but has NOT
    // sent available.
    let (repl_tx, _repl_rx) = mpsc::channel::<OutboundStanza>(8);
    let owner = state
        .deps
        .protocol
        .connection_registry
        .register(dropped.clone(), repl_tx);

    // Invalidate the stale detached (available) session: the sibling
    // must still receive the unavailable — the silent replacement has
    // superseded nothing presence-wise.
    let detached = make_detached_session(&dropped, true);
    super::super::cleanup::cleanup_invalidated_detached_session(
        state.as_ref(),
        detached.clone(),
        Some(&owner),
    )
    .await;
    let received = tokio::time::timeout(std::time::Duration::from_millis(500), sib_rx.recv())
        .await
        .expect("a registered-but-silent replacement must not suppress the detached unavailable")
        .expect("channel open");
    let frame = stanza_to_xml(&received.stanza);
    assert!(
        frame.contains("unavailable") && frame.contains("alice@example.com/web"),
        "expected unavailable despite the silent replacement owner, got {frame}"
    );

    // Companion: once the replacement is presence-available, the
    // broadcast is suppressed.
    state
        .deps
        .protocol
        .connection_registry
        .update_presence(&dropped, true, 0);
    super::super::cleanup::cleanup_invalidated_detached_session(
        state.as_ref(),
        detached,
        Some(&owner),
    )
    .await;
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(300), sib_rx.recv())
            .await
            .is_err(),
        "no unavailable may be broadcast while an available replacement is live"
    );
}
