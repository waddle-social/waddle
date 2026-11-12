//! Durable XEP-0198 session authority over single-node SQLite.
//!
//! These cases drive the store's public API against an in-memory database.
//! PostgreSQL attachment, the claim fence and commit-uncertainty schedules
//! need a database fixture and are not covered here.

use waddle_server::db::Database;
use waddle_server::sm_authority::{
    DurableSmError, DurableSmMode, DurableSmSelection, DurableSmStore,
};
use waddle_xmpp::ownership::SmSessionKey;
use waddle_xmpp::stream_management::durable::ExposureBounds;

fn bounds() -> ExposureBounds {
    ExposureBounds::new(4, 4096).expect("test bounds")
}

#[tokio::test]
async fn open_creates_the_relations_and_is_repeatable() {
    let db = Database::in_memory("sm-durable-open")
        .await
        .expect("in-memory database");
    let first = DurableSmStore::open(db.clone(), bounds(), false)
        .await
        .expect("first open");
    let second = DurableSmStore::open(db, bounds(), false)
        .await
        .expect("second open over the same relations");
    assert_eq!(first.bounds(), second.bounds());
    assert_eq!(
        second
            .session(SmSessionKey::fresh())
            .await
            .expect("read an absent session"),
        None
    );
}

#[tokio::test]
async fn a_clustered_store_refuses_sqlite() {
    let db = Database::in_memory("sm-durable-clustered")
        .await
        .expect("in-memory database");
    assert!(matches!(
        DurableSmStore::open(db, bounds(), true).await,
        Err(DurableSmError::ClusterRequiresPostgres)
    ));
}

#[tokio::test]
async fn the_selector_defaults_to_legacy_and_opens_a_store_only_when_durable() {
    assert_eq!(DurableSmMode::default(), DurableSmMode::Legacy);
    assert_eq!("legacy".parse(), Ok(DurableSmMode::Legacy));
    assert_eq!("durable".parse(), Ok(DurableSmMode::Durable));
    assert!("on".parse::<DurableSmMode>().is_err());

    let db = Database::in_memory("sm-durable-selector")
        .await
        .expect("in-memory database");
    let legacy = DurableSmSelection::open(DurableSmMode::Legacy, &db, false)
        .await
        .expect("legacy selection");
    assert!(legacy.store().is_none());
    let durable = DurableSmSelection::open(DurableSmMode::Durable, &db, false)
        .await
        .expect("durable selection");
    assert!(durable.store().is_some());
}
