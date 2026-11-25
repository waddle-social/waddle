//! Pure-Rust tenant isolation domain types, backend-agnostic storage/authz/admission
//! trait contracts, Verus-verified security kernels, and PObserve-paired JSONL trace
//! verification for Waddle's Credential & Inference Broker.

pub mod admission;
pub mod identity;
pub mod kernels;
pub mod permission;
pub mod placement;
pub mod store;
pub mod trace_verifier;

pub use admission::{
    AdmissionError, AdmissionLedgerStore, AdmitAttemptRequest, AdmitReceipt, AttemptId,
    AttemptOutcome, BudgetLimits, CredentialKey, CredentialScope, FenceKey, FenceKind,
    FenceSnapshot, FenceState, RoomScopeKey, RunId, StepId, ThreeFenceSnapshot,
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
pub use store::{
    StorageBackendKind, TenantDbHandle, TenantQueryScope, TenantStore, TenantStoreError,
};
