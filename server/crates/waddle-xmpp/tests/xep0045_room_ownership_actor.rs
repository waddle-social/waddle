//! XEP-0045 §7.2 admission fencing for a deposed room actor.
//!
//! A room incarnation whose durable ownership cannot be proven must not
//! admit an occupant or apply resolver-derived affiliation state. Otherwise
//! two owners could independently authorize the same XEP-0045 room.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use jid::BareJid;
use kameo::actor::{ActorRef, Spawn};
use kameo::error::SendError;
use waddle_xmpp::muc::affiliation::AffiliationEntry;
use waddle_xmpp::muc::durable::{
    DurableRoomState, MucDurableFuture, MucDurableStore, RoomClaimFenceContext,
};
use waddle_xmpp::muc::room_actor::{
    GetAffiliation, GetConfig, Join, JoinAffiliationGrant, JoinWithAffiliation, OccupantCount,
    ResolverAffiliationSyncOutcome, RestoreDurableRoomState, RoomActor, RoomActorError,
    RoomMutationError, SyncResolverAffiliation, UpdateConfig,
};
use waddle_xmpp::muc::room_registry_actor::{
    CreateRoom, DestroyRoomWithSnapshot, DestroyRoomWithSnapshotOutcome, GetOrCreateRoom,
    RoomCreation, RoomRegistryActor, RoomRegistryError, WireClusteringClaims,
};
use waddle_xmpp::muc::{MucRoom, RoomConfig, SubjectState};
use waddle_xmpp::ownership::{
    ClaimEpoch, ClaimStore, Entity, EntityType, InProcessClaimStore, NodeIdentity,
    SharedNodeIdentity,
};
use waddle_xmpp::xep::xep0421::{OccupantIdSecret, OCCUPANT_ID_SECRET_MIN_BYTES};
use waddle_xmpp::{Affiliation, Role, XmppError};

const OWNED: u8 = 0;
const DEPOSED: u8 = 1;
const UNCERTAIN: u8 = 2;

struct OwnershipStore {
    state: AtomicU8,
    restore: Option<DurableRoomState>,
    authoritative_fences: Mutex<HashMap<BareJid, RoomClaimFenceContext>>,
    observed_fences: Mutex<Vec<(BareJid, RoomClaimFenceContext)>>,
    block_next_config_save: AtomicBool,
    config_save_started: Option<Arc<tokio::sync::Notify>>,
    allow_config_save: Option<Arc<tokio::sync::Notify>>,
    block_next_delete: AtomicBool,
    delete_started: Option<Arc<tokio::sync::Notify>>,
    allow_delete: Option<Arc<tokio::sync::Notify>>,
}

impl OwnershipStore {
    fn new() -> Self {
        Self {
            state: AtomicU8::new(OWNED),
            restore: None,
            authoritative_fences: Mutex::new(HashMap::new()),
            observed_fences: Mutex::new(Vec::new()),
            block_next_config_save: AtomicBool::new(false),
            config_save_started: None,
            allow_config_save: None,
            block_next_delete: AtomicBool::new(false),
            delete_started: None,
            allow_delete: None,
        }
    }

    fn restoring(state: DurableRoomState) -> Self {
        Self {
            state: AtomicU8::new(OWNED),
            restore: Some(state),
            authoritative_fences: Mutex::new(HashMap::new()),
            observed_fences: Mutex::new(Vec::new()),
            block_next_config_save: AtomicBool::new(false),
            config_save_started: None,
            allow_config_save: None,
            block_next_delete: AtomicBool::new(false),
            delete_started: None,
            allow_delete: None,
        }
    }

    fn blocking_config_save_and_delete(
        config_save_started: Arc<tokio::sync::Notify>,
        allow_config_save: Arc<tokio::sync::Notify>,
        delete_started: Arc<tokio::sync::Notify>,
        allow_delete: Arc<tokio::sync::Notify>,
    ) -> Self {
        Self {
            state: AtomicU8::new(OWNED),
            restore: None,
            authoritative_fences: Mutex::new(HashMap::new()),
            observed_fences: Mutex::new(Vec::new()),
            block_next_config_save: AtomicBool::new(true),
            config_save_started: Some(config_save_started),
            allow_config_save: Some(allow_config_save),
            block_next_delete: AtomicBool::new(true),
            delete_started: Some(delete_started),
            allow_delete: Some(allow_delete),
        }
    }

