//! ADR-0017 Phase 3 Slice 6: the `waddle_xmpp::stream_management::RemoteResumeAsker`
//! implementation over `RelayHandle` — the resuming node's side of the
//! cross-node XEP-0198 resume live-steal handshake.
//!
//! Kept separate from [`super::resume_bridge`] (the answering node's side,
//! reachable from `RelayActor`) since the two run on different nodes and
//! have no shared state: this type only ever constructs fresh
//! [`super::relay::RelayHandle`]s per ask.

use std::time::Duration;

use jid::BareJid;
use tokio_util::sync::CancellationToken;
use waddle_xmpp::ownership::{ClaimEpoch, NodeIdentity};
use waddle_xmpp::stream_management::{RemoteResumeAskOutcome, RemoteResumeAsker};

use super::relay::{RelayHandle, RelayResumeStealReply};
use super::NodeId;

/// Constructs a fresh [`RelayHandle`] per ask (the target `node_id` varies
/// call to call, and `RelayHandle` is bound to one node at construction).
pub struct SwarmRemoteResumeAsker {
    stop_token: CancellationToken,
    mailbox_timeout: Duration,
    reply_timeout: Duration,
}

impl SwarmRemoteResumeAsker {
    pub fn new(
        stop_token: CancellationToken,
        mailbox_timeout: Duration,
        reply_timeout: Duration,
    ) -> Self {
        Self {
            stop_token,
            mailbox_timeout,
            reply_timeout,
        }
    }
}

#[async_trait::async_trait]
impl RemoteResumeAsker for SwarmRemoteResumeAsker {
    async fn ask_remote_detach(
        &self,
        expected_owner: &NodeIdentity,
        observed: ClaimEpoch,
        stream_id: &str,
        requester_bare_jid: &BareJid,
    ) -> RemoteResumeAskOutcome {
        let mut relay = RelayHandle::new(
            NodeId::new(expected_owner.node_id.clone()),
            self.stop_token.clone(),
        )
        .with_ask_timeouts(self.mailbox_timeout, self.reply_timeout);
        match relay
            .resume_steal(
                waddle_xmpp::pending_delivery::SmSessionId::new(stream_id.to_string()),
                requester_bare_jid.clone(),
                expected_owner.clone(),
                observed,
            )
            .await
        {
            Ok(RelayResumeStealReply::Detached) => RemoteResumeAskOutcome::Detached,
            Ok(RelayResumeStealReply::IdentityMismatch) => RemoteResumeAskOutcome::IdentityMismatch,
            Ok(RelayResumeStealReply::NotLiveLocally) => RemoteResumeAskOutcome::NotLiveRemotely,
            Err(error) => {
                tracing::debug!(
                    node_id = %expected_owner.node_id,
                    node_epoch = %expected_owner.node_epoch,
                    claim_epoch = observed.0,
                    stream_id,
                    %error,
                    "cross-node resume: resume_steal ask failed; treating as owner-unreachable"
                );
                RemoteResumeAskOutcome::Unreachable
            }
        }
    }
}
