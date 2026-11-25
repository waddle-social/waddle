use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use jsonwebtoken::jwk::{
    AlgorithmParameters, CommonParameters, EllipticCurve, EllipticCurveKeyParameters, Jwk, JwkSet,
    KeyAlgorithm, PublicKeyUse,
};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::pkcs8::EncodePrivateKey;
use p256::SecretKey;
use ring::rand::{SecureRandom, SystemRandom};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use waddle_broker::{
    authorize_admin_policy_mutation, build_device_enroll_pop_transcript, enforce_not_scram,
    p256_jwk_thumbprint, seal_credential_for_broker, AuthError, AuthProvenance, BrokerSubkeyRing,
    CellAssertedAdminFlags, ClientSealContext, ColonyBrokerClaims, ColonyJwtVerifier,
    ConfirmationClaim, DeviceKeyRegistry, RegisteredWebAuthnKey, RootBkskPinSet, RootBkskSigner,
    SealError, SecretBytes, SignedDeviceCommand, WebAuthnVerifier,
};
use waddle_broker_core::identity::{AccountId, TenantId};

fn tenant(raw: &str) -> TenantId {
    TenantId::parse(raw).expect("valid tenant")
}

fn gen_p256_key() -> SecretKey {
    let mut buf = [0u8; 32];
    let rng = SystemRandom::new();
    loop {
        rng.fill(&mut buf).expect("rng");
        if let Ok(sk) = SecretKey::from_slice(&buf) {
            return sk;
        }
    }
}

fn make_es256_jwk_and_encoding_key(kid: &str) -> (Jwk, EncodingKey) {
    let sk = gen_p256_key();
    let pk = sk.public_key();
    let point = pk.to_encoded_point(false);
    let x = URL_SAFE_NO_PAD.encode(point.x().expect("x"));
    let y = URL_SAFE_NO_PAD.encode(point.y().expect("y"));
    let jwk = Jwk {
        common: CommonParameters {
            public_key_use: Some(PublicKeyUse::Signature),
            key_operations: None,
            key_algorithm: Some(KeyAlgorithm::ES256),
            key_id: Some(kid.to_owned()),
            x509_url: None,
            x509_chain: None,
            x509_sha1_fingerprint: None,
            x509_sha256_fingerprint: None,
        },
        algorithm: AlgorithmParameters::EllipticCurve(EllipticCurveKeyParameters {
            key_type: jsonwebtoken::jwk::EllipticCurveKeyType::EC,
            curve: EllipticCurve::P256,
            x,
            y,
        }),
    };
    let pem = sk
        .to_pkcs8_pem(p256::pkcs8::LineEnding::LF)
        .expect("pkcs8 pem");
    let enc = EncodingKey::from_ec_pem(pem.as_bytes()).expect("ec encoding key");
    (jwk, enc)
}

fn sign_colony_jwt(
    kid: Option<&str>,
    alg: Algorithm,
    enc: &EncodingKey,
    claims: &ColonyBrokerClaims,
) -> String {
    let mut header = Header::new(alg);
    header.kid = kid.map(str::to_owned);
    encode(&header, claims, enc).expect("sign jwt")
}

