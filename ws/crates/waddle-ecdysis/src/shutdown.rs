//! Graceful shutdown coordinator.
//!
//! Provides signal-driven shutdown with connection draining:
//! - `SIGTERM` → stop accepting, drain, exit
//! - `SIGQUIT` → restart (exec new process), stop accepting, drain, exit
//!
//! Uses `CancellationToken` for coordination and `ConnectionGuard` for drain tracking.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::signal::unix::{signal, SignalKind};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

/// Signal indicating why shutdown was triggered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownSignal {
    /// SIGTERM: graceful stop (drain and exit).
    Terminate,
    /// SIGQUIT: graceful restart (exec new process, drain old, exit).
    Restart,
}

/// Tracks active connections for drain coordination.
///
/// The coordinator waits for all `ConnectionGuard`s to drop before
/// considering drain complete.
#[derive(Clone)]
pub struct ConnectionGuard {
    _drop_notifier: Arc<DropNotifier>,
}

struct DropNotifier {
    counter: Arc<AtomicUsize>,
    notify: Arc<tokio::sync::Notify>,
}

impl Drop for DropNotifier {
    fn drop(&mut self) {
        let prev = self.counter.fetch_sub(1, Ordering::SeqCst);
        if prev == 1 {
            self.notify.notify_waiters();
        }
    }
}

/// Coordinator for graceful shutdown with connection draining.
pub struct GracefulShutdown {
    /// Token cancelled when the server should stop accepting new connections.
    stop_accepting: CancellationToken,

    /// Active connection counter.
    connection_count: Arc<AtomicUsize>,

    /// Notified when the last connection drains.
    drain_notify: Arc<tokio::sync::Notify>,

    /// Drain timeout.
    drain_timeout: Duration,
}

impl GracefulShutdown {
    /// Create a new shutdown coordinator.
    pub fn new(drain_timeout: Duration) -> Self {
        Self {
            stop_accepting: CancellationToken::new(),
            connection_count: Arc::new(AtomicUsize::new(0)),
            drain_notify: Arc::new(tokio::sync::Notify::new()),
            drain_timeout,
        }
    }

