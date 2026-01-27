//! S2S Federation Integration Tests
//!
//! These tests verify the S2S (Server-to-Server) federation implementation:
//! - Dialback authentication (XEP-0220)
//! - Connection pooling configuration
//! - DNS SRV resolution
//!
//! Note: Full end-to-end S2S testing requires two server instances with
//! proper DNS setup. These tests focus on unit-level verification of
//! the S2S components.
//!
//! Run with: `cargo test -p waddle-xmpp --test s2s_integration`

mod common;

use std::time::Duration;

use waddle_xmpp::s2s::{
    dialback::{
        build_db_result, build_db_result_response, build_db_verify, build_db_verify_response,
        DialbackKey, DialbackResult, DialbackState, NS_DIALBACK, NS_DIALBACK_FEATURES,
    },
    dns::{ResolvedTarget, DEFAULT_S2S_PORT},
    pool::{PooledConnectionState, RetryConfig, S2sPoolConfig, S2sPoolMetrics},
    S2sDirection, S2sMetrics, S2sState,
};

/// Initialize test environment.
fn init_test() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        common::install_crypto_provider();
        let _ = tracing_subscriber::fmt()
            .with_env_filter("debug")
            .with_test_writer()
            .try_init();
    });
}

// =============================================================================
// Test: Dialback Key Generation and Verification
// =============================================================================

/// Test dialback key generation is deterministic for same inputs.
#[test]
fn test_dialback_key_generation_deterministic() {
    let secret = b"test-dialback-secret";
    let key_gen = DialbackKey::new(secret);

    let stream_id = "stream-12345";
    let originating = "alice.example.com";
    let receiving = "bob.example.org";

    let key1 = key_gen.generate(stream_id, receiving, originating);
    let key2 = key_gen.generate(stream_id, receiving, originating);

    assert_eq!(key1, key2, "Same inputs should produce same key");
}

/// Test dialback key is different for different inputs.
#[test]
fn test_dialback_key_uniqueness() {
    let secret = b"test-dialback-secret";
    let key_gen = DialbackKey::new(secret);
    let stream_id = "stream-12345";

    let key1 = key_gen.generate(stream_id, "beta.example.org", "alpha.example.com");
    let key2 = key_gen.generate(stream_id, "beta.example.org", "gamma.example.com");
    let key3 = key_gen.generate(stream_id, "delta.example.org", "alpha.example.com");

    assert_ne!(key1, key2, "Different originating domain should produce different key");
    assert_ne!(key1, key3, "Different receiving domain should produce different key");
}

/// Test dialback key verification.
#[test]
fn test_dialback_key_verification() {
    let secret = b"dialback-secret-12345";
    let key_gen = DialbackKey::new(secret);

    let stream_id = "unique-stream-id";
    let originating = "sender.xmpp.net";
    let receiving = "receiver.xmpp.net";

    // Generate a key
    let key = key_gen.generate(stream_id, receiving, originating);

    // Verify with correct parameters
    assert!(
        key_gen.verify(&key, stream_id, receiving, originating),
        "Key should verify with correct parameters"
    );

    // Verify with wrong stream_id
    assert!(
        !key_gen.verify(&key, "wrong-stream-id", receiving, originating),
        "Key should not verify with wrong stream_id"
    );

    // Verify with wrong domain
    assert!(
        !key_gen.verify(&key, stream_id, receiving, "wrong.xmpp.net"),
        "Key should not verify with wrong originating domain"
    );

    // Different secret should also fail
    let wrong_key_gen = DialbackKey::new(b"wrong-secret");
    let wrong_key = wrong_key_gen.generate(stream_id, receiving, originating);
    assert_ne!(key, wrong_key, "Different secret should produce different key");
}

// =============================================================================
// Test: Dialback State Machine
// =============================================================================

/// Test dialback state values.
#[test]
fn test_dialback_state_values() {
    // Test state equality
    assert_eq!(DialbackState::None, DialbackState::None);
    assert_eq!(DialbackState::Pending, DialbackState::Pending);
    assert_eq!(DialbackState::Verified, DialbackState::Verified);
    assert_eq!(DialbackState::Failed, DialbackState::Failed);

    // Test state inequality
    assert_ne!(DialbackState::None, DialbackState::Pending);
    assert_ne!(DialbackState::Pending, DialbackState::Verified);
    assert_ne!(DialbackState::Verified, DialbackState::Failed);

    // Test default
    let default_state = DialbackState::default();
    assert_eq!(default_state, DialbackState::None);
}

