//! XEP-0176: Jingle ICE-UDP Transport Method
//!
//! Supports ICE-UDP transport signaling in Jingle content.

use minidom::Element;

use super::xep0320::{
    build_fingerprint_element, parse_fingerprint_element, DtlsFingerprint, NS_JINGLE_DTLS,
};

/// Namespace for XEP-0176 ICE-UDP transport.
pub const NS_JINGLE_ICE_UDP: &str = "urn:xmpp:jingle:transports:ice-udp:1";

/// ICE candidate type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CandidateType {
    Host,
    Srflx,
    Prflx,
    Relay,
    Other(String),
}

impl CandidateType {
    pub fn from_str_attr(value: &str) -> Self {
        match value {
            "host" => Self::Host,
            "srflx" => Self::Srflx,
            "prflx" => Self::Prflx,
            "relay" => Self::Relay,
            other => Self::Other(other.to_owned()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Host => "host",
            Self::Srflx => "srflx",
            Self::Prflx => "prflx",
            Self::Relay => "relay",
            Self::Other(value) => value,
        }
    }
}

/// ICE candidate entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IceUdpCandidate {
    pub foundation: String,
    pub component: u16,
    pub protocol: String,
    pub priority: u32,
    pub ip: String,
    pub port: u16,
    pub candidate_type: CandidateType,
    pub generation: Option<u32>,
}

impl IceUdpCandidate {
    /// Create an ICE candidate with required core fields.
    pub fn new(
        foundation: impl Into<String>,
        component: u16,
        protocol: impl Into<String>,
        priority: u32,
        ip: impl Into<String>,
        port: u16,
        candidate_type: CandidateType,
    ) -> Self {
        Self {
            foundation: foundation.into(),
            component,
            protocol: protocol.into(),
            priority,
            ip: ip.into(),
            port,
            candidate_type,
            generation: None,
        }
    }
}

/// ICE-UDP transport payload.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IceUdpTransport {
    pub ufrag: Option<String>,
    pub pwd: Option<String>,
    pub fingerprints: Vec<DtlsFingerprint>,
    pub candidates: Vec<IceUdpCandidate>,
}

impl IceUdpTransport {
    /// Create an empty ICE transport.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a DTLS fingerprint.
    pub fn with_fingerprint(mut self, fingerprint: DtlsFingerprint) -> Self {
        self.fingerprints.push(fingerprint);
        self
    }

    /// Add a candidate.
    pub fn with_candidate(mut self, candidate: IceUdpCandidate) -> Self {
        self.candidates.push(candidate);
        self
    }
}

/// Check whether an element is an ICE-UDP `<transport/>`.
pub fn is_ice_udp_transport_element(elem: &Element) -> bool {
    elem.name() == "transport" && elem.ns() == NS_JINGLE_ICE_UDP
}

/// Parse ICE candidate child element.
pub fn parse_candidate_element(elem: &Element) -> Option<IceUdpCandidate> {
    if elem.name() != "candidate" || elem.ns() != NS_JINGLE_ICE_UDP {
        return None;
    }

    let foundation = elem
        .attr("foundation")
        .filter(|v| !v.is_empty())?
        .to_owned();
    let component = elem.attr("component")?.parse::<u16>().ok()?;
    let protocol = elem.attr("protocol").filter(|v| !v.is_empty())?.to_owned();
    let priority = elem.attr("priority")?.parse::<u32>().ok()?;
    let ip = elem.attr("ip").filter(|v| !v.is_empty())?.to_owned();
    let port = elem.attr("port")?.parse::<u16>().ok()?;
    let candidate_type = CandidateType::from_str_attr(elem.attr("type")?);
    let generation = elem.attr("generation").and_then(|v| v.parse::<u32>().ok());

    Some(IceUdpCandidate {
        foundation,
        component,
        protocol,
        priority,
        ip,
        port,
        candidate_type,
        generation,
    })
}

