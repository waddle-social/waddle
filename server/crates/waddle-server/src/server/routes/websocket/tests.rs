use super::*;
use super::{
    frame::handle_xmpp_frame,
    interpret_loop::build_interpret_deps,
    replay::drive_interpret_loop,
    state::WsConnState,
    transport_xml::{build_stream_features_xml, sasl_failure_xml, sasl_success_xml},
};
use crate::config::ServerConfig;
use crate::db::{DatabaseConfig, DatabasePool, MigrationRunner, PoolConfig};
use crate::permissions::{Object, ObjectType, Permission, Relation, Subject, Tuple, WriteTuple};
use crate::server::bootstrap_membership::DEPLOYMENT_SERVER_ID;
use crate::server::AppState;
use hmac::{Hmac, Mac};
use pbkdf2::pbkdf2_hmac;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio::sync::mpsc;
// Handler functions moved to sub-modules but called directly in tests
use handlers::iq::{
    handle_iq, handle_iq_with_conn_state, managed_channel_permission_allowed, IqConnState,
};
use handlers::presence::{handle_muc_join, handle_muc_leave, parse_room_jid_context};
// Types moved out of mod.rs scope but used in tests
use waddle_extensions::ExtensionConfig;
use waddle_xmpp::commands::{CommandContext, CommandResult};
use waddle_xmpp::muc::room_actor::{
    ChangeAffiliation, GetSnapshot, JoinWithAffiliation, SetSubject,
};
use waddle_xmpp::registry::BroadcastOutcome;
use waddle_xmpp::Affiliation;
use xmpp_parsers::iq::{Iq, IqType};
use xmpp_parsers::message::MessageType as XmppMessageType;

mod broadcast;
mod dispatch;
mod frame_parsing;
mod iq;
mod messages;
mod misc;
mod muc;
mod registration;
mod send;
mod stream_features;
mod stream_management;

pub(crate) async fn create_test_websocket_state() -> Arc<WebSocketState> {
    create_test_websocket_state_with_extension_manager(empty_extension_manager().await).await
}

async fn empty_extension_manager() -> Arc<ExtensionManager> {
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

async fn create_test_websocket_state_with_extension_manager(
    extension_manager: Arc<ExtensionManager>,
) -> Arc<WebSocketState> {
    let config = DatabaseConfig::default();
    let pool_config = PoolConfig;
    let db_pool = DatabasePool::new(config, pool_config)
        .await
        .expect("db pool");

    let runner = MigrationRunner::global();
    runner.run(db_pool.global()).await.expect("migrations");

    let server_config = ServerConfig::test_homeserver();
    let app_state = Arc::new(AppState::new(Arc::new(db_pool)));
    let mut auth_state_inner = AuthState::new(
        app_state.clone(),
        &server_config,
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
    let mam_storage: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());

    let mut dispatcher = StanzaDispatcher::new();
    waddle_xmpp::protocol::handlers::register_default_handlers(&mut dispatcher);
    waddle_xmpp::protocol::handlers::register_default_message_handlers(&mut dispatcher);
    let pubsub_storage = Arc::new(
        crate::pubsub::DatabasePubSubStorage::open(Some("sqlite::memory:"))
            .await
            .expect("pubsub storage"),
    );
    let notification_settings_projection = Arc::new(
        crate::notification_settings_projection::NotificationSettingsProjectionStore::new(
            pubsub_storage.database(),
        ),
    );
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
            b"waddle-push-service-test-secret-key",
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

    let test_inbox_storage: Arc<dyn waddle_xmpp::inbox::storage::InboxStorage> =
        Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());

    Arc::new(WebSocketState {
            deps: WebSocketDeps {
                app_state: Arc::clone(&app_state),
                auth_state,
                service_domains: XmppServiceDomains {
                    muc: "muc.example.com".to_string(),
                    spaces: "spaces.example.com".to_string(),
                    upload: "upload.example.com".to_string(),
                    extensions: "extensions.example.com".to_string(),
                    push: "push.example.com".to_string(),
                    community: "community.example.com".to_string(),
                },
                protocol: ProtocolServices {
                    connection_registry: Arc::new(ConnectionRegistry::new()),
                    room_registry: kameo::spawn(RoomRegistryActor::new(
                        "muc.example.com".to_string(),
                        OccupantIdSecret::new(b"test-occupant-id-secret-32-bytes-long".to_vec())
                            .expect("test secret meets length floor"),
                    )),
                    mam_storage,
                    inbox_storage: Arc::clone(&test_inbox_storage),
                    threads_storage: Arc::new(
                        crate::threads::storage::InboxBackedThreadsStorage::new(Arc::clone(
                            &test_inbox_storage,
                        )),
                    ),
                    blocking_storage: Arc::new(
                        waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new(),
                    ),
                    pending_delivery_storage: Arc::new(
                        waddle_xmpp::pending_delivery::storage::InMemoryPendingDeliveryStorage::with_default_quota(),
                    ),
                    command_registry: Arc::new(CommandRegistry::new()),
                    extension_manager,
                    dispatcher: Arc::new(dispatcher),
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
                    notification_settings_projection,
                    notification_activity,
                    isr_token_store: waddle_xmpp::isr::create_shared_store(),
                    sm_session_registry: Arc::new(InMemorySmSessionRegistry::new()),
                    resumable_sessions: Arc::new(dashmap::DashMap::new()),
                    caps_resolver: Arc::new(
                        crate::server::caps_resolution::CapsResolver::default(),
                    ),
                    avatar_source_locks: Arc::new(crate::profile::AvatarLockMap::new()),
                    profile_publish_tracker: tokio_util::task::TaskTracker::new(),
                    pep_feed_bridge: Arc::new(crate::pep_feed_bridge::PepFeedBridge::new()),
                    sfu: None,
                },
                occupant_id_secret: OccupantIdSecret::new(
                    b"test-occupant-id-secret-32-bytes-long".to_vec(),
                )
                .expect("test secret meets length floor"),
                provider_ingress: Arc::new(
                    crate::server::routes::extension_webhooks::ProviderIngressRegistry::default(),
                ),
                provider_dispatch_tasks:
                    crate::server::routes::extension_webhooks::ProviderDispatchTracker::new(),
            },
        })
}

