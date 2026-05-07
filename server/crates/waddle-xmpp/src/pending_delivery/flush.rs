//! XEP-0160 offline-message flush wire-shape builder (issue #209,
//! locked Q5 / Q6c).
//!
//! When a recipient comes online and the server has rows in
//! `pending_delivery` for them, each row is replayed as a `<message>`
//! stanza shaped per:
//!
//! - **XEP-0160 §3** — preserve sender's `from` and original `to`
//!   (locked Q5a). Append a `<delay/>` per XEP-0203.
//! - **XEP-0203 §4.1** — `<delay xmlns='urn:xmpp:delay' from='SERVER'
//!   stamp='ORIGINAL_RECEIPT_ISO8601'>Offline Storage</delay>`. The
//!   stamp is the original (failed) receipt time, NOT the flush time
//!   (XEP-0198 §5 line 364 + locked Q6c).
//! - **XEP-0359 §3.1** — for `Archived` rows the server-stamped
//!   `<stanza-id by='RECIPIENT_BARE'/>` is preserved (it was added at
//!   archive time and lives in MAM); for `Transient` rows no
//!   stanza-id is added (locked Q5c — strict §3.1 conformance).
//! - **Locked Q5e** — sender-set extension elements (hints, origin-id,
//!   etc.) preserved unchanged.
//!
//! This module exposes a pure, testable wire-shape builder. The actual
//! flush trigger (presence-driven `claim_for_session` + push to the
//! recovering session via the connection registry + SM unacked
//! lifecycle) is wired in `waddle-server` against this builder.

use crate::pending_delivery::{PendingPayload, PendingRow};
use crate::xep::xep0203::{build_delay_element, DelayInfo};
use crate::xep::NS_DELAY;
use chrono::{DateTime, Utc};
use minidom::Element;
use xmpp_parsers::message::Message;

/// Resolved payload for a flush row.
///
/// `Archived(_)` rows reference a MAM stanza-id; the caller resolves
/// the row by reading the archived [`Message`] from
/// [`crate::mam::storage::MamStorage`] and passes the resolved
/// `Message` as `MaterializedPayload::Archived`. `Transient` rows
/// carry the [`Message`] inline directly.
///
/// Splitting payload-resolution out of the wire-shape builder keeps
/// the builder pure (no I/O, no async).
#[derive(Debug)]
pub enum MaterializedPayload {
    /// Resolved Archived payload — caller has read the MAM row.
    Archived(Box<Message>),
    /// Inline Transient payload — taken straight from the row.
    Transient(Box<Message>),
}

impl MaterializedPayload {
    /// Materialize a [`PendingPayload::Transient`] row directly. For
    /// `Archived` rows, callers must read the MAM stanza first.
    pub fn from_transient(row: &PendingRow) -> Option<Self> {
        match &row.payload {
            PendingPayload::Transient(m) => Some(Self::Transient(m.clone())),
            PendingPayload::Archived(_) => None,
        }
    }

    fn into_message(self) -> Box<Message> {
        match self {
            Self::Archived(m) | Self::Transient(m) => m,
        }
    }
}

/// Reason text to attach to the `<delay/>` element added by
/// [`build_replay_stanza`].
///
/// XEP-0203 §4.2 makes the element text purely informational; the
/// authoritative delivery-history signal is the `from` + `stamp`
/// attribute pair. The reason therefore varies by replay path so
/// clients/operators surfacing the description aren't told the wrong
/// story (issue #209 finding #4).
#[derive(Debug, Clone, Copy)]
pub enum ReplayReason {
    /// Replay from XEP-0160 `pending_delivery` storage (offline hold).
    OfflineStorage,
    /// XEP-0198 SM-expiry redelivery to an alternate live resource.
    /// The stanza was never persisted offline; only the timestamp is
    /// late, so no human-readable reason is attached.
    SmRedelivery,
}

impl ReplayReason {
    fn description(self) -> Option<&'static str> {
        match self {
            Self::OfflineStorage => Some("Offline Storage"),
            Self::SmRedelivery => None,
        }
    }
}

