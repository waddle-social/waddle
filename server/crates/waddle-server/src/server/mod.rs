use crate::auth::{NativeUserStore, RegisterRequest};
use crate::config::{ServerConfig, ServerInfo};
use crate::db::{DatabasePool, PoolHealth};
use crate::inbox::build_inbox_storage;
use anyhow::Result;
use axum::{
    extract::State,
    http::{header, HeaderName, Method, StatusCode},
    response::{IntoResponse, Json},
    routing::get,
    Router,
};
use futures::StreamExt;
use opentelemetry::trace::TraceContextExt;
use opentelemetry_http::HeaderExtractor;
use routes::auth::AuthState;
use routes::channels::ChannelState;
use routes::permissions::PermissionState;
use routes::uploads::UploadState;
use routes::waddles::WaddleState;
use routes::websocket::{ProtocolServices, WebSocketDeps, WebSocketState};
use rustls::ServerConfig as RustlsServerConfig;
use rustls_acme::caches::DirCache;
use rustls_acme::tower::TowerHttp01ChallengeService;
use rustls_acme::{AcmeConfig, UseChallenge};
use serde::Serialize;
use serde_json::json;
use std::{net::SocketAddr, path::PathBuf, sync::Arc};
use tower_http::{
    compression::CompressionLayer,
    cors::CorsLayer,
    trace::{DefaultOnResponse, TraceLayer},
};
use tracing::{info, info_span, warn, Level, Span};
use tracing_opentelemetry::OpenTelemetrySpanExt;
use waddle_extensions::{ExtensionConfig, ExtensionManager};
use waddle_xmpp::inbox::storage::InboxStorage;
use waddle_xmpp::mam::{MamStorage, SqlxMamStorage};
use waddle_xmpp::XmppServerConfig;
use waddle_xmpp::{muc::room_registry_actor::RoomRegistryActor, registry::ConnectionRegistry};

mod routes;
pub mod xmpp_state;

pub use xmpp_state::XmppAppState;

#[derive(Debug, Clone)]
pub struct XmppAcmeConfig {
    /// Whether ACME-managed certificates are enabled
    pub enabled: bool,
    /// Contact email for ACME account registration
    pub email: Option<String>,
    /// Cache directory for ACME account and certificate material
    pub cache_dir: PathBuf,
    /// Use Let's Encrypt production directory instead of staging
    pub production: bool,
}

#[derive(Clone)]
struct AcmeRuntime {
    tls_server_config: Arc<RustlsServerConfig>,
    http01_challenge_service: TowerHttp01ChallengeService,
}

/// Server application state
pub struct AppState {
    /// Database pool for global and per-waddle databases
    pub db_pool: Arc<DatabasePool>,
    /// Blob storage backend for file uploads (XEP-0363).
    pub blob_storage: Arc<dyn crate::storage::BlobStorage>,
    /// Shared XEP-0430 inbox projection storage.
    pub inbox_storage: Arc<dyn InboxStorage>,
}

