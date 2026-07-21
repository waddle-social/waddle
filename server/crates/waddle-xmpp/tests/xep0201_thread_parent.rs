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
//! XEP MUST have a dedicated Rust test suite. XEP-0201 is Informational
//! and defines no disco#info feature, so Waddle does NOT advertise a
//! `urn:xmpp:threads:*` namespace — but the wire behavior (the optional
//! `parent=` attribute on `<thread/>`) is still implemented per the
//! spec. This file is that suite for the parent-attribute branch of
//! the spec; basic `<thread/>` element handling is covered by the
//! unit tests in `waddle-xmpp-core/src/xep0201.rs`.

use chrono::Utc;
use jid::{BareJid, Jid};
use minidom::Element;
use waddle_xmpp::mam::{ArchivedMessage, MamArchiveKind, MamQuery, MamStorage, StoreOutcome};
use waddle_xmpp::mam::{InMemoryMamStorage, SqlxMamStorage};
use waddle_xmpp_core::mam::ThreadId;
use waddle_xmpp_core::xep0201::{
    build_thread_element, parse_thread_info, thread_info_from_message, ThreadInfo,
};
use xmpp_parsers::message::{Message, MessageType};

const ROOM: &str = "team@conference.example.com";

fn room_bare() -> BareJid {
    ROOM.parse::<BareJid>().expect("valid bare jid")
}

fn jid_lit(value: &str) -> Jid {
    value.parse::<Jid>().expect("valid jid literal")
}

fn nested_thread_groupchat_row(
    archive_id: &str,
    nick: &str,
    body: &str,
    thread_id: &str,
    parent_thread_id: Option<&str>,
) -> ArchivedMessage {
    let id = ThreadId::new(thread_id).expect("non-empty thread id");
    let thread = match parent_thread_id.and_then(ThreadId::new) {
        Some(parent) => Some(ThreadInfo::child(id, parent)),
        None => Some(ThreadInfo::root(id)),
    };
    ArchivedMessage {
        id: archive_id.to_string(),
        timestamp: Utc::now(),
        from: jid_lit(&format!("{ROOM}/{nick}")),
        to: jid_lit(ROOM),
        body: Some(body.to_string()),
        stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
            format!("wire-{archive_id}"),
            jid_lit(ROOM),
        )),
        thread,
        reply: None,
        origin_id: None,
        message_type: MessageType::Groupchat,
        stanza_xml: None,
        rich: None,
        nickname_generation: Some(0),
    }
}

fn element_xml(element: &Element) -> String {
    let mut bytes = Vec::new();
    element.write_to(&mut bytes).expect("serialize element");
    String::from_utf8(bytes).expect("element xml is utf-8")
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
    let store_outcome = storage
        .store_message(&room_bare(), &row)
        .await
        .expect("store row");

    let query = MamQuery::default();
    let result = storage
        .query_messages(&room_bare(), MamArchiveKind::Room, &query)
        .await
        .expect("query archive");
    assert_eq!(result.messages.len(), 1);
    let retrieved = &result.messages[0];
    let thread = retrieved.thread.as_ref().expect("thread present");
    assert_eq!(thread.id.as_str(), "child-thread");
    assert_eq!(
        thread.parent.as_ref().map(|t| t.as_str()),
        Some("root-thread")
    );

    // Build the MAM result envelope and assert the inner `<message>`
    // carries `<thread parent='root-thread'>child-thread</thread>`.
    let envelopes = waddle_xmpp_core::mam::build_result_messages(
        "q1",
        &jid_lit("room@conference.example.com"),
        &jid_lit("alice@example.com"),
        &result.messages,
    );
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
        .find(|c| c.name() == "thread" && c.ns() == "jabber:client")
        .expect("thread element on replay");
    assert_eq!(thread_elem.text().trim(), "child-thread");
    assert_eq!(thread_elem.attr("parent"), Some("root-thread"));

    // Sanity: assert the same archive id was assigned (groupchat
    // canonical-id invariant).
    assert_eq!(store_outcome, StoreOutcome::Stored(retrieved.id.clone()));
}

