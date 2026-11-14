//! Why an S2S configuration snapshot failed to load.
//!
//! Errors name the setting and, for list entries, the entry's position.
//! Apart from the TOML parser's own message for a syntax error, they never
//! carry a configured domain, an address, a path or file contents.

use thiserror::Error;
use waddle_xmpp::identity::IdentityError;

use crate::s2s::verify::{TlsConfigError, TrustError};

/// A list or table of the configuration file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigSection {
    LocalDomains,
    Allowlist,
    Denylist,
    DialbackList,
    TrustedHomes,
    RequirePrivateCa,
    Credentials,
    PrivateCa,
    Endpoints,
    Listen,
    Limits,
    Dialback,
}

impl ConfigSection {
    /// The setting's name in the configuration file.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LocalDomains => "local_domains",
            Self::Allowlist => "admission.allow",
            Self::Denylist => "admission.deny",
            Self::DialbackList => "admission.dialback",
            Self::TrustedHomes => "admission.trusted_homes",
            Self::RequirePrivateCa => "admission.require_private_ca",
            Self::Credentials => "credentials",
            Self::PrivateCa => "private_ca",
            Self::Endpoints => "endpoints",
            Self::Listen => "listen",
            Self::Limits => "limits",
            Self::Dialback => "dialback",
        }
    }
}

impl std::fmt::Display for ConfigSection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// An S2S configuration that cannot be used. S2S does not start.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("WADDLE_S2S_ENABLED must be `true` or `false`")]
    InvalidEnabledFlag,
    #[error("WADDLE_S2S_ENABLED is true but WADDLE_S2S_CONFIG_FILE is not set")]
    MissingConfigFile,
    #[error("WADDLE_S2S_CONFIG_FILE is not an absolute path")]
    RelativeConfigFile,
    #[error("the S2S configuration file could not be read")]
    ConfigFileUnreadable,
    #[error("a file named by `{0}` could not be read")]
    MaterialUnreadable(ConfigSection),
    #[error("a path in `{0}` is not absolute")]
    RelativePath(ConfigSection),
    #[error("the S2S configuration file is larger than the limit")]
    ConfigFileTooLarge,
    #[error("a file named by `{0}` is larger than the limit")]
    MaterialTooLarge(ConfigSection),
    #[error("the configuration and its files together exceed the size limit")]
    TotalTooLarge,
    #[error("the S2S configuration file is not valid TOML for this schema: {0}")]
    Syntax(String),
    #[error("`generation` is not a UUID")]
    InvalidGeneration,
    #[error("entry {index} of `{section}` cannot be prepared as a domain: {error}")]
    InvalidDomain {
        section: ConfigSection,
        index: usize,
        error: IdentityError,
    },
    #[error("entry {index} of `{section}` is an IP address, a numeric IPv4 form or a single-label name, which cannot name a federation peer")]
    NotAPeerDomain {
        section: ConfigSection,
        index: usize,
    },
    #[error(
        "entry {index} of `{section}` is outside the `.test` domain, which `peer_scope = \"test\"` requires"
    )]
    OutsideTestDomain {
        section: ConfigSection,
        index: usize,
    },
    #[error("entry {index} of `{section}` repeats an earlier entry")]
    Duplicate {
        section: ConfigSection,
        index: usize,
    },
    #[error("entry {index} of `{section}` is covered by the denylist")]
    Denied {
        section: ConfigSection,
        index: usize,
    },
    #[error("entry {index} of `{section}` names or covers a local domain")]
    LocalDomain {
        section: ConfigSection,
        index: usize,
    },
    #[error("`{0}` has too many entries")]
    TooManyEntries(ConfigSection),
    #[error("entry {index} of `admission.require_private_ca` is not a trusted home")]
    NotTrustedHome { index: usize },
    #[error("entry {index} of `admission.require_private_ca` has no private-CA profile")]
    NoPrivateCaProfile { index: usize },
    #[error("`local_domains` is empty")]
    NoLocalDomains,
    #[error("local domain {index} has no credentials")]
    MissingCredentials { index: usize },
    #[error("entry {index} of `credentials` is for a domain that is not local")]
    UnservedCredentials { index: usize },
    #[error("entry {index} of `credentials` has no certificate")]
    EmptyCertificateChain { index: usize },
    #[error("entry {index} of `credentials` has no usable private key")]
    InvalidPrivateKey { index: usize },
    #[error("entry {index} of `credentials` is unusable: {error}")]
    Credentials { index: usize, error: TlsConfigError },
    #[error("a file named by `{section}` holds no {expected} in PEM form")]
    NoPem {
        section: ConfigSection,
        expected: &'static str,
    },
    #[error("entry {index} of `private_ca` is refused: {error}")]
    Trust { index: usize, error: TrustError },
    #[error("the private-CA profile for entry {index} of `private_ca` lists no domain")]
    EmptyProfile { index: usize },
    #[error("`{0}` must be a loopback address while `peer_scope` is `test`")]
    NotLoopback(ConfigSection),
    #[error("`{0}` is not a socket address")]
    InvalidAddress(ConfigSection),
    #[error("`limits.{0}` must be greater than zero and at most its ceiling")]
    LimitOutOfRange(&'static str),
    /// A limit within its own range that does not fit another: established
    /// streams and unauthenticated sockets beyond the socket cap, a peer
    /// cap not below the inbound socket cap, or a global input rate or
    /// burst below the per-socket one.
    #[error("`limits.{0}` does not fit the other limits")]
    InconsistentLimit(&'static str),
    #[error("the clock cannot tell the current time")]
    ClockUnavailable,
    #[error("`dialback.outbound` requires `dialback.secret_file`")]
    DialbackSecretMissing,
    #[error("the dialback secret is shorter than {0} bytes")]
    DialbackSecretTooShort(usize),
}
