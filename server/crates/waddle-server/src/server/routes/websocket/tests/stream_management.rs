use super::super::{
    cleanup::{
        cleanup_connection_shutdown, cleanup_force_detach_connection_shutdown,
        cleanup_muc_presence_for_jid,
    },
    frame::{handle_xmpp_frame, settle_inbound_dispatch},
    frame_backstop::InboundDisposition,
    handlers::{self, presence::handle_muc_join},
    interpret_loop::build_interpret_deps,
    replay::drive_interpret_loop,
    state::{WsConnState, TERMINAL_RECOVERY_QUEUE_CAP},
    stream_management::is_countable_stanza,
    transport_xml::{build_stream_features_xml, element_to_xml, sasl_success_xml, stanza_to_xml},
};
use super::{
    create_test_server_owner_session, create_test_session, create_test_websocket_state,
    create_test_websocket_state_with_sm_registry,
    create_test_websocket_state_with_sm_registry_and_pending_storage,
    create_test_websocket_state_with_sm_registry_pending_and_blocking, message_frame_xml_with_id,
    register_test_connection, register_test_native_user, scram_client_final_from_challenge,
    snapshot_room, store_resumable_detached_session,
};
use crate::auth::Session;
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use jid::{BareJid, FullJid};
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, Notify, Semaphore};
use waddle_xmpp::{
    ownership::{ClaimStore, InProcessClaimStore, NodeIdentity, SharedNodeIdentity},
    protocol::{Blocklist, ConnectionPhase, InboundEvent},
    registry::{
        ConnectionEntry, DeliveryKind, ForceDetachOrigin, ForceDetachOutcome, ForceDetachRequest,
        GetUser, OutboundStanza, RegisterUserResource, UserRegistryError, WireUserClusteringClaims,
    },
    stream_management::{SmSessionRegistry, SM_NS},
    telemetry::attributes::SmEvictionPath,
    Stanza,
};
use xmpp_parsers::minidom::Element;

struct HangingEnsureClaimStore {
    inner: waddle_xmpp::ownership::InProcessClaimStore,
}

/// A transient promotion failure with a real pending-delivery backend behind
/// it. The first insertion fails; the cleanup-triggered retry can therefore
/// exercise the exact same state object after storage recovers.
struct FailFirstPendingStorage {
    inner: waddle_xmpp::pending_delivery::storage::InMemoryPendingDeliveryStorage,
    fail_next_insert: std::sync::atomic::AtomicBool,
}

impl FailFirstPendingStorage {
    fn new() -> Self {
        Self {
            inner:
                waddle_xmpp::pending_delivery::storage::InMemoryPendingDeliveryStorage::unlimited(),
            fail_next_insert: std::sync::atomic::AtomicBool::new(true),
        }
    }
}

#[async_trait::async_trait]
impl waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage for FailFirstPendingStorage {
    async fn insert(
        &self,
        row: waddle_xmpp::pending_delivery::PendingRow,
    ) -> Result<
        waddle_xmpp::pending_delivery::InsertOutcome,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        if self
            .fail_next_insert
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(
                waddle_xmpp::pending_delivery::storage::PendingStorageError::Other(
                    "simulated transient backend failure".to_string(),
                ),
            );
        }
        self.inner.insert(row).await
    }

    async fn list(
        &self,
        recipient: &BareJid,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::PendingRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner.list(recipient).await
    }

    async fn claim_for_session(
        &self,
        recipient: &BareJid,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::PendingRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner.claim_for_session(recipient, session).await
    }

    async fn claim_batch_for_session(
        &self,
        recipient: &BareJid,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
        after: Option<&waddle_xmpp::pending_delivery::PendingRowId>,
        limit: usize,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::PendingRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner
            .claim_batch_for_session(recipient, session, after, limit)
            .await
    }

    async fn delete_claimed(
        &self,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.delete_claimed(session).await
    }

    async fn delete_row(
        &self,
        id: &waddle_xmpp::pending_delivery::PendingRowId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.delete_row(id).await
    }

    async fn release_claim(
        &self,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.release_claim(session).await
    }

    async fn release_row(
        &self,
        id: &waddle_xmpp::pending_delivery::PendingRowId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.release_row(id).await
    }

    async fn record_pushed_at(
        &self,
        id: &waddle_xmpp::pending_delivery::PendingRowId,
        sequence: u32,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.record_pushed_at(id, sequence).await
    }

    async fn delete_acked_in_window(
        &self,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
        from_exclusive: u32,
        to_inclusive: u32,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner
            .delete_acked_in_window(session, from_exclusive, to_inclusive)
            .await
    }

    async fn list_orphaned_claims(
        &self,
        live_sessions: &[waddle_xmpp::pending_delivery::SmSessionId],
        claimed_before_ms: i64,
    ) -> Result<
        Vec<(
            waddle_xmpp::pending_delivery::PendingRowId,
            waddle_xmpp::pending_delivery::SmSessionId,
        )>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner
            .list_orphaned_claims(live_sessions, claimed_before_ms)
            .await
    }

    async fn count(
        &self,
        recipient: &BareJid,
    ) -> Result<u32, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.count(recipient).await
    }

    async fn delete_older_than(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.delete_older_than(cutoff).await
    }

    async fn scrub_for_tombstone(
        &self,
        target: &waddle_xmpp::tombstone::TombstoneTarget,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.scrub_for_tombstone(target).await
    }
}

struct FailDeleteSmPersistence {
    inner: waddle_xmpp::stream_management::persistence::InMemorySmPersistence,
}

impl FailDeleteSmPersistence {
    fn new() -> Self {
        Self {
            inner: waddle_xmpp::stream_management::persistence::InMemorySmPersistence::new(),
        }
    }
}

#[async_trait::async_trait]
impl waddle_xmpp::stream_management::persistence::SmPersistenceStorage for FailDeleteSmPersistence {
    async fn upsert_session(
        &self,
        session: waddle_xmpp::stream_management::persistence::PersistedSession,
    ) -> Result<(), waddle_xmpp::stream_management::persistence::SmPersistenceError> {
        self.inner.upsert_session(session).await
    }

    async fn get_session(
        &self,
        stream_id: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<
        Option<waddle_xmpp::stream_management::persistence::PersistedSession>,
        waddle_xmpp::stream_management::persistence::SmPersistenceError,
    > {
        self.inner.get_session(stream_id).await
    }

    async fn delete_session(
        &self,
        _stream_id: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<(), waddle_xmpp::stream_management::persistence::SmPersistenceError> {
        Err(
            waddle_xmpp::stream_management::persistence::SmPersistenceError::Other(
                "simulated delete failure".to_string(),
            ),
        )
    }

    async fn append_unacked(
        &self,
        stanza: waddle_xmpp::stream_management::persistence::PersistedUnackedStanza,
    ) -> Result<(), waddle_xmpp::stream_management::persistence::SmPersistenceError> {
        self.inner.append_unacked(stanza).await
    }

    async fn ack_through(
        &self,
        stream_id: &waddle_xmpp::pending_delivery::SmSessionId,
        up_to_sequence: u32,
    ) -> Result<u64, waddle_xmpp::stream_management::persistence::SmPersistenceError> {
        self.inner.ack_through(stream_id, up_to_sequence).await
    }

    async fn delete_unacked(
        &self,
        stream_id: &waddle_xmpp::pending_delivery::SmSessionId,
        sequences: &[u32],
    ) -> Result<u64, waddle_xmpp::stream_management::persistence::SmPersistenceError> {
        self.inner.delete_unacked(stream_id, sequences).await
    }

    async fn list_unacked(
        &self,
        stream_id: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<
        Vec<waddle_xmpp::stream_management::persistence::PersistedUnackedStanza>,
        waddle_xmpp::stream_management::persistence::SmPersistenceError,
    > {
        self.inner.list_unacked(stream_id).await
    }

    async fn list_expired_sessions(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<
        Vec<waddle_xmpp::stream_management::persistence::PersistedSession>,
        waddle_xmpp::stream_management::persistence::SmPersistenceError,
    > {
        self.inner.list_expired_sessions(now).await
    }

    async fn list_all_sessions(
        &self,
    ) -> Result<
        Vec<waddle_xmpp::stream_management::persistence::PersistedSession>,
        waddle_xmpp::stream_management::persistence::SmPersistenceError,
    > {
        self.inner.list_all_sessions().await
    }

    async fn store_session_atomic(
        &self,
        session: waddle_xmpp::stream_management::persistence::PersistedSession,
        unacked: Vec<waddle_xmpp::stream_management::persistence::PersistedUnackedStanza>,
    ) -> Result<(), waddle_xmpp::stream_management::persistence::SmPersistenceError> {
        self.inner.store_session_atomic(session, unacked).await
    }

    async fn store_session_atomic_with_principal(
        &self,
        principal: &waddle_xmpp::auth::AuthenticatedPrincipalRef,
        session: waddle_xmpp::stream_management::persistence::PersistedSession,
        unacked: Vec<waddle_xmpp::stream_management::persistence::PersistedUnackedStanza>,
    ) -> Result<(), waddle_xmpp::stream_management::persistence::SmPersistenceError> {
        self.inner
            .store_session_atomic_with_principal(principal, session, unacked)
            .await
    }

    async fn get_session_principal(
        &self,
        stream_id: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<
        Option<waddle_xmpp::auth::AuthenticatedPrincipalRef>,
        waddle_xmpp::stream_management::persistence::SmPersistenceError,
    > {
        self.inner.get_session_principal(stream_id).await
    }
}

/// Gate the first promotion insert so cleanup can be held mid-await while a
/// same-FullJID replacement binds and rejoins.
struct GatedFirstInsertPendingStorage {
    inner: waddle_xmpp::pending_delivery::storage::InMemoryPendingDeliveryStorage,
    gate_next_insert: std::sync::atomic::AtomicBool,
    insert_started: std::sync::atomic::AtomicBool,
    started_notify: Notify,
    release_insert: Semaphore,
}

impl GatedFirstInsertPendingStorage {
    fn new() -> Self {
        Self {
            inner:
                waddle_xmpp::pending_delivery::storage::InMemoryPendingDeliveryStorage::unlimited(),
            gate_next_insert: std::sync::atomic::AtomicBool::new(true),
            insert_started: std::sync::atomic::AtomicBool::new(false),
            started_notify: Notify::new(),
            release_insert: Semaphore::new(0),
        }
    }

    async fn wait_until_insert_blocks(&self) {
        while !self
            .insert_started
            .load(std::sync::atomic::Ordering::Acquire)
        {
            self.started_notify.notified().await;
        }
    }

    fn release_insert(&self) {
        self.release_insert.add_permits(1);
    }
}

struct RetractOnMessageIdInsertPendingStorage {
    inner: waddle_xmpp::pending_delivery::storage::InMemoryPendingDeliveryStorage,
    sm_registry: Arc<waddle_xmpp::stream_management::InMemorySmSessionRegistry>,
    target_message_id: &'static str,
    tombstone_target: waddle_xmpp::tombstone::TombstoneTarget,
    fired: std::sync::atomic::AtomicBool,
}

#[async_trait::async_trait]
impl waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage
    for RetractOnMessageIdInsertPendingStorage
{
    async fn insert(
        &self,
        row: waddle_xmpp::pending_delivery::PendingRow,
    ) -> Result<
        waddle_xmpp::pending_delivery::InsertOutcome,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        if matches!(
            &row.payload,
            waddle_xmpp::pending_delivery::PendingPayload::Transient(message)
                if message
                    .id
                    .as_ref()
                    .is_some_and(|id| id.0 == self.target_message_id)
        ) && !self.fired.swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            self.sm_registry
                .scrub_unacked_for_tombstone(&self.tombstone_target)
                .await
                .expect("racing scrub must succeed");
            self.inner
                .scrub_for_tombstone(&self.tombstone_target)
                .await
                .expect("racing pending scrub must succeed");
        }
        self.inner.insert(row).await
    }

    async fn list(
        &self,
        recipient: &BareJid,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::PendingRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner.list(recipient).await
    }

    async fn claim_for_session(
        &self,
        recipient: &BareJid,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::PendingRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner.claim_for_session(recipient, session).await
    }

    async fn claim_batch_for_session(
        &self,
        recipient: &BareJid,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
        after: Option<&waddle_xmpp::pending_delivery::PendingRowId>,
        limit: usize,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::PendingRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner
            .claim_batch_for_session(recipient, session, after, limit)
            .await
    }

    async fn delete_claimed(
        &self,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.delete_claimed(session).await
    }

    async fn delete_row(
        &self,
        id: &waddle_xmpp::pending_delivery::PendingRowId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.delete_row(id).await
    }

    async fn release_claim(
        &self,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.release_claim(session).await
    }

    async fn release_row(
        &self,
        id: &waddle_xmpp::pending_delivery::PendingRowId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.release_row(id).await
    }

    async fn record_pushed_at(
        &self,
        id: &waddle_xmpp::pending_delivery::PendingRowId,
        sequence: u32,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.record_pushed_at(id, sequence).await
    }

    async fn delete_acked_in_window(
        &self,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
        from_exclusive: u32,
        to_inclusive: u32,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner
            .delete_acked_in_window(session, from_exclusive, to_inclusive)
            .await
    }

    async fn list_orphaned_claims(
        &self,
        live_sessions: &[waddle_xmpp::pending_delivery::SmSessionId],
        claimed_before_ms: i64,
    ) -> Result<
        Vec<(
            waddle_xmpp::pending_delivery::PendingRowId,
            waddle_xmpp::pending_delivery::SmSessionId,
        )>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner
            .list_orphaned_claims(live_sessions, claimed_before_ms)
            .await
    }

    async fn count(
        &self,
        recipient: &BareJid,
    ) -> Result<u32, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.count(recipient).await
    }

    async fn delete_older_than(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.delete_older_than(cutoff).await
    }

    async fn scrub_for_tombstone(
        &self,
        target: &waddle_xmpp::tombstone::TombstoneTarget,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.scrub_for_tombstone(target).await
    }
}

fn direct_tombstone_target(
    wire_id: &str,
    author: &str,
    archive: &str,
) -> waddle_xmpp::tombstone::TombstoneTarget {
    waddle_xmpp::tombstone::TombstoneTarget::Direct {
        wire_id: wire_id.to_string(),
        author: author.parse().expect("direct tombstone author"),
        archive: archive.parse().expect("direct tombstone archive"),
    }
}

#[async_trait::async_trait]
impl waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage
    for GatedFirstInsertPendingStorage
{
    async fn insert(
        &self,
        row: waddle_xmpp::pending_delivery::PendingRow,
    ) -> Result<
        waddle_xmpp::pending_delivery::InsertOutcome,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        if self
            .gate_next_insert
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            self.insert_started
                .store(true, std::sync::atomic::Ordering::Release);
            self.started_notify.notify_waiters();
            self.release_insert
                .acquire()
                .await
                .expect("gated insert semaphore closed")
                .forget();
        }
        self.inner.insert(row).await
    }

    async fn list(
        &self,
        recipient: &BareJid,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::PendingRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner.list(recipient).await
    }

    async fn claim_for_session(
        &self,
        recipient: &BareJid,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::PendingRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner.claim_for_session(recipient, session).await
    }

    async fn claim_batch_for_session(
        &self,
        recipient: &BareJid,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
        after: Option<&waddle_xmpp::pending_delivery::PendingRowId>,
        limit: usize,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::PendingRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner
            .claim_batch_for_session(recipient, session, after, limit)
            .await
    }

    async fn delete_claimed(
        &self,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.delete_claimed(session).await
    }

    async fn delete_row(
        &self,
        id: &waddle_xmpp::pending_delivery::PendingRowId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.delete_row(id).await
    }

    async fn release_claim(
        &self,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.release_claim(session).await
    }

    async fn release_row(
        &self,
        id: &waddle_xmpp::pending_delivery::PendingRowId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.release_row(id).await
    }

    async fn record_pushed_at(
        &self,
        id: &waddle_xmpp::pending_delivery::PendingRowId,
        sequence: u32,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.record_pushed_at(id, sequence).await
    }

    async fn delete_acked_in_window(
        &self,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
        from_exclusive: u32,
        to_inclusive: u32,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner
            .delete_acked_in_window(session, from_exclusive, to_inclusive)
            .await
    }

    async fn list_orphaned_claims(
        &self,
        live_sessions: &[waddle_xmpp::pending_delivery::SmSessionId],
        claimed_before_ms: i64,
    ) -> Result<
        Vec<(
            waddle_xmpp::pending_delivery::PendingRowId,
            waddle_xmpp::pending_delivery::SmSessionId,
        )>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner
            .list_orphaned_claims(live_sessions, claimed_before_ms)
            .await
    }

    async fn count(
        &self,
        recipient: &BareJid,
    ) -> Result<u32, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.count(recipient).await
    }

    async fn delete_older_than(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.delete_older_than(cutoff).await
    }

    async fn scrub_for_tombstone(
        &self,
        target: &waddle_xmpp::tombstone::TombstoneTarget,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.scrub_for_tombstone(target).await
    }
}

fn register_sm_publish_owner(
    state: &super::super::state::WebSocketState,
    conn: &mut WsConnState,
    jid: &FullJid,
) {
    let (tx, _rx) = mpsc::channel(1);
    conn.registry_owner = Some(
        state
            .deps
            .protocol
            .connection_registry
            .register(jid.clone(), tx),
    );
}

#[async_trait::async_trait]
impl waddle_xmpp::ownership::ClaimStore for HangingEnsureClaimStore {
    async fn ensure_schema(&self) -> Result<(), waddle_xmpp::ownership::ClaimError> {
        self.inner.ensure_schema().await
    }

    async fn acquire(
        &self,
        entity: &waddle_xmpp::ownership::Entity,
        me: &waddle_xmpp::ownership::NodeIdentity,
    ) -> Result<waddle_xmpp::ownership::ClaimEpoch, waddle_xmpp::ownership::ClaimError> {
        self.inner.acquire(entity, me).await
    }

    async fn ensure_claimed(
        &self,
        _entity: &waddle_xmpp::ownership::Entity,
        _me: &waddle_xmpp::ownership::NodeIdentity,
    ) -> Result<waddle_xmpp::ownership::ClaimEpoch, waddle_xmpp::ownership::ClaimError> {
        std::future::pending().await
    }

    async fn steal_stale(
        &self,
        entity: &waddle_xmpp::ownership::Entity,
        observed: waddle_xmpp::ownership::ClaimEpoch,
        staleness: waddle_xmpp::ownership::StalePredicate,
        me: &waddle_xmpp::ownership::NodeIdentity,
    ) -> Result<waddle_xmpp::ownership::ClaimEpoch, waddle_xmpp::ownership::ClaimError> {
        self.inner
            .steal_stale(entity, observed, staleness, me)
            .await
    }

    async fn steal_for_resume(
        &self,
        entity: &waddle_xmpp::ownership::Entity,
        observed: waddle_xmpp::ownership::ClaimEpoch,
        witness: waddle_xmpp::ownership::ResumeIdentityProof,
        me: &waddle_xmpp::ownership::NodeIdentity,
    ) -> Result<waddle_xmpp::ownership::ClaimEpoch, waddle_xmpp::ownership::ClaimError> {
        self.inner
            .steal_for_resume(entity, observed, witness, me)
            .await
    }

    async fn current_claim(
        &self,
        entity: &waddle_xmpp::ownership::Entity,
    ) -> Result<Option<waddle_xmpp::ownership::ClaimSnapshot>, waddle_xmpp::ownership::ClaimError>
    {
        self.inner.current_claim(entity).await
    }

    async fn fence(
        &self,
        entity: &waddle_xmpp::ownership::Entity,
        me: &waddle_xmpp::ownership::NodeIdentity,
        mine: waddle_xmpp::ownership::ClaimEpoch,
    ) -> Result<bool, waddle_xmpp::ownership::ClaimError> {
        self.inner.fence(entity, me, mine).await
    }

    async fn release(
        &self,
        entity: &waddle_xmpp::ownership::Entity,
        me: &waddle_xmpp::ownership::NodeIdentity,
        mine: waddle_xmpp::ownership::ClaimEpoch,
    ) -> Result<(), waddle_xmpp::ownership::ClaimError> {
        self.inner.release(entity, me, mine).await
    }

    async fn release_many(
        &self,
        entities: &[waddle_xmpp::ownership::Entity],
        me: &waddle_xmpp::ownership::NodeIdentity,
    ) -> Result<(), waddle_xmpp::ownership::ClaimError> {
        self.inner.release_many(entities, me).await
    }
}

fn resume_frame_xml(stream_id: &str, handled_count: u32) -> String {
    element_to_xml(
        Element::builder("resume", SM_NS)
            .attr(minidom::rxml::xml_ncname!("previd").to_owned(), stream_id)
            .attr(
                minidom::rxml::xml_ncname!("h").to_owned(),
                handled_count.to_string(),
            )
            .build(),
    )
}

/// Seed a detached snapshot exactly as a successful authenticated detach
/// does: the snapshot and its durable principal reference are stored
/// atomically.  Legacy `store_session` fixtures intentionally remain only in
/// rejection tests that exercise a missing principal context.
async fn store_resumable_test_session(
    state: &super::super::state::WebSocketState,
    detached: waddle_xmpp::stream_management::DetachedSession,
) -> Session {
    let username = detached
        .jid
        .node()
        .expect("test detached JID has a localpart")
        .to_string();
    let session = create_test_session(state, &username).await;
    store_resumable_detached_session(state, &session, detached).await;
    session
}

// ---- XEP-0198 stream management --------------------------------

#[test]
fn timed_out_inbound_stanza_preserves_sender_responsibility() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let websocket_state = runtime.block_on(create_test_websocket_state());
    let mut state = waddle_xmpp::stream_management::StreamManagementState::new();
    state.enable("timeout-regression".to_string(), true, Some(300));
    let mut completion = crate::server::routes::interpret::SmInboundCompletionTracker::default();
    let sequence = completion.reserve(&state);

    settle_inbound_dispatch(
        &websocket_state.deps.protocol.ingress_shadow,
        InboundDisposition::Unhandled,
        true,
        Some(sequence),
        &mut completion,
        &mut state,
    );

    assert_eq!(state.get_inbound_count(), 0);
    assert!(!completion.has_pending());
    assert!(completion.has_unhandled_hole());

    // A late ordered-relay completion cannot turn the cancelled dispatch into
    // an acknowledgement: the sender must retain and replay this stanza.
    completion.complete(sequence, &mut state, |_submission| {});
    assert_eq!(state.get_inbound_count(), 0);
}

#[tokio::test]
async fn timed_out_inbound_stanza_detaches_and_resumes_before_the_hole() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let (tx, mut rx) = mpsc::channel::<OutboundStanza>(4);
    let owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), tx);

    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    conn.authenticated_session = Some(create_test_session(state.as_ref(), "alice").await);
    conn.registry_owner = Some(owner);
    conn.sm_state
        .enable("timeout-detach".to_string(), true, Some(300));

    let handled = conn.sm_inbound_completion.reserve(&conn.sm_state);
    settle_inbound_dispatch(
        &crate::ingress_shadow::IngressShadowHandle::disabled(),
        InboundDisposition::Handled,
        false,
        Some(handled),
        &mut conn.sm_inbound_completion,
        &mut conn.sm_state,
    );
    let timed_out = conn.sm_inbound_completion.reserve(&conn.sm_state);
    settle_inbound_dispatch(
        &crate::ingress_shadow::IngressShadowHandle::disabled(),
        InboundDisposition::Unhandled,
        false,
        Some(timed_out),
        &mut conn.sm_inbound_completion,
        &mut conn.sm_state,
    );

    assert_eq!(conn.sm_state.get_inbound_count(), 1);
    assert!(conn.sm_inbound_completion.has_unhandled_hole());
    assert!(
        !conn.phase.is_closing(),
        "timeout must use resumable transport termination, not a clean stream close"
    );

    let outcome = cleanup_connection_shutdown(state.as_ref(), &mut rx, &mut conn, false).await;
    assert_eq!(
        outcome,
        super::super::cleanup::ConnectionShutdownOutcome::Detached
    );
    let detached = state
        .deps
        .protocol
        .sm_session_registry
        .peek_session("timeout-detach")
        .await
        .expect("registry lookup")
        .expect("resumable snapshot");
    assert_eq!(detached.inbound_count, 1);

    let mut resumed = WsConnState::new();
    resumed.phase = ConnectionPhase::authenticated(&jid);
    let responses = handle_xmpp_frame(
        &resume_frame_xml("timeout-detach", 0),
        "example.com",
        state.as_ref(),
        &mut resumed,
    )
    .await;
    let resumed_frame = responses
        .iter()
        .map(|xml| Element::from_str(xml).expect("response xml"))
        .find(|element| element.name() == "resumed")
        .expect("resume succeeds");
    assert_eq!(
        resumed_frame.attr("h"),
        Some("1"),
        "the timed-out second stanza must remain outside the server acknowledgement"
    );
}

/// The force-detach cleanup path must not acknowledge `Detached` if its
/// synchronous unregister ask is transport-ambiguous and the ordered
/// pending-unregister record cannot be submitted.  A remote resume retry can
/// still discover the persisted snapshot, but the old node never reports an
/// untracked actor-tree cleanup as complete.
#[tokio::test]
async fn force_detach_cleanup_returns_not_persisted_when_unregister_retry_cannot_be_recorded() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "force-detach-unavailable@example.com/web"
        .parse()
        .expect("jid");
    let (tx, mut rx) = mpsc::channel::<OutboundStanza>(4);
    let owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), tx);
    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    conn.authenticated_session =
        Some(create_test_session(state.as_ref(), "force-detach-unavailable").await);
    conn.registry_owner = Some(owner);
    conn.sm_state
        .enable("force-detach-ambiguous".to_string(), true, Some(300));

    // Both the original synchronous ask and its ordered retry-record ask
    // fail, exercising the pre-handler transport-failure branch.
    state.deps.protocol.user_registry.kill();
    state.deps.protocol.user_registry.wait_for_shutdown().await;

    let outcome = cleanup_force_detach_connection_shutdown(
        state.as_ref(),
        &mut rx,
        &mut conn,
        false,
        waddle_xmpp::registry::ForceDetachOrigin::CrossNodeResume,
    )
    .await;
    assert_eq!(
        outcome,
        super::super::cleanup::ConnectionShutdownOutcome::NotPersisted
    );
    assert!(
        state
            .deps
            .protocol
            .sm_session_registry
            .peek_session("force-detach-ambiguous")
            .await
            .expect("registry lookup")
            .is_some(),
        "the persisted snapshot is retained for the remote resume retry path"
    );
}

/// Cross-node force-detach must not acknowledge `Detached` when the
/// sender-side relay cannot prove the remote owner either converged its
/// unregister or recorded a janitor retry. A local `AlreadyAbsent`
/// actor outcome on this node is not enough.
#[cfg(feature = "clustering")]
#[tokio::test]
async fn force_detach_cleanup_returns_not_persisted_when_remote_owner_unregister_proof_is_missing()
{
    use crate::clustering::route_bridge::OrderedRelayDeliveryBridge;
    use crate::clustering::{ClusteringHandles, NodeId};
    use crate::server::routes::websocket::tests::create_test_websocket_state_with_clustering;
    use tokio_util::sync::CancellationToken;
    use waddle_xmpp::stream_management::InMemorySmSessionRegistry;

    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &crate::config::ClusteringMessagingConfig::default(),
    );
    let clustering = ClusteringHandles {
        ordered_relay_delivery_bridge: Some(Arc::clone(&bridge)),
        ..Default::default()
    };
    let state = create_test_websocket_state_with_clustering(
        clustering,
        Arc::new(InMemorySmSessionRegistry::new().with_persistence(Arc::new(
            waddle_xmpp::stream_management::persistence::InMemorySmPersistence::new(),
        ))),
    )
    .await;

    let jid: FullJid = "force-detach-remote-proof@example.com/web"
        .parse()
        .expect("jid");
    let (tx, mut rx) = mpsc::channel::<OutboundStanza>(4);
    let owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), tx);
    bridge
        .test_insert_remote_socket_registration(
            jid.clone(),
            Arc::clone(&owner),
            NodeId::new("missing-owner-node".to_string()),
        )
        .await;

    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    conn.authenticated_session =
        Some(create_test_session(state.as_ref(), "force-detach-remote-proof").await);
    conn.registry_owner = Some(owner);
    // Enable resumable SM through the real frame path so the enable-time
    // registry claim exists — a directly-mutated `sm_state` never publishes
    // it, and the detach-time store then fails before the proof gate runs.
    let enable_responses = handle_xmpp_frame(
        "<enable xmlns='urn:xmpp:sm:3' resume='true'/>",
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;
    let stream_id = Element::from_str(&enable_responses[0])
        .expect("enabled xml")
        .attr("id")
        .expect("stream id")
        .to_string();
    conn.publish_pending_sm_enable(state.as_ref());

    let outcome = cleanup_force_detach_connection_shutdown(
        state.as_ref(),
        &mut rx,
        &mut conn,
        false,
        waddle_xmpp::registry::ForceDetachOrigin::CrossNodeResume,
    )
    .await;

    assert_eq!(
        outcome,
        super::super::cleanup::ConnectionShutdownOutcome::NotPersisted
    );
    assert_eq!(
        bridge.test_pending_remote_socket_unregister_count().await,
        1,
        "sender-side relay failure must retain a retryable remote unregister obligation"
    );
    assert!(
        state
            .deps
            .protocol
            .sm_session_registry
            .peek_session(&stream_id)
            .await
            .expect("registry lookup")
            .is_some(),
        "the persisted snapshot is retained until remote-owner cleanup is proved"
    );
}

/// Stale-actor retirement owns the actor-tree removal in the waiting
/// `UserRegistryActor` turn, so the connection cleanup must still report a
/// successful detach even if the registry itself is unavailable. Re-entering
/// the registry here would incorrectly degrade to `NotPersisted`.
#[tokio::test]
async fn stale_actor_force_detach_cleanup_detaches_without_registry_reentry() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "stale-force-detach@example.com/web".parse().expect("jid");
    let (tx, mut rx) = mpsc::channel::<OutboundStanza>(4);
    let owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), tx);
    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    conn.authenticated_session =
        Some(create_test_session(state.as_ref(), "stale-force-detach").await);
    conn.registry_owner = Some(owner);
    conn.sm_state
        .enable("stale-force-detach-stream".to_string(), true, Some(300));

    state.deps.protocol.user_registry.kill();
    state.deps.protocol.user_registry.wait_for_shutdown().await;

    let outcome = cleanup_force_detach_connection_shutdown(
        state.as_ref(),
        &mut rx,
        &mut conn,
        false,
        waddle_xmpp::registry::ForceDetachOrigin::RegistryStaleActorRetirement,
    )
    .await;
    assert_eq!(
        outcome,
        super::super::cleanup::ConnectionShutdownOutcome::Detached
    );
    assert!(
        state
            .deps
            .protocol
            .sm_session_registry
            .peek_session("stale-force-detach-stream")
            .await
            .expect("registry lookup")
            .is_some(),
        "the detach snapshot must persist without depending on a registry re-entry"
    );
}

/// A short child-actor critical section during a cross-node resume must be
/// retried synchronously, so a successful second ask reaches `Released`
/// instead of taking the janitor fallback.
#[tokio::test]
async fn force_detach_busy_unregister_retries_until_the_child_clears() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let attempts = Arc::new(AtomicUsize::new(0));
    let outcome = super::super::cleanup::retry_force_detach_busy_unregister({
        let attempts = Arc::clone(&attempts);
        move || {
            let attempts = Arc::clone(&attempts);
            async move {
                let outcome = if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    waddle_xmpp::registry::UnregisterAndReleaseOutcome::RetryableFailure(
                        waddle_xmpp::registry::user_registry::UnregisterAndReleaseRetryableFailure::UserActorBusy,
                    )
                } else {
                    waddle_xmpp::registry::UnregisterAndReleaseOutcome::Released
                };
                Ok::<_, ()>(outcome)
            }
        }
    })
    .await;

    assert_eq!(
        outcome,
        Ok(waddle_xmpp::registry::UnregisterAndReleaseOutcome::Released)
    );
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn force_detach_busy_unregister_stops_after_three_attempts() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let attempts = Arc::new(AtomicUsize::new(0));
    let outcome = super::super::cleanup::retry_force_detach_busy_unregister({
        let attempts = Arc::clone(&attempts);
        move || {
            let attempts = Arc::clone(&attempts);
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                Ok::<_, ()>(
                    waddle_xmpp::registry::UnregisterAndReleaseOutcome::RetryableFailure(
                        waddle_xmpp::registry::user_registry::UnregisterAndReleaseRetryableFailure::UserActorBusy,
                    ),
                )
            }
        }
    })
    .await;

    assert!(matches!(
        outcome,
        Ok(waddle_xmpp::registry::UnregisterAndReleaseOutcome::RetryableFailure(
            waddle_xmpp::registry::user_registry::UnregisterAndReleaseRetryableFailure::UserActorBusy
        ))
    ));
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
}

/// A real force-detach can encounter a full old-node UserActor mailbox while
/// a short operation is in flight.  Releasing that operation during the
/// bounded retry window must prune the actor synchronously, without leaving a
/// janitor pending-unregister record behind.
#[tokio::test]
async fn force_detach_busy_child_retries_and_converges_without_janitor() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "force-detach-busy@example.com/web".parse().expect("jid");
    let bare_jid = jid.to_bare();
    let (tx, mut rx) = mpsc::channel::<OutboundStanza>(4);
    let owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), tx);
    let entry = state
        .deps
        .protocol
        .connection_registry
        .get_entry(&jid)
        .expect("registered entry");
    state
        .deps
        .protocol
        .user_registry
        .ask(RegisterUserResource {
            jid: jid.clone(),
            entry,
        })
        .await
        .expect("mirror into UserActor");

    let actor = state
        .deps
        .protocol
        .user_registry
        .ask(GetUser {
            bare_jid: bare_jid.clone(),
        })
        .await
        .expect("get UserActor")
        .expect("UserActor exists");
    let entered = Arc::new(Notify::new());
    let (release_tx, release_rx) = oneshot::channel();
    actor
        .tell(
            waddle_xmpp::registry::user_actor::test_support::GateMailbox {
                entered: Arc::clone(&entered),
                release_rx,
            },
        )
        .await
        .expect("queue mailbox gate");
    entered.notified().await;

    let mut saw_mailbox_full = false;
    for _ in 0..128 {
        let sent = actor
            .tell(waddle_xmpp::registry::user_actor::test_support::MailboxNoop)
            .try_send();
        if matches!(sent, Err(kameo::error::SendError::MailboxFull(_))) {
            saw_mailbox_full = true;
            break;
        }
        sent.expect("mailbox filler should enqueue or report full");
    }
    assert!(saw_mailbox_full, "child mailbox must be busy before detach");

    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    conn.authenticated_session =
        Some(create_test_session(state.as_ref(), "force-detach-busy").await);
    conn.registry_owner = Some(owner);
    conn.sm_state
        .enable("force-detach-busy".to_string(), true, Some(300));

    let cleanup_state = Arc::clone(&state);
    let cleanup = tokio::spawn(async move {
        cleanup_force_detach_connection_shutdown(
            cleanup_state.as_ref(),
            &mut rx,
            &mut conn,
            false,
            waddle_xmpp::registry::ForceDetachOrigin::CrossNodeResume,
        )
        .await
    });
    // The first child ask observes the full mailbox immediately.  Release
    // before its 50ms retry backoff expires so the second bounded ask drains
    // the queued no-ops and removes the exact owner.
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    release_tx.send(()).expect("release mailbox gate");

    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(2), cleanup)
            .await
            .expect("bounded retry must not hang")
            .expect("cleanup task joins"),
        super::super::cleanup::ConnectionShutdownOutcome::Detached
    );
    assert_eq!(
        state
            .deps
            .protocol
            .user_registry
            .ask(waddle_xmpp::registry::user_registry::test_support::PendingUnregisterCount,)
            .await
            .expect("pending unregister count"),
        0,
        "a successful bounded retry must not leave janitor work behind"
    );
    assert!(
        state
            .deps
            .protocol
            .user_registry
            .ask(GetUser { bare_jid })
            .await
            .expect("get UserActor after detach")
            .is_none(),
        "successful force-detach prunes the now-empty UserActor"
    );
}

