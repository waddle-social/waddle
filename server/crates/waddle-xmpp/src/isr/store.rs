//! [`IsrTokenStore`] trait + [`InMemoryIsrTokenStore`] (ADR-0017 Phase 3
//! Slice 8, Q8 resolution).
//!
//! Mirrors the `ClaimStore` split (ADR-0017 Phase 3 Slice 1, Q1): the trait
//! plus the trivial single-node implementation live here, unconditionally
//! compiled; the Postgres-backed, fenced implementation
//! (`PostgresIsrTokenStore`) lives downstream in
//! `waddle-server::clustering::isr`, gated `#[cfg(feature = "clustering")]`.
//!
//! **`InMemoryIsrTokenStore` is never advertised in production** — ADR-0017
//! Phase 3 Slice 8's compounding decision (Q8) gates XEP-0397 advertisement
//! on `clustering.enabled && Postgres`, full stop. This type exists so the
//! trait has a real single-node implementor (matching `InProcessClaimStore`'s
//! role for `ClaimStore`) and so unit tests can exercise the trait contract
//! without a live Postgres instance, not because single-node ISR is a
//! supported deployment shape.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, RwLock};

use async_trait::async_trait;
use subtle::ConstantTimeEq;

use crate::pending_delivery::SmSessionId;
use crate::stream_management::persistence::SmClaimFence;

/// A freshly issued or rotated ISR token (ADR-0017 Phase 3 Slice 8,
/// XEP-0397). The token string itself is a secret credential — never
/// compared with `==`; see [`IsrTokenStore::consume`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedIsrToken {
    pub token: String,
    pub mechanism: String,
}

impl IssuedIsrToken {
    /// Generate a fresh typed issuance before any fallible persistence await.
    /// Keeping the exact value in the caller lets cancellation cleanup revoke
    /// a write whose commit outcome became ambiguous.
    pub fn new(mechanism: impl Into<String>) -> Self {
        Self {
            token: super::generate_isr_token(),
            mechanism: mechanism.into(),
        }
    }
}

/// Outcome of a fenced [`IsrTokenStore::consume`] attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IsrConsumeOutcome {
    /// The presented token matched (compared in constant time). The old
    /// token row was destroyed and a fresh one rotated in, atomically, in
    /// the same fencing transaction (XEP-0397 "Successful Stream
    /// Resumption": the server's success reply MUST include a *new* ISR
    /// token). `rotated` is the value actually committed — never a
    /// pre-commit guess.
    Matched { rotated: IssuedIsrToken },
    /// A token row EXISTED for this SM-ID, but the presented token did not
    /// match the stored one, or the stored token's pinned mechanism did not
    /// match the mechanism the caller is presenting it under. Per
    /// XEP-0397's anti-brute-force MUST, the row was already destroyed
    /// unconditionally before this variant is returned — this is a genuine
    /// wrong-token attempt against a real ISR-enabled session, so the
    /// caller MUST additionally destroy the SM session state the SM-ID
    /// identified (council-adjudicated FIX 3, ADR-0017 Phase 3 Slice 8).
    Mismatched,
    /// No token row existed for this SM-ID at all — either this session
    /// never opted into ISR (no `<isr-enable/>` was ever issued for it), or
    /// a previous attempt already consumed/destroyed it. Nothing was
    /// touched: no row existed to delete (council-adjudicated FIX 1's
    /// phantom-delete guard — a concurrent loser's blocked read observes
    /// exactly this after the winner's commit, per Postgres's
    /// delete-not-update row-lock semantics; see
    /// `PostgresIsrTokenStore::consume`'s doc comment). The caller MUST
    /// return the failure WITHOUT destroying any session state
    /// (council-adjudicated FIX 3): an ISR-authenticate attempt against a
    /// resumable-but-never-ISR-enabled session, or a replay of an
    /// already-consumed attempt, must not destroy anything.
    NoSuchToken,
}

/// Outcome of exact provisional-token revocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsrRevokeOutcome {
    /// The exact provisional issuance was present and removed.
    Revoked,
    /// A different issuance is present. The store retained an exact negative
    /// fence for the provisional value, so a delayed write of that older
    /// issuance cannot overwrite the current token.
    Superseded,
    /// No live row was visible. The store installed an exact negative fence
    /// atomically with this observation, so a delayed matching write is
    /// suppressed and consumes the fence instead of publishing a token.
    Missing,
}

/// [`IsrTokenStore`] failures. Typed per the repo's typed-payloads hard
/// rule — never a bare `String` masquerading as structured data.
#[derive(Debug, thiserror::Error)]
pub enum IsrTokenStoreError {
    /// The backing store's own error, converted to its `Display` text (the
    /// same necessary exception `ClaimError::Backend` documents: a
    /// Postgres-backed implementation's richly-typed error cannot be named
    /// from this unconditionally-compiled crate without an illegal reverse
    /// dependency).
    #[error("ISR token store backend error: {0}")]
    Backend(String),
    /// The caller's claimed fencing epoch no longer holds the SM-session
    /// claim (ADR-0017 Phase 3 Slice 8's locked spec: consume runs inside
    /// the SAME epoch-fenced transaction as the SM claim's own
    /// `SELECT ... FOR SHARE`). The caller lost ownership of the entity
    /// mid-flight; no token row is touched.
    #[error("SM-session claim fencing check failed: caller no longer holds this claim")]
    NotOwner,
    /// In-process bookkeeping lock was poisoned by a panicking holder.
    #[error("ISR token store internal lock poisoned")]
    Poisoned,
}

const DEFAULT_REVOCATION_CAPACITY: usize = 128;
const REVOCATION_ATTEMPT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);
const REVOCATION_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(100);

