//! S2S Connection Pool for outbound federation connections.
//!
//! This module provides a connection pool for managing outbound connections
//! to remote XMPP servers. It handles:
//! - Connection reuse (multiple requests to the same domain use the same connection)
//! - Connection health checking
//! - Automatic cleanup of idle or unhealthy connections
//! - DNS SRV resolution for target discovery
//!
//! # Architecture
//!
//! The pool uses a `DashMap` for lock-free concurrent access to connections
//! keyed by remote domain. Each pool entry tracks connection state, last activity,
//! and health status.
//!
//! # Example
//!
//! ```ignore
//! use waddle_xmpp::s2s::pool::{S2sConnectionPool, S2sPoolConfig};
//!
//! let config = S2sPoolConfig::default();
//! let pool = S2sConnectionPool::new(config, local_domain, tls_connector, dialback_secret).await?;
//!
//! // Get or create a connection to a remote server
//! let conn = pool.get_or_connect("example.com").await?;
//!
//! // Send a stanza through the connection
//! conn.send_stanza(&stanza).await?;
//! ```

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use thiserror::Error;
use tokio::sync::RwLock;
use tracing::{debug, info, instrument, warn};

use crate::s2s::dns::{DnsError, SrvResolver};
use crate::s2s::outbound::{OutboundConnectionError, S2sOutboundConfig, S2sOutboundConnection};

/// Default maximum connections per domain.
pub const DEFAULT_MAX_CONNECTIONS_PER_DOMAIN: usize = 2;

/// Default connection idle timeout in seconds.
pub const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 300; // 5 minutes

/// Default health check interval in seconds.
pub const DEFAULT_HEALTH_CHECK_INTERVAL_SECS: u64 = 60;

/// Default connection timeout in seconds.
pub const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 30;

/// Errors that can occur in the S2S connection pool.
#[derive(Debug, Error)]
pub enum S2sPoolError {
    /// DNS resolution failed
    #[error("DNS resolution failed: {0}")]
    DnsResolution(#[from] DnsError),

    /// Connection failed
    #[error("Connection failed: {0}")]
    ConnectionFailed(#[from] OutboundConnectionError),

    /// No healthy connections available
    #[error("No healthy connections available to {domain}")]
    NoHealthyConnections { domain: String },

    /// Pool is shutting down
    #[error("Connection pool is shutting down")]
    Shutdown,

    /// Connection timeout
    #[error("Connection to {domain} timed out")]
    Timeout { domain: String },

    /// Internal error
    #[error("Internal pool error: {0}")]
    Internal(String),
}

/// Configuration for the S2S connection pool.
#[derive(Debug, Clone)]
pub struct S2sPoolConfig {
    /// Maximum connections per remote domain.
    pub max_connections_per_domain: usize,

    /// Idle timeout after which connections are closed.
    pub idle_timeout: Duration,

    /// Interval for health checking connections.
    pub health_check_interval: Duration,

    /// Timeout for establishing new connections.
    pub connect_timeout: Duration,

    /// Whether to use DNS SRV records for discovery.
    pub use_dns_srv: bool,

    /// Retry configuration for failed connections.
    pub retry_config: RetryConfig,
}

impl Default for S2sPoolConfig {
    fn default() -> Self {
        Self {
            max_connections_per_domain: DEFAULT_MAX_CONNECTIONS_PER_DOMAIN,
            idle_timeout: Duration::from_secs(DEFAULT_IDLE_TIMEOUT_SECS),
            health_check_interval: Duration::from_secs(DEFAULT_HEALTH_CHECK_INTERVAL_SECS),
            connect_timeout: Duration::from_secs(DEFAULT_CONNECT_TIMEOUT_SECS),
            use_dns_srv: true,
            retry_config: RetryConfig::default(),
        }
    }
}

/// Configuration for connection retry behavior.
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Maximum number of retry attempts.
    pub max_attempts: u32,

    /// Initial delay between retries.
    pub initial_delay: Duration,

    /// Maximum delay between retries (for exponential backoff).
    pub max_delay: Duration,

    /// Multiplier for exponential backoff.
    pub backoff_multiplier: f64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(30),
            backoff_multiplier: 2.0,
        }
    }
}

