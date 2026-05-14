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

mod dispatch;
mod registration;
mod send;
mod stream_management;

#[test]
fn service_domains_use_xmpp_domain_for_all_components() {
    let domains = XmppServiceDomains::new("waddle.social");

    assert_eq!(domains.extensions, "extensions.waddle.social");
    assert_eq!(domains.muc, "muc.waddle.social");
    assert_eq!(domains.spaces, "spaces.waddle.social");
    assert_eq!(domains.upload, "upload.waddle.social");
}

async fn create_test_websocket_state() -> Arc<WebSocketState> {
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

    Arc::new(WebSocketState {
            deps: WebSocketDeps {
                app_state,
                auth_state,
                service_domains: XmppServiceDomains {
                    muc: "muc.example.com".to_string(),
                    spaces: "spaces.example.com".to_string(),
                    upload: "upload.example.com".to_string(),
                    extensions: "extensions.example.com".to_string(),
                },
                protocol: ProtocolServices {
                    connection_registry: Arc::new(ConnectionRegistry::new()),
                    room_registry: kameo::spawn(RoomRegistryActor::new(
                        "muc.example.com".to_string(),
                        OccupantIdSecret::new(b"test-occupant-id-secret-32-bytes-long".to_vec())
                            .expect("test secret meets length floor"),
                    )),
                    mam_storage,
                    inbox_storage: Arc::new(
                        waddle_xmpp::inbox::storage::InMemoryInboxStorage::new(),
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
                    pubsub_storage: Arc::new(waddle_xmpp::pubsub::InMemoryPubSubStorage::new()),
                    push_store: Arc::new(waddle_xmpp::push::InMemoryPushStore::new()),
                    isr_token_store: waddle_xmpp::isr::create_shared_store(),
                    sm_session_registry: Arc::new(InMemorySmSessionRegistry::new()),
                    resumable_sessions: Arc::new(dashmap::DashMap::new()),
                    caps_resolver: Arc::new(
                        crate::server::caps_resolution::CapsResolver::default(),
                    ),
                    avatar_source_locks: Arc::new(dashmap::DashMap::new()),
                    profile_publish_tracker: tokio_util::task::TaskTracker::new(),
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

#[tokio::test]
async fn extension_route_channel_permission_allows_bootstrap_chat_member() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "alice").await;

    assert!(
        !managed_channel_permission_allowed(
            state.as_ref(),
            Some(&session),
            "chat",
            Permission::View,
        )
        .await
        .expect("initial permission check"),
        "server membership should be required before default chat policy applies"
    );

    state
        .deps
        .app_state
        .permission_actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(ObjectType::Server, DEPLOYMENT_SERVER_ID),
                Relation::new("member"),
                Subject::user(&session.user_id),
            ),
        })
        .await
        .expect("server member tuple");

    let owner_session = create_test_session(state.as_ref(), "owner").await;
    state
        .deps
        .app_state
        .permission_actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(ObjectType::Server, DEPLOYMENT_SERVER_ID),
                Relation::new("owner"),
                Subject::user(&owner_session.user_id),
            ),
        })
        .await
        .expect("server owner tuple");

    assert!(
        managed_channel_permission_allowed(
            state.as_ref(),
            Some(&session),
            "chat",
            Permission::View,
        )
        .await
        .expect("chat permission check"),
        "default chat routes should inherit deployment membership"
    );
    assert!(
        managed_channel_permission_allowed(
            state.as_ref(),
            Some(&session),
            "announcements",
            Permission::View,
        )
        .await
        .expect("announcements view permission check"),
        "default announcement route reads should inherit deployment membership"
    );
    assert!(
        managed_channel_permission_allowed(
            state.as_ref(),
            Some(&owner_session),
            "chat",
            Permission::View,
        )
        .await
        .expect("owner chat permission check"),
        "default room membership policy must include deployment owners"
    );
    assert!(
        managed_channel_permission_allowed(
            state.as_ref(),
            Some(&owner_session),
            "announcements",
            Permission::SendMessage,
        )
        .await
        .expect("owner announcements send permission check"),
        "deployment owners should be allowed to publish announcement extension state"
    );
    assert!(
        !managed_channel_permission_allowed(
            state.as_ref(),
            Some(&session),
            "announcements",
            Permission::SendMessage,
        )
        .await
        .expect("announcements send permission check"),
        "announcement extension publishes still require owner permissions"
    );
    state
        .deps
        .app_state
        .permission_actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(ObjectType::Channel, "announcements"),
                Relation::new("writer"),
                Subject::user(&session.user_id),
            ),
        })
        .await
        .expect("announcement writer tuple");
    assert!(
        !managed_channel_permission_allowed(
            state.as_ref(),
            Some(&session),
            "announcements",
            Permission::SendMessage,
        )
        .await
        .expect("announcements writer permission check"),
        "announcement channel writer grants must not bypass server-owner write policy"
    );
    assert!(
        !managed_channel_permission_allowed(
            state.as_ref(),
            Some(&session),
            "random",
            Permission::View,
        )
        .await
        .expect("random channel permission check"),
        "non-default channels still require channel permissions"
    );
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

#[tokio::test]
async fn handle_xmpp_frame_open_dispatches_via_typed_ingress() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<open xmlns="urn:ietf:params:xml:ns:xmpp-framing" to="example.com" version="1.0"/>"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 2);
    assert!(responses[0].contains("urn:ietf:params:xml:ns:xmpp-framing"));
    assert!(responses[1].contains("<features"));
}

#[tokio::test]
async fn handle_xmpp_frame_auth_dispatches_via_typed_ingress() {
    let state = create_test_websocket_state().await;
    let session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let payload = BASE64_STANDARD.encode(format!("n,,\x01auth=Bearer {}\x01\x01", session.id));
    let frame = element_to_xml(
        Element::builder("auth", waddle_xmpp::ns::SASL)
            .attr("mechanism", "OAUTHBEARER")
            .append(payload)
            .build(),
    );
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(&frame, "example.com", state.as_ref(), &mut conn).await;

    assert_eq!(responses, vec![sasl_success_xml()]);
    assert!(conn.phase.is_authenticated());
    assert!(conn.authenticated_session.is_some());
    assert!(matches!(conn.phase, ConnectionPhase::Authenticated { .. }));
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_auth_returns_malformed_request() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<auth xmlns="urn:ietf:params:xml:ns:xmpp-sasl">payload</auth>"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses, vec![sasl_failure_xml("malformed-request")]);
}

#[tokio::test]
async fn sync_state_machine_phase_mirrors_closing_into_sm() {
    // PR269 review fix #2/#6: when WsConnState.phase transitions
    // to Closing (via SASL failure / stream-error / explicit
    // shutdown inside `handle_xmpp_frame`), the per-connection SM
    // must mirror so it stops accepting late `PeerStanza`
    // dispatches from the outbound channel.
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let jid: jid::FullJid = "alice@example.com/web".parse().expect("jid");
    conn.ensure_state_machine(
        "example.com",
        &state.deps.protocol.dispatcher,
        jid.clone(),
        false,
        Blocklist::empty(),
    );

    // Sanity: SM starts in Ready.
    assert!(matches!(
        conn.state_machine.as_ref().expect("sm").phase(),
        ConnectionPhase::Ready { .. }
    ));

    // Simulate the legacy phase tracker transitioning to Closing.
    conn.phase = ConnectionPhase::closing(Some(jid.clone()));
    conn.sync_state_machine_phase();

    assert!(matches!(
        conn.state_machine.as_ref().expect("sm").phase(),
        ConnectionPhase::Closing { .. }
    ));
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_sasl_response_returns_malformed_request() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<response xmlns="urn:ietf:params:xml:ns:xmpp-sasl">not-closed"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses, vec![sasl_failure_xml("malformed-request")]);
}

#[tokio::test]
async fn handle_xmpp_frame_wrong_namespace_auth_stays_silent() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<auth xmlns="jabber:client" mechanism="SCRAM-SHA-256">x</auth>"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert!(responses.is_empty(), "expected no response: {responses:?}");
}

#[tokio::test]
async fn websocket_features_advertise_oauthbearer() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<open xmlns="urn:ietf:params:xml:ns:xmpp-framing" to="example.com" version="1.0"/>"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 2);
    let features = &responses[1];
    assert!(
        features.contains("<mechanism>OAUTHBEARER</mechanism>"),
        "expected OAUTHBEARER in WebSocket SASL mechanisms"
    );
    assert!(
        features.contains("<mechanism>SCRAM-SHA-256</mechanism>"),
        "expected SCRAM-SHA-256 in WebSocket SASL mechanisms"
    );
    assert!(
        !features.contains("<mechanism>PLAIN</mechanism>"),
        "expected WebSocket SASL mechanisms to exclude PLAIN"
    );
}

#[tokio::test]
async fn websocket_close_moves_connection_into_closing_phase() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<close xmlns="urn:ietf:params:xml:ns:xmpp-framing"/>"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(
        responses,
        vec![r#"<close xmlns="urn:ietf:params:xml:ns:xmpp-framing"/>"#.to_string()]
    );
    assert!(matches!(conn.phase, ConnectionPhase::Closing { .. }));
}

#[tokio::test]
async fn websocket_close_keeps_bound_connection_in_closing_phase() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
    conn.phase = ConnectionPhase::ready(jid, false);

    let responses = handle_xmpp_frame(
        r#"<close xmlns="urn:ietf:params:xml:ns:xmpp-framing"/>"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(
        responses,
        vec![r#"<close xmlns="urn:ietf:params:xml:ns:xmpp-framing"/>"#.to_string()]
    );
    assert!(matches!(conn.phase, ConnectionPhase::Closing { .. }));

    let _ = handle_xmpp_frame(
        r#"<close xmlns="urn:ietf:params:xml:ns:xmpp-framing"/>"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;
    assert!(matches!(conn.phase, ConnectionPhase::Closing { .. }));
}

#[tokio::test]
async fn websocket_rejects_plain_auth() {
    let state = create_test_websocket_state().await;
    let frame = element_to_xml(
        Element::builder("auth", waddle_xmpp::ns::SASL)
            .attr("mechanism", "PLAIN")
            .append(BASE64_STANDARD.encode("\0alice\0session-token"))
            .build(),
    );
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(&frame, "example.com", state.as_ref(), &mut conn).await;

    assert_eq!(responses, vec![sasl_failure_xml("invalid-mechanism")]);
    assert!(!conn.phase.is_authenticated());
    assert!(!conn.phase.is_ready());
    assert!(conn.authenticated_session.is_none());
}

#[tokio::test]
async fn websocket_oauthbearer_authenticates_session_token() {
    let state = create_test_websocket_state().await;
    let session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let payload = BASE64_STANDARD.encode(format!("n,,\x01auth=Bearer {}\x01\x01", session.id));
    let frame = element_to_xml(
        Element::builder("auth", waddle_xmpp::ns::SASL)
            .attr("mechanism", "OAUTHBEARER")
            .append(payload)
            .build(),
    );
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(&frame, "example.com", state.as_ref(), &mut conn).await;

    assert_eq!(responses, vec![sasl_success_xml()]);
    assert!(conn.phase.is_authenticated());
    assert!(!conn.phase.is_ready());
    assert_eq!(
        conn.authenticated_session
            .as_ref()
            .map(|s| s.user_id.as_str()),
        Some(session.user_id.as_str())
    );
    let expected_bare =
        localpart_to_jid(&session.xmpp_localpart, &state.deps.auth_state.xmpp_domain)
            .expect("session localpart should produce JID");
    assert_eq!(
        conn.phase.authenticated_bare_jid().map(ToString::to_string),
        Some(expected_bare)
    );
    assert!(matches!(conn.phase, ConnectionPhase::Authenticated { .. }));
}

#[tokio::test]
async fn websocket_rejects_reauthentication_after_successful_sasl() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "alice").await;
    let payload = BASE64_STANDARD.encode(format!("n,,\x01auth=Bearer {}\x01\x01", session.id));
    let frame = element_to_xml(
        Element::builder("auth", waddle_xmpp::ns::SASL)
            .attr("mechanism", "OAUTHBEARER")
            .append(payload)
            .build(),
    );
    let mut conn = WsConnState::new();

    let first = handle_xmpp_frame(&frame, "example.com", state.as_ref(), &mut conn).await;
    assert_eq!(first, vec![sasl_success_xml()]);
    let first_bare_jid = conn.phase.authenticated_bare_jid().cloned();
    let first_user_id = conn
        .authenticated_session
        .as_ref()
        .map(|saved| saved.user_id.clone());

    let second = handle_xmpp_frame(&frame, "example.com", state.as_ref(), &mut conn).await;

    assert_eq!(second, vec![sasl_failure_xml("not-authorized")]);
    assert!(conn.phase.is_authenticated());
    assert!(!conn.phase.is_ready());
    assert_eq!(conn.phase.authenticated_bare_jid(), first_bare_jid.as_ref());
    assert_eq!(
        conn.authenticated_session
            .as_ref()
            .map(|saved| saved.user_id.clone()),
        first_user_id
    );
    assert!(matches!(conn.phase, ConnectionPhase::Authenticated { .. }));
}

#[tokio::test]
async fn websocket_failed_scram_response_resets_phase_to_unauthenticated() {
    let state = create_test_websocket_state().await;
    let domain = state.deps.auth_state.xmpp_domain.clone();
    register_test_native_user(state.as_ref(), "alice", "correct horse battery staple").await;
    let client_first = BASE64_STANDARD.encode("n,,n=alice,r=fyko+d2lbbFgONRv9qkxdawL");
    let auth_frame = element_to_xml(
        Element::builder("auth", waddle_xmpp::ns::SASL)
            .attr("mechanism", "SCRAM-SHA-256")
            .append(client_first)
            .build(),
    );
    let response_frame = element_to_xml(
        Element::builder("response", waddle_xmpp::ns::SASL)
            .append(BASE64_STANDARD.encode("not-valid"))
            .build(),
    );
    let mut conn = WsConnState::new();

    let auth_responses = handle_xmpp_frame(&auth_frame, &domain, state.as_ref(), &mut conn).await;
    assert_eq!(auth_responses.len(), 1);
    let challenge = Element::from_str(&auth_responses[0]).expect("challenge xml");
    assert_eq!(challenge.name(), "challenge");
    assert_eq!(conn.phase.scram_pending_username(), Some("alice"));

    let response_responses =
        handle_xmpp_frame(&response_frame, &domain, state.as_ref(), &mut conn).await;

    assert_eq!(response_responses, vec![sasl_failure_xml("not-authorized")]);
    assert!(matches!(conn.phase, ConnectionPhase::Unauthenticated));
    assert!(!conn.phase.is_authenticated());
    assert!(conn.authenticated_session.is_none());
}

