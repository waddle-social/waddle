//! Delivery of what the S2S router owes a local resource: the response
//! that completes its request to a remote entity, or the error answering
//! that request.

use std::sync::{Arc, Weak};

use async_trait::async_trait;
use jid::FullJid;
use waddle_xmpp::Stanza;

use super::interpret_loop::build_interpret_deps;
use super::WebSocketState;
use crate::s2s::route::{LocalDelivery, LocalDeliveryOutcome};
use crate::server::routes::interpret::{
    deliver_peer_to_full_with_registered_remote, FullJidDeliveryOutcome,
};

/// [`LocalDelivery`] through the WebSocket connection registry and, in a
/// cluster, the replica that holds the resource's connection. The stanza
/// takes the peer-stanza path, so the recipient's blocking list applies.
///
/// It holds the state weakly: the state holds the router, which holds this.
pub(crate) struct WebSocketLocalDelivery {
    state: Weak<WebSocketState>,
}

impl WebSocketLocalDelivery {
    pub(crate) fn new(state: &Arc<WebSocketState>) -> Self {
        Self {
            state: Arc::downgrade(state),
        }
    }
}

#[async_trait]
impl LocalDelivery for WebSocketLocalDelivery {
    async fn deliver(&self, to: &FullJid, stanza: Stanza) -> LocalDeliveryOutcome {
        let Some(state) = self.state.upgrade() else {
            return LocalDeliveryOutcome::Unavailable;
        };
        let deps = build_interpret_deps(&state, None);
        match deliver_peer_to_full_with_registered_remote(&deps, to, &stanza).await {
            FullJidDeliveryOutcome::Delivered
            | FullJidDeliveryOutcome::QueuedDetached
            | FullJidDeliveryOutcome::MaybeCommitted => LocalDeliveryOutcome::Delivered,
            FullJidDeliveryOutcome::Dropped | FullJidDeliveryOutcome::Unavailable => {
                LocalDeliveryOutcome::Unavailable
            }
        }
    }
}
