use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use proptest::prelude::{any, prop_assert_eq, proptest};
use uuid::Uuid;
use waddle_broker::{
    decrypt_secret, encrypt_secret, BrokerError, CryptoError, EnvelopeContext, Key256,
    KeyHierarchyService, KmsError, KmsKeyRef, KmsProvider, LocalKmsProvider, ScalewayKmsConfig,
    ScalewayKmsProvider, SecretBytes, VaultTransitConfig, VaultTransitKmsProvider, WrappedKeyBlob,
    MAX_SECRET_BYTES,
};
use waddle_broker_core::identity::{AccountId, TenantId};
use zeroize::Zeroizing;

fn tenant(raw: &str) -> TenantId {
    TenantId::parse(raw).expect("valid tenant id")
}

#[test]
fn envelope_roundtrip_and_redacted_debug_no_sentinel_leak() {
    let sentinel = "sk-waddle-m3-envelope-sentinel-8a7b6c5d4e3f";
    let secret = SecretBytes::from_slice(sentinel.as_bytes()).expect("valid secret");
    let dek = Key256::generate().expect("dek");
    let ctx = EnvelopeContext {
        trust_domain: "waddle.social".to_owned(),
        tenant_id: tenant("alpha.waddle.social"),
        account_id: Some(AccountId::new_v4()),
        provider: "openrouter".to_owned(),
        credential_id: Uuid::now_v7(),
        version: 1,
    };

    let env = encrypt_secret(&dek, &ctx, Uuid::now_v7(), 1, &secret).expect("encrypt");
    let debug_secret = format!("{secret:?}");
    let debug_dek = format!("{dek:?}");
    let env_json = serde_json::to_string(&env).expect("serialize envelope");

    assert!(!debug_secret.contains(sentinel));
    assert!(debug_secret.contains("REDACTED"));
    assert!(debug_dek.contains("REDACTED"));
    assert!(!env_json.contains(sentinel));

    let recovered = decrypt_secret(&dek, &ctx, &env).expect("decrypt");
    assert_eq!(recovered, secret);
}

#[test]
fn aead_boundary_rejects_empty_oversized_and_single_bit_tamper() {
    assert!(matches!(
        SecretBytes::new(Vec::new()),
        Err(CryptoError::EmptySecret)
    ));
    assert!(matches!(
        SecretBytes::new(vec![b'x'; MAX_SECRET_BYTES + 1]),
        Err(CryptoError::SecretTooLarge {
            max: MAX_SECRET_BYTES
        })
    ));
    let max_ok = SecretBytes::new(vec![b'a'; MAX_SECRET_BYTES]).expect("16 KiB boundary accepted");
    assert_eq!(max_ok.len(), MAX_SECRET_BYTES);

    let dek = Key256::generate().expect("dek");
    let ctx = EnvelopeContext {
        trust_domain: "waddle.social".to_owned(),
        tenant_id: tenant("alpha.waddle.social"),
        account_id: Some(AccountId::new_v4()),
        provider: "anthropic".to_owned(),
        credential_id: Uuid::now_v7(),
        version: 1,
    };
    let secret = SecretBytes::from_slice(b"sk-ant-boundary-test").expect("secret");
    let env = encrypt_secret(&dek, &ctx, Uuid::now_v7(), 1, &secret).expect("encrypt");

    // 1-bit flip in AEAD tag (last byte of ciphertext).
    let mut tag_flipped = env.clone();
    let last_idx = tag_flipped.ciphertext.len() - 1;
    tag_flipped.ciphertext[last_idx] ^= 0x01;
    assert!(matches!(
        decrypt_secret(&dek, &ctx, &tag_flipped),
        Err(CryptoError::DecryptFailed)
    ));

    // 1-bit flip in nonce.
    let mut nonce_flipped = env.clone();
    nonce_flipped.nonce[0] ^= 0x01;
    assert!(matches!(
        decrypt_secret(&dek, &ctx, &nonce_flipped),
        Err(CryptoError::DecryptFailed)
    ));

    // 1-bit/field tamper in AAD (tenant_id, provider, account_id, version).
    let mut wrong_tenant = ctx.clone();
    wrong_tenant.tenant_id = tenant("beta.waddle.social");
    assert!(matches!(
        decrypt_secret(&dek, &wrong_tenant, &env),
        Err(CryptoError::DecryptFailed)
    ));

    let mut wrong_provider = ctx.clone();
    wrong_provider.provider = "openai".to_owned();
    assert!(matches!(
        decrypt_secret(&dek, &wrong_provider, &env),
        Err(CryptoError::DecryptFailed)
    ));

    let mut wrong_version = ctx.clone();
    wrong_version.version = 2;
    assert!(matches!(
        decrypt_secret(&dek, &wrong_version, &env),
        Err(CryptoError::DecryptFailed)
    ));
}

