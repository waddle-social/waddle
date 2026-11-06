//! Bounded continuation of already-proven live executor receipts.
//!
//! This worker only inserts exact receipt identities. It never runs an effect
//! or terminalizes a message, and its admission guard outlives the caller.
#[cfg(feature = "test-support")]
use std::collections::HashMap;
use std::collections::VecDeque;
#[cfg(feature = "test-support")]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tokio::sync::{oneshot, Notify, OwnedRwLockReadGuard, RwLock};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use waddle_xmpp::ingress::MessageKey;

use super::execute::ProvenReceiptBatch;
use super::EffectReceiptKey;
use crate::db::Database;
use crate::ingress_uow::EffectReceiptRepository;

const MAX_BATCHES: usize = 256;
const MAX_RECEIPTS: usize = 4096;
const WORKERS: usize = 8;
const ATTEMPT_BUDGET: Duration = Duration::from_secs(5);
const RETRY_DELAY: Duration = Duration::from_millis(250);

struct Batch {
    message: MessageKey,
    receipts: Vec<EffectReceiptKey>,
    next: usize,
    completion: oneshot::Sender<()>,
    _admission: OwnedRwLockReadGuard<bool>,
}

#[derive(Default)]
struct Queue {
    closed: bool,
    incomplete: bool,
    batches: VecDeque<Batch>,
    outstanding_batches: usize,
    outstanding_receipts: usize,
    worker: Option<JoinHandle<()>>,
    #[cfg(feature = "test-support")]
    test_capacity: Option<(usize, usize)>,
}

struct Shared {
    database: Database,
    admission: Arc<RwLock<bool>>,
    force_stop: CancellationToken,
    fatal_fence: OnceLock<CancellationToken>,
    queue: Mutex<Queue>,
    notify: Notify,
    #[cfg(feature = "test-support")]
    probes: Mutex<HashMap<MessageKey, Arc<ReceiptCustodyProbe>>>,
}

pub(super) struct ReceiptCustody {
    shared: Arc<Shared>,
    joining: tokio::sync::Mutex<()>,
}

