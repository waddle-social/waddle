//! Typed `urn:waddle:credential:1` IQ handler and public bounce-path entrypoints.
//!
//! Enforces:
//! - Strict typed parsing via `waddle_xmpp::waddle_credential` (rejects malformed
//!   XML, unknown versions, and oversized stanzas with opaque `<iq type='error'>`
//!   and `payload: None`)
//! - Independent IdP proof verification (`ColonyJwtVerifier`), device-key PoP,
//!   and explicit rejection of SCRAM (`native_users`) & cell-asserted admin flags
//! - Client-sealed envelope decryption solely at the broker (`BrokerSubkeyRing`)
//! - Idempotent XEP-0198 replay nonces (same nonce + same payload digest ->
//!   identical opaque receipt; same nonce + different payload digest -> `<conflict/>`)
//! - Opaque-only IQ results and errors (`payload: None` on all errors) so
//!   XEP-0198 `sm_unacked` never persists secrets or ciphertexts.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::server::routes::websocket::stanza_to_xml as ws_stanza_to_xml;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use jid::{DomainRef, FullJid, Jid};
use minidom::Element;
use sha2::{Digest, Sha256};
use uuid::Uuid;
use waddle_broker::{
    authorize_admin_policy_mutation, enforce_not_scram, AuthProvenance, BrokerCustodyService,
    BrokerError, BrokerSubkeyRing, CellAssertedAdminFlags, ClientSealContext, ClientSealedBlob,
    ColonyJwtVerifier, CredentialLifecycleStatus, DeviceKeyRegistry, EnrollCredentialInput,
    VerifiedIdpIntent,
};
use waddle_broker_core::identity::{AccountId, TenantId};
use waddle_xmpp::waddle_credential::{
    CredentialProtocolVersion, CredentialPurpose, CredentialReceiptXml, CredentialRequest,
    CredentialResponse, CredentialStatus, CredentialStatusEntry, DeviceReceiptXml, IdpProofXml,
    PolicyReceiptXml, SealedPayloadXml, SigningAlg, SubkeyAlg, SubkeyBundleXml,
    MAX_CREDENTIAL_IQ_BYTES,
};
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

/// Per-request caller provenance and clock context for `CredentialIqHandler`.
#[derive(Debug, Clone)]
pub struct CredentialIqContext {
    pub trust_domain: String,
    pub provenance: AuthProvenance,
    pub cell_admin_flags: CellAssertedAdminFlags,
    pub expected_challenge: String,
    pub now_ms: i64,
}

/// Server-domain IQ handler for `urn:waddle:credential:1`.
pub struct CredentialIqHandler {
    custody: Arc<BrokerCustodyService>,
    subkey_ring: Arc<BrokerSubkeyRing>,
    jwt_verifier: Arc<ColonyJwtVerifier>,
    device_registry: Arc<DeviceKeyRegistry>,
    policy_generations: RwLock<HashMap<(String, String, String), u64>>,
}

impl CredentialIqHandler {
    pub fn new(
        custody: Arc<BrokerCustodyService>,
        subkey_ring: Arc<BrokerSubkeyRing>,
        jwt_verifier: Arc<ColonyJwtVerifier>,
        device_registry: Arc<DeviceKeyRegistry>,
    ) -> Self {
        Self {
            custody,
            subkey_ring,
            jwt_verifier,
            device_registry,
            policy_generations: RwLock::new(HashMap::new()),
        }
    }

