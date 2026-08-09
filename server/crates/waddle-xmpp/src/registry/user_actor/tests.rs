use super::delivery::{
    GetConnectionEntry, SelectRoutableResources, TrySendDirect, TrySendPeer, TrySendPendingFlush,
};
use super::*;
use crate::pending_delivery::PendingRowId;
use crate::registry::connection_registry::{
    BroadcastOutcome, ConnectionEntry, DeliveryKind, OutboundStanza,
};
use kameo::actor::{ActorRef, Spawn};
use kameo::error::SendError;
use tokio::sync::{mpsc, oneshot};

/// Default mailbox capacity used by `Spawn::spawn` in kameo 0.20 — the test
/// exercises the bounded-mailbox backpressure path without depending on the
/// numeric default.
const DEFAULT_MAILBOX_CAPACITY: usize = 64;

/// Build a `ConnectionEntry` backed by a bounded channel of `cap`, returning
/// the entry (to register) and the receiver (to assert delivered frames).
/// The caller must keep the receiver alive; dropping it closes the channel.
fn entry_with_capacity(cap: usize) -> (ConnectionEntry, mpsc::Receiver<OutboundStanza>) {
    let (tx, rx) = mpsc::channel(cap);
    (ConnectionEntry::new(tx), rx)
}

/// Convenience `ConnectionEntry` with a comfortable capacity for lifecycle
/// tests that do not assert delivery.
fn entry() -> (ConnectionEntry, mpsc::Receiver<OutboundStanza>) {
    entry_with_capacity(16)
}

/// A throwaway presence stanza — delivery-surface behavior is independent of
/// stanza content, so any stanza works to exercise the routing path.
fn any_stanza() -> Stanza {
    Stanza::Presence(xmpp_parsers::presence::Presence::new(
        xmpp_parsers::presence::Type::None,
    ))
}

struct HoldMailboxUntilReleased {
    release_rx: oneshot::Receiver<()>,
}

impl kameo::message::Message<HoldMailboxUntilReleased> for UserActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: HoldMailboxUntilReleased,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let _ = msg.release_rx.await;
    }
}

fn bare(user: &str) -> BareJid {
    format!("{user}@example.com").parse().expect("bare jid")
}

fn full(user: &str, resource: &str) -> FullJid {
    format!("{user}@example.com/{resource}")
        .parse()
        .expect("full jid")
}

async fn spawn_actor(user: &str) -> ActorRef<UserActor> {
    UserActor::spawn(UserActor::new(bare(user)))
}

#[tokio::test]
async fn test_register_and_resource_count() {
    let actor = spawn_actor("alice").await;
    let (e, _rx) = entry();

    actor
        .ask(RegisterConnection {
            jid: full("alice", "phone"),
            entry: e,
        })
        .await
        .expect("register");

    let count: usize = actor.ask(ResourceCount).await.expect("count");
    assert_eq!(count, 1);
}

#[tokio::test]
async fn test_unregister_cleans_up() {
    let actor = spawn_actor("alice").await;
    let jid = full("alice", "phone");
    let (e, _rx) = entry();

    actor
        .ask(RegisterConnection {
            jid: jid.clone(),
            entry: e,
        })
        .await
        .expect("register");

    actor
        .ask(UnregisterConnection {
            jid: jid.clone(),
            owner: None,
        })
        .await
        .expect("unregister");

    let count: usize = actor.ask(ResourceCount).await.expect("count");
    assert_eq!(count, 0);

    let connected: bool = actor.ask(IsConnected { jid }).await.expect("connected");
    assert!(!connected);
}

#[tokio::test]
async fn test_register_if_owner_or_absent_refuses_replacement_owner() {
    let actor = spawn_actor("alice").await;
    let jid = full("alice", "phone");

    let (entry1, _rx1) = entry();
    let owner1 = std::sync::Arc::clone(&entry1.carbons_enabled);
    let registered: bool = actor
        .ask(RegisterConnectionIfOwnerOrAbsent {
            jid: jid.clone(),
            entry: entry1,
            owner: owner1.clone(),
        })
        .await
        .expect("guarded register empty slot");
    assert!(registered);

    let (entry2, _rx2) = entry();
    let owner2 = std::sync::Arc::clone(&entry2.carbons_enabled);
    let registered: bool = actor
        .ask(RegisterConnectionIfOwnerOrAbsent {
            jid: jid.clone(),
            entry: entry2,
            owner: owner2.clone(),
        })
        .await
        .expect("guarded register occupied slot");
    assert!(!registered);

    let current = actor
        .ask(GetConnectionEntry { jid })
        .await
        .expect("entry lookup")
        .expect("entry remains registered");
    assert!(std::sync::Arc::ptr_eq(&current.carbons_enabled, &owner1));
    assert!(!std::sync::Arc::ptr_eq(&current.carbons_enabled, &owner2));
}

