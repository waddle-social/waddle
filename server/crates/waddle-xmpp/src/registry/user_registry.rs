//! User Registry Actor.
//!
//! A Kameo actor that maps bare JIDs to per-user `UserActor` instances.
//! One `UserRegistryActor` exists for the entire server, replacing the
//! DashMap-based lookup portion of `ConnectionRegistry` for user-level
//! concerns (Phase 2 of the actor-model migration).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use futures::future::join_all;
use jid::{BareJid, FullJid};
use kameo::actor::{ActorRef, Spawn};
use kameo::error::SendError;
use kameo::message::Context;
use kameo::Actor;
use thiserror::Error;
use tracing::{debug, error, info, warn};

use super::connection_registry::{ConnectionEntry, ForceDetachOutcome, ForceDetachRequest};
use super::user_actor::delivery::GetConnectionEntry;
use super::user_actor::{
    GetResources, RegisterConnection, RegisterConnectionIfOwnerOrAbsent, ResourceCount,
    UnregisterConnectionAndReportEmpty, UserActor,
};
use crate::metrics;
use crate::ownership::{
    ClaimEpoch, ClaimError, ClaimStore, Entity, EntityType, InProcessClaimStore, NodeIdentity,
    SharedNodeIdentity, StalePredicate,
};

const CHILD_ACTOR_TIMEOUT: Duration = Duration::from_secs(2);

/// A locally-held UserActor ownership lease. The `owner` is captured at
/// acquisition time rather than read back from `SharedNodeIdentity` on
/// release, because self-fence rotates the shared identity while old local
/// actors may still be demoted or removed.
#[derive(Clone)]
struct UserClaimLease {
    owner: NodeIdentity,
    epoch: ClaimEpoch,
}

/// A locally-spawned user's actor ref plus the ownership lease this node holds
/// for that bare JID.
#[derive(Clone)]
struct UserEntry {
    actor_ref: ActorRef<UserActor>,
    claim: UserClaimLease,
    /// Actor-authoritative registration mirror used only for exact hard
    /// demotion. Keeping the cheap Arc-backed entries here avoids asking a
    /// potentially wedged child actor before it can be killed.
    resources: HashMap<FullJid, ConnectionEntry>,
}

enum UserEntryClaimStatus {
    Current,
    ProvenStale,
    ValidationUnavailable,
}

/// Server-wide registry that maps bare JIDs to their `UserActor`.
///
/// All mutations are serialised through the actor mailbox, so no
/// external synchronisation is required.
#[derive(Actor)]
pub struct UserRegistryActor {
    users: HashMap<BareJid, UserEntry>,
    poisoned_users: HashSet<BareJid>,
    claim_store: Arc<dyn ClaimStore>,
    node_identity: SharedNodeIdentity,
}

impl UserRegistryActor {
    /// Create an empty registry.
    pub fn new() -> Self {
        info!("Creating user registry actor");
        Self {
            users: HashMap::new(),
            poisoned_users: HashSet::new(),
            claim_store: Arc::new(InProcessClaimStore::new()),
            node_identity: SharedNodeIdentity::new(NodeIdentity::local()),
        }
    }

