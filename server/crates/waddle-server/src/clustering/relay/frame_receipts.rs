//! The origin confirms reply receipts only after the client transport writes its frames.
use super::*;
use crate::clustering::ordered_relay::OrderedRelayAck;
use crate::ingress::execute::RelayFrameReceiptCompletion;
use std::collections::HashMap;

const MAX_PENDING_REPLY_RECEIPTS: usize = 128;
const REPLY_RECEIPT_TTL: Duration = Duration::from_secs(30);

/// Unpredictable proof carried only in the reply that contains the frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RelayReplyReceiptToken(uuid::Uuid);

#[derive(Default)]
pub(crate) struct PendingReplyReceipts {
    entries: HashMap<RelayReplyReceiptToken, PendingReplyReceipt>,
}

struct PendingReplyReceipt {
    expires_at: tokio::time::Instant,
    completion: RelayFrameReceiptCompletion,
}

impl PendingReplyReceipts {
    pub(crate) fn register(
        &mut self,
        completion: RelayFrameReceiptCompletion,
    ) -> Option<RelayReplyReceiptToken> {
        let now = tokio::time::Instant::now();
        self.entries.retain(|_, pending| pending.expires_at > now);
        if self.entries.len() >= MAX_PENDING_REPLY_RECEIPTS {
            // Dropping the report preserves its durable unresolved obligations.
            return None;
        }
        let token = RelayReplyReceiptToken(uuid::Uuid::new_v4());
        self.entries.insert(
            token,
            PendingReplyReceipt {
                expires_at: now + REPLY_RECEIPT_TTL,
                completion,
            },
        );
        Some(token)
    }

    fn take(&mut self, token: RelayReplyReceiptToken) -> Option<RelayFrameReceiptCompletion> {
        self.entries
            .remove(&token)
            .filter(|pending| pending.expires_at > tokio::time::Instant::now())
            .map(|pending| pending.completion)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct RelayConfirmReplyReceipt {
    token: RelayReplyReceiptToken,
}

#[kameo::remote_message("waddle.clustering.relay.confirm_reply_receipt.v1")]
impl Message<RelayConfirmReplyReceipt> for RelayActor {
    type Reply = kameo::reply::DelegatedReply<bool>;

    async fn handle(
        &mut self,
        msg: RelayConfirmReplyReceipt,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let receipts = Arc::clone(&self.pending_reply_receipts);
        spawn_in_dispatch_span(
            ctx,
            tracing::info_span!("clustering.relay.reply_receipt"),
            async move {
                let completion = receipts.lock().await.take(msg.token);
                let Some(completion) = completion else {
                    return false;
                };
                match completion.complete().await {
                    Ok(_) => true,
                    Err(error) => {
                        tracing::warn!(%error, "relay reply receipt persistence remains pending");
                        false
                    }
                }
            },
        )
    }
}

impl RelayHandle {
    /// Called only after the client transport successfully writes the reply batch.
    pub(crate) async fn confirm_reply_receipt(
        &mut self,
        token: RelayReplyReceiptToken,
    ) -> Result<bool, RelayAskError> {
        let stop_token = self.stop_token.clone();
        tokio::select! {
            biased;
            _ = stop_token.cancelled() => Err(RelayAskError::Cancelled),
            result = async {
                let remote = self.resolve().await?;
                remote
                    .ask(&RelayConfirmReplyReceipt { token })
                    .mailbox_timeout(Duration::from_secs(1))
                    .reply_timeout(Duration::from_secs(5))
                    .await
                    .map_err(send_error)
            } => result,
        }
    }
}

impl OrderedRelayAck {
    /// Preserve the owner's receipt obligation through the origin's client write.
    pub(crate) fn into_frame_delivery(
        self,
        owner: NodeId,
        stop_token: CancellationToken,
    ) -> (
        Vec<waddle_xmpp::Stanza>,
        Option<RelayFrameReceiptCompletion>,
    ) {
        let completion = self
            .reply_receipt
            .map(|token| RelayFrameReceiptCompletion::remote(owner, token, stop_token));
        let frames = self
            .client_replies
            .into_iter()
            .map(|remote| remote.0)
            .collect();
        (frames, completion)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn completion() -> RelayFrameReceiptCompletion {
        let database = crate::db::Database::in_memory("relay-reply-receipts")
            .await
            .expect("database");
        RelayFrameReceiptCompletion::new(super::super::super::route_bridge::RelayFrameCompletion {
            authority: Arc::new(crate::ingress::IngressAuthority::for_test(database).await),
            report: crate::ingress::execute::ExecutionReport::default(),
        })
    }

    #[tokio::test]
    async fn ingress_relay_reply_requires_exact_received_token_and_consumes_it_once() {
        let mut pending = PendingReplyReceipts::default();
        let token = pending.register(completion().await).expect("registered");
        assert!(pending
            .take(RelayReplyReceiptToken(uuid::Uuid::new_v4()))
            .is_none());
        assert_eq!(pending.entries.len(), 1, "unreceived replies stay pending");
        assert!(
            pending.take(token).is_some(),
            "the transport completion confirms receipt"
        );
        assert!(
            pending.take(token).is_none(),
            "confirmation is consumed once"
        );
    }

    #[tokio::test]
    async fn ingress_relay_lost_reply_expires_without_releasing_completion() {
        let mut pending = PendingReplyReceipts::default();
        let token = pending.register(completion().await).expect("registered");
        pending.entries.get_mut(&token).expect("pending").expires_at = tokio::time::Instant::now();
        assert!(pending.take(token).is_none());
        assert!(pending.entries.is_empty());
    }

    #[tokio::test]
    async fn ingress_relay_pending_reply_memory_is_bounded() {
        let mut pending = PendingReplyReceipts::default();
        let completion = completion().await;
        for _ in 0..MAX_PENDING_REPLY_RECEIPTS {
            assert!(pending.register(completion.clone()).is_some());
        }
        assert!(pending.register(completion).is_none());
        assert_eq!(pending.entries.len(), MAX_PENDING_REPLY_RECEIPTS);
    }
}
