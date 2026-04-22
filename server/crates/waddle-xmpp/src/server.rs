//! XMPP listener implementation.
//!
//! The standalone listener only handles optional server-to-server (S2S)
//! federation. Client-to-server traffic is served by the HTTP/WebSocket stack.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use rustls::ServerConfig as RustlsServerConfig;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tracing::info;
use waddle_extensions::ExtensionConfig;

use crate::registry::ConnectionRegistry;
use crate::routing::{RouterConfig, StanzaRouter};
use crate::s2s::{S2sListener, S2sListenerConfig};
use crate::{AppState, XmppError};

/// XMPP server configuration.
#[derive(Debug, Clone)]
pub struct XmppServerConfig {
    /// Address to bind for S2S connections (default: 0.0.0.0:5269)
    pub s2s_addr: Option<SocketAddr>,
    /// Whether S2S federation is enabled (default: false)
    /// When enabled, the server listens on s2s_addr for incoming S2S connections.
    pub s2s_enabled: bool,
    /// TLS certificate path (PEM format)
    pub tls_cert_path: String,
    /// TLS private key path (PEM format)
    pub tls_key_path: String,
    /// Pre-built rustls server config (when set, cert/key paths are ignored)
    pub tls_server_config: Option<Arc<RustlsServerConfig>>,
    /// Server domain (e.g., "waddle.social")
    pub domain: String,
    /// MAM database path (None for in-memory, Some(path) for file-based)
    pub mam_db_path: Option<PathBuf>,
    /// Whether native JID authentication is enabled (default: true)
    /// When enabled, users can authenticate with SCRAM-SHA-256 using native credentials.
    pub native_auth_enabled: bool,
    /// Whether XEP-0077 In-Band Registration is enabled (default: false)
    /// When enabled, users can register new accounts before authentication.
    /// Security note: Enable with caution on public servers.
    pub registration_enabled: bool,
    /// Whether the server operates in single-tenant mode (default: false).
    /// When true, all spaces are publicly discoverable regardless of membership.
    /// Controlled by `WADDLE_SINGLE_TENANT` env var.
    pub single_tenant: bool,
    /// Runtime extension configuration.
    pub extensions: ExtensionConfig,
}

impl Default for XmppServerConfig {
    fn default() -> Self {
        Self {
            s2s_addr: Some("0.0.0.0:5269".parse().unwrap()),
            s2s_enabled: false, // S2S disabled by default
            tls_cert_path: "certs/server.crt".to_string(),
            tls_key_path: "certs/server.key".to_string(),
            tls_server_config: None,
            domain: "localhost".to_string(),
            mam_db_path: None, // In-memory by default
            native_auth_enabled: true,
            registration_enabled: false, // Disabled by default for security
            single_tenant: false,
            extensions: ExtensionConfig::default(),
        }
    }
}

/// Generate an ephemeral self-signed TLS configuration for the given domain.
///
/// Uses `rcgen` to create a self-signed certificate in memory with SANs
/// for the given domain and `localhost`. No files are read or written.
pub fn generate_ephemeral_tls_config(domain: &str) -> Result<Arc<RustlsServerConfig>, XmppError> {
    use rcgen::{generate_simple_self_signed, CertifiedKey};
    use rustls_pemfile::{certs, pkcs8_private_keys};
    use std::io::{BufReader, Cursor};
    use tokio_rustls::rustls::pki_types::PrivateKeyDer;

    let mut subject_alt_names = vec![domain.to_string()];
    if domain != "localhost" {
        subject_alt_names.push("localhost".to_string());
    }

    let CertifiedKey { cert, key_pair } = generate_simple_self_signed(subject_alt_names)
        .map_err(|e| XmppError::config(format!("Failed to generate ephemeral certificate: {e}")))?;

    let cert_pem = cert.pem();
    let key_pem = key_pair.serialize_pem();

    let certs: Vec<_> = certs(&mut BufReader::new(Cursor::new(cert_pem.as_bytes())))
        .filter_map(|r| r.ok())
        .collect();

    let keys: Vec<_> = pkcs8_private_keys(&mut BufReader::new(Cursor::new(key_pem.as_bytes())))
        .filter_map(|r| r.ok())
        .collect();

    let key = keys
        .into_iter()
        .next()
        .ok_or_else(|| XmppError::config("No private key in generated ephemeral cert"))?;

    let server_config = RustlsServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, PrivateKeyDer::Pkcs8(key))
        .map_err(|e| XmppError::config(format!("Ephemeral TLS config error: {e}")))?;

    Ok(Arc::new(server_config))
}

/// XMPP server instance.
pub struct XmppServer<S: AppState> {
    config: XmppServerConfig,
    // Tie the server handle to the embedding app-state type even though the
    // standalone listener only uses S2S-only state today.
    _app_state_marker: std::marker::PhantomData<S>,
    tls_acceptor: TlsAcceptor,
    connection_registry: Arc<ConnectionRegistry>,
    /// S2S listener — passed in if S2S federation is enabled.
    s2s_listener: Option<TcpListener>,
    /// Shutdown token — when cancelled, the accept loop stops.
    shutdown_token: tokio_util::sync::CancellationToken,
}

