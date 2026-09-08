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

pub(crate) struct PendingReplyReceipts {
    capacity: Arc<tokio::sync::Semaphore>,
    entries: HashMap<RelayReplyReceiptToken, PendingReplyReceipt>,
}

impl Default for PendingReplyReceipts {
    fn default() -> Self {
        Self {
            capacity: Arc::new(tokio::sync::Semaphore::new(MAX_PENDING_REPLY_RECEIPTS)),
            entries: HashMap::new(),
        }
    }
}

struct PendingReplyReceipt {
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
    expires_at: tokio::time::Instant,
    completion: ReplyReceiptCompletion,
}

#[derive(Clone)]
enum ReplyReceiptCompletion {
    Pending(RelayFrameReceiptCompletion),
    Confirmed,
}

impl PendingReplyReceipts {
    pub(crate) fn reserve(&mut self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        let now = tokio::time::Instant::now();
        self.entries.retain(|_, pending| pending.expires_at > now);
        Arc::clone(&self.capacity).try_acquire_owned().ok()
    }

    #[cfg(test)]
    pub(crate) fn register(
        &mut self,
        completion: RelayFrameReceiptCompletion,
    ) -> Option<RelayReplyReceiptToken> {
        let permit = self.reserve()?;
        Some(self.register_reserved(permit, completion))
    }

    pub(crate) fn register_reserved(
        &mut self,
        permit: tokio::sync::OwnedSemaphorePermit,
        completion: RelayFrameReceiptCompletion,
    ) -> RelayReplyReceiptToken {
        let token = RelayReplyReceiptToken(uuid::Uuid::new_v4());
        self.entries.insert(
            token,
            PendingReplyReceipt {
                permit: Some(permit),
                expires_at: tokio::time::Instant::now() + REPLY_RECEIPT_TTL,
                completion: ReplyReceiptCompletion::Pending(completion),
            },
        );
        token
    }

    pub(crate) fn contains(&mut self, token: RelayReplyReceiptToken) -> bool {
        self.get(token).is_some()
    }

    #[cfg(test)]
    fn pending_count(&self) -> usize {
        self.entries
            .values()
            .filter(|entry| matches!(entry.completion, ReplyReceiptCompletion::Pending(_)))
            .count()
    }

    fn get(&mut self, token: RelayReplyReceiptToken) -> Option<ReplyReceiptCompletion> {
        self.entries
            .retain(|_, pending| pending.expires_at > tokio::time::Instant::now());
        self.entries
            .get(&token)
            .map(|pending| pending.completion.clone())
    }
}