/// When a cross-node force-detach is already shutting this connection down,
/// a stale-actor retirement request can queue behind it and block the
/// `UserRegistryActor` turn waiting on that second acknowledgement. The
/// connection must therefore release the stale-retirement waiter before its
/// cross-node cleanup re-enters the registry, or the synchronous unregister
/// ask times out behind the waiting actor turn.
#[tokio::test]
async fn queued_stale_force_detach_waiter_is_released_before_cross_node_cleanup() {
    let state = create_test_websocket_state().await;
    let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
    let shared_identity = SharedNodeIdentity::new(NodeIdentity::new("node-this", "epoch-old"));
    state
        .deps
        .protocol
        .user_registry
        .ask(WireUserClusteringClaims {
            claim_store,
            node_identity: shared_identity.clone(),
        })
        .await
        .expect("wire rotating identity");

    let old_jid: FullJid = "queued-force-detach@example.com/old"
        .parse()
        .expect("old jid");
    let new_jid: FullJid = "queued-force-detach@example.com/new"
        .parse()
        .expect("new jid");
    let bare_jid = old_jid.to_bare();

    let (old_tx, mut old_rx) = mpsc::channel::<OutboundStanza>(4);
    let owner = state
        .deps
        .protocol
        .connection_registry
        .register(old_jid.clone(), old_tx);
    let old_entry = state
        .deps
        .protocol
        .connection_registry
        .get_entry(&old_jid)
        .expect("registered entry");
    state
        .deps
        .protocol
        .user_registry
        .ask(RegisterUserResource {
            jid: old_jid.clone(),
            entry: old_entry.clone(),
        })
        .await
        .expect("mirror old resource");
    let mut force_detach_rx = Some(
        old_entry
            .take_force_detach_rx()
            .expect("connection task owns receiver"),
    );

    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready(old_jid.clone(), false);
    conn.authenticated_session =
        Some(create_test_session(state.as_ref(), "queued-force-detach").await);
    conn.registry_owner = Some(owner);
    conn.sm_state
        .enable("queued-force-detach-stream".to_string(), true, Some(300));

    let (crossnode_ack_tx, crossnode_ack_rx) = oneshot::channel();
    old_entry
        .force_detach_sender()
        .try_send(ForceDetachRequest {
            origin: ForceDetachOrigin::CrossNodeResume,
            requester_bare_jid: bare_jid.clone(),
            ack: crossnode_ack_tx,
        })
        .expect("queue cross-node detach");

    shared_identity
        .rotate(NodeIdentity::new("node-this", "epoch-new"))
        .await;
    let state_for_register = Arc::clone(&state);
    let register_task = tokio::spawn(async move {
        let (new_tx, _new_rx) = mpsc::channel::<OutboundStanza>(4);
        state_for_register
            .deps
            .protocol
            .user_registry
            .ask(RegisterUserResource {
                jid: new_jid,
                entry: ConnectionEntry::new(new_tx),
            })
            .await
    });

    let primary = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        force_detach_rx.as_mut().expect("receiver").recv(),
    )
    .await
    .expect("cross-node detach received")
    .expect("primary request");
    assert_eq!(primary.origin, ForceDetachOrigin::CrossNodeResume);

    let drained = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let drained = super::super::connection::drain_ready_force_detach_requests(
                &mut force_detach_rx,
                &bare_jid,
            );
            if !drained.is_empty() {
                break drained;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("stale detach queued behind cross-node");
    assert_eq!(drained.len(), 1, "exactly one stale waiter should queue");
    assert_eq!(
        drained[0].origin,
        ForceDetachOrigin::RegistryStaleActorRetirement
    );

    let mut pending_force_detach = vec![primary];
    pending_force_detach.extend(drained);
    super::super::connection::release_stale_force_detach_waiters_before_cross_node_cleanup(
        &mut pending_force_detach,
        Some(ForceDetachOrigin::CrossNodeResume),
    );

    let register_outcome = tokio::time::timeout(std::time::Duration::from_secs(1), register_task)
        .await
        .expect("stale-retirement waiter must be released")
        .expect("register task joins");
    assert!(
        matches!(
            register_outcome,
            Ok(())
                | Err(kameo::error::SendError::HandlerError(
                    UserRegistryError::ClaimHeldByAnotherNode(_)
                        | UserRegistryError::UserActorBusy(_)
                ))
        ),
        "stale-retirement release should avoid a terminal retirement failure: {register_outcome:?}"
    );

    let cleanup_outcome = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        cleanup_force_detach_connection_shutdown(
            state.as_ref(),
            &mut old_rx,
            &mut conn,
            false,
            ForceDetachOrigin::CrossNodeResume,
        ),
    )
    .await
    .expect("cross-node cleanup must not time out behind stale waiter");
    assert_eq!(
        cleanup_outcome,
        super::super::cleanup::ConnectionShutdownOutcome::Detached
    );

    let crossnode_outcome = match cleanup_outcome {
        super::super::cleanup::ConnectionShutdownOutcome::Detached => ForceDetachOutcome::Detached,
        super::super::cleanup::ConnectionShutdownOutcome::NotPersisted => {
            ForceDetachOutcome::NotPersisted
        }
    };
    for request in pending_force_detach {
        let _ = request.ack.send(crossnode_outcome);
    }

    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(1), crossnode_ack_rx)
            .await
            .expect("cross-node waiter completes")
            .expect("cross-node ack"),
        ForceDetachOutcome::Detached
    );
}

/// Codex 1668 round: a stale-retirement request drained AHEAD of a
/// cross-node-resume one must not demote cleanup to stale-retirement
/// semantics — cross-node is authoritative for the whole set, and the
/// stale waiter is released early so its registry turn can finish.
#[tokio::test]
async fn cross_node_origin_is_authoritative_when_stale_retirement_queues_first() {
    let bare_jid = BareJid::from_str("reversed-order@example.com").expect("valid bare jid");
    let (stale_ack_tx, mut stale_ack_rx) = oneshot::channel();
    let (crossnode_ack_tx, mut crossnode_ack_rx) = oneshot::channel();
    let mut pending_force_detach = vec![
        ForceDetachRequest {
            origin: ForceDetachOrigin::RegistryStaleActorRetirement,
            requester_bare_jid: bare_jid.clone(),
            ack: stale_ack_tx,
        },
        ForceDetachRequest {
            origin: ForceDetachOrigin::CrossNodeResume,
            requester_bare_jid: bare_jid,
            ack: crossnode_ack_tx,
        },
    ];

    let origin = super::super::connection::authoritative_force_detach_origin(&pending_force_detach);
    assert_eq!(
        origin,
        Some(ForceDetachOrigin::CrossNodeResume),
        "any queued cross-node request must make cross-node semantics authoritative"
    );

    super::super::connection::release_stale_force_detach_waiters_before_cross_node_cleanup(
        &mut pending_force_detach,
        origin,
    );
    assert_eq!(
        pending_force_detach.len(),
        1,
        "the stale-retirement waiter at index 0 must be released early"
    );
    assert_eq!(
        pending_force_detach[0].origin,
        ForceDetachOrigin::CrossNodeResume
    );
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_millis(250), &mut stale_ack_rx)
            .await
            .expect("stale waiter must be acknowledged before cross-node cleanup")
            .expect("stale ack"),
        ForceDetachOutcome::NotPersisted
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(25), &mut crossnode_ack_rx)
            .await
            .is_err(),
        "the cross-node waiter must stay queued for the real cleanup outcome"
    );
}

#[tokio::test]
async fn owner_managed_force_detach_waiter_stays_queued_during_cross_node_cleanup() {
    let bare_jid = BareJid::from_str("owner-managed@example.com").expect("valid bare jid");
    let (crossnode_ack_tx, mut crossnode_ack_rx) = oneshot::channel();
    let (owner_managed_ack_tx, mut owner_managed_ack_rx) = oneshot::channel();
    let mut pending_force_detach = vec![
        ForceDetachRequest {
            origin: ForceDetachOrigin::CrossNodeResume,
            requester_bare_jid: bare_jid.clone(),
            ack: crossnode_ack_tx,
        },
        ForceDetachRequest {
            origin: ForceDetachOrigin::OwnerManagedRetirement,
            requester_bare_jid: bare_jid,
            ack: owner_managed_ack_tx,
        },
    ];

    super::super::connection::release_stale_force_detach_waiters_before_cross_node_cleanup(
        &mut pending_force_detach,
        Some(ForceDetachOrigin::CrossNodeResume),
    );

    assert_eq!(pending_force_detach.len(), 2);
    assert_eq!(
        pending_force_detach[1].origin,
        ForceDetachOrigin::OwnerManagedRetirement
    );
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(25),
            &mut owner_managed_ack_rx
        )
        .await
        .is_err(),
        "owner-managed cleanup must stay queued until its owning lifecycle finishes"
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(25), &mut crossnode_ack_rx)
            .await
            .is_err(),
        "the primary cross-node request must remain queued too"
    );
}

/// When a send-window pause exhausts its reserved inbound headroom, the
/// recorded prefix is promoted through the established XEP-0198 recovery
/// chain instead of becoming a resumable snapshot with an unrecorded tail.
#[tokio::test]
async fn deferred_cap_exhaustion_promotes_recorded_prefix_and_rejects_resume() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/cap-exhaustion".parse().expect("jid");
    let (tx, mut rx) = mpsc::channel::<OutboundStanza>(4);
    let owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), tx.clone());

    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    conn.authenticated_session = Some(create_test_session(state.as_ref(), "alice").await);
    conn.registry_owner = Some(owner);
    let enable_responses = handle_xmpp_frame(
        "<enable xmlns='urn:xmpp:sm:3' resume='true'/>",
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;
    let stream_id = Element::from_str(&enable_responses[0])
        .expect("enabled xml")
        .attr("id")
        .expect("stream id")
        .to_string();
    conn.publish_pending_sm_enable(state.as_ref());

    // This row has already been written to the live wire and bound to the
    // SM sequence. Its detached XML has no row id, so terminal cleanup must
    // recover the durable binding by `(stream_id, outbound_sequence)` rather
    // than promote a second Archived row from the replay copy.
    let pending_row_id = waddle_xmpp::pending_delivery::PendingRowId::fresh();
    let original_receipt_at = chrono::Utc::now();
    let archived_id = waddle_xmpp_core::xep0359::StanzaId::new(
        "terminal-row-backed-archive",
        jid::Jid::from(jid.to_bare()),
    );
    let mut row_backed = xmpp_parsers::message::Message::new(Some(jid::Jid::from(jid.to_bare())));
    row_backed.from = Some("bob@example.com/phone".parse().expect("sender jid"));
    row_backed.id = Some(xmpp_parsers::message::Id("terminal-row-backed".to_string()));
    row_backed.type_ = xmpp_parsers::message::MessageType::Chat;
    row_backed.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "row-backed live unacked".to_string(),
    );
    waddle_xmpp_core::xep0359::add_stanza_id(&mut row_backed, &archived_id);
    state
        .deps
        .protocol
        .pending_delivery_storage
        .insert(waddle_xmpp::pending_delivery::PendingRow {
            id: pending_row_id.clone(),
            recipient: jid.to_bare(),
            original_receipt_at,
            payload: waddle_xmpp::pending_delivery::PendingPayload::Archived(archived_id.clone()),
            flushed_in_session: Some(waddle_xmpp::pending_delivery::SmSessionId::new(
                stream_id.clone(),
            )),
            outbound_sequence: Some(1),
        })
        .await
        .expect("seed written claimed archived pending row");
    let _ = conn.sm_state.record_outbound(
        waddle_xmpp::parser::stanza_to_string(row_backed).expect("serialize written flush stanza"),
        SmEvictionPath::Batch,
    );
    for sequence in 2..=8 {
        let _ = conn.sm_state.record_outbound(
            message_frame_xml_with_id(format!("recorded-{sequence}")),
            SmEvictionPath::Batch,
        );
    }
    conn.begin_terminal_sm_recovery();

    for sequence in 0..TERMINAL_RECOVERY_QUEUE_CAP {
        let xml = if sequence == 0 {
            let mut prefix =
                xmpp_parsers::message::Message::new(Some(jid::Jid::from(jid.to_bare())));
            prefix.from = Some("bob@example.com/phone".parse().expect("sender jid"));
            prefix.id = Some(xmpp_parsers::message::Id("terminal-prefix-0".to_string()));
            prefix.type_ = xmpp_parsers::message::MessageType::Chat;
            prefix.bodies.insert(
                xmpp_parsers::message::Lang::new(),
                "older retained terminal prefix".to_string(),
            );
            waddle_xmpp::parser::stanza_to_string(prefix).expect("serialize terminal prefix")
        } else {
            message_frame_xml_with_id(format!("terminal-prefix-{sequence}"))
        };
        conn.record_terminal_recovery_outbound(xml);
    }
    assert_eq!(
        conn.terminal_sm_recovery.queue_len(),
        TERMINAL_RECOVERY_QUEUE_CAP,
        "the terminal replay buffer starts physically full"
    );

    // An accepted exact-FullJID <no-store/> stanza must bypass the full
    // in-memory terminal queue and promote directly to pending delivery.
    let mut buffered = xmpp_parsers::message::Message::new(Some(jid::Jid::from(jid.clone())));
    buffered.from = Some("bob@example.com/phone".parse().expect("sender jid"));
    buffered.id = Some(xmpp_parsers::message::Id("accepted-backlog".to_string()));
    buffered.type_ = xmpp_parsers::message::MessageType::Chat;
    buffered.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "accepted backlog".to_string(),
    );
    waddle_xmpp::xep::xep0334::add_hint(
        &mut buffered,
        waddle_xmpp::xep::xep0334::Hint::NoPermanentStore,
    );
    tx.send(OutboundStanza::new(Stanza::Message(buffered)))
        .await
        .expect("queue accepted outbound stanza");

    assert_eq!(
        cleanup_connection_shutdown(state.as_ref(), &mut rx, &mut conn, false).await,
        super::super::cleanup::ConnectionShutdownOutcome::NotPersisted
    );
    assert!(
        state
            .deps
            .protocol
            .sm_session_registry
            .peek_session(&stream_id)
            .await
            .expect("registry lookup")
            .is_none(),
        "the cap-exhausted stream must never persist a resumable snapshot"
    );

    let mut resumed = WsConnState::new();
    resumed.phase = ConnectionPhase::authenticated(&jid);
    let resume_responses = handle_xmpp_frame(
        &resume_frame_xml(&stream_id, 0),
        "example.com",
        state.as_ref(),
        &mut resumed,
    )
    .await;
    assert!(
        resume_responses.iter().any(|xml| xml.contains("<failed")),
        "a later resume is deliberately rejected after recovery promotion"
    );
    let pending = state
        .deps
        .protocol
        .pending_delivery_storage
        .list(&jid.to_bare())
        .await
        .expect("list promoted pending rows");
    assert!(
        pending.iter().any(|row| {
            matches!(
                &row.payload,
                waddle_xmpp::pending_delivery::PendingPayload::Transient(message)
                    if message.id.as_ref().is_some_and(|id| id.0 == "accepted-backlog")
            )
        }),
        "accepted outbound work beyond the terminal cap is promoted instead of silently dropped"
    );
    let first_terminal_prefix = pending
        .iter()
        .position(|row| {
            matches!(
                &row.payload,
                waddle_xmpp::pending_delivery::PendingPayload::Transient(message)
                    if message.id.as_ref().is_some_and(|id| id.0 == "terminal-prefix-0")
            )
        })
        .expect("the retained terminal prefix is promoted");
    let accepted_backlog = pending
        .iter()
        .position(|row| {
            matches!(
                &row.payload,
                waddle_xmpp::pending_delivery::PendingPayload::Transient(message)
                    if message.id.as_ref().is_some_and(|id| id.0 == "accepted-backlog")
            )
        })
        .expect("the accepted overflow is promoted");
    assert!(
        first_terminal_prefix < accepted_backlog,
        "incremental overflow promotion must not overtake the retained terminal prefix"
    );
    let row_backed_rows: Vec<_> = pending
        .iter()
        .filter(|row| row.id == pending_row_id)
        .collect();
    assert_eq!(
        row_backed_rows.len(),
        1,
        "terminal promotion must not insert a duplicate written flush stanza"
    );
    assert!(
        matches!(
            &row_backed_rows[0].payload,
            waddle_xmpp::pending_delivery::PendingPayload::Archived(id) if id == &archived_id
        ) && row_backed_rows[0].flushed_in_session.is_none()
            && row_backed_rows[0].outbound_sequence.is_none(),
        "the original archived row is released for normal pending-delivery redelivery"
    );
}

#[tokio::test]
async fn refused_detach_keeps_claim_fence_until_promotion_retry_settles() {
    // Exercise the cleanup caller, not just promotion in isolation: a
    // missing durable principal makes cleanup terminally promote the detached
    // stream. A transient storage failure must reinsert that stream without
    // deferring its claim into the janitor's terminal-release inventory.
    let claim_store: Arc<dyn waddle_xmpp::ownership::ClaimStore> =
        Arc::new(waddle_xmpp::ownership::InProcessClaimStore::new());
    let sm_registry = Arc::new(
        waddle_xmpp::stream_management::InMemorySmSessionRegistry::new().with_claim_store(
            Arc::clone(&claim_store),
            waddle_xmpp::ownership::SharedNodeIdentity::new(
                waddle_xmpp::ownership::NodeIdentity::new("cleanup-retry-node", "incarnation"),
            ),
        ),
    );
    let pending_impl = Arc::new(FailFirstPendingStorage::new());
    let pending: Arc<dyn waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage> =
        Arc::clone(&pending_impl) as Arc<_>;
    let state = create_test_websocket_state_with_sm_registry_and_pending_storage(
        Arc::clone(&sm_registry),
        pending,
    )
    .await;
    let jid: FullJid = "alice@example.com/cleanup-retry".parse().expect("jid");
    let stream_id = "stream-cleanup-retry";
    assert!(sm_registry.ensure_session_claim(stream_id).await.is_some());

    let (tx, mut rx) = mpsc::channel::<OutboundStanza>(1);
    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    conn.registry_owner = Some(
        state
            .deps
            .protocol
            .connection_registry
            .register(jid.clone(), tx),
    );
    conn.sm_state.enable(stream_id.to_string(), true, Some(300));
    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(jid.to_bare())));
    message.from = Some("bob@example.com/phone".parse().expect("sender jid"));
    message.type_ = xmpp_parsers::message::MessageType::Chat;
    message
        .bodies
        .insert(xmpp_parsers::message::Lang::new(), "retry me".to_string());
    let _ = conn.sm_state.record_outbound(
        waddle_xmpp::parser::stanza_to_string(message).expect("serialize queued message"),
        SmEvictionPath::DirectOutbound,
    );

    assert_eq!(
        cleanup_connection_shutdown(state.as_ref(), &mut rx, &mut conn, false).await,
        super::super::cleanup::ConnectionShutdownOutcome::NotPersisted
    );
    assert_eq!(
        sm_registry.pending_claim_release_count(),
        0,
        "cleanup must not defer a terminal claim while its promotion retry is live"
    );
    assert!(
        sm_registry
            .locally_owned_claim_ids()
            .expect("owned claim inventory")
            .iter()
            .any(|owned| owned == stream_id),
        "the retry retains the claim fence needed by fenced pending insertion"
    );

    let retry = sm_registry.drain_expired().await.expect("drain retry");
    assert_eq!(retry.len(), 1, "failed cleanup promotion is retryable");
    let settled = crate::sm_promotion::promote_displaced_sessions(
        retry,
        crate::sm_promotion::DisplacedPromotionDeps {
            sm_registry: sm_registry.as_ref(),
            connection_registry: &state.deps.protocol.connection_registry,
            user_registry: &state.deps.protocol.user_registry,
            pending_storage: &state.deps.protocol.pending_delivery_storage,
            blocking_storage: state.deps.protocol.blocking_storage.as_ref(),
            server_domain: "example.com",
        },
    )
    .await;
    assert!(!settled.is_retrying(stream_id));
    assert_eq!(
        sm_registry.pending_claim_release_count(),
        0,
        "the retry settles its own claim rather than adding terminal-release work"
    );
    assert!(
        !sm_registry
            .locally_owned_claim_ids()
            .expect("owned claim inventory")
            .iter()
            .any(|owned| owned == stream_id),
        "the claim fence is released after the retry succeeds"
    );
    assert!(
        claim_store
            .current_claim(&waddle_xmpp::ownership::Entity::new(
                waddle_xmpp::ownership::EntityType::SmSession,
                stream_id,
            ))
            .await
            .expect("claim lookup")
            .is_none(),
        "the settled retry releases the exact stream fence"
    );
}

#[tokio::test]
async fn terminal_incremental_overflow_rechecks_tombstones_per_item() {
    let sm_registry = Arc::new(waddle_xmpp::stream_management::InMemorySmSessionRegistry::new());
    let pending_storage = Arc::new(RetractOnMessageIdInsertPendingStorage {
        inner: waddle_xmpp::pending_delivery::storage::InMemoryPendingDeliveryStorage::unlimited(),
        sm_registry: Arc::clone(&sm_registry),
        target_message_id: "accepted-backlog",
        tombstone_target: direct_tombstone_target(
            "accepted-backlog",
            "bob@example.com",
            "alice@example.com",
        ),
        fired: std::sync::atomic::AtomicBool::new(false),
    });
    let state = create_test_websocket_state_with_sm_registry_and_pending_storage(
        Arc::clone(&sm_registry),
        pending_storage.clone(),
    )
    .await;
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");

    let (tx, mut rx) = mpsc::channel::<OutboundStanza>(4);
    let owner = super::register_test_connection(state.as_ref(), &jid, tx.clone()).await;
    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    conn.registry_owner = Some(owner);
    conn.sm_state
        .enable("terminal-tombstone-race".to_string(), true, Some(300));
    for sequence in 1..=8 {
        let _ = conn.sm_state.record_outbound(
            message_frame_xml_with_id(format!("recorded-{sequence}")),
            SmEvictionPath::Batch,
        );
    }
    conn.begin_terminal_sm_recovery();

    for sequence in 0..TERMINAL_RECOVERY_QUEUE_CAP {
        let xml = if sequence == 0 {
            let mut prefix =
                xmpp_parsers::message::Message::new(Some(jid::Jid::from(jid.to_bare())));
            prefix.from = Some("bob@example.com/phone".parse().expect("sender jid"));
            prefix.id = Some(xmpp_parsers::message::Id("terminal-prefix-0".to_string()));
            prefix.type_ = xmpp_parsers::message::MessageType::Chat;
            prefix.bodies.insert(
                xmpp_parsers::message::Lang::new(),
                "older retained terminal prefix".to_string(),
            );
            waddle_xmpp::parser::stanza_to_string(prefix).expect("serialize terminal prefix")
        } else {
            message_frame_xml_with_id(format!("terminal-prefix-{sequence}"))
        };
        conn.record_terminal_recovery_outbound(xml);
    }

    let mut buffered = xmpp_parsers::message::Message::new(Some(jid::Jid::from(jid.clone())));
    buffered.from = Some("bob@example.com/phone".parse().expect("sender jid"));
    buffered.id = Some(xmpp_parsers::message::Id("accepted-backlog".to_string()));
    buffered.type_ = xmpp_parsers::message::MessageType::Chat;
    buffered.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "accepted backlog".to_string(),
    );
    waddle_xmpp::xep::xep0334::add_hint(
        &mut buffered,
        waddle_xmpp::xep::xep0334::Hint::NoPermanentStore,
    );
    tx.send(OutboundStanza::new(Stanza::Message(buffered)))
        .await
        .expect("queue accepted outbound stanza");

    assert_eq!(
        cleanup_connection_shutdown(state.as_ref(), &mut rx, &mut conn, false).await,
        super::super::cleanup::ConnectionShutdownOutcome::NotPersisted
    );

    let pending = state
        .deps
        .protocol
        .pending_delivery_storage
        .list(&jid.to_bare())
        .await
        .expect("list promoted pending rows");
    assert!(
        pending.iter().any(|row| {
            matches!(
                &row.payload,
                waddle_xmpp::pending_delivery::PendingPayload::Transient(message)
                    if message.id.as_ref().is_some_and(|id| id.0 == "terminal-prefix-0")
            )
        }),
        "the retained terminal prefix still promotes"
    );
    assert!(
        !pending.iter().any(|row| {
            matches!(
                &row.payload,
                waddle_xmpp::pending_delivery::PendingPayload::Transient(message)
                    if message.id.as_ref().is_some_and(|id| id.0 == "accepted-backlog")
            )
        }),
        "a tombstone recorded during incremental overflow promotion must scrub the overflow row"
    );
}

#[tokio::test]
async fn refused_detach_confirm_failure_defers_claim_when_retry_is_untracked() {
    let claim_store: Arc<dyn waddle_xmpp::ownership::ClaimStore> =
        Arc::new(waddle_xmpp::ownership::InProcessClaimStore::new());
    let persistence = Arc::new(FailDeleteSmPersistence::new());
    let sm_registry = Arc::new(
        waddle_xmpp::stream_management::InMemorySmSessionRegistry::new()
            .with_persistence(persistence)
            .with_claim_store(
                Arc::clone(&claim_store),
                waddle_xmpp::ownership::SharedNodeIdentity::new(
                    waddle_xmpp::ownership::NodeIdentity::new(
                        "cleanup-confirm-fail-node",
                        "incarnation",
                    ),
                ),
            ),
    );
    let pending: Arc<dyn waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage> = Arc::new(
        waddle_xmpp::pending_delivery::storage::InMemoryPendingDeliveryStorage::unlimited(),
    );
    let state = create_test_websocket_state_with_sm_registry_and_pending_storage(
        Arc::clone(&sm_registry),
        pending,
    )
    .await;
    let jid: FullJid = "alice@example.com/cleanup-confirm-fail"
        .parse()
        .expect("jid");
    let stream_id = "stream-cleanup-confirm-fail";
    assert!(sm_registry.ensure_session_claim(stream_id).await.is_some());

    let (tx, mut rx) = mpsc::channel::<OutboundStanza>(1);
    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    conn.registry_owner = Some(
        state
            .deps
            .protocol
            .connection_registry
            .register(jid.clone(), tx),
    );
    conn.sm_state.enable(stream_id.to_string(), true, Some(300));
    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(jid.to_bare())));
    message.from = Some("bob@example.com/phone".parse().expect("sender jid"));
    message.type_ = xmpp_parsers::message::MessageType::Chat;
    message.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "confirm should fail".to_string(),
    );
    let _ = conn.sm_state.record_outbound(
        waddle_xmpp::parser::stanza_to_string(message).expect("serialize queued message"),
        SmEvictionPath::DirectOutbound,
    );

    assert_eq!(
        cleanup_connection_shutdown(state.as_ref(), &mut rx, &mut conn, false).await,
        super::super::cleanup::ConnectionShutdownOutcome::NotPersisted
    );
    assert_eq!(
        sm_registry.pending_claim_release_count(),
        1,
        "an untracked synthetic session must fall back to terminal claim defer"
    );
    assert!(
        sm_registry
            .drain_expired()
            .await
            .expect("drain retry")
            .is_empty(),
        "cleanup must not report a retry unless the registry actually retained one"
    );
    assert!(
        sm_registry
            .locally_owned_claim_ids()
            .expect("owned claim inventory")
            .iter()
            .any(|owned| owned == stream_id),
        "the deferred terminal-release inventory must still hold the exact claim"
    );
}

/// If a same-FullJID replacement wins before cleanup rechecks ownership, the
/// stale non-superseded path must still promote the predecessor's recorded
/// prefix and accepted backlog without touching the replacement entry.
#[tokio::test]
async fn ownership_moved_before_terminal_cleanup_still_promotes_without_touching_replacement() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/superseded-recovery"
        .parse()
        .expect("jid");
    let (old_tx, mut old_rx) = mpsc::channel::<OutboundStanza>(4);
    let old_owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), old_tx.clone());

    let mut old_conn = WsConnState::new();
    old_conn.phase = ConnectionPhase::ready(jid.clone(), false);
    old_conn.authenticated_session = Some(create_test_session(state.as_ref(), "alice").await);
    old_conn.registry_owner = Some(old_owner);
    let enabled = handle_xmpp_frame(
        "<enable xmlns='urn:xmpp:sm:3' resume='true'/>",
        "example.com",
        state.as_ref(),
        &mut old_conn,
    )
    .await;
    let old_stream_id = Element::from_str(&enabled[0])
        .expect("enabled xml")
        .attr("id")
        .expect("stream id")
        .to_string();
    old_conn.publish_pending_sm_enable(state.as_ref());
    let mut prefix = xmpp_parsers::message::Message::new(Some(jid::Jid::from(jid.clone())));
    prefix.id = Some(xmpp_parsers::message::Id("superseded-prefix".to_string()));
    let _ = old_conn.sm_state.record_outbound(
        waddle_xmpp::parser::stanza_to_string(prefix).expect("serialize prefix"),
        SmEvictionPath::Batch,
    );
    old_conn.begin_terminal_sm_recovery();

    let (replacement_tx, _replacement_rx) = mpsc::channel(1);
    let replacement_owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), replacement_tx);
    let replacement_stream =
        waddle_xmpp::pending_delivery::SmSessionId::new("replacement-stream".to_string());
    assert!(state
        .deps
        .protocol
        .connection_registry
        .set_sm_stream_id_if_owner(&jid, &replacement_owner, Some(replacement_stream.clone())));

    let mut accepted = xmpp_parsers::message::Message::new(Some(jid::Jid::from(jid.to_bare())));
    accepted.from = Some("bob@example.com/phone".parse().expect("sender jid"));
    accepted.id = Some(xmpp_parsers::message::Id("superseded-backlog".to_string()));
    accepted.type_ = xmpp_parsers::message::MessageType::Chat;
    accepted.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "accepted before replacement cleanup".to_string(),
    );
    waddle_xmpp::xep::xep0334::add_hint(
        &mut accepted,
        waddle_xmpp::xep::xep0334::Hint::NoPermanentStore,
    );
    old_tx
        .send(OutboundStanza::new(Stanza::Message(accepted)))
        .await
        .expect("old receiver accepted backlog");

    assert_eq!(
        cleanup_connection_shutdown(state.as_ref(), &mut old_rx, &mut old_conn, false).await,
        super::super::cleanup::ConnectionShutdownOutcome::NotPersisted
    );
    assert!(state
        .deps
        .protocol
        .sm_session_registry
        .peek_session(&old_stream_id)
        .await
        .expect("registry lookup")
        .is_none());
    assert_eq!(
        state
            .deps
            .protocol
            .connection_registry
            .get_entry(&jid)
            .and_then(|entry| entry.sm_stream_id()),
        Some(replacement_stream),
        "terminal recovery must not mutate the replacement registry entry"
    );
    let pending = state
        .deps
        .protocol
        .pending_delivery_storage
        .list(&jid.to_bare())
        .await
        .expect("list promoted rows");
    for expected_id in ["superseded-prefix", "superseded-backlog"] {
        assert!(
            pending.iter().any(|row| {
                matches!(
                    &row.payload,
                    waddle_xmpp::pending_delivery::PendingPayload::Transient(message)
                        if message.id.as_ref().is_some_and(|id| id.0 == expected_id)
                )
            }),
            "terminal recovery must promote {expected_id}"
        );
    }
}

/// Sequence-bound pending_delivery rows released during terminal cleanup must
/// be re-driven to a currently available replacement resource even when that
/// replacement already spent its initial once-per-session offline-flush claim.
#[tokio::test]
async fn terminal_release_reflushes_row_backed_pending_delivery_to_live_replacement() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/release-reflush".parse().expect("jid");
    let (old_tx, mut old_rx) = mpsc::channel::<OutboundStanza>(4);
    let old_owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), old_tx);

    let mut old_conn = WsConnState::new();
    old_conn.phase = ConnectionPhase::ready(jid.clone(), false);
    old_conn.authenticated_session = Some(create_test_session(state.as_ref(), "alice").await);
    old_conn.registry_owner = Some(old_owner);
    let enabled = handle_xmpp_frame(
        "<enable xmlns='urn:xmpp:sm:3' resume='true'/>",
        "example.com",
        state.as_ref(),
        &mut old_conn,
    )
    .await;
    let old_stream_id = Element::from_str(&enabled[0])
        .expect("enabled xml")
        .attr("id")
        .expect("stream id")
        .to_string();
    old_conn.publish_pending_sm_enable(state.as_ref());
    seed_claimed_pending_row(state.as_ref(), &jid.to_bare(), &old_stream_id, 1).await;
    let _ = old_conn.sm_state.record_outbound(
        message_frame_xml_with_id("terminal-release-replay-copy".to_string()),
        SmEvictionPath::Batch,
    );
    old_conn.begin_terminal_sm_recovery();

    let (replacement_tx, mut replacement_rx) = mpsc::channel::<OutboundStanza>(4);
    let replacement_owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), replacement_tx);
    state
        .deps
        .protocol
        .connection_registry
        .update_presence(&jid, true, 0);
    let replacement_stream = waddle_xmpp::pending_delivery::SmSessionId::new(
        "terminal-release-replacement-stream".to_string(),
    );
    assert!(state
        .deps
        .protocol
        .connection_registry
        .set_sm_stream_id_if_owner(&jid, &replacement_owner, Some(replacement_stream.clone())));
    let replacement_entry = state
        .deps
        .protocol
        .connection_registry
        .entry_if_owner(&jid, &replacement_owner)
        .expect("replacement entry");
    assert!(
        replacement_entry.claim_offline_flush(),
        "replacement already spent its initial offline flush claim"
    );

    assert_eq!(
        cleanup_connection_shutdown(state.as_ref(), &mut old_rx, &mut old_conn, false).await,
        super::super::cleanup::ConnectionShutdownOutcome::NotPersisted
    );

    let delivered = replacement_rx
        .recv()
        .await
        .expect("terminal cleanup should re-drive the released row");
    match &delivered.stanza {
        Stanza::Message(message) => {
            assert_eq!(
                message.id.as_ref().map(|id| id.0.as_str()),
                Some("pd-1"),
                "the live replacement should receive the released pending_delivery row"
            );
        }
        other => panic!("expected replayed Message, got {other:?}"),
    }
    let rows = state
        .deps
        .protocol
        .pending_delivery_storage
        .list(&jid.to_bare())
        .await
        .expect("list pending rows");
    assert_eq!(
        rows.len(),
        1,
        "the row remains until the replacement acks it"
    );
    assert_eq!(
        rows[0].flushed_in_session.as_ref(),
        Some(&replacement_stream),
        "terminal cleanup must rebind the released row to the live replacement stream"
    );
}

/// Model a terminal pending-row release transaction that fails before it can
/// free any claimed rows, while leaving the underlying storage usable for the
/// rest of cleanup.
struct FailFirstReleaseRowsPendingStorage {
    inner: waddle_xmpp::pending_delivery::storage::InMemoryPendingDeliveryStorage,
    fail_next_release: std::sync::atomic::AtomicBool,
}

