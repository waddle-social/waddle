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
        .expect("first get_or_create");

    let second: ActorRef<RoomActor> = registry
        .ask(GetOrCreateRoom {
            room_jid: jid,
            waddle_id: "w-1".to_string(),
            channel_id: "c-1".to_string(),
            config: RoomConfig::default(),
        })
        .await
        .expect("second get_or_create");

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
        .expect("first get_or_create");
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
        .expect("recreate room after explicit destroy");
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
        .expect("get_or_create");

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
        .expect("get_or_create");

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
        .expect("get_or_create");

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
        .expect("get_or_create");

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
