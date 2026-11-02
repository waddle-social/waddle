//! Test-only WebSocket state construction shared by in-crate and public XEP suites.
//!
//! These helpers intentionally use `expect`: they construct fixed, local test
//! fixtures and cannot report a useful runtime error to their callers.

use super::AuthState;
use super::{ProtocolServices, WebSocketDeps, WebSocketState, XmppServiceDomains};
use crate::auth::Session;
use crate::call_teardown_outbox::CallTeardownOutboxStore;
use crate::call_teardown_outbox::CallTeardownPersistenceSupervisor;
use crate::clustering::ClusteringHandles;
use crate::config::ServerConfig;
use crate::db::DatabaseDriver;
use crate::db::{DatabaseConfig, DatabasePool, MigrationRunner, PoolConfig};
use crate::dnd_projection::DndProjectionStore;
use crate::dnd_reader::PepDndReader;
use crate::inbox::DatabaseInboxStorage;
use crate::ingress::IngressAuthority;
use crate::muc_destroy_completion_outbox::MucDestroyCompletionOutboxStore;
use crate::notification_activity::NotificationActivityStore;
use crate::notification_outbox::NotificationOutboxStore;
use crate::notification_settings_projection::NotificationSettingsProjectionStore;
use crate::pep_feed_bridge::PepFeedBridge;
use crate::profile::AvatarLockMap;
use crate::pubsub::DatabasePubSubStorage;
use crate::push_registrations::DatabasePushRegistrationStore;
use crate::push_service::DatabasePushServiceStore;
use crate::room_effect_outbox::RoomEffectArmSupervisor;
use crate::room_effect_outbox::RoomEffectOutboxStore;
use crate::server::caps_resolution::CapsResolver;
use crate::server::extension_bot_avatar::BotAvatars;
use crate::server::routes::extension_webhooks::ProviderDispatchTracker;
use crate::server::routes::extension_webhooks::ProviderIngressRegistry;
use crate::server::routes::websocket::DmPinStore;
use crate::server::AppState;
use crate::threads::storage::InboxBackedThreadsStorage;
use kameo::actor::Spawn;
use std::sync::Arc;
use tokio::sync::mpsc;
use waddle_extensions::{ExtensionConfig, ExtensionManager};
use waddle_xmpp::commands::CommandRegistry;
use waddle_xmpp::mam::MamStorage;
use waddle_xmpp::muc::room_registry_actor::RoomRegistryActor;
use waddle_xmpp::protocol::StanzaDispatcher;
use waddle_xmpp::registry::ConnectionRegistry;
use waddle_xmpp::stream_management::InMemorySmSessionRegistry;

/// Optional fixture substitutions for one test websocket state; unset fields
/// take the shared defaults.
#[derive(Default)]
pub(crate) struct TestStateOverrides {
    pub(crate) db_pool: Option<Arc<DatabasePool>>,
    /// Explicit production-backed dependencies for native characterization.
    pub(crate) app_state: Option<Arc<AppState>>,
    pub(crate) database_pubsub_storage: Option<Arc<DatabasePubSubStorage>>,
    pub(crate) call_sfu: Option<Arc<dyn waddle_sfu::SfuService>>,
    pub(crate) clustering: Option<ClusteringHandles>,
    pub(crate) sm_session_registry: Option<Arc<InMemorySmSessionRegistry>>,
    pub(crate) blocking_storage: Option<Arc<dyn waddle_xmpp::xep::xep0191::BlockingStorage>>,
    pub(crate) pending_delivery_storage:
        Option<Arc<dyn waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage>>,
    pub(crate) ingress: Option<Arc<IngressAuthority>>,
    pub(crate) notification_settings_projection: Option<Arc<NotificationSettingsProjectionStore>>,
    /// Wire `deps.protocol.room_registry` to the app-state registry, as
    /// production does, for tests that drive admin commands and janitors
    /// against the same rooms.
    pub(crate) share_app_room_registry: bool,
}

/// Push-service signing secret shared by every test state in this process, so
/// states built over one database agree on it without a fixed key in source.
fn test_push_service_secret() -> &'static [u8] {
    static SECRET: std::sync::OnceLock<[u8; 32]> = std::sync::OnceLock::new();
    SECRET.get_or_init(rand::random)
}

