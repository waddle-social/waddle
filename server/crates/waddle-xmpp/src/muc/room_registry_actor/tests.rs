use super::*;
use kameo::actor::Spawn;
use kameo::error::SendError;

struct RememberOrdinaryReleaseForTest {
    room_jid: BareJid,
    claim_fence: crate::muc::RoomClaimFenceContext,
}

impl kameo::message::Message<RememberOrdinaryReleaseForTest> for RoomRegistryActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: RememberOrdinaryReleaseForTest,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.remember_pending_room_release(msg.room_jid, msg.claim_fence)
    }
}

struct ReservePendingAcquisitionForTest {
    room_jid: BareJid,
    owner: NodeIdentity,
}

impl kameo::message::Message<ReservePendingAcquisitionForTest> for RoomRegistryActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: ReservePendingAcquisitionForTest,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.reserve_pending_room_acquisition(&msg.room_jid, &msg.owner)
    }
}

struct PendingPreparationWaitersForTest {
    room_jid: BareJid,
}

struct PendingPreparationCountForTest;

struct PendingRoomOwnershipResponsibilityCountForTest;

impl kameo::message::Message<PendingRoomOwnershipResponsibilityCountForTest> for RoomRegistryActor {
    type Reply = usize;

    async fn handle(
        &mut self,
        _msg: PendingRoomOwnershipResponsibilityCountForTest,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.pending_room_ownership_responsibility_count_for_test()
    }
}

impl kameo::message::Message<PendingPreparationCountForTest> for RoomRegistryActor {
    type Reply = usize;

    async fn handle(
        &mut self,
        _msg: PendingPreparationCountForTest,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.pending_room_preparations.len()
    }
}

impl kameo::message::Message<PendingPreparationWaitersForTest> for RoomRegistryActor {
    type Reply = Option<usize>;

    async fn handle(
        &mut self,
        msg: PendingPreparationWaitersForTest,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.pending_room_preparations
            .get(&msg.room_jid)
            .map(|pending| pending.waiters.len())
    }
}

fn test_room_jid(name: &str) -> BareJid {
    format!("{}@muc.example.com", name)
        .parse()
        .expect("valid test JID")
}

async fn spawn_registry() -> ActorRef<RoomRegistryActor> {
    RoomRegistryActor::spawn(RoomRegistryActor::new(
        "muc.example.com".to_string(),
        OccupantIdSecret::for_testing(b"test-secret".to_vec()),
    ))
}

#[tokio::test]
async fn test_room_count_starts_at_zero() {
    let registry = spawn_registry().await;
    let count: usize = registry.ask(RoomCount).await.expect("ask");
    assert_eq!(count, 0);
}

#[tokio::test]
async fn test_create_room() {
    let registry = spawn_registry().await;
    let jid = test_room_jid("general");

    // Kameo flattens Result replies: ask() returns T directly
    let _actor_ref: ActorRef<RoomActor> = registry
        .ask(CreateRoom {
            room_jid: jid.clone(),
            waddle_id: "w-1".to_string(),
            channel_id: "c-1".to_string(),
            config: RoomConfig::default(),
        })
        .await
        .expect("create room");

    let exists: bool = registry
        .ask(RoomExists { room_jid: jid })
        .await
        .expect("exists");
    assert!(exists);

    let count: usize = registry.ask(RoomCount).await.expect("count");
    assert_eq!(count, 1);
}

#[tokio::test]
async fn test_create_duplicate_room_fails() {
    let registry = spawn_registry().await;
    let jid = test_room_jid("dup");

    registry
        .ask(CreateRoom {
            room_jid: jid.clone(),
            waddle_id: "w-1".to_string(),
            channel_id: "c-1".to_string(),
            config: RoomConfig::default(),
        })
        .await
        .expect("first create");

    let result = registry
        .ask(CreateRoom {
            room_jid: jid.clone(),
            waddle_id: "w-1".to_string(),
            channel_id: "c-1".to_string(),
            config: RoomConfig::default(),
        })
        .await;

    assert!(matches!(
        result,
        Err(SendError::HandlerError(RoomRegistryError::RoomAlreadyExists(room_jid)))
            if room_jid == jid
    ));
}

#[tokio::test]
async fn test_get_room() {
    let registry = spawn_registry().await;
    let jid = test_room_jid("lookup");

    // Non-existent room returns None
    let got: Option<ActorRef<RoomActor>> = registry
        .ask(GetRoom {
            room_jid: jid.clone(),
        })
        .await
        .expect("get room");
    assert!(got.is_none());

    // Create it
    registry
        .ask(CreateRoom {
            room_jid: jid.clone(),
            waddle_id: "w-1".to_string(),
            channel_id: "c-1".to_string(),
            config: RoomConfig::default(),
        })
        .await
        .expect("create");

    // Now it should be found
    let got: Option<ActorRef<RoomActor>> = registry
        .ask(GetRoom { room_jid: jid })
        .await
        .expect("get room");
    assert!(got.is_some());
}

#[tokio::test]
async fn test_get_or_create_room_idempotent() {
    let registry = spawn_registry().await;
    let jid = test_room_jid("idempotent");

    let first: ActorRef<RoomActor> = registry
        .ask(GetOrCreateRoom {
            room_jid: jid.clone(),
            waddle_id: "w-1".to_string(),
            channel_id: "c-1".to_string(),
            config: RoomConfig::default(),
        })
        .await
        .expect("first get_or_create")
        .actor_ref;

    let second: ActorRef<RoomActor> = registry
        .ask(GetOrCreateRoom {
            room_jid: jid,
            waddle_id: "w-1".to_string(),
            channel_id: "c-1".to_string(),
            config: RoomConfig::default(),
        })
        .await
        .expect("second get_or_create")
        .actor_ref;

    assert_eq!(first.id(), second.id());

    let count: usize = registry.ask(RoomCount).await.expect("count");
    assert_eq!(count, 1);
}

#[tokio::test]
async fn test_destroy_room() {
    let registry = spawn_registry().await;
    let jid = test_room_jid("doomed");

    registry
        .ask(CreateRoom {
            room_jid: jid.clone(),
            waddle_id: "w-1".to_string(),
            channel_id: "c-1".to_string(),
            config: RoomConfig::default(),
        })
        .await
        .expect("create");

    let removed: DestroyRoomOutcome = registry
        .ask(DestroyRoom {
            room_jid: jid.clone(),
            reason: DestroyRoomReason::Destroy,
        })
        .await
        .expect("destroy");
    assert_eq!(removed, DestroyRoomOutcome::Destroyed);

    let exists: bool = registry
        .ask(RoomExists { room_jid: jid })
        .await
        .expect("exists");
    assert!(!exists);
}

/// #1134: the "did this call create the room?" bit must come from the
/// registry's serialized handler — exactly one caller observes
/// `Created`, so exactly one racing first-join can grant itself the
/// XEP-0045 §10.1.1 creator Owner. Inferring the bit at the call site
/// ("no actor existed when I looked") let both racers claim it.
#[tokio::test]
async fn get_or_create_reports_created_exactly_once() {
    let registry = spawn_registry().await;
    let jid = test_room_jid("first-join-race");

    let first: RoomAcquisition = registry
        .ask(GetOrCreateRoom {
            room_jid: jid.clone(),
            waddle_id: "w-1".to_string(),
            channel_id: "c-1".to_string(),
            config: RoomConfig::default(),
        })
        .await
        .expect("first get_or_create");
    assert_eq!(
        first.creation,
        RoomCreation::Created,
        "the call that spawned the room reports Created"
    );

    let second: RoomAcquisition = registry
        .ask(GetOrCreateRoom {
            room_jid: jid.clone(),
            waddle_id: "w-1".to_string(),
            channel_id: "c-1".to_string(),
            config: RoomConfig::default(),
        })
        .await
        .expect("second get_or_create");
    assert_eq!(
        second.creation,
        RoomCreation::Existing,
        "every later call reports Existing — only one creator (#1134)"
    );
    assert_eq!(
        second.actor_ref.id(),
        first.actor_ref.id(),
        "both calls resolve the same room actor"
    );
}

/// #1108: the janitor's IsDormant → destroy sequence is a TOCTOU.
/// A join that lands between the dormancy probe and the destroy must
/// make the guarded destroy refuse — the revision it carries is stale
/// and the seal re-check runs inside the room actor's own mailbox,
/// serialized against the join.
#[tokio::test]
async fn guarded_destroy_refuses_when_join_landed_after_dormancy_probe() {
    use crate::muc::room_actor::{
        IsDormant, JoinAffiliationGrant, JoinWithAffiliation, LeaveByRealJid, OccupantCount,
        SealGuard,
    };
    use crate::types::Affiliation;

    let registry = spawn_registry().await;
    let jid = test_room_jid("toctou");
    let actor: ActorRef<RoomActor> = registry
        .ask(GetOrCreateRoom {
            room_jid: jid.clone(),
            waddle_id: "w-1".to_string(),
            channel_id: "c-1".to_string(),
            config: RoomConfig::default(),
        })
        .await
        .expect("create room")
        .actor_ref;

    // Janitor half 1: probe dormancy, capturing the revision.
    let probe = actor.ask(IsDormant).await.expect("dormancy probe");
    assert!(probe.dormant, "fresh empty room is dormant");

    // The race: a join lands between the probe and the destroy.
    let alice: jid::FullJid = "alice@example.com/web".parse().expect("full jid");
    actor
        .ask(JoinWithAffiliation {
            sender_jid: alice.clone(),
            nick: "alice".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: 0,
        })
        .await
        .expect("interleaved join");

    // Janitor half 2: guarded destroy with the stale revision → refused.
    let destroyed: bool = registry
        .ask(DestroyRoomIfInactive {
            room_jid: jid.clone(),
            expected_occupancy_revision: probe.occupancy_revision,
            guard: SealGuard::Dormant,
        })
        .await
        .expect("guarded destroy ask");
    assert!(
        !destroyed,
        "a join after the dormancy probe must refuse the guarded destroy \
         — destroying here orphans the freshly-admitted occupant (#1108)"
    );
    assert!(
        registry
            .ask(RoomExists {
                room_jid: jid.clone()
            })
            .await
            .expect("exists"),
        "the room stays registered"
    );
    assert_eq!(
        actor.ask(OccupantCount).await.expect("count"),
        1,
        "the interleaved occupant is intact"
    );

    // After the occupant leaves, a fresh probe + matching revision
    // destroys the actually-dormant room.
    actor
        .ask(LeaveByRealJid { sender_jid: alice })
        .await
        .expect("leave")
        .expect("outcome");
    let probe = actor.ask(IsDormant).await.expect("second probe");
    assert!(probe.dormant, "room is dormant again after the leave");
    let destroyed: bool = registry
        .ask(DestroyRoomIfInactive {
            room_jid: jid.clone(),
            expected_occupancy_revision: probe.occupancy_revision,
            guard: SealGuard::Dormant,
        })
        .await
        .expect("guarded destroy ask");
    assert!(destroyed, "an actually-dormant room is destroyed");
    assert!(
        !registry
            .ask(RoomExists { room_jid: jid })
            .await
            .expect("exists"),
        "the room is gone from the registry"
    );
}

/// #1108 second half: a caller that grabbed the ActorRef before the
/// guarded destroy and sends its join afterwards must get a typed,
/// retryable refusal (the sealed actor never silently admits an
/// occupant into a destroyed room), so the join handler can re-run
/// the registry lookup and respawn the room.
#[tokio::test]
async fn sealed_room_refuses_late_join_with_retryable_error() {
    use crate::muc::room_actor::{
        IsDormant, JoinAffiliationGrant, JoinWithAffiliation, RoomActorError, SealGuard,
    };
    use crate::types::Affiliation;

    let registry = spawn_registry().await;
    let jid = test_room_jid("sealed");
    let stale_ref: ActorRef<RoomActor> = registry
        .ask(GetOrCreateRoom {
            room_jid: jid.clone(),
            waddle_id: "w-1".to_string(),
            channel_id: "c-1".to_string(),
            config: RoomConfig::default(),
        })
        .await
        .expect("create room")
        .actor_ref;
    let probe = stale_ref.ask(IsDormant).await.expect("probe");
    assert!(probe.dormant);

    let destroyed: bool = registry
        .ask(DestroyRoomIfInactive {
            room_jid: jid.clone(),
            expected_occupancy_revision: probe.occupancy_revision,
            guard: SealGuard::Dormant,
        })
        .await
        .expect("guarded destroy ask");
    assert!(destroyed);

    let alice: jid::FullJid = "alice@example.com/web".parse().expect("full jid");
    let result = stale_ref
        .ask(JoinWithAffiliation {
            sender_jid: alice,
            nick: "alice".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: 0,
        })
        .await;
    assert!(
        matches!(
            result,
            Err(SendError::HandlerError(RoomActorError::RoomSealed))
        ),
        "a sealed room actor refuses late joins with the typed retryable \
         error instead of admitting an occupant into a destroyed room; \
         got: {result:?}"
    );
}

#[tokio::test]
async fn test_destroy_non_existent_room_returns_false() {
    let registry = spawn_registry().await;
    let jid = test_room_jid("ghost");

    let removed: DestroyRoomOutcome = registry
        .ask(DestroyRoom {
            room_jid: jid,
            reason: DestroyRoomReason::Destroy,
        })
        .await
        .expect("destroy");
    assert_eq!(removed, DestroyRoomOutcome::NotRegistered);
}

#[tokio::test]
async fn test_is_muc_jid() {
    let registry = spawn_registry().await;

    let muc_jid: BareJid = "room@muc.example.com".parse().expect("valid JID");
    let other_jid: BareJid = "user@example.com".parse().expect("valid JID");

    let is_muc: bool = registry
        .ask(IsMucJid { jid: muc_jid })
        .await
        .expect("is_muc");
    assert!(is_muc);

    let is_muc: bool = registry
        .ask(IsMucJid { jid: other_jid })
        .await
        .expect("is_muc");
    assert!(!is_muc);
}

#[tokio::test]
async fn test_list_rooms() {
    let registry = spawn_registry().await;

    registry
        .ask(CreateRoom {
            room_jid: test_room_jid("alpha"),
            waddle_id: "w-1".to_string(),
            channel_id: "c-1".to_string(),
            config: RoomConfig::default(),
        })
        .await
        .expect("create alpha");

    registry
        .ask(CreateRoom {
            room_jid: test_room_jid("beta"),
            waddle_id: "w-2".to_string(),
            channel_id: "c-2".to_string(),
            config: RoomConfig::default(),
        })
        .await
        .expect("create beta");

    let mut rooms: Vec<BareJid> = registry.ask(ListRooms).await.expect("list");
    rooms.sort_by_key(|a| a.to_string());

    assert_eq!(rooms.len(), 2);
    assert_eq!(rooms[0].to_string(), "alpha@muc.example.com");
    assert_eq!(rooms[1].to_string(), "beta@muc.example.com");
}

#[tokio::test]
async fn list_rooms_owned_by_excludes_fresh_post_rotation_rooms() {
    let registry = spawn_registry().await;
    let old = NodeIdentity::new("room-node", "old-incarnation");
    let fresh = NodeIdentity::new("room-node", "fresh-incarnation");
    let identity = SharedNodeIdentity::new(old.clone());
    let claim_store = Arc::new(InProcessClaimStore::new());
    registry
        .ask(WireClusteringClaims {
            claim_store: claim_store.clone(),
            node_identity: identity.clone(),
            durable_store: None,
            rollout_backoff: None,
        })
        .await
        .expect("wire claims");

    registry
        .ask(CreateRoom {
            room_jid: test_room_jid("old-owner"),
            waddle_id: "w-old".to_string(),
            channel_id: "c-old".to_string(),
            config: RoomConfig::default(),
        })
        .await
        .expect("create old-owner room");
    identity.rotate(fresh.clone()).await;
    registry
        .ask(CreateRoom {
            room_jid: test_room_jid("fresh-owner"),
            waddle_id: "w-fresh".to_string(),
            channel_id: "c-fresh".to_string(),
            config: RoomConfig::default(),
        })
        .await
        .expect("create fresh-owner room");

    assert_eq!(
        registry
            .ask(ListRoomsOwnedBy { owner: old })
            .await
            .expect("old owner rooms"),
        vec![test_room_jid("old-owner")]
    );
    assert_eq!(
        registry
            .ask(ListRoomsOwnedBy { owner: fresh })
            .await
            .expect("fresh owner rooms"),
        vec![test_room_jid("fresh-owner")]
    );
}

#[tokio::test]
async fn stale_exact_owner_demotion_preserves_a_fresh_same_jid_room() {
    let registry = spawn_registry().await;
    let old = NodeIdentity::new("room-node", "old-incarnation");
    let fresh = NodeIdentity::new("room-node", "fresh-incarnation");
    let identity = SharedNodeIdentity::new(old.clone());
    let claim_store = Arc::new(InProcessClaimStore::new());
    registry
        .ask(WireClusteringClaims {
            claim_store: claim_store.clone(),
            node_identity: identity.clone(),
            durable_store: None,
            rollout_backoff: None,
        })
        .await
        .expect("wire claims");
    let room_jid = test_room_jid("same-jid");
    registry
        .ask(CreateRoom {
            room_jid: room_jid.clone(),
            waddle_id: "w-old".to_string(),
            channel_id: "c-old".to_string(),
            config: RoomConfig::default(),
        })
        .await
        .expect("create old room");
    assert!(registry
        .ask(DemoteRoomIfOwner {
            room_jid: room_jid.clone(),
            owner: old.clone(),
        })
        .await
        .expect("demote old room"));
    claim_store
        .release(
            &Entity::new(EntityType::RoomActor, room_jid.to_string()),
            &old,
            ClaimEpoch(0),
        )
        .await
        .expect("simulate authoritative old-claim retirement");

    identity.rotate(fresh).await;
    registry
        .ask(CreateRoom {
            room_jid: room_jid.clone(),
            waddle_id: "w-fresh".to_string(),
            channel_id: "c-fresh".to_string(),
            config: RoomConfig::default(),
        })
        .await
        .expect("create fresh room");

    assert!(!registry
        .ask(DemoteRoomIfOwner {
            room_jid: room_jid.clone(),
            owner: old,
        })
        .await
        .expect("stale demotion"));
    assert!(registry
        .ask(GetRoom { room_jid })
        .await
        .expect("fresh room lookup")
        .is_some());
}

#[tokio::test]
async fn test_get_or_create_fails_fast_for_dead_room_until_explicit_destroy() {
    let registry = spawn_registry().await;
    let room_jid = test_room_jid("restart");

    let first: ActorRef<RoomActor> = registry
        .ask(GetOrCreateRoom {
            room_jid: room_jid.clone(),
            waddle_id: "w-1".to_string(),
            channel_id: "c-1".to_string(),
            config: RoomConfig::default(),
        })
        .await
        .expect("first get_or_create")
        .actor_ref;
    first.kill();
    tokio::task::yield_now().await;

    let result = registry
        .ask(GetOrCreateRoom {
            room_jid: room_jid.clone(),
            waddle_id: "w-1".to_string(),
            channel_id: "c-1".to_string(),
            config: RoomConfig::default(),
        })
        .await;
    assert!(matches!(
        result,
        Err(SendError::HandlerError(RoomRegistryError::RoomActorStateLost(jid)))
            if jid == room_jid
    ));

    let destroyed = registry
        .ask(DestroyRoom {
            room_jid: room_jid.clone(),
            reason: DestroyRoomReason::Destroy,
        })
        .await
        .expect("destroy poisoned room");
    assert_eq!(destroyed, DestroyRoomOutcome::Destroyed);

    let recreated: ActorRef<RoomActor> = registry
        .ask(GetOrCreateRoom {
            room_jid,
            waddle_id: "w-1".to_string(),
            channel_id: "c-1".to_string(),
            config: RoomConfig::default(),
        })
        .await
        .expect("recreate room after explicit destroy")
        .actor_ref;
    assert!(recreated.is_alive());
}

// ---------------------------------------------------------------------------
// #1135 — durable-recipient hydration at spawn
// ---------------------------------------------------------------------------

use crate::muc::affiliation::{DurableMembershipFuture, DurableMembershipSource};
use crate::muc::room_actor::GetRoomSnapshot;
use std::sync::Arc;

/// Fake durable membership store: returns a fixed member list and
/// records the (waddle_id, channel_id) pairs it was queried with.
struct StaticMembershipSource {
    members: Vec<BareJid>,
    queries: std::sync::Mutex<Vec<(String, String)>>,
}

impl StaticMembershipSource {
    fn new(members: Vec<BareJid>) -> Self {
        Self {
            members,
            queries: std::sync::Mutex::new(Vec::new()),
        }
    }
}

impl DurableMembershipSource for StaticMembershipSource {
    fn list_durable_member_jids(
        &self,
        waddle_id: &str,
        channel_id: &str,
    ) -> DurableMembershipFuture<'_> {
        let members = self.members.clone();
        if let Ok(mut queries) = self.queries.lock() {
            queries.push((waddle_id.to_string(), channel_id.to_string()));
        }
        Box::pin(async move { Ok(members) })
    }
}

async fn spawn_registry_with_source(
    source: Arc<dyn DurableMembershipSource>,
) -> ActorRef<RoomRegistryActor> {
    RoomRegistryActor::spawn(
        RoomRegistryActor::new(
            "muc.example.com".to_string(),
            OccupantIdSecret::for_testing(b"test-secret".to_vec()),
        )
        .with_membership_source(source),
    )
}

fn test_bare(value: &str) -> BareJid {
    value.parse().expect("valid bare JID")
}

/// #1135 acceptance 1: a freshly spawned room actor (no joins, no
/// point mutations — the post-deploy/respawn state) must report the
/// durable members from the membership source as durable inbox
/// recipients.
#[tokio::test]
async fn respawned_room_reports_durable_members_as_recipients_without_any_join() {
    let offline_member = test_bare("offline-member@example.com");
    let source = Arc::new(StaticMembershipSource::new(vec![offline_member.clone()]));
    let registry = spawn_registry_with_source(source.clone()).await;

    let room_actor: ActorRef<RoomActor> = registry
        .ask(GetOrCreateRoom {
            room_jid: test_room_jid("respawned"),
            waddle_id: "w-1".to_string(),
            channel_id: "c-1".to_string(),
            config: RoomConfig::default(),
        })
        .await
        .expect("get_or_create")
        .actor_ref;

    let snapshot = room_actor
        .ask(GetRoomSnapshot {
            sender_jid: "someone@example.com/web".parse().expect("valid full JID"),
        })
        .await
        .expect("snapshot");

    assert_eq!(
        snapshot.durable_recipient_bare_jids,
        vec![offline_member],
        "fresh actor incarnation must hydrate durable members from the \
         membership source instead of only join-observed affiliations"
    );
    let queries = source.queries.lock().expect("queries lock").clone();
    assert_eq!(
        queries,
        vec![("w-1".to_string(), "c-1".to_string())],
        "hydration must query the membership source with the room's \
         waddle_id + channel_id exactly once"
    );
}

/// #1135 clobber-safety: hydration must never downgrade a richer
/// affiliation observed at runtime. The hydrated set lives beside the
/// affiliation list (it is never written into it), so a post-spawn
/// Owner grant survives and the member appears exactly once in the
/// recipient set.
#[tokio::test]
async fn hydration_does_not_clobber_richer_runtime_affiliations() {
    use crate::muc::room_actor::{ChangeAffiliation, ListAffiliations};
    use crate::types::Affiliation;

    let alice = test_bare("alice@example.com");
    let source = Arc::new(StaticMembershipSource::new(vec![alice.clone()]));
    let registry = spawn_registry_with_source(source).await;

    let room_actor: ActorRef<RoomActor> = registry
        .ask(GetOrCreateRoom {
            room_jid: test_room_jid("clobber-safety"),
            waddle_id: "w-1".to_string(),
            channel_id: "c-1".to_string(),
            config: RoomConfig::default(),
        })
        .await
        .expect("get_or_create")
        .actor_ref;

    room_actor
        .ask(ChangeAffiliation {
            jid: alice.clone(),
            affiliation: Affiliation::Owner,
        })
        .await
        .expect("promote alice to owner");

    let entries = room_actor
        .ask(ListAffiliations { filter: None })
        .await
        .expect("list affiliations");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].jid, alice);
    assert_eq!(
        entries[0].affiliation,
        Affiliation::Owner,
        "hydrated durable membership must not downgrade a richer \
         runtime-granted affiliation"
    );

    let snapshot = room_actor
        .ask(GetRoomSnapshot {
            sender_jid: "someone@example.com/web".parse().expect("valid full JID"),
        })
        .await
        .expect("snapshot");
    assert_eq!(
        snapshot.durable_recipient_bare_jids,
        vec![alice],
        "a JID both hydrated and runtime-affiliated appears exactly once"
    );
}

/// #1135: a durable member banned at runtime (Outcast) must drop out
/// of the durable recipient set immediately — the runtime demotion
/// wins over the spawn-time hydrated mirror.
#[tokio::test]
async fn runtime_outcast_demotion_excludes_hydrated_durable_member() {
    use crate::muc::room_actor::ChangeAffiliation;
    use crate::types::Affiliation;

    let banned = test_bare("banned@example.com");
    let kept = test_bare("kept@example.com");
    let source = Arc::new(StaticMembershipSource::new(vec![
        banned.clone(),
        kept.clone(),
    ]));
    let registry = spawn_registry_with_source(source).await;

    let room_actor: ActorRef<RoomActor> = registry
        .ask(GetOrCreateRoom {
            room_jid: test_room_jid("runtime-ban"),
            waddle_id: "w-1".to_string(),
            channel_id: "c-1".to_string(),
            config: RoomConfig::default(),
        })
        .await
        .expect("get_or_create")
        .actor_ref;

    room_actor
        .ask(ChangeAffiliation {
            jid: banned.clone(),
            affiliation: Affiliation::Outcast,
        })
        .await
        .expect("ban member");

    let snapshot = room_actor
        .ask(GetRoomSnapshot {
            sender_jid: "someone@example.com/web".parse().expect("valid full JID"),
        })
        .await
        .expect("snapshot");
    assert_eq!(
        snapshot.durable_recipient_bare_jids,
        vec![kept],
        "runtime Outcast demotion must exclude the hydrated durable \
         member from inbox fan-out without waiting for a respawn"
    );
}

