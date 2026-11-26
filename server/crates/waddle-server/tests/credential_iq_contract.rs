use std::sync::Arc;

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use jid::Jid;
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
use uuid::Uuid;
use waddle_broker::{
    build_device_enroll_pop_transcript, p256_jwk_thumbprint, seal_credential_for_broker,
    AuthProvenance, BrokerCustodyService, BrokerSubkeyRing, CellAssertedAdminFlags,
    ClientSealContext, ColonyBrokerClaims, ColonyJwtVerifier, ConfirmationClaim, DeviceKeyRegistry,
    Key256, KeyHierarchyService, LocalKmsProvider, RootBkskPinSet, RootBkskSigner, SecretBytes,
    SignedDeviceCommand, SignedSubkeyBundle, SqliteCompatLedgerStore,
};
use waddle_broker_core::identity::{AccountId, TenantId};
use waddle_server::credential_iq::{
    compute_revoke_payload_digest, stanza_to_xml, CredentialIqContext, CredentialIqHandler,
};
use waddle_xmpp::waddle_credential::{
    CredentialProtocolVersion, CredentialPurpose, CredentialRequest, CredentialResponse,
    CredentialStatus, CredentialStatusQuery, DeviceSignatureXml, EnrollCredentialRequest,
    EnrollDeviceRequest, IdpProofXml, MutatePolicyRequest, RevokeCredentialRequest,
    RotateCredentialRequest, SealAlg, SealedPayloadXml, SigningAlg,
};
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::DefinedCondition;

fn gen_p256_key() -> SecretKey {
    loop {
        let key = Key256::generate().expect("rng");
        if let Ok(sk) = SecretKey::from_slice(key.as_bytes()) {
            return sk;
        }
    }
}

fn make_jwk(kid: &str) -> (Jwk, EncodingKey) {
    let sk = gen_p256_key();
    let point = sk.public_key().to_encoded_point(false);
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
            x: URL_SAFE_NO_PAD.encode(point.x().expect("x")),
            y: URL_SAFE_NO_PAD.encode(point.y().expect("y")),
        }),
    };
    let pem = sk.to_pkcs8_pem(p256::pkcs8::LineEnding::LF).expect("pkcs8");
    let enc = EncodingKey::from_ec_pem(pem.as_bytes()).expect("enc");
    (jwk, enc)
}

fn make_jwt(
    enc: &EncodingKey,
    acct: AccountId,
    tenant: &str,
    challenge: &str,
    jkt: Option<String>,
    admin: bool,
    now_s: u64,
) -> String {
    let mut header = Header::new(Algorithm::ES256);
    header.kid = Some("colony-k1".to_owned());
    let claims = ColonyBrokerClaims {
        iss: "https://colony.example.com".to_owned(),
        sub: acct.to_string(),
        aud: "urn:waddle:broker:example.com".to_owned(),
        exp: now_s + 120,
        nbf: now_s - 5,
        iat: now_s,
        jti: Uuid::now_v7().to_string(),
        tenant_id: tenant.to_owned(),
        challenge: challenge.to_owned(),
        cnf: jkt.map(|j| ConfirmationClaim { jkt: j }),
        waddle_roles: if admin {
            vec!["tenant_admin".to_owned()]
        } else {
            vec![]
        },
    };
    encode(&header, &claims, enc).expect("sign jwt")
}