    #[tracing::instrument(
        name = "xmpp.user_registry.acquire_claim",
        skip_all,
        fields(otel.status_code = tracing::field::Empty)
    )]
    async fn acquire_user_claim(
        &self,
        bare_jid: &BareJid,
    ) -> Result<UserClaimLease, UserRegistryError> {
        let entity = Entity::new(EntityType::UserActor, bare_jid.to_string());
        let identity = self.node_identity.current();
        match self.claim_store.ensure_claimed(&entity, &identity).await {
            Ok(epoch) => Ok(UserClaimLease {
                owner: identity,
                epoch,
            }),
            Err(ClaimError::AlreadyClaimed) => {
                self.steal_from_dead_user_owner(&entity, bare_jid, &identity)
                    .await
            }
            Err(error) => {
                // This is a claim-store failure, not ordinary contention; the
                // explicit status survives the typed actor-error translation.
                crate::telemetry::mark_span_error();
                error!(
                    jid = %bare_jid,
                    %error,
                    "UserActor claim acquisition failed"
                );
                Err(UserRegistryError::ClaimUnavailable(bare_jid.clone()))
            }
        }
    }

    #[tracing::instrument(
        name = "xmpp.user_registry.steal_stale_claim",
        skip_all,
        fields(otel.status_code = tracing::field::Empty)
    )]
    async fn steal_from_dead_user_owner(
        &self,
        entity: &Entity,
        bare_jid: &BareJid,
        identity: &NodeIdentity,
    ) -> Result<UserClaimLease, UserRegistryError> {
        let snapshot = match self.claim_store.current_claim(entity).await {
            Ok(Some(snapshot)) => snapshot,
            Ok(None) => return Err(UserRegistryError::ClaimHeldByAnotherNode(bare_jid.clone())),
            Err(error) => {
                // A failed ownership lookup cannot safely be treated as proof
                // that the foreign claim disappeared.
                crate::telemetry::mark_span_error();
                error!(jid = %bare_jid, %error, "UserActor claim owner lookup failed");
                return Err(UserRegistryError::ClaimHeldByAnotherNode(bare_jid.clone()));
            }
        };
        if snapshot.owner_lease_fresh {
            debug!(
                jid = %bare_jid,
                owner = %snapshot.owner.node_id,
                "UserActor ownership claim is held by a live foreign node"
            );
            return Err(UserRegistryError::ClaimHeldByAnotherNode(bare_jid.clone()));
        }
        match self
            .claim_store
            .steal_stale(
                entity,
                snapshot.claim_epoch,
                StalePredicate::OwnerStale,
                identity,
            )
            .await
        {
            Ok(epoch) => {
                info!(
                    jid = %bare_jid,
                    previous_owner = %snapshot.owner.node_id,
                    "reclaimed UserActor ownership from a dead owner"
                );
                Ok(UserClaimLease {
                    owner: identity.clone(),
                    epoch,
                })
            }
            Err(error) => {
                debug!(
                    jid = %bare_jid,
                    %error,
                    "UserActor dead-owner steal lost the claim race"
                );
                Err(UserRegistryError::ClaimHeldByAnotherNode(bare_jid.clone()))
            }
        }
    }

    fn spawn_user_actor(
        &mut self,
        bare_jid: BareJid,
        claim: UserClaimLease,
    ) -> ActorRef<UserActor> {
        let actor = UserActor::new(bare_jid.clone());
        let actor_ref = UserActor::spawn(actor);
        self.users.insert(
            bare_jid,
            UserEntry {
                actor_ref: actor_ref.clone(),
                claim,
                resources: HashMap::new(),
            },
        );
        actor_ref
    }

    async fn acquire_and_publish_user(
        &mut self,
        bare_jid: BareJid,
    ) -> Result<ActorRef<UserActor>, UserRegistryError> {
        let claim = self.acquire_user_claim(&bare_jid).await?;
        let Some(_publication_guard) = self.node_identity.guard_if_current(&claim.owner).await
        else {
            self.release_user_claim(&bare_jid, &claim).await;
            return Err(UserRegistryError::ClaimUnavailable(bare_jid));
        };
        Ok(self.spawn_user_actor(bare_jid, claim))
    }

    #[tracing::instrument(
        name = "xmpp.user_registry.release_claim",
        skip_all,
        fields(otel.status_code = tracing::field::Empty)
    )]
    async fn release_user_claim(&self, bare_jid: &BareJid, claim: &UserClaimLease) {
        let entity = Entity::new(EntityType::UserActor, bare_jid.to_string());
        if let Err(error) = self
            .claim_store
            .release(&entity, &claim.owner, claim.epoch)
            .await
        {
            // Release is best-effort for actor cleanup, but a backend failure
            // leaves ownership behind and must remain visible in trace queries.
            crate::telemetry::mark_span_error();
            error!(
                jid = %bare_jid,
                owner = %claim.owner.node_id,
                epoch = claim.epoch.0,
                %error,
                "failed to release UserActor ownership claim"
            );
        }
    }

    #[tracing::instrument(
        name = "xmpp.user_registry.validate_claim",
        skip_all,
        fields(otel.status_code = tracing::field::Empty)
    )]
    async fn validate_existing_user_entry_claim(
        &self,
        bare_jid: &BareJid,
        entry: &UserEntry,
    ) -> UserEntryClaimStatus {
        let current_identity = self.node_identity.current();
        if entry.claim.owner != current_identity {
            warn!(
                jid = %bare_jid,
                claim_owner = %entry.claim.owner.node_id,
                current_owner = %current_identity.node_id,
                "existing UserActor claim identity is stale; demoting before reuse"
            );
            return UserEntryClaimStatus::ProvenStale;
        }
        let entity = Entity::new(EntityType::UserActor, bare_jid.to_string());
        match self
            .claim_store
            .fence(&entity, &entry.claim.owner, entry.claim.epoch)
            .await
        {
            Ok(true) => UserEntryClaimStatus::Current,
            Ok(false) => {
                warn!(
                    jid = %bare_jid,
                    epoch = entry.claim.epoch.0,
                    "existing UserActor no longer owns its fenced claim; demoting before reuse"
                );
                UserEntryClaimStatus::ProvenStale
            }
            Err(error) => {
                // Refusing reuse is the safe response, but the unavailable
                // fence proof is still an internal dependency failure.
                crate::telemetry::mark_span_error();
                error!(
                    jid = %bare_jid,
                    epoch = entry.claim.epoch.0,
                    %error,
                    "failed to validate existing UserActor claim; refusing reuse without mutating actor state"
                );
                UserEntryClaimStatus::ValidationUnavailable
            }
        }
    }

    #[tracing::instrument(
        name = "xmpp.user_registry.reuse_claim",
        skip_all,
        fields(otel.status_code = tracing::field::Empty)
    )]
    async fn existing_user_actor_for_current_claim(
        &mut self,
        bare_jid: &BareJid,
    ) -> Result<Option<ActorRef<UserActor>>, UserRegistryError> {
        let Some(entry) = self.users.get(bare_jid).cloned() else {
            return Ok(None);
        };
        if !entry.actor_ref.is_alive() {
            // A registry entry pointing at a dead actor is an internal state
            // loss even though the caller receives a typed recoverable error.
            crate::telemetry::mark_span_error();
            error!(jid = %bare_jid, "Detected dead UserActor; failing fast");
            return Err(self.mark_actor_state_lost(bare_jid).await);
        }
        match self
            .validate_existing_user_entry_claim(bare_jid, &entry)
            .await
        {
            UserEntryClaimStatus::Current => return Ok(Some(entry.actor_ref)),
            UserEntryClaimStatus::ValidationUnavailable => {
                return Err(UserRegistryError::ClaimUnavailable(bare_jid.clone()));
            }
            UserEntryClaimStatus::ProvenStale => {}
        }
        if !self
            .force_detach_stale_actor_resources(bare_jid, &entry.actor_ref)
            .await
        {
            return Err(UserRegistryError::StaleUserActorRetirementFailed(
                bare_jid.clone(),
            ));
        }
        self.users.remove(bare_jid);
        entry.actor_ref.kill();
        Ok(None)
    }

    #[tracing::instrument(
        name = "xmpp.user_registry.force_detach_stale",
        skip_all,
        fields(otel.status_code = tracing::field::Empty)
    )]
    async fn force_detach_stale_actor_resources(
        &self,
        bare_jid: &BareJid,
        actor_ref: &ActorRef<UserActor>,
    ) -> bool {
        let resources = match actor_ref
            .ask(GetResources)
            .mailbox_timeout(CHILD_ACTOR_TIMEOUT)
            .reply_timeout(CHILD_ACTOR_TIMEOUT)
            .await
        {
            Ok(resources) => resources,
            Err(error) => {
                // Refused claim reuse is an internal actor-coordination
                // failure, so mark the active operation before returning false.
                crate::telemetry::mark_span_error();
                error!(
                    jid = %bare_jid,
                    ?error,
                    "failed to enumerate stale UserActor resources; refusing claim reuse"
                );
                return false;
            }
        };
        let mut ack_receivers = Vec::new();
        for jid in resources {
            let entry = match actor_ref
                .ask(GetConnectionEntry { jid: jid.clone() })
                .mailbox_timeout(CHILD_ACTOR_TIMEOUT)
                .reply_timeout(CHILD_ACTOR_TIMEOUT)
                .await
            {
                Ok(Some(entry)) => entry,
                Ok(None) => continue,
                Err(error) => {
                    crate::telemetry::mark_span_error();
                    error!(
                        jid = %jid,
                        ?error,
                        "failed to read stale UserActor resource entry; refusing claim reuse"
                    );
                    return false;
                }
            };
            let (ack, ack_rx) = tokio::sync::oneshot::channel();
            let request = ForceDetachRequest {
                requester_bare_jid: bare_jid.clone(),
                ack,
            };
            if let Err(error) = entry.force_detach_sender().try_send(request) {
                crate::telemetry::mark_span_error();
                error!(
                    jid = %jid,
                    ?error,
                    "failed to queue stale UserActor resource force-detach; refusing claim reuse"
                );
                return false;
            }
            ack_receivers.push((jid, ack_rx));
        }
        let ack_waits = ack_receivers.into_iter().map(|(jid, ack_rx)| async move {
            (jid, tokio::time::timeout(CHILD_ACTOR_TIMEOUT, ack_rx).await)
        });
        for (jid, outcome) in join_all(ack_waits).await {
            match outcome {
                Ok(Ok(ForceDetachOutcome::Detached | ForceDetachOutcome::NotPersisted)) => {}
                Ok(Ok(ForceDetachOutcome::IdentityMismatch)) => {
                    crate::telemetry::mark_span_error();
                    error!(
                        jid = %jid,
                        requester = %bare_jid,
                        "stale UserActor resource force-detach identity mismatch; refusing claim reuse"
                    );
                    return false;
                }
                Ok(Err(_closed)) => {
                    crate::telemetry::mark_span_error();
                    error!(
                        jid = %jid,
                        "stale UserActor resource force-detach ack channel closed; refusing claim reuse"
                    );
                    return false;
                }
                Err(_elapsed) => {
                    crate::telemetry::mark_span_error();
                    error!(
                        jid = %jid,
                        timeout_ms = CHILD_ACTOR_TIMEOUT.as_millis() as u64,
                        "stale UserActor resource force-detach timed out; refusing claim reuse"
                    );
                    return false;
                }
            }
        }
        true
    }

    async fn mark_actor_state_lost(&mut self, bare_jid: &BareJid) -> UserRegistryError {
        if let Some(entry) = self.users.remove(bare_jid) {
            self.release_user_claim(bare_jid, &entry.claim).await;
        }
        self.poisoned_users.insert(bare_jid.clone());
        metrics::record_actor_restart("user_actor", "detected_dead_actor_fail_fast");
        UserRegistryError::UserActorStateLost(bare_jid.clone())
    }
}