#[test]
fn idp_intent_colony_jwt_and_webauthn_verification() {
    let (jwk, enc) = make_es256_jwk_and_encoding_key("colony-k1");
    let verifier = ColonyJwtVerifier::new(
        "https://colony.waddle.social",
        "urn:waddle:broker:waddle.social",
        JwkSet { keys: vec![jwk] },
    );

    let t = tenant("alpha.waddle.social");
    let acct = AccountId::new_v4();
    let dev_sk = gen_p256_key();
    let dev_pk_sec1 = dev_sk.public_key().to_sec1_bytes().to_vec();
    let jkt = p256_jwk_thumbprint(&dev_pk_sec1).expect("jkt");

    let now = 1_760_000_000u64;
    let claims = ColonyBrokerClaims {
        iss: "https://colony.waddle.social".to_owned(),
        sub: acct.to_string(),
        aud: "urn:waddle:broker:waddle.social".to_owned(),
        exp: now + 120,
        nbf: now - 5,
        iat: now,
        jti: Uuid::now_v7().to_string(),
        tenant_id: t.as_str().to_owned(),
        challenge: "chal-enroll-1".to_owned(),
        cnf: Some(ConfirmationClaim { jkt: jkt.clone() }),
        waddle_roles: vec!["tenant_admin".to_owned()],
    };
    let jwt = sign_colony_jwt(Some("colony-k1"), Algorithm::ES256, &enc, &claims);
    let verified = verifier
        .verify(&jwt, &t, acct, "chal-enroll-1", Some(&jkt), now)
        .expect("valid colony jwt");
    assert!(verified.is_tenant_admin);
    assert_eq!(verified.device_jkt.as_deref(), Some(jkt.as_str()));

    // WebAuthn assertion verification with UP+UV and monotonic signCount.
    let wa = WebAuthnVerifier::new(
        "waddle.social",
        vec!["https://app.waddle.social".to_owned()],
    );
    let wa_sk = gen_p256_key();
    let wa_signing = SigningKey::from(&wa_sk);
    wa.register_credential(
        "cred-wa-1".to_owned(),
        RegisteredWebAuthnKey {
            account_id: acct,
            tenant_id: t.clone(),
            public_key_sec1: wa_sk.public_key().to_sec1_bytes().to_vec(),
            sign_count: 1,
            is_tenant_admin: true,
        },
    );

    let challenge_b64 = URL_SAFE_NO_PAD.encode(b"broker-wa-challenge-1");
    let client_data_json = format!(
        r#"{{"type":"webauthn.get","challenge":"{challenge_b64}","origin":"https://app.waddle.social"}}"#
    );
    let mut auth_data = Vec::new();
    auth_data.extend_from_slice(&Sha256::digest(b"waddle.social"));
    auth_data.push(0x05); // UP | UV
    auth_data.extend_from_slice(&2u32.to_be_bytes()); // signCount = 2 > 1

    let mut signed_msg = auth_data.clone();
    signed_msg.extend_from_slice(&Sha256::digest(client_data_json.as_bytes()));
    let sig: Signature = wa_signing.sign(&signed_msg);

    let wa_intent = wa
        .verify_assertion(
            "cred-wa-1",
            &auth_data,
            client_data_json.as_bytes(),
            &sig.to_bytes()[..],
            &t,
            acct,
            &challenge_b64,
        )
        .expect("valid webauthn assertion");
    assert!(wa_intent.is_tenant_admin);

    // Replaying same signCount = 2 must fail with SignCountRollback.
    let replay = wa.verify_assertion(
        "cred-wa-1",
        &auth_data,
        client_data_json.as_bytes(),
        &sig.to_bytes()[..],
        &t,
        acct,
        &challenge_b64,
    );
    assert!(matches!(replay, Err(AuthError::SignCountRollback)));
}