#[tokio::test]
async fn test_unregister_and_report_empty_is_atomic_per_user_actor() {
    let actor = spawn_actor("alice").await;
    let jid = full("alice", "phone");
    let (e, _rx) = entry();

    actor
        .ask(RegisterConnection {
            jid: jid.clone(),
            entry: e,
        })
        .await
        .expect("register");

    let outcome = actor
        .ask(UnregisterConnectionAndReportEmpty { jid, owner: None })
        .await
        .expect("unregister+check");
    assert_eq!(
        outcome,
        UnregisterConnectionOutcome::Removed { is_empty: true }
    );
}

/// Owner-gated unregister mirrors the DashMap `unregister_if_owner`: a lagging
/// teardown carrying the OLD session's token must not evict the resource once a
/// replacement session has re-registered the same full JID. Only the token that
/// matches the stored entry may remove it.
#[tokio::test]
async fn test_unregister_is_owner_gated() {
    let actor = spawn_actor("alice").await;
    let jid = full("alice", "phone");

    let (old_entry, _old_rx) = entry();
    let old_owner = std::sync::Arc::clone(&old_entry.carbons_enabled);
    actor
        .ask(RegisterConnection {
            jid: jid.clone(),
            entry: old_entry,
        })
        .await
        .expect("register old");

    // A replacement session takes over the same full JID with a fresh entry
    // (distinct ownership token).
    let (new_entry, _new_rx) = entry();
    let new_owner = std::sync::Arc::clone(&new_entry.carbons_enabled);
    actor
        .ask(RegisterConnection {
            jid: jid.clone(),
            entry: new_entry,
        })
        .await
        .expect("register replacement");

    // The old session's lagging teardown must NOT evict the replacement.
    actor
        .ask(UnregisterConnection {
            jid: jid.clone(),
            owner: Some(old_owner),
        })
        .await
        .expect("stale unregister");
    let connected: bool = actor
        .ask(IsConnected { jid: jid.clone() })
        .await
        .expect("connected");
    assert!(
        connected,
        "stale-owner unregister must not evict the replacement resource"
    );

    // The replacement's own teardown (matching token) removes it.
    let outcome = actor
        .ask(UnregisterConnectionAndReportEmpty {
            jid,
            owner: Some(new_owner),
        })
        .await
        .expect("owned unregister");
    assert_eq!(
        outcome,
        UnregisterConnectionOutcome::Removed { is_empty: true },
        "matching-owner unregister must remove the resource"
    );
}

#[tokio::test]
async fn test_get_resources() {
    let actor = spawn_actor("alice").await;
    let (e1, _rx1) = entry();
    let (e2, _rx2) = entry();

    actor
        .ask(RegisterConnection {
            jid: full("alice", "phone"),
            entry: e1,
        })
        .await
        .expect("register");

    actor
        .ask(RegisterConnection {
            jid: full("alice", "laptop"),
            entry: e2,
        })
        .await
        .expect("register");

    let resources: Vec<FullJid> = actor.ask(GetResources).await.expect("resources");
    assert_eq!(resources.len(), 2);
}

#[tokio::test]
async fn test_get_other_resources() {
    let actor = spawn_actor("alice").await;
    let phone = full("alice", "phone");
    let laptop = full("alice", "laptop");
    let (e1, _rx1) = entry();
    let (e2, _rx2) = entry();

    actor
        .ask(RegisterConnection {
            jid: phone.clone(),
            entry: e1,
        })
        .await
        .expect("register");

    actor
        .ask(RegisterConnection {
            jid: laptop.clone(),
            entry: e2,
        })
        .await
        .expect("register");

    let others: Vec<FullJid> = actor
        .ask(GetOtherResources {
            exclude: phone.clone(),
        })
        .await
        .expect("others");

    assert_eq!(others.len(), 1);
    assert_eq!(others[0], laptop);
}

