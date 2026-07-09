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

/// Readiness check endpoint (for orchestrators).
///
/// Readiness is stricter than liveness and validates overall DB pool health
/// **and** the ADR-0017 Phase 3 Slice 2 clustering readiness signal: a node
/// that has self-fenced its entity-ownership claims (node-lease heartbeat
/// lost, or Postgres unreachable past the lease deadline) must not stay in
/// the client Service/Ingress endpoint set — clients whose sockets it just
/// closed would otherwise be routed straight back to the still-refusing
/// node. Non-clustering deployments never flip this signal, so it is
/// always ready — today's behavior, unchanged.
pub(crate) async fn readiness_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    if !state.clustering_readiness.is_ready() {
        warn!("Readiness check: this node has self-fenced its clustering claims");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "status": "not_ready",
                "service": "waddle-server",
                "version": env!("CARGO_PKG_VERSION"),
                "clustering": "self-fenced"
            })),
        );
    }
    match state.db_pool.health_check().await {
        Ok(health) if health.is_healthy() => (
            StatusCode::OK,
            Json(json!({
                "status": "ready",
                "service": "waddle-server",
                "version": env!("CARGO_PKG_VERSION"),
                "database": "ready"
            })),
        ),
        Ok(health) => {
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
        Err(e) => {
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
    let mut metrics = waddle_xmpp::prometheus::render_metrics();
    render_build_info_metric(
        &mut metrics,
        crate::build_identity::embedded_git_sha(),
        |name| std::env::var(name).ok(),
    );
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        metrics,
    )
}

#[derive(Debug, PartialEq, Eq)]
struct DeploymentIdentity {
    commit: String,
    environment: String,
    cluster: String,
}

fn deployment_identity(
    embedded_commit: Option<&str>,
    get: impl Fn(&str) -> Option<String>,
) -> DeploymentIdentity {
    let bounded_label = |name: &str| {
        get(name)
            .filter(|value| {
                !value.is_empty()
                    && value.len() <= 64
                    && value.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')
                    })
            })
            .unwrap_or_else(|| "unknown".to_string())
    };
    let commit = embedded_commit
        .filter(|value| {
            value.len() == 40
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .map(str::to_string)
        .unwrap_or_else(|| "unknown".to_string());
    DeploymentIdentity {
        commit,
        environment: bounded_label("DEPLOYMENT_ENVIRONMENT_NAME"),
        cluster: bounded_label("DEPLOYMENT_CLUSTER_NAME"),
    }
}

fn render_build_info_metric(
    out: &mut String,
    embedded_commit: Option<&str>,
    get: impl Fn(&str) -> Option<String>,
) {
    let identity = deployment_identity(embedded_commit, get);
    out.push_str(
        "# HELP waddle_build_info Deployment identity for evidence scoping; value is always one.\n",
    );
    out.push_str("# TYPE waddle_build_info gauge\n");
    out.push_str("waddle_build_info{commit=\"");
    out.push_str(&identity.commit);
    out.push_str("\",deployment_environment=\"");
    out.push_str(&identity.environment);
    out.push_str("\",cluster=\"");
    out.push_str(&identity.cluster);
    out.push_str("\"} 1\n");
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn build_info_metric_binds_validated_deployment_identity() {
        let values = BTreeMap::from([
            ("DEPLOYMENT_ENVIRONMENT_NAME", "production"),
            ("DEPLOYMENT_CLUSTER_NAME", "waddle-cloud"),
        ]);
        let mut rendered = String::new();
        render_build_info_metric(
            &mut rendered,
            Some("0123456789abcdef0123456789abcdef01234567"),
            |name| values.get(name).map(ToString::to_string),
        );

        assert!(rendered.contains(
            "waddle_build_info{commit=\"0123456789abcdef0123456789abcdef01234567\",deployment_environment=\"production\",cluster=\"waddle-cloud\"} 1"
        ));
    }

    #[test]
    fn build_info_metric_rejects_unbounded_or_unsafe_labels() {
        let values = BTreeMap::from([
            ("WADDLE_GIT_SHA", "not-a-commit"),
            ("DEPLOYMENT_ENVIRONMENT_NAME", "production\nsecret"),
            ("DEPLOYMENT_CLUSTER_NAME", "cluster/secret"),
        ]);
        let mut rendered = String::new();
        render_build_info_metric(&mut rendered, Some("not-a-commit"), |name| {
            values.get(name).map(ToString::to_string)
        });

        assert!(rendered.contains(
            "waddle_build_info{commit=\"unknown\",deployment_environment=\"unknown\",cluster=\"unknown\"} 1"
        ));
        assert!(!rendered.contains("secret"));
    }

    #[test]
    fn build_info_metric_cannot_be_spoofed_by_runtime_release_metadata() {
        let values = BTreeMap::from([
            ("WADDLE_GIT_SHA", "ffffffffffffffffffffffffffffffffffffffff"),
            ("DEPLOYMENT_ENVIRONMENT_NAME", "production"),
            ("DEPLOYMENT_CLUSTER_NAME", "waddle-cloud"),
        ]);
        let mut rendered = String::new();
        render_build_info_metric(
            &mut rendered,
            Some("0123456789abcdef0123456789abcdef01234567"),
            |name| values.get(name).map(ToString::to_string),
        );

        assert!(rendered.contains("commit=\"0123456789abcdef0123456789abcdef01234567\""));
        assert!(!rendered.contains("ffffffffffffffffffffffffffffffffffffffff"));
    }
}
