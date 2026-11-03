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
fn identity_stamps_bind_to_the_exact_shared_source() {
    let initial = NodeIdentity::new("node-a", "epoch-a");
    let source = SharedNodeIdentity::new(initial.clone());
    let shared_clone = source.clone();
    let other_source = SharedNodeIdentity::new(initial.clone());
    let stamp = source.current_stamp().expect("active identity stamp");
    let clone_stamp = shared_clone.current_stamp().expect("shared identity stamp");
    let other_stamp = other_source.current_stamp().expect("other identity stamp");

    assert_eq!(stamp.identity(), &initial);
    assert_eq!(other_stamp.identity(), &initial);
    assert_eq!(
        shared_clone.with_current_stamp(&stamp, Clone::clone),
        Some(initial.clone())
    );
    assert_eq!(
        source.with_current_stamp(&clone_stamp, Clone::clone),
        Some(initial)
    );
    assert!(other_source.with_current_stamp(&stamp, |_| ()).is_none());
    assert!(source.with_current_stamp(&other_stamp, |_| ()).is_none());
}

#[tokio::test]
async fn identity_stamp_revisions_never_revive_after_aba_rotation() {
    let first = NodeIdentity::new("node-a", "epoch-a");
    let second = NodeIdentity::new("node-b", "epoch-b");
    let source = SharedNodeIdentity::new(first.clone());
    let original = source.current_stamp().expect("initial stamp");

    source.rotate(second.clone()).await;
    let middle = source.current_stamp().expect("second stamp");
    assert!(original.is_revoked());
    assert!(source.with_current_stamp(&original, |_| ()).is_none());
    assert_eq!(middle.identity(), &second);

    source.rotate(first.clone()).await;
    let latest = source.current_stamp().expect("latest stamp");
    assert!(original.is_revoked());
    assert!(middle.is_revoked());
    assert!(source.with_current_stamp(&original, |_| ()).is_none());
    assert!(source.with_current_stamp(&middle, |_| ()).is_none());
    assert_eq!(
        source.with_current_stamp(&latest, Clone::clone),
        Some(first)
    );
    tokio::time::timeout(Duration::from_secs(1), original.revoked())
        .await
        .expect("old revocation remains observable");
}

#[tokio::test]
async fn identity_stamp_disable_wakes_waiters_and_rejects_effects() {
    let source = SharedNodeIdentity::new(NodeIdentity::new("node-a", "epoch-a"));
    let stamp = source.current_stamp().expect("initial stamp");
    let revoked = stamp.revoked();
    tokio::pin!(revoked);
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut revoked)
            .await
            .is_err()
    );

    source.disable().await;
    tokio::time::timeout(Duration::from_secs(1), &mut revoked)
        .await
        .expect("disable wakes a pending revocation waiter");
    assert!(stamp.is_revoked());
    assert!(source.current_stamp().is_none());
    assert!(source.with_current_stamp(&stamp, |_| ()).is_none());
    tokio::time::timeout(Duration::from_secs(1), stamp.revoked())
        .await
        .expect("revocation also persists for later waiters");
}

#[tokio::test]
async fn same_active_identity_rotation_keeps_stamp_and_notifies() {
    let initial = NodeIdentity::new("node-a", "epoch-a");
    let source = SharedNodeIdentity::new(initial.clone());
    let stamp = source.current_stamp().expect("initial stamp");
    let mut changes = source.subscribe_rotations();

    source.rotate(initial.clone()).await;
    tokio::time::timeout(Duration::from_secs(1), changes.changed())
        .await
        .expect("same identity rotation still notifies")
        .expect("rotation sender remains open");
    assert!(!stamp.is_revoked());
    assert_eq!(
        source.with_current_stamp(&stamp, Clone::clone),
        Some(initial)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn identity_stamp_effect_and_rotation_are_serialized() {
    let initial = NodeIdentity::new("node-a", "epoch-a");
    let next = NodeIdentity::new("node-b", "epoch-b");
    let source = SharedNodeIdentity::new(initial.clone());
    let stamp = source.current_stamp().expect("initial stamp");
    let reader_source = source.clone();
    let reader_stamp = stamp.clone();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        reader_source.with_current_stamp(&reader_stamp, |identity| {
            assert!(!reader_stamp.is_revoked());
            entered_tx.send(()).expect("effect entry observed");
            release_rx.recv().expect("effect released");
            assert!(!reader_stamp.is_revoked());
            identity.clone()
        })
    });
    entered_rx.await.expect("effect entered");

    let rotating = source.clone();
    let rotation = tokio::spawn(async move { rotating.rotate(next).await });
    tokio::time::timeout(Duration::from_secs(1), async {
        while source.rotation_gate.try_read().is_ok() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("rotation acquired the publication gate");
    assert!(!rotation.is_finished());
    assert!(!stamp.is_revoked());
    release_tx.send(()).expect("release effect");
    assert_eq!(reader.join().expect("effect thread"), Some(initial));
    rotation.await.expect("rotation completes");
    assert!(stamp.is_revoked());
    assert!(source.with_current_stamp(&stamp, |_| ()).is_none());
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
        let stamp = shared.current_stamp().expect("active identity stamp");
        let writer = shared.clone();
        std::thread::spawn(move || {
            let mut identity = writer.identity.write().expect("unpoisoned identity");
            identity.identity.node_id = "partially-rotated-node".to_string();
            panic!("writer failed before updating its incarnation");
        })
        .join()
        .expect_err("writer panicked while holding the identity lock");

        match operation.to_str().expect("test operation") {
            "current" => {
                let _ = shared.current();
            }
            "with_current" => shared.with_current(|_| ()),
            "current_stamp" => {
                let _ = shared.current_stamp();
            }
            "with_current_stamp" => {
                let _ = shared.with_current_stamp(&stamp, |_| ());
            }
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

    for operation in [
        "current",
        "with_current",
        "current_stamp",
        "with_current_stamp",
        "guard_if_current",
    ] {
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