pub(crate) async fn empty_extension_manager() -> Arc<ExtensionManager> {
    Arc::new(
        ExtensionManager::from_config(ExtensionConfig {
            enabled: false,
            cache_dir: std::env::temp_dir()
                .join("waddle-extension-test-cache")
                .display()
                .to_string(),
            modules: Vec::new(),
        })
        .await
        .expect("empty extension manager"),
    )
}

#[cfg(test)]
pub(crate) async fn create_test_websocket_state_with_extension_manager(
    extension_manager: Arc<ExtensionManager>,
    overrides: TestStateOverrides,
) -> Arc<WebSocketState> {
    create_test_websocket_state_with_ingress_factory(extension_manager, overrides, || {
        std::future::ready(None)
    })
    .await
}

pub(crate) async fn create_test_websocket_state_with_ingress_factory<F, Fut>(
    extension_manager: Arc<ExtensionManager>,
    overrides: TestStateOverrides,
    start_ingress: F,
) -> Arc<WebSocketState>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Option<Arc<IngressAuthority>>>,
{
    let TestStateOverrides {
        db_pool: db_pool_override,
        app_state: app_state_override,
        database_pubsub_storage: database_pubsub_storage_override,
        call_sfu,
        clustering: clustering_override,
        sm_session_registry: sm_session_registry_override,
        blocking_storage: blocking_storage_override,
        pending_delivery_storage: pending_delivery_storage_override,
        ingress: ingress_override,
        notification_settings_projection: notification_settings_projection_override,
        share_app_room_registry,
    } = overrides;
    let db_pool = match db_pool_override {
        Some(db_pool) => db_pool,
        None => Arc::new(
            DatabasePool::try_new(DatabaseConfig::default(), PoolConfig)
                .await
                .expect("db pool"),
        ),
    };

    let global = db_pool.global();
    let mam_storage: Arc<dyn MamStorage> = Arc::new(match global.sqlite_pool() {
        Some(pool) => waddle_xmpp::mam::SqlxMamStorage::from_sqlite_pool(pool.clone())
            .await
            .expect("shared MAM schema"),
        None => waddle_xmpp::mam::SqlxMamStorage::open(global.database_url())
            .await
            .expect("shared MAM schema"),
    });
    let test_inbox_storage: Arc<dyn waddle_xmpp::inbox::storage::InboxStorage> = Arc::new(
        DatabaseInboxStorage::from_database(db_pool.global().clone())
            .await
            .expect("shared inbox schema"),
    );

    let runner = MigrationRunner::single();
    runner.run(db_pool.global()).await.expect("migrations");
    MucDestroyCompletionOutboxStore::try_new(db_pool.global().clone())
        .await
        .expect("MUC destroy completion outbox");

    let server_config = ServerConfig::test_homeserver();
    let public_websocket_url = url::Url::parse("wss://example.com/ws").expect("test WebSocket URL");
    let shared_pubsub = app_state_override
        .as_ref()
        .map(|state| Arc::clone(&state.pubsub_storage));
    let app_state = match app_state_override {
        Some(state) => {
            assert!(
                Arc::ptr_eq(&state.db_pool, &db_pool),
                "explicit state must share the configured database pool"
            );
            state
        }
        None => {
            let mut state = AppState::new(db_pool);
            if let Some(clustering) = clustering_override {
                state.clustering_claims = clustering;
            }
            Arc::new(state)
        }
    };
    let mut auth_state_inner = AuthState::new(
        app_state.clone(),
        &server_config,
        &public_websocket_url,
        Some(b"test-encryption-key-32-bytes!!!"),
    );
    // The dispatcher path's bare-JID branch
    // (`OutboundEvent::RouteToConnection`) drops cross-domain
    // bare JIDs without running the headless recipient pass —
    // the production env var defaults `xmpp_domain` to
    // `"localhost"`, but every fixture in this test module uses
    // `@example.com` JIDs. Pin the local domain to match so the
    // headless recipient pass actually fires for offline-bare
    // JID delivery (#229 PR15) under unit-test fixtures.
    auth_state_inner.xmpp_domain = "example.com".to_string();
    let auth_state = Arc::new(auth_state_inner);

    let mut dispatcher = StanzaDispatcher::new();
    waddle_xmpp::protocol::handlers::register_default_handlers(&mut dispatcher);
    waddle_xmpp::protocol::handlers::register_default_message_handlers(&mut dispatcher);
    if let Some(sfu) = call_sfu.as_ref() {
        // Mirror `http.rs`: when an SFU is configured the XEP-0166
        // Jingle + XEP-0215 extdisco handlers are registered on the
        // dispatcher. Without this, `has_iq_handler(NS_JINGLE)` is
        // false and a call IQ never reaches the forward path.
        waddle_xmpp::protocol::handlers::register_call_handlers(
            &mut dispatcher,
            Arc::clone(sfu),
            443,
            3478,
        );
    }
    let pubsub_storage = match database_pubsub_storage_override {
        Some(storage) => storage,
        None => Arc::new(
            DatabasePubSubStorage::open(Some("sqlite::memory:"))
                .await
                .expect("pubsub storage"),
        ),
    };
    let notification_settings_projection = notification_settings_projection_override
        .unwrap_or_else(|| {
            Arc::new(NotificationSettingsProjectionStore::new(
                pubsub_storage.database(),
            ))
        });
    let dnd_projection = Arc::new(DndProjectionStore::new(pubsub_storage.database()));
    let dnd_reader = Arc::new(PepDndReader::with_system_clock(Arc::clone(&dnd_projection)));
    let notification_activity = Arc::new(
        NotificationActivityStore::try_new(app_state.db_pool.global().clone())
            .await
            .expect("notification activity store"),
    );
    let push_service = Arc::new(
        DatabasePushServiceStore::new_with_secret_key_and_pubsub(
            app_state.db_pool.global().clone(),
            test_push_service_secret(),
            "push.example.com".parse().expect("push service jid"),
            pubsub_storage.clone(),
        )
        .await
        .expect("push service"),
    );
    let notification_outbox = Arc::new(
        NotificationOutboxStore::try_new(app_state.db_pool.global().clone())
            .await
            .expect("notification outbox"),
    );
    push_service
        .adopt_notification_ancestry()
        .await
        .expect("canonical notification ancestry");
    let call_teardown_node_identity = app_state
        .clustering_claims
        .node_identity
        .clone()
        .unwrap_or_else(|| {
            waddle_xmpp::ownership::SharedNodeIdentity::new(
                waddle_xmpp::ownership::NodeIdentity::local(),
            )
        });
    let call_teardown_outbox = Arc::new(
        CallTeardownOutboxStore::new_with_node_identity(
            app_state.db_pool.global().clone(),
            call_teardown_node_identity,
        )
        .await
        .expect("call teardown outbox"),
    );
    let call_teardown_persistence = CallTeardownPersistenceSupervisor::new(
        Arc::clone(&call_teardown_outbox),
        tokio::runtime::Handle::current(),
    );
    let room_effect_outbox = Arc::new(
        RoomEffectOutboxStore::try_new(app_state.db_pool.global().clone())
            .await
            .expect("room effect outbox"),
    );
    let room_effect_arm_supervisor = RoomEffectArmSupervisor::new(
        Arc::clone(&room_effect_outbox),
        tokio::runtime::Handle::current(),
    );

    let service_domains = XmppServiceDomains {
        muc: "muc.example.com".to_string(),
        spaces: "spaces.example.com".to_string(),
        upload: "upload.example.com".to_string(),
        extensions: "extensions.example.com".to_string(),
        push: "push.example.com".to_string(),
        community: "community.example.com".to_string(),
    };
    // Mirror `http.rs`: the bot hat is server-assigned for every bot JID.
    crate::server::extension_bot::install_bot_hats(
        &app_state.server_hats,
        service_domains.clone(),
        Arc::clone(&extension_manager),
    );

    Arc::new(WebSocketState {
            deps: WebSocketDeps {
                app_state: Arc::clone(&app_state),
                auth_state,
                service_domains,
                protocol: ProtocolServices {
                    connection_registry: Arc::new(ConnectionRegistry::new()),
                    user_registry: waddle_xmpp::registry::UserRegistryActor::spawn(
                        waddle_xmpp::registry::UserRegistryActor::new(),
                    ),
                    room_registry: if share_app_room_registry {
                        app_state.room_registry.clone()
                    } else {
                        RoomRegistryActor::spawn(
                            RoomRegistryActor::new(
                                "muc.example.com".to_string(),
                                crate::config::test_occupant_id_secret(),
                            )
                            .with_server_hats(app_state.server_hats.clone()),
                        )
                    },
                    mam_storage,
                    inbox_storage: Arc::clone(&test_inbox_storage),
                    threads_storage: Arc::new(
                        InboxBackedThreadsStorage::new(Arc::clone(
                            &test_inbox_storage,
                        )),
                    ),
                    blocking_storage: blocking_storage_override.unwrap_or_else(|| {
                        Arc::new(waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new())
                    }),
                    pending_delivery_storage: pending_delivery_storage_override.unwrap_or_else(|| {
                        Arc::new(
                            waddle_xmpp::pending_delivery::storage::InMemoryPendingDeliveryStorage::with_default_quota(),
                        )
                    }),
                    command_registry: {
                        // The XEP-0050 push commands (`register-device`,
                        // `disable-device`) are registered here so the
                        // unit-test harness mirrors what `http.rs` wires
                        // up at boot. Without this, push command IQs would
                        // fall through to the registry's unknown-node
                        // `item-not-found` arm (XEP-0050 §4.4) and shadow
                        // the actual handler behaviour we want to assert.
                        let registry = Arc::new(CommandRegistry::new());
                        crate::push_service::commands::register(
                            &registry,
                            Arc::clone(&push_service),
                        )
                        .await;
                        registry
                    },
                    extension_manager,
                    bot_avatars: Arc::new(
                        BotAvatars::loopback(),
                    ),
                    dispatcher: Arc::new(dispatcher),
                    muji_pre_dispatch_terminate_rate_limit: Arc::new(
                        waddle_xmpp::protocol::handlers::session_initiate_rate_limit::TerminateRateLimit::with_defaults(),
                    ),
                    muji_pre_dispatch_action_rate_limit: Arc::new(
                        waddle_xmpp::protocol::handlers::session_initiate_rate_limit::MujiActionRateLimit::with_defaults(),
                    ),
                    pubsub_storage: shared_pubsub.unwrap_or(pubsub_storage),
                    push_store: Arc::new(
                        DatabasePushRegistrationStore::try_new(
                            app_state.db_pool.global().clone(),
                        )
                        .await
                        .expect("push registration store"),
                    ),
                    push_service,
                    notification_outbox,
                    call_teardown_outbox,
                    call_teardown_persistence,
                    room_effect_outbox,
                    room_effect_arm_supervisor,
                    call_teardown_executor: None,
                    notification_settings_projection,
                    dnd_projection,
                    dnd_reader,
                    notification_activity,
                    sm_session_registry: sm_session_registry_override.unwrap_or_else(|| {
                        Arc::new(InMemorySmSessionRegistry::new().with_persistence(Arc::new(
                            waddle_xmpp::stream_management::persistence::InMemorySmPersistence::new(),
                        )))
                    }),
                    // Finish every schema constructor, including push registration,
                    // before invoking the deferred authority factory.
                    ingress: match ingress_override {
                        Some(ingress) => ingress,
                        None => match start_ingress().await {
                            Some(ingress) => ingress,
                            None => Arc::new(IngressAuthority::for_test(app_state.db_pool.global().clone()).await),
                        },
                    },
                    link_preview_resolves:
                        crate::server::routes::websocket::default_link_preview_resolve_permits(),
                    caps_resolver: Arc::new(
                        CapsResolver::default(),
                    ),
                    avatar_source_locks: Arc::new(AvatarLockMap::new()),
                    profile_publish_tracker: tokio_util::task::TaskTracker::new(),
                    pep_feed_bridge: Arc::new(PepFeedBridge::new()),
                    call_threads: Arc::new(dashmap::DashMap::new()),
                    call_thread_end_locks: Arc::new(dashmap::DashMap::new()),
                    extension_bot_rooms: Default::default(),
                remote_muc_memberships: Arc::new(super::RemoteMucMemberships::default()),
                    pending_local_muc_departures: Arc::new(
                        super::PendingLocalMucDepartures::default(),
                    ),
                    resolver_affiliation_syncs: Arc::new(
                        super::ResolverAffiliationSyncScheduler::default(),
                    ),
                    dm_call_threads: Arc::new(dashmap::DashMap::new()),
                    dm_pin_store: Arc::new(DmPinStore::default()),
                    dm_call_thread_projections: Arc::new(dashmap::DashSet::new()),
                    pending_dm_call_offers: Arc::new(dashmap::DashMap::new()),
                    sfu: call_sfu,
                },
                occupant_id_secret: crate::config::test_occupant_id_secret(),
                link_preview: server_config.link_preview.clone(),
                ws_keepalive: server_config.ws_keepalive,
                shutdown: waddle_ecdysis::GracefulShutdown::new(std::time::Duration::from_secs(1))
                    .handle(),
                provider_ingress: Arc::new(
                    ProviderIngressRegistry::default(),
                ),
                provider_dispatch_tasks:
                    ProviderDispatchTracker::new(),
            },
        })
}

