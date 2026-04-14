//! XEP-0176: ICE-UDP dedicated suite.

use minidom::Element;

use waddle_xmpp::xep::{
    parse_candidate_element, parse_ice_udp_transport_element, CandidateType, NS_JINGLE_ICE_UDP,
};

#[test]
fn xep0176_transport_parses_valid_candidate() {
    let xml = "<transport xmlns='urn:xmpp:jingle:transports:ice-udp:1' ufrag='abc' pwd='xyz'>\
                 <candidate foundation='1' component='1' protocol='udp' priority='2130706431' ip='192.0.2.1' port='3478' type='host' generation='0'/>\
               </transport>";
    let elem = xml.parse::<Element>().expect("valid xml");

    let parsed = parse_ice_udp_transport_element(&elem).expect("transport parses");
    assert_eq!(parsed.ufrag.as_deref(), Some("abc"));
    assert_eq!(parsed.pwd.as_deref(), Some("xyz"));
    assert_eq!(parsed.candidates.len(), 1);
    assert_eq!(parsed.candidates[0].candidate_type, CandidateType::Host);
}

#[test]
fn xep0176_validation_candidate_missing_required_fields_is_rejected() {
    let candidate = Element::builder("candidate", NS_JINGLE_ICE_UDP)
        .attr("foundation", "1")
        .build();
    assert!(parse_candidate_element(&candidate).is_none());
}

#[test]
fn xep0176_validation_transport_ignores_invalid_candidates() {
    let xml = "<transport xmlns='urn:xmpp:jingle:transports:ice-udp:1'>\
                 <candidate foundation='1'/>\
                 <candidate foundation='2' component='1' protocol='udp' priority='1' ip='198.51.100.10' port='5000' type='srflx'/>\
               </transport>";
    let elem = xml.parse::<Element>().expect("valid xml");

    let parsed = parse_ice_udp_transport_element(&elem).expect("transport parses");
    assert_eq!(parsed.candidates.len(), 1);
    assert_eq!(parsed.candidates[0].foundation, "2");
    assert_eq!(parsed.candidates[0].candidate_type, CandidateType::Srflx);
}
