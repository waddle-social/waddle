use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use serde_json::{json, Map, Value};
use tokio::sync::{mpsc, OnceCell};
use uuid::Uuid;
use waddle_broker::admission::invocation::{
    InvocationPrincipal, PayerSelection, SignedInferenceInvocation,
};
use waddle_broker::admission::service::{AdmitInvocationInput, BrokerAdmissionService};
use waddle_broker::egress::client::{
    BrokerEgressClient, EgressError, EgressInferenceRequest, ExpectedEgressBinding,
    ProviderOperationWire,
};
use waddle_broker::idp_proof::AuthProvenance;
use waddle_broker_core::admission::{
    AdmissionLedgerStore, AttemptId, BudgetLimits, RoomScopeKey, RunId, StepId,
};
use waddle_broker_core::identity::{AccountId, HolderInstance, TenantId};
use waddle_extensions::host_tools::{InvocationContext, InvocationKind};
use waddle_extensions::runtime::{
    ExtensionInferenceHost, HostInferenceChunk, HostInferenceError, HostInferenceErrorCode,
    HostInferenceRequest, InferenceCredentialRef, OpenedInferenceStream, StreamCancelHandle,
    StreamItem, VerifiedInferenceUsage, DEFAULT_MAX_STREAM_CHUNK_BYTES,
    DEFAULT_STREAM_CHANNEL_CAPACITY,
};

#[derive(Default)]
pub(crate) struct DeferredExtensionInferenceHost {
    inner: OnceCell<Arc<dyn ExtensionInferenceHost>>,
}

impl DeferredExtensionInferenceHost {
    #[expect(dead_code, reason = "populated once startup broker admission/egress wiring lands")]
    pub(crate) fn set(&self, host: Arc<dyn ExtensionInferenceHost>) {
        let _ = self.inner.set(host);
    }

    fn host(&self) -> Result<&Arc<dyn ExtensionInferenceHost>, HostInferenceError> {
        self.inner.get().ok_or_else(|| {
            HostInferenceError::new(
                HostInferenceErrorCode::Denied,
                "brokered inference host is not wired into the server",
            )
        })
    }
}

#[async_trait]
impl ExtensionInferenceHost for DeferredExtensionInferenceHost {
    async fn start_stream(
        &self,
        context: &InvocationContext,
        request: HostInferenceRequest,
    ) -> Result<OpenedInferenceStream, HostInferenceError> {
        self.host()?.start_stream(context, request).await
    }
}

/// Resolves the enrolled broker credential ID and optional signed invocation
/// for a given `(tenant_id, context, request)` without exposing raw secrets to
/// cells or guests.
#[async_trait]
pub trait BrokerCredentialResolver: Send + Sync {
    async fn resolve_tenant_credential(
        &self,
        tenant_id: &TenantId,
        provider: &str,
        credential_name: &str,
    ) -> Result<Uuid, HostInferenceError>;

    async fn resolve_member_invocation(
        &self,
        tenant_id: &TenantId,
        context: &InvocationContext,
        provider: &str,
    ) -> Result<(AccountId, Uuid, SignedInferenceInvocation), HostInferenceError>;
}

/// Production [`ExtensionInferenceHost`] bridging `waddle-extensions` Wasm
/// components to [`BrokerAdmissionService`] and [`BrokerEgressClient`].
pub struct BrokerExtensionInferenceHost<L> {
    trust_domain: String,
    admission: Arc<BrokerAdmissionService<L>>,
    egress: Arc<BrokerEgressClient>,
    holder: HolderInstance,
    credential_resolver: Arc<dyn BrokerCredentialResolver>,
    default_budget: BudgetLimits,
    default_deployment: String,
    default_reserved_spend_microcents: u64,
}

