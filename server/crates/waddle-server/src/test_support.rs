//! Minimal public fixtures for this crate's XEP integration suites.
//!
//! The helpers are test-only by construction and use `expect` internally when
//! creating fixed local fixtures; they are never enabled in release images.

#[cfg(feature = "test-support")]
pub mod iq_forward;
#[cfg(feature = "test-support")]
pub mod supervised_io;
#[cfg(feature = "test-support")]
pub mod connection_actor {
    //! Dark, test-only vertical proof for a connection actor that owns the
    //! real WebSocket connection state and its reserved physical pump pair.
    pub use crate::server::routes::websocket::connection_actor::{
        canonical_actor_gate, start_connection_actor, start_connection_actor_with_effect,
        start_connection_actor_with_hooks, AcknowledgementResult, CanonicalActorAdmission,
        CanonicalActorGate, CompleteHandledInbound, CompleteHandledInboundAtLifecycleGate,
        CompleteHandledInboundBeforeLifecycleGate, CompletePrebindAuth,
        CompleteValidatedPrebindAuth, ConfirmBindingInstall, ConnectionActor,
        ConnectionActorBindingAttempt, ConnectionActorController, ConnectionActorError,
        ConnectionActorGateBarrier, ConnectionActorOperationId, ConnectionActorSequence,
        ConnectionActorSnapshot, ConnectionActorStartBarrier, ConnectionActorStartError,
        ConnectionActorStartHooks, ConnectionWireFrame, CountableEffectDisposition,
        CountableInboundEffect, CountableStanzaEffect, InstallBinding, PanicConnectionActor,
        PauseConnectionActor, PrebindAuthAttempt, PrebindAuthOutcome, PrebindAuthToken,
        PrebindInput, PrebindPhase, PrebindSaslInput, PrebindSnapshot, PrebindValidationAttempt,
        PrepareBinding, RequestAcknowledgement, ReserveHandledInbound, RetireBinding,
        SeedInboundCounterForTest, SnapshotConnection, SnapshotPrebind, StopConnectionActor,
    };
}

use crate::db::DatabasePool;
use crate::ingress::IngressAuthority;
use crate::notification_outbox::NotificationOutboxStore;
use crate::notification_settings_projection::NotificationSettingsProjectionStore;
use crate::pending_delivery::DatabasePendingDeliveryStorage;
use std::sync::Arc;

pub use crate::server::routes::websocket::muc_invites::{
    claim_invite, list_invites, record_invite_at, InviteStorageError, OutstandingInvite,
    RecordOutcome,
};
/// The production `/ws` RFC 7395 router, so suites can serve a test state
/// over a real loopback WebSocket and drive the live connection loop.
pub use crate::server::routes::websocket::router;
pub use crate::server::routes::websocket::test_state::{
    create_test_session, register_test_connection, seed_local_account,
};
use crate::server::routes::websocket::test_state::{
    create_test_websocket_state_with_ingress_factory, empty_extension_manager, TestStateOverrides,
};
pub use crate::server::routes::websocket::WebSocketState;

/// Build the default in-memory WebSocket state the in-crate suites use
/// (SQLite in memory, single-node registries, the test ingress authority).
///
/// # Panics
/// Panics if database schema or WebSocket fixture construction fails.
pub async fn websocket_state() -> Arc<WebSocketState> {
    create_test_websocket_state_with_ingress_factory(
        empty_extension_manager().await,
        TestStateOverrides::default(),
        || std::future::ready(None),
    )
    .await
}

/// Overrides for production WebSocket admission in in-process tests.
#[cfg(feature = "test-support")]
pub struct WebSocketAdmissionTestConfig {
    pub capacity: crate::config::MaxWebSocketConnections,
    /// `None` is standalone; `Some(None)` represents missing clustered identity.
    pub clustered_identity: Option<Option<waddle_xmpp::ownership::SharedNodeIdentity>>,
    pub barriers: crate::websocket_transport::AdmissionTestBarriers,
}

#[cfg(feature = "test-support")]
pub async fn websocket_state_with_admission(
    config: WebSocketAdmissionTestConfig,
) -> Arc<WebSocketState> {
    create_test_websocket_state_with_ingress_factory(
        empty_extension_manager().await,
        TestStateOverrides {
            websocket_admission: Some(config),
            ..Default::default()
        },
        || std::future::ready(None),
    )
    .await
}

/// Build a minimal WebSocket state over a caller-owned database pool and ingress authority.
///
/// The ingress authority must come from [`crate::ingress::IngressAuthority::try_new`] when a test
/// drives recovery through `trigger_maintenance`, because it owns the maintenance task.
pub async fn websocket_state_with_ingress(
    db_pool: Arc<DatabasePool>,
    ingress: Arc<IngressAuthority>,
) -> Arc<WebSocketState> {
    build_websocket_state_with_ingress(db_pool, move || std::future::ready(Some(ingress))).await
}

/// Build all fixture schemas before creating an authority that starts maintenance.
///
/// `start_ingress` runs only after the WebSocket state's final schema constructor
/// completes. The returned handle is the exact authority installed in the state,
/// so callers can bind recovery and join its real maintenance task.
///
/// # Panics
/// Panics if database schema or WebSocket fixture construction fails.
pub async fn websocket_state_with_ingress_factory<F, Fut>(
    db_pool: Arc<DatabasePool>,
    start_ingress: F,
) -> (Arc<WebSocketState>, Arc<IngressAuthority>)
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = IngressAuthority>,
{
    let state = build_websocket_state_with_ingress(db_pool, move || async move {
        Some(Arc::new(start_ingress().await))
    })
    .await;
    let ingress = Arc::clone(&state.deps.protocol.ingress);
    (state, ingress)
}

async fn build_websocket_state_with_ingress<F, Fut>(
    db_pool: Arc<DatabasePool>,
    start_ingress: F,
) -> Arc<WebSocketState>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Option<Arc<IngressAuthority>>>,
{
    let database = db_pool.global().clone();
    let pending_delivery_storage = Arc::new(
        DatabasePendingDeliveryStorage::from_database(
            database.clone(),
            waddle_xmpp::pending_delivery::QuotaPolicy::Unlimited,
        )
        .await
        .expect("database-backed pending delivery test storage"),
    );
    let notification_settings_projection =
        Arc::new(NotificationSettingsProjectionStore::new(database.clone()));
    NotificationOutboxStore::try_new(database)
        .await
        .expect("notification outbox test schema");
    create_test_websocket_state_with_ingress_factory(
        empty_extension_manager().await,
        TestStateOverrides {
            db_pool: Some(db_pool),
            pending_delivery_storage: Some(pending_delivery_storage),
            notification_settings_projection: Some(notification_settings_projection),
            ..Default::default()
        },
        start_ingress,
    )
    .await
}
