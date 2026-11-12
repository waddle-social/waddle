use super::delivery::{
    GetConnectionEntry, SelectRoutableResources, TrySendDirect, TrySendPeer, TrySendPendingFlush,
};
use super::test_support::{
    ModelComparison, ResourceMutationDisposition, ResourceMutationObservation,
    TakeResourceMutationObservations,
};
use super::{
    health_check_or_wedge_kill, BareJid, ClearPresenceState, ConfirmProvisionalConnection,
    ConflictCloseAllResources, Context, DrainPendingSubscriptions, FullJid, GetAvailableResources,
    GetOtherResources, GetPresenceState, GetResources, HealthCheck, IsCarbonsEnabled, IsConnected,
    PresenceState, QueuePendingSubscription, RegisterConnection, RegisterConnectionIfOwnerOrAbsent,
    RegisterProvisionalConnectionIfExpected, ResourceCount, SetCarbonsEnabled, Stanza,
    UnregisterConnection, UnregisterConnectionAndReportEmpty, UnregisterConnectionOutcome,
    UpdatePresence, UpdatePresenceState, UserActor,
};
use crate::pending_delivery::PendingRowId;
use crate::registry::connection_registry::{
    ActorSocketOutboundEndpoint, ArchiveDispatchRefusalReason, BroadcastOutcome, ConnectionEntry,
    DeliveryKind, OutboundStanza, OutboundWriteAcceptance, SocketOutboundEndpoint,
};
use crate::registry::{
    ConnectionRegistry, LocalRetryDispatchOutcome, LocalRetryRefusal, TrySendDirectWithCustody,
    TrySendPeerWithCustody,
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

#[tokio::test]
async fn provisional_registry_and_actor_mirror_activate_only_exact_current_binding() {
    let registry = ConnectionRegistry::new();
    let actor = spawn_actor("alice").await;
    let jid = full("alice", "phone");
    let connection = crate::ownership::ConnectionRef::fresh(
        crate::ownership::NodeLeaseIdentity::local_process(),
    );
    let binding = crate::ownership::BindingRef::fresh(connection.clone());
    let (socket, mut receiver, _custody) =
        ActorSocketOutboundEndpoint::channel(connection, 2, 4096).expect("actor channel");
    let candidate =
        ConnectionEntry::new_actor_provisional_for_binding(socket, binding, jid.clone())
            .expect("physical binding matches");
    let other_jid = full("alice", "tablet");
    let other_guard = registry.lock_bind(&other_jid).await;
    assert!(!registry.publish_provisional_if_expected(
        &other_guard,
        other_jid.clone(),
        &candidate,
        None,
    ));
    drop(other_guard);
    assert!(!actor
        .ask(RegisterProvisionalConnectionIfExpected {
            jid: other_jid,
            candidate: candidate.clone(),
            expected: None,
        })
        .await
        .expect("cross-JID mirror refusal"));
    let guard = registry.lock_bind(&jid).await;
    assert!(registry.publish_provisional_if_expected(&guard, jid.clone(), &candidate, None));
    drop(guard);
    let registry_entry = registry.entry(&jid).expect("published provisional entry");
    assert!(candidate.matches_entry(&registry_entry));
    assert!(actor
        .ask(RegisterProvisionalConnectionIfExpected {
            jid: jid.clone(),
            candidate: candidate.clone(),
            expected: None,
        })
        .await
        .expect("mirror publication"));
    let actor_entry = actor
        .ask(GetConnectionEntry { jid: jid.clone() })
        .await
        .expect("mirror read")
        .expect("published actor entry");
    assert!(registry_entry.same_exact_local_binding(&actor_entry));
    assert!(receiver.try_recv().is_err());
    let confirmed = actor
        .ask(ConfirmProvisionalConnection {
            jid: jid.clone(),
            candidate: candidate.clone(),
        })
        .await
        .expect("confirm ask")
        .expect("exact mirror confirmed");
    let guard = registry.lock_bind(&jid).await;
    assert!(registry.activate_provisional_if_confirmed(&guard, &jid, &candidate, &confirmed));

    let next_binding = crate::ownership::BindingRef::fresh(candidate.connection_ref().clone());
    let (next_socket, _next_receiver, _next_custody) =
        ActorSocketOutboundEndpoint::channel(candidate.connection_ref().clone(), 2, 4096)
            .expect("fresh channel");
    let next =
        ConnectionEntry::new_actor_provisional_for_binding(next_socket, next_binding, jid.clone())
            .expect("same physical connection");
    assert!(registry.publish_provisional_if_expected(
        &guard,
        jid.clone(),
        &next,
        Some(candidate.binding_id()),
    ));
    drop(guard);
    assert!(actor
        .ask(RegisterProvisionalConnectionIfExpected {
            jid: jid.clone(),
            candidate: next.clone(),
            expected: Some(candidate.binding_id()),
        })
        .await
        .expect("successor mirror"));
    let guard = registry.lock_bind(&jid).await;
    assert!(!registry.activate_provisional_if_confirmed(&guard, &jid, &candidate, &confirmed));
    assert!(!registry.retire_provisional_if_exact(&guard, &jid, &candidate));
    assert_eq!(
        registry
            .entry(&jid)
            .expect("successor remains")
            .binding_id(),
        next.binding_id()
    );
}

/// A throwaway presence stanza — delivery-surface behavior is independent of
/// stanza content, so any stanza works to exercise the routing path.
fn any_stanza() -> Stanza {
    Stanza::Presence(xmpp_parsers::presence::Presence::new(
        xmpp_parsers::presence::Type::None,
    ))
}

fn retry_envelope(
    jid: &FullJid,
    kind: DeliveryKind,
    write_acceptance: OutboundWriteAcceptance,
) -> OutboundStanza {
    let mut message = xmpp_parsers::message::Message::new(Some(jid.clone().into()));
    message.id = Some(xmpp_parsers::message::Id("custody-envelope".to_owned()));
    let stanza = Stanza::Message(message);
    let mut outbound = match kind {
        DeliveryKind::DirectFrame => OutboundStanza::new(stanza),
        DeliveryKind::PeerStanza => OutboundStanza::peer_stanza(stanza),
    };

    let receipt_at = chrono::Utc::now();
    outbound.pending_row_id = Some(PendingRowId::new("custody-row"));
    outbound.original_receipt_at = Some(receipt_at);
    outbound.write_acceptance = Some(write_acceptance);
    outbound.ingress_append = Some(crate::stream_management::SmRelayedAppendObligation {
        key: crate::stream_management::SmIngressAppendKey {
            message_key: crate::ingress::MessageKey::new(),
            kind: crate::stream_management::SmIngressReceiptKind::from_storage(1),
            semantic_identity_hash: [0x21; 32],
            resource: jid.clone(),
        },
        sender_bare: "alice@example.com".parse().expect("valid sender bare JID"),
        received_at: Some(receipt_at),
        archive_positions: vec![crate::stream_management::ArchiveDispatchPosition {
            archive: jid.to_bare(),
            ordinal: crate::mam::ArchiveOrdinal::from_storage(17).expect("valid archive ordinal"),
        }],
        dispatch_stream: None,
    });
    outbound
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
async fn resource_mutation_observer_records_child_decisions_as_inconclusive() {
    let actor = spawn_actor("alice").await;
    let jid = full("alice", "phone");
    let (first, _first_rx) = entry();
    let stale_owner = first.binding_id();
    actor
        .ask(RegisterConnection {
            jid: jid.clone(),
            entry: first,
        })
        .await
        .expect("first child registration");

    let (replacement, _replacement_rx) = entry();
    let replacement_owner = replacement.binding_id();
    actor
        .ask(RegisterConnection {
            jid: jid.clone(),
            entry: replacement,
        })
        .await
        .expect("replacement child registration");
    actor
        .ask(UnregisterConnection {
            jid,
            owner: Some(stale_owner),
        })
        .await
        .expect("stale unregister");

    let batch = actor
        .ask(TakeResourceMutationObservations)
        .await
        .expect("observation batch");
    assert_eq!(batch.overflow_count, 0);
    assert_eq!(batch.observations.len(), 3);
    assert_eq!(
        batch.model_comparison,
        ModelComparison::InconclusiveMissingExternalReceipts
    );
    assert_eq!(batch.unsupported_direct_replacement_count, 1);
    let claim_identity = batch.synthetic_claim_identity.clone();
    let next_batch = actor
        .ask(TakeResourceMutationObservations)
        .await
        .expect("empty observation batch");
    assert_eq!(next_batch.synthetic_claim_identity, claim_identity);
    assert!(next_batch.observations.is_empty());

    assert!(matches!(
        &batch.observations[0],
        ResourceMutationObservation::Register {
            previous: None,
            disposition: ResourceMutationDisposition::Inserted,
            candidate_binding: Some(_),
            ..
        }
    ));
    let ResourceMutationObservation::Register {
        candidate_binding: Some(binding_identity),
        ..
    } = &batch.observations[0]
    else {
        unreachable!("the first local registration carries bounded identity evidence");
    };
    assert!(binding_identity.local_process_incarnation.is_some());
    assert!(matches!(
        &batch.observations[1],
        ResourceMutationObservation::Register {
            previous: Some(previous),
            candidate,
            disposition: ResourceMutationDisposition::Replaced,
            candidate_binding: Some(_),
        } if *previous == stale_owner && *candidate == replacement_owner
    ));
    assert!(matches!(
        &batch.observations[2],
        ResourceMutationObservation::Remove {
            previous: Some(previous),
            owner: Some(owner),
            disposition: ResourceMutationDisposition::Retained,
        } if *previous == replacement_owner && *owner == stale_owner
    ));
}

#[tokio::test]
async fn resource_mutation_observer_bounds_its_journal() {
    let actor = spawn_actor("alice").await;
    let jid = full("alice", "phone");
    for _ in 0..(super::test_support::RESOURCE_MUTATION_OBSERVATION_CAPACITY + 3) {
        let (entry, _rx) = entry();
        actor
            .ask(RegisterConnection {
                jid: jid.clone(),
                entry,
            })
            .await
            .expect("registration remains successful when observation journal fills");
    }
    let jid = full("alice", "overflow");
    let connection = crate::ownership::ConnectionRef::fresh(
        crate::ownership::NodeLeaseIdentity::from_node_identity(
            &crate::ownership::NodeIdentity::new("n".repeat(4096), "e".repeat(4096)),
        ),
    );
    let binding = crate::ownership::BindingRef::fresh(connection.clone());
    let (endpoint, _receiver, _custody) =
        ActorSocketOutboundEndpoint::channel(connection, 2, 4096).expect("endpoint");
    let oversized_entry =
        ConnectionEntry::new_actor_provisional_for_binding(endpoint, binding, jid.clone())
            .expect("binding matches connection")
            .entry_for_publication();
    actor
        .ask(RegisterConnection {
            jid,
            entry: oversized_entry,
        })
        .await
        .expect("oversized observation does not affect registration");

    let batch = actor
        .ask(TakeResourceMutationObservations)
        .await
        .expect("observation batch");
    assert_eq!(
        batch.observations.len(),
        super::test_support::RESOURCE_MUTATION_OBSERVATION_CAPACITY
    );
    assert_eq!(batch.overflow_count, 4);
    assert_eq!(
        batch.model_comparison,
        ModelComparison::InconclusiveMissingExternalReceipts
    );
    let count: usize = actor.ask(ResourceCount).await.expect("actor still serves");
    assert_eq!(count, 2);
}

#[tokio::test]
async fn resource_mutation_observer_bounds_binding_identity_and_skips_snapshots_when_full() {
    use super::test_support::{snapshot_if_capacity, BINDING_OBSERVATION_FIELD_BYTE_CAP};
    use std::cell::Cell;

    let actor = spawn_actor("alice").await;
    let jid = full("alice", "phone");
    let node_id = format!("{}🦆", "n".repeat(BINDING_OBSERVATION_FIELD_BYTE_CAP - 1));
    let node_epoch = "e".repeat(BINDING_OBSERVATION_FIELD_BYTE_CAP + 20);
    let connection = crate::ownership::ConnectionRef::fresh(
        crate::ownership::NodeLeaseIdentity::from_node_identity(
            &crate::ownership::NodeIdentity::new(node_id, node_epoch),
        ),
    );
    let binding = crate::ownership::BindingRef::fresh(connection.clone());
    let (endpoint, _receiver, _custody) =
        ActorSocketOutboundEndpoint::channel(connection, 2, 4096).expect("endpoint");
    let entry = ConnectionEntry::new_actor_provisional_for_binding(endpoint, binding, jid.clone())
        .expect("binding matches connection")
        .entry_for_publication();
    actor
        .ask(RegisterConnection { jid, entry })
        .await
        .expect("registration");

    let batch = actor
        .ask(TakeResourceMutationObservations)
        .await
        .expect("observation batch");
    let ResourceMutationObservation::Register {
        candidate_binding: Some(identity),
        ..
    } = &batch.observations[0]
    else {
        panic!("local binding identity evidence should be present");
    };
    assert_eq!(
        identity.node_id_prefix.as_ref().map(String::len),
        Some(BINDING_OBSERVATION_FIELD_BYTE_CAP - 1),
        "UTF-8 truncation must stop before a partial codepoint"
    );
    assert_eq!(
        identity.node_epoch_prefix.as_ref().map(String::len),
        Some(BINDING_OBSERVATION_FIELD_BYTE_CAP)
    );
    assert!(identity.node_identity_truncated);
    assert_eq!(
        batch.model_comparison,
        ModelComparison::InconclusiveMissingExternalReceipts
    );

    let factory_called = Cell::new(false);
    let snapshot = snapshot_if_capacity(
        super::test_support::RESOURCE_MUTATION_OBSERVATION_CAPACITY,
        || {
            factory_called.set(true);
            String::from("must not allocate after saturation")
        },
    );
    assert!(snapshot.is_none());
    assert!(
        !factory_called.get(),
        "saturated journals must skip snapshot creation"
    );
}

#[tokio::test]
async fn resource_mutation_observer_reports_remote_entries_as_unmodeled() {
    let actor = spawn_actor("alice").await;
    let jid = full("alice", "remote");
    let (endpoint, _receiver) = SocketOutboundEndpoint::channel(1);
    let remote = ConnectionEntry::remote_hosted(endpoint);
    actor
        .ask(RegisterConnection { jid, entry: remote })
        .await
        .expect("remote mirror registration");

    let batch = actor
        .ask(TakeResourceMutationObservations)
        .await
        .expect("observation batch");
    assert!(matches!(
        &batch.observations[0],
        ResourceMutationObservation::Register {
            candidate_binding: None,
            disposition: ResourceMutationDisposition::Inserted,
            ..
        }
    ));
    assert_eq!(
        batch.model_comparison,
        ModelComparison::InconclusiveMissingExternalReceipts
    );
}

#[tokio::test]
async fn resource_mutation_observer_records_provisional_mirror_refusal() {
    let actor = spawn_actor("alice").await;
    let jid = full("alice", "phone");
    let wrong_jid = full("alice", "tablet");
    let connection = crate::ownership::ConnectionRef::fresh(
        crate::ownership::NodeLeaseIdentity::local_process(),
    );
    let binding = crate::ownership::BindingRef::fresh(connection.clone());
    let (endpoint, _receiver, _custody) =
        ActorSocketOutboundEndpoint::channel(connection, 2, 4096).expect("endpoint");
    let candidate = ConnectionEntry::new_actor_provisional_for_binding(endpoint, binding, jid)
        .expect("candidate matches connection");

    let accepted = actor
        .ask(RegisterProvisionalConnectionIfExpected {
            jid: wrong_jid,
            candidate,
            expected: None,
        })
        .await
        .expect("provisional mirror refusal");
    assert!(!accepted);

    let batch = actor
        .ask(TakeResourceMutationObservations)
        .await
        .expect("observation batch");
    assert!(matches!(
        &batch.observations[0],
        ResourceMutationObservation::ProvisionalMirror {
            previous: None,
            candidate_binding: Some(_),
            disposition: ResourceMutationDisposition::Rejected,
            ..
        }
    ));
    assert_eq!(
        batch.model_comparison,
        ModelComparison::InconclusiveMissingExternalReceipts
    );
}

#[tokio::test]
async fn resource_mutation_observer_records_absent_and_unconditional_removal() {
    let actor = spawn_actor("alice").await;
    let jid = full("alice", "phone");
    actor
        .ask(UnregisterConnection {
            jid: jid.clone(),
            owner: Some(crate::registry::LocalConnectionBindingId::fresh()),
        })
        .await
        .expect("absent owner-gated unregister");
    let (entry, _rx) = entry();
    actor
        .ask(RegisterConnection {
            jid: jid.clone(),
            entry,
        })
        .await
        .expect("register");
    actor
        .ask(UnregisterConnection { jid, owner: None })
        .await
        .expect("unconditional unregister");

    let batch = actor
        .ask(TakeResourceMutationObservations)
        .await
        .expect("observation batch");
    assert!(matches!(
        &batch.observations[0],
        ResourceMutationObservation::Remove {
            previous: None,
            owner: Some(_),
            disposition: ResourceMutationDisposition::AlreadyAbsent,
        }
    ));
    assert!(matches!(
        &batch.observations[2],
        ResourceMutationObservation::Remove {
            previous: Some(_),
            owner: None,
            disposition: ResourceMutationDisposition::Removed,
        }
    ));
}

#[tokio::test]
async fn resource_mutation_observer_sees_child_commit_after_caller_timeout() {
    use super::test_support::GateMailbox;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::{oneshot, Notify};

    let actor = spawn_actor("alice").await;
    let entered = Arc::new(Notify::new());
    let (release_tx, release_rx) = oneshot::channel();
    actor
        .tell(GateMailbox {
            entered: Arc::clone(&entered),
            release_rx,
        })
        .await
        .expect("gate enqueues");
    entered.notified().await;

    let (entry, _rx) = entry();
    let jid = full("alice", "phone");
    let result = tokio::time::timeout(
        Duration::from_millis(10),
        actor.ask(RegisterConnection { jid, entry }),
    )
    .await;
    assert!(result.is_err(), "caller times out behind the blocked child");
    release_tx.send(()).expect("release child");

    let count: usize = actor.ask(ResourceCount).await.expect("later child query");
    assert_eq!(
        count, 1,
        "queued child mutation commits after caller timeout"
    );
    let batch = actor
        .ask(TakeResourceMutationObservations)
        .await
        .expect("observation batch");
    assert!(matches!(
        &batch.observations[0],
        ResourceMutationObservation::Register {
            disposition: ResourceMutationDisposition::Inserted,
            ..
        }
    ));
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
    let owner1 = entry1.binding_id();
    let registered: bool = actor
        .ask(RegisterConnectionIfOwnerOrAbsent {
            jid: jid.clone(),
            entry: entry1,
            owner: owner1,
        })
        .await
        .expect("guarded register empty slot");
    assert!(registered);

    let (entry2, _rx2) = entry();
    let owner2 = entry2.binding_id();
    let registered: bool = actor
        .ask(RegisterConnectionIfOwnerOrAbsent {
            jid: jid.clone(),
            entry: entry2,
            owner: owner2,
        })
        .await
        .expect("guarded register occupied slot");
    assert!(!registered);

    let current = actor
        .ask(GetConnectionEntry { jid })
        .await
        .expect("entry lookup")
        .expect("entry remains registered");
    assert_eq!(current.binding_id(), owner1);
    assert_ne!(current.binding_id(), owner2);
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
    let old_owner = old_entry.binding_id();
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
    let new_owner = new_entry.binding_id();
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
                ingress_append: None,
                jid: jid.clone(),
                stanza: any_stanza(),
                original_receipt_at: None,
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
                ingress_append: None,
                jid: jid.clone(),
                stanza: any_stanza(),
                original_receipt_at: None,
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
            ingress_append: None,
            jid: jid.clone(),
            stanza: any_stanza(),
            original_receipt_at: None,
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
            ingress_append: None,
            jid,
            stanza: any_stanza(),
            original_receipt_at: None,
        })
        .await
        .expect("peer");
    assert_eq!(outcome, BroadcastOutcome::Delivered);
    assert_eq!(rx.try_recv().expect("frame").kind, DeliveryKind::PeerStanza);
}