async fn create_test_session(state: &WebSocketState, username: &str) -> Session {
    let session = Session::new(&uuid::Uuid::new_v4().to_string(), username, username);
    state
        .deps
        .auth_state
        .session_manager
        .create_session(&session)
        .await
        .expect("session");
    session
}

async fn create_test_server_owner_session(state: &WebSocketState, username: &str) -> Session {
    let session = create_test_session(state, username).await;
    state
        .deps
        .app_state
        .permission_actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(ObjectType::Server, DEPLOYMENT_SERVER_ID),
                Relation::new("owner"),
                Subject::user(&session.user_id),
            ),
        })
        .await
        .expect("server owner tuple");
    session
}

async fn register_test_native_user(state: &WebSocketState, username: &str, password: &str) {
    let native_user_store =
        NativeUserStore::new(state.deps.app_state.db_pool.global_actor().clone());
    native_user_store
        .register(crate::auth::native::RegisterRequest {
            username: username.to_string(),
            domain: state.deps.auth_state.xmpp_domain.clone(),
            password: password.to_string(),
            email: None,
        })
        .await
        .expect("native user");
}

fn scram_client_final_from_challenge(
    username: &str,
    password: &str,
    client_nonce: &str,
    challenge_b64: &str,
) -> String {
    type HmacSha256 = Hmac<Sha256>;

    fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
        let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
        mac.update(data);
        mac.finalize().into_bytes().to_vec()
    }

    fn sha256(data: &[u8]) -> Vec<u8> {
        let mut hasher = Sha256::new();
        hasher.update(data);
        hasher.finalize().to_vec()
    }

    let challenge = String::from_utf8(
        BASE64_STANDARD
            .decode(challenge_b64)
            .expect("challenge base64"),
    )
    .expect("challenge utf8");
    let mut combined_nonce = None;
    let mut salt_b64 = None;
    let mut iterations = None;
    for attr in challenge.split(',') {
        if let Some(value) = attr.strip_prefix("r=") {
            combined_nonce = Some(value.to_string());
        } else if let Some(value) = attr.strip_prefix("s=") {
            salt_b64 = Some(value.to_string());
        } else if let Some(value) = attr.strip_prefix("i=") {
            iterations = Some(value.parse::<u32>().expect("iterations"));
        }
    }

    let combined_nonce = combined_nonce.expect("combined nonce");
    let salt = BASE64_STANDARD
        .decode(salt_b64.expect("salt"))
        .expect("salt base64");
    let iterations = iterations.expect("iterations");

    let mut salted_password = vec![0u8; 32];
    pbkdf2_hmac::<Sha256>(password.as_bytes(), &salt, iterations, &mut salted_password);
    let client_key = hmac_sha256(&salted_password, b"Client Key");
    let stored_key = sha256(&client_key);
    let channel_binding = BASE64_STANDARD.encode("n,,");
    let client_final_without_proof = format!("c={channel_binding},r={combined_nonce}");
    let client_first_bare = format!(
        "n={},r={client_nonce}",
        waddle_xmpp::auth::encode_sasl_name(username)
    );
    let auth_message = format!("{client_first_bare},{challenge},{client_final_without_proof}");
    let client_signature = hmac_sha256(&stored_key, auth_message.as_bytes());
    let client_proof: Vec<u8> = client_key
        .iter()
        .zip(client_signature.iter())
        .map(|(left, right)| left ^ right)
        .collect();

    format!(
        "{client_final_without_proof},p={}",
        BASE64_STANDARD.encode(client_proof)
    )
}

