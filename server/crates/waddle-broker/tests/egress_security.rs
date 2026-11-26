//! Feature 17 tests: constrained provider egress profiles, DNS-resolved SSRF
//! blocklist, byte/time/concurrency/spend caps, response/error/header stripping,
//! admin endpoint blocking, and `unseal_for_provider_egress` metadata binding.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::Utc;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::elliptic_curve::rand_core::OsRng;
use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde_json::json;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{watch, Notify};
use tokio_rustls::TlsAcceptor;
use uuid::Uuid;
use waddle_broker::admission::invocation::{
    InvocationPrincipal, PayerSelection, SignedInferenceInvocation,
};
use waddle_broker::admission::replica_fence::BrokerReplicaFence;
use waddle_broker::admission::service::{
    AdmitInvocationInput, AdmittedDispatch, BoundDispatchContext, BrokerAdmissionService,
    TraceRecorder,
};
use waddle_broker::admission::turso::TursoAdmissionLedgerStore;
use waddle_broker::admission::ReplicaFenceStore;
use waddle_broker::authz::local::LocalPermissionBackend;
use waddle_broker::authz::AuthzCallerRole;
use waddle_broker::custody::{BrokerCustodyService, EnrollCredentialInput};
use waddle_broker::egress::client::{
    BrokerEgressClient, EgressError, EgressInferenceRequest, ExpectedEgressBinding,
    ProviderOperationWire,
};
use waddle_broker::egress::profile::{
    is_blocked_admin_path, EgressBindingPolicy, EgressCaps, EgressProfileRegistry, ProfileError,
    ProviderOperation, ProviderProfile,
};
use waddle_broker::egress::ssrf::{resolve_and_validate_addrs, DnsResolver, SsrfError};
use waddle_broker::error::BrokerError;
use waddle_broker::idp_proof::{
    build_device_enroll_pop_transcript, p256_jwk_thumbprint, AuthProvenance, DeviceKeyRegistry,
    EnrollDeviceKeyInput, VerifiedIdpIntent,
};
use waddle_broker::key_hierarchy::KeyHierarchyService;
use waddle_broker::kms::local::LocalKmsProvider;
use waddle_broker::ledger::turso::TursoLedgerStore;
use waddle_broker::secret::SecretBytes;
use waddle_broker_core::admission::{
    AdmitReceipt, AttemptId, BudgetLimits, RoomScopeKey, RunId, StepId,
};
use waddle_broker_core::boot_clock::SystemBootClock;
use waddle_broker_core::identity::{
    AccountId, HolderInstance, IncarnationPubKey, SpiffeId, TenantId, TlsExporterProof,
};
use waddle_broker_core::permission::{
    AuthzModule, AuthzObject, AuthzObjectType, AuthzRelation, AuthzSubject, AuthzSubjectType,
    AuthzTuple, PermissionBackend,
};

struct MockDnsResolver {
    addrs: Vec<SocketAddr>,
}

#[async_trait]
impl DnsResolver for MockDnsResolver {
    async fn resolve(&self, _host: &str, _port: u16) -> Result<Vec<SocketAddr>, SsrfError> {
        Ok(self.addrs.clone())
    }
}

struct EmptyDnsResolver;

#[async_trait]
impl DnsResolver for EmptyDnsResolver {
    async fn resolve(&self, _host: &str, _port: u16) -> Result<Vec<SocketAddr>, SsrfError> {
        Ok(Vec::new())
    }
}

struct SlowDnsResolver {
    delay: Duration,
    addrs: Vec<SocketAddr>,
}

#[async_trait]
impl DnsResolver for SlowDnsResolver {
    async fn resolve(&self, _host: &str, _port: u16) -> Result<Vec<SocketAddr>, SsrfError> {
        tokio::time::sleep(self.delay).await;
        Ok(self.addrs.clone())
    }
}

struct TestTlsCertMaterial {
    ca_pem: Vec<u8>,
    server_config: Arc<rustls::ServerConfig>,
}

fn generate_test_tls_material(domain: &str) -> TestTlsCertMaterial {
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).expect("ca params");
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca_key = KeyPair::generate().expect("ca key");
    let ca_cert = ca_params.self_signed(&ca_key).expect("ca self signed");
    let ca_pem = ca_cert.pem().into_bytes();

    let leaf_params = CertificateParams::new(vec![domain.to_owned()]).expect("leaf params");
    let leaf_key = KeyPair::generate().expect("leaf key");
    let leaf_cert = leaf_params
        .signed_by(&leaf_key, &ca_cert, &ca_key)
        .expect("leaf signed");

    let cert_der: CertificateDer<'static> = leaf_cert.der().clone();
    let ca_der: CertificateDer<'static> = ca_cert.der().clone();
    let key_der = PrivateKeyDer::from(PrivatePkcs8KeyDer::from(leaf_key.serialize_der()));
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let server_config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("tls versions")
        .with_no_client_auth()
        .with_single_cert(vec![cert_der, ca_der], key_der)
        .expect("server config");

    TestTlsCertMaterial {
        ca_pem,
        server_config: Arc::new(server_config),
    }
}

