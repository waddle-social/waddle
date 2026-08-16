use super::*;
use jid::Jid;
use std::time::Duration;
use xmpp_parsers::message::{Message, MessageType};

fn test_jid(user: &str) -> FullJid {
    format!("{}@example.com/resource", user).parse().unwrap()
}

fn make_test_message(to: &str) -> Message {
    let bare_jid: jid::BareJid = to.parse().unwrap();
    let mut msg = Message::new(Some(Jid::from(bare_jid)));
    msg.type_ = MessageType::Chat;
    msg
}

#[test]
fn test_registry_creation() {
    let registry = ConnectionRegistry::new();
    assert_eq!(registry.connection_count(), 0);
}

#[test]
fn outbound_stanza_new_defaults_to_direct_frame() {
    // The default constructor preserves PR1-PR9 behavior — every
    // existing caller treats the destination's main loop as a
    // dumb wire-writer. The recipient-pass plumbing
    // (DeliveryKind::PeerStanza) lands later in the #229 staged
    // cutover.
    let msg = make_test_message("alice@example.com");
    let outbound = OutboundStanza::new(Stanza::Message(msg));
    assert_eq!(outbound.kind, DeliveryKind::DirectFrame);
}

#[test]
fn outbound_stanza_peer_stanza_marks_kind_for_recipient_pass() {
    // The opt-in constructor used by `RouteToConnection` once
    // PR12 lands. The destination's main loop will dispatch on
    // `kind` and feed PeerStanza values through the recipient
    // pass before any wire write.
    let msg = make_test_message("bob@example.com");
    let outbound = OutboundStanza::peer_stanza(Stanza::Message(msg));
    assert_eq!(outbound.kind, DeliveryKind::PeerStanza);
}

#[test]
fn test_register_connection() {
    let registry = ConnectionRegistry::new();
    let jid = test_jid("user1");
    let (tx, _rx) = mpsc::channel(16);

    registry.register(jid.clone(), tx);

    assert!(registry.is_connected(&jid));
    assert_eq!(registry.connection_count(), 1);
}

#[tokio::test]
async fn register_and_unregister_publish_connection_gauge() {
    let guard = crate::telemetry::test_support::acquire().await;
    let registry = ConnectionRegistry::new();
    let jid = test_jid("user1");
    let (tx, _rx) = mpsc::channel(16);

    registry.register(jid.clone(), tx);
    registry.unregister(&jid);

    assert!(
        guard
            .metric_names()
            .contains(&"xmpp.connections.active".to_string()),
        "register/unregister must publish the active-connections gauge",
    );
}

#[test]
fn test_register_replaces_existing() {
    let registry = ConnectionRegistry::new();
    let jid = test_jid("user1");

    let (tx1, _rx1) = mpsc::channel(16);
    let (tx2, _rx2) = mpsc::channel(16);

    registry.register(jid.clone(), tx1);
    registry.register(jid.clone(), tx2);

    // Should still only have one connection
    assert_eq!(registry.connection_count(), 1);
}

#[tokio::test]
async fn send_to_if_owner_delivers_only_to_the_owning_session() {
    // Issue #1220 (Qodo review): `send_to_if_owner` must deliver only while
    // the entry still belongs to the given owner token, and must NOT reroute
    // to a replacement — unlike `send_to`.
    let registry = ConnectionRegistry::new();
    let jid = test_jid("user1");
    let (tx, mut rx) = mpsc::channel(16);
    let owner = registry.register(jid.clone(), tx);
    let stale_owner = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    // A non-matching owner token delivers nothing.
    let outcome = registry
        .send_to_if_owner(
            &jid,
            &stale_owner,
            Stanza::Message(make_test_message("user1@example.com")),
        )
        .await;
    assert!(matches!(outcome, SendResult::NotConnected));
    assert!(rx.try_recv().is_err());

    // The live owner token delivers.
    let outcome = registry
        .send_to_if_owner(
            &jid,
            &owner,
            Stanza::Message(make_test_message("user1@example.com")),
        )
        .await;
    assert!(outcome.is_sent());
    assert!(rx.try_recv().is_ok());
}