/// Build the `<message>` stanza to send to a recovering resource
/// (locked Q5).
///
/// `server_domain` is the JID-form value used as the `from` attribute
/// on the appended `<delay/>` element (XEP-0203 §4.1: "the server
/// itself is the entity that adds the delay information").
///
/// `original_receipt_at` is the server-side receipt time of the
/// original stanza (before it was queued for offline delivery). This
/// is what XEP-0203 §4.2 + XEP-0198 §5 line 364 require on the
/// `<delay/>` stamp.
///
/// `reason` selects the optional human-readable description carried
/// by the `<delay/>` element. SM-expiry redelivery uses
/// [`ReplayReason::SmRedelivery`] to avoid mislabeling the path as
/// offline-storage replay (issue #209 finding #4).
///
/// The function preserves all sender-set children. It does NOT add a
/// `<stanza-id/>` here; for `Archived` rows the stamp lives in the
/// MAM payload already and is preserved by `MaterializedPayload`, and
/// for `Transient` rows we deliberately omit it (locked Q5c).
pub fn build_replay_stanza(
    payload: MaterializedPayload,
    server_domain: &str,
    original_receipt_at: DateTime<Utc>,
    reason: ReplayReason,
) -> Message {
    let mut message = *payload.into_message();
    // Strip ONLY any pre-existing `<delay/>` whose `from` attribute
    // matches our own server domain — that would be a stamp this
    // server itself added on a prior path (round-trip via MAM, replay
    // through a transient pass, etc.) and re-emitting it would result
    // in two `<delay from='our-domain'/>` siblings.
    //
    // Legitimate upstream `<delay/>` elements (e.g. ones a different
    // server in a federated chain stamped, or a forwarded-message
    // delay from a stored stanza) are preserved unchanged so the
    // recipient sees the full delivery history per XEP-0203 §5
    // ("multiple delay elements MAY appear").
    message.payloads.retain(|payload| {
        !(payload.name() == "delay"
            && payload.ns() == NS_DELAY
            && payload.attr("from") == Some(server_domain))
    });
    message.payloads.push(build_offline_delay(
        server_domain,
        original_receipt_at,
        reason.description(),
    ));
    message
}

