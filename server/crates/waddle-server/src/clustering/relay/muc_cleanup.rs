//! A retained departure needs no live socket registration, only its exact
//! occupancy generation. The trusted cluster forwards it through the current
//! UserActor claim owner, which still fences the ordinary ordered MUC relay.
use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayMucCleanup {
    pub sender: jid::FullJid,
    pub occupant: jid::FullJid,
    pub generation: waddle_xmpp_core::OccupancySessionGeneration,
    pub user_claim_epoch: ClaimEpoch,
    #[serde(default)]
    pub trace: RelayTraceContext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Reply)]
pub enum RelayMucCleanupOutcome {
    Converged,
    Retry,
}

#[kameo::remote_message("waddle.clustering.relay.muc_cleanup.v1")]
impl Message<RelayMucCleanup> for RelayActor {
    type Reply = kameo::reply::DelegatedReply<RelayMucCleanupOutcome>;

    async fn handle(
        &mut self,
        msg: RelayMucCleanup,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let span = relay_dispatch_span(RelayDispatchKind::MucCleanup, &msg.trace);
        span.record("jid", tracing::field::display(&msg.sender));
        let bridge = Arc::clone(&self.ordered_delivery_bridge);
        spawn_in_dispatch_span(ctx, span, async move {
            bridge.cleanup_muc_on_user_owner(msg).await
        })
    }
}

impl RelayHandle {
    pub async fn cleanup_muc(
        &mut self,
        mut message: RelayMucCleanup,
    ) -> Result<RelayMucCleanupOutcome, RelayAskError> {
        message.trace = RelayTraceContext::capture();
        let stop_token = self.stop_token.clone();
        tokio::select! {
            biased;
            _ = stop_token.cancelled() => Err(RelayAskError::Cancelled),
            result = self.cleanup_muc_inner(message) => result,
        }
    }

    async fn cleanup_muc_inner(
        &mut self,
        message: RelayMucCleanup,
    ) -> Result<RelayMucCleanupOutcome, RelayAskError> {
        let remote_ref = self.resolve().await?;
        match remote_ref
            .ask(&message)
            .mailbox_timeout(self.mailbox_timeout)
            .reply_timeout(self.reply_timeout)
            .await
        {
            Ok(reply) => Ok(reply),
            Err(error) if is_no_effect_stale_ref_relookup_error(&error) => {
                self.cached = None;
                self.resolve()
                    .await?
                    .ask(&message)
                    .mailbox_timeout(self.mailbox_timeout)
                    .reply_timeout(self.reply_timeout)
                    .await
                    .map_err(send_error)
            }
            Err(error) => Err(send_error(error)),
        }
    }
}
