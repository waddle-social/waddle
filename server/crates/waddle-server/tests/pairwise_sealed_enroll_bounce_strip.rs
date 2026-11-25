use std::sync::Arc;

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use jid::{DomainPart, FullJid, Jid};
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
use waddle_server::credential_iq::{
    bounce_iq_error_xml_with_payload, bounce_outgoing_block_frames, bounce_undeliverable_iq_reply,
    bounce_unroutable_iq_reply, CredentialIqContext, CredentialIqHandler,
};
use waddle_server::destination::UnroutableReason;
use waddle_xmpp::stream_management::StreamManagementState;
use waddle_xmpp::telemetry::attributes::SmEvictionPath;
use waddle_xmpp::waddle_credential::{
    CredentialProtocolVersion, CredentialPurpose, CredentialRequest, EnrollCredentialRequest,
    IdpProofXml, SealAlg, SealedPayloadXml, NS_WADDLE_CREDENTIAL,
};
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

const PAIRWISE_SENTINEL: &str = "sk-waddle-pairwise-sealed-sentinel-f0e1d2c3";

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

#[tokio::test]
async fn pairwise_sealed_enroll_and_all_four_bounce_strip_paths() {
    let now_s = 1_760_000_000u64;
    let now_ms = (now_s * 1000) as i64;
    let (jwk, enc) = make_jwk("pw-k1");
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
    let handler = CredentialIqHandler::new(
        custody.clone(),
        ring,
        verifier,
        Arc::new(DeviceKeyRegistry::new()),
    );

    let acct = AccountId::new_v4();
    let tenant_id = TenantId::parse("example.com").expect("tenant");
    let seal_ctx = ClientSealContext {
        trust_domain: "example.com".to_owned(),
        tenant_id: tenant_id.clone(),
        account_id: Some(acct),
        provider: "openrouter".to_owned(),
        purpose: "member-inference".to_owned(),
        nonce: "pw-nonce-1".to_owned(),
        expires_at_ms: now_ms + 60_000,
        version: 1,
    };
    let blob = seal_credential_for_broker(
        &pins,
        &bundle,
        &seal_ctx,
        &SecretBytes::from_slice(PAIRWISE_SENTINEL.as_bytes()).expect("secret"),
        now_ms,
    )
    .expect("seal");
    let ct_b64 = STANDARD.encode(&blob.ciphertext);

    let mut header = Header::new(Algorithm::ES256);
    header.kid = Some("pw-k1".to_owned());
    let jwt = encode(
        &header,
        &ColonyBrokerClaims {
            iss: "https://colony.example.com".to_owned(),
            sub: acct.to_string(),
            aud: "urn:waddle:broker:example.com".to_owned(),
            exp: now_s + 120,
            nbf: now_s - 5,
            iat: now_s,
            jti: Uuid::now_v7().to_string(),
            tenant_id: "example.com".to_owned(),
            challenge: "pw-chal".to_owned(),
            cnf: None,
            waddle_roles: vec![],
        },
        &enc,
    )
    .expect("jwt");

    let enroll_payload = CredentialRequest::EnrollCredential(EnrollCredentialRequest {
        version: CredentialProtocolVersion::V1,
        tenant_id: "example.com".to_owned(),
        account_id: Some(acct.to_string()),
        provider: "openrouter".to_owned(),
        purpose: CredentialPurpose::MemberInference,
        nonce: "pw-nonce-1".to_owned(),
        expires_at_ms: now_ms + 60_000,
        sealed_payload: SealedPayloadXml {
            subkey_id: blob.subkey_id.to_string(),
            alg: SealAlg::P256EcdhHkdfSha256Aes256Gcm,
            ephemeral_key_b64: STANDARD.encode(&blob.ephemeral_public_sec1),
            ciphertext_b64: ct_b64.clone(),
        },
        idp_proof: IdpProofXml::ColonyJwt { token: jwt },
        device_signature: None,
    })
    .build_element();

    let iq = Iq::Set {
        from: Some("alice@example.com/web".parse::<Jid>().expect("from")),
        to: Some("example.com".parse::<Jid>().expect("to")),
        id: "pw-enr-1".to_owned(),
        payload: enroll_payload.clone(),
    };

    // 1. Happy-path enrollment succeeds and returns opaque receipt.
    let ctx = CredentialIqContext {
        trust_domain: "example.com".to_owned(),
        provenance: AuthProvenance::ExternalIdp,
        cell_admin_flags: CellAssertedAdminFlags::default(),
        expected_challenge: "pw-chal".to_owned(),
        now_ms,
    };
    let reply = handler.handle_iq(&iq, &ctx).await;
    let reply_xml = stanza_to_xml(&Stanza::Iq(Box::new(reply)));
    assert!(!reply_xml.contains(PAIRWISE_SENTINEL));
    assert!(!reply_xml.contains(&ct_b64));

    // 2. If the same sealed IQ is misrouted through ANY of the 4 bounce paths,
    // neither the ciphertext nor the credential namespace is echoed back.
    let mut sm = StreamManagementState::new();
    sm.enable("pw-sm".to_owned(), true, Some(300));

    let b1 = stanza_to_xml(
        &bounce_undeliverable_iq_reply(&Stanza::Iq(Box::new(iq.clone()))).expect("b1"),
    );
    let sender: Jid = "alice@example.com/web".parse().expect("sender");
    let home = DomainPart::new("example.com").expect("home");
    let b2 = stanza_to_xml(&Stanza::Iq(Box::new(
        bounce_unroutable_iq_reply(&iq, &sender, &home, UnroutableReason::FederationDisabled)
            .expect("b2"),
    )));
    let sender_full: FullJid = "alice@example.com/web".parse().expect("full");
    let target: Jid = "example.com".parse().expect("target");
    let b3 = bounce_outgoing_block_frames(&iq, &sender_full, &target)
        .into_iter()
        .next()
        .expect("b3");
    let b4 = bounce_iq_error_xml_with_payload(
        "pw-b4",
        Some("example.com"),
        Some("alice@example.com/web"),
        enroll_payload,
        StanzaError::new(
            ErrorType::Cancel,
            DefinedCondition::ServiceUnavailable,
            "en",
            "unavailable",
        ),
    );

    for xml in [&b1, &b2, &b3, &b4] {
        assert!(!xml.contains(PAIRWISE_SENTINEL));
        assert!(!xml.contains(&ct_b64));
        assert!(!xml.contains(NS_WADDLE_CREDENTIAL));
        let _ = sm.record_outbound(xml.clone(), SmEvictionPath::DirectOutbound);
    }

    for entry in sm.stanzas_to_resend(0) {
        assert!(!entry.stanza_xml.contains(PAIRWISE_SENTINEL));
        assert!(!entry.stanza_xml.contains(&ct_b64));
    }
}
