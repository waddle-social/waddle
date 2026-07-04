//! XEP-0198 + XEP-0203 — delay stamping on `<resumed/>` replay
//! (issue #1178).
//!
//! When a stream resumes, unacked stanzas replayed from the queue MUST
//! carry a `<delay xmlns='urn:xmpp:delay'/>` whose `stamp` is the
//! server-side receipt time of the original stanza — not the replay
//! time — so clients sort them at their true timeline position instead
//! of the drain time. XEP-0198's Acks section requires this delay for
//! failed-session redelivery ("add a delay element with the original
//! (failed) delivery timestamp, as per XEP-0203"); we apply the same
//! stamping to the `<resumed/>` replay by analogy.

use chrono::{TimeZone, Utc};
use minidom::Element;
use std::str::FromStr;
use waddle_xmpp::stream_management::{stamp_replay_delay, StreamManagementState};

const NS_DELAY: &str = "urn:xmpp:delay";

fn fixed_receipt() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 7, 1, 9, 15, 30).unwrap()
}

fn delay_children(xml: &str) -> Vec<Element> {
    let element = Element::from_str(xml).expect("stamped replay stays well-formed XML");
    element
        .children()
        .filter(|child| child.name() == "delay" && child.ns() == NS_DELAY)
        .cloned()
        .collect()
}

#[test]
fn replayed_message_carries_delay_with_original_receipt_stamp() {
    let original =
        "<message xmlns='jabber:client' from='bob@example.com/x' to='alice@example.com' \
         type='chat' id='m1'><body>hi</body></message>";

    let stamped = stamp_replay_delay(original, "example.com", fixed_receipt());

    let delays = delay_children(&stamped);
    assert_eq!(delays.len(), 1, "exactly one delay element on replay");
    assert_eq!(delays[0].attr("from"), Some("example.com"));
    // XEP-0082 §3.2 canonical UTC form with the `Z` suffix, matching
    // the original receipt time — never the replay time.
    assert_eq!(delays[0].attr("stamp"), Some("2026-07-01T09:15:30Z"));
    assert!(
        delays[0].text().is_empty(),
        "SM resume replay carries no offline-storage reason text"
    );
}

#[test]
fn replayed_iq_is_not_stamped() {
    // XEP-0203 §3 defines delay annotations for message and presence
    // stanzas; a replayed <iq/> must go out byte-identical.
    let original = "<iq xmlns='jabber:client' from='example.com' to='alice@example.com/r' \
                    type='result' id='q1'/>";

    let stamped = stamp_replay_delay(original, "example.com", fixed_receipt());

    assert!(delay_children(&stamped).is_empty(), "iq gains no delay");
}

#[test]
fn replay_preserves_existing_self_stamped_delay_and_upstream_delay() {
    // A queued stanza can already carry a delay this server itself
    // stamped on an earlier path — the offline flush, or a Q6
    // SM-expiry redelivery whose queue receipt time is the REDELIVERY
    // time, not the original send time. In that case the existing
    // self-stamp is the only accurate record of the original time and
    // MUST be kept as-is (no second self-stamp, no overwrite with the
    // later queue receipt). Delays from other entities are the
    // recipient's delivery history and stay untouched.
    let original = "<message xmlns='jabber:client' from='bob@example.com/x' \
         to='alice@example.com' type='chat' id='m2'><body>hi</body>\
         <delay xmlns='urn:xmpp:delay' from='example.com' \
         stamp='2026-06-30T08:00:00Z'>Offline Storage</delay>\
         <delay xmlns='urn:xmpp:delay' from='upstream.example.org' \
         stamp='2026-06-30T07:59:00Z'>Forwarded by upstream</delay></message>";

    let stamped = stamp_replay_delay(original, "example.com", fixed_receipt());

    let delays = delay_children(&stamped);
    let self_stamped: Vec<_> = delays
        .iter()
        .filter(|delay| delay.attr("from") == Some("example.com"))
        .collect();
    assert_eq!(self_stamped.len(), 1, "exactly one self-stamped delay");
    assert_eq!(
        self_stamped[0].attr("stamp"),
        Some("2026-06-30T08:00:00Z"),
        "the earlier accurate self-stamp is kept, not overwritten with \
         the (possibly later) queue receipt time"
    );
    assert_eq!(
        self_stamped[0].text(),
        "Offline Storage",
        "the original delay's reason text survives replay"
    );
    assert!(
        delays
            .iter()
            .any(|delay| delay.attr("from") == Some("upstream.example.org")),
        "upstream delay metadata preserved"
    );
}

#[test]
fn first_send_is_recorded_unstamped_and_gains_delay_only_at_replay() {
    // The unacked queue must hold the stanza byte-for-byte as it was
    // first written to the wire — the XEP-0203 stamp is applied only
    // when the stanza is replayed after <resumed/>. Stamping at record
    // time would leak a delay element onto the FIRST delivery, telling
    // the recipient a live message was delayed.
    let mut state = StreamManagementState::new();
    state.enable("stream-first-send".to_string(), true, Some(300));

    let wire = "<message xmlns='jabber:client' from='bob@example.com/x' to='alice@example.com' \
         type='chat' id='live-1'><body>live</body></message>";
    let _ = state.record_outbound_with_receipt_at(wire.to_string(), fixed_receipt());

    let replay = state.get_stanzas_to_resend(0);
    assert_eq!(replay.len(), 1);
    assert_eq!(
        replay[0].stanza_xml, wire,
        "queue preserves the first-send bytes — no delay on first delivery"
    );
    assert!(delay_children(&replay[0].stanza_xml).is_empty());

    let stamped = stamp_replay_delay(
        &replay[0].stanza_xml,
        "example.com",
        replay[0].original_receipt_at,
    );
    assert_eq!(delay_children(&stamped).len(), 1, "delay appears at replay");
}

#[test]
fn resend_set_carries_each_stanzas_original_receipt_time() {
    // The queue records `original_receipt_at` per stanza; the resume
    // replay set must surface it alongside the XML so the caller can
    // stamp the XEP-0203 delay with the true send time — not the
    // resume time.
    let mut state = StreamManagementState::new();
    state.enable("stream-1178".to_string(), true, Some(300));

    let first_receipt = Utc.with_ymd_and_hms(2026, 7, 1, 9, 0, 0).unwrap();
    let second_receipt = Utc.with_ymd_and_hms(2026, 7, 1, 9, 5, 0).unwrap();
    let _ = state.record_outbound_with_receipt_at(
        "<message xmlns='jabber:client' id='a'/>".to_string(),
        first_receipt,
    );
    let _ = state.record_outbound_with_receipt_at(
        "<message xmlns='jabber:client' id='b'/>".to_string(),
        second_receipt,
    );

    let replay = state.get_stanzas_to_resend(0);
    assert_eq!(replay.len(), 2);
    assert_eq!(replay[0].original_receipt_at, first_receipt);
    assert!(replay[0].stanza_xml.contains("id='a'"));
    assert_eq!(replay[1].original_receipt_at, second_receipt);
    assert!(replay[1].stanza_xml.contains("id='b'"));
}
