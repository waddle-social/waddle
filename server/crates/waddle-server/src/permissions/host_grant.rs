//! The canonical host-grant authority for an enrolled scope (ADR-0020 D5).
//!
//! PostgreSQL holds the canonical grants of an enrolled community and SpiceDB
//! a fenced projection of them. This module is the first slice of that
//! authority: one fresh, isolated community with managed members-only rooms
//! and explicit room affiliations.
//!
//! - [`HostGrantStore`] is the canonical store. A scope's state row is its
//!   authority guard: a mutation or an activation locks it exclusively, and a
//!   fresh room-manifest acceptance locks it shared, in its own transaction,
//!   before it reads the policy state it admits under. Every operation is
//!   recorded once under a stable identity and an immutable fingerprint.
//! - A reduction of access (a revoke, an outcast, a demotion) takes effect at
//!   its PostgreSQL commit. An increase is recorded and becomes effective
//!   only after its projection has been written, verified and activated by
//!   the exact operation that is still current.
//! - [`HostGrantAuthority`] projects the canonical state into SpiceDB through
//!   the permission actor. Every projection write carries the scope's marker
//!   and is conditioned on the exact marker it replaces, so a writer holding
//!   an older operation cannot apply it after a newer one.
//! - Admission of a fresh room manifest is decided from PostgreSQL alone
//!   (`ingress_uow::HostGrantAdmissionRepository`); SpiceDB is not consulted.
//!   The acceptance reduces its plan to the admitted audience before it
//!   records anything (`ingress::host_grant_plan`).
//! - A replica admits a managed room only after it verified its runtime
//!   role's privileges on the store when it started
//!   ([`HostGrantAttachment`]).
//!
//! The decisions behind the lock order, the store capabilities and the
//! marker protocol are recorded in
//! `server/docs/planning/host-grant-authority-2047.md`.
//!
//! Errors and log lines carry no JID, room or Space identifier.

mod admission;
mod authority;
mod error;
mod issuance;
mod marker;
mod model;
mod policy;
mod projection;
mod runtime_attach;
mod store;

pub(crate) use admission::{
    admit_room_acceptance, managing_scope, AdmissionRelations, ManagingScope, RoomManagement,
};
pub use admission::{AdmissionDenial, AdmissionWitness, RoomAcceptance, RoomAdmission};
pub use authority::{
    HostGrantAuthority, ProjectionContradiction, Reconciliation, VerifiedProjection,
};
pub use error::HostGrantError;
pub use issuance::{
    IssuanceBasis, IssuancePolicy, IssuanceRefusal, ServedAccountDomains, SubjectHome,
};
pub use marker::{
    ProjectionMarker, HOST_GRANT_PROJECTION_DEFINITION, HOST_GRANT_PROJECTION_MARKER_RELATION,
};
pub use model::{
    AdmissionState, AuthorityIncarnation, BootstrapGrant, CandidateAdmission, ClosedReason,
    EnrollScope, GrantGeneration, GrantOperationId, GrantRow, GrantSubject, GrantSubjectError,
    ManagedRoom, MutationReceipt, OperationKind, OperationOutcome, OperationRecord,
    ProjectionDigest, RetireScope, RoomAffiliation, ScopeKey, ScopeLifecycleId, ScopeState,
    SetAffiliation, HOST_GRANT_POLICY_VERSION, HOST_GRANT_SCOPE_MAX_GRANT_ROWS,
    HOST_GRANT_SCOPE_MAX_ROOMS,
};
pub use policy::{affiliation_allows, grant_allows, honoured_affiliation, HostGrantAction};
pub use projection::{
    MarkerObservation, ProjectedGrant, ProjectedRelationship, ProjectionIntent,
    ProjectionObservation, ProjectionPlan, ProjectionRefusal, ProjectionWrite,
    HOST_GRANT_PROJECTION_OBSERVATION_TIMEOUT, HOST_GRANT_PROJECTION_RPC_TIMEOUT,
};
pub use runtime_attach::HostGrantAttachment;
#[cfg(feature = "test-support")]
pub use store::{with_host_grant_commit_fault, HostGrantCommitFault};
pub use store::{Activation, HostGrantStore};