    /// Parses and handles a raw XML `<iq/>` string, returning a serialized
    /// opaque `<iq type='result'|'error'>` XML string.
    ///
    /// Rejects oversized stanzas (`> MAX_CREDENTIAL_IQ_BYTES`), malformed XML,
    /// and unknown `urn:waddle:credential:<ver>` versions with an opaque error
    /// (`payload: None`) containing zero echoed secrets.
    pub async fn handle_raw_iq_xml(&self, raw_xml: &str, ctx: &CredentialIqContext) -> String {
        if raw_xml.len() > MAX_CREDENTIAL_IQ_BYTES {
            let err_iq = opaque_error(
                None,
                None,
                "oversized",
                ErrorType::Modify,
                DefinedCondition::PolicyViolation,
                "credential IQ exceeds maximum permitted size",
            );
            return ws_stanza_to_xml(&Stanza::Iq(Box::new(err_iq)));
        }

        let elem: Element = match raw_xml.parse() {
            Ok(el) => el,
            Err(_) => {
                let err_iq = opaque_error(
                    None,
                    None,
                    "malformed",
                    ErrorType::Modify,
                    DefinedCondition::BadRequest,
                    "malformed credential IQ XML",
                );
                return ws_stanza_to_xml(&Stanza::Iq(Box::new(err_iq)));
            }
        };

        let iq = match Iq::try_from(elem) {
            Ok(iq) => iq,
            Err(_) => {
                let err_iq = opaque_error(
                    None,
                    None,
                    "malformed",
                    ErrorType::Modify,
                    DefinedCondition::BadRequest,
                    "invalid IQ stanza structure",
                );
                return ws_stanza_to_xml(&Stanza::Iq(Box::new(err_iq)));
            }
        };

        let reply = self.handle_iq(&iq, ctx).await;
        ws_stanza_to_xml(&Stanza::Iq(Box::new(reply)))
    }

    /// Handles a parsed [`Iq`] stanza carrying a `urn:waddle:credential:1` payload.
    pub async fn handle_iq(&self, iq: &Iq, ctx: &CredentialIqContext) -> Iq {
        let (from, to, id, payload) = match iq {
            Iq::Get {
                from,
                to,
                id,
                payload,
                ..
            }
            | Iq::Set {
                from,
                to,
                id,
                payload,
                ..
            } => (from.clone(), to.clone(), id.clone(), payload),
            Iq::Result { .. } | Iq::Error { .. } => {
                return opaque_error(
                    iq.to().cloned(),
                    iq.from().cloned(),
                    iq.id(),
                    ErrorType::Modify,
                    DefinedCondition::BadRequest,
                    "expected IQ get or set",
                );
            }
        };

        let req = match CredentialRequest::try_from(payload) {
            Ok(r) => r,
            Err(err) => {
                return opaque_error(
                    to,
                    from,
                    &id,
                    ErrorType::Modify,
                    DefinedCondition::BadRequest,
                    &err.to_string(),
                );
            }
        };

        match self.dispatch(req, ctx).await {
            Ok(response) => Iq::Result {
                from: to,
                to: from,
                id,
                payload: Some(response.build_element()),
            },
            Err((err_type, condition, msg)) => {
                opaque_error(to, from, &id, err_type, condition, &msg)
            }
        }
    }

