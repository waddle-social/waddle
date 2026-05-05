use crate::auth::{NativeUserStore, RegisterRequest};
use crate::config::ServerConfig;
use crate::db::actor::{DbExecute, DbQueryOne};
use crate::db::{DatabasePool, PoolHealth};
use crate::inbox::build_inbox_storage;
use crate::permissions::{
    CheckPermission, Object, ObjectType, Permission, PermissionActor, Subject,
};
use crate::pubsub::build_pubsub_storage;
use crate::server::bootstrap_membership::DEPLOYMENT_SERVER_ID;
use crate::server::managed_channel_policy::{
    server_policy_for_managed_channel, ManagedChannelServerPolicy,
    DEPLOYMENT_MEMBERSHIP_PERMISSIONS,
};
use anyhow::Result;
use async_trait::async_trait;
use axum::{
    extract::State,
    http::{header, HeaderName, Method, StatusCode},
    response::{IntoResponse, Json},
    routing::get,
    Router,
};
use futures::StreamExt;
use jid::BareJid;
use kameo::actor::ActorRef;
use opentelemetry::trace::TraceContextExt;
use opentelemetry_http::HeaderExtractor;
use routes::auth::AuthState;
use routes::uploads::UploadState;
use routes::websocket::{ProtocolServices, WebSocketDeps, WebSocketState, XmppServiceDomains};
use rustls_acme::caches::DirCache;
use rustls_acme::tower::TowerHttp01ChallengeService;
use rustls_acme::{AcmeConfig, UseChallenge};
use serde::Serialize;
use serde_json::json;
use std::{net::SocketAddr, path::PathBuf, sync::Arc};
use tokio::sync::OnceCell;
use tower_http::{
    compression::CompressionLayer,
    cors::CorsLayer,
    trace::{DefaultOnResponse, TraceLayer},
};
use tracing::{info, info_span, warn, Level, Span};
use tracing_opentelemetry::OpenTelemetrySpanExt;
use waddle_extensions::{
    host_tools as ext_host, CommandAction as ExtensionCommandAction, CommandSessionId,
    DataForm as ExtensionDataForm, DataFormField as ExtensionDataFormField,
    DataFormType as ExtensionDataFormType, DataFormValue, ExtensionConfig, ExtensionEffect,
    ExtensionManager, FormFieldOption, FormFieldType as ExtensionFormFieldType, FormFieldValue,
    FullJidValue, LaunchContext, LaunchId, PubSubPublish, RoomJid as ExtensionRoomJid, StanzaId,
    UiActionId, WaddleId, INVOKE_COMMAND_NODE,
};
use waddle_xmpp::inbox::storage::InboxStorage;
use waddle_xmpp::mam::{MamStorage, SqlxMamStorage};
use waddle_xmpp::pubsub::{NodeConfig, PubSubItem, PubSubStorage};
use waddle_xmpp::{muc::room_registry_actor::RoomRegistryActor, registry::ConnectionRegistry};

pub(crate) mod bootstrap_membership;
pub mod extension_host_adapter;
pub(crate) mod managed_channel_policy;
mod routes;
pub mod xmpp_state;

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
    http01_challenge_service: TowerHttp01ChallengeService,
}

/// Server application state
pub struct AppState {
    /// Database pool for global and per-waddle databases
    pub db_pool: Arc<DatabasePool>,
    /// Blob storage backend for file uploads (XEP-0363).
    pub blob_storage: Arc<dyn crate::storage::BlobStorage>,
    /// Shared Waddle inbox projection storage.
    pub inbox_storage: Arc<dyn InboxStorage>,
    /// Shared permission actor handle.
    pub permission_actor: ActorRef<PermissionActor>,
    /// Bare JIDs of server owners (resolved from
    /// `WADDLE_SERVER_OWNER_LOCALPARTS` + the XMPP user-bearing domain at
    /// startup). Used to seed `Affiliation::Owner` rows on Spaces PubSub
    /// nodes so XEP-0060 admin operations work for these accounts.
    pub server_owner_jids: Arc<[BareJid]>,
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
        let permission_actor = kameo::spawn(PermissionActor::new_for_tests(Arc::new(
            db_pool.global().clone(),
        )));
        Self::new_with_deps(
            db_pool,
            blob_storage,
            Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new()),
            permission_actor,
            Arc::from(Vec::<BareJid>::new()),
        )
    }

    pub fn new_with_deps(
        db_pool: Arc<DatabasePool>,
        blob_storage: Arc<dyn crate::storage::BlobStorage>,
        inbox_storage: Arc<dyn InboxStorage>,
        permission_actor: ActorRef<PermissionActor>,
        server_owner_jids: Arc<[BareJid]>,
    ) -> Self {
        Self {
            db_pool,
            blob_storage,
            inbox_storage,
            permission_actor,
            server_owner_jids,
        }
    }
}

/// Resolve `WADDLE_SERVER_OWNER_LOCALPARTS` localparts into bare JIDs against
/// `xmpp_domain`. Bad localparts produce a `warn!` and are skipped; they do
/// not block startup.
pub fn resolve_server_owner_jids(
    config: &bootstrap_membership::BootstrapMembershipConfig,
    xmpp_domain: &str,
) -> Arc<[BareJid]> {
    let mut jids = Vec::new();
    for localpart in config.owner_localparts() {
        let raw = format!("{localpart}@{xmpp_domain}");
        match raw.parse::<BareJid>() {
            Ok(jid) => jids.push(jid),
            Err(error) => warn!(
                localpart = %localpart,
                xmpp_domain = %xmpp_domain,
                error = %error,
                "skipping invalid server-owner localpart for spaces affiliation seeding",
            ),
        }
    }
    Arc::from(jids)
}

#[derive(Default)]
struct DeferredExtensionHostTools {
    inner: OnceCell<Arc<dyn ext_host::ExtensionHostTools>>,
}

impl DeferredExtensionHostTools {
    fn set(&self, tools: Arc<dyn ext_host::ExtensionHostTools>) {
        let _ = self.inner.set(tools);
    }

    fn tools(
        &self,
    ) -> std::result::Result<&Arc<dyn ext_host::ExtensionHostTools>, ext_host::HostToolError> {
        self.inner.get().ok_or_else(|| ext_host::HostToolError {
            code: ext_host::HostToolErrorCode::Unsupported,
            message: waddle_extensions::DisplayText::new(
                "extension host tools are not wired into the server",
            )
            .expect("static host-tool error is non-empty"),
        })
    }
}

#[async_trait]
impl ext_host::ExtensionHostTools for DeferredExtensionHostTools {
    async fn list_channels(
        &self,
        context: &ext_host::InvocationContext,
        request: ext_host::ListChannelsRequest,
    ) -> std::result::Result<ext_host::ListChannelsResponse, ext_host::HostToolError> {
        self.tools()?.list_channels(context, request).await
    }

    async fn list_spaces(
        &self,
        context: &ext_host::InvocationContext,
        request: ext_host::ListSpacesRequest,
    ) -> std::result::Result<ext_host::ListSpacesResponse, ext_host::HostToolError> {
        self.tools()?.list_spaces(context, request).await
    }

    async fn list_room_members(
        &self,
        context: &ext_host::InvocationContext,
        request: ext_host::ListRoomMembersRequest,
    ) -> std::result::Result<ext_host::ListRoomMembersResponse, ext_host::HostToolError> {
        self.tools()?.list_room_members(context, request).await
    }

    async fn get_presence(
        &self,
        context: &ext_host::InvocationContext,
        request: ext_host::GetPresenceRequest,
    ) -> std::result::Result<ext_host::GetPresenceResponse, ext_host::HostToolError> {
        self.tools()?.get_presence(context, request).await
    }

    async fn get_roster(
        &self,
        context: &ext_host::InvocationContext,
        request: ext_host::GetRosterRequest,
    ) -> std::result::Result<ext_host::GetRosterResponse, ext_host::HostToolError> {
        self.tools()?.get_roster(context, request).await
    }

