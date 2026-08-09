use super::*;
use crate::ownership::{
    ClaimError, ClaimSnapshot, ClaimStore, Entity, EntityType, InProcessClaimStore, NodeIdentity,
    ResumeIdentityProof, SharedNodeIdentity, StalePredicate,
};
use crate::registry::connection_registry::{ConnectionEntry, ForceDetachOutcome, OutboundStanza};
use async_trait::async_trait;
use kameo::actor::Spawn;
use kameo::error::SendError;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

struct WedgeUserActor {
    entered: Arc<tokio::sync::Notify>,
}

struct GateUserActor {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

impl kameo::message::Message<GateUserActor> for UserActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: GateUserActor,
        _ctx: &mut kameo::message::Context<Self, Self::Reply>,
    ) -> Self::Reply {
        msg.entered.notify_one();
        msg.release.notified().await;
    }
}

impl kameo::message::Message<WedgeUserActor> for UserActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: WedgeUserActor,
        _ctx: &mut kameo::message::Context<Self, Self::Reply>,
    ) -> Self::Reply {
        msg.entered.notify_one();
        std::future::pending().await
    }
}

/// A bounded outbound channel for a test registration. Returns the sender to
/// register and the receiver, which the caller keeps alive so the channel
/// does not report closed.
fn outbound_channel() -> (mpsc::Sender<OutboundStanza>, mpsc::Receiver<OutboundStanza>) {
    mpsc::channel(16)
}

fn bare(user: &str) -> BareJid {
    format!("{user}@example.com").parse().expect("valid JID")
}

fn full(user: &str, resource: &str) -> FullJid {
    format!("{user}@example.com/{resource}")
        .parse()
        .expect("valid JID")
}

fn user_entity(jid: &BareJid) -> Entity {
    Entity::new(EntityType::UserActor, jid.to_string())
}

fn this_identity() -> NodeIdentity {
    NodeIdentity::new("node-this", "epoch-this")
}

fn foreign_identity() -> NodeIdentity {
    NodeIdentity::new("node-foreign", "epoch-foreign")
}

async fn spawn_registry() -> ActorRef<UserRegistryActor> {
    UserRegistryActor::spawn(UserRegistryActor::new())
}

async fn wire_claims(
    registry: &ActorRef<UserRegistryActor>,
    claim_store: Arc<dyn ClaimStore>,
    identity: NodeIdentity,
) {
    wire_shared_claims(registry, claim_store, SharedNodeIdentity::new(identity)).await;
}

async fn wire_shared_claims(
    registry: &ActorRef<UserRegistryActor>,
    claim_store: Arc<dyn ClaimStore>,
    node_identity: SharedNodeIdentity,
) {
    registry
        .ask(WireUserClusteringClaims {
            claim_store,
            node_identity,
        })
        .await
        .expect("wire user claims");
}

struct RecordingClaimStore {
    state: Arc<Mutex<Option<(NodeIdentity, ClaimEpoch, bool)>>>,
    release_owners: Mutex<Vec<NodeIdentity>>,
    fence_errors: AtomicBool,
    fail_releases: AtomicBool,
    block_release: AtomicBool,
    detach_release: AtomicBool,
    late_release_pending: Arc<AtomicBool>,
    steal_calls: AtomicUsize,
    block_ensure: AtomicBool,
    ensure_entered: tokio::sync::Notify,
    continue_ensure: tokio::sync::Notify,
    release_entered: tokio::sync::Notify,
    continue_release: tokio::sync::Notify,
    late_release_started: Arc<tokio::sync::Notify>,
    allow_late_release: Arc<tokio::sync::Notify>,
    late_release_completed: Arc<tokio::sync::Notify>,
}

impl RecordingClaimStore {
    fn empty() -> Self {
        Self {
            state: Arc::new(Mutex::new(None)),
            release_owners: Mutex::new(Vec::new()),
            fence_errors: AtomicBool::new(false),
            fail_releases: AtomicBool::new(false),
            block_release: AtomicBool::new(false),
            detach_release: AtomicBool::new(false),
            late_release_pending: Arc::new(AtomicBool::new(false)),
            steal_calls: AtomicUsize::new(0),
            block_ensure: AtomicBool::new(false),
            ensure_entered: tokio::sync::Notify::new(),
            continue_ensure: tokio::sync::Notify::new(),
            release_entered: tokio::sync::Notify::new(),
            continue_release: tokio::sync::Notify::new(),
            late_release_started: Arc::new(tokio::sync::Notify::new()),
            allow_late_release: Arc::new(tokio::sync::Notify::new()),
            late_release_completed: Arc::new(tokio::sync::Notify::new()),
        }
    }

    fn seeded(owner: NodeIdentity, epoch: ClaimEpoch, owner_lease_fresh: bool) -> Self {
        Self {
            state: Arc::new(Mutex::new(Some((owner, epoch, owner_lease_fresh)))),
            release_owners: Mutex::new(Vec::new()),
            fence_errors: AtomicBool::new(false),
            fail_releases: AtomicBool::new(false),
            block_release: AtomicBool::new(false),
            detach_release: AtomicBool::new(false),
            late_release_pending: Arc::new(AtomicBool::new(false)),
            steal_calls: AtomicUsize::new(0),
            block_ensure: AtomicBool::new(false),
            ensure_entered: tokio::sync::Notify::new(),
            continue_ensure: tokio::sync::Notify::new(),
            release_entered: tokio::sync::Notify::new(),
            continue_release: tokio::sync::Notify::new(),
            late_release_started: Arc::new(tokio::sync::Notify::new()),
            allow_late_release: Arc::new(tokio::sync::Notify::new()),
            late_release_completed: Arc::new(tokio::sync::Notify::new()),
        }
    }

    fn release_owners(&self) -> Vec<NodeIdentity> {
        self.release_owners.lock().expect("lock").clone()
    }

    fn set_owner_lease_fresh(&self, owner_lease_fresh: bool) {
        let mut state = self.state.lock().expect("lock");
        let Some((owner, epoch, _)) = state.clone() else {
            panic!("claim store must be seeded before changing owner freshness");
        };
        *state = Some((owner, epoch, owner_lease_fresh));
    }

    fn set_fence_errors(&self, fence_errors: bool) {
        self.fence_errors.store(fence_errors, Ordering::SeqCst);
    }

    fn set_fail_releases(&self, fail_releases: bool) {
        self.fail_releases.store(fail_releases, Ordering::SeqCst);
    }

    fn block_next_ensure(&self) {
        self.block_ensure.store(true, Ordering::SeqCst);
    }

    fn block_next_release(&self) {
        self.block_release.store(true, Ordering::SeqCst);
    }

    fn detach_next_release(&self) {
        self.detach_release.store(true, Ordering::SeqCst);
    }
}

#[async_trait]
impl ClaimStore for RecordingClaimStore {
    async fn ensure_schema(&self) -> Result<(), ClaimError> {
        Ok(())
    }

    async fn acquire(&self, _entity: &Entity, me: &NodeIdentity) -> Result<ClaimEpoch, ClaimError> {
        let mut state = self.state.lock().expect("lock");
        if state.is_some() {
            return Err(ClaimError::AlreadyClaimed);
        }
        *state = Some((me.clone(), ClaimEpoch(0), true));
        Ok(ClaimEpoch(0))
    }

    async fn ensure_claimed(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        let existing = self.state.lock().expect("lock").clone();
        let result = match existing {
            None => self.acquire(entity, me).await,
            Some((owner, epoch, _)) if owner == *me => Ok(epoch),
            Some(_) => Err(ClaimError::AlreadyClaimed),
        };
        if self.block_ensure.swap(false, Ordering::SeqCst) {
            self.ensure_entered.notify_one();
            self.continue_ensure.notified().await;
        }
        result
    }

    async fn steal_stale(
        &self,
        _entity: &Entity,
        observed: ClaimEpoch,
        _staleness: StalePredicate,
        me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        self.steal_calls.fetch_add(1, Ordering::SeqCst);
        let mut state = self.state.lock().expect("lock");
        match &*state {
            Some((_, epoch, false)) if *epoch == observed => {
                let new_epoch = ClaimEpoch(epoch.0 + 1);
                *state = Some((me.clone(), new_epoch, true));
                Ok(new_epoch)
            }
            _ => Err(ClaimError::Conflict),
        }
    }

    async fn steal_for_resume(
        &self,
        _entity: &Entity,
        _observed: ClaimEpoch,
        _witness: ResumeIdentityProof,
        _me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        Err(ClaimError::Conflict)
    }

    async fn current_claim(&self, _entity: &Entity) -> Result<Option<ClaimSnapshot>, ClaimError> {
        Ok(self.state.lock().expect("lock").clone().map(
            |(owner, claim_epoch, owner_lease_fresh)| ClaimSnapshot {
                owner,
                claim_epoch,
                owner_lease_fresh,
            },
        ))
    }

    async fn fence(
        &self,
        _entity: &Entity,
        me: &NodeIdentity,
        mine: ClaimEpoch,
    ) -> Result<bool, ClaimError> {
        if self.fence_errors.load(Ordering::SeqCst) {
            return Err(ClaimError::Backend("test fence unavailable".to_string()));
        }
        Ok(
            matches!(&*self.state.lock().expect("lock"), Some((owner, epoch, _)) if owner == me && *epoch == mine),
        )
    }

    async fn release(
        &self,
        _entity: &Entity,
        me: &NodeIdentity,
        mine: ClaimEpoch,
    ) -> Result<(), ClaimError> {
        self.release_owners.lock().expect("lock").push(me.clone());
        if self.block_release.swap(false, Ordering::SeqCst) {
            self.release_entered.notify_one();
            self.continue_release.notified().await;
        }
        if self.late_release_pending.load(Ordering::SeqCst) {
            return std::future::pending::<Result<(), ClaimError>>().await;
        }
        if self.detach_release.swap(false, Ordering::SeqCst) {
            self.late_release_pending.store(true, Ordering::SeqCst);
            let state_writer = Arc::clone(&self.state);
            let started = Arc::clone(&self.late_release_started);
            let allow = Arc::clone(&self.allow_late_release);
            let completed = Arc::clone(&self.late_release_completed);
            let pending = Arc::clone(&self.late_release_pending);
            let me = me.clone();
            tokio::spawn(async move {
                started.notify_one();
                allow.notified().await;
                let mut state = state_writer.lock().expect("lock");
                if matches!(&*state, Some((owner, epoch, _)) if *owner == me && *epoch == mine) {
                    *state = None;
                }
                pending.store(false, Ordering::SeqCst);
                completed.notify_one();
            });
            return std::future::pending::<Result<(), ClaimError>>().await;
        }
        if self.fail_releases.load(Ordering::SeqCst) {
            return Err(ClaimError::Backend("test release unavailable".to_string()));
        }
        let mut state = self.state.lock().expect("lock");
        if matches!(&*state, Some((owner, epoch, _)) if owner == me && *epoch == mine) {
            *state = None;
        }
        Ok(())
    }