impl Default for UserRegistryActor {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum UserRegistryError {
    #[error("user actor state for {0} was lost; explicit cleanup required before recreating")]
    UserActorStateLost(BareJid),
    #[error("user actor for {0} is temporarily overloaded")]
    UserActorBusy(BareJid),
    #[error("user actor {0}'s ownership claim is held by another node")]
    ClaimHeldByAnotherNode(BareJid),
    #[error("user actor {0}'s ownership claim is unavailable")]
    ClaimUnavailable(BareJid),
    #[error("stale user actor {0} could not be retired before claim reuse")]
    StaleUserActorRetirementFailed(BareJid),
}

/// Wire the real, clustering-backed claim store/identity into an
/// already-spawned user registry (ADR-0017 Phase 4 Slice 1b).
///
/// Construction-order note: the user registry is spawned while constructing
/// `WebSocketState`, after clustering startup has produced its shared claim
/// pair on `AppState`. A `None` pair leaves this registry on its default
/// single-node [`InProcessClaimStore`] plus [`NodeIdentity::local`].
pub struct WireUserClusteringClaims {
    pub claim_store: Arc<dyn ClaimStore>,
    pub node_identity: SharedNodeIdentity,
}

impl kameo::message::Message<WireUserClusteringClaims> for UserRegistryActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: WireUserClusteringClaims,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.claim_store = msg.claim_store;
        self.node_identity = msg.node_identity;
    }
}

