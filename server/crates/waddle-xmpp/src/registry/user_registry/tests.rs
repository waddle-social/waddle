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

struct HoldUserRegistry {
    entered: tokio::sync::oneshot::Sender<()>,
    release: Arc<tokio::sync::Notify>,
}

impl kameo::message::Message<HoldUserRegistry> for UserRegistryActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: HoldUserRegistry,
        _ctx: &mut kameo::message::Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let _ = msg.entered.send(());
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
    state: Mutex<Option<(NodeIdentity, ClaimEpoch, bool)>>,
    release_owners: Mutex<Vec<NodeIdentity>>,
    fence_errors: AtomicBool,
    steal_calls: AtomicUsize,
    block_ensure: AtomicBool,
    ensure_entered: tokio::sync::Notify,
    continue_ensure: tokio::sync::Notify,
}

impl RecordingClaimStore {
    fn empty() -> Self {
        Self {
            state: Mutex::new(None),
            release_owners: Mutex::new(Vec::new()),
            fence_errors: AtomicBool::new(false),
            steal_calls: AtomicUsize::new(0),
            block_ensure: AtomicBool::new(false),
            ensure_entered: tokio::sync::Notify::new(),
            continue_ensure: tokio::sync::Notify::new(),
        }
    }

    fn seeded(owner: NodeIdentity, epoch: ClaimEpoch, owner_lease_fresh: bool) -> Self {
        Self {
            state: Mutex::new(Some((owner, epoch, owner_lease_fresh))),
            release_owners: Mutex::new(Vec::new()),
            fence_errors: AtomicBool::new(false),
            steal_calls: AtomicUsize::new(0),
            block_ensure: AtomicBool::new(false),
            ensure_entered: tokio::sync::Notify::new(),
            continue_ensure: tokio::sync::Notify::new(),
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

    fn block_next_ensure(&self) {
        self.block_ensure.store(true, Ordering::SeqCst);
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
async fn queued_authority_registration_cannot_block_terminal_identity_disable() {
    let registry = spawn_registry().await;
    let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
    let identity = this_identity();
    let shared_identity = SharedNodeIdentity::new(identity.clone());
    wire_shared_claims(&registry, Arc::clone(&claim_store), shared_identity.clone()).await;

    let publication_guard = shared_identity
        .guard_if_current(&identity)
        .await
        .expect("active identity guard");
    let publication_permit = publication_guard.permit();

    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let release = Arc::new(tokio::sync::Notify::new());
    let blocker = tokio::spawn({
        let registry = registry.clone();
        let release = Arc::clone(&release);
        async move {
            registry
                .ask(HoldUserRegistry {
                    entered: entered_tx,
                    release,
                })
                .await
        }
    });
    entered_rx.await.expect("registry blocker entered");

    let jid = full("queued-permit", "phone");
    let bare_jid = jid.to_bare();
    let (tx, _rx) = outbound_channel();
    let registering = tokio::spawn({
        let registry = registry.clone();
        async move {
            registry
                .ask(RegisterUserResourceUnderAuthority {
                    jid,
                    entry: ConnectionEntry::new(tx),
                    publication_permit,
                })
                .await
        }
    });
    tokio::task::yield_now().await;
    drop(publication_guard);

    let disabled = tokio::time::timeout(Duration::from_millis(100), shared_identity.disable())
        .await
        .expect("a queued actor message must not retain the publication read lock");
    assert!(shared_identity.owns_disabled(&disabled));

    release.notify_one();
    blocker
        .await
        .expect("blocker task")
        .expect("blocker message");
    let result = registering.await.expect("registration task");
    assert!(
        matches!(
            result,
            Err(SendError::HandlerError(UserRegistryError::ClaimUnavailable(ref jid)))
                if *jid == bare_jid
        ),
        "the late queued registration must fail after terminal disable: {result:?}"
    );
    assert!(registry
        .ask(ListUsers)
        .await
        .expect("list users")
        .is_empty());
    assert!(
        claim_store
            .current_claim(&user_entity(&bare_jid))
            .await
            .expect("claim lookup")
            .is_none(),
        "a rejected late registration must not leak a UserActor claim"
    );
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
async fn test_register_after_identity_rotation_reclaims_before_reusing_stale_user_actor() {
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
    registry
        .ask(RegisterUserResource {
            jid: jid.clone(),
            entry: ConnectionEntry::new(tx),
        })
        .await
        .expect("register");

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
async fn test_register_after_identity_rotation_force_detaches_stale_actor_resources() {
    let registry = spawn_registry().await;
    let claim_store = Arc::new(RecordingClaimStore::empty());
    let claim_store_trait: Arc<dyn ClaimStore> = claim_store.clone();
    let shared_identity = SharedNodeIdentity::new(this_identity());
    let bare_jid = bare("missed-live-fence");
    let old_jid = full("missed-live-fence", "old-phone");
    let new_jid = full("missed-live-fence", "new-phone");
    let entity = user_entity(&bare_jid);
    wire_shared_claims(&registry, claim_store_trait, shared_identity.clone()).await;

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

    shared_identity
        .rotate(NodeIdentity::new("node-this", "epoch-after-self-fence"))
        .await;
    claim_store.set_owner_lease_fresh(false);

    let force_detach_task = tokio::spawn({
        let bare_jid = bare_jid.clone();
        async move {
            let request = force_detach_rx
                .recv()
                .await
                .expect("stale resource force-detach request");
            assert_eq!(request.requester_bare_jid, bare_jid);
            let _ = request.ack.send(ForceDetachOutcome::NotPersisted);
        }
    });
    let (new_tx, _new_rx) = outbound_channel();
    registry
        .ask(RegisterUserResource {
            jid: new_jid.clone(),
            entry: ConnectionEntry::new(new_tx),
        })
        .await
        .expect("register new after forced stale detach");

    force_detach_task.await.expect("force-detach task");
    old_actor.wait_for_shutdown().await;
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
async fn test_register_claim_validation_error_does_not_force_detach_or_remove_live_actor() {
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
