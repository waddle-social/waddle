use thiserror::Error;
use waddle_xmpp::ownership::account_coordinator::{AccountResourcesError, BindingFenceError};
use waddle_xmpp::ownership::account_resource_transition::AccountResourceRestoreError;
use waddle_xmpp::telemetry::attributes::MailboxRejection;

use super::claims::AccountClaimError;
use crate::account_bindings::AccountBindingStoreError;

/// Why a coordinator did not decide a request. None of these is an answer
/// about the account's resources: a caller must not read one as "no
/// resources" or as a rejection of its binding.
///
/// After [`Self::Store`] from a mutation the commit outcome is unknown. The
/// caller repeats the operation under the same identity and receives the
/// recorded outcome if it committed.
#[derive(Debug, Error)]
pub enum AccountCoordinatorError {
    #[error("the account binding store failed")]
    Store(#[from] AccountBindingStoreError),
    #[error("the account refused the operation: {0}")]
    Resources(AccountResourcesError),
    #[error("the account claim could not be established")]
    Claim(#[from] AccountClaimError),
    #[error("a stored resource row does not describe a slot")]
    Restore(#[from] AccountResourceRestoreError),
    #[error("the operation identity is already recorded for a different operation")]
    OperationIdConflict,
    /// Another binding already waits to replace the resource's binding.
    #[error("the resource already has a pending replacement")]
    ReplacementAlreadyPending,
    /// The bounded set of retirements awaiting endpoint evidence is full.
    #[error("the account has too many retirements awaiting evidence")]
    RetirementBacklogFull,
    #[error("the binding is not the resource's current binding: {0}")]
    Fenced(BindingFenceError),
    /// The store committed an operation without returning the row.
    #[error("the binding store and the coordinator disagree about a resource")]
    Diverged,
    #[error("the coordinator mailbox refused the request: {0:?}")]
    Mailbox(MailboxRejection),
    /// The node that owns the account did not decide the request: it could
    /// not be reached in time, it failed, or it does not serve the account
    /// coordinator. After a mutation the commit outcome is unknown, as
    /// after [`Self::Store`].
    #[error("the node that owns the account did not decide the request")]
    OwnerUndecided,
    /// The application never selected the account coordinator, which
    /// startup always does; only a fixture that skipped startup reads it.
    #[error("no account coordinator was selected")]
    Unselected,
}
