//! Narrow integration-test access to the unwired shared correctness facade.

use crate::config::{IngressConfig, LineageConfig};
use crate::db::DatabasePool;
use crate::ingress::commit::IngressCommitFailure;
use crate::ingress::{
    DarkSharedCorrectness, DarkSharedCorrectnessError, IngressDecision, IngressSubmission,
};
use waddle_xmpp::mam::ArchivedMessage;
use waddle_xmpp_core::xep0359::StanzaId;

#[cfg(feature = "clustering")]
use waddle_xmpp::ownership::SharedNodeIdentity;

/// Test-only owner of the opaque facade; it exposes no underlying handles.
///
/// ```compile_fail
/// use waddle_server::test_support::SharedCorrectnessFixture;
/// fn escape(fixture: &SharedCorrectnessFixture) {
///     let _ = fixture.database();
/// }
/// ```
///
/// ```compile_fail
/// use waddle_server::ingress::DarkSharedCorrectness;
/// ```
pub struct SharedCorrectnessFixture(DarkSharedCorrectness);

#[derive(Debug, thiserror::Error)]
pub enum SharedCorrectnessSubmitError {
    #[error("the fixture only accepts ephemeral stream identities")]
    UnsupportedStreamIdentity,
    #[error("the fixture requires an origin identifier")]
    MissingOriginId,
    #[error(transparent)]
    Commit(#[from] IngressCommitFailure),
    #[error("shared correctness fixture failed")]
    Other,
}

impl From<DarkSharedCorrectnessError> for SharedCorrectnessSubmitError {
    fn from(error: DarkSharedCorrectnessError) -> Self {
        match error {
            DarkSharedCorrectnessError::UnsupportedStreamIdentity => {
                Self::UnsupportedStreamIdentity
            }
            DarkSharedCorrectnessError::MissingOriginId => Self::MissingOriginId,
            DarkSharedCorrectnessError::Commit(error) => Self::Commit(error),
            _ => Self::Other,
        }
    }
}

impl SharedCorrectnessFixture {
    pub fn from_pool(
        pool: &DatabasePool,
        config: &IngressConfig,
        lineage: LineageConfig,
        #[cfg(feature = "clustering")] node_identity: Option<SharedNodeIdentity>,
    ) -> anyhow::Result<Self> {
        Ok(Self(DarkSharedCorrectness::from_pool(
            pool,
            config,
            lineage,
            #[cfg(feature = "clustering")]
            node_identity,
        )?))
    }

    /// Attach to an owner-initialized PostgreSQL MAM schema without DDL.
    /// The schema observation is a snapshot; fencing old sessions and DDL is
    /// still an external prerequisite. The returned fixture exposes no pool.
    pub async fn attach_postgres_mam_from_pool(
        pool: &DatabasePool,
        config: &IngressConfig,
        lineage: LineageConfig,
        owner: &crate::db::DarkMigrationOwnerConfig,
        #[cfg(feature = "clustering")] node_identity: Option<SharedNodeIdentity>,
    ) -> anyhow::Result<Self> {
        let fixture = Self::from_pool(
            pool,
            config,
            lineage,
            #[cfg(feature = "clustering")]
            node_identity,
        )?;
        crate::db::bootstrap::verify_mam_runtime_attachment(pool.global(), owner).await?;
        Ok(fixture)
    }

    pub async fn submit(
        &self,
        submission: &IngressSubmission,
    ) -> Result<IngressDecision, SharedCorrectnessSubmitError> {
        self.0.submit(submission).await.map_err(Into::into)
    }

    /// The test hook simulates lost acknowledgement after a successful commit;
    /// it does not model an in-flight PostgreSQL backend.
    pub async fn submit_with_simulated_lost_commit_ack(
        &self,
        submission: &IngressSubmission,
    ) -> Result<IngressDecision, SharedCorrectnessSubmitError> {
        crate::ingress::commit::commit_hooks::with_simulated_lost_commit_ack(
            self.0.submit(submission),
        )
        .await
        .map_err(Into::into)
    }

    pub async fn lookup_archive_or_stanza_id(
        &self,
        stanza: &StanzaId,
    ) -> anyhow::Result<Option<ArchivedMessage>> {
        Ok(self.0.lookup_archive_or_stanza_id(stanza).await?)
    }
}

/// Probe PUBLIC privileges inside transactions that are rolled back.
/// The caller supplies a disposable administrator database; no grant commits.
pub async fn probe_public_mam_acl_rollback(
    database: &crate::db::Database,
    owner: &crate::db::DarkMigrationOwnerConfig,
) -> anyhow::Result<bool> {
    let schema = &owner
        .expected_lineage()
        .postgres_identity
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("PostgreSQL identity required"))?
        .schema
        .name
        .0;
    crate::db::physical_attestation::validate_identifier(schema, "MAM schema")?;
    let relation = format!(
        "{}.mam_messages",
        crate::db::physical_attestation::quote_identifier(schema)
    );
    for privilege in ["SELECT", "SELECT (body)", "TRUNCATE"] {
        let mut tx = database.begin().await?;
        tx.execute(&format!("GRANT {privilege} ON {relation} TO PUBLIC"), ())
            .await?;
        let refused = matches!(
            crate::db::bootstrap::verify_mam_runtime_acl(&mut tx, schema, owner.runtime().as_str())
                .await,
            Err(crate::db::bootstrap::RuntimeAttachmentError::UnsafeMamRuntime)
        );
        tx.rollback().await?;
        if !refused {
            return Ok(false);
        }
    }
    Ok(true)
}