    fn set(&self, state: u8) {
        self.state.store(state, Ordering::SeqCst);
    }

    fn observe_exact_fence(&self, room_jid: &BareJid, fence: &RoomClaimFenceContext) -> bool {
        self.observed_fences
            .lock()
            .expect("observed-fence lock")
            .push((room_jid.clone(), fence.clone()));

        let expected_entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        fence.entity == expected_entity
            && self
                .authoritative_fences
                .lock()
                .expect("authoritative-fence lock")
                .get(room_jid)
                == Some(fence)
    }

    fn take_observed_fences(&self) -> Vec<(BareJid, RoomClaimFenceContext)> {
        std::mem::take(&mut *self.observed_fences.lock().expect("observed-fence lock"))
    }
}

impl MucDurableStore for OwnershipStore {
    fn load_room_state_fenced<'a>(
        &'a self,
        room_jid: &'a BareJid,
        fence: &'a RoomClaimFenceContext,
    ) -> MucDurableFuture<'a, Option<DurableRoomState>> {
        let restore = self.restore.clone();
        let exact = self.observe_exact_fence(room_jid, fence);
        Box::pin(async move {
            if !exact {
                return Err(XmppError::internal(
                    "durable load used a stale or cross-room fence",
                ));
            }
            Ok(restore)
        })
    }

    fn save_config_fenced<'a>(
        &'a self,
        room_jid: &'a BareJid,
        _waddle_id: &'a str,
        _channel_id: &'a str,
        _config: &'a RoomConfig,
        fence: &'a RoomClaimFenceContext,
    ) -> MucDurableFuture<'a, ()> {
        let exact = self.observe_exact_fence(room_jid, fence);
        let block = self.block_next_config_save.swap(false, Ordering::SeqCst);
        let started = self.config_save_started.clone();
        let allow = self.allow_config_save.clone();
        Box::pin(async move {
            if !exact {
                return Err(XmppError::internal(
                    "config save used a stale or cross-room fence",
                ));
            }
            if block {
                started.expect("blocked save start signal").notify_one();
                allow.expect("blocked save release signal").notified().await;
            }
            Ok(())
        })
    }

    fn save_subject_fenced<'a>(
        &'a self,
        room_jid: &'a BareJid,
        _subject: Option<&'a SubjectState>,
        fence: &'a RoomClaimFenceContext,
    ) -> MucDurableFuture<'a, ()> {
        let exact = self.observe_exact_fence(room_jid, fence);
        Box::pin(async move {
            if !exact {
                return Err(XmppError::internal(
                    "subject save used a stale or cross-room fence",
                ));
            }
            Ok(())
        })
    }

    fn save_affiliation_fenced<'a>(
        &'a self,
        room_jid: &'a BareJid,
        _entry: &'a AffiliationEntry,
        fence: &'a RoomClaimFenceContext,
    ) -> MucDurableFuture<'a, ()> {
        let exact = self.observe_exact_fence(room_jid, fence);
        Box::pin(async move {
            if !exact {
                return Err(XmppError::internal(
                    "affiliation save used a stale or cross-room fence",
                ));
            }
            Ok(())
        })
    }

    fn delete_room_state_fenced<'a>(
        &'a self,
        room_jid: &'a BareJid,
        fence: &'a RoomClaimFenceContext,
    ) -> MucDurableFuture<'a, ()> {
        let exact = self.observe_exact_fence(room_jid, fence);
        let block = self.block_next_delete.swap(false, Ordering::SeqCst);
        let started = self.delete_started.clone();
        let allow = self.allow_delete.clone();
        Box::pin(async move {
            if !exact {
                return Err(XmppError::internal(
                    "delete used a stale or cross-room fence",
                ));
            }
            if block {
                started.expect("blocked delete start signal").notify_one();
                allow
                    .expect("blocked delete release signal")
                    .notified()
                    .await;
            }
            Ok(())
        })
    }

    fn record_claim_fence(&self, room_jid: &BareJid, fence: RoomClaimFenceContext) {
        assert_eq!(
            fence.entity,
            Entity::new(EntityType::RoomActor, room_jid.to_string()),
            "the test store must never install a cross-room authority"
        );
        self.authoritative_fences
            .lock()
            .expect("authoritative-fence lock")
            .insert(room_jid.clone(), fence);
    }

    fn check_exact_claim_fence<'a>(
        &'a self,
        room_jid: &'a BareJid,
        fence: &'a RoomClaimFenceContext,
    ) -> MucDurableFuture<'a, bool> {
        let state = self.state.load(Ordering::SeqCst);
        let exact = self.observe_exact_fence(room_jid, fence);
        Box::pin(async move {
            match state {
                OWNED => Ok(exact),
                DEPOSED => Ok(false),
                UNCERTAIN => Err(XmppError::internal("ownership proof unavailable")),
                _ => unreachable!("test ownership state"),
            }
        })
    }
}

