//! Test utilities for XMPP interoperability testing.
//!
//! Provides helpers for starting test servers, generating TLS certificates,
//! and simulating XMPP client connections.
//!
//! Integration tests each compile as a standalone crate that re-includes this
//! module, so helpers consumed by only some tests appear unused to the rest.
//! Silence `dead_code` at module scope rather than fragmenting the shared
//! helpers.

#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::io::{BufReader, Cursor};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use base64::prelude::*;
use jid::Jid;
use rcgen::{generate_simple_self_signed, CertifiedKey};
use rustls::pki_types::PrivateKeyDer;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{oneshot, Mutex};
use tokio::time::timeout;
use tokio_rustls::{
    rustls::{ClientConfig, RootCertStore, ServerConfig},
    TlsAcceptor, TlsConnector,
};
use waddle_xmpp::inbox::{
    storage::{InMemoryInboxStorage, InboxStorage},
    InboxEntry,
};
use waddle_xmpp::{roster, AppState, ScramCredentials, Session, XmppError};

/// Install the ring crypto provider for rustls.
/// Must be called once before any TLS operations.
pub fn install_crypto_provider() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        rustls::crypto::ring::default_provider()
            .install_default()
            .expect("Failed to install crypto provider");
        touch_common_symbols();
    });
}

fn touch_common_symbols() {
    let _ = DEFAULT_TIMEOUT;

    let _ = MockAppState::new("localhost");
    let _ = MockAppState::rejecting("localhost");

    let _ = TestServer::start;
    let _ = TestServer::start_with_state::<MockAppState>;
    let _ = TestServer::tls_connector as fn(&TestServer) -> TlsConnector;
    let _ = run_test_server::<MockAppState>;

    let dummy_server = TestServer {
        addr: "127.0.0.1:5222".parse().expect("valid socket address"),
        domain: "localhost".to_string(),
        tls_credentials: TestTlsCredentials {
            cert_pem: Vec::new(),
            key_pem: Vec::new(),
        },
        shutdown_tx: None,
    };
    let _ = dummy_server.addr;
    let _ = dummy_server.domain.as_str();

    let _ = RawXmppClient::connect;
    let _ = RawXmppClient::send;
    let _ = RawXmppClient::read;
    let _ = RawXmppClient::read_until;
    let _ = RawXmppClient::clear as fn(&mut RawXmppClient);
    let _ = RawXmppClient::take_buffer as fn(&mut RawXmppClient) -> String;
    let _ = RawXmppClient::upgrade_tls;
    let _ = RawXmppClient::is_tls as fn(&RawXmppClient) -> bool;

    let _ = validate_stream_header as fn(&str) -> Result<(), String>;
    let _ = extract_bound_jid as fn(&str) -> Option<String>;
    let _ = init_test_env as fn();
    let _ = establish_bound_session;
    let _ = disco_info_query;
    let _ = join_muc_room;
    let _ = ping_query;
}

/// Default timeout for test operations.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// Test application state that accepts any authentication.
pub struct MockAppState {
    pub domain: String,
    /// Whether to accept all auth attempts (true) or reject them (false)
    pub accept_auth: bool,
    /// XEP-0503: waddle details for spaces service tests
    pub waddle_details: Vec<waddle_xmpp::WaddleDetails>,
    /// XEP-0503: channels per waddle (keyed by waddle ID)
    pub waddle_channels: std::collections::HashMap<String, Vec<waddle_xmpp::ChannelInfo>>,
    blocked_jids: Mutex<HashMap<String, HashSet<String>>>,
    known_users: Mutex<HashSet<String>>,
    inbox_storage: InMemoryInboxStorage,
}

impl MockAppState {
    pub fn new(domain: &str) -> Self {
        Self {
            domain: domain.to_string(),
            accept_auth: true,
            waddle_details: Vec::new(),
            waddle_channels: std::collections::HashMap::new(),
            blocked_jids: Mutex::new(HashMap::new()),
            known_users: Mutex::new(HashSet::new()),
            inbox_storage: InMemoryInboxStorage::new(),
        }
    }

    pub fn rejecting(domain: &str) -> Self {
        Self {
            domain: domain.to_string(),
            accept_auth: false,
            waddle_details: Vec::new(),
            waddle_channels: std::collections::HashMap::new(),
            blocked_jids: Mutex::new(HashMap::new()),
            known_users: Mutex::new(HashSet::new()),
            inbox_storage: InMemoryInboxStorage::new(),
        }
    }

