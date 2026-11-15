//! Delivery of what the S2S router owes a local resource: the response
//! that completes its request to a remote entity, or the error answering
//! that request.

use std::sync::{Arc, Weak};

use async_trait::async_trait;
use jid::FullJid;
use waddle_xmpp::Stanza;

use super::interpret_loop::build_interpret_deps;
use super::WebSocketState;
use crate::s2s::route::{LocalDelivery, LocalDeliveryOutcome, LocalOrigin};
use crate::server::routes::interpret::{
    deliver_direct_to_full_with_registered_remote, deliver_peer_to_full_with_registered_remote,
    FullJidDeliveryOutcome,
};

/// [`LocalDelivery`] through the WebSocket connection registry and, in a
/// cluster, the replica that holds the resource's connection. A detached
/// resource with stream management receives it into that session's custody.
///
/// A remote entity's response takes the peer-stanza path, so the
/// recipient's XEP-0191 blocking list applies and a response from a blocked
/// address is dropped. An error this server generated takes the direct-frame
/// path: it names the remote entity as `from`, but that entity did not send
/// it, so the blocking list must not swallow the only answer the request
/// will get.
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
    async fn deliver(
        &self,
        to: &FullJid,
        stanza: Stanza,
        origin: LocalOrigin,
    ) -> LocalDeliveryOutcome {
        let Some(state) = self.state.upgrade() else {
            return LocalDeliveryOutcome::Unavailable;
        };
        let deps = build_interpret_deps(&state, None);
        let outcome = match origin {
            LocalOrigin::Remote => {
                deliver_peer_to_full_with_registered_remote(&deps, to, &stanza).await
            }
            LocalOrigin::Server => {
                deliver_direct_to_full_with_registered_remote(&deps, to, &stanza).await
            }
        };
        match outcome {
            FullJidDeliveryOutcome::Delivered
            | FullJidDeliveryOutcome::QueuedDetached
            | FullJidDeliveryOutcome::MaybeCommitted => LocalDeliveryOutcome::Delivered,
            FullJidDeliveryOutcome::Dropped | FullJidDeliveryOutcome::Unavailable => {
                LocalDeliveryOutcome::Unavailable
            }
        }
    }
}
