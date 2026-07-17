//! XEP-0359: Unique and Stable Stanza IDs — dedicated suite.
//!
//! Locked invariants from issue #209:
//!
//! - **Q5c**: Archived `pending_delivery` rows preserve the
//!   recipient-stamped `<stanza-id by='recipient' id='...'/>`
//!   verbatim on flush. Transient rows omit it (no MAM entry exists
//!   to dedupe against, so `<stanza-id/>` would be confusing).
//! - **Q10c (client-dedup contract)**: the same stanza-id appears on
//!   the live-delivered copy AND on the recipient's MAM catch-up,
//!   so client-side dedup works.
//!
//! Citations refer to `xeps/xep-0359.xml` unless otherwise noted.

use chrono::Utc;
use jid::{BareJid, Jid};
use waddle_xmpp::mam::{InMemoryMamStorage, MamStorage, StoreOutcome};
use waddle_xmpp::pending_delivery::flush::{
    build_replay_stanza, MaterializedPayload, ReplayReason,
};
use waddle_xmpp::pending_delivery::{PendingPayload, PendingRow, PendingRowId};
use waddle_xmpp_core::xep0359::{build_stanza_id_element, StanzaId, NS_SID};
use xmpp_parsers::message::{Message, MessageType};

fn bare(s: &str) -> BareJid {
    s.parse().expect("bare jid")
}

fn dm(from: &str, to: &str, body: &str) -> Message {
    let mut m = Message::new(Some(to.parse::<Jid>().expect("jid")));
    m.from = Some(from.parse::<Jid>().expect("jid"));
    m.type_ = MessageType::Chat;
    m.bodies
        .insert(xmpp_parsers::message::Lang::new(), body.to_string());
    m
}

/// XEP-0359 §2: an origin-id remains stable when the originating client
/// retries. A MUC leave/rejoin changes the nickname generation and the real
/// full-JID resource, but the same real bare account and occupant JID still
/// identify the retry for storage dedupe (issue #1374).
#[tokio::test]
async fn xep0359_groupchat_origin_retry_after_rejoin_reuses_archive_id() {
    use waddle_xmpp_core::mam::{ArchivedMessage, ArchivedMucSender, ArchivedRichMessage};
    use waddle_xmpp_core::types::{Affiliation, Role};
    use waddle_xmpp_core::xep0359::OriginId;

    fn archived(id: &str, real_jid: &str, generation: u64) -> ArchivedMessage {
        ArchivedMessage {
            id: id.to_string(),
            body: Some("retry me".to_string()),
            origin_id: Some(OriginId::new("stable-client-origin")),
            message_type: MessageType::Groupchat,
            nickname_generation: Some(generation),
            rich: Some(ArchivedRichMessage {
                muc_sender: Some(ArchivedMucSender {
                    jid: real_jid.parse().expect("real sender JID"),
                    affiliation: Affiliation::Member,
                    role: Role::Participant,
                }),
                ..ArchivedRichMessage::default()
            }),
            ..ArchivedMessage::for_test(
                "room@conference.example.com/alice"
                    .parse()
                    .expect("occupant JID"),
                "room@conference.example.com".parse().expect("room JID"),
            )
        }
    }

    let storage = InMemoryMamStorage::new();
    let room = bare("room@conference.example.com");
    let first = archived("archive-first", "alice@example.com/session-a", 7);
    let retry = archived("archive-retry", "alice@example.com/session-b", 8);

    assert_eq!(
        storage.store_message(&room, &first).await.expect("store"),
        StoreOutcome::Stored("archive-first".to_string())
    );
    assert_eq!(
        storage.store_message(&room, &retry).await.expect("retry"),
        StoreOutcome::Deduplicated("archive-first".to_string())
    );
    assert_eq!(
        storage.count_messages(&room).await.expect("count"),
        1,
        "the retry must not create a second MAM row"
    );
}

/// XEP-0359 §3: `<stanza-id/>` MUST carry the `by` attribute (the
/// JID that stamped it). Verify the helper enforces this so the
/// typed `StanzaIdRef` cannot be lossy at the wire boundary.
#[test]
fn xep0359_stanza_id_element_carries_by_attribute() {
    let alice = Jid::from(bare("alice@example.com"));
    let element = build_stanza_id_element("opaque-mam-id", &alice);
    assert_eq!(element.name(), "stanza-id");
    assert_eq!(element.ns(), NS_SID);
    assert_eq!(element.attr("id"), Some("opaque-mam-id"));
    assert_eq!(element.attr("by"), Some("alice@example.com"));
}

