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
    authorize_admin_policy_mutation, enforce_not_scram, AuthError, AuthProvenance,
    BrokerCustodyService, BrokerError, BrokerSubkeyRing, CellAssertedAdminFlags, ClientSealContext,
    ClientSealedBlob, ColonyJwtVerifier, CredentialLifecycleStatus, CredentialReceipt,
    DeviceKeyRegistry, EnrollCredentialInput, EnrollDeviceKeyInput, SecretBytes,
    SignedDeviceCommand, VerifiedIdpIntent,
};
use waddle_broker_core::identity::{AccountId, TenantId};
use waddle_xmpp::waddle_credential::{
    CredentialProtocolVersion, CredentialPurpose, CredentialReceiptXml, CredentialRequest,
    CredentialResponse, CredentialStatus, CredentialStatusEntry, CredentialStatusQuery,
    DeviceReceiptXml, DeviceSignatureXml, EnrollCredentialRequest, EnrollDeviceRequest,
    IdpProofXml, MutatePolicyRequest, PolicyReceiptXml, RevokeCredentialRequest,
    RotateCredentialRequest, SealedPayloadXml, SigningAlg, SubkeyAlg, SubkeyBundleXml,
    MAX_CREDENTIAL_IQ_BYTES,
};
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

/// Per-request caller provenance, authenticated identity, and clock context for
/// `CredentialIqHandler`.
#[derive(Debug, Clone)]
pub struct CredentialIqContext {
    pub trust_domain: String,
    pub caller_tenant_id: TenantId,
    pub caller_account_id: AccountId,
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
            return opaque_error_xml(
                "oversized",
                ErrorType::Modify,
                DefinedCondition::PolicyViolation,
                "credential IQ exceeds maximum permitted size",
            );
        }

        let elem: Element = match raw_xml.parse() {
            Ok(el) => el,
            Err(_) => {
                return opaque_error_xml(
                    "malformed",
                    ErrorType::Modify,
                    DefinedCondition::BadRequest,
                    "malformed credential IQ XML",
                );
            }
        };

        let iq = match Iq::try_from(elem) {
            Ok(iq) => iq,
            Err(_) => {
                return opaque_error_xml(
                    "malformed",
                    ErrorType::Modify,
                    DefinedCondition::BadRequest,
                    "invalid IQ stanza structure",
                );
            }
        };

        let reply = self.handle_iq(&iq, ctx).await;
        ws_stanza_to_xml(&Stanza::Iq(Box::new(reply)))
    }

    /// Handles a parsed [`Iq`] stanza carrying a `urn:waddle:credential:1` payload.
    pub async fn handle_iq(&self, iq: &Iq, ctx: &CredentialIqContext) -> Iq {
        let (is_get, from, to, id, payload) = match iq {
            Iq::Get {
                from,
                to,
                id,
                payload,
                ..
            } => (true, from.clone(), to.clone(), id.clone(), payload),
            Iq::Set {
                from,
                to,
                id,
                payload,
                ..
            } => (false, from.clone(), to.clone(), id.clone(), payload),
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

        let requires_get = matches!(
            req,
            CredentialRequest::GetSubkey | CredentialRequest::Status(_)
        );
        if is_get != requires_get {
            return opaque_error(
                to,
                from,
                &id,
                ErrorType::Modify,
                DefinedCondition::BadRequest,
                if requires_get {
                    "credential query requires IQ type='get'"
                } else {
                    "credential mutation requires IQ type='set'"
                },
            );
        }

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
    ) -> Result<CredentialResponse, IqError> {
        match req {
            CredentialRequest::GetSubkey => self.handle_get_subkey(),
            CredentialRequest::EnrollDevice(r) => self.handle_enroll_device(r, ctx),
            CredentialRequest::EnrollCredential(r) => self.handle_enroll_credential(r, ctx).await,
            CredentialRequest::Status(q) => self.handle_status(q, ctx).await,
            CredentialRequest::RotateCredential(r) => self.handle_rotate_credential(r, ctx).await,
            CredentialRequest::RevokeCredential(r) => self.handle_revoke_credential(r, ctx).await,
            CredentialRequest::MutatePolicy(r) => self.handle_mutate_policy(r, ctx),
        }
    }

    fn handle_get_subkey(&self) -> Result<CredentialResponse, IqError> {
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

    fn handle_enroll_device(
        &self,
        r: EnrollDeviceRequest,
        ctx: &CredentialIqContext,
    ) -> Result<CredentialResponse, IqError> {
        let tenant_id = verify_caller_tenant(&r.tenant_id, ctx)?;
        let account_id = parse_account(&r.account_id)?;
        if account_id != ctx.caller_account_id {
            return Err(forbidden(
                "enroll-device account_id does not match authenticated caller",
            ));
        }
        let device_key_id = parse_uuid(&r.device_key_id)?;
        let pubkey_sec1 = decode_b64(&r.device_public_key_b64)?;
        let pop_sig = decode_b64(&r.pop_signature_b64)?;
        let jkt = waddle_broker::p256_jwk_thumbprint(&pubkey_sec1).map_err(auth_err)?;
        let intent = self.verify_idp_proof(
            &r.idp_proof,
            &tenant_id,
            ctx.caller_account_id,
            &ctx.expected_challenge,
            Some(&jkt),
            ctx.now_ms,
        )?;
        let enrolled = self
            .device_registry
            .enroll_device_key(EnrollDeviceKeyInput {
                provenance: ctx.provenance,
                idp_intent: &intent,
                trust_domain: &ctx.trust_domain,
                device_key_id,
                public_key_sec1: pubkey_sec1,
                nonce: &r.nonce,
                expires_at_ms: r.expires_at_ms,
                now_ms: ctx.now_ms,
                pop_signature: &pop_sig,
            })
            .map_err(auth_err)?;
        Ok(CredentialResponse::DeviceReceipt(DeviceReceiptXml {
            device_key_id: enrolled.device_key_id.to_string(),
            status: "active".to_owned(),
        }))
    }

    async fn handle_enroll_credential(
        &self,
        r: EnrollCredentialRequest,
        ctx: &CredentialIqContext,
    ) -> Result<CredentialResponse, IqError> {
        let tenant_id = verify_caller_tenant(&r.tenant_id, ctx)?;
        let (account_id, payload_digest) =
            self.authorize_sealed_mutation(&SealedMutationAuthInput {
                ctx,
                tenant_id: &tenant_id,
                raw_account_id: r.account_id.as_deref(),
                purpose: r.purpose,
                idp_proof: &r.idp_proof,
                device_signature: r.device_signature.as_ref(),
                operation: "enroll-credential",
                target_id: "",
                provider: &r.provider,
                nonce: &r.nonce,
                expires_at_ms: r.expires_at_ms,
                sealed_payload: &r.sealed_payload,
            })?;
        let plaintext = self.unseal_sealed_payload(&UnsealPayloadInput {
            ctx,
            tenant_id: &tenant_id,
            account_id,
            provider: &r.provider,
            purpose: r.purpose,
            nonce: &r.nonce,
            expires_at_ms: r.expires_at_ms,
            sealed_payload: &r.sealed_payload,
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
        Ok(credential_receipt_response(receipt))
    }

    async fn handle_status(
        &self,
        q: CredentialStatusQuery,
        ctx: &CredentialIqContext,
    ) -> Result<CredentialResponse, IqError> {
        let tenant_id = verify_caller_tenant(&q.tenant_id, ctx)?;
        let records = self
            .custody
            .ledger()
            .list_envelopes_for_tenant(&tenant_id)
            .await
            .map_err(|err| internal_error(err.to_string()))?;
        let entries = records
            .into_iter()
            .filter_map(|rec| build_status_entry(rec, &q, ctx.caller_account_id))
            .collect();
        Ok(CredentialResponse::StatusResult(entries))
    }

    async fn handle_rotate_credential(
        &self,
        r: RotateCredentialRequest,
        ctx: &CredentialIqContext,
    ) -> Result<CredentialResponse, IqError> {
        let tenant_id = verify_caller_tenant(&r.tenant_id, ctx)?;
        let credential_id = parse_uuid(&r.credential_id)?;
        let (account_id, _) = self.authorize_sealed_mutation(&SealedMutationAuthInput {
            ctx,
            tenant_id: &tenant_id,
            raw_account_id: r.account_id.as_deref(),
            purpose: r.purpose,
            idp_proof: &r.idp_proof,
            device_signature: r.device_signature.as_ref(),
            operation: "rotate-credential",
            target_id: &r.credential_id,
            provider: &r.provider,
            nonce: &r.nonce,
            expires_at_ms: r.expires_at_ms,
            sealed_payload: &r.sealed_payload,
        })?;
        if let Some(existing) = self
            .custody
            .ledger()
            .get_envelope(&tenant_id, credential_id)
            .await
            .map_err(|err| internal_error(err.to_string()))?
        {
            if existing.account_id != account_id {
                return Err(forbidden("rotation target scope/owner mismatch"));
            }
        }
        let plaintext = self.unseal_sealed_payload(&UnsealPayloadInput {
            ctx,
            tenant_id: &tenant_id,
            account_id,
            provider: &r.provider,
            purpose: r.purpose,
            nonce: &r.nonce,
            expires_at_ms: r.expires_at_ms,
            sealed_payload: &r.sealed_payload,
        })?;
        let receipt = self
            .custody
            .rotate(&tenant_id, credential_id, &plaintext)
            .await
            .map_err(broker_err)?;
        Ok(credential_receipt_response(receipt))
    }

    async fn handle_revoke_credential(
        &self,
        r: RevokeCredentialRequest,
        ctx: &CredentialIqContext,
    ) -> Result<CredentialResponse, IqError> {
        let tenant_id = verify_caller_tenant(&r.tenant_id, ctx)?;
        let credential_id = parse_uuid(&r.credential_id)?;
        let account_id = r.account_id.as_deref().map(parse_account).transpose()?;
        if r.idp_proof.is_none() && r.device_signature.is_none() {
            return Err((
                ErrorType::Auth,
                DefinedCondition::NotAuthorized,
                "revocation requires IdP proof or enrolled device signature".to_owned(),
            ));
        }
        let verified_intent = r
            .idp_proof
            .as_ref()
            .map(|idp| {
                self.verify_idp_proof(
                    idp,
                    &tenant_id,
                    ctx.caller_account_id,
                    &ctx.expected_challenge,
                    None,
                    ctx.now_ms,
                )
            })
            .transpose()?;
        if let Some(ref dev_sig) = r.device_signature {
            self.verify_device_signature(&VerifyDeviceSigInput {
                dev_sig,
                ctx,
                tenant_id: &tenant_id,
                account_id: ctx.caller_account_id,
                operation: "revoke-credential",
                target_id: &r.credential_id,
                provider: "",
                nonce: &r.nonce,
                expires_at_ms: r.expires_at_ms,
                payload_sha256: compute_revoke_payload_digest(&r.credential_id),
            })?;
        }
        let existing = self
            .custody
            .ledger()
            .get_envelope(&tenant_id, credential_id)
            .await
            .map_err(|err| internal_error(err.to_string()))?;
        authorize_revocation_target(
            account_id,
            existing.as_ref().map(|e| e.account_id),
            verified_intent.as_ref(),
            ctx,
        )?;
        let receipt = self
            .custody
            .revoke_and_destroy(&tenant_id, credential_id)
            .await
            .map_err(broker_err)?;
        Ok(credential_receipt_response(receipt))
    }

    fn handle_mutate_policy(
        &self,
        r: MutatePolicyRequest,
        ctx: &CredentialIqContext,
    ) -> Result<CredentialResponse, IqError> {
        let tenant_id = verify_caller_tenant(&r.tenant_id, ctx)?;
        let intent = self.verify_idp_proof(
            &r.idp_proof,
            &tenant_id,
            ctx.caller_account_id,
            &ctx.expected_challenge,
            None,
            ctx.now_ms,
        )?;
        authorize_admin_policy_mutation(ctx.provenance, Some(&intent), &ctx.cell_admin_flags)
            .map_err(auth_err)?;

        let key = (
            tenant_id.as_str().to_owned(),
            r.scope.clone(),
            r.target_id.clone(),
        );
        let gen = {
            let mut guard = self
                .policy_generations
                .write()
                .map_err(|_| internal_error("policy lock poisoned"))?;
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

    fn authorize_sealed_mutation(
        &self,
        input: &SealedMutationAuthInput<'_>,
    ) -> Result<(Option<AccountId>, [u8; 32]), IqError> {
        let account_id = input.raw_account_id.map(parse_account).transpose()?;
        validate_purpose_and_account(input.purpose, account_id, input.ctx.caller_account_id)?;
        let intent = self.verify_idp_proof(
            input.idp_proof,
            input.tenant_id,
            input.ctx.caller_account_id,
            &input.ctx.expected_challenge,
            None,
            input.ctx.now_ms,
        )?;
        if input.purpose == CredentialPurpose::TenantInference {
            authorize_admin_policy_mutation(
                input.ctx.provenance,
                Some(&intent),
                &input.ctx.cell_admin_flags,
            )
            .map_err(auth_err)?;
        }
        let payload_digest =
            compute_sealed_payload_digest(input.provider, input.purpose, input.sealed_payload);
        if let Some(dev_sig) = input.device_signature {
            self.verify_device_signature(&VerifyDeviceSigInput {
                dev_sig,
                ctx: input.ctx,
                tenant_id: input.tenant_id,
                account_id: input.ctx.caller_account_id,
                operation: input.operation,
                target_id: input.target_id,
                provider: input.provider,
                nonce: input.nonce,
                expires_at_ms: input.expires_at_ms,
                payload_sha256: payload_digest,
            })?;
        }
        Ok((account_id, payload_digest))
    }

    fn unseal_sealed_payload(
        &self,
        input: &UnsealPayloadInput<'_>,
    ) -> Result<SecretBytes, IqError> {
        let blob = decode_sealed_blob(input.sealed_payload)?;
        let seal_ctx = ClientSealContext {
            trust_domain: input.ctx.trust_domain.clone(),
            tenant_id: input.tenant_id.clone(),
            account_id: input.account_id,
            provider: input.provider.to_owned(),
            purpose: input.purpose.as_str().to_owned(),
            nonce: input.nonce.to_owned(),
            expires_at_ms: input.expires_at_ms,
            version: 1,
        };
        self.subkey_ring
            .unseal_client_envelope(&blob, &seal_ctx, input.ctx.now_ms)
            .map_err(|err| bad_request(err.to_string()))
    }

    fn verify_device_signature(&self, input: &VerifyDeviceSigInput<'_>) -> Result<(), IqError> {
        let device_key_id = parse_uuid(&input.dev_sig.device_key_id)?;
        let sig_bytes = decode_b64(&input.dev_sig.signature_b64)?;
        let cmd = SignedDeviceCommand {
            trust_domain: &input.ctx.trust_domain,
            tenant_id: input.tenant_id,
            account_id: input.account_id,
            device_key_id,
            operation: input.operation,
            target_id: input.target_id,
            provider: input.provider,
            nonce: input.nonce,
            expires_at_ms: input.expires_at_ms,
            payload_sha256: input.payload_sha256,
        };
        self.device_registry
            .verify_signed_command(&cmd, &sig_bytes, input.ctx.now_ms)
            .map_err(auth_err)
    }

    fn verify_idp_proof(
        &self,
        proof: &IdpProofXml,
        tenant_id: &TenantId,
        account_id: AccountId,
        expected_challenge: &str,
        expected_device_jkt: Option<&str>,
        now_ms: i64,
    ) -> Result<VerifiedIdpIntent, IqError> {
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

type IqError = (ErrorType, DefinedCondition, String);

struct SealedMutationAuthInput<'a> {
    ctx: &'a CredentialIqContext,
    tenant_id: &'a TenantId,
    raw_account_id: Option<&'a str>,
    purpose: CredentialPurpose,
    idp_proof: &'a IdpProofXml,
    device_signature: Option<&'a DeviceSignatureXml>,
    operation: &'a str,
    target_id: &'a str,
    provider: &'a str,
    nonce: &'a str,
    expires_at_ms: i64,
    sealed_payload: &'a SealedPayloadXml,
}

struct UnsealPayloadInput<'a> {
    ctx: &'a CredentialIqContext,
    tenant_id: &'a TenantId,
    account_id: Option<AccountId>,
    provider: &'a str,
    purpose: CredentialPurpose,
    nonce: &'a str,
    expires_at_ms: i64,
    sealed_payload: &'a SealedPayloadXml,
}

struct VerifyDeviceSigInput<'a> {
    dev_sig: &'a DeviceSignatureXml,
    ctx: &'a CredentialIqContext,
    tenant_id: &'a TenantId,
    account_id: AccountId,
    operation: &'a str,
    target_id: &'a str,
    provider: &'a str,
    nonce: &'a str,
    expires_at_ms: i64,
    payload_sha256: [u8; 32],
}

fn bad_request(msg: impl Into<String>) -> IqError {
    (ErrorType::Modify, DefinedCondition::BadRequest, msg.into())
}

fn forbidden(msg: impl Into<String>) -> IqError {
    (ErrorType::Cancel, DefinedCondition::Forbidden, msg.into())
}

fn internal_error(msg: impl Into<String>) -> IqError {
    (
        ErrorType::Cancel,
        DefinedCondition::InternalServerError,
        msg.into(),
    )
}

fn verify_caller_tenant(
    raw_tenant_id: &str,
    ctx: &CredentialIqContext,
) -> Result<TenantId, IqError> {
    enforce_not_scram(ctx.provenance).map_err(auth_err)?;
    let tenant_id = parse_tenant(raw_tenant_id)?;
    enforce_caller_tenant(&tenant_id, &ctx.caller_tenant_id)?;
    Ok(tenant_id)
}

fn credential_receipt_response(receipt: CredentialReceipt) -> CredentialResponse {
    CredentialResponse::CredentialReceipt(CredentialReceiptXml {
        credential_id: receipt.credential_id.to_string(),
        version: receipt.version,
        status: map_status(receipt.status),
    })
}

fn build_status_entry(
    rec: waddle_broker::StoredEnvelopeRecord,
    query: &CredentialStatusQuery,
    caller_account_id: AccountId,
) -> Option<CredentialStatusEntry> {
    if rec
        .account_id
        .is_some_and(|owner| owner != caller_account_id)
    {
        return None;
    }
    if query.provider.as_ref().is_some_and(|p| rec.provider != *p) {
        return None;
    }
    let purpose = if rec.account_id.is_some() {
        CredentialPurpose::MemberInference
    } else {
        CredentialPurpose::TenantInference
    };
    if query.purpose.is_some_and(|expected| purpose != expected) {
        return None;
    }
    Some(CredentialStatusEntry {
        id: rec.credential_id.to_string(),
        provider: rec.provider,
        purpose,
        status: CredentialStatus::Active,
        version: rec.version,
        updated_at_ms: rec.created_at_ms,
    })
}

fn authorize_revocation_target(
    account_id: Option<AccountId>,
    existing_owner: Option<Option<AccountId>>,
    verified_intent: Option<&VerifiedIdpIntent>,
    ctx: &CredentialIqContext,
) -> Result<(), IqError> {
    let targets_tenant_cred = account_id.is_none() || matches!(existing_owner, Some(None));
    if targets_tenant_cred {
        return authorize_admin_policy_mutation(
            ctx.provenance,
            verified_intent,
            &ctx.cell_admin_flags,
        )
        .map_err(auth_err);
    }
    let is_admin = verified_intent.is_some_and(|intent| intent.is_tenant_admin);
    let mismatches_requested = account_id.is_some_and(|a| a != ctx.caller_account_id);
    let mismatches_existing =
        existing_owner.is_some_and(|owner| owner != Some(ctx.caller_account_id));
    if (mismatches_requested || mismatches_existing) && !is_admin {
        return Err(forbidden("cannot revoke another member's credential"));
    }
    Ok(())
}

fn enforce_caller_tenant(requested: &TenantId, caller: &TenantId) -> Result<(), IqError> {
    if requested != caller {
        return Err(forbidden(
            "requested tenant does not match authenticated caller tenant",
        ));
    }
    Ok(())
}

fn validate_purpose_and_account(
    purpose: CredentialPurpose,
    account_id: Option<AccountId>,
    caller_account_id: AccountId,
) -> Result<(), IqError> {
    match (purpose, account_id) {
        (CredentialPurpose::MemberInference, Some(acct)) if acct == caller_account_id => Ok(()),
        (CredentialPurpose::MemberInference, Some(_)) => Err(forbidden(
            "member-inference account_id does not match authenticated caller",
        )),
        (CredentialPurpose::MemberInference, None) => {
            Err(bad_request("member-inference requires account-id"))
        }
        (CredentialPurpose::TenantInference, None) => Ok(()),
        (CredentialPurpose::TenantInference, Some(_)) => {
            Err(bad_request("tenant-inference must not specify account-id"))
        }
    }
}

/// Computes the canonical SHA-256 digest of a sealed credential payload.
pub fn compute_sealed_payload_digest(
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

/// Computes the canonical SHA-256 payload digest signed by a device key for
/// `<revoke-credential/>`.
pub fn compute_revoke_payload_digest(credential_id: &str) -> [u8; 32] {
    Sha256::digest(credential_id.as_bytes()).into()
}

fn decode_sealed_blob(sealed: &SealedPayloadXml) -> Result<ClientSealedBlob, IqError> {
    Ok(ClientSealedBlob {
        subkey_id: parse_uuid(&sealed.subkey_id)?,
        ephemeral_public_sec1: decode_b64(&sealed.ephemeral_key_b64)?,
        ciphertext: decode_b64(&sealed.ciphertext_b64)?,
    })
}

fn decode_b64(raw: &str) -> Result<Vec<u8>, IqError> {
    STANDARD
        .decode(raw.trim())
        .map_err(|_| bad_request("invalid base64 payload"))
}

fn parse_tenant(raw: &str) -> Result<TenantId, IqError> {
    TenantId::parse(raw).map_err(|err| bad_request(err.to_string()))
}

fn parse_account(raw: &str) -> Result<AccountId, IqError> {
    parse_uuid(raw).map(AccountId::from_uuid)
}

fn parse_uuid(raw: &str) -> Result<Uuid, IqError> {
    Uuid::parse_str(raw).map_err(|_| bad_request("invalid UUID"))
}

fn map_status(s: CredentialLifecycleStatus) -> CredentialStatus {
    match s {
        CredentialLifecycleStatus::Active => CredentialStatus::Active,
        CredentialLifecycleStatus::Revoked => CredentialStatus::Revoked,
    }
}

fn auth_err(err: AuthError) -> IqError {
    match err {
        AuthError::DeviceKeyAlreadyExists(_) => (
            ErrorType::Cancel,
            DefinedCondition::Conflict,
            err.to_string(),
        ),
        other => forbidden(other.to_string()),
    }
}

fn broker_err(err: BrokerError) -> IqError {
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
        | BrokerError::CredentialDestroyed(_) => forbidden(err.to_string()),
        BrokerError::EgressClosedPendingRestore => (
            ErrorType::Wait,
            DefinedCondition::ServiceUnavailable,
            err.to_string(),
        ),
        other => internal_error(other.to_string()),
    }
}

fn opaque_error_xml(
    id: &str,
    error_type: ErrorType,
    condition: DefinedCondition,
    text: &str,
) -> String {
    let err_iq = opaque_error(None, None, id, error_type, condition, text);
    ws_stanza_to_xml(&Stanza::Iq(Box::new(err_iq)))
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