/// Return the `UserActor` for the given bare JID, spawning one if it does not
/// already exist.
pub struct GetOrCreateUser {
    pub bare_jid: BareJid,
}

impl kameo::message::Message<GetOrCreateUser> for UserRegistryActor {
    type Reply = Result<ActorRef<UserActor>, UserRegistryError>;

    async fn handle(
        &mut self,
        msg: GetOrCreateUser,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if self.poisoned_users.contains(&msg.bare_jid) {
            return Err(UserRegistryError::UserActorStateLost(msg.bare_jid));
        }

        if let Some(actor_ref) = self
            .existing_user_actor_for_current_claim(&msg.bare_jid)
            .await?
        {
            debug!(jid = %msg.bare_jid, "Returning existing UserActor");
            return Ok(actor_ref);
        }

        debug!(jid = %msg.bare_jid, "Spawning new UserActor");
        let actor_ref = self.acquire_and_publish_user(msg.bare_jid).await?;
        Ok(actor_ref)
    }
}

/// Look up an existing `UserActor` without creating one.
pub struct GetUser {
    pub bare_jid: BareJid,
}

impl kameo::message::Message<GetUser> for UserRegistryActor {
    type Reply = Result<Option<ActorRef<UserActor>>, UserRegistryError>;

    async fn handle(&mut self, msg: GetUser, _ctx: &mut Context<Self, Self::Reply>) -> Self::Reply {
        if self.poisoned_users.contains(&msg.bare_jid) {
            return Err(UserRegistryError::UserActorStateLost(msg.bare_jid));
        }
        self.existing_user_actor_for_current_claim(&msg.bare_jid)
            .await
    }
}