impl AppState {
    /// Test-only default constructor — uses a disabled media backend
    /// and the filesystem blob storage from `WADDLE_UPLOAD_DIR`.
    /// Production code should call [`Self::new_with_deps`] so each
    /// dependency is explicit.
    #[cfg(test)]
    pub fn new(db_pool: Arc<DatabasePool>) -> Self {
        let blob_storage = crate::storage::build_blob_storage()
            .unwrap_or_else(|e| panic!("failed to initialize blob storage: {e}"));
        Self::new_with_deps(
            db_pool,
            blob_storage,
            Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new()),
        )
    }

    pub fn new_with_deps(
        db_pool: Arc<DatabasePool>,
        blob_storage: Arc<dyn crate::storage::BlobStorage>,
        inbox_storage: Arc<dyn InboxStorage>,
    ) -> Self {
        Self {
            db_pool,
            blob_storage,
            inbox_storage,
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
    /// Server-to-server bind address (default: "0.0.0.0:5269")
    pub s2s_addr: SocketAddr,
    /// Whether S2S federation is enabled (default: false)
    pub s2s_enabled: bool,
    /// TLS certificate path (default: "certs/server.crt")
    pub tls_cert_path: String,
    /// TLS key path (default: "certs/server.key")
    pub tls_key_path: String,
    /// MAM database URL (prefers dedicated XMPP DSN, otherwise the main runtime DSN)
    pub mam_database_url: Option<String>,
    /// Inbox database URL (prefers dedicated XMPP DSN, otherwise the main runtime DSN)
    pub inbox_database_url: Option<String>,
    /// Whether native JID authentication is enabled (default: true)
    /// When enabled, users can authenticate with SCRAM-SHA-256 using native credentials.
    pub native_auth_enabled: bool,
    /// Whether XEP-0077 In-Band Registration is enabled (default: false)
    /// When enabled, users can register new accounts before authentication.
    /// Security note: Enable with caution on public servers.
    pub registration_enabled: bool,
    /// Whether the server operates in single-tenant mode (default: false).
    /// When true, all spaces are publicly discoverable regardless of membership.
    /// Controlled by `WADDLE_SINGLE_TENANT` env var.
    pub single_tenant: bool,
    /// ACME configuration for managed TLS certificates.
    pub acme: XmppAcmeConfig,
    /// Whether to generate ephemeral self-signed TLS certificates in memory.
    /// Enabled via `WADDLE_CERTS_EPHEMERAL=true` or `--ephemeral-certs`.
    pub ephemeral_certs: bool,
    /// Runtime extension configuration.
    pub extensions: ExtensionConfig,
}

impl Default for XmppConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            domain: "localhost".to_string(),
            s2s_addr: "0.0.0.0:5269".parse().expect("Valid default S2S address"),
            s2s_enabled: false, // Disabled by default
            tls_cert_path: "certs/server.crt".to_string(),
            tls_key_path: "certs/server.key".to_string(),
            mam_database_url: None,
            inbox_database_url: None,
            native_auth_enabled: true,
            registration_enabled: false, // Disabled by default for security
            single_tenant: false,
            extensions: ExtensionConfig::default(),
            acme: XmppAcmeConfig {
                enabled: false,
                email: None,
                cache_dir: PathBuf::from("certs/acme-cache"),
                production: false,
            },
            ephemeral_certs: false,
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

        let tls_cert_path = std::env::var("WADDLE_XMPP_TLS_CERT")
            .unwrap_or_else(|_| "certs/server.crt".to_string());

        let tls_key_path =
            std::env::var("WADDLE_XMPP_TLS_KEY").unwrap_or_else(|_| "certs/server.key".to_string());

        let mam_database_url = resolve_xmpp_database_url("WADDLE_XMPP_MAM_DATABASE_URL");
        let inbox_database_url = resolve_xmpp_database_url("WADDLE_XMPP_INBOX_DATABASE_URL");

        let native_auth_enabled = std::env::var("WADDLE_NATIVE_AUTH_ENABLED")
            .map(|v| v.to_lowercase() != "false" && v != "0")
            .unwrap_or(true);

        let registration_enabled = std::env::var("WADDLE_REGISTRATION_ENABLED")
            .map(|v| v.to_lowercase() == "true" || v == "1")
            .unwrap_or(false);

        let acme_enabled = std::env::var("WADDLE_XMPP_ACME_ENABLED")
            .map(|v| v.to_lowercase() == "true" || v == "1")
            .unwrap_or(false);
        let acme_email = std::env::var("WADDLE_XMPP_ACME_EMAIL")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());
        let acme_cache_dir = std::env::var("WADDLE_XMPP_ACME_CACHE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("certs/acme-cache"));
        let acme_production = std::env::var("WADDLE_XMPP_ACME_PRODUCTION")
            .map(|v| v.to_lowercase() == "true" || v == "1")
            .unwrap_or(false);

        let single_tenant = std::env::var("WADDLE_SINGLE_TENANT")
            .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
            .unwrap_or(false);

        let s2s_enabled = std::env::var("WADDLE_XMPP_S2S_ENABLED")
            .map(|v| v.to_lowercase() == "true" || v == "1")
            .unwrap_or(false);

        let s2s_addr = std::env::var("WADDLE_XMPP_S2S_ADDR")
            .unwrap_or_else(|_| "0.0.0.0:5269".to_string())
            .parse()
            .unwrap_or_else(|_| "0.0.0.0:5269".parse().expect("Valid fallback S2S address"));
        let extensions = ExtensionConfig::from_env().unwrap_or_else(|error| {
            warn!(error = %error, "Invalid extension config from environment; using defaults");
            ExtensionConfig::default()
        });

        let ephemeral_certs = std::env::var("WADDLE_CERTS_EPHEMERAL")
            .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
            .unwrap_or(false)
            || std::env::args().any(|a| a == "--ephemeral-certs");

        Self {
            enabled,
            domain,
            s2s_addr,
            s2s_enabled,
            tls_cert_path,
            tls_key_path,
            mam_database_url,
            inbox_database_url,
            native_auth_enabled,
            registration_enabled,
            single_tenant,
            extensions,
            acme: XmppAcmeConfig {
                enabled: acme_enabled,
                email: acme_email,
                cache_dir: acme_cache_dir,
                production: acme_production,
            },
            ephemeral_certs,
        }
    }

    /// Convert to waddle_xmpp::XmppServerConfig.
    pub fn to_xmpp_server_config(
        &self,
        tls_server_config: Option<Arc<RustlsServerConfig>>,
    ) -> XmppServerConfig {
        XmppServerConfig {
            s2s_addr: if self.s2s_enabled {
                Some(self.s2s_addr)
            } else {
                None
            },
            s2s_enabled: self.s2s_enabled,
            tls_cert_path: self.tls_cert_path.clone(),
            tls_key_path: self.tls_key_path.clone(),
            tls_server_config,
            domain: self.domain.clone(),
            mam_database_url: self.mam_database_url.clone(),
            native_auth_enabled: self.native_auth_enabled,
            registration_enabled: self.registration_enabled,
            single_tenant: self.single_tenant,
            extensions: self.extensions.clone(),
        }
    }
}

fn resolve_xmpp_database_url(env_key: &str) -> Option<String> {
    std::env::var(env_key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            std::env::var("WADDLE_DATABASE_URL")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
}

fn start_acme_runtime(
    xmpp_config: &XmppConfig,
    stop_token: tokio_util::sync::CancellationToken,
) -> Option<AcmeRuntime> {
    if !xmpp_config.enabled || !xmpp_config.acme.enabled {
        return None;
    }

    if xmpp_config.domain == "localhost" {
        warn!(
            "ACME is enabled but XMPP domain is localhost; public DNS domain is required for Let's Encrypt"
        );
    }

    let mut acme_config = AcmeConfig::new([xmpp_config.domain.as_str()])
        .cache(DirCache::new(xmpp_config.acme.cache_dir.clone()))
        .directory_lets_encrypt(xmpp_config.acme.production)
        .challenge_type(UseChallenge::Http01);

    if let Some(email) = xmpp_config.acme.email.as_deref() {
        let contact = if email.starts_with("mailto:") {
            email.to_string()
        } else {
            format!("mailto:{email}")
        };
        acme_config = acme_config.contact_push(contact);
    } else {
        warn!("ACME is enabled without WADDLE_XMPP_ACME_EMAIL; proceeding without contact email");
    }

    let mut state = acme_config.state();
    let tls_server_config = state.default_rustls_config();
    let http01_challenge_service = state.http01_challenge_tower_service();

    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = stop_token.cancelled() => {
                    info!("ACME task stopped (shutdown token cancelled)");
                    break;
                }
                event = state.next() => {
                    match event {
                        Some(Ok(ok)) => info!(event = ?ok, "ACME event"),
                        Some(Err(err)) => warn!(error = %err, "ACME event failed"),
                        None => {
                            warn!("ACME stream ended unexpectedly");
                            break;
                        }
                    }
                }
            }
        }
    });

    info!(
        domain = %xmpp_config.domain,
        production = xmpp_config.acme.production,
        cache_dir = %xmpp_config.acme.cache_dir.display(),
        "ACME certificate management enabled (HTTP-01)"
    );

    Some(AcmeRuntime {
        tls_server_config,
        http01_challenge_service,
    })
}