#[tokio::test]
async fn test_presence_update_and_available_resources() {
    let actor = spawn_actor("alice").await;
    let phone = full("alice", "phone");
    let laptop = full("alice", "laptop");
    let (e1, _rx1) = entry();
    let (e2, _rx2) = entry();

    actor
        .ask(RegisterConnection {
            jid: phone.clone(),
            entry: e1,
        })
        .await
        .expect("register");

    actor
        .ask(RegisterConnection {
            jid: laptop.clone(),
            entry: e2,
        })
        .await
        .expect("register");

    // Initially no resources are available
    let available: Vec<(FullJid, i8)> = actor.ask(GetAvailableResources).await.expect("available");
    assert!(available.is_empty());

    // Make phone available with priority 5
    let updated: bool = actor
        .ask(UpdatePresence {
            jid: phone.clone(),
            available: true,
            priority: 5,
        })
        .await
        .expect("update");
    assert!(updated);

    let available: Vec<(FullJid, i8)> = actor.ask(GetAvailableResources).await.expect("available");
    assert_eq!(available.len(), 1);
    assert_eq!(available[0].0, phone);
    assert_eq!(available[0].1, 5);
}

#[tokio::test]
async fn test_update_presence_missing_resource() {
    let actor = spawn_actor("alice").await;
    let missing = full("alice", "missing");

    let updated: bool = actor
        .ask(UpdatePresence {
            jid: missing,
            available: true,
            priority: 0,
        })
        .await
        .expect("update");
    assert!(!updated);
}

#[tokio::test]
async fn test_presence_state() {
    let actor = spawn_actor("alice").await;
    let jid = full("alice", "phone");

    // No state before setting
    let state: Option<PresenceState> = actor
        .ask(GetPresenceState { jid: jid.clone() })
        .await
        .expect("get");
    assert!(state.is_none());

    // Set state
    actor
        .ask(UpdatePresenceState {
            jid: jid.clone(),
            show: Some("away".to_string()),
            status: Some("Gone fishing".to_string()),
            priority: 3,
        })
        .await
        .expect("update");

    let state: Option<PresenceState> = actor
        .ask(GetPresenceState { jid: jid.clone() })
        .await
        .expect("get");
    let state = state.expect("should have state");
    assert_eq!(state.show.as_deref(), Some("away"));
    assert_eq!(state.status.as_deref(), Some("Gone fishing"));
    assert_eq!(state.priority, 3);

    // Clear state
    actor
        .ask(ClearPresenceState { jid: jid.clone() })
        .await
        .expect("clear");

    let state: Option<PresenceState> = actor.ask(GetPresenceState { jid }).await.expect("get");
    assert!(state.is_none());
}

#[tokio::test]
async fn test_pending_subscriptions() {
    let actor = spawn_actor("alice").await;

    let subscribe = xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::Subscribe);
    actor
        .ask(QueuePendingSubscription {
            stanza: Stanza::Presence(subscribe),
        })
        .await
        .expect("queue");

    let drained: Vec<Stanza> = actor.ask(DrainPendingSubscriptions).await.expect("drain");
    assert_eq!(drained.len(), 1);

    // Second drain should be empty
    let drained: Vec<Stanza> = actor.ask(DrainPendingSubscriptions).await.expect("drain");
    assert!(drained.is_empty());
}

#[tokio::test]
async fn test_carbons() {
    let actor = spawn_actor("alice").await;
    let jid = full("alice", "phone");
    let (e, _rx) = entry();

    actor
        .ask(RegisterConnection {
            jid: jid.clone(),
            entry: e,
        })
        .await
        .expect("register");

    // Default is disabled
    let enabled: bool = actor
        .ask(IsCarbonsEnabled { jid: jid.clone() })
        .await
        .expect("check");
    assert!(!enabled);

    // Enable
    actor
        .ask(SetCarbonsEnabled {
            jid: jid.clone(),
            enabled: true,
        })
        .await
        .expect("set");

    let enabled: bool = actor.ask(IsCarbonsEnabled { jid }).await.expect("check");
    assert!(enabled);
}