/// Look up a local `UserActor` for local-claim health/demotion logic without
/// triggering the dead-actor state-lost path. A caller that is already in the
/// self-fence/deposed-owner path must not release the durable claim as a side
/// effect of observing a dead actor; demotion owns the fenced cleanup.
pub struct GetUserForLocalClaim {
    pub bare_jid: BareJid,
}

impl kameo::message::Message<GetUserForLocalClaim> for UserRegistryActor {
    type Reply = Option<ActorRef<UserActor>>;

    async fn handle(
        &mut self,
        msg: GetUserForLocalClaim,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.users.get(&msg.bare_jid).and_then(|entry| {
            if entry.actor_ref.is_alive() {
                Some(entry.actor_ref.clone())
            } else {
                None
            }
        })
    }
}

/// Register a user resource through the registry actor, serializing user lifecycle mutations.
///
/// Carries the live [`ConnectionEntry`] — the SAME `Arc`-backed struct held in
/// the DashMap `ConnectionRegistry` — so the spawned `UserActor` shares its
/// sender AND its presence/carbons atomics. Because every field is `Arc`- or
/// channel-backed, a later `update_presence` / `set_carbons_enabled` on the
/// DashMap entry is automatically visible through the actor's clone; no
/// per-site presence mirroring is required (ADR-0017 Phase 1).
pub struct RegisterUserResource {
    pub jid: FullJid,
    pub entry: ConnectionEntry,
}

impl kameo::message::Message<RegisterUserResource> for UserRegistryActor {
    type Reply = Result<(), UserRegistryError>;

    async fn handle(
        &mut self,
        msg: RegisterUserResource,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let bare_jid = msg.jid.to_bare();
        let mirrored_entry = msg.entry.clone();
        if self.poisoned_users.contains(&bare_jid) {
            return Err(UserRegistryError::UserActorStateLost(bare_jid));
        }

        let user_actor = if let Some(actor_ref) = self
            .existing_user_actor_for_current_claim(&bare_jid)
            .await?
        {
            actor_ref
        } else {
            self.acquire_and_publish_user(bare_jid.clone()).await?
        };

        match user_actor
            .ask(RegisterConnection {
                jid: msg.jid.clone(),
                entry: msg.entry,
            })
            .mailbox_timeout(CHILD_ACTOR_TIMEOUT)
            .reply_timeout(CHILD_ACTOR_TIMEOUT)
            .await
        {
            Ok(()) => {
                if let Some(user) = self.users.get_mut(&bare_jid) {
                    user.resources.insert(msg.jid.clone(), mirrored_entry);
                }
            }
            Err(SendError::MailboxFull(_) | SendError::Timeout(_)) => {
                return Err(UserRegistryError::UserActorBusy(bare_jid));
            }
            Err(_) => return Err(self.mark_actor_state_lost(&bare_jid).await),
        }

        Ok(())
    }
}