    async fn release_many(
        &self,
        _entities: &[Entity],
        me: &NodeIdentity,
    ) -> Result<(), ClaimError> {
        self.release_owners.lock().expect("lock").push(me.clone());
        let mut state = self.state.lock().expect("lock");
        if matches!(&*state, Some((owner, _, _)) if owner == me) {
            *state = None;
        }
        Ok(())
    }
}

#[tokio::test]
async fn test_get_or_create_spawns_new_user() {
    let registry = spawn_registry().await;

    let actor_ref = registry
        .ask(GetOrCreateUser {
            bare_jid: bare("alice"),
        })
        .await
        .expect("ask failed");

    // Asking again should return the same actor (by id).
    let actor_ref2 = registry
        .ask(GetOrCreateUser {
            bare_jid: bare("alice"),
        })
        .await
        .expect("ask failed");

    assert_eq!(actor_ref.id(), actor_ref2.id());
}

#[tokio::test]
async fn test_get_user_returns_none_when_absent() {
    let registry = spawn_registry().await;

    let result = registry
        .ask(GetUser {
            bare_jid: bare("ghost"),
        })
        .await
        .expect("ask failed");

    assert!(result.is_none());
}

#[tokio::test]
async fn test_get_user_returns_some_after_create() {
    let registry = spawn_registry().await;

    registry
        .ask(GetOrCreateUser {
            bare_jid: bare("bob"),
        })
        .await
        .expect("ask failed");

    let result = registry
        .ask(GetUser {
            bare_jid: bare("bob"),
        })
        .await
        .expect("ask failed");

    assert!(result.is_some());
}

#[tokio::test]
async fn test_remove_user() {
    let registry = spawn_registry().await;

    registry
        .ask(GetOrCreateUser {
            bare_jid: bare("carol"),
        })
        .await
        .expect("ask failed");

    let removed = registry
        .ask(RemoveUser {
            bare_jid: bare("carol"),
        })
        .await
        .expect("ask failed");

    assert!(removed);

    // Removing again should return false.
    let removed_again = registry
        .ask(RemoveUser {
            bare_jid: bare("carol"),
        })
        .await
        .expect("ask failed");

    assert!(!removed_again);
}