    /// Add a waddle with its channels for XEP-0503 tests.
    pub fn with_waddle(
        mut self,
        details: waddle_xmpp::WaddleDetails,
        channels: Vec<waddle_xmpp::ChannelInfo>,
    ) -> Self {
        let waddle_id = details.id.clone();
        self.waddle_details.push(details);
        self.waddle_channels.insert(waddle_id, channels);
        self
    }
}

impl AppState for MockAppState {
    async fn validate_session(&self, jid: &Jid, _token: &str) -> Result<Session, XmppError> {
        let accept = self.accept_auth;
        let jid = jid.clone();
        if accept {
            if let Some(node) = jid.node() {
                self.known_users.lock().await.insert(node.to_string());
            }
            Ok(Session {
                user_id: format!(
                    "user-test-{}",
                    jid.node().map(|n| n.to_string()).unwrap_or_default()
                ),
                jid: jid.to_bare(),
                created_at: chrono::Utc::now(),
                expires_at: chrono::Utc::now() + chrono::Duration::hours(24),
            })
        } else {
            Err(XmppError::auth_failed("Mock auth rejection"))
        }
    }

    async fn check_permission(
        &self,
        _resource: &str,
        _action: &str,
        _subject: &str,
    ) -> Result<bool, XmppError> {
        Ok(true)
    }

    async fn validate_session_token(&self, token: &str) -> Result<Session, XmppError> {
        let accept = self.accept_auth;
        let domain = self.domain.clone();
        let token = token.to_string();
        if accept {
            let username = format!("user_{}", &token[..token.len().min(8)]);
            self.known_users.lock().await.insert(username.clone());
            // Mock: derive a JID from the token
            let mock_jid = format!("{}@{}", username, domain);
            Ok(Session {
                user_id: format!("user-mock-{}", &token[..token.len().min(8)]),
                jid: mock_jid
                    .parse()
                    .unwrap_or_else(|_| "fallback@test.local".parse().unwrap()),
                created_at: chrono::Utc::now(),
                expires_at: chrono::Utc::now() + chrono::Duration::hours(24),
            })
        } else {
            Err(XmppError::auth_failed("Mock auth rejection"))
        }
    }

    fn domain(&self) -> &str {
        &self.domain
    }

    fn oauth_discovery_url(&self) -> String {
        format!(
            "https://{}/.well-known/oauth-authorization-server",
            self.domain
        )
    }

    async fn list_relations(
        &self,
        _resource: &str,
        _subject: &str,
    ) -> Result<Vec<String>, XmppError> {
        // Mock returns member relation by default
        Ok(vec!["member".to_string()])
    }

    async fn list_subjects(
        &self,
        _resource: &str,
        _relation: &str,
    ) -> Result<Vec<String>, XmppError> {
        // Mock returns empty list by default
        Ok(vec![])
    }

    async fn lookup_scram_credentials(
        &self,
        _username: &str,
    ) -> Result<Option<ScramCredentials>, XmppError> {
        // Mock returns None - native JID auth not supported in mock
        Ok(None)
    }

    async fn register_native_user(
        &self,
        username: &str,
        _password: &str,
        _email: Option<&str>,
    ) -> Result<(), XmppError> {
        // Mock registration always succeeds
        self.known_users.lock().await.insert(username.to_string());
        Ok(())
    }

    async fn native_user_exists(&self, username: &str) -> Result<bool, XmppError> {
        Ok(self.known_users.lock().await.contains(username))
    }

    async fn get_vcard(&self, _jid: &jid::BareJid) -> Result<Option<String>, XmppError> {
        // Mock returns None - no vCards exist in mock by default
        Ok(None)
    }

    async fn set_vcard(&self, _jid: &jid::BareJid, _vcard_xml: &str) -> Result<(), XmppError> {
        // Mock vCard storage always succeeds
        Ok(())
    }

    async fn get_user_avatar_url(&self, _jid: &jid::BareJid) -> Result<Option<String>, XmppError> {
        // Mock has no avatar URLs on file.
        Ok(None)
    }

    async fn create_upload_slot(
        &self,
        _requester_jid: &jid::BareJid,
        filename: &str,
        size: u64,
        content_type: Option<&str>,
    ) -> Result<waddle_xmpp::UploadSlotInfo, XmppError> {
        let domain = self.domain.clone();
        let filename = filename.to_string();
        let content_type = content_type.map(|s| s.to_string());
        // Check size limit (mock limit: 10 MB)
        if size > 10 * 1024 * 1024 {
            return Err(XmppError::not_acceptable(Some(
                "File too large. Maximum size is 10485760 bytes.".to_string(),
            )));
        }

        // Generate mock URLs
        let slot_id = format!("mock-slot-{}", uuid::Uuid::new_v4());
        let put_url = format!("https://{}/api/upload/{}", domain, slot_id);
        let get_url = format!("https://{}/api/files/{}/{}", domain, slot_id, filename);

        Ok(waddle_xmpp::UploadSlotInfo {
            put_url,
            get_url,
            put_headers: vec![(
                "Content-Type".to_string(),
                content_type.unwrap_or_else(|| "application/octet-stream".to_string()),
            )],
        })
    }