/// Dark P3.1 refusal custody must identify the replacement binding attempt,
/// even when it shares the failed-resume transport with the displaced attempt.
#[tokio::test]
async fn local_direct_refusal_retains_replacement_binding_and_exact_envelope() {
    let actor = spawn_actor("alice").await;
    let jid = full("alice", "phone");
    let transport = crate::ownership::ConnectionRef::fresh(
        crate::ownership::NodeLeaseIdentity::local_process(),
    );
    let failed_resume = crate::ownership::BindingRef::fresh(transport.clone());
    let fresh_bind = crate::ownership::BindingRef::fresh(transport);
    assert_ne!(failed_resume, fresh_bind);

    let (old_endpoint, mut old_rx) = SocketOutboundEndpoint::channel(1);
    let old_entry = ConnectionEntry::new_for_binding(old_endpoint, failed_resume.clone());
    let old_owner = old_entry.binding_id();
    register(&actor, jid.clone(), old_entry).await;

    let (replacement_endpoint, mut replacement_rx) = SocketOutboundEndpoint::channel(1);
    let replacement = ConnectionEntry::new_for_binding(replacement_endpoint, fresh_bind.clone());
    let replacement_owner = replacement.binding_id();
    assert!(replacement.seal_binding(replacement_owner, &fresh_bind));
    register(&actor, jid.clone(), replacement).await;

    let (acceptance, mut acceptance_rx) = OutboundWriteAcceptance::new();
    let expected = retry_envelope(&jid, DeliveryKind::DirectFrame, acceptance.clone());
    let expected_ingress = expected
        .ingress_append
        .clone()
        .expect("test envelope ingress identity");
    let expected_row = expected.pending_row_id.clone();
    let expected_receipt_at = expected.original_receipt_at;

    let outcome: LocalRetryDispatchOutcome = actor
        .ask(TrySendDirectWithCustody {
            jid,
            outbound: expected,
        })
        .await
        .expect("direct custody request");
    let LocalRetryDispatchOutcome::Refused(LocalRetryRefusal::Enqueue {
        owner,
        binding_ref,
        reason,
        outbound,
    }) = outcome
    else {
        panic!("sealed local binding should return its exact refusal custody");
    };
    assert_eq!(owner, replacement_owner);
    assert_ne!(owner, old_owner);
    assert_eq!(binding_ref, fresh_bind);
    assert_ne!(binding_ref, failed_resume);
    assert_eq!(reason, ArchiveDispatchRefusalReason::Sealed);
    assert_eq!(outbound.kind, DeliveryKind::DirectFrame);
    assert_eq!(outbound.pending_row_id, expected_row);
    assert_eq!(outbound.original_receipt_at, expected_receipt_at);
    assert_eq!(outbound.ingress_append, Some(expected_ingress));
    let Stanza::Message(message) = &outbound.stanza else {
        panic!("expected message envelope");
    };
    assert_eq!(
        message.id.as_ref().map(|id| id.0.as_str()),
        Some("custody-envelope")
    );
    assert!(
        old_rx.try_recv().is_err(),
        "displaced binding receives nothing"
    );
    assert!(
        replacement_rx.try_recv().is_err(),
        "sealed binding receives nothing"
    );
    assert!(matches!(
        acceptance_rx.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));

    outbound
        .write_acceptance
        .as_ref()
        .expect("the original callback stays with the envelope")
        .acknowledge();
    acceptance_rx
        .await
        .expect("returned callback remains the original callback");
}

