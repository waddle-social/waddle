use thiserror::Error;
use waddle_xmpp::ownership::account_resource_transition::AccountResourceRestoreError;

use super::runtime_attach::AccountBindingAttachError;
use crate::db::DatabaseError;
use crate::ingress_substrate::{GuardedStoreWriterError, IngressSubstrateError};

/// A failure of the account binding store that is not a decision about the
/// resource. Returned before a commit is attempted, it means nothing
/// changed; returned by the commit itself, the outcome is unknown and the
/// caller resolves it by repeating the operation under the same identity.
#[derive(Debug, Error)]
pub enum AccountBindingStoreError {
    #[error(transparent)]
    Database(#[from] DatabaseError),
    #[error("account resource row has an invalid {field}")]
    CorruptRow { field: &'static str },
    #[error("account resource row does not describe a slot")]
    Restore(#[from] AccountResourceRestoreError),
    #[error("the transaction's lock and statement bounds could not be installed")]
    BoundsUnproven,
    /// The resource relations are epoch-guarded and the write could not
    /// prove the live protocol epoch: it is one this binary does not
    /// implement, or the connection is not the attested one.
    #[error("the account binding write could not prove the live protocol epoch")]
    EpochUnproven(#[source] IngressSubstrateError),
    #[error("the account binding schema could not be verified for runtime attachment")]
    RuntimeAttach(#[from] AccountBindingAttachError),
    #[error("a clustered account binding store requires PostgreSQL")]
    ClusterRequiresPostgres,
    /// A clustered write named a standalone process identity, so no claim
    /// row can prove this node may write the account.
    #[error("a clustered account binding write requires a clustered claim identity")]
    ClaimRequired,
    #[error("the account claim the write names is not held")]
    ClaimNotHeld,
    #[error("the full JID belongs to a different account than the claim")]
    AccountMismatch,
    /// Only a bind creates a resource row.
    #[error("the account has no row for this full JID")]
    UnknownResource,
    #[error("the operation identity is already recorded for a different operation")]
    OperationIdConflict,
    #[error("the snapshot exceeds the snapshot bounds")]
    SnapshotTooLarge,
    /// A snapshot names a stream-management session that has no durable
    /// session row.
    #[error("the snapshot references a stream-management session that is not stored")]
    SessionUnknown,
    /// A snapshot names a session generation other than the one the durable
    /// session row is bound at.
    #[error("the snapshot references a stream-management session generation that is not current")]
    SessionGenerationMismatch,
    #[error("a revision or generation does not fit the stored integer range")]
    CounterOutOfRange,
    /// The in-memory store was told to refuse, to exercise callers.
    #[error("the account binding store is unavailable")]
    Unavailable,
}

impl From<GuardedStoreWriterError> for AccountBindingStoreError {
    fn from(error: GuardedStoreWriterError) -> Self {
        match error {
            GuardedStoreWriterError::Database(error) => Self::Database(error),
            GuardedStoreWriterError::BoundsUnproven => Self::BoundsUnproven,
            GuardedStoreWriterError::EpochUnproven(error) => Self::EpochUnproven(error),
        }
    }
}
