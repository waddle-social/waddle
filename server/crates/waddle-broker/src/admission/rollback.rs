//! Safe rollback controls for brokered inference.
//!
//! Rollback closes admission, signals cancellation/drain for active streams,
//! and advances affected durable fences while strictly preserving all fence
//! generations, destruction-ledger entries, retired-account markers, and
//! consumed nonces. Restoring plaintext credentials or removing fences is
//! unconditionally forbidden.

use std::collections::HashSet;
use std::sync::{Arc, RwLock};

use thiserror::Error;
use tokio_util::sync::CancellationToken;
use waddle_broker_core::admission::{AdmissionLedgerStore, CredentialKey, FenceKey, RoomScopeKey};
use waddle_broker_core::identity::TenantId;
use waddle_broker_core::placement::FenceGeneration;

use crate::admission::replica_fence::BrokerReplicaFence;
use crate::ledger::DestructionLedgerStore;

/// Granularity at which an emergency rollback disables inference admission.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RollbackScope {
    /// Disable all brokered inference across the deployment.
    AllInference,
    /// Disable inference for a single tenant.
    Tenant(TenantId),
    /// Disable inference for a specific provider/deployment/model profile.
    ProviderProfile {
        provider: String,
        deployment: String,
        model: String,
    },
    /// Disable inference for a single credential scope.
    Credential {
        tenant: TenantId,
        credential: CredentialKey,
    },
    /// Disable inference for a single room scope.
    Room {
        tenant: TenantId,
        room: RoomScopeKey,
    },
}

/// Runtime handles exercised by [`RollbackController::execute_rollback`].
pub struct RollbackTarget<'a, A, L> {
    pub admission_store: &'a A,
    pub replica_fence: Option<&'a BrokerReplicaFence>,
    pub destruction_ledger: &'a L,
    pub cancel_token: Option<&'a CancellationToken>,
}

/// Summary returned after applying a rollback without weakening any fence or
/// destruction invariant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackReport {
    pub scope: RollbackScope,
    pub closed_fences: Vec<(FenceKey, FenceGeneration)>,
    pub replica_streams_canceled: usize,
    pub disabled_scopes_count: usize,
    pub stream_drain_signaled: bool,
}

/// Errors raised when admission is blocked by rollback or an unsafe recovery is attempted.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RollbackError {
    #[error("inference admission is closed by active rollback")]
    AdmissionDisabled,
    #[error("restoring plaintext credentials during rollback is strictly prohibited")]
    PlaintextCredentialRestoreForbidden,
    #[error("removing or lowering fences to recover availability is strictly prohibited")]
    FenceRemovalForbidden,
    #[error("rollback state lock poisoned")]
    StateLockPoisoned,
    #[error("rollback failed to advance durable admission fence: {0}")]
    AdmissionStore(String),
}

#[derive(Debug, Default)]
struct RollbackState {
    global_disabled: bool,
    disabled_tenants: HashSet<TenantId>,
    disabled_profiles: HashSet<(String, String, String)>,
    disabled_credentials: HashSet<(TenantId, CredentialKey)>,
    disabled_rooms: HashSet<(TenantId, RoomScopeKey)>,
}

impl RollbackState {
    fn total_scopes(&self) -> usize {
        usize::from(self.global_disabled)
            + self.disabled_tenants.len()
            + self.disabled_profiles.len()
            + self.disabled_credentials.len()
            + self.disabled_rooms.len()
    }
}

/// Thread-safe controller managing inference admission kill-switches and
/// fence-preserving rollback execution.
#[derive(Clone, Default)]
pub struct RollbackController {
    state: Arc<RwLock<RollbackState>>,
}