/// Register a user resource without replacing a different owner token.
///
/// This keeps clustered remote-resource mirrors from deleting a live local
/// same-full-JID resource that won the slot while the remote register was in
/// flight. It otherwise follows [`RegisterUserResource`] so user lifecycle and
/// claim acquisition remain serialized by this actor.
pub struct RegisterUserResourceIfOwnerOrAbsent {
    pub jid: FullJid,
    pub entry: ConnectionEntry,
    pub owner: Arc<AtomicBool>,
}

impl kameo::message::Message<RegisterUserResourceIfOwnerOrAbsent> for UserRegistryActor {
    type Reply = Result<bool, UserRegistryError>;

    async fn handle(
        &mut self,
        msg: RegisterUserResourceIfOwnerOrAbsent,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let bare_jid = msg.jid.to_bare();
        let mirrored_entry = msg.entry.clone();
        if self.poisoned_users.contains(&bare_jid) {
            return Err(UserRegistryError::UserActorStateLost(bare_jid));
        }

        let user_actor = if let Some(actor_ref) = self
            .existing_user_actor_for_current_claim(&bare_jid)
            .await?
        {
            actor_ref
        } else {
            self.acquire_and_publish_user(bare_jid.clone()).await?
        };

        match user_actor
            .ask(RegisterConnectionIfOwnerOrAbsent {
                jid: msg.jid.clone(),
                entry: msg.entry,
                owner: msg.owner,
            })
            .mailbox_timeout(CHILD_ACTOR_TIMEOUT)
            .reply_timeout(CHILD_ACTOR_TIMEOUT)
            .await
        {
            Ok(registered) => {
                if registered {
                    if let Some(user) = self.users.get_mut(&bare_jid) {
                        user.resources.insert(msg.jid.clone(), mirrored_entry);
                    }
                }
                Ok(registered)
            }
            Err(SendError::MailboxFull(_) | SendError::Timeout(_)) => {
                Err(UserRegistryError::UserActorBusy(bare_jid))
            }
            Err(_) => Err(self.mark_actor_state_lost(&bare_jid).await),
        }
    }
}

/// Unregister a user resource atomically in the actor-owned path and prune empty users.
///
/// `owner` is the ownership token forwarded to the `UserActor` so the removal
/// is ownership-gated (`UnregisterConnection` semantics); `None` removes
/// unconditionally, matching a plain DashMap `unregister`.
pub struct UnregisterUserResource {
    pub jid: FullJid,
    pub owner: Option<Arc<AtomicBool>>,
}

impl kameo::message::Message<UnregisterUserResource> for UserRegistryActor {
    type Reply = Result<(), UserRegistryError>;

    async fn handle(
        &mut self,
        msg: UnregisterUserResource,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let bare_jid = msg.jid.to_bare();
        let resource_jid = msg.jid.clone();
        if self.poisoned_users.contains(&bare_jid) {
            return Err(UserRegistryError::UserActorStateLost(bare_jid));
        }

        let Some(entry) = self.users.get(&bare_jid).cloned() else {
            return Ok(());
        };
        if !entry.actor_ref.is_alive() {
            return Err(self.mark_actor_state_lost(&bare_jid).await);
        }

        let unregister = match entry
            .actor_ref
            .ask(UnregisterConnectionAndReportEmpty {
                jid: msg.jid,
                owner: msg.owner,
            })
            .mailbox_timeout(CHILD_ACTOR_TIMEOUT)
            .reply_timeout(CHILD_ACTOR_TIMEOUT)
            .await
        {
            Ok(outcome) => outcome,
            Err(SendError::MailboxFull(_) | SendError::Timeout(_)) => {
                return Err(UserRegistryError::UserActorBusy(bare_jid));
            }
            Err(_) => return Err(self.mark_actor_state_lost(&bare_jid).await),
        };

        if unregister.removed {
            if let Some(user) = self.users.get_mut(&bare_jid) {
                user.resources.remove(&resource_jid);
            }
        }

        if unregister.is_empty {
            if let Some(entry) = self.users.remove(&bare_jid) {
                self.release_user_claim(&bare_jid, &entry.claim).await;
            }
            self.poisoned_users.remove(&bare_jid);
        }

        Ok(())
    }
}