#[tokio::test]
async fn send_to_with_write_acceptance_attaches_token_without_acknowledging_enqueue() {
    let registry = ConnectionRegistry::new();
    let jid = test_jid("write-accepted");
    let (tx, mut rx) = mpsc::channel(1);
    registry.register(jid.clone(), tx);
    let (acceptance, mut accepted) = OutboundWriteAcceptance::new();

    assert!(registry
        .send_to_with_write_acceptance(
            &jid,
            Stanza::Message(make_test_message("write-accepted@example.com")),
            acceptance,
        )
        .await
        .is_sent());
    assert!(matches!(
        accepted.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));

    let outbound = rx
        .recv()
        .await
        .expect("destination dequeues outbound frame");
    assert_eq!(outbound.kind, DeliveryKind::DirectFrame);
    assert!(matches!(
        accepted.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));

    assert!(outbound.write_acceptance.is_some());
    drop(outbound);
    assert!(matches!(
        accepted.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Closed)
    ));
}

#[tokio::test]
async fn send_to_with_write_acceptance_not_connected_never_acknowledges() {
    let registry = ConnectionRegistry::new();
    let jid = test_jid("absent");
    let (acceptance, mut accepted) = OutboundWriteAcceptance::new();

    assert!(matches!(
        registry
            .send_to_with_write_acceptance(
                &jid,
                Stanza::Message(make_test_message("absent@example.com")),
                acceptance,
            )
            .await,
        SendResult::NotConnected
    ));
    assert!(matches!(
        accepted.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Closed)
    ));
}

#[tokio::test]
async fn test_register_entry_preserves_prebuilt_entry_state_and_sender() {
    let registry = ConnectionRegistry::new();
    let jid: FullJid = "user@example.com/web".parse().unwrap();
    let bare = jid.to_bare();
    let (tx, mut rx) = mpsc::channel(16);
    let entry = ConnectionEntry::new(tx);
    entry.carbons_enabled.store(true, Ordering::Relaxed);
    entry.roster_interested.store(true, Ordering::Relaxed);
    entry.blocklist_interested.store(true, Ordering::Relaxed);
    entry.presence_available.store(true, Ordering::Relaxed);
    entry.presence_priority.store(7, Ordering::Relaxed);

    let owner = registry.register_entry(jid.clone(), entry.clone());

    assert!(Arc::ptr_eq(&owner, &entry.carbons_enabled));
    assert!(registry.is_carbons_enabled(&jid));
    assert_eq!(
        registry.get_roster_interested_resources_for_user(&bare),
        vec![jid.clone()]
    );
    assert_eq!(
        registry.get_blocklist_interested_resources_for_user(&bare),
        vec![jid.clone()]
    );
    assert_eq!(
        registry.get_available_resources_for_user(&bare),
        vec![(jid.clone(), 7)]
    );

    let message = Stanza::Message(make_test_message("user@example.com"));
    assert!(registry.send_to(&jid, message.clone()).await.is_sent());
    let outbound = rx
        .try_recv()
        .expect("prebuilt sender receives registry fanout");
    assert_eq!(outbound.stanza.name(), message.name());
}

#[test]
fn test_register_entry_if_owner_or_absent_refuses_replacement_owner() {
    let registry = ConnectionRegistry::new();
    let jid: FullJid = "user@example.com/web".parse().unwrap();
    let (tx1, _rx1) = mpsc::channel(16);
    let entry1 = ConnectionEntry::new(tx1);
    let owner1 = entry1.carbons_handle();

    assert!(registry.register_entry_if_owner_or_absent(jid.clone(), entry1.clone(), &owner1));
    assert!(registry.entry_if_owner(&jid, &owner1).is_some());

    let (tx2, _rx2) = mpsc::channel(16);
    let entry2 = ConnectionEntry::new(tx2);
    let owner2 = entry2.carbons_handle();

    assert!(!registry.register_entry_if_owner_or_absent(jid.clone(), entry2.clone(), &owner2));
    assert!(registry.entry_if_owner(&jid, &owner1).is_some());
    assert!(registry.entry_if_owner(&jid, &owner2).is_none());
    assert_eq!(registry.connection_count(), 1);

    assert!(registry.register_entry_if_owner_or_absent(jid.clone(), entry1.clone(), &owner1));
    assert!(registry.entry_if_owner(&jid, &owner1).is_some());
    assert_eq!(registry.connection_count(), 1);
}