#[tokio::test]
async fn websocket_malformed_scram_response_resets_phase_and_allows_retry() {
    let state = create_test_websocket_state().await;
    let domain = state.deps.auth_state.xmpp_domain.clone();
    register_test_native_user(state.as_ref(), "alice", "correct horse battery staple").await;
    let client_first = BASE64_STANDARD.encode("n,,n=alice,r=fyko+d2lbbFgONRv9qkxdawL");
    let auth_frame = element_to_xml(
        Element::builder("auth", waddle_xmpp::ns::SASL)
            .attr("mechanism", "SCRAM-SHA-256")
            .append(client_first)
            .build(),
    );
    let malformed_response = r#"<response xmlns="urn:ietf:params:xml:ns:xmpp-sasl">not-closed"#;
    let mut conn = WsConnState::new();

    let first = handle_xmpp_frame(&auth_frame, &domain, state.as_ref(), &mut conn).await;
    assert_eq!(first.len(), 1);
    assert_eq!(conn.phase.scram_pending_username(), Some("alice"));

    let malformed = handle_xmpp_frame(malformed_response, &domain, state.as_ref(), &mut conn).await;
    assert_eq!(malformed, vec![sasl_failure_xml("malformed-request")]);
    assert!(matches!(conn.phase, ConnectionPhase::Unauthenticated));
    assert!(!conn.phase.is_authenticated());
    assert!(conn.authenticated_session.is_none());

    let retry = handle_xmpp_frame(&auth_frame, &domain, state.as_ref(), &mut conn).await;
    assert_eq!(retry.len(), 1);
    let challenge = Element::from_str(&retry[0]).expect("challenge xml");
    assert_eq!(challenge.name(), "challenge");
    assert_eq!(conn.phase.scram_pending_username(), Some("alice"));
}

#[tokio::test]
async fn websocket_failed_reauth_during_scram_resets_phase_and_allows_retry() {
    let state = create_test_websocket_state().await;
    let domain = state.deps.auth_state.xmpp_domain.clone();
    register_test_native_user(state.as_ref(), "alice", "correct horse battery staple").await;
    let client_first = BASE64_STANDARD.encode("n,,n=alice,r=fyko+d2lbbFgONRv9qkxdawL");
    let auth_frame = element_to_xml(
        Element::builder("auth", waddle_xmpp::ns::SASL)
            .attr("mechanism", "SCRAM-SHA-256")
            .append(client_first)
            .build(),
    );
    let mut conn = WsConnState::new();

    let first = handle_xmpp_frame(&auth_frame, &domain, state.as_ref(), &mut conn).await;
    assert_eq!(first.len(), 1);
    assert_eq!(conn.phase.scram_pending_username(), Some("alice"));

    let second = handle_xmpp_frame(&auth_frame, &domain, state.as_ref(), &mut conn).await;
    assert_eq!(second, vec![sasl_failure_xml("not-authorized")]);
    assert!(matches!(conn.phase, ConnectionPhase::Unauthenticated));

    let third = handle_xmpp_frame(&auth_frame, &domain, state.as_ref(), &mut conn).await;
    assert_eq!(third.len(), 1);
    let challenge = Element::from_str(&third[0]).expect("challenge xml");
    assert_eq!(challenge.name(), "challenge");
    assert_eq!(conn.phase.scram_pending_username(), Some("alice"));
}

#[tokio::test]
async fn websocket_resource_bind_returns_client_iq() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "alice").await;
    let payload = BASE64_STANDARD.encode(format!("n,,\x01auth=Bearer {}\x01\x01", session.id));
    let auth_frame = element_to_xml(
        Element::builder("auth", waddle_xmpp::ns::SASL)
            .attr("mechanism", "OAUTHBEARER")
            .append(payload)
            .build(),
    );
    let bind_frame = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr("id", "bind-1")
            .attr("type", "set")
            .append(
                Element::builder("bind", waddle_xmpp::ns::BIND)
                    .append(
                        Element::builder("resource", waddle_xmpp::ns::BIND)
                            .append("web")
                            .build(),
                    )
                    .build(),
            )
            .build(),
    );
    let mut conn = WsConnState::new();

    let auth_responses =
        handle_xmpp_frame(&auth_frame, "example.com", state.as_ref(), &mut conn).await;
    assert_eq!(auth_responses, vec![sasl_success_xml()]);

    let bind_responses =
        handle_xmpp_frame(&bind_frame, "example.com", state.as_ref(), &mut conn).await;

    assert!(conn.phase.is_ready());
    assert_eq!(bind_responses.len(), 1);

    let response = Element::from_str(&bind_responses[0]).expect("bind response XML");
    assert_eq!(response.name(), "iq");
    assert_eq!(response.ns(), waddle_xmpp::ns::JABBER_CLIENT);
    assert_eq!(response.attr("id"), Some("bind-1"));
    assert_eq!(response.attr("type"), Some("result"));

    let bind = response
        .get_child("bind", waddle_xmpp::ns::BIND)
        .expect("bind child");
    let jid = bind
        .get_child("jid", waddle_xmpp::ns::BIND)
        .expect("jid child");
    let expected_bare =
        localpart_to_jid(&session.xmpp_localpart, &state.deps.auth_state.xmpp_domain)
            .expect("session localpart should produce JID");
    let expected_full = format!("{expected_bare}/web");
    assert!(
        jid.text() == expected_full,
        "bound jid should match expected resource"
    );
    let bound_jid = conn.phase.bound_jid().map(ToString::to_string);
    assert!(
        bound_jid.as_deref() == Some(expected_full.as_str()),
        "connection state should store the bound jid"
    );
    assert!(matches!(
        &conn.phase,
        ConnectionPhase::Ready {
            full_jid,
            resumed: false,
            ..
        } if full_jid.to_string() == expected_full
    ));
}

#[tokio::test]
async fn websocket_resource_bind_without_resource_uses_unique_server_resource() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "alice").await;
    let payload = BASE64_STANDARD.encode(format!("n,,\x01auth=Bearer {}\x01\x01", session.id));
    let auth_frame = element_to_xml(
        Element::builder("auth", waddle_xmpp::ns::SASL)
            .attr("mechanism", "OAUTHBEARER")
            .append(payload)
            .build(),
    );
    let bind_frame = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr("id", "bind-2")
            .attr("type", "set")
            .append(Element::builder("bind", waddle_xmpp::ns::BIND).build())
            .build(),
    );
    let mut conn = WsConnState::new();

    let auth_responses =
        handle_xmpp_frame(&auth_frame, "example.com", state.as_ref(), &mut conn).await;
    assert_eq!(auth_responses, vec![sasl_success_xml()]);

    let bind_responses =
        handle_xmpp_frame(&bind_frame, "example.com", state.as_ref(), &mut conn).await;

    assert!(conn.phase.is_ready());
    assert_eq!(bind_responses.len(), 1);

    let response = Element::from_str(&bind_responses[0]).expect("bind response XML");
    let bind = response
        .get_child("bind", waddle_xmpp::ns::BIND)
        .expect("bind child");
    let jid = bind
        .get_child("jid", waddle_xmpp::ns::BIND)
        .expect("jid child")
        .text();

    let expected_bare =
        localpart_to_jid(&session.xmpp_localpart, &state.deps.auth_state.xmpp_domain)
            .expect("session localpart should produce JID");
    let prefix = format!("{expected_bare}/ws-");
    assert!(
        jid.starts_with(&prefix),
        "server-assigned resource should be unique ws-* value: {jid}"
    );
    assert!(matches!(
        &conn.phase,
        ConnectionPhase::Ready {
            full_jid,
            resumed: false,
            ..
        } if full_jid.to_string() == jid
    ));
}

#[tokio::test]
async fn websocket_rejects_second_resource_bind_after_ready() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "alice").await;
    let payload = BASE64_STANDARD.encode(format!("n,,\x01auth=Bearer {}\x01\x01", session.id));
    let auth_frame = element_to_xml(
        Element::builder("auth", waddle_xmpp::ns::SASL)
            .attr("mechanism", "OAUTHBEARER")
            .append(payload)
            .build(),
    );
    let bind_one = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr("id", "bind-1")
            .attr("type", "set")
            .append(
                Element::builder("bind", waddle_xmpp::ns::BIND)
                    .append(
                        Element::builder("resource", waddle_xmpp::ns::BIND)
                            .append("web")
                            .build(),
                    )
                    .build(),
            )
            .build(),
    );
    let bind_two = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr("id", "bind-2")
            .attr("type", "set")
            .append(
                Element::builder("bind", waddle_xmpp::ns::BIND)
                    .append(
                        Element::builder("resource", waddle_xmpp::ns::BIND)
                            .append("mobile")
                            .build(),
                    )
                    .build(),
            )
            .build(),
    );
    let mut conn = WsConnState::new();

    let auth_responses =
        handle_xmpp_frame(&auth_frame, "example.com", state.as_ref(), &mut conn).await;
    assert_eq!(auth_responses, vec![sasl_success_xml()]);
    let bind_one_responses =
        handle_xmpp_frame(&bind_one, "example.com", state.as_ref(), &mut conn).await;
    assert_eq!(bind_one_responses.len(), 1);
    let first_bound_jid = conn.phase.bound_jid().cloned();

    let bind_two_responses =
        handle_xmpp_frame(&bind_two, "example.com", state.as_ref(), &mut conn).await;

    assert_eq!(
        bind_two_responses,
        vec![build_iq_error_xml_typed(
            "bind-2",
            None,
            None,
            not_authorized_iq_error("Authentication required."),
        )]
    );
    assert_eq!(conn.phase.bound_jid(), first_bound_jid.as_ref());
    assert!(matches!(conn.phase, ConnectionPhase::Ready { .. }));
}

#[tokio::test]
async fn muc_stale_leave_does_not_remove_current_resource() {
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "channel@muc.example.com".parse().expect("room jid");
    let current_jid: FullJid = "alice@example.com/current".parse().expect("current jid");
    let stale_jid: FullJid = "alice@example.com/stale".parse().expect("stale jid");

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &current_jid,
        "alice",
        &Some(owner_session),
    )
    .await;

    let responses = handle_muc_leave(state.as_ref(), &room_jid, &stale_jid, "alice").await;

    assert_eq!(responses.len(), 1);
    let response = Element::from_str(&responses[0]).expect("leave response XML");
    assert_eq!(response.name(), "presence");
    assert_eq!(response.attr("type"), Some("unavailable"));

    let room = snapshot_room(state.as_ref(), &room_jid).await.room;
    assert_eq!(room.find_nick_by_real_jid(&current_jid), Some("alice"));
    assert!(room.find_nick_by_real_jid(&stale_jid).is_none());
    assert_eq!(room.occupant_count(), 1);
}

#[tokio::test]
async fn muc_join_responses_use_client_namespace() {
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "channel@muc.example.com".parse().expect("room jid");
    let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");

    let responses = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &sender_jid,
        "alice",
        &Some(owner_session),
    )
    .await;

    assert_eq!(responses.len(), 2);

    let self_presence = Element::from_str(&responses[0]).expect("self presence xml");
    assert_eq!(self_presence.name(), "presence");
    assert_eq!(self_presence.ns(), waddle_xmpp::ns::JABBER_CLIENT);
    let user_x = self_presence
        .get_child("x", "http://jabber.org/protocol/muc#user")
        .expect("muc user payload");
    let item = user_x
        .get_child("item", "http://jabber.org/protocol/muc#user")
        .expect("muc user item");
    assert_eq!(item.attr("jid"), Some("alice@example.com/web"));
    assert_eq!(item.attr("affiliation"), Some("owner"));
    assert_eq!(item.attr("role"), Some("moderator"));
    assert!(user_x
        .children()
        .any(|child| child.name() == "status" && child.attr("code") == Some("100")));
    assert!(user_x
        .children()
        .any(|child| child.name() == "status" && child.attr("code") == Some("110")));
    assert!(
        self_presence
            .get_child("occupant-id", waddle_xmpp::xep::xep0421::NS_OCCUPANT_ID)
            .is_some(),
        "self-presence must carry XEP-0421 occupant-id"
    );

    let subject_message = Element::from_str(&responses[1]).expect("subject xml");
    assert_eq!(subject_message.name(), "message");
    assert_eq!(subject_message.ns(), waddle_xmpp::ns::JABBER_CLIENT);
    assert_eq!(subject_message.attr("type"), Some("groupchat"));
}

#[tokio::test]
async fn xep_0045_join_replay_exposes_existing_occupant_real_jids() {
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "icepuma").await;
    let room_jid: BareJid = "mentions@muc.example.com".parse().expect("room jid");

    let occupants = [
        ("icepuma", "icepuma@example.com/web"),
        ("randax", "randax@example.com/desktop"),
        ("rawkode", "rawkode@example.com/mobile"),
    ];

    for (index, (nick, full_jid)) in occupants.iter().enumerate() {
        let sender_jid: FullJid = full_jid.parse().expect("occupant jid");
        let session = if index == 0 {
            Some(owner_session.clone())
        } else {
            None
        };
        let _ = handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &sender_jid,
            nick,
            &session,
        )
        .await;
    }

    let joiner: FullJid = "witness@example.com/browser".parse().expect("joiner jid");
    let responses = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &joiner,
        "witness",
        &None,
    )
    .await;

    for (nick, full_jid) in occupants {
        let from = format!("{room_jid}/{nick}");
        let replay = responses
            .iter()
            .filter_map(|xml| Element::from_str(xml).ok())
            .find(|element| {
                element.name() == "presence"
                    && element.attr("from") == Some(from.as_str())
                    && element.attr("to") == Some(joiner.as_str())
            })
            .unwrap_or_else(|| panic!("missing replay presence for {nick}: {responses:?}"));
        let user_x = replay
            .get_child("x", "http://jabber.org/protocol/muc#user")
            .expect("muc user payload");
        let item = user_x
            .get_child("item", "http://jabber.org/protocol/muc#user")
            .expect("muc user item");

        assert_eq!(item.attr("jid"), Some(full_jid));
        assert!(
            user_x
                .children()
                .any(|child| child.name() == "status" && child.attr("code") == Some("100")),
            "non-anonymous replay must disclose status 100 for {nick}"
        );
        assert!(
            !user_x
                .children()
                .any(|child| child.name() == "status" && child.attr("code") == Some("110")),
            "replay presence for another occupant must not be self-presence"
        );
        assert!(
            replay
                .get_child("occupant-id", waddle_xmpp::xep::xep0421::NS_OCCUPANT_ID)
                .is_some(),
            "replay presence for {nick} must carry XEP-0421 occupant-id"
        );
    }
}