#[tokio::test]
async fn local_peer_refusal_retains_exact_envelope() {
    let actor = spawn_actor("alice").await;
    let jid = full("alice", "tablet");
    let binding_ref = crate::ownership::BindingRef::fresh(crate::ownership::ConnectionRef::fresh(
        crate::ownership::NodeLeaseIdentity::local_process(),
    ));
    let (endpoint, mut receiver) = SocketOutboundEndpoint::channel(1);
    let entry = ConnectionEntry::new_for_binding(endpoint, binding_ref.clone());
    let owner = entry.binding_id();
    assert_eq!(
        entry.try_send_archive_ordered(OutboundStanza::new(any_stanza())),
        Ok(()),
        "fill the local queue before the custody attempt"
    );
    register(&actor, jid.clone(), entry).await;

    let (acceptance, mut acceptance_rx) = OutboundWriteAcceptance::new();
    let expected = retry_envelope(&jid, DeliveryKind::PeerStanza, acceptance.clone());
    let expected_ingress = expected
        .ingress_append
        .clone()
        .expect("test envelope ingress identity");
    let expected_row = expected.pending_row_id.clone();
    let expected_receipt_at = expected.original_receipt_at;
    let outcome: LocalRetryDispatchOutcome = actor
        .ask(TrySendPeerWithCustody {
            jid,
            outbound: expected,
        })
        .await
        .expect("peer custody request");
    let LocalRetryDispatchOutcome::Refused(LocalRetryRefusal::Enqueue {
        owner: actual_owner,
        binding_ref: actual_binding,
        reason,
        outbound,
    }) = outcome
    else {
        panic!("full local queue should return its exact refusal custody");
    };
    assert_eq!(actual_owner, owner);
    assert_eq!(actual_binding, binding_ref);
    assert_eq!(reason, ArchiveDispatchRefusalReason::Full);
    assert_eq!(outbound.kind, DeliveryKind::PeerStanza);
    assert_eq!(outbound.pending_row_id, expected_row);
    assert_eq!(outbound.original_receipt_at, expected_receipt_at);
    assert_eq!(outbound.ingress_append, Some(expected_ingress));
    let Stanza::Message(message) = &outbound.stanza else {
        panic!("expected message envelope");
    };
    assert_eq!(
        message.id.as_ref().map(|id| id.0.as_str()),
        Some("custody-envelope")
    );
    assert!(
        receiver.try_recv().is_ok(),
        "the preexisting frame stays queued"
    );
    assert!(
        receiver.try_recv().is_err(),
        "refused frame does not join the queue"
    );
    assert!(matches!(
        acceptance_rx.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));
    outbound
        .write_acceptance
        .as_ref()
        .expect("the original callback stays with the envelope")
        .acknowledge();
    acceptance_rx
        .await
        .expect("returned callback remains the original callback");
}