#[test]
fn test_register_replacement_does_not_inherit_roster_interest() {
    let registry = ConnectionRegistry::new();
    let jid: FullJid = "user@example.com/web".parse().unwrap();
    let bare = jid.to_bare();

    let (tx1, _rx1) = mpsc::channel(16);
    let (tx2, _rx2) = mpsc::channel(16);

    registry.register(jid.clone(), tx1);
    registry.mark_roster_interested(&jid);
    assert!(registry.is_roster_interested(&jid));

    registry.register(jid.clone(), tx2);
    assert!(!registry.is_roster_interested(&jid));
    assert!(registry
        .get_roster_interested_resources_for_user(&bare)
        .is_empty());
}

#[test]
fn test_blocklist_interest_tracks_only_requesting_resources() {
    let registry = ConnectionRegistry::new();
    let web: FullJid = "user@example.com/web".parse().unwrap();
    let phone: FullJid = "user@example.com/phone".parse().unwrap();
    let bare = web.to_bare();

    let (web_tx, _web_rx) = mpsc::channel(16);
    let (phone_tx, _phone_rx) = mpsc::channel(16);

    registry.register(web.clone(), web_tx);
    registry.register(phone.clone(), phone_tx);
    registry.mark_blocklist_interested(&web);

    assert_eq!(
        registry.get_blocklist_interested_resources_for_user(&bare),
        vec![web]
    );
}

#[test]
fn test_register_with_stream_state_seeds_blocklist_interest() {
    let registry = ConnectionRegistry::new();
    let jid: FullJid = "user@example.com/web".parse().unwrap();
    let bare = jid.to_bare();
    let (tx, _rx) = mpsc::channel(16);

    registry.register_with_stream_state(jid.clone(), tx, false, false, true);

    assert!(registry.is_blocklist_interested(&jid));
    assert_eq!(
        registry.get_blocklist_interested_resources_for_user(&bare),
        vec![jid]
    );
}

#[test]
fn test_unregister_connection() {
    let registry = ConnectionRegistry::new();
    let jid = test_jid("user1");
    let (tx, _rx) = mpsc::channel(16);

    registry.register(jid.clone(), tx);
    assert!(registry.is_connected(&jid));

    let removed = registry.unregister(&jid);
    assert!(removed.is_some());
    assert!(!registry.is_connected(&jid));
    assert_eq!(registry.connection_count(), 0);
}

#[test]
fn test_unregister_nonexistent() {
    let registry = ConnectionRegistry::new();
    let jid = test_jid("user1");

    let removed = registry.unregister(&jid);
    assert!(removed.is_none());
}

#[tokio::test]
async fn test_send_to_connected_user() {
    let registry = ConnectionRegistry::new();
    let jid = test_jid("user1");
    let (tx, mut rx) = mpsc::channel(16);

    registry.register(jid.clone(), tx);

    let msg = make_test_message("user1@example.com");
    let stanza = Stanza::Message(msg);

    let result = registry.send_to(&jid, stanza).await;
    assert!(matches!(result, SendResult::Sent));

    // Verify the message was received
    let received = rx.recv().await;
    assert!(received.is_some());
}

#[tokio::test]
async fn test_send_to_disconnected_user() {
    let registry = ConnectionRegistry::new();
    let jid = test_jid("user1");

    let msg = make_test_message("user1@example.com");
    let stanza = Stanza::Message(msg);

    let result = registry.send_to(&jid, stanza).await;
    assert!(matches!(result, SendResult::NotConnected));
}

#[tokio::test]
async fn test_send_to_closed_channel() {
    let registry = ConnectionRegistry::new();
    let jid = test_jid("user1");
    let (tx, rx) = mpsc::channel(16);

    registry.register(jid.clone(), tx);

    // Drop the receiver to close the channel
    drop(rx);

    let msg = make_test_message("user1@example.com");
    let stanza = Stanza::Message(msg);

    let result = registry.send_to(&jid, stanza).await;
    assert!(matches!(result, SendResult::ChannelClosed));

    // Connection should have been removed
    assert!(!registry.is_connected(&jid));
}

