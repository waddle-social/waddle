//! Typed errors for `waddle-broker`.

use thiserror::Error;
use uuid::Uuid;
use waddle_broker_core::identity::{AccountId, TenantId};
use waddle_broker_core::permission::PermissionBackendError;
use waddle_broker_core::store::TenantStoreError;

/// Errors raised by AEAD envelope encryption and decryption.
#[derive(Debug, Error)]
pub enum CryptoError {
    #[error("secret plaintext must not be empty")]
    EmptySecret,
    #[error("secret plaintext exceeds maximum length of {max} bytes")]
    SecretTooLarge { max: usize },
    #[error("failed to encode AAD: {0}")]
    AadEncoding(String),
    #[error("CSPRNG failure")]
    RngFailure,
    #[error("AEAD encryption failed")]
    EncryptFailed,
    #[error("AEAD decryption failed (authentication tag or AAD mismatch)")]
    DecryptFailed,
    #[error("HKDF key derivation failed")]
    KdfFailed,
}

/// Errors raised by broker-only KMS providers.
#[derive(Debug, Error)]
pub enum KmsError {
    #[error("KMS key not found: {0}")]
    KeyNotFound(String),
    #[error("KMS key permanently destroyed: {0}")]
    KeyDestroyed(String),
    #[error("KMS provider unavailable: {0}")]
    Unavailable(String),
    #[error("KMS cryptographic failure: {0}")]
    Crypto(String),
    #[error("KMS configuration invalid: {0}")]
    InvalidConfig(String),
    #[error("KMS HTTP transport error: {0}")]
    Transport(String),
}

/// Errors raised by the durable destruction ledger and custody store.
#[derive(Debug, Error)]
pub enum LedgerError {
    #[error("database backend error: {0}")]
    Backend(String),
    #[error("serialization error: {0}")]
    Serialization(String),
    #[error("invalid stored state: {0}")]
    InvalidState(String),
}

/// Errors raised by IdP proof and device-key verification.
#[derive(Debug, Error)]
pub enum AuthError {
    #[error("IdP proof is required for this operation")]
    IdpProofRequired,
    #[error("SCRAM native_users accounts are ineligible for broker credential/policy operations")]
    ScramAccountIneligible,
    #[error("broker-verified IdP admin proof is required; cell-asserted admin flags are rejected")]
    AdminProofRequired,
    #[error("JWT header missing required kid")]
    MissingKid,
    #[error("JWT kid not found in broker-configured JWKS: {0}")]
    UnknownKid(String),
    #[error("JWT algorithm not in asymmetric allowlist: {0}")]
    DisallowedAlgorithm(String),
    #[error("JWT validation failed: {0}")]
    InvalidJwt(String),
    #[error("JWT jti replay detected")]
    JtiReplay,
    #[error("JWT challenge mismatch")]
    ChallengeMismatch,
    #[error("JWT cnf.jkt does not match device signing public key")]
    ConfirmationKeyMismatch,
    #[error("WebAuthn assertion invalid: {0}")]
    InvalidWebAuthn(String),
    #[error("WebAuthn signCount did not strictly increase")]
    SignCountRollback,
    #[error("device key not found or inactive: {0}")]
    UnknownOrInactiveDeviceKey(Uuid),
    #[error("device key already registered: {0}")]
    DeviceKeyAlreadyExists(Uuid),
    #[error("device key proof-of-possession or command signature invalid")]
    InvalidDeviceSignature,
    #[error("signed command expired or TTL exceeds maximum")]
    CommandExpired,
    #[error("command nonce replay detected")]
    CommandNonceReplay,
}

/// Errors raised by client-pinned root subkey verification and sealed enrollment.
#[derive(Debug, Error)]
pub enum SealError {
    #[error("unknown root broker-key-signing key id: {0}")]
    UnknownRootKid(String),
    #[error("subkey bundle trust_domain mismatch")]
    TrustDomainMismatch,
    #[error("subkey bundle expired or not yet valid")]
    SubkeyValidityWindow,
    #[error("subkey bundle key_epoch rollback detected (seen {seen}, got {got})")]
    SubkeyEpochRollback { seen: u64, got: u64 },
    #[error("subkey bundle root signature verification failed")]
    InvalidRootSignature,
    #[error("invalid P-256 public key encoding")]
    InvalidPublicKey,
    #[error("unknown or destroyed broker encryption subkey: {0}")]
    UnknownSubkey(Uuid),
    #[error("sealed enrollment envelope expired")]
    EnvelopeExpired,
    #[error("sealed enrollment crypto failure: {0}")]
    Crypto(#[from] CryptoError),
}

/// Top-level error for `BrokerCustodyService`.
#[derive(Debug, Error)]
pub enum BrokerError {
    #[error("broker egress is closed pending destruction ledger application after restore")]
    EgressClosedPendingRestore,
    #[error("credential {0} has been permanently destroyed")]
    CredentialDestroyed(Uuid),
    #[error("credential {0} not found")]
    CredentialNotFound(Uuid),
    #[error("account {0} has been retired")]
    AccountRetired(AccountId),
    #[error("tenant {0} has been destroyed")]
    TenantDestroyed(TenantId),
    #[error("enrollment nonce reused with conflicting payload digest")]
    NoncePayloadConflict,
    #[error("crypto error: {0}")]
    Crypto(#[from] CryptoError),
    #[error("KMS error: {0}")]
    Kms(#[from] KmsError),
    #[error("ledger error: {0}")]
    Ledger(#[from] LedgerError),
    #[error("auth error: {0}")]
    Auth(#[from] AuthError),
    #[error("seal error: {0}")]
    Seal(#[from] SealError),
    #[error("tenant store error: {0}")]
    TenantStore(#[from] TenantStoreError),
    #[error("permission backend error: {0}")]
    PermissionBackend(#[from] PermissionBackendError),
}