impl<L> BrokerExtensionInferenceHost<L>
where
    L: AdmissionLedgerStore + Send + Sync + 'static,
{
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        trust_domain: impl Into<String>,
        admission: Arc<BrokerAdmissionService<L>>,
        egress: Arc<BrokerEgressClient>,
        holder: HolderInstance,
        credential_resolver: Arc<dyn BrokerCredentialResolver>,
        default_budget: BudgetLimits,
        default_deployment: impl Into<String>,
        default_reserved_spend_microcents: u64,
    ) -> Self {
        Self {
            trust_domain: trust_domain.into(),
            admission,
            egress,
            holder,
            credential_resolver,
            default_budget,
            default_deployment: default_deployment.into(),
            default_reserved_spend_microcents,
        }
    }
}

#[async_trait]
impl<L> ExtensionInferenceHost for BrokerExtensionInferenceHost<L>
where
    L: AdmissionLedgerStore + Send + Sync + 'static,
{
    async fn start_stream(
        &self,
        context: &InvocationContext,
        request: HostInferenceRequest,
    ) -> Result<OpenedInferenceStream, HostInferenceError> {
        let tenant_id = context.tenant_id.clone().ok_or_else(|| {
            HostInferenceError::new(
                HostInferenceErrorCode::Denied,
                "canonical tenant_id is required for brokered inference",
            )
        })?;

        let room_scope = RoomScopeKey {
            scope_id: context
                .source_room
                .as_ref()
                .map(|r| r.as_str().to_owned())
                .unwrap_or_else(|| format!("waddle:{}", context.waddle_id.as_str())),
        };
        let extension_digest = sha256_digest(context.plugin_id.as_str().as_bytes());
        let command_digest = sha256_digest(
            format!(
                "{}:{}:{}:{}",
                context.plugin_id.as_str(),
                request.provider,
                request.deployment,
                request.model
            )
            .as_bytes(),
        );
        let now_ms = Utc::now().timestamp_millis();

        let (principal, payer, signed_invocation, invocation_nonce, expires_at_ms, budget) =
            match &request.credential {
                InferenceCredentialRef::Tenant(name) => {
                    let cred_id = self
                        .credential_resolver
                        .resolve_tenant_credential(&tenant_id, &request.provider, name)
                        .await?;
                    let principal = match context.kind {
                        InvocationKind::RoomMessageObserve => InvocationPrincipal::Observation,
                        InvocationKind::ProviderWebhook => InvocationPrincipal::Webhook,
                        InvocationKind::MessageHook => InvocationPrincipal::Schedule,
                        InvocationKind::Command | InvocationKind::Launch => {
                            InvocationPrincipal::LocalMember {
                                account_id: AccountId::new_v4(),
                                provenance: AuthProvenance::ExternalIdp,
                            }
                        }
                        _ => {
                            return Err(HostInferenceError::new(
                                HostInferenceErrorCode::Denied,
                                "unsupported invocation kind for brokered inference",
                            ));
                        }
                    };
                    (
                        principal,
                        PayerSelection::Tenant {
                            credential_id: cred_id,
                        },
                        None,
                        random_nonce32(),
                        now_ms + 60_000,
                        self.default_budget,
                    )
                }
                InferenceCredentialRef::InvokingUser(provider_name) => {
                    if !matches!(
                        context.kind,
                        InvocationKind::Command | InvocationKind::Launch
                    ) || context.requester.is_none()
                    {
                        return Err(HostInferenceError::new(
                            HostInferenceErrorCode::Denied,
                            "invoking-user credential requires an interactive user invocation",
                        ));
                    }
                    let (account_id, cred_id, signed) = self
                        .credential_resolver
                        .resolve_member_invocation(&tenant_id, context, provider_name)
                        .await?;
                    let nonce = signed.nonce;
                    let exp = signed.expires_at_ms;
                    let signed_budget = signed.budget;
                    (
                        InvocationPrincipal::LocalMember {
                            account_id,
                            provenance: AuthProvenance::ExternalIdp,
                        },
                        PayerSelection::Member {
                            account_id,
                            credential_id: cred_id,
                        },
                        Some(signed),
                        nonce,
                        exp,
                        signed_budget,
                    )
                }
            };

        let deployment = if request.deployment.trim().is_empty() {
            self.default_deployment.clone()
        } else {
            request.deployment.clone()
        };
        let reserved_spend = self
            .default_reserved_spend_microcents
            .min(budget.max_spend_microcents);

        let admit_input = AdmitInvocationInput {
            attempt_id: AttemptId::new_v4(),
            run_id: RunId::new_v4(),
            step_id: StepId::new(0),
            trust_domain: self.trust_domain.clone(),
            tenant_id: tenant_id.clone(),
            principal,
            extension_id: context.plugin_id.as_str().to_owned(),
            extension_digest_sha256: extension_digest,
            provider: request.provider.clone(),
            payer,
            room_or_space: room_scope.clone(),
            holder: self.holder.clone(),
            command_digest_sha256: command_digest,
            invocation_nonce,
            expires_at_ms,
            reserved_spend_microcents: reserved_spend,
            budget,
            signed_invocation,
            now_ms,
        };

        let admitted = self
            .admission
            .admit_invocation(admit_input)
            .await
            .map_err(|_| {
                HostInferenceError::new(
                    HostInferenceErrorCode::Denied,
                    "broker admission denied inference invocation",
                )
            })?;

        let mut body_map = Map::new();
        body_map.insert("model".to_owned(), Value::String(request.model.clone()));
        body_map.insert("stream".to_owned(), Value::Bool(true));
        let messages: Vec<Value> = request
            .messages
            .iter()
            .map(|m| json!({"role": m.role, "content": m.content}))
            .collect();
        body_map.insert("messages".to_owned(), Value::Array(messages));
        if let Some(max_out) = request.max_output_tokens {
            let capped = u64::from(max_out).min(admitted.dispatch_context.budget.max_output_tokens);
            if capped == 0 {
                return Err(HostInferenceError::new(
                    HostInferenceErrorCode::InvalidRequest,
                    "max_output_tokens must be greater than zero",
                ));
            }
            body_map.insert("max_output_tokens".to_owned(), Value::Number(capped.into()));
        }

        let egress_req = EgressInferenceRequest {
            provider: request.provider.clone(),
            deployment: deployment.clone(),
            model: request.model.clone(),
            operation: ProviderOperationWire::ChatCompletions,
            requested_path: None,
            body_json: Value::Object(body_map),
            stream: true,
        };

        let expected = ExpectedEgressBinding {
            trust_domain: &self.trust_domain,
            extension_id: context.plugin_id.as_str(),
            extension_digest_sha256: extension_digest,
            room_or_space: &room_scope.scope_id,
            command_digest_sha256: command_digest,
            deployment: &deployment,
            model: &request.model,
            operation: ProviderOperationWire::ChatCompletions,
        };

        let mut egress_stream = self
            .egress
            .execute_streaming_checked(&admitted, &egress_req, expected)
            .await
            .map_err(map_egress_err)?;

        let (cancel, mut host_cancel_rx) = StreamCancelHandle::new();
        let (item_tx, item_rx) = mpsc::channel(DEFAULT_STREAM_CHANNEL_CAPACITY);
        let mut admit_cancel_rx = admitted.cancel_rx.clone();
        let provider_id = request.provider;
        let model_id = request.model;

        tokio::spawn(async move {
            let mut seq = 0u64;
            let mut total_output_bytes = 0u64;
            loop {
                if *admit_cancel_rx.borrow() {
                    egress_stream.cancel_sync();
                    let _ = item_tx.try_send(StreamItem::Error(HostInferenceError::new(
                        HostInferenceErrorCode::Canceled,
                        "inference stream canceled by broker replica fence",
                    )));
                    break;
                }
                if *host_cancel_rx.borrow() {
                    egress_stream.cancel_sync();
                    break;
                }
                tokio::select! {
                    biased;
                    changed = admit_cancel_rx.changed() => {
                        if changed.is_err() || *admit_cancel_rx.borrow() {
                            egress_stream.cancel_sync();
                            let _ = item_tx.try_send(StreamItem::Error(HostInferenceError::new(
                                HostInferenceErrorCode::Canceled,
                                "inference stream canceled by broker replica fence",
                            )));
                            break;
                        }
                    }
                    changed = host_cancel_rx.changed() => {
                        if changed.is_err() || *host_cancel_rx.borrow() {
                            egress_stream.cancel_sync();
                            break;
                        }
                    }
                    chunk_res = egress_stream.next_chunk() => {
                        match chunk_res {
                            Ok(Some(chunk)) => {
                                total_output_bytes = total_output_bytes.saturating_add(chunk.data.len() as u64);
                                let item = StreamItem::Chunk(HostInferenceChunk {
                                    delta: chunk.data,
                                    sequence: seq,
                                    finish_reason: None,
                                });
                                seq = seq.saturating_add(1);
                                let send_ok = tokio::select! {
                                    biased;
                                    _ = host_cancel_rx.changed() => {
                                        egress_stream.cancel_sync();
                                        false
                                    }
                                    _ = admit_cancel_rx.changed() => {
                                        egress_stream.cancel_sync();
                                        let _ = item_tx.try_send(StreamItem::Error(HostInferenceError::new(
                                            HostInferenceErrorCode::Canceled,
                                            "inference stream canceled by broker replica fence",
                                        )));
                                        false
                                    }
                                    res = item_tx.send(item) => res.is_ok(),
                                };
                                if !send_ok {
                                    egress_stream.cancel_sync();
                                    break;
                                }
                            }
                            Ok(None) => {
                                let _ = item_tx
                                    .send(StreamItem::EndOfStream(VerifiedInferenceUsage {
                                        provider: provider_id,
                                        model: model_id,
                                        input_tokens: 0,
                                        output_tokens: total_output_bytes,
                                        cost_microcents: reserved_spend,
                                    }))
                                    .await;
                                break;
                            }
                            Err(err) => {
                                egress_stream.cancel_sync();
                                let _ = item_tx.try_send(StreamItem::Error(map_egress_err(err)));
                                break;
                            }
                        }
                    }
                }
            }
        });

        Ok(OpenedInferenceStream {
            rx: item_rx,
            cancel,
            max_chunk_bytes: DEFAULT_MAX_STREAM_CHUNK_BYTES,
        })
    }
}

