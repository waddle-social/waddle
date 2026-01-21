//! XMPP server implementation.
//!
//! The server listens on TCP port 5222 for client-to-server (C2S) connections.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tracing::{info, info_span, Instrument};

use crate::connection::ConnectionActor;
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
}

impl Default for XmppServerConfig {
    fn default() -> Self {
        Self {
            c2s_addr: "0.0.0.0:5222".parse().unwrap(),
            s2s_addr: None,
            tls_cert_path: "certs/server.crt".to_string(),
            tls_key_path: "certs/server.key".to_string(),
            domain: "localhost".to_string(),
        }
    }
}

/// XMPP server instance.
pub struct XmppServer<S: AppState> {
    config: XmppServerConfig,
    app_state: Arc<S>,
    tls_acceptor: TlsAcceptor,
}

impl<S: AppState> XmppServer<S> {
    /// Create a new XMPP server instance.
    pub async fn new(config: XmppServerConfig, app_state: Arc<S>) -> Result<Self, XmppError> {
        let tls_acceptor = Self::load_tls_config(&config)?;

        Ok(Self {
            config,
            app_state,
            tls_acceptor,
        })
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

            tokio::spawn(
                async move {
                    if let Err(e) =
                        ConnectionActor::handle_connection(stream, peer_addr, tls_acceptor, domain.clone(), app_state)
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
}