async fn read_http_request(
    stream: &mut tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
) -> String {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 2048];
    loop {
        let n = stream.read(&mut tmp).await.expect("read tls request");
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(headers_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let header_text = String::from_utf8_lossy(&buf[..headers_end]);
            let content_len = header_text
                .lines()
                .find_map(|line| {
                    let (name, val) = line.split_once(':')?;
                    if name.eq_ignore_ascii_case("content-length") {
                        val.trim().parse::<usize>().ok()
                    } else {
                        None
                    }
                })
                .unwrap_or(0);
            if buf.len() >= headers_end + 4 + content_len {
                break;
            }
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

async fn setup_custody_with_credential(
    dir: &TempDir,
    provider: &str,
    plaintext: &[u8],
) -> (Arc<BrokerCustodyService>, TenantId, Uuid) {
    let tenant = TenantId::parse("acme.example").expect("tenant");
    let ledger = Arc::new(
        TursoLedgerStore::open_in_dir(dir.path())
            .await
            .expect("ledger"),
    );
    let kms = Arc::new(LocalKmsProvider::new());
    let hierarchy = Arc::new(KeyHierarchyService::new("trust.example", kms));
    let custody = Arc::new(
        BrokerCustodyService::new(hierarchy, ledger)
            .await
            .expect("custody"),
    );
    let receipt = custody
        .enroll(EnrollCredentialInput {
            tenant_id: tenant.clone(),
            account_id: None,
            provider: provider.to_owned(),
            nonce: format!("enroll-{provider}-{}", Uuid::new_v4()),
            payload_digest_sha256: [0x11; 32],
            expires_at_ms: Utc::now().timestamp_millis() + 60_000,
            plaintext: SecretBytes::new(plaintext.to_vec()).expect("secret"),
        })
        .await
        .expect("enroll");
    (custody, tenant, receipt.credential_id)
}

fn synthetic_dispatch(
    tenant: TenantId,
    provider: &str,
    credential_id: Uuid,
    reserved_spend_microcents: u64,
    max_spend_microcents: u64,
) -> (AdmittedDispatch, watch::Sender<bool>) {
    let (tx, rx) = watch::channel(false);
    let dispatch = AdmittedDispatch {
        receipt: AdmitReceipt {
            attempt_id: AttemptId::new_v4(),
            run_id: RunId::new_v4(),
            step_id: StepId::new(0),
            admitted_at_ms: Utc::now().timestamp_millis(),
            reserved_spend_microcents,
        },
        dispatch_context: BoundDispatchContext {
            trust_domain: "trust.example".to_owned(),
            tenant_id: tenant,
            extension_id: "ai-chatbot".to_owned(),
            extension_digest_sha256: [0xAA; 32],
            provider: provider.to_owned(),
            payer: PayerSelection::Tenant { credential_id },
            room_or_space: RoomScopeKey {
                scope_id: "general".to_owned(),
            },
            command_digest_sha256: [0xBB; 32],
            reserved_spend_microcents,
            budget: BudgetLimits {
                max_input_tokens: 4_000,
                max_output_tokens: 1_000,
                max_spend_microcents,
                max_steps: 4,
            },
        },
        cancel_rx: rx,
    };
    (dispatch, tx)
}

fn openai_profile(base_origin: &str, caps: EgressCaps) -> ProviderProfile {
    ProviderProfile {
        provider: "openai".to_owned(),
        deployment: "prod-us".to_owned(),
        model: "gpt-4o-mini".to_owned(),
        operation: ProviderOperation::ChatCompletions,
        base_origin: base_origin.to_owned(),
        operation_path: "/v1/chat/completions".to_owned(),
        pinned_ca_pem: None,
        caps,
    }
}

#[tokio::test]
async fn egress_profile_enforces_approved_tuple_and_provider_metadata_binding() {
    let dir = TempDir::new().expect("tempdir");
    let (custody, tenant, cred_id) =
        setup_custody_with_credential(&dir, "openai", b"sk-test-openai").await;

    // Metadata-verified unseal blocks cross-provider credential misuse.
    let mismatch = custody
        .unseal_for_provider_egress(&tenant, cred_id, "anthropic")
        .await
        .expect_err("must reject wrong provider");
    assert!(matches!(mismatch, BrokerError::ProviderMismatch { .. }));
    assert_eq!(
        custody
            .unseal_for_provider_egress(&tenant, cred_id, "openai")
            .await
            .expect("matching provider succeeds")
            .as_bytes(),
        b"sk-test-openai"
    );

    let mut registry = EgressProfileRegistry::new();
    registry
        .register(openai_profile(
            "https://api.openai.com",
            EgressCaps::default(),
        ))
        .expect("register profile");

    // Non-HTTPS origins and path-bearing origins are rejected.
    assert_eq!(
        registry.register(openai_profile(
            "http://api.openai.com",
            EgressCaps::default()
        )),
        Err(ProfileError::InvalidOrigin)
    );
    assert_eq!(
        registry.register(openai_profile(
            "https://api.openai.com/v1",
            EgressCaps::default()
        )),
        Err(ProfileError::InvalidOrigin)
    );

    let client = BrokerEgressClient::new(registry, Arc::clone(&custody));
    let (dispatch, _tx) = synthetic_dispatch(tenant.clone(), "openai", cred_id, 100, 1_000);

    // Unapproved model rejected before any network I/O.
    let bad_model = client
        .execute_unary(
            &dispatch,
            &EgressInferenceRequest {
                provider: "openai".to_owned(),
                deployment: "prod-us".to_owned(),
                model: "gpt-unapproved".to_owned(),
                operation: ProviderOperationWire::ChatCompletions,
                requested_path: None,
                body_json: json!({"model": "gpt-unapproved"}),
                stream: false,
            },
        )
        .await
        .expect_err("unapproved model must fail");
    assert!(matches!(
        bad_model,
        EgressError::Profile(ProfileError::UnapprovedProfile { .. })
    ));

    // Unapproved deployment rejected.
    let bad_deploy = client
        .execute_unary(
            &dispatch,
            &EgressInferenceRequest {
                provider: "openai".to_owned(),
                deployment: "evil-proxy".to_owned(),
                model: "gpt-4o-mini".to_owned(),
                operation: ProviderOperationWire::ChatCompletions,
                requested_path: None,
                body_json: json!({}),
                stream: false,
            },
        )
        .await
        .expect_err("unapproved deployment must fail");
    assert!(matches!(
        bad_deploy,
        EgressError::Profile(ProfileError::UnapprovedProfile { .. })
    ));

    // Unapproved operation rejected.
    let bad_op = client
        .execute_unary(
            &dispatch,
            &EgressInferenceRequest {
                provider: "openai".to_owned(),
                deployment: "prod-us".to_owned(),
                model: "gpt-4o-mini".to_owned(),
                operation: ProviderOperationWire::Messages,
                requested_path: None,
                body_json: json!({}),
                stream: false,
            },
        )
        .await
        .expect_err("unapproved operation must fail");
    assert!(matches!(
        bad_op,
        EgressError::Profile(ProfileError::UnapprovedProfile { .. })
    ));

    // Switching provider vs admitted dispatch fails closed.
    let bad_provider = client
        .execute_unary(
            &dispatch,
            &EgressInferenceRequest {
                provider: "anthropic".to_owned(),
                deployment: "prod-us".to_owned(),
                model: "gpt-4o-mini".to_owned(),
                operation: ProviderOperationWire::ChatCompletions,
                requested_path: None,
                body_json: json!({}),
                stream: false,
            },
        )
        .await
        .expect_err("provider mismatch must fail");
    assert!(matches!(bad_provider, EgressError::BindingMismatch(_)));
}

#[tokio::test]
async fn egress_admin_block_rejects_all_admin_and_key_inspection_endpoints() {
    for blocked in [
        "/v1/admin",
        "/v1/admin/users",
        "/v1/keys",
        "/v1/keys/list",
        "/v1/organization/api_keys",
        "/v1/auth/session",
        "/admin",
        "/keys",
        "/tokens",
        "/billing",
        "/v1/chat/completions/../keys",
        "/v1/%2e%2e/admin",
    ] {
        assert!(
            is_blocked_admin_path(blocked),
            "expected {blocked} to be unconditionally blocked"
        );
    }

    let mut registry = EgressProfileRegistry::new();
    for path in [
        "/v1/admin",
        "/v1/keys",
        "/v1/organization/keys",
        "/v1/billing",
    ] {
        let mut p = openai_profile("https://api.openai.com", EgressCaps::default());
        p.operation_path = path.to_owned();
        assert_eq!(
            registry.register(p),
            Err(ProfileError::BlockedAdminEndpoint)
        );
    }

    // Even when a valid profile is registered, passing an admin requested_path at
    // dispatch time is unconditionally blocked.
    registry
        .register(openai_profile(
            "https://api.openai.com",
            EgressCaps::default(),
        ))
        .expect("valid profile");
    let dir = TempDir::new().expect("tempdir");
    let (custody, tenant, cred_id) =
        setup_custody_with_credential(&dir, "openai", b"sk-test-openai").await;
    let client = BrokerEgressClient::new(registry, custody);
    let (dispatch, _tx) = synthetic_dispatch(tenant, "openai", cred_id, 100, 1_000);

    for admin_path in [
        "/v1/admin",
        "/v1/keys",
        "/v1/organization/members",
        "/tokens",
    ] {
        let err = client
            .execute_unary(
                &dispatch,
                &EgressInferenceRequest {
                    provider: "openai".to_owned(),
                    deployment: "prod-us".to_owned(),
                    model: "gpt-4o-mini".to_owned(),
                    operation: ProviderOperationWire::ChatCompletions,
                    requested_path: Some(admin_path.to_owned()),
                    body_json: json!({}),
                    stream: false,
                },
            )
            .await
            .expect_err("admin path must be rejected");
        assert!(matches!(
            err,
            EgressError::Profile(ProfileError::BlockedAdminEndpoint)
        ));
    }
}

#[tokio::test]
async fn egress_ssrf_and_ssrf_boundary_block_private_metadata_ipv4_mapped_and_redirects() {
    // 1. Direct DNS-resolved SSRF boundary tests across all required ranges.
    let blocked_ips: Vec<IpAddr> = vec![
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
        IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
        IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1)),
        IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)),
        IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254)),
        IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1)),
        IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)),
        IpAddr::V4(Ipv4Addr::new(224, 0, 0, 1)),
        IpAddr::V6(Ipv6Addr::UNSPECIFIED),
        IpAddr::V6(Ipv6Addr::LOCALHOST),
        IpAddr::V6("fc00::1".parse().expect("ula")),
        IpAddr::V6("fd12:3456::1".parse().expect("ula")),
        IpAddr::V6("fe80::1".parse().expect("link-local")),
        IpAddr::V6("ff02::1".parse().expect("multicast")),
        IpAddr::V6(Ipv4Addr::new(127, 0, 0, 1).to_ipv6_mapped()),
        IpAddr::V6(Ipv4Addr::new(169, 254, 169, 254).to_ipv6_mapped()),
        IpAddr::V6(Ipv4Addr::new(10, 0, 0, 1).to_ipv6_mapped()),
        IpAddr::V6(Ipv4Addr::new(100, 64, 0, 1).to_ipv6_mapped()),
    ];

    for ip in blocked_ips {
        let resolver = MockDnsResolver {
            addrs: vec![
                // Include a public IP alongside the blocked IP to verify ANY blocked IP fails closed.
                SocketAddr::new(IpAddr::V4(Ipv4Addr::new(104, 18, 32, 7)), 443),
                SocketAddr::new(ip, 443),
            ],
        };
        let err = resolve_and_validate_addrs(&resolver, "api.openai.com", 443)
            .await
            .expect_err("blocked IP in DNS answer must fail");
        assert_eq!(err, SsrfError::BlockedIpAddress(ip));
    }

    assert_eq!(
        resolve_and_validate_addrs(&EmptyDnsResolver, "api.openai.com", 443).await,
        Err(SsrfError::EmptyResolution)
    );

    let public_resolver = MockDnsResolver {
        addrs: vec![SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(104, 18, 32, 7)),
            443,
        )],
    };
    let pinned = resolve_and_validate_addrs(&public_resolver, "api.openai.com", 443)
        .await
        .expect("public IP allowed");
    assert_eq!(pinned.len(), 1);

    // 2. Full BrokerEgressClient SSRF check: DNS resolving to metadata 169.254.169.254
    //    or IPv4-mapped ::ffff:169.254.169.254 is rejected before TCP connect.
    let dir = TempDir::new().expect("tempdir");
    let (custody, tenant, cred_id) =
        setup_custody_with_credential(&dir, "openai", b"sk-ssrf-test").await;
    let mut registry = EgressProfileRegistry::new();
    registry
        .register(openai_profile(
            "https://api.openai.com",
            EgressCaps::default(),
        ))
        .expect("register");

    let ssrf_client = BrokerEgressClient::with_resolver(
        registry,
        Arc::clone(&custody),
        Arc::new(MockDnsResolver {
            addrs: vec![SocketAddr::new(
                IpAddr::V6(Ipv4Addr::new(169, 254, 169, 254).to_ipv6_mapped()),
                443,
            )],
        }),
    );
    let (dispatch, _tx) = synthetic_dispatch(tenant, "openai", cred_id, 100, 1_000);
    let err = ssrf_client
        .execute_unary(
            &dispatch,
            &EgressInferenceRequest {
                provider: "openai".to_owned(),
                deployment: "prod-us".to_owned(),
                model: "gpt-4o-mini".to_owned(),
                operation: ProviderOperationWire::ChatCompletions,
                requested_path: None,
                body_json: json!({"prompt": "hello"}),
                stream: false,
            },
        )
        .await
        .expect_err("SSRF IMDS address must be blocked");
    assert!(matches!(
        err,
        EgressError::Ssrf(SsrfError::BlockedIpAddress(_))
    ));
}

