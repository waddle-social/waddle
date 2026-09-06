use crate::config::ServerConfig;
use crate::server::extension_commands::{build_extension_manager, register_extension_commands};
use crate::server::extension_host_adapter;
use crate::server::extension_host_tools::DeferredExtensionHostTools;
use crate::server::health::{
    configure_cors, detailed_health_handler, health_handler, metrics_handler, readiness_handler,
};
use crate::server::routes;
use crate::server::routes::auth::AuthState;
use crate::server::routes::uploads::UploadState;
use crate::server::routes::websocket::{
    ProtocolServices, WebSocketDeps, WebSocketState, XmppServiceDomains,
};
use crate::server::session_janitors::{
    spawn_auth_state_janitor, spawn_call_teardown_outbox_janitor,
    spawn_critical_registry_supervisor, spawn_destroy_completion_janitor,
    spawn_graceful_shutdown_drain, spawn_notification_outbox_janitor, spawn_orphan_reaper_janitor,
    spawn_pending_delivery_claim_janitor, spawn_push_service_publish_job_janitor,
    spawn_room_dormancy_janitor, spawn_room_effect_outbox_janitor, spawn_sm_expiry_janitor,
    spawn_user_actor_reaper,
};
use crate::server::topology::bootstrap_fresh_xmpp_topology;
use crate::server::trace::{attach_http_route_template, make_request_span, observe_http_response};
use crate::server::{AppState, XmppConfig};
use anyhow::Result;
use axum::{middleware, routing::get, Router};
use rustls_acme::tower::TowerHttp01ChallengeService;
use std::future::IntoFuture as _;
use std::sync::Arc;
use tower_http::{compression::CompressionLayer, trace::TraceLayer};
use tracing::{info, warn};
use waddle_xmpp::mam::{MamStorage, SqlxMamStorage};
use waddle_xmpp::registry::ConnectionRegistry;

pub(crate) struct HttpServerDeps {
    pub(crate) state: Arc<AppState>,
    pub(crate) server_config: ServerConfig,
    pub(crate) xmpp_config: XmppConfig,
    pub(crate) mam_storage: Arc<dyn MamStorage>,
    /// Concrete `DatabasePubSubStorage` handle built once in
    /// `start_with_config`. Threaded down here so the notification
    /// settings projection can attach to the same SQL database without
    /// re-opening it. The trait-object view lives on
    /// [`AppState::pubsub_storage`] for the rest of the WebSocket graph.
    pub(crate) pubsub_database_storage: Arc<crate::pubsub::DatabasePubSubStorage>,
    pub(crate) acme_http01_challenge_service: Option<TowerHttp01ChallengeService>,
    pub(crate) listener: tokio::net::TcpListener,
    /// Ecdysis shutdown view: carries the stop token that gates the
    /// graceful_shutdown closure and mints per-connection guards in
    /// the WebSocket accept path (issue #1091).
    pub(crate) shutdown_handle: waddle_ecdysis::ShutdownHandle,
    /// Q6 graceful-shutdown drain completion signal — fired by the
    /// drain task when it finishes promoting unacked queues. The
    /// HTTP server's graceful_shutdown closure waits on this after
    /// stop_token cancels so the runtime doesn't tear down mid-drain.
    pub(crate) drain_complete: Arc<tokio::sync::Notify>,
}

const HTTP_FORCED_EXIT_MARGIN: std::time::Duration = std::time::Duration::from_secs(1);

#[derive(Debug)]
struct LiveKitAdminFailureObserver;

impl waddle_sfu::AdminCallObserver for LiveKitAdminFailureObserver {
    fn admin_call_failed(&self, op: waddle_sfu::AdminOp) {
        let op = match op {
            waddle_sfu::AdminOp::DeleteRoom => {
                waddle_xmpp::telemetry::attributes::AdminOp::DeleteRoom
            }
            waddle_sfu::AdminOp::RemoveParticipant => {
                waddle_xmpp::telemetry::attributes::AdminOp::RemoveParticipant
            }
            waddle_sfu::AdminOp::UpdateParticipant => {
                waddle_xmpp::telemetry::attributes::AdminOp::UpdateParticipant
            }
            waddle_sfu::AdminOp::ListRooms => {
                waddle_xmpp::telemetry::attributes::AdminOp::ListRooms
            }
            waddle_sfu::AdminOp::RoomOccupancy => {
                waddle_xmpp::telemetry::attributes::AdminOp::RoomOccupancy
            }
        };
        waddle_xmpp::telemetry::call::increment_admin_call_failed(op);
    }
}

async fn await_http_server_or_forced_exit<F, T>(
    server: F,
    stop_token: tokio_util::sync::CancellationToken,
    drain_timeout: std::time::Duration,
) -> Option<T>
where
    F: std::future::Future<Output = T>,
{
    tokio::pin!(server);
    tokio::select! {
        result = server.as_mut() => Some(result),
        _ = async {
            stop_token.cancelled().await;
            tokio::time::sleep(drain_timeout + HTTP_FORCED_EXIT_MARGIN).await;
        } => None,
    }
}

/// Start the HTTP server with graceful shutdown support.
pub(crate) async fn start_http_server(deps: HttpServerDeps) -> Result<()> {
    let HttpServerDeps {
        state,
        server_config,
        xmpp_config,
        mam_storage,
        pubsub_database_storage,
        acme_http01_challenge_service,
        listener,
        shutdown_handle,
        drain_complete,
    } = deps;

    let stop_token = shutdown_handle.stop_token();
    let force_stop_token = stop_token.clone();
    let drain_timeout = shutdown_handle.drain_timeout();
    let app = create_router(RouterDeps {
        state,
        server_config,
        xmpp_config,
        mam_storage,
        pubsub_database_storage,
        acme_http01_challenge_service,
        shutdown_handle,
        drain_complete: drain_complete.clone(),
    })
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

    let server = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            stop_token.cancelled().await;
            info!("HTTP server received shutdown signal; awaiting SM Q6 drain");
            let drain_wait = drain_timeout;
            if tokio::time::timeout(drain_wait, drain_complete.notified())
                .await
                .is_err()
            {
                warn!(
                    timeout_secs = drain_timeout.as_secs(),
                    "HTTP server: SM drain notification timed out; forcing connection drain"
                );
            } else {
                info!("HTTP server: SM drain complete; draining connections");
            }
        })
        .into_future();
    match await_http_server_or_forced_exit(server, force_stop_token, drain_timeout).await {
        Some(result) => result?,
        None => {
            warn!(
                timeout_secs = drain_timeout.as_secs(),
                margin_secs = HTTP_FORCED_EXIT_MARGIN.as_secs(),
                "HTTP graceful shutdown exceeded its absolute deadline; terminating remaining connection tasks"
            );
        }
    }

    Ok(())
}

#[derive(Debug, thiserror::Error)]
enum MamIngressColocationError {
    #[error("MAM and ingress must use the same database backend")]
    BackendMismatch,
    #[error("MAM and ingress must share one durable SQLite database")]
    SqliteDatabaseMismatch,
}

async fn ensure_mam_ingress_colocation(
    storage: &SqlxMamStorage,
    global: &crate::db::Database,
) -> Result<()> {
    if let Some(pool) = storage.postgres_pool() {
        if global.driver() != crate::db::DatabaseDriver::Postgres {
            return Err(MamIngressColocationError::BackendMismatch.into());
        }
        let mam_identity = crate::db::lineage::live_postgres_identity_via_pg_pool(pool).await?;
        let global_identity = crate::db::lineage::live_postgres_identity(global).await?;
        if mam_identity != global_identity {
            return Err(
                waddle_xmpp::mam::MamStorageError::ClusterColocationMismatch {
                    identities: Box::new(waddle_xmpp::ClusterColocationIdentities {
                        store: (&mam_identity).into(),
                        global: (&global_identity).into(),
                    }),
                }
                .into(),
            );
        }
        return Ok(());
    }
    if global.driver() != crate::db::DatabaseDriver::Sqlite {
        return Err(MamIngressColocationError::BackendMismatch.into());
    }
    let pool = storage
        .sqlite_pool()
        .ok_or(MamIngressColocationError::BackendMismatch)?;
    let (_, _, mam_file): (i64, String, String) = sqlx::query_as("PRAGMA database_list")
        .fetch_one(pool)
        .await?;
    let connection = global.guard().await?;
    let mut rows = connection.query("PRAGMA database_list", ()).await?;
    let global_file: String = rows
        .next()
        .await?
        .ok_or(MamIngressColocationError::SqliteDatabaseMismatch)?
        .get(2)?;
    if mam_file.is_empty()
        || global_file.is_empty()
        || std::fs::canonicalize(mam_file)? != std::fs::canonicalize(global_file)?
    {
        return Err(MamIngressColocationError::SqliteDatabaseMismatch.into());
    }
    Ok(())
}

