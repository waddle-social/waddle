use std::sync::Arc;

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use jid::Jid;
use jsonwebtoken::jwk::{
    AlgorithmParameters, CommonParameters, EllipticCurve, EllipticCurveKeyParameters, Jwk, JwkSet,
    KeyAlgorithm, PublicKeyUse,
};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::pkcs8::EncodePrivateKey;
use p256::SecretKey;
use uuid::Uuid;
use waddle_broker::{
    seal_credential_for_broker, AuthProvenance, BrokerCustodyService, BrokerSubkeyRing,
    CellAssertedAdminFlags, ClientSealContext, ColonyBrokerClaims, ColonyJwtVerifier,
    DeviceKeyRegistry, Key256, KeyHierarchyService, LocalKmsProvider, RootBkskPinSet,
    RootBkskSigner, SecretBytes, SqliteCompatLedgerStore,
};
use waddle_broker_core::identity::{AccountId, TenantId};
use waddle_server::credential_iq::stanza_to_xml;
use waddle_server::credential_iq::{CredentialIqContext, CredentialIqHandler};
use waddle_xmpp::waddle_credential::{
    CredentialProtocolVersion, CredentialPurpose, CredentialRequest, CredentialResponse,
    EnrollCredentialRequest, IdpProofXml, SealAlg, SealedPayloadXml,
};
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::DefinedCondition;

fn gen_p256() -> SecretKey {
    loop {
        let key = Key256::generate().expect("rng");
        if let Ok(sk) = SecretKey::from_slice(key.as_bytes()) {
            return sk;
        }
    }
}

fn make_jwk(kid: &str) -> (Jwk, EncodingKey) {
    let sk = gen_p256();
    let pt = sk.public_key().to_encoded_point(false);
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
            x: URL_SAFE_NO_PAD.encode(pt.x().expect("x")),
            y: URL_SAFE_NO_PAD.encode(pt.y().expect("y")),
        }),
    };
    let pem = sk.to_pkcs8_pem(p256::pkcs8::LineEnding::LF).expect("pem");
    (jwk, EncodingKey::from_ec_pem(pem.as_bytes()).expect("enc"))
}

fn fresh_jwt(enc: &EncodingKey, acct: AccountId, now_s: u64) -> String {
    let mut h = Header::new(Algorithm::ES256);
    h.kid = Some("k1".to_owned());
    let claims = ColonyBrokerClaims {
        iss: "https://colony.example.com".to_owned(),
        sub: acct.to_string(),
        aud: "urn:waddle:broker:example.com".to_owned(),
        exp: now_s + 120,
        nbf: now_s - 5,
        iat: now_s,
        jti: Uuid::now_v7().to_string(),
        tenant_id: "example.com".to_owned(),
        challenge: "nonce-chal".to_owned(),
        cnf: None,
        waddle_roles: vec![],
    };
    encode(&h, &claims, enc).expect("jwt")
}

