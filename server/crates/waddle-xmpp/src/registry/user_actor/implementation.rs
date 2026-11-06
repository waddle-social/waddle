//! Implementation behind the parent module facade.

use super::debug;
use super::Actor;
use super::Context;
use super::HashMap;
use super::Ordering;
use super::Stanza;
use super::{BareJid, FullJid};
use super::{ConnectionEntry, LocalConnectionBindingId, PresenceState};
#[cfg(any(test, feature = "test-utils"))]
use crate::registry::ActorProvisionalConnectionEntry;

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
    pub(in crate::registry::user_actor) bare_jid: BareJid,
    /// Connected resources keyed by full JID. The [`ConnectionEntry`] carries
    /// the outbound `mpsc::Sender` and the presence-**availability**/priority
    /// and carbons atomics, so those are single-sourced here with no parallel
    /// map to drift out of sync. (`presence_states` below is a distinct
    /// concern — the full show/status/priority snapshot used for XEP-0012 /
    /// probe responses — not a duplicate of the availability atomics.)
    pub(in crate::registry::user_actor) connections: HashMap<FullJid, ConnectionEntry>,
    /// Per-resource show/status/priority snapshot for presence-probe replies.
    pub(in crate::registry::user_actor) presence_states: HashMap<FullJid, PresenceState>,
    pub(in crate::registry::user_actor) pending_subscriptions: Vec<Stanza>,
    /// Test-only, bounded child-mutation journal. It stores copied IDs and
    /// dispositions only; it never retains serving state or affects replies.
    #[cfg(any(test, feature = "test-utils"))]
    pub(in crate::registry::user_actor) transition_observations:
        Vec<super::test_support::ResourceMutationObservation>,
    #[cfg(any(test, feature = "test-utils"))]
    pub(in crate::registry::user_actor) transition_observation_overflow: usize,
    /// Fixture label only; never read by production authority paths.
    #[cfg(any(test, feature = "test-utils"))]
    pub(in crate::registry::user_actor) synthetic_claim_identity:
        crate::ownership::UserClaimIdentity,
}