/// Start both HTTP and XMPP servers with Ecdysis graceful restart support.
///
/// On SIGTERM: graceful drain and exit.
/// On SIGQUIT: re-exec with fd passing, then drain and exit.
pub async fn start(
    db_pool: DatabasePool,
    server_config: ServerConfig,
    inherited: Option<waddle_ecdysis::ListenerSet>,
) -> Result<()> {
    let xmpp_config = XmppConfig::from_env();

    start_with_config(db_pool, xmpp_config, server_config, inherited).await
}

/// Start both HTTP and XMPP servers with explicit configuration.
pub async fn start_with_config(
    db_pool: DatabasePool,
    xmpp_config: XmppConfig,
    server_config: ServerConfig,
    mut inherited: Option<waddle_ecdysis::ListenerSet>,
) -> Result<()> {
    let encryption_key = server_config.session_key.clone();

    // Set up Ecdysis graceful shutdown coordinator
    let shutdown = waddle_ecdysis::GracefulShutdown::from_env();
    let stop_token = shutdown.stop_token();

    // Acquire listeners: inherited from parent process, or bind fresh.
    // Two explicit paths — no silent fallback.
    let (http_listener, s2s_listener) = if let Some(ref mut set) = inherited {
        // Ecdysis restart path: all listeners MUST be inherited
        let http = set.take("http");
        let s2s = if xmpp_config.enabled && xmpp_config.s2s_enabled {
            Some(set.take("xmpp-s2s"))
        } else {
            None
        };
        (http, s2s)
    } else {
        // Cold start path: bind listeners fresh
        let http_addr: SocketAddr = std::env::var("WADDLE_HTTP_ADDR")
            .unwrap_or_else(|_| "0.0.0.0:3000".to_string())
            .parse()
            .unwrap_or_else(|_| "0.0.0.0:3000".parse().expect("Valid fallback HTTP address"));
        let http = tokio::net::TcpListener::bind(http_addr).await?;
        if let Ok(addr) = http.local_addr() {
            info!(addr = %addr, "Bound HTTP listener");
        } else {
            info!(addr = %http_addr, "Bound HTTP listener");
        }

        let s2s = if xmpp_config.enabled && xmpp_config.s2s_enabled {
            let listener = tokio::net::TcpListener::bind(xmpp_config.s2s_addr).await?;
            info!(addr = %xmpp_config.s2s_addr, "Bound XMPP S2S listener");
            Some(listener)
        } else {
            None
        };

        (http, s2s)
    };

    // If we inherited, verify we consumed everything
    if let Some(set) = inherited {
        set.assert_empty();
    }

    // Collect listeners for restart fd-passing (cloning the raw fds)
    // We need references to pass to restart() on SIGQUIT.
    // Since listeners are moved into server tasks, we use SO_REUSEADDR
    // approach: on SIGQUIT, the new process binds fresh (listeners are
    // in the server tasks). The key Ecdysis value is the graceful drain.
    //
    // For true fd-passing on restart, we'd need to stop the accept loops
    // first, extract the listeners, then pass them. This is the design:

    // Wrap db_pool in Arc for shared ownership between HTTP and XMPP states
    let db_pool = Arc::new(db_pool);
    ensure_fixed_test_account(&db_pool, &xmpp_config).await?;
    let inbox_storage = build_inbox_storage(xmpp_config.inbox_database_url.clone())
        .await
        .map_err(|error| anyhow::anyhow!("Failed to initialize inbox storage: {}", error))?;

    // Create XMPP app state
    let xmpp_app_state = if xmpp_config.enabled {
        Some(Arc::new(
            XmppAppState::new(
                xmpp_config.domain.clone(),
                Arc::new(db_pool.global().clone()),
                db_pool.global_actor().clone(),
                encryption_key.as_ref().map(|s| s.as_bytes()),
            )
            .with_db_pool(Arc::clone(&db_pool))
            .with_inbox_storage(Arc::clone(&inbox_storage)),
        ))
    } else {
        None
    };

    // Single-tenant boot-time guard: fail-fast if no waddles exist
    if xmpp_config.single_tenant {
        if let Some(ref xmpp_state) = xmpp_app_state {
            use waddle_xmpp::AppState as XmppAppStateTrait;
            let waddles = xmpp_state.list_all_waddles(1, 0).await.unwrap_or_default();
            if waddles.is_empty() {
                anyhow::bail!(
                    "Single-tenant mode is enabled (WADDLE_SINGLE_TENANT=true) but no waddles exist. \
                     Create a waddle before enabling single-tenant mode."
                );
            }
        }
    }

    // Create HTTP state (shares db_pool via Arc)
    let blob_storage = crate::storage::build_blob_storage()
        .map_err(|e| anyhow::anyhow!("Failed to initialize blob storage: {}", e))?;
    let state = Arc::new(AppState::new_with_deps(
        Arc::clone(&db_pool),
        blob_storage,
        Arc::clone(&inbox_storage),
    ));
    let websocket_mam_storage =
        create_websocket_mam_storage(xmpp_config.mam_database_url.clone()).await?;
    let xmpp_native_auth_enabled = xmpp_config.native_auth_enabled;
    let acme_runtime = start_acme_runtime(&xmpp_config, stop_token.clone());

    // Start HTTP server
    let http_state = state.clone();
    let http_mam_storage = websocket_mam_storage.clone();
    let http_server_config = server_config.clone();
    let http_stop = stop_token.clone();
    let acme_http01_challenge_service = acme_runtime
        .as_ref()
        .map(|runtime| runtime.http01_challenge_service.clone());
    let http_handle = tokio::spawn(async move {
        start_http_server(HttpServerDeps {
            state: http_state,
            server_config: http_server_config,
            xmpp_native_auth_enabled,
            mam_storage: http_mam_storage,
            acme_http01_challenge_service,
            listener: http_listener,
            stop_token: http_stop,
        })
        .await
    });

    // Start the standalone XMPP listener only for optional federation.
    // Client-to-server traffic is WebSocket-only and is served by the HTTP
    // router at `/xmpp-websocket`.
    let xmpp_handle = if xmpp_config.enabled && xmpp_config.s2s_enabled {
        let xmpp_app_state = xmpp_app_state.expect("XMPP enabled but missing app state");
        let xmpp_tls_server_config = if xmpp_config.ephemeral_certs {
            info!(
                "Using ephemeral self-signed TLS certificate for domain '{}'",
                xmpp_config.domain
            );
            Some(waddle_xmpp::generate_ephemeral_tls_config(
                &xmpp_config.domain,
            )?)
        } else {
            acme_runtime
                .as_ref()
                .map(|runtime| runtime.tls_server_config.clone())
        };
        let xmpp_server_config = xmpp_config.to_xmpp_server_config(xmpp_tls_server_config);
        let xmpp_stop = stop_token.clone();

        Some(tokio::spawn(async move {
            start_xmpp_server(xmpp_server_config, xmpp_app_state, s2s_listener, xmpp_stop).await
        }))
    } else {
        info!("TCP XMPP listener disabled; serving WebSocket C2S only");
        None
    };

    // Run the Ecdysis shutdown lifecycle
    let shutdown_handle = tokio::spawn(async move {
        let signal = shutdown
            .run(|| async {
                // SIGQUIT restart: the new process will read the same binary
                // and bind fresh listeners. The old process drains gracefully.
                // True fd-passing requires stopping accept loops first to
                // extract listeners — implemented here as a clean restart.
                info!("SIGQUIT received — new process will start, old process draining");
                // In the future, we could extract listeners from the tasks
                // and call waddle_ecdysis::restart() for zero-gap fd passing.
                // For now, the new process binds fresh (brief listen gap).
            })
            .await;

        info!(signal = ?signal, "Shutdown lifecycle complete");
    });

    // Wait for any task to complete
    tokio::select! {
        result = http_handle => {
            match result {
                Ok(Ok(())) => {
                    info!("HTTP server stopped");
                    Ok(())
                },
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
                Ok(Ok(())) => {
                    info!("XMPP server stopped");
                    Ok(())
                },
                Ok(Err(e)) => Err(e),
                Err(e) => Err(anyhow::anyhow!("XMPP server task failed: {}", e)),
            }
        }
        _ = shutdown_handle => {
            info!("Graceful shutdown complete");
            Ok(())
        }
    }
}