/// Build a `<delay/>` element shaped per XEP-0203 §4 + the XEP-0160 §3
/// example.
fn build_offline_delay(
    server_domain: &str,
    stamp: DateTime<Utc>,
    description: Option<&str>,
) -> Element {
    build_delay_element(&DelayInfo {
        from: Some(server_domain.to_string()),
        stamp,
        reason: description.map(str::to_string),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pending_delivery::{PendingPayload, PendingRow, SmSessionId};
    use chrono::TimeZone;
    use jid::{BareJid, Jid};
    use waddle_xmpp_core::xep0359::StanzaId;
    use xmpp_parsers::message::{Body, MessageType};

    fn bare(s: &str) -> BareJid {
        s.parse().expect("bare jid")
    }

    fn original_message(from: &str, to: &str, body: &str) -> Message {
        let mut m = Message::new(Some(to.parse::<Jid>().expect("jid")));
        m.from = Some(from.parse::<Jid>().expect("jid"));
        m.type_ = MessageType::Chat;
        m.bodies.insert(String::new(), Body(body.to_string()));
        m
    }

    fn fixed_receipt() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 5, 1, 12, 30, 0).unwrap()
    }

    fn archived_row(recipient: &str) -> PendingRow {
        let archive_jid: jid::Jid = bare(recipient).into();
        PendingRow {
            id: crate::pending_delivery::PendingRowId::fresh(),
            recipient: bare(recipient),
            original_receipt_at: fixed_receipt(),
            payload: PendingPayload::Archived(StanzaId::new("mam-id", archive_jid)),
            flushed_in_session: Some(SmSessionId::new("s1")),
            outbound_sequence: None,
        }
    }

    fn transient_row(recipient: &str, body: &str) -> PendingRow {
        PendingRow {
            id: crate::pending_delivery::PendingRowId::fresh(),
            recipient: bare(recipient),
            original_receipt_at: fixed_receipt(),
            payload: PendingPayload::Transient(Box::new(original_message(
                "bob@elsewhere/x",
                recipient,
                body,
            ))),
            flushed_in_session: None,
            outbound_sequence: None,
        }
    }

    fn delay_element(message: &Message) -> Option<&Element> {
        message
            .payloads
            .iter()
            .find(|payload| payload.name() == "delay" && payload.ns() == NS_DELAY)
    }

    #[test]
    fn replay_stanza_appends_delay_with_server_domain_and_original_receipt() {
        let row = transient_row("alice@example.com", "off the record");
        let payload =
            MaterializedPayload::from_transient(&row).expect("transient row materializes");
        let replayed = build_replay_stanza(
            payload,
            "example.com",
            row.original_receipt_at,
            ReplayReason::OfflineStorage,
        );
        let delay = delay_element(&replayed).expect("delay element appended");
        assert_eq!(delay.attr("from"), Some("example.com"));
        // ISO-8601 stamp matches the original receipt time, not "now".
        // XEP-0082 §3.2 BNF requires the `Z` suffix for UTC datetimes;
        // chrono renders `+00:00` by default, so we force the `Z` form
        // in build_delay_element.
        assert_eq!(
            delay.attr("stamp").map(str::to_string),
            Some("2026-05-01T12:30:00Z".to_string())
        );
        assert_eq!(delay.text(), "Offline Storage");
    }

    #[test]
    fn replay_stanza_sm_redelivery_omits_offline_storage_reason_text() {
        // SM-expiry redelivery never persisted the stanza offline; the
        // load-bearing signal is the typed `from`+`stamp` pair, not the
        // optional human-readable description (XEP-0203 §4.2). Issue
        // #209 finding #4 — clients/operators surfacing the description
        // were told the wrong story when SM redelivery reused the
        // offline-flush builder verbatim.
        let row = transient_row("alice@example.com", "hi");
        let payload = MaterializedPayload::from_transient(&row).expect("transient");
        let replayed = build_replay_stanza(
            payload,
            "example.com",
            row.original_receipt_at,
            ReplayReason::SmRedelivery,
        );
        let delay = delay_element(&replayed).expect("delay element appended");
        assert_eq!(delay.attr("from"), Some("example.com"));
        assert_eq!(
            delay.attr("stamp").map(str::to_string),
            Some("2026-05-01T12:30:00Z".to_string())
        );
        assert!(
            delay.text().is_empty(),
            "SmRedelivery must not emit the 'Offline Storage' description"
        );
    }

    #[test]
    fn replay_stanza_preserves_original_to_and_from() {
        let row = transient_row("alice@example.com", "hi");
        let payload = MaterializedPayload::from_transient(&row).unwrap();
        let replayed = build_replay_stanza(
            payload,
            "example.com",
            row.original_receipt_at,
            ReplayReason::OfflineStorage,
        );
        let from_jid = replayed.from.expect("from preserved");
        assert_eq!(from_jid.to_string(), "bob@elsewhere/x");
        let to_jid = replayed.to.expect("to preserved");
        // Original `to` was the bare JID; preserved per locked Q5a.
        assert_eq!(to_jid.to_string(), "alice@example.com");
    }

    #[test]
    fn replay_stanza_preserves_sender_extensions() {
        // Add a custom sender-set extension element to a Transient row.
        let row = transient_row("alice@example.com", "secret");
        if let PendingPayload::Transient(_) = &row.payload {
            // Already a transient row.
        } else {
            panic!("expected transient");
        }
        // Mutate the inline payload to add an extension.
        let mut row_with_ext = row;
        if let PendingPayload::Transient(msg) = &mut row_with_ext.payload {
            msg.payloads
                .push(Element::builder("custom", "urn:test:custom").build());
        }
        let payload = MaterializedPayload::from_transient(&row_with_ext).unwrap();
        let replayed = build_replay_stanza(
            payload,
            "example.com",
            row_with_ext.original_receipt_at,
            ReplayReason::OfflineStorage,
        );
        let custom_present = replayed
            .payloads
            .iter()
            .any(|payload| payload.ns() == "urn:test:custom" && payload.name() == "custom");
        assert!(custom_present, "sender-set extension preserved on replay");
    }

    #[test]
    fn replay_stanza_strips_only_self_stamped_delay_to_avoid_double_stamping() {
        // Pre-existing delay stamped by THIS server should be replaced
        // by the freshly-built one (avoid two `<delay from='example.com'/>`
        // siblings). Round-trip safety: if a stanza somehow ends up
        // re-flushed, we don't want two of the server's own stamps.
        let mut row = transient_row("alice@example.com", "hi");
        if let PendingPayload::Transient(msg) = &mut row.payload {
            msg.payloads
                .push(build_offline_delay("example.com", fixed_receipt(), None));
        }
        let payload = MaterializedPayload::from_transient(&row).unwrap();
        let replayed = build_replay_stanza(
            payload,
            "example.com",
            row.original_receipt_at,
            ReplayReason::OfflineStorage,
        );
        let self_stamped = replayed
            .payloads
            .iter()
            .filter(|payload| {
                payload.name() == "delay"
                    && payload.ns() == NS_DELAY
                    && payload.attr("from") == Some("example.com")
            })
            .count();
        assert_eq!(
            self_stamped, 1,
            "exactly one self-stamped delay after replay"
        );
    }

    #[test]
    fn replay_stanza_preserves_upstream_delay_metadata() {
        // XEP-0203 §5 allows multiple `<delay/>` elements, e.g. when a
        // stanza traverses multiple servers in a federated chain. Our
        // offline-delay stamping must NOT strip delays stamped by
        // other entities.
        let mut row = transient_row("alice@example.com", "hi");
        if let PendingPayload::Transient(msg) = &mut row.payload {
            msg.payloads.push(build_offline_delay(
                "upstream.example.org",
                fixed_receipt(),
                Some("Forwarded by upstream"),
            ));
        }
        let payload = MaterializedPayload::from_transient(&row).unwrap();
        let replayed = build_replay_stanza(
            payload,
            "example.com",
            row.original_receipt_at,
            ReplayReason::OfflineStorage,
        );
        let upstream_preserved = replayed.payloads.iter().any(|payload| {
            payload.name() == "delay"
                && payload.ns() == NS_DELAY
                && payload.attr("from") == Some("upstream.example.org")
        });
        let self_added = replayed.payloads.iter().any(|payload| {
            payload.name() == "delay"
                && payload.ns() == NS_DELAY
                && payload.attr("from") == Some("example.com")
        });
        assert!(upstream_preserved, "upstream delay metadata preserved");
        assert!(self_added, "server's own offline delay stamped");
    }

    #[test]
    fn from_transient_returns_none_for_archived_rows() {
        let row = archived_row("alice@example.com");
        assert!(MaterializedPayload::from_transient(&row).is_none());
    }
}