#[tokio::test]
async fn xep_0045_section_7_2_15_join_replay_serializes_full_subject_envelope() {
    // Boundary test for the WebSocket join wiring (Copilot review,
    // PR #319). Pre-populates `MucRoom.subject` with a multi-language
    // SubjectState via the production `SetSubject` actor message,
    // then drives a fresh join through `handle_muc_join` and asserts
    // the serialized subject message carries every conformance
    // element: `from='room/setter_nick'`, every persisted
    // `<subject xml:lang='...'>`, the XEP-0203 `<delay/>` from the
    // room JID, and the XEP-0421 `<occupant-id/>`.
    use chrono::TimeZone;
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let setter_session = create_test_server_owner_session(state.as_ref(), "setter").await;
    let room_jid: BareJid = "channel@muc.example.com".parse().expect("room jid");
    let setter_jid: FullJid = "setter@example.com/web".parse().expect("setter jid");
    let joiner_jid: FullJid = "alice@example.com/web".parse().expect("joiner jid");

    // Bootstrap the room actor by joining the setter (first joiner
    // becomes Owner → Moderator), then seed the subject state with
    // a multi-language `texts` map matching what a real §8.1
    // dispatch would produce.
    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &setter_jid,
        "setter-nick",
        &Some(setter_session),
    )
    .await;
    let room_actor = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor");
    let texts = waddle_xmpp::muc::RoomSubjectTexts::from_iter([
        (String::new(), "Default subject".to_string()),
        ("en".to_string(), "English subject".to_string()),
    ]);
    let set_at = chrono::Utc.with_ymd_and_hms(2026, 5, 2, 12, 0, 0).unwrap();
    room_actor
        .ask(SetSubject {
            texts,
            setter: setter_jid.to_bare(),
            setter_nick: "setter-nick".to_string(),
            set_at,
        })
        .await
        .expect("SetSubject succeeds");

    let responses = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &joiner_jid,
        "alice",
        &Some(owner_session),
    )
    .await;

    // 1 existing-occupant presence (setter) + self-presence + subject = 3.
    assert_eq!(
        responses.len(),
        3,
        "join responses: existing-occupants + self-presence + subject"
    );

    let subject_msg =
        Element::from_str(responses.last().expect("subject is last")).expect("subject xml");
    assert_eq!(subject_msg.name(), "message");
    assert_eq!(subject_msg.attr("type"), Some("groupchat"));
    assert_eq!(
        subject_msg.attr("from"),
        Some("channel@muc.example.com/setter-nick"),
        "§7.2.15 nick-form `from` for set room"
    );

    let subject_children: Vec<&Element> = subject_msg
        .children()
        .filter(|c| c.name() == "subject")
        .collect();
    assert_eq!(
        subject_children.len(),
        2,
        "every persisted xml:lang variant round-trips into the join replay"
    );
    let default_subject = subject_children
        .iter()
        .find(|c| c.attr("xml:lang").is_none() || c.attr("xml:lang") == Some(""))
        .expect("default-language subject present");
    assert_eq!(default_subject.text(), "Default subject");
    let en_subject = subject_children
        .iter()
        .find(|c| c.attr("xml:lang") == Some("en"))
        .expect("xml:lang=en subject present");
    assert_eq!(en_subject.text(), "English subject");

    let delay = subject_msg
        .get_child("delay", "urn:xmpp:delay")
        .expect("XEP-0203 <delay/> stamped per §7.2.15 SHOULD");
    assert_eq!(
        delay.attr("from"),
        Some("channel@muc.example.com"),
        "§7.2.15 conditional MUST: delay's `from` is the room JID"
    );
    assert!(
        delay.attr("stamp").is_some_and(|s| !s.is_empty()),
        "delay stamp present and non-empty"
    );

    let occupant_id = subject_msg
        .get_child("occupant-id", "urn:xmpp:occupant-id:0")
        .expect("XEP-0421 <occupant-id/> stamped on set-subject replay");
    assert!(
        occupant_id.attr("id").is_some_and(|s| !s.is_empty()),
        "occupant-id `id` attribute present"
    );

    assert!(
        subject_msg.children().all(|c| c.name() != "body"),
        "subject message MUST have no <body/>"
    );
}

#[test]
fn test_parse_room_jid_valid() {
    let jid: jid::BareJid = "channel456@muc.example.com".parse().unwrap();
    let (waddle, channel) = parse_room_jid_context(&jid);
    assert_eq!(waddle, "space");
    assert_eq!(channel, "channel456");
}

#[test]
fn test_parse_room_jid_fallback() {
    let jid: jid::BareJid = "singlename@muc.example.com".parse().unwrap();
    let (waddle, channel) = parse_room_jid_context(&jid);
    assert_eq!(waddle, "space");
    assert_eq!(channel, "singlename");
}

#[tokio::test]
async fn handle_iq_roster_query_returns_parseable_result() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
    let frame = r#"<iq xmlns="jabber:client" id="roster-1" type="get"><query xmlns="jabber:iq:roster"/></iq>"#;
    let responses = handle_iq(
        frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;
    assert_eq!(responses.len(), 1);

    let iq_xml = responses.first().expect("roster response");
    let element = Element::from_str(iq_xml).expect("valid IQ XML");
    let iq = xmpp_parsers::iq::Iq::try_from(element).expect("parseable IQ");

    assert_eq!(iq.id, "roster-1");
    match iq.payload {
        xmpp_parsers::iq::IqType::Result(Some(payload)) => {
            assert_eq!(payload.name(), "query");
            assert_eq!(payload.ns(), "jabber:iq:roster");
        }
        other => panic!("expected roster IQ result payload, got {other:?}"),
    }
}

#[tokio::test]
async fn handle_xmpp_frame_roster_get_marks_connection_interested_for_detach() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready(jid, false);
    let frame = r#"<iq xmlns="jabber:client" id="roster-interest" type="get"><query xmlns="jabber:iq:roster"/></iq>"#;

    let responses = handle_xmpp_frame(frame, "example.com", state.as_ref(), &mut conn).await;

    assert_eq!(responses.len(), 1);
    assert!(
        conn.roster_interested,
        "roster get must persist interest on WsConnState for SM detach"
    );
}

#[tokio::test]
async fn handle_iq_roster_query_without_xmlns_survives_xmlns_like_attribute_value() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
    let frame = r#"<iq id="roster-attr" type="get" data="xmlns=bogus"><query xmlns="jabber:iq:roster"/></iq>"#;
    let responses = handle_iq(
        frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;
    assert_eq!(responses.len(), 1);

    let iq_xml = responses.first().expect("roster response");
    let element = Element::from_str(iq_xml).expect("valid IQ XML");
    let iq = xmpp_parsers::iq::Iq::try_from(element).expect("parseable IQ");
    assert_eq!(iq.id, "roster-attr");
    assert!(matches!(
        iq.payload,
        xmpp_parsers::iq::IqType::Result(Some(_))
    ));
}

#[tokio::test]
async fn handle_iq_roster_query_requires_ready_phase() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "alice").await;
    let frame = r#"<iq xmlns="jabber:client" id="roster-prebind" type="get"><query xmlns="jabber:iq:roster"/></iq>"#;

    let responses = handle_iq(
        frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session.clone()),
        &authenticated_phase_for_session(&session, "example.com"),
    )
    .await;

    let response = responses.first().expect("roster auth error");
    assert!(
        response.contains("not-authorized"),
        "pre-bind roster should be rejected: {response}"
    );
    assert!(
        !response.contains("feature-not-implemented"),
        "pre-bind roster should not fall through as unimplemented: {response}"
    );
}

#[tokio::test]
async fn handle_xmpp_frame_drops_oversized_input() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let huge = format!(
        "<iq id=\"big\">{}</iq>",
        "a".repeat(waddle_xmpp::protocol::frame::MAX_FRAME_SIZE)
    );
    let responses = handle_xmpp_frame(&huge, "example.com", state.as_ref(), &mut conn).await;
    assert!(responses.is_empty());
}

#[tokio::test]
async fn handle_xmpp_frame_drops_whitespace_padded_oversized_input() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let huge = format!(
        "{}<iq id=\"big\"/>",
        " ".repeat(waddle_xmpp::protocol::frame::MAX_FRAME_SIZE)
    );
    let responses = handle_xmpp_frame(&huge, "example.com", state.as_ref(), &mut conn).await;
    assert!(responses.is_empty());
}

#[tokio::test]
async fn handle_xmpp_frame_invalid_iq_returns_feature_not_implemented() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq id="bad-iq" type="get"><nope/></iq>"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(responses[0].contains("type=\"error\""));
    assert!(responses[0].contains("id=\"bad-iq\""));
    assert!(responses[0].contains("feature-not-implemented"));
}

#[tokio::test]
async fn handle_xmpp_frame_invalid_iq_result_returns_no_response() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq id="bad-result" type="result"><a/><b/></iq>"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert!(responses.is_empty(), "expected no response: {responses:?}");
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_xml_iq_request_preserves_legacy_error() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq id="broken-iq" type="get"><ping xmlns="urn:xmpp:ping"></iq"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(responses[0].contains("type=\"error\""));
    assert!(responses[0].contains("id=\"broken-iq\""));
    assert!(responses[0].contains("feature-not-implemented"));
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_iq_ignores_type_suffix_attributes() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq id="req-1" mimetype="result" type="get"><ping xmlns="urn:xmpp:ping"></iq"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(responses[0].contains("type=\"error\""));
    assert!(responses[0].contains("id=\"req-1\""));
    assert!(responses[0].contains("feature-not-implemented"));
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_iq_recovers_attrs_with_spaces_and_gt() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq note="1 > 0" id = "req-2" type = "get"><ping xmlns="urn:xmpp:ping"></iq"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(responses[0].contains("type=\"error\""));
    assert!(responses[0].contains("id=\"req-2\""));
    assert!(responses[0].contains("feature-not-implemented"));
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_iq_skips_unquoted_attr_and_keeps_scanning() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq bogus=x id="req-3" type="get"><ping xmlns="urn:xmpp:ping"></iq"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(responses[0].contains("type=\"error\""));
    assert!(responses[0].contains("id=\"req-3\""));
    assert!(responses[0].contains("feature-not-implemented"));
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_iq_skips_unquoted_attr_with_slashes() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq bogus=http://x id="req-4" type="get"><ping xmlns="urn:xmpp:ping"></iq"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(responses[0].contains("type=\"error\""));
    assert!(responses[0].contains("id=\"req-4\""));
    assert!(responses[0].contains("feature-not-implemented"));
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_iq_keeps_id_after_empty_attr_value() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq bogus= id="req-5" type="get"><ping xmlns="urn:xmpp:ping"></iq"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(responses[0].contains("type=\"error\""));
    assert!(responses[0].contains("id=\"req-5\""));
    assert!(responses[0].contains("feature-not-implemented"));
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_iq_recovers_unquoted_type_value() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq type=get id="req-6"><ping xmlns="urn:xmpp:ping"></iq"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(responses[0].contains("type=\"error\""));
    assert!(responses[0].contains("id=\"req-6\""));
    assert!(responses[0].contains("feature-not-implemented"));
}