/// Open MAM on the ingress database so archive writes participate in Phase B.
///
/// SQLite: MAM always lives in the global database (one pool, one file or one
/// in-memory database); a different `WADDLE_XMPP_MAM_DATABASE_URL` is a typed
/// configuration error. PostgreSQL: the configured URL must resolve to the
/// same live database/schema identity as the global database. Cluster
/// fencing is enabled only when the cluster has a live claim pair.
pub(crate) async fn create_websocket_mam_storage(
    database_url: Option<String>,
    clustering_enabled: bool,
    clustering_claim_pair_live: bool,
    global_db: &crate::db::Database,
) -> Result<Arc<SqlxMamStorage>> {
    let storage = match global_db.driver() {
        crate::db::DatabaseDriver::Sqlite => {
            if database_url
                .as_deref()
                .is_some_and(|url| url != global_db.database_url())
            {
                return Err(MamIngressColocationError::SqliteDatabaseMismatch.into());
            }
            let pool = global_db
                .sqlite_pool()
                .ok_or(MamIngressColocationError::BackendMismatch)?;
            SqlxMamStorage::from_sqlite_pool(pool.clone()).await?
        }
        crate::db::DatabaseDriver::Postgres => {
            let database_url = database_url
                .as_deref()
                .unwrap_or_else(|| global_db.database_url());
            let opened = SqlxMamStorage::open(database_url).await.map_err(|error| {
                anyhow::anyhow!("Failed to initialize WebSocket MAM storage: {error}")
            })?;
            ensure_mam_ingress_colocation(&opened, global_db).await?;
            opened
        }
    };
    Ok(Arc::new(storage.with_cluster_fencing(
        clustering_enabled && clustering_claim_pair_live,
    )))
}

/// Explicit dependency bundle for [`create_router`]. Grouped into a
/// single struct so the signature stays under clippy's
/// `too_many_arguments` threshold as new deps are wired in (e.g. the
/// admin V2 plumbing PR additions to `AppState`).
pub(crate) struct RouterDeps {
    pub(crate) state: Arc<AppState>,
    pub(crate) server_config: ServerConfig,
    pub(crate) xmpp_config: XmppConfig,
    pub(crate) mam_storage: Arc<dyn MamStorage>,
    pub(crate) pubsub_database_storage: Arc<crate::pubsub::DatabasePubSubStorage>,
    pub(crate) acme_http01_challenge_service: Option<TowerHttp01ChallengeService>,
    pub(crate) shutdown_handle: waddle_ecdysis::ShutdownHandle,
    pub(crate) drain_complete: Arc<tokio::sync::Notify>,
}

/// Create the Axum router with all routes and middleware.
pub(crate) async fn create_router(deps: RouterDeps) -> Result<Router> {
    let RouterDeps {
        state,
        server_config,
        xmpp_config,
        mam_storage,
        pubsub_database_storage,
        acme_http01_challenge_service,
        shutdown_handle,
        drain_complete,
    } = deps;
    let shutdown_stop_token = shutdown_handle.stop_token();
    // Create auth broker state
    let auth_state = Arc::new(AuthState::new(
        state.clone(),
        &server_config,
        &xmpp_config.public_websocket_url,
        Some(server_config.session_key.as_bytes()),
    ));

    let websocket_state = create_websocket_state(
        Arc::clone(&state),
        &server_config,
        &xmpp_config,
        Arc::clone(&auth_state),
        mam_storage,
        pubsub_database_storage,
        shutdown_handle,
    )
    .await?;

    // Install the OIDC → PEP profile bridge hook now that
    // `WebSocketState` (and its `pubsub_storage`) exists. The hook
    // captures cheap clones of the deps and runs the publish chain
    // in a `tokio::spawn` from `auth/callback.rs`.
    install_profile_publish_hook(&auth_state, websocket_state.clone());

    // One-shot OIDC-profile backfill at boot — best-effort,
    // bounded-concurrency, idempotent. Registered with
    // `profile_publish_tracker` so the graceful-shutdown drain
    // awaits it before tearing down. The auth callback path
    // continues to fire per-login, so a partial backfill is just
    // "we'll catch up next boot."
    //
    // The `shutdown_stop_token` is forwarded into the run so SIGTERM
    // short-circuits the row stream — without that, an N-row pass
    // could block the publish-tracker `wait()` in the graceful-
    // shutdown drain past the deployment grace period.
    let lineage_attested = websocket_state
        .deps
        .app_state
        .lineage_startup
        .get()
        .map(crate::db::lineage::LineageReport::is_attested)
        .unwrap_or(false);
    if lineage_attested {
        let db_actor = websocket_state
            .deps
            .app_state
            .db_pool
            .global_actor()
            .clone();
        let backfill_state = websocket_state.clone();
        let xmpp_domain = auth_state.xmpp_domain.clone();
        let backfill_cancel = shutdown_stop_token.clone();
        let span = tracing::info_span!("oidc_profile_backfill");
        let backfill_tracker = backfill_state.deps.protocol.profile_publish_tracker.clone();
        let _backfill_handle = backfill_tracker.spawn(async move {
            use tracing::Instrument;
            let db = match db_actor.ask(crate::db::actor::GetDatabase).await {
                Ok(db) => db,
                Err(error) => {
                    warn!(
                        error = %error,
                        "OIDC profile backfill: failed to acquire database; skipping run"
                    );
                    return;
                }
            };
            let deps = crate::profile::ProfilePublishDeps {
                state: backfill_state,
                vcard_store: crate::vcard::VCardStore::new(db.into()),
                fetch_policy: crate::profile::FetchPolicy::default(),
            };
            let _report = async move {
                crate::profile::run_startup_backfill(&deps, &xmpp_domain, backfill_cancel).await;
            }
            .instrument(span)
            .await;
        });
    } else {
        warn!("skipping OIDC profile backfill: lineage attestation failed");
    }

    spawn_critical_registry_supervisor(&websocket_state).await;
    // The startup attestation ran in `create_websocket_state`, before any
    // data-mutating bootstrap. A definitive failure latched the lifecycle
    // there (permanent, alive-unready; restart to recover); a transient-only
    // failure already failed startup outright, so reaching this point
    // unattested means the latched case.
    if lineage_attested {
        promote_to_serving_and_spawn_janitors(
            &websocket_state,
            server_config.clustering.orphan_reaper_interval,
        );
    }
    spawn_graceful_shutdown_drain(
        Arc::clone(&websocket_state),
        shutdown_stop_token,
        Arc::clone(&drain_complete),
    );

    let extension_webhooks_router =
        routes::extension_webhooks::router(Arc::clone(&websocket_state));
    let livekit_webhook_router = routes::livekit_webhook::router(Arc::clone(&websocket_state));
    let websocket_router = routes::websocket::router(websocket_state.clone());
    let calendar_feed_router =
        routes::calendar_feed::router(Arc::new(routes::calendar_feed::CalendarFeedState::new(
            Arc::clone(&auth_state),
            Arc::clone(&websocket_state.deps.protocol.pubsub_storage),
            server_config.session_key.as_bytes(),
        )));

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
            "/.well-known/acme-challenge/{challenge_token}",
            challenge_service,
        );
    }

    // Always merge auth surfaces. If no providers are configured these endpoints
    // return explicit errors.
    let auth_router = routes::auth::router(auth_state.clone());
    let device_router = routes::device::router(auth_state.clone());
    let xmpp_oauth_router = routes::xmpp_oauth::router(auth_state.clone());
    let auth_page_router = routes::auth_page::router(auth_state.clone());

    // Application routes live behind the same admission gate as WebSocket
    // upgrades: a node held out of `Serving` (failed/latched lineage
    // attestation, fencing, drain) must not accept HTTP requests that read
    // or mutate application state through a database it cannot vouch for.
    // Operational endpoints (health/readiness/metrics, ACME) stay ungated
    // on the base router above.
    let mut app_router = Router::new()
        .merge(auth_router)
        .merge(device_router)
        .merge(xmpp_oauth_router)
        .merge(auth_page_router)
        .merge(extension_webhooks_router)
        .merge(livekit_webhook_router)
        .merge(calendar_feed_router);

    // Test-only profile-publish route. Only mounted when:
    // 1. The fixed-account flag is on (the test harness opt-in).
    // 2. A non-empty `WADDLE_TEST_PROFILE_PUBLISH_TOKEN` is set.
    // Both must be true. Production deployments set neither, and even
    // a misconfigured staging that flips the flag without the token
    // does not mount the route.
    if let Some(auth) = build_test_profile_publish_auth(&auth_state.xmpp_domain) {
        app_router = app_router.merge(crate::server::profile_publish_route::router(
            websocket_state.clone(),
            auth,
        ));
    }

    // Debug state-inventory route. Mounted only when
    // `WADDLE_DEBUG_STATE_TOKEN` is set; production canary opts in by
    // setting the env var. Returns per-map `.len()` counts so a
    // Prometheus scrape can correlate the offending structure with
    // process RSS.
    if let Some(auth) = crate::server::state_inventory_route::debug_state_auth_from_env() {
        info!("Mounting /debug/state-inventory (WADDLE_DEBUG_STATE_TOKEN set)");
        app_router = app_router.merge(crate::server::state_inventory_route::router(
            websocket_state.clone(),
            auth,
        ));
    }

    // Always merge common routes required by XMPP, auth, upload, and operations.
    let app_router = app_router
        // Merge XMPP over WebSocket endpoint
        .merge(websocket_router)
        // Merge well-known endpoints for XMPP service discovery
        .merge(well_known_router)
        // Merge upload routes for XEP-0363 HTTP File Upload
        .merge(upload_router)
        .layer(middleware::from_fn_with_state(
            Arc::clone(&websocket_state.deps.app_state),
            admission_gate,
        ));
    let router = router
        .merge(app_router)
        .layer(CompressionLayer::new())
        .layer(configure_cors())
        // These two observability layers stay outside CORS so preflight
        // responses are measured and receive the same span attributes as
        // requests that reach a route handler.
        .layer(middleware::from_fn(attach_http_route_template))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(make_request_span)
                .on_response(observe_http_response),
        );
    Ok(router)
}