/// Remove a user from the registry.
///
/// Returns `true` if the user was present and removed.
pub struct RemoveUser {
    pub bare_jid: BareJid,
}

impl kameo::message::Message<RemoveUser> for UserRegistryActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: RemoveUser,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let removed_entry = self.users.remove(&msg.bare_jid);
        let removed = removed_entry.is_some();
        let cleared_poison = self.poisoned_users.remove(&msg.bare_jid);
        if let Some(entry) = removed_entry {
            self.release_user_claim(&msg.bare_jid, &entry.claim).await;
        }
        if removed {
            debug!(jid = %msg.bare_jid, "Removed user from registry");
        }
        removed || cleared_poison
    }
}

/// Forget and hard-kill a locally held user actor after this node has been
/// demoted as owner. The Postgres claim is deliberately not released here:
/// demotion means the claim has already moved or this node's old identity is
/// no longer valid. Releasing would race the new owner; forgetting locally is
/// the safe fenced outcome.
pub struct DemoteUserActor {
    pub bare_jid: BareJid,
}

impl kameo::message::Message<DemoteUserActor> for UserRegistryActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: DemoteUserActor,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let Some(entry) = self.users.remove(&msg.bare_jid) else {
            return false;
        };
        entry.actor_ref.kill();
        debug!(jid = %msg.bare_jid, "Demoted local UserActor");
        true
    }
}

/// One exact resource removed with an owner-conditional UserActor demotion.
pub struct DemotedUserResource {
    pub jid: FullJid,
    pub entry: ConnectionEntry,
}

/// Resources that belonged to the exact UserActor entry that was demoted.
#[derive(kameo::Reply)]
pub struct DemotedUserActor {
    pub resources: Vec<DemotedUserResource>,
}

/// Demote a UserActor only if its immutable claim lease still belongs to
/// `owner`. Resource capture, owner comparison, removal, and kill share one
/// registry mailbox turn, preventing a stale sweep from targeting an ABA
/// same-JID replacement.
pub struct DemoteUserActorIfOwner {
    pub bare_jid: BareJid,
    pub owner: NodeIdentity,
}

impl kameo::message::Message<DemoteUserActorIfOwner> for UserRegistryActor {
    type Reply = Option<DemotedUserActor>;

    async fn handle(
        &mut self,
        msg: DemoteUserActorIfOwner,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let entry = self.users.get(&msg.bare_jid)?.clone();
        if entry.claim.owner != msg.owner {
            return None;
        }
        let removed = self.users.remove(&msg.bare_jid)?;
        removed.actor_ref.kill();
        Some(DemotedUserActor {
            resources: removed
                .resources
                .into_iter()
                .map(|(jid, entry)| DemotedUserResource { jid, entry })
                .collect(),
        })
    }
}

/// List all bare JIDs that currently have a `UserActor`.
pub struct ListUsers;

impl kameo::message::Message<ListUsers> for UserRegistryActor {
    type Reply = Vec<BareJid>;

    async fn handle(
        &mut self,
        _msg: ListUsers,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.users.keys().cloned().collect()
    }
}

/// List local UserActors whose immutable claim lease belongs to `owner`.
pub struct ListUsersOwnedBy {
    pub owner: NodeIdentity,
}

impl kameo::message::Message<ListUsersOwnedBy> for UserRegistryActor {
    type Reply = Vec<BareJid>;

