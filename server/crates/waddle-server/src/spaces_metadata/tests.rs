//! Round-trip tests for the SQLite-backed [`SpacesMetadataStore`].

use jid::BareJid;

use super::{DatabaseSpacesMetadataStore, SpaceMetadata, SpacesMetadataStore};

fn space_jid(local: &str) -> BareJid {
    format!("{local}@spaces.localhost")
        .parse()
        .expect("valid space JID")
}

fn metadata(
    space_jid: BareJid,
    name: &str,
    description: Option<&str>,
    icon_url: Option<&str>,
    created_at: i64,
    updated_at: i64,
) -> SpaceMetadata {
    SpaceMetadata {
        space_jid,
        name: name.to_string(),
        description: description.map(str::to_string),
        icon_url: icon_url.map(str::to_string),
        created_at,
        updated_at,
    }
}

async fn fresh_store() -> DatabaseSpacesMetadataStore {
    DatabaseSpacesMetadataStore::open(Some("sqlite::memory:"))
        .await
        .expect("open in-memory spaces_metadata store")
}

#[tokio::test]
async fn upsert_then_get_round_trips_full_row() {
    let store = fresh_store().await;
    let row = metadata(
        space_jid("design"),
        "Design",
        Some("Shared design space"),
        Some("https://cdn.example/design.png"),
        1_700_000_000,
        1_700_000_000,
    );

    store.upsert(&row).await.expect("upsert");

    let fetched = store
        .get(&row.space_jid)
        .await
        .expect("get")
        .expect("row present after upsert");
    assert_eq!(fetched, row);
}

#[tokio::test]
async fn upsert_overwrites_existing_row_and_updates_timestamp() {
    let store = fresh_store().await;
    let initial = metadata(
        space_jid("design"),
        "Design",
        Some("v1 desc"),
        Some("https://cdn.example/v1.png"),
        1_700_000_000,
        1_700_000_000,
    );
    store.upsert(&initial).await.expect("initial upsert");

    let updated = SpaceMetadata {
        name: "Design Studio".to_string(),
        description: Some("v2 desc".to_string()),
        icon_url: None,
        updated_at: 1_700_000_500,
        // created_at is left at its original value — admin V2 callers
        // pass through the existing value on update.
        ..initial.clone()
    };
    store.upsert(&updated).await.expect("re-upsert");

    let fetched = store
        .get(&initial.space_jid)
        .await
        .expect("get")
        .expect("row present after re-upsert");
    assert_eq!(fetched.name, "Design Studio");
    assert_eq!(fetched.description.as_deref(), Some("v2 desc"));
    assert!(fetched.icon_url.is_none(), "icon cleared on update");
    assert_eq!(fetched.updated_at, 1_700_000_500);
    assert_eq!(
        fetched.created_at, 1_700_000_000,
        "created_at preserved across overwrite"
    );
}

#[tokio::test]
async fn delete_returns_true_when_row_present() {
    let store = fresh_store().await;
    let row = metadata(
        space_jid("ops"),
        "Ops",
        None,
        None,
        1_700_000_000,
        1_700_000_000,
    );
    store.upsert(&row).await.expect("upsert");

    let removed = store.delete(&row.space_jid).await.expect("delete");
    assert!(removed, "delete returns true when a row was removed");
    let after = store.get(&row.space_jid).await.expect("get");
    assert!(after.is_none(), "row absent after delete");
}

#[tokio::test]
async fn delete_returns_false_when_row_absent() {
    let store = fresh_store().await;
    let removed = store
        .delete(&space_jid("ghost"))
        .await
        .expect("delete missing");
    assert!(!removed, "delete on missing row returns false");
}

#[tokio::test]
async fn get_returns_none_for_unknown_jid() {
    let store = fresh_store().await;
    let result = store.get(&space_jid("ghost")).await.expect("get");
    assert!(result.is_none(), "get returns None for unknown space JID");
}

#[tokio::test]
async fn list_all_returns_rows_in_created_at_ascending_order() {
    let store = fresh_store().await;
    let alpha = metadata(
        space_jid("alpha"),
        "Alpha",
        None,
        None,
        1_700_000_000,
        1_700_000_000,
    );
    let beta = metadata(
        space_jid("beta"),
        "Beta",
        None,
        None,
        1_700_000_100,
        1_700_000_100,
    );
    let gamma = metadata(
        space_jid("gamma"),
        "Gamma",
        None,
        None,
        1_700_000_200,
        1_700_000_200,
    );

    // Insert out of order to confirm ordering is by `created_at`, not by
    // physical insertion order.
    store.upsert(&beta).await.expect("upsert beta");
    store.upsert(&gamma).await.expect("upsert gamma");
    store.upsert(&alpha).await.expect("upsert alpha");

    let rows = store.list_all().await.expect("list_all");
    assert_eq!(rows, vec![alpha, beta, gamma]);
}

#[tokio::test]
async fn list_all_on_empty_store_returns_empty_vec() {
    let store = fresh_store().await;
    let rows = store.list_all().await.expect("list_all empty");
    assert!(rows.is_empty());
}
