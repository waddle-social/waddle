//! Test utilities for XMPP interoperability testing.
//!
//! Provides helpers for starting test servers, generating TLS certificates,
//! and simulating XMPP client connections.

use std::future::Future;
use std::io::{BufReader, Cursor};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use base64::prelude::*;
use jid::Jid;
use rcgen::{CertifiedKey, generate_simple_self_signed};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::time::timeout;
use tokio_rustls::{TlsAcceptor, TlsConnector, rustls::{ClientConfig, RootCertStore, ServerConfig}};
use waddle_xmpp::{AppState, ScramCredentials, Session, XmppError, roster};

/// Install the ring crypto provider for rustls.
/// Must be called once before any TLS operations.
pub fn install_crypto_provider() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        rustls::crypto::ring::default_provider()
            .install_default()
            .expect("Failed to install crypto provider");
    });
}

/// Default timeout for test operations.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// Test application state that accepts any authentication.
pub struct MockAppState {
    pub domain: String,
    /// Whether to accept all auth attempts (true) or reject them (false)
    pub accept_auth: bool,
}

impl MockAppState {
    pub fn new(domain: &str) -> Self {
        Self {
            domain: domain.to_string(),
            accept_auth: true,
        }
    }

    pub fn rejecting(domain: &str) -> Self {
        Self {
            domain: domain.to_string(),
            accept_auth: false,
        }
    }
}

impl AppState for MockAppState {
    fn validate_session(
        &self,
        jid: &Jid,
        _token: &str,
    ) -> impl Future<Output = Result<Session, XmppError>> + Send {
        let accept = self.accept_auth;
        let jid = jid.clone();
        async move {
            if accept {
                Ok(Session {
                    did: format!("did:plc:test{}", jid.node().map(|n| n.to_string()).unwrap_or_default()),
                    jid: jid.to_bare().into(),
                    created_at: chrono::Utc::now(),
                    expires_at: chrono::Utc::now() + chrono::Duration::hours(24),
                })
            } else {
                Err(XmppError::auth_failed("Mock auth rejection"))
            }
        }
    }

    fn check_permission(
        &self,
        _resource: &str,
        _action: &str,
        _subject: &str,
    ) -> impl Future<Output = Result<bool, XmppError>> + Send {
        async { Ok(true) }
    }

    fn validate_session_token(
        &self,
        token: &str,
    ) -> impl Future<Output = Result<Session, XmppError>> + Send {
        let accept = self.accept_auth;
        let domain = self.domain.clone();
        let token = token.to_string();
        async move {
            if accept {
                // Mock: derive a JID from the token
                let mock_jid = format!("user_{}@{}", &token[..token.len().min(8)], domain);
                Ok(Session {
                    did: format!("did:plc:mock{}", &token[..token.len().min(8)]),
                    jid: mock_jid.parse().unwrap_or_else(|_| "fallback@test.local".parse().unwrap()),
                    created_at: chrono::Utc::now(),
                    expires_at: chrono::Utc::now() + chrono::Duration::hours(24),
                })
            } else {
                Err(XmppError::auth_failed("Mock auth rejection"))
            }
        }
    }

    fn domain(&self) -> &str {
        &self.domain
    }

    fn oauth_discovery_url(&self) -> String {
        format!("https://{}/.well-known/oauth-authorization-server", self.domain)
    }

    fn list_relations(
        &self,
        _resource: &str,
        _subject: &str,
    ) -> impl Future<Output = Result<Vec<String>, XmppError>> + Send {
        // Mock returns member relation by default
        async { Ok(vec!["member".to_string()]) }
    }

    fn list_subjects(
        &self,
        _resource: &str,
        _relation: &str,
    ) -> impl Future<Output = Result<Vec<String>, XmppError>> + Send {
        // Mock returns empty list by default
        async { Ok(vec![]) }
    }

