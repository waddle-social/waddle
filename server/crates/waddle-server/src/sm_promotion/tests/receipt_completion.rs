//! Each actual promote -> canonical-confirm path releases portable receipts.
use super::{
    bare, detached_session_with_unacked, dm_xml, full, promote_displaced_sessions,
    run_test_shutdown_drain, test_user_registry, Arc, ConnectionRegistry, DisplacedPromotionDeps,
    Instant, PendingDeliveryStorage, QuotaPolicy,
};
use crate::pending_delivery::DatabasePendingDeliveryStorage;
use crate::sm_persistence::DatabaseSmPersistence;
use waddle_xmpp::pending_delivery::{PendingPayload, PendingRow, PendingRowId, SmSessionId};
use waddle_xmpp::stream_management::persistence::SmPersistenceStorage;
use waddle_xmpp::stream_management::{InMemorySmSessionRegistry, SmSessionRegistry};

struct ReceiptCompletionFixture {
    _directory: tempfile::TempDir,
    source: Arc<DatabaseSmPersistence>,
    registry: Arc<InMemorySmSessionRegistry>,
    pending: Arc<DatabasePendingDeliveryStorage>,
    stream: SmSessionId,
}

impl ReceiptCompletionFixture {
    async fn new(stream: &str) -> Self {
        let directory = tempfile::tempdir().expect("independent portable databases");
        let source_url = format!(
            "sqlite://{}?mode=rwc",
            directory.path().join("sm.db").display()
        );
        let pending_url = format!(
            "sqlite://{}?mode=rwc",
            directory.path().join("pending.db").display()
        );
        let source = Arc::new(
            DatabaseSmPersistence::open(Some(&source_url))
                .await
                .expect("canonical SM database"),
        );
        let registry =
            Arc::new(InMemorySmSessionRegistry::with_capacity(1).with_persistence(source.clone()));
        let pending = Arc::new(
            DatabasePendingDeliveryStorage::open(Some(&pending_url), QuotaPolicy::Unlimited)
                .await
                .expect("portable pending database"),
        );
        let xml = dm_xml(
            "bob@example.com/web",
            "alice@example.com",
            "interrupted portable handoff",
        );
        let message = xmpp_parsers::message::Message::try_from(
            xml.parse::<xmpp_parsers::minidom::Element>()
                .expect("message XML"),
        )
        .expect("peer message");
        let mut session =
            detached_session_with_unacked(stream, full("alice@example.com/phone"), vec![xml]);
        session.max_resume_time = Some(0);
        session.detached_at = Instant::now() - std::time::Duration::from_secs(30);
        let received_at = session.unacked_stanzas[0].original_receipt_at;
        assert!(registry
            .store_session(session)
            .await
            .expect("store expired typed source")
            .is_empty());
        // Simulate interruption after the independent pending commit but before
        // SM source retirement. Every completion path must settle this receipt.
        assert_eq!(
            pending
                .insert_fenced_and_prune_unacked(
                    PendingRow {
                        id: PendingRowId::fresh(),
                        recipient: bare("alice@example.com"),
                        original_receipt_at: received_at,
                        payload: PendingPayload::Transient(Box::new(message)),
                        flushed_in_session: None,
                        outbound_sequence: None,
                    },
                    stream,
                    1
                )
                .await
                .expect("commit portable handoff"),
            waddle_xmpp::pending_delivery::InsertOutcome::Inserted
        );
        let stream = SmSessionId::new(stream);
        assert!(source
            .get_session(&stream)
            .await
            .expect("canonical source")
            .is_some());
        assert_eq!(
            pending
                .promotion_receipt_sources_before(i64::MAX, None, 8)
                .await
                .expect("receipt exists before completion"),
            vec![stream.clone()]
        );
        Self {
            _directory: directory,
            source,
            registry,
            pending,
            stream,
        }
    }

    async fn assert_confirmed(&self) {
        assert!(self
            .source
            .get_session(&self.stream)
            .await
            .expect("canonical confirmation")
            .is_none());
        assert!(self
            .source
            .list_unacked(&self.stream)
            .await
            .expect("canonical replay retirement")
            .is_empty());
        let rows = self
            .pending
            .list(&bare("alice@example.com"))
            .await
            .expect("pending custody retained");
        assert_eq!(
            rows.len(),
            1,
            "retry must not duplicate the accepted handoff"
        );
        let PendingPayload::Transient(message) = &rows[0].payload else {
            panic!("peer message remains transient");
        };
        assert_eq!(
            message
                .bodies
                .get(&xmpp_parsers::message::Lang::new())
                .map(String::as_str),
            Some("interrupted portable handoff")
        );
        assert!(self
            .pending
            .promotion_receipt_sources_before(i64::MAX, None, 8)
            .await
            .expect("receipts deleted by real completion")
            .is_empty());
    }
}

#[tokio::test]
async fn displaced_completion_deletes_portable_receipts() {
    let fixture = ReceiptCompletionFixture::new("receipt-displaced").await;
    let displaced = fixture
        .registry
        .store_session(detached_session_with_unacked(
            "receipt-surviving-stream",
            full("carol@example.com/web"),
            Vec::new(),
        ))
        .await
        .expect("displace source");
    assert_eq!(displaced.len(), 1);
    let pending: Arc<dyn PendingDeliveryStorage> = fixture.pending.clone();
    let connections = ConnectionRegistry::new();
    let users = test_user_registry();
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    let outcome = promote_displaced_sessions(
        displaced,
        DisplacedPromotionDeps {
            sm_registry: fixture.registry.as_ref(),
            connection_registry: &connections,
            user_registry: &users,
            pending_storage: &pending,
            blocking_storage: &blocking,
            server_domain: "example.com",
        },
    )
    .await;
    assert_eq!(
        outcome.completed_stream_ids().collect::<Vec<_>>(),
        vec![fixture.stream.as_str()]
    );
    fixture.assert_confirmed().await;
}

#[tokio::test]
async fn expiry_completion_deletes_portable_receipts() {
    let fixture = ReceiptCompletionFixture::new("receipt-expiry").await;
    let state = crate::server::routes::websocket::tests::create_test_websocket_state_with_sm_registry_and_pending_storage(fixture.registry.clone(), fixture.pending.clone()).await;
    crate::server::session_janitors::run_sm_expiry_sweep(&state).await;
    fixture.assert_confirmed().await;
}

#[tokio::test]
async fn graceful_shutdown_completion_deletes_portable_receipts() {
    let fixture = ReceiptCompletionFixture::new("receipt-shutdown").await;
    let state = crate::server::routes::websocket::tests::create_test_websocket_state_with_sm_registry_and_pending_storage(fixture.registry.clone(), fixture.pending.clone()).await;
    run_test_shutdown_drain(state).await;
    fixture.assert_confirmed().await;
}
