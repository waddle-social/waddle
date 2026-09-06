//! Durable ingress authority: immutable planning, atomic commit, bounded execution.
pub mod commit;
mod commit_room;
mod commit_stream;
pub mod decision;
mod durable;
pub mod execute;
pub(crate) mod gc;
pub mod identity;
mod receipts;
mod recorded;
mod rejection;
pub mod restamp;
pub mod submission;
pub mod suppression;

pub use crate::server::routes::interpret::{effects, Deps};
pub use decision::{AliasOutcomeClass, EffectReceiptKey, IngressDecision, IngressDecisionClass};
pub use effects::{
    DurableEffect, ExternalEffect, ImmediateSink, IngressPlan, PlanSuppressionPolicy,
    PlannedEffect, RoomExecutionPath,
};
pub use execute::{ExecutionReport, ExternalOutcome, FrameObligation};
pub use identity::{IngressCanonicalRef, IngressStreamIdentity};
pub use submission::IngressSubmission;

#[cfg(feature = "clustering")]
use crate::db::DatabaseDriver;
use crate::{
    config::{IngressConfig, IngressConfigError, LineageConfig},
    db::{Database, DatabaseConfig, DatabaseError},
    ingress_uow::{IngressUnitOfWork, IngressUowError, SmIngressStreamRepository},
};
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;
#[cfg(feature = "clustering")]
use waddle_xmpp::ownership::SharedNodeIdentity;
use waddle_xmpp::{
    ingress::{SmIngressId, WireHandledCount},
    pending_delivery::SmSessionId,
};