    /// Create with drain timeout from `WADDLE_DRAIN_TIMEOUT_SECS` env or default (30s).
    pub fn from_env() -> Self {
        let timeout_secs: u64 = std::env::var("WADDLE_DRAIN_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30);

        Self::new(Duration::from_secs(timeout_secs))
    }

    /// Get a `CancellationToken` that fires when the accept loop should stop.
    pub fn stop_token(&self) -> CancellationToken {
        self.stop_accepting.clone()
    }

    /// Create a `ConnectionGuard` for a new connection.
    ///
    /// Increments the counter on creation, decrements on drop.
    pub fn connection_guard(&self) -> ConnectionGuard {
        self.connection_count.fetch_add(1, Ordering::SeqCst);
        ConnectionGuard {
            _drop_notifier: Arc::new(DropNotifier {
                counter: Arc::clone(&self.connection_count),
                notify: Arc::clone(&self.drain_notify),
            }),
        }
    }

    /// Get the current number of active connections.
    pub fn active_connections(&self) -> usize {
        self.connection_count.load(Ordering::SeqCst)
    }

    /// Wait for a shutdown signal (SIGTERM or SIGQUIT).
    pub async fn wait_for_signal(&self) -> ShutdownSignal {
        let mut sigterm =
            signal(SignalKind::terminate()).expect("Failed to register SIGTERM handler");
        let mut sigquit = signal(SignalKind::quit()).expect("Failed to register SIGQUIT handler");

        tokio::select! {
            _ = sigterm.recv() => {
                info!("Received SIGTERM — initiating graceful shutdown");
                ShutdownSignal::Terminate
            }
            _ = sigquit.recv() => {
                info!("Received SIGQUIT — initiating graceful restart");
                ShutdownSignal::Restart
            }
        }
    }

    /// Trigger the stop-accepting phase programmatically (for testing).
    pub fn trigger_stop(&self) {
        self.stop_accepting.cancel();
    }

    /// Run the drain phase: wait for all connections to complete or timeout.
    ///
    /// Returns `true` if all connections drained, `false` if timed out.
    pub async fn drain(&self) -> bool {
        let active = self.active_connections();
        if active == 0 {
            info!("No active connections, drain complete");
            return true;
        }

        info!(
            active_connections = active,
            timeout_secs = self.drain_timeout.as_secs(),
            "Draining active connections"
        );

        tokio::select! {
            _ = self.wait_for_drain() => {
                info!("All connections drained cleanly");
                true
            }
            _ = tokio::time::sleep(self.drain_timeout) => {
                let remaining = self.active_connections();
                warn!(
                    remaining_connections = remaining,
                    timeout_secs = self.drain_timeout.as_secs(),
                    "Drain timeout expired, force-exiting"
                );
                false
            }
        }
    }

    async fn wait_for_drain(&self) {
        loop {
            if self.connection_count.load(Ordering::SeqCst) == 0 {
                return;
            }
            self.drain_notify.notified().await;
        }
    }

    /// Run the full shutdown lifecycle.
    ///
    /// 1. Wait for SIGTERM or SIGQUIT
    /// 2. If SIGQUIT, call `on_restart` (which should call `restart()` and never return)
    /// 3. Cancel the stop token (stops accept loops)
    /// 4. Drain connections or timeout
    /// 5. Return the signal
    pub async fn run<F, Fut>(self, on_restart: F) -> ShutdownSignal
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        let signal = self.wait_for_signal().await;

        if signal == ShutdownSignal::Restart {
            info!("Executing restart callback");
            on_restart().await;
            // If on_restart returns, it means restart() was not called or failed.
            // Fall through to drain — we're shutting down either way.
            error!("Restart callback returned — falling through to drain and exit");
        }

        info!("Stopping accept loops");
        self.stop_accepting.cancel();

        let clean = self.drain().await;
        if !clean {
            error!(
                remaining_connections = self.active_connections(),
                "Force-exiting with remaining connections"
            );
        }

        signal
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_graceful_drain() {
        let shutdown = GracefulShutdown::new(Duration::from_secs(5));
        let stop_token = shutdown.stop_token();

        let guard1 = shutdown.connection_guard();
        let guard2 = shutdown.connection_guard();
        assert_eq!(shutdown.active_connections(), 2);

        shutdown.trigger_stop();
        assert!(stop_token.is_cancelled());

        drop(guard1);
        assert_eq!(shutdown.active_connections(), 1);

        drop(guard2);
        assert_eq!(shutdown.active_connections(), 0);

        let drained = shutdown.drain().await;
        assert!(drained);
    }

    #[tokio::test]
    async fn test_drain_timeout() {
        let shutdown = GracefulShutdown::new(Duration::from_millis(100));

        let _guard = shutdown.connection_guard();
        assert_eq!(shutdown.active_connections(), 1);

        shutdown.trigger_stop();

        let drained = shutdown.drain().await;
        assert!(!drained);
        assert_eq!(shutdown.active_connections(), 1);
    }

    #[tokio::test]
    async fn test_connection_guard_counting() {
        let shutdown = GracefulShutdown::new(Duration::from_secs(1));

        assert_eq!(shutdown.active_connections(), 0);

        let mut guards: Vec<_> = (0..10).map(|_| shutdown.connection_guard()).collect();
        assert_eq!(shutdown.active_connections(), 10);

        guards.truncate(5);
        assert_eq!(shutdown.active_connections(), 5);

        drop(guards);
        assert_eq!(shutdown.active_connections(), 0);
    }

    #[test]
    fn test_from_env_default() {
        std::env::remove_var("WADDLE_DRAIN_TIMEOUT_SECS");
        let shutdown = GracefulShutdown::from_env();
        assert_eq!(shutdown.drain_timeout, Duration::from_secs(30));
    }
}
