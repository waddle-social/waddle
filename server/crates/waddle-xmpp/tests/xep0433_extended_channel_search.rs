//! XEP-0433: Extended Channel Search dedicated suite.
//!
//! Wire-level round trips of the search IQ pair, id/addressing
//! mirroring on the response, resilience to malformed channel
//! entries, and the case-insensitive `Searchable` matcher.

use minidom::Element;
use waddle_xmpp::xep::{
    build_search_request, build_search_response, is_search_request, parse_search_request,
    parse_search_results, ChannelResult, SearchRequest, Searchable, NS_CHANNEL_SEARCH,
};
use xmpp_parsers::iq::Iq;

fn wire_round_trip(iq: Iq) -> Iq {
    let elem = Element::from(iq);
    let xml = String::from(&elem);
    Iq::try_from(xml.parse::<Element>().expect("well-formed XML")).expect("valid IQ")
}

fn muc_service() -> jid::Jid {
    "muc.example.com".parse().expect("valid jid")
}

#[test]
fn xep0433_namespace_is_exact() {
    assert_eq!(NS_CHANNEL_SEARCH, "urn:xmpp:channel-search:0");
}

#[test]
fn xep0433_search_request_round_trips_through_wire() {
    let request = SearchRequest::new("general").with_max(20);
    let iq = wire_round_trip(build_search_request(muc_service(), &request, "search-1"));

    assert!(is_search_request(&iq));
    assert_eq!(iq.id(), "search-1");
    let parsed = parse_search_request(&iq).expect("parses");
    assert_eq!(parsed, request);
}

#[test]
fn xep0433_request_without_max_round_trips() {
    let iq = wire_round_trip(build_search_request(
        muc_service(),
        &SearchRequest::new("rust"),
        "s2",
    ));
    let parsed = parse_search_request(&iq).expect("parses");
    assert_eq!(parsed.query, "rust");
    assert_eq!(parsed.max, None);
}

#[test]
fn xep0433_iq_set_is_not_a_search_request() {
    let elem = Element::builder("search", NS_CHANNEL_SEARCH).build();
    let iq = Iq::Set {
        from: None,
        to: None,
        id: "x".to_owned(),
        payload: elem,
    };
    assert!(!is_search_request(&iq));
    assert!(parse_search_request(&iq).is_none());
}

#[test]
fn xep0433_response_mirrors_id_and_swaps_addressing() {
    let requester: jid::Jid = "hag66@shakespeare.lit/pda".parse().expect("valid jid");
    let request_iq = Iq::Get {
        from: Some(requester.clone()),
        to: Some(muc_service()),
        id: "search-9".to_owned(),
        payload: Element::builder("search", NS_CHANNEL_SEARCH).build(),
    };

    let response = build_search_response(&request_iq, &[]);
    let Iq::Result { from, to, id, .. } = &response else {
        panic!("search response must be an IQ result");
    };
    assert_eq!(id, "search-9");
    assert_eq!(from.as_ref(), Some(&muc_service()));
    assert_eq!(to.as_ref(), Some(&requester));
}

#[test]
fn xep0433_full_search_round_trip_with_metadata() {
    let request_iq = build_search_request(
        muc_service(),
        &SearchRequest::new("general").with_max(20),
        "search-1",
    );
    let results = vec![
        ChannelResult::new("general@muc.example.com")
            .with_name("General")
            .with_description("Main discussion")
            .with_occupants(42),
        ChannelResult::new("random@muc.example.com").with_name("Random"),
    ];

    let response = wire_round_trip(build_search_response(&request_iq, &results));
    let parsed = parse_search_results(&response);
    assert_eq!(parsed, results);
}

#[test]
fn xep0433_channel_without_jid_is_dropped() {
    let xml = "<iq xmlns='jabber:client' type='result' id='s1'>\
               <result xmlns='urn:xmpp:channel-search:0'>\
               <channel name='No JID'/>\
               <channel jid='kept@muc.example.com'/>\
               </result></iq>";
    let iq = Iq::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid iq");
    let parsed = parse_search_results(&iq);
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].jid, "kept@muc.example.com");
}

#[test]
fn xep0433_non_numeric_occupants_is_ignored_not_fatal() {
    let xml = "<iq xmlns='jabber:client' type='result' id='s1'>\
               <result xmlns='urn:xmpp:channel-search:0'>\
               <channel jid='room@muc.example.com' occupants='many'/>\
               </result></iq>";
    let iq = Iq::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid iq");
    let parsed = parse_search_results(&iq);
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].occupants, None);
}

#[test]
fn xep0433_result_in_wrong_namespace_yields_no_channels() {
    let xml = "<iq xmlns='jabber:client' type='result' id='s1'>\
               <result xmlns='urn:xmpp:channel-search:1'>\
               <channel jid='room@muc.example.com'/>\
               </result></iq>";
    let iq = Iq::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid iq");
    assert!(parse_search_results(&iq).is_empty());
}

#[test]
fn xep0433_searchable_matches_name_description_and_jid_case_insensitively() {
    let channel = ChannelResult::new("general@muc.example.com")
        .with_name("General Discussion")
        .with_description("Main chat for everyone");

    assert!(channel.matches_query("GENERAL"));
    assert!(channel.matches_query("discussion"));
    assert!(channel.matches_query("everyone"));
    assert!(channel.matches_query("muc.example.com"));
    assert!(!channel.matches_query("random"));
}

#[test]
fn xep0433_display_name_falls_back_to_jid_localpart() {
    assert_eq!(
        ChannelResult::new("room@muc.example.com")
            .with_name("My Room")
            .display_name(),
        "My Room"
    );
    assert_eq!(
        ChannelResult::new("room@muc.example.com").display_name(),
        "room"
    );
}

#[test]
fn xep0433_attribute_values_survive_escaping_round_trip() {
    let request_iq = build_search_request(muc_service(), &SearchRequest::new("q"), "esc-1");
    let results = vec![ChannelResult::new("odd@muc.example.com")
        .with_name("Tom & Jerry's <Room>")
        .with_description("\"quoted\" & <bracketed>")];

    let response = wire_round_trip(build_search_response(&request_iq, &results));
    let parsed = parse_search_results(&response);
    assert_eq!(parsed[0].name.as_deref(), Some("Tom & Jerry's <Room>"));
    assert_eq!(
        parsed[0].description.as_deref(),
        Some("\"quoted\" & <bracketed>")
    );
}
