use super::*;
use crate::muc::RoomClaimFenceContext;
use crate::xep::xep0421::OccupantIdSecret;
use std::time::Duration;

fn test_registry() -> RoomRegistry {
    RoomRegistry::spawn(
        "muc.example.com".to_string(),
        OccupantIdSecret::for_testing(b"test-secret".to_vec()),
        None,
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
async fn reclaimed_room_reservations_are_bounded() {
    let registry = test_registry();
    for index in 0..crate::muc::room_registry_actor::MAX_PENDING_RECLAIMED_ROOMS {
        assert!(registry
            .reserve_pending_reclaimed_room(test_room_jid(&format!("pending-{index}")))
            .await
            .expect("reservation ask"));
    }
    assert!(!registry
        .reserve_pending_reclaimed_room(test_room_jid("overflow"))
        .await
        .expect("overflow reservation ask"));
    let backlog = registry
        .pending_reclaimed_room_backlog()
        .await
        .expect("backlog");
    assert_eq!(
        backlog.depth,
        crate::muc::room_registry_actor::MAX_PENDING_RECLAIMED_ROOMS
    );
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

    // Pending-reclaim registration is mailbox-acknowledged, not reply-
    // acknowledged. Even though the actor remains wedged in the handler
    // above, successful enqueue is definitive and cannot be mistaken for an
    // uncertain commit followed by concurrent exact release.
    let queued_room = test_room_jid("queued-reclaim");
    let accepted = registry
        .remember_pending_reclaimed_room(
            queued_room.clone(),
            RoomClaimFenceContext::new(
                crate::ownership::Entity::new(
                    crate::ownership::EntityType::RoomActor,
                    queued_room.to_string(),
                ),
                crate::ownership::NodeIdentity::new("current", "incarnation"),
                crate::ownership::ClaimEpoch(7),
            ),
            crate::ownership::NodeIdentity::new("dead", "incarnation"),
        )
        .await;
    assert_eq!(accepted, Ok(()));
}

#[tokio::test(start_paused = true)]
async fn shutdown_room_ownership_drain_uses_the_supplied_terminal_timeout() {
    let registry = test_registry();

    let mut wedged = Box::pin(registry.hang_forever());
    assert!(
        futures::poll!(wedged.as_mut()).is_pending(),
        "request must not resolve before the reply timeout elapses"
    );

    let mut drain = Box::pin(
        registry
            .drain_room_ownership_for_shutdown_with_timeout(Vec::new(), Duration::from_secs(30)),
    );
    assert!(
        futures::poll!(drain.as_mut()).is_pending(),
        "shutdown drain should wait behind the wedged request"
    );

    tokio::time::advance(ROOM_REGISTRY_REPLY_TIMEOUT + Duration::from_millis(1)).await;
    assert!(
        futures::poll!(drain.as_mut()).is_pending(),
        "shutdown drain must outlive the normal reply timeout"
    );

    tokio::time::advance(Duration::from_secs(25)).await;
    let result = drain.await;
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
