//! Waddle Credential & Inference Broker custody, envelope encryption, KMS
//! providers, durable destruction ledger, IdP intent verification, and
//! client-sealed enrollment.

pub mod custody;
pub mod envelope;
pub mod error;
pub mod idp_proof;
pub mod key_hierarchy;
pub mod kms;
pub mod ledger;
pub mod sealed_enrollment;
pub mod secret;

pub use custody::{
    BrokerCustodyService, CredentialLifecycleStatus, CredentialReceipt, EnrollCredentialInput,
    RestoreReopenReport,
};
pub use envelope::{
    decrypt_secret, encrypt_secret, EnvelopeCipher, EnvelopeContext, SealedEnvelope,
};
pub use error::{AuthError, BrokerError, CryptoError, KmsError, LedgerError, SealError};
pub use idp_proof::{
    authorize_admin_policy_mutation, build_device_enroll_pop_transcript, enforce_not_scram,
    p256_jwk_thumbprint, AuthProvenance, CellAssertedAdminFlags, ColonyBrokerClaims,
    ColonyJwtVerifier, ConfirmationClaim, DeviceKeyRegistry, EnrollDeviceKeyInput, IdpProof,
    RegisteredDeviceKey, RegisteredWebAuthnKey, SignedDeviceCommand, VerifiedIdpIntent,
    WebAuthnAssertionInput, WebAuthnVerifier,
};
pub use key_hierarchy::{KeyHierarchyService, TenantDekRecord, UserDekRecord};
pub use kms::local::LocalKmsProvider;
pub use kms::scaleway::{ScalewayKmsConfig, ScalewayKmsProvider};
pub use kms::vault::{VaultTransitConfig, VaultTransitKmsProvider};
pub use kms::{KmsKeyRef, KmsProvider, WrappedKeyBlob};
pub use ledger::postgres::{PostgresLedgerStore, SqliteCompatLedgerStore};
pub use ledger::turso::TursoLedgerStore;
pub use ledger::{
    DestructionLedgerStore, DestructionReason, DestructionRecord, DestructionTargetKind,
    EgressGateState, EnrollmentNonceRecord, NonceClaimOutcome, RetiredAccountRecord,
    StoredEnvelopeRecord,
};
pub use sealed_enrollment::{
    compute_root_kid, seal_credential_for_broker, BrokerSubkeyRing, ClientSealContext,
    ClientSealedBlob, RootBkskPinSet, RootBkskSigner, SignedSubkeyBundle,
};
pub use secret::{Key256, SecretBytes, MAX_SECRET_BYTES};
