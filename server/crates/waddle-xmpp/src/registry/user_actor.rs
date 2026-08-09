//! Per-user actor managing connection state, presence, and delivery.
//!
//! One `UserActor` exists per bare JID. It owns all mutable per-user state
//! (connected resources with their outbound channels, presence, pending
//! subscriptions, carbons) and processes messages sequentially, removing the
//! need for external locking.
//!
//! The delivery surface lives in the [`delivery`] submodule: it reproduces
//! the `ConnectionRegistry` routing invariants (RFC 6121 §8.5.2.1 bare-JID
//! resource selection, the `DeliveryKind` recipient-pass split, non-blocking
//! `try_send` fan-out with typed [`BroadcastOutcome`], and the Q7b
//! pending-flush SM-row binding) inside the actor so the DashMap delivery
//! path can be retired once the migration's invariant tests cover it (see
//! ADR-0017 Phase 1).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use jid::{BareJid, FullJid};
use kameo::message::Context;
use kameo::Actor;
use tracing::debug;

use crate::registry::connection_registry::{ConnectionEntry, PresenceState};
use crate::Stanza;

pub mod delivery;

/// Actor that manages per-user connection state and stanza delivery.
///
/// Each connected bare JID gets exactly one `UserActor`. The actor owns one
/// [`ConnectionEntry`] per connected resource — the entry is the single
/// source of truth for that resource's outbound channel plus its presence
/// availability, priority, and carbons atomics — along with per-resource
/// presence state and any pending subscription stanzas that arrived while the
/// user was offline.
#[derive(Actor)]
pub struct UserActor {
    bare_jid: BareJid,
    /// Connected resources keyed by full JID. The [`ConnectionEntry`] carries
    /// the outbound `mpsc::Sender` and the presence-**availability**/priority
    /// and carbons atomics, so those are single-sourced here with no parallel
    /// map to drift out of sync. (`presence_states` below is a distinct
    /// concern — the full show/status/priority snapshot used for XEP-0012 /
    /// probe responses — not a duplicate of the availability atomics.)
    connections: HashMap<FullJid, ConnectionEntry>,
    /// Per-resource show/status/priority snapshot for presence-probe replies.
    presence_states: HashMap<FullJid, PresenceState>,
    pending_subscriptions: Vec<Stanza>,
}

impl UserActor {
    /// Create a new `UserActor` for the given bare JID.
    pub fn new(bare_jid: BareJid) -> Self {
        Self {
            bare_jid,
            connections: HashMap::new(),
            presence_states: HashMap::new(),
            pending_subscriptions: Vec::new(),
        }
    }

    /// The bare JID this actor manages.
    pub fn bare_jid(&self) -> &BareJid {
        &self.bare_jid
    }

    /// Remove all state associated with a resource.
    fn remove_resource(&mut self, jid: &FullJid) {
        self.connections.remove(jid);
        self.presence_states.remove(jid);
    }

    /// Ownership-gated removal, mirroring the DashMap
    /// `ConnectionRegistry::unregister_if_owner` semantics (ADR-0017 Phase 1
    /// Slice 0). `owner` is the resource's ownership token — the entry's
    /// `carbons_enabled` `Arc<AtomicBool>`, the same handle the DashMap
    /// compares with `Arc::ptr_eq`. `Some(owner)` removes only when the stored
    /// entry is still that owner's, so a lagging unregister for a superseded
    /// connection cannot evict the replacement resource; `None` removes
    /// unconditionally, matching a plain DashMap `unregister`. Returns whether
    /// an entry was removed.
    fn remove_resource_if_owner(&mut self, jid: &FullJid, owner: Option<&Arc<AtomicBool>>) -> bool {
        let should_remove = match (self.connections.get(jid), owner) {
            (Some(entry), Some(token)) => Arc::ptr_eq(&entry.carbons_enabled, token),
            (Some(_), None) => true,
            (None, _) => false,
        };
        if should_remove {
            self.remove_resource(jid);
        }
        should_remove
    }
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

/// Register a new connection (resource) for this user.
///
/// Carries the resource's outbound channel via a [`ConnectionEntry`] so the
/// actor owns the delivery surface for the resource. Re-registering an
/// already-present full JID replaces the entry — this is the actor-model
/// analogue of the `ConnectionRegistry` replacement path (a client's new
/// stream taking over an existing resource), made race-free by the mailbox
/// serializing register against every send.
pub struct RegisterConnection {
    pub jid: FullJid,
    pub entry: ConnectionEntry,
}

impl kameo::message::Message<RegisterConnection> for UserActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: RegisterConnection,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        debug!(jid = %msg.jid, bare = %self.bare_jid, "Registering connection");
        self.connections.insert(msg.jid, msg.entry);
    }
}

