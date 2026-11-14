use super::super::cleanup::{
    cleanup_connection_shutdown, cleanup_force_detach_connection_shutdown,
    cleanup_muc_presence_for_jid,
};
use super::super::frame::{handle_xmpp_frame, settle_inbound_dispatch};
use super::super::frame_backstop::InboundDisposition;
use super::super::handlers;
use super::super::interpret_loop::build_interpret_deps;
use super::super::replay::drive_interpret_loop;
use super::super::state::{WsConnState, TERMINAL_RECOVERY_QUEUE_CAP};
use super::super::stream_management::is_countable_stanza;
use super::super::transport_xml::{
    build_stream_features_xml, element_to_xml, sasl_success_xml, stanza_to_xml,
};
use super::{
    create_test_server_owner_session, create_test_session, create_test_websocket_state,
    create_test_websocket_state_with_sm_registry,
    create_test_websocket_state_with_sm_registry_and_pending_storage, handle_muc_join,
    handle_muc_join_with_occupancy_session, message_frame_xml_with_id, register_test_connection,
    register_test_native_user, scram_client_final_from_challenge, snapshot_room,
};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use jid::{BareJid, FullJid};
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, Notify, Semaphore};
use waddle_xmpp::ownership::{ClaimStore, InProcessClaimStore, NodeIdentity, SharedNodeIdentity};
use waddle_xmpp::protocol::{Blocklist, ConnectionPhase, InboundEvent};
use waddle_xmpp::registry::{
    ConnectionEntry, DeliveryKind, ForceDetachOrigin, ForceDetachOutcome, ForceDetachRequest,
    GetUser, OutboundStanza, RegisterUserResource, UserRegistryError, WireUserClusteringClaims,
};
use waddle_xmpp::stream_management::{InMemorySmSessionRegistry, SmSessionRegistry, SM_NS};
use waddle_xmpp::telemetry::attributes::SmEvictionPath;
use waddle_xmpp::Stanza;
use xmpp_parsers::minidom::Element;

/// A transient promotion failure with a real pending-delivery backend behind
/// it. The first insertion fails; the cleanup-triggered retry can therefore
/// exercise the exact same state object after storage recovers.
struct FailFirstPendingStorage {
    inner: waddle_xmpp::pending_delivery::storage::InMemoryPendingDeliveryStorage,
    fail_next_insert: std::sync::atomic::AtomicBool,
    fail_next_ack: std::sync::atomic::AtomicBool,
    reject_next_fenced_ack: std::sync::atomic::AtomicBool,
    saw_fenced_ack: std::sync::atomic::AtomicBool,
}

impl FailFirstPendingStorage {
    fn new() -> Self {
        Self {
            inner:
                waddle_xmpp::pending_delivery::storage::InMemoryPendingDeliveryStorage::unlimited(),
            fail_next_insert: std::sync::atomic::AtomicBool::new(true),
            fail_next_ack: std::sync::atomic::AtomicBool::new(false),
            reject_next_fenced_ack: std::sync::atomic::AtomicBool::new(false),
            saw_fenced_ack: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

#[async_trait::async_trait]
impl waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage for FailFirstPendingStorage {
    async fn notification_recovery_high_water(
        &self,
    ) -> Result<
        Option<waddle_xmpp::pending_delivery::storage::PendingNotificationRecoveryOrdinal>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner.notification_recovery_high_water().await
    }

    async fn list_unoutboxed_archived_after(
        &self,
        after: Option<waddle_xmpp::pending_delivery::storage::PendingNotificationRecoveryOrdinal>,
        through: Option<waddle_xmpp::pending_delivery::storage::PendingNotificationRecoveryOrdinal>,
        limit: usize,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::storage::PendingNotificationRecoveryRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner
            .list_unoutboxed_archived_after(after, through, limit)
            .await
    }

    fn notification_custody_mode(
        &self,
    ) -> waddle_xmpp::pending_delivery::storage::PendingNotificationCustodyMode {
        self.inner.notification_custody_mode()
    }

    async fn mark_notification_outboxed(
        &self,
        id: &waddle_xmpp::pending_delivery::PendingRowId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.mark_notification_outboxed(id).await
    }

    fn quota_policy(&self) -> waddle_xmpp::pending_delivery::QuotaPolicy {
        self.inner.quota_policy()
    }

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

    async fn claim_archive_ordered_batch_for_session(
        &self,
        recipient: &BareJid,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
        token: &waddle_xmpp::pending_delivery::storage::PendingClaimToken,
        limit: usize,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::PendingRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner
            .claim_archive_ordered_batch_for_session(recipient, session, token, limit)
            .await
    }

    async fn release_unpushed_row_if_session(
        &self,
        id: &waddle_xmpp::pending_delivery::PendingRowId,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
        token: &waddle_xmpp::pending_delivery::storage::PendingClaimToken,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner
            .release_unpushed_row_if_session(id, session, token)
            .await
    }

    async fn mark_claim_offered(
        &self,
        claim: &waddle_xmpp::pending_delivery::storage::PendingClaim,
    ) -> Result<bool, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.mark_claim_offered(claim).await
    }

    async fn release_unqueued_offer(
        &self,
        claim: &waddle_xmpp::pending_delivery::storage::PendingClaim,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.release_unqueued_offer(claim).await
    }

    async fn list_unoffered_claims(
        &self,
        recipient: &BareJid,
        after: Option<&waddle_xmpp::pending_delivery::PendingRowId>,
        claimed_before_ms: i64,
        limit: usize,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::storage::PendingClaim>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner
            .list_unoffered_claims(recipient, after, claimed_before_ms, limit)
            .await
    }

    async fn delete_unsequenced_claim(
        &self,
        claim: &waddle_xmpp::pending_delivery::storage::PendingClaim,
        phase: waddle_xmpp::pending_delivery::storage::PendingClaimPhase,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.delete_unsequenced_claim(claim, phase).await
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
        authority: waddle_xmpp::pending_delivery::storage::PendingAckAuthority<'_>,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        if let waddle_xmpp::pending_delivery::storage::PendingAckAuthority::Fenced(_) = authority {
            self.saw_fenced_ack
                .store(true, std::sync::atomic::Ordering::SeqCst);
            if self
                .reject_next_fenced_ack
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                return Err(
                    waddle_xmpp::pending_delivery::storage::PendingStorageError::NotOwner {
                        entity: waddle_xmpp::ownership::Entity::new(
                            waddle_xmpp::ownership::EntityType::SmSession,
                            session.as_str().to_string(),
                        ),
                    },
                );
            }
        }
        if self
            .fail_next_ack
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(
                waddle_xmpp::pending_delivery::storage::PendingStorageError::Other(
                    "injected pending ACK failure".to_owned(),
                ),
            );
        }
        self.inner
            .delete_acked_in_window(session, from_exclusive, to_inclusive, authority)
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

async fn claim_enabled_cleanup_fixture(
    state: &super::super::state::WebSocketState,
    conn: &mut WsConnState,
) {
    let stream_id = conn
        .sm_state
        .stream_id
        .as_deref()
        .expect("enabled fixture stream");
    let authority = state
        .deps
        .protocol
        .sm_session_registry
        .ensure_session_claim(stream_id)
        .await
        .expect("fresh enabled fixture claim");
    conn.sm_ingress_fence = state
        .deps
        .protocol
        .sm_session_registry
        .current_sm_claim_fence(stream_id);
    assert!(conn.sm_ingress_fence.is_some());
    drop(authority);
}

struct FailDeleteSmPersistence {
    inner: waddle_xmpp::stream_management::persistence::InMemorySmPersistence,
    gate_inventory_store: std::sync::atomic::AtomicBool,
    fail_delete: std::sync::atomic::AtomicBool,
    get_session_calls: std::sync::atomic::AtomicUsize,
    inventory_store_calls: std::sync::atomic::AtomicUsize,
    reject_next_principal_store: std::sync::atomic::AtomicBool,
    reject_next_promotion_append: std::sync::atomic::AtomicBool,
    append_started: Notify,
    append_release: Semaphore,
}

impl FailDeleteSmPersistence {
    fn new() -> Self {
        Self {
            inner: waddle_xmpp::stream_management::persistence::InMemorySmPersistence::new(),
            gate_inventory_store: std::sync::atomic::AtomicBool::new(false),
            fail_delete: std::sync::atomic::AtomicBool::new(true),
            get_session_calls: std::sync::atomic::AtomicUsize::new(0),
            inventory_store_calls: std::sync::atomic::AtomicUsize::new(0),
            reject_next_principal_store: std::sync::atomic::AtomicBool::new(false),
            reject_next_promotion_append: std::sync::atomic::AtomicBool::new(false),
            append_started: Notify::new(),
            append_release: Semaphore::new(0),
        }
    }
}

#[async_trait::async_trait]
impl waddle_xmpp::stream_management::persistence::SmPersistenceStorage for FailDeleteSmPersistence {
    async fn append_detached_stanza_atomic(
        &self,
        append: waddle_xmpp::stream_management::persistence::PersistedDetachedAppend,
        fence: Option<&waddle_xmpp::stream_management::persistence::SmClaimFence>,
    ) -> Result<
        waddle_xmpp::stream_management::persistence::KeyedSnapshotOutcome,
        waddle_xmpp::stream_management::persistence::SmPersistenceError,
    > {
        self.inner
            .append_detached_stanza_atomic(append, fence)
            .await
    }

    async fn acknowledge_claimed_session(
        &self,
        stream_id: &waddle_xmpp::pending_delivery::SmSessionId,
        acknowledged_h: u32,
        expected_fence: &waddle_xmpp::stream_management::persistence::SmClaimFence,
        authority: Option<&waddle_xmpp::ownership::CurrentNodeIdentityGuard>,
    ) -> Result<(), waddle_xmpp::stream_management::persistence::SmPersistenceError> {
        self.inner
            .acknowledge_claimed_session(stream_id, acknowledged_h, expected_fence, authority)
            .await
    }

    async fn pending_acknowledgements(
        &self,
        stream_id: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<
        Vec<waddle_xmpp::stream_management::persistence::SmAckWindow>,
        waddle_xmpp::stream_management::persistence::SmPersistenceError,
    > {
        self.inner.pending_acknowledgements(stream_id).await
    }

    async fn confirm_acknowledgement(
        &self,
        stream_id: &waddle_xmpp::pending_delivery::SmSessionId,
        window: waddle_xmpp::stream_management::persistence::SmAckWindow,
        expected_fence: &waddle_xmpp::stream_management::persistence::SmClaimFence,
        authority: Option<&waddle_xmpp::ownership::CurrentNodeIdentityGuard>,
    ) -> Result<(), waddle_xmpp::stream_management::persistence::SmPersistenceError> {
        self.inner
            .confirm_acknowledgement(stream_id, window, expected_fence, authority)
            .await
    }

    async fn pending_claim_protected_streams(
        &self,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::SmSessionId>,
        waddle_xmpp::stream_management::persistence::SmPersistenceError,
    > {
        self.inner.pending_claim_protected_streams().await
    }

    async fn mark_session_nonresumable(
        &self,
        stream_id: &waddle_xmpp::pending_delivery::SmSessionId,
        acknowledged_h: u32,
        expected_fence: &waddle_xmpp::stream_management::persistence::SmClaimFence,
        authority: Option<&waddle_xmpp::ownership::CurrentNodeIdentityGuard>,
    ) -> Result<(), waddle_xmpp::stream_management::persistence::SmPersistenceError> {
        self.inner
            .mark_session_nonresumable(stream_id, acknowledged_h, expected_fence, authority)
            .await
    }

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
        self.get_session_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.get_session(stream_id).await
    }

    async fn delete_session(
        &self,
        stream_id: &waddle_xmpp::pending_delivery::SmSessionId,
    ) -> Result<(), waddle_xmpp::stream_management::persistence::SmPersistenceError> {
        if !self.fail_delete.load(std::sync::atomic::Ordering::SeqCst) {
            return self.inner.delete_session(stream_id).await;
        }
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

    async fn append_promotion_stanza(
        &self,
        stanza: waddle_xmpp::stream_management::persistence::PersistedUnackedStanza,
    ) -> Result<
        waddle_xmpp::stream_management::persistence::PromotionAppendOutcome,
        waddle_xmpp::stream_management::persistence::SmPersistenceError,
    > {
        if self
            .reject_next_promotion_append
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(
                waddle_xmpp::stream_management::persistence::SmPersistenceError::Other(
                    "transient terminal suffix append failure".to_owned(),
                ),
            );
        }
        self.inner.append_promotion_stanza(stanza).await
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
        self.inventory_store_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self
            .gate_inventory_store
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            self.append_started.notify_one();
            self.append_release
                .acquire()
                .await
                .expect("inventory gate")
                .forget();
        }
        self.inner.store_session_atomic(session, unacked).await
    }

