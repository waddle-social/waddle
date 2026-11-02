//! Unwired composition of ingress and MAM over one admitted database pool.

use crate::config::{IngressConfig, LineageConfig};
use crate::db::physical_attestation::AttestedPostgresWriterCapability;
use crate::db::{Database, DatabaseDriver, DatabaseError, DatabasePool};
use crate::ingress::commit::{commit_submission, IngressCommitFailure};
use crate::ingress::{IngressDecision, IngressStreamIdentity, IngressSubmission};
use crate::ingress_uow::{IngressUnitOfWork, IngressUowError};
use waddle_xmpp::mam::{ArchivedMessage, MamStorageError, SqlxMamStorage};
use waddle_xmpp_core::xep0359::StanzaId;

tokio::task_local! {
    static MAM_ROLLBACK_GATE: (std::sync::Arc<tokio::sync::Notify>, std::sync::Arc<tokio::sync::Notify>);
}

/// Test-only fault boundary after the MAM query and before transaction cleanup.
pub(crate) async fn with_mam_rollback_gate<F: std::future::Future>(
    entered: std::sync::Arc<tokio::sync::Notify>,
    proceed: std::sync::Arc<tokio::sync::Notify>,
    future: F,
) -> F::Output {
    MAM_ROLLBACK_GATE.scope((entered, proceed), future).await
}

#[cfg(feature = "clustering")]
use waddle_xmpp::ownership::SharedNodeIdentity;

