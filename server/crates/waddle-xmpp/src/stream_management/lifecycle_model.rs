//! Inactive model of the P3.3 Stream Management lifecycle and resume fence.
//!
//! This fixture is reachable only from unit tests or the explicit `test-utils`
//! feature. It serializes transitions with `&mut self`; it does not implement a
//! durable compare-and-set, endpoint, socket, writer, or production registry.
//! Receipts are equality evidence supplied by tests, not proof of external
//! commits. The model stores only opaque identity references and revisions.

use std::collections::BTreeSet;
use std::collections::VecDeque;

use super::frontier_model::{
    AckError, AckKind, AckOutcome, AppendError, AttemptError, AttemptOutcome, FrameWriteError,
    FrameWriteResult, FrontierModel, IngressError, InitError,
};
use crate::ownership::{BindingRef, ConnectionRef};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionId(u64);

impl SessionId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PrincipalId(u64);

impl PrincipalId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClaimId(u64);

impl ClaimId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OperationId(u64);

impl OperationId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TombstoneId(u64);

impl TombstoneId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
}

/// Durable session fence value. It is not a transport generation or a local
/// counter, and it refuses to advance past `u64::MAX`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionGeneration(u64);

impl SessionGeneration {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn value(self) -> u64 {
        self.0
    }