/// Seed an OIDC-provisioned local account directly into the `users`
/// table, the way the OIDC login flow does. Needed since #1246: a
/// message routed to a local bare JID with no registered account is
/// bounced with `<service-unavailable/>` (RFC 6121 §8.5.1) instead of
/// being persisted, so tests that message an offline recipient must
/// give that recipient an account first.
pub async fn seed_local_account(state: &WebSocketState, localpart: &str) {
    use crate::db::actor::DbExecute;
    let sql = match state.deps.app_state.db_pool.global().driver() {
        DatabaseDriver::Sqlite => {
            "INSERT OR IGNORE INTO users \
             (jid, username, xmpp_localpart, localpart_key, display_name, avatar_url, primary_email, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"
        }
        DatabaseDriver::Postgres => {
            "INSERT INTO users \
             (jid, username, xmpp_localpart, localpart_key, display_name, avatar_url, primary_email, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT DO NOTHING"
        }
    };
    state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .ask(DbExecute {
            sql: sql.to_string(),
            params: vec![
                format!("{localpart}@example.com").into(),
                localpart.into(),
                localpart.into(),
                localpart.into(),
                "Test User".into(),
                crate::db::Value::NullText,
                crate::db::Value::NullText,
                "2026-01-01T00:00:00Z".into(),
                "2026-01-01T00:00:00Z".into(),
            ],
        })
        .await
        .expect("seed local account");
}

