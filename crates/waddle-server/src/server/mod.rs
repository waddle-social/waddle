use crate::config::{ServerConfig, ServerInfo};
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
use routes::uploads::UploadState;
use routes::waddles::WaddleState;
use routes::websocket::WebSocketState;
use serde::Serialize;
use serde_json::json;
use std::{net::SocketAddr, path::PathBuf, sync::Arc};
use tower_http::{
    compression::CompressionLayer,
    cors::CorsLayer,
    trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer},
};
use tracing::{info, warn, Level};
use waddle_xmpp::XmppServerConfig;
use waddle_xmpp::{muc::MucRoomRegistry, registry::ConnectionRegistry};

mod routes;
pub mod xmpp_state;

pub use xmpp_state::XmppAppState;

/// Server application state
pub struct AppState {
    /// Database pool for global and per-waddle databases
    pub db_pool: DatabasePool,
    /// Server configuration (mode, etc.)
    pub server_config: ServerConfig,
}

impl AppState {
    pub fn new(db_pool: DatabasePool, server_config: ServerConfig) -> Self {
        Self {
            db_pool,
            server_config,
        }
    }
}

/// XMPP server configuration loaded from environment variables.
#[derive(Debug, Clone)]
pub struct XmppConfig {
    /// Whether XMPP server is enabled (default: true)
    pub enabled: bool,
    /// XMPP server domain (default: "localhost")
    pub domain: String,
    /// Client-to-server bind address (default: "0.0.0.0:5222")
    pub c2s_addr: SocketAddr,
    /// Server-to-server bind address (default: "0.0.0.0:5269")
    pub s2s_addr: SocketAddr,
    /// Whether S2S federation is enabled (default: false)
    pub s2s_enabled: bool,
    /// TLS certificate path (default: "certs/server.crt")
    pub tls_cert_path: String,
    /// TLS key path (default: "certs/server.key")
    pub tls_key_path: String,
    /// MAM database path (None for in-memory)
    pub mam_db_path: Option<PathBuf>,
    /// Whether native JID authentication is enabled (default: true)
    /// When enabled, users can authenticate with SCRAM-SHA-256 using native credentials.
    pub native_auth_enabled: bool,
    /// Whether XEP-0077 In-Band Registration is enabled (default: false)
    /// When enabled, users can register new accounts before authentication.
    /// Security note: Enable with caution on public servers.
    pub registration_enabled: bool,
}

impl Default for XmppConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            domain: "localhost".to_string(),
            c2s_addr: "0.0.0.0:5222".parse().expect("Valid default address"),
            s2s_addr: "0.0.0.0:5269".parse().expect("Valid default S2S address"),
            s2s_enabled: false, // Disabled by default
            tls_cert_path: "certs/server.crt".to_string(),
            tls_key_path: "certs/server.key".to_string(),
            mam_db_path: None,
            native_auth_enabled: true,
            registration_enabled: false, // Disabled by default for security
        }
    }
}

impl XmppConfig {
    /// Load XMPP configuration from environment variables.
    pub fn from_env() -> Self {
        let enabled = std::env::var("WADDLE_XMPP_ENABLED")
            .map(|v| v.to_lowercase() != "false" && v != "0")
            .unwrap_or(true);

        let domain =
            std::env::var("WADDLE_XMPP_DOMAIN").unwrap_or_else(|_| "localhost".to_string());

        let c2s_addr = std::env::var("WADDLE_XMPP_C2S_ADDR")
            .unwrap_or_else(|_| "0.0.0.0:5222".to_string())
            .parse()
            .unwrap_or_else(|_| "0.0.0.0:5222".parse().expect("Valid fallback address"));

        let tls_cert_path = std::env::var("WADDLE_XMPP_TLS_CERT")
            .unwrap_or_else(|_| "certs/server.crt".to_string());

        let tls_key_path =
            std::env::var("WADDLE_XMPP_TLS_KEY").unwrap_or_else(|_| "certs/server.key".to_string());

        let mam_db_path = std::env::var("WADDLE_XMPP_MAM_DB").ok().map(PathBuf::from);

        let native_auth_enabled = std::env::var("WADDLE_NATIVE_AUTH_ENABLED")
            .map(|v| v.to_lowercase() != "false" && v != "0")
            .unwrap_or(true);

        let registration_enabled = std::env::var("WADDLE_REGISTRATION_ENABLED")
            .map(|v| v.to_lowercase() == "true" || v == "1")
            .unwrap_or(false);

        let s2s_enabled = std::env::var("WADDLE_XMPP_S2S_ENABLED")
            .map(|v| v.to_lowercase() == "true" || v == "1")
            .unwrap_or(false);

        let s2s_addr = std::env::var("WADDLE_XMPP_S2S_ADDR")
            .unwrap_or_else(|_| "0.0.0.0:5269".to_string())
            .parse()
            .unwrap_or_else(|_| "0.0.0.0:5269".parse().expect("Valid fallback S2S address"));

        Self {
            enabled,
            domain,
            c2s_addr,
            s2s_addr,
            s2s_enabled,
            tls_cert_path,
            tls_key_path,
            mam_db_path,
            native_auth_enabled,
            registration_enabled,
        }
    }

