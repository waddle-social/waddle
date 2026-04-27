use waddle_xmpp::xep::xep0176::{
    build_ice_udp_transport, ice_candidates_have_credentials, IcePassword, IceUfrag,
    NS_JINGLE_ICE_UDP,
};
use xmpp_parsers::minidom::Element;

#[test]
fn xep0176_builds_ice_udp_transport() {
    let ufrag = IceUfrag::new("ufrag").expect("ufrag");
    let pwd = IcePassword::new("pwd").expect("pwd");
    let elem = build_ice_udp_transport(Some(&ufrag), Some(&pwd));
    assert_eq!(elem.name(), "transport");
    assert_eq!(elem.ns(), NS_JINGLE_ICE_UDP);
    assert_eq!(elem.attr("ufrag"), Some("ufrag"));
    assert_eq!(elem.attr("pwd"), Some("pwd"));
}

#[test]
fn xep0176_candidates_require_credentials() {
    let without_credentials = Element::builder("transport", NS_JINGLE_ICE_UDP)
        .append(Element::builder("candidate", NS_JINGLE_ICE_UDP).build())
        .build();
    assert!(!ice_candidates_have_credentials(&without_credentials));

    let with_credentials = Element::builder("transport", NS_JINGLE_ICE_UDP)
        .attr("ufrag", "u")
        .attr("pwd", "p")
        .append(Element::builder("candidate", NS_JINGLE_ICE_UDP).build())
        .build();
    assert!(ice_candidates_have_credentials(&with_credentials));
}
