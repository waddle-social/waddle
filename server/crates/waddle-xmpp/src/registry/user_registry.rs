//! User Registry Actor.
//!
//! A Kameo actor that maps bare JIDs to per-user `UserActor` instances.
//! One `UserRegistryActor` exists for the entire server, replacing the
//! DashMap-based lookup portion of `ConnectionRegistry` for user-level
//! concerns (Phase 2 of the actor-model migration).

use std::collections::{HashMap, HashSet, VecDeque};
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

use super::connection_registry::{
    ConnectionEntry, ForceDetachOrigin, ForceDetachOutcome, ForceDetachRequest,
};
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

/// Bound for one child-actor operation issued from a registry handler.
///
/// Server-side callers whose reply waits encompass registry handlers derive
/// their budgets from this value, because registration may first drain one
/// pending child unregister and then issue its own child register ask.
pub const CHILD_ACTOR_TIMEOUT: Duration = Duration::from_secs(2);

/// Bound convergence retry work to keep one registry mailbox turn within the
/// caller budgets that drive janitor sweeps. Each pending-unregister retry can
/// spend a full [`CHILD_ACTOR_TIMEOUT`] on a wedged child actor, so processing
/// more than two in one turn would exceed the server reaper's 5-second ask
/// timeout before any other registry work can run.
const CONVERGENCE_RETRY_BATCH_LIMIT: usize = 2;
/// Durable claim releases talk to the clustered claim store, so they need a
/// much shorter cutoff than the child-actor timeout. Two pending-unregister
/// retries plus two timed-out releases still fit within the janitor's
/// 5-second ask budget.
const CLAIM_RELEASE_TIMEOUT: Duration = Duration::from_millis(250);

/// Upper bound on waiting for stale-retirement force-detach
/// acknowledgements before the retirement finalizes anyway. Long enough for
/// a socket mid-stanza to finish its cleanup handshake; short enough that a
/// wedged socket cannot pin the stale entry (and every registration retry
/// behind it) indefinitely.
const STALE_RETIREMENT_ACK_TIMEOUT: Duration = Duration::from_secs(4);
const STALE_RETIREMENT_RETRY_DELAY: Duration = Duration::from_millis(50);
const STALE_RETIREMENT_MAX_QUEUE_RETRIES: u8 = 3;
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

/// An exact durable ownership fence whose release failed after its local
/// actor was retired.  Keeping the original owner and epoch is essential:
/// releasing a freshly reacquired claim would be unsafe.
#[derive(Clone)]
struct TerminalUserClaimRelease {
    bare_jid: BareJid,
    claim: UserClaimLease,
}

/// One stale-retirement retry unit. Keeping the handler input typed avoids
/// open-coded tuple plumbing as the queue is selected and processed.
#[derive(Clone)]
struct PendingStaleUserRetirement {
    bare_jid: BareJid,
}

/// A resource removal that could not enter the child actor before its owner
/// disconnected.  The registry retries this exact owner-gated removal from
/// the janitor rather than leaving the full-JID slot occupied forever.
#[derive(Clone)]
struct PendingUserUnregister {
    jid: FullJid,
    owner: Option<Arc<AtomicBool>>,
}

enum UserEntryClaimStatus {
    Current,
    ProvenStale,
    ValidationUnavailable,
}