#[tokio::test]
async fn test_send_to_closed_channel_does_not_remove_replacement() {
    let registry = ConnectionRegistry::new();
    let jid = test_jid("user1");
    let (old_tx, old_rx) = mpsc::channel(1);
    let (new_tx, mut new_rx) = mpsc::channel(16);

    registry.register(jid.clone(), old_tx);
    assert!(matches!(
        registry
            .send_to(
                &jid,
                Stanza::Message(make_test_message("user1@example.com"))
            )
            .await,
        SendResult::Sent
    ));

    let send = registry.send_to(
        &jid,
        Stanza::Message(make_test_message("user1@example.com")),
    );
    tokio::pin!(send);
    assert!(tokio::time::timeout(Duration::from_millis(50), &mut send)
        .await
        .is_err());

    registry.register(jid.clone(), new_tx);
    drop(old_rx);

    assert!(matches!(send.await, SendResult::Sent));
    assert!(new_rx.recv().await.is_some());
    assert!(registry.is_connected(&jid));
    assert!(matches!(
        registry
            .send_to(
                &jid,
                Stanza::Message(make_test_message("user1@example.com"))
            )
            .await,
        SendResult::Sent
    ));
    assert!(new_rx.recv().await.is_some());
}

#[tokio::test]
async fn test_send_to_waits_for_capacity_instead_of_dropping() {
    let registry = ConnectionRegistry::new();
    let jid = test_jid("user1");
    let (tx, mut rx) = mpsc::channel(1);

    registry.register(jid.clone(), tx);

    // Fill the channel so the next send must wait for capacity.
    let msg1 = make_test_message("user1@example.com");
    assert!(matches!(
        registry.send_to(&jid, Stanza::Message(msg1)).await,
        SendResult::Sent
    ));

    let msg2 = make_test_message("user1@example.com");
    let send = registry.send_to(&jid, Stanza::Message(msg2));
    tokio::pin!(send);

    assert!(tokio::time::timeout(Duration::from_millis(50), &mut send)
        .await
        .is_err());

    let first = rx.recv().await;
    assert!(first.is_some(), "first stanza should remain queued");

    let result = tokio::time::timeout(Duration::from_secs(1), &mut send)
        .await
        .expect("second send should complete once capacity is available");
    assert!(matches!(result, SendResult::Sent));

    let second = rx.recv().await;
    assert!(
        second.is_some(),
        "second stanza should be delivered after backpressure"
    );
}

// ADR-0017 Slice 3: `try_send_peer_to`/`send_peer_to` were deleted (peer-routed
// delivery now goes through the authoritative `UserActor`'s `TrySendPeer`), so
// their dedicated non-blocking / PeerStanza-kind tests moved with the behaviour
// to the `user_actor::delivery` invariant suite and the interpret routing
// tests.

#[test]
fn test_list_connections() {
    let registry = ConnectionRegistry::new();

    let jid1 = test_jid("user1");
    let jid2 = test_jid("user2");

    let (tx1, _rx1) = mpsc::channel(16);
    let (tx2, _rx2) = mpsc::channel(16);

    registry.register(jid1.clone(), tx1);
    registry.register(jid2.clone(), tx2);

    let connections = registry.list_connections();
    assert_eq!(connections.len(), 2);
    assert!(connections.contains(&jid1));
    assert!(connections.contains(&jid2));
}

#[test]
fn test_cleanup_stale() {
    let registry = ConnectionRegistry::new();
    let jid = test_jid("user1");
    let (tx, rx) = mpsc::channel(16);

    registry.register(jid.clone(), tx);
    assert!(registry.is_connected(&jid));

    // Drop the receiver to make the channel stale
    drop(rx);

    let removed = registry.cleanup_stale();
    assert_eq!(removed, 1);
    assert!(!registry.is_connected(&jid));
}

#[tokio::test]
async fn test_send_to_many() {
    let registry = ConnectionRegistry::new();

    let jid1 = test_jid("user1");
    let jid2 = test_jid("user2");
    let jid3 = test_jid("user3"); // Not registered

    let (tx1, mut rx1) = mpsc::channel(16);
    let (tx2, mut rx2) = mpsc::channel(16);

    registry.register(jid1.clone(), tx1);
    registry.register(jid2.clone(), tx2);

    let msg = make_test_message("room@muc.example.com");
    let stanza = Stanza::Message(msg);

    let recipients = vec![&jid1, &jid2, &jid3];
    let results = registry.send_to_many(recipients, stanza).await;

    assert_eq!(results.len(), 3);

    // Check results
    let result_map: std::collections::HashMap<_, _> = results.into_iter().collect();
    assert!(matches!(result_map.get(&jid1), Some(SendResult::Sent)));
    assert!(matches!(result_map.get(&jid2), Some(SendResult::Sent)));
    assert!(matches!(
        result_map.get(&jid3),
        Some(SendResult::NotConnected)
    ));

    // Verify messages were received
    assert!(rx1.recv().await.is_some());
    assert!(rx2.recv().await.is_some());
}