    async fn store_session_atomic_with_principal(
        &self,
        principal: &waddle_xmpp::auth::AuthenticatedPrincipalRef,
        session: waddle_xmpp::stream_management::persistence::PersistedSession,
        unacked: Vec<waddle_xmpp::stream_management::persistence::PersistedUnackedStanza>,
    ) -> Result<(), waddle_xmpp::stream_management::persistence::SmPersistenceError> {
        if self
            .reject_next_principal_store
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(
                waddle_xmpp::stream_management::persistence::SmPersistenceError::Other(
                    "simulated initial detach snapshot failure".to_owned(),
                ),
            );
        }
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

    async fn store_session_atomic_with_ingress_append(
        &self,
        session: waddle_xmpp::stream_management::persistence::PersistedSession,
        unacked: Vec<waddle_xmpp::stream_management::persistence::PersistedUnackedStanza>,
        append: waddle_xmpp::stream_management::persistence::PersistedIngressAppend,
    ) -> Result<
        waddle_xmpp::stream_management::persistence::KeyedSnapshotOutcome,
        waddle_xmpp::stream_management::persistence::SmPersistenceError,
    > {
        self.inner
            .store_session_atomic_with_ingress_append(session, unacked, append)
            .await
    }

    async fn store_session_atomic_with_principal_and_ingress_appends(
        &self,
        principal: &waddle_xmpp::auth::AuthenticatedPrincipalRef,
        session: waddle_xmpp::stream_management::persistence::PersistedSession,
        unacked: Vec<waddle_xmpp::stream_management::persistence::PersistedUnackedStanza>,
        appends: Vec<waddle_xmpp::stream_management::persistence::PersistedIngressAppend>,
    ) -> Result<
        Vec<waddle_xmpp::stream_management::SmIngressAppendKey>,
        waddle_xmpp::stream_management::persistence::SmPersistenceError,
    > {
        self.inner
            .store_session_atomic_with_principal_and_ingress_appends(
                principal, session, unacked, appends,
            )
            .await
    }

    async fn get_ingress_append(
        &self,
        key: &waddle_xmpp::stream_management::SmIngressAppendKey,
    ) -> Result<
        Option<waddle_xmpp::stream_management::persistence::PersistedIngressAppend>,
        waddle_xmpp::stream_management::persistence::SmPersistenceError,
    > {
        self.inner.get_ingress_append(key).await
    }

    async fn list_pending_ingress_appends_after(
        &self,
        after: Option<&waddle_xmpp::stream_management::SmIngressAppendKey>,
        limit: usize,
    ) -> Result<
        Vec<waddle_xmpp::stream_management::persistence::PersistedIngressAppend>,
        waddle_xmpp::stream_management::persistence::SmPersistenceError,
    > {
        self.inner
            .list_pending_ingress_appends_after(after, limit)
            .await
    }

    async fn scrub_ingress_custody(
        &self,
        target: &waddle_xmpp::tombstone::TombstoneTarget,
        through: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), waddle_xmpp::stream_management::persistence::SmPersistenceError> {
        self.inner.scrub_ingress_custody(target, through).await
    }

    async fn delete_tombstoned_unacked(
        &self,
        stream: &waddle_xmpp::pending_delivery::SmSessionId,
        sequences: &[u32],
    ) -> Result<u64, waddle_xmpp::stream_management::persistence::SmPersistenceError> {
        self.inner
            .delete_tombstoned_unacked(stream, sequences)
            .await
    }

    async fn get_ingress_appends_for_sequence(
        &self,
        stream: &waddle_xmpp::pending_delivery::SmSessionId,
        sequence: u32,
    ) -> Result<
        Vec<waddle_xmpp::stream_management::persistence::PersistedIngressAppend>,
        waddle_xmpp::stream_management::persistence::SmPersistenceError,
    > {
        self.inner
            .get_ingress_appends_for_sequence(stream, sequence)
            .await
    }

    async fn list_pending_ingress_appends(
        &self,
        limit: usize,
    ) -> Result<
        Vec<waddle_xmpp::stream_management::persistence::PersistedIngressAppend>,
        waddle_xmpp::stream_management::persistence::SmPersistenceError,
    > {
        self.inner.list_pending_ingress_appends(limit).await
    }

    async fn complete_ingress_append(
        &self,
        key: &waddle_xmpp::stream_management::SmIngressAppendKey,
        stream: &waddle_xmpp::pending_delivery::SmSessionId,
        sequence: u32,
        disposition: waddle_xmpp::stream_management::persistence::IngressCustodyDisposition,
    ) -> Result<bool, waddle_xmpp::stream_management::persistence::SmPersistenceError> {
        self.inner
            .complete_ingress_append(key, stream, sequence, disposition)
            .await
    }

    async fn complete_ingress_appends_through(
        &self,
        stream: &waddle_xmpp::pending_delivery::SmSessionId,
        from_exclusive: u32,
        h: u32,
    ) -> Result<(), waddle_xmpp::stream_management::persistence::SmPersistenceError> {
        self.inner
            .complete_ingress_appends_through(stream, from_exclusive, h)
            .await
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
    async fn notification_recovery_high_water(
        &self,
    ) -> Result<
        Option<waddle_xmpp::pending_delivery::storage::PendingNotificationRecoveryOrdinal>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner.notification_recovery_high_water().await
    }

    async fn list_unoutboxed_archived_after(
        &self,
        after: Option<waddle_xmpp::pending_delivery::storage::PendingNotificationRecoveryOrdinal>,
        through: Option<waddle_xmpp::pending_delivery::storage::PendingNotificationRecoveryOrdinal>,
        limit: usize,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::storage::PendingNotificationRecoveryRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner
            .list_unoutboxed_archived_after(after, through, limit)
            .await
    }

    fn notification_custody_mode(
        &self,
    ) -> waddle_xmpp::pending_delivery::storage::PendingNotificationCustodyMode {
        self.inner.notification_custody_mode()
    }

    async fn mark_notification_outboxed(
        &self,
        id: &waddle_xmpp::pending_delivery::PendingRowId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.mark_notification_outboxed(id).await
    }

    fn quota_policy(&self) -> waddle_xmpp::pending_delivery::QuotaPolicy {
        self.inner.quota_policy()
    }

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

    async fn claim_archive_ordered_batch_for_session(
        &self,
        recipient: &BareJid,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
        token: &waddle_xmpp::pending_delivery::storage::PendingClaimToken,
        limit: usize,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::PendingRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner
            .claim_archive_ordered_batch_for_session(recipient, session, token, limit)
            .await
    }

    async fn release_unpushed_row_if_session(
        &self,
        id: &waddle_xmpp::pending_delivery::PendingRowId,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
        token: &waddle_xmpp::pending_delivery::storage::PendingClaimToken,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner
            .release_unpushed_row_if_session(id, session, token)
            .await
    }