/// Bounded process-local responsibility for exact provisional ISR-token
/// revocation. Capacity is reserved before persistence starts; cancellation
/// therefore cannot create committed cleanup work that has nowhere to live.
#[derive(Clone)]
pub struct IsrRevocationQueue {
    inner: Arc<IsrRevocationQueueInner>,
}

struct IsrRevocationQueueInner {
    state: Mutex<IsrRevocationQueueState>,
    capacity: usize,
    next_id: std::sync::atomic::AtomicU64,
}

#[derive(Default)]
struct IsrRevocationQueueState {
    entries: VecDeque<IsrRevocationEntry>,
    worker_running: bool,
}

struct IsrRevocationEntry {
    id: u64,
    sm_id: SmSessionId,
    store: Arc<dyn IsrTokenStore>,
    issued: IssuedIsrToken,
    runtime: tokio::runtime::Handle,
    state: IsrRevocationState,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum IsrRevocationState {
    Reserved,
    Active,
    InFlight,
}

/// A pre-persistence capacity reservation. Dropping an armed reservation
/// activates exact cleanup; disarming it after the `<enabled/>` write removes
/// the inventory entry because the token now belongs to the live SM session.
pub struct IsrRevocationReservation {
    queue: IsrRevocationQueue,
    id: u64,
    armed: bool,
}

impl Default for IsrRevocationQueue {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_REVOCATION_CAPACITY)
    }
}

/// Return the bounded revocation inventory associated with this exact store
/// instance. Weak registry entries avoid extending a store's lifetime when it
/// has no reserved or active cleanup work.
pub fn revocation_queue_for_store(store: &Arc<dyn IsrTokenStore>) -> IsrRevocationQueue {
    static QUEUES: std::sync::OnceLock<
        Mutex<HashMap<usize, std::sync::Weak<IsrRevocationQueueInner>>>,
    > = std::sync::OnceLock::new();
    let key = Arc::as_ptr(store) as *const () as usize;
    let queues = QUEUES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut queues = queues
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    queues.retain(|_, queue| queue.strong_count() > 0);
    if let Some(inner) = queues.get(&key).and_then(std::sync::Weak::upgrade) {
        return IsrRevocationQueue { inner };
    }
    let queue = IsrRevocationQueue::default();
    queues.insert(key, Arc::downgrade(&queue.inner));
    queue
}

impl IsrRevocationQueue {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: Arc::new(IsrRevocationQueueInner {
                state: Mutex::new(IsrRevocationQueueState::default()),
                capacity,
                next_id: std::sync::atomic::AtomicU64::new(1),
            }),
        }
    }

    /// Reserve cleanup capacity before persisting `issued`. Returns `None`
    /// under backpressure, allowing the caller to reject enablement before a
    /// token can be committed without retained cleanup responsibility.
    pub fn reserve(
        &self,
        sm_id: SmSessionId,
        store: Arc<dyn IsrTokenStore>,
        issued: IssuedIsrToken,
    ) -> Option<IsrRevocationReservation> {
        let runtime = tokio::runtime::Handle::try_current().ok()?;
        let reservation = {
            let mut state = self.inner.state.lock().ok()?;
            // A reservation can outlive the runtime on which it was created.
            // Any later live caller lends its runtime to retained active work
            // before capacity is checked, so a full queue can still recover.
            for entry in &mut state.entries {
                entry.runtime = runtime.clone();
            }
            if state.entries.len() >= self.inner.capacity {
                None
            } else {
                let id = self
                    .inner
                    .next_id
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                state.entries.push_back(IsrRevocationEntry {
                    id,
                    sm_id,
                    store,
                    issued,
                    runtime,
                    state: IsrRevocationState::Reserved,
                });
                Some(IsrRevocationReservation {
                    queue: self.clone(),
                    id,
                    armed: true,
                })
            }
        };
        self.start_worker();
        reservation
    }

    pub fn pending_len(&self) -> usize {
        self.inner
            .state
            .lock()
            .map(|state| state.entries.len())
            .unwrap_or(0)
    }

    fn remove(&self, id: u64) {
        if let Ok(mut state) = self.inner.state.lock() {
            state.entries.retain(|entry| entry.id != id);
        }
    }

    fn activate(&self, id: u64) {
        if let Ok(mut state) = self.inner.state.lock() {
            if let Some(entry) = state.entries.iter_mut().find(|entry| entry.id == id) {
                entry.state = IsrRevocationState::Active;
            }
        }
        self.start_worker();
    }

    fn start_worker(&self) {
        let runtime = self.inner.state.lock().ok().and_then(|mut state| {
            if state.worker_running
                || !state
                    .entries
                    .iter()
                    .any(|entry| entry.state == IsrRevocationState::Active)
            {
                return None;
            }
            let runtime = state
                .entries
                .iter()
                .find(|entry| entry.state == IsrRevocationState::Active)
                .map(|entry| entry.runtime.clone())?;
            state.worker_running = true;
            Some(runtime)
        });
        let Some(runtime) = runtime else {
            return;
        };
        let queue = self.clone();
        // Construct the guard before task admission. If the captured runtime
        // has already shut down, Tokio drops the never-polled future and this
        // guard resets `worker_running` instead of permanently wedging it.
        let guard = IsrRevocationWorkerGuard {
            queue: self.clone(),
            armed: true,
        };
        runtime.spawn(async move { queue.run_worker(guard).await });
    }

    fn schedule_worker_restart(&self) {
        let Some(runtime) = self.inner.state.lock().ok().and_then(|state| {
            state
                .entries
                .iter()
                .find(|entry| entry.state == IsrRevocationState::Active)
                .map(|entry| entry.runtime.clone())
        }) else {
            return;
        };
        let queue = self.clone();
        runtime.spawn(async move {
            tokio::time::sleep(REVOCATION_RETRY_DELAY).await;
            queue.start_worker();
        });
    }

    async fn run_worker(self, mut guard: IsrRevocationWorkerGuard) {
        loop {
            let work = self.inner.state.lock().ok().and_then(|mut state| {
                let Some(entry) = state
                    .entries
                    .iter_mut()
                    .find(|entry| entry.state == IsrRevocationState::Active)
                else {
                    state.worker_running = false;
                    return None;
                };
                entry.state = IsrRevocationState::InFlight;
                Some((
                    entry.id,
                    entry.sm_id.clone(),
                    Arc::clone(&entry.store),
                    entry.issued.clone(),
                ))
            });
            let Some((id, sm_id, store, issued)) = work else {
                guard.disarm();
                return;
            };
            let completed = matches!(
                tokio::time::timeout(
                    REVOCATION_ATTEMPT_TIMEOUT,
                    store.revoke_if_current(&sm_id, &issued),
                )
                .await,
                Ok(Ok(_))
            );
            if let Ok(mut state) = self.inner.state.lock() {
                let Some(index) = state.entries.iter().position(|entry| entry.id == id) else {
                    continue;
                };
                if completed {
                    state.entries.remove(index);
                } else if let Some(mut entry) = state.entries.remove(index) {
                    entry.state = IsrRevocationState::Active;
                    state.entries.push_back(entry);
                }
            }
            if !completed {
                tokio::time::sleep(REVOCATION_RETRY_DELAY).await;
            }
        }
    }
}

