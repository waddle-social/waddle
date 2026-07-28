//! XEP-0203: fallback retry delay stamps.
//!
//! A failed XEP-0198 resume retries countable client stanzas on a fresh
//! stream. XEP-0203 records their original delivery instant for both message
//! and presence stanzas; IQ requests are never replayed as delayed delivery.

use chrono::{TimeZone, Utc};
use minidom::Element;
use waddle_xmpp_client::stream_management::SmState;

const NS_CLIENT: &str = "jabber:client";
const NS_DELAY: &str = "urn:xmpp:delay";

fn delay(stamp: Option<&str>) -> Element {
    let mut delay = Element::builder("delay", NS_DELAY);
    if let Some(stamp) = stamp {
        delay = delay.attr(minidom::rxml::xml_ncname!("stamp").to_owned(), stamp);
    }
    delay.build()
}

fn fallback_retry(stanza: Element) -> Element {
    let sent_at = Utc.with_ymd_and_hms(2026, 7, 28, 10, 11, 12).unwrap();
    let mut state = SmState::new();
    state.record_sent_stanza_at(&stanza, sent_at);
    state
        .unhandled_stanzas_for_fallback_retry()
        .into_iter()
        .next()
        .expect("countable stanza has one fallback retry")
}

#[test]
fn xep0203_fallback_retry_replaces_duplicate_and_malformed_delays_for_message_and_presence() {
    for name in ["message", "presence"] {
        let mut stanza = Element::builder(name, NS_CLIENT).build();
        stanza.append_child(delay(Some("2025-01-01T01:02:03+01:00")));
        stanza.append_child(delay(Some("not-a-timestamp")));
        stanza.append_child(delay(None));

        let retry = fallback_retry(stanza);
        let delays = retry
            .children()
            .filter(|child| child.name() == "delay" && child.ns() == NS_DELAY)
            .collect::<Vec<_>>();

        assert_eq!(delays.len(), 1, "{name} has exactly one XEP-0203 delay");
        assert_eq!(
            delays[0].attr("stamp"),
            Some("2025-01-01T00:02:03.000Z"),
            "{name} keeps one normalized UTC original-delivery delay",
        );

        let mut malformed = Element::builder(name, NS_CLIENT).build();
        malformed.append_child(delay(Some("not-a-timestamp")));
        malformed.append_child(delay(None));
        let retry = fallback_retry(malformed);
        let delays = retry
            .children()
            .filter(|child| child.name() == "delay" && child.ns() == NS_DELAY)
            .collect::<Vec<_>>();
        assert_eq!(delays.len(), 1, "{name} replaces malformed delays");
        assert_eq!(delays[0].attr("stamp"), Some("2026-07-28T10:11:12.000Z"));
    }
}

#[test]
fn xep0203_fallback_retry_never_adds_or_normalizes_delay_on_iq() {
    let mut iq = Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "request-1")
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
        .build();
    iq.append_child(Element::builder("query", "jabber:iq:version").build());

    assert_eq!(
        fallback_retry(iq.clone()),
        iq,
        "IQ is not a delayed fallback delivery",
    );
}