impl FailFirstReleaseRowsPendingStorage {
    fn new() -> Self {
        Self {
            inner:
                waddle_xmpp::pending_delivery::storage::InMemoryPendingDeliveryStorage::unlimited(),
            fail_next_release: std::sync::atomic::AtomicBool::new(true),
        }
    }
}

#[async_trait::async_trait]
impl waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage
    for FailFirstReleaseRowsPendingStorage
{
    async fn insert(
        &self,
        row: waddle_xmpp::pending_delivery::PendingRow,
    ) -> Result<
        waddle_xmpp::pending_delivery::InsertOutcome,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner.insert(row).await
    }

    async fn list(
        &self,
        recipient: &BareJid,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::PendingRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner.list(recipient).await
    }

    async fn claim_for_session(
        &self,
        recipient: &BareJid,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::PendingRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner.claim_for_session(recipient, session).await
    }

    async fn claim_batch_for_session(
        &self,
        recipient: &BareJid,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
        after: Option<&waddle_xmpp::pending_delivery::PendingRowId>,
        limit: usize,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::PendingRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner
            .claim_batch_for_session(recipient, session, after, limit)
            .await
    }

    async fn delete_claimed(
        &self,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.delete_claimed(session).await
    }

    async fn delete_row(
        &self,
        id: &waddle_xmpp::pending_delivery::PendingRowId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.delete_row(id).await
    }

    async fn release_claim(
        &self,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.release_claim(session).await
    }

    async fn release_row(
        &self,
        id: &waddle_xmpp::pending_delivery::PendingRowId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.release_row(id).await
    }

    async fn release_rows_for_outbound_sequences(
        &self,
        recipient: &BareJid,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
        sequences: &std::collections::HashSet<u32>,
    ) -> waddle_xmpp::pending_delivery::storage::ReleaseRowsForOutboundSequencesOutcome {
        if self
            .fail_next_release
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return waddle_xmpp::pending_delivery::storage::ReleaseRowsForOutboundSequencesOutcome::failed(
                waddle_xmpp::pending_delivery::storage::PendingStorageError::Other(
                    "simulated release transaction failure".to_string(),
                ),
            );
        }
        self.inner
            .release_rows_for_outbound_sequences(recipient, session, sequences)
            .await
    }

    async fn record_pushed_at(
        &self,
        id: &waddle_xmpp::pending_delivery::PendingRowId,
        sequence: u32,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.record_pushed_at(id, sequence).await
    }

    async fn delete_acked_in_window(
        &self,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
        from_exclusive: u32,
        to_inclusive: u32,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner
            .delete_acked_in_window(session, from_exclusive, to_inclusive)
            .await
    }

    async fn list_orphaned_claims(
        &self,
        live_sessions: &[waddle_xmpp::pending_delivery::SmSessionId],
        claimed_before_ms: i64,
    ) -> Result<
        Vec<(
            waddle_xmpp::pending_delivery::PendingRowId,
            waddle_xmpp::pending_delivery::SmSessionId,
        )>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner
            .list_orphaned_claims(live_sessions, claimed_before_ms)
            .await
    }

    async fn count(
        &self,
        recipient: &BareJid,
    ) -> Result<u32, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.count(recipient).await
    }

    async fn delete_older_than(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.delete_older_than(cutoff).await
    }

    async fn scrub_for_tombstone(
        &self,
        target: &waddle_xmpp::tombstone::TombstoneTarget,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.scrub_for_tombstone(target).await
    }
}

/// Force exactly one `claim_batch_for_session` failure while keeping a real
/// in-memory backend behind it so terminal cleanup can exercise the production
/// "spent replacement CAS + retryable unclaimed row" path.
struct FailFirstClaimBatchPendingStorage {
    inner: waddle_xmpp::pending_delivery::storage::InMemoryPendingDeliveryStorage,
    remaining_claim_batch_failures: std::sync::atomic::AtomicU32,
}

impl FailFirstClaimBatchPendingStorage {
    fn new() -> Self {
        Self {
            inner:
                waddle_xmpp::pending_delivery::storage::InMemoryPendingDeliveryStorage::unlimited(),
            // Cleanup now retries the re-drive once (pre-session and
            // pre-promotion), so a PERSISTENT abort needs both bounded
            // in-cleanup attempts to fail before the re-arm semantics can be
            // observed.
            remaining_claim_batch_failures: std::sync::atomic::AtomicU32::new(2),
        }
    }
}

#[async_trait::async_trait]
impl waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage
    for FailFirstClaimBatchPendingStorage
{
    async fn insert(
        &self,
        row: waddle_xmpp::pending_delivery::PendingRow,
    ) -> Result<
        waddle_xmpp::pending_delivery::InsertOutcome,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner.insert(row).await
    }

    async fn list(
        &self,
        recipient: &BareJid,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::PendingRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner.list(recipient).await
    }

    async fn claim_for_session(
        &self,
        recipient: &BareJid,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::PendingRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner.claim_for_session(recipient, session).await
    }

    async fn claim_batch_for_session(
        &self,
        recipient: &BareJid,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
        after: Option<&waddle_xmpp::pending_delivery::PendingRowId>,
        limit: usize,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::PendingRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        let remaining = self
            .remaining_claim_batch_failures
            .load(std::sync::atomic::Ordering::SeqCst);
        if remaining > 0 {
            self.remaining_claim_batch_failures
                .store(remaining - 1, std::sync::atomic::Ordering::SeqCst);
            return Err(
                waddle_xmpp::pending_delivery::storage::PendingStorageError::Other(
                    "simulated claim-batch failure".to_string(),
                ),
            );
        }
        self.inner
            .claim_batch_for_session(recipient, session, after, limit)
            .await
    }

    async fn delete_claimed(
        &self,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.delete_claimed(session).await
    }

    async fn delete_row(
        &self,
        id: &waddle_xmpp::pending_delivery::PendingRowId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.delete_row(id).await
    }

    async fn release_claim(
        &self,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.release_claim(session).await
    }

    async fn release_row(
        &self,
        id: &waddle_xmpp::pending_delivery::PendingRowId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.release_row(id).await
    }

    async fn record_pushed_at(
        &self,
        id: &waddle_xmpp::pending_delivery::PendingRowId,
        sequence: u32,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.record_pushed_at(id, sequence).await
    }

    async fn delete_acked_in_window(
        &self,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
        from_exclusive: u32,
        to_inclusive: u32,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner
            .delete_acked_in_window(session, from_exclusive, to_inclusive)
            .await
    }

    async fn list_orphaned_claims(
        &self,
        live: &[waddle_xmpp::pending_delivery::SmSessionId],
        claimed_before_ms: i64,
    ) -> Result<
        Vec<(
            waddle_xmpp::pending_delivery::PendingRowId,
            waddle_xmpp::pending_delivery::SmSessionId,
        )>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner
            .list_orphaned_claims(live, claimed_before_ms)
            .await
    }

    async fn count(
        &self,
        recipient: &BareJid,
    ) -> Result<u32, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.count(recipient).await
    }

    async fn delete_older_than(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.delete_older_than(cutoff).await
    }

    async fn stamp_unstamped_claims(
        &self,
        now_ms: i64,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.stamp_unstamped_claims(now_ms).await
    }

    async fn scrub_for_tombstone(
        &self,
        target: &waddle_xmpp::tombstone::TombstoneTarget,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.scrub_for_tombstone(target).await
    }
}

struct BlockOnNthListEntriesStorage {
    inner: waddle_xmpp::xep::xep0191::InMemoryBlockingStorage,
    calls: std::sync::atomic::AtomicU32,
    block_on_call: u32,
    user: BareJid,
    blocked_entries: Vec<jid::Jid>,
}

#[async_trait::async_trait]
impl waddle_xmpp::xep::xep0191::BlockingStorage for BlockOnNthListEntriesStorage {
    async fn list_blocked_jids(
        &self,
        user: &BareJid,
    ) -> Result<Vec<BareJid>, waddle_xmpp::xep::xep0191::BlockingStorageError> {
        Ok(self
            .list_blocked_jid_entries(user)
            .await?
            .into_iter()
            .filter_map(|entry| entry.try_into().ok())
            .collect())
    }

    async fn list_blocked_jid_entries(
        &self,
        user: &BareJid,
    ) -> Result<Vec<jid::Jid>, waddle_xmpp::xep::xep0191::BlockingStorageError> {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        if call == self.block_on_call && *user == self.user {
            self.inner
                .set_blocklist_jids(self.user.clone(), self.blocked_entries.clone());
        }
        waddle_xmpp::xep::xep0191::BlockingStorage::list_blocked_jid_entries(&self.inner, user)
            .await
    }
}

/// A terminal release failure must not promote the replay copy of a claimed
/// pending row into a second durable row. The original row should survive and
/// become ordinary pending delivery again once the dead stream's claim is
/// released later in cleanup.
#[tokio::test]
async fn terminal_release_failure_blocks_row_backed_replay_promotion() {
    let sm_registry = Arc::new(waddle_xmpp::stream_management::InMemorySmSessionRegistry::new());
    let pending_storage: Arc<dyn waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage> =
        Arc::new(FailFirstReleaseRowsPendingStorage::new());
    let state = create_test_websocket_state_with_sm_registry_and_pending_storage(
        Arc::clone(&sm_registry),
        pending_storage,
    )
    .await;
    let jid: FullJid = "alice@example.com/release-failure".parse().expect("jid");
    let (old_tx, mut old_rx) = mpsc::channel::<OutboundStanza>(4);
    let old_owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), old_tx);

    let mut old_conn = WsConnState::new();
    old_conn.phase = ConnectionPhase::ready(jid.clone(), false);
    old_conn.authenticated_session = Some(create_test_session(state.as_ref(), "alice").await);
    old_conn.registry_owner = Some(old_owner);
    let enabled = handle_xmpp_frame(
        "<enable xmlns='urn:xmpp:sm:3' resume='true'/>",
        "example.com",
        state.as_ref(),
        &mut old_conn,
    )
    .await;
    let old_stream_id = Element::from_str(&enabled[0])
        .expect("enabled xml")
        .attr("id")
        .expect("stream id")
        .to_string();
    old_conn.publish_pending_sm_enable(state.as_ref());
    seed_claimed_pending_row(state.as_ref(), &jid.to_bare(), &old_stream_id, 1).await;
    let _ = old_conn.sm_state.record_outbound(
        message_frame_xml_with_id("terminal-release-replay-copy".to_string()),
        SmEvictionPath::Batch,
    );
    old_conn.begin_terminal_sm_recovery();

    assert_eq!(
        cleanup_connection_shutdown(state.as_ref(), &mut old_rx, &mut old_conn, false).await,
        super::super::cleanup::ConnectionShutdownOutcome::NotPersisted
    );

    let rows = state
        .deps
        .protocol
        .pending_delivery_storage
        .list(&jid.to_bare())
        .await
        .expect("list pending rows");
    assert_eq!(
        rows.len(),
        1,
        "cleanup must not promote the replay copy into a duplicate pending row"
    );
    assert_eq!(
        rows[0].flushed_in_session, None,
        "cleanup should eventually release the dead stream's claim on the original row"
    );
    assert!(matches!(
        &rows[0].payload,
        waddle_xmpp::pending_delivery::PendingPayload::Transient(message)
            if message.id.as_ref().is_some_and(|id| id.0 == "pd-1")
    ));
}

/// Fail one `list` and one `release_rows_for_outbound_sequences` call when
/// armed, leaving the in-memory backend intact so a later retry can converge.
struct FailOnceListAndReleasePendingStorage {
    inner: waddle_xmpp::pending_delivery::storage::InMemoryPendingDeliveryStorage,
    fail_next_list: std::sync::atomic::AtomicBool,
    fail_next_release: std::sync::atomic::AtomicBool,
}

impl FailOnceListAndReleasePendingStorage {
    fn new() -> Self {
        Self {
            inner:
                waddle_xmpp::pending_delivery::storage::InMemoryPendingDeliveryStorage::unlimited(),
            fail_next_list: std::sync::atomic::AtomicBool::new(false),
            fail_next_release: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn arm(&self) {
        self.fail_next_list
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.fail_next_release
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage
    for FailOnceListAndReleasePendingStorage
{
    async fn insert(
        &self,
        row: waddle_xmpp::pending_delivery::PendingRow,
    ) -> Result<
        waddle_xmpp::pending_delivery::InsertOutcome,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner.insert(row).await
    }

    async fn list(
        &self,
        recipient: &BareJid,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::PendingRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        if self
            .fail_next_list
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(
                waddle_xmpp::pending_delivery::storage::PendingStorageError::Other(
                    "simulated list failure".to_string(),
                ),
            );
        }
        self.inner.list(recipient).await
    }

    async fn claim_for_session(
        &self,
        recipient: &BareJid,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::PendingRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner.claim_for_session(recipient, session).await
    }

    async fn claim_batch_for_session(
        &self,
        recipient: &BareJid,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
        after: Option<&waddle_xmpp::pending_delivery::PendingRowId>,
        limit: usize,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::PendingRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner
            .claim_batch_for_session(recipient, session, after, limit)
            .await
    }

    async fn delete_claimed(
        &self,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.delete_claimed(session).await
    }

    async fn delete_row(
        &self,
        id: &waddle_xmpp::pending_delivery::PendingRowId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.delete_row(id).await
    }

    async fn release_claim(
        &self,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.release_claim(session).await
    }

    async fn release_row(
        &self,
        id: &waddle_xmpp::pending_delivery::PendingRowId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.release_row(id).await
    }

    async fn release_rows_for_outbound_sequences(
        &self,
        recipient: &BareJid,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
        sequences: &std::collections::HashSet<u32>,
    ) -> waddle_xmpp::pending_delivery::storage::ReleaseRowsForOutboundSequencesOutcome {
        if self
            .fail_next_release
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return waddle_xmpp::pending_delivery::storage::ReleaseRowsForOutboundSequencesOutcome::failed(
                waddle_xmpp::pending_delivery::storage::PendingStorageError::Other(
                    "simulated release transaction failure".to_string(),
                ),
            );
        }
        self.inner
            .release_rows_for_outbound_sequences(recipient, session, sequences)
            .await
    }

    async fn record_pushed_at(
        &self,
        id: &waddle_xmpp::pending_delivery::PendingRowId,
        sequence: u32,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.record_pushed_at(id, sequence).await
    }

    async fn delete_acked_in_window(
        &self,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
        from_exclusive: u32,
        to_inclusive: u32,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner
            .delete_acked_in_window(session, from_exclusive, to_inclusive)
            .await
    }

    async fn list_orphaned_claims(
        &self,
        live_sessions: &[waddle_xmpp::pending_delivery::SmSessionId],
        claimed_before_ms: i64,
    ) -> Result<
        Vec<(
            waddle_xmpp::pending_delivery::PendingRowId,
            waddle_xmpp::pending_delivery::SmSessionId,
        )>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner
            .list_orphaned_claims(live_sessions, claimed_before_ms)
            .await
    }

    async fn count(
        &self,
        recipient: &BareJid,
    ) -> Result<u32, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.count(recipient).await
    }

    async fn delete_older_than(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.delete_older_than(cutoff).await
    }

    async fn scrub_for_tombstone(
        &self,
        target: &waddle_xmpp::tombstone::TombstoneTarget,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.scrub_for_tombstone(target).await
    }
}

/// PR #1669 round 8: when BOTH the ownership preflight (`list`) and the
/// sequence release fail during terminal cleanup, row-backed replay copies
/// cannot be told apart from fresh work. The whole queue must stay out of
/// promotion (no duplicate row is minted while the original stays claimed)
/// and converge via the SM-expiry janitor once ownership can be discovered
/// again.
#[tokio::test]
async fn terminal_ownership_discovery_failure_defers_promotion_until_janitor_retry() {
    let sm_registry = Arc::new(waddle_xmpp::stream_management::InMemorySmSessionRegistry::new());
    let failing_storage = Arc::new(FailOnceListAndReleasePendingStorage::new());
    let pending_storage: Arc<dyn waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage> =
        Arc::clone(&failing_storage) as _;
    let state = create_test_websocket_state_with_sm_registry_and_pending_storage(
        Arc::clone(&sm_registry),
        pending_storage,
    )
    .await;
    let jid: FullJid = "alice@example.com/ownership-unknown".parse().expect("jid");
    let (old_tx, mut old_rx) = mpsc::channel::<OutboundStanza>(4);
    let old_owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), old_tx);

    let mut old_conn = WsConnState::new();
    old_conn.phase = ConnectionPhase::ready(jid.clone(), false);
    old_conn.authenticated_session = Some(create_test_session(state.as_ref(), "alice").await);
    old_conn.registry_owner = Some(old_owner);
    let enabled = handle_xmpp_frame(
        "<enable xmlns='urn:xmpp:sm:3' resume='true'/>",
        "example.com",
        state.as_ref(),
        &mut old_conn,
    )
    .await;
    let old_stream_id = Element::from_str(&enabled[0])
        .expect("enabled xml")
        .attr("id")
        .expect("stream id")
        .to_string();
    old_conn.publish_pending_sm_enable(state.as_ref());
    seed_claimed_pending_row(state.as_ref(), &jid.to_bare(), &old_stream_id, 1).await;
    let _ = old_conn.sm_state.record_outbound(
        message_frame_xml_with_id("terminal-release-replay-copy".to_string()),
        SmEvictionPath::Batch,
    );
    old_conn.begin_terminal_sm_recovery();

    failing_storage.arm();
    assert_eq!(
        cleanup_connection_shutdown(state.as_ref(), &mut old_rx, &mut old_conn, false).await,
        super::super::cleanup::ConnectionShutdownOutcome::NotPersisted
    );

    let rows = state
        .deps
        .protocol
        .pending_delivery_storage
        .list(&jid.to_bare())
        .await
        .expect("list pending rows");
    assert_eq!(
        rows.len(),
        1,
        "with ownership unknown, no replay copy may be promoted into a duplicate row"
    );
    assert_eq!(
        rows[0].flushed_in_session.as_ref().map(|s| s.as_str()),
        Some(old_stream_id.as_str()),
        "the original row must stay claimed until the retry discovers ownership"
    );

    // Storage has recovered (the failures were one-shot). The janitor's next
    // sweep re-runs ownership discovery, releases the row, strips the replay
    // copy, and settles the synthetic session without duplicating anything.
    crate::server::session_janitors::run_sm_expiry_sweep(&state).await;

    let rows = state
        .deps
        .protocol
        .pending_delivery_storage
        .list(&jid.to_bare())
        .await
        .expect("list pending rows after janitor retry");
    assert_eq!(
        rows.len(),
        1,
        "the janitor retry must not promote the replay copy into a duplicate row"
    );
    assert_eq!(
        rows[0].flushed_in_session, None,
        "the janitor retry must release the dead stream's claim on the original row"
    );
    assert!(
        sm_registry
            .drain_expired()
            .await
            .expect("drain after retry")
            .is_empty(),
        "the synthetic session must settle once ownership discovery succeeds"
    );
}

/// PR #1669 round 8: when the ownership preflight identifies a sequence-bound
/// row but the release itself fails, the replay copy is stripped and promotion
/// later frees the row via `release_claim`. The rows must then be re-driven to
/// a live replacement whose once-only offline flush is already spent, instead
/// of sitting pending until some future bind.
#[tokio::test]
async fn terminal_release_failure_redrives_row_after_promotion_frees_claim() {
    let sm_registry = Arc::new(waddle_xmpp::stream_management::InMemorySmSessionRegistry::new());
    let pending_storage: Arc<dyn waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage> =
        Arc::new(FailFirstReleaseRowsPendingStorage::new());
    let state = create_test_websocket_state_with_sm_registry_and_pending_storage(
        Arc::clone(&sm_registry),
        pending_storage,
    )
    .await;
    let jid: FullJid = "alice@example.com/release-failure-redrive"
        .parse()
        .expect("jid");
    let (old_tx, mut old_rx) = mpsc::channel::<OutboundStanza>(4);
    let old_owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), old_tx);

    let mut old_conn = WsConnState::new();
    old_conn.phase = ConnectionPhase::ready(jid.clone(), false);
    old_conn.authenticated_session = Some(create_test_session(state.as_ref(), "alice").await);
    old_conn.registry_owner = Some(old_owner);
    let enabled = handle_xmpp_frame(
        "<enable xmlns='urn:xmpp:sm:3' resume='true'/>",
        "example.com",
        state.as_ref(),
        &mut old_conn,
    )
    .await;
    let old_stream_id = Element::from_str(&enabled[0])
        .expect("enabled xml")
        .attr("id")
        .expect("stream id")
        .to_string();
    old_conn.publish_pending_sm_enable(state.as_ref());
    seed_claimed_pending_row(state.as_ref(), &jid.to_bare(), &old_stream_id, 1).await;
    let _ = old_conn.sm_state.record_outbound(
        message_frame_xml_with_id("terminal-release-replay-copy".to_string()),
        SmEvictionPath::Batch,
    );
    old_conn.begin_terminal_sm_recovery();

    let (replacement_tx, mut replacement_rx) = mpsc::channel::<OutboundStanza>(4);
    let replacement_owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), replacement_tx);
    state
        .deps
        .protocol
        .connection_registry
        .update_presence(&jid, true, 0);
    let replacement_stream = waddle_xmpp::pending_delivery::SmSessionId::new(
        "release-failure-replacement-stream".to_string(),
    );
    assert!(state
        .deps
        .protocol
        .connection_registry
        .set_sm_stream_id_if_owner(&jid, &replacement_owner, Some(replacement_stream.clone())));
    let replacement_entry = state
        .deps
        .protocol
        .connection_registry
        .entry_if_owner(&jid, &replacement_owner)
        .expect("replacement entry");
    assert!(
        replacement_entry.claim_offline_flush(),
        "replacement already spent its initial offline flush claim"
    );

    assert_eq!(
        cleanup_connection_shutdown(state.as_ref(), &mut old_rx, &mut old_conn, false).await,
        super::super::cleanup::ConnectionShutdownOutcome::NotPersisted
    );

    let delivered = replacement_rx
        .recv()
        .await
        .expect("cleanup must re-drive the row freed by promotion's release_claim");
    match &delivered.stanza {
        Stanza::Message(message) => {
            assert_eq!(
                message.id.as_ref().map(|id| id.0.as_str()),
                Some("pd-1"),
                "the live replacement should receive the release-failed row after \
                 promotion freed its claim"
            );
        }
        other => panic!("expected replayed Message, got {other:?}"),
    }
    let rows = state
        .deps
        .protocol
        .pending_delivery_storage
        .list(&jid.to_bare())
        .await
        .expect("list pending rows");
    assert_eq!(
        rows.len(),
        1,
        "the replay copy must not be promoted into a duplicate row"
    );
    assert_eq!(
        rows[0].flushed_in_session.as_ref(),
        Some(&replacement_stream),
        "the post-promotion re-drive must rebind the freed row to the live replacement"
    );
}

/// PR #1669 round 8: the terminal prefix promotion snapshots online resources
/// once. A replacement that binds and spends its once-only offline flush while
/// a prefix insert is still in flight would otherwise leave the freshly queued
/// row stranded (and later incremental traffic could overtake it). The
/// post-prefix recheck must re-drive the row to that replacement.
#[tokio::test]
async fn terminal_prefix_promotion_redrives_rows_to_replacement_bound_mid_insert() {
    let sm_registry = Arc::new(waddle_xmpp::stream_management::InMemorySmSessionRegistry::new());
    let gated_storage = Arc::new(GatedFirstInsertPendingStorage::new());
    let pending_storage: Arc<dyn waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage> =
        Arc::clone(&gated_storage) as _;
    let state = create_test_websocket_state_with_sm_registry_and_pending_storage(
        Arc::clone(&sm_registry),
        pending_storage,
    )
    .await;
    let jid: FullJid = "alice@example.com/prefix-race".parse().expect("jid");
    let (old_tx, mut old_rx) = mpsc::channel::<OutboundStanza>(4);
    let old_owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), old_tx);

    let mut old_conn = WsConnState::new();
    old_conn.phase = ConnectionPhase::ready(jid.clone(), false);
    old_conn.authenticated_session = Some(create_test_session(state.as_ref(), "alice").await);
    old_conn.registry_owner = Some(old_owner);
    let _ = handle_xmpp_frame(
        "<enable xmlns='urn:xmpp:sm:3' resume='true'/>",
        "example.com",
        state.as_ref(),
        &mut old_conn,
    )
    .await;
    old_conn.publish_pending_sm_enable(state.as_ref());
    let mut queued = xmpp_parsers::message::Message::new(Some(jid::Jid::from(jid.to_bare())));
    queued.from = Some("bob@example.com/phone".parse().expect("sender jid"));
    queued.id = Some(xmpp_parsers::message::Id("prefix-race-message".to_string()));
    queued.type_ = xmpp_parsers::message::MessageType::Chat;
    queued.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "queued while the replacement was binding".to_string(),
    );
    waddle_xmpp::xep::xep0334::add_hint(
        &mut queued,
        waddle_xmpp::xep::xep0334::Hint::NoPermanentStore,
    );
    let _ = old_conn.sm_state.record_outbound(
        waddle_xmpp::parser::stanza_to_string(queued).expect("serialize queued message"),
        SmEvictionPath::Batch,
    );
    old_conn.begin_terminal_sm_recovery();

    let cleanup_state = Arc::clone(&state);
    let cleanup = tokio::spawn(async move {
        cleanup_connection_shutdown(cleanup_state.as_ref(), &mut old_rx, &mut old_conn, false).await
    });
    gated_storage.wait_until_insert_blocks().await;

    // The replacement binds while the prefix insert is mid-flight and spends
    // its once-per-session offline flush before the row commits.
    let (replacement_tx, mut replacement_rx) = mpsc::channel::<OutboundStanza>(4);
    let replacement_owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), replacement_tx);
    state
        .deps
        .protocol
        .connection_registry
        .update_presence(&jid, true, 0);
    let replacement_stream =
        waddle_xmpp::pending_delivery::SmSessionId::new("prefix-race-replacement".to_string());
    assert!(state
        .deps
        .protocol
        .connection_registry
        .set_sm_stream_id_if_owner(&jid, &replacement_owner, Some(replacement_stream.clone())));
    let replacement_entry = state
        .deps
        .protocol
        .connection_registry
        .entry_if_owner(&jid, &replacement_owner)
        .expect("replacement entry");
    assert!(
        replacement_entry.claim_offline_flush(),
        "replacement already spent its initial offline flush claim"
    );
    gated_storage.release_insert();

    assert_eq!(
        cleanup.await.expect("cleanup task"),
        super::super::cleanup::ConnectionShutdownOutcome::NotPersisted
    );

    let delivered = replacement_rx
        .recv()
        .await
        .expect("the post-prefix recheck must re-drive the freshly queued row");
    match &delivered.stanza {
        Stanza::Message(message) => {
            assert_eq!(
                message.id.as_ref().map(|id| id.0.as_str()),
                Some("prefix-race-message"),
                "the replacement should receive the row inserted while it bound"
            );
        }
        other => panic!("expected replayed Message, got {other:?}"),
    }
    let rows = state
        .deps
        .protocol
        .pending_delivery_storage
        .list(&jid.to_bare())
        .await
        .expect("list pending rows");
    assert_eq!(rows.len(), 1, "exactly the one promoted row exists");
    assert_eq!(
        rows[0].flushed_in_session.as_ref(),
        Some(&replacement_stream),
        "the re-driven row must be claimed by the live replacement stream"
    );
}

/// Reject every insert whose Transient payload carries `fail_id`, and gate
/// the first surviving insert behind the wrapped storage's semaphore. Models
/// a per-stanza durable-storage failure that persists across promotion
/// retries while a replacement binds mid-insert.
struct FailInsertsForMessageIdPendingStorage {
    inner: GatedFirstInsertPendingStorage,
    fail_id: &'static str,
}

#[async_trait::async_trait]
impl waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage
    for FailInsertsForMessageIdPendingStorage
{
    async fn insert(
        &self,
        row: waddle_xmpp::pending_delivery::PendingRow,
    ) -> Result<
        waddle_xmpp::pending_delivery::InsertOutcome,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        if matches!(
            &row.payload,
            waddle_xmpp::pending_delivery::PendingPayload::Transient(message)
                if message.id.as_ref().is_some_and(|id| id.0 == self.fail_id)
        ) {
            return Err(
                waddle_xmpp::pending_delivery::storage::PendingStorageError::Other(
                    "simulated per-stanza insert failure".to_string(),
                ),
            );
        }
        self.inner.insert(row).await
    }

    async fn list(
        &self,
        recipient: &BareJid,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::PendingRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner.list(recipient).await
    }

    async fn claim_for_session(
        &self,
        recipient: &BareJid,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::PendingRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner.claim_for_session(recipient, session).await
    }

    async fn claim_batch_for_session(
        &self,
        recipient: &BareJid,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
        after: Option<&waddle_xmpp::pending_delivery::PendingRowId>,
        limit: usize,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::PendingRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner
            .claim_batch_for_session(recipient, session, after, limit)
            .await
    }

    async fn delete_claimed(
        &self,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.delete_claimed(session).await
    }

    async fn delete_row(
        &self,
        id: &waddle_xmpp::pending_delivery::PendingRowId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.delete_row(id).await
    }

    async fn release_claim(
        &self,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.release_claim(session).await
    }

    async fn release_row(
        &self,
        id: &waddle_xmpp::pending_delivery::PendingRowId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.release_row(id).await
    }

    async fn record_pushed_at(
        &self,
        id: &waddle_xmpp::pending_delivery::PendingRowId,
        sequence: u32,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.record_pushed_at(id, sequence).await
    }

    async fn delete_acked_in_window(
        &self,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
        from_exclusive: u32,
        to_inclusive: u32,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner
            .delete_acked_in_window(session, from_exclusive, to_inclusive)
            .await
    }

    async fn list_orphaned_claims(
        &self,
        live_sessions: &[waddle_xmpp::pending_delivery::SmSessionId],
        claimed_before_ms: i64,
    ) -> Result<
        Vec<(
            waddle_xmpp::pending_delivery::PendingRowId,
            waddle_xmpp::pending_delivery::SmSessionId,
        )>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner
            .list_orphaned_claims(live_sessions, claimed_before_ms)
            .await
    }

    async fn count(
        &self,
        recipient: &BareJid,
    ) -> Result<u32, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.count(recipient).await
    }

    async fn delete_older_than(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.delete_older_than(cutoff).await
    }

    async fn scrub_for_tombstone(
        &self,
        target: &waddle_xmpp::tombstone::TombstoneTarget,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.scrub_for_tombstone(target).await
    }
}

fn transient_chat_message_xml(id: &str, recipient: &BareJid) -> String {
    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(recipient.clone())));
    message.from = Some("bob@example.com/phone".parse().expect("sender jid"));
    message.id = Some(xmpp_parsers::message::Id(id.to_string()));
    message.type_ = xmpp_parsers::message::MessageType::Chat;
    message
        .bodies
        .insert(xmpp_parsers::message::Lang::new(), format!("body of {id}"));
    waddle_xmpp::xep::xep0334::add_hint(
        &mut message,
        waddle_xmpp::xep::xep0334::Hint::NoPermanentStore,
    );
    waddle_xmpp::parser::stanza_to_string(message).expect("serialize message")
}

/// Codex round-9 finding 1: an overflow row queued while a replacement binds
/// must be re-driven BEFORE the next channel frame is promoted, otherwise the
/// later frame's fresh online snapshot delivers it live ahead of the earlier
/// stanza and inverts the stream's FIFO order.
#[tokio::test]
async fn terminal_overflow_queued_row_redrives_before_next_frame_preserving_fifo() {
    let sm_registry = Arc::new(waddle_xmpp::stream_management::InMemorySmSessionRegistry::new());
    let gated_storage = Arc::new(GatedFirstInsertPendingStorage::new());
    let pending_storage: Arc<dyn waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage> =
        Arc::clone(&gated_storage) as _;
    let state = create_test_websocket_state_with_sm_registry_and_pending_storage(
        Arc::clone(&sm_registry),
        pending_storage,
    )
    .await;
    let jid: FullJid = "alice@example.com/overflow-fifo".parse().expect("jid");
    let (old_tx, mut old_rx) = mpsc::channel::<OutboundStanza>(8);
    let old_owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), old_tx.clone());

    let mut old_conn = WsConnState::new();
    old_conn.phase = ConnectionPhase::ready(jid.clone(), false);
    old_conn.authenticated_session = Some(create_test_session(state.as_ref(), "alice").await);
    old_conn.registry_owner = Some(old_owner);
    let _ = handle_xmpp_frame(
        "<enable xmlns='urn:xmpp:sm:3' resume='true'/>",
        "example.com",
        state.as_ref(),
        &mut old_conn,
    )
    .await;
    old_conn.publish_pending_sm_enable(state.as_ref());
    old_conn.begin_terminal_sm_recovery();

    for id in ["overflow-first", "overflow-second"] {
        let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(jid.to_bare())));
        message.from = Some("bob@example.com/phone".parse().expect("sender jid"));
        message.id = Some(xmpp_parsers::message::Id(id.to_string()));
        message.type_ = xmpp_parsers::message::MessageType::Chat;
        message
            .bodies
            .insert(xmpp_parsers::message::Lang::new(), format!("body of {id}"));
        waddle_xmpp::xep::xep0334::add_hint(
            &mut message,
            waddle_xmpp::xep::xep0334::Hint::NoPermanentStore,
        );
        old_tx
            .send(OutboundStanza::new(Stanza::Message(message)))
            .await
            .expect("queue accepted overflow frame");
    }

    let cleanup_state = Arc::clone(&state);
    let cleanup = tokio::spawn(async move {
        cleanup_connection_shutdown(cleanup_state.as_ref(), &mut old_rx, &mut old_conn, false).await
    });
    gated_storage.wait_until_insert_blocks().await;

    let (replacement_tx, mut replacement_rx) = mpsc::channel::<OutboundStanza>(8);
    let replacement_owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), replacement_tx);
    state
        .deps
        .protocol
        .connection_registry
        .update_presence(&jid, true, 0);
    let replacement_stream =
        waddle_xmpp::pending_delivery::SmSessionId::new("overflow-fifo-replacement".to_string());
    assert!(state
        .deps
        .protocol
        .connection_registry
        .set_sm_stream_id_if_owner(&jid, &replacement_owner, Some(replacement_stream)));
    let replacement_entry = state
        .deps
        .protocol
        .connection_registry
        .entry_if_owner(&jid, &replacement_owner)
        .expect("replacement entry");
    assert!(
        replacement_entry.claim_offline_flush(),
        "replacement already spent its initial offline flush claim"
    );
    gated_storage.release_insert();

    assert_eq!(
        cleanup.await.expect("cleanup task"),
        super::super::cleanup::ConnectionShutdownOutcome::NotPersisted
    );

    let mut delivered_ids = Vec::new();
    for _ in 0..2 {
        let delivered = replacement_rx
            .recv()
            .await
            .expect("both overflow frames must reach the replacement");
        if let Stanza::Message(message) = &delivered.stanza {
            delivered_ids.push(message.id.as_ref().map(|id| id.0.clone()));
        }
    }
    assert_eq!(
        delivered_ids,
        vec![
            Some("overflow-first".to_string()),
            Some("overflow-second".to_string())
        ],
        "the queued first frame must be re-driven before the second frame is \
         promoted live, preserving FIFO"
    );
}

