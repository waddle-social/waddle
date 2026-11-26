use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use sha2::{Digest, Sha256};
use uuid::Uuid;
use waddle_broker::admission::replica_fence::BrokerReplicaFence;
use waddle_broker::admission::service::{BrokerAdmissionService, TraceRecorder};
use waddle_broker::admission::turso::TursoAdmissionLedgerStore;
use waddle_broker::admission::ReplicaFenceStore;
use waddle_broker::authz::local::LocalPermissionBackend;
use waddle_broker::authz::AuthzCallerRole;
use waddle_broker::custody::{BrokerCustodyService, EnrollCredentialInput};
use waddle_broker::egress::client::BrokerEgressClient;
use waddle_broker::egress::profile::{
    EgressBindingPolicy, EgressCaps, EgressProfileRegistry, ProviderOperation, ProviderProfile,
};
use waddle_broker::egress::ssrf::{DnsResolver, SsrfError};
use waddle_broker::idp_proof::DeviceKeyRegistry;
use waddle_broker::key_hierarchy::KeyHierarchyService;
use waddle_broker::kms::local::LocalKmsProvider;
use waddle_broker::kms::KmsProvider;
use waddle_broker::ledger::turso::TursoLedgerStore;
use waddle_broker::secret::SecretBytes;
use waddle_broker_core::admission::BudgetLimits;
use waddle_broker_core::boot_clock::SystemBootClock;
use waddle_broker_core::holder_pop::HolderKeypair;
use waddle_broker_core::identity::{AccountId, HolderInstance, SpiffeId, TenantId};
use waddle_broker_core::permission::{
    AuthzModule, AuthzObject, AuthzObjectType, AuthzRelation, AuthzSubject, AuthzTuple,
    PermissionBackend,
};
use waddle_extensions::PluginId;
use waddle_server::server::extension_inference_host::{
    BrokerCredentialResolver, BrokerExtensionInferenceHost, BrokerExtensionInferenceHostConfig,
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

pub fn default_budget() -> BudgetLimits {
    BudgetLimits {
        max_input_tokens: 4_096,
        max_output_tokens: 1_024,
        max_spend_microcents: 50_000,
        max_steps: 4,
    }
}

pub struct BrokerTestConfig<'a> {
    pub data_dir: &'a Path,
    pub trust_domain: &'static str,
    pub broker_id: &'static str,
    pub spiffe_id: &'static str,
    pub channel_binding: [u8; 32],
    pub provider: &'static str,
    pub deployment: &'static str,
    pub model: &'static str,
    pub base_origin: &'static str,
    pub operation_path: &'static str,
    pub plugin_id: &'static str,
    pub credential_secret: &'a [u8],
    pub credential_account_id: Option<AccountId>,
    pub permission_grant: Option<(AuthzObjectType, AuthzSubject)>,
    pub device_keys: Arc<DeviceKeyRegistry>,
    pub trace: TraceRecorder,
    pub server_addr: SocketAddr,
    pub ca_pem: Vec<u8>,
}

pub struct BrokerTestEnv {
    trust_domain: String,
    deployment: String,
    pub tenant_id: TenantId,
    pub credential_id: Uuid,
    pub plugin_id: PluginId,
    pub spiffe_id: SpiffeId,
    pub channel_binding: [u8; 32],
    pub holder: HolderInstance,
    pub admission: Arc<BrokerAdmissionService<TursoAdmissionLedgerStore>>,
    pub egress: Arc<BrokerEgressClient>,
}

impl BrokerTestEnv {
    pub fn build_host_config(
        &self,
        reserved_spend_microcents: u64,
    ) -> BrokerExtensionInferenceHostConfig {
        BrokerExtensionInferenceHostConfig {
            trust_domain: self.trust_domain.clone(),
            holder: self.holder.clone(),
            peer_spiffe_id: self.spiffe_id.clone(),
            channel_binding: self.channel_binding,
            default_budget: default_budget(),
            default_deployment: self.deployment.clone(),
            default_reserved_spend_microcents: reserved_spend_microcents,
        }
    }

    pub fn build_host(
        &self,
        resolver: Arc<dyn BrokerCredentialResolver>,
        reserved_spend_microcents: u64,
    ) -> BrokerExtensionInferenceHost<TursoAdmissionLedgerStore> {
        BrokerExtensionInferenceHost::new(
            self.build_host_config(reserved_spend_microcents),
            Arc::clone(&self.admission),
            Arc::clone(&self.egress),
            resolver,
        )
    }
}