    // =========================================================================
    // RFC 6121 Roster Storage Methods (Mock implementations)
    // =========================================================================

    async fn get_roster(
        &self,
        _user_jid: &jid::BareJid,
    ) -> Result<Vec<roster::RosterItem>, XmppError> {
        // Mock returns empty roster
        Ok(vec![])
    }

    async fn get_roster_item(
        &self,
        _user_jid: &jid::BareJid,
        _contact_jid: &jid::BareJid,
    ) -> Result<Option<roster::RosterItem>, XmppError> {
        // Mock returns None - no roster items exist in mock
        Ok(None)
    }

    async fn set_roster_item(
        &self,
        _user_jid: &jid::BareJid,
        item: &roster::RosterItem,
    ) -> Result<roster::RosterSetResult, XmppError> {
        // Mock always reports item as added
        let item = item.clone();
        Ok(roster::RosterSetResult::Added(item))
    }

    async fn remove_roster_item(
        &self,
        _user_jid: &jid::BareJid,
        _contact_jid: &jid::BareJid,
    ) -> Result<bool, XmppError> {
        // Mock returns false - no items to remove in mock
        Ok(false)
    }

    async fn get_roster_version(
        &self,
        _user_jid: &jid::BareJid,
    ) -> Result<Option<String>, XmppError> {
        // Mock returns None - no roster versioning in mock
        Ok(None)
    }

    async fn update_roster_subscription(
        &self,
        _user_jid: &jid::BareJid,
        contact_jid: &jid::BareJid,
        subscription: roster::Subscription,
        ask: Option<roster::AskType>,
    ) -> Result<roster::RosterItem, XmppError> {
        // Mock returns a new roster item with the specified subscription
        let contact_jid = contact_jid.clone();
        Ok(roster::RosterItem {
            jid: contact_jid,
            name: None,
            subscription,
            ask,
            groups: vec![],
        })
    }

    async fn get_presence_subscribers(
        &self,
        _user_jid: &jid::BareJid,
    ) -> Result<Vec<jid::BareJid>, XmppError> {
        // Mock returns empty list - no subscribers in mock
        Ok(vec![])
    }

    async fn get_presence_subscriptions(
        &self,
        _user_jid: &jid::BareJid,
    ) -> Result<Vec<jid::BareJid>, XmppError> {
        // Mock returns empty list - no subscriptions in mock
        Ok(vec![])
    }

    // =========================================================================
    // XEP-0191 Blocking Command Methods (Mock implementations)
    // =========================================================================

    async fn get_blocklist(&self, user_jid: &jid::BareJid) -> Result<Vec<String>, XmppError> {
        Ok(self
            .blocked_jids
            .lock()
            .await
            .get(&user_jid.to_string())
            .map(|entries| {
                let mut blocked: Vec<_> = entries.iter().cloned().collect();
                blocked.sort();
                blocked
            })
            .unwrap_or_default())
    }

    async fn is_blocked(
        &self,
        user_jid: &jid::BareJid,
        blocked_jid: &jid::BareJid,
    ) -> Result<bool, XmppError> {
        Ok(self
            .blocked_jids
            .lock()
            .await
            .get(&user_jid.to_string())
            .is_some_and(|entries| entries.contains(&blocked_jid.to_string())))
    }

    async fn add_blocks(
        &self,
        user_jid: &jid::BareJid,
        blocked_jids: &[String],
    ) -> Result<usize, XmppError> {
        let mut store = self.blocked_jids.lock().await;
        let entry = store.entry(user_jid.to_string()).or_default();
        let mut added = 0;
        for blocked_jid in blocked_jids {
            if entry.insert(blocked_jid.clone()) {
                added += 1;
            }
        }
        Ok(added)
    }

    async fn remove_blocks(
        &self,
        user_jid: &jid::BareJid,
        blocked_jids: &[String],
    ) -> Result<usize, XmppError> {
        let mut store = self.blocked_jids.lock().await;
        let Some(entry) = store.get_mut(&user_jid.to_string()) else {
            return Ok(0);
        };
        let mut removed = 0;
        for blocked_jid in blocked_jids {
            if entry.remove(blocked_jid) {
                removed += 1;
            }
        }
        Ok(removed)
    }