    async fn dispatch(
        &self,
        req: CredentialRequest,
        ctx: &CredentialIqContext,
    ) -> Result<CredentialResponse, (ErrorType, DefinedCondition, String)> {
        match req {
            CredentialRequest::GetSubkey => {
                let bundle = self.subkey_ring.active_bundle().ok_or_else(|| {
                    (
                        ErrorType::Cancel,
                        DefinedCondition::ServiceUnavailable,
                        "no active broker encryption subkey".to_owned(),
                    )
                })?;
                Ok(CredentialResponse::SubkeyBundle(SubkeyBundleXml {
                    version: CredentialProtocolVersion::V1,
                    trust_domain: bundle.trust_domain,
                    root_kid: bundle.root_kid,
                    subkey_id: bundle.subkey_id.to_string(),
                    alg: SubkeyAlg::P256Ecdh,
                    key_epoch: bundle.key_epoch,
                    not_before_ms: bundle.not_before_ms,
                    not_after_ms: bundle.not_after_ms,
                    public_key_b64: STANDARD.encode(&bundle.subkey_public_sec1),
                    root_sig_alg: SigningAlg::P256Ecdsa,
                    root_signature_b64: STANDARD.encode(&bundle.root_signature),
                }))
            }
            CredentialRequest::EnrollDevice(r) => {
                enforce_not_scram(ctx.provenance).map_err(auth_err)?;
                let tenant_id = parse_tenant(&r.tenant_id)?;
                let account_id = parse_account(&r.account_id)?;
                let device_key_id = parse_uuid(&r.device_key_id)?;
                let pubkey_sec1 = decode_b64(&r.device_public_key_b64)?;
                let pop_sig = decode_b64(&r.pop_signature_b64)?;
                let jkt = waddle_broker::p256_jwk_thumbprint(&pubkey_sec1).map_err(auth_err)?;
                let intent = self.verify_idp_proof(
                    &r.idp_proof,
                    &tenant_id,
                    account_id,
                    &ctx.expected_challenge,
                    Some(&jkt),
                    ctx.now_ms,
                )?;
                let enrolled = self
                    .device_registry
                    .enroll_device_key(
                        ctx.provenance,
                        &intent,
                        &ctx.trust_domain,
                        device_key_id,
                        pubkey_sec1,
                        &r.nonce,
                        r.expires_at_ms,
                        ctx.now_ms,
                        &pop_sig,
                    )
                    .map_err(auth_err)?;
                Ok(CredentialResponse::DeviceReceipt(DeviceReceiptXml {
                    device_key_id: enrolled.device_key_id.to_string(),
                    status: "active".to_owned(),
                }))
            }
            CredentialRequest::EnrollCredential(r) => {
                enforce_not_scram(ctx.provenance).map_err(auth_err)?;
                let tenant_id = parse_tenant(&r.tenant_id)?;
                let account_id = r.account_id.as_deref().map(parse_account).transpose()?;
                let auth_account = account_id.unwrap_or_else(AccountId::new_v4);
                let _intent = self.verify_idp_proof(
                    &r.idp_proof,
                    &tenant_id,
                    auth_account,
                    &ctx.expected_challenge,
                    None,
                    ctx.now_ms,
                )?;
                let payload_digest =
                    compute_sealed_payload_digest(&r.provider, r.purpose, &r.sealed_payload);
                let blob = decode_sealed_blob(&r.sealed_payload)?;
                let seal_ctx = ClientSealContext {
                    trust_domain: ctx.trust_domain.clone(),
                    tenant_id: tenant_id.clone(),
                    account_id,
                    provider: r.provider.clone(),
                    purpose: r.purpose.as_str().to_owned(),
                    nonce: r.nonce.clone(),
                    expires_at_ms: r.expires_at_ms,
                    version: 1,
                };
                let plaintext = self
                    .subkey_ring
                    .unseal_client_envelope(&blob, &seal_ctx, ctx.now_ms)
                    .map_err(|err| {
                        (
                            ErrorType::Modify,
                            DefinedCondition::BadRequest,
                            err.to_string(),
                        )
                    })?;
                let receipt = self
                    .custody
                    .enroll(EnrollCredentialInput {
                        tenant_id,
                        account_id,
                        provider: r.provider,
                        nonce: r.nonce,
                        payload_digest_sha256: payload_digest,
                        expires_at_ms: r.expires_at_ms,
                        plaintext,
                    })
                    .await
                    .map_err(broker_err)?;
                Ok(CredentialResponse::CredentialReceipt(
                    CredentialReceiptXml {
                        credential_id: receipt.credential_id.to_string(),
                        version: receipt.version,
                        status: map_status(receipt.status),
                    },
                ))
            }
            CredentialRequest::Status(q) => {
                let tenant_id = parse_tenant(&q.tenant_id)?;
                let records = self
                    .custody
                    .ledger()
                    .list_envelopes_for_tenant(&tenant_id)
                    .await
                    .map_err(|err| {
                        (
                            ErrorType::Cancel,
                            DefinedCondition::InternalServerError,
                            err.to_string(),
                        )
                    })?;
                let mut entries = Vec::new();
                for rec in records {
                    if let Some(ref p) = q.provider {
                        if rec.provider != *p {
                            continue;
                        }
                    }
                    let purpose = if rec.account_id.is_some() {
                        CredentialPurpose::MemberInference
                    } else {
                        CredentialPurpose::TenantInference
                    };
                    if let Some(expected_purpose) = q.purpose {
                        if purpose != expected_purpose {
                            continue;
                        }
                    }
                    entries.push(CredentialStatusEntry {
                        id: rec.credential_id.to_string(),
                        provider: rec.provider,
                        purpose,
                        status: CredentialStatus::Active,
                        version: rec.version,
                        updated_at_ms: rec.created_at_ms,
                    });
                }
                Ok(CredentialResponse::StatusResult(entries))
            }
            CredentialRequest::RotateCredential(r) => {
                enforce_not_scram(ctx.provenance).map_err(auth_err)?;
                let tenant_id = parse_tenant(&r.tenant_id)?;
                let credential_id = parse_uuid(&r.credential_id)?;
                let account_id = r.account_id.as_deref().map(parse_account).transpose()?;
                let auth_account = account_id.unwrap_or_else(AccountId::new_v4);
                let _intent = self.verify_idp_proof(
                    &r.idp_proof,
                    &tenant_id,
                    auth_account,
                    &ctx.expected_challenge,
                    None,
                    ctx.now_ms,
                )?;
                let blob = decode_sealed_blob(&r.sealed_payload)?;
                let seal_ctx = ClientSealContext {
                    trust_domain: ctx.trust_domain.clone(),
                    tenant_id: tenant_id.clone(),
                    account_id,
                    provider: r.provider.clone(),
                    purpose: r.purpose.as_str().to_owned(),
                    nonce: r.nonce.clone(),
                    expires_at_ms: r.expires_at_ms,
                    version: 1,
                };
                let plaintext = self
                    .subkey_ring
                    .unseal_client_envelope(&blob, &seal_ctx, ctx.now_ms)
                    .map_err(|err| {
                        (
                            ErrorType::Modify,
                            DefinedCondition::BadRequest,
                            err.to_string(),
                        )
                    })?;
                let receipt = self
                    .custody
                    .rotate(&tenant_id, credential_id, &plaintext)
                    .await
                    .map_err(broker_err)?;
                Ok(CredentialResponse::CredentialReceipt(
                    CredentialReceiptXml {
                        credential_id: receipt.credential_id.to_string(),
                        version: receipt.version,
                        status: map_status(receipt.status),
                    },
                ))
            }
            CredentialRequest::RevokeCredential(r) => {
                enforce_not_scram(ctx.provenance).map_err(auth_err)?;
                let tenant_id = parse_tenant(&r.tenant_id)?;
                let credential_id = parse_uuid(&r.credential_id)?;
                let account_id = r.account_id.as_deref().map(parse_account).transpose()?;
                if let Some(ref idp) = r.idp_proof {
                    let auth_account = account_id.unwrap_or_else(AccountId::new_v4);
                    let _ = self.verify_idp_proof(
                        idp,
                        &tenant_id,
                        auth_account,
                        &ctx.expected_challenge,
                        None,
                        ctx.now_ms,
                    )?;
                }
                let receipt = self
                    .custody
                    .revoke_and_destroy(&tenant_id, credential_id)
                    .await
                    .map_err(broker_err)?;
                Ok(CredentialResponse::CredentialReceipt(
                    CredentialReceiptXml {
                        credential_id: receipt.credential_id.to_string(),
                        version: receipt.version,
                        status: map_status(receipt.status),
                    },
                ))
            }
            CredentialRequest::MutatePolicy(r) => {
                enforce_not_scram(ctx.provenance).map_err(auth_err)?;
                let tenant_id = parse_tenant(&r.tenant_id)?;
                let admin_account = extract_sub_account(&r.idp_proof)?;
                let intent = self.verify_idp_proof(
                    &r.idp_proof,
                    &tenant_id,
                    admin_account,
                    &ctx.expected_challenge,
                    None,
                    ctx.now_ms,
                )?;
                authorize_admin_policy_mutation(
                    ctx.provenance,
                    Some(&intent),
                    &ctx.cell_admin_flags,
                )
                .map_err(auth_err)?;

                let key = (
                    tenant_id.as_str().to_owned(),
                    r.scope.clone(),
                    r.target_id.clone(),
                );
                let gen = {
                    let mut guard = self.policy_generations.write().map_err(|_| {
                        (
                            ErrorType::Cancel,
                            DefinedCondition::InternalServerError,
                            "policy lock poisoned".to_owned(),
                        )
                    })?;
                    let entry = guard.entry(key).or_insert(0);
                    *entry += 1;
                    *entry
                };
                Ok(CredentialResponse::PolicyReceipt(PolicyReceiptXml {
                    policy_id: format!("{}:{}:{}", r.tenant_id, r.scope, r.target_id),
                    generation: gen,
                    status: "applied".to_owned(),
                }))
            }
        }
    }