#[tokio::test]
async fn credential_iq_full_contract_subkey_device_enroll_status_rotate_revoke_policy() {
    let now_s = 1_760_000_000u64;
    let now_ms = (now_s * 1000) as i64;
    let (jwk, enc) = make_jwk("colony-k1");
    let root_signer = RootBkskSigner::generate().expect("root");
    let pins = RootBkskPinSet::new(&[root_signer.public_key_sec1()]).expect("pins");

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
    let _ = subkey_ring
        .rotate_subkey(&root_signer, 1, now_ms - 1_000, now_ms + 86_400_000)
        .expect("subkey");
    let jwt_verifier = Arc::new(ColonyJwtVerifier::new(
        "https://colony.example.com",
        "urn:waddle:broker:example.com",
        JwkSet { keys: vec![jwk] },
    ));
    let device_registry = Arc::new(DeviceKeyRegistry::new());
    let handler =
        CredentialIqHandler::new(custody.clone(), subkey_ring, jwt_verifier, device_registry);

    let from: Jid = "alice@example.com/web".parse().expect("from");
    let to: Jid = "example.com".parse().expect("to");
    let acct = AccountId::new_v4();
    let tenant_id = TenantId::parse("example.com").expect("tenant");
    let ctx = CredentialIqContext {
        trust_domain: "example.com".to_owned(),
        caller_tenant_id: tenant_id.clone(),
        caller_account_id: acct,
        provenance: AuthProvenance::ExternalIdp,
        cell_admin_flags: CellAssertedAdminFlags::default(),
        expected_challenge: "chal-contract".to_owned(),
        now_ms,
    };

    // 1. GetSubkey -> client verifies against pinned Root BKSK.
    let get_sk = Iq::Get {
        from: Some(from.clone()),
        to: Some(to.clone()),
        id: "iq-sk-1".to_owned(),
        payload: CredentialRequest::GetSubkey.build_element(),
    };
    let sk_reply = handler.handle_iq(&get_sk, &ctx).await;
    let sk_payload = match sk_reply {
        Iq::Result {
            payload: Some(p), ..
        } => p,
        other => panic!("expected result, got {other:?}"),
    };
    let CredentialResponse::SubkeyBundle(bundle_xml) =
        CredentialResponse::try_from(&sk_payload).expect("parse subkey bundle")
    else {
        panic!("expected SubkeyBundle");
    };
    let signed_bundle = SignedSubkeyBundle {
        trust_domain: bundle_xml.trust_domain,
        root_kid: bundle_xml.root_kid,
        subkey_id: Uuid::parse_str(&bundle_xml.subkey_id).expect("uuid"),
        key_epoch: bundle_xml.key_epoch,
        not_before_ms: bundle_xml.not_before_ms,
        not_after_ms: bundle_xml.not_after_ms,
        subkey_public_sec1: STANDARD.decode(&bundle_xml.public_key_b64).expect("pk"),
        root_signature: STANDARD
            .decode(&bundle_xml.root_signature_b64)
            .expect("sig"),
    };

    // 2. EnrollDevice with Colony JWT cnf.jkt + PoP signature.
    let dev_sk = gen_p256_key();
    let dev_signing = SigningKey::from(&dev_sk);
    let dev_pk_sec1 = dev_sk.public_key().to_sec1_bytes().to_vec();
    let jkt = p256_jwk_thumbprint(&dev_pk_sec1).expect("jkt");
    let dev_id = Uuid::now_v7();
    let pop_tx = build_device_enroll_pop_transcript(
        "example.com",
        &tenant_id,
        acct,
        dev_id,
        "dev-n1",
        now_ms + 60_000,
    );
    let pop_sig: Signature = dev_signing.sign(&pop_tx);
    let enroll_dev_iq = Iq::Set {
        from: Some(from.clone()),
        to: Some(to.clone()),
        id: "iq-dev-1".to_owned(),
        payload: CredentialRequest::EnrollDevice(EnrollDeviceRequest {
            version: CredentialProtocolVersion::V1,
            tenant_id: "example.com".to_owned(),
            account_id: acct.to_string(),
            device_key_id: dev_id.to_string(),
            alg: SigningAlg::P256Ecdsa,
            nonce: "dev-n1".to_owned(),
            expires_at_ms: now_ms + 60_000,
            device_public_key_b64: STANDARD.encode(&dev_pk_sec1),
            pop_signature_b64: STANDARD.encode(&pop_sig.to_bytes()[..]),
            idp_proof: IdpProofXml::ColonyJwt {
                token: make_jwt(
                    &enc,
                    acct,
                    "example.com",
                    "chal-contract",
                    Some(jkt),
                    false,
                    now_s,
                ),
            },
        })
        .build_element(),
    };
    let dev_reply = handler.handle_iq(&enroll_dev_iq, &ctx).await;
    assert!(matches!(
        dev_reply,
        Iq::Result {
            payload: Some(_),
            ..
        }
    ));

    // 3. EnrollCredential (client-sealed).
    let secret_v1 = "sk-or-v1-contract-secret-001";
    let seal_ctx = ClientSealContext {
        trust_domain: "example.com".to_owned(),
        tenant_id: tenant_id.clone(),
        account_id: Some(acct),
        provider: "openrouter".to_owned(),
        purpose: "member-inference".to_owned(),
        nonce: "cred-n1".to_owned(),
        expires_at_ms: now_ms + 60_000,
        version: 1,
    };
    let blob_v1 = seal_credential_for_broker(
        &pins,
        &signed_bundle,
        &seal_ctx,
        &SecretBytes::from_slice(secret_v1.as_bytes()).expect("s1"),
        now_ms,
    )
    .expect("seal v1");
    let enroll_cred_iq = Iq::Set {
        from: Some(from.clone()),
        to: Some(to.clone()),
        id: "iq-enr-1".to_owned(),
        payload: CredentialRequest::EnrollCredential(EnrollCredentialRequest {
            version: CredentialProtocolVersion::V1,
            tenant_id: "example.com".to_owned(),
            account_id: Some(acct.to_string()),
            provider: "openrouter".to_owned(),
            purpose: CredentialPurpose::MemberInference,
            nonce: "cred-n1".to_owned(),
            expires_at_ms: now_ms + 60_000,
            sealed_payload: SealedPayloadXml {
                subkey_id: blob_v1.subkey_id.to_string(),
                alg: SealAlg::P256EcdhHkdfSha256Aes256Gcm,
                ephemeral_key_b64: STANDARD.encode(&blob_v1.ephemeral_public_sec1),
                ciphertext_b64: STANDARD.encode(&blob_v1.ciphertext),
            },
            idp_proof: IdpProofXml::ColonyJwt {
                token: make_jwt(
                    &enc,
                    acct,
                    "example.com",
                    "chal-contract",
                    None,
                    false,
                    now_s,
                ),
            },
            device_signature: None,
        })
        .build_element(),
    };
    let enr_reply = handler.handle_iq(&enroll_cred_iq, &ctx).await;
    let enr_xml = stanza_to_xml(&Stanza::Iq(Box::new(enr_reply.clone())));
    assert!(!enr_xml.contains(secret_v1));
    let Iq::Result {
        payload: Some(enr_el),
        ..
    } = enr_reply
    else {
        panic!("expected enroll result");
    };
    let CredentialResponse::CredentialReceipt(rec) =
        CredentialResponse::try_from(&enr_el).expect("parse receipt")
    else {
        panic!("expected receipt");
    };
    assert_eq!(rec.version, 1);
    assert_eq!(rec.status, CredentialStatus::Active);

    // 4. Status query returns opaque metadata only.
    let status_iq = Iq::Get {
        from: Some(from.clone()),
        to: Some(to.clone()),
        id: "iq-st-1".to_owned(),
        payload: CredentialRequest::Status(CredentialStatusQuery {
            tenant_id: "example.com".to_owned(),
            provider: Some("openrouter".to_owned()),
            purpose: Some(CredentialPurpose::MemberInference),
        })
        .build_element(),
    };
    let st_reply = handler.handle_iq(&status_iq, &ctx).await;
    let st_xml = stanza_to_xml(&Stanza::Iq(Box::new(st_reply.clone())));
    assert!(!st_xml.contains(secret_v1));
    assert!(st_xml.contains(&rec.credential_id));

    // 5. RotateCredential -> version 2.
    let secret_v2 = "sk-or-v1-contract-secret-002";
    let seal_ctx_v2 = ClientSealContext {
        nonce: "cred-rot-1".to_owned(),
        ..seal_ctx
    };
    let blob_v2 = seal_credential_for_broker(
        &pins,
        &signed_bundle,
        &seal_ctx_v2,
        &SecretBytes::from_slice(secret_v2.as_bytes()).expect("s2"),
        now_ms,
    )
    .expect("seal v2");
    let rot_iq = Iq::Set {
        from: Some(from.clone()),
        to: Some(to.clone()),
        id: "iq-rot-1".to_owned(),
        payload: CredentialRequest::RotateCredential(RotateCredentialRequest {
            version: CredentialProtocolVersion::V1,
            credential_id: rec.credential_id.clone(),
            tenant_id: "example.com".to_owned(),
            account_id: Some(acct.to_string()),
            provider: "openrouter".to_owned(),
            purpose: CredentialPurpose::MemberInference,
            nonce: "cred-rot-1".to_owned(),
            expires_at_ms: now_ms + 60_000,
            sealed_payload: SealedPayloadXml {
                subkey_id: blob_v2.subkey_id.to_string(),
                alg: SealAlg::P256EcdhHkdfSha256Aes256Gcm,
                ephemeral_key_b64: STANDARD.encode(&blob_v2.ephemeral_public_sec1),
                ciphertext_b64: STANDARD.encode(&blob_v2.ciphertext),
            },
            idp_proof: IdpProofXml::ColonyJwt {
                token: make_jwt(
                    &enc,
                    acct,
                    "example.com",
                    "chal-contract",
                    None,
                    false,
                    now_s,
                ),
            },
            device_signature: None,
        })
        .build_element(),
    };
    let rot_reply = handler.handle_iq(&rot_iq, &ctx).await;
    let Iq::Result {
        payload: Some(rot_el),
        ..
    } = rot_reply
    else {
        panic!("expected rotate result");
    };
    let CredentialResponse::CredentialReceipt(rot_rec) =
        CredentialResponse::try_from(&rot_el).expect("parse rot")
    else {
        panic!("expected receipt");
    };
    assert_eq!(rot_rec.version, 2);

    // 6. RevokeCredential -> revoked.
    let rev_iq = Iq::Set {
        from: Some(from.clone()),
        to: Some(to.clone()),
        id: "iq-rev-1".to_owned(),
        payload: CredentialRequest::RevokeCredential(RevokeCredentialRequest {
            version: CredentialProtocolVersion::V1,
            credential_id: rec.credential_id.clone(),
            tenant_id: "example.com".to_owned(),
            account_id: Some(acct.to_string()),
            nonce: "cred-rev-1".to_owned(),
            expires_at_ms: now_ms + 60_000,
            idp_proof: Some(IdpProofXml::ColonyJwt {
                token: make_jwt(
                    &enc,
                    acct,
                    "example.com",
                    "chal-contract",
                    None,
                    false,
                    now_s,
                ),
            }),
            device_signature: None,
        })
        .build_element(),
    };
    let rev_reply = handler.handle_iq(&rev_iq, &ctx).await;
    assert!(matches!(
        rev_reply,
        Iq::Result {
            payload: Some(_),
            ..
        }
    ));

    // 7. MutatePolicy: rejects cell-asserted admin flags without IdP admin role; accepts IdP admin.
    let pol_non_admin = Iq::Set {
        from: Some(from.clone()),
        to: Some(to.clone()),
        id: "iq-pol-deny".to_owned(),
        payload: CredentialRequest::MutatePolicy(MutatePolicyRequest {
            version: CredentialProtocolVersion::V1,
            tenant_id: "example.com".to_owned(),
            scope: "tenant".to_owned(),
            target_id: "example.com".to_owned(),
            action: "allow-provider".to_owned(),
            provider: "openrouter".to_owned(),
            nonce: "pol-n1".to_owned(),
            expires_at_ms: now_ms + 60_000,
            idp_proof: IdpProofXml::ColonyJwt {
                token: make_jwt(
                    &enc,
                    acct,
                    "example.com",
                    "chal-contract",
                    None,
                    false,
                    now_s,
                ),
            },
        })
        .build_element(),
    };
    let cell_admin_ctx = CredentialIqContext {
        cell_admin_flags: CellAssertedAdminFlags {
            in_server_owner_localparts: true,
            pubsub_owner_affiliation: true,
        },
        ..ctx.clone()
    };
    assert!(matches!(
        handler.handle_iq(&pol_non_admin, &cell_admin_ctx).await,
        Iq::Error { payload: None, .. }
    ));

    let pol_admin = Iq::Set {
        from: Some(from),
        to: Some(to),
        id: "iq-pol-ok".to_owned(),
        payload: CredentialRequest::MutatePolicy(MutatePolicyRequest {
            version: CredentialProtocolVersion::V1,
            tenant_id: "example.com".to_owned(),
            scope: "tenant".to_owned(),
            target_id: "example.com".to_owned(),
            action: "allow-provider".to_owned(),
            provider: "openrouter".to_owned(),
            nonce: "pol-n2".to_owned(),
            expires_at_ms: now_ms + 60_000,
            idp_proof: IdpProofXml::ColonyJwt {
                token: make_jwt(
                    &enc,
                    acct,
                    "example.com",
                    "chal-contract",
                    None,
                    true,
                    now_s,
                ),
            },
        })
        .build_element(),
    };
    assert!(matches!(
        handler.handle_iq(&pol_admin, &ctx).await,
        Iq::Result {
            payload: Some(_),
            ..
        }
    ));

    // 8. SCRAM provenance is rejected with opaque error (payload: None).
    let scram_ctx = CredentialIqContext {
        provenance: AuthProvenance::NativeScram,
        ..ctx
    };
    assert!(matches!(
        handler.handle_iq(&enroll_cred_iq, &scram_ctx).await,
        Iq::Error { payload: None, .. }
    ));
}