impl RollbackController {
    /// Construct an active controller with no scopes rolled back.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Verify that no active rollback disables `tenant` or `(provider, deployment, model)`.
    ///
    /// # Errors
    ///
    /// Returns [`RollbackError::AdmissionDisabled`] if any matching scope is disabled.
    pub fn check_admissible(
        &self,
        tenant: &TenantId,
        provider: &str,
        deployment: &str,
        model: &str,
    ) -> Result<(), RollbackError> {
        let guard = self
            .state
            .read()
            .map_err(|_| RollbackError::StateLockPoisoned)?;
        if guard.global_disabled || guard.disabled_tenants.contains(tenant) {
            return Err(RollbackError::AdmissionDisabled);
        }
        if guard.disabled_profiles.contains(&(
            provider.to_owned(),
            deployment.to_owned(),
            model.to_owned(),
        )) {
            return Err(RollbackError::AdmissionDisabled);
        }
        Ok(())
    }

    /// Execute a fail-closed rollback across admission state, stream cancellation,
    /// and durable fence closure while leaving the destruction ledger untouched.
    ///
    /// # Errors
    ///
    /// Fails if closing/advancing any affected durable fence fails or if the state
    /// lock is poisoned.
    pub async fn execute_rollback<A, L>(
        &self,
        scope: RollbackScope,
        affected_fences: &[FenceKey],
        target: RollbackTarget<'_, A, L>,
    ) -> Result<RollbackReport, RollbackError>
    where
        A: AdmissionLedgerStore + Sync,
        L: DestructionLedgerStore + Sync,
    {
        let disabled_scopes_count = {
            let mut guard = self
                .state
                .write()
                .map_err(|_| RollbackError::StateLockPoisoned)?;
            match &scope {
                RollbackScope::AllInference => guard.global_disabled = true,
                RollbackScope::Tenant(tenant) => {
                    guard.disabled_tenants.insert(tenant.clone());
                }
                RollbackScope::ProviderProfile {
                    provider,
                    deployment,
                    model,
                } => {
                    guard.disabled_profiles.insert((
                        provider.clone(),
                        deployment.clone(),
                        model.clone(),
                    ));
                }
                RollbackScope::Credential { tenant, credential } => {
                    guard
                        .disabled_credentials
                        .insert((tenant.clone(), credential.clone()));
                }
                RollbackScope::Room { tenant, room } => {
                    guard.disabled_rooms.insert((tenant.clone(), room.clone()));
                }
            }
            guard.total_scopes()
        };

        let mut stream_drain_signaled = false;
        if let Some(cancel_token) = target.cancel_token {
            cancel_token.cancel();
            stream_drain_signaled = true;
        }

        let mut closed_fences = Vec::with_capacity(affected_fences.len());
        for key in affected_fences {
            let new_gen = target
                .admission_store
                .close_and_advance_fence(key)
                .await
                .map_err(|err| RollbackError::AdmissionStore(err.to_string()))?;
            closed_fences.push((key.clone(), new_gen));
        }

        let replica_streams_canceled = if let Some(replica_fence) = target.replica_fence {
            let (canceled, _acks) = replica_fence
                .tick_once()
                .await
                .map_err(|err| RollbackError::AdmissionStore(err.to_string()))?;
            canceled.len()
        } else {
            0
        };

        // Never delete or weaken destruction ledger records during rollback.
        let _ = target.destruction_ledger;

        Ok(RollbackReport {
            scope,
            closed_fences,
            replica_streams_canceled,
            disabled_scopes_count,
            stream_drain_signaled,
        })
    }

    /// Explicitly reject any operator workflow attempting to restore plaintext
    /// provider credentials to recover availability.
    ///
    /// # Errors
    ///
    /// Always returns [`RollbackError::PlaintextCredentialRestoreForbidden`].
    pub fn reject_plaintext_credential_restore(&self) -> Result<(), RollbackError> {
        Err(RollbackError::PlaintextCredentialRestoreForbidden)
    }

    /// Explicitly reject any operator workflow attempting to remove or lower
    /// durable fences to recover availability.
    ///
    /// # Errors
    ///
    /// Always returns [`RollbackError::FenceRemovalForbidden`].
    pub fn reject_fence_removal(&self) -> Result<(), RollbackError> {
        Err(RollbackError::FenceRemovalForbidden)
    }
}
