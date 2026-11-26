//! Pure-Rust tenant isolation domain types, backend-agnostic storage/authz/admission
//! trait contracts, Verus-verified security kernels, and PObserve-paired JSONL trace
//! verification for Waddle's Credential & Inference Broker.

pub mod admission;
pub mod boot_clock;
pub mod holder_pop;
pub mod identity;
pub mod kernels;
pub mod permission;
pub mod placement;
pub mod quotas;
pub mod store;
pub mod tenant_cache;
pub mod trace_verifier;

pub use admission::{
    AdmissionError, AdmissionLedgerStore, AdmitAttemptRequest, AdmitReceipt, AttemptId,
    AttemptOutcome, BudgetLimits, CredentialKey, CredentialScope, FenceKey, FenceKind,
    FenceSnapshot, FenceState, RoomScopeKey, RunId, StepId, ThreeFenceSnapshot,
};
pub use boot_clock::{suspend_counted, BootClock, BootInstant, FakeBootClock, SystemBootClock};
pub use holder_pop::{
    build_holder_pop_transcript, format_trace_holder_key, verify_channel_bound_holder,
    HolderKeypair, HolderPopError, VerifiedHolderInstance, ED25519_SIGNATURE_LEN,
    HOLDER_POP_DOMAIN_SEPARATOR, RFC9266_TLS_EXPORTER_LABEL, TLS_EXPORTER_CHANNEL_BINDING_LEN,
};
pub use identity::{
    AccountId, CellRole, HolderInstance, IdentityError, IncarnationPubKey, SpiffeId, TenantId,
    TlsExporterProof, MAX_TENANT_ID_BYTES, MAX_TLS_EXPORTER_PROOF_BYTES,
};
pub use permission::{
    AuthzConsistency, AuthzModule, AuthzObject, AuthzObjectType, AuthzRelation, AuthzSubject,
    AuthzSubjectType, AuthzTuple, CheckDecision, CheckRequest, PermissionBackend,
    PermissionBackendError, TenantAuthzStoreRef,
};
pub use placement::{
    FenceGeneration, LeaseError, PlacementLease, PlacementLeaseStore, PlacementMode, TransferState,
};
pub use quotas::{
    QuotaError, QuotaResourceKind, TenantMemoryTracker, TenantResourceController,
    TenantResourceQuotas, WasmInvocationPermit, MIN_CONTROL_PLANE_PERMITS,
};
pub use store::{
    StorageBackendKind, TenantDbHandle, TenantQueryScope, TenantStore, TenantStoreError,
};
pub use tenant_cache::{TenantKey, TenantScopedCache};
