//! Bounded continuation of already-proven live executor receipts.
//!
//! This worker only inserts exact receipt identities. It never runs an effect
//! or terminalizes a message, and its admission guard outlives the caller.
#[cfg(feature = "test-support")]
use std::collections::HashMap;
use std::collections::{HashSet, VecDeque};
#[cfg(feature = "test-support")]
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tokio::sync::{oneshot, Notify, OwnedRwLockReadGuard, RwLock};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use waddle_xmpp::ingress::MessageKey;

use super::execute::ProvenReceiptBatch;
use super::EffectReceiptKey;
#[cfg(feature = "test-support")]
use crate::clustering::singleton_lease::GuardBackend;
use crate::clustering::singleton_lease::LeaseGrant;
use crate::db::physical_attestation::{
    CustodiedReceiptAttempt, PinnedReceiptAuthority, PinnedReceiptCommit, ReceiptOperation,
    ReceiptReadback,
};
use crate::ingress_uow::StandaloneReceiptWriter;

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
    pinned: Option<Vec<PinnedReceiptWork>>,
}

struct PinnedReceiptWork {
    operation: ReceiptOperation,
    original: Option<CustodiedReceiptAttempt>,
}

struct PinnedSelection {
    authority: PinnedReceiptAuthority,
    grant: LeaseGrant,
}

#[derive(Default)]
struct Queue {
    closed: bool,
    incomplete: bool,
    batches: VecDeque<Batch>,
    outstanding_batches: usize,
    retained_batches: usize,
    outstanding_receipts: usize,
    worker: Option<JoinHandle<()>>,
    #[cfg(feature = "test-support")]
    test_capacity: Option<(usize, usize)>,
    unresolved: Vec<PinnedReceiptWork>,
    pinned_admitted: HashSet<uuid::Uuid>,
}

struct Shared {
    writer: StandaloneReceiptWriter,
    admission: Arc<RwLock<bool>>,
    force_stop: CancellationToken,
    fatal_fence: OnceLock<CancellationToken>,
    queue: Mutex<Queue>,
    notify: Notify,
    #[cfg(feature = "test-support")]
    probes: Mutex<HashMap<MessageKey, Arc<ReceiptCustodyProbe>>>,
    pinned: OnceLock<PinnedSelection>,
}

pub(super) struct ReceiptCustody {
    shared: Arc<Shared>,
    joining: tokio::sync::Mutex<()>,
}

