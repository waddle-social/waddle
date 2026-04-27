//! XEP-0320 DTLS-SRTP constants and builders.

use minidom::Element;
use std::fmt;

/// Namespace for Jingle DTLS-SRTP fingerprints.
pub const NS_JINGLE_DTLS: &str = xmpp_parsers::ns::JINGLE_DTLS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DtlsFingerprintHash {
    Sha256,
    Sha384,
    Sha512,
}

impl DtlsFingerprintHash {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sha256 => "sha-256",
            Self::Sha384 => "sha-384",
            Self::Sha512 => "sha-512",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DtlsSetup {
    Actpass,
    Active,
    Passive,
    Holdconn,
}

impl DtlsSetup {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Actpass => "actpass",
            Self::Active => "active",
            Self::Passive => "passive",
            Self::Holdconn => "holdconn",
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct DtlsFingerprint(String);

impl DtlsFingerprint {
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (!value.trim().is_empty()).then_some(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for DtlsFingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("DtlsFingerprint").field(&self.0).finish()
    }
}

/// Build a DTLS fingerprint element.
pub fn build_dtls_fingerprint(
    hash: DtlsFingerprintHash,
    setup: DtlsSetup,
    fingerprint: &DtlsFingerprint,
) -> Element {
    Element::builder("fingerprint", NS_JINGLE_DTLS)
        .attr("hash", hash.as_str())
        .attr("setup", setup.as_str())
        .append(fingerprint.as_str())
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_fingerprint() {
        let fingerprint = DtlsFingerprint::new("AA:BB").expect("fingerprint");
        let elem = build_dtls_fingerprint(
            DtlsFingerprintHash::Sha256,
            DtlsSetup::Actpass,
            &fingerprint,
        );
        assert_eq!(elem.name(), "fingerprint");
        assert_eq!(elem.ns(), NS_JINGLE_DTLS);
        assert_eq!(elem.attr("hash"), Some("sha-256"));
        assert_eq!(elem.attr("setup"), Some("actpass"));
        assert_eq!(elem.text(), "AA:BB");
    }
}
