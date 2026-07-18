//! XEP-0319: Last User Interaction in Presence dedicated suite.
//!
//! Round trips the `<idle/>` payload through presence stanzas,
//! including timezone-offset timestamps, replacement semantics of
//! `add_idle`, and the typed error for malformed `since` values.

use chrono::{DateTime, TimeZone, Utc};
use minidom::Element;
use waddle_xmpp::xep::{
    add_idle, build_idle_element, extract_idle_from_presence, has_idle, is_idle_element,
    parse_idle_element, strip_idle, IdleCarrier, IdleError, NS_IDLE,
};
use xmpp_parsers::presence::Presence;

fn presence_from(xml: &str) -> Presence {
    Presence::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid presence")
}

fn test_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2024, 6, 1, 12, 0, 0)
        .single()
        .expect("valid test date")
}

#[test]
fn xep0319_namespace_is_exact() {
    assert_eq!(NS_IDLE, "urn:xmpp:idle:1");
}

#[test]
fn xep0319_spec_example_presence_extracts_idle() {
    // Shape from XEP-0319 Example 1.
    let presence = presence_from(
        "<presence xmlns='jabber:client' from='juliet@capulet.com/balcony'>\
         <show>away</show>\
         <idle xmlns='urn:xmpp:idle:1' since='2024-06-01T12:00:00Z'/>\
         </presence>",
    );
    assert!(has_idle(&presence));
    let idle = extract_idle_from_presence(&presence).expect("idle present");
    assert_eq!(idle.since, test_time());
}

#[test]
fn xep0319_offset_timestamp_normalizes_to_utc() {
    // XEP-0082 timestamps may carry a zone offset; +02:00 at 14:00
    // local is 12:00Z.
    let presence = presence_from(
        "<presence xmlns='jabber:client'>\
         <idle xmlns='urn:xmpp:idle:1' since='2024-06-01T14:00:00+02:00'/>\
         </presence>",
    );
    let idle = extract_idle_from_presence(&presence).expect("idle present");
    assert_eq!(idle.since, test_time());
}

#[test]
fn xep0319_build_element_round_trips_through_wire_xml() {
    let elem = build_idle_element(test_time());
    assert!(is_idle_element(&elem));

    let reparsed: Element = String::from(&elem)
        .parse()
        .expect("serialized idle is well-formed XML");
    let idle = parse_idle_element(&reparsed).expect("round trip");
    assert_eq!(idle.since, test_time());
}

#[test]
fn xep0319_add_idle_replaces_existing_payload() {
    let mut presence = presence_from(
        "<presence xmlns='jabber:client'>\
         <idle xmlns='urn:xmpp:idle:1' since='2020-01-01T00:00:00Z'/>\
         </presence>",
    );

    add_idle(&mut presence, test_time());

    let idle_payloads: Vec<_> = presence
        .payloads
        .iter()
        .filter(|e| is_idle_element(e))
        .collect();
    assert_eq!(idle_payloads.len(), 1, "add_idle must not duplicate");
    assert_eq!(
        extract_idle_from_presence(&presence).expect("idle").since,
        test_time()
    );
}

#[test]
fn xep0319_strip_idle_removes_payload_but_keeps_others() {
    let mut presence = presence_from(
        "<presence xmlns='jabber:client'>\
         <show>away</show>\
         <idle xmlns='urn:xmpp:idle:1' since='2024-06-01T12:00:00Z'/>\
         </presence>",
    );
    strip_idle(&mut presence);
    assert!(!has_idle(&presence));
    assert!(!presence.is_idle());
}

#[test]
fn xep0319_missing_since_is_typed_error() {
    let elem = Element::builder("idle", NS_IDLE).build();
    let err = parse_idle_element(&elem).expect_err("must reject missing since");
    assert!(matches!(err, IdleError::InvalidSince(_)));
}

#[test]
fn xep0319_malformed_since_is_typed_error() {
    let elem: Element = "<idle xmlns='urn:xmpp:idle:1' since='yesterday-ish'/>"
        .parse()
        .expect("valid xml");
    let err = parse_idle_element(&elem).expect_err("must reject junk since");
    assert!(matches!(err, IdleError::InvalidSince(_)));
}

#[test]
fn xep0319_idle_element_in_wrong_namespace_is_not_detected() {
    let presence = presence_from(
        "<presence xmlns='jabber:client'>\
         <idle xmlns='urn:xmpp:idle:0' since='2024-06-01T12:00:00Z'/>\
         </presence>",
    );
    assert!(!has_idle(&presence));
    assert!(extract_idle_from_presence(&presence).is_none());
}

#[test]
fn xep0319_idle_carrier_trait_exposes_since() {
    let presence = presence_from(
        "<presence xmlns='jabber:client'>\
         <idle xmlns='urn:xmpp:idle:1' since='2024-06-01T12:00:00Z'/>\
         </presence>",
    );
    assert!(presence.is_idle());
    assert_eq!(presence.idle_since(), Some(test_time()));

    let plain = presence_from("<presence xmlns='jabber:client'/>");
    assert!(!plain.is_idle());
    assert_eq!(plain.idle_since(), None);
}

#[tokio::test]
async fn xep0319_idle_payload_survives_sm_detach_snapshot() {
    // Issue #1103: a probe response for a XEP-0198 detached (awaiting
    // resume) resource must still carry the client's advertised
    // `<idle xmlns='urn:xmpp:idle:1' since='…'/>`. The registry therefore
    // stores the resource's presence extension payloads on the detached
    // session and returns them from the detached presence-state reads.
    use std::time::Instant;
    use waddle_xmpp::stream_management::{
        DetachedSession, InMemorySmSessionRegistry, SmSessionGenerationId, SmSessionRegistry,
    };

    let registry = InMemorySmSessionRegistry::new();
    let jid: jid::FullJid = "juliet@capulet.com/balcony".parse().expect("full jid");
    let idle_element = build_idle_element(test_time());
    registry
        .store_session(DetachedSession {
            stream_id: "xep0319-detached".to_string(),
            generation_id: SmSessionGenerationId::new(),
            user_id: "juliet@capulet.com".to_string(),
            jid: jid.clone(),
            inbound_count: 0,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: Instant::now(),
            carbons_enabled: false,
            roster_interested: true,
            blocklist_interested: false,
            presence_available: true,
            presence_show: Some(xmpp_parsers::presence::Show::Away),
            presence_status: None,
            presence_priority: 0,
            presence_payloads: vec![idle_element.clone()],
            pending_subscribes_flushed: false,
        })
        .await
        .expect("store detached session");

    let state = registry
        .detached_presence_state(&jid)
        .await
        .expect("registry read")
        .expect("detached presence state present");
    assert_eq!(
        state.payloads,
        vec![idle_element.clone()],
        "detached presence state must return the stored XEP-0319 idle payload verbatim"
    );

    let all_states = registry
        .available_detached_presence_states_for_user(&jid.to_bare())
        .await
        .expect("registry read");
    assert_eq!(all_states.len(), 1);
    assert_eq!(all_states[0].resource, jid);
    let idle = parse_idle_element(&all_states[0].payloads[0]).expect("payload is a valid idle");
    assert_eq!(
        idle.since,
        test_time(),
        "the idle instant round-trips through the detached snapshot unchanged"
    );
}