/// Test dialback result values.
#[test]
fn test_dialback_result_values() {
    assert_eq!(DialbackResult::Valid.as_str(), "valid");
    assert_eq!(DialbackResult::Invalid.as_str(), "invalid");

    // Test parsing
    assert_eq!(DialbackResult::parse("valid"), Some(DialbackResult::Valid));
    assert_eq!(DialbackResult::parse("invalid"), Some(DialbackResult::Invalid));
    assert_eq!(DialbackResult::parse("unknown"), None);
}

// =============================================================================
// Test: S2S Connection State
// =============================================================================

/// Test S2S connection state values.
#[test]
fn test_s2s_state_values() {
    assert_eq!(S2sState::Initial, S2sState::Initial);
    assert_ne!(S2sState::Initial, S2sState::Dialback);
    assert_ne!(S2sState::Dialback, S2sState::Established);
    assert_ne!(S2sState::Established, S2sState::Closed);
}

/// Test S2S direction values.
#[test]
fn test_s2s_direction_values() {
    assert_eq!(S2sDirection::Inbound, S2sDirection::Inbound);
    assert_eq!(S2sDirection::Outbound, S2sDirection::Outbound);
    assert_ne!(S2sDirection::Inbound, S2sDirection::Outbound);
}

// =============================================================================
// Test: S2S Metrics
// =============================================================================

/// Test S2S metrics tracking.
#[test]
fn test_s2s_metrics() {
    let metrics = S2sMetrics::new();

    assert_eq!(metrics.active_connections(), 0);
    assert_eq!(metrics.total_connection_attempts(), 0);
    assert_eq!(metrics.total_tls_established(), 0);

    // Record some events
    metrics.record_connection_attempt();
    metrics.record_connection_attempt();
    assert_eq!(metrics.total_connection_attempts(), 2);

    metrics.record_tls_established();
    assert_eq!(metrics.total_tls_established(), 1);

    metrics.record_connection_established();
    assert_eq!(metrics.active_connections(), 1);

    metrics.record_connection_established();
    assert_eq!(metrics.active_connections(), 2);

    metrics.record_connection_closed();
    assert_eq!(metrics.active_connections(), 1);
}

// =============================================================================
// Test: Resolved Target
// =============================================================================

/// Test resolved target structure.
#[test]
fn test_resolved_target() {
    let target = ResolvedTarget {
        host: "xmpp.example.com".to_string(),
        port: 5269,
        priority: 10,
        weight: 100,
    };

    assert_eq!(target.host, "xmpp.example.com");
    assert_eq!(target.port, 5269);
    assert_eq!(target.priority, 10);
    assert_eq!(target.weight, 100);
}

// =============================================================================
// Test: S2S Pool Configuration
// =============================================================================

/// Test pool configuration defaults.
#[test]
fn test_pool_config_defaults() {
    let config = S2sPoolConfig::default();

    // Should have reasonable defaults
    assert!(config.max_connections_per_domain > 0);
    assert!(config.connect_timeout > Duration::ZERO);
    assert!(config.idle_timeout > Duration::ZERO);
    assert!(config.health_check_interval > Duration::ZERO);
    assert!(config.use_dns_srv);
}

/// Test retry configuration.
#[test]
fn test_retry_config() {
    let config = RetryConfig::default();

    // Should have reasonable defaults
    assert!(config.max_attempts > 0);
    assert!(config.initial_delay > Duration::ZERO);
    assert!(config.max_delay > config.initial_delay);
    assert!(config.backoff_multiplier > 1.0);
}

