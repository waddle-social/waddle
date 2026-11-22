use std::fmt;
use std::str::FromStr;

use uuid::Uuid;

/// Failures when parsing or validating domain identifier strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum IdentifierParseError {
    /// Value was empty or whitespace-only.
    #[error("{field} must not be empty")]
    Empty {
        /// Name of the validated field.
        field: &'static str,
    },
    /// Value exceeded the maximum permitted byte length.
    #[error("{field} exceeds maximum length of {max} characters")]
    TooLong {
        /// Name of the validated field.
        field: &'static str,
        /// Maximum permitted length.
        max: usize,
    },
    /// Value was not a valid lower-case RFC-4122 UUID.
    #[error("{field} must be a lower-case RFC-4122 UUID")]
    InvalidUuid {
        /// Name of the validated field.
        field: &'static str,
    },
    /// Value was not a valid Kubernetes DNS label / resource name.
    #[error("{field} must be a valid lower-case Kubernetes resource name")]
    InvalidResourceName {
        /// Name of the validated field.
        field: &'static str,
    },
    /// Value was not a valid PostgreSQL identifier (`^[a-z_][a-z0-9_]{0,62}$`).
    #[error("{field} must be a valid lower-case PostgreSQL identifier")]
    InvalidPgIdentifier {
        /// Name of the validated field.
        field: &'static str,
    },
    /// Image digest was malformed or the prohibited all-zero placeholder.
    #[error("image digest must be sha256:<64 lower-case hex> and not all zeros")]
    InvalidImageDigest,
    /// Git commit SHA was not 40 lower-case hex characters.
    #[error("git sha must be 40 lower-case hexadecimal characters")]
    InvalidGitSha,
    /// Content digest was not 64 lower-case hex characters.
    #[error("content digest must be 64 lower-case hexadecimal characters")]
    InvalidContentDigest,
    /// XMPP domain failed canonical hostname validation.
    #[error("xmpp domain must be a valid lower-case canonical DNS name")]
    InvalidXmppDomain,
}

/// Lower-case RFC-4122 UUID identifying a Waddle deployment across restarts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeploymentUuid(Uuid);

impl DeploymentUuid {
    /// Parses a strictly lower-case RFC-4122 UUID string.
    ///
    /// # Errors
    /// Returns [`IdentifierParseError::InvalidUuid`] if `raw` is not a lower-case RFC-4122 UUID.
    pub fn parse_exact(raw: &str) -> Result<Self, IdentifierParseError> {
        let trimmed = raw.trim();
        let parsed = Uuid::parse_str(trimmed).map_err(|_| IdentifierParseError::InvalidUuid {
            field: "deploymentUuid",
        })?;
        let canonical = parsed.hyphenated().to_string();
        if trimmed != canonical {
            return Err(IdentifierParseError::InvalidUuid {
                field: "deploymentUuid",
            });
        }
        Ok(Self(parsed))
    }

    /// Wraps an already-constructed `Uuid` as a deployment identifier.
    #[must_use]
    pub const fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    /// Returns the inner `Uuid`.
    #[must_use]
    pub const fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl FromStr for DeploymentUuid {
    type Err = IdentifierParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse_exact(s)
    }
}

impl fmt::Display for DeploymentUuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.hyphenated().to_string())
    }
}

/// Validated lower-case Kubernetes object name (RFC 1123 subdomain, up to 253 bytes).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ResourceName(String);

impl ResourceName {
    const MAX_LEN: usize = 253;

    /// Validates and constructs a Kubernetes resource name.
    ///
    /// # Errors
    /// Returns an [`IdentifierParseError`] if `raw` is empty, longer than 253 bytes,
    /// or contains characters outside `[a-z0-9-.]`.
    pub fn try_new(raw: &str) -> Result<Self, IdentifierParseError> {
        if raw.is_empty() {
            return Err(IdentifierParseError::Empty {
                field: "resourceName",
            });
        }
        if raw.len() > Self::MAX_LEN {
            return Err(IdentifierParseError::TooLong {
                field: "resourceName",
                max: Self::MAX_LEN,
            });
        }
        if !is_valid_rfc1123_subdomain(raw) {
            return Err(IdentifierParseError::InvalidResourceName {
                field: "resourceName",
            });
        }
        Ok(Self(raw.to_owned()))
    }

    /// Borrows the validated resource name string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for ResourceName {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl FromStr for ResourceName {
    type Err = IdentifierParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::try_new(s)
    }
}

