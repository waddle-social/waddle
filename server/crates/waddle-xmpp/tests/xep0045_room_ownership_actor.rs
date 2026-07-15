//! XEP-0045 §7.2 admission fencing for a deposed room actor.
//!
//! A room incarnation whose durable ownership cannot be proven must not
//! admit an occupant or apply resolver-derived affiliation state. Otherwise
//! two owners could independently authorize the same XEP-0045 room.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use jid::BareJid;
use kameo::actor::{ActorRef, Spawn};
use kameo::error::SendError;
use waddle_xmpp::muc::affiliation::AffiliationEntry;
use waddle_xmpp::muc::durable::{
    DurableRoomState, MucDurableFuture, MucDurableStore, RoomClaimFenceContext,
};
use waddle_xmpp::muc::room_actor::{
    GetAffiliation, Join, JoinAffiliationGrant, JoinWithAffiliation, OccupantCount,
    ResolverAffiliationSyncOutcome, RestoreDurableRoomState, RoomActor, RoomActorError,
    SyncResolverAffiliation,
};
use waddle_xmpp::muc::room_registry_actor::{
    CreateRoom, GetOrCreateRoom, RoomCreation, RoomRegistryActor, RoomRegistryError,
    WireClusteringClaims,
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
}

impl OwnershipStore {
    fn new() -> Self {
        Self {
            state: AtomicU8::new(OWNED),
            restore: None,
        }
    }

    fn restoring(state: DurableRoomState) -> Self {
        Self {
            state: AtomicU8::new(OWNED),
            restore: Some(state),
        }
    }

    fn set(&self, state: u8) {
        self.state.store(state, Ordering::SeqCst);
    }
}

impl MucDurableStore for OwnershipStore {
    fn load_room_state_fenced<'a>(
        &'a self,
        _room_jid: &'a BareJid,
        _fence: &'a RoomClaimFenceContext,
    ) -> MucDurableFuture<'a, Option<DurableRoomState>> {
        let restore = self.restore.clone();
        Box::pin(async move { Ok(restore) })
    }

    fn save_config_fenced<'a>(
        &'a self,
        _room_jid: &'a BareJid,
        _waddle_id: &'a str,
        _channel_id: &'a str,
        _config: &'a RoomConfig,
        _fence: &'a RoomClaimFenceContext,
    ) -> MucDurableFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }

    fn save_subject_fenced<'a>(
        &'a self,
        _room_jid: &'a BareJid,
        _subject: Option<&'a SubjectState>,
        _fence: &'a RoomClaimFenceContext,
    ) -> MucDurableFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }

    fn save_affiliation_fenced<'a>(
        &'a self,
        _room_jid: &'a BareJid,
        _entry: &'a AffiliationEntry,
        _fence: &'a RoomClaimFenceContext,
    ) -> MucDurableFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }

    fn delete_room_state_fenced<'a>(
        &'a self,
        _room_jid: &'a BareJid,
        _fence: &'a RoomClaimFenceContext,
    ) -> MucDurableFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }

    fn check_exact_claim_fence<'a>(
        &'a self,
        _room_jid: &'a BareJid,
        _fence: &'a RoomClaimFenceContext,
    ) -> MucDurableFuture<'a, bool> {
        let state = self.state.load(Ordering::SeqCst);
        Box::pin(async move {
            match state {
                OWNED => Ok(true),
                DEPOSED => Ok(false),
                UNCERTAIN => Err(XmppError::internal("ownership proof unavailable")),
                _ => unreachable!("test ownership state"),
            }
        })
    }
}

async fn spawn_fenced_room() -> (ActorRef<RoomActor>, Arc<OwnershipStore>) {
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
    let store = Arc::new(OwnershipStore::new());
    actor
        .ask(RestoreDurableRoomState {
            store: Arc::clone(&store) as Arc<dyn MucDurableStore>,
            claim_fence: RoomClaimFenceContext::new(
                Entity::new(EntityType::RoomActor, room_jid.to_string()),
                NodeIdentity::new("test-node", "test-node-epoch"),
                ClaimEpoch(1),
            ),
        })
        .await
        .expect("install durable ownership store");
    (actor, store)
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