#[tokio::test]
async fn kms_three_tier_hierarchy_shredding_and_outage_fail_closed() {
    let kms = Arc::new(LocalKmsProvider::new());
    let hierarchy = KeyHierarchyService::new("waddle.social", kms.clone());
    let t = tenant("alpha.waddle.social");
    let alice = AccountId::new_v4();

    let (tenant_rec, tenant_dek) = hierarchy
        .create_tenant_dek(&t, 1)
        .await
        .expect("tenant dek");
    let (user_rec, user_eff_key) = hierarchy
        .create_user_dek(&tenant_rec, &tenant_dek, alice, 1)
        .await
        .expect("user dek");

    // Unwrap succeeds normally.
    let unwrapped = hierarchy
        .unwrap_user_dek(&tenant_rec, &user_rec)
        .await
        .expect("unwrap user dek");
    assert_eq!(unwrapped, user_eff_key);

    // Simulated KMS outage fails closed.
    kms.set_available(false);
    let outage_err = hierarchy.unwrap_user_dek(&tenant_rec, &user_rec).await;
    assert!(matches!(
        outage_err,
        Err(BrokerError::Kms(KmsError::Unavailable(_)))
    ));
    kms.set_available(true);

    // Destroying the KMS per-user wrapping secret cryptographically shreds user DEK.
    let _ = hierarchy
        .destroy_user_kms_secret(&t, alice)
        .await
        .expect("destroy user kms secret");
    let shredded = hierarchy.unwrap_user_dek(&tenant_rec, &user_rec).await;
    assert!(matches!(
        shredded,
        Err(BrokerError::Kms(KmsError::KeyDestroyed(_)))
    ));

    // Tenant DEK itself remains intact for other users until tenant KEK is destroyed.
    let _ = hierarchy
        .unwrap_tenant_dek(&tenant_rec)
        .await
        .expect("tenant dek still usable");
    let _ = hierarchy
        .destroy_tenant_kms_kek(&t)
        .await
        .expect("destroy tenant kek");
    let tenant_shredded = hierarchy.unwrap_tenant_dek(&tenant_rec).await;
    assert!(matches!(
        tenant_shredded,
        Err(BrokerError::Kms(KmsError::KeyDestroyed(_)))
    ));
}

#[test]
fn kms_external_provider_configs_validate_and_redact_tokens() {
    let scw_cfg = ScalewayKmsConfig {
        base_url: "https://api.scaleway.com".to_owned(),
        region: "fr-par".to_owned(),
        project_id: "11111111-2222-3333-4444-555555555555".to_owned(),
        auth_token: Zeroizing::new("scw-secret-token-xyz".to_owned()),
        root_key_id: None,
        timeout: Duration::from_secs(5),
    };
    assert!(!format!("{scw_cfg:?}").contains("scw-secret-token-xyz"));
    let _ = ScalewayKmsProvider::new(scw_cfg).expect("valid scaleway config");

    let vault_cfg = VaultTransitConfig {
        base_url: "https://vault.internal:8200".to_owned(),
        mount: "transit".to_owned(),
        token: Zeroizing::new("hvs.vault-secret-token".to_owned()),
        namespace: Some("waddle".to_owned()),
        timeout: Duration::from_secs(5),
    };
    assert!(!format!("{vault_cfg:?}").contains("hvs.vault-secret-token"));
    let _ = VaultTransitKmsProvider::new(vault_cfg).expect("valid vault config");
}

proptest! {
    #[test]
    fn envelope_proptest_roundtrip(payload in proptest::collection::vec(any::<u8>(), 1..512)) {
        let secret = SecretBytes::new(payload).expect("valid secret");
        let dek = Key256::generate().expect("dek");
        let ctx = EnvelopeContext {
            trust_domain: "waddle.social".to_owned(),
            tenant_id: tenant("prop.waddle.social"),
            account_id: None,
            provider: "openrouter".to_owned(),
            credential_id: Uuid::now_v7(),
            version: 1,
        };
        let env = encrypt_secret(&dek, &ctx, Uuid::now_v7(), 1, &secret).expect("encrypt");
        let dec = decrypt_secret(&dek, &ctx, &env).expect("decrypt");
        prop_assert_eq!(dec.as_bytes(), secret.as_bytes());
    }
}

#[derive(Default)]
struct RecordingVaultStyleKms {
    destroyed: Mutex<Vec<String>>,
}

#[async_trait]
impl KmsProvider for RecordingVaultStyleKms {
    async fn wrap_key(
        &self,
        key_ref: &KmsKeyRef,
        plaintext: &Key256,
        _aad: &[u8],
    ) -> Result<WrappedKeyBlob, KmsError> {
        Ok(WrappedKeyBlob {
            key_ref: key_ref.clone(),
            ciphertext: plaintext.as_bytes().to_vec(),
        })
    }