    pub fn checked_next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthenticationStatus {
    Current,
    Revoked,
}

/// Current authentication evidence supplied for one resume attempt. It carries
/// no credentials or permission snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthenticationProof {
    pub principal: PrincipalId,
    pub identity_revision: u64,
    pub status: AuthenticationStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResourceContinuity {
    pub resource_revision: u64,
    pub occupancy_revision: u64,
    pub presence_revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnableConfig {
    pub session: SessionId,
    pub principal: PrincipalId,
    pub identity_revision: u64,
    pub claim: ClaimId,
    pub generation: SessionGeneration,
    pub binding: BindingRef,
    pub resources: ResourceContinuity,
    pub operation: OperationId,
    pub frontier_capacity: u32,
}

/// Exact current owner fence, including both the binding token and its socket.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LifecycleFence {
    pub session: SessionId,
    pub principal: PrincipalId,
    pub identity_revision: u64,
    pub claim: ClaimId,
    pub generation: SessionGeneration,
    pub binding: BindingRef,
    pub connection: ConnectionRef,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleStatus {
    EnablingLive,
    Live,
    DetachedResumable,
    TransferHydrating,
    Terminal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallationStatus {
    Installing,
    Published,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RepairResponsibility {
    EnableCommitUnknown(OperationId),
    TransferCommitUnknown(OperationId),
    TombstoneCustodyPending(TombstoneId),
    TombstoneCustodyUnknown(TombstoneId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CommitStatus {
    Pending,
    Unknown,
    Committed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnableCommitReceipt {
    pub operation: OperationId,
    pub fence: LifecycleFence,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResumeRequest {
    pub authentication: AuthenticationProof,
    pub expected: LifecycleFence,
    pub new_binding: BindingRef,
    pub new_claim: ClaimId,
    pub acknowledged_h: u32,
    pub transfer: OperationId,
    pub endpoint_installation: OperationId,
    pub hydration: OperationId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferCommitReceipt {
    pub transfer: OperationId,
    pub fence: LifecycleFence,
    pub peer_acked: u64,
    pub endpoint_installation: OperationId,
    pub hydration: OperationId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicationReceipt {
    pub transfer: OperationId,
    pub fence: LifecycleFence,
    pub installation: OperationId,
    pub resources: ResourceContinuity,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HydrationReceipt {
    pub transfer: OperationId,
    pub fence: LifecycleFence,
    pub hydration: OperationId,
    pub resources: ResourceContinuity,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferAbortReceipt {
    pub transfer: OperationId,
    pub restored_fence: LifecycleFence,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferPlan {
    pub transfer: OperationId,
    pub commit: TransferCommitReceipt,
    pub publication: PublicationReceipt,
    pub hydration: HydrationReceipt,
    pub abort: TransferAbortReceipt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalReason {
    Expired,
    Replaced,
    ExplicitlyClosed,
    EnableRejected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TombstoneRetention {
    Pending,
    Unknown,
    Retained,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TombstoneEvidence {
    pub id: TombstoneId,
    pub fence: LifecycleFence,
    pub reason: TerminalReason,
    pub final_peer_acked: u64,
    pub final_exposed: u64,
    pub final_appended: u64,
    pub final_ingress: u64,
    pub retention: TombstoneRetention,
    pub cleanup_complete: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TombstoneCustodyReceipt {
    pub id: TombstoneId,
    pub fence: LifecycleFence,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CleanupReceipt {
    pub fence: LifecycleFence,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CleanupOutcome {
    Applied,
    StaleIgnored,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputDisposition {
    Buffered,
    Contiguous,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ModelError {
    WrongState,
    StaleFence,
    IdentityMismatch,
    IdentityRevoked,
    GenerationExhausted,
    SameBinding,
    ReceiptMismatch,
    CommitNotConfirmed,
    TransferUnknown,
    InputUnavailable,
    StaleBinding,
    Append(AppendError),
    Attempt(AttemptError),
    FrameWrite(FrameWriteError),
    Input(IngressError),
    Ack(AckError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TransferState {
    plan: TransferPlan,
    previous_frontier: FrontierModel,
    commit: CommitStatus,
    publication_received: bool,
    hydration_received: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SmLifecycleModel {
    session: SessionId,
    principal: PrincipalId,
    identity_revision: u64,
    claim: ClaimId,
    generation: SessionGeneration,
    binding: BindingRef,
    resources: ResourceContinuity,
    state: LifecycleStatus,
    enable_operation: OperationId,
    enabled_effect_ready: bool,
    transfer: Option<TransferState>,
    pending_ingress: VecDeque<u64>,
    frontier: FrontierModel,
    repair: BTreeSet<RepairResponsibility>,
    tombstone: Option<TombstoneEvidence>,
}

impl SmLifecycleModel {
    pub fn enabling_live(config: EnableConfig) -> Result<Self, InitError> {
        let frontier = FrontierModel::new(config.frontier_capacity)?;
        Ok(Self {
            session: config.session,
            principal: config.principal,
            identity_revision: config.identity_revision,
            claim: config.claim,
            generation: config.generation,
            binding: config.binding,
            resources: config.resources,
            state: LifecycleStatus::EnablingLive,
            enable_operation: config.operation,
            enabled_effect_ready: false,
            transfer: None,
            pending_ingress: VecDeque::new(),
            frontier,
            repair: BTreeSet::new(),
            tombstone: None,
        })
    }

    pub fn state(&self) -> LifecycleStatus {
        self.state
    }

    pub fn fence(&self) -> LifecycleFence {
        LifecycleFence {
            session: self.session,
            principal: self.principal,
            identity_revision: self.identity_revision,
            claim: self.claim,
            generation: self.generation,
            binding: self.binding.clone(),
            connection: self.binding.connection_ref().clone(),
        }
    }

    pub fn resources(&self) -> ResourceContinuity {
        self.resources
    }

    pub fn frontier(&self) -> &FrontierModel {
        &self.frontier
    }

    pub fn enabled_effect_ready(&self) -> bool {
        self.enabled_effect_ready
    }

    pub fn repair_responsibilities(&self) -> impl Iterator<Item = RepairResponsibility> + '_ {
        self.repair.iter().copied()
    }

    pub fn tombstone(&self) -> Option<&TombstoneEvidence> {
        self.tombstone.as_ref()
    }

    pub fn endpoint_resource_installation(&self) -> Option<InstallationStatus> {
        let transfer = self.transfer.as_ref()?;
        Some(if transfer.publication_received {
            InstallationStatus::Published
        } else {
            InstallationStatus::Installing
        })
    }

    pub fn expected_enable_commit_receipt(&self) -> EnableCommitReceipt {
        EnableCommitReceipt {
            operation: self.enable_operation,
            fence: self.fence(),
        }
    }

    pub fn record_enable_commit_unknown(&mut self) -> Result<(), ModelError> {
        if self.state != LifecycleStatus::EnablingLive {
            return Err(ModelError::WrongState);
        }
        self.repair
            .insert(RepairResponsibility::EnableCommitUnknown(
                self.enable_operation,
            ));
        Ok(())
    }

    /// A durable-commit receipt is required before the enabled effect becomes
    /// available or buffered input is released.
    pub fn commit_enable(&mut self, receipt: &EnableCommitReceipt) -> Result<(), ModelError> {
        if self.state != LifecycleStatus::EnablingLive {
            return Err(ModelError::WrongState);
        }
        if *receipt != self.expected_enable_commit_receipt() {
            return Err(ModelError::ReceiptMismatch);
        }
        let mut frontier = self.frontier.clone();
        for ordinal in &self.pending_ingress {
            frontier
                .record_contiguous_ingress(*ordinal)
                .map_err(ModelError::Input)?;
        }
        self.frontier = frontier;
        self.pending_ingress.clear();
        self.repair
            .remove(&RepairResponsibility::EnableCommitUnknown(
                self.enable_operation,
            ));
        self.enabled_effect_ready = true;
        self.state = LifecycleStatus::Live;
        Ok(())
    }

    pub fn detach(&mut self, expected: &LifecycleFence) -> Result<(), ModelError> {
        if self.state != LifecycleStatus::Live {
            return Err(ModelError::WrongState);
        }
        if *expected != self.fence() {
            return Err(ModelError::StaleFence);
        }
        self.state = LifecycleStatus::DetachedResumable;
        self.enabled_effect_ready = false;
        Ok(())
    }

    /// Atomically plans the ACK floor, claim/generation advance, and endpoint
    /// installation state. No replay or ingress is available until the exact
    /// commit, publication, and hydration receipts complete the transfer.
    pub fn begin_resume(&mut self, request: ResumeRequest) -> Result<TransferPlan, ModelError> {
        if self.state != LifecycleStatus::DetachedResumable {
            return Err(ModelError::WrongState);
        }
        if request.authentication.status == AuthenticationStatus::Revoked {
            return Err(ModelError::IdentityRevoked);
        }
        if request.authentication.principal != self.principal
            || request.authentication.identity_revision != self.identity_revision
            || request.expected.principal != self.principal
            || request.expected.session != self.session
        {
            return Err(ModelError::IdentityMismatch);
        }
        if request.expected != self.fence() {
            return Err(ModelError::StaleFence);
        }
        if request.new_binding == self.binding {
            return Err(ModelError::SameBinding);
        }
        let generation = self
            .generation
            .checked_next()
            .ok_or(ModelError::GenerationExhausted)?;
        let mut frontier = self.frontier.clone();
        frontier
            .acknowledge(request.acknowledged_h, AckKind::Resume)
            .map_err(ModelError::Ack)?;

        let previous_frontier = self.frontier.clone();
        let restored_fence = self.fence();
        let new_fence = LifecycleFence {
            session: self.session,
            principal: self.principal,
            identity_revision: self.identity_revision,
            claim: request.new_claim,
            generation,
            connection: request.new_binding.connection_ref().clone(),
            binding: request.new_binding.clone(),
        };
        let plan = TransferPlan {
            transfer: request.transfer,
            commit: TransferCommitReceipt {
                transfer: request.transfer,
                fence: new_fence.clone(),
                peer_acked: frontier.peer_acked(),
                endpoint_installation: request.endpoint_installation,
                hydration: request.hydration,
            },
            publication: PublicationReceipt {
                transfer: request.transfer,
                fence: new_fence.clone(),
                installation: request.endpoint_installation,
                resources: self.resources,
            },
            hydration: HydrationReceipt {
                transfer: request.transfer,
                fence: new_fence.clone(),
                hydration: request.hydration,
                resources: self.resources,
            },
            abort: TransferAbortReceipt {
                transfer: request.transfer,
                restored_fence,
            },
        };

        self.claim = request.new_claim;
        self.generation = generation;
        self.binding = request.new_binding;
        self.frontier = frontier;
        self.transfer = Some(TransferState {
            plan: plan.clone(),
            previous_frontier,
            commit: CommitStatus::Pending,
            publication_received: false,
            hydration_received: false,
        });
        self.pending_ingress.clear();
        self.state = LifecycleStatus::TransferHydrating;
        Ok(plan)
    }

    pub fn record_transfer_commit_unknown(
        &mut self,
        transfer: OperationId,
    ) -> Result<(), ModelError> {
        let state = self.transfer.as_mut().ok_or(ModelError::TransferUnknown)?;
        if self.state != LifecycleStatus::TransferHydrating || state.plan.transfer != transfer {
            return Err(ModelError::TransferUnknown);
        }
        state.commit = CommitStatus::Unknown;
        self.repair
            .insert(RepairResponsibility::TransferCommitUnknown(transfer));
        Ok(())
    }

    pub fn commit_transfer(&mut self, receipt: &TransferCommitReceipt) -> Result<(), ModelError> {
        if self.state != LifecycleStatus::TransferHydrating {
            return Err(ModelError::WrongState);
        }
        let state = self.transfer.as_mut().ok_or(ModelError::TransferUnknown)?;
        if *receipt != state.plan.commit {
            return Err(ModelError::ReceiptMismatch);
        }
        state.commit = CommitStatus::Committed;
        self.repair
            .remove(&RepairResponsibility::TransferCommitUnknown(
                receipt.transfer,
            ));
        Ok(())
    }

    pub fn record_publication(&mut self, receipt: &PublicationReceipt) -> Result<bool, ModelError> {
        if self.state != LifecycleStatus::TransferHydrating {
            return Err(ModelError::WrongState);
        }
        let state = self.transfer.as_mut().ok_or(ModelError::TransferUnknown)?;
        if state.commit != CommitStatus::Committed {
            return Err(ModelError::CommitNotConfirmed);
        }
        if *receipt != state.plan.publication {
            return Err(ModelError::ReceiptMismatch);
        }
        state.publication_received = true;
        self.finish_transfer_if_ready()
    }

    pub fn record_hydration(&mut self, receipt: &HydrationReceipt) -> Result<bool, ModelError> {
        self.record_transfer_receipt_hydration(receipt)
    }

    fn record_transfer_receipt_hydration(
        &mut self,
        hydration: &HydrationReceipt,
    ) -> Result<bool, ModelError> {
        if self.state != LifecycleStatus::TransferHydrating {
            return Err(ModelError::WrongState);
        }
        let state = self.transfer.as_mut().ok_or(ModelError::TransferUnknown)?;
        if state.commit != CommitStatus::Committed {
            return Err(ModelError::CommitNotConfirmed);
        }
        if *hydration != state.plan.hydration {
            return Err(ModelError::ReceiptMismatch);
        }
        state.hydration_received = true;
        self.finish_transfer_if_ready()
    }

    fn finish_transfer_if_ready(&mut self) -> Result<bool, ModelError> {
        let state = self.transfer.as_ref().ok_or(ModelError::TransferUnknown)?;
        if state.commit != CommitStatus::Committed
            || !state.publication_received
            || !state.hydration_received
        {
            return Ok(false);
        }
        let mut frontier = self.frontier.clone();
        for ordinal in &self.pending_ingress {
            frontier
                .record_contiguous_ingress(*ordinal)
                .map_err(ModelError::Input)?;
        }
        self.frontier = frontier;
        self.pending_ingress.clear();
        self.transfer = None;
        self.state = LifecycleStatus::Live;
        Ok(true)
    }

    /// Compensates only a transfer with a known, not-committed result and no
    /// endpoint publication or hydration receipt.
    pub fn compensate_transfer(
        &mut self,
        receipt: &TransferAbortReceipt,
    ) -> Result<(), ModelError> {
        if self.state != LifecycleStatus::TransferHydrating {
            return Err(ModelError::WrongState);
        }
        let state = self.transfer.as_ref().ok_or(ModelError::TransferUnknown)?;
        if *receipt != state.plan.abort {
            return Err(ModelError::ReceiptMismatch);
        }
        if state.commit != CommitStatus::Pending
            || state.publication_received
            || state.hydration_received
        {
            return Err(ModelError::CommitNotConfirmed);
        }
        self.claim = receipt.restored_fence.claim;
        self.generation = receipt.restored_fence.generation;
        self.binding = receipt.restored_fence.binding.clone();
        self.frontier = state.previous_frontier.clone();
        self.transfer = None;
        self.pending_ingress.clear();
        self.state = LifecycleStatus::DetachedResumable;
        Ok(())
    }

    pub fn append_outbound(&mut self) -> Result<u64, ModelError> {
        if self.state != LifecycleStatus::Live {
            return Err(ModelError::WrongState);
        }
        self.frontier.append().map_err(ModelError::Append)
    }

    pub fn record_write_attempt(&mut self, ordinal: u64) -> Result<AttemptOutcome, ModelError> {
        if self.state != LifecycleStatus::Live {
            return Err(ModelError::WrongState);
        }
        self.frontier
            .record_assumed_committed_attempt(ordinal)
            .map_err(ModelError::Attempt)
    }

    pub fn record_frame_write_result(
        &mut self,
        ordinal: u64,
        result: FrameWriteResult,
    ) -> Result<(), ModelError> {
        if self.state != LifecycleStatus::Live {
            return Err(ModelError::WrongState);
        }
        self.frontier
            .record_frame_write_result(ordinal, result)
            .map_err(ModelError::FrameWrite)
    }

    pub fn replay_ordinals(&self) -> Option<Vec<u64>> {
        (self.state == LifecycleStatus::Live).then(|| self.frontier.replay().collect())
    }

    pub fn acknowledge_live(&mut self, h: u32) -> Result<AckOutcome, ModelError> {
        if self.state != LifecycleStatus::Live {
            return Err(ModelError::WrongState);
        }
        self.frontier
            .acknowledge(h, AckKind::Live)
            .map_err(ModelError::Ack)
    }

    pub fn receive_input(
        &mut self,
        binding: &BindingRef,
        ordinal: u64,
    ) -> Result<InputDisposition, ModelError> {
        if !matches!(
            self.state,
            LifecycleStatus::EnablingLive
                | LifecycleStatus::Live
                | LifecycleStatus::TransferHydrating
        ) {
            return Err(ModelError::InputUnavailable);
        }
        if *binding != self.binding {
            return Err(ModelError::StaleBinding);
        }
        let buffered = self.state != LifecycleStatus::Live;
        if buffered {
            let queued = u64::try_from(self.pending_ingress.len())
                .map_err(|_| ModelError::Input(IngressError::OrdinalExhausted))?;
            let expected = self
                .frontier
                .ingress()
                .checked_add(queued)
                .and_then(|next| next.checked_add(1))
                .ok_or(ModelError::Input(IngressError::OrdinalExhausted))?;
            if ordinal != expected {
                return Err(ModelError::Input(IngressError::NonContiguous {
                    expected,
                    actual: ordinal,
                }));
            }
            self.pending_ingress.push_back(ordinal);
            return Ok(InputDisposition::Buffered);
        }
        self.frontier
            .record_contiguous_ingress(ordinal)
            .map_err(ModelError::Input)?;
        Ok(InputDisposition::Contiguous)
    }

    pub fn terminalize(
        &mut self,
        expected: &LifecycleFence,
        id: TombstoneId,
        reason: TerminalReason,
    ) -> Result<(), ModelError> {
        if self.state == LifecycleStatus::Terminal
            || self.state == LifecycleStatus::TransferHydrating
        {
            return Err(ModelError::WrongState);
        }
        if *expected != self.fence() {
            return Err(ModelError::StaleFence);
        }
        let fence = self.fence();
        self.tombstone = Some(TombstoneEvidence {
            id,
            fence,
            reason,
            final_peer_acked: self.frontier.peer_acked(),
            final_exposed: self.frontier.exposed(),
            final_appended: self.frontier.appended(),
            final_ingress: self.frontier.ingress(),
            retention: TombstoneRetention::Pending,
            cleanup_complete: false,
        });
        self.repair
            .insert(RepairResponsibility::TombstoneCustodyPending(id));
        self.state = LifecycleStatus::Terminal;
        self.enabled_effect_ready = false;
        Ok(())
    }

    pub fn mark_tombstone_custody_unknown(&mut self, id: TombstoneId) -> Result<(), ModelError> {
        let tombstone = self.tombstone.as_mut().ok_or(ModelError::WrongState)?;
        if self.state != LifecycleStatus::Terminal || tombstone.id != id {
            return Err(ModelError::ReceiptMismatch);
        }
        tombstone.retention = TombstoneRetention::Unknown;
        self.repair
            .remove(&RepairResponsibility::TombstoneCustodyPending(id));
        self.repair
            .insert(RepairResponsibility::TombstoneCustodyUnknown(id));
        Ok(())
    }

    pub fn retain_tombstone(
        &mut self,
        receipt: &TombstoneCustodyReceipt,
    ) -> Result<(), ModelError> {
        let tombstone = self.tombstone.as_mut().ok_or(ModelError::WrongState)?;
        if self.state != LifecycleStatus::Terminal
            || receipt.id != tombstone.id
            || receipt.fence != tombstone.fence
        {
            return Err(ModelError::ReceiptMismatch);
        }
        tombstone.retention = TombstoneRetention::Retained;
        self.repair
            .remove(&RepairResponsibility::TombstoneCustodyUnknown(receipt.id));
        self.repair
            .remove(&RepairResponsibility::TombstoneCustodyPending(receipt.id));
        Ok(())
    }

    pub fn apply_late_cleanup(
        &mut self,
        receipt: &CleanupReceipt,
    ) -> Result<CleanupOutcome, ModelError> {
        if self.state != LifecycleStatus::Terminal {
            return Ok(CleanupOutcome::StaleIgnored);
        }
        let tombstone = self.tombstone.as_mut().ok_or(ModelError::WrongState)?;
        if receipt.fence != tombstone.fence {
            return Ok(CleanupOutcome::StaleIgnored);
        }
        tombstone.cleanup_complete = true;
        Ok(CleanupOutcome::Applied)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AttemptOutcome, AuthenticationProof, AuthenticationStatus, ClaimId, CleanupOutcome,
        CleanupReceipt, EnableConfig, HydrationReceipt, InputDisposition, LifecycleStatus,
        ModelError, OperationId, PrincipalId, PublicationReceipt, RepairResponsibility,
        ResourceContinuity, ResumeRequest, SessionGeneration, SessionId, SmLifecycleModel,
        TerminalReason, TombstoneCustodyReceipt, TombstoneId, TombstoneRetention,
    };
    use crate::ownership::{BindingRef, ConnectionRef, NodeLeaseIdentity};
    use crate::stream_management::frontier_model::{AckError, AckOutcome, FrameWriteResult};

    fn new_binding() -> (ConnectionRef, BindingRef) {
        let connection = ConnectionRef::fresh(NodeLeaseIdentity::local_process());
        let binding = BindingRef::fresh(connection.clone());
        (connection, binding)
    }

    fn resources() -> ResourceContinuity {
        ResourceContinuity {
            resource_revision: 17,
            occupancy_revision: 23,
            presence_revision: 41,
        }
    }

    fn enabling_model(generation: u64) -> SmLifecycleModel {
        let (_, binding) = new_binding();
        SmLifecycleModel::enabling_live(EnableConfig {
            session: SessionId::new(101),
            principal: PrincipalId::new(202),
            identity_revision: 3,
            claim: ClaimId::new(303),
            generation: SessionGeneration::new(generation),
            binding,
            resources: resources(),
            operation: OperationId::new(404),
            frontier_capacity: 8,
        })
        .expect("valid frontier")
    }

    fn make_live(model: &mut SmLifecycleModel) {
        let receipt = model.expected_enable_commit_receipt();
        model.commit_enable(&receipt).expect("enable commit");
    }

    fn detached_model() -> SmLifecycleModel {
        let mut model = enabling_model(7);
        make_live(&mut model);
        let fence = model.fence();
        model.detach(&fence).expect("detach live session");
        model
    }

    fn resume_request(
        model: &SmLifecycleModel,
        new_binding: BindingRef,
        claim: u64,
        transfer: u64,
    ) -> ResumeRequest {
        ResumeRequest {
            authentication: AuthenticationProof {
                principal: PrincipalId::new(202),
                identity_revision: 3,
                status: AuthenticationStatus::Current,
            },
            expected: model.fence(),
            new_binding,
            new_claim: ClaimId::new(claim),
            acknowledged_h: model.frontier().peer_acked() as u32,
            transfer: OperationId::new(transfer),
            endpoint_installation: OperationId::new(transfer + 100),
            hydration: OperationId::new(transfer + 200),
        }
    }

    fn complete_transfer(model: &mut SmLifecycleModel, plan: &super::TransferPlan) {
        model
            .commit_transfer(&plan.commit)
            .expect("atomic commit receipt");
        assert!(!model
            .record_publication(&plan.publication)
            .expect("publication receipt"));
        assert!(model
            .record_hydration(&plan.hydration)
            .expect("hydration receipt"));
    }

    #[test]
    fn transition_table_requires_receipts_at_each_activation_boundary() {
        let mut model = enabling_model(7);
        assert_eq!(model.state(), LifecycleStatus::EnablingLive);
        assert!(!model.enabled_effect_ready());
        assert_eq!(model.replay_ordinals(), None);
        assert_eq!(model.append_outbound(), Err(ModelError::WrongState));
        let initial_binding = model.fence().binding.clone();
        assert_eq!(
            model.receive_input(&initial_binding, 1),
            Ok(InputDisposition::Buffered)
        );
        assert_eq!(model.frontier().ingress(), 0);

        let receipt = model.expected_enable_commit_receipt();
        model
            .commit_enable(&receipt)
            .expect("durable commit receipt");
        assert_eq!(model.state(), LifecycleStatus::Live);
        assert!(model.enabled_effect_ready());
        assert_eq!(model.frontier().ingress(), 1);

        let old_fence = model.fence();
        model.detach(&old_fence).expect("detach");
        assert_eq!(model.state(), LifecycleStatus::DetachedResumable);
        assert_eq!(model.replay_ordinals(), None);

        let (_, new_binding) = new_binding();
        let plan = model
            .begin_resume(resume_request(&model, new_binding, 304, 500))
            .expect("planned transfer");
        assert_eq!(model.state(), LifecycleStatus::TransferHydrating);
        assert_eq!(
            model.endpoint_resource_installation(),
            Some(super::InstallationStatus::Installing)
        );
        assert_eq!(model.replay_ordinals(), None);
        assert_eq!(model.append_outbound(), Err(ModelError::WrongState));
        assert_eq!(
            model.receive_input(&plan.commit.fence.binding, 2),
            Ok(InputDisposition::Buffered)
        );
        assert_eq!(model.frontier().ingress(), 1);

        complete_transfer(&mut model, &plan);
        assert_eq!(model.state(), LifecycleStatus::Live);
        assert_eq!(model.endpoint_resource_installation(), None);
        assert_eq!(model.frontier().ingress(), 2);
        assert_eq!(model.resources(), resources());
        assert!(model.replay_ordinals().is_some());

        let current = model.fence();
        model
            .terminalize(
                &current,
                TombstoneId::new(700),
                TerminalReason::ExplicitlyClosed,
            )
            .expect("terminal transition");
        assert_eq!(model.state(), LifecycleStatus::Terminal);
        assert!(model.tombstone().is_some());
        assert_eq!(model.replay_ordinals(), None);
    }

    #[test]
    fn simultaneous_resume_and_displaced_owner_leave_the_winner_unchanged() {
        let mut model = detached_model();
        let stale_fence = model.fence();
        let (_, winner_binding) = new_binding();
        let winner = model
            .begin_resume(resume_request(&model, winner_binding, 304, 500))
            .expect("first compare-and-set wins");
        let after_winner = model.clone();

        let (_, losing_binding) = new_binding();
        let mut losing_request = resume_request(&model, losing_binding, 305, 501);
        losing_request.expected = stale_fence.clone();
        assert_eq!(
            model.begin_resume(losing_request),
            Err(ModelError::WrongState)
        );
        assert_eq!(model, after_winner);
        assert_eq!(winner.commit.fence.claim, ClaimId::new(304));

        complete_transfer(&mut model, &winner);
        let current = model.clone();
        assert_eq!(
            model.terminalize(
                &stale_fence,
                TombstoneId::new(701),
                TerminalReason::Replaced,
            ),
            Err(ModelError::StaleFence)
        );
        assert_eq!(model, current);
    }

    #[test]
    fn principal_mismatch_and_revocation_reject_before_mutation() {
        for (principal, status, expected) in [
            (
                PrincipalId::new(999),
                AuthenticationStatus::Current,
                ModelError::IdentityMismatch,
            ),
            (
                PrincipalId::new(202),
                AuthenticationStatus::Revoked,
                ModelError::IdentityRevoked,
            ),
        ] {
            let mut model = detached_model();
            let (_, binding) = new_binding();
            let mut request = resume_request(&model, binding, 304, 500);
            request.authentication = AuthenticationProof {
                principal,
                identity_revision: 3,
                status,
            };
            let before = model.clone();
            assert_eq!(model.begin_resume(request), Err(expected));
            assert_eq!(model, before);
        }
    }

    #[test]
    fn stale_session_claim_generation_binding_and_connection_are_non_mutating() {
        for stale_field in 0..5 {
            let mut model = detached_model();
            let (_, binding) = new_binding();
            let mut request = resume_request(&model, binding, 304, 500);
            let mut expected_error = ModelError::StaleFence;
            match stale_field {
                0 => {
                    request.expected.session = SessionId::new(999);
                    expected_error = ModelError::IdentityMismatch;
                }
                1 => request.expected.claim = ClaimId::new(999),
                2 => request.expected.generation = SessionGeneration::new(999),
                3 => {
                    let (_, stale_binding) = new_binding();
                    request.expected.binding = stale_binding;
                }
                _ => {
                    let (stale_connection, _) = new_binding();
                    request.expected.connection = stale_connection;
                }
            }
            let before = model.clone();
            assert_eq!(model.begin_resume(request), Err(expected_error));
            assert_eq!(model, before);
        }
    }

    #[test]
    fn unknown_enable_commit_keeps_repair_and_holds_pipelined_input() {
        let mut model = enabling_model(7);
        let binding = model.fence().binding;
        assert_eq!(
            model.receive_input(&binding, 1),
            Ok(InputDisposition::Buffered)
        );
        model
            .record_enable_commit_unknown()
            .expect("uncertain result retained");
        assert_eq!(model.state(), LifecycleStatus::EnablingLive);
        assert!(!model.enabled_effect_ready());
        assert_eq!(model.frontier().ingress(), 0);
        assert_eq!(
            model.repair_responsibilities().collect::<Vec<_>>(),
            vec![RepairResponsibility::EnableCommitUnknown(OperationId::new(
                404
            ))]
        );

        let expected = model.expected_enable_commit_receipt();
        let mut wrong = expected.clone();
        wrong.operation = OperationId::new(405);
        let before = model.clone();
        assert_eq!(
            model.commit_enable(&wrong),
            Err(ModelError::ReceiptMismatch)
        );
        assert_eq!(model, before);
        model
            .commit_enable(&expected)
            .expect("read-back commit receipt");
        assert!(model.enabled_effect_ready());
        assert!(model.repair_responsibilities().next().is_none());
        assert_eq!(model.frontier().ingress(), 1);
    }

    #[test]
    fn transfer_unknown_commit_blocks_publication_hydration_and_effects() {
        let mut model = detached_model();
        let (_, binding) = new_binding();
        let plan = model
            .begin_resume(resume_request(&model, binding, 304, 500))
            .expect("planned transfer");
        model
            .record_transfer_commit_unknown(plan.transfer)
            .expect("unknown commit remains repairable");
        let before = model.clone();
        assert_eq!(
            model.record_publication(&plan.publication),
            Err(ModelError::CommitNotConfirmed)
        );
        assert_eq!(model, before);
        assert_eq!(model.replay_ordinals(), None);
        assert_eq!(
            model.repair_responsibilities().collect::<Vec<_>>(),
            vec![RepairResponsibility::TransferCommitUnknown(plan.transfer)]
        );

        model
            .commit_transfer(&plan.commit)
            .expect("commit read-back");
        assert!(model.repair_responsibilities().next().is_none());
        assert!(!model.record_publication(&plan.publication).unwrap());
        assert_eq!(
            model.endpoint_resource_installation(),
            Some(super::InstallationStatus::Published)
        );
        assert!(model.record_hydration(&plan.hydration).unwrap());
        assert_eq!(model.state(), LifecycleStatus::Live);
    }

    #[test]
    fn same_socket_fallback_gets_a_new_binding_and_durable_generation() {
        let mut detached = detached_model();
        let old_fence = detached.fence();
        let mut rejected = resume_request(&detached, old_fence.binding.clone(), 304, 500);
        rejected.new_claim = ClaimId::new(304);
        let before = detached.clone();
        assert_eq!(
            detached.begin_resume(rejected),
            Err(ModelError::SameBinding)
        );
        assert_eq!(detached, before);

        let fallback_binding = BindingRef::fresh(old_fence.connection.clone());
        assert_ne!(fallback_binding, old_fence.binding);
        assert_eq!(fallback_binding.connection_ref(), &old_fence.connection);
        let fallback = SmLifecycleModel::enabling_live(EnableConfig {
            session: SessionId::new(102),
            principal: old_fence.principal,
            identity_revision: old_fence.identity_revision,
            claim: ClaimId::new(401),
            generation: old_fence
                .generation
                .checked_next()
                .expect("generation increments"),
            binding: fallback_binding.clone(),
            resources: resources(),
            operation: OperationId::new(402),
            frontier_capacity: 8,
        })
        .expect("new session on same socket");
        assert_eq!(fallback.state(), LifecycleStatus::EnablingLive);
        assert_eq!(fallback.fence().connection, old_fence.connection);
        assert_ne!(fallback.fence().binding, old_fence.binding);
    }

    #[test]
    fn stale_cleanup_and_late_compensation_cannot_revert_a_completed_resume() {
        let mut model = detached_model();
        let old_fence = model.fence();
        let (_, binding) = new_binding();
        let plan = model
            .begin_resume(resume_request(&model, binding, 304, 500))
            .expect("planned transfer");
        complete_transfer(&mut model, &plan);
        let after_resume = model.clone();
        assert_eq!(
            model.apply_late_cleanup(&CleanupReceipt { fence: old_fence }),
            Ok(CleanupOutcome::StaleIgnored)
        );
        assert_eq!(model, after_resume);
        assert_eq!(
            model.compensate_transfer(&plan.abort),
            Err(ModelError::WrongState)
        );
        assert_eq!(model, after_resume);
    }

    #[test]
    fn tombstone_retention_unknown_commit_and_exact_cleanup_keep_evidence() {
        let mut model = detached_model();
        let fence = model.fence();
        let tombstone_id = TombstoneId::new(701);
        model
            .terminalize(&fence, tombstone_id, TerminalReason::Expired)
            .expect("retain terminal evidence");
        model
            .mark_tombstone_custody_unknown(tombstone_id)
            .expect("custody is unknown");
        assert_eq!(
            model.tombstone().unwrap().retention,
            TombstoneRetention::Unknown
        );
        assert!(model
            .repair_responsibilities()
            .any(|item| { item == RepairResponsibility::TombstoneCustodyUnknown(tombstone_id) }));

        let before = model.clone();
        let wrong_receipt = TombstoneCustodyReceipt {
            id: TombstoneId::new(702),
            fence: fence.clone(),
        };
        assert_eq!(
            model.retain_tombstone(&wrong_receipt),
            Err(ModelError::ReceiptMismatch)
        );
        assert_eq!(model, before);

        model
            .retain_tombstone(&TombstoneCustodyReceipt {
                id: tombstone_id,
                fence: fence.clone(),
            })
            .expect("exact retained custody receipt");
        assert_eq!(
            model.tombstone().unwrap().retention,
            TombstoneRetention::Retained
        );
        assert!(model.repair_responsibilities().next().is_none());
        assert_eq!(
            model.apply_late_cleanup(&CleanupReceipt { fence }),
            Ok(CleanupOutcome::Applied)
        );
        assert!(model.tombstone().unwrap().cleanup_complete);
        assert_eq!(model.state(), LifecycleStatus::Terminal);
        assert!(model.tombstone().is_some());
    }

    #[test]
    fn resume_ack_and_generation_overflow_reject_without_partial_mutation() {
        let mut model = detached_model();
        model.frontier.append().unwrap();
        let before = model.clone();
        let (_, binding) = new_binding();
        let mut request = resume_request(&model, binding, 304, 500);
        request.acknowledged_h = 1;
        assert_eq!(
            model.begin_resume(request),
            Err(ModelError::Ack(AckError::HandledTooHigh {
                h: 1,
                send_count: 0,
            }))
        );
        assert_eq!(model, before);

        let mut exhausted = enabling_model(u64::MAX);
        make_live(&mut exhausted);
        let fence = exhausted.fence();
        exhausted.detach(&fence).unwrap();
        let (_, binding) = new_binding();
        let request = resume_request(&exhausted, binding, 304, 500);
        let before = exhausted.clone();
        assert_eq!(
            exhausted.begin_resume(request),
            Err(ModelError::GenerationExhausted)
        );
        assert_eq!(exhausted, before);
    }

    #[test]
    fn wrong_publication_or_hydration_receipt_does_not_complete_transfer() {
        let mut model = detached_model();
        let (_, binding) = new_binding();
        let plan = model
            .begin_resume(resume_request(&model, binding, 304, 500))
            .expect("planned transfer");
        model.commit_transfer(&plan.commit).unwrap();

        let before = model.clone();
        let mut wrong_publication: PublicationReceipt = plan.publication.clone();
        wrong_publication.installation = OperationId::new(999);
        assert_eq!(
            model.record_publication(&wrong_publication),
            Err(ModelError::ReceiptMismatch)
        );
        assert_eq!(model, before);

        let mut wrong_hydration: HydrationReceipt = plan.hydration.clone();
        wrong_hydration.resources.presence_revision += 1;
        assert_eq!(
            model.record_hydration(&wrong_hydration),
            Err(ModelError::ReceiptMismatch)
        );
        assert_eq!(model, before);
        assert_eq!(model.state(), LifecycleStatus::TransferHydrating);
    }

    #[test]
    fn checked_frontier_records_uncertain_frame_write_until_retry_or_ack() {
        let mut model = enabling_model(7);
        make_live(&mut model);
        assert_eq!(model.append_outbound(), Ok(1));
        assert_eq!(
            model.record_write_attempt(1),
            Ok(AttemptOutcome::NewlyExposed)
        );
        model
            .record_frame_write_result(1, FrameWriteResult::Unknown)
            .unwrap();
        assert_eq!(model.replay_ordinals(), Some(vec![1]));
        assert!(model.frontier().has_uncertain_writes());
        assert_eq!(model.record_write_attempt(1), Ok(AttemptOutcome::Retry));
        model
            .record_frame_write_result(1, FrameWriteResult::Accepted)
            .unwrap();
        assert!(!model.frontier().has_uncertain_writes());
        assert_eq!(
            model.acknowledge_live(1),
            Ok(AckOutcome::Advanced { through: 1 })
        );
        assert_eq!(model.replay_ordinals(), Some(vec![]));
    }
}