#[tokio::test]
async fn egress_ssrf_boundary_and_egress_error_strip_with_loopback_listener() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let server_task = tokio::spawn(async move {
        let _ = listener.accept().await;
    });

    let dir = TempDir::new().expect("tempdir");
    let (custody, tenant, cred_id) =
        setup_custody_with_credential(&dir, "openai", b"sk-test-tls").await;
    let mut registry = EgressProfileRegistry::new();
    let prof = openai_profile(
        &format!("https://provider.example:{port}"),
        EgressCaps::default(),
    );
    registry.register(prof).expect("register");

    let client = BrokerEgressClient::with_resolver(
        registry,
        custody,
        Arc::new(MockDnsResolver {
            addrs: vec![SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
                port,
            )],
        }),
    );
    let (dispatch, _tx) = synthetic_dispatch(tenant, "openai", cred_id, 100, 1_000);
    let err = client
        .execute_unary(
            &dispatch,
            &EgressInferenceRequest {
                provider: "openai".to_owned(),
                deployment: "prod-us".to_owned(),
                model: "gpt-4o-mini".to_owned(),
                operation: ProviderOperationWire::ChatCompletions,
                requested_path: None,
                body_json: json!({}),
                stream: false,
            },
        )
        .await
        .expect_err("loopback 127.0.0.1 must be rejected by SSRF guard");
    assert!(matches!(
        err,
        EgressError::Ssrf(SsrfError::BlockedIpAddress(IpAddr::V4(ip))) if ip.is_loopback()
    ));
    server_task.abort();
}