pub async fn create_test_session(state: &WebSocketState, username: &str) -> Session {
    seed_local_account(state, username).await;
    let session = Session::new(
        &format!("{username}@{}", state.deps.auth_state.xmpp_domain),
        username,
        username,
    );
    state
        .deps
        .auth_state
        .session_manager
        .create_session(&session)
        .await
        .expect("session");
    session
}

pub async fn register_test_connection(
    state: &WebSocketState,
    jid: &jid::FullJid,
    sender: mpsc::Sender<waddle_xmpp::registry::OutboundStanza>,
) -> waddle_xmpp::registry::LocalConnectionBindingId {
    let owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), sender);
    // `register` always inserts, so the entry must be present — fail fast
    // rather than silently skipping the actor mirror, which would leave the
    // actor tree empty and mask a regression as the offline/headless path
    // (Copilot review on PR #1177).
    let entry = state
        .deps
        .protocol
        .connection_registry
        .entry(jid)
        .expect("connection entry must exist immediately after register");
    let registered = crate::server::dual_registration::mirror_register(
        &state.deps.protocol.user_registry,
        jid.clone(),
        entry,
    )
    .await;
    assert!(
        registered,
        "test dual-registration should confirm the resource in the actor tree for {jid}"
    );
    owner
}

/// Drive the active frame entry with an explicitly authenticated and bound test
/// session. This is dispatch evidence, not a SASL exchange or socket test.
#[cfg(all(test, target_os = "linux"))]
pub(crate) async fn dispatch_native_bound_iq(
    state: &WebSocketState,
    session: &Session,
    bound: &jid::FullJid,
    iq: xmpp_parsers::iq::Iq,
) -> Vec<String> {
    let (sender, _receiver) = mpsc::channel(8);
    let mut conn = super::state::WsConnState::new();
    conn.phase = waddle_xmpp::protocol::ConnectionPhase::ready(bound.clone(), false);
    conn.authenticated_session = Some(session.clone());
    conn.registry_owner = Some(register_test_connection(state, bound, sender).await);
    let frame = super::element_to_xml(minidom::Element::from(iq));
    let result = super::frame::handle_xmpp_frame(&frame, "example.com", state, &mut conn).await;
    assert!(!conn.phase.is_closing(), "bound frame must remain open");
    assert!(!conn.sm_inbound_completion.has_unhandled_hole());
    result
}
