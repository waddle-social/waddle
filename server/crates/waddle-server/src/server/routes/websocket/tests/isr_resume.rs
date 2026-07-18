//! Dedicated XEP-0397 (Instant Stream Resumption) test suite (ADR-0017
//! Phase 3 Slice 8, per this repo's XEP custom-test-suite hard rule).
//!
//! Complements:
//! - `waddle-xmpp`'s `isr::store::tests` / `isr::wire::tests` (unit tests
//!   for the trait contract and wire shapes, no Postgres required).
//! - `waddle-server`'s `clustering::isr::tests` (Postgres-gated: the fenced
//!   consume transaction itself — constant-time compare, single-use
//!   atomicity under real concurrency, fencing failure).
//! - `tests/stream_features.rs` (advertisement gating).
//! - `tests/xep0054_0049_0191_ws.rs::websocket_legacy_isr_token_request_iq_is_gone`
//!   (the retired IQ path).
//!
//! This file drives the full WebSocket-layer flow: `<enable>` +
//! `<isr-enable/>` issuance, then a SASL2 `<authenticate/>` + inline
//! `<inst-resume/>` instant-resume attempt, against a real Postgres
//! (Postgres-gated via `WADDLE_TEST_POSTGRES_URL`, silently no-op without
//! it, matching every other Postgres-gated test in this crate).

use super::super::isr_resume::handle_isr_resume_authenticate;
use super::super::stream_management::SmCtx;
use super::*;
use crate::clustering::claims::{clustering_control_plane_table_lock, PostgresClaimStore};
use crate::clustering::isr::PostgresIsrTokenStore;
use crate::clustering::ClusteringHandles;
use crate::db::{Database, DatabaseConfig, DatabaseDriver};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use waddle_xmpp::isr::{InMemoryIsrTokenStore, IsrTokenStore, ISR_NS, ISR_PINNED_MECHANISM};
use waddle_xmpp::ownership::{ClaimStore, InProcessClaimStore, NodeIdentity, SharedNodeIdentity};
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::{
    persistence::SmUnackedStanzaPurpose, DetachedSession, SmResume, SmSessionRegistry,
};

struct ControlledPersistIsrTokenStore {
    inner: waddle_xmpp::isr::InMemoryIsrTokenStore,
    hang_once: std::sync::atomic::AtomicBool,
    error_once: std::sync::atomic::AtomicBool,
    revoked: Option<Arc<tokio::sync::Notify>>,
}

fn register_isr_publish_owner(
    state: &super::super::state::WebSocketState,
    conn: &mut WsConnState,
    jid: &FullJid,
) {
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    conn.registry_owner = Some(
        state
            .deps
            .protocol
            .connection_registry
            .register(jid.clone(), tx),
    );
}

#[async_trait::async_trait]
impl IsrTokenStore for ControlledPersistIsrTokenStore {
    async fn ensure_schema(&self) -> Result<(), waddle_xmpp::isr::IsrTokenStoreError> {
        self.inner.ensure_schema().await
    }

    async fn persist_issued(
        &self,
        sm_id: &waddle_xmpp::pending_delivery::SmSessionId,
        issued: &waddle_xmpp::isr::IssuedIsrToken,
    ) -> Result<(), waddle_xmpp::isr::IsrTokenStoreError> {
        if self
            .error_once
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(waddle_xmpp::isr::IsrTokenStoreError::Backend(
                "injected persistence failure".to_string(),
            ));
        }
        self.inner.persist_issued(sm_id, issued).await?;
        if self
            .hang_once
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return std::future::pending().await;
        }
        Ok(())
    }

    async fn revoke_if_current(
        &self,
        sm_id: &waddle_xmpp::pending_delivery::SmSessionId,
        issued: &waddle_xmpp::isr::IssuedIsrToken,
    ) -> Result<waddle_xmpp::isr::IsrRevokeOutcome, waddle_xmpp::isr::IsrTokenStoreError> {
        let revoked = self.inner.revoke_if_current(sm_id, issued).await?;
        if let Some(notify) = &self.revoked {
            notify.notify_one();
        }
        Ok(revoked)
    }

    async fn consume(
        &self,
        sm_id: &waddle_xmpp::pending_delivery::SmSessionId,
        presented_token: &[u8],
        mechanism: &str,
        fence: &waddle_xmpp::stream_management::persistence::SmClaimFence,
    ) -> Result<waddle_xmpp::isr::IsrConsumeOutcome, waddle_xmpp::isr::IsrTokenStoreError> {
        self.inner
            .consume(sm_id, presented_token, mechanism, fence)
            .await
    }

    async fn sweep_expired(
        &self,
        max_age: std::time::Duration,
    ) -> Result<u64, waddle_xmpp::isr::IsrTokenStoreError> {
        self.inner.sweep_expired(max_age).await
    }
}

async fn test_db() -> Option<Database> {
    let url = std::env::var("WADDLE_TEST_POSTGRES_URL").ok()?;
    let db = Database::from_config(
        "isr-resume-ws-test",
        &DatabaseConfig::new(DatabaseDriver::Postgres, url)
            .with_control_plane_pool(crate::db::DEFAULT_CONTROL_PLANE_POOL_SIZE),
    )
    .await
    .expect("open test postgres");
    Some(db)
}

/// Everything needed to drive the ISR flow against a real, clustering
/// -enabled `WebSocketState`.
struct IsrFixture {
    state: Arc<WebSocketState>,
    isr_token_store: std::sync::Arc<dyn IsrTokenStore>,
}

async fn isr_fixture() -> Option<IsrFixture> {
    let db = test_db().await?;
    let claim_store: std::sync::Arc<dyn ClaimStore> =
        std::sync::Arc::new(PostgresClaimStore::new(db.clone()));
    claim_store
        .ensure_schema()
        .await
        .expect("ensure claims schema");
    let isr_store = PostgresIsrTokenStore::new(db.clone());
    isr_store.ensure_schema().await.expect("ensure isr schema");
    let isr_token_store: std::sync::Arc<dyn IsrTokenStore> = std::sync::Arc::new(isr_store);

    let node_identity = SharedNodeIdentity::new(NodeIdentity::new(
        uuid::Uuid::new_v4().to_string(),
        uuid::Uuid::new_v4().to_string(),
    ));

    let sm_session_registry = Arc::new(
        InMemorySmSessionRegistry::new()
            .with_claim_store(claim_store.clone(), node_identity.clone()),
    );

    let clustering = ClusteringHandles {
        claim_store: Some(claim_store),
        node_identity: Some(node_identity),
        local_claims: None,
        room_local_claims: None,
        user_local_claims: None,
        muc_durable_store: None,
        isr_token_store: Some(isr_token_store.clone()),
        node_lease: None,
        lease_ttl: None,
        pod_template_hash: None,
        resume_bridge: None,
        ordered_relay_delivery_bridge: None,
        stop_token: None,
        fatal_fence: None,
        resume_handshake_timeout: None,
    };

    let state = create_test_websocket_state_with_clustering(clustering, sm_session_registry).await;
    Some(IsrFixture {
        state,
        isr_token_store,
    })
}