fn random_nonce32() -> [u8; 32] {
    let mut out = [0u8; 32];
    out[..16].copy_from_slice(Uuid::new_v4().as_bytes());
    out[16..].copy_from_slice(Uuid::new_v4().as_bytes());
    out
}

fn sha256_digest(bytes: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let out = Sha256::digest(bytes);
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&out);
    arr
}

fn map_egress_err(err: EgressError) -> HostInferenceError {
    match err {
        EgressError::Profile(_) | EgressError::BindingMismatch(_) | EgressError::RequestTooLarge { .. } => {
            HostInferenceError::new(HostInferenceErrorCode::InvalidRequest, err.to_string())
        }
        EgressError::Ssrf(_) | EgressError::Custody(_) | EgressError::SpendCapExceeded { .. } => {
            HostInferenceError::new(HostInferenceErrorCode::Denied, err.to_string())
        }
        EgressError::ConcurrencyExhausted => {
            HostInferenceError::new(HostInferenceErrorCode::RateLimited, err.to_string())
        }
        EgressError::Canceled => {
            HostInferenceError::new(HostInferenceErrorCode::Canceled, err.to_string())
        }
        EgressError::ChunkTooLarge { .. } => {
            HostInferenceError::new(HostInferenceErrorCode::ChunkLimitExceeded, err.to_string())
        }
        EgressError::Timeout
        | EgressError::ResponseTooLarge { .. }
        | EgressError::RedirectBlocked { .. }
        | EgressError::UpstreamStatus { .. }
        | EgressError::MalformedUpstreamResponse
        | EgressError::TransportFailed => {
            HostInferenceError::new(HostInferenceErrorCode::TemporaryFailure, err.to_string())
        }
    }
}