#[test]
fn test_update_presence_and_get_available_resources() {
    let registry = ConnectionRegistry::new();

    let jid1: FullJid = "user@example.com/one".parse().unwrap();
    let jid2: FullJid = "user@example.com/two".parse().unwrap();
    let bare: BareJid = "user@example.com".parse().unwrap();

    let (tx1, _rx1) = mpsc::channel(16);
    let (tx2, _rx2) = mpsc::channel(16);
    registry.register(jid1.clone(), tx1);
    registry.register(jid2.clone(), tx2);

    // Default is unavailable until initial presence is sent.
    assert!(registry.get_available_resources_for_user(&bare).is_empty());

    assert!(registry.update_presence(&jid1, true, 5));
    assert!(registry.update_presence(&jid2, true, -1));

    let mut resources = registry.get_available_resources_for_user(&bare);
    resources.sort_by_key(|a| a.0.to_string());
    assert_eq!(resources.len(), 2);
    assert_eq!(resources[0].0, jid1);
    assert_eq!(resources[0].1, 5);
    assert_eq!(resources[1].0, jid2);
    assert_eq!(resources[1].1, -1);
}

#[test]
fn test_update_presence_missing_jid_returns_false() {
    let registry = ConnectionRegistry::new();
    let missing: FullJid = "missing@example.com/resource".parse().unwrap();
    assert!(!registry.update_presence(&missing, true, 1));
}

#[test]
fn test_queue_and_drain_pending_subscription_stanzas() {
    let registry = ConnectionRegistry::new();
    let bare: BareJid = "user@example.com".parse().unwrap();

    let mut subscribe =
        xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::Subscribe);
    subscribe.to = Some(jid::Jid::from(bare.clone()));

    let mut unsubscribed =
        xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::Unsubscribed);
    unsubscribed.to = Some(jid::Jid::from(bare.clone()));

    registry.queue_pending_subscription_stanza(&bare, Stanza::Presence(subscribe));
    registry.queue_pending_subscription_stanza(&bare, Stanza::Presence(unsubscribed));

    let drained = registry.drain_pending_subscription_stanzas(&bare);
    assert_eq!(drained.len(), 2);
    assert!(
        matches!(&drained[0], Stanza::Presence(p) if p.type_ == xmpp_parsers::presence::Type::Subscribe)
    );
    assert!(
        matches!(&drained[1], Stanza::Presence(p) if p.type_ == xmpp_parsers::presence::Type::Unsubscribed)
    );

    // Draining again should be empty.
    assert!(registry
        .drain_pending_subscription_stanzas(&bare)
        .is_empty());
}

#[test]
fn test_pending_subscribe_is_deduplicated_and_not_drained_by_read() {
    let registry = ConnectionRegistry::new();
    let recipient: BareJid = "alice@example.com".parse().unwrap();
    let requester: BareJid = "bob@example.com".parse().unwrap();

    for _ in 0..2 {
        let mut subscribe =
            xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::Subscribe);
        subscribe.from = Some(jid::Jid::from(requester.clone()));
        subscribe.to = Some(jid::Jid::from(recipient.clone()));
        registry.queue_pending_subscription_stanza(&recipient, Stanza::Presence(subscribe));
    }

    assert_eq!(registry.pending_subscription_stanzas(&recipient).len(), 1);
    assert_eq!(registry.pending_subscription_stanzas(&recipient).len(), 1);
    assert_eq!(registry.remove_pending_subscribe(&recipient, &requester), 1);
    assert!(registry.pending_subscription_stanzas(&recipient).is_empty());
}

