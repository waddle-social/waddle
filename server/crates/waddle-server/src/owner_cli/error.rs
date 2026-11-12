//! Errors and exit codes of the operator subcommands.

use std::error::Error;
use std::path::PathBuf;

use thiserror::Error;

use crate::db::physical_attestation::PhysicalAttestationError;
use crate::db::runtime_startup::{HandoffError, RuntimeStartupError};
use crate::db::DatabaseError;
use crate::server::runtime_owner::RestrictedRuntimeOwnerError;

/// Process exit codes. A deployment job retries on [`Self::Failed`] and
/// [`Self::Pending`] and stops for an operator on [`Self::Refused`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitStatus {
    /// The subcommand completed.
    Success,
    /// The connection or the database returned an error, a lock stayed
    /// contended or timed out, or an outcome is unknown. Nothing says a
    /// rerun would fail; the diagnostic says what happened.
    Failed,
    /// The arguments or the environment are incomplete or invalid. Nothing
    /// was sent to the database.
    Usage,
    /// Every schema is prepared and granted, and the keypair-pool generation
    /// is inside its activation delay. Run `owner-phase` again after the
    /// printed time.
    Pending,
    /// A verifier refused the database, the plan or the document. A rerun
    /// refuses again until an operator changes one of them or, for a
    /// keypair slot that is still leased or a database that is not
    /// exclusive, until the server holding it is gone.
    Refused,
}

impl ExitStatus {
    pub fn code(self) -> i32 {
        match self {
            Self::Success => 0,
            Self::Failed => 1,
            Self::Usage => 2,
            Self::Pending => 3,
            Self::Refused => 4,
        }
    }
}