#[tokio::test]
async fn local_retry_api_preserves_envelope_without_remote_or_offline_fallback() {
    let actor = spawn_actor("alice").await;
    let jid = full("alice", "remote");
    let (endpoint, mut receiver) = SocketOutboundEndpoint::channel(1);
    register(
        &actor,
        jid.clone(),
        ConnectionEntry::remote_hosted(endpoint),
    )
    .await;

    let (acceptance, mut acceptance_rx) = OutboundWriteAcceptance::new();
    let expected = retry_envelope(&jid, DeliveryKind::DirectFrame, acceptance.clone());
    let expected_ingress = expected
        .ingress_append
        .clone()
        .expect("test envelope ingress identity");
    let outcome: LocalRetryDispatchOutcome = actor
        .ask(TrySendDirectWithCustody {
            jid,
            outbound: expected,
        })
        .await
        .expect("direct custody request");
    let LocalRetryDispatchOutcome::Refused(LocalRetryRefusal::NotLocallyHosted { outbound }) =
        outcome
    else {
        panic!("remote mirror must be refused by the local-only API");
    };
    assert_eq!(outbound.kind, DeliveryKind::DirectFrame);
    assert_eq!(outbound.ingress_append, Some(expected_ingress));
    assert!(
        receiver.try_recv().is_err(),
        "remote mirror receives nothing"
    );
    assert!(matches!(
        acceptance_rx.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));
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
            ingress_append: None,
            jid: jid.clone(),
            stanza: any_stanza(),
            original_receipt_at: None,
        })
        .await
        .expect("first");
    assert_eq!(first, BroadcastOutcome::Delivered);

    let second: BroadcastOutcome = actor
        .ask(TrySendDirect {
            ingress_append: None,
            jid: jid.clone(),
            stanza: any_stanza(),
            original_receipt_at: None,
        })
        .await
        .expect("second");
    assert_eq!(second, BroadcastOutcome::DroppedFull);

    let absent: BroadcastOutcome = actor
        .ask(TrySendDirect {
            ingress_append: None,
            jid: full("alice", "ghost"),
            stanza: any_stanza(),
            original_receipt_at: None,
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
            ingress_append: None,
            jid: jid.clone(),
            stanza: any_stanza(),
            original_receipt_at: None,
        })
        .await
        .expect("closed send");
    assert_eq!(outcome, BroadcastOutcome::DroppedClosed);
    let observed = actor
        .ask(TakeResourceMutationObservations)
        .await
        .expect("closed-channel observation");
    assert!(matches!(
        observed.observations.last(),
        Some(ResourceMutationObservation::Remove {
            previous: Some(_),
            disposition: ResourceMutationDisposition::Removed,
            ..
        })
    ));

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
            ingress_append: None,
            jid,
            stanza: any_stanza(),
            original_receipt_at: None,
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
            ingress_append: None,
            jid,
            stanza: any_stanza(),
            original_receipt_at: None,
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
    assert_eq!(frame.original_receipt_at, Some(receipt_at));
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
    let observed = actor
        .ask(TakeResourceMutationObservations)
        .await
        .expect("conflict-close observation");
    assert!(matches!(
        observed.observations.last(),
        Some(ResourceMutationObservation::ConflictClose { resource_count: 2 })
    ));
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

