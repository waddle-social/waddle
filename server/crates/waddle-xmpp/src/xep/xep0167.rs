//! XEP-0167: Jingle RTP Sessions
//!
//! Supports RTP media descriptions for Jingle content.

use minidom::Element;

/// Namespace for XEP-0167 RTP descriptions.
pub const NS_JINGLE_RTP: &str = "urn:xmpp:jingle:apps:rtp:1";

/// RTP media type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MediaType {
    Audio,
    Video,
    Application,
    Other(String),
}

impl MediaType {
    pub fn from_str_attr(value: &str) -> Self {
        match value {
            "audio" => Self::Audio,
            "video" => Self::Video,
            "application" => Self::Application,
            other => Self::Other(other.to_owned()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Audio => "audio",
            Self::Video => "video",
            Self::Application => "application",
            Self::Other(value) => value,
        }
    }
}

/// RTP payload parameter (`<parameter/>`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RtpParameter {
    pub name: String,
    pub value: Option<String>,
}

/// RTP payload type (`<payload-type/>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtpPayloadType {
    pub id: u8,
    pub name: Option<String>,
    pub clockrate: Option<u32>,
    pub channels: Option<u8>,
    pub parameters: Vec<RtpParameter>,
}

impl RtpPayloadType {
    /// Create payload type with required ID.
    pub fn new(id: u8) -> Self {
        Self {
            id,
            name: None,
            clockrate: None,
            channels: None,
            parameters: Vec::new(),
        }
    }
}

/// RTP description payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtpDescription {
    pub media: MediaType,
    pub payload_types: Vec<RtpPayloadType>,
    pub rtcp_mux: bool,
}

impl RtpDescription {
    /// Create a media description for the specified media kind.
    pub fn new(media: MediaType) -> Self {
        Self {
            media,
            payload_types: Vec::new(),
            rtcp_mux: false,
        }
    }

    /// Add one payload type.
    pub fn with_payload_type(mut self, payload_type: RtpPayloadType) -> Self {
        self.payload_types.push(payload_type);
        self
    }
}

/// Check whether an element is an RTP `<description/>`.
pub fn is_rtp_description_element(elem: &Element) -> bool {
    elem.name() == "description" && elem.ns() == NS_JINGLE_RTP
}

/// Parse RTP description from `<description xmlns='urn:xmpp:jingle:apps:rtp:1'/>`.
pub fn parse_rtp_description_element(elem: &Element) -> Option<RtpDescription> {
    if !is_rtp_description_element(elem) {
        return None;
    }

    let media = MediaType::from_str_attr(elem.attr("media")?);

    let payload_types = elem
        .children()
        .filter(|child| child.name() == "payload-type" && child.ns() == NS_JINGLE_RTP)
        .filter_map(|payload| {
            let id = payload.attr("id")?.parse::<u8>().ok()?;
            let name = payload
                .attr("name")
                .filter(|v| !v.is_empty())
                .map(str::to_owned);
            let clockrate = payload
                .attr("clockrate")
                .and_then(|v| v.parse::<u32>().ok());
            let channels = payload.attr("channels").and_then(|v| v.parse::<u8>().ok());
            let parameters = payload
                .children()
                .filter(|param| param.name() == "parameter" && param.ns() == NS_JINGLE_RTP)
                .filter_map(|param| {
                    let name = param.attr("name").filter(|v| !v.is_empty())?.to_owned();
                    let value = param
                        .attr("value")
                        .filter(|v| !v.is_empty())
                        .map(str::to_owned);
                    Some(RtpParameter { name, value })
                })
                .collect();

            Some(RtpPayloadType {
                id,
                name,
                clockrate,
                channels,
                parameters,
            })
        })
        .collect();

    let rtcp_mux = elem
        .children()
        .any(|child| child.name() == "rtcp-mux" && child.ns() == NS_JINGLE_RTP);

    Some(RtpDescription {
        media,
        payload_types,
        rtcp_mux,
    })
}

/// Build RTP description element.
pub fn build_rtp_description_element(description: &RtpDescription) -> Element {
    let mut elem = Element::builder("description", NS_JINGLE_RTP)
        .attr("media", description.media.as_str())
        .build();

    for payload_type in &description.payload_types {
        let mut payload = Element::builder("payload-type", NS_JINGLE_RTP)
            .attr("id", payload_type.id.to_string())
            .build();

        if let Some(ref name) = payload_type.name {
            payload.set_attr("name", name);
        }

        if let Some(clockrate) = payload_type.clockrate {
            payload.set_attr("clockrate", clockrate.to_string());
        }

        if let Some(channels) = payload_type.channels {
            payload.set_attr("channels", channels.to_string());
        }

        for parameter in &payload_type.parameters {
            let mut param = Element::builder("parameter", NS_JINGLE_RTP)
                .attr("name", parameter.name.as_str())
                .build();

            if let Some(ref value) = parameter.value {
                param.set_attr("value", value);
            }

            payload.append_child(param);
        }

        elem.append_child(payload);
    }

    if description.rtcp_mux {
        elem.append_child(Element::builder("rtcp-mux", NS_JINGLE_RTP).build());
    }

    elem
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_rtp_description() {
        let xml = "<description xmlns='urn:xmpp:jingle:apps:rtp:1' media='audio'>\
                     <payload-type id='111' name='opus' clockrate='48000' channels='2'>\
                       <parameter name='minptime' value='10'/>\
                     </payload-type>\
                     <rtcp-mux/>\
                   </description>";
        let elem = xml.parse::<Element>().expect("valid xml");

        let parsed = parse_rtp_description_element(&elem).expect("description should parse");
        assert_eq!(parsed.media, MediaType::Audio);
        assert!(parsed.rtcp_mux);
        assert_eq!(parsed.payload_types.len(), 1);
        assert_eq!(parsed.payload_types[0].id, 111);
        assert_eq!(parsed.payload_types[0].parameters[0].name, "minptime");
    }

    #[test]
    fn test_parse_requires_media_attr() {
        let elem = Element::builder("description", NS_JINGLE_RTP).build();
        assert!(parse_rtp_description_element(&elem).is_none());
    }

    #[test]
    fn test_build_rtp_description() {
        let payload = RtpPayloadType {
            id: 111,
            name: Some("opus".to_owned()),
            clockrate: Some(48000),
            channels: Some(2),
            parameters: vec![RtpParameter {
                name: "minptime".to_owned(),
                value: Some("10".to_owned()),
            }],
        };

        let description = RtpDescription::new(MediaType::Audio).with_payload_type(payload);
        let elem = build_rtp_description_element(&description);

        assert!(is_rtp_description_element(&elem));
        assert_eq!(elem.attr("media"), Some("audio"));
        assert_eq!(
            elem.children()
                .filter(|child| child.name() == "payload-type" && child.ns() == NS_JINGLE_RTP)
                .count(),
            1
        );
    }
}
