use super::{message_stanza_with_id, realistic_test_session, GatedSnapshotPersistence};
use crate::ownership::{
    ClaimEpoch, ClaimError, ClaimSnapshot, ClaimStore, Entity, EntityType, ExactReleaseOutcome,
    InProcessClaimStore, NodeIdentity, ResumeIdentityProof, SharedNodeIdentity, StalePredicate,
};
use crate::pending_delivery::SmSessionId;
use crate::stream_management::persistence::SmPersistenceStorage;
use crate::stream_management::{
    InMemorySmSessionRegistry, PromotionCaptureCustody, SmIngressAppendKey, SmIngressReceiptKind,
    SmSessionRegistry,
};
use chrono::Utc;
use std::io;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;
use tracing::Instrument;

const RECOVERY_WARNING: &str = "Detached SM append failed after fence recovery was unavailable";

#[derive(Clone, Default)]
struct CaptureWriter {
    bytes: Arc<Mutex<Vec<u8>>>,
    recovery_failed: Arc<Notify>,
}

impl io::Write for CaptureWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut captured = self.bytes.lock().expect("capture lock");
        captured.extend_from_slice(bytes);
        if String::from_utf8_lossy(&captured).contains(RECOVERY_WARNING) {
            self.recovery_failed.notify_one();
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
    type Writer = Self;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

fn recovery_events(writer: &CaptureWriter) -> Vec<serde_json::Value> {
    let captured = writer.bytes.lock().expect("capture lock");
    String::from_utf8_lossy(&captured)
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("JSON log"))
        .filter(|event| event["fields"]["message"] == RECOVERY_WARNING)
        .collect()
}

/// Only the external claim lookup is faulted; ownership changes and reads used
/// to inspect the fixture still go through the real in-process claim store.
#[derive(Clone, Copy, Default)]
enum LookupFault {
    #[default]
    Healthy,
    Failed,
    Stalled,
}

#[derive(Default)]
struct LookupFaultClaims {
    inner: InProcessClaimStore,
    fault: Mutex<LookupFault>,
}

#[async_trait::async_trait]
impl ClaimStore for LookupFaultClaims {
    async fn ensure_schema(&self) -> Result<(), ClaimError> {
        self.inner.ensure_schema().await
    }
    async fn acquire(&self, entity: &Entity, me: &NodeIdentity) -> Result<ClaimEpoch, ClaimError> {
        self.inner.acquire(entity, me).await
    }
    async fn ensure_claimed(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        self.inner.ensure_claimed(entity, me).await
    }
    async fn steal_stale(
        &self,
        entity: &Entity,
        observed: ClaimEpoch,
        staleness: StalePredicate,
        me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        self.inner
            .steal_stale(entity, observed, staleness, me)
            .await
    }
    async fn steal_for_resume(
        &self,
        entity: &Entity,
        observed: ClaimEpoch,
        witness: ResumeIdentityProof,
        me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        self.inner
            .steal_for_resume(entity, observed, witness, me)
            .await
    }
    async fn current_claim(&self, entity: &Entity) -> Result<Option<ClaimSnapshot>, ClaimError> {
        let fault = *self.fault.lock().expect("lookup fault");
        match fault {
            LookupFault::Healthy => self.inner.current_claim(entity).await,
            LookupFault::Failed => Err(ClaimError::Backend(
                "injected claim lookup failure".to_string(),
            )),
            LookupFault::Stalled => std::future::pending().await,
        }
    }
    async fn fence(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
        mine: ClaimEpoch,
    ) -> Result<bool, ClaimError> {
        self.inner.fence(entity, me, mine).await
    }
    async fn release(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
        mine: ClaimEpoch,
    ) -> Result<(), ClaimError> {
        self.inner.release(entity, me, mine).await
    }
    async fn release_exact(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
        mine: ClaimEpoch,
    ) -> Result<ExactReleaseOutcome, ClaimError> {
        self.inner.release_exact(entity, me, mine).await
    }
    async fn release_many(&self, entities: &[Entity], me: &NodeIdentity) -> Result<(), ClaimError> {
        self.inner.release_many(entities, me).await
    }
}

#[derive(Clone, Copy, Debug)]
enum MissingCapture {
    AbsentClaim,
    ForeignOwner,
    MissingSource,
    FailedSourceRead,
    DisabledIdentity,
    ChangedIdentity,
    UnprovenCustody,
    FailedClaimLookup,
    TimedOutClaimLookup,
    PendingReservation,
    FailedRecovery,
}

impl MissingCapture {
    fn reason(self) -> &'static str {
        match self {
            Self::AbsentClaim => "claim_lookup_absent",
            Self::ForeignOwner => "foreign_owner",
            Self::MissingSource => "source_missing",
            Self::FailedSourceRead => "source_read_failed",
            Self::DisabledIdentity => "identity_disabled",
            Self::ChangedIdentity => "identity_changed",
            Self::UnprovenCustody => "custody_unproven",
            Self::FailedClaimLookup => "claim_lookup_failed",
            Self::TimedOutClaimLookup => "claim_lookup_timeout",
            Self::PendingReservation => "reservation_pending",
            Self::FailedRecovery => "recovery_failed",
        }
    }
}