async fn confirm(receipts: &Mutex<PendingReplyReceipts>, token: RelayReplyReceiptToken) -> bool {
    // Clone the shared, idempotent completion before awaiting storage. A failed
    // or cancelled confirmation must not consume the only proof of the write.
    let Some(completion) = receipts.lock().await.get(token) else {
        return false;
    };
    let ReplyReceiptCompletion::Pending(completion) = completion else {
        return true;
    };
    match completion.complete().await {
        Ok(_) => {
            if let Some(entry) = receipts.lock().await.entries.get_mut(&token) {
                // Release the heavy report and pending capacity, while retaining
                // the small proof through its original TTL for lost confirmations.
                // Completed token count follows throughput during this bounded TTL.
                entry.completion = ReplyReceiptCompletion::Confirmed;
                entry.permit = None;
            }
            true
        }
        Err(error) => {
            tracing::warn!(%error, "relay reply receipt persistence remains pending");
            false
        }
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
            async move { confirm(&receipts, msg.token).await },
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
        let completion = self.reply_receipt.map(|token| {
            RelayFrameReceiptCompletion::remote(owner, token, self.owner_receipts, stop_token)
        });
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
    async fn ingress_relay_full_reply_table_backpressures_before_owner_effects() {
        let receipts = Arc::new(Mutex::new(PendingReplyReceipts::default()));
        let completion = completion().await;
        let mut tokens = Vec::new();
        for _ in 0..MAX_PENDING_REPLY_RECEIPTS {
            tokens.push(
                receipts
                    .lock()
                    .await
                    .register(completion.clone())
                    .expect("capacity"),
            );
        }
        let receiver = Arc::new(Mutex::new(OrderedRelayReceiverState::default()));
        let envelope = super::super::tests::timeout_envelope();
        // An unwired owner bridge returns Unreachable if execution is entered.
        // Backpressure must be decided first, without entering that delivery path.
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &crate::config::ClusteringMessagingConfig::default(),
        );
        let reservation = receiver.lock().await.reserve(envelope.clone());
        let reply = finish_ordered_reservation(
            Arc::clone(&receiver),
            Arc::clone(&bridge),
            reservation,
            Arc::clone(&receipts),
        )
        .await;
        assert!(matches!(
            reply,
            OrderedRelayReply::Nack(OrderedRelayNack {
                reason: OrderedRelayNackReason::ReplyReceiptBackpressure,
                ..
            })
        ));
        assert_eq!(
            receipts.lock().await.pending_count(),
            MAX_PENDING_REPLY_RECEIPTS
        );
        assert!(confirm(&receipts, tokens[0]).await);
        let retry = receiver.lock().await.reserve(envelope);
        assert!(
            matches!(retry, OrderedRelayReservation::Reserved(_)),
            "backpressure does not consume or divert the sequence"
        );
        let reply =
            finish_ordered_reservation(receiver, bridge, retry, Arc::clone(&receipts)).await;
        assert!(
            matches!(
                reply,
                OrderedRelayReply::Nack(OrderedRelayNack {
                    reason: OrderedRelayNackReason::Unreachable,
                    ..
                })
            ),
            "released capacity permits entry to owner delivery"
        );
        assert!(
            receipts.lock().await.reserve().is_some(),
            "failed owner delivery releases its permit"
        );
    }

    #[tokio::test]
    async fn ingress_relay_expired_cached_proof_never_acknowledges_without_receipt() {
        let receipts = Arc::new(Mutex::new(PendingReplyReceipts::default()));
        let token = receipts
            .lock()
            .await
            .register(completion().await)
            .expect("token");
        let receiver = Arc::new(Mutex::new(OrderedRelayReceiverState::default()));
        let envelope = super::super::tests::timeout_envelope();
        let OrderedRelayReservation::Reserved(reserved) =
            receiver.lock().await.reserve(envelope.clone())
        else {
            panic!("first reservation")
        };
        receiver.lock().await.commit_reserved_with_reply_receipt(
            *reserved,
            Vec::new(),
            Some(token),
            Vec::new(),
        );
        receipts
            .lock()
            .await
            .entries
            .get_mut(&token)
            .expect("pending token")
            .expires_at = tokio::time::Instant::now();
        let retry = receiver.lock().await.reserve(envelope);
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &crate::config::ClusteringMessagingConfig::default(),
        );
        let reply = finish_ordered_reservation(receiver, bridge, retry, receipts).await;
        assert!(matches!(
            reply,
            OrderedRelayReply::Nack(OrderedRelayNack {
                reason: OrderedRelayNackReason::MaybeCommitted,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn ingress_relay_reply_requires_exact_received_token_and_retains_retry_proof() {
        let mut pending = PendingReplyReceipts::default();
        let token = pending.register(completion().await).expect("registered");
        assert!(pending
            .get(RelayReplyReceiptToken(uuid::Uuid::new_v4()))
            .is_none());
        assert_eq!(pending.entries.len(), 1, "unreceived replies stay pending");
        assert!(
            pending.get(token).is_some(),
            "the transport completion confirms receipt"
        );
        assert!(
            pending.get(token).is_some(),
            "confirmation remains retryable until expiry"
        );
    }

    #[tokio::test]
    async fn ingress_relay_lost_reply_expires_without_releasing_completion() {
        let mut pending = PendingReplyReceipts::default();
        let token = pending.register(completion().await).expect("registered");
        pending.entries.get_mut(&token).expect("pending").expires_at = tokio::time::Instant::now();
        assert!(pending.get(token).is_none());
        assert!(pending.entries.is_empty());
    }

    #[tokio::test]
    async fn ingress_relay_confirmed_tokens_release_pending_capacity_and_expire() {
        let pending = Mutex::new(PendingReplyReceipts::default());
        let completion = completion().await;
        let mut tokens = Vec::new();
        for _ in 0..MAX_PENDING_REPLY_RECEIPTS {
            tokens.push(
                pending
                    .lock()
                    .await
                    .register(completion.clone())
                    .expect("capacity"),
            );
        }
        assert!(pending.lock().await.register(completion.clone()).is_none());
        for token in &tokens {
            assert!(confirm(&pending, *token).await);
            assert!(confirm(&pending, *token).await, "lost confirmation retries");
        }
        let mut entries = pending.lock().await;
        assert_eq!(entries.pending_count(), 0);
        assert!(
            entries.register(completion).is_some(),
            "confirmed proofs do not consume pending capacity"
        );
        for token in &tokens {
            entries
                .entries
                .get_mut(token)
                .expect("confirmed proof")
                .expires_at = tokio::time::Instant::now();
        }
        assert!(entries.get(tokens[0]).is_none());
        assert_eq!(
            entries.entries.len(),
            1,
            "expired confirmed proofs are pruned"
        );
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

#[cfg(test)]
#[path = "frame_receipts_tests.rs"]
mod persistence_tests;
