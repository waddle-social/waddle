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
    spawn_pending_delivery_claim_janitor, spawn_push_service_publish_job_janitor,
    spawn_room_dormancy_janitor, spawn_sm_expiry_janitor,
};
use crate::server::topology::bootstrap_fresh_xmpp_topology;
use crate::server::trace::make_request_span;
use crate::server::{AppState, XmppConfig};
use anyhow::Result;
use axum::{routing::get, Router};
use rustls_acme::tower::TowerHttp01ChallengeService;
use sqlx::sqlite::SqliteConnectOptions;
use std::str::FromStr;
use std::sync::Arc;
use tower_http::{
    compression::CompressionLayer,
    trace::{DefaultOnResponse, TraceLayer},
};
use tracing::{info, warn, Level};
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
    pub(crate) stop_token: tokio_util::sync::CancellationToken,
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
        stop_token,
        drain_complete,
    } = deps;

    let app = create_router(RouterDeps {
        state,
        server_config,
        xmpp_config,
        mam_storage,
        pubsub_database_storage,
        acme_http01_challenge_service,
        shutdown_stop_token: stop_token.clone(),
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

pub(crate) async fn create_websocket_mam_storage(
    database_url: Option<String>,
) -> Result<Arc<dyn MamStorage>> {
    let storage = match database_url.as_deref() {
        Some(database_url) => SqlxMamStorage::open(database_url).await,
        None => SqlxMamStorage::open_in_memory().await,
    }
    .map_err(|error| anyhow::anyhow!("Failed to initialize WebSocket MAM storage: {error}"))?;

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
    pub(crate) shutdown_stop_token: tokio_util::sync::CancellationToken,
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
        shutdown_stop_token,
        drain_complete,
    } = deps;
    // Create auth broker state
    let auth_state = Arc::new(AuthState::new(
        state.clone(),
        &server_config,
        Some(server_config.session_key.as_bytes()),
    ));

    let websocket_state = create_websocket_state(
        Arc::clone(&state),
        &server_config,
        &xmpp_config,
        Arc::clone(&auth_state),
        mam_storage,
        pubsub_database_storage,
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
    spawn_pending_delivery_claim_janitor(&websocket_state);
    spawn_notification_outbox_janitor(&websocket_state);
    spawn_push_service_publish_job_janitor(&websocket_state);
    spawn_auth_state_janitor(&websocket_state);
    spawn_room_dormancy_janitor(&websocket_state);
    crate::server::state_inventory_metrics::spawn_state_inventory_publisher(&websocket_state);
    spawn_graceful_shutdown_drain(
        Arc::clone(&websocket_state),
        shutdown_stop_token,
        Arc::clone(&drain_complete),
    );

    let extension_webhooks_router =
        routes::extension_webhooks::router(Arc::clone(&websocket_state));
    let websocket_router = routes::websocket::router(websocket_state.clone());

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
        .merge(extension_webhooks_router);

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
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(make_request_span)
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        )
        .layer(CompressionLayer::new())
        .layer(configure_cors());
    Ok(router)
}

async fn create_websocket_state(
    state: Arc<AppState>,
    server_config: &ServerConfig,
    xmpp_config: &XmppConfig,
    auth_state: Arc<AuthState>,
    mam_storage: Arc<dyn MamStorage>,
    pubsub_database_storage: Arc<crate::pubsub::DatabasePubSubStorage>,
) -> Result<Arc<WebSocketState>> {
    // Create connection registry for WebSocket message routing
    let connection_registry = Arc::new(ConnectionRegistry::new());

    // Read the MUC room registry and PubSub storage off the shared
    // `AppState` — both are built in `start_with_config` so admin V2
    // handlers and the WebSocket transport operate on the same handles.
    let xmpp_domain = auth_state.xmpp_domain.clone();
    let service_domains = XmppServiceDomains::new(&xmpp_domain);
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
    match waddle_sfu::SfuConfig::from_env() {
        Ok(Some(sfu_config)) => {
            let turn_tls_port = sfu_config.turn_tls_port;
            let turn_udp_port = sfu_config.turn_udp_port;
            let sfu: std::sync::Arc<dyn waddle_sfu::SfuService> =
                std::sync::Arc::new(waddle_sfu::LiveKitSfu::new(sfu_config));
            waddle_xmpp::protocol::handlers::register_call_handlers(
                &mut stanza_dispatcher,
                Arc::clone(&sfu),
                turn_tls_port,
                turn_udp_port,
            );
            sfu_service = Some(sfu);
            tracing::info!(
                "LiveKit SFU configured; XEP-0166 Jingle + XEP-0215 extdisco handlers registered"
            );
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

    let sm_session_registry = create_sm_session_registry(xmpp_config).await;
    let resumable_sessions: Arc<dashmap::DashMap<String, crate::auth::Session>> =
        Arc::new(dashmap::DashMap::new());
    let blocking_storage: Arc<dyn waddle_xmpp::xep::xep0191::BlockingStorage> = Arc::new(
        crate::db::blocking::DatabaseBlockingStorage::new(state.db_pool.global().clone()),
    );
    let pending_delivery_storage = create_pending_delivery_storage(xmpp_config).await;

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
            service_domains,
            protocol: ProtocolServices {
                connection_registry,
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
                isr_token_store: waddle_xmpp::isr::create_shared_store(),
                sm_session_registry,
                resumable_sessions,
                caps_resolver,
                avatar_source_locks: Arc::new(crate::profile::AvatarLockMap::new()),
                profile_publish_tracker: tokio_util::task::TaskTracker::new(),
                pep_feed_bridge: Arc::new(crate::pep_feed_bridge::PepFeedBridge::new()),
                sfu: sfu_service,
            },
            occupant_id_secret: server_config.occupant_id_secret.clone(),
            provider_ingress,
            provider_dispatch_tasks,
        },
    });
    deferred_extension_host_tools.set(Arc::new(extension_host_adapter::ExtensionHostAdapter::new(
        Arc::clone(&websocket_state),
    )));
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

async fn create_sm_session_registry(
    xmpp_config: &XmppConfig,
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
    let sm_persistence: Arc<dyn waddle_xmpp::stream_management::persistence::SmPersistenceStorage> =
        Arc::new(
            crate::sm_persistence::DatabaseSmPersistence::open(sm_database_url.as_deref())
                .await
                .expect("open SM persistence storage"),
        );
    let sm_session_registry = waddle_xmpp::stream_management::InMemorySmSessionRegistry::new()
        .with_persistence(Arc::clone(&sm_persistence));
    if let Err(error) = sm_session_registry.restore_from_persistence().await {
        warn!(
            error = %error,
            "restore_from_persistence failed at startup; continuing with empty \
             in-memory SM session view. XEP-0198 resume will return <failed/> \
             until storage health is restored."
        );
    }
    Arc::new(sm_session_registry)
}

async fn create_pending_delivery_storage(
    xmpp_config: &XmppConfig,
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
    Arc::new(
        crate::pending_delivery::DatabasePendingDeliveryStorage::open(
            pending_delivery_url.as_deref(),
            waddle_xmpp::pending_delivery::QuotaPolicy::default_policy(),
        )
        .await
        .expect("open pending_delivery storage"),
    )
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
