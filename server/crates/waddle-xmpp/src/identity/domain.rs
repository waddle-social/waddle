use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr};

use idna::uts46::{AsciiDenyList, DnsLength, Hyphens, Uts46};
use jid::{DomainPart, DomainRef};

use super::error::IdentityError;

/// What a canonical domain names, as [`CanonicalDomain::kind`] classifies
/// it. Every domainpart a JID can carry has exactly one kind.
///
/// Only [`DomainKind::Dns`] can name a server-to-server peer (ADR-0020 D3);
/// see [`DomainKind::names_federation_peer`]. The other kinds are valid in
/// local addresses, and every place that admits, routes, discovers or dials
/// a remote domain refuses them by this classification rather than by
/// re-reading the text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DomainKind {
    /// A fully qualified DNS name: two or more labels, the final one not a
    /// number (see [`DomainKind::NumericIpv4Form`]).
    Dns,
    /// A DNS name of one label, such as `localhost`. A resolver could
    /// complete it from its search list.
    SingleLabel,
    /// A name a resolver or URL parser reads as an IPv4 address in a form
    /// other than the dotted quad: its final label is a number, either all
    /// decimal digits (`2130706433`, `127.1`, the leading-zero octal form
    /// `0177.0.0.1`) or `0x` followed by hexadecimal digits (`0x7f.1`,
    /// `0x7f000001`). This is the WHATWG URL Standard's "ends in a number"
    /// rule, applied to the canonical (lowercase) text.
    NumericIpv4Form,
    /// An IPv4 address in dotted-quad form (RFC 7622 §3.2 `IPv4address`).
    Ipv4,
    /// A bracketed IPv6 address (RFC 3986 `IP-literal`).
    Ipv6,
}

impl DomainKind {
    /// Whether a domain of this kind can name a server-to-server peer: only
    /// a fully qualified DNS name. An IP address or numeric IPv4 form has no
    /// source domain for a certificate to prove and would let an address
    /// choose a socket without discovery; a resolver could complete a
    /// single-label name from its search list.
    pub fn names_federation_peer(self) -> bool {
        matches!(self, Self::Dns)
    }

    /// Whether this is one of the two IP-address forms of RFC 7622 §3.2.
    pub fn is_ip_literal(self) -> bool {
        matches!(self, Self::Ipv4 | Self::Ipv6)
    }

    /// A stable code for logs and reports.
    pub fn code(self) -> &'static str {
        match self {
            Self::Dns => "dns",
            Self::SingleLabel => "single_label",
            Self::NumericIpv4Form => "numeric_ipv4_form",
            Self::Ipv4 => "ipv4",
            Self::Ipv6 => "ipv6",
        }
    }
}

/// A domainpart in the profile's one canonical form.
///
/// - One trailing label separator is removed before anything else
///   (RFC 7622 §3.2).
/// - A DNS name is prepared under IDNA2008 with UTS #46 non-transitional
///   processing, STD3 ASCII rules (letters, digits and hyphens), the
///   IDNA2008 hyphen restrictions and DNS length limits, and is written in
///   lowercase A-label form. `bücher.example` and `xn--bcher-kva.example`
///   are the same canonical domain, `xn--bcher-kva.example`.
/// - An IPv4 address keeps its dotted-quad text and an IPv6 address its
///   bracketed RFC 5952 text.
/// - Every canonical domain has one [`DomainKind`]. A name whose final label
///   is a number is a [`DomainKind::NumericIpv4Form`], never a
///   [`DomainKind::Dns`] name, so no DNS name can be another spelling of an
///   IPv4 address. Such a name is a valid domainpart and keeps its own text;
///   it is not rewritten to the dotted quad a resolver would read.
///
/// The canonical text is accepted unchanged by the `jid` parser, so it is
/// also a valid [`DomainPart`]. The A-label form is the comparison form for
/// admission lists, reference identities, routing and stored identities; a
/// display may use [`CanonicalDomain::to_unicode`].
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CanonicalDomain(DomainPart);

