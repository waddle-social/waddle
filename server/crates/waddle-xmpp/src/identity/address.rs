use std::fmt;

use jid::{BareJid, DomainRef, FullJid, Jid, NodePart, NodeRef, ResourcePart};

use super::domain::CanonicalDomain;
use super::error::IdentityError;

/// The raw parts of an address, split the way RFC 7622 §3.2 and the `jid`
/// parser split it: the first `/` starts the resourcepart, and an `@`
/// before it ends the localpart. Separators are matched before any
/// preparation, so a code point that preparation would decompose into a
/// separator cannot move a boundary.
struct RawParts<'a> {
    localpart: Option<&'a str>,
    domainpart: &'a str,
    resourcepart: Option<&'a str>,
}

impl<'a> RawParts<'a> {
    fn split(input: &'a str) -> Result<Self, IdentityError> {
        if input.is_empty() {
            return Err(IdentityError::Empty);
        }
        let (bare, resourcepart) = match input.split_once('/') {
            Some((bare, resource)) => (bare, Some(resource)),
            None => (input, None),
        };
        let (localpart, domainpart) = match bare.split_once('@') {
            Some((local, domain)) => (Some(local), domain),
            None => (None, bare),
        };
        if domainpart.contains('@') {
            return Err(IdentityError::Malformed);
        }
        Ok(Self {
            localpart,
            domainpart,
            resourcepart,
        })
    }
}

/// Accept a localpart only when the `jid` parser (Nodeprep) leaves it
/// byte-for-byte unchanged. Case, width, compatibility forms, `ß`, final
/// sigma and code points Nodeprep maps to nothing are refused here instead
/// of being folded into another localpart.
pub fn parse_localpart(input: &str) -> Result<NodePart, IdentityError> {
    let prepared = NodePart::new(input).map_err(|_| IdentityError::InvalidLocalpart)?;
    if prepared.as_str() != input {
        return Err(IdentityError::NonCanonicalLocalpart);
    }
    Ok(prepared.into_owned())
}

/// Accept a resourcepart only when the `jid` parser (Resourceprep) leaves
/// it byte-for-byte unchanged, so two accepted resources are distinct
/// exactly when their text is.
pub fn parse_resourcepart(input: &str) -> Result<ResourcePart, IdentityError> {
    let prepared = ResourcePart::new(input).map_err(|_| IdentityError::InvalidResourcepart)?;
    if prepared.as_str() != input {
        return Err(IdentityError::NonCanonicalResourcepart);
    }
    Ok(prepared.into_owned())
}

/// Accept an address decoded from a protocol input, before the `jid`
/// parser could fold it, and return it in canonical form.
///
/// The localpart and resourcepart must be prepared already (see
/// [`parse_localpart`] and [`parse_resourcepart`]); the domainpart must be
/// one canonical spelling, optionally with one trailing dot (see
/// [`CanonicalDomain::parse_exact`]). The result carries the canonical
/// domain, so `a@bücher.example.` becomes `a@xn--bcher-kva.example`.
pub fn parse_jid(input: &str) -> Result<Jid, IdentityError> {
    let parts = RawParts::split(input)?;
    let domain = CanonicalDomain::parse_exact(parts.domainpart)?;
    let localpart = parts.localpart.map(parse_localpart).transpose()?;
    let resourcepart = parts.resourcepart.map(parse_resourcepart).transpose()?;
    Ok(Jid::from_parts(
        localpart.as_deref(),
        domain.as_domain_ref(),
        resourcepart.as_deref(),
    ))
}

/// [`parse_jid`] for an input that must be a bare JID.
pub fn parse_bare_jid(input: &str) -> Result<BareJid, IdentityError> {
    match parse_jid(input)?.try_into_full() {
        Ok(_) => Err(IdentityError::NotBare),
        Err(bare) => Ok(bare),
    }
}

/// [`parse_jid`] for an input that must be a full JID.
pub fn parse_full_jid(input: &str) -> Result<FullJid, IdentityError> {
    parse_jid(input)?
        .try_into_full()
        .map_err(|_| IdentityError::NotFull)
}