    async fn mark_claim_offered(
        &self,
        claim: &waddle_xmpp::pending_delivery::storage::PendingClaim,
    ) -> Result<bool, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.mark_claim_offered(claim).await
    }

    async fn release_unqueued_offer(
        &self,
        claim: &waddle_xmpp::pending_delivery::storage::PendingClaim,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.release_unqueued_offer(claim).await
    }

    async fn list_unoffered_claims(
        &self,
        recipient: &BareJid,
        after: Option<&waddle_xmpp::pending_delivery::PendingRowId>,
        claimed_before_ms: i64,
        limit: usize,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::storage::PendingClaim>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner
            .list_unoffered_claims(recipient, after, claimed_before_ms, limit)
            .await
    }

    async fn delete_unsequenced_claim(
        &self,
        claim: &waddle_xmpp::pending_delivery::storage::PendingClaim,
        phase: waddle_xmpp::pending_delivery::storage::PendingClaimPhase,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.delete_unsequenced_claim(claim, phase).await
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
        authority: waddle_xmpp::pending_delivery::storage::PendingAckAuthority<'_>,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner
            .delete_acked_in_window(session, from_exclusive, to_inclusive, authority)
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
    async fn notification_recovery_high_water(
        &self,
    ) -> Result<
        Option<waddle_xmpp::pending_delivery::storage::PendingNotificationRecoveryOrdinal>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner.notification_recovery_high_water().await
    }

    async fn list_unoutboxed_archived_after(
        &self,
        after: Option<waddle_xmpp::pending_delivery::storage::PendingNotificationRecoveryOrdinal>,
        through: Option<waddle_xmpp::pending_delivery::storage::PendingNotificationRecoveryOrdinal>,
        limit: usize,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::storage::PendingNotificationRecoveryRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner
            .list_unoutboxed_archived_after(after, through, limit)
            .await
    }

    fn notification_custody_mode(
        &self,
    ) -> waddle_xmpp::pending_delivery::storage::PendingNotificationCustodyMode {
        self.inner.notification_custody_mode()
    }

    async fn mark_notification_outboxed(
        &self,
        id: &waddle_xmpp::pending_delivery::PendingRowId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.mark_notification_outboxed(id).await
    }

    fn quota_policy(&self) -> waddle_xmpp::pending_delivery::QuotaPolicy {
        self.inner.quota_policy()
    }

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

    async fn claim_archive_ordered_batch_for_session(
        &self,
        recipient: &BareJid,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
        token: &waddle_xmpp::pending_delivery::storage::PendingClaimToken,
        limit: usize,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::PendingRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner
            .claim_archive_ordered_batch_for_session(recipient, session, token, limit)
            .await
    }

    async fn release_unpushed_row_if_session(
        &self,
        id: &waddle_xmpp::pending_delivery::PendingRowId,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
        token: &waddle_xmpp::pending_delivery::storage::PendingClaimToken,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner
            .release_unpushed_row_if_session(id, session, token)
            .await
    }

    async fn mark_claim_offered(
        &self,
        claim: &waddle_xmpp::pending_delivery::storage::PendingClaim,
    ) -> Result<bool, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.mark_claim_offered(claim).await
    }

    async fn release_unqueued_offer(
        &self,
        claim: &waddle_xmpp::pending_delivery::storage::PendingClaim,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.release_unqueued_offer(claim).await
    }

    async fn list_unoffered_claims(
        &self,
        recipient: &BareJid,
        after: Option<&waddle_xmpp::pending_delivery::PendingRowId>,
        claimed_before_ms: i64,
        limit: usize,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::storage::PendingClaim>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner
            .list_unoffered_claims(recipient, after, claimed_before_ms, limit)
            .await
    }

    async fn delete_unsequenced_claim(
        &self,
        claim: &waddle_xmpp::pending_delivery::storage::PendingClaim,
        phase: waddle_xmpp::pending_delivery::storage::PendingClaimPhase,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.delete_unsequenced_claim(claim, phase).await
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
        authority: waddle_xmpp::pending_delivery::storage::PendingAckAuthority<'_>,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner
            .delete_acked_in_window(session, from_exclusive, to_inclusive, authority)
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

// ---- XEP-0198 stream management --------------------------------

#[test]
fn timed_out_inbound_stanza_preserves_sender_responsibility() {
    let mut state = waddle_xmpp::stream_management::StreamManagementState::new();
    state.enable("timeout-regression".to_string(), true, Some(300));
    let mut completion = crate::server::routes::interpret::SmInboundCompletionTracker::default();
    let sequence = completion.reserve(&state);

    settle_inbound_dispatch(
        InboundDisposition::Unhandled,
        true,
        Some(sequence),
        &mut completion,
        &mut state,
    );

    assert_eq!(state.inbound_count(), 0);
    assert!(!completion.has_pending());
    assert!(completion.has_unhandled_hole());

    // A late ordered-relay completion cannot turn the cancelled dispatch into
    // an acknowledgement: the sender must retain and replay this stanza.
    completion.complete(sequence, &mut state);
    assert_eq!(state.inbound_count(), 0);
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
    claim_enabled_cleanup_fixture(&state, &mut conn).await;
    // A production enabled stream always carries a live claim fence; without
    // one the fenceless force-detach teardown promotes terminally instead of
    // taking the detach path this test exercises.
    drop(
        state
            .deps
            .protocol
            .sm_session_registry
            .ensure_session_claim("force-detach-ambiguous")
            .await
            .expect("enable claim"),
    );

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
    claim_enabled_cleanup_fixture(&state, &mut conn).await;
    // A production enabled stream always carries a live claim fence
    // (`ensure_session_claim` gates <enabled/>); without one the fenceless
    // force-detach teardown correctly promotes terminally instead of
    // detaching, which is not what this test exercises.
    drop(
        state
            .deps
            .protocol
            .sm_session_registry
            .ensure_session_claim("stale-force-detach-stream")
            .await
            .expect("enable claim"),
    );

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
        .entry(&jid)
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
    claim_enabled_cleanup_fixture(&state, &mut conn).await;
    // A production enabled stream always carries a live claim fence; without
    // one the fenceless force-detach teardown promotes terminally instead of
    // taking the detach path this test exercises.
    drop(
        state
            .deps
            .protocol
            .sm_session_registry
            .ensure_session_claim("force-detach-busy")
            .await
            .expect("enable claim"),
    );

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
        .entry(&old_jid)
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
    claim_enabled_cleanup_fixture(&state, &mut conn).await;
    // See stale_actor_force_detach_cleanup_detaches_without_registry_reentry:
    // an enabled stream carries a live fence in production.
    drop(
        state
            .deps
            .protocol
            .sm_session_registry
            .ensure_session_claim("queued-force-detach-stream")
            .await
            .expect("enable claim"),
    );

    let (crossnode_ack_tx, crossnode_ack_rx) = oneshot::channel();
    old_entry
        .force_detach_sender()
        .try_send(ForceDetachRequest {
            origin: ForceDetachOrigin::CrossNodeResume,
            target_binding: None,
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
                // Retirement stamps its request with the retired entry's
                // binding; the drain drops requests for any other binding.
                old_entry.binding_ref(),
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
            target_binding: None,
            requester_bare_jid: bare_jid.clone(),
            ack: stale_ack_tx,
        },
        ForceDetachRequest {
            origin: ForceDetachOrigin::CrossNodeResume,
            target_binding: None,
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
async fn fresh_replacement_wins_queued_resume_and_releases_stale_actor_waiter() {
    for reverse in [false, true] {
        let bare_jid = BareJid::from_str("fresh-replacement@example.com").unwrap();
        let (stale_tx, mut stale_rx) = oneshot::channel();
        let (resume_tx, mut resume_rx) = oneshot::channel();
        let (fresh_tx, mut fresh_rx) = oneshot::channel();
        let mut requests = vec![
            ForceDetachRequest {
                origin: ForceDetachOrigin::RegistryStaleActorRetirement,
                target_binding: None,
                requester_bare_jid: bare_jid.clone(),
                ack: stale_tx,
            },
            ForceDetachRequest {
                origin: ForceDetachOrigin::CrossNodeResume,
                target_binding: None,
                requester_bare_jid: bare_jid.clone(),
                ack: resume_tx,
            },
            ForceDetachRequest {
                origin: ForceDetachOrigin::FreshBindReplacement,
                target_binding: None,
                requester_bare_jid: bare_jid,
                ack: fresh_tx,
            },
        ];
        if reverse {
            requests.reverse();
        }
        let origin = super::super::connection::authoritative_force_detach_origin(&requests);
        assert_eq!(origin, Some(ForceDetachOrigin::FreshBindReplacement));
        super::super::connection::release_stale_force_detach_waiters_before_cross_node_cleanup(
            &mut requests,
            origin,
        );
        assert_eq!(
            stale_rx.try_recv().unwrap(),
            ForceDetachOutcome::NotPersisted,
            "the stale registry actor turn must be released before terminal cleanup reenters it"
        );
        assert_eq!(requests.len(), 2);
        assert!(matches!(
            resume_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert!(matches!(
            fresh_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        // Both remaining waiters observe the terminal cleanup result; the
        // competing resume never receives a false detached-state promise.
        for request in requests {
            request.ack.send(ForceDetachOutcome::NotPersisted).unwrap();
        }
        assert_eq!(resume_rx.await.unwrap(), ForceDetachOutcome::NotPersisted);
        assert_eq!(fresh_rx.await.unwrap(), ForceDetachOutcome::NotPersisted);
    }
}

#[tokio::test]
async fn owner_managed_force_detach_waiter_stays_queued_during_cross_node_cleanup() {
    let bare_jid = BareJid::from_str("owner-managed@example.com").expect("valid bare jid");
    let (crossnode_ack_tx, mut crossnode_ack_rx) = oneshot::channel();
    let (owner_managed_ack_tx, mut owner_managed_ack_rx) = oneshot::channel();
    let mut pending_force_detach = vec![
        ForceDetachRequest {
            origin: ForceDetachOrigin::CrossNodeResume,
            target_binding: None,
            requester_bare_jid: bare_jid.clone(),
            ack: crossnode_ack_tx,
        },
        ForceDetachRequest {
            origin: ForceDetachOrigin::OwnerManagedRetirement,
            target_binding: None,
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
    claim_enabled_cleanup_fixture(&state, &mut conn).await;
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

async fn assert_promotion_failure_survives_restart(terminal_recovery: bool) {
    let persistence =
        Arc::new(waddle_xmpp::stream_management::persistence::InMemorySmPersistence::new());
    let registry = Arc::new(InMemorySmSessionRegistry::new().with_persistence(persistence.clone()));
    let failures = Arc::new(FailCountedInsertsForMessageIdPendingStorage {
        inner: waddle_xmpp::pending_delivery::storage::InMemoryPendingDeliveryStorage::unlimited(),
        fail_id: "survives-restart",
        remaining_failures: std::sync::atomic::AtomicU32::new(if terminal_recovery {
            2
        } else {
            1
        }),
    });
    let pending: Arc<dyn waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage> =
        failures.clone();
    let state = create_test_websocket_state_with_sm_registry_and_pending_storage(
        registry.clone(),
        pending.clone(),
    )
    .await;
    let jid: FullJid = "alice@example.com/durable-terminal".parse().expect("jid");
    let stream_id = "durable-terminal-promotion";
    assert!(registry.ensure_session_claim(stream_id).await.is_some());
    let (tx, mut rx) = mpsc::channel::<OutboundStanza>(3);
    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    conn.registry_owner = Some(
        state
            .deps
            .protocol
            .connection_registry
            .register(jid.clone(), tx.clone()),
    );
    conn.sm_state.enable(stream_id.to_owned(), true, Some(300));
    claim_enabled_cleanup_fixture(&state, &mut conn).await;
    let mut message = xmpp_parsers::message::Message::new(Some(jid.to_bare().into()));
    message.from = Some("bob@example.com/phone".parse().expect("sender"));
    message.type_ = xmpp_parsers::message::MessageType::Chat;
    message.id = Some(xmpp_parsers::message::Id("survives-restart".to_owned()));
    message.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "keep this queue".to_owned(),
    );
    let _ = conn.sm_state.record_outbound(
        waddle_xmpp::parser::stanza_to_string(message).expect("serialize"),
        SmEvictionPath::DirectOutbound,
    );
    if terminal_recovery {
        conn.begin_terminal_sm_recovery();
        for id in ["accepted-suffix-1", "accepted-suffix-2"] {
            let mut suffix = xmpp_parsers::message::Message::new(Some(jid.to_bare().into()));
            suffix.from = Some("bob@example.com/phone".parse().expect("sender"));
            suffix.type_ = xmpp_parsers::message::MessageType::Chat;
            suffix.id = Some(xmpp_parsers::message::Id(id.to_owned()));
            suffix.bodies.insert(
                xmpp_parsers::message::Lang::new(),
                "accepted tail".to_owned(),
            );
            tx.send(OutboundStanza::new(Stanza::Message(suffix)))
                .await
                .expect("accepted suffix");
        }
    }
    let expected = if terminal_recovery { 3 } else { 1 };
    let _ = cleanup_connection_shutdown(&state, &mut rx, &mut conn, false).await;
    assert!(
        pending
            .list(&jid.to_bare())
            .await
            .expect("failed prefix")
            .is_empty(),
        "later accepted work must remain behind the failed prefix"
    );
    failures
        .remaining_failures
        .store(0, std::sync::atomic::Ordering::SeqCst);

    // Rebuild without consulting the old registry's in-memory retry map.
    let restarted = InMemorySmSessionRegistry::new().with_persistence(persistence.clone());
    assert_eq!(
        restarted.restore_from_persistence().await.expect("restore"),
        1,
        "failed terminal promotion must have a durable non-resumable inventory"
    );
    assert!(restarted
        .peek_session(stream_id)
        .await
        .expect("peek")
        .is_none());
    let retry = restarted.drain_expired().await.expect("janitor retry");
    assert_eq!(retry.len(), 1);
    assert_eq!(retry[0].unacked_stanzas.len(), expected);
    let sequences: Vec<_> = retry[0]
        .unacked_stanzas
        .iter()
        .map(|entry| entry.sequence)
        .collect();
    assert_eq!(
        sequences,
        (1..=u32::try_from(expected).unwrap()).collect::<Vec<_>>(),
        "accepted suffixes keep distinct FIFO sequence numbers across restart"
    );
    assert!(retry[0].unacked_stanzas[0]
        .stanza_xml
        .contains("survives-restart"));
    let settled = crate::sm_promotion::promote_displaced_sessions(
        retry,
        crate::sm_promotion::DisplacedPromotionDeps {
            sm_registry: &restarted,
            connection_registry: &state.deps.protocol.connection_registry,
            user_registry: &state.deps.protocol.user_registry,
            pending_storage: &pending,
            blocking_storage: state.deps.protocol.blocking_storage.as_ref(),
            server_domain: "example.com",
        },
    )
    .await;
    assert!(!settled.is_retrying(stream_id));
    assert_eq!(
        pending.list(&jid.to_bare()).await.expect("pending").len(),
        expected
    );
    let final_restart = InMemorySmSessionRegistry::new().with_persistence(persistence);
    assert_eq!(
        final_restart
            .restore_from_persistence()
            .await
            .expect("settled inventory"),
        0
    );
}

#[tokio::test]
async fn terminal_promotion_failure_survives_registry_restart_without_becoming_resumable() {
    assert_promotion_failure_survives_restart(false).await;
}

#[tokio::test]
async fn terminal_prefix_and_accepted_overflow_survive_restart_in_fifo_order() {
    assert_promotion_failure_survives_restart(true).await;
}

#[tokio::test]
async fn cancelled_terminal_prefix_remains_available_to_the_live_janitor() {
    let persistence =
        Arc::new(waddle_xmpp::stream_management::persistence::InMemorySmPersistence::new());
    let registry = Arc::new(InMemorySmSessionRegistry::new().with_persistence(persistence));
    let gate = Arc::new(GatedFirstInsertPendingStorage::new());
    let state = create_test_websocket_state_with_sm_registry_and_pending_storage(
        registry.clone(),
        gate.clone(),
    )
    .await;
    let jid: FullJid = "alice@example.com/cancelled-inventory".parse().unwrap();
    let (tx, mut rx) = mpsc::channel(1);
    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    conn.registry_owner = Some(
        state
            .deps
            .protocol
            .connection_registry
            .register(jid.clone(), tx),
    );
    conn.sm_state
        .enable("cancelled-inventory".to_owned(), true, Some(300));
    claim_enabled_cleanup_fixture(&state, &mut conn).await;
    let mut message = xmpp_parsers::message::Message::new(Some(jid.to_bare().into()));
    message.from = Some("bob@example.com/phone".parse().unwrap());
    message.type_ = xmpp_parsers::message::MessageType::Chat;
    message.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "cancel safely".to_owned(),
    );
    let _ = conn.sm_state.record_outbound(
        waddle_xmpp::parser::stanza_to_string(message).unwrap(),
        SmEvictionPath::DirectOutbound,
    );
    conn.begin_terminal_sm_recovery();
    let cleanup_state = state.clone();
    let cleanup = tokio::spawn(async move {
        cleanup_connection_shutdown(&cleanup_state, &mut rx, &mut conn, false).await
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        gate.wait_until_insert_blocks(),
    )
    .await
    .expect("promotion reached gated sink");
    cleanup.abort();
    assert!(cleanup.await.unwrap_err().is_cancelled());
    let retry = registry.drain_expired().await.expect("janitor custody");
    assert_eq!(retry.len(), 1);
    assert_eq!(retry[0].unacked_stanzas.len(), 1);
    let outcome = crate::sm_promotion::promote_displaced_sessions(
        retry,
        crate::sm_promotion::DisplacedPromotionDeps {
            sm_registry: &registry,
            connection_registry: &state.deps.protocol.connection_registry,
            user_registry: &state.deps.protocol.user_registry,
            pending_storage: &state.deps.protocol.pending_delivery_storage,
            blocking_storage: state.deps.protocol.blocking_storage.as_ref(),
            server_domain: "example.com",
        },
    )
    .await;
    assert!(!outcome.is_retrying("cancelled-inventory"));
    assert_eq!(
        state
            .deps
            .protocol
            .pending_delivery_storage
            .list(&jid.to_bare())
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn cancelled_initial_inventory_write_retains_the_entire_terminal_backlog() {
    let persistence = Arc::new(FailDeleteSmPersistence::new());
    persistence
        .gate_inventory_store
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let registry = Arc::new(InMemorySmSessionRegistry::new().with_persistence(persistence.clone()));
    let state = create_test_websocket_state_with_sm_registry(registry.clone()).await;
    let jid: FullJid = "alice@example.com/cancelled-backlog".parse().unwrap();
    let (tx, mut rx) = mpsc::channel(1);
    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    conn.registry_owner = Some(state.deps.protocol.connection_registry.register(jid, tx));
    conn.sm_state
        .enable("cancelled-backlog".to_owned(), true, Some(300));
    claim_enabled_cleanup_fixture(&state, &mut conn).await;
    let _ = conn.sm_state.record_outbound(
        message_frame_xml_with_id("live-prefix".to_owned()),
        SmEvictionPath::DirectOutbound,
    );
    conn.begin_terminal_sm_recovery();
    for id in ["captured-first", "captured-second"] {
        conn.record_terminal_recovery_outbound(message_frame_xml_with_id(id.to_owned()));
    }
    let cleanup_state = state.clone();
    let cleanup = tokio::spawn(async move {
        cleanup_connection_shutdown(&cleanup_state, &mut rx, &mut conn, false).await
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        persistence.append_started.notified(),
    )
    .await
    .expect("initial inventory write blocked");
    cleanup.abort();
    assert!(cleanup.await.unwrap_err().is_cancelled());
    let retry = registry
        .drain_expired()
        .await
        .expect("janitor can retry the captured backlog");
    assert_eq!(retry.len(), 1);
    assert_eq!(
        retry[0].unacked_stanzas.len(),
        3,
        "cancellation before the initial write must retain every captured backlog entry"
    );
    let sequences: Vec<_> = retry[0]
        .unacked_stanzas
        .iter()
        .map(|entry| entry.sequence)
        .collect();
    assert_eq!(sequences, vec![1, 2, 3]);
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
    claim_enabled_cleanup_fixture(&state, &mut conn).await;
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
async fn refused_detach_confirm_failure_keeps_durable_promotion_custody() {
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
    claim_enabled_cleanup_fixture(&state, &mut conn).await;
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
        0,
        "the registered inventory owns its claim until durable confirmation succeeds"
    );
    let retry = sm_registry
        .drain_expired()
        .await
        .expect("drain registered inventory");
    assert_eq!(
        retry.len(),
        1,
        "failed confirmation retains janitor custody"
    );
    assert_eq!(retry[0].stream_id, stream_id);
    assert!(
        sm_registry
            .locally_owned_claim_ids()
            .expect("owned claim inventory")
            .iter()
            .any(|owned| owned == stream_id),
        "the promotion inventory must still hold the exact claim"
    );
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
    async fn notification_recovery_high_water(
        &self,
    ) -> Result<
        Option<waddle_xmpp::pending_delivery::storage::PendingNotificationRecoveryOrdinal>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner.notification_recovery_high_water().await
    }

    async fn list_unoutboxed_archived_after(
        &self,
        after: Option<waddle_xmpp::pending_delivery::storage::PendingNotificationRecoveryOrdinal>,
        through: Option<waddle_xmpp::pending_delivery::storage::PendingNotificationRecoveryOrdinal>,
        limit: usize,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::storage::PendingNotificationRecoveryRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner
            .list_unoutboxed_archived_after(after, through, limit)
            .await
    }

    fn notification_custody_mode(
        &self,
    ) -> waddle_xmpp::pending_delivery::storage::PendingNotificationCustodyMode {
        self.inner.notification_custody_mode()
    }

    async fn mark_notification_outboxed(
        &self,
        id: &waddle_xmpp::pending_delivery::PendingRowId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.mark_notification_outboxed(id).await
    }

    fn quota_policy(&self) -> waddle_xmpp::pending_delivery::QuotaPolicy {
        self.inner.quota_policy()
    }

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

    async fn claim_archive_ordered_batch_for_session(
        &self,
        recipient: &BareJid,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
        token: &waddle_xmpp::pending_delivery::storage::PendingClaimToken,
        limit: usize,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::PendingRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner
            .claim_archive_ordered_batch_for_session(recipient, session, token, limit)
            .await
    }

    async fn release_unpushed_row_if_session(
        &self,
        id: &waddle_xmpp::pending_delivery::PendingRowId,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
        token: &waddle_xmpp::pending_delivery::storage::PendingClaimToken,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner
            .release_unpushed_row_if_session(id, session, token)
            .await
    }

    async fn mark_claim_offered(
        &self,
        claim: &waddle_xmpp::pending_delivery::storage::PendingClaim,
    ) -> Result<bool, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.mark_claim_offered(claim).await
    }

    async fn release_unqueued_offer(
        &self,
        claim: &waddle_xmpp::pending_delivery::storage::PendingClaim,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.release_unqueued_offer(claim).await
    }

    async fn list_unoffered_claims(
        &self,
        recipient: &BareJid,
        after: Option<&waddle_xmpp::pending_delivery::PendingRowId>,
        claimed_before_ms: i64,
        limit: usize,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::storage::PendingClaim>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner
            .list_unoffered_claims(recipient, after, claimed_before_ms, limit)
            .await
    }

    async fn delete_unsequenced_claim(
        &self,
        claim: &waddle_xmpp::pending_delivery::storage::PendingClaim,
        phase: waddle_xmpp::pending_delivery::storage::PendingClaimPhase,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.delete_unsequenced_claim(claim, phase).await
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
        expected_fence: Option<&waddle_xmpp::stream_management::persistence::SmClaimFence>,
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
            .release_rows_for_outbound_sequences(recipient, session, sequences, expected_fence)
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
        authority: waddle_xmpp::pending_delivery::storage::PendingAckAuthority<'_>,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner
            .delete_acked_in_window(session, from_exclusive, to_inclusive, authority)
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

#[tokio::test]
async fn failed_pending_ownership_preflight_never_releases_a_durable_replay_copy() {
    let persistence =
        Arc::new(waddle_xmpp::stream_management::persistence::InMemorySmPersistence::new());
    let registry = Arc::new(InMemorySmSessionRegistry::new().with_persistence(persistence));
    let pending = Arc::new(FailOnceListAndReleasePendingStorage::new());
    let state = create_test_websocket_state_with_sm_registry_and_pending_storage(
        registry.clone(),
        pending.clone(),
    )
    .await;
    let jid: FullJid = "alice@example.com/preflight-inventory".parse().unwrap();
    let stream_id = "preflight-inventory";
    seed_claimed_pending_row(&state, &jid.to_bare(), stream_id, 1).await;
    handoff_expired_claimed_session(
        &state,
        stream_id,
        &jid,
        vec![waddle_xmpp::stream_management::DetachedUnackedStanza {
            provenance: Default::default(),
            ingress_receipts: Vec::new(),
            sequence: 1,
            stanza_xml: message_frame_xml_with_id("terminal-release-replay-copy".to_owned()),
            original_receipt_at: chrono::Utc::now(),
        }],
    )
    .await;
    let mut inventory = registry.drain_expired().await.unwrap().pop().unwrap();
    pending
        .fail_next_list
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let outcome = crate::sm_promotion::release_row_backed_replay_copies(
        &registry,
        &state.deps.protocol.pending_delivery_storage,
        &mut inventory,
    )
    .await;
    assert!(outcome.ownership_unknown);
    assert!(!outcome.released_rows);
    let rows = state
        .deps
        .protocol
        .pending_delivery_storage
        .list(&jid.to_bare())
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0]
            .flushed_in_session
            .as_ref()
            .map(|session| session.as_str()),
        Some(stream_id)
    );
    inventory = registry
        .register_promotion_session(&inventory)
        .await
        .unwrap();
    assert_eq!(inventory.unacked_stanzas.len(), 1);

    let released = crate::sm_promotion::release_row_backed_replay_copies(
        &registry,
        &state.deps.protocol.pending_delivery_storage,
        &mut inventory,
    )
    .await;
    assert!(released.released_rows);
    assert!(
        registry
            .register_promotion_session(&inventory)
            .await
            .unwrap()
            .unacked_stanzas
            .is_empty(),
        "canonical hydration must not recreate the released pending replay copy"
    );
    assert_eq!(
        state
            .deps
            .protocol
            .pending_delivery_storage
            .list(&jid.to_bare())
            .await
            .unwrap()
            .len(),
        1
    );
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
    let old_stream_id = "ownership-unknown-expired-claim".to_string();
    seed_claimed_pending_row(state.as_ref(), &jid.to_bare(), &old_stream_id, 1).await;
    handoff_expired_claimed_session(
        state.as_ref(),
        &old_stream_id,
        &jid,
        vec![waddle_xmpp::stream_management::DetachedUnackedStanza {
            provenance: Default::default(),
            ingress_receipts: Vec::new(),
            sequence: 1,
            stanza_xml: message_frame_xml_with_id("terminal-release-replay-copy".to_string()),
            original_receipt_at: chrono::Utc::now(),
        }],
    )
    .await;

    failing_storage.arm();
    crate::server::session_janitors::run_sm_expiry_sweep(&state).await;

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

    // Ownership discovery failed closed before attempting release. The next
    // sweep consumes its one-shot release failure, and a final sweep settles.
    crate::server::session_janitors::run_sm_expiry_sweep(&state).await;
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

/// Codex round-9 finding 2: the janitor must re-drive released row-backed
/// rows BEFORE promoting the remaining queue, otherwise later unacked traffic
/// reaches a live replacement ahead of the earlier released row.
#[tokio::test]
async fn janitor_redrives_released_rows_before_promoting_remainder() {
    use waddle_xmpp::stream_management::DetachedUnackedStanza;
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/janitor-fifo".parse().expect("jid");
    let stream_id = "janitor-fifo-expired".to_string();
    seed_claimed_pending_row(state.as_ref(), &jid.to_bare(), &stream_id, 1).await;

    handoff_expired_claimed_session(
        state.as_ref(),
        &stream_id,
        &jid,
        vec![
            DetachedUnackedStanza {
                provenance: Default::default(),
                ingress_receipts: Vec::new(),
                sequence: 1,
                stanza_xml: message_frame_xml_with_id("row-backed-copy".to_string()),
                original_receipt_at: chrono::Utc::now(),
            },
            DetachedUnackedStanza {
                provenance: Default::default(),
                ingress_receipts: Vec::new(),
                sequence: 2,
                stanza_xml: transient_chat_message_xml("janitor-later", &jid.to_bare()),
                original_receipt_at: chrono::Utc::now(),
            },
        ],
    )
    .await;

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

    let first = replacement_rx
        .try_recv()
        .expect("released pending row is re-driven first");
    assert!(
        replacement_rx.try_recv().is_err(),
        "later promotion waits until the earlier row is written"
    );
    state
        .deps
        .protocol
        .pending_delivery_storage
        .record_pushed_at(
            first.pending_row_id.as_ref().expect("pending replay row"),
            1,
        )
        .await
        .expect("model successful replacement wire write");
    crate::server::session_janitors::run_sm_expiry_sweep(&state).await;
    let second = replacement_rx
        .try_recv()
        .expect("later traffic follows the written prefix");
    assert!(
        replacement_rx.try_recv().is_err(),
        "no duplicate tail delivery"
    );
    let delivered_ids: Vec<_> = [first, second]
        .into_iter()
        .map(|delivered| {
            let Stanza::Message(message) = delivered.stanza else {
                panic!("expected replay message")
            };
            message.id.map(|id| id.0)
        })
        .collect();
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
    async fn notification_recovery_high_water(
        &self,
    ) -> Result<
        Option<waddle_xmpp::pending_delivery::storage::PendingNotificationRecoveryOrdinal>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner.notification_recovery_high_water().await
    }

    async fn list_unoutboxed_archived_after(
        &self,
        after: Option<waddle_xmpp::pending_delivery::storage::PendingNotificationRecoveryOrdinal>,
        through: Option<waddle_xmpp::pending_delivery::storage::PendingNotificationRecoveryOrdinal>,
        limit: usize,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::storage::PendingNotificationRecoveryRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner
            .list_unoutboxed_archived_after(after, through, limit)
            .await
    }

    fn notification_custody_mode(
        &self,
    ) -> waddle_xmpp::pending_delivery::storage::PendingNotificationCustodyMode {
        self.inner.notification_custody_mode()
    }

    async fn mark_notification_outboxed(
        &self,
        id: &waddle_xmpp::pending_delivery::PendingRowId,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.mark_notification_outboxed(id).await
    }

    fn quota_policy(&self) -> waddle_xmpp::pending_delivery::QuotaPolicy {
        self.inner.quota_policy()
    }

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

    async fn claim_archive_ordered_batch_for_session(
        &self,
        recipient: &BareJid,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
        token: &waddle_xmpp::pending_delivery::storage::PendingClaimToken,
        limit: usize,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::PendingRow>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner
            .claim_archive_ordered_batch_for_session(recipient, session, token, limit)
            .await
    }

    async fn release_unpushed_row_if_session(
        &self,
        id: &waddle_xmpp::pending_delivery::PendingRowId,
        session: &waddle_xmpp::pending_delivery::SmSessionId,
        token: &waddle_xmpp::pending_delivery::storage::PendingClaimToken,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner
            .release_unpushed_row_if_session(id, session, token)
            .await
    }

    async fn mark_claim_offered(
        &self,
        claim: &waddle_xmpp::pending_delivery::storage::PendingClaim,
    ) -> Result<bool, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.mark_claim_offered(claim).await
    }

    async fn release_unqueued_offer(
        &self,
        claim: &waddle_xmpp::pending_delivery::storage::PendingClaim,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.release_unqueued_offer(claim).await
    }

    async fn list_unoffered_claims(
        &self,
        recipient: &BareJid,
        after: Option<&waddle_xmpp::pending_delivery::PendingRowId>,
        claimed_before_ms: i64,
        limit: usize,
    ) -> Result<
        Vec<waddle_xmpp::pending_delivery::storage::PendingClaim>,
        waddle_xmpp::pending_delivery::storage::PendingStorageError,
    > {
        self.inner
            .list_unoffered_claims(recipient, after, claimed_before_ms, limit)
            .await
    }

    async fn delete_unsequenced_claim(
        &self,
        claim: &waddle_xmpp::pending_delivery::storage::PendingClaim,
        phase: waddle_xmpp::pending_delivery::storage::PendingClaimPhase,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner.delete_unsequenced_claim(claim, phase).await
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
        authority: waddle_xmpp::pending_delivery::storage::PendingAckAuthority<'_>,
    ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
        self.inner
            .delete_acked_in_window(session, from_exclusive, to_inclusive, authority)
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
async fn sm_resume_legacy_snapshot_without_a_principal_is_not_authorized() {
    use waddle_xmpp::stream_management::DetachedSession;

    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(DetachedSession {
            promotion_custody:
                waddle_xmpp::stream_management::PromotionCaptureCustody::never_claimed(),
            stream_id: "stream-legacy-null-context".to_string(),
            user_id: "alice@example.com".to_string(),
            jid: jid.clone(),
            occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            inbound_count: 0,
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
async fn non_resumable_sm_enable_after_bind_returns_enabled_and_tracks_counters() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    register_sm_publish_owner(state.as_ref(), &mut conn, &jid);

    let responses = handle_xmpp_frame(
        "<enable xmlns='urn:xmpp:sm:3'/>",
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;
    assert_eq!(responses.len(), 1);
    let el = Element::from_str(&responses[0]).expect("xml");
    assert_eq!(el.name(), "enabled");
    assert_eq!(el.attr("resume"), None);
    assert!(el.attr("id").filter(|s| !s.is_empty()).is_some());
    assert!(
        !conn.sm_state.enabled,
        "SM must remain unpublished until the <enabled/> write succeeds"
    );
    conn.publish_pending_sm_enable(state.as_ref());
    assert!(conn.sm_state.enabled);
    assert!(!conn.sm_state.is_resumable());

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
    assert_eq!(conn.sm_state.inbound_count(), 1);

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
        "<enable xmlns='urn:xmpp:sm:3'/>",
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
        "<enable xmlns='urn:xmpp:sm:3'/>",
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

/// Enable non-resumable SM on a fresh ready connection and return the
/// negotiated stream id. Shared setup for the live `<a h='N'/>` validation
/// tests (issue #1099): a stream the durable authority does not hold
/// validates and settles its acknowledgements on the connection.
async fn enable_sm_for_live_ack_tests(
    state: &super::super::state::WebSocketState,
    conn: &mut WsConnState,
    jid: &FullJid,
) -> String {
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    register_sm_publish_owner(state, conn, jid);
    let responses = handle_xmpp_frame(
        "<enable xmlns='urn:xmpp:sm:3'/>",
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

/// Build a fresh detached snapshot, claim it for resume, let it expire while
/// claimed, then release it through issue #1667's off-map promotion handoff.
async fn handoff_expired_claimed_session(
    state: &super::super::state::WebSocketState,
    stream_id: &str,
    jid: &FullJid,
    unacked_stanzas: Vec<waddle_xmpp::stream_management::DetachedUnackedStanza>,
) {
    let registry = &state.deps.protocol.sm_session_registry;
    registry
        .store_session(waddle_xmpp::stream_management::DetachedSession {
            promotion_custody:
                waddle_xmpp::stream_management::PromotionCaptureCustody::never_claimed(),
            stream_id: stream_id.to_string(),
            user_id: jid.to_bare().to_string(),
            jid: jid.clone(),
            occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            inbound_count: 0,
            outbound_count: unacked_stanzas.len() as u32,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas,
            max_resume_time: Some(1),
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
        .expect("store fresh session before claimed expiry");
    assert!(registry
        .claim_session(stream_id)
        .await
        .expect("claim session before expiry")
        .is_some());
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    registry
        .release_claim(stream_id)
        .await
        .expect("handoff expired claimed session");
    assert!(registry
        .claim_session(stream_id)
        .await
        .expect("promotion-owned session is not resumable")
        .is_none());
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
        conn.sm_state.stanzas_to_resend(0).len(),
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
        conn.sm_state.stanzas_to_resend(0).len(),
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
        conn.sm_state.stanzas_to_resend(0).len(),
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
        promotion_custody: waddle_xmpp::stream_management::PromotionCaptureCustody::never_claimed(),
        stream_id: stream_id.clone(),
        user_id: "alice@example.com".to_string(),
        jid: jid.clone(),
        occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
        inbound_count: 0,
        outbound_count: 2,
        last_acked: u32::MAX - 1,
        replay_gap_through: None,
        unacked_stanzas: vec![
            waddle_xmpp::stream_management::DetachedUnackedStanza {
                provenance: Default::default(),
                ingress_receipts: Vec::new(),
                sequence: u32::MAX,
                stanza_xml: "<message xmlns='jabber:client' id='pre-wrap'/>".to_string(),
                original_receipt_at: chrono::Utc::now(),
            },
            waddle_xmpp::stream_management::DetachedUnackedStanza {
                provenance: Default::default(),
                ingress_receipts: Vec::new(),
                sequence: 1,
                stanza_xml: "<message xmlns='jabber:client' id='post-wrap-1'/>".to_string(),
                original_receipt_at: chrono::Utc::now(),
            },
            waddle_xmpp::stream_management::DetachedUnackedStanza {
                provenance: Default::default(),
                ingress_receipts: Vec::new(),
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
        conn.sm_state.stanzas_to_resend(u32::MAX).len(),
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
        conn.sm_state.stanzas_to_resend(1).len(),
        1,
        "ignored ack must not purge the replay queue"
    );
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
            promotion_custody:
                waddle_xmpp::stream_management::PromotionCaptureCustody::never_claimed(),
            stream_id: stream_id.clone(),
            user_id: "alice@example.com".to_string(),
            jid: alice_jid,
            occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            inbound_count: 0,
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
            promotion_custody:
                waddle_xmpp::stream_management::PromotionCaptureCustody::never_claimed(),
            stream_id: stream_id.clone(),
            user_id: "alice@example.com".to_string(),
            jid: alice_jid,
            occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            inbound_count: 0,
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
            promotion_custody:
                waddle_xmpp::stream_management::PromotionCaptureCustody::never_claimed(),
            stream_id: sent_stream_id.clone(),
            user_id: "alice@example.com".to_string(),
            jid: alice_laptop.clone(),
            occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            inbound_count: 0,
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
    alice.authenticated_session = Some(create_test_session(state.as_ref(), "alice").await);
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
            promotion_custody:
                waddle_xmpp::stream_management::PromotionCaptureCustody::never_claimed(),
            stream_id: received_stream_id.clone(),
            user_id: "alice@example.com".to_string(),
            jid: alice_laptop,
            occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            inbound_count: 0,
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
    bob.authenticated_session = Some(create_test_session(state.as_ref(), "bob").await);
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
    // Roster contacts must be existing local accounts.
    super::seed_local_account(state.as_ref(), "alice").await;
    super::seed_local_account(state.as_ref(), "bob").await;

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
    crate::server::routes::websocket::test_bindings::sync_test_bindings(state.as_ref()).await;
    let _ = handle_xmpp_frame(
            r#"<iq xmlns="jabber:client" type="get" id="alice-roster"><query xmlns="jabber:iq:roster"/></iq>"#,
            "example.com",
            state.as_ref(),
            &mut alice,
        )
        .await;
    crate::server::routes::websocket::test_bindings::sync_test_bindings(state.as_ref()).await;
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
    crate::server::routes::websocket::test_bindings::sync_test_bindings(state.as_ref()).await;
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

    crate::server::routes::websocket::test_bindings::sync_test_bindings(state.as_ref()).await;
    let _ = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client" type="subscribed" to="bob@example.com"/>"#,
        "example.com",
        state.as_ref(),
        &mut alice,
    )
    .await;
    while bob_rx.try_recv().is_ok() {}

    crate::server::routes::websocket::test_bindings::sync_test_bindings(state.as_ref()).await;
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
async fn subscription_approval_replays_current_presence_from_detached_available_resource() {
    use waddle_xmpp::stream_management::DetachedSession;
    let state = create_test_websocket_state().await;
    // Roster contacts must be existing local accounts.
    super::seed_local_account(state.as_ref(), "alice").await;
    super::seed_local_account(state.as_ref(), "bob").await;

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
    crate::server::routes::websocket::test_bindings::sync_test_bindings(state.as_ref()).await;
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
            promotion_custody:
                waddle_xmpp::stream_management::PromotionCaptureCustody::never_claimed(),
            stream_id,
            user_id: "alice@example.com".to_string(),
            jid: alice_web_jid.clone(),
            occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            inbound_count: 0,
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
    crate::server::routes::websocket::test_bindings::sync_test_bindings(state.as_ref()).await;
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
    // Roster contacts must be existing local accounts.
    super::seed_local_account(state.as_ref(), "alice").await;
    super::seed_local_account(state.as_ref(), "bob").await;

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
            promotion_custody:
                waddle_xmpp::stream_management::PromotionCaptureCustody::never_claimed(),
            stream_id: "stream-detached-probe".to_string(),
            user_id: "alice@example.com".to_string(),
            jid: alice_jid,
            occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            inbound_count: 0,
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
    // Roster contacts must be existing local accounts.
    super::seed_local_account(state.as_ref(), "alice").await;
    super::seed_local_account(state.as_ref(), "bob").await;

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
                promotion_custody:
                    waddle_xmpp::stream_management::PromotionCaptureCustody::never_claimed(),
                stream_id: stream_id.to_string(),
                user_id: "alice@example.com".to_string(),
                jid,
                occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
                inbound_count: 0,
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
            promotion_custody:
                waddle_xmpp::stream_management::PromotionCaptureCustody::never_claimed(),
            stream_id: "stream-detached-probe-denied".to_string(),
            user_id: "alice@example.com".to_string(),
            jid: alice_jid,
            occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            inbound_count: 0,
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
    // Roster contacts must be existing local accounts.
    super::seed_local_account(state.as_ref(), "alice").await;
    super::seed_local_account(state.as_ref(), "bob").await;

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
    crate::server::routes::websocket::test_bindings::sync_test_bindings(state.as_ref()).await;
    let _ = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client" type="subscribe" to="alice@example.com"/>"#,
        "example.com",
        state.as_ref(),
        &mut bob,
    )
    .await;
    let mut alice = WsConnState::new();
    alice.phase = ConnectionPhase::ready(alice_jid.clone(), false);
    crate::server::routes::websocket::test_bindings::sync_test_bindings(state.as_ref()).await;
    let _ = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client" type="subscribed" to="bob@example.com"/>"#,
        "example.com",
        state.as_ref(),
        &mut alice,
    )
    .await;
    while bob_rx.try_recv().is_ok() {}
    while alice_sibling_rx.try_recv().is_ok() {}

    crate::server::routes::websocket::test_bindings::sync_test_bindings(state.as_ref()).await;
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
        Some(&owner_session.clone()),
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
    claim_enabled_cleanup_fixture(&state, &mut conn).await;
    // A production enabled stream always carries a live claim fence;
    // without one the fenceless teardown promotes terminally instead of
    // taking the detach path this test exercises.
    drop(
        state
            .deps
            .protocol
            .sm_session_registry
            .ensure_session_claim("stream-detach")
            .await
            .expect("enable claim"),
    );
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
        .entry(&jid)
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
    claim_enabled_cleanup_fixture(&state, &mut conn).await;
    // A production enabled stream always carries a live claim fence;
    // without one the fenceless teardown promotes terminally instead of
    // taking the detach path this test exercises.
    drop(
        state
            .deps
            .protocol
            .sm_session_registry
            .ensure_session_claim("stream-detach-actor")
            .await
            .expect("enable claim"),
    );

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
    let mut conn = WsConnState::new();

    let _ = handle_muc_join_with_occupancy_session(
        state.as_ref(),
        "example.com",
        &room_jid,
        &jid,
        "alice",
        None,
        (conn.occupancy_session, Some(&owner_session.clone())),
    )
    .await;
    let (tx, mut rx) = mpsc::channel::<OutboundStanza>(4);
    let owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), tx);

    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    conn.authenticated_session = Some(owner_session.clone());
    conn.registry_owner = Some(owner);
    conn.sm_state
        .enable("stream-close".to_string(), false, Some(300));
    claim_enabled_cleanup_fixture(&state, &mut conn).await;

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
        Some(&owner_session.clone()),
    )
    .await;

    let (tx, rx) = mpsc::channel::<OutboundStanza>(4);
    let owner = super::register_test_connection(state.as_ref(), &jid, tx).await;
    state
        .deps
        .protocol
        .connection_registry
        .update_presence(&jid, true, 0);
    crate::server::routes::websocket::test_bindings::sync_test_bindings(state.as_ref()).await;
    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    conn.registry_owner = Some(owner);
    conn.sm_state
        .enable("terminal-replacement-stream".to_string(), true, Some(300));
    claim_enabled_cleanup_fixture(&state, &mut conn).await;
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
        Some(&owner_session),
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
    let mut conn = WsConnState::new();

    let _ = handle_muc_join_with_occupancy_session(
        state.as_ref(),
        "example.com",
        &room_jid,
        &jid,
        "alice",
        None,
        (conn.occupancy_session, Some(&owner_session)),
    )
    .await;

    let (tx, mut rx) = mpsc::channel::<OutboundStanza>(4);
    let owner = super::register_test_connection(state.as_ref(), &jid, tx).await;
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    conn.registry_owner = Some(owner);
    conn.sm_state.enable(
        "terminal-no-replacement-stream".to_string(),
        true,
        Some(300),
    );
    claim_enabled_cleanup_fixture(&state, &mut conn).await;
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
    let occupancy_session = waddle_xmpp_core::OccupancySessionGeneration::mint();

    // Put alice in the room, as if she'd detached with SM.
    let _ = handle_muc_join_with_occupancy_session(
        state.as_ref(),
        "example.com",
        &room_jid,
        &jid,
        "alice",
        None,
        (occupancy_session, Some(&owner_session.clone())),
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
            promotion_custody:
                waddle_xmpp::stream_management::PromotionCaptureCustody::never_claimed(),
            stream_id: stream_id.clone(),
            user_id: "alice@example.com".to_string(),
            jid: jid.clone(),
            occupancy_session,
            inbound_count: 0,
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
    cleanup_muc_presence_for_jid(
        state.as_ref(),
        &drained[0].jid,
        waddle_xmpp::muc::room_actor::LeaveSessionSelector::Generation(
            drained[0].occupancy_session,
        ),
    )
    .await;

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
        Some(&owner_session.clone()),
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
    claim_enabled_cleanup_fixture(&state, &mut conn).await;
    // A production enabled stream always carries a live claim fence;
    // without one the fenceless teardown promotes terminally instead of
    // taking the detach path this test exercises.
    drop(
        state
            .deps
            .protocol
            .sm_session_registry
            .ensure_session_claim("stream-detach-call")
            .await
            .expect("enable claim"),
    );

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
        conn.sm_state.stanzas_to_resend(0).len(),
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

/// Exercise failed ordering-release handoff through real terminal cleanup,
/// sharing the same database between MAM, ingress ordering, and pending rows.
mod terminal_ordering_release_tests {
    use super::*;
    use crate::pending_delivery::DatabasePendingDeliveryStorage;
    use waddle_xmpp::pending_delivery::storage::{
        PendingClaimToken, PendingDeliveryStorage, PendingStorageError,
    };
    use waddle_xmpp::pending_delivery::{
        InsertOutcome, PendingPayload, PendingRow, PendingRowId, QuotaPolicy, SmSessionId,
    };

    struct HeldOrderingReleaseStorage {
        inner: std::sync::OnceLock<DatabasePendingDeliveryStorage>,
        fail_releases: std::sync::atomic::AtomicBool,
        failed: Notify,
        claim_calls: std::sync::atomic::AtomicUsize,
        pause_claim_at: std::sync::atomic::AtomicUsize,
        claim_paused: Notify,
        resume_claim: Semaphore,
    }

    impl HeldOrderingReleaseStorage {
        fn new(fail_releases: bool) -> Self {
            Self {
                inner: std::sync::OnceLock::new(),
                fail_releases: std::sync::atomic::AtomicBool::new(fail_releases),
                failed: Notify::new(),
                claim_calls: std::sync::atomic::AtomicUsize::new(0),
                pause_claim_at: std::sync::atomic::AtomicUsize::new(0),
                claim_paused: Notify::new(),
                resume_claim: Semaphore::new(0),
            }
        }

        fn inner(&self) -> &DatabasePendingDeliveryStorage {
            self.inner
                .get()
                .expect("shared database initialized before use")
        }
    }
    #[async_trait::async_trait]
    impl PendingDeliveryStorage for HeldOrderingReleaseStorage {
        async fn notification_recovery_high_water(
            &self,
        ) -> Result<
            Option<waddle_xmpp::pending_delivery::storage::PendingNotificationRecoveryOrdinal>,
            waddle_xmpp::pending_delivery::storage::PendingStorageError,
        > {
            self.inner().notification_recovery_high_water().await
        }

        async fn list_unoutboxed_archived_after(
            &self,
            after: Option<
                waddle_xmpp::pending_delivery::storage::PendingNotificationRecoveryOrdinal,
            >,
            through: Option<
                waddle_xmpp::pending_delivery::storage::PendingNotificationRecoveryOrdinal,
            >,
            limit: usize,
        ) -> Result<
            Vec<waddle_xmpp::pending_delivery::storage::PendingNotificationRecoveryRow>,
            waddle_xmpp::pending_delivery::storage::PendingStorageError,
        > {
            self.inner()
                .list_unoutboxed_archived_after(after, through, limit)
                .await
        }

        fn notification_custody_mode(
            &self,
        ) -> waddle_xmpp::pending_delivery::storage::PendingNotificationCustodyMode {
            self.inner().notification_custody_mode()
        }

        async fn mark_notification_outboxed(
            &self,
            id: &waddle_xmpp::pending_delivery::PendingRowId,
        ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
            self.inner().mark_notification_outboxed(id).await
        }

        fn quota_policy(&self) -> waddle_xmpp::pending_delivery::QuotaPolicy {
            self.inner().quota_policy()
        }

        async fn insert(&self, row: PendingRow) -> Result<InsertOutcome, PendingStorageError> {
            self.inner().insert(row).await
        }
        async fn list(&self, recipient: &BareJid) -> Result<Vec<PendingRow>, PendingStorageError> {
            self.inner().list(recipient).await
        }
        async fn claim_for_session(
            &self,
            recipient: &BareJid,
            session: &waddle_xmpp::pending_delivery::SmSessionId,
        ) -> Result<Vec<PendingRow>, PendingStorageError> {
            self.inner().claim_for_session(recipient, session).await
        }
        async fn claim_batch_for_session(
            &self,
            recipient: &BareJid,
            session: &waddle_xmpp::pending_delivery::SmSessionId,
            after: Option<&PendingRowId>,
            limit: usize,
        ) -> Result<Vec<PendingRow>, PendingStorageError> {
            self.inner()
                .claim_batch_for_session(recipient, session, after, limit)
                .await
        }
        async fn claim_archive_ordered_batch_for_session(
            &self,
            recipient: &BareJid,
            session: &SmSessionId,
            token: &PendingClaimToken,
            limit: usize,
        ) -> Result<Vec<PendingRow>, PendingStorageError> {
            let call = self
                .claim_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            if call
                == self
                    .pause_claim_at
                    .load(std::sync::atomic::Ordering::SeqCst)
            {
                self.claim_paused.notify_one();
                self.resume_claim
                    .acquire()
                    .await
                    .expect("release paused claim")
                    .forget();
            }
            self.inner()
                .claim_archive_ordered_batch_for_session(recipient, session, token, limit)
                .await
        }
        async fn release_unpushed_row_if_session(
            &self,
            id: &PendingRowId,
            session: &SmSessionId,
            token: &PendingClaimToken,
        ) -> Result<u64, PendingStorageError> {
            if self.fail_releases.load(std::sync::atomic::Ordering::SeqCst) {
                self.failed.notify_one();
                return Err(PendingStorageError::Other(
                    "held ordering release outage".into(),
                ));
            }
            self.inner()
                .release_unpushed_row_if_session(id, session, token)
                .await
        }
        async fn mark_claim_offered(
            &self,
            claim: &waddle_xmpp::pending_delivery::storage::PendingClaim,
        ) -> Result<bool, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
            self.inner().mark_claim_offered(claim).await
        }

        async fn release_unqueued_offer(
            &self,
            claim: &waddle_xmpp::pending_delivery::storage::PendingClaim,
        ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
            self.inner().release_unqueued_offer(claim).await
        }

        async fn list_unoffered_claims(
            &self,
            recipient: &BareJid,
            after: Option<&waddle_xmpp::pending_delivery::PendingRowId>,
            claimed_before_ms: i64,
            limit: usize,
        ) -> Result<
            Vec<waddle_xmpp::pending_delivery::storage::PendingClaim>,
            waddle_xmpp::pending_delivery::storage::PendingStorageError,
        > {
            self.inner()
                .list_unoffered_claims(recipient, after, claimed_before_ms, limit)
                .await
        }

        async fn delete_unsequenced_claim(
            &self,
            claim: &waddle_xmpp::pending_delivery::storage::PendingClaim,
            phase: waddle_xmpp::pending_delivery::storage::PendingClaimPhase,
        ) -> Result<u64, waddle_xmpp::pending_delivery::storage::PendingStorageError> {
            self.inner().delete_unsequenced_claim(claim, phase).await
        }

        async fn delete_claimed(
            &self,
            session: &waddle_xmpp::pending_delivery::SmSessionId,
        ) -> Result<u64, PendingStorageError> {
            self.inner().delete_claimed(session).await
        }
        async fn delete_row(&self, id: &PendingRowId) -> Result<u64, PendingStorageError> {
            self.inner().delete_row(id).await
        }
        async fn release_claim(
            &self,
            session: &waddle_xmpp::pending_delivery::SmSessionId,
        ) -> Result<u64, PendingStorageError> {
            self.inner().release_claim(session).await
        }
        async fn release_row(&self, id: &PendingRowId) -> Result<u64, PendingStorageError> {
            self.inner().release_row(id).await
        }
        async fn record_pushed_at(
            &self,
            id: &PendingRowId,
            sequence: u32,
        ) -> Result<u64, PendingStorageError> {
            self.inner().record_pushed_at(id, sequence).await
        }
        async fn delete_acked_in_window(
            &self,
            session: &waddle_xmpp::pending_delivery::SmSessionId,
            from_exclusive: u32,
            to_inclusive: u32,
            authority: waddle_xmpp::pending_delivery::storage::PendingAckAuthority<'_>,
        ) -> Result<u64, PendingStorageError> {
            self.inner()
                .delete_acked_in_window(session, from_exclusive, to_inclusive, authority)
                .await
        }
        async fn list_orphaned_claims(
            &self,
            live: &[waddle_xmpp::pending_delivery::SmSessionId],
            claimed_before_ms: i64,
        ) -> Result<
            Vec<(PendingRowId, waddle_xmpp::pending_delivery::SmSessionId)>,
            PendingStorageError,
        > {
            self.inner()
                .list_orphaned_claims(live, claimed_before_ms)
                .await
        }
        async fn count(&self, recipient: &BareJid) -> Result<u32, PendingStorageError> {
            self.inner().count(recipient).await
        }
        async fn delete_older_than(
            &self,
            cutoff: chrono::DateTime<chrono::Utc>,
        ) -> Result<u64, PendingStorageError> {
            self.inner().delete_older_than(cutoff).await
        }
        async fn scrub_for_tombstone(
            &self,
            target: &waddle_xmpp::tombstone::TombstoneTarget,
        ) -> Result<u64, PendingStorageError> {
            self.inner().scrub_for_tombstone(target).await
        }
    }

    async fn seed_archive_row(
        state: &super::super::super::state::WebSocketState,
        recipient: &BareJid,
        id: &str,
        session: &SmSessionId,
    ) -> PendingRowId {
        let sender: jid::Jid = "bob@example.com/phone".parse().expect("sender");
        let mut message = xmpp_parsers::message::Message::new(Some(recipient.clone().into()));
        message.from = Some(sender.clone());
        message.id = Some(xmpp_parsers::message::Id(id.to_owned()));
        message.type_ = xmpp_parsers::message::MessageType::Chat;
        message
            .bodies
            .insert(xmpp_parsers::message::Lang::new(), id.to_owned());
        let stamp = waddle_xmpp_core::xep0359::StanzaId::new(id, recipient.clone().into());
        let mut archived =
            waddle_xmpp::mam::ArchivedMessage::for_test(sender, recipient.clone().into());
        archived.id = id.to_owned();
        archived.stanza_id = Some(stamp.clone());
        archived.stanza_xml = Some(stanza_to_xml(&Stanza::Message(message)));
        state
            .deps
            .protocol
            .mam_storage
            .store_message(recipient, &archived)
            .await
            .expect("archive pending message");
        let row_id = PendingRowId::fresh();
        state
            .deps
            .protocol
            .pending_delivery_storage
            .insert(PendingRow {
                id: row_id.clone(),
                recipient: recipient.clone(),
                original_receipt_at: chrono::Utc::now(),
                payload: PendingPayload::Archived(stamp),
                flushed_in_session: None,
                outbound_sequence: None,
            })
            .await
            .expect("seed archived pending row");
        // The production database intentionally inserts every new row unclaimed;
        // establish custody through its real claim and writer-stamp operations.
        let claimed = state
            .deps
            .protocol
            .pending_delivery_storage
            .claim_archive_ordered_batch_for_session(
                recipient,
                session,
                &PendingClaimToken::fresh(),
                1,
            )
            .await
            .expect("claim seeded archive row");
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].id, row_id);
        assert_eq!(
            state
                .deps
                .protocol
                .pending_delivery_storage
                .record_pushed_at(&row_id, 1)
                .await
                .expect("stamp seeded row's outbound sequence"),
            1
        );
        row_id
    }

    #[tokio::test]
    async fn pending_claim_janitor_recovers_lost_retry_on_live_stream_without_duplicate_offer() {
        let storage = Arc::new(HeldOrderingReleaseStorage::new(true));
        let state = create_test_websocket_state_with_sm_registry_and_pending_storage(
            Arc::new(InMemorySmSessionRegistry::new()),
            storage.clone(),
        )
        .await;
        assert!(storage
            .inner
            .set(
                DatabasePendingDeliveryStorage::from_database(
                    state.deps.app_state.db_pool.global().clone(),
                    QuotaPolicy::Unlimited,
                )
                .await
                .expect("shared pending database")
            )
            .is_ok());
        let resource: FullJid = "alice@example.com/lost-retry"
            .parse()
            .expect("local resource");
        let recipient = resource.to_bare();
        let phone = SmSessionId::new("phone-predecessor");
        seed_archive_row(state.as_ref(), &recipient, "lost-retry-a", &phone).await;
        let b = seed_archive_row(
            state.as_ref(),
            &recipient,
            "lost-retry-b",
            &SmSessionId::new("seed-b"),
        )
        .await;
        storage
            .release_row(&b)
            .await
            .expect("B awaits initial flush");
        let (tx, mut rx) = mpsc::channel(8);
        let owner = register_test_connection(state.as_ref(), &resource, tx).await;
        state
            .deps
            .protocol
            .connection_registry
            .update_presence(&resource, true, 0);
        let session = SmSessionId::new("same-live-sm-session");
        assert!(state
            .deps
            .protocol
            .connection_registry
            .set_sm_stream_id_if_owner(&resource, &owner, Some(session.clone())));
        let entry = state
            .deps
            .protocol
            .connection_registry
            .entry_if_owner(&resource, &owner)
            .expect("live local entry");
        assert!(
            entry.claim_offline_flush(),
            "initial presence has spent its flush opportunity"
        );
        let retry = {
            let state = state.clone();
            let resource = resource.clone();
            let session = session.clone();
            tokio::spawn(async move {
                let resolver = crate::pending_delivery::MamArchiveResolver {
                    mam_storage: state.deps.protocol.mam_storage.clone(),
                };
                crate::pending_delivery::flush_for_resource_with_retry(
                    &state.deps.protocol.pending_delivery_storage,
                    &state.deps.protocol.connection_registry,
                    &resource.to_bare(),
                    &resource,
                    crate::pending_delivery::FlushContext {
                        server_domain: "example.com",
                        sm_session: Some(&session),
                        blocking_storage: Some(&state.deps.protocol.blocking_storage),
                        owner: Some(&owner),
                        archive_resolver: &resolver,
                        dispatch_gate: Some(state.deps.protocol.ingress.as_ref()),
                    },
                )
                .await
            })
        };
        tokio::time::timeout(std::time::Duration::from_secs(2), storage.failed.notified())
            .await
            .expect("B was claimed and its ordering release failed");
        retry.abort();
        assert!(retry
            .await
            .expect_err("original retry future is gone")
            .is_cancelled());
        assert!(rx.try_recv().is_err());
        let stranded = storage
            .list(&recipient)
            .await
            .expect("stranded pending rows");
        let stranded = stranded
            .iter()
            .find(|row| row.id == b)
            .expect("B survives task loss");
        assert_eq!(stranded.flushed_in_session.as_ref(), Some(&session));
        assert!(stranded.outbound_sequence.is_none());
        assert!(
            !entry.claim_offline_flush(),
            "no additional presence trigger is available"
        );
        assert_eq!(
            storage
                .delete_acked_in_window(
                    &phone,
                    0,
                    1,
                    waddle_xmpp::pending_delivery::storage::PendingAckAuthority::NonResumable
                )
                .await
                .expect("predecessor ACK"),
            1
        );
        storage
            .fail_releases
            .store(false, std::sync::atomic::Ordering::SeqCst);
        let db = storage.inner().database();
        db.guard()
            .await
            .expect("pending inspection")
            .execute(
                "UPDATE pending_delivery SET claimed_at_ms = 0 WHERE row_id = ?",
                crate::db_params![b.as_str()],
            )
            .await
            .expect("age the interrupted claim without sleeping");
        let unoffered = storage
            .list_unoffered_claims(&recipient, None, i64::MAX, 8)
            .await
            .expect("durable recovery candidates");
        assert_eq!(unoffered.len(), 1);
        assert_eq!(unoffered[0].row_id, b);
        // A higher-priority remote mirror must not steal local recovery
        // work from the node hosting the surviving stream.
        let remote: FullJid = "alice@example.com/remote-mirror"
            .parse()
            .expect("remote resource");
        let (remote_tx, mut remote_rx) = mpsc::channel(8);
        state
            .deps
            .protocol
            .connection_registry
            .register_entry(remote.clone(), ConnectionEntry::remote_hosted(remote_tx));
        state
            .deps
            .protocol
            .connection_registry
            .update_presence(&remote, true, 10);
        let mut recovery = crate::pending_delivery::PendingRecovery::default();
        crate::server::session_janitors::run_pending_delivery_claim_sweep(&state, 1, &mut recovery)
            .await;
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            recovery.finish_current_pass(),
        )
        .await
        .expect("periodic recipient recovery completes");
        assert!(
            remote_rx.try_recv().is_err(),
            "periodic recovery is local to the hosting node"
        );
        let offered = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("same live stream receives B without another presence")
            .expect("B delivery");
        assert_eq!(offered.pending_row_id.as_ref(), Some(&b));
        let Stanza::Message(message) = offered.stanza else {
            panic!("B is a message")
        };
        assert_eq!(
            message.id.as_ref().map(|id| id.0.as_str()),
            Some("lost-retry-b")
        );

        // Deliberately do not stamp the writer's sequence yet. A second
        // periodic pass must distinguish already offered custody from the
        // never-offered claim recovered above, even when both are old/unsequenced.
        db.guard()
            .await
            .expect("pending inspection")
            .execute(
                "UPDATE pending_delivery SET claimed_at_ms = 0 WHERE row_id = ?",
                crate::db_params![b.as_str()],
            )
            .await
            .expect("age the offered claim too");
        crate::server::session_janitors::run_pending_delivery_claim_sweep(&state, 1, &mut recovery)
            .await;
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            recovery.finish_current_pass(),
        )
        .await
        .expect("second periodic pass completes");
        assert!(
            rx.try_recv().is_err(),
            "an offered but unsequenced stanza must not be duplicated"
        );
        let retained = storage
            .list(&recipient)
            .await
            .expect("offered row retains custody");
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].id, b);
        assert_eq!(retained[0].flushed_in_session.as_ref(), Some(&session));
        assert!(retained[0].outbound_sequence.is_none());
        assert!(storage
            .list_unoffered_claims(&recipient, None, i64::MAX, 8)
            .await
            .expect("remaining unoffered candidates")
            .is_empty());
    }

    async fn terminal_redrive_with_remote_mirror(local_available: bool) {
        use super::super::super::cleanup::{
            redrive_terminal_pending_rows_to_live_resource, TerminalRedriveOutcome,
        };
        let state = create_test_websocket_state().await;
        let recipient: BareJid = "alice@example.com".parse().expect("recipient");
        let seed = SmSessionId::new("terminal-locality-seed");
        seed_claimed_pending_row(state.as_ref(), &recipient, seed.as_str(), 1).await;
        state
            .deps
            .protocol
            .pending_delivery_storage
            .release_claim(&seed)
            .await
            .expect("release pending prefix");
        let remote: FullJid = "alice@example.com/remote".parse().expect("remote resource");
        let (remote_tx, mut remote_rx) = mpsc::channel(8);
        state
            .deps
            .protocol
            .connection_registry
            .register_entry(remote.clone(), ConnectionEntry::remote_hosted(remote_tx));
        state
            .deps
            .protocol
            .connection_registry
            .update_presence(&remote, true, 10);
        let local: FullJid = "alice@example.com/local".parse().expect("local resource");
        let (local_tx, mut local_rx) = mpsc::channel(8);
        let local_stream = SmSessionId::new("terminal-locality-local-stream");
        if local_available {
            let owner = state
                .deps
                .protocol
                .connection_registry
                .register(local.clone(), local_tx);
            state
                .deps
                .protocol
                .connection_registry
                .update_presence(&local, true, 0);
            assert!(state
                .deps
                .protocol
                .connection_registry
                .set_sm_stream_id_if_owner(&local, &owner, Some(local_stream.clone())));
        }
        let outcome =
            redrive_terminal_pending_rows_to_live_resource(state.as_ref(), &recipient).await;
        assert!(outcome == TerminalRedriveOutcome::OrderingDeferred);
        assert!(
            outcome.blocks_promotion(),
            "pending prefix blocks later terminal traffic"
        );
        assert!(
            remote_rx.try_recv().is_err(),
            "this node must never offer pending custody to a remote mirror"
        );
        let rows = state
            .deps
            .protocol
            .pending_delivery_storage
            .list(&recipient)
            .await
            .expect("pending prefix retained");
        assert_eq!(rows.len(), 1);
        assert!(rows[0].outbound_sequence.is_none());
        if local_available {
            let offered = local_rx
                .try_recv()
                .expect("lower-priority local resource receives the pending prefix");
            assert_eq!(offered.pending_row_id.as_ref(), Some(&rows[0].id));
            assert_eq!(rows[0].flushed_in_session.as_ref(), Some(&local_stream));
            assert!(local_rx.try_recv().is_err());
        } else {
            assert!(
                rows[0].flushed_in_session.is_none(),
                "remote host retains responsibility for starting its local flush"
            );
            state
                .deps
                .protocol
                .pending_delivery_storage
                .delete_row(&rows[0].id)
                .await
                .expect("remote host settles the pending prefix");
            let settled =
                redrive_terminal_pending_rows_to_live_resource(state.as_ref(), &recipient).await;
            assert!(settled == TerminalRedriveOutcome::Settled);
            assert!(
                !settled.blocks_promotion(),
                "remote-only presence does not stall an unrelated tail after the prefix is gone"
            );
        }
    }

    #[tokio::test]
    async fn terminal_redrive_prefers_local_resource_over_higher_priority_remote_mirror() {
        terminal_redrive_with_remote_mirror(true).await;
    }

    #[tokio::test]
    async fn terminal_redrive_with_only_remote_mirror_preserves_prefix_and_blocks_promotion() {
        terminal_redrive_with_remote_mirror(false).await;
    }

    #[tokio::test]
    async fn cancelled_terminal_redrive_preserves_pending_custody() {
        use super::super::super::cleanup::{
            redrive_terminal_pending_rows_to_live_resource_with_cancel, TerminalRedriveOutcome,
        };
        let state = create_test_websocket_state().await;
        let resource: FullJid = "alice@example.com/cancelled-redrive"
            .parse()
            .expect("resource");
        let recipient = resource.to_bare();
        let seed = SmSessionId::new("cancelled-redrive-seed");
        seed_claimed_pending_row(state.as_ref(), &recipient, seed.as_str(), 1).await;
        state
            .deps
            .protocol
            .pending_delivery_storage
            .release_claim(&seed)
            .await
            .expect("release pending prefix");
        let (sender, mut receiver) = mpsc::channel(8);
        register_test_connection(state.as_ref(), &resource, sender).await;
        state
            .deps
            .protocol
            .connection_registry
            .update_presence(&resource, true, 0);
        let cancel = tokio_util::sync::CancellationToken::new();
        cancel.cancel();
        let outcome = redrive_terminal_pending_rows_to_live_resource_with_cancel(
            state.as_ref(),
            &recipient,
            cancel,
        )
        .await;
        assert!(outcome == TerminalRedriveOutcome::Aborted);
        assert!(receiver.try_recv().is_err());
        assert_eq!(
            state
                .deps
                .protocol
                .pending_delivery_storage
                .list(&recipient)
                .await
                .expect("pending custody")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn repeated_terminal_ordering_deferrals_share_one_retry_pump() {
        use super::super::super::cleanup::{
            redrive_terminal_pending_rows_to_live_resource, TerminalRedriveOutcome,
        };
        let storage = Arc::new(HeldOrderingReleaseStorage::new(false));
        let state = create_test_websocket_state_with_sm_registry_and_pending_storage(
            Arc::new(InMemorySmSessionRegistry::new()),
            storage.clone(),
        )
        .await;
        assert!(storage
            .inner
            .set(
                DatabasePendingDeliveryStorage::from_database(
                    state.deps.app_state.db_pool.global().clone(),
                    QuotaPolicy::Unlimited,
                )
                .await
                .expect("shared pending database")
            )
            .is_ok());
        let resource: FullJid = "alice@example.com/single-terminal-pump"
            .parse()
            .expect("resource");
        let recipient = resource.to_bare();
        let phone = SmSessionId::new("single-pump-predecessor");
        seed_archive_row(state.as_ref(), &recipient, "single-pump-a", &phone).await;
        let b = seed_archive_row(
            state.as_ref(),
            &recipient,
            "single-pump-b",
            &SmSessionId::new("seed-b"),
        )
        .await;
        storage.release_row(&b).await.expect("B waits behind A");
        let (tx, mut rx) = mpsc::channel(8);
        let owner = register_test_connection(state.as_ref(), &resource, tx).await;
        state
            .deps
            .protocol
            .connection_registry
            .update_presence(&resource, true, 0);
        let session = SmSessionId::new("single-terminal-pump-stream");
        assert!(state
            .deps
            .protocol
            .connection_registry
            .set_sm_stream_id_if_owner(&resource, &owner, Some(session)));
        let entry = state
            .deps
            .protocol
            .connection_registry
            .entry_if_owner(&resource, &owner)
            .expect("live entry");
        assert!(entry.claim_offline_flush());
        // The first bounded pass claims B, observes A, and SUCCESSFULLY
        // releases B. Pause only the background pump's following claim,
        // leaving B unclaimed so a missing lease would admit more pumps.
        let paused_call = storage
            .claim_calls
            .load(std::sync::atomic::Ordering::SeqCst)
            + 2;
        storage
            .pause_claim_at
            .store(paused_call, std::sync::atomic::Ordering::SeqCst);
        assert!(
            redrive_terminal_pending_rows_to_live_resource(state.as_ref(), &recipient).await
                == TerminalRedriveOutcome::OrderingDeferred
        );
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            storage.claim_paused.notified(),
        )
        .await
        .expect("single background retry reached the claim gate");
        let pending = storage
            .list(&recipient)
            .await
            .expect("successfully released B");
        assert!(pending
            .iter()
            .find(|row| row.id == b)
            .expect("B")
            .flushed_in_session
            .is_none());
        for _ in 0..8 {
            assert!(
                redrive_terminal_pending_rows_to_live_resource(state.as_ref(), &recipient).await
                    == TerminalRedriveOutcome::OrderingDeferred
            );
        }
        assert_eq!(
            storage
                .claim_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            paused_call,
            "repeated terminal passes must not claim rows or create additional retry pumps"
        );
        assert!(rx.try_recv().is_err());
        assert_eq!(
            storage
                .delete_acked_in_window(
                    &phone,
                    0,
                    1,
                    waddle_xmpp::pending_delivery::storage::PendingAckAuthority::NonResumable
                )
                .await
                .expect("A acknowledged"),
            1
        );
        storage.resume_claim.add_permits(1);
        let offered = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("existing pump advances after A acknowledgement")
            .expect("B delivery");
        assert_eq!(offered.pending_row_id.as_ref(), Some(&b));
        storage
            .record_pushed_at(&b, 1)
            .await
            .expect("writer stamps B");
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Some(lease) = entry.try_acquire_terminal_ordering_retry() {
                    drop(lease);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("completed retry releases its per-entry lease");
        assert!(
            rx.try_recv().is_err(),
            "only one B offer reaches the writer"
        );
    }

    #[test]
    fn terminal_ordering_wait_blocks_shutdown_and_replay_promotion() {
        use super::super::super::cleanup::TerminalRedriveOutcome;
        assert!(TerminalRedriveOutcome::OrderingDeferred.blocks_promotion());
        assert!(TerminalRedriveOutcome::Aborted.blocks_promotion());
        assert!(!TerminalRedriveOutcome::Settled.blocks_promotion());
        assert!(!TerminalRedriveOutcome::NoLiveTarget.blocks_promotion());
    }
}

async fn assert_terminal_inventory_refresh_preserves_accepted_suffix(fail_first_append: bool) {
    use waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage;
    use waddle_xmpp::stream_management::persistence::SmPersistenceStorage;
    let persistence = Arc::new(FailDeleteSmPersistence::new());
    persistence
        .fail_delete
        .store(false, std::sync::atomic::Ordering::SeqCst);
    persistence
        .reject_next_promotion_append
        .store(fail_first_append, std::sync::atomic::Ordering::SeqCst);
    let registry = Arc::new(InMemorySmSessionRegistry::new().with_persistence(persistence.clone()));
    let directory = tempfile::tempdir().expect("pending custody database directory");
    let database_url = format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("terminal-pending.db").display()
    );
    let pending = Arc::new(
        crate::pending_delivery::DatabasePendingDeliveryStorage::open(
            Some(&database_url),
            waddle_xmpp::pending_delivery::QuotaPolicy::Unlimited,
        )
        .await
        .expect("durable pending custody"),
    );
    let state =
        create_test_websocket_state_with_sm_registry_and_pending_storage(registry.clone(), pending)
            .await;
    let jid: FullJid = "alice@example.com/one-terminal-registration"
        .parse()
        .expect("jid");
    let (tx, mut rx) = mpsc::channel(1);
    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    conn.registry_owner = Some(
        state
            .deps
            .protocol
            .connection_registry
            .register(jid.clone(), tx.clone()),
    );
    conn.sm_state
        .enable("one-terminal-registration".to_owned(), true, Some(300));
    claim_enabled_cleanup_fixture(&state, &mut conn).await;
    let stanza = |id: &str| {
        let mut message = xmpp_parsers::message::Message::new(Some(jid.to_bare().into()));
        message.from = Some("bob@example.com/phone".parse().expect("sender"));
        message.type_ = xmpp_parsers::message::MessageType::Chat;
        message.id = Some(xmpp_parsers::message::Id(id.to_owned()));
        message
            .bodies
            .insert(xmpp_parsers::message::Lang::new(), id.to_owned());
        message
    };
    let _ = conn.sm_state.record_outbound(
        waddle_xmpp::parser::stanza_to_string(stanza("recorded-prefix")).expect("prefix XML"),
        SmEvictionPath::DirectOutbound,
    );
    conn.begin_terminal_sm_recovery();
    tx.send(OutboundStanza::new(Stanza::Message(stanza(
        "accepted-suffix",
    ))))
    .await
    .expect("accepted suffix");
    let _ = cleanup_connection_shutdown(state.as_ref(), &mut rx, &mut conn, false).await;
    assert!(
        !persistence
            .reject_next_promotion_append
            .load(std::sync::atomic::Ordering::SeqCst),
        "the accepted suffix reached the one-shot append fault"
    );
    assert_eq!(
        persistence
            .inventory_store_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    // Initial admission reads its source and reconciles once; final preparation
    // refreshes the appended watermark, and confirmation verifies that snapshot.
    // Re-registering at every terminal phase exceeds this metadata-read budget.
    assert!(
        persistence
            .get_session_calls
            .load(std::sync::atomic::Ordering::SeqCst)
            <= 5
    );
    let rows = state
        .deps
        .protocol
        .pending_delivery_storage
        .list(&jid.to_bare())
        .await
        .expect("durable terminal rows");
    assert_eq!(
        rows.len(),
        2,
        "prefix and accepted suffix both transfer custody"
    );
    assert!(
        persistence
            .inner
            .get_session(&waddle_xmpp::pending_delivery::SmSessionId::new(
                "one-terminal-registration"
            ))
            .await
            .expect("confirmed source read")
            .is_none(),
        "confirmation accepts the refreshed appended watermark and retires the source"
    );

    let restarted = InMemorySmSessionRegistry::new().with_persistence(persistence.clone());
    assert_eq!(
        restarted
            .restore_from_persistence()
            .await
            .expect("restart source inventory"),
        0
    );
    let reopened = crate::pending_delivery::DatabasePendingDeliveryStorage::open(
        Some(&database_url),
        waddle_xmpp::pending_delivery::QuotaPolicy::Unlimited,
    )
    .await
    .expect("reopen durable pending custody");
    let durable_rows = reopened
        .list(&jid.to_bare())
        .await
        .expect("restart pending rows");
    let ids: std::collections::BTreeSet<_> = durable_rows
        .iter()
        .filter_map(|row| match &row.payload {
            waddle_xmpp::pending_delivery::PendingPayload::Transient(message) => {
                message.id.as_ref().map(|id| id.0.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(ids, std::collections::BTreeSet::from(["recorded-prefix", "accepted-suffix"]),
        "confirmed source deletion must leave every accepted frame in durable pending custody across restart");
}

#[tokio::test]
async fn terminal_inventory_registers_once_and_refreshes_after_accepted_suffix() {
    assert_terminal_inventory_refresh_preserves_accepted_suffix(false).await;
}

#[tokio::test]
async fn terminal_inventory_refresh_recovers_a_transient_accepted_suffix_append_failure() {
    assert_terminal_inventory_refresh_preserves_accepted_suffix(true).await;
}

#[tokio::test]
async fn terminal_drain_discards_retired_append_without_redelivery_or_retry() {
    use super::super::replay::{
        drain_outbound_into_terminal_recovery, PendingRowDrainPolicy, TerminalDrainContext,
    };
    use waddle_xmpp::stream_management::{DetachedSession, PromotionCaptureCustody};

    let persistence =
        Arc::new(waddle_xmpp::stream_management::persistence::InMemorySmPersistence::new());
    let registry = Arc::new(InMemorySmSessionRegistry::new().with_persistence(persistence));
    let state = create_test_websocket_state_with_sm_registry(registry.clone()).await;
    let jid: FullJid = "alice@example.com/retired-terminal-append"
        .parse()
        .expect("jid");
    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    conn.sm_state
        .enable("retired-terminal-append".to_owned(), true, Some(300));
    claim_enabled_cleanup_fixture(&state, &mut conn).await;
    let capture = DetachedSession {
        promotion_custody: PromotionCaptureCustody::claimed(
            conn.sm_ingress_fence.clone().expect("claim"),
        ),
        stream_id: "retired-terminal-append".to_owned(),
        user_id: jid.to_bare().to_string(),
        jid: jid.clone(),
        occupancy_session: conn.occupancy_session,
        inbound_count: 0,
        outbound_count: 3,
        last_acked: 0,
        replay_gap_through: None,
        unacked_stanzas: Vec::new(),
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
    };
    let mut stale = registry
        .register_promotion_session(&capture)
        .await
        .expect("registered inventory");
    // Another operation has already retired sequence 2 and advanced to 3;
    // this surviving producer copy still tries to allocate that old sequence.
    stale.outbound_count = 1;
    let mut message = xmpp_parsers::message::Message::new(Some(jid.to_bare().into()));
    message.from = Some("bob@example.com/phone".parse().expect("sender"));
    message.type_ = xmpp_parsers::message::MessageType::Chat;
    message.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "already settled".to_owned(),
    );
    let (tx, mut rx) = mpsc::channel(1);
    tx.send(OutboundStanza::new(Stanza::Message(message)))
        .await
        .expect("accepted frame");
    let blocklist = waddle_xmpp::protocol::session_state::Blocklist::empty();
    let outcome = drain_outbound_into_terminal_recovery(
        state.as_ref(),
        &mut conn,
        &mut rx,
        PendingRowDrainPolicy::ReleaseForTerminalRecovery,
        TerminalDrainContext {
            session: &mut stale,
            blocklist: Some(&blocklist),
            recent_tombstones: &[],
            promote_incrementally: true,
            inventory_ready: true,
        },
    )
    .await;
    assert!(!outcome.queued_pending_rows);
    assert!(
        stale.unacked_stanzas.is_empty(),
        "retired allocation cannot remain in retry custody"
    );
    assert!(
        state
            .deps
            .protocol
            .pending_delivery_storage
            .list(&jid.to_bare())
            .await
            .expect("pending rows")
            .is_empty(),
        "retired allocation cannot be promoted a second time"
    );
    let refreshed = registry
        .refresh_promotion_session(&stale)
        .await
        .expect("canonical source unchanged");
    assert_eq!(refreshed.outbound_count, 3);
    assert!(refreshed.unacked_stanzas.is_empty());
}
