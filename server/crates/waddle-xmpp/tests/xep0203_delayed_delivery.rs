//! XEP-0203: Delayed Delivery — dedicated suite.
//!
//! XEP-0203 §4.1: a server adding a `<delay/>` element MUST set
//!   - `from` to the server's JID, and
//!   - `stamp` to the original receipt time of the stanza (in
//!     XEP-0082 §3.2 BNF — UTC, literal `Z` form).
//!
//! In waddle, the delay element is appended on two paths:
//!
//!   1. `pending_delivery` flush replay (XEP-0160 §3 step 5):
//!      `build_replay_stanza` adds `<delay from='server' stamp='T0'/>`
//!      where T0 is the row's `original_receipt_at`.
//!   2. SM-expiry promotion (XEP-0198 §5 line 364): the row Q6 inserts
//!      into `pending_delivery` carries the source `original_receipt_at`,
//!      which then flows through path 1 on the next flush.
//!
//! Issue #209 PR #361 plumbed `original_receipt_at` end-to-end so
//! both paths advertise the real failed-delivery time, not a wall-
//! clock at flush/expiry time.
//!
//! Citations refer to `xeps/xep-0203.xml` unless otherwise noted.

use chrono::{TimeZone, Utc};
use jid::{BareJid, Jid};
use waddle_xmpp::pending_delivery::flush::{
    build_replay_stanza, MaterializedPayload, ReplayReason,
};
use waddle_xmpp::pending_delivery::{PendingPayload, PendingRow, PendingRowId};
use waddle_xmpp::xep::NS_DELAY;
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

fn transient_row(recipient: &str, body: &str, t: chrono::DateTime<Utc>) -> PendingRow {
    PendingRow {
        id: PendingRowId::fresh(),
        recipient: bare(recipient),
        original_receipt_at: t,
        payload: PendingPayload::Transient(Box::new(dm("bob@elsewhere/x", recipient, body))),
        flushed_in_session: None,
        outbound_sequence: None,
    }
}

/// XEP-0203 §4.1: `from` attribute on the appended `<delay/>` is the
/// server JID (the entity that injected the delay), NOT the original
/// sender or the recipient.
#[test]
fn xep0203_delay_from_attribute_is_server_jid() {
    let t = Utc.with_ymd_and_hms(2026, 5, 1, 12, 30, 0).unwrap();
    let row = transient_row("alice@example.com", "hi", t);
    let payload = MaterializedPayload::from_transient(&row).expect("transient");
    let replayed = build_replay_stanza(
        payload,
        "example.com",
        row.original_receipt_at,
        ReplayReason::OfflineStorage,
    );
    let delay = replayed
        .payloads
        .iter()
        .find(|p| p.name() == "delay" && p.ns() == NS_DELAY)
        .expect("delay element appended");
    assert_eq!(delay.attr("from"), Some("example.com"));
}

/// XEP-0203 §4.1: `stamp` is the ORIGINAL receipt time, NOT the flush
/// or build time. XEP-0082 §3.2 BNF: literal `Z` for UTC.
#[test]
fn xep0203_delay_stamp_is_original_receipt_time_in_xep0082_z_form() {
    let t = Utc.with_ymd_and_hms(2026, 4, 17, 9, 15, 30).unwrap();
    let row = transient_row("alice@example.com", "hi", t);
    let payload = MaterializedPayload::from_transient(&row).expect("transient");
    let replayed = build_replay_stanza(
        payload,
        "example.com",
        row.original_receipt_at,
        ReplayReason::OfflineStorage,
    );
    let delay = replayed
        .payloads
        .iter()
        .find(|p| p.name() == "delay" && p.ns() == NS_DELAY)
        .expect("delay element appended");
    assert_eq!(delay.attr("stamp"), Some("2026-04-17T09:15:30Z"));
}

/// Issue #209 PR #361: the per-stanza `original_receipt_at` MUST be
/// preserved end-to-end through the SM-promote path. The flush
/// build at the bottom of that chain stamps the SAME receipt time
/// regardless of how long the row sat in pending_delivery before
/// being flushed.
///
/// Pin this by varying the `original_receipt_at` argument across
/// two builds (a row is just one possible source of the value) and
/// verifying the wire stamp matches whatever the caller passed.
/// `build_replay_stanza` does NOT consult any wall-clock of its
/// own (Copilot review on PR #362).
#[test]
fn xep0203_delay_stamp_uses_caller_supplied_receipt_time_verbatim() {
    let row = transient_row(
        "alice@example.com",
        "hi",
        // Row's stored `original_receipt_at` — irrelevant to this
        // test, since we pass a different value into the builder
        // below to prove the builder uses its argument verbatim.
        Utc::now(),
    );

    // Build #1: caller supplies a year-old timestamp.
    let t_old =
        chrono::DateTime::<Utc>::from_timestamp_millis(1_700_000_000_000).expect("valid timestamp");
    let payload = MaterializedPayload::from_transient(&row).expect("transient");
    let replayed = build_replay_stanza(payload, "example.com", t_old, ReplayReason::OfflineStorage);
    let delay = replayed
        .payloads
        .iter()
        .find(|p| p.name() == "delay" && p.ns() == NS_DELAY)
        .expect("delay element appended");
    assert_eq!(delay.attr("stamp"), Some("2023-11-14T22:13:20Z"));

    // Build #2: caller supplies a different timestamp (would-be
    // "flush time"). The wire stamp follows the caller's value, NOT
    // the original row's `original_receipt_at` and NOT any wall-
    // clock the builder might consult.
    let t_recent = Utc.with_ymd_and_hms(2026, 1, 15, 8, 0, 0).unwrap();
    let payload2 = MaterializedPayload::from_transient(&row).expect("transient");
    let replayed2 = build_replay_stanza(
        payload2,
        "example.com",
        t_recent,
        ReplayReason::OfflineStorage,
    );
    let delay2 = replayed2
        .payloads
        .iter()
        .find(|p| p.name() == "delay" && p.ns() == NS_DELAY)
        .expect("delay element appended");
    assert_eq!(delay2.attr("stamp"), Some("2026-01-15T08:00:00Z"));
}