struct RegressionFixture {
    handler: CredentialIqHandler,
    custody: Arc<BrokerCustodyService>,
    enc: EncodingKey,
    pins: RootBkskPinSet,
    bundle: SignedSubkeyBundle,
}

impl RegressionFixture {
    async fn new(now_ms: i64) -> Self {
        let (jwk, enc) = make_jwk("colony-k1");
        let root_signer = RootBkskSigner::generate().expect("root signer");
        let pins = RootBkskPinSet::new(&[root_signer.public_key_sec1()]).expect("pins");
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
        let bundle = subkey_ring
            .rotate_subkey(&root_signer, 1, now_ms - 1_000, now_ms + 86_400_000)
            .expect("subkey");
        let jwt_verifier = Arc::new(ColonyJwtVerifier::new(
            "https://colony.example.com",
            "urn:waddle:broker:example.com",
            JwkSet { keys: vec![jwk] },
        ));
        let device_registry = Arc::new(DeviceKeyRegistry::new());
        let handler =
            CredentialIqHandler::new(custody.clone(), subkey_ring, jwt_verifier, device_registry);
        Self {
            handler,
            custody,
            enc,
            pins,
            bundle,
        }
    }

    fn seal_for_iq(
        &self,
        tenant_id: &TenantId,
        account_id: Option<AccountId>,
        purpose: CredentialPurpose,
        nonce: &str,
        now_ms: i64,
        secret: &[u8],
    ) -> SealedPayloadXml {
        let seal_ctx = ClientSealContext {
            trust_domain: "example.com".to_owned(),
            tenant_id: tenant_id.clone(),
            account_id,
            provider: "openrouter".to_owned(),
            purpose: purpose.as_str().to_owned(),
            nonce: nonce.to_owned(),
            expires_at_ms: now_ms + 60_000,
            version: 1,
        };
        let blob = seal_credential_for_broker(
            &self.pins,
            &self.bundle,
            &seal_ctx,
            &SecretBytes::from_slice(secret).expect("secret"),
            now_ms,
        )
        .expect("seal");
        SealedPayloadXml {
            subkey_id: blob.subkey_id.to_string(),
            alg: SealAlg::P256EcdhHkdfSha256Aes256Gcm,
            ephemeral_key_b64: STANDARD.encode(&blob.ephemeral_public_sec1),
            ciphertext_b64: STANDARD.encode(&blob.ciphertext),
        }
    }
}

fn assert_iq_error_condition(reply: &Iq, expected: DefinedCondition) {
    match reply {
        Iq::Error { payload, error, .. } => {
            assert!(
                payload.is_none(),
                "credential IQ error must not echo payload"
            );
            assert_eq!(error.defined_condition, expected);
        }
        other => panic!("expected Iq::Error({expected:?}), got {other:?}"),
    }
}

