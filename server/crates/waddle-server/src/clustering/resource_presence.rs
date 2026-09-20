//! #1803 asking side: "does the node owning this account's `UserActor` claim
//! know this exact full JID?"
//!
//! A `UserActor` claim is per account, the ghost-occupant question is per
//! resource. The room's host node therefore cannot answer from the claim row
//! alone — it has to ask the owner. That hop is expressed as a trait so the
//! ghost-eviction guard can be exercised against a scripted owner in unit
//! fixtures while production runs the real relay ask.
//!
//! Every failure is fail-closed at the call site
//! (`ingress::recovery_ghosts::foreign_claim`): an `Err` of any kind — an old
//! peer answering `UnknownMessage`, a timeout, a transport failure, a decode
//! failure — leaves the occupant seated and its copy owed.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use jid::FullJid;
use tokio_util::sync::CancellationToken;
use waddle_xmpp::ownership::NodeIdentity;

use super::relay::{
    RelayAskError, RelayHandle, RelayResourcePresenceReply, RelaySendEffect, RelaySendFailure,
};
use super::NodeId;

/// Overall bound on one cross-node presence hop, kademlia name resolution
/// included (that resolution sits OUTSIDE the per-ask timeouts and can burn
/// its own backoff budget). Deliberately tight: this runs inside one bounded
/// maintenance recovery attempt on a row that is already stalled, and an
/// answer that arrives late is worth nothing — the next stalled pass re-asks
/// against a warm relay cache.
const RESOURCE_PRESENCE_ASK_TIMEOUT: Duration = Duration::from_secs(1);
/// Per-ask mailbox/reply split inside [`RESOURCE_PRESENCE_ASK_TIMEOUT`], far
/// under the clustering defaults (5s/20s) for the same reason.
const RESOURCE_PRESENCE_MAILBOX_TIMEOUT: Duration = Duration::from_millis(250);
const RESOURCE_PRESENCE_REPLY_TIMEOUT: Duration = Duration::from_millis(750);

/// Ask the node holding an account's `UserActor` claim about one exact
/// resource.
#[async_trait]
pub trait ResourcePresenceAsker: Send + Sync {
    /// `Ok(Absent)` is the only answer that proves the resource is gone.
    /// Every other reply, and every `Err`, means "could not prove absence".
    async fn resource_presence(
        &self,
        owner: &NodeIdentity,
        target: &FullJid,
    ) -> Result<RelayResourcePresenceReply, RelayAskError>;
}

/// Production implementation: one bounded [`RelayHandle`] ask per probe,
/// resolved through kademlia exactly like the #1594 webhook relay hop.
pub struct RelayResourcePresenceAsker {
    stop_token: CancellationToken,
}

impl RelayResourcePresenceAsker {
    pub fn new(stop_token: CancellationToken) -> Arc<Self> {
        Arc::new(Self { stop_token })
    }
}

#[async_trait]
impl ResourcePresenceAsker for RelayResourcePresenceAsker {
    async fn resource_presence(
        &self,
        owner: &NodeIdentity,
        target: &FullJid,
    ) -> Result<RelayResourcePresenceReply, RelayAskError> {
        let mut relay =
            RelayHandle::new(NodeId::new(owner.node_id.clone()), self.stop_token.clone())
                .with_ask_timeouts(
                    RESOURCE_PRESENCE_MAILBOX_TIMEOUT,
                    RESOURCE_PRESENCE_REPLY_TIMEOUT,
                );
        match tokio::time::timeout(
            RESOURCE_PRESENCE_ASK_TIMEOUT,
            relay.resource_presence(target.clone()),
        )
        .await
        {
            Ok(result) => result,
            // The receiver's handler is read-only, so an elapsed overall
            // budget has no effect to reconcile — it is simply "no answer".
            Err(_elapsed) => Err(RelayAskError::Send {
                failure: RelaySendFailure::ReplyTimeout,
                effect: RelaySendEffect::NoEffect,
                message: "resource-presence ask exceeded its overall budget".to_string(),
            }),
        }
    }
}
