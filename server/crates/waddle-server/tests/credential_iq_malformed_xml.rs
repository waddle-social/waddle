use std::sync::Arc;

use jsonwebtoken::jwk::JwkSet;
use waddle_broker::{
    AuthProvenance, BrokerCustodyService, BrokerSubkeyRing, CellAssertedAdminFlags,
    ColonyJwtVerifier, DeviceKeyRegistry, KeyHierarchyService, LocalKmsProvider,
    SqliteCompatLedgerStore,
};
use waddle_broker_core::identity::{AccountId, TenantId};
use waddle_server::credential_iq::{CredentialIqContext, CredentialIqHandler};
use waddle_xmpp::waddle_credential::MAX_CREDENTIAL_IQ_BYTES;

const SECRET_SENTINEL: &str = "sk-waddle-malformed-xml-sentinel-cafe1234";

async fn build_handler() -> CredentialIqHandler {
    let kms = Arc::new(LocalKmsProvider::new());
    let hierarchy = Arc::new(KeyHierarchyService::new("example.com", kms));
    let ledger = Arc::new(
        SqliteCompatLedgerStore::connect_in_memory()
            .await
            .expect("ledger"),
    );
    let custody = Arc::new(
        BrokerCustodyService::new(hierarchy, ledger)
            .await
            .expect("custody"),
    );
    let subkey_ring = Arc::new(BrokerSubkeyRing::new("example.com"));
    let jwt_verifier = Arc::new(ColonyJwtVerifier::new(
        "https://colony.example.com",
        "urn:waddle:broker:example.com",
        JwkSet { keys: vec![] },
    ));
    let device_registry = Arc::new(DeviceKeyRegistry::new());
    CredentialIqHandler::new(custody, subkey_ring, jwt_verifier, device_registry)
}

fn default_ctx() -> CredentialIqContext {
    CredentialIqContext {
        trust_domain: "example.com".to_owned(),
        caller_tenant_id: TenantId::parse("example.com").expect("tenant"),
        caller_account_id: AccountId::new_v4(),
        provenance: AuthProvenance::ExternalIdp,
        cell_admin_flags: CellAssertedAdminFlags::default(),
        expected_challenge: "chal-1".to_owned(),
        now_ms: 1_760_000_000_000,
    }
}

#[tokio::test]
async fn malformed_xml_unknown_version_and_oversized_return_opaque_errors_without_echo() {
    let handler = build_handler().await;
    let ctx = default_ctx();

    // 1. Malformed XML carrying secret sentinel.
    let malformed = format!(
        "<iq type='set' id='bad-1'><enroll-credential xmlns='urn:waddle:credential:1'>{SECRET_SENTINEL}"
    );
    let resp_malformed = handler.handle_raw_iq_xml(&malformed, &ctx).await;
    assert!(resp_malformed.contains("type='error'") || resp_malformed.contains("type=\"error\""));
    assert!(!resp_malformed.contains(SECRET_SENTINEL));
    assert!(!resp_malformed.contains("urn:waddle:credential:1"));

    // 2. Unknown protocol version `urn:waddle:credential:99`.
    let unknown_ver = format!(
        "<iq xmlns='jabber:client' type='set' to='example.com' id='ver-99'><enroll-credential xmlns='urn:waddle:credential:99' version='99'>{SECRET_SENTINEL}</enroll-credential></iq>"
    );
    let resp_unknown_ver = handler.handle_raw_iq_xml(&unknown_ver, &ctx).await;
    assert!(
        resp_unknown_ver.contains("type='error'") || resp_unknown_ver.contains("type=\"error\"")
    );
    assert!(!resp_unknown_ver.contains(SECRET_SENTINEL));
    assert!(!resp_unknown_ver.contains("urn:waddle:credential:99"));

    // 3. Oversized stanza (> MAX_CREDENTIAL_IQ_BYTES) carrying sentinel.
    let pad = "A".repeat(MAX_CREDENTIAL_IQ_BYTES + 1024);
    let oversized = format!(
        "<iq xmlns='jabber:client' type='set' to='example.com' id='big-1'><enroll-credential xmlns='urn:waddle:credential:1'>{SECRET_SENTINEL}{pad}</enroll-credential></iq>"
    );
    let resp_oversized = handler.handle_raw_iq_xml(&oversized, &ctx).await;
    assert!(resp_oversized.contains("type='error'") || resp_oversized.contains("type=\"error\""));
    assert!(resp_oversized.contains("policy-violation"));
    assert!(!resp_oversized.contains(SECRET_SENTINEL));
}