#[tokio::test]
async fn revoke_requires_verified_device_signature_and_enforces_ownership_and_admin_scope() {
    let now_s = 1_760_000_000u64;
    let now_ms = (now_s * 1000) as i64;
    let fx = RegressionFixture::new(now_ms).await;
    let (handler, custody, enc) = (&fx.handler, &fx.custody, &fx.enc);

    let tenant_id = TenantId::parse("example.com").expect("tenant");
    let alice = AccountId::new_v4();
    let bob = AccountId::new_v4();
    let admin = AccountId::new_v4();
    let from: Jid = "alice@example.com/web".parse().expect("from");
    let to: Jid = "example.com".parse().expect("to");

    let alice_ctx = CredentialIqContext {
        trust_domain: "example.com".to_owned(),
        caller_tenant_id: tenant_id.clone(),
        caller_account_id: alice,
        provenance: AuthProvenance::ExternalIdp,
        cell_admin_flags: CellAssertedAdminFlags::default(),
        expected_challenge: "chal-f2".to_owned(),
        now_ms,
    };
    let bob_ctx = CredentialIqContext {
        caller_account_id: bob,
        ..alice_ctx.clone()
    };
    let admin_ctx = CredentialIqContext {
        caller_account_id: admin,
        ..alice_ctx.clone()
    };

    // Enroll Alice's device key.
    let alice_dev_sk = gen_p256_key();
    let alice_dev_signing = SigningKey::from(&alice_dev_sk);
    let alice_dev_pk = alice_dev_sk.public_key().to_sec1_bytes().to_vec();
    let alice_dev_jkt = p256_jwk_thumbprint(&alice_dev_pk).expect("jkt");
    let alice_dev_id = Uuid::now_v7();
    let pop_a = build_device_enroll_pop_transcript(
        "example.com",
        &tenant_id,
        alice,
        alice_dev_id,
        "pop-alice",
        now_ms + 60_000,
    );
    let pop_a_sig: Signature = alice_dev_signing.sign(&pop_a);
    let enroll_dev_a = Iq::Set {
        from: Some(from.clone()),
        to: Some(to.clone()),
        id: "dev-a".to_owned(),
        payload: CredentialRequest::EnrollDevice(EnrollDeviceRequest {
            version: CredentialProtocolVersion::V1,
            tenant_id: "example.com".to_owned(),
            account_id: alice.to_string(),
            device_key_id: alice_dev_id.to_string(),
            alg: SigningAlg::P256Ecdsa,
            device_public_key_b64: STANDARD.encode(&alice_dev_pk),
            nonce: "pop-alice".to_owned(),
            expires_at_ms: now_ms + 60_000,
            pop_signature_b64: STANDARD.encode(pop_a_sig.to_bytes()),
            idp_proof: IdpProofXml::ColonyJwt {
                token: make_jwt(
                    enc,
                    alice,
                    "example.com",
                    "chal-f2",
                    Some(alice_dev_jkt),
                    false,
                    now_s,
                ),
            },
        })
        .build_element(),
    };
    assert!(matches!(
        handler.handle_iq(&enroll_dev_a, &alice_ctx).await,
        Iq::Result { .. }
    ));

    // Enroll Bob's device key.
    let bob_dev_sk = gen_p256_key();
    let bob_dev_signing = SigningKey::from(&bob_dev_sk);
    let bob_dev_pk = bob_dev_sk.public_key().to_sec1_bytes().to_vec();
    let bob_dev_jkt = p256_jwk_thumbprint(&bob_dev_pk).expect("jkt");
    let bob_dev_id = Uuid::now_v7();
    let pop_b = build_device_enroll_pop_transcript(
        "example.com",
        &tenant_id,
        bob,
        bob_dev_id,
        "pop-bob",
        now_ms + 60_000,
    );
    let pop_b_sig: Signature = bob_dev_signing.sign(&pop_b);
    let enroll_dev_b = Iq::Set {
        from: Some(from.clone()),
        to: Some(to.clone()),
        id: "dev-b".to_owned(),
        payload: CredentialRequest::EnrollDevice(EnrollDeviceRequest {
            version: CredentialProtocolVersion::V1,
            tenant_id: "example.com".to_owned(),
            account_id: bob.to_string(),
            device_key_id: bob_dev_id.to_string(),
            alg: SigningAlg::P256Ecdsa,
            device_public_key_b64: STANDARD.encode(&bob_dev_pk),
            nonce: "pop-bob".to_owned(),
            expires_at_ms: now_ms + 60_000,
            pop_signature_b64: STANDARD.encode(pop_b_sig.to_bytes()),
            idp_proof: IdpProofXml::ColonyJwt {
                token: make_jwt(
                    enc,
                    bob,
                    "example.com",
                    "chal-f2",
                    Some(bob_dev_jkt),
                    false,
                    now_s,
                ),
            },
        })
        .build_element(),
    };
    assert!(matches!(
        handler.handle_iq(&enroll_dev_b, &bob_ctx).await,
        Iq::Result { .. }
    ));

    // Enroll Alice's member credential.
    let alice_sealed = fx.seal_for_iq(
        &tenant_id,
        Some(alice),
        CredentialPurpose::MemberInference,
        "enr-alice-1",
        now_ms,
        b"sk-alice-member-cred",
    );
    let enroll_alice_iq = Iq::Set {
        from: Some(from.clone()),
        to: Some(to.clone()),
        id: "enr-a1".to_owned(),
        payload: CredentialRequest::EnrollCredential(EnrollCredentialRequest {
            version: CredentialProtocolVersion::V1,
            tenant_id: "example.com".to_owned(),
            account_id: Some(alice.to_string()),
            provider: "openrouter".to_owned(),
            purpose: CredentialPurpose::MemberInference,
            nonce: "enr-alice-1".to_owned(),
            expires_at_ms: now_ms + 60_000,
            sealed_payload: alice_sealed.clone(),
            idp_proof: IdpProofXml::ColonyJwt {
                token: make_jwt(enc, alice, "example.com", "chal-f2", None, false, now_s),
            },
            device_signature: None,
        })
        .build_element(),
    };
    let alice_cred_id = match handler.handle_iq(&enroll_alice_iq, &alice_ctx).await {
        Iq::Result {
            payload: Some(p), ..
        } => match CredentialResponse::try_from(&p).expect("resp") {
            CredentialResponse::CredentialReceipt(r) => r.credential_id,
            other => panic!("unexpected {other:?}"),
        },
        other => panic!("expected result, got {other:?}"),
    };
    let alice_cred_uuid = Uuid::parse_str(&alice_cred_id).expect("uuid");

    // 1. Forged device_signature without idp_proof MUST be rejected, leaving cred active.
    let forged_revoke = Iq::Set {
        from: Some(from.clone()),
        to: Some(to.clone()),
        id: "rev-forged".to_owned(),
        payload: CredentialRequest::RevokeCredential(RevokeCredentialRequest {
            version: CredentialProtocolVersion::V1,
            tenant_id: "example.com".to_owned(),
            account_id: Some(alice.to_string()),
            credential_id: alice_cred_id.clone(),
            nonce: "rev-n-forged".to_owned(),
            expires_at_ms: now_ms + 60_000,
            idp_proof: None,
            device_signature: Some(DeviceSignatureXml {
                device_key_id: alice_dev_id.to_string(),
                alg: SigningAlg::P256Ecdsa,
                signature_b64: STANDARD.encode([0xAAu8; 64]),
            }),
        })
        .build_element(),
    };
    assert_iq_error_condition(
        &handler.handle_iq(&forged_revoke, &alice_ctx).await,
        DefinedCondition::Forbidden,
    );
    assert!(custody
        .unseal_for_egress(&tenant_id, alice_cred_uuid)
        .await
        .is_ok());

    // 2. Bob signing a revoke for Alice's credential with Bob's own key MUST be rejected.
    let bob_rev_cmd = SignedDeviceCommand {
        trust_domain: "example.com",
        tenant_id: &tenant_id,
        account_id: bob,
        device_key_id: bob_dev_id,
        operation: "revoke-credential",
        target_id: &alice_cred_id,
        provider: "",
        nonce: "rev-n-bob",
        expires_at_ms: now_ms + 60_000,
        payload_sha256: compute_revoke_payload_digest(&alice_cred_id),
    };
    let bob_rev_sig: Signature = bob_dev_signing.sign(&bob_rev_cmd.canonical_transcript());
    let bob_revoke_alice = Iq::Set {
        from: Some(from.clone()),
        to: Some(to.clone()),
        id: "rev-bob-alice".to_owned(),
        payload: CredentialRequest::RevokeCredential(RevokeCredentialRequest {
            version: CredentialProtocolVersion::V1,
            tenant_id: "example.com".to_owned(),
            account_id: Some(alice.to_string()),
            credential_id: alice_cred_id.clone(),
            nonce: "rev-n-bob".to_owned(),
            expires_at_ms: now_ms + 60_000,
            idp_proof: None,
            device_signature: Some(DeviceSignatureXml {
                device_key_id: bob_dev_id.to_string(),
                alg: SigningAlg::P256Ecdsa,
                signature_b64: STANDARD.encode(bob_rev_sig.to_bytes()),
            }),
        })
        .build_element(),
    };
    assert_iq_error_condition(
        &handler.handle_iq(&bob_revoke_alice, &bob_ctx).await,
        DefinedCondition::Forbidden,
    );
    assert!(custody
        .unseal_for_egress(&tenant_id, alice_cred_uuid)
        .await
        .is_ok());

    // 3. Valid enrolled device_signature from owner Alice succeeds (and replay fails).
    let alice_rev_cmd = SignedDeviceCommand {
        trust_domain: "example.com",
        tenant_id: &tenant_id,
        account_id: alice,
        device_key_id: alice_dev_id,
        operation: "revoke-credential",
        target_id: &alice_cred_id,
        provider: "",
        nonce: "rev-n-alice-ok",
        expires_at_ms: now_ms + 60_000,
        payload_sha256: compute_revoke_payload_digest(&alice_cred_id),
    };
    let alice_rev_sig: Signature = alice_dev_signing.sign(&alice_rev_cmd.canonical_transcript());
    let alice_revoke_iq = Iq::Set {
        from: Some(from.clone()),
        to: Some(to.clone()),
        id: "rev-alice-ok".to_owned(),
        payload: CredentialRequest::RevokeCredential(RevokeCredentialRequest {
            version: CredentialProtocolVersion::V1,
            tenant_id: "example.com".to_owned(),
            account_id: Some(alice.to_string()),
            credential_id: alice_cred_id.clone(),
            nonce: "rev-n-alice-ok".to_owned(),
            expires_at_ms: now_ms + 60_000,
            idp_proof: None,
            device_signature: Some(DeviceSignatureXml {
                device_key_id: alice_dev_id.to_string(),
                alg: SigningAlg::P256Ecdsa,
                signature_b64: STANDARD.encode(alice_rev_sig.to_bytes()),
            }),
        })
        .build_element(),
    };
    match handler.handle_iq(&alice_revoke_iq, &alice_ctx).await {
        Iq::Result {
            payload: Some(p), ..
        } => {
            let CredentialResponse::CredentialReceipt(r) =
                CredentialResponse::try_from(&p).expect("resp")
            else {
                panic!("expected receipt");
            };
            assert_eq!(r.status, CredentialStatus::Revoked);
        }
        other => panic!("expected revoked result, got {other:?}"),
    }
    // Replay of the exact same device-signed revoke MUST fail (CommandNonceReplay -> Forbidden).
    assert_iq_error_condition(
        &handler.handle_iq(&alice_revoke_iq, &alice_ctx).await,
        DefinedCondition::Forbidden,
    );

    // 4. Device-only revocation of a TenantInference credential MUST be rejected.
    let tenant_sealed = fx.seal_for_iq(
        &tenant_id,
        None,
        CredentialPurpose::TenantInference,
        "enr-tenant-1",
        now_ms,
        b"sk-tenant-shared",
    );
    let enroll_tenant_iq = Iq::Set {
        from: Some(from.clone()),
        to: Some(to.clone()),
        id: "enr-t1".to_owned(),
        payload: CredentialRequest::EnrollCredential(EnrollCredentialRequest {
            version: CredentialProtocolVersion::V1,
            tenant_id: "example.com".to_owned(),
            account_id: None,
            provider: "openrouter".to_owned(),
            purpose: CredentialPurpose::TenantInference,
            nonce: "enr-tenant-1".to_owned(),
            expires_at_ms: now_ms + 60_000,
            sealed_payload: tenant_sealed,
            idp_proof: IdpProofXml::ColonyJwt {
                token: make_jwt(enc, admin, "example.com", "chal-f2", None, true, now_s),
            },
            device_signature: None,
        })
        .build_element(),
    };
    let tenant_cred_id = match handler.handle_iq(&enroll_tenant_iq, &admin_ctx).await {
        Iq::Result {
            payload: Some(p), ..
        } => match CredentialResponse::try_from(&p).expect("resp") {
            CredentialResponse::CredentialReceipt(r) => r.credential_id,
            other => panic!("unexpected {other:?}"),
        },
        other => panic!("expected tenant enroll result, got {other:?}"),
    };
    let tenant_cred_uuid = Uuid::parse_str(&tenant_cred_id).expect("uuid");

    let dev_rev_tenant_cmd = SignedDeviceCommand {
        trust_domain: "example.com",
        tenant_id: &tenant_id,
        account_id: alice,
        device_key_id: alice_dev_id,
        operation: "revoke-credential",
        target_id: &tenant_cred_id,
        provider: "",
        nonce: "rev-n-tenant-dev",
        expires_at_ms: now_ms + 60_000,
        payload_sha256: compute_revoke_payload_digest(&tenant_cred_id),
    };
    let dev_rev_tenant_sig: Signature =
        alice_dev_signing.sign(&dev_rev_tenant_cmd.canonical_transcript());
    let dev_revoke_tenant_iq = Iq::Set {
        from: Some(from.clone()),
        to: Some(to.clone()),
        id: "rev-t-dev".to_owned(),
        payload: CredentialRequest::RevokeCredential(RevokeCredentialRequest {
            version: CredentialProtocolVersion::V1,
            tenant_id: "example.com".to_owned(),
            account_id: None,
            credential_id: tenant_cred_id.clone(),
            nonce: "rev-n-tenant-dev".to_owned(),
            expires_at_ms: now_ms + 60_000,
            idp_proof: None,
            device_signature: Some(DeviceSignatureXml {
                device_key_id: alice_dev_id.to_string(),
                alg: SigningAlg::P256Ecdsa,
                signature_b64: STANDARD.encode(dev_rev_tenant_sig.to_bytes()),
            }),
        })
        .build_element(),
    };
    assert_iq_error_condition(
        &handler.handle_iq(&dev_revoke_tenant_iq, &alice_ctx).await,
        DefinedCondition::Forbidden,
    );
    assert!(custody
        .unseal_for_egress(&tenant_id, tenant_cred_uuid)
        .await
        .is_ok());

    // 5. Forged device_signature attached to EnrollCredential MUST be rejected.
    let enroll_forged_dev = Iq::Set {
        from: Some(from),
        to: Some(to),
        id: "enr-forged-dev".to_owned(),
        payload: CredentialRequest::EnrollCredential(EnrollCredentialRequest {
            version: CredentialProtocolVersion::V1,
            tenant_id: "example.com".to_owned(),
            account_id: Some(alice.to_string()),
            provider: "openrouter".to_owned(),
            purpose: CredentialPurpose::MemberInference,
            nonce: "enr-alice-forged-dev".to_owned(),
            expires_at_ms: now_ms + 60_000,
            sealed_payload: alice_sealed,
            idp_proof: IdpProofXml::ColonyJwt {
                token: make_jwt(enc, alice, "example.com", "chal-f2", None, false, now_s),
            },
            device_signature: Some(DeviceSignatureXml {
                device_key_id: alice_dev_id.to_string(),
                alg: SigningAlg::P256Ecdsa,
                signature_b64: STANDARD.encode([0xBBu8; 64]),
            }),
        })
        .build_element(),
    };
    assert_iq_error_condition(
        &handler.handle_iq(&enroll_forged_dev, &alice_ctx).await,
        DefinedCondition::Forbidden,
    );
}

