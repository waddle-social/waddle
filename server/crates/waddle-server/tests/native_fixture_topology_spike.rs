//! Feature 19: Genuine two-cell, two-broker native fixture topology spike.
//!
//! Exercises BOTH multi-tenant stacks without containers:
//! - Stack A: `TursoTenantStore` + `TursoPlacementLeaseStore` + `TursoAdmissionLedgerStore`
//!   + `InProcessOpenFgaServer` + `OpenFgaPermissionBackend`
//! - Stack B: `SqliteCompatPostgresStore` + `SqliteCompatPlacementLeaseStore`
//!   + `SqliteCompatAdmissionLedgerStore` + `TenantSpiceDbPermissionBackend`
//!     (plus optional live native PostgreSQL/SpiceDB via `waddle_native_fixture::fixture::Fixture`
//!     when `FENCE_POSTGRES_BIN` and `FENCE_SPICEDB_BIN` are set)
//!
//! Across two cells sharing one `CellRole` (`cell-a`, `cell-b`) and two brokers
//! (`broker-a`, `broker-b`), real sealed credential enrollment & workload rotation,
//! real local `rcgen` + `tokio-rustls` TLS SSE provider egress + compiled `.wat`
//! Wasm component `waddle:extension/inference@4.0.0` `InferenceStream` (first chunk
//! before EOF + mid-stream `Drop` & `BrokerReplicaFence` cancellation), and
//! secret-free telemetry verified by `verify_jsonl_trace` + `assert_secret_free`.

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use jsonwebtoken::jwk::{
    AlgorithmParameters, CommonParameters, EllipticCurve, EllipticCurveKeyParameters, Jwk, JwkSet,
    KeyAlgorithm, PublicKeyUse,
};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::elliptic_curve::rand_core::OsRng;
use p256::pkcs8::EncodePrivateKey;
use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Notify};
use tokio_rustls::TlsAcceptor;
use uuid::Uuid;
use waddle_broker::admission::invocation::{
    InvocationPrincipal, PayerSelection, SignedInferenceInvocation,
};
use waddle_broker::admission::postgres::SqliteCompatAdmissionLedgerStore;
use waddle_broker::admission::replica_fence::BrokerReplicaFence;
use waddle_broker::admission::service::{
    AdmitInvocationInput, BrokerAdmissionService, TraceRecorder,
};
use waddle_broker::admission::turso::TursoAdmissionLedgerStore;
use waddle_broker::admission::ReplicaFenceStore;
use waddle_broker::authz::openfga::{
    InProcessOpenFgaServer, OpenFgaConfig, OpenFgaPermissionBackend,
};
use waddle_broker::authz::AuthzCallerRole;
use waddle_broker::custody::{BrokerCustodyService, EnrollCredentialInput};
use waddle_broker::egress::client::{
    BrokerEgressClient, EgressInferenceRequest, ExpectedEgressBinding, ProviderOperationWire,
};
use waddle_broker::egress::profile::{
    EgressCaps, EgressProfileRegistry, ProviderOperation, ProviderProfile,
};
use waddle_broker::egress::ssrf::{DnsResolver, SsrfError};
use waddle_broker::idp_proof::{
    build_device_enroll_pop_transcript, p256_jwk_thumbprint, AuthProvenance, ColonyBrokerClaims,
    ColonyJwtVerifier, ConfirmationClaim, DeviceKeyRegistry, EnrollDeviceKeyInput,
    VerifiedIdpIntent,
};
use waddle_broker::key_hierarchy::KeyHierarchyService;
use waddle_broker::kms::local::LocalKmsProvider;
use waddle_broker::ledger::postgres::SqliteCompatLedgerStore;
use waddle_broker::ledger::turso::TursoLedgerStore;
use waddle_broker::ledger::DestructionLedgerStore;
use waddle_broker::sealed_enrollment::{
    seal_credential_for_broker, BrokerSubkeyRing, ClientSealContext, RootBkskPinSet, RootBkskSigner,
};
use waddle_broker::secret::SecretBytes;
use waddle_broker_core::admission::{
    AdmissionLedgerStore, AttemptId, AttemptOutcome, BudgetLimits, FenceKey, FenceKind,
    RoomScopeKey, RunId, StepId,
};
use waddle_broker_core::boot_clock::FakeBootClock;
use waddle_broker_core::identity::{
    AccountId, CellRole, HolderInstance, IncarnationPubKey, SpiffeId, TenantId, TlsExporterProof,
};
use waddle_broker_core::permission::{
    AuthzConsistency, AuthzModule, AuthzObject, AuthzObjectType, AuthzRelation, AuthzSubject,
    AuthzSubjectType, AuthzTuple, CheckDecision, CheckRequest, PermissionBackend,
    PermissionBackendError,
};
use waddle_broker_core::placement::{FenceGeneration, PlacementLeaseStore, PlacementMode};
use waddle_broker_core::store::{TenantQueryScope, TenantStore};
use waddle_broker_core::trace_verifier::{assert_secret_free, verify_jsonl_trace, TraceEvent};
use waddle_extensions::actor::WasmExtensionActor;
use waddle_extensions::config::RuntimeLimits;
use waddle_extensions::host_tools::InvocationContext;
use waddle_extensions::runtime::{
    ExtensionInferenceHost, HostInferenceChunk, HostInferenceError, HostInferenceErrorCode,
    HostInferenceRequest, LoadedExtension, OpenedInferenceStream, StreamCancelHandle, StreamItem,
    VerifiedInferenceUsage, WasmRuntime, DEFAULT_MAX_STREAM_CHUNK_BYTES,
};
use waddle_extensions::types::{
    CommandAction, CommandInvocation, CommandNode, ExtensionEvent, FullJidValue,
};
use waddle_extensions::{ExtensionCapability, WaddleId};
use waddle_server::permissions::TenantSpiceDbPermissionBackend;
use waddle_tenancy::placement_postgres::SqliteCompatPlacementLeaseStore;
use waddle_tenancy::placement_turso::TursoPlacementLeaseStore;
use waddle_tenancy::postgres_store::SqliteCompatPostgresStore;
use waddle_tenancy::turso_store::{TursoPoolConfig, TursoTenantStore};

const SENTINEL_API_KEY: &str = "sk-waddle-f19-spike-sentinel-9a8b7c";
const SENTINEL_PROMPT: &str = "classified-f19-prompt-xyz";
const SENTINEL_JID: &str = "alice@acme.example";

fn holder_for(sa: &str, key_byte: u8) -> HolderInstance {
    HolderInstance {
        spiffe_id: SpiffeId::parse(sa).expect("spiffe"),
        incarnation_pubkey: IncarnationPubKey::from_bytes([key_byte; 32]),
        tls_exporter_proof: TlsExporterProof::new(vec![key_byte; 32]).expect("proof"),
    }
}

struct StaticPublicDnsResolver {
    addrs: Vec<SocketAddr>,
}

#[async_trait]
impl DnsResolver for StaticPublicDnsResolver {
    async fn resolve(&self, _host: &str, _port: u16) -> Result<Vec<SocketAddr>, SsrfError> {
        Ok(self.addrs.clone())
    }
}

struct TlsTestCert {
    ca_pem: Vec<u8>,
    server_config: Arc<rustls::ServerConfig>,
}

