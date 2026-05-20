//! XEP-0503 Spaces — single-membership invariant tests at the storage
//! trait surface.
//!
//! These tests pin the storage primitives the publish path uses to
//! enforce that a XEP-0503 channel bookmark lives in exactly one
//! Space at a time. A prior bug let the same room bookmark sit in
//! both `general` and another Space simultaneously; the
//! `find_node_for_item` query then returned the alphabetically-first
//! Space, pinning rooms under `general` regardless of where the user
//! had actually published them.
//!
//! The fix has two surfaces:
//!
//! 1. `list_node_names_for_item` — exposes every node that contains
//!    a given item id, so the publish path can identify and retract
//!    stale duplicates before re-publishing.
//! 2. `find_node_for_item` — returns at most one node; the in-memory
//!    backend returns the only/first matching node, the DB backend
//!    tiebreaks by most recent publish (`ORDER BY seq DESC`) so a
//!    user-driven move wins over a legacy duplicate.
//!
//! Tests run against `InMemoryPubSubStorage` so they exercise the
//! trait surface directly without needing a live SQLite backend.

use minidom::Element;
use waddle_xmpp::pubsub::{InMemoryPubSubStorage, PubSubItem, PubSubStorage};

fn spaces_jid() -> jid::BareJid {
    "spaces.example.com".parse().expect("bare jid")
}

fn room_bookmark_item(room_jid: &str) -> PubSubItem {
    let payload = Element::builder("conference", "urn:xmpp:bookmarks:1")
        .attr(minidom::rxml::xml_ncname!("name").to_owned(), "Test Room")
        .attr(minidom::rxml::xml_ncname!("autojoin").to_owned(), "true")
        .build();
    PubSubItem {
        id: Some(room_jid.to_string()),
        publisher: None,
        payload: Some(payload),
    }
}

async fn make_node(storage: &InMemoryPubSubStorage, owner: &jid::BareJid, node: &str) {
    storage
        .get_or_create_node(owner, node)
        .await
        .expect("create node");
}

// ── §3 single-membership: list_node_names_for_item ──────────────────

#[tokio::test]
async fn xep0503_list_returns_empty_when_item_missing() {
    let storage = InMemoryPubSubStorage::new();
    let spaces = spaces_jid();
    make_node(&storage, &spaces, "general").await;
    make_node(&storage, &spaces, "notifications").await;

    let names = storage
        .list_node_names_for_item(&spaces, "room@muc.example.com")
        .await
        .expect("ok");
    assert!(names.is_empty());
}

#[tokio::test]
async fn xep0503_list_returns_single_node_after_single_publish() {
    let storage = InMemoryPubSubStorage::new();
    let spaces = spaces_jid();
    make_node(&storage, &spaces, "notifications").await;
    storage
        .publish_item(
            &spaces,
            "notifications",
            &room_bookmark_item("alerts@muc.example.com"),
            None,
            false,
        )
        .await
        .expect("publish");

    let names = storage
        .list_node_names_for_item(&spaces, "alerts@muc.example.com")
        .await
        .expect("ok");
    assert_eq!(names, vec!["notifications".to_string()]);
}

#[tokio::test]
async fn xep0503_list_surfaces_every_node_containing_item() {
    // The reproducer for the original "rooms always show under
    // General" bug: the same room bookmark id ends up in both the
    // seeded `general` Space and the user-chosen `notifications`
    // Space. `list_node_names_for_item` MUST surface BOTH so the
    // publish path can compensate by retracting the stale copy.
    let storage = InMemoryPubSubStorage::new();
    let spaces = spaces_jid();
    make_node(&storage, &spaces, "general").await;
    make_node(&storage, &spaces, "notifications").await;
    let item = room_bookmark_item("alerts@muc.example.com");

    storage
        .publish_item(&spaces, "general", &item, None, false)
        .await
        .expect("publish to general");
    storage
        .publish_item(&spaces, "notifications", &item, None, false)
        .await
        .expect("publish to notifications");

    let mut names = storage
        .list_node_names_for_item(&spaces, "alerts@muc.example.com")
        .await
        .expect("ok");
    names.sort();
    assert_eq!(
        names,
        vec!["general".to_string(), "notifications".to_string()],
        "the duplicate must be visible to the publish path so it can retract"
    );
}