#[derive(Debug, thiserror::Error)]
pub enum IngressStartupError {
    #[error(transparent)]
    Config(#[from] IngressConfigError),
    #[error("clustered ingress requires the canonical node identity")]
    NodeIdentityMissing,
    #[error("an isolated ingress pool cannot share a private in-memory SQLite database")]
    InMemoryDatabase,
    #[error("failed to open the dedicated ingress pool")]
    Pool(#[source] DatabaseError),
    #[error(transparent)]
    UnitOfWork(#[from] IngressUowError),
}

/// Boot-owned handle. Shutdown blocks new work and joins admitted operations and GC.
pub struct IngressAuthority {
    database: Database,
    uow: IngressUnitOfWork,
    config: IngressConfig,
    gc: gc::RetentionGcCoordinator,
    cancellation: CancellationToken,
    force_stop: CancellationToken,
    gc_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    admission: RwLock<bool>,
}

impl IngressAuthority {
    pub async fn new(
        config: IngressConfig,
        database: Database,
        lineage: LineageConfig,
        #[cfg(feature = "clustering")] node_identity: Option<SharedNodeIdentity>,
    ) -> Result<Self, IngressStartupError> {
        config.validate()?;
        #[cfg(feature = "clustering")]
        if database.driver() == DatabaseDriver::Postgres && node_identity.is_none() {
            return Err(IngressStartupError::NodeIdentityMissing);
        }
        if database.is_in_memory_sqlite() {
            return Err(IngressStartupError::InMemoryDatabase);
        }
        let mut pool_config = DatabaseConfig::new(database.driver(), database.database_url());
        pool_config.pool_size = config.pool_size;
        let database = Database::from_config("ingress", &pool_config)
            .await
            .map_err(IngressStartupError::Pool)?;
        let uow = match database.driver() {
            #[cfg(feature = "clustering")]
            DatabaseDriver::Postgres => IngressUnitOfWork::open_with_node_identity(
                database.clone(),
                lineage,
                node_identity.ok_or(IngressStartupError::NodeIdentityMissing)?,
            )?,
            _ => IngressUnitOfWork::open(database.clone(), lineage)?,
        };
        // Validate epoch and lineage before publishing a usable authority.
        uow.begin_with_timeouts(Duration::from_millis(100), Duration::from_millis(250))
            .await?
            .commit()
            .await?;
        let gc = gc::RetentionGcCoordinator::new(database.clone());
        let cancellation = CancellationToken::new();
        let force_stop = CancellationToken::new();
        let gc_task = tokio::spawn(gc::run_retention_gc_coordinator(
            gc.clone(),
            cancellation.clone(),
            force_stop.clone(),
        ));
        Ok(Self {
            database,
            uow,
            config,
            gc,
            cancellation,
            force_stop,
            gc_task: Mutex::new(Some(gc_task)),
            admission: RwLock::new(true),
        })
    }

    pub async fn commit(&self, submission: &IngressSubmission) -> IngressDecision {
        let admission = self.admission.read().await;
        if self.cancellation.is_cancelled() || !*admission {
            return non_advancing(IngressDecisionClass::Storage);
        }
        match commit::commit_submission(&self.uow, submission, self.config.retry_attempts).await {
            Ok(decision) => {
                self.gc.trigger();
                decision
            }
            Err(failure) => non_advancing(failure.class()),
        }
    }

    pub async fn execute(
        &self,
        decision: &IngressDecision,
        sink: &ImmediateSink,
        deps: &Deps<'_>,
    ) -> ExecutionReport {
        let admission = self.admission.read().await;
        if self.cancellation.is_cancelled() || !*admission {
            return ExecutionReport::default();
        }
        execute::execute_effects(
            &self.uow,
            &self.database,
            decision,
            sink,
            deps,
            Duration::from_secs(5),
        )
        .await
    }

    /// Confirm all report frames only after the transport has successfully written them.
    pub async fn complete_frame_obligations(
        &self,
        report: &mut ExecutionReport,
    ) -> Result<bool, execute::ExecutionPersistenceFailure> {
        let admission = self.admission.read().await;
        if self.cancellation.is_cancelled() || !*admission {
            return Err(IngressUowError::AuthorityStopped.into());
        }
        report
            .complete_frame_obligations(&self.uow, &self.database, Duration::from_secs(5))
            .await
    }

    pub async fn flush_checkpoint(
        &self,
        stream: SmIngressId,
        h: WireHandledCount,
    ) -> Result<(), IngressUowError> {
        let admission = self.admission.read().await;
        if self.cancellation.is_cancelled() || !*admission {
            return Err(IngressUowError::AuthorityStopped);
        }
        let mut transaction = self
            .uow
            .begin_with_timeouts(Duration::from_millis(100), Duration::from_millis(250))
            .await?;
        SmIngressStreamRepository::flush_checkpoint(&mut transaction, stream, h).await?;
        transaction.commit().await
    }

    pub async fn load_resume_checkpoint(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<Option<WireHandledCount>, IngressUowError> {
        let admission = self.admission.read().await;
        if self.cancellation.is_cancelled() || !*admission {
            return Err(IngressUowError::AuthorityStopped);
        }
        let mut transaction = self
            .uow
            .begin_with_timeouts(Duration::from_millis(100), Duration::from_millis(250))
            .await?;
        let checkpoint =
            match SmIngressStreamRepository::lookup_unclaimed(&mut transaction, stream_id).await? {
                Some(id) => {
                    SmIngressStreamRepository::load_stream_checkpoint(&mut transaction, id).await?
                }
                None => None,
            };
        transaction.commit().await?;
        Ok(checkpoint)
    }

    pub async fn drain_and_join(&self, budget: Duration) -> bool {
        self.cancellation.cancel();
        let drained = tokio::time::timeout(budget, async {
            *self.admission.write().await = false;
            let mut task = self.gc_task.lock().await;
            if let Some(handle) = task.as_mut() {
                if handle.await.is_err() {
                    return false;
                }
            }
            task.take();
            true
        })
        .await;
        if let Ok(result) = drained {
            return result;
        }
        self.force_stop.cancel();
        false
    }
}

impl Drop for IngressAuthority {
    fn drop(&mut self) {
        self.cancellation.cancel();
        self.force_stop.cancel();
    }
}

fn non_advancing(class: IngressDecisionClass) -> IngressDecision {
    IngressDecision {
        class,
        message_key: None,
        ordinal: None,
        alias: AliasOutcomeClass::NoOrigin,
        verdict: None,
        archive_ids: Vec::new(),
        applied_durable: Default::default(),
        external_dependencies: Vec::new(),
        external: Vec::new(),
        external_receipts: Vec::new(),
        receipts_pending: Vec::new(),
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;

    async fn authority() -> IngressAuthority {
        let database = Database::in_memory("ingress-lifecycle")
            .await
            .expect("open test database");
        let uow = IngressUnitOfWork::open(database.clone(), LineageConfig::default())
            .expect("open test unit of work");
        IngressAuthority {
            gc: gc::RetentionGcCoordinator::new(database.clone()),
            database,
            uow,
            config: IngressConfig::default(),
            cancellation: CancellationToken::new(),
            force_stop: CancellationToken::new(),
            gc_task: Mutex::new(None),
            admission: RwLock::new(true),
        }
    }

    /// Boot refuses a database no other node could ever observe, before any
    /// authority is published.
    #[tokio::test]
    async fn boot_refuses_a_private_in_memory_database() {
        let database = Database::in_memory("ingress-boot")
            .await
            .expect("open test database");
        let error = IngressAuthority::new(
            IngressConfig::default(),
            database,
            LineageConfig::default(),
            #[cfg(feature = "clustering")]
            None,
        )
        .await;
        assert!(matches!(error, Err(IngressStartupError::InMemoryDatabase)));
    }

    #[tokio::test]
    async fn stopped_authority_rejects_checkpoint_operations_before_database_access() {
        let authority = authority().await;
        assert!(authority.drain_and_join(Duration::from_secs(1)).await);
        assert!(matches!(
            authority
                .flush_checkpoint(SmIngressId::new(), WireHandledCount::from_storage(1))
                .await,
            Err(IngressUowError::AuthorityStopped)
        ));
        assert!(matches!(
            authority
                .load_resume_checkpoint(&SmSessionId::new("stopped-stream"))
                .await,
            Err(IngressUowError::AuthorityStopped)
        ));
    }

    #[tokio::test]
    async fn authority_drain_waits_for_admitted_work_and_can_be_rejoined() {
        let authority = authority().await;
        let admitted = authority.admission.read().await;
        assert!(!authority.drain_and_join(Duration::from_millis(1)).await);
        assert!(authority.cancellation.is_cancelled());
        assert!(authority.force_stop.is_cancelled());
        drop(admitted);
        assert!(authority.drain_and_join(Duration::from_secs(1)).await);
    }
}

#[cfg(test)]
mod room_pin_tests;

#[cfg(test)]
pub(crate) mod test_support {
    use crate as waddle_server;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/ingress_support.rs"
    ));
}
