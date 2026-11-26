//! T4-S07: Release Evidence Gate 3/4 (M6) — Member-funded `/ai` full E2E
//! lifecycle: output-mode enforcement, device-signed P-256 member invocation,
//! post-admission pre-egress room disclosure, loopback TLS streaming inference,
//! and ledger usage reconciliation.

#[path = "broker_test_support/admission_db.rs"]
mod admission_db;
#[path = "broker_test_support/broker_host.rs"]
mod broker_host;
#[path = "broker_test_support/tls_provider.rs"]
mod tls_provider;

use std::sync::Arc;
use std::time::Duration;

use admission_db::load_attempt_ids;
use async_trait::async_trait;
use broker_host::{build_broker_test_env, default_budget, BrokerTestConfig};
use chrono::Utc;
use jid::BareJid;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::elliptic_curve::rand_core::OsRng;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tls_provider::{read_http_request, LoopbackTlsServer};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use uuid::Uuid;
use waddle_broker::admission::invocation::{PayerSelection, SignedInferenceInvocation};
use waddle_broker::admission::service::TraceRecorder;
use waddle_broker::admission::{AdmissionQueryStore, AttemptStatus};
use waddle_broker::idp_proof::{
    build_device_enroll_pop_transcript, p256_jwk_thumbprint, AuthProvenance, DeviceKeyRegistry,
    EnrollDeviceKeyInput, VerifiedIdpIntent,
};
use waddle_broker_core::admission::{BudgetLimits, RoomScopeKey};
use waddle_broker_core::identity::{AccountId, TenantId};
use waddle_broker_core::permission::{AuthzObjectType, AuthzSubject, AuthzSubjectType};
use waddle_extensions::host_tools::{InvocationContext, InvocationKind};
use waddle_extensions::runtime::{
    ExtensionInferenceHost, HostInferenceError, HostInferenceMessage, HostInferenceRequest,
    InferenceCredentialRef, StreamItem,
};
use waddle_extensions::{
    DataFormValue, FormFieldValue, RoomJid as ExtensionRoomJid, UiActionId, WaddleId,
};
use waddle_server::server::extension_inference_host::{
    BrokerCredentialResolver, RoomDisclosureEmitter, RoomDisclosureEvent,
};
use waddle_server::server::validate_room_ai_output_mode;

struct MemberSignedResolver {
    trust_domain: String,
    account_id: AccountId,
    credential_id: Uuid,
    device_key_id: Uuid,
    signing_key: SigningKey,
    budget: BudgetLimits,
}

#[async_trait]
impl BrokerCredentialResolver for MemberSignedResolver {
    async fn resolve_requester_account(
        &self,
        _tenant_id: &TenantId,
        _requester: &BareJid,
    ) -> Result<(AccountId, AuthProvenance), HostInferenceError> {
        Ok((self.account_id, AuthProvenance::ExternalIdp))
    }

    async fn resolve_tenant_credential(
        &self,
        _tenant_id: &TenantId,
        _provider: &str,
        _credential_name: &str,
    ) -> Result<Uuid, HostInferenceError> {
        unreachable!("member-funded lifecycle selects InvokingUser, not Tenant")
    }