/// Codex round-9 finding 4: rows queued by the displaced promotion while a
/// replacement bound mid-insert are already durable and are NOT re-reported
/// by a promotion retry. They must be re-driven even when a later stanza's
/// persistent storage failure leaves the session retrying.
#[tokio::test]
async fn terminal_promotion_queued_row_redrives_even_while_session_retries() {
    let sm_registry = Arc::new(waddle_xmpp::stream_management::InMemorySmSessionRegistry::new());
    let storage = Arc::new(FailInsertsForMessageIdPendingStorage {
        inner: GatedFirstInsertPendingStorage::new(),
        fail_id: "retry-poisoned",
    });
    let pending_storage: Arc<dyn waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage> =
        Arc::clone(&storage) as _;
    let state = create_test_websocket_state_with_sm_registry_and_pending_storage(
        Arc::clone(&sm_registry),
        pending_storage,
    )
    .await;
    let jid: FullJid = "alice@example.com/queued-despite-retry"
        .parse()
        .expect("jid");
    let (old_tx, mut old_rx) = mpsc::channel::<OutboundStanza>(4);
    let old_owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), old_tx);

    let mut old_conn = WsConnState::new();
    old_conn.phase = ConnectionPhase::ready(jid.clone(), false);
    old_conn.authenticated_session = Some(create_test_session(state.as_ref(), "alice").await);
    old_conn.registry_owner = Some(old_owner);
    let _ = handle_xmpp_frame(
        "<enable xmlns='urn:xmpp:sm:3' resume='true'/>",
        "example.com",
        state.as_ref(),
        &mut old_conn,
    )
    .await;
    old_conn.publish_pending_sm_enable(state.as_ref());
    let _ = old_conn.sm_state.record_outbound(
        transient_chat_message_xml("retry-queued", &jid.to_bare()),
        SmEvictionPath::Batch,
    );
    let _ = old_conn.sm_state.record_outbound(
        transient_chat_message_xml("retry-poisoned", &jid.to_bare()),
        SmEvictionPath::Batch,
    );
    // Drop the principal so cleanup takes the refuse-detach path: the
    // displaced promotion (not the terminal prefix) performs the inserts.
    old_conn.authenticated_session = None;

    let cleanup_state = Arc::clone(&state);
    let cleanup = tokio::spawn(async move {
        cleanup_connection_shutdown(cleanup_state.as_ref(), &mut old_rx, &mut old_conn, false).await
    });
    storage.inner.wait_until_insert_blocks().await;

    // The replacement binds mid-insert with its once-only offline flush
    // spent; the second stanza's insert keeps failing, so the session stays
    // in the retry inventory.
    let (replacement_tx, mut replacement_rx) = mpsc::channel::<OutboundStanza>(4);
    let replacement_owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), replacement_tx);
    state
        .deps
        .protocol
        .connection_registry
        .update_presence(&jid, true, 0);
    let replacement_stream = waddle_xmpp::pending_delivery::SmSessionId::new(
        "queued-despite-retry-replacement".to_string(),
    );
    assert!(state
        .deps
        .protocol
        .connection_registry
        .set_sm_stream_id_if_owner(&jid, &replacement_owner, Some(replacement_stream.clone())));
    let replacement_entry = state
        .deps
        .protocol
        .connection_registry
        .entry_if_owner(&jid, &replacement_owner)
        .expect("replacement entry");
    assert!(
        replacement_entry.claim_offline_flush(),
        "replacement already spent its initial offline flush claim"
    );
    storage.inner.release_insert();

    assert_eq!(
        cleanup.await.expect("cleanup task"),
        super::super::cleanup::ConnectionShutdownOutcome::NotPersisted
    );

    let delivered = replacement_rx
        .recv()
        .await
        .expect("the queued row must be re-driven even though the session retries");
    match &delivered.stanza {
        Stanza::Message(message) => {
            assert_eq!(
                message.id.as_ref().map(|id| id.0.as_str()),
                Some("retry-queued"),
                "the replacement should receive the row queued during its bind"
            );
        }
        other => panic!("expected replayed Message, got {other:?}"),
    }
    let rows = state
        .deps
        .protocol
        .pending_delivery_storage
        .list(&jid.to_bare())
        .await
        .expect("list pending rows");
    assert!(
        rows.iter().any(|row| {
            matches!(
                &row.payload,
                waddle_xmpp::pending_delivery::PendingPayload::Transient(message)
                    if message.id.as_ref().is_some_and(|id| id.0 == "retry-queued")
            ) && row.flushed_in_session.as_ref() == Some(&replacement_stream)
        }),
        "the re-driven row must be claimed by the live replacement stream"
    );
    assert_eq!(
        sm_registry
            .drain_expired()
            .await
            .expect("drain retry inventory")
            .len(),
        1,
        "the poisoned stanza's storage failure must have left the session retrying"
    );
}

/// Codex round-9 finding 2: the janitor must re-drive released row-backed
/// rows BEFORE promoting the remaining queue, otherwise later unacked traffic
/// reaches a live replacement ahead of the earlier released row.
#[tokio::test]
async fn janitor_redrives_released_rows_before_promoting_remainder() {
    use waddle_xmpp::stream_management::{DetachedSession, DetachedUnackedStanza};
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/janitor-fifo".parse().expect("jid");
    let stream_id = "janitor-fifo-expired".to_string();
    seed_claimed_pending_row(state.as_ref(), &jid.to_bare(), &stream_id, 1).await;

    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id: stream_id.clone(),
            user_id: "alice@example.com".to_string(),
            jid: jid.clone(),
            inbound_count: 0,
            shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
            outbound_count: 2,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: vec![
                DetachedUnackedStanza {
                    sequence: 1,
                    stanza_xml: message_frame_xml_with_id("row-backed-copy".to_string()),
                    original_receipt_at: chrono::Utc::now(),
                },
                DetachedUnackedStanza {
                    sequence: 2,
                    stanza_xml: transient_chat_message_xml("janitor-later", &jid.to_bare()),
                    original_receipt_at: chrono::Utc::now(),
                },
            ],
            max_resume_time: Some(0),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        })
        .await
        .expect("store expired session");
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let (replacement_tx, mut replacement_rx) = mpsc::channel::<OutboundStanza>(8);
    let replacement_owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), replacement_tx);
    state
        .deps
        .protocol
        .connection_registry
        .update_presence(&jid, true, 0);
    let replacement_stream =
        waddle_xmpp::pending_delivery::SmSessionId::new("janitor-fifo-replacement".to_string());
    assert!(state
        .deps
        .protocol
        .connection_registry
        .set_sm_stream_id_if_owner(&jid, &replacement_owner, Some(replacement_stream)));
    let replacement_entry = state
        .deps
        .protocol
        .connection_registry
        .entry_if_owner(&jid, &replacement_owner)
        .expect("replacement entry");
    assert!(
        replacement_entry.claim_offline_flush(),
        "replacement already spent its initial offline flush claim"
    );

    crate::server::session_janitors::run_sm_expiry_sweep(&state).await;

    let mut delivered_ids = Vec::new();
    while let Ok(delivered) = replacement_rx.try_recv() {
        if let Stanza::Message(message) = &delivered.stanza {
            delivered_ids.push(message.id.as_ref().map(|id| id.0.clone()));
        }
    }
    assert_eq!(
        delivered_ids,
        vec![Some("pd-1".to_string()), Some("janitor-later".to_string())],
        "the janitor must re-drive the released earlier row before promoting the \
         later unacked stanza to the live replacement"
    );
}

/// Fail the first `fails` inserts whose Transient payload carries `fail_id`,
/// keeping the backend usable otherwise. Models a per-stanza durable-storage
/// failure that recovers on a later retry.
struct FailCountedInsertsForMessageIdPendingStorage {
    inner: waddle_xmpp::pending_delivery::storage::InMemoryPendingDeliveryStorage,
    fail_id: &'static str,
    remaining_failures: std::sync::atomic::AtomicU32,
}

#[async_trait::async_trait]
impl waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage
    for FailCountedInsertsForMessageIdPendingStorage
{
    async fn insert(
        &self,
        row: waddle_xmpp::pending_delivery::PendingRow,
    ) -> Result<
        waddle_xmpp::pending_delivery::InsertOutcome,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        if matches!(
            &row.payload,
            waddle_xmpp::pending_delivery::PendingPayload::Transient(message)
                if message.id.as_ref().is_some_and(|id| id.0 == self.fail_id)
        ) {
            let remaining = self
                .remaining_failures
                .load(std::sync::atomic::Ordering::SeqCst);
            if remaining > 0 {
                self.remaining_failures
                    .store(remaining - 1, std::sync::atomic::Ordering::SeqCst);
                return Err(
                    waddle_xmpp::pending_delivery::storage::PendingStorageError::Other(
                        "simulated per-stanza insert failure".to_string(),
                    ),
                );
            }
        }
        self.inner.insert(row).await
    }

    async fn list(
        &self,
        recipient: &BareJid,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::PendingRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner.list(recipient).await
    }

    async fn claim_for_session(
        &self,
        recipient: &BareJid,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::PendingRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner.claim_for_session(recipient, session).await
    }

    async fn claim_batch_for_session(
        &self,
        recipient: &BareJid,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
        after: Option<&waddle_xmpp::pending_delivery::PendingRowId>,
        limit: usize,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::PendingRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner
            .claim_batch_for_session(recipient, session, after, limit)
            .await
    }

    async fn delete_claimed(
        &self,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.delete_claimed(session).await
    }

    async fn delete_row(
        &self,
        id: &waddle_xmpp::pending_delivery::PendingRowId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.delete_row(id).await
    }

    async fn release_claim(
        &self,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.release_claim(session).await
    }

    async fn release_row(
        &self,
        id: &waddle_xmpp::pending_delivery::PendingRowId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.release_row(id).await
    }

    async fn record_pushed_at(
        &self,
        id: &waddle_xmpp::pending_delivery::PendingRowId,
        sequence: u32,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.record_pushed_at(id, sequence).await
    }

    async fn delete_acked_in_window(
        &self,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
        from_exclusive: u32,
        to_inclusive: u32,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner
            .delete_acked_in_window(session, from_exclusive, to_inclusive)
            .await
    }

    async fn list_orphaned_claims(
        &self,
        live_sessions: &[waddle_xmpp::pending_delivery::SmSessionId],
        claimed_before_ms: i64,
    ) -> Result<
        Vec<(
            waddle_xmpp::pending_delivery::PendingRowId,
            waddle_xmpp::pending_delivery::SmSessionId,
        )>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner
            .list_orphaned_claims(live_sessions, claimed_before_ms)
            .await
    }

    async fn count(
        &self,
        recipient: &BareJid,
    ) -> Result<u32, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.count(recipient).await
    }

    async fn delete_older_than(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.delete_older_than(cutoff).await
    }

    async fn scrub_for_tombstone(
        &self,
        target: &waddle_xmpp::tombstone::TombstoneTarget,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.scrub_for_tombstone(target).await
    }
}

/// Codex 1669 round 9: an early prefix entry whose durable insert fails must
/// keep every LATER entry queued behind it. Batch promotion previously
/// retained only the failed entry while later successes were already
/// inserted (and re-driven) ahead of its retry, inverting the stream's
/// accepted FIFO order at the storage layer.
#[tokio::test]
async fn terminal_prefix_storage_failure_keeps_later_entries_behind_the_failed_one() {
    let sm_registry = Arc::new(waddle_xmpp::stream_management::InMemorySmSessionRegistry::new());
    let pending_storage: Arc<dyn waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage> =
        Arc::new(FailCountedInsertsForMessageIdPendingStorage {
            inner:
                waddle_xmpp::pending_delivery::storage::InMemoryPendingDeliveryStorage::unlimited(),
            fail_id: "prefix-first",
            remaining_failures: std::sync::atomic::AtomicU32::new(1),
        });
    let state = create_test_websocket_state_with_sm_registry_and_pending_storage(
        Arc::clone(&sm_registry),
        pending_storage,
    )
    .await;
    let jid: FullJid = "alice@example.com/prefix-fifo".parse().expect("jid");
    let (old_tx, mut old_rx) = mpsc::channel::<OutboundStanza>(4);
    let old_owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), old_tx);

    let mut old_conn = WsConnState::new();
    old_conn.phase = ConnectionPhase::ready(jid.clone(), false);
    old_conn.authenticated_session = Some(create_test_session(state.as_ref(), "alice").await);
    old_conn.registry_owner = Some(old_owner);
    let _ = handle_xmpp_frame(
        "<enable xmlns='urn:xmpp:sm:3' resume='true'/>",
        "example.com",
        state.as_ref(),
        &mut old_conn,
    )
    .await;
    old_conn.publish_pending_sm_enable(state.as_ref());
    for id in ["prefix-first", "prefix-second"] {
        let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(jid.to_bare())));
        message.from = Some("bob@example.com/phone".parse().expect("sender jid"));
        message.id = Some(xmpp_parsers::message::Id(id.to_string()));
        message.type_ = xmpp_parsers::message::MessageType::Chat;
        message
            .bodies
            .insert(xmpp_parsers::message::Lang::new(), format!("body of {id}"));
        waddle_xmpp::xep::xep0334::add_hint(
            &mut message,
            waddle_xmpp::xep::xep0334::Hint::NoPermanentStore,
        );
        let _ = old_conn.sm_state.record_outbound(
            waddle_xmpp::parser::stanza_to_string(message).expect("serialize message"),
            SmEvictionPath::Batch,
        );
    }
    old_conn.begin_terminal_sm_recovery();

    assert_eq!(
        cleanup_connection_shutdown(state.as_ref(), &mut old_rx, &mut old_conn, false).await,
        super::super::cleanup::ConnectionShutdownOutcome::NotPersisted
    );

    let rows = state
        .deps
        .protocol
        .pending_delivery_storage
        .list(&jid.to_bare())
        .await
        .expect("list pending rows");
    let ids: Vec<Option<String>> = rows
        .iter()
        .map(|row| match &row.payload {
            waddle_xmpp::pending_delivery::PendingPayload::Transient(message) => {
                message.id.as_ref().map(|id| id.0.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        ids,
        vec![
            Some("prefix-first".to_string()),
            Some("prefix-second".to_string())
        ],
        "the retried first entry must be inserted BEFORE the later entry — later \
         entries stay queued behind a storage failure instead of overtaking it"
    );
}

/// Regression: a terminal reflush that aborts before claiming any row must
/// still re-open the replacement session's once-only offline-flush gate so the
/// released backlog can be retried on the next live flush trigger.
#[tokio::test]
async fn terminal_release_rearms_replacement_flush_after_zero_deferred_claim_failure() {
    let sm_registry = Arc::new(waddle_xmpp::stream_management::InMemorySmSessionRegistry::new());
    let pending: Arc<dyn waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage> =
        Arc::new(FailFirstClaimBatchPendingStorage::new());
    let state = create_test_websocket_state_with_sm_registry_and_pending_storage(
        Arc::clone(&sm_registry),
        Arc::clone(&pending),
    )
    .await;
    let jid: FullJid = "alice@example.com/release-rearm".parse().expect("jid");
    let (old_tx, mut old_rx) = mpsc::channel::<OutboundStanza>(4);
    let old_owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), old_tx);

    let mut old_conn = WsConnState::new();
    old_conn.phase = ConnectionPhase::ready(jid.clone(), false);
    old_conn.authenticated_session = Some(create_test_session(state.as_ref(), "alice").await);
    old_conn.registry_owner = Some(old_owner);
    let enabled = handle_xmpp_frame(
        "<enable xmlns='urn:xmpp:sm:3' resume='true'/>",
        "example.com",
        state.as_ref(),
        &mut old_conn,
    )
    .await;
    let old_stream_id = Element::from_str(&enabled[0])
        .expect("enabled xml")
        .attr("id")
        .expect("stream id")
        .to_string();
    old_conn.publish_pending_sm_enable(state.as_ref());
    seed_claimed_pending_row(state.as_ref(), &jid.to_bare(), &old_stream_id, 1).await;
    let _ = old_conn.sm_state.record_outbound(
        message_frame_xml_with_id("terminal-release-rearm-copy".to_string()),
        SmEvictionPath::Batch,
    );
    old_conn.begin_terminal_sm_recovery();

    let (replacement_tx, mut replacement_rx) = mpsc::channel::<OutboundStanza>(4);
    let replacement_owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), replacement_tx);
    state
        .deps
        .protocol
        .connection_registry
        .update_presence(&jid, true, 0);
    let replacement_stream = waddle_xmpp::pending_delivery::SmSessionId::new(
        "terminal-release-rearm-stream".to_string(),
    );
    assert!(state
        .deps
        .protocol
        .connection_registry
        .set_sm_stream_id_if_owner(&jid, &replacement_owner, Some(replacement_stream.clone())));
    let replacement_entry = state
        .deps
        .protocol
        .connection_registry
        .entry_if_owner(&jid, &replacement_owner)
        .expect("replacement entry");
    assert!(
        replacement_entry.claim_offline_flush(),
        "replacement already spent its initial offline flush claim"
    );

    assert_eq!(
        cleanup_connection_shutdown(state.as_ref(), &mut old_rx, &mut old_conn, false).await,
        super::super::cleanup::ConnectionShutdownOutcome::NotPersisted
    );
    assert!(
        replacement_rx.try_recv().is_err(),
        "the forced claim-batch failure should prevent immediate redelivery"
    );

    let rows = state
        .deps
        .protocol
        .pending_delivery_storage
        .list(&jid.to_bare())
        .await
        .expect("list pending rows");
    assert_eq!(rows.len(), 1);
    assert!(
        rows[0].flushed_in_session.is_none(),
        "failed terminal reflush must leave the row retryable"
    );
    assert!(
        replacement_entry.claim_offline_flush(),
        "terminal cleanup must re-open the replacement flush gate when retryable rows remain"
    );

    let resolver = crate::pending_delivery::MamArchiveResolver {
        mam_storage: Arc::clone(&state.deps.protocol.mam_storage),
    };
    let outcome = crate::pending_delivery::flush_for_resource(
        &state.deps.protocol.pending_delivery_storage,
        &state.deps.protocol.connection_registry,
        &jid.to_bare(),
        &jid,
        crate::pending_delivery::FlushContext {
            server_domain: state.deps.auth_state.xmpp_domain.as_str(),
            sm_session: Some(&replacement_stream),
            blocking_storage: Some(&state.deps.protocol.blocking_storage),
            owner: Some(&replacement_owner),
            archive_resolver: &resolver,
        },
    )
    .await;
    assert_eq!(outcome.claimed, 1);
    assert_eq!(outcome.pushed, 1);
    assert_eq!(outcome.deferred_transient, 0);

    let delivered = replacement_rx
        .recv()
        .await
        .expect("re-armed replacement flush should deliver the row");
    match &delivered.stanza {
        Stanza::Message(message) => {
            assert_eq!(
                message.id.as_ref().map(|id| id.0.as_str()),
                Some("pd-1"),
                "the replacement should receive the released pending_delivery row after re-arming"
            );
        }
        other => panic!("expected replayed Message, got {other:?}"),
    }
}

/// When terminal cleanup releases an earlier row-backed replay entry but still
/// has later unacked traffic to promote, the released prefix must be re-driven
/// before the promoted tail reaches a live replacement so stream FIFO survives.
#[tokio::test]
async fn terminal_release_reflush_precedes_later_live_promotion_to_preserve_fifo() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/release-order".parse().expect("jid");
    let (old_tx, mut old_rx) = mpsc::channel::<OutboundStanza>(4);
    let old_owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), old_tx);

    let mut old_conn = WsConnState::new();
    old_conn.phase = ConnectionPhase::ready(jid.clone(), false);
    old_conn.authenticated_session = Some(create_test_session(state.as_ref(), "alice").await);
    old_conn.registry_owner = Some(old_owner);
    let enabled = handle_xmpp_frame(
        "<enable xmlns='urn:xmpp:sm:3' resume='true'/>",
        "example.com",
        state.as_ref(),
        &mut old_conn,
    )
    .await;
    let old_stream_id = Element::from_str(&enabled[0])
        .expect("enabled xml")
        .attr("id")
        .expect("stream id")
        .to_string();
    old_conn.publish_pending_sm_enable(state.as_ref());
    seed_claimed_pending_row(state.as_ref(), &jid.to_bare(), &old_stream_id, 1).await;
    let _ = old_conn.sm_state.record_outbound(
        message_frame_xml_with_id("terminal-release-replay-copy".to_string()),
        SmEvictionPath::Batch,
    );

    let mut later = xmpp_parsers::message::Message::new(Some(jid::Jid::from(jid.clone())));
    later.from = Some("bob@example.com/phone".parse().expect("sender jid"));
    later.id = Some(xmpp_parsers::message::Id(
        "terminal-release-later".to_string(),
    ));
    later.type_ = xmpp_parsers::message::MessageType::Chat;
    later.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "later terminal traffic".to_string(),
    );
    let _ = old_conn.sm_state.record_outbound(
        stanza_to_xml(&Stanza::Message(later)),
        SmEvictionPath::Batch,
    );
    old_conn.begin_terminal_sm_recovery();

    let (replacement_tx, mut replacement_rx) = mpsc::channel::<OutboundStanza>(8);
    let replacement_owner = register_test_connection(state.as_ref(), &jid, replacement_tx).await;
    state
        .deps
        .protocol
        .connection_registry
        .update_presence(&jid, true, 0);
    let replacement_stream = waddle_xmpp::pending_delivery::SmSessionId::new(
        "terminal-release-order-replacement-stream".to_string(),
    );
    assert!(state
        .deps
        .protocol
        .connection_registry
        .set_sm_stream_id_if_owner(&jid, &replacement_owner, Some(replacement_stream.clone())));
    let replacement_entry = state
        .deps
        .protocol
        .connection_registry
        .entry_if_owner(&jid, &replacement_owner)
        .expect("replacement entry");
    assert!(
        replacement_entry.claim_offline_flush(),
        "replacement already spent its initial offline flush claim"
    );

    assert_eq!(
        cleanup_connection_shutdown(state.as_ref(), &mut old_rx, &mut old_conn, false).await,
        super::super::cleanup::ConnectionShutdownOutcome::NotPersisted
    );

    let first = replacement_rx
        .recv()
        .await
        .expect("released row should be re-driven first");
    let second = replacement_rx
        .recv()
        .await
        .expect("later unacked traffic should still be promoted");

    let first_id = match &first.stanza {
        Stanza::Message(message) => message.id.as_ref().map(|id| id.0.as_str()),
        other => panic!("expected first replayed Message, got {other:?}"),
    };
    let second_id = match &second.stanza {
        Stanza::Message(message) => message.id.as_ref().map(|id| id.0.as_str()),
        other => panic!("expected second replayed Message, got {other:?}"),
    };
    assert_eq!(
        first_id,
        Some("pd-1"),
        "the earlier released pending row must arrive before later promoted traffic"
    );
    assert_eq!(
        second_id,
        Some("terminal-release-later"),
        "the later unacked stanza should still reach the live replacement after the released row"
    );
}

#[tokio::test]
async fn terminal_release_reflushes_receiver_tail_row_to_live_replacement() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/release-reflush-tail"
        .parse()
        .expect("jid");
    let (old_tx, mut old_rx) = mpsc::channel::<OutboundStanza>(4);
    let old_owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), old_tx.clone());

    let mut old_conn = WsConnState::new();
    old_conn.phase = ConnectionPhase::ready(jid.clone(), false);
    old_conn.authenticated_session = Some(create_test_session(state.as_ref(), "alice").await);
    old_conn.registry_owner = Some(old_owner);
    let enabled = handle_xmpp_frame(
        "<enable xmlns='urn:xmpp:sm:3' resume='true'/>",
        "example.com",
        state.as_ref(),
        &mut old_conn,
    )
    .await;
    let old_stream_id = Element::from_str(&enabled[0])
        .expect("enabled xml")
        .attr("id")
        .expect("stream id")
        .to_string();
    old_conn.publish_pending_sm_enable(state.as_ref());
    old_conn.begin_terminal_sm_recovery();

    let receipt_at = chrono::Utc::now();
    let row_id = waddle_xmpp::pending_delivery::PendingRowId::fresh();
    let mut queued = xmpp_parsers::message::Message::new(Some(jid::Jid::from(jid.to_bare())));
    queued.from = Some("bob@example.com/phone".parse().expect("sender jid"));
    queued.id = Some(xmpp_parsers::message::Id("pd-tail-1".to_string()));
    queued.type_ = xmpp_parsers::message::MessageType::Chat;
    queued.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "receiver-tail pending row".to_string(),
    );
    state
        .deps
        .protocol
        .pending_delivery_storage
        .insert(waddle_xmpp::pending_delivery::PendingRow {
            id: row_id.clone(),
            recipient: jid.to_bare(),
            original_receipt_at: receipt_at,
            payload: waddle_xmpp::pending_delivery::PendingPayload::Transient(Box::new(
                queued.clone(),
            )),
            flushed_in_session: Some(waddle_xmpp::pending_delivery::SmSessionId::new(
                old_stream_id,
            )),
            outbound_sequence: None,
        })
        .await
        .expect("seed queued pending row");
    old_tx
        .send(OutboundStanza::for_pending_flush(
            Stanza::Message(queued),
            row_id.clone(),
            receipt_at,
        ))
        .await
        .expect("queue row-backed receiver-tail stanza");

    let (replacement_tx, mut replacement_rx) = mpsc::channel::<OutboundStanza>(4);
    let replacement_owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), replacement_tx);
    state
        .deps
        .protocol
        .connection_registry
        .update_presence(&jid, true, 0);
    let replacement_stream = waddle_xmpp::pending_delivery::SmSessionId::new(
        "terminal-release-tail-replacement-stream".to_string(),
    );
    assert!(state
        .deps
        .protocol
        .connection_registry
        .set_sm_stream_id_if_owner(&jid, &replacement_owner, Some(replacement_stream.clone())));
    let replacement_entry = state
        .deps
        .protocol
        .connection_registry
        .entry_if_owner(&jid, &replacement_owner)
        .expect("replacement entry");
    assert!(
        replacement_entry.claim_offline_flush(),
        "replacement already spent its initial offline flush claim"
    );

    assert_eq!(
        cleanup_connection_shutdown(state.as_ref(), &mut old_rx, &mut old_conn, false).await,
        super::super::cleanup::ConnectionShutdownOutcome::NotPersisted
    );

    let delivered = replacement_rx
        .recv()
        .await
        .expect("terminal cleanup should re-drive the released receiver-tail row");
    match &delivered.stanza {
        Stanza::Message(message) => {
            assert_eq!(
                message.id.as_ref().map(|id| id.0.as_str()),
                Some("pd-tail-1"),
                "the live replacement should receive the released receiver-tail row"
            );
        }
        other => panic!("expected replayed Message, got {other:?}"),
    }
    let rows = state
        .deps
        .protocol
        .pending_delivery_storage
        .list(&jid.to_bare())
        .await
        .expect("list pending rows");
    assert_eq!(
        rows.len(),
        1,
        "the released row remains until the replacement acks it"
    );
    assert_eq!(
        rows[0].id, row_id,
        "cleanup must re-use the original pending row"
    );
    assert_eq!(
        rows[0].flushed_in_session.as_ref(),
        Some(&replacement_stream),
        "terminal cleanup must rebind the released receiver-tail row to the replacement stream"
    );
}

#[tokio::test]
async fn terminal_incremental_overflow_reloads_blocklist_per_item() {
    let sm_registry = Arc::new(waddle_xmpp::stream_management::InMemorySmSessionRegistry::new());
    let pending: Arc<dyn waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage> = Arc::new(
        waddle_xmpp::pending_delivery::storage::InMemoryPendingDeliveryStorage::unlimited(),
    );
    let recipient: BareJid = "alice@example.com".parse().expect("recipient");
    let blocking_impl = Arc::new(BlockOnNthListEntriesStorage {
        inner: waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new(),
        calls: std::sync::atomic::AtomicU32::new(0),
        block_on_call: 2,
        user: recipient.clone(),
        blocked_entries: vec!["bob@example.com".parse().expect("blocked jid")],
    });
    let blocking: Arc<dyn waddle_xmpp::xep::xep0191::BlockingStorage> =
        Arc::clone(&blocking_impl) as Arc<_>;
    let state = create_test_websocket_state_with_sm_registry_pending_and_blocking(
        Arc::clone(&sm_registry),
        pending,
        blocking,
    )
    .await;
    let jid: FullJid = "alice@example.com/blocklist-refresh".parse().expect("jid");

    let (tx, mut rx) = mpsc::channel::<OutboundStanza>(4);
    let owner = register_test_connection(state.as_ref(), &jid, tx.clone()).await;
    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    conn.authenticated_session = Some(create_test_session(state.as_ref(), "alice").await);
    conn.registry_owner = Some(owner);
    let _ = handle_xmpp_frame(
        "<enable xmlns='urn:xmpp:sm:3' resume='true'/>",
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;
    conn.publish_pending_sm_enable(state.as_ref());
    conn.begin_terminal_sm_recovery();

    let mut buffered = xmpp_parsers::message::Message::new(Some(jid::Jid::from(jid.to_bare())));
    buffered.from = Some("bob@example.com/phone".parse().expect("sender jid"));
    buffered.id = Some(xmpp_parsers::message::Id("accepted-backlog".to_string()));
    buffered.type_ = xmpp_parsers::message::MessageType::Chat;
    buffered.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "accepted backlog".to_string(),
    );
    waddle_xmpp::xep::xep0334::add_hint(
        &mut buffered,
        waddle_xmpp::xep::xep0334::Hint::NoPermanentStore,
    );
    tx.send(OutboundStanza::new(Stanza::Message(buffered)))
        .await
        .expect("queue accepted outbound stanza");

    assert_eq!(
        cleanup_connection_shutdown(state.as_ref(), &mut rx, &mut conn, false).await,
        super::super::cleanup::ConnectionShutdownOutcome::NotPersisted
    );

    let pending_rows = state
        .deps
        .protocol
        .pending_delivery_storage
        .list(&jid.to_bare())
        .await
        .expect("list promoted pending rows");
    assert!(
        !pending_rows.iter().any(|row| {
            matches!(
                &row.payload,
                waddle_xmpp::pending_delivery::PendingPayload::Transient(message)
                    if message.id.as_ref().is_some_and(|id| id.0 == "accepted-backlog")
            )
        }),
        "a sender blocked after prefix promotion must not be re-promoted from the receiver tail"
    );
    assert!(
        blocking_impl
            .calls
            .load(std::sync::atomic::Ordering::SeqCst)
            >= 2,
        "terminal recovery must reload the blocklist before promoting overflow items"
    );
}

#[tokio::test]
async fn sm_features_advertise_sm_namespace() {
    // Stream features after successful auth must include <sm/>.
    let features = build_stream_features_xml(true);
    let el = Element::from_str(&features).expect("features xml");
    assert!(
        el.children()
            .any(|child| child.name() == "sm" && child.ns() == SM_NS),
        "post-auth features must advertise urn:xmpp:sm:3"
    );
}

#[test]
fn is_countable_stanza_matches_element_name_not_prefix() {
    // Real stanzas that must count toward SM handled/sent counters.
    assert!(is_countable_stanza(
        "<iq xmlns='jabber:client' type='get' id='1'/>"
    ));
    assert!(is_countable_stanza("<message xmlns='jabber:client'/>"));
    assert!(is_countable_stanza("<presence xmlns='jabber:client'/>"));
    assert!(is_countable_stanza(
        "<jc:message xmlns:jc='jabber:client'/>"
    ));
    assert!(is_countable_stanza(
        "<jc:presence xmlns:jc='jabber:client'/>"
    ));
    assert!(is_countable_stanza(
        "<jc:iq xmlns:jc='jabber:client' id='1'/>"
    ));
    // Leading whitespace is tolerated (matches the pre-existing
    // trim behaviour — frames are always serialized with a
    // namespace by minidom, so callers never produce bare `<iq/>`).
    assert!(is_countable_stanza("  <iq xmlns='jabber:client' id='1'/>"));

    // SM control nonzas and stream-level frames must NOT count.
    assert!(!is_countable_stanza("<r xmlns='urn:xmpp:sm:3'/>"));
    assert!(!is_countable_stanza("<a xmlns='urn:xmpp:sm:3' h='1'/>"));
    assert!(!is_countable_stanza(
        "<enable xmlns='urn:xmpp:sm:3' resume='1'/>"
    ));
    assert!(!is_countable_stanza(
        "<resumed xmlns='urn:xmpp:sm:3' previd='x' h='0'/>"
    ));

    // Substring prefix collisions that the old `starts_with`
    // implementation would have accepted. These are all non-standard
    // today but the element-name match is how we stay safe if any
    // future XEP introduces similarly-named nonzas.
    assert!(!is_countable_stanza("<messages xmlns='urn:example'/>"));
    assert!(!is_countable_stanza("<presences xmlns='urn:example'/>"));
    assert!(!is_countable_stanza("<iqsomething/>"));
    assert!(!is_countable_stanza(
        "<jc:messages xmlns:jc='urn:example'/>"
    ));

    // Malformed XML just doesn't count — no panic, no false positive.
    assert!(!is_countable_stanza("not-xml-at-all"));
    assert!(!is_countable_stanza(""));
}

#[tokio::test]
async fn handle_xmpp_frame_drops_oversized_sm_nonza_before_parse() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let huge = element_to_xml(
        Element::builder("r", SM_NS)
            .attr(
                minidom::rxml::xml_ncname!("note").to_owned(),
                "a".repeat(waddle_xmpp::protocol::frame::MAX_FRAME_SIZE),
            )
            .build(),
    );

    let responses = handle_xmpp_frame(&huge, "example.com", state.as_ref(), &mut conn).await;

    assert!(responses.is_empty());
}

#[tokio::test]
async fn sm_enable_requires_resource_binding() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    // Without resource_bound, enable must fail.
    let frame = "<enable xmlns='urn:xmpp:sm:3' resume='true'/>";
    let responses = handle_xmpp_frame(frame, "example.com", state.as_ref(), &mut conn).await;
    assert_eq!(responses.len(), 1);
    let el = Element::from_str(&responses[0]).expect("xml");
    assert_eq!(el.name(), "failed");
    assert!(!conn.sm_state.enabled);
}

#[tokio::test]
async fn sm_enable_claim_timeout_returns_failure_without_enabling_state() {
    let registry = Arc::new(
        waddle_xmpp::stream_management::InMemorySmSessionRegistry::new().with_claim_store(
            Arc::new(HangingEnsureClaimStore {
                inner: waddle_xmpp::ownership::InProcessClaimStore::new(),
            }),
            waddle_xmpp::ownership::SharedNodeIdentity::new(
                waddle_xmpp::ownership::NodeIdentity::new("sm-node", "incarnation"),
            ),
        ),
    );
    let state = create_test_websocket_state_with_sm_registry(registry).await;
    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready("alice@example.com/web".parse().expect("bound jid"), false);

    let responses = handle_xmpp_frame(
        "<enable xmlns='urn:xmpp:sm:3' resume='true'/>",
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1);
    let failed = Element::from_str(&responses[0]).expect("failed xml");
    assert_eq!(failed.name(), "failed");
    assert!(failed
        .get_child("resource-constraint", "urn:ietf:params:xml:ns:xmpp-stanzas")
        .is_some());
    assert!(!conn.sm_state.enabled);
    assert!(conn.sm_state.stream_id.is_none());
}

#[tokio::test]
async fn sm_resume_requires_authentication() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let resume_frame = resume_frame_xml("stream-xyz", 0);
    let responses =
        handle_xmpp_frame(&resume_frame, "example.com", state.as_ref(), &mut conn).await;

    assert_eq!(responses.len(), 1);
    let el = Element::from_str(&responses[0]).expect("xml");
    assert_eq!(el.name(), "failed");
    assert!(el
        .get_child("unexpected-request", "urn:ietf:params:xml:ns:xmpp-stanzas")
        .is_some());
    assert!(matches!(conn.phase, ConnectionPhase::Unauthenticated));
}

