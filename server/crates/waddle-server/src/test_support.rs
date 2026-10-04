//! Minimal public fixtures for this crate's XEP integration suites.
//!
//! The helpers are test-only by construction and use `expect` internally when
//! creating fixed local fixtures; they are never enabled in release images.

use std::sync::Arc;

pub use crate::server::routes::websocket::muc_invites::{
    claim_invite, list_invites, record_invite_at, InviteStorageError, OutstandingInvite,
    RecordOutcome,
};
pub use crate::server::routes::websocket::test_state::{
    create_test_session, register_test_connection, seed_local_account,
};
pub use crate::server::routes::websocket::WebSocketState;

/// Build a minimal WebSocket state over a caller-owned database pool and ingress authority.
///
/// The ingress authority must come from [`crate::ingress::IngressAuthority::new`] when a test
/// drives recovery through `trigger_maintenance`, because it owns the maintenance task.
pub async fn websocket_state_with_ingress(
    db_pool: Arc<crate::db::DatabasePool>,
    ingress: Arc<crate::ingress::IngressAuthority>,
) -> Arc<WebSocketState> {
    let state = crate::server::routes::websocket::test_state::create_test_websocket_state_with_extension_manager(
        crate::server::routes::websocket::test_state::empty_extension_manager().await,
        crate::server::routes::websocket::test_state::TestStateOverrides {
            db_pool: Some(Arc::clone(&db_pool)),
            ingress: Some(ingress),
            ..Default::default()
        },
    )
    .await;
    let mut state = Arc::try_unwrap(state).unwrap_or_else(|_| panic!("unique test state"));
    state.deps.protocol.notification_settings_projection = Arc::new(
        crate::notification_settings_projection::NotificationSettingsProjectionStore::new(
            db_pool.global().clone(),
        ),
    );
    state.deps.protocol.pending_delivery_storage = Arc::new(
        crate::pending_delivery::DatabasePendingDeliveryStorage::from_database(
            db_pool.global().clone(),
            waddle_xmpp::pending_delivery::QuotaPolicy::Unlimited,
        )
        .await
        .expect("database-backed pending delivery test storage"),
    );
    crate::notification_outbox::NotificationOutboxStore::new(db_pool.global().clone())
        .await
        .expect("notification outbox test schema");
    Arc::new(state)
}
