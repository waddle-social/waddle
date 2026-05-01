//! XEP-0201: Best Practices for Message Threads — dedicated L4 test suite.
//!
//! Locks the nested-thread (`<thread parent='X'>Y</thread>`) round-trip
//! end-to-end through the MAM archive: write site preserves parent,
//! storage round-trips parent through the new `parent_thread_id` column,
//! and replay reconstruction emits `<thread parent='X'>Y</thread>` on the
//! wire. Plus the contracts the plan locked: cross-archive parent
//! (no validation against parent thread membership), parent-only input
//! rejection (RFC 6121 §5.2.5 incoherence guard at the parser).
//!
//! Per the CLAUDE.md "XEP custom test-suite hard rule": every implemented
//! XEP — including XEP-0201, advertised via `urn:xmpp:threads:0` in
//! disco#info — MUST have a dedicated Rust test suite. This file is that
//! suite for the parent-attribute branch of the spec; the basic
//! `<thread/>` element handling is covered by the unit tests in
//! `waddle-xmpp-core/src/xep0201.rs`.

use chrono::Utc;
use minidom::Element;
use waddle_xmpp::mam::{ArchivedMessage, MamQuery, MamStorage};
use waddle_xmpp::mam::{InMemoryMamStorage, SqlxMamStorage};
use waddle_xmpp_core::xep0201::{
    build_thread_element, parse_thread_info, thread_info_from_message, ThreadInfo,
};
use xmpp_parsers::message::Message;

const ROOM: &str = "team@conference.example.com";

fn nested_thread_groupchat_row(
    archive_id: &str,
    nick: &str,
    body: &str,
    thread_id: &str,
    parent_thread_id: Option<&str>,
) -> ArchivedMessage {
    ArchivedMessage {
        id: archive_id.to_string(),
        timestamp: Utc::now(),
        from: format!("{ROOM}/{nick}"),
        to: ROOM.to_string(),
        body: body.to_string(),
        stanza_id: Some(format!("wire-{archive_id}")),
        thread_id: Some(thread_id.to_string()),
        parent_thread_id: parent_thread_id.map(str::to_string),
        reply_to_id: None,
        reply_to_jid: None,
        origin_id: None,
        message_type: "groupchat".to_string(),
        stanza_xml: None,
        rich: None,
        nickname_generation: Some(0),
    }
}

#[tokio::test]
async fn xep_0201_nested_thread_round_trips_through_mam() {
    // L4 promise: write a row with `<thread parent='root-thread'>child-thread</thread>`
    // semantics, query it back, and assert the replay payload contains
    // `<thread parent='root-thread'>child-thread</thread>` on the wire.
    let storage = SqlxMamStorage::open_in_memory()
        .await
        .expect("open sqlite in-memory");
    let row = nested_thread_groupchat_row(
        "archive-1",
        "alice",
        "nested reply body",
        "child-thread",
        Some("root-thread"),
    );
    let archive_id = storage.store_message(ROOM, &row).await.expect("store row");

    let query = MamQuery::default();
    let result = storage
        .query_messages(ROOM, &query)
        .await
        .expect("query archive");
    assert_eq!(result.messages.len(), 1);
    let retrieved = &result.messages[0];
    assert_eq!(retrieved.thread_id.as_deref(), Some("child-thread"));
    assert_eq!(retrieved.parent_thread_id.as_deref(), Some("root-thread"));

    // Build the MAM result envelope and assert the inner `<message>`
    // carries `<thread parent='root-thread'>child-thread</thread>`.
    let envelopes =
        waddle_xmpp_core::mam::build_result_messages("q1", "alice@example.com", &result.messages);
    assert_eq!(envelopes.len(), 1);
    let result_payload = envelopes[0]
        .payloads
        .iter()
        .find(|p| p.name() == "result" && p.ns() == waddle_xmpp_core::mam::MAM_NS)
        .expect("result payload");
    let forwarded = result_payload
        .children()
        .find(|c| c.name() == "forwarded" && c.ns() == waddle_xmpp_core::mam::FORWARD_NS)
        .expect("forwarded");
    let inner_msg = forwarded
        .children()
        .find(|c| c.name() == "message")
        .expect("inner message");
    let thread_elem = inner_msg
        .children()
        .find(|c| c.name() == "thread")
        .expect("thread element on replay");
    assert_eq!(thread_elem.text().trim(), "child-thread");
    assert_eq!(thread_elem.attr("parent"), Some("root-thread"));

    // Sanity: assert the same archive id was assigned (groupchat
    // canonical-id invariant).
    assert_eq!(retrieved.id, archive_id);
}

