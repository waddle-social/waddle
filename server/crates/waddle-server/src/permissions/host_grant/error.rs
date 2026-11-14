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
/// its identity, never by assuming a rollback. No variant carries a JID, a
/// room or a Space identifier.
#[derive(Debug, Error)]
pub enum HostGrantError {
    #[error("the host-grant authority requires PostgreSQL")]
    PostgresRequired,
    #[error(transparent)]
    Database(#[from] DatabaseError),
    #[error("the host-grant transaction's lock and statement bounds could not be installed")]
    BoundsUnproven,
    #[error("the host-grant write could not prove the live protocol epoch")]
    EpochUnproven(#[source] IngressSubstrateError),
    #[error("a lock or statement bound expired before the host-grant transaction committed")]
    Busy,
    #[error(
        "the host-grant commit was not acknowledged; read the operation back under its identity"
    )]
    CommitOutcomeUnknown,
    #[error("the task that owns the host-grant transaction did not finish")]
    OwnerTaskFailed,
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
    #[error("the projection store did not answer: {0}")]
    ProjectionStore(#[source] PermissionError),
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
    /// or statement bound is [`Self::Busy`], and nothing was changed.
    pub(crate) fn from_database(error: DatabaseError) -> Self {
        if crate::ingress_uow::is_database_timeout(&error) {
            Self::Busy
        } else {
            Self::Database(error)
        }
    }
}