/// Refusal to construct or use the dark shared-pool facade.
#[derive(Debug, thiserror::Error)]
pub(crate) enum DarkSharedCorrectnessError {
    #[error(
        "shared PostgreSQL authority requires an attested main pool and its reserved ingress lane"
    )]
    AttestedIngressLaneRequired,
    #[error("invalid ingress admission configuration")]
    InvalidIngressConfig,
    #[error("in-memory SQLite requires exactly one physical pool connection")]
    InMemorySqlitePoolRequiresOneConnection,
    #[error("the dark shared facade only accepts ephemeral stream identities")]
    UnsupportedStreamIdentity,
    #[error("the dark shared facade requires an origin identifier")]
    MissingOriginId,
    #[error(transparent)]
    Commit(#[from] IngressCommitFailure),
    #[error("failed to bound the MAM read transaction")]
    ReadBounds,
    #[error(transparent)]
    Database(#[from] DatabaseError),
    #[error(transparent)]
    Ingress(#[from] IngressUowError),
    #[error(transparent)]
    Mam(#[from] MamStorageError),
}

/// A closed, unwired facade over the exact main and reserved ingress lanes.
///
/// The submission operation accepts only an ephemeral stream identity with an
/// origin identifier and uses the existing generic ingress contract: a
/// caller must obtain its plan and principal from a trusted source. This is
/// not a new direct-message-only authorization or an activation capability.
/// Other `Database` handles may still exist outside this facade. A caller
/// cannot extract a pool, connection, transaction, or unit of work from it.
///
/// A cancelled or unacknowledged commit remains indeterminate. The caller
/// must not treat it as a rollback, automatically retry, or execute effects
/// based on it. An origin identifier does not establish a commit barrier;
/// conflicting aliases can still mint new denial keys.
pub(crate) struct DarkSharedCorrectness {
    main: Database,
    ingress: IngressUnitOfWork,
    postgres: Option<AttestedPostgresWriterCapability>,
    retry_attempts: usize,
}

impl DarkSharedCorrectness {
    pub(crate) fn uses_postgres(&self) -> bool {
        self.postgres.is_some()
    }

    /// Bind both lanes from one pool. SQLite retains the exact global pool;
    /// PostgreSQL requires the hook-bearing pool's own reserved ingress lane.
    pub(crate) fn from_pool(
        pool: &DatabasePool,
        config: &IngressConfig,
        lineage: LineageConfig,
        #[cfg(feature = "clustering")] node_identity: Option<SharedNodeIdentity>,
    ) -> Result<Self, DarkSharedCorrectnessError> {
        config
            .validate()
            .map_err(|_| DarkSharedCorrectnessError::InvalidIngressConfig)?;
        let main = pool.global().clone();
        let ingress = pool.ingress();
        let postgres = match main.driver() {
            DatabaseDriver::Sqlite => {
                let sqlite = main
                    .sqlite_pool()
                    .ok_or(DarkSharedCorrectnessError::InMemorySqlitePoolRequiresOneConnection)?;
                if main.is_in_memory_sqlite() && sqlite.options().get_max_connections() != 1 {
                    return Err(
                        DarkSharedCorrectnessError::InMemorySqlitePoolRequiresOneConnection,
                    );
                }
                None
            }
            DatabaseDriver::Postgres => {
                if !ingress.is_ingress_lane()
                    || ingress.ingress_pool_size() != Some(config.pool_size)
                {
                    return Err(DarkSharedCorrectnessError::AttestedIngressLaneRequired);
                }
                let capability = main
                    .attested_pg_writer_capability()
                    .ok_or(DarkSharedCorrectnessError::AttestedIngressLaneRequired)?;
                ingress
                    .verify_attested_pg_writer_capability(&capability)
                    .map_err(DatabaseError::from)?;
                capability.attested_lineage_for_deployment(lineage.deployment_uuid)?;
                Some(capability)
            }
        };
        #[cfg(feature = "clustering")]
        let ingress = match node_identity {
            Some(identity) => {
                IngressUnitOfWork::open_with_node_identity(ingress, lineage, identity)?
            }
            None => IngressUnitOfWork::open(ingress, lineage)?,
        };
        #[cfg(not(feature = "clustering"))]
        let ingress = IngressUnitOfWork::open(ingress, lineage)?;
        Ok(Self {
            main,
            ingress,
            postgres,
            retry_attempts: config.retry_attempts,
        })
    }

    /// Commit the existing trusted ingress submission as one operation.
    pub(crate) async fn submit(
        &self,
        submission: &IngressSubmission,
    ) -> Result<IngressDecision, DarkSharedCorrectnessError> {
        if !matches!(
            &submission.identity,
            IngressStreamIdentity::Ephemeral { .. }
        ) {
            return Err(DarkSharedCorrectnessError::UnsupportedStreamIdentity);
        }
        if submission.digest_input.origin().is_none() {
            return Err(DarkSharedCorrectnessError::MissingOriginId);
        }
        Ok(commit_submission(&self.ingress, submission, self.retry_attempts).await?)
    }

    /// Look up an archive-scoped stanza using the main admission lane.
    pub(crate) async fn lookup_archive_or_stanza_id(
        &self,
        stanza: &StanzaId,
    ) -> Result<Option<ArchivedMessage>, DarkSharedCorrectnessError> {
        let mut tx = self.main.begin().await?;
        let result = if let Some(capability) = &self.postgres {
            tx.execute(
                "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ ONLY",
                (),
            )
            .await?;
            crate::db::read_only_timeout::bound_postgres_waits(&mut tx)
                .await
                .map_err(|_| DarkSharedCorrectnessError::ReadBounds)?;
            capability.attest_transaction(&mut tx).await?;
            let schema = &capability.identity().schema.name.0;
            let connection = tx
                .postgres_connection()
                .ok_or(DarkSharedCorrectnessError::AttestedIngressLaneRequired)?;
            SqlxMamStorage::lookup_archive_or_stanza_id_on_postgres_connection_in_schema(
                connection, schema, stanza,
            )
            .await
        } else {
            let connection = tx
                .sqlite_connection()
                .ok_or(DarkSharedCorrectnessError::AttestedIngressLaneRequired)?;
            SqlxMamStorage::lookup_archive_or_stanza_id_on_sqlite_connection(connection, stanza)
                .await
        };
        // A failed or cancelled rollback must never become a definitive MAM
        // miss, including when decoding the row returned a permanent error.
        if let Ok((entered, proceed)) = MAM_ROLLBACK_GATE.try_with(Clone::clone) {
            entered.notify_one();
            proceed.notified().await;
        }
        tx.rollback().await?;
        result.map_err(Into::into)
    }
}
