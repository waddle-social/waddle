//! XEP-0167: RTP dedicated suite.

use minidom::Element;

use waddle_xmpp::xep::{
    build_rtp_description_element, parse_rtp_description_element, MediaType, RtpDescription,
    RtpPayloadType, NS_JINGLE_RTP,
};

#[test]
fn xep0167_rtp_description_round_trip() {
    let payload = RtpPayloadType {
        id: 111,
        name: Some("opus".to_string()),
        clockrate: Some(48_000),
        channels: Some(2),
        parameters: vec![waddle_xmpp::xep::RtpParameter {
            name: "minptime".to_string(),
            value: Some("10".to_string()),
        }],
    };
    let description = RtpDescription::new(MediaType::Audio).with_payload_type(payload);

    let elem = build_rtp_description_element(&description);
    let parsed = parse_rtp_description_element(&elem).expect("description parses");

    assert_eq!(parsed.media, MediaType::Audio);
    assert_eq!(parsed.payload_types.len(), 1);
    assert_eq!(parsed.payload_types[0].id, 111);
}

#[test]
fn xep0167_validation_missing_media_is_rejected() {
    let elem = Element::builder("description", NS_JINGLE_RTP).build();
    assert!(parse_rtp_description_element(&elem).is_none());
}

#[test]
fn xep0167_validation_invalid_payload_id_is_ignored() {
    let xml = "<description xmlns='urn:xmpp:jingle:apps:rtp:1' media='audio'>\
                 <payload-type id='not-a-number' name='opus'/>\
                 <payload-type id='111' name='opus' clockrate='48000'/>\
               </description>";
    let elem = xml.parse::<Element>().expect("valid xml");

    let parsed = parse_rtp_description_element(&elem).expect("description parses");
    assert_eq!(parsed.payload_types.len(), 1);
    assert_eq!(parsed.payload_types[0].id, 111);
}
