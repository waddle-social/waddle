//! XEP-0167: Jingle RTP Sessions — dedicated test suite.
//!
//! Spec: <https://xmpp.org/extensions/xep-0167.html> (v1.2.1, Final).
//!
//! Pins the wire shape of the `<description xmlns='urn:xmpp:jingle:apps:rtp:1'/>`
//! payload Waddle builds for the audio and video legs of a call:
//!
//! * The element is qualified by `urn:xmpp:jingle:apps:rtp:1` (XEP-0167 §3.1).
//! * The `media` attribute is `audio` or `video` only (XEP-0167 §3.1).
//! * Each description carries at least one `<payload-type/>` child
//!   describing a codec the initiator supports (XEP-0167 §3.2).
//! * Each description advertises `<rtcp-mux/>` so the peer can
//!   multiplex RTP and RTCP on a single port (XEP-0167 §3.3 / RFC 5761).
//!
//! Construction goes through the typed
//! [`waddle_xmpp::xep::xep0167::opus_audio_description`] /
//! [`waddle_xmpp::xep::xep0167::vp8_video_description`] helpers and
//! the typed [`MediaKind`] enum — no `format!`, no string
//! concatenation (CLAUDE.md XML hard rule).

use minidom::Element;
use waddle_xmpp::xep::xep0167::{
    opus_audio_description, vp8_video_description, MediaKind, NS_JINGLE_RTP, NS_JINGLE_RTP_AUDIO,
    NS_JINGLE_RTP_VIDEO,
};

fn description_element(media: MediaKind) -> Element {
    match media {
        MediaKind::Audio => opus_audio_description().into(),
        MediaKind::Video => vp8_video_description().into(),
    }
}

// ── §3.1 namespace and `media` attribute ──────────────────────────────

#[test]
fn xep_0167_namespace_is_canonical() {
    assert_eq!(NS_JINGLE_RTP, "urn:xmpp:jingle:apps:rtp:1");
    assert_eq!(NS_JINGLE_RTP, xmpp_parsers::ns::JINGLE_RTP);
    // Audio/video sub-namespaces used by feature-vars in disco.
    assert_eq!(NS_JINGLE_RTP_AUDIO, "urn:xmpp:jingle:apps:rtp:audio");
    assert_eq!(NS_JINGLE_RTP_VIDEO, "urn:xmpp:jingle:apps:rtp:video");
}

#[test]
fn xep_0167_audio_description_has_media_attribute() {
    // §3.1: the `media` attribute identifies the SDP-style media kind
    // for the RTP session. Audio descriptions carry `media='audio'`.
    let elem = description_element(MediaKind::Audio);
    assert_eq!(elem.name(), "description");
    assert_eq!(elem.ns(), NS_JINGLE_RTP);
    assert_eq!(elem.attr("media"), Some("audio"));
}

#[test]
fn xep_0167_video_description_has_media_attribute() {
    // §3.1: video descriptions carry `media='video'`.
    let elem = description_element(MediaKind::Video);
    assert_eq!(elem.attr("media"), Some("video"));
}

// ── MediaKind parsing — typed value, not stringly-typed ──────────────

#[test]
fn xep_0167_media_kind_parses_audio_and_video_only() {
    // The typed `MediaKind` enum is the single source of truth for
    // the `media` attribute. XEP-0167 §3.1 admits only `audio` and
    // `video`; everything else (e.g. `data`) is rejected at the
    // parser boundary so callers don't smuggle bogus values onto
    // the wire.
    assert_eq!(MediaKind::parse("audio"), Some(MediaKind::Audio));
    assert_eq!(MediaKind::parse("video"), Some(MediaKind::Video));
    assert_eq!(MediaKind::parse("data"), None);
    assert_eq!(MediaKind::parse(""), None);
    // Round-trip: parse(as_str()) is the identity on every variant.
    assert_eq!(
        MediaKind::parse(MediaKind::Audio.as_str()),
        Some(MediaKind::Audio)
    );
    assert_eq!(
        MediaKind::parse(MediaKind::Video.as_str()),
        Some(MediaKind::Video)
    );
}

// ── §3.2 payload-type child ──────────────────────────────────────────

#[test]
fn xep_0167_audio_description_carries_opus_payload_type() {
    // §3.2: the `<description/>` MUST carry at least one
    // `<payload-type/>` child enumerating the codecs the initiator
    // supports. Waddle advertises Opus on PT 111 (the de facto
    // dynamic-PT value used by browser WebRTC) with the 48 kHz
    // clockrate / stereo channel count Opus requires.
    let elem = description_element(MediaKind::Audio);
    let payload = elem
        .children()
        .find(|c| c.name() == "payload-type")
        .expect("§3.2: description has at least one <payload-type/>");
    assert_eq!(payload.attr("name"), Some("opus"));
    // PT 111 is the dynamic payload type Waddle commits to.
    assert_eq!(payload.attr("id"), Some("111"));
    assert_eq!(payload.attr("clockrate"), Some("48000"));
    assert_eq!(payload.attr("channels"), Some("2"));
}

