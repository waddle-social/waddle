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

#[tokio::test]
async fn replacement_repairs_the_family_and_preserves_unrelated_relations() {
    let actor = spawn_test_actor().await;
    let object = Object::new(ObjectType::Channel, "replace-channel");
    let subject = Subject::user("alice@example.com");
    for relation in ["owner", "member", "moderator"] {
        actor
            .ask(WriteTuple {
                tuple: Tuple::new(object.clone(), Relation::new(relation), subject.clone()),
            })
            .await
            .expect("seed relation");
    }
    for replacement in [Some("admin"), None] {
        actor
            .ask(ReplaceExclusiveRelation {
                object: object.clone(),
                subject: subject.clone(),
                family: affiliation_family(),
                replacement: replacement.map(Relation::new),
            })
            .await
            .expect("replace complete family");
        let mut actual = held_relations(&actor, &object, &subject).await;
        actual.sort();
        let mut expected = vec!["moderator".to_owned()];
        expected.extend(replacement.map(str::to_owned));
        expected.sort();
        assert_eq!(actual, expected);
    }
}

#[tokio::test]
async fn failed_replacement_preserves_the_previous_affiliation() {
    let db = Arc::new(Database::in_memory("failed-replacement").await.expect("db"));
    MigrationRunner::global()
        .run(&db)
        .await
        .expect("migrations");
    let actor = PermissionActor::spawn(PermissionActor::new_for_tests(db.clone()));
    let object = Object::new(ObjectType::Channel, "replace-channel");
    let subject = Subject::user("alice@example.com");
    actor
        .ask(WriteTuple {
            tuple: Tuple::new(object.clone(), Relation::new("member"), subject.clone()),
        })
        .await
        .expect("seed affiliation");
    db.execute(
        "CREATE TRIGGER reject_affiliation BEFORE INSERT ON permission_tuples \
         WHEN NEW.relation = 'admin' BEGIN SELECT RAISE(ABORT, 'injected failure'); END",
    )
    .await
    .expect("inject replacement failure");
    assert!(actor
        .ask(ReplaceExclusiveRelation {
            object: object.clone(),
            subject: subject.clone(),
            family: affiliation_family(),
            replacement: Some(Relation::new("admin")),
        })
        .await
        .is_err());
    assert_eq!(
        held_relations(&actor, &object, &subject).await,
        vec!["member"]
    );
}

async fn concurrent_replacement_and_rollback(db: Arc<Database>) {
    let writer = PermissionActor::spawn(PermissionActor::new_for_tests(db.clone()));
    let rollback = PermissionActor::spawn(PermissionActor::new_for_tests(db));
    let object = Object::new(ObjectType::Channel, "concurrent-replacement");
    let subject = Subject::user("alice@example.com");
    for initial in [Some("member"), None, Some("outcast")]
        .into_iter()
        .cycle()
        .take(48)
    {
        writer
            .ask(ReplaceExclusiveRelation {
                object: object.clone(),
                subject: subject.clone(),
                family: affiliation_family(),
                replacement: initial.map(Relation::new),
            })
            .await
            .expect("seed initial affiliation");
        let (write_result, rollback_result) = tokio::join!(
            writer.ask(ReplaceExclusiveRelation {
                object: object.clone(),
                subject: subject.clone(),
                family: affiliation_family(),
                replacement: Some(Relation::new("admin")),
            }),
            rollback.ask(SwapExclusiveRelation {
                object: object.clone(),
                subject: subject.clone(),
                family: affiliation_family(),
                expected: (initial == Some("outcast")).then(|| Relation::new("outcast")),
                replacement: Some(Relation::new(if initial == Some("outcast") {
                    "member"
                } else {
                    "outcast"
                })),
            }),
        );
        write_result.expect("normal replacement");
        let outcome = rollback_result.expect("conditional rollback");
        if initial == Some("member") {
            assert_eq!(
                outcome,
                ExclusiveRelationSwap::Mismatch,
                "a rollback must never observe a partially replaced family"
            );
        }
        assert_eq!(
            held_relations(&writer, &object, &subject).await,
            vec!["admin"]
        );
    }
}

#[tokio::test]
async fn replacement_serializes_with_rollback_across_sqlite_actors() {
    let db = Arc::new(
        Database::in_memory("concurrent-replacement")
            .await
            .expect("db"),
    );
    MigrationRunner::global()
        .run(&db)
        .await
        .expect("migrations");
    concurrent_replacement_and_rollback(db).await;
}

#[tokio::test]
async fn replacement_serializes_with_rollback_across_postgres_actors() {
    use crate::db::{DatabaseConfig, DatabaseDriver};
    let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set");
        return;
    };
    let schema = format!("affiliation_swap_{}", uuid::Uuid::new_v4().simple());
    let admin = sqlx::PgPool::connect(&database_url)
        .await
        .expect("Postgres admin");
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .expect("isolated schema");
    let mut url = url::Url::parse(&database_url).expect("Postgres URL");
    let retained: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained)
        .append_pair("options", &format!("-c search_path={schema}"));
    let config = DatabaseConfig::new(DatabaseDriver::Postgres, url.to_string());
    let db = Arc::new(
        Database::from_config("affiliation-swap", &config)
            .await
            .expect("db"),
    );
    MigrationRunner::global()
        .run(&db)
        .await
        .expect("migrations");
    let object = Object::new(ObjectType::Channel, schema.clone());
    let subject = Subject::user("alice@example.com");
    let scope_key = format!(
        "permission_tuples|channel|{}|user|alice@example.com",
        object.id
    );
    let mut blocker = db.begin().await.expect("lock transaction");
    blocker
        .query(
            "SELECT pg_advisory_xact_lock(hashtext(?))",
            crate::db_params![scope_key.as_str()],
        )
        .await
        .expect("hold family lock");
    let writer = PermissionActor::spawn(PermissionActor::new_for_tests(db.clone()));
    let replace = async {
        writer
            .ask(ReplaceExclusiveRelation {
                object,
                subject,
                family: affiliation_family(),
                replacement: Some(Relation::new("admin")),
            })
            .await
    };
    tokio::pin!(replace);
    let waiting = async {
        loop {
            let queued: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_locks WHERE locktype = 'advisory' \
                 AND objid = (hashtext($1)::bigint & 4294967295)::oid AND NOT granted)",
            )
            .bind(&scope_key)
            .fetch_one(&admin)
            .await
            .expect("inspect lock waiter");
            if queued {
                break;
            }
            tokio::task::yield_now().await;
        }
    };
    tokio::select! {
        result = &mut replace => panic!("replacement bypassed the swap lock: {result:?}"),
        result = tokio::time::timeout(std::time::Duration::from_secs(5), waiting) => {
            result.expect("replacement must wait for the same advisory lock as rollback");
        }
    }
    blocker.commit().await.expect("release family lock");
    replace.await.expect("replacement after release");
    concurrent_replacement_and_rollback(db).await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .expect("drop schema");
}