/// An always-on ISR fixture for security-boundary tests that do not need to
/// exercise Postgres fencing itself. The in-process claim/token stores obey
/// the same claim and single-use token contracts, so CI cannot silently skip
/// these tests when `WADDLE_TEST_POSTGRES_URL` is absent.
async fn in_memory_isr_fixture() -> IsrFixture {
    let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
    let isr_token_store: Arc<dyn IsrTokenStore> = Arc::new(InMemoryIsrTokenStore::new());
    let node_identity = SharedNodeIdentity::new(NodeIdentity::new(
        uuid::Uuid::new_v4().to_string(),
        uuid::Uuid::new_v4().to_string(),
    ));
    let sm_session_registry = Arc::new(
        InMemorySmSessionRegistry::new()
            .with_claim_store(Arc::clone(&claim_store), node_identity.clone()),
    );
    let clustering = ClusteringHandles {
        claim_store: Some(claim_store),
        node_identity: Some(node_identity),
        local_claims: None,
        room_local_claims: None,
        user_local_claims: None,
        muc_durable_store: None,
        isr_token_store: Some(Arc::clone(&isr_token_store)),
        node_lease: None,
        lease_ttl: None,
        pod_template_hash: None,
        resume_bridge: None,
        ordered_relay_delivery_bridge: None,
        stop_token: None,
        fatal_fence: None,
        resume_handshake_timeout: None,
    };
    let state = create_test_websocket_state_with_clustering(clustering, sm_session_registry).await;
    IsrFixture {
        state,
        isr_token_store,
    }
}

fn isr_authenticate_frame(bare_jid: &str, token: &str, previd: &str, h: u32) -> String {
    let initial_response = BASE64_STANDARD.encode(format!("\0{bare_jid}\0{token}"));
    format!(
        "<authenticate xmlns='urn:xmpp:sasl:2' mechanism='PLAIN'>\
            <initial-response>{initial_response}</initial-response>\
            <inst-resume xmlns='{ISR_NS}' with-isr-token='true'>\
                <resume xmlns='urn:xmpp:sm:3' h='{h}' previd='{previd}'/>\
            </inst-resume>\
         </authenticate>"
    )
}