impl IsrRevocationReservation {
    pub fn disarm(mut self) {
        self.queue.remove(self.id);
        self.armed = false;
    }
}

impl Drop for IsrRevocationReservation {
    fn drop(&mut self) {
        if self.armed {
            self.queue.activate(self.id);
        }
    }
}

struct IsrRevocationWorkerGuard {
    queue: IsrRevocationQueue,
    armed: bool,
}

impl IsrRevocationWorkerGuard {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for IsrRevocationWorkerGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let retained_active = if let Ok(mut state) = self.queue.inner.state.lock() {
            state.worker_running = false;
            for entry in &mut state.entries {
                if entry.state == IsrRevocationState::InFlight {
                    entry.state = IsrRevocationState::Active;
                }
            }
            state
                .entries
                .iter()
                .any(|entry| entry.state == IsrRevocationState::Active)
        } else {
            false
        };
        if retained_active {
            self.queue.schedule_worker_restart();
        }
    }
}

/// Postgres-authoritative, epoch-fenced ISR token storage (ADR-0017 Phase 3
/// Slice 8, element 10). See the module doc for the trait/impl split
/// rationale.
///
/// [`consume`](Self::consume) is the locked-spec operation: fetch the
/// token row by the non-secret `sm_id` key (never by token), compare the
/// stored token against `presented_token` in Rust with a constant-time
/// primitive, and only then delete — all inside one epoch-fenced
/// transaction bound to `me`/`mine`'s currently-held SM-session claim.
/// Matching the token in a SQL `WHERE` clause is explicitly banned as a
/// timing oracle; every implementation of this trait must honor that ban.
#[async_trait]
pub trait IsrTokenStore: Send + Sync {
    /// Create the backing schema if it does not exist. Idempotent.
    async fn ensure_schema(&self) -> Result<(), IsrTokenStoreError>;

    /// Persist the exact caller-generated issuance for `sm_id`. The caller
    /// retains this value across the await so cancellation or an ambiguous
    /// backend outcome can be followed by [`Self::revoke_if_current`].
    async fn persist_issued(
        &self,
        sm_id: &SmSessionId,
        issued: &IssuedIsrToken,
    ) -> Result<(), IsrTokenStoreError>;

    /// Mint and store a fresh token for `sm_id`, pinned to `mechanism`
    /// (XEP-0397's "mechanism pinning", element 10: the entities involved
    /// MUST only use or allow this mechanism when performing ISR with the
    /// returned token). Overwrites any existing token for this SM-ID —
    /// issuance is a fresh `<isr-enable/>`, never a rotation of an existing
    /// token; rotation is [`consume`](Self::consume)'s job.
    async fn issue(
        &self,
        sm_id: &SmSessionId,
        mechanism: &str,
    ) -> Result<IssuedIsrToken, IsrTokenStoreError> {
        let issued = IssuedIsrToken::new(mechanism);
        self.persist_issued(sm_id, &issued).await?;
        Ok(issued)
    }

    /// Revoke a provisional issuance only if the row still contains the
    /// exact token and mechanism returned by [`Self::issue`]. A delayed
    /// cleanup must never delete a newer issuance or a token rotated by a
    /// successful resume. Implementations MUST atomically retain an exact
    /// negative fence for `issued` when the live value is missing or
    /// superseded. A later [`Self::persist_issued`] of that exact value MUST
    /// consume the fence without publishing the token. This makes every
    /// successful outcome terminal without mistaking temporary absence for
    /// proof that a timed-out write cannot commit.
    async fn revoke_if_current(
        &self,
        sm_id: &SmSessionId,
        issued: &IssuedIsrToken,
    ) -> Result<IsrRevokeOutcome, IsrTokenStoreError>;