/// Why a subcommand did not complete.
#[derive(Debug, Error)]
pub enum OwnerCliError {
    #[error("{0}")]
    Configuration(String),
    #[error("could not read {}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not write {}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{} is a symbolic link; an output file is never written through one", path.display())]
    OutputIsSymlink { path: PathBuf },
    #[error("could not open the database connection")]
    Connect(#[source] DatabaseError),
    #[error("the owner phase did not complete")]
    OwnerPhase(#[source] RestrictedRuntimeOwnerError),
    #[error(
        "every schema is prepared and granted, but the keypair-pool generation is pending: \
         {activation}"
    )]
    KeypoolActivationPending { activation: String },
    #[error("the attestor installation did not complete")]
    Attestor(#[source] PhysicalAttestationError),
    #[error("the handoff was not emitted")]
    Handoff(#[source] HandoffError),
    #[error("the composed handoff did not pass the pinned preflight on the runtime connection")]
    Preflight(#[source] RuntimeStartupError),
}

impl OwnerCliError {
    pub fn exit_status(&self) -> ExitStatus {
        match self {
            Self::Configuration(_) | Self::Read { .. } | Self::OutputIsSymlink { .. } => {
                ExitStatus::Usage
            }
            Self::Write { .. } | Self::Connect(_) => ExitStatus::Failed,
            Self::KeypoolActivationPending { .. } => ExitStatus::Pending,
            // The throwaway preflight pool did not close in time: the
            // preflight itself reached no verdict.
            Self::Preflight(RuntimeStartupError::PreflightPoolCloseIndeterminate) => {
                ExitStatus::Failed
            }
            Self::OwnerPhase(_) | Self::Attestor(_) | Self::Handoff(_) | Self::Preflight(_) => {
                if transient_failure(self) {
                    ExitStatus::Failed
                } else {
                    ExitStatus::Refused
                }
            }
        }
    }

    /// Print the error and each of its causes to standard error.
    pub(super) fn report(&self) {
        let label = match self.exit_status() {
            ExitStatus::Pending => "pending",
            ExitStatus::Refused => "refused",
            ExitStatus::Success | ExitStatus::Failed | ExitStatus::Usage => "error",
        };
        eprintln!("{label}: {self}");
        let mut cause = self.source();
        while let Some(error) = cause {
            eprintln!("  caused by: {error}");
            cause = error.source();
        }
    }
}

/// Whether the chain of causes holds something other than a verdict on the
/// database: a failed statement or connection, a lock that stayed contended
/// or timed out, or a rollback that was not acknowledged. A store error
/// that hides such a failure behind an opaque variant is reported as a
/// refusal; its printed text still names the failure.
fn transient_failure(error: &(dyn Error + 'static)) -> bool {
    let mut cause = Some(error);
    while let Some(error) = cause {
        if error.downcast_ref::<sqlx::Error>().is_some()
            || error
                .downcast_ref::<DatabaseError>()
                .is_some_and(database_error_is_transient)
            || error
                .downcast_ref::<PhysicalAttestationError>()
                .is_some_and(attestation_error_is_transient)
            || clustering_transient_failure(error)
        {
            return true;
        }
        cause = error.source();
    }
    false
}

/// Ledger, lineage and attestation errors are verdicts on the database.
/// Every other database error is a failed statement or connection, a lock
/// that was not obtained, an unconfirmed rollback, or a pool that could not
/// be built as configured.
fn database_error_is_transient(error: &DatabaseError) -> bool {
    match error {
        DatabaseError::MigrationLedger(_) | DatabaseError::Lineage(_) => false,
        DatabaseError::PhysicalAttestation(error) => attestation_error_is_transient(error),
        DatabaseError::MigrationApply { source, .. } => database_error_is_transient(source),
        _ => true,
    }
}

fn attestation_error_is_transient(error: &PhysicalAttestationError) -> bool {
    matches!(
        error,
        PhysicalAttestationError::Database(_)
            | PhysicalAttestationError::CommitUnknown { .. }
            | PhysicalAttestationError::PostCommitUnverified { .. }
            | PhysicalAttestationError::InstallationIndeterminate
    )
}

/// The clustering owner steps forward a database error transparently, so it
/// does not appear as a separate cause, and report their own lock timeouts.
#[cfg(feature = "clustering")]
fn clustering_transient_failure(error: &(dyn Error + 'static)) -> bool {
    use crate::clustering::allowlist::AllowlistOwnerPreparationError;
    use crate::clustering::lease::KeypoolOwnerError;

    error
        .downcast_ref::<KeypoolOwnerError>()
        .is_some_and(|error| match error {
            KeypoolOwnerError::Database(error) => database_error_is_transient(error),
            KeypoolOwnerError::LockTimeout => true,
            _ => false,
        })
        || error
            .downcast_ref::<AllowlistOwnerPreparationError>()
            .is_some_and(|error| match error {
                AllowlistOwnerPreparationError::Database(error) => {
                    database_error_is_transient(error)
                }
                AllowlistOwnerPreparationError::LockTimeout => true,
                _ => false,
            })
}

#[cfg(not(feature = "clustering"))]
fn clustering_transient_failure(_error: &(dyn Error + 'static)) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use crate::db::lineage::LineageError;
    use crate::db::physical_attestation::PhysicalAttestationError;
    use crate::db::DatabaseError;
    use crate::server::runtime_owner::RestrictedRuntimeOwnerError;

    use super::{ExitStatus, OwnerCliError};

    fn owner_phase(error: DatabaseError) -> ExitStatus {
        OwnerCliError::OwnerPhase(RestrictedRuntimeOwnerError::GrantInventory(error)).exit_status()
    }

    #[test]
    fn contended_locks_and_unknown_outcomes_are_retryable() {
        for error in [
            DatabaseError::MigrationCatalogFenceContended,
            DatabaseError::MigrationLedgerLockContended,
            DatabaseError::MigrationLockTimeout,
            DatabaseError::MigrationLockRollbackUnconfirmed,
            DatabaseError::CatalogMutationFenceContended,
            DatabaseError::CatalogMutationGateContended,
            DatabaseError::CatalogMutationLockTimeout,
            DatabaseError::CatalogMutationRollbackUnconfirmed,
            DatabaseError::QueryFailed("statement".to_owned()),
            DatabaseError::ConnectionFailed("connection".to_owned()),
            DatabaseError::MigrationApply {
                version: 1,
                description: "migration".to_owned(),
                source: Box::new(DatabaseError::MigrationLockTimeout),
            },
        ] {
            let text = error.to_string();
            assert_eq!(owner_phase(error), ExitStatus::Failed, "{text}");
        }
        for error in [
            PhysicalAttestationError::InstallationIndeterminate,
            PhysicalAttestationError::CommitUnknown {
                operation_id: uuid::Uuid::nil(),
            },
        ] {
            assert_eq!(
                OwnerCliError::Attestor(error).exit_status(),
                ExitStatus::Failed
            );
        }
    }

    #[test]
    fn verdicts_on_the_database_are_refusals() {
        assert_eq!(
            OwnerCliError::OwnerPhase(RestrictedRuntimeOwnerError::UnclassifiedRelation {
                relation: "stray".to_owned(),
            })
            .exit_status(),
            ExitStatus::Refused
        );
        assert_eq!(
            OwnerCliError::OwnerPhase(RestrictedRuntimeOwnerError::DatabaseNotExclusive {
                sessions: 1,
                prepared_transactions: 0,
            })
            .exit_status(),
            ExitStatus::Refused
        );
        assert_eq!(
            owner_phase(DatabaseError::Lineage(LineageError::MissingRow)),
            ExitStatus::Refused
        );
        assert_eq!(
            OwnerCliError::Attestor(PhysicalAttestationError::InstallationConflict).exit_status(),
            ExitStatus::Refused
        );
        assert_eq!(
            OwnerCliError::Attestor(PhysicalAttestationError::NotPrimary).exit_status(),
            ExitStatus::Refused
        );
    }
}