#[tokio::test]
async fn test_mailbox_backpressure_marks_best_effort_as_dropped() {
    let actor = spawn_actor("alice").await;

    let (release_tx, release_rx) = oneshot::channel();
    actor
        .tell(HoldMailboxUntilReleased { release_rx })
        .await
        .expect("hold message should enqueue");

    let jid = full("alice", "phone");
    let mut saw_mailbox_full = false;
    for _ in 0..(DEFAULT_MAILBOX_CAPACITY * 2) {
        let send_result = actor
            .tell(SetCarbonsEnabled {
                jid: jid.clone(),
                enabled: true,
            })
            .try_send();
        if matches!(send_result, Err(SendError::MailboxFull(_))) {
            saw_mailbox_full = true;
            break;
        }
        send_result.expect("best-effort tell should either enqueue or return MailboxFull");
    }
    assert!(
        saw_mailbox_full,
        "bounded mailbox should eventually apply backpressure"
    );

    let _ = release_tx.send(());
}

// ---------------------------------------------------------------------------
// Delivery surface (ADR-0017 Phase 1 invariants)
// ---------------------------------------------------------------------------

/// The connection outbound-channel capacity — mirrors
/// `waddle_server::…::websocket::connection::OUTBOUND_CHANNEL_SIZE` (256).
/// The per-connection task that drains this channel is the "connection
/// actor" of ADR-0017 Phase 1; this constant is its mailbox capacity
/// (`CONNECTION_ACTOR_MAILBOX_CAPACITY`), never kameo's spawn default of 64.
const CONNECTION_ACTOR_MAILBOX_CAPACITY: usize = 256;

async fn register(actor: &ActorRef<UserActor>, jid: FullJid, entry: ConnectionEntry) {
    actor
        .ask(RegisterConnection { jid, entry })
        .await
        .expect("register");
}

async fn make_available(actor: &ActorRef<UserActor>, jid: FullJid, priority: i8) {
    let updated: bool = actor
        .ask(UpdatePresence {
            jid,
            available: true,
            priority,
        })
        .await
        .expect("update presence");
    assert!(updated);
}

/// Invariant 6: no outbound drop-rate regression under a join-burst. A slow
/// client that never drains its channel while a 200-occupant room fans in
/// must buffer the whole burst without a single `DroppedFull` — the 256
/// capacity absorbs it, where kameo's default 64 would drop from the 65th.
#[tokio::test]
async fn join_burst_does_not_drop_at_capacity_256() {
    const OCCUPANTS: usize = 200;
    // Compile-time guard: the burst must fit under the 256 mailbox.
    const _: () = assert!(OCCUPANTS < CONNECTION_ACTOR_MAILBOX_CAPACITY);

    // This test drives 200 sends through the process-global broadcast
    // counters; serialize against metric-asserting tests so it cannot perturb
    // their reads (Copilot review on PR #1177).
    let _guard = crate::prometheus::metrics_test_lock().lock().await;

    let actor = spawn_actor("alice").await;
    let jid = full("alice", "phone");
    // Never drained: the receiver is held but no recv() is called, modelling a
    // slow socket during the burst.
    let (e, _rx) = entry_with_capacity(CONNECTION_ACTOR_MAILBOX_CAPACITY);
    register(&actor, jid.clone(), e).await;

    for _ in 0..OCCUPANTS {
        let outcome: BroadcastOutcome = actor
            .ask(TrySendPeer {
                jid: jid.clone(),
                stanza: any_stanza(),
            })
            .await
            .expect("fan-out send");
        assert_eq!(
            outcome,
            BroadcastOutcome::Delivered,
            "256 capacity must absorb a 200-occupant join burst without drops"
        );
    }
}

/// The contrast that justifies the 256 capacity: kameo's default 64 would
/// drop under the same 200-occupant burst, so the explicit constant is
/// load-bearing, not incidental.
#[tokio::test]
async fn join_burst_drops_at_default_capacity_64() {
    const OCCUPANTS: usize = 200;
    // Serialize against metric-asserting tests (Copilot review on PR #1177).
    let _guard = crate::prometheus::metrics_test_lock().lock().await;

    let actor = spawn_actor("alice").await;
    let jid = full("alice", "phone");
    let (e, _rx) = entry_with_capacity(64);
    register(&actor, jid.clone(), e).await;

    let mut dropped = 0usize;
    for _ in 0..OCCUPANTS {
        let outcome: BroadcastOutcome = actor
            .ask(TrySendPeer {
                jid: jid.clone(),
                stanza: any_stanza(),
            })
            .await
            .expect("fan-out send");
        if outcome == BroadcastOutcome::DroppedFull {
            dropped += 1;
        }
    }
    assert!(
        dropped > 0,
        "a 64-capacity channel must drop under a 200-occupant burst"
    );
}