/// Bundle of parameters for [`start_http_server`].
struct HttpServerDeps {
    state: Arc<AppState>,
    server_config: ServerConfig,
    xmpp_native_auth_enabled: bool,
    mam_storage: Arc<dyn MamStorage>,
    acme_http01_challenge_service: Option<TowerHttp01ChallengeService>,
    listener: tokio::net::TcpListener,
    stop_token: tokio_util::sync::CancellationToken,
}

/// Start the HTTP server with graceful shutdown support.
async fn start_http_server(deps: HttpServerDeps) -> Result<()> {
    let HttpServerDeps {
        state,
        server_config,
        xmpp_native_auth_enabled,
        mam_storage,
        acme_http01_challenge_service,
        listener,
        stop_token,
    } = deps;

    let app = create_router(
        state,
        server_config,
        xmpp_native_auth_enabled,
        mam_storage,
        acme_http01_challenge_service,
    )
    .await;

    let addr = listener.local_addr()?;
    info!("Starting Axum HTTP server on {}", addr);

    // When WADDLE_HTTP_PORT_FILE is set, write the bound port so test
    // harnesses can discover it after binding to port 0.
    if let Ok(path) = std::env::var("WADDLE_HTTP_PORT_FILE") {
        if let Err(e) = std::fs::write(&path, addr.port().to_string()) {
            warn!(path = %path, error = %e, "Failed to write HTTP port file");
        }
    }

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            stop_token.cancelled().await;
            info!("HTTP server received shutdown signal, draining connections");
        })
        .await?;

    Ok(())
}

