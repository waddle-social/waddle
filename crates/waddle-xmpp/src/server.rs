//! XMPP server implementation.
//!
//! The server listens on TCP port 5222 for client-to-server (C2S) connections
//! and optionally on port 5269 for server-to-server (S2S) federation.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tracing::{info, info_span, warn, Instrument};

use crate::connection::ConnectionActor;
use crate::isr::{create_shared_store, SharedIsrTokenStore};
use crate::mam::LibSqlMamStorage;
use crate::muc::MucRoomRegistry;
use crate::registry::ConnectionRegistry;
use crate::s2s::{S2sListener, S2sListenerConfig};
use crate::{AppState, XmppError};

/// XMPP server configuration.
#[derive(Debug, Clone)]
pub struct XmppServerConfig {
    /// Address to bind for C2S connections (default: 0.0.0.0:5222)
    pub c2s_addr: SocketAddr,
    /// Address to bind for S2S connections (default: 0.0.0.0:5269)
    pub s2s_addr: Option<SocketAddr>,
    /// Whether S2S federation is enabled (default: false)
    /// When enabled, the server listens on s2s_addr for incoming S2S connections.
    pub s2s_enabled: bool,
    /// TLS certificate path (PEM format)
    pub tls_cert_path: String,
    /// TLS private key path (PEM format)
    pub tls_key_path: String,
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
}

impl Default for XmppServerConfig {
    fn default() -> Self {
        Self {
            c2s_addr: "0.0.0.0:5222".parse().unwrap(),
            s2s_addr: Some("0.0.0.0:5269".parse().unwrap()),
            s2s_enabled: false, // S2S disabled by default
            tls_cert_path: "certs/server.crt".to_string(),
            tls_key_path: "certs/server.key".to_string(),
            domain: "localhost".to_string(),
            mam_db_path: None, // In-memory by default
            native_auth_enabled: true,
            registration_enabled: false, // Disabled by default for security
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
    ///
    /// When S2S is enabled, this runs both C2S and S2S listeners concurrently.
    /// Either listener failing will return an error.
    pub async fn run(self) -> Result<(), XmppError> {
        // Start S2S listener if enabled
        let s2s_handle = if self.config.s2s_enabled {
            let s2s_addr = self.config.s2s_addr
                .unwrap_or_else(|| "0.0.0.0:5269".parse().unwrap());

            let s2s_config = S2sListenerConfig {
                addr: s2s_addr,
                domain: self.config.domain.clone(),
            };

            let s2s_listener = S2sListener::new(s2s_config, self.tls_acceptor.clone());

            info!(
                addr = %s2s_addr,
                domain = %self.config.domain,
                "S2S federation enabled"
            );

            Some(tokio::spawn(async move {
                s2s_listener.run().await
            }))
        } else {
            info!("S2S federation disabled");
            None
        };

        // Start C2S listener
        let c2s_handle = {
            let listener = TcpListener::bind(&self.config.c2s_addr).await?;
            info!(addr = %self.config.c2s_addr, "XMPP C2S server listening");

            let app_state = self.app_state;
            let tls_acceptor = self.tls_acceptor;
            let domain = self.config.domain;
            let room_registry = self.room_registry;
            let connection_registry = self.connection_registry;
            let mam_storage = self.mam_storage;
            let isr_token_store = self.isr_token_store;
            let registration_enabled = self.config.registration_enabled;

            tokio::spawn(async move {
                loop {
                    let (stream, peer_addr) = match listener.accept().await {
                        Ok(conn) => conn,
                        Err(e) => {
                            warn!(error = %e, "Failed to accept C2S connection");
                            continue;
                        }
                    };

                    let app_state = Arc::clone(&app_state);
                    let tls_acceptor = tls_acceptor.clone();
                    let domain = domain.clone();
                    let room_registry = Arc::clone(&room_registry);
                    let connection_registry = Arc::clone(&connection_registry);
                    let mam_storage = Arc::clone(&mam_storage);
                    let isr_token_store = Arc::clone(&isr_token_store);

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
                                    registration_enabled,
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
            })
        };

        // Wait for either listener to complete (or error)
        tokio::select! {
            result = c2s_handle => {
                match result {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(e)) => Err(e),
                    Err(e) => Err(XmppError::internal(format!("C2S listener task failed: {}", e))),
                }
            }
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