    async fn resolve_member_invocation(
        &self,
        tenant_id: &TenantId,
        context: &InvocationContext,
        provider: &str,
    ) -> Result<(AccountId, Uuid, SignedInferenceInvocation), HostInferenceError> {
        let mut ext_digest = [0u8; 32];
        ext_digest.copy_from_slice(&Sha256::digest(context.plugin_id.as_str().as_bytes()));
        let mut cmd_digest = [0u8; 32];
        cmd_digest.copy_from_slice(&Sha256::digest(
            format!(
                "{}:{}:default:openrouter/free",
                context.plugin_id.as_str(),
                provider
            )
            .as_bytes(),
        ));
        let room_scope = RoomScopeKey {
            scope_id: context
                .source_room
                .as_ref()
                .map(|r| r.as_str().to_owned())
                .unwrap_or_else(|| format!("waddle:{}", context.waddle_id.as_str())),
        };
        let mut nonce = [0u8; 32];
        nonce[..16].copy_from_slice(Uuid::new_v4().as_bytes());
        nonce[16..].copy_from_slice(Uuid::new_v4().as_bytes());

        let mut signed = SignedInferenceInvocation {
            trust_domain: self.trust_domain.clone(),
            tenant_id: tenant_id.clone(),
            account_id: self.account_id,
            device_key_id: self.device_key_id,
            extension_id: context.plugin_id.as_str().to_owned(),
            extension_digest_sha256: ext_digest,
            room_scope,
            provider: provider.to_owned(),
            payer: PayerSelection::Member {
                account_id: self.account_id,
                credential_id: self.credential_id,
            },
            command_digest_sha256: cmd_digest,
            nonce,
            expires_at_ms: Utc::now().timestamp_millis() + 60_000,
            budget: self.budget,
            signature: Vec::new(),
        };
        let sig: Signature = self.signing_key.sign(&signed.canonical_transcript());
        signed.signature = sig.to_bytes().to_vec();
        Ok((self.account_id, self.credential_id, signed))
    }
}

