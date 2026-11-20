//! Typed `jabber:server` stream headers (RFC 6120 §4.7).

use std::fmt;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use jid::{DomainPart, Jid};

/// Longest stream ID accepted from a peer. Longer values are ignored as if
/// absent; the header byte ceiling bounds them in any case.
const MAX_STREAM_ID_BYTES: usize = 256;

/// Longest `xml:lang` value accepted from a peer.
const MAX_LANGUAGE_BYTES: usize = 64;

/// A server-to-server stream header address (`from` or `to`).
///
/// Server-to-server stream addresses are domain-only JIDs. A present value
/// that is not a valid, prepared domainpart is kept as [`Self::Invalid`]
/// without its text so the negotiation can choose the RFC 6120 error
/// condition without reflecting peer input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeaderAddress {
    /// The attribute was not present.
    Absent,
    /// The attribute was present but empty, not a JID, or not domain-only.
    Invalid,
    /// A prepared domain-only address.
    Domain(DomainPart),
}

impl HeaderAddress {
    /// Classify a raw attribute value.
    pub fn parse(value: Option<&str>) -> Self {
        let Some(value) = value else {
            return Self::Absent;
        };
        if value.is_empty() {
            return Self::Invalid;
        }
        match Jid::new(value) {
            Ok(jid) if jid.node().is_none() && jid.resource().is_none() => {
                Self::Domain(DomainPart::from(jid.domain()))
            }
            _ => Self::Invalid,
        }
    }

    /// The domain, when the address is a valid domain-only JID.
    pub fn domain(&self) -> Option<&DomainPart> {
        match self {
            Self::Domain(domain) => Some(domain),
            Self::Absent | Self::Invalid => None,
        }
    }
}

impl From<DomainPart> for HeaderAddress {
    fn from(domain: DomainPart) -> Self {
        Self::Domain(domain)
    }
}

impl From<Option<DomainPart>> for HeaderAddress {
    fn from(domain: Option<DomainPart>) -> Self {
        domain.map_or(Self::Absent, Self::Domain)
    }
}

/// An XMPP stream `version` (RFC 6120 §4.7.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StreamVersion {
    major: u32,
    minor: u32,
}

impl StreamVersion {
    /// XMPP 1.0, the version this codec speaks.
    pub const V1_0: Self = Self { major: 1, minor: 0 };

    /// Parse `<major>.<minor>`, comparing the parts as integers and ignoring
    /// leading zeros.
    pub fn parse(value: &str) -> Option<Self> {
        let (major, minor) = value.split_once('.')?;
        Some(Self {
            major: parse_version_part(major)?,
            minor: parse_version_part(minor)?,
        })
    }

    /// Major version number.
    pub fn major(self) -> u32 {
        self.major
    }

    /// Minor version number.
    pub fn minor(self) -> u32 {
        self.minor
    }

    /// Whether the peer advertises the stream features, TLS, SASL and
    /// stream errors defined by XMPP 1.0.
    pub fn supports_xmpp_1_0(self) -> bool {
        self >= Self::V1_0
    }
}

fn parse_version_part(part: &str) -> Option<u32> {
    if part.is_empty() || part.len() > 9 || !part.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    part.parse().ok()
}

impl fmt::Display for StreamVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// A stream ID (RFC 6120 §4.7.3).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StreamId(String);

impl StreamId {
    /// A fresh, unpredictable stream ID for a response stream header.
    pub fn random() -> Self {
        let bytes: [u8; 18] = rand::random();
        Self(URL_SAFE_NO_PAD.encode(bytes))
    }

    /// Accept a peer-supplied ID, or `None` when it is empty or too long.
    pub fn parse(value: &str) -> Option<Self> {
        if value.is_empty() || value.len() > MAX_STREAM_ID_BYTES {
            return None;
        }
        Some(Self(value.to_owned()))
    }

    /// The ID as sent on the wire.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for StreamId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A stream default language (`xml:lang`, RFC 6120 §4.7.4).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StreamLanguage(String);

impl StreamLanguage {
    /// English, the default language of Waddle-generated streams.
    pub fn english() -> Self {
        Self("en".to_owned())
    }

    /// Accept a language tag made of 1–8 character ASCII alphanumeric
    /// subtags separated by single hyphens.
    pub fn parse(value: &str) -> Option<Self> {
        if value.is_empty() || value.len() > MAX_LANGUAGE_BYTES {
            return None;
        }
        let well_formed = value.split('-').all(|subtag| {
            (1..=8).contains(&subtag.len()) && subtag.bytes().all(|b| b.is_ascii_alphanumeric())
        });
        well_formed.then(|| Self(value.to_owned()))
    }

    /// The tag as sent on the wire.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for StreamLanguage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The attributes of a `jabber:server` stream header.
///
/// The decoder has already checked the stream namespace, its prefix and the
/// content namespace before producing this value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamHeader {
    pub from: HeaderAddress,
    pub to: HeaderAddress,
    pub id: Option<StreamId>,
    pub version: Option<StreamVersion>,
    pub lang: Option<StreamLanguage>,
}

impl StreamHeader {
    /// An initial stream header sent by the initiating server.
    pub fn initial(from: DomainPart, to: DomainPart, lang: StreamLanguage) -> Self {
        Self {
            from: HeaderAddress::Domain(from),
            to: HeaderAddress::Domain(to),
            id: None,
            version: Some(StreamVersion::V1_0),
            lang: Some(lang),
        }
    }

    /// A response stream header sent by the receiving server, with a fresh
    /// stream ID.
    pub fn response(from: DomainPart, to: Option<DomainPart>, lang: StreamLanguage) -> Self {
        Self {
            from: HeaderAddress::Domain(from),
            to: HeaderAddress::from(to),
            id: Some(StreamId::random()),
            version: Some(StreamVersion::V1_0),
            lang: Some(lang),
        }
    }
}
