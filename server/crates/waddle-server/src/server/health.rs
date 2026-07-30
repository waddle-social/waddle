use crate::db::PoolHealth;
use crate::server::AppState;
use axum::{
    extract::State,
    http::{header, HeaderName, Method, StatusCode},
    response::{IntoResponse, Json},
};
use serde::Serialize;
use serde_json::json;
use std::sync::Arc;
use tower_http::cors::CorsLayer;
use tracing::{info, warn};

/// Configure CORS layer.
///
/// If `WADDLE_CORS_ORIGINS` is set (comma-separated list of origins),
/// only those origins are allowed. Otherwise, falls back to permissive
/// CORS (suitable for development).
pub(crate) fn configure_cors() -> CorsLayer {
    let origins = std::env::var("WADDLE_CORS_ORIGINS").ok();
    build_cors(origins.as_deref())
}

pub(crate) fn build_cors(origins: Option<&str>) -> CorsLayer {
    use tower_http::cors::AllowOrigin;

    match origins {
        Some(origins) if !origins.is_empty() => {
            let allowed: Vec<_> = origins
                .split(',')
                .filter_map(|o| o.trim().parse().ok())
                .collect();
            if allowed.is_empty() {
                warn!(
                    "WADDLE_CORS_ORIGINS set but no valid origins parsed, falling back to permissive CORS"
                );
                CorsLayer::permissive()
            } else {
                info!(origins = ?allowed, "Configured CORS with explicit allowed origins");
                CorsLayer::new()
                    .allow_origin(AllowOrigin::list(allowed))
                    .allow_methods([
                        Method::GET,
                        Method::POST,
                        Method::PUT,
                        Method::PATCH,
                        Method::DELETE,
                        Method::OPTIONS,
                    ])
                    .allow_headers([
                        header::ACCEPT,
                        header::AUTHORIZATION,
                        header::CONTENT_TYPE,
                        header::ORIGIN,
                        // W3C Trace Context — browsers won't send these
                        // cross-origin unless explicitly allowed, which
                        // would silently break end-to-end traces from
                        // the chat frontend into the server.
                        HeaderName::from_static("traceparent"),
                        HeaderName::from_static("tracestate"),
                        HeaderName::from_static("baggage"),
                        HeaderName::from_static("x-waddle-session-id"),
                    ])
                    .allow_credentials(true)
            }
        }
        _ => CorsLayer::permissive(),
    }
}

/// Response for detailed health check
#[derive(Debug, Serialize)]
struct DetailedHealthResponse {
    status: String,
    service: String,
    version: String,
    license: String,
    database: DatabaseHealthStatus,
}

#[derive(Debug, Serialize)]
struct DatabaseHealthStatus {
    status: String,
    global_healthy: bool,
    waddle_dbs_healthy: bool,
    loaded_waddle_count: usize,
}

impl From<PoolHealth> for DatabaseHealthStatus {
    fn from(health: PoolHealth) -> Self {
        Self {
            status: if health.is_healthy() {
                "healthy"
            } else {
                "unhealthy"
            }
            .to_string(),
            global_healthy: health.global_healthy,
            waddle_dbs_healthy: health.waddle_dbs_healthy,
            loaded_waddle_count: health.loaded_waddle_count,
        }
    }
}

/// Simple health check endpoint (for load balancers)
pub(crate) async fn health_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    // Quick health check - just verify the global DB is accessible
    match state.db_pool.global().health_check().await {
        Ok(true) => (
            StatusCode::OK,
            Json(json!({
                "status": "healthy",
                "service": "waddle-server",
                "version": env!("CARGO_PKG_VERSION"),
                "license": "AGPL-3.0"
            })),
        ),
        Ok(false) => {
            warn!("Health check: database unhealthy");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "status": "unhealthy",
                    "service": "waddle-server",
                    "version": env!("CARGO_PKG_VERSION"),
                    "error": "database unhealthy"
                })),
            )
        }
        Err(e) => {
            warn!("Health check failed: {}", e);
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "status": "unhealthy",
                    "service": "waddle-server",
                    "version": env!("CARGO_PKG_VERSION"),
                    "error": format!("database error: {}", e)
                })),
            )
        }
    }
}

async fn health_for_serving_generation<T>(
    lifecycle: &crate::clustering::NodeLifecycle,
    health_check: impl std::future::Future<Output = T>,
) -> Result<T, crate::clustering::NodeAdmissionError> {
    let permit = lifecycle.admit()?;
    let health = health_check.await;
    permit.revalidate()?;
    Ok(health)
}

fn lifecycle_not_ready_response(
    lifecycle: &crate::clustering::NodeLifecycle,
    error: &crate::clustering::NodeAdmissionError,
) -> (StatusCode, Json<serde_json::Value>) {
    let admission = lifecycle.admission();
    warn!(?admission, %error, "Readiness check: node is not admitting clients");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({
            "status": "not_ready",
            "service": "waddle-server",
            "version": env!("CARGO_PKG_VERSION"),
            "admission": format!("{admission:?}")
        })),
    )
}