/// Start the XMPP server.
async fn start_xmpp_server(
    config: XmppServerConfig,
    app_state: Arc<XmppAppState>,
    s2s_listener: Option<tokio::net::TcpListener>,
    stop_token: tokio_util::sync::CancellationToken,
) -> Result<()> {
    info!(
        domain = %config.domain,
        "Starting XMPP server"
    );

    let server = waddle_xmpp::start(config, Arc::clone(&app_state), s2s_listener, stop_token)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to create XMPP server: {}", e))?;

    server
        .run()
        .await
        .map_err(|e| anyhow::anyhow!("XMPP server error: {}", e))?;

    Ok(())
}

async fn create_websocket_mam_storage(database_url: Option<String>) -> Result<Arc<dyn MamStorage>> {
    let storage = match database_url.as_deref() {
        Some(database_url) => SqlxMamStorage::open(database_url).await,
        None => SqlxMamStorage::open_in_memory().await,
    }
    .map_err(|error| anyhow::anyhow!("Failed to initialize WebSocket MAM storage: {error}"))?;

    Ok(Arc::new(storage))
}

#[derive(Debug, Clone)]
struct FixedTestAccountConfig {
    username: String,
    password: String,
    domain: String,
    email: Option<String>,
}

async fn ensure_fixed_test_account(
    db_pool: &Arc<DatabasePool>,
    xmpp_config: &XmppConfig,
) -> Result<()> {
    let enabled = std::env::var("WADDLE_TEST_FIXED_ACCOUNT_ENABLED")
        .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false);
    if !enabled {
        return Ok(());
    }

    if !xmpp_config.enabled {
        anyhow::bail!("WADDLE_TEST_FIXED_ACCOUNT_ENABLED=true requires WADDLE_XMPP_ENABLED=true");
    }
    if !xmpp_config.native_auth_enabled {
        anyhow::bail!(
            "WADDLE_TEST_FIXED_ACCOUNT_ENABLED=true requires WADDLE_NATIVE_AUTH_ENABLED=true"
        );
    }

    let username = std::env::var("WADDLE_TEST_FIXED_ACCOUNT_USERNAME")
        .unwrap_or_else(|_| "admin".to_string())
        .trim()
        .to_string();
    if username.is_empty() {
        anyhow::bail!("WADDLE_TEST_FIXED_ACCOUNT_USERNAME cannot be empty");
    }

    let password = std::env::var("WADDLE_TEST_FIXED_ACCOUNT_PASSWORD")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "WADDLE_TEST_FIXED_ACCOUNT_PASSWORD must be set when WADDLE_TEST_FIXED_ACCOUNT_ENABLED=true"
            )
        })?;

    let domain = std::env::var("WADDLE_TEST_FIXED_ACCOUNT_DOMAIN")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| xmpp_config.domain.clone());
    let email = std::env::var("WADDLE_TEST_FIXED_ACCOUNT_EMAIL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    seed_fixed_test_account(
        db_pool,
        &FixedTestAccountConfig {
            username,
            password,
            domain,
            email,
        },
    )
    .await
}

async fn seed_fixed_test_account(
    db_pool: &Arc<DatabasePool>,
    config: &FixedTestAccountConfig,
) -> Result<()> {
    let native_user_store = NativeUserStore::new(db_pool.global_actor().clone());
    if native_user_store
        .user_exists(&config.username, &config.domain)
        .await
        .map_err(|err| anyhow::anyhow!("Failed checking fixed test account: {err}"))?
    {
        native_user_store
            .delete_user(&config.username, &config.domain)
            .await
            .map_err(|err| anyhow::anyhow!("Failed resetting fixed test account: {err}"))?;
    }

    native_user_store
        .register(RegisterRequest {
            username: config.username.clone(),
            domain: config.domain.clone(),
            password: config.password.clone(),
            email: config.email.clone(),
        })
        .await
        .map_err(|err| anyhow::anyhow!("Failed creating fixed test account: {err}"))?;

    info!(
        username = %config.username,
        domain = %config.domain,
        "Provisioned fixed native test account"
    );
    Ok(())
}

/// State for the server-info endpoint
#[derive(Clone)]
struct ServerInfoState {
    server_info: ServerInfo,
}

/// Configure CORS layer.
///
/// If `WADDLE_CORS_ORIGINS` is set (comma-separated list of origins),
/// only those origins are allowed. Otherwise, falls back to permissive
/// CORS (suitable for development).
fn configure_cors() -> CorsLayer {
    let origins = std::env::var("WADDLE_CORS_ORIGINS").ok();
    build_cors(origins.as_deref())
}

fn build_cors(origins: Option<&str>) -> CorsLayer {
    use tower_http::cors::AllowOrigin;

    match origins {
        Some(origins) if !origins.is_empty() => {
            let allowed: Vec<_> = origins
                .split(',')
                .filter_map(|o| o.trim().parse().ok())
                .collect();
            if allowed.is_empty() {
                warn!("WADDLE_CORS_ORIGINS set but no valid origins parsed, falling back to permissive CORS");
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
                    ])
                    .allow_credentials(true)
            }
        }
        _ => CorsLayer::permissive(),
    }
}

