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
//!   role's privileges on the store and the restore fence
//!   ([`HostGrantAttachment`], held by the replica's
//!   [`HostGrantAttachmentGate`], which verifies it again when the
//!   authority changes and drops it when the fence refuses).
//! - A restore of PostgreSQL cannot reopen authority a later revoke closed:
//!   the authority incarnation is bound to the declared restore epoch, the
//!   database lineage and what SpiceDB witnesses (`witness`); a declared
//!   restore closes every scope under a new incarnation, and a closed scope
//!   reopens only through a verified reconstruction and an operator's
//!   reopen (`restore`).
//! - Every other surface that admits a principal to a managed room (joins,
//!   archive and discovery reads, PubSub, room administration, the
//!   planner's audience sources) decides through [`HostGrantGate`], from the
//!   same canonical grants and the same policy, and only while the
//!   replica's attachment gate holds an attachment. Legacy reads of a
//!   managed room's authorization objects through the permission actor
//!   answer nothing.
//! - Whose grants every one of those decisions honours is the replica's
//!   [`PrincipalPolicy`]: the account domains it serves and the S2S snapshot
//!   it runs (ADR-0020 D1 and D8, gate G17). A remote principal's grants
//!   contribute nothing unless its domain is admitted now, and its owner or
//!   admin grant only for a trusted home on a `pkix` stream; the trusted
//!   homes issuance accepts come from the same value.
//! - Whether an object is enrolled at all is answered from the replica's
//!   in-memory enrolled set ([`EnrolledScopes`]), refreshed in the
//!   background, so rooms no scope manages never wait on PostgreSQL.
//! - A revoke is acknowledged once no replica can act on a decision read
//!   before it: each replica holds a lease, installs every scope's new
//!   generation and records it, and a decision is acted on only while its
//!   [`FenceTicket`] is current ([`ReplicaFence`], `fence`).
//!
//! The decisions behind the lock order, the store capabilities and the
//! marker protocol are recorded in
//! `server/docs/planning/host-grant-authority-2047.md`.
//!
//! Errors and log lines carry no JID, room or Space identifier.

mod admission;
mod authority;
mod boot_clock;
mod checkpoint;
mod enrolled;
mod error;
mod fence;
mod gate;
mod issuance;
mod marker;
mod model;
mod policy;
mod principal;
mod projection;
mod rate_limited;
mod restore;
mod runtime_attach;
mod standing;
mod store;
mod witness;

pub(crate) use admission::{
    admit_room_acceptance, managing_scope, AdmissionRelations, ManagingScope, RoomManagement,
};
pub use admission::{AdmissionDenial, AdmissionWitness, RoomAcceptance, RoomAdmission};
pub use authority::{
    HostGrantAuthority, ProjectionContradiction, Reconciliation, VerifiedProjection,
};
pub(crate) use checkpoint::reached as reach_checkpoint;
pub use checkpoint::DecisionCheckpoint;
#[cfg(feature = "test-support")]
pub use checkpoint::{pause_at as pause_at_checkpoint, CheckpointGate};
pub use enrolled::{
    EnrolledScopes, EnrolledSet, Enrollment, ENROLLED_SET_REFRESH_INTERVAL,
    ENROLLED_SET_STALE_AFTER,
};
pub use error::HostGrantError;
pub use fence::{
    FenceAcknowledgement, FenceTicket, ReplicaFence, RevocationSweep,
    FENCE_ACKNOWLEDGEMENT_DEADLINE, FENCE_CLOCK_SKEW_WARNING, FENCE_EXPIRY_GRACE,
    FENCE_LEASE_MARGIN, FENCE_LEASE_TTL, FENCE_TICK,
};
pub use gate::{
    HostGrantGate, HostGrantUnavailable, ManagedRoomGrants, ManagedSpaceGrant, RoomGate,
    StoredRoomGrants,
};
pub use issuance::{
    IssuanceBasis, IssuancePolicy, IssuanceRefusal, ServedAccountDomains, SubjectHome,
};
pub use marker::{
    ProjectionMarker, HOST_GRANT_PROJECTION_DEFINITION, HOST_GRANT_PROJECTION_MARKER_RELATION,
};
pub(crate) use model::guard_queue_key;
pub use model::{
    AdmissionState, AuthorityIncarnation, BindSpace, BootstrapGrant, CandidateAdmission,
    ClosedReason, EnrollScope, GrantGeneration, GrantOperationId, GrantRow, GrantSubject,
    GrantSubjectError, ManagedRoom, MutationReceipt, OperationKind, OperationOutcome,
    OperationRecord, ProjectionDigest, RetireScope, RoomAffiliation, ScopeKey, ScopeLifecycleId,
    ScopeState, SetAffiliation, HOST_GRANT_POLICY_VERSION, HOST_GRANT_SCOPE_MAX_GRANT_ROWS,
    HOST_GRANT_SCOPE_MAX_ROOMS,
};
pub use policy::{affiliation_allows, grant_allows, honoured_affiliation, HostGrantAction};
pub use principal::{
    named_evidence, FederationSnapshot, NoSeats, PrincipalEvidence, PrincipalPolicy,
    PrincipalWitness, RemoteRefusal, RemoteWitness, SeatEvidence, StreamEvidence,
};
pub use projection::{
    MarkerObservation, ProjectedGrant, ProjectedRelationship, ProjectionIntent,
    ProjectionObservation, ProjectionPlan, ProjectionRefusal, ProjectionWrite, RestoredPlan,
    HOST_GRANT_PROJECTION_OBSERVATION_TIMEOUT, HOST_GRANT_PROJECTION_RPC_TIMEOUT,
};
pub(crate) use rate_limited::RateLimitedWarning;
pub use restore::{
    reconstruction_targets, verify_restore_fence, verify_restore_fence_with, AuthorityBinding,
    FenceEvidence, FenceMemory, OperatorRevoke, ReconstructionTarget, ReopenReport,
    ReopenSelection, RestoreDeclaration, RestoreEpoch, RestoreFenceRefusal, RestoreFenceReport,
    HOST_GRANT_RESTORE_FENCE_DEADLINE, HOST_GRANT_RESTORE_FENCE_SCOPE_ALLOWANCE,
    HOST_GRANT_RESTORE_SCAN_TIMEOUT,
};
pub use runtime_attach::{
    verification_retried, GateReadiness, GateState, HostGrantAttachment, HostGrantAttachmentGate,
    ReplicaFailure, INCOMPLETE_READINESS_GRACE, MAX_INCOMPLETE_PASSES, REVERIFY_INTERVAL,
};
pub use standing::{RoomBinding, RoomGrants, RoomLookup};
#[cfg(feature = "test-support")]
pub use store::{with_host_grant_commit_fault, HostGrantCommitFault};
pub use store::{Activation, HostGrantStore, RESTORE_TRANSITION_BATCH};
pub(crate) use witness::position_resource_authority;
#[cfg(feature = "test-support")]
pub use witness::with_host_grant_witness_write_fault;
pub use witness::{
    epoch_witness_resource, head_witness_resource, latch_witness_resource, position_standing,
    position_witness_resource, read_position_witness, regression, room_witness_digest,
    room_witness_resource, sync_journal_witness, sync_journal_witnesses, sync_position_witness,
    sync_witnesses_after_fence, LatchKind, ListedWitness, MalformedWitness,
    PositionAcknowledgement, PositionLatch, PositionStanding, PositionWitness, PositionWriter,
    PostgresPosition, WitnessChange, WitnessPass, WitnessSync,
};
