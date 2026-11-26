//! XEP-0115 verified capabilities shared across replicas (`entity_caps`).
//!
//! Replicas share the verified `(hash, ver)` entries through the database, so
//! a replica that did not hold a resource's socket still knows what the
//! resource's advertised caps mean (XEP-0163 notification filtering). These
//! tests exercise the shared store on SQLite: content-addressing, bounds,
//! pruning, and rejection of a row that is not a well-formed disco#info
//! answer. The cross-node fan-out that depends on it is covered by
//! `clustering_cluster_e2e::pep_notification_reaches_a_contact_connected_to_another_node`.

use waddle_server::db::entity_caps::{
    EntityCapsStore, PutOutcome, MAX_FEATURES, MAX_IDENTITIES, MAX_INFO_BYTES, MAX_ROWS,
};
use waddle_server::db::{Database, MigrationRunner};
use waddle_xmpp::disco::info::{Feature, Identity};
use waddle_xmpp::xep::xep0115::{CachedDiscoInfo, CapsCacheKey};

async fn store() -> (Database, EntityCapsStore) {
    let db = Database::in_memory(&format!("entity-caps-{}", uuid::Uuid::new_v4()))
        .await
        .expect("in-memory database");
    MigrationRunner::single()
        .run(&db)
        .await
        .expect("migrations");
    let store = EntityCapsStore::new(db.clone());
    (db, store)
}

fn sample() -> CachedDiscoInfo {
    CachedDiscoInfo::new(
        vec![
            Identity::new("client", "pc", Some("Waddle Test")).with_lang(Some("en")),
            Identity::new("client", "pc", None),
        ],
        vec![
            Feature::new("http://jabber.org/protocol/disco#info"),
            Feature::new("http://jabber.org/protocol/tune+notify"),
        ],
    )
}

fn features(info: &CachedDiscoInfo) -> Vec<&str> {
    info.features.iter().map(|f| f.0.as_str()).collect()
}

#[tokio::test]
async fn a_recorded_entry_reads_back_with_its_identities_and_features() {
    let (_db, store) = store().await;
    let key = CapsCacheKey::new("sha-1", "ver-roundtrip");

    assert!(store.get(&key).await.expect("get").is_none());
    assert_eq!(
        store.put(&key, &sample(), 1).await.expect("put"),
        PutOutcome::Recorded
    );

    let read = store.get(&key).await.expect("get").expect("entry");
    assert_eq!(features(&read), features(&sample()));
    assert_eq!(read.identities.len(), 2);
    let named = &read.identities[0];
    assert_eq!(named.category, "client");
    assert_eq!(named.type_, "pc");
    assert_eq!(named.name.as_deref(), Some("Waddle Test"));
    assert_eq!(named.lang.as_deref(), Some("en"));
    assert_eq!(read.identities[1].name, None);
}

#[tokio::test]
async fn entries_are_keyed_by_hash_and_ver_and_the_first_write_stays() {
    let (_db, store) = store().await;
    let key = CapsCacheKey::new("sha-1", "ver-keyed");
    store.put(&key, &sample(), 1).await.expect("put");

    // The key is content-addressed: a second write for it changes nothing.
    let other = CachedDiscoInfo::new(vec![], vec![Feature::new("urn:example:other")]);
    assert_eq!(
        store.put(&key, &other, 2).await.expect("second put"),
        PutOutcome::Recorded
    );
    let read = store.get(&key).await.expect("get").expect("entry");
    assert_eq!(features(&read), features(&sample()));

    // The same ver under another hash family is a different entry.
    assert!(store
        .get(&CapsCacheKey::new("sha-256", "ver-keyed"))
        .await
        .expect("get")
        .is_none());
}

#[tokio::test]
async fn an_entry_over_a_bound_is_not_recorded() {
    let (_db, store) = store().await;

    let many_features = CachedDiscoInfo::new(
        vec![],
        (0..=MAX_FEATURES)
            .map(|i| Feature::new(&format!("urn:example:f{i}")))
            .collect(),
    );
    let key = CapsCacheKey::new("sha-1", "ver-many-features");
    assert_eq!(
        store.put(&key, &many_features, 1).await.expect("put"),
        PutOutcome::OverBound
    );
    assert!(store.get(&key).await.expect("get").is_none());

    let many_identities = CachedDiscoInfo::new(
        (0..=MAX_IDENTITIES)
            .map(|i| Identity::new("client", "pc", Some(&format!("n{i}"))))
            .collect(),
        vec![],
    );
    let key = CapsCacheKey::new("sha-1", "ver-many-identities");
    assert_eq!(
        store.put(&key, &many_identities, 1).await.expect("put"),
        PutOutcome::OverBound
    );

    let long_feature =
        CachedDiscoInfo::new(vec![], vec![Feature::new(&"x".repeat(MAX_INFO_BYTES))]);
    let key = CapsCacheKey::new("sha-1", "ver-long-feature");
    assert_eq!(
        store.put(&key, &long_feature, 1).await.expect("put"),
        PutOutcome::OverBound
    );

    let key = CapsCacheKey::new("sha-1", "v".repeat(257));
    assert_eq!(
        store.put(&key, &sample(), 1).await.expect("put"),
        PutOutcome::OverBound
    );
    assert!(store.get(&key).await.expect("get").is_none());
}

#[tokio::test]
async fn a_row_that_is_not_a_well_formed_answer_is_not_an_entry() {
    let (db, store) = store().await;
    let guard = db.guard().await.expect("guard");
    for (ver, info) in [
        ("ver-garbage", "not xml"),
        (
            "ver-duplicate-feature",
            "<query xmlns='http://jabber.org/protocol/disco#info'>\
             <feature var='urn:example:a'/><feature var='urn:example:a'/></query>",
        ),
        ("ver-wrong-root", "<iq xmlns='jabber:client'/>"),
    ] {
        guard
            .execute(
                "INSERT INTO entity_caps (hash, ver, info, verified_at_ms) VALUES (?, ?, ?, ?)",
                waddle_server::db_params!["sha-1", ver, info, 1_i64],
            )
            .await
            .expect("insert");
        assert!(
            store
                .get(&CapsCacheKey::new("sha-1", ver))
                .await
                .expect("get")
                .is_none(),
            "{ver} must not read as an entry"
        );
    }
}

#[tokio::test]
async fn a_write_over_the_row_bound_prunes_the_oldest_entries() {
    let (db, store) = store().await;
    let stub = "<query xmlns='http://jabber.org/protocol/disco#info'/>";
    {
        let guard = db.guard().await.expect("guard");
        guard
            .execute(
                "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?) \
                 INSERT INTO entity_caps (hash, ver, info, verified_at_ms) \
                 SELECT 'sha-1', 'v' || i, ?, i FROM n",
                waddle_server::db_params![MAX_ROWS, stub],
            )
            .await
            .expect("fill the table to its bound");
    }
    assert!(store
        .get(&CapsCacheKey::new("sha-1", "v1"))
        .await
        .expect("get")
        .is_some());

    let fresh = CapsCacheKey::new("sha-1", "ver-fresh");
    store
        .put(&fresh, &sample(), MAX_ROWS + 1)
        .await
        .expect("put");

    assert!(
        store
            .get(&CapsCacheKey::new("sha-1", "v1"))
            .await
            .expect("get")
            .is_none(),
        "the oldest entry is pruned"
    );
    assert!(store
        .get(&CapsCacheKey::new("sha-1", "v2"))
        .await
        .expect("get")
        .is_some());
    assert!(store.get(&fresh).await.expect("get").is_some());
}