impl UserActor {
    /// Create a new `UserActor` for the given bare JID.
    pub fn new(bare_jid: BareJid) -> Self {
        #[cfg(any(test, feature = "test-utils"))]
        let synthetic_claim_identity = crate::ownership::UserClaimIdentity::fresh(
            bare_jid.clone(),
            crate::ownership::NodeLeaseIdentity::local_process(),
            crate::ownership::ClaimEpoch(0),
        );
        Self {
            bare_jid,
            connections: HashMap::new(),
            presence_states: HashMap::new(),
            pending_subscriptions: Vec::new(),
            #[cfg(any(test, feature = "test-utils"))]
            transition_observations: Vec::with_capacity(
                super::test_support::RESOURCE_MUTATION_OBSERVATION_CAPACITY,
            ),
            #[cfg(any(test, feature = "test-utils"))]
            transition_observation_overflow: 0,
            #[cfg(any(test, feature = "test-utils"))]
            synthetic_claim_identity,
        }
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub(in crate::registry::user_actor) fn observe_resource_mutation(
        &mut self,
        observation: impl FnOnce() -> super::test_support::ResourceMutationObservation,
    ) {
        if self.transition_observations.len()
            < super::test_support::RESOURCE_MUTATION_OBSERVATION_CAPACITY
        {
            self.transition_observations.push(observation());
        } else {
            self.transition_observation_overflow =
                self.transition_observation_overflow.saturating_add(1);
        }
    }

    #[cfg(any(test, feature = "test-utils"))]
    fn bounded_binding_for_observation(
        &self,
        binding_ref: Option<&crate::ownership::BindingRef>,
    ) -> Option<crate::ownership::BoundedBindingObservation> {
        super::test_support::snapshot_if_capacity(self.transition_observations.len(), || {
            binding_ref.map(crate::ownership::BindingRef::bounded_observation)
        })
        .flatten()
    }

    /// The bare JID this actor manages.
    pub fn bare_jid(&self) -> &BareJid {
        &self.bare_jid
    }

    /// Remove all state associated with a resource.
    pub(in crate::registry::user_actor) fn remove_resource(&mut self, jid: &FullJid) {
        self.remove_resource_with_owner(jid, None);
    }

    fn remove_resource_with_owner(
        &mut self,
        jid: &FullJid,
        owner: Option<LocalConnectionBindingId>,
    ) {
        #[cfg(not(any(test, feature = "test-utils")))]
        let _ = owner;
        #[cfg(any(test, feature = "test-utils"))]
        let previous = self.connections.get(jid).map(ConnectionEntry::binding_id);
        if let Some(entry) = self.connections.get(jid) {
            entry.seal_actor_binding_if_any();
        }
        self.connections.remove(jid);
        self.presence_states.remove(jid);
        #[cfg(any(test, feature = "test-utils"))]
        self.observe_resource_mutation(|| {
            super::test_support::ResourceMutationObservation::Remove {
                previous,
                owner,
                disposition: if previous.is_some() {
                    super::test_support::ResourceMutationDisposition::Removed
                } else {
                    super::test_support::ResourceMutationDisposition::AlreadyAbsent
                },
            }
        });
    }

    /// Ownership-gated removal, mirroring the DashMap
    /// `ConnectionRegistry::unregister_if_owner` semantics (ADR-0017 Phase 1
    /// Slice 0). `owner` is the entry's local binding identity, the same value
    /// compared by the registry. `Some(owner)` removes only when the stored
    /// entry is still that owner's, so a lagging unregister for a superseded
    /// connection cannot evict the replacement resource; `None` removes
    /// unconditionally, matching a plain DashMap `unregister`. Returns whether
    /// an entry was removed.
    pub(in crate::registry::user_actor) fn remove_resource_if_owner(
        &mut self,
        jid: &FullJid,
        owner: Option<&LocalConnectionBindingId>,
    ) -> bool {
        let should_remove = match (self.connections.get(jid), owner) {
            (Some(entry), Some(token)) => entry.binding_id() == *token,
            (Some(_), None) => true,
            (None, _) => false,
        };
        if should_remove {
            self.remove_resource_with_owner(jid, owner.copied());
        } else {
            #[cfg(any(test, feature = "test-utils"))]
            let previous = self.connections.get(jid).map(ConnectionEntry::binding_id);
            #[cfg(any(test, feature = "test-utils"))]
            self.observe_resource_mutation(|| {
                super::test_support::ResourceMutationObservation::Remove {
                    previous,
                    owner: owner.copied(),
                    disposition: if previous.is_some() {
                        super::test_support::ResourceMutationDisposition::Retained
                    } else {
                        super::test_support::ResourceMutationDisposition::AlreadyAbsent
                    },
                }
            });
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
        #[cfg(any(test, feature = "test-utils"))]
        let previous = self
            .connections
            .get(&msg.jid)
            .map(ConnectionEntry::binding_id);
        #[cfg(any(test, feature = "test-utils"))]
        let candidate = msg.entry.binding_id();
        #[cfg(any(test, feature = "test-utils"))]
        let candidate_binding = self.bounded_binding_for_observation(msg.entry.binding_ref());
        if let Some(entry) = self.connections.get(&msg.jid) {
            entry.seal_actor_binding_if_any();
        }
        self.connections.insert(msg.jid, msg.entry);
        #[cfg(any(test, feature = "test-utils"))]
        self.observe_resource_mutation(|| {
            super::test_support::ResourceMutationObservation::Register {
                previous,
                candidate,
                candidate_binding,
                disposition: if previous.is_some() {
                    super::test_support::ResourceMutationDisposition::Replaced
                } else {
                    super::test_support::ResourceMutationDisposition::Inserted
                },
            }
        });
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
    pub owner: LocalConnectionBindingId,
}

impl kameo::message::Message<RegisterConnectionIfOwnerOrAbsent> for UserActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: RegisterConnectionIfOwnerOrAbsent,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if msg.entry.binding_id() != msg.owner {
            #[cfg(any(test, feature = "test-utils"))]
            let candidate_binding = self.bounded_binding_for_observation(msg.entry.binding_ref());
            #[cfg(any(test, feature = "test-utils"))]
            let previous = self
                .connections
                .get(&msg.jid)
                .map(ConnectionEntry::binding_id);
            #[cfg(any(test, feature = "test-utils"))]
            self.observe_resource_mutation(|| {
                super::test_support::ResourceMutationObservation::RegisterIfOwnerOrAbsent {
                    previous,
                    candidate: msg.entry.binding_id(),
                    candidate_binding,
                    owner: msg.owner,
                    disposition: super::test_support::ResourceMutationDisposition::Rejected,
                }
            });
            return false;
        }
        if self
            .connections
            .get(&msg.jid)
            .is_some_and(|entry| entry.binding_id() != msg.owner)
        {
            #[cfg(any(test, feature = "test-utils"))]
            let candidate_binding = self.bounded_binding_for_observation(msg.entry.binding_ref());
            #[cfg(any(test, feature = "test-utils"))]
            let previous = self
                .connections
                .get(&msg.jid)
                .map(ConnectionEntry::binding_id);
            #[cfg(any(test, feature = "test-utils"))]
            self.observe_resource_mutation(|| {
                super::test_support::ResourceMutationObservation::RegisterIfOwnerOrAbsent {
                    previous,
                    candidate: msg.entry.binding_id(),
                    candidate_binding,
                    owner: msg.owner,
                    disposition: super::test_support::ResourceMutationDisposition::Rejected,
                }
            });
            return false;
        }
        debug!(jid = %msg.jid, bare = %self.bare_jid, "Registering owner-gated connection");
        if let Some(entry) = self.connections.get(&msg.jid) {
            entry.seal_actor_binding_if_any();
        }
        #[cfg(any(test, feature = "test-utils"))]
        let previous = self
            .connections
            .get(&msg.jid)
            .map(ConnectionEntry::binding_id);
        #[cfg(any(test, feature = "test-utils"))]
        let candidate = msg.entry.binding_id();
        #[cfg(any(test, feature = "test-utils"))]
        let candidate_binding = self.bounded_binding_for_observation(msg.entry.binding_ref());
        self.connections.insert(msg.jid, msg.entry);
        #[cfg(any(test, feature = "test-utils"))]
        self.observe_resource_mutation(|| {
            super::test_support::ResourceMutationObservation::RegisterIfOwnerOrAbsent {
                previous,
                candidate,
                candidate_binding,
                owner: msg.owner,
                disposition: if previous.is_some() {
                    super::test_support::ResourceMutationDisposition::Replaced
                } else {
                    super::test_support::ResourceMutationDisposition::Inserted
                },
            }
        });
        true
    }
}

/// Mirror a still-provisional local actor binding only if the exact
/// predecessor remains. This message never activates its endpoint.
#[cfg(any(test, feature = "test-utils"))]
pub struct RegisterProvisionalConnectionIfExpected {
    pub jid: FullJid,
    pub candidate: ActorProvisionalConnectionEntry,
    pub expected: Option<LocalConnectionBindingId>,
}

/// Test-only mailbox barrier for ambiguous provisional mirror outcomes.
#[cfg(feature = "test-utils")]
pub struct HoldUserActorForPublication {
    pub entered: tokio::sync::oneshot::Sender<()>,
    pub release: tokio::sync::oneshot::Receiver<()>,
}

#[cfg(feature = "test-utils")]
impl kameo::message::Message<HoldUserActorForPublication> for UserActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: HoldUserActorForPublication,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let _ = msg.entered.send(());
        let _ = msg.release.await;
    }
}