#[test]
fn test_presence_state_tracking() {
    let registry = ConnectionRegistry::new();
    let jid: FullJid = "user@example.com/resource".parse().unwrap();

    let (tx, _rx) = mpsc::channel(16);
    registry.register(jid.clone(), tx);

    // No state initially
    assert!(registry.get_presence_state(&jid).is_none());

    // Store presence state, including the resource's extension payloads
    // (relayed verbatim on probe/subscription delivery — issue #1101).
    let idle_payload = minidom::Element::builder("idle", "urn:xmpp:idle:1")
        .attr(
            minidom::rxml::xml_ncname!("since").to_owned(),
            "2024-06-01T12:00:00Z",
        )
        .build();
    registry.update_presence_state(
        &jid,
        Some("away".to_string()),
        Some("Gone fishing".to_string()),
        5,
        vec![idle_payload.clone()],
    );

    let state = registry.get_presence_state(&jid).expect("should exist");
    assert_eq!(state.show.as_deref(), Some("away"));
    assert_eq!(state.status.as_deref(), Some("Gone fishing"));
    assert_eq!(state.priority, 5);
    assert_eq!(state.payloads, vec![idle_payload]);

    // Update with different values — and no payloads (return from idle).
    registry.update_presence_state(&jid, None, None, 0, Vec::new());
    let state = registry.get_presence_state(&jid).expect("should exist");
    assert!(state.show.is_none());
    assert!(state.status.is_none());
    assert!(state.payloads.is_empty());
    assert_eq!(state.priority, 0);

    // Clean up on unregister
    registry.unregister(&jid);
    assert!(registry.get_presence_state(&jid).is_none());
}

/// XEP-0198 + XEP-0280: when a stream resumes the client expects its
/// previous carbons opt-in to still be in effect. `register` creates a
/// fresh entry with carbons disabled, so the resume path needs a variant
/// that seeds the flag to the value captured when the session detached.
#[test]
fn test_register_with_carbons_seeds_initial_flag() {
    let registry = ConnectionRegistry::new();
    let jid = test_jid("user1");
    let (tx, _rx) = mpsc::channel(16);

    let handle = registry.register_with_carbons(jid.clone(), tx, true);

    assert!(
        handle.load(Ordering::Relaxed),
        "handle returned by register_with_carbons(.., true) should start enabled"
    );
    assert!(
        registry.is_carbons_enabled(&jid),
        "registry should report carbons as enabled for the seeded entry"
    );
}

#[test]
fn test_register_with_carbons_false_leaves_disabled() {
    let registry = ConnectionRegistry::new();
    let jid = test_jid("user2");
    let (tx, _rx) = mpsc::channel(16);

    let handle = registry.register_with_carbons(jid.clone(), tx, false);

    assert!(!handle.load(Ordering::Relaxed));
    assert!(!registry.is_carbons_enabled(&jid));
}

#[test]
fn test_set_carbons_enabled_updates_existing_entry() {
    let registry = ConnectionRegistry::new();
    let jid = test_jid("user3");
    let (tx, _rx) = mpsc::channel(16);
    registry.register(jid.clone(), tx);

    assert!(registry.set_carbons_enabled(&jid, true));
    assert!(registry.is_carbons_enabled(&jid));

    assert!(registry.set_carbons_enabled(&jid, false));
    assert!(!registry.is_carbons_enabled(&jid));
}

#[test]
fn test_set_carbons_enabled_returns_false_for_missing_entry() {
    let registry = ConnectionRegistry::new();
    let jid = test_jid("missing");
    assert!(!registry.set_carbons_enabled(&jid, true));
}

#[test]
fn test_try_send_to_dropped_full_keeps_connection_registered() {
    let registry = ConnectionRegistry::new();
    let jid = test_jid("full");
    let (tx, _rx) = mpsc::channel(1);
    registry.register(jid.clone(), tx);

    let first = registry.try_send_to(&jid, Stanza::Message(make_test_message("full@example.com")));
    let second = registry.try_send_to(&jid, Stanza::Message(make_test_message("full@example.com")));

    assert_eq!(first, BroadcastOutcome::Delivered);
    assert_eq!(second, BroadcastOutcome::DroppedFull);
    assert!(
        registry.is_connected(&jid),
        "full channel should not be evicted"
    );
}

#[test]
fn test_try_send_to_closed_evicts_connection() {
    let registry = ConnectionRegistry::new();
    let jid = test_jid("closed");
    let (tx, rx) = mpsc::channel(1);
    registry.register(jid.clone(), tx);
    drop(rx);

    let outcome = registry.try_send_to(
        &jid,
        Stanza::Message(make_test_message("closed@example.com")),
    );

    assert_eq!(outcome, BroadcastOutcome::DroppedClosed);
    assert!(
        !registry.is_connected(&jid),
        "closed channel should be cleaned up"
    );
}

#[test]
fn test_remove_if_sender_closed_keeps_new_live_registration() {
    let registry = ConnectionRegistry::new();
    let jid = test_jid("racy");

    let (tx_closed, rx_closed) = mpsc::channel(1);
    registry.register(jid.clone(), tx_closed);
    drop(rx_closed);

    let (tx_live, _rx_live) = mpsc::channel(1);
    registry.register(jid.clone(), tx_live);

    registry.remove_if_sender_closed(&jid);

    assert!(
        registry.is_connected(&jid),
        "race-safe stale cleanup must not remove a newly registered live sender"
    );
}