#[tokio::test]
async fn handle_xmpp_frame_wrong_namespace_iq_stays_silent() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
            r#"<iq xmlns="urn:ietf:params:xml:ns:xmpp-sasl" id="bad-ns" type="get"><ping xmlns="urn:xmpp:ping"/></iq>"#,
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;

    assert!(responses.is_empty(), "expected no response: {responses:?}");
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_self_closing_iq_result_stays_silent() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq type=result/>"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert!(responses.is_empty(), "expected no response: {responses:?}");
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_iq_skips_url_like_attr_with_equals() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq bogus=http://x=y id="req-7" type="get"><ping xmlns="urn:xmpp:ping"></iq"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(responses[0].contains("type=\"error\""));
    assert!(responses[0].contains("id=\"req-7\""));
    assert!(responses[0].contains("feature-not-implemented"));
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_iq_does_not_shadow_real_type_attr() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq prev=type=result id="req-8" type="get"><ping xmlns="urn:xmpp:ping"></iq"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(responses[0].contains("type=\"error\""));
    assert!(responses[0].contains("id=\"req-8\""));
    assert!(responses[0].contains("feature-not-implemented"));
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_iq_ignores_embedded_quoted_type_in_broken_value() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq prev=type="result" id="req-9" type="get"><ping xmlns="urn:xmpp:ping"></iq"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(responses[0].contains("type=\"error\""));
    assert!(responses[0].contains("id=\"req-9\""));
    assert!(responses[0].contains("feature-not-implemented"));
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_iq_prefers_later_quoted_type_attr() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq type=result id="req-10" type="get"><ping xmlns="urn:xmpp:ping"></iq"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(responses[0].contains("type=\"error\""));
    assert!(responses[0].contains("id=\"req-10\""));
    assert!(responses[0].contains("feature-not-implemented"));
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_iq_prefers_later_unquoted_type_attr() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq type=result type=get id="req-10b"><ping xmlns="urn:xmpp:ping"></iq"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(responses[0].contains("type=\"error\""));
    assert!(responses[0].contains("id=\"req-10b\""));
    assert!(responses[0].contains("feature-not-implemented"));
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_iq_keeps_type_when_later_attr_is_truncated() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq type=get id="req-11" bogus="#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(responses[0].contains("type=\"error\""));
    assert!(responses[0].contains("id=\"req-11\""));
    assert!(responses[0].contains("feature-not-implemented"));
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_iq_keeps_unquoted_type_when_later_quote_is_unterminated() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq type=get id="req-12" to="alice@example.com"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(responses[0].contains("type=\"error\""));
    assert!(responses[0].contains("id=\"req-12\""));
    assert!(responses[0].contains("feature-not-implemented"));
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_iq_unescapes_recovered_id() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq id="a&amp;b" type="get"><ping xmlns="urn:xmpp:ping"></iq"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(responses[0].contains(r#"id="a&amp;b""#));
    assert!(!responses[0].contains(r#"id="a&amp;amp;b""#));
    assert!(responses[0].contains("feature-not-implemented"));
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_iq_recovers_later_unquoted_type_attr() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq bogus= type=get id="req-13"><ping xmlns="urn:xmpp:ping"></iq"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(responses[0].contains(r#"id="req-13""#));
    assert!(responses[0].contains("feature-not-implemented"));
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_iq_recovers_later_spaced_quoted_type_attr() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq bogus= type = "get" id="req-13b"><ping xmlns="urn:xmpp:ping"></iq"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(responses[0].contains(r#"id="req-13b""#));
    assert!(responses[0].contains("feature-not-implemented"));
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_iq_does_not_treat_next_id_attr_as_type_value() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq type= id="req-14"><ping xmlns="urn:xmpp:ping"></iq"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert!(responses.is_empty(), "expected no response: {responses:?}");
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_iq_does_not_treat_next_type_attr_as_id_value() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq id= type="get"><ping xmlns="urn:xmpp:ping"></iq"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(!responses[0].contains(r#"id="type=&quot;get&quot;""#));
    assert!(responses[0].contains("feature-not-implemented"));
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_iq_keeps_invalid_numeric_entity_escaped() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq id="&#1;" type="get"><ping xmlns="urn:xmpp:ping"></iq"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(responses[0].contains(r#"id="&amp;#1;""#));
    assert!(responses[0].contains("feature-not-implemented"));
}

#[tokio::test]
async fn handle_xmpp_frame_ping_roundtrips_through_sans_io_path() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
    conn.phase = ConnectionPhase::ready(jid, false);

    let responses = handle_xmpp_frame(
        r#"<iq id="ping-roundtrip" type="get"><ping xmlns="urn:xmpp:ping"/></iq>"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1);
    let element = Element::from_str(&responses[0]).expect("valid IQ XML");
    let iq = xmpp_parsers::iq::Iq::try_from(element).expect("parseable IQ");
    assert_eq!(iq.id, "ping-roundtrip");
    assert!(matches!(iq.payload, xmpp_parsers::iq::IqType::Result(None)));
}

#[tokio::test]
async fn handle_iq_carbons_enable_returns_parseable_result() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
    let frame = r#"<iq xmlns="jabber:client" id="carbons-1" type="set"><enable xmlns="urn:xmpp:carbons:2"/></iq>"#;
    let responses = handle_iq(
        frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;
    assert_eq!(responses.len(), 1);

    let iq_xml = responses.first().expect("carbons response");
    let element = Element::from_str(iq_xml).expect("valid IQ XML");
    let iq = xmpp_parsers::iq::Iq::try_from(element).expect("parseable IQ");

    assert_eq!(iq.id, "carbons-1");
    match iq.payload {
        xmpp_parsers::iq::IqType::Result(None) => {}
        other => panic!("expected empty IQ result, got {other:?}"),
    }
}

#[tokio::test]
async fn handle_iq_carbons_toggle_updates_registry_flag() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
    let (tx, _rx) = tokio::sync::mpsc::channel(8);
    state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), tx);
    assert!(!state
        .deps
        .protocol
        .connection_registry
        .is_carbons_enabled(&jid));

    let enable = r#"<iq xmlns="jabber:client" id="carbons-enable" type="set"><enable xmlns="urn:xmpp:carbons:2"/></iq>"#;
    let enable_responses = handle_iq(
        enable,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;
    assert_eq!(enable_responses.len(), 1);
    assert!(state
        .deps
        .protocol
        .connection_registry
        .is_carbons_enabled(&jid));

    let disable = r#"<iq xmlns="jabber:client" id="carbons-disable" type="set"><disable xmlns="urn:xmpp:carbons:2"/></iq>"#;
    let disable_responses = handle_iq(
        disable,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;
    assert_eq!(disable_responses.len(), 1);
    assert!(!state
        .deps
        .protocol
        .connection_registry
        .is_carbons_enabled(&jid));
}

#[tokio::test]
async fn handle_iq_unknown_includes_routing_addresses_in_error() {
    let state = create_test_websocket_state().await;
    let frame = r#"<iq xmlns="jabber:client" id="unknown-1" type="get" from="alice@example.com/web" to="example.com"><foo xmlns="urn:waddle:test:0"/></iq>"#;
    let responses = handle_iq(
        frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ConnectionPhase::Unauthenticated,
    )
    .await;
    assert_eq!(responses.len(), 1);

    let iq_xml = responses.first().expect("error response");
    let element = Element::from_str(iq_xml).expect("valid IQ XML");
    let iq = xmpp_parsers::iq::Iq::try_from(element).expect("parseable IQ");

    assert_eq!(iq.id, "unknown-1");
    assert_eq!(
        iq.from.as_ref().map(ToString::to_string).as_deref(),
        Some("example.com")
    );
    assert_eq!(
        iq.to.as_ref().map(ToString::to_string).as_deref(),
        Some("alice@example.com/web")
    );
    match iq.payload {
        xmpp_parsers::iq::IqType::Error(_) => {}
        other => panic!("expected IQ error payload, got {other:?}"),
    }
}

#[tokio::test]
async fn handle_iq_result_returns_empty_response() {
    let state = create_test_websocket_state().await;
    let frame = r#"<iq xmlns="jabber:client" id="ack-1" type="result" from="alice@example.com/web" to="muc.example.com"/>"#;
    let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");
    let responses = handle_iq(
        frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&sender_jid),
    )
    .await;
    assert!(
        responses.is_empty(),
        "IQ result should produce no response, got: {responses:?}"
    );
}

#[tokio::test]
async fn handle_iq_error_returns_empty_response() {
    let state = create_test_websocket_state().await;
    let frame = r#"<iq xmlns="jabber:client" id="err-1" type="error" from="alice@example.com/web" to="muc.example.com"><error type="cancel"><feature-not-implemented xmlns="urn:ietf:params:xml:ns:xmpp-stanzas"/></error></iq>"#;
    let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");
    let responses = handle_iq(
        frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&sender_jid),
    )
    .await;
    assert!(
        responses.is_empty(),
        "IQ error should produce no response, got: {responses:?}"
    );
}

#[tokio::test]
async fn handle_xmpp_frame_server_iq_error_returns_empty_response() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");
    conn.phase = ConnectionPhase::ready(sender_jid, false);

    let responses = handle_xmpp_frame(
            r#"<iq xmlns="jabber:client" from="waddle.social" id="016f8556-3f56-4a75-b159-ee0a1eb0823e" type="error"><error type="cancel"><feature-not-implemented xmlns="urn:ietf:params:xml:ns:xmpp-stanzas"/></error></iq>"#,
            "waddle.social",
            state.as_ref(),
            &mut conn,
        )
        .await;

    assert!(
        responses.is_empty(),
        "IQ error should produce no response, got: {responses:?}"
    );
}

#[tokio::test]
async fn handle_iq_command_request_routes_to_registry() {
    let state = create_test_websocket_state().await;
    state
        .deps
        .protocol
        .command_registry
        .register(
            "test:adhoc-command",
            "Test Command",
            |ctx: CommandContext| async move {
                CommandResult::Executing {
                    form: waddle_xmpp::xep::xep0004::DataForm::new(
                        waddle_xmpp::xep::xep0004::FormType::Form,
                    ),
                    session_id: ctx.command.session_id.unwrap_or_default(),
                    notes: vec![],
                }
            },
        )
        .await;

    let session = create_test_session(state.as_ref(), "alice").await;
    let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");
    let frame = r#"<iq xmlns="jabber:client" id="cmd-1" type="set" to="example.com"><command xmlns="http://jabber.org/protocol/commands" node="test:adhoc-command" action="execute"/></iq>"#;
    let responses = handle_iq(
        frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &ready_phase(&sender_jid),
    )
    .await;

    assert_eq!(responses.len(), 1);
    let response = responses.first().expect("command response");
    assert!(
        response.contains("status=\"executing\"") || response.contains("status='executing'"),
        "expected executing command response, got: {response}"
    );
    assert!(
        response.contains("sessionid=\"") || response.contains("sessionid='"),
        "expected command session ID in response, got: {response}"
    );
    assert!(
        !response.contains("feature-not-implemented"),
        "command IQ should not fall through to unhandled feature-not-implemented: {response}"
    );
}

#[tokio::test]
async fn handle_iq_command_request_requires_ready_phase() {
    let state = create_test_websocket_state().await;
    state
        .deps
        .protocol
        .command_registry
        .register(
            "test:adhoc-command",
            "Test Command",
            |_ctx: CommandContext| async move {
                CommandResult::Executing {
                    form: waddle_xmpp::xep::xep0004::DataForm::new(
                        waddle_xmpp::xep::xep0004::FormType::Form,
                    ),
                    session_id: String::new(),
                    notes: vec![],
                }
            },
        )
        .await;

    let session = create_test_session(state.as_ref(), "alice").await;
    let pending_jid: FullJid = "alice@example.com/pending".parse().expect("pending jid");
    let mut carbons_enabled = false;
    let mut roster_interested = false;
    let frame = r#"<iq xmlns="jabber:client" id="cmd-prebind-1" type="set" to="example.com"><command xmlns="http://jabber.org/protocol/commands" node="test:adhoc-command" action="execute"/></iq>"#;
    let mut conn_state = IqConnState {
        carbons_enabled: &mut carbons_enabled,
        roster_interested: &mut roster_interested,
        state_machine: None,
    };
    let responses = handle_iq_with_conn_state(
        parse_iq_for_test(frame),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &ConnectionPhase::authenticated(&pending_jid),
        &mut conn_state,
    )
    .await;

    let response = responses.first().expect("command error response");
    assert!(
        response.contains("not-authorized"),
        "pre-bind command IQ should be rejected: {response}"
    );
    assert!(
        !response.contains("status=\"executing\"") && !response.contains("status='executing'"),
        "pre-bind command IQ must not reach the registry: {response}"
    );
}

#[tokio::test]
async fn handle_iq_disco_info_advertises_replies() {
    let server_domain = "example.com";
    let muc_domain = "muc.example.com";
    let state = create_test_websocket_state().await;

    let server_query = disco_info_iq_frame("srv1", "example.com", None);
    let server_responses = handle_iq(
        &server_query,
        server_domain,
        muc_domain,
        state.as_ref(),
        &None,
        &ConnectionPhase::Unauthenticated,
    )
    .await;
    let server_response = server_responses.first().expect("server disco response");
    assert!(server_response.contains("urn:xmpp:reply:0"));
    assert!(!server_response.contains("urn:xmpp:spaces:0"));
    assert!(!server_response.contains("urn:xmpp:fulltext:0"));
    assert!(!server_response.contains("urn:waddle:test-extension:1"));

    let muc_query = disco_info_iq_frame("muc1", "muc.example.com", None);
    let muc_responses = handle_iq(
        &muc_query,
        server_domain,
        muc_domain,
        state.as_ref(),
        &None,
        &ConnectionPhase::Unauthenticated,
    )
    .await;
    let muc_response = muc_responses.first().expect("muc disco response");
    assert!(muc_response.contains("urn:xmpp:reply:0"));
    assert!(!muc_response.contains("urn:waddle:test-extension:1"));

    let room_query = disco_info_iq_frame("room1", "room@muc.example.com", None);
    let room_responses = handle_iq(
        &room_query,
        server_domain,
        muc_domain,
        state.as_ref(),
        &None,
        &ConnectionPhase::Unauthenticated,
    )
    .await;
    let room_response = room_responses.first().expect("room disco response");
    assert!(room_response.contains("urn:xmpp:mam:2"));
    assert!(room_response.contains("urn:xmpp:reply:0"));
    assert!(room_response.contains("urn:xmpp:fulltext:0"));
    assert!(!room_response.contains("urn:waddle:test-extension:1"));

    let user_jid: FullJid = "alice@example.com/waddle".parse().expect("user jid");
    let user_query = disco_info_iq_frame("user1", "alice@example.com", None);
    let user_responses = handle_iq(
        &user_query,
        server_domain,
        muc_domain,
        state.as_ref(),
        &None,
        &ready_phase(&user_jid),
    )
    .await;
    let user_response = user_responses.first().expect("user disco response");
    assert!(user_response.contains("urn:xmpp:mam:2"));
    assert!(user_response.contains("urn:xmpp:fulltext:0"));
}

#[tokio::test]
async fn handle_iq_cross_user_pep_disco_resolves_session_backed_accounts() {
    let state = create_test_websocket_state().await;
    let alice = create_test_session(state.as_ref(), "alice-session").await;
    let bob = create_test_session(state.as_ref(), "bob-session").await;
    let bob_jid: FullJid = format!("{}@example.com/phone", bob.xmpp_localpart)
        .parse()
        .expect("bob jid");

    let query = disco_info_iq_frame("session-pep", "alice-session@example.com", None);
    let responses = handle_iq(
        &query,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(bob),
        &ready_phase(&bob_jid),
    )
    .await;
    let response = responses.first().expect("session-backed PEP disco");

    assert!(
        response.contains("type=\"result\"") || response.contains("type='result'"),
        "session-backed user should expose PEP disco: {response}"
    );
    assert!(
        response.contains("http://jabber.org/protocol/pubsub#auto-create"),
        "expected PEP features for session-backed user: {response}"
    );
    assert!(
        !response.contains("urn:xmpp:mam:2"),
        "cross-user PEP disco must not expose personal MAM: {response}"
    );

    let missing_query = disco_info_iq_frame("session-pep-missing", "missing@example.com", None);
    let missing_responses = handle_iq(
        &missing_query,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(alice),
        &ready_phase(
            &"alice-session@example.com/phone"
                .parse()
                .expect("alice jid"),
        ),
    )
    .await;
    let missing_response = missing_responses
        .first()
        .expect("missing session-backed PEP disco");
    assert!(
        missing_response.contains("item-not-found"),
        "unknown local user should not expose PEP disco: {missing_response}"
    );
}

#[tokio::test]
async fn handle_iq_disco_items_server_advertises_spaces_service() {
    let state = create_test_websocket_state().await;
    let query = disco_items_iq_frame("srv-items", "example.com", None);

    let responses = handle_iq(
        &query,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ConnectionPhase::Unauthenticated,
    )
    .await;
    let response = responses.first().expect("server disco items response");

    assert!(
        response.contains("muc.example.com"),
        "expected MUC service: {response}"
    );
    assert!(
        response.contains("spaces.example.com"),
        "expected spaces service in server disco#items: {response}"
    );
}

#[tokio::test]
async fn handle_iq_disco_items_spaces_is_empty_without_owner_created_spaces() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "alice").await;

    let authenticated_session = Some(session);
    let authenticated_jid: FullJid = format!(
        "{}@example.com/web",
        authenticated_session
            .as_ref()
            .expect("session")
            .xmpp_localpart
    )
    .parse()
    .expect("authenticated jid");
    let authenticated_phase = ready_phase(&authenticated_jid);
    let query = disco_items_iq_frame("spaces-items", "spaces.example.com", None);

    let responses = handle_iq(
        &query,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &authenticated_session,
        &authenticated_phase,
    )
    .await;
    let response = responses.first().expect("spaces disco items response");

    assert!(
        !response.contains("node="),
        "fresh deployments must not advertise a synthetic space node: {response}"
    );
}

