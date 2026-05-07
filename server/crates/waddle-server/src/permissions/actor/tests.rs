use std::sync::Arc;

use kameo::actor::ActorRef;

use super::*;
use crate::config::ServerConfig;
use crate::db::{Database, MigrationRunner};

async fn spawn_test_actor() -> ActorRef<PermissionActor> {
    let db = Database::in_memory("test-permission-actor")
        .await
        .expect("db");
    let db = Arc::new(db);

    let runner = MigrationRunner::global();
    runner.run(&db).await.expect("migrations");

    kameo::spawn(PermissionActor::new_for_tests(db))
}

#[tokio::test]
async fn write_and_check_permission_with_typed_payloads() {
    let actor = spawn_test_actor().await;
    let tuple = Tuple::new(
        Object::new(ObjectType::Space, "test-space"),
        Relation::new("owner"),
        Subject::user("user-alice"),
    );

    actor
        .ask(WriteTuple { tuple })
        .await
        .expect("write should succeed");

    let response = actor
        .ask(CheckPermission {
            subject: Subject::user("user-alice"),
            permission: Permission::Delete,
            object: Object::new(ObjectType::Space, "test-space"),
        })
        .await
        .expect("check should succeed");

    assert!(response.allowed);
}

#[tokio::test]
async fn write_and_delete_tuple_with_typed_payloads() {
    let actor = spawn_test_actor().await;
    let tuple = Tuple::new(
        Object::new(ObjectType::Space, "test-space"),
        Relation::new("member"),
        Subject::user("user-alice"),
    );

    actor
        .ask(WriteTuple {
            tuple: tuple.clone(),
        })
        .await
        .expect("write should succeed");

    actor
        .ask(DeleteTuple {
            tuple: tuple.clone(),
        })
        .await
        .expect("delete should succeed");

    let response = actor
        .ask(CheckPermission {
            subject: Subject::user("user-alice"),
            permission: Permission::Member,
            object: Object::new(ObjectType::Space, "test-space"),
        })
        .await
        .expect("check should succeed");

    assert!(!response.allowed);
}

#[tokio::test]
async fn list_relations_returns_typed_relations() {
    let actor = spawn_test_actor().await;

    actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(ObjectType::Space, "test-space"),
                Relation::new("owner"),
                Subject::user("user-alice"),
            ),
        })
        .await
        .expect("write owner");

    actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(ObjectType::Space, "test-space"),
                Relation::new("admin"),
                Subject::user("user-alice"),
            ),
        })
        .await
        .expect("write admin");

    let relations = actor
        .ask(ListRelations {
            subject: Subject::user("user-alice"),
            object: Object::new(ObjectType::Space, "test-space"),
        })
        .await
        .expect("list relations should succeed");

    assert_eq!(relations.len(), 2);
    assert!(relations.contains(&Relation::new("owner")));
    assert!(relations.contains(&Relation::new("admin")));
}

#[tokio::test]
async fn list_subjects_returns_typed_subjects() {
    let actor = spawn_test_actor().await;

    actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(ObjectType::Space, "test-space"),
                Relation::new("member"),
                Subject::user("user-alice"),
            ),
        })
        .await
        .expect("write alice");

    actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(ObjectType::Space, "test-space"),
                Relation::new("member"),
                Subject::user("user-bob"),
            ),
        })
        .await
        .expect("write bob");

    let subjects = actor
        .ask(ListSubjects {
            object: Object::new(ObjectType::Space, "test-space"),
            relation: Relation::new("member"),
        })
        .await
        .expect("list subjects should succeed");

    assert_eq!(subjects.len(), 2);
    assert!(subjects.contains(&Subject::user("user-alice")));
    assert!(subjects.contains(&Subject::user("user-bob")));
}

#[tokio::test]
async fn duplicate_tuple_returns_typed_error() {
    let actor = spawn_test_actor().await;
    let tuple = Tuple::new(
        Object::new(ObjectType::Space, "test-space"),
        Relation::new("owner"),
        Subject::user("user-alice"),
    );

    actor
        .ask(WriteTuple {
            tuple: tuple.clone(),
        })
        .await
        .expect("first write should succeed");

    let result = actor.ask(WriteTuple { tuple }).await;
    assert!(result.is_err(), "duplicate tuple should return an error");
}

#[tokio::test]
async fn from_server_config_requires_spicedb_config() {
    let config = ServerConfig::default();
    let result = PermissionActor::from_server_config(&config).await;

    assert!(matches!(result, Err(PermissionError::SpiceDbConfigMissing)));
}