fn claim_fence(room_jid: &BareJid, node_epoch: &str, epoch: i64) -> RoomClaimFenceContext {
    RoomClaimFenceContext::new(
        Entity::new(EntityType::RoomActor, room_jid.to_string()),
        NodeIdentity::new("test-node", node_epoch),
        ClaimEpoch(epoch),
    )
}

async fn spawn_room_with_fences(
    room_jid: BareJid,
    retained_fence: RoomClaimFenceContext,
    authoritative_fence: RoomClaimFenceContext,
) -> (ActorRef<RoomActor>, Arc<OwnershipStore>) {
    let room = MucRoom::new(
        room_jid.clone(),
        "waddle-1".to_string(),
        "channel-1".to_string(),
        RoomConfig::default(),
    );
    let secret = OccupantIdSecret::new(vec![7; OCCUPANT_ID_SECRET_MIN_BYTES])
        .expect("valid occupant-id secret");
    let actor = RoomActor::spawn(RoomActor::new(room, secret));
    let store = Arc::new(OwnershipStore::new());
    store.record_claim_fence(&room_jid, authoritative_fence);
    actor
        .ask(RestoreDurableRoomState {
            store: Arc::clone(&store) as Arc<dyn MucDurableStore>,
            claim_fence: retained_fence,
        })
        .await
        .expect("install durable ownership store");
    (actor, store)
}

async fn spawn_fenced_room() -> (ActorRef<RoomActor>, Arc<OwnershipStore>) {
    let room_jid: BareJid = "ownership@muc.example.com".parse().expect("valid room JID");
    let fence = claim_fence(&room_jid, "test-node-epoch", 1);
    spawn_room_with_fences(room_jid, fence.clone(), fence).await
}

fn resolver_join() -> JoinWithAffiliation {
    JoinWithAffiliation {
        sender_jid: "member@example.com/web".parse().expect("full JID"),
        nick: "member".to_string(),
        affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
        local_domain: "example.com".to_string(),
        admission_revision: 0,
    }
}

fn creator_join() -> JoinWithAffiliation {
    JoinWithAffiliation {
        sender_jid: "creator@example.com/web".parse().expect("full JID"),
        nick: "creator".to_string(),
        affiliation_grant: JoinAffiliationGrant::CreatorOwner,
        local_domain: "example.com".to_string(),
        admission_revision: 0,
    }
}

fn legacy_join() -> Join {
    Join {
        real_jid: "legacy@example.com/web".parse().expect("full JID"),
        nick: "legacy".to_string(),
        role: Role::Participant,
        affiliation: Affiliation::Member,
    }
}

#[tokio::test]
async fn deposed_room_refuses_xep0045_resolver_join() {
    let (actor, store) = spawn_fenced_room().await;
    store.set(DEPOSED);

    assert!(matches!(
        actor.ask(resolver_join()).await,
        Err(SendError::HandlerError(RoomActorError::RoomSealed))
    ));
    assert_eq!(actor.ask(OccupantCount).await.expect("occupant count"), 0);
}

