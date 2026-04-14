//! XEP-0272: Multiparty Jingle (Muji)
//!
//! Presence-level multiparty call signaling profile built on top of Jingle.

use minidom::Element;
use xmpp_parsers::presence::Presence;

use super::xep0166::{build_jingle_content_element, parse_jingle_content_element, JingleContent};

/// Namespace for XEP-0272 Muji.
pub const NS_MUJI: &str = "urn:xmpp:muji:0";

/// Muji call state marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MujiStatus {
    Preparing,
    Initiating,
    Ringing,
    Active,
    Ended,
}

impl MujiStatus {
    fn from_element_name(name: &str) -> Option<Self> {
        match name {
            "preparing" => Some(Self::Preparing),
            "initiating" => Some(Self::Initiating),
            "ringing" => Some(Self::Ringing),
            "active" => Some(Self::Active),
            "ended" => Some(Self::Ended),
            _ => None,
        }
    }

    fn as_element_name(self) -> &'static str {
        match self {
            Self::Preparing => "preparing",
            Self::Initiating => "initiating",
            Self::Ringing => "ringing",
            Self::Active => "active",
            Self::Ended => "ended",
        }
    }
}

/// Parsed Muji conference payload.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Muji {
    /// SFU service JID used for the conference topology.
    pub sfu_jid: Option<String>,
    /// Payload mapping owner. First release defaults to SFU canonical.
    pub payload_owner: Option<String>,
    /// Current Muji statuses.
    pub statuses: Vec<MujiStatus>,
    /// Embedded Jingle content descriptors.
    pub contents: Vec<JingleContent>,
}

impl Muji {
    /// Create empty Muji payload.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set SFU service JID.
    pub fn with_sfu_jid(mut self, jid: impl Into<String>) -> Self {
        self.sfu_jid = Some(jid.into());
        self
    }

    /// Set payload owner marker.
    pub fn with_payload_owner(mut self, owner: impl Into<String>) -> Self {
        self.payload_owner = Some(owner.into());
        self
    }

    /// Add a status marker.
    pub fn with_status(mut self, status: MujiStatus) -> Self {
        self.statuses.push(status);
        self
    }

    /// Add a Jingle content descriptor.
    pub fn with_content(mut self, content: JingleContent) -> Self {
        self.contents.push(content);
        self
    }
}

/// Trait for types that can carry Muji payload.
pub trait MujiCarrier {
    /// Extract Muji payload from this carrier.
    fn muji(&self) -> Option<Muji>;

    /// Returns true when Muji payload is present.
    fn has_muji(&self) -> bool {
        self.muji().is_some()
    }
}

impl MujiCarrier for Presence {
    fn muji(&self) -> Option<Muji> {
        extract_muji_from_presence(self)
    }
}

/// Check whether an element is Muji payload.
pub fn is_muji_element(elem: &Element) -> bool {
    elem.name() == "muji" && elem.ns() == NS_MUJI
}

/// Check whether presence contains Muji payload.
pub fn has_muji(presence: &Presence) -> bool {
    presence.payloads.iter().any(is_muji_element)
}

/// Parse Muji payload element.
pub fn parse_muji_element(elem: &Element) -> Option<Muji> {
    if !is_muji_element(elem) {
        return None;
    }

    let sfu_jid = elem
        .get_child("service", NS_MUJI)
        .and_then(|service| service.attr("jid"))
        .filter(|value| !value.is_empty())
        .map(str::to_owned);

    let payload_owner = elem
        .attr("payload-owner")
        .filter(|value| !value.is_empty())
        .map(str::to_owned);

    let statuses = elem
        .children()
        .filter(|child| child.ns() == NS_MUJI)
        .filter_map(|child| MujiStatus::from_element_name(child.name()))
        .collect();

    let contents = elem
        .children()
        .filter(|child| child.name() == "content" && child.ns() == super::xep0166::NS_JINGLE)
        .filter_map(parse_jingle_content_element)
        .collect();

    Some(Muji {
        sfu_jid,
        payload_owner,
        statuses,
        contents,
    })
}