    async fn remove_all_blocks(&self, user_jid: &jid::BareJid) -> Result<usize, XmppError> {
        Ok(self
            .blocked_jids
            .lock()
            .await
            .remove(&user_jid.to_string())
            .map(|entries| entries.len())
            .unwrap_or(0))
    }

    // =========================================================================
    // XEP-0049 Private XML Storage Methods (Mock implementations)
    // =========================================================================

    async fn get_private_xml(
        &self,
        _jid: &jid::BareJid,
        _namespace: &str,
    ) -> Result<Option<String>, XmppError> {
        // Mock returns None - no private data in mock
        Ok(None)
    }

    async fn set_private_xml(
        &self,
        _jid: &jid::BareJid,
        _namespace: &str,
        _xml_content: &str,
    ) -> Result<(), XmppError> {
        // Mock private XML storage always succeeds
        Ok(())
    }

    async fn list_inbox(&self, user_jid: &jid::BareJid) -> Result<Vec<InboxEntry>, XmppError> {
        self.inbox_storage
            .list(user_jid)
            .await
            .map_err(|error| XmppError::internal(format!("Inbox error: {}", error)))
    }

    async fn upsert_inbox_entry(
        &self,
        user_jid: &jid::BareJid,
        entry: InboxEntry,
        increment_unread: bool,
    ) -> Result<(), XmppError> {
        self.inbox_storage
            .upsert(user_jid, entry, increment_unread)
            .await
            .map(|_| ())
            .map_err(|error| XmppError::internal(format!("Inbox error: {}", error)))
    }

    async fn mark_inbox_read(
        &self,
        user_jid: &jid::BareJid,
        partner_jid: &jid::BareJid,
    ) -> Result<(), XmppError> {
        self.inbox_storage
            .mark_read(user_jid, partner_jid, None)
            .await
            .map_err(|error| XmppError::internal(format!("Inbox error: {}", error)))
    }

    async fn inbox_total_unread(&self, user_jid: &jid::BareJid) -> Result<u64, XmppError> {
        self.inbox_storage
            .total_unread(user_jid)
            .await
            .map_err(|error| XmppError::internal(format!("Inbox error: {}", error)))
    }

    async fn list_user_waddles(
        &self,
        _user_id: &str,
    ) -> Result<Vec<waddle_xmpp::WaddleInfo>, XmppError> {
        // Mock returns no waddles — auto-join is a no-op in tests by default
        Ok(Vec::new())
    }

    async fn list_waddle_channels(
        &self,
        waddle_id: &str,
    ) -> Result<Vec<waddle_xmpp::ChannelInfo>, XmppError> {
        Ok(self
            .waddle_channels
            .get(waddle_id)
            .cloned()
            .unwrap_or_default())
    }

    async fn get_channel_room_info(
        &self,
        waddle_id: &str,
        channel_id: &str,
    ) -> Result<Option<waddle_xmpp::ChannelRoomInfo>, XmppError> {
        Ok(self
            .waddle_channels
            .get(waddle_id)
            .and_then(|channels| {
                channels
                    .iter()
                    .find(|channel| channel.id == channel_id)
                    .cloned()
            })
            .map(|channel| waddle_xmpp::ChannelRoomInfo {
                waddle_id: waddle_id.to_string(),
                channel,
            }))
    }

    async fn get_waddle_details(
        &self,
        waddle_id: &str,
    ) -> Result<Option<waddle_xmpp::WaddleDetails>, XmppError> {
        Ok(self
            .waddle_details
            .iter()
            .find(|w| w.id == waddle_id)
            .cloned())
    }

    async fn get_user_waddles_with_details(
        &self,
        _user_id: &str,
    ) -> Result<Vec<waddle_xmpp::WaddleDetails>, XmppError> {
        // In tests, return all waddles for any authenticated user
        Ok(self.waddle_details.clone())
    }

    async fn list_all_waddles(
        &self,
        limit: usize,
        _offset: usize,
    ) -> Result<Vec<waddle_xmpp::WaddleDetails>, XmppError> {
        Ok(self.waddle_details.iter().take(limit).cloned().collect())
    }
}

/// Generated TLS credentials for testing.
pub struct TestTlsCredentials {
    pub cert_pem: Vec<u8>,
    pub key_pem: Vec<u8>,
}