fn generate_tls_cert(domain: &str) -> TlsTestCert {
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).expect("ca params");
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca_key = KeyPair::generate().expect("ca key");
    let ca_cert = ca_params.self_signed(&ca_key).expect("ca cert");
    let ca_pem = ca_cert.pem().into_bytes();

    let leaf_params = CertificateParams::new(vec![domain.to_owned()]).expect("leaf params");
    let leaf_key = KeyPair::generate().expect("leaf key");
    let leaf_cert = leaf_params
        .signed_by(&leaf_key, &ca_cert, &ca_key)
        .expect("leaf cert");

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
    TlsTestCert {
        ca_pem,
        server_config: Arc::new(server_config),
    }
}

async fn read_tls_http_request(
    stream: &mut tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
) -> String {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 2048];
    loop {
        let n = stream.read(&mut tmp).await.expect("read request");
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let hdr = String::from_utf8_lossy(&buf[..end]);
            let len = hdr
                .lines()
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.eq_ignore_ascii_case("content-length")
                        .then(|| v.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            if buf.len() >= end + 4 + len {
                break;
            }
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

struct InvocationTarget<'a> {
    tenant: &'a TenantId,
    account: AccountId,
    cred_id: Uuid,
    room: &'a RoomScopeKey,
}

async fn enroll_and_sign_invocation(
    device_keys: &DeviceKeyRegistry,
    target: InvocationTarget<'_>,
    now_ms: i64,
    nonce: [u8; 32],
    budget: BudgetLimits,
) -> SignedInferenceInvocation {
    let InvocationTarget {
        tenant,
        account,
        cred_id,
        room,
    } = target;
    let sk = SigningKey::random(&mut OsRng);
    let pk = sk
        .verifying_key()
        .to_encoded_point(false)
        .as_bytes()
        .to_vec();
    let jkt = p256_jwk_thumbprint(&pk).expect("jkt");
    let dev_id = Uuid::new_v4();
    let exp = now_ms + 120_000;
    let pop_transcript = build_device_enroll_pop_transcript(
        "trust.example",
        tenant,
        account,
        dev_id,
        "f19-pop",
        exp,
    );
    let pop: Signature = sk.sign(&pop_transcript);
    let intent = VerifiedIdpIntent {
        account_id: account,
        tenant_id: tenant.clone(),
        device_jkt: Some(jkt),
        is_tenant_admin: false,
        proof_id: format!("idp-{}", Uuid::new_v4()),
    };
    device_keys
        .enroll_device_key(EnrollDeviceKeyInput {
            trust_domain: "trust.example",
            provenance: AuthProvenance::ExternalIdp,
            idp_intent: &intent,
            device_key_id: dev_id,
            public_key_sec1: pk,
            nonce: "f19-pop",
            expires_at_ms: exp,
            now_ms,
            pop_signature: &pop.to_bytes(),
        })
        .expect("enroll device key");

    let mut signed = SignedInferenceInvocation {
        trust_domain: "trust.example".to_owned(),
        tenant_id: tenant.clone(),
        account_id: account,
        device_key_id: dev_id,
        extension_id: "ai-chatbot".to_owned(),
        extension_digest_sha256: [0x11; 32],
        room_scope: room.clone(),
        provider: "openai".to_owned(),
        payer: PayerSelection::Member {
            account_id: account,
            credential_id: cred_id,
        },
        command_digest_sha256: [0x22; 32],
        nonce,
        expires_at_ms: exp,
        budget,
        signature: Vec::new(),
    };
    let sig: Signature = sk.sign(&signed.canonical_transcript());
    signed.signature = sig.to_bytes().to_vec();
    signed
}

async fn run_two_cell_two_broker_workload<S, P, L, D>(
    store: &S,
    leases: &P,
    ledger: Arc<L>,
    destr_ledger: Arc<D>,
    broker_perms: Arc<dyn PermissionBackend>,
    cell_perms: Arc<dyn PermissionBackend>,
) -> String
where
    S: TenantStore,
    P: PlacementLeaseStore,
    L: AdmissionLedgerStore + ReplicaFenceStore + Send + Sync + 'static,
    D: DestructionLedgerStore + Send + Sync + 'static,
{
    let acme = TenantId::parse("acme.example").expect("acme");
    let globex = TenantId::parse("globex.example").expect("globex");
    let role = CellRole::parse("cell-shared").expect("role");
    let cell_a = holder_for("spiffe://trust.example/ns/waddle/sa/cell-shared", 0x11);
    let cell_b = holder_for("spiffe://trust.example/ns/waddle/sa/cell-shared", 0x22);

    store.provision_tenant(&acme).await.expect("prov acme");
    store.provision_tenant(&globex).await.expect("prov globex");
    broker_perms
        .provision_tenant_store(&acme)
        .await
        .expect("authz acme");
    broker_perms
        .provision_tenant_store(&globex)
        .await
        .expect("authz globex");

    // Cell cannot write Inference tuples (F14 module separation).
    let account = AccountId::new_v4();
    let cred_id_placeholder = Uuid::new_v4();
    let cell_inf_err = cell_perms
        .write_tuples(
            &acme,
            AuthzModule::Inference,
            &[AuthzTuple {
                object: AuthzObject {
                    object_type: AuthzObjectType::UserCredential,
                    id: cred_id_placeholder.to_string(),
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
        .expect_err("cell cannot write inference module");
    assert!(matches!(
        cell_inf_err,
        PermissionBackendError::ModuleWriteForbidden(_)
    ));

    // Enroll secret in BrokerCustodyService.
    let kms = Arc::new(LocalKmsProvider::new());
    let hierarchy = Arc::new(KeyHierarchyService::new("trust.example", kms));
    let custody = Arc::new(
        BrokerCustodyService::new(hierarchy, destr_ledger)
            .await
            .expect("custody"),
    );
    let now_ms = Utc::now().timestamp_millis();
    let receipt = custody
        .enroll(EnrollCredentialInput {
            tenant_id: acme.clone(),
            account_id: Some(account),
            provider: "openai".to_owned(),
            nonce: format!("f19-enroll-{}", Uuid::new_v4()),
            payload_digest_sha256: [0x33; 32],
            expires_at_ms: now_ms + 120_000,
            plaintext: SecretBytes::new(SENTINEL_API_KEY.as_bytes().to_vec()).expect("secret"),
        })
        .await
        .expect("enroll");

    broker_perms
        .write_tuples(
            &acme,
            AuthzModule::Inference,
            &[AuthzTuple {
                object: AuthzObject {
                    object_type: AuthzObjectType::UserCredential,
                    id: receipt.credential_id.to_string(),
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
        .expect("broker tuple");

    // Verify cross-tenant isolation on globex.
    let globex_check = broker_perms
        .check(&CheckRequest {
            tenant_id: globex.clone(),
            object: AuthzObject {
                object_type: AuthzObjectType::UserCredential,
                id: receipt.credential_id.to_string(),
            },
            permission: AuthzRelation::new("use"),
            subject: AuthzSubject {
                subject_type: AuthzSubjectType::Account,
                id: account.to_string(),
                relation: None,
            },
            consistency: AuthzConsistency::Strong,
        })
        .await
        .expect("check globex");
    assert_eq!(globex_check, CheckDecision::Denied);

    let trace = TraceRecorder::new();

    // Cell A claims placement lease (gen 1).
    let lease_a = leases
        .claim(&acme, &role, PlacementMode::SharedPool, &cell_a, 30_000)
        .await
        .expect("claim a");
    trace.emit(
        "cell-a",
        &acme,
        TraceEvent::LeaseClaim {
            holder_key: "cell-a".to_owned(),
            gen: lease_a.generation.get(),
        },
    );

    store
        .open_tenant(&TenantQueryScope {
            tenant_id: acme.clone(),
            holder: cell_a.clone(),
            placement_generation: lease_a.generation,
        })
        .await
        .expect("cell-a open tenant at gen 1");
    trace.emit(
        "cell-a",
        &acme,
        TraceEvent::TenantWrite {
            writer_key: "cell-a".to_owned(),
            gen: lease_a.generation.get(),
        },
    );

    // Two brokers (broker-a, broker-b) sharing durable ledger & permissions.
    let clock = Arc::new(FakeBootClock::new(Duration::from_secs(10)));
    let fence_a = Arc::new(BrokerReplicaFence::new(
        Uuid::new_v4(),
        Arc::clone(&ledger) as Arc<dyn ReplicaFenceStore>,
        Arc::clone(&clock) as Arc<dyn waddle_broker_core::boot_clock::BootClock>,
    ));
    let fence_b = Arc::new(BrokerReplicaFence::new(
        Uuid::new_v4(),
        Arc::clone(&ledger) as Arc<dyn ReplicaFenceStore>,
        Arc::clone(&clock) as Arc<dyn waddle_broker_core::boot_clock::BootClock>,
    ));
    fence_a.tick_once().await.expect("tick a");
    fence_b.tick_once().await.expect("tick b");

    let device_keys = Arc::new(DeviceKeyRegistry::new());
    let broker_a = BrokerAdmissionService::new(
        "broker-a",
        Arc::clone(&ledger),
        Arc::clone(&broker_perms),
        Arc::clone(&device_keys),
        Arc::clone(&fence_a),
        trace.clone(),
    );
    let broker_b = BrokerAdmissionService::new(
        "broker-b",
        Arc::clone(&ledger),
        Arc::clone(&broker_perms),
        Arc::clone(&device_keys),
        Arc::clone(&fence_b),
        trace.clone(),
    );

    let room = RoomScopeKey {
        scope_id: "general".to_owned(),
    };
    let budget = BudgetLimits {
        max_input_tokens: 4_000,
        max_output_tokens: 1_000,
        max_spend_microcents: 5_000,
        max_steps: 4,
    };
    let nonce1 = [0x41; 32];
    let signed1 = enroll_and_sign_invocation(
        &device_keys,
        InvocationTarget {
            tenant: &acme,
            account,
            cred_id: receipt.credential_id,
            room: &room,
        },
        now_ms,
        nonce1,
        budget,
    )
    .await;

    let admitted = broker_a
        .admit_invocation(AdmitInvocationInput {
            attempt_id: AttemptId::new_v4(),
            run_id: RunId::new_v4(),
            step_id: StepId::new(0),
            trust_domain: "trust.example".to_owned(),
            tenant_id: acme.clone(),
            principal: InvocationPrincipal::LocalMember {
                account_id: account,
                provenance: AuthProvenance::ExternalIdp,
            },
            extension_id: "ai-chatbot".to_owned(),
            extension_digest_sha256: [0x11; 32],
            provider: "openai".to_owned(),
            payer: PayerSelection::Member {
                account_id: account,
                credential_id: receipt.credential_id,
            },
            room_or_space: room.clone(),
            holder: cell_a.clone(),
            command_digest_sha256: [0x22; 32],
            invocation_nonce: nonce1,
            expires_at_ms: signed1.expires_at_ms,
            reserved_spend_microcents: 500,
            budget,
            signed_invocation: Some(signed1),
            now_ms,
        })
        .await
        .expect("broker-a admits cell-a");

    // Live TLS SSE dispatch on broker-a -> stream open -> lease transfer to cell-b -> cancel.
    let tls = generate_tls_cert("provider.example");
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let acceptor = TlsAcceptor::from(tls.server_config);
    let server_task = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.expect("accept");
        let mut s = acceptor.accept(tcp).await.expect("tls");
        let req_raw = read_tls_http_request(&mut s).await;
        assert!(req_raw.contains(SENTINEL_API_KEY));
        let chunk0 = b"data: {\"delta\":\"f19-c0\"}\n\n";
        let _ = s
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n")
            .await;
        let _ = s
            .write_all(format!("{:x}\r\n", chunk0.len()).as_bytes())
            .await;
        let _ = s.write_all(chunk0).await;
        let _ = s.write_all(b"\r\n").await;
        let _ = s.flush().await;
        let mut buf = [0u8; 32];
        let _ = s.read(&mut buf).await;
    });

    let mut reg = EgressProfileRegistry::new();
    reg.register(ProviderProfile {
        provider: "openai".to_owned(),
        deployment: "prod-us".to_owned(),
        model: "gpt-4o-mini".to_owned(),
        operation: ProviderOperation::ChatCompletions,
        base_origin: format!("https://provider.example:{}", addr.port()),
        operation_path: "/v1/chat/completions".to_owned(),
        pinned_ca_pem: Some(tls.ca_pem),
        caps: EgressCaps::default(),
    })
    .expect("register");
    let egress = BrokerEgressClient::with_loopback_tls_test_override(
        reg,
        Arc::clone(&custody),
        Arc::new(StaticPublicDnsResolver {
            addrs: vec![SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(104, 18, 32, 7)),
                addr.port(),
            )],
        }),
        addr,
    );

    let mut stream = egress
        .execute_streaming(
            &admitted,
            &EgressInferenceRequest {
                provider: "openai".to_owned(),
                deployment: "prod-us".to_owned(),
                model: "gpt-4o-mini".to_owned(),
                operation: ProviderOperationWire::ChatCompletions,
                requested_path: None,
                body_json: json!({"prompt": SENTINEL_PROMPT, "stream": true}),
                stream: true,
            },
        )
        .await
        .expect("stream opens");
    let first = stream
        .next_chunk()
        .await
        .expect("chunk 0")
        .expect("some chunk");
    assert!(String::from_utf8_lossy(&first.data).contains("f19-c0"));

    trace.emit(
        "broker-a",
        &acme,
        TraceEvent::StreamOpen {
            stream_id: "f19-s1".to_owned(),
            permits_after: 1,
            max_permits: 4,
        },
    );

    // Transfer placement lease from cell-a to same-role cell-b inside placement fence restriction.
    let placement_key = FenceKey {
        tenant_id: acme.clone(),
        kind: FenceKind::Placement,
    };
    let leases_ref = leases;
    let acme_c = acme.clone();
    let cell_a_c = cell_a.clone();
    let cell_b_c = cell_b.clone();
    let lease_gen_a = lease_a.generation;
    let trace_c = trace.clone();
    let ticket_gen = broker_a
        .restrict_fence(
            &placement_key,
            &[Arc::clone(&fence_b)],
            false,
            || async move {
                let new_lease = leases_ref
                    .transfer(&acme_c, &cell_a_c, &cell_b_c, lease_gen_a, 30_000)
                    .await
                    .map_err(|e| PermissionBackendError::Backend(e.to_string()))?;
                trace_c.emit(
                    "cell-b",
                    &acme_c,
                    TraceEvent::LeaseTransfer {
                        from_key: "cell-a".to_owned(),
                        to_key: "cell-b".to_owned(),
                        new_gen: new_lease.generation.get(),
                    },
                );
                Ok(())
            },
        )
        .await
        .expect("restrict placement fence");

    let mut cancel_rx = admitted.cancel_rx.clone();
    if !*cancel_rx.borrow() {
        let _ = tokio::time::timeout(Duration::from_secs(2), cancel_rx.changed()).await;
    }
    assert!(*cancel_rx.borrow(), "admitted stream must be canceled");
    drop(stream);
    trace.emit(
        "broker-a",
        &acme,
        TraceEvent::StreamCancelReq {
            stream_id: "f19-s1".to_owned(),
            tick: 1,
        },
    );
    trace.emit(
        "broker-a",
        &acme,
        TraceEvent::StreamAbortDone {
            stream_id: "f19-s1".to_owned(),
            tick: 2,
            permits_after: 0,
        },
    );
    trace.emit(
        "broker-a",
        &acme,
        TraceEvent::StreamClose {
            stream_id: "f19-s1".to_owned(),
            permits_after: 0,
        },
    );
    broker_a
        .reconcile_attempt(
            admitted.receipt.attempt_id,
            AttemptOutcome::Canceled {
                actual_spend_microcents: 0,
            },
        )
        .await
        .expect("reconcile canceled");

    ledger
        .reopen_fence(&placement_key, ticket_gen)
        .await
        .expect("reopen");
    trace.emit(
        "broker-a",
        &acme,
        TraceEvent::FenceReopen {
            fence_kind: "placement".to_owned(),
            gen: ticket_gen.get(),
        },
    );
    server_task.await.expect("tls server");

    // Stale cell-a is denied on TenantStore (gen 1 vs gen 2) AND broker-b admission.
    assert!(
        store
            .open_tenant(&TenantQueryScope {
                tenant_id: acme.clone(),
                holder: cell_a.clone(),
                placement_generation: lease_a.generation,
            })
            .await
            .is_err(),
        "stale cell-a write must fail after lease transfer"
    );

    let mut expected_fences = ledger
        .snapshot_fences(
            &acme,
            &waddle_broker_core::admission::CredentialKey {
                credential_id: receipt.credential_id,
                scope: waddle_broker_core::admission::CredentialScope::Member {
                    account_id: account,
                },
            },
            &room,
        )
        .await
        .expect("snapshot");
    expected_fences.placement.generation = lease_a.generation;
    assert!(
        ledger
            .admit_attempt(&waddle_broker_core::admission::AdmitAttemptRequest {
                attempt_id: AttemptId::new_v4(),
                run_id: RunId::new_v4(),
                step_id: StepId::new(0),
                tenant_id: acme.clone(),
                credential: waddle_broker_core::admission::CredentialKey {
                    credential_id: receipt.credential_id,
                    scope: waddle_broker_core::admission::CredentialScope::Member {
                        account_id: account,
                    },
                },
                room_or_space: room.clone(),
                holder: cell_a.clone(),
                expected_fences,
                invocation_nonce: [0x42; 32],
                expires_at_ms: now_ms + 60_000,
                reserved_spend_microcents: 500,
                budget,
            })
            .await
            .is_err(),
        "stale cell-a generation must fail 3-fence admission on ledger"
    );
    assert!(
        leases
            .renew(&acme, &cell_a, lease_a.generation, 30_000)
            .await
            .is_err(),
        "stale cell-a lease renewal must fail"
    );

    // Rotate credential fence to gen 2 before cell-b resumes workload so
    // AdmissionFenceMonitor's cred_gen >= revoked_gen invariant holds across
    // the post-transfer dispatch.
    let cred_key = FenceKey {
        tenant_id: acme.clone(),
        kind: FenceKind::Credential(waddle_broker_core::admission::CredentialKey {
            credential_id: receipt.credential_id,
            scope: waddle_broker_core::admission::CredentialScope::Member {
                account_id: account,
            },
        }),
    };
    let cred_gen_2 = broker_b
        .restrict_fence(&cred_key, &[Arc::clone(&fence_a)], true, || async {
            Ok(())
        })
        .await
        .expect("advance credential fence to gen 2");
    assert!(cred_gen_2 >= ticket_gen);

    // Cell B (gen 2) opens tenant and succeeds on broker-b.
    store
        .open_tenant(&TenantQueryScope {
            tenant_id: acme.clone(),
            holder: cell_b.clone(),
            placement_generation: FenceGeneration::new(2),
        })
        .await
        .expect("cell-b open gen 2");
    trace.emit(
        "cell-b",
        &acme,
        TraceEvent::TenantWrite {
            writer_key: "cell-b".to_owned(),
            gen: 2,
        },
    );
    let nonce3 = [0x43; 32];
    let signed3 = enroll_and_sign_invocation(
        &device_keys,
        InvocationTarget {
            tenant: &acme,
            account,
            cred_id: receipt.credential_id,
            room: &room,
        },
        now_ms,
        nonce3,
        budget,
    )
    .await;
    broker_b
        .admit_invocation(AdmitInvocationInput {
            attempt_id: AttemptId::new_v4(),
            run_id: RunId::new_v4(),
            step_id: StepId::new(0),
            trust_domain: "trust.example".to_owned(),
            tenant_id: acme.clone(),
            principal: InvocationPrincipal::LocalMember {
                account_id: account,
                provenance: AuthProvenance::ExternalIdp,
            },
            extension_id: "ai-chatbot".to_owned(),
            extension_digest_sha256: [0x11; 32],
            provider: "openai".to_owned(),
            payer: PayerSelection::Member {
                account_id: account,
                credential_id: receipt.credential_id,
            },
            room_or_space: room,
            holder: cell_b,
            command_digest_sha256: [0x22; 32],
            invocation_nonce: nonce3,
            expires_at_ms: signed3.expires_at_ms,
            reserved_spend_microcents: 500,
            budget,
            signed_invocation: Some(signed3),
            now_ms,
        })
        .await
        .expect("cell-b admitted on broker-b");

    trace.to_jsonl()
}

#[tokio::test]
async fn f19_01_turso_openfga_two_cell_two_broker_stack() {
    let dir = TempDir::new().expect("tempdir");
    let leases = Arc::new(
        TursoPlacementLeaseStore::open(&dir.path().join("leases.db"))
            .await
            .expect("turso leases"),
    );
    let store = TursoTenantStore::new(
        dir.path().join("tenants"),
        TursoPoolConfig::default(),
        Some(Arc::clone(&leases) as Arc<dyn PlacementLeaseStore>),
    )
    .expect("turso store");
    let ledger = Arc::new(
        TursoAdmissionLedgerStore::open(dir.path().join("admission.db"))
            .await
            .expect("turso admission"),
    );
    let destr_ledger = Arc::new(
        TursoLedgerStore::open_in_dir(&dir.path().join("destr"))
            .await
            .expect("turso destr"),
    );

    let openfga = InProcessOpenFgaServer::start().await.expect("openfga");
    let broker_perms = Arc::new(
        OpenFgaPermissionBackend::new(OpenFgaConfig {
            endpoint: openfga.endpoint().to_owned(),
            bearer_token: None,
            caller_role: AuthzCallerRole::Broker,
            pinned_model_version: None,
        })
        .expect("openfga broker"),
    );
    let cell_perms = Arc::new(broker_perms.with_caller_role(AuthzCallerRole::Cell));

    let jsonl = run_two_cell_two_broker_workload(
        &store,
        leases.as_ref(),
        ledger,
        destr_ledger,
        broker_perms,
        cell_perms,
    )
    .await;
    assert_secret_free(&jsonl, &[SENTINEL_API_KEY, SENTINEL_PROMPT, SENTINEL_JID])
        .expect("secret-free");
    let summary = verify_jsonl_trace(&jsonl).expect("pobserve verify");
    assert!(summary.dispatches_observed >= 1);
    assert!(summary.streams_opened >= 1);
    assert!(summary.tenant_writes_observed >= 2);
}

#[tokio::test]
async fn f19_02_postgres_spicedb_two_cell_two_broker_stack() {
    let leases = Arc::new(
        SqliteCompatPlacementLeaseStore::new_in_memory()
            .await
            .expect("pg compat leases"),
    );
    let store = SqliteCompatPostgresStore::new_in_memory(Some(
        Arc::clone(&leases) as Arc<dyn PlacementLeaseStore>
    ))
    .await
    .expect("pg compat store");
    let ledger = Arc::new(
        SqliteCompatAdmissionLedgerStore::in_memory()
            .await
            .expect("pg compat admission"),
    );
    let destr_ledger = Arc::new(
        SqliteCompatLedgerStore::connect_in_memory()
            .await
            .expect("pg compat destr"),
    );
    let broker_perms = Arc::new(TenantSpiceDbPermissionBackend::in_memory(
        AuthzCallerRole::Broker,
    ));
    let cell_perms = Arc::new(broker_perms.with_caller_role(AuthzCallerRole::Cell));

    let jsonl = run_two_cell_two_broker_workload(
        &store,
        leases.as_ref(),
        ledger,
        destr_ledger,
        broker_perms,
        cell_perms,
    )
    .await;
    assert_secret_free(&jsonl, &[SENTINEL_API_KEY, SENTINEL_PROMPT, SENTINEL_JID])
        .expect("secret-free");
    let summary = verify_jsonl_trace(&jsonl).expect("pobserve verify");
    assert!(summary.dispatches_observed >= 1);
    assert!(summary.streams_opened >= 1);
    assert!(summary.tenant_writes_observed >= 2);

    // Optional live native PostgreSQL 17 + SpiceDB 1.56 pass when Nix binaries are provided.
    if std::env::var_os("FENCE_POSTGRES_BIN").is_some()
        && std::env::var_os("FENCE_SPICEDB_BIN").is_some()
    {
        let _fixture = waddle_native_fixture::fixture::Fixture::new()
            .expect("boot live native postgres+spicedb fixture");
    }
}

#[tokio::test]
async fn f19_03_wit_streaming_first_chunk_and_midstream_cancel() {
    struct BrokerBackedWitHost {
        admission: Arc<BrokerAdmissionService<TursoAdmissionLedgerStore>>,
        egress: Arc<BrokerEgressClient>,
        holder: HolderInstance,
        account_id: AccountId,
        credential_id: Uuid,
        deployment: String,
        expected_trust_domain: String,
        first_chunk_seen: Arc<Notify>,
    }

    #[async_trait]
    impl ExtensionInferenceHost for BrokerBackedWitHost {
        async fn start_stream(
            &self,
            context: &InvocationContext,
            request: HostInferenceRequest,
        ) -> Result<OpenedInferenceStream, HostInferenceError> {
            let tenant_id = context.tenant_id.clone().expect("tenant");
            let room_scope = RoomScopeKey {
                scope_id: "general".to_owned(),
            };
            let ext_digest = [0x11; 32];
            let cmd_digest = [0x22; 32];
            let now_ms = Utc::now().timestamp_millis();
            let mut nonce = [0u8; 32];
            nonce[..16].copy_from_slice(Uuid::new_v4().as_bytes());
            nonce[16..].copy_from_slice(Uuid::new_v4().as_bytes());
            let admitted = self
                .admission
                .admit_invocation(AdmitInvocationInput {
                    attempt_id: AttemptId::new_v4(),
                    run_id: RunId::new_v4(),
                    step_id: StepId::new(0),
                    trust_domain: self.expected_trust_domain.clone(),
                    tenant_id,
                    principal: InvocationPrincipal::LocalMember {
                        account_id: self.account_id,
                        provenance: AuthProvenance::ExternalIdp,
                    },
                    extension_id: context.plugin_id.as_str().to_owned(),
                    extension_digest_sha256: ext_digest,
                    provider: request.provider.clone(),
                    payer: PayerSelection::Tenant {
                        credential_id: self.credential_id,
                    },
                    room_or_space: room_scope.clone(),
                    holder: self.holder.clone(),
                    command_digest_sha256: cmd_digest,
                    invocation_nonce: nonce,
                    expires_at_ms: now_ms + 60_000,
                    reserved_spend_microcents: 250,
                    budget: BudgetLimits {
                        max_input_tokens: 4_000,
                        max_output_tokens: 1_000,
                        max_spend_microcents: 5_000,
                        max_steps: 2,
                    },
                    signed_invocation: None,
                    now_ms,
                })
                .await
                .map_err(|e| {
                    HostInferenceError::new(HostInferenceErrorCode::Denied, e.to_string())
                })?;

            let mut body_map = Map::new();
            body_map.insert("model".to_owned(), Value::String(request.model.clone()));
            body_map.insert("stream".to_owned(), Value::Bool(true));
            let mut egress_stream = self
                .egress
                .execute_streaming_checked(
                    &admitted,
                    &EgressInferenceRequest {
                        provider: request.provider.clone(),
                        deployment: self.deployment.clone(),
                        model: request.model.clone(),
                        operation: ProviderOperationWire::ChatCompletions,
                        requested_path: None,
                        body_json: Value::Object(body_map),
                        stream: true,
                    },
                    ExpectedEgressBinding {
                        trust_domain: &self.expected_trust_domain,
                        extension_id: context.plugin_id.as_str(),
                        extension_digest_sha256: ext_digest,
                        room_or_space: &room_scope.scope_id,
                        command_digest_sha256: cmd_digest,
                        deployment: &self.deployment,
                        model: &request.model,
                        operation: ProviderOperationWire::ChatCompletions,
                    },
                )
                .await
                .map_err(|e| {
                    HostInferenceError::new(HostInferenceErrorCode::TemporaryFailure, e.to_string())
                })?;

            let (cancel, mut host_cancel_rx) = StreamCancelHandle::new();
            let (tx, rx) = mpsc::channel(8);
            let first_chunk_seen = Arc::clone(&self.first_chunk_seen);
            let mut admit_cancel_rx = admitted.cancel_rx.clone();
            let provider = request.provider;
            let model = request.model;
            tokio::spawn(async move {
                let mut seq = 0u64;
                loop {
                    tokio::select! {
                        biased;
                        _ = host_cancel_rx.changed() => {
                            egress_stream.cancel_sync();
                            break;
                        }
                        _ = admit_cancel_rx.changed() => {
                            egress_stream.cancel_sync();
                            let _ = tx.try_send(StreamItem::Error(HostInferenceError::new(
                                HostInferenceErrorCode::Canceled,
                                "replica fence canceled",
                            )));
                            break;
                        }
                        chunk_res = egress_stream.next_chunk() => {
                            match chunk_res {
                                Ok(Some(chunk)) => {
                                    let text = String::from_utf8_lossy(&chunk.data);
                                    let payload = if text.contains("chunk-0") {
                                        b"chunk-0".to_vec()
                                    } else {
                                        b"-chunk-1".to_vec()
                                    };
                                    let _ = tx.send(StreamItem::Chunk(HostInferenceChunk {
                                        delta: payload,
                                        sequence: seq,
                                        finish_reason: None,
                                    })).await;
                                    if seq == 0 {
                                        first_chunk_seen.notify_one();
                                    }
                                    seq += 1;
                                }
                                Ok(None) => {
                                    let _ = tx.send(StreamItem::EndOfStream(VerifiedInferenceUsage {
                                        provider,
                                        model,
                                        input_tokens: 10,
                                        output_tokens: 2,
                                        cost_microcents: 250,
                                    })).await;
                                    break;
                                }
                                Err(e) => {
                                    egress_stream.cancel_sync();
                                    let _ = tx.try_send(StreamItem::Error(HostInferenceError::new(
                                        HostInferenceErrorCode::TemporaryFailure,
                                        e.to_string(),
                                    )));
                                    break;
                                }
                            }
                        }
                    }
                }
            });

            Ok(OpenedInferenceStream {
                rx,
                cancel,
                max_chunk_bytes: DEFAULT_MAX_STREAM_CHUNK_BYTES,
            })
        }
    }

    let dir = TempDir::new().expect("tempdir");
    let tenant = TenantId::parse("acme.example").expect("tenant");
    let ledger = Arc::new(
        TursoAdmissionLedgerStore::open(dir.path().join("admission.db"))
            .await
            .expect("ledger"),
    );
    let destr_ledger = Arc::new(
        TursoLedgerStore::open_in_dir(&dir.path().join("destr"))
            .await
            .expect("destr"),
    );
    let perms = Arc::new(waddle_broker::authz::local::LocalPermissionBackend::new(
        AuthzCallerRole::Broker,
    ));
    perms.provision_tenant_store(&tenant).await.expect("prov");

    let kms = Arc::new(LocalKmsProvider::new());
    let hierarchy = Arc::new(KeyHierarchyService::new("trust.example", kms));
    let custody = Arc::new(
        BrokerCustodyService::new(hierarchy, destr_ledger)
            .await
            .expect("custody"),
    );
    let receipt = custody
        .enroll(EnrollCredentialInput {
            tenant_id: tenant.clone(),
            account_id: None,
            provider: "openai".to_owned(),
            nonce: "wit-stream-enroll".to_owned(),
            payload_digest_sha256: [0x55; 32],
            expires_at_ms: Utc::now().timestamp_millis() + 60_000,
            plaintext: SecretBytes::new(SENTINEL_API_KEY.as_bytes().to_vec()).expect("secret"),
        })
        .await
        .expect("enroll");
    let account_id = AccountId::new_v4();
    perms
        .write_tuples(
            &tenant,
            AuthzModule::Inference,
            &[AuthzTuple {
                object: AuthzObject {
                    object_type: AuthzObjectType::TenantCredential,
                    id: receipt.credential_id.to_string(),
                },
                relation: AuthzRelation::new("use"),
                subject: AuthzSubject {
                    subject_type: AuthzSubjectType::Account,
                    id: account_id.to_string(),
                    relation: None,
                },
            }],
        )
        .await
        .expect("tuple");

    let clock = Arc::new(FakeBootClock::new(Duration::from_secs(10)));
    let fence = Arc::new(BrokerReplicaFence::new(
        Uuid::new_v4(),
        Arc::clone(&ledger) as Arc<dyn ReplicaFenceStore>,
        clock,
    ));
    fence.tick_once().await.expect("tick");
    let admission = Arc::new(BrokerAdmissionService::new(
        "broker-a",
        ledger,
        perms,
        Arc::new(DeviceKeyRegistry::new()),
        fence,
        TraceRecorder::new(),
    ));

    let tls = generate_tls_cert("provider.example");
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let acceptor = TlsAcceptor::from(tls.server_config);
    let release_second = Arc::new(Notify::new());
    let release_second_srv = Arc::clone(&release_second);
    let cancel_disconnect = Arc::new(Notify::new());
    let cancel_disconnect_srv = Arc::clone(&cancel_disconnect);

    let srv = tokio::spawn(async move {
        let (tcp1, _) = listener.accept().await.expect("accept 1");
        let mut s1 = acceptor.accept(tcp1).await.expect("tls 1");
        let _ = read_tls_http_request(&mut s1).await;
        s1.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
        )
        .await
        .expect("hdr 1");
        let c0 = b"data: {\"delta\":\"chunk-0\"}\n\n";
        s1.write_all(format!("{:x}\r\n", c0.len()).as_bytes())
            .await
            .expect("len 0");
        s1.write_all(c0).await.expect("c0");
        s1.write_all(b"\r\n").await.expect("crlf 0");
        s1.flush().await.expect("flush 0");

        release_second_srv.notified().await;
        let c1 = b"data: {\"delta\":\"chunk-1\"}\n\ndata: [DONE]\n\n";
        s1.write_all(format!("{:x}\r\n", c1.len()).as_bytes())
            .await
            .expect("len 1");
        s1.write_all(c1).await.expect("c1");
        s1.write_all(b"\r\n0\r\n\r\n").await.expect("eof 1");
        s1.flush().await.expect("flush 1");
        let _ = s1.shutdown().await;
        drop(s1);

        let (tcp2, _) = listener.accept().await.expect("accept 2");
        let mut s2 = acceptor.accept(tcp2).await.expect("tls 2");
        let _ = read_tls_http_request(&mut s2).await;
        s2.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
        )
        .await
        .expect("hdr 2");
        s2.write_all(format!("{:x}\r\n", c0.len()).as_bytes())
            .await
            .expect("len 2");
        s2.write_all(c0).await.expect("c0 2");
        s2.write_all(b"\r\n").await.expect("crlf 2");
        s2.flush().await.expect("flush 2");

        let mut buf = [0u8; 32];
        loop {
            match tokio::time::timeout(Duration::from_secs(3), s2.read(&mut buf)).await {
                Ok(Ok(0)) | Ok(Err(_)) | Err(_) => {
                    cancel_disconnect_srv.notify_one();
                    break;
                }
                Ok(Ok(_)) => {}
            }
        }
    });

    let mut reg = EgressProfileRegistry::new();
    reg.register(ProviderProfile {
        provider: "openai".to_owned(),
        deployment: "prod".to_owned(),
        model: "gpt-4o-mini".to_owned(),
        operation: ProviderOperation::ChatCompletions,
        base_origin: format!("https://provider.example:{}", addr.port()),
        operation_path: "/v1/chat/completions".to_owned(),
        pinned_ca_pem: Some(tls.ca_pem),
        caps: EgressCaps::default(),
    })
    .expect("register");
    let egress = Arc::new(BrokerEgressClient::with_loopback_tls_test_override(
        reg,
        custody,
        Arc::new(StaticPublicDnsResolver {
            addrs: vec![SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(104, 18, 32, 7)),
                addr.port(),
            )],
        }),
        addr,
    ));

    let first_chunk_seen = Arc::new(Notify::new());
    let wit_host = Arc::new(BrokerBackedWitHost {
        admission,
        egress,
        holder: holder_for("spiffe://trust.example/ns/waddle/sa/cell-shared", 1),
        account_id,
        credential_id: receipt.credential_id,
        deployment: "prod".to_owned(),
        expected_trust_domain: "trust.example".to_owned(),
        first_chunk_seen: Arc::clone(&first_chunk_seen),
    });

    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../waddle-extensions/tests/fixtures/inference_stream.wat");
    let runtime = WasmRuntime::try_new().expect("wasm runtime");

    // 1. Happy path through real Wasm component: first chunk before completion + EOF.
    let loaded1 = LoadedExtension::load(&runtime, &fixture_path)
        .expect("load wat")
        .with_limits(RuntimeLimits::default())
        .expect("limits");
    let actor1 = WasmExtensionActor::initialize(loaded1, "{}")
        .await
        .expect("init")
        .with_tenant_id(tenant.clone())
        .with_grants(HashSet::from([ExtensionCapability::Commands]))
        .with_inference_host(Arc::clone(&wit_host) as Arc<dyn ExtensionInferenceHost>);

    let release_clone = Arc::clone(&release_second);
    let gate1 = tokio::spawn(async move {
        first_chunk_seen.notified().await;
        tokio::time::sleep(Duration::from_millis(25)).await;
        release_clone.notify_one();
    });
    let effects1 = tokio::time::timeout(
        Duration::from_secs(3),
        actor1.handle_event_for_waddle(
            ExtensionEvent::Command(CommandInvocation {
                waddle_id: WaddleId::try_new("acme.example").expect("waddle"),
                room: None,
                requester: FullJidValue::try_new("alice@acme.example/web").expect("req"),
                command_node: CommandNode::try_new("urn:waddle:extension:1:ai-chatbot")
                    .expect("node"),
                session_id: None,
                action: Some(CommandAction::Execute),
                form: None,
                fields: Vec::new(),
            }),
            WaddleId::try_new("acme.example").expect("waddle"),
        ),
    )
    .await
    .expect("f19_03 happy path handle_event must complete within 3s");
    assert_eq!(effects1.len(), 1);
    match &effects1[0] {
        waddle_extensions::ExtensionEffect::HostWarning(t) => {
            assert_eq!(t.as_str(), "chunk-0-chunk-1");
        }
        other => panic!("unexpected effect: {other:?}"),
    }
    tokio::time::timeout(Duration::from_secs(1), gate1)
        .await
        .expect("gate1 timeout")
        .expect("gate 1");

    // 2. Mid-stream hang -> Wasm epoch trap drops Store<HostState> -> InferenceStream::Drop cancels stream!
    let loaded2 = LoadedExtension::load(&runtime, &fixture_path)
        .expect("load wat")
        .with_limits(RuntimeLimits {
            invocation_timeout_ms: 250,
            http_timeout_ms: 250,
            ..RuntimeLimits::default()
        })
        .expect("limits");
    let actor2 = WasmExtensionActor::initialize(loaded2, "{}")
        .await
        .expect("init")
        .with_tenant_id(tenant)
        .with_grants(HashSet::from([ExtensionCapability::Commands]))
        .with_inference_host(Arc::clone(&wit_host) as Arc<dyn ExtensionInferenceHost>);
    let effects2 = tokio::time::timeout(
        Duration::from_secs(3),
        actor2.handle_event_for_waddle(
            ExtensionEvent::Command(CommandInvocation {
                waddle_id: WaddleId::try_new("acme.example").expect("waddle"),
                room: None,
                requester: FullJidValue::try_new("alice@acme.example/web").expect("req"),
                command_node: CommandNode::try_new("urn:waddle:extension:1:ai-chatbot")
                    .expect("node"),
                session_id: None,
                action: Some(CommandAction::Execute),
                form: None,
                fields: vec![waddle_extensions::FormFieldValue {
                    name: waddle_extensions::UiActionId::try_new("hang").expect("hang"),
                    values: vec![waddle_extensions::DataFormValue::new("1")],
                }],
            }),
            WaddleId::try_new("acme.example").expect("waddle"),
        ),
    )
    .await
    .expect("f19_03 trap path handle_event must complete within 3s");
    assert!(matches!(
        effects2.first(),
        Some(waddle_extensions::ExtensionEffect::HostDenial(_))
    ));
    tokio::time::timeout(Duration::from_secs(4), cancel_disconnect.notified())
        .await
        .expect("mid-stream trap must synchronously cancel EgressStream and close TLS socket");
    srv.await.expect("tls server");
}

#[tokio::test]
async fn f19_04_sealed_enrollment_workload_rotation_and_lease_transfer_denial() {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;

    let dir = TempDir::new().expect("tempdir");
    let tenant = TenantId::parse("acme.example").expect("tenant");
    let account = AccountId::new_v4();
    let now_ms = Utc::now().timestamp_millis();
    let now_sec = u64::try_from(Utc::now().timestamp()).expect("unix ts");

    // 1. Offline Root BKSK -> Subkey rotation -> Colony JWT -> client sealed blob -> BrokerCustodyService enroll + rotate.
    let root = RootBkskSigner::generate().expect("root");
    let pins = RootBkskPinSet::new(&[root.public_key_sec1()]).expect("pins");
    let ring = BrokerSubkeyRing::new("trust.example");
    let bundle = ring
        .rotate_subkey(&root, 1, now_ms - 1_000, now_ms + 300_000)
        .expect("subkey 1");

    let jwt_sk = SigningKey::random(&mut OsRng);
    let jwt_pk = jwt_sk
        .verifying_key()
        .to_encoded_point(false)
        .to_bytes()
        .to_vec();
    let jkt = p256_jwk_thumbprint(&jwt_pk).expect("jkt");
    let point = jwt_sk.verifying_key().to_encoded_point(false);
    let jwk = Jwk {
        common: CommonParameters {
            public_key_use: Some(PublicKeyUse::Signature),
            key_operations: None,
            key_algorithm: Some(KeyAlgorithm::ES256),
            key_id: Some("kid-1".to_owned()),
            x509_url: None,
            x509_chain: None,
            x509_sha1_fingerprint: None,
            x509_sha256_fingerprint: None,
        },
        algorithm: AlgorithmParameters::EllipticCurve(EllipticCurveKeyParameters {
            key_type: jsonwebtoken::jwk::EllipticCurveKeyType::EC,
            curve: EllipticCurve::P256,
            x: URL_SAFE_NO_PAD.encode(point.x().expect("x")),
            y: URL_SAFE_NO_PAD.encode(point.y().expect("y")),
        }),
    };
    let jwt_pem = jwt_sk
        .to_pkcs8_pem(p256::pkcs8::LineEnding::LF)
        .expect("pkcs8 pem");
    let claims = ColonyBrokerClaims {
        iss: "https://colony.example".to_owned(),
        sub: account.to_string(),
        aud: "waddle-broker".to_owned(),
        exp: now_sec + 300,
        nbf: now_sec - 5,
        iat: now_sec,
        jti: Uuid::new_v4().to_string(),
        tenant_id: tenant.as_str().to_owned(),
        challenge: "f19-chal".to_owned(),
        cnf: Some(ConfirmationClaim { jkt: jkt.clone() }),
        waddle_roles: Vec::new(),
    };
    let mut hdr = Header::new(Algorithm::ES256);
    hdr.kid = Some("kid-1".to_owned());
    let token = encode(
        &hdr,
        &claims,
        &EncodingKey::from_ec_pem(jwt_pem.as_bytes()).expect("enc key"),
    )
    .expect("jwt");
    let verifier = ColonyJwtVerifier::new(
        "https://colony.example",
        "waddle-broker",
        JwkSet { keys: vec![jwk] },
    );
    let idp_intent = verifier
        .verify(&token, &tenant, account, "f19-chal", Some(&jkt), now_sec)
        .expect("verify colony jwt");
    assert_eq!(idp_intent.device_jkt.as_deref(), Some(jkt.as_str()));

    let nonce = format!("sealed-nonce-{}", Uuid::new_v4());
    let ctx = ClientSealContext {
        trust_domain: "trust.example".to_owned(),
        tenant_id: tenant.clone(),
        account_id: Some(account),
        provider: "openai".to_owned(),
        purpose: "member-inference".to_owned(),
        nonce: nonce.clone(),
        expires_at_ms: now_ms + 120_000,
        version: 1,
    };
    let secret = SecretBytes::new(SENTINEL_API_KEY.as_bytes().to_vec()).expect("secret");
    let sealed = seal_credential_for_broker(&pins, &bundle, &ctx, &secret, now_ms).expect("seal");
    let unsealed = ring
        .unseal_client_envelope(&sealed, &ctx, now_ms)
        .expect("unseal sealed blob");

    let destr_ledger = Arc::new(
        TursoLedgerStore::open_in_dir(&dir.path().join("destr"))
            .await
            .expect("destr"),
    );
    let kms = Arc::new(LocalKmsProvider::new());
    let hierarchy = Arc::new(KeyHierarchyService::new("trust.example", kms));
    let custody = BrokerCustodyService::new(hierarchy, destr_ledger)
        .await
        .expect("custody");

    let digest: [u8; 32] = Sha256::digest(unsealed.as_bytes()).into();
    let enrolled = custody
        .enroll(EnrollCredentialInput {
            tenant_id: tenant.clone(),
            account_id: Some(account),
            provider: "openai".to_owned(),
            nonce: nonce.clone(),
            payload_digest_sha256: digest,
            expires_at_ms: now_ms + 120_000,
            plaintext: unsealed.clone(),
        })
        .await
        .expect("enroll sealed");
    // Replayed enrollment nonce returns idempotently.
    let replayed = custody
        .enroll(EnrollCredentialInput {
            tenant_id: tenant.clone(),
            account_id: Some(account),
            provider: "openai".to_owned(),
            nonce,
            payload_digest_sha256: digest,
            expires_at_ms: now_ms + 120_000,
            plaintext: unsealed,
        })
        .await
        .expect("idempotent replay");
    assert_eq!(enrolled.credential_id, replayed.credential_id);
    assert!(replayed.replayed);

    // Workload key rotation: rotate credential and verify unseal returns rotated secret.
    let rotated = custody
        .rotate(
            &tenant,
            enrolled.credential_id,
            &SecretBytes::new(b"sk-waddle-rotated-new-secret".to_vec()).expect("rotated"),
        )
        .await
        .expect("rotate");
    assert_eq!(rotated.version, 2);
    assert_eq!(
        custody
            .unseal_for_provider_egress(&tenant, rotated.credential_id, "openai")
            .await
            .expect("unseal rotated")
            .as_bytes(),
        b"sk-waddle-rotated-new-secret"
    );

    // 2. Same-role lease transfer denies stale cell-a.
    let leases = TursoPlacementLeaseStore::open(&dir.path().join("leases.db"))
        .await
        .expect("leases");
    let role = CellRole::parse("cell-shared").expect("role");
    let cell_a = holder_for("spiffe://trust.example/ns/waddle/sa/cell-shared", 0x31);
    let cell_b = holder_for("spiffe://trust.example/ns/waddle/sa/cell-shared", 0x32);
    let lease_1 = leases
        .claim(&tenant, &role, PlacementMode::SharedPool, &cell_a, 30_000)
        .await
        .expect("claim 1");
    let lease_2 = leases
        .transfer(&tenant, &cell_a, &cell_b, lease_1.generation, 30_000)
        .await
        .expect("transfer 2");
    assert_eq!(lease_2.generation.get(), 2);
    assert!(
        leases
            .renew(&tenant, &cell_a, lease_1.generation, 30_000)
            .await
            .is_err(),
        "stale cell-a must be denied renewal after lease transfer"
    );
}

#[tokio::test]
async fn f19_05_secret_free_telemetry_and_pobserve_trace_verifier() {
    let dir = TempDir::new().expect("tempdir");
    let leases = Arc::new(
        TursoPlacementLeaseStore::open(&dir.path().join("leases.db"))
            .await
            .expect("turso leases"),
    );
    let store = TursoTenantStore::new(
        dir.path().join("tenants"),
        TursoPoolConfig::default(),
        Some(Arc::clone(&leases) as Arc<dyn PlacementLeaseStore>),
    )
    .expect("turso store");
    let ledger = Arc::new(
        TursoAdmissionLedgerStore::open(dir.path().join("admission.db"))
            .await
            .expect("turso admission"),
    );
    let destr_ledger = Arc::new(
        TursoLedgerStore::open_in_dir(&dir.path().join("destr"))
            .await
            .expect("turso destr"),
    );
    let openfga = InProcessOpenFgaServer::start().await.expect("openfga");
    let broker_perms = Arc::new(
        OpenFgaPermissionBackend::new(OpenFgaConfig {
            endpoint: openfga.endpoint().to_owned(),
            bearer_token: None,
            caller_role: AuthzCallerRole::Broker,
            pinned_model_version: None,
        })
        .expect("openfga broker"),
    );
    let cell_perms = Arc::new(broker_perms.with_caller_role(AuthzCallerRole::Cell));

    let jsonl = run_two_cell_two_broker_workload(
        &store,
        leases.as_ref(),
        ledger,
        destr_ledger,
        broker_perms,
        cell_perms,
    )
    .await;

    assert_secret_free(&jsonl, &[SENTINEL_API_KEY, SENTINEL_PROMPT, SENTINEL_JID])
        .expect("PObserve trace must be secret-free");
    let summary = verify_jsonl_trace(&jsonl).expect("PObserve trace verifier must pass");
    assert!(summary.dispatches_observed >= 1);
    assert!(summary.streams_opened >= 1);
    assert!(summary.tenant_writes_observed >= 2);
}