#[cfg(any(test, feature = "test-utils"))]
impl kameo::message::Message<RegisterProvisionalConnectionIfExpected> for UserActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: RegisterProvisionalConnectionIfExpected,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let previous = self
            .connections
            .get(&msg.jid)
            .map(ConnectionEntry::binding_id);
        let candidate = msg.candidate.binding_id();
        let candidate_binding = self.bounded_binding_for_observation(msg.candidate.binding_ref());
        if msg.jid.to_bare() != self.bare_jid
            || msg.candidate.jid() != &msg.jid
            || !msg.candidate.is_provisional()
            || msg.candidate.binding_ref().is_none()
            || self
                .connections
                .get(&msg.jid)
                .map(ConnectionEntry::binding_id)
                != msg.expected
        {
            self.observe_resource_mutation(|| {
                super::test_support::ResourceMutationObservation::ProvisionalMirror {
                    previous,
                    candidate,
                    candidate_binding,
                    expected: msg.expected,
                    disposition: super::test_support::ResourceMutationDisposition::Rejected,
                }
            });
            return false;
        }
        if let Some(previous) = self.connections.get(&msg.jid) {
            previous.seal_actor_binding_if_any();
        }
        self.connections
            .insert(msg.jid, msg.candidate.entry_for_publication());
        self.observe_resource_mutation(|| {
            super::test_support::ResourceMutationObservation::ProvisionalMirror {
                previous,
                candidate,
                candidate_binding,
                expected: msg.expected,
                disposition: if previous.is_some() {
                    super::test_support::ResourceMutationDisposition::Replaced
                } else {
                    super::test_support::ResourceMutationDisposition::Inserted
                },
            }
        });
        true
    }
}