impl ReceiptCustody {
    pub(super) fn new(
        database: Database,
        admission: Arc<RwLock<bool>>,
        force_stop: CancellationToken,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                database,
                admission,
                force_stop,
                fatal_fence: OnceLock::new(),
                queue: Mutex::new(Queue::default()),
                notify: Notify::new(),
                #[cfg(feature = "test-support")]
                probes: Mutex::new(HashMap::new()),
            }),
            joining: tokio::sync::Mutex::new(()),
        }
    }

    pub(super) fn bind_fatal_fence(&self, fatal_fence: CancellationToken) -> bool {
        self.shared.fatal_fence.set(fatal_fence).is_ok()
    }

    pub(super) fn has_fatal_fence(&self) -> bool {
        self.shared.fatal_fence.get().is_some()
    }

    #[cfg(feature = "test-support")]
    pub(super) fn shares_ingress_lane(&self, database: &Database) -> bool {
        self.shared.database.shares_ingress_lane(database)
    }

    /// Retain the complete eligible set synchronously, or reject it in full.
    /// Only the admitted live executor calls this after calculating proof for
    /// all siblings and excluding transaction-arm-owned receipts.
    pub(super) fn retain_proven(
        &self,
        proof: ProvenReceiptBatch,
    ) -> Result<oneshot::Receiver<()>, ()> {
        let (message, receipts) = proof.into_parts();
        let Some(fatal_fence) = self.shared.fatal_fence.get() else {
            return Err(());
        };
        if self
            .shared
            .database
            .attested_pg_writer_capability()
            .is_some()
        {
            return Err(());
        }
        let mut queue = self
            .shared
            .queue
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        let Some(receipt_count) = queue.outstanding_receipts.checked_add(receipts.len()) else {
            return Err(());
        };
        #[cfg(feature = "test-support")]
        let (max_batches, max_receipts) =
            queue.test_capacity.unwrap_or((MAX_BATCHES, MAX_RECEIPTS));
        #[cfg(not(feature = "test-support"))]
        let (max_batches, max_receipts) = (MAX_BATCHES, MAX_RECEIPTS);
        if receipts.is_empty()
            || queue.closed
            || self.shared.force_stop.is_cancelled()
            || fatal_fence.is_cancelled()
            || queue.outstanding_batches >= max_batches
            || receipt_count > max_receipts
            || queue.worker.as_ref().is_some_and(JoinHandle::is_finished)
        {
            return Err(());
        }
        let Ok(admission) = Arc::clone(&self.shared.admission).try_read_owned() else {
            return Err(());
        };
        if !*admission {
            return Err(());
        }
        let (completion, receiver) = oneshot::channel();
        queue.outstanding_batches += 1;
        queue.outstanding_receipts = receipt_count;
        queue.batches.push_back(Batch {
            message,
            receipts,
            next: 0,
            completion,
            _admission: admission,
        });
        if queue.worker.is_none() {
            queue.worker = Some(tokio::spawn(run(Arc::clone(&self.shared))));
        }
        drop(queue);
        self.shared.notify.notify_one();
        Ok(receiver)
    }

    pub(super) fn close(&self) {
        self.shared
            .queue
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .closed = true;
        self.shared.notify.notify_waiters();
    }

    #[cfg(feature = "test-support")]
    pub(super) fn test_probe(
        &self,
        message: MessageKey,
        lose_ack: bool,
    ) -> Arc<ReceiptCustodyProbe> {
        let probe = Arc::new(ReceiptCustodyProbe {
            reached: Notify::new(),
            release: Notify::new(),
            lose_ack: AtomicBool::new(lose_ack),
            pause_attempts: AtomicBool::new(true),
            pause_effect: AtomicBool::new(false),
            effect_reached: Notify::new(),
            effect_release: Notify::new(),
        });
        self.shared
            .probes
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .insert(message, Arc::clone(&probe));
        probe
    }

    #[cfg(feature = "test-support")]
    pub(super) async fn before_effect(&self, message: MessageKey) {
        let probe = self
            .shared
            .probes
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .get(&message)
            .cloned();
        if let Some(probe) = probe.filter(|probe| probe.pause_effect.swap(false, Ordering::SeqCst))
        {
            probe.effect_reached.notify_one();
            probe.effect_release.notified().await;
        }
    }

    #[cfg(feature = "test-support")]
    pub(super) fn test_capacity(&self, batches: usize, receipts: usize) {
        self.shared
            .queue
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .test_capacity = Some((batches, receipts));
    }

    /// A timed-out caller puts the handle back, so a later drain can join it.
    pub(super) async fn join(&self) -> bool {
        let _joining = self.joining.lock().await;
        let handle = self
            .shared
            .queue
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .worker
            .take();
        let mut taken = TakenWorker {
            shared: &self.shared,
            handle,
        };
        let result = match taken.handle.as_mut() {
            Some(handle) => handle.await.is_ok(),
            None => true,
        };
        taken.handle.take();
        let mut queue = self
            .shared
            .queue
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        if !result {
            queue.incomplete = true;
            queue.batches.clear();
            queue.outstanding_batches = 0;
            queue.outstanding_receipts = 0;
        }
        result && !queue.incomplete
    }
}

struct TakenWorker<'a> {
    shared: &'a Shared,
    handle: Option<JoinHandle<()>>,
}

impl Drop for TakenWorker<'_> {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            self.shared
                .queue
                .lock()
                .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
                .worker = Some(handle);
        }
    }
}

async fn run(shared: Arc<Shared>) {
    let Some(fatal_fence) = shared.fatal_fence.get().cloned() else {
        let mut queue = shared
            .queue
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        queue.incomplete = true;
        queue.batches.clear();
        return;
    };
    let mut workers = tokio::task::JoinSet::new();
    for _ in 0..WORKERS {
        workers.spawn(run_worker(Arc::clone(&shared), fatal_fence.clone()));
    }
    let mut failed = false;
    while let Some(result) = workers.join_next().await {
        if result.is_err() {
            failed = true;
            shared.force_stop.cancel();
        }
    }
    let mut queue = shared
        .queue
        .lock()
        .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
    queue.incomplete |= failed || queue.outstanding_batches > 0;
    queue.batches.clear();
    queue.outstanding_batches = 0;
    queue.outstanding_receipts = 0;
}

