//! XEP-0198: dedicated client stream-management resumption suite.
//!
//! Durable browser and FFI state reaches this client boundary as parsed
//! `Element`s. These tests pin the XEP-0198 rule that only countable
//! `jabber:client` stanzas may be restored and replayed.

use chrono::{TimeZone, Utc};
use minidom::Element;
use waddle_xmpp_client::{
    stream_management::{InvalidSmInboundControl, SmInboundControl, SmState},
    SmResumeState, StreamId, UnhandledOutboundEntry,
};

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
        StreamId::new("previous-stream"),
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

#[test]
fn xep0198_failed_accepts_only_the_schema_stanza_error_group() {
    let failed = Element::builder("failed", "urn:xmpp:sm:3")
        .append(
            Element::builder("service-unavailable", "urn:ietf:params:xml:ns:xmpp-stanzas").build(),
        )
        .build();

    assert_eq!(
        SmState::parse_inbound_control(&failed),
        Ok(SmInboundControl::Failed { h: None }),
        "XEP-0198 permits one recognized stanza condition",
    );

    let application_child = Element::builder("failed", "urn:xmpp:sm:3")
        .append(
            Element::builder("service-unavailable", "urn:ietf:params:xml:ns:xmpp-stanzas").build(),
        )
        .append(Element::builder("retry-after", "urn:waddle:diagnostics").build())
        .build();
    assert_eq!(
        SmState::parse_inbound_control(&application_child),
        Err(InvalidSmInboundControl),
        "XEP-0198 failed does not admit an application-defined child",
    );

    let text = Element::builder("failed", "urn:xmpp:sm:3")
        .append(
            Element::builder("service-unavailable", "urn:ietf:params:xml:ns:xmpp-stanzas").build(),
        )
        .append(
            Element::builder("text", "urn:ietf:params:xml:ns:xmpp-stanzas")
                .attr_ns(
                    minidom::rxml::Namespace::XML,
                    minidom::rxml::xml_ncname!("lang").to_owned(),
                    "en",
                )
                .append("Resume on a new stream")
                .build(),
        )
        .build();
    assert_eq!(
        SmState::parse_inbound_control(&text),
        Err(InvalidSmInboundControl),
        "XEP-0198's failed schema does not include err:text",
    );

    let empty_text = Element::builder("failed", "urn:xmpp:sm:3")
        .append(
            Element::builder("service-unavailable", "urn:ietf:params:xml:ns:xmpp-stanzas").build(),
        )
        .append(Element::builder("text", "urn:ietf:params:xml:ns:xmpp-stanzas").build())
        .build();
    assert_eq!(
        SmState::parse_inbound_control(&empty_text),
        Err(InvalidSmInboundControl),
        "an empty err:text is invalid too",
    );

    let out_of_order = Element::builder("failed", "urn:xmpp:sm:3")
        .append(
            Element::builder("service-unavailable", "urn:ietf:params:xml:ns:xmpp-stanzas").build(),
        )
        .append(Element::builder("retry-after", "urn:waddle:diagnostics").build())
        .append(
            Element::builder("text", "urn:ietf:params:xml:ns:xmpp-stanzas")
                .append("late")
                .build(),
        )
        .build();
    assert_eq!(
        SmState::parse_inbound_control(&out_of_order),
        Err(InvalidSmInboundControl),
        "an application condition after err:text is invalid because err:text is not in the group",
    );
}

mod rejection_support;

#[test]
fn rejection_preserves_sm_ordinals_but_is_excluded_from_fresh_stream_retry() {
    let entries = [
        persisted(
            "<message xmlns='jabber:client' type='chat' id='before' to='bob@example.com'/>",
            0,
        ),
        persisted(
            "<message xmlns='jabber:client' type='chat' id='rejected' to='chat@example.com'/>",
            1,
        ),
        persisted(
            "<message xmlns='jabber:client' type='chat' id='after' to='bob@example.com'/>",
            2,
        ),
    ];
    let resume =
        SmResumeState::from_unhandled_outbound_entries(StreamId::new("stream"), 0, 3, entries)
            .unwrap();
    let mut runtime = rejection_support::runtime(Some(resume));
    runtime.handle_app_stanza(&rejection_support::rejection("chat@example.com"));
    let snapshot = runtime.resume_state().unwrap();
    let flags: Vec<_> = snapshot
        .unhandled_outbound_entries()
        .map(UnhandledOutboundEntry::is_rejected)
        .collect();
    assert_eq!(flags, vec![false, true, false]);
    assert_eq!(snapshot.outbound_h(), 3);
    let mut restored = SmState::from_resume_state(&snapshot);
    assert_eq!(restored.server_h, 0, "a rejection cannot shift SM ordinals");
    assert_eq!(
        restored
            .process_ack(1)
            .iter()
            .map(|id| id.as_str())
            .collect::<Vec<_>>(),
        vec!["before"]
    );
    assert!(
        restored.process_ack(2).is_empty(),
        "a rejected send cannot become delivered"
    );
    assert_eq!(
        restored
            .process_ack(3)
            .iter()
            .map(|id| id.as_str())
            .collect::<Vec<_>>(),
        vec!["after"]
    );
    let fallback = SmState::from_resume_state(&snapshot)
        .fallback_retry_state()
        .unwrap();
    assert_eq!(
        fallback
            .unhandled_outbound_entries()
            .map(|entry| entry.message_stanza_id().unwrap().as_str())
            .collect::<Vec<_>>(),
        vec!["before", "after"]
    );
    let retries = SmState::from_resume_state(&snapshot).unhandled_stanzas_for_fallback_retry();
    assert_eq!(
        retries
            .iter()
            .map(|entry| entry.attr("id").unwrap())
            .collect::<Vec<_>>(),
        vec!["before", "after"]
    );
}

#[test]
fn rejection_changes_retry_only_for_the_retained_outbound_recipient() {
    for (recipient, muc, from, reject) in [
        (
            "chat@remote.example",
            false,
            "chat@remote.example/phone",
            true,
        ),
        ("chat@remote.example", false, "remote.example", true),
        ("chat@remote.example", false, "example.com", true),
        (
            "chat@remote.example",
            false,
            "mallory@remote.example",
            false,
        ),
        ("chat@remote.example", false, "other.example", false),
        ("room@muc.example/nick", true, "room@muc.example/nick", true),
        (
            "room@muc.example/nick",
            true,
            "room@muc.example/other",
            false,
        ),
        ("room@muc.example/nick", true, "room@muc.example", true),
        ("room@muc.example/nick", true, "muc.example", true),
    ] {
        let mut outbound = Element::builder("message", "jabber:client")
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "chat")
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "rejected")
            .attr(minidom::rxml::xml_ncname!("to").to_owned(), recipient);
        if muc {
            outbound = outbound
                .append(Element::builder("x", "http://jabber.org/protocol/muc#user").build());
        }
        let entry = UnhandledOutboundEntry::try_new(outbound.build(), Utc::now()).unwrap();
        let snapshot =
            SmResumeState::from_unhandled_outbound_entries(StreamId::new("stream"), 0, 1, [entry])
                .unwrap();
        let mut runtime = rejection_support::runtime(Some(snapshot));
        runtime.handle_app_stanza(&rejection_support::rejection(from));
        let snapshot = runtime.resume_state().unwrap();
        assert_eq!(
            snapshot
                .unhandled_outbound_entries()
                .next()
                .unwrap()
                .is_rejected(),
            reject,
            "from {from} for {recipient}"
        );
    }
}