/// Register a resource only if the slot is empty or still belongs to `owner`.
///
/// Used by clustered remote-resource mirrors, where the socket node has already
/// published a local registry entry and the owner node must not clobber a newer
/// local same-resource bind while mirroring the remote entry.
pub struct RegisterConnectionIfOwnerOrAbsent {
    pub jid: FullJid,
    pub entry: ConnectionEntry,
    pub owner: Arc<AtomicBool>,
}

impl kameo::message::Message<RegisterConnectionIfOwnerOrAbsent> for UserActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: RegisterConnectionIfOwnerOrAbsent,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if !Arc::ptr_eq(&msg.entry.carbons_enabled, &msg.owner) {
            return false;
        }
        if self
            .connections
            .get(&msg.jid)
            .is_some_and(|entry| !Arc::ptr_eq(&entry.carbons_enabled, &msg.owner))
        {
            return false;
        }
        debug!(jid = %msg.jid, bare = %self.bare_jid, "Registering owner-gated connection");
        self.connections.insert(msg.jid, msg.entry);
        true
    }
}

/// Unregister a connection (resource) for this user.
///
/// `owner` is the ownership token (the resource's `carbons_enabled`
/// `Arc<AtomicBool>`): `Some` gates removal on the stored entry still being
/// that owner's (so a lagging unregister can't evict a replacement), `None`
/// removes unconditionally — matching the DashMap `unregister_if_owner` vs
/// plain `unregister` call the teardown site makes.
pub struct UnregisterConnection {
    pub jid: FullJid,
    pub owner: Option<Arc<AtomicBool>>,
}

impl kameo::message::Message<UnregisterConnection> for UserActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: UnregisterConnection,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        debug!(jid = %msg.jid, bare = %self.bare_jid, "Unregistering connection");
        self.remove_resource_if_owner(&msg.jid, msg.owner.as_ref());
    }
}

/// Unregister a connection and return whether the user has no resources left.
///
/// `owner` is ownership-gated exactly like [`UnregisterConnection`].
pub struct UnregisterConnectionAndReportEmpty {
    pub jid: FullJid,
    pub owner: Option<Arc<AtomicBool>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, kameo::Reply)]
pub enum UnregisterConnectionOutcome {
    Removed { is_empty: bool },
    AlreadyAbsent { is_empty: bool },
    RetainedTargetPresent,
}

impl kameo::message::Message<UnregisterConnectionAndReportEmpty> for UserActor {
    type Reply = UnregisterConnectionOutcome;

    async fn handle(
        &mut self,
        msg: UnregisterConnectionAndReportEmpty,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        debug!(
            jid = %msg.jid,
            bare = %self.bare_jid,
            "Unregistering connection and checking emptiness"
        );
        let target_present_before = self.connections.contains_key(&msg.jid);
        let removed = self.remove_resource_if_owner(&msg.jid, msg.owner.as_ref());
        let is_empty = self.connections.is_empty();
        if removed {
            UnregisterConnectionOutcome::Removed { is_empty }
        } else if target_present_before {
            UnregisterConnectionOutcome::RetainedTargetPresent
        } else {
            UnregisterConnectionOutcome::AlreadyAbsent { is_empty }
        }
    }
}

/// Get all connected resource JIDs.
pub struct GetResources;

impl kameo::message::Message<GetResources> for UserActor {
    type Reply = Vec<FullJid>;

    async fn handle(
        &mut self,
        _msg: GetResources,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.connections.keys().cloned().collect()
    }
}

/// Get available resources with their priorities (presence-available only,
/// with priority) — distinct from [`GetResources`] (all connected) and
/// `SelectRoutableResources` (RFC-6121 routable ranking). ADR-0017 Phase 3
/// Slice 9 retired its former production caller (the DashMap-liveness
/// selection filter); it is retained as a supported `UserActor` query of the
/// availability accounting, exercised by the actor's own tests.
pub struct GetAvailableResources;

impl kameo::message::Message<GetAvailableResources> for UserActor {
    type Reply = Vec<(FullJid, i8)>;

    async fn handle(
        &mut self,
        _msg: GetAvailableResources,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.connections
            .iter()
            .filter(|(_, entry)| entry.is_presence_available())
            .map(|(jid, entry)| (jid.clone(), entry.presence_priority()))
            .collect()
    }
}

/// Get all connected resources except a specific one.
pub struct GetOtherResources {
    pub exclude: FullJid,
}