#[tokio::test]
async fn sm_resume_is_rejected_during_scram_and_scram_can_still_complete() {
    let state = create_test_websocket_state().await;
    let domain = state.deps.auth_state.xmpp_domain.clone();
    let password = "correct horse battery staple";
    let client_nonce = "fyko+d2lbbFgONRv9qkxdawL";
    register_test_native_user(state.as_ref(), "alice", password).await;

    let auth_frame = element_to_xml(
        Element::builder("auth", waddle_xmpp::ns::SASL)
            .attr(
                minidom::rxml::xml_ncname!("mechanism").to_owned(),
                "SCRAM-SHA-256",
            )
            .append(BASE64_STANDARD.encode(format!("n,,n=alice,r={client_nonce}")))
            .build(),
    );
    let mut conn = WsConnState::new();

    let auth_responses = handle_xmpp_frame(&auth_frame, &domain, state.as_ref(), &mut conn).await;
    let challenge = Element::from_str(&auth_responses[0]).expect("challenge xml");
    let challenge_b64 = challenge.text();
    assert_eq!(conn.phase.scram_pending_username(), Some("alice"));

    let resume_frame = resume_frame_xml("stream-xyz", 0);
    let resume_responses =
        handle_xmpp_frame(&resume_frame, &domain, state.as_ref(), &mut conn).await;
    assert_eq!(resume_responses.len(), 1);
    let failed = Element::from_str(&resume_responses[0]).expect("failed xml");
    assert_eq!(failed.name(), "failed");
    assert!(failed
        .get_child("unexpected-request", "urn:ietf:params:xml:ns:xmpp-stanzas")
        .is_some());
    assert_eq!(conn.phase.scram_pending_username(), Some("alice"));

    let response_frame = element_to_xml(
        Element::builder("response", waddle_xmpp::ns::SASL)
            .append(BASE64_STANDARD.encode(scram_client_final_from_challenge(
                "alice",
                password,
                client_nonce,
                &challenge_b64,
            )))
            .build(),
    );
    let response_responses =
        handle_xmpp_frame(&response_frame, &domain, state.as_ref(), &mut conn).await;

    assert_eq!(response_responses.len(), 1);
    let success = Element::from_str(&response_responses[0]).expect("success xml");
    assert_eq!(success.name(), "success");
    assert!(matches!(conn.phase, ConnectionPhase::Authenticated { .. }));
    assert!(conn.phase.is_authenticated());
}

#[tokio::test]
async fn sm_resume_is_allowed_after_auth_before_bind() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "alice").await;
    let payload = BASE64_STANDARD.encode(format!("n,,\x01auth=Bearer {}\x01\x01", session.id));
    let auth_frame = element_to_xml(
        Element::builder("auth", waddle_xmpp::ns::SASL)
            .attr(
                minidom::rxml::xml_ncname!("mechanism").to_owned(),
                "OAUTHBEARER",
            )
            .append(payload)
            .build(),
    );
    let mut conn = WsConnState::new();

    let auth_responses =
        handle_xmpp_frame(&auth_frame, "example.com", state.as_ref(), &mut conn).await;
    assert_eq!(auth_responses, vec![sasl_success_xml()]);

    let resume_frame = resume_frame_xml("stream-xyz", 0);
    let responses =
        handle_xmpp_frame(&resume_frame, "example.com", state.as_ref(), &mut conn).await;

    assert_eq!(responses.len(), 1);
    let el = Element::from_str(&responses[0]).expect("xml");
    assert_eq!(el.name(), "failed");
    assert!(el
        .get_child("item-not-found", "urn:ietf:params:xml:ns:xmpp-stanzas")
        .is_some());
    assert!(matches!(conn.phase, ConnectionPhase::Authenticated { .. }));
    assert!(!conn.phase.is_resumed());
}

#[tokio::test]
async fn sm_resume_rejects_when_replay_window_has_gap() {
    use waddle_xmpp::stream_management::DetachedSession;

    let state = create_test_websocket_state().await;
    let domain = state.deps.auth_state.xmpp_domain.clone();
    let session = create_test_session(state.as_ref(), "alice").await;
    let payload = BASE64_STANDARD.encode(format!("n,,\x01auth=Bearer {}\x01\x01", session.id));
    let auth_frame = element_to_xml(
        Element::builder("auth", waddle_xmpp::ns::SASL)
            .attr(
                minidom::rxml::xml_ncname!("mechanism").to_owned(),
                "OAUTHBEARER",
            )
            .append(payload)
            .build(),
    );
    let mut conn = WsConnState::new();
    let auth_responses = handle_xmpp_frame(&auth_frame, &domain, state.as_ref(), &mut conn).await;
    assert_eq!(auth_responses, vec![sasl_success_xml()]);

    let mut detached = DetachedSession {
        stream_id: "stream-gap".to_string(),
        user_id: format!("alice@{domain}"),
        jid: format!("alice@{domain}/web").parse().expect("jid"),
        inbound_count: 5,
        shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
        outbound_count: 0,
        last_acked: 0,
        replay_gap_through: None,
        unacked_stanzas: Vec::new(),
        max_resume_time: Some(300),
        detached_at: std::time::Instant::now(),
        carbons_enabled: false,
        roster_interested: false,
        blocklist_interested: false,
        presence_available: false,
        presence_show: None,
        presence_status: None,
        presence_priority: 0,
        presence_payloads: Vec::new(),
        pending_subscribes_flushed: false,
    };
    for sequence in 1..=(waddle_xmpp::stream_management::DEFAULT_MAX_UNACKED_QUEUE_SIZE as u32 + 1)
    {
        detached.record_detached_outbound_at(
            sequence,
            message_frame_xml_with_id(format!("m{sequence}")),
            chrono::Utc::now(),
        );
    }
    assert_eq!(detached.replay_gap_through, Some(1));
    let _detached_session = store_resumable_test_session(state.as_ref(), detached.clone()).await;

    let resume_frame = resume_frame_xml("stream-gap", 0);
    let responses = handle_xmpp_frame(&resume_frame, &domain, state.as_ref(), &mut conn).await;

    assert_eq!(responses.len(), 1);
    let el = Element::from_str(&responses[0]).expect("xml");
    assert_eq!(el.name(), "failed");
    assert_eq!(el.attr("h"), Some("5"));
    assert!(el
        .get_child("resource-constraint", "urn:ietf:params:xml:ns:xmpp-stanzas")
        .is_some());
    assert!(matches!(conn.phase, ConnectionPhase::Authenticated { .. }));
    assert!(!conn.phase.is_resumed());

    let stored = state
        .deps
        .protocol
        .sm_session_registry
        .take_session("stream-gap")
        .await
        .expect("take")
        .expect("detached session should remain for expiry/fallback handling");
    assert_eq!(stored.jid, detached.jid);
}

#[tokio::test]
async fn sm_resume_rejects_authenticated_identity_mismatch_and_preserves_session() {
    use waddle_xmpp::stream_management::DetachedSession;

    let state = create_test_websocket_state().await;
    let domain = state.deps.auth_state.xmpp_domain.clone();
    let session = create_test_session(state.as_ref(), "bob").await;
    let payload = BASE64_STANDARD.encode(format!("n,,\x01auth=Bearer {}\x01\x01", session.id));
    let auth_frame = element_to_xml(
        Element::builder("auth", waddle_xmpp::ns::SASL)
            .attr(
                minidom::rxml::xml_ncname!("mechanism").to_owned(),
                "OAUTHBEARER",
            )
            .append(payload)
            .build(),
    );
    let mut conn = WsConnState::new();
    let auth_responses = handle_xmpp_frame(&auth_frame, &domain, state.as_ref(), &mut conn).await;
    assert_eq!(auth_responses, vec![sasl_success_xml()]);
    assert!(matches!(conn.phase, ConnectionPhase::Authenticated { .. }));

    let detached = DetachedSession {
        stream_id: "stream-auth-mismatch".to_string(),
        user_id: format!("alice@{domain}"),
        jid: format!("alice@{domain}/web").parse().expect("jid"),
        inbound_count: 0,
        shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
        outbound_count: 0,
        last_acked: 0,
        replay_gap_through: None,
        unacked_stanzas: Vec::new(),
        max_resume_time: Some(300),
        detached_at: std::time::Instant::now(),
        carbons_enabled: false,
        roster_interested: false,
        blocklist_interested: false,
        presence_available: false,
        presence_show: None,
        presence_status: None,
        presence_priority: 0,
        presence_payloads: Vec::new(),
        pending_subscribes_flushed: false,
    };
    let _detached_session = store_resumable_test_session(state.as_ref(), detached.clone()).await;

    let resume_frame = resume_frame_xml("stream-auth-mismatch", 0);
    let responses = handle_xmpp_frame(&resume_frame, &domain, state.as_ref(), &mut conn).await;

    assert_eq!(responses.len(), 1);
    let el = Element::from_str(&responses[0]).expect("xml");
    assert_eq!(el.name(), "failed");
    assert!(el
        .get_child("not-authorized", "urn:ietf:params:xml:ns:xmpp-stanzas")
        .is_some());
    assert!(matches!(conn.phase, ConnectionPhase::Authenticated { .. }));
    assert_eq!(
        conn.phase.authenticated_bare_jid().map(ToString::to_string),
        Some(format!("bob@{domain}"))
    );

    let stored = state
        .deps
        .protocol
        .sm_session_registry
        .take_session("stream-auth-mismatch")
        .await
        .expect("take")
        .expect("detached session should remain");
    assert_eq!(stored.jid, detached.jid);
}

#[tokio::test]
async fn sm_resume_final_principal_recheck_rejects_without_committing_staged_state() {
    use waddle_xmpp::stream_management::DetachedSession;

    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "alice").await;
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let stream_id = "stream-final-principal-recheck".to_string();
    store_resumable_detached_session(
        state.as_ref(),
        &session,
        DetachedSession {
            stream_id: stream_id.clone(),
            user_id: session.user_jid.clone(),
            jid: jid.clone(),
            inbound_count: 4,
            shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
            outbound_count: 1,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: vec![waddle_xmpp::stream_management::DetachedUnackedStanza {
                sequence: 1,
                stanza_xml: "<message xmlns='jabber:client' id='queued'/>".to_string(),
                original_receipt_at: chrono::Utc::now(),
            }],
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: true,
            roster_interested: true,
            blocklist_interested: true,
            presence_available: true,
            presence_show: Some(xmpp_parsers::presence::Show::Away),
            presence_status: Some("detached".to_string()),
            presence_priority: 3,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: true,
        },
    )
    .await;

    let reached = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::authenticated(&jid);
    conn.pre_final_principal_recheck_test_hook = Some((reached.clone(), release.clone()));
    let resume_state = state.clone();
    let resume = tokio::spawn(async move {
        let responses = handle_xmpp_frame(
            &resume_frame_xml(&stream_id, 0),
            "example.com",
            resume_state.as_ref(),
            &mut conn,
        )
        .await;
        (responses, conn)
    });

    reached.notified().await;
    state
        .deps
        .auth_state
        .session_manager
        .delete_session(&session.id)
        .await
        .expect("delete durable authenticated session after first resolution");
    release.notify_one();
    let (responses, conn) = resume.await.expect("resume task");

    assert_eq!(responses.len(), 1);
    let failed = Element::from_str(&responses[0]).expect("failed resume XML");
    assert_eq!(failed.name(), "failed");
    assert!(failed
        .get_child("not-authorized", "urn:ietf:params:xml:ns:xmpp-stanzas")
        .is_some());
    assert!(
        !conn.sm_state.enabled,
        "rejected staging installs no SM state"
    );
    assert_eq!(
        conn.sm_state.queue_len(),
        0,
        "rejected staging installs no queue"
    );
    assert!(matches!(conn.phase, ConnectionPhase::Authenticated { .. }));
    assert!(
        conn.pending_resume_claim.is_none(),
        "claim guard was released"
    );
    assert!(
        state
            .deps
            .protocol
            .connection_registry
            .get_entry(&jid)
            .is_none(),
        "rejected staging performs no registration"
    );
    assert!(
        state
            .deps
            .protocol
            .sm_session_registry
            .peek_session("stream-final-principal-recheck")
            .await
            .expect("peek retained snapshot")
            .is_some(),
        "a final-recheck rejection retains the detached snapshot"
    );
}

#[tokio::test]
async fn dropping_resume_after_claim_returns_the_snapshot_via_the_claim_guard() {
    use waddle_xmpp::stream_management::DetachedSession;

    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "alice").await;
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let stream_id = "stream-cancelled-after-claim".to_string();
    store_resumable_detached_session(
        state.as_ref(),
        &session,
        DetachedSession {
            stream_id: stream_id.clone(),
            user_id: session.user_jid.clone(),
            jid: jid.clone(),
            inbound_count: 0,
            shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        },
    )
    .await;

    let reached = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::authenticated(&jid);
    conn.pre_final_principal_recheck_test_hook = Some((reached.clone(), release));
    let resume_state = state.clone();
    let resume = tokio::spawn(async move {
        let _ = handle_xmpp_frame(
            &resume_frame_xml(&stream_id, 0),
            "example.com",
            resume_state.as_ref(),
            &mut conn,
        )
        .await;
    });

    reached.notified().await;
    resume.abort();
    let _ = resume.await;
    assert!(
        state
            .deps
            .protocol
            .sm_session_registry
            .claim_session("stream-cancelled-after-claim")
            .await
            .expect("claim returned snapshot")
            .is_some(),
        "dropping the resume future must synchronously return its frozen claim"
    );
}

#[tokio::test]
async fn sm_resume_legacy_snapshot_without_a_principal_is_not_authorized() {
    use waddle_xmpp::stream_management::DetachedSession;

    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id: "stream-legacy-null-context".to_string(),
            user_id: "alice@example.com".to_string(),
            jid: jid.clone(),
            inbound_count: 0,
            shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        })
        .await
        .expect("store legacy snapshot without a principal");

    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::authenticated(&jid);
    let responses = handle_xmpp_frame(
        &resume_frame_xml("stream-legacy-null-context", 0),
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    let failed = Element::from_str(&responses[0]).expect("failed resume XML");
    assert!(failed
        .get_child("not-authorized", "urn:ietf:params:xml:ns:xmpp-stanzas")
        .is_some());
    assert!(matches!(conn.phase, ConnectionPhase::Authenticated { .. }));
}

#[tokio::test]
async fn sm_resume_storage_error_is_reported_as_internal_server_error() {
    use waddle_xmpp::stream_management::DetachedSession;

    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "alice").await;
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    store_resumable_detached_session(
        state.as_ref(),
        &session,
        DetachedSession {
            stream_id: "stream-storage-error".to_string(),
            user_id: session.user_jid.clone(),
            jid: jid.clone(),
            inbound_count: 0,
            shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        },
    )
    .await;

    state.deps.auth_state.session_manager.actor_ref().kill();
    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::authenticated(&jid);
    let responses = handle_xmpp_frame(
        &resume_frame_xml("stream-storage-error", 0),
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    let failed = Element::from_str(&responses[0]).expect("failed resume XML");
    assert!(failed
        .get_child(
            "internal-server-error",
            "urn:ietf:params:xml:ns:xmpp-stanzas",
        )
        .is_some());
    assert!(matches!(conn.phase, ConnectionPhase::Authenticated { .. }));
    assert!(!conn.sm_state.enabled);
}

#[tokio::test]
async fn sm_resume_matching_authenticated_identity_restores_durable_principal_session() {
    use waddle_xmpp::stream_management::DetachedSession;

    let state = create_test_websocket_state().await;
    let domain = state.deps.auth_state.xmpp_domain.clone();
    let session = create_test_session(state.as_ref(), "bob").await;
    let payload = BASE64_STANDARD.encode(format!("n,,\x01auth=Bearer {}\x01\x01", session.id));
    let auth_frame = element_to_xml(
        Element::builder("auth", waddle_xmpp::ns::SASL)
            .attr(
                minidom::rxml::xml_ncname!("mechanism").to_owned(),
                "OAUTHBEARER",
            )
            .append(payload)
            .build(),
    );
    let mut conn = WsConnState::new();
    let auth_responses = handle_xmpp_frame(&auth_frame, &domain, state.as_ref(), &mut conn).await;
    assert_eq!(auth_responses, vec![sasl_success_xml()]);
    assert!(matches!(conn.phase, ConnectionPhase::Authenticated { .. }));

    let detached_jid: FullJid = format!("bob@{domain}/web").parse().expect("jid");
    let _detached_session = store_resumable_test_session(
        state.as_ref(),
        DetachedSession {
            stream_id: "stream-auth-match".to_string(),
            user_id: format!("bob@{domain}"),
            jid: detached_jid.clone(),
            inbound_count: 2,
            shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
            outbound_count: 3,
            last_acked: 3,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: true,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        },
    )
    .await;

    let resume_frame = resume_frame_xml("stream-auth-match", 3);
    let responses = handle_xmpp_frame(&resume_frame, &domain, state.as_ref(), &mut conn).await;

    assert_eq!(responses.len(), 1);
    let resumed = Element::from_str(&responses[0]).expect("xml");
    assert_eq!(resumed.name(), "resumed");
    assert_eq!(conn.phase.bound_jid(), Some(&detached_jid));
    assert!(conn.phase.is_ready());
    assert!(conn.phase.is_resumed());
    assert!(matches!(
        &conn.phase,
        ConnectionPhase::Ready {
            full_jid,
            resumed: true,
            ..
        } if full_jid == &detached_jid
    ));
    assert_eq!(
        conn.authenticated_session
            .as_ref()
            .map(|saved| saved.user_jid.as_str()),
        Some(session.user_jid.as_str())
    );
}

#[tokio::test]
async fn sm_resume_matching_authenticated_identity_restores_detached_principal_session() {
    use waddle_xmpp::stream_management::DetachedSession;

    let state = create_test_websocket_state().await;
    let domain = state.deps.auth_state.xmpp_domain.clone();
    let fresh_session = create_test_session(state.as_ref(), "bob").await;
    let payload =
        BASE64_STANDARD.encode(format!("n,,\x01auth=Bearer {}\x01\x01", fresh_session.id));
    let auth_frame = element_to_xml(
        Element::builder("auth", waddle_xmpp::ns::SASL)
            .attr(
                minidom::rxml::xml_ncname!("mechanism").to_owned(),
                "OAUTHBEARER",
            )
            .append(payload)
            .build(),
    );
    let mut conn = WsConnState::new();
    let auth_responses = handle_xmpp_frame(&auth_frame, &domain, state.as_ref(), &mut conn).await;
    assert_eq!(auth_responses, vec![sasl_success_xml()]);

    let stream_id = "stream-auth-match-with-sidecar";
    let detached_jid: FullJid = format!("bob@{domain}/web").parse().expect("jid");
    let resumed_session = create_test_session(state.as_ref(), "bob").await;
    state
        .deps
        .protocol
        .sm_session_registry
        .store_session_with_principal(
            DetachedSession {
                stream_id: stream_id.to_string(),
                user_id: format!("bob@{domain}"),
                jid: detached_jid.clone(),
                inbound_count: 0,
                shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
                outbound_count: 0,
                last_acked: 0,
                replay_gap_through: None,
                unacked_stanzas: Vec::new(),
                max_resume_time: Some(300),
                detached_at: std::time::Instant::now(),
                carbons_enabled: false,
                roster_interested: false,
                blocklist_interested: false,
                presence_available: false,
                presence_show: None,
                presence_status: None,
                presence_priority: 0,
                presence_payloads: Vec::new(),
                pending_subscribes_flushed: false,
            },
            resumed_session
                .authenticated_principal_ref()
                .expect("auth context"),
        )
        .await
        .expect("store");

    let resume_frame = resume_frame_xml(stream_id, 0);
    let responses = handle_xmpp_frame(&resume_frame, &domain, state.as_ref(), &mut conn).await;

    assert_eq!(responses.len(), 1);
    let resumed = Element::from_str(&responses[0]).expect("xml");
    assert_eq!(resumed.name(), "resumed");
    assert!(matches!(
        &conn.phase,
        ConnectionPhase::Ready {
            full_jid,
            resumed: true,
            ..
        } if full_jid == &detached_jid
    ));
    assert_eq!(
        conn.authenticated_session
            .as_ref()
            .map(|saved| saved.id.as_str()),
        Some(resumed_session.id.as_str())
    );
    assert_ne!(
        conn.authenticated_session
            .as_ref()
            .map(|saved| saved.id.as_str()),
        Some(fresh_session.id.as_str())
    );
}

#[tokio::test]
async fn sm_resume_rejects_ready_phase() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    conn.phase = ConnectionPhase::ready(jid, false);

    let resume_frame = resume_frame_xml("stream-xyz", 0);
    let responses =
        handle_xmpp_frame(&resume_frame, "example.com", state.as_ref(), &mut conn).await;

    assert_eq!(responses.len(), 1);
    let el = Element::from_str(&responses[0]).expect("xml");
    assert_eq!(el.name(), "failed");
    assert!(el
        .get_child("unexpected-request", "urn:ietf:params:xml:ns:xmpp-stanzas")
        .is_some());
    assert!(matches!(conn.phase, ConnectionPhase::Ready { .. }));
}

#[tokio::test]
async fn sm_enable_after_bind_returns_enabled_and_tracks_counters() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    register_sm_publish_owner(state.as_ref(), &mut conn, &jid);

    let responses = handle_xmpp_frame(
        "<enable xmlns='urn:xmpp:sm:3' resume='true'/>",
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;
    assert_eq!(responses.len(), 1);
    let el = Element::from_str(&responses[0]).expect("xml");
    assert_eq!(el.name(), "enabled");
    assert_eq!(el.attr("resume"), Some("true"));
    assert!(el.attr("id").filter(|s| !s.is_empty()).is_some());
    assert!(
        !conn.sm_state.enabled,
        "SM must remain unpublished until the <enabled/> write succeeds"
    );
    conn.publish_pending_sm_enable(state.as_ref());
    assert!(conn.sm_state.enabled);
    assert!(conn.sm_state.is_resumable());

    // An ack request bumps no counters but produces <a h=inbound_count/>.
    let ack_responses = handle_xmpp_frame(
        "<r xmlns='urn:xmpp:sm:3'/>",
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;
    assert_eq!(ack_responses.len(), 1);
    let ack_el = Element::from_str(&ack_responses[0]).expect("xml");
    assert_eq!(ack_el.name(), "a");
    assert_eq!(ack_el.attr("h"), Some("0"));

    // A countable inbound stanza bumps the inbound counter.
    let _ = handle_xmpp_frame(
        "<presence xmlns='jabber:client'/>",
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;
    assert_eq!(conn.sm_state.get_inbound_count(), 1);

    // Subsequent <r/> should now report h=1.
    let ack2 = handle_xmpp_frame(
        "<r xmlns='urn:xmpp:sm:3'/>",
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;
    let ack2_el = Element::from_str(&ack2[0]).expect("xml");
    assert_eq!(ack2_el.attr("h"), Some("1"));
}

#[tokio::test]
async fn pipelined_sm_enable_cannot_replace_the_unpublished_commit() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/pipelined-enable".parse().expect("jid");
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    register_sm_publish_owner(state.as_ref(), &mut conn, &jid);

    let first = handle_xmpp_frame(
        "<enable xmlns='urn:xmpp:sm:3' resume='true'/>",
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;
    let first_enabled = Element::from_str(&first[0]).expect("first enabled xml");
    let first_stream_id = first_enabled
        .attr("id")
        .expect("first stream id")
        .to_string();
    assert!(conn.pending_sm_enable_commit.is_some());
    assert!(!conn.sm_state.enabled);

    let second = handle_xmpp_frame(
        "<enable xmlns='urn:xmpp:sm:3' resume='true'/>",
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;
    let failed = Element::from_str(&second[0]).expect("second failed xml");
    assert_eq!(failed.name(), "failed");
    assert!(failed
        .get_child("unexpected-request", "urn:ietf:params:xml:ns:xmpp-stanzas")
        .is_some());

    conn.publish_pending_sm_enable(state.as_ref());
    assert!(conn.sm_state.enabled);
    assert_eq!(
        conn.sm_state.stream_id.as_deref(),
        Some(first_stream_id.as_str())
    );
}

#[tokio::test]
async fn resumable_enable_cancelled_before_write_never_publishes_and_releases_exact_claim() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/cancelled-enable".parse().expect("jid");
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    register_sm_publish_owner(state.as_ref(), &mut conn, &jid);

    let responses = handle_xmpp_frame(
        "<enable xmlns='urn:xmpp:sm:3' resume='true'/>",
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;
    let enabled = Element::from_str(&responses[0]).expect("enabled xml");
    let stream_id = enabled.attr("id").expect("stream id").to_string();
    assert!(!conn.sm_state.enabled);
    assert!(conn.pending_sm_enable_commit.is_some());

    drop(conn);

    let registry = &state.deps.protocol.sm_session_registry;
    assert_eq!(registry.pending_claim_release_count(), 1);
    assert_eq!(registry.retry_pending_claim_releases(1).await, 1);
    assert!(
        !registry
            .locally_owned_claim_ids()
            .expect("local ownership inventory")
            .contains(&stream_id),
        "an unpublished previd must not retain a resumable claim"
    );
}

#[tokio::test]
async fn replaced_connection_commits_written_enable_without_publishing_stale_alias() {
    let state = create_test_websocket_state().await;
    let mut old_conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/replaced-enable".parse().expect("jid");
    old_conn.phase = ConnectionPhase::ready(jid.clone(), false);
    register_sm_publish_owner(state.as_ref(), &mut old_conn, &jid);

    let responses = handle_xmpp_frame(
        "<enable xmlns='urn:xmpp:sm:3' resume='true'/>",
        "example.com",
        state.as_ref(),
        &mut old_conn,
    )
    .await;
    let enabled = Element::from_str(&responses[0]).expect("enabled xml");
    let stream_id = waddle_xmpp::pending_delivery::SmSessionId::new(
        enabled.attr("id").expect("stream id").to_string(),
    );

    let (replacement_tx, _replacement_rx) = mpsc::channel(1);
    let _replacement_owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), replacement_tx);

    old_conn.publish_pending_sm_enable(state.as_ref());

    assert!(
        old_conn.sm_state.enabled,
        "a successfully written <enabled/> commits local XEP-0198 state"
    );
    assert!(old_conn.pending_sm_enable_commit.is_none());
    assert!(
        state
            .deps
            .protocol
            .connection_registry
            .get_entry(&jid)
            .expect("replacement entry")
            .sm_stream_id()
            .is_none(),
        "stale publication must not stamp the replacement entry"
    );
    assert!(
        state
            .deps
            .protocol
            .connection_registry
            .sm_stream_owner(&stream_id)
            .is_none(),
        "stale publication must not create a reverse-index alias"
    );
    let registry = &state.deps.protocol.sm_session_registry;
    assert_eq!(registry.pending_claim_release_count(), 0);

    let (_outbound_tx, mut outbound_rx) = mpsc::channel(1);
    assert_eq!(
        cleanup_connection_shutdown(state.as_ref(), &mut outbound_rx, &mut old_conn, false).await,
        super::super::cleanup::ConnectionShutdownOutcome::NotPersisted
    );
    assert_eq!(registry.pending_claim_release_count(), 1);
    assert_eq!(registry.retry_pending_claim_releases(1).await, 1);
}

#[tokio::test]
async fn replacement_after_enable_publication_terminalizes_only_the_old_stream_claim() {
    let state = create_test_websocket_state().await;
    let mut old_conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/replaced-after-enable"
        .parse()
        .expect("jid");
    old_conn.phase = ConnectionPhase::ready(jid.clone(), false);
    register_sm_publish_owner(state.as_ref(), &mut old_conn, &jid);

    let responses = handle_xmpp_frame(
        "<enable xmlns='urn:xmpp:sm:3' resume='true'/>",
        "example.com",
        state.as_ref(),
        &mut old_conn,
    )
    .await;
    let enabled = Element::from_str(&responses[0]).expect("enabled xml");
    let old_stream_id = enabled.attr("id").expect("stream id").to_string();
    old_conn.publish_pending_sm_enable(state.as_ref());
    assert!(old_conn.sm_state.enabled);

    let (replacement_tx, _replacement_rx) = mpsc::channel(1);
    let replacement_owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), replacement_tx);
    let replacement_stream =
        waddle_xmpp::pending_delivery::SmSessionId::new("replacement-owned-stream".to_string());
    assert!(state
        .deps
        .protocol
        .connection_registry
        .set_sm_stream_id_if_owner(&jid, &replacement_owner, Some(replacement_stream.clone()),));

    let (_outbound_tx, mut outbound_rx) = mpsc::channel(1);
    assert_eq!(
        cleanup_connection_shutdown(state.as_ref(), &mut outbound_rx, &mut old_conn, false).await,
        super::super::cleanup::ConnectionShutdownOutcome::NotPersisted
    );

    assert_eq!(
        state
            .deps
            .protocol
            .connection_registry
            .get_entry(&jid)
            .expect("replacement entry")
            .sm_stream_id(),
        Some(replacement_stream),
        "old-stream cleanup must not alter the replacement entry"
    );
    let registry = &state.deps.protocol.sm_session_registry;
    assert_eq!(registry.pending_claim_release_count(), 1);
    assert!(registry
        .locally_owned_claim_ids()
        .expect("ownership inventory")
        .contains(&old_stream_id));
    assert_eq!(registry.retry_pending_claim_releases(1).await, 1);
}

#[tokio::test]
async fn non_resumable_sm_enable_does_not_create_cluster_claim() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/non-resumable".parse().expect("jid");
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    register_sm_publish_owner(state.as_ref(), &mut conn, &jid);

    let responses = handle_xmpp_frame(
        "<enable xmlns='urn:xmpp:sm:3' resume='false'/>",
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;
    assert_eq!(responses.len(), 1);
    let enabled = Element::from_str(&responses[0]).expect("enabled xml");
    assert_eq!(enabled.name(), "enabled");
    assert_eq!(enabled.attr("resume"), None);
    let stream_id = enabled.attr("id").expect("SM id");
    conn.publish_pending_sm_enable(state.as_ref());
    assert!(conn.sm_state.enabled);
    assert!(!conn.sm_state.is_resumable());
    assert!(
        !state
            .deps
            .protocol
            .sm_session_registry
            .locally_owned_claim_ids()
            .expect("local claim snapshot")
            .contains(&stream_id.to_string()),
        "non-resumable SM must not retain a clustered ownership claim"
    );
}

/// Enable SM on a fresh ready connection and return the negotiated
/// stream id. Shared setup for the live `<a h='N'/>` validation tests
/// (issue #1099).
async fn enable_sm_for_live_ack_tests(
    state: &super::super::state::WebSocketState,
    conn: &mut WsConnState,
    jid: &FullJid,
) -> String {
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    register_sm_publish_owner(state, conn, jid);
    let responses = handle_xmpp_frame(
        "<enable xmlns='urn:xmpp:sm:3' resume='true'/>",
        "example.com",
        state,
        conn,
    )
    .await;
    let enabled = Element::from_str(&responses[0]).expect("enabled xml");
    assert_eq!(enabled.name(), "enabled");
    conn.publish_pending_sm_enable(state);
    enabled.attr("id").expect("stream id").to_string()
}

/// Seed a pending_delivery row claimed by `stream_id` whose flush
/// stanza was recorded at `outbound_sequence`, mirroring the Q7b
/// SM-ack lifecycle rows that `<a h='N'/>` range-deletes.
async fn seed_claimed_pending_row(
    state: &super::super::state::WebSocketState,
    recipient: &BareJid,
    stream_id: &str,
    outbound_sequence: u32,
) {
    state
        .deps
        .protocol
        .pending_delivery_storage
        .insert(waddle_xmpp::pending_delivery::PendingRow {
            id: waddle_xmpp::pending_delivery::PendingRowId::fresh(),
            recipient: recipient.clone(),
            original_receipt_at: chrono::Utc::now(),
            payload: waddle_xmpp::pending_delivery::PendingPayload::Transient(Box::new({
                let mut m =
                    xmpp_parsers::message::Message::new(Some(jid::Jid::from(recipient.clone())));
                m.id = Some(xmpp_parsers::message::Id("pd-1".to_string()));
                m
            })),
            flushed_in_session: Some(waddle_xmpp::pending_delivery::SmSessionId::new(
                stream_id.to_string(),
            )),
            outbound_sequence: Some(outbound_sequence),
        })
        .await
        .expect("seed claimed pending_delivery row");
}

