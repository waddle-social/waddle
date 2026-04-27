use waddle_xmpp::xep::xep0167::{
    build_payload_type, build_rtp_description, PayloadTypeName, RtpMedia, NS_JINGLE_RTP,
    NS_JINGLE_RTP_AUDIO, NS_JINGLE_RTP_VIDEO,
};

#[test]
fn xep0167_builds_rtp_description() {
    let elem = build_rtp_description(RtpMedia::Audio);
    assert_eq!(elem.name(), "description");
    assert_eq!(elem.ns(), NS_JINGLE_RTP);
    assert_eq!(elem.attr("media"), Some("audio"));
}

#[test]
fn xep0167_builds_payload_type() {
    let name = PayloadTypeName::new("opus").expect("payload name");
    let elem = build_payload_type(111, Some(&name), Some(48_000));
    assert_eq!(elem.name(), "payload-type");
    assert_eq!(elem.ns(), NS_JINGLE_RTP);
    assert_eq!(elem.attr("id"), Some("111"));
    assert_eq!(elem.attr("name"), Some("opus"));
    assert_eq!(elem.attr("clockrate"), Some("48000"));
}

#[test]
fn xep0167_exposes_audio_video_feature_constants() {
    assert_eq!(NS_JINGLE_RTP_AUDIO, "urn:xmpp:jingle:apps:rtp:audio");
    assert_eq!(NS_JINGLE_RTP_VIDEO, "urn:xmpp:jingle:apps:rtp:video");
}