#[test]
fn xep_0167_video_description_carries_vp8_payload_type() {
    // §3.2: video descriptions carry a payload-type per codec; VP8
    // is on the WebRTC mandatory-to-implement list and gets PT 96.
    let elem = description_element(MediaKind::Video);
    let payload = elem
        .children()
        .find(|c| c.name() == "payload-type")
        .expect("video description carries <payload-type/>");
    assert_eq!(payload.attr("name"), Some("VP8"));
    assert_eq!(payload.attr("id"), Some("96"));
    // §3.2 examples for video use a 90 kHz clockrate.
    assert_eq!(payload.attr("clockrate"), Some("90000"));
}

// ── §3.3 RTCP multiplexing ───────────────────────────────────────────

#[test]
fn xep_0167_audio_description_advertises_rtcp_mux() {
    // §3.3 / RFC 5761: `<rtcp-mux/>` signals that RTP and RTCP can
    // be multiplexed on a single port. Required by modern WebRTC
    // stacks (which is what LiveKit is). Omitting this is a
    // protocol downgrade against every modern peer.
    let elem = description_element(MediaKind::Audio);
    let rtcp_mux = elem.children().find(|c| c.name() == "rtcp-mux");
    assert!(
        rtcp_mux.is_some(),
        "XEP-0167 §3.3: audio description MUST advertise <rtcp-mux/>"
    );
    let rtcp_mux = rtcp_mux.expect("checked above");
    assert_eq!(rtcp_mux.ns(), NS_JINGLE_RTP);
}

#[test]
fn xep_0167_video_description_advertises_rtcp_mux() {
    let elem = description_element(MediaKind::Video);
    assert!(
        elem.children().any(|c| c.name() == "rtcp-mux"),
        "XEP-0167 §3.3: video description MUST advertise <rtcp-mux/>"
    );
}

// ── Round-trip through xmpp-parsers' typed surface ───────────────────

#[test]
fn xep_0167_audio_description_round_trips_through_typed_parser() {
    // The typed `xmpp_parsers::jingle_rtp::Description` is the
    // canonical parse target for a `<description/>` element. Build
    // an audio description via the Waddle helper, serialise to
    // `Element`, then reparse — proves no information is lost across
    // the typed/wire boundary.
    let elem: Element = opus_audio_description().into();
    let reparsed: xmpp_parsers::jingle_rtp::Description =
        elem.clone().try_into().expect("reparses");
    assert_eq!(reparsed.media, MediaKind::Audio.as_str());
    let payload = reparsed
        .payload_types
        .first()
        .expect("at least one payload");
    assert_eq!(payload.name.as_deref(), Some("opus"));
    assert_eq!(payload.clockrate, Some(48_000));
    assert_eq!(payload.channels, xmpp_parsers::jingle_rtp::Channels(2));
    assert!(
        reparsed.rtcp_mux.is_some(),
        "typed parse preserves rtcp-mux"
    );
}

#[test]
fn xep_0167_video_description_round_trips_through_typed_parser() {
    let elem: Element = vp8_video_description().into();
    let reparsed: xmpp_parsers::jingle_rtp::Description = elem.try_into().expect("reparses");
    assert_eq!(reparsed.media, MediaKind::Video.as_str());
    let payload = reparsed
        .payload_types
        .first()
        .expect("at least one payload");
    assert_eq!(payload.name.as_deref(), Some("VP8"));
    assert_eq!(payload.clockrate, Some(90_000));
}

#[test]
fn xep_0167_parses_third_party_description_wire_shape() {
    // Inbound `<description/>` from a non-Waddle peer (e.g. an
    // XMPP-compliant softphone) MUST parse through the typed
    // `Description` surface even when its codec list differs from
    // ours. Pins the parser contract for foreign offers.
    let xml = r#"<description xmlns='urn:xmpp:jingle:apps:rtp:1' media='audio'>
                   <payload-type id='8' name='PCMA' clockrate='8000'/>
                   <payload-type id='0' name='PCMU' clockrate='8000'/>
                 </description>"#;
    let elem: Element = xml.parse().expect("element parses");
    let desc: xmpp_parsers::jingle_rtp::Description = elem.try_into().expect("description parses");
    assert_eq!(desc.media, "audio");
    assert_eq!(desc.payload_types.len(), 2);
    assert_eq!(desc.payload_types[0].name.as_deref(), Some("PCMA"));
    assert_eq!(desc.payload_types[1].name.as_deref(), Some("PCMU"));
}

#[test]
fn xep_0167_rejects_description_in_wrong_namespace() {
    // A `<description/>` in a namespace other than
    // `urn:xmpp:jingle:apps:rtp:1` MUST NOT parse as an RTP
    // description. Waddle's Jingle handler relies on this typed
    // discriminator to refuse non-RTP descriptions.
    let elem = Element::builder("description", "urn:xmpp:jingle:apps:file-transfer:5")
        .attr("media", "audio")
        .build();
    let parsed: Result<xmpp_parsers::jingle_rtp::Description, _> = elem.try_into();
    assert!(
        parsed.is_err(),
        "description in non-RTP namespace MUST be rejected"
    );
}
