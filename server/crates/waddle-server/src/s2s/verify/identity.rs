//! Reference identities and how presented identifiers match them
//! (RFC 6120 §13.7.2.1, RFC 9525 §6).
//!
//! The reference identity of a stream is its source domain in the identity
//! profile's canonical form: lowercase A-labels. A U-label spelling and its
//! A-label spelling are one [`CanonicalDomain`], so both select the same
//! presented identifiers. The reference identity never comes from DNS, the
//! socket address or the certificate.

use rustls::pki_types::{DnsName, ServerName};
use waddle_xmpp::identity::CanonicalDomain;
use webpki::EndEntityCert;

use super::error::{TrustError, TrustErrorKind, VerifyError};
use super::facts::{IdentifierError, SrvName, XmppIdentifiers};
use super::trust::TrustProfileKind;

/// The SRV service label of server-to-server streams (RFC 6120 §13.7.1.2.1).
/// It carries no `_tcp` label (RFC 4985 §2), and direct TLS (XEP-0368) uses
/// the same identifier although its SRV record is `_xmpps-server`.
const XMPP_SERVER_SERVICE: &str = "_xmpp-server";

/// The domain a peer certificate must prove.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceIdentity {
    domain: CanonicalDomain,
    dns_name: DnsName<'static>,
}

impl ReferenceIdentity {
    /// The reference identity for `domain`.
    ///
    /// Only a fully qualified DNS name can name a federation peer (ADR-0020
    /// D3); an IP literal, a numeric IPv4 form or a single-label name is
    /// refused here, before any handshake.
    pub fn new(domain: CanonicalDomain) -> Result<Self, TrustError> {
        if !domain.kind().names_federation_peer() {
            return Err(TrustError::new(TrustErrorKind::NotAPeerDomain));
        }
        let dns_name = DnsName::try_from(domain.as_str().to_owned())
            .map_err(|_| TrustError::new(TrustErrorKind::NotAPeerDomain))?;
        Ok(Self { domain, dns_name })
    }

    /// The canonical source domain.
    pub fn domain(&self) -> &CanonicalDomain {
        &self.domain
    }

    /// The TLS server name for an initiating connection: the source domain,
    /// which is also what the Server Name Indication carries (XEP-0368).
    pub fn server_name(&self) -> ServerName<'static> {
        ServerName::DnsName(self.dns_name.clone())
    }

    /// Whether `server_name` is this identity, compared as RFC 9525 §6.3
    /// compares A-labels.
    pub(super) fn is_server_name(&self, server_name: &ServerName<'_>) -> bool {
        match server_name {
            ServerName::DnsName(name) => name.as_ref().eq_ignore_ascii_case(self.domain.as_str()),
            _ => false,
        }
    }
}

/// Which presented identifier matched the reference identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MatchedIdentifier {
    /// A dNSName entry, exact or with a wildcard as its whole left-most
    /// label.
    DnsId,
    /// An `_xmpp-server` SRVName entry (RFC 4985). Matched on a private-CA
    /// profile only.
    SrvId,
    /// An `id-on-xmppAddr` entry (RFC 6120 §13.7.1.4). Matched on a
    /// private-CA profile only.
    XmppAddr,
}

/// Match the end-entity certificate against the reference identity.
///
/// `end_entity` must be the certificate webpki verified, and `identifiers`
/// the XMPP identifiers read from the same bytes.
///
/// On the public-WebPKI profile only DNS-IDs are matched. The TLS Baseline
/// Requirements do not let a publicly trusted CA issue an `otherName`
/// entry, so no public CA has validated an SRV-ID or XmppAddr, and an entry
/// of either form in a public certificate proves nothing.
///
/// `name_constrained` tells whether any CA in the verified path carries a
/// nameConstraints extension. webpki applies dNSName constraints to dNSName
/// entries only, and nothing applies RFC 4985 SRVName constraints, so under
/// a constrained path the SRV-ID and XmppAddr forms are not matched on a
/// private-CA profile either: an issuer limited to one DNS subtree could
/// otherwise name any domain through them.
pub(super) fn match_reference(
    reference: &ReferenceIdentity,
    end_entity: &EndEntityCert<'_>,
    identifiers: &Result<XmppIdentifiers, IdentifierError>,
    profile: TrustProfileKind,
    name_constrained: bool,
) -> Result<MatchedIdentifier, VerifyError> {
    // An invalid XMPP identifier fails the certificate on either profile,
    // even when a DNS-ID would match.
    let identifiers = match identifiers {
        Ok(identifiers) => identifiers,
        Err(IdentifierError::Malformed) => return Err(VerifyError::MalformedIdentifier),
        Err(IdentifierError::Duplicate) => return Err(VerifyError::DuplicateIdentifier),
    };

    // DNS-ID: webpki's matcher on the unmodified certificate. It compares
    // A-labels without regard to ASCII case, accepts `*` only as the whole
    // left-most label standing for exactly one label, ignores presented
    // names it cannot parse, and never consults the subject Common Name.
    match end_entity.verify_is_valid_for_subject_name(&reference.server_name()) {
        Ok(()) => return Ok(MatchedIdentifier::DnsId),
        Err(webpki::Error::CertNotValidForName(_)) => {}
        Err(error) => return Err(VerifyError::Path(error)),
    }

    if profile == TrustProfileKind::PublicWebPki || name_constrained {
        return Err(VerifyError::IdentityMismatch);
    }
    if identifiers
        .srv_names
        .iter()
        .any(|srv_name| srv_name_matches(reference, srv_name))
    {
        return Ok(MatchedIdentifier::SrvId);
    }
    if identifiers
        .xmpp_addrs
        .iter()
        .any(|addr| xmpp_addr_matches(reference, addr))
    {
        return Ok(MatchedIdentifier::XmppAddr);
    }
    Err(VerifyError::IdentityMismatch)
}

/// SRV-ID: the service is `_xmpp-server` and the name is the source domain,
/// label for label. Both sides are lowercase ASCII: the reference is in
/// A-label form and an SRVName is an IA5String (RFC 4985 §2). No wildcard is
/// honoured and no label is stripped, so `_xmpp-server._tcp.<domain>` and
/// `_xmpp-client.<domain>` do not match.
fn srv_name_matches(reference: &ReferenceIdentity, srv_name: &SrvName) -> bool {
    srv_name.service == XMPP_SERVER_SERVICE && srv_name.name == reference.domain.as_str()
}

/// XmppAddr: a UTF8String holding a JID. Only a bare domain can name a
/// server, so an address with a localpart or resourcepart never matches.
/// The value is compared in the canonical domain form after lowercasing its
/// ASCII letters; any other spelling the identity profile would have to map
/// is not this domain.
fn xmpp_addr_matches(reference: &ReferenceIdentity, addr: &str) -> bool {
    if addr.contains(['@', '/']) || addr.ends_with('.') {
        return false;
    }
    CanonicalDomain::parse_exact(&addr.to_ascii_lowercase())
        .is_ok_and(|domain| domain == reference.domain)
}
