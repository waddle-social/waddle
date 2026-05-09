use dashmap::DashMap;
use jid::BareJid;
use std::sync::Arc;

use super::*;
use crate::db::Database;

async fn setup_test_db() -> Database {
    let db = Database::in_memory("test-roster").await.unwrap();
    // Run migrations
    let runner = crate::db::MigrationRunner::global();
    runner.run(&db).await.unwrap();
    db
}

fn make_row(contact_jid: &str, subscription: &str) -> RosterItemRow {
    RosterItemRow {
        contact_jid: contact_jid.to_string(),
        name: None,
        subscription: subscription.to_string(),
        ask: None,
        approved: false,
        groups: vec![],
    }
}

#[tokio::test]
async fn test_apply_roster_change_upsert_and_remove() {
    let db = setup_test_db().await;
    let storage = DatabaseRosterStorage::new(db);

    let user_jid: BareJid = "alice@example.com".parse().unwrap();
    let contact_jid: BareJid = "bob@example.com".parse().unwrap();

    let roster = storage.get_roster(&user_jid).await.unwrap();
    assert!(roster.is_empty());

    let row = RosterItemRow {
        contact_jid: contact_jid.to_string(),
        name: Some("Bob".to_string()),
        subscription: "none".to_string(),
        ask: None,
        approved: false,
        groups: vec!["Friends".to_string()],
    };
    let (added, _) = storage
        .apply_roster_change(&user_jid, RosterRowChange::Upsert(row))
        .await
        .unwrap();
    assert!(matches!(added.kind, RosterRowMutationKind::Added(_)));

    let updated_row = RosterItemRow {
        contact_jid: contact_jid.to_string(),
        name: Some("Robert".to_string()),
        subscription: "both".to_string(),
        ask: None,
        approved: false,
        groups: vec!["Friends".to_string(), "Work".to_string()],
    };
    let (updated, _) = storage
        .apply_roster_change(&user_jid, RosterRowChange::Upsert(updated_row))
        .await
        .unwrap();
    assert!(matches!(updated.kind, RosterRowMutationKind::Updated(_)));
    assert_ne!(added.version, updated.version);

    let (removed, _) = storage
        .apply_roster_change(&user_jid, RosterRowChange::Remove(contact_jid.clone()))
        .await
        .unwrap();
    assert!(matches!(removed.kind, RosterRowMutationKind::Removed(_)));
    assert_ne!(updated.version, removed.version);

    assert!(!storage
        .has_roster_item(&user_jid, &contact_jid)
        .await
        .unwrap());
}

#[tokio::test]
async fn test_apply_roster_change_remove_missing_returns_error() {
    let db = setup_test_db().await;
    let storage = DatabaseRosterStorage::new(db);

    let user_jid: BareJid = "alice@example.com".parse().unwrap();
    let contact_jid: BareJid = "ghost@example.com".parse().unwrap();

    let result = storage
        .apply_roster_change(&user_jid, RosterRowChange::Remove(contact_jid))
        .await;
    assert!(matches!(result, Err(RosterStorageError::ItemNotFound)));
}

#[tokio::test]
async fn test_apply_subscription_update_bumps_version() {
    let db = setup_test_db().await;
    let storage = DatabaseRosterStorage::new(db);

    let user_jid: BareJid = "alice@example.com".parse().unwrap();
    let contact_jid: BareJid = "bob@example.com".parse().unwrap();

    let (first, _) = storage
        .apply_subscription_update(&user_jid, &contact_jid, "none", Some("subscribe"))
        .await
        .unwrap();
    match &first.kind {
        RosterRowMutationKind::Added(row) => {
            assert_eq!(row.subscription, "none");
            assert_eq!(row.ask, Some("subscribe".to_string()));
        }
        other => panic!("expected Added, got {:?}", other),
    }

    let (second, _) = storage
        .apply_subscription_update(&user_jid, &contact_jid, "to", None)
        .await
        .unwrap();
    match &second.kind {
        RosterRowMutationKind::Updated(row) => {
            assert_eq!(row.subscription, "to");
            assert!(row.ask.is_none());
        }
        other => panic!("expected Updated, got {:?}", other),
    }
    assert_ne!(first.version, second.version);
}

#[test]
fn subscription_update_uses_sql_boolean_literal_for_postgres() {
    let sql = super::mutation::COMMIT_SUBSCRIPTION_UPDATE_SQL;
    assert!(
        sql.contains("FALSE"),
        "Postgres roster_items.approved is BOOLEAN and must not be written as an integer literal"
    );
    assert!(
        !sql.contains(", 0,"),
        "integer approved literals fail against Postgres BOOLEAN columns"
    );
}

#[tokio::test]
async fn test_presence_queries() {
    let db = setup_test_db().await;
    let storage = DatabaseRosterStorage::new(db);

    let user_jid: BareJid = "alice@example.com".parse().unwrap();

    for (contact, subscription) in [
        ("bob@example.com", "to"),
        ("carol@example.com", "from"),
        ("dan@example.com", "both"),
        ("eve@example.com", "none"),
    ] {
        let _ = storage
            .apply_roster_change(
                &user_jid,
                RosterRowChange::Upsert(make_row(contact, subscription)),
            )
            .await
            .unwrap();
    }

    let subscribers = storage.get_presence_subscribers(&user_jid).await.unwrap();
    assert_eq!(subscribers.len(), 2);
    assert!(subscribers.contains(&"carol@example.com".to_string()));
    assert!(subscribers.contains(&"dan@example.com".to_string()));

    let subscriptions = storage.get_presence_subscriptions(&user_jid).await.unwrap();
    assert_eq!(subscriptions.len(), 2);
    assert!(subscriptions.contains(&"bob@example.com".to_string()));
    assert!(subscriptions.contains(&"dan@example.com".to_string()));
}

