//! Waddle-specific MAM stanza-id filter — custom test suite.
//!
//! This is a Waddle extension, not a XEP-0359 conformance suite. The filter
//! form-field var lives under `urn:waddle:mam-stanza-id:0` (not
//! `urn:xmpp:sid:0`) per CLAUDE.md: "official XEP namespaces must conform
//! exactly; Waddle-specific semantics use `urn:waddle:*`". XEP-0359 only
//! defines the stanza-id wire protocol; XEP-0313 §4.2 + XEP-0068 allow
//! custom data-form fields for archive filtering.
//!
//! The wire-level form-field parser tests live in
//! `waddle_xmpp_core::mam::tests`. This module exercises the storage
//! contract: the form field constant matches the Waddle namespace, the
//! validation caps match the pin protocol they serve, the storage filter
//! returns only the requested ids, no-match queries return an empty
//! result set (not an error), and rich payloads survive the round-trip
//! through `build_result_messages`.
//!
//! Out of scope: occupancy authz for non-occupant MAM access. XEP-0313
//! §5.1.3 requires this for members-only rooms, but today the MAM IQ
//! handler does not enforce it — that pre-existing gap is tracked
//! separately. See test 4 (`stanza_id_filter_with_no_match_returns_empty_not_error`)
//! for the storage-level access-agnostic contract.

#![cfg(test)]

use jid::{BareJid, Jid};
use minidom::Element;
use waddle_xmpp_core::mam::{
    ArchivedMessage, ArchivedRichMessage, MamFilterStanzaId, MamQuery, MAX_FILTER_STANZA_ID_LEN,
    STANZA_ID_FILTER_FIELD,
};
use waddle_xmpp_core::xep0359::StanzaId;
use xmpp_parsers::message::MessageType;

use crate::mam::{build_result_messages, InMemoryMamStorage, MamArchiveKind, MamStorage};

// ── helpers ──────────────────────────────────────────────────────────────────

fn jid(value: &str) -> Jid {
    value.parse::<Jid>().expect("valid jid literal")
}

fn bare(value: &str) -> BareJid {
    value.parse::<BareJid>().expect("valid bare jid literal")
}

fn filter_id(s: &str) -> MamFilterStanzaId {
    MamFilterStanzaId::new(s).expect("valid test fixture id")
}

/// Build an `ArchivedMessage` that mirrors the production data layout.
///
/// - `archive_id`: the canonical XEP-0359 room-stamped UUID stored in the SQL
///   `id` column (primary key). `MamQuery.stanza_ids` filters on this column.
///   This is what the chat client supplies via `roomAssignedStanzaId`.
/// - `wire_id`: the client's `<message id>` attribute stored in the SQL
///   `stanza_id` column. Different from `archive_id`.
///
/// See `groupchat_archive.rs:10,94-97` for the authoritative server-side
/// column assignment.
fn archived_with_stanza_id(archive: &BareJid, archive_id: &str, wire_id: &str) -> ArchivedMessage {
    ArchivedMessage {
        id: archive_id.to_string(),
        stanza_id: Some(StanzaId::new(
            wire_id.to_string(),
            jid(&archive.to_string()),
        )),
        message_type: MessageType::Groupchat,
        body: Some(format!("message with archive-id {archive_id}")),
        ..ArchivedMessage::for_test(jid(&format!("{archive}/alice")), jid(&archive.to_string()))
    }
}

// ── test 1 ───────────────────────────────────────────────────────────────────

/// The Waddle-specific stanza-id MAM filter field var MUST live under the
/// `urn:waddle:mam-stanza-id:0` namespace, NOT `urn:xmpp:sid:0`. XEP-0359
/// does not define "filter MAM archive by stanza-id"; the Waddle extension
/// uses XEP-0313 §4.2 + XEP-0068 Clark-notation to define its own field.
/// This test pins the constant to the Waddle namespace so a mis-edit is
/// caught immediately.
#[test]
fn stanza_id_filter_field_constant_matches_waddle_namespace() {
    assert_eq!(
        STANZA_ID_FILTER_FIELD,
        "{urn:waddle:mam-stanza-id:0}stanza-id"
    );
}

// ── test 2 ───────────────────────────────────────────────────────────────────

/// The stanza-id filter length cap must equal the pin protocol's target
/// stanza-id length cap so that a pinned-message batch query never rejects a
/// valid pin target as too long.
#[test]
fn stanza_id_filter_caps_match_pin_protocol() {
    use crate::xep::xep_waddle_pin::MAX_TARGET_STANZA_ID_LEN;
    assert_eq!(MAX_FILTER_STANZA_ID_LEN, MAX_TARGET_STANZA_ID_LEN);
}

// ── test 3 ───────────────────────────────────────────────────────────────────