#[tokio::test]
async fn tenant_inference_enroll_and_rotate_enforce_admin_proof_and_purpose_account_consistency() {
    let now_s = 1_760_000_000u64;
    let now_ms = (now_s * 1000) as i64;
    let fx = RegressionFixture::new(now_ms).await;
    let (handler, custody, enc) = (&fx.handler, &fx.custody, &fx.enc);

    let tenant_id = TenantId::parse("example.com").expect("tenant");
    let member = AccountId::new_v4();
    let admin = AccountId::new_v4();
    let from: Jid = "admin@example.com/web".parse().expect("from");
    let to: Jid = "example.com".parse().expect("to");

    // Non-admin member even with untrusted cell_admin_flags all true.
    let member_ctx = CredentialIqContext {
        trust_domain: "example.com".to_owned(),
        caller_tenant_id: tenant_id.clone(),
        caller_account_id: member,
        provenance: AuthProvenance::ExternalIdp,
        cell_admin_flags: CellAssertedAdminFlags {
            in_server_owner_localparts: true,
            pubsub_owner_affiliation: true,
        },
        expected_challenge: "chal-f6".to_owned(),
        now_ms,
    };
    let admin_ctx = CredentialIqContext {
        caller_account_id: admin,
        cell_admin_flags: CellAssertedAdminFlags::default(),
        ..member_ctx.clone()
    };

    let tenant_sealed_v1 = fx.seal_for_iq(
        &tenant_id,
        None,
        CredentialPurpose::TenantInference,
        "f6-enr-1",
        now_ms,
        b"sk-tenant-v1",
    );

    // 1. Non-admin member attempting TenantInference enroll MUST fail (Forbidden).
    let non_admin_enroll = Iq::Set {
        from: Some(from.clone()),
        to: Some(to.clone()),
        id: "f6-non-admin-enr".to_owned(),
        payload: CredentialRequest::EnrollCredential(EnrollCredentialRequest {
            version: CredentialProtocolVersion::V1,
            tenant_id: "example.com".to_owned(),
            account_id: None,
            provider: "openrouter".to_owned(),
            purpose: CredentialPurpose::TenantInference,
            nonce: "f6-enr-1".to_owned(),
            expires_at_ms: now_ms + 60_000,
            sealed_payload: tenant_sealed_v1.clone(),
            idp_proof: IdpProofXml::ColonyJwt {
                token: make_jwt(enc, member, "example.com", "chal-f6", None, false, now_s),
            },
            device_signature: None,
        })
        .build_element(),
    };
    assert_iq_error_condition(
        &handler.handle_iq(&non_admin_enroll, &member_ctx).await,
        DefinedCondition::Forbidden,
    );

    // 2. Verified tenant admin enrolling TenantInference (account_id: None) MUST succeed!
    let admin_enroll = Iq::Set {
        from: Some(from.clone()),
        to: Some(to.clone()),
        id: "f6-admin-enr".to_owned(),
        payload: CredentialRequest::EnrollCredential(EnrollCredentialRequest {
            version: CredentialProtocolVersion::V1,
            tenant_id: "example.com".to_owned(),
            account_id: None,
            provider: "openrouter".to_owned(),
            purpose: CredentialPurpose::TenantInference,
            nonce: "f6-enr-1".to_owned(),
            expires_at_ms: now_ms + 60_000,
            sealed_payload: tenant_sealed_v1,
            idp_proof: IdpProofXml::ColonyJwt {
                token: make_jwt(enc, admin, "example.com", "chal-f6", None, true, now_s),
            },
            device_signature: None,
        })
        .build_element(),
    };
    let cred_id = match handler.handle_iq(&admin_enroll, &admin_ctx).await {
        Iq::Result {
            payload: Some(p), ..
        } => match CredentialResponse::try_from(&p).expect("resp") {
            CredentialResponse::CredentialReceipt(r) => {
                assert_eq!(r.version, 1);
                assert_eq!(r.status, CredentialStatus::Active);
                r.credential_id
            }
            other => panic!("unexpected {other:?}"),
        },
        other => panic!("expected admin TenantInference enroll result, got {other:?}"),
    };
    let cred_uuid = Uuid::parse_str(&cred_id).expect("uuid");
    assert_eq!(
        custody
            .unseal_for_egress(&tenant_id, cred_uuid)
            .await
            .expect("unseal v1")
            .as_bytes(),
        b"sk-tenant-v1"
    );

    // 3. Non-admin member attempting TenantInference rotate MUST fail (Forbidden).
    let tenant_sealed_v2 = fx.seal_for_iq(
        &tenant_id,
        None,
        CredentialPurpose::TenantInference,
        "f6-rot-2",
        now_ms,
        b"sk-tenant-v2",
    );
    let non_admin_rotate = Iq::Set {
        from: Some(from.clone()),
        to: Some(to.clone()),
        id: "f6-non-admin-rot".to_owned(),
        payload: CredentialRequest::RotateCredential(RotateCredentialRequest {
            version: CredentialProtocolVersion::V1,
            tenant_id: "example.com".to_owned(),
            account_id: None,
            credential_id: cred_id.clone(),
            provider: "openrouter".to_owned(),
            purpose: CredentialPurpose::TenantInference,
            nonce: "f6-rot-2".to_owned(),
            expires_at_ms: now_ms + 60_000,
            sealed_payload: tenant_sealed_v2.clone(),
            idp_proof: IdpProofXml::ColonyJwt {
                token: make_jwt(enc, member, "example.com", "chal-f6", None, false, now_s),
            },
            device_signature: None,
        })
        .build_element(),
    };
    assert_iq_error_condition(
        &handler.handle_iq(&non_admin_rotate, &member_ctx).await,
        DefinedCondition::Forbidden,
    );

    // 4. Verified tenant admin rotating TenantInference (account_id: None) MUST succeed!
    let admin_rotate = Iq::Set {
        from: Some(from.clone()),
        to: Some(to.clone()),
        id: "f6-admin-rot".to_owned(),
        payload: CredentialRequest::RotateCredential(RotateCredentialRequest {
            version: CredentialProtocolVersion::V1,
            tenant_id: "example.com".to_owned(),
            account_id: None,
            credential_id: cred_id.clone(),
            provider: "openrouter".to_owned(),
            purpose: CredentialPurpose::TenantInference,
            nonce: "f6-rot-2".to_owned(),
            expires_at_ms: now_ms + 60_000,
            sealed_payload: tenant_sealed_v2.clone(),
            idp_proof: IdpProofXml::ColonyJwt {
                token: make_jwt(enc, admin, "example.com", "chal-f6", None, true, now_s),
            },
            device_signature: None,
        })
        .build_element(),
    };
    match handler.handle_iq(&admin_rotate, &admin_ctx).await {
        Iq::Result {
            payload: Some(p), ..
        } => {
            let CredentialResponse::CredentialReceipt(r) =
                CredentialResponse::try_from(&p).expect("resp")
            else {
                panic!("expected receipt");
            };
            assert_eq!(r.version, 2);
        }
        other => panic!("expected admin TenantInference rotate result, got {other:?}"),
    }
    assert_eq!(
        custody
            .unseal_for_egress(&tenant_id, cred_uuid)
            .await
            .expect("unseal v2")
            .as_bytes(),
        b"sk-tenant-v2"
    );

    // 5. Mismatched purpose vs account_id MUST fail with BadRequest.
    let bad_member_no_acct = Iq::Set {
        from: Some(from.clone()),
        to: Some(to.clone()),
        id: "f6-bad-1".to_owned(),
        payload: CredentialRequest::EnrollCredential(EnrollCredentialRequest {
            version: CredentialProtocolVersion::V1,
            tenant_id: "example.com".to_owned(),
            account_id: None,
            provider: "openrouter".to_owned(),
            purpose: CredentialPurpose::MemberInference,
            nonce: "f6-bad-n1".to_owned(),
            expires_at_ms: now_ms + 60_000,
            sealed_payload: tenant_sealed_v2.clone(),
            idp_proof: IdpProofXml::ColonyJwt {
                token: make_jwt(enc, member, "example.com", "chal-f6", None, false, now_s),
            },
            device_signature: None,
        })
        .build_element(),
    };
    assert_iq_error_condition(
        &handler.handle_iq(&bad_member_no_acct, &member_ctx).await,
        DefinedCondition::BadRequest,
    );

    let bad_tenant_with_acct = Iq::Set {
        from: Some(from),
        to: Some(to),
        id: "f6-bad-2".to_owned(),
        payload: CredentialRequest::EnrollCredential(EnrollCredentialRequest {
            version: CredentialProtocolVersion::V1,
            tenant_id: "example.com".to_owned(),
            account_id: Some(admin.to_string()),
            provider: "openrouter".to_owned(),
            purpose: CredentialPurpose::TenantInference,
            nonce: "f6-bad-n2".to_owned(),
            expires_at_ms: now_ms + 60_000,
            sealed_payload: tenant_sealed_v2,
            idp_proof: IdpProofXml::ColonyJwt {
                token: make_jwt(enc, admin, "example.com", "chal-f6", None, true, now_s),
            },
            device_signature: None,
        })
        .build_element(),
    };
    assert_iq_error_condition(
        &handler.handle_iq(&bad_tenant_with_acct, &admin_ctx).await,
        DefinedCondition::BadRequest,
    );
}

