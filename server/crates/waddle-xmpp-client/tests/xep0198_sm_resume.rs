//! XEP-0198: dedicated client stream-management resumption suite.
//!
//! Durable browser and FFI state reaches this client boundary as parsed
//! `Element`s. These tests pin the XEP-0198 rule that only countable
//! `jabber:client` stanzas may be restored and replayed.

use chrono::{TimeZone, Utc};
use minidom::Element;
use waddle_xmpp_client::{stream_management::SmState, SmResumeState, UnhandledOutboundEntry};

fn persisted(xml: &str, second: u32) -> UnhandledOutboundEntry {
    UnhandledOutboundEntry::try_new(
        xml.parse::<Element>().expect("test stanza XML parses"),
        Utc.with_ymd_and_hms(2026, 7, 27, 12, 0, second)
            .single()
            .expect("test timestamp is valid"),
    )
    .expect("countable jabber:client stanza")
}

#[test]
fn xep0198_restored_countable_stanzas_replay_in_order_without_losing_extensions() {
    let message = "<message xmlns='jabber:client' id='m-1'><body>one</body><origin-id xmlns='urn:xmpp:sid:0' id='origin-1'/><opaque xmlns='urn:example:opaque' z='1'/></message>";
    let presence = "<presence xmlns='jabber:client'><show>away</show><x xmlns='vcard-temp:x:update'><photo>digest</photo></x></presence>";
    let iq =
        "<iq xmlns='jabber:client' id='iq-1' type='get'><query xmlns='jabber:iq:version'/></iq>";
    let resume = SmResumeState::from_unhandled_outbound_entries(
        "previous-stream",
        4,
        7,
        [
            persisted(message, 0),
            persisted(presence, 1),
            persisted(iq, 2),
        ],
    )
    .expect("XEP-0198 state is valid");

    let mut sm = SmState::from_resume_state(&resume);
    let replay = sm.mark_unhandled_for_replay();

    assert_eq!(
        replay.iter().map(Element::name).collect::<Vec<_>>(),
        vec!["message", "presence", "iq"],
        "XEP-0198 §5 replay preserves the unacked order",
    );
    assert_eq!(
        replay,
        vec![
            message.parse::<Element>().expect("message parses"),
            presence.parse::<Element>().expect("presence parses"),
            iq.parse::<Element>().expect("IQ parses"),
        ],
        "opaque extension payloads and their child order survive replay",
    );
}

#[test]
fn xep0198_rejects_stream_controls_and_non_client_roots_from_durable_replay() {
    for xml in [
        "<r xmlns='urn:xmpp:sm:3'/>",
        "<a xmlns='urn:xmpp:sm:3' h='1'/>",
        "<enable xmlns='urn:xmpp:sm:3'/>",
        "<resumed xmlns='urn:xmpp:sm:3' h='1' previd='old'/>",
        "<foo xmlns='jabber:client'/>",
        "<message xmlns='urn:example:not-client'/>",
    ] {
        let element = xml.parse::<Element>().expect("test XML parses");
        assert!(
            UnhandledOutboundEntry::try_new(element, Utc::now()).is_err(),
            "{xml} is not a countable XEP-0198 replay stanza",
        );
    }
}
