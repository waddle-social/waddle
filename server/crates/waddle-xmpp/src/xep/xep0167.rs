//! XEP-0167: Jingle RTP Sessions.
//!
//! Re-exports [`xmpp_parsers::jingle_rtp`] types and owns the
//! feature-var constants the server advertises in disco. Builder
//! helpers create the typed `<description/>` payloads Waddle uses on
//! `session-initiate` for audio and video content.

pub use xmpp_parsers::jingle_rtp::{
    Description as RtpDescription, Parameter, PayloadType, RtcpMux,
};

pub const NS_JINGLE_RTP: &str = "urn:xmpp:jingle:apps:rtp:1";
pub const NS_JINGLE_RTP_AUDIO: &str = "urn:xmpp:jingle:apps:rtp:audio";
pub const NS_JINGLE_RTP_VIDEO: &str = "urn:xmpp:jingle:apps:rtp:video";

/// Media kind on a Jingle RTP `<description/>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Audio,
    Video,
}

impl MediaKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Audio => "audio",
            Self::Video => "video",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "audio" => Some(Self::Audio),
            "video" => Some(Self::Video),
            _ => None,
        }
    }
}

/// Build a minimal Opus audio RTP description suitable for use on
/// session-initiate when LiveKit will negotiate the actual codecs.
/// LiveKit clients renegotiate codecs internally via SDP; the
/// payload-type list here is informational so an XEP-compliant
/// peer-viewer sees a sensible advertisement.
pub fn opus_audio_description() -> RtpDescription {
    let mut desc = RtpDescription::new(MediaKind::Audio.as_str().to_string());
    let mut payload = PayloadType::new(111, "opus".to_string(), 48_000, 2);
    payload.maxptime = Some(60);
    desc.payload_types.push(payload);
    desc
}

/// Build a minimal VP8 video RTP description for symmetry with
/// [`opus_audio_description`].
pub fn vp8_video_description() -> RtpDescription {
    let mut desc = RtpDescription::new(MediaKind::Video.as_str().to_string());
    let payload = PayloadType::new(96, "VP8".to_string(), 90_000, 1);
    desc.payload_types.push(payload);
    desc
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;

    #[test]
    fn opus_description_emits_payload_type() {
        let desc = opus_audio_description();
        let elem: Element = desc.into();
        assert_eq!(elem.name(), "description");
        assert_eq!(elem.ns(), NS_JINGLE_RTP);
        assert_eq!(elem.attr("media"), Some("audio"));
        let payload = elem
            .children()
            .find(|c| c.name() == "payload-type")
            .expect("payload-type present");
        assert_eq!(payload.attr("name"), Some("opus"));
        assert_eq!(payload.attr("clockrate"), Some("48000"));
        assert_eq!(payload.attr("channels"), Some("2"));
    }

    #[test]
    fn vp8_description_is_video_media() {
        let desc = vp8_video_description();
        let elem: Element = desc.into();
        assert_eq!(elem.attr("media"), Some("video"));
        let payload = elem
            .children()
            .find(|c| c.name() == "payload-type")
            .expect("payload-type present");
        assert_eq!(payload.attr("name"), Some("VP8"));
    }

    #[test]
    fn media_kind_parses_and_serializes() {
        assert_eq!(MediaKind::parse("audio"), Some(MediaKind::Audio));
        assert_eq!(MediaKind::parse("video"), Some(MediaKind::Video));
        assert_eq!(MediaKind::parse("data"), None);
        assert_eq!(MediaKind::Audio.as_str(), "audio");
        assert_eq!(MediaKind::Video.as_str(), "video");
    }

    #[test]
    fn ns_constants_match_xmpp_parsers() {
        assert_eq!(NS_JINGLE_RTP, xmpp_parsers::ns::JINGLE_RTP);
        assert_eq!(NS_JINGLE_RTP_AUDIO, xmpp_parsers::ns::JINGLE_RTP_AUDIO);
        assert_eq!(NS_JINGLE_RTP_VIDEO, xmpp_parsers::ns::JINGLE_RTP_VIDEO);
    }
}