    fn verify_idp_proof(
        &self,
        proof: &IdpProofXml,
        tenant_id: &TenantId,
        account_id: AccountId,
        expected_challenge: &str,
        expected_device_jkt: Option<&str>,
        now_ms: i64,
    ) -> Result<VerifiedIdpIntent, (ErrorType, DefinedCondition, String)> {
        let now_secs = u64::try_from(now_ms / 1000).unwrap_or(0);
        match proof {
            IdpProofXml::ColonyJwt { token } => self
                .jwt_verifier
                .verify(
                    token,
                    tenant_id,
                    account_id,
                    expected_challenge,
                    expected_device_jkt,
                    now_secs,
                )
                .map_err(auth_err),
            IdpProofXml::WebAuthn { .. } => Err((
                ErrorType::Auth,
                DefinedCondition::NotAuthorized,
                "WebAuthn verifier not attached to handler".to_owned(),
            )),
        }
    }
}

fn extract_sub_account(
    proof: &IdpProofXml,
) -> Result<AccountId, (ErrorType, DefinedCondition, String)> {
    match proof {
        IdpProofXml::ColonyJwt { token } => {
            let parts: Vec<&str> = token.split('.').collect();
            if parts.len() != 3 {
                return Err((
                    ErrorType::Auth,
                    DefinedCondition::NotAuthorized,
                    "malformed JWT".to_owned(),
                ));
            }
            let payload_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(parts[1])
                .map_err(|_| {
                    (
                        ErrorType::Auth,
                        DefinedCondition::NotAuthorized,
                        "invalid JWT payload encoding".to_owned(),
                    )
                })?;
            let val: serde_json::Value = serde_json::from_slice(&payload_bytes).map_err(|_| {
                (
                    ErrorType::Auth,
                    DefinedCondition::NotAuthorized,
                    "invalid JWT payload JSON".to_owned(),
                )
            })?;
            let sub = val
                .get("sub")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    (
                        ErrorType::Auth,
                        DefinedCondition::NotAuthorized,
                        "missing sub claim".to_owned(),
                    )
                })?;
            parse_account(sub)
        }
        IdpProofXml::WebAuthn { .. } => Err((
            ErrorType::Auth,
            DefinedCondition::NotAuthorized,
            "WebAuthn proof unsupported here".to_owned(),
        )),
    }
}