    /// Convert to waddle_xmpp::XmppServerConfig.
    pub fn to_xmpp_server_config(&self) -> XmppServerConfig {
        XmppServerConfig {
            c2s_addr: self.c2s_addr,
            s2s_addr: if self.s2s_enabled {
                Some(self.s2s_addr)
            } else {
                None
            },
            s2s_enabled: self.s2s_enabled,
            tls_cert_path: self.tls_cert_path.clone(),
            tls_key_path: self.tls_key_path.clone(),
            domain: self.domain.clone(),
            mam_db_path: self.mam_db_path.clone(),
            native_auth_enabled: self.native_auth_enabled,
            registration_enabled: self.registration_enabled,
        }
    }
}

/// Start both HTTP and XMPP servers.
///
/// This function spawns both servers concurrently and returns when either fails.
pub async fn start(db_pool: DatabasePool, server_config: ServerConfig) -> Result<()> {
    let xmpp_config = XmppConfig::from_env();

    start_with_config(db_pool, xmpp_config, server_config).await
}

/// Start both HTTP and XMPP servers with explicit configuration.
pub async fn start_with_config(
    db_pool: DatabasePool,
    xmpp_config: XmppConfig,
    server_config: ServerConfig,
) -> Result<()> {
    let encryption_key = server_config.session_key.clone();

    // Create XMPP app state first (before db_pool is moved)
    // Clone the global database for XMPP state
    let xmpp_app_state = if xmpp_config.enabled {
        Some(Arc::new(XmppAppState::new(
            xmpp_config.domain.clone(),
            Arc::new(db_pool.global().clone()),
            encryption_key.as_ref().map(|s| s.as_bytes()),
        )))
    } else {
        None
    };

    // Now create HTTP state (takes ownership of db_pool)
    let state = Arc::new(AppState::new(db_pool, server_config.clone()));
    let xmpp_native_auth_enabled = xmpp_config.native_auth_enabled;

    // Start HTTP server
    let http_state = state.clone();
    let http_server_config = server_config.clone();
    let http_handle = tokio::spawn(async move {
        start_http_server(http_state, http_server_config, xmpp_native_auth_enabled).await
    });

    // Optionally start XMPP server
    let xmpp_handle = if let Some(xmpp_app_state) = xmpp_app_state {
        let xmpp_server_config = xmpp_config.to_xmpp_server_config();

        Some(tokio::spawn(async move {
            start_xmpp_server(xmpp_server_config, xmpp_app_state).await
        }))
    } else {
        info!("XMPP server disabled via WADDLE_XMPP_ENABLED=false");
        None
    };

    // Wait for either server to complete (usually due to an error)
    tokio::select! {
        result = http_handle => {
            match result {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => Err(e),
                Err(e) => Err(anyhow::anyhow!("HTTP server task failed: {}", e)),
            }
        }
        result = async {
            match xmpp_handle {
                Some(handle) => handle.await,
                None => std::future::pending().await,
            }
        } => {
            match result {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => Err(e),
                Err(e) => Err(anyhow::anyhow!("XMPP server task failed: {}", e)),
            }
        }
    }
}

