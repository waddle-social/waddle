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
    DestructionLedgerStore, DeviceKeyRegistry, Key256, KeyHierarchyService, LocalKmsProvider,
    RootBkskPinSet, RootBkskSigner, SecretBytes, SqliteCompatLedgerStore,
};
use waddle_broker_core::identity::{AccountId, TenantId};
use waddle_server::credential_iq::stanza_to_xml;
use waddle_server::credential_iq::{CredentialIqContext, CredentialIqHandler};
use waddle_xmpp::stream_management::StreamManagementState;
use waddle_xmpp::telemetry::attributes::SmEvictionPath;
use waddle_xmpp::waddle_credential::{
    CredentialProtocolVersion, CredentialPurpose, CredentialRequest, CredentialResponse,
    EnrollCredentialRequest, IdpProofXml, SealAlg, SealedPayloadXml,
};
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::Iq;

const SENTINEL: &str = "sk-waddle-sm-replay-sentinel-77889900";

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
        challenge: "sm-chal".to_owned(),
        cnf: None,
        waddle_roles: vec![],
    };
    encode(&h, &claims, enc).expect("jwt")
}

#[tokio::test]
async fn xep0198_sm_replay_is_idempotent_with_opaque_results_and_secret_free_sm_unacked() {
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
        BrokerCustodyService::new(hierarchy, ledger.clone())
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
        tenant_id: tenant_id.clone(),
        account_id: Some(acct),
        provider: "openrouter".to_owned(),
        purpose: "member-inference".to_owned(),
        nonce: "sm-replay-nonce-1".to_owned(),
        expires_at_ms: now_ms + 60_000,
        version: 1,
    };
    let blob = seal_credential_for_broker(
        &pins,
        &bundle,
        &seal_ctx,
        &SecretBytes::from_slice(SENTINEL.as_bytes()).expect("secret"),
        now_ms,
    )
    .expect("seal");
    let sealed_xml = SealedPayloadXml {
        subkey_id: blob.subkey_id.to_string(),
        alg: SealAlg::P256EcdhHkdfSha256Aes256Gcm,
        ephemeral_key_b64: STANDARD.encode(&blob.ephemeral_public_sec1),
        ciphertext_b64: STANDARD.encode(&blob.ciphertext),
    };

    let ctx = CredentialIqContext {
        trust_domain: "example.com".to_owned(),
        provenance: AuthProvenance::ExternalIdp,
        cell_admin_flags: CellAssertedAdminFlags::default(),
        expected_challenge: "sm-chal".to_owned(),
        now_ms,
    };

    let mut sm = StreamManagementState::new();
    sm.enable("sm-stream-1".to_owned(), true, Some(300));

    let mut first_cred_id = None;
    for attempt in 0..3 {
        let iq = Iq::Set {
            from: Some("alice@example.com/web".parse::<Jid>().expect("from")),
            to: Some("example.com".parse::<Jid>().expect("to")),
            id: format!("sm-enr-{attempt}"),
            payload: CredentialRequest::EnrollCredential(EnrollCredentialRequest {
                version: CredentialProtocolVersion::V1,
                tenant_id: "example.com".to_owned(),
                account_id: Some(acct.to_string()),
                provider: "openrouter".to_owned(),
                purpose: CredentialPurpose::MemberInference,
                nonce: "sm-replay-nonce-1".to_owned(),
                expires_at_ms: now_ms + 60_000,
                sealed_payload: sealed_xml.clone(),
                idp_proof: IdpProofXml::ColonyJwt {
                    token: fresh_jwt(&enc, acct, now_s),
                },
                device_signature: None,
            })
            .build_element(),
        };
        let reply = handler.handle_iq(&iq, &ctx).await;
        let reply_xml = stanza_to_xml(&Stanza::Iq(Box::new(reply.clone())));
        let _ = sm.record_outbound(reply_xml.clone(), SmEvictionPath::DirectOutbound);

        assert!(!reply_xml.contains(SENTINEL));
        assert!(!reply_xml.contains(&sealed_xml.ciphertext_b64));
        let Iq::Result {
            payload: Some(el), ..
        } = reply
        else {
            panic!("expected opaque result on attempt {attempt}");
        };
        let CredentialResponse::CredentialReceipt(rec) =
            CredentialResponse::try_from(&el).expect("receipt")
        else {
            panic!("expected CredentialReceipt");
        };
        match &first_cred_id {
            None => first_cred_id = Some(rec.credential_id),
            Some(expected) => assert_eq!(&rec.credential_id, expected),
        }
    }

    // Only 1 credential row created across 3 XEP-0198 replays.
    let stored = ledger
        .list_envelopes_for_tenant(&tenant_id)
        .await
        .expect("list");
    assert_eq!(stored.len(), 1);

    // Every stanza in sm_unacked is opaque and secret-free.
    assert_eq!(sm.unacked_count(), 3);
    for entry in sm.stanzas_to_resend(0) {
        assert!(!entry.stanza_xml.contains(SENTINEL));
        assert!(!entry.stanza_xml.contains(&sealed_xml.ciphertext_b64));
    }
}