#[tokio::test]
async fn xep_0201_cross_archive_parent_thread_id_round_trips_without_validation() {
    // XEP-0201 is silent on whether the `parent` thread id must exist
    // in the same archive. Waddle's archive-write path does no
    // cross-archive validation: a parent that references a thread the
    // archive never saw is stored verbatim and replayed verbatim.
    // This test locks that contract so a future "validate parent
    // exists" change cannot land silently.
    let storage = SqlxMamStorage::open_in_memory()
        .await
        .expect("open sqlite in-memory");
    let row = nested_thread_groupchat_row(
        "archive-cross",
        "alice",
        "child of an unknown parent",
        "child-thread-a",
        Some("parent-not-in-this-archive"),
    );
    storage.store_message(ROOM, &row).await.expect("store row");

    let result = storage
        .query_messages(ROOM, &MamQuery::default())
        .await
        .expect("query");
    assert_eq!(result.messages.len(), 1);
    let retrieved = &result.messages[0];
    assert_eq!(retrieved.thread_id.as_deref(), Some("child-thread-a"));
    assert_eq!(
        retrieved.parent_thread_id.as_deref(),
        Some("parent-not-in-this-archive"),
        "cross-archive parent must round-trip; archive does not validate parent membership"
    );
}

#[test]
fn xep_0201_parent_only_input_is_rejected_at_parser() {
    // RFC 6121 §5.2.5 incoherence guard: a `<thread parent='X'/>` with
    // empty body is ill-formed (parent is meaningful only as a
    // back-reference from a thread that has its own id). The parser
    // returns None so the write path never persists `parent_thread_id`
    // without a `thread_id` companion.
    let xml = "<message xmlns='jabber:client'><thread parent='root-1'></thread></message>"
        .parse::<Element>()
        .expect("valid xml");
    assert_eq!(parse_thread_info(&xml), None);

    // Same guard via the post-reattach helper that the archive write
    // path actually uses.
    let mut msg = Message::new(None::<jid::Jid>);
    msg.payloads.push(
        Element::builder("thread", "jabber:client")
            .attr("parent", "root-1")
            .build(),
    );
    assert_eq!(thread_info_from_message(&msg), None);
}

#[test]
fn xep_0201_build_thread_element_round_trips_through_parse_thread_info() {
    // Sanity: builder + parser are inverses on the wire shape.
    let info = ThreadInfo::child("child-2", "root-1");
    let elem = build_thread_element(&info, "jabber:client");
    let wrapped = Element::builder("message", "jabber:client")
        .append(elem)
        .build();
    let parsed = parse_thread_info(&wrapped).expect("parse should succeed");
    assert_eq!(parsed.id, "child-2");
    assert_eq!(parsed.parent.as_deref(), Some("root-1"));
}

#[tokio::test]
async fn xep_0201_inmemory_mam_storage_also_round_trips_parent() {
    // The in-memory MAM backend (used by tests outside of `sqlx::sqlite`)
    // uses the same `ArchivedMessage` shape and so must round-trip
    // parent identically to the sqlx backend.
    let storage = InMemoryMamStorage::new();
    let row = nested_thread_groupchat_row(
        "archive-mem",
        "alice",
        "inmem nested",
        "child-thread-m",
        Some("root-thread-m"),
    );
    storage
        .store_message(ROOM, &row)
        .await
        .expect("store in-memory");
    let result = storage
        .query_messages(ROOM, &MamQuery::default())
        .await
        .expect("query in-memory");
    assert_eq!(result.messages.len(), 1);
    assert_eq!(
        result.messages[0].parent_thread_id.as_deref(),
        Some("root-thread-m")
    );
}