/// #1135 acceptance 2 + 3 — restart simulation, end-to-end through the
/// room handler chain's inbox projection:
///
/// 1. durable membership exists for an offline member (permission
///    tuples survive the restart; the fake source stands in for them),
/// 2. the room actor is a *fresh incarnation* (deploy/respawn) that the
///    offline member never joined,
/// 3. a sender joins and sends a groupchat message,
/// 4. the offline member still gets a `ProjectGroupchatInbox` event
///    with `is_recipient = true` — the unread-count/inbox-row
///    candidate the interpreter persists.
#[tokio::test]
async fn offline_durable_member_gets_inbox_projection_after_actor_respawn() {
    use crate::muc::room_actor::{JoinAffiliationGrant, JoinWithAffiliation};
    use crate::protocol::event::OutboundEvent;
    use crate::protocol::id_gen::FixedIdGenerator;
    use crate::protocol::room::inbox::MucInboxHandler;
    use crate::protocol::room::{OccupantSnapshot, RoomContext, RoomHandler, RoomHandlerOutcome};
    use crate::types::Affiliation;
    use jid::FullJid;

    let offline_member = test_bare("offline-member@example.com");
    let source = Arc::new(StaticMembershipSource::new(vec![offline_member.clone()]));
    let registry = spawn_registry_with_source(source).await;

    let room_jid = test_room_jid("restarted-channel");
    let room_actor: ActorRef<RoomActor> = registry
        .ask(GetOrCreateRoom {
            room_jid: room_jid.clone(),
            waddle_id: "w-1".to_string(),
            channel_id: "c-1".to_string(),
            config: RoomConfig::default(),
        })
        .await
        .expect("get_or_create")
        .actor_ref;

    // Only the sender rejoins after the "restart".
    let sender: FullJid = "sender@example.com/web".parse().expect("valid full JID");
    room_actor
        .ask(JoinWithAffiliation {
            sender_jid: sender.clone(),
            nick: "sender".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: 0,
        })
        .await
        .expect("sender join");

    // Same snapshot → context flow as the DispatchToRoom interpreter arm.
    let snapshot = room_actor
        .ask(GetRoomSnapshot {
            sender_jid: sender.clone(),
        })
        .await
        .expect("snapshot");
    let occupants: Vec<OccupantSnapshot> = snapshot
        .occupants
        .iter()
        .map(|o| OccupantSnapshot {
            full_jid: o.full_jid.clone(),
            nick: o.nick.clone(),
            affiliation: o.affiliation,
            role: o.role,
        })
        .collect();
    let id_gen = FixedIdGenerator("ignored".to_string());
    let secret = OccupantIdSecret::for_testing(b"test-secret".to_vec());
    let ctx = RoomContext {
        room: &room_jid,
        sender_full: &sender,
        occupants: &occupants,
        durable_recipient_bare_jids: &snapshot.durable_recipient_bare_jids,
        managed_room_forbidden: false,
        room_moderated: snapshot.config.moderated,
        room_occupants_may_change_subject: false,
        room_members_only: snapshot.config.members_only,
        pin_permission: snapshot.config.pin_permission,
        id_gen: &id_gen,
        occupant_id_secret: &secret,
        sender_nickname_generation: snapshot.sender_nickname_generation.unwrap_or(0),
        project_sender_inbox: true,
        synthetic_sender_authority: None,
        dispatch_timestamp: 1_777_000_000,
    };

    let mut message = xmpp_parsers::message::Message::new(None::<jid::Jid>);
    message.from = Some(jid::Jid::from(sender.clone()));
    message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    message.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "hello after the deploy".to_string(),
    );

    let RoomHandlerOutcome::Continue(events) = MucInboxHandler.handle(&mut message, &ctx) else {
        panic!("inbox handler never halts");
    };
    let offline_projection = events
        .iter()
        .find_map(|event| match event {
            OutboundEvent::ProjectGroupchatInbox {
                owner,
                is_recipient,
                is_durable_recipient,
                is_live_occupant,
                ..
            } if owner == &offline_member => {
                Some((*is_recipient, *is_durable_recipient, *is_live_occupant))
            }
            _ => None,
        })
        .expect(
            "offline durable member must receive a ProjectGroupchatInbox \
             candidate after an actor respawn (#1135)",
        );
    assert_eq!(
        offline_projection,
        (true, true, false),
        "offline member's row must bump unread (is_recipient) as a \
         durable, non-live recipient"
    );
}

// ---------------------------------------------------------------------------
// ADR-0017 Phase 3 Slice 7: durable MUC room ownership + re-election.
// ---------------------------------------------------------------------------

mod ownership_claims_tests {
    use super::*;
    use crate::muc::affiliation::AffiliationEntry;
    use crate::muc::durable::{
        DurableRoomState, MucDurableFuture, MucDurableStore, RoomClaimFenceContext,
    };
    use crate::muc::room_actor::{
        GetAffiliation, GetConfig, IsSealed, JoinAffiliationGrant, JoinWithAffiliation,
        ResolverAffiliationSyncOutcome, SyncResolverAffiliation, UpdateConfig,
    };
    use crate::muc::subject::SubjectState;
    use crate::ownership::{
        ClaimEpoch, ClaimError, ClaimSnapshot, ClaimStore, Entity, EntityType, InProcessClaimStore,
        NodeIdentity, ResumeIdentityProof, SharedNodeIdentity, StalePredicate,
    };
    use crate::types::Affiliation;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
    use std::sync::Mutex;

    fn foreign_identity() -> NodeIdentity {
        NodeIdentity::new("foreign-node", "foreign-epoch")
    }

    fn this_identity() -> NodeIdentity {
        NodeIdentity::new("this-node", "this-epoch")
    }

