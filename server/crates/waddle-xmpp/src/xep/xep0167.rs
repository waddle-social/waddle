//! XEP-0167 Jingle RTP constants and builders.

use minidom::Element;
use std::fmt;

/// Namespace for Jingle RTP session descriptions.
pub const NS_JINGLE_RTP: &str = xmpp_parsers::ns::JINGLE_RTP;
/// Jingle RTP audio disco feature.
pub const NS_JINGLE_RTP_AUDIO: &str = xmpp_parsers::ns::JINGLE_RTP_AUDIO;
/// Jingle RTP video disco feature.
pub const NS_JINGLE_RTP_VIDEO: &str = xmpp_parsers::ns::JINGLE_RTP_VIDEO;
/// Namespace for Jingle RTP errors.
pub const NS_JINGLE_RTP_ERRORS: &str = "urn:xmpp:jingle:apps:rtp:errors:1";
/// Namespace for Jingle RTP session-info payloads.
pub const NS_JINGLE_RTP_INFO: &str = "urn:xmpp:jingle:apps:rtp:info:1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtpMedia {
    Audio,
    Video,
}

impl RtpMedia {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Audio => "audio",
            Self::Video => "video",
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct PayloadTypeName(String);

impl PayloadTypeName {
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (!value.trim().is_empty()).then_some(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for PayloadTypeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("PayloadTypeName").field(&self.0).finish()
    }
}

/// Build an RTP `<description/>` element.
pub fn build_rtp_description(media: RtpMedia) -> Element {
    Element::builder("description", NS_JINGLE_RTP)
        .attr("media", media.as_str())
        .build()
}

/// Build an RTP `<payload-type/>` element.
pub fn build_payload_type(
    id: u8,
    name: Option<&PayloadTypeName>,
    clockrate: Option<u32>,
) -> Element {
    let mut elem = Element::builder("payload-type", NS_JINGLE_RTP)
        .attr("id", id.to_string())
        .build();
    if let Some(name) = name {
        elem.set_attr("name", name.as_str());
    }
    if let Some(clockrate) = clockrate {
        elem.set_attr("clockrate", clockrate.to_string());
    }
    elem
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_rtp_payload_type() {
        let name = PayloadTypeName::new("opus").expect("payload name");
        let elem = build_payload_type(111, Some(&name), Some(48_000));
        assert_eq!(elem.name(), "payload-type");
        assert_eq!(elem.ns(), NS_JINGLE_RTP);
        assert_eq!(elem.attr("id"), Some("111"));
        assert_eq!(elem.attr("name"), Some("opus"));
        assert_eq!(elem.attr("clockrate"), Some("48000"));
    }
}
