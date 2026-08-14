//! XEP-0045 §7.2 admission fencing for a deposed room actor.
//!
//! A room incarnation whose durable ownership cannot be proven must not
//! admit an occupant or apply resolver-derived affiliation state. Otherwise
//! two owners could independently authorize the same XEP-0045 room.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Mutex;

use jid::BareJid;
use kameo::actor::{ActorRef, Spawn};
use kameo::error::SendError;
use waddle_xmpp::muc::affiliation::AffiliationEntry;
use waddle_xmpp::muc::durable::{
    DurableRoomState, MucDurableFuture, MucDurableStore, RoomClaimFenceContext, RoomCommitError,
    RoomCommitFuture, RoomCommittedCoordinates, RoomDurableMutation, RoomLifecycleId, RoomRevision,
};
use waddle_xmpp::muc::room_actor::{
    DurableRestoreReadiness, GetAffiliation, GetConfig, GetDurableRestoreReadiness,
    GetRoomSealState, GetRoomSnapshot, Join, JoinAffiliationGrant, JoinWithAffiliation,
    OccupantCount, ResolverAffiliationSyncOutcome, RestoreDurableRoomState, RoomActor,
    RoomActorError, RoomMutationError, RoomSealState, SyncResolverAffiliation, UpdateConfig,
};
use waddle_xmpp::muc::room_registry_actor::{
    CreateRoom, GetOrCreateRoom, RoomCreation, RoomRegistryActor, RoomRegistryError,
    WireClusteringClaims,
};
use waddle_xmpp::muc::{MucRoom, RoomConfig};
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
    expected_owner: NodeIdentity,
    expected_epoch: ClaimEpoch,
    observed_loads: AtomicUsize,
    established_fences: Mutex<HashMap<BareJid, RoomClaimFenceContext>>,
    lifecycle: std::sync::OnceLock<RoomLifecycleId>,
    next_revision: AtomicUsize,
}

impl OwnershipStore {
    fn new() -> Self {
        Self::expecting(NodeIdentity::local(), ClaimEpoch(0), None)
    }

    fn restoring(state: DurableRoomState) -> Self {
        Self::expecting(NodeIdentity::local(), ClaimEpoch(0), Some(state))
    }

    fn expecting(
        expected_owner: NodeIdentity,
        expected_epoch: ClaimEpoch,
        restore: Option<DurableRoomState>,
    ) -> Self {
        Self {
            state: AtomicU8::new(OWNED),
            restore,
            expected_owner,
            expected_epoch,
            observed_loads: AtomicUsize::new(0),
            established_fences: Mutex::new(HashMap::new()),
            lifecycle: std::sync::OnceLock::new(),
            next_revision: AtomicUsize::new(0),
        }
    }

    fn set(&self, state: u8) {
        self.state.store(state, Ordering::SeqCst);
    }

    fn take_observed_load_count(&self) -> usize {
        self.observed_loads.swap(0, Ordering::SeqCst)
    }

    fn validate_fence(
        &self,
        room_jid: &BareJid,
        fence: &RoomClaimFenceContext,
    ) -> Result<(), XmppError> {
        let expected_entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        if fence.entity != expected_entity
            || fence.owner != self.expected_owner
            || fence.epoch != self.expected_epoch
        {
            return Err(XmppError::OwnershipLost {
                entity: expected_entity,
            });
        }
        Ok(())
    }

    fn next_commit_coordinates(&self) -> RoomCommittedCoordinates {
        let lifecycle = *self.lifecycle.get_or_init(RoomLifecycleId::generate);
        let revision = self.next_revision.fetch_add(1, Ordering::SeqCst) + 1;
        RoomCommittedCoordinates {
            lifecycle,
            revision: RoomRevision::from_stored(revision as i64).expect("positive revision"),
        }
    }
}