/// Readiness check endpoint (for orchestrators).
///
/// Readiness is stricter than liveness and validates overall DB pool health
/// **and** the ADR-0017 Phase 3 Slice 2 clustering readiness signal: a node
/// that has self-fenced its entity-ownership claims (node-lease heartbeat
/// lost, or Postgres unreachable past the lease deadline) must not stay in
/// the client Service/Ingress endpoint set — clients whose sockets it just
/// closed would otherwise be routed straight back to the still-refusing
/// node. The same lifecycle also fails closed when a critical local actor
/// terminates in a single-node deployment.
pub(crate) async fn readiness_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match health_for_serving_generation(&state.node_lifecycle, state.db_pool.health_check()).await {
        Err(error) => lifecycle_not_ready_response(&state.node_lifecycle, &error),
        Ok(Ok(health)) if health.is_healthy() => (
            StatusCode::OK,
            Json(json!({
                "status": "ready",
                "service": "waddle-server",
                "version": env!("CARGO_PKG_VERSION"),
                "database": "ready"
            })),
        ),
        Ok(Ok(health)) => {
            warn!(
                global_healthy = health.global_healthy,
                waddle_dbs_healthy = health.waddle_dbs_healthy,
                loaded_waddle_count = health.loaded_waddle_count,
                "Readiness check: database pool not fully ready"
            );
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "status": "not_ready",
                    "service": "waddle-server",
                    "version": env!("CARGO_PKG_VERSION"),
                    "database": {
                        "status": "not_ready",
                        "global_healthy": health.global_healthy,
                        "waddle_dbs_healthy": health.waddle_dbs_healthy,
                        "loaded_waddle_count": health.loaded_waddle_count
                    }
                })),
            )
        }
        Ok(Err(e)) => {
            warn!(error = %e, "Readiness check failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "status": "not_ready",
                    "service": "waddle-server",
                    "version": env!("CARGO_PKG_VERSION"),
                    "database": {
                        "status": format!("error: {}", e)
                    }
                })),
            )
        }
    }
}

/// Prometheus metrics endpoint.
pub(crate) async fn metrics_handler() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        waddle_xmpp::prometheus::render_metrics(),
    )
}

#[cfg(test)]
mod readiness_generation_tests {
    use super::*;

    async fn assert_transition_during_health_returns_unavailable(
        transition: impl FnOnce(&crate::clustering::NodeLifecycle),
    ) -> crate::clustering::NodeAdmissionError {
        let lifecycle = crate::clustering::NodeLifecycle::new();
        let worker_lifecycle = lifecycle.clone();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let worker = tokio::spawn(async move {
            health_for_serving_generation(&worker_lifecycle, async move {
                let _ = started_tx.send(());
                let _ = release_rx.await;
                true
            })
            .await
        });

        started_rx.await.expect("health check started");
        transition(&lifecycle);
        let _ = release_tx.send(());
        let error = worker
            .await
            .expect("health race task")
            .expect_err("changed serving generation must not become ready");

        assert_eq!(
            lifecycle_not_ready_response(&lifecycle, &error)
                .into_response()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        error
    }

    #[tokio::test]
    async fn fence_during_database_health_cannot_return_ready() {
        let _ = assert_transition_during_health_returns_unavailable(|lifecycle| {
            lifecycle.begin_fenced_recovery();
        })
        .await;
    }

    #[tokio::test]
    async fn critical_failure_during_database_health_cannot_return_ready() {
        let _ = assert_transition_during_health_returns_unavailable(|lifecycle| {
            lifecycle.fail(crate::clustering::CriticalNodeFailure::RoomRegistryTerminated);
        })
        .await;
    }

    #[tokio::test]
    async fn fence_and_recovery_during_database_health_revokes_old_readiness_probe() {
        let error = assert_transition_during_health_returns_unavailable(|lifecycle| {
            lifecycle.begin_fenced_recovery();
            lifecycle.serve();
            assert_eq!(
                lifecycle.admission(),
                crate::clustering::NodeAdmission::Serving
            );
        })
        .await;
        assert_eq!(error, crate::clustering::NodeAdmissionError::Revoked);
    }

    #[tokio::test]
    async fn healthy_database_in_same_serving_generation_is_ready() {
        let lifecycle = crate::clustering::NodeLifecycle::new();
        assert!(health_for_serving_generation(&lifecycle, async { true })
            .await
            .expect("serving generation"));
    }
}

/// Detailed health check endpoint (for monitoring)
pub(crate) async fn detailed_health_handler(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    match state.db_pool.health_check().await {
        Ok(health) => {
            let status = if health.is_healthy() {
                "healthy"
            } else {
                "degraded"
            };
            let status_code = if health.is_healthy() {
                StatusCode::OK
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            };

            (
                status_code,
                Json(DetailedHealthResponse {
                    status: status.to_string(),
                    service: "waddle-server".to_string(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    license: "AGPL-3.0".to_string(),
                    database: health.into(),
                }),
            )
        }
        Err(e) => {
            warn!("Detailed health check failed: {}", e);
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(DetailedHealthResponse {
                    status: "unhealthy".to_string(),
                    service: "waddle-server".to_string(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    license: "AGPL-3.0".to_string(),
                    database: DatabaseHealthStatus {
                        status: format!("error: {}", e),
                        global_healthy: false,
                        waddle_dbs_healthy: false,
                        loaded_waddle_count: 0,
                    },
                }),
            )
        }
    }
}