    async fn handle(
        &mut self,
        msg: ListUsersOwnedBy,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.users
            .iter()
            .filter(|(_, entry)| entry.claim.owner == msg.owner)
            .map(|(jid, _)| jid.clone())
            .collect()
    }
}

/// Reap a user's `UserActor` iff it currently has zero connected resources.
///
/// Closes the empty-actor accumulation gap the ADR-0017 Phase 1 Slice 2
/// delivery cutover opens (Copilot review on PR #1177): production delivery now
/// runs through the actor's `TrySend*`, whose `try_deliver` evicts a
/// closed-channel resource. When that eviction removes a `UserActor`'s *last*
/// resource, the explicit `UnregisterConnectionAndReportEmpty` prune path does
/// not run (e.g. the teardown's best-effort `mirror_unregister` was dropped on
/// a mailbox timeout), so the now-empty actor would otherwise linger in `users`
/// forever. A periodic reaper (see `spawn_user_actor_reaper`) drives this
/// message per listed user.
///
/// Correctness (the race the ADR warns against): the `ResourceCount == 0` read
/// and the `users` removal happen in this one registry handler with no yield to
/// *other registry* messages between them — kameo does not dequeue the next
/// message while a handler awaits a child ask — so a concurrent
/// `RegisterUserResource` cannot insert a resource between the count read and
/// the removal. That is why the reaper is a single atomic registry message
/// rather than a non-atomic `IsEmpty`-then-`RemoveUser` pair (which would race
/// an in-flight re-registration and could evict a live resource), and why the
/// `UserActor` does NOT self-prune on empty.
///
/// Returns `true` only when an empty actor was removed.
pub struct ReapUserIfEmpty {
    pub bare_jid: BareJid,
}

impl kameo::message::Message<ReapUserIfEmpty> for UserRegistryActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: ReapUserIfEmpty,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        // A poisoned entry's lifecycle is owned by the state-lost path; never
        // race it here.
        if self.poisoned_users.contains(&msg.bare_jid) {
            return false;
        }
        let Some(actor_ref) = self.users.get(&msg.bare_jid) else {
            return false;
        };
        if !actor_ref.actor_ref.is_alive() {
            // A dead actor is a state-lost condition, not an empty one; fold it
            // into the poison path so that set stays the single source of
            // dead-actor truth rather than silently dropping it here.
            self.mark_actor_state_lost(&msg.bare_jid).await;
            return false;
        }
        let actor_ref = actor_ref.actor_ref.clone();
        let count = match actor_ref
            .ask(ResourceCount)
            .mailbox_timeout(CHILD_ACTOR_TIMEOUT)
            .reply_timeout(CHILD_ACTOR_TIMEOUT)
            .await
        {
            Ok(count) => count,
            // Busy / unreachable — leave it for the next sweep rather than
            // removing an actor whose state we could not read.
            Err(_) => return false,
        };
        if count == 0 {
            if let Some(entry) = self.users.remove(&msg.bare_jid) {
                self.release_user_claim(&msg.bare_jid, &entry.claim).await;
            }
            debug!(jid = %msg.bare_jid, "Reaped empty UserActor");
            true
        } else {
            false
        }
    }
}

// ADR-0017 Phase 1: the registry-level routing convenience messages
// (SelectRoutableResourcesForUser / ResourcesForUser / TrySendPeerToUser) that
// wired bare-JID selection and MUC fan-out through the actor tree were removed.
// Both cutovers proved unsound over a best-effort async mirror — set-selection
// can't be verified complete (partial-mirror miss), and a timed-out fan-out
// ask can still deliver while the DashMap fallback delivers a duplicate on the
// same channel. Delivery/selection cutover waits for actor-authoritative
// registration in Phase 1 completion, where the actor is the sole source and
// no DashMap fallback is needed. The per-resource delivery surface on
// `UserActor` (SelectRoutableResources, TrySendDirect/Peer/PendingFlush) stays
// — it is the tested foundation those cutovers will build on.

/// Return the number of tracked users.
pub struct UserCount;

impl kameo::message::Message<UserCount> for UserRegistryActor {
    type Reply = usize;

    async fn handle(
        &mut self,
        _msg: UserCount,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.users.len()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