#[tokio::test]
async fn egress_caps_enforce_request_size_spend_cap_concurrency_and_cancel() {
    let dir = TempDir::new().expect("tempdir");
    let (custody, tenant, cred_id) =
        setup_custody_with_credential(&dir, "openai", b"sk-caps").await;
    let mut registry = EgressProfileRegistry::new();
    registry
        .register(openai_profile(
            "https://api.openai.com",
            EgressCaps {
                max_request_bytes: 32,
                max_response_bytes: 128,
                max_chunk_bytes: 64,
                request_timeout_ms: 5_000,
                max_concurrent_requests: 1,
                max_spend_microcents: 500,
            },
        ))
        .expect("register");

    let client = BrokerEgressClient::with_resolver(
        registry,
        Arc::clone(&custody),
        Arc::new(MockDnsResolver {
            addrs: vec![SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(104, 18, 32, 7)),
                443,
            )],
        }),
    );

    // 1. Spend cap exceeded.
    let (over_spend, _tx) = synthetic_dispatch(tenant.clone(), "openai", cred_id, 600, 600);
    let err = client
        .execute_unary(
            &over_spend,
            &EgressInferenceRequest {
                provider: "openai".to_owned(),
                deployment: "prod-us".to_owned(),
                model: "gpt-4o-mini".to_owned(),
                operation: ProviderOperationWire::ChatCompletions,
                requested_path: None,
                body_json: json!({"a": 1}),
                stream: false,
            },
        )
        .await
        .expect_err("spend cap exceeded");
    assert!(matches!(err, EgressError::SpendCapExceeded { .. }));

    // 2. Request byte cap exceeded.
    let (dispatch, tx) = synthetic_dispatch(tenant.clone(), "openai", cred_id, 100, 500);
    let err = client
        .execute_unary(
            &dispatch,
            &EgressInferenceRequest {
                provider: "openai".to_owned(),
                deployment: "prod-us".to_owned(),
                model: "gpt-4o-mini".to_owned(),
                operation: ProviderOperationWire::ChatCompletions,
                requested_path: None,
                body_json: json!({"prompt": "this string definitely exceeds 32 bytes"}),
                stream: false,
            },
        )
        .await
        .expect_err("request size exceeded");
    assert!(matches!(err, EgressError::RequestTooLarge { .. }));

    // 3. Replica fence cancellation aborts immediately before egress.
    tx.send(true).expect("signal cancel");
    let err = client
        .execute_unary(
            &dispatch,
            &EgressInferenceRequest {
                provider: "openai".to_owned(),
                deployment: "prod-us".to_owned(),
                model: "gpt-4o-mini".to_owned(),
                operation: ProviderOperationWire::ChatCompletions,
                requested_path: None,
                body_json: json!({"a": 1}),
                stream: false,
            },
        )
        .await
        .expect_err("canceled dispatch must abort");
    assert_eq!(err, EgressError::Canceled);
}

