//! Durable XEP-0198 session authority over single-node SQLite.
//!
//! These cases drive the store's public API against an in-memory database.
//! PostgreSQL attachment, the claim fence and commit-uncertainty schedules
//! need a database fixture and are not covered here.

use waddle_server::db::Database;
use waddle_server::sm_authority::{
    check_durable_sm_mode, DurableSmError, DurableSmSelection, DurableSmStore,
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

/// The durable authority is the only one: the selector variable accepts
/// `durable` and refuses every other value, `legacy` included, and the
/// selection always carries a store.
#[tokio::test]
async fn the_selection_is_always_durable() {
    assert_eq!(check_durable_sm_mode("durable"), Ok(()));
    assert_eq!(check_durable_sm_mode(" durable "), Ok(()));
    assert!(check_durable_sm_mode("legacy").is_err());
    assert!(check_durable_sm_mode("on").is_err());

    let db = Database::in_memory("sm-durable-selector")
        .await
        .expect("in-memory database");
    let durable = DurableSmSelection::open(&db, false)
        .await
        .expect("durable selection");
    assert!(durable.store().is_some());
}
