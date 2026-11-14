//! A bounded set of worker threads for the expensive part of a TLS
//! handshake.
//!
//! rustls calls a certificate verifier synchronously, from whichever thread
//! feeds it handshake bytes, and neither an async timeout nor dropping a
//! future can stop a verification that is already running. A connection
//! therefore processes its handshake records on one of these workers
//! instead of on the async runtime:
//!
//! - A job starts only after it holds a permit, so at most
//!   [`VerifierWorkers::limit`] jobs execute at once and no job is ever
//!   queued. A bounded number of callers may wait for a permit; beyond that
//!   a request is refused immediately.
//! - The worker keeps its permit until the job has finished executing. A
//!   connection that times out can drop its [`Completion`] and close its
//!   socket, which discards the late result, but that neither stops the job
//!   nor frees the permit early: an abandoned job still counts against the
//!   limit.
//! - Each job runs on its own short-lived thread and nothing is pooled, so
//!   a finished job leaves no state behind.
//!
//! How long a job can run is bounded by what it does. For a handshake that
//! is the verifier's chain ceilings and webpki's path-building budgets, not
//! a wall-clock limit.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::thread;

use tokio::sync::{oneshot, OwnedSemaphorePermit, Semaphore};

use super::error::{WorkerError, WorkerLost};
use super::limits::{VERIFIER_WAITERS, VERIFIER_WORKERS};

/// The name of every worker thread.
const WORKER_THREAD_NAME: &str = "s2s-tls-verify";

/// Runs handshake jobs off the async runtime, a bounded number at a time.
#[derive(Debug, Clone)]
pub struct VerifierWorkers {
    permits: Arc<Semaphore>,
    waiting: Arc<AtomicUsize>,
    limit: usize,
    max_waiting: usize,
}

impl Default for VerifierWorkers {
    fn default() -> Self {
        Self::new(VERIFIER_WORKERS, VERIFIER_WAITERS)
    }
}

impl VerifierWorkers {
    /// Workers that execute at most `limit` jobs at once and let at most
    /// `max_waiting` callers wait for one. A `limit` of zero is raised to
    /// one.
    pub fn new(limit: usize, max_waiting: usize) -> Self {
        let limit = limit.max(1);
        Self {
            permits: Arc::new(Semaphore::new(limit)),
            waiting: Arc::new(AtomicUsize::new(0)),
            limit,
            max_waiting,
        }
    }

    /// The number of jobs that may execute at once.
    pub fn limit(&self) -> usize {
        self.limit
    }

    /// The number of workers not executing a job right now.
    pub fn available(&self) -> usize {
        self.permits.available_permits()
    }

    /// The number of callers waiting for a worker right now.
    pub fn waiting(&self) -> usize {
        self.waiting.load(Ordering::SeqCst)
    }

    /// Run `job` on a worker if one is free, and refuse otherwise. Nothing
    /// is queued.
    pub fn try_run<T, F>(&self, job: F) -> Result<Completion<T>, WorkerError>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let permit = Arc::clone(&self.permits)
            .try_acquire_owned()
            .map_err(|_| WorkerError::Saturated)?;
        start(permit, job)
    }

    /// Run `job` on a worker, waiting for one to become free.
    ///
    /// The wait has no deadline of its own: the caller bounds it with the
    /// connection's handshake deadline, and dropping this future gives up
    /// the place in line. When [`VerifierWorkers::new`]'s `max_waiting`
    /// callers are already waiting, the request is refused instead.
    pub async fn run<T, F>(&self, job: F) -> Result<Completion<T>, WorkerError>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let _waiter = Waiter::enter(&self.waiting, self.max_waiting)?;
        let permit = Arc::clone(&self.permits)
            .acquire_owned()
            .await
            .map_err(|_| WorkerError::Saturated)?;
        start(permit, job)
    }
}

/// Counts one caller waiting for a worker until it is dropped.
struct Waiter<'a>(&'a AtomicUsize);

impl<'a> Waiter<'a> {
    fn enter(waiting: &'a AtomicUsize, max_waiting: usize) -> Result<Self, WorkerError> {
        waiting
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |waiting| {
                (waiting < max_waiting).then_some(waiting + 1)
            })
            .map(|_| Self(waiting))
            .map_err(|_| WorkerError::Saturated)
    }
}

impl Drop for Waiter<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Start `job` on its own thread. The thread owns `permit` and releases it
/// only when the job has returned or unwound.
fn start<T, F>(permit: OwnedSemaphorePermit, job: F) -> Result<Completion<T>, WorkerError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let (sender, receiver) = oneshot::channel();
    thread::Builder::new()
        .name(WORKER_THREAD_NAME.to_owned())
        .spawn(move || {
            let permit = permit;
            let output = job();
            // The job has finished executing, so its worker is free before
            // anyone can observe the result.
            drop(permit);
            // The receiver is gone when the connection gave up; the late
            // result is discarded.
            drop(sender.send(output));
        })
        .map(|_| Completion { receiver })
        // A thread that could not start drops the closure, and with it the
        // permit and the job.
        .map_err(|_| WorkerError::Spawn)
}

/// The pending result of a job on a verifier worker.
///
/// Dropping it abandons the result; the job still runs to its end and
/// holds its worker until then.
#[derive(Debug)]
pub struct Completion<T> {
    receiver: oneshot::Receiver<T>,
}

impl<T> Future for Completion<T> {
    type Output = Result<T, WorkerLost>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.receiver)
            .poll(context)
            .map(|result| result.map_err(|_| WorkerLost))
    }
}