#[test]
fn test_try_send_to_load_reports_single_delivery_then_drops() {
    let registry = ConnectionRegistry::new();
    let jid = test_jid("load");
    let (tx, _rx) = mpsc::channel(1);
    registry.register(jid.clone(), tx);

    let mut delivered = 0;
    let mut dropped_full = 0;
    for _ in 0..32 {
        match registry.try_send_to(&jid, Stanza::Message(make_test_message("load@example.com"))) {
            BroadcastOutcome::Delivered => delivered += 1,
            BroadcastOutcome::DroppedFull => dropped_full += 1,
            other => panic!("unexpected outcome during load test: {other:?}"),
        }
    }

    assert_eq!(delivered, 1);
    assert_eq!(dropped_full, 31);
}

/// ADR-0017 Phase 1 (Greptile P1 on PR #1177): the SM-expiry janitor must not
/// evict a live REPLACEMENT session. `unregister_if_sm_stream_id` removes the
/// entry only when its published SM stream id matches the expired session's, so
/// a replacement S2 that rebound the same full JID after S1 detached survives
/// S1's expiry — on BOTH the DashMap here and (via the caller's gated mirror)
/// the actor tree.
#[test]
fn unregister_if_sm_stream_id_spares_a_replacement_session() {
    use crate::pending_delivery::SmSessionId;

    let registry = ConnectionRegistry::new();
    let jid = test_jid("alice");

    // S1 binds and publishes its SM stream id.
    let (s1_tx, _s1_rx) = mpsc::channel(4);
    registry.register(jid.clone(), s1_tx);
    let s1_stream = SmSessionId::new("s1-stream".to_string());
    registry
        .get_entry(&jid)
        .expect("s1 entry")
        .set_sm_stream_id(Some(s1_stream.clone()));

    // S2 rebinds the SAME full JID (replacing the DashMap entry) with its own
    // SM stream id — the detach→rebind race the janitor must tolerate.
    let (s2_tx, _s2_rx) = mpsc::channel(4);
    registry.register(jid.clone(), s2_tx);
    let s2_stream = SmSessionId::new("s2-stream".to_string());
    registry
        .get_entry(&jid)
        .expect("s2 entry")
        .set_sm_stream_id(Some(s2_stream.clone()));

    // S1's expiry must NOT remove S2.
    assert!(
        registry
            .unregister_if_sm_stream_id(&jid, &s1_stream)
            .is_none(),
        "S1's stream id no longer owns the entry; nothing must be removed"
    );
    assert!(
        registry.is_connected(&jid),
        "the replacement session S2 must survive S1's expiry"
    );

    // S2's own expiry removes it (matching stream id).
    assert!(
        registry
            .unregister_if_sm_stream_id(&jid, &s2_stream)
            .is_some(),
        "a matching stream id removes the entry"
    );
    assert!(!registry.is_connected(&jid));
}

/// Owner-gated presence publication (registration race): after A
/// registers but before it publishes its restored presence, a same-JID
/// replacement can take the slot. A's publication runs with a stale
/// owner token and must be refused in the same call that would perform
/// the write — a check-then-write across separate calls reintroduces
/// the window.
#[test]
fn update_presence_if_owner_refuses_stale_owner() {
    let registry = ConnectionRegistry::new();
    let jid = test_jid("alice");

    let (tx_a, _rx_a) = mpsc::channel(4);
    let stale_owner = registry.register(jid.clone(), tx_a);

    // Replacement supersedes A and publishes its own availability.
    let (tx_b, _rx_b) = mpsc::channel(4);
    let live_owner = registry.register(jid.clone(), tx_b);
    assert!(registry.update_presence_if_owner(&jid, &live_owner, true, 3));

    // A's delayed publication must be refused and must not overwrite
    // the replacement's availability/priority.
    assert!(
        !registry.update_presence_if_owner(&jid, &stale_owner, true, 9),
        "a stale owner must not update presence"
    );
    let entry = registry.get_entry(&jid).expect("replacement entry");
    assert_eq!(
        entry.presence_priority(),
        3,
        "the replacement's priority must survive the stale-owner write"
    );
}

