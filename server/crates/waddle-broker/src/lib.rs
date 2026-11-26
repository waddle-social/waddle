//! Waddle Credential & Inference Broker custody, envelope encryption, KMS
//! providers, durable destruction ledger, IdP intent verification, and
//! client-sealed enrollment.

pub mod admission;
pub mod authz;
pub mod custody;
pub mod egress;
pub mod envelope;
pub mod error;
pub mod idp_proof;
pub mod key_hierarchy;
pub mod kms;
pub mod ledger;
pub mod sealed_enrollment;
pub mod secret;

pub use admission::invocation::{
    validate_principal_and_payer, validate_signed_invocation, InvocationError, InvocationPrincipal,
    PayerSelection, SignedInferenceInvocation, MAX_INVOCATION_TTL_MS,
};
pub use admission::postgres::{PostgresAdmissionLedgerStore, SqliteCompatAdmissionLedgerStore};
pub use admission::replica_fence::{
    BrokerReplicaFence, FenceTicket, BROKER_FENCE_ACK_DEADLINE, BROKER_FENCE_EXPIRY_GRACE,
    BROKER_FENCE_LEASE_MARGIN, BROKER_FENCE_LEASE_TTL, BROKER_FENCE_TICK,
};
pub use admission::rollback::{
    RollbackController, RollbackError, RollbackReport, RollbackScope, RollbackTarget,
};
pub use admission::service::{
    AdmissionBarrierHooks, AdmissionServiceError, AdmitInvocationInput, AdmittedDispatch,
    BoundDispatchContext, BrokerAdmissionService, TraceRecorder,
};
pub use admission::turso::TursoAdmissionLedgerStore;
pub use admission::{
    AdmissionQueryStore, AttemptRecord, AttemptStatus, FenceAcknowledgement, ReplicaFenceStore,
    RunBudgetSnapshot,
};
pub use authz::local::LocalPermissionBackend;
pub use authz::openfga::{
    parse_openfga_consistency, InProcessOpenFgaServer, OpenFgaCheckWireRequest, OpenFgaConfig,
    OpenFgaConsistency, OpenFgaPermissionBackend, OpenFgaTupleKey, RecordedOpenFgaRequest,
};
pub use authz::openfga_model::{
    openfga_authorization_model_json, openfga_module_grant_object, openfga_store_grant_object,
    OPENFGA_CHAT_MODULE_DSL, OPENFGA_INFERENCE_MODULE_DSL, OPENFGA_SCHEMA_VERSION,
};
pub use authz::{
    enforce_module_write, enforce_object_module_write, evaluate_permission_in_memory,
    object_type_module, AuthzCallerRole,
};
pub use custody::{
    BrokerCustodyService, CredentialLifecycleStatus, CredentialReceipt, EnrollCredentialInput,
    RestoreReopenReport,
};
pub use egress::{
    is_blocked_admin_path, resolve_and_validate_addrs, validate_operation_path, BrokerEgressClient,
    DataCollectionPolicy, DnsResolver, EgressBindingPolicy, EgressCaps, EgressChunk, EgressError,
    EgressInferenceRequest, EgressProfileRegistry, EgressResponse, EgressStream, EgressStreamEvent,
    ExpectedEgressBinding, ProfileError, ProviderOperation, ProviderOperationWire, ProviderProfile,
    QualifiedZdrRoute, RouteKeyOrigin, RouteQualificationError, SsrfError, TokioDnsResolver,
    ZdrRouteQuery, ZdrRouteRegistry,
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