struct Fixture {
    registry: Arc<InMemorySmSessionRegistry>,
    storage: Arc<GatedSnapshotPersistence>,
    stream: SmSessionId,
    claims: Arc<LookupFaultClaims>,
}

async fn fixture(cause: MissingCapture) -> Fixture {
    let stream = SmSessionId::new("append-recovery-diagnostic");
    let storage = Arc::new(GatedSnapshotPersistence::new(stream.as_str()));
    let owner = NodeIdentity::new("append-node", "first-incarnation");
    let identity = SharedNodeIdentity::new(owner.clone());
    let claims = Arc::new(LookupFaultClaims::default());
    let registry = Arc::new(
        InMemorySmSessionRegistry::new()
            .with_persistence(storage.clone())
            .with_claim_store(claims.clone(), identity.clone()),
    );
    registry
        .store_session(realistic_test_session(stream.as_str()))
        .await
        .expect("store source");
    let original = registry
        .current_sm_claim_fence(stream.as_str())
        .expect("original capture");
    registry
        .claim_fences
        .write()
        .expect("fences")
        .remove(stream.as_str());
    let custody = PromotionCaptureCustody::never_claimed();
    if matches!(
        cause,
        MissingCapture::ChangedIdentity | MissingCapture::FailedRecovery
    ) {
        custody.begin_first_claim(&owner);
    }
    registry
        .sessions
        .write()
        .expect("sessions")
        .get_mut(stream.as_str())
        .expect("source")
        .promotion_custody = custody;
    let entity = Entity::new(EntityType::SmSession, stream.as_str());
    match cause {
        MissingCapture::AbsentClaim
        | MissingCapture::ForeignOwner
        | MissingCapture::FailedRecovery => {
            claims
                .release_exact(&entity, &owner, original.epoch())
                .await
                .expect("release old claim");
            if matches!(cause, MissingCapture::ForeignOwner) {
                claims
                    .acquire(
                        &entity,
                        &NodeIdentity::new("other-node", "other-incarnation"),
                    )
                    .await
                    .expect("foreign claim");
            }
        }
        MissingCapture::MissingSource => storage
            .inner
            .delete_session(&stream)
            .await
            .expect("delete source"),
        MissingCapture::FailedSourceRead => storage.fail_source_read.store(true, Ordering::SeqCst),
        MissingCapture::DisabledIdentity => identity.disable().await,
        MissingCapture::ChangedIdentity => {
            identity
                .rotate(NodeIdentity::new("append-node", "second-incarnation"))
                .await
        }
        MissingCapture::UnprovenCustody => {
            registry
                .sessions
                .write()
                .expect("sessions")
                .get_mut(stream.as_str())
                .expect("source")
                .promotion_custody = PromotionCaptureCustody::unproven();
        }
        MissingCapture::FailedClaimLookup => {
            *claims.fault.lock().expect("lookup fault") = LookupFault::Failed;
        }
        MissingCapture::TimedOutClaimLookup => {
            *claims.fault.lock().expect("lookup fault") = LookupFault::Stalled;
        }
        MissingCapture::PendingReservation => {
            assert!(registry.reserve_claim_fence_capacity(stream.as_str()));
        }
    }
    Fixture {
        registry,
        storage,
        stream,
        claims,
    }
}