async fn open_ingress_authority(
    config: &ServerConfig,
    state: &AppState,
) -> Result<Arc<crate::ingress::IngressAuthority>> {
    #[cfg(feature = "clustering")]
    let node_identity = if config.clustering.enabled {
        Some(
            state
                .clustering_claims
                .node_identity
                .clone()
                .ok_or(crate::ingress::IngressStartupError::NodeIdentityMissing)?,
        )
    } else {
        None
    };
    Ok(Arc::new(
        crate::ingress::IngressAuthority::new(
            config.ingress.clone(),
            state.db_pool.global().clone(),
            state.lineage_config.clone(),
            #[cfg(feature = "clustering")]
            node_identity,
        )
        .await?,
    ))
}

async fn create_websocket_state(
    state: Arc<AppState>,
    server_config: &ServerConfig,
    xmpp_config: &XmppConfig,
    auth_state: Arc<AuthState>,
    mam_storage: Arc<dyn MamStorage>,
    pubsub_database_storage: Arc<crate::pubsub::DatabasePubSubStorage>,
    shutdown_handle: waddle_ecdysis::ShutdownHandle,
) -> Result<Arc<WebSocketState>> {
    // Create connection registry for WebSocket message routing
    let connection_registry = Arc::new(ConnectionRegistry::new());
    // ADR-0017 Phase 3 Slice 6: complete the cross-node resume bridge
    // `clustering::start_if_enabled` handed the swarm's `RelayActor` empty
    // (the connection registry didn't exist yet at that point in startup) —
    // the same construction-order chicken-and-egg fix `local_claims` below
    // already applies. A no-op when clustering is disabled or this binary
    // lacks the `clustering` feature (`resume_bridge` is `None`).
    #[cfg(feature = "clustering")]
    if let Some(resume_bridge) = &state.clustering_claims.resume_bridge {
        resume_bridge.wire(Arc::clone(&connection_registry));
    }

    // ADR-0017 Phase 1: spawn the actor-backed per-user registry. It is
    // populated alongside `connection_registry` on the live register/
    // unregister path (dual-registration). It is now the authoritative routing
    // surface: bare-JID selection (`route_to_connection`, Slice 1) sources its
    // candidate set + RFC priority ranking from this actor (intersected with
    // DashMap liveness), and 1:1/DM/groupchat delivery (`deliver_*_to_full`,
    // Slice 2) routes through its `TrySend*` with no DashMap send fallback. The
    // empty-actor reaper (`spawn_user_actor_reaper`) prunes actors left empty by
    // delivery-path closed-channel eviction.
    let user_registry = {
        use kameo::actor::Spawn;
        waddle_xmpp::registry::UserRegistryActor::spawn(
            waddle_xmpp::registry::UserRegistryActor::new(),
        )
    };
    // ADR-0017 Phase 4 Slice 1b: wire the same clustering claim
    // store/identity pair into UserRegistry that SM sessions and rooms use.
    // A `None` pair (clustering disabled, non-Postgres, or non-clustering
    // build) leaves the registry on its single-node in-process default.
    // The single-node default is already observed: the registry constructs
    // its own ObservedClaimStore-wrapped InProcessClaimStore, so no
    // unconditional re-wiring is needed here (re-wiring after spawn races
    // hydration; #1648 review).
    if let Some((claim_store, node_identity)) = state.clustering_claims.claim_pair() {
        if let Err(_error) = user_registry
            .tell(waddle_xmpp::registry::WireUserClusteringClaims {
                claim_store,
                node_identity,
            })
            .mailbox_timeout(std::time::Duration::from_secs(2))
            .await
        {
            warn!("failed to wire clustering claims into the user registry");
        }
    }
    #[cfg(feature = "clustering")]
    if let Some(user_local_claims) = &state.clustering_claims.user_local_claims {
        user_local_claims.wire(user_registry.clone());
        user_local_claims.wire_connection_registry(Arc::clone(&connection_registry));
    }

    // Read the MUC room registry and PubSub storage off the shared
    // `AppState` — both are built in `start_with_config` so admin V2
    // handlers and the WebSocket transport operate on the same handles.
    let xmpp_domain = auth_state.xmpp_domain.clone();
    // #757: `muc`/`spaces` must match the registry/component domains the rest
    // of the server is built from, honoring `WADDLE_MUC_DOMAIN` /
    // `WADDLE_SPACES_JID`, rather than re-deriving `muc.<domain>`.
    let service_domains = XmppServiceDomains::new(
        &xmpp_domain,
        xmpp_config.muc_domain.as_str(),
        // `spaces` is consumed as a plain domain in disco routing
        // (`target_to == spaces_domain`); take the domain part so a
        // node-qualified `WADDLE_SPACES_JID` cannot break the comparison.
        xmpp_config.spaces_jid.domain().as_str(),
    );
    let room_registry = state.room_registry.clone();
    let pubsub_storage = Arc::clone(&state.pubsub_storage);

    let deferred_extension_host_tools = Arc::new(DeferredExtensionHostTools::default());
    let extension_manager = build_extension_manager(
        server_config,
        &xmpp_domain,
        Arc::clone(&deferred_extension_host_tools),
    )
    .await?;

    let websocket_command_registry = Arc::new(waddle_xmpp::commands::CommandRegistry::new());

    let call_teardown_node_identity = state
        .clustering_claims
        .node_identity
        .clone()
        .unwrap_or_else(|| {
            waddle_xmpp::ownership::SharedNodeIdentity::new(
                waddle_xmpp::ownership::NodeIdentity::local(),
            )
        });
    let call_teardown_outbox = Arc::new(
        crate::call_teardown_outbox::CallTeardownOutboxStore::new_with_node_identity(
            state.db_pool.global().clone(),
            call_teardown_node_identity,
        )
        .await
        .map_err(|error| anyhow::anyhow!("failed to initialize call teardown outbox: {error}"))?,
    );
    let call_teardown_persistence =
        crate::call_teardown_outbox::CallTeardownPersistenceSupervisor::new(
            Arc::clone(&call_teardown_outbox),
            tokio::runtime::Handle::current(),
        );
    let room_effect_outbox = Arc::new(
        crate::room_effect_outbox::RoomEffectOutboxStore::new(state.db_pool.global().clone())
            .await
            .map_err(|error| anyhow::anyhow!("failed to initialize room effect outbox: {error}"))?,
    );
    let room_effect_arm_supervisor = crate::room_effect_outbox::RoomEffectArmSupervisor::new(
        Arc::clone(&room_effect_outbox),
        tokio::runtime::Handle::current(),
    );
    // Build the sans-I/O stanza dispatcher with the handlers migrated so far.
    // See `waddle_xmpp::protocol` for the state-machine design; any IQ
    // namespace registered here short-circuits the legacy string-matching
    // path in `routes::websocket::handle_iq`.
    let mut stanza_dispatcher = waddle_xmpp::protocol::StanzaDispatcher::new();
    waddle_xmpp::protocol::handlers::register_default_handlers(&mut stanza_dispatcher);
    waddle_xmpp::protocol::handlers::register_default_message_handlers(&mut stanza_dispatcher);
    // Holds the `SfuService` after `register_call_handlers` so the
    // WebSocket layer can also call `unregister_call_participant`
    // when a session leaves a MUC (graceful unavailable or
    // disconnect/SM-expiry cleanup). Without this the SFU
    // participant lingers until LiveKit times them out — a stolen
    // JWT could replay in the meantime, and the channel UI keeps
    // showing the user as "in call" until the SFU notices on its
    // own.
    let mut sfu_service: Option<std::sync::Arc<dyn waddle_sfu::SfuService>> = None;
    // The async reconciliation view of the same concrete SFU, captured
    // here so a background task can poll LiveKit for ghosts once
    // `websocket_state` exists (see `spawn_reconciliation_task` below).
    let mut sfu_reconciler: Option<std::sync::Arc<dyn waddle_sfu::SfuReconciler>> = None;
    let mut call_teardown_executor: Option<waddle_sfu::LiveKitTeardownExecutor> = None;
    match waddle_sfu::SfuConfig::from_env() {
        Ok(Some(sfu_config)) => {
            let turn_tls_port = sfu_config.turn_tls_port;
            let turn_udp_port = sfu_config.turn_udp_port;
            match waddle_sfu::LiveKitSfu::new_with_observer(
                sfu_config,
                Arc::new(LiveKitAdminFailureObserver),
            ) {
                Ok(sfu_impl) => {
                    let sink_store = Arc::clone(&call_teardown_outbox);
                    let failure_sink: waddle_sfu::TeardownFailureSink = Arc::new(move |lite| {
                        let target = match lite.target {
                            waddle_sfu::TeardownTargetLite::Participant {
                                identity,
                                participant_sid,
                            } => crate::call_teardown_outbox::TeardownTarget::Participant {
                                identity: identity.as_jid().clone(),
                                participant_sid,
                            },
                            waddle_sfu::TeardownTargetLite::Room => {
                                crate::call_teardown_outbox::TeardownTarget::Room
                            }
                        };
                        let intent = crate::call_teardown_outbox::CallTeardownIntent {
                            call_id: lite.call_id,
                            target,
                            generation: lite.generation,
                            room_sid: lite.room_sid,
                            occupant: lite.occupant_session,
                            unbound_occupant: lite.unbound_occupant,
                            session: None,
                        };
                        let sink_store = Arc::clone(&sink_store);
                        Box::pin(async move {
                            let mut retry_delay = std::time::Duration::from_secs(5);
                            loop {
                                match sink_store.enqueue(intent.clone()).await {
                                    Ok(_) => break,
                                    Err(error) => {
                                        warn!(
                                            %error,
                                            retry_delay_ms = retry_delay.as_millis(),
                                            "failed to persist reported call teardown intent; retrying"
                                        );
                                        tokio::time::sleep(retry_delay).await;
                                        retry_delay = retry_delay
                                            .saturating_mul(2)
                                            .min(std::time::Duration::from_secs(10 * 60));
                                    }
                                }
                            }
                        })
                    });
                    let sfu_impl = sfu_impl.with_teardown_failure_sink(failure_sink);
                    call_teardown_executor = Some(sfu_impl.teardown_executor());
                    // Build the concrete SFU once, then hand out two
                    // trait-object views of it: the sync `SfuService`
                    // the XMPP handlers + WebSocket layer consume, and
                    // the async `SfuReconciler` the webhook route's
                    // background reconciliation task drives.
                    let sfu_concrete = std::sync::Arc::new(sfu_impl);
                    let sfu: std::sync::Arc<dyn waddle_sfu::SfuService> = sfu_concrete.clone();
                    let reconciler: std::sync::Arc<dyn waddle_sfu::SfuReconciler> = sfu_concrete;
                    waddle_xmpp::protocol::handlers::register_call_handlers(
                        &mut stanza_dispatcher,
                        Arc::clone(&sfu),
                        turn_tls_port,
                        turn_udp_port,
                    );
                    sfu_service = Some(sfu);
                    sfu_reconciler = Some(reconciler);
                    tracing::info!(
                        "LiveKit SFU configured; XEP-0166 Jingle + XEP-0215 extdisco handlers registered"
                    );
                }
                Err(error) => {
                    warn!(
                        %error,
                        "failed to build LiveKit SFU bridge; A/V calling will be unavailable"
                    );
                }
            }
        }
        Ok(None) => {
            tracing::info!(
                "LIVEKIT_* env vars unset; XMPP-native A/V calling disabled for this process"
            );
        }
        Err(error) => {
            warn!(%error, "invalid LiveKit configuration; A/V calling will be unavailable");
        }
    }
    let stanza_dispatcher = Arc::new(stanza_dispatcher);
    let sm_session_registry = create_sm_session_registry(
        xmpp_config,
        server_config,
        &state.clustering_claims,
        state.db_pool.global(),
        &state,
    )
    .await?;
    #[cfg(feature = "clustering")]
    if let Some(local_claims) = &state.clustering_claims.local_claims {
        local_claims.wire_connection_registry(Arc::clone(&connection_registry));
    }
    // Pending delivery opens here (rather than with the other protocol
    // stores below) so that EVERY durable pool is registered before the
    // startup attestation gate that follows — it is the last registrant.
    let pending_delivery_storage = create_pending_delivery_storage(
        xmpp_config,
        server_config,
        &state.clustering_claims,
        state.db_pool.global(),
        &state,
    )
    .await?;
    // ---- Startup lineage attestation gate (#1652) ----
    // All durable stores are open and registered; seal the registry and run
    // the one startup attestation pass BEFORE anything mutates application
    // data (XMPP topology bootstrap, VAPID provisioning, profile backfill,
    // LiveKit reconciliation). A mis-provisioned database must not be
    // written to by a node that will never serve from it. Transient
    // transport errors get a short bounded retry; a definitive failure
    // latches the lifecycle so no path (including clustering lease
    // recovery) can ever promote this process to `Serving` — recovery is an
    // operator restart after the cause is fixed.
    state.seal_lineage_registry();
    let lineage_report = attest_startup_lineage(&state).await;
    let lineage_attested = lineage_report.is_attested();
    if lineage_attested {
        info!("startup database lineage attestation passed");
    } else if lineage_report.is_transient_only() {
        // Unreachable database, nothing definitive: fail startup outright,
        // exactly like migrations against an unreachable database — the
        // orchestrator's restart backoff is the retry loop, and the next
        // boot runs the FULL bootstrap (topology, VAPID, backfill) instead
        // of promoting a half-bootstrapped node. Definitive refusals (the
        // `else` below) stay alive-unready instead, because restarting
        // cannot fix them and the readiness JSON is the diagnostic surface.
        for (store, status) in lineage_report.failures() {
            tracing::error!(
                store = %store,
                status = status.as_str(),
                "startup lineage attestation could not reach the database; exiting so the restart retries a full bootstrap"
            );
        }
        return Err(anyhow::anyhow!(
            "database unreachable during startup lineage attestation"
        ));
    } else {
        for (store, status) in lineage_report.failures() {
            tracing::error!(
                store = %store,
                status = status.as_str(),
                "database lineage attestation failed; node stays unready until the cause is fixed and the pod is restarted"
            );
        }
        let _ = state.lineage_latched.set(lineage_report.clone());
        state.node_lifecycle.latch_startup_block();
    }
    let _ = state.lineage_startup.set(lineage_report);
    let extension_pubsub_owner: jid::BareJid = service_domains.extensions.parse()?;
    register_extension_commands(
        Arc::clone(&extension_manager),
        Arc::clone(&websocket_command_registry),
        Arc::clone(&pubsub_storage),
        extension_pubsub_owner,
        Arc::clone(&state),
    )
    .await;
    // Admin V1 ad-hoc commands (`urn:waddle:admin:*`). Registered
    // alongside the extension commands so all `<command>` IQs route
    // through the same `CommandRegistry`. The admin commands rely on
    // [`crate::admin::is_community_owner`] for ACL; the registry has
    // no opinion on authorization, so refusing non-owners is the
    // handler's job.
    crate::admin::users_list::register(
        &websocket_command_registry,
        Arc::clone(&state),
        xmpp_domain.clone(),
    )
    .await;
    // Admin V2 ad-hoc commands. The spaces handlers delegate to the
    // typed dependencies on `AppState` (`spaces_metadata_store`,
    // `pubsub_storage`, `room_registry`) wired up via #682/#683.
    crate::admin::spaces::register(&websocket_command_registry, Arc::clone(&state)).await;
    if lineage_attested {
        crate::server::bootstrap_membership::reconcile_existing_accounts_or_warn(
            state.db_pool.global_actor(),
            &state.permission_actor,
            &crate::server::bootstrap_membership::BootstrapMembershipConfig::from_env(),
        )
        .await;
        if let Err(error) = bootstrap_fresh_xmpp_topology(
            &state,
            Arc::clone(&pubsub_storage),
            &service_domains,
            &room_registry,
        )
        .await
        {
            warn!(error = %error, "Failed to bootstrap fresh XMPP topology");
        }
    } else {
        warn!(
            "skipping membership reconciliation and XMPP topology bootstrap: lineage attestation failed"
        );
    }
    ensure_push_service_global_database_is_durable()?;
    let push_store: Arc<dyn waddle_xmpp::push::PushSubscriptionStore> = Arc::new(
        crate::push_registrations::DatabasePushRegistrationStore::new(
            state.db_pool.global().clone(),
        )
        .await
        .map_err(|error| anyhow::anyhow!("failed to initialize XEP-0357 storage: {error}"))?,
    );
    // Provision (or load) the VAPID signing key once at boot. The
    // signer is `Arc<dyn VapidSigner>` so the publish-job worker can
    // sign once per (kid, aud, sub) and share the JWT across the device
    // fan-out. `load_or_provision` returns `Err` only when the root key
    // is unusable or the env-bootstrap value is malformed — in that
    // case the entire push service degrades, so the caller intentionally
    // fails boot rather than silently dispatching without VAPID.
    let vapid_signer = if lineage_attested {
        crate::push_service::vapid_storage::VapidStorage::load_or_provision(
            state.db_pool.global().clone(),
            server_config.session_key.as_bytes(),
        )
        .await
        .map_err(|error| anyhow::anyhow!("failed to load VAPID signer: {error}"))?
    } else {
        // Never provision (write) a key into an unattested database. The
        // node is permanently unready in this state, so an ephemeral,
        // unpersisted signer only has to keep the object graph valid.
        crate::push_service::vapid_storage::VapidStorage::load_or_ephemeral(
            state.db_pool.global().clone(),
            server_config.session_key.as_bytes(),
        )
        .await
        .map_err(|error| anyhow::anyhow!("failed to load VAPID signer: {error}"))?
    };
    // Rate-limit outbound sends: global semaphore caps concurrent
    // requests at 64; per-(endpoint, urgency) leaky bucket spaces
    // same-pair sends to at least 100ms apart so one chatty relay+class
    // can't monopolize the global cap.
    let limiter = Arc::new(waddle_xmpp::push::limiter::Limiter::with_defaults());
    let raw_sender: Arc<dyn waddle_xmpp::push::WebPushSender> =
        Arc::new(waddle_xmpp::push::HttpWebPushSender::new());
    let web_push_sender: Arc<dyn waddle_xmpp::push::WebPushSender> = Arc::new(
        waddle_xmpp::push::limiter::RateLimitedWebPushSender::new(raw_sender, limiter),
    );
    // RFC 8292 §2 — the VAPID `sub` claim identifies this push service
    // operator. We use `mailto:postmaster@<xmpp_domain>` per RFC 2142 so
    // relays (FCM, Mozilla autopush, Apple Web Push) have a reachable
    // contact when our deliveries misbehave.
    let vapid_sub = waddle_xmpp::push::types::VapidSub::default_for_domain(&xmpp_domain)
        .map_err(|error| anyhow::anyhow!("failed to derive VAPID sub claim: {error}"))?;
    let push_service = Arc::new(
        crate::push_service::DatabasePushServiceStore::new_with_secret_key_and_pubsub(
            state.db_pool.global().clone(),
            server_config.session_key.as_bytes(),
            service_domains.push.parse()?,
            Arc::clone(&pubsub_storage),
        )
        .await
        .map_err(|error| anyhow::anyhow!("failed to initialize XMPP Push Service: {error}"))?
        .with_web_push_provider(vapid_signer, web_push_sender, vapid_sub),
    );
    // XEP-0050 ad-hoc command handlers for `register-device` and
    // `disable-device` on `push.<domain>`. The command dispatcher
    // routes by node ([`CommandBoundary::PushService`]); registration
    // here is what makes the disco#items + dispatch arms wire up to
    // typed storage calls instead of returning service-unavailable.
    crate::push_service::commands::register(&websocket_command_registry, Arc::clone(&push_service))
        .await;
    let notification_outbox = Arc::new(
        crate::notification_outbox::NotificationOutboxStore::new(state.db_pool.global().clone())
            .await
            .map_err(|error| {
                anyhow::anyhow!("failed to initialize notification candidate outbox: {error}")
            })?,
    );
    let notification_settings_projection = Arc::new(
        crate::notification_settings_projection::NotificationSettingsProjectionStore::new(
            pubsub_database_storage.database(),
        ),
    );
    let dnd_projection = Arc::new(crate::dnd_projection::DndProjectionStore::new(
        pubsub_database_storage.database(),
    ));
    let dnd_reader = Arc::new(crate::dnd_reader::PepDndReader::with_system_clock(
        Arc::clone(&dnd_projection),
    ));
    let notification_activity = Arc::new(
        crate::notification_activity::NotificationActivityStore::new(
            state.db_pool.global().clone(),
        )
        .await
        .map_err(|error| {
            anyhow::anyhow!("failed to initialize notification activity projection: {error}")
        })?,
    );

    let blocking_storage: Arc<dyn waddle_xmpp::xep::xep0191::BlockingStorage> = Arc::new(
        crate::db::blocking::DatabaseBlockingStorage::new(state.db_pool.global().clone()),
    );
    let caps_resolver = Arc::new(crate::server::caps_resolution::CapsResolver::default());

    let provider_ingress = Arc::new(
        crate::server::routes::extension_webhooks::ProviderIngressRegistry::from_env()
            .map_err(|error| anyhow::anyhow!("failed to load provider webhook config: {error}"))?,
    );
    let provider_dispatch_tasks =
        crate::server::routes::extension_webhooks::ProviderDispatchTracker::new();
    let ingress = open_ingress_authority(server_config, &state).await?;
    let websocket_state = Arc::new(WebSocketState {
        deps: WebSocketDeps {
            app_state: state.clone(),
            auth_state: auth_state.clone(),
            service_domains,
            protocol: ProtocolServices {
                connection_registry,
                user_registry,
                room_registry,
                mam_storage,
                inbox_storage: Arc::clone(&state.inbox_storage),
                threads_storage: Arc::new(crate::threads::storage::InboxBackedThreadsStorage::new(
                    Arc::clone(&state.inbox_storage),
                )),
                blocking_storage,
                pending_delivery_storage,
                command_registry: websocket_command_registry,
                extension_manager,
                dispatcher: stanza_dispatcher,
                muji_pre_dispatch_terminate_rate_limit: Arc::new(
                    waddle_xmpp::protocol::handlers::session_initiate_rate_limit::TerminateRateLimit::with_defaults(),
                ),
                muji_pre_dispatch_action_rate_limit: Arc::new(
                    waddle_xmpp::protocol::handlers::session_initiate_rate_limit::MujiActionRateLimit::with_defaults(),
                ),
                pubsub_storage,
                push_store,
                push_service,
                notification_outbox,
                call_teardown_outbox,
                call_teardown_persistence,
                room_effect_outbox,
                room_effect_arm_supervisor: room_effect_arm_supervisor.clone(),
                call_teardown_executor,
                notification_settings_projection,
                dnd_projection,
                dnd_reader,
                notification_activity,
                sm_session_registry,
                ingress,
                link_preview_resolves:
                    crate::server::routes::websocket::default_link_preview_resolve_permits(),
                caps_resolver,
                avatar_source_locks: Arc::new(crate::profile::AvatarLockMap::new()),
                profile_publish_tracker: tokio_util::task::TaskTracker::new(),
                pep_feed_bridge: Arc::new(crate::pep_feed_bridge::PepFeedBridge::new()),
                call_threads: Arc::new(dashmap::DashMap::new()),
                call_thread_end_locks: Arc::new(dashmap::DashMap::new()),
                remote_muc_memberships: Arc::new(
                    crate::server::routes::websocket::RemoteMucMemberships::default(),
                ),
                pending_local_muc_departures: Arc::new(
                    crate::server::routes::websocket::PendingLocalMucDepartures::default(),
                ),
                resolver_affiliation_syncs: Arc::new(
                    crate::server::routes::websocket::ResolverAffiliationSyncScheduler::default(),
                ),
                dm_call_threads: Arc::new(dashmap::DashMap::new()),
                dm_pin_store: Arc::new(crate::server::routes::websocket::DmPinStore::default()),
                dm_call_thread_projections: Arc::new(dashmap::DashSet::new()),
                pending_dm_call_offers: Arc::new(dashmap::DashMap::new()),
                sfu: sfu_service,
            },
            occupant_id_secret: server_config.occupant_id_secret.clone(),
            link_preview: server_config.link_preview.clone(),
            ws_keepalive: server_config.ws_keepalive,
            provider_ingress,
            provider_dispatch_tasks,
            shutdown: shutdown_handle,
        },
    });
    room_effect_arm_supervisor.attach_drain_state(&websocket_state);
    crate::admin::channels::register(
        &websocket_state.deps.protocol.command_registry,
        Arc::clone(&state),
        Arc::clone(&websocket_state),
        Arc::clone(&websocket_state.deps.protocol.connection_registry),
        websocket_state.deps.protocol.user_registry.clone(),
        Arc::clone(&websocket_state.deps.protocol.sm_session_registry),
        websocket_state.deps.protocol.sfu.clone(),
    )
    .await;
    #[cfg(feature = "clustering")]
    if let (Some(bridge), Some((claim_store, node_identity))) = (
        &state.clustering_claims.ordered_relay_delivery_bridge,
        state.clustering_claims.claim_pair(),
    ) {
        if let Some(node_lease) = &state.clustering_claims.node_lease {
            bridge.wire(Arc::new(
                crate::clustering::route_bridge::OrderedRelayDeliveryServices {
                    claim_store,
                    allowlist_store: Arc::new(
                        crate::clustering::allowlist::PostgresAllowlistStore::new(
                            state.db_pool.global().clone(),
                        ),
                    ),
                    node_lease: Arc::clone(node_lease),
                    node_identity,
                    connection_registry: Arc::clone(
                        &websocket_state.deps.protocol.connection_registry,
                    ),
                    user_registry: websocket_state.deps.protocol.user_registry.clone(),
                    sm_session_registry: Arc::clone(
                        &websocket_state.deps.protocol.sm_session_registry,
                    ),
                    blocking_storage: Arc::clone(&websocket_state.deps.protocol.blocking_storage),
                    web_socket_state: Arc::downgrade(&websocket_state),
                },
            ));
        } else {
            warn!("ordered relay delivery bridge not wired: clustering node lease handle missing");
        }
    }
    deferred_extension_host_tools.set(Arc::new(extension_host_adapter::ExtensionHostAdapter::new(
        Arc::clone(&websocket_state),
    )));
    // Start the SFU ghost-reconciliation backstop: periodically ask
    // LiveKit who is actually connected and sweep registry entries (and
    // their MUC Muji presence) left behind by a lost
    // `participant_left`/`room_finished` webhook delivery. No-op when
    // A/V calling is unconfigured (`sfu_reconciler` is `None`).
    if let Some(reconciler) = sfu_reconciler {
        if lineage_attested {
            routes::livekit_webhook::spawn_reconciliation_task(
                Arc::clone(&websocket_state),
                reconciler,
            );
        } else {
            warn!("skipping LiveKit ghost reconciliation: lineage attestation failed");
        }
    }
    Ok(websocket_state)
}

