//! Independent IdP intent verification (`Colony` JWT & WebAuthn),
//! registered P-256 device signing key enrollment & single-use signed command
//! verification, and explicit rejection of cell-local SCRAM (`native_users`)
//! and cell-asserted admin flags (`serverOwnerLocalparts` / `pubsub_affiliations`).

use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use p256::ecdsa::signature::Verifier;
use p256::ecdsa::{Signature, VerifyingKey};
use p256::EncodedPoint;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use uuid::Uuid;
use waddle_broker_core::identity::{AccountId, TenantId};

use crate::error::AuthError;

/// Maximum permitted clock skew for Colony JWT validation (30 seconds).
pub const MAX_JWT_CLOCK_SKEW_SECS: u64 = 30;
/// Maximum permitted TTL (`exp - iat`) for broker intent JWTs (5 minutes).
pub const MAX_JWT_TTL_SECS: u64 = 300;
/// Maximum permitted TTL for device-signed commands (2 minutes).
pub const MAX_DEVICE_COMMAND_TTL_MS: i64 = 120_000;

/// Authentication provenance reported by the cell or broker identity directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthProvenance {
    /// Authenticated via external IdP (`Colony` OIDC / WebAuthn).
    ExternalIdp,
    /// Cell-local SCRAM-SHA-256 (`native_users`) — ALWAYS ineligible for broker
    /// credential enrollment or policy mutation.
    NativeScram,
}

/// Cell-asserted admin claims (explicitly untrusted by the broker).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CellAssertedAdminFlags {
    pub in_server_owner_localparts: bool,
    pub pubsub_owner_affiliation: bool,
}

/// Independent IdP proof carried inside `urn:waddle:credential:1` IQs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum IdpProof {
    ColonyJwt {
        token: String,
    },
    WebAuthnAssertion {
        credential_id: String,
        authenticator_data: Vec<u8>,
        client_data_json: Vec<u8>,
        signature: Vec<u8>,
    },
}