impl kameo::message::Message<GetOtherResources> for UserActor {
    type Reply = Vec<FullJid>;

    async fn handle(
        &mut self,
        msg: GetOtherResources,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.connections
            .keys()
            .filter(|jid| **jid != msg.exclude)
            .cloned()
            .collect()
    }
}

/// Update presence availability and priority for a resource.
pub struct UpdatePresence {
    pub jid: FullJid,
    pub available: bool,
    pub priority: i8,
}

impl kameo::message::Message<UpdatePresence> for UserActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: UpdatePresence,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if let Some(entry) = self.connections.get(&msg.jid) {
            entry
                .presence_available
                .store(msg.available, Ordering::Relaxed);
            entry
                .presence_priority
                .store(msg.priority, Ordering::Relaxed);
            true
        } else {
            false
        }
    }
}

/// Update full presence state (show/status/priority) for a resource.
pub struct UpdatePresenceState {
    pub jid: FullJid,
    pub show: Option<String>,
    pub status: Option<String>,
    pub priority: i8,
}

impl kameo::message::Message<UpdatePresenceState> for UserActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: UpdatePresenceState,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.presence_states.insert(
            msg.jid,
            PresenceState {
                show: msg.show,
                status: msg.status,
                priority: msg.priority,
                // This actor store is not on the payload relay path (the
                // connection registry is); it never carries extension payloads.
                payloads: Vec::new(),
            },
        );
    }
}

/// Get the stored presence state for a resource.
pub struct GetPresenceState {
    pub jid: FullJid,
}

impl kameo::message::Message<GetPresenceState> for UserActor {
    type Reply = Option<PresenceState>;

    async fn handle(
        &mut self,
        msg: GetPresenceState,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.presence_states.get(&msg.jid).cloned()
    }
}

/// Clear stored presence state for a resource.
pub struct ClearPresenceState {
    pub jid: FullJid,
}

impl kameo::message::Message<ClearPresenceState> for UserActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: ClearPresenceState,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.presence_states.remove(&msg.jid);
    }
}

/// Queue a subscription stanza for later delivery.
pub struct QueuePendingSubscription {
    pub stanza: Stanza,
}

impl kameo::message::Message<QueuePendingSubscription> for UserActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: QueuePendingSubscription,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.pending_subscriptions.push(msg.stanza);
    }
}

/// Drain and return all pending subscription stanzas.
pub struct DrainPendingSubscriptions;

impl kameo::message::Message<DrainPendingSubscriptions> for UserActor {
    type Reply = Vec<Stanza>;

    async fn handle(
        &mut self,
        _msg: DrainPendingSubscriptions,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        std::mem::take(&mut self.pending_subscriptions)
    }
}

/// Check if a specific resource is connected.
pub struct IsConnected {
    pub jid: FullJid,
}

impl kameo::message::Message<IsConnected> for UserActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: IsConnected,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.connections.contains_key(&msg.jid)
    }
}

/// Set carbons enabled/disabled for a resource.
///
/// No-op when the resource is not connected (the entry, and therefore the
/// carbons atomic, only exists while the resource is registered).
pub struct SetCarbonsEnabled {
    pub jid: FullJid,
    pub enabled: bool,
}

impl kameo::message::Message<SetCarbonsEnabled> for UserActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: SetCarbonsEnabled,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if let Some(entry) = self.connections.get(&msg.jid) {
            entry.carbons_enabled.store(msg.enabled, Ordering::Relaxed);
        }
    }
}

/// Check if carbons is enabled for a resource.
pub struct IsCarbonsEnabled {
    pub jid: FullJid,
}

impl kameo::message::Message<IsCarbonsEnabled> for UserActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: IsCarbonsEnabled,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.connections
            .get(&msg.jid)
            .map(|entry| entry.is_carbons_enabled())
            .unwrap_or(false)
    }
}

/// Get the number of connected resources.
pub struct ResourceCount;

impl kameo::message::Message<ResourceCount> for UserActor {
    type Reply = usize;

    async fn handle(
        &mut self,
        _msg: ResourceCount,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.connections.len()
    }
}

// ---------------------------------------------------------------------------
// Ownership (ADR-0017 Phase 3 Slice 3: steal-intent owner-veto path)
// ---------------------------------------------------------------------------