/// Refuse application-route requests while this node is not admitting
/// clients — the same [`crate::clustering::NodeLifecycle`] gate WebSocket
/// upgrades use. Keeps a lineage-latched, fenced, or draining node from
/// reading or mutating application state over plain HTTP through a database
/// it cannot vouch for. Operational endpoints are mounted outside this gate.
async fn admission_gate(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    request: axum::extract::Request,
    next: middleware::Next,
) -> axum::response::Response {
    if let Err(error) = state.node_lifecycle.admit() {
        warn!(%error, path = %request.uri().path(), "refusing application request: node not admitting");
        use axum::response::IntoResponse as _;
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            axum::response::Json(serde_json::json!({
                "status": "not_ready",
                "error": "node is not admitting application traffic",
            })),
        )
            .into_response();
    }
    next.run(request).await
}

/// Startup-only promotion plus the janitor fleet. Called exactly once per
/// process, when startup attestation passed. Returns without spawning
/// anything if a fence/drain/failure won the race to leave `Starting`.
fn promote_to_serving_and_spawn_janitors(
    websocket_state: &Arc<WebSocketState>,
    orphan_reaper_interval: std::time::Duration,
) {
    // Both critical registries have lifetime supervision before this runs.
    // Only after that fence is armed may a node still in `Starting`
    // transition to `Serving`. A lease fence/drain/failure that won during
    // slow startup remains authoritative until its own recovery path
    // explicitly serves.
    if let crate::clustering::StartupServingTransition::Blocked(admission) = websocket_state
        .deps
        .app_state
        .node_lifecycle
        .finish_startup()
    {
        // A fence/drain/failure won the race during slow startup: preserve
        // the non-serving state, but STILL start the janitor fleet. The
        // janitors are fence-aware (they run through runtime fences and
        // drains in steady state), and clustering's later lease-recovery
        // `serve()` re-admits clients without re-running this function — a
        // node recovered that way must not serve janitor-less forever.
        // Lineage attestation already passed (the only caller gates on it),
        // so the fail-closed no-janitors state applies solely to unattested
        // nodes.
        warn!(
            ?admission,
            "promotion attempted after node admission left Starting; preserving non-serving state"
        );
    }
    spawn_sm_expiry_janitor(websocket_state);
    spawn_orphan_reaper_janitor(websocket_state, orphan_reaper_interval);
    spawn_pending_delivery_claim_janitor(websocket_state);
    spawn_notification_outbox_janitor(websocket_state);
    spawn_call_teardown_outbox_janitor(websocket_state);
    spawn_room_effect_outbox_janitor(websocket_state);
    spawn_push_service_publish_job_janitor(websocket_state);
    spawn_auth_state_janitor(websocket_state);
    spawn_destroy_completion_janitor(websocket_state);
    spawn_room_dormancy_janitor(websocket_state);
    spawn_user_actor_reaper(websocket_state);
    crate::server::session_janitors::spawn_local_muc_departure_janitor(websocket_state);
    #[cfg(feature = "clustering")]
    crate::server::session_janitors::spawn_remote_muc_membership_reconciler(websocket_state);
    crate::server::state_inventory_metrics::spawn_state_inventory_publisher(websocket_state);
}