/// Start the HTTP server.
async fn start_http_server(
    state: Arc<AppState>,
    server_config: ServerConfig,
    xmpp_native_auth_enabled: bool,
) -> Result<()> {
    let app = create_router(state, server_config, xmpp_native_auth_enabled);

    let addr = SocketAddr::from(([0, 0, 0, 0], 3000));
    info!("Starting Axum HTTP server on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

/// Start the XMPP server.
async fn start_xmpp_server(config: XmppServerConfig, app_state: Arc<XmppAppState>) -> Result<()> {
    info!(
        addr = %config.c2s_addr,
        domain = %config.domain,
        "Starting XMPP server"
    );

    let server = waddle_xmpp::start(config, app_state)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to create XMPP server: {}", e))?;

    server
        .run()
        .await
        .map_err(|e| anyhow::anyhow!("XMPP server error: {}", e))?;

    Ok(())
}

/// State for the server-info endpoint
#[derive(Clone)]
struct ServerInfoState {
    server_info: ServerInfo,
}

/// Create the Axum router with all routes and middleware
fn create_router(
    state: Arc<AppState>,
    server_config: ServerConfig,
    xmpp_native_auth_enabled: bool,
) -> Router {
    // Create auth state with configuration from environment or defaults
    // The base URL is used to construct client_id and redirect_uri for OAuth
    let base_url = server_config.base_url.clone();
    let encryption_key = server_config.session_key.clone();

    let auth_state = Arc::new(AuthState::new(
        state.clone(),
        &base_url,
        encryption_key.as_ref().map(|s| s.as_bytes()),
    ));

    // Create connection registry for WebSocket message routing
    let connection_registry = Arc::new(ConnectionRegistry::new());

    // Create MUC room registry with the MUC domain
    let domain = url::Url::parse(&base_url)
        .ok()
        .and_then(|u| u.host_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "localhost".to_string());
    let muc_domain = format!("muc.{}", domain);
    let muc_registry = Arc::new(MucRoomRegistry::new(muc_domain));

    // XMPP over WebSocket (RFC 7395) with registries for message routing
    let websocket_state = Arc::new(WebSocketState {
        auth_state: auth_state.clone(),
        connection_registry,
        muc_registry,
    });
    let websocket_router = routes::websocket::router(websocket_state);

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

    // Upload router for XEP-0363 HTTP File Upload
    let upload_state = Arc::new(UploadState::new(state.clone()));
    let upload_router = routes::uploads::router(upload_state);

    // Create server info for the /api/v1/server-info endpoint
    let server_info = ServerInfo::from_config(&server_config, xmpp_native_auth_enabled);
    let server_info_state = ServerInfoState { server_info };
    // Well-known endpoints for XMPP service discovery (XEP-0156)
    let well_known_router = routes::well_known::router(auth_state.clone());

    // Build the base router with health and server-info endpoints
    let mut router = Router::new()
        .route("/health", get(health_handler))
        .route("/api/v1/health", get(detailed_health_handler))
        .with_state(state)
        .route("/api/v1/server-info", get(server_info_handler))
        .with_state(server_info_state);

    // Conditionally merge ATProto routes based on server mode
    if server_config.mode.atproto_enabled() {
        info!("Registering ATProto OAuth routes (HomeServer mode)");

        // Auth router uses its own state type, so we apply .with_state() before merging
        // This converts Router<Arc<AuthState>> to Router<()>, which can then be merged
        let auth_router = routes::auth::router(auth_state.clone());

        // Device flow router for CLI authentication
        let device_store = Arc::new(dashmap::DashMap::new());
        let device_router = routes::device::router(auth_state.clone(), device_store);

        // XMPP OAuth router (XEP-0493) for standard XMPP client authentication
        let xmpp_oauth_router = routes::xmpp_oauth::router(auth_state.clone());

        // Auth page router for web-based XMPP credential retrieval
        let auth_page_router = routes::auth_page::router(auth_state.clone());

        router = router
            // Merge auth routes after the main router has its state applied
            .merge(auth_router)
            // Merge device flow routes for CLI authentication
            .merge(device_router)
            // Merge XMPP OAuth routes for standard XMPP client authentication (XEP-0493)
            .merge(xmpp_oauth_router)
            // Merge auth page routes for web-based XMPP credential retrieval
            .merge(auth_page_router);
    } else {
        info!("ATProto OAuth routes disabled (Standalone mode)");
    }

    // Always merge common routes (WebSocket, permissions, waddles, channels, uploads)
    router
        // Merge XMPP over WebSocket endpoint
        .merge(websocket_router)
        // Merge permission routes
        .merge(permission_router)
        // Merge waddles routes
        .merge(waddles_router)
        // Merge channels routes
        .merge(channels_router)
        // Merge well-known endpoints for XMPP service discovery
        .merge(well_known_router)
        // Merge upload routes for XEP-0363 HTTP File Upload
        .merge(upload_router)
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::new().level(Level::INFO))
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        )
        .layer(CompressionLayer::new())
        .layer(CorsLayer::permissive()) // TODO: Configure proper CORS in production
}

/// Handler for the /api/v1/server-info endpoint
async fn server_info_handler(State(state): State<ServerInfoState>) -> impl IntoResponse {
    (StatusCode::OK, Json(state.server_info))
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

        Arc::new(AppState::new(db_pool, ServerConfig::test_homeserver()))
    }

    #[tokio::test]
    async fn test_health_endpoint() {
        let state = create_test_state().await;
        let server_config = ServerConfig::test_homeserver();
        let app = create_router(state, server_config, true);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
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
        assert_eq!(json["service"], "waddle-server");
    }

    #[tokio::test]
    async fn test_detailed_health_endpoint() {
        let state = create_test_state().await;
        let server_config = ServerConfig::test_homeserver();
        let app = create_router(state, server_config, true);

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