impl ReceiptCustody {
    pub(super) fn new(
        writer: StandaloneReceiptWriter,
        admission: Arc<RwLock<bool>>,
        force_stop: CancellationToken,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                writer,
                admission,
                force_stop,
                fatal_fence: OnceLock::new(),
                queue: Mutex::new(Queue::default()),
                notify: Notify::new(),
                #[cfg(feature = "test-support")]
                probes: Mutex::new(HashMap::new()),
                pinned: OnceLock::new(),
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
    pub(super) fn select_pinned(
        &self,
        authority: PinnedReceiptAuthority,
        grant: LeaseGrant,
    ) -> bool {
        let queue = self
            .shared
            .queue
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        if queue.closed || queue.worker.is_some() || grant.generation() <= 0 {
            return false;
        }
        self.shared
            .pinned
            .set(PinnedSelection { authority, grant })
            .is_ok()
    }

    pub(super) fn can_retain(&self) -> bool {
        if self.pinned_selected() {
            return true;
        }
        !self.shared.writer.has_attested_writer()
    }

    pub(super) fn pinned_selected(&self) -> bool {
        self.shared.pinned.get().is_some()
    }

    #[cfg(feature = "test-support")]
    pub(super) fn shares_writer(&self, writer: &StandaloneReceiptWriter) -> bool {
        self.shared.writer.same_writer(writer)
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
        if !self.can_retain() {
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
            || queue
                .outstanding_batches
                .saturating_add(queue.retained_batches)
                >= max_batches
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
        // The full set, including stable operation IDs, is owned before the
        // executor can reach its next cancellable await.
        let pinned = self.shared.pinned.get().map(|_| {
            receipts
                .iter()
                .map(|key| PinnedReceiptWork {
                    operation: receipt_operation(message, key),
                    original: None,
                })
                .collect::<Vec<_>>()
        });
        if let Some(work) = &pinned {
            let mut ids = HashSet::new();
            if work.iter().any(|item| {
                !ids.insert(item.operation.operation_id)
                    || queue.pinned_admitted.contains(&item.operation.operation_id)
            }) {
                return Err(());
            }
            queue.pinned_admitted.extend(ids);
        }
        #[cfg(feature = "test-support")]
        if let Some(probe) = self
            .shared
            .probes
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .get(&message)
        {
            if let Some(work) = &pinned {
                *probe
                    .operations
                    .lock()
                    .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison) =
                    work.iter().map(|item| item.operation).collect();
            }
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
            pinned,
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
            operations: Mutex::new(Vec::new()),
            record_attempts: AtomicUsize::new(0),
            readbacks: AtomicUsize::new(0),
            original: Mutex::new(None),
            pause_pinned: AtomicBool::new(false),
            pinned_ready: Notify::new(),
            pinned_resume: Mutex::new(None),
            pinned_backends: Mutex::new(None),
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

    #[cfg(feature = "test-support")]
    pub(super) fn test_unresolved(&self) -> Vec<ReceiptCustodyUnresolved> {
        self.shared
            .queue
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .unresolved
            .iter()
            .map(|work| {
                let observed = work.original.as_ref();
                ReceiptCustodyUnresolved {
                    operation: work.operation,
                    attempted: observed.is_some(),
                    backend_pid: observed.and_then(|attempt| attempt.backend_pid),
                    backend: observed.and_then(|attempt| attempt.backend),
                }
            })
            .collect()
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
            retain_stopped_batches(&mut queue);
            queue.outstanding_batches = 0;
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
        retain_stopped_batches(&mut queue);
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
    retain_stopped_batches(&mut queue);
    queue.outstanding_batches = 0;
}

fn retain_stopped_batches(queue: &mut Queue) {
    while let Some(batch) = queue.batches.pop_front() {
        if let Some(receipts) = batch.pinned {
            for (index, work) in receipts.into_iter().enumerate() {
                // A positive readback in an earlier position still leaves
                // that receipt's original backend fenced independently.
                if index < batch.next && work.original.is_none() {
                    queue.pinned_admitted.remove(&work.operation.operation_id);
                    continue;
                }
                let (backend_pid, backend) = work
                    .original
                    .as_ref()
                    .map(|attempt| (attempt.backend_pid, attempt.backend))
                    .unwrap_or((None, None));
                tracing::warn!(operation_id = %work.operation.operation_id, ?backend_pid, ?backend,
                    "pinned receipt continuation stopped unresolved");
                queue.unresolved.push(work);
            }
        }
    }
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
            tokio::select! {
                _ = probe.release.notified() => {},
                _ = shared.force_stop.cancelled() => {},
                _ = fatal_fence.cancelled() => {},
            }
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
        let succeeded = if let Some(work) = batch.pinned.as_mut() {
            let Some(selection) = shared.pinned.get() else {
                shared.force_stop.cancel();
                shared
                    .queue
                    .lock()
                    .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
                    .batches
                    .push_front(batch);
                continue;
            };
            pinned_attempt(
                selection,
                &mut work[batch.next],
                #[cfg(feature = "test-support")]
                probe.as_deref(),
            )
            .await
        } else {
            let key = &batch.receipts[batch.next];
            let result =
                tokio::time::timeout(ATTEMPT_BUDGET, shared.writer.record(batch.message, key))
                    .await;
            matches!(result, Ok(Ok(())))
        };
        #[cfg(feature = "test-support")]
        let succeeded = succeeded
            && (batch.pinned.is_some()
                || !probe
                    .as_ref()
                    .is_some_and(|probe| probe.lose_ack.swap(false, Ordering::SeqCst)));
        if succeeded {
            batch.next += 1;
        } else {
            // Legacy retries the exact insert. Pinned custody instead retains
            // the first attempt and retries only its exact journal lookup.
            tracing::debug!("proven ingress receipt remains unresolved");
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
                let mut retained = 0;
                if let Some(work) = batch.pinned {
                    for item in work {
                        if item.original.is_some() {
                            // Positive readback establishes the journal row,
                            // not termination of the original backend.
                            queue.unresolved.push(item);
                            retained += 1;
                        } else {
                            queue.pinned_admitted.remove(&item.operation.operation_id);
                        }
                    }
                }
                queue.outstanding_receipts -= batch.receipts.len() - retained;
                if retained > 0 {
                    queue.retained_batches += 1;
                    queue.incomplete = true;
                }
                let _ = batch.completion.send(());
            } else {
                queue.batches.push_back(batch);
            }
        }
        shared.notify.notify_waiters();
    }
}

/// Versioned derivation from the durable effect-intent identity. Reconstructing
/// an ID after restart does not fence an original backend or authorize replay.
pub(crate) fn receipt_operation(message: MessageKey, key: &EffectReceiptKey) -> ReceiptOperation {
    use sha2::{Digest, Sha256};

    let mut digest = Sha256::new();
    digest.update(b"waddle.p21.receipt-operation.v1\0");
    digest.update(message.to_storage().as_bytes());
    digest.update(key.kind.to_storage().to_be_bytes());
    digest.update(key.semantic_identity_hash);
    let hash = digest.finalize();
    let mut id = [0u8; 16];
    id.copy_from_slice(&hash[..16]);
    // RFC 9562 UUIDv8 with the IETF variant; these bits also rule out nil.
    id[6] = (id[6] & 0x0f) | 0x80;
    id[8] = (id[8] & 0x3f) | 0x80;
    ReceiptOperation {
        operation_id: uuid::Uuid::from_bytes(id),
        message,
        kind: key.kind,
        semantic_identity_hash: key.semantic_identity_hash,
    }
}

async fn pinned_attempt(
    selection: &PinnedSelection,
    work: &mut PinnedReceiptWork,
    #[cfg(feature = "test-support")] probe: Option<&ReceiptCustodyProbe>,
) -> bool {
    if work.original.is_some() {
        #[cfg(feature = "test-support")]
        if let Some(probe) = probe {
            probe.readbacks.fetch_add(1, Ordering::SeqCst);
        }
        return matches!(
            selection.authority.readback(&work.operation).await,
            Ok(ReceiptReadback::Committed { .. })
        );
    }
    #[cfg(feature = "test-support")]
    if let Some(probe) = probe {
        probe.record_attempts.fetch_add(1, Ordering::SeqCst);
    }
    #[cfg(feature = "test-support")]
    let mut attempt = if let Some(probe) =
        probe.filter(|probe| probe.pause_pinned.swap(false, Ordering::SeqCst))
    {
        let (ready, observed) = oneshot::channel();
        let (resume, release) = oneshot::channel();
        *probe
            .pinned_resume
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison) = Some(resume);
        let future = selection.authority.record_for_custody(
            &selection.grant,
            &work.operation,
            Some((ready, release)),
        );
        tokio::pin!(future);
        tokio::select! {
            result = &mut future => result,
            pair = observed => {
                if let Ok(pair) = pair {
                    *probe.pinned_backends.lock()
                        .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison) = Some(pair);
                    probe.pinned_ready.notify_one();
                }
                future.await
            }
        }
    } else {
        selection
            .authority
            .record_for_custody(&selection.grant, &work.operation, None)
            .await
    };
    #[cfg(not(feature = "test-support"))]
    let attempt = selection
        .authority
        .record_for_custody(&selection.grant, &work.operation, None)
        .await;
    #[cfg(feature = "test-support")]
    if probe.is_some_and(|probe| probe.lose_ack.swap(false, Ordering::SeqCst))
        && matches!(&attempt.result, Ok(PinnedReceiptCommit::Committed { .. }))
    {
        attempt.result = Ok(PinnedReceiptCommit::Indeterminate {
            operation_id: work.operation.operation_id,
            backend_pid: attempt.backend_pid,
            backend: attempt.backend,
        });
    }
    if matches!(&attempt.result, Ok(PinnedReceiptCommit::Committed { .. })) {
        return true;
    }
    #[cfg(feature = "test-support")]
    if let Some(probe) = probe {
        *probe
            .original
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison) =
            Some(ReceiptCustodyUnresolved {
                operation: work.operation,
                attempted: true,
                backend_pid: attempt.backend_pid,
                backend: attempt.backend,
            });
    }
    // Every other result, including a failure before PID observation, is
    // retained verbatim. Only positive exact readback can complete receipt
    // observation; absence never authorizes another write attempt.
    work.original = Some(attempt);
    false
}