fn compute_sealed_payload_digest(
    provider: &str,
    purpose: CredentialPurpose,
    sealed: &SealedPayloadXml,
) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(provider.as_bytes());
    h.update(b"|");
    h.update(purpose.as_str().as_bytes());
    h.update(b"|");
    h.update(sealed.subkey_id.as_bytes());
    h.update(b"|");
    h.update(sealed.ephemeral_key_b64.as_bytes());
    h.update(b"|");
    h.update(sealed.ciphertext_b64.as_bytes());
    h.finalize().into()
}

fn decode_sealed_blob(
    sealed: &SealedPayloadXml,
) -> Result<ClientSealedBlob, (ErrorType, DefinedCondition, String)> {
    Ok(ClientSealedBlob {
        subkey_id: parse_uuid(&sealed.subkey_id)?,
        ephemeral_public_sec1: decode_b64(&sealed.ephemeral_key_b64)?,
        ciphertext: decode_b64(&sealed.ciphertext_b64)?,
    })
}

fn decode_b64(raw: &str) -> Result<Vec<u8>, (ErrorType, DefinedCondition, String)> {
    STANDARD.decode(raw.trim()).map_err(|_| {
        (
            ErrorType::Modify,
            DefinedCondition::BadRequest,
            "invalid base64 payload".to_owned(),
        )
    })
}