impl fmt::Display for ResourceName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Validated lower-case PostgreSQL identifier (`^[a-z_][a-z0-9_]{0,62}$`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PgIdentifier(String);

impl PgIdentifier {
    const MAX_LEN: usize = 63;

    /// Validates and constructs a PostgreSQL identifier.
    ///
    /// # Errors
    /// Returns an [`IdentifierParseError`] if `raw` is not a valid lower-case PG identifier.
    pub fn try_new(raw: &str) -> Result<Self, IdentifierParseError> {
        if raw.is_empty() {
            return Err(IdentifierParseError::Empty {
                field: "pgIdentifier",
            });
        }
        if raw.len() > Self::MAX_LEN {
            return Err(IdentifierParseError::TooLong {
                field: "pgIdentifier",
                max: Self::MAX_LEN,
            });
        }
        let mut bytes = raw.bytes();
        let first_ok = bytes
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b == b'_');
        let rest_ok = bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
        if !first_ok || !rest_ok {
            return Err(IdentifierParseError::InvalidPgIdentifier {
                field: "pgIdentifier",
            });
        }
        Ok(Self(raw.to_owned()))
    }

    /// Borrows the validated identifier string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for PgIdentifier {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl FromStr for PgIdentifier {
    type Err = IdentifierParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::try_new(s)
    }
}

impl fmt::Display for PgIdentifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Validated immutable container image digest (`sha256:<64 lower-case hex>`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImageDigest(String);

impl ImageDigest {
    const ZERO_DIGEST: &str =
        "sha256:0000000000000000000000000000000000000000000000000000000000000000";

    /// Validates an OCI `sha256:` digest and rejects the all-zero placeholder.
    ///
    /// # Errors
    /// Returns [`IdentifierParseError::InvalidImageDigest`] if malformed or all zeros.
    pub fn try_new(raw: &str) -> Result<Self, IdentifierParseError> {
        let Some(hex_part) = raw.strip_prefix("sha256:") else {
            return Err(IdentifierParseError::InvalidImageDigest);
        };
        if !is_lower_hex(hex_part, 64) || raw == Self::ZERO_DIGEST {
            return Err(IdentifierParseError::InvalidImageDigest);
        }
        Ok(Self(raw.to_owned()))
    }

    /// Borrows the digest string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for ImageDigest {
    type Err = IdentifierParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::try_new(s)
    }
}

impl fmt::Display for ImageDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Validated 40-character lower-case hexadecimal Git commit SHA.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GitSha(String);

impl GitSha {
    /// Validates a 40-character lower-case Git SHA.
    ///
    /// # Errors
    /// Returns [`IdentifierParseError::InvalidGitSha`] if `raw` is not 40 lower-case hex chars.
    pub fn try_new(raw: &str) -> Result<Self, IdentifierParseError> {
        if !is_lower_hex(raw, 40) {
            return Err(IdentifierParseError::InvalidGitSha);
        }
        Ok(Self(raw.to_owned()))
    }

    /// Borrows the Git SHA string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for GitSha {
    type Err = IdentifierParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::try_new(s)
    }
}

impl fmt::Display for GitSha {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Lower-case 64-character SHA-256 hex digest of canonical non-secret or hashed content.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContentDigest(String);

impl ContentDigest {
    /// Validates a 64-character lower-case SHA-256 hex string.
    ///
    /// # Errors
    /// Returns [`IdentifierParseError::InvalidContentDigest`] if malformed.
    pub fn try_new(raw: &str) -> Result<Self, IdentifierParseError> {
        if !is_lower_hex(raw, 64) {
            return Err(IdentifierParseError::InvalidContentDigest);
        }
        Ok(Self(raw.to_owned()))
    }

    /// Borrows the hex digest string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for ContentDigest {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl FromStr for ContentDigest {
    type Err = IdentifierParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::try_new(s)
    }
}

impl fmt::Display for ContentDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Validated canonical lower-case XMPP domain name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct XmppDomain(String);

impl XmppDomain {
    /// Validates a canonical lower-case XMPP domain name.
    ///
    /// # Errors
    /// Returns [`IdentifierParseError::InvalidXmppDomain`] if empty, longer than 253 chars,
    /// contains uppercase letters, schemes, ports, or invalid DNS labels.
    pub fn try_new(raw: &str) -> Result<Self, IdentifierParseError> {
        if raw.is_empty()
            || raw.len() > 253
            || raw.starts_with('.')
            || raw.ends_with('.')
            || raw.contains("://")
            || raw.contains(':')
            || !is_valid_rfc1123_subdomain(raw)
        {
            return Err(IdentifierParseError::InvalidXmppDomain);
        }
        Ok(Self(raw.to_owned()))
    }

    /// Borrows the domain string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for XmppDomain {
    type Err = IdentifierParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::try_new(s)
    }
}

impl fmt::Display for XmppDomain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn is_lower_hex(raw: &str, expected_len: usize) -> bool {
    raw.len() == expected_len
        && raw
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_valid_rfc1123_subdomain(raw: &str) -> bool {
    raw.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && label
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    })
}
