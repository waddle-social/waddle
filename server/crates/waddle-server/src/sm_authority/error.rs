use thiserror::Error;
use waddle_xmpp::stream_management::persistence::SmPersistenceError;

use crate::db::DatabaseError;
use crate::sm_persistence::SmRuntimeAttachError;

/// A failure of the durable session store that is not a decision about the
/// session. Returned before a commit is attempted, it means nothing changed.
#[derive(Debug, Error)]
pub enum DurableSmError {
    #[error(transparent)]
    Database(#[from] DatabaseError),
    /// A `sm_sessions` or `sm_unacked` row could not be encoded or decoded.
    #[error(transparent)]
    Rows(#[from] SmPersistenceError),
    #[error("durable SM session row has an invalid {field}")]
    CorruptRecord { field: &'static str },
    /// The retained frames do not cover the session's replay range.
    #[error("durable SM session retains {found} frames for a replay range of {expected}")]
    ReplayGap { expected: u64, found: u64 },
    #[error("the transaction's lock and statement bounds could not be installed")]
    BoundsUnproven,
    /// The attested runtime pool never runs owner DDL; it attaches instead.
    #[error("the durable SM store must attach, not bootstrap, on an attested PostgreSQL pool")]
    AttestedPoolRequiresAttach,
    #[error("the durable SM schema could not be verified for runtime attachment")]
    RuntimeAttach(#[from] SmRuntimeAttachError),
    #[error("a clustered durable SM store requires PostgreSQL")]
    ClusterRequiresPostgres,
    /// A clustered write named no claim, so nothing proves this node may
    /// write the session.
    #[error("a clustered durable SM write requires the session claim")]
    ClaimRequired,
    #[error("durable SM does not support {operation}")]
    Unsupported { operation: &'static str },
}