#[tokio::test]
async fn uncertain_room_refuses_xep0045_resolver_join() {
    let (actor, store) = spawn_fenced_room().await;
    store.set(UNCERTAIN);

    assert!(matches!(
        actor.ask(resolver_join()).await,
        Err(SendError::HandlerError(
            RoomActorError::OwnershipUnavailable
        ))
    ));
    assert_eq!(actor.ask(OccupantCount).await.expect("occupant count"), 0);
}

#[tokio::test]
async fn deposed_room_refuses_xep0045_creator_join() {
    let (actor, store) = spawn_fenced_room().await;
    store.set(DEPOSED);

    assert!(matches!(
        actor.ask(creator_join()).await,
        Err(SendError::HandlerError(RoomActorError::RoomSealed))
    ));
}

#[tokio::test]
async fn uncertain_room_refuses_xep0045_creator_join() {
    let (actor, store) = spawn_fenced_room().await;
    store.set(UNCERTAIN);

    assert!(matches!(
        actor.ask(creator_join()).await,
        Err(SendError::HandlerError(
            RoomActorError::OwnershipUnavailable
        ))
    ));
}

#[tokio::test]
async fn deposed_room_refuses_legacy_xep0045_join() {
    let (actor, store) = spawn_fenced_room().await;
    store.set(DEPOSED);

    assert!(matches!(
        actor.ask(legacy_join()).await,
        Err(SendError::HandlerError(RoomActorError::RoomSealed))
    ));
    assert_eq!(actor.ask(OccupantCount).await.expect("occupant count"), 0);
}

#[tokio::test]
async fn uncertain_room_refuses_legacy_xep0045_join() {
    let (actor, store) = spawn_fenced_room().await;
    store.set(UNCERTAIN);

    assert!(matches!(
        actor.ask(legacy_join()).await,
        Err(SendError::HandlerError(
            RoomActorError::OwnershipUnavailable
        ))
    ));
    assert_eq!(actor.ask(OccupantCount).await.expect("occupant count"), 0);
}

#[tokio::test]
async fn deposed_room_refuses_resolver_affiliation_sync() {
    let (actor, store) = spawn_fenced_room().await;
    store.set(DEPOSED);
    let jid: BareJid = "member@example.com".parse().expect("bare JID");

    assert_eq!(
        actor
            .ask(SyncResolverAffiliation {
                jid: jid.clone(),
                affiliation: Affiliation::Member,
                expected_admission_revision: 0,
            })
            .await
            .expect("sync outcome"),
        ResolverAffiliationSyncOutcome::RoomSealed,
    );
    assert_eq!(
        actor
            .ask(GetAffiliation { jid })
            .await
            .expect("affiliation"),
        Affiliation::None,
    );
}

#[tokio::test]
async fn uncertain_room_returns_typed_resolver_sync_outcome() {
    let (actor, store) = spawn_fenced_room().await;
    store.set(UNCERTAIN);
    let jid: BareJid = "member@example.com".parse().expect("bare JID");

    assert_eq!(
        actor
            .ask(SyncResolverAffiliation {
                jid: jid.clone(),
                affiliation: Affiliation::Member,
                expected_admission_revision: 0,
            })
            .await
            .expect("sync outcome"),
        ResolverAffiliationSyncOutcome::OwnershipUnavailable,
    );
    assert_eq!(
        actor
            .ask(GetAffiliation { jid })
            .await
            .expect("affiliation"),
        Affiliation::None,
    );
}