fn parse_tenant(raw: &str) -> Result<TenantId, (ErrorType, DefinedCondition, String)> {
    TenantId::parse(raw).map_err(|err| {
        (
            ErrorType::Modify,
            DefinedCondition::BadRequest,
            err.to_string(),
        )
    })
}

fn parse_account(raw: &str) -> Result<AccountId, (ErrorType, DefinedCondition, String)> {
    parse_uuid(raw).map(AccountId::from_uuid)
}

fn parse_uuid(raw: &str) -> Result<Uuid, (ErrorType, DefinedCondition, String)> {
    Uuid::parse_str(raw).map_err(|_| {
        (
            ErrorType::Modify,
            DefinedCondition::BadRequest,
            "invalid UUID".to_owned(),
        )
    })
}

fn map_status(s: CredentialLifecycleStatus) -> CredentialStatus {
    match s {
        CredentialLifecycleStatus::Active => CredentialStatus::Active,
        CredentialLifecycleStatus::Revoked => CredentialStatus::Revoked,
    }
}

fn auth_err(err: waddle_broker::AuthError) -> (ErrorType, DefinedCondition, String) {
    (
        ErrorType::Cancel,
        DefinedCondition::Forbidden,
        err.to_string(),
    )
}

fn broker_err(err: BrokerError) -> (ErrorType, DefinedCondition, String) {
    match err {
        BrokerError::NoncePayloadConflict => (
            ErrorType::Cancel,
            DefinedCondition::Conflict,
            err.to_string(),
        ),
        BrokerError::CredentialNotFound(_) => (
            ErrorType::Cancel,
            DefinedCondition::ItemNotFound,
            err.to_string(),
        ),
        BrokerError::AccountRetired(_)
        | BrokerError::TenantDestroyed(_)
        | BrokerError::CredentialDestroyed(_) => (
            ErrorType::Cancel,
            DefinedCondition::Forbidden,
            err.to_string(),
        ),
        BrokerError::EgressClosedPendingRestore => (
            ErrorType::Wait,
            DefinedCondition::ServiceUnavailable,
            err.to_string(),
        ),
        other => (
            ErrorType::Cancel,
            DefinedCondition::InternalServerError,
            other.to_string(),
        ),
    }
}

fn opaque_error(
    from: Option<Jid>,
    to: Option<Jid>,
    id: &str,
    error_type: ErrorType,
    condition: DefinedCondition,
    text: &str,
) -> Iq {
    Iq::Error {
        from,
        to,
        id: id.to_owned(),
        payload: None,
        error: StanzaError::new(error_type, condition, "en", text),
    }
}

// --- Public entrypoints to the 4 production IQ error-bounce paths ---

/// Serializes a [`Stanza`] to canonical XML wire form.
pub fn stanza_to_xml(stanza: &Stanza) -> String {
    ws_stanza_to_xml(stanza)
}

/// Path 1: `route_to_connection::undeliverable_iq_reply`.
pub fn bounce_undeliverable_iq_reply(stanza: &Stanza) -> Option<Stanza> {
    crate::server::routes::interpret::undeliverable_iq_reply(stanza)
}

/// Path 2: `destination::unroutable_iq_reply`.
pub fn bounce_unroutable_iq_reply(
    iq: &Iq,
    sender: &Jid,
    home: &DomainRef,
    reason: crate::destination::UnroutableReason,
) -> Option<Iq> {
    crate::destination::unroutable_iq_reply(iq, sender, home, reason)
}

/// Path 3: `full_jid_forward::outgoing_block_frames`.
pub fn bounce_outgoing_block_frames(iq: &Iq, sender: &FullJid, target: &Jid) -> Vec<String> {
    crate::server::routes::websocket::handlers::iq::full_jid_forward::outgoing_block_frames(
        iq, sender, target,
    )
}

/// Path 4: `transport_xml::build_iq_error_xml_with_payload` (used by `muc_admin.rs`).
pub fn bounce_iq_error_xml_with_payload(
    id: &str,
    from: Option<&str>,
    to: Option<&str>,
    payload: xmpp_parsers::minidom::Element,
    error: xmpp_parsers::stanza_error::StanzaError,
) -> String {
    crate::server::routes::websocket::build_iq_error_xml_with_payload(id, from, to, payload, error)
}