    #[tokio::test]
    async fn get_or_create_room_acquires_the_claim_and_destroy_releases_it() {
        let registry = spawn_registry().await;
        let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
        let identity = SharedNodeIdentity::new(this_identity());
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store),
                node_identity: identity.clone(),
                durable_store: None,
                rollout_backoff: None,
            })
            .await
            .expect("wire");

        let jid = test_room_jid("claimed");
        let entity = Entity::new(EntityType::RoomActor, jid.to_string());

        registry
            .ask(GetOrCreateRoom {
                room_jid: jid.clone(),
                waddle_id: "w-1".to_string(),
                channel_id: "c-1".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("get_or_create_room");

        let snapshot = claim_store
            .current_claim(&entity)
            .await
            .expect("current_claim")
            .expect("claim exists after spawn");
        assert_eq!(snapshot.owner, this_identity());

        registry
            .ask(DestroyRoom {
                room_jid: jid.clone(),
                reason: DestroyRoomReason::Destroy,
            })
            .await
            .expect("destroy");

        assert!(
            claim_store
                .current_claim(&entity)
                .await
                .expect("current_claim")
                .is_none(),
            "DestroyRoom must release the Postgres claim (element 7's \
             'graceful release')"
        );
    }

    /// XEP-0045 §10.9 (#1261): an explicit `DestroyRoom` must wipe the
    /// room's durable rows (config/subject/affiliations incl. bans) so
    /// the destroyed room cannot resurrect from storage on the next
    /// join — "the room ... destroys the room, even if it was defined
    /// as persistent".
    #[tokio::test]
    async fn destroy_room_deletes_durable_room_state() {
        let registry = spawn_registry().await;
        let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
        let durable_store = Arc::new(RecordingDurableStore::default());
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store),
                node_identity: SharedNodeIdentity::new(this_identity()),
                durable_store: Some(Arc::clone(&durable_store) as Arc<dyn MucDurableStore>),
                rollout_backoff: None,
            })
            .await
            .expect("wire");

        let jid = test_room_jid("durable-destroy");
        registry
            .ask(GetOrCreateRoom {
                room_jid: jid.clone(),
                waddle_id: "w-1".to_string(),
                channel_id: "c-1".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("get_or_create_room");

        registry
            .ask(DestroyRoom {
                room_jid: jid.clone(),
                reason: DestroyRoomReason::Destroy,
            })
            .await
            .expect("destroy");

        assert_eq!(
            *durable_store.deleted_rooms.lock().expect("lock"),
            vec![jid.to_string()],
            "DestroyRoom must delete the durable room state exactly once"
        );
    }

    /// A destroy whose durable delete fails must FAIL (returning
    /// `false` and keeping the room registered) — acknowledging it
    /// would leave rows behind that resurrect the "destroyed" room on
    /// the next join.
    #[tokio::test]
    async fn destroy_room_fails_and_keeps_room_when_durable_delete_fails() {
        let registry = spawn_registry().await;
        let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
        let durable_store = Arc::new(RecordingDurableStore {
            fail_deletes: true,
            ..RecordingDurableStore::default()
        });
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store),
                node_identity: SharedNodeIdentity::new(this_identity()),
                durable_store: Some(Arc::clone(&durable_store) as Arc<dyn MucDurableStore>),
                rollout_backoff: None,
            })
            .await
            .expect("wire");

        let jid = test_room_jid("durable-destroy-fails");
        registry
            .ask(GetOrCreateRoom {
                room_jid: jid.clone(),
                waddle_id: "w-1".to_string(),
                channel_id: "c-1".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("get_or_create_room");

        let destroyed = registry
            .ask(DestroyRoom {
                room_jid: jid.clone(),
                reason: DestroyRoomReason::Destroy,
            })
            .await
            .expect("destroy ask");
        assert_eq!(
            destroyed,
            DestroyRoomOutcome::DurableWipeFailed,
            "a destroy whose durable delete failed must not be acknowledged"
        );
        let still_there = registry
            .ask(GetRoom {
                room_jid: jid.clone(),
            })
            .await
            .expect("get room");
        assert!(
            still_there.is_some(),
            "the room stays registered so the destroy can be retried"
        );
    }

    /// The deposed-node eviction path (fenced fan-out check observed a
    /// steal) evicts the LOCAL actor only — the room lives on under
    /// its new owner, so `DestroyRoomReason::DeposedEviction` MUST NOT
    /// wipe the durable rows. Without the split, a same-node re-claim
    /// racing the queued eviction could pass the write fence and wipe
    /// a legitimately re-claimed room's config/subject/ban list.
    #[tokio::test]
    async fn deposed_eviction_bypasses_release_backlog_without_deleting_durable_state() {
        let registry = spawn_registry().await;
        let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
        let durable_store = Arc::new(RecordingDurableStore::default());
        let identity = SharedNodeIdentity::new(this_identity());
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store),
                node_identity: identity.clone(),
                durable_store: Some(Arc::clone(&durable_store) as Arc<dyn MucDurableStore>),
                rollout_backoff: None,
            })
            .await
            .expect("wire");

        let jid = test_room_jid("deposed-evict");
        let stale_actor = registry
            .ask(GetOrCreateRoom {
                room_jid: jid.clone(),
                waddle_id: "w-1".to_string(),
                channel_id: "c-1".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("get_or_create_room")
            .actor_ref;

        for index in 0..MAX_PENDING_ROOM_RELEASES {
            let pending_jid = test_room_jid(&format!("deposed-backlog-{index}"));
            assert!(registry
                .ask(RememberOrdinaryReleaseForTest {
                    room_jid: pending_jid.clone(),
                    claim_fence: room_claim_fence(&pending_jid, ClaimEpoch(index as i64)),
                })
                .await
                .expect("fill exact-release backlog"));
        }
        identity.rotate(foreign_identity()).await;

        assert_eq!(
            registry
            .ask(DestroyRoom {
                room_jid: jid.clone(),
                reason: DestroyRoomReason::DeposedEviction,
            })
            .await
            .expect("evict"),
            DestroyRoomOutcome::Destroyed,
            "a serve-fence-proven deposed actor must be evicted even when local release capacity is full"
        );

        assert!(
            durable_store.deleted_rooms.lock().expect("lock").is_empty(),
            "a deposed eviction must never delete the room's durable state"
        );
        assert_eq!(registry.ask(RoomCount).await.expect("room count"), 0);
        stale_actor.wait_for_shutdown().await;
        assert!(!stale_actor.is_alive());
        assert!(
            stale_actor.ask(crate::muc::room_actor::IsDormant).await.is_err(),
            "a stale ActorRef must be unusable even if the best-effort Demote notification never arrives"
        );
        assert!(
            claim_store
                .current_claim(&Entity::new(EntityType::RoomActor, jid.to_string()))
                .await
                .expect("claim lookup")
                .is_none(),
            "deposed eviction must still attempt exact release for identity-rotation cases"
        );
        assert_eq!(
            registry
                .ask(GetPendingRoomReleaseBacklog)
                .await
                .expect("bounded backlog")
                .depth,
            MAX_PENDING_ROOM_RELEASES,
            "deposed eviction must never grow the saturated retry inventory"
        );
    }

    /// ADR-0017 Phase 3 Slice 7 FIX 3 (council-adjudicated): `live_room`'s
    /// dead-actor branch must capture the entry's claim epoch and release
    /// the Postgres claim BEFORE removing the entry — previously it just
    /// removed the entry, orphaning the claim (Postgres kept attributing
    /// the room to this node with no local record left to release it).
    #[tokio::test]
    async fn dead_actor_detection_releases_the_orphaned_claim() {
        let registry = spawn_registry().await;
        let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
        let identity = SharedNodeIdentity::new(this_identity());
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store),
                node_identity: identity.clone(),
                durable_store: None,
                rollout_backoff: None,
            })
            .await
            .expect("wire");

        let jid = test_room_jid("panics");
        let entity = Entity::new(EntityType::RoomActor, jid.to_string());

        let acquisition = registry
            .ask(GetOrCreateRoom {
                room_jid: jid.clone(),
                waddle_id: "w-1".to_string(),
                channel_id: "c-1".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("get_or_create_room");
        assert_eq!(acquisition.creation, RoomCreation::Created);
        let actor_ref = acquisition.actor_ref;
        assert!(
            claim_store
                .current_claim(&entity)
                .await
                .expect("current_claim")
                .is_some(),
            "claim must be held immediately after spawn"
        );

        // Simulate the actor panicking: hard-kill it without going
        // through the graceful `DestroyRoom` path.
        actor_ref.kill();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        while actor_ref.is_alive() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "actor did not die in time"
            );
            tokio::task::yield_now().await;
        }

        // `GetRoom` (any op that routes through `live_room`) detects the
        // dead actor and must release the claim as a side effect, even
        // though this ask itself reports `RoomActorStateLost`.
        let result = registry
            .ask(GetRoom {
                room_jid: jid.clone(),
            })
            .await;
        assert!(matches!(
            result,
            Err(SendError::HandlerError(RoomRegistryError::RoomActorStateLost(ref got))) if *got == jid
        ));

        assert!(
            claim_store
                .current_claim(&entity)
                .await
                .expect("current_claim")
                .is_none(),
            "the dead actor's Postgres claim must be released, not orphaned"
        );

        // Proves the release is genuine, not merely internal bookkeeping:
        // a DIFFERENT node/registry can now acquire the same entity.
        claim_store
            .acquire(&entity, &foreign_identity())
            .await
            .expect("another node can now acquire the previously-orphaned claim");
    }

    #[tokio::test]
    async fn get_or_create_room_refuses_when_claim_is_held_by_a_live_foreign_node() {
        let registry = spawn_registry().await;
        let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
        let jid = test_room_jid("foreign-claimed");
        let entity = Entity::new(EntityType::RoomActor, jid.to_string());
        claim_store
            .acquire(&entity, &foreign_identity())
            .await
            .expect("foreign acquire");

        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store),
                node_identity: SharedNodeIdentity::new(this_identity()),
                durable_store: None,
                rollout_backoff: None,
            })
            .await
            .expect("wire");

        let result = registry
            .ask(GetOrCreateRoom {
                room_jid: jid.clone(),
                waddle_id: "w-1".to_string(),
                channel_id: "c-1".to_string(),
                config: RoomConfig::default(),
            })
            .await;

        assert!(
            matches!(
                result,
                Err(SendError::HandlerError(RoomRegistryError::ClaimHeldByAnotherNode(ref room)))
                    if *room == jid
            ),
            "a live foreign owner must not be displaced (cross-node proxy \
             routing is Phase 4 scope, not this slice's) — got {result:?}"
        );
    }

    /// A `ClaimStore` fake whose single claim always reports
    /// `owner_lease_fresh: false` — simulating a dead owner (the node's
    /// own liveness lease has expired) so `steal_stale(OwnerStale)` is the
    /// re-election path under test, distinct from
    /// [`InProcessClaimStore`]'s "always fresh" single-node contract.
    struct DeadOwnerClaimStore {
        state: Mutex<Option<(NodeIdentity, ClaimEpoch)>>,
        steal_calls: AtomicUsize,
        current_claim_failures: AtomicUsize,
        fence_calls: AtomicUsize,
        fence_fail_on_call: AtomicUsize,
        fence_lose_claim_on_call: AtomicUsize,
        release_failures: AtomicUsize,
        release_delay_ms: AtomicU64,
        fence_delay_ms: AtomicU64,
        ensure_delay_ms: AtomicU64,
        ensure_post_commit_delay_ms: AtomicU64,
        steal_post_commit_delay_ms: AtomicU64,
        force_next_steal_conflict: AtomicBool,
        drop_claim_on_forced_steal_conflict: AtomicBool,
    }

    impl DeadOwnerClaimStore {
        fn seeded(owner: NodeIdentity, epoch: ClaimEpoch) -> Self {
            Self {
                state: Mutex::new(Some((owner, epoch))),
                steal_calls: AtomicUsize::new(0),
                current_claim_failures: AtomicUsize::new(0),
                fence_calls: AtomicUsize::new(0),
                fence_fail_on_call: AtomicUsize::new(usize::MAX),
                fence_lose_claim_on_call: AtomicUsize::new(usize::MAX),
                release_failures: AtomicUsize::new(0),
                release_delay_ms: AtomicU64::new(0),
                fence_delay_ms: AtomicU64::new(0),
                ensure_delay_ms: AtomicU64::new(0),
                ensure_post_commit_delay_ms: AtomicU64::new(0),
                steal_post_commit_delay_ms: AtomicU64::new(0),
                force_next_steal_conflict: AtomicBool::new(false),
                drop_claim_on_forced_steal_conflict: AtomicBool::new(false),
            }
        }

        fn empty() -> Self {
            let mut store = Self::seeded(this_identity(), ClaimEpoch(0));
            *store.state.get_mut().expect("lock") = None;
            store
        }

        fn fail_fence_on_call(&self, call: usize) {
            self.fence_fail_on_call.store(call, Ordering::SeqCst);
        }

        fn lose_claim_on_fence_call(&self, call: usize) {
            self.fence_lose_claim_on_call.store(call, Ordering::SeqCst);
        }

        fn fail_next_current_claim(&self) {
            self.current_claim_failures.store(1, Ordering::SeqCst);
        }

        fn fail_next_release(&self) {
            self.release_failures.store(1, Ordering::SeqCst);
        }

        fn set_release_delay(&self, delay: std::time::Duration) {
            self.release_delay_ms.store(
                u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
                Ordering::SeqCst,
            );
        }

        fn set_fence_delay(&self, delay: std::time::Duration) {
            self.fence_delay_ms.store(
                u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
                Ordering::SeqCst,
            );
        }

        fn set_ensure_delay(&self, delay: std::time::Duration) {
            self.ensure_delay_ms.store(
                u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
                Ordering::SeqCst,
            );
        }

        fn set_ensure_post_commit_delay(&self, delay: std::time::Duration) {
            self.ensure_post_commit_delay_ms.store(
                u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
                Ordering::SeqCst,
            );
        }

        fn set_steal_post_commit_delay(&self, delay: std::time::Duration) {
            self.steal_post_commit_delay_ms.store(
                u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
                Ordering::SeqCst,
            );
        }

        fn conflict_next_steal(&self, drop_claim: bool) {
            self.drop_claim_on_forced_steal_conflict
                .store(drop_claim, Ordering::SeqCst);
            self.force_next_steal_conflict.store(true, Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl ClaimStore for DeadOwnerClaimStore {
        async fn ensure_schema(&self) -> Result<(), ClaimError> {
            Ok(())
        }

        async fn acquire(
            &self,
            _entity: &Entity,
            me: &NodeIdentity,
        ) -> Result<ClaimEpoch, ClaimError> {
            let mut state = self.state.lock().expect("lock");
            if state.is_some() {
                return Err(ClaimError::AlreadyClaimed);
            }
            *state = Some((me.clone(), ClaimEpoch(0)));
            Ok(ClaimEpoch(0))
        }

        async fn ensure_claimed(
            &self,
            entity: &Entity,
            me: &NodeIdentity,
        ) -> Result<ClaimEpoch, ClaimError> {
            let delay_ms = self.ensure_delay_ms.load(Ordering::SeqCst);
            if delay_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }
            let existing = self.state.lock().expect("lock").clone();
            let result = match existing {
                None => self.acquire(entity, me).await,
                Some((owner, epoch)) if owner == *me => Ok(epoch),
                Some(_) => Err(ClaimError::AlreadyClaimed),
            };
            if result.is_ok() {
                let delay_ms = self.ensure_post_commit_delay_ms.load(Ordering::SeqCst);
                if delay_ms > 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                }
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
            if self.force_next_steal_conflict.swap(false, Ordering::SeqCst) {
                if self
                    .drop_claim_on_forced_steal_conflict
                    .swap(false, Ordering::SeqCst)
                {
                    *self.state.lock().expect("lock") = None;
                }
                return Err(ClaimError::Conflict);
            }
            let result = {
                let mut state = self.state.lock().expect("lock");
                match &*state {
                    Some((_, epoch)) if *epoch == observed => {
                        let new_epoch = ClaimEpoch(epoch.0 + 1);
                        *state = Some((me.clone(), new_epoch));
                        Ok(new_epoch)
                    }
                    _ => Err(ClaimError::Conflict),
                }
            };
            if result.is_ok() {
                let delay_ms = self.steal_post_commit_delay_ms.load(Ordering::SeqCst);
                if delay_ms > 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                }
            }
            result
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

        async fn current_claim(
            &self,
            _entity: &Entity,
        ) -> Result<Option<ClaimSnapshot>, ClaimError> {
            if self
                .current_claim_failures
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
            {
                return Err(ClaimError::Backend(
                    "test current-claim failure".to_string(),
                ));
            }
            Ok(self
                .state
                .lock()
                .expect("lock")
                .clone()
                .map(|(owner, claim_epoch)| ClaimSnapshot {
                    owner,
                    claim_epoch,
                    owner_lease_fresh: false,
                }))
        }

        async fn fence(
            &self,
            _entity: &Entity,
            me: &NodeIdentity,
            mine: ClaimEpoch,
        ) -> Result<bool, ClaimError> {
            let call = self.fence_calls.fetch_add(1, Ordering::SeqCst) + 1;
            let delay_ms = self.fence_delay_ms.load(Ordering::SeqCst);
            if delay_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }
            if call == self.fence_fail_on_call.load(Ordering::SeqCst) {
                return Err(ClaimError::Backend("test final fence failure".to_string()));
            }
            if call == self.fence_lose_claim_on_call.load(Ordering::SeqCst) {
                *self.state.lock().expect("lock") =
                    Some((foreign_identity(), ClaimEpoch(mine.0.saturating_add(1))));
            }
            Ok(
                matches!(&*self.state.lock().expect("lock"), Some((owner, epoch)) if owner == me && *epoch == mine),
            )
        }

        async fn release(
            &self,
            _entity: &Entity,
            me: &NodeIdentity,
            mine: ClaimEpoch,
        ) -> Result<(), ClaimError> {
            let delay_ms = self.release_delay_ms.load(Ordering::SeqCst);
            if delay_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }
            if self
                .release_failures
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
            {
                return Err(ClaimError::Backend("test release failure".to_string()));
            }
            let mut state = self.state.lock().expect("lock");
            if matches!(&*state, Some((owner, epoch)) if owner == me && *epoch == mine) {
                *state = None;
            }
            Ok(())
        }

        async fn release_exact(
            &self,
            _entity: &Entity,
            me: &NodeIdentity,
            mine: ClaimEpoch,
        ) -> Result<crate::ownership::ExactReleaseOutcome, ClaimError> {
            let delay_ms = self.release_delay_ms.load(Ordering::SeqCst);
            if delay_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }
            if self
                .release_failures
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
            {
                return Err(ClaimError::Backend("test release failure".to_string()));
            }
            let mut state = self.state.lock().expect("lock");
            if matches!(&*state, Some((owner, epoch)) if owner == me && *epoch == mine) {
                *state = None;
                Ok(crate::ownership::ExactReleaseOutcome::Released)
            } else {
                Ok(crate::ownership::ExactReleaseOutcome::NotOwned)
            }
        }

        async fn release_many(
            &self,
            _entities: &[Entity],
            me: &NodeIdentity,
        ) -> Result<(), ClaimError> {
            let mut state = self.state.lock().expect("lock");
            if matches!(&*state, Some((owner, _)) if owner == me) {
                *state = None;
            }
            Ok(())
        }
    }

    /// Multi-entity claim store whose second fence for each room stalls. The
    /// first fence is the detached readiness preflight; the second is the
    /// authoritative publication-boundary fence executed by the registry.
    struct SlowPublicationFenceStore {
        inner: InProcessClaimStore,
        fence_counts: Mutex<HashMap<Entity, usize>>,
        publication_fence_started: tokio::sync::Notify,
        publication_fences_started: AtomicUsize,
        mailbox_marker_seen: Arc<AtomicBool>,
        second_publication_started_before_marker: AtomicBool,
    }

    impl SlowPublicationFenceStore {
        fn new(mailbox_marker_seen: Arc<AtomicBool>) -> Self {
            Self {
                inner: InProcessClaimStore::new(),
                fence_counts: Mutex::new(HashMap::new()),
                publication_fence_started: tokio::sync::Notify::new(),
                publication_fences_started: AtomicUsize::new(0),
                mailbox_marker_seen,
                second_publication_started_before_marker: AtomicBool::new(false),
            }
        }
    }

    #[async_trait]
    impl ClaimStore for SlowPublicationFenceStore {
        async fn ensure_schema(&self) -> Result<(), ClaimError> {
            self.inner.ensure_schema().await
        }

        async fn acquire(
            &self,
            entity: &Entity,
            me: &NodeIdentity,
        ) -> Result<ClaimEpoch, ClaimError> {
            self.inner.acquire(entity, me).await
        }

        async fn ensure_claimed(
            &self,
            entity: &Entity,
            me: &NodeIdentity,
        ) -> Result<ClaimEpoch, ClaimError> {
            self.inner.ensure_claimed(entity, me).await
        }

        async fn steal_stale(
            &self,
            entity: &Entity,
            observed: ClaimEpoch,
            staleness: StalePredicate,
            me: &NodeIdentity,
        ) -> Result<ClaimEpoch, ClaimError> {
            self.inner
                .steal_stale(entity, observed, staleness, me)
                .await
        }

        async fn steal_for_resume(
            &self,
            entity: &Entity,
            observed: ClaimEpoch,
            witness: ResumeIdentityProof,
            me: &NodeIdentity,
        ) -> Result<ClaimEpoch, ClaimError> {
            self.inner
                .steal_for_resume(entity, observed, witness, me)
                .await
        }

        async fn current_claim(
            &self,
            entity: &Entity,
        ) -> Result<Option<ClaimSnapshot>, ClaimError> {
            self.inner.current_claim(entity).await
        }

        async fn fence(
            &self,
            entity: &Entity,
            me: &NodeIdentity,
            mine: ClaimEpoch,
        ) -> Result<bool, ClaimError> {
            let call = {
                let mut counts = self.fence_counts.lock().expect("lock");
                let count = counts.entry(entity.clone()).or_default();
                *count += 1;
                *count
            };
            if call == 2 {
                let publication_number = self
                    .publication_fences_started
                    .fetch_add(1, Ordering::SeqCst)
                    + 1;
                if publication_number == 2 && !self.mailbox_marker_seen.load(Ordering::SeqCst) {
                    self.second_publication_started_before_marker
                        .store(true, Ordering::SeqCst);
                }
                self.publication_fence_started.notify_one();
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            }
            self.inner.fence(entity, me, mine).await
        }

        async fn release(
            &self,
            entity: &Entity,
            me: &NodeIdentity,
            mine: ClaimEpoch,
        ) -> Result<(), ClaimError> {
            self.inner.release(entity, me, mine).await
        }

        async fn release_exact(
            &self,
            entity: &Entity,
            me: &NodeIdentity,
            mine: ClaimEpoch,
        ) -> Result<crate::ownership::ExactReleaseOutcome, ClaimError> {
            self.inner.release_exact(entity, me, mine).await
        }

        async fn release_many(
            &self,
            entities: &[Entity],
            me: &NodeIdentity,
        ) -> Result<(), ClaimError> {
            self.inner.release_many(entities, me).await
        }
    }

    /// The first exact release starts a detached database-side delete and
    /// then never completes its caller future. This models a driver/backend
    /// operation that is not cancellation-safe: the registry times out, but
    /// the original delete can still commit later.
    struct NonCancelSafeReleaseStore {
        inner: Arc<InProcessClaimStore>,
        release_calls: AtomicUsize,
        late_release_started: Arc<tokio::sync::Notify>,
        allow_late_release: Arc<tokio::sync::Notify>,
        late_release_completed: Arc<tokio::sync::Notify>,
    }

    impl NonCancelSafeReleaseStore {
        fn new() -> Self {
            Self {
                inner: Arc::new(InProcessClaimStore::new()),
                release_calls: AtomicUsize::new(0),
                late_release_started: Arc::new(tokio::sync::Notify::new()),
                allow_late_release: Arc::new(tokio::sync::Notify::new()),
                late_release_completed: Arc::new(tokio::sync::Notify::new()),
            }
        }
    }

    #[async_trait]
    impl ClaimStore for NonCancelSafeReleaseStore {
        async fn ensure_schema(&self) -> Result<(), ClaimError> {
            Ok(())
        }

        async fn acquire(
            &self,
            entity: &Entity,
            me: &NodeIdentity,
        ) -> Result<ClaimEpoch, ClaimError> {
            self.inner.acquire(entity, me).await
        }

        async fn ensure_claimed(
            &self,
            entity: &Entity,
            me: &NodeIdentity,
        ) -> Result<ClaimEpoch, ClaimError> {
            self.inner.ensure_claimed(entity, me).await
        }

        async fn steal_stale(
            &self,
            entity: &Entity,
            observed: ClaimEpoch,
            staleness: StalePredicate,
            me: &NodeIdentity,
        ) -> Result<ClaimEpoch, ClaimError> {
            self.inner
                .steal_stale(entity, observed, staleness, me)
                .await
        }

        async fn steal_for_resume(
            &self,
            entity: &Entity,
            observed: ClaimEpoch,
            witness: ResumeIdentityProof,
            me: &NodeIdentity,
        ) -> Result<ClaimEpoch, ClaimError> {
            self.inner
                .steal_for_resume(entity, observed, witness, me)
                .await
        }

        async fn current_claim(
            &self,
            entity: &Entity,
        ) -> Result<Option<ClaimSnapshot>, ClaimError> {
            self.inner.current_claim(entity).await
        }

        async fn fence(
            &self,
            entity: &Entity,
            me: &NodeIdentity,
            mine: ClaimEpoch,
        ) -> Result<bool, ClaimError> {
            self.inner.fence(entity, me, mine).await
        }

        async fn release(
            &self,
            entity: &Entity,
            me: &NodeIdentity,
            mine: ClaimEpoch,
        ) -> Result<(), ClaimError> {
            self.inner.release(entity, me, mine).await
        }

        async fn release_exact(
            &self,
            entity: &Entity,
            me: &NodeIdentity,
            mine: ClaimEpoch,
        ) -> Result<crate::ownership::ExactReleaseOutcome, ClaimError> {
            if self.release_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                let inner = Arc::clone(&self.inner);
                let entity = entity.clone();
                let me = me.clone();
                let started = Arc::clone(&self.late_release_started);
                let allow = Arc::clone(&self.allow_late_release);
                let completed = Arc::clone(&self.late_release_completed);
                tokio::spawn(async move {
                    started.notify_one();
                    allow.notified().await;
                    inner
                        .release_exact(&entity, &me, mine)
                        .await
                        .expect("detached late release");
                    completed.notify_one();
                });
                std::future::pending().await
            } else {
                self.inner.release_exact(entity, me, mine).await
            }
        }

        async fn release_many(
            &self,
            entities: &[Entity],
            me: &NodeIdentity,
        ) -> Result<(), ClaimError> {
            self.inner.release_many(entities, me).await
        }
    }

    /// A [`MucDurableStore`] fake recording `notify_previous_owner_demoted`
    /// calls and returning a fixed `load_room_state` result.
    #[derive(Default)]
    struct RecordingDurableStore {
        load_result: Option<DurableRoomState>,
        fail_load: bool,
        block_all_loads: bool,
        block_load_for: Option<BareJid>,
        load_started: Option<Arc<tokio::sync::Notify>>,
        allow_load: Option<Arc<tokio::sync::Notify>>,
        load_calls: AtomicUsize,
        config_save_calls: AtomicUsize,
        block_next_config_save: AtomicBool,
        config_save_started: Option<Arc<tokio::sync::Notify>>,
        allow_config_save: Option<Arc<tokio::sync::Notify>>,
        stale_config_save_rejected: Arc<AtomicBool>,
        fence_lost: AtomicBool,
        demote_notifications: Mutex<Vec<(String, String)>>,
        deleted_rooms: Mutex<Vec<String>>,
        fail_deletes: bool,
        claim_fences: Arc<Mutex<HashMap<BareJid, RoomClaimFenceContext>>>,
    }

    impl MucDurableStore for RecordingDurableStore {
        fn load_room_state<'a>(
            &'a self,
            _room_jid: &'a BareJid,
        ) -> MucDurableFuture<'a, Option<DurableRoomState>> {
            if self.fail_load {
                return Box::pin(async {
                    Err(crate::XmppError::internal("load refused by test store"))
                });
            }
            let result = self.load_result.clone();
            Box::pin(async move { Ok(result) })
        }

        fn load_room_state_fenced<'a>(
            &'a self,
            room_jid: &'a BareJid,
        ) -> MucDurableFuture<'a, Option<DurableRoomState>> {
            self.load_calls.fetch_add(1, Ordering::SeqCst);
            if !self
                .claim_fences
                .lock()
                .expect("lock")
                .contains_key(room_jid)
            {
                return Box::pin(async {
                    Err(crate::XmppError::internal(
                        "fenced load attempted before exact claim fence was recorded",
                    ))
                });
            }
            if self.fence_lost.load(Ordering::SeqCst) {
                return Box::pin(async {
                    Err(crate::XmppError::internal(
                        "fenced load rejected after ownership loss",
                    ))
                });
            }
            let should_block =
                self.block_all_loads || self.block_load_for.as_ref() == Some(room_jid);
            let load_started = self.load_started.clone();
            let allow_load = self.allow_load.clone();
            Box::pin(async move {
                if should_block {
                    if let Some(started) = load_started {
                        started.notify_one();
                    }
                    if let Some(allow) = allow_load {
                        allow.notified().await;
                    }
                }
                self.load_room_state(room_jid).await
            })
        }

        fn save_config<'a>(
            &'a self,
            room_jid: &'a BareJid,
            _waddle_id: &'a str,
            _channel_id: &'a str,
            _config: &'a RoomConfig,
        ) -> MucDurableFuture<'a, ()> {
            self.config_save_calls.fetch_add(1, Ordering::SeqCst);
            let block = self.block_next_config_save.swap(false, Ordering::SeqCst);
            let config_save_started = self.config_save_started.clone();
            let allow_config_save = self.allow_config_save.clone();
            let starting_fence = self
                .claim_fences
                .lock()
                .expect("lock")
                .get(room_jid)
                .cloned();
            Box::pin(async move {
                if block {
                    let claim_fences = Arc::clone(&self.claim_fences);
                    let stale_rejected = Arc::clone(&self.stale_config_save_rejected);
                    let room_jid = room_jid.clone();
                    tokio::spawn(async move {
                        if let Some(started) = config_save_started {
                            started.notify_one();
                        }
                        if let Some(allow) = allow_config_save {
                            allow.notified().await;
                        }
                        let current_fence =
                            claim_fences.lock().expect("lock").get(&room_jid).cloned();
                        if current_fence != starting_fence {
                            stale_rejected.store(true, Ordering::SeqCst);
                        }
                    });
                    std::future::pending().await
                }
                Ok(())
            })
        }

        fn check_fenced_fanout<'a>(&'a self, _room_jid: &'a BareJid) -> MucDurableFuture<'a, bool> {
            let owned = !self.fence_lost.load(Ordering::SeqCst);
            Box::pin(async move { Ok(owned) })
        }

        fn save_subject<'a>(
            &'a self,
            _room_jid: &'a BareJid,
            _subject: Option<&'a SubjectState>,
        ) -> MucDurableFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }

        fn save_affiliation<'a>(
            &'a self,
            _room_jid: &'a BareJid,
            _entry: &'a AffiliationEntry,
        ) -> MucDurableFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }

        fn delete_room_state<'a>(&'a self, room_jid: &'a BareJid) -> MucDurableFuture<'a, ()> {
            if self.fail_deletes {
                return Box::pin(async {
                    Err(crate::XmppError::internal("delete refused by test store"))
                });
            }
            self.deleted_rooms
                .lock()
                .expect("lock")
                .push(room_jid.to_string());
            Box::pin(async { Ok(()) })
        }

        fn record_claim_fence(&self, room_jid: &BareJid, fence: RoomClaimFenceContext) {
            self.claim_fences
                .lock()
                .expect("lock")
                .insert(room_jid.clone(), fence);
        }

        fn forget_claim_fence(&self, room_jid: &BareJid, expected: &RoomClaimFenceContext) {
            let mut fences = self.claim_fences.lock().expect("lock");
            if fences.get(room_jid) == Some(expected) {
                fences.remove(room_jid);
            }
        }

        fn current_claim_fence(&self, room_jid: &BareJid) -> Option<RoomClaimFenceContext> {
            self.claim_fences
                .lock()
                .expect("lock")
                .get(room_jid)
                .cloned()
        }

        fn notify_previous_owner_demoted<'a>(
            &'a self,
            room_jid: &'a BareJid,
            previous_owner_node_id: &'a str,
            _previous_owner_node_epoch: &'a str,
            _new_epoch: ClaimEpoch,
        ) -> MucDurableFuture<'a, ()> {
            self.demote_notifications
                .lock()
                .expect("lock")
                .push((room_jid.to_string(), previous_owner_node_id.to_string()));
            Box::pin(async { Ok(()) })
        }
    }

    fn blocking_restore_store(
        room_jid: BareJid,
    ) -> (
        Arc<RecordingDurableStore>,
        Arc<tokio::sync::Notify>,
        Arc<tokio::sync::Notify>,
    ) {
        blocking_restore_store_with_result(room_jid, None)
    }

    fn blocking_restore_store_with_result(
        room_jid: BareJid,
        load_result: Option<DurableRoomState>,
    ) -> (
        Arc<RecordingDurableStore>,
        Arc<tokio::sync::Notify>,
        Arc<tokio::sync::Notify>,
    ) {
        let started = Arc::new(tokio::sync::Notify::new());
        let allow = Arc::new(tokio::sync::Notify::new());
        let store = Arc::new(RecordingDurableStore {
            load_result,
            block_load_for: Some(room_jid),
            load_started: Some(Arc::clone(&started)),
            allow_load: Some(Arc::clone(&allow)),
            ..RecordingDurableStore::default()
        });
        (store, started, allow)
    }

    fn restored_room_snapshot(name: &str) -> DurableRoomState {
        DurableRoomState {
            waddle_id: "restored-waddle".to_string(),
            channel_id: "restored-channel".to_string(),
            config: RoomConfig {
                name: name.to_string(),
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

    async fn wire_recording_store(
        registry: &ActorRef<RoomRegistryActor>,
        store: Arc<RecordingDurableStore>,
    ) -> Arc<InProcessClaimStore> {
        let claim_store = Arc::new(InProcessClaimStore::new());
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(this_identity()),
                durable_store: Some(store as Arc<dyn MucDurableStore>),
                rollout_backoff: None,
            })
            .await
            .expect("wire blocking durable store");
        claim_store
    }

    fn get_or_create(room_jid: BareJid) -> GetOrCreateRoom {
        GetOrCreateRoom {
            room_jid,
            waddle_id: "w".to_string(),
            channel_id: "c".to_string(),
            config: RoomConfig::default(),
        }
    }

    #[tokio::test]
    async fn blocked_restore_coalesces_its_lookup_without_blocking_unrelated_rooms() {
        let registry = spawn_registry().await;
        let blocked_jid = test_room_jid("blocked-restore");
        let unrelated_jid = test_room_jid("unrelated-live");
        let (store, started, allow) = blocking_restore_store(blocked_jid.clone());
        wire_recording_store(&registry, Arc::clone(&store)).await;

        registry
            .ask(get_or_create(unrelated_jid.clone()))
            .await
            .expect("create unrelated room");
        let registry_for_create = registry.clone();
        let blocked_for_create = blocked_jid.clone();
        let create = tokio::spawn(async move {
            registry_for_create
                .ask(get_or_create(blocked_for_create))
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(1), started.notified())
            .await
            .expect("blocked restore started");

        let unrelated = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            registry.ask(GetRoom {
                room_jid: unrelated_jid,
            }),
        )
        .await
        .expect("unrelated lookup must not wait for restore")
        .expect("unrelated lookup reply");
        assert!(unrelated.is_some());
        let lookup_registry = registry.clone();
        let lookup_jid = blocked_jid.clone();
        let lookup = tokio::spawn(async move {
            lookup_registry
                .ask(GetRoom {
                    room_jid: lookup_jid,
                })
                .await
        });
        tokio::time::timeout(std::time::Duration::from_millis(200), async {
            loop {
                if registry
                    .ask(PendingPreparationWaitersForTest {
                        room_jid: blocked_jid.clone(),
                    })
                    .await
                    .expect("waiter count")
                    == Some(2)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("same-room lookup coalesces behind restore");
        assert!(!lookup.is_finished());

        allow.notify_one();
        create
            .await
            .expect("create task")
            .expect("blocked room publishes after restore");
        assert!(lookup
            .await
            .expect("lookup task")
            .expect("lookup reply")
            .is_some());
        assert_eq!(store.load_calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn concurrent_same_room_creations_coalesce_behind_one_restore() {
        let registry = spawn_registry().await;
        let room_jid = test_room_jid("coalesced-restore");
        let (store, started, allow) = blocking_restore_store(room_jid.clone());
        wire_recording_store(&registry, Arc::clone(&store)).await;

        let first_registry = registry.clone();
        let first_jid = room_jid.clone();
        let first = tokio::spawn(async move { first_registry.ask(get_or_create(first_jid)).await });
        tokio::time::timeout(std::time::Duration::from_secs(1), started.notified())
            .await
            .expect("first restore started");
        let second_registry = registry.clone();
        let second_jid = room_jid.clone();
        let second =
            tokio::spawn(async move { second_registry.ask(get_or_create(second_jid)).await });

        tokio::time::timeout(std::time::Duration::from_millis(200), async {
            loop {
                if registry
                    .ask(PendingPreparationWaitersForTest {
                        room_jid: room_jid.clone(),
                    })
                    .await
                    .expect("waiter count")
                    == Some(2)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("duplicate request coalesced");
        assert_eq!(
            store.load_calls.load(Ordering::SeqCst),
            1,
            "coalescing must spawn only one actor/restore"
        );

        allow.notify_one();
        let first = first.await.expect("first task").expect("first creation");
        let second = second.await.expect("second task").expect("second creation");
        assert_eq!(first.creation, RoomCreation::Created);
        assert_eq!(second.creation, RoomCreation::Existing);
        assert_eq!(first.actor_ref.id(), second.actor_ref.id());
    }

    #[tokio::test]
    async fn concurrent_waiters_for_restored_room_are_all_existing() {
        let registry = spawn_registry().await;
        let room_jid = test_room_jid("coalesced-restored-room");
        let (store, started, allow) = blocking_restore_store_with_result(
            room_jid.clone(),
            Some(restored_room_snapshot("restored")),
        );
        wire_recording_store(&registry, Arc::clone(&store)).await;

        let first_registry = registry.clone();
        let first_jid = room_jid.clone();
        let first = tokio::spawn(async move { first_registry.ask(get_or_create(first_jid)).await });
        tokio::time::timeout(std::time::Duration::from_secs(1), started.notified())
            .await
            .expect("restore started");
        let second_registry = registry.clone();
        let second_jid = room_jid.clone();
        let second =
            tokio::spawn(async move { second_registry.ask(get_or_create(second_jid)).await });

        tokio::time::timeout(std::time::Duration::from_millis(200), async {
            loop {
                if registry
                    .ask(PendingPreparationWaitersForTest {
                        room_jid: room_jid.clone(),
                    })
                    .await
                    .expect("waiter count")
                    == Some(2)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("duplicate request coalesced");

        allow.notify_one();
        let first = first.await.expect("first task").expect("first acquisition");
        let second = second
            .await
            .expect("second task")
            .expect("second acquisition");
        assert_eq!(first.creation, RoomCreation::Existing);
        assert_eq!(second.creation, RoomCreation::Existing);
        assert_eq!(first.actor_ref.id(), second.actor_ref.id());
        assert_eq!(store.load_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn exclusive_create_rejects_existing_durable_room() {
        let registry = spawn_registry().await;
        let room_jid = test_room_jid("exclusive-restored-room");
        let store = Arc::new(RecordingDurableStore {
            load_result: Some(restored_room_snapshot("restored")),
            ..RecordingDurableStore::default()
        });
        wire_recording_store(&registry, store).await;

        assert!(matches!(
            registry
                .ask(CreateRoom {
                    room_jid: room_jid.clone(),
                    waddle_id: "caller-waddle".to_string(),
                    channel_id: "caller-channel".to_string(),
                    config: RoomConfig::default(),
                })
                .await,
            Err(SendError::HandlerError(RoomRegistryError::RoomAlreadyExists(ref room)))
                if *room == room_jid
        ));
        assert!(registry
            .ask(GetRoom {
                room_jid: room_jid.clone(),
            })
            .await
            .expect("restored room lookup")
            .is_some());
    }

    #[tokio::test]
    async fn cancelled_fresh_creator_handoff_promotes_next_waiter() {
        let registry = spawn_registry().await;
        let room_jid = test_room_jid("cancelled-creator-handoff");
        let (store, started, allow) = blocking_restore_store(room_jid.clone());
        wire_recording_store(&registry, store).await;

        let first_registry = registry.clone();
        let first_jid = room_jid.clone();
        let first = tokio::spawn(async move { first_registry.ask(get_or_create(first_jid)).await });
        tokio::time::timeout(std::time::Duration::from_secs(1), started.notified())
            .await
            .expect("restore started");
        first.abort();
        let second_registry = registry.clone();
        let second_jid = room_jid.clone();
        let second =
            tokio::spawn(async move { second_registry.ask(get_or_create(second_jid)).await });

        tokio::time::timeout(std::time::Duration::from_millis(200), async {
            loop {
                if registry
                    .ask(PendingPreparationWaitersForTest {
                        room_jid: room_jid.clone(),
                    })
                    .await
                    .expect("waiter count")
                    == Some(2)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("replacement creator coalesced");

        allow.notify_one();
        let acquisition = second
            .await
            .expect("second task")
            .expect("replacement creator acquisition");
        assert_eq!(acquisition.creation, RoomCreation::Created);
        assert!(registry
            .ask(GetRoom {
                room_jid: room_jid.clone(),
            })
            .await
            .expect("published room lookup")
            .is_some());
    }

    #[tokio::test]
    async fn cancelled_fresh_creator_cannot_handoff_an_incompatible_creation_spec() {
        let registry = spawn_registry().await;
        let room_jid = test_room_jid("cancelled-incompatible-creator-handoff");
        let (store, started, allow) = blocking_restore_store(room_jid.clone());
        wire_recording_store(&registry, store).await;

        let first_registry = registry.clone();
        let first_jid = room_jid.clone();
        let first = tokio::spawn(async move {
            first_registry
                .ask(GetOrCreateRoom {
                    room_jid: first_jid,
                    waddle_id: "managed-waddle".to_string(),
                    channel_id: "managed-channel".to_string(),
                    config: RoomConfig {
                        name: "Managed room".to_string(),
                        members_only: true,
                        persistent: true,
                        ..RoomConfig::default()
                    },
                })
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(1), started.notified())
            .await
            .expect("restore started");
        first.abort();

        let replacement_registry = registry.clone();
        let replacement_jid = room_jid.clone();
        let replacement = tokio::spawn(async move {
            replacement_registry
                .ask(CreateInstantRoom {
                    room_jid: replacement_jid,
                })
                .await
        });
        tokio::time::timeout(std::time::Duration::from_millis(200), async {
            loop {
                if registry
                    .ask(PendingPreparationWaitersForTest {
                        room_jid: room_jid.clone(),
                    })
                    .await
                    .expect("waiter count")
                    == Some(2)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("incompatible replacement coalesced");

        allow.notify_one();
        assert!(matches!(
            replacement.await.expect("replacement task"),
            Err(SendError::HandlerError(
                RoomRegistryError::OwnershipUnavailable(ref room)
            )) if *room == room_jid
        ));
        assert!(registry
            .ask(GetRoom {
                room_jid: room_jid.clone(),
            })
            .await
            .expect("unpublished incompatible room lookup")
            .is_none());

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if registry
                    .ask(GetPendingRoomReleaseBacklog)
                    .await
                    .expect("release backlog")
                    .depth
                    == 0
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("canceled preparation claim released");

        let retry_registry = registry.clone();
        let retry_jid = room_jid.clone();
        let retry = tokio::spawn(async move {
            retry_registry
                .ask(CreateInstantRoom {
                    room_jid: retry_jid,
                })
                .await
        });
        allow.notify_one();
        let acquisition = retry
            .await
            .expect("retry task")
            .expect("instant room retry");
        assert_eq!(acquisition.creation, RoomCreation::Created);
    }

    #[tokio::test]
    async fn same_room_preparation_waiters_are_bounded() {
        let registry = spawn_registry().await;
        let room_jid = test_room_jid("bounded-preparation-waiters");
        let (store, started, _allow) = blocking_restore_store(room_jid.clone());
        wire_recording_store(&registry, store).await;

        let mut waiters = Vec::with_capacity(MAX_ROOM_PREPARATION_WAITERS);
        let first_registry = registry.clone();
        let first_jid = room_jid.clone();
        waiters.push(tokio::spawn(async move {
            first_registry.ask(get_or_create(first_jid)).await
        }));
        tokio::time::timeout(std::time::Duration::from_secs(1), started.notified())
            .await
            .expect("restore started");
        for _ in 1..MAX_ROOM_PREPARATION_WAITERS {
            let waiter_registry = registry.clone();
            let waiter_jid = room_jid.clone();
            waiters.push(tokio::spawn(async move {
                waiter_registry.ask(get_or_create(waiter_jid)).await
            }));
        }
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if registry
                    .ask(PendingPreparationWaitersForTest {
                        room_jid: room_jid.clone(),
                    })
                    .await
                    .expect("waiter count")
                    == Some(MAX_ROOM_PREPARATION_WAITERS)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("waiter inventory fills to its bound");

        assert!(matches!(
            tokio::time::timeout(
                std::time::Duration::from_millis(200),
                registry.ask(get_or_create(room_jid.clone())),
            )
            .await
            .expect("saturated waiter fails immediately"),
            Err(SendError::HandlerError(
                RoomRegistryError::OwnershipReconciliationPending(ref room)
            )) if *room == room_jid
        ));
        registry.kill();
        for waiter in waiters {
            waiter.abort();
        }
    }

    #[test]
    fn ownership_capacity_scan_stops_at_the_shared_cap() {
        let responsibilities = (0..=MAX_PENDING_ROOM_OWNERSHIP_RESPONSIBILITIES)
            .map(|index| {
                let room_jid = test_room_jid(&format!("bounded-scan-{index}"));
                let claim_fence = room_claim_fence(&room_jid, ClaimEpoch(index as i64 + 1));
                (room_jid, claim_fence)
            })
            .collect::<Vec<_>>();
        let inspected = std::cell::Cell::new(0usize);
        let mut pending = HashSet::with_capacity(MAX_PENDING_ROOM_OWNERSHIP_RESPONSIBILITIES);

        assert!(
            !RoomRegistryActor::extend_pending_room_ownership_responsibilities_until_full(
                &mut pending,
                responsibilities.iter().map(|(room_jid, claim_fence)| {
                    let next = inspected.get() + 1;
                    assert!(
                        next <= MAX_PENDING_ROOM_OWNERSHIP_RESPONSIBILITIES,
                        "capacity scan consumed an entry after reaching the shared cap"
                    );
                    inspected.set(next);
                    PendingRoomOwnershipResponsibility::Exact {
                        room_jid,
                        claim_fence,
                    }
                }),
            )
        );
        assert_eq!(inspected.get(), MAX_PENDING_ROOM_OWNERSHIP_RESPONSIBILITIES);
        assert_eq!(pending.len(), MAX_PENDING_ROOM_OWNERSHIP_RESPONSIBILITIES);
    }

    #[test]
    fn saturated_capacity_admits_existing_but_rejects_novel_responsibility() {
        let mut registry = RoomRegistryActor::new(
            "muc.example.com".to_string(),
            OccupantIdSecret::for_testing(b"test-secret".to_vec()),
        );
        for index in 0..MAX_PENDING_ROOM_RELEASES {
            let room_jid = test_room_jid(&format!("saturated-release-{index}"));
            let claim_fence = room_claim_fence(&room_jid, ClaimEpoch(index as i64 + 1));
            registry.pending_room_releases.insert(
                (room_jid, claim_fence),
                PendingRoomReleaseState {
                    retry_order: index as u64,
                    first_pending_at: std::time::Instant::now(),
                },
            );
        }
        for index in 0..MAX_PENDING_RECLAIMED_ROOMS {
            registry
                .pending_reclaimed_reservations
                .insert(test_room_jid(&format!("saturated-reservation-{index}")));
        }

        assert!(!registry.can_admit_new_room_ownership_responsibility());
        let ((existing_room_jid, existing_claim_fence), _) = registry
            .pending_room_releases
            .iter()
            .next()
            .expect("saturated release inventory is populated");
        assert!(registry.can_admit_room_ownership_responsibility(
            PendingRoomOwnershipResponsibility::Exact {
                room_jid: existing_room_jid,
                claim_fence: existing_claim_fence,
            }
        ));
        let existing_reservation = registry
            .pending_reclaimed_reservations
            .iter()
            .next()
            .expect("saturated reservation inventory is populated");
        assert!(registry.can_admit_room_ownership_responsibility(
            PendingRoomOwnershipResponsibility::ReclaimedReservation(existing_reservation)
        ));

        let novel_room_jid = test_room_jid("saturated-novel");
        let novel_claim_fence = room_claim_fence(&novel_room_jid, ClaimEpoch(999));
        assert!(!registry.can_admit_room_ownership_responsibility(
            PendingRoomOwnershipResponsibility::Exact {
                room_jid: &novel_room_jid,
                claim_fence: &novel_claim_fence,
            }
        ));
    }

    #[tokio::test]
    async fn healthy_rooms_are_excluded_but_foreign_rooms_consume_capacity() {
        let current_identity = this_identity();
        let identity = SharedNodeIdentity::new(current_identity.clone());
        let mut registry = RoomRegistryActor::new(
            "muc.example.com".to_string(),
            OccupantIdSecret::for_testing(b"test-secret".to_vec()),
        );
        registry.node_identity = identity.clone();
        for index in 0..=MAX_PENDING_ROOM_OWNERSHIP_RESPONSIBILITIES {
            let room_jid = test_room_jid(&format!("healthy-capacity-{index}"));
            let actor_ref = RoomActor::spawn(RoomActor::new(
                MucRoom::new(
                    room_jid.clone(),
                    "waddle".to_string(),
                    "channel".to_string(),
                    RoomConfig::default(),
                ),
                OccupantIdSecret::for_testing(b"test-secret".to_vec()),
            ));
            registry.rooms.insert(
                room_jid.clone(),
                RoomEntry {
                    actor_ref,
                    claim_fence: RoomClaimFenceContext::new(
                        Entity::new(EntityType::RoomActor, room_jid.to_string()),
                        current_identity.clone(),
                        ClaimEpoch(index as i64 + 1),
                    ),
                },
            );
        }

        assert!(registry.can_admit_new_room_ownership_responsibility());
        identity.rotate(foreign_identity()).await;
        assert!(!registry.can_admit_new_room_ownership_responsibility());
        for entry in registry.rooms.values() {
            entry.actor_ref.kill();
        }
    }

    #[tokio::test]
    async fn dead_room_consumes_the_last_ownership_capacity_slot() {
        let current_identity = this_identity();
        let mut registry = RoomRegistryActor::new(
            "muc.example.com".to_string(),
            OccupantIdSecret::for_testing(b"test-secret".to_vec()),
        );
        registry.node_identity = SharedNodeIdentity::new(current_identity.clone());
        for index in 0..MAX_PENDING_ROOM_RELEASES {
            let room_jid = test_room_jid(&format!("dead-slot-release-{index}"));
            registry.pending_room_releases.insert(
                (
                    room_jid.clone(),
                    room_claim_fence(&room_jid, ClaimEpoch(index as i64 + 1)),
                ),
                PendingRoomReleaseState {
                    retry_order: index as u64,
                    first_pending_at: std::time::Instant::now(),
                },
            );
        }
        for index in 0..(MAX_PENDING_RECLAIMED_ROOMS - 1) {
            registry
                .pending_reclaimed_reservations
                .insert(test_room_jid(&format!("dead-slot-reservation-{index}")));
        }
        let room_jid = test_room_jid("dead-slot-room");
        let actor_ref = RoomActor::spawn(RoomActor::new(
            MucRoom::new(
                room_jid.clone(),
                "waddle".to_string(),
                "channel".to_string(),
                RoomConfig::default(),
            ),
            OccupantIdSecret::for_testing(b"test-secret".to_vec()),
        ));
        registry.rooms.insert(
            room_jid.clone(),
            RoomEntry {
                actor_ref: actor_ref.clone(),
                claim_fence: RoomClaimFenceContext::new(
                    Entity::new(EntityType::RoomActor, room_jid.to_string()),
                    current_identity,
                    ClaimEpoch(999),
                ),
            },
        );

        assert!(registry.can_admit_new_room_ownership_responsibility());
        actor_ref.kill();
        actor_ref.wait_for_shutdown().await;
        assert!(!actor_ref.is_alive());
        assert!(!registry.can_admit_new_room_ownership_responsibility());
    }

    #[tokio::test]
    async fn preparation_and_release_responsibilities_share_one_bound() {
        let registry = spawn_registry().await;
        // Keep the ordinary release backlog below its independent admission
        // limit so new claims may still enter preparation. The combined limit
        // must nevertheless cap the two inventories together.
        let release_count = MAX_PENDING_ROOM_RELEASES / 2;
        for index in 0..release_count {
            let release_jid = test_room_jid(&format!("bounded-release-{index}"));
            assert!(registry
                .ask(RememberOrdinaryReleaseForTest {
                    room_jid: release_jid.clone(),
                    claim_fence: room_claim_fence(&release_jid, ClaimEpoch(index as i64 + 1)),
                })
                .await
                .expect("remember release"));
        }
        let store = Arc::new(RecordingDurableStore {
            block_all_loads: true,
            allow_load: Some(Arc::new(tokio::sync::Notify::new())),
            ..RecordingDurableStore::default()
        });
        wire_recording_store(&registry, store).await;

        let preparation_capacity = MAX_PENDING_ROOM_OWNERSHIP_RESPONSIBILITIES - release_count;
        let mut pending = Vec::with_capacity(preparation_capacity);
        for index in 0..preparation_capacity {
            let pending_registry = registry.clone();
            let pending_jid = test_room_jid(&format!("bounded-preparation-{index}"));
            pending.push(tokio::spawn(async move {
                pending_registry.ask(get_or_create(pending_jid)).await
            }));
        }
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if registry
                    .ask(PendingPreparationCountForTest)
                    .await
                    .expect("preparation count")
                    == preparation_capacity
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("combined responsibility inventory reaches its bound");

        let overflow_jid = test_room_jid("bounded-preparation-overflow");
        assert!(matches!(
            registry.ask(get_or_create(overflow_jid.clone())).await,
            Err(SendError::HandlerError(
                RoomRegistryError::OwnershipReconciliationPending(ref room)
            )) if *room == overflow_jid
        ));
        assert!(!registry
            .ask(ReservePendingReclaimedRoom {
                room_jid: test_room_jid("bounded-reclaimed-overflow"),
            })
            .await
            .expect("reclaimed admission observes the same global bound"));
        let release_overflow_jid = test_room_jid("bounded-release-overflow");
        assert!(!registry
            .ask(RememberOrdinaryReleaseForTest {
                room_jid: release_overflow_jid.clone(),
                claim_fence: room_claim_fence(&release_overflow_jid, ClaimEpoch(999)),
            })
            .await
            .expect("ordinary release admission observes the same global bound"));
        registry.kill();
        for task in pending {
            task.abort();
        }
    }

    #[tokio::test]
    async fn reclaimed_and_release_responsibilities_exhaust_preparation_capacity() {
        let registry = spawn_registry().await;
        for index in 0..MAX_PENDING_RECLAIMED_ROOMS {
            let room_jid = test_room_jid(&format!("bounded-reclaimed-{index}"));
            assert!(registry
                .ask(ReservePendingReclaimedRoom {
                    room_jid: room_jid.clone(),
                })
                .await
                .expect("reserve reclaimed room"));
            if index % 2 == 0 {
                registry
                    .ask(RememberPendingReclaimedRoom {
                        room_jid: room_jid.clone(),
                        claim_fence: room_claim_fence(&room_jid, ClaimEpoch(index as i64 + 1)),
                        previous_owner: foreign_identity(),
                    })
                    .await
                    .expect("replace reservation with exact reclaimed responsibility");
            }
        }
        for index in 0..MAX_PENDING_ROOM_RELEASES {
            let room_jid = test_room_jid(&format!("bounded-release-{index}"));
            assert!(registry
                .ask(RememberOrdinaryReleaseForTest {
                    room_jid: room_jid.clone(),
                    claim_fence: room_claim_fence(&room_jid, ClaimEpoch(index as i64 + 1)),
                })
                .await
                .expect("remember release"));
        }
        wire_recording_store(&registry, Arc::new(RecordingDurableStore::default())).await;

        let overflow_jid = test_room_jid("reclaimed-preparation-overflow");
        assert!(matches!(
            registry.ask(get_or_create(overflow_jid.clone())).await,
            Err(SendError::HandlerError(
                RoomRegistryError::OwnershipReconciliationPending(ref room)
            )) if *room == overflow_jid
        ));
        registry.kill();
    }

    #[tokio::test]
    async fn reclaimed_preparation_overlap_counts_each_exact_fence_once() {
        let registry = spawn_registry().await;
        let epoch = ClaimEpoch(301);
        let claim_store = Arc::new(DeadOwnerClaimStore::seeded(this_identity(), epoch));
        let allow_load = Arc::new(tokio::sync::Notify::new());
        let durable_store = Arc::new(RecordingDurableStore {
            load_result: Some(reclaimed_snapshot("bounded-overlap")),
            block_all_loads: true,
            allow_load: Some(Arc::clone(&allow_load)),
            ..RecordingDurableStore::default()
        });
        registry
            .ask(WireClusteringClaims {
                claim_store: claim_store as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(this_identity()),
                durable_store: Some(durable_store as Arc<dyn MucDurableStore>),
                rollout_backoff: None,
            })
            .await
            .expect("wire");
        let room_jid = test_room_jid("bounded-overlap");
        let claim_fence = room_claim_fence(&room_jid, epoch);
        registry
            .ask(RememberPendingReclaimedRoom {
                room_jid: room_jid.clone(),
                claim_fence: claim_fence.clone(),
                previous_owner: foreign_identity(),
            })
            .await
            .expect("remember exact reclaimed responsibility");

        let reconcile_registry = registry.clone();
        let reconcile_jid = room_jid.clone();
        let reconcile_fence = claim_fence.clone();
        let reconcile = tokio::spawn(async move {
            reconcile_registry
                .ask(ReconcileReclaimedRoom {
                    room_jid: reconcile_jid,
                    claim_fence: reconcile_fence,
                    previous_owner: foreign_identity(),
                })
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if registry
                    .ask(PendingPreparationCountForTest)
                    .await
                    .expect("preparation count")
                    == 1
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("reclaimed room reaches blocked preparation");
        assert_eq!(
            registry
                .ask(PendingRoomOwnershipResponsibilityCountForTest)
                .await
                .expect("responsibility count"),
            1,
            "one exact fence present in reclaimed and preparation state consumes one slot"
        );

        registry
            .ask(RememberPendingReclaimedRoom {
                room_jid: room_jid.clone(),
                claim_fence: room_claim_fence(&room_jid, ClaimEpoch(epoch.0 + 1)),
                previous_owner: foreign_identity(),
            })
            .await
            .expect("remember newer exact generation");
        assert_eq!(
            registry
                .ask(PendingRoomOwnershipResponsibilityCountForTest)
                .await
                .expect("responsibility count"),
            2,
            "different exact fences for one room remain distinct responsibilities"
        );

        registry.kill();
        reconcile.abort();
    }

    async fn saturated_registry_with_deposed_room(
        name: &str,
    ) -> (
        ActorRef<RoomRegistryActor>,
        Arc<DeadOwnerClaimStore>,
        SharedNodeIdentity,
        BareJid,
        NodeIdentity,
    ) {
        let registry = spawn_registry().await;
        let owner = this_identity();
        let identity = SharedNodeIdentity::new(owner.clone());
        let claim_store = Arc::new(DeadOwnerClaimStore::empty());
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: identity.clone(),
                durable_store: None,
                rollout_backoff: None,
            })
            .await
            .expect("wire");
        let room_jid = test_room_jid(name);
        registry
            .ask(get_or_create(room_jid.clone()))
            .await
            .expect("create room before saturation");
        for index in 0..MAX_PENDING_ROOM_RELEASES {
            let release_jid = test_room_jid(&format!("{name}-release-{index}"));
            assert!(registry
                .ask(RememberOrdinaryReleaseForTest {
                    room_jid: release_jid.clone(),
                    claim_fence: room_claim_fence(&release_jid, ClaimEpoch(index as i64 + 1),),
                })
                .await
                .expect("fill release inventory"));
        }
        for index in 0..MAX_PENDING_RECLAIMED_ROOMS {
            assert!(registry
                .ask(ReservePendingReclaimedRoom {
                    room_jid: test_room_jid(&format!("{name}-reclaimed-{index}")),
                })
                .await
                .expect("fill reclaimed inventory"));
        }
        identity.rotate(foreign_identity()).await;
        claim_store.fail_next_release();
        assert_eq!(
            registry
                .ask(PendingRoomOwnershipResponsibilityCountForTest)
                .await
                .expect("saturated responsibility count"),
            MAX_PENDING_ROOM_OWNERSHIP_RESPONSIBILITIES + 1,
            "identity rotation turns the live old-identity room into one additional retained responsibility"
        );
        (registry, claim_store, identity, room_jid, owner)
    }

    #[tokio::test]
    async fn saturated_demotion_retains_failed_exact_release() {
        let (registry, claim_store, _identity, room_jid, owner) =
            saturated_registry_with_deposed_room("saturated-demotion").await;

        assert!(registry
            .ask(DemoteRoomIfOwner {
                room_jid: room_jid.clone(),
                owner,
            })
            .await
            .expect("demote old-identity room"));
        assert!(registry
            .ask(IsPendingRoomReleaseOnly {
                room_jid: room_jid.clone(),
            })
            .await
            .expect("deposed fence remains typed"));
        assert_eq!(
            registry
                .ask(PendingRoomOwnershipResponsibilityCountForTest)
                .await
                .expect("post-demotion responsibility count"),
            MAX_PENDING_ROOM_OWNERSHIP_RESPONSIBILITIES + 1,
            "moving the deposed entry into release state is slot-neutral"
        );
        assert!(claim_store
            .current_claim(&Entity::new(EntityType::RoomActor, room_jid.to_string()))
            .await
            .expect("claim lookup")
            .is_some());
        registry.kill();
    }

    #[tokio::test]
    async fn saturated_deposed_eviction_retains_failed_exact_release() {
        let (registry, claim_store, _identity, room_jid, _owner) =
            saturated_registry_with_deposed_room("saturated-deposed-eviction").await;

        assert_eq!(
            registry
                .ask(DestroyRoom {
                    room_jid: room_jid.clone(),
                    reason: DestroyRoomReason::DeposedEviction,
                })
                .await
                .expect("evict deposed room"),
            DestroyRoomOutcome::Destroyed
        );
        assert!(registry
            .ask(IsPendingRoomReleaseOnly {
                room_jid: room_jid.clone(),
            })
            .await
            .expect("deposed fence remains typed"));
        assert_eq!(
            registry
                .ask(PendingRoomOwnershipResponsibilityCountForTest)
                .await
                .expect("post-eviction responsibility count"),
            MAX_PENDING_ROOM_OWNERSHIP_RESPONSIBILITIES + 1,
            "deposed eviction transfers rather than discards the saturated responsibility"
        );
        assert!(claim_store
            .current_claim(&Entity::new(EntityType::RoomActor, room_jid.to_string()))
            .await
            .expect("claim lookup")
            .is_some());
        registry.kill();
    }

    #[tokio::test]
    async fn terminal_drain_cancels_pending_publication_and_releases_claim() {
        let registry = spawn_registry().await;
        let room_jid = test_room_jid("terminal-drain-pending-restore");
        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        let (store, started, allow) = blocking_restore_store(room_jid.clone());
        let claim_store = wire_recording_store(&registry, store).await;

        let create_registry = registry.clone();
        let create_jid = room_jid.clone();
        let create =
            tokio::spawn(async move { create_registry.ask(get_or_create(create_jid)).await });
        tokio::time::timeout(std::time::Duration::from_secs(1), started.notified())
            .await
            .expect("restore started");

        let drained = registry
            .ask(DrainRoomOwnershipForShutdown {
                pending_handoffs: Vec::new(),
            })
            .await
            .expect("terminal drain");
        assert_eq!(drained.released, 1);
        assert_eq!(drained.retained, 0);
        assert!(matches!(
            create.await.expect("create task"),
            Err(SendError::HandlerError(RoomRegistryError::OwnershipUnavailable(ref room)))
                if *room == room_jid
        ));
        allow.notify_one();
        tokio::task::yield_now().await;
        assert!(registry
            .ask(GetRoom {
                room_jid: room_jid.clone(),
            })
            .await
            .expect("lookup after terminal drain")
            .is_none());
        assert!(claim_store
            .current_claim(&entity)
            .await
            .expect("claim lookup")
            .is_none());
    }

    #[tokio::test]
    async fn destroy_cancels_pending_read_then_wipes_and_releases_exact_claim() {
        let registry = spawn_registry().await;
        let room_jid = test_room_jid("destroy-pending-read");
        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        let (store, started, allow) = blocking_restore_store_with_result(
            room_jid.clone(),
            Some(restored_room_snapshot("destroyed")),
        );
        let claim_store = wire_recording_store(&registry, Arc::clone(&store)).await;

        let create_registry = registry.clone();
        let create_jid = room_jid.clone();
        let create =
            tokio::spawn(async move { create_registry.ask(get_or_create(create_jid)).await });
        tokio::time::timeout(std::time::Duration::from_secs(1), started.notified())
            .await
            .expect("restore started");

        assert_eq!(
            registry
                .ask(DestroyRoom {
                    room_jid: room_jid.clone(),
                    reason: DestroyRoomReason::Destroy,
                })
                .await
                .expect("destroy pending room"),
            DestroyRoomOutcome::Destroyed,
        );
        assert!(matches!(
            create.await.expect("create task"),
            Err(SendError::HandlerError(RoomRegistryError::OwnershipUnavailable(ref room)))
                if *room == room_jid
        ));
        allow.notify_one();
        assert!(registry
            .ask(GetRoom {
                room_jid: room_jid.clone(),
            })
            .await
            .expect("lookup after destroy")
            .is_none());
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if claim_store
                    .current_claim(&entity)
                    .await
                    .expect("claim lookup")
                    .is_none()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached exact release completes");
        assert_eq!(
            *store.deleted_rooms.lock().expect("deleted rooms"),
            vec![room_jid.to_string()]
        );
    }

    #[tokio::test]
    async fn matching_owner_demotion_cancels_pending_publication() {
        let registry = spawn_registry().await;
        let room_jid = test_room_jid("demote-pending-restore");
        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        let (store, started, allow) = blocking_restore_store(room_jid.clone());
        let claim_store = wire_recording_store(&registry, Arc::clone(&store)).await;

        let create_registry = registry.clone();
        let create_jid = room_jid.clone();
        let create =
            tokio::spawn(async move { create_registry.ask(get_or_create(create_jid)).await });
        tokio::time::timeout(std::time::Duration::from_secs(1), started.notified())
            .await
            .expect("restore started");

        assert!(registry
            .ask(DemoteRoomIfOwner {
                room_jid: room_jid.clone(),
                owner: this_identity(),
            })
            .await
            .expect("demote pending room"));
        assert!(matches!(
            create.await.expect("create task"),
            Err(SendError::HandlerError(RoomRegistryError::OwnershipUnavailable(ref room)))
                if *room == room_jid
        ));
        allow.notify_one();
        tokio::task::yield_now().await;
        assert!(registry
            .ask(GetRoom {
                room_jid: room_jid.clone(),
            })
            .await
            .expect("lookup after demotion")
            .is_none());
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if claim_store
                    .current_claim(&entity)
                    .await
                    .expect("claim lookup")
                    .is_none()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("demotion releases exact old claim");
        assert!(store.current_claim_fence(&room_jid).is_none());
    }

    #[tokio::test]
    async fn claim_lookup_failure_is_not_misclassified_as_a_remote_owner() {
        let registry = spawn_registry().await;
        let claim_store = Arc::new(DeadOwnerClaimStore::seeded(
            foreign_identity(),
            ClaimEpoch(3),
        ));
        claim_store.fail_next_current_claim();
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(this_identity()),
                durable_store: None,
                rollout_backoff: None,
            })
            .await
            .expect("wire");
        let jid = test_room_jid("claim-lookup-error");

        assert!(matches!(
            registry
                .ask(GetOrCreateRoom {
                    room_jid: jid.clone(),
                    waddle_id: "w".to_string(),
                    channel_id: "c".to_string(),
                    config: RoomConfig::default(),
                })
                .await,
            Err(SendError::HandlerError(RoomRegistryError::OwnershipUnavailable(ref room)))
                if *room == jid
        ));
        assert!(registry
            .ask(GetRoom { room_jid: jid })
            .await
            .expect("room lookup")
            .is_none());
    }

    #[tokio::test]
    async fn claim_disappearing_during_stale_owner_steal_is_reconciliation_pending() {
        let registry = spawn_registry().await;
        let claim_store = Arc::new(DeadOwnerClaimStore::seeded(
            foreign_identity(),
            ClaimEpoch(3),
        ));
        claim_store.conflict_next_steal(true);
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(this_identity()),
                durable_store: None,
                rollout_backoff: None,
            })
            .await
            .expect("wire");
        let jid = test_room_jid("claim-gone-during-steal");

        assert!(matches!(
            registry
                .ask(GetOrCreateRoom {
                    room_jid: jid.clone(),
                    waddle_id: "w".to_string(),
                    channel_id: "c".to_string(),
                    config: RoomConfig::default(),
                })
                .await,
            Err(SendError::HandlerError(
                RoomRegistryError::OwnershipReconciliationPending(ref room)
            )) if *room == jid
        ));
        assert!(registry
            .ask(GetRoom {
                room_jid: jid.clone(),
            })
            .await
            .expect("room lookup")
            .is_none());

        registry
            .ask(GetOrCreateRoom {
                room_jid: jid,
                waddle_id: "w".to_string(),
                channel_id: "c".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("next attempt acquires the now-unclaimed room");
    }

    #[tokio::test]
    async fn invalid_local_stealer_conflict_is_not_misclassified_as_a_remote_owner() {
        let registry = spawn_registry().await;
        let claim_store = Arc::new(DeadOwnerClaimStore::seeded(
            foreign_identity(),
            ClaimEpoch(3),
        ));
        // Postgres returns the same catch-all Conflict when the stale-owner
        // predicate matched but this node's own lease is missing, expired, or
        // draining. Preserve the stale foreign claim to model that branch.
        claim_store.conflict_next_steal(false);
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(this_identity()),
                durable_store: None,
                rollout_backoff: None,
            })
            .await
            .expect("wire");
        let jid = test_room_jid("local-stealer-invalid");

        assert!(matches!(
            registry
                .ask(GetOrCreateRoom {
                    room_jid: jid.clone(),
                    waddle_id: "w".to_string(),
                    channel_id: "c".to_string(),
                    config: RoomConfig::default(),
                })
                .await,
            Err(SendError::HandlerError(
                RoomRegistryError::OwnershipReconciliationPending(ref room)
            )) if *room == jid
        ));
        assert!(registry
            .ask(GetRoom { room_jid: jid })
            .await
            .expect("room lookup")
            .is_none());
    }

    #[tokio::test]
    async fn get_or_create_room_steals_from_a_dead_owner_and_notifies_it() {
        let registry = spawn_registry().await;
        let claim_store = Arc::new(DeadOwnerClaimStore::seeded(
            foreign_identity(),
            ClaimEpoch(3),
        ));
        let durable_store = Arc::new(RecordingDurableStore::default());
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(this_identity()),
                durable_store: Some(Arc::clone(&durable_store) as Arc<dyn MucDurableStore>),
                rollout_backoff: None,
            })
            .await
            .expect("wire");

        let jid = test_room_jid("dead-owner");
        registry
            .ask(GetOrCreateRoom {
                room_jid: jid.clone(),
                waddle_id: "w-1".to_string(),
                channel_id: "c-1".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("get_or_create_room steals from a dead owner");

        assert_eq!(
            claim_store.steal_calls.load(Ordering::SeqCst),
            1,
            "re-election must go through steal_stale(OwnerStale), not a bare acquire"
        );

        // The Demote notification (two-part demotion protocol, part (a))
        // fires on a detached task — give it a moment to land.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if !durable_store
                .demote_notifications
                .lock()
                .expect("lock")
                .is_empty()
            {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "best-effort Demote notification never fired"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let notifications = durable_store
            .demote_notifications
            .lock()
            .expect("lock")
            .clone();
        assert_eq!(
            notifications,
            vec![(jid.to_string(), foreign_identity().node_id)]
        );
    }

    #[tokio::test]
    async fn restore_durable_room_state_applies_before_any_join() {
        let registry = spawn_registry().await;
        let restored_config = RoomConfig {
            name: "restored".to_string(),
            members_only: true,
            ..RoomConfig::default()
        };
        let restored_owner: BareJid = "alice@example.com".parse().expect("valid jid");
        let durable_store = Arc::new(RecordingDurableStore {
            load_result: Some(DurableRoomState {
                waddle_id: "restored-waddle".to_string(),
                channel_id: "restored-channel".to_string(),
                config: restored_config.clone(),
                subject: None,
                affiliations: vec![AffiliationEntry::new(
                    restored_owner.clone(),
                    Affiliation::Owner,
                )],
            }),
            ..RecordingDurableStore::default()
        });
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::new(InProcessClaimStore::new()) as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(this_identity()),
                durable_store: Some(durable_store as Arc<dyn MucDurableStore>),
                rollout_backoff: None,
            })
            .await
            .expect("wire");

        let jid = test_room_jid("restore-me");
        let acquisition = registry
            .ask(GetOrCreateRoom {
                room_jid: jid.clone(),
                waddle_id: "caller-waddle".to_string(),
                channel_id: "caller-channel".to_string(),
                // The caller's freshly-computed default config must be
                // overwritten by the durable restore before any join can
                // observe it (element 7's restore-before-join guarantee).
                config: RoomConfig::default(),
            })
            .await
            .expect("get_or_create_room");
        assert_eq!(acquisition.creation, RoomCreation::Existing);
        let actor_ref = acquisition.actor_ref;

        let config = actor_ref.ask(GetConfig).await.expect("ask");
        assert_eq!(config.name, "restored");
        assert!(config.members_only);

        let affiliation = actor_ref
            .ask(GetAffiliation {
                jid: restored_owner,
            })
            .await
            .expect("ask");
        assert_eq!(affiliation, Affiliation::Owner);
    }

    #[tokio::test]
    async fn failed_initial_restore_is_never_published_on_demand() {
        let registry = spawn_registry().await;
        let claim_store = Arc::new(InProcessClaimStore::new());
        let durable_store = Arc::new(RecordingDurableStore {
            fence_lost: AtomicBool::new(true),
            ..RecordingDurableStore::default()
        });
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(this_identity()),
                durable_store: Some(Arc::clone(&durable_store) as Arc<dyn MucDurableStore>),
                rollout_backoff: None,
            })
            .await
            .expect("wire");

        let jid = test_room_jid("failed-initial-demand-restore");
        let result = registry
            .ask(GetOrCreateRoom {
                room_jid: jid.clone(),
                waddle_id: "caller-waddle".to_string(),
                channel_id: "caller-channel".to_string(),
                config: RoomConfig::default(),
            })
            .await;
        assert!(matches!(
            result,
            Err(SendError::HandlerError(
                RoomRegistryError::OwnershipUnavailable(room)
            )) if room == jid
        ));
        assert!(registry
            .ask(GetRoom {
                room_jid: jid.clone(),
            })
            .await
            .expect("room lookup after failed restore")
            .is_none());
        assert!(claim_store
            .current_claim(&Entity::new(EntityType::RoomActor, jid.to_string()))
            .await
            .expect("claim lookup after failed restore")
            .is_none());
        assert!(
            durable_store.current_claim_fence(&jid).is_none(),
            "a definitively released demand claim must clear its exact durable fence",
        );
    }

    fn reclaimed_snapshot(name: &str) -> DurableRoomState {
        DurableRoomState {
            waddle_id: "reclaimed-waddle".to_string(),
            channel_id: "reclaimed-channel".to_string(),
            config: RoomConfig {
                name: name.to_string(),
                persistent: true,
                ..RoomConfig::default()
            },
            subject: None,
            affiliations: Vec::new(),
        }
    }

    fn room_claim_fence(room_jid: &BareJid, epoch: ClaimEpoch) -> RoomClaimFenceContext {
        RoomClaimFenceContext::new(
            Entity::new(EntityType::RoomActor, room_jid.to_string()),
            this_identity(),
            epoch,
        )
    }

    #[tokio::test]
    async fn reclaimed_room_hydrates_once_at_the_exact_won_epoch() {
        let registry = spawn_registry().await;
        let epoch = ClaimEpoch(4);
        let claim_store = Arc::new(DeadOwnerClaimStore::seeded(this_identity(), epoch));
        let durable_store = Arc::new(RecordingDurableStore {
            load_result: Some(reclaimed_snapshot("proactively restored")),
            ..RecordingDurableStore::default()
        });
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(this_identity()),
                durable_store: Some(durable_store as Arc<dyn MucDurableStore>),
                rollout_backoff: None,
            })
            .await
            .expect("wire");

        let jid = test_room_jid("proactive");
        let first = registry
            .ask(ReconcileReclaimedRoom {
                room_jid: jid.clone(),
                claim_fence: room_claim_fence(&jid, epoch),
                previous_owner: this_identity(),
            })
            .await
            .expect("reconcile");
        assert_eq!(first, ReclaimedRoomOutcome::Hydrated);
        let actor = registry
            .ask(GetRoom {
                room_jid: jid.clone(),
            })
            .await
            .expect("get hydrated room")
            .expect("room exists");
        assert_eq!(
            actor.ask(GetConfig).await.expect("config").name,
            "proactively restored"
        );

        let second = registry
            .ask(ReconcileReclaimedRoom {
                room_jid: jid.clone(),
                claim_fence: room_claim_fence(&jid, epoch),
                previous_owner: this_identity(),
            })
            .await
            .expect("idempotent reconcile");
        assert_eq!(second, ReclaimedRoomOutcome::AlreadyLive);
        assert_eq!(registry.ask(RoomCount).await.expect("count"), 1);
    }

    #[tokio::test]
    async fn failed_initial_reclaimed_restore_is_never_published_and_retains_the_fence() {
        let registry = spawn_registry().await;
        let epoch = ClaimEpoch(5);
        let claim_store = Arc::new(DeadOwnerClaimStore::seeded(this_identity(), epoch));
        let durable_store = Arc::new(RecordingDurableStore {
            load_result: Some(reclaimed_snapshot("must remain hidden")),
            fence_lost: AtomicBool::new(true),
            ..RecordingDurableStore::default()
        });
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(this_identity()),
                durable_store: Some(Arc::clone(&durable_store) as Arc<dyn MucDurableStore>),
                rollout_backoff: None,
            })
            .await
            .expect("wire");

        let jid = test_room_jid("failed-initial-reclaimed-restore");
        let claim_fence = room_claim_fence(&jid, epoch);
        let outcome = registry
            .ask(ReconcileReclaimedRoom {
                room_jid: jid.clone(),
                claim_fence: claim_fence.clone(),
                previous_owner: this_identity(),
            })
            .await
            .expect("reconcile failed restore");
        assert_eq!(outcome, ReclaimedRoomOutcome::PendingRetry);
        assert!(registry
            .ask(GetRoom {
                room_jid: jid.clone(),
            })
            .await
            .expect("room lookup after failed restore")
            .is_none());
        assert_eq!(
            registry
                .ask(GetPendingReclaimedRoomBacklog)
                .await
                .expect("pending reclaimed backlog")
                .depth,
            1,
        );
        assert_eq!(
            durable_store.current_claim_fence(&jid),
            Some(claim_fence),
            "an uncertain reclaimed preparation retains exact-epoch responsibility",
        );
    }

    #[tokio::test]
    async fn reclaimed_room_final_fence_uncertainty_never_installs_actor() {
        let registry = spawn_registry().await;
        let epoch = ClaimEpoch(40);
        let claim_store = Arc::new(DeadOwnerClaimStore::seeded(this_identity(), epoch));
        // Initial authorization, post-load authorization, then the exact
        // publication fence after durable hydration messages are enqueued.
        claim_store.fail_fence_on_call(3);
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(this_identity()),
                durable_store: Some(Arc::new(RecordingDurableStore {
                    load_result: Some(reclaimed_snapshot("must-not-install")),
                    ..RecordingDurableStore::default()
                }) as Arc<dyn MucDurableStore>),
                rollout_backoff: None,
            })
            .await
            .expect("wire");

        let jid = test_room_jid("final-fence");
        let outcome = registry
            .ask(ReconcileReclaimedRoom {
                room_jid: jid.clone(),
                claim_fence: room_claim_fence(&jid, epoch),
                previous_owner: this_identity(),
            })
            .await
            .expect("reconcile");
        assert_eq!(outcome, ReclaimedRoomOutcome::PendingRetry);
        assert!(registry
            .ask(GetRoom { room_jid: jid })
            .await
            .expect("get")
            .is_none());
    }

    #[tokio::test]
    async fn reclaimed_room_without_durable_state_releases_for_demand_recreation() {
        let registry = spawn_registry().await;
        let epoch = ClaimEpoch(8);
        let claim_store = Arc::new(DeadOwnerClaimStore::seeded(this_identity(), epoch));
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(this_identity()),
                durable_store: Some(
                    Arc::new(RecordingDurableStore::default()) as Arc<dyn MucDurableStore>
                ),
                rollout_backoff: None,
            })
            .await
            .expect("wire");

        let jid = test_room_jid("ephemeral-orphan");
        let outcome = registry
            .ask(ReconcileReclaimedRoom {
                room_jid: jid.clone(),
                claim_fence: room_claim_fence(&jid, epoch),
                previous_owner: this_identity(),
            })
            .await
            .expect("reconcile");
        assert_eq!(outcome, ReclaimedRoomOutcome::Released);
        assert!(
            claim_store
                .current_claim(&Entity::new(EntityType::RoomActor, jid.to_string()))
                .await
                .expect("claim lookup")
                .is_none(),
            "an unhydratable room claim must not remain owned by a fresh node"
        );
    }

    #[tokio::test]
    async fn reclaimed_room_load_failure_retains_exact_epoch_for_retry() {
        let registry = spawn_registry().await;
        let epoch = ClaimEpoch(9);
        let claim_store = Arc::new(DeadOwnerClaimStore::seeded(this_identity(), epoch));
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(this_identity()),
                durable_store: Some(Arc::new(RecordingDurableStore {
                    fail_load: true,
                    ..RecordingDurableStore::default()
                }) as Arc<dyn MucDurableStore>),
                rollout_backoff: None,
            })
            .await
            .expect("wire");

        let jid = test_room_jid("load-failure");
        let outcome = registry
            .ask(ReconcileReclaimedRoom {
                room_jid: jid.clone(),
                claim_fence: room_claim_fence(&jid, epoch),
                previous_owner: this_identity(),
            })
            .await
            .expect("reconcile");
        assert_eq!(outcome, ReclaimedRoomOutcome::PendingRetry);
        assert!(
            claim_store
                .current_claim(&Entity::new(EntityType::RoomActor, jid.to_string()))
                .await
                .expect("claim lookup")
                .is_some(),
            "a transient hydrate failure must retain the exact won epoch"
        );
        assert_eq!(
            registry
                .ask(ListPendingReclaimedRooms { limit: 8 })
                .await
                .expect("pending retry")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn identity_rotation_before_room_worker_releases_the_exact_old_fence() {
        let registry = spawn_registry().await;
        let old_owner = this_identity();
        let identity = SharedNodeIdentity::new(old_owner.clone());
        let epoch = ClaimEpoch(10);
        let claim_store = Arc::new(DeadOwnerClaimStore::seeded(old_owner.clone(), epoch));
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: identity.clone(),
                durable_store: Some(Arc::new(RecordingDurableStore {
                    load_result: Some(reclaimed_snapshot("must not hydrate")),
                    ..RecordingDurableStore::default()
                }) as Arc<dyn MucDurableStore>),
                rollout_backoff: None,
            })
            .await
            .expect("wire");
        let jid = test_room_jid("rotate-before-worker");
        let fence = RoomClaimFenceContext::new(
            Entity::new(EntityType::RoomActor, jid.to_string()),
            old_owner,
            epoch,
        );

        identity.rotate(foreign_identity()).await;
        let outcome = registry
            .ask(ReconcileReclaimedRoom {
                room_jid: jid.clone(),
                claim_fence: fence,
                previous_owner: foreign_identity(),
            })
            .await
            .expect("reconcile rotated work");

        assert_eq!(outcome, ReclaimedRoomOutcome::Released);
        assert!(claim_store
            .current_claim(&Entity::new(EntityType::RoomActor, jid.to_string()))
            .await
            .expect("claim lookup")
            .is_none());
        assert!(registry
            .ask(GetRoom { room_jid: jid })
            .await
            .expect("room lookup")
            .is_none());
    }

    #[tokio::test]
    async fn identity_rotation_between_room_retries_releases_the_exact_old_fence() {
        let registry = spawn_registry().await;
        let old_owner = this_identity();
        let identity = SharedNodeIdentity::new(old_owner.clone());
        let epoch = ClaimEpoch(11);
        let claim_store = Arc::new(DeadOwnerClaimStore::seeded(old_owner.clone(), epoch));
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: identity.clone(),
                durable_store: Some(Arc::new(RecordingDurableStore {
                    fail_load: true,
                    ..RecordingDurableStore::default()
                }) as Arc<dyn MucDurableStore>),
                rollout_backoff: None,
            })
            .await
            .expect("wire");
        let jid = test_room_jid("rotate-between-retries");
        let fence = RoomClaimFenceContext::new(
            Entity::new(EntityType::RoomActor, jid.to_string()),
            old_owner,
            epoch,
        );

        let first = registry
            .ask(ReconcileReclaimedRoom {
                room_jid: jid.clone(),
                claim_fence: fence.clone(),
                previous_owner: foreign_identity(),
            })
            .await
            .expect("first reconcile");
        assert_eq!(first, ReclaimedRoomOutcome::PendingRetry);

        identity.rotate(foreign_identity()).await;
        let retried = registry
            .ask(ReconcileReclaimedRoom {
                room_jid: jid.clone(),
                claim_fence: fence,
                previous_owner: foreign_identity(),
            })
            .await
            .expect("retry after rotation");

        assert_eq!(retried, ReclaimedRoomOutcome::Released);
        assert!(claim_store
            .current_claim(&Entity::new(EntityType::RoomActor, jid.to_string()))
            .await
            .expect("claim lookup")
            .is_none());
        assert!(registry
            .ask(ListPendingReclaimedRooms { limit: 8 })
            .await
            .expect("pending retries")
            .is_empty());
    }

    #[tokio::test]
    async fn identity_rotation_during_demand_publication_never_installs_old_fence() {
        let registry = spawn_registry().await;
        let old_owner = this_identity();
        let identity = SharedNodeIdentity::new(old_owner.clone());
        let claim_store = Arc::new(DeadOwnerClaimStore::seeded(old_owner, ClaimEpoch(20)));
        claim_store.set_fence_delay(std::time::Duration::from_millis(100));
        claim_store.fail_next_release();
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: identity.clone(),
                durable_store: Some(
                    Arc::new(RecordingDurableStore::default()) as Arc<dyn MucDurableStore>
                ),
                rollout_backoff: None,
            })
            .await
            .expect("wire");
        let jid = test_room_jid("rotate-at-demand-publish");
        let registry_for_create = registry.clone();
        let jid_for_create = jid.clone();
        let create = tokio::spawn(async move {
            registry_for_create
                .ask(GetOrCreateRoom {
                    room_jid: jid_for_create,
                    waddle_id: "w".to_string(),
                    channel_id: "c".to_string(),
                    config: RoomConfig::default(),
                })
                .await
        });
        while claim_store.fence_calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        identity.rotate(foreign_identity()).await;

        let result = create.await.expect("join create");
        assert!(matches!(
            result,
            Err(SendError::HandlerError(RoomRegistryError::OwnershipUnavailable(ref room)))
                if *room == jid
        ));
        assert!(registry
            .ask(GetRoom {
                room_jid: jid.clone(),
            })
            .await
            .expect("lookup")
            .is_none());
        assert_eq!(
            registry
                .ask(GetPendingRoomReleaseBacklog)
                .await
                .expect("pending exact cleanup")
                .depth,
            1,
            "failed post-hydration cleanup must retain its exact fence"
        );
        assert_eq!(
            registry
                .ask(RetryPendingRoomReleases { limit: 8 })
                .await
                .expect("retry exact cleanup"),
            1
        );
        assert!(claim_store
            .current_claim(&Entity::new(EntityType::RoomActor, jid.to_string()))
            .await
            .expect("claim lookup")
            .is_none());
    }

    #[tokio::test]
    async fn identity_rotation_during_claim_acquisition_is_not_a_remote_owner() {
        let registry = spawn_registry().await;
        let old_owner = this_identity();
        let identity = SharedNodeIdentity::new(old_owner.clone());
        let claim_store = Arc::new(DeadOwnerClaimStore::empty());
        claim_store.set_ensure_post_commit_delay(std::time::Duration::from_millis(100));
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: identity.clone(),
                durable_store: None,
                rollout_backoff: None,
            })
            .await
            .expect("wire");
        let jid = test_room_jid("rotate-during-claim-acquire");
        let entity = Entity::new(EntityType::RoomActor, jid.to_string());
        let create_registry = registry.clone();
        let create_jid = jid.clone();
        let create = tokio::spawn(async move {
            create_registry
                .ask(GetOrCreateRoom {
                    room_jid: create_jid,
                    waddle_id: "w".to_string(),
                    channel_id: "c".to_string(),
                    config: RoomConfig::default(),
                })
                .await
        });

        while claim_store
            .current_claim(&entity)
            .await
            .expect("claim lookup")
            .is_none()
        {
            tokio::task::yield_now().await;
        }
        identity.rotate(foreign_identity()).await;

        assert!(matches!(
            create.await.expect("create task"),
            Err(SendError::HandlerError(
                RoomRegistryError::OwnershipReconciliationPending(ref room)
            )) if *room == jid
        ));
        assert!(registry
            .ask(GetRoom {
                room_jid: jid.clone(),
            })
            .await
            .expect("room lookup")
            .is_none());
        assert!(claim_store
            .current_claim(&entity)
            .await
            .expect("released old claim")
            .is_none());
    }

    #[tokio::test]
    async fn queued_rotation_wins_before_the_final_publication_guard() {
        let registry = spawn_registry().await;
        let old_owner = this_identity();
        let identity = SharedNodeIdentity::new(old_owner.clone());
        let boundary_blocker = identity
            .guard_if_current(&old_owner)
            .await
            .expect("old identity starts current");
        let claim_store = Arc::new(DeadOwnerClaimStore::seeded(
            old_owner.clone(),
            ClaimEpoch(21),
        ));
        claim_store.set_fence_delay(std::time::Duration::from_millis(100));
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: identity.clone(),
                durable_store: None,
                rollout_backoff: None,
            })
            .await
            .expect("wire");
        let jid = test_room_jid("queued-rotation-at-publish-boundary");
        let registry_for_create = registry.clone();
        let jid_for_create = jid.clone();
        let create = tokio::spawn(async move {
            registry_for_create
                .ask(GetOrCreateRoom {
                    room_jid: jid_for_create,
                    waddle_id: "w".to_string(),
                    channel_id: "c".to_string(),
                    config: RoomConfig::default(),
                })
                .await
        });
        while claim_store.fence_calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }

        let rotating_identity = identity.clone();
        let rotation = tokio::spawn(async move {
            rotating_identity.rotate(foreign_identity()).await;
        });
        tokio::task::yield_now().await;
        assert!(
            !rotation.is_finished(),
            "the synthetic boundary guard must hold rotation before publication"
        );
        drop(boundary_blocker);
        rotation.await.expect("rotation task");

        assert!(matches!(
            create.await.expect("join create"),
            Err(SendError::HandlerError(RoomRegistryError::OwnershipUnavailable(ref room)))
                if *room == jid
        ));
        assert!(registry
            .ask(GetRoom {
                room_jid: jid.clone(),
            })
            .await
            .expect("lookup")
            .is_none());
        assert!(claim_store
            .current_claim(&Entity::new(EntityType::RoomActor, jid.to_string()))
            .await
            .expect("claim lookup")
            .is_none());
    }

    #[tokio::test]
    async fn stale_selected_retry_cannot_evict_newer_published_fence_cache() {
        let registry = spawn_registry().await;
        let old_owner = this_identity();
        let new_owner = foreign_identity();
        let identity = SharedNodeIdentity::new(old_owner.clone());
        let old_epoch = ClaimEpoch(30);
        let claim_store = Arc::new(DeadOwnerClaimStore::seeded(old_owner.clone(), old_epoch));
        claim_store.fail_fence_on_call(1);
        let durable_store = Arc::new(RecordingDurableStore::default());
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: identity.clone(),
                durable_store: Some(Arc::clone(&durable_store) as Arc<dyn MucDurableStore>),
                rollout_backoff: None,
            })
            .await
            .expect("wire");
        let jid = test_room_jid("stale-retry-cache");
        let old_fence = RoomClaimFenceContext::new(
            Entity::new(EntityType::RoomActor, jid.to_string()),
            old_owner.clone(),
            old_epoch,
        );
        assert_eq!(
            registry
                .ask(ReconcileReclaimedRoom {
                    room_jid: jid.clone(),
                    claim_fence: old_fence.clone(),
                    previous_owner: old_owner.clone(),
                })
                .await
                .expect("seed pending"),
            ReclaimedRoomOutcome::PendingRetry
        );
        let selected = registry
            .ask(ListPendingReclaimedRooms { limit: 1 })
            .await
            .expect("select old retry")
            .pop()
            .expect("pending old retry");

        identity.rotate(new_owner.clone()).await;
        registry
            .ask(GetOrCreateRoom {
                room_jid: jid.clone(),
                waddle_id: "new-w".to_string(),
                channel_id: "new-c".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("publish newer demand actor");
        let new_fence = durable_store
            .current_claim_fence(&jid)
            .expect("new fence cached");
        assert_eq!(new_fence.owner(), new_owner);
        assert_ne!(
            new_fence, old_fence,
            "owner identity plus epoch identifies the exact generation even when a recreated in-memory claim resets its numeric epoch"
        );

        assert_eq!(
            registry
                .ask(ReconcileReclaimedRoom {
                    room_jid: selected.room_jid,
                    claim_fence: selected.claim_fence,
                    previous_owner: selected.previous_owner,
                })
                .await
                .expect("deliver stale selected retry"),
            ReclaimedRoomOutcome::LostRace
        );
        assert_eq!(durable_store.current_claim_fence(&jid), Some(new_fence));
    }

    #[tokio::test(start_paused = true)]
    async fn hung_demand_claim_store_call_is_bounded_inside_registry_actor() {
        let registry = spawn_registry().await;
        let claim_store = Arc::new(DeadOwnerClaimStore::seeded(this_identity(), ClaimEpoch(40)));
        claim_store.set_ensure_delay(std::time::Duration::from_secs(60));
        registry
            .ask(WireClusteringClaims {
                claim_store: claim_store as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(this_identity()),
                durable_store: None,
                rollout_backoff: None,
            })
            .await
            .expect("wire");
        let jid = test_room_jid("hung-ordinary-claim");
        let registry_for_create = registry.clone();
        let create = tokio::spawn(async move {
            registry_for_create
                .ask(GetOrCreateRoom {
                    room_jid: jid,
                    waddle_id: "w".to_string(),
                    channel_id: "c".to_string(),
                    config: RoomConfig::default(),
                })
                .await
        });
        tokio::task::yield_now().await;
        tokio::time::advance(ROOM_OWNERSHIP_CALL_TIMEOUT).await;
        tokio::task::yield_now().await;
        assert!(matches!(
            create.await.expect("join create"),
            Err(SendError::HandlerError(
                RoomRegistryError::OwnershipUnavailable(_)
            ))
        ));
        assert_eq!(
            registry.ask(RoomCount).await.expect("registry responsive"),
            0
        );
    }

    #[tokio::test(start_paused = true)]
    async fn hung_final_publication_fence_does_not_block_the_registry_mailbox() {
        let registry = spawn_registry().await;
        let claim_store = Arc::new(DeadOwnerClaimStore::empty());
        claim_store.set_fence_delay(std::time::Duration::from_secs(60));
        let durable_store = Arc::new(RecordingDurableStore::default());
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(this_identity()),
                durable_store: Some(durable_store as Arc<dyn MucDurableStore>),
                rollout_backoff: None,
            })
            .await
            .expect("wire");
        let jid = test_room_jid("hung-final-publication-fence");
        let create_registry = registry.clone();
        let create = tokio::spawn(async move {
            create_registry
                .ask(GetOrCreateRoom {
                    room_jid: jid,
                    waddle_id: "w".to_string(),
                    channel_id: "c".to_string(),
                    config: RoomConfig::default(),
                })
                .await
        });
        while claim_store.fence_calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }

        let count_registry = registry.clone();
        let count = tokio::spawn(async move { count_registry.ask(RoomCount).await });
        for _ in 0..8 {
            if count.is_finished() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(
            count.is_finished(),
            "the detached final fence must not hold the registry mailbox"
        );
        assert_eq!(count.await.expect("count task").expect("room count"), 0);

        tokio::time::advance(ROOM_OWNERSHIP_CALL_TIMEOUT).await;
        tokio::task::yield_now().await;
        assert!(matches!(
            create.await.expect("create task"),
            Err(SendError::HandlerError(
                RoomRegistryError::OwnershipUnavailable(_)
            ))
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn ready_publications_yield_the_mailbox_between_bounded_final_fences() {
        let registry = spawn_registry().await;
        let mailbox_marker_seen = Arc::new(AtomicBool::new(false));
        let claim_store = Arc::new(SlowPublicationFenceStore::new(Arc::clone(
            &mailbox_marker_seen,
        )));
        let durable_store = Arc::new(RecordingDurableStore::default());
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(this_identity()),
                durable_store: Some(durable_store as Arc<dyn MucDurableStore>),
                rollout_backoff: None,
            })
            .await
            .expect("wire");

        let mut creates = Vec::new();
        for room in ["ready-fence-one", "ready-fence-two"] {
            let create_registry = registry.clone();
            let jid = test_room_jid(room);
            creates.push(tokio::spawn(async move {
                create_registry.ask(get_or_create(jid)).await
            }));
        }
        claim_store.publication_fence_started.notified().await;

        let marker_registry = registry.clone();
        let marker = Arc::clone(&mailbox_marker_seen);
        let marker_task =
            tokio::spawn(async move { marker_registry.ask(MarkRegistryProgress(marker)).await });
        tokio::task::yield_now().await;
        tokio::time::advance(ROOM_OWNERSHIP_CALL_TIMEOUT).await;
        tokio::task::yield_now().await;
        marker_task
            .await
            .expect("marker task")
            .expect("registry marker");

        while claim_store
            .publication_fences_started
            .load(Ordering::SeqCst)
            < 2
        {
            tokio::task::yield_now().await;
        }
        assert!(mailbox_marker_seen.load(Ordering::SeqCst));
        assert!(
            !claim_store
                .second_publication_started_before_marker
                .load(Ordering::SeqCst),
            "unrelated registry work must run between bounded final fences"
        );
        tokio::time::advance(ROOM_OWNERSHIP_CALL_TIMEOUT).await;
        tokio::task::yield_now().await;
        for create in creates {
            assert!(matches!(
                create.await.expect("create task"),
                Err(SendError::HandlerError(
                    RoomRegistryError::OwnershipUnavailable(_)
                ))
            ));
        }
    }

    #[tokio::test]
    async fn actual_publication_boundary_rejects_claim_lost_after_detached_preflight() {
        let registry = spawn_registry().await;
        let claim_store = Arc::new(DeadOwnerClaimStore::empty());
        claim_store.lose_claim_on_fence_call(2);
        let durable_store = Arc::new(RecordingDurableStore::default());
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(this_identity()),
                durable_store: Some(durable_store as Arc<dyn MucDurableStore>),
                rollout_backoff: None,
            })
            .await
            .expect("wire");
        let jid = test_room_jid("claim-lost-after-publication-preflight");

        assert!(matches!(
            registry
                .ask(GetOrCreateRoom {
                    room_jid: jid.clone(),
                    waddle_id: "w".to_string(),
                    channel_id: "c".to_string(),
                    config: RoomConfig::default(),
                })
                .await,
            Err(SendError::HandlerError(
                RoomRegistryError::OwnershipUnavailable(ref room)
            )) if *room == jid
        ));
        assert_eq!(
            claim_store.fence_calls.load(Ordering::SeqCst),
            2,
            "publication must re-fence after detached readiness preflight"
        );
        assert_eq!(registry.ask(RoomCount).await.expect("room count"), 0);
        assert!(registry
            .ask(GetRoom { room_jid: jid })
            .await
            .expect("lost room lookup")
            .is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn fresh_acquire_commit_before_timeout_is_retained_and_exactly_released() {
        let registry = spawn_registry().await;
        let claim_store = Arc::new(DeadOwnerClaimStore::empty());
        claim_store.set_ensure_post_commit_delay(std::time::Duration::from_secs(60));
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(this_identity()),
                durable_store: None,
                rollout_backoff: None,
            })
            .await
            .expect("wire");
        let jid = test_room_jid("fresh-commit-before-timeout");
        let registry_for_create = registry.clone();
        let jid_for_create = jid.clone();
        let create = tokio::spawn(async move {
            registry_for_create
                .ask(GetOrCreateRoom {
                    room_jid: jid_for_create,
                    waddle_id: "w".to_string(),
                    channel_id: "c".to_string(),
                    config: RoomConfig::default(),
                })
                .await
        });
        tokio::task::yield_now().await;
        tokio::time::advance(ROOM_OWNERSHIP_CALL_TIMEOUT).await;
        tokio::task::yield_now().await;
        assert!(matches!(
            create.await.expect("join create"),
            Err(SendError::HandlerError(
                RoomRegistryError::OwnershipUnavailable(_)
            ))
        ));
        assert_eq!(
            registry
                .ask(GetPendingRoomReleaseBacklog)
                .await
                .expect("pending acquisition")
                .depth,
            1
        );
        claim_store.fail_next_release();
        tokio::time::advance(PENDING_ROOM_RETRY_DELAY).await;
        tokio::task::yield_now().await;
        assert_eq!(
            registry
                .ask(GetPendingRoomReleaseBacklog)
                .await
                .expect("failed release remains scheduled")
                .depth,
            1,
            "a failed exact release must transfer responsibility without losing the retry loop"
        );
        tokio::time::advance(PENDING_ROOM_RETRY_DELAY).await;
        tokio::task::yield_now().await;
        assert_eq!(
            registry
                .ask(GetPendingRoomReleaseBacklog)
                .await
                .expect("automatic acquisition reconciliation")
                .depth,
            0,
            "the actor must redrive transferred release responsibility without an external janitor"
        );
        assert!(claim_store
            .current_claim(&Entity::new(EntityType::RoomActor, jid.to_string()))
            .await
            .expect("claim lookup")
            .is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn terminal_drain_reconciles_a_fresh_acquire_committed_before_timeout() {
        let registry = spawn_registry().await;
        let claim_store = Arc::new(DeadOwnerClaimStore::empty());
        claim_store.set_ensure_post_commit_delay(std::time::Duration::from_secs(60));
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(this_identity()),
                durable_store: None,
                rollout_backoff: None,
            })
            .await
            .expect("wire");
        let jid = test_room_jid("terminal-fresh-commit-before-timeout");
        let create_registry = registry.clone();
        let create_jid = jid.clone();
        let create = tokio::spawn(async move {
            create_registry
                .ask(GetOrCreateRoom {
                    room_jid: create_jid,
                    waddle_id: "w".to_string(),
                    channel_id: "c".to_string(),
                    config: RoomConfig::default(),
                })
                .await
        });
        tokio::task::yield_now().await;
        tokio::time::advance(ROOM_OWNERSHIP_CALL_TIMEOUT).await;
        tokio::task::yield_now().await;
        assert!(matches!(
            create.await.expect("create task"),
            Err(SendError::HandlerError(
                RoomRegistryError::OwnershipUnavailable(_)
            ))
        ));
        assert_eq!(
            registry
                .ask(GetPendingRoomReleaseBacklog)
                .await
                .expect("uncertain acquisition backlog")
                .depth,
            1
        );

        let outcome = registry
            .ask(DrainRoomOwnershipForShutdown {
                pending_handoffs: Vec::new(),
            })
            .await
            .expect("terminal drain");
        assert_eq!(
            outcome,
            RoomOwnershipDrainOutcome {
                released: 1,
                preserved_live: 0,
                retained: 0,
            }
        );
        assert!(claim_store
            .current_claim(&Entity::new(EntityType::RoomActor, jid.to_string()))
            .await
            .expect("claim lookup")
            .is_none());
        assert_eq!(
            registry
                .ask(GetPendingRoomReleaseBacklog)
                .await
                .expect("drained ownership backlog")
                .depth,
            0
        );
    }

    #[tokio::test(start_paused = true)]
    async fn stale_owner_steal_commit_before_timeout_is_retained_and_exactly_released() {
        let registry = spawn_registry().await;
        let claim_store = Arc::new(DeadOwnerClaimStore::seeded(
            foreign_identity(),
            ClaimEpoch(70),
        ));
        claim_store.set_steal_post_commit_delay(std::time::Duration::from_secs(60));
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(this_identity()),
                durable_store: None,
                rollout_backoff: None,
            })
            .await
            .expect("wire");
        let jid = test_room_jid("steal-commit-before-timeout");
        let registry_for_create = registry.clone();
        let jid_for_create = jid.clone();
        let create = tokio::spawn(async move {
            registry_for_create
                .ask(GetOrCreateRoom {
                    room_jid: jid_for_create,
                    waddle_id: "w".to_string(),
                    channel_id: "c".to_string(),
                    config: RoomConfig::default(),
                })
                .await
        });
        tokio::task::yield_now().await;
        tokio::time::advance(ROOM_OWNERSHIP_CALL_TIMEOUT).await;
        tokio::task::yield_now().await;
        assert!(matches!(
            create.await.expect("join create"),
            Err(SendError::HandlerError(
                RoomRegistryError::OwnershipUnavailable(_)
            ))
        ));
        assert_eq!(
            registry
                .ask(GetPendingRoomReleaseBacklog)
                .await
                .expect("pending acquisition")
                .depth,
            1
        );
        assert_eq!(
            registry
                .ask(RetryPendingRoomReleases { limit: 8 })
                .await
                .expect("reconcile acquisition"),
            1
        );
        assert!(claim_store
            .current_claim(&Entity::new(EntityType::RoomActor, jid.to_string()))
            .await
            .expect("claim lookup")
            .is_none());
    }

    #[tokio::test]
    async fn fence_uncertainty_with_live_old_epoch_actor_is_pending_until_exact_proof() {
        let registry = spawn_registry().await;
        let old_epoch = ClaimEpoch(30);
        let new_epoch = ClaimEpoch(31);
        let claim_store = Arc::new(DeadOwnerClaimStore::seeded(this_identity(), old_epoch));
        let durable_store = Arc::new(RecordingDurableStore {
            load_result: Some(reclaimed_snapshot("clean retry snapshot")),
            ..RecordingDurableStore::default()
        });
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(this_identity()),
                durable_store: Some(durable_store as Arc<dyn MucDurableStore>),
                rollout_backoff: None,
            })
            .await
            .expect("wire");
        let jid = test_room_jid("uncertain-old-epoch");
        let original = registry
            .ask(GetOrCreateRoom {
                room_jid: jid.clone(),
                waddle_id: "w".to_string(),
                channel_id: "c".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("spawn old-epoch actor");
        *claim_store.state.lock().expect("claim state") = Some((this_identity(), new_epoch));
        // Make the initial exact proof uncertain. The old actor must remain
        // untouched until a later retry proves the new generation.
        claim_store.fail_fence_on_call(claim_store.fence_calls.load(Ordering::SeqCst) + 1);

        let uncertain = registry
            .ask(ReconcileReclaimedRoom {
                room_jid: jid.clone(),
                claim_fence: room_claim_fence(&jid, new_epoch),
                previous_owner: this_identity(),
            })
            .await
            .expect("uncertain reconcile");
        assert_eq!(uncertain, ReclaimedRoomOutcome::PendingRetry);
        let pending = registry
            .ask(ListPendingReclaimedRooms { limit: 8 })
            .await
            .expect("list pending");
        assert_eq!(
            pending,
            vec![PendingReclaimedRoom {
                room_jid: jid.clone(),
                claim_fence: room_claim_fence(&jid, new_epoch),
                previous_owner: this_identity(),
            }]
        );
        let still_local = registry
            .ask(GetRoom {
                room_jid: jid.clone(),
            })
            .await
            .expect("get local")
            .expect("actor retained while proof is uncertain");
        assert_eq!(still_local.id(), original.actor_ref.id());

        let retried = registry
            .ask(ReconcileReclaimedRoom {
                room_jid: jid.clone(),
                claim_fence: room_claim_fence(&jid, new_epoch),
                previous_owner: this_identity(),
            })
            .await
            .expect("retry with exact proof");
        assert_eq!(retried, ReclaimedRoomOutcome::Hydrated);
        let replacement = registry
            .ask(GetRoom {
                room_jid: jid.clone(),
            })
            .await
            .expect("get replacement")
            .expect("replacement actor");
        assert_ne!(replacement.id(), original.actor_ref.id());
        assert_eq!(
            replacement
                .ask(GetConfig)
                .await
                .expect("replacement config")
                .name,
            "clean retry snapshot"
        );
        assert!(registry
            .ask(ListPendingReclaimedRooms { limit: 8 })
            .await
            .expect("list pending after adoption")
            .is_empty());
    }

    #[tokio::test]
    async fn failed_release_retains_epoch_until_a_retry_confirms_release() {
        let registry = spawn_registry().await;
        let epoch = ClaimEpoch(40);
        let claim_store = Arc::new(DeadOwnerClaimStore::seeded(this_identity(), epoch));
        claim_store.fail_next_release();
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(this_identity()),
                durable_store: None,
                rollout_backoff: None,
            })
            .await
            .expect("wire");
        let jid = test_room_jid("release-failure");

        let first = registry
            .ask(ReconcileReclaimedRoom {
                room_jid: jid.clone(),
                claim_fence: room_claim_fence(&jid, epoch),
                previous_owner: this_identity(),
            })
            .await
            .expect("first reconcile");
        assert_eq!(first, ReclaimedRoomOutcome::PendingRetry);
        assert!(
            claim_store
                .current_claim(&Entity::new(EntityType::RoomActor, jid.to_string()))
                .await
                .expect("claim")
                .is_some(),
            "a failed release must retain the won epoch"
        );
        assert_eq!(
            registry
                .ask(ListPendingReclaimedRooms { limit: 8 })
                .await
                .expect("pending")
                .len(),
            1
        );

        let retried = registry
            .ask(ReconcileReclaimedRoom {
                room_jid: jid.clone(),
                claim_fence: room_claim_fence(&jid, epoch),
                previous_owner: this_identity(),
            })
            .await
            .expect("retry");
        assert_eq!(retried, ReclaimedRoomOutcome::Released);
        assert!(claim_store
            .current_claim(&Entity::new(EntityType::RoomActor, jid.to_string()))
            .await
            .expect("claim after retry")
            .is_none());
    }

    #[tokio::test]
    async fn ordinary_destroy_release_failure_is_deduped_and_retried_exactly() {
        let registry = spawn_registry().await;
        let epoch = ClaimEpoch(41);
        let claim_store = Arc::new(DeadOwnerClaimStore::seeded(this_identity(), epoch));
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(this_identity()),
                durable_store: None,
                rollout_backoff: None,
            })
            .await
            .expect("wire");
        let jid = test_room_jid("ordinary-release-retry");
        registry
            .ask(CreateInstantRoom {
                room_jid: jid.clone(),
            })
            .await
            .expect("create room");

        claim_store.fail_next_release();
        assert_eq!(
            registry
                .ask(DestroyRoom {
                    room_jid: jid.clone(),
                    reason: DestroyRoomReason::Destroy,
                })
                .await
                .expect("destroy"),
            DestroyRoomOutcome::Destroyed
        );
        assert_eq!(
            registry
                .ask(GetPendingRoomReleaseBacklog)
                .await
                .expect("backlog")
                .depth,
            1
        );

        assert_eq!(
            registry
                .ask(RetryPendingRoomReleases { limit: 1 })
                .await
                .expect("retry"),
            1
        );
        assert_eq!(
            registry
                .ask(GetPendingRoomReleaseBacklog)
                .await
                .expect("cleared backlog")
                .depth,
            0
        );
        assert!(claim_store
            .current_claim(&Entity::new(EntityType::RoomActor, jid.to_string()))
            .await
            .expect("claim lookup")
            .is_none());
    }

    #[tokio::test]
    async fn pending_release_generations_survive_claim_epoch_aba() {
        let registry = spawn_registry().await;
        let jid = test_room_jid("release-aba");
        let owner_a = this_identity();
        let owner_b = foreign_identity();
        let fence_a = crate::muc::RoomClaimFenceContext::new(
            Entity::new(EntityType::RoomActor, jid.to_string()),
            owner_a,
            ClaimEpoch(41),
        );
        let fence_b = crate::muc::RoomClaimFenceContext::new(
            Entity::new(EntityType::RoomActor, jid.to_string()),
            owner_b,
            ClaimEpoch(0),
        );
        for fence in [fence_a, fence_b] {
            registry
                .ask(RememberOrdinaryReleaseForTest {
                    room_jid: jid.clone(),
                    claim_fence: fence,
                })
                .await
                .expect("remember exact release");
        }
        assert_eq!(
            registry
                .ask(GetPendingRoomReleaseBacklog)
                .await
                .expect("backlog")
                .depth,
            2,
            "A/41 and recreated B/0 are distinct exact release responsibilities"
        );
    }

    #[tokio::test]
    async fn pending_reclaimed_generations_survive_claim_epoch_aba() {
        let registry = spawn_registry().await;
        let jid = test_room_jid("reclaimed-aba");
        let fence_a = crate::muc::RoomClaimFenceContext::new(
            Entity::new(EntityType::RoomActor, jid.to_string()),
            this_identity(),
            ClaimEpoch(41),
        );
        let fence_c = crate::muc::RoomClaimFenceContext::new(
            Entity::new(EntityType::RoomActor, jid.to_string()),
            foreign_identity(),
            ClaimEpoch(1),
        );
        for (fence, previous_owner) in [
            (fence_a.clone(), foreign_identity()),
            (fence_c.clone(), this_identity()),
        ] {
            registry
                .ask(RememberPendingReclaimedRoom {
                    room_jid: jid.clone(),
                    claim_fence: fence,
                    previous_owner,
                })
                .await
                .expect("remember reclaimed generation");
        }
        let pending = registry
            .ask(ListPendingReclaimedRooms { limit: 8 })
            .await
            .expect("pending generations");
        assert_eq!(pending.len(), 2);
        assert!(pending.iter().any(|entry| entry.claim_fence == fence_a));
        assert!(pending.iter().any(|entry| entry.claim_fence == fence_c));
    }

    #[tokio::test]
    async fn full_release_backlog_does_not_seal_inactive_room() {
        let registry = spawn_registry().await;
        let jid = test_room_jid("must-remain-open");
        let acquisition = registry
            .ask(GetOrCreateRoom {
                room_jid: jid.clone(),
                waddle_id: "w".to_string(),
                channel_id: "c".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("create target before saturating cleanup inventory");
        for index in 0..MAX_PENDING_ROOM_RELEASES {
            let jid = test_room_jid(&format!("backlog-{index}"));
            registry
                .ask(RememberOrdinaryReleaseForTest {
                    room_jid: jid.clone(),
                    claim_fence: room_claim_fence(&jid, ClaimEpoch(index as i64)),
                })
                .await
                .expect("fill backlog");
        }
        assert!(!registry
            .ask(DestroyRoomIfInactive {
                room_jid: jid,
                expected_occupancy_revision: 0,
                guard: SealGuard::Dormant,
            })
            .await
            .expect("capacity refusal"));
        assert!(!acquisition
            .actor_ref
            .ask(IsSealed)
            .await
            .expect("sealed probe"));
    }

    #[tokio::test]
    async fn full_release_backlog_refuses_new_claim_and_never_grows() {
        let registry = spawn_registry().await;
        for index in 0..MAX_PENDING_ROOM_RELEASES {
            let jid = test_room_jid(&format!("bounded-backlog-{index}"));
            assert!(registry
                .ask(RememberOrdinaryReleaseForTest {
                    room_jid: jid.clone(),
                    claim_fence: room_claim_fence(&jid, ClaimEpoch(index as i64)),
                })
                .await
                .expect("fill backlog"));
        }
        let overflow_jid = test_room_jid("bounded-overflow");
        assert!(!registry
            .ask(RememberOrdinaryReleaseForTest {
                room_jid: overflow_jid.clone(),
                claim_fence: room_claim_fence(&overflow_jid, ClaimEpoch(999)),
            })
            .await
            .expect("bounded insertion outcome"));
        assert!(matches!(
            registry
                .ask(GetOrCreateRoom {
                    room_jid: overflow_jid.clone(),
                    waddle_id: "w".to_string(),
                    channel_id: "c".to_string(),
                    config: RoomConfig::default(),
                })
                .await,
            Err(SendError::HandlerError(RoomRegistryError::OwnershipUnavailable(ref room)))
                if *room == overflow_jid
        ));
        assert_eq!(
            registry
                .ask(GetPendingRoomReleaseBacklog)
                .await
                .expect("bounded backlog")
                .depth,
            MAX_PENDING_ROOM_RELEASES
        );
    }

    #[tokio::test]
    async fn blocked_acquisitions_cannot_starve_release_retries() {
        let registry = spawn_registry().await;
        let claim_store = Arc::new(InProcessClaimStore::new());
        let owner = this_identity();
        for index in 0..PENDING_ROOM_RETRY_BATCH {
            let jid = test_room_jid(&format!("fair-acquisition-{index}"));
            claim_store
                .acquire(&Entity::new(EntityType::RoomActor, jid.to_string()), &owner)
                .await
                .expect("seed current-owner claim");
            assert!(registry
                .ask(ReservePendingAcquisitionForTest {
                    room_jid: jid,
                    owner: owner.clone(),
                })
                .await
                .expect("reserve acquisition"));
        }
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(owner),
                durable_store: None,
                rollout_backoff: None,
            })
            .await
            .expect("wire seeded claim store");
        for index in 0..MAX_PENDING_ROOM_RELEASES {
            let jid = test_room_jid(&format!("fair-release-{index}"));
            assert!(registry
                .ask(RememberOrdinaryReleaseForTest {
                    room_jid: jid.clone(),
                    claim_fence: room_claim_fence(&jid, ClaimEpoch(index as i64)),
                })
                .await
                .expect("fill release backlog"));
        }

        assert_eq!(
            registry
                .ask(RetryPendingRoomReleases {
                    limit: PENDING_ROOM_RETRY_BATCH,
                })
                .await
                .expect("defer blocked acquisitions"),
            PENDING_ROOM_RETRY_BATCH
        );
        assert_eq!(
            registry
                .ask(RetryPendingRoomReleases {
                    limit: PENDING_ROOM_RETRY_BATCH,
                })
                .await
                .expect("release retry gets a fair turn"),
            PENDING_ROOM_RETRY_BATCH
        );
        assert_eq!(
            registry
                .ask(GetPendingRoomReleaseBacklog)
                .await
                .expect("backlog after fair release batch")
                .depth,
            MAX_PENDING_ROOM_RELEASES,
            "the second batch must clear releases instead of selecting the same blocked acquisitions forever"
        );
    }

    #[tokio::test]
    async fn pending_only_room_is_shutdown_sealable_but_not_current_live_generation() {
        let registry = spawn_registry().await;
        let jid = test_room_jid("pending-only");
        assert!(registry
            .ask(RememberOrdinaryReleaseForTest {
                room_jid: jid.clone(),
                claim_fence: room_claim_fence(&jid, ClaimEpoch(7)),
            })
            .await
            .expect("remember pending-only generation"));

        assert!(!registry
            .ask(IsCurrentRoomPendingRelease {
                room_jid: jid.clone(),
            })
            .await
            .expect("generation-scoped live query"));
        assert!(registry
            .ask(IsPendingRoomReleaseOnly { room_jid: jid })
            .await
            .expect("shutdown pending-only query"));
        assert!(!registry
            .ask(IsCurrentIdentityPendingRoomReleaseOnly {
                room_jid: test_room_jid("pending-only"),
            })
            .await
            .expect("current-identity shutdown query"));
    }

    #[tokio::test]
    async fn shutdown_batches_only_pending_room_fences_from_current_identity() {
        let registry = spawn_registry().await;
        let current_jid = test_room_jid("pending-current-identity");
        let stale_jid = test_room_jid("pending-stale-identity");
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::new(InProcessClaimStore::new()),
                node_identity: SharedNodeIdentity::new(this_identity()),
                durable_store: None,
                rollout_backoff: None,
            })
            .await
            .expect("wire current identity");
        assert!(registry
            .ask(RememberOrdinaryReleaseForTest {
                room_jid: current_jid.clone(),
                claim_fence: room_claim_fence(&current_jid, ClaimEpoch(8)),
            })
            .await
            .expect("remember current fence"));
        assert!(registry
            .ask(RememberOrdinaryReleaseForTest {
                room_jid: stale_jid.clone(),
                claim_fence: RoomClaimFenceContext::new(
                    Entity::new(EntityType::RoomActor, stale_jid.to_string()),
                    foreign_identity(),
                    ClaimEpoch(9),
                ),
            })
            .await
            .expect("remember stale fence"));

        assert!(registry
            .ask(IsCurrentIdentityPendingRoomReleaseOnly {
                room_jid: current_jid,
            })
            .await
            .expect("current fence query"));
        assert!(!registry
            .ask(IsCurrentIdentityPendingRoomReleaseOnly {
                room_jid: stale_jid,
            })
            .await
            .expect("stale fence query"));
    }

    #[tokio::test]
    async fn pending_release_with_dead_map_entry_is_shutdown_sealable() {
        let registry = spawn_registry().await;
        let jid = test_room_jid("pending-dead-entry");
        let actor = registry
            .ask(GetOrCreateRoom {
                room_jid: jid.clone(),
                waddle_id: "w".to_string(),
                channel_id: "c".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("create room")
            .actor_ref;
        assert!(registry
            .ask(RememberOrdinaryReleaseForTest {
                room_jid: jid.clone(),
                claim_fence: room_claim_fence(&jid, ClaimEpoch(77)),
            })
            .await
            .expect("remember exact pending responsibility"));
        actor.kill();
        actor.wait_for_shutdown().await;

        assert!(registry
            .ask(IsPendingRoomReleaseOnly { room_jid: jid })
            .await
            .expect("dead-entry pending-only query"));
    }

    #[tokio::test]
    async fn dead_actor_redrives_oldest_release_when_backlog_is_full() {
        let registry = spawn_registry().await;
        let target = test_room_jid("dead-under-saturation");
        let actor = registry
            .ask(GetOrCreateRoom {
                room_jid: target.clone(),
                waddle_id: "w".to_string(),
                channel_id: "c".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("create target")
            .actor_ref;
        actor.kill();
        actor.wait_for_shutdown().await;
        for index in 0..MAX_PENDING_ROOM_RELEASES {
            let jid = test_room_jid(&format!("dead-progress-{index}"));
            assert!(registry
                .ask(RememberOrdinaryReleaseForTest {
                    room_jid: jid.clone(),
                    claim_fence: room_claim_fence(&jid, ClaimEpoch(index as i64 + 100)),
                })
                .await
                .expect("fill backlog"));
        }

        assert!(matches!(
            registry
                .ask(GetRoom {
                    room_jid: target.clone(),
                })
                .await,
            Err(SendError::HandlerError(RoomRegistryError::RoomActorStateLost(ref room)))
                if *room == target
        ));
        assert_eq!(
            registry
                .ask(GetPendingRoomReleaseBacklog)
                .await
                .expect("backlog after opportunistic redrive")
                .depth,
            MAX_PENDING_ROOM_RELEASES - 1,
            "one stale exact release is confirmed NotOwned, freeing capacity so the dead actor can retire"
        );
        assert_eq!(registry.ask(RoomCount).await.expect("room count"), 0);
    }

    #[tokio::test]
    async fn stale_pending_messages_cannot_regress_or_clear_a_newer_epoch() {
        let registry = spawn_registry().await;
        let current_epoch = ClaimEpoch(61);
        let stale_epoch = ClaimEpoch(60);
        let claim_store = Arc::new(DeadOwnerClaimStore::seeded(this_identity(), current_epoch));
        registry
            .ask(WireClusteringClaims {
                claim_store: claim_store as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(this_identity()),
                durable_store: None,
                rollout_backoff: None,
            })
            .await
            .expect("wire");
        let jid = test_room_jid("monotonic-pending");
        registry
            .ask(RememberPendingReclaimedRoom {
                room_jid: jid.clone(),
                claim_fence: room_claim_fence(&jid, current_epoch),
                previous_owner: this_identity(),
            })
            .await
            .expect("remember current");
        registry
            .ask(RememberPendingReclaimedRoom {
                room_jid: jid.clone(),
                claim_fence: room_claim_fence(&jid, stale_epoch),
                previous_owner: this_identity(),
            })
            .await
            .expect("late stale remember");
        let stale = registry
            .ask(ReconcileReclaimedRoom {
                room_jid: jid.clone(),
                claim_fence: room_claim_fence(&jid, stale_epoch),
                previous_owner: this_identity(),
            })
            .await
            .expect("stale reconcile");
        assert_eq!(stale, ReclaimedRoomOutcome::LostRace);
        assert_eq!(
            registry
                .ask(ListPendingReclaimedRooms { limit: 8 })
                .await
                .expect("pending"),
            vec![PendingReclaimedRoom {
                room_jid: jid.clone(),
                claim_fence: room_claim_fence(&jid, current_epoch),
                previous_owner: this_identity(),
            }]
        );
    }

    #[tokio::test]
    async fn terminal_drain_resolves_a_won_reservation_without_an_exact_handoff() {
        let registry = spawn_registry().await;
        let claim_store = Arc::new(InProcessClaimStore::new());
        let owner = this_identity();
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(owner.clone()),
                durable_store: None,
                rollout_backoff: None,
            })
            .await
            .expect("wire");
        let jid = test_room_jid("terminal-reservation-handoff");
        assert!(registry
            .ask(ReservePendingReclaimedRoom {
                room_jid: jid.clone(),
            })
            .await
            .expect("reserve before steal"));
        let entity = Entity::new(EntityType::RoomActor, jid.to_string());
        claim_store
            .acquire(&entity, &owner)
            .await
            .expect("seed ambiguously committed claim");

        let outcome = registry
            .ask(DrainRoomOwnershipForShutdown {
                pending_handoffs: Vec::new(),
            })
            .await
            .expect("terminal drain");

        assert_eq!(
            outcome,
            RoomOwnershipDrainOutcome {
                released: 1,
                preserved_live: 0,
                retained: 0,
            }
        );
        assert!(claim_store
            .current_claim(&entity)
            .await
            .expect("read drained claim")
            .is_none());
        assert_eq!(
            registry
                .ask(GetPendingReclaimedRoomBacklog)
                .await
                .expect("terminal backlog"),
            PendingReclaimedRoomBacklog {
                depth: 0,
                oldest_age_ms: 0,
            }
        );
    }

    #[tokio::test]
    async fn terminal_drain_releases_an_already_pending_exact_release() {
        let registry = spawn_registry().await;
        let claim_store = Arc::new(InProcessClaimStore::new());
        let owner = this_identity();
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(owner.clone()),
                durable_store: None,
                rollout_backoff: None,
            })
            .await
            .expect("wire");
        let jid = test_room_jid("terminal-pending-exact-release");
        let entity = Entity::new(EntityType::RoomActor, jid.to_string());
        let epoch = claim_store
            .acquire(&entity, &owner)
            .await
            .expect("seed exact claim");
        assert!(registry
            .ask(RememberOrdinaryReleaseForTest {
                room_jid: jid.clone(),
                claim_fence: RoomClaimFenceContext::new(entity.clone(), owner, epoch),
            })
            .await
            .expect("remember pending exact release"));

        let outcome = registry
            .ask(DrainRoomOwnershipForShutdown {
                pending_handoffs: Vec::new(),
            })
            .await
            .expect("terminal drain");

        assert_eq!(
            outcome,
            RoomOwnershipDrainOutcome {
                released: 1,
                preserved_live: 0,
                retained: 0,
            }
        );
        assert!(claim_store
            .current_claim(&entity)
            .await
            .expect("read drained claim")
            .is_none());
        assert_eq!(
            registry
                .ask(GetPendingRoomReleaseBacklog)
                .await
                .expect("pending release backlog")
                .depth,
            0
        );
    }

    #[tokio::test]
    async fn terminal_drain_retains_a_reservation_until_a_late_steal_commit_is_observed() {
        let registry = spawn_registry().await;
        let claim_store = Arc::new(InProcessClaimStore::new());
        let owner = this_identity();
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(owner.clone()),
                durable_store: None,
                rollout_backoff: None,
            })
            .await
            .expect("wire");
        let jid = test_room_jid("terminal-late-steal-commit");
        let entity = Entity::new(EntityType::RoomActor, jid.to_string());
        assert!(registry
            .ask(ReservePendingReclaimedRoom {
                room_jid: jid.clone(),
            })
            .await
            .expect("reserve before steal"));

        let before_commit = registry
            .ask(DrainRoomOwnershipForShutdown {
                pending_handoffs: Vec::new(),
            })
            .await
            .expect("terminal drain before late commit");
        assert_eq!(
            before_commit,
            RoomOwnershipDrainOutcome {
                released: 0,
                preserved_live: 0,
                retained: 1,
            },
            "an absent snapshot cannot prove that a canceled steal CAS will not commit later"
        );
        assert_eq!(
            registry
                .ask(GetPendingReclaimedRoomBacklog)
                .await
                .expect("ambiguous reservation backlog")
                .depth,
            1
        );

        claim_store
            .acquire(&entity, &owner)
            .await
            .expect("simulate the dropped steal future committing after the first read");
        let after_commit = registry
            .ask(DrainRoomOwnershipForShutdown {
                pending_handoffs: Vec::new(),
            })
            .await
            .expect("terminal drain after late commit");
        assert_eq!(
            after_commit,
            RoomOwnershipDrainOutcome {
                released: 1,
                preserved_live: 0,
                retained: 0,
            }
        );
        assert!(claim_store
            .current_claim(&entity)
            .await
            .expect("read drained late claim")
            .is_none());
        assert_eq!(
            registry
                .ask(GetPendingReclaimedRoomBacklog)
                .await
                .expect("resolved reservation backlog")
                .depth,
            0
        );
    }

    #[tokio::test]
    async fn terminal_drain_keeps_a_discovered_reservation_fence_typed_on_release_failure() {
        let registry = spawn_registry().await;
        let owner = this_identity();
        let epoch = ClaimEpoch(74);
        let claim_store = Arc::new(DeadOwnerClaimStore::seeded(owner.clone(), epoch));
        claim_store.fail_next_release();
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(owner),
                durable_store: None,
                rollout_backoff: None,
            })
            .await
            .expect("wire");
        let jid = test_room_jid("terminal-reservation-release-failure");
        assert!(registry
            .ask(ReservePendingReclaimedRoom {
                room_jid: jid.clone(),
            })
            .await
            .expect("reserve before steal"));

        let outcome = registry
            .ask(DrainRoomOwnershipForShutdown {
                pending_handoffs: Vec::new(),
            })
            .await
            .expect("terminal drain");
        assert_eq!(
            outcome,
            RoomOwnershipDrainOutcome {
                released: 0,
                preserved_live: 0,
                retained: 1,
            }
        );
        assert!(registry
            .ask(IsCurrentIdentityPendingRoomReleaseOnly {
                room_jid: jid.clone(),
            })
            .await
            .expect("typed exact-release inventory"));
        assert_eq!(
            registry
                .ask(GetPendingReclaimedRoomBacklog)
                .await
                .expect("bare reservation transferred to exact inventory")
                .depth,
            0
        );
        assert_eq!(
            registry
                .ask(RetryPendingRoomReleases { limit: 1 })
                .await
                .expect("retry exact release"),
            1
        );
        assert!(!registry
            .ask(IsPendingRoomReleaseOnly { room_jid: jid })
            .await
            .expect("exact fence cleared after successful retry"));
    }

    #[tokio::test]
    async fn terminal_drain_releases_the_active_snapshot_after_local_authority_is_disabled() {
        let registry = spawn_registry().await;
        let claim_store = Arc::new(InProcessClaimStore::new());
        let owner = this_identity();
        let identity = SharedNodeIdentity::new(owner.clone());
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: identity.clone(),
                durable_store: None,
                rollout_backoff: None,
            })
            .await
            .expect("wire");
        let jid = test_room_jid("terminal-disabled-identity");
        let entity = Entity::new(EntityType::RoomActor, jid.to_string());
        claim_store
            .acquire(&entity, &owner)
            .await
            .expect("seed active claim");
        assert!(registry
            .ask(ReservePendingReclaimedRoom {
                room_jid: jid.clone(),
            })
            .await
            .expect("reserve before terminal fencing"));
        identity.disable().await;

        let outcome = registry
            .ask(DrainRoomOwnershipForShutdown {
                pending_handoffs: Vec::new(),
            })
            .await
            .expect("terminal drain");
        assert_eq!(
            outcome,
            RoomOwnershipDrainOutcome {
                released: 1,
                preserved_live: 0,
                retained: 0,
            }
        );
        assert!(claim_store
            .current_claim(&entity)
            .await
            .expect("read drained claim")
            .is_none());
        assert!(!registry
            .ask(IsPendingRoomReleaseOnly { room_jid: jid })
            .await
            .expect("typed release cleared"));
    }

    #[tokio::test]
    async fn terminal_drain_releases_registered_reclaimed_epochs_and_disables_acquisition() {
        let registry = spawn_registry().await;
        let claim_store = Arc::new(InProcessClaimStore::new());
        let owner = this_identity();
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(owner.clone()),
                durable_store: None,
                rollout_backoff: None,
            })
            .await
            .expect("wire");
        let jid = test_room_jid("terminal-reclaimed-drain");
        let entity = Entity::new(EntityType::RoomActor, jid.to_string());
        let epoch = claim_store
            .acquire(&entity, &owner)
            .await
            .expect("seed won claim");
        let fence = RoomClaimFenceContext::new(entity.clone(), owner.clone(), epoch);
        let handoff = PendingReclaimedRoom {
            room_jid: jid.clone(),
            claim_fence: fence.clone(),
            previous_owner: foreign_identity(),
        };
        registry
            .ask(RememberPendingReclaimedRoom {
                room_jid: jid.clone(),
                claim_fence: fence,
                previous_owner: handoff.previous_owner.clone(),
            })
            .await
            .expect("register won claim");

        let outcome = registry
            .ask(DrainRoomOwnershipForShutdown {
                pending_handoffs: vec![handoff],
            })
            .await
            .expect("terminal drain");
        assert_eq!(
            outcome,
            RoomOwnershipDrainOutcome {
                released: 1,
                preserved_live: 0,
                retained: 0,
            }
        );
        assert!(claim_store
            .current_claim(&entity)
            .await
            .expect("read drained claim")
            .is_none());
        assert!(registry
            .ask(ListPendingReclaimedRooms { limit: 8 })
            .await
            .expect("pending after drain")
            .is_empty());
        assert!(!registry
            .ask(ReservePendingReclaimedRoom {
                room_jid: jid.clone(),
            })
            .await
            .expect("terminal reservation refusal"));

        let demand = registry
            .ask(GetOrCreateRoom {
                room_jid: jid.clone(),
                waddle_id: "w-terminal".to_string(),
                channel_id: "c-terminal".to_string(),
                config: RoomConfig::default(),
            })
            .await;
        assert!(matches!(
            demand,
            Err(SendError::HandlerError(
                RoomRegistryError::OwnershipUnavailable(room)
            )) if room == jid
        ));
    }

    #[tokio::test]
    async fn terminal_drain_retains_registered_epoch_when_exact_release_fails() {
        let registry = spawn_registry().await;
        let owner = this_identity();
        let epoch = ClaimEpoch(73);
        let claim_store = Arc::new(DeadOwnerClaimStore::seeded(owner.clone(), epoch));
        claim_store.fail_next_release();
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(owner.clone()),
                durable_store: None,
                rollout_backoff: None,
            })
            .await
            .expect("wire");
        let jid = test_room_jid("terminal-reclaimed-retained");
        let fence = room_claim_fence(&jid, epoch);
        registry
            .ask(RememberPendingReclaimedRoom {
                room_jid: jid.clone(),
                claim_fence: fence.clone(),
                previous_owner: foreign_identity(),
            })
            .await
            .expect("register won claim");

        let outcome = registry
            .ask(DrainRoomOwnershipForShutdown {
                pending_handoffs: Vec::new(),
            })
            .await
            .expect("terminal drain");
        assert_eq!(
            outcome,
            RoomOwnershipDrainOutcome {
                released: 0,
                preserved_live: 0,
                retained: 1,
            }
        );
        assert_eq!(
            registry
                .ask(ListPendingReclaimedRooms { limit: 8 })
                .await
                .expect("retained pending claim"),
            vec![PendingReclaimedRoom {
                room_jid: jid,
                claim_fence: fence,
                previous_owner: foreign_identity(),
            }]
        );
    }

    #[tokio::test]
    async fn terminal_drain_preserves_a_live_room_with_the_same_reserved_claim() {
        let registry = spawn_registry().await;
        let claim_store = Arc::new(InProcessClaimStore::new());
        let owner = this_identity();
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(owner.clone()),
                durable_store: None,
                rollout_backoff: None,
            })
            .await
            .expect("wire");
        let jid = test_room_jid("terminal-live-duplicate");
        let entity = Entity::new(EntityType::RoomActor, jid.to_string());
        registry
            .ask(GetOrCreateRoom {
                room_jid: jid.clone(),
                waddle_id: "w-live".to_string(),
                channel_id: "c-live".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("publish live room");
        let snapshot = claim_store
            .current_claim(&entity)
            .await
            .expect("read live claim")
            .expect("live claim exists");
        assert!(registry
            .ask(ReservePendingReclaimedRoom {
                room_jid: jid.clone(),
            })
            .await
            .expect("reserve ambiguous steal"));

        let outcome = registry
            .ask(DrainRoomOwnershipForShutdown {
                pending_handoffs: Vec::new(),
            })
            .await
            .expect("terminal drain");
        assert_eq!(
            outcome,
            RoomOwnershipDrainOutcome {
                released: 0,
                preserved_live: 1,
                retained: 0,
            }
        );
        assert!(claim_store
            .fence(&entity, &owner, snapshot.claim_epoch)
            .await
            .expect("fence live claim"));
        assert!(registry
            .ask(GetRoom {
                room_jid: jid.clone(),
            })
            .await
            .expect("get live room")
            .is_some());
        assert!(registry
            .ask(ListPendingReclaimedRooms { limit: 8 })
            .await
            .expect("pending after live duplicate cleanup")
            .is_empty());
    }

    #[tokio::test]
    async fn pending_retry_page_rotates_past_a_persistent_full_prefix() {
        let registry = spawn_registry().await;
        let previous_owner = foreign_identity();
        let mut rooms = Vec::new();
        for index in 0..65 {
            let room_jid = test_room_jid(&format!("fair-retry-{index:02}"));
            registry
                .ask(RememberPendingReclaimedRoom {
                    room_jid: room_jid.clone(),
                    claim_fence: room_claim_fence(&room_jid, ClaimEpoch(70)),
                    previous_owner: previous_owner.clone(),
                })
                .await
                .expect("remember pending room");
            rooms.push(room_jid);
        }

        let first = registry
            .ask(ListPendingReclaimedRooms { limit: 64 })
            .await
            .expect("first page");
        assert_eq!(first.len(), 64);
        assert!(!first.iter().any(|entry| entry.room_jid == rooms[64]));

        let second = registry
            .ask(ListPendingReclaimedRooms { limit: 64 })
            .await
            .expect("rotated page");
        assert!(
            second.iter().any(|entry| entry.room_jid == rooms[64]),
            "an entry behind a permanently failing full page must receive a retry"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn late_non_cancel_safe_release_cannot_delete_a_new_live_claim() {
        let registry = spawn_registry().await;
        let claim_store = Arc::new(NonCancelSafeReleaseStore::new());
        let identity = this_identity();
        let jid = test_room_jid("late-release-before-reacquire");
        let entity = Entity::new(EntityType::RoomActor, jid.to_string());
        let epoch = claim_store
            .ensure_claimed(&entity, &identity)
            .await
            .expect("seed self-owned claim");
        let fence = RoomClaimFenceContext::new(entity.clone(), identity.clone(), epoch);
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(identity.clone()),
                durable_store: None,
                rollout_backoff: None,
            })
            .await
            .expect("wire");
        registry
            .ask(RememberOrdinaryReleaseForTest {
                room_jid: jid.clone(),
                claim_fence: fence,
            })
            .await
            .expect("remember ambiguous release");

        assert_eq!(
            registry
                .ask(RetryPendingRoomReleases { limit: 1 })
                .await
                .expect("time out first non-cancel-safe release"),
            1,
        );
        claim_store.late_release_started.notified().await;

        let acquisition = registry
            .ask(GetOrCreateRoom {
                room_jid: jid.clone(),
                waddle_id: "w".to_string(),
                channel_id: "c".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("converge old release then publish fresh actor");
        let live = claim_store
            .current_claim(&entity)
            .await
            .expect("current claim")
            .expect("fresh claim exists");
        assert!(
            live.claim_epoch > epoch,
            "demand must acquire a fresh epoch"
        );

        claim_store.allow_late_release.notify_one();
        claim_store.late_release_completed.notified().await;
        assert!(claim_store
            .fence(&entity, &identity, live.claim_epoch)
            .await
            .expect("fresh claim survives late old delete"));
        assert!(acquisition.actor_ref.is_alive());
    }

    #[tokio::test(start_paused = true)]
    async fn reclaimed_reconcile_cannot_republish_a_fence_with_a_late_release_pending() {
        let registry = spawn_registry().await;
        let claim_store = Arc::new(NonCancelSafeReleaseStore::new());
        let identity = this_identity();
        let jid = test_room_jid("reclaimed-publish-with-late-release");
        let entity = Entity::new(EntityType::RoomActor, jid.to_string());
        let epoch = claim_store
            .ensure_claimed(&entity, &identity)
            .await
            .expect("seed self-owned claim");
        let fence = RoomClaimFenceContext::new(entity.clone(), identity.clone(), epoch);
        let durable_store = Arc::new(RecordingDurableStore {
            load_result: Some(reclaimed_snapshot("must-not-republish")),
            ..RecordingDurableStore::default()
        });
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(identity),
                durable_store: Some(durable_store as Arc<dyn MucDurableStore>),
                rollout_backoff: None,
            })
            .await
            .expect("wire");
        registry
            .ask(RememberOrdinaryReleaseForTest {
                room_jid: jid.clone(),
                claim_fence: fence.clone(),
            })
            .await
            .expect("remember ambiguous release");

        assert_eq!(
            registry
                .ask(RetryPendingRoomReleases { limit: 1 })
                .await
                .expect("time out first non-cancel-safe release"),
            1,
        );
        claim_store.late_release_started.notified().await;

        assert_eq!(
            registry
                .ask(ReconcileReclaimedRoom {
                    room_jid: jid.clone(),
                    claim_fence: fence,
                    previous_owner: foreign_identity(),
                })
                .await
                .expect("reconcile while exact release is pending"),
            ReclaimedRoomOutcome::PendingRetry,
        );
        assert!(
            registry
                .ask(GetRoom {
                    room_jid: jid.clone(),
                })
                .await
                .expect("room lookup")
                .is_none(),
            "an exact fence with a non-cancel-safe delete in flight must never be republished"
        );

        claim_store.allow_late_release.notify_one();
        claim_store.late_release_completed.notified().await;
        registry
            .ask(RetryPendingRoomReleases { limit: 1 })
            .await
            .expect("converge completed late release");
        assert_eq!(
            registry
                .ask(ReconcileReclaimedRoom {
                    room_jid: jid.clone(),
                    claim_fence: room_claim_fence(&jid, epoch),
                    previous_owner: foreign_identity(),
                })
                .await
                .expect("reconcile released fence"),
            ReclaimedRoomOutcome::LostRace,
        );
    }

    #[tokio::test(start_paused = true)]
    async fn pre_acquire_convergence_uses_one_short_budget_and_releases_the_mailbox() {
        let registry = spawn_registry().await;
        let claim_store = Arc::new(NonCancelSafeReleaseStore::new());
        let identity = this_identity();
        let jid = test_room_jid("bounded-pre-acquire-convergence");
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(identity.clone()),
                durable_store: None,
                rollout_backoff: None,
            })
            .await
            .expect("wire");
        for epoch in 1..=6 {
            registry
                .ask(RememberOrdinaryReleaseForTest {
                    room_jid: jid.clone(),
                    claim_fence: RoomClaimFenceContext::new(
                        Entity::new(EntityType::RoomActor, jid.to_string()),
                        identity.clone(),
                        ClaimEpoch(epoch),
                    ),
                })
                .await
                .expect("remember ambiguous release");
        }

        let create_registry = registry.clone();
        let create_jid = jid.clone();
        let create = tokio::spawn(async move {
            create_registry
                .ask(GetOrCreateRoom {
                    room_jid: create_jid,
                    waddle_id: "w".to_string(),
                    channel_id: "c".to_string(),
                    config: RoomConfig::default(),
                })
                .await
        });
        claim_store.late_release_started.notified().await;
        let count_registry = registry.clone();
        let unrelated = tokio::spawn(async move { count_registry.ask(RoomCount).await });

        tokio::time::advance(PRE_ACQUIRE_CONVERGENCE_BUDGET).await;
        assert!(matches!(
            create.await.expect("create task"),
            Err(SendError::HandlerError(
                RoomRegistryError::OwnershipReconciliationPending(ref room)
            ))
                if *room == jid
        ));
        assert_eq!(
            unrelated
                .await
                .expect("unrelated task")
                .expect("registry count"),
            0,
            "unrelated mailbox work must resume after the short shared budget"
        );
        assert_eq!(
            claim_store.release_calls.load(Ordering::SeqCst),
            1,
            "convergence must stop on the first exact fence that stays pending"
        );
        assert_eq!(
            registry
                .ask(GetPendingRoomReleaseBacklog)
                .await
                .expect("pending backlog")
                .depth,
            6,
            "unattempted exact fences must remain for background retry"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn reclaimed_late_release_is_converged_before_demand_reacquires() {
        let registry = spawn_registry().await;
        let claim_store = Arc::new(NonCancelSafeReleaseStore::new());
        let identity = this_identity();
        let jid = test_room_jid("late-reclaimed-release-before-reacquire");
        let entity = Entity::new(EntityType::RoomActor, jid.to_string());
        let epoch = claim_store
            .ensure_claimed(&entity, &identity)
            .await
            .expect("seed reclaimed self-owned claim");
        let fence = RoomClaimFenceContext::new(entity.clone(), identity.clone(), epoch);
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(identity.clone()),
                durable_store: None,
                rollout_backoff: None,
            })
            .await
            .expect("wire");

        assert_eq!(
            registry
                .ask(ReconcileReclaimedRoom {
                    room_jid: jid.clone(),
                    claim_fence: fence,
                    previous_owner: foreign_identity(),
                })
                .await
                .expect("first reclaimed release times out"),
            ReclaimedRoomOutcome::PendingRetry,
        );
        claim_store.late_release_started.notified().await;

        let acquisition = registry
            .ask(GetOrCreateRoom {
                room_jid: jid.clone(),
                waddle_id: "w".to_string(),
                channel_id: "c".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("converge reclaimed release then publish fresh actor");
        let live = claim_store
            .current_claim(&entity)
            .await
            .expect("current claim")
            .expect("fresh claim exists");
        assert!(
            live.claim_epoch > epoch,
            "demand must acquire a fresh epoch"
        );

        claim_store.allow_late_release.notify_one();
        claim_store.late_release_completed.notified().await;
        assert!(claim_store
            .fence(&entity, &identity, live.claim_epoch)
            .await
            .expect("fresh claim survives late reclaimed delete"));
        assert!(acquisition.actor_ref.is_alive());
    }

    #[tokio::test]
    async fn bare_reclaimed_reservation_blocks_demand_claim_acquisition() {
        let registry = spawn_registry().await;
        let claim_store = Arc::new(InProcessClaimStore::new());
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(this_identity()),
                durable_store: None,
                rollout_backoff: None,
            })
            .await
            .expect("wire");
        let jid = test_room_jid("reserved-reclaimed-blocks-demand");
        assert!(registry
            .ask(ReservePendingReclaimedRoom {
                room_jid: jid.clone(),
            })
            .await
            .expect("reserve reclaimed room"));

        assert!(matches!(
            registry
                .ask(GetOrCreateRoom {
                    room_jid: jid.clone(),
                    waddle_id: "w".to_string(),
                    channel_id: "c".to_string(),
                    config: RoomConfig::default(),
                })
                .await,
            Err(SendError::HandlerError(
                RoomRegistryError::OwnershipReconciliationPending(blocked)
            ))
                if blocked == jid
        ));
        assert!(claim_store
            .current_claim(&Entity::new(EntityType::RoomActor, jid.to_string()))
            .await
            .expect("claim lookup")
            .is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn timed_out_release_is_cancelled_and_retried_without_wedging_registry() {
        let registry = spawn_registry().await;
        let epoch = ClaimEpoch(50);
        let claim_store = Arc::new(DeadOwnerClaimStore::seeded(this_identity(), epoch));
        claim_store.set_release_delay(std::time::Duration::from_secs(60));
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(this_identity()),
                durable_store: None,
                rollout_backoff: None,
            })
            .await
            .expect("wire");
        let jid = test_room_jid("release-timeout");

        let first = registry
            .ask(ReconcileReclaimedRoom {
                room_jid: jid.clone(),
                claim_fence: room_claim_fence(&jid, epoch),
                previous_owner: this_identity(),
            })
            .await
            .expect("bounded reconcile");
        assert_eq!(first, ReclaimedRoomOutcome::PendingRetry);
        assert!(registry
            .ask(ListPendingReclaimedRooms { limit: 8 })
            .await
            .expect("registry remains responsive after timeout")
            .iter()
            .any(|entry| entry.room_jid == jid && entry.claim_fence.epoch == epoch));
        assert!(claim_store
            .current_claim(&Entity::new(EntityType::RoomActor, jid.to_string()))
            .await
            .expect("claim retained")
            .is_some());

        claim_store.set_release_delay(std::time::Duration::ZERO);
        let retried = registry
            .ask(ReconcileReclaimedRoom {
                room_jid: jid.clone(),
                claim_fence: room_claim_fence(&jid, epoch),
                previous_owner: this_identity(),
            })
            .await
            .expect("release retry");
        assert_eq!(retried, ReclaimedRoomOutcome::Released);
    }

    #[tokio::test]
    async fn demand_side_creation_winning_before_reconcile_is_not_duplicated() {
        let registry = spawn_registry().await;
        let epoch = ClaimEpoch(12);
        let claim_store = Arc::new(DeadOwnerClaimStore::seeded(this_identity(), epoch));
        registry
            .ask(WireClusteringClaims {
                claim_store: claim_store as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(this_identity()),
                durable_store: Some(
                    Arc::new(RecordingDurableStore::default()) as Arc<dyn MucDurableStore>
                ),
                rollout_backoff: None,
            })
            .await
            .expect("wire");

        let jid = test_room_jid("demand-race");
        let demand = registry
            .ask(GetOrCreateRoom {
                room_jid: jid.clone(),
                waddle_id: "demand-waddle".to_string(),
                channel_id: "demand-channel".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("demand-side creation");
        let outcome = registry
            .ask(ReconcileReclaimedRoom {
                room_jid: jid.clone(),
                claim_fence: room_claim_fence(&jid, epoch),
                previous_owner: this_identity(),
            })
            .await
            .expect("reconcile after demand");
        assert_eq!(outcome, ReclaimedRoomOutcome::AlreadyLive);
        let registered = registry
            .ask(GetRoom { room_jid: jid })
            .await
            .expect("get")
            .expect("room exists");
        assert_eq!(registered.id(), demand.actor_ref.id());
        assert_eq!(registry.ask(RoomCount).await.expect("count"), 1);

        let stale = registry
            .ask(ReconcileReclaimedRoom {
                room_jid: test_room_jid("demand-race"),
                claim_fence: room_claim_fence(
                    &test_room_jid("demand-race"),
                    ClaimEpoch(epoch.0 - 1),
                ),
                previous_owner: this_identity(),
            })
            .await
            .expect("stale reconciliation message");
        assert_eq!(stale, ReclaimedRoomOutcome::LostRace);
        let still_registered = registry
            .ask(GetRoom {
                room_jid: test_room_jid("demand-race"),
            })
            .await
            .expect("get after stale adoption")
            .expect("live actor survives");
        assert_eq!(still_registered.id(), demand.actor_ref.id());
    }

    #[tokio::test]
    async fn lost_exact_reclaimed_fence_evicts_the_matching_live_actor() {
        let registry = spawn_registry().await;
        let owner = this_identity();
        let epoch = ClaimEpoch(12);
        let claim_store = Arc::new(DeadOwnerClaimStore::seeded(owner.clone(), epoch));
        let durable_store = Arc::new(RecordingDurableStore::default());
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(owner),
                durable_store: Some(Arc::clone(&durable_store) as Arc<dyn MucDurableStore>),
                rollout_backoff: None,
            })
            .await
            .expect("wire");
        let jid = test_room_jid("lost-live-reclaimed-fence");
        let live = registry
            .ask(GetOrCreateRoom {
                room_jid: jid.clone(),
                waddle_id: "w".to_string(),
                channel_id: "c".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("publish exact live actor")
            .actor_ref;
        let fence = durable_store
            .current_claim_fence(&jid)
            .expect("published fence cache");

        *claim_store.state.lock().expect("claim state") =
            Some((foreign_identity(), ClaimEpoch(epoch.0 + 1)));
        assert_eq!(
            registry
                .ask(ReconcileReclaimedRoom {
                    room_jid: jid.clone(),
                    claim_fence: fence,
                    previous_owner: foreign_identity(),
                })
                .await
                .expect("lost-race reconciliation"),
            ReclaimedRoomOutcome::LostRace,
        );
        assert!(
            registry
                .ask(GetRoom {
                    room_jid: jid.clone(),
                })
                .await
                .expect("registry lookup")
                .is_none(),
            "an unfenced live actor must not remain registered"
        );
        assert!(!live.is_alive(), "the stale ActorRef must be killed");
        assert_eq!(durable_store.current_claim_fence(&jid), None);
    }

    #[tokio::test]
    async fn in_flight_old_epoch_mutation_is_replaced_by_clean_fenced_hydration() {
        let registry = spawn_registry().await;
        let old_owner = this_identity();
        let new_owner = foreign_identity();
        let old_epoch = ClaimEpoch(20);
        let new_epoch = ClaimEpoch(21);
        let claim_store = Arc::new(DeadOwnerClaimStore::seeded(old_owner.clone(), old_epoch));
        let node_identity = SharedNodeIdentity::new(old_owner.clone());
        let config_save_started = Arc::new(tokio::sync::Notify::new());
        let allow_config_save = Arc::new(tokio::sync::Notify::new());
        let durable_store = Arc::new(RecordingDurableStore {
            load_result: Some(reclaimed_snapshot("authoritative rehydrated config")),
            block_next_config_save: AtomicBool::new(false),
            config_save_started: Some(Arc::clone(&config_save_started)),
            allow_config_save: Some(Arc::clone(&allow_config_save)),
            ..RecordingDurableStore::default()
        });
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: node_identity.clone(),
                durable_store: Some(Arc::clone(&durable_store) as Arc<dyn MucDurableStore>),
                rollout_backoff: None,
            })
            .await
            .expect("wire");
        let jid = test_room_jid("local-old-epoch");
        let original = registry
            .ask(GetOrCreateRoom {
                room_jid: jid.clone(),
                waddle_id: "local-waddle".to_string(),
                channel_id: "local-channel".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("create local actor");

        durable_store
            .block_next_config_save
            .store(true, Ordering::SeqCst);
        let original_actor = original.actor_ref.clone();
        let mutation = tokio::spawn(async move {
            original_actor
                .ask(UpdateConfig {
                    config: RoomConfig {
                        name: "old-epoch in-memory mutation".to_string(),
                        ..RoomConfig::default()
                    },
                })
                .await
        });
        config_save_started.notified().await;

        // Stand in for the dead-node reaper's exact CAS after this process
        // refreshed its node identity while the old actor still has a
        // mutation blocked in its mailbox handler.
        node_identity.rotate(new_owner.clone()).await;
        *claim_store.state.lock().expect("claim state lock") = Some((new_owner.clone(), new_epoch));
        let outcome = registry
            .ask(ReconcileReclaimedRoom {
                room_jid: jid.clone(),
                claim_fence: RoomClaimFenceContext::new(
                    Entity::new(EntityType::RoomActor, jid.to_string()),
                    new_owner,
                    new_epoch,
                ),
                previous_owner: old_owner,
            })
            .await
            .expect("replace old epoch");
        assert_eq!(outcome, ReclaimedRoomOutcome::Hydrated);
        let restored = registry
            .ask(GetRoom { room_jid: jid })
            .await
            .expect("get replacement actor")
            .expect("replacement remains registered");
        assert_ne!(
            restored.id(),
            original.actor_ref.id(),
            "an actor with old-epoch work in flight must never be adopted in place"
        );
        assert_eq!(
            restored.ask(GetConfig).await.expect("restored config").name,
            "authoritative rehydrated config",
            "only the newly fenced durable snapshot may seed replacement memory"
        );
        allow_config_save.notify_waiters();
        let mutation = mutation.await.expect("mutation task");
        assert!(
            mutation.is_err(),
            "the in-flight old-fence mutation must not report durable success"
        );
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !durable_store
                .stale_config_save_rejected
                .load(Ordering::SeqCst)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached old-fence save reached its fence check");
        assert!(
            durable_store
                .stale_config_save_rejected
                .load(Ordering::SeqCst),
            "the durable fake must observe and reject the old generation after identity rotation"
        );
        assert_eq!(
            restored.ask(GetConfig).await.expect("restored config").name,
            "authoritative rehydrated config",
            "the rejected old-fence write must not alter the replacement actor"
        );
    }

    #[tokio::test]
    async fn deposed_actor_ref_refuses_resolver_granted_join() {
        let registry = spawn_registry().await;
        let durable_store = Arc::new(RecordingDurableStore::default());
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::new(InProcessClaimStore::new()),
                node_identity: SharedNodeIdentity::new(this_identity()),
                durable_store: Some(Arc::clone(&durable_store) as Arc<dyn MucDurableStore>),
                rollout_backoff: None,
            })
            .await
            .expect("wire");
        let actor = registry
            .ask(GetOrCreateRoom {
                room_jid: test_room_jid("deposed-resolver-join"),
                waddle_id: "w".to_string(),
                channel_id: "c".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("create room")
            .actor_ref;
        durable_store.fence_lost.store(true, Ordering::SeqCst);

        let join = actor
            .ask(JoinWithAffiliation {
                sender_jid: "member@example.com/web".parse().expect("full JID"),
                nick: "member".to_string(),
                affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
                local_domain: "example.com".to_string(),
                admission_revision: 0,
            })
            .await;
        assert!(matches!(
            join,
            Err(SendError::HandlerError(
                crate::muc::room_actor::RoomActorError::RoomSealed
            ))
        ));
    }

    #[tokio::test]
    async fn deposed_actor_ref_refuses_resolver_affiliation_sync() {
        let registry = spawn_registry().await;
        let durable_store = Arc::new(RecordingDurableStore::default());
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::new(InProcessClaimStore::new()),
                node_identity: SharedNodeIdentity::new(this_identity()),
                durable_store: Some(Arc::clone(&durable_store) as Arc<dyn MucDurableStore>),
                rollout_backoff: None,
            })
            .await
            .expect("wire");
        let actor = registry
            .ask(GetOrCreateRoom {
                room_jid: test_room_jid("deposed-resolver-sync"),
                waddle_id: "w".to_string(),
                channel_id: "c".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("create room")
            .actor_ref;
        durable_store.fence_lost.store(true, Ordering::SeqCst);
        let jid: BareJid = "member@example.com".parse().expect("bare JID");

        assert_eq!(
            actor
                .ask(SyncResolverAffiliation {
                    jid: jid.clone(),
                    affiliation: Affiliation::Member,
                    expected_admission_revision: 0,
                })
                .await
                .expect("sync reply"),
            ResolverAffiliationSyncOutcome::RoomSealed,
        );
        assert_eq!(
            actor
                .ask(GetAffiliation { jid })
                .await
                .expect("affiliation query"),
            Affiliation::None,
        );
    }

    #[tokio::test]
    async fn intervening_owner_prevents_stale_local_actor_adoption() {
        let registry = spawn_registry().await;
        let owner_a = NodeIdentity::new("node-a", "incarnation-a");
        let owner_b = NodeIdentity::new("node-b", "incarnation-b");
        let owner_c = NodeIdentity::new("node-c", "incarnation-c");
        let old_epoch = ClaimEpoch(80);
        let won_epoch = ClaimEpoch(82);
        let claim_store = Arc::new(DeadOwnerClaimStore::seeded(owner_a.clone(), old_epoch));
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(owner_a),
                durable_store: None,
                rollout_backoff: None,
            })
            .await
            .expect("wire owner A");
        let jid = test_room_jid("intervening-owner");
        let stale_actor = registry
            .ask(GetOrCreateRoom {
                room_jid: jid.clone(),
                waddle_id: "owner-a".to_string(),
                channel_id: "owner-a-channel".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("spawn owner A actor")
            .actor_ref;

        let durable_store = Arc::new(RecordingDurableStore {
            load_result: Some(reclaimed_snapshot("owner B durable config")),
            ..RecordingDurableStore::default()
        });
        *claim_store.state.lock().expect("claim state") = Some((owner_c.clone(), won_epoch));
        registry
            .ask(WireClusteringClaims {
                claim_store: claim_store as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(owner_c.clone()),
                durable_store: Some(durable_store as Arc<dyn MucDurableStore>),
                rollout_backoff: None,
            })
            .await
            .expect("wire owner C");

        let outcome = registry
            .ask(ReconcileReclaimedRoom {
                room_jid: jid.clone(),
                claim_fence: RoomClaimFenceContext::new(
                    Entity::new(EntityType::RoomActor, jid.to_string()),
                    owner_c,
                    won_epoch,
                ),
                previous_owner: owner_b,
            })
            .await
            .expect("reconcile after intervening owner");
        assert_eq!(outcome, ReclaimedRoomOutcome::Hydrated);
        let restored = registry
            .ask(GetRoom { room_jid: jid })
            .await
            .expect("get restored room")
            .expect("restored actor exists");
        assert_ne!(restored.id(), stale_actor.id());
        assert_eq!(
            restored.ask(GetConfig).await.expect("restored config").name,
            "owner B durable config"
        );
    }

    #[tokio::test]
    async fn inactive_destroy_hard_kills_a_previously_deposed_actor() {
        let registry = spawn_registry().await;
        let claim_store = Arc::new(NonCancelSafeReleaseStore::new());
        let durable_store = Arc::new(RecordingDurableStore::default());
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(this_identity()),
                durable_store: Some(Arc::clone(&durable_store) as Arc<dyn MucDurableStore>),
                rollout_backoff: None,
            })
            .await
            .expect("wire clustering");

        let room_jid = test_room_jid("deposed-before-inactive-destroy");
        let actor = registry
            .ask(GetOrCreateRoom {
                room_jid: room_jid.clone(),
                waddle_id: "w-1".to_string(),
                channel_id: "c-1".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("create room")
            .actor_ref;
        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        let claim = claim_store
            .inner
            .current_claim(&entity)
            .await
            .expect("read claim")
            .expect("room is initially claimed");
        assert_eq!(
            claim_store
                .inner
                .release_exact(&entity, &claim.owner, claim.claim_epoch)
                .await
                .expect("remove claim"),
            crate::ownership::ExactReleaseOutcome::Released,
        );
        durable_store.fence_lost.store(true, Ordering::SeqCst);

        assert!(matches!(
            actor
                .ask(JoinWithAffiliation {
                    sender_jid: "alice@example.com/device".parse().expect("full JID"),
                    nick: "alice".to_string(),
                    affiliation_grant: JoinAffiliationGrant::Unaffiliated,
                    local_domain: "example.com".to_string(),
                    admission_revision: 0,
                })
                .await,
            Err(SendError::HandlerError(
                crate::muc::room_actor::RoomActorError::RoomSealed
            ))
        ));

        assert!(registry
            .ask(DestroyRoomIfInactive {
                room_jid: room_jid.clone(),
                expected_occupancy_revision: 0,
                guard: SealGuard::Dormant,
            })
            .await
            .expect("typed inactive destroy"));
        actor.wait_for_shutdown().await;
        assert!(
            !actor.is_alive(),
            "the deposed ActorRef must be hard-killed"
        );
        assert!(registry
            .ask(GetRoom {
                room_jid: room_jid.clone(),
            })
            .await
            .expect("room lookup")
            .is_none());
        assert_eq!(claim_store.release_calls.load(Ordering::SeqCst), 0);
        assert!(durable_store.current_claim_fence(&room_jid).is_none());
        assert!(durable_store.deleted_rooms.lock().expect("lock").is_empty());
    }

    /// A join fence can be the first component to prove that this actor's
    /// durable ownership moved. That seal is materially different from an
    /// inactivity seal: the deposed actor must leave the registry even when
    /// the bounded exact-release inventory is already full, while the ordinary
    /// seal must retain its capacity guard.
    #[tokio::test]
    async fn reap_sealed_room_forces_deposed_eviction_without_weakening_inactive_fence() {
        let registry = spawn_registry().await;
        let claim_store = Arc::new(NonCancelSafeReleaseStore::new());
        let durable_store = Arc::new(RecordingDurableStore::default());
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: SharedNodeIdentity::new(this_identity()),
                durable_store: Some(Arc::clone(&durable_store) as Arc<dyn MucDurableStore>),
                rollout_backoff: None,
            })
            .await
            .expect("wire clustering");

        let inactive_jid = test_room_jid("inactive-seal-full-backlog");
        let inactive_actor = registry
            .ask(GetOrCreateRoom {
                room_jid: inactive_jid.clone(),
                waddle_id: "w-1".to_string(),
                channel_id: "c-inactive".to_string(),
                config: RoomConfig {
                    persistent: false,
                    ..RoomConfig::default()
                },
            })
            .await
            .expect("create inactive target")
            .actor_ref;
        assert_eq!(
            inactive_actor
                .ask(crate::muc::room_actor::SealIfInactive {
                    expected_occupancy_revision: 0,
                    guard: crate::muc::room_actor::SealGuard::EmptyNonPersistent,
                })
                .await
                .expect("seal inactive target"),
            crate::muc::room_actor::SealIfInactiveOutcome::Inactive,
        );

        let deposed_jid = test_room_jid("deposed-seal-full-backlog");
        let deposed_actor = registry
            .ask(GetOrCreateRoom {
                room_jid: deposed_jid.clone(),
                waddle_id: "w-1".to_string(),
                channel_id: "c-deposed".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("create deposed target")
            .actor_ref;
        let deposed_entity = Entity::new(EntityType::RoomActor, deposed_jid.to_string());
        let deposed_claim = claim_store
            .inner
            .current_claim(&deposed_entity)
            .await
            .expect("read deposed claim")
            .expect("deposed room is initially claimed");
        assert_eq!(
            claim_store
                .inner
                .release_exact(
                    &deposed_entity,
                    &deposed_claim.owner,
                    deposed_claim.claim_epoch,
                )
                .await
                .expect("remove claim before the actor fence observes loss"),
            crate::ownership::ExactReleaseOutcome::Released,
        );

        for index in 0..MAX_PENDING_ROOM_RELEASES {
            let pending_jid = test_room_jid(&format!("sealed-reap-backlog-{index}"));
            assert!(registry
                .ask(RememberOrdinaryReleaseForTest {
                    room_jid: pending_jid.clone(),
                    claim_fence: room_claim_fence(&pending_jid, ClaimEpoch(index as i64)),
                })
                .await
                .expect("fill exact-release backlog"));
        }

        assert!(!registry
            .ask(ReapSealedRoom {
                room_jid: inactive_jid.clone(),
            })
            .await
            .expect("ordinary reaper remains fenced"));
        assert!(registry
            .ask(GetRoom {
                room_jid: inactive_jid,
            })
            .await
            .expect("get inactive target")
            .is_some());

        durable_store.fence_lost.store(true, Ordering::SeqCst);
        let join = deposed_actor
            .ask(JoinWithAffiliation {
                sender_jid: "alice@example.com/device".parse().expect("full JID"),
                nick: "alice".to_string(),
                affiliation_grant: JoinAffiliationGrant::Unaffiliated,
                local_domain: "example.com".to_string(),
                admission_revision: 0,
            })
            .await;
        assert!(matches!(
            join,
            Err(SendError::HandlerError(
                crate::muc::room_actor::RoomActorError::RoomSealed
            ))
        ));

        assert!(registry
            .ask(ReapSealedRoom {
                room_jid: deposed_jid.clone(),
            })
            .await
            .expect("deposed reaper bypasses backlog"));
        deposed_actor.wait_for_shutdown().await;
        assert!(!deposed_actor.is_alive());
        assert!(registry
            .ask(GetRoom {
                room_jid: deposed_jid.clone(),
            })
            .await
            .expect("get deposed target")
            .is_none());
        assert!(durable_store.deleted_rooms.lock().expect("lock").is_empty());
        assert_eq!(
            claim_store.release_calls.load(Ordering::SeqCst),
            0,
            "definitive ownership loss must not issue a redundant, potentially late release"
        );
        assert!(
            durable_store.current_claim_fence(&deposed_jid).is_none(),
            "deposed eviction forgets only the local cached fence"
        );
        assert_eq!(
            registry
                .ask(GetPendingRoomReleaseBacklog)
                .await
                .expect("bounded backlog")
                .depth,
            MAX_PENDING_ROOM_RELEASES,
            "deposed eviction must not grow a saturated release inventory"
        );
    }

    #[tokio::test]
    async fn reaping_an_identity_rotated_actor_releases_its_old_exact_claim() {
        let registry = spawn_registry().await;
        let claim_store = Arc::new(InProcessClaimStore::new());
        let durable_store = Arc::new(RecordingDurableStore::default());
        let identity = SharedNodeIdentity::new(this_identity());
        registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store) as Arc<dyn ClaimStore>,
                node_identity: identity.clone(),
                durable_store: Some(Arc::clone(&durable_store) as Arc<dyn MucDurableStore>),
                rollout_backoff: None,
            })
            .await
            .expect("wire clustering");
        let room_jid = test_room_jid("identity-rotated-sealed-reap");
        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        let actor = registry
            .ask(GetOrCreateRoom {
                room_jid: room_jid.clone(),
                waddle_id: "w".to_string(),
                channel_id: "c".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("create room")
            .actor_ref;

        identity.rotate(foreign_identity()).await;
        durable_store.fence_lost.store(true, Ordering::SeqCst);
        assert!(matches!(
            actor
                .ask(JoinWithAffiliation {
                    sender_jid: "alice@example.com/device".parse().expect("full JID"),
                    nick: "alice".to_string(),
                    affiliation_grant: JoinAffiliationGrant::Unaffiliated,
                    local_domain: "example.com".to_string(),
                    admission_revision: 0,
                })
                .await,
            Err(SendError::HandlerError(
                crate::muc::room_actor::RoomActorError::RoomSealed
            ))
        ));

        assert!(registry
            .ask(ReapSealedRoom {
                room_jid: room_jid.clone(),
            })
            .await
            .expect("reap identity-rotated actor"));
        actor.wait_for_shutdown().await;
        assert!(!actor.is_alive());
        assert!(claim_store
            .current_claim(&entity)
            .await
            .expect("old exact claim lookup")
            .is_none());
        assert!(durable_store.current_claim_fence(&room_jid).is_none());
    }
}