#[tokio::test]
async fn replaced_room_actor_cannot_borrow_the_successors_fence() {
    let room_jid: BareJid = "replacement@muc.example.com".parse().expect("room JID");
    let old_fence = claim_fence(&room_jid, "old-incarnation", 1);
    let (actor, store) =
        spawn_room_with_fences(room_jid.clone(), old_fence.clone(), old_fence.clone()).await;
    store.take_observed_fences();

    let replacement_fence = claim_fence(&room_jid, "replacement-incarnation", 2);
    store.record_claim_fence(&room_jid, replacement_fence);
    let original = actor.ask(GetConfig).await.expect("original config");
    let mut attempted = original.clone();
    attempted.name = "must not apply".to_string();

    let result = actor.ask(UpdateConfig { config: attempted }).await;

    assert!(matches!(
        result,
        Err(SendError::HandlerError(RoomMutationError::NotOwner))
    ));
    assert_eq!(
        actor.ask(GetConfig).await.expect("config after rejection"),
        original,
        "the deposed actor must remain unchanged"
    );
    assert_eq!(
        store.take_observed_fences(),
        vec![(room_jid, old_fence)],
        "the actor must present its retained fence, never the cache's replacement fence"
    );
}

#[tokio::test]
async fn second_restore_cannot_transplant_an_actor_to_a_successor_fence() {
    let room_jid: BareJid = "restore-transplant@muc.example.com"
        .parse()
        .expect("room JID");
    let original_fence = claim_fence(&room_jid, "original-incarnation", 11);
    let (actor, original_store) = spawn_room_with_fences(
        room_jid.clone(),
        original_fence.clone(),
        original_fence.clone(),
    )
    .await;
    original_store.take_observed_fences();
    let original_config = actor.ask(GetConfig).await.expect("original config");

    let successor_fence = claim_fence(&room_jid, "successor-incarnation", 12);
    let successor_store = Arc::new(OwnershipStore::restoring(durable_existing_room()));
    successor_store.record_claim_fence(&room_jid, successor_fence.clone());
    actor
        .ask(RestoreDurableRoomState {
            store: Arc::clone(&successor_store) as Arc<dyn MucDurableStore>,
            claim_fence: successor_fence,
        })
        .await
        .expect("mismatched restore is handled fail-closed");

    assert!(
        successor_store.take_observed_fences().is_empty(),
        "the mismatched successor store must not receive a durable load"
    );
    assert!(
        original_store.take_observed_fences().is_empty(),
        "rejecting the transplant must not probe or replace either store"
    );

    // A retry carrying the actor's original exact tuple remains the only
    // accepted restore identity. It can reload the original store, but it
    // must not reopen the actor after the transplant attempt sealed it.
    actor
        .ask(RestoreDurableRoomState {
            store: Arc::clone(&original_store) as Arc<dyn MucDurableStore>,
            claim_fence: original_fence.clone(),
        })
        .await
        .expect("same-fence restore retry");
    assert_eq!(
        original_store.take_observed_fences(),
        vec![(room_jid.clone(), original_fence)],
        "only the actor's originally retained full fence remains acceptable"
    );
    assert!(successor_store.take_observed_fences().is_empty());

    let mut attempted = original_config.clone();
    attempted.name = "must remain sealed".to_string();
    assert!(matches!(
        actor.ask(UpdateConfig { config: attempted }).await,
        Err(SendError::HandlerError(RoomMutationError::NotOwner))
    ));
    assert_eq!(
        actor.ask(GetConfig).await.expect("config after rejection"),
        original_config,
        "a mismatched second restore must not install successor state or allow mutation"
    );
    assert!(matches!(
        actor.ask(resolver_join()).await,
        Err(SendError::HandlerError(RoomActorError::RoomSealed))
    ));
    assert_eq!(actor.ask(OccupantCount).await.expect("occupant count"), 0);
}

#[tokio::test]
async fn same_owner_and_epoch_cannot_make_a_cross_room_fence_valid() {
    let room_jid: BareJid = "room-a@muc.example.com".parse().expect("room A JID");
    let other_room: BareJid = "room-b@muc.example.com".parse().expect("room B JID");
    let authoritative_fence = claim_fence(&room_jid, "shared-incarnation", 7);
    let cross_room_fence = claim_fence(&other_room, "shared-incarnation", 7);
    let (actor, store) = spawn_room_with_fences(
        room_jid.clone(),
        cross_room_fence.clone(),
        authoritative_fence,
    )
    .await;
    store.take_observed_fences();
    let original = actor.ask(GetConfig).await.expect("original config");
    let mut attempted = original.clone();
    attempted.name = "must not cross rooms".to_string();

    let result = actor.ask(UpdateConfig { config: attempted }).await;

    assert!(matches!(
        result,
        Err(SendError::HandlerError(RoomMutationError::NotOwner))
    ));
    assert_eq!(
        actor.ask(GetConfig).await.expect("config after rejection"),
        original,
        "a fence for another room must not authorize a mutation"
    );
    assert_eq!(
        store.take_observed_fences(),
        vec![(room_jid, cross_room_fence)],
        "the full entity-bearing fence must reach the ownership check"
    );
}