async fn run_worker(shared: Arc<Shared>, fatal_fence: CancellationToken) {
    loop {
        let notified = shared.notify.notified();
        tokio::pin!(notified);
        // Register before checking the queue so close or completion cannot
        // leave an idle worker asleep after the final batch is removed.
        notified.as_mut().enable();
        let next = {
            let mut queue = shared
                .queue
                .lock()
                .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
            if shared.force_stop.is_cancelled() || fatal_fence.is_cancelled() {
                queue.incomplete |= queue.outstanding_batches > 0;
                queue.batches.clear();
                return;
            }
            match queue.batches.pop_front() {
                Some(batch) => Some(batch),
                None if queue.closed && queue.outstanding_batches == 0 => return,
                None => None,
            }
        };
        let Some(mut batch) = next else {
            tokio::select! {
                _ = &mut notified => {},
                _ = shared.force_stop.cancelled() => {},
                _ = fatal_fence.cancelled() => {},
            }
            continue;
        };
        #[cfg(feature = "test-support")]
        let probe = shared
            .probes
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .get(&batch.message)
            .cloned();
        #[cfg(feature = "test-support")]
        if let Some(probe) = probe
            .as_ref()
            .filter(|probe| probe.pause_attempts.load(Ordering::SeqCst))
        {
            probe.reached.notify_one();
            probe.release.notified().await;
        }
        if shared.force_stop.is_cancelled() || fatal_fence.is_cancelled() {
            shared
                .queue
                .lock()
                .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
                .batches
                .push_front(batch);
            continue;
        }
        let key = &batch.receipts[batch.next];
        let result = tokio::time::timeout(
            ATTEMPT_BUDGET,
            EffectReceiptRepository::record_receipt_pooled(
                &shared.database,
                batch.message,
                key.kind,
                &key.semantic_identity_hash,
            ),
        )
        .await;
        let succeeded = matches!(result, Ok(Ok(())));
        #[cfg(feature = "test-support")]
        let succeeded = succeeded
            && !probe
                .as_ref()
                .is_some_and(|probe| probe.lose_ack.swap(false, Ordering::SeqCst));
        if succeeded {
            batch.next += 1;
        } else {
            // A COMMIT error or timeout is not evidence of rollback. Retrying
            // the same immutable insert cannot re-execute the effect.
            tracing::debug!("proven ingress receipt remains unresolved; retrying exact insert");
            // The delay belongs to this batch, not the shared queue. Other
            // workers can continue with independent receipt identities.
            tokio::select! {
                _ = shared.force_stop.cancelled() => {},
                _ = fatal_fence.cancelled() => {},
                _ = tokio::time::sleep(RETRY_DELAY) => {},
            }
        }
        {
            let mut queue = shared
                .queue
                .lock()
                .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
            if batch.next == batch.receipts.len() {
                queue.outstanding_batches -= 1;
                queue.outstanding_receipts -= batch.receipts.len();
                let _ = batch.completion.send(());
            } else {
                queue.batches.push_back(batch);
            }
        }
        shared.notify.notify_waiters();
    }
}

/// Test-only barriers around each exact insert and optional lost acknowledgement.
#[cfg(feature = "test-support")]
pub struct ReceiptCustodyProbe {
    reached: Notify,
    release: Notify,
    lose_ack: AtomicBool,
    pause_attempts: AtomicBool,
    pause_effect: AtomicBool,
    effect_reached: Notify,
    effect_release: Notify,
}

#[cfg(feature = "test-support")]
impl ReceiptCustodyProbe {
    pub async fn wait_for_attempt(&self) {
        self.reached.notified().await;
    }

    pub fn release_attempt(&self) {
        self.release.notify_one();
    }

    pub fn stop_pausing_attempts(&self) {
        self.pause_attempts.store(false, Ordering::SeqCst);
    }

    pub fn pause_before_effect(&self) {
        self.pause_effect.store(true, Ordering::SeqCst);
    }

    pub async fn wait_before_effect(&self) {
        self.effect_reached.notified().await;
    }

    pub fn release_effect(&self) {
        self.effect_release.notify_one();
    }
}