#[tokio::test]
async fn sm_live_ack_with_impossible_handled_count_closes_stream_without_purging() {
    // Issue #1099 / XEP-0198 §4: "If the value of 'h' is greater than
    // the number of stanzas sent by the server... it is RECOMMENDED
    // to close the stream with an undefined-condition stream error"
    // carrying <handled-count-too-high/>. The live `<a h='N'/>` path
    // previously acknowledged unconditionally, silently destroying
    // the replay queue and the claimed pending_delivery rows.
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let stream_id = enable_sm_for_live_ack_tests(state.as_ref(), &mut conn, &jid).await;

    // Two outbound stanzas recorded → send-count is 2.
    let _ = conn.sm_state.record_outbound(
        "<message xmlns='jabber:client' id='o1'/>".to_string(),
        SmEvictionPath::DirectOutbound,
    );
    let _ = conn.sm_state.record_outbound(
        "<message xmlns='jabber:client' id='o2'/>".to_string(),
        SmEvictionPath::DirectOutbound,
    );
    let recipient: BareJid = "alice@example.com".parse().expect("bare jid");
    seed_claimed_pending_row(state.as_ref(), &recipient, &stream_id, 1).await;

    // Client claims it handled 5 stanzas; we only sent 2.
    let responses = handle_xmpp_frame(
        "<a xmlns='urn:xmpp:sm:3' h='5'/>",
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(
        responses.len(),
        2,
        "bogus live ack must yield stream error + close: {responses:?}"
    );
    let stream_error = Element::from_str(&responses[0]).expect("stream error xml");
    assert_eq!(stream_error.name(), "error");
    assert_eq!(stream_error.ns(), waddle_xmpp::ns::STREAM);
    assert!(
        responses[0].contains("undefined-condition")
            && responses[0].contains("handled-count-too-high")
            && (responses[0].contains("h='5'") || responses[0].contains("h=\"5\""))
            && (responses[0].contains("send-count='2'")
                || responses[0].contains("send-count=\"2\"")),
        "expected handled-count-too-high stream error: {responses:?}"
    );
    let close = Element::from_str(&responses[1]).expect("close frame xml");
    assert_eq!(close.name(), "close");
    assert_eq!(close.ns(), "urn:ietf:params:xml:ns:xmpp-framing");
    assert!(
        conn.phase.is_closing(),
        "connection must be Closing after handled-count-too-high"
    );

    // Nothing was purged: both stanzas remain replayable and the
    // claimed pending_delivery row survives.
    assert_eq!(
        conn.sm_state.get_stanzas_to_resend(0).len(),
        2,
        "bogus ack must not purge the replay queue"
    );
    let rows = state
        .deps
        .protocol
        .pending_delivery_storage
        .list(&recipient)
        .await
        .expect("list pending rows");
    assert_eq!(rows.len(), 1, "bogus ack must not delete pending rows");
}

#[tokio::test]
async fn sm_live_ack_with_valid_handled_count_purges_queue_and_rows() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let stream_id = enable_sm_for_live_ack_tests(state.as_ref(), &mut conn, &jid).await;

    let _ = conn.sm_state.record_outbound(
        "<message xmlns='jabber:client' id='o1'/>".to_string(),
        SmEvictionPath::DirectOutbound,
    );
    let _ = conn.sm_state.record_outbound(
        "<message xmlns='jabber:client' id='o2'/>".to_string(),
        SmEvictionPath::DirectOutbound,
    );
    let recipient: BareJid = "alice@example.com".parse().expect("bare jid");
    seed_claimed_pending_row(state.as_ref(), &recipient, &stream_id, 1).await;

    let responses = handle_xmpp_frame(
        "<a xmlns='urn:xmpp:sm:3' h='1'/>",
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert!(responses.is_empty(), "valid ack yields no response frames");
    assert!(!conn.phase.is_closing(), "valid ack must not close");
    assert_eq!(
        conn.sm_state.get_stanzas_to_resend(0).len(),
        1,
        "acked prefix must be purged, unacked tail retained"
    );
    let rows = state
        .deps
        .protocol
        .pending_delivery_storage
        .list(&recipient)
        .await
        .expect("list pending rows");
    assert!(
        rows.is_empty(),
        "acked pending_delivery rows must be range-deleted"
    );
}

#[tokio::test]
async fn sm_live_ack_at_exact_outbound_count_is_accepted() {
    // Boundary: h == send-count is a full ack, not a violation
    // (XEP-0198 §4 only forbids h GREATER than the sent count).
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let _stream_id = enable_sm_for_live_ack_tests(state.as_ref(), &mut conn, &jid).await;

    let _ = conn.sm_state.record_outbound(
        "<message xmlns='jabber:client' id='o1'/>".to_string(),
        SmEvictionPath::DirectOutbound,
    );
    let _ = conn.sm_state.record_outbound(
        "<message xmlns='jabber:client' id='o2'/>".to_string(),
        SmEvictionPath::DirectOutbound,
    );

    let responses = handle_xmpp_frame(
        "<a xmlns='urn:xmpp:sm:3' h='2'/>",
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert!(responses.is_empty(), "h == send-count is a valid full ack");
    assert!(!conn.phase.is_closing());
    assert_eq!(
        conn.sm_state.get_stanzas_to_resend(0).len(),
        0,
        "full ack empties the replay queue"
    );
}

#[tokio::test]
async fn sm_live_ack_is_wrap_aware_past_u32_max() {
    // XEP-0198 §4: counters wrap at 2^32 ("in the unlikely case that
    // the number of stanzas handled ... exceeds 2^32"). "Greater than"
    // must therefore be judged mod 2^32: with outbound_count wrapped
    // to 2, a client ack of h = 4294967295 (u32::MAX, i.e. 3 stanzas
    // behind the wrap) is VALID, not "too high".
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let stream_id = enable_sm_for_live_ack_tests(state.as_ref(), &mut conn, &jid).await;

    // Restore counters as a wrapped session would carry them: the
    // server has sent 2^32 + 2 stanzas, the client acked u32::MAX - 1.
    let detached = waddle_xmpp::stream_management::DetachedSession {
        stream_id: stream_id.clone(),
        user_id: "alice@example.com".to_string(),
        jid: jid.clone(),
        inbound_count: 0,
        shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
        outbound_count: 2,
        last_acked: u32::MAX - 1,
        replay_gap_through: None,
        unacked_stanzas: vec![
            waddle_xmpp::stream_management::DetachedUnackedStanza {
                sequence: u32::MAX,
                stanza_xml: "<message xmlns='jabber:client' id='pre-wrap'/>".to_string(),
                original_receipt_at: chrono::Utc::now(),
            },
            waddle_xmpp::stream_management::DetachedUnackedStanza {
                sequence: 1,
                stanza_xml: "<message xmlns='jabber:client' id='post-wrap-1'/>".to_string(),
                original_receipt_at: chrono::Utc::now(),
            },
            waddle_xmpp::stream_management::DetachedUnackedStanza {
                sequence: 2,
                stanza_xml: "<message xmlns='jabber:client' id='post-wrap-2'/>".to_string(),
                original_receipt_at: chrono::Utc::now(),
            },
        ],
        max_resume_time: Some(300),
        detached_at: std::time::Instant::now(),
        carbons_enabled: false,
        roster_interested: false,
        blocklist_interested: false,
        presence_available: false,
        presence_show: None,
        presence_status: None,
        presence_priority: 0,
        presence_payloads: Vec::new(),
        pending_subscribes_flushed: false,
    };
    conn.sm_state.restore_from_session(&detached);

    // h = u32::MAX acks the pre-wrap stanza. A naive `h > outbound`
    // comparison would misread this as handled-count-too-high.
    let responses = handle_xmpp_frame(
        &waddle_xmpp::stream_management::SmAck::new(u32::MAX).to_xml(),
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert!(
        responses.is_empty(),
        "wrapped ack behind the counter is valid: {responses:?}"
    );
    assert!(!conn.phase.is_closing(), "wrapped valid ack must not close");
    assert_eq!(
        conn.sm_state.get_stanzas_to_resend(u32::MAX).len(),
        2,
        "pre-wrap stanza purged; post-wrap stanzas retained"
    );
}

#[tokio::test]
async fn sm_live_ack_at_half_window_distance_is_ignored_not_acknowledged() {
    // XEP-0198 §4 exact-window corner: h == outbound_count +
    // 0x8000_0000 sits at exactly mod-2^32 distance 2^31 from the
    // confirmed window — the one point where "ahead" and "behind" are
    // indistinguishable. Whatever it is classified as, it MUST NOT be
    // acknowledged (that would poison last_acked and purge the replay
    // queue). The regress guard runs first and its half-space
    // comparison is true at exactly 2^31, so the live path ignores it
    // inert, exactly like any other stale mod-behind h.
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let _stream_id = enable_sm_for_live_ack_tests(state.as_ref(), &mut conn, &jid).await;

    let _ = conn.sm_state.record_outbound(
        "<message xmlns='jabber:client' id='o1'/>".to_string(),
        SmEvictionPath::DirectOutbound,
    );
    let _ = conn.sm_state.record_outbound(
        "<message xmlns='jabber:client' id='o2'/>".to_string(),
        SmEvictionPath::DirectOutbound,
    );
    // Full valid ack first: last_acked == outbound_count == 2.
    let responses = handle_xmpp_frame(
        "<a xmlns='urn:xmpp:sm:3' h='2'/>",
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;
    assert!(responses.is_empty(), "full ack is valid");

    let bogus_h = 2u32.wrapping_add(0x8000_0000);
    let responses = handle_xmpp_frame(
        &waddle_xmpp::stream_management::SmAck::new(bogus_h).to_xml(),
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert!(
        responses.is_empty(),
        "half-window h must be ignored inert: {responses:?}"
    );
    assert!(
        !conn.phase.is_closing(),
        "ignored half-window h must not close the stream"
    );
    // MUST NOT have acknowledged: a later in-window ack still works
    // against uncorrupted state.
    assert_eq!(
        conn.sm_state.unacked_count(),
        0,
        "last_acked must not be poisoned by the ignored h"
    );
}

#[tokio::test]
async fn sm_live_ack_in_regressed_half_space_is_ignored_without_purge() {
    // h == outbound_count + 0x8000_0001 lands mod-2^32 BEHIND
    // last_acked: the regress guard must ignore it wholesale (before
    // the exact-window too-high check reclassifies it), leaving the
    // replay queue and last_acked untouched.
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let _stream_id = enable_sm_for_live_ack_tests(state.as_ref(), &mut conn, &jid).await;

    let _ = conn.sm_state.record_outbound(
        "<message xmlns='jabber:client' id='o1'/>".to_string(),
        SmEvictionPath::DirectOutbound,
    );
    let _ = conn.sm_state.record_outbound(
        "<message xmlns='jabber:client' id='o2'/>".to_string(),
        SmEvictionPath::DirectOutbound,
    );
    // Partial ack: last_acked = 1, one stanza still unacked.
    let responses = handle_xmpp_frame(
        "<a xmlns='urn:xmpp:sm:3' h='1'/>",
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;
    assert!(responses.is_empty(), "partial ack is valid");

    let stale_h = 2u32.wrapping_add(0x8000_0001);
    let responses = handle_xmpp_frame(
        &waddle_xmpp::stream_management::SmAck::new(stale_h).to_xml(),
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert!(
        responses.is_empty(),
        "regressed-half-space h must be ignored inert: {responses:?}"
    );
    assert!(!conn.phase.is_closing(), "ignored ack must not close");
    assert_eq!(
        conn.sm_state.get_stanzas_to_resend(1).len(),
        1,
        "ignored ack must not purge the replay queue"
    );
}

#[tokio::test]
async fn sm_resume_at_half_window_distance_is_rejected_as_handled_count_too_high() {
    // Resume-path twin of the live half-window corner: a detached
    // session with outbound_count == last_acked == 2 must reject
    // h == 2 + 0x8000_0000 as handled-count-too-high instead of
    // resuming and poisoning the restored counters.
    use waddle_xmpp::stream_management::DetachedSession;
    let state = create_test_websocket_state().await;

    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let _detached_session = store_resumable_test_session(
        state.as_ref(),
        DetachedSession {
            stream_id: "stream-half-window".to_string(),
            user_id: "alice@example.com".to_string(),
            jid: jid.clone(),
            inbound_count: 0,
            shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
            outbound_count: 2,
            last_acked: 2,
            replay_gap_through: None,
            unacked_stanzas: vec![],
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        },
    )
    .await;

    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::authenticated(&jid);
    let bogus_h = 2u32.wrapping_add(0x8000_0000);
    let resume_frame = resume_frame_xml("stream-half-window", bogus_h);
    let responses =
        handle_xmpp_frame(&resume_frame, "example.com", state.as_ref(), &mut conn).await;

    assert!(
        !responses.iter().any(|frame| frame.contains("<resumed")),
        "half-window h must not resume: {responses:?}"
    );
    assert!(
        responses
            .iter()
            .any(|frame| frame.contains("handled-count-too-high")),
        "expected handled-count-too-high stream error: {responses:?}"
    );
    assert!(
        conn.phase.is_closing(),
        "connection must be Closing after handled-count-too-high"
    );
}

#[tokio::test]
async fn sm_resume_with_regressed_h_fails_resume_instead_of_stream_error() {
    // A resume h mod-2^32 BEHIND the detached last_acked is a failed
    // resume (<failed/> resource-constraint via can_resume_from), NOT
    // a handled-count-too-high stream error — matching the live path
    // where the regress guard runs before the exact-window check.
    use waddle_xmpp::stream_management::DetachedSession;
    let state = create_test_websocket_state().await;

    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let _detached_session = store_resumable_test_session(
        state.as_ref(),
        DetachedSession {
            stream_id: "stream-regressed".to_string(),
            user_id: "alice@example.com".to_string(),
            jid: jid.clone(),
            inbound_count: 0,
            shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
            outbound_count: 2,
            last_acked: 2,
            replay_gap_through: None,
            unacked_stanzas: vec![],
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        },
    )
    .await;

    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::authenticated(&jid);
    let resume_frame = resume_frame_xml("stream-regressed", 1);
    let responses =
        handle_xmpp_frame(&resume_frame, "example.com", state.as_ref(), &mut conn).await;

    assert!(
        responses
            .iter()
            .any(|frame| frame.contains("<failed") && frame.contains("resource-constraint")),
        "regressed h must produce a failed resume: {responses:?}"
    );
    assert!(
        !responses
            .iter()
            .any(|frame| frame.contains("handled-count-too-high")),
        "regressed h must not be reclassified as too-high: {responses:?}"
    );
    assert!(
        !conn.phase.is_closing(),
        "failed resume keeps the stream open for a fresh session"
    );
}

#[tokio::test]
async fn sm_resume_restores_session_and_replays_unacked() {
    use waddle_xmpp::stream_management::DetachedSession;
    let state = create_test_websocket_state().await;

    // Seed a detached session directly in the registry — this is the
    // shape left behind by a prior WebSocket task after detach-on-close.
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let stream_id = "stream-xyz".to_string();
    let detached = DetachedSession {
        stream_id: stream_id.clone(),
        user_id: "alice@example.com".to_string(),
        jid: jid.clone(),
        inbound_count: 7,
        shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
        outbound_count: 10,
        last_acked: 8,
        replay_gap_through: None,
        unacked_stanzas: vec![
            waddle_xmpp::stream_management::DetachedUnackedStanza {
                sequence: 9,
                stanza_xml: "<message xmlns='jabber:client' id='m9'/>".to_string(),
                original_receipt_at: chrono::Utc::now(),
            },
            waddle_xmpp::stream_management::DetachedUnackedStanza {
                sequence: 10,
                stanza_xml: "<message xmlns='jabber:client' id='m10'/>".to_string(),
                original_receipt_at: chrono::Utc::now(),
            },
        ],
        max_resume_time: Some(300),
        detached_at: std::time::Instant::now(),
        carbons_enabled: true,
        roster_interested: false,
        blocklist_interested: false,
        presence_available: false,
        presence_show: None,
        presence_status: None,
        presence_priority: 0,
        presence_payloads: Vec::new(),
        pending_subscribes_flushed: false,
    };
    let _detached_session = store_resumable_test_session(state.as_ref(), detached).await;

    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::authenticated(&jid);
    // Client reports it has acked through 9, so only m10 needs replay.
    let frame = resume_frame_xml(&stream_id, 9);
    let responses = handle_xmpp_frame(&frame, "example.com", state.as_ref(), &mut conn).await;

    // Expect <resumed/> first, then exactly the one unacked stanza.
    assert!(!responses.is_empty());
    let resumed = Element::from_str(&responses[0]).expect("resumed xml");
    assert_eq!(resumed.name(), "resumed");
    assert_eq!(resumed.attr("previd"), Some(stream_id.as_str()));

    let replay_count = responses.len() - 1;
    assert_eq!(
        replay_count, 1,
        "only m10 should be replayed: {responses:?}"
    );
    assert!(responses[1].contains("m10"));

    // Session identity restored without SASL or bind frames.
    assert!(conn.phase.is_authenticated());
    assert!(conn.phase.is_ready());
    assert_eq!(conn.phase.bound_jid(), Some(&jid));
    assert!(conn.phase.is_resumed());
    assert!(conn.carbons_enabled);
    assert!(matches!(
        &conn.phase,
        ConnectionPhase::Ready {
            full_jid,
            resumed: true,
            ..
        } if full_jid == &jid
    ));
}

#[tokio::test]
async fn sm_resume_rejects_impossible_client_handled_count() {
    use waddle_xmpp::stream_management::DetachedSession;
    let state = create_test_websocket_state().await;

    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let _detached_session = store_resumable_test_session(
        state.as_ref(),
        DetachedSession {
            stream_id: "stream-too-far".to_string(),
            user_id: "alice@example.com".to_string(),
            jid: jid.clone(),
            inbound_count: 4,
            shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
            outbound_count: 2,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: vec![waddle_xmpp::stream_management::DetachedUnackedStanza {
                sequence: 1,
                stanza_xml: "<message xmlns='jabber:client' id='m1'/>".to_string(),
                original_receipt_at: chrono::Utc::now(),
            }],
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        },
    )
    .await;

    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::authenticated(&jid);
    let resume_frame = resume_frame_xml("stream-too-far", 3);
    let responses =
        handle_xmpp_frame(&resume_frame, "example.com", state.as_ref(), &mut conn).await;

    assert_eq!(responses.len(), 2);
    let stream_error = Element::from_str(&responses[0]).expect("stream error xml");
    assert_eq!(stream_error.name(), "error");
    assert_eq!(stream_error.ns(), waddle_xmpp::ns::STREAM);
    assert!(
        !responses[0].contains("</stream:stream>"),
        "RFC 7395 WebSocket stream close must be a separate close frame"
    );
    assert!(
        responses[0].contains("stream:error")
            && responses[0].contains("undefined-condition")
            && responses[0].contains("handled-count-too-high")
            && (responses[0].contains("h='3'") || responses[0].contains("h=\"3\""))
            && (responses[0].contains("send-count='2'")
                || responses[0].contains("send-count=\"2\"")),
        "invalid resume count should be a handled-count-too-high stream error: {responses:?}"
    );
    let close = Element::from_str(&responses[1]).expect("close frame xml");
    assert_eq!(close.name(), "close");
    assert_eq!(close.ns(), "urn:ietf:params:xml:ns:xmpp-framing");
    assert!(
        !conn.sm_state.enabled,
        "rejected resume must not pollute the fresh stream SM state"
    );
    assert!(
        !conn.sm_state.is_resumable(),
        "rejected resume must not make the fresh stream resumable"
    );
    assert!(
        state
            .deps
            .protocol
            .sm_session_registry
            .take_session("stream-too-far")
            .await
            .expect("lookup")
            .is_some(),
        "rejected resume must release the detached session for a valid retry"
    );
}

#[tokio::test]
async fn sm_resume_replays_roster_push_recorded_while_detached() {
    use waddle_xmpp::stream_management::DetachedSession;
    let state = create_test_websocket_state().await;

    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let stream_id = "stream-roster-replay".to_string();
    let _detached_session = store_resumable_test_session(
        state.as_ref(),
        DetachedSession {
            stream_id: stream_id.clone(),
            user_id: "alice@example.com".to_string(),
            jid: jid.clone(),
            inbound_count: 0,
            shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: true,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        },
    )
    .await;

    let recorded = state
            .deps
            .protocol
            .sm_session_registry
            .record_stanza_for_detached_resource(
                &jid,
                &Stanza::Iq(Box::new(
                    xmpp_parsers::iq::Iq::try_from(
                        Element::from_str(
                            "<iq xmlns='jabber:client' type='set' id='detached-roster-push'><query xmlns='jabber:iq:roster'/></iq>",
                        )
                        .expect("iq element"),
                    )
                    .expect("iq stanza"),
                )),
                chrono::Utc::now(),
            )
            .await
            .expect("record detached roster push");
    assert!(recorded);

    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::authenticated(&jid);
    let frame = resume_frame_xml(&stream_id, 0);
    let responses = handle_xmpp_frame(&frame, "example.com", state.as_ref(), &mut conn).await;

    assert_eq!(
        responses.len(),
        2,
        "expected resumed plus replay: {responses:?}"
    );
    assert!(responses[0].contains("<resumed"));
    assert!(
        responses[1].contains("detached-roster-push"),
        "detached roster push should replay after resume: {responses:?}"
    );
    assert!(conn.roster_interested);
}

#[tokio::test]
async fn direct_full_jid_message_records_for_detached_resource_replay() {
    use waddle_xmpp::stream_management::DetachedSession;
    let state = create_test_websocket_state().await;

    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let alice_jid: FullJid = "alice@example.com/phone".parse().expect("alice jid");
    let stream_id = "stream-detached-direct-message".to_string();
    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id: stream_id.clone(),
            user_id: "alice@example.com".to_string(),
            jid: alice_jid,
            inbound_count: 0,
            shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        })
        .await
        .expect("store detached alice");

    let mut bob = WsConnState::new();
    bob.phase = ConnectionPhase::ready(bob_jid.clone(), false);
    bob.ensure_state_machine(
        "example.com",
        &state.deps.protocol.dispatcher,
        bob_jid,
        false,
        Blocklist::empty(),
    );
    let responses = handle_xmpp_frame(
            r#"<message xmlns="jabber:client" type="chat" to="alice@example.com/phone" id="detached-dm-1"><body>queued while detached</body></message>"#,
            "example.com",
            state.as_ref(),
            &mut bob,
        )
        .await;
    assert!(responses.is_empty());

    let detached = state
        .deps
        .protocol
        .sm_session_registry
        .take_session(&stream_id)
        .await
        .expect("take detached")
        .expect("detached session remains");
    assert!(
        detached
            .unacked_stanzas
            .iter()
            .any(|entry| entry.stanza_xml.contains("detached-dm-1")),
        "full-JID direct message should be recorded for detached replay: {detached:?}"
    );
}

#[tokio::test]
async fn bare_jid_message_records_for_detached_resource_replay() {
    use waddle_xmpp::stream_management::DetachedSession;
    let state = create_test_websocket_state().await;

    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let alice_jid: FullJid = "alice@example.com/phone".parse().expect("alice jid");
    let stream_id = "stream-detached-bare-message".to_string();
    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id: stream_id.clone(),
            user_id: "alice@example.com".to_string(),
            jid: alice_jid,
            inbound_count: 0,
            shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        })
        .await
        .expect("store detached alice");

    let mut bob = WsConnState::new();
    bob.phase = ConnectionPhase::ready(bob_jid.clone(), false);
    bob.ensure_state_machine(
        "example.com",
        &state.deps.protocol.dispatcher,
        bob_jid,
        false,
        Blocklist::empty(),
    );
    let responses = handle_xmpp_frame(
            r#"<message xmlns="jabber:client" type="chat" to="alice@example.com" id="detached-bare-dm-1"><body>queued while detached</body></message>"#,
            "example.com",
            state.as_ref(),
            &mut bob,
        )
        .await;
    assert!(responses.is_empty());

    let detached = state
        .deps
        .protocol
        .sm_session_registry
        .take_session(&stream_id)
        .await
        .expect("take detached")
        .expect("detached session remains");
    assert!(
        detached
            .unacked_stanzas
            .iter()
            .any(|entry| entry.stanza_xml.contains("detached-bare-dm-1")),
        "bare-JID direct message should be recorded for detached replay: {detached:?}"
    );
    // RFC 6121 §8.5.2.1.1: bare-JID delivery routes the original
    // stanza to each available resource without rewriting `to`.
    // The dispatcher path preserves this — legacy `handle_message`
    // rewrote `to` to the per-resource full JID, which was a
    // server-side deviation from the RFC. Assert only the
    // reachability semantic here; integration tests verify the
    // wire shape end-to-end.
}

#[tokio::test]
async fn message_carbons_record_for_detached_enabled_resources() {
    use waddle_xmpp::stream_management::DetachedSession;
    let state = create_test_websocket_state().await;
    // #1246: an unregistered recipient would bounce with
    // <service-unavailable/>; this test is about carbons for the
    // sender's detached sibling, so give bob a real account.
    crate::server::routes::websocket::tests::seed_local_account(state.as_ref(), "bob").await;

    let alice_phone: FullJid = "alice@example.com/phone".parse().expect("alice phone");
    let alice_laptop: FullJid = "alice@example.com/laptop".parse().expect("alice laptop");
    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let sent_stream_id = "stream-detached-sent-carbon".to_string();
    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id: sent_stream_id.clone(),
            user_id: "alice@example.com".to_string(),
            jid: alice_laptop.clone(),
            inbound_count: 0,
            shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: true,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        })
        .await
        .expect("store detached alice laptop");

    let mut alice = WsConnState::new();
    alice.phase = ConnectionPhase::ready(alice_phone.clone(), false);
    alice.ensure_state_machine(
        "example.com",
        &state.deps.protocol.dispatcher,
        alice_phone.clone(),
        false,
        Blocklist::empty(),
    );
    let responses = handle_xmpp_frame(
            r#"<message xmlns="jabber:client" type="chat" to="bob@example.com/web" id="detached-sent-carbon-source"><body>copy me</body></message>"#,
            "example.com",
            state.as_ref(),
            &mut alice,
        )
        .await;
    assert!(responses.is_empty());

    let sent_detached = state
        .deps
        .protocol
        .sm_session_registry
        .take_session(&sent_stream_id)
        .await
        .expect("take sent detached")
        .expect("sent detached session remains");
    assert!(
        sent_detached
            .unacked_stanzas
            .iter()
            .any(|entry| entry.stanza_xml.contains("<sent")
                && entry.stanza_xml.contains("urn:xmpp:carbons:2")
                && entry.stanza_xml.contains("detached-sent-carbon-source")),
        "sent carbon should be recorded for detached opted-in resource: {sent_detached:?}"
    );

    let received_stream_id = "stream-detached-received-carbon".to_string();
    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id: received_stream_id.clone(),
            user_id: "alice@example.com".to_string(),
            jid: alice_laptop,
            inbound_count: 0,
            shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: true,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        })
        .await
        .expect("store detached alice laptop again");

    // Build alice/phone's per-connection state machine so we can
    // drive the recipient-pass carbon fan-out the dispatcher path
    // owns. In production this happens automatically via
    // alice/phone's main loop dispatching the queued
    // `DeliveryKind::PeerStanza`; the unit test reproduces the
    // same step explicitly.
    let mut alice_phone_conn = WsConnState::new();
    alice_phone_conn.phase = ConnectionPhase::ready(alice_phone.clone(), false);
    alice_phone_conn.ensure_state_machine(
        "example.com",
        &state.deps.protocol.dispatcher,
        alice_phone.clone(),
        false,
        Blocklist::empty(),
    );
    let (alice_phone_tx, mut alice_phone_rx) = mpsc::channel::<OutboundStanza>(16);
    // ADR-0017 Slice 2: delivery reads the actor tree, so register into both.
    super::register_test_connection(state.as_ref(), &alice_phone, alice_phone_tx).await;

    let mut bob = WsConnState::new();
    bob.phase = ConnectionPhase::ready(bob_jid.clone(), false);
    bob.ensure_state_machine(
        "example.com",
        &state.deps.protocol.dispatcher,
        bob_jid,
        false,
        Blocklist::empty(),
    );
    let responses = handle_xmpp_frame(
            r#"<message xmlns="jabber:client" type="chat" to="alice@example.com/phone" id="detached-received-carbon-source"><body>copy me too</body></message>"#,
            "example.com",
            state.as_ref(),
            &mut bob,
        )
        .await;
    assert!(responses.is_empty());

    // Pump the queued PeerStanza through alice/phone's SM so the
    // recipient pass runs and the dispatcher emits the
    // received-carbon fan-out. This is the same dispatch the
    // production main loop performs on `DeliveryKind::PeerStanza`.
    while let Ok(outbound) = alice_phone_rx.try_recv() {
        if !matches!(outbound.kind, DeliveryKind::PeerStanza) {
            continue;
        }
        let sm = alice_phone_conn.state_machine.as_mut().expect("alice SM");
        let events = sm.handle(InboundEvent::StanzaFromPeer(Box::new(outbound.stanza)));
        let deps = build_interpret_deps(state.as_ref(), None);
        let _ = drive_interpret_loop(events, sm, &deps).await;
    }

    let received_detached = state
        .deps
        .protocol
        .sm_session_registry
        .take_session(&received_stream_id)
        .await
        .expect("take received detached")
        .expect("received detached session remains");
    assert!(
        received_detached.unacked_stanzas.iter().any(|entry| entry
            .stanza_xml
            .contains("<received")
            && entry.stanza_xml.contains("urn:xmpp:carbons:2")
            && entry.stanza_xml.contains("detached-received-carbon-source")),
        "received carbon should be recorded for detached opted-in resource: {received_detached:?}"
    );
}

#[tokio::test]
async fn duplicate_subscribe_ack_reaches_non_roster_interested_resource() {
    let state = create_test_websocket_state().await;

    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let alice_jid: FullJid = "alice@example.com/phone".parse().expect("alice jid");
    let (bob_tx, mut bob_rx) = mpsc::channel::<OutboundStanza>(16);
    let (alice_tx, mut alice_rx) = mpsc::channel::<OutboundStanza>(16);
    // ADR-0017 Phase 3 Slice 9: the subscription-ack path enumerates the
    // requester's (bob's) resources through the actor-authoritative registry,
    // so bob must be dual-registered exactly as production bind does. Alice is
    // reached via the available/roster-interested paths (unchanged), so a bare
    // DashMap register still suffices for her.
    let bob_owner = super::register_test_connection(state.as_ref(), &bob_jid, bob_tx).await;
    let alice_owner = state
        .deps
        .protocol
        .connection_registry
        .register(alice_jid.clone(), alice_tx);

    let mut alice = WsConnState::new();
    alice.phase = ConnectionPhase::ready(alice_jid.clone(), false);
    // #1208: presence registry writes are owner-gated; the fixture
    // registers out-of-band, so carry the owner token like real
    // registration does.
    alice.registry_owner = Some(alice_owner);
    let _ = handle_xmpp_frame(
            r#"<iq xmlns="jabber:client" type="get" id="alice-roster"><query xmlns="jabber:iq:roster"/></iq>"#,
            "example.com",
            state.as_ref(),
            &mut alice,
        )
        .await;
    let _ = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client"/>"#,
        "example.com",
        state.as_ref(),
        &mut alice,
    )
    .await;
    while alice_rx.try_recv().is_ok() {}

    let mut bob = WsConnState::new();
    bob.phase = ConnectionPhase::ready(bob_jid.clone(), false);
    bob.registry_owner = Some(bob_owner);
    let _ = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client" type="subscribe" to="alice@example.com"/>"#,
        "example.com",
        state.as_ref(),
        &mut bob,
    )
    .await;
    let _ = tokio::time::timeout(std::time::Duration::from_millis(250), alice_rx.recv())
        .await
        .expect("alice receives initial subscribe")
        .expect("subscribe stanza");

    let _ = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client" type="subscribed" to="bob@example.com"/>"#,
        "example.com",
        state.as_ref(),
        &mut alice,
    )
    .await;
    while bob_rx.try_recv().is_ok() {}

    let _ = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client" type="subscribe" to="alice@example.com"/>"#,
        "example.com",
        state.as_ref(),
        &mut bob,
    )
    .await;
    let ack = tokio::time::timeout(std::time::Duration::from_millis(250), bob_rx.recv())
        .await
        .expect("duplicate subscribe ack")
        .expect("ack stanza");
    let frame = stanza_to_xml(&ack.stanza);
    assert!(
        frame.contains("from='alice@example.com'")
            && frame.contains("to='bob@example.com'")
            && frame.contains("type='subscribed'"),
        "duplicate subscribe ack should reach a live resource even before roster get: {frame}"
    );
}

#[tokio::test]
async fn roster_set_records_push_for_detached_interested_resource() {
    use waddle_xmpp::stream_management::DetachedSession;
    let state = create_test_websocket_state().await;

    let detached_jid: FullJid = "alice@example.com/web".parse().expect("detached jid");
    let source_jid: FullJid = "alice@example.com/phone".parse().expect("source jid");
    let stream_id = "stream-roster-fanout".to_string();
    let _detached_session = store_resumable_test_session(
        state.as_ref(),
        DetachedSession {
            stream_id: stream_id.clone(),
            user_id: "alice@example.com".to_string(),
            jid: detached_jid.clone(),
            inbound_count: 0,
            shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: true,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        },
    )
    .await;

    let mut source = WsConnState::new();
    source.phase = ConnectionPhase::ready(source_jid, false);
    let responses = handle_xmpp_frame(
            r#"<iq xmlns="jabber:client" type="set" id="roster-detached-fanout"><query xmlns="jabber:iq:roster"><item jid="bob@example.com" name="Bob"/></query></iq>"#,
            "example.com",
            state.as_ref(),
            &mut source,
        )
        .await;
    assert!(
        responses.iter().any(
            |frame| frame.contains("roster-detached-fanout") && frame.contains("type='result'")
        ),
        "roster set should succeed: {responses:?}"
    );

    let mut resumed = WsConnState::new();
    resumed.phase = ConnectionPhase::authenticated(&detached_jid);
    let resume_frame = resume_frame_xml(&stream_id, 0);
    let replay =
        handle_xmpp_frame(&resume_frame, "example.com", state.as_ref(), &mut resumed).await;
    assert!(
        replay
            .iter()
            .any(|frame| frame.contains("jabber:iq:roster") && frame.contains("bob@example.com")),
        "detached interested resource should replay roster fanout push: {replay:?}"
    );
    assert!(resumed.roster_interested);
}

#[tokio::test]
async fn blocking_set_records_push_for_detached_blocklist_interested_resource() {
    use waddle_xmpp::stream_management::DetachedSession;
    let state = create_test_websocket_state().await;

    let detached_jid: FullJid = "alice@example.com/web".parse().expect("detached jid");
    let source_jid: FullJid = "alice@example.com/phone".parse().expect("source jid");
    let stream_id = "stream-blocking-fanout".to_string();
    let _detached_session = store_resumable_test_session(
        state.as_ref(),
        DetachedSession {
            stream_id: stream_id.clone(),
            user_id: "alice@example.com".to_string(),
            jid: detached_jid.clone(),
            inbound_count: 0,
            shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: true,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        },
    )
    .await;

    let mut source = WsConnState::new();
    source.phase = ConnectionPhase::ready(source_jid, false);
    let responses = handle_xmpp_frame(
        r#"<iq xmlns="jabber:client" type="set" id="blocking-detached-fanout"><block xmlns="urn:xmpp:blocking"><item jid="bob@example.com"/></block></iq>"#,
        "example.com",
        state.as_ref(),
        &mut source,
    )
    .await;
    assert!(
        responses
            .iter()
            .any(|frame| frame.contains("blocking-detached-fanout")
                && frame.contains("type='result'")),
        "block set should succeed: {responses:?}"
    );

    let mut resumed = WsConnState::new();
    resumed.phase = ConnectionPhase::authenticated(&detached_jid);
    let resume_frame = resume_frame_xml(&stream_id, 0);
    let replay =
        handle_xmpp_frame(&resume_frame, "example.com", state.as_ref(), &mut resumed).await;
    assert!(
        replay
            .iter()
            .any(|frame| frame.contains("urn:xmpp:blocking") && frame.contains("bob@example.com")),
        "detached blocklist-interested resource should replay blocking push: {replay:?}"
    );
    assert!(resumed.blocklist_interested);
}

#[tokio::test]
async fn subscription_approval_replays_current_presence_from_detached_available_resource() {
    use waddle_xmpp::stream_management::DetachedSession;
    let state = create_test_websocket_state().await;

    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let alice_web_jid: FullJid = "alice@example.com/web".parse().expect("alice web jid");
    let alice_phone_jid: FullJid = "alice@example.com/phone".parse().expect("alice phone jid");
    let (bob_tx, mut bob_rx) = mpsc::channel::<OutboundStanza>(16);
    state
        .deps
        .protocol
        .connection_registry
        .register(bob_jid.clone(), bob_tx);
    state
        .deps
        .protocol
        .connection_registry
        .mark_roster_interested(&bob_jid);
    state
        .deps
        .protocol
        .connection_registry
        .update_presence(&bob_jid, true, 0);

    let mut bob = WsConnState::new();
    bob.phase = ConnectionPhase::ready(bob_jid.clone(), false);
    let _ = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client" type="subscribe" to="alice@example.com"/>"#,
        "example.com",
        state.as_ref(),
        &mut bob,
    )
    .await;
    while bob_rx.try_recv().is_ok() {}

    let stream_id = "stream-detached-current-presence".to_string();
    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id,
            user_id: "alice@example.com".to_string(),
            jid: alice_web_jid.clone(),
            inbound_count: 0,
            shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: true,
            presence_show: Some(xmpp_parsers::presence::Show::Chat),
            presence_status: Some("ready from detach".to_string()),
            presence_priority: 7,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        })
        .await
        .expect("store detached alice web");

    let mut alice_phone = WsConnState::new();
    alice_phone.phase = ConnectionPhase::ready(alice_phone_jid, false);
    let _ = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client" type="subscribed" to="bob@example.com"/>"#,
        "example.com",
        state.as_ref(),
        &mut alice_phone,
    )
    .await;

    let mut delivered = Vec::new();
    for _ in 0..4 {
        if let Ok(Some(outbound)) =
            tokio::time::timeout(std::time::Duration::from_millis(250), bob_rx.recv()).await
        {
            delivered.push(stanza_to_xml(&outbound.stanza));
        }
    }
    assert!(
        delivered.iter().any(|frame| {
            frame.contains("from='alice@example.com/web'")
                && frame.contains("<show>chat</show>")
                && frame.contains("<status>ready from detach</status>")
                && frame.contains("<priority>7</priority>")
        }),
        "approval should deliver current rich presence from detached available resource: {delivered:?}"
    );
}

