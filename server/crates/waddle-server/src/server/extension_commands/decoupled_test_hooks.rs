use std::str::FromStr;
use std::sync::Arc;

use kameo::actor::Spawn;
use waddle_ecdysis::GracefulShutdown;
use waddle_extensions::{ExtensionConfig, ExtensionManager};
use waddle_xmpp::commands::CommandRegistry;
use waddle_xmpp::mam::MamStorage;
use waddle_xmpp::muc::room_registry_actor::RoomRegistryActor;
use waddle_xmpp::ownership::{ConnectionRef, NodeLeaseIdentity};
use waddle_xmpp::protocol::handlers::session_initiate_rate_limit::{
    MujiActionRateLimit, TerminateRateLimit,
};
use waddle_xmpp::protocol::{ConnectionPhase, KeepaliveConfig, StanzaDispatcher};
use waddle_xmpp::pubsub::PubSubStorage;
use waddle_xmpp::registry::{
    ConnectionRegistry, OutboundStanza, RegisterUserResource, SocketOutboundEndpoint,
    UserRegistryActor,
};
use waddle_xmpp::stream_management::persistence::InMemorySmPersistence;
use waddle_xmpp::stream_management::InMemorySmSessionRegistry;
use waddle_xmpp::xep::xep0317::ServerHats;
use waddle_xmpp::xep::xep0421::OccupantIdSecret;

use crate::account_coordinator::AccountAuthoritySelection;
use crate::auth::redirect::TrustedWebOrigins;
use crate::auth::Session;
use crate::call_teardown_outbox::{CallTeardownOutboxStore, CallTeardownPersistenceSupervisor};
use crate::channel_space_links::{ChannelSpaceLinkStore, DatabaseChannelSpaceLinkStore};
use crate::clustering::{ClusteringHandles, NodeAdmissionPermit, NodeLifecycle};
use crate::config::{
    AuthConfig, AuthzBackendSelection, BrokerRuntimeConfig, ClusteringConfig, IngressConfig,
    LineageConfig, LinkPreviewConfig, MaxWebSocketConnections, MultiTenantConfig, ServerConfig,
    ServerMode,
};
use crate::db::{DatabaseConfig, DatabasePool, MigrationRunner, PoolConfig};
use crate::destination::FederationDisabled;
use crate::dnd_projection::DndProjectionStore;
use crate::dnd_reader::PepDndReader;
use crate::inbox::DatabaseInboxStorage;
use crate::ingress::IngressAuthority;
use crate::muc_destroy_completion_outbox::MucDestroyCompletionOutboxStore;
use crate::notification_activity::NotificationActivityStore;
use crate::notification_outbox::NotificationOutboxStore;
use crate::notification_settings_projection::NotificationSettingsProjectionStore;
use crate::pep_feed_bridge::PepFeedBridge;
use crate::permissions::host_grant::RestoreEpoch;
use crate::permissions::PermissionActor;
use crate::profile::AvatarLockMap;
use crate::pubsub::DatabasePubSubStorage;
use crate::push_registrations::DatabasePushRegistrationStore;
use crate::push_service::DatabasePushServiceStore;
use crate::room_effect_outbox::{RoomEffectArmSupervisor, RoomEffectOutboxStore};
use crate::server::caps_resolution::{CapsResolver, ServerDomainJid};
use crate::server::extension_bot_avatar::BotAvatars;
use crate::server::routes::auth::AuthState;
use crate::server::routes::extension_webhooks::{ProviderDispatchTracker, ProviderIngressRegistry};
use crate::server::routes::websocket::{
    default_link_preview_resolve_permits, handle_xmpp_frame_with_admission, DmPinStore,
    PendingLocalMucDepartures, ProtocolServices, RemoteMucMemberships,
    ResolverAffiliationSyncScheduler, RetiredAccounts, StreamingInferenceSession, WebSocketDeps,
    WebSocketState, WsConnState, XmppServiceDomains,
};
use crate::server::state::{AppState, AppStateDeps};
use crate::sm_authority::{DurableDetachedRecipients, DurableSmSelection, DurableSmStore};
use crate::spaces_metadata::{DatabaseSpacesMetadataStore, SpacesMetadataStore};
use crate::threads::storage::InboxBackedThreadsStorage;