    fn lookup_scram_credentials(
        &self,
        _username: &str,
    ) -> impl Future<Output = Result<Option<ScramCredentials>, XmppError>> + Send {
        // Mock returns None - native JID auth not supported in mock
        async { Ok(None) }
    }

    fn register_native_user(
        &self,
        _username: &str,
        _password: &str,
        _email: Option<&str>,
    ) -> impl Future<Output = Result<(), XmppError>> + Send {
        // Mock registration always succeeds
        async { Ok(()) }
    }

    fn native_user_exists(
        &self,
        _username: &str,
    ) -> impl Future<Output = Result<bool, XmppError>> + Send {
        // Mock returns false - no users exist in mock by default
        async { Ok(false) }
    }

    fn get_vcard(
        &self,
        _jid: &jid::BareJid,
    ) -> impl Future<Output = Result<Option<String>, XmppError>> + Send {
        // Mock returns None - no vCards exist in mock by default
        async { Ok(None) }
    }

    fn set_vcard(
        &self,
        _jid: &jid::BareJid,
        _vcard_xml: &str,
    ) -> impl Future<Output = Result<(), XmppError>> + Send {
        // Mock vCard storage always succeeds
        async { Ok(()) }
    }

    fn create_upload_slot(
        &self,
        requester_jid: &jid::BareJid,
        filename: &str,
        size: u64,
        content_type: Option<&str>,
    ) -> impl Future<Output = Result<waddle_xmpp::UploadSlotInfo, XmppError>> + Send {
        let domain = self.domain.clone();
        let filename = filename.to_string();
        let content_type = content_type.map(|s| s.to_string());
        let _jid = requester_jid.clone();
        async move {
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
                put_headers: vec![
                    ("Content-Type".to_string(), content_type.unwrap_or_else(|| "application/octet-stream".to_string())),
                ],
            })
        }
    }

    // =========================================================================
    // RFC 6121 Roster Storage Methods (Mock implementations)
    // =========================================================================

    fn get_roster(
        &self,
        _user_jid: &jid::BareJid,
    ) -> impl Future<Output = Result<Vec<roster::RosterItem>, XmppError>> + Send {
        // Mock returns empty roster
        async { Ok(vec![]) }
    }

    fn get_roster_item(
        &self,
        _user_jid: &jid::BareJid,
        _contact_jid: &jid::BareJid,
    ) -> impl Future<Output = Result<Option<roster::RosterItem>, XmppError>> + Send {
        // Mock returns None - no roster items exist in mock
        async { Ok(None) }
    }

    fn set_roster_item(
        &self,
        _user_jid: &jid::BareJid,
        item: &roster::RosterItem,
    ) -> impl Future<Output = Result<roster::RosterSetResult, XmppError>> + Send {
        // Mock always reports item as added
        let item = item.clone();
        async move { Ok(roster::RosterSetResult::Added(item)) }
    }

    fn remove_roster_item(
        &self,
        _user_jid: &jid::BareJid,
        _contact_jid: &jid::BareJid,
    ) -> impl Future<Output = Result<bool, XmppError>> + Send {
        // Mock returns false - no items to remove in mock
        async { Ok(false) }
    }

    fn get_roster_version(
        &self,
        _user_jid: &jid::BareJid,
    ) -> impl Future<Output = Result<Option<String>, XmppError>> + Send {
        // Mock returns None - no roster versioning in mock
        async { Ok(None) }
    }

    fn update_roster_subscription(
        &self,
        _user_jid: &jid::BareJid,
        contact_jid: &jid::BareJid,
        subscription: roster::Subscription,
        ask: Option<roster::AskType>,
    ) -> impl Future<Output = Result<roster::RosterItem, XmppError>> + Send {
        // Mock returns a new roster item with the specified subscription
        let contact_jid = contact_jid.clone();
        async move {
            Ok(roster::RosterItem {
                jid: contact_jid,
                name: None,
                subscription,
                ask,
                groups: vec![],
            })
        }
    }

    fn get_presence_subscribers(
        &self,
        _user_jid: &jid::BareJid,
    ) -> impl Future<Output = Result<Vec<jid::BareJid>, XmppError>> + Send {
        // Mock returns empty list - no subscribers in mock
        async { Ok(vec![]) }
    }

    fn get_presence_subscriptions(
        &self,
        _user_jid: &jid::BareJid,
    ) -> impl Future<Output = Result<Vec<jid::BareJid>, XmppError>> + Send {
        // Mock returns empty list - no subscriptions in mock
        async { Ok(vec![]) }
    }
}

