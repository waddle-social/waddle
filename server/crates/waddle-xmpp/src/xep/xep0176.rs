//! XEP-0176 Jingle ICE-UDP constants and validation helpers.

use minidom::Element;
use std::fmt;

/// Namespace for Jingle ICE-UDP transports.
pub const NS_JINGLE_ICE_UDP: &str = xmpp_parsers::ns::JINGLE_ICE_UDP;

#[derive(Clone, PartialEq, Eq)]
pub struct IceUfrag(String);

impl IceUfrag {
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (!value.trim().is_empty()).then_some(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for IceUfrag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("IceUfrag").field(&self.0).finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct IcePassword(String);

impl IcePassword {
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (!value.trim().is_empty()).then_some(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for IcePassword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("IcePassword(<redacted>)")
    }
}

/// Build an empty ICE-UDP transport element.
pub fn build_ice_udp_transport(ufrag: Option<&IceUfrag>, pwd: Option<&IcePassword>) -> Element {
    let mut elem = Element::builder("transport", NS_JINGLE_ICE_UDP).build();
    if let Some(ufrag) = ufrag {
        elem.set_attr("ufrag", ufrag.as_str());
    }
    if let Some(pwd) = pwd {
        elem.set_attr("pwd", pwd.as_str());
    }
    elem
}

/// Return true when this transport includes candidates and therefore has
/// the ICE credentials XEP-0176 requires.
pub fn ice_candidates_have_credentials(transport: &Element) -> bool {
    if transport.name() != "transport" || transport.ns() != NS_JINGLE_ICE_UDP {
        return false;
    }
    let has_candidate = transport
        .children()
        .any(|child| child.name() == "candidate" && child.ns() == NS_JINGLE_ICE_UDP);
    !has_candidate
        || (transport
            .attr("ufrag")
            .is_some_and(|value| !value.is_empty())
            && transport.attr("pwd").is_some_and(|value| !value.is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_require_credentials() {
        let transport = Element::builder("transport", NS_JINGLE_ICE_UDP)
            .append(Element::builder("candidate", NS_JINGLE_ICE_UDP).build())
            .build();
        assert!(!ice_candidates_have_credentials(&transport));

        let transport = Element::builder("transport", NS_JINGLE_ICE_UDP)
            .attr("ufrag", "u")
            .attr("pwd", "p")
            .append(Element::builder("candidate", NS_JINGLE_ICE_UDP).build())
            .build();
        assert!(ice_candidates_have_credentials(&transport));
    }
}