/// State of a pooled connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PooledConnectionState {
    /// Connection is being established
    Connecting,
    /// Connection is healthy and ready
    Ready,
    /// Connection is unhealthy but may recover
    Unhealthy,
    /// Connection is closed
    Closed,
}

/// A pooled connection entry.
pub struct PooledConnection {
    /// The underlying outbound connection.
    pub connection: S2sOutboundConnection,

    /// Current state of this connection.
    pub state: PooledConnectionState,

    /// When the connection was created.
    pub created_at: Instant,

    /// Last activity time (for idle timeout).
    pub last_activity: Instant,

    /// Number of stanzas sent through this connection.
    pub stanzas_sent: u64,

    /// Number of errors encountered.
    pub error_count: u32,
}

impl PooledConnection {
    /// Create a new pooled connection.
    pub fn new(connection: S2sOutboundConnection) -> Self {
        let now = Instant::now();
        Self {
            connection,
            state: PooledConnectionState::Ready,
            created_at: now,
            last_activity: now,
            stanzas_sent: 0,
            error_count: 0,
        }
    }

    /// Check if the connection is healthy.
    pub fn is_healthy(&self) -> bool {
        self.state == PooledConnectionState::Ready && self.connection.is_connected()
    }

    /// Check if the connection is idle (exceeded idle timeout).
    pub fn is_idle(&self, idle_timeout: Duration) -> bool {
        self.last_activity.elapsed() > idle_timeout
    }

    /// Record activity on this connection.
    pub fn record_activity(&mut self) {
        self.last_activity = Instant::now();
        self.stanzas_sent += 1;
    }

    /// Record an error on this connection.
    pub fn record_error(&mut self) {
        self.error_count += 1;
        if self.error_count >= 3 {
            self.state = PooledConnectionState::Unhealthy;
        }
    }
}

/// Entry in the connection pool for a specific domain.
pub struct DomainPoolEntry {
    /// The remote domain.
    pub domain: String,

    /// Active connections to this domain.
    pub connections: Vec<PooledConnection>,

    /// Whether a connection attempt is in progress.
    pub connecting: bool,

    /// Time of last connection attempt.
    pub last_connect_attempt: Option<Instant>,

    /// Consecutive connection failures (for backoff).
    pub consecutive_failures: u32,
}

impl DomainPoolEntry {
    /// Create a new domain pool entry.
    pub fn new(domain: String) -> Self {
        Self {
            domain,
            connections: Vec::new(),
            connecting: false,
            last_connect_attempt: None,
            consecutive_failures: 0,
        }
    }

    /// Get a healthy connection from this entry.
    pub fn get_healthy_connection(&mut self) -> Option<&mut PooledConnection> {
        self.connections.iter_mut().find(|c| c.is_healthy())
    }

    /// Count healthy connections.
    pub fn healthy_count(&self) -> usize {
        self.connections.iter().filter(|c| c.is_healthy()).count()
    }

    /// Remove closed connections.
    pub fn remove_closed(&mut self) {
        self.connections
            .retain(|c| c.state != PooledConnectionState::Closed);
    }

    /// Record a successful connection.
    pub fn record_connect_success(&mut self) {
        self.connecting = false;
        self.consecutive_failures = 0;
    }

    /// Record a failed connection attempt.
    pub fn record_connect_failure(&mut self) {
        self.connecting = false;
        self.consecutive_failures += 1;
        self.last_connect_attempt = Some(Instant::now());
    }

    /// Calculate backoff duration for retry.
    pub fn backoff_duration(&self, config: &RetryConfig) -> Duration {
        if self.consecutive_failures == 0 {
            return Duration::ZERO;
        }

        let delay = config.initial_delay.as_secs_f64()
            * config
                .backoff_multiplier
                .powi(self.consecutive_failures as i32 - 1);
        Duration::from_secs_f64(delay.min(config.max_delay.as_secs_f64()))
    }

