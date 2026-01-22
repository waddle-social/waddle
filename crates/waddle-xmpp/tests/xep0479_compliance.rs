//! XEP-0479 (XMPP Compliance Suites 2023) Testing
//!
//! This module runs the XMPP Interop Testing suite against the waddle-xmpp server
//! to verify compliance with XEP-0479 specifications.
//!
//! The XMPP Interop Testing suite contains 933+ tests covering various XMPP
//! specifications and is the standard compliance testing framework for XMPP servers.
//!
//! ## Running
//!
//! ```bash
//! # Run with default settings (expects server on localhost:5222)
//! cargo test -p waddle-xmpp --test xep0479_compliance -- --ignored
//!
//! # Run with verbose output
//! cargo test -p waddle-xmpp --test xep0479_compliance -- --ignored --nocapture
//! ```
//!
//! ## Requirements
//!
//! - Docker must be running
//! - The waddle-xmpp server must be started separately for these tests
//!
//! ## References
//!
//! - [XMPP Interop Testing](https://xmpp-interop-testing.github.io/)
//! - [XEP-0479](https://xmpp.org/extensions/xep-0479.html)

mod common;

use std::time::Duration;

use testcontainers::{
    core::{ContainerRequest, WaitFor},
    runners::AsyncRunner,
    GenericImage, ImageExt,
};

/// Container image for XMPP Interop Testing
const XMPP_INTEROP_IMAGE: &str = "ghcr.io/xmpp-interop-testing/xmpp_interop_tests";
const XMPP_INTEROP_TAG: &str = "latest";

/// XMPP Interop Testing container configuration
struct XmppInteropTestsImage {
    domain: String,
    host: String,
    admin_username: String,
    admin_password: String,
    security_mode: String,
    timeout: u32,
    disabled_specs: Vec<String>,
    enabled_specs: Vec<String>,
}

impl Default for XmppInteropTestsImage {
    fn default() -> Self {
        Self {
            domain: "localhost".to_string(),
            host: "host.docker.internal".to_string(), // Connect to host machine
            admin_username: "admin".to_string(),
            admin_password: "interop-test-password".to_string(),
            security_mode: "disabled".to_string(),
            timeout: 10000,
            // Default disabled specs - features not yet implemented
            disabled_specs: vec![
                "XEP-0220".to_string(), // Server Dialback (S2S)
                "XEP-0045".to_string(), // MUC (partial implementation)
                "XEP-0060".to_string(), // PubSub
                "XEP-0163".to_string(), // PEP
                "XEP-0363".to_string(), // HTTP File Upload
                "XEP-0054".to_string(), // vcard-temp
                "XEP-0191".to_string(), // Blocking
            ],
            enabled_specs: vec![],
        }
    }
}

impl XmppInteropTestsImage {
    fn new() -> Self {
        Self::default()
    }

    fn with_domain(mut self, domain: impl Into<String>) -> Self {
        self.domain = domain.into();
        self
    }

    fn with_host(mut self, host: impl Into<String>) -> Self {
        self.host = host.into();
        self
    }

    fn with_admin(mut self, username: impl Into<String>, password: impl Into<String>) -> Self {
        self.admin_username = username.into();
        self.admin_password = password.into();
        self
    }

    fn with_security_mode(mut self, mode: impl Into<String>) -> Self {
        self.security_mode = mode.into();
        self
    }

    fn with_disabled_specs(mut self, specs: Vec<String>) -> Self {
        self.disabled_specs = specs;
        self
    }

    fn with_enabled_specs(mut self, specs: Vec<String>) -> Self {
        self.enabled_specs = specs;
        self
    }

    /// Build command line arguments for the container
    fn build_args(&self) -> Vec<String> {
        let mut args = vec![
            format!("--domain={}", self.domain),
            format!("--host={}", self.host),
            format!("--timeout={}", self.timeout),
            format!("--adminAccountUsername={}", self.admin_username),
            format!("--adminAccountPassword={}", self.admin_password),
            format!("--securityMode={}", self.security_mode),
        ];

        if !self.enabled_specs.is_empty() {
            args.push(format!(
                "--enabledSpecifications={}",
                self.enabled_specs.join(",")
            ));
        } else if !self.disabled_specs.is_empty() {
            args.push(format!(
                "--disabledSpecifications={}",
                self.disabled_specs.join(",")
            ));
        }

        args
    }

    /// Create the ContainerRequest for testcontainers
    fn into_container_request(self) -> ContainerRequest<GenericImage> {
        let args = self.build_args();

        GenericImage::new(XMPP_INTEROP_IMAGE, XMPP_INTEROP_TAG)
            .with_wait_for(WaitFor::message_on_stdout("tests completed"))
            .with_cmd(args)
    }
}