/// One startup attestation pass with a short bounded retry for transient
/// errors and an overall deadline. Definitive lineage failures do not retry.
async fn attest_startup_lineage(state: &AppState) -> crate::db::lineage::LineageReport {
    const ATTEMPTS: u32 = 3;
    const RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(2);
    const OVERALL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);
    // Bounds each boundary's probe so one stalled pool cannot withhold
    // another boundary's definitive answer for the whole deadline.
    const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

    let Some(registry) = state.lineage_registry.get() else {
        return crate::db::lineage::LineageReport::initializing();
    };
    // Each completed pass lands in this slot so the overall deadline cannot
    // discard an already-returned DEFINITIVE report (one boundary answering
    // a mismatch while another stalls must latch, not fabricate a
    // transient-only timeout that exit-and-retries forever).
    let completed: std::sync::Mutex<Option<crate::db::lineage::LineageReport>> =
        std::sync::Mutex::new(None);
    let pass = async {
        let mut last = registry
            .attest(
                &state.lineage_config,
                state.clustering_enabled,
                PROBE_TIMEOUT,
            )
            .await;
        if let Ok(mut slot) = completed.lock() {
            *slot = Some(last.clone());
        }
        for _ in 1..ATTEMPTS {
            if !last.is_transient_only() {
                break;
            }
            tokio::time::sleep(RETRY_DELAY).await;
            last = registry
                .attest(
                    &state.lineage_config,
                    state.clustering_enabled,
                    PROBE_TIMEOUT,
                )
                .await;
            if let Ok(mut slot) = completed.lock() {
                *slot = Some(last.clone());
            }
        }
        last
    };
    let report = match tokio::time::timeout(OVERALL_DEADLINE, pass).await {
        Ok(report) => report,
        Err(_) => completed
            .lock()
            .ok()
            .and_then(|mut slot| slot.take())
            .unwrap_or_else(crate::db::lineage::LineageReport::timeout),
    };
    let unmatched = state.lineage_adopt_unmatched();
    if !unmatched.is_empty() {
        // Not a readiness failure: after a successful adoption (or a pod
        // restart with the one-shot action still rendered) the replayed
        // entries legitimately match nothing, while a restore that still
        // needs adoption keeps failing on its own identity mismatch. Loud
        // so an operator TYPO is still visible.
        for uuid in &unmatched {
            warn!(
                unmatched = %uuid,
                "lineage adopt entry matched no boundary; if this adoption already \
                 completed, remove WADDLE_DB_LINEAGE_ACTION — otherwise check the UUID"
            );
        }
    }
    report
}

