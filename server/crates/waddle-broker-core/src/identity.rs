use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

pub const MAX_TENANT_ID_BYTES: usize = 253;
pub const MAX_TLS_EXPORTER_PROOF_BYTES: usize = 128;
pub const MAX_CELL_ROLE_BYTES: usize = 63;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum IdentityError {
    #[error("tenant id must not be empty")]
    EmptyTenantId,
    #[error("tenant id exceeds maximum length of {MAX_TENANT_ID_BYTES} bytes")]
    TenantIdTooLong,
    #[error("tenant id must be a canonical lowercase DNS domain")]
    InvalidTenantId,
    #[error("SPIFFE id must start with 'spiffe://' and contain a trust domain and path")]
    InvalidSpiffeId,
    #[error(
        "TLS exporter proof must be non-empty and at most {MAX_TLS_EXPORTER_PROOF_BYTES} bytes"
    )]
    InvalidTlsExporterProof,
    #[error("cell role must be non-empty lowercase ASCII alphanumeric/hyphen")]
    InvalidCellRole,
}

/// Canonical validated tenant identifier derived from verified local domain routing.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TenantId(String);

impl TenantId {
    pub fn parse(raw: &str) -> Result<Self, IdentityError> {
        if raw.is_empty() {
            return Err(IdentityError::EmptyTenantId);
        }
        if raw.len() > MAX_TENANT_ID_BYTES {
            return Err(IdentityError::TenantIdTooLong);
        }
        if raw.starts_with('.') || raw.ends_with('.') {
            return Err(IdentityError::InvalidTenantId);
        }

        for label in raw.split('.') {
            if label.is_empty() || label.len() > 63 {
                return Err(IdentityError::InvalidTenantId);
            }
            if label.starts_with('-') || label.ends_with('-') {
                return Err(IdentityError::InvalidTenantId);
            }
            if !label
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            {
                return Err(IdentityError::InvalidTenantId);
            }
        }

        Ok(Self(raw.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TenantId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for TenantId {
    type Err = IdentityError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl TryFrom<String> for TenantId {
    type Error = IdentityError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<TenantId> for String {
    fn from(value: TenantId) -> Self {
        value.0
    }
}

/// Immutable account identifier decoupled from mutable XMPP localparts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AccountId(Uuid);

impl AccountId {
    pub fn new_v4() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn new_v7(ts: uuid::Timestamp) -> Self {
        Self(Uuid::new_v7(ts))
    }

    pub const fn from_uuid(id: Uuid) -> Self {
        Self(id)
    }

    pub const fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl fmt::Display for AccountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Validated SPIFFE workload identity (`spiffe://<trust-domain>/<path>`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SpiffeId(String);

impl SpiffeId {
    pub fn parse(raw: &str) -> Result<Self, IdentityError> {
        let rest = raw
            .strip_prefix("spiffe://")
            .ok_or(IdentityError::InvalidSpiffeId)?;
        let (trust_domain, path) = rest.split_once('/').ok_or(IdentityError::InvalidSpiffeId)?;
        if trust_domain.is_empty()
            || trust_domain.len() > MAX_TENANT_ID_BYTES
            || !trust_domain.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-' || b == b'_'
            })
        {
            return Err(IdentityError::InvalidSpiffeId);
        }
        if path.is_empty() || path.contains("..") || path.contains("//") || path.ends_with('/') {
            return Err(IdentityError::InvalidSpiffeId);
        }
        if !path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'/' || b == b'-' || b == b'_' || b == b'.')
        {
            return Err(IdentityError::InvalidSpiffeId);
        }
        Ok(Self(raw.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SpiffeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for SpiffeId {
    type Err = IdentityError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl TryFrom<String> for SpiffeId {
    type Error = IdentityError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<SpiffeId> for String {
    fn from(value: SpiffeId) -> Self {
        value.0
    }
}

/// Ephemeral Ed25519/X25519 incarnation public key for a single process instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IncarnationPubKey([u8; 32]);

impl IncarnationPubKey {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// RFC 9266 `tls-exporter` channel-binding proof signed by the incarnation key.
/// Redacts raw proof bytes in `Debug` output to prevent credential/channel-binding leakage.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "Vec<u8>", into = "Vec<u8>")]
pub struct TlsExporterProof(Vec<u8>);

impl TlsExporterProof {
    pub fn new(bytes: Vec<u8>) -> Result<Self, IdentityError> {
        if bytes.is_empty() || bytes.len() > MAX_TLS_EXPORTER_PROOF_BYTES {
            return Err(IdentityError::InvalidTlsExporterProof);
        }
        Ok(Self(bytes))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for TlsExporterProof {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TlsExporterProof([REDACTED; {} bytes])", self.0.len())
    }
}

impl TryFrom<Vec<u8>> for TlsExporterProof {
    type Error = IdentityError;

    fn try_from(value: Vec<u8>) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<TlsExporterProof> for Vec<u8> {
    fn from(value: TlsExporterProof) -> Self {
        value.0
    }
}

/// Channel-bound process incarnation identity for leaseholders and broker peers.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HolderInstance {
    pub spiffe_id: SpiffeId,
    pub incarnation_pubkey: IncarnationPubKey,
    pub tls_exporter_proof: TlsExporterProof,
}

/// Logical cell role (e.g., `shared-pool` or `dedicated-acme`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CellRole(String);

impl CellRole {
    pub fn parse(raw: &str) -> Result<Self, IdentityError> {
        if raw.is_empty()
            || raw.len() > MAX_CELL_ROLE_BYTES
            || raw.starts_with('-')
            || raw.ends_with('-')
            || !raw
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(IdentityError::InvalidCellRole);
        }
        Ok(Self(raw.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CellRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for CellRole {
    type Err = IdentityError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl TryFrom<String> for CellRole {
    type Error = IdentityError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<CellRole> for String {
    fn from(value: CellRole) -> Self {
        value.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tenant_id_validation() {
        assert!(TenantId::parse("acme.waddle.social").is_ok());
        assert!(TenantId::parse("localhost").is_ok());
        assert_eq!(TenantId::parse(""), Err(IdentityError::EmptyTenantId));
        assert_eq!(
            TenantId::parse("Acme.example.com"),
            Err(IdentityError::InvalidTenantId)
        );
        assert_eq!(
            TenantId::parse("acme.example.com."),
            Err(IdentityError::InvalidTenantId)
        );
        assert_eq!(
            TenantId::parse("-bad.example.com"),
            Err(IdentityError::InvalidTenantId)
        );
        let too_long = "a".repeat(MAX_TENANT_ID_BYTES + 1);
        assert_eq!(
            TenantId::parse(&too_long),
            Err(IdentityError::TenantIdTooLong)
        );
    }

    #[test]
    fn spiffe_id_and_proof_validation() {
        assert!(SpiffeId::parse("spiffe://waddle.internal/ns/cells/sa/shared-pool").is_ok());
        assert_eq!(
            SpiffeId::parse("https://waddle.internal/sa/cell"),
            Err(IdentityError::InvalidSpiffeId)
        );
        assert_eq!(
            SpiffeId::parse("spiffe://waddle.internal/"),
            Err(IdentityError::InvalidSpiffeId)
        );
        assert_eq!(
            SpiffeId::parse("spiffe://waddle.internal/ns/../escape"),
            Err(IdentityError::InvalidSpiffeId)
        );

        let proof = TlsExporterProof::new(vec![0xAB; 32]).expect("valid proof");
        let debug_str = format!("{proof:?}");
        assert!(debug_str.contains("REDACTED"));
        assert!(!debug_str.contains("171"));
        assert!(TlsExporterProof::new(Vec::new()).is_err());
    }

    #[test]
    fn cell_role_validation() {
        assert!(CellRole::parse("shared-pool").is_ok());
        assert!(CellRole::parse("dedicated-acme").is_ok());
        assert!(CellRole::parse("").is_err());
        assert!(CellRole::parse("SharedPool").is_err());
        assert!(CellRole::parse("-bad").is_err());
    }
}