    async fn unwrap_key(
        &self,
        _key_ref: &KmsKeyRef,
        wrapped: &WrappedKeyBlob,
        _aad: &[u8],
    ) -> Result<Key256, KmsError> {
        let arr: [u8; 32] = wrapped
            .ciphertext
            .clone()
            .try_into()
            .map_err(|_| KmsError::Crypto("invalid len".to_owned()))?;
        Ok(Key256::from_bytes(arr))
    }

    fn user_wrapping_secret_ref(
        &self,
        tenant: &TenantId,
        account: AccountId,
    ) -> Result<KmsKeyRef, KmsError> {
        KmsKeyRef::new(format!("vault-custom-user-{}-{}", tenant.as_str(), account))
    }

    fn tenant_kek_ref(&self, tenant: &TenantId) -> Result<KmsKeyRef, KmsError> {
        KmsKeyRef::new(format!("vault-custom-tenant-{}", tenant.as_str()))
    }

    async fn ensure_user_wrapping_secret(
        &self,
        tenant: &TenantId,
        account: AccountId,
    ) -> Result<KmsKeyRef, KmsError> {
        self.user_wrapping_secret_ref(tenant, account)
    }

    async fn ensure_tenant_kek(&self, tenant: &TenantId) -> Result<KmsKeyRef, KmsError> {
        self.tenant_kek_ref(tenant)
    }

    async fn destroy_wrapping_secret(&self, key_ref: &KmsKeyRef) -> Result<(), KmsError> {
        self.destroyed
            .lock()
            .expect("lock")
            .push(key_ref.as_str().to_owned());
        Ok(())
    }

    async fn is_destroyed(&self, key_ref: &KmsKeyRef) -> Result<bool, KmsError> {
        Ok(self
            .destroyed
            .lock()
            .expect("lock")
            .iter()
            .any(|k| k == key_ref.as_str()))
    }
}

#[tokio::test]
async fn kms_provider_key_ref_resolution_matches_provider_format() {
    let t = tenant("alpha.waddle.social");
    let acct = AccountId::new_v4();

    let local = LocalKmsProvider::new();
    assert_eq!(
        local.tenant_kek_ref(&t).expect("local tenant ref").as_str(),
        format!("kms:tenant:{}", t.as_str())
    );
    assert_eq!(
        local
            .user_wrapping_secret_ref(&t, acct)
            .expect("local user ref")
            .as_str(),
        format!("kms:user:{}:{}", t.as_str(), acct)
    );

    let scw = ScalewayKmsProvider::new(ScalewayKmsConfig {
        base_url: "https://api.scaleway.com".to_owned(),
        region: "fr-par".to_owned(),
        project_id: "11111111-2222-3333-4444-555555555555".to_owned(),
        auth_token: Zeroizing::new("scw-token".to_owned()),
        root_key_id: None,
        timeout: Duration::from_secs(5),
    })
    .expect("scw");
    assert_eq!(
        scw.tenant_kek_ref(&t).expect("scw tenant ref").as_str(),
        format!("scw-tenant-{}", t.as_str())
    );
    assert_eq!(
        scw.user_wrapping_secret_ref(&t, acct)
            .expect("scw user ref")
            .as_str(),
        format!("scw-user-{}-{}", t.as_str(), acct)
    );

    let vault = VaultTransitKmsProvider::new(VaultTransitConfig {
        base_url: "https://vault.internal:8200".to_owned(),
        mount: "transit".to_owned(),
        token: Zeroizing::new("hvs.token".to_owned()),
        namespace: None,
        timeout: Duration::from_secs(5),
    })
    .expect("vault");
    assert_eq!(
        vault.tenant_kek_ref(&t).expect("vault tenant ref").as_str(),
        format!("vault-tenant-{}", t.as_str())
    );
    assert_eq!(
        vault
            .user_wrapping_secret_ref(&t, acct)
            .expect("vault user ref")
            .as_str(),
        format!("vault-user-{}-{}", t.as_str(), acct)
    );

    // Verify KeyHierarchyService::destroy_* resolves refs via KmsProvider, not hardcoded "kms:...".
    let custom_kms = Arc::new(RecordingVaultStyleKms::default());
    let hierarchy = KeyHierarchyService::new("waddle.social", custom_kms.clone());
    let destroyed_user_ref = hierarchy
        .destroy_user_kms_secret(&t, acct)
        .await
        .expect("destroy user ref");
    let destroyed_tenant_ref = hierarchy
        .destroy_tenant_kms_kek(&t)
        .await
        .expect("destroy tenant ref");
    assert_eq!(
        destroyed_user_ref.as_str(),
        format!("vault-custom-user-{}-{}", t.as_str(), acct)
    );
    assert_eq!(
        destroyed_tenant_ref.as_str(),
        format!("vault-custom-tenant-{}", t.as_str())
    );
    let recorded = custom_kms.destroyed.lock().expect("lock").clone();
    assert_eq!(
        recorded,
        vec![
            format!("vault-custom-user-{}-{}", t.as_str(), acct),
            format!("vault-custom-tenant-{}", t.as_str()),
        ]
    );
}
