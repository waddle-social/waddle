use super::*;
use crate::db::actor::DbActor;
use crate::db::{Database, MigrationRunner};
use crate::permissions::tuple::{Relation, Tuple};
use kameo::actor::Spawn;

async fn setup_test_db() -> (kameo::actor::ActorRef<DbActor>, TupleStore) {
    let db = Database::in_memory("test-check").await.unwrap();

    let runner = MigrationRunner::global();
    runner.run(&db).await.unwrap();

    let actor = DbActor::spawn(DbActor::new(db));
    let store = TupleStore::new(actor.clone());
    (actor, store)
}

#[tokio::test]
async fn test_direct_permission_check() {
    let (actor, store) = setup_test_db().await;
    let checker = PermissionChecker::new(actor.clone(), PermissionSchema::default());

    // Create tuple: alice is owner of space:test
    let tuple = Tuple::new(
        Object::new(ObjectType::Space, "test"),
        Relation::new("owner"),
        Subject::user("user-alice"),
    );
    store.write(tuple).await.unwrap();

    // Check: alice has owner permission on space:test
    let request = CheckRequest::new(
        Subject::user("user-alice"),
        "owner",
        Object::new(ObjectType::Space, "test"),
    );
    let response = checker.check(request).await.unwrap();
    assert!(response.allowed);

    // Check: bob does NOT have owner permission
    let request = CheckRequest::new(
        Subject::user("user-bob"),
        "owner",
        Object::new(ObjectType::Space, "test"),
    );
    let response = checker.check(request).await.unwrap();
    assert!(!response.allowed);
}

#[tokio::test]
async fn test_computed_permission_union() {
    let (actor, store) = setup_test_db().await;
    let checker = PermissionChecker::new(actor.clone(), PermissionSchema::default());

    // Create tuple: alice is admin of space:test (not owner)
    let tuple = Tuple::new(
        Object::new(ObjectType::Space, "test"),
        Relation::new("admin"),
        Subject::user("user-alice"),
    );
    store.write(tuple).await.unwrap();

    // Check: alice has manage_settings permission (granted to owner OR admin)
    let request = CheckRequest::new(
        Subject::user("user-alice"),
        "manage_settings",
        Object::new(ObjectType::Space, "test"),
    );
    let response = checker.check(request).await.unwrap();
    assert!(response.allowed);
}

#[tokio::test]
async fn test_arrow_permission() {
    let (actor, store) = setup_test_db().await;
    let checker = PermissionChecker::new(actor.clone(), PermissionSchema::default());

    // Setup:
    // 1. alice is admin of space:test
    // 2. channel:general has parent space:test
    store
        .write(Tuple::new(
            Object::new(ObjectType::Space, "test"),
            Relation::new("admin"),
            Subject::user("user-alice"),
        ))
        .await
        .unwrap();

    store
        .write(Tuple::new(
            Object::new(ObjectType::Channel, "general"),
            Relation::new("parent"),
            Subject::userset(SubjectType::Space, "test", ""),
        ))
        .await
        .unwrap();

    // Check: alice can delete channel:general (requires parent->admin)
    let request = CheckRequest::new(
        Subject::user("user-alice"),
        "delete",
        Object::new(ObjectType::Channel, "general"),
    );
    let response = checker.check(request).await.unwrap();
    assert!(
        response.allowed,
        "Alice should be able to delete channel via arrow permission"
    );
}

#[tokio::test]
async fn test_inherited_permission_via_membership() {
    let (actor, store) = setup_test_db().await;
    let checker = PermissionChecker::new(actor.clone(), PermissionSchema::default());

    // Setup:
    // 1. alice is a member of space:test
    // 2. channel:general has parent space:test
    store
        .write(Tuple::new(
            Object::new(ObjectType::Space, "test"),
            Relation::new("member"),
            Subject::user("user-alice"),
        ))
        .await
        .unwrap();

    store
        .write(Tuple::new(
            Object::new(ObjectType::Channel, "general"),
            Relation::new("parent"),
            Subject::userset(SubjectType::Space, "test", ""),
        ))
        .await
        .unwrap();

    // Check: alice can view channel:general (via parent->member)
    let request = CheckRequest::new(
        Subject::user("user-alice"),
        "view",
        Object::new(ObjectType::Channel, "general"),
    );
    let response = checker.check(request).await.unwrap();
    assert!(
        response.allowed,
        "Space member should be able to view channel through inheritance"
    );
}

#[tokio::test]
async fn test_userset_permission() {
    let (db, store) = setup_test_db().await;
    let checker = PermissionChecker::new(db.clone(), PermissionSchema::default());

    // Setup:
    // 1. alice is a member of space:test
    // 2. channel:general grants viewer to space:test#member (all members)
    store
        .write(Tuple::new(
            Object::new(ObjectType::Space, "test"),
            Relation::new("member"),
            Subject::user("user-alice"),
        ))
        .await
        .unwrap();

    store
        .write(Tuple::new(
            Object::new(ObjectType::Channel, "general"),
            Relation::new("viewer"),
            Subject::userset(SubjectType::Space, "test", "member"),
        ))
        .await
        .unwrap();

    // Check: alice has viewer permission via userset
    let request = CheckRequest::new(
        Subject::user("user-alice"),
        "viewer",
        Object::new(ObjectType::Channel, "general"),
    );
    let response = checker.check(request).await.unwrap();
    assert!(
        response.allowed,
        "Alice should have viewer via userset membership"
    );
}

#[tokio::test]
async fn test_cache() {
    let (db, store) = setup_test_db().await;
    let checker = PermissionChecker::new(db.clone(), PermissionSchema::default());

    // Create tuple
    store
        .write(Tuple::new(
            Object::new(ObjectType::Space, "test"),
            Relation::new("owner"),
            Subject::user("user-alice"),
        ))
        .await
        .unwrap();

    // First check
    let request = CheckRequest::new(
        Subject::user("user-alice"),
        "owner",
        Object::new(ObjectType::Space, "test"),
    );
    let response1 = checker.check(request.clone()).await.unwrap();
    assert!(response1.allowed);

    // Second check should be cached
    let response2 = checker.check(request).await.unwrap();
    assert!(response2.allowed);
    assert_eq!(response2.reason, Some("cached".to_string()));
}

#[tokio::test]
async fn test_owner_has_delete() {
    let (db, store) = setup_test_db().await;
    let checker = PermissionChecker::new(db.clone(), PermissionSchema::default());

    // Create owner tuple
    store
        .write(Tuple::new(
            Object::new(ObjectType::Space, "test"),
            Relation::new("owner"),
            Subject::user("user-alice"),
        ))
        .await
        .unwrap();

    // Check delete permission (computed from owner relation)
    let request = CheckRequest::new(
        Subject::user("user-alice"),
        "delete",
        Object::new(ObjectType::Space, "test"),
    );
    let response = checker.check(request).await.unwrap();
    assert!(response.allowed, "Owner should have delete permission");
}