enum StaleRetirementQueueResult {
    Queued,
    Saturated,
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
    terminal_claim_releases: Vec<TerminalUserClaimRelease>,
    pending_unregisters: Vec<PendingUserUnregister>,
    pending_stale_retirements: HashSet<BareJid>,
    prioritized_stale_retirements: VecDeque<BareJid>,
    stale_retirement_queue_retries: HashMap<BareJid, u8>,
    stale_retirement_retry_scheduled: bool,
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
            terminal_claim_releases: Vec::new(),
            pending_unregisters: Vec::new(),
            pending_stale_retirements: HashSet::new(),
            prioritized_stale_retirements: VecDeque::new(),
            stale_retirement_queue_retries: HashMap::new(),
            stale_retirement_retry_scheduled: false,
        }
    }

    fn schedule_pending_stale_retirement_retry(&mut self, actor_ref: &ActorRef<Self>) {
        if self.stale_retirement_retry_scheduled || self.pending_stale_retirements.is_empty() {
            return;
        }
        self.stale_retirement_retry_scheduled = true;
        std::mem::drop(
            actor_ref
                .tell(RetryPendingStaleUserRetirements)
                .send_after(STALE_RETIREMENT_RETRY_DELAY),
        );
    }

    fn kick_stale_retirement_if_pending(&mut self, bare_jid: &BareJid, actor_ref: &ActorRef<Self>) {
        if self.pending_stale_retirements.contains(bare_jid) {
            self.prioritize_pending_stale_retirement(bare_jid);
            self.schedule_pending_stale_retirement_retry(actor_ref);
        }
    }

    fn prioritize_pending_stale_retirement(&mut self, bare_jid: &BareJid) {
        if self.pending_stale_retirements.contains(bare_jid)
            && !self.prioritized_stale_retirements.contains(bare_jid)
        {
            self.prioritized_stale_retirements
                .push_back(bare_jid.clone());
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
        if !self
            .converge_terminal_claim_releases_before_acquire(&bare_jid)
            .await
        {
            return Err(UserRegistryError::ClaimUnavailable(bare_jid));
        }
        let claim = self.acquire_user_claim(&bare_jid).await?;
        let Some(_publication_guard) = self.node_identity.guard_if_current(&claim.owner).await
        else {
            let _ = self.release_user_claim(&bare_jid, &claim).await;
            return Err(UserRegistryError::ClaimUnavailable(bare_jid));
        };
        Ok(self.spawn_user_actor(bare_jid, claim))
    }

    /// A timed-out release can still commit after its future is dropped.
    /// Demand-side self-reacquisition must therefore converge every pending
    /// exact fence for this bare JID before publishing a new actor.
    async fn converge_terminal_claim_releases_before_acquire(
        &mut self,
        bare_jid: &BareJid,
    ) -> bool {
        let pending = std::mem::take(&mut self.terminal_claim_releases);
        let (matching, retained): (Vec<_>, Vec<_>) = pending
            .into_iter()
            .partition(|release| release.bare_jid == *bare_jid);
        self.terminal_claim_releases = retained;

        let mut matching = VecDeque::from(matching);
        while let Some(release) = matching.pop_front() {
            if !self
                .release_user_claim(&release.bare_jid, &release.claim)
                .await
            {
                self.terminal_claim_releases.extend(matching);
                return false;
            }
        }
        true
    }

    fn cancel_terminal_claim_release(&mut self, bare_jid: &BareJid) {
        self.terminal_claim_releases
            .retain(|release| release.bare_jid != *bare_jid);
    }

    fn remember_terminal_claim_release(&mut self, bare_jid: &BareJid, claim: &UserClaimLease) {
        if !self.terminal_claim_releases.iter().any(|release| {
            release.bare_jid == *bare_jid
                && release.claim.owner == claim.owner
                && release.claim.epoch == claim.epoch
        }) {
            self.terminal_claim_releases.push(TerminalUserClaimRelease {
                bare_jid: bare_jid.clone(),
                claim: claim.clone(),
            });
        }
    }

    fn remember_pending_unregister(&mut self, jid: FullJid, owner: Option<Arc<AtomicBool>>) {
        // Owner tokens identify an exact resource incarnation.  A timeout can
        // record the same conditional removal both in the ask handler and in
        // its caller's fallback, so collapse records only when those pointer
        // identities match as well.
        if self.pending_unregisters.iter().any(|pending| {
            pending.jid == jid
                && match (&pending.owner, &owner) {
                    (None, None) => true,
                    (Some(pending_owner), Some(owner)) => Arc::ptr_eq(pending_owner, owner),
                    _ => false,
                }
        }) {
            return;
        }
        self.pending_unregisters
            .push(PendingUserUnregister { jid, owner });
    }

    /// Retry the one pending removal that can affect the current resource
    /// before registering its replacement.
    ///
    /// A registry handler must not serially await an unbounded number of old
    /// owner tokens: an owner-gated record that no longer matches the current
    /// entry cannot remove a later replacement, so the janitor can converge it
    /// independently. A single unowned record is the exception because it can
    /// remove any entry; it takes priority and is deduplicated by
    /// [`Self::remember_pending_unregister`]. Otherwise, at most one
    /// owner-gated record can match the one current full-JID entry. This keeps
    /// registration bounded to one drain ask plus one register ask.
    async fn drain_pending_unregisters_for(&mut self, jid: &FullJid) {
        let current_owner = self
            .users
            .get(&jid.to_bare())
            .and_then(|user| user.resources.get(jid))
            .map(ConnectionEntry::carbons_handle);
        let pending = std::mem::take(&mut self.pending_unregisters);
        let mut matching_owner_pending = None;
        let mut unowned_pending = None;
        for pending_unregister in pending {
            if pending_unregister.jid != *jid {
                self.pending_unregisters.push(pending_unregister);
                continue;
            }

            if pending_unregister.owner.is_none() && unowned_pending.is_none() {
                unowned_pending = Some(pending_unregister);
            } else if matching_owner_pending.is_none()
                && current_owner.as_ref().is_some_and(|current_owner| {
                    pending_unregister
                        .owner
                        .as_ref()
                        .is_some_and(|owner| Arc::ptr_eq(owner, current_owner))
                })
            {
                matching_owner_pending = Some(pending_unregister);
            } else {
                self.pending_unregisters.push(pending_unregister);
            }
        }

        // An unowned removal can evict any later replacement, so process it
        // first regardless of insertion order. Its owner-gated predecessor is
        // safe to leave for janitor convergence after the unowned removal.
        let pending_unregister = if let Some(unowned_pending) = unowned_pending {
            if let Some(owner_pending) = matching_owner_pending {
                self.pending_unregisters.push(owner_pending);
            }
            unowned_pending
        } else if let Some(matching_owner_pending) = matching_owner_pending {
            matching_owner_pending
        } else {
            return;
        };
        let retry_jid = pending_unregister.jid.clone();
        let retry_owner = pending_unregister.owner.clone();
        if matches!(
            self.unregister_and_release_if_empty(pending_unregister.jid, pending_unregister.owner)
                .await,
            UnregisterAndReleaseOutcome::RetryableFailure(_)
        ) {
            self.remember_pending_unregister(retry_jid, retry_owner);
        }
    }

    #[tracing::instrument(
        name = "xmpp.user_registry.release_claim",
        skip_all,
        fields(otel.status_code = tracing::field::Empty)
    )]
    async fn release_user_claim(&mut self, bare_jid: &BareJid, claim: &UserClaimLease) -> bool {
        let entity = Entity::new(EntityType::UserActor, bare_jid.to_string());
        match tokio::time::timeout(
            CLAIM_RELEASE_TIMEOUT,
            self.claim_store.release(&entity, &claim.owner, claim.epoch),
        )
        .await
        {
            Ok(Ok(())) => true,
            Ok(Err(error)) => {
                // Release is best-effort for actor cleanup, but a backend
                // failure leaves ownership behind and must remain visible in
                // trace queries.
                crate::telemetry::mark_span_error();
                error!(
                    jid = %bare_jid,
                    owner = %claim.owner.node_id,
                    epoch = claim.epoch.0,
                    %error,
                    "failed to release UserActor ownership claim"
                );
                self.remember_terminal_claim_release(bare_jid, claim);
                false
            }
            Err(_elapsed) => {
                crate::telemetry::mark_span_error();
                warn!(
                    jid = %bare_jid,
                    owner = %claim.owner.node_id,
                    epoch = claim.epoch.0,
                    timeout_ms = CLAIM_RELEASE_TIMEOUT.as_millis() as u64,
                    "timed out releasing UserActor ownership claim; deferring retry"
                );
                self.remember_terminal_claim_release(bare_jid, claim);
                false
            }
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
        // Strict bound: this await runs inside the single global registry
        // actor turn, and a saturated control-plane pool can hold a bare
        // `fence()` for ~SQLx's 30 s acquisition timeout — long enough for
        // every unrelated bind to miss its mailbox window. A timed-out check
        // reports ValidationUnavailable and retries on a later sweep.
        let fence = match tokio::time::timeout(
            CLAIM_RELEASE_TIMEOUT,
            self.claim_store
                .fence(&entity, &entry.claim.owner, entry.claim.epoch),
        )
        .await
        {
            Ok(result) => result,
            Err(_elapsed) => {
                warn!(
                    jid = %bare_jid,
                    "stale-retirement claim fence timed out; deferring validation to a \
                     later sweep instead of holding the registry turn"
                );
                return UserEntryClaimStatus::ValidationUnavailable;
            }
        };
        match fence {
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

    async fn existing_user_actor_for_register(
        &mut self,
        bare_jid: &BareJid,
        actor_ref: &ActorRef<Self>,
    ) -> Result<Option<ActorRef<UserActor>>, UserRegistryError> {
        let Some(entry) = self.users.get(bare_jid).cloned() else {
            self.clear_pending_stale_retirement(bare_jid);
            return Ok(None);
        };
        if !entry.actor_ref.is_alive() {
            crate::telemetry::mark_span_error();
            error!(jid = %bare_jid, "Detected dead UserActor; failing fast");
            return Err(self.mark_actor_state_lost(bare_jid).await);
        }
        if self.pending_stale_retirements.contains(bare_jid) {
            self.schedule_pending_stale_retirement_retry(actor_ref);
            return Err(UserRegistryError::UserActorBusy(bare_jid.clone()));
        }
        match self
            .validate_existing_user_entry_claim(bare_jid, &entry)
            .await
        {
            UserEntryClaimStatus::Current => Ok(Some(entry.actor_ref)),
            UserEntryClaimStatus::ValidationUnavailable => {
                Err(UserRegistryError::ClaimUnavailable(bare_jid.clone()))
            }
            UserEntryClaimStatus::ProvenStale => {
                self.pending_stale_retirements.insert(bare_jid.clone());
                self.schedule_pending_stale_retirement_retry(actor_ref);
                Err(UserRegistryError::UserActorBusy(bare_jid.clone()))
            }
        }
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
                origin: ForceDetachOrigin::RegistryStaleActorRetirement,
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
            let _ = self.release_user_claim(bare_jid, &entry.claim).await;
        }
        self.poisoned_users.insert(bare_jid.clone());
        metrics::record_actor_restart("user_actor", "detected_dead_actor_fail_fast");
        UserRegistryError::UserActorStateLost(bare_jid.clone())
    }

    /// Remove one resource and retire an empty user actor.  The whole
    /// operation is serialized by this registry actor, so a successful
    /// `Released` result is a real liveness fence rather than a best-effort
    /// mirror acknowledgement.
    async fn unregister_and_release_if_empty(
        &mut self,
        jid: FullJid,
        owner: Option<Arc<AtomicBool>>,
    ) -> UnregisterAndReleaseOutcome {
        let bare_jid = jid.to_bare();
        if self.poisoned_users.contains(&bare_jid) {
            if !self.users.contains_key(&bare_jid) {
                return UnregisterAndReleaseOutcome::AlreadyAbsent;
            }
            return UnregisterAndReleaseOutcome::RetryableFailure(
                UnregisterAndReleaseRetryableFailure::UserActorStateLost,
            );
        }
        let Some(entry) = self.users.get(&bare_jid).cloned() else {
            return UnregisterAndReleaseOutcome::AlreadyAbsent;
        };
        if !entry.actor_ref.is_alive() {
            let _ = self.mark_actor_state_lost(&bare_jid).await;
            return UnregisterAndReleaseOutcome::AlreadyAbsent;
        }
        let unregister = match entry
            .actor_ref
            .ask(UnregisterConnectionAndReportEmpty {
                jid: jid.clone(),
                owner,
            })
            .mailbox_timeout(CHILD_ACTOR_TIMEOUT)
            .reply_timeout(CHILD_ACTOR_TIMEOUT)
            .await
        {
            Ok(outcome) => outcome,
            Err(SendError::MailboxFull(_) | SendError::Timeout(_)) => {
                return UnregisterAndReleaseOutcome::RetryableFailure(
                    UnregisterAndReleaseRetryableFailure::UserActorBusy,
                );
            }
            Err(_) => {
                let _ = self.mark_actor_state_lost(&bare_jid).await;
                return UnregisterAndReleaseOutcome::AlreadyAbsent;
            }
        };

        match unregister {
            super::user_actor::UnregisterConnectionOutcome::Removed { is_empty } => {
                if let Some(user) = self.users.get_mut(&bare_jid) {
                    user.resources.remove(&jid);
                }
                if !is_empty {
                    return UnregisterAndReleaseOutcome::RetainedLiveResources;
                }
            }
            super::user_actor::UnregisterConnectionOutcome::AlreadyAbsent { is_empty } => {
                if let Some(user) = self.users.get_mut(&bare_jid) {
                    user.resources.remove(&jid);
                }
                if !is_empty {
                    return UnregisterAndReleaseOutcome::RetainedLiveResources;
                }
            }
            super::user_actor::UnregisterConnectionOutcome::RetainedTargetPresent => {
                return UnregisterAndReleaseOutcome::RetainedLiveResources;
            }
        }
        if let Some(entry) = self.users.remove(&bare_jid) {
            if entry.claim.owner == self.node_identity.current() {
                let _ = self.release_user_claim(&bare_jid, &entry.claim).await;
            } else {
                // Stale-actor retirement runs after a local identity rotation
                // or a lost fence. The old lease may already belong to a
                // different live owner, so this janitor convergence must not
                // release it while pruning our local mirror.
                debug!(jid = %bare_jid, "retired stale UserActor without releasing an old claim");
            }
        }
        self.poisoned_users.remove(&bare_jid);
        self.clear_pending_stale_retirement(&bare_jid);
        UnregisterAndReleaseOutcome::Released
    }

    fn clear_pending_stale_retirement(&mut self, bare_jid: &BareJid) {
        self.pending_stale_retirements.remove(bare_jid);
        self.prioritized_stale_retirements
            .retain(|pending| pending != bare_jid);
        self.stale_retirement_queue_retries.remove(bare_jid);
    }

    fn finalize_stale_actor_retirement(&mut self, bare_jid: &BareJid) {
        // The ack waiter fires exactly once per queued retirement; the entry
        // may legitimately be gone already (dead actor path, RemoveUser).
        if !self.pending_stale_retirements.contains(bare_jid) {
            return;
        }
        self.retire_stale_user_without_releasing_claim(bare_jid);
    }

    fn retire_stale_user_without_releasing_claim(&mut self, bare_jid: &BareJid) {
        if let Some(entry) = self.users.remove(bare_jid) {
            entry.actor_ref.kill();
        }
        self.clear_pending_stale_retirement(bare_jid);
    }

    fn issue_stale_retirement_force_detach(
        &self,
        bare_jid: &BareJid,
        entry: &UserEntry,
    ) -> (
        StaleRetirementQueueResult,
        Vec<tokio::sync::oneshot::Receiver<ForceDetachOutcome>>,
    ) {
        let mut saturated = false;
        let mut acks = Vec::new();
        let mut jids = entry.resources.keys().cloned().collect::<Vec<_>>();
        jids.sort();
        for jid in jids {
            let Some(resource) = entry.resources.get(&jid) else {
                continue;
            };
            let (ack, ack_rx) = tokio::sync::oneshot::channel();
            let request = ForceDetachRequest {
                origin: ForceDetachOrigin::RegistryStaleActorRetirement,
                requester_bare_jid: bare_jid.clone(),
                ack,
            };
            match resource.force_detach_sender().try_send(request) {
                Ok(()) => acks.push(ack_rx),
                Err(error) => match error {
                    tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                        warn!(
                            jid = %jid,
                            "stale UserActor resource force-detach receiver is closed; demoting stale actor"
                        );
                    }
                    tokio::sync::mpsc::error::TrySendError::Full(_) => {
                        saturated = true;
                        warn!(
                            jid = %jid,
                            "stale UserActor resource force-detach queue is full; retrying asynchronously"
                        );
                    }
                },
            }
        }
        (
            if saturated {
                StaleRetirementQueueResult::Saturated
            } else {
                StaleRetirementQueueResult::Queued
            },
            acks,
        )
    }

    async fn retry_pending_stale_retirement_work(&mut self, registry_ref: &ActorRef<Self>) {
        let Some(pending) = self.next_pending_stale_retirement() else {
            return;
        };
        self.retry_one_pending_stale_retirement(pending, registry_ref)
            .await;
    }

    fn next_pending_stale_retirement(&mut self) -> Option<PendingStaleUserRetirement> {
        while let Some(bare_jid) = self.prioritized_stale_retirements.pop_front() {
            if self.pending_stale_retirements.contains(&bare_jid) {
                return Some(PendingStaleUserRetirement { bare_jid });
            }
        }
        self.pending_stale_retirements
            .iter()
            .next()
            .cloned()
            .map(|bare_jid| PendingStaleUserRetirement { bare_jid })
    }

    async fn retry_one_pending_stale_retirement(
        &mut self,
        pending: PendingStaleUserRetirement,
        registry_ref: &ActorRef<Self>,
    ) {
        let bare_jid = pending.bare_jid;
        let Some(entry) = self.users.get(&bare_jid).cloned() else {
            self.clear_pending_stale_retirement(&bare_jid);
            return;
        };
        if !entry.actor_ref.is_alive() {
            self.retire_stale_user_without_releasing_claim(&bare_jid);
            return;
        }
        match self
            .validate_existing_user_entry_claim(&bare_jid, &entry)
            .await
        {
            UserEntryClaimStatus::Current => {
                self.clear_pending_stale_retirement(&bare_jid);
            }
            UserEntryClaimStatus::ValidationUnavailable => {}
            UserEntryClaimStatus::ProvenStale => {
                if entry.resources.is_empty() {
                    self.retire_stale_user_without_releasing_claim(&bare_jid);
                } else {
                    match self.issue_stale_retirement_force_detach(&bare_jid, &entry) {
                        (StaleRetirementQueueResult::Queued, acks) => {
                            self.stale_retirement_queue_retries.remove(&bare_jid);
                            debug!(
                                jid = %bare_jid,
                                resources = entry.resources.len(),
                                acks = acks.len(),
                                "queued stale UserActor force-detach requests; awaiting \
                                 acknowledgements before retiring the stale actor mirror"
                            );
                            // Queue admission is not closure proof: a socket
                            // mid-stanza would keep processing under a killed
                            // actor while a replacement publishes. Await the
                            // detach acknowledgements OUTSIDE the registry
                            // turn (bounded, so a wedged socket cannot pin
                            // the retirement forever), then finalize via a
                            // self-message; the entry stays pending so
                            // registration retries observe Busy meanwhile.
                            let registry = registry_ref.clone();
                            let finalize_jid = bare_jid.clone();
                            tokio::spawn(async move {
                                let _ = tokio::time::timeout(
                                    STALE_RETIREMENT_ACK_TIMEOUT,
                                    join_all(acks),
                                )
                                .await;
                                let _ = registry
                                    .tell(FinalizeStaleActorRetirement {
                                        bare_jid: finalize_jid,
                                    })
                                    .await;
                            });
                        }
                        (StaleRetirementQueueResult::Saturated, _) => {
                            let retries = self
                                .stale_retirement_queue_retries
                                .entry(bare_jid.clone())
                                .or_default();
                            *retries = retries.saturating_add(1);
                            if *retries >= STALE_RETIREMENT_MAX_QUEUE_RETRIES {
                                warn!(
                                    jid = %bare_jid,
                                    retries = *retries,
                                    "stale UserActor force-detach remained saturated; demoting stale actor"
                                );
                                self.retire_stale_user_without_releasing_claim(&bare_jid);
                            }
                        }
                    }
                }
            }
        }
    }

    fn take_retry_batch<T>(inventory: &mut Vec<T>) -> Vec<T> {
        if inventory.len() <= CONVERGENCE_RETRY_BATCH_LIMIT {
            return std::mem::take(inventory);
        }
        let mut batch = std::mem::take(inventory);
        let remainder = batch.split_off(CONVERGENCE_RETRY_BATCH_LIMIT);
        *inventory = remainder;
        batch
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

/// A closed, typed explanation for a retryable force-detach convergence
/// failure.  These are deliberately not transport error strings: janitors
/// need to distinguish liveness from protocol ownership.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnregisterAndReleaseRetryableFailure {
    UserActorBusy,
    UserActorStateLost,
}

/// Result of atomically pruning a resource and, if it was the last one,
/// releasing the UserActor ownership claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, kameo::Reply)]
pub enum UnregisterAndReleaseOutcome {
    Released,
    RetainedLiveResources,
    AlreadyAbsent,
    RetryableFailure(UnregisterAndReleaseRetryableFailure),
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
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let bare_jid = msg.jid.to_bare();
        let mirrored_entry = msg.entry.clone();
        if self.poisoned_users.contains(&bare_jid) {
            return Err(UserRegistryError::UserActorStateLost(bare_jid));
        }

        self.drain_pending_unregisters_for(&msg.jid).await;
        if self.poisoned_users.contains(&bare_jid) {
            return Err(UserRegistryError::UserActorStateLost(bare_jid));
        }

        self.kick_stale_retirement_if_pending(&bare_jid, ctx.actor_ref());
        let user_actor = if let Some(actor_ref) = self
            .existing_user_actor_for_register(&bare_jid, ctx.actor_ref())
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
                self.cancel_terminal_claim_release(&bare_jid);
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
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let bare_jid = msg.jid.to_bare();
        let mirrored_entry = msg.entry.clone();
        if self.poisoned_users.contains(&bare_jid) {
            return Err(UserRegistryError::UserActorStateLost(bare_jid));
        }

        self.drain_pending_unregisters_for(&msg.jid).await;
        if self.poisoned_users.contains(&bare_jid) {
            return Err(UserRegistryError::UserActorStateLost(bare_jid));
        }

        self.kick_stale_retirement_if_pending(&bare_jid, ctx.actor_ref());
        let user_actor = if let Some(actor_ref) = self
            .existing_user_actor_for_register(&bare_jid, ctx.actor_ref())
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
                    self.cancel_terminal_claim_release(&bare_jid);
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
        let retry_jid = msg.jid.clone();
        let retry_owner = msg.owner.clone();
        match self
            .unregister_and_release_if_empty(msg.jid, msg.owner)
            .await
        {
            UnregisterAndReleaseOutcome::Released
            | UnregisterAndReleaseOutcome::RetainedLiveResources
            | UnregisterAndReleaseOutcome::AlreadyAbsent => Ok(()),
            UnregisterAndReleaseOutcome::RetryableFailure(reason) => {
                self.remember_pending_unregister(retry_jid, retry_owner);
                Err(match reason {
                    UnregisterAndReleaseRetryableFailure::UserActorBusy => {
                        UserRegistryError::UserActorBusy(bare_jid)
                    }
                    UnregisterAndReleaseRetryableFailure::UserActorStateLost => {
                        UserRegistryError::UserActorStateLost(bare_jid)
                    }
                })
            }
        }
    }
}

