use super::*;
use crate::xep::xep0421::OccupantIdSecret;
use std::time::Duration;

fn test_registry() -> RoomRegistry {
    RoomRegistry::spawn(
        "muc.example.com".to_string(),
        OccupantIdSecret::for_testing(b"test-secret".to_vec()),
    )
}

fn test_room_jid(name: &str) -> BareJid {
    format!("{name}@muc.example.com")
        .parse()
        .expect("valid test room JID")
}

#[tokio::test]
async fn fresh_registry_reports_zero_rooms_and_is_alive() {
    let registry = test_registry();
    assert!(registry.is_alive());
    let count = registry.room_count().await.expect("room_count");
    assert_eq!(count, 0);
}

#[tokio::test]
async fn idle_mailbox_depth_is_zero() {
    let registry = test_registry();
    assert_eq!(
        registry.max_capacity(),
        ROOM_REGISTRY_MAILBOX_CAPACITY as i64
    );
    // A freshly-spawned, idle registry has drained its mailbox: depth 0.
    assert_eq!(registry.mailbox_depth(), Some(0));
}

#[tokio::test]
async fn create_then_room_exists_is_true() {
    let registry = test_registry();
    let jid = test_room_jid("general");
    registry
        .create_room(
            jid.clone(),
            "w-1".to_string(),
            "c-1".to_string(),
            RoomConfig::default(),
        )
        .await
        .expect("create_room");
    assert!(registry.room_exists(jid).await.expect("room_exists"));
    assert_eq!(registry.room_count().await.expect("count"), 1);
}

#[tokio::test(start_paused = true)]
async fn wedged_request_maps_to_typed_timeout_error() {
    let registry = test_registry();

    // Drive a never-returning handler. Under paused time, advancing past the
    // reply timeout makes the wrapper's `.reply_timeout(..)` elapse, which must
    // surface as a typed `RoomRegistryError::Timeout` rather than hanging.
    let mut pending = Box::pin(registry.hang_forever());

    // Before the budget elapses, the request is still outstanding.
    assert!(
        futures::poll!(pending.as_mut()).is_pending(),
        "request must not resolve before the reply timeout elapses"
    );

    tokio::time::advance(ROOM_REGISTRY_REPLY_TIMEOUT + Duration::from_millis(1)).await;

    let result = pending.await;
    assert_eq!(result, Err(RoomRegistryError::Timeout));
}

#[tokio::test]
async fn duplicate_create_preserves_typed_handler_error() {
    let registry = test_registry();
    let jid = test_room_jid("dup");
    registry
        .create_room(
            jid.clone(),
            "w-1".to_string(),
            "c-1".to_string(),
            RoomConfig::default(),
        )
        .await
        .expect("first create");

    let err = registry
        .create_room(
            jid.clone(),
            "w-1".to_string(),
            "c-1".to_string(),
            RoomConfig::default(),
        )
        .await
        .expect_err("duplicate create must fail");

    assert_eq!(err, RoomRegistryError::RoomAlreadyExists(jid));
}