impl TestTlsCredentials {
    /// Generate self-signed TLS credentials for testing.
    pub fn generate(domain: &str) -> Self {
        install_crypto_provider();
        let subject_alt_names = vec![domain.to_string(), "localhost".to_string()];
        let CertifiedKey { cert, key_pair } = generate_simple_self_signed(subject_alt_names)
            .expect("Failed to generate test certificate");

        let cert_pem = cert.pem().into_bytes();
        let key_pem = key_pair.serialize_pem().into_bytes();
        Self { cert_pem, key_pem }
    }

    /// Create a TLS acceptor (server-side) from these credentials.
    pub fn tls_acceptor(&self) -> TlsAcceptor {
        use rustls_pemfile::{certs, pkcs8_private_keys};

        let certs: Vec<_> = certs(&mut BufReader::new(Cursor::new(&self.cert_pem)))
            .filter_map(|r| r.ok())
            .collect();

        let keys: Vec<_> = pkcs8_private_keys(&mut BufReader::new(Cursor::new(&self.key_pem)))
            .filter_map(|r| r.ok())
            .collect();

        let key = keys.into_iter().next().expect("No private key");

        let server_config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, PrivateKeyDer::Pkcs8(key))
            .expect("Failed to create server config");

        TlsAcceptor::from(Arc::new(server_config))
    }

    /// Create a TLS connector (client-side) that trusts this certificate.
    pub fn tls_connector(&self) -> TlsConnector {
        use rustls_pemfile::certs;

        let mut root_store = RootCertStore::empty();
        let cert_der = certs(&mut BufReader::new(Cursor::new(&self.cert_pem)))
            .filter_map(|r| r.ok())
            .next()
            .expect("No certificate");
        root_store.add(cert_der).expect("Failed to add cert");

        let client_config = ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();

        TlsConnector::from(Arc::new(client_config))
    }
}

/// Test server handle.
pub struct TestServer {
    pub addr: SocketAddr,
    pub domain: String,
    pub tls_credentials: TestTlsCredentials,
    shutdown_tx: Option<oneshot::Sender<()>>,
}

impl TestServer {
    /// Start a test XMPP server on an available port.
    pub async fn start() -> Self {
        Self::start_with_state(Arc::new(MockAppState::new("localhost"))).await
    }

    /// Start a test server with custom app state.
    pub async fn start_with_state<S: AppState>(app_state: Arc<S>) -> Self {
        Self::start_with_state_config(app_state, false).await
    }

    /// Start a test server with custom app state and single-tenant mode.
    pub async fn start_with_state_single_tenant<S: AppState>(app_state: Arc<S>) -> Self {
        Self::start_with_state_config(app_state, true).await
    }

    /// Start a test server with custom app state and configuration.
    async fn start_with_state_config<S: AppState>(app_state: Arc<S>, single_tenant: bool) -> Self {
        install_crypto_provider();
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("Failed to bind test server");
        let addr = listener.local_addr().expect("Failed to get local addr");

        let domain = app_state.domain().to_string();
        let tls_credentials = TestTlsCredentials::generate(&domain);
        let tls_acceptor = tls_credentials.tls_acceptor();

        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        // Spawn server task
        tokio::spawn(run_test_server(
            listener,
            tls_acceptor,
            domain.clone(),
            app_state,
            shutdown_rx,
            single_tenant,
        ));

        Self {
            addr,
            domain,
            tls_credentials,
            shutdown_tx: Some(shutdown_tx),
        }
    }