impl<S: AppState> XmppServer<S> {
    /// Create a new XMPP server instance.
    ///
    /// Requires a shutdown token.
    pub async fn new(
        config: XmppServerConfig,
        _app_state: Arc<S>,
        _c2s_listener: Option<TcpListener>,
        s2s_listener: Option<TcpListener>,
        shutdown_token: tokio_util::sync::CancellationToken,
    ) -> Result<Self, XmppError> {
        let tls_acceptor = Self::load_tls_config(&config)?;

        // Create the connection registry for message routing
        let connection_registry = Arc::new(ConnectionRegistry::new());

        Ok(Self {
            config,
            _app_state_marker: std::marker::PhantomData,
            tls_acceptor,
            connection_registry,
            s2s_listener,
            shutdown_token,
        })
    }

    /// Load TLS configuration from certificate and key files.
    fn load_tls_config(config: &XmppServerConfig) -> Result<TlsAcceptor, XmppError> {
        if let Some(server_config) = &config.tls_server_config {
            return Ok(TlsAcceptor::from(server_config.clone()));
        }

        use rustls_pemfile::{certs, read_one, Item};
        use std::fs::File;
        use std::io::BufReader;
        use tokio_rustls::rustls::pki_types::PrivateKeyDer;

        let cert_file = File::open(&config.tls_cert_path).map_err(|e| {
            XmppError::config(format!(
                "Failed to open cert file {}: {}",
                config.tls_cert_path, e
            ))
        })?;
        let key_file = File::open(&config.tls_key_path).map_err(|e| {
            XmppError::config(format!(
                "Failed to open key file {}: {}",
                config.tls_key_path, e
            ))
        })?;

        let certs: Vec<_> = certs(&mut BufReader::new(cert_file))
            .filter_map(|r| r.ok())
            .collect();

        let mut key_reader = BufReader::new(key_file);
        let mut key: Option<PrivateKeyDer<'static>> = None;
        loop {
            let Some(item) = read_one(&mut key_reader)
                .map_err(|e| XmppError::config(format!("Failed to read key file: {}", e)))?
            else {
                break;
            };

            key = match item {
                Item::Pkcs8Key(pkcs8) => Some(PrivateKeyDer::Pkcs8(pkcs8)),
                Item::Pkcs1Key(pkcs1) => Some(PrivateKeyDer::Pkcs1(pkcs1)),
                Item::Sec1Key(sec1) => Some(PrivateKeyDer::Sec1(sec1)),
                _ => None,
            };

            if key.is_some() {
                break;
            }
        }

        let key = key.ok_or_else(|| XmppError::config("No private key found"))?;

        let server_config = RustlsServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|e| XmppError::config(format!("TLS config error: {}", e)))?;

        Ok(TlsAcceptor::from(Arc::new(server_config)))
    }

    /// Start the XMPP listener tasks.
    ///
    /// When S2S is enabled, this runs the federation listener.
    pub async fn run(self) -> Result<(), XmppError> {
        // Start S2S listener if enabled and listener was provided
        let s2s_handle = if let Some(s2s_tcp_listener) = self.s2s_listener {
            let s2s_addr = self
                .config
                .s2s_addr
                .unwrap_or_else(|| "0.0.0.0:5269".parse().unwrap());

            // Generate a dialback secret for this server instance
            let mut dialback_secret = vec![0u8; 32];
            {
                use rand::RngCore;
                rand::rng().fill_bytes(&mut dialback_secret);
            }

            let s2s_config = S2sListenerConfig {
                addr: s2s_addr,
                domain: self.config.domain.clone(),
                dialback_secret,
            };

            // Create a StanzaRouter for routing inbound S2S stanzas to local users
            let router_config = RouterConfig::new(self.config.domain.clone()).with_federation(true);
            let stanza_router = Arc::new(StanzaRouter::new(
                router_config,
                Arc::clone(&self.connection_registry),
                None, // S2S pool not needed for inbound routing
            ));

            let s2s_listener = S2sListener::new(
                s2s_config,
                self.tls_acceptor.clone(),
                s2s_tcp_listener,
                self.shutdown_token.clone(),
            )
            .with_stanza_router(stanza_router);

            info!(
                addr = %s2s_addr,
                domain = %self.config.domain,
                "S2S federation enabled"
            );

            Some(tokio::spawn(async move { s2s_listener.run().await }))
        } else {
            info!("S2S federation disabled");
            None
        };

        // Wait for the federation listener to complete (or error)
        tokio::select! {
            result = async {
                match s2s_handle {
                    Some(handle) => handle.await,
                    None => std::future::pending().await,
                }
            } => {
                match result {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(e)) => Err(e),
                    Err(e) => Err(XmppError::internal(format!("S2S listener task failed: {}", e))),
                }
            }
        }
    }

    /// Get the server configuration.
    pub fn config(&self) -> &XmppServerConfig {
        &self.config
    }
}