/// Verified IdP intent extracted from a valid Colony JWT or WebAuthn assertion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedIdpIntent {
    pub account_id: AccountId,
    pub tenant_id: TenantId,
    pub device_jkt: Option<String>,
    pub is_tenant_admin: bool,
    pub proof_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ConfirmationClaim {
    pub jkt: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ColonyBrokerClaims {
    pub iss: String,
    pub sub: String,
    pub aud: String,
    pub exp: u64,
    pub nbf: u64,
    pub iat: u64,
    pub jti: String,
    pub tenant_id: String,
    pub challenge: String,
    #[serde(default)]
    pub cnf: Option<ConfirmationClaim>,
    #[serde(default)]
    pub waddle_roles: Vec<String>,
}

/// Broker-side Colony JWT verifier configured directly against the broker's
/// trusted IdP JWKS (never cell-supplied JWKS or cell-local session keys).
pub struct ColonyJwtVerifier {
    expected_issuer: String,
    expected_audience: String,
    jwks: JwkSet,
    consumed_jtis: RwLock<HashSet<String>>,
}

impl ColonyJwtVerifier {
    pub fn new(
        expected_issuer: impl Into<String>,
        expected_audience: impl Into<String>,
        jwks: JwkSet,
    ) -> Self {
        Self {
            expected_issuer: expected_issuer.into(),
            expected_audience: expected_audience.into(),
            jwks,
            consumed_jtis: RwLock::new(HashSet::new()),
        }
    }

    /// Verifies a Colony JWT with strict `kid` matching (NO fallback to
    /// `keys[0]`), asymmetric `alg` allowlist (`ES256`/`EdDSA`/`RS256`),
    /// dedicated broker `aud`, tight `exp`/`nbf`/TTL, single-use `jti`,
    /// `challenge`, and optional `cnf.jkt` binding.
    pub fn verify(
        &self,
        token: &str,
        expected_tenant: &TenantId,
        expected_account: AccountId,
        expected_challenge: &str,
        expected_device_jkt: Option<&str>,
        now_secs: u64,
    ) -> Result<VerifiedIdpIntent, AuthError> {
        let header = decode_header(token).map_err(|err| AuthError::InvalidJwt(err.to_string()))?;

        // Strict asymmetric algorithm allowlist (rejects `none`, `HS256`, etc.).
        if !matches!(
            header.alg,
            Algorithm::ES256 | Algorithm::EdDSA | Algorithm::RS256
        ) {
            return Err(AuthError::DisallowedAlgorithm(format!("{:?}", header.alg)));
        }

        // Strict `kid` match: NEVER fall back to `jwks.keys[0]`.
        let kid = header.kid.ok_or(AuthError::MissingKid)?;
        let jwk = self
            .jwks
            .find(&kid)
            .ok_or_else(|| AuthError::UnknownKid(kid.clone()))?;
        let decoding_key =
            DecodingKey::from_jwk(jwk).map_err(|err| AuthError::InvalidJwt(err.to_string()))?;

        let mut validation = Validation::new(header.alg);
        validation.leeway = MAX_JWT_CLOCK_SKEW_SECS;
        validation.validate_exp = false;
        validation.validate_nbf = false;
        validation.set_issuer(&[&self.expected_issuer]);
        validation.set_audience(&[&self.expected_audience]);

        let token_data = decode::<ColonyBrokerClaims>(token, &decoding_key, &validation)
            .map_err(|err| AuthError::InvalidJwt(err.to_string()))?;
        let claims = token_data.claims;

        // Deterministic time-window & tight TTL enforcement.
        if now_secs > claims.exp.saturating_add(MAX_JWT_CLOCK_SKEW_SECS) {
            return Err(AuthError::InvalidJwt("token expired".to_owned()));
        }
        if now_secs.saturating_add(MAX_JWT_CLOCK_SKEW_SECS) < claims.nbf {
            return Err(AuthError::InvalidJwt("token not yet valid".to_owned()));
        }
        if claims.exp.saturating_sub(claims.iat) > MAX_JWT_TTL_SECS {
            return Err(AuthError::InvalidJwt(
                "token TTL exceeds maximum".to_owned(),
            ));
        }
        if claims.tenant_id != expected_tenant.as_str() {
            return Err(AuthError::InvalidJwt("tenant_id claim mismatch".to_owned()));
        }
        if claims.sub != expected_account.to_string() {
            return Err(AuthError::InvalidJwt(
                "sub claim does not match account_id".to_owned(),
            ));
        }
        if !bool::from(
            claims
                .challenge
                .as_bytes()
                .ct_eq(expected_challenge.as_bytes()),
        ) {
            return Err(AuthError::ChallengeMismatch);
        }

        let device_jkt = claims.cnf.as_ref().map(|c| c.jkt.clone());
        if let Some(expected_jkt) = expected_device_jkt {
            match device_jkt.as_deref() {
                Some(actual) if bool::from(actual.as_bytes().ct_eq(expected_jkt.as_bytes())) => {}
                _ => return Err(AuthError::ConfirmationKeyMismatch),
            }
        }

        if claims.jti.trim().is_empty() {
            return Err(AuthError::InvalidJwt("empty jti".to_owned()));
        }
        {
            let mut guard = self
                .consumed_jtis
                .write()
                .map_err(|_| AuthError::InvalidJwt("jti lock poisoned".to_owned()))?;
            if !guard.insert(claims.jti.clone()) {
                return Err(AuthError::JtiReplay);
            }
        }

        let is_tenant_admin = claims
            .waddle_roles
            .iter()
            .any(|r| r == "tenant_admin" || r == &format!("tenant_admin:{}", expected_tenant));

        Ok(VerifiedIdpIntent {
            account_id: expected_account,
            tenant_id: expected_tenant.clone(),
            device_jkt,
            is_tenant_admin,
            proof_id: claims.jti,
        })
    }
}

#[derive(Debug, Deserialize)]
struct WebAuthnClientData {
    #[serde(rename = "type")]
    kind: String,
    challenge: String,
    origin: String,
}

/// Registered IdP-attested WebAuthn P-256 credential record.
#[derive(Debug, Clone)]
pub struct RegisteredWebAuthnKey {
    pub account_id: AccountId,
    pub tenant_id: TenantId,
    pub public_key_sec1: Vec<u8>,
    pub sign_count: u32,
    pub is_tenant_admin: bool,
}

/// Broker-side WebAuthn assertion verifier (`p256::ecdsa` + `sha2`).
pub struct WebAuthnVerifier {
    rp_id: String,
    allowed_origins: HashSet<String>,
    credentials: RwLock<HashMap<String, RegisteredWebAuthnKey>>,
}

impl WebAuthnVerifier {
    pub fn new(
        rp_id: impl Into<String>,
        allowed_origins: impl IntoIterator<Item = String>,
    ) -> Self {
        Self {
            rp_id: rp_id.into(),
            allowed_origins: allowed_origins.into_iter().collect(),
            credentials: RwLock::new(HashMap::new()),
        }
    }

    pub fn register_credential(&self, credential_id: String, key: RegisteredWebAuthnKey) {
        if let Ok(mut guard) = self.credentials.write() {
            guard.insert(credential_id, key);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn verify_assertion(
        &self,
        credential_id: &str,
        authenticator_data: &[u8],
        client_data_json: &[u8],
        signature_bytes: &[u8],
        expected_tenant: &TenantId,
        expected_account: AccountId,
        expected_challenge_b64url: &str,
    ) -> Result<VerifiedIdpIntent, AuthError> {
        let client_data: WebAuthnClientData = serde_json::from_slice(client_data_json)
            .map_err(|err| AuthError::InvalidWebAuthn(format!("clientDataJSON: {err}")))?;
        if client_data.kind != "webauthn.get" {
            return Err(AuthError::InvalidWebAuthn(
                "clientData.type must be webauthn.get".to_owned(),
            ));
        }
        if !self.allowed_origins.contains(&client_data.origin) {
            return Err(AuthError::InvalidWebAuthn(format!(
                "origin not allowlisted: {}",
                client_data.origin
            )));
        }
        if !bool::from(
            client_data
                .challenge
                .as_bytes()
                .ct_eq(expected_challenge_b64url.as_bytes()),
        ) {
            return Err(AuthError::ChallengeMismatch);
        }

        // authenticatorData must be at least 37 bytes: 32B rpIdHash + 1B flags + 4B signCount.
        if authenticator_data.len() < 37 {
            return Err(AuthError::InvalidWebAuthn(
                "authenticatorData shorter than 37 bytes".to_owned(),
            ));
        }
        let expected_rp_hash: [u8; 32] = Sha256::digest(self.rp_id.as_bytes()).into();
        if !bool::from(authenticator_data[..32].ct_eq(&expected_rp_hash)) {
            return Err(AuthError::InvalidWebAuthn("rpIdHash mismatch".to_owned()));
        }
        let flags = authenticator_data[32];
        // Require UP (0x01) and UV (0x04).
        if (flags & 0x05) != 0x05 {
            return Err(AuthError::InvalidWebAuthn(
                "UP and UV flags are required".to_owned(),
            ));
        }
        let sign_count = u32::from_be_bytes([
            authenticator_data[33],
            authenticator_data[34],
            authenticator_data[35],
            authenticator_data[36],
        ]);

        let mut guard = self
            .credentials
            .write()
            .map_err(|_| AuthError::InvalidWebAuthn("credentials lock poisoned".to_owned()))?;
        let cred = guard.get_mut(credential_id).ok_or_else(|| {
            AuthError::InvalidWebAuthn(format!("unknown WebAuthn credential {credential_id}"))
        })?;
        if cred.tenant_id != *expected_tenant || cred.account_id != expected_account {
            return Err(AuthError::InvalidWebAuthn(
                "credential tenant/account mismatch".to_owned(),
            ));
        }
        if sign_count != 0 && sign_count <= cred.sign_count {
            return Err(AuthError::SignCountRollback);
        }

        let verifying_key = VerifyingKey::from_sec1_bytes(&cred.public_key_sec1)
            .map_err(|_| AuthError::InvalidWebAuthn("invalid stored P-256 key".to_owned()))?;
        let sig = Signature::from_slice(signature_bytes)
            .or_else(|_| Signature::from_der(signature_bytes))
            .map_err(|_| {
                AuthError::InvalidWebAuthn("invalid ECDSA signature encoding".to_owned())
            })?;

        let client_data_hash = Sha256::digest(client_data_json);
        let mut signed_msg = Vec::with_capacity(authenticator_data.len() + 32);
        signed_msg.extend_from_slice(authenticator_data);
        signed_msg.extend_from_slice(&client_data_hash);

        verifying_key
            .verify(&signed_msg, &sig)
            .map_err(|_| AuthError::InvalidWebAuthn("ECDSA verification failed".to_owned()))?;

        if sign_count > cred.sign_count {
            cred.sign_count = sign_count;
        }

        Ok(VerifiedIdpIntent {
            account_id: expected_account,
            tenant_id: expected_tenant.clone(),
            device_jkt: None,
            is_tenant_admin: cred.is_tenant_admin,
            proof_id: format!("webauthn:{credential_id}:{sign_count}"),
        })
    }
}

/// Computes the canonical RFC 7638 SHA-256 JWK thumbprint (`base64url`) for an
/// uncompressed SEC1 P-256 public key.
pub fn p256_jwk_thumbprint(public_key_sec1: &[u8]) -> Result<String, AuthError> {
    let point =
        EncodedPoint::from_bytes(public_key_sec1).map_err(|_| AuthError::InvalidDeviceSignature)?;
    let x = point.x().ok_or(AuthError::InvalidDeviceSignature)?;
    let y = point.y().ok_or(AuthError::InvalidDeviceSignature)?;
    let canonical = format!(
        r#"{{"crv":"P-256","kty":"EC","x":"{}","y":"{}"}}"#,
        URL_SAFE_NO_PAD.encode(x),
        URL_SAFE_NO_PAD.encode(y)
    );
    Ok(URL_SAFE_NO_PAD.encode(Sha256::digest(canonical.as_bytes())))
}

fn encode_len_prefixed(parts: &[&[u8]]) -> Vec<u8> {
    let total: usize = parts.iter().map(|p| 4 + p.len()).sum();
    let mut out = Vec::with_capacity(total);
    for p in parts {
        let len = u32::try_from(p.len()).unwrap_or(u32::MAX);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(p);
    }
    out
}

/// Builds the canonical proof-of-possession transcript signed by a client device
/// key during `EnrollDevice`.
pub fn build_device_enroll_pop_transcript(
    trust_domain: &str,
    tenant_id: &TenantId,
    account_id: AccountId,
    device_key_id: Uuid,
    nonce: &str,
    expires_at_ms: i64,
) -> Vec<u8> {
    encode_len_prefixed(&[
        b"waddle-device-enroll-v1",
        trust_domain.as_bytes(),
        tenant_id.as_str().as_bytes(),
        account_id.as_uuid().as_bytes(),
        device_key_id.as_bytes(),
        nonce.as_bytes(),
        &expires_at_ms.to_be_bytes(),
    ])
}

/// Canonical fields of a single-use device-signed command.
#[derive(Debug, Clone)]
pub struct SignedDeviceCommand<'a> {
    pub trust_domain: &'a str,
    pub tenant_id: &'a TenantId,
    pub account_id: AccountId,
    pub device_key_id: Uuid,
    pub operation: &'a str,
    pub target_id: &'a str,
    pub provider: &'a str,
    pub nonce: &'a str,
    pub expires_at_ms: i64,
    pub payload_sha256: [u8; 32],
}

impl SignedDeviceCommand<'_> {
    pub fn canonical_transcript(&self) -> Vec<u8> {
        encode_len_prefixed(&[
            b"waddle-device-cmd-v1",
            b"1",
            self.trust_domain.as_bytes(),
            self.tenant_id.as_str().as_bytes(),
            self.account_id.as_uuid().as_bytes(),
            self.device_key_id.as_bytes(),
            self.operation.as_bytes(),
            self.target_id.as_bytes(),
            self.provider.as_bytes(),
            self.nonce.as_bytes(),
            &self.expires_at_ms.to_be_bytes(),
            &self.payload_sha256,
        ])
    }
}

#[derive(Debug, Clone)]
pub struct RegisteredDeviceKey {
    pub device_key_id: Uuid,
    pub tenant_id: TenantId,
    pub account_id: AccountId,
    pub public_key_sec1: Vec<u8>,
    pub active: bool,
}

/// Broker-side registry of IdP-enrolled client P-256 device signing keys and
/// single-use command nonces.
#[derive(Default)]
pub struct DeviceKeyRegistry {
    keys: RwLock<HashMap<Uuid, RegisteredDeviceKey>>,
    consumed_nonces: RwLock<HashSet<String>>,
}

impl DeviceKeyRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Enrolls a client P-256 device signing key after verifying:
    /// 1. Account is not `NativeScram`,
    /// 2. Broker-verified `VerifiedIdpIntent` binds `cnf.jkt == jkt(public_key_sec1)`,
    /// 3. Device private key signed the proof-of-possession transcript.
    #[allow(clippy::too_many_arguments)]
    pub fn enroll_device_key(
        &self,
        provenance: AuthProvenance,
        idp_intent: &VerifiedIdpIntent,
        trust_domain: &str,
        device_key_id: Uuid,
        public_key_sec1: Vec<u8>,
        nonce: &str,
        expires_at_ms: i64,
        now_ms: i64,
        pop_signature: &[u8],
    ) -> Result<RegisteredDeviceKey, AuthError> {
        enforce_not_scram(provenance)?;
        if expires_at_ms <= now_ms || expires_at_ms - now_ms > MAX_DEVICE_COMMAND_TTL_MS {
            return Err(AuthError::CommandExpired);
        }

        let computed_jkt = p256_jwk_thumbprint(&public_key_sec1)?;
        match idp_intent.device_jkt.as_deref() {
            Some(jkt) if bool::from(jkt.as_bytes().ct_eq(computed_jkt.as_bytes())) => {}
            _ => return Err(AuthError::ConfirmationKeyMismatch),
        }

        let verifying_key = VerifyingKey::from_sec1_bytes(&public_key_sec1)
            .map_err(|_| AuthError::InvalidDeviceSignature)?;
        let sig = Signature::from_slice(pop_signature)
            .or_else(|_| Signature::from_der(pop_signature))
            .map_err(|_| AuthError::InvalidDeviceSignature)?;
        let transcript = build_device_enroll_pop_transcript(
            trust_domain,
            &idp_intent.tenant_id,
            idp_intent.account_id,
            device_key_id,
            nonce,
            expires_at_ms,
        );
        verifying_key
            .verify(&transcript, &sig)
            .map_err(|_| AuthError::InvalidDeviceSignature)?;

        let rec = RegisteredDeviceKey {
            device_key_id,
            tenant_id: idp_intent.tenant_id.clone(),
            account_id: idp_intent.account_id,
            public_key_sec1,
            active: true,
        };
        let mut guard = self
            .keys
            .write()
            .map_err(|_| AuthError::InvalidDeviceSignature)?;
        guard.insert(device_key_id, rec.clone());
        Ok(rec)
    }

    /// Revokes a registered device key so future commands fail closed.
    pub fn revoke_device_key(&self, device_key_id: Uuid) {
        if let Ok(mut guard) = self.keys.write() {
            if let Some(key) = guard.get_mut(&device_key_id) {
                key.active = false;
            }
        }
    }

    /// Verifies a single-use device-signed command against an active enrolled key.
    pub fn verify_signed_command(
        &self,
        cmd: &SignedDeviceCommand<'_>,
        signature_bytes: &[u8],
        now_ms: i64,
    ) -> Result<(), AuthError> {
        if cmd.expires_at_ms <= now_ms || cmd.expires_at_ms - now_ms > MAX_DEVICE_COMMAND_TTL_MS {
            return Err(AuthError::CommandExpired);
        }

        let pubkey_bytes = {
            let guard = self
                .keys
                .read()
                .map_err(|_| AuthError::InvalidDeviceSignature)?;
            let rec = guard
                .get(&cmd.device_key_id)
                .ok_or(AuthError::UnknownOrInactiveDeviceKey(cmd.device_key_id))?;
            if !rec.active || rec.tenant_id != *cmd.tenant_id || rec.account_id != cmd.account_id {
                return Err(AuthError::UnknownOrInactiveDeviceKey(cmd.device_key_id));
            }
            rec.public_key_sec1.clone()
        };

        let verifying_key = VerifyingKey::from_sec1_bytes(&pubkey_bytes)
            .map_err(|_| AuthError::InvalidDeviceSignature)?;
        let sig = Signature::from_slice(signature_bytes)
            .or_else(|_| Signature::from_der(signature_bytes))
            .map_err(|_| AuthError::InvalidDeviceSignature)?;
        let transcript = cmd.canonical_transcript();
        verifying_key
            .verify(&transcript, &sig)
            .map_err(|_| AuthError::InvalidDeviceSignature)?;

        let nonce_key = format!("{}:{}:{}", cmd.tenant_id, cmd.account_id, cmd.nonce);
        let mut nonces = self
            .consumed_nonces
            .write()
            .map_err(|_| AuthError::CommandNonceReplay)?;
        if !nonces.insert(nonce_key) {
            return Err(AuthError::CommandNonceReplay);
        }
        Ok(())
    }
}

/// Explicitly rejects cell-local `native_users` SCRAM accounts for any broker
/// credential or policy operation.
pub fn enforce_not_scram(provenance: AuthProvenance) -> Result<(), AuthError> {
    match provenance {
        AuthProvenance::ExternalIdp => Ok(()),
        AuthProvenance::NativeScram => Err(AuthError::ScramAccountIneligible),
    }
}

/// Authorizes an administrative policy mutation ONLY when backed by a
/// broker-verified IdP admin proof. Cell-asserted `serverOwnerLocalparts` and
/// `pubsub_affiliations` flags are explicitly ignored and rejected when IdP
/// admin proof is absent.
pub fn authorize_admin_policy_mutation(
    provenance: AuthProvenance,
    idp_intent: Option<&VerifiedIdpIntent>,
    _untrusted_cell_flags: &CellAssertedAdminFlags,
) -> Result<(), AuthError> {
    enforce_not_scram(provenance)?;
    let intent = idp_intent.ok_or(AuthError::AdminProofRequired)?;
    if !intent.is_tenant_admin {
        return Err(AuthError::AdminProofRequired);
    }
    Ok(())
}
