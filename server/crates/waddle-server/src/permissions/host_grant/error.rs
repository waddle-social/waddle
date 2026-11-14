//! Failures of the host-grant authority.

use thiserror::Error;

use super::issuance::IssuanceRefusal;
use super::model::{GrantGeneration, GrantSubjectError};
use super::projection::ProjectionRefusal;
use crate::db::DatabaseError;
use crate::ingress_substrate::{GuardedStoreWriterError, IngressSubstrateError};
use crate::permissions::PermissionError;

/// A refusal or failure of the host-grant authority.
///
/// Every variant except [`Self::CommitOutcomeUnknown`] and
/// [`Self::ScopeExhausted`] means the call changed nothing.
/// `CommitOutcomeUnknown` is resolved by reading the operation back under
/// its identity, never by assuming a rollback.
///
/// No variant carries a JID, a room or a Space identifier, and neither does
/// any variant's own message: a wrapped database or projection-store failure
/// is reachable only as the error's source, whose text may quote stored
/// values. Log [`Self::kind`], not the source chain.
#[derive(Debug, Error)]
pub enum HostGrantError {
    #[error("the host-grant authority requires PostgreSQL")]
    PostgresRequired,
    #[error("a statement of the host-grant store failed")]
    Database(#[from] DatabaseError),
    #[error("the host-grant transaction's lock and statement bounds could not be installed")]
    BoundsUnproven,
    #[error("the host-grant write could not prove the live protocol epoch")]
    EpochUnproven(#[source] IngressSubstrateError),
    #[error("a lock or statement bound expired before the host-grant transaction committed")]
    Busy,
    /// The commit was not acknowledged, or the task that owns the
    /// transaction ended without reporting. Either way the transaction may
    /// have committed.
    #[error(
        "the host-grant commit was not acknowledged; read the operation back under its identity"
    )]
    CommitOutcomeUnknown,
    /// PostgreSQL refused a statement for the role that ran it. Enrollment
    /// and retirement write the lifecycle ledger, which the runtime role
    /// only reads, so this is what either returns on a runtime handle.
    #[error(
        "PostgreSQL refused a statement of the operation for this role; enrollment and \
         retirement run in the owner phase"
    )]
    OwnerPhaseRequired,
    #[error("a stored host-grant row has an invalid {field}")]
    CorruptRow { field: &'static str },
    #[error("the runtime role or its privileges are unsafe at {component}")]
    UnsafeRuntimePrivileges { component: &'static str },
    #[error("host-grant relation {component} carries an unexpected trigger or rule")]
    UnexpectedExecutableSchema { component: &'static str },
    #[error("could not verify the PostgreSQL {setting} bound for the host-grant attachment")]
    InvalidTimeoutSetting { setting: &'static str },

    #[error("the scope key is not a canonical service JID and a nonempty Space node")]
    InvalidScopeKey,
    #[error("the room is not a canonical managed room")]
    UnmanagedRoom,
    #[error(
        "an enrollment names no room, repeats a room or a grant, grants no affiliation, or \
         exceeds a scope's bounds"
    )]
    InvalidEnrollment,
    #[error("the scope key already holds a lifecycle, or the lifecycle identity is in use")]
    ScopeAlreadyEnrolled,
    #[error("a room is already bound to a scope")]
    RoomAlreadyBound,
    #[error("no scope is enrolled under the lifecycle")]
    UnknownScope,
    #[error("the room is not managed by the scope")]
    UnknownRoom,
    #[error("the scope is at a different generation than the command expected")]
    StaleGeneration { current: GrantGeneration },
    #[error("the operation identity is already recorded for a different operation")]
    OperationConflict,
    #[error("the initiator holds no effective authority for this change")]
    InitiatorNotAuthorized,
    #[error("the change would leave the room without an owner whose standing is effective")]
    LastOwner,
    #[error("the scope is retired")]
    ScopeRetired,
    #[error("the scope has no generation left and is closed")]
    ScopeExhausted,
    #[error("the scope holds as many grant rows as one projection write carries")]
    GrantLimit,
    #[error("the scope is bound to another authority incarnation than the current one")]
    IncarnationChanged,
    #[error("the scope was enrolled under an admission policy this build does not evaluate")]
    PolicyVersionUnsupported,
    #[error(transparent)]
    Subject(#[from] GrantSubjectError),
    #[error(transparent)]
    Issuance(#[from] IssuanceRefusal),
    #[error(transparent)]
    Projection(#[from] ProjectionRefusal),
    #[error("the projection store gave no acknowledged answer")]
    ProjectionStore(#[source] PermissionError),
    /// The host-grant relations are missing from a schema that held them.
    /// Migrations only add them, so this is never read as "no scope is
    /// enrolled".
    #[error("the host-grant relations are missing from a schema that held them")]
    RelationsMissing,
}

impl From<GuardedStoreWriterError> for HostGrantError {
    fn from(error: GuardedStoreWriterError) -> Self {
        match error {
            GuardedStoreWriterError::Database(error) => Self::from_database(error),
            GuardedStoreWriterError::BoundsUnproven => Self::BoundsUnproven,
            GuardedStoreWriterError::EpochUnproven(error) => Self::EpochUnproven(error),
        }
    }
}

impl HostGrantError {
    /// Classify a failure of a statement before the commit: an expired lock
    /// or statement bound is [`Self::Busy`], a statement the role may not
    /// run is [`Self::OwnerPhaseRequired`], and nothing was changed.
    pub(crate) fn from_database(error: DatabaseError) -> Self {
        if crate::ingress_uow::is_database_timeout(&error) {
            Self::Busy
        } else if insufficient_privilege(&error) {
            Self::OwnerPhaseRequired
        } else {
            Self::Database(error)
        }
    }

    /// A stable name of the failure for logs and metrics. It names the
    /// variant and nothing the variant wraps, so it never holds a JID, a
    /// room, a Space or an operation identifier.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::PostgresRequired => "postgres_required",
            Self::Database(_) => "database",
            Self::BoundsUnproven => "bounds_unproven",
            Self::EpochUnproven(_) => "epoch_unproven",
            Self::Busy => "busy",
            Self::CommitOutcomeUnknown => "commit_outcome_unknown",
            Self::OwnerPhaseRequired => "owner_phase_required",
            Self::CorruptRow { .. } => "corrupt_row",
            Self::UnsafeRuntimePrivileges { .. } => "unsafe_runtime_privileges",
            Self::UnexpectedExecutableSchema { .. } => "unexpected_executable_schema",
            Self::InvalidTimeoutSetting { .. } => "invalid_timeout_setting",
            Self::InvalidScopeKey => "invalid_scope_key",
            Self::UnmanagedRoom => "unmanaged_room",
            Self::InvalidEnrollment => "invalid_enrollment",
            Self::ScopeAlreadyEnrolled => "scope_already_enrolled",
            Self::RoomAlreadyBound => "room_already_bound",
            Self::UnknownScope => "unknown_scope",
            Self::UnknownRoom => "unknown_room",
            Self::StaleGeneration { .. } => "stale_generation",
            Self::OperationConflict => "operation_conflict",
            Self::InitiatorNotAuthorized => "initiator_not_authorized",
            Self::LastOwner => "last_owner",
            Self::ScopeRetired => "scope_retired",
            Self::ScopeExhausted => "scope_exhausted",
            Self::GrantLimit => "grant_limit",
            Self::IncarnationChanged => "incarnation_changed",
            Self::PolicyVersionUnsupported => "policy_version_unsupported",
            Self::Subject(_) => "subject",
            Self::Issuance(_) => "issuance",
            Self::Projection(_) => "projection_refused",
            Self::ProjectionStore(_) => "projection_store",
            Self::RelationsMissing => "relations_missing",
        }
    }
}

/// PostgreSQL refused the statement for the role that ran it.
fn insufficient_privilege(error: &DatabaseError) -> bool {
    matches!(
        error,
        DatabaseError::Internal(sqlx::Error::Database(error))
            if error.code().as_deref() == Some("42501")
    )
}