    async fn query_mam(
        &self,
        context: &ext_host::InvocationContext,
        query: ext_host::MamQuery,
    ) -> std::result::Result<ext_host::MamQueryResponse, ext_host::HostToolError> {
        self.tools()?.query_mam(context, query).await
    }

    async fn send_message(
        &self,
        context: &ext_host::InvocationContext,
        request: ext_host::SendMessageRequest,
    ) -> std::result::Result<ext_host::SendMessageResponse, ext_host::HostToolError> {
        self.tools()?.send_message(context, request).await
    }
}

/// XMPP server configuration loaded from environment variables.
#[derive(Debug, Clone)]
pub struct XmppConfig {
    /// Whether XMPP server is enabled (default: true)
    pub enabled: bool,
    /// XMPP server domain (default: "localhost")
    pub domain: String,
    /// Parent domain for XMPP component services such as MUC and Spaces.
    pub component_domain: String,
    /// MAM database URL (prefers dedicated XMPP DSN, otherwise the main runtime DSN)
    pub mam_database_url: Option<String>,
    /// Inbox database URL (prefers dedicated XMPP DSN, otherwise the main runtime DSN)
    pub inbox_database_url: Option<String>,
    /// XEP-0160 offline-message (`pending_delivery`) database URL —
    /// prefers dedicated XMPP DSN, otherwise the main runtime DSN.
    /// Resolution order (matches `resolve_xmpp_database_url`):
    /// `WADDLE_XMPP_PENDING_DELIVERY_DATABASE_URL` →
    /// `WADDLE_DATABASE_URL`. When neither is set the storage falls
    /// back to in-memory SQLite — suitable only for tests; production
    /// deployments MUST set one of these env vars so queued offline
    /// DMs survive restart per issue #209.
    pub pending_delivery_database_url: Option<String>,
    /// XEP-0198 stream-management persistence database URL —
    /// prefers dedicated XMPP DSN, otherwise the main runtime DSN.
    /// Resolution order:
    /// `WADDLE_XMPP_SM_DATABASE_URL` → `WADDLE_DATABASE_URL`. When
    /// unset the storage falls back to in-memory SQLite — suitable
    /// for tests; production deployments MUST set one of these env
    /// vars so detached sessions survive restart per issue #209
    /// slice (d) Q8 = B.
    pub sm_database_url: Option<String>,
    /// PubSub/PEP database URL (prefers dedicated XMPP DSN, otherwise the main runtime DSN)
    pub pubsub_database_url: Option<String>,
    /// Whether native JID authentication is enabled (default: true)
    /// When enabled, users can authenticate with SCRAM-SHA-256 using native credentials.
    pub native_auth_enabled: bool,
    /// ACME configuration for managed TLS certificates.
    pub acme: XmppAcmeConfig,
}