#[tokio::test]
async fn handle_iq_pubsub_items_spaces_node_lists_published_bookmarks() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "alice").await;

    let spaces_jid: BareJid = "spaces.example.com".parse().expect("spaces jid");
    state
        .deps
        .protocol
        .pubsub_storage
        .get_or_create_node(&spaces_jid, "team")
        .await
        .expect("space node");
    let channel = waddle_xmpp::ChannelInfo {
        id: "general".to_string(),
        name: "General".to_string(),
        channel_type: "text".to_string(),
    };
    let item =
        waddle_xmpp::xep::build_channel_item(&channel, "muc.example.com").expect("bookmark item");
    state
        .deps
        .protocol
        .pubsub_storage
        .publish_item(&spaces_jid, "team", &item, None, false)
        .await
        .expect("publish bookmark");

    let authenticated_session = Some(session);
    let authenticated_jid: FullJid = format!(
        "{}@example.com/web",
        authenticated_session
            .as_ref()
            .expect("session")
            .xmpp_localpart
    )
    .parse()
    .expect("authenticated jid");
    let authenticated_phase = ready_phase(&authenticated_jid);
    let query = r#"<iq xmlns="jabber:client" id="space-node-items" type="get" to="spaces.example.com"><pubsub xmlns="http://jabber.org/protocol/pubsub"><items node="team"/></pubsub></iq>"#;

    let responses = handle_iq(
        query,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &authenticated_session,
        &authenticated_phase,
    )
    .await;
    let response = responses
        .first()
        .expect("spaces node pubsub items response");

    assert!(
        response.contains("general@muc.example.com"),
        "expected channel room JID in spaces node pubsub items: {response}"
    );
    assert!(
        response.contains("conference") && response.contains("urn:xmpp:bookmarks:1"),
        "expected XEP-0402 conference item in spaces node pubsub items: {response}"
    );
    assert!(
        response.contains("General"),
        "expected channel name in spaces node pubsub items: {response}"
    );
}

#[tokio::test]
async fn standard_muc_owner_config_persists_room_and_enforces_nonanonymous_defaults() {
    let state = create_test_websocket_state().await;
    let session = create_test_server_owner_session(state.as_ref(), "alice").await;

    let room_jid: BareJid = "project@muc.example.com".parse().expect("room jid");
    let alice_jid: FullJid = format!("{}@example.com/web", session.xmpp_localpart)
        .parse()
        .expect("alice jid");
    let ready = ready_phase(&alice_jid);

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        &Some(session.clone()),
    )
    .await;

    let submit_form = Element::builder("x", waddle_xmpp::muc::DATA_FORMS_NS)
        .attr("type", "submit")
        .append(
            Element::builder("field", waddle_xmpp::muc::DATA_FORMS_NS)
                .attr("var", "muc#roomconfig_roomname")
                .append(
                    Element::builder("value", waddle_xmpp::muc::DATA_FORMS_NS)
                        .append("Project Room")
                        .build(),
                )
                .build(),
        )
        .append(
            Element::builder("field", waddle_xmpp::muc::DATA_FORMS_NS)
                .attr("var", "muc#roomconfig_persistentroom")
                .append(
                    Element::builder("value", waddle_xmpp::muc::DATA_FORMS_NS)
                        .append("0")
                        .build(),
                )
                .build(),
        )
        .append(
            Element::builder("field", waddle_xmpp::muc::DATA_FORMS_NS)
                .attr("var", "muc#roomconfig_whois")
                .append(
                    Element::builder("value", waddle_xmpp::muc::DATA_FORMS_NS)
                        .append("moderators")
                        .build(),
                )
                .build(),
        )
        .build();
    let owner_iq = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr("id", "owner-submit")
            .attr("type", "set")
            .attr("to", room_jid.to_string())
            .append(
                Element::builder("query", waddle_xmpp::muc::NS_MUC_OWNER)
                    .append(submit_form)
                    .build(),
            )
            .build(),
    );

    let responses = handle_iq(
        &owner_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &ready,
    )
    .await;
    assert_eq!(responses.len(), 1, "owner config response: {responses:?}");
    assert!(responses[0].contains("type=\"result\""));

    let actor = state.deps.app_state.db_pool.global_actor().clone();
    let channel = crate::server::xmpp_state::get_xmpp_channel(actor, "project")
        .await
        .expect("channel lookup")
        .expect("persisted channel");
    assert_eq!(channel.name, "Project Room");

    let snapshot = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor")
        .ask(GetSnapshot)
        .await
        .expect("snapshot")
        .room;
    assert!(snapshot.config.persistent);

    let disco = disco_items_iq_frame("muc-items", "muc.example.com", None);
    let responses = handle_iq(
        &disco,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ConnectionPhase::Unauthenticated,
    )
    .await;
    assert!(responses[0].contains("project@muc.example.com"));
}

#[tokio::test]
async fn standard_muc_owner_get_returns_config_without_persisting_room() {
    let state = create_test_websocket_state().await;
    let session = create_test_server_owner_session(state.as_ref(), "alice").await;

    let room_jid: BareJid = "config-get@muc.example.com".parse().expect("room jid");
    let alice_jid: FullJid = format!("{}@example.com/web", session.xmpp_localpart)
        .parse()
        .expect("alice jid");
    let ready = ready_phase(&alice_jid);

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        &Some(session.clone()),
    )
    .await;

    let owner_iq = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr("id", "owner-get")
            .attr("type", "get")
            .attr("to", room_jid.to_string())
            .append(Element::builder("query", waddle_xmpp::muc::NS_MUC_OWNER).build())
            .build(),
    );

    let responses = handle_iq(
        &owner_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &ready,
    )
    .await;
    assert_eq!(responses.len(), 1, "owner get response: {responses:?}");
    assert!(responses[0].contains("type=\"result\""));
    assert!(responses[0].contains("muc#roomconfig_roomname"));

    let actor = state.deps.app_state.db_pool.global_actor().clone();
    let channel = crate::server::xmpp_state::get_xmpp_channel(actor, "config-get")
        .await
        .expect("channel lookup");
    assert!(channel.is_none());
}

#[tokio::test]
async fn standard_muc_owner_config_rejects_non_owner() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let bob_session = create_test_session(state.as_ref(), "bob").await;

    let room_jid: BareJid = "locked@muc.example.com".parse().expect("room jid");
    let alice_jid: FullJid = format!("{}@example.com/web", alice_session.xmpp_localpart)
        .parse()
        .expect("alice jid");
    let bob_jid: FullJid = format!("{}@example.com/web", bob_session.xmpp_localpart)
        .parse()
        .expect("bob jid");
    let bob_ready = ready_phase(&bob_jid);

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        &Some(alice_session),
    )
    .await;

    let submit_form = Element::builder("x", waddle_xmpp::muc::DATA_FORMS_NS)
        .attr("type", "submit")
        .append(
            Element::builder("field", waddle_xmpp::muc::DATA_FORMS_NS)
                .attr("var", "muc#roomconfig_roomname")
                .append(
                    Element::builder("value", waddle_xmpp::muc::DATA_FORMS_NS)
                        .append("Hacked")
                        .build(),
                )
                .build(),
        )
        .build();
    let owner_iq = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr("id", "owner-submit")
            .attr("type", "set")
            .attr("to", room_jid.to_string())
            .append(
                Element::builder("query", waddle_xmpp::muc::NS_MUC_OWNER)
                    .append(submit_form)
                    .build(),
            )
            .build(),
    );

    let responses = handle_iq(
        &owner_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(bob_session.clone()),
        &bob_ready,
    )
    .await;
    assert_eq!(responses.len(), 1, "owner config response: {responses:?}");
    assert!(responses[0].contains("type=\"error\""));
    assert!(responses[0].contains("forbidden"));

    let snapshot = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor")
        .ask(GetSnapshot)
        .await
        .expect("snapshot")
        .room;
    assert_ne!(snapshot.config.name, "Hacked");

    let room_actor = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor");
    room_actor
        .ask(ChangeAffiliation {
            jid: bob_jid.to_bare(),
            affiliation: Affiliation::Admin,
        })
        .await
        .expect("set admin affiliation");

    let responses = handle_iq(
        &owner_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(bob_session),
        &bob_ready,
    )
    .await;
    assert_eq!(
        responses.len(),
        1,
        "admin owner config response: {responses:?}"
    );
    assert!(responses[0].contains("type=\"error\""));
    assert!(responses[0].contains("forbidden"));
}

#[tokio::test]
async fn room_disco_info_advertises_parent_space_metadata_for_linked_channel() {
    let state = create_test_websocket_state().await;
    let space_db = state.deps.app_state.db_pool.global();
    let conn = space_db.guard().await.expect("persistent connection");
    conn.execute(
            "INSERT INTO channels (id, name, description, channel_type, position, is_default) VALUES (?, ?, ?, 'text', 0, 0)",
            crate::db_params!["linked", "Linked", "Linked channel description"],
        )
        .await
        .expect("insert channel");
    drop(conn);
    let spaces_jid: BareJid = "spaces.example.com".parse().expect("spaces jid");
    state
        .deps
        .protocol
        .pubsub_storage
        .get_or_create_node(&spaces_jid, "team")
        .await
        .expect("space node");
    let channel = waddle_xmpp::ChannelInfo {
        id: "linked".to_string(),
        name: "Linked".to_string(),
        channel_type: "text".to_string(),
    };
    let item =
        waddle_xmpp::xep::build_channel_item(&channel, "muc.example.com").expect("bookmark item");
    state
        .deps
        .protocol
        .pubsub_storage
        .publish_item(&spaces_jid, "team", &item, None, false)
        .await
        .expect("publish bookmark");

    let query = disco_info_iq_frame("room-info", "linked@muc.example.com", None);
    let responses = handle_iq(
        &query,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ConnectionPhase::Unauthenticated,
    )
    .await;
    let response = responses.first().expect("room disco response");
    assert!(response.contains("muc_nonanonymous"));
    assert!(response.contains("urn:xmpp:spaces:0"));
    assert!(response.contains("var=\"parent\""));
    assert!(response.contains("xmpp:spaces.example.com?;node=team"));
    assert!(response.contains("http://jabber.org/protocol/muc#roominfo"));
    assert!(response.contains("muc#roomconfig_pubsub"));
    assert!(response.contains("muc#roominfo_description"));
    assert!(response.contains("Linked channel description"));
}

#[tokio::test]
async fn active_room_disco_preserves_managed_announcement_channel_type() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "alice").await;
    let conn = state
        .deps
        .app_state
        .db_pool
        .global()
        .guard()
        .await
        .expect("persistent connection");
    conn.execute(
            "INSERT INTO channels (id, name, description, channel_type, position, is_default) VALUES (?, ?, ?, 'announcement', 0, 0)",
            crate::db_params!["announcements", "Announcements", "Owner-posted announcements"],
        )
        .await
        .expect("insert announcement channel");
    drop(conn);

    let room_jid: BareJid = "announcements@muc.example.com".parse().expect("room jid");
    let alice_jid: FullJid = format!("{}@example.com/web", session.xmpp_localpart)
        .parse()
        .expect("alice jid");
    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        &Some(session.clone()),
    )
    .await;

    let query = disco_info_iq_frame("announcement-info", "announcements@muc.example.com", None);
    let responses = handle_iq(
        &query,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &ready_phase(&alice_jid),
    )
    .await;
    let response = responses.first().expect("room disco response");
    assert!(response.contains("muc_moderated"), "response: {response}");
    assert!(
        response.contains("waddle#channel_type"),
        "response: {response}"
    );
    assert!(response.contains("announcement"), "response: {response}");
    assert!(
        !response.contains("<value>text</value>"),
        "announcement room must not be reported as text: {response}"
    );
}

#[tokio::test]
async fn handle_iq_disco_info_spaces_node_reports_open_for_public_space() {
    let state = create_test_websocket_state().await;
    let viewer = create_test_session(state.as_ref(), "viewer").await;
    let spaces_jid: BareJid = "spaces.example.com".parse().expect("spaces jid");
    state
        .deps
        .protocol
        .pubsub_storage
        .get_or_create_node(&spaces_jid, "team")
        .await
        .expect("space node");

    let viewer_phase = authenticated_phase_for_session(&viewer, "example.com");
    let query = disco_info_iq_frame("space-node-info", "spaces.example.com", Some("team"));
    let responses = handle_iq(
        &query,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(viewer),
        &viewer_phase,
    )
    .await;
    let response = responses.first().expect("spaces node disco info response");

    assert!(
        response.contains("type=\"result\"") || response.contains("type='result'"),
        "expected successful node disco#info response: {response}"
    );
    assert!(
        response.contains("pubsub#access_model"),
        "expected access model metadata in node disco#info: {response}"
    );
    assert!(
        response.contains(">open<"),
        "expected public access model=open in metadata: {response}"
    );
}

#[tokio::test]
async fn handle_iq_disco_info_unknown_spaces_node_returns_item_not_found() {
    let state = create_test_websocket_state().await;
    let viewer = create_test_session(state.as_ref(), "viewer").await;

    let viewer_phase = authenticated_phase_for_session(&viewer, "example.com");
    let query = disco_info_iq_frame(
        "space-node-info-private",
        "spaces.example.com",
        Some("unknown"),
    );
    let responses = handle_iq(
        &query,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(viewer),
        &viewer_phase,
    )
    .await;
    let response = responses
        .first()
        .expect("spaces node private disco info response");

    assert!(
        response.contains("item-not-found"),
        "unknown space node should not be discoverable: {response}"
    );
}

#[tokio::test]
async fn handle_message_direct_rejects_client_authored_extension_envelope() {
    let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");
    let recipient_jid: FullJid = "bob@example.com/mobile".parse().expect("recipient jid");
    let state = create_test_websocket_state().await;

    let mut message =
        xmpp_parsers::message::Message::new(Some(jid::Jid::from(recipient_jid.clone())));
    message.id = Some("dm-extension-spoof-1".to_string());
    message.type_ = XmppMessageType::Chat;
    message.bodies.insert(
        String::new(),
        xmpp_parsers::message::Body("spoofed extension".to_string()),
    );
    message
        .payloads
        .push(Element::builder("extensions", "urn:waddle:extension:1").build());
    message
        .payloads
        .push(Element::builder("spoof", "urn:waddle:test-extension:1").build());

    let responses = handle_message_for_test(state.as_ref(), &sender_jid, None, message).await;

    assert_eq!(responses.len(), 1);
    assert!(
        responses[0].contains("bad-request"),
        "response was {}",
        responses[0]
    );
    assert!(
        !responses[0].contains("urn:waddle:extension:1"),
        "response was {}",
        responses[0]
    );
    assert!(
        !responses[0].contains("urn:waddle:test-extension:1"),
        "response was {}",
        responses[0]
    );
    assert!(
        responses[0].contains("from=\"bob@example.com/mobile\""),
        "response was {}",
        responses[0]
    );
    assert!(
        responses[0].contains("to=\"alice@example.com/web\""),
        "response was {}",
        responses[0]
    );
}

