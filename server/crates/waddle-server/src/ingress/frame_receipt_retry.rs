use super::IngressAuthority;
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Weak},
    time::Duration,
};
use waddle_xmpp::{ingress::MessageKey, stream_management::SmIngressFrameReceipt};

/// Proofs outlive socket state. The queue lock never covers database work.
#[derive(Default)]
pub(super) struct FrameReceiptRetries {
    messages: HashMap<MessageKey, Vec<SmIngressFrameReceipt>>,
    order: VecDeque<MessageKey>,
    pub(super) task: Option<tokio::task::JoinHandle<()>>,
}

impl IngressAuthority {
    /// Return once proofs are retained; storage completion runs independently.
    pub(crate) async fn retry_frame_receipts(
        self: &Arc<Self>,
        receipts: &[SmIngressFrameReceipt],
    ) -> bool {
        let mut queue = self
            .frame_receipt_retries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for receipt in receipts {
            if !queue.messages.contains_key(&receipt.message_key) {
                queue.order.push_back(receipt.message_key);
            }
            let pending = queue.messages.entry(receipt.message_key).or_default();
            if !pending.contains(receipt) {
                pending.push(receipt.clone());
            }
        }
        if !queue.order.is_empty() && queue.task.as_ref().is_none_or(|task| task.is_finished()) {
            queue.task = Some(tokio::spawn(retry(Arc::downgrade(self))));
        }
        true
    }
}

async fn retry(authority: Weak<IngressAuthority>) {
    loop {
        let Some(current) = authority.upgrade() else {
            return;
        };
        let receipts = {
            let mut queue = current
                .frame_receipt_retries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(key) = queue.order.front() else {
                // Clear under the retention lock, so a concurrent enqueue starts
                // a worker even before this task has returned.
                queue.task.take();
                return;
            };
            queue.messages[key].clone()
        };
        let mut reports = super::ExecutionReport::replay_frame_completions(&receipts);
        let report = &mut reports[0];
        let result = tokio::select! {
            biased;
            _ = current.cancellation.cancelled() => return,
            result = current.complete_frame_obligations(report) => result,
        };
        if let Err(error) = result {
            tracing::warn!(%error, "Failed to receipt retained ingress frames");
            let cancellation = current.cancellation.clone();
            drop(current);
            tokio::select! {
                _ = cancellation.cancelled() => return,
                _ = tokio::time::sleep(Duration::from_millis(250)) => {}
            }
            continue;
        }
        let mut queue = current
            .frame_receipt_retries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = receipts[0].message_key;
        if let Some(pending) = queue.messages.get_mut(&key) {
            pending.retain(|receipt| !receipts.contains(receipt));
            if pending.is_empty() {
                queue.messages.remove(&key);
                queue.order.pop_front();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ingress_receipt_timeouts_do_not_serialize_proof_retention() {
        let database = crate::db::Database::in_memory("receipt-retry-isolation")
            .await
            .expect("database");
        let authority = Arc::new(IngressAuthority::for_test(database).await);
        tokio::time::pause();
        // The persistence operation waits for admission until its full five
        // second budget expires. Socket-facing retention must never await it.
        let admission = authority.admission.write().await;
        let receipt = SmIngressFrameReceipt {
            message_key: MessageKey::new(),
            kind: waddle_xmpp::stream_management::SmIngressReceiptKind::from_storage(1),
            semantic_identity_hash: [1; 32],
        };
        assert!(
            authority
                .retry_frame_receipts(std::slice::from_ref(&receipt))
                .await
        );
        tokio::task::yield_now().await;
        let worker = authority
            .frame_receipt_retries
            .lock()
            .expect("queue")
            .task
            .as_ref()
            .expect("worker")
            .id();
        for _ in 0..2 {
            tokio::time::advance(Duration::from_secs(6)).await;
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_millis(250)).await;
            tokio::task::yield_now().await;
            let start = tokio::time::Instant::now();
            assert!(tokio::time::timeout(
                Duration::from_millis(10),
                authority.retry_frame_receipts(std::slice::from_ref(&receipt))
            )
            .await
            .expect("unrelated socket proof retention is nonblocking"));
            assert!(tokio::time::timeout(
                Duration::from_millis(10),
                authority.retry_frame_receipts(&[])
            )
            .await
            .expect("empty batch tick is nonblocking"));
            assert_eq!(start.elapsed(), Duration::ZERO);
            let queue = authority.frame_receipt_retries.lock().expect("queue");
            assert_eq!(queue.task.as_ref().expect("same worker").id(), worker);
            assert_eq!(queue.messages[&receipt.message_key], vec![receipt.clone()]);
        }
        drop(admission);
        assert!(authority.drain_and_join(Duration::from_secs(1)).await);
    }
}