#[tokio::test]
async fn egress_error_strip_discards_upstream_error_bodies_and_headers_end_to_end() {
    // Verify BrokerAdmissionService signs, binds, and admits an invocation whose
    // BoundDispatchContext carries all signed fields intact, then exercise real
    // tokio-rustls upstream 502 error-body/header stripping and 302 redirect blocking.
    let dir = TempDir::new().expect("tempdir");
    let ledger = Arc::new(
        TursoAdmissionLedgerStore::open(dir.path().join("admission.db"))
            .await
            .expect("ledger"),
    );
    let tenant = TenantId::parse("acme.example").expect("tenant");
    let perms = Arc::new(LocalPermissionBackend::new(AuthzCallerRole::Broker));
    perms
        .provision_tenant_store(&tenant)
        .await
        .expect("provision");
    let account = AccountId::new_v4();
    let cred_id = Uuid::new_v4();
    perms
        .write_tuples(
            &tenant,
            AuthzModule::Inference,
            &[AuthzTuple {
                object: AuthzObject {
                    object_type: AuthzObjectType::UserCredential,
                    id: cred_id.to_string(),
                },
                relation: AuthzRelation::new("use"),
                subject: AuthzSubject {
                    subject_type: AuthzSubjectType::Account,
                    id: account.to_string(),
                    relation: None,
                },
            }],
        )
        .await
        .expect("tuple");

    let device_keys = Arc::new(DeviceKeyRegistry::new());
    let sk = SigningKey::random(&mut OsRng);
    let pubkey_sec1 = sk
        .verifying_key()
        .to_encoded_point(false)
        .as_bytes()
        .to_vec();
    let jkt = p256_jwk_thumbprint(&pubkey_sec1).expect("jkt");
    let dev_id = Uuid::new_v4();
    let now = Utc::now().timestamp_millis();
    let key_exp = now + 60_000;
    let transcript = build_device_enroll_pop_transcript(
        "trust.example",
        &tenant,
        account,
        dev_id,
        "ch",
        key_exp,
    );
    let pop: Signature = sk.sign(&transcript);
    let idp_intent = VerifiedIdpIntent {
        account_id: account,
        tenant_id: tenant.clone(),
        device_jkt: Some(jkt),
        is_tenant_admin: false,
        proof_id: "idp-proof-1".to_owned(),
    };
    device_keys
        .enroll_device_key(EnrollDeviceKeyInput {
            trust_domain: "trust.example",
            provenance: AuthProvenance::ExternalIdp,
            idp_intent: &idp_intent,
            device_key_id: dev_id,
            public_key_sec1: pubkey_sec1,
            nonce: "ch",
            expires_at_ms: key_exp,
            now_ms: now,
            pop_signature: &pop.to_bytes(),
        })
        .expect("device key");

    let fence = Arc::new(BrokerReplicaFence::new(
        Uuid::new_v4(),
        Arc::clone(&ledger) as Arc<dyn ReplicaFenceStore>,
        Arc::new(SystemBootClock),
    ));
    fence.tick_once().await.expect("tick");
    let svc = BrokerAdmissionService::new(
        "broker-1",
        ledger,
        perms,
        device_keys,
        fence,
        TraceRecorder::new(),
    );

    let room = RoomScopeKey {
        scope_id: "general".to_owned(),
    };
    let payer = PayerSelection::Member {
        account_id: account,
        credential_id: cred_id,
    };
    let budget = BudgetLimits {
        max_input_tokens: 4_000,
        max_output_tokens: 1_000,
        max_spend_microcents: 5_000,
        max_steps: 2,
    };
    let nonce = [0x44; 32];
    let exp = now + 60_000;
    let mut signed = SignedInferenceInvocation {
        trust_domain: "trust.example".to_owned(),
        tenant_id: tenant.clone(),
        account_id: account,
        device_key_id: dev_id,
        extension_id: "ai-chatbot".to_owned(),
        extension_digest_sha256: [0x99; 32],
        room_scope: room.clone(),
        provider: "openai".to_owned(),
        payer,
        command_digest_sha256: [0x77; 32],
        nonce,
        expires_at_ms: exp,
        budget,
        signature: Vec::new(),
    };
    let sig: Signature = sk.sign(&signed.canonical_transcript());
    signed.signature = sig.to_bytes().to_vec();

    let admitted = svc
        .admit_invocation(AdmitInvocationInput {
            attempt_id: AttemptId::new_v4(),
            run_id: RunId::new_v4(),
            step_id: StepId::new(0),
            trust_domain: "trust.example".to_owned(),
            tenant_id: tenant,
            principal: InvocationPrincipal::LocalMember {
                account_id: account,
                provenance: AuthProvenance::ExternalIdp,
            },
            extension_id: "ai-chatbot".to_owned(),
            extension_digest_sha256: [0x99; 32],
            provider: "openai".to_owned(),
            payer,
            room_or_space: room,
            holder: HolderInstance {
                spiffe_id: SpiffeId::parse("spiffe://trust.example/ns/waddle/sa/cell-a")
                    .expect("spiffe"),
                incarnation_pubkey: IncarnationPubKey::from_bytes([1u8; 32]),
                tls_exporter_proof: TlsExporterProof::new(vec![1u8; 32]).expect("proof"),
            },
            command_digest_sha256: [0x77; 32],
            invocation_nonce: nonce,
            expires_at_ms: exp,
            reserved_spend_microcents: 500,
            budget,
            signed_invocation: Some(signed),
            now_ms: now,
        })
        .await
        .expect("admitted");

    assert_eq!(admitted.dispatch_context.provider, "openai");
    assert_eq!(admitted.dispatch_context.extension_id, "ai-chatbot");
    assert_eq!(
        admitted.dispatch_context.extension_digest_sha256,
        [0x99; 32]
    );
    assert_eq!(admitted.dispatch_context.command_digest_sha256, [0x77; 32]);

    // Enroll sentinel secret in custody under the admitted credential_id.
    let ledger = Arc::new(
        TursoLedgerStore::open_in_dir(dir.path())
            .await
            .expect("ledger"),
    );
    let kms = Arc::new(LocalKmsProvider::new());
    let hierarchy = Arc::new(KeyHierarchyService::new("trust.example", kms));
    let custody = Arc::new(
        BrokerCustodyService::new(hierarchy, ledger)
            .await
            .expect("custody"),
    );
    let receipt = custody
        .enroll(EnrollCredentialInput {
            tenant_id: admitted.dispatch_context.tenant_id.clone(),
            account_id: Some(account),
            provider: "openai".to_owned(),
            nonce: "enroll-502-302-live".to_owned(),
            payload_digest_sha256: [0x22; 32],
            expires_at_ms: now + 60_000,
            plaintext: SecretBytes::new(b"sk-live-secret-openai-999".to_vec()).expect("secret"),
        })
        .await
        .expect("enroll");

    // Re-bind admitted dispatch payer to the enrolled custody credential_id.
    let mut live_dispatch = admitted;
    live_dispatch.dispatch_context.payer = PayerSelection::Member {
        account_id: account,
        credential_id: receipt.credential_id,
    };

    let tls = generate_test_tls_material("provider.example");
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let acceptor = TlsAcceptor::from(tls.server_config);
    let connections = Arc::new(AtomicUsize::new(0));
    let conn_counter = Arc::clone(&connections);

    let server_task = tokio::spawn(async move {
        // Connection 1: 502 Bad Gateway with sensitive header + JSON body.
        let (tcp, _) = listener.accept().await.expect("accept 502");
        conn_counter.fetch_add(1, Ordering::SeqCst);
        let mut tls_stream = acceptor.accept(tcp).await.expect("tls 502");
        let req_raw = read_http_request(&mut tls_stream).await;
        assert!(
            req_raw
                .to_ascii_lowercase()
                .contains("authorization: bearer sk-live-secret-openai-999"),
            "missing expected Authorization header in request: {req_raw}"
        );
        let body_502 = r#"{"error":{"message":"upstream key sk-leak-sentinel invalid"}}"#;
        let resp_502 = format!(
            "HTTP/1.1 502 Bad Gateway\r\nContent-Type: application/json\r\nX-Upstream-Secret: sk-leak-sentinel\r\nSet-Cookie: session=secret-cookie\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body_502.len(),
            body_502
        );
        tls_stream
            .write_all(resp_502.as_bytes())
            .await
            .expect("write 502");
        let _ = tls_stream.shutdown().await;

        // Connection 2: 302 Found pointing to cloud IMDS metadata endpoint.
        let (tcp2, _) = listener.accept().await.expect("accept 302");
        conn_counter.fetch_add(1, Ordering::SeqCst);
        let mut tls_stream2 = acceptor.accept(tcp2).await.expect("tls 302");
        let _ = read_http_request(&mut tls_stream2).await;
        let resp_302 = "HTTP/1.1 302 Found\r\nLocation: http://169.254.169.254/latest/meta-data\r\nX-Redirect-Secret: redirect-sentinel\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        tls_stream2
            .write_all(resp_302.as_bytes())
            .await
            .expect("write 302");
        let _ = tls_stream2.shutdown().await;
    });

    let mut registry = EgressProfileRegistry::new();
    let mut prof = openai_profile(
        &format!("https://provider.example:{}", addr.port()),
        EgressCaps::default(),
    );
    prof.pinned_ca_pem = Some(tls.ca_pem);
    registry.register(prof).expect("register");

    let client = BrokerEgressClient::with_loopback_tls_test_override(
        registry,
        custody,
        Arc::new(MockDnsResolver {
            addrs: vec![SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(104, 18, 32, 7)),
                addr.port(),
            )],
        }),
        addr,
    );

    let req = EgressInferenceRequest {
        provider: "openai".to_owned(),
        deployment: "prod-us".to_owned(),
        model: "gpt-4o-mini".to_owned(),
        operation: ProviderOperationWire::ChatCompletions,
        requested_path: None,
        body_json: json!({"prompt": "hi"}),
        stream: false,
    };

    let err_502 = client
        .execute_unary(&live_dispatch, &req)
        .await
        .expect_err("502 must fail");
    assert_eq!(err_502, EgressError::UpstreamStatus { status: 502 });
    let msg_502 = err_502.to_string();
    assert!(!msg_502.contains("sk-leak-sentinel"));
    assert!(!msg_502.contains("secret-cookie"));
    assert!(!msg_502.contains("X-Upstream-Secret"));
    assert!(!msg_502.contains("Set-Cookie"));
    assert!(!msg_502.contains("sk-live-secret-openai-999"));

    let err_302 = client
        .execute_unary(&live_dispatch, &req)
        .await
        .expect_err("302 must be blocked");
    assert_eq!(err_302, EgressError::RedirectBlocked { status: 302 });
    let msg_302 = err_302.to_string();
    assert!(!msg_302.contains("169.254.169.254"));
    assert!(!msg_302.contains("redirect-sentinel"));

    server_task.await.expect("mock server");
    assert_eq!(connections.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn egress_ssrf_and_pinned_ca_pem_reject_literal_ip_origins_and_untrusted_cas() {
    // 1. Literal IP origins are rejected immediately at registration time.
    let mut registry = EgressProfileRegistry::new();
    for literal_origin in [
        "https://127.0.0.1",
        "https://127.0.0.1:8443",
        "https://[::1]",
        "https://[::ffff:127.0.0.1]",
        "https://169.254.169.254",
        "https://104.18.32.7",
    ] {
        assert_eq!(
            registry.register(openai_profile(literal_origin, EgressCaps::default())),
            Err(ProfileError::InvalidOrigin),
            "literal IP origin {literal_origin} must be rejected"
        );
    }

    // 2. Expanded IPv6 transition & discard-only ranges are rejected by SSRF validation.
    for transition_ip in [
        "::127.0.0.1",
        "::169.254.169.254",
        "::ffff:0:127.0.0.1",
        "64:ff9b::127.0.0.1",
        "64:ff9b:1::1",
        "2002:7f00:1::1",
        "2001::1",
        "100::1",
    ] {
        let ip: IpAddr = IpAddr::V6(transition_ip.parse().expect("valid v6"));
        let resolver = MockDnsResolver {
            addrs: vec![SocketAddr::new(ip, 443)],
        };
        assert_eq!(
            resolve_and_validate_addrs(&resolver, "api.openai.com", 443).await,
            Err(SsrfError::BlockedIpAddress(ip))
        );
    }

    // 3. Strict pinned_ca_pem verification: untrusted CA rejected, pinned CA accepted.
    let dir = TempDir::new().expect("tempdir");
    let (custody, tenant, cred_id) =
        setup_custody_with_credential(&dir, "openai", b"sk-tls-pin-check").await;
    let trusted_tls = generate_test_tls_material("provider.example");
    let untrusted_tls = generate_test_tls_material("provider.example");

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let untrusted_acceptor = TlsAcceptor::from(untrusted_tls.server_config);
    let trusted_acceptor = TlsAcceptor::from(trusted_tls.server_config);

    let server_task = tokio::spawn(async move {
        // First connection presents untrusted CA -> client aborts TLS handshake.
        let (tcp1, _) = listener.accept().await.expect("accept untrusted");
        let _ = untrusted_acceptor.accept(tcp1).await;

        // Second connection presents trusted CA -> client sends request and receives 200 OK.
        let (tcp2, _) = listener.accept().await.expect("accept trusted");
        let mut tls_stream = trusted_acceptor.accept(tcp2).await.expect("trusted tls");
        let _ = read_http_request(&mut tls_stream).await;
        let ok_body = r#"{"ok":true}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            ok_body.len(),
            ok_body
        );
        tls_stream.write_all(resp.as_bytes()).await.expect("write");
        let _ = tls_stream.shutdown().await;
    });

    let mut reg = EgressProfileRegistry::new();
    let mut prof = openai_profile(
        &format!("https://provider.example:{}", addr.port()),
        EgressCaps::default(),
    );
    prof.pinned_ca_pem = Some(trusted_tls.ca_pem);
    reg.register(prof).expect("register");

    let client = BrokerEgressClient::with_loopback_tls_test_override(
        reg,
        custody,
        Arc::new(MockDnsResolver {
            addrs: vec![SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(104, 18, 32, 7)),
                addr.port(),
            )],
        }),
        addr,
    );
    let (dispatch, _tx) = synthetic_dispatch(tenant, "openai", cred_id, 100, 1_000);
    let req = EgressInferenceRequest {
        provider: "openai".to_owned(),
        deployment: "prod-us".to_owned(),
        model: "gpt-4o-mini".to_owned(),
        operation: ProviderOperationWire::ChatCompletions,
        requested_path: None,
        body_json: json!({"prompt": "pin"}),
        stream: false,
    };

    let untrusted_err = client
        .execute_unary(&dispatch, &req)
        .await
        .expect_err("untrusted CA must fail");
    assert_eq!(untrusted_err, EgressError::TransportFailed);

    let ok_resp = client
        .execute_unary(&dispatch, &req)
        .await
        .expect("pinned CA must succeed");
    assert_eq!(ok_resp.status, 200);
    assert_eq!(ok_resp.body, br#"{"ok":true}"#);

    server_task.await.expect("tls pin server");
}

