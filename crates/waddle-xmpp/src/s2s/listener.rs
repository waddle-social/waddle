//! S2S (Server-to-Server) listener for XMPP federation.
//!
//! Handles incoming connections from remote XMPP servers on port 5269.
//! Implements RFC 6120 and TLS 1.3 for secure inter-server communication.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tracing::{info, info_span, warn, Instrument};

use crate::s2s::connection::S2sConnectionActor;
use crate::s2s::S2sMetrics;
use crate::XmppError;

/// S2S listener configuration.
#[derive(Debug, Clone)]
pub struct S2sListenerConfig {
    /// Address to bind for S2S connections (default: 0.0.0.0:5269)
    pub addr: SocketAddr,
    /// Server domain (e.g., "waddle.social")
    pub domain: String,
}

impl Default for S2sListenerConfig {
    fn default() -> Self {
        Self {
            addr: "0.0.0.0:5269".parse().unwrap(),
            domain: "localhost".to_string(),
        }
    }
}

/// S2S listener accepting connections from remote XMPP servers.
pub struct S2sListener {
    config: S2sListenerConfig,
    tls_acceptor: TlsAcceptor,
    metrics: Arc<S2sMetrics>,
}

impl S2sListener {
    /// Create a new S2S listener with the given configuration.
    pub fn new(
        config: S2sListenerConfig,
        tls_acceptor: TlsAcceptor,
    ) -> Self {
        Self {
            config,
            tls_acceptor,
            metrics: Arc::new(S2sMetrics::new()),
        }
    }

    /// Start listening for incoming S2S connections.
    ///
    /// This method runs indefinitely, accepting connections and spawning
    /// connection actors to handle each one.
    pub async fn run(self) -> Result<(), XmppError> {
        let listener = TcpListener::bind(&self.config.addr).await?;
        info!(addr = %self.config.addr, "XMPP S2S server listening");

        // Initialize metrics
        self.metrics.record_listener_start();

        loop {
            match listener.accept().await {
                Ok((stream, peer_addr)) => {
                    let tls_acceptor = self.tls_acceptor.clone();
                    let domain = self.config.domain.clone();
                    let metrics = Arc::clone(&self.metrics);

                    // Record incoming connection
                    metrics.record_connection_attempt();

                    tokio::spawn(
                        async move {
                            if let Err(e) = S2sConnectionActor::handle_connection(
                                stream,
                                peer_addr,
                                tls_acceptor,
                                domain,
                                metrics,
                            )
                            .await
                            {
                                warn!(error = %e, peer = %peer_addr, "S2S connection error");
                            }
                        }
                        .instrument(info_span!(
                            "xmpp.s2s.connection.lifecycle",
                            peer_ip = %peer_addr,
                            transport = "tcp+tls",
                            remote_domain = tracing::field::Empty,
                        )),
                    );
                }
                Err(e) => {
                    warn!(error = %e, "Failed to accept S2S connection");
                }
            }
        }
    }

    /// Get the listener's metrics.
    pub fn metrics(&self) -> &Arc<S2sMetrics> {
        &self.metrics
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = S2sListenerConfig::default();
        assert_eq!(config.addr.port(), 5269);
        assert_eq!(config.domain, "localhost");
    }

    #[test]
    fn test_custom_config() {
        let config = S2sListenerConfig {
            addr: "127.0.0.1:15269".parse().unwrap(),
            domain: "example.com".to_string(),
        };
        assert_eq!(config.addr.port(), 15269);
        assert_eq!(config.domain, "example.com");
    }
}
