use std::sync::Arc;

use kameo::actor::{ActorRef, Spawn};

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

    PermissionActor::spawn(PermissionActor::new_for_tests(db))
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

fn affiliation_family() -> Vec<Relation> {
    ["owner", "admin", "member", "outcast"]
        .into_iter()
        .map(Relation::new)
        .collect()
}

async fn held_relations(
    actor: &ActorRef<PermissionActor>,
    object: &Object,
    subject: &Subject,
) -> Vec<String> {
    actor
        .ask(ListRelations {
            subject: subject.clone(),
            object: object.clone(),
        })
        .await
        .expect("list relations")
        .into_iter()
        .map(|relation| relation.name)
        .collect()
}

#[tokio::test]
async fn swap_exclusive_relation_writes_only_while_the_family_holds_expected() {
    let actor = spawn_test_actor().await;
    let object = Object::new(ObjectType::Channel, "swap-channel");
    let subject = Subject::user("alice@example.com");
    let swap = |expected: Option<&str>, replacement: Option<&str>| SwapExclusiveRelation {
        object: object.clone(),
        subject: subject.clone(),
        family: affiliation_family(),
        expected: expected.map(Relation::new),
        replacement: replacement.map(Relation::new),
    };

    assert_eq!(
        actor.ask(swap(None, Some("member"))).await.expect("swap"),
        ExclusiveRelationSwap::Swapped
    );
    assert_eq!(
        held_relations(&actor, &object, &subject).await,
        vec!["member"]
    );

    assert_eq!(
        actor
            .ask(swap(Some("outcast"), Some("admin")))
            .await
            .expect("swap"),
        ExclusiveRelationSwap::Mismatch,
        "a family that does not hold `expected` is left alone"
    );
    assert_eq!(
        held_relations(&actor, &object, &subject).await,
        vec!["member"]
    );

    assert_eq!(
        actor.ask(swap(None, Some("admin"))).await.expect("swap"),
        ExclusiveRelationSwap::Mismatch,
        "`expected: None` requires the family to be empty"
    );

    assert_eq!(
        actor
            .ask(swap(Some("member"), Some("admin")))
            .await
            .expect("swap"),
        ExclusiveRelationSwap::Swapped
    );
    assert_eq!(
        held_relations(&actor, &object, &subject).await,
        vec!["admin"]
    );

    assert_eq!(
        actor.ask(swap(Some("admin"), None)).await.expect("swap"),
        ExclusiveRelationSwap::Swapped
    );
    assert!(held_relations(&actor, &object, &subject).await.is_empty());
}