/// Locked Q10c: an Archived `pending_delivery` flush preserves the
/// MAM-stamped `<stanza-id/>` so the recipient's later MAM catch-up
/// sees the same id and dedupes against the flush.
#[test]
fn xep0359_archived_flush_preserves_stanza_id_for_dedupe() {
    let recipient = bare("alice@example.com");
    let stanza_id = "mam-stable-id-001";
    // Build the archived form: a message with a recipient-stamped
    // <stanza-id/>, the way MAM persists it.
    let mut archived = dm("bob@elsewhere/x", "alice@example.com", "missed");
    archived.payloads.push(build_stanza_id_element(
        stanza_id,
        &Jid::from(recipient.clone()),
    ));
    let payload = MaterializedPayload::Archived(Box::new(archived));
    let replayed = build_replay_stanza(
        payload,
        "example.com",
        Utc::now(),
        ReplayReason::OfflineStorage,
    );
    let stanza_id_el = replayed
        .payloads
        .iter()
        .find(|p| p.name() == "stanza-id" && p.ns() == NS_SID)
        .expect("flush replay carries the stanza-id");
    assert_eq!(stanza_id_el.attr("id"), Some(stanza_id));
    assert_eq!(stanza_id_el.attr("by"), Some("alice@example.com"));
}

/// Locked Q5c: Transient (`<no-permanent-store/>`) flushes OMIT
/// `<stanza-id/>` because no MAM row exists to dedupe against — a
/// stamped id would mislead the client into thinking it can fetch
/// the message from the archive.
#[test]
fn xep0359_transient_flush_omits_stanza_id() {
    let recipient = bare("alice@example.com");
    let row = PendingRow {
        id: PendingRowId::fresh(),
        recipient: recipient.clone(),
        original_receipt_at: Utc::now(),
        payload: PendingPayload::Transient(Box::new(dm(
            "bob@elsewhere/x",
            "alice@example.com",
            "ephemeral",
        ))),
        flushed_in_session: None,
        outbound_sequence: None,
    };
    let payload = MaterializedPayload::from_transient(&row).expect("transient");
    let replayed = build_replay_stanza(
        payload,
        "example.com",
        row.original_receipt_at,
        ReplayReason::OfflineStorage,
    );
    let stanza_id_el = replayed
        .payloads
        .iter()
        .find(|p| p.name() == "stanza-id" && p.ns() == NS_SID);
    assert!(
        stanza_id_el.is_none(),
        "Transient flush MUST NOT add <stanza-id/> (locked Q5c)"
    );
}

/// XEP-0359 §3: a message MAY carry multiple `<stanza-id/>` elements
/// from different `by` entities (server, recipient, MUC). The flush
/// path preserves all of them — it never strips or replaces.
#[test]
fn xep0359_archived_flush_preserves_multiple_stanza_ids() {
    let recipient = bare("alice@example.com");
    let server = Jid::from(bare("example.com"));
    let mut archived = dm("bob@elsewhere/x", "alice@example.com", "twin-stamps");
    archived.payloads.push(build_stanza_id_element(
        "recipient-mam-id",
        &Jid::from(recipient.clone()),
    ));
    archived
        .payloads
        .push(build_stanza_id_element("server-stamp-id", &server));
    let payload = MaterializedPayload::Archived(Box::new(archived));
    let replayed = build_replay_stanza(
        payload,
        "example.com",
        Utc::now(),
        ReplayReason::OfflineStorage,
    );
    let ids: Vec<_> = replayed
        .payloads
        .iter()
        .filter(|p| p.name() == "stanza-id" && p.ns() == NS_SID)
        .collect();
    assert_eq!(ids.len(), 2, "both <stanza-id/> stamps preserved");
    let by_attrs: std::collections::HashSet<_> =
        ids.iter().filter_map(|el| el.attr("by")).collect();
    assert!(by_attrs.contains("alice@example.com"));
    assert!(by_attrs.contains("example.com"));
}

/// Sanity: the canonical `xep0359::StanzaId` (the workspace's
/// consolidated stanza-id type after issue #329) round-trips
/// through `build_stanza_id_element` losslessly so any code path
/// that reconstructs a stamp from the typed reference produces
/// wire-identical output.
#[test]
fn xep0359_typed_stanza_id_round_trips_through_element() {
    let recipient = bare("alice@example.com");
    let typed = StanzaId::new("mam-id-typed", Jid::from(recipient.clone()));
    let element = build_stanza_id_element(typed.as_str(), &typed.by);
    assert_eq!(element.attr("id"), Some("mam-id-typed"));
    assert_eq!(element.attr("by"), Some("alice@example.com"));
}