/// Build ICE candidate child element.
pub fn build_candidate_element(candidate: &IceUdpCandidate) -> Element {
    let mut elem = Element::builder("candidate", NS_JINGLE_ICE_UDP)
        .attr("foundation", candidate.foundation.as_str())
        .attr("component", candidate.component.to_string())
        .attr("protocol", candidate.protocol.as_str())
        .attr("priority", candidate.priority.to_string())
        .attr("ip", candidate.ip.as_str())
        .attr("port", candidate.port.to_string())
        .attr("type", candidate.candidate_type.as_str())
        .build();

    if let Some(generation) = candidate.generation {
        elem.set_attr("generation", generation.to_string());
    }

    elem
}

/// Parse ICE-UDP transport element.
pub fn parse_ice_udp_transport_element(elem: &Element) -> Option<IceUdpTransport> {
    if !is_ice_udp_transport_element(elem) {
        return None;
    }

    let ufrag = elem
        .attr("ufrag")
        .filter(|v| !v.is_empty())
        .map(str::to_owned);
    let pwd = elem
        .attr("pwd")
        .filter(|v| !v.is_empty())
        .map(str::to_owned);

    let fingerprints = elem
        .children()
        .filter(|child| child.name() == "fingerprint" && child.ns() == NS_JINGLE_DTLS)
        .filter_map(parse_fingerprint_element)
        .collect();

    let candidates = elem
        .children()
        .filter(|child| child.name() == "candidate" && child.ns() == NS_JINGLE_ICE_UDP)
        .filter_map(parse_candidate_element)
        .collect();

    Some(IceUdpTransport {
        ufrag,
        pwd,
        fingerprints,
        candidates,
    })
}

/// Build ICE-UDP transport element.
pub fn build_ice_udp_transport_element(transport: &IceUdpTransport) -> Element {
    let mut elem = Element::builder("transport", NS_JINGLE_ICE_UDP).build();

    if let Some(ref ufrag) = transport.ufrag {
        elem.set_attr("ufrag", ufrag);
    }

    if let Some(ref pwd) = transport.pwd {
        elem.set_attr("pwd", pwd);
    }

    for fingerprint in &transport.fingerprints {
        elem.append_child(build_fingerprint_element(fingerprint));
    }

    for candidate in &transport.candidates {
        elem.append_child(build_candidate_element(candidate));
    }

    elem
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xep::xep0320::{DtlsFingerprint, FingerprintSetup};

    #[test]
    fn test_parse_candidate() {
        let elem = Element::builder("candidate", NS_JINGLE_ICE_UDP)
            .attr("foundation", "1")
            .attr("component", "1")
            .attr("protocol", "udp")
            .attr("priority", "2130706431")
            .attr("ip", "192.0.2.1")
            .attr("port", "3478")
            .attr("type", "host")
            .attr("generation", "0")
            .build();

        let parsed = parse_candidate_element(&elem).expect("candidate should parse");
        assert_eq!(parsed.foundation, "1");
        assert_eq!(parsed.component, 1);
        assert_eq!(parsed.port, 3478);
        assert_eq!(parsed.candidate_type, CandidateType::Host);
        assert_eq!(parsed.generation, Some(0));
    }

    #[test]
    fn test_build_and_parse_transport() {
        let candidate = IceUdpCandidate::new(
            "1",
            1,
            "udp",
            2130706431,
            "192.0.2.1",
            3478,
            CandidateType::Host,
        );

        let transport = IceUdpTransport::new()
            .with_fingerprint(
                DtlsFingerprint::new("sha-256", "AA:BB").with_setup(FingerprintSetup::Actpass),
            )
            .with_candidate(candidate);

        let elem = build_ice_udp_transport_element(&transport);
        assert!(is_ice_udp_transport_element(&elem));

        let reparsed = parse_ice_udp_transport_element(&elem).expect("transport should parse");
        assert_eq!(reparsed.fingerprints.len(), 1);
        assert_eq!(reparsed.candidates.len(), 1);
        assert_eq!(reparsed.fingerprints[0].hash, "sha-256");
    }

    #[test]
    fn test_parse_transport_ignores_invalid_candidates() {
        let mut transport = Element::builder("transport", NS_JINGLE_ICE_UDP).build();
        transport.append_child(Element::builder("candidate", NS_JINGLE_ICE_UDP).build());

        let parsed = parse_ice_udp_transport_element(&transport).expect("transport should parse");
        assert!(parsed.candidates.is_empty());
    }
}