#[tokio::test]
async fn handle_message_error_with_extension_envelope_does_not_emit_error_loop() {
    let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");
    let recipient_jid: FullJid = "bob@example.com/mobile".parse().expect("recipient jid");
    let state = create_test_websocket_state().await;

    let mut message =
        xmpp_parsers::message::Message::new(Some(jid::Jid::from(recipient_jid.clone())));
    message.id = Some("dm-extension-error-1".to_string());
    message.type_ = XmppMessageType::Error;
    message
        .payloads
        .push(Element::builder("extensions", "urn:waddle:extension:1").build());

    let responses = handle_message_for_test(state.as_ref(), &sender_jid, None, message).await;

    assert!(
        responses.is_empty(),
        "message errors must not trigger another error: {responses:?}"
    );
}

#[tokio::test]
async fn handle_message_groupchat_rejects_client_authored_extension_envelope() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "alice").await;
    let sender_jid: FullJid = format!("{}@example.com/web", session.xmpp_localpart)
        .parse()
        .expect("sender jid");
    let room_jid: BareJid = "general@muc.example.com".parse().expect("room jid");
    let room_actor = get_or_create_room_actor(
        state.as_ref(),
        &room_jid,
        RoomConfig::default(),
        "space".to_string(),
        "general".to_string(),
    )
    .await
    .expect("create room");
    room_actor
        .ask(JoinWithAffiliation {
            sender_jid: sender_jid.clone(),
            nick: "alice".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
        })
        .await
        .expect("join alice");

    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(room_jid.clone())));
    message.id = Some("muc-extension-spoof-1".to_string());
    message.type_ = XmppMessageType::Groupchat;
    message.bodies.insert(
        String::new(),
        xmpp_parsers::message::Body("spoofed extension".to_string()),
    );
    message
        .payloads
        .push(Element::builder("extensions", "urn:waddle:extension:1").build());
    message
        .payloads
        .push(Element::builder("spoof", "urn:waddle:test-extension:1").build());

    let responses =
        handle_message_for_test(state.as_ref(), &sender_jid, Some(&session), message).await;

    assert_eq!(responses.len(), 1);
    assert!(
        responses[0].contains("bad-request"),
        "response was {}",
        responses[0]
    );
    assert!(
        !responses[0].contains("urn:waddle:extension:1"),
        "response was {}",
        responses[0]
    );
    assert!(
        !responses[0].contains("urn:waddle:test-extension:1"),
        "response was {}",
        responses[0]
    );
    assert!(
        responses[0].contains("from=\"general@muc.example.com\""),
        "response was {}",
        responses[0]
    );
    assert!(
        responses[0].contains("to=\"alice@example.com/web\""),
        "response was {}",
        responses[0]
    );
}

#[tokio::test]
async fn handle_message_groupchat_extension_envelope_preserves_non_occupant_error() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_session(state.as_ref(), "alice").await;
    let alice_jid: FullJid = format!("{}@example.com/web", alice_session.xmpp_localpart)
        .parse()
        .expect("alice jid");
    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let room_jid: BareJid = "general@muc.example.com".parse().expect("room jid");
    let room_actor = get_or_create_room_actor(
        state.as_ref(),
        &room_jid,
        RoomConfig::default(),
        "space".to_string(),
        "general".to_string(),
    )
    .await
    .expect("create room");
    room_actor
        .ask(JoinWithAffiliation {
            sender_jid: bob_jid,
            nick: "bob".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
        })
        .await
        .expect("join bob");

    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(room_jid.clone())));
    message.id = Some("muc-extension-non-occupant-1".to_string());
    message.type_ = XmppMessageType::Groupchat;
    message.bodies.insert(
        String::new(),
        xmpp_parsers::message::Body("spoofed extension".to_string()),
    );
    message
        .payloads
        .push(Element::builder("extensions", "urn:waddle:extension:1").build());
    message
        .payloads
        .push(Element::builder("spoof", "urn:waddle:test-extension:1").build());

    let responses =
        handle_message_for_test(state.as_ref(), &alice_jid, Some(&alice_session), message).await;

    assert_eq!(responses.len(), 1);
    assert!(
        responses[0].contains("not-acceptable"),
        "response was {}",
        responses[0]
    );
    assert!(
        !responses[0].contains("bad-request"),
        "response was {}",
        responses[0]
    );
    assert!(
        !responses[0].contains("urn:waddle:extension:1"),
        "response was {}",
        responses[0]
    );
    assert!(
        !responses[0].contains("urn:waddle:test-extension:1"),
        "response was {}",
        responses[0]
    );
    assert!(
        responses[0].contains("from=\"general@muc.example.com\""),
        "response was {}",
        responses[0]
    );
    assert!(
        responses[0].contains("to=\"alice@example.com/web\""),
        "response was {}",
        responses[0]
    );
}

#[tokio::test]
async fn handle_message_direct_with_sample_extension_payload_preserves_payload_for_recipient() {
    let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");
    let recipient_jid: FullJid = "bob@example.com/mobile".parse().expect("recipient jid");
    let state = create_test_websocket_state().await;

    let (recipient_tx, mut recipient_rx) = mpsc::channel(8);
    state
        .deps
        .protocol
        .connection_registry
        .register(recipient_jid.clone(), recipient_tx);

    let mut message =
        xmpp_parsers::message::Message::new(Some(jid::Jid::from(recipient_jid.clone())));
    message.id = Some("dm-extension-payload-1".to_string());
    message.type_ = XmppMessageType::Chat;
    message.bodies.insert(
        String::new(),
        xmpp_parsers::message::Body("Repo payload already attached".to_string()),
    );
    message.payloads.push(
        Element::builder("repo", "urn:waddle:test-extension:1")
            .attr("owner", "rust-lang")
            .attr("name", "rust")
            .attr("url", "xmpp:example.com?extension=test")
            .build(),
    );

    let responses = handle_message_for_test(state.as_ref(), &sender_jid, None, message).await;

    assert!(
        responses.is_empty(),
        "direct messages should not get a special extension echo"
    );

    // Recipient may receive an inbox push headline before the routed
    // chat message.  Drain until we find the chat stanza.
    let mut found_chat = false;
    while let Ok(routed) = recipient_rx.try_recv() {
        let routed_xml = stanza_to_xml(&routed.stanza);
        if routed_xml.contains("type=\"chat\"") || routed_xml.contains("type='chat'") {
            assert!(
                routed_xml.contains("to=\"bob@example.com/mobile\"")
                    || routed_xml.contains("to='bob@example.com/mobile'"),
                "routed stanza should target recipient resource: {routed_xml}"
            );
            assert!(
                routed_xml.contains("urn:waddle:test-extension:1"),
                "routed stanza should preserve extension payload: {routed_xml}"
            );
            assert_sample_payload(
                &routed_xml,
                "repo",
                "xmpp:example.com?extension=test",
                "rust-lang",
                "rust",
            );
            found_chat = true;
            break;
        }
    }
    assert!(found_chat, "recipient should receive the chat message");
}

#[tokio::test]
async fn handle_message_direct_chat_to_bare_jid_fans_out_to_all_connected_resources() {
    let state = create_test_websocket_state().await;
    let sender_jid: FullJid = "bob@example.com/phone".parse().expect("sender jid");
    let recipient_web: FullJid = "alice@example.com/web-123".parse().expect("recipient web");
    let recipient_mobile: FullJid = "alice@example.com/mobile-456"
        .parse()
        .expect("recipient mobile");

    let (web_tx, mut web_rx) = mpsc::channel(8);
    let (mobile_tx, mut mobile_rx) = mpsc::channel(8);
    state
        .deps
        .protocol
        .connection_registry
        .register(recipient_web.clone(), web_tx);
    state
        .deps
        .protocol
        .connection_registry
        .register(recipient_mobile.clone(), mobile_tx);
    // RFC 6121 §8.5.2.1: bare-JID delivery selects available
    // resources by presence priority. The dispatcher path enforces
    // this; mark both recipient resources available so fan-out
    // reaches them.
    state
        .deps
        .protocol
        .connection_registry
        .update_presence(&recipient_web, true, 0);
    state
        .deps
        .protocol
        .connection_registry
        .update_presence(&recipient_mobile, true, 0);

    let responses = handle_message_for_test(
        state.as_ref(),
        &sender_jid,
        None,
        parse_message_for_test(
            "<message xmlns='jabber:client' to='alice@example.com' type='chat' id='dm-bare-1'>\
                <body>hello all resources</body>\
             </message>",
        ),
    )
    .await;

    assert!(responses.is_empty(), "plain DM should not echo to sender");

    let mut web_chat = None;
    while let Ok(outbound) = web_rx.try_recv() {
        let xml = stanza_to_xml(&outbound.stanza);
        if xml.contains("hello all resources") && !xml.contains("urn:xmpp:carbons:2") {
            web_chat = Some(xml);
            break;
        }
    }

    let mut mobile_chat = None;
    while let Ok(outbound) = mobile_rx.try_recv() {
        let xml = stanza_to_xml(&outbound.stanza);
        if xml.contains("hello all resources") && !xml.contains("urn:xmpp:carbons:2") {
            mobile_chat = Some(xml);
            break;
        }
    }

    let _web_xml = web_chat.expect("web resource should receive original bare-JID message");
    let _mobile_xml =
        mobile_chat.expect("mobile resource should receive original bare-JID message");
    // RFC 6121 §8.5.2.1.1: bare-JID fan-out delivers the original
    // stanza to each available resource without rewriting the
    // `to` attribute. The dispatcher path preserves this; legacy
    // `handle_message` rewrote `to` to the per-resource full JID,
    // which was a deviation from the RFC. Both resources received
    // the stanza — the reachability semantic — and that is what
    // this unit test now asserts.
}

#[tokio::test]
async fn handle_message_direct_chat_sends_sent_carbon_to_opted_in_sibling_resource() {
    let state = create_test_websocket_state().await;
    let sender_jid: FullJid = "alice@example.com/phone".parse().expect("sender jid");
    let sibling_jid: FullJid = "alice@example.com/desktop".parse().expect("sibling jid");

    let (sender_tx, _sender_rx) = mpsc::channel(8);
    let (sibling_tx, mut sibling_rx) = mpsc::channel(8);
    state
        .deps
        .protocol
        .connection_registry
        .register(sender_jid.clone(), sender_tx);
    state
        .deps
        .protocol
        .connection_registry
        .register_with_carbons(sibling_jid.clone(), sibling_tx, true);

    let responses = handle_message_for_test(
        state.as_ref(),
        &sender_jid,
        None,
        parse_message_for_test(
            "<message xmlns='jabber:client' to='ghost@example.com' type='chat' id='sent-carbon-1'>\
                <body>sent carbon over websocket</body>\
             </message>",
        ),
    )
    .await;

    assert!(responses.is_empty(), "plain DM should not echo to sender");

    let mut sent_carbon = None;
    while let Ok(outbound) = sibling_rx.try_recv() {
        let xml = stanza_to_xml(&outbound.stanza);
        if xml.contains("urn:xmpp:carbons:2") && xml.contains("<sent") {
            sent_carbon = Some(xml);
            break;
        }
    }

    let carbon_xml = sent_carbon.expect("opted-in sibling should receive sent carbon");
    assert!(
        carbon_xml.contains("sent carbon over websocket"),
        "sent carbon should preserve message body: {carbon_xml}"
    );
}

#[tokio::test]
async fn handle_message_direct_chat_sends_received_carbon_to_opted_in_sibling_resource() {
    let state = create_test_websocket_state().await;
    let sender_jid: FullJid = "bob@example.com/phone".parse().expect("sender jid");
    let recipient_jid: FullJid = "alice@example.com/phone".parse().expect("recipient jid");
    let sibling_jid: FullJid = "alice@example.com/desktop".parse().expect("sibling jid");

    let (recipient_tx, mut recipient_rx) = mpsc::channel(8);
    let (sibling_tx, mut sibling_rx) = mpsc::channel(8);
    state
        .deps
        .protocol
        .connection_registry
        .register(recipient_jid.clone(), recipient_tx);
    state
        .deps
        .protocol
        .connection_registry
        .register_with_carbons(sibling_jid.clone(), sibling_tx, true);

    let frame = format!(
        "<message xmlns='jabber:client' to='{recipient_jid}' type='chat' id='recv-carbon-1'>\
                <body>received carbon over websocket</body>\
             </message>"
    );

    // Build alice/phone's per-connection state machine so we can
    // drive the recipient pass the dispatcher path now owns. In
    // production this happens automatically via alice/phone's
    // main loop dispatching the queued
    // `DeliveryKind::PeerStanza`; the unit test reproduces the
    // same step explicitly so we observe the recipient-side
    // received-carbon fan-out.
    let mut recipient_conn = WsConnState::new();
    recipient_conn.phase = ConnectionPhase::ready(recipient_jid.clone(), false);
    recipient_conn.ensure_state_machine(
        "example.com",
        &state.deps.protocol.dispatcher,
        recipient_jid.clone(),
        false,
        Blocklist::empty(),
    );

    let responses = handle_message_for_test(
        state.as_ref(),
        &sender_jid,
        None,
        parse_message_for_test(frame.as_str()),
    )
    .await;

    assert!(responses.is_empty(), "plain DM should not echo to sender");

    // Pump alice/phone's queued PeerStanza through her SM so the
    // recipient pass runs and emits SendCarbons (received) to
    // alice/desktop.
    while let Ok(outbound) = recipient_rx.try_recv() {
        if !matches!(outbound.kind, DeliveryKind::PeerStanza) {
            continue;
        }
        let sm = recipient_conn.state_machine.as_mut().expect("alice SM");
        let events = sm.handle(InboundEvent::StanzaFromPeer(Box::new(outbound.stanza)));
        let deps = build_interpret_deps(state.as_ref(), None);
        let _ = drive_interpret_loop(events, sm, &deps).await;
    }

    let mut received_carbon = None;
    while let Ok(outbound) = sibling_rx.try_recv() {
        let xml = stanza_to_xml(&outbound.stanza);
        if xml.contains("urn:xmpp:carbons:2") && xml.contains("<received") {
            received_carbon = Some(xml);
            break;
        }
    }

    let carbon_xml = received_carbon.expect("opted-in sibling should receive received carbon");
    assert!(
        carbon_xml.contains("received carbon over websocket"),
        "received carbon should preserve message body: {carbon_xml}"
    );
}