#[tokio::test]
async fn presence_probe_returns_detached_available_resource_presence() {
    use waddle_xmpp::stream_management::DetachedSession;
    let state = create_test_websocket_state().await;

    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let alice_jid: FullJid = "alice@example.com/phone".parse().expect("alice jid");
    let (bob_tx, mut bob_rx) = mpsc::channel::<OutboundStanza>(16);
    // ADR-0017 Phase 3 Slice 9: the probe path enumerates the requester's
    // (bob's) resources through the actor-authoritative registry, so bob must
    // be dual-registered exactly as production bind does.
    super::register_test_connection(state.as_ref(), &bob_jid, bob_tx).await;

    let mut bob = WsConnState::new();
    bob.phase = ConnectionPhase::ready(bob_jid.clone(), false);
    let _ = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client" type="subscribe" to="alice@example.com"/>"#,
        "example.com",
        state.as_ref(),
        &mut bob,
    )
    .await;
    let mut alice = WsConnState::new();
    alice.phase = ConnectionPhase::ready(alice_jid.clone(), false);
    let _ = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client" type="subscribed" to="bob@example.com"/>"#,
        "example.com",
        state.as_ref(),
        &mut alice,
    )
    .await;
    while bob_rx.try_recv().is_ok() {}

    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id: "stream-detached-probe".to_string(),
            user_id: "alice@example.com".to_string(),
            jid: alice_jid,
            inbound_count: 0,
            shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: true,
            presence_show: Some(xmpp_parsers::presence::Show::Away),
            presence_status: Some("stepped away".to_string()),
            presence_priority: 5,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        })
        .await
        .expect("store detached alice");

    bob.phase = ConnectionPhase::ready(bob_jid, false);
    let responses = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client" type="probe" to="alice@example.com"/>"#,
        "example.com",
        state.as_ref(),
        &mut bob,
    )
    .await;
    assert!(responses.is_empty());

    let outbound = tokio::time::timeout(std::time::Duration::from_millis(250), bob_rx.recv())
        .await
        .expect("probe response")
        .expect("outbound stanza");
    let frame = stanza_to_xml(&outbound.stanza);
    assert!(
        frame.contains("from='alice@example.com/phone'")
            && frame.contains("to='bob@example.com'")
            && frame.contains("<show>away</show>")
            && frame.contains("<status>stepped away</status>")
            && frame.contains("<priority>5</priority>"),
        "probe should return rich presence from detached available resource: {frame}"
    );
}

#[tokio::test]
async fn full_jid_presence_probe_returns_only_that_resources_availability() {
    use waddle_xmpp::stream_management::DetachedSession;
    let state = create_test_websocket_state().await;

    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let alice_phone: FullJid = "alice@example.com/phone".parse().expect("alice phone");
    let alice_tablet: FullJid = "alice@example.com/tablet".parse().expect("alice tablet");
    let (bob_tx, mut bob_rx) = mpsc::channel::<OutboundStanza>(16);
    // ADR-0017 Phase 3 Slice 9: the probe path enumerates the requester's
    // (bob's) resources through the actor-authoritative registry, so bob must
    // be dual-registered exactly as production bind does.
    super::register_test_connection(state.as_ref(), &bob_jid, bob_tx).await;

    let mut bob = WsConnState::new();
    bob.phase = ConnectionPhase::ready(bob_jid.clone(), false);
    let _ = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client" type="subscribe" to="alice@example.com"/>"#,
        "example.com",
        state.as_ref(),
        &mut bob,
    )
    .await;
    let mut alice = WsConnState::new();
    alice.phase = ConnectionPhase::ready(alice_phone.clone(), false);
    let _ = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client" type="subscribed" to="bob@example.com"/>"#,
        "example.com",
        state.as_ref(),
        &mut alice,
    )
    .await;
    while bob_rx.try_recv().is_ok() {}

    for (stream_id, jid, show, status) in [
        (
            "stream-probe-phone",
            alice_phone.clone(),
            xmpp_parsers::presence::Show::Away,
            "phone detail",
        ),
        (
            "stream-probe-tablet",
            alice_tablet,
            xmpp_parsers::presence::Show::Chat,
            "tablet detail",
        ),
    ] {
        state
            .deps
            .protocol
            .sm_session_registry
            .store_session(DetachedSession {
                stream_id: stream_id.to_string(),
                user_id: "alice@example.com".to_string(),
                jid,
                inbound_count: 0,
                shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
                outbound_count: 0,
                last_acked: 0,
                replay_gap_through: None,
                unacked_stanzas: Vec::new(),
                max_resume_time: Some(300),
                detached_at: std::time::Instant::now(),
                carbons_enabled: false,
                roster_interested: false,
                blocklist_interested: false,
                presence_available: true,
                presence_show: Some(show),
                presence_status: Some(status.to_string()),
                presence_priority: 5,
                presence_payloads: Vec::new(),
                pending_subscribes_flushed: false,
            })
            .await
            .expect("store detached alice resource");
    }

    let responses = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client" type="probe" to="alice@example.com/phone"/>"#,
        "example.com",
        state.as_ref(),
        &mut bob,
    )
    .await;
    assert!(responses.is_empty());

    let outbound = tokio::time::timeout(std::time::Duration::from_millis(250), bob_rx.recv())
        .await
        .expect("full-jid probe response")
        .expect("outbound stanza");
    let frame = stanza_to_xml(&outbound.stanza);
    assert!(
        frame.contains("from='alice@example.com/phone'")
            && frame.contains("to='bob@example.com'")
            && frame.contains("<show>away</show>")
            && frame.contains("<status>phone detail</status>")
            && frame.contains("<priority>5</priority>")
            && !frame.contains("alice@example.com/tablet"),
        "full-JID probe should return rich presence only for the requested resource: {frame}"
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), bob_rx.recv())
            .await
            .is_err(),
        "full-JID probe must not return sibling resources"
    );
}

#[tokio::test]
async fn presence_probe_without_subscription_does_not_reveal_detached_presence() {
    use waddle_xmpp::stream_management::DetachedSession;
    let state = create_test_websocket_state().await;

    let mallory_jid: FullJid = "mallory@example.com/web".parse().expect("mallory jid");
    let alice_jid: FullJid = "alice@example.com/phone".parse().expect("alice jid");
    let (mallory_tx, mut mallory_rx) = mpsc::channel::<OutboundStanza>(16);
    // ADR-0017 Phase 3 Slice 9: the (unsubscribed) probe path enumerates the
    // requester's (mallory's) resources through the actor-authoritative
    // registry to deliver the `unsubscribed` signal, so mallory must be
    // dual-registered exactly as production bind does. The privacy guarantee
    // (no detached presence leaked to an unauthorized prober) is unchanged.
    super::register_test_connection(state.as_ref(), &mallory_jid, mallory_tx).await;
    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id: "stream-detached-probe-denied".to_string(),
            user_id: "alice@example.com".to_string(),
            jid: alice_jid,
            inbound_count: 0,
            shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: true,
            presence_show: Some(xmpp_parsers::presence::Show::Away),
            presence_status: Some("private".to_string()),
            presence_priority: 5,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        })
        .await
        .expect("store detached alice");

    let mut mallory = WsConnState::new();
    mallory.phase = ConnectionPhase::ready(mallory_jid, false);
    let responses = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client" type="probe" to="alice@example.com"/>"#,
        "example.com",
        state.as_ref(),
        &mut mallory,
    )
    .await;
    assert!(responses.is_empty());
    let outbound = tokio::time::timeout(std::time::Duration::from_millis(250), mallory_rx.recv())
        .await
        .expect("unsubscribed probe response")
        .expect("outbound stanza");
    let frame = stanza_to_xml(&outbound.stanza);
    assert!(
        frame.contains("from='alice@example.com'")
            && frame.contains("to='mallory@example.com'")
            && frame.contains("type='unsubscribed'")
            && !frame.contains("alice@example.com/phone")
            && !frame.contains("private"),
        "unauthorized probe must return only an unsubscribed signal: {frame}"
    );
}

#[tokio::test]
async fn expired_detached_available_session_broadcasts_unavailable_to_subscribers() {
    let state = create_test_websocket_state().await;

    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let alice_jid: FullJid = "alice@example.com/phone".parse().expect("alice jid");
    let alice_sibling_jid: FullJid = "alice@example.com/laptop".parse().expect("alice sibling");
    let (bob_tx, mut bob_rx) = mpsc::channel::<OutboundStanza>(16);
    state
        .deps
        .protocol
        .connection_registry
        .register(bob_jid.clone(), bob_tx);
    state
        .deps
        .protocol
        .connection_registry
        .update_presence(&bob_jid, true, 0);
    let (alice_sibling_tx, mut alice_sibling_rx) = mpsc::channel::<OutboundStanza>(16);
    state
        .deps
        .protocol
        .connection_registry
        .register(alice_sibling_jid.clone(), alice_sibling_tx);
    state
        .deps
        .protocol
        .connection_registry
        .update_presence(&alice_sibling_jid, true, 0);

    let mut bob = WsConnState::new();
    bob.phase = ConnectionPhase::ready(bob_jid, false);
    let _ = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client" type="subscribe" to="alice@example.com"/>"#,
        "example.com",
        state.as_ref(),
        &mut bob,
    )
    .await;
    let mut alice = WsConnState::new();
    alice.phase = ConnectionPhase::ready(alice_jid.clone(), false);
    let _ = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client" type="subscribed" to="bob@example.com"/>"#,
        "example.com",
        state.as_ref(),
        &mut alice,
    )
    .await;
    while bob_rx.try_recv().is_ok() {}
    while alice_sibling_rx.try_recv().is_ok() {}

    handlers::presence::broadcast_unavailable_for_terminated_session(state.as_ref(), &alice_jid)
        .await;

    let outbound = tokio::time::timeout(std::time::Duration::from_millis(250), bob_rx.recv())
        .await
        .expect("unavailable broadcast")
        .expect("outbound stanza");
    let frame = stanza_to_xml(&outbound.stanza);
    assert!(
        frame.contains("from='alice@example.com/phone'")
            && frame.contains("to='bob@example.com'")
            && frame.contains("type='unavailable'"),
        "expired detached session should broadcast unavailable presence: {frame}"
    );
    let sibling_outbound = tokio::time::timeout(
        std::time::Duration::from_millis(250),
        alice_sibling_rx.recv(),
    )
    .await
    .expect("sibling unavailable broadcast")
    .expect("outbound stanza");
    let sibling_frame = stanza_to_xml(&sibling_outbound.stanza);
    assert!(
        sibling_frame.contains("from='alice@example.com/phone'")
            && sibling_frame.contains("to='alice@example.com'")
            && sibling_frame.contains("type='unavailable'"),
        "expired detached session should notify sibling resources: {sibling_frame}"
    );
}

#[tokio::test]
async fn subscription_approval_records_roster_push_for_detached_interested_resource() {
    use waddle_xmpp::stream_management::DetachedSession;
    let state = create_test_websocket_state().await;

    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let mut bob = WsConnState::new();
    bob.phase = ConnectionPhase::ready(bob_jid.clone(), false);
    let _ = handle_xmpp_frame(
            r#"<iq xmlns="jabber:client" type="get" id="bob-roster"><query xmlns="jabber:iq:roster"/></iq>"#,
            "example.com",
            state.as_ref(),
            &mut bob,
        )
        .await;
    let _ = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client" type="subscribe" to="alice@example.com"/>"#,
        "example.com",
        state.as_ref(),
        &mut bob,
    )
    .await;

    let stream_id = "stream-detached-subscription-roster-push".to_string();
    let _detached_session = store_resumable_test_session(
        state.as_ref(),
        DetachedSession {
            stream_id: stream_id.clone(),
            user_id: "bob@example.com".to_string(),
            jid: bob_jid.clone(),
            inbound_count: 0,
            shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: true,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        },
    )
    .await;

    let mut alice = WsConnState::new();
    alice.phase = ConnectionPhase::ready(alice_jid, false);
    let _ = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client" type="subscribed" to="bob@example.com"/>"#,
        "example.com",
        state.as_ref(),
        &mut alice,
    )
    .await;

    let mut resumed = WsConnState::new();
    resumed.phase = ConnectionPhase::authenticated(&bob_jid);
    let resume_frame = resume_frame_xml(&stream_id, 0);
    let replay =
        handle_xmpp_frame(&resume_frame, "example.com", state.as_ref(), &mut resumed).await;
    assert!(
        replay.iter().any(|frame| {
            frame.contains("jabber:iq:roster")
                && frame.contains("alice@example.com")
                && frame.contains("subscription='to'")
        }),
        "detached interested resource should replay subscription roster push: {replay:?}"
    );
}

#[tokio::test]
async fn subscribe_to_detached_available_resource_replays_on_resume() {
    use waddle_xmpp::stream_management::DetachedSession;
    let state = create_test_websocket_state().await;

    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let stream_id = "stream-detached-subscribe-recipient".to_string();
    let _detached_session = store_resumable_test_session(
        state.as_ref(),
        DetachedSession {
            stream_id: stream_id.clone(),
            user_id: "alice@example.com".to_string(),
            jid: alice_jid.clone(),
            inbound_count: 0,
            shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: true,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        },
    )
    .await;

    let mut bob = WsConnState::new();
    bob.phase = ConnectionPhase::ready(bob_jid, false);
    let _ = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client" type="subscribe" to="alice@example.com"/>"#,
        "example.com",
        state.as_ref(),
        &mut bob,
    )
    .await;

    let mut resumed = WsConnState::new();
    resumed.phase = ConnectionPhase::authenticated(&alice_jid);
    let resume_frame = resume_frame_xml(&stream_id, 0);
    let replay =
        handle_xmpp_frame(&resume_frame, "example.com", state.as_ref(), &mut resumed).await;
    assert!(
        replay.iter().any(|frame| {
            frame.contains("type='subscribe'") && frame.contains("from='bob@example.com'")
        }),
        "detached available recipient should replay inbound subscribe: {replay:?}"
    );
}

#[tokio::test]
async fn presence_broadcast_to_detached_available_subscriber_replays_on_resume() {
    use waddle_xmpp::stream_management::DetachedSession;
    let state = create_test_websocket_state().await;

    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");

    let mut bob = WsConnState::new();
    bob.phase = ConnectionPhase::ready(bob_jid.clone(), false);
    let _ = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client" type="subscribe" to="alice@example.com"/>"#,
        "example.com",
        state.as_ref(),
        &mut bob,
    )
    .await;
    let mut alice = WsConnState::new();
    alice.phase = ConnectionPhase::ready(alice_jid.clone(), false);
    let _ = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client" type="subscribed" to="bob@example.com"/>"#,
        "example.com",
        state.as_ref(),
        &mut alice,
    )
    .await;

    let stream_id = "stream-detached-presence-broadcast".to_string();
    let _detached_session = store_resumable_test_session(
        state.as_ref(),
        DetachedSession {
            stream_id: stream_id.clone(),
            user_id: "bob@example.com".to_string(),
            jid: bob_jid.clone(),
            inbound_count: 0,
            shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: true,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        },
    )
    .await;

    let _ = handle_xmpp_frame(
            r#"<presence xmlns="jabber:client"><show>away</show><status>broadcast while detached</status><priority>5</priority></presence>"#,
            "example.com",
            state.as_ref(),
            &mut alice,
        )
        .await;

    let mut resumed = WsConnState::new();
    resumed.phase = ConnectionPhase::authenticated(&bob_jid);
    let resume_frame = resume_frame_xml(&stream_id, 0);
    let replay =
        handle_xmpp_frame(&resume_frame, "example.com", state.as_ref(), &mut resumed).await;
    assert!(
        replay.iter().any(|frame| {
            frame.contains("from='alice@example.com/web'")
                && frame.contains("<show>away</show>")
                && frame.contains("<status>broadcast while detached</status>")
                && frame.contains("<priority>5</priority>")
        }),
        "detached available subscriber should replay presence broadcast: {replay:?}"
    );
}

#[tokio::test]
async fn sm_resume_with_unknown_stream_id_fails() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    conn.phase = ConnectionPhase::authenticated(&jid);
    let frame = resume_frame_xml("does-not-exist", 0);
    let responses = handle_xmpp_frame(&frame, "example.com", state.as_ref(), &mut conn).await;
    assert_eq!(responses.len(), 1);
    let el = Element::from_str(&responses[0]).expect("xml");
    assert_eq!(el.name(), "failed");
    assert!(el
        .get_child("item-not-found", "urn:ietf:params:xml:ns:xmpp-stanzas")
        .is_some());
    // Must NOT mark the session as bound/resumed.
    assert!(conn.phase.is_authenticated());
    assert!(!conn.phase.is_ready());
    assert!(!conn.phase.is_resumed());
    assert_eq!(
        metrics.counter_sum("xmpp.sm.resume.results", &[("outcome", "not_found")]),
        Some(1),
        "one failed resume attempt must emit exactly one not_found result"
    );
}

#[tokio::test]
async fn sm_resume_signals_suppress_record_so_main_loop_skips_replay() {
    // Regression guard for the double-record bug reported in PR review:
    // `handle_sm_resume` must request suppression of outbound recording
    // for its own response batch. Replayed stanzas are already in the
    // unacked queue — re-recording them would bump `outbound_count` and
    // create duplicates.
    use waddle_xmpp::stream_management::DetachedSession;
    let state = create_test_websocket_state().await;

    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let stream_id = "stream-dup-check".to_string();
    let detached = DetachedSession {
        stream_id: stream_id.clone(),
        user_id: "alice@example.com".to_string(),
        jid: jid.clone(),
        inbound_count: 0,
        shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
        outbound_count: 2,
        last_acked: 0,
        replay_gap_through: None,
        unacked_stanzas: vec![
            waddle_xmpp::stream_management::DetachedUnackedStanza {
                sequence: 1,
                stanza_xml: "<message xmlns='jabber:client' id='m1'/>".to_string(),
                original_receipt_at: chrono::Utc::now(),
            },
            waddle_xmpp::stream_management::DetachedUnackedStanza {
                sequence: 2,
                stanza_xml: "<message xmlns='jabber:client' id='m2'/>".to_string(),
                original_receipt_at: chrono::Utc::now(),
            },
        ],
        max_resume_time: Some(300),
        detached_at: std::time::Instant::now(),
        carbons_enabled: false,
        roster_interested: false,
        blocklist_interested: false,
        presence_available: false,
        presence_show: None,
        presence_status: None,
        presence_priority: 0,
        presence_payloads: Vec::new(),
        pending_subscribes_flushed: false,
    };
    let _detached_session = store_resumable_test_session(state.as_ref(), detached).await;

    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::authenticated(&jid);
    let frame = resume_frame_xml(&stream_id, 0);
    let _ = handle_xmpp_frame(&frame, "example.com", state.as_ref(), &mut conn).await;

    // The resume handler must have raised the suppress flag so the main
    // loop skips re-recording its own response batch.
    assert!(
        conn.suppress_sm_record_next_batch,
        "handle_sm_resume must ask the main loop to skip SM recording for this batch"
    );
    // And the restored counters must still reflect what the client had
    // acknowledged, not the inflated post-re-record values (2, not 4).
    assert_eq!(conn.sm_state.outbound_count, 2);
    assert_eq!(conn.sm_state.queue_len(), 2);
}

#[tokio::test]
async fn cleanup_shutdown_detaches_resumable_session_on_transport_drop() {
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "detached-channel@muc.example.com".parse().expect("room");
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &jid,
        "alice",
        None,
        &Some(owner_session.clone()),
    )
    .await;
    let (tx, mut rx) = mpsc::channel::<OutboundStanza>(4);
    let owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), tx);
    state
        .deps
        .protocol
        .connection_registry
        .update_presence(&jid, true, 0);
    state
        .deps
        .protocol
        .connection_registry
        .update_presence_state(
            &jid,
            Some("away".to_string()),
            Some("stepped out".to_string()),
            3,
            Vec::new(),
        );

    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    conn.authenticated_session = Some(owner_session.clone());
    conn.registry_owner = Some(owner);
    conn.roster_interested = true;
    conn.sm_state
        .enable("stream-detach".to_string(), true, Some(300));
    state
        .deps
        .protocol
        .connection_registry
        .send_to(
            &jid,
            Stanza::Presence(xmpp_parsers::presence::Presence::new(
                xmpp_parsers::presence::Type::None,
            )),
        )
        .await;

    let _ = cleanup_connection_shutdown(state.as_ref(), &mut rx, &mut conn, false).await;

    assert!(!state.deps.protocol.connection_registry.is_connected(&jid));
    let detached = state
        .deps
        .protocol
        .sm_session_registry
        .take_session("stream-detach")
        .await
        .expect("registry lookup");
    let detached = detached.expect("detached session");
    assert!(
        detached.roster_interested,
        "detached session must preserve roster-interest state"
    );
    assert!(
        detached.presence_available,
        "detached session must preserve available-presence state"
    );
    assert_eq!(
        detached.presence_show,
        Some(xmpp_parsers::presence::Show::Away)
    );
    assert_eq!(detached.presence_status.as_deref(), Some("stepped out"));
    assert_eq!(detached.presence_priority, 3);
    assert!(
        detached
            .unacked_stanzas
            .iter()
            .any(|entry| entry.stanza_xml.contains("<presence")),
        "cleanup must record queued-but-unwritten outbound stanzas before detaching"
    );
    assert!(snapshot_room(state.as_ref(), &room_jid)
        .await
        .room
        .find_nick_by_real_jid(&jid)
        .is_some());
}

/// ADR-0017 Phase 1 (Greptile review on PR #1177): an SM detach prunes the
/// resource's actor-tree entry as well as its DashMap entry. Without this, a
/// session that detaches and then expires without ever resuming would leak its
/// `UserActor` entry forever — the SM-expiry janitor cannot converge it because
/// the DashMap entry is already gone at detach, so its removal-gated mirror
/// never fires. Detached delivery is unaffected (it is sourced from the SM
/// session registry, not the actor), and a resume re-registers a fresh entry.
#[tokio::test]
async fn cleanup_shutdown_detach_prunes_actor_tree_entry() {
    use waddle_xmpp::registry::GetUser;
    let state = create_test_websocket_state().await;
    let owner_session = create_test_session(state.as_ref(), "alice").await;
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let (tx, mut rx) = mpsc::channel::<OutboundStanza>(4);
    let owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), tx);
    // Mirror into the actor tree exactly as the production bind path does,
    // sharing the same Arc-backed entry.
    let entry = state
        .deps
        .protocol
        .connection_registry
        .get_entry(&jid)
        .expect("entry just registered");
    assert!(
        crate::server::dual_registration::mirror_register(
            &state.deps.protocol.user_registry,
            jid.clone(),
            entry,
        )
        .await,
        "actor mirror register should confirm the resource"
    );
    state
        .deps
        .protocol
        .connection_registry
        .update_presence(&jid, true, 0);

    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    conn.authenticated_session = Some(owner_session);
    conn.registry_owner = Some(owner);
    conn.sm_state
        .enable("stream-detach-actor".to_string(), true, Some(300));

    let _ = cleanup_connection_shutdown(state.as_ref(), &mut rx, &mut conn, false).await;

    // DashMap entry removed AND the resumable session stored (detached).
    assert!(!state.deps.protocol.connection_registry.is_connected(&jid));
    let stored = state
        .deps
        .protocol
        .sm_session_registry
        .peek_session("stream-detach-actor")
        .await
        .expect("registry lookup");
    assert!(stored.is_some(), "detach stores the resumable session");

    // The actor-tree entry for this bare JID is pruned (its only resource was
    // removed, so the UserActor reports empty and is pruned). GetUser is
    // FIFO-ordered after the unregister mirror on the same registry mailbox, so
    // this observes the pruned state deterministically — no leak.
    let user = state
        .deps
        .protocol
        .user_registry
        .ask(GetUser {
            bare_jid: jid.to_bare(),
        })
        .await
        .expect("get user");
    assert!(
        user.is_none(),
        "SM detach must prune the actor-tree entry, not leak it until expiry"
    );
}

#[tokio::test]
async fn cleanup_shutdown_does_not_detach_explicit_close() {
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "closing-channel@muc.example.com".parse().expect("room");
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &jid,
        "alice",
        None,
        &Some(owner_session.clone()),
    )
    .await;
    let (tx, mut rx) = mpsc::channel::<OutboundStanza>(4);
    let owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), tx);

    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    conn.authenticated_session = Some(owner_session.clone());
    conn.registry_owner = Some(owner);
    conn.sm_state
        .enable("stream-close".to_string(), false, Some(300));

    let _ = cleanup_connection_shutdown(state.as_ref(), &mut rx, &mut conn, false).await;

    assert!(!state.deps.protocol.connection_registry.is_connected(&jid));
    let detached = state
        .deps
        .protocol
        .sm_session_registry
        .take_session("stream-close")
        .await
        .expect("registry lookup");
    assert!(
        detached.is_none(),
        "explicit <close/> must not leave a resumable detached session behind"
    );
    assert!(snapshot_room(state.as_ref(), &room_jid)
        .await
        .room
        .find_nick_by_real_jid(&jid)
        .is_none());
}

#[tokio::test]
async fn cleanup_shutdown_does_not_unregister_replacement_session() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let (old_tx, mut old_rx) = mpsc::channel::<OutboundStanza>(4);
    let (new_tx, _new_rx) = mpsc::channel::<OutboundStanza>(4);

    let old_owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), old_tx);
    let new_owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), new_tx);

    let mut old_conn = WsConnState::new();
    old_conn.phase = ConnectionPhase::ready(jid.clone(), false);
    old_conn.registry_owner = Some(old_owner);

    let _ = cleanup_connection_shutdown(state.as_ref(), &mut old_rx, &mut old_conn, false).await;

    assert!(
        state.deps.protocol.connection_registry.is_connected(&jid),
        "cleanup for a replaced connection must leave the replacement registered"
    );
    assert!(
        state
            .deps
            .protocol
            .connection_registry
            .unregister_if_owner(&jid, &new_owner)
            .is_some(),
        "the remaining registry owner should be the replacement session"
    );
}

#[tokio::test]
async fn terminal_cleanup_skips_shared_fulljid_teardown_after_replacement_rejoins() {
    let sm_registry = Arc::new(
        waddle_xmpp::stream_management::InMemorySmSessionRegistry::new().with_claim_store(
            Arc::new(waddle_xmpp::ownership::InProcessClaimStore::new()),
            waddle_xmpp::ownership::SharedNodeIdentity::new(
                waddle_xmpp::ownership::NodeIdentity::new(
                    "terminal-replacement-node",
                    "incarnation",
                ),
            ),
        ),
    );
    let pending_storage = Arc::new(GatedFirstInsertPendingStorage::new());
    let state = create_test_websocket_state_with_sm_registry_and_pending_storage(
        Arc::clone(&sm_registry),
        pending_storage.clone(),
    )
    .await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "terminal-replacement@muc.example.com"
        .parse()
        .expect("room");
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let sibling_jid: FullJid = "alice@example.com/laptop".parse().expect("sibling jid");
    let (sibling_tx, mut sibling_rx) = mpsc::channel::<OutboundStanza>(4);
    state
        .deps
        .protocol
        .connection_registry
        .register(sibling_jid.clone(), sibling_tx);
    state
        .deps
        .protocol
        .connection_registry
        .update_presence(&sibling_jid, true, -1);

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &jid,
        "alice",
        None,
        &Some(owner_session.clone()),
    )
    .await;

    let (tx, rx) = mpsc::channel::<OutboundStanza>(4);
    let owner = super::register_test_connection(state.as_ref(), &jid, tx).await;
    state
        .deps
        .protocol
        .connection_registry
        .update_presence(&jid, true, 0);
    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    conn.registry_owner = Some(owner);
    conn.sm_state
        .enable("terminal-replacement-stream".to_string(), true, Some(300));
    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(jid.to_bare())));
    message.from = Some("bob@example.com/phone".parse().expect("sender jid"));
    message.id = Some(xmpp_parsers::message::Id(
        "terminal-replacement-msg".to_string(),
    ));
    message.type_ = xmpp_parsers::message::MessageType::Chat;
    message
        .bodies
        .insert(xmpp_parsers::message::Lang::new(), "promote me".to_string());
    let _ = conn.sm_state.record_outbound(
        waddle_xmpp::parser::stanza_to_string(message).expect("serialize queued message"),
        SmEvictionPath::DirectOutbound,
    );
    conn.begin_terminal_sm_recovery();

    let state_for_cleanup = Arc::clone(&state);
    let cleanup_task = tokio::spawn(async move {
        let mut rx = rx;
        let mut conn = conn;
        cleanup_connection_shutdown(state_for_cleanup.as_ref(), &mut rx, &mut conn, false).await
    });

    pending_storage.wait_until_insert_blocks().await;

    let (_replacement_tx, _replacement_rx) = mpsc::channel::<OutboundStanza>(4);
    super::register_test_connection(state.as_ref(), &jid, _replacement_tx).await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &jid,
        "alice",
        None,
        &Some(owner_session),
    )
    .await;

    pending_storage.release_insert();

    assert_eq!(
        cleanup_task.await.expect("cleanup join"),
        super::super::cleanup::ConnectionShutdownOutcome::NotPersisted
    );
    assert!(
        state.deps.protocol.connection_registry.is_connected(&jid),
        "replacement bind must survive the predecessor's terminal cleanup"
    );
    let unavailable =
        tokio::time::timeout(std::time::Duration::from_millis(250), sibling_rx.recv())
            .await
            .expect("silent replacement must not suppress predecessor unavailable")
            .expect("sibling channel open");
    let unavailable_xml = stanza_to_xml(&unavailable.stanza);
    assert!(
        unavailable_xml.contains("from='alice@example.com/web'")
            && unavailable_xml.contains("type='unavailable'"),
        "terminal cleanup must broadcast unavailable past a silent replacement: {unavailable_xml}"
    );
    assert!(
        snapshot_room(state.as_ref(), &room_jid)
            .await
            .room
            .find_nick_by_real_jid(&jid)
            .is_some(),
        "replacement rejoin must keep its room occupancy after the old stream finishes promotion"
    );
    let promoted = state
        .deps
        .protocol
        .pending_delivery_storage
        .list(&jid.to_bare())
        .await
        .expect("list promoted rows");
    assert!(
        promoted.iter().any(|row| {
            matches!(
                &row.payload,
                waddle_xmpp::pending_delivery::PendingPayload::Transient(message)
                    if message
                        .id
                        .as_ref()
                        .is_some_and(|id| id.0 == "terminal-replacement-msg")
            )
        }),
        "terminal cleanup must still promote the recorded queue"
    );
}

#[tokio::test]
async fn terminal_cleanup_without_replacement_still_tears_down_room_membership() {
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "terminal-no-replacement@muc.example.com"
        .parse()
        .expect("room");
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &jid,
        "alice",
        None,
        &Some(owner_session),
    )
    .await;

    let (tx, mut rx) = mpsc::channel::<OutboundStanza>(4);
    let owner = super::register_test_connection(state.as_ref(), &jid, tx).await;
    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    conn.registry_owner = Some(owner);
    conn.sm_state.enable(
        "terminal-no-replacement-stream".to_string(),
        true,
        Some(300),
    );
    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(jid.to_bare())));
    message.from = Some("bob@example.com/phone".parse().expect("sender jid"));
    message.id = Some(xmpp_parsers::message::Id(
        "terminal-no-replacement-msg".to_string(),
    ));
    message.type_ = xmpp_parsers::message::MessageType::Chat;
    message
        .bodies
        .insert(xmpp_parsers::message::Lang::new(), "promote me".to_string());
    let _ = conn.sm_state.record_outbound(
        waddle_xmpp::parser::stanza_to_string(message).expect("serialize queued message"),
        SmEvictionPath::DirectOutbound,
    );

    assert_eq!(
        cleanup_connection_shutdown(state.as_ref(), &mut rx, &mut conn, false).await,
        super::super::cleanup::ConnectionShutdownOutcome::NotPersisted
    );
    assert!(
        !state.deps.protocol.connection_registry.is_connected(&jid),
        "without a replacement the terminal cleanup must retire the old route"
    );
    assert!(
        snapshot_room(state.as_ref(), &room_jid)
            .await
            .room
            .find_nick_by_real_jid(&jid)
            .is_none(),
        "without a replacement the terminal cleanup must still remove the old room occupancy"
    );
    let promoted = state
        .deps
        .protocol
        .pending_delivery_storage
        .list(&jid.to_bare())
        .await
        .expect("list promoted rows");
    assert!(
        promoted.iter().any(|row| {
            matches!(
                &row.payload,
                waddle_xmpp::pending_delivery::PendingPayload::Transient(message)
                    if message
                        .id
                        .as_ref()
                        .is_some_and(|id| id.0 == "terminal-no-replacement-msg")
            )
        }),
        "terminal cleanup without a principal must still promote its recorded queue"
    );
}

#[tokio::test]
async fn sm_janitor_helper_drains_expired_and_cleans_muc() {
    // Exercise the pieces the janitor composes: drain_expired() returns
    // the removed sessions, and cleanup_muc_presence_for_jid removes the
    // occupant that was held while the session was detached.
    use waddle_xmpp::stream_management::DetachedSession;
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "expired-channel@muc.example.com".parse().expect("room");
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");

    // Put alice in the room, as if she'd detached with SM.
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &jid,
        "alice",
        None,
        &Some(owner_session.clone()),
    )
    .await;
    assert!(snapshot_room(state.as_ref(), &room_jid)
        .await
        .room
        .find_nick_by_real_jid(&jid)
        .is_some());

    // Seed an immediately-expired detached session for that JID.
    let stream_id = "already-expired".to_string();
    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id: stream_id.clone(),
            user_id: "alice@example.com".to_string(),
            jid: jid.clone(),
            inbound_count: 0,
            shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(0), // already expired
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        })
        .await
        .expect("store");

    // Wait a hair so the 0-second TTL is definitely in the past.
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let drained = state
        .deps
        .protocol
        .sm_session_registry
        .drain_expired()
        .await
        .expect("drain");
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].stream_id, stream_id);

    // The janitor body: remove the MUC occupant + any routing slot.
    state
        .deps
        .protocol
        .connection_registry
        .unregister(&drained[0].jid);
    cleanup_muc_presence_for_jid(state.as_ref(), &drained[0].jid).await;

    assert!(
        snapshot_room(state.as_ref(), &room_jid)
            .await
            .room
            .find_nick_by_real_jid(&jid)
            .is_none(),
        "MUC occupant must be gone after janitor sweep"
    );
}

