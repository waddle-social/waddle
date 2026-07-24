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
    spawn_auth_state_janitor, spawn_graceful_shutdown_drain, spawn_notification_outbox_janitor,
    spawn_orphan_reaper_janitor, spawn_pending_delivery_claim_janitor,
    spawn_push_service_publish_job_janitor, spawn_room_dormancy_janitor, spawn_sm_expiry_janitor,
    spawn_user_actor_reaper,
};
use crate::server::topology::bootstrap_fresh_xmpp_topology;
use crate::server::trace::{attach_http_route_template, make_request_span, observe_http_response};
use crate::server::{AppState, XmppConfig};
use anyhow::Result;
use axum::{middleware, routing::get, Router};
use rustls_acme::tower::TowerHttp01ChallengeService;
use sqlx::sqlite::SqliteConnectOptions;
use std::str::FromStr;
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

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            stop_token.cancelled().await;
            info!("HTTP server received shutdown signal; awaiting SM Q6 drain");
            drain_complete.notified().await;
            info!("HTTP server: SM drain complete; draining connections");
        })
        .await?;

    Ok(())
}

/// ADR-0017 Phase 3 Slice 7 FIX 1 (council-adjudicated): open MAM storage
/// for cluster mode, mirroring
/// `pending_delivery::open_for_cluster_mode`'s/`sm_persistence::open_for_cluster_mode`'s
/// identical co-location-then-construct pattern, one table over. Lives here
/// (in `waddle-server`, not `waddle-xmpp` where `SqlxMamStorage` itself is
/// defined) because the co-location mismatch error needs
/// `crate::db::redact_database_url` — a `waddle-server`-only helper — and
/// because the clustering global database URL only exists on this side of
/// the crate boundary.
///
/// When clustering is enabled AND the resolved `database_url` is a
/// Postgres DSN, the co-location invariant is checked BEFORE fencing is
/// ever attached: the resolved URL must be an EXACT string match for
/// `global_db.database_url()` (deliberately no DSN normalization, matching
/// every sibling `open_for_cluster_mode`) — the fencing
/// `SELECT ... FOR SHARE` `SqlxMamStorage::store_message_fenced` issues
/// targets `clustering_claims`, which only exists in the clustering global
/// database. A mismatch fails startup with
/// [`waddle_xmpp::mam::storage::MamStorageError::ClusterColocationMismatch`]
/// rather than silently fencing (or, on a build without this check,
/// silently NOT fencing) against a table that may not exist wherever
/// `database_url` actually points.
///
/// When clustering is disabled, `database_url` is not a
/// `postgres://`/`postgresql://` DSN, or the clustering subsystem produced
/// no live claim pair, this is exactly `SqlxMamStorage::open` + `quota` —
/// no fencing attached, the unfenced path exactly as before.
pub(crate) async fn create_websocket_mam_storage(
    database_url: Option<String>,
    clustering_enabled: bool,
    clustering_claim_pair_live: bool,
    global_db: &crate::db::Database,
) -> Result<Arc<dyn MamStorage>> {
    if !cfg!(test) {
        ensure_mam_database_is_durable(
            database_url.as_deref(),
            env_flag("WADDLE_XMPP_MAM_ALLOW_IN_MEMORY"),
        )?;
    }
    let storage = match database_url.as_deref() {
        Some(database_url) => {
            let opened = SqlxMamStorage::open(database_url).await.map_err(|error| {
                anyhow::anyhow!("Failed to initialize WebSocket MAM storage: {error}")
            })?;
            #[cfg(feature = "clustering")]
            {
                let resolved_url = (database_url.starts_with("postgres://")
                    || database_url.starts_with("postgresql://"))
                .then_some(database_url);
                if let (true, true, Some(resolved_url)) =
                    (clustering_enabled, clustering_claim_pair_live, resolved_url)
                {
                    if resolved_url != global_db.database_url() {
                        return Err(anyhow::anyhow!(
                            waddle_xmpp::mam::MamStorageError::ClusterColocationMismatch {
                                mam_database_url: crate::db::redact_database_url(resolved_url),
                                global_database_url: crate::db::redact_database_url(
                                    global_db.database_url()
                                ),
                            }
                        ));
                    }
                    opened.with_cluster_fencing(true)
                } else {
                    opened
                }
            }
            #[cfg(not(feature = "clustering"))]
            {
                let _ = (clustering_enabled, clustering_claim_pair_live, global_db);
                opened
            }
        }
        None => SqlxMamStorage::open_in_memory().await.map_err(|error| {
            anyhow::anyhow!("Failed to initialize WebSocket MAM storage: {error}")
        })?,
    };

    Ok(Arc::new(storage))
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
    {
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
    }

    spawn_sm_expiry_janitor(&websocket_state);
    spawn_orphan_reaper_janitor(
        &websocket_state,
        server_config.clustering.orphan_reaper_interval,
    );
    spawn_pending_delivery_claim_janitor(&websocket_state);
    spawn_notification_outbox_janitor(&websocket_state);
    spawn_push_service_publish_job_janitor(&websocket_state);
    spawn_auth_state_janitor(&websocket_state);
    spawn_room_dormancy_janitor(&websocket_state);
    spawn_user_actor_reaper(&websocket_state);
    #[cfg(feature = "clustering")]
    crate::server::session_janitors::spawn_remote_muc_membership_reconciler(&websocket_state);
    crate::server::state_inventory_metrics::spawn_state_inventory_publisher(&websocket_state);
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

    router = router
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
        router = router.merge(crate::server::profile_publish_route::router(
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
        router = router.merge(crate::server::state_inventory_route::router(
            websocket_state.clone(),
            auth,
        ));
    }

    // Always merge common routes required by XMPP, auth, upload, and operations.
    let router = router
        // Merge XMPP over WebSocket endpoint
        .merge(websocket_router)
        // Merge well-known endpoints for XMPP service discovery
        .merge(well_known_router)
        // Merge upload routes for XEP-0363 HTTP File Upload
        .merge(upload_router)
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
    #[cfg(feature = "clustering")]
    if let Some((claim_store, node_identity)) = state.clustering_claims.claim_pair() {
        if let Err(error) = user_registry
            .tell(waddle_xmpp::registry::WireUserClusteringClaims {
                claim_store,
                node_identity,
            })
            .mailbox_timeout(std::time::Duration::from_secs(2))
            .await
        {
            warn!(
                %error,
                "failed to wire clustering claims into the user registry"
            );
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
    match waddle_sfu::SfuConfig::from_env() {
        Ok(Some(sfu_config)) => {
            let turn_tls_port = sfu_config.turn_tls_port;
            let turn_udp_port = sfu_config.turn_udp_port;
            match waddle_sfu::LiveKitSfu::new(sfu_config) {
                Ok(sfu_impl) => {
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
    )
    .await;
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
    crate::admin::channels::register(
        &websocket_command_registry,
        Arc::clone(&state),
        Arc::clone(&connection_registry),
        user_registry.clone(),
        Arc::clone(&sm_session_registry),
        sfu_service.clone(),
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
    let vapid_signer = crate::push_service::vapid_storage::VapidStorage::load_or_provision(
        state.db_pool.global().clone(),
        server_config.session_key.as_bytes(),
    )
    .await
    .map_err(|error| anyhow::anyhow!("failed to load VAPID signer: {error}"))?;
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

    let resumable_sessions: Arc<dashmap::DashMap<String, crate::auth::Session>> =
        Arc::new(dashmap::DashMap::new());
    let blocking_storage: Arc<dyn waddle_xmpp::xep::xep0191::BlockingStorage> = Arc::new(
        crate::db::blocking::DatabaseBlockingStorage::new(state.db_pool.global().clone()),
    );
    let pending_delivery_storage = create_pending_delivery_storage(
        xmpp_config,
        server_config,
        &state.clustering_claims,
        state.db_pool.global(),
    )
    .await;

    let caps_resolver = Arc::new(crate::server::caps_resolution::CapsResolver::default());

    let provider_ingress = Arc::new(
        crate::server::routes::extension_webhooks::ProviderIngressRegistry::from_env()
            .map_err(|error| anyhow::anyhow!("failed to load provider webhook config: {error}"))?,
    );
    let provider_dispatch_tasks =
        crate::server::routes::extension_webhooks::ProviderDispatchTracker::new();
    let websocket_state = Arc::new(WebSocketState {
        deps: WebSocketDeps {
            app_state: state.clone(),
            auth_state: auth_state.clone(),
            transport_security: routes::websocket::TransportSecurity::from_public_websocket_url(
                &xmpp_config.public_websocket_url,
            ),
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
                pubsub_storage,
                push_store,
                push_service,
                notification_outbox,
                notification_settings_projection,
                dnd_projection,
                dnd_reader,
                notification_activity,
                sm_session_registry,
                link_preview_resolves:
                    crate::server::routes::websocket::default_link_preview_resolve_permits(),
                resumable_sessions,
                caps_resolver,
                avatar_source_locks: Arc::new(crate::profile::AvatarLockMap::new()),
                profile_publish_tracker: tokio_util::task::TaskTracker::new(),
                pep_feed_bridge: Arc::new(crate::pep_feed_bridge::PepFeedBridge::new()),
                call_threads: Arc::new(dashmap::DashMap::new()),
                remote_muc_memberships: Arc::new(
                    crate::server::routes::websocket::RemoteMucMemberships::default(),
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
        routes::livekit_webhook::spawn_reconciliation_task(
            Arc::clone(&websocket_state),
            reconciler,
        );
    }
    Ok(websocket_state)
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
) -> Arc<waddle_xmpp::stream_management::InMemorySmSessionRegistry> {
    let sm_database_url = xmpp_config.sm_database_url.clone();
    if sm_database_url.is_none() {
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
    let sm_persistence: Arc<dyn waddle_xmpp::stream_management::persistence::SmPersistenceStorage> =
        crate::sm_persistence::open_for_cluster_mode(
            sm_database_url.as_deref(),
            server_config.clustering.enabled,
            clustering.claim_pair(),
            global_db,
        )
        .await
        .expect("open SM persistence storage");
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
    sm_session_registry
}

async fn create_pending_delivery_storage(
    xmpp_config: &XmppConfig,
    server_config: &ServerConfig,
    clustering: &crate::clustering::ClusteringHandles,
    global_db: &crate::db::Database,
) -> Arc<dyn waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage> {
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
    // ADR-0017 Phase 3 Slice 5 FIX 3: cluster mode attaches claim-fenced
    // Q6-promotion inserts; every other deployment shape (clustering
    // disabled, non-Postgres, or a build without the `clustering` feature)
    // keeps today's portable, unfenced path. All of that branching —
    // including the co-location check against `global_db` — lives inside
    // `open_for_cluster_mode` itself, mirroring `create_sm_session_registry`'s
    // identical `sm_persistence::open_for_cluster_mode` call above.
    Arc::new(
        crate::pending_delivery::open_for_cluster_mode(
            pending_delivery_url.as_deref(),
            waddle_xmpp::pending_delivery::QuotaPolicy::default_policy(),
            server_config.clustering.enabled,
            clustering.claim_pair(),
            global_db,
        )
        .await
        .expect("open pending_delivery storage"),
    )
}

/// Fail-fast guard for XEP-0313 MAM storage durability. A missing or
/// in-memory database URL is only acceptable for dev/test runs that
/// explicitly opt in via `WADDLE_XMPP_MAM_ALLOW_IN_MEMORY`.
pub(crate) fn ensure_mam_database_is_durable(
    database_url: Option<&str>,
    allow_in_memory: bool,
) -> Result<()> {
    if database_url.is_some_and(|url| !sqlite_url_is_in_memory(url)) {
        return Ok(());
    }
    if allow_in_memory {
        warn!(
            "WADDLE_XMPP_MAM_ALLOW_IN_MEMORY is set; the XEP-0313 MAM archive \
             is using in-memory SQLite and will NOT survive restart. \
             Use only in tests."
        );
        return Ok(());
    }
    anyhow::bail!(
        "XEP-0313 MAM archive requires durable storage. Set \
         WADDLE_XMPP_MAM_DATABASE_URL (or the WADDLE_DATABASE_URL fallback) \
         to a durable SQLite/Postgres DSN, or set \
         WADDLE_XMPP_MAM_ALLOW_IN_MEMORY=true only for tests."
    );
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
        crate::db::DatabaseDriver::Sqlite => !sqlite_url_is_in_memory(&db_runtime.database_url),
    }
}

fn sqlite_url_is_in_memory(database_url: &str) -> bool {
    let trimmed = database_url.trim().to_ascii_lowercase();
    let base = trimmed
        .split_once('?')
        .map_or(trimmed.as_str(), |(base, _)| base);
    if matches!(
        base,
        ":memory:" | "sqlite::memory:" | "sqlite://{memory}:" | "sqlite:///{memory}:"
    ) || base.ends_with("file::memory:")
        || sqlite_url_query_requests_memory(&trimmed)
    {
        return true;
    }
    SqliteConnectOptions::from_str(database_url)
        .map(|options| {
            let filename = options
                .get_filename()
                .to_string_lossy()
                .to_ascii_lowercase();
            filename == ":memory:"
                || filename == "file::memory:"
                || filename.contains("sqlx-in-memory-")
        })
        .unwrap_or(false)
}

fn sqlite_url_query_requests_memory(database_url: &str) -> bool {
    let Some((_, query)) = database_url.split_once('?') else {
        return false;
    };
    query.split('&').any(|param| {
        param
            .split_once('=')
            .is_some_and(|(key, value)| key == "mode" && value == "memory")
    })
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