impl CanonicalDomain {
    /// Accept a domainpart decoded from a protocol input.
    ///
    /// The text must be one canonical spelling of the domain, optionally
    /// followed by one trailing dot: every label is either its canonical
    /// A-label or its canonical U-label. Any spelling that preparation would
    /// change further (letter case, width, compatibility forms, other label
    /// separators, an IP address in non-canonical text) is refused instead
    /// of being folded.
    pub fn parse_exact(input: &str) -> Result<Self, IdentityError> {
        let unrooted = strip_root(input)?;
        if let Some(literal) = ip_literal(unrooted) {
            let domain = literal?;
            return if domain.as_str() == unrooted {
                Ok(domain)
            } else {
                Err(IdentityError::NonCanonicalDomain)
            };
        }
        let ascii = dns_to_ascii(unrooted)?;
        let unicode = dns_to_unicode(&ascii)?;
        if !spelled_as(unrooted, &ascii, &unicode) {
            return Err(IdentityError::NonCanonicalDomain);
        }
        Self::from_parser_stable(ascii)
    }

    /// Prepare a domain from operator configuration under RFC 7622 §3.2,
    /// applying the IDNA2008 mappings (case, width) that an input spelling
    /// may not rely on. An entry that cannot be prepared is an error, never
    /// a skipped entry.
    pub fn prepare(input: &str) -> Result<Self, IdentityError> {
        let unrooted = strip_root(input)?;
        if let Some(literal) = ip_literal(unrooted) {
            return literal;
        }
        Self::from_parser_stable(dns_to_ascii(unrooted)?)
    }

    /// Accept text that is already the canonical text of a domain: no
    /// trailing dot, lowercase, A-labels. This is the rule for configured
    /// domains this deployment serves and for stored identities, where any
    /// other spelling is refused rather than mapped.
    pub fn parse_canonical(input: &str) -> Result<Self, IdentityError> {
        let domain = Self::parse_exact(input)?;
        if domain.as_str() != input {
            return Err(IdentityError::NonCanonicalDomain);
        }
        Ok(domain)
    }

    /// The canonical form of a domainpart the `jid` parser has already
    /// prepared. The parser keeps a trailing dot and U-labels; both are
    /// mapped here. Spellings the parser has already folded cannot be
    /// detected afterwards, which is why protocol inputs go through
    /// [`CanonicalDomain::parse_exact`] before parsing.
    pub fn from_domain(domain: &DomainRef) -> Result<Self, IdentityError> {
        Self::prepare(domain.as_str())
    }

    fn from_parser_stable(text: String) -> Result<Self, IdentityError> {
        let part = DomainPart::new(&text).map_err(|_| IdentityError::InvalidDomain)?;
        if part.as_str() != text {
            return Err(IdentityError::InvalidDomain);
        }
        Ok(Self(part.into_owned()))
    }

    /// The canonical text.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// The canonical domain as a typed domainpart.
    pub fn as_domain_ref(&self) -> &DomainRef {
        &self.0
    }

    /// The canonical domain as an owned typed domainpart.
    pub fn into_domain_part(self) -> DomainPart {
        self.0
    }

    /// What the domain names. The classification is a function of the
    /// canonical text alone; see [`DomainKind`].
    pub fn kind(&self) -> DomainKind {
        let text = self.as_str();
        if text.starts_with('[') {
            DomainKind::Ipv6
        } else if text.parse::<Ipv4Addr>().is_ok() {
            DomainKind::Ipv4
        } else if ends_in_a_number(text) {
            DomainKind::NumericIpv4Form
        } else if text.contains('.') {
            DomainKind::Dns
        } else {
            DomainKind::SingleLabel
        }
    }

    /// The U-label presentation of a name, for display. IP literals and
    /// all-ASCII names are returned as they are.
    pub fn to_unicode(&self) -> String {
        if self.kind().is_ip_literal() {
            return self.as_str().to_owned();
        }
        dns_to_unicode(self.as_str()).unwrap_or_else(|_| self.as_str().to_owned())
    }

