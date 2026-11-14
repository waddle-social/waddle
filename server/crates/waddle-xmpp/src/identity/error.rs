use thiserror::Error;

/// Why an identity input is outside the identity profile.
///
/// Values carry no input material, so they can be logged and mapped to
/// protocol errors without exposing the refused address.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq, Hash)]
pub enum IdentityError {
    #[error("address is empty")]
    Empty,
    #[error("address is not a well-formed JID")]
    Malformed,
    #[error("domainpart cannot be prepared")]
    InvalidDomain,
    #[error("domainpart is not written in a canonical spelling")]
    NonCanonicalDomain,
    #[error("localpart cannot be prepared")]
    InvalidLocalpart,
    #[error("localpart would be changed by preparation")]
    NonCanonicalLocalpart,
    #[error("resourcepart cannot be prepared")]
    InvalidResourcepart,
    #[error("resourcepart would be changed by preparation")]
    NonCanonicalResourcepart,
    #[error("address has a resourcepart where a bare JID is required")]
    NotBare,
    #[error("address has no resourcepart where a full JID is required")]
    NotFull,
    #[error("address has no localpart where an account is required")]
    MissingLocalpart,
    #[error("localpart is outside the account profile")]
    OutsideAccountProfile,
}

impl IdentityError {
    /// A stable code for logs and reports.
    pub fn code(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Malformed => "malformed",
            Self::InvalidDomain => "invalid_domain",
            Self::NonCanonicalDomain => "non_canonical_domain",
            Self::InvalidLocalpart => "invalid_localpart",
            Self::NonCanonicalLocalpart => "non_canonical_localpart",
            Self::InvalidResourcepart => "invalid_resourcepart",
            Self::NonCanonicalResourcepart => "non_canonical_resourcepart",
            Self::NotBare => "not_bare",
            Self::NotFull => "not_full",
            Self::MissingLocalpart => "missing_localpart",
            Self::OutsideAccountProfile => "outside_account_profile",
        }
    }
}