/// Build the [`TestSeamAuth`] for the test profile-publish route, or
/// `None` if either the fixed-account flag or the per-process token is
/// not configured. The token is consumed from
/// `WADDLE_TEST_PROFILE_PUBLISH_TOKEN`; the JID allowlist is derived
/// from `WADDLE_TEST_FIXED_ACCOUNT_USERNAME` (default `admin`) plus
/// any `WADDLE_TEST_EXTRA_FIXED_ACCOUNTS` entries, all in the
/// configured XMPP domain.
fn build_test_profile_publish_auth(
    xmpp_domain: &str,
) -> Option<crate::server::profile_publish_route::TestSeamAuth> {
    use crate::server::profile_publish_route::TestSeamAuth;

    if !crate::server::fixed_account::fixed_test_account_enabled() {
        return None;
    }

    let token = std::env::var("WADDLE_TEST_PROFILE_PUBLISH_TOKEN")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let Some(token) = token else {
        warn!(
            "WADDLE_TEST_FIXED_ACCOUNT_ENABLED is set but WADDLE_TEST_PROFILE_PUBLISH_TOKEN is empty — \
             not mounting /api/test/profile-publish (set the token to enable wire-conformance tests)"
        );
        return None;
    };

    let mut localparts: Vec<String> = vec![std::env::var("WADDLE_TEST_FIXED_ACCOUNT_USERNAME")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "admin".to_string())];
    if let Ok(extras) = std::env::var("WADDLE_TEST_EXTRA_FIXED_ACCOUNTS") {
        for entry in extras.split(',') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            let username = entry
                .split_once(':')
                .map(|(u, _)| u)
                .unwrap_or(entry)
                .trim();
            if !username.is_empty() {
                localparts.push(username.to_string());
            }
        }
    }
    let allowed_jids: Vec<jid::BareJid> = localparts
        .into_iter()
        .filter_map(|lp| format!("{}@{}", lp, xmpp_domain).parse().ok())
        .collect();

    if allowed_jids.is_empty() {
        warn!(
            "WADDLE_TEST_PROFILE_PUBLISH_TOKEN is set but no valid fixed-account JID could be \
             parsed — not mounting /api/test/profile-publish"
        );
        return None;
    }

    Some(TestSeamAuth {
        token,
        allowed_jids,
    })
}