/// gpt-5.5 review follow-up to #1108: when the registry's seal ask
/// times out but the seal lands anyway, the sealed actor stays
/// registered and every join gets RoomSealed. ReapSealedRoom purges
/// exactly that state so the join retry respawns a fresh room.
#[tokio::test]
async fn reap_sealed_room_purges_a_sealed_but_registered_actor() {
    let registry = spawn_registry().await;
    let jid = test_room_jid("stuck");
    let acquisition: RoomAcquisition = registry
        .ask(GetOrCreateRoom {
            room_jid: jid.clone(),
            waddle_id: "w-1".to_string(),
            channel_id: "c-1".to_string(),
            config: RoomConfig {
                persistent: false,
                ..RoomConfig::default()
            },
        })
        .await
        .expect("create");
    // Seal directly, simulating a SealIfInactive that ran after the
    // registry's DestroyRoomIfInactive ask had already timed out.
    let sealed = acquisition
        .actor_ref
        .ask(crate::muc::room_actor::SealIfInactive {
            expected_occupancy_revision: 0,
            guard: crate::muc::room_actor::SealGuard::EmptyNonPersistent,
        })
        .await
        .expect("seal");
    assert_eq!(
        sealed,
        crate::muc::room_actor::SealIfInactiveOutcome::Inactive,
        "fresh empty instant room must seal"
    );

    let reaped: bool = registry
        .ask(ReapSealedRoom {
            room_jid: jid.clone(),
        })
        .await
        .expect("reap");
    assert!(reaped, "the sealed actor must be purged from the registry");

    // A subsequent get-or-create must respawn: the caller is the
    // creator again, and the new actor accepts joins.
    let fresh: RoomAcquisition = registry
        .ask(GetOrCreateRoom {
            room_jid: jid.clone(),
            waddle_id: "w-1".to_string(),
            channel_id: "c-1".to_string(),
            config: RoomConfig {
                persistent: false,
                ..RoomConfig::default()
            },
        })
        .await
        .expect("recreate");
    assert_eq!(fresh.creation, RoomCreation::Created);

    // Reaping a live (unsealed) room must refuse.
    let not_reaped: bool = registry
        .ask(ReapSealedRoom {
            room_jid: jid.clone(),
        })
        .await
        .expect("reap live");
    assert!(!not_reaped, "an unsealed room must never be reaped");
}