/// Invariant 7: RFC 6121 §8.5.2.1 — only available resources are candidates,
/// negative-priority resources are excluded, and ties at the top priority all
/// route.
#[tokio::test]
async fn select_routable_excludes_negative_priority_and_unavailable() {
    let actor = spawn_actor("alice").await;
    let phone = full("alice", "phone");
    let laptop = full("alice", "laptop");
    let bot = full("alice", "bot");
    let offline = full("alice", "offline");
    let (e1, _r1) = entry();
    let (e2, _r2) = entry();
    let (e3, _r3) = entry();
    let (e4, _r4) = entry();
    register(&actor, phone.clone(), e1).await;
    register(&actor, laptop.clone(), e2).await;
    register(&actor, bot.clone(), e3).await;
    register(&actor, offline.clone(), e4).await;

    // phone + laptop tie at priority 5; bot is negative; offline is unavailable.
    make_available(&actor, phone.clone(), 5).await;
    make_available(&actor, laptop.clone(), 5).await;
    make_available(&actor, bot.clone(), -1).await;
    // `offline` never advertises availability.

    let mut selected: Vec<FullJid> = actor.ask(SelectRoutableResources).await.expect("select");
    selected.sort_by_key(|j| j.to_string());
    assert_eq!(
        selected,
        vec![laptop, phone],
        "tie at top priority routes to both; negative and unavailable excluded"
    );
}

/// Invariant 7 (fallback): all-negative or no-available resources select
/// nothing, so the caller falls back to offline storage.
#[tokio::test]
async fn select_routable_empty_when_only_negative_priority() {
    let actor = spawn_actor("alice").await;
    let bot = full("alice", "bot");
    let (e, _r) = entry();
    register(&actor, bot.clone(), e).await;
    make_available(&actor, bot, -1).await;

    let selected: Vec<FullJid> = actor.ask(SelectRoutableResources).await.expect("select");
    assert!(
        selected.is_empty(),
        "no non-negative available resource → offline fallback"
    );
}

/// Invariant 7 (top-priority): RFC 6121 §8.5.2.1.2 — a lower positive priority
/// is NOT a destination when a strictly-higher priority is available. Pins the
/// `*p == max_priority` filter so it can't silently degrade to a plain `>= 0`
/// filter (which would deliver a bare-JID 1:1 message to both resources).
#[tokio::test]
async fn select_routable_prefers_top_priority_over_lower_positive() {
    let actor = spawn_actor("alice").await;
    let phone = full("alice", "phone");
    let laptop = full("alice", "laptop");
    let (e1, _r1) = entry();
    let (e2, _r2) = entry();
    register(&actor, phone.clone(), e1).await;
    register(&actor, laptop.clone(), e2).await;

    // Both available and non-negative, but at DIFFERENT priorities.
    make_available(&actor, phone.clone(), 5).await;
    make_available(&actor, laptop.clone(), 3).await;

    let selected: Vec<FullJid> = actor.ask(SelectRoutableResources).await.expect("select");
    assert_eq!(
        selected,
        vec![phone],
        "only the strictly-highest-priority resource is a destination"
    );
}

/// Invariant 3: the DirectFrame vs PeerStanza recipient-pass split survives
/// the actor boundary on the queued envelope.
#[tokio::test]
async fn delivery_preserves_direct_vs_peer_kind() {
    let actor = spawn_actor("alice").await;
    let jid = full("alice", "phone");
    let (e, mut rx) = entry();
    register(&actor, jid.clone(), e).await;

    let outcome: BroadcastOutcome = actor
        .ask(TrySendDirect {
            jid: jid.clone(),
            stanza: any_stanza(),
        })
        .await
        .expect("direct");
    assert_eq!(outcome, BroadcastOutcome::Delivered);
    assert_eq!(
        rx.try_recv().expect("frame").kind,
        DeliveryKind::DirectFrame
    );

    let outcome: BroadcastOutcome = actor
        .ask(TrySendPeer {
            jid,
            stanza: any_stanza(),
        })
        .await
        .expect("peer");
    assert_eq!(outcome, BroadcastOutcome::Delivered);
    assert_eq!(rx.try_recv().expect("frame").kind, DeliveryKind::PeerStanza);
}