impl MucDurableStore for OwnershipStore {
    fn commit_room_mutation<'a>(
        &'a self,
        room_jid: &'a BareJid,
        fence: &'a RoomClaimFenceContext,
        _intent: RoomDurableMutation,
    ) -> RoomCommitFuture<'a> {
        let validation = self.validate_fence(room_jid, fence);
        let established =
            self.established_fences.lock().expect("lock").get(room_jid) == Some(fence);
        let state = self.state.load(Ordering::SeqCst);
        let coordinates = self.next_commit_coordinates();
        Box::pin(async move {
            validation.map_err(|_| RoomCommitError::NotOwner)?;
            if !established {
                return Err(RoomCommitError::OwnershipUnavailable);
            }
            match state {
                OWNED => Ok(coordinates),
                DEPOSED => Err(RoomCommitError::NotOwner),
                UNCERTAIN => Err(RoomCommitError::OwnershipUnavailable),
                _ => unreachable!("test ownership state"),
            }
        })
    }

    fn load_room_state_fenced<'a>(
        &'a self,
        room_jid: &'a BareJid,
        fence: &'a RoomClaimFenceContext,
    ) -> MucDurableFuture<'a, Option<DurableRoomState>> {
        self.observed_loads.fetch_add(1, Ordering::SeqCst);
        let validation = self.validate_fence(room_jid, fence);
        let restore = self.restore.clone();
        let state = self.state.load(Ordering::SeqCst);
        let entity = fence.entity.clone();
        Box::pin(async move {
            validation?;
            match state {
                OWNED => Ok(restore),
                DEPOSED => Err(XmppError::OwnershipLost { entity }),
                UNCERTAIN => Err(XmppError::internal("ownership proof unavailable")),
                _ => unreachable!("test ownership state"),
            }
        })
    }

    fn establish_claim_fence(&self, room_jid: &BareJid, fence: RoomClaimFenceContext) {
        self.established_fences
            .lock()
            .expect("lock")
            .insert(room_jid.clone(), fence);
    }

    fn check_exact_claim_fence<'a>(
        &'a self,
        room_jid: &'a BareJid,
        fence: &'a RoomClaimFenceContext,
    ) -> MucDurableFuture<'a, bool> {
        let exact_fence = self.validate_fence(room_jid, fence).is_ok();
        let state = self.state.load(Ordering::SeqCst);
        Box::pin(async move {
            if !exact_fence {
                return Ok(false);
            }
            match state {
                OWNED => Ok(true),
                DEPOSED => Ok(false),
                UNCERTAIN => Err(XmppError::internal("ownership proof unavailable")),
                _ => unreachable!("test ownership state"),
            }
        })
    }
}

async fn spawn_unrestored_fenced_room() -> (
    ActorRef<RoomActor>,
    Arc<OwnershipStore>,
    RoomClaimFenceContext,
) {
    let room_jid: BareJid = "ownership@muc.example.com".parse().expect("valid room JID");
    let room = MucRoom::new(
        room_jid.clone(),
        "waddle-1".to_string(),
        "channel-1".to_string(),
        RoomConfig::default(),
    );
    let secret = OccupantIdSecret::new(vec![7; OCCUPANT_ID_SECRET_MIN_BYTES])
        .expect("valid occupant-id secret");
    let actor = RoomActor::spawn(RoomActor::new(room, secret));
    let owner = NodeIdentity::new("test-node", "test-node-epoch");
    let epoch = ClaimEpoch(1);
    let store = Arc::new(OwnershipStore::expecting(owner.clone(), epoch, None));
    let claim_fence = RoomClaimFenceContext::new(
        Entity::new(EntityType::RoomActor, room_jid.to_string()),
        owner,
        epoch,
    );
    (actor, store, claim_fence)
}

async fn spawn_fenced_room() -> (ActorRef<RoomActor>, Arc<OwnershipStore>) {
    let (actor, store, claim_fence) = spawn_unrestored_fenced_room().await;
    store.establish_claim_fence(
        &"ownership@muc.example.com".parse().expect("valid room JID"),
        claim_fence.clone(),
    );
    actor
        .ask(RestoreDurableRoomState {
            store: Arc::clone(&store) as Arc<dyn MucDurableStore>,
            claim_fence,
        })
        .await
        .expect("install durable ownership store");
    (actor, store)
}

