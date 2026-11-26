//! T4-S06: Release Evidence Gate 3 — Complete 8-Case Authority & Boundary
//! Denial Matrix + Positive Control.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use serde_json::json;
use tempfile::TempDir;
use uuid::Uuid;
use waddle_broker::admission::invocation::{
    validate_principal_and_payer, InvocationError, InvocationPrincipal, PayerSelection,
};
use waddle_broker::admission::replica_fence::BrokerReplicaFence;
use waddle_broker::admission::service::{
    AdmissionServiceError, AdmitInvocationInput, BrokerAdmissionService, TraceRecorder,
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
    EgressCaps, EgressProfileRegistry, ProfileError, ProviderOperation, ProviderProfile,
};
use waddle_broker::egress::ssrf::{DnsResolver, SsrfError};
use waddle_broker::error::AuthError;
use waddle_broker::idp_proof::{
    authorize_admin_policy_mutation, enforce_not_scram, AuthProvenance, CellAssertedAdminFlags,
    DeviceKeyRegistry,
};
use waddle_broker::key_hierarchy::KeyHierarchyService;
use waddle_broker::kms::local::LocalKmsProvider;
use waddle_broker::ledger::turso::TursoLedgerStore;
use waddle_broker::secret::SecretBytes;
use waddle_broker_core::admission::{AttemptId, BudgetLimits, RoomScopeKey, RunId, StepId};
use waddle_broker_core::boot_clock::FakeBootClock;
use waddle_broker_core::identity::{
    AccountId, HolderInstance, IncarnationPubKey, SpiffeId, TenantId, TlsExporterProof,
};
use waddle_broker_core::permission::{
    AuthzModule, AuthzObject, AuthzObjectType, AuthzRelation, AuthzSubject, AuthzSubjectType,
    AuthzTuple, PermissionBackend, PermissionBackendError,
};

struct FixedResolver(Vec<SocketAddr>);

#[async_trait]
impl DnsResolver for FixedResolver {
    async fn resolve(&self, _host: &str, _port: u16) -> Result<Vec<SocketAddr>, SsrfError> {
        Ok(self.0.clone())
    }
}