/// One resource of a selection case: name, availability, priority, carbons.
type SelectionCase = &'static [(&'static str, bool, i8, bool)];

const SELECTION_CASES: &[SelectionCase] = &[
    &[],
    &[("solo", true, 0, false)],
    &[("solo", false, 5, true)],
    &[("low", true, 1, false), ("high", true, 10, true)],
    &[
        ("a", true, 3, false),
        ("b", true, 3, true),
        ("c", true, 2, false),
    ],
    &[("negative", true, -1, true), ("zero", true, 0, false)],
    &[("negative", true, -1, false), ("lower", true, -128, true)],
    &[("away", false, 100, true), ("here", true, 1, false)],
    &[("max", true, 127, false), ("also-max", true, 127, false)],
];

/// The account coordinator's pure state answers every selection read the
/// way the `UserActor` does for the same resources.
#[tokio::test]
async fn account_resources_select_the_same_resources_as_the_user_actor() {
    use std::num::NonZeroUsize;

    use crate::ownership::account_coordinator::AccountResources;
    use crate::ownership::account_resource_transition::{
        AccountResourceExecution, AccountResourceMutation, AccountResourceOperation,
        AccountResourceOperationId, AccountResourceOperationOutcome, AccountResourceRevision,
        AccountResourceSnapshot, ActorRegistrationReceipt, BindingEvidence,
        EndpointPublicationReceipt,
    };
    use crate::ownership::joint_identity::ResourceLifetime;
    use crate::ownership::{
        BindingRef, ClaimEpoch, ConnectionRef, NodeLeaseIdentity, UserClaimIdentity,
    };
    use crate::registry::user_actor::GetRoutingResources;

    for (index, case) in SELECTION_CASES.iter().enumerate() {
        let actor = spawn_actor("alice").await;
        let claim = UserClaimIdentity::fresh(
            bare("alice"),
            NodeLeaseIdentity::local_process(),
            ClaimEpoch(1),
        );
        let mut account = AccountResources::new(
            claim.clone(),
            NonZeroUsize::new(16).expect("positive slot bound"),
        );
        let mut receivers = Vec::new();
        for (name, available, priority, carbons) in case.iter().copied() {
            let jid = full("alice", name);
            let (connection, receiver) = entry();
            receivers.push(receiver);
            actor
                .ask(RegisterConnection {
                    jid: jid.clone(),
                    entry: connection,
                })
                .await
                .expect("register with the user actor");
            actor
                .ask(UpdatePresence {
                    jid: jid.clone(),
                    available,
                    priority,
                })
                .await
                .expect("presence on the user actor");
            actor
                .ask(SetCarbonsEnabled {
                    jid: jid.clone(),
                    enabled: carbons,
                })
                .await
                .expect("carbons on the user actor");

            let binding =
                BindingRef::fresh(ConnectionRef::fresh(NodeLeaseIdentity::local_process()));
            let evidence = BindingEvidence::for_binding(&binding);
            let mutations = [
                AccountResourceMutation::Bind {
                    claim_identity: claim.clone(),
                    expected_revision: AccountResourceRevision::new(0),
                    binding_ref: binding.clone(),
                    lifetime: ResourceLifetime::fresh(),
                },
                AccountResourceMutation::PublishEndpoint {
                    claim_identity: claim.clone(),
                    expected_revision: AccountResourceRevision::new(1),
                    binding_ref: binding.clone(),
                    publication: EndpointPublicationReceipt::observed(evidence.clone()),
                },
                AccountResourceMutation::RegisterActor {
                    claim_identity: claim.clone(),
                    expected_revision: AccountResourceRevision::new(2),
                    binding_ref: binding.clone(),
                    registration: ActorRegistrationReceipt::observed(claim.clone(), evidence),
                },
                AccountResourceMutation::UpdateSnapshot {
                    claim_identity: claim.clone(),
                    expected_revision: AccountResourceRevision::new(3),
                    binding_ref: binding,
                    snapshot: AccountResourceSnapshot {
                        presence_available: available,
                        presence_priority: priority,
                        carbons_enabled: carbons,
                        ..AccountResourceSnapshot::default()
                    },
                },
            ];
            for mutation in mutations {
                let execution = account
                    .execute(
                        &jid,
                        AccountResourceOperation::new(
                            AccountResourceOperationId::fresh(),
                            mutation,
                        ),
                    )
                    .expect("the account accepts the operation");
                assert!(
                    matches!(
                        execution,
                        AccountResourceExecution::Applied(
                            AccountResourceOperationOutcome::Committed { .. }
                        )
                    ),
                    "case {index}: {execution:?}"
                );
            }
        }

        let mut resources = actor.ask(GetResources).await.expect("resources");
        resources.sort();
        assert_eq!(account.resources(), resources, "case {index}: resources");

        let mut available = actor
            .ask(GetAvailableResources)
            .await
            .expect("available resources");
        available.sort();
        assert_eq!(
            account.available_resources(),
            available,
            "case {index}: available resources"
        );

        let mut routable = actor
            .ask(SelectRoutableResources)
            .await
            .expect("routable resources");
        routable.sort();
        assert_eq!(
            account.routable_resources(),
            routable,
            "case {index}: routable resources"
        );

        let routing: Vec<_> = actor
            .ask(GetRoutingResources)
            .await
            .expect("routing snapshot")
            .into_iter()
            .map(|state| {
                (
                    state.jid,
                    state.available,
                    state.priority,
                    state.carbons_enabled,
                )
            })
            .collect();
        let modelled: Vec<_> = account
            .routing_snapshot()
            .into_iter()
            .map(|state| {
                (
                    state.jid,
                    state.available,
                    state.priority,
                    state.carbons_enabled,
                )
            })
            .collect();
        assert_eq!(modelled, routing, "case {index}: routing snapshot");
    }
}
