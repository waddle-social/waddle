//! XEP-0320: Use of DTLS-SRTP in Jingle Sessions
//!
//! Supports DTLS fingerprint signaling inside Jingle transports.

use minidom::Element;

/// Namespace for XEP-0320 DTLS fingerprint signaling.
pub const NS_JINGLE_DTLS: &str = "urn:xmpp:jingle:apps:dtls:0";

/// DTLS setup role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FingerprintSetup {
    Actpass,
    Active,
    Passive,
    Holdconn,
}

impl FingerprintSetup {
    /// Parse setup attribute value.
    pub fn from_str_attr(value: &str) -> Option<Self> {
        match value {
            "actpass" => Some(Self::Actpass),
            "active" => Some(Self::Active),
            "passive" => Some(Self::Passive),
            "holdconn" => Some(Self::Holdconn),
            _ => None,
        }
    }

    /// Convert setup to attribute string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Actpass => "actpass",
            Self::Active => "active",
            Self::Passive => "passive",
            Self::Holdconn => "holdconn",
        }
    }
}

/// DTLS fingerprint metadata.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DtlsFingerprint {
    /// Hash function name (e.g. `sha-256`).
    pub hash: String,
    /// Optional setup role.
    pub setup: Option<FingerprintSetup>,
    /// Colon-separated fingerprint value.
    pub value: String,
}

impl DtlsFingerprint {
    /// Create a new DTLS fingerprint.
    pub fn new(hash: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            hash: hash.into(),
            setup: None,
            value: value.into(),
        }
    }

    /// Add setup role.
    pub fn with_setup(mut self, setup: FingerprintSetup) -> Self {
        self.setup = Some(setup);
        self
    }
}

/// Check whether an element is a DTLS fingerprint element.
pub fn is_fingerprint_element(elem: &Element) -> bool {
    elem.name() == "fingerprint" && elem.ns() == NS_JINGLE_DTLS
}

/// Parse `<fingerprint xmlns='urn:xmpp:jingle:apps:dtls:0'/>`.
pub fn parse_fingerprint_element(elem: &Element) -> Option<DtlsFingerprint> {
    if !is_fingerprint_element(elem) {
        return None;
    }

    let hash = elem.attr("hash").filter(|v| !v.is_empty())?.to_owned();
    let value = elem.text();
    if value.is_empty() {
        return None;
    }

    let setup = elem.attr("setup").and_then(FingerprintSetup::from_str_attr);

    Some(DtlsFingerprint { hash, setup, value })
}

/// Build a DTLS fingerprint element.
pub fn build_fingerprint_element(fingerprint: &DtlsFingerprint) -> Element {
    let mut elem = Element::builder("fingerprint", NS_JINGLE_DTLS)
        .attr("hash", fingerprint.hash.as_str())
        .append(fingerprint.value.as_str())
        .build();

    if let Some(setup) = fingerprint.setup {
        elem.set_attr("setup", setup.as_str());
    }

    elem
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_fingerprint_element() {
        let elem = Element::builder("fingerprint", NS_JINGLE_DTLS)
            .attr("hash", "sha-256")
            .attr("setup", "actpass")
            .append("12:34:56")
            .build();

        let parsed = parse_fingerprint_element(&elem).expect("fingerprint should parse");
        assert_eq!(parsed.hash, "sha-256");
        assert_eq!(parsed.value, "12:34:56");
        assert_eq!(parsed.setup, Some(FingerprintSetup::Actpass));
    }

    #[test]
    fn test_parse_fingerprint_requires_hash_and_value() {
        let missing_hash = Element::builder("fingerprint", NS_JINGLE_DTLS)
            .append("12:34")
            .build();
        assert!(parse_fingerprint_element(&missing_hash).is_none());

        let missing_value = Element::builder("fingerprint", NS_JINGLE_DTLS)
            .attr("hash", "sha-256")
            .build();
        assert!(parse_fingerprint_element(&missing_value).is_none());
    }

    #[test]
    fn test_build_fingerprint_element() {
        let fingerprint =
            DtlsFingerprint::new("sha-256", "AA:BB:CC").with_setup(FingerprintSetup::Passive);
        let elem = build_fingerprint_element(&fingerprint);

        assert!(is_fingerprint_element(&elem));
        assert_eq!(elem.attr("hash"), Some("sha-256"));
        assert_eq!(elem.attr("setup"), Some("passive"));
        assert_eq!(elem.text(), "AA:BB:CC");
    }
}