/// Create the Axum router with all routes and middleware.
async fn create_router(
    state: Arc<AppState>,
    server_config: ServerConfig,
    xmpp_native_auth_enabled: bool,
    mam_storage: Arc<dyn MamStorage>,
    acme_http01_challenge_service: Option<TowerHttp01ChallengeService>,
) -> Router {
    // Create auth broker state
    let encryption_key = server_config.session_key.clone();

    let auth_state = Arc::new(AuthState::new(
        state.clone(),
        &server_config,
        encryption_key.as_ref().map(|s| s.as_bytes()),
    ));

    // Create connection registry for WebSocket message routing
    let connection_registry = Arc::new(ConnectionRegistry::new());

    // Create MUC room registry with the XMPP domain (not the HTTP base_url host)
    let xmpp_domain = auth_state.xmpp_domain.clone();
    let muc_domain = format!("muc.{}", xmpp_domain);
    let room_registry = kameo::spawn(RoomRegistryActor::new(muc_domain));

    let extension_manager = Arc::new(
        match ExtensionManager::from_config(server_config.extensions.clone()).await {
            Ok(mgr) => mgr,
            Err(error) => {
                warn!(error = %error, "Failed to initialize extension manager; continuing fail-open");
                ExtensionManager::from_config(ExtensionConfig {
                    enabled: false,
                    cache_dir: String::new(),
                    modules: Vec::new(),
                })
                .await
                .expect("BUG: failed to create disabled ExtensionManager")
            }
        },
    );

    let websocket_command_registry = Arc::new(waddle_xmpp::commands::CommandRegistry::new());
    {
        use waddle_xmpp::commands::{handle_create_channel, NODE_CREATE_CHANNEL};

        let app_state_for_command = Arc::new(
            XmppAppState::new(
                xmpp_domain.clone(),
                Arc::new(state.db_pool.global().clone()),
                state.db_pool.global_actor().clone(),
                encryption_key.as_ref().map(|s| s.as_bytes()),
            )
            .with_db_pool(Arc::clone(&state.db_pool)),
        );
        websocket_command_registry
            .register(NODE_CREATE_CHANNEL, "Create Channel", move |ctx| {
                let deps = Arc::clone(&app_state_for_command);
                handle_create_channel(deps, ctx)
            })
            .await;
        info!("Registered create-channel command for WebSocket");
    }

    // Build the sans-I/O stanza dispatcher with the handlers migrated so far.
    // See `waddle_xmpp::protocol` for the state-machine design; any IQ
    // namespace registered here short-circuits the legacy string-matching
    // path in `routes::websocket::handle_iq`.
    let mut stanza_dispatcher = waddle_xmpp::protocol::StanzaDispatcher::new();
    waddle_xmpp::protocol::handlers::register_default_handlers(&mut stanza_dispatcher);
    let stanza_dispatcher = Arc::new(stanza_dispatcher);

    // Shared PubSub/PEP storage for the WebSocket transport (XEP-0060/0163).
    let pubsub_storage: Arc<dyn waddle_xmpp::pubsub::PubSubStorage> =
        Arc::new(waddle_xmpp::pubsub::InMemoryPubSubStorage::new());

    // XEP-0198 detached-session registry for stream resumption across
    // transient WebSocket drops.
    let sm_session_registry =
        Arc::new(waddle_xmpp::stream_management::InMemorySmSessionRegistry::new());
    let resumable_sessions: Arc<dashmap::DashMap<String, crate::auth::Session>> =
        Arc::new(dashmap::DashMap::new());

    // XMPP over WebSocket (RFC 7395) with registries for message routing
    let websocket_state = Arc::new(WebSocketState {
        deps: WebSocketDeps {
            app_state: state.clone(),
            auth_state: auth_state.clone(),
            protocol: ProtocolServices {
                connection_registry,
                room_registry,
                mam_storage,
                inbox_storage: Arc::clone(&state.inbox_storage),
                command_registry: websocket_command_registry,
                extension_manager,
                dispatcher: stanza_dispatcher,
                pubsub_storage,
                sm_session_registry,
                resumable_sessions,
            },
        },
    });
    // XEP-0198 expired-session janitor. Without this, detached SM sessions
    // whose resume window elapses leave MUC occupants in their rooms forever
    // (ghosts) and the `resumable_sessions` sidecar grows unbounded. Holds a
    // Weak reference so it doesn't keep the WebSocketState alive past the
    // server's lifetime.
    {
        let weak_state = Arc::downgrade(&websocket_state);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(60));
            // Skip the first tick (immediate) so we don't sweep before the
            // server has accepted any connections.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                let Some(state) = weak_state.upgrade() else {
                    break;
                };
                let drained: Vec<waddle_xmpp::stream_management::DetachedSession> = match state
                    .deps
                    .protocol
                    .sm_session_registry
                    .drain_expired()
                    .await
                {
                    Ok(sessions) => sessions,
                    Err(err) => {
                        warn!(error = %err, "SM janitor: drain_expired failed");
                        continue;
                    }
                };
                if drained.is_empty() {
                    continue;
                }
                info!(
                    count = drained.len(),
                    "SM janitor: cleaning up expired detached sessions"
                );
                for session in drained {
                    state
                        .deps
                        .protocol
                        .resumable_sessions
                        .remove(&session.stream_id);
                    state
                        .deps
                        .protocol
                        .connection_registry
                        .unregister(&session.jid);
                    routes::websocket::cleanup_muc_presence_for_jid(&state, &session.jid).await;
                }
            }
        });
    }

    let websocket_router = routes::websocket::router(websocket_state);

    // Permission router with Zanzibar-inspired permission service
    let permission_state = Arc::new(PermissionState::new(state.clone()));
    let permission_router = routes::permissions::router(permission_state);

    // Waddles router for community CRUD operations
    let waddle_state = Arc::new(WaddleState::new(
        state.clone(),
        encryption_key.as_ref().map(|s| s.as_bytes()),
        server_config.single_tenant,
    ));
    let waddles_router = routes::waddles::router(waddle_state.clone());

    // Channels router for channel CRUD operations
    let channel_state = Arc::new(ChannelState::new(
        state.clone(),
        encryption_key.as_ref().map(|s| s.as_bytes()),
    ));
    let channels_router = routes::channels::router(channel_state.clone());

    // Upload router for XEP-0363 HTTP File Upload
    let upload_state = Arc::new(UploadState::new(state.clone()));
    let upload_router = routes::uploads::router(upload_state);
    let users_router = routes::users::router(waddle_state.clone());

    // Create server info for the /api/v1/server-info endpoint
    let server_info = ServerInfo::from_config(&server_config, xmpp_native_auth_enabled);
    let server_info_state = ServerInfoState { server_info };
    // Well-known endpoints for XMPP service discovery (XEP-0156)
    let well_known_router = routes::well_known::router(auth_state.clone());

    // Build the base router with health and server-info endpoints
    let mut router = Router::new()
        .route("/health", get(health_handler))
        .route("/healthz", get(health_handler))
        .route("/ready", get(readiness_handler))
        .route("/readyz", get(readiness_handler))
        .route("/metrics", get(metrics_handler))
        .route("/api/v1/health", get(detailed_health_handler))
        .with_state(state)
        .route("/api/v1/server-info", get(server_info_handler))
        .with_state(server_info_state);

    if let Some(challenge_service) = acme_http01_challenge_service {
        router = router.route_service(
            "/.well-known/acme-challenge/:challenge_token",
            challenge_service,
        );
    }

    // Always merge auth surfaces. If no providers are configured these endpoints
    // return explicit errors.
    let auth_router = routes::auth::router(auth_state.clone());
    let device_router = routes::device::router(auth_state.clone());
    let xmpp_oauth_router = routes::xmpp_oauth::router(auth_state.clone());
    let auth_page_router = routes::auth_page::router(auth_state.clone());

    router = router
        .merge(auth_router)
        .merge(device_router)
        .merge(xmpp_oauth_router)
        .merge(auth_page_router);

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
        // Merge authenticated user search
        .merge(users_router)
        // Merge well-known endpoints for XMPP service discovery
        .merge(well_known_router)
        // Merge upload routes for XEP-0363 HTTP File Upload
        .merge(upload_router)
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(make_request_span)
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        )
        .layer(CompressionLayer::new())
        .layer(configure_cors())
}