/// Handle returned to integration tests driving the real
/// `handle_xmpp_frame_with_admission` -> `handle_command_iq` ->
/// `register_extension_commands` pipeline.
#[doc(hidden)]
pub struct DecoupledCommandDispatchHandle {
    pub immediate_frames: Vec<String>,
    pub outbound_rx: tokio::sync::mpsc::Receiver<OutboundStanza>,
    pub session: Option<StreamingInferenceSession>,
    pub inference_stream_tasks: tokio_util::task::TaskTracker,
    pub shutdown: GracefulShutdown,
    _websocket_state: Arc<WebSocketState>,
    _conn: WsConnState,
    _lifecycle: NodeLifecycle,
    _permit: NodeAdmissionPermit,
}

fn build_fixture_server_config() -> ServerConfig {
    let trusted_web_origins =
        TrustedWebOrigins::try_new("http://localhost:3000", None).expect("trusted origin");
    let occupant_id_secret =
        OccupantIdSecret::try_new(b"test-occupant-id-secret-32-bytes-long".to_vec())
            .expect("occupant secret");
    ServerConfig {
        mode: ServerMode::HomeServer,
        base_url: "http://localhost:3000".to_string(),
        session_key: "test-session-key-32-bytes-minimum".to_string(),
        auth: AuthConfig::default(),
        extensions: ExtensionConfig::default(),
        link_preview: LinkPreviewConfig::default(),
        ws_keepalive: KeepaliveConfig::default(),
        max_websocket_connections: MaxWebSocketConnections::default(),
        spicedb: None,
        openfga: None,
        authz_backend: AuthzBackendSelection::Auto,
        broker: BrokerRuntimeConfig::default(),
        turso: None,
        tenancy: MultiTenantConfig::default(),
        occupant_id_secret,
        ingress: IngressConfig::default(),
        clustering: ClusteringConfig::default(),
        apns: None,
        fcm: None,
        trusted_web_origins,
        host_grant_restore_epoch: RestoreEpoch::INITIAL,
    }
}