/// Sanity check: only a single `<delay/>` is added by the flush
/// builder (no double-stamp on multiple flush attempts of the same
/// row, which would happen if delete-on-push were lost).
#[test]
fn xep0203_flush_appends_single_delay_element() {
    let t = Utc.with_ymd_and_hms(2026, 5, 1, 12, 30, 0).unwrap();
    let row = transient_row("alice@example.com", "hi", t);
    let payload = MaterializedPayload::from_transient(&row).expect("transient");
    let replayed = build_replay_stanza(
        payload,
        "example.com",
        row.original_receipt_at,
        ReplayReason::OfflineStorage,
    );
    let delays: Vec<_> = replayed
        .payloads
        .iter()
        .filter(|p| p.name() == "delay" && p.ns() == NS_DELAY)
        .collect();
    assert_eq!(delays.len(), 1, "exactly one <delay/> element");
}

/// Locked Q5 + XEP-0203 §4.1: the delay element is appended to the
/// stanza payload list, not into the message body. This preserves
/// XEP-0203 §6 ("Receiving Entities ... SHOULD NOT include such
/// elements when they are responsible for serializing the messages
/// to/from the wire").
#[test]
fn xep0203_delay_lives_in_stanza_payloads_not_body() {
    let t = Utc.with_ymd_and_hms(2026, 5, 1, 12, 30, 0).unwrap();
    let row = transient_row("alice@example.com", "hi-with-body", t);
    let payload = MaterializedPayload::from_transient(&row).expect("transient");
    let replayed = build_replay_stanza(
        payload,
        "example.com",
        row.original_receipt_at,
        ReplayReason::OfflineStorage,
    );
    // Body is unchanged; delay is in payloads.
    assert_eq!(
        replayed.bodies.get("").map(|b| b.as_str()),
        Some("hi-with-body")
    );
    let delay = replayed
        .payloads
        .iter()
        .find(|p| p.name() == "delay" && p.ns() == NS_DELAY);
    assert!(delay.is_some(), "delay element appended to payloads");
}

// SM-promote → pending_delivery → flush path delay stamping is
// covered by the classifier-level Q6 promotion tests in
// `server/crates/waddle-server/src/sm_promotion.rs::tests` and the
// e2e test
// `pending_delivery::tests::xep0160_promoted_stanzas_carry_original_receipt_time_in_delay`.
// Those live in the server crate where `flush_for_resource` and the
// promotion path are reachable.

/// XEP-0203 conformance of the ingress semantic-digest boundary (#1650),
/// in this XEP's dedicated suite per the repo test rule: exactly the
/// `{urn:xmpp:delay}delay` expanded name is excluded from digest material
/// (retry-added delay must be digest-neutral), while OTHER element names
/// in the delay namespace remain digest-sensitive extensions.
mod ingress_digest_boundary {
    use minidom::{rxml::xml_ncname, Element};
    use waddle_xmpp::ingress::digest::v1;
    use waddle_xmpp::ingress::{DigestContext, DigestInput, NormalizedTarget};
    use xmpp_parsers::message::Message;

    const DELAY_NS: &str = "urn:xmpp:delay";

    fn context() -> DigestContext {
        DigestContext {
            target: NormalizedTarget::Absent,
            server_authorities: Vec::new(),
            stanza_lang: None,
        }
    }

    fn digest_of(message: &Message) -> waddle_xmpp::ingress::SemanticDigest {
        v1::digest(&DigestInput::from_parsed(message, &context()).expect("valid input"))
    }

    #[test]
    fn delay_element_is_digest_neutral() {
        let bare = Message::normal(None);
        let delayed =
            Message::normal(None).with_payloads(vec![Element::builder("delay", DELAY_NS)
                .attr(xml_ncname!("from").to_owned(), "server.example")
                .attr(xml_ncname!("stamp").to_owned(), "2026-08-11T00:00:00Z")
                .build()]);
        assert_eq!(
            digest_of(&bare),
            digest_of(&delayed),
            "retry-added XEP-0203 delay must never change semantic identity"
        );
    }

    #[test]
    fn other_names_in_the_delay_namespace_stay_digest_sensitive() {
        let bare = Message::normal(None);
        let custom =
            Message::normal(None)
                .with_payloads(vec![Element::builder("not-delay", DELAY_NS).build()]);
        assert_ne!(
            digest_of(&bare),
            digest_of(&custom),
            "only the exact {{urn:xmpp:delay}}delay expanded name is excluded"
        );
    }
}