/// Force-detach-only variant of [`UnregisterUserResource`].  It is an ask so
/// the resume-steal acknowledgement cannot run ahead of registry convergence.
pub struct UnregisterAndReleaseIfEmpty {
    pub jid: FullJid,
    pub owner: Option<Arc<AtomicBool>>,
}

impl kameo::message::Message<UnregisterAndReleaseIfEmpty> for UserRegistryActor {
    type Reply = UnregisterAndReleaseOutcome;

    async fn handle(
        &mut self,
        msg: UnregisterAndReleaseIfEmpty,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let retry_jid = msg.jid.clone();
        let retry_owner = msg.owner.clone();
        let outcome = self
            .unregister_and_release_if_empty(msg.jid, msg.owner)
            .await;
        if matches!(outcome, UnregisterAndReleaseOutcome::RetryableFailure(_)) {
            self.remember_pending_unregister(retry_jid, retry_owner);
        }
        outcome
    }
}

/// Force-detach retry variant of [`UnregisterAndReleaseIfEmpty`].
///
/// The caller owns the retry budget and records the pending-unregister only
/// after every bounded synchronous attempt has returned a retryable failure.
/// Recording each short-lived `UserActorBusy` result would unnecessarily
/// leave janitor inventory behind after a later retry has already converged.
pub struct UnregisterAndReleaseIfEmptyWithoutPendingRecord {
    pub jid: FullJid,
    pub owner: Option<Arc<AtomicBool>>,
}