#[tokio::test]
async fn test_get_or_create_acquires_user_actor_claim() {
    let registry = spawn_registry().await;
    let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
    let bare_jid = bare("claimed");
    let entity = user_entity(&bare_jid);
    wire_claims(&registry, Arc::clone(&claim_store), this_identity()).await;

    registry
        .ask(GetOrCreateUser {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("get_or_create");

    let snapshot = claim_store
        .current_claim(&entity)
        .await
        .expect("current_claim")
        .expect("user claim should be held after actor spawn");
    assert_eq!(snapshot.owner, this_identity());
}

#[tokio::test]
async fn get_or_create_releases_a_claim_if_identity_rotates_after_the_cas() {
    let registry = spawn_registry().await;
    let claim_store = Arc::new(RecordingClaimStore::empty());
    let claim_store_trait: Arc<dyn ClaimStore> = claim_store.clone();
    let old = this_identity();
    let fresh = NodeIdentity::new("node-this", "epoch-after-self-fence");
    let shared_identity = SharedNodeIdentity::new(old);
    wire_shared_claims(&registry, claim_store_trait, shared_identity.clone()).await;
    claim_store.block_next_ensure();
    let bare_jid = bare("post-cas-rotation");

    let creating = tokio::spawn({
        let registry = registry.clone();
        let bare_jid = bare_jid.clone();
        async move { registry.ask(GetOrCreateUser { bare_jid }).await }
    });
    claim_store.ensure_entered.notified().await;
    shared_identity.rotate(fresh).await;
    claim_store.continue_ensure.notify_one();

    assert!(matches!(
        creating.await.expect("creation task"),
        Err(SendError::HandlerError(UserRegistryError::ClaimUnavailable(jid))) if jid == bare_jid
    ));
    assert!(registry
        .ask(ListUsers)
        .await
        .expect("list users")
        .is_empty());
    assert!(claim_store
        .current_claim(&user_entity(&bare_jid))
        .await
        .expect("claim lookup")
        .is_none());
}

#[tokio::test]
async fn list_users_owned_by_excludes_fresh_post_rotation_users() {
    let registry = spawn_registry().await;
    let old = this_identity();
    let fresh = NodeIdentity::new("node-this", "fresh-incarnation");
    let shared_identity = SharedNodeIdentity::new(old.clone());
    wire_shared_claims(
        &registry,
        Arc::new(InProcessClaimStore::new()),
        shared_identity.clone(),
    )
    .await;
    registry
        .ask(GetOrCreateUser {
            bare_jid: bare("old-owner-user"),
        })
        .await
        .expect("old user");
    shared_identity.rotate(fresh.clone()).await;
    registry
        .ask(GetOrCreateUser {
            bare_jid: bare("fresh-owner-user"),
        })
        .await
        .expect("fresh user");

    assert_eq!(
        registry
            .ask(ListUsersOwnedBy { owner: old })
            .await
            .expect("old-owner users"),
        vec![bare("old-owner-user")]
    );
    assert_eq!(
        registry
            .ask(ListUsersOwnedBy { owner: fresh })
            .await
            .expect("fresh-owner users"),
        vec![bare("fresh-owner-user")]
    );
}

#[tokio::test]
async fn stale_exact_owner_demotion_preserves_a_fresh_same_jid_user() {
    let registry = spawn_registry().await;
    let old = this_identity();
    let fresh = NodeIdentity::new("node-this", "fresh-incarnation");
    let shared_identity = SharedNodeIdentity::new(old.clone());
    let claim_store = Arc::new(InProcessClaimStore::new());
    wire_shared_claims(&registry, claim_store.clone(), shared_identity.clone()).await;
    let bare_jid = bare("same-owner-jid");
    registry
        .ask(GetOrCreateUser {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("old user");
    assert!(registry
        .ask(DemoteUserActorIfOwner {
            bare_jid: bare_jid.clone(),
            owner: old.clone(),
        })
        .await
        .expect("demote old user")
        .is_some());
    claim_store
        .release(&user_entity(&bare_jid), &old, ClaimEpoch(0))
        .await
        .expect("simulate authoritative old-claim retirement");

    shared_identity.rotate(fresh).await;
    registry
        .ask(GetOrCreateUser {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("fresh user");
    assert!(registry
        .ask(DemoteUserActorIfOwner {
            bare_jid: bare_jid.clone(),
            owner: old,
        })
        .await
        .expect("stale demotion")
        .is_none());
    assert_eq!(
        registry.ask(ListUsers).await.expect("list users"),
        vec![bare_jid]
    );
}

#[tokio::test]
async fn exact_owner_demotion_never_waits_on_a_wedged_multi_resource_actor() {
    let registry = spawn_registry().await;
    let owner = this_identity();
    wire_claims(
        &registry,
        Arc::new(InProcessClaimStore::new()),
        owner.clone(),
    )
    .await;
    let bare_jid = bare("wedged-exact-demotion");
    let mut receivers = Vec::new();
    for resource in ["one", "two", "three"] {
        let (sender, receiver) = outbound_channel();
        receivers.push(receiver);
        registry
            .ask(RegisterUserResource {
                jid: full("wedged-exact-demotion", resource),
                entry: ConnectionEntry::new(sender),
            })
            .await
            .expect("register resource");
    }
    let actor = registry
        .ask(GetUserForLocalClaim {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("get user")
        .expect("live user");
    let entered = Arc::new(tokio::sync::Notify::new());
    let wedge = tokio::spawn({
        let actor = actor.clone();
        let entered = entered.clone();
        async move { actor.ask(WedgeUserActor { entered }).await }
    });
    entered.notified().await;

    let demoted = tokio::time::timeout(
        Duration::from_millis(100),
        registry.ask(DemoteUserActorIfOwner { bare_jid, owner }),
    )
    .await
    .expect("exact demotion must not wait on the wedged child")
    .expect("registry demotion")
    .expect("matching owner");
    assert_eq!(demoted.resources.len(), 3);
    assert!(!actor.is_alive());
    assert!(wedge.await.expect("wedge task").is_err());
    drop(receivers);
}

#[tokio::test]
async fn test_remove_user_releases_user_actor_claim() {
    let registry = spawn_registry().await;
    let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
    let bare_jid = bare("released");
    let entity = user_entity(&bare_jid);
    wire_claims(&registry, Arc::clone(&claim_store), this_identity()).await;

    registry
        .ask(GetOrCreateUser {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("get_or_create");
    assert!(claim_store
        .current_claim(&entity)
        .await
        .expect("current_claim")
        .is_some());

    let removed = registry
        .ask(RemoveUser {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("remove");
    assert!(removed);

    assert!(claim_store
        .current_claim(&entity)
        .await
        .expect("current_claim")
        .is_none());
    claim_store
        .acquire(&entity, &foreign_identity())
        .await
        .expect("a different node can acquire after release");
}

#[tokio::test]
async fn test_remove_user_releases_with_the_acquisition_identity_after_identity_rotation() {
    let registry = spawn_registry().await;
    let claim_store = Arc::new(RecordingClaimStore::empty());
    let claim_store_trait: Arc<dyn ClaimStore> = claim_store.clone();
    let shared_identity = SharedNodeIdentity::new(this_identity());
    let bare_jid = bare("rotated-release");
    let entity = user_entity(&bare_jid);
    wire_shared_claims(&registry, claim_store_trait, shared_identity.clone()).await;

    registry
        .ask(GetOrCreateUser {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("get_or_create");

    shared_identity
        .rotate(NodeIdentity::new("node-this", "epoch-after-self-fence"))
        .await;

    let removed = registry
        .ask(RemoveUser {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("remove");
    assert!(removed);

    assert!(
        claim_store
            .current_claim(&entity)
            .await
            .expect("current_claim")
            .is_none(),
        "release must use the identity that acquired the claim, not the rotated shared identity"
    );
    assert_eq!(claim_store.release_owners(), vec![this_identity()]);
}

#[tokio::test]
async fn test_unregister_last_resource_releases_user_actor_claim() {
    let registry = spawn_registry().await;
    let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
    let bare_jid = bare("phone");
    let jid = full("phone", "mobile");
    let entity = user_entity(&bare_jid);
    wire_claims(&registry, Arc::clone(&claim_store), this_identity()).await;

    let (tx, _rx) = outbound_channel();
    registry
        .ask(RegisterUserResource {
            jid: jid.clone(),
            entry: ConnectionEntry::new(tx),
        })
        .await
        .expect("register");
    assert!(claim_store
        .current_claim(&entity)
        .await
        .expect("current_claim")
        .is_some());

    registry
        .ask(UnregisterUserResource { jid, owner: None })
        .await
        .expect("unregister");

    assert_eq!(registry.ask(UserCount).await.expect("count"), 0);
    assert!(claim_store
        .current_claim(&entity)
        .await
        .expect("current_claim")
        .is_none());
}

#[tokio::test]
async fn unregister_release_timeout_defers_claim_release_without_blocking_the_actor_turn() {
    let registry = spawn_registry().await;
    let store = Arc::new(RecordingClaimStore::empty());
    let store_trait: Arc<dyn ClaimStore> = store.clone();
    wire_claims(&registry, store_trait, this_identity()).await;
    let jid = full("timeout-release", "phone");
    let entity = user_entity(&jid.to_bare());
    let (tx, _rx) = outbound_channel();
    registry
        .ask(RegisterUserResource {
            jid: jid.clone(),
            entry: ConnectionEntry::new(tx),
        })
        .await
        .expect("register");

    store.block_next_release();
    let outcome = tokio::time::timeout(
        CLAIM_RELEASE_TIMEOUT + Duration::from_millis(150),
        registry.ask(UnregisterAndReleaseIfEmpty {
            jid: jid.clone(),
            owner: None,
        }),
    )
    .await
    .expect("force-detach ask must stop waiting once claim release times out")
    .expect("typed outcome");
    assert_eq!(outcome, UnregisterAndReleaseOutcome::Released);
    store.release_entered.notified().await;
    assert!(
        store.current_claim(&entity).await.expect("claim").is_some(),
        "timed-out release must stay queued for janitor convergence"
    );

    registry
        .ask(RetryUserRegistryConvergence)
        .await
        .expect("retry");
    assert!(
        store.current_claim(&entity).await.expect("claim").is_none(),
        "a later janitor sweep should finish the deferred release once the store responds"
    );
}

#[tokio::test]
async fn test_get_or_create_steals_user_actor_claim_from_a_dead_owner() {
    let registry = spawn_registry().await;
    let claim_store = Arc::new(RecordingClaimStore::seeded(
        foreign_identity(),
        ClaimEpoch(3),
        false,
    ));
    let claim_store_trait: Arc<dyn ClaimStore> = claim_store.clone();
    let bare_jid = bare("dead-foreign");
    let entity = user_entity(&bare_jid);
    wire_claims(&registry, claim_store_trait, this_identity()).await;

    let actor = registry
        .ask(GetOrCreateUser {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("get_or_create should steal from a dead owner");
    assert!(actor.is_alive());

    let snapshot = claim_store
        .current_claim(&entity)
        .await
        .expect("current_claim")
        .expect("claim exists after steal");
    assert_eq!(snapshot.owner, this_identity());
    assert_eq!(snapshot.claim_epoch, ClaimEpoch(4));
    assert_eq!(
        claim_store.steal_calls.load(Ordering::SeqCst),
        1,
        "dead-owner recovery must go through steal_stale(OwnerStale)"
    );
}

#[tokio::test]
async fn test_get_or_create_refuses_live_foreign_user_actor_claim() {
    let registry = spawn_registry().await;
    let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
    let bare_jid = bare("foreign");
    let entity = user_entity(&bare_jid);
    claim_store
        .acquire(&entity, &foreign_identity())
        .await
        .expect("foreign acquire");
    wire_claims(&registry, Arc::clone(&claim_store), this_identity()).await;

    let result = registry
        .ask(GetOrCreateUser {
            bare_jid: bare_jid.clone(),
        })
        .await;

    assert!(
        matches!(
            result,
            Err(SendError::HandlerError(UserRegistryError::ClaimHeldByAnotherNode(ref jid)))
                if *jid == bare_jid
        ),
        "a live foreign user owner must not be displaced: {result:?}"
    );
    assert_eq!(registry.ask(UserCount).await.expect("count"), 0);
}

#[tokio::test]
async fn test_register_refuses_live_foreign_user_actor_claim() {
    let registry = spawn_registry().await;
    let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
    let bare_jid = bare("foreign-register");
    let jid = full("foreign-register", "mobile");
    let entity = user_entity(&bare_jid);
    claim_store
        .acquire(&entity, &foreign_identity())
        .await
        .expect("foreign acquire");
    wire_claims(&registry, Arc::clone(&claim_store), this_identity()).await;

    let (tx, _rx) = outbound_channel();
    let result = registry
        .ask(RegisterUserResource {
            jid,
            entry: ConnectionEntry::new(tx),
        })
        .await;

    assert!(
        matches!(
            result,
            Err(SendError::HandlerError(UserRegistryError::ClaimHeldByAnotherNode(ref jid)))
                if *jid == bare_jid
        ),
        "register must fail closed when another live node owns the bare JID: {result:?}"
    );
    assert_eq!(registry.ask(UserCount).await.expect("count"), 0);
}

#[tokio::test]
async fn test_register_after_identity_rotation_reclaims_after_a_retryable_busy_reply() {
    let registry = spawn_registry().await;
    let claim_store = Arc::new(RecordingClaimStore::empty());
    let claim_store_trait: Arc<dyn ClaimStore> = claim_store.clone();
    let shared_identity = SharedNodeIdentity::new(this_identity());
    let bare_jid = bare("missed-fence");
    let jid = full("missed-fence", "phone");
    let entity = user_entity(&bare_jid);
    wire_shared_claims(&registry, claim_store_trait, shared_identity.clone()).await;

    let old_actor = registry
        .ask(GetOrCreateUser {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("create");

    shared_identity
        .rotate(NodeIdentity::new("node-this", "epoch-after-self-fence"))
        .await;
    claim_store.set_owner_lease_fresh(false);

    let (tx, _rx) = outbound_channel();
    let first_attempt = registry
        .ask(RegisterUserResource {
            jid: jid.clone(),
            entry: ConnectionEntry::new(tx),
        })
        .await;
    assert!(matches!(
        first_attempt,
        Err(SendError::HandlerError(UserRegistryError::UserActorBusy(ref failed_jid)))
            if *failed_jid == bare_jid
    ));

    tokio::time::sleep(STALE_RETIREMENT_RETRY_DELAY.saturating_mul(2)).await;
    let (retry_tx, _retry_rx) = outbound_channel();
    registry
        .ask(RegisterUserResource {
            jid: jid.clone(),
            entry: ConnectionEntry::new(retry_tx),
        })
        .await
        .expect("register after stale retirement");

    old_actor.wait_for_shutdown().await;
    let new_actor = registry
        .ask(GetUser {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("get")
        .expect("actor");
    assert_ne!(
        old_actor.id(),
        new_actor.id(),
        "registration must not reuse a UserActor whose local claim was minted by the pre-fence identity"
    );
    let resources = new_actor
        .ask(crate::registry::user_actor::GetResources)
        .await
        .expect("resources");
    assert_eq!(resources, vec![jid]);

    let snapshot = claim_store
        .current_claim(&entity)
        .await
        .expect("current_claim")
        .expect("claim");
    assert_eq!(
        snapshot.owner,
        NodeIdentity::new("node-this", "epoch-after-self-fence")
    );
    assert_eq!(snapshot.claim_epoch, ClaimEpoch(1));
    assert!(
        claim_store.release_owners().is_empty(),
        "stale-entry demotion must not release a claim after identity rotation"
    );
}

#[tokio::test]
async fn stale_multi_resource_retirement_returns_busy_then_allows_a_later_register() {
    let registry = spawn_registry().await;
    let claim_store = Arc::new(RecordingClaimStore::empty());
    let claim_store_trait: Arc<dyn ClaimStore> = claim_store.clone();
    let shared_identity = SharedNodeIdentity::new(this_identity());
    let bare_jid = bare("missed-live-fence");
    let old_jids = [
        full("missed-live-fence", "old-phone"),
        full("missed-live-fence", "old-laptop"),
        full("missed-live-fence", "old-tablet"),
    ];
    let new_jid = full("missed-live-fence", "new-phone");
    let entity = user_entity(&bare_jid);
    wire_shared_claims(&registry, claim_store_trait, shared_identity.clone()).await;

    let mut force_detach_receivers = Vec::new();
    for old_jid in &old_jids {
        let (old_tx, _old_rx) = outbound_channel();
        let old_entry = ConnectionEntry::new(old_tx);
        force_detach_receivers.push(
            old_entry
                .take_force_detach_rx()
                .expect("connection task owns the force-detach receiver"),
        );
        registry
            .ask(RegisterUserResource {
                jid: old_jid.clone(),
                entry: old_entry,
            })
            .await
            .expect("register old resource");
    }
    let old_actor = registry
        .ask(GetUser {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("get old")
        .expect("old actor");

    shared_identity
        .rotate(NodeIdentity::new("node-this", "epoch-after-self-fence"))
        .await;
    claim_store.set_owner_lease_fresh(false);

    let (new_tx, _new_rx) = outbound_channel();
    let first_attempt = tokio::time::timeout(
        std::time::Duration::from_millis(250),
        registry.ask(RegisterUserResource {
            jid: new_jid.clone(),
            entry: ConnectionEntry::new(new_tx),
        }),
    )
    .await
    .expect("stale retirement must reply within the normal registration budget");
    assert!(matches!(
        first_attempt,
        Err(SendError::HandlerError(UserRegistryError::UserActorBusy(jid))) if jid == bare_jid
    ));

    for force_detach_rx in &mut force_detach_receivers {
        let request = tokio::time::timeout(
            std::time::Duration::from_millis(250),
            force_detach_rx.recv(),
        )
        .await
        .expect("stale resource retirement should be queued")
        .expect("stale resource force-detach request");
        assert_eq!(request.requester_bare_jid, bare_jid);
        assert_eq!(
            request.origin,
            crate::registry::ForceDetachOrigin::RegistryStaleActorRetirement
        );
        // A connection acknowledges this origin before its normal cleanup can
        // re-enter the registry; the stale actor retirement itself owns the
        // eventual mirror demotion.
        let _ = request.ack.send(ForceDetachOutcome::NotPersisted);
    }

    tokio::time::timeout(
        std::time::Duration::from_millis(250),
        old_actor.wait_for_shutdown(),
    )
    .await
    .expect("stale actor should retire after queueing force-detach requests");

    registry
        .ask(RegisterUserResource {
            jid: new_jid.clone(),
            entry: ConnectionEntry::new(outbound_channel().0),
        })
        .await
        .expect("later retry registers after retirement converges");
    let new_actor = registry
        .ask(GetUser {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("get new")
        .expect("new actor");
    assert_ne!(old_actor.id(), new_actor.id());
    let resources = new_actor
        .ask(crate::registry::user_actor::GetResources)
        .await
        .expect("resources");
    assert_eq!(resources, vec![new_jid]);

    let snapshot = claim_store
        .current_claim(&entity)
        .await
        .expect("current_claim")
        .expect("claim");
    assert_eq!(
        snapshot.owner,
        NodeIdentity::new("node-this", "epoch-after-self-fence")
    );
    assert_eq!(snapshot.claim_epoch, ClaimEpoch(1));
    assert!(
        claim_store.release_owners().is_empty(),
        "stale-entry retirement must not release after identity rotation"
    );
}

#[tokio::test]
async fn stale_retirement_with_a_closed_force_detach_receiver_does_not_remain_busy() {
    let registry = spawn_registry().await;
    let claim_store = Arc::new(RecordingClaimStore::empty());
    let claim_store_trait: Arc<dyn ClaimStore> = claim_store.clone();
    let shared_identity = SharedNodeIdentity::new(this_identity());
    let bare_jid = bare("closed-stale-retirement");
    let old_jid = full("closed-stale-retirement", "old-phone");
    let new_jid = full("closed-stale-retirement", "new-phone");
    wire_shared_claims(&registry, claim_store_trait, shared_identity.clone()).await;

    let (old_tx, _old_rx) = outbound_channel();
    let old_entry = ConnectionEntry::new(old_tx);
    drop(
        old_entry
            .take_force_detach_rx()
            .expect("connection task owns the force-detach receiver"),
    );
    registry
        .ask(RegisterUserResource {
            jid: old_jid,
            entry: old_entry,
        })
        .await
        .expect("register old resource");

    shared_identity
        .rotate(NodeIdentity::new("node-this", "epoch-after-self-fence"))
        .await;
    claim_store.set_owner_lease_fresh(false);

    let first_attempt = registry
        .ask(RegisterUserResource {
            jid: new_jid.clone(),
            entry: ConnectionEntry::new(outbound_channel().0),
        })
        .await;
    assert!(matches!(
        first_attempt,
        Err(SendError::HandlerError(UserRegistryError::UserActorBusy(jid))) if jid == bare_jid
    ));

    tokio::time::sleep(STALE_RETIREMENT_RETRY_DELAY.saturating_mul(2)).await;
    registry
        .ask(RegisterUserResource {
            jid: new_jid.clone(),
            entry: ConnectionEntry::new(outbound_channel().0),
        })
        .await
        .expect("closed stale-retirement channel must converge instead of staying busy");

    let actor = registry
        .ask(GetUser {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("get user")
        .expect("new actor");
    assert_eq!(
        actor
            .ask(crate::registry::user_actor::GetResources)
            .await
            .expect("resources"),
        vec![new_jid]
    );
}

#[tokio::test]
async fn mixed_closed_and_live_stale_retirement_queues_the_live_detach_before_converging() {
    let registry = spawn_registry().await;
    let claim_store = Arc::new(RecordingClaimStore::empty());
    let claim_store_trait: Arc<dyn ClaimStore> = claim_store.clone();
    let shared_identity = SharedNodeIdentity::new(this_identity());
    let bare_jid = bare("mixed-stale-retirement");
    let closed_jid = full("mixed-stale-retirement", "a-closed");
    let live_jid = full("mixed-stale-retirement", "z-live");
    let new_jid = full("mixed-stale-retirement", "new-phone");
    wire_shared_claims(&registry, claim_store_trait, shared_identity.clone()).await;

    let (closed_tx, _closed_outbound_rx) = outbound_channel();
    let closed_entry = ConnectionEntry::new(closed_tx);
    drop(
        closed_entry
            .take_force_detach_rx()
            .expect("connection task owns the closed force-detach receiver"),
    );
    registry
        .ask(RegisterUserResource {
            jid: closed_jid,
            entry: closed_entry,
        })
        .await
        .expect("register closed old resource");

    let (live_tx, _live_outbound_rx) = outbound_channel();
    let live_entry = ConnectionEntry::new(live_tx);
    let mut live_force_detach_rx = live_entry
        .take_force_detach_rx()
        .expect("connection task owns the live force-detach receiver");
    registry
        .ask(RegisterUserResource {
            jid: live_jid,
            entry: live_entry,
        })
        .await
        .expect("register live old resource");

    let old_actor = registry
        .ask(GetUser {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("get old actor")
        .expect("old actor exists");

    shared_identity
        .rotate(NodeIdentity::new("node-this", "epoch-after-self-fence"))
        .await;
    claim_store.set_owner_lease_fresh(false);

    let first_attempt = registry
        .ask(RegisterUserResource {
            jid: new_jid.clone(),
            entry: ConnectionEntry::new(outbound_channel().0),
        })
        .await;
    assert!(matches!(
        first_attempt,
        Err(SendError::HandlerError(UserRegistryError::UserActorBusy(ref jid))) if jid == &bare_jid
    ));

    let request = tokio::time::timeout(
        std::time::Duration::from_millis(250),
        live_force_detach_rx.recv(),
    )
    .await
    .expect("live resource should still be detached during mixed stale retirement")
    .expect("live force-detach request");
    assert_eq!(request.requester_bare_jid, bare_jid);
    assert_eq!(
        request.origin,
        crate::registry::ForceDetachOrigin::RegistryStaleActorRetirement
    );
    // Ack-gated retirement (codex 1668 round): the actor is killed only
    // after the queued detach requests acknowledge (or their bounded wait
    // elapses), so the deposed socket cannot keep processing under a
    // replacement. Acknowledge as the socket would.
    request
        .ack
        .send(crate::registry::ForceDetachOutcome::NotPersisted)
        .expect("registry ack waiter listens");

    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        old_actor.wait_for_shutdown(),
    )
    .await
    .expect("stale actor should retire after every live/dead resource was handled");

    registry
        .ask(RegisterUserResource {
            jid: new_jid.clone(),
            entry: ConnectionEntry::new(outbound_channel().0),
        })
        .await
        .expect("replacement register succeeds after mixed stale retirement converges");
}

#[tokio::test]
async fn stale_retirement_scans_past_closed_receivers_before_converging() {
    let bare_jid = bare("mixed-stale-retirement");
    let actor_ref = UserActor::spawn(UserActor::new(bare_jid.clone()));
    let closed_jid = full("mixed-stale-retirement", "a-closed");
    let live_jid = full("mixed-stale-retirement", "z-live");

    let (closed_tx, _closed_outbound_rx) = outbound_channel();
    let closed_entry = ConnectionEntry::new(closed_tx);
    drop(
        closed_entry
            .take_force_detach_rx()
            .expect("connection task owns the closed force-detach receiver"),
    );

    let (live_tx, _live_outbound_rx) = outbound_channel();
    let live_entry = ConnectionEntry::new(live_tx);
    let mut live_force_detach_rx = live_entry
        .take_force_detach_rx()
        .expect("connection task owns the live force-detach receiver");

    let registry = UserRegistryActor::new();
    let entry = UserEntry {
        actor_ref,
        claim: UserClaimLease {
            owner: this_identity(),
            epoch: ClaimEpoch(1),
        },
        resources: HashMap::from([(closed_jid, closed_entry), (live_jid.clone(), live_entry)]),
    };

    assert!(matches!(
        registry.issue_stale_retirement_force_detach(&bare_jid, &entry),
        (StaleRetirementQueueResult::Queued, _)
    ));

    let request = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        live_force_detach_rx.recv(),
    )
    .await
    .expect("live resource should still be scanned after a closed predecessor")
    .expect("live force-detach request");
    assert_eq!(request.requester_bare_jid, bare_jid);
    assert_eq!(
        request.origin,
        crate::registry::ForceDetachOrigin::RegistryStaleActorRetirement
    );
}

#[tokio::test]
async fn kicked_stale_retirement_front_runs_the_active_registration_in_the_first_turn() {
    let registry = spawn_registry().await;
    let claim_store = Arc::new(RecordingClaimStore::empty());
    let claim_store_trait: Arc<dyn ClaimStore> = claim_store.clone();
    let shared_identity = SharedNodeIdentity::new(this_identity());
    wire_shared_claims(&registry, claim_store_trait, shared_identity.clone()).await;

    let background_bares = (0..2)
        .map(|index| bare(&format!("background-stale-{index}")))
        .collect::<Vec<_>>();
    let target_bare = bare("actively-retrying-stale");

    let mut background_actors = Vec::new();
    for (index, bare_jid) in background_bares.iter().enumerate() {
        let old_jid = full(&format!("background-stale-{index}"), "old-phone");
        let (old_tx, _old_outbound_rx) = outbound_channel();
        let old_entry = ConnectionEntry::new(old_tx);
        drop(
            old_entry
                .take_force_detach_rx()
                .expect("connection task owns the closed force-detach receiver"),
        );
        registry
            .ask(RegisterUserResource {
                jid: old_jid.clone(),
                entry: old_entry,
            })
            .await
            .expect("register stale background resource");
        background_actors.push(
            registry
                .ask(GetUser {
                    bare_jid: bare_jid.clone(),
                })
                .await
                .expect("get background actor")
                .expect("background actor exists"),
        );
    }

    let target_old_jid = full("actively-retrying-stale", "old-phone");
    let (target_old_tx, _target_old_outbound_rx) = outbound_channel();
    let target_old_entry = ConnectionEntry::new(target_old_tx);
    drop(
        target_old_entry
            .take_force_detach_rx()
            .expect("connection task owns the closed target force-detach receiver"),
    );
    registry
        .ask(RegisterUserResource {
            jid: target_old_jid,
            entry: target_old_entry,
        })
        .await
        .expect("register stale target resource");
    let target_old_actor = registry
        .ask(GetUser {
            bare_jid: target_bare.clone(),
        })
        .await
        .expect("get target actor")
        .expect("target actor exists");

    shared_identity
        .rotate(NodeIdentity::new("node-this", "epoch-after-self-fence"))
        .await;
    claim_store.set_owner_lease_fresh(false);

    for (index, bare_jid) in background_bares.iter().enumerate() {
        let new_jid = full(&format!("background-stale-{index}"), "new-phone");
        let first_attempt = registry
            .ask(RegisterUserResource {
                jid: new_jid,
                entry: ConnectionEntry::new(outbound_channel().0),
            })
            .await;
        assert!(matches!(
            first_attempt,
            Err(SendError::HandlerError(UserRegistryError::UserActorBusy(ref jid))) if jid == bare_jid
        ));
    }

    let target_new_jid = full("actively-retrying-stale", "new-phone");
    let first_target_attempt = registry
        .ask(RegisterUserResource {
            jid: target_new_jid.clone(),
            entry: ConnectionEntry::new(outbound_channel().0),
        })
        .await;
    assert!(matches!(
        first_target_attempt,
        Err(SendError::HandlerError(UserRegistryError::UserActorBusy(ref jid))) if jid == &target_bare
    ));

    let second_target_attempt = registry
        .ask(RegisterUserResource {
            jid: target_new_jid.clone(),
            entry: ConnectionEntry::new(outbound_channel().0),
        })
        .await;
    assert!(matches!(
        second_target_attempt,
        Err(SendError::HandlerError(UserRegistryError::UserActorBusy(ref jid))) if jid == &target_bare
    ));

    tokio::time::sleep(STALE_RETIREMENT_RETRY_DELAY + Duration::from_millis(40)).await;
    let retired_backgrounds = background_actors
        .iter()
        .filter(|actor| !actor.is_alive())
        .count();
    assert_eq!(
        retired_backgrounds, 0,
        "the first stale-retirement turn must spend its only slot on the kicked bare JID"
    );
    assert!(
        !target_old_actor.is_alive(),
        "the actively retried bare JID should retire in the first bounded stale-retirement turn"
    );

    let success = tokio::time::timeout(
        std::time::Duration::from_millis(150),
        registry.ask(RegisterUserResource {
            jid: target_new_jid.clone(),
            entry: ConnectionEntry::new(outbound_channel().0),
        }),
    )
    .await
    .expect("target retry should finish within the resumable registration backoff window");
    assert!(
        success.is_ok(),
        "the kicked bare JID should register once the first stale-retirement turn retires it"
    );
}

#[test]
fn prioritized_stale_retirement_selection_front_runs_requested_jid() {
    let mut registry = UserRegistryActor::new();
    let mut all = Vec::new();
    for index in 0..3 {
        let bare_jid = bare(&format!("pending-stale-{index}"));
        registry.pending_stale_retirements.insert(bare_jid.clone());
        all.push(bare_jid);
    }
    let target = all.last().cloned().expect("target stale jid");
    registry.prioritize_pending_stale_retirement(&target);

    let selected = registry
        .next_pending_stale_retirement()
        .expect("at least one pending stale retirement");
    assert_eq!(selected.bare_jid, target);
    registry
        .pending_stale_retirements
        .remove(&selected.bare_jid);
    assert!(
        registry.next_pending_stale_retirement().is_some(),
        "the selector should leave later stale bare JIDs for later turns"
    );
}

#[tokio::test]
async fn stale_retirement_retry_processes_only_one_pending_user_per_turn() {
    let mut registry = UserRegistryActor::new();
    registry.node_identity = SharedNodeIdentity::new(NodeIdentity::new("node-this", "fresh-epoch"));
    let stale_claim = UserClaimLease {
        owner: NodeIdentity::new("node-this", "stale-epoch"),
        epoch: ClaimEpoch(0),
    };
    let first_bare = bare("per-turn-stale-one");
    let second_bare = bare("per-turn-stale-two");
    let first_actor = UserActor::spawn(UserActor::new(first_bare.clone()));
    let second_actor = UserActor::spawn(UserActor::new(second_bare.clone()));
    for (bare_jid, actor_ref) in [
        (first_bare.clone(), first_actor.clone()),
        (second_bare.clone(), second_actor.clone()),
    ] {
        registry.users.insert(
            bare_jid.clone(),
            UserEntry {
                actor_ref,
                claim: stale_claim.clone(),
                resources: HashMap::new(),
            },
        );
        registry.pending_stale_retirements.insert(bare_jid);
    }

    let registry_ref = UserRegistryActor::spawn(UserRegistryActor::new());
    registry
        .retry_pending_stale_retirement_work(&registry_ref)
        .await;

    assert_eq!(
        registry.users.len(),
        1,
        "one retry turn must spend only one claim-validation/elapsed-time bound"
    );
    assert_eq!(
        registry.pending_stale_retirements.len(),
        1,
        "one pending stale bare JID must remain for the next retry turn"
    );
    assert!(
        registry.users.contains_key(&first_bare) || registry.users.contains_key(&second_bare),
        "one stale registry entry must remain for the next turn"
    );
    first_actor.kill();
    second_actor.kill();
}

#[tokio::test]
async fn already_absent_child_unregister_reconciles_the_parent_resource_mirror() {
    let registry = spawn_registry().await;
    wire_claims(
        &registry,
        Arc::new(InProcessClaimStore::new()),
        this_identity(),
    )
    .await;
    let first = full("already-absent-parent", "phone");
    let remaining = full("already-absent-parent", "laptop");
    let bare_jid = first.to_bare();

    for jid in [&first, &remaining] {
        let (tx, _rx) = outbound_channel();
        registry
            .ask(RegisterUserResource {
                jid: jid.clone(),
                entry: ConnectionEntry::new(tx),
            })
            .await
            .expect("register resource");
    }
    let actor = registry
        .ask(GetUser {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("get user")
        .expect("user actor");

    // Model an original unregister that committed in the child after its
    // caller timed out. Its retry must prune the parent mirror as well.
    assert_eq!(
        actor
            .ask(
                crate::registry::user_actor::UnregisterConnectionAndReportEmpty {
                    jid: first.clone(),
                    owner: None,
                }
            )
            .await
            .expect("child unregister"),
        crate::registry::user_actor::UnregisterConnectionOutcome::Removed { is_empty: false }
    );
    assert_eq!(
        registry
            .ask(UnregisterAndReleaseIfEmpty {
                jid: first.clone(),
                owner: None,
            })
            .await
            .expect("retry unregister"),
        UnregisterAndReleaseOutcome::RetainedLiveResources
    );

    let demoted = registry
        .ask(DemoteUserActorIfOwner {
            bare_jid,
            owner: this_identity(),
        })
        .await
        .expect("demote mirrored user")
        .expect("matching owner");
    assert_eq!(demoted.resources.len(), 1);
    assert_eq!(demoted.resources[0].jid, remaining);
}

#[tokio::test(flavor = "current_thread")]
async fn test_register_claim_validation_error_does_not_force_detach_or_remove_live_actor() {
    let spans = crate::telemetry::test_support::acquire_spans();
    let registry = spawn_registry().await;
    let claim_store = Arc::new(RecordingClaimStore::empty());
    let claim_store_trait: Arc<dyn ClaimStore> = claim_store.clone();
    let bare_jid = bare("validation-outage");
    let old_jid = full("validation-outage", "old-phone");
    let new_jid = full("validation-outage", "new-phone");
    wire_claims(&registry, claim_store_trait, this_identity()).await;

    let (old_tx, _old_rx) = outbound_channel();
    let old_entry = ConnectionEntry::new(old_tx);
    let mut force_detach_rx = old_entry
        .take_force_detach_rx()
        .expect("connection task owns the force-detach receiver");
    registry
        .ask(RegisterUserResource {
            jid: old_jid.clone(),
            entry: old_entry,
        })
        .await
        .expect("register old");
    let old_actor = registry
        .ask(GetUser {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("get old")
        .expect("old actor");

    claim_store.set_fence_errors(true);

    let (new_tx, _new_rx) = outbound_channel();
    let result = registry
        .ask(RegisterUserResource {
            jid: new_jid,
            entry: ConnectionEntry::new(new_tx),
        })
        .await;
    assert!(
        matches!(
            result,
            Err(SendError::HandlerError(UserRegistryError::ClaimUnavailable(ref jid)))
                if *jid == bare_jid
        ),
        "claim validation outage must fail closed without retiring the actor: {result:?}"
    );
    assert!(matches!(
        force_detach_rx.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));
    let still_registered = registry
        .ask(GetUserForLocalClaim {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("get local claim actor");
    assert_eq!(
        still_registered.map(|actor| actor.id()),
        Some(old_actor.id()),
        "validation outage must not remove the live actor from the registry"
    );
    let resources = old_actor
        .ask(crate::registry::user_actor::GetResources)
        .await
        .expect("resources");
    assert_eq!(resources, vec![old_jid]);
    assert!(old_actor.is_alive());
    assert!(claim_store.release_owners().is_empty());
    assert!(
        spans.has_error_status("xmpp.user_registry.validate_claim"),
        "claim validation failure must export at least one ERROR operation span"
    );
}

#[tokio::test]
async fn test_demote_user_actor_hard_kills_without_releasing_the_claim() {
    let registry = spawn_registry().await;
    let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
    let bare_jid = bare("demoted");
    let entity = user_entity(&bare_jid);
    wire_claims(&registry, Arc::clone(&claim_store), this_identity()).await;

    let actor = registry
        .ask(GetOrCreateUser {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("get_or_create");
    assert!(actor.is_alive());

    let demoted = registry
        .ask(DemoteUserActor {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("demote");
    assert!(demoted);
    actor.wait_for_shutdown().await;

    let lookup = registry
        .ask(GetUser {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("get_user after demote");
    assert!(
        lookup.is_none(),
        "demote forgets the local registry entry without poisoning the user"
    );
    assert!(
        claim_store
            .current_claim(&entity)
            .await
            .expect("current_claim")
            .is_some(),
        "demotion must not release a claim that may already belong to a new owner"
    );
}

#[tokio::test]
async fn test_list_users() {
    let registry = spawn_registry().await;

    registry
        .ask(GetOrCreateUser {
            bare_jid: bare("alice"),
        })
        .await
        .expect("ask failed");

    registry
        .ask(GetOrCreateUser {
            bare_jid: bare("bob"),
        })
        .await
        .expect("ask failed");

    let mut users = registry.ask(ListUsers).await.expect("ask failed");

    users.sort_by_key(|a| a.to_string());

    assert_eq!(users.len(), 2);
    assert_eq!(users[0], bare("alice"));
    assert_eq!(users[1], bare("bob"));
}

#[tokio::test]
async fn test_user_count() {
    let registry = spawn_registry().await;

    let count = registry.ask(UserCount).await.expect("ask failed");
    assert_eq!(count, 0);

    registry
        .ask(GetOrCreateUser {
            bare_jid: bare("alice"),
        })
        .await
        .expect("ask failed");

    let count = registry.ask(UserCount).await.expect("ask failed");
    assert_eq!(count, 1);
}

#[tokio::test]
async fn test_different_users_get_different_actors() {
    let registry = spawn_registry().await;

    let alice = registry
        .ask(GetOrCreateUser {
            bare_jid: bare("alice"),
        })
        .await
        .expect("ask failed");

    let bob = registry
        .ask(GetOrCreateUser {
            bare_jid: bare("bob"),
        })
        .await
        .expect("ask failed");

    assert_ne!(alice.id(), bob.id());
}

#[tokio::test]
async fn test_get_or_create_fails_fast_for_dead_actor_until_explicit_cleanup() {
    let registry = spawn_registry().await;
    let bare_jid = bare("restart");

    let first = registry
        .ask(GetOrCreateUser {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("ask failed");
    first.kill();
    tokio::task::yield_now().await;

    let result = registry
        .ask(GetOrCreateUser {
            bare_jid: bare_jid.clone(),
        })
        .await;
    assert!(matches!(
        result,
        Err(SendError::HandlerError(UserRegistryError::UserActorStateLost(jid)))
            if jid == bare_jid
    ));

    let removed = registry
        .ask(RemoveUser {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("remove should clear poisoned user");
    assert!(removed);

    let recreated = registry
        .ask(GetOrCreateUser { bare_jid })
        .await
        .expect("actor should be recreated after explicit cleanup");
    assert!(recreated.is_alive());
}

#[tokio::test]
async fn test_dead_actor_detection_releases_user_actor_claim() {
    let registry = spawn_registry().await;
    let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
    let bare_jid = bare("dead-claim");
    let entity = user_entity(&bare_jid);
    wire_claims(&registry, Arc::clone(&claim_store), this_identity()).await;

    let actor = registry
        .ask(GetOrCreateUser {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("create");
    assert!(claim_store
        .current_claim(&entity)
        .await
        .expect("current_claim")
        .is_some());

    actor.kill();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while actor.is_alive() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "actor did not die in time"
        );
        tokio::task::yield_now().await;
    }

    let result = registry
        .ask(GetUser {
            bare_jid: bare_jid.clone(),
        })
        .await;
    assert!(matches!(
        result,
        Err(SendError::HandlerError(UserRegistryError::UserActorStateLost(jid)))
            if jid == bare_jid
    ));

    assert!(claim_store
        .current_claim(&entity)
        .await
        .expect("current_claim")
        .is_none());
    claim_store
        .acquire(&entity, &foreign_identity())
        .await
        .expect("dead-actor detection releases the claim for another node");
}

#[tokio::test]
async fn test_unregister_and_register_are_serialized_without_user_loss() {
    let registry = spawn_registry().await;
    let bare_jid = bare("alice");
    let phone = full("alice", "phone");
    let laptop = full("alice", "laptop");

    let (phone_tx, _phone_rx) = outbound_channel();
    let (laptop_tx, _laptop_rx) = outbound_channel();

    registry
        .ask(RegisterUserResource {
            jid: phone.clone(),
            entry: ConnectionEntry::new(phone_tx),
        })
        .await
        .expect("register phone");

    let unregister = registry.ask(UnregisterUserResource {
        jid: phone,
        owner: None,
    });
    let register = registry.ask(RegisterUserResource {
        jid: laptop.clone(),
        entry: ConnectionEntry::new(laptop_tx),
    });
    let (unregister_done, register_done) = tokio::join!(unregister, register);
    unregister_done.expect("unregister");
    register_done.expect("register replacement");

    let user_actor = registry
        .ask(GetUser {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("get user")
        .expect("user actor should still exist with replacement resource");
    let resources = user_actor
        .ask(crate::registry::user_actor::GetResources)
        .await
        .expect("resources");
    assert_eq!(resources, vec![laptop]);

    let count = registry.ask(UserCount).await.expect("count");
    assert_eq!(count, 1);
}

fn sample_stanza(to: &FullJid) -> crate::Stanza {
    let mut msg = xmpp_parsers::message::Message::new(Some(jid::Jid::from(to.clone())));
    msg.type_ = xmpp_parsers::message::MessageType::Chat;
    msg.bodies
        .insert(xmpp_parsers::message::Lang::new(), "hi".to_string());
    crate::Stanza::Message(msg)
}

/// ADR-0017 Phase 1 Slice 2 (Copilot review on PR #1177): the delivery cutover
/// makes `try_deliver`'s closed-channel eviction reachable in production. When
/// it removes a user's *last* resource, the actor is left empty but still
/// registered (the explicit unregister-prune path did not run). The reaper must
/// remove such an orphaned empty actor.
#[tokio::test]
async fn test_reap_user_if_empty_removes_orphaned_empty_actor() {
    use crate::registry::connection_registry::BroadcastOutcome;
    use crate::registry::TrySendPeer;

    let registry = spawn_registry().await;
    let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
    let bare_jid = bare("alice");
    let phone = full("alice", "phone");
    let entity = user_entity(&bare_jid);
    wire_claims(&registry, Arc::clone(&claim_store), this_identity()).await;

    let (phone_tx, phone_rx) = outbound_channel();
    registry
        .ask(RegisterUserResource {
            jid: phone.clone(),
            entry: ConnectionEntry::new(phone_tx),
        })
        .await
        .expect("register phone");

    // Close the channel, then drive one delivery so `try_deliver` evicts the
    // last resource — exactly the production path that orphans an empty actor.
    drop(phone_rx);
    let user_actor = registry
        .ask(GetUser {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("get user")
        .expect("actor exists");
    let outcome = user_actor
        .ask(TrySendPeer {
            jid: phone.clone(),
            stanza: sample_stanza(&phone),
        })
        .await
        .expect("try send");
    assert_eq!(outcome, BroadcastOutcome::DroppedClosed);

    // The actor is now empty but still registered.
    assert_eq!(registry.ask(UserCount).await.expect("count"), 1);

    let reaped = registry
        .ask(ReapUserIfEmpty {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("reap");
    assert!(reaped, "an empty orphaned actor must be reaped");
    assert_eq!(registry.ask(UserCount).await.expect("count"), 0);
    assert!(registry
        .ask(GetUser { bare_jid })
        .await
        .expect("get user")
        .is_none());
    assert!(claim_store
        .current_claim(&entity)
        .await
        .expect("current_claim")
        .is_none());
}

/// The reaper must never remove a user that still has a live resource — the
/// race the atomic check-and-remove guards against.
#[tokio::test]
async fn test_reap_user_if_empty_keeps_nonempty_actor() {
    let registry = spawn_registry().await;
    let bare_jid = bare("alice");
    let phone = full("alice", "phone");

    let (phone_tx, _phone_rx) = outbound_channel();
    registry
        .ask(RegisterUserResource {
            jid: phone.clone(),
            entry: ConnectionEntry::new(phone_tx),
        })
        .await
        .expect("register phone");

    let reaped = registry
        .ask(ReapUserIfEmpty {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("reap");
    assert!(!reaped, "a user with a live resource must not be reaped");
    assert_eq!(registry.ask(UserCount).await.expect("count"), 1);
    let resources = registry
        .ask(GetUser {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("get user")
        .expect("actor still present")
        .ask(crate::registry::user_actor::GetResources)
        .await
        .expect("resources");
    assert_eq!(resources, vec![phone]);
}

/// Reaping an unknown bare JID is a no-op that reports nothing reaped.
#[tokio::test]
async fn test_reap_user_if_empty_absent_is_false() {
    let registry = spawn_registry().await;
    let reaped = registry
        .ask(ReapUserIfEmpty {
            bare_jid: bare("ghost"),
        })
        .await
        .expect("reap");
    assert!(!reaped);
    assert_eq!(registry.ask(UserCount).await.expect("count"), 0);
}

/// A dead `UserActor` is a state-lost condition, not an empty one: the reaper
/// must fold it into the poison path (so `poisoned_users` stays the single
/// source of dead-actor truth) and report nothing reaped, leaving
/// `GetOrCreateUser` failing fast until explicit cleanup.
#[tokio::test]
async fn test_reap_user_if_empty_poisons_dead_actor() {
    let registry = spawn_registry().await;
    let bare_jid = bare("restart");

    let actor = registry
        .ask(GetOrCreateUser {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("create");
    actor.kill();
    tokio::task::yield_now().await;

    let reaped = registry
        .ask(ReapUserIfEmpty {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("reap");
    assert!(!reaped, "a dead actor is poisoned, not reaped");

    // The dead actor is now poisoned: GetOrCreateUser fails fast until cleanup.
    let result = registry
        .ask(GetOrCreateUser {
            bare_jid: bare_jid.clone(),
        })
        .await;
    assert!(matches!(
        result,
        Err(SendError::HandlerError(UserRegistryError::UserActorStateLost(jid)))
            if jid == bare_jid
    ));
}

/// A force-detach ask keeps the exact claim fence after a durable release
/// failure and the janitor retry clears it once the store recovers.
#[tokio::test]
async fn force_detach_release_failure_converges_through_registry_retry() {
    let registry = spawn_registry().await;
    let store = Arc::new(RecordingClaimStore::empty());
    store.set_fail_releases(true);
    let store_trait: Arc<dyn ClaimStore> = store.clone();
    wire_claims(&registry, store_trait, this_identity()).await;
    let jid = full("converge", "phone");
    let entity = user_entity(&jid.to_bare());
    let (tx, _rx) = outbound_channel();
    registry
        .ask(RegisterUserResource {
            jid: jid.clone(),
            entry: ConnectionEntry::new(tx),
        })
        .await
        .expect("register");

    let outcome = registry
        .ask(UnregisterAndReleaseIfEmpty { jid, owner: None })
        .await
        .expect("force detach ask");
    assert_eq!(outcome, UnregisterAndReleaseOutcome::Released);
    assert!(store.current_claim(&entity).await.expect("claim").is_some());

    store.set_fail_releases(false);
    assert_eq!(
        registry
            .ask(RetryUserRegistryConvergence)
            .await
            .expect("retry"),
        (0, 0)
    );
    assert!(store.current_claim(&entity).await.expect("claim").is_none());
}

/// `ensure_claimed` can self-reacquire with the same epoch.  A terminal
/// retry therefore uses the live registry entry, rather than the epoch, as
/// its liveness fence.
#[tokio::test]
async fn terminal_release_retry_retains_same_node_reacquisition() {
    let registry = spawn_registry().await;
    let store = Arc::new(RecordingClaimStore::empty());
    store.set_fail_releases(true);
    let store_trait: Arc<dyn ClaimStore> = store.clone();
    wire_claims(&registry, store_trait, this_identity()).await;
    let jid = full("reacquire", "phone");
    let bare_jid = jid.to_bare();
    let entity = user_entity(&bare_jid);
    let (tx, _rx) = outbound_channel();
    registry
        .ask(RegisterUserResource {
            jid: jid.clone(),
            entry: ConnectionEntry::new(tx),
        })
        .await
        .expect("register");
    registry
        .ask(UnregisterAndReleaseIfEmpty { jid, owner: None })
        .await
        .expect("remove");

    store.set_fail_releases(false);
    registry
        .ask(GetOrCreateUser { bare_jid })
        .await
        .expect("same-node reacquire");
    registry
        .ask(RetryUserRegistryConvergence)
        .await
        .expect("retry");
    assert!(store.current_claim(&entity).await.expect("claim").is_some());
}

#[tokio::test(start_paused = true)]
async fn reacquire_waits_for_a_timed_out_release_to_converge() {
    let registry = spawn_registry().await;
    let store = Arc::new(RecordingClaimStore::empty());
    let store_trait: Arc<dyn ClaimStore> = store.clone();
    wire_claims(&registry, store_trait, this_identity()).await;
    let jid = full("late-release", "phone");
    let bare_jid = jid.to_bare();
    let entity = user_entity(&bare_jid);
    let (tx, _rx) = outbound_channel();
    registry
        .ask(RegisterUserResource {
            jid: jid.clone(),
            entry: ConnectionEntry::new(tx),
        })
        .await
        .expect("register");

    store.detach_next_release();
    let unregister = tokio::spawn({
        let registry = registry.clone();
        let jid = jid.clone();
        async move {
            registry
                .ask(UnregisterAndReleaseIfEmpty { jid, owner: None })
                .await
        }
    });
    store.late_release_started.notified().await;
    tokio::time::advance(CLAIM_RELEASE_TIMEOUT + Duration::from_millis(1)).await;
    assert!(matches!(
        unregister.await.expect("unregister task"),
        Ok(UnregisterAndReleaseOutcome::Released)
    ));
    let _stale_claim = store
        .current_claim(&entity)
        .await
        .expect("stale claim lookup")
        .expect("timed-out release still owns the claim");

    let reacquire = tokio::spawn({
        let registry = registry.clone();
        let bare_jid = bare_jid.clone();
        async move { registry.ask(GetOrCreateUser { bare_jid }).await }
    });
    tokio::time::advance(CLAIM_RELEASE_TIMEOUT + Duration::from_millis(1)).await;
    assert!(matches!(
        reacquire.await.expect("reacquire task"),
        Err(SendError::HandlerError(UserRegistryError::ClaimUnavailable(ref jid)))
            if *jid == bare_jid
    ));

    store.allow_late_release.notify_one();
    store.late_release_completed.notified().await;
    let actor = registry
        .ask(GetOrCreateUser {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("reacquire after convergence");
    let live_claim = store
        .current_claim(&entity)
        .await
        .expect("live claim lookup")
        .expect("fresh claim exists");
    assert!(actor.is_alive());
    assert!(store
        .fence(&entity, &live_claim.owner, live_claim.claim_epoch)
        .await
        .expect("fresh claim survives the converged delete"));
}

#[tokio::test]
async fn terminal_release_retry_times_out_without_blocking_the_janitor_turn() {
    let registry = spawn_registry().await;
    let store = Arc::new(RecordingClaimStore::empty());
    let store_trait: Arc<dyn ClaimStore> = store.clone();
    wire_claims(&registry, store_trait, this_identity()).await;
    let jid = full("timeout-retry", "phone");
    let entity = user_entity(&jid.to_bare());
    let (tx, _rx) = outbound_channel();
    registry
        .ask(RegisterUserResource {
            jid: jid.clone(),
            entry: ConnectionEntry::new(tx),
        })
        .await
        .expect("register");

    store.set_fail_releases(true);
    registry
        .ask(UnregisterAndReleaseIfEmpty {
            jid: jid.clone(),
            owner: None,
        })
        .await
        .expect("force detach");
    assert!(store.current_claim(&entity).await.expect("claim").is_some());

    store.set_fail_releases(false);
    store.block_next_release();
    assert_eq!(
        tokio::time::timeout(
            CLAIM_RELEASE_TIMEOUT + Duration::from_millis(150),
            registry.ask(RetryUserRegistryConvergence),
        )
        .await
        .expect("janitor retry must stop waiting once claim release times out")
        .expect("retry result"),
        (0, 1),
        "timed-out janitor retries must preserve the terminal release backlog"
    );
    store.release_entered.notified().await;
    assert!(store.current_claim(&entity).await.expect("claim").is_some());

    assert_eq!(
        registry
            .ask(RetryUserRegistryConvergence)
            .await
            .expect("follow-up retry"),
        (0, 0)
    );
    assert!(store.current_claim(&entity).await.expect("claim").is_none());
}

#[tokio::test]
async fn force_detach_retains_user_claim_when_other_resources_are_live() {
    let registry = spawn_registry().await;
    let phone = full("retain", "phone");
    let laptop = full("retain", "laptop");
    let (phone_tx, _phone_rx) = outbound_channel();
    let (laptop_tx, _laptop_rx) = outbound_channel();
    for (jid, entry) in [
        (phone.clone(), ConnectionEntry::new(phone_tx)),
        (laptop, ConnectionEntry::new(laptop_tx)),
    ] {
        registry
            .ask(RegisterUserResource { jid, entry })
            .await
            .expect("register");
    }
    assert_eq!(
        registry
            .ask(UnregisterAndReleaseIfEmpty {
                jid: phone,
                owner: None,
            })
            .await
            .expect("unregister"),
        UnregisterAndReleaseOutcome::RetainedLiveResources
    );
    assert_eq!(registry.ask(UserCount).await.expect("count"), 1);
}

#[tokio::test]
async fn busy_force_detach_records_pending_unregister_and_retry_frees_slot() {
    let registry = spawn_registry().await;
    let jid = full("busy", "phone");
    let bare_jid = jid.to_bare();
    let (tx, _rx) = outbound_channel();
    registry
        .ask(RegisterUserResource {
            jid: jid.clone(),
            entry: ConnectionEntry::new(tx),
        })
        .await
        .expect("register");
    let actor = registry
        .ask(GetUser {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("get")
        .expect("actor");
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    actor
        .tell(GateUserActor {
            entered: entered.clone(),
            release: release.clone(),
        })
        .await
        .expect("queue gate");
    entered.notified().await;

    assert_eq!(
        registry
            .ask(UnregisterAndReleaseIfEmpty {
                jid: jid.clone(),
                owner: None,
            })
            .await
            .expect("typed result"),
        UnregisterAndReleaseOutcome::RetryableFailure(
            UnregisterAndReleaseRetryableFailure::UserActorBusy
        )
    );
    release.notify_one();
    tokio::task::yield_now().await;
    assert_eq!(
        registry
            .ask(RetryUserRegistryConvergence)
            .await
            .expect("retry"),
        (0, 0)
    );
    assert!(
        registry
            .ask(GetUser { bare_jid })
            .await
            .expect("get")
            .is_none(),
        "retry removed the stale full-JID owner slot"
    );
}

#[test]
fn duplicate_owner_gated_pending_unregister_uses_one_inventory_entry() {
    let mut registry = UserRegistryActor::new();
    let jid = full("dedupe-owner", "phone");
    let owner = Arc::new(AtomicBool::new(true));

    registry.remember_pending_unregister(jid.clone(), Some(owner.clone()));
    registry.remember_pending_unregister(jid, Some(owner));

    assert_eq!(registry.pending_unregisters.len(), 1);
}

#[tokio::test]
async fn busy_owner_gated_pending_unregister_allows_immediate_resumed_registration() {
    let registry = spawn_registry().await;
    let jid = full("busy-resume", "phone");
    let bare_jid = jid.to_bare();
    let (old_tx, _old_rx) = outbound_channel();
    let old_entry = ConnectionEntry::new(old_tx);
    let old_owner = old_entry.carbons_handle();
    registry
        .ask(RegisterUserResource {
            jid: jid.clone(),
            entry: old_entry,
        })
        .await
        .expect("register old resource");

    let actor = registry
        .ask(GetUser {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("get old actor")
        .expect("old actor");
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    actor
        .tell(GateUserActor {
            entered: entered.clone(),
            release: release.clone(),
        })
        .await
        .expect("queue gate");
    entered.notified().await;

    assert_eq!(
        registry
            .ask(UnregisterAndReleaseIfEmpty {
                jid: jid.clone(),
                owner: Some(old_owner),
            })
            .await
            .expect("typed result"),
        UnregisterAndReleaseOutcome::RetryableFailure(
            UnregisterAndReleaseRetryableFailure::UserActorBusy
        )
    );

    let (fresh_tx, _fresh_rx) = outbound_channel();
    let fresh_entry = ConnectionEntry::new(fresh_tx);
    let fresh_owner = fresh_entry.carbons_handle();
    release.notify_one();
    assert!(
        registry
            .ask(RegisterUserResourceIfOwnerOrAbsent {
                jid: jid.clone(),
                entry: fresh_entry,
                owner: fresh_owner.clone(),
            })
            .await
            .expect("immediate resumed registration"),
        "the inline pending-unregister drain must vacate the stale owner slot"
    );

    let fresh_actor = registry
        .ask(GetUser { bare_jid })
        .await
        .expect("get fresh actor")
        .expect("fresh actor");
    let registered = fresh_actor
        .ask(GetConnectionEntry { jid: jid.clone() })
        .await
        .expect("get fresh entry")
        .expect("fresh entry");
    assert!(Arc::ptr_eq(&registered.carbons_handle(), &fresh_owner));
    assert_eq!(registry.ask(UserCount).await.expect("count"), 1);
}

#[tokio::test]
async fn stale_owner_gated_pending_unregister_preserves_fresh_same_resource() {
    let registry = spawn_registry().await;
    let jid = full("stale-pending", "phone");
    let (old_tx, _old_rx) = outbound_channel();
    let old_entry = ConnectionEntry::new(old_tx);
    let old_owner = old_entry.carbons_handle();
    registry
        .ask(RegisterUserResource {
            jid: jid.clone(),
            entry: old_entry,
        })
        .await
        .expect("register old resource");

    let (fresh_tx, _fresh_rx) = outbound_channel();
    let fresh_entry = ConnectionEntry::new(fresh_tx);
    let fresh_owner = fresh_entry.carbons_handle();
    assert!(!Arc::ptr_eq(&old_owner, &fresh_owner));
    registry
        .ask(RegisterUserResource {
            jid: jid.clone(),
            entry: fresh_entry.clone(),
        })
        .await
        .expect("register fresh resource");
    registry
        .ask(RecordPendingUserUnregister {
            jid: jid.clone(),
            owner: Some(old_owner),
        })
        .await
        .expect("record stale pending unregister");

    assert!(
        registry
            .ask(RegisterUserResourceIfOwnerOrAbsent {
                jid: jid.clone(),
                entry: fresh_entry,
                owner: fresh_owner.clone(),
            })
            .await
            .expect("re-register fresh owner"),
        "a stale owner-gated pending unregister must not evict the live owner"
    );

    let actor = registry
        .ask(GetUser {
            bare_jid: jid.to_bare(),
        })
        .await
        .expect("get actor")
        .expect("actor");
    let registered = actor
        .ask(GetConnectionEntry { jid })
        .await
        .expect("get fresh entry")
        .expect("fresh entry");
    assert!(Arc::ptr_eq(&registered.carbons_handle(), &fresh_owner));
}

#[tokio::test]
async fn retry_user_registry_convergence_batches_pending_unregister_retries_per_turn() {
    let registry = spawn_registry().await;
    let bare_jid = bare("retry-batch");
    let mut receivers = Vec::new();
    for resource in ["one", "two", "three"] {
        let jid = full("retry-batch", resource);
        let (tx, rx) = outbound_channel();
        receivers.push(rx);
        registry
            .ask(RegisterUserResource {
                jid: jid.clone(),
                entry: ConnectionEntry::new(tx),
            })
            .await
            .expect("register resource");
        registry
            .ask(RecordPendingUserUnregister { jid, owner: None })
            .await
            .expect("record pending unregister");
    }

    let actor = registry
        .ask(GetUser {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("get actor")
        .expect("actor");
    let entered = Arc::new(tokio::sync::Notify::new());
    let wedge = tokio::spawn({
        let actor = actor.clone();
        let entered = entered.clone();
        async move { actor.ask(WedgeUserActor { entered }).await }
    });
    entered.notified().await;

    assert_eq!(
        registry
            .ask(RetryUserRegistryConvergence)
            .mailbox_timeout(Duration::from_secs(5))
            .reply_timeout(Duration::from_secs(5))
            .await
            .expect("convergence retry should stay within the reaper ask budget"),
        (3, 0),
        "one turn should retry only a bounded batch and leave the remaining work queued"
    );
    assert_eq!(
        registry
            .ask(RetryUserRegistryConvergence)
            .mailbox_timeout(Duration::from_secs(5))
            .reply_timeout(Duration::from_secs(5))
            .await
            .expect("next sweep should see the preserved backlog"),
        (3, 0),
        "the retries that did not converge in the prior turn must remain queued for the next sweep"
    );

    actor.kill();
    assert!(wedge.await.expect("wedge task").is_err());
    drop(receivers);
}

#[tokio::test]
async fn pending_unregister_state_loss_does_not_recreate_the_user_actor() {
    let registry = spawn_registry().await;
    let jid = full("pending-state-loss", "phone");
    let bare_jid = jid.to_bare();
    let (old_tx, _old_rx) = outbound_channel();
    let old_entry = ConnectionEntry::new(old_tx);
    let old_owner = old_entry.carbons_handle();
    registry
        .ask(RegisterUserResource {
            jid: jid.clone(),
            entry: old_entry,
        })
        .await
        .expect("register old resource");
    registry
        .ask(RecordPendingUserUnregister {
            jid: jid.clone(),
            owner: Some(old_owner),
        })
        .await
        .expect("record pending unregister");

    let actor = registry
        .ask(GetUser {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("get actor")
        .expect("actor");
    actor.kill();
    tokio::task::yield_now().await;

    let (fresh_tx, _fresh_rx) = outbound_channel();
    let result = registry
        .ask(RegisterUserResource {
            jid,
            entry: ConnectionEntry::new(fresh_tx),
        })
        .await;
    assert!(matches!(
        result,
        Err(SendError::HandlerError(UserRegistryError::UserActorStateLost(jid)))
            if jid == bare_jid
    ));
    assert!(matches!(
        registry
            .ask(GetOrCreateUser { bare_jid: bare_jid.clone() })
            .await,
        Err(SendError::HandlerError(UserRegistryError::UserActorStateLost(jid)))
            if jid == bare_jid
    ));
}

/// State loss releases the user claim and removes the registry entry before
/// poisoning the JID.  A queued force-detach unregister is therefore already
/// discharged, and the janitor must consume it rather than retry it forever.
#[tokio::test]
async fn pending_unregister_state_loss_converges_without_retrying_forever() {
    let registry = spawn_registry().await;
    let jid = full("pending-state-loss-converges", "phone");
    let bare_jid = jid.to_bare();
    let (tx, _rx) = outbound_channel();
    registry
        .ask(RegisterUserResource {
            jid: jid.clone(),
            entry: ConnectionEntry::new(tx),
        })
        .await
        .expect("register old resource");
    registry
        .ask(RecordPendingUserUnregister {
            jid: jid.clone(),
            owner: None,
        })
        .await
        .expect("record pending unregister");

    let actor = registry
        .ask(GetUser {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("get actor")
        .expect("actor");
    actor.kill();
    tokio::task::yield_now().await;

    assert_eq!(
        registry
            .ask(UnregisterAndReleaseIfEmpty {
                jid: jid.clone(),
                owner: None,
            })
            .await
            .expect("state-loss detach"),
        UnregisterAndReleaseOutcome::AlreadyAbsent
    );
    assert_eq!(
        registry
            .ask(RetryUserRegistryConvergence)
            .await
            .expect("janitor retry"),
        (0, 0),
        "the poison-path cleanup drains queued unregisters"
    );
    assert_eq!(
        registry
            .ask(RetryUserRegistryConvergence)
            .await
            .expect("second janitor retry"),
        (0, 0),
        "no stale unregister remains to fail subsequent sweeps"
    );
    assert!(matches!(
        registry.ask(GetUser { bare_jid }).await,
        Err(SendError::HandlerError(
            UserRegistryError::UserActorStateLost(_)
        ))
    ));
}

/// The legacy tell operation and janitor reaper share the same exact-fence
/// terminal-release inventory as the synchronous force-detach ask.
#[tokio::test]
async fn legacy_unregister_and_reaper_release_failures_are_retried() {
    for use_reaper in [false, true] {
        let registry = spawn_registry().await;
        let store = Arc::new(RecordingClaimStore::empty());
        store.set_fail_releases(true);
        let store_trait: Arc<dyn ClaimStore> = store.clone();
        wire_claims(&registry, store_trait, this_identity()).await;
        let bare_jid = bare(if use_reaper {
            "reaper-release"
        } else {
            "legacy-release"
        });
        let entity = user_entity(&bare_jid);
        if use_reaper {
            registry
                .ask(GetOrCreateUser {
                    bare_jid: bare_jid.clone(),
                })
                .await
                .expect("create empty actor");
            assert!(registry
                .ask(ReapUserIfEmpty {
                    bare_jid: bare_jid.clone(),
                })
                .await
                .expect("reap"));
        } else {
            let jid = full("legacy-release", "phone");
            let (tx, _rx) = outbound_channel();
            registry
                .ask(RegisterUserResource {
                    jid: jid.clone(),
                    entry: ConnectionEntry::new(tx),
                })
                .await
                .expect("register");
            registry
                .ask(UnregisterUserResource { jid, owner: None })
                .await
                .expect("legacy unregister");
        }
        assert!(store.current_claim(&entity).await.expect("claim").is_some());
        store.set_fail_releases(false);
        registry
            .ask(RetryUserRegistryConvergence)
            .await
            .expect("retry");
        assert!(store.current_claim(&entity).await.expect("claim").is_none());
    }
}