/// Invariant 1 + 5: a full channel drops non-blocking as `DroppedFull`; an
/// absent resource is `NotConnected`.
#[tokio::test]
async fn delivery_full_channel_drops_without_blocking() {
    let actor = spawn_actor("alice").await;
    let jid = full("alice", "phone");
    // Capacity 1, and we never drain — the second send finds the channel full.
    let (e, _rx) = entry_with_capacity(1);
    register(&actor, jid.clone(), e).await;

    let first: BroadcastOutcome = actor
        .ask(TrySendDirect {
            jid: jid.clone(),
            stanza: any_stanza(),
        })
        .await
        .expect("first");
    assert_eq!(first, BroadcastOutcome::Delivered);

    let second: BroadcastOutcome = actor
        .ask(TrySendDirect {
            jid: jid.clone(),
            stanza: any_stanza(),
        })
        .await
        .expect("second");
    assert_eq!(second, BroadcastOutcome::DroppedFull);

    let absent: BroadcastOutcome = actor
        .ask(TrySendDirect {
            jid: full("alice", "ghost"),
            stanza: any_stanza(),
        })
        .await
        .expect("absent");
    assert_eq!(absent, BroadcastOutcome::NotConnected);
}

/// Invariant 2 + 5: a closed channel yields `DroppedClosed` and evicts the
/// stale entry; a re-registered resource with a fresh sender then delivers to
/// the new channel (the actor-model analogue of the DashMap replacement path,
/// race-free because register and send serialize through the mailbox).
#[tokio::test]
async fn delivery_closed_channel_evicts_and_replacement_delivers() {
    let actor = spawn_actor("alice").await;
    let jid = full("alice", "phone");

    let (e_dead, rx_dead) = entry();
    register(&actor, jid.clone(), e_dead).await;
    drop(rx_dead); // consumer task died — channel now closed.

    let outcome: BroadcastOutcome = actor
        .ask(TrySendDirect {
            jid: jid.clone(),
            stanza: any_stanza(),
        })
        .await
        .expect("closed send");
    assert_eq!(outcome, BroadcastOutcome::DroppedClosed);

    // Entry was evicted.
    let connected: bool = actor
        .ask(IsConnected { jid: jid.clone() })
        .await
        .expect("connected");
    assert!(!connected, "closed entry should have been evicted");

    // A replacement connection registers and receives.
    let (e_live, mut rx_live) = entry();
    register(&actor, jid.clone(), e_live).await;
    let outcome: BroadcastOutcome = actor
        .ask(TrySendDirect {
            jid,
            stanza: any_stanza(),
        })
        .await
        .expect("replacement send");
    assert_eq!(outcome, BroadcastOutcome::Delivered);
    assert!(rx_live.try_recv().is_ok(), "replacement channel receives");
}

/// Invariant 2: re-registering an already-connected resource whose channel is
/// still OPEN takes over the resource — delivery lands on the new receiver and
/// the stale live sender receives nothing (the common "new stream takes over"
/// case, distinct from register-after-eviction).
#[tokio::test]
async fn register_over_live_entry_routes_to_new_receiver() {
    let actor = spawn_actor("alice").await;
    let jid = full("alice", "phone");

    let (e_old, mut rx_old) = entry();
    register(&actor, jid.clone(), e_old).await;

    // Re-register the same resource without closing the old channel.
    let (e_new, mut rx_new) = entry();
    register(&actor, jid.clone(), e_new).await;

    let outcome: BroadcastOutcome = actor
        .ask(TrySendDirect {
            jid,
            stanza: any_stanza(),
        })
        .await
        .expect("send");
    assert_eq!(outcome, BroadcastOutcome::Delivered);
    assert!(
        rx_new.try_recv().is_ok(),
        "delivery must land on the new receiver"
    );
    assert!(
        rx_old.try_recv().is_err(),
        "the replaced live sender must receive nothing"
    );
}