#[tokio::test]
async fn duplicate_nonce_is_idempotent_while_mutated_ciphertext_fails_closed_with_conflict() {
    let now_s = 1_760_000_000u64;
    let now_ms = (now_s * 1000) as i64;
    let (jwk, enc) = make_jwk("k1");
    let root = RootBkskSigner::generate().expect("root");
    let pins = RootBkskPinSet::new(&[root.public_key_sec1()]).expect("pins");

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
    let ring = Arc::new(BrokerSubkeyRing::new("example.com"));
    let bundle = ring
        .rotate_subkey(&root, 1, now_ms - 1_000, now_ms + 86_400_000)
        .expect("subkey");
    let verifier = Arc::new(ColonyJwtVerifier::new(
        "https://colony.example.com",
        "urn:waddle:broker:example.com",
        JwkSet { keys: vec![jwk] },
    ));
    let handler =
        CredentialIqHandler::new(custody, ring, verifier, Arc::new(DeviceKeyRegistry::new()));

    let acct = AccountId::new_v4();
    let tenant_id = TenantId::parse("example.com").expect("tenant");
    let seal_ctx = ClientSealContext {
        trust_domain: "example.com".to_owned(),
        tenant_id,
        account_id: Some(acct),
        provider: "openrouter".to_owned(),
        purpose: "member-inference".to_owned(),
        nonce: "shared-nonce-42".to_owned(),
        expires_at_ms: now_ms + 60_000,
        version: 1,
    };

    let blob_a = seal_credential_for_broker(
        &pins,
        &bundle,
        &seal_ctx,
        &SecretBytes::from_slice(b"sk-secret-alpha").expect("a"),
        now_ms,
    )
    .expect("seal a");
    let blob_b = seal_credential_for_broker(
        &pins,
        &bundle,
        &seal_ctx,
        &SecretBytes::from_slice(b"sk-secret-beta-mutated").expect("b"),
        now_ms,
    )
    .expect("seal b");

    let sealed_a = SealedPayloadXml {
        subkey_id: blob_a.subkey_id.to_string(),
        alg: SealAlg::P256EcdhHkdfSha256Aes256Gcm,
        ephemeral_key_b64: STANDARD.encode(&blob_a.ephemeral_public_sec1),
        ciphertext_b64: STANDARD.encode(&blob_a.ciphertext),
    };
    let sealed_b = SealedPayloadXml {
        subkey_id: blob_b.subkey_id.to_string(),
        alg: SealAlg::P256EcdhHkdfSha256Aes256Gcm,
        ephemeral_key_b64: STANDARD.encode(&blob_b.ephemeral_public_sec1),
        ciphertext_b64: STANDARD.encode(&blob_b.ciphertext),
    };

    let ctx = CredentialIqContext {
        trust_domain: "example.com".to_owned(),
        provenance: AuthProvenance::ExternalIdp,
        cell_admin_flags: CellAssertedAdminFlags::default(),
        expected_challenge: "nonce-chal".to_owned(),
        now_ms,
    };

    let make_iq = |id: &str, sealed: SealedPayloadXml| Iq::Set {
        from: Some("alice@example.com/web".parse::<Jid>().expect("from")),
        to: Some("example.com".parse::<Jid>().expect("to")),
        id: id.to_owned(),
        payload: CredentialRequest::EnrollCredential(EnrollCredentialRequest {
            version: CredentialProtocolVersion::V1,
            tenant_id: "example.com".to_owned(),
            account_id: Some(acct.to_string()),
            provider: "openrouter".to_owned(),
            purpose: CredentialPurpose::MemberInference,
            nonce: "shared-nonce-42".to_owned(),
            expires_at_ms: now_ms + 60_000,
            sealed_payload: sealed,
            idp_proof: IdpProofXml::ColonyJwt {
                token: fresh_jwt(&enc, acct, now_s),
            },
            device_signature: None,
        })
        .build_element(),
    };

    // First enrollment succeeds.
    let r1 = handler
        .handle_iq(&make_iq("n-1", sealed_a.clone()), &ctx)
        .await;
    let Iq::Result {
        payload: Some(el1), ..
    } = r1
    else {
        panic!("expected result");
    };
    let CredentialResponse::CredentialReceipt(rec1) =
        CredentialResponse::try_from(&el1).expect("rec1")
    else {
        panic!("expected receipt");
    };

    // Identical replay with same nonce + same ciphertext returns identical receipt.
    let r2 = handler.handle_iq(&make_iq("n-2", sealed_a), &ctx).await;
    let Iq::Result {
        payload: Some(el2), ..
    } = r2
    else {
        panic!("expected idempotent result");
    };
    let CredentialResponse::CredentialReceipt(rec2) =
        CredentialResponse::try_from(&el2).expect("rec2")
    else {
        panic!("expected receipt");
    };
    assert_eq!(rec1.credential_id, rec2.credential_id);
    assert_eq!(rec1.version, rec2.version);

    // Same nonce + mutated ciphertext MUST fail closed with <conflict/> and payload: None.
    let r3 = handler
        .handle_iq(&make_iq("n-3", sealed_b.clone()), &ctx)
        .await;
    let r3_xml = stanza_to_xml(&Stanza::Iq(Box::new(r3.clone())));
    assert!(!r3_xml.contains(&sealed_b.ciphertext_b64));
    match r3 {
        Iq::Error {
            payload: None,
            error,
            ..
        } => {
            assert_eq!(error.defined_condition, DefinedCondition::Conflict);
        }
        other => panic!("expected opaque Conflict error, got {other:?}"),
    }
}
