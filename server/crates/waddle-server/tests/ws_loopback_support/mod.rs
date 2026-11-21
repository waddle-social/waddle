//! An in-process server for suites that drive the production `/ws` router
//! (RFC 7395) against a test state they also set up directly: accounts,
//! catalog rows, permission relationships and PubSub nodes.
//!
//! Clients authenticate with SASL SCRAM-SHA-256 against native accounts and
//! bind a resource, as a deployed client does. Stanzas are built and parsed
//! as typed values with `waddle_ws_test_support::stanza`; this module holds
//! only what needs the server's test state, so every suite that includes it
//! uses all of it.

use std::sync::Arc;

use jid::BareJid;
use waddle_server::auth::{NativeUserStore, RegisterRequest};
use waddle_server::db::actor::DbExecute;
use waddle_server::test_support::{router, seed_local_account, WebSocketState};
use waddle_ws_test_support::WsXmppClient;

/// The test state pins its account domain (`test_state.rs`).
pub const DOMAIN: &str = "example.com";
const PASSWORD: &str = "loopback-account-password";

/// A test state served over a loopback TCP listener.
pub struct Loopback {
    pub state: Arc<WebSocketState>,
    url: String,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Loopback {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Loopback {
    pub async fn serve(state: Arc<WebSocketState>) -> Self {
        // As startup does once the state is built: admin commands notify
        // the subscribers of what they take out of a node.
        waddle_server::test_support::bind_admin_notifications(&state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback listener");
        let addr = listener.local_addr().expect("listener address");
        let app = router(Arc::clone(&state));
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self {
            state,
            url: format!("ws://{addr}/ws"),
            server,
        }
    }

    /// Create the native account `localpart@example.com`.
    pub async fn account(&self, localpart: &str) -> BareJid {
        seed_local_account(&self.state, localpart).await;
        NativeUserStore::new(self.state.deps.app_state.db_pool.global_actor().clone())
            .register(RegisterRequest {
                username: localpart.to_owned(),
                domain: DOMAIN.to_owned(),
                password: PASSWORD.to_owned(),
                email: None,
            })
            .await
            .expect("register native account");
        format!("{localpart}@{DOMAIN}")
            .parse()
            .expect("account JID")
    }

    /// An authenticated client of `localpart` with `resource` bound.
    pub async fn client(&self, localpart: &str, resource: &str) -> WsXmppClient {
        WsXmppClient::connect_and_auth(&self.url, DOMAIN, localpart, PASSWORD, resource)
            .await
            .expect("authenticated client")
    }

    /// Insert or replace a channel catalog row, as the admin commands and
    /// the topology bootstrap write it.
    pub async fn channel(&self, id: &str, members_only: bool) {
        let now = chrono::Utc::now().to_rfc3339();
        self.state
            .deps
            .app_state
            .db_pool
            .global_actor()
            .ask(DbExecute {
                sql: "INSERT INTO channels (id, name, description, channel_type, position, \
                      is_default, pin_permission, members_only, public_room, created_at, \
                      updated_at) VALUES (?, ?, ?, 'text', 0, ?, 'anyone', ?, ?, ?, ?) \
                      ON CONFLICT(id) DO UPDATE SET members_only = excluded.members_only"
                    .to_owned(),
                params: vec![
                    id.into(),
                    id.into(),
                    None::<String>.into(),
                    false.into(),
                    members_only.into(),
                    true.into(),
                    now.clone().into(),
                    now.into(),
                ],
            })
            .await
            .expect("channel catalog row");
    }
}