#[tokio::test]
async fn egress_streaming_progressive_first_chunk_split_sse_and_drop_cancel() {
    let dir = TempDir::new().expect("tempdir");
    let (custody, tenant, cred_id) =
        setup_custody_with_credential(&dir, "openai", b"sk-stream-test").await;
    let tls = generate_test_tls_material("provider.example");
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let acceptor = TlsAcceptor::from(tls.server_config);

    let release_second_chunk = Arc::new(Notify::new());
    let release_clone = Arc::clone(&release_second_chunk);
    let drop_observed = Arc::new(Notify::new());
    let drop_observed_clone = Arc::clone(&drop_observed);

    let server_task = tokio::spawn(async move {
        // Stream 1: progressive first chunk, split `\n` line across two writes, clean [DONE].
        let (tcp1, _) = listener.accept().await.expect("accept stream 1");
        let mut s1 = acceptor.accept(tcp1).await.expect("tls 1");
        let _ = read_http_request(&mut s1).await;
        s1.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
        )
        .await
        .expect("headers 1");
        let c0 = b"data: {\"delta\":\"first\"}\n\n";
        s1.write_all(format!("{:x}\r\n", c0.len()).as_bytes())
            .await
            .expect("len 0");
        s1.write_all(c0).await.expect("c0");
        s1.write_all(b"\r\n").await.expect("crlf 0");
        s1.flush().await.expect("flush 0");

        // Wait until client actually proves it received chunk 0 before EOF!
        release_clone.notified().await;

        let part1 = b"data: {\"del";
        s1.write_all(format!("{:x}\r\n", part1.len()).as_bytes())
            .await
            .expect("len part1");
        s1.write_all(part1).await.expect("part1");
        s1.write_all(b"\r\n").await.expect("crlf part1");
        s1.flush().await.expect("flush part1");

        let part2 = b"ta\":\"split\"}\n\ndata: [DONE]\n\n";
        s1.write_all(format!("{:x}\r\n", part2.len()).as_bytes())
            .await
            .expect("len part2");
        s1.write_all(part2).await.expect("part2");
        s1.write_all(b"\r\n0\r\n\r\n").await.expect("eof 1");
        let _ = s1.shutdown().await;

        // Stream 2: mid-stream drop must close socket promptly.
        let (tcp2, _) = listener.accept().await.expect("accept stream 2");
        let mut s2 = acceptor.accept(tcp2).await.expect("tls 2");
        let _ = read_http_request(&mut s2).await;
        s2.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
        )
        .await
        .expect("headers 2");
        s2.write_all(format!("{:x}\r\n", c0.len()).as_bytes())
            .await
            .expect("len 2");
        s2.write_all(c0).await.expect("c0 2");
        s2.write_all(b"\r\n").await.expect("crlf 2");
        s2.flush().await.expect("flush 2");

        let mut read_buf = [0u8; 64];
        loop {
            match s2.read(&mut read_buf).await {
                Ok(0) | Err(_) => {
                    drop_observed_clone.notify_one();
                    break;
                }
                Ok(_) => {}
            }
        }

        // Stream 3: malformed unterminated SSE at EOF must yield MalformedUpstreamResponse.
        let (tcp3, _) = listener.accept().await.expect("accept stream 3");
        let mut s3 = acceptor.accept(tcp3).await.expect("tls 3");
        let _ = read_http_request(&mut s3).await;
        let bad_sse = b"data: {\"unterminated";
        let resp3 = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            bad_sse.len()
        );
        s3.write_all(resp3.as_bytes()).await.expect("hdr 3");
        s3.write_all(bad_sse).await.expect("body 3");
        let _ = s3.shutdown().await;

        // Stream 4: unary call proves concurrency permit (=1) was released after Drop.
        let (tcp4, _) = listener.accept().await.expect("accept unary 4");
        let mut s4 = acceptor.accept(tcp4).await.expect("tls 4");
        let _ = read_http_request(&mut s4).await;
        let ok_body = br#"{"permit_released":true}"#;
        let resp4 = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            ok_body.len()
        );
        s4.write_all(resp4.as_bytes()).await.expect("hdr 4");
        s4.write_all(ok_body).await.expect("body 4");
        let _ = s4.shutdown().await;
    });

    let mut reg = EgressProfileRegistry::new();
    let mut prof = openai_profile(
        &format!("https://provider.example:{}", addr.port()),
        EgressCaps {
            max_concurrent_requests: 1,
            request_timeout_ms: 5_000,
            ..EgressCaps::default()
        },
    );
    prof.pinned_ca_pem = Some(tls.ca_pem);
    reg.register(prof).expect("register");

    let client = BrokerEgressClient::with_loopback_tls_test_override(
        reg,
        custody,
        Arc::new(MockDnsResolver {
            addrs: vec![SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(104, 18, 32, 7)),
                addr.port(),
            )],
        }),
        addr,
    );
    let (dispatch, _tx) = synthetic_dispatch(tenant, "openai", cred_id, 100, 1_000);
    let stream_req = EgressInferenceRequest {
        provider: "openai".to_owned(),
        deployment: "prod-us".to_owned(),
        model: "gpt-4o-mini".to_owned(),
        operation: ProviderOperationWire::ChatCompletions,
        requested_path: None,
        body_json: json!({"prompt": "stream", "stream": true}),
        stream: true,
    };

    // 1. First chunk arrives BEFORE server sends chunk 1 or EOF!
    let mut stream1 = client
        .execute_streaming(&dispatch, &stream_req)
        .await
        .expect("stream 1 open");
    let first = tokio::time::timeout(Duration::from_secs(2), stream1.next_chunk())
        .await
        .expect("chunk 0 timely")
        .expect("chunk 0 ok")
        .expect("chunk 0 some");
    assert_eq!(first.data, b"data: {\"delta\":\"first\"}\n\n");

    // Signal server to emit split `\n` chunks + [DONE].
    release_second_chunk.notify_one();
    let mut collected = first.data;
    while let Some(chunk) = stream1.next_chunk().await.expect("next chunk ok") {
        collected.extend_from_slice(&chunk.data);
    }
    assert!(String::from_utf8_lossy(&collected).contains("\"delta\":\"split\""));
    assert!(stream1.total_bytes() >= collected.len());
    drop(stream1);
    tokio::time::sleep(Duration::from_millis(20)).await;

    // 2. Drop mid-stream closes socket and frees concurrency permit.
    let mut stream2 = client
        .execute_streaming(&dispatch, &stream_req)
        .await
        .expect("stream 2 open");
    let _ = stream2
        .next_chunk()
        .await
        .expect("chunk 0 ok")
        .expect("chunk 0 some");
    drop(stream2);
    tokio::time::timeout(Duration::from_secs(2), drop_observed.notified())
        .await
        .expect("server must observe TLS disconnect after EgressStream drop");
    tokio::time::sleep(Duration::from_millis(20)).await;

    // 3. Malformed unterminated SSE at EOF fails with MalformedUpstreamResponse.
    let mut stream3 = client
        .execute_streaming(&dispatch, &stream_req)
        .await
        .expect("stream 3 open");
    let mut err3 = None;
    loop {
        match stream3.next_chunk().await {
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(e) => {
                err3 = Some(e);
                break;
            }
        }
    }
    assert_eq!(err3, Some(EgressError::MalformedUpstreamResponse));
    drop(stream3);
    tokio::time::sleep(Duration::from_millis(20)).await;

    // 4. Subsequent unary call acquires the single concurrency permit cleanly.
    let unary_req = EgressInferenceRequest {
        stream: false,
        body_json: json!({"prompt": "after-drop"}),
        ..stream_req
    };
    let resp4 = client
        .execute_unary(&dispatch, &unary_req)
        .await
        .expect("permit released");
    assert_eq!(resp4.body, br#"{"permit_released":true}"#);

    server_task.await.expect("server task");
}