async fn snapshot_room(
    state: &WebSocketState,
    room_jid: &BareJid,
) -> waddle_xmpp::muc::room_actor::RoomSnapshot {
    let room_actor = get_room_actor(state, room_jid).await.expect("room actor");
    room_actor.ask(GetSnapshot).await.expect("room snapshot")
}

fn parse_message_for_test(xml: &str) -> xmpp_parsers::message::Message {
    match parse_frame(xml).expect("message parses") {
        InboundFrame::Stanza(stanza) => match *stanza {
            Stanza::Message(msg) => msg,
            _ => panic!("expected message stanza"),
        },
        _ => panic!("expected message stanza"),
    }
}

fn message_frame_xml_with_id(id: String) -> String {
    let mut message = xmpp_parsers::message::Message::new(None::<jid::Jid>);
    message.id = Some(id);
    stanza_to_xml(&Stanza::Message(message))
}

fn assert_sample_payload(xml: &str, element_name: &str, url: &str, owner: &str, name: &str) {
    let parsed = parse_message_for_test(xml);
    let payload = parsed
        .payloads
        .iter()
        .find(|payload| {
            payload.name() == element_name && payload.ns() == "urn:waddle:test-extension:1"
        })
        .unwrap_or_else(|| panic!("missing {element_name} sample payload"));
    assert_eq!(payload.attr("url"), Some(url));
    assert_eq!(payload.attr("owner"), Some(owner));
    assert_eq!(payload.attr("name"), Some(name));
}

fn parse_iq_for_test(xml: &str) -> xmpp_parsers::iq::Iq {
    match parse_frame(xml).expect("iq parses") {
        InboundFrame::Stanza(stanza) => match *stanza {
            Stanza::Iq(iq) => iq,
            _ => panic!("expected iq stanza"),
        },
        _ => panic!("expected iq stanza"),
    }
}

fn disco_items_iq_frame(id: &str, to: &str, node: Option<&str>) -> String {
    let mut query =
        xmpp_parsers::minidom::Element::builder("query", waddle_xmpp::disco::DISCO_ITEMS_NS);
    if let Some(node) = node {
        query = query.attr("node", node);
    }
    stanza_to_xml(&Stanza::Iq(Iq {
        from: None,
        to: Some(to.parse().expect("valid iq destination")),
        id: id.to_string(),
        payload: IqType::Get(query.build()),
    }))
}

fn disco_info_iq_frame(id: &str, to: &str, node: Option<&str>) -> String {
    let mut query =
        xmpp_parsers::minidom::Element::builder("query", waddle_xmpp::disco::DISCO_INFO_NS);
    if let Some(node) = node {
        query = query.attr("node", node);
    }
    stanza_to_xml(&Stanza::Iq(Iq {
        from: None,
        to: Some(to.parse().expect("valid iq destination")),
        id: id.to_string(),
        payload: IqType::Get(query.build()),
    }))
}

fn iq_set_frame(id: &str, to: &str, payload: xmpp_parsers::minidom::Element) -> String {
    stanza_to_xml(&Stanza::Iq(Iq {
        from: None,
        to: Some(to.parse().expect("valid iq destination")),
        id: id.to_string(),
        payload: IqType::Set(payload),
    }))
}

fn ready_phase(jid: &FullJid) -> ConnectionPhase {
    ConnectionPhase::ready(jid.clone(), false)
}

/// Construct a per-connection [`XmppStateMachine`] seeded with the
/// shared test dispatcher (registered with the default message
/// handler chain) and drive the given message through the new
/// thin-adapter [`handle_message`]. Mirrors the production main
/// loop's bind-time wiring (#229 PR11/PR13) closely enough for the
/// unit-level tests in this module to assert end-to-end semantics
/// against the dispatcher path.
async fn handle_message_for_test(
    state: &WebSocketState,
    sender_jid: &FullJid,
    session: Option<&Session>,
    message: xmpp_parsers::message::Message,
) -> Vec<String> {
    let mut sm = XmppStateMachine::new(
        state.deps.auth_state.xmpp_domain.clone(),
        (*state.deps.protocol.dispatcher).clone(),
    );
    sm.transition_to_ready(sender_jid.clone(), false);
    sm.set_blocklist(Blocklist::empty());
    let phase = ConnectionPhase::ready(sender_jid.clone(), false);
    handlers::message::handle_message(message, state, &phase, Some(&mut sm), session).await
}

fn authenticated_phase_for_session(session: &Session, domain: &str) -> ConnectionPhase {
    let pending_jid: FullJid = format!("{}@{domain}/pending", session.xmpp_localpart)
        .parse()
        .expect("pending jid");
    ConnectionPhase::authenticated(&pending_jid)
}

// ---- B: Non-blocking broadcast ------------------------------------

// ---- C: MUC nick handling -----------------------------------------

// ---- D: stream feature advertisement --------------------------------