    /// Get a TLS connector that trusts this server.
    pub fn tls_connector(&self) -> TlsConnector {
        self.tls_credentials.tls_connector()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

/// Run the test server accept loop.
async fn run_test_server<S: AppState>(
    listener: TcpListener,
    tls_acceptor: TlsAcceptor,
    domain: String,
    app_state: Arc<S>,
    mut shutdown_rx: oneshot::Receiver<()>,
    single_tenant: bool,
) {
    // Create SHARED registries at server level - these are used by all connections
    let muc_domain = format!("muc.{}", domain);
    let room_registry =
        kameo::spawn(waddle_xmpp::muc::room_registry_actor::RoomRegistryActor::new(muc_domain));
    let connection_registry = std::sync::Arc::new(waddle_xmpp::registry::ConnectionRegistry::new());
    let user_registry = kameo::spawn(waddle_xmpp::registry::UserRegistryActor::new());
    // Create an in-memory MAM storage for the test (shared)
    let mam_storage = std::sync::Arc::new(
        waddle_xmpp::mam::SqlxMamStorage::open_in_memory()
            .await
            .unwrap(),
    );
    // Create a shared ISR token store
    let isr_token_store = waddle_xmpp::isr::create_shared_store();
    // Create a shared SM session registry for stream resumption
    let sm_session_registry: std::sync::Arc<dyn waddle_xmpp::stream_management::SmSessionRegistry> =
        std::sync::Arc::new(waddle_xmpp::stream_management::InMemorySmSessionRegistry::new());
    // Create a shared PubSub storage for PEP
    let pubsub_storage: std::sync::Arc<dyn waddle_xmpp::pubsub::PubSubStorage + Send + Sync> =
        std::sync::Arc::new(waddle_xmpp::pubsub::InMemoryPubSubStorage::new());
    let push_store: std::sync::Arc<dyn waddle_xmpp::push::PushSubscriptionStore + Send + Sync> =
        std::sync::Arc::new(waddle_xmpp::push::InMemoryPushStore::new());
    let push_sender: std::sync::Arc<dyn waddle_xmpp::push::WebPushSender + Send + Sync> =
        std::sync::Arc::new(waddle_xmpp::push::HttpWebPushSender::new());
    let extension_manager = Arc::new(
        waddle_extensions::ExtensionManager::from_env()
            .await
            .expect("extension manager"),
    );
    // Create a shared command registry for ad-hoc commands
    let command_registry = std::sync::Arc::new(waddle_xmpp::commands::CommandRegistry::new());

    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, peer_addr)) => {
                        let tls = tls_acceptor.clone();
                        let dom = domain.clone();
                        let state = Arc::clone(&app_state);
                        // Clone the shared registries for this connection
                        let rooms = room_registry.clone();
                        let conns = Arc::clone(&connection_registry);
                        let users = user_registry.clone();
                        let mam = Arc::clone(&mam_storage);
                        let isr = Arc::clone(&isr_token_store);
                        let sm_reg = Arc::clone(&sm_session_registry);
                        let pubsub = Arc::clone(&pubsub_storage);
                        let push_store = Arc::clone(&push_store);
                        let push_sender = Arc::clone(&push_sender);
                        let ext = Arc::clone(&extension_manager);
                        let cmd_registry = Arc::clone(&command_registry);
                        // Enable registration for tests
                        let registration_enabled = true;
                        let st = single_tenant;
                        tokio::spawn(async move {
                            let _ = waddle_xmpp::connection::ConnectionActor::handle_connection(
                                stream, peer_addr, tls, dom, state, rooms, conns, users, mam, isr, sm_reg, registration_enabled, pubsub, ext, st, push_store, push_sender, cmd_registry
                            ).await;
                        });
                    }
                    Err(e) => {
                        eprintln!("Accept error: {}", e);
                    }
                }
            }
            _ = &mut shutdown_rx => {
                break;
            }
        }
    }
}

/// A simpler client that operates directly on streams for more precise control.
pub struct RawXmppClient {
    tcp: Option<TcpStream>,
    tls: Option<tokio_rustls::client::TlsStream<TcpStream>>,
    buffer: String,
}

impl RawXmppClient {
    /// Connect to a server.
    pub async fn connect(addr: SocketAddr) -> std::io::Result<Self> {
        let tcp = TcpStream::connect(addr).await?;
        Ok(Self {
            tcp: Some(tcp),
            tls: None,
            buffer: String::new(),
        })
    }

    /// Send raw data.
    pub async fn send(&mut self, data: &str) -> std::io::Result<()> {
        if let Some(ref mut tls) = self.tls {
            tls.write_all(data.as_bytes()).await?;
            tls.flush().await?;
        } else if let Some(ref mut tcp) = self.tcp {
            tcp.write_all(data.as_bytes()).await?;
            tcp.flush().await?;
        }
        Ok(())
    }

    /// Read with timeout.
    pub async fn read(&mut self, timeout_dur: Duration) -> std::io::Result<String> {
        let mut buf = [0u8; 4096];
        let n = timeout(timeout_dur, async {
            if let Some(ref mut tls) = self.tls {
                tls.read(&mut buf).await
            } else if let Some(ref mut tcp) = self.tcp {
                tcp.read(&mut buf).await
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "Not connected",
                ))
            }
        })
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "Timeout"))??;

        let data = String::from_utf8_lossy(&buf[..n]).to_string();
        self.buffer.push_str(&data);
        Ok(data)
    }

    /// Read until pattern found.
    pub async fn read_until(
        &mut self,
        pattern: &str,
        timeout_dur: Duration,
    ) -> std::io::Result<String> {
        let start = std::time::Instant::now();
        while !self.buffer.contains(pattern) {
            if start.elapsed() > timeout_dur {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("Timeout waiting for: {}", pattern),
                ));
            }
            let remaining = timeout_dur.saturating_sub(start.elapsed());
            self.read(remaining).await?;
        }
        Ok(self.buffer.clone())
    }

    /// Clear buffer.
    pub fn clear(&mut self) {
        self.buffer.clear();
    }

    /// Take the buffered response data.
    pub fn take_buffer(&mut self) -> String {
        std::mem::take(&mut self.buffer)
    }

    /// Upgrade to TLS.
    pub async fn upgrade_tls(
        &mut self,
        connector: TlsConnector,
        domain: &str,
    ) -> std::io::Result<()> {
        let tcp = self
            .tcp
            .take()
            .ok_or_else(|| std::io::Error::other("No TCP connection or already TLS"))?;

        let server_name: rustls::pki_types::ServerName<'static> =
            domain.to_string().try_into().map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "Invalid server name")
            })?;

        let tls = connector.connect(server_name, tcp).await?;
        self.tls = Some(tls);
        self.buffer.clear();
        Ok(())
    }

    /// Check if the connection has been upgraded to TLS.
    pub fn is_tls(&self) -> bool {
        self.tls.is_some()
    }
}