/// Test-only snapshot of an operation whose original backend remains unfenced.
#[cfg(feature = "test-support")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReceiptCustodyUnresolved {
    pub operation: ReceiptOperation,
    pub attempted: bool,
    pub backend_pid: Option<i32>,
    pub backend: Option<GuardBackend>,
}

#[cfg(feature = "test-support")]
/// Test-only barriers around each exact insert and optional lost acknowledgement.
pub struct ReceiptCustodyProbe {
    reached: Notify,
    release: Notify,
    lose_ack: AtomicBool,
    pause_attempts: AtomicBool,
    pause_effect: AtomicBool,
    effect_reached: Notify,
    effect_release: Notify,
    operations: Mutex<Vec<ReceiptOperation>>,
    record_attempts: AtomicUsize,
    readbacks: AtomicUsize,
    original: Mutex<Option<ReceiptCustodyUnresolved>>,
    pause_pinned: AtomicBool,
    pinned_ready: Notify,
    pinned_resume: Mutex<Option<oneshot::Sender<()>>>,
    pinned_backends: Mutex<Option<(GuardBackend, GuardBackend)>>,
}

#[cfg(feature = "test-support")]
impl ReceiptCustodyProbe {
    pub fn operations(&self) -> Vec<ReceiptOperation> {
        self.operations
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .clone()
    }

    pub fn pinned_counts(&self) -> (usize, usize) {
        (
            self.record_attempts.load(Ordering::SeqCst),
            self.readbacks.load(Ordering::SeqCst),
        )
    }

    pub fn original_attempt(&self) -> Option<ReceiptCustodyUnresolved> {
        *self
            .original
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
    }

    pub fn pause_in_pinned_transaction(&self) {
        self.pause_pinned.store(true, Ordering::SeqCst);
    }

    pub async fn wait_in_pinned_transaction(&self) -> Option<(GuardBackend, GuardBackend)> {
        self.pinned_ready.notified().await;
        *self
            .pinned_backends
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
    }

    pub fn release_pinned_transaction(&self) {
        if let Some(resume) = self
            .pinned_resume
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison)
            .take()
        {
            let _ = resume.send(());
        }
    }
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