async fn append(fixture: &Fixture, keyed: bool) -> Result<(), super::SmRegistryError> {
    let stanza = message_stanza_with_id("recovery-diagnostic".into());
    if keyed {
        let key = SmIngressAppendKey {
            message_key: crate::ingress::MessageKey::new(),
            kind: SmIngressReceiptKind::from_storage(3),
            semantic_identity_hash: [19; 32],
            resource: "user@example.com/resource".parse().expect("resource"),
        };
        fixture
            .registry
            .record_keyed_outbound_for_detached_stream_at(
                fixture.stream.as_str(),
                8,
                &stanza,
                Utc::now(),
                key,
            )
            .await
            .map(|_| ())
    } else {
        fixture
            .registry
            .record_outbound_for_detached_stream(fixture.stream.as_str(), &stanza, Utc::now())
            .await
            .map(|_| ())
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn detached_append_failures_expose_specific_recovery_reasons() {
    for cause in [
        MissingCapture::AbsentClaim,
        MissingCapture::ForeignOwner,
        MissingCapture::MissingSource,
        MissingCapture::FailedSourceRead,
        MissingCapture::DisabledIdentity,
        MissingCapture::ChangedIdentity,
        MissingCapture::UnprovenCustody,
        MissingCapture::FailedClaimLookup,
        MissingCapture::TimedOutClaimLookup,
        MissingCapture::PendingReservation,
        MissingCapture::FailedRecovery,
    ] {
        for keyed in [false, true] {
            let writer = CaptureWriter::default();
            let _subscriber = tracing::subscriber::set_default(
                tracing_subscriber::fmt()
                    .json()
                    .with_max_level(tracing::Level::WARN)
                    .with_writer(writer.clone())
                    .finish(),
            );
            let fixture = fixture(cause).await;
            let entity = Entity::new(EntityType::SmSession, fixture.stream.as_str());
            let claim_before = fixture
                .claims
                .inner
                .current_claim(&entity)
                .await
                .expect("claim before");
            fixture
                .storage
                .fail_after_gate
                .store(true, Ordering::SeqCst);
            assert!(
                append(&fixture, keyed).await.is_err(),
                "append must reject {cause:?}"
            );
            let events = recovery_events(&writer);
            assert_eq!(
                events.len(),
                1,
                "one diagnostic for {cause:?}, keyed={keyed}: {events:?}"
            );
            assert_eq!(events[0]["fields"]["reason"], cause.reason());
            assert_eq!(events[0]["fields"]["stream_id"], fixture.stream.as_str());
            assert!(events[0]["fields"]["error"]
                .as_str()
                .expect("backend error")
                .contains("injected failure"));
            assert_eq!(
                fixture
                    .claims
                    .inner
                    .current_claim(&entity)
                    .await
                    .expect("claim after"),
                claim_before,
                "diagnostic recovery must not steal or acquire authority"
            );
            let stored = fixture
                .storage
                .inner
                .get_session(&fixture.stream)
                .await
                .expect("canonical source");
            if let Some(stored) = stored {
                assert_eq!(stored.outbound_count, 7);
            }
            assert_eq!(
                fixture
                    .storage
                    .inner
                    .list_unacked(&fixture.stream)
                    .await
                    .expect("replay rows")
                    .len(),
                if matches!(cause, MissingCapture::MissingSource) {
                    0
                } else {
                    2
                }
            );
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn successful_portable_unfenced_appends_do_not_warn_about_recovery() {
    for cause in [MissingCapture::AbsentClaim, MissingCapture::ForeignOwner] {
        for keyed in [false, true] {
            let writer = CaptureWriter::default();
            let _subscriber = tracing::subscriber::set_default(
                tracing_subscriber::fmt()
                    .json()
                    .with_max_level(tracing::Level::WARN)
                    .with_writer(writer.clone())
                    .finish(),
            );
            let fixture = fixture(cause).await;
            let entity = Entity::new(EntityType::SmSession, fixture.stream.as_str());
            let claim_before = fixture
                .claims
                .inner
                .current_claim(&entity)
                .await
                .expect("claim before");
            append(&fixture, keyed)
                .await
                .expect("portable adapter accepts no fence");
            assert!(recovery_events(&writer).is_empty());
            assert_eq!(
                fixture
                    .claims
                    .inner
                    .current_claim(&entity)
                    .await
                    .expect("claim after"),
                claim_before
            );
            assert_eq!(
                fixture
                    .storage
                    .inner
                    .get_session(&fixture.stream)
                    .await
                    .expect("source")
                    .expect("canonical source")
                    .outbound_count,
                8
            );
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn cancelled_append_preserves_specific_recovery_warning_after_storage_settles() {
    for keyed in [false, true] {
        let writer = CaptureWriter::default();
        let _subscriber = tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .json()
                .with_max_level(tracing::Level::WARN)
                .with_writer(writer.clone())
                .finish(),
        );
        let fixture = Arc::new(fixture(MissingCapture::AbsentClaim).await);
        fixture.storage.armed.store(true, Ordering::SeqCst);
        fixture
            .storage
            .fail_after_gate
            .store(true, Ordering::SeqCst);
        let appending = tokio::spawn({
            let fixture = fixture.clone();
            async move { append(&fixture, keyed).await }.instrument(tracing::warn_span!(
                "cancelled_append",
                context = "caller_trace"
            ))
        });
        tokio::time::timeout(Duration::from_secs(5), fixture.storage.reached.notified())
            .await
            .expect("storage task reached");
        appending.abort();
        assert!(appending
            .await
            .expect_err("caller cancelled")
            .is_cancelled());
        fixture.storage.proceed.notify_one();
        tokio::time::timeout(Duration::from_secs(5), writer.recovery_failed.notified())
            .await
            .expect("settle task preserves diagnostic");
        let events = recovery_events(&writer);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["fields"]["reason"], "claim_lookup_absent");
        assert_eq!(events[0]["span"]["context"], "caller_trace");
        assert_eq!(
            fixture
                .storage
                .inner
                .get_session(&fixture.stream)
                .await
                .expect("source")
                .expect("canonical source")
                .outbound_count,
            7
        );
    }
}
