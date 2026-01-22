//! Server-to-Server (S2S) federation.
//!
//! This module implements S2S federation for XMPP, including:
//! - TCP listener on port 5269 for incoming S2S connections
//! - TLS 1.3 for secure inter-server communication
//! - Stream negotiation for S2S
//! - Server dialback (XEP-0220)
//! - DNS SRV record discovery for `_xmpp-server._tcp.{domain}`
//! - Remote JID routing - planned
//!
//! # Architecture
//!
//! S2S connections work differently from C2S:
//! - Authentication uses Server Dialback (XEP-0220) or SASL EXTERNAL with certificates
//! - Stream namespace is `jabber:server` instead of `jabber:client`
//! - Both inbound and outbound connection pools are maintained
//!
//! # Usage
//!
//! S2S is enabled by setting `WADDLE_XMPP_S2S_ENABLED=true` environment variable
//! and optionally `WADDLE_XMPP_S2S_ADDR` to customize the bind address.

pub mod connection;
pub mod dialback;
pub mod dns;
pub mod listener;

use std::sync::atomic::{AtomicI64, Ordering};

pub use connection::S2sConnectionActor;
pub use dialback::{DialbackKey, DialbackResult, DialbackState, NS_DIALBACK, NS_DIALBACK_FEATURES};
pub use dns::{DnsError, ResolvedTarget, SrvResolver, DEFAULT_S2S_PORT};
pub use listener::{S2sListener, S2sListenerConfig};

/// S2S connection state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum S2sState {
    /// Initial connection (before TLS)
    Initial,
    /// Dialback in progress
    Dialback,
    /// Authenticated and ready for stanza routing
    Established,
    /// Connection closed
    Closed,
}

/// S2S connection direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum S2sDirection {
    /// Inbound connection (remote server connected to us)
    Inbound,
    /// Outbound connection (we connected to remote server)
    Outbound,
}

/// Metrics for S2S connections.
///
/// Thread-safe metrics tracking for S2S listener and connections.
#[derive(Debug)]
pub struct S2sMetrics {
    /// Total connection attempts received
    connection_attempts: AtomicI64,
    /// Current active connections
    active_connections: AtomicI64,
    /// Total TLS handshakes completed
    tls_established: AtomicI64,
    /// Total connections that reached established state
    connections_established: AtomicI64,
}

impl S2sMetrics {
    /// Create a new metrics instance.
    pub fn new() -> Self {
        Self {
            connection_attempts: AtomicI64::new(0),
            active_connections: AtomicI64::new(0),
            tls_established: AtomicI64::new(0),
            connections_established: AtomicI64::new(0),
        }
    }

    /// Record that the listener has started.
    pub fn record_listener_start(&self) {
        tracing::info!("S2S listener started");
    }

    /// Record an incoming connection attempt.
    pub fn record_connection_attempt(&self) {
        self.connection_attempts.fetch_add(1, Ordering::Relaxed);
        crate::metrics::record_s2s_connection_attempt();
    }

    /// Record a connection being established (post-auth).
    pub fn record_connection_established(&self) {
        self.active_connections.fetch_add(1, Ordering::Relaxed);
        self.connections_established.fetch_add(1, Ordering::Relaxed);
        crate::metrics::record_s2s_connection_count(self.active_connections());
    }

    /// Record a connection being closed.
    pub fn record_connection_closed(&self) {
        self.active_connections.fetch_sub(1, Ordering::Relaxed);
        crate::metrics::record_s2s_connection_count(self.active_connections());
    }

    /// Record TLS being established.
    pub fn record_tls_established(&self) {
        self.tls_established.fetch_add(1, Ordering::Relaxed);
        crate::metrics::record_s2s_tls_established();
    }

    /// Get the current number of active connections.
    pub fn active_connections(&self) -> i64 {
        self.active_connections.load(Ordering::Relaxed)
    }

    /// Get the total number of connection attempts.
    pub fn total_connection_attempts(&self) -> i64 {
        self.connection_attempts.load(Ordering::Relaxed)
    }

    /// Get the total number of TLS handshakes completed.
    pub fn total_tls_established(&self) -> i64 {
        self.tls_established.load(Ordering::Relaxed)
    }
}

impl Default for S2sMetrics {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_s2s_state() {
        let state = S2sState::Initial;
        assert_eq!(state, S2sState::Initial);

        let state = S2sState::Dialback;
        assert_eq!(state, S2sState::Dialback);

        let state = S2sState::Established;
        assert_eq!(state, S2sState::Established);

        let state = S2sState::Closed;
        assert_eq!(state, S2sState::Closed);
    }

    #[test]
    fn test_s2s_direction() {
        assert_eq!(S2sDirection::Inbound, S2sDirection::Inbound);
        assert_ne!(S2sDirection::Inbound, S2sDirection::Outbound);
    }

    #[test]
    fn test_s2s_metrics() {
        let metrics = S2sMetrics::new();

        assert_eq!(metrics.active_connections(), 0);
        assert_eq!(metrics.total_connection_attempts(), 0);

        metrics.record_connection_attempt();
        assert_eq!(metrics.total_connection_attempts(), 1);

        metrics.record_connection_established();
        assert_eq!(metrics.active_connections(), 1);

        metrics.record_connection_closed();
        assert_eq!(metrics.active_connections(), 0);
    }
}