struct Gate4DisclosureEmitter {
    timeline: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl RoomDisclosureEmitter for Gate4DisclosureEmitter {
    async fn emit_room_disclosure(
        &self,
        _event: &RoomDisclosureEvent,
    ) -> Result<(), HostInferenceError> {
        self.timeline.lock().await.push("disclosure".to_owned());
        Ok(())
    }
}

#[tokio::test]
async fn gate4_member_ai_chatbot_full_e2e_lifecycle() {
    let ext_room = ExtensionRoomJid::try_new("general@muc.acme.example").expect("room");
    let field = |name: &str, value: &str| FormFieldValue {
        name: UiActionId::try_new(name).expect("name"),
        values: vec![DataFormValue::new(value)],
    };
    assert!(validate_room_ai_output_mode(
        "urn:waddle:extension:1:ai-chatbot",
        Some(&ext_room),
        &[field("prompt", "hello"), field("output", "private")],
    )
    .is_err());
    validate_room_ai_output_mode(
        "urn:waddle:extension:1:ai-chatbot",
        Some(&ext_room),
        &[field("prompt", "hello"), field("output", "channel")],
    )
    .expect("channel output allowed");

    let tls = LoopbackTlsServer::bind("openrouter.acme.test").await;
    let listener = tls.listener;
    let acceptor = tls.acceptor;
    let timeline = Arc::new(Mutex::new(Vec::new()));
    let timeline_srv = Arc::clone(&timeline);

    let srv = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.expect("accept");
        let mut tls_stream = acceptor.accept(tcp).await.expect("tls accept");
        let req = read_http_request(&mut tls_stream).await;
        assert!(req.contains("sk-member-byok-secret"));
        timeline_srv.lock().await.push("egress".to_owned());
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Member answer\"}}]}\n\n",
            "data: {\"usage\":{\"prompt_tokens\":21,\"completion_tokens\":9}}\n\n",
            "data: [DONE]\n\n"
        );
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        tls_stream.write_all(resp.as_bytes()).await.expect("write");
        tls_stream.shutdown().await.expect("shutdown");
    });

    let dir = TempDir::new().expect("tempdir");
    let tenant_id = TenantId::parse("acme.example").expect("tenant");
    let account_id = AccountId::new_v4();

    let device_keys = Arc::new(DeviceKeyRegistry::new());
    let sk = SigningKey::random(&mut OsRng);
    let pk = sk
        .verifying_key()
        .to_encoded_point(false)
        .as_bytes()
        .to_vec();
    let jkt = p256_jwk_thumbprint(&pk).expect("jkt");
    let dev_id = Uuid::new_v4();
    let now = Utc::now().timestamp_millis();
    let exp = now + 60_000;
    let transcript = build_device_enroll_pop_transcript(
        "acme.example",
        &tenant_id,
        account_id,
        dev_id,
        "pop-1",
        exp,
    );
    let sig: Signature = sk.sign(&transcript);
    device_keys
        .enroll_device_key(EnrollDeviceKeyInput {
            trust_domain: "acme.example",
            provenance: AuthProvenance::ExternalIdp,
            idp_intent: &VerifiedIdpIntent {
                account_id,
                tenant_id,
                device_jkt: Some(jkt),
                is_tenant_admin: false,
                proof_id: "idp-member".to_owned(),
            },
            device_key_id: dev_id,
            public_key_sec1: pk,
            nonce: "pop-1",
            expires_at_ms: exp,
            now_ms: now,
            pop_signature: &sig.to_bytes(),
        })
        .expect("enroll device key");

    let (env, ledger, _fence) = build_broker_test_env(BrokerTestConfig {
        data_dir: dir.path(),
        trust_domain: "acme.example",
        broker_id: "broker-gate4-member",
        spiffe_id: "spiffe://acme.example/ns/cells/sa/shared-pool",
        channel_binding: [0x12u8; 32],
        provider: "openrouter",
        deployment: "default",
        model: "openrouter/free",
        base_origin: "https://openrouter.acme.test",
        operation_path: "/api/v1/chat/completions",
        plugin_id: "ai-chatbot",
        credential_secret: b"sk-member-byok-secret",
        credential_account_id: Some(account_id),
        permission_grant: Some((
            AuthzObjectType::UserCredential,
            AuthzSubject {
                subject_type: AuthzSubjectType::Account,
                id: account_id.to_string(),
                relation: None,
            },
        )),
        device_keys,
        trace: TraceRecorder::new(),
        server_addr: tls.addr,
        ca_pem: tls.ca_pem,
    })
    .await;

    let host = env
        .build_host(
            Arc::new(MemberSignedResolver {
                trust_domain: "acme.example".to_owned(),
                account_id,
                credential_id: env.credential_id,
                device_key_id: dev_id,
                signing_key: sk,
                budget: default_budget(),
            }),
            120,
        )
        .with_disclosure_emitter(Arc::new(Gate4DisclosureEmitter {
            timeline: Arc::clone(&timeline),
        }));

    let mut opened = host
        .start_stream(
            &InvocationContext {
                tenant_id: Some(env.tenant_id),
                waddle_id: WaddleId::try_new("acme.example").expect("waddle"),
                plugin_id: env.plugin_id,
                requester: Some("alice@acme.example".parse().expect("alice")),
                source_room: Some("general@muc.acme.example".parse().expect("room")),
                kind: InvocationKind::Command,
                provider_room_grants: Vec::new(),
            },
            HostInferenceRequest {
                credential: InferenceCredentialRef::InvokingUser("openrouter".to_owned()),
                provider: "openrouter".to_owned(),
                deployment: "default".to_owned(),
                model: "openrouter/free".to_owned(),
                messages: vec![HostInferenceMessage {
                    role: "user".to_owned(),
                    content: "summarize release".to_owned(),
                }],
                max_output_tokens: Some(128),
            },
        )
        .await
        .expect("start_stream");

    let mut usage = None;
    let mut chunks = String::new();
    while let Some(item) = tokio::time::timeout(Duration::from_secs(5), opened.rx.recv())
        .await
        .expect("stream timeout")
    {
        match item {
            StreamItem::Chunk(c) => chunks.push_str(&String::from_utf8_lossy(&c.delta)),
            StreamItem::EndOfStream(u) => {
                usage = Some(u);
                break;
            }
            StreamItem::Error(err) => panic!("unexpected stream error: {err:?}"),
        }
    }
    srv.await.expect("server");
    assert!(chunks.contains("Member answer"));
    let u = usage.expect("usage");
    assert_eq!(u.input_tokens, 21);
    assert_eq!(u.output_tokens, 9);
    assert_eq!(timeline.lock().await.as_slice(), &["disclosure", "egress"]);

    let attempt_id = load_attempt_ids(dir.path()).await[0];
    let rec = ledger
        .get_attempt(attempt_id)
        .await
        .expect("get attempt")
        .expect("attempt row");
    assert_eq!(rec.status, AttemptStatus::Completed);
    assert_eq!(rec.input_tokens, 21);
    assert_eq!(rec.output_tokens, 9);
}
