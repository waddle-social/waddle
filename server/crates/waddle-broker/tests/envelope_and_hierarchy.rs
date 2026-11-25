use std::sync::Arc;
use std::time::Duration;

use proptest::prelude::{any, prop_assert_eq, proptest};
use uuid::Uuid;
use waddle_broker::{
    decrypt_secret, encrypt_secret, BrokerError, CryptoError, EnvelopeContext, Key256,
    KeyHierarchyService, KmsError, LocalKmsProvider, ScalewayKmsConfig, ScalewayKmsProvider,
    SecretBytes, VaultTransitConfig, VaultTransitKmsProvider, MAX_SECRET_BYTES,
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
        auth_token: Zeroizing::new("scw-secret-token-xyz".to_owned()),
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