/// Invariant 4: the Q7b pending-flush SM-row binding rides the queued
/// envelope so the destination can bind the assigned XEP-0198 counter back to
/// the row.
#[tokio::test]
async fn pending_flush_carries_row_binding() {
    let actor = spawn_actor("alice").await;
    let jid = full("alice", "phone");
    let (e, mut rx) = entry();
    register(&actor, jid.clone(), e).await;

    let row_id = PendingRowId::fresh();
    let receipt_at = chrono::DateTime::from_timestamp(1_700_000_000, 0).expect("timestamp");
    let outcome: BroadcastOutcome = actor
        .ask(TrySendPendingFlush {
            jid,
            stanza: any_stanza(),
            row_id: row_id.clone(),
            original_receipt_at: receipt_at,
        })
        .await
        .expect("flush");
    assert_eq!(outcome, BroadcastOutcome::Delivered);

    let frame = rx.try_recv().expect("frame");
    assert_eq!(frame.kind, DeliveryKind::DirectFrame);
    assert_eq!(frame.pending_row_id.as_ref(), Some(&row_id));
    assert_eq!(frame.pending_row_original_receipt_at, Some(receipt_at));
}

/// The read-only accessor returns the live entry for a connected resource and
/// `None` otherwise.
#[tokio::test]
async fn get_connection_entry_reflects_registration() {
    let actor = spawn_actor("alice").await;
    let jid = full("alice", "phone");

    let none: Option<ConnectionEntry> = actor
        .ask(GetConnectionEntry { jid: jid.clone() })
        .await
        .expect("get");
    assert!(none.is_none());

    let (e, _rx) = entry();
    register(&actor, jid.clone(), e).await;

    let some: Option<ConnectionEntry> = actor.ask(GetConnectionEntry { jid }).await.expect("get");
    assert!(some.is_some());
}

// ---------------------------------------------------------------------------
// Ownership (ADR-0017 Phase 3 Slice 3: steal-intent owner-veto path)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn health_check_replies_when_the_actor_is_idle() {
    let actor = spawn_actor("alice").await;
    actor.ask(HealthCheck).await.expect("health check replies");
}

#[tokio::test]
async fn conflict_close_all_resources_clears_every_registered_resource() {
    let actor = spawn_actor("alice").await;
    let (e1, _rx1) = entry();
    let (e2, _rx2) = entry();
    register(&actor, full("alice", "phone"), e1).await;
    register(&actor, full("alice", "desktop"), e2).await;

    let torn_down: usize = actor
        .ask(ConflictCloseAllResources)
        .await
        .expect("conflict close");
    assert_eq!(torn_down, 2);

    let count: usize = actor.ask(ResourceCount).await.expect("count");
    assert_eq!(count, 0);
}

#[tokio::test]
async fn health_check_or_wedge_kill_returns_true_for_a_healthy_actor() {
    let actor = spawn_actor("alice").await;
    let healthy = health_check_or_wedge_kill(&actor, std::time::Duration::from_secs(5)).await;
    assert!(healthy);
    assert!(actor.is_alive(), "a healthy actor must not be killed");
}

#[tokio::test]
async fn health_check_or_wedge_kill_kills_a_wedged_actor() {
    let actor = spawn_actor("alice").await;
    let (e, _rx) = entry();
    register(&actor, full("alice", "phone"), e).await;

    // Wedge the actor: this message occupies the mailbox loop indefinitely
    // (until released), so `HealthCheck` — enqueued after it — can never be
    // answered before a short timeout elapses.
    let (_release_tx, release_rx) = oneshot::channel();
    actor
        .tell(HoldMailboxUntilReleased { release_rx })
        .await
        .expect("hold message should enqueue");

    let healthy = health_check_or_wedge_kill(&actor, std::time::Duration::from_millis(100)).await;
    assert!(!healthy, "a wedged actor must fail its bounded health ask");

    // `kill()` is unconditional (unlike the best-effort conflict-close
    // tell), so the actor must stop even though it never got to process
    // `ConflictCloseAllResources`.
    let mut killed = false;
    for _ in 0..50 {
        if !actor.is_alive() {
            killed = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(
        killed,
        "a failed health ask must proactively kill the actor"
    );
    // `_release_tx` drops here, releasing the wedge — harmless, the actor
    // is already gone.
}