#[test]
fn device_key_enrollment_and_single_use_signed_commands() {
    let (jwk, enc) = make_es256_jwk_and_encoding_key("colony-k1");
    let verifier = ColonyJwtVerifier::new(
        "https://colony.waddle.social",
        "urn:waddle:broker:waddle.social",
        JwkSet { keys: vec![jwk] },
    );

    let t = tenant("alpha.waddle.social");
    let acct = AccountId::new_v4();
    let dev_sk = gen_p256_key();
    let dev_signing = SigningKey::from(&dev_sk);
    let dev_pk_sec1 = dev_sk.public_key().to_sec1_bytes().to_vec();
    let jkt = p256_jwk_thumbprint(&dev_pk_sec1).expect("jkt");

    let now_s = 1_760_000_000u64;
    let now_ms = (now_s * 1000) as i64;
    let claims = ColonyBrokerClaims {
        iss: "https://colony.waddle.social".to_owned(),
        sub: acct.to_string(),
        aud: "urn:waddle:broker:waddle.social".to_owned(),
        exp: now_s + 120,
        nbf: now_s - 5,
        iat: now_s,
        jti: Uuid::now_v7().to_string(),
        tenant_id: t.as_str().to_owned(),
        challenge: "enroll-dev-1".to_owned(),
        cnf: Some(ConfirmationClaim { jkt: jkt.clone() }),
        waddle_roles: vec![],
    };
    let jwt = sign_colony_jwt(Some("colony-k1"), Algorithm::ES256, &enc, &claims);
    let intent = verifier
        .verify(&jwt, &t, acct, "enroll-dev-1", Some(&jkt), now_s)
        .expect("verified intent");

    let registry = DeviceKeyRegistry::new();
    let device_key_id = Uuid::now_v7();
    let expires_ms = now_ms + 60_000;
    let pop_transcript = build_device_enroll_pop_transcript(
        "waddle.social",
        &t,
        acct,
        device_key_id,
        "pop-nonce-1",
        expires_ms,
    );
    let pop_sig: Signature = dev_signing.sign(&pop_transcript);

    let enrolled = registry
        .enroll_device_key(
            AuthProvenance::ExternalIdp,
            &intent,
            "waddle.social",
            device_key_id,
            dev_pk_sec1,
            "pop-nonce-1",
            expires_ms,
            now_ms,
            &pop_sig.to_bytes()[..],
        )
        .expect("device key enrolled");
    assert!(enrolled.active);

    let cmd = SignedDeviceCommand {
        trust_domain: "waddle.social",
        tenant_id: &t,
        account_id: acct,
        device_key_id,
        operation: "invoke-inference",
        target_id: "room-general",
        provider: "openrouter",
        nonce: "cmd-nonce-1",
        expires_at_ms: now_ms + 30_000,
        payload_sha256: Sha256::digest(b"prompt-hash").into(),
    };
    let cmd_sig: Signature = dev_signing.sign(&cmd.canonical_transcript());
    registry
        .verify_signed_command(&cmd, &cmd_sig.to_bytes()[..], now_ms)
        .expect("valid signed command");

    // Single-use nonce replay must fail closed.
    assert!(matches!(
        registry.verify_signed_command(&cmd, &cmd_sig.to_bytes()[..], now_ms),
        Err(AuthError::CommandNonceReplay)
    ));

    // Revoking device key rejects subsequent fresh commands.
    registry.revoke_device_key(device_key_id);
    let cmd2 = SignedDeviceCommand {
        nonce: "cmd-nonce-2",
        ..cmd
    };
    let cmd2_sig: Signature = dev_signing.sign(&cmd2.canonical_transcript());
    assert!(matches!(
        registry.verify_signed_command(&cmd2, &cmd2_sig.to_bytes()[..], now_ms),
        Err(AuthError::UnknownOrInactiveDeviceKey(_))
    ));
}

#[test]
fn reject_scram_native_users_for_credentials_and_policy() {
    assert!(matches!(
        enforce_not_scram(AuthProvenance::NativeScram),
        Err(AuthError::ScramAccountIneligible)
    ));

    let flags = CellAssertedAdminFlags {
        in_server_owner_localparts: true,
        pubsub_owner_affiliation: true,
    };
    assert!(matches!(
        authorize_admin_policy_mutation(AuthProvenance::NativeScram, None, &flags),
        Err(AuthError::ScramAccountIneligible)
    ));
}

#[test]
fn reject_cell_admin_flags_without_idp_admin_proof() {
    let cell_flags = CellAssertedAdminFlags {
        in_server_owner_localparts: true,
        pubsub_owner_affiliation: true,
    };
    // No IdP proof at all -> rejected even though both cell admin flags are true.
    assert!(matches!(
        authorize_admin_policy_mutation(AuthProvenance::ExternalIdp, None, &cell_flags),
        Err(AuthError::AdminProofRequired)
    ));

    // Non-admin IdP proof -> rejected even though cell claims serverOwnerLocalparts & pubsub Owner.
    let non_admin_intent = waddle_broker::VerifiedIdpIntent {
        account_id: AccountId::new_v4(),
        tenant_id: tenant("alpha.waddle.social"),
        device_jkt: None,
        is_tenant_admin: false,
        proof_id: "jti-1".to_owned(),
    };
    assert!(matches!(
        authorize_admin_policy_mutation(
            AuthProvenance::ExternalIdp,
            Some(&non_admin_intent),
            &cell_flags
        ),
        Err(AuthError::AdminProofRequired)
    ));

    // Broker-verified IdP admin proof -> allowed even with cell flags false.
    let admin_intent = waddle_broker::VerifiedIdpIntent {
        is_tenant_admin: true,
        ..non_admin_intent
    };
    assert!(authorize_admin_policy_mutation(
        AuthProvenance::ExternalIdp,
        Some(&admin_intent),
        &CellAssertedAdminFlags::default()
    )
    .is_ok());
}

