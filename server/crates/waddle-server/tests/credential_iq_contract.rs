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
    SignedSubkeyBundle, SqliteCompatLedgerStore,
};
use waddle_broker_core::identity::{AccountId, TenantId};
use waddle_server::credential_iq::stanza_to_xml;
use waddle_server::credential_iq::{CredentialIqContext, CredentialIqHandler};
use waddle_xmpp::waddle_credential::{
    CredentialProtocolVersion, CredentialPurpose, CredentialRequest, CredentialResponse,
    CredentialStatus, CredentialStatusQuery, EnrollCredentialRequest, EnrollDeviceRequest,
    IdpProofXml, MutatePolicyRequest, RevokeCredentialRequest, RotateCredentialRequest, SealAlg,
    SealedPayloadXml, SigningAlg,
};
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::Iq;

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
