//! Fixtures shared by the feed and stories verb suites.

use std::sync::Arc;

use crate::{WaddleClient, WaddleClientEvent, WaddleConfig, WaddleEventListener};

struct SilentListener;

impl WaddleEventListener for SilentListener {
    fn on_event(&self, _event: WaddleClientEvent) {}
}

/// A client for `alice@waddle.test` that never connected.
pub(crate) fn offline_client() -> Arc<WaddleClient> {
    Arc::new(WaddleClient {
        config: WaddleConfig {
            server_url: "wss://xmpp.waddle.test".to_string(),
            jid: "alice@waddle.test".to_string(),
            access_token: "token".to_string(),
            resource: "test".to_string(),
            resumable: true,
        },
        listener: Arc::new(Box::new(SilentListener) as Box<dyn WaddleEventListener>),
        handle: tokio::sync::Mutex::new(None),
        inbox_query_gate: tokio::sync::Mutex::new(()),
    })
}