/// Initialize tracing for tests
fn init_tracing() {
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

/// Run XEP-0479 compliance tests against an external XMPP server.
///
/// This test is ignored by default because it requires:
/// 1. Docker to be running
/// 2. An XMPP server to be running on localhost:5222
///
/// To run this test:
/// 1. Start the waddle server: `cargo run -p waddle-server`
/// 2. Run the test: `cargo test -p waddle-xmpp --test xep0479_compliance -- --ignored`
#[tokio::test]
#[ignore = "Requires Docker and external XMPP server"]
async fn test_xep0479_compliance_external_server() {
    init_tracing();

    let config = XmppInteropTestsImage::new()
        .with_domain("localhost")
        .with_host("host.docker.internal")
        .with_admin("admin", "interop-test-password")
        .with_security_mode("disabled");

    let image = config.into_container_request();

    println!("Starting XMPP Interop Testing container...");
    println!("This may fail initially - that's expected for unimplemented features.");

    let container = image
        .with_network("host")
        .start()
        .await
        .expect("Failed to start XMPP Interop Testing container");

    // Wait for container to complete (tests run and exit)
    tokio::time::sleep(Duration::from_secs(120)).await;

    // Get logs
    let stdout = container.stdout_to_vec().await.unwrap_or_default();
    let stderr = container.stderr_to_vec().await.unwrap_or_default();

    println!("=== XMPP Interop Test Results ===");
    println!("{}", String::from_utf8_lossy(&stdout));

    if !stderr.is_empty() {
        eprintln!("=== Errors ===");
        eprintln!("{}", String::from_utf8_lossy(&stderr));
    }

    // Note: We don't assert on success here because many tests will fail
    // until all XEPs are implemented. The goal is to track progress over time.
}

/// Run a subset of compliance tests for core XMPP functionality.
///
/// This tests only the RFCs and XEPs that are implemented.
#[tokio::test]
#[ignore = "Requires Docker and external XMPP server"]
async fn test_core_compliance() {
    init_tracing();

    let config = XmppInteropTestsImage::new()
        .with_domain("localhost")
        .with_host("host.docker.internal")
        .with_admin("admin", "interop-test-password")
        .with_security_mode("disabled")
        .with_enabled_specs(vec![
            "RFC6120".to_string(), // XMPP Core
            "RFC6121".to_string(), // XMPP IM
            "XEP-0030".to_string(), // Service Discovery
        ]);

    let image = config.into_container_request();

    println!("Running core compliance tests (RFC6120, RFC6121, XEP-0030)...");

    let container = image
        .with_network("host")
        .start()
        .await
        .expect("Failed to start XMPP Interop Testing container");

    tokio::time::sleep(Duration::from_secs(60)).await;

    let stdout = container.stdout_to_vec().await.unwrap_or_default();
    println!("=== Core Compliance Results ===");
    println!("{}", String::from_utf8_lossy(&stdout));
}

/// Run compliance tests with the in-process test server.
///
/// This starts the TestServer from the common module and runs
/// the XMPP Interop Tests against it.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_compliance_with_test_server() {
    init_tracing();

    // Start our test server
    let server = common::TestServer::start().await;
    let port = server.addr.port();

    println!("Test server started on port {}", port);

    // On macOS/Windows, use host.docker.internal
    // On Linux, we might need to use the actual IP or host networking
    let host = if cfg!(target_os = "linux") {
        "172.17.0.1" // Docker bridge network gateway
    } else {
        "host.docker.internal"
    };

    let config = XmppInteropTestsImage::new()
        .with_domain("localhost")
        .with_host(format!("{}:{}", host, port))
        .with_admin("testuser", "testtoken") // MockAppState accepts any credentials
        .with_security_mode("disabled")
        .with_enabled_specs(vec![
            "XEP-0030".to_string(), // Service Discovery (implemented)
        ]);

    let image = config.into_container_request();

    println!("Starting XMPP Interop Testing against test server...");

    // Note: We can't use host networking AND specify a custom port easily
    // The container expects the server on default port 5222
    // This test serves as a template - for full testing, use the external server test

    let container_result = image
        .with_network("host")
        .start()
        .await;

    match container_result {
        Ok(container) => {
            tokio::time::sleep(Duration::from_secs(30)).await;
            let stdout = container.stdout_to_vec().await.unwrap_or_default();
            println!("{}", String::from_utf8_lossy(&stdout));
        }
        Err(e) => {
            println!("Container failed to start (may be expected): {}", e);
        }
    }

    // Keep server alive briefly for cleanup
    drop(server);
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn test_image_config_default() {
        let config = XmppInteropTestsImage::default();
        assert_eq!(config.domain, "localhost");
        assert_eq!(config.security_mode, "disabled");
        assert!(!config.disabled_specs.is_empty());
    }

    #[test]
    fn test_image_config_args() {
        let config = XmppInteropTestsImage::new()
            .with_domain("example.com")
            .with_host("192.168.1.1")
            .with_admin("admin", "secret");

        let args = config.build_args();

        assert!(args.contains(&"--domain=example.com".to_string()));
        assert!(args.contains(&"--host=192.168.1.1".to_string()));
        assert!(args.contains(&"--adminAccountUsername=admin".to_string()));
        assert!(args.contains(&"--adminAccountPassword=secret".to_string()));
    }

    #[test]
    fn test_enabled_specs_override() {
        let config = XmppInteropTestsImage::new()
            .with_enabled_specs(vec!["XEP-0030".to_string()]);

        let args = config.build_args();

        // When enabled specs are set, disabled specs should not be included
        let has_enabled = args
            .iter()
            .any(|a| a.starts_with("--enabledSpecifications="));
        let has_disabled = args
            .iter()
            .any(|a| a.starts_with("--disabledSpecifications="));

        assert!(has_enabled);
        assert!(!has_disabled);
    }
}
