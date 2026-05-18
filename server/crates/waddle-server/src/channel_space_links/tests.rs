//! Round-trip tests for the SQLite-backed [`ChannelSpaceLinkStore`].

use jid::BareJid;

use super::{ChannelSpaceLink, ChannelSpaceLinkStore, DatabaseChannelSpaceLinkStore};

fn channel_jid(local: &str) -> BareJid {
    format!("{local}@muc.localhost")
        .parse()
        .expect("valid channel JID")
}

fn space_jid(local: &str) -> BareJid {
    format!("{local}@spaces.localhost")
        .parse()
        .expect("valid space JID")
}

fn link(channel: BareJid, space: BareJid, created_at: i64) -> ChannelSpaceLink {
    ChannelSpaceLink {
        channel_jid: channel,
        space_jid: space,
        created_at,
    }
}

async fn fresh_store() -> DatabaseChannelSpaceLinkStore {
    DatabaseChannelSpaceLinkStore::open(Some("sqlite::memory:"))
        .await
        .expect("open in-memory channel_space_links store")
}

#[tokio::test]
async fn set_then_get_round_trips_full_row() {
    let store = fresh_store().await;
    let row = link(channel_jid("general"), space_jid("eng"), 1_700_000_000);

    store.set(&row).await.expect("set");

    let fetched = store
        .get(&row.channel_jid)
        .await
        .expect("get")
        .expect("row present after set");
    assert_eq!(fetched, row);
}

#[tokio::test]
async fn set_preserves_created_at_when_relinking_channel() {
    // Reassigning a channel to a different space MUST keep the original
    // `created_at` so the list ordering stays stable.
    let store = fresh_store().await;
    let initial = link(channel_jid("general"), space_jid("eng"), 1_700_000_000);
    store.set(&initial).await.expect("set initial");

    let relinked = ChannelSpaceLink {
        space_jid: space_jid("design"),
        created_at: 1_700_000_500,
        ..initial.clone()
    };
    store.set(&relinked).await.expect("relink");

    let fetched = store
        .get(&initial.channel_jid)
        .await
        .expect("get")
        .expect("row present after relink");
    assert_eq!(fetched.space_jid, space_jid("design"));
    assert_eq!(
        fetched.created_at, 1_700_000_000,
        "created_at preserved across relink"
    );
}

#[tokio::test]
async fn clear_returns_true_when_row_present() {
    let store = fresh_store().await;
    let row = link(channel_jid("general"), space_jid("eng"), 1_700_000_000);
    store.set(&row).await.expect("set");

    let removed = store.clear(&row.channel_jid).await.expect("clear");
    assert!(removed, "clear returns true when a row was removed");
    let after = store.get(&row.channel_jid).await.expect("get");
    assert!(after.is_none(), "row absent after clear");
}

#[tokio::test]
async fn clear_returns_false_when_row_absent() {
    let store = fresh_store().await;
    let removed = store
        .clear(&channel_jid("ghost"))
        .await
        .expect("clear missing");
    assert!(!removed, "clear on missing row returns false");
}

#[tokio::test]
async fn get_returns_none_for_unknown_channel() {
    let store = fresh_store().await;
    let result = store.get(&channel_jid("ghost")).await.expect("get");
    assert!(result.is_none(), "get returns None for unknown channel JID");
}

#[tokio::test]
async fn list_channels_in_space_filters_by_space_and_orders_by_created_at() {
    let store = fresh_store().await;
    let alpha = link(channel_jid("alpha"), space_jid("eng"), 1_700_000_000);
    let beta = link(channel_jid("beta"), space_jid("eng"), 1_700_000_100);
    let gamma = link(channel_jid("gamma"), space_jid("eng"), 1_700_000_200);
    let unrelated = link(channel_jid("delta"), space_jid("design"), 1_700_000_050);

    // Insert out of order to confirm ordering is by `created_at`, not by
    // physical insertion order.
    store.set(&beta).await.expect("set beta");
    store.set(&unrelated).await.expect("set unrelated");
    store.set(&gamma).await.expect("set gamma");
    store.set(&alpha).await.expect("set alpha");

    let in_eng = store
        .list_channels_in_space(&space_jid("eng"))
        .await
        .expect("list eng");
    assert_eq!(
        in_eng,
        vec![alpha.channel_jid, beta.channel_jid, gamma.channel_jid]
    );

    let in_design = store
        .list_channels_in_space(&space_jid("design"))
        .await
        .expect("list design");
    assert_eq!(in_design, vec![unrelated.channel_jid]);

    let in_empty = store
        .list_channels_in_space(&space_jid("ghost"))
        .await
        .expect("list ghost");
    assert!(in_empty.is_empty());
}

#[tokio::test]
async fn list_all_orders_by_created_at_ascending() {
    let store = fresh_store().await;
    let alpha = link(channel_jid("alpha"), space_jid("eng"), 1_700_000_000);
    let beta = link(channel_jid("beta"), space_jid("design"), 1_700_000_100);
    let gamma = link(channel_jid("gamma"), space_jid("eng"), 1_700_000_200);

    store.set(&gamma).await.expect("set gamma");
    store.set(&alpha).await.expect("set alpha");
    store.set(&beta).await.expect("set beta");

    let rows = store.list_all().await.expect("list_all");
    assert_eq!(rows, vec![alpha, beta, gamma]);
}

#[tokio::test]
async fn list_all_on_empty_store_returns_empty_vec() {
    let store = fresh_store().await;
    let rows = store.list_all().await.expect("list_all empty");
    assert!(rows.is_empty());
}
