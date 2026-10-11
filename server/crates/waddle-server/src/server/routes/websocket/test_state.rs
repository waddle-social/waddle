//! Test-only WebSocket state construction shared by in-crate and public XEP suites.
//!
//! These helpers intentionally use `expect`: they construct fixed, local test
//! fixtures and cannot report a useful runtime error to their callers.

use super::*;
use super::{ProtocolServices, WebSocketDeps, WebSocketState, XmppServiceDomains};
use crate::{
    auth::Session,
    config::ServerConfig,
    db::{DatabaseConfig, DatabasePool, MigrationRunner, PoolConfig},
    server::AppState,
};
use kameo::actor::Spawn;
use std::sync::Arc;
use tokio::sync::mpsc;
use waddle_extensions::{ExtensionConfig, ExtensionManager};
use waddle_xmpp::{
    commands::CommandRegistry, mam::MamStorage, muc::room_registry_actor::RoomRegistryActor,
    protocol::StanzaDispatcher, registry::ConnectionRegistry,
    stream_management::InMemorySmSessionRegistry,
};

/// Optional fixture substitutions for one test websocket state; unset fields
/// take the shared defaults.
#[derive(Default)]
pub(crate) struct TestStateOverrides {
    pub(crate) db_pool: Option<Arc<DatabasePool>>,
    pub(crate) call_sfu: Option<Arc<dyn waddle_sfu::SfuService>>,
    pub(crate) clustering: Option<crate::clustering::ClusteringHandles>,
    pub(crate) sm_session_registry: Option<Arc<InMemorySmSessionRegistry>>,
    pub(crate) blocking_storage: Option<Arc<dyn waddle_xmpp::xep::xep0191::BlockingStorage>>,
    pub(crate) pending_delivery_storage:
        Option<Arc<dyn waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage>>,
    pub(crate) ingress: Option<Arc<crate::ingress::IngressAuthority>>,
    pub(crate) notification_settings_projection:
        Option<Arc<crate::notification_settings_projection::NotificationSettingsProjectionStore>>,
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

pub(crate) async fn create_test_websocket_state_with_extension_manager(
    extension_manager: Arc<ExtensionManager>,
    overrides: TestStateOverrides,
) -> Arc<WebSocketState> {
    let TestStateOverrides {
        db_pool: db_pool_override,
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
            DatabasePool::new(DatabaseConfig::default(), PoolConfig)
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
        crate::inbox::DatabaseInboxStorage::from_database(db_pool.global().clone())
            .await
            .expect("shared inbox schema"),
    );

    let runner = MigrationRunner::single();
    runner.run(db_pool.global()).await.expect("migrations");
    crate::muc_destroy_completion_outbox::MucDestroyCompletionOutboxStore::new(
        db_pool.global().clone(),
    )
    .await
    .expect("MUC destroy completion outbox");

    let server_config = ServerConfig::test_homeserver();
    let public_websocket_url = url::Url::parse("wss://example.com/ws").expect("test WebSocket URL");
    let mut app_state_built = AppState::new(db_pool);
    if let Some(clustering) = clustering_override {
        app_state_built.clustering_claims = clustering;
    }
    let app_state = Arc::new(app_state_built);
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
    let pubsub_storage = Arc::new(
        crate::pubsub::DatabasePubSubStorage::open(Some("sqlite::memory:"))
            .await
            .expect("pubsub storage"),
    );
    let notification_settings_projection = notification_settings_projection_override
        .unwrap_or_else(|| {
            Arc::new(
                crate::notification_settings_projection::NotificationSettingsProjectionStore::new(
                    pubsub_storage.database(),
                ),
            )
        });
    let dnd_projection = Arc::new(crate::dnd_projection::DndProjectionStore::new(
        pubsub_storage.database(),
    ));
    let dnd_reader = Arc::new(crate::dnd_reader::PepDndReader::with_system_clock(
        Arc::clone(&dnd_projection),
    ));
    let notification_activity = Arc::new(
        crate::notification_activity::NotificationActivityStore::new(
            app_state.db_pool.global().clone(),
        )
        .await
        .expect("notification activity store"),
    );
    let push_service = Arc::new(
        crate::push_service::DatabasePushServiceStore::new_with_secret_key_and_pubsub(
            app_state.db_pool.global().clone(),
            test_push_service_secret(),
            "push.example.com".parse().expect("push service jid"),
            pubsub_storage.clone(),
        )
        .await
        .expect("push service"),
    );
    let notification_outbox = Arc::new(
        crate::notification_outbox::NotificationOutboxStore::new(
            app_state.db_pool.global().clone(),
        )
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
        crate::call_teardown_outbox::CallTeardownOutboxStore::new_with_node_identity(
            app_state.db_pool.global().clone(),
            call_teardown_node_identity,
        )
        .await
        .expect("call teardown outbox"),
    );
    let call_teardown_persistence =
        crate::call_teardown_outbox::CallTeardownPersistenceSupervisor::new(
            Arc::clone(&call_teardown_outbox),
            tokio::runtime::Handle::current(),
        );
    let room_effect_outbox = Arc::new(
        crate::room_effect_outbox::RoomEffectOutboxStore::new(app_state.db_pool.global().clone())
            .await
            .expect("room effect outbox"),
    );
    let room_effect_arm_supervisor = crate::room_effect_outbox::RoomEffectArmSupervisor::new(
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
                        crate::threads::storage::InboxBackedThreadsStorage::new(Arc::clone(
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
                        crate::server::extension_bot_avatar::BotAvatars::loopback(),
                    ),
                    dispatcher: Arc::new(dispatcher),
                    muji_pre_dispatch_terminate_rate_limit: Arc::new(
                        waddle_xmpp::protocol::handlers::session_initiate_rate_limit::TerminateRateLimit::with_defaults(),
                    ),
                    muji_pre_dispatch_action_rate_limit: Arc::new(
                        waddle_xmpp::protocol::handlers::session_initiate_rate_limit::MujiActionRateLimit::with_defaults(),
                    ),
                    pubsub_storage,
                    push_store: Arc::new(
                        crate::push_registrations::DatabasePushRegistrationStore::new(
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
                    ingress: match ingress_override {
                        Some(ingress) => ingress,
                        None => Arc::new(crate::ingress::IngressAuthority::for_test(app_state.db_pool.global().clone()).await),
                    },
                    link_preview_resolves:
                        crate::server::routes::websocket::default_link_preview_resolve_permits(),
                    caps_resolver: Arc::new(
                        crate::server::caps_resolution::CapsResolver::default(),
                    ),
                    avatar_source_locks: Arc::new(crate::profile::AvatarLockMap::new()),
                    profile_publish_tracker: tokio_util::task::TaskTracker::new(),
                    pep_feed_bridge: Arc::new(crate::pep_feed_bridge::PepFeedBridge::new()),
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
                    dm_pin_store: Arc::new(crate::server::routes::websocket::DmPinStore::default()),
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
                    crate::server::routes::extension_webhooks::ProviderIngressRegistry::default(),
                ),
                provider_dispatch_tasks:
                    crate::server::routes::extension_webhooks::ProviderDispatchTracker::new(),
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
        crate::db::DatabaseDriver::Sqlite => {
            "INSERT OR IGNORE INTO users \
             (jid, username, xmpp_localpart, localpart_key, display_name, avatar_url, primary_email, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"
        }
        crate::db::DatabaseDriver::Postgres => {
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
) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
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
        .get_entry(jid)
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