#[tokio::test]
async fn iq_verb_enforcement_and_status_scram_tenant_member_isolation() {
    let now_s = 1_760_000_000u64;
    let now_ms = (now_s * 1000) as i64;
    let fx = RegressionFixture::new(now_ms).await;
    let (handler, enc) = (&fx.handler, &fx.enc);

    let tenant_id = TenantId::parse("example.com").expect("tenant");
    let other_tenant = TenantId::parse("other.example.com").expect("other tenant");
    let alice = AccountId::new_v4();
    let bob = AccountId::new_v4();
    let from: Jid = "alice@example.com/web".parse().expect("from");
    let to: Jid = "example.com".parse().expect("to");

    let alice_ctx = CredentialIqContext {
        trust_domain: "example.com".to_owned(),
        caller_tenant_id: tenant_id.clone(),
        caller_account_id: alice,
        provenance: AuthProvenance::ExternalIdp,
        cell_admin_flags: CellAssertedAdminFlags::default(),
        expected_challenge: "chal-f8".to_owned(),
        now_ms,
    };
    let bob_ctx = CredentialIqContext {
        caller_account_id: bob,
        ..alice_ctx.clone()
    };

    // 1. IQ verb mismatches: Set for GetSubkey/Status and Get for mutations MUST fail BadRequest.
    let set_get_subkey = Iq::Set {
        from: Some(from.clone()),
        to: Some(to.clone()),
        id: "verb-1".to_owned(),
        payload: CredentialRequest::GetSubkey.build_element(),
    };
    assert_iq_error_condition(
        &handler.handle_iq(&set_get_subkey, &alice_ctx).await,
        DefinedCondition::BadRequest,
    );

    let set_status = Iq::Set {
        from: Some(from.clone()),
        to: Some(to.clone()),
        id: "verb-2".to_owned(),
        payload: CredentialRequest::Status(CredentialStatusQuery {
            tenant_id: "example.com".to_owned(),
            provider: None,
            purpose: None,
        })
        .build_element(),
    };
    assert_iq_error_condition(
        &handler.handle_iq(&set_status, &alice_ctx).await,
        DefinedCondition::BadRequest,
    );

    let sealed_a = fx.seal_for_iq(
        &tenant_id,
        Some(alice),
        CredentialPurpose::MemberInference,
        "f8-enr-a",
        now_ms,
        b"sk-alice-f8",
    );
    let get_enroll = Iq::Get {
        from: Some(from.clone()),
        to: Some(to.clone()),
        id: "verb-3".to_owned(),
        payload: CredentialRequest::EnrollCredential(EnrollCredentialRequest {
            version: CredentialProtocolVersion::V1,
            tenant_id: "example.com".to_owned(),
            account_id: Some(alice.to_string()),
            provider: "openrouter".to_owned(),
            purpose: CredentialPurpose::MemberInference,
            nonce: "f8-enr-a".to_owned(),
            expires_at_ms: now_ms + 60_000,
            sealed_payload: sealed_a.clone(),
            idp_proof: IdpProofXml::ColonyJwt {
                token: make_jwt(enc, alice, "example.com", "chal-f8", None, false, now_s),
            },
            device_signature: None,
        })
        .build_element(),
    };
    assert_iq_error_condition(
        &handler.handle_iq(&get_enroll, &alice_ctx).await,
        DefinedCondition::BadRequest,
    );

    // 2. Enroll one member credential for Alice and one for Bob.
    let enroll_a = Iq::Set {
        from: Some(from.clone()),
        to: Some(to.clone()),
        id: "f8-a".to_owned(),
        payload: CredentialRequest::EnrollCredential(EnrollCredentialRequest {
            version: CredentialProtocolVersion::V1,
            tenant_id: "example.com".to_owned(),
            account_id: Some(alice.to_string()),
            provider: "openrouter".to_owned(),
            purpose: CredentialPurpose::MemberInference,
            nonce: "f8-enr-a".to_owned(),
            expires_at_ms: now_ms + 60_000,
            sealed_payload: sealed_a,
            idp_proof: IdpProofXml::ColonyJwt {
                token: make_jwt(enc, alice, "example.com", "chal-f8", None, false, now_s),
            },
            device_signature: None,
        })
        .build_element(),
    };
    let alice_cred_id = match handler.handle_iq(&enroll_a, &alice_ctx).await {
        Iq::Result {
            payload: Some(p), ..
        } => match CredentialResponse::try_from(&p).expect("resp") {
            CredentialResponse::CredentialReceipt(r) => r.credential_id,
            other => panic!("unexpected {other:?}"),
        },
        other => panic!("expected alice enroll result, got {other:?}"),
    };

    let sealed_b = fx.seal_for_iq(
        &tenant_id,
        Some(bob),
        CredentialPurpose::MemberInference,
        "f8-enr-b",
        now_ms,
        b"sk-bob-f8",
    );
    let enroll_b = Iq::Set {
        from: Some(from.clone()),
        to: Some(to.clone()),
        id: "f8-b".to_owned(),
        payload: CredentialRequest::EnrollCredential(EnrollCredentialRequest {
            version: CredentialProtocolVersion::V1,
            tenant_id: "example.com".to_owned(),
            account_id: Some(bob.to_string()),
            provider: "openrouter".to_owned(),
            purpose: CredentialPurpose::MemberInference,
            nonce: "f8-enr-b".to_owned(),
            expires_at_ms: now_ms + 60_000,
            sealed_payload: sealed_b,
            idp_proof: IdpProofXml::ColonyJwt {
                token: make_jwt(enc, bob, "example.com", "chal-f8", None, false, now_s),
            },
            device_signature: None,
        })
        .build_element(),
    };
    let bob_cred_id = match handler.handle_iq(&enroll_b, &bob_ctx).await {
        Iq::Result {
            payload: Some(p), ..
        } => match CredentialResponse::try_from(&p).expect("resp") {
            CredentialResponse::CredentialReceipt(r) => r.credential_id,
            other => panic!("unexpected {other:?}"),
        },
        other => panic!("expected bob enroll result, got {other:?}"),
    };

    let status_iq = Iq::Get {
        from: Some(from.clone()),
        to: Some(to.clone()),
        id: "f8-st".to_owned(),
        payload: CredentialRequest::Status(CredentialStatusQuery {
            tenant_id: "example.com".to_owned(),
            provider: None,
            purpose: None,
        })
        .build_element(),
    };

    // 3. SCRAM caller querying Status MUST be rejected (Forbidden).
    let scram_ctx = CredentialIqContext {
        provenance: AuthProvenance::NativeScram,
        ..alice_ctx.clone()
    };
    assert_iq_error_condition(
        &handler.handle_iq(&status_iq, &scram_ctx).await,
        DefinedCondition::Forbidden,
    );

    // 4. Cross-tenant caller querying Status MUST be rejected (Forbidden).
    let other_tenant_ctx = CredentialIqContext {
        caller_tenant_id: other_tenant,
        ..alice_ctx.clone()
    };
    assert_iq_error_condition(
        &handler.handle_iq(&status_iq, &other_tenant_ctx).await,
        DefinedCondition::Forbidden,
    );

    // 5. Bob querying Status MUST see only Bob's own member credential (NOT Alice's).
    match handler.handle_iq(&status_iq, &bob_ctx).await {
        Iq::Result {
            payload: Some(p), ..
        } => {
            let CredentialResponse::StatusResult(entries) =
                CredentialResponse::try_from(&p).expect("status resp")
            else {
                panic!("expected StatusResult");
            };
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].id, bob_cred_id);
            assert_ne!(entries[0].id, alice_cred_id);
        }
        other => panic!("expected bob status result, got {other:?}"),
    }
}
