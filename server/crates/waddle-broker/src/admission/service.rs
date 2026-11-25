//! `BrokerAdmissionService` orchestrating the Step 5 3-fence admission,
//! restriction, revocation, and reconciliation protocol with secret-free
//! PObserve JSONL trace emission and optional deterministic `Barrier` hooks.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::{watch, Barrier};
use waddle_broker_core::admission::{
    AdmissionError, AdmissionLedgerStore, AdmitAttemptRequest, AdmitReceipt, AttemptId,
    AttemptOutcome, BudgetLimits, FenceKey, FenceKind, RoomScopeKey, RunId, StepId,
};
use waddle_broker_core::identity::{HolderInstance, TenantId};
use waddle_broker_core::permission::{
    AuthzConsistency, AuthzObject, AuthzObjectType, AuthzRelation, AuthzSubject, AuthzSubjectType,
    CheckDecision, CheckRequest, PermissionBackend, PermissionBackendError,
};
use waddle_broker_core::placement::FenceGeneration;
use waddle_broker_core::trace_verifier::{TraceEvent, TraceRecord};

use crate::admission::invocation::{
    validate_principal_and_payer, validate_signed_invocation, InvocationError, InvocationPrincipal,
    PayerSelection, SignedInferenceInvocation,
};
use crate::admission::replica_fence::{BrokerReplicaFence, BROKER_FENCE_ACK_DEADLINE};
use crate::idp_proof::DeviceKeyRegistry;