/// Waddle-specific MAM stanza-id filter (XEP-0313 §4.2 + XEP-0068 custom
/// data-form var): a `{urn:waddle:mam-stanza-id:0}stanza-id` filter with a
/// single value MUST return only messages whose server-assigned stanza-id
/// matches that value.
///
/// `MamQuery.stanza_ids` filters by either the archive-primary id or the
/// stored wire `<message id>`. Room pins supply archive ids; DM pins supply
/// pair-stable wire ids so either participant can hydrate the same logical
/// pin through their personal archive.
///
/// Integration level: storage API (`InMemoryMamStorage::query_messages`).
/// The filter is exercised through the same `MamQuery.stanza_ids` path that
/// the IQ parser populates on the wire, giving full coverage of the storage
/// contract without requiring a room-actor fixture.
#[tokio::test]
async fn stanza_id_filter_returns_only_matching_message() {
    // archive_id "uuid-m1/m2" = canonical room UUID (what pin's target_stanza_id is,
    //                           stored in SQL `id` column)
    // wire_id    "wire-m1/m2" = client's <message id> (SQL `stanza_id` column)
    let store = InMemoryMamStorage::new();
    let archive = bare("room@conference.example.com");

    store
        .store_message(
            &archive,
            &archived_with_stanza_id(&archive, "uuid-m1", "wire-m1"),
        )
        .await
        .expect("store m1");
    store
        .store_message(
            &archive,
            &archived_with_stanza_id(&archive, "uuid-m2", "wire-m2"),
        )
        .await
        .expect("store m2");

    let result = store
        .query_messages(
            &archive,
            MamArchiveKind::Room,
            &MamQuery {
                stanza_ids: vec![filter_id("uuid-m1")],
                ..Default::default()
            },
        )
        .await
        .expect("query must succeed");

    assert_eq!(result.messages.len(), 1, "exactly one message returned");
    assert_eq!(
        result.messages[0].id.as_str(),
        "uuid-m1",
        "returned message has the canonical archive id"
    );
    assert!(result.complete, "single-result page is complete");
}

#[tokio::test]
async fn stanza_id_filter_matches_wire_message_id_for_dm_pin_hydration() {
    let store = InMemoryMamStorage::new();
    let archive = bare("alice@example.com");

    store
        .store_message(
            &archive,
            &archived_with_stanza_id(&archive, "alice-archive-1", "shared-wire-id"),
        )
        .await
        .expect("store matching DM archive row");
    store
        .store_message(
            &archive,
            &archived_with_stanza_id(&archive, "alice-archive-2", "other-wire-id"),
        )
        .await
        .expect("store non-matching DM archive row");

    let result = store
        .query_messages(
            &archive,
            MamArchiveKind::Room,
            &MamQuery {
                stanza_ids: vec![filter_id("shared-wire-id")],
                ..Default::default()
            },
        )
        .await
        .expect("query must succeed");

    assert_eq!(result.messages.len(), 1, "exactly one message returned");
    assert_eq!(
        result.messages[0].id.as_str(),
        "alice-archive-1",
        "wire-id filter returns the matching archive row"
    );
}

// ── test 4 ───────────────────────────────────────────────────────────────────

/// Waddle-specific MAM stanza-id filter (XEP-0313 §4.2 + XEP-0068):
/// a `{urn:waddle:mam-stanza-id:0}stanza-id` filter that matches no
/// archived message MUST return an empty result set (not an error). The
/// storage layer is correctly access-agnostic — it returns messages
/// scoped to the archive JID regardless of caller.
///
/// **Note on MAM occupancy authz (out of scope for this test):** XEP-0313
/// §5.1.3 requires that, for members-only MUC archives, only owners /
/// admins / members can query. Today the MAM IQ handler does not enforce
/// this — `archive_inbox_upload.rs` only checks that the target domain
/// matches the MUC domain. The pin-list IQ handler does check occupancy;
/// the MAM handler does not. That pre-existing gap is tracked as a
/// separate follow-up; this test does NOT compensate for it, and the
/// stanza-id filter inherits whatever authz the MAM path provides.
///
/// Integration level: storage API.
#[tokio::test]
async fn stanza_id_filter_with_no_match_returns_empty_not_error() {
    let store = InMemoryMamStorage::new();
    let archive = bare("room@conference.example.com");

    store
        .store_message(&archive, &archived_with_stanza_id(&archive, "m1", "sid-A"))
        .await
        .expect("store m1");

    let result = store
        .query_messages(
            &archive,
            MamArchiveKind::Room,
            &MamQuery {
                stanza_ids: vec![filter_id("sid-missing")],
                ..Default::default()
            },
        )
        .await
        .expect("query must succeed, not error, when no match");

    assert!(
        result.messages.is_empty(),
        "no match produces empty result, not an error"
    );
    assert!(result.complete, "empty result set is complete");
}

// ── test 5 ───────────────────────────────────────────────────────────────────