/// Extract Muji payload from presence.
pub fn extract_muji_from_presence(presence: &Presence) -> Option<Muji> {
    presence
        .payloads
        .iter()
        .find(|elem| is_muji_element(elem))
        .and_then(parse_muji_element)
}

/// Build Muji payload element.
pub fn build_muji_element(muji: &Muji) -> Element {
    let mut elem = Element::builder("muji", NS_MUJI).build();

    if let Some(ref payload_owner) = muji.payload_owner {
        elem.set_attr("payload-owner", payload_owner);
    }

    if let Some(ref sfu_jid) = muji.sfu_jid {
        elem.append_child(
            Element::builder("service", NS_MUJI)
                .attr("jid", sfu_jid.as_str())
                .build(),
        );
    }

    for status in &muji.statuses {
        elem.append_child(Element::builder(status.as_element_name(), NS_MUJI).build());
    }

    for content in &muji.contents {
        elem.append_child(build_jingle_content_element(content));
    }

    elem
}

/// Set Muji payload on presence, replacing existing Muji payload.
pub fn set_muji(presence: &mut Presence, muji: &Muji) {
    strip_muji(presence);
    presence.payloads.push(build_muji_element(muji));
}

/// Remove Muji payload from presence.
pub fn strip_muji(presence: &mut Presence) {
    presence.payloads.retain(|elem| elem.ns() != NS_MUJI);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xep::xep0166::{ContentCreator, JingleContent};
    use crate::xep::xep0167::{build_rtp_description_element, MediaType, RtpDescription};
    use crate::xep::xep0176::{build_ice_udp_transport_element, IceUdpTransport};

    #[test]
    fn test_parse_muji_presence() {
        let xml = "<presence xmlns='jabber:client'>\
                     <muji xmlns='urn:xmpp:muji:0' payload-owner='sfu'>\
                       <service jid='sfu.waddle.social'/>\
                       <preparing/>\
                       <content xmlns='urn:xmpp:jingle:1' creator='initiator' name='audio'>\
                         <description xmlns='urn:xmpp:jingle:apps:rtp:1' media='audio'/>\
                         <transport xmlns='urn:xmpp:jingle:transports:ice-udp:1'/>\
                       </content>\
                     </muji>\
                   </presence>";

        let presence =
            Presence::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid presence");

        let muji = extract_muji_from_presence(&presence).expect("muji should parse");
        assert_eq!(muji.sfu_jid.as_deref(), Some("sfu.waddle.social"));
        assert_eq!(muji.payload_owner.as_deref(), Some("sfu"));
        assert_eq!(muji.statuses, vec![MujiStatus::Preparing]);
        assert_eq!(muji.contents.len(), 1);
    }

    #[test]
    fn test_build_muji_element() {
        let content = JingleContent {
            creator: ContentCreator::Initiator,
            name: "audio".to_owned(),
            senders: None,
            description: Some(build_rtp_description_element(&RtpDescription::new(
                MediaType::Audio,
            ))),
            transport: Some(build_ice_udp_transport_element(&IceUdpTransport::new())),
        };

        let muji = Muji::new()
            .with_sfu_jid("sfu.waddle.social")
            .with_payload_owner("sfu")
            .with_status(MujiStatus::Preparing)
            .with_content(content);

        let elem = build_muji_element(&muji);
        assert!(is_muji_element(&elem));
        assert_eq!(elem.attr("payload-owner"), Some("sfu"));
        assert!(elem.get_child("service", NS_MUJI).is_some());
    }

    #[test]
    fn test_set_and_strip_muji() {
        let mut presence = Presence::try_from(
            "<presence xmlns='jabber:client'><status>online</status></presence>"
                .parse::<Element>()
                .expect("valid xml"),
        )
        .expect("valid presence");

        let muji = Muji::new().with_status(MujiStatus::Active);
        set_muji(&mut presence, &muji);
        assert!(has_muji(&presence));

        strip_muji(&mut presence);
        assert!(!has_muji(&presence));
    }
}