#[tokio::test]
async fn egress_caps_wire_response_chunk_and_full_execution_timeout() {
    let dir = TempDir::new().expect("tempdir");
    let (custody, tenant, cred_id) =
        setup_custody_with_credential(&dir, "openai", b"sk-wire-caps").await;
    let tls = generate_test_tls_material("provider.example");
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let acceptor = TlsAcceptor::from(tls.server_config);

    let server_task = tokio::spawn(async move {
        // 1. Response exceeding max_response_bytes (40 bytes) sent in 18-byte chunks (<= max_chunk_bytes 20).
        let (tcp1, _) = listener.accept().await.expect("accept 1");
        let mut s1 = acceptor.accept(tcp1).await.expect("tls 1");
        let _ = read_http_request(&mut s1).await;
        s1.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n",
        )
        .await
        .expect("hdr 1");
        for chunk in [
            b"{\"msg\":\"aaaaaaaaaa" as &[u8],
            b"bbbbbbbbbbbbbbbbbb",
            b"cccccccccccccccccc\"}",
        ] {
            s1.write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
                .await
                .expect("len 1");
            s1.write_all(chunk).await.expect("chunk 1");
            s1.write_all(b"\r\n").await.expect("crlf 1");
            s1.flush().await.expect("flush 1");
        }
        s1.write_all(b"0\r\n\r\n").await.expect("eof 1");
        let _ = s1.shutdown().await;

        // 2. Streaming chunk exceeding max_chunk_bytes (20 bytes).
        let (tcp2, _) = listener.accept().await.expect("accept 2");
        let mut s2 = acceptor.accept(tcp2).await.expect("tls 2");
        let _ = read_http_request(&mut s2).await;
        let big_chunk = b"data: {\"delta\":\"01234567890123456789\"}\n\n";
        let resp2 = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            big_chunk.len()
        );
        s2.write_all(resp2.as_bytes()).await.expect("hdr 2");
        s2.write_all(big_chunk).await.expect("body 2");
        let _ = s2.shutdown().await;
    });

    let mut reg = EgressProfileRegistry::new();
    let mut prof = openai_profile(
        &format!("https://provider.example:{}", addr.port()),
        EgressCaps {
            max_request_bytes: 256,
            max_response_bytes: 40,
            max_chunk_bytes: 20,
            request_timeout_ms: 1_500,
            max_concurrent_requests: 1,
            max_spend_microcents: 1_000,
        },
    );
    prof.pinned_ca_pem = Some(tls.ca_pem.clone());
    reg.register(prof).expect("register");

    let client = BrokerEgressClient::with_loopback_tls_test_override(
        reg.clone(),
        Arc::clone(&custody),
        Arc::new(MockDnsResolver {
            addrs: vec![SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(104, 18, 32, 7)),
                addr.port(),
            )],
        }),
        addr,
    );
    let (dispatch, _tx) = synthetic_dispatch(tenant.clone(), "openai", cred_id, 100, 1_000);

    let err_resp_large = client
        .execute_unary(
            &dispatch,
            &EgressInferenceRequest {
                provider: "openai".to_owned(),
                deployment: "prod-us".to_owned(),
                model: "gpt-4o-mini".to_owned(),
                operation: ProviderOperationWire::ChatCompletions,
                requested_path: None,
                body_json: json!({"prompt": "big"}),
                stream: false,
            },
        )
        .await
        .expect_err("response too large");
    assert_eq!(err_resp_large, EgressError::ResponseTooLarge { max: 40 });

    let mut stream = client
        .execute_streaming(
            &dispatch,
            &EgressInferenceRequest {
                provider: "openai".to_owned(),
                deployment: "prod-us".to_owned(),
                model: "gpt-4o-mini".to_owned(),
                operation: ProviderOperationWire::ChatCompletions,
                requested_path: None,
                body_json: json!({"prompt": "chunk"}),
                stream: true,
            },
        )
        .await
        .expect("stream open");
    let err_chunk = stream.next_chunk().await.expect_err("chunk too large");
    assert!(matches!(
        err_chunk,
        EgressError::ChunkTooLarge { max: 20, .. }
    ));
    drop(stream);
    server_task.await.expect("server task");

    // 3. Full-execution timeout wraps DNS resolution + custody + HTTP.
    let mut timeout_reg = EgressProfileRegistry::new();
    timeout_reg
        .register(openai_profile(
            "https://provider.example",
            EgressCaps {
                request_timeout_ms: 100,
                max_concurrent_requests: 1,
                ..EgressCaps::default()
            },
        ))
        .expect("register");
    let slow_client = BrokerEgressClient::with_resolver(
        timeout_reg,
        custody,
        Arc::new(SlowDnsResolver {
            delay: Duration::from_millis(350),
            addrs: vec![SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(104, 18, 32, 7)),
                443,
            )],
        }),
    );
    let started = Instant::now();
    let timeout_err = slow_client
        .execute_unary(
            &dispatch,
            &EgressInferenceRequest {
                provider: "openai".to_owned(),
                deployment: "prod-us".to_owned(),
                model: "gpt-4o-mini".to_owned(),
                operation: ProviderOperationWire::ChatCompletions,
                requested_path: None,
                body_json: json!({"prompt": "slow-dns"}),
                stream: false,
            },
        )
        .await
        .expect_err("slow DNS must hit full request_timeout_ms");
    assert_eq!(timeout_err, EgressError::Timeout);
    assert!(
        started.elapsed() < Duration::from_millis(280),
        "elapsed {:?} exceeded bounded request_timeout_ms",
        started.elapsed()
    );
}

