//! XEP-0431: Full Text Search in MUC Archives — dedicated test suite.
//!
//! Covers `MamStorage::query_messages` with a `fulltext` filter and the
//! `matches_fulltext` predicate end-to-end.

use chrono::Utc;
use waddle_xmpp::mam::{ArchivedMessage, InMemoryMamStorage, MamQuery, MamStorage, RichText};

fn msg(from: &str, body: &str) -> ArchivedMessage {
    ArchivedMessage {
        id: String::new(),
        timestamp: Utc::now(),
        from: format!("room@muc.example.com/{from}"),
        to: "room@muc.example.com".to_string(),
        body: body.to_string(),
        stanza_id: None,
        ..Default::default()
    }
}

async fn store(storage: &InMemoryMamStorage, room: &str, m: ArchivedMessage) {
    storage.store_message(room, &m).await.expect("store");
}

#[tokio::test]
async fn xep0431_fulltext_single_term_matches_body() {
    let storage = InMemoryMamStorage::new();
    store(&storage, "room@muc", msg("alice", "the deploy is broken")).await;
    store(&storage, "room@muc", msg("bob", "working on a new feature")).await;

    let query = MamQuery {
        fulltext: RichText::new("deploy"),
        ..Default::default()
    };
    let result = storage
        .query_messages("room@muc", &query)
        .await
        .expect("query");

    assert_eq!(result.messages.len(), 1);
    assert!(result.messages[0].body.contains("deploy"));
}

#[tokio::test]
async fn xep0431_fulltext_multi_term_requires_all_terms() {
    let storage = InMemoryMamStorage::new();
    store(
        &storage,
        "room@muc",
        msg("alice", "release notes for version 2"),
    )
    .await;
    store(&storage, "room@muc", msg("bob", "notes about the build")).await;
    store(&storage, "room@muc", msg("carol", "unrelated message")).await;

    let query = MamQuery {
        fulltext: RichText::new("release notes"),
        ..Default::default()
    };
    let result = storage
        .query_messages("room@muc", &query)
        .await
        .expect("query");

    assert_eq!(result.messages.len(), 1);
    assert!(result.messages[0].body.contains("release"));
    assert!(result.messages[0].body.contains("notes"));
}

#[tokio::test]
async fn xep0431_fulltext_is_case_insensitive() {
    let storage = InMemoryMamStorage::new();
    store(
        &storage,
        "room@muc",
        msg("alice", "Randax mentioned the API deadline"),
    )
    .await;

    let query = MamQuery {
        fulltext: RichText::new("randax api"),
        ..Default::default()
    };
    let result = storage
        .query_messages("room@muc", &query)
        .await
        .expect("query");

    assert_eq!(result.messages.len(), 1);
}

#[tokio::test]
async fn xep0431_fulltext_no_match_returns_empty() {
    let storage = InMemoryMamStorage::new();
    store(&storage, "room@muc", msg("alice", "hello world")).await;

    let query = MamQuery {
        fulltext: RichText::new("kubernetes"),
        ..Default::default()
    };
    let result = storage
        .query_messages("room@muc", &query)
        .await
        .expect("query");

    assert!(result.messages.is_empty());
    assert!(result.complete);
}

#[tokio::test]
async fn xep0431_fulltext_scoped_to_archive_jid() {
    let storage = InMemoryMamStorage::new();
    store(
        &storage,
        "room-a@muc",
        msg("alice", "discussing the roadmap"),
    )
    .await;
    store(
        &storage,
        "room-b@muc",
        msg("bob", "unrelated channel message"),
    )
    .await;

    let query = MamQuery {
        fulltext: RichText::new("roadmap"),
        ..Default::default()
    };

    let result_a = storage
        .query_messages("room-a@muc", &query)
        .await
        .expect("query room-a");
    assert_eq!(result_a.messages.len(), 1);

    let result_b = storage
        .query_messages("room-b@muc", &query)
        .await
        .expect("query room-b");
    assert!(result_b.messages.is_empty());
}

#[tokio::test]
async fn xep0431_fulltext_respects_max_limit() {
    let storage = InMemoryMamStorage::new();
    for i in 0..10 {
        store(
            &storage,
            "room@muc",
            msg("user", &format!("message about the project number {i}")),
        )
        .await;
    }

    let query = MamQuery {
        fulltext: RichText::new("project"),
        max: Some(3),
        ..Default::default()
    };
    let result = storage
        .query_messages("room@muc", &query)
        .await
        .expect("query");

    assert_eq!(result.messages.len(), 3);
    assert!(!result.complete);
}

#[tokio::test]
async fn xep0431_no_fulltext_returns_all_messages() {
    let storage = InMemoryMamStorage::new();
    store(&storage, "room@muc", msg("alice", "first message")).await;
    store(&storage, "room@muc", msg("bob", "second message")).await;

    let query = MamQuery::default();
    let result = storage
        .query_messages("room@muc", &query)
        .await
        .expect("query");

    assert_eq!(result.messages.len(), 2);
}

#[tokio::test]
async fn xep0431_empty_last_page_query_returns_recent_messages() {
    let storage = InMemoryMamStorage::new();
    for i in 0..5 {
        store(&storage, "room@muc", msg("user", &format!("message {i}"))).await;
    }

    // XEP-0059 §2.5: empty before_id requests the last page.
    let query = MamQuery {
        before_id: Some(String::new()),
        max: Some(3),
        ..Default::default()
    };
    let result = storage
        .query_messages("room@muc", &query)
        .await
        .expect("query");

    assert_eq!(result.messages.len(), 3);
}