impl kameo::message::Message<UnregisterAndReleaseIfEmptyWithoutPendingRecord>
    for UserRegistryActor
{
    type Reply = UnregisterAndReleaseOutcome;

    async fn handle(
        &mut self,
        msg: UnregisterAndReleaseIfEmptyWithoutPendingRecord,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.unregister_and_release_if_empty(msg.jid, msg.owner)
            .await
    }
}

/// Retry actor-owned convergence inventories.  The registry entry is checked
/// in this handler immediately before every durable release, which prevents a
/// stale terminal retry from releasing a same-node self-reacquisition. Each
/// mailbox turn processes only a bounded batch so a large retry backlog cannot
/// monopolize the registry and time out the janitor's ask budget.
pub struct RetryUserRegistryConvergence;

/// Finalize an asynchronously queued stale-actor retirement once its
/// force-detach acknowledgements settled (or their bounded wait elapsed).
/// Sent by the ack-waiter task the retirement spawned; a no-op when the
/// retirement is no longer pending.
pub struct FinalizeStaleActorRetirement {
    pub bare_jid: BareJid,
}

impl kameo::message::Message<FinalizeStaleActorRetirement> for UserRegistryActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: FinalizeStaleActorRetirement,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.finalize_stale_actor_retirement(&msg.bare_jid);
    }
}