#[test]
fn xep_0201_groupchat_stanza_xml_replay_preserves_thread_metadata() {
    // Regression for archived `stanza_xml` replay: normalization must
    // remove only the per-recipient `to` and keep/reinstall the
    // XEP-0201 `<thread/>` element.
    let archived_stanza = Element::builder("message", "jabber:client")
        .attr(
            minidom::rxml::xml_ncname!("from").to_owned(),
            "team@conference.example.com/alice",
        )
        .attr(
            minidom::rxml::xml_ncname!("to").to_owned(),
            "bob@example.com/web",
        )
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "groupchat")
        .attr(
            minidom::rxml::xml_ncname!("id").to_owned(),
            "wire-archive-xml",
        )
        .append(
            Element::builder("body", "jabber:client")
                .append("threaded reply")
                .build(),
        )
        .append(build_thread_element(
            &ThreadInfo::root(ThreadId::new("stale-thread").expect("non-empty")),
            "jabber:client",
        ))
        .append(
            Element::builder("reply", "urn:xmpp:reply:0")
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "root-thread")
                .build(),
        )
        .append(
            Element::builder("thread", "urn:example:other:0")
                .attr(minidom::rxml::xml_ncname!("kind").to_owned(), "extension")
                .append("not-xep-0201")
                .build(),
        )
        .build();
    let row = ArchivedMessage {
        id: "archive-xml".to_string(),
        timestamp: Utc::now(),
        from: jid_lit("team@conference.example.com/alice"),
        to: jid_lit(ROOM),
        body: Some("threaded reply".to_string()),
        stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
            "wire-archive-xml",
            jid_lit(ROOM),
        )),
        thread: Some(ThreadInfo::child(
            ThreadId::new("root-thread").expect("thread id"),
            ThreadId::new("parent-thread").expect("parent id"),
        )),
        reply: Some(waddle_xmpp_core::mam::ArchivedReply {
            id: waddle_xmpp_core::mam::RichMessageId::new("root-thread")
                .expect("non-empty reply id"),
            to: None,
        }),
        origin_id: None,
        message_type: MessageType::Groupchat,
        stanza_xml: Some(element_xml(&archived_stanza)),
        rich: None,
        nickname_generation: Some(0),
    };

    let envelopes = waddle_xmpp_core::mam::build_result_messages(
        "q-xml",
        &jid_lit("room@conference.example.com"),
        &jid_lit("bob@example.com"),
        &[row],
    );
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
        .find(|c| c.name() == "thread" && c.ns() == "jabber:client")
        .expect("thread element on replay");

    assert_eq!(inner_msg.attr("to"), None);
    assert_eq!(thread_elem.text().trim(), "root-thread");
    assert_eq!(thread_elem.attr("parent"), Some("parent-thread"));
    assert!(inner_msg.children().any(|child| {
        child.name() == "thread"
            && child.ns() == "urn:example:other:0"
            && child.text() == "not-xep-0201"
    }));
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
    storage
        .store_message(&room_bare(), &row)
        .await
        .expect("store row");

    let result = storage
        .query_messages(&room_bare(), MamArchiveKind::Room, &MamQuery::default())
        .await
        .expect("query");
    assert_eq!(result.messages.len(), 1);
    let retrieved = &result.messages[0];
    let thread = retrieved.thread.as_ref().expect("thread present");
    assert_eq!(thread.id.as_str(), "child-thread-a");
    assert_eq!(
        thread.parent.as_ref().map(|t| t.as_str()),
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
            .attr(minidom::rxml::xml_ncname!("parent").to_owned(), "root-1")
            .build(),
    );
    assert_eq!(thread_info_from_message(&msg), None);
}

#[test]
fn xep_0201_build_thread_element_round_trips_through_parse_thread_info() {
    // Sanity: builder + parser are inverses on the wire shape.
    let info = ThreadInfo::child(
        ThreadId::new("child-2").expect("non-empty"),
        ThreadId::new("root-1").expect("non-empty"),
    );
    let elem = build_thread_element(&info, "jabber:client");
    let wrapped = Element::builder("message", "jabber:client")
        .append(elem)
        .build();
    let parsed = parse_thread_info(&wrapped).expect("parse should succeed");
    assert_eq!(parsed.id.as_str(), "child-2");
    assert_eq!(parsed.parent.as_ref().map(|t| t.as_str()), Some("root-1"));
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
        .store_message(&room_bare(), &row)
        .await
        .expect("store in-memory");
    let result = storage
        .query_messages(&room_bare(), MamArchiveKind::Room, &MamQuery::default())
        .await
        .expect("query in-memory");
    assert_eq!(result.messages.len(), 1);
    let thread = result.messages[0]
        .thread
        .as_ref()
        .expect("in-memory backend round-trips thread");
    assert_eq!(
        thread.parent.as_ref().map(|t| t.as_str()),
        Some("root-thread-m")
    );
}