    /// Fenced, single-use, constant-time consume. See the trait-level doc
    /// for the locked spec this must implement exactly.
    async fn consume(
        &self,
        sm_id: &SmSessionId,
        presented_token: &[u8],
        mechanism: &str,
        fence: &SmClaimFence,
    ) -> Result<IsrConsumeOutcome, IsrTokenStoreError>;

    /// Council-adjudicated FIX 4 (ADR-0017 Phase 3 Slice 8): reap token
    /// rows older than `max_age`, together with exact negative revocation
    /// fences older than the same bound. A row is minted per `<isr-enable/>` and
    /// is never reaped by the ordinary [`consume`](Self::consume) path
    /// alone — a token issued but never resumed (or whose SM session is
    /// later expired/reaped by some other path with no cascade hook back
    /// to this store) would otherwise sit in `clustering_isr_tokens`
    /// forever. Returns the number of live token rows deleted; maintenance of
    /// expired negative fences is intentionally not included in that count.
    ///
    /// [`InMemoryIsrTokenStore`]'s implementation is a no-op returning `0`:
    /// per this module's own doc comment it is never advertised/exercised
    /// in production (ISR requires `clustering.enabled && Postgres`), so
    /// nothing ever accumulates there worth sweeping, and it tracks no
    /// issuance/fence timestamp to sweep by in the first place.
    async fn sweep_expired(&self, max_age: std::time::Duration) -> Result<u64, IsrTokenStoreError>;
}

/// Trivial single-node [`IsrTokenStore`]. See the module doc: never
/// advertised in production (ISR requires `clustering.enabled && Postgres`),
/// kept for trait-contract symmetry and unit testing only.
#[derive(Debug, Default)]
pub struct InMemoryIsrTokenStore {
    state: RwLock<InMemoryIsrTokenState>,
}

#[derive(Debug, Default)]
struct InMemoryIsrTokenState {
    tokens: HashMap<SmSessionId, IssuedIsrToken>,
    revocation_fences: HashMap<SmSessionId, Vec<IssuedIsrToken>>,
}

impl InMemoryIsrTokenStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl IsrTokenStore for InMemoryIsrTokenStore {
    async fn ensure_schema(&self) -> Result<(), IsrTokenStoreError> {
        // No backing schema — in-memory only.
        Ok(())
    }

    async fn persist_issued(
        &self,
        sm_id: &SmSessionId,
        issued: &IssuedIsrToken,
    ) -> Result<(), IsrTokenStoreError> {
        let mut state = self
            .state
            .write()
            .map_err(|_| IsrTokenStoreError::Poisoned)?;
        if let Some(fences) = state.revocation_fences.get_mut(sm_id) {
            if let Some(index) = fences.iter().position(|fenced| {
                fenced.mechanism == issued.mechanism
                    && bool::from(fenced.token.as_bytes().ct_eq(issued.token.as_bytes()))
            }) {
                fences.remove(index);
                if fences.is_empty() {
                    state.revocation_fences.remove(sm_id);
                }
                return Ok(());
            }
        }
        state.tokens.insert(sm_id.clone(), issued.clone());
        Ok(())
    }

    async fn revoke_if_current(
        &self,
        sm_id: &SmSessionId,
        issued: &IssuedIsrToken,
    ) -> Result<IsrRevokeOutcome, IsrTokenStoreError> {
        let mut state = self
            .state
            .write()
            .map_err(|_| IsrTokenStoreError::Poisoned)?;
        let matches = state.tokens.get(sm_id).is_some_and(|current| {
            current.mechanism == issued.mechanism
                && bool::from(current.token.as_bytes().ct_eq(issued.token.as_bytes()))
        });
        let outcome = if matches {
            state.tokens.remove(sm_id);
            IsrRevokeOutcome::Revoked
        } else if state.tokens.contains_key(sm_id) {
            IsrRevokeOutcome::Superseded
        } else {
            IsrRevokeOutcome::Missing
        };
        let fences = state.revocation_fences.entry(sm_id.clone()).or_default();
        if !fences.iter().any(|fenced| {
            fenced.mechanism == issued.mechanism
                && bool::from(fenced.token.as_bytes().ct_eq(issued.token.as_bytes()))
        }) {
            fences.push(issued.clone());
        }
        Ok(outcome)
    }

    async fn consume(
        &self,
        sm_id: &SmSessionId,
        presented_token: &[u8],
        mechanism: &str,
        // No node-liveness table exists for the single-node case (mirrors
        // `InProcessClaimStore`'s own module doc): there is only one node,
        // so fencing is a no-op here rather than a meaningful check.
        _fence: &SmClaimFence,
    ) -> Result<IsrConsumeOutcome, IsrTokenStoreError> {
        let mut state = self
            .state
            .write()
            .map_err(|_| IsrTokenStoreError::Poisoned)?;
        // Council-adjudicated FIX 1/FIX 3: distinguish "no row at all"
        // (never opted in / already consumed) from "a row existed but
        // didn't match" (genuine wrong-token attempt) — mirroring
        // `PostgresIsrTokenStore::consume`'s same guard.
        let Some(stored) = state.tokens.remove(sm_id) else {
            return Ok(IsrConsumeOutcome::NoSuchToken);
        };
        // Constant-time comparison (ADR-0017 Phase 3 Slice 8, element 10):
        // never `==` on the secret token bytes. The mechanism pin is
        // non-secret metadata, compared plainly.
        let matches = stored.mechanism == mechanism
            && bool::from(stored.token.as_bytes().ct_eq(presented_token));
        if !matches {
            // Already removed above — destroyed unconditionally, per the
            // XEP's anti-brute-force MUST.
            return Ok(IsrConsumeOutcome::Mismatched);
        }
        let rotated = IssuedIsrToken::new(mechanism);
        state.tokens.insert(sm_id.clone(), rotated.clone());
        Ok(IsrConsumeOutcome::Matched { rotated })
    }

