use super::*;
use kameo::actor::Spawn;
use kameo::error::SendError;

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

    let removed: bool = registry
        .ask(DestroyRoom {
            room_jid: jid.clone(),
        })
        .await
        .expect("destroy");
    assert!(removed);

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

    let removed: bool = registry
        .ask(DestroyRoom { room_jid: jid })
        .await
        .expect("destroy");
    assert!(!removed);
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
        })
        .await
        .expect("destroy poisoned room");
    assert!(destroyed);

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
    use crate::muc::durable::{DurableRoomState, MucDurableFuture, MucDurableStore};
    use crate::muc::room_actor::{GetAffiliation, GetConfig};
    use crate::muc::subject::SubjectState;
    use crate::ownership::{
        ClaimEpoch, ClaimError, ClaimSnapshot, ClaimStore, Entity, EntityType, InProcessClaimStore,
        NodeIdentity, ResumeIdentityProof, SharedNodeIdentity, StalePredicate,
    };
    use crate::types::Affiliation;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
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

        let actor_ref = registry
            .ask(GetOrCreateRoom {
                room_jid: jid.clone(),
                waddle_id: "w-1".to_string(),
                channel_id: "c-1".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("get_or_create_room")
            .actor_ref;
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
    }

    impl DeadOwnerClaimStore {
        fn seeded(owner: NodeIdentity, epoch: ClaimEpoch) -> Self {
            Self {
                state: Mutex::new(Some((owner, epoch))),
                steal_calls: AtomicUsize::new(0),
            }
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
            let existing = self.state.lock().expect("lock").clone();
            match existing {
                None => self.acquire(entity, me).await,
                Some((owner, epoch)) if owner == *me => Ok(epoch),
                Some(_) => Err(ClaimError::AlreadyClaimed),
            }
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
                Some((_, epoch)) if *epoch == observed => {
                    let new_epoch = ClaimEpoch(epoch.0 + 1);
                    *state = Some((me.clone(), new_epoch));
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

        async fn current_claim(
            &self,
            _entity: &Entity,
        ) -> Result<Option<ClaimSnapshot>, ClaimError> {
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
            let mut state = self.state.lock().expect("lock");
            if matches!(&*state, Some((owner, epoch)) if owner == me && *epoch == mine) {
                *state = None;
            }
            Ok(())
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

    /// A [`MucDurableStore`] fake recording `notify_previous_owner_demoted`
    /// calls and returning a fixed `load_room_state` result.
    #[derive(Default)]
    struct RecordingDurableStore {
        load_result: Option<DurableRoomState>,
        demote_notifications: Mutex<Vec<(String, String)>>,
    }

    impl MucDurableStore for RecordingDurableStore {
        fn load_room_state<'a>(
            &'a self,
            _room_jid: &'a BareJid,
        ) -> MucDurableFuture<'a, Option<DurableRoomState>> {
            let result = self.load_result.clone();
            Box::pin(async move { Ok(result) })
        }

        fn save_config<'a>(
            &'a self,
            _room_jid: &'a BareJid,
            _waddle_id: &'a str,
            _channel_id: &'a str,
            _config: &'a RoomConfig,
        ) -> MucDurableFuture<'a, ()> {
            Box::pin(async { Ok(()) })
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
            demote_notifications: Mutex::new(Vec::new()),
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
        let actor_ref = registry
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
            .expect("get_or_create_room")
            .actor_ref;

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
    assert!(sealed, "fresh empty instant room must seal");

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
