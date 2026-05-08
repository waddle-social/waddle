use super::*;
use kameo::error::SendError;

fn test_room_jid(name: &str) -> BareJid {
    format!("{}@muc.example.com", name)
        .parse()
        .expect("valid test JID")
}

async fn spawn_registry() -> ActorRef<RoomRegistryActor> {
    kameo::spawn(RoomRegistryActor::new(
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