/// Errors returned by [`BrokerAdmissionService`].
#[derive(Debug, Error)]
pub enum AdmissionServiceError {
    #[error("invocation validation failed: {0}")]
    Invocation(#[from] InvocationError),
    #[error("authorization denied for requested payer ({payer:?})")]
    AuthorizationDenied { payer: PayerSelection },
    #[error("permission backend error: {0}")]
    Permission(#[from] PermissionBackendError),
    #[error("admission ledger error: {0}")]
    Ledger(#[from] AdmissionError),
}

/// Optional deterministic synchronization hooks for testing mid-check races.
#[derive(Clone, Default)]
pub struct AdmissionBarrierHooks {
    /// Pauses after `snapshot_fences` and before `PermissionBackend::check`.
    pub before_authz_check: Option<Arc<Barrier>>,
    /// Pauses after `PermissionBackend::check` and before `admit_attempt`.
    pub after_authz_check: Option<Arc<Barrier>>,
}

/// Shared secret-free trace sink verified by `waddle_broker_core::trace_verifier`.
#[derive(Clone, Default)]
pub struct TraceRecorder {
    ts: Arc<AtomicU64>,
    records: Arc<Mutex<Vec<TraceRecord>>>,
}

impl TraceRecorder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn emit(&self, actor_id: &str, tenant_id: &TenantId, event: TraceEvent) {
        let ts_logical = self.ts.fetch_add(1, Ordering::SeqCst) + 1;
        let tenant_hash = hex::encode(&Sha256::digest(tenant_id.as_str().as_bytes())[..8]);
        if let Ok(mut guard) = self.records.lock() {
            guard.push(TraceRecord {
                ts_logical,
                actor_id: actor_id.to_owned(),
                tenant_hash,
                event,
            });
        }
    }

    pub fn to_jsonl(&self) -> String {
        let records = self.records.lock().map(|g| g.clone()).unwrap_or_default();
        let mut out = String::new();
        for rec in records {
            if let Ok(line) = serde_json::to_string(&rec) {
                out.push_str(&line);
                out.push('\n');
            }
        }
        out
    }

    pub fn records(&self) -> Vec<TraceRecord> {
        self.records.lock().map(|g| g.clone()).unwrap_or_default()
    }
}

/// Input to [`BrokerAdmissionService::admit_invocation`].
#[derive(Debug, Clone)]
pub struct AdmitInvocationInput {
    pub attempt_id: AttemptId,
    pub run_id: RunId,
    pub step_id: StepId,
    pub tenant_id: TenantId,
    pub principal: InvocationPrincipal,
    pub payer: PayerSelection,
    pub room_or_space: RoomScopeKey,
    pub holder: HolderInstance,
    pub invocation_nonce: [u8; 32],
    pub expires_at_ms: i64,
    pub reserved_spend_microcents: u64,
    pub budget: BudgetLimits,
    pub signed_invocation: Option<SignedInferenceInvocation>,
    pub now_ms: i64,
}

/// Successful admission outcome containing the durable receipt and the local
/// stream cancellation receiver from [`BrokerReplicaFence`].
pub struct AdmittedDispatch {
    pub receipt: AdmitReceipt,
    pub cancel_rx: watch::Receiver<bool>,
}

/// Full 3-fence broker admission & restriction orchestrator.
pub struct BrokerAdmissionService<L> {
    actor_id: String,
    ledger: Arc<L>,
    permissions: Arc<dyn PermissionBackend>,
    device_keys: Arc<DeviceKeyRegistry>,
    replica_fence: Arc<BrokerReplicaFence>,
    trace: TraceRecorder,
    hooks: AdmissionBarrierHooks,
}

impl<L> BrokerAdmissionService<L>
where
    L: AdmissionLedgerStore + Send + Sync + 'static,
{
    pub fn new(
        actor_id: impl Into<String>,
        ledger: Arc<L>,
        permissions: Arc<dyn PermissionBackend>,
        device_keys: Arc<DeviceKeyRegistry>,
        replica_fence: Arc<BrokerReplicaFence>,
        trace: TraceRecorder,
    ) -> Self {
        Self {
            actor_id: actor_id.into(),
            ledger,
            permissions,
            device_keys,
            replica_fence,
            trace,
            hooks: AdmissionBarrierHooks::default(),
        }
    }

    pub fn with_barrier_hooks(mut self, hooks: AdmissionBarrierHooks) -> Self {
        self.hooks = hooks;
        self
    }

    pub fn trace_recorder(&self) -> &TraceRecorder {
        &self.trace
    }

    /// Executes the full Step 5 admission sequence:
    /// 1. Validate principal & payer (strict payer separation; never switches payer).
    /// 2. For member-funded use, verify device-signed invocation against `DeviceKeyRegistry`.
    /// 3. Snapshot all 3 fences (`Credential`, `RoomOrSpace`, `Placement`).
    /// 4. Run fresh `PermissionBackend::check` with `AuthzConsistency::Strong`.
    /// 5. Atomically commit `admit_attempt` verifying all 3 fences remain `Open` at
    ///    those exact snapshotted generations.
    /// 6. Re-verify `BrokerReplicaFence` ticket and register the stream cancel handle.
    pub async fn admit_invocation(
        &self,
        input: AdmitInvocationInput,
    ) -> Result<AdmittedDispatch, AdmissionServiceError> {
        validate_principal_and_payer(&input.principal, input.payer)?;

        if let PayerSelection::Member { .. } = input.payer {
            let signed = input
                .signed_invocation
                .as_ref()
                .ok_or(InvocationError::SignedInvocationRequired)?;
            if signed.tenant_id != input.tenant_id
                || signed.payer != input.payer
                || signed.nonce != input.invocation_nonce
            {
                return Err(InvocationError::SignedInvocationBindingMismatch.into());
            }
            validate_signed_invocation(&self.device_keys, signed, input.now_ms)?;
        }

        let cred_key = input.payer.credential_key();
        let snapshot = self
            .ledger
            .snapshot_fences(&input.tenant_id, &cred_key, &input.room_or_space)
            .await?;

        let fence_bindings = [
            (
                FenceKey {
                    tenant_id: input.tenant_id.clone(),
                    kind: FenceKind::Credential(cred_key.clone()),
                },
                snapshot.credential.generation,
            ),
            (
                FenceKey {
                    tenant_id: input.tenant_id.clone(),
                    kind: FenceKind::RoomOrSpace(input.room_or_space.clone()),
                },
                snapshot.room_or_space.generation,
            ),
            (
                FenceKey {
                    tenant_id: input.tenant_id.clone(),
                    kind: FenceKind::Placement,
                },
                snapshot.placement.generation,
            ),
        ];

        let ticket = self.replica_fence.issue_ticket(fence_bindings.clone())?;

        if let Some(barrier) = &self.hooks.before_authz_check {
            barrier.wait().await;
        }

        // Fresh strongly-consistent authorization check for the requested payer ONLY.
        // Never fall back to the alternate payer if this check denies!
        let (authz_object, authz_perm, authz_subject) = build_authz_check(&input)?;
        let decision = self
            .permissions
            .check(&CheckRequest {
                tenant_id: input.tenant_id.clone(),
                object: authz_object,
                permission: authz_perm,
                subject: authz_subject,
                consistency: AuthzConsistency::Strong,
            })
            .await?;

        if decision != CheckDecision::Allowed {
            return Err(AdmissionServiceError::AuthorizationDenied { payer: input.payer });
        }

        if let Some(barrier) = &self.hooks.after_authz_check {
            barrier.wait().await;
        }

        let req = AdmitAttemptRequest {
            attempt_id: input.attempt_id,
            run_id: input.run_id,
            step_id: input.step_id,
            tenant_id: input.tenant_id.clone(),
            credential: cred_key,
            room_or_space: input.room_or_space.clone(),
            holder: input.holder,
            expected_fences: snapshot,
            invocation_nonce: input.invocation_nonce,
            expires_at_ms: input.expires_at_ms,
            reserved_spend_microcents: input.reserved_spend_microcents,
            budget: input.budget,
        };
        let receipt = self.ledger.admit_attempt(&req).await?;

        if !self.replica_fence.is_ticket_current(&ticket) {
            let _ = self
                .ledger
                .reconcile_attempt(input.attempt_id, AttemptOutcome::FailedBeforeSend)
                .await;
            return Err(AdmissionError::HolderUnauthorized.into());
        }

        let cancel_rx = self
            .replica_fence
            .register_stream(input.attempt_id, fence_bindings)?;

        let payer_label = input.payer.trace_label().to_owned();
        self.trace.emit(
            &self.actor_id,
            &input.tenant_id,
            TraceEvent::AdmissionCommit {
                attempt_id: input.attempt_id.as_uuid().to_string(),
                payer: payer_label.clone(),
                requested_payer: payer_label,
                cred_gen: snapshot.credential.generation.get(),
                room_gen: snapshot.room_or_space.generation.get(),
                place_gen: snapshot.placement.generation.get(),
            },
        );
        self.trace.emit(
            &self.actor_id,
            &input.tenant_id,
            TraceEvent::ProviderDispatch {
                attempt_id: input.attempt_id.as_uuid().to_string(),
                cred_gen: snapshot.credential.generation.get(),
            },
        );

        Ok(AdmittedDispatch { receipt, cancel_rx })
    }

    /// Executes the restriction/revocation protocol:
    /// 1. Close fence and advance generation BEFORE any authz/placement mutation.
    /// 2. Apply `mutation` (e.g. delete tuples in `PermissionBackend` or transfer lease).
    /// 3. Tick local + peer `BrokerReplicaFence`s (aborting affected in-flight streams)
    ///    and await `ReplicaFenceStore` acknowledgement.
    /// 4. Emit `RevokeAck`.
    /// 5. Optionally reopen the fence at `new_gen` if `reopen_after` is true.
    pub async fn restrict_fence<F, Fut>(
        &self,
        key: &FenceKey,
        peer_fences: &[Arc<BrokerReplicaFence>],
        reopen_after: bool,
        mutation: F,
    ) -> Result<FenceGeneration, AdmissionServiceError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<(), PermissionBackendError>>,
    {
        let new_gen = self.ledger.close_and_advance_fence(key).await?;
        let kind_label = fence_kind_trace_label(&key.kind).to_owned();
        self.trace.emit(
            &self.actor_id,
            &key.tenant_id,
            TraceEvent::FenceClose {
                fence_kind: kind_label.clone(),
                new_gen: new_gen.get(),
            },
        );

        mutation().await?;

        // Drive local fence and any peer replicas supplied (in production, peer
        // replicas run their own short-tick background loop).
        let _ = self.replica_fence.tick_once().await?;
        self.trace.emit(
            &self.actor_id,
            &key.tenant_id,
            TraceEvent::ReplicaFenceAck {
                replica: self.replica_fence.replica_id().to_string(),
                gen: new_gen.get(),
            },
        );
        for peer in peer_fences {
            let _ = peer.tick_once().await?;
            self.trace.emit(
                &self.actor_id,
                &key.tenant_id,
                TraceEvent::ReplicaFenceAck {
                    replica: peer.replica_id().to_string(),
                    gen: new_gen.get(),
                },
            );
        }

        self.replica_fence
            .await_revocation_fence(
                key,
                new_gen,
                Duration::min(BROKER_FENCE_ACK_DEADLINE, Duration::from_secs(2)),
            )
            .await?;

        self.trace.emit(
            &self.actor_id,
            &key.tenant_id,
            TraceEvent::RevokeAck { gen: new_gen.get() },
        );

        if reopen_after {
            self.ledger.reopen_fence(key, new_gen).await?;
            self.trace.emit(
                &self.actor_id,
                &key.tenant_id,
                TraceEvent::FenceReopen {
                    fence_kind: kind_label,
                    gen: new_gen.get(),
                },
            );
        }

        Ok(new_gen)
    }

    pub async fn reconcile_attempt(
        &self,
        attempt_id: AttemptId,
        outcome: AttemptOutcome,
    ) -> Result<(), AdmissionServiceError> {
        self.replica_fence.unregister_stream(attempt_id);
        self.ledger.reconcile_attempt(attempt_id, outcome).await?;
        Ok(())
    }
}

fn build_authz_check(
    input: &AdmitInvocationInput,
) -> Result<(AuthzObject, AuthzRelation, AuthzSubject), AdmissionServiceError> {
    match input.payer {
        PayerSelection::Member {
            account_id,
            credential_id,
        } => Ok((
            AuthzObject {
                object_type: AuthzObjectType::UserCredential,
                id: credential_id.to_string(),
            },
            AuthzRelation::new("use"),
            AuthzSubject {
                subject_type: AuthzSubjectType::Account,
                id: account_id.to_string(),
                relation: None,
            },
        )),
        PayerSelection::Tenant { credential_id } => {
            let subject = match &input.principal {
                InvocationPrincipal::LocalMember { account_id, .. }
                | InvocationPrincipal::TenantAdmin { account_id, .. } => AuthzSubject {
                    subject_type: AuthzSubjectType::Account,
                    id: account_id.to_string(),
                    relation: None,
                },
                InvocationPrincipal::Observation => AuthzSubject {
                    subject_type: AuthzSubjectType::Extension,
                    id: "observation".to_owned(),
                    relation: None,
                },
                InvocationPrincipal::Schedule => AuthzSubject {
                    subject_type: AuthzSubjectType::Extension,
                    id: "schedule".to_owned(),
                    relation: None,
                },
                InvocationPrincipal::Webhook => AuthzSubject {
                    subject_type: AuthzSubjectType::Extension,
                    id: "webhook".to_owned(),
                    relation: None,
                },
                InvocationPrincipal::FederatedRemote { remote_jid } => AuthzSubject {
                    subject_type: AuthzSubjectType::Account,
                    id: remote_jid.clone(),
                    relation: None,
                },
            };
            Ok((
                AuthzObject {
                    object_type: AuthzObjectType::TenantCredential,
                    id: credential_id.to_string(),
                },
                AuthzRelation::new("use"),
                subject,
            ))
        }
    }
}

const fn fence_kind_trace_label(kind: &FenceKind) -> &'static str {
    match kind {
        FenceKind::Credential(_) => "cred",
        FenceKind::RoomOrSpace(_) => "room",
        FenceKind::Placement => "placement",
    }
}