/// Internal liveness probe for the owner-veto path (ADR-0017 Phase 3 Slice
/// 3, element 4's "Unwedge" text): a successful reply proves this actor's
/// mailbox loop is live and responsive right now. Kameo processes a mailbox
/// strictly in order, so an actor wedged inside a prior handler never
/// reaches this message — the caller observes a timeout, not a reply,
/// which is exactly the signal [`health_check_or_wedge_kill`] acts on.
///
/// Phase 4 Slice 1b wires production `UserActor` claim acquisition/release in
/// `UserRegistryActor`; the server-side `UserLocalClaims` implementation asks
/// this probe during deposed-owner veto and routes failures through
/// [`health_check_or_wedge_kill`].
pub struct HealthCheck;

impl kameo::message::Message<HealthCheck> for UserActor {
    type Reply = ();

    async fn handle(
        &mut self,
        _msg: HealthCheck,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
    }
}

/// Proactively tear down every locally-registered resource for this user
/// (ADR-0017 Phase 3 Slice 3's "conflict-closes any live local socket for
/// that user"). Internal actor message only: no wire stanza or stream error
/// is synthesized here (hard rule — no new stanzas/namespaces this slice).
/// Dropping each [`ConnectionEntry`]'s `sender` breaks delivery to that
/// resource immediately; the resource's own connection task discovers its
/// outbound channel has closed on its next send attempt and tears the
/// socket down from there — the same path `BroadcastOutcome::DroppedClosed`
/// already exercises for a single stale entry, applied here to every
/// resource at once.
///
/// Returns the number of resources torn down (observability/tests).
pub struct ConflictCloseAllResources;

impl kameo::message::Message<ConflictCloseAllResources> for UserActor {
    type Reply = usize;

    async fn handle(
        &mut self,
        _msg: ConflictCloseAllResources,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let count = self.connections.len();
        self.connections.clear();
        self.presence_states.clear();
        count
    }
}

/// Health-ask `actor_ref` and, on failure, proactively wedge-kill it (the
/// ADR's unwedge text, verbatim): "an owner whose internal health ask fails
/// during steal-intent processing does not wait to be stolen from: it kills
/// the wedged actor and conflict-closes its sockets before the steal lands
/// at `intent_ttl`, since it already knows the steal will proceed."
///
/// Bounded by `timeout` on both mailbox admission and reply — mirrors
/// `muc::room_registry_handle::RoomRegistry`'s bounded-ask pattern (#757):
/// kameo processes messages strictly in mailbox order, so a wedged actor
/// (stuck inside a prior handler) never reaches [`HealthCheck`] and the ask
/// times out here rather than hanging the caller.
///
/// Returns `true` if the actor answered healthily (no action taken), `false`
/// if it was wedge-killed (resources best-effort torn down, then the actor
/// stopped outright).
pub async fn health_check_or_wedge_kill(
    actor_ref: &kameo::actor::ActorRef<UserActor>,
    timeout: std::time::Duration,
) -> bool {
    let healthy = actor_ref
        .ask(HealthCheck)
        .mailbox_timeout(timeout)
        .reply_timeout(timeout)
        .await
        .is_ok();
    if healthy {
        return true;
    }
    tracing::warn!(
        "UserActor failed its internal health ask; proactively wedge-killing and \
         conflict-closing its resources ahead of the pending steal"
    );
    // Best-effort: `tell` only enqueues, so this can still reach a jammed
    // mailbox even though the bounded ask above could not complete in time.
    // `kill()` below is what actually guarantees the outcome regardless —
    // dropping the actor's state drops every `ConnectionEntry`'s `sender`
    // too, closing every resource's outbound channel even if this enqueued
    // message never gets to run.
    let _ = actor_ref.tell(ConflictCloseAllResources).await;
    actor_ref.kill();
    false
}

/// Cross-crate test controls, kept behind the existing `test-utils` feature.
#[cfg(feature = "test-utils")]
pub mod test_support {
    use super::UserActor;
    use std::sync::Arc;
    use tokio::sync::{oneshot, Notify};

    pub struct GateMailbox {
        pub entered: Arc<Notify>,
        pub release_rx: oneshot::Receiver<()>,
    }

    impl kameo::message::Message<GateMailbox> for UserActor {
        type Reply = ();

        async fn handle(
            &mut self,
            msg: GateMailbox,
            _ctx: &mut kameo::message::Context<Self, Self::Reply>,
        ) -> Self::Reply {
            msg.entered.notify_one();
            let _ = msg.release_rx.await;
        }
    }

    pub struct MailboxNoop;

    impl kameo::message::Message<MailboxNoop> for UserActor {
        type Reply = ();

        async fn handle(
            &mut self,
            _msg: MailboxNoop,
            _ctx: &mut kameo::message::Context<Self, Self::Reply>,
        ) -> Self::Reply {
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