/// Build the OIDC → PEP profile-publish hook with cheap-clone deps
/// captured, and install it onto `auth_state`. The hook is invoked
/// from `auth/callback.rs` after every successful OIDC login (within
/// `tokio::spawn`, so login latency is unaffected).
fn install_profile_publish_hook(
    auth_state: &Arc<AuthState>,
    websocket_state: Arc<routes::websocket::WebSocketState>,
) {
    use crate::profile::{
        ensure_pep_profile_published, FetchPolicy, ProfilePublishDeps, ProfileSource,
    };

    let db_actor = websocket_state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .clone();

    let tracker = websocket_state
        .deps
        .protocol
        .profile_publish_tracker
        .clone();
    let hook: super::routes::auth::ProfilePublishHook =
        std::sync::Arc::new(move |jid: jid::BareJid, source: ProfileSource| {
            let websocket_state = Arc::clone(&websocket_state);
            let db_actor = db_actor.clone();
            // Carry the JID into the spawned task's tracing span so a
            // failure logged inside the future is correlated to the
            // user even after the closure capture is gone.
            let span = tracing::info_span!("oidc_profile_publish", jid = %jid);
            let fut = async move {
                let db = match db_actor.ask(crate::db::actor::GetDatabase).await {
                    Ok(db) => db,
                    Err(error) => {
                        warn!(
                            error = %error,
                            "OIDC profile publish: failed to acquire database; skipping"
                        );
                        return;
                    }
                };
                let deps = ProfilePublishDeps {
                    state: websocket_state,
                    vcard_store: crate::vcard::VCardStore::new(db.into()),
                    fetch_policy: FetchPolicy::default(),
                };
                if let Err(error) = ensure_pep_profile_published(&deps, &jid, source).await {
                    warn!(error = %error, "OIDC profile publish chain failed (background)");
                }
            };
            use tracing::Instrument;
            // Register with the shutdown tracker so the
            // graceful-shutdown drain can `close().wait()` on
            // in-flight publishes before tearing down the runtime.
            // Closed trackers refuse new spawns; the auth callback
            // accepts that as "shutting down — skip this publish".
            // The `_handle` binding (rather than `_`) is intentional:
            // detaching the JoinHandle is exactly what we want here
            // (the tracker holds its own reference for `wait()`).
            let _handle = tracker.spawn(fut.instrument(span));
        });
    auth_state.install_profile_publish_hook(hook);
    tracing::debug!("OIDC → PEP profile-publish hook installed");
}

/// Maximum number of detached XEP-0198 sessions held resumable at
/// once (issue #1097 sizing item). Overflow evicts the oldest session
/// through the promote → confirm chain (no message loss), but each
/// eviction still costs its owner the resume window — size this above
/// the expected concurrent detached-session peak (roughly: peak
/// concurrent users × resume-window churn rate).
fn sm_max_sessions_from_env() -> usize {
    const MIN_SESSIONS: usize = 100;
    const MAX_SESSIONS: usize = 10_000_000;
    std::env::var("WADDLE_SM_MAX_SESSIONS")
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .map(|v| v.clamp(MIN_SESSIONS, MAX_SESSIONS))
        .unwrap_or(waddle_xmpp::stream_management::DEFAULT_MAX_SESSIONS)
}

async fn create_sm_session_registry(
    xmpp_config: &XmppConfig,
    server_config: &ServerConfig,
    clustering: &crate::clustering::ClusteringHandles,
    global_db: &crate::db::Database,
    state: &Arc<AppState>,
) -> Result<Arc<waddle_xmpp::stream_management::InMemorySmSessionRegistry>> {
    let sm_database_url = xmpp_config.sm_database_url.clone();
    if sm_database_url.is_ephemeral_fallback() {
        warn!(
            "Neither WADDLE_XMPP_SM_DATABASE_URL nor WADDLE_DATABASE_URL is set; \
             falling back to in-memory SQLite for SM session persistence. \
             Detached XEP-0198 sessions will NOT survive restart. Set one of \
             these env vars for durable session resumption (issue #209)."
        );
    }
    // ADR-0017 Phase 3 Slice 4: cluster mode selects the Postgres-fenced
    // `SmPersistenceStorage`; every other deployment shape (clustering
    // disabled, non-Postgres, or a build without the `clustering` feature)
    // keeps today's portable, single-node implementation. All of that
    // branching — including FIX 4's co-location check against `global_db`,
    // the same handle `clustering::start_if_enabled` itself received —
    // lives inside `open_for_cluster_mode` itself.
    let opened = crate::sm_persistence::open_for_cluster_mode_with_lineage(
        sm_database_url.as_deref(),
        server_config.clustering.enabled,
        clustering.claim_pair(),
        global_db,
    )
    .await?;
    if opened.aliases_global {
        state.register_lineage_alias(
            crate::db::lineage::DurableStore::Sm,
            crate::db::lineage::DurableStore::Global,
        );
    } else if let Some(database) = opened.database.clone() {
        if database.is_in_memory_sqlite() {
            state.register_lineage_ephemeral(crate::db::lineage::DurableStore::Sm);
        } else {
            crate::server::bootstrap_store_lineage(
                state,
                crate::db::lineage::DurableStore::Sm,
                database.clone(),
            )
            .await?;
            state.register_lineage_database(crate::db::lineage::DurableStore::Sm, database);
        }
    }
    let sm_persistence = opened.storage;
    let mut sm_session_registry =
        waddle_xmpp::stream_management::InMemorySmSessionRegistry::with_capacity(
            sm_max_sessions_from_env(),
        )
        .with_persistence(Arc::clone(&sm_persistence));
    // ADR-0017 Phase 3 Slice 5: wire the SAME `ClaimStore`/live-identity
    // pair `clustering::start_if_enabled` constructed (never a second,
    // independent store) into the session registry's own claim
    // bookkeeping — without this, `claim_session`/`restore_from_persistence`
    // silently stay on the single-node `InProcessClaimStore` default even
    // with clustering enabled and the fenced `SmPersistenceStorage` wired
    // in above, defeating acquire-then-hydrate entirely. A `None` pair
    // (clustering disabled, non-Postgres, or a build without the
    // `clustering` feature) leaves today's single-node default untouched.
    // The registry's own single-node default is already an
    // ObservedClaimStore-wrapped InProcessClaimStore (#1648).
    if let Some((claim_store, node_identity)) = clustering.claim_pair() {
        sm_session_registry = sm_session_registry.with_claim_store(claim_store, node_identity);
    }
    // ADR-0017 Phase 3 Slice 6: wire the cross-node resume live-handshake
    // asker (over `RelayHandle`) alongside the claim store above — both are
    // `None`/absent under the exact same conditions (clustering disabled,
    // non-Postgres, or a build without the `clustering` feature), so the
    // cross-node resume fallback never has anything to ask in those cases
    // and single-node behavior stays byte-identical.
    #[cfg(feature = "clustering")]
    if let Some(stop_token) = &clustering.stop_token {
        let asker = crate::clustering::resume_asker::SwarmRemoteResumeAsker::new(
            stop_token.clone(),
            server_config.clustering.messaging.mailbox_timeout,
            server_config.clustering.messaging.reply_timeout,
        );
        sm_session_registry = sm_session_registry.with_remote_resume_asker(Arc::new(asker));
    }
    if let Err(error) = sm_session_registry.restore_from_persistence().await {
        warn!(
            error = %error,
            "restore_from_persistence failed at startup; continuing with empty \
             in-memory SM session view. XEP-0198 resume will return <failed/> \
             until storage health is restored."
        );
    }
    let sm_session_registry = Arc::new(sm_session_registry);
    // ADR-0017 Phase 3 Slice 5 (carried debt (b)): complete the
    // `LocallyClaimedEntities` `clustering::start_if_enabled` handed to
    // `self_fence::run_node_lease` empty — see
    // `clustering::local_claims::SmSessionLocalClaims`'s doc comment for
    // the full construction-order rationale. A no-op when clustering is
    // disabled or this binary lacks the `clustering` feature
    // (`clustering.local_claims` is `None`).
    #[cfg(feature = "clustering")]
    if let Some(local_claims) = &clustering.local_claims {
        local_claims.wire(Arc::clone(&sm_session_registry));
    }
    Ok(sm_session_registry)
}