#[tokio::test]
async fn xep0503_list_drops_node_after_retract() {
    // The "move" operation in handle_spaces_publish: retract from
    // every other Space node before publishing. After the retract,
    // the membership list must shrink to just the target node.
    let storage = InMemoryPubSubStorage::new();
    let spaces = spaces_jid();
    make_node(&storage, &spaces, "general").await;
    make_node(&storage, &spaces, "notifications").await;
    let item = room_bookmark_item("alerts@muc.example.com");

    storage
        .publish_item(&spaces, "general", &item, None, false)
        .await
        .expect("publish to general");
    storage
        .publish_item(&spaces, "notifications", &item, None, false)
        .await
        .expect("publish to notifications");

    // Simulate the publish path's retract-from-other-nodes step.
    let retracted = storage
        .retract_item(&spaces, "general", "alerts@muc.example.com")
        .await
        .expect("retract from general");
    assert!(
        retracted,
        "the legacy general copy MUST be found and removed"
    );

    let names = storage
        .list_node_names_for_item(&spaces, "alerts@muc.example.com")
        .await
        .expect("ok");
    assert_eq!(
        names,
        vec!["notifications".to_string()],
        "after moving, the room MUST live in exactly one Space"
    );
}

// ── find_node_for_item: single-source-of-truth lookup ──────────────

#[tokio::test]
async fn xep0503_find_node_returns_none_when_item_missing() {
    let storage = InMemoryPubSubStorage::new();
    let spaces = spaces_jid();
    make_node(&storage, &spaces, "general").await;

    let node = storage
        .find_node_for_item(&spaces, "ghost@muc.example.com")
        .await
        .expect("ok");
    assert!(node.is_none());
}

#[tokio::test]
async fn xep0503_find_node_returns_the_only_membership_after_move() {
    // Post-fix steady state: handle_spaces_publish retracted the
    // legacy `general` copy, so `find_node_for_item` returns the
    // single remaining Space deterministically.
    let storage = InMemoryPubSubStorage::new();
    let spaces = spaces_jid();
    make_node(&storage, &spaces, "general").await;
    make_node(&storage, &spaces, "notifications").await;
    let item = room_bookmark_item("alerts@muc.example.com");

    storage
        .publish_item(&spaces, "general", &item, None, false)
        .await
        .expect("publish to general");
    storage
        .publish_item(&spaces, "notifications", &item, None, false)
        .await
        .expect("publish to notifications");
    storage
        .retract_item(&spaces, "general", "alerts@muc.example.com")
        .await
        .expect("retract from general");

    let node = storage
        .find_node_for_item(&spaces, "alerts@muc.example.com")
        .await
        .expect("ok")
        .expect("room is in exactly one space");
    assert_eq!(
        node.node_name, "notifications",
        "room disco MUST resolve to the Space the user moved it into"
    );
}

#[tokio::test]
async fn xep0503_publish_then_retract_clears_membership_entirely() {
    // The "remove from all spaces" path: a retract on the only
    // remaining Space leaves the room unattached. The room disco
    // metadata extension should then emit no parent form.
    let storage = InMemoryPubSubStorage::new();
    let spaces = spaces_jid();
    make_node(&storage, &spaces, "general").await;
    let item = room_bookmark_item("alerts@muc.example.com");

    storage
        .publish_item(&spaces, "general", &item, None, false)
        .await
        .expect("publish to general");
    storage
        .retract_item(&spaces, "general", "alerts@muc.example.com")
        .await
        .expect("retract from general");

    assert!(storage
        .find_node_for_item(&spaces, "alerts@muc.example.com")
        .await
        .expect("ok")
        .is_none());
    assert!(storage
        .list_node_names_for_item(&spaces, "alerts@muc.example.com")
        .await
        .expect("ok")
        .is_empty());
}