    /// Check if we should wait before retrying.
    pub fn should_wait_for_retry(&self, config: &RetryConfig) -> bool {
        if let Some(last_attempt) = self.last_connect_attempt {
            let backoff = self.backoff_duration(config);
            last_attempt.elapsed() < backoff
        } else {
            false
        }
    }
}

/// S2S Connection Pool for managing outbound federation connections.
///
/// Thread-safe pool using `DashMap` for concurrent access without explicit locking.
pub struct S2sConnectionPool {
    /// Pool configuration.
    config: S2sPoolConfig,

    /// Local server domain.
    local_domain: String,

    /// Outbound connection configuration.
    outbound_config: Arc<S2sOutboundConfig>,

    /// DNS resolver for SRV lookups.
    resolver: SrvResolver,

    /// Connection entries by domain.
    connections: DashMap<String, Arc<RwLock<DomainPoolEntry>>>,

    /// Metrics for the pool.
    metrics: Arc<S2sPoolMetrics>,

    /// Shutdown flag.
    shutdown: Arc<std::sync::atomic::AtomicBool>,
}

/// Metrics for the S2S connection pool.
#[derive(Debug)]
pub struct S2sPoolMetrics {
    /// Total connections created.
    pub connections_created: AtomicI64,

    /// Total connections closed.
    pub connections_closed: AtomicI64,

    /// Current active connections.
    pub active_connections: AtomicI64,

    /// Total connection errors.
    pub connection_errors: AtomicI64,

    /// Total stanzas sent through the pool.
    pub stanzas_sent: AtomicU64,

    /// Pool hits (existing connection reused).
    pub pool_hits: AtomicU64,

    /// Pool misses (new connection created).
    pub pool_misses: AtomicU64,
}

impl S2sPoolMetrics {
    /// Create new pool metrics.
    pub fn new() -> Self {
        Self {
            connections_created: AtomicI64::new(0),
            connections_closed: AtomicI64::new(0),
            active_connections: AtomicI64::new(0),
            connection_errors: AtomicI64::new(0),
            stanzas_sent: AtomicU64::new(0),
            pool_hits: AtomicU64::new(0),
            pool_misses: AtomicU64::new(0),
        }
    }