/// Generated TLS credentials for testing.
pub struct TestTlsCredentials {
    pub cert_pem: Vec<u8>,
    pub key_pem: Vec<u8>,
    pub cert_der: CertificateDer<'static>,
}

impl TestTlsCredentials {
    /// Generate self-signed TLS credentials for testing.
    pub fn generate(domain: &str) -> Self {
        let subject_alt_names = vec![domain.to_string(), "localhost".to_string()];
        let CertifiedKey { cert, key_pair } = generate_simple_self_signed(subject_alt_names)
            .expect("Failed to generate test certificate");

        let cert_pem = cert.pem().into_bytes();
        let key_pem = key_pair.serialize_pem().into_bytes();
        let cert_der = CertificateDer::from(cert.der().to_vec());

        Self {
            cert_pem,
            key_pem,
            cert_der,
        }
    }

    /// Create a TLS acceptor (server-side) from these credentials.
    pub fn tls_acceptor(&self) -> TlsAcceptor {
        use rustls_pemfile::{certs, pkcs8_private_keys};

        let certs: Vec<CertificateDer> = certs(&mut BufReader::new(Cursor::new(&self.cert_pem)))
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
        let mut root_store = RootCertStore::empty();
        root_store.add(self.cert_der.clone()).expect("Failed to add cert");

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
        ));

        Self {
            addr,
            domain,
            tls_credentials,
            shutdown_tx: Some(shutdown_tx),
        }
    }

    /// Get a TCP stream connected to this server.
    pub async fn connect(&self) -> TcpStream {
        TcpStream::connect(self.addr)
            .await
            .expect("Failed to connect to test server")
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
) {
    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, peer_addr)) => {
                        let tls = tls_acceptor.clone();
                        let dom = domain.clone();
                        let state = Arc::clone(&app_state);
                        tokio::spawn(async move {
                            // Create a MUC room registry for the test
                            let muc_domain = format!("muc.{}", dom);
                            let room_registry = std::sync::Arc::new(
                                waddle_xmpp::muc::MucRoomRegistry::new(muc_domain)
                            );
                            // Create a connection registry for the test
                            let connection_registry = std::sync::Arc::new(
                                waddle_xmpp::registry::ConnectionRegistry::new()
                            );
                            // Create an in-memory MAM storage for the test
                            let db = libsql::Builder::new_local(":memory:")
                                .build()
                                .await
                                .unwrap();
                            let conn = db.connect().unwrap();
                            let mam_storage = std::sync::Arc::new(
                                waddle_xmpp::mam::LibSqlMamStorage::new(conn)
                            );
                            // Create an ISR token store for the test
                            let isr_token_store = waddle_xmpp::isr::create_shared_store();
                            // Enable registration for tests
                            let registration_enabled = true;
                            let _ = waddle_xmpp::connection::ConnectionActor::handle_connection(
                                stream, peer_addr, tls, dom, state, room_registry, connection_registry, mam_storage, isr_token_store, registration_enabled
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

/// XMPP test client for interop testing.
pub struct TestClient {
    stream: TestClientStream,
    domain: String,
    buffer: Vec<u8>,
}

enum TestClientStream {
    Tcp(TcpStream),
    Tls(tokio_rustls::client::TlsStream<TcpStream>),
}

impl TestClient {
    /// Create a new test client connected to the server.
    pub async fn connect(server: &TestServer) -> Self {
        let stream = server.connect().await;
        Self {
            stream: TestClientStream::Tcp(stream),
            domain: server.domain.clone(),
            buffer: Vec::new(),
        }
    }

    /// Send raw XML data.
    pub async fn send(&mut self, data: &str) -> Result<(), std::io::Error> {
        match &mut self.stream {
            TestClientStream::Tcp(s) => {
                s.write_all(data.as_bytes()).await?;
                s.flush().await?;
            }
            TestClientStream::Tls(s) => {
                s.write_all(data.as_bytes()).await?;
                s.flush().await?;
            }
        }
        Ok(())
    }

    /// Read raw data with timeout.
    pub async fn read_raw(&mut self, timeout_dur: Duration) -> Result<String, std::io::Error> {
        let mut buf = [0u8; 8192];
        let n = match timeout(timeout_dur, async {
            match &mut self.stream {
                TestClientStream::Tcp(s) => s.read(&mut buf).await,
                TestClientStream::Tls(s) => s.read(&mut buf).await,
            }
        }).await {
            Ok(result) => result?,
            Err(_) => return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "Read timeout")),
        };

        self.buffer.extend_from_slice(&buf[..n]);
        Ok(String::from_utf8_lossy(&self.buffer).to_string())
    }

    /// Read until we find a specific pattern.
    pub async fn read_until(&mut self, pattern: &str, timeout_dur: Duration) -> Result<String, std::io::Error> {
        let start = std::time::Instant::now();
        loop {
            let data = String::from_utf8_lossy(&self.buffer).to_string();
            if data.contains(pattern) {
                return Ok(data);
            }

            if start.elapsed() > timeout_dur {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("Timeout waiting for pattern: {}", pattern),
                ));
            }

            let mut buf = [0u8; 4096];
            let remaining = timeout_dur - start.elapsed();
            let n = match timeout(remaining, async {
                match &mut self.stream {
                    TestClientStream::Tcp(s) => s.read(&mut buf).await,
                    TestClientStream::Tls(s) => s.read(&mut buf).await,
                }
            }).await {
                Ok(Ok(n)) => n,
                Ok(Err(e)) => return Err(e),
                Err(_) => return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "Read timeout")),
            };

            if n == 0 {
                return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "Connection closed"));
            }

            self.buffer.extend_from_slice(&buf[..n]);
        }
    }

    /// Clear the read buffer.
    pub fn clear_buffer(&mut self) {
        self.buffer.clear();
    }

    /// Send XMPP stream header.
    pub async fn send_stream_header(&mut self) -> Result<(), std::io::Error> {
        let header = format!(
            "<?xml version='1.0'?>\
            <stream:stream \
            xmlns='jabber:client' \
            xmlns:stream='http://etherx.jabber.org/streams' \
            to='{}' \
            version='1.0'>",
            self.domain
        );
        self.send(&header).await
    }

    /// Wait for stream header response from server.
    pub async fn expect_stream_header(&mut self) -> Result<String, std::io::Error> {
        self.read_until("<stream:stream", DEFAULT_TIMEOUT).await
    }

    /// Wait for stream features.
    pub async fn expect_features(&mut self) -> Result<String, std::io::Error> {
        self.read_until("</stream:features>", DEFAULT_TIMEOUT).await
    }

    /// Send STARTTLS request.
    pub async fn send_starttls(&mut self) -> Result<(), std::io::Error> {
        self.send("<starttls xmlns='urn:ietf:params:xml:ns:xmpp-tls'/>").await
    }

    /// Wait for STARTTLS proceed response.
    pub async fn expect_proceed(&mut self) -> Result<String, std::io::Error> {
        self.read_until("<proceed", DEFAULT_TIMEOUT).await
    }

    /// Upgrade the connection to TLS.
    /// Note: This method is not used - prefer RawXmppClient for TLS tests.
    #[allow(dead_code)]
    pub async fn upgrade_to_tls(&mut self, _connector: TlsConnector, _domain: &str) -> Result<(), std::io::Error> {
        // Note: TestClient with proper TLS upgrade would require unsafe or interior mutability
        // Use RawXmppClient instead for TLS upgrade tests
        Err(std::io::Error::new(std::io::ErrorKind::Other, "Use RawXmppClient for TLS upgrade"))
    }

    /// Send SASL PLAIN auth.
    pub async fn send_sasl_plain(&mut self, jid: &str, token: &str) -> Result<(), std::io::Error> {
        // SASL PLAIN format: \0authcid\0password
        let auth_data = format!("\0{}\0{}", jid, token);
        let encoded = BASE64_STANDARD.encode(auth_data.as_bytes());

        self.send(&format!(
            "<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='PLAIN'>{}</auth>",
            encoded
        )).await
    }

    /// Wait for SASL success.
    pub async fn expect_sasl_success(&mut self) -> Result<String, std::io::Error> {
        self.read_until("<success", DEFAULT_TIMEOUT).await
    }

    /// Wait for SASL failure.
    pub async fn expect_sasl_failure(&mut self) -> Result<String, std::io::Error> {
        self.read_until("<failure", DEFAULT_TIMEOUT).await
    }

    /// Send resource bind request.
    pub async fn send_bind(&mut self, resource: Option<&str>) -> Result<(), std::io::Error> {
        let bind_body = match resource {
            Some(r) => format!("<resource>{}</resource>", r),
            None => String::new(),
        };

        self.send(&format!(
            "<iq type='set' id='bind_1' xmlns='jabber:client'>\
                <bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'>{}</bind>\
            </iq>",
            bind_body
        )).await
    }

    /// Wait for bind result.
    pub async fn expect_bind_result(&mut self) -> Result<String, std::io::Error> {
        self.read_until("</iq>", DEFAULT_TIMEOUT).await
    }

    /// Send stream close.
    pub async fn send_stream_close(&mut self) -> Result<(), std::io::Error> {
        self.send("</stream:stream>").await
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
                Err(std::io::Error::new(std::io::ErrorKind::NotConnected, "Not connected"))
            }
        }).await.map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "Timeout"))??;

        let data = String::from_utf8_lossy(&buf[..n]).to_string();
        self.buffer.push_str(&data);
        Ok(data)
    }

    /// Read until pattern found.
    pub async fn read_until(&mut self, pattern: &str, timeout_dur: Duration) -> std::io::Result<String> {
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

    /// Take the buffer.
    pub fn take_buffer(&mut self) -> String {
        std::mem::take(&mut self.buffer)
    }

    /// Upgrade to TLS.
    pub async fn upgrade_tls(&mut self, connector: TlsConnector, domain: &str) -> std::io::Result<()> {
        let tcp = self.tcp.take().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::Other, "No TCP connection or already TLS")
        })?;

        let server_name: rustls::pki_types::ServerName<'static> = domain.to_string().try_into().map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "Invalid server name")
        })?;

        let tls = connector.connect(server_name, tcp).await?;
        self.tls = Some(tls);
        self.buffer.clear();
        Ok(())
    }

    /// Check if using TLS.
    pub fn is_tls(&self) -> bool {
        self.tls.is_some()
    }
}

/// Helper to encode SASL PLAIN credentials.
pub fn encode_sasl_plain(jid: &str, password: &str) -> String {
    let data = format!("\0{}\0{}", jid, password);
    BASE64_STANDARD.encode(data.as_bytes())
}

/// Helper to validate stream header attributes.
pub fn validate_stream_header(response: &str) -> Result<(), String> {
    // Check for required xmlns
    if !response.contains("xmlns='jabber:client'") && !response.contains("xmlns=\"jabber:client\"") {
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