#[tokio::test]
async fn ownership_check_error_leaves_config_mutation_unchanged() {
    let (actor, store) = spawn_fenced_room().await;
    store.take_observed_fences();
    store.set(UNCERTAIN);
    let original = actor.ask(GetConfig).await.expect("original config");
    let mut attempted = original.clone();
    attempted.name = "must not apply while uncertain".to_string();

    let result = actor.ask(UpdateConfig { config: attempted }).await;

    assert!(matches!(
        result,
        Err(SendError::HandlerError(
            RoomMutationError::OwnershipUnavailable
        ))
    ));
    assert_eq!(
        actor.ask(GetConfig).await.expect("config after rejection"),
        original,
        "an ownership backend error must fail before the state mutation"
    );
    let observed = store.take_observed_fences();
    assert_eq!(
        observed.len(),
        1,
        "the failed gate should make one exact check"
    );
    assert_eq!(
        observed[0].0,
        "ownership@muc.example.com"
            .parse::<BareJid>()
            .expect("room JID")
    );
    assert_eq!(
        observed[0].1,
        claim_fence(&observed[0].0, "test-node-epoch", 1)
    );
}

fn durable_existing_room() -> DurableRoomState {
    DurableRoomState {
        waddle_id: "durable-waddle".to_string(),
        channel_id: "durable-channel".to_string(),
        config: RoomConfig {
            name: "Existing durable room".to_string(),
            persistent: true,
            ..RoomConfig::default()
        },
        subject: None,
        affiliations: vec![AffiliationEntry::new(
            "owner@example.com".parse().expect("owner JID"),
            Affiliation::Owner,
        )],
    }
}

async fn spawn_restore_registry(store: Arc<OwnershipStore>) -> ActorRef<RoomRegistryActor> {
    let secret = OccupantIdSecret::new(vec![7; OCCUPANT_ID_SECRET_MIN_BYTES])
        .expect("valid occupant-id secret");
    let registry = RoomRegistryActor::spawn(RoomRegistryActor::new(
        "muc.example.com".to_string(),
        secret,
    ));
    registry
        .ask(WireClusteringClaims {
            claim_store: Arc::new(InProcessClaimStore::new()) as Arc<dyn ClaimStore>,
            node_identity: SharedNodeIdentity::new(NodeIdentity::local()),
            durable_store: Some(store as Arc<dyn MucDurableStore>),
            rollout_backoff: None,
        })
        .await
        .expect("wire durable restore store");
    registry
}

fn demand_room(room_jid: BareJid) -> GetOrCreateRoom {
    GetOrCreateRoom {
        room_jid,
        waddle_id: "caller-waddle".to_string(),
        channel_id: "caller-channel".to_string(),
        config: RoomConfig::default(),
    }
}

#[tokio::test]
async fn xep0045_creator_classification_requires_absent_durable_room() {
    let fresh_registry = spawn_restore_registry(Arc::new(OwnershipStore::new())).await;
    let fresh_jid: BareJid = "fresh@muc.example.com".parse().expect("fresh room JID");
    let fresh = fresh_registry
        .ask(demand_room(fresh_jid))
        .await
        .expect("fresh acquisition");
    assert_eq!(fresh.creation, RoomCreation::Created);

    let restored_registry =
        spawn_restore_registry(Arc::new(OwnershipStore::restoring(durable_existing_room()))).await;
    let restored_jid: BareJid = "restored@muc.example.com"
        .parse()
        .expect("restored room JID");
    let restored = restored_registry
        .ask(demand_room(restored_jid))
        .await
        .expect("restored acquisition");
    assert_eq!(restored.creation, RoomCreation::Existing);
}

