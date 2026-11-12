//! Errors and exit codes of the operator subcommands.

use std::error::Error;
use std::path::PathBuf;

use thiserror::Error;

use crate::db::physical_attestation::PhysicalAttestationError;
use crate::db::runtime_startup::{HandoffError, RuntimeStartupError};
use crate::db::DatabaseError;
use crate::server::runtime_owner::RestrictedRuntimeOwnerError;

/// Process exit codes. A deployment job retries on [`Self::Pending`] and
/// stops for an operator on [`Self::Refused`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitStatus {
    /// The subcommand completed.
    Success,
    /// The connection or the database returned an error, or an outcome is
    /// unknown. The diagnostic says which.
    Failed,
    /// The arguments or the environment are incomplete or invalid. Nothing
    /// was sent to the database.
    Usage,
    /// Every schema is prepared and granted, and the keypair-pool generation
    /// is inside its activation delay. Run `owner-phase` again after the
    /// printed time.
    Pending,
    /// A verifier refused the database, the plan or the document. Running
    /// the subcommand again without changing either refuses again.
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
            Self::Configuration(_) | Self::Read { .. } => ExitStatus::Usage,
            Self::Write { .. } | Self::Connect(_) => ExitStatus::Failed,
            Self::KeypoolActivationPending { .. } => ExitStatus::Pending,
            // An unknown commit outcome or an installation that is not
            // visible is not a verdict on the database.
            Self::Attestor(
                PhysicalAttestationError::CommitUnknown { .. }
                | PhysicalAttestationError::PostCommitUnverified { .. }
                | PhysicalAttestationError::InstallationIndeterminate,
            ) => ExitStatus::Failed,
            Self::OwnerPhase(_) | Self::Attestor(_) | Self::Handoff(_) | Self::Preflight(_) => {
                if database_failure(self) {
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

/// Whether the chain of causes holds a statement or connection the database
/// failed, as opposed to a verifier's refusal. A store error that hides a
/// database failure behind an opaque variant is reported as a refusal; its
/// printed text still names the failure.
fn database_failure(error: &(dyn Error + 'static)) -> bool {
    let mut cause = Some(error);
    while let Some(error) = cause {
        if error.downcast_ref::<sqlx::Error>().is_some()
            || matches!(
                error.downcast_ref::<DatabaseError>(),
                Some(
                    DatabaseError::ConnectionFailed(_)
                        | DatabaseError::QueryFailed(_)
                        | DatabaseError::Internal(_)
                )
            )
            || matches!(
                error.downcast_ref::<PhysicalAttestationError>(),
                Some(PhysicalAttestationError::Database(_))
            )
            || clustering_database_failure(error)
        {
            return true;
        }
        cause = error.source();
    }
    false
}

/// The clustering owner steps forward a database error transparently, so it
/// does not appear as a separate cause.
#[cfg(feature = "clustering")]
fn clustering_database_failure(error: &(dyn Error + 'static)) -> bool {
    use crate::clustering::allowlist::AllowlistOwnerPreparationError;
    use crate::clustering::lease::KeypoolOwnerError;

    matches!(
        error.downcast_ref::<KeypoolOwnerError>(),
        Some(KeypoolOwnerError::Database(_))
    ) || matches!(
        error.downcast_ref::<AllowlistOwnerPreparationError>(),
        Some(AllowlistOwnerPreparationError::Database(_))
    )
}

#[cfg(not(feature = "clustering"))]
fn clustering_database_failure(_error: &(dyn Error + 'static)) -> bool {
    false
}