/// Waddle stanza-id filter / rich-payload round-trip: a message whose
/// `stanza_xml` contains both an OMEMO `<encrypted/>` element and an SFS
/// `<file-sharing/>` element MUST survive the full store → `stanza_ids`
/// query → `build_result_messages` pipeline with both child elements present
/// in the inner forwarded stanza, byte-for-byte.
///
/// This exercises the critical path that the chat client uses when restoring
/// the pinned-message panel: the raw `stanza_xml` column is used as the
/// authoritative inner stanza and must be preserved even for multi-payload
/// messages.
///
/// Integration level: storage API + `build_result_messages` wire builder.
#[tokio::test]
async fn stanza_id_filter_preserves_rich_payload_roundtrip() {
    let store = InMemoryMamStorage::new();
    let archive = bare("room@conference.example.com");
    let archive_jid = jid("room@conference.example.com");

    // Craft a stanza_xml that carries both an OMEMO encrypted element and an
    // XEP-0447 file-sharing element in the same message. Built via
    // minidom::Element::builder per the XML-generation hard rule.
    const CLIENT_NS: &str = "jabber:client";
    const OMEMO_AXOLOTL_NS: &str = "eu.siacs.conversations.axolotl";
    const SFS_NS: &str = "urn:xmpp:sfs:0";

    let encrypted_header = Element::builder("header", OMEMO_AXOLOTL_NS)
        .attr(minidom::rxml::xml_ncname!("sid").to_owned(), "12345")
        .build();
    let encrypted = Element::builder("encrypted", OMEMO_AXOLOTL_NS)
        .append(encrypted_header)
        .build();

    let file_name = Element::builder("name", SFS_NS).append("photo.jpg").build();
    let file = Element::builder("file", SFS_NS).append(file_name).build();
    let file_sharing = Element::builder("file-sharing", SFS_NS)
        .append(file)
        .build();

    let message = Element::builder("message", CLIENT_NS)
        .attr(
            minidom::rxml::xml_ncname!("from").to_owned(),
            "room@conference.example.com/alice",
        )
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "groupchat")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "sid-rich")
        .append(encrypted)
        .append(file_sharing)
        .build();

    let rich_stanza_xml = String::from(&message);

    let msg = ArchivedMessage {
        id: "archive-rich-1".to_string(),
        stanza_id: Some(StanzaId::new("sid-rich", archive_jid.clone())),
        message_type: MessageType::Groupchat,
        stanza_xml: Some(rich_stanza_xml.to_string()),
        rich: Some(ArchivedRichMessage::default()),
        body: None,
        ..ArchivedMessage::for_test(
            jid("room@conference.example.com/alice"),
            archive_jid.clone(),
        )
    };

    store
        .store_message(&archive, &msg)
        .await
        .expect("store rich message");

    // Query by canonical archive id — the Waddle MAM stanza-id filter path
    // (XEP-0313 §4.2 + XEP-0068 custom var under urn:waddle:mam-stanza-id:0).
    // `MamQuery.stanza_ids` filters by the `id` column (canonical UUID),
    // not by `stanza_id` (wire <message id>). See `groupchat_archive.rs:10,94-97`.
    let result = store
        .query_messages(
            &archive,
            MamArchiveKind::Room,
            &MamQuery {
                stanza_ids: vec![filter_id("archive-rich-1")],
                ..Default::default()
            },
        )
        .await
        .expect("stanza-id query succeeds");

    assert_eq!(result.messages.len(), 1, "exactly one message returned");

    // Run through the wire serializer used by the MAM IQ handler.
    let requester = jid("alice@example.com/device");
    let archive = jid("alice@example.com");
    let wire_messages = build_result_messages("qid-1", &archive, &requester, &result.messages);
    assert_eq!(wire_messages.len(), 1, "one wire message produced");

    // Extract the inner forwarded stanza from the MAM result element and
    // inspect it for both child namespaces.
    let result_el = wire_messages[0]
        .payloads
        .iter()
        .find(|e| e.name() == "result" && e.ns() == "urn:xmpp:mam:2")
        .expect("<result xmlns='urn:xmpp:mam:2'/> present");

    let forwarded = result_el
        .children()
        .find(|e| e.name() == "forwarded")
        .expect("<forwarded/> present");

    let inner = forwarded
        .children()
        .find(|e| e.name() == "message")
        .expect("inner <message/> present");

    let has_encrypted = inner
        .children()
        .any(|e| e.ns() == "eu.siacs.conversations.axolotl" && e.name() == "encrypted");
    assert!(
        has_encrypted,
        "<encrypted xmlns='eu.siacs.conversations.axolotl'/> must survive the round-trip"
    );

    let has_file_sharing = inner
        .children()
        .any(|e| e.ns() == "urn:xmpp:sfs:0" && e.name() == "file-sharing");
    assert!(
        has_file_sharing,
        "<file-sharing xmlns='urn:xmpp:sfs:0'/> must survive the round-trip"
    );
}