#[tokio::test]
async fn xep0045_exclusive_create_rejects_durable_existing_room() {
    let registry =
        spawn_restore_registry(Arc::new(OwnershipStore::restoring(durable_existing_room()))).await;
    let room_jid: BareJid = "exclusive-existing@muc.example.com"
        .parse()
        .expect("room JID");

    assert!(matches!(
        registry
            .ask(CreateRoom {
                room_jid: room_jid.clone(),
                waddle_id: "caller-waddle".to_string(),
                channel_id: "caller-channel".to_string(),
                config: RoomConfig::default(),
            })
            .await,
        Err(SendError::HandlerError(
            RoomRegistryError::RoomAlreadyExists(ref existing)
        )) if *existing == room_jid
    ));
}

#[tokio::test]
async fn destroy_snapshot_includes_join_queued_before_the_actor_seal() {
    let config_save_started = Arc::new(tokio::sync::Notify::new());
    let allow_config_save = Arc::new(tokio::sync::Notify::new());
    let delete_started = Arc::new(tokio::sync::Notify::new());
    let allow_delete = Arc::new(tokio::sync::Notify::new());
    let store = Arc::new(OwnershipStore::blocking_config_save_and_delete(
        Arc::clone(&config_save_started),
        Arc::clone(&allow_config_save),
        Arc::clone(&delete_started),
        Arc::clone(&allow_delete),
    ));
    let registry = spawn_restore_registry(Arc::clone(&store)).await;
    let room_jid: BareJid = "destroy-snapshot@muc.example.com"
        .parse()
        .expect("room JID");
    let actor = registry
        .ask(demand_room(room_jid.clone()))
        .await
        .expect("create room")
        .actor_ref;

    // Hold the actor inside a durable mutation so the join and destroy seal
    // can be placed into one deterministic mailbox order.
    let mutation_actor = actor.clone();
    let mutation = tokio::spawn(async move {
        mutation_actor
            .ask(UpdateConfig {
                config: RoomConfig {
                    name: "blocked durable mutation".to_string(),
                    ..RoomConfig::default()
                },
            })
            .await
    });
    config_save_started.notified().await;

    let joined_session: jid::FullJid = "member@example.com/web".parse().expect("joined full JID");
    let mut queued_join = resolver_join();
    queued_join.admission_revision = 1;
    actor
        .tell(queued_join)
        .await
        .expect("queue resolver join before destroy");
    let destroy_registry = registry.clone();
    let destroy_jid = room_jid.clone();
    let destroy = tokio::spawn(async move {
        destroy_registry
            .ask(DestroyRoomWithSnapshot {
                room_jid: destroy_jid,
            })
            .await
    });

    allow_config_save.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(2), delete_started.notified())
        .await
        .unwrap_or_else(|_| {
            panic!(
                "destroy did not reach the durable delete; observed fences: {:?}",
                store.take_observed_fences()
            )
        });
    mutation
        .await
        .expect("mutation task")
        .expect("durable mutation completed");

    // Reaching the durable delete proves that the actor processed the queued
    // join, then the seal, then the post-seal snapshot. Keep deletion blocked
    // long enough to prove the still-live sealed actor rejects later joins.
    let late_join = JoinWithAffiliation {
        sender_jid: "late@example.com/web".parse().expect("late full JID"),
        nick: "late".to_string(),
        affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
        local_domain: "example.com".to_string(),
        admission_revision: 0,
    };
    assert!(matches!(
        actor.ask(late_join).await,
        Err(SendError::HandlerError(RoomActorError::RoomSealed))
    ));

    allow_delete.notify_one();
    let outcome = destroy.await.expect("destroy task").expect("destroy reply");
    let DestroyRoomWithSnapshotOutcome::Destroyed(Some(snapshot)) = outcome else {
        panic!("expected a post-seal snapshot, got {outcome:?}");
    };
    assert_eq!(
        snapshot.room.get_occupant_sessions("member"),
        vec![joined_session],
        "the §10.9 notification snapshot must include every join ordered before the seal"
    );
}