/// Constructs a real `WebSocketState` + `GracefulShutdown` with registered extension
/// commands for integration tests that run without `--features test-support`.
#[doc(hidden)]
pub async fn build_websocket_state_for_test(
    extension_manager: Arc<ExtensionManager>,
    domain: &str,
    drain_timeout_secs: u64,
) -> (Arc<WebSocketState>, GracefulShutdown) {
    let domain = domain.to_owned();
    let db_pool = Arc::new(
        DatabasePool::try_new(DatabaseConfig::default(), PoolConfig)
            .await
            .expect("db pool"),
    );
    MigrationRunner::single()
        .run(db_pool.global())
        .await
        .expect("migrations");

    let global = db_pool.global();
    let mam_storage: Arc<dyn MamStorage> = Arc::new(match global.sqlite_pool() {
        Some(pool) => waddle_xmpp::mam::SqlxMamStorage::from_sqlite_pool(pool.clone())
            .await
            .expect("shared MAM schema"),
        None => waddle_xmpp::mam::SqlxMamStorage::open(global.database_url())
            .await
            .expect("shared MAM schema"),
    });
    let inbox_storage: Arc<dyn waddle_xmpp::inbox::storage::InboxStorage> = Arc::new(
        DatabaseInboxStorage::from_database(global.clone())
            .await
            .expect("shared inbox schema"),
    );
    MucDestroyCompletionOutboxStore::try_new(global.clone())
        .await
        .expect("MUC destroy completion outbox");

    let server_config = build_fixture_server_config();
    let pubsub_storage = Arc::new(
        DatabasePubSubStorage::open(Some("sqlite::memory:"))
            .await
            .expect("pubsub storage"),
    );
    let pubsub_dyn: Arc<dyn PubSubStorage> = pubsub_storage.clone();
    let spaces_metadata_store: Arc<dyn SpacesMetadataStore> = Arc::new(
        DatabaseSpacesMetadataStore::open(Some("sqlite::memory:"))
            .await
            .expect("spaces metadata"),
    );
    let channel_space_link_store: Arc<dyn ChannelSpaceLinkStore> = Arc::new(
        DatabaseChannelSpaceLinkStore::open(Some("sqlite::memory:"))
            .await
            .expect("channel space links"),
    );
    let server_hats = ServerHats::default();
    let muc_domain_str = format!("muc.{domain}");
    let room_registry = RoomRegistryActor::spawn(
        RoomRegistryActor::new(
            muc_domain_str.clone(),
            server_config.occupant_id_secret.clone(),
        )
        .with_server_hats(server_hats.clone()),
    );
    let permission_actor = PermissionActor::spawn(PermissionActor::new_for_tests(Arc::new(
        db_pool.global().clone(),
    )));
    let app_state = Arc::new(AppState::new_with_deps(AppStateDeps {
        db_pool: Arc::clone(&db_pool),
        blob_storage: crate::storage::build_blob_storage().expect("blob storage"),
        inbox_storage: Arc::clone(&inbox_storage),
        spaces_metadata_store,
        channel_space_link_store,
        pubsub_storage: Arc::clone(&pubsub_dyn),
        room_registry: room_registry.clone(),
        spaces_jid: format!("spaces.{domain}").parse().expect("spaces jid"),
        muc_domain: jid::DomainPart::from_str(&muc_domain_str).expect("muc domain"),
        occupant_id_secret: server_config.occupant_id_secret.clone(),
        server_hats,
        permission_actor,
        server_owner_jids: Arc::from(Vec::<jid::BareJid>::new()),
        node_lifecycle: NodeLifecycle::new(),
        max_websocket_connections: MaxWebSocketConnections::default(),
        clustering_claims: ClusteringHandles::default(),
        lineage_config: LineageConfig::default(),
        clustering_enabled: false,
    }));

    let account_authority = AccountAuthoritySelection::open(
        app_state.db_pool.global(),
        &ClusteringHandles::default(),
        false,
        true,
    )
    .await
    .expect("open account coordinator");
    app_state.publish_account_authority(account_authority);

    let sm_selection = DurableSmSelection::open(app_state.db_pool.global(), false)
        .await
        .expect("open durable SM");
    app_state.publish_durable_sm(sm_selection);

    let public_websocket_url =
        url::Url::parse(&format!("wss://{domain}/ws")).expect("test websocket url");
    let mut auth_state_inner = AuthState::new(
        Arc::clone(&app_state),
        &server_config,
        &public_websocket_url,
        Some(b"test-encryption-key-32-bytes!!!"),
    );
    auth_state_inner.xmpp_domain = domain.clone();
    auth_state_inner.caps_server_domain =
        ServerDomainJid::parse(&domain).expect("fixture XMPP domain");
    let auth_state = Arc::new(auth_state_inner);

    let mut dispatcher = StanzaDispatcher::new();
    waddle_xmpp::protocol::handlers::register_default_handlers(&mut dispatcher);
    waddle_xmpp::protocol::handlers::register_default_message_handlers(&mut dispatcher);

    let notification_settings_projection = Arc::new(NotificationSettingsProjectionStore::new(
        pubsub_storage.database(),
    ));
    let dnd_projection = Arc::new(DndProjectionStore::new(pubsub_storage.database()));
    let dnd_reader = Arc::new(PepDndReader::with_system_clock(Arc::clone(&dnd_projection)));
    let notification_activity = Arc::new(
        NotificationActivityStore::try_new(app_state.db_pool.global().clone())
            .await
            .expect("notification activity store"),
    );
    let push_secret: [u8; 32] = rand::random();
    let push_service = Arc::new(
        DatabasePushServiceStore::new_with_secret_key_and_pubsub(
            app_state.db_pool.global().clone(),
            &push_secret,
            format!("push.{domain}").parse().expect("push service jid"),
            Arc::clone(&pubsub_dyn),
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

    let call_teardown_outbox = Arc::new(
        CallTeardownOutboxStore::new_with_node_identity(
            app_state.db_pool.global().clone(),
            waddle_xmpp::ownership::SharedNodeIdentity::new(
                waddle_xmpp::ownership::NodeIdentity::local(),
            ),
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

    let tenant_id = waddle_broker_core::identity::TenantId::parse(&domain)
        .expect("requester domain parses as canonical TenantId");
    let command_registry = Arc::new(CommandRegistry::for_tenant(tenant_id));
    super::register_extension_commands(
        Arc::clone(&extension_manager),
        Arc::clone(&command_registry),
        Arc::clone(&pubsub_dyn),
        format!("pubsub.{domain}").parse().expect("pubsub owner"),
        Arc::clone(&app_state),
    )
    .await;

    let sm_session_registry = Arc::new(
        InMemorySmSessionRegistry::new().with_persistence(Arc::new(InMemorySmPersistence::new())),
    );
    if let Some(store) = app_state.durable_sm() {
        sm_session_registry.attach_durable_detached(Arc::new(DurableDetachedRecipients::new(
            DurableSmStore::clone(store),
        )));
    }

    let ingress = Arc::new(
        IngressAuthority::try_new(
            server_config.ingress.clone(),
            app_state.db_pool.global().clone(),
            LineageConfig {
                deployment_uuid: Some(
                    "018f47b2-4b2e-7a3a-9a4c-52a5a6a90001"
                        .parse()
                        .expect("fixture deployment UUID"),
                ),
                action: None,
            },
            #[cfg(feature = "clustering")]
            None,
        )
        .await
        .expect("ingress authority"),
    );

    let connection_registry = Arc::new(ConnectionRegistry::new());
    let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
    let shutdown = GracefulShutdown::new(std::time::Duration::from_secs(drain_timeout_secs));
    let service_domains = XmppServiceDomains {
        muc: muc_domain_str,
        spaces: format!("spaces.{domain}"),
        upload: format!("upload.{domain}"),
        extensions: format!("extensions.{domain}"),
        push: format!("push.{domain}"),
        community: format!("community.{domain}"),
    };
    let inference_stream_tasks = tokio_util::task::TaskTracker::new();

    let websocket_state = Arc::new(WebSocketState {
        #[cfg(feature = "test-support")]
        sasl2_test_capability: false,
        room_listings: Default::default(),
        muji_session_rooms: Default::default(),
        local_space_publications: Arc::new(tokio::sync::Semaphore::new(8)),
        deps: WebSocketDeps {
            app_state,
            auth_state,
            service_domains,
            remote_route: Arc::new(FederationDisabled),
            protocol: ProtocolServices {
                connection_registry: Arc::clone(&connection_registry),
                user_registry: user_registry.clone(),
                room_registry,
                mam_storage,
                inbox_storage: Arc::clone(&inbox_storage),
                threads_storage: Arc::new(InboxBackedThreadsStorage::new(inbox_storage)),
                blocking_storage: Arc::new(waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new()),
                pending_delivery_storage: Arc::new(
                    waddle_xmpp::pending_delivery::storage::InMemoryPendingDeliveryStorage::with_default_quota(),
                ),
                command_registry,
                extension_manager,
                bot_avatars: Arc::new(BotAvatars::default()),
                dispatcher: Arc::new(dispatcher),
                muji_pre_dispatch_terminate_rate_limit: Arc::new(TerminateRateLimit::with_defaults()),
                muji_pre_dispatch_action_rate_limit: Arc::new(MujiActionRateLimit::with_defaults()),
                pubsub_storage,
                push_store: Arc::new(
                    DatabasePushRegistrationStore::try_new(db_pool.global().clone())
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
                sm_session_registry,
                ingress,
                link_preview_resolves: default_link_preview_resolve_permits(),
                caps_resolver: Arc::new(CapsResolver::default()),
                avatar_source_locks: Arc::new(AvatarLockMap::new()),
                profile_publish_tracker: tokio_util::task::TaskTracker::new(),
                pep_feed_bridge: Arc::new(PepFeedBridge::new()),
                call_threads: Arc::new(dashmap::DashMap::new()),
                call_thread_end_locks: Arc::new(dashmap::DashMap::new()),
                extension_bot_rooms: Default::default(),
                remote_muc_memberships: Arc::new(RemoteMucMemberships::default()),
                pending_local_muc_departures: Arc::new(PendingLocalMucDepartures::default()),
                retired_accounts: Arc::new(RetiredAccounts::default()),
                resolver_affiliation_syncs: Arc::new(ResolverAffiliationSyncScheduler::default()),
                dm_call_threads: Arc::new(dashmap::DashMap::new()),
                dm_pin_store: Arc::new(DmPinStore::default()),
                dm_call_thread_projections: Arc::new(dashmap::DashSet::new()),
                pending_dm_call_offers: Arc::new(dashmap::DashMap::new()),
                sfu: None,
            },
            occupant_id_secret: server_config.occupant_id_secret.clone(),
            link_preview: server_config.link_preview.clone(),
            ws_keepalive: server_config.ws_keepalive,
            provider_ingress: Arc::new(ProviderIngressRegistry::default()),
            provider_dispatch_tasks: ProviderDispatchTracker::new(),
            inference_stream_tasks: inference_stream_tasks.clone(),
            shutdown: shutdown.handle(),
        },
    });

    (websocket_state, shutdown)
}

/// Exercises the live XEP-0050 decoupled extension command path through
/// `handle_xmpp_frame_with_admission` -> `handle_iq` -> `handle_command_iq` ->
/// `register_extension_commands` -> `ExtensionManager::invoke_command_decoupled_with_chunks`.
#[doc(hidden)]
pub async fn run_extension_command_decoupled_for_test(
    extension_manager: Arc<ExtensionManager>,
    node: String,
    requester: jid::FullJid,
    drain_timeout_secs: u64,
) -> DecoupledCommandDispatchHandle {
    let domain = requester.domain().as_str().to_owned();
    let (websocket_state, shutdown) =
        build_websocket_state_for_test(extension_manager, &domain, drain_timeout_secs).await;
    let connection_registry = Arc::clone(&websocket_state.deps.protocol.connection_registry);
    let user_registry = websocket_state.deps.protocol.user_registry.clone();
    let inference_stream_tasks = websocket_state.deps.inference_stream_tasks.clone();

    let (endpoint, outbound_rx) = SocketOutboundEndpoint::channel(16);
    let owner = connection_registry.register(requester.clone(), endpoint);
    let entry = connection_registry.entry(&requester).expect("entry");
    user_registry
        .ask(RegisterUserResource {
            jid: requester.clone(),
            entry,
        })
        .await
        .expect("register user resource");

    let mut conn = WsConnState::new_for_connection_ref(ConnectionRef::fresh(
        NodeLeaseIdentity::local_process(),
    ));
    conn.phase = ConnectionPhase::ready(requester.clone(), false);
    let localpart = requester
        .node()
        .map(|node| node.as_str().to_owned())
        .unwrap_or_else(|| "user".to_owned());
    conn.authenticated_session = Some(Session::new(
        &requester.to_bare().to_string(),
        &localpart,
        &localpart,
    ));
    conn.registry_owner = Some(owner);

    let frame = format!(
        "<iq type='set' from='{requester}' to='extensions.{domain}' id='decoupled-iq-1'>\
         <command xmlns='http://jabber.org/protocol/commands' node='{node}' action='execute'/>\
         </iq>"
    );
    let lifecycle = NodeLifecycle::new();
    let permit = lifecycle.admit().expect("serving permit");
    let stop_token = shutdown.stop_token();

    let batch = handle_xmpp_frame_with_admission(
        &frame,
        &domain,
        &websocket_state,
        &mut conn,
        &permit,
        &stop_token,
    )
    .await;
    let session = conn.active_inference_sessions.pop();
    let immediate_frames = batch.into_serialized_frames();

    DecoupledCommandDispatchHandle {
        immediate_frames,
        outbound_rx,
        session,
        inference_stream_tasks,
        shutdown,
        _websocket_state: websocket_state,
        _conn: conn,
        _lifecycle: lifecycle,
        _permit: permit,
    }
}