#[tokio::test]
async fn sm_resume_replay_stamps_xep0203_delay_with_original_receipt_time() {
    // Issue #1178: stanzas replayed after <resumed/> must carry a
    // XEP-0203 <delay/> whose stamp is the ORIGINAL server receipt
    // time — otherwise clients timestamp them at drain time and sort
    // them to the bottom of the timeline.
    use chrono::{TimeZone, Utc};
    use waddle_xmpp::stream_management::{DetachedSession, DetachedUnackedStanza};

    let state = create_test_websocket_state().await;
    let domain = state.deps.auth_state.xmpp_domain.clone();
    let session = create_test_session(state.as_ref(), "bob").await;
    let payload = BASE64_STANDARD.encode(format!("n,,\x01auth=Bearer {}\x01\x01", session.id));
    let auth_frame = element_to_xml(
        Element::builder("auth", waddle_xmpp::ns::SASL)
            .attr(
                minidom::rxml::xml_ncname!("mechanism").to_owned(),
                "OAUTHBEARER",
            )
            .append(payload)
            .build(),
    );
    let mut conn = WsConnState::new();
    let auth_responses = handle_xmpp_frame(&auth_frame, &domain, state.as_ref(), &mut conn).await;
    assert_eq!(auth_responses, vec![sasl_success_xml()]);

    let original_receipt = Utc.with_ymd_and_hms(2026, 7, 1, 9, 15, 30).unwrap();
    let detached_jid: FullJid = format!("bob@{domain}/web").parse().expect("jid");
    let queued_message_xml = {
        let mut message =
            xmpp_parsers::message::Message::new(Some(jid::Jid::from(detached_jid.clone())));
        message.from = Some(
            format!("alice@{domain}/a")
                .parse::<jid::Jid>()
                .expect("jid"),
        );
        message.type_ = xmpp_parsers::message::MessageType::Chat;
        message.id = Some(xmpp_parsers::message::Id("replayed-1".to_string()));
        message.bodies.insert(
            xmpp_parsers::message::Lang::new(),
            "while you were away".to_string(),
        );
        stanza_to_xml(&Stanza::Message(message))
    };
    let queued_iq_xml = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("from").to_owned(),
                domain.as_str(),
            )
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                detached_jid.to_string(),
            )
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "replayed-iq")
            .build(),
    );
    store_resumable_detached_session(
        state.as_ref(),
        &session,
        DetachedSession {
            stream_id: "stream-replay-delay".to_string(),
            user_id: format!("bob@{domain}"),
            jid: detached_jid.clone(),
            inbound_count: 1,
            shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
            outbound_count: 2,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: vec![
                DetachedUnackedStanza {
                    sequence: 1,
                    stanza_xml: queued_message_xml,
                    original_receipt_at: original_receipt,
                },
                DetachedUnackedStanza {
                    sequence: 2,
                    stanza_xml: queued_iq_xml,
                    original_receipt_at: original_receipt,
                },
            ],
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        },
    )
    .await;

    let resume_frame = resume_frame_xml("stream-replay-delay", 0);
    let responses = handle_xmpp_frame(&resume_frame, &domain, state.as_ref(), &mut conn).await;

    assert_eq!(responses.len(), 3, "<resumed/> + 2 replayed stanzas");
    let resumed = Element::from_str(&responses[0]).expect("xml");
    assert_eq!(resumed.name(), "resumed");

    // The replayed <message/> carries the server's delay stamp with the
    // original receipt time, not the resume time.
    let replayed_message = Element::from_str(&responses[1]).expect("replayed message xml");
    assert_eq!(replayed_message.name(), "message");
    let delay = replayed_message
        .children()
        .find(|child| child.name() == "delay" && child.ns() == "urn:xmpp:delay")
        .expect("replayed message must carry a XEP-0203 delay");
    assert_eq!(delay.attr("from"), Some(domain.as_str()));
    assert_eq!(delay.attr("stamp"), Some("2026-07-01T09:15:30Z"));

    // The replayed <iq/> stays unstamped — XEP-0203 covers message and
    // presence only.
    let replayed_iq = Element::from_str(&responses[2]).expect("replayed iq xml");
    assert_eq!(replayed_iq.name(), "iq");
    assert!(
        !replayed_iq.children().any(|child| child.name() == "delay"),
        "iq replay must not gain a delay element"
    );
}

#[tokio::test]
async fn sm_detach_on_transport_drop_does_not_evict_sfu_call_session() {
    // #935 decided behavior: presence loss must never end a healthy
    // LiveKit session — an SM-resumable transport drop keeps the MUC
    // occupant slot, and the SFU participant must survive with it.
    // Only involuntary moderation (kick 307 / ban 301) or terminal
    // session death may tear the call down.
    let recorder = std::sync::Arc::new(super::RecordingSfu::default());
    let state = super::create_test_websocket_state_with_sfu(recorder.clone()).await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "detach-keeps-call@muc.example.com".parse().expect("room");
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &jid,
        "alice",
        None,
        &Some(owner_session.clone()),
    )
    .await;
    let (tx, mut rx) = mpsc::channel::<OutboundStanza>(4);
    let owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), tx);

    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    conn.authenticated_session = Some(owner_session.clone());
    conn.registry_owner = Some(owner);
    conn.sm_state
        .enable("stream-detach-call".to_string(), true, Some(300));

    let _ = cleanup_connection_shutdown(state.as_ref(), &mut rx, &mut conn, false).await;

    assert!(
        recorder.snapshot().is_empty(),
        "resumable SM detach must not evict the SFU participant"
    );
    assert!(
        recorder.note_snapshot().is_empty(),
        "resumable SM detach must not touch SFU bookkeeping either"
    );
    assert!(
        snapshot_room(state.as_ref(), &room_jid)
            .await
            .room
            .find_nick_by_real_jid(&jid)
            .is_some(),
        "occupant slot survives the detach"
    );
}

/// Council-adjudicated FIX 3: race `attempt_cross_node_resume` against this
/// node's graceful-shutdown token, Postgres-gated (needs a real
/// `PostgresClaimStore` foreign claim so `attempt_cross_node_resume`'s
/// retry loop actually runs, rather than short-circuiting to `NotFound`).
/// Skipped (not failed) when `WADDLE_TEST_POSTGRES_URL` is unset, mirroring
/// every other Postgres-gated test in this crate.
#[cfg(feature = "clustering")]
mod fix3_shutdown_race {
    use super::*;
    use crate::clustering::claims::{clustering_control_plane_table_lock, PostgresClaimStore};
    use crate::clustering::ClusteringHandles;
    use crate::db::{Database, DatabaseConfig, DatabaseDriver, DEFAULT_CONTROL_PLANE_POOL_SIZE};
    use crate::server::routes::websocket::tests::create_test_websocket_state_with_clustering;
    use std::sync::Arc;
    use std::time::Duration;
    use waddle_xmpp::ownership::{
        ClaimStore, Entity, EntityType, NodeIdentity, SharedNodeIdentity,
    };
    use waddle_xmpp::stream_management::{
        InMemorySmSessionRegistry, RemoteResumeAskOutcome, RemoteResumeAsker,
    };

    fn node_identity() -> NodeIdentity {
        NodeIdentity::new(
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
        )
    }

    /// Never resolves — the asker equivalent of a permanently wedged owner.
    /// Without FIX 3, a resume attempt against this asker would hold this
    /// connection's own graceful shutdown hostage for the entire
    /// (here, deliberately generous) handshake budget.
    struct HangingAsker;

    #[async_trait::async_trait]
    impl RemoteResumeAsker for HangingAsker {
        async fn ask_remote_detach(
            &self,
            _node_id: &str,
            _stream_id: &str,
            _requester_bare_jid: &BareJid,
        ) -> RemoteResumeAskOutcome {
            std::future::pending().await
        }
    }

    #[tokio::test]
    async fn shutdown_mid_resume_abandons_the_attempt_without_waiting_out_the_budget() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Ok(url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
            eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set");
            return;
        };
        let db = Database::from_config(
            "fix3-shutdown-race-test",
            &DatabaseConfig::new(DatabaseDriver::Postgres, url)
                .with_control_plane_pool(DEFAULT_CONTROL_PLANE_POOL_SIZE),
        )
        .await
        .expect("open test postgres");
        let claim_store = PostgresClaimStore::new(db.clone());
        claim_store
            .ensure_schema()
            .await
            .expect("ensure claims schema");
        {
            let conn = db.guard().await.expect("guard");
            conn.execute("DELETE FROM clustering_claims", ())
                .await
                .expect("clean claims");
            conn.execute("DELETE FROM clustering_nodes", ())
                .await
                .expect("clean nodes");
        }

        // A foreign, live claim: owned by a different node than the one
        // about to attempt the resume, so `attempt_cross_node_resume`
        // actually dispatches into branch 2/3 (live handshake) instead of
        // short-circuiting.
        let owner = node_identity();
        let entity = Entity::new(EntityType::SmSession, "stream-shutdown-race".to_string());
        claim_store
            .acquire(&entity, &owner)
            .await
            .expect("owner claims the entity");

        let resuming_identity = node_identity();
        let resuming_identity_handle = SharedNodeIdentity::new(resuming_identity.clone());
        let resuming_claim_store: Arc<dyn ClaimStore> =
            Arc::new(PostgresClaimStore::new(db.clone()));
        let sm_session_registry = Arc::new(
            InMemorySmSessionRegistry::new()
                .with_claim_store(
                    Arc::clone(&resuming_claim_store),
                    resuming_identity_handle.clone(),
                )
                .with_remote_resume_asker(Arc::new(HangingAsker)),
        );

        // Deliberately generous: if FIX 3's shutdown race did not work,
        // this test would need to wait out the whole budget before
        // observing the (wrong) outcome — this value is what proves the
        // difference.
        let handshake_budget = Duration::from_secs(30);
        let clustering = ClusteringHandles {
            claim_store: Some(Arc::clone(&resuming_claim_store)),
            node_identity: Some(resuming_identity_handle),
            local_claims: None,
            room_local_claims: None,
            user_local_claims: None,
            muc_durable_store: None,
            node_lease: None,
            lease_ttl: None,
            pod_template_hash: None,
            resume_bridge: None,
            ordered_relay_delivery_bridge: None,
            stop_token: None,
            fatal_fence: None,
            resume_handshake_timeout: Some(handshake_budget),
        };

        let mut state =
            create_test_websocket_state_with_clustering(clustering, sm_session_registry).await;
        let graceful = waddle_ecdysis::GracefulShutdown::new(Duration::from_secs(5));
        Arc::get_mut(&mut state)
            .expect("sole owner immediately after construction")
            .deps
            .shutdown = graceful.handle();

        let jid: FullJid = "alice@example.com/phone".parse().expect("valid jid");
        let frame = resume_frame_xml("stream-shutdown-race", 0);
        let state_for_task = Arc::clone(&state);
        let handle = tokio::spawn(async move {
            let mut conn = WsConnState::new();
            conn.phase = ConnectionPhase::authenticated(&jid);
            let started = std::time::Instant::now();
            let responses =
                handle_xmpp_frame(&frame, "example.com", state_for_task.as_ref(), &mut conn).await;
            (started.elapsed(), responses)
        });

        // Give the spawned resume attempt a moment to actually enter its
        // held-response retry loop (past the `current_claim`/persistence
        // reads, into the `HangingAsker` ask) before shutdown fires.
        tokio::time::sleep(Duration::from_millis(200)).await;
        graceful.trigger_stop();

        let (elapsed, responses) = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("the resume attempt must return promptly once shutdown fires")
            .expect("task must not panic");

        assert!(
            elapsed < handshake_budget,
            "resume attempt took {elapsed:?}, which did not plausibly finish faster than \
             the {handshake_budget:?} budget — shutdown did not preempt the held resume"
        );
        assert!(
            responses.is_empty(),
            "an abandoned-on-shutdown resume must send no response of its own — the \
             connection's own system-shutdown close is the actual signal to the client; \
             got {responses:?}"
        );
    }
}

/// ADR-0017 Phase 3 deviation 55 (FIX A): a second adversarial convergence
/// pass found deviation 47's shutdown race (`fix3_shutdown_race`, above)
/// was unsound past the CAS itself — `tokio::select!` could drop the whole
/// `attempt_cross_node_resume` future between `steal_for_resume` committing
/// in Postgres and `hydrate_reclaimed`/`claim_session` completing, stranding
/// a self-owned, un-hydrated claim. The fix splits the call at its write
/// boundary (`prepare_cross_node_resume` + `finish_cross_node_steal`) and
/// only races the read-only `prepare` half; `finish_cross_node_steal` is
/// never raced once reached. This module proves that end-to-end through
/// `handle_xmpp_frame`, Postgres-gated (needs a real `PostgresClaimStore` so
/// `steal_for_resume` genuinely commits, and a real `PostgresFencedSmPersistence`
/// so `hydrate_reclaimed` has somewhere to read from).
#[cfg(feature = "clustering")]
mod fix_a_post_cas_shutdown {
    use super::*;
    use crate::clustering::claims::{clustering_control_plane_table_lock, PostgresClaimStore};
    use crate::clustering::ClusteringHandles;
    use crate::db::{Database, DatabaseConfig, DatabaseDriver, DEFAULT_CONTROL_PLANE_POOL_SIZE};
    use crate::server::routes::websocket::tests::create_test_websocket_state_with_clustering;
    use crate::sm_persistence_fenced::PostgresFencedSmPersistence;
    use std::sync::Arc;
    use std::time::Duration;
    use waddle_xmpp::ownership::{
        ClaimEpoch, ClaimError, ClaimSnapshot, ClaimStore, Entity, NodeIdentity,
        ResumeIdentityProof, SharedNodeIdentity, StalePredicate,
    };
    use waddle_xmpp::stream_management::{DetachedSession, InMemorySmSessionRegistry};

    fn node_identity() -> NodeIdentity {
        NodeIdentity::new(
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
        )
    }

    /// `ClaimStore` test double: delegates every method to a real
    /// `PostgresClaimStore`, except `ensure_claimed`, which notifies
    /// `arrived` and then waits on `release_gate` before delegating. In
    /// this test's flow, `hydrate_reclaimed`'s own internal self-reacquire
    /// `ensure_claimed` call — issued only AFTER `steal_for_resume` has
    /// already won — is the sole call this double ever sees, giving the
    /// test a precise, deterministic window "the CAS has committed, the
    /// finish sequence is mid-flight" to fire shutdown into.
    struct GatedEnsureClaimedStore {
        inner: Arc<dyn ClaimStore>,
        arrived: Arc<tokio::sync::Notify>,
        release_gate: Arc<tokio::sync::Notify>,
        /// Only the FIRST `ensure_claimed` call (`hydrate_reclaimed`'s own
        /// post-CAS-win self-reacquire) gates — `claim_session`'s own
        /// subsequent self-reacquire call must pass straight through, or
        /// this double would need a second `release_gate` notification the
        /// test never sends.
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl ClaimStore for GatedEnsureClaimedStore {
        async fn ensure_schema(&self) -> Result<(), ClaimError> {
            self.inner.ensure_schema().await
        }

        async fn acquire(
            &self,
            entity: &Entity,
            me: &NodeIdentity,
        ) -> Result<ClaimEpoch, ClaimError> {
            self.inner.acquire(entity, me).await
        }

        async fn ensure_claimed(
            &self,
            entity: &Entity,
            me: &NodeIdentity,
        ) -> Result<ClaimEpoch, ClaimError> {
            let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if call == 0 {
                self.arrived.notify_one();
                self.release_gate.notified().await;
            }
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

        async fn current_claim(
            &self,
            entity: &Entity,
        ) -> Result<Option<ClaimSnapshot>, ClaimError> {
            self.inner.current_claim(entity).await
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

        async fn release_many(
            &self,
            entities: &[Entity],
            me: &NodeIdentity,
        ) -> Result<(), ClaimError> {
            self.inner.release_many(entities, me).await
        }
    }

    #[tokio::test]
    async fn shutdown_firing_mid_finish_does_not_abandon_an_already_won_steal() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Ok(url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
            eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set");
            return;
        };
        let db = Database::from_config(
            "fix-a-post-cas-shutdown-test",
            &DatabaseConfig::new(DatabaseDriver::Postgres, url)
                .with_control_plane_pool(DEFAULT_CONTROL_PLANE_POOL_SIZE),
        )
        .await
        .expect("open test postgres");
        {
            let claim_store = PostgresClaimStore::new(db.clone());
            claim_store
                .ensure_schema()
                .await
                .expect("ensure claims schema");
            // Provision the SM schema BEFORE cleaning it: under CI's fresh
            // per-run Postgres this test can be the first to touch
            // sm_sessions/sm_unacked, and a bare DELETE against a
            // not-yet-created table fails 42P01 (caught by nixTest on the
            // Slice 7 push — locally another suite always ran first).
            let schema_identity = SharedNodeIdentity::new(node_identity());
            let schema_claims: Arc<dyn ClaimStore> = Arc::new(PostgresClaimStore::new(db.clone()));
            let _schema_only = PostgresFencedSmPersistence::open(
                db.clone(),
                Arc::clone(&schema_claims),
                schema_identity,
            )
            .await
            .expect("provision sm schema");
            let conn = db.guard().await.expect("guard");
            conn.execute("DELETE FROM clustering_claims", ())
                .await
                .expect("clean claims");
            conn.execute("DELETE FROM clustering_nodes", ())
                .await
                .expect("clean nodes");
            conn.execute("DELETE FROM sm_unacked", ())
                .await
                .expect("clean sm_unacked");
            conn.execute("DELETE FROM sm_sessions", ())
                .await
                .expect("clean sm_sessions");
        }

        // Owner node: stores a real detached session via a real
        // `PostgresFencedSmPersistence`, so the resuming node below reads
        // a genuine persisted row (branch 1's fast path) rather than
        // needing a live-handshake asker at all.
        let owner_identity = SharedNodeIdentity::new(node_identity());
        let owner_claim_store: Arc<dyn ClaimStore> = Arc::new(PostgresClaimStore::new(db.clone()));
        let owner_persistence = PostgresFencedSmPersistence::open(
            db.clone(),
            Arc::clone(&owner_claim_store),
            owner_identity.clone(),
        )
        .await
        .expect("open owner fenced persistence");
        let owner_registry = InMemorySmSessionRegistry::new()
            .with_persistence(Arc::new(owner_persistence))
            .with_claim_store(owner_claim_store, owner_identity);

        let jid: FullJid = "alice@example.com/phone".parse().expect("valid full jid");
        let owner_detached = DetachedSession {
            stream_id: "stream-post-cas-shutdown".to_string(),
            user_id: "alice@example.com".to_string(),
            jid: jid.clone(),
            inbound_count: 0,
            shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        };

        // Resuming node: the one wired into the websocket state under
        // test. Its `ClaimStore` is gated on `ensure_claimed` — the call
        // `hydrate_reclaimed` issues right after `steal_for_resume` wins.
        let resuming_identity = node_identity();
        let resuming_identity_handle = SharedNodeIdentity::new(resuming_identity.clone());
        let real_claim_store: Arc<dyn ClaimStore> = Arc::new(PostgresClaimStore::new(db.clone()));
        let arrived = Arc::new(tokio::sync::Notify::new());
        let release_gate = Arc::new(tokio::sync::Notify::new());
        let gated_claim_store: Arc<dyn ClaimStore> = Arc::new(GatedEnsureClaimedStore {
            inner: Arc::clone(&real_claim_store),
            arrived: Arc::clone(&arrived),
            release_gate: Arc::clone(&release_gate),
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let resuming_persistence = PostgresFencedSmPersistence::open(
            db.clone(),
            Arc::clone(&gated_claim_store),
            resuming_identity_handle.clone(),
        )
        .await
        .expect("open resuming fenced persistence");
        let sm_session_registry = Arc::new(
            InMemorySmSessionRegistry::new()
                .with_persistence(Arc::new(resuming_persistence))
                .with_claim_store(
                    Arc::clone(&gated_claim_store),
                    resuming_identity_handle.clone(),
                ),
        );

        // Generous and, crucially, irrelevant to the outcome: branch 1's
        // persisted-snapshot fast path fires immediately (no remote ask
        // needed), and once `finish_cross_node_steal` starts it no longer
        // consults this budget at all (FIX A).
        let handshake_budget = Duration::from_secs(30);
        let clustering = ClusteringHandles {
            claim_store: Some(Arc::clone(&gated_claim_store)),
            node_identity: Some(resuming_identity_handle),
            local_claims: None,
            room_local_claims: None,
            user_local_claims: None,
            muc_durable_store: None,
            node_lease: None,
            lease_ttl: None,
            pod_template_hash: None,
            resume_bridge: None,
            ordered_relay_delivery_bridge: None,
            stop_token: None,
            fatal_fence: None,
            resume_handshake_timeout: Some(handshake_budget),
        };

        let mut state =
            create_test_websocket_state_with_clustering(clustering, sm_session_registry).await;
        let graceful = waddle_ecdysis::GracefulShutdown::new(Duration::from_secs(5));
        Arc::get_mut(&mut state)
            .expect("sole owner immediately after construction")
            .deps
            .shutdown = graceful.handle();
        // The durable fence authorizes resume against the sessions table:
        // seed the account + session row and persist the snapshot WITH its
        // principal, exactly as production detach does.
        let alice_session = super::create_test_session(state.as_ref(), "alice").await;
        owner_registry
            .store_session_with_principal(
                owner_detached,
                alice_session
                    .authenticated_principal_ref()
                    .expect("test session carries an auth context"),
            )
            .await
            .expect("owner stores the principal-carrying detached session");

        let frame = resume_frame_xml("stream-post-cas-shutdown", 0);
        let state_for_task = Arc::clone(&state);
        let handle = tokio::spawn(async move {
            let mut conn = WsConnState::new();
            conn.phase = ConnectionPhase::authenticated(&jid);
            handle_xmpp_frame(&frame, "example.com", state_for_task.as_ref(), &mut conn).await
        });

        // Wait until the finish sequence is genuinely mid-flight — PAST
        // `steal_for_resume`'s real Postgres commit, INSIDE
        // `hydrate_reclaimed`'s self-reacquire — before firing shutdown.
        tokio::time::timeout(Duration::from_secs(5), arrived.notified())
            .await
            .expect("hydrate_reclaimed's ensure_claimed must be reached promptly");
        graceful.trigger_stop();
        // Give the connection's own select loop a moment to observe the
        // (now-irrelevant, since `finish_cross_node_steal` is never raced)
        // cancelled token, proving it has no effect on the in-flight finish.
        tokio::time::sleep(Duration::from_millis(200)).await;
        release_gate.notify_one();

        let responses = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("the resume must complete promptly once the gate is released")
            .expect("task must not panic");

        assert_eq!(
            responses.len(),
            1,
            "shutdown firing mid-finish must not truncate/abandon the response; got {responses:?}"
        );
        let resumed = Element::from_str(&responses[0]).expect("xml");
        assert_eq!(
            resumed.name(),
            "resumed",
            "the already-won steal must complete to a real <resumed/>, not be dropped by the \
             shutdown token that fired mid-sequence; got {responses:?}"
        );
    }
}

/// XEP-0198 §4 counters are mod 2^32: a resume whose `h` sits just
/// behind a freshly wrapped `outbound_count` is VALID, not
/// handled-count-too-high (gpt-5.5 review follow-up to #1099 — the
/// live ack path was made wrap-aware, resume must agree).
#[tokio::test]
async fn sm_resume_accepts_handled_count_behind_wrapped_outbound() {
    use waddle_xmpp::stream_management::DetachedSession;
    let state = create_test_websocket_state().await;

    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let _detached_session = store_resumable_test_session(
        state.as_ref(),
        DetachedSession {
            stream_id: "stream-wrapped".to_string(),
            user_id: "alice@example.com".to_string(),
            jid: jid.clone(),
            inbound_count: 0,
            shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
            // The server's send counter wrapped past 2^32: it now
            // reads 2, while the client last handled u32::MAX.
            outbound_count: 2,
            last_acked: u32::MAX - 1,
            replay_gap_through: None,
            unacked_stanzas: vec![
                waddle_xmpp::stream_management::DetachedUnackedStanza {
                    sequence: u32::MAX,
                    stanza_xml: "<message xmlns='jabber:client' id='pre-wrap'/>".to_string(),
                    original_receipt_at: chrono::Utc::now(),
                },
                waddle_xmpp::stream_management::DetachedUnackedStanza {
                    sequence: 1,
                    stanza_xml: "<message xmlns='jabber:client' id='post-wrap-1'/>".to_string(),
                    original_receipt_at: chrono::Utc::now(),
                },
                waddle_xmpp::stream_management::DetachedUnackedStanza {
                    sequence: 2,
                    stanza_xml: "<message xmlns='jabber:client' id='post-wrap-2'/>".to_string(),
                    original_receipt_at: chrono::Utc::now(),
                },
            ],
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        },
    )
    .await;

    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::authenticated(&jid);
    // h = u32::MAX acks the pre-wrap stanza; a naive `h > outbound`
    // comparison misreads this as handled-count-too-high.
    let resume_frame = resume_frame_xml("stream-wrapped", u32::MAX);
    let responses =
        handle_xmpp_frame(&resume_frame, "example.com", state.as_ref(), &mut conn).await;

    let resumed = responses
        .iter()
        .any(|frame| frame.contains("<resumed") && frame.contains("stream-wrapped"));
    assert!(
        resumed,
        "wrapped-counter resume must succeed, got frames: {responses:?}"
    );
    assert!(
        responses.iter().any(|frame| frame.contains("post-wrap-1"))
            && responses.iter().any(|frame| frame.contains("post-wrap-2")),
        "post-wrap unacked stanzas must be replayed: {responses:?}"
    );
    assert!(
        !responses.iter().any(|frame| frame.contains("pre-wrap")),
        "the acked pre-wrap stanza must not be replayed: {responses:?}"
    );
}

/// Conformance review follow-up to #1103: resuming must carry the
/// detached session's presence extension payloads back onto the live
/// registry entry. RFC 6121 §4.3.2 requires probe responses to
/// reproduce the full last presence stanza; before this fix a resume
/// silently stripped the XEP-0319 idle stamp (and caps) even though
/// the client sent no new presence.
#[tokio::test]
async fn sm_resume_restores_presence_payloads_to_the_live_registry() {
    use waddle_xmpp::stream_management::DetachedSession;
    let state = create_test_websocket_state().await;

    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let idle = waddle_xmpp::xep::xep0319::build_idle_element(
        chrono::DateTime::parse_from_rfc3339("2026-07-07T00:00:00Z")
            .expect("timestamp")
            .with_timezone(&chrono::Utc),
    );
    let _detached_session = store_resumable_test_session(
        state.as_ref(),
        DetachedSession {
            stream_id: "stream-idle".to_string(),
            user_id: "alice@example.com".to_string(),
            jid: jid.clone(),
            inbound_count: 0,
            shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: vec![],
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: true,
            presence_show: Some(xmpp_parsers::presence::Show::Away),
            presence_status: None,
            presence_priority: 0,
            presence_payloads: vec![idle.clone()],
            pending_subscribes_flushed: false,
        },
    )
    .await;

    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::authenticated(&jid);
    let responses = handle_xmpp_frame(
        &resume_frame_xml("stream-idle", 0),
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;
    assert!(
        responses.iter().any(|frame| frame.contains("<resumed")),
        "resume must succeed: {responses:?}"
    );
    let (tx, _rx) = mpsc::channel::<OutboundStanza>(8);
    let mut pending_tx = Some(tx);
    super::super::registration::register_bound_connection_after_frame(
        state.as_ref(),
        "example.com",
        &mut conn,
        &mut pending_tx,
    )
    .await;

    let presence_state = state
        .deps
        .protocol
        .connection_registry
        .get_presence_state(&jid)
        .expect("live presence state after resume");
    assert!(
        presence_state.payloads.contains(&idle),
        "the XEP-0319 idle payload must survive resume onto the live \
         registry entry, got {:?}",
        presence_state.payloads
    );
}

/// Conformance review follow-up to #1104: an XEP-0198 resume is the
/// SAME session (§5) — the once-per-session pending-subscribe claim
/// must survive it. Before this fix the fresh ConnectionEntry's CAS
/// re-armed, so the first auto-away flip after a resume re-prompted
/// the user with the still-unanswered subscribe.
#[tokio::test]
async fn sm_resume_preserves_the_pending_subscribe_once_per_session_claim() {
    use waddle_xmpp::stream_management::DetachedSession;
    let state = create_test_websocket_state().await;

    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let _detached_session = store_resumable_test_session(
        state.as_ref(),
        DetachedSession {
            stream_id: "stream-claimed".to_string(),
            user_id: "alice@example.com".to_string(),
            jid: jid.clone(),
            inbound_count: 0,
            shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: vec![],
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            // Still available at detach; the claim consumed by the
            // initial available presence is recorded explicitly.
            presence_available: true,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: true,
        },
    )
    .await;

    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::authenticated(&jid);
    let responses = handle_xmpp_frame(
        &resume_frame_xml("stream-claimed", 0),
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;
    assert!(
        responses.iter().any(|frame| frame.contains("<resumed")),
        "resume must succeed: {responses:?}"
    );
    let (tx, _rx) = mpsc::channel::<OutboundStanza>(8);
    let mut pending_tx = Some(tx);
    super::super::registration::register_bound_connection_after_frame(
        state.as_ref(),
        "example.com",
        &mut conn,
        &mut pending_tx,
    )
    .await;

    let entry = state
        .deps
        .protocol
        .connection_registry
        .get_entry(&jid)
        .expect("registered entry after resume");
    assert!(
        !entry.claim_pending_subscribes_flush(),
        "the once-per-session claim must already be consumed on the \
         resumed entry — a presence flip after resume must not \
         re-deliver pending subscribes"
    );
}

/// The consumed claim must survive detach even when the session went
/// UNAVAILABLE after its initial available presence: presence state at
/// detach says nothing about whether the flush already happened, so
/// the claim is carried explicitly on the detached session. Before
/// this fix the pre-claim was gated on `presence_available`, so an
/// available → unavailable → detach → resume sequence re-armed the CAS
/// and the next available re-prompted the user.
#[tokio::test]
async fn sm_resume_preserves_consumed_claim_when_detached_unavailable() {
    use waddle_xmpp::stream_management::DetachedSession;
    let state = create_test_websocket_state().await;

    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let _detached_session = store_resumable_test_session(
        state.as_ref(),
        DetachedSession {
            stream_id: "stream-claimed-unavail".to_string(),
            user_id: "alice@example.com".to_string(),
            jid: jid.clone(),
            inbound_count: 0,
            shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: vec![],
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            // Went available (claim consumed), then unavailable before
            // the transport dropped.
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: true,
        },
    )
    .await;

    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::authenticated(&jid);
    let responses = handle_xmpp_frame(
        &resume_frame_xml("stream-claimed-unavail", 0),
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;
    assert!(
        responses.iter().any(|frame| frame.contains("<resumed")),
        "resume must succeed: {responses:?}"
    );
    let (tx, _rx) = mpsc::channel::<OutboundStanza>(8);
    let mut pending_tx = Some(tx);
    super::super::registration::register_bound_connection_after_frame(
        state.as_ref(),
        "example.com",
        &mut conn,
        &mut pending_tx,
    )
    .await;

    let entry = state
        .deps
        .protocol
        .connection_registry
        .get_entry(&jid)
        .expect("registered entry after resume");
    assert!(
        !entry.claim_pending_subscribes_flush(),
        "the claim consumed before the unavailable flip must stay \
         consumed across resume — the next available presence must \
         not re-deliver pending subscribes"
    );
}

/// Companion: a session that NEVER went available before detaching has
/// an unconsumed claim, and resume must keep it armed — the resumed
/// session's true initial available presence still owes the RFC 6121
/// §3.1.3 pending-subscribe delivery.
#[tokio::test]
async fn sm_resume_keeps_unconsumed_claim_armed() {
    use waddle_xmpp::stream_management::DetachedSession;
    let state = create_test_websocket_state().await;

    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let _detached_session = store_resumable_test_session(
        state.as_ref(),
        DetachedSession {
            stream_id: "stream-unclaimed".to_string(),
            user_id: "alice@example.com".to_string(),
            jid: jid.clone(),
            inbound_count: 0,
            shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: vec![],
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        },
    )
    .await;

    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::authenticated(&jid);
    let responses = handle_xmpp_frame(
        &resume_frame_xml("stream-unclaimed", 0),
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;
    assert!(
        responses.iter().any(|frame| frame.contains("<resumed")),
        "resume must succeed: {responses:?}"
    );
    let (tx, _rx) = mpsc::channel::<OutboundStanza>(8);
    let mut pending_tx = Some(tx);
    super::super::registration::register_bound_connection_after_frame(
        state.as_ref(),
        "example.com",
        &mut conn,
        &mut pending_tx,
    )
    .await;

    let entry = state
        .deps
        .protocol
        .connection_registry
        .get_entry(&jid)
        .expect("registered entry after resume");
    assert!(
        entry.claim_pending_subscribes_flush(),
        "a never-available session's claim must still be armed after \
         resume so the initial available presence delivers the queued \
         subscribes"
    );
}

/// Round-2 concurrency review on #1099: an `h` in the wrap-BEHIND
/// half-space (mod-2^32 "less than" everything the session ever acked)
/// passed the too-high guard as "behind", then the numeric range-delete
/// wiped every claimed pending_delivery row and corrupted last_acked.
/// A live ack outside the valid window [last_acked, outbound_count] on
/// the low side is stale garbage: it must be ignored wholesale — no
/// purge, no counter movement, no stream error.
#[tokio::test]
async fn sm_live_ack_behind_last_acked_is_ignored_without_purging() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let stream_id = enable_sm_for_live_ack_tests(state.as_ref(), &mut conn, &jid).await;

    let _ = conn.sm_state.record_outbound(
        "<message xmlns='jabber:client' id='o1'/>".to_string(),
        SmEvictionPath::DirectOutbound,
    );
    let _ = conn.sm_state.record_outbound(
        "<message xmlns='jabber:client' id='o2'/>".to_string(),
        SmEvictionPath::DirectOutbound,
    );
    let recipient: BareJid = "alice@example.com".parse().expect("bare jid");
    seed_claimed_pending_row(state.as_ref(), &recipient, &stream_id, 1).await;

    // 0xC0000000 is mod-2^32 "behind" outbound_count=2, so the
    // too-high guard alone does not catch it — but numerically it
    // exceeds every real sequence, so an unguarded range-delete would
    // destroy all rows.
    let responses = handle_xmpp_frame(
        "<a xmlns='urn:xmpp:sm:3' h='3221225472'/>",
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert!(
        responses.is_empty(),
        "stale wrap-behind ack must be ignored, got {responses:?}"
    );
    assert!(
        !conn.phase.is_closing(),
        "stale ack must not terminate the stream"
    );
    assert_eq!(
        conn.sm_state.last_acked, 0,
        "stale ack must not move last_acked"
    );
    assert_eq!(
        conn.sm_state.get_stanzas_to_resend(0).len(),
        2,
        "stale ack must not purge the replay queue"
    );
    let rows = state
        .deps
        .protocol
        .pending_delivery_storage
        .list(&recipient)
        .await
        .expect("list pending rows");
    assert_eq!(
        rows.len(),
        1,
        "stale ack must not range-delete pending rows"
    );
}

/// Companion to the wrap-behind live-ack guard: a resume whose `h`
/// regressed mod-2^32 behind the session's last_acked cannot be
/// replayed (that prefix was purged when the ack landed) and must be
/// refused as a failed resume — session preserved, nothing purged.
#[tokio::test]
async fn sm_resume_rejects_handled_count_behind_last_acked() {
    use waddle_xmpp::stream_management::DetachedSession;
    let state = create_test_websocket_state().await;

    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let _detached_session = store_resumable_test_session(
        state.as_ref(),
        DetachedSession {
            stream_id: "stream-regressed".to_string(),
            user_id: "alice@example.com".to_string(),
            jid: jid.clone(),
            inbound_count: 0,
            shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
            outbound_count: 4,
            last_acked: 3,
            replay_gap_through: None,
            unacked_stanzas: vec![waddle_xmpp::stream_management::DetachedUnackedStanza {
                sequence: 4,
                stanza_xml: "<message xmlns='jabber:client' id='m4'/>".to_string(),
                original_receipt_at: chrono::Utc::now(),
            }],
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        },
    )
    .await;

    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::authenticated(&jid);
    // h = 0xC0000000 is mod-2^32 behind last_acked=3 (stale garbage);
    // the wrap-aware too-high guard alone would classify it "behind
    // outbound" and let acknowledge() + the numeric row range-delete
    // run.
    let responses = handle_xmpp_frame(
        &resume_frame_xml("stream-regressed", 0xC000_0000),
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert!(
        responses
            .iter()
            .any(|frame| frame.contains("<failed") && frame.contains("resource-constraint")),
        "regressed-h resume must fail as unresumable, got {responses:?}"
    );
    // The detached session survives for a corrected retry.
    let restored = state
        .deps
        .protocol
        .sm_session_registry
        .claim_session("stream-regressed")
        .await
        .expect("registry")
        .expect("session preserved after failed resume");
    assert_eq!(restored.last_acked, 3);
}
