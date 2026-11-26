use std::sync::Arc;

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use jid::Jid;
use jsonwebtoken::jwk::{
    AlgorithmParameters, CommonParameters, EllipticCurve, EllipticCurveKeyParameters,
    EllipticCurveKeyType, Jwk, JwkSet, KeyAlgorithm, PublicKeyUse,
};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::pkcs8::{EncodePrivateKey, LineEnding};
use p256::SecretKey;
use uuid::Uuid;
use waddle_broker::sealed_enrollment::SignedSubkeyBundle;
use waddle_broker::{
    seal_credential_for_broker, AuthProvenance, BrokerCustodyService, BrokerSubkeyRing,
    CellAssertedAdminFlags, ClientSealContext, ColonyBrokerClaims, ColonyJwtVerifier,
    DestructionLedgerStore, DeviceKeyRegistry, Key256, KeyHierarchyService, LocalKmsProvider,
    RootBkskPinSet, RootBkskSigner, SecretBytes, SqliteCompatLedgerStore,
};
use waddle_broker_core::identity::{AccountId, TenantId};
use waddle_server::credential_iq::{CredentialIqContext, CredentialIqHandler};
use waddle_xmpp::waddle_credential::{
    CredentialProtocolVersion, CredentialPurpose, CredentialRequest, EnrollCredentialRequest,
    IdpProofXml, SealAlg, SealedPayloadXml,
};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::minidom::Element;

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
            key_type: EllipticCurveKeyType::EC,
            curve: EllipticCurve::P256,
            x: URL_SAFE_NO_PAD.encode(pt.x().expect("x")),
            y: URL_SAFE_NO_PAD.encode(pt.y().expect("y")),
        }),
    };
    let pem = sk.to_pkcs8_pem(LineEnding::LF).expect("pem");
    (jwk, EncodingKey::from_ec_pem(pem.as_bytes()).expect("enc"))
}

pub struct CredIqTestHarness {
    now_s: u64,
    now_ms: i64,
    kid: String,
    enc: EncodingKey,
    pins: RootBkskPinSet,
    bundle: SignedSubkeyBundle,
    acct: AccountId,
    pub tenant_id: TenantId,
    pub handler: CredentialIqHandler,
}

impl CredIqTestHarness {
    pub async fn new(kid: &str) -> (Self, Arc<SqliteCompatLedgerStore>) {
        let now_s = 1_760_000_000u64;
        let now_ms = (now_s * 1000) as i64;
        let (jwk, enc) = make_jwk(kid);
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
            BrokerCustodyService::new(
                hierarchy,
                Arc::clone(&ledger) as Arc<dyn DestructionLedgerStore>,
            )
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

        (
            Self {
                now_s,
                now_ms,
                kid: kid.to_owned(),
                enc,
                pins,
                bundle,
                acct,
                tenant_id,
                handler,
            },
            ledger,
        )
    }

    pub fn seal_member_payload(&self, nonce: &str, plaintext: &[u8]) -> SealedPayloadXml {
        let seal_ctx = ClientSealContext {
            trust_domain: "example.com".to_owned(),
            tenant_id: self.tenant_id.clone(),
            account_id: Some(self.acct),
            provider: "openrouter".to_owned(),
            purpose: "member-inference".to_owned(),
            nonce: nonce.to_owned(),
            expires_at_ms: self.now_ms + 60_000,
            version: 1,
        };
        let blob = seal_credential_for_broker(
            &self.pins,
            &self.bundle,
            &seal_ctx,
            &SecretBytes::from_slice(plaintext).expect("secret"),
            self.now_ms,
        )
        .expect("seal");
        SealedPayloadXml {
            subkey_id: blob.subkey_id.to_string(),
            alg: SealAlg::P256EcdhHkdfSha256Aes256Gcm,
            ephemeral_key_b64: STANDARD.encode(&blob.ephemeral_public_sec1),
            ciphertext_b64: STANDARD.encode(&blob.ciphertext),
        }
    }

    fn fresh_jwt(&self, challenge: &str) -> String {
        let mut h = Header::new(Algorithm::ES256);
        h.kid = Some(self.kid.clone());
        let claims = ColonyBrokerClaims {
            iss: "https://colony.example.com".to_owned(),
            sub: self.acct.to_string(),
            aud: "urn:waddle:broker:example.com".to_owned(),
            exp: self.now_s + 120,
            nbf: self.now_s - 5,
            iat: self.now_s,
            jti: Uuid::now_v7().to_string(),
            tenant_id: "example.com".to_owned(),
            challenge: challenge.to_owned(),
            cnf: None,
            waddle_roles: vec![],
        };
        encode(&h, &claims, &self.enc).expect("jwt")
    }

    pub fn build_enroll_element(
        &self,
        nonce: &str,
        sealed_payload: SealedPayloadXml,
        challenge: &str,
    ) -> Element {
        CredentialRequest::EnrollCredential(EnrollCredentialRequest {
            version: CredentialProtocolVersion::V1,
            tenant_id: "example.com".to_owned(),
            account_id: Some(self.acct.to_string()),
            provider: "openrouter".to_owned(),
            purpose: CredentialPurpose::MemberInference,
            nonce: nonce.to_owned(),
            expires_at_ms: self.now_ms + 60_000,
            sealed_payload,
            idp_proof: IdpProofXml::ColonyJwt {
                token: self.fresh_jwt(challenge),
            },
            device_signature: None,
        })
        .build_element()
    }

    pub fn build_enroll_iq(&self, id: &str, payload: Element) -> Iq {
        Iq::Set {
            from: Some("alice@example.com/web".parse::<Jid>().expect("from")),
            to: Some("example.com".parse::<Jid>().expect("to")),
            id: id.to_owned(),
            payload,
        }
    }

    pub fn context(&self, expected_challenge: &str) -> CredentialIqContext {
        CredentialIqContext {
            trust_domain: "example.com".to_owned(),
            caller_tenant_id: self.tenant_id.clone(),
            caller_account_id: self.acct,
            provenance: AuthProvenance::ExternalIdp,
            cell_admin_flags: CellAssertedAdminFlags::default(),
            expected_challenge: expected_challenge.to_owned(),
            now_ms: self.now_ms,
        }
    }
}