fn claim_fence(room_jid: &BareJid, node_epoch: &str, claim_epoch: i64) -> RoomClaimFenceContext {
    RoomClaimFenceContext::new(
        Entity::new(EntityType::RoomActor, room_jid.to_string()),
        NodeIdentity::new("test-node", node_epoch),
        ClaimEpoch(claim_epoch),
    )
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
async fn restore_ownership_loss_is_terminal_and_never_retries_xep0045_join() {
    let (actor, store, claim_fence) = spawn_unrestored_fenced_room().await;
    store.set(UNCERTAIN);

    actor
        .ask(RestoreDurableRoomState {
            store: Arc::clone(&store) as Arc<dyn MucDurableStore>,
            claim_fence,
        })
        .await
        .expect("transient restore failure is retained as pending");
    assert_eq!(store.take_observed_load_count(), 1);
    assert_eq!(
        actor
            .ask(GetDurableRestoreReadiness)
            .await
            .expect("restore readiness"),
        DurableRestoreReadiness::Pending
    );

    store.set(DEPOSED);
    assert!(matches!(
        actor.ask(resolver_join()).await,
        Err(SendError::HandlerError(RoomActorError::RoomSealed))
    ));
    assert_eq!(
        actor
            .ask(GetDurableRestoreReadiness)
            .await
            .expect("terminal restore readiness"),
        DurableRestoreReadiness::OwnershipLost
    );
    assert_eq!(
        actor.ask(GetRoomSealState).await.expect("room seal state"),
        RoomSealState::OwnershipLost
    );
    assert_eq!(store.take_observed_load_count(), 1);

    assert!(matches!(
        actor.ask(resolver_join()).await,
        Err(SendError::HandlerError(RoomActorError::RoomSealed))
    ));
    assert_eq!(
        store.take_observed_load_count(),
        0,
        "the terminal restore state must not retry the stale fence"
    );
    actor
        .ask(RestoreDurableRoomState {
            store: Arc::clone(&store) as Arc<dyn MucDurableStore>,
            claim_fence: RoomClaimFenceContext::new(
                Entity::new(EntityType::RoomActor, "ownership@muc.example.com"),
                NodeIdentity::new("test-node", "test-node-epoch"),
                ClaimEpoch(1),
            ),
        })
        .await
        .expect("terminal restore ignores the same stale fence");
    assert_eq!(
        actor
            .ask(GetDurableRestoreReadiness)
            .await
            .expect("terminal restore readiness"),
        DurableRestoreReadiness::OwnershipLost
    );
    assert_eq!(
        store.take_observed_load_count(),
        0,
        "a terminal restore state must ignore duplicate same-fence restores"
    );
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

async fn assert_invalid_restore_fence_fails_closed(invalid_fence: RoomClaimFenceContext) {
    let room_jid: BareJid = "invalid-fence@muc.example.com".parse().expect("room JID");
    let expected_owner = NodeIdentity::new("expected-node", "expected-incarnation");
    let expected_epoch = ClaimEpoch(7);
    let room = MucRoom::new(
        room_jid.clone(),
        "waddle-1".to_string(),
        "channel-1".to_string(),
        RoomConfig::default(),
    );
    let secret = OccupantIdSecret::new(vec![7; OCCUPANT_ID_SECRET_MIN_BYTES])
        .expect("valid occupant-id secret");
    let actor = RoomActor::spawn(RoomActor::new(room, secret));
    let store = Arc::new(OwnershipStore::expecting(
        expected_owner,
        expected_epoch,
        None,
    ));

    actor
        .ask(RestoreDurableRoomState {
            store: Arc::clone(&store) as Arc<dyn MucDurableStore>,
            claim_fence: invalid_fence,
        })
        .await
        .expect("invalid restore is retained only as a fail-closed state");

    assert_eq!(store.take_observed_load_count(), 1);
    assert_eq!(
        actor
            .ask(GetDurableRestoreReadiness)
            .await
            .expect("restore readiness"),
        DurableRestoreReadiness::OwnershipLost,
    );
    assert_eq!(
        actor.ask(GetRoomSealState).await.expect("room seal state"),
        RoomSealState::OwnershipLost,
    );
    let original_config = actor.ask(GetConfig).await.expect("original config");
    let mut attempted = original_config.clone();
    attempted.name = "must not mutate".to_string();
    assert!(matches!(
        actor.ask(UpdateConfig { config: attempted }).await,
        Err(SendError::HandlerError(RoomMutationError::NotOwner))
    ));
    assert_eq!(
        actor.ask(GetConfig).await.expect("config after rejection"),
        original_config,
    );
    assert!(matches!(
        actor.ask(resolver_join()).await,
        Err(SendError::HandlerError(RoomActorError::RoomSealed))
    ));
    assert_eq!(actor.ask(OccupantCount).await.expect("occupant count"), 0);
}

#[tokio::test]
async fn invalid_entity_owner_and_epoch_fences_fail_closed() {
    let room_jid: BareJid = "invalid-fence@muc.example.com".parse().expect("room JID");
    let expected_entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
    let expected_owner = NodeIdentity::new("expected-node", "expected-incarnation");

    assert_invalid_restore_fence_fails_closed(RoomClaimFenceContext::new(
        Entity::new(EntityType::RoomActor, "other@muc.example.com".to_string()),
        expected_owner.clone(),
        ClaimEpoch(7),
    ))
    .await;
    assert_invalid_restore_fence_fails_closed(RoomClaimFenceContext::new(
        expected_entity.clone(),
        NodeIdentity::new("other-node", "other-incarnation"),
        ClaimEpoch(7),
    ))
    .await;
    assert_invalid_restore_fence_fails_closed(RoomClaimFenceContext::new(
        expected_entity,
        expected_owner,
        ClaimEpoch(8),
    ))
    .await;
}

#[tokio::test]
async fn second_restore_cannot_transplant_an_actor_to_a_successor_fence() {
    let room_jid: BareJid = "restore-transplant@muc.example.com"
        .parse()
        .expect("room JID");
    let room = MucRoom::new(
        room_jid.clone(),
        "waddle-1".to_string(),
        "channel-1".to_string(),
        RoomConfig::default(),
    );
    let secret = OccupantIdSecret::new(vec![7; OCCUPANT_ID_SECRET_MIN_BYTES])
        .expect("valid occupant-id secret");
    let actor = RoomActor::spawn(RoomActor::new(room, secret));
    let original_fence = claim_fence(&room_jid, "original-incarnation", 11);
    let original_store = Arc::new(OwnershipStore::expecting(
        original_fence.owner.clone(),
        original_fence.epoch,
        None,
    ));
    actor
        .ask(RestoreDurableRoomState {
            store: Arc::clone(&original_store) as Arc<dyn MucDurableStore>,
            claim_fence: original_fence.clone(),
        })
        .await
        .expect("initial exact-fence restore");
    original_store.take_observed_load_count();
    let original_config = actor.ask(GetConfig).await.expect("original config");
    let snapshot_sender: jid::FullJid = "observer@example.com/test"
        .parse()
        .expect("snapshot sender");
    assert_eq!(
        actor
            .ask(GetRoomSnapshot {
                sender_jid: snapshot_sender.clone(),
            })
            .await
            .expect("snapshot before transplant attempt")
            .claim_fence,
        Some(original_fence.clone()),
    );

    let successor_fence = claim_fence(&room_jid, "successor-incarnation", 12);
    let successor_store = Arc::new(OwnershipStore::expecting(
        successor_fence.owner.clone(),
        successor_fence.epoch,
        Some(durable_existing_room()),
    ));
    actor
        .ask(RestoreDurableRoomState {
            store: Arc::clone(&successor_store) as Arc<dyn MucDurableStore>,
            claim_fence: successor_fence,
        })
        .await
        .expect("mismatched restore is handled fail-closed");

    assert!(
        successor_store.take_observed_load_count() == 0,
        "the successor store must not receive a load from the old actor incarnation"
    );
    assert_eq!(original_store.take_observed_load_count(), 0);
    // The rejected transplant seals the actor, and sealed actors refuse
    // dispatch snapshots outright — stronger than the earlier contract of
    // serving the original fence: the successor's fence can never be
    // observed through this actor at all.
    assert!(
        matches!(
            actor
                .ask(GetRoomSnapshot {
                    sender_jid: snapshot_sender,
                })
                .await,
            Err(SendError::HandlerError(RoomActorError::RoomSealed))
        ),
        "a sealed actor must fail dispatch snapshots closed after a rejected successor restore",
    );

    let mut attempted = original_config.clone();
    attempted.name = "must remain sealed".to_string();
    assert!(matches!(
        actor.ask(UpdateConfig { config: attempted }).await,
        Err(SendError::HandlerError(RoomMutationError::NotOwner))
    ));
    assert_eq!(
        actor.ask(GetConfig).await.expect("config after rejection"),
        original_config,
    );
    assert!(matches!(
        actor.ask(resolver_join()).await,
        Err(SendError::HandlerError(RoomActorError::RoomSealed))
    ));
    assert_eq!(actor.ask(OccupantCount).await.expect("occupant count"), 0);
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