async fn create_pending_delivery_storage(
    xmpp_config: &XmppConfig,
    server_config: &ServerConfig,
    clustering: &crate::clustering::ClusteringHandles,
    global_db: &crate::db::Database,
    state: &Arc<AppState>,
) -> Result<Arc<dyn waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage>> {
    let pending_delivery_url = xmpp_config.pending_delivery_database_url.clone();
    if pending_delivery_url.is_ephemeral_fallback() {
        warn!(
            "Neither WADDLE_XMPP_PENDING_DELIVERY_DATABASE_URL nor \
             WADDLE_DATABASE_URL is set; falling back to in-memory SQLite. \
             Offline DMs queued via XEP-0160 will NOT survive restart. \
             Set one of these env vars to a SQLite path or Postgres URL \
             for durable offline delivery (issue #209)."
        );
    }
    // ADR-0017 Phase 3 Slice 5 FIX 3: cluster mode attaches claim-fenced
    // Q6-promotion inserts; every other deployment shape (clustering
    // disabled, non-Postgres, or a build without the `clustering` feature)
    // keeps today's portable, unfenced path. All of that branching —
    // including the co-location check against `global_db` — lives inside
    // `open_for_cluster_mode` itself, mirroring `create_sm_session_registry`'s
    // identical `sm_persistence::open_for_cluster_mode` call above.
    let storage = crate::pending_delivery::open_for_cluster_mode(
        pending_delivery_url.as_deref(),
        waddle_xmpp::pending_delivery::QuotaPolicy::default_policy(),
        server_config.clustering.enabled,
        clustering.claim_pair(),
        global_db,
    )
    .await?;
    let database = storage.database();
    if database.is_in_memory_sqlite() {
        state.register_lineage_ephemeral(crate::db::lineage::DurableStore::PendingDelivery);
    } else {
        crate::server::bootstrap_store_lineage(
            state,
            crate::db::lineage::DurableStore::PendingDelivery,
            database.clone(),
        )
        .await?;
        state
            .register_lineage_database(crate::db::lineage::DurableStore::PendingDelivery, database);
    }
    Ok(Arc::new(storage))
}

fn ensure_push_service_global_database_is_durable() -> Result<()> {
    if cfg!(test) {
        return Ok(());
    }
    let db_runtime = crate::config::DatabaseRuntimeConfig::from_env()
        .map_err(|error| anyhow::anyhow!("failed to load database runtime config: {error}"))?;
    if push_service_database_is_restart_durable(&db_runtime) {
        return Ok(());
    }
    if env_flag("WADDLE_XMPP_PUSH_SERVICE_ALLOW_IN_MEMORY") {
        warn!(
            "WADDLE_XMPP_PUSH_SERVICE_ALLOW_IN_MEMORY is set; XEP-0357 Push Service \
             publish jobs are using in-memory SQLite and will NOT survive restart. \
             Use only in tests."
        );
        return Ok(());
    }
    anyhow::bail!(
        "XEP-0357 Push Service publish jobs require durable global storage. \
         Set WADDLE_DATABASE_URL to a durable SQLite/Postgres DSN, or set \
         WADDLE_XMPP_PUSH_SERVICE_ALLOW_IN_MEMORY=true only for tests."
    );
}

pub(crate) fn push_service_database_is_restart_durable(
    db_runtime: &crate::config::DatabaseRuntimeConfig,
) -> bool {
    match db_runtime.driver {
        crate::db::DatabaseDriver::Postgres => true,
        crate::db::DatabaseDriver::Sqlite => {
            !crate::db::sqlite_url_is_in_memory(&db_runtime.database_url)
        }
    }
}

fn env_flag(key: &str) -> bool {
    std::env::var(key)
        .map(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod shutdown_bound_tests {
    use super::await_http_server_or_forced_exit;

    #[tokio::test(start_paused = true)]
    async fn stalled_connection_work_cannot_outlive_http_shutdown_deadline() {
        let stop = tokio_util::sync::CancellationToken::new();
        stop.cancel();

        let outcome = await_http_server_or_forced_exit(
            std::future::pending::<()>(),
            stop,
            std::time::Duration::from_millis(10),
        )
        .await;

        assert!(outcome.is_none());
    }

    #[tokio::test]
    async fn completed_http_server_wins_before_forced_exit() {
        let stop = tokio_util::sync::CancellationToken::new();
        let outcome = await_http_server_or_forced_exit(
            std::future::ready(7_u8),
            stop,
            std::time::Duration::from_secs(30),
        )
        .await;

        assert_eq!(outcome, Some(7));
    }
}

#[cfg(test)]
mod admin_failure_observer_tests {
    use super::LiveKitAdminFailureObserver;

    #[tokio::test]
    async fn observer_emits_admin_failure_counter() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        waddle_sfu::AdminCallObserver::admin_call_failed(
            &LiveKitAdminFailureObserver,
            waddle_sfu::AdminOp::ListRooms,
        );
        assert_eq!(
            metrics.counter_sum("waddle.call.admin.call_failed", &[("op", "list_rooms")]),
            Some(1)
        );
    }
}

#[cfg(test)]
mod ingress_colocation_tests {
    use super::*;

    #[tokio::test]
    async fn private_memory_router_fixture_shares_the_ingress_pool() {
        let global = crate::db::Database::in_memory("router-ingress")
            .await
            .expect("global");
        global
            .execute("CREATE TABLE shared_pool_marker (id INTEGER)")
            .await
            .expect("marker");
        let mam = create_websocket_mam_storage(None, false, false, &global)
            .await
            .expect("shared fixture MAM");
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'shared_pool_marker'",
        )
        .fetch_one(mam.sqlite_pool().expect("SQLite pool"))
        .await
        .expect("shared marker");
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn sqlite_requires_the_same_durable_file_without_clustering() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let global = crate::db::Database::open_local(
            "ingress-colocation",
            directory.path().join("global.db"),
        )
        .await
        .expect("global database");
        let colocated = create_websocket_mam_storage(
            Some(global.database_url().to_owned()),
            false,
            false,
            &global,
        )
        .await;
        assert!(colocated.is_ok());
        let separate =
            crate::db::Database::open_local("separate-mam", directory.path().join("mam.db"))
                .await
                .expect("separate database");
        let error = create_websocket_mam_storage(
            Some(separate.database_url().to_owned()),
            false,
            false,
            &global,
        )
        .await
        .err()
        .expect("reject separate file");
        assert!(error.downcast_ref::<MamIngressColocationError>().is_some());
    }

    #[tokio::test]
    async fn postgres_requires_the_same_schema_without_clustering() {
        let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
            eprintln!("skipping postgres_requires_the_same_schema_without_clustering: WADDLE_TEST_POSTGRES_URL not set");
            return;
        };
        let admin = sqlx::PgPool::connect(&database_url)
            .await
            .expect("postgres admin");
        let global_schema = format!("ingress_colocation_{}", uuid::Uuid::new_v4().simple());
        let separate_schema = format!("mam_colocation_{}", uuid::Uuid::new_v4().simple());
        for schema in [&global_schema, &separate_schema] {
            sqlx::query(&format!("CREATE SCHEMA {schema}"))
                .execute(&admin)
                .await
                .expect("create schema");
        }
        let mut global_url = url::Url::parse(&database_url).expect("database URL");
        global_url
            .query_pairs_mut()
            .append_pair("options", &format!("-c search_path={global_schema}"));
        let mut separate_url = url::Url::parse(&database_url).expect("database URL");
        separate_url
            .query_pairs_mut()
            .append_pair("options", &format!("-c search_path={separate_schema}"));
        let config = crate::db::DatabaseConfig::new(
            crate::db::DatabaseDriver::Postgres,
            global_url.to_string(),
        );
        let global = crate::db::Database::from_config("ingress-colocation", &config)
            .await
            .expect("global");
        let shared =
            create_websocket_mam_storage(Some(global_url.to_string()), false, false, &global)
                .await
                .expect("colocated MAM");
        assert!(create_websocket_mam_storage(
            Some(separate_url.to_string()),
            false,
            false,
            &global
        )
        .await
        .is_err());
        drop(shared);
        drop(global);
        for schema in [&global_schema, &separate_schema] {
            sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
                .execute(&admin)
                .await
                .expect("drop schema");
        }
    }

    #[tokio::test]
    async fn sqlite_rejects_private_memory_as_colocated() {
        let global = crate::db::Database::in_memory("ingress-colocation")
            .await
            .expect("global");
        let mam = SqlxMamStorage::open_in_memory().await.expect("MAM");
        assert!(ensure_mam_ingress_colocation(&mam, &global).await.is_err());
    }
}