#[tokio::test]
async fn direct_messages_round_trip_through_inbox_query_and_mark_read() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_session(state.as_ref(), "alice").await;
    let bob_session = create_test_session(state.as_ref(), "bob").await;
    let alice_jid: FullJid = format!("{}@example.com/web", alice_session.xmpp_localpart)
        .parse()
        .expect("alice jid");
    let bob_jid: FullJid = format!("{}@example.com/mobile", bob_session.xmpp_localpart)
        .parse()
        .expect("bob jid");

    let message_xml = format!(
        "<message xmlns='jabber:client' to='{}' type='chat' id='dm-inbox-1'>\
                <body>Hello from Alice</body>\
             </message>",
        bob_jid.to_bare()
    );
    let responses = handle_message_for_test(
        state.as_ref(),
        &alice_jid,
        Some(&alice_session),
        parse_message_for_test(message_xml.as_str()),
    )
    .await;
    assert!(responses.is_empty(), "plain DM should not echo to sender");

    let inbox_query = format!(
        "<iq xmlns='jabber:client' type='get' to='{}' id='inbox-1'>\
                <query xmlns='urn:waddle:inbox:0'/>\
             </iq>",
        bob_jid.to_bare()
    );
    let inbox_responses = handle_iq(
        inbox_query.as_str(),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(bob_session.clone()),
        &ready_phase(&bob_jid),
    )
    .await;
    let inbox_xml = inbox_responses.first().expect("inbox response");
    assert!(
        inbox_xml.contains("partner=\"alice@example.com\""),
        "inbox response should include Alice conversation: {inbox_xml}"
    );
    assert!(
        inbox_xml.contains("unread=\"1\""),
        "inbox response should report one unread DM: {inbox_xml}"
    );
    assert!(
        inbox_xml.contains("<preview>Hello from Alice</preview>"),
        "inbox response should include preview text: {inbox_xml}"
    );

    let mark_read = format!(
        "<iq xmlns='jabber:client' type='set' to='{}' id='inbox-2'>\
                <mark-read xmlns='urn:waddle:inbox:0' partner='alice@example.com'/>\
             </iq>",
        bob_jid.to_bare()
    );
    let mark_read_responses = handle_iq(
        mark_read.as_str(),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(bob_session.clone()),
        &ready_phase(&bob_jid),
    )
    .await;
    let mark_read_xml = mark_read_responses.first().expect("mark-read result");
    assert!(
        mark_read_xml.contains("type=\"result\""),
        "mark-read should succeed: {mark_read_xml}"
    );

    let unread_only_query = format!(
        "<iq xmlns='jabber:client' type='get' to='{}' id='inbox-3'>\
                <query xmlns='urn:waddle:inbox:0' only-unread='true'/>\
             </iq>",
        bob_jid.to_bare()
    );
    let unread_only_responses = handle_iq(
        unread_only_query.as_str(),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(bob_session),
        &ready_phase(&bob_jid),
    )
    .await;
    let unread_only_xml = unread_only_responses.first().expect("unread-only response");
    assert!(
        unread_only_xml.contains("total-unread=\"0\""),
        "mark-read should clear the unread count: {unread_only_xml}"
    );
    assert!(
        !unread_only_xml.contains("<conversation "),
        "unread-only query should be empty after mark-read: {unread_only_xml}"
    );
}

#[tokio::test]
async fn inbox_query_requires_ready_phase() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "bob").await;
    let pending_jid: FullJid = "bob@example.com/pending".parse().expect("pending jid");
    let mut carbons_enabled = false;
    let mut roster_interested = false;
    let frame = r#"<iq xmlns='jabber:client' type='get' to='bob@example.com' id='inbox-prebind-1'><query xmlns='urn:waddle:inbox:0'/></iq>"#;
    let mut conn_state = IqConnState {
        carbons_enabled: &mut carbons_enabled,
        roster_interested: &mut roster_interested,
        state_machine: None,
    };
    let responses = handle_iq_with_conn_state(
        parse_iq_for_test(frame),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &ConnectionPhase::authenticated(&pending_jid),
        &mut conn_state,
    )
    .await;

    let response = responses.first().expect("inbox error response");
    assert!(
        response.contains("not-authorized"),
        "pre-bind inbox IQ should be rejected: {response}"
    );
}

#[tokio::test]
async fn encrypted_sfs_messages_without_bodies_still_project_into_inbox() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_session(state.as_ref(), "alice").await;
    let bob_session = create_test_session(state.as_ref(), "bob").await;
    let alice_jid: FullJid = format!("{}@example.com/web", alice_session.xmpp_localpart)
        .parse()
        .expect("alice jid");
    let bob_jid: FullJid = format!("{}@example.com/mobile", bob_session.xmpp_localpart)
        .parse()
        .expect("bob jid");

    let message_xml = format!(
        "<message xmlns='jabber:client' to='{}' type='chat' id='dm-esfs-1'>\
                <file-sharing xmlns='urn:xmpp:sfs:0'/>\
                <encrypted xmlns='urn:xmpp:esfs:0' cipher='urn:xmpp:ciphers:aes-256-gcm-nopadding:0'>\
                    <key>a2V5</key>\
                    <iv>aXY=</iv>\
                    <sources xmlns='urn:xmpp:sfs:0'>\
                        <url-data target='https://files.example.com/secret.enc'/>\
                    </sources>\
                </encrypted>\
             </message>",
        bob_jid.to_bare()
    );
    let responses = handle_message_for_test(
        state.as_ref(),
        &alice_jid,
        Some(&alice_session),
        parse_message_for_test(message_xml.as_str()),
    )
    .await;
    assert!(
        responses.is_empty(),
        "encrypted file-sharing DM should not echo to sender"
    );

    let inbox_query = format!(
        "<iq xmlns='jabber:client' type='get' to='{}' id='inbox-esfs-1'>\
                <query xmlns='urn:waddle:inbox:0'/>\
             </iq>",
        bob_jid.to_bare()
    );
    let inbox_responses = handle_iq(
        inbox_query.as_str(),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(bob_session),
        &ready_phase(&bob_jid),
    )
    .await;
    let inbox_xml = inbox_responses.first().expect("inbox response");
    assert!(
        inbox_xml.contains("partner=\"alice@example.com\""),
        "encrypted file-sharing inbox entry should target Alice: {inbox_xml}"
    );
    assert!(
        inbox_xml.contains("unread=\"1\""),
        "encrypted file-sharing inbox entry should increment unread: {inbox_xml}"
    );
    assert!(
        !inbox_xml.contains("<preview>"),
        "bodyless encrypted file-sharing message should not invent preview text: {inbox_xml}"
    );
}

#[tokio::test]
async fn groupchat_messages_are_archived_and_returned_via_mam() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "alice").await;
    let waddle_id = "waddle-alpha";
    let channel_id = "channel-bravo";
    let room_jid: BareJid = format!("{channel_id}@muc.example.com")
        .parse()
        .expect("room jid");
    let sender_jid: FullJid = format!("{}@example.com/web", session.xmpp_localpart)
        .parse()
        .expect("sender jid");
    // #229 PR18 cutover: groupchat reflections flow through the
    // connection registry as PeerStanza, so register the sender's
    // outbound channel before driving `handle_message`.
    let (sender_tx, mut sender_rx) = mpsc::channel(8);
    state
        .deps
        .protocol
        .connection_registry
        .register(sender_jid.clone(), sender_tx);
    let room_actor = get_or_create_room_actor(
        state.as_ref(),
        &room_jid,
        RoomConfig::default(),
        waddle_id.to_string(),
        channel_id.to_string(),
    )
    .await
    .expect("create room");
    room_actor
        .ask(JoinWithAffiliation {
            sender_jid: sender_jid.clone(),
            nick: "alice".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
        })
        .await
        .expect("join room");

    let message_xml = format!(
        "<message xmlns='jabber:client' to='{room_jid}' type='groupchat' id='client-msg-1'>\
                <body>Hello from WebSocket</body>\
             </message>"
    );
    let _message_responses = handle_message_for_test(
        state.as_ref(),
        &sender_jid,
        Some(&session),
        parse_message_for_test(message_xml.as_str()),
    )
    .await;
    let echo_stanza = sender_rx
        .try_recv()
        .expect("sender echo queued on outbound channel");
    let _echo_xml = stanza_to_xml(&echo_stanza.stanza);

    let mam_query = format!(
        "<iq xmlns='jabber:client' type='set' to='{room_jid}' id='mam-1'>\
                <query xmlns='urn:xmpp:mam:2' queryid='q1'>\
                    <x xmlns='jabber:x:data' type='submit'>\
                        <field var='FORM_TYPE' type='hidden'><value>urn:xmpp:mam:2</value></field>\
                    </x>\
                    <set xmlns='http://jabber.org/protocol/rsm'><max>50</max></set>\
                </query>\
             </iq>"
    );
    let mam_responses = handle_iq(
        mam_query.as_str(),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &ready_phase(&sender_jid),
    )
    .await;

    assert!(
        mam_responses
            .iter()
            .any(|stanza| stanza.contains("urn:xmpp:mam:2") && stanza.contains("<result")),
        "expected at least one MAM result stanza, got: {:?}",
        mam_responses
    );
    assert!(
        mam_responses
            .iter()
            .any(|stanza| stanza.contains("Hello from WebSocket")),
        "expected archived body in MAM replay, got: {:?}",
        mam_responses
    );
    assert!(
        mam_responses
            .iter()
            .any(|stanza| stanza.contains("<fin") && stanza.contains("urn:xmpp:mam:2")),
        "expected MAM fin stanza, got: {:?}",
        mam_responses
    );
}

#[tokio::test]
async fn personal_mam_query_uses_ready_phase_when_sidecar_session_is_missing() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_session(state.as_ref(), "alice").await;
    let bob_session = create_test_session(state.as_ref(), "bob").await;
    let alice_jid: FullJid = format!("{}@example.com/web", alice_session.xmpp_localpart)
        .parse()
        .expect("alice jid");
    let bob_jid: FullJid = format!("{}@example.com/mobile", bob_session.xmpp_localpart)
        .parse()
        .expect("bob jid");

    let message_xml = format!(
        "<message xmlns='jabber:client' to='{}' type='chat' id='dm-mam-1'>\
                <body>Hello from Alice</body>\
             </message>",
        bob_jid.to_bare()
    );
    let message_responses = handle_message_for_test(
        state.as_ref(),
        &alice_jid,
        Some(&alice_session),
        parse_message_for_test(message_xml.as_str()),
    )
    .await;
    assert!(
        message_responses.is_empty(),
        "plain DM should not echo to sender"
    );

    let mam_query = format!(
        "<iq xmlns='jabber:client' type='set' to='{}' id='mam-personal-1'>\
                <query xmlns='urn:xmpp:mam:2' queryid='q1'>\
                    <x xmlns='jabber:x:data' type='submit'>\
                        <field var='FORM_TYPE' type='hidden'><value>urn:xmpp:mam:2</value></field>\
                    </x>\
                    <set xmlns='http://jabber.org/protocol/rsm'><max>50</max></set>\
                </query>\
             </iq>",
        bob_jid.to_bare()
    );
    let mam_responses = handle_iq(
        mam_query.as_str(),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&bob_jid),
    )
    .await;

    assert!(
        mam_responses
            .iter()
            .any(|stanza| stanza.contains("urn:xmpp:mam:2") && stanza.contains("<result")),
        "expected personal MAM result stanza for resumed ready phase, got: {:?}",
        mam_responses
    );
    assert!(
        mam_responses
            .iter()
            .any(|stanza| stanza.contains("Hello from Alice")),
        "expected archived DM in personal MAM replay, got: {:?}",
        mam_responses
    );
    assert!(
        mam_responses
            .iter()
            .any(|stanza| stanza.contains(&format!("to=\"{}\"", bob_jid))
                || stanza.contains(&format!("to='{}'", bob_jid))),
        "expected MAM results addressed to the resumed bound resource, got: {:?}",
        mam_responses
    );
    assert!(
        mam_responses
            .iter()
            .all(|stanza| !stanza.contains("unknown@localhost")),
        "resumed personal MAM must not fall back to unknown recipient: {:?}",
        mam_responses
    );
}

#[tokio::test]
async fn upload_slot_request_requires_ready_phase() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "alice").await;
    let pending_phase = authenticated_phase_for_session(&session, "example.com");
    let frame = r#"<iq xmlns='jabber:client' type='get' to='upload.example.com' id='upload-prebind-1'><request xmlns='urn:xmpp:http:upload:0' filename='hello.txt' size='5' content-type='text/plain'/></iq>"#;

    let responses = handle_iq(
        frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &pending_phase,
    )
    .await;

    let response = responses.first().expect("upload error response");
    assert!(
        response.contains("not-authorized"),
        "pre-bind upload request should be rejected: {response}"
    );
}

#[test]
fn stanza_to_xml_includes_payloads() {
    let mut msg = xmpp_parsers::message::Message::new(Some(jid::Jid::from(
        "bob@example.com".parse::<jid::BareJid>().unwrap(),
    )));
    msg.from = Some(jid::Jid::from(
        "alice@example.com".parse::<jid::BareJid>().unwrap(),
    ));
    msg.id = Some("test-1".into());
    msg.type_ = xmpp_parsers::message::MessageType::Chat;
    msg.bodies
        .insert(String::new(), xmpp_parsers::message::Body("Hello".into()));

    let embed = xmpp_parsers::minidom::Element::builder("repo", "urn:waddle:test-extension:1")
        .attr("owner", "cuenv")
        .attr("name", "cuenv")
        .build();
    msg.payloads.push(embed);

    let xml = stanza_to_xml(&Stanza::Message(msg));

    assert!(xml.contains("<body>Hello</body>"), "body must be present");
    assert!(
        xml.contains("urn:waddle:test-extension:1"),
        "payload namespace must be serialized: {xml}"
    );
    assert!(
        xml.contains("cuenv"),
        "payload attributes must be serialized: {xml}"
    );
    // Must not contain XML declaration inside the message
    assert!(
        !xml.contains("<?xml"),
        "payload must not include XML declaration: {xml}"
    );
    // The whole thing must be a single <message>...</message>
    assert!(
        xml.starts_with("<message"),
        "must start with <message: {xml}"
    );
    assert!(
        xml.ends_with("</message>"),
        "must end with </message>: {xml}"
    );
}

#[test]
fn stanza_to_xml_no_payloads_still_works() {
    let mut msg = xmpp_parsers::message::Message::new(Some(jid::Jid::from(
        "bob@example.com".parse::<jid::BareJid>().unwrap(),
    )));
    msg.from = Some(jid::Jid::from(
        "alice@example.com".parse::<jid::BareJid>().unwrap(),
    ));
    msg.id = Some("test-2".into());
    msg.type_ = xmpp_parsers::message::MessageType::Chat;
    msg.bodies.insert(
        String::new(),
        xmpp_parsers::message::Body("No embeds".into()),
    );

    let xml = stanza_to_xml(&Stanza::Message(msg));
    assert!(xml.contains("<body>No embeds</body>"));
    assert!(xml.ends_with("</message>"));
}