#[tokio::test]
async fn test_get_or_create_roster_version_synthesises_for_empty_roster() {
    let db = setup_test_db().await;
    let storage = DatabaseRosterStorage::new(db);

    let user_jid: BareJid = "alice@example.com".parse().unwrap();
    assert!(storage
        .get_roster_version(&user_jid)
        .await
        .unwrap()
        .is_none());

    let v1 = storage
        .get_or_create_roster_version(&user_jid)
        .await
        .unwrap();
    let v2 = storage
        .get_or_create_roster_version(&user_jid)
        .await
        .unwrap();
    assert_eq!(v1, v2, "second call should return the same version");
}

/// XEP-0237 §2.6 conformance regression test (T6 in PR #336).
///
/// Concurrent mutations against the same user must yield distinct versions —
/// "the version contained in a roster push MUST be unique" / "in order of
/// modification". The per-user lock in `apply_roster_change` is what holds
/// these MUSTs. If a future change splits the mutation+version-bump into
/// two awaits without serialisation, this test will start failing under
/// load.
///
/// Runs on a multi-threaded runtime so the spawned tasks interleave on
/// distinct OS threads, exercising real lock contention rather than
/// cooperative scheduling on a single thread.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn apply_roster_change_emits_unique_versions_under_concurrency() {
    let db = setup_test_db().await;
    let storage = DatabaseRosterStorage::new(db);
    let user_jid: BareJid = "alice@example.com".parse().unwrap();

    const N: usize = 16;
    let mut handles = Vec::with_capacity(N);
    for i in 0..N {
        let storage = storage.clone();
        let user_jid = user_jid.clone();
        let row = make_row(&format!("contact{i}@example.com"), "none");
        handles.push(tokio::spawn(async move {
            let (mutation, _lock) = storage
                .apply_roster_change(&user_jid, RosterRowChange::Upsert(row))
                .await
                .unwrap();
            mutation.version
        }));
    }

    let mut versions = Vec::with_capacity(N);
    for h in handles {
        versions.push(h.await.unwrap());
    }

    let unique: std::collections::HashSet<_> =
        versions.iter().map(|v| v.as_str().to_string()).collect();
    assert_eq!(
        unique.len(),
        N,
        "all {N} concurrent mutations must produce distinct versions; got {versions:?}"
    );
}

/// XEP-0237 §2.6 conformance regression test (companion to T6, addresses
/// the snapshot-vs-mutation race called out by code review on PR #336).
///
/// Under concurrent writers and a reader spinning `snapshot_roster`, every
/// snapshot must see a (items, version) pair that was actually a state of
/// the storage at some point in time — never a torn read where items
/// reflect mutation k+1 but version reads as V_k. Without the per-user
/// lock around `snapshot_roster`, this property does not hold.
///
/// We probe the invariant by recording each writer's (post-mutation
/// version, post-mutation item count) into a shared map. Every snapshot
/// the reader takes must produce a (items.len(), version) pair that
/// matches one of those recorded states (or the empty starting state).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn snapshot_roster_is_atomic_against_concurrent_mutations() {
    let db = setup_test_db().await;
    let storage = DatabaseRosterStorage::new(db);
    let user_jid: BareJid = "alice@example.com".parse().unwrap();

    // Map from observed (items_count, version_string) to how many times
    // that pair was the post-mutation state of the storage. The empty
    // starting state is implicitly allowed because `snapshot_roster`
    // synthesises a fresh ver for an empty roster — and that fresh ver
    // is then stored, so a subsequent mutation will produce ver != that.
    // We record only writer-observed states.
    let known_states: Arc<DashMap<(usize, String), ()>> = Arc::new(DashMap::new());

    const WRITERS: usize = 12;
    const READS_PER_WRITER: usize = 4;

    let mut writers = Vec::with_capacity(WRITERS);
    for i in 0..WRITERS {
        let storage = storage.clone();
        let user_jid = user_jid.clone();
        let states = known_states.clone();
        writers.push(tokio::spawn(async move {
            let row = make_row(&format!("contact{i}@example.com"), "none");
            let (mutation, _lock) = storage
                .apply_roster_change(&user_jid, RosterRowChange::Upsert(row))
                .await
                .unwrap();
            // Record the post-mutation count + version *before* dropping
            // the lock so a concurrent reader can never observe a state
            // ahead of what's recorded here.
            let count = storage.get_roster(&user_jid).await.unwrap().len();
            states.insert((count, mutation.version.as_str().to_string()), ());
        }));
    }

    let mut readers = Vec::with_capacity(WRITERS * READS_PER_WRITER);
    for _ in 0..(WRITERS * READS_PER_WRITER) {
        let storage = storage.clone();
        let user_jid = user_jid.clone();
        let states = known_states.clone();
        readers.push(tokio::spawn(async move {
            let (items, version) = storage.snapshot_roster(&user_jid).await.unwrap();
            let key = (items.len(), version.as_str().to_string());
            // The snapshot's pair must either be the empty-roster bootstrap
            // (count=0, ver synthesised) or a state recorded by a writer.
            // A reader that synthesises and stores a ver before any
            // writer runs is harmless — it then becomes the storage's
            // current ver, and the next writer will replace it.
            key.0 == 0 || states.contains_key(&key)
        }));
    }

    for w in writers {
        w.await.unwrap();
    }
    for r in readers {
        assert!(
            r.await.unwrap(),
            "snapshot_roster returned a (count, ver) pair that no mutation ever produced"
        );
    }
}