#[tokio::test]
async fn egress_profile_and_dispatch_binding_reject_body_overrides_and_policy_mismatch() {
    let dir = TempDir::new().expect("tempdir");
    let (custody, tenant, cred_id) =
        setup_custody_with_credential(&dir, "openai", b"sk-bind-check").await;
    let mut registry = EgressProfileRegistry::new();
    registry
        .register_with_policy(
            openai_profile("https://api.openai.com", EgressCaps::default()),
            EgressBindingPolicy {
                expected_trust_domain: Some("trust.example".to_owned()),
                allowed_extension_ids: vec!["ai-chatbot".to_owned()],
                allowed_extension_digests: vec![[0xAA; 32]],
            },
        )
        .expect("register scoped");
    let client = BrokerEgressClient::new(registry, custody);
    let (dispatch, _tx) = synthetic_dispatch(tenant, "openai", cred_id, 100, 1_000);

    // 1. body_json["stream"] override rejected in both unary & streaming.
    let err_stream_override = client
        .execute_unary(
            &dispatch,
            &EgressInferenceRequest {
                provider: "openai".to_owned(),
                deployment: "prod-us".to_owned(),
                model: "gpt-4o-mini".to_owned(),
                operation: ProviderOperationWire::ChatCompletions,
                requested_path: None,
                body_json: json!({"stream": true}),
                stream: false,
            },
        )
        .await
        .expect_err("stream override rejected");
    assert!(matches!(
        err_stream_override,
        EgressError::BindingMismatch(_)
    ));

    // 2. body_json["model"] mismatch rejected.
    let err_model_override = client
        .execute_unary(
            &dispatch,
            &EgressInferenceRequest {
                provider: "openai".to_owned(),
                deployment: "prod-us".to_owned(),
                model: "gpt-4o-mini".to_owned(),
                operation: ProviderOperationWire::ChatCompletions,
                requested_path: None,
                body_json: json!({"model": "gpt-4o"}),
                stream: false,
            },
        )
        .await
        .expect_err("model override rejected");
    assert!(matches!(
        err_model_override,
        EgressError::BindingMismatch(_)
    ));

    // 3. body_json["max_output_tokens"] exceeding admitted budget rejected.
    let err_tokens = client
        .execute_unary(
            &dispatch,
            &EgressInferenceRequest {
                provider: "openai".to_owned(),
                deployment: "prod-us".to_owned(),
                model: "gpt-4o-mini".to_owned(),
                operation: ProviderOperationWire::ChatCompletions,
                requested_path: None,
                body_json: json!({"max_output_tokens": 9_999}),
                stream: false,
            },
        )
        .await
        .expect_err("token cap override rejected");
    assert!(matches!(err_tokens, EgressError::BindingMismatch(_)));

    // 4. Policy mismatch (wrong extension_id) rejected.
    let mut wrong_ext_dispatch = dispatch;
    wrong_ext_dispatch.dispatch_context.extension_id = "other-plugin".to_owned();
    let err_policy = client
        .execute_unary(
            &wrong_ext_dispatch,
            &EgressInferenceRequest {
                provider: "openai".to_owned(),
                deployment: "prod-us".to_owned(),
                model: "gpt-4o-mini".to_owned(),
                operation: ProviderOperationWire::ChatCompletions,
                requested_path: None,
                body_json: json!({"prompt": "hi"}),
                stream: false,
            },
        )
        .await
        .expect_err("unscoped extension rejected");
    assert!(matches!(err_policy, EgressError::BindingMismatch(_)));

    // 5. ExpectedEgressBinding mismatch rejected.
    wrong_ext_dispatch.dispatch_context.extension_id = "ai-chatbot".to_owned();
    let err_expected = client
        .execute_unary_checked(
            &wrong_ext_dispatch,
            &EgressInferenceRequest {
                provider: "openai".to_owned(),
                deployment: "prod-us".to_owned(),
                model: "gpt-4o-mini".to_owned(),
                operation: ProviderOperationWire::ChatCompletions,
                requested_path: None,
                body_json: json!({"prompt": "hi"}),
                stream: false,
            },
            ExpectedEgressBinding {
                trust_domain: "trust.example",
                extension_id: "ai-chatbot",
                extension_digest_sha256: [0xAA; 32],
                room_or_space: "other-room",
                command_digest_sha256: [0xBB; 32],
                deployment: "prod-us",
                model: "gpt-4o-mini",
                operation: ProviderOperationWire::ChatCompletions,
            },
        )
        .await
        .expect_err("room mismatch rejected");
    assert!(matches!(err_expected, EgressError::BindingMismatch(_)));
}
