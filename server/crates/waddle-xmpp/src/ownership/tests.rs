use super::{
    ClaimEpoch, ClaimError, ClaimStore, Duration, Entity, EntityType, InProcessClaimStore,
    NodeIdentity, SharedNodeIdentity, ENTITY_ID_MAX_LEN,
};

#[test]
fn entity_type_db_str_round_trips() {
    for variant in [
        EntityType::UserActor,
        EntityType::RoomActor,
        EntityType::SmSession,
    ] {
        assert_eq!(EntityType::from_db_str(variant.as_db_str()), Some(variant));
    }
}

#[test]
fn entity_type_from_db_str_rejects_unknown_values() {
    assert_eq!(EntityType::from_db_str("not-a-real-entity-type"), None);
}

/// ADR-0017 Phase 3 Slice 7 FIX 9 (council-adjudicated): the wire
/// -deserialize bound on `Entity::id`, mirroring
/// `SmSessionId`'s identical boundary tests one type over.
#[test]
fn entity_deserialize_accepts_the_boundary_length() {
    let id = "a".repeat(ENTITY_ID_MAX_LEN);
    let json = serde_json::json!({ "entity_type": "RoomActor", "id": id }).to_string();
    let entity: Entity = serde_json::from_str(&json).expect("exactly at the cap must deserialize");
    assert_eq!(entity.id.len(), ENTITY_ID_MAX_LEN);
}

#[test]
fn entity_deserialize_rejects_one_byte_over_the_cap() {
    let id = "a".repeat(ENTITY_ID_MAX_LEN + 1);
    let json = serde_json::json!({ "entity_type": "RoomActor", "id": id }).to_string();
    let error =
        serde_json::from_str::<Entity>(&json).expect_err("one byte over the cap must be rejected");
    assert!(
        error.to_string().contains("exceeds"),
        "unexpected error message: {error}"
    );
}

#[test]
fn entity_deserialize_rejects_a_malicious_multi_kb_id() {
    let id = "x".repeat(64 * 1024);
    let json = serde_json::json!({ "entity_type": "RoomActor", "id": id }).to_string();
    assert!(serde_json::from_str::<Entity>(&json).is_err());
}

#[tokio::test]
async fn rotation_notifications_follow_publication_gate_and_coalesce() {
    let initial = NodeIdentity::new("initial", "first");
    let shared = SharedNodeIdentity::new(initial.clone());
    let mut changes = shared.subscribe_rotations();
    let guard = shared
        .guard_if_current(&initial)
        .await
        .expect("current guard");
    let next = NodeIdentity::new("next", "second");
    let rotation = shared.rotate(next.clone());
    tokio::pin!(rotation);
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut rotation)
            .await
            .is_err()
    );
    assert!(!changes.has_changed().expect("open watch"));
    assert_eq!(shared.current(), initial);
    drop(guard);
    rotation.await;
    let latest = NodeIdentity::new("latest", "third");
    shared.rotate(latest.clone()).await;
    changes
        .changed()
        .await
        .expect("coalesced rotation notification");
    assert_eq!(shared.current(), latest);
    assert!(!changes.has_changed().expect("notification consumed"));
}

#[test]
fn node_identity_local_is_stable() {
    assert_eq!(NodeIdentity::local(), NodeIdentity::local());
}

#[test]
fn claim_epoch_orders_by_value() {
    assert!(ClaimEpoch(0) < ClaimEpoch(1));
}

#[cfg(unix)]
#[test]
fn poisoned_node_identity_reads_abort_the_process() {
    use std::os::unix::process::ExitStatusExt;

    const CHILD_ENV: &str = "WADDLE_POISONED_IDENTITY_CHILD";
    if let Some(operation) = std::env::var_os(CHILD_ENV) {
        let shared = SharedNodeIdentity::new(NodeIdentity::new("node-a", "epoch-0"));
        let writer = shared.clone();
        std::thread::spawn(move || {
            let mut identity = writer.identity.write().expect("unpoisoned identity");
            identity.node_id = "partially-rotated-node".to_string();
            panic!("writer failed before updating its incarnation");
        })
        .join()
        .expect_err("writer panicked while holding the identity lock");

        match operation.to_str().expect("test operation") {
            "current" => {
                let _ = shared.current();
            }
            "with_current" => shared.with_current(|_| ()),
            "guard_if_current" => {
                let _ = tokio::runtime::Builder::new_current_thread()
                    .build()
                    .expect("test runtime")
                    .block_on(shared.guard_if_current(&NodeIdentity::new("node-a", "epoch-0")));
            }
            _ => panic!("unknown test operation"),
        }
        return;
    }

    for operation in ["current", "with_current", "guard_if_current"] {
        let output =
            std::process::Command::new(std::env::current_exe().expect("current test binary"))
                .args([
                    "--exact",
                    "ownership::tests::poisoned_node_identity_reads_abort_the_process",
                    "--nocapture",
                ])
                .env(CHILD_ENV, operation)
                .output()
                .expect("isolated poisoned identity process");
        assert_eq!(
                output.status.signal(),
                Some(6), // SIGABRT on the supported Unix server platforms.
                "{operation} must abort instead of returning recovered authority or merely unwinding: {}\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr),
            );
    }
}

#[tokio::test]
async fn shared_node_identity_rotation_is_visible_through_every_clone() {
    let shared = SharedNodeIdentity::new(NodeIdentity::new("node-a", "epoch-0"));
    let clone = shared.clone();
    assert_eq!(clone.current(), NodeIdentity::new("node-a", "epoch-0"));

    shared.rotate(NodeIdentity::new("node-a", "epoch-1")).await;

    assert_eq!(clone.current(), NodeIdentity::new("node-a", "epoch-1"));
    assert_eq!(shared.current(), NodeIdentity::new("node-a", "epoch-1"));
}

#[tokio::test]
async fn terminally_disabled_identity_rejects_guards_and_claim_acquisition() {
    let active = NodeIdentity::new("node-a", "epoch-0");
    let shared = SharedNodeIdentity::new(active.clone());
    shared.disable().await;

    let disabled = shared.current();
    assert!(!disabled.is_active());
    assert_ne!(disabled, active);
    assert!(disabled.same_incarnation(&active));
    assert!(!disabled.same_incarnation(&NodeIdentity::new("node-a", "epoch-1")));
    assert!(shared.guard_if_current(&disabled).await.is_none());

    let store = InProcessClaimStore::new();
    let entity = Entity::new(EntityType::SmSession, "disabled-owner");
    assert!(matches!(
        store.acquire(&entity, &disabled).await,
        Err(ClaimError::AuthorityDisabled)
    ));
}

#[tokio::test]
async fn guarded_identity_boundary_delays_rotation_until_use_completes() {
    let old = NodeIdentity::new("node-a", "epoch-0");
    let new = NodeIdentity::new("node-a", "epoch-1");
    let shared = SharedNodeIdentity::new(old.clone());
    let guard = shared
        .guard_if_current(&old)
        .await
        .expect("old identity starts current");
    let rotating = shared.clone();
    let rotation = tokio::spawn(async move {
        rotating.rotate(new).await;
    });

    tokio::task::yield_now().await;
    assert!(
        !rotation.is_finished(),
        "rotation must wait at the exact post-check/use boundary"
    );
    assert_eq!(guard.identity(), &old);
    drop(guard);

    rotation.await.expect("rotation task");
    assert_eq!(shared.current(), NodeIdentity::new("node-a", "epoch-1"));
    assert!(shared.guard_if_current(&old).await.is_none());
}