pub async fn build_broker_test_env(
    cfg: BrokerTestConfig<'_>,
) -> (
    BrokerTestEnv,
    Arc<TursoAdmissionLedgerStore>,
    Arc<BrokerReplicaFence>,
) {
    let tenant_id = TenantId::parse(cfg.trust_domain).expect("tenant");
    let spiffe_id = SpiffeId::parse(cfg.spiffe_id).expect("spiffe");
    let keypair = HolderKeypair::generate().expect("keypair");
    let holder = keypair.sign_channel_binding(&spiffe_id, &cfg.channel_binding);

    let ledger = Arc::new(
        TursoAdmissionLedgerStore::open(cfg.data_dir.join("admission.db"))
            .await
            .expect("admission db"),
    );
    let replica_fence = Arc::new(BrokerReplicaFence::new(
        Uuid::new_v4(),
        Arc::clone(&ledger) as Arc<dyn ReplicaFenceStore>,
        Arc::new(SystemBootClock),
    ));
    replica_fence.tick_once().await.expect("tick_once");

    let custody_ledger = Arc::new(
        TursoLedgerStore::open_in_dir(&cfg.data_dir.join("custody"))
            .await
            .expect("custody db"),
    );
    let kms: Arc<dyn KmsProvider> = Arc::new(LocalKmsProvider::new());
    let hierarchy = Arc::new(KeyHierarchyService::new(cfg.trust_domain, kms));
    let custody = Arc::new(
        BrokerCustodyService::new(hierarchy, custody_ledger)
            .await
            .expect("custody service"),
    );
    let mut payload_digest = [0u8; 32];
    payload_digest.copy_from_slice(&Sha256::digest(cfg.credential_secret));
    let cred = custody
        .enroll(EnrollCredentialInput {
            tenant_id: tenant_id.clone(),
            account_id: cfg.credential_account_id,
            provider: cfg.provider.to_owned(),
            nonce: format!("nonce-{}", Uuid::new_v4()),
            payload_digest_sha256: payload_digest,
            expires_at_ms: Utc::now().timestamp_millis() + 60_000,
            plaintext: SecretBytes::new(cfg.credential_secret.to_vec()).expect("secret"),
        })
        .await
        .expect("enroll cred");

    let perms = Arc::new(LocalPermissionBackend::new(AuthzCallerRole::Broker));
    perms
        .provision_tenant_store(&tenant_id)
        .await
        .expect("provision store");
    if let Some((object_type, subject)) = cfg.permission_grant {
        perms
            .write_tuples(
                &tenant_id,
                AuthzModule::Inference,
                &[AuthzTuple {
                    object: AuthzObject {
                        object_type,
                        id: cred.credential_id.to_string(),
                    },
                    relation: AuthzRelation::new("use"),
                    subject,
                }],
            )
            .await
            .expect("write inference tuple");
    }

    let plugin_id = PluginId::try_new(cfg.plugin_id).expect("plugin id");
    let mut ext_digest = [0u8; 32];
    ext_digest.copy_from_slice(&Sha256::digest(plugin_id.as_str().as_bytes()));
    let mut profile_registry = EgressProfileRegistry::new();
    profile_registry
        .register_with_policy(
            ProviderProfile {
                provider: cfg.provider.to_owned(),
                deployment: cfg.deployment.to_owned(),
                model: cfg.model.to_owned(),
                operation: ProviderOperation::ChatCompletions,
                base_origin: cfg.base_origin.to_owned(),
                operation_path: cfg.operation_path.to_owned(),
                caps: EgressCaps {
                    max_request_bytes: 64 * 1024,
                    max_response_bytes: 1024 * 1024,
                    max_chunk_bytes: 64 * 1024,
                    request_timeout_ms: 5_000,
                    max_concurrent_requests: 4,
                    max_spend_microcents: 50_000,
                },
                pinned_ca_pem: Some(cfg.ca_pem),
            },
            EgressBindingPolicy {
                expected_trust_domain: Some(cfg.trust_domain.to_owned()),
                allowed_extension_ids: vec![cfg.plugin_id.to_owned()],
                allowed_extension_digests: vec![ext_digest],
            },
        )
        .expect("register profile");

    let egress = Arc::new(BrokerEgressClient::with_loopback_tls_test_override(
        profile_registry,
        custody,
        Arc::new(MockDnsResolver {
            addrs: vec![SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)),
                443,
            )],
        }),
        cfg.server_addr,
    ));

    let admission = Arc::new(BrokerAdmissionService::new(
        cfg.broker_id,
        Arc::clone(&ledger),
        perms,
        cfg.device_keys,
        Arc::clone(&replica_fence),
        cfg.trace,
    ));

    let env = BrokerTestEnv {
        trust_domain: cfg.trust_domain.to_owned(),
        deployment: cfg.deployment.to_owned(),
        tenant_id,
        credential_id: cred.credential_id,
        plugin_id,
        spiffe_id,
        channel_binding: cfg.channel_binding,
        holder,
        admission,
        egress,
    };

    (env, ledger, replica_fence)
}
