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
