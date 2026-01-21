//! XMPP server implementation.
//!
//! The server listens on TCP port 5222 for client-to-server (C2S) connections.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tracing::{info, info_span, Instrument};

use crate::connection::ConnectionActor;
use crate::isr::{create_shared_store, SharedIsrTokenStore};
use crate::mam::LibSqlMamStorage;
use crate::muc::MucRoomRegistry;
use crate::registry::ConnectionRegistry;
use crate::{AppState, XmppError};

/// XMPP server configuration.
#[derive(Debug, Clone)]
pub struct XmppServerConfig {
    /// Address to bind for C2S connections (default: 0.0.0.0:5222)
    pub c2s_addr: SocketAddr,
    /// Address to bind for S2S connections (default: 0.0.0.0:5269)
    pub s2s_addr: Option<SocketAddr>,
    /// TLS certificate path (PEM format)
    pub tls_cert_path: String,
    /// TLS private key path (PEM format)
    pub tls_key_path: String,
    /// Server domain (e.g., "waddle.social")
    pub domain: String,
    /// MAM database path (None for in-memory, Some(path) for file-based)
    pub mam_db_path: Option<PathBuf>,
}

impl Default for XmppServerConfig {
    fn default() -> Self {
        Self {
            c2s_addr: "0.0.0.0:5222".parse().unwrap(),
            s2s_addr: None,
            tls_cert_path: "certs/server.crt".to_string(),
            tls_key_path: "certs/server.key".to_string(),
            domain: "localhost".to_string(),
            mam_db_path: None, // In-memory by default
        }
    }
}

/// XMPP server instance.
pub struct XmppServer<S: AppState> {
    config: XmppServerConfig,
    app_state: Arc<S>,
    tls_acceptor: TlsAcceptor,
    room_registry: Arc<MucRoomRegistry>,
    connection_registry: Arc<ConnectionRegistry>,
    mam_storage: Arc<LibSqlMamStorage>,
    /// XEP-0397 ISR token store shared across all connections
    isr_token_store: SharedIsrTokenStore,
}

impl<S: AppState> XmppServer<S> {
    /// Create a new XMPP server instance.
    pub async fn new(config: XmppServerConfig, app_state: Arc<S>) -> Result<Self, XmppError> {
        let tls_acceptor = Self::load_tls_config(&config)?;

        // Create the MUC room registry with the MUC domain
        let muc_domain = format!("muc.{}", config.domain);
        let room_registry = Arc::new(MucRoomRegistry::new(muc_domain));

        // Create the connection registry for message routing
        let connection_registry = Arc::new(ConnectionRegistry::new());

        // Create the MAM storage
        let mam_storage = Self::create_mam_storage(&config).await?;

        // Create the ISR token store for instant stream resumption (XEP-0397)
        let isr_token_store = create_shared_store();
        info!("ISR token store initialized");

        Ok(Self {
            config,
            app_state,
            tls_acceptor,
            room_registry,
            connection_registry,
            mam_storage,
            isr_token_store,
        })
    }

    /// Create MAM storage from configuration.
    async fn create_mam_storage(config: &XmppServerConfig) -> Result<Arc<LibSqlMamStorage>, XmppError> {
        let db_path = config
            .mam_db_path
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| ":memory:".to_string());

        let db = libsql::Builder::new_local(&db_path)
            .build()
            .await
            .map_err(|e| XmppError::config(format!("Failed to create MAM database: {}", e)))?;

        let conn = db
            .connect()
            .map_err(|e| XmppError::config(format!("Failed to connect to MAM database: {}", e)))?;

        let storage = LibSqlMamStorage::new(conn);

        // Initialize the schema
        storage
            .initialize()
            .await
            .map_err(|e| XmppError::config(format!("Failed to initialize MAM schema: {}", e)))?;

        info!(db_path = %db_path, "MAM storage initialized");

        Ok(Arc::new(storage))
    }

    /// Load TLS configuration from certificate and key files.
    fn load_tls_config(config: &XmppServerConfig) -> Result<TlsAcceptor, XmppError> {
        use rustls_pemfile::{certs, pkcs8_private_keys};
        use std::fs::File;
        use std::io::BufReader;
        use tokio_rustls::rustls::{pki_types::PrivateKeyDer, ServerConfig};

        let cert_file = File::open(&config.tls_cert_path).map_err(|e| {
            XmppError::config(format!("Failed to open cert file {}: {}", config.tls_cert_path, e))
        })?;
        let key_file = File::open(&config.tls_key_path).map_err(|e| {
            XmppError::config(format!("Failed to open key file {}: {}", config.tls_key_path, e))
        })?;

        let certs: Vec<_> = certs(&mut BufReader::new(cert_file))
            .filter_map(|r| r.ok())
            .collect();

        let keys: Vec<_> = pkcs8_private_keys(&mut BufReader::new(key_file))
            .filter_map(|r| r.ok())
            .collect();

        let key = keys
            .into_iter()
            .next()
            .ok_or_else(|| XmppError::config("No private key found"))?;

        let server_config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, PrivateKeyDer::Pkcs8(key))
            .map_err(|e| XmppError::config(format!("TLS config error: {}", e)))?;

        Ok(TlsAcceptor::from(Arc::new(server_config)))
    }

    /// Start the XMPP server and listen for connections.
    pub async fn run(self) -> Result<(), XmppError> {
        let listener = TcpListener::bind(&self.config.c2s_addr).await?;
        info!(addr = %self.config.c2s_addr, "XMPP C2S server listening");

        loop {
            let (stream, peer_addr) = listener.accept().await?;

            let app_state = Arc::clone(&self.app_state);
            let tls_acceptor = self.tls_acceptor.clone();
            let domain = self.config.domain.clone();
            let room_registry = Arc::clone(&self.room_registry);
            let connection_registry = Arc::clone(&self.connection_registry);
            let mam_storage = Arc::clone(&self.mam_storage);
            let isr_token_store = Arc::clone(&self.isr_token_store);

            tokio::spawn(
                async move {
                    if let Err(e) =
                        ConnectionActor::handle_connection(
                            stream,
                            peer_addr,
                            tls_acceptor,
                            domain.clone(),
                            app_state,
                            room_registry,
                            connection_registry,
                            mam_storage,
                            isr_token_store,
                        )
                        .await
                    {
                        tracing::warn!(error = %e, "Connection error");
                    }
                }
                .instrument(info_span!(
                    "xmpp.connection.lifecycle",
                    client_ip = %peer_addr,
                    transport = "tcp+tls",
                    jid = tracing::field::Empty,  // Set later during authentication
                )),
            );
        }
    }

    /// Get the server configuration.
    pub fn config(&self) -> &XmppServerConfig {
        &self.config
    }

    /// Get the room registry.
    pub fn room_registry(&self) -> &Arc<MucRoomRegistry> {
        &self.room_registry
    }

    /// Get the connection registry.
    pub fn connection_registry(&self) -> &Arc<ConnectionRegistry> {
        &self.connection_registry
    }

    /// Get the MAM storage.
    pub fn mam_storage(&self) -> &Arc<LibSqlMamStorage> {
        &self.mam_storage
    }
}