    /// Whether this domain is `ancestor` or a subdomain of it, compared
    /// label by label: `muc.example.net` is within `example.net`,
    /// `badexample.net` is not. Only DNS and single-label names have
    /// subdomains; IP literals and numeric IPv4 forms are within only
    /// themselves.
    pub fn is_within(&self, ancestor: &CanonicalDomain) -> bool {
        if self == ancestor {
            return true;
        }
        let has_labels =
            |kind: DomainKind| matches!(kind, DomainKind::Dns | DomainKind::SingleLabel);
        if !has_labels(self.kind()) || !has_labels(ancestor.kind()) {
            return false;
        }
        self.as_str()
            .strip_suffix(ancestor.as_str())
            .is_some_and(|prefix| prefix.len() > 1 && prefix.ends_with('.'))
    }
}

impl fmt::Display for CanonicalDomain {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl From<CanonicalDomain> for DomainPart {
    fn from(domain: CanonicalDomain) -> Self {
        domain.0
    }
}

/// Remove one trailing label separator (RFC 7622 §3.2), before any other
/// step.
fn strip_root(input: &str) -> Result<&str, IdentityError> {
    let unrooted = input.strip_suffix('.').unwrap_or(input);
    if unrooted.is_empty() {
        return Err(IdentityError::Empty);
    }
    Ok(unrooted)
}

/// `None` when `text` is not written as an IP literal, otherwise its
/// canonical form or why it is not one.
fn ip_literal(text: &str) -> Option<Result<CanonicalDomain, IdentityError>> {
    if let Some(inner) = text.strip_prefix('[') {
        let literal = inner
            .strip_suffix(']')
            .and_then(|inner| inner.parse::<Ipv6Addr>().ok())
            .ok_or(IdentityError::InvalidDomain)
            .and_then(|address| CanonicalDomain::from_parser_stable(format!("[{address}]")));
        return Some(literal);
    }
    text.parse::<Ipv4Addr>()
        .ok()
        .map(|address| CanonicalDomain::from_parser_stable(address.to_string()))
}

/// The WHATWG URL Standard's "ends in a number checker" for canonical text,
/// which never ends in a dot: the final label is non-empty and all ASCII
/// digits (decimal, or octal with a leading zero), or `0x` followed by zero
/// or more hexadecimal digits. A resolver or URL parser reads such a name as
/// an IPv4 address.
fn ends_in_a_number(text: &str) -> bool {
    let last = text.rsplit('.').next().unwrap_or(text);
    if !last.is_empty() && last.bytes().all(|byte| byte.is_ascii_digit()) {
        return true;
    }
    last.strip_prefix("0x")
        .or_else(|| last.strip_prefix("0X"))
        .is_some_and(|digits| digits.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn dns_to_ascii(text: &str) -> Result<String, IdentityError> {
    Uts46::new()
        .to_ascii(
            text.as_bytes(),
            AsciiDenyList::STD3,
            Hyphens::Check,
            DnsLength::Verify,
        )
        .map(|ascii| ascii.into_owned())
        .map_err(|_| IdentityError::InvalidDomain)
}

fn dns_to_unicode(ascii: &str) -> Result<String, IdentityError> {
    let (unicode, result) =
        Uts46::new().to_unicode(ascii.as_bytes(), AsciiDenyList::STD3, Hyphens::Check);
    result
        .map(|()| unicode.into_owned())
        .map_err(|_| IdentityError::InvalidDomain)
}

/// Whether each label of `input` is the corresponding A-label or U-label.
fn spelled_as(input: &str, ascii: &str, unicode: &str) -> bool {
    let mut input_labels = input.split('.');
    let mut ascii_labels = ascii.split('.');
    let mut unicode_labels = unicode.split('.');
    loop {
        match (
            input_labels.next(),
            ascii_labels.next(),
            unicode_labels.next(),
        ) {
            (None, None, None) => return true,
            (Some(label), Some(a_label), Some(u_label)) => {
                if label != a_label && label != u_label {
                    return false;
                }
            }
            _ => return false,
        }
    }
}