#[tokio::test]
async fn xep_0201_collapsed_thread_field_round_trips_nested_through_storage() {
    // #228 commit 4: `ArchivedMessage.thread: Option<ThreadInfo>`
    // collapses the previous flat (`thread_id`, `parent_thread_id`)
    // pair. SQL schema is unchanged (still two columns); encode splits,
    // decode combines. This locks the typed-struct round-trip end to
    // end so the field-level collapse never silently regresses to the
    // flat shape.
    let storage = SqlxMamStorage::open_in_memory()
        .await
        .expect("open sqlite in-memory");
    let original = ThreadInfo::child(
        ThreadId::new("c-collapse").expect("non-empty"),
        ThreadId::new("r-collapse").expect("non-empty"),
    );
    let mut row = nested_thread_groupchat_row(
        "archive-collapse",
        "alice",
        "collapsed",
        "c-collapse",
        Some("r-collapse"),
    );
    row.thread = Some(original.clone());
    storage
        .store_message(&room_bare(), &row)
        .await
        .expect("store row");

    let result = storage
        .query_messages(&room_bare(), MamArchiveKind::Room, &MamQuery::default())
        .await
        .expect("query");
    assert_eq!(result.messages.len(), 1);
    assert_eq!(
        result.messages[0].thread.as_ref(),
        Some(&original),
        "the typed ThreadInfo struct must round-trip exactly"
    );
}

#[tokio::test]
async fn xep_0201_collapsed_thread_field_round_trips_root_only_through_storage() {
    // Root thread (parent = None) must round-trip as `Some(ThreadInfo
    // { id, parent: None })`, not `None`. The encode/decode pair MUST
    // distinguish "no thread" from "root thread".
    let storage = SqlxMamStorage::open_in_memory()
        .await
        .expect("open sqlite in-memory");
    let original = ThreadInfo::root(ThreadId::new("root-only").expect("non-empty"));
    let mut row =
        nested_thread_groupchat_row("archive-root-only", "alice", "root only", "root-only", None);
    row.thread = Some(original.clone());
    storage
        .store_message(&room_bare(), &row)
        .await
        .expect("store row");

    let result = storage
        .query_messages(&room_bare(), MamArchiveKind::Room, &MamQuery::default())
        .await
        .expect("query");
    assert_eq!(result.messages.len(), 1);
    let thread = result.messages[0]
        .thread
        .as_ref()
        .expect("root-only thread must round-trip as Some");
    assert_eq!(thread, &original);
    assert!(
        thread.parent.is_none(),
        "root thread must decode with parent = None"
    );
}

#[tokio::test]
async fn xep_0201_collapsed_thread_field_round_trips_no_thread_through_storage() {
    // `thread: None` (no `<thread/>` element on the wire and no row
    // metadata) MUST round-trip as `None` and never decode to
    // `Some(ThreadInfo { id: "", .. })` or similar.
    let storage = SqlxMamStorage::open_in_memory()
        .await
        .expect("open sqlite in-memory");
    let row = ArchivedMessage {
        id: "archive-no-thread".to_string(),
        timestamp: Utc::now(),
        from: jid_lit(&format!("{ROOM}/alice")),
        to: jid_lit(ROOM),
        body: Some("plain body".to_string()),
        stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
            "wire-no-thread",
            jid_lit(ROOM),
        )),
        thread: None,
        reply: None,
        origin_id: None,
        message_type: MessageType::Groupchat,
        stanza_xml: None,
        rich: None,
        nickname_generation: Some(0),
    };
    storage
        .store_message(&room_bare(), &row)
        .await
        .expect("store row");

    let result = storage
        .query_messages(&room_bare(), MamArchiveKind::Room, &MamQuery::default())
        .await
        .expect("query");
    assert_eq!(result.messages.len(), 1);
    assert!(
        result.messages[0].thread.is_none(),
        "rows with no thread metadata must decode as `thread: None`"
    );
}