/// Helper to encode SASL PLAIN credentials.
pub fn encode_sasl_plain(jid: &str, password: &str) -> String {
    let data = format!("\0{}\0{}", jid, password);
    BASE64_STANDARD.encode(data.as_bytes())
}

/// Generate a unique test credential (not a real secret).
pub fn test_secret(label: &str) -> String {
    format!("{label}-{}", uuid::Uuid::new_v4())
}

/// Helper to validate stream header attributes.
pub fn validate_stream_header(response: &str) -> Result<(), String> {
    // Check for required xmlns
    if !response.contains("xmlns='jabber:client'") && !response.contains("xmlns=\"jabber:client\"")
    {
        return Err("Missing xmlns='jabber:client'".to_string());
    }

    // Check for xmlns:stream
    if !response.contains("xmlns:stream='http://etherx.jabber.org/streams'")
        && !response.contains("xmlns:stream=\"http://etherx.jabber.org/streams\"")
    {
        return Err("Missing xmlns:stream".to_string());
    }

    // Check for version
    if !response.contains("version='1.0'") && !response.contains("version=\"1.0\"") {
        return Err("Missing or incorrect version attribute".to_string());
    }

    // Check for id
    if !response.contains("id='") && !response.contains("id=\"") {
        return Err("Missing id attribute".to_string());
    }

    // Check for from
    if !response.contains("from='") && !response.contains("from=\"") {
        return Err("Missing from attribute".to_string());
    }

    Ok(())
}

/// Extract a JID from a bind result.
pub fn extract_bound_jid(response: &str) -> Option<String> {
    // Look for <jid>...</jid>
    let start = response.find("<jid>")?;
    let end = response.find("</jid>")?;
    let jid = &response[start + 5..end];
    Some(jid.to_string())
}

/// Initialize shared test environment (tracing + crypto provider).
pub fn init_test_env() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        install_crypto_provider();
        let _ = tracing_subscriber::fmt()
            .with_env_filter("debug")
            .with_test_writer()
            .try_init();
    });
}

/// Establish a full authenticated and bound XMPP session.
pub async fn establish_bound_session(
    client: &mut RawXmppClient,
    server: &TestServer,
    username: &str,
    resource: &str,
) -> std::io::Result<String> {
    // Initial stream.
    client
        .send(&format!(
            "<?xml version='1.0'?>\
            <stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' \
            to='{}' version='1.0'>",
            server.domain
        ))
        .await?;
    client
        .read_until("</stream:features>", DEFAULT_TIMEOUT)
        .await?;
    client.clear();

    // STARTTLS.
    client
        .send("<starttls xmlns='urn:ietf:params:xml:ns:xmpp-tls'/>")
        .await?;
    client.read_until("<proceed", DEFAULT_TIMEOUT).await?;
    client.clear();

    // Upgrade to TLS.
    let connector = server.tls_connector();
    client.upgrade_tls(connector, &server.domain).await?;

    // Post-TLS stream for SASL features.
    client
        .send(&format!(
            "<?xml version='1.0'?>\
            <stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' \
            to='{}' version='1.0'>",
            server.domain
        ))
        .await?;
    client
        .read_until("</stream:features>", DEFAULT_TIMEOUT)
        .await?;
    client.clear();

    // SASL PLAIN auth.
    let auth_secret = test_secret("bind-session");
    let auth_data = encode_sasl_plain(&format!("{}@{}", username, server.domain), &auth_secret);
    client
        .send(&format!(
            "<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='PLAIN'>{}</auth>",
            auth_data
        ))
        .await?;
    client.read_until("<success", DEFAULT_TIMEOUT).await?;
    client.clear();

    // Post-auth stream for bind features.
    client
        .send(&format!(
            "<?xml version='1.0'?>\
            <stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' \
            to='{}' version='1.0'>",
            server.domain
        ))
        .await?;
    client
        .read_until("</stream:features>", DEFAULT_TIMEOUT)
        .await?;
    client.clear();

    // Resource bind.
    client
        .send(&format!(
            "<iq type='set' id='bind-1' xmlns='jabber:client'>\
                <bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'>\
                    <resource>{}</resource>\
                </bind>\
            </iq>",
            resource
        ))
        .await?;
    let bind_response = client.read_until("</iq>", DEFAULT_TIMEOUT).await?;
    client.clear();

    extract_bound_jid(&bind_response).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Bind response missing <jid>",
        )
    })
}