#[test]
fn broker_subkey_pinning_rotation_and_anti_rollback() {
    let root_current = RootBkskSigner::generate().expect("root1");
    let root_next = RootBkskSigner::generate().expect("root2");
    let attacker_root = RootBkskSigner::generate().expect("attacker root");

    let pins = RootBkskPinSet::new(&[root_current.public_key_sec1(), root_next.public_key_sec1()])
        .expect("pin set");

    let ring = BrokerSubkeyRing::new("waddle.social");
    let now_ms = 1_760_000_000_000i64;
    let b1 = ring
        .rotate_subkey(&root_current, 1, now_ms - 1_000, now_ms + 86_400_000)
        .expect("subkey 1");
    assert!(pins
        .verify_subkey_bundle(&b1, "waddle.social", now_ms)
        .is_ok());

    // Rotate to epoch 2 signed by next root (zero-downtime rollover).
    let b2 = ring
        .rotate_subkey(&root_next, 2, now_ms - 1_000, now_ms + 86_400_000)
        .expect("subkey 2");
    assert!(pins
        .verify_subkey_bundle(&b2, "waddle.social", now_ms)
        .is_ok());

    // Anti-rollback: replaying epoch 1 after seeing epoch 2 must fail.
    assert!(matches!(
        pins.verify_subkey_bundle(&b1, "waddle.social", now_ms),
        Err(SealError::SubkeyEpochRollback { seen: 2, got: 1 })
    ));

    // Wrong-root-signed subkey must fail.
    let forged = ring
        .rotate_subkey(&attacker_root, 3, now_ms - 1_000, now_ms + 86_400_000)
        .expect("forged");
    assert!(matches!(
        pins.verify_subkey_bundle(&forged, "waddle.social", now_ms),
        Err(SealError::UnknownRootKid(_))
    ));

    // Tampered signature under known root_kid must fail.
    let mut tampered = b2.clone();
    tampered.root_signature[0] ^= 0xFF;
    assert!(matches!(
        pins.verify_subkey_bundle(&tampered, "waddle.social", now_ms),
        Err(SealError::InvalidRootSignature)
    ));
}

#[test]
fn sealed_envelope_client_to_broker_only_decryption() {
    let root = RootBkskSigner::generate().expect("root");
    let pins = RootBkskPinSet::new(&[root.public_key_sec1()]).expect("pins");
    let ring = BrokerSubkeyRing::new("waddle.social");

    let now_ms = 1_760_000_000_000i64;
    let bundle = ring
        .rotate_subkey(&root, 1, now_ms - 1_000, now_ms + 86_400_000)
        .expect("subkey");

    let ctx = ClientSealContext {
        trust_domain: "waddle.social".to_owned(),
        tenant_id: tenant("alpha.waddle.social"),
        account_id: Some(AccountId::new_v4()),
        provider: "openrouter".to_owned(),
        purpose: "member-inference".to_owned(),
        nonce: "seal-nonce-1".to_owned(),
        expires_at_ms: now_ms + 60_000,
        version: 1,
    };
    let sentinel = "sk-or-v1-client-sealed-secret-0123456789";
    let secret = SecretBytes::from_slice(sentinel.as_bytes()).expect("secret");
    let blob =
        seal_credential_for_broker(&pins, &bundle, &ctx, &secret, now_ms).expect("client seal");

    // Wire blob contains zero plaintext sentinel.
    let blob_json = serde_json::to_string(&blob).expect("blob json");
    assert!(!blob_json.contains(sentinel));

    // Broker unseals cleanly with exact context.
    let unsealed = ring
        .unseal_client_envelope(&blob, &ctx, now_ms)
        .expect("broker unseal");
    assert_eq!(unsealed, secret);

    // Mismatched tenant_id / provider / nonce fails closed.
    let mut wrong_ctx = ctx.clone();
    wrong_ctx.tenant_id = tenant("other.waddle.social");
    assert!(ring
        .unseal_client_envelope(&blob, &wrong_ctx, now_ms)
        .is_err());

    // Retiring subkey prevents future unsealing.
    ring.retire_subkey(bundle.subkey_id);
    assert!(matches!(
        ring.unseal_client_envelope(&blob, &ctx, now_ms),
        Err(SealError::UnknownSubkey(_))
    ));
}

