use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::Router;
use tokio::net::TcpListener;

use crate::error::OperatorError;
use crate::metrics::OperatorMetrics;

/// Shared liveness and readiness state exposed over HTTP probes.
#[derive(Debug)]
pub struct HealthState {
    alive: AtomicBool,
    ready: AtomicBool,
    metrics: Arc<OperatorMetrics>,
}

impl HealthState {
    /// Creates initial health state (`alive=true`, `ready=false`).
    #[must_use]
    pub fn new(metrics: Arc<OperatorMetrics>) -> Self {
        Self {
            alive: AtomicBool::new(true),
            ready: AtomicBool::new(false),
            metrics,
        }
    }

    /// Marks the operator ready: either as a standby waiting on another replica's Lease,
    /// or as the leader once its primary reflector has synced.
    pub fn set_ready(&self, ready: bool) {
        self.ready.store(ready, Ordering::Release);
    }

    /// Marks the operator unhealthy on unrecoverable liveness failure.
    pub fn set_alive(&self, alive: bool) {
        self.alive.store(alive, Ordering::Release);
    }

    /// Returns whether the operator is ready.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    /// Returns whether the operator is alive.
    #[must_use]
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }
}

/// Builds the axum probe & metrics router (`/healthz`, `/readyz`, `/metrics`).
pub fn probe_router(state: Arc<HealthState>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics))
        .with_state(state)
}

/// Runs the HTTP health/metrics server until `shutdown` completes.
///
/// # Errors
/// Returns [`OperatorError::HttpServer`] if binding or serving fails.
pub async fn serve_probes(
    bind: SocketAddr,
    state: Arc<HealthState>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), OperatorError> {
    let listener = TcpListener::bind(bind).await?;
    axum::serve(listener, probe_router(state))
        .with_graceful_shutdown(shutdown)
        .await?;
    Ok(())
}

async fn healthz(State(state): State<Arc<HealthState>>) -> (StatusCode, &'static str) {
    if state.is_alive() {
        (StatusCode::OK, "ok\n")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "unhealthy\n")
    }
}

async fn readyz(State(state): State<Arc<HealthState>>) -> (StatusCode, &'static str) {
    if state.is_ready() {
        (StatusCode::OK, "ready\n")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "not ready\n")
    }
}

async fn metrics(State(state): State<Arc<HealthState>>) -> String {
    state.metrics.render_prometheus()
}