async fn read_iq_response(client: &mut RawXmppClient) -> std::io::Result<String> {
    let start = std::time::Instant::now();
    loop {
        let buffered = client.buffer.as_str();
        if buffered.contains("</iq>")
            || (buffered.contains("<iq") && buffered.contains("/>") && !buffered.contains("</iq>"))
        {
            let response = client.buffer.clone();
            client.clear();
            return Ok(response);
        }

        if start.elapsed() > DEFAULT_TIMEOUT {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Timeout waiting for IQ response",
            ));
        }

        let remaining = DEFAULT_TIMEOUT.saturating_sub(start.elapsed());
        client.read(remaining).await?;
    }
}

/// Send a disco#info query and read a single IQ response.
pub async fn disco_info_query(
    client: &mut RawXmppClient,
    to: &str,
    id: &str,
) -> std::io::Result<String> {
    client
        .send(&format!(
            "<iq type='get' id='{}' to='{}' xmlns='jabber:client'>\
                <query xmlns='http://jabber.org/protocol/disco#info'/>\
            </iq>",
            id, to
        ))
        .await?;
    read_iq_response(client).await
}

/// Join a MUC room and wait for self-presence (status code 110).
pub async fn join_muc_room(
    client: &mut RawXmppClient,
    room_jid: &str,
    nick: &str,
) -> std::io::Result<String> {
    client
        .send(&format!(
            "<presence to='{}/{}' xmlns='jabber:client'>\
                <x xmlns='http://jabber.org/protocol/muc'>\
                    <history maxstanzas='0'/>\
                </x>\
            </presence>",
            room_jid, nick
        ))
        .await?;
    let response = client.read_until("110", DEFAULT_TIMEOUT).await?;
    client.clear();
    Ok(response)
}

/// Send an XEP-0199 ping query and read a single IQ response.
pub async fn ping_query(client: &mut RawXmppClient, to: &str, id: &str) -> std::io::Result<String> {
    client
        .send(&format!(
            "<iq type='get' id='{}' to='{}' xmlns='jabber:client'>\
                <ping xmlns='urn:xmpp:ping'/>\
            </iq>",
            id, to
        ))
        .await?;
    read_iq_response(client).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_surface_smoke_test() {
        let accepting = MockAppState::new("localhost");
        let rejecting = MockAppState::rejecting("localhost");
        assert!(accepting.accept_auth);
        assert!(!rejecting.accept_auth);

        let creds = TestTlsCredentials::generate("localhost");
        let _ = creds.tls_acceptor();
        let _ = creds.tls_connector();

        let dummy_server = TestServer {
            addr: "127.0.0.1:5222".parse().expect("valid socket address"),
            domain: "localhost".to_string(),
            tls_credentials: TestTlsCredentials::generate("localhost"),
            shutdown_tx: None,
        };
        let _ = dummy_server.tls_connector();

        let mut client = RawXmppClient {
            tcp: None,
            tls: None,
            buffer: "hello".to_string(),
        };
        assert!(!client.is_tls());
        assert_eq!(client.take_buffer(), "hello");
        client.clear();
        assert!(client.buffer.is_empty());

        let auth_secret = test_secret("plain-auth");
        assert_eq!(
            encode_sasl_plain("alice@localhost", &auth_secret),
            BASE64_STANDARD.encode(format!("\0alice@localhost\0{auth_secret}").as_bytes())
        );
        assert_eq!(
            extract_bound_jid("<jid>alice@localhost/desktop</jid>").as_deref(),
            Some("alice@localhost/desktop")
        );
        assert!(validate_stream_header(
            "<stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' id='stream-id' from='localhost' version='1.0'>"
        )
        .is_ok());
        assert_eq!(DEFAULT_TIMEOUT, Duration::from_secs(5));
    }
}