#[test]
fn jwt_boundary_rejects_expired_wrong_aud_alg_none_unknown_kid_and_expired_subkey() {
    let (jwk, enc) = make_es256_jwk_and_encoding_key("colony-k1");
    let verifier = ColonyJwtVerifier::new(
        "https://colony.waddle.social",
        "urn:waddle:broker:waddle.social",
        JwkSet { keys: vec![jwk] },
    );
    let t = tenant("alpha.waddle.social");
    let acct = AccountId::new_v4();
    let now = 1_760_000_000u64;

    let base_claims = ColonyBrokerClaims {
        iss: "https://colony.waddle.social".to_owned(),
        sub: acct.to_string(),
        aud: "urn:waddle:broker:waddle.social".to_owned(),
        exp: now + 60,
        nbf: now - 5,
        iat: now,
        jti: Uuid::now_v7().to_string(),
        tenant_id: t.as_str().to_owned(),
        challenge: "chal-b".to_owned(),
        cnf: None,
        waddle_roles: vec![],
    };

    // 1. Expired JWT.
    let mut expired_claims = base_claims.clone();
    expired_claims.iat = now - 200;
    expired_claims.nbf = now - 200;
    expired_claims.exp = now - 100;
    let expired_jwt = sign_colony_jwt(Some("colony-k1"), Algorithm::ES256, &enc, &expired_claims);
    assert!(matches!(
        verifier.verify(&expired_jwt, &t, acct, "chal-b", None, now),
        Err(AuthError::InvalidJwt(_))
    ));

    // 2. Wrong audience (e.g. cell OIDC client_id replayed to broker).
    let mut wrong_aud = base_claims.clone();
    wrong_aud.aud = "waddle-cell-client-id".to_owned();
    let wrong_aud_jwt = sign_colony_jwt(Some("colony-k1"), Algorithm::ES256, &enc, &wrong_aud);
    assert!(matches!(
        verifier.verify(&wrong_aud_jwt, &t, acct, "chal-b", None, now),
        Err(AuthError::InvalidJwt(_))
    ));

    // 3. `alg: none` unsigned token rejected.
    let alg_none_header =
        URL_SAFE_NO_PAD.encode(br#"{"alg":"none","typ":"JWT","kid":"colony-k1"}"#);
    let alg_none_payload =
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&base_claims).expect("claims json"));
    let alg_none_jwt = format!("{alg_none_header}.{alg_none_payload}.");
    assert!(verifier
        .verify(&alg_none_jwt, &t, acct, "chal-b", None, now)
        .is_err());

    // 4. Symmetric `HS256` rejected by asymmetric allowlist.
    let hs_jwt = sign_colony_jwt(
        Some("colony-k1"),
        Algorithm::HS256,
        &EncodingKey::from_secret(b"symmetric-secret"),
        &base_claims,
    );
    assert!(matches!(
        verifier.verify(&hs_jwt, &t, acct, "chal-b", None, now),
        Err(AuthError::DisallowedAlgorithm(_))
    ));

    // 5. Missing `kid` and unknown `kid` rejected (NO fallback to jwks.keys[0]).
    let no_kid_jwt = sign_colony_jwt(None, Algorithm::ES256, &enc, &base_claims);
    assert!(matches!(
        verifier.verify(&no_kid_jwt, &t, acct, "chal-b", None, now),
        Err(AuthError::MissingKid)
    ));
    let unknown_kid_jwt =
        sign_colony_jwt(Some("unknown-kid"), Algorithm::ES256, &enc, &base_claims);
    assert!(matches!(
        verifier.verify(&unknown_kid_jwt, &t, acct, "chal-b", None, now),
        Err(AuthError::UnknownKid(_))
    ));

    // 6. Single-use `jti` replay rejected.
    let valid_jwt = sign_colony_jwt(Some("colony-k1"), Algorithm::ES256, &enc, &base_claims);
    verifier
        .verify(&valid_jwt, &t, acct, "chal-b", None, now)
        .expect("first use succeeds");
    assert!(matches!(
        verifier.verify(&valid_jwt, &t, acct, "chal-b", None, now),
        Err(AuthError::JtiReplay)
    ));

    // 7. Expired broker subkey rejected.
    let root = RootBkskSigner::generate().expect("root");
    let pins = RootBkskPinSet::new(&[root.public_key_sec1()]).expect("pins");
    let ring = BrokerSubkeyRing::new("waddle.social");
    let now_ms = (now * 1000) as i64;
    let expired_subkey = ring
        .rotate_subkey(&root, 1, now_ms - 10_000, now_ms - 1)
        .expect("expired subkey");
    assert!(matches!(
        pins.verify_subkey_bundle(&expired_subkey, "waddle.social", now_ms),
        Err(SealError::SubkeyValidityWindow)
    ));
}
