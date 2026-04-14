//! XEP-0166: Jingle dedicated suite.

use minidom::Element;
use xmpp_parsers::iq::Iq;

use waddle_xmpp::xep::{
    build_ice_udp_transport_element, build_jingle_element, build_rtp_description_element,
    parse_jingle_element, parse_jingle_iq, ContentCreator, Jingle, JingleAction, JingleContent,
    MediaType, RtpDescription, Senders, NS_JINGLE,
};

#[test]
fn xep0166_session_initiate_round_trip_with_rtp_and_ice() {
    let content = JingleContent {
        creator: ContentCreator::Initiator,
        name: "audio".to_string(),
        senders: Some(Senders::Both),
        description: Some(build_rtp_description_element(&RtpDescription::new(
            MediaType::Audio,
        ))),
        transport: Some(build_ice_udp_transport_element(
            &waddle_xmpp::xep::IceUdpTransport::new(),
        )),
    };

    let jingle = Jingle::new(JingleAction::SessionInitiate, "sid-123").with_content(content);
    let elem = build_jingle_element(&jingle);
    let parsed = parse_jingle_element(&elem).expect("jingle parses");

    assert_eq!(parsed.action, JingleAction::SessionInitiate);
    assert_eq!(parsed.sid, "sid-123");
    assert_eq!(parsed.contents.len(), 1);
    assert_eq!(parsed.contents[0].name, "audio");
}

#[test]
fn xep0166_validation_missing_sid_is_rejected() {
    let elem = Element::builder("jingle", NS_JINGLE)
        .attr("action", "session-initiate")
        .build();

    assert!(parse_jingle_element(&elem).is_none());
}

#[test]
fn xep0166_validation_invalid_content_creator_is_ignored() {
    let xml = "<iq xmlns='jabber:client' type='set' id='j-invalid'>\
                 <jingle xmlns='urn:xmpp:jingle:1' action='session-initiate' sid='sid-1'>\
                   <content creator='invalid' name='audio'/>\
                 </jingle>\
               </iq>";
    let iq = Iq::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid iq");

    let parsed = parse_jingle_iq(&iq).expect("jingle payload should still parse");
    assert_eq!(parsed.contents.len(), 0);
}
