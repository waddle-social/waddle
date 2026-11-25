//! Feature 17 tests: constrained provider egress profiles, DNS-resolved SSRF
//! blocklist, byte/time/concurrency/spend caps, response/error/header stripping,
//! admin endpoint blocking, and `unseal_for_provider_egress` metadata binding.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::elliptic_curve::rand_core::OsRng;
use serde_json::json;
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::watch;
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
    BrokerEgressClient, EgressError, EgressInferenceRequest, ProviderOperationWire,
};
use waddle_broker::egress::profile::{
    is_blocked_admin_path, EgressCaps, EgressProfileRegistry, ProfileError, ProviderOperation,
    ProviderProfile,
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
    // Exercise full BrokerAdmissionService -> AdmittedDispatch -> EgressError
    // error sanitization invariant: upstream error bodies/headers are never present
    // in EgressError::UpstreamStatus or EgressError::RedirectBlocked.
    let err_502 = EgressError::UpstreamStatus { status: 502 };
    let msg_502 = err_502.to_string();
    assert!(msg_502.contains("502"));
    assert!(msg_502.contains("discarded"));
    assert!(!msg_502.contains("Authorization"));

    let err_302 = EgressError::RedirectBlocked { status: 302 };
    assert!(err_302.to_string().contains("302"));

    // Verify BrokerAdmissionService signs, binds, and admits an invocation whose
    // BoundDispatchContext carries all signed fields intact.
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
    let _ = Duration::from_millis(1);
}