/// Same race for the JID-keyed presence-state map: the ownership check
/// and the `presence_states` write must happen inside one owner-gated
/// method call.
#[test]
fn update_presence_state_if_owner_refuses_stale_owner() {
    let registry = ConnectionRegistry::new();
    let jid = test_jid("alice");

    let (tx_a, _rx_a) = mpsc::channel(4);
    let stale_owner = registry.register(jid.clone(), tx_a);

    let (tx_b, _rx_b) = mpsc::channel(4);
    let live_owner = registry.register(jid.clone(), tx_b);

    // The live owner's write lands.
    assert!(registry.update_presence_state_if_owner(
        &jid,
        &live_owner,
        Some("dnd".to_string()),
        Some("busy".to_string()),
        3,
        Vec::new(),
    ));

    // A's stale-owner write must be refused and leave the state intact.
    assert!(
        !registry.update_presence_state_if_owner(
            &jid,
            &stale_owner,
            Some("chat".to_string()),
            Some("a-status".to_string()),
            9,
            Vec::new(),
        ),
        "a stale owner must not write presence state"
    );
    let state = registry
        .get_presence_state(&jid)
        .expect("replacement presence state");
    assert_eq!(state.show.as_deref(), Some("dnd"));
    assert_eq!(state.status.as_deref(), Some("busy"));
    assert_eq!(state.priority, 3);
}

/// Same race for the unavailable path: a superseded connection's late
/// `<presence type='unavailable'/>` must not clear the replacement's
/// JID-keyed presence state. The ownership check and the
/// `presence_states` removal happen inside one owner-gated call.
#[test]
fn clear_presence_state_if_owner_refuses_stale_owner() {
    let registry = ConnectionRegistry::new();
    let jid = test_jid("alice");

    let (tx_a, _rx_a) = mpsc::channel(4);
    let stale_owner = registry.register(jid.clone(), tx_a);

    let (tx_b, _rx_b) = mpsc::channel(4);
    let live_owner = registry.register(jid.clone(), tx_b);
    assert!(registry.update_presence_state_if_owner(
        &jid,
        &live_owner,
        Some("dnd".to_string()),
        Some("busy".to_string()),
        3,
        Vec::new(),
    ));

    // A's stale-owner clear must be refused and leave B's state intact.
    assert!(
        !registry.clear_presence_state_if_owner(&jid, &stale_owner),
        "a stale owner must not clear presence state"
    );
    assert!(
        registry.get_presence_state(&jid).is_some(),
        "the replacement's presence state must survive the stale-owner clear"
    );

    // The live owner's clear lands.
    assert!(registry.clear_presence_state_if_owner(&jid, &live_owner));
    assert!(registry.get_presence_state(&jid).is_none());
}

/// Owner-gated presence-state write against an unregistered JID is
/// refused and writes nothing.
#[test]
fn update_presence_state_if_owner_refuses_absent_entry() {
    let registry = ConnectionRegistry::new();
    let jid = test_jid("ghost");
    let orphan_owner = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    assert!(!registry.update_presence_state_if_owner(
        &jid,
        &orphan_owner,
        None,
        None,
        0,
        Vec::new(),
    ));
    assert!(registry.get_presence_state(&jid).is_none());
}

#[test]
fn try_send_outbound_if_owner_preserves_kind_and_refuses_stale_owner() {
    let registry = ConnectionRegistry::new();
    let jid = test_jid("alice");

    let (tx_a, _rx_a) = mpsc::channel(4);
    let stale_owner = registry.register(jid.clone(), tx_a);

    let (tx_b, mut rx_b) = mpsc::channel(4);
    let live_owner = registry.register(jid.clone(), tx_b);

    let stale = registry.try_send_outbound_if_owner(
        &jid,
        &stale_owner,
        OutboundStanza::peer_stanza(Stanza::Message(make_test_message("alice@example.com"))),
    );
    assert_eq!(stale, BroadcastOutcome::NotConnected);
    assert!(rx_b.try_recv().is_err());

    let live = registry.try_send_outbound_if_owner(
        &jid,
        &live_owner,
        OutboundStanza::peer_stanza(Stanza::Message(make_test_message("alice@example.com"))),
    );
    assert_eq!(live, BroadcastOutcome::Delivered);
    let outbound = rx_b.try_recv().expect("live owner should receive frame");
    assert_eq!(outbound.kind, DeliveryKind::PeerStanza);
}
