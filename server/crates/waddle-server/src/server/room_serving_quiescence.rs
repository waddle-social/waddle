//! Process-wide admission and quiescence fence for room-capable work.
//!
//! The fence deliberately separates admission ([`RoomServingHandle`]) from
//! shutdown authority ([`RoomServingCloser`]). Every admitted operation owns
//! one [`RoomServingScope`]. A scope starts unarmed so callers can acquire
//! before a shutdown check, then decide whether the selected path is actually
//! room-capable. Once armed, it must be completed explicitly. Dropping an
//! armed scope because its future was cancelled, aborted, or panicked marks
//! the eventual release unsafe synchronously and permanently. That ambiguity
//! does not stop the live server: only the explicit shutdown close operation
//! closes admission and cancels producers.
//!
//! A normal Rust return is not automatically a clean completion. Some normal
//! protocol outcomes (for example, `MaybeCommitted`) still carry an ambiguous
//! room-side effect. Callers must classify their outcome before invoking
//! [`RoomServingScope::complete_clean`]. [`RoomServingScope::run_clean`] is
//! only a convenience for audited futures whose every returned output,
//! including typed errors, is fully settled.

use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// Construct the two capabilities for one process-wide room-serving fence.
pub(crate) struct RoomServingQuiescence;

impl RoomServingQuiescence {
    pub(crate) fn create() -> (RoomServingHandle, RoomServingCloser) {
        let inner = Arc::new(Inner {
            state: Mutex::new(State {
                admission_open: true,
                active_scopes: 0,
                unsafe_to_release: false,
                generation: 0,
            }),
            changed: tokio::sync::Notify::new(),
            producer_cancel: CancellationToken::new(),
        });
        (
            RoomServingHandle {
                inner: Arc::clone(&inner),
            },
            RoomServingCloser { inner },
        )
    }
}

#[derive(Debug)]
struct Inner {
    state: Mutex<State>,
    changed: tokio::sync::Notify,
    producer_cancel: CancellationToken,
}

impl Inner {
    /// A poisoned std mutex means a panic crossed a gate critical section.
    /// Recover the data only after making the whole fence fail closed.
    fn lock_state(&self) -> MutexGuard<'_, State> {
        match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                let mut state = poisoned.into_inner();
                state.admission_open = false;
                if !state.unsafe_to_release {
                    state.generation = state.generation.wrapping_add(1);
                }
                state.unsafe_to_release = true;
                self.producer_cancel.cancel();
                self.changed.notify_waiters();
                state
            }
        }
    }

    fn status(&self) -> RoomServingStatus {
        let state = self.lock_state();
        RoomServingStatus {
            admission_open: state.admission_open,
            active_scopes: state.active_scopes,
            unsafe_to_release: state.unsafe_to_release,
        }
    }

    fn clean_fence(self: &Arc<Self>) -> Option<RoomServingFence> {
        let state = self.lock_state();
        if state.admission_open || state.unsafe_to_release || state.active_scopes != 0 {
            return None;
        }
        Some(RoomServingFence {
            inner: Arc::clone(self),
            generation: state.generation,
        })
    }

    fn finish_scope(&self, poison: bool) {
        let mut fatal_close = false;
        {
            let mut state = self.lock_state();
            match state.active_scopes.checked_sub(1) {
                Some(active_scopes) => state.active_scopes = active_scopes,
                None => {
                    // Counter underflow is an internal correctness failure.
                    // Keep the count at zero, close admission, and poison.
                    state.admission_open = false;
                    if !state.unsafe_to_release {
                        state.generation = state.generation.wrapping_add(1);
                    }
                    state.unsafe_to_release = true;
                    fatal_close = true;
                }
            }
            if poison {
                if !state.unsafe_to_release {
                    state.generation = state.generation.wrapping_add(1);
                }
                state.unsafe_to_release = true;
            }
        }
        if fatal_close {
            self.producer_cancel.cancel();
        }
        self.changed.notify_waiters();
    }

    fn poison(&self) {
        {
            let mut state = self.lock_state();
            if !state.unsafe_to_release {
                state.generation = state.generation.wrapping_add(1);
            }
            state.unsafe_to_release = true;
        }
        self.changed.notify_waiters();
    }
}

#[derive(Debug)]
struct State {
    admission_open: bool,
    active_scopes: usize,
    /// Sticky evidence that an admitted operation may have produced an
    /// unaccounted room-side effect. This forbids terminal release but does
    /// not interrupt live traffic.
    unsafe_to_release: bool,
    /// Changes when the gate first closes and whenever a clean generation is
    /// invalidated by poison. A fence is useful only for this exact value and
    /// this exact [`Inner`] allocation.
    generation: u64,
}