    async fn sweep_expired(
        &self,
        _max_age: std::time::Duration,
    ) -> Result<u64, IsrTokenStoreError> {
        // See the trait method's doc comment: never advertised/exercised in
        // production, and this store tracks no issuance timestamp to sweep
        // by — a no-op, not a partial implementation of a real sweep.
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FailFirstRevokeStore {
        inner: InMemoryIsrTokenStore,
        attempts: std::sync::atomic::AtomicUsize,
        revoked: tokio::sync::Notify,
    }

    struct PanicFirstRevokeStore {
        inner: InMemoryIsrTokenStore,
        attempts: std::sync::atomic::AtomicUsize,
        revoked: tokio::sync::Notify,
    }

    struct DelayedVisibilityStore {
        inner: InMemoryIsrTokenStore,
        attempts: std::sync::atomic::AtomicUsize,
        observed_missing: tokio::sync::Notify,
        revoked: tokio::sync::Notify,
    }

    struct HangFirstRevokeStore {
        inner: InMemoryIsrTokenStore,
        attempts: std::sync::atomic::AtomicUsize,
        first_attempt_entered: std::sync::Barrier,
        revoked: tokio::sync::Notify,
    }

    #[async_trait]
    impl IsrTokenStore for HangFirstRevokeStore {
        async fn ensure_schema(&self) -> Result<(), IsrTokenStoreError> {
            self.inner.ensure_schema().await
        }

        async fn persist_issued(
            &self,
            sm_id: &SmSessionId,
            issued: &IssuedIsrToken,
        ) -> Result<(), IsrTokenStoreError> {
            self.inner.persist_issued(sm_id, issued).await
        }

        async fn revoke_if_current(
            &self,
            sm_id: &SmSessionId,
            issued: &IssuedIsrToken,
        ) -> Result<IsrRevokeOutcome, IsrTokenStoreError> {
            if self
                .attempts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                == 0
            {
                self.first_attempt_entered.wait();
                return std::future::pending().await;
            }
            let revoked = self.inner.revoke_if_current(sm_id, issued).await?;
            self.revoked.notify_one();
            Ok(revoked)
        }

        async fn consume(
            &self,
            sm_id: &SmSessionId,
            presented_token: &[u8],
            mechanism: &str,
            fence: &SmClaimFence,
        ) -> Result<IsrConsumeOutcome, IsrTokenStoreError> {
            self.inner
                .consume(sm_id, presented_token, mechanism, fence)
                .await
        }

        async fn sweep_expired(
            &self,
            max_age: std::time::Duration,
        ) -> Result<u64, IsrTokenStoreError> {
            self.inner.sweep_expired(max_age).await
        }
    }

    #[async_trait]
    impl IsrTokenStore for DelayedVisibilityStore {
        async fn ensure_schema(&self) -> Result<(), IsrTokenStoreError> {
            self.inner.ensure_schema().await
        }

        async fn persist_issued(
            &self,
            sm_id: &SmSessionId,
            issued: &IssuedIsrToken,
        ) -> Result<(), IsrTokenStoreError> {
            self.inner.persist_issued(sm_id, issued).await
        }

        async fn revoke_if_current(
            &self,
            sm_id: &SmSessionId,
            issued: &IssuedIsrToken,
        ) -> Result<IsrRevokeOutcome, IsrTokenStoreError> {
            self.attempts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let outcome = self.inner.revoke_if_current(sm_id, issued).await?;
            match outcome {
                IsrRevokeOutcome::Missing => self.observed_missing.notify_one(),
                IsrRevokeOutcome::Revoked => self.revoked.notify_one(),
                IsrRevokeOutcome::Superseded => {}
            }
            Ok(outcome)
        }

        async fn consume(
            &self,
            sm_id: &SmSessionId,
            presented_token: &[u8],
            mechanism: &str,
            fence: &SmClaimFence,
        ) -> Result<IsrConsumeOutcome, IsrTokenStoreError> {
            self.inner
                .consume(sm_id, presented_token, mechanism, fence)
                .await
        }

        async fn sweep_expired(
            &self,
            max_age: std::time::Duration,
        ) -> Result<u64, IsrTokenStoreError> {
            self.inner.sweep_expired(max_age).await
        }
    }

    #[async_trait]
    impl IsrTokenStore for PanicFirstRevokeStore {
        async fn ensure_schema(&self) -> Result<(), IsrTokenStoreError> {
            self.inner.ensure_schema().await
        }

        async fn persist_issued(
            &self,
            sm_id: &SmSessionId,
            issued: &IssuedIsrToken,
        ) -> Result<(), IsrTokenStoreError> {
            self.inner.persist_issued(sm_id, issued).await
        }

        async fn revoke_if_current(
            &self,
            sm_id: &SmSessionId,
            issued: &IssuedIsrToken,
        ) -> Result<IsrRevokeOutcome, IsrTokenStoreError> {
            if self
                .attempts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                == 0
            {
                panic!("injected first revoke panic");
            }
            let revoked = self.inner.revoke_if_current(sm_id, issued).await?;
            self.revoked.notify_one();
            Ok(revoked)
        }

        async fn consume(
            &self,
            sm_id: &SmSessionId,
            presented_token: &[u8],
            mechanism: &str,
            fence: &SmClaimFence,
        ) -> Result<IsrConsumeOutcome, IsrTokenStoreError> {
            self.inner
                .consume(sm_id, presented_token, mechanism, fence)
                .await
        }

        async fn sweep_expired(
            &self,
            max_age: std::time::Duration,
        ) -> Result<u64, IsrTokenStoreError> {
            self.inner.sweep_expired(max_age).await
        }
    }

    #[async_trait]
    impl IsrTokenStore for FailFirstRevokeStore {
        async fn ensure_schema(&self) -> Result<(), IsrTokenStoreError> {
            self.inner.ensure_schema().await
        }

        async fn persist_issued(
            &self,
            sm_id: &SmSessionId,
            issued: &IssuedIsrToken,
        ) -> Result<(), IsrTokenStoreError> {
            self.inner.persist_issued(sm_id, issued).await
        }

        async fn revoke_if_current(
            &self,
            sm_id: &SmSessionId,
            issued: &IssuedIsrToken,
        ) -> Result<IsrRevokeOutcome, IsrTokenStoreError> {
            if self
                .attempts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                == 0
            {
                return Err(IsrTokenStoreError::Backend(
                    "injected first revoke failure".to_string(),
                ));
            }
            let revoked = self.inner.revoke_if_current(sm_id, issued).await?;
            self.revoked.notify_one();
            Ok(revoked)
        }

        async fn consume(
            &self,
            sm_id: &SmSessionId,
            presented_token: &[u8],
            mechanism: &str,
            fence: &SmClaimFence,
        ) -> Result<IsrConsumeOutcome, IsrTokenStoreError> {
            self.inner
                .consume(sm_id, presented_token, mechanism, fence)
                .await
        }

        async fn sweep_expired(
            &self,
            max_age: std::time::Duration,
        ) -> Result<u64, IsrTokenStoreError> {
            self.inner.sweep_expired(max_age).await
        }
    }

    fn fence() -> SmClaimFence {
        SmClaimFence::new(
            crate::ownership::NodeIdentity::local(),
            crate::ownership::ClaimEpoch(0),
        )
    }

    fn sid(value: &str) -> SmSessionId {
        SmSessionId::new(value)
    }

    #[tokio::test]
    async fn issue_then_consume_with_matching_token_rotates() {
        let store = InMemoryIsrTokenStore::new();
        let stream_id = sid("sm-1");
        let issued = store.issue(&stream_id, "PLAIN").await.expect("issue");
        let outcome = store
            .consume(&stream_id, issued.token.as_bytes(), "PLAIN", &fence())
            .await
            .expect("consume");
        let IsrConsumeOutcome::Matched { rotated } = outcome else {
            panic!("expected Matched, got {outcome:?}");
        };
        assert_ne!(rotated.token, issued.token);
    }

    #[tokio::test]
    async fn consume_with_wrong_token_is_mismatched_and_destroys_the_row() {
        let store = InMemoryIsrTokenStore::new();
        let stream_id = sid("sm-1");
        let issued = store.issue(&stream_id, "PLAIN").await.expect("issue");
        let outcome = store
            .consume(&stream_id, b"not-the-token", "PLAIN", &fence())
            .await
            .expect("consume");
        assert_eq!(outcome, IsrConsumeOutcome::Mismatched);

        // The row is gone — a second consume attempt with the ORIGINAL
        // (correct) token now finds no row at all (FIX 3: distinct from
        // the genuine-mismatch outcome above), proving destruction happened.
        let second = store
            .consume(&stream_id, issued.token.as_bytes(), "PLAIN", &fence())
            .await
            .expect("consume");
        assert_eq!(second, IsrConsumeOutcome::NoSuchToken);
    }

    #[tokio::test]
    async fn consume_is_single_use() {
        let store = InMemoryIsrTokenStore::new();
        let stream_id = sid("sm-1");
        let issued = store.issue(&stream_id, "PLAIN").await.expect("issue");
        let first = store
            .consume(&stream_id, issued.token.as_bytes(), "PLAIN", &fence())
            .await
            .expect("consume");
        assert!(matches!(first, IsrConsumeOutcome::Matched { .. }));

        // The OLD token no longer works, even though a rotated token now
        // exists under the same sm_id — a genuine mismatch (a row DOES
        // exist), not `NoSuchToken`.
        let replay = store
            .consume(&stream_id, issued.token.as_bytes(), "PLAIN", &fence())
            .await
            .expect("consume");
        assert_eq!(replay, IsrConsumeOutcome::Mismatched);
    }

    #[tokio::test]
    async fn consume_with_no_issued_token_is_no_such_token() {
        let store = InMemoryIsrTokenStore::new();
        let outcome = store
            .consume(&sid("no-such-sm-id"), b"anything", "PLAIN", &fence())
            .await
            .expect("consume");
        assert_eq!(outcome, IsrConsumeOutcome::NoSuchToken);
    }

    #[tokio::test]
    async fn consume_pins_the_mechanism() {
        let store = InMemoryIsrTokenStore::new();
        let stream_id = sid("sm-1");
        let issued = store.issue(&stream_id, "PLAIN").await.expect("issue");
        let outcome = store
            .consume(
                &stream_id,
                issued.token.as_bytes(),
                "SCRAM-SHA-256",
                &fence(),
            )
            .await
            .expect("consume");
        assert_eq!(outcome, IsrConsumeOutcome::Mismatched);
    }

    #[tokio::test]
    async fn provisional_revoke_is_exact_and_cannot_delete_a_newer_issue() {
        let store = InMemoryIsrTokenStore::new();
        let stream_id = sid("sm-1");
        let old = store.issue(&stream_id, "PLAIN").await.expect("old issue");
        let current = store.issue(&stream_id, "PLAIN").await.expect("new issue");

        assert_eq!(
            store
                .revoke_if_current(&stream_id, &old)
                .await
                .expect("stale revoke"),
            IsrRevokeOutcome::Superseded
        );
        store
            .persist_issued(&stream_id, &old)
            .await
            .expect("late old persistence is suppressed by its exact fence");
        assert_eq!(
            store
                .revoke_if_current(&stream_id, &current)
                .await
                .expect("exact revoke"),
            IsrRevokeOutcome::Revoked
        );
        assert_eq!(
            store
                .consume(&stream_id, current.token.as_bytes(), "PLAIN", &fence(),)
                .await
                .expect("lookup after revoke"),
            IsrConsumeOutcome::NoSuchToken
        );
    }

    #[tokio::test]
    async fn revocation_queue_fences_missing_before_a_late_persist_becomes_visible() {
        let store = Arc::new(DelayedVisibilityStore {
            inner: InMemoryIsrTokenStore::new(),
            attempts: std::sync::atomic::AtomicUsize::new(0),
            observed_missing: tokio::sync::Notify::new(),
            revoked: tokio::sync::Notify::new(),
        });
        let stream_id = sid("sm-late-persist");
        let issued = IssuedIsrToken::new("PLAIN");
        let queue = IsrRevocationQueue::with_capacity(1);
        let dyn_store: Arc<dyn IsrTokenStore> = store.clone();
        let reservation = queue
            .reserve(stream_id.clone(), dyn_store.clone(), issued.clone())
            .expect("reserve cleanup capacity");
        let observed_missing = store.observed_missing.notified();

        // Model a timed-out persistence future whose database write is not
        // visible when cancellation cleanup first runs.
        drop(reservation);
        tokio::time::timeout(std::time::Duration::from_secs(1), observed_missing)
            .await
            .expect("cleanup first observes a missing row");
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while queue.pending_len() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("negative fence makes Missing terminal without leaking capacity");

        store
            .persist_issued(&stream_id, &issued)
            .await
            .expect("late persistence reaches the negative fence");
        assert_eq!(
            store
                .consume(&stream_id, issued.token.as_bytes(), "PLAIN", &fence())
                .await
                .expect("lookup after suppressed late persistence"),
            IsrConsumeOutcome::NoSuchToken
        );

        assert_eq!(store.attempts.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(queue.pending_len(), 0);

        let next = queue
            .reserve(
                sid("sm-after-definite-failure"),
                dyn_store,
                IssuedIsrToken::new("PLAIN"),
            )
            .expect("definite missing persistence releases bounded capacity");
        next.disarm();
    }

    #[tokio::test]
    async fn revocation_queue_retries_an_exact_issuance_after_transient_failure() {
        let store = Arc::new(FailFirstRevokeStore {
            inner: InMemoryIsrTokenStore::new(),
            attempts: std::sync::atomic::AtomicUsize::new(0),
            revoked: tokio::sync::Notify::new(),
        });
        let issued = IssuedIsrToken::new("PLAIN");
        let stream_id = sid("sm-retry");
        store
            .persist_issued(&stream_id, &issued)
            .await
            .expect("persist provisional token");
        let queue = IsrRevocationQueue::with_capacity(1);
        let dyn_store: Arc<dyn IsrTokenStore> = store.clone();
        let reservation = queue
            .reserve(stream_id, dyn_store, issued)
            .expect("reserve cleanup capacity");
        let revoked = store.revoked.notified();

        drop(reservation);
        tokio::time::timeout(std::time::Duration::from_secs(1), revoked)
            .await
            .expect("second revoke succeeds after first failure");
        // `revoked` is notified inside the store call; the worker frees
        // the queue slot only after that call returns, so wait for it.
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while queue.pending_len() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("retrying worker releases queue capacity");

        assert_eq!(store.attempts.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert_eq!(queue.pending_len(), 0);
    }

    #[tokio::test]
    async fn revocation_queue_restarts_after_worker_panic() {
        let store = Arc::new(PanicFirstRevokeStore {
            inner: InMemoryIsrTokenStore::new(),
            attempts: std::sync::atomic::AtomicUsize::new(0),
            revoked: tokio::sync::Notify::new(),
        });
        let issued = IssuedIsrToken::new("PLAIN");
        let stream_id = sid("sm-panic");
        store
            .persist_issued(&stream_id, &issued)
            .await
            .expect("persist provisional token");
        let queue = IsrRevocationQueue::with_capacity(1);
        let dyn_store: Arc<dyn IsrTokenStore> = store.clone();
        let reservation = queue
            .reserve(stream_id, dyn_store, issued)
            .expect("reserve cleanup capacity");
        let revoked = store.revoked.notified();

        drop(reservation);
        tokio::time::timeout(std::time::Duration::from_secs(1), revoked)
            .await
            .expect("replacement worker revokes after predecessor panic");
        // `revoked` is notified inside the store call; the worker frees
        // the queue slot only after that call returns, so wait for it.
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while queue.pending_len() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("replacement worker releases queue capacity");

        assert_eq!(store.attempts.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert_eq!(queue.pending_len(), 0);
    }

    #[tokio::test]
    async fn revocation_queue_reserves_capacity_before_persistence() {
        let queue = IsrRevocationQueue::with_capacity(1);
        let store: Arc<dyn IsrTokenStore> = Arc::new(InMemoryIsrTokenStore::new());
        let reservation = queue
            .reserve(sid("sm-one"), store.clone(), IssuedIsrToken::new("PLAIN"))
            .expect("first reservation");

        assert!(queue
            .reserve(sid("sm-two"), store, IssuedIsrToken::new("PLAIN"),)
            .is_none());
        reservation.disarm();
        assert_eq!(queue.pending_len(), 0);
    }

    #[test]
    fn revocation_queue_uses_captured_runtime_when_reservation_drops_outside_it() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("build runtime");
        let store = Arc::new(FailFirstRevokeStore {
            inner: InMemoryIsrTokenStore::new(),
            attempts: std::sync::atomic::AtomicUsize::new(0),
            revoked: tokio::sync::Notify::new(),
        });
        let queue = IsrRevocationQueue::with_capacity(1);
        let reservation = runtime.block_on(async {
            let issued = IssuedIsrToken::new("PLAIN");
            let stream_id = sid("sm-outside-runtime-drop");
            store
                .persist_issued(&stream_id, &issued)
                .await
                .expect("persist provisional token");
            let dyn_store: Arc<dyn IsrTokenStore> = store.clone();
            queue
                .reserve(stream_id, dyn_store, issued)
                .expect("reserve cleanup capacity")
        });

        // There is deliberately no entered Tokio context on this thread.
        // The reservation must reuse the handle captured by `reserve`.
        drop(reservation);

        runtime.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(1), store.revoked.notified())
                .await
                .expect("captured runtime completes exact revocation");
            // `revoked` is notified inside the store call; the worker frees
            // the queue slot only after that call returns, so wait for it.
            tokio::time::timeout(std::time::Duration::from_secs(1), async {
                while queue.pending_len() != 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("revocation worker releases queue capacity");
        });
        assert_eq!(store.attempts.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert_eq!(queue.pending_len(), 0);
    }

    #[test]
    fn revocation_queue_recovers_when_the_captured_runtime_has_shut_down() {
        let runtime_a = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build first runtime");
        let store = Arc::new(FailFirstRevokeStore {
            inner: InMemoryIsrTokenStore::new(),
            attempts: std::sync::atomic::AtomicUsize::new(0),
            revoked: tokio::sync::Notify::new(),
        });
        let queue = IsrRevocationQueue::with_capacity(1);
        let reservation = runtime_a.block_on(async {
            let issued = IssuedIsrToken::new("PLAIN");
            let stream_id = sid("sm-dead-runtime");
            store
                .persist_issued(&stream_id, &issued)
                .await
                .expect("persist provisional token");
            let dyn_store: Arc<dyn IsrTokenStore> = store.clone();
            queue
                .reserve(stream_id, dyn_store, issued)
                .expect("reserve cleanup capacity")
        });
        drop(runtime_a);

        // Activating outside any runtime first tries the now-dead captured
        // handle. Its never-polled worker must not leave `worker_running` set.
        drop(reservation);
        assert_eq!(queue.pending_len(), 1);

        let runtime_b = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build recovery runtime");
        runtime_b.block_on(async {
            let dyn_store: Arc<dyn IsrTokenStore> = store.clone();
            assert!(queue
                .reserve(
                    sid("sm-capacity-probe"),
                    dyn_store,
                    IssuedIsrToken::new("PLAIN"),
                )
                .is_none());
            tokio::time::timeout(std::time::Duration::from_secs(1), store.revoked.notified())
                .await
                .expect("later live runtime recovers retained cleanup");
            // `revoked` is notified inside the store call; the worker frees
            // the queue slot only after that call returns, so wait for it.
            tokio::time::timeout(std::time::Duration::from_secs(1), async {
                while queue.pending_len() != 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("recovered worker releases queue capacity");

            let dyn_store: Arc<dyn IsrTokenStore> = store.clone();
            queue
                .reserve(
                    sid("sm-after-recovery"),
                    dyn_store,
                    IssuedIsrToken::new("PLAIN"),
                )
                .expect("recovered queue releases capacity")
                .disarm();
        });
    }

    #[test]
    fn revocation_queue_lends_a_live_runtime_to_in_flight_cleanup() {
        let runtime_a = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("build first runtime");
        let runtime_b = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build recovery runtime");
        let store = Arc::new(HangFirstRevokeStore {
            inner: InMemoryIsrTokenStore::new(),
            attempts: std::sync::atomic::AtomicUsize::new(0),
            first_attempt_entered: std::sync::Barrier::new(2),
            revoked: tokio::sync::Notify::new(),
        });
        let queue = IsrRevocationQueue::with_capacity(1);
        let reservation = runtime_a.block_on(async {
            let issued = IssuedIsrToken::new("PLAIN");
            let stream_id = sid("sm-in-flight-runtime-loss");
            store
                .persist_issued(&stream_id, &issued)
                .await
                .expect("persist provisional token");
            let dyn_store: Arc<dyn IsrTokenStore> = store.clone();
            queue
                .reserve(stream_id, dyn_store, issued)
                .expect("reserve cleanup capacity")
        });
        drop(reservation);
        store.first_attempt_entered.wait();

        runtime_b.block_on(async {
            let dyn_store: Arc<dyn IsrTokenStore> = store.clone();
            assert!(queue
                .reserve(
                    sid("sm-in-flight-capacity-probe"),
                    dyn_store,
                    IssuedIsrToken::new("PLAIN"),
                )
                .is_none());
        });
        runtime_a.shutdown_timeout(std::time::Duration::from_secs(1));

        runtime_b.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(1), store.revoked.notified())
                .await
                .expect("in-flight cleanup restarts on the lent live runtime");
            // `revoked` is notified inside the store call; the worker frees
            // the queue slot only after that call returns, so wait for it.
            tokio::time::timeout(std::time::Duration::from_secs(1), async {
                while queue.pending_len() != 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("restarted worker releases queue capacity");
        });
        assert_eq!(store.attempts.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert_eq!(queue.pending_len(), 0);
    }
}
