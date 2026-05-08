use std::sync::Arc;

use uuid::Uuid;

use super::*;
use crate::db::Database;

#[test]
fn test_object_parse() {
    let obj = Object::parse("space:penguin-club").unwrap();
    assert_eq!(obj.object_type, ObjectType::Space);
    assert_eq!(obj.id, "penguin-club");

    let obj = Object::parse("channel:general").unwrap();
    assert_eq!(obj.object_type, ObjectType::Channel);
    assert_eq!(obj.id, "general");
}

#[test]
fn test_object_display() {
    let obj = Object::new(ObjectType::Space, "penguin-club");
    assert_eq!(obj.to_string(), "space:penguin-club");
}

#[test]
fn test_subject_parse_direct() {
    let subj = Subject::parse("user:user-alice").unwrap();
    assert_eq!(subj.subject_type, SubjectType::User);
    assert_eq!(subj.id, "user-alice");
    assert_eq!(subj.relation, None);
}

#[test]
fn test_subject_parse_userset() {
    let subj = Subject::parse("space:penguin-club#member").unwrap();
    assert_eq!(subj.subject_type, SubjectType::Space);
    assert_eq!(subj.id, "penguin-club");
    assert_eq!(subj.relation, Some("member".to_string()));
}

#[test]
fn test_subject_display() {
    let subj = Subject::user("user-alice");
    assert_eq!(subj.to_string(), "user:user-alice");

    let subj = Subject::userset(SubjectType::Space, "penguin-club", "member");
    assert_eq!(subj.to_string(), "space:penguin-club#member");

    let subj = Subject::userset(SubjectType::Space, "penguin-club", "");
    assert_eq!(subj.to_string(), "space:penguin-club");
    assert_eq!(subj.relation, None);
}

#[test]
fn test_tuple_parse() {
    let tuple = Tuple::parse("space:penguin-club#owner@user:user-alice").unwrap();
    assert_eq!(tuple.object.object_type, ObjectType::Space);
    assert_eq!(tuple.object.id, "penguin-club");
    assert_eq!(tuple.relation.name, "owner");
    assert_eq!(tuple.subject.subject_type, SubjectType::User);
    assert_eq!(tuple.subject.id, "user-alice");
}

#[test]
fn test_tuple_display() {
    let tuple = Tuple::new(
        Object::new(ObjectType::Space, "penguin-club"),
        Relation::new("owner"),
        Subject::user("user-alice"),
    );
    assert_eq!(
        tuple.to_string(),
        "space:penguin-club#owner@user:user-alice"
    );
}

#[tokio::test]
async fn test_tuple_store_write_and_exists() {
    let db = Database::in_memory("test-tuple-store").await.unwrap();
    let db = Arc::new(db);

    let runner = crate::db::MigrationRunner::global();
    runner.run(&db).await.unwrap();

    let actor = kameo::spawn(crate::db::actor::DbActor::new((*db).clone()));
    let store = TupleStore::new(actor);

    let object = Object::new(ObjectType::Space, "test-space");
    let subject = Subject::user("user-alice");

    assert!(!store.exists(&object, "owner", &subject).await.unwrap());

    let tuple = Tuple::new(object.clone(), Relation::new("owner"), subject.clone());
    store.write(tuple).await.unwrap();

    assert!(store.exists(&object, "owner", &subject).await.unwrap());
}

#[tokio::test]
async fn test_tuple_store_delete() {
    let db = Database::in_memory("test-tuple-store-delete")
        .await
        .unwrap();
    let db = Arc::new(db);

    let runner = crate::db::MigrationRunner::global();
    runner.run(&db).await.unwrap();

    let actor = kameo::spawn(crate::db::actor::DbActor::new((*db).clone()));
    let store = TupleStore::new(actor);

    let object = Object::new(ObjectType::Space, "test-space");
    let subject = Subject::user("user-alice");

    let tuple = Tuple::new(object.clone(), Relation::new("owner"), subject.clone());
    store.write(tuple.clone()).await.unwrap();

    assert!(store.exists(&object, "owner", &subject).await.unwrap());

    store.delete(&tuple).await.unwrap();

    assert!(!store.exists(&object, "owner", &subject).await.unwrap());
}

#[tokio::test]
async fn test_tuple_store_list_subjects() {
    let db = Database::in_memory("test-tuple-store-list").await.unwrap();
    let db = Arc::new(db);

    let runner = crate::db::MigrationRunner::global();
    runner.run(&db).await.unwrap();

    let actor = kameo::spawn(crate::db::actor::DbActor::new((*db).clone()));
    let store = TupleStore::new(actor);

    let object = Object::new(ObjectType::Space, "test-space");

    store
        .write(Tuple::new(
            object.clone(),
            Relation::new("member"),
            Subject::user("user-alice"),
        ))
        .await
        .unwrap();
    store
        .write(Tuple::new(
            object.clone(),
            Relation::new("member"),
            Subject::user("user-bob"),
        ))
        .await
        .unwrap();

    let subjects = store.list_subjects(&object, "member").await.unwrap();
    assert_eq!(subjects.len(), 2);

    let ids: Vec<_> = subjects.iter().map(|s| s.id.as_str()).collect();
    assert!(ids.contains(&"user-alice"));
    assert!(ids.contains(&"user-bob"));
}

#[tokio::test(flavor = "multi_thread")]
async fn test_tuple_store_write_and_exists_file_backed() {
    let base_dir = std::env::temp_dir().join(format!("space-tuple-store-{}", Uuid::new_v4()));
    let db_path = base_dir.join("permissions.db");

    let db = Database::open_local("test-tuple-store-file", &db_path)
        .await
        .unwrap();
    let db = Arc::new(db);

    let runner = crate::db::MigrationRunner::global();
    runner.run(&db).await.unwrap();

    let actor = kameo::spawn(crate::db::actor::DbActor::new((*db).clone()));
    let store = TupleStore::new(actor);
    let object = Object::new(ObjectType::Space, "test-space");
    let subject = Subject::user("user-alice");
    let tuple = Tuple::new(object.clone(), Relation::new("owner"), subject.clone());

    store.write(tuple).await.unwrap();
    assert!(store.exists(&object, "owner", &subject).await.unwrap());

    drop(store);
    drop(db);
    std::fs::remove_dir_all(base_dir).unwrap();
}