fn seeded_detached_session(stream_id: &str, jid: &FullJid) -> DetachedSession {
    DetachedSession {
        stream_id: stream_id.to_string(),
        user_id: jid.to_bare().to_string(),
        jid: jid.clone(),
        inbound_count: 3,
        outbound_count: 5,
        last_acked: 4,
        replay_gap_through: None,
        unacked_stanzas: vec![waddle_xmpp::stream_management::DetachedUnackedStanza {
            sequence: 5,
            stanza_xml: "<message id='m5'/>".to_string(),
            original_receipt_at: chrono::Utc::now(),
            purpose: SmUnackedStanzaPurpose::Application,
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
    }
}

/// Same shape as [`seeded_detached_session`], but with a properly
/// namespaced unacked stanza — required by real persistence
/// (`PostgresFencedSmPersistence` parses each unacked stanza's XML on
/// store, and rejects a namespace-less fragment like
/// `seeded_detached_session`'s own `<message id='m5'/>`, which is only
/// ever exercised against the plain in-memory `isr_fixture()` registry,
/// never real persistence).
fn seeded_detached_session_for_persistence(stream_id: &str, jid: &FullJid) -> DetachedSession {
    DetachedSession {
        unacked_stanzas: vec![waddle_xmpp::stream_management::DetachedUnackedStanza {
            sequence: 5,
            stanza_xml: "<message xmlns='jabber:client' id='m5'/>".to_string(),
            original_receipt_at: chrono::Utc::now(),
            purpose: SmUnackedStanzaPurpose::Application,
        }],
        ..seeded_detached_session(stream_id, jid)
    }
}

#[tokio::test]
async fn isr_identity_rotation_forgets_local_session_and_releases_old_exact_claim() {
    let identity_handle = SharedNodeIdentity::new(NodeIdentity::new("sm-node", "old-incarnation"));
    let claim_store = Arc::new(waddle_xmpp::ownership::InProcessClaimStore::new());
    let registry = Arc::new(
        InMemorySmSessionRegistry::new()
            .with_claim_store(claim_store.clone(), identity_handle.clone()),
    );
    let stream_id = "isr-rotated-owner";
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    registry
        .store_session(seeded_detached_session(stream_id, &jid))
        .await
        .expect("store detached session");
    assert!(
        registry
            .claim_session(stream_id)
            .await
            .expect("claim detached ISR session")
            .is_some(),
        "identity rotation occurs after ISR has moved the session into claimed state"
    );
    let entity = waddle_xmpp::ownership::Entity::new(
        waddle_xmpp::ownership::EntityType::SmSession,
        stream_id,
    );
    identity_handle
        .rotate(NodeIdentity::new("sm-node", "new-incarnation"))
        .await;
    registry
        .reconcile_claim_after_epoch_lookup_failure(stream_id)
        .await
        .expect("retire rotated claim");

    assert!(
        registry
            .live_session_ids()
            .expect("live session inventory")
            .is_empty(),
        "the old-incarnation session must not remain locally resumable"
    );
    assert!(
        claim_store
            .current_claim(&entity)
            .await
            .expect("claim lookup after cleanup")
            .is_none(),
        "the captured old-incarnation claim must be released exactly"
    );
}

// ---- Issuance: <isr-enable/> on <enable/> -----------------------------

#[tokio::test]
async fn isr_enable_mints_a_token_when_clustering_and_postgres_are_available() {
    let _guard = clustering_control_plane_table_lock().lock().await;
    let Some(fixture) = isr_fixture().await else {
        return;
    };

    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    register_isr_publish_owner(fixture.state.as_ref(), &mut conn, &jid);

    let responses = handle_xmpp_frame(
        &format!(
            "<enable xmlns='urn:xmpp:sm:3' resume='true'>\
                <isr-enable xmlns='{ISR_NS}' mechanism='PLAIN'/>\
             </enable>"
        ),
        "example.com",
        fixture.state.as_ref(),
        &mut conn,
    )
    .await;
    conn.publish_pending_sm_enable(fixture.state.as_ref());

    assert_eq!(responses.len(), 1);
    let enabled = Element::from_str(&responses[0]).expect("enabled xml");
    assert_eq!(enabled.name(), "enabled");
    let isr_enabled = enabled
        .get_child("isr-enabled", ISR_NS)
        .expect("isr-enabled child must be present when ISR is available");
    assert!(isr_enabled
        .attr("token")
        .filter(|t| !t.is_empty())
        .is_some());
}

#[tokio::test]
async fn timed_out_isr_persist_revokes_ambiguous_commit_and_enable_is_retry_safe() {
    let claim_store: std::sync::Arc<dyn ClaimStore> =
        std::sync::Arc::new(waddle_xmpp::ownership::InProcessClaimStore::new());
    let node_identity = SharedNodeIdentity::new(NodeIdentity::new("sm-node", "incarnation"));
    let sm_session_registry = Arc::new(
        InMemorySmSessionRegistry::new()
            .with_claim_store(claim_store.clone(), node_identity.clone()),
    );
    let revoked = Arc::new(tokio::sync::Notify::new());
    let isr_token_store: Arc<dyn IsrTokenStore> = Arc::new(ControlledPersistIsrTokenStore {
        inner: waddle_xmpp::isr::InMemoryIsrTokenStore::new(),
        hang_once: std::sync::atomic::AtomicBool::new(true),
        error_once: std::sync::atomic::AtomicBool::new(false),
        revoked: Some(revoked.clone()),
    });
    let state = create_test_websocket_state_with_clustering(
        ClusteringHandles {
            claim_store: Some(claim_store),
            node_identity: Some(node_identity),
            isr_token_store: Some(isr_token_store),
            ..ClusteringHandles::default()
        },
        sm_session_registry,
    )
    .await;
    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    register_isr_publish_owner(state.as_ref(), &mut conn, &jid);
    let enable = format!(
        "<enable xmlns='urn:xmpp:sm:3' resume='true'>\
            <isr-enable xmlns='{ISR_NS}' mechanism='PLAIN'/>\
         </enable>"
    );

    let revoke_completed = revoked.notified();
    let failed = handle_xmpp_frame(&enable, "example.com", state.as_ref(), &mut conn).await;
    let failed = Element::from_str(&failed[0]).expect("failed xml");
    assert_eq!(failed.name(), "failed");
    assert!(failed
        .get_child("resource-constraint", "urn:ietf:params:xml:ns:xmpp-stanzas")
        .is_some());
    assert!(!conn.sm_state.enabled);
    tokio::time::timeout(std::time::Duration::from_secs(1), revoke_completed)
        .await
        .expect("exact cleanup of ambiguous committed issuance");

    let retried = handle_xmpp_frame(&enable, "example.com", state.as_ref(), &mut conn).await;
    let enabled = Element::from_str(&retried[0]).expect("enabled xml");
    assert_eq!(enabled.name(), "enabled");
    conn.publish_pending_sm_enable(state.as_ref());
    assert!(conn.sm_state.enabled);
    assert!(enabled.get_child("isr-enabled", ISR_NS).is_some());
}

#[tokio::test]
async fn isr_persist_error_returns_sm_failure_instead_of_tokenless_success() {
    let claim_store: std::sync::Arc<dyn ClaimStore> =
        std::sync::Arc::new(waddle_xmpp::ownership::InProcessClaimStore::new());
    let node_identity = SharedNodeIdentity::new(NodeIdentity::new("sm-node", "incarnation"));
    let sm_session_registry = Arc::new(
        InMemorySmSessionRegistry::new()
            .with_claim_store(claim_store.clone(), node_identity.clone()),
    );
    let isr_token_store: Arc<dyn IsrTokenStore> = Arc::new(ControlledPersistIsrTokenStore {
        inner: waddle_xmpp::isr::InMemoryIsrTokenStore::new(),
        hang_once: std::sync::atomic::AtomicBool::new(false),
        error_once: std::sync::atomic::AtomicBool::new(true),
        revoked: None,
    });
    let state = create_test_websocket_state_with_clustering(
        ClusteringHandles {
            claim_store: Some(claim_store),
            node_identity: Some(node_identity),
            isr_token_store: Some(isr_token_store),
            ..ClusteringHandles::default()
        },
        sm_session_registry,
    )
    .await;
    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/persist-error".parse().expect("jid");
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    register_isr_publish_owner(state.as_ref(), &mut conn, &jid);

    let responses = handle_xmpp_frame(
        &format!(
            "<enable xmlns='urn:xmpp:sm:3' resume='true'>\
                <isr-enable xmlns='{ISR_NS}' mechanism='PLAIN'/>\
             </enable>"
        ),
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
    assert!(conn.pending_sm_enable_commit.is_none());
}

#[tokio::test]
async fn dropped_isr_enable_commit_revokes_the_committed_provisional_token() {
    let claim_store: std::sync::Arc<dyn ClaimStore> =
        std::sync::Arc::new(waddle_xmpp::ownership::InProcessClaimStore::new());
    let node_identity = SharedNodeIdentity::new(NodeIdentity::new("sm-node", "incarnation"));
    let sm_session_registry = Arc::new(
        InMemorySmSessionRegistry::new()
            .with_claim_store(claim_store.clone(), node_identity.clone()),
    );
    let revoked = Arc::new(tokio::sync::Notify::new());
    let isr_token_store = Arc::new(ControlledPersistIsrTokenStore {
        inner: waddle_xmpp::isr::InMemoryIsrTokenStore::new(),
        hang_once: std::sync::atomic::AtomicBool::new(false),
        error_once: std::sync::atomic::AtomicBool::new(false),
        revoked: Some(revoked.clone()),
    });
    let state = create_test_websocket_state_with_clustering(
        ClusteringHandles {
            claim_store: Some(claim_store),
            node_identity: Some(node_identity.clone()),
            isr_token_store: Some(isr_token_store.clone()),
            ..ClusteringHandles::default()
        },
        sm_session_registry,
    )
    .await;
    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    register_isr_publish_owner(state.as_ref(), &mut conn, &jid);

    let responses = handle_xmpp_frame(
        &format!(
            "<enable xmlns='urn:xmpp:sm:3' resume='true'>\
                <isr-enable xmlns='{ISR_NS}' mechanism='PLAIN'/>\
             </enable>"
        ),
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;
    let enabled = Element::from_str(&responses[0]).expect("enabled xml");
    let stream_id = enabled.attr("id").expect("stream id").to_string();
    let token = enabled
        .get_child("isr-enabled", ISR_NS)
        .and_then(|element| element.attr("token"))
        .expect("issued ISR token")
        .to_string();
    assert!(!conn.sm_state.enabled);
    assert!(conn.pending_sm_enable_commit.is_some());

    let revoke_completed = revoked.notified();
    drop(conn.pending_sm_enable_commit.take());
    tokio::time::timeout(std::time::Duration::from_secs(1), revoke_completed)
        .await
        .expect("bounded provisional revoke");

    let fence = waddle_xmpp::stream_management::persistence::SmClaimFence::new(
        node_identity.current(),
        waddle_xmpp::ownership::ClaimEpoch(0),
    );
    assert_eq!(
        isr_token_store
            .consume(
                &SmSessionId::new(stream_id.clone()),
                token.as_bytes(),
                ISR_PINNED_MECHANISM,
                &fence,
            )
            .await
            .expect("consume after dropped publication"),
        waddle_xmpp::isr::IsrConsumeOutcome::NoSuchToken
    );
}

#[tokio::test]
async fn written_isr_enable_that_loses_publication_revokes_on_terminal_cleanup() {
    let claim_store: std::sync::Arc<dyn ClaimStore> =
        std::sync::Arc::new(waddle_xmpp::ownership::InProcessClaimStore::new());
    let node_identity = SharedNodeIdentity::new(NodeIdentity::new("sm-node", "incarnation"));
    let sm_session_registry = Arc::new(
        InMemorySmSessionRegistry::new()
            .with_claim_store(claim_store.clone(), node_identity.clone()),
    );
    let revoked = Arc::new(tokio::sync::Notify::new());
    let isr_token_store = Arc::new(ControlledPersistIsrTokenStore {
        inner: waddle_xmpp::isr::InMemoryIsrTokenStore::new(),
        hang_once: std::sync::atomic::AtomicBool::new(false),
        error_once: std::sync::atomic::AtomicBool::new(false),
        revoked: Some(revoked.clone()),
    });
    let state = create_test_websocket_state_with_clustering(
        ClusteringHandles {
            claim_store: Some(claim_store),
            node_identity: Some(node_identity.clone()),
            isr_token_store: Some(isr_token_store.clone()),
            ..ClusteringHandles::default()
        },
        sm_session_registry,
    )
    .await;
    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/publication-race".parse().expect("jid");
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    register_isr_publish_owner(state.as_ref(), &mut conn, &jid);

    let responses = handle_xmpp_frame(
        &format!(
            "<enable xmlns='urn:xmpp:sm:3' resume='true'>\
                <isr-enable xmlns='{ISR_NS}' mechanism='PLAIN'/>\
             </enable>"
        ),
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;
    let enabled = Element::from_str(&responses[0]).expect("enabled xml");
    let stream_id = enabled.attr("id").expect("stream id").to_string();
    let token = enabled
        .get_child("isr-enabled", ISR_NS)
        .and_then(|element| element.attr("token"))
        .expect("issued ISR token")
        .to_string();

    let (replacement_tx, _replacement_rx) = tokio::sync::mpsc::channel(1);
    state
        .deps
        .protocol
        .connection_registry
        .register(jid, replacement_tx);
    conn.publish_pending_sm_enable(state.as_ref());
    assert!(
        conn.sm_state.enabled,
        "written XEP-0198 state remains committed"
    );
    assert!(conn.unpublished_isr_cleanup.is_some());

    let revoke_completed = revoked.notified();
    let (_outbound_tx, mut outbound_rx) = tokio::sync::mpsc::channel(1);
    assert_eq!(
        super::super::cleanup::cleanup_connection_shutdown(
            state.as_ref(),
            &mut outbound_rx,
            &mut conn,
            false,
        )
        .await,
        super::super::cleanup::ConnectionShutdownOutcome::NotPersisted
    );
    tokio::time::timeout(std::time::Duration::from_secs(1), revoke_completed)
        .await
        .expect("terminal cleanup revokes unpublished ISR credential");

    let fence = waddle_xmpp::stream_management::persistence::SmClaimFence::new(
        node_identity.current(),
        waddle_xmpp::ownership::ClaimEpoch(0),
    );
    assert_eq!(
        isr_token_store
            .consume(
                &SmSessionId::new(stream_id.clone()),
                token.as_bytes(),
                ISR_PINNED_MECHANISM,
                &fence,
            )
            .await
            .expect("consume after terminal cleanup"),
        waddle_xmpp::isr::IsrConsumeOutcome::NoSuchToken
    );
}

#[tokio::test]
async fn isr_is_not_advertised_or_issued_for_unsecured_configured_transport() {
    let claim_store: std::sync::Arc<dyn ClaimStore> =
        std::sync::Arc::new(waddle_xmpp::ownership::InProcessClaimStore::new());
    let node_identity = SharedNodeIdentity::new(NodeIdentity::new("sm-node", "incarnation"));
    let sm_session_registry = Arc::new(
        InMemorySmSessionRegistry::new()
            .with_claim_store(claim_store.clone(), node_identity.clone()),
    );
    let isr_token_store: Arc<dyn IsrTokenStore> =
        Arc::new(waddle_xmpp::isr::InMemoryIsrTokenStore::new());
    let mut state = create_test_websocket_state_with_clustering(
        ClusteringHandles {
            claim_store: Some(claim_store),
            node_identity: Some(node_identity),
            isr_token_store: Some(isr_token_store),
            ..ClusteringHandles::default()
        },
        sm_session_registry,
    )
    .await;
    Arc::get_mut(&mut state)
        .expect("fixture state uniquely owned")
        .deps
        .transport_security = super::super::state::TransportSecurity::Unsecured;
    assert!(!state.deps.isr_available());

    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/insecure".parse().expect("jid");
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    register_isr_publish_owner(state.as_ref(), &mut conn, &jid);
    let open_responses = handle_xmpp_frame(
        "<open xmlns='urn:ietf:params:xml:ns:xmpp-framing' to='example.com' version='1.0'/>",
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;
    let features = Element::from_str(&open_responses[1]).expect("stream features xml");
    assert!(!features
        .children()
        .any(|child| child.name() == "isr" && child.ns() == ISR_NS));
    let responses = handle_xmpp_frame(
        &format!(
            "<enable xmlns='urn:xmpp:sm:3' resume='true'>\
                <isr-enable xmlns='{ISR_NS}' mechanism='PLAIN'/>\
             </enable>"
        ),
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;
    let enabled = Element::from_str(&responses[0]).expect("enabled xml");
    assert!(enabled.get_child("isr-enabled", ISR_NS).is_none());
}

#[tokio::test]
async fn isr_enable_is_silently_ignored_without_clustering() {
    // No clustering/Postgres wired at all — the plain default fixture.
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    register_isr_publish_owner(state.as_ref(), &mut conn, &jid);

    let responses = handle_xmpp_frame(
        &format!(
            "<enable xmlns='urn:xmpp:sm:3'>\
                <isr-enable xmlns='{ISR_NS}' mechanism='PLAIN'/>\
             </enable>"
        ),
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;
    conn.publish_pending_sm_enable(state.as_ref());

    let enabled = Element::from_str(&responses[0]).expect("enabled xml");
    assert!(
        enabled.get_child("isr-enabled", ISR_NS).is_none(),
        "no token should be minted when ISR is unavailable"
    );
}

/// Council-adjudicated FIX 7: a non-resumable `<enable/>` (no
/// `resume='true'`) has no XEP-0198 `previd` an ISR resume could ever
/// reference — minting a token for it is permanently unconsumable dead
/// weight. Gated independently of clustering/Postgres availability (the
/// `isr_fixture` here has both wired, unlike
/// `isr_enable_is_silently_ignored_without_clustering` above).
#[tokio::test]
async fn isr_enable_is_ignored_when_not_resumable() {
    let _guard = clustering_control_plane_table_lock().lock().await;
    let Some(fixture) = isr_fixture().await else {
        return;
    };

    let mut conn = WsConnState::new();
    let jid: FullJid = "grace@example.com/web".parse().expect("jid");
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    register_isr_publish_owner(fixture.state.as_ref(), &mut conn, &jid);

    let responses = handle_xmpp_frame(
        &format!(
            "<enable xmlns='urn:xmpp:sm:3'>\
                <isr-enable xmlns='{ISR_NS}' mechanism='PLAIN'/>\
             </enable>"
        ),
        "example.com",
        fixture.state.as_ref(),
        &mut conn,
    )
    .await;
    conn.publish_pending_sm_enable(fixture.state.as_ref());

    assert_eq!(responses.len(), 1);
    let enabled = Element::from_str(&responses[0]).expect("enabled xml");
    assert_eq!(enabled.name(), "enabled");
    assert!(
        enabled.get_child("isr-enabled", ISR_NS).is_none(),
        "no token should be minted for a non-resumable <enable/>, even with ISR available"
    );
}

// ---- Resume: SASL2 <authenticate/> + inline <inst-resume/> -------------

#[tokio::test]
async fn isr_resume_succeeds_matches_token_rotates_and_replays() {
    let _guard = clustering_control_plane_table_lock().lock().await;
    let Some(fixture) = isr_fixture().await else {
        return;
    };

    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let stream_id = format!("sm-{}", uuid::Uuid::new_v4());
    fixture
        .state
        .deps
        .protocol
        .sm_session_registry
        .store_session(seeded_detached_session(&stream_id, &jid))
        .await
        .expect("seed detached session");
    let issued = fixture
        .isr_token_store
        .issue(&SmSessionId::new(stream_id.clone()), ISR_PINNED_MECHANISM)
        .await
        .expect("issue token");

    let mut conn = WsConnState::new();
    let frame = isr_authenticate_frame(&jid.to_bare().to_string(), &issued.token, &stream_id, 4);
    let responses =
        handle_xmpp_frame(&frame, "example.com", fixture.state.as_ref(), &mut conn).await;

    assert!(
        !responses.is_empty(),
        "expected at least a <success/> reply"
    );
    let success = Element::from_str(&responses[0]).expect("success xml");
    assert_eq!(success.name(), "success");
    assert_eq!(success.ns(), "urn:xmpp:sasl:2");
    assert_eq!(
        success
            .get_child("authorization-identifier", "urn:xmpp:sasl:2")
            .map(|e| e.text()),
        Some(jid.to_bare().to_string())
    );
    let inst_resumed = success
        .get_child("inst-resumed", ISR_NS)
        .expect("inst-resumed child");
    let new_token = inst_resumed
        .attr("token")
        .expect("rotated token attribute")
        .to_string();
    assert_ne!(new_token, issued.token, "token must rotate on success");
    let resumed = inst_resumed
        .get_child("resumed", "urn:xmpp:sm:3")
        .expect("resumed child");
    assert_eq!(resumed.attr("previd"), Some(stream_id.as_str()));

    // Replay: the one unacked stanza (m5) should follow.
    assert_eq!(
        responses.len(),
        2,
        "expected exactly one replayed stanza: {responses:?}"
    );
    assert!(responses[1].contains("m5"));

    // Connection state actually resumed.
    assert!(conn.phase.is_ready());
    assert!(conn.phase.is_resumed());
    assert_eq!(conn.phase.bound_jid(), Some(&jid));

    // Single-use: the OLD token no longer works for a second attempt.
    let mut conn2 = WsConnState::new();
    let replay_frame =
        isr_authenticate_frame(&jid.to_bare().to_string(), &issued.token, &stream_id, 4);
    let replay_responses = handle_xmpp_frame(
        &replay_frame,
        "example.com",
        fixture.state.as_ref(),
        &mut conn2,
    )
    .await;
    let failure = Element::from_str(&replay_responses[0]).expect("failure xml");
    assert_eq!(failure.name(), "failure");
    assert_eq!(failure.ns(), "urn:xmpp:sasl:2");
}

#[tokio::test]
async fn isr_resume_rejects_wrong_token_with_bare_failure_and_destroys_session() {
    let _guard = clustering_control_plane_table_lock().lock().await;
    let Some(fixture) = isr_fixture().await else {
        return;
    };

    let jid: FullJid = "bob@example.com/web".parse().expect("jid");
    let stream_id = format!("sm-{}", uuid::Uuid::new_v4());
    fixture
        .state
        .deps
        .protocol
        .sm_session_registry
        .store_session(seeded_detached_session(&stream_id, &jid))
        .await
        .expect("seed detached session");
    fixture
        .isr_token_store
        .issue(&SmSessionId::new(stream_id.clone()), ISR_PINNED_MECHANISM)
        .await
        .expect("issue token");

    let mut conn = WsConnState::new();
    let frame = isr_authenticate_frame(
        &jid.to_bare().to_string(),
        "totally-wrong-token",
        &stream_id,
        4,
    );
    let responses =
        handle_xmpp_frame(&frame, "example.com", fixture.state.as_ref(), &mut conn).await;

    assert_eq!(responses.len(), 1);
    let failure = Element::from_str(&responses[0]).expect("failure xml");
    assert_eq!(failure.name(), "failure");
    assert_eq!(failure.ns(), "urn:xmpp:sasl:2");
    assert!(!conn.phase.is_authenticated());

    // Session state is destroyed: an ordinary XEP-0198 resume for the same
    // previd (with authentication established out-of-band, as a normal
    // reconnect would establish it) must now fail with item-not-found —
    // the anti-brute-force MUST.
    let mut conn2 = WsConnState::new();
    conn2.phase = ConnectionPhase::authenticated(&jid);
    let resume_responses = handle_xmpp_frame(
        &format!("<resume xmlns='urn:xmpp:sm:3' h='4' previd='{stream_id}'/>"),
        "example.com",
        fixture.state.as_ref(),
        &mut conn2,
    )
    .await;
    let failed = Element::from_str(&resume_responses[0]).expect("failed xml");
    assert_eq!(failed.name(), "failed");
}

/// Council-adjudicated FIX 3: ISR-authenticating against a session that
/// never opted into ISR at all (no `<isr-enable/>` ever ran for it — no
/// `clustering_isr_tokens` row exists) must fail WITHOUT destroying the
/// session — distinct from `isr_resume_rejects_wrong_token_with_bare_failure_and_destroys_session`
/// above, where a row genuinely existed and the presented token didn't
/// match it.
#[tokio::test]
async fn isr_resume_against_never_isr_enabled_session_fails_without_destroying() {
    let _guard = clustering_control_plane_table_lock().lock().await;
    let Some(fixture) = isr_fixture().await else {
        return;
    };

    let jid: FullJid = "frank@example.com/web".parse().expect("jid");
    let stream_id = format!("sm-{}", uuid::Uuid::new_v4());
    fixture
        .state
        .deps
        .protocol
        .sm_session_registry
        .store_session(seeded_detached_session(&stream_id, &jid))
        .await
        .expect("seed detached session");
    // Deliberately no `fixture.isr_token_store.issue(...)` call — this
    // session never opted into ISR.

    let mut conn = WsConnState::new();
    let frame = isr_authenticate_frame(
        &jid.to_bare().to_string(),
        "some-arbitrary-token",
        &stream_id,
        4,
    );
    let responses =
        handle_xmpp_frame(&frame, "example.com", fixture.state.as_ref(), &mut conn).await;

    assert_eq!(responses.len(), 1);
    let failure = Element::from_str(&responses[0]).expect("failure xml");
    assert_eq!(failure.name(), "failure");
    assert_eq!(failure.ns(), "urn:xmpp:sasl:2");

    // The session was NOT destroyed: an ordinary XEP-0198 resume for the
    // same previd (identity established out-of-band, as a normal reconnect
    // would establish it) must still succeed.
    let mut conn2 = WsConnState::new();
    conn2.phase = ConnectionPhase::authenticated(&jid);
    let resume_responses = handle_xmpp_frame(
        &format!("<resume xmlns='urn:xmpp:sm:3' h='4' previd='{stream_id}'/>"),
        "example.com",
        fixture.state.as_ref(),
        &mut conn2,
    )
    .await;
    let resumed = Element::from_str(&resume_responses[0]).expect("resumed xml");
    assert_eq!(
        resumed.name(),
        "resumed",
        "session must NOT have been destroyed by a NoSuchToken outcome: {resume_responses:?}"
    );
}

#[tokio::test]
async fn isr_resume_rejects_identity_mismatch_without_destroying_session() {
    let _guard = clustering_control_plane_table_lock().lock().await;
    let Some(fixture) = isr_fixture().await else {
        return;
    };

    let jid: FullJid = "carol@example.com/web".parse().expect("jid");
    let stream_id = format!("sm-{}", uuid::Uuid::new_v4());
    fixture
        .state
        .deps
        .protocol
        .sm_session_registry
        .store_session(seeded_detached_session(&stream_id, &jid))
        .await
        .expect("seed detached session");
    let issued = fixture
        .isr_token_store
        .issue(&SmSessionId::new(stream_id.clone()), ISR_PINNED_MECHANISM)
        .await
        .expect("issue token");

    // A different bare JID presents the CORRECT token — identity binding
    // must reject this before the token compare ever runs.
    let mut conn = WsConnState::new();
    let frame = isr_authenticate_frame("mallory@example.com", &issued.token, &stream_id, 4);
    let responses =
        handle_xmpp_frame(&frame, "example.com", fixture.state.as_ref(), &mut conn).await;
    let failure = Element::from_str(&responses[0]).expect("failure xml");
    assert_eq!(failure.name(), "failure");
    assert_eq!(failure.ns(), "urn:xmpp:sasl:2");

    // The token and session state were NOT destroyed: the legitimate owner
    // can still resume with the ORIGINAL token right after.
    let mut conn2 = WsConnState::new();
    let good_frame =
        isr_authenticate_frame(&jid.to_bare().to_string(), &issued.token, &stream_id, 4);
    let good_responses = handle_xmpp_frame(
        &good_frame,
        "example.com",
        fixture.state.as_ref(),
        &mut conn2,
    )
    .await;
    let success = Element::from_str(&good_responses[0]).expect("success xml");
    assert_eq!(success.name(), "success");
    assert!(success.get_child("inst-resumed", ISR_NS).is_some());
}

#[tokio::test]
async fn isr_resume_authenticated_but_impossible_wraps_failed_in_success() {
    let _guard = clustering_control_plane_table_lock().lock().await;
    let Some(fixture) = isr_fixture().await else {
        return;
    };

    let jid: FullJid = "dave@example.com/web".parse().expect("jid");
    let stream_id = format!("sm-{}", uuid::Uuid::new_v4());
    fixture
        .state
        .deps
        .protocol
        .sm_session_registry
        .store_session(seeded_detached_session(&stream_id, &jid))
        .await
        .expect("seed detached session");
    let issued = fixture
        .isr_token_store
        .issue(&SmSessionId::new(stream_id.clone()), ISR_PINNED_MECHANISM)
        .await
        .expect("issue token");

    // `h` exceeds the server's own outbound count (5) — the token is valid
    // (authentication succeeds) but resumption itself is impossible.
    let mut conn = WsConnState::new();
    let frame = isr_authenticate_frame(&jid.to_bare().to_string(), &issued.token, &stream_id, 999);
    let responses =
        handle_xmpp_frame(&frame, "example.com", fixture.state.as_ref(), &mut conn).await;

    assert_eq!(responses.len(), 1);
    let success = Element::from_str(&responses[0]).expect("success xml");
    assert_eq!(success.name(), "success");
    assert_eq!(success.ns(), "urn:xmpp:sasl:2");
    let inst_resume_failed = success
        .get_child("inst-resume-failed", ISR_NS)
        .expect("inst-resume-failed child");
    let failed = inst_resume_failed
        .get_child("failed", "urn:xmpp:sm:3")
        .expect("failed child");
    assert!(failed.attr("h").is_some());
    // The connection was NOT resumed — it stays in its pre-attempt phase.
    assert!(!conn.phase.is_ready());
}

#[tokio::test]
async fn isr_resume_rejects_unsecured_transport_without_consuming_token() {
    let mut fixture = in_memory_isr_fixture().await;

    Arc::get_mut(&mut fixture.state)
        .expect("fixture owns its WebSocket state")
        .deps
        .transport_security = TransportSecurity::Unsecured;

    let jid: FullJid = "tls-gate@example.com/web".parse().expect("jid");
    let stream_id = format!("sm-{}", uuid::Uuid::new_v4());
    fixture
        .state
        .deps
        .protocol
        .sm_session_registry
        .store_session(seeded_detached_session(&stream_id, &jid))
        .await
        .expect("seed detached session");
    let issued = fixture
        .isr_token_store
        .issue(&SmSessionId::new(stream_id.clone()), ISR_PINNED_MECHANISM)
        .await
        .expect("issue token");

    let frame = isr_authenticate_frame(&jid.to_bare().to_string(), &issued.token, &stream_id, 4);
    let mut unsecured_conn = WsConnState::new();
    let responses = handle_xmpp_frame(
        &frame,
        "example.com",
        fixture.state.as_ref(),
        &mut unsecured_conn,
    )
    .await;
    let failure = Element::from_str(&responses[0]).expect("failure XML");
    assert_eq!(failure.name(), "failure");
    assert!(failure
        .get_child("invalid-mechanism", waddle_xmpp::ns::SASL)
        .is_some());

    // The transport gate runs before session claim or token consumption. Once
    // the trusted public endpoint is secure, the original token can still
    // resume the original detached session.
    Arc::get_mut(&mut fixture.state)
        .expect("fixture still owns its WebSocket state")
        .deps
        .transport_security = TransportSecurity::Tls;
    let mut secure_conn = WsConnState::new();
    let resumed = handle_xmpp_frame(
        &frame,
        "example.com",
        fixture.state.as_ref(),
        &mut secure_conn,
    )
    .await;
    let success = Element::from_str(&resumed[0]).expect("success XML");
    assert_eq!(success.name(), "success");
    assert!(success.get_child("inst-resumed", ISR_NS).is_some());
}

#[tokio::test]
async fn isr_resume_rejects_an_overlong_wire_session_id_before_backend_work() {
    let fixture = in_memory_isr_fixture().await;
    let previd = "s".repeat(waddle_xmpp::pending_delivery::SM_SESSION_ID_MAX_LEN + 1);
    let frame = isr_authenticate_frame("alice@example.com", "unused-token", &previd, 0);
    let mut conn = WsConnState::new();

    let responses =
        handle_xmpp_frame(&frame, "example.com", fixture.state.as_ref(), &mut conn).await;

    assert_eq!(responses.len(), 1);
    let failure = Element::from_str(&responses[0]).expect("failure XML");
    assert_eq!(failure.name(), "failure");
    assert!(failure
        .get_child("not-authorized", waddle_xmpp::ns::SASL)
        .is_some());
    assert!(!conn.phase.is_ready());
}

// Sanity check that the module-level handler is reachable directly too
// (mirrors how `stream_management.rs` tests call `handle_sm_stanza`
// directly in a couple of places) — exercised via the SmCtx-taking
// entrypoint the frame parser dispatches to.
#[tokio::test]
async fn handle_isr_resume_authenticate_is_a_noop_failure_without_clustering() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let mut pending_sm_enable_commit = None;
    let ctx = SmCtx {
        phase: &mut conn.phase,
        sm_state: &mut conn.sm_state,
        authenticated_session: &mut conn.authenticated_session,
        carbons_enabled: &mut conn.carbons_enabled,
        presence_available: &mut conn.presence_available,
        presence_show: &mut conn.presence_show,
        presence_status: &mut conn.presence_status,
        presence_priority: &mut conn.presence_priority,
        presence_payloads: &mut conn.presence_payloads,
        pending_subscribes_flushed: &mut conn.pending_subscribes_flushed,
        pending_resume_stream_id: &mut conn.pending_resume_stream_id,
        pending_resume_h: &mut conn.pending_resume_h,
        suppress_sm_record_next_batch: &mut conn.suppress_sm_record_next_batch,
        roster_interested: &mut conn.roster_interested,
        blocklist_interested: &mut conn.blocklist_interested,
        pending_sm_enable_commit: &mut pending_sm_enable_commit,
    };
    let responses = handle_isr_resume_authenticate(
        "PLAIN".to_string(),
        BASE64_STANDARD.encode("\0alice@example.com\0some-token"),
        SmResume {
            previd: "does-not-matter".to_string(),
            h: 0,
        },
        state.as_ref(),
        ctx,
    )
    .await;
    assert_eq!(responses.len(), 1);
    let failure = Element::from_str(&responses[0]).expect("failure xml");
    assert_eq!(failure.name(), "failure");
}

// ---- FIX 2 (council-adjudicated): cross-node ISR resume ---------------

/// Council-adjudicated FIX 2: an ISR resume for a session whose SM claim is
/// owned by ANOTHER node must succeed via the SAME cross-node claim-steal
/// machinery `handle_sm_resume` uses (Slice 6) — reused, not reinvented,
/// through `stream_management::attempt_cross_node_resume_raced`.
///
/// Simulates two nodes sharing one Postgres: node A's registry persists +
/// claims a detached session (mirroring `xep0198_cross_node_resume.rs`'s own
/// "detached, owned elsewhere" setup via `store_session`, which both
/// persists the snapshot and self-claims it under node A's identity). Node
/// B's `WebSocketState` — this fixture, a distinct `NodeIdentity`/registry —
/// then performs the SASL2 ISR-resume authenticate. The ISR token itself was
/// issued against the shared Postgres before node B ever sees this stream,
/// exactly as `<isr-enable/>` would mint it; `consume`'s epoch fence binds
/// to whichever node currently holds the claim, so it must succeed against
/// node B's freshly-won epoch after the steal.
#[tokio::test]
async fn isr_resume_wins_a_cross_node_steal_and_consumes_the_token() {
    let _guard = clustering_control_plane_table_lock().lock().await;
    let Some(db) = test_db().await else {
        return;
    };

    // ---- Node A: persists + self-claims a detached session ------------
    let claim_store_a: std::sync::Arc<dyn ClaimStore> =
        std::sync::Arc::new(PostgresClaimStore::new(db.clone()));
    claim_store_a
        .ensure_schema()
        .await
        .expect("ensure claims schema");
    let identity_a = SharedNodeIdentity::new(NodeIdentity::new(
        uuid::Uuid::new_v4().to_string(),
        uuid::Uuid::new_v4().to_string(),
    ));
    let persistence_a = crate::sm_persistence_fenced::PostgresFencedSmPersistence::open(
        db.clone(),
        std::sync::Arc::clone(&claim_store_a),
        identity_a.clone(),
    )
    .await
    .expect("open node A's fenced persistence");
    let registry_a = std::sync::Arc::new(
        InMemorySmSessionRegistry::new()
            .with_persistence(std::sync::Arc::new(persistence_a))
            .with_claim_store(claim_store_a, identity_a),
    );

    let jid: FullJid = "erin@example.com/phone".parse().expect("jid");
    let stream_id = format!("sm-{}", uuid::Uuid::new_v4());
    registry_a
        .store_session(seeded_detached_session_for_persistence(&stream_id, &jid))
        .await
        .expect("node A stores + claims + persists the detached session");

    // ---- The ISR token: issued against the shared Postgres, before node
    // ---- B ever sees this stream, exactly as <isr-enable/> would mint it.
    let isr_store = PostgresIsrTokenStore::new(db.clone());
    isr_store.ensure_schema().await.expect("ensure isr schema");
    let issued = isr_store
        .issue(&SmSessionId::new(stream_id.clone()), ISR_PINNED_MECHANISM)
        .await
        .expect("issue token");

    // ---- Node B: a distinct identity/registry/claim-store handle,
    // ---- sharing the SAME Postgres — this connection's <authenticate/>
    // ---- lands here, not on node A.
    let claim_store_b: std::sync::Arc<dyn ClaimStore> =
        std::sync::Arc::new(PostgresClaimStore::new(db.clone()));
    let identity_b = SharedNodeIdentity::new(NodeIdentity::new(
        uuid::Uuid::new_v4().to_string(),
        uuid::Uuid::new_v4().to_string(),
    ));
    let persistence_b = crate::sm_persistence_fenced::PostgresFencedSmPersistence::open(
        db.clone(),
        std::sync::Arc::clone(&claim_store_b),
        identity_b.clone(),
    )
    .await
    .expect("open node B's fenced persistence");
    let sm_session_registry_b = std::sync::Arc::new(
        InMemorySmSessionRegistry::new()
            .with_persistence(std::sync::Arc::new(persistence_b))
            .with_claim_store(claim_store_b.clone(), identity_b.clone()),
    );

    let clustering_b = ClusteringHandles {
        claim_store: Some(claim_store_b),
        node_identity: Some(identity_b),
        local_claims: None,
        room_local_claims: None,
        user_local_claims: None,
        muc_durable_store: None,
        isr_token_store: Some(std::sync::Arc::new(isr_store) as std::sync::Arc<dyn IsrTokenStore>),
        node_lease: None,
        lease_ttl: None,
        pod_template_hash: None,
        ordered_relay_delivery_bridge: None,
        resume_bridge: None,
        stop_token: None,
        fatal_fence: None,
        // FIX 2: a real handshake budget — node B's cross-node steal must
        // actually reach branch 1 (`current_claim` finds node A's foreign
        // claim; a persisted snapshot already exists too), never
        // short-circuit on a zero budget (a `None` budget is only safe when
        // no foreign claim exists at all, in which case the budget is
        // never even consulted — see `attempt_cross_node_resume_raced`'s
        // own doc comment).
        resume_handshake_timeout: Some(std::time::Duration::from_secs(2)),
    };
    let state_b =
        create_test_websocket_state_with_clustering(clustering_b, sm_session_registry_b).await;

    let mut conn = WsConnState::new();
    let frame = isr_authenticate_frame(&jid.to_bare().to_string(), &issued.token, &stream_id, 4);
    let responses = handle_xmpp_frame(&frame, "example.com", state_b.as_ref(), &mut conn).await;

    assert!(
        !responses.is_empty(),
        "expected at least a <success/> reply: {responses:?}"
    );
    let success = Element::from_str(&responses[0]).expect("success xml");
    assert_eq!(success.name(), "success", "responses: {responses:?}");
    assert_eq!(success.ns(), "urn:xmpp:sasl:2");
    let inst_resumed = success
        .get_child("inst-resumed", ISR_NS)
        .expect("inst-resumed child");
    let rotated = inst_resumed
        .attr("token")
        .expect("rotated token attribute")
        .to_string();
    assert_ne!(rotated, issued.token, "token must rotate on success");
    let resumed = inst_resumed
        .get_child("resumed", "urn:xmpp:sm:3")
        .expect("resumed child");
    assert_eq!(resumed.attr("previd"), Some(stream_id.as_str()));

    // The connection actually resumed, bound to node A's original session.
    assert!(conn.phase.is_ready());
    assert!(conn.phase.is_resumed());
    assert_eq!(conn.phase.bound_jid(), Some(&jid));

    // Replay: the one unacked stanza (m5, from `seeded_detached_session`)
    // must follow, proving the h-counter/unacked-queue state genuinely
    // crossed the steal, not just the bare claim.
    assert_eq!(
        responses.len(),
        2,
        "expected exactly one replayed stanza: {responses:?}"
    );
    assert!(responses[1].contains("m5"));
}
