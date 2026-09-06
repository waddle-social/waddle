//! Durable ingress authority: immutable planning, atomic commit, bounded execution.
mod capture;
pub use capture::{IngressEffectCapture, IngressEffectCaptureSnapshot};
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
pub use execute::{ExecutionReport, ExternalOutcome};
pub use identity::{IngressCanonicalRef, IngressStreamIdentity};
pub use submission::IngressSubmission;

#[cfg(feature = "clustering")]
use crate::db::DatabaseDriver;
use crate::{
    config::{IngressConfig, IngressConfigError, LineageConfig},
    db::{Database, DatabaseConfig, DatabaseError},
    ingress_uow::{
        CanonicalMessageRepository, IngressUnitOfWork, IngressUowError, SmIngressRepository,
        SmIngressStreamRepository,
    },
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex as StdMutex, Weak},
    time::Duration,
};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngressRetirementOutcome {
    Deleted,
    DeferredClaim,
    StreamMissing,
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
    streams: StdMutex<HashMap<SmSessionId, Weak<RwLock<()>>>>,
    retirement_cursor: Mutex<Option<SmSessionId>>,
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
            streams: StdMutex::new(HashMap::new()),
            retirement_cursor: Mutex::new(None),
        })
    }

    /// Real database-backed fixture, enrolled through the production lineage repository.
    /// Fixtures share a fixed deployment UUID so reusing a test pool re-attests its row.
    #[cfg(test)]
    pub(crate) async fn for_test(database: Database) -> Self {
        let lineage = test_lineage_config();
        crate::db::lineage::enroll(&database, &lineage)
            .await
            .expect("enroll test ingress lineage");
        let uow = IngressUnitOfWork::open(database.clone(), lineage)
            .expect("open test ingress unit of work");
        Self {
            gc: gc::RetentionGcCoordinator::new(database.clone()),
            database,
            uow,
            config: IngressConfig::default(),
            cancellation: CancellationToken::new(),
            force_stop: CancellationToken::new(),
            gc_task: Mutex::new(None),
            admission: RwLock::new(true),
            streams: StdMutex::new(HashMap::new()),
            retirement_cursor: Mutex::new(None),
        }
    }

    fn stream_activity(&self, stream_id: &SmSessionId) -> Arc<RwLock<()>> {
        let mut streams = self
            .streams
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        streams.retain(|_, activity| activity.strong_count() > 0);
        if let Some(activity) = streams.get(stream_id).and_then(Weak::upgrade) {
            return activity;
        }
        let activity = Arc::new(RwLock::new(()));
        streams.insert(stream_id.clone(), Arc::downgrade(&activity));
        activity
    }

    #[cfg(test)]
    pub(crate) async fn hold_test_commit(
        &self,
        stream_id: &SmSessionId,
    ) -> (
        tokio::sync::RwLockReadGuard<'_, bool>,
        tokio::sync::OwnedRwLockReadGuard<()>,
    ) {
        (
            self.admission.read().await,
            self.stream_activity(stream_id).read_owned().await,
        )
    }

    #[cfg(test)]
    pub(crate) async fn block_test_stream(
        &self,
        stream_id: &SmSessionId,
    ) -> tokio::sync::OwnedRwLockWriteGuard<()> {
        self.stream_activity(stream_id).write_owned().await
    }

    pub async fn wait_for_stream_idle(&self, stream_id: &SmSessionId, budget: Duration) -> bool {
        tokio::time::timeout(budget, self.stream_activity(stream_id).write_owned())
            .await
            .is_ok()
    }

    pub async fn enroll_stream(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<SmIngressId, IngressUowError> {
        let admission = self.admission.read().await;
        if self.cancellation.is_cancelled() || !*admission {
            return Err(IngressUowError::AuthorityStopped);
        }
        let _stream_guard = self.stream_activity(stream_id).write_owned().await;
        let mut transaction = self
            .uow
            .begin_with_timeouts(Duration::from_millis(100), Duration::from_millis(250))
            .await?;
        let id = SmIngressStreamRepository::mint(&mut transaction, stream_id).await?;
        transaction.commit().await?;
        Ok(id)
    }

    pub async fn lookup_stream(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<Option<SmIngressId>, IngressUowError> {
        let admission = self.admission.read().await;
        if self.cancellation.is_cancelled() || !*admission {
            return Err(IngressUowError::AuthorityStopped);
        }
        let mut transaction = self
            .uow
            .begin_with_timeouts(Duration::from_millis(100), Duration::from_millis(250))
            .await?;
        let id = SmIngressStreamRepository::lookup_unclaimed(&mut transaction, stream_id).await?;
        transaction.commit().await?;
        Ok(id)
    }

    /// Rotate through durable retirement candidates without retaining a work queue.
    pub async fn next_retirement_candidates(&self) -> Result<Vec<SmSessionId>, IngressUowError> {
        const PAGE_SIZE: u32 = 64;
        let admission = self.admission.read().await;
        if self.cancellation.is_cancelled() || !*admission {
            return Err(IngressUowError::AuthorityStopped);
        }
        let mut cursor = self.retirement_cursor.lock().await;
        let mut transaction = self
            .uow
            .begin_with_timeouts(Duration::from_millis(100), Duration::from_millis(250))
            .await?;
        let mut streams = SmIngressStreamRepository::retirement_candidates(
            &mut transaction,
            cursor.as_ref(),
            PAGE_SIZE,
        )
        .await?;
        if streams.is_empty() && cursor.is_some() {
            streams =
                SmIngressStreamRepository::retirement_candidates(&mut transaction, None, PAGE_SIZE)
                    .await?;
        }
        transaction.commit().await?;
        if streams.is_empty() {
            *cursor = None;
        }
        Ok(streams)
    }

    /// Advance immediately before attempting a candidate, never past an unattempted row.
    /// A cancelled attempt remains durable and will be retried after the cursor wraps.
    pub async fn mark_retirement_candidate_attempted(&self, stream: &SmSessionId) {
        *self.retirement_cursor.lock().await = Some(stream.clone());
    }

    pub async fn forget_stream(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<IngressRetirementOutcome, IngressUowError> {
        let admission = self.admission.read().await;
        if self.cancellation.is_cancelled() || !*admission {
            return Err(IngressUowError::AuthorityStopped);
        }
        let _stream_guard = self.stream_activity(stream_id).write_owned().await;
        let mut transaction = self
            .uow
            .begin_with_timeouts(Duration::from_millis(100), Duration::from_millis(250))
            .await?;
        #[cfg(feature = "clustering")]
        if self.database.driver() == DatabaseDriver::Postgres
            && !SmIngressStreamRepository::fence_claim_absence_for_retirement(
                &mut transaction,
                stream_id,
            )
            .await?
        {
            transaction.commit().await?;
            return Ok(IngressRetirementOutcome::DeferredClaim);
        }
        let Some(id) =
            SmIngressStreamRepository::lookup_unclaimed(&mut transaction, stream_id).await?
        else {
            transaction.commit().await?;
            return Ok(IngressRetirementOutcome::StreamMissing);
        };
        for key in SmIngressRepository::message_keys_for_stream(&mut transaction, id).await? {
            CanonicalMessageRepository::terminalize(&mut transaction, key, chrono::Utc::now())
                .await?;
        }
        SmIngressRepository::delete_for_stream(&mut transaction, id).await?;
        SmIngressStreamRepository::delete_unclaimed(&mut transaction, stream_id).await?;
        transaction.commit().await?;
        self.gc.trigger();
        Ok(IngressRetirementOutcome::Deleted)
    }

    pub async fn commit(&self, submission: &IngressSubmission) -> IngressDecision {
        let admission = self.admission.read().await;
        if self.cancellation.is_cancelled() || !*admission {
            return non_advancing(IngressDecisionClass::Storage);
        }
        let _stream_guard = match &submission.identity {
            IngressStreamIdentity::Resumable { stream_id, .. } => {
                Some(self.stream_activity(stream_id).read_owned().await)
            }
            _ => None,
        };
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
fn test_lineage_config() -> LineageConfig {
    LineageConfig {
        deployment_uuid: Some(
            "018f47b2-4b2e-7a3a-9a4c-52a5a6a90001"
                .parse()
                .expect("fixture deployment UUID"),
        ),
        action: None,
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;

    async fn authority() -> IngressAuthority {
        let database = Database::in_memory("ingress-lifecycle")
            .await
            .expect("open test database");
        IngressAuthority::for_test(database).await
    }

    #[tokio::test]
    async fn stream_idle_waits_for_commit_and_releases_on_cancellation() {
        let authority = authority().await;
        let stream = SmSessionId::new("pending-commit");
        let held = authority.hold_test_commit(&stream).await;
        assert!(
            !authority
                .wait_for_stream_idle(&stream, Duration::from_millis(1))
                .await
        );
        assert!(
            authority
                .wait_for_stream_idle(&SmSessionId::new("other"), Duration::from_millis(10))
                .await
        );
        drop(held);
        assert!(
            authority
                .wait_for_stream_idle(&stream, Duration::from_millis(10))
                .await
        );
    }

    async fn enrollment_checkpoint_and_retirement_round_trip(authority: &IngressAuthority) {
        let stream = SmSessionId::new("enrolled-stream");
        let id = authority.enroll_stream(&stream).await.expect("enroll");
        assert_eq!(
            authority
                .enroll_stream(&stream)
                .await
                .expect("enroll twice"),
            id
        );
        assert_eq!(
            authority.lookup_stream(&stream).await.expect("lookup"),
            Some(id)
        );
        assert_eq!(
            authority
                .next_retirement_candidates()
                .await
                .expect("candidate page"),
            vec![stream.clone()]
        );
        authority.mark_retirement_candidate_attempted(&stream).await;
        assert_eq!(
            authority
                .next_retirement_candidates()
                .await
                .expect("candidate cursor wraps"),
            vec![stream.clone()]
        );
        authority
            .flush_checkpoint(id, WireHandledCount::from_storage(3))
            .await
            .expect("flush checkpoint");
        assert_eq!(
            authority
                .load_resume_checkpoint(&stream)
                .await
                .expect("checkpoint"),
            Some(WireHandledCount::from_storage(3))
        );
        #[cfg(feature = "clustering")]
        if authority.database.driver() == DatabaseDriver::Postgres {
            authority.database.execute("INSERT INTO clustering_claims (entity, entity_type) VALUES ('sm_session:enrolled-stream', 'sm_session')").await.expect("retain promotion claim");
            assert_eq!(
                authority
                    .forget_stream(&stream)
                    .await
                    .expect("retirement while claimed"),
                IngressRetirementOutcome::DeferredClaim
            );
            assert_eq!(
                authority
                    .lookup_stream(&stream)
                    .await
                    .expect("retained ingress stream"),
                Some(id)
            );
            authority
                .database
                .execute(
                    "DELETE FROM clustering_claims WHERE entity = 'sm_session:enrolled-stream'",
                )
                .await
                .expect("confirm promotion and release claim");
        }
        assert_eq!(
            authority.forget_stream(&stream).await.expect("retire"),
            IngressRetirementOutcome::Deleted
        );
        assert_eq!(
            authority
                .lookup_stream(&stream)
                .await
                .expect("lookup retired"),
            None
        );
        assert_eq!(
            authority
                .forget_stream(&stream)
                .await
                .expect("retire absent"),
            IngressRetirementOutcome::StreamMissing
        );
    }

    #[tokio::test]
    async fn retirement_scan_pages_past_live_streams_and_wraps() {
        let authority = authority().await;
        crate::db::MigrationRunner::global()
            .run(&authority.database)
            .await
            .expect("migrate");
        for index in 0..65 {
            authority
                .enroll_stream(&SmSessionId::new(format!("stream-{index:03}")))
                .await
                .expect("enroll");
        }
        let first = authority
            .next_retirement_candidates()
            .await
            .expect("first page");
        assert_eq!(first.len(), 64);
        authority
            .mark_retirement_candidate_attempted(first.last().expect("first page tail"))
            .await;

        assert_eq!(
            authority
                .next_retirement_candidates()
                .await
                .expect("tail page"),
            vec![SmSessionId::new("stream-064")]
        );
        authority
            .mark_retirement_candidate_attempted(&SmSessionId::new("stream-064"))
            .await;
        assert_eq!(
            authority
                .next_retirement_candidates()
                .await
                .expect("wrapped page"),
            first
        );
    }

    #[tokio::test]
    async fn sqlite_enrollment_checkpoint_and_retirement() {
        let authority = authority().await;
        crate::db::MigrationRunner::global()
            .run(&authority.database)
            .await
            .expect("migrate ingress");
        enrollment_checkpoint_and_retirement_round_trip(&authority).await;
    }

    #[tokio::test]
    async fn postgres_enrollment_checkpoint_and_retirement() {
        let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
            eprintln!("skipping postgres_enrollment_checkpoint_and_retirement: WADDLE_TEST_POSTGRES_URL not set");
            return;
        };
        let admin = sqlx::PgPool::connect(&database_url)
            .await
            .expect("postgres admin");
        let schema = format!("ingress_lifecycle_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .expect("create schema");
        let mut url = url::Url::parse(&database_url).expect("database URL");
        url.query_pairs_mut()
            .append_pair("options", &format!("-c search_path={schema}"));
        let config = DatabaseConfig::new(crate::db::DatabaseDriver::Postgres, url.to_string());
        let db = Database::from_config("ingress-lifecycle", &config)
            .await
            .expect("database");
        crate::db::MigrationRunner::single()
            .run(&db)
            .await
            .expect("migrate");
        let lineage = test_lineage_config();
        let mut authority = IngressAuthority::for_test(db.clone()).await;
        #[cfg(feature = "clustering")]
        {
            db.execute("CREATE TABLE IF NOT EXISTS clustering_claims (entity TEXT NOT NULL, entity_type TEXT NOT NULL, PRIMARY KEY (entity, entity_type))").await.expect("claims schema");
            authority.uow = IngressUnitOfWork::open_with_node_identity(
                db,
                lineage,
                SharedNodeIdentity::new(waddle_xmpp::ownership::NodeIdentity::new(
                    "lifecycle",
                    "test",
                )),
            )
            .expect("unit of work");
        }
        #[cfg(not(feature = "clustering"))]
        {
            authority.uow = IngressUnitOfWork::open(db, lineage).expect("unit of work");
        }
        enrollment_checkpoint_and_retirement_round_trip(&authority).await;
        drop(authority);
        sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
            .execute(&admin)
            .await
            .expect("drop schema");
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