#[tokio::test]
async fn e2e_gate3_authority_boundary_denial_matrix_and_positive_control() {
    let dir = TempDir::new().expect("tempdir");
    let tenant = TenantId::parse("acme.example").expect("tenant");
    let alice = AccountId::new_v4();
    let bob = AccountId::new_v4();
    let alice_cred = Uuid::new_v4();
    let tenant_cred = Uuid::new_v4();

    // Case 1: NativeScram account cannot use member-funded credentials or enroll.
    assert!(matches!(
        enforce_not_scram(AuthProvenance::NativeScram),
        Err(AuthError::ScramAccountIneligible)
    ));
    assert!(matches!(
        validate_principal_and_payer(
            &InvocationPrincipal::LocalMember {
                account_id: alice,
                provenance: AuthProvenance::NativeScram,
            },
            PayerSelection::Member {
                account_id: alice,
                credential_id: alice_cred,
            }
        ),
        Err(InvocationError::Auth(AuthError::ScramAccountIneligible))
    ));

    // Case 2: Observation / Schedule / Webhook / FederatedRemote cannot select Member payer.
    for principal in [
        InvocationPrincipal::Observation,
        InvocationPrincipal::Schedule,
        InvocationPrincipal::Webhook,
        InvocationPrincipal::FederatedRemote {
            remote_jid: "mallory@evil.example".to_owned(),
        },
    ] {
        assert!(matches!(
            validate_principal_and_payer(
                &principal,
                PayerSelection::Member {
                    account_id: alice,
                    credential_id: alice_cred,
                }
            ),
            Err(InvocationError::PrincipalCannotUseMemberCredential { .. })
        ));
    }

    // Case 3: Cross-account member credential selection rejected.
    assert!(matches!(
        validate_principal_and_payer(
            &InvocationPrincipal::LocalMember {
                account_id: bob,
                provenance: AuthProvenance::ExternalIdp,
            },
            PayerSelection::Member {
                account_id: alice,
                credential_id: alice_cred,
            }
        ),
        Err(InvocationError::MemberAccountMismatch)
    ));

    // Case 4: Cell-asserted admin flags without broker-verified IdP admin proof rejected.
    assert!(matches!(
        authorize_admin_policy_mutation(
            AuthProvenance::ExternalIdp,
            None,
            &CellAssertedAdminFlags {
                in_server_owner_localparts: true,
                pubsub_owner_affiliation: true,
            },
        ),
        Err(AuthError::AdminProofRequired)
    ));

    // Case 5: Cell role cannot write to Inference AuthzModule.
    let perms = Arc::new(LocalPermissionBackend::new(AuthzCallerRole::Broker));
    perms.provision_tenant_store(&tenant).await.expect("prov");
    let cell_perms = perms.with_caller_role(AuthzCallerRole::Cell);
    assert!(matches!(
        cell_perms
            .write_tuples(
                &tenant,
                AuthzModule::Inference,
                &[AuthzTuple {
                    object: AuthzObject {
                        object_type: AuthzObjectType::TenantCredential,
                        id: tenant_cred.to_string(),
                    },
                    relation: AuthzRelation::new("use"),
                    subject: AuthzSubject {
                        subject_type: AuthzSubjectType::Account,
                        id: alice.to_string(),
                        relation: None,
                    },
                }],
            )
            .await,
        Err(PermissionBackendError::ModuleWriteForbidden(
            AuthzModule::Inference
        ))
    ));

    // Case 6: Member payer without signed invocation fails closed; never falls back to Tenant!
    let ledger = Arc::new(
        TursoAdmissionLedgerStore::open(dir.path().join("authz_matrix.db"))
            .await
            .expect("ledger"),
    );
    let clock = Arc::new(FakeBootClock::new(Duration::from_secs(10)));
    let fence = Arc::new(BrokerReplicaFence::new(
        Uuid::new_v4(),
        Arc::clone(&ledger) as Arc<dyn ReplicaFenceStore>,
        clock,
    ));
    fence.tick_once().await.expect("tick");
    let svc = BrokerAdmissionService::new(
        "broker-1",
        Arc::clone(&ledger),
        Arc::clone(&perms) as Arc<dyn PermissionBackend>,
        Arc::new(DeviceKeyRegistry::new()),
        fence,
        TraceRecorder::new(),
    );
    let holder = HolderInstance {
        spiffe_id: SpiffeId::parse("spiffe://trust.example/ns/waddle/sa/cell-a").expect("spiffe"),
        incarnation_pubkey: IncarnationPubKey::from_bytes([1u8; 32]),
        tls_exporter_proof: TlsExporterProof::new(vec![1u8; 32]).expect("proof"),
    };
    let now = Utc::now().timestamp_millis();
    assert!(matches!(
        svc.admit_invocation(AdmitInvocationInput {
            attempt_id: AttemptId::new_v4(),
            run_id: RunId::new_v4(),
            step_id: StepId::new(0),
            trust_domain: "trust.example".to_owned(),
            tenant_id: tenant.clone(),
            principal: InvocationPrincipal::LocalMember {
                account_id: alice,
                provenance: AuthProvenance::ExternalIdp,
            },
            extension_id: "ai-chatbot".to_owned(),
            extension_digest_sha256: [0xAA; 32],
            provider: "openai".to_owned(),
            payer: PayerSelection::Member {
                account_id: alice,
                credential_id: alice_cred,
            },
            room_or_space: RoomScopeKey {
                scope_id: "general".to_owned(),
            },
            holder: holder.clone(),
            command_digest_sha256: [0xBB; 32],
            invocation_nonce: [1u8; 32],
            expires_at_ms: now + 60_000,
            reserved_spend_microcents: 100,
            budget: BudgetLimits {
                max_input_tokens: 1_000,
                max_output_tokens: 500,
                max_spend_microcents: 5_000,
                max_steps: 2,
            },
            signed_invocation: None,
            now_ms: now,
        })
        .await,
        Err(AdmissionServiceError::Invocation(
            InvocationError::SignedInvocationRequired
        ))
    ));

    // Case 7 & 8: Admin endpoint & SSRF IMDS metadata endpoint unconditionally blocked.
    let custody_ledger: Arc<dyn waddle_broker::ledger::DestructionLedgerStore> = Arc::new(
        TursoLedgerStore::open_in_dir(&dir.path().join("custody"))
            .await
            .expect("custody ledger"),
    );
    let custody = Arc::new(
        BrokerCustodyService::new(
            Arc::new(KeyHierarchyService::new(
                "trust.example",
                Arc::new(LocalKmsProvider::new()),
            )),
            custody_ledger,
        )
        .await
        .expect("custody"),
    );
    let enrolled = custody
        .enroll(EnrollCredentialInput {
            tenant_id: tenant.clone(),
            account_id: None,
            provider: "openai".to_owned(),
            nonce: "gate3-tenant-cred".to_owned(),
            payload_digest_sha256: [0x22; 32],
            expires_at_ms: now + 60_000,
            plaintext: SecretBytes::new(b"sk-tenant-positive".to_vec()).expect("secret"),
        })
        .await
        .expect("enroll");
    perms
        .write_tuples(
            &tenant,
            AuthzModule::Inference,
            &[AuthzTuple {
                object: AuthzObject {
                    object_type: AuthzObjectType::TenantCredential,
                    id: enrolled.credential_id.to_string(),
                },
                relation: AuthzRelation::new("use"),
                subject: AuthzSubject {
                    subject_type: AuthzSubjectType::Account,
                    id: alice.to_string(),
                    relation: None,
                },
            }],
        )
        .await
        .expect("grant tenant cred");

    // Positive control: Tenant payer authorized by broker passes admission!
    let positive_dispatch = svc
        .admit_invocation(AdmitInvocationInput {
            attempt_id: AttemptId::new_v4(),
            run_id: RunId::new_v4(),
            step_id: StepId::new(0),
            trust_domain: "trust.example".to_owned(),
            tenant_id: tenant.clone(),
            principal: InvocationPrincipal::LocalMember {
                account_id: alice,
                provenance: AuthProvenance::ExternalIdp,
            },
            extension_id: "ai-chatbot".to_owned(),
            extension_digest_sha256: [0xAA; 32],
            provider: "openai".to_owned(),
            payer: PayerSelection::Tenant {
                credential_id: enrolled.credential_id,
            },
            room_or_space: RoomScopeKey {
                scope_id: "general".to_owned(),
            },
            holder,
            command_digest_sha256: [0xBB; 32],
            invocation_nonce: [2u8; 32],
            expires_at_ms: now + 60_000,
            reserved_spend_microcents: 100,
            budget: BudgetLimits {
                max_input_tokens: 1_000,
                max_output_tokens: 500,
                max_spend_microcents: 5_000,
                max_steps: 2,
            },
            signed_invocation: None,
            now_ms: now,
        })
        .await
        .expect("positive control tenant dispatch admitted");

    let mut registry = EgressProfileRegistry::new();
    registry
        .register(ProviderProfile {
            provider: "openai".to_owned(),
            deployment: "prod".to_owned(),
            model: "gpt-4o-mini".to_owned(),
            operation: ProviderOperation::ChatCompletions,
            base_origin: "https://api.openai.com".to_owned(),
            operation_path: "/v1/chat/completions".to_owned(),
            pinned_ca_pem: None,
            caps: EgressCaps::default(),
        })
        .expect("register");
    let egress = BrokerEgressClient::with_resolver(
        registry,
        custody,
        Arc::new(FixedResolver(vec![SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254)),
            443,
        )])),
    );
    assert!(matches!(
        egress
            .execute_unary(
                &positive_dispatch,
                &EgressInferenceRequest {
                    provider: "openai".to_owned(),
                    deployment: "prod".to_owned(),
                    model: "gpt-4o-mini".to_owned(),
                    operation: ProviderOperationWire::ChatCompletions,
                    requested_path: Some("/v1/keys".to_owned()),
                    body_json: json!({}),
                    stream: false,
                }
            )
            .await,
        Err(EgressError::Profile(ProfileError::BlockedAdminEndpoint))
    ));
    assert!(matches!(
        egress
            .execute_unary(
                &positive_dispatch,
                &EgressInferenceRequest {
                    provider: "openai".to_owned(),
                    deployment: "prod".to_owned(),
                    model: "gpt-4o-mini".to_owned(),
                    operation: ProviderOperationWire::ChatCompletions,
                    requested_path: None,
                    body_json: json!({}),
                    stream: false,
                }
            )
            .await,
        Err(EgressError::Ssrf(SsrfError::BlockedIpAddress(_)))
    ));
}