/// Test pool metrics.
#[test]
fn test_pool_metrics() {
    let metrics = S2sPoolMetrics::new();

    // Initial state
    assert_eq!(metrics.active_connections.load(std::sync::atomic::Ordering::Relaxed), 0);
    assert_eq!(metrics.connections_created.load(std::sync::atomic::Ordering::Relaxed), 0);

    // Record connection created
    metrics.record_connection_created();
    assert_eq!(metrics.connections_created.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert_eq!(metrics.active_connections.load(std::sync::atomic::Ordering::Relaxed), 1);

    // Record connection closed
    metrics.record_connection_closed();
    assert_eq!(metrics.connections_created.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert_eq!(metrics.active_connections.load(std::sync::atomic::Ordering::Relaxed), 0);
}

/// Test pooled connection state values.
#[test]
fn test_pooled_connection_state() {
    assert_eq!(PooledConnectionState::Connecting, PooledConnectionState::Connecting);
    assert_eq!(PooledConnectionState::Ready, PooledConnectionState::Ready);
    assert_eq!(PooledConnectionState::Unhealthy, PooledConnectionState::Unhealthy);
    assert_eq!(PooledConnectionState::Closed, PooledConnectionState::Closed);

    assert_ne!(PooledConnectionState::Ready, PooledConnectionState::Unhealthy);
}

// =============================================================================
// Test: Dialback XML Generation
// =============================================================================

/// Test dialback result XML generation.
#[test]
fn test_build_db_result() {
    let xml = build_db_result("originating.com", "receiving.com", "abc123key");

    assert!(xml.contains("<db:result"));
    assert!(xml.contains("xmlns:db"));
    assert!(xml.contains(NS_DIALBACK));
    assert!(xml.contains("from='originating.com'") || xml.contains("from=\"originating.com\""));
    assert!(xml.contains("to='receiving.com'") || xml.contains("to=\"receiving.com\""));
    assert!(xml.contains("abc123key"));
}

/// Test dialback result response XML generation.
#[test]
fn test_build_db_result_response() {
    // Valid result
    let valid_xml = build_db_result_response("receiving.com", "originating.com", DialbackResult::Valid);
    assert!(valid_xml.contains("type='valid'") || valid_xml.contains("type=\"valid\""));
    assert!(valid_xml.contains("from='receiving.com'") || valid_xml.contains("from=\"receiving.com\""));
    assert!(valid_xml.contains("to='originating.com'") || valid_xml.contains("to=\"originating.com\""));

    // Invalid result
    let invalid_xml = build_db_result_response("receiving.com", "originating.com", DialbackResult::Invalid);
    assert!(invalid_xml.contains("type='invalid'") || invalid_xml.contains("type=\"invalid\""));
}

/// Test dialback verify XML generation.
#[test]
fn test_build_db_verify() {
    let xml = build_db_verify("receiving.com", "originating.com", "stream-123", "keyvalue");

    assert!(xml.contains("<db:verify"));
    assert!(xml.contains("xmlns:db"));
    assert!(xml.contains(NS_DIALBACK));
    assert!(xml.contains("id='stream-123'") || xml.contains("id=\"stream-123\""));
    assert!(xml.contains("keyvalue"));
}

/// Test dialback verify response XML generation.
#[test]
fn test_build_db_verify_response() {
    let valid_xml = build_db_verify_response("originating.com", "receiving.com", "stream-123", DialbackResult::Valid);
    assert!(valid_xml.contains("<db:verify"));
    assert!(valid_xml.contains("type='valid'") || valid_xml.contains("type=\"valid\""));
    assert!(valid_xml.contains("id='stream-123'") || valid_xml.contains("id=\"stream-123\""));
}

// =============================================================================
// Test: S2S Namespace Constants
// =============================================================================

/// Test S2S namespace constants.
#[test]
fn test_s2s_namespaces() {
    // Dialback namespace (XEP-0220)
    assert_eq!(NS_DIALBACK, "jabber:server:dialback");

    // Dialback features namespace
    assert!(NS_DIALBACK_FEATURES.contains("dialback"));
}

// =============================================================================
// Test: Default S2S Port
// =============================================================================

/// Test default S2S port.
#[test]
fn test_default_s2s_port() {
    assert_eq!(DEFAULT_S2S_PORT, 5269, "Default S2S port should be 5269");
}

// =============================================================================
// Note: Full E2E S2S Tests
// =============================================================================

// Full end-to-end S2S testing would require:
// 1. Two XMPP server instances running on different ports/domains
// 2. DNS SRV mocking or custom resolver
// 3. TLS certificates for both domains
// 4. Connection pool setup for both servers
//
// This is typically done in integration test environments with:
// - Docker containers for isolated server instances
// - Mocked DNS resolution
// - Test CA for certificate generation
//
// For now, the above unit tests verify the individual components work correctly.
// A FederationTestHarness would look like:
//
// ```ignore
// pub struct FederationTestHarness {
//     server_a: TestServer,  // domain: alpha.local, c2s: 15222, s2s: 15269
//     server_b: TestServer,  // domain: beta.local, c2s: 15223, s2s: 15270
//     dns_mock: MockDnsResolver,
// }
// ```