    /// Record a new connection created.
    pub fn record_connection_created(&self) {
        self.connections_created.fetch_add(1, Ordering::Relaxed);
        self.active_connections.fetch_add(1, Ordering::Relaxed);
        self.pool_misses.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a connection closed.
    pub fn record_connection_closed(&self) {
        self.connections_closed.fetch_add(1, Ordering::Relaxed);
        self.active_connections.fetch_sub(1, Ordering::Relaxed);
    }

    /// Record a pool hit (connection reused).
    pub fn record_pool_hit(&self) {
        self.pool_hits.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a connection error.
    pub fn record_connection_error(&self) {
        self.connection_errors.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a stanza sent.
    pub fn record_stanza_sent(&self) {
        self.stanzas_sent.fetch_add(1, Ordering::Relaxed);
    }

    /// Get current active connection count.
    pub fn active_count(&self) -> i64 {
        self.active_connections.load(Ordering::Relaxed)
    }
}

impl Default for S2sPoolMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl S2sConnectionPool {
    /// Create a new S2S connection pool.
    #[instrument(skip(outbound_config), name = "s2s.pool.new")]
    pub async fn new(
        config: S2sPoolConfig,
        local_domain: String,
        outbound_config: S2sOutboundConfig,
    ) -> Result<Self, S2sPoolError> {
        let resolver = SrvResolver::new()
            .await
            .map_err(S2sPoolError::DnsResolution)?;

        info!(
            local_domain = %local_domain,
            max_per_domain = config.max_connections_per_domain,
            idle_timeout_secs = config.idle_timeout.as_secs(),
            "S2S connection pool created"
        );

        Ok(Self {
            config,
            local_domain,
            outbound_config: Arc::new(outbound_config),
            resolver,
            connections: DashMap::new(),
            metrics: Arc::new(S2sPoolMetrics::new()),
            shutdown: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    }

    /// Get or create a connection to the specified domain.
    ///
    /// This is the primary method for obtaining connections from the pool.
    /// It will:
    /// 1. Check for an existing healthy connection
    /// 2. If none available, create a new connection
    /// 3. Return the connection ready for sending stanzas
    #[instrument(skip(self), name = "s2s.pool.get_or_connect", fields(domain = %domain))]
    pub async fn get_or_connect(&self, domain: &str) -> Result<PoolConnectionHandle, S2sPoolError> {
        if self.shutdown.load(Ordering::Relaxed) {
            return Err(S2sPoolError::Shutdown);
        }

        // Get or create the domain entry
        let entry = self
            .connections
            .entry(domain.to_string())
            .or_insert_with(|| Arc::new(RwLock::new(DomainPoolEntry::new(domain.to_string()))))
            .clone();

        // Try to get an existing healthy connection
        {
            let mut entry_guard = entry.write().await;

            // Clean up closed connections first
            entry_guard.remove_closed();

            if let Some(conn) = entry_guard.get_healthy_connection() {
                debug!(domain = %domain, "Reusing existing connection");
                self.metrics.record_pool_hit();
                conn.record_activity();

                return Ok(PoolConnectionHandle {
                    domain: domain.to_string(),
                    pool_metrics: Arc::clone(&self.metrics),
                    connections: self.connections.clone(),
                });
            }

            // Check if we should wait for backoff
            if entry_guard.should_wait_for_retry(&self.config.retry_config) {
                let backoff = entry_guard.backoff_duration(&self.config.retry_config);
                debug!(
                    domain = %domain,
                    backoff_ms = backoff.as_millis(),
                    "Waiting for backoff before retry"
                );
                // We could wait here, but instead we'll return an error
                // to allow the caller to decide
                return Err(S2sPoolError::NoHealthyConnections {
                    domain: domain.to_string(),
                });
            }

            // Check if we're at max connections and all are unhealthy
            if entry_guard.connections.len() >= self.config.max_connections_per_domain {
                return Err(S2sPoolError::NoHealthyConnections {
                    domain: domain.to_string(),
                });
            }

            // Mark that we're connecting
            entry_guard.connecting = true;
        }

        // Create a new connection
        debug!(domain = %domain, "Creating new outbound connection");
        let connection_result = self.create_connection(domain).await;

        // Update entry based on result
        let mut entry_guard = entry.write().await;
        match connection_result {
            Ok(conn) => {
                entry_guard.connections.push(PooledConnection::new(conn));
                entry_guard.record_connect_success();
                self.metrics.record_connection_created();

                info!(
                    domain = %domain,
                    total_connections = entry_guard.connections.len(),
                    "New S2S outbound connection established"
                );

                Ok(PoolConnectionHandle {
                    domain: domain.to_string(),
                    pool_metrics: Arc::clone(&self.metrics),
                    connections: self.connections.clone(),
                })
            }
            Err(e) => {
                entry_guard.record_connect_failure();
                self.metrics.record_connection_error();
                warn!(
                    domain = %domain,
                    error = %e,
                    consecutive_failures = entry_guard.consecutive_failures,
                    "Failed to create S2S outbound connection"
                );
                Err(e)
            }
        }
    }

    /// Create a new outbound connection.
    async fn create_connection(&self, domain: &str) -> Result<S2sOutboundConnection, S2sPoolError> {
        // Resolve targets using DNS
        let targets = if self.config.use_dns_srv {
            self.resolver.resolve_xmpp_server(domain).await?
        } else {
            vec![crate::s2s::dns::ResolvedTarget::fallback(
                domain.to_string(),
            )]
        };

        debug!(
            domain = %domain,
            target_count = targets.len(),
            "Resolved DNS targets for S2S connection"
        );

        // Try each target in priority order
        let mut last_error = None;
        for target in targets {
            debug!(
                domain = %domain,
                host = %target.host,
                port = target.port,
                priority = target.priority,
                "Attempting connection to target"
            );

            match tokio::time::timeout(
                self.config.connect_timeout,
                S2sOutboundConnection::connect(
                    &target.host,
                    target.port,
                    domain,
                    &self.local_domain,
                    (*self.outbound_config).clone(),
                ),
            )
            .await
            {
                Ok(Ok(conn)) => {
                    info!(
                        domain = %domain,
                        target = %target.host,
                        port = target.port,
                        "S2S outbound connection established"
                    );
                    return Ok(conn);
                }
                Ok(Err(e)) => {
                    warn!(
                        domain = %domain,
                        target = %target.host,
                        error = %e,
                        "Failed to connect to target"
                    );
                    last_error = Some(S2sPoolError::ConnectionFailed(e));
                }
                Err(_) => {
                    warn!(
                        domain = %domain,
                        target = %target.host,
                        "Connection attempt timed out"
                    );
                    last_error = Some(S2sPoolError::Timeout {
                        domain: domain.to_string(),
                    });
                }
            }
        }

        Err(last_error.unwrap_or_else(|| {
            S2sPoolError::Internal(format!("No targets available for domain: {}", domain))
        }))
    }

    /// Clean up idle and unhealthy connections.
    ///
    /// This should be called periodically (e.g., from a background task).
    #[instrument(skip(self), name = "s2s.pool.cleanup")]
    pub async fn cleanup(&self) -> usize {
        let mut total_cleaned = 0;

        for entry in self.connections.iter() {
            let mut entry_guard = entry.value().write().await;
            let before_count = entry_guard.connections.len();

            // Remove closed connections
            entry_guard.remove_closed();

            // Remove idle connections
            entry_guard
                .connections
                .retain(|c| !c.is_idle(self.config.idle_timeout));

            let removed = before_count - entry_guard.connections.len();
            if removed > 0 {
                debug!(
                    domain = %entry_guard.domain,
                    removed = removed,
                    "Cleaned up idle/closed connections"
                );
                total_cleaned += removed;

                // Update metrics
                for _ in 0..removed {
                    self.metrics.record_connection_closed();
                }
            }
        }

        // Remove empty entries
        self.connections.retain(|_, _entry| {
            // We need to check if entry is empty in a sync context
            // For now, we'll keep all entries to avoid async in retain
            true
        });

        if total_cleaned > 0 {
            info!(total_cleaned = total_cleaned, "Pool cleanup completed");
        }

        total_cleaned
    }

    /// Perform health checks on all connections.
    #[instrument(skip(self), name = "s2s.pool.health_check")]
    pub async fn health_check(&self) {
        for entry in self.connections.iter() {
            let mut entry_guard = entry.value().write().await;
            let domain = entry_guard.domain.clone();

            for conn in &mut entry_guard.connections {
                if conn.state == PooledConnectionState::Ready {
                    // Check if the connection is still alive
                    if !conn.connection.is_connected() {
                        conn.state = PooledConnectionState::Closed;
                        debug!(
                            domain = %domain,
                            "Connection marked as closed during health check"
                        );
                    }
                }
            }
        }
    }

    /// Start background maintenance tasks.
    ///
    /// This spawns tasks for periodic cleanup and health checking.
    pub fn start_maintenance(&self) -> MaintenanceHandle {
        let pool = self.clone_for_maintenance();
        let config = self.config.clone();
        let shutdown = Arc::clone(&self.shutdown);

        let handle = tokio::spawn(async move {
            let mut cleanup_interval = tokio::time::interval(config.idle_timeout / 2);
            let mut health_check_interval = tokio::time::interval(config.health_check_interval);

            loop {
                tokio::select! {
                    _ = cleanup_interval.tick() => {
                        if shutdown.load(Ordering::Relaxed) {
                            break;
                        }
                        pool.cleanup().await;
                    }
                    _ = health_check_interval.tick() => {
                        if shutdown.load(Ordering::Relaxed) {
                            break;
                        }
                        pool.health_check().await;
                    }
                }
            }

            debug!("Pool maintenance task stopped");
        });

        MaintenanceHandle { handle }
    }

    /// Clone pool for use in maintenance task.
    fn clone_for_maintenance(&self) -> S2sConnectionPoolRef {
        S2sConnectionPoolRef {
            config: self.config.clone(),
            connections: self.connections.clone(),
            metrics: Arc::clone(&self.metrics),
            shutdown: Arc::clone(&self.shutdown),
        }
    }

    /// Shutdown the pool.
    pub async fn shutdown(&self) {
        info!("Shutting down S2S connection pool");
        self.shutdown.store(true, Ordering::Relaxed);

        // Close all connections
        for entry in self.connections.iter() {
            let mut entry_guard = entry.value().write().await;
            for conn in &mut entry_guard.connections {
                conn.state = PooledConnectionState::Closed;
                // Note: actual stream shutdown would happen in the connection
            }
        }
    }

    /// Get pool metrics.
    pub fn metrics(&self) -> &Arc<S2sPoolMetrics> {
        &self.metrics
    }

    /// Get the number of domains with active connections.
    pub fn domain_count(&self) -> usize {
        self.connections.len()
    }

    /// Get the local domain.
    pub fn local_domain(&self) -> &str {
        &self.local_domain
    }

    /// Send a stanza to a remote domain.
    ///
    /// This is a convenience method that gets or creates a connection to the
    /// specified domain and sends the stanza through it.
    ///
    /// # Arguments
    ///
    /// * `domain` - The remote domain to send the stanza to
    /// * `stanza_xml` - The XML stanza bytes to send
    ///
    /// # Returns
    ///
    /// Returns `Ok(())` if the stanza was sent successfully, or an error if
    /// connection failed or the write failed.
    #[instrument(skip(self, stanza_xml), name = "s2s.pool.send_stanza", fields(domain = %domain, data_len = stanza_xml.len()))]
    pub async fn send_stanza(&self, domain: &str, stanza_xml: &[u8]) -> Result<(), S2sPoolError> {
        // Get or create a connection to the domain
        let handle = self.get_or_connect(domain).await?;

        // Send the stanza through the connection
        handle.send_stanza(stanza_xml).await
    }
}

/// Reference to pool for maintenance tasks.
struct S2sConnectionPoolRef {
    config: S2sPoolConfig,
    connections: DashMap<String, Arc<RwLock<DomainPoolEntry>>>,
    metrics: Arc<S2sPoolMetrics>,
    /// Note: shutdown is kept here for future use to enable graceful shutdown
    /// of the maintenance task when checking connection health.
    #[allow(dead_code)]
    shutdown: Arc<std::sync::atomic::AtomicBool>,
}

impl S2sConnectionPoolRef {
    async fn cleanup(&self) -> usize {
        let mut total_cleaned = 0;

        for entry in self.connections.iter() {
            let mut entry_guard = entry.value().write().await;
            let before_count = entry_guard.connections.len();

            entry_guard.remove_closed();
            entry_guard
                .connections
                .retain(|c| !c.is_idle(self.config.idle_timeout));

            let removed = before_count - entry_guard.connections.len();
            if removed > 0 {
                total_cleaned += removed;
                for _ in 0..removed {
                    self.metrics.record_connection_closed();
                }
            }
        }

        total_cleaned
    }

    async fn health_check(&self) {
        for entry in self.connections.iter() {
            let mut entry_guard = entry.value().write().await;

            for conn in &mut entry_guard.connections {
                if conn.state == PooledConnectionState::Ready && !conn.connection.is_connected() {
                    conn.state = PooledConnectionState::Closed;
                }
            }
        }
    }
}

/// Handle for the maintenance background task.
pub struct MaintenanceHandle {
    handle: tokio::task::JoinHandle<()>,
}

impl MaintenanceHandle {
    /// Stop the maintenance task.
    pub fn stop(self) {
        self.handle.abort();
    }

    /// Wait for the maintenance task to complete.
    pub async fn join(self) -> Result<(), tokio::task::JoinError> {
        self.handle.await
    }
}

/// Handle for a pooled connection.
///
/// This is returned from `get_or_connect` and tracks metrics for the connection.
pub struct PoolConnectionHandle {
    domain: String,
    pool_metrics: Arc<S2sPoolMetrics>,
    /// Reference to the pool's connections for sending stanzas.
    connections: DashMap<String, Arc<RwLock<DomainPoolEntry>>>,
}

impl PoolConnectionHandle {
    /// Get the domain this connection is for.
    pub fn domain(&self) -> &str {
        &self.domain
    }

    /// Record that a stanza was sent.
    pub fn record_stanza_sent(&self) {
        self.pool_metrics.record_stanza_sent();
    }

    /// Send a stanza (as raw bytes) through this connection.
    ///
    /// This writes the XML stanza bytes to the underlying S2S connection.
    ///
    /// # Arguments
    ///
    /// * `data` - The XML stanza bytes to send
    ///
    /// # Returns
    ///
    /// Returns `Ok(())` if the stanza was sent successfully, or an error if
    /// the connection is not available or the write failed.
    #[instrument(skip(self, data), fields(domain = %self.domain, data_len = data.len()))]
    pub async fn send_stanza(&self, data: &[u8]) -> Result<(), S2sPoolError> {
        // Get the connection entry for this domain
        let entry = self.connections.get(&self.domain).ok_or_else(|| {
            S2sPoolError::NoHealthyConnections {
                domain: self.domain.clone(),
            }
        })?;

        // Lock and get a healthy connection
        let mut entry_guard = entry.write().await;

        // Find a healthy connection
        let conn = entry_guard.get_healthy_connection().ok_or_else(|| {
            S2sPoolError::NoHealthyConnections {
                domain: self.domain.clone(),
            }
        })?;

        // Send the stanza through the connection
        match conn.connection.send_raw(data).await {
            Ok(()) => {
                conn.record_activity();
                self.pool_metrics.record_stanza_sent();
                debug!(domain = %self.domain, "Stanza sent successfully via S2S");
                Ok(())
            }
            Err(e) => {
                conn.record_error();
                self.pool_metrics.record_connection_error();
                warn!(domain = %self.domain, error = %e, "Failed to send stanza via S2S");
                Err(S2sPoolError::ConnectionFailed(e))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pool_config_default() {
        let config = S2sPoolConfig::default();
        assert_eq!(
            config.max_connections_per_domain,
            DEFAULT_MAX_CONNECTIONS_PER_DOMAIN
        );
        assert_eq!(
            config.idle_timeout,
            Duration::from_secs(DEFAULT_IDLE_TIMEOUT_SECS)
        );
        assert!(config.use_dns_srv);
    }

    #[test]
    fn test_retry_config_default() {
        let config = RetryConfig::default();
        assert_eq!(config.max_attempts, 3);
        assert_eq!(config.initial_delay, Duration::from_millis(500));
        assert_eq!(config.backoff_multiplier, 2.0);
    }

    #[test]
    fn test_domain_pool_entry() {
        let entry = DomainPoolEntry::new("example.com".to_string());
        assert_eq!(entry.domain, "example.com");
        assert!(entry.connections.is_empty());
        assert!(!entry.connecting);
        assert_eq!(entry.consecutive_failures, 0);
        assert_eq!(entry.healthy_count(), 0);
    }

    #[test]
    fn test_backoff_duration() {
        let config = RetryConfig::default();
        let mut entry = DomainPoolEntry::new("example.com".to_string());

        // No failures = no backoff
        assert_eq!(entry.backoff_duration(&config), Duration::ZERO);

        // First failure = initial delay
        entry.consecutive_failures = 1;
        assert_eq!(entry.backoff_duration(&config), Duration::from_millis(500));

        // Second failure = doubled
        entry.consecutive_failures = 2;
        assert_eq!(entry.backoff_duration(&config), Duration::from_millis(1000));

        // Third failure = doubled again
        entry.consecutive_failures = 3;
        assert_eq!(entry.backoff_duration(&config), Duration::from_millis(2000));
    }

    #[test]
    fn test_backoff_max_cap() {
        let config = RetryConfig {
            max_delay: Duration::from_secs(5),
            ..Default::default()
        };
        let mut entry = DomainPoolEntry::new("example.com".to_string());

        // Many failures should be capped at max_delay
        entry.consecutive_failures = 100;
        assert_eq!(entry.backoff_duration(&config), Duration::from_secs(5));
    }

    #[test]
    fn test_pool_metrics() {
        let metrics = S2sPoolMetrics::new();

        assert_eq!(metrics.active_count(), 0);

        metrics.record_connection_created();
        assert_eq!(metrics.active_count(), 1);
        assert_eq!(metrics.pool_misses.load(Ordering::Relaxed), 1);

        metrics.record_pool_hit();
        assert_eq!(metrics.pool_hits.load(Ordering::Relaxed), 1);

        metrics.record_connection_closed();
        assert_eq!(metrics.active_count(), 0);

        metrics.record_stanza_sent();
        assert_eq!(metrics.stanzas_sent.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_pooled_connection_state() {
        assert_eq!(PooledConnectionState::Ready, PooledConnectionState::Ready);
        assert_ne!(PooledConnectionState::Ready, PooledConnectionState::Closed);
    }

    #[test]
    fn test_pool_connection_handle_domain() {
        let metrics = Arc::new(S2sPoolMetrics::new());
        let connections = DashMap::new();

        let handle = PoolConnectionHandle {
            domain: "example.com".to_string(),
            pool_metrics: metrics,
            connections,
        };

        assert_eq!(handle.domain(), "example.com");
    }

    #[tokio::test]
    async fn test_pool_connection_handle_send_stanza_no_connection() {
        let metrics = Arc::new(S2sPoolMetrics::new());
        let connections: DashMap<String, Arc<RwLock<DomainPoolEntry>>> = DashMap::new();

        let handle = PoolConnectionHandle {
            domain: "example.com".to_string(),
            pool_metrics: metrics,
            connections,
        };

        // Attempting to send should fail with NoHealthyConnections since there's no entry
        let result = handle.send_stanza(b"<message/>").await;
        assert!(result.is_err());
        match result {
            Err(S2sPoolError::NoHealthyConnections { domain }) => {
                assert_eq!(domain, "example.com");
            }
            _ => panic!("Expected NoHealthyConnections error"),
        }
    }

    #[tokio::test]
    async fn test_pool_connection_handle_send_stanza_no_healthy_connection() {
        let metrics = Arc::new(S2sPoolMetrics::new());
        let connections: DashMap<String, Arc<RwLock<DomainPoolEntry>>> = DashMap::new();

        // Create an empty domain entry (has no connections)
        let entry = DomainPoolEntry::new("example.com".to_string());
        connections.insert("example.com".to_string(), Arc::new(RwLock::new(entry)));

        let handle = PoolConnectionHandle {
            domain: "example.com".to_string(),
            pool_metrics: metrics,
            connections,
        };

        // Attempting to send should fail with NoHealthyConnections since the entry has no connections
        let result = handle.send_stanza(b"<message/>").await;
        assert!(result.is_err());
        match result {
            Err(S2sPoolError::NoHealthyConnections { domain }) => {
                assert_eq!(domain, "example.com");
            }
            _ => panic!("Expected NoHealthyConnections error"),
        }
    }
}
