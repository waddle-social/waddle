//! The typed identity of a sender on an inbound S2S stream.

use jid::{FullJid, Jid};
use thiserror::Error;
use uuid::Uuid;
use waddle_xmpp::identity::{canonical_jid, CanonicalBareJid, CanonicalDomain};

use crate::s2s::conn::{AuthenticationStrength, StreamDirection, VerifiedPeerStream};
use crate::s2s::verify::TrustProfileKind;

/// An account or service at a remote domain, as an inbound S2S stream
/// proved it (ADR-0020, "Remote principal").
///
/// It is built from the `from` address of a stanza on a
/// [`VerifiedPeerStream`] whose remote domain is that address's domain, and
/// carries the stream's authentication strength and trust-profile kind to
/// whatever decides a protected effect. The replica that verified the
/// stream may hand those facts to the replica that holds the room the
/// stanza addresses ([`CarriedPrincipal`]), which builds the same principal
/// from them ([`Self::carried`]); nothing else builds one. It is never a local session,
/// account or grant: domain authentication does not grant access to
/// anything, and a remote account's grants are decided at the effect
/// boundary from its canonical bare JID, its home, the strength and the
/// trust-home list (D8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemotePrincipal {
    address: Jid,
    account: CanonicalBareJid,
    home: CanonicalDomain,
    strength: AuthenticationStrength,
    trust_profile: Option<TrustProfileKind>,
    connection: u64,
    generation: Uuid,
}

/// What an inbound S2S stream proved of a stanza's sender, as the replica
/// that verified the stream hands it to another replica of this deployment
/// with the stanza (ADR-0020 D10): the facts of the stream and nothing
/// decided from them.
///
/// Only the cluster relay carries one, between replicas of one deployment
/// that authenticate each other; it never comes from a peer server. Whether
/// the sender's domain is admitted, and what its grants are honoured as, is
/// decided again by the replica that receives it, under the S2S snapshot
/// that replica runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CarriedPrincipal {
    /// The address the stanza came from, in canonical form.
    pub address: Jid,
    /// How the sender's home domain authenticated on the stream.
    pub strength: AuthenticationStrength,
    /// The trust profile its certificate was verified under, if it
    /// presented one.
    pub trust_profile: Option<TrustProfileKind>,
    /// The configuration snapshot the stream was admitted under.
    pub generation: Uuid,
}

/// Why an address cannot be a remote principal of a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum PrincipalError {
    /// Only an inbound stream carries stanzas from its remote domain.
    #[error("the stream does not carry stanzas from its remote domain")]
    NotInbound,
    /// The address is not in the identity profile's canonical form.
    #[error("the address is not in canonical form")]
    NotCanonical,
    /// The address is not at the stream's authenticated remote domain.
    #[error("the address is not at the stream's authenticated domain")]
    WrongDomain,
}

impl RemotePrincipal {
    /// The principal `address` names on the inbound `stream`. The address
    /// must be canonical and at exactly the stream's remote domain: a
    /// subdomain or parent domain is another domain the stream did not
    /// authenticate.
    pub fn from_stream(stream: &VerifiedPeerStream, address: &Jid) -> Result<Self, PrincipalError> {
        if stream.direction() != StreamDirection::Inbound {
            return Err(PrincipalError::NotInbound);
        }
        let canonical = canonical_jid(address).map_err(|_| PrincipalError::NotCanonical)?;
        if canonical.as_str() != address.as_str() {
            return Err(PrincipalError::NotCanonical);
        }
        if canonical.domain().as_str() != stream.remote().as_str() {
            return Err(PrincipalError::WrongDomain);
        }
        let account =
            CanonicalBareJid::new(canonical.to_bare()).map_err(|_| PrincipalError::NotCanonical)?;
        Ok(Self {
            address: canonical,
            account,
            home: stream.remote().clone(),
            strength: stream.strength(),
            trust_profile: stream.trust_profile(),
            connection: stream.connection(),
            generation: stream.generation(),
        })
    }

    /// The principal another replica of this deployment verified on an
    /// inbound stream and carried here with its stanza. The address must be
    /// canonical, as on a stream; its home is the address's domain, which
    /// the verifying replica matched against the stream's.
    pub fn carried(carried: CarriedPrincipal) -> Result<Self, PrincipalError> {
        let canonical =
            canonical_jid(&carried.address).map_err(|_| PrincipalError::NotCanonical)?;
        if canonical.as_str() != carried.address.as_str() {
            return Err(PrincipalError::NotCanonical);
        }
        let home = CanonicalDomain::from_domain(canonical.domain())
            .map_err(|_| PrincipalError::NotCanonical)?;
        let account =
            CanonicalBareJid::new(canonical.to_bare()).map_err(|_| PrincipalError::NotCanonical)?;
        Ok(Self {
            address: canonical,
            account,
            home,
            strength: carried.strength,
            trust_profile: carried.trust_profile,
            // The connection is another replica's.
            connection: 0,
            generation: carried.generation,
        })
    }

    /// The facts of this principal's stream, for the replica that holds
    /// the room its stanza addresses.
    pub fn to_carried(&self) -> CarriedPrincipal {
        CarriedPrincipal {
            address: self.address.clone(),
            strength: self.strength,
            trust_profile: self.trust_profile,
            generation: self.generation,
        }
    }

    /// The address the stanza came from, full or bare, in canonical form.
    pub fn address(&self) -> &Jid {
        &self.address
    }

    /// The full JID, when the sender is a bound resource or occupant.
    pub fn full(&self) -> Option<FullJid> {
        self.address.clone().try_into_full().ok()
    }

    /// The canonical bare JID: the subject of grants and bootstrap.
    pub fn account(&self) -> &CanonicalBareJid {
        &self.account
    }

    /// The home domain: the stream's authenticated remote domain.
    pub fn home(&self) -> &CanonicalDomain {
        &self.home
    }

    /// How the home domain authenticated.
    pub fn strength(&self) -> AuthenticationStrength {
        self.strength
    }

    /// The trust profile the home's certificate was verified under; `None`
    /// when the home authenticated by dialback without presenting a
    /// certificate.
    pub fn trust_profile(&self) -> Option<TrustProfileKind> {
        self.trust_profile
    }

    /// The connection that carried the stanza.
    pub fn connection(&self) -> u64 {
        self.connection
    }

    /// The configuration snapshot the stream was admitted under.
    pub fn generation(&self) -> Uuid {
        self.generation
    }
}