/// The canonical form of an address the `jid` parser has already prepared:
/// one trailing dot removed from the domainpart and the domain in A-label
/// form. The parts are re-read from the address text because the parser
/// keeps a trailing dot in the text and offsets its part accessors by it.
///
/// The parser may already have folded the original spelling; only
/// [`parse_jid`], applied to the decoded input, can refuse that. A
/// localpart or resourcepart that is not a fixed point of its own
/// preparation is refused.
pub fn canonical_jid(address: &Jid) -> Result<Jid, IdentityError> {
    let parts = RawParts::split(address.as_str())?;
    let domain = CanonicalDomain::prepare(parts.domainpart)?;
    let localpart = parts.localpart.map(parse_localpart).transpose()?;
    let resourcepart = parts.resourcepart.map(parse_resourcepart).transpose()?;
    Ok(Jid::from_parts(
        localpart.as_deref(),
        domain.as_domain_ref(),
        resourcepart.as_deref(),
    ))
}

/// The text of `address` as presented to a peer: the domain in U-labels
/// (RFC 7622 §3.2.1, [`CanonicalDomain::to_unicode`]), the localpart and
/// resourcepart of its canonical form unchanged.
///
/// The result is attribute text for a stream boundary. It is not parsed
/// back into a [`Jid`]: the `jid` parser applies Nameprep to a U-label,
/// which folds some IDNA2008 labels into a different domain.
pub fn presentation_text(address: &Jid) -> Result<String, IdentityError> {
    let canonical = canonical_jid(address)?;
    let domain = CanonicalDomain::from_domain(canonical.domain())?;
    let mut text = String::new();
    if let Some(localpart) = canonical.node() {
        text.push_str(localpart.as_str());
        text.push('@');
    }
    text.push_str(&domain.to_unicode());
    if let Some(resourcepart) = canonical.resource() {
        text.push('/');
        text.push_str(resourcepart.as_str());
    }
    Ok(text)
}

/// A bare JID in canonical form: the identity of an account, a service or a
/// grant subject.
///
/// Its text is exactly what [`parse_jid`] produces, so it is stable under
/// the `jid` parser, its domain is a [`CanonicalDomain`], and two values are
/// the same identity exactly when their text is equal.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CanonicalBareJid(BareJid);

impl CanonicalBareJid {
    /// Accept a bare JID that is already in canonical form; a trailing dot
    /// or a U-label domain is refused rather than mapped.
    pub fn new(jid: BareJid) -> Result<Self, IdentityError> {
        let canonical = canonical_jid(&Jid::from(jid.clone()))?;
        if canonical.as_str() != jid.as_str() {
            return Err(IdentityError::NonCanonicalDomain);
        }
        Ok(Self(jid))
    }

    /// Accept a decoded bare JID input (see [`parse_jid`]).
    pub fn parse(input: &str) -> Result<Self, IdentityError> {
        parse_bare_jid(input).map(Self)
    }

    /// Accept text that must already be the canonical text of a bare JID.
    pub fn parse_exact(input: &str) -> Result<Self, IdentityError> {
        let jid = Self::parse(input)?;
        if jid.as_str() != input {
            return Err(IdentityError::NonCanonicalDomain);
        }
        Ok(jid)
    }

    /// The canonical form of a bare JID the `jid` parser has already
    /// prepared (see [`canonical_jid`]).
    pub fn canonicalize(jid: &BareJid) -> Result<Self, IdentityError> {
        match canonical_jid(&Jid::from(jid.clone()))?.try_into_full() {
            Ok(_) => Err(IdentityError::NotBare),
            Err(bare) => Ok(Self(bare)),
        }
    }

    /// The account-shaped address `localpart@domain`, accepting the
    /// localpart only when preparation would leave it unchanged.
    pub fn from_parts(localpart: &str, domain: &CanonicalDomain) -> Result<Self, IdentityError> {
        let localpart = parse_localpart(localpart)?;
        Ok(Self(BareJid::from_parts(
            Some(&localpart),
            domain.as_domain_ref(),
        )))
    }

    pub(super) fn from_node(localpart: &NodeRef, domain: &CanonicalDomain) -> Self {
        Self(BareJid::from_parts(Some(localpart), domain.as_domain_ref()))
    }

    /// The localpart, if any.
    pub fn localpart(&self) -> Option<&NodeRef> {
        self.0.node()
    }

    /// The canonical domainpart.
    pub fn domain(&self) -> &DomainRef {
        self.0.domain()
    }

    /// The canonical text.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub fn as_bare_jid(&self) -> &BareJid {
        &self.0
    }

    pub fn into_bare_jid(self) -> BareJid {
        self.0
    }
}

impl fmt::Display for CanonicalBareJid {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl From<CanonicalBareJid> for BareJid {
    fn from(jid: CanonicalBareJid) -> Self {
        jid.0
    }
}
