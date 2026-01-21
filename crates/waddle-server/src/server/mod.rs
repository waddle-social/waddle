use crate::db::{DatabasePool, PoolHealth};
use anyhow::Result;
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::get,
    Router,
};
use routes::auth::AuthState;
use routes::channels::ChannelState;
use routes::permissions::PermissionState;
use routes::waddles::WaddleState;
use serde::Serialize;
use serde_json::json;
use std::{net::SocketAddr, sync::Arc};
use tower_http::{
    compression::CompressionLayer,
    cors::CorsLayer,
    trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer},
};
use tracing::{info, warn, Level};

mod routes;

/// Server application state
pub struct AppState {
    /// Database pool for global and per-waddle databases
    pub db_pool: DatabasePool,
}

impl AppState {
    pub fn new(db_pool: DatabasePool) -> Self {
        Self { db_pool }
    }
}

/// Start the HTTP server
pub async fn start(db_pool: DatabasePool) -> Result<()> {
    let state = Arc::new(AppState::new(db_pool));

    let app = create_router(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], 3000));
    info!("Starting Axum HTTP server on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

/// Create the Axum router with all routes and middleware
fn create_router(state: Arc<AppState>) -> Router {
    // Create auth state with configuration from environment or defaults
    let client_id = std::env::var("WADDLE_OAUTH_CLIENT_ID")
        .unwrap_or_else(|_| "https://waddle.social/oauth/client".to_string());
    let redirect_uri = std::env::var("WADDLE_OAUTH_REDIRECT_URI")
        .unwrap_or_else(|_| "https://waddle.social/v1/auth/atproto/callback".to_string());
    let encryption_key = std::env::var("WADDLE_SESSION_KEY").ok();

    let auth_state = Arc::new(AuthState::new(
        state.clone(),
        &client_id,
        &redirect_uri,
        encryption_key.as_ref().map(|s| s.as_bytes()),
    ));

    // Auth router uses its own state type, so we apply .with_state() before merging
    // This converts Router<Arc<AuthState>> to Router<()>, which can then be merged
    let auth_router = routes::auth::router(auth_state);

    // Permission router with Zanzibar-inspired permission service
    let permission_state = Arc::new(PermissionState::new(state.clone()));
    let permission_router = routes::permissions::router(permission_state);

    // Waddles router for community CRUD operations
    let waddle_state = Arc::new(WaddleState::new(
        state.clone(),
        encryption_key.as_ref().map(|s| s.as_bytes()),
    ));
    let waddles_router = routes::waddles::router(waddle_state);

    // Channels router for channel CRUD operations
    let channel_state = Arc::new(ChannelState::new(
        state.clone(),
        encryption_key.as_ref().map(|s| s.as_bytes()),
    ));
    let channels_router = routes::channels::router(channel_state);

    Router::new()
        .route("/health", get(health_handler))
        .route("/api/v1/health", get(detailed_health_handler))
        // Future routes will be mounted here
        // .merge(routes::messages::router())
        .with_state(state)
        // Merge auth routes after the main router has its state applied
        .merge(auth_router)
        // Merge permission routes
        .merge(permission_router)
        // Merge waddles routes
        .merge(waddles_router)
        // Merge channels routes
        .merge(channels_router)
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::new().level(Level::INFO))
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        )
        .layer(CompressionLayer::new())
        .layer(CorsLayer::permissive()) // TODO: Configure proper CORS in production
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
            status: if health.is_healthy() { "healthy" } else { "unhealthy" }.to_string(),
            global_healthy: health.global_healthy,
            waddle_dbs_healthy: health.waddle_dbs_healthy,
            loaded_waddle_count: health.loaded_waddle_count,
        }
    }
}

/// Simple health check endpoint (for load balancers)
async fn health_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
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

/// Detailed health check endpoint (for monitoring)
async fn detailed_health_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match state.db_pool.health_check().await {
        Ok(health) => {
            let status = if health.is_healthy() { "healthy" } else { "degraded" };
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
    use crate::db::{DatabaseConfig, MigrationRunner, PoolConfig};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn create_test_state() -> Arc<AppState> {
        let config = DatabaseConfig::default();
        let pool_config = PoolConfig::default();
        let db_pool = DatabasePool::new(config, pool_config).await.unwrap();

        // Run migrations
        let runner = MigrationRunner::global();
        runner.run(db_pool.global()).await.unwrap();

        Arc::new(AppState::new(db_pool))
    }

    #[tokio::test]
    async fn test_health_endpoint() {
        let state = create_test_state().await;
        let app = create_router(state);

        let response = app
            .oneshot(Request::builder().uri("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        // Parse response body
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["status"], "healthy");
        assert_eq!(json["service"], "waddle-server");
    }

    #[tokio::test]
    async fn test_detailed_health_endpoint() {
        let state = create_test_state().await;
        let app = create_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        // Parse response body
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["status"], "healthy");
        assert_eq!(json["database"]["status"], "healthy");
        assert!(json["database"]["global_healthy"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn test_database_in_app_state() {
        let state = create_test_state().await;

        // Verify we can access the database through AppState
        let health = state.db_pool.health_check().await.unwrap();
        assert!(health.is_healthy());

        // Verify we can create waddle databases
        let waddle_db = state.db_pool.create_waddle_db("test-waddle").await.unwrap();

        // Run waddle migrations
        let runner = MigrationRunner::waddle();
        runner.run(&waddle_db).await.unwrap();

        // Verify tables exist - use persistent connection for in-memory database
        let conn = waddle_db.persistent_connection().unwrap();
        let conn = conn.lock().await;
        let mut rows = conn
            .query(
                "SELECT name FROM sqlite_master WHERE type='table' AND name='channels'",
                (),
            )
            .await
            .unwrap();

        assert!(rows.next().await.unwrap().is_some());
    }
}