/// Build the per-request `tracing` span and attach the inbound W3C
/// trace context (if any) as its OpenTelemetry parent.
///
/// `opentelemetry_http::HeaderExtractor` implements the `Extractor`
/// trait the propagator expects. After extraction, we only call
/// `set_parent` when the extracted span context is valid
/// (`parent_cx.span().span_context().is_valid()`), which
/// distinguishes a propagated request from an internal / non-browser
/// caller carrying no headers — the latter keeps starting a fresh
/// root span instead of being silently re-parented to whatever the
/// extractor returns for the empty case.
fn make_request_span(request: &axum::http::Request<axum::body::Body>) -> Span {
    let span = info_span!(
        "http_request",
        method = %request.method(),
        uri = %request.uri(),
        version = ?request.version(),
    );
    let parent_cx = opentelemetry::global::get_text_map_propagator(|propagator| {
        propagator.extract(&HeaderExtractor(request.headers()))
    });
    if parent_cx.span().span_context().is_valid() {
        span.set_parent(parent_cx);
    }
    span
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

/// Readiness check endpoint (for orchestrators).
///
/// Readiness is stricter than liveness and validates overall DB pool health.
async fn readiness_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
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
async fn metrics_handler() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        waddle_xmpp::prometheus::render_metrics(),
    )
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
    use axum::http::{header, Request, StatusCode};
    use base64::prelude::*;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    static ENV_MUTEX: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        ENV_MUTEX
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .expect("env mutex")
    }

    async fn create_test_state() -> Arc<AppState> {
        let config = DatabaseConfig::default();
        let pool_config = PoolConfig::default();
        let db_pool = DatabasePool::new(config, pool_config).await.unwrap();

        // Run migrations
        let runner = MigrationRunner::global();
        runner.run(db_pool.global()).await.unwrap();

        Arc::new(AppState::new(Arc::new(db_pool)))
    }

    #[test]
    fn test_xmpp_config_prefers_dedicated_database_urls() {
        let _guard = env_lock();
        for key in [
            "WADDLE_XMPP_MAM_DATABASE_URL",
            "WADDLE_XMPP_INBOX_DATABASE_URL",
            "WADDLE_DATABASE_URL",
        ] {
            std::env::remove_var(key);
        }

        std::env::set_var("WADDLE_DATABASE_URL", "postgres://main/runtime");
        std::env::set_var("WADDLE_XMPP_MAM_DATABASE_URL", "postgres://mam/runtime");
        std::env::set_var("WADDLE_XMPP_INBOX_DATABASE_URL", "postgres://inbox/runtime");

        let config = XmppConfig::from_env();
        assert_eq!(
            config.mam_database_url.as_deref(),
            Some("postgres://mam/runtime")
        );
        assert_eq!(
            config.inbox_database_url.as_deref(),
            Some("postgres://inbox/runtime")
        );

        for key in [
            "WADDLE_XMPP_MAM_DATABASE_URL",
            "WADDLE_XMPP_INBOX_DATABASE_URL",
            "WADDLE_DATABASE_URL",
        ] {
            std::env::remove_var(key);
        }
    }

    #[test]
    fn test_xmpp_config_falls_back_to_main_database_url() {
        let _guard = env_lock();
        for key in [
            "WADDLE_XMPP_MAM_DATABASE_URL",
            "WADDLE_XMPP_INBOX_DATABASE_URL",
            "WADDLE_DATABASE_URL",
        ] {
            std::env::remove_var(key);
        }

        std::env::set_var("WADDLE_DATABASE_URL", "postgres://main/runtime");

        let config = XmppConfig::from_env();
        assert_eq!(
            config.mam_database_url.as_deref(),
            Some("postgres://main/runtime")
        );
        assert_eq!(
            config.inbox_database_url.as_deref(),
            Some("postgres://main/runtime")
        );

        std::env::remove_var("WADDLE_DATABASE_URL");
    }

    #[tokio::test]
    async fn test_health_endpoint() {
        let state = create_test_state().await;
        let server_config = ServerConfig::test_homeserver();
        let mam_storage = create_websocket_mam_storage(None).await.unwrap();
        let app = create_router(state, server_config, true, mam_storage, None).await;

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
    async fn test_healthz_alias_endpoint() {
        let state = create_test_state().await;
        let server_config = ServerConfig::test_homeserver();
        let mam_storage = create_websocket_mam_storage(None).await.unwrap();
        let app = create_router(state, server_config, true, mam_storage, None).await;

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_detailed_health_endpoint() {
        let state = create_test_state().await;
        let server_config = ServerConfig::test_homeserver();
        let mam_storage = create_websocket_mam_storage(None).await.unwrap();
        let app = create_router(state, server_config, true, mam_storage, None).await;

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
    async fn test_ready_endpoint() {
        let state = create_test_state().await;
        let server_config = ServerConfig::test_homeserver();
        let mam_storage = create_websocket_mam_storage(None).await.unwrap();
        let app = create_router(state, server_config, true, mam_storage, None).await;

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/ready")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "ready");
        assert_eq!(json["database"], "ready");
    }

    #[tokio::test]
    async fn test_readyz_alias_endpoint() {
        let state = create_test_state().await;
        let server_config = ServerConfig::test_homeserver();
        let mam_storage = create_websocket_mam_storage(None).await.unwrap();
        let app = create_router(state, server_config, true, mam_storage, None).await;

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/readyz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_metrics_endpoint() {
        let state = create_test_state().await;
        let server_config = ServerConfig::test_homeserver();
        let mam_storage = create_websocket_mam_storage(None).await.unwrap();
        let app = create_router(state, server_config, true, mam_storage, None).await;

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|h| h.to_str().ok()),
            Some("text/plain; version=0.0.4; charset=utf-8")
        );

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let metrics = String::from_utf8(body.to_vec()).unwrap();
        assert!(metrics.contains("waddle_connected_users"));
        assert!(metrics.contains("waddle_messages_per_second"));
        assert!(metrics.contains("waddle_room_count"));
    }

    #[tokio::test]
    async fn test_explicit_cors_allows_credentials() {
        let app = Router::new()
            .route("/health", get(|| async { StatusCode::OK }))
            .layer(build_cors(Some(
                "https://waddle.chat,http://localhost:4321",
            )));

        let response = app
            .oneshot(
                Request::builder()
                    .method("OPTIONS")
                    .uri("/health")
                    .header(header::ORIGIN, "https://waddle.chat")
                    .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert!(response.status().is_success());
        assert_eq!(
            response
                .headers()
                .get("access-control-allow-origin")
                .and_then(|value| value.to_str().ok()),
            Some("https://waddle.chat")
        );
        assert_eq!(
            response
                .headers()
                .get("access-control-allow-credentials")
                .and_then(|value| value.to_str().ok()),
            Some("true")
        );
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
        let conn = waddle_db.guard().await.unwrap();
        let mut rows = conn
            .query(
                "SELECT name FROM sqlite_master WHERE type='table' AND name='channels'",
                (),
            )
            .await
            .unwrap();

        assert!(rows.next().await.unwrap().is_some());
    }

    #[tokio::test]
    async fn test_seed_fixed_test_account_creates_user() {
        let state = create_test_state().await;
        let password = format!("fixed-account-{}", rand::random::<u64>());
        let config = FixedTestAccountConfig {
            username: "admin".to_string(),
            password: password.clone(),
            domain: "localhost".to_string(),
            email: Some("admin@localhost".to_string()),
        };

        seed_fixed_test_account(&state.db_pool, &config)
            .await
            .unwrap();

        let native_user_store = NativeUserStore::new(state.db_pool.global_actor().clone());
        assert!(native_user_store
            .user_exists(&config.username, &config.domain)
            .await
            .unwrap());
        assert!(native_user_store
            .verify_password(&config.username, &config.domain, &config.password)
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn test_seed_fixed_test_account_replaces_existing_credentials() {
        let state = create_test_state().await;
        let native_user_store = NativeUserStore::new(state.db_pool.global_actor().clone());
        let old_password = format!("fixed-account-old-{}", rand::random::<u64>());
        let new_password = format!("fixed-account-new-{}", rand::random::<u64>());

        native_user_store
            .register(RegisterRequest {
                username: "admin".to_string(),
                domain: "localhost".to_string(),
                password: old_password.clone(),
                email: None,
            })
            .await
            .unwrap();

        let config = FixedTestAccountConfig {
            username: "admin".to_string(),
            password: new_password.clone(),
            domain: "localhost".to_string(),
            email: None,
        };
        seed_fixed_test_account(&state.db_pool, &config)
            .await
            .unwrap();

        assert!(native_user_store
            .verify_password(&config.username, &config.domain, &config.password)
            .await
            .unwrap());
        assert!(!native_user_store
            .verify_password(&config.username, &config.domain, &old_password)
            .await
            .unwrap());

        let credentials = native_user_store
            .get_scram_credentials(&config.username, &config.domain)
            .await
            .unwrap()
            .expect("credentials should exist");
        let salt = BASE64_STANDARD.decode(credentials.salt_b64).unwrap();
        let (stored_key, _) = waddle_xmpp::auth::scram::generate_scram_keys(
            &config.password,
            &salt,
            credentials.iterations,
        );
        assert_eq!(credentials.stored_key, stored_key);
    }
}