impl kameo::message::Message<RetryUserRegistryConvergence> for UserRegistryActor {
    type Reply = (usize, usize);

    async fn handle(
        &mut self,
        _msg: RetryUserRegistryConvergence,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let pending = Self::take_retry_batch(&mut self.pending_unregisters);
        let mut pending_remaining = self.pending_unregisters.len();
        for pending_unregister in pending {
            let retry_jid = pending_unregister.jid.clone();
            let retry_owner = pending_unregister.owner.clone();
            if matches!(
                self.unregister_and_release_if_empty(
                    pending_unregister.jid,
                    pending_unregister.owner
                )
                .await,
                UnregisterAndReleaseOutcome::RetryableFailure(_)
            ) {
                self.remember_pending_unregister(retry_jid, retry_owner);
                pending_remaining += 1;
            }
        }

        // Pending-unregister retries still run inline because they already
        // have strict child-actor timeouts; only the claim-store release path
        // needs an extra cutoff to keep janitor turns bounded.
        let releases = Self::take_retry_batch(&mut self.terminal_claim_releases);
        let mut releases_remaining = self.terminal_claim_releases.len();
        for release in releases {
            // A live entry is the authority for retention, even when its
            // claim epoch happens to be identical after self-reacquisition.
            if self.users.contains_key(&release.bare_jid) {
                continue;
            }
            if !self
                .release_user_claim(&release.bare_jid, &release.claim)
                .await
            {
                releases_remaining += 1;
            }
        }
        (pending_remaining, releases_remaining)
    }
}

