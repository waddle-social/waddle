use super::*;
use crate::clustering::ClusteringHandles;
use crate::server::routes::websocket::tests::create_test_websocket_state_with_clustering;
use std::io::Write;
use std::sync::Mutex;
use tracing::instrument::WithSubscriber;
use waddle_xmpp::ownership::{
    ClaimEpoch, ClaimError, ClaimSnapshot, ClaimStore, Entity, EntityType, InProcessClaimStore,
    NodeIdentity, ResumeIdentityProof, SharedNodeIdentity, StalePredicate,
};

enum ObservationFailure {
    Backend,
    Timeout,
}

struct ObservationFailingStore {
    inner: InProcessClaimStore,
    failure: ObservationFailure,
}

#[async_trait::async_trait]
impl ClaimStore for ObservationFailingStore {
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

    async fn current_claim(&self, _entity: &Entity) -> Result<Option<ClaimSnapshot>, ClaimError> {
        match self.failure {
            ObservationFailure::Backend => {
                Err(ClaimError::Backend("observation unavailable".into()))
            }
            ObservationFailure::Timeout => std::future::pending().await,
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

    async fn release_many(&self, entities: &[Entity], me: &NodeIdentity) -> Result<(), ClaimError> {
        self.inner.release_many(entities, me).await
    }
}

#[derive(Clone, Default)]
struct WarningCapture(Arc<Mutex<Vec<u8>>>);

impl Write for WarningCapture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl WarningCapture {
    fn subscriber(&self) -> impl tracing::Subscriber + Send + Sync + 'static {
        let writer = self.clone();
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish()
    }

    fn messages(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

async fn state_with_claim_store(store: Arc<dyn ClaimStore>) -> Arc<WebSocketState> {
    create_test_websocket_state_with_clustering(
        ClusteringHandles {
            claim_store: Some(store),
            node_identity: Some(SharedNodeIdentity::new(NodeIdentity::new(
                "local",
                "incarnation",
            ))),
            ..Default::default()
        },
        Arc::new(waddle_xmpp::stream_management::InMemorySmSessionRegistry::new()),
    )
    .await
}

async fn assert_failed_observation_warns_once_per_full_jid(failure: ObservationFailure) {
    let pause_after_setup = matches!(failure, ObservationFailure::Timeout);
    let state = state_with_claim_store(Arc::new(ObservationFailingStore {
        inner: InProcessClaimStore::new(),
        failure,
    }))
    .await;
    // Database fixture setup needs real time; only claim observation uses virtual time.
    if pause_after_setup {
        tokio::time::pause();
    }
    let jid: FullJid = "departed@example.com/web".parse().unwrap();
    let sibling: FullJid = "departed@example.com/mobile".parse().unwrap();
    let capture = WarningCapture::default();
    async {
        for _ in 0..2 {
            assert!(
                acquire_remote_muc_cleanup_origin(&state, &jid)
                    .await
                    .is_none()
            );
        }
        let warnings = capture.messages();
        assert_eq!(warnings.lines().count(), 1, "{warnings}");
        assert!(warnings.contains("failed to observe UserActor claim for remote MUC cleanup"));
        assert!(
            acquire_remote_muc_cleanup_origin(&state, &sibling)
                .await
                .is_none()
        );
        assert_eq!(capture.messages().lines().count(), 2);
    }
    .with_subscriber(capture.subscriber())
    .await;
}

#[tokio::test]
async fn backend_observation_failure_is_closed_and_warning_is_throttled() {
    assert_failed_observation_warns_once_per_full_jid(ObservationFailure::Backend).await;
}

#[tokio::test]
async fn timed_out_observation_is_closed_and_warning_is_throttled() {
    assert_failed_observation_warns_once_per_full_jid(ObservationFailure::Timeout).await;
}

#[tokio::test]
async fn fresh_foreign_claim_defers_cleanup_without_consuming_warning_budget() {
    let store = Arc::new(InProcessClaimStore::new());
    let jid: FullJid = "departed@example.com/web".parse().unwrap();
    let entity = Entity::new(EntityType::UserActor, jid.to_bare().to_string());
    store
        .acquire(&entity, &NodeIdentity::new("foreign", "incarnation"))
        .await
        .unwrap();
    let state = state_with_claim_store(store).await;
    let capture = WarningCapture::default();
    async {
        assert!(
            acquire_remote_muc_cleanup_origin(&state, &jid)
                .await
                .is_none()
        );
        assert!(
            acquire_remote_muc_cleanup_origin(&state, &jid)
                .await
                .is_none()
        );
    }
    .with_subscriber(capture.subscriber())
    .await;
    assert!(capture.messages().is_empty());
    assert!(
        state
            .deps
            .protocol
            .remote_muc_memberships
            .should_warn_cleanup(&jid)
    );
}