impl Default for XmppConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            domain: "localhost".to_string(),
            component_domain: "localhost".to_string(),
            mam_database_url: None,
            inbox_database_url: None,
            pending_delivery_database_url: None,
            sm_database_url: None,
            pubsub_database_url: None,
            native_auth_enabled: true,
            acme: XmppAcmeConfig {
                enabled: false,
                email: None,
                cache_dir: PathBuf::from("certs/acme-cache"),
                production: false,
            },
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
        let component_domain =
            std::env::var("WADDLE_XMPP_COMPONENT_DOMAIN").unwrap_or_else(|_| domain.clone());

        let mam_database_url = resolve_xmpp_database_url("WADDLE_XMPP_MAM_DATABASE_URL");
        let inbox_database_url = resolve_xmpp_database_url("WADDLE_XMPP_INBOX_DATABASE_URL");
        let pending_delivery_database_url =
            resolve_xmpp_database_url("WADDLE_XMPP_PENDING_DELIVERY_DATABASE_URL");
        let sm_database_url = resolve_xmpp_database_url("WADDLE_XMPP_SM_DATABASE_URL");
        let pubsub_database_url = resolve_xmpp_database_url("WADDLE_XMPP_PUBSUB_DATABASE_URL");

        let native_auth_enabled = std::env::var("WADDLE_NATIVE_AUTH_ENABLED")
            .map(|v| v.to_lowercase() != "false" && v != "0")
            .unwrap_or(true);

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

        Self {
            enabled,
            domain,
            component_domain,
            mam_database_url,
            inbox_database_url,
            pending_delivery_database_url,
            sm_database_url,
            pubsub_database_url,
            native_auth_enabled,
            acme: XmppAcmeConfig {
                enabled: acme_enabled,
                email: acme_email,
                cache_dir: acme_cache_dir,
                production: acme_production,
            },
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

async fn bootstrap_fresh_xmpp_topology(
    state: &Arc<AppState>,
    pubsub_storage: Arc<dyn PubSubStorage>,
    services: &XmppServiceDomains,
) -> Result<()> {
    let spaces_jid: jid::BareJid = services
        .spaces
        .parse()
        .map_err(|error| anyhow::anyhow!("invalid spaces service JID: {error}"))?;

    let actor = state.db_pool.global_actor().clone();
    let row = actor
        .ask(DbQueryOne {
            sql: "SELECT COUNT(*) FROM channels".to_string(),
            params: vec![],
        })
        .await
        .map_err(|error| anyhow::anyhow!("failed to count channels: {error}"))?;
    let channel_count = row
        .as_ref()
        .and_then(|row| row.first())
        .and_then(|value| match value {
            crate::db::Value::Integer(value) => Some(*value),
            _ => None,
        })
        .unwrap_or(0);
    let should_seed_db = channel_count == 0;
    let should_seed_pubsub = should_seed_db
        || (actor
            .ask(DbQueryOne {
                sql: "SELECT 1 FROM channels WHERE id = 'chat'".to_string(),
                params: vec![],
            })
            .await
            .map_err(|error| anyhow::anyhow!("failed to inspect chat channel: {error}"))?
            .is_some()
            && actor
                .ask(DbQueryOne {
                    sql: "SELECT 1 FROM channels WHERE id = 'announcements'".to_string(),
                    params: vec![],
                })
                .await
                .map_err(|error| {
                    anyhow::anyhow!("failed to inspect announcements channel: {error}")
                })?
                .is_some());

    if should_seed_pubsub {
        seed_initial_xmpp_topology(
            &actor,
            &pubsub_storage,
            services,
            &spaces_jid,
            should_seed_db,
        )
        .await?;
    }

    seed_spaces_admin_affiliations(&pubsub_storage, &spaces_jid, &state.server_owner_jids).await;

    Ok(())
}

async fn seed_initial_xmpp_topology(
    actor: &kameo::actor::ActorRef<crate::db::actor::DbActor>,
    pubsub_storage: &Arc<dyn PubSubStorage>,
    services: &XmppServiceDomains,
    spaces_jid: &BareJid,
    should_seed_db: bool,
) -> Result<()> {
    if should_seed_db {
        let now = chrono::Utc::now().to_rfc3339();
        for (id, name, description, position, is_default, channel_type) in [
            ("chat", "Chat", "General member chat", 0_i64, 1_i64, "text"),
            (
                "announcements",
                "Announcements",
                "Owner-posted announcements",
                1_i64,
                0_i64,
                "announcement",
            ),
        ] {
            actor
                .ask(DbExecute {
                    sql: r#"
                        INSERT INTO channels (id, name, description, channel_type, position, is_default, created_at, updated_at)
                        VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                        ON CONFLICT(id) DO NOTHING
                    "#
                    .to_string(),
                    params: vec![
                        id.into(),
                        name.into(),
                        description.into(),
                        channel_type.into(),
                        position.into(),
                        is_default.into(),
                        now.clone().into(),
                        now.clone().into(),
                    ],
                })
                .await
                .map_err(|error| anyhow::anyhow!("failed to seed channel {id}: {error}"))?;
        }
    }

    pubsub_storage
        .get_or_create_node(spaces_jid, "general")
        .await
        .map_err(|error| anyhow::anyhow!("failed to create General space node: {error}"))?;
    pubsub_storage
        .update_node_config(spaces_jid, "general", &NodeConfig::spaces_public())
        .await
        .map_err(|error| anyhow::anyhow!("failed to configure General space node: {error}"))?;

    for (id, name) in [("chat", "Chat"), ("announcements", "Announcements")] {
        let room_jid = waddle_xmpp::managed_room_jid(id, &services.muc)
            .map_err(|error| anyhow::anyhow!("invalid seeded room JID: {error}"))?;
        let bookmark = waddle_xmpp::xep::xep0402::Bookmark::new(room_jid)
            .with_name(name)
            .with_autojoin(id == "chat");
        let item = PubSubItem {
            id: Some(bookmark.jid.to_string()),
            publisher: None,
            payload: Some(waddle_xmpp::xep::xep0402::build_bookmark_element(&bookmark)),
        };
        pubsub_storage
            .publish_item(spaces_jid, "general", &item, Some(spaces_jid), false)
            .await
            .map_err(|error| anyhow::anyhow!("failed to publish {name} bookmark: {error}"))?;
    }

    info!(
        muc = %services.muc,
        spaces = %services.spaces,
        "Seeded fresh XMPP General Space with Chat and Announcements MUCs"
    );
    Ok(())
}

/// Mirror server-owner permissions into `Affiliation::Owner` rows on every
/// existing Spaces PubSub node so XEP-0060 admin operations
/// (`<configure/>`, `<purge/>`, `<affiliations/>`) succeed for accounts in
/// `WADDLE_SERVER_OWNER_LOCALPARTS`. Per-entity failures are logged and do
/// not abort the batch.
async fn seed_spaces_admin_affiliations(
    pubsub_storage: &Arc<dyn PubSubStorage>,
    spaces_jid: &BareJid,
    server_owner_jids: &[BareJid],
) {
    if server_owner_jids.is_empty() {
        return;
    }
    let nodes = match pubsub_storage.list_nodes(spaces_jid).await {
        Ok(nodes) => nodes,
        Err(error) => {
            warn!(
                spaces = %spaces_jid,
                error = %error,
                "failed to enumerate Spaces nodes for server-owner affiliation seed",
            );
            return;
        }
    };
    for node in &nodes {
        crate::spaces_pubsub_seed::seed_owners_on_node(
            pubsub_storage,
            spaces_jid,
            node,
            server_owner_jids,
        )
        .await;
    }
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
    // Set up Ecdysis graceful shutdown coordinator
    let shutdown = waddle_ecdysis::GracefulShutdown::from_env();
    let stop_token = shutdown.stop_token();

    // Acquire listeners: inherited from parent process, or bind fresh.
    // Two explicit paths — no silent fallback.
    let http_listener = if let Some(ref mut set) = inherited {
        // Ecdysis restart path: all listeners MUST be inherited
        set.take("http")
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

        http
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
    let global_db = Arc::new(db_pool.global().clone());
    let permission_actor_impl = if server_config.spicedb.is_none() && fixed_test_account_enabled() {
        PermissionActor::new_for_tests(Arc::clone(&global_db))
    } else {
        PermissionActor::from_server_config(&server_config).await?
    };
    info!(
        backend = permission_actor_impl.backend_name(),
        "Permission backend configured"
    );
    let permission_actor = kameo::spawn(permission_actor_impl);
    permission_actor
        .ask(crate::permissions::EnsureSchema)
        .await
        .map_err(|error| anyhow::anyhow!("Failed to ensure permission schema: {}", error))?;
    bootstrap_membership::reconcile_existing_accounts_or_warn(
        db_pool.global_actor(),
        &permission_actor,
        &bootstrap_membership::BootstrapMembershipConfig::from_env(),
    )
    .await;
    let inbox_storage = build_inbox_storage(xmpp_config.inbox_database_url.clone())
        .await
        .map_err(|error| anyhow::anyhow!("Failed to initialize inbox storage: {}", error))?;

    // Create HTTP state (shares db_pool via Arc)
    let blob_storage = crate::storage::build_blob_storage()
        .map_err(|e| anyhow::anyhow!("Failed to initialize blob storage: {}", e))?;
    let server_owner_jids = resolve_server_owner_jids(
        &bootstrap_membership::BootstrapMembershipConfig::from_env(),
        &xmpp_config.domain,
    );
    let state = Arc::new(AppState::new_with_deps(
        Arc::clone(&db_pool),
        blob_storage,
        Arc::clone(&inbox_storage),
        permission_actor.clone(),
        server_owner_jids,
    ));
    let websocket_mam_storage =
        create_websocket_mam_storage(xmpp_config.mam_database_url.clone()).await?;
    let acme_runtime = start_acme_runtime(&xmpp_config, stop_token.clone());

    // Start HTTP server
    let http_state = state.clone();
    let http_mam_storage = websocket_mam_storage.clone();
    let http_server_config = server_config.clone();
    let http_xmpp_config = xmpp_config.clone();
    let http_stop = stop_token.clone();
    let acme_http01_challenge_service = acme_runtime
        .as_ref()
        .map(|runtime| runtime.http01_challenge_service.clone());
    let http_handle = tokio::spawn(async move {
        start_http_server(HttpServerDeps {
            state: http_state,
            server_config: http_server_config,
            xmpp_config: http_xmpp_config,
            mam_storage: http_mam_storage,
            acme_http01_challenge_service,
            listener: http_listener,
            stop_token: http_stop,
        })
        .await
    });

    info!("TCP XMPP listener disabled; serving WebSocket C2S only");

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
    xmpp_config: XmppConfig,
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
        xmpp_config,
        mam_storage,
        acme_http01_challenge_service,
        listener,
        stop_token,
    } = deps;

    let app = create_router(
        state,
        server_config,
        xmpp_config,
        mam_storage,
        acme_http01_challenge_service,
    )
    .await?;

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

fn fixed_test_account_enabled() -> bool {
    std::env::var("WADDLE_TEST_FIXED_ACCOUNT_ENABLED")
        .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

async fn ensure_fixed_test_account(
    db_pool: &Arc<DatabasePool>,
    xmpp_config: &XmppConfig,
) -> Result<()> {
    let enabled = fixed_test_account_enabled();
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
            domain: domain.clone(),
            email,
        },
    )
    .await?;

    if let Ok(extra_accounts) = std::env::var("WADDLE_TEST_EXTRA_FIXED_ACCOUNTS") {
        for account in extra_accounts
            .split(',')
            .filter(|entry| !entry.trim().is_empty())
        {
            let Some((username, password)) = account.split_once(':') else {
                anyhow::bail!("WADDLE_TEST_EXTRA_FIXED_ACCOUNTS entries must be username:password");
            };
            seed_fixed_test_account(
                db_pool,
                &FixedTestAccountConfig {
                    username: username.trim().to_string(),
                    password: password.trim().to_string(),
                    domain: domain.clone(),
                    email: None,
                },
            )
            .await?;
        }
    }

    Ok(())
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
    xmpp_config: XmppConfig,
    mam_storage: Arc<dyn MamStorage>,
    acme_http01_challenge_service: Option<TowerHttp01ChallengeService>,
) -> Result<Router> {
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
    let service_domains = XmppServiceDomains::new(&xmpp_domain, &xmpp_config.component_domain);
    let room_registry = kameo::spawn(RoomRegistryActor::new(
        service_domains.muc.clone(),
        server_config.occupant_id_secret.clone(),
    ));

    let extension_launch_key = server_config
        .session_key
        .clone()
        .unwrap_or_else(|| format!("development-extension-launch-key:{xmpp_domain}"));
    let deferred_extension_host_tools = Arc::new(DeferredExtensionHostTools::default());
    let extension_manager = Arc::new(
        match ExtensionManager::from_config_with_host_tools(
            server_config.extensions.clone(),
            Arc::clone(&deferred_extension_host_tools) as Arc<dyn ext_host::ExtensionHostTools>,
        )
        .await
        {
            Ok(mgr) => mgr.with_launch_signing_key(extension_launch_key.as_bytes()),
            Err(error) => {
                if server_config.extensions.enabled && !server_config.extensions.modules.is_empty()
                {
                    return Err(anyhow::anyhow!(
                        "failed to initialize configured extensions: {error}"
                    ));
                }
                warn!(error = %error, "Failed to initialize disabled extension manager; continuing without extensions");
                ExtensionManager::from_config_with_host_tools(
                    ExtensionConfig {
                        enabled: false,
                        cache_dir: String::new(),
                        modules: Vec::new(),
                    },
                    Arc::clone(&deferred_extension_host_tools)
                        as Arc<dyn ext_host::ExtensionHostTools>,
                )
                .await
                .map(|mgr| mgr.with_launch_signing_key(extension_launch_key.as_bytes()))
                .expect("BUG: failed to create disabled ExtensionManager")
            }
        },
    );

    let websocket_command_registry = Arc::new(waddle_xmpp::commands::CommandRegistry::new());

    // Build the sans-I/O stanza dispatcher with the handlers migrated so far.
    // See `waddle_xmpp::protocol` for the state-machine design; any IQ
    // namespace registered here short-circuits the legacy string-matching
    // path in `routes::websocket::handle_iq`.
    let mut stanza_dispatcher = waddle_xmpp::protocol::StanzaDispatcher::new();
    waddle_xmpp::protocol::handlers::register_default_handlers(&mut stanza_dispatcher);
    waddle_xmpp::protocol::handlers::register_default_message_handlers(&mut stanza_dispatcher);
    let stanza_dispatcher = Arc::new(stanza_dispatcher);

    // Shared durable PubSub/PEP storage for the WebSocket transport (XEP-0060/0163).
    let pubsub_storage = build_pubsub_storage(xmpp_config.pubsub_database_url.clone()).await?;
    let extension_pubsub_owner: jid::BareJid = service_domains.extensions.parse()?;
    register_extension_commands(
        Arc::clone(&extension_manager),
        Arc::clone(&websocket_command_registry),
        Arc::clone(&pubsub_storage),
        extension_pubsub_owner,
        Arc::clone(&state),
    )
    .await;
    if let Err(error) =
        bootstrap_fresh_xmpp_topology(&state, Arc::clone(&pubsub_storage), &service_domains).await
    {
        warn!(error = %error, "Failed to bootstrap fresh XMPP topology");
    }
    let push_store: Arc<dyn waddle_xmpp::push::PushSubscriptionStore> =
        Arc::new(waddle_xmpp::push::InMemoryPushStore::new());

    // XEP-0198 detached-session registry for stream resumption across
    // transient WebSocket drops. Backed by `DatabaseSmPersistence` so
    // detached sessions and their unacked queues survive restart per
    // locked Q8 = B (issue #209 slice d).
    let sm_database_url = xmpp_config.sm_database_url.clone();
    if sm_database_url.is_none() {
        warn!(
            "Neither WADDLE_XMPP_SM_DATABASE_URL nor WADDLE_DATABASE_URL is set; \
             falling back to in-memory SQLite for SM session persistence. \
             Detached XEP-0198 sessions will NOT survive restart. Set one of \
             these env vars for durable session resumption (issue #209)."
        );
    }
    let sm_persistence: Arc<dyn waddle_xmpp::stream_management::persistence::SmPersistenceStorage> =
        Arc::new(
            crate::sm_persistence::DatabaseSmPersistence::open(sm_database_url.as_deref())
                .await
                .expect("open SM persistence storage"),
        );
    let sm_session_registry = waddle_xmpp::stream_management::InMemorySmSessionRegistry::new()
        .with_persistence(Arc::clone(&sm_persistence));
    // Don't panic on a top-level restore error — `restore_from_persistence`
    // already skips individual corrupt rows internally; a top-level
    // failure here would only surface for catastrophic storage outages
    // (table unreadable, etc.). Log and continue with an empty in-memory
    // view; clients reconnecting after restart will see `<failed/>` on
    // resume and fall back to fresh sessions (XEP-0198 §5). Was
    // previously `expect()` which turned one bad row into a boot
    // failure — Qodo review on PR #344.
    if let Err(error) = sm_session_registry.restore_from_persistence().await {
        warn!(
            error = %error,
            "restore_from_persistence failed at startup; continuing with empty \
             in-memory SM session view. XEP-0198 resume will return <failed/> \
             until storage health is restored."
        );
    }
    let sm_session_registry = Arc::new(sm_session_registry);
    let resumable_sessions: Arc<dashmap::DashMap<String, crate::auth::Session>> =
        Arc::new(dashmap::DashMap::new());

    // Shared XEP-0191 blocking-list storage for the headless
    // offline-recipient pass (#229 PR15) and any future per-session
    // bind path that wants to pull a recipient's blocklist via the
    // protocol-side trait rather than the concrete struct.
    let blocking_storage: Arc<dyn waddle_xmpp::xep::xep0191::BlockingStorage> = Arc::new(
        crate::db::blocking::DatabaseBlockingStorage::new(state.db_pool.global().clone()),
    );

    // XEP-0160 offline-message storage. Open the SQLite/Postgres-backed
    // PendingDeliveryStorage so XMPP DMs to fully-offline local users
    // are durably queued and replayed on reconnect (issue #209).
    //
    // URL resolution (via `resolve_xmpp_database_url`):
    //   WADDLE_XMPP_PENDING_DELIVERY_DATABASE_URL → WADDLE_DATABASE_URL
    // When neither is set we fall back to in-memory SQLite — which
    // loses rows on restart; warn loudly so operators see the
    // deployment misconfiguration.
    let pending_delivery_url = xmpp_config.pending_delivery_database_url.clone();
    if pending_delivery_url.is_none() {
        warn!(
            "Neither WADDLE_XMPP_PENDING_DELIVERY_DATABASE_URL nor \
             WADDLE_DATABASE_URL is set; falling back to in-memory SQLite. \
             Offline DMs queued via XEP-0160 will NOT survive restart. \
             Set one of these env vars to a SQLite path or Postgres URL \
             for durable offline delivery (issue #209)."
        );
    }
    let pending_delivery_storage: Arc<
        dyn waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage,
    > = Arc::new(
        crate::pending_delivery::DatabasePendingDeliveryStorage::open(
            pending_delivery_url.as_deref(),
            waddle_xmpp::pending_delivery::QuotaPolicy::default_policy(),
        )
        .await
        .expect("open pending_delivery storage"),
    );

    // XMPP over WebSocket (RFC 7395) with registries for message routing
    let websocket_state = Arc::new(WebSocketState {
        deps: WebSocketDeps {
            app_state: state.clone(),
            auth_state: auth_state.clone(),
            service_domains,
            protocol: ProtocolServices {
                connection_registry,
                room_registry,
                mam_storage,
                inbox_storage: Arc::clone(&state.inbox_storage),
                blocking_storage,
                pending_delivery_storage,
                command_registry: websocket_command_registry,
                extension_manager,
                dispatcher: stanza_dispatcher,
                pubsub_storage,
                push_store,
                isr_token_store: waddle_xmpp::isr::create_shared_store(),
                sm_session_registry,
                resumable_sessions,
            },
            occupant_id_secret: server_config.occupant_id_secret.clone(),
        },
    });
    deferred_extension_host_tools.set(Arc::new(extension_host_adapter::ExtensionHostAdapter::new(
        Arc::clone(&websocket_state),
    )));
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
                    if session.presence_available {
                        routes::websocket::handlers::presence::broadcast_unavailable_for_expired_detached_session(
                            &state,
                            &session.jid,
                        )
                        .await;
                    }
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

    // Upload router for XEP-0363 HTTP File Upload
    let upload_state = Arc::new(UploadState::new(state.clone()));
    let upload_router = routes::uploads::router(upload_state);

    // Well-known endpoints for XMPP service discovery (XEP-0156)
    let well_known_router = routes::well_known::router(auth_state.clone());

    // Build the base router with operational health endpoints.
    let mut router = Router::new()
        .route("/health", get(health_handler))
        .route("/healthz", get(health_handler))
        .route("/ready", get(readiness_handler))
        .route("/readyz", get(readiness_handler))
        .route("/metrics", get(metrics_handler))
        .route("/api/v1/health", get(detailed_health_handler))
        .with_state(state);

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

    // Always merge common routes required by XMPP, auth, upload, and operations.
    let router = router
        // Merge XMPP over WebSocket endpoint
        .merge(websocket_router)
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
        .layer(configure_cors());
    Ok(router)
}

async fn register_extension_commands(
    extension_manager: Arc<ExtensionManager>,
    command_registry: Arc<waddle_xmpp::commands::CommandRegistry>,
    pubsub_storage: Arc<dyn PubSubStorage>,
    extension_pubsub_owner: jid::BareJid,
    app_state: Arc<AppState>,
) {
    let launch_manager = Arc::clone(&extension_manager);
    let launch_storage = Arc::clone(&pubsub_storage);
    let launch_owner = extension_pubsub_owner.clone();
    let launch_app_state = Arc::clone(&app_state);
    command_registry
        .register(INVOKE_COMMAND_NODE, "Invoke extension action", move |ctx| {
            let manager = Arc::clone(&launch_manager);
            let storage = Arc::clone(&launch_storage);
            let owner = launch_owner.clone();
            let app_state = Arc::clone(&launch_app_state);
            async move {
                let submitted_form = ctx.command.form.as_ref();
                let fields = extension_command_fields(submitted_form);
                let Some(plugin) = extension_field_value(&fields, "plugin")
                    .or_else(|| extension_field_value(&fields, "waddle#plugin_id"))
                else {
                    return extension_warning_result("Extension launch is missing plugin");
                };
                let Some(launch) = extension_field_value(&fields, "launch-id")
                    .or_else(|| extension_field_value(&fields, "waddle#launch_id"))
                else {
                    return extension_warning_result("Extension launch is missing launch-id");
                };
                let Some(action_id) = extension_field_value(&fields, "action")
                    .or_else(|| extension_field_value(&fields, "waddle#action_id"))
                else {
                    return extension_warning_result("Extension launch is missing action id");
                };
                let Some(launch_token) = extension_field_value(&fields, "launch-token")
                    .or_else(|| extension_field_value(&fields, "waddle#launch_token"))
                else {
                    return extension_warning_result("Extension launch is missing launch token");
                };
                let waddle_id = extension_field_value(&fields, "waddle-id")
                    .or_else(|| extension_field_value(&fields, "waddle#waddle_id"))
                    .unwrap_or_else(|| ctx.from.to_string());
                let Ok(launch_id) = LaunchId::new(launch) else {
                    return extension_warning_result("Extension launch id is invalid");
                };
                let Ok(waddle_id) = WaddleId::new(waddle_id) else {
                    return extension_warning_result("Extension launch waddle id is invalid");
                };
                let room = extension_field_value(&fields, "room")
                    .or_else(|| extension_field_value(&fields, "waddle#room_jid"))
                    .and_then(|value| ExtensionRoomJid::new(value).ok());
                let source_stanza_id = extension_field_value(&fields, "source-stanza-id")
                    .or_else(|| extension_field_value(&fields, "waddle#message_stanza_id"))
                    .and_then(|value| StanzaId::new(value).ok());
                let expires_at = extension_field_value(&fields, "expires-at")
                    .or_else(|| extension_field_value(&fields, "waddle#expires_at"))
                    .and_then(|value| waddle_extensions::Timestamp::new(value).ok());
                let context = LaunchContext {
                    waddle_id,
                    room,
                    source_stanza_id,
                };
                if !manager.validates_launch_invocation(
                    waddle_extensions::manager::LaunchValidationRequest {
                        plugin_name: &plugin,
                        action_id: &action_id,
                        launch_id: &launch_id,
                        context: &context,
                        fields: &fields,
                        expires_at: expires_at.as_ref(),
                        launch_token: &launch_token,
                    },
                ) {
                    return extension_warning_result(
                        "Extension launch token is missing, expired, or invalid",
                    );
                }
                let effects = manager
                    .invoke_launch(waddle_extensions::manager::LaunchInvocationRequest {
                        plugin_name: &plugin,
                        action_id: &action_id,
                        launch_id,
                        context,
                        requester: FullJidValue::new(ctx.from.to_string())
                            .expect("requester JID string is non-empty"),
                        session_id: extension_session_id(ctx.command.session_id),
                        action: ctx.command.action.map(extension_command_action),
                        fields,
                        form: submitted_form.and_then(extension_data_form),
                        expires_at,
                        launch_token: &launch_token,
                    })
                    .await;
                extension_command_result(
                    effects,
                    Some(ExtensionPubSubContext {
                        storage,
                        owner,
                        app_state,
                        extension_manager: manager,
                        authenticated_user_id: ctx.authenticated_user_id,
                    }),
                )
                .await
            }
        })
        .await;

    for (node, name) in extension_manager.command_nodes() {
        let manager = Arc::clone(&extension_manager);
        let storage = Arc::clone(&pubsub_storage);
        let owner = extension_pubsub_owner.clone();
        let app_state = Arc::clone(&app_state);
        let registered_node = node.clone();
        command_registry
            .register(node, name, move |ctx| {
                let manager = Arc::clone(&manager);
                let storage = Arc::clone(&storage);
                let owner = owner.clone();
                let app_state = Arc::clone(&app_state);
                let registered_node = registered_node.clone();
                async move {
                    let waddle_id = match WaddleId::new(ctx.from.to_string()) {
                        Ok(value) => value,
                        Err(error) => {
                            return waddle_xmpp::commands::CommandResult::Completed {
                                form: None,
                                notes: vec![waddle_xmpp::commands::Note::warn(format!(
                                    "Invalid requester JID: {error}"
                                ))],
                            };
                        }
                    };
                    let submitted_form = ctx.command.form.as_ref();
                    let fields = extension_command_fields(submitted_form);
                    let room = extension_field_value(&fields, "room")
                        .or_else(|| extension_field_value(&fields, "waddle#room_jid"))
                        .and_then(|value| ExtensionRoomJid::new(value).ok());
                    let effects = manager
                        .invoke_command(waddle_extensions::manager::CommandInvocationRequest {
                            node: &registered_node,
                            waddle_id,
                            room,
                            requester: match waddle_extensions::FullJidValue::new(
                                ctx.from.to_string(),
                            ) {
                                Ok(value) => value,
                                Err(error) => {
                                    return waddle_xmpp::commands::CommandResult::Completed {
                                        form: None,
                                        notes: vec![waddle_xmpp::commands::Note::warn(format!(
                                            "Invalid requester JID: {error}"
                                        ))],
                                    };
                                }
                            },
                            session_id: extension_session_id(ctx.command.session_id),
                            action: ctx.command.action.map(extension_command_action),
                            fields,
                            form: submitted_form.and_then(extension_data_form),
                        })
                        .await;
                    extension_command_result(
                        effects,
                        Some(ExtensionPubSubContext {
                            storage,
                            owner,
                            app_state,
                            extension_manager: manager,
                            authenticated_user_id: ctx.authenticated_user_id,
                        }),
                    )
                    .await
                }
            })
            .await;
    }
}

fn extension_command_fields(
    form: Option<&waddle_xmpp::xep::xep0004::DataForm>,
) -> Vec<FormFieldValue> {
    form.map(|form| {
        form.fields
            .iter()
            .filter_map(|field| {
                let name = UiActionId::new(field.var.clone()?).ok()?;
                let values = field
                    .values
                    .iter()
                    .map(|value| DataFormValue::new(value.clone()))
                    .collect();
                Some(FormFieldValue { name, values })
            })
            .collect()
    })
    .unwrap_or_default()
}

fn extension_field_value(fields: &[FormFieldValue], name: &str) -> Option<String> {
    fields
        .iter()
        .find(|field| field.name.as_str() == name)
        .and_then(|field| field.values.first())
        .map(|value| value.as_str().to_string())
}

fn extension_data_form(form: &waddle_xmpp::xep::xep0004::DataForm) -> Option<ExtensionDataForm> {
    Some(ExtensionDataForm {
        form_type: extension_data_form_type(form.form_type),
        title: form
            .title
            .clone()
            .and_then(|title| waddle_extensions::DisplayText::new(title).ok()),
        instructions: form
            .instructions
            .iter()
            .filter_map(|instruction| waddle_extensions::DisplayText::new(instruction.clone()).ok())
            .collect(),
        fields: form
            .fields
            .iter()
            .filter_map(extension_data_form_field)
            .collect(),
    })
}

fn extension_data_form_type(
    form_type: waddle_xmpp::xep::xep0004::FormType,
) -> ExtensionDataFormType {
    match form_type {
        waddle_xmpp::xep::xep0004::FormType::Form => ExtensionDataFormType::Form,
        waddle_xmpp::xep::xep0004::FormType::Submit => ExtensionDataFormType::Submit,
        waddle_xmpp::xep::xep0004::FormType::Cancel => ExtensionDataFormType::Cancel,
        waddle_xmpp::xep::xep0004::FormType::Result => ExtensionDataFormType::Result,
    }
}

fn extension_data_form_field(
    field: &waddle_xmpp::xep::xep0004::Field,
) -> Option<ExtensionDataFormField> {
    Some(ExtensionDataFormField {
        name: UiActionId::new(field.var.clone()?).ok()?,
        field_type: extension_form_field_type(field.field_type),
        label: field
            .label
            .clone()
            .and_then(|label| waddle_extensions::DisplayText::new(label).ok()),
        required: field.required,
        values: field
            .values
            .iter()
            .map(|value| DataFormValue::new(value.clone()))
            .collect(),
        options: field
            .options
            .iter()
            .map(|option| FormFieldOption {
                label: option
                    .label
                    .clone()
                    .and_then(|label| waddle_extensions::DisplayText::new(label).ok()),
                value: DataFormValue::new(option.value.clone()),
            })
            .collect(),
    })
}

fn extension_form_field_type(
    field_type: waddle_xmpp::xep::xep0004::FieldType,
) -> ExtensionFormFieldType {
    match field_type {
        waddle_xmpp::xep::xep0004::FieldType::Boolean => ExtensionFormFieldType::Boolean,
        waddle_xmpp::xep::xep0004::FieldType::Fixed => ExtensionFormFieldType::Fixed,
        waddle_xmpp::xep::xep0004::FieldType::Hidden => ExtensionFormFieldType::Hidden,
        waddle_xmpp::xep::xep0004::FieldType::JidMulti => ExtensionFormFieldType::JidMulti,
        waddle_xmpp::xep::xep0004::FieldType::JidSingle => ExtensionFormFieldType::JidSingle,
        waddle_xmpp::xep::xep0004::FieldType::ListMulti => ExtensionFormFieldType::ListMulti,
        waddle_xmpp::xep::xep0004::FieldType::ListSingle => ExtensionFormFieldType::ListSingle,
        waddle_xmpp::xep::xep0004::FieldType::TextMulti => ExtensionFormFieldType::TextMulti,
        waddle_xmpp::xep::xep0004::FieldType::TextPrivate => ExtensionFormFieldType::TextPrivate,
        waddle_xmpp::xep::xep0004::FieldType::TextSingle => ExtensionFormFieldType::TextSingle,
    }
}

fn extension_session_id(session_id: Option<String>) -> Option<CommandSessionId> {
    session_id.and_then(|session_id| CommandSessionId::new(session_id).ok())
}

fn extension_command_action(action: waddle_xmpp::xep::xep0050::Action) -> ExtensionCommandAction {
    match action {
        waddle_xmpp::xep::xep0050::Action::Execute => ExtensionCommandAction::Execute,
        waddle_xmpp::xep::xep0050::Action::Next => ExtensionCommandAction::Next,
        waddle_xmpp::xep::xep0050::Action::Prev => ExtensionCommandAction::Prev,
        waddle_xmpp::xep::xep0050::Action::Complete => ExtensionCommandAction::Complete,
        waddle_xmpp::xep::xep0050::Action::Cancel => ExtensionCommandAction::Cancel,
    }
}

fn extension_warning_result(message: &str) -> waddle_xmpp::commands::CommandResult {
    waddle_xmpp::commands::CommandResult::Completed {
        form: None,
        notes: vec![waddle_xmpp::commands::Note::warn(message.to_string())],
    }
}

struct ExtensionPubSubContext {
    storage: Arc<dyn PubSubStorage>,
    owner: jid::BareJid,
    app_state: Arc<AppState>,
    extension_manager: Arc<ExtensionManager>,
    authenticated_user_id: Option<String>,
}

async fn authorize_extension_pubsub_publish(
    context: &ExtensionPubSubContext,
    node: &waddle_extensions::types::PubSubNode,
) -> Result<(), String> {
    let Some(user_id) = context.authenticated_user_id.as_deref() else {
        return Err("authenticated user required".to_string());
    };
    let Some(room) = context.extension_manager.room_for_pubsub_node(node) else {
        return Err("PubSub node is not bound to a channel".to_string());
    };
    let room_jid: jid::BareJid = room
        .as_str()
        .parse()
        .map_err(|error| format!("invalid channel JID in PubSub node: {error}"))?;
    let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(&room_jid) else {
        return Err("PubSub node is not bound to a managed channel".to_string());
    };
    let object = Object::new(ObjectType::Channel, channel_id.clone());
    let subject = Subject::user(user_id);
    let outcast = context
        .app_state
        .permission_actor
        .ask(CheckPermission {
            subject: subject.clone(),
            permission: Permission::Custom("outcast".into()),
            object: object.clone(),
        })
        .await
        .map_err(|error| format!("permission check failed: {error}"))?;
    if outcast.allowed {
        return Err("requester is not allowed in this channel".to_string());
    }
    if managed_channel_permission_allowed(
        &context.app_state,
        &subject,
        channel_id.as_str(),
        Permission::SendMessage,
    )
    .await?
    {
        Ok(())
    } else {
        Err("requester cannot write extension state for this channel".to_string())
    }
}

async fn managed_channel_permission_allowed(
    app_state: &AppState,
    subject: &Subject,
    channel_id: &str,
    permission: Permission,
) -> Result<bool, String> {
    let policy = server_policy_for_managed_channel(channel_id, &permission);
    if policy == ManagedChannelServerPolicy::DeploymentOwnerOnly {
        let server_owner = app_state
            .permission_actor
            .ask(CheckPermission {
                subject: subject.clone(),
                permission: Permission::Owner,
                object: Object::new(ObjectType::Server, DEPLOYMENT_SERVER_ID),
            })
            .await
            .map_err(|error| format!("permission check failed: {error}"))?;
        return Ok(server_owner.allowed);
    }

    let allowed = app_state
        .permission_actor
        .ask(CheckPermission {
            subject: subject.clone(),
            permission: permission.clone(),
            object: Object::new(ObjectType::Channel, channel_id),
        })
        .await
        .map_err(|error| format!("permission check failed: {error}"))?;
    if allowed.allowed {
        return Ok(true);
    }

    if policy == ManagedChannelServerPolicy::DeploymentMembership {
        // Keep these as explicit relation/permission checks. The local permission
        // schema makes `member` inherit owner/admin, but the SpiceDB schema uses
        // server relations directly for compatibility.
        for server_permission in DEPLOYMENT_MEMBERSHIP_PERMISSIONS {
            let server_allowed = app_state
                .permission_actor
                .ask(CheckPermission {
                    subject: subject.clone(),
                    permission: server_permission,
                    object: Object::new(ObjectType::Server, DEPLOYMENT_SERVER_ID),
                })
                .await
                .map_err(|error| format!("permission check failed: {error}"))?;
            if server_allowed.allowed {
                return Ok(true);
            }
        }
        return Ok(false);
    }

    Ok(false)
}

async fn extension_command_result(
    effects: Vec<ExtensionEffect>,
    pubsub: Option<ExtensionPubSubContext>,
) -> waddle_xmpp::commands::CommandResult {
    let mut notes = Vec::new();
    let mut result_form = None;
    for effect in effects {
        match effect {
            ExtensionEffect::PublishPubSub(publish) => match pubsub.as_ref() {
                Some(context) => {
                    match authorize_extension_pubsub_publish(context, &publish.node).await {
                        Ok(()) => {}
                        Err(error) => {
                            notes.push(waddle_xmpp::commands::Note::warn(format!(
                                "PubSub publish denied: {error}"
                            )));
                            continue;
                        }
                    }
                    match publish_extension_pubsub(
                        context.storage.as_ref(),
                        &context.owner,
                        publish,
                    )
                    .await
                    {
                        Ok(item_id) => notes.push(waddle_xmpp::commands::Note::info(format!(
                            "Published PubSub item {item_id}"
                        ))),
                        Err(error) => notes.push(waddle_xmpp::commands::Note::warn(format!(
                            "PubSub publish failed: {error}"
                        ))),
                    }
                }
                None => notes.push(waddle_xmpp::commands::Note::warn(
                    "PubSub publish unavailable".to_string(),
                )),
            },
            ExtensionEffect::ReferenceArtifact(artifact) => {
                let text = format!("Referenced artifact {}", artifact.uri.as_str());
                notes.push(waddle_xmpp::commands::Note::info(text));
            }
            ExtensionEffect::CommandForm(form) => {
                return waddle_xmpp::commands::CommandResult::Executing {
                    form: extension_data_form_to_xmpp(form),
                    session_id: String::new(),
                    notes,
                };
            }
            ExtensionEffect::HostWarning(message) => {
                notes.push(waddle_xmpp::commands::Note::warn(
                    message.as_str().to_string(),
                ));
            }
            ExtensionEffect::EnrichMessage(envelope) => {
                let count = envelope.enrichments.len();
                if result_form.is_none() {
                    result_form = Some(extension_enrichment_result_form(&envelope));
                }
                let summaries = extension_enrichment_texts(&envelope);
                if summaries.is_empty() {
                    notes.push(waddle_xmpp::commands::Note::info(format!(
                        "Produced {count} message enrichment{}",
                        if count == 1 { "" } else { "s" }
                    )));
                } else {
                    notes.extend(summaries.into_iter().map(waddle_xmpp::commands::Note::info));
                }
            }
            ExtensionEffect::Noop => {}
        }
    }
    if notes.is_empty() {
        notes.push(waddle_xmpp::commands::Note::warn(
            "Extension action completed without a visible result".to_string(),
        ));
    }

    waddle_xmpp::commands::CommandResult::Completed {
        form: result_form,
        notes,
    }
}

fn extension_data_form_to_xmpp(
    form: waddle_extensions::DataForm,
) -> waddle_xmpp::xep::xep0004::DataForm {
    use waddle_xmpp::xep::xep0004::{DataForm, Field, FieldOption, FieldType, FormType};

    let form_type = match form.form_type {
        ExtensionDataFormType::Form => FormType::Form,
        ExtensionDataFormType::Submit => FormType::Submit,
        ExtensionDataFormType::Cancel => FormType::Cancel,
        ExtensionDataFormType::Result => FormType::Result,
    };
    let mut out = DataForm::new(form_type);
    if let Some(title) = form.title {
        out = out.with_title(title.into_string());
    }
    for instruction in form.instructions {
        out = out.add_instructions(instruction.into_string());
    }
    for field in form.fields {
        let field_type = match field.field_type {
            ExtensionFormFieldType::Boolean => FieldType::Boolean,
            ExtensionFormFieldType::Fixed => FieldType::Fixed,
            ExtensionFormFieldType::Hidden => FieldType::Hidden,
            ExtensionFormFieldType::JidMulti => FieldType::JidMulti,
            ExtensionFormFieldType::JidSingle => FieldType::JidSingle,
            ExtensionFormFieldType::ListMulti => FieldType::ListMulti,
            ExtensionFormFieldType::ListSingle => FieldType::ListSingle,
            ExtensionFormFieldType::TextMulti => FieldType::TextMulti,
            ExtensionFormFieldType::TextPrivate => FieldType::TextPrivate,
            ExtensionFormFieldType::TextSingle => FieldType::TextSingle,
        };
        let mut xmpp_field = Field::new(field.name.into_string(), field_type);
        if let Some(label) = field.label {
            xmpp_field = xmpp_field.with_label(label.into_string());
        }
        if field.required {
            xmpp_field = xmpp_field.with_required();
        }
        for value in field.values {
            xmpp_field.values.push(value.into_string());
        }
        for option in field.options {
            let value = option.value.into_string();
            let xmpp_option = match option.label {
                Some(label) => FieldOption::with_label(label.into_string(), value),
                None => FieldOption::new(value),
            };
            xmpp_field = xmpp_field.add_option(xmpp_option);
        }
        out = out.add_field(xmpp_field);
    }
    out
}

fn extension_enrichment_texts(envelope: &waddle_extensions::ExtensionEnvelope) -> Vec<String> {
    envelope
        .enrichments
        .iter()
        .flat_map(|enrichment| enrichment.ui.iter())
        .flat_map(|view| view.blocks.iter())
        .filter_map(|block| {
            if let waddle_extensions::types::UiBlock::Text(text) = block {
                Some(text.text.as_str().to_string())
            } else {
                None
            }
        })
        .collect()
}

fn extension_enrichment_result_form(
    envelope: &waddle_extensions::ExtensionEnvelope,
) -> waddle_xmpp::xep::xep0004::DataForm {
    use waddle_xmpp::xep::xep0004::{DataForm, Field, FormType};

    let mut form = DataForm::new(FormType::Result)
        .with_title("Extension result")
        .add_field(Field::form_type("urn:waddle:extension:1:result"));
    let Some(enrichment) = envelope.enrichments.first() else {
        return form;
    };
    form = form
        .add_field(Field::text_single("extension#id", enrichment.id.as_str()))
        .add_field(Field::text_single(
            "extension#plugin",
            enrichment.plugin.as_str(),
        ))
        .add_field(Field::text_single(
            "extension#title",
            enrichment
                .ui
                .first()
                .and_then(|view| view.title.as_ref())
                .map(|title| title.as_str())
                .unwrap_or_else(|| enrichment.plugin.as_str()),
        ))
        .add_field(Field::text_single(
            "extension#summary",
            enrichment.payload_namespace.as_str(),
        ))
        .add_field(Field::text_single(
            "launch-count",
            enrichment.launches.len().to_string(),
        ));
    for (view_index, view) in enrichment.ui.iter().enumerate() {
        for (block_index, block) in view.blocks.iter().enumerate() {
            if let waddle_extensions::types::UiBlock::Text(text) = block {
                form = form.add_field(Field::text_single(
                    format!("view#{view_index}#text#{block_index}"),
                    text.text.as_str(),
                ));
            }
        }
    }
    for (index, launch) in enrichment.launches.iter().enumerate() {
        let prefix = format!("launch#{index}");
        form = form
            .add_field(Field::text_single(
                format!("{prefix}#id"),
                launch.id.as_str(),
            ))
            .add_field(Field::text_single(
                format!("{prefix}#plugin"),
                launch.plugin.as_str(),
            ))
            .add_field(Field::text_single(
                format!("{prefix}#action"),
                launch.action.as_str(),
            ))
            .add_field(Field::text_single(
                format!("{prefix}#command-node"),
                launch.command_node.as_str(),
            ))
            .add_field(Field::text_single(
                format!("{prefix}#label"),
                launch.label.as_str(),
            ))
            .add_field(Field::text_single(
                format!("{prefix}#waddle-id"),
                launch.context.waddle_id.as_str(),
            ));
        if let Some(stanza_id) = &launch.context.source_stanza_id {
            form = form.add_field(Field::text_single(
                format!("{prefix}#source-stanza-id"),
                stanza_id.as_str(),
            ));
        }
        if let Some(token) = &launch.token {
            form = form.add_field(Field::text_single(
                format!("{prefix}#token"),
                token.as_str(),
            ));
        }
        if let Some(expires_at) = &launch.expires_at {
            form = form.add_field(Field::text_single(
                format!("{prefix}#expires-at"),
                expires_at.as_str(),
            ));
        }
        for (payload_index, payload) in launch.payloads.iter().enumerate() {
            let payload_prefix = format!("{prefix}#payload#{payload_index}");
            form = form
                .add_field(Field::text_single(
                    format!("{payload_prefix}#namespace"),
                    payload.namespace.as_str(),
                ))
                .add_field(Field::text_single(
                    format!("{payload_prefix}#name"),
                    payload.root.local_name.as_str(),
                ));
            for child in &payload.root.children {
                if let waddle_extensions::XmlNode::Text(text) = child {
                    form = form.add_field(Field::text_single(
                        format!("{payload_prefix}#text"),
                        text.as_str(),
                    ));
                }
            }
            for attribute in &payload.root.attributes {
                if attribute.local_name == "xmlns" {
                    continue;
                }
                form = form.add_field(Field::text_single(
                    format!("{payload_prefix}#attr#{}", attribute.local_name),
                    attribute.value.as_str(),
                ));
            }
        }
    }
    form
}

const MAX_EXTENSION_PUBSUB_ITEMS: u32 = 500;

async fn publish_extension_pubsub(
    storage: &dyn PubSubStorage,
    owner: &jid::BareJid,
    publish: PubSubPublish,
) -> Result<String, waddle_xmpp::XmppError> {
    ensure_extension_pubsub_node(storage, owner, publish.node.as_str()).await?;
    let item = PubSubItem::new(
        publish.item_id.map(|item_id| item_id.into_string()),
        Some(publish.payload.to_minidom()),
    );
    let result = storage
        .publish_item(owner, publish.node.as_str(), &item, Some(owner), false)
        .await?;
    Ok(result.item_id)
}

async fn ensure_extension_pubsub_node(
    storage: &dyn PubSubStorage,
    owner: &jid::BareJid,
    node: &str,
) -> Result<(), waddle_xmpp::XmppError> {
    let config = extension_pubsub_node_config();
    let (existing, _) = storage.get_or_create_node(owner, node).await?;
    if existing.config != config {
        storage.update_node_config(owner, node, &config).await?;
    }
    storage
        .set_affiliation(owner, node, owner, waddle_xmpp::pubsub::Affiliation::Owner)
        .await?;
    Ok(())
}

fn extension_pubsub_node_config() -> NodeConfig {
    let mut config = NodeConfig::spaces_private();
    config.max_items = MAX_EXTENSION_PUBSUB_ITEMS;
    config
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
    use crate::permissions::{Relation, Tuple, WriteTuple};
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

    /// XmppConfig for unit tests: uses in-memory SQLite for all storage backends.
    fn test_xmpp_config() -> XmppConfig {
        XmppConfig {
            pubsub_database_url: Some("sqlite::memory:".to_string()),
            ..XmppConfig::default()
        }
    }

    async fn create_test_state() -> Arc<AppState> {
        let config = DatabaseConfig::default();
        let pool_config = PoolConfig;
        let db_pool = DatabasePool::new(config, pool_config).await.unwrap();

        // Run migrations
        let runner = MigrationRunner::global();
        runner.run(db_pool.global()).await.unwrap();

        Arc::new(AppState::new(Arc::new(db_pool)))
    }

    #[tokio::test]
    async fn extension_pubsub_permission_allows_bootstrap_chat_member() {
        let state = create_test_state().await;
        let subject = Subject::user("user-alice");

        assert!(
            !managed_channel_permission_allowed(&state, &subject, "chat", Permission::SendMessage)
                .await
                .expect("initial permission check"),
            "server membership should be required before default chat policy applies"
        );

        state
            .permission_actor
            .ask(WriteTuple {
                tuple: Tuple::new(
                    Object::new(ObjectType::Server, DEPLOYMENT_SERVER_ID),
                    Relation::new("member"),
                    subject.clone(),
                ),
            })
            .await
            .expect("server member tuple");

        let owner_subject = Subject::user("user-owner");
        state
            .permission_actor
            .ask(WriteTuple {
                tuple: Tuple::new(
                    Object::new(ObjectType::Server, DEPLOYMENT_SERVER_ID),
                    Relation::new("owner"),
                    owner_subject.clone(),
                ),
            })
            .await
            .expect("server owner tuple");

        assert!(
            managed_channel_permission_allowed(&state, &subject, "chat", Permission::SendMessage)
                .await
                .expect("chat permission check"),
            "default chat extension publishes should inherit deployment membership"
        );
        assert!(
            managed_channel_permission_allowed(&state, &subject, "announcements", Permission::View)
                .await
                .expect("announcements view permission check"),
            "default announcement route reads should inherit deployment membership"
        );
        assert!(
            managed_channel_permission_allowed(&state, &owner_subject, "chat", Permission::View)
                .await
                .expect("owner chat permission check"),
            "default room membership policy must include deployment owners"
        );
        assert!(
            managed_channel_permission_allowed(
                &state,
                &owner_subject,
                "announcements",
                Permission::SendMessage,
            )
            .await
            .expect("owner announcements send permission check"),
            "deployment owners should be allowed to publish announcement extension state"
        );
        assert!(
            !managed_channel_permission_allowed(
                &state,
                &subject,
                "announcements",
                Permission::SendMessage,
            )
            .await
            .expect("announcements send permission check"),
            "announcement extension publishes still require owner permissions"
        );
        state
            .permission_actor
            .ask(WriteTuple {
                tuple: Tuple::new(
                    Object::new(ObjectType::Channel, "announcements"),
                    Relation::new("writer"),
                    subject.clone(),
                ),
            })
            .await
            .expect("announcement writer tuple");
        assert!(
            !managed_channel_permission_allowed(
                &state,
                &subject,
                "announcements",
                Permission::SendMessage,
            )
            .await
            .expect("announcements writer permission check"),
            "announcement channel writer grants must not bypass server-owner write policy"
        );
        assert!(
            !managed_channel_permission_allowed(
                &state,
                &subject,
                "random",
                Permission::SendMessage
            )
            .await
            .expect("random permission check"),
            "non-default channels still require channel permissions"
        );
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
        let app = create_router(state, server_config, test_xmpp_config(), mam_storage, None)
            .await
            .unwrap();

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
        let app = create_router(state, server_config, test_xmpp_config(), mam_storage, None)
            .await
            .unwrap();

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
        let app = create_router(state, server_config, test_xmpp_config(), mam_storage, None)
            .await
            .unwrap();

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
        let app = create_router(state, server_config, test_xmpp_config(), mam_storage, None)
            .await
            .unwrap();

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
        let app = create_router(state, server_config, test_xmpp_config(), mam_storage, None)
            .await
            .unwrap();

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
        let app = create_router(state, server_config, test_xmpp_config(), mam_storage, None)
            .await
            .unwrap();

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

        let db = state.db_pool.global();

        let runner = MigrationRunner::waddle();
        runner.run(db).await.unwrap();

        // Verify tables exist - use persistent connection for in-memory database
        let conn = db.guard().await.unwrap();
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