pub struct RetryPendingStaleUserRetirements;

impl kameo::message::Message<RetryPendingStaleUserRetirements> for UserRegistryActor {
    type Reply = ();

    async fn handle(
        &mut self,
        _msg: RetryPendingStaleUserRetirements,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.stale_retirement_retry_scheduled = false;
        self.retry_pending_stale_retirement_work(ctx.actor_ref())
            .await;
        if !self.pending_stale_retirements.is_empty() {
            self.schedule_pending_stale_retirement_retry(ctx.actor_ref());
        }
    }
}

/// Persist a force-detach removal for janitor convergence when the caller's
/// preceding ask timed out before it could observe whether the handler ran.
/// This is intentionally a separate actor message: submitting it after the
/// original ask preserves mailbox order, so it covers both pre-handler and
/// post-handler reply-loss cases without guessing which occurred.
pub struct RecordPendingUserUnregister {
    pub jid: FullJid,
    pub owner: Option<Arc<AtomicBool>>,
}

impl kameo::message::Message<RecordPendingUserUnregister> for UserRegistryActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: RecordPendingUserUnregister,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.remember_pending_unregister(msg.jid, msg.owner);
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
            let _ = self.release_user_claim(&msg.bare_jid, &entry.claim).await;
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
                let _ = self.release_user_claim(&msg.bare_jid, &entry.claim).await;
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

/// Cross-crate test controls, kept behind the existing `test-utils` feature.
#[cfg(feature = "test-utils")]
pub mod test_support {
    use super::UserRegistryActor;

    pub struct PendingUnregisterCount;

    impl kameo::message::Message<PendingUnregisterCount> for UserRegistryActor {
        type Reply = usize;

        async fn handle(
            &mut self,
            _msg: PendingUnregisterCount,
            _ctx: &mut kameo::message::Context<Self, Self::Reply>,
        ) -> Self::Reply {
            self.pending_unregisters.len()
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