/// The actor can mint this only while its exact provisional mirror is still
/// current. A later replacement seals the shared endpoint before publishing.
#[derive(Debug, Clone, kameo::Reply)]
#[cfg(any(test, feature = "test-utils"))]
pub struct ConfirmedProvisionalMirror {
    jid: FullJid,
    entry: ConnectionEntry,
}

#[cfg(any(test, feature = "test-utils"))]
impl ConfirmedProvisionalMirror {
    pub(in crate::registry) fn matches(&self, candidate: &ActorProvisionalConnectionEntry) -> bool {
        candidate.jid() == &self.jid && candidate.matches_entry(&self.entry)
    }
}

#[cfg(any(test, feature = "test-utils"))]
pub struct ConfirmProvisionalConnection {
    pub jid: FullJid,
    pub candidate: ActorProvisionalConnectionEntry,
}

#[cfg(any(test, feature = "test-utils"))]
impl kameo::message::Message<ConfirmProvisionalConnection> for UserActor {
    type Reply = Option<ConfirmedProvisionalMirror>;

    async fn handle(
        &mut self,
        msg: ConfirmProvisionalConnection,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if msg.candidate.jid() != &msg.jid || msg.jid.to_bare() != self.bare_jid {
            return None;
        }
        self.connections.get(&msg.jid).and_then(|entry| {
            msg.candidate
                .matches_entry(entry)
                .then(|| ConfirmedProvisionalMirror {
                    jid: msg.jid.clone(),
                    entry: entry.clone(),
                })
        })
    }
}

/// Unregister a connection (resource) for this user.
///
/// `owner` is the resource's local binding identity: `Some` gates removal on the stored entry still being
/// that owner's (so a lagging unregister can't evict a replacement), `None`
/// removes unconditionally — matching the DashMap `unregister_if_owner` vs
/// plain `unregister` call the teardown site makes.
pub struct UnregisterConnection {
    pub jid: FullJid,
    pub owner: Option<LocalConnectionBindingId>,
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
    pub owner: Option<LocalConnectionBindingId>,
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

/// One atomic routing-policy snapshot from the actor that owns the account.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ResourceRoutingState {
    pub jid: FullJid,
    pub available: bool,
    pub priority: i8,
    pub carbons_enabled: bool,
}

pub struct GetRoutingResources;

impl kameo::message::Message<GetRoutingResources> for UserActor {
    type Reply = Vec<ResourceRoutingState>;

    async fn handle(
        &mut self,
        _msg: GetRoutingResources,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let mut resources: Vec<_> = self
            .connections
            .iter()
            .map(|(jid, entry)| ResourceRoutingState {
                jid: jid.clone(),
                available: entry.is_presence_available(),
                priority: entry.presence_priority(),
                carbons_enabled: entry.carbons_enabled.load(Ordering::Relaxed),
            })
            .collect();
        resources.sort_by(|a, b| a.jid.cmp(&b.jid));
        resources
    }
}

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
        #[cfg(any(test, feature = "test-utils"))]
        self.observe_resource_mutation(|| {
            super::test_support::ResourceMutationObservation::ConflictClose {
                resource_count: count,
            }
        });
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