/// Cloneable capability used by request and background-work producers.
#[derive(Clone, Debug)]
pub(crate) struct RoomServingHandle {
    inner: Arc<Inner>,
}

impl RoomServingHandle {
    /// Admit one potential room-serving operation.
    ///
    /// Admission and shutdown closure use the same mutex, making their race
    /// linearizable: a scope is either counted before closure or rejected
    /// after it. The returned scope is intentionally unarmed.
    pub(crate) fn try_scope(&self) -> Result<RoomServingScope, RoomServingAdmissionError> {
        let mut state = self.inner.lock_state();
        if !state.admission_open {
            return Err(if state.unsafe_to_release {
                RoomServingAdmissionError::Poisoned
            } else {
                RoomServingAdmissionError::Closed
            });
        }
        let Some(active_scopes) = state.active_scopes.checked_add(1) else {
            state.admission_open = false;
            state.generation = state.generation.wrapping_add(1);
            state.unsafe_to_release = true;
            drop(state);
            self.inner.producer_cancel.cancel();
            self.inner.changed.notify_waiters();
            return Err(RoomServingAdmissionError::Poisoned);
        };
        state.active_scopes = active_scopes;
        drop(state);
        Ok(RoomServingScope {
            inner: Arc::clone(&self.inner),
            state: ScopeState::Unarmed,
        })
    }

    /// Admit and arm a task before giving it to Tokio.
    ///
    /// The task factory receives the armed scope and must move it into the
    /// returned future. It must call [`RoomServingScope::complete_clean`] only
    /// after classifying its final outcome as settled. Returning while the
    /// scope is still armed poisons the fence, as do panic and task abort.
    pub(crate) fn spawn<T, F, Fut>(
        &self,
        task: F,
    ) -> Result<JoinHandle<T>, RoomServingAdmissionError>
    where
        T: Send + 'static,
        F: FnOnce(RoomServingScope) -> Fut,
        Fut: Future<Output = T> + Send + 'static,
    {
        let mut scope = self.try_scope()?;
        scope.arm();
        let future = task(scope);
        Ok(tokio::spawn(future))
    }

    /// Cancellation observed by periodic/background producers.
    ///
    /// Closure cancels this token before the terminal registry barrier, so a
    /// producer can stop admitting new work and unwind its already-counted
    /// scope. The token is shared; callers must treat it as read-only.
    pub(crate) fn producer_cancellation(&self) -> CancellationToken {
        self.inner.producer_cancel.clone()
    }

    #[cfg(test)]
    pub(crate) fn status(&self) -> RoomServingStatus {
        self.inner.status()
    }

    /// Record an ambiguous room-side effect without stopping live admission.
    ///
    /// This cloneable capability is intentionally one-way: request/relay
    /// graphs may make terminal release unsafe, but only the single-owner
    /// closer can stop admission or mint a clean release fence.
    pub(crate) fn mark_unsafe_to_release(&self) {
        self.inner.poison();
    }
}

/// Single-owner shutdown capability. It cannot mint serving scopes.
#[derive(Debug)]
pub(crate) struct RoomServingCloser {
    inner: Arc<Inner>,
}