#[test]
fn parse_message_stanza_preserves_thread_and_reply() {
    let xml = r#"<message to="room@muc.localhost" type="groupchat" id="msg-1">
            <body>Hello</body>
            <thread>thread-root</thread>
            <reply xmlns="urn:xmpp:reply:0" id="parent-msg" to="alice@localhost"/>
        </message>"#;

    let parsed = parse_message_for_test(xml);
    assert_eq!(parsed.id.as_deref(), Some("msg-1"));
    assert_eq!(
        parsed.thread.as_ref().map(|t| t.0.as_str()),
        Some("thread-root")
    );
    assert!(parsed
        .payloads
        .iter()
        .any(|p| p.name() == "reply" && p.ns() == "urn:xmpp:reply:0"));
}

#[test]
fn stanza_to_xml_preserves_thread_and_reply() {
    let xml = r#"<message to="room@muc.localhost" type="groupchat" id="msg-2">
            <body>Follow-up</body>
            <thread>thread-root</thread>
            <reply xmlns="urn:xmpp:reply:0" id="msg-1" to="alice@localhost"/>
        </message>"#;

    let parsed = parse_message_for_test(xml);
    let rendered = stanza_to_xml(&Stanza::Message(parsed));
    let reparsed = parse_message_for_test(&rendered);
    assert_eq!(
        reparsed.thread.as_ref().map(|thread| thread.0.as_str()),
        Some("thread-root"),
        "rendered stanza: {rendered}"
    );
    assert!(reparsed.payloads.iter().any(|payload| {
        payload.name() == "reply"
            && payload.ns() == "urn:xmpp:reply:0"
            && payload.attr("id") == Some("msg-1")
            && payload.attr("to") == Some("alice@localhost")
    }));
}

#[test]
fn xep_0201_thread_reattach_ignores_unrelated_namespaced_thread_payload() {
    // RFC 6121 §5.2.5 scopes `<thread/>` to the enclosing message's
    // namespace. If a stanza already contains a `<thread>` payload
    // in some other namespace (an unrelated extension), the
    // reattach branch must NOT see it as a conflict and skip
    // serializing the typed `message.thread` field — that would
    // drop the actual conversation thread on the wire (Copilot
    // review on PR #305).
    use xmpp_parsers::message::{Body, Message, MessageType, Thread};
    use xmpp_parsers::minidom::Element;

    let mut msg = Message::new(Some(jid::Jid::from(
        "bob@example.com".parse::<jid::BareJid>().expect("jid"),
    )));
    msg.from = Some(jid::Jid::from(
        "alice@example.com/web"
            .parse::<jid::FullJid>()
            .expect("jid"),
    ));
    msg.id = Some("msg-ns".to_string());
    msg.type_ = MessageType::Chat;
    msg.bodies.insert(String::new(), Body("hi".to_string()));
    msg.thread = Some(Thread("conversation-thread".to_string()));
    // Unrelated extension element happening to be named "thread"
    // in a different namespace — must not suppress reattachment.
    msg.payloads.push(
        Element::builder("thread", "urn:example:other:0")
            .attr("kind", "unrelated")
            .build(),
    );

    let rendered = stanza_to_xml(&Stanza::Message(msg));
    let reparsed = parse_message_for_test(&rendered);
    assert_eq!(
        reparsed.thread.as_ref().map(|t| t.0.as_str()),
        Some("conversation-thread"),
        "RFC 6121 thread must survive serialization despite unrelated <thread> in another ns; rendered: {rendered}"
    );
}

#[tokio::test]
async fn handle_iq_pubsub_publish_returns_result() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
    let frame = r#"<iq xmlns="jabber:client" id="pub-1" type="set"><pubsub xmlns="http://jabber.org/protocol/pubsub"><publish node="http://jabber.org/protocol/mood"><item id="current"><mood xmlns="http://jabber.org/protocol/mood"><happy/></mood></item></publish></pubsub></iq>"#;
    let responses = handle_iq(
        frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    let element = Element::from_str(&responses[0]).expect("valid XML");
    let iq = xmpp_parsers::iq::Iq::try_from(element).expect("parseable IQ");
    assert_eq!(iq.id, "pub-1");
    match iq.payload {
        xmpp_parsers::iq::IqType::Result(Some(payload)) => {
            assert_eq!(payload.ns(), "http://jabber.org/protocol/pubsub");
        }
        other => panic!("expected pubsub result, got {other:?}"),
    }
}

#[tokio::test]
async fn handle_iq_pubsub_items_empty_node_returns_result() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
    let frame = r#"<iq xmlns="jabber:client" id="items-1" type="get"><pubsub xmlns="http://jabber.org/protocol/pubsub"><items node="http://jabber.org/protocol/mood"/></pubsub></iq>"#;
    let responses = handle_iq(
        frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    let element = Element::from_str(&responses[0]).expect("valid XML");
    let iq = xmpp_parsers::iq::Iq::try_from(element).expect("parseable IQ");
    assert_eq!(iq.id, "items-1");
}

// ---- B: Non-blocking broadcast ------------------------------------

#[tokio::test]
async fn try_send_to_returns_dropped_full_on_backpressured_channel() {
    // A size-1 channel lets us prove try_send does not block when the
    // receiver isn't draining: the second send must report DroppedFull
    // immediately instead of awaiting capacity. Callers rely on this
    // variant to count silent drops for observability.
    let registry = ConnectionRegistry::new();
    let jid: FullJid = "user@example.com/res".parse().expect("jid");
    let (tx, _rx) = mpsc::channel::<OutboundStanza>(1);
    registry.register(jid.clone(), tx);

    let stanza_a = Stanza::Presence(xmpp_parsers::presence::Presence::new(
        xmpp_parsers::presence::Type::None,
    ));
    let stanza_b = Stanza::Presence(xmpp_parsers::presence::Presence::new(
        xmpp_parsers::presence::Type::None,
    ));

    assert_eq!(
        registry.try_send_to(&jid, stanza_a),
        BroadcastOutcome::Delivered
    );
    assert_eq!(
        registry.try_send_to(&jid, stanza_b),
        BroadcastOutcome::DroppedFull
    );
}

#[tokio::test]
async fn try_send_to_returns_dropped_closed_and_unregisters() {
    let registry = ConnectionRegistry::new();
    let jid: FullJid = "gone@example.com/res".parse().expect("jid");
    let (tx, rx) = mpsc::channel::<OutboundStanza>(4);
    registry.register(jid.clone(), tx);
    drop(rx); // close the channel so try_send sees Closed

    let stanza = Stanza::Presence(xmpp_parsers::presence::Presence::new(
        xmpp_parsers::presence::Type::None,
    ));
    assert_eq!(
        registry.try_send_to(&jid, stanza),
        BroadcastOutcome::DroppedClosed
    );
    assert!(!registry.is_connected(&jid));
}

#[tokio::test]
async fn try_send_to_returns_not_connected_when_unregistered() {
    let registry = ConnectionRegistry::new();
    let jid: FullJid = "nobody@example.com/res".parse().expect("jid");
    let stanza = Stanza::Presence(xmpp_parsers::presence::Presence::new(
        xmpp_parsers::presence::Type::None,
    ));
    assert_eq!(
        registry.try_send_to(&jid, stanza),
        BroadcastOutcome::NotConnected
    );
}

#[tokio::test]
async fn try_send_to_does_not_unregister_replacement_entry() {
    // Simulate the replacement race: connection A is registered, its
    // receiver is dropped so the sender is closed, connection B takes
    // over the same JID with a live sender, then something (e.g. a MUC
    // broadcast task that still holds a clone of A's sender) tries to
    // send. The try_send would see Closed on A's cloned sender — but
    // the entry in the registry is now B's live one and must NOT be
    // evicted.
    let registry = ConnectionRegistry::new();
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");

    let (tx_a, rx_a) = mpsc::channel::<OutboundStanza>(4);
    registry.register(jid.clone(), tx_a);
    drop(rx_a); // A's sender is now closed

    // B takes over. The register() call replaces A's entry; only B's
    // (live) sender is now in the registry.
    let (tx_b, _rx_b) = mpsc::channel::<OutboundStanza>(4);
    registry.register(jid.clone(), tx_b);

    // A broadcast path now tries to send. From its perspective, it sees
    // whatever sender is currently in the registry — which is B's live
    // one — so try_send_to returns Delivered. Either way, the entry
    // must remain in the registry.
    let stanza = Stanza::Presence(xmpp_parsers::presence::Presence::new(
        xmpp_parsers::presence::Type::None,
    ));
    let _outcome = registry.try_send_to(&jid, stanza);
    assert!(
        registry.is_connected(&jid),
        "replacement entry must still be registered after a try_send_to that races with eviction"
    );
}

// ---- C: MUC nick handling -----------------------------------------

#[tokio::test]
async fn muc_self_rejoin_does_not_emit_ghost_presence() {
    // Same user joins the same nick twice from different resources —
    // the second join must NOT include a presence for the old resource
    // in the response (which used to be seen as a "ghost" occupant and
    // broke self-presence detection on the client).
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "rejoin-channel@muc.example.com".parse().expect("room");
    let first: FullJid = "alice@example.com/tab-1".parse().expect("first");
    let second: FullJid = "alice@example.com/tab-2".parse().expect("second");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &first,
        "alice",
        &Some(owner_session),
    )
    .await;
    let responses = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &second,
        "alice",
        &None,
    )
    .await;

    // Count presences emitted to the joiner that came from room/alice.
    // Only the self-presence (status 110) should be there — no "ghost".
    let alice_presences_for_joiner = responses
        .iter()
        .filter_map(|xml| Element::from_str(xml).ok())
        .filter(|el| el.name() == "presence")
        .filter(|el| el.attr("from") == Some(&format!("{room_jid}/alice")))
        .filter(|el| el.attr("to") == Some(&second.to_string()))
        .count();
    assert_eq!(
        alice_presences_for_joiner, 1,
        "self-rejoin must produce exactly one self-presence, not a ghost + self pair"
    );

    // And the one presence we got must carry status 110.
    let self_presence = responses
        .iter()
        .filter_map(|xml| Element::from_str(xml).ok())
        .find(|el| {
            el.name() == "presence"
                && el.attr("from") == Some(&format!("{room_jid}/alice"))
                && el.attr("to") == Some(&second.to_string())
        })
        .expect("self-presence must be present");
    let user_x = self_presence
        .get_child("x", "http://jabber.org/protocol/muc#user")
        .expect("muc user payload");
    assert!(
        user_x
            .children()
            .any(|child| child.name() == "status" && child.attr("code") == Some("110")),
        "status 110 must be present on self-rejoin"
    );
}

#[tokio::test]
async fn muc_join_broadcast_includes_real_occupant_jid() {
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "public-channel@muc.example.com".parse().expect("room");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice");
    let bob: FullJid = "bob@example.com/phone".parse().expect("bob");

    let (alice_tx, mut alice_rx) = mpsc::channel::<OutboundStanza>(4);
    state
        .deps
        .protocol
        .connection_registry
        .register(alice.clone(), alice_tx);

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        &Some(owner_session),
    )
    .await;
    let _ = handle_muc_join(state.as_ref(), "example.com", &room_jid, &bob, "bob", &None).await;

    let broadcast = alice_rx.try_recv().expect("bob join broadcast to alice");
    let broadcast_xml = stanza_to_xml(&broadcast.stanza);
    let presence = Element::from_str(&broadcast_xml).expect("broadcast presence XML");
    let user_x = presence
        .get_child("x", "http://jabber.org/protocol/muc#user")
        .expect("muc user payload");
    let item = user_x
        .get_child("item", "http://jabber.org/protocol/muc#user")
        .expect("muc user item");

    let expected_from = format!("{room_jid}/bob");
    let expected_to = alice.to_string();
    assert_eq!(presence.attr("from"), Some(expected_from.as_str()));
    assert_eq!(presence.attr("to"), Some(expected_to.as_str()));
    assert_eq!(item.attr("jid"), Some("bob@example.com/phone"));
    assert_eq!(item.attr("affiliation"), Some("member"));
    assert_eq!(item.attr("role"), Some("participant"));
    assert!(
        user_x
            .children()
            .any(|child| child.name() == "status" && child.attr("code") == Some("100")),
        "non-anonymous room presence must advertise status 100: {broadcast_xml}"
    );
}

#[tokio::test]
async fn muc_nick_collision_returns_conflict_presence() {
    // Two different users try to hold the same nick — second gets a
    // <presence type='error'/> with <conflict/>, and room state for
    // the incumbent is untouched.
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "conflict-channel@muc.example.com".parse().expect("room");
    let alice: FullJid = "alice@example.com/desktop".parse().expect("alice");
    let bob: FullJid = "bob@example.com/phone".parse().expect("bob");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "dino",
        &Some(owner_session),
    )
    .await;
    let responses = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob,
        "dino",
        &None,
    )
    .await;

    assert_eq!(responses.len(), 1, "exactly one error presence");
    let el = Element::from_str(&responses[0]).expect("valid XML");
    assert_eq!(el.name(), "presence");
    assert_eq!(el.attr("type"), Some("error"));
    let bob_str = bob.to_string();
    assert_eq!(el.attr("to"), Some(bob_str.as_str()));
    let err = el
        .get_child("error", waddle_xmpp::ns::JABBER_CLIENT)
        .expect("error element");
    assert_eq!(err.attr("type"), Some("cancel"));
    assert!(err
        .get_child("conflict", "urn:ietf:params:xml:ns:xmpp-stanzas")
        .is_some());

    // Alice still owns the nick.
    let room = snapshot_room(state.as_ref(), &room_jid).await.room;
    assert_eq!(room.find_nick_by_real_jid(&alice), Some("dino"));
    assert!(room.find_nick_by_real_jid(&bob).is_none());
    assert_eq!(room.occupant_count(), 1);
}

// ---- D: stream feature advertisement --------------------------------

#[tokio::test]
async fn xep0237_features_advertise_roster_versioning() {
    let features = build_stream_features_xml(true);
    let el = Element::from_str(&features).expect("features xml");
    assert!(
        el.children()
            .any(|child| { child.name() == "ver" && child.ns() == waddle_xmpp::ns::ROSTERVER }),
        "post-auth features must advertise urn:xmpp:features:rosterver"
    );
}

#[tokio::test]
async fn rfc6121_features_advertise_subscription_preapproval() {
    let features = build_stream_features_xml(true);
    let el = Element::from_str(&features).expect("features xml");
    assert!(
        el.children().any(|child| {
            child.name() == "sub" && child.ns() == "urn:xmpp:features:pre-approval"
        }),
        "post-auth features must advertise RFC 6121 subscription pre-approval"
    );
}