impl RoomServingCloser {
    /// Cloneable capability that may close admission but cannot wait or mint
    /// the terminal release fence.
    pub(crate) fn close_handle(&self) -> RoomServingCloseHandle {
        RoomServingCloseHandle {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Close admission and signal background producers. Idempotent.
    pub(crate) fn close(&self) -> RoomServingStatus {
        self.close_handle().close()
    }

    /// Mark eventual release unsafe without interrupting the live server.
    pub(crate) fn poison(&self) {
        self.inner.poison();
    }

    /// Wait for a closed fence to reach a clean zero within `budget`.
    ///
    /// An unsafe generation still waits for its counted work to unwind before
    /// returning `Poisoned`; ambiguity forbids release, but does not justify
    /// tearing the runtime out from under active operations.
    /// Calling this before [`Self::close`] is a misuse and never reports a
    /// clean fence.
    pub(crate) async fn wait_for_zero_for(&self, budget: Duration) -> RoomServingWaitOutcome {
        let deadline = tokio::time::Instant::now() + budget;
        loop {
            // Register before reading state. `enable` puts this waiter in
            // Notify's queue, closing the notify-between-read-and-await race.
            let changed = self.inner.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();

            let status = self.inner.status();
            if status.admission_open {
                return RoomServingWaitOutcome::AdmissionStillOpen {
                    active_scopes: status.active_scopes,
                };
            }
            if status.active_scopes == 0 {
                return if status.unsafe_to_release {
                    RoomServingWaitOutcome::Poisoned { active_scopes: 0 }
                } else {
                    RoomServingWaitOutcome::Quiescent
                };
            }

            if tokio::time::timeout_at(deadline, changed).await.is_err() {
                // Crossing the deadline is itself terminal. Even if the last
                // scope completes concurrently, the timeout wins this
                // linearization point and invalidates the generation before
                // any later code could mint a clean release capability.
                let active_scopes = self.inner.status().active_scopes;
                self.poison();
                return RoomServingWaitOutcome::TimedOut { active_scopes };
            }
        }
    }

    /// Consume the shutdown capability and mint the only claim-release proof.
    ///
    /// Callers first wait for root scopes, then complete terminal room
    /// registry preparation, poisoning this closer on ambiguity. Finalization
    /// rechecks the root gate atomically after that preparation and is the
    /// only operation that can construct a fence.
    pub(crate) fn finalize(self) -> RoomServingTerminalOutcome {
        let status = self.inner.status();
        if status.unsafe_to_release {
            return RoomServingTerminalOutcome::Poisoned {
                active_scopes: status.active_scopes,
            };
        }
        if status.admission_open {
            return RoomServingTerminalOutcome::AdmissionStillOpen {
                active_scopes: status.active_scopes,
            };
        }
        if status.active_scopes != 0 {
            return RoomServingTerminalOutcome::Active {
                active_scopes: status.active_scopes,
            };
        }
        match self.inner.clean_fence() {
            Some(fence) => RoomServingTerminalOutcome::Clean(fence),
            None => {
                let status = self.inner.status();
                if status.unsafe_to_release {
                    RoomServingTerminalOutcome::Poisoned {
                        active_scopes: status.active_scopes,
                    }
                } else {
                    RoomServingTerminalOutcome::Active {
                        active_scopes: status.active_scopes,
                    }
                }
            }
        }
    }
}

/// Cloneable stop-path capability. It can close root admission immediately,
/// but cannot wait, poison, finalize, or mint a release fence.
#[derive(Clone, Debug)]
pub(crate) struct RoomServingCloseHandle {
    inner: Arc<Inner>,
}

impl RoomServingCloseHandle {
    pub(crate) fn close(&self) -> RoomServingStatus {
        {
            let mut state = self.inner.lock_state();
            if state.admission_open {
                state.admission_open = false;
                state.generation = state.generation.wrapping_add(1);
            }
        }
        self.inner.producer_cancel.cancel();
        self.inner.changed.notify_waiters();
        self.inner.status()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RoomServingStatus {
    pub(crate) admission_open: bool,
    pub(crate) active_scopes: usize,
    pub(crate) unsafe_to_release: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RoomServingAdmissionError {
    Closed,
    Poisoned,
}

#[derive(Debug)]
pub(crate) enum RoomServingWaitOutcome {
    Quiescent,
    AdmissionStillOpen { active_scopes: usize },
    Poisoned { active_scopes: usize },
    TimedOut { active_scopes: usize },
}

#[derive(Debug)]
pub(crate) enum RoomServingTerminalOutcome {
    Clean(RoomServingFence),
    AdmissionStillOpen { active_scopes: usize },
    Active { active_scopes: usize },
    Poisoned { active_scopes: usize },
}

/// Unforgeable proof that one exact gate generation was closed, safe to
/// release, and had no active scopes.
///
/// The private fields bind the proof to both the gate allocation and its
/// terminal generation. This type is intentionally not `Clone`: the shutdown
/// coordinator moves the sole release capability into the drain API.
#[derive(Debug)]
pub(crate) struct RoomServingFence {
    inner: Arc<Inner>,
    generation: u64,
}

impl RoomServingFence {
    /// Revalidate immediately before a claim-authorizing action.
    ///
    /// A later explicit poison changes the generation and invalidates an
    /// already-issued fence. Admission cannot reopen.
    pub(crate) fn is_current_clean(&self) -> bool {
        let state = self.inner.lock_state();
        !state.admission_open
            && !state.unsafe_to_release
            && state.active_scopes == 0
            && state.generation == self.generation
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScopeState {
    Unarmed,
    Armed,
    Finished,
}

/// One counted room-serving operation.
#[derive(Debug)]
pub(crate) struct RoomServingScope {
    inner: Arc<Inner>,
    state: ScopeState,
}

impl RoomServingScope {
    /// Mark this scope room-capable. Idempotent.
    pub(crate) fn arm(&mut self) {
        if self.state == ScopeState::Unarmed {
            self.state = ScopeState::Armed;
        }
    }

    /// Confirm that the operation and all room-side effects are settled.
    pub(crate) fn complete_clean(mut self) {
        self.inner.finish_scope(false);
        self.state = ScopeState::Finished;
    }

    /// Explicitly fail closed for a normally-returned ambiguous outcome.
    pub(crate) fn poison(mut self) {
        self.inner.finish_scope(true);
        self.state = ScopeState::Finished;
    }

    /// Run a future for which every normal output is known to be settled.
    ///
    /// Cancellation, abort, or panic drops the armed scope and poisons.
    pub(crate) async fn run_clean<F>(mut self, future: F) -> F::Output
    where
        F: Future,
    {
        self.arm();
        let output = future.await;
        self.complete_clean();
        output
    }
}

impl Drop for RoomServingScope {
    fn drop(&mut self) {
        match self.state {
            ScopeState::Unarmed => self.inner.finish_scope(false),
            ScopeState::Armed => self.inner.finish_scope(true),
            ScopeState::Finished => {}
        }
        self.state = ScopeState::Finished;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WAIT: Duration = Duration::from_secs(1);

    #[tokio::test]
    async fn close_rejects_new_admission_and_waits_for_a_clean_scope() {
        let (handle, closer) = RoomServingQuiescence::create();
        let mut scope = handle.try_scope().expect("admitted before close");
        scope.arm();

        assert_eq!(
            closer.close(),
            RoomServingStatus {
                admission_open: false,
                active_scopes: 1,
                unsafe_to_release: false,
            }
        );
        assert_eq!(
            handle.try_scope().expect_err("closed admission"),
            RoomServingAdmissionError::Closed
        );

        scope.complete_clean();
        assert_clean(wait_and_finalize(closer).await);
    }

    #[tokio::test]
    async fn unarmed_drop_is_clean() {
        let (handle, closer) = RoomServingQuiescence::create();
        drop(handle.try_scope().expect("admitted"));
        closer.close();

        assert_clean(wait_and_finalize(closer).await);
    }

    #[tokio::test]
    async fn normal_typed_error_can_be_explicitly_completed_clean() {
        #[derive(Debug, PartialEq, Eq)]
        struct SettledError;

        let (handle, closer) = RoomServingQuiescence::create();
        let scope = handle.try_scope().expect("admitted");
        let outcome: Result<(), SettledError> = scope.run_clean(async { Err(SettledError) }).await;
        assert_eq!(outcome, Err(SettledError));

        closer.close();
        assert_clean(wait_and_finalize(closer).await);
    }

    #[tokio::test]
    async fn normal_ambiguous_return_left_incomplete_poisons() {
        #[derive(Debug, PartialEq, Eq)]
        enum Delivery {
            MaybeCommitted,
        }

        let (handle, closer) = RoomServingQuiescence::create();
        let producer_cancel = handle.producer_cancellation();
        let mut scope = handle.try_scope().expect("admitted");
        scope.arm();
        let outcome = async { Delivery::MaybeCommitted }.await;
        assert_eq!(outcome, Delivery::MaybeCommitted);
        drop(scope);

        assert_eq!(
            handle.status(),
            RoomServingStatus {
                admission_open: true,
                active_scopes: 0,
                unsafe_to_release: true,
            }
        );
        assert!(
            !producer_cancel.is_cancelled(),
            "ambiguity must not stop live producers"
        );
        drop(
            handle
                .try_scope()
                .expect("live admission continues after ambiguity"),
        );
        closer.close();
        assert!(matches!(
            closer.wait_for_zero_for(WAIT).await,
            RoomServingWaitOutcome::Poisoned { active_scopes: 0 }
        ));
        assert_eq!(
            handle
                .try_scope()
                .expect_err("explicit close stops admission"),
            RoomServingAdmissionError::Poisoned
        );
    }

    #[tokio::test]
    async fn aborting_spawned_armed_task_poisons() {
        let (handle, closer) = RoomServingQuiescence::create();
        let task = handle
            .spawn(|scope| async move {
                let _scope = scope;
                std::future::pending::<()>().await;
            })
            .expect("spawn admitted");
        task.abort();
        let error = task.await.expect_err("task was aborted");
        assert!(error.is_cancelled());

        assert!(
            !handle.producer_cancellation().is_cancelled(),
            "task ambiguity must not stop live producers"
        );
        drop(
            handle
                .try_scope()
                .expect("task ambiguity must not close live admission"),
        );
        closer.close();
        assert!(matches!(
            closer.wait_for_zero_for(WAIT).await,
            RoomServingWaitOutcome::Poisoned { active_scopes: 0 }
        ));
    }

    #[tokio::test]
    async fn panic_in_spawned_armed_task_poisons() {
        let (handle, closer) = RoomServingQuiescence::create();
        let task = handle
            .spawn(|scope| async move {
                let _scope = scope;
                panic!("injected task panic");
            })
            .expect("spawn admitted");
        let error = task.await.expect_err("task panicked");
        assert!(error.is_panic());

        assert!(
            !handle.producer_cancellation().is_cancelled(),
            "task ambiguity must not stop live producers"
        );
        drop(
            handle
                .try_scope()
                .expect("task ambiguity must not close live admission"),
        );
        closer.close();
        assert!(matches!(
            closer.wait_for_zero_for(WAIT).await,
            RoomServingWaitOutcome::Poisoned { active_scopes: 0 }
        ));
    }

    #[tokio::test]
    async fn deadline_timeout_is_terminal_even_after_later_completion() {
        let (handle, closer) = RoomServingQuiescence::create();
        let mut scope = handle.try_scope().expect("admitted");
        scope.arm();
        closer.close();

        assert!(matches!(
            closer.wait_for_zero_for(Duration::ZERO).await,
            RoomServingWaitOutcome::TimedOut { active_scopes: 1 }
        ));
        assert!(handle.status().unsafe_to_release);

        scope.complete_clean();
        assert!(handle.status().unsafe_to_release);
        assert_eq!(
            handle
                .try_scope()
                .expect_err("timed-out generation stays poisoned"),
            RoomServingAdmissionError::Poisoned
        );
        assert!(matches!(
            closer.finalize(),
            RoomServingTerminalOutcome::Poisoned { active_scopes: 0 }
        ));
    }

    #[tokio::test]
    async fn wait_before_close_never_reports_a_clean_fence() {
        let (_handle, closer) = RoomServingQuiescence::create();
        assert!(matches!(
            closer.wait_for_zero_for(WAIT).await,
            RoomServingWaitOutcome::AdmissionStillOpen { active_scopes: 0 }
        ));
    }

    #[tokio::test]
    async fn close_cancels_background_producers() {
        let (handle, closer) = RoomServingQuiescence::create();
        let producer_cancel = handle.producer_cancellation();
        assert!(!producer_cancel.is_cancelled());

        closer.close();
        producer_cancel.cancelled().await;
        assert!(producer_cancel.is_cancelled());
    }

    #[tokio::test]
    async fn waiter_does_not_miss_clean_completion_notification() {
        let (handle, closer) = RoomServingQuiescence::create();
        let mut scope = handle.try_scope().expect("admitted");
        scope.arm();
        closer.close();

        let waiter = tokio::spawn(async move {
            let outcome = closer.wait_for_zero_for(WAIT).await;
            (closer, outcome)
        });
        tokio::task::yield_now().await;
        scope.complete_clean();

        let (closer, outcome) = waiter.await.expect("waiter task");
        assert!(matches!(outcome, RoomServingWaitOutcome::Quiescent));
        assert_clean(closer.finalize());
    }

    async fn wait_and_finalize(closer: RoomServingCloser) -> RoomServingTerminalOutcome {
        let outcome = closer.wait_for_zero_for(WAIT).await;
        assert!(matches!(outcome, RoomServingWaitOutcome::Quiescent));
        closer.finalize()
    }

    fn assert_clean(outcome: RoomServingTerminalOutcome) {
        let RoomServingTerminalOutcome::Clean(fence) = outcome else {
            panic!("expected clean fence, got {outcome:?}");
        };
        assert!(fence.is_current_clean());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn close_and_acquire_race_is_linearizable() {
        for _ in 0..256 {
            let (handle, closer) = RoomServingQuiescence::create();
            let start = Arc::new(tokio::sync::Barrier::new(2));
            let acquire = tokio::spawn({
                let handle = handle.clone();
                let start = Arc::clone(&start);
                async move {
                    start.wait().await;
                    handle.try_scope()
                }
            });
            let close = tokio::spawn({
                let start = Arc::clone(&start);
                async move {
                    start.wait().await;
                    let status = closer.close();
                    (closer, status)
                }
            });

            let admitted = acquire.await.expect("acquire task");
            let (closer, closed) = close.await.expect("close task");
            assert!(!closed.admission_open);
            match admitted {
                Ok(scope) => {
                    assert_eq!(closed.active_scopes, 1);
                    drop(scope);
                }
                Err(RoomServingAdmissionError::Closed) => {
                    assert_eq!(closed.active_scopes, 0);
                }
                Err(RoomServingAdmissionError::Poisoned) => {
                    panic!("race must not poison a healthy gate");
                }
            }
            assert_clean(wait_and_finalize(closer).await);
        }
    }

    #[tokio::test]
    async fn spawn_counts_scope_before_child_is_polled() {
        let (handle, closer) = RoomServingQuiescence::create();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let task = handle
            .spawn(|scope| async move {
                let _ = release_rx.await;
                scope.complete_clean();
            })
            .expect("spawn admitted");

        // This observation is synchronous with `spawn` returning; the child
        // does not need to have received a poll for its scope to be counted.
        assert_eq!(handle.status().active_scopes, 1);
        closer.close();
        assert_eq!(
            handle.try_scope().expect_err("closed before child release"),
            RoomServingAdmissionError::Closed
        );

        release_tx.send(()).expect("release child");
        task.await.expect("child task");
        assert_clean(wait_and_finalize(closer).await);
    }

    #[tokio::test]
    async fn later_poison_invalidates_an_issued_clean_fence() {
        let (_handle, closer) = RoomServingQuiescence::create();
        closer.close();
        assert!(matches!(
            closer.wait_for_zero_for(WAIT).await,
            RoomServingWaitOutcome::Quiescent
        ));
        let RoomServingTerminalOutcome::Clean(fence) = closer.finalize() else {
            panic!("expected clean fence");
        };
        assert!(fence.is_current_clean());

        fence.inner.poison();
        assert!(!fence.is_current_clean());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn completion_and_deadline_race_never_mints_an_invalid_fence() {
        for _ in 0..256 {
            let (handle, closer) = RoomServingQuiescence::create();
            let mut scope = handle.try_scope().expect("admitted");
            scope.arm();
            closer.close();
            let start = Arc::new(tokio::sync::Barrier::new(2));
            let waiter = tokio::spawn({
                let start = Arc::clone(&start);
                async move {
                    start.wait().await;
                    let outcome = closer.wait_for_zero_for(Duration::ZERO).await;
                    (closer, outcome)
                }
            });
            let completion = tokio::spawn({
                let start = Arc::clone(&start);
                async move {
                    start.wait().await;
                    scope.complete_clean();
                }
            });

            let (closer, outcome) = waiter.await.expect("waiter");
            completion.await.expect("completion");
            match outcome {
                RoomServingWaitOutcome::Quiescent => {
                    let terminal = closer.finalize();
                    assert_clean(terminal);
                    assert!(!handle.status().unsafe_to_release);
                }
                RoomServingWaitOutcome::TimedOut { .. } => {
                    assert!(handle.status().unsafe_to_release);
                    assert!(matches!(
                        closer.finalize(),
                        RoomServingTerminalOutcome::Poisoned { .. }
                    ));
                }
                other => panic!("unexpected race outcome: {other:?}"),
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn explicit_poison_and_deadline_race_never_mints_a_fence() {
        for _ in 0..256 {
            let (handle, closer) = RoomServingQuiescence::create();
            let mut scope = handle.try_scope().expect("admitted");
            scope.arm();
            closer.close();
            let start = Arc::new(tokio::sync::Barrier::new(2));
            let waiter = tokio::spawn({
                let start = Arc::clone(&start);
                async move {
                    start.wait().await;
                    let outcome = closer.wait_for_zero_for(Duration::ZERO).await;
                    (closer, outcome)
                }
            });
            let poison = tokio::spawn({
                let start = Arc::clone(&start);
                async move {
                    start.wait().await;
                    scope.poison();
                }
            });

            let (closer, outcome) = waiter.await.expect("waiter");
            poison.await.expect("poison");
            assert!(matches!(
                outcome,
                RoomServingWaitOutcome::Poisoned { .. } | RoomServingWaitOutcome::TimedOut { .. }
            ));
            assert!(handle.status().unsafe_to_release);
            assert!(matches!(
                closer.finalize(),
                RoomServingTerminalOutcome::Poisoned { .. }
            ));
        }
    }
}
