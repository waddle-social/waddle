//! Inactive model of a proposed P3 endpoint transition.
//!
//! This is not a repository, distributed lock, publication authority, or pump
//! implementation. All evidence is supplied by a caller as an assumption;
//! this model cannot prove custody, pump quiescence, SM persistence, claims,
//! or a remote mirror. No production registration or send path uses it.
//! `with_admission` models a synchronous commit action only: Rust permits its
//! closure to return a future without polling it, so this type does not prove
//! that a real enqueue completed. Future wiring must require a completed
//! enqueue or reservation transfer before its guard releases. Sealing and
//! that action must be atomic after capacity reservation, without awaiting a
//! socket while holding an endpoint lock.

use std::collections::{HashMap, HashSet};

use jid::FullJid;
use uuid::Uuid;

use super::{BindingRef, ConnectionRef};

/// One logical resource lifetime. A fresh bind mints one; resume preserves it.
/// This proposed identity is separate from room occupancy and physical I/O.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LogicalResourceGeneration(Uuid);

impl LogicalResourceGeneration {
    pub fn fresh() -> Self {
        Self(Uuid::new_v4())
    }
}

/// Monotonic revision of the one endpoint slot for a canonical full JID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EndpointRevision(u64);

impl EndpointRevision {
    pub const INITIAL: Self = Self(0);

    fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

/// Idempotency and recovery key for one attempted transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EndpointOperationId(Uuid);

impl EndpointOperationId {
    pub fn fresh() -> Self {
        Self(Uuid::new_v4())
    }
}

/// Exact destination of a published endpoint. Neither identity alone grants
/// node publication authority or durable delivery custody. Each bind or
/// resume attempt needs a fresh `BindingRef`, even on the same transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointBinding {
    pub binding_ref: BindingRef,
    pub logical_generation: LogicalResourceGeneration,
}

/// Resumable logical state with no active endpoint. The prior attempt is kept
/// only to compare caller-supplied retirement and pump-quiescence assumptions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetachedResource {
    pub logical_generation: LogicalResourceGeneration,
    pub prior_binding_ref: BindingRef,
}

/// A fresh bind replaces logical resource state; a resume retains it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindKind {
    Fresh,
    Resume,
}

/// Candidate held out of all admitting indexes until activation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointCandidate {
    binding: EndpointBinding,
    kind: BindKind,
}

impl EndpointCandidate {
    /// Mint a distinct attempt on an existing physical connection. Cloning a
    /// candidate retains the same attempt; it does not create a new one.
    pub fn new_attempt(
        connection_ref: ConnectionRef,
        logical_generation: LogicalResourceGeneration,
        kind: BindKind,
    ) -> Self {
        Self {
            binding: EndpointBinding {
                binding_ref: BindingRef::fresh(connection_ref),
                logical_generation,
            },
            kind,
        }
    }

    pub fn binding(&self) -> &EndpointBinding {
        &self.binding
    }

    pub fn kind(&self) -> BindKind {
        self.kind
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageError {
    RevisionConflict,
    AlreadyStaged,
    OperationReused,
    LogicalGenerationReused,
    LogicalGenerationMismatch,
    BindingRefReused,
    ResumeWithoutPredecessor,
    ResumeOnSameTransport,
    RevisionExhausted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionError {
    StaleOperation,
    RevisionConflict,
    EndpointMismatch,
    EvidenceMismatch,
    CommitUnknown,
    NotPrepared,
    PreparedCannotCompensate,
    RevisionExhausted,
}

/// Caller-supplied assumption that this exact attempt's old pump has stopped
/// and cannot resume during retirement or transfer. Constructing this value
/// does not stop, observe, or prevent the restart of any pump.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PumpQuiescence {
    pub binding_ref: BindingRef,
}

/// Caller-supplied disposition of work accepted by the predecessor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetiredCustody {
    NoAcceptedWork,
    AcceptedWorkHasRecoveryOwner,
}

/// Caller-supplied retirement, custody and quiescence for one exact binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetiredPredecessor {
    pub binding_ref: BindingRef,
    pub pump_quiescence: PumpQuiescence,
    pub custody: RetiredCustody,
}

impl RetiredPredecessor {
    fn matches(&self, expected: &BindingRef) -> bool {
        &self.binding_ref == expected && &self.pump_quiescence.binding_ref == expected
    }
}

/// Caller-supplied assumption that one exact SM handoff committed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeHandoff {
    pub prior_binding_ref: BindingRef,
    pub candidate_binding_ref: BindingRef,
    pub logical_generation: LogicalResourceGeneration,
}

/// Assumptions for a non-admitting mirror and, where applicable, predecessor
/// retirement and SM handoff. The operation, revision, and all binding values
/// must match the stage; supplying them does not establish the underlying facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparationEvidence {
    pub operation_id: EndpointOperationId,
    pub staged_revision: EndpointRevision,
    pub non_admitting_mirror: EndpointBinding,
    pub retired_predecessor: Option<RetiredPredecessor>,
    pub resume_handoff: Option<ResumeHandoff>,
}

/// Caller-supplied acknowledgement for the exact prepared activation. It does
/// not establish current claim authority, a durable write, or remote delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivationAcknowledgement {
    pub operation_id: EndpointOperationId,
    pub staged_revision: EndpointRevision,
    pub binding: EndpointBinding,
}

/// A successful preparation is still non-routable. Only this fixture can
/// produce the token; callers must separately obtain an exact acknowledgement
/// for activation. The token itself proves no external fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedEndpoint {
    operation_id: EndpointOperationId,
    staged_revision: EndpointRevision,
    candidate: EndpointCandidate,
}

/// What is known about the predecessor when abandoning a staged candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PredecessorOnCompensation {
    /// The stage began with no attached or detached predecessor.
    NoPredecessor,
    /// The old endpoint and its queue were never detached or modified.
    StillLiveAndUnchanged { binding_ref: BindingRef },
    /// The old detached logical state was never modified.
    DetachedAndUnchanged { binding_ref: BindingRef },
    /// Retirement, accepted work and pump quiescence are assumed for the
    /// exact predecessor; merely losing a response is not sufficient.
    RetiredWithCustody(RetiredPredecessor),
    /// A timeout or cancellation did not establish any of these facts.
    Unknown,
}

/// Local model disposition. An unresolved entry records a missing reply, not
/// the state of an external store or proof that a commit did not happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationDisposition {
    Staged(EndpointRevision),
    PreparationUnresolved(EndpointRevision),
    Prepared(EndpointRevision),
    ActivationUnresolved(EndpointRevision),
    Activated(EndpointRevision),
    Compensated(EndpointRevision),
}

/// A lost commit reply is explicitly unknown, not a conflict or rollback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitObservation<T> {
    Known(T),
    Unknown { operation_id: EndpointOperationId },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnqueueOutcome<T> {
    Committed(T),
    RetryBeforeEnqueue,
    NoEndpoint,
}

#[derive(Debug, Clone)]
enum EndpointState {
    Empty,
    Attached(EndpointBinding),
    Detached(DetachedResource),
}

impl EndpointState {
    fn prior_binding_ref(&self) -> Option<&BindingRef> {
        match self {
            Self::Empty => None,
            Self::Attached(binding) => Some(&binding.binding_ref),
            Self::Detached(resource) => Some(&resource.prior_binding_ref),
        }
    }

    fn logical_generation(&self) -> Option<LogicalResourceGeneration> {
        match self {
            Self::Empty => None,
            Self::Attached(binding) => Some(binding.logical_generation),
            Self::Detached(resource) => Some(resource.logical_generation),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnknownCommit {
    Preparation,
    Activation,
}

#[derive(Debug, Clone)]
struct Staged {
    operation_id: EndpointOperationId,
    revision: EndpointRevision,
    candidate: EndpointCandidate,
    prepared: Option<PreparationEvidence>,
    unknown_commit: Option<UnknownCommit>,
}

/// A single full-JID state-machine fixture. All methods are synchronous so a
/// test can serialize a capacity-reserved enqueue with a stage transition.
/// That serialization is a model condition, not an implementation mechanism.
#[derive(Debug)]
pub struct EndpointSlot {
    full_jid: FullJid,
    revision: EndpointRevision,
    state: EndpointState,
    staged: Option<Staged>,
    used_logical_generations: HashSet<LogicalResourceGeneration>,
    used_binding_refs: HashSet<BindingRef>,
    operations: HashMap<EndpointOperationId, OperationDisposition>,
}

impl EndpointSlot {
    pub fn new(full_jid: FullJid) -> Self {
        Self {
            full_jid,
            revision: EndpointRevision::INITIAL,
            state: EndpointState::Empty,
            staged: None,
            used_logical_generations: HashSet::new(),
            used_binding_refs: HashSet::new(),
            operations: HashMap::new(),
        }
    }

    pub fn key(&self) -> &FullJid {
        &self.full_jid
    }

    pub fn revision(&self) -> EndpointRevision {
        self.revision
    }

    pub fn active(&self) -> Option<&EndpointBinding> {
        if self.staged.is_some() {
            return None;
        }
        match &self.state {
            EndpointState::Attached(binding) => Some(binding),
            EndpointState::Empty | EndpointState::Detached(_) => None,
        }
    }

    pub fn detached(&self) -> Option<&DetachedResource> {
        match &self.state {
            EndpointState::Detached(resource) => Some(resource),
            EndpointState::Empty | EndpointState::Attached(_) => None,
        }
    }

    /// Retain resumable logical state while removing the active endpoint.
    /// The caller must supply retirement and pump quiescence for the exact
    /// active binding; neither assumption is independently verified here.
    pub fn detach(
        &mut self,
        expected: EndpointRevision,
        binding: &EndpointBinding,
        retirement: &RetiredPredecessor,
    ) -> Result<EndpointRevision, TransitionError> {
        if self.staged.is_some() {
            return Err(TransitionError::StaleOperation);
        }
        if self.revision != expected {
            return Err(TransitionError::RevisionConflict);
        }
        if !matches!(&self.state, EndpointState::Attached(current) if current == binding) {
            return Err(TransitionError::EndpointMismatch);
        }
        if !retirement.matches(&binding.binding_ref) {
            return Err(TransitionError::EvidenceMismatch);
        }
        let next = self
            .revision
            .next()
            .ok_or(TransitionError::RevisionExhausted)?;
        self.state = EndpointState::Detached(DetachedResource {
            logical_generation: binding.logical_generation,
            prior_binding_ref: binding.binding_ref.clone(),
        });
        self.revision = next;
        Ok(next)
    }

    /// Seal all admission before predecessor detach or asynchronous prepare.
    /// A staged operation is recorded at a new revision, including from empty.
    pub fn stage(
        &mut self,
        expected: EndpointRevision,
        operation_id: EndpointOperationId,
        candidate: EndpointCandidate,
    ) -> Result<EndpointRevision, StageError> {
        if self.operations.contains_key(&operation_id) {
            return Err(StageError::OperationReused);
        }
        if self.revision != expected {
            return Err(StageError::RevisionConflict);
        }
        if self.staged.is_some() {
            return Err(StageError::AlreadyStaged);
        }
        if self
            .used_binding_refs
            .contains(&candidate.binding.binding_ref)
        {
            return Err(StageError::BindingRefReused);
        }
        match candidate.kind {
            BindKind::Fresh => {
                if self
                    .used_logical_generations
                    .contains(&candidate.binding.logical_generation)
                {
                    return Err(StageError::LogicalGenerationReused);
                }
            }
            BindKind::Resume => {
                let prior = self
                    .state
                    .prior_binding_ref()
                    .ok_or(StageError::ResumeWithoutPredecessor)?;
                if self.state.logical_generation() != Some(candidate.binding.logical_generation) {
                    return Err(StageError::LogicalGenerationMismatch);
                }
                if prior.connection_ref() == candidate.binding.binding_ref.connection_ref() {
                    return Err(StageError::ResumeOnSameTransport);
                }
            }
        }
        let next = self.revision.next().ok_or(StageError::RevisionExhausted)?;
        if candidate.kind == BindKind::Fresh {
            self.used_logical_generations
                .insert(candidate.binding.logical_generation);
        }
        self.used_binding_refs
            .insert(candidate.binding.binding_ref.clone());
        self.revision = next;
        self.staged = Some(Staged {
            operation_id,
            revision: next,
            candidate,
            prepared: None,
            unknown_commit: None,
        });
        self.operations
            .insert(operation_id, OperationDisposition::Staged(next));
        Ok(next)
    }

    /// Model a synchronous enqueue commit while the exact active binding
    /// remains unsealed. An earlier capacity reservation grants no admission.
    /// This method cannot reject a closure that merely returns a future; a
    /// real implementation needs a stronger completed-enqueue boundary.
    pub fn with_admission<T>(
        &mut self,
        expected: &EndpointBinding,
        enqueue: impl FnOnce() -> T,
    ) -> EnqueueOutcome<T> {
        if self.staged.is_some() {
            return EnqueueOutcome::RetryBeforeEnqueue;
        }
        match &self.state {
            EndpointState::Attached(current) if current == expected => {
                EnqueueOutcome::Committed(enqueue())
            }
            EndpointState::Attached(_) => EnqueueOutcome::RetryBeforeEnqueue,
            EndpointState::Empty | EndpointState::Detached(_) => EnqueueOutcome::NoEndpoint,
        }
    }

    /// Validate exact caller assumptions and produce a non-routable token.
    /// A missing commit reply seals the slot until this same preparation is
    /// resolved; it is never interpreted as a failed or rolled-back commit.
    pub fn prepare(
        &mut self,
        operation_id: EndpointOperationId,
        staged_revision: EndpointRevision,
        observation: CommitObservation<PreparationEvidence>,
    ) -> Result<PreparedEndpoint, TransitionError> {
        let staged = self
            .staged
            .as_mut()
            .filter(|stage| stage.operation_id == operation_id && stage.revision == staged_revision)
            .ok_or(TransitionError::StaleOperation)?;
        if staged.unknown_commit == Some(UnknownCommit::Activation) {
            return Err(TransitionError::CommitUnknown);
        }
        let evidence = match observation {
            CommitObservation::Unknown {
                operation_id: unknown_id,
            } => {
                if unknown_id != operation_id || staged.prepared.is_some() {
                    return Err(TransitionError::EvidenceMismatch);
                }
                staged.unknown_commit = Some(UnknownCommit::Preparation);
                self.operations.insert(
                    operation_id,
                    OperationDisposition::PreparationUnresolved(staged_revision),
                );
                return Err(TransitionError::CommitUnknown);
            }
            CommitObservation::Known(evidence) => evidence,
        };
        let predecessor = self.state.prior_binding_ref();
        let retirement_matches = match (&evidence.retired_predecessor, predecessor) {
            (None, None) => true,
            (Some(retired), Some(prior)) => retired.matches(prior),
            _ => false,
        };
        let handoff_matches = match (&evidence.resume_handoff, staged.candidate.kind, predecessor) {
            (None, BindKind::Fresh, _) => true,
            (Some(handoff), BindKind::Resume, Some(prior)) => {
                &handoff.prior_binding_ref == prior
                    && handoff.candidate_binding_ref == staged.candidate.binding.binding_ref
                    && handoff.logical_generation == staged.candidate.binding.logical_generation
            }
            _ => false,
        };
        if evidence.operation_id != operation_id
            || evidence.staged_revision != staged_revision
            || evidence.non_admitting_mirror != staged.candidate.binding
            || !retirement_matches
            || !handoff_matches
            || staged
                .prepared
                .as_ref()
                .is_some_and(|previous| previous != &evidence)
        {
            return Err(TransitionError::EvidenceMismatch);
        }
        staged.prepared = Some(evidence);
        staged.unknown_commit = None;
        self.operations.insert(
            operation_id,
            OperationDisposition::Prepared(staged_revision),
        );
        Ok(PreparedEndpoint {
            operation_id,
            staged_revision,
            candidate: staged.candidate.clone(),
        })
    }

    /// Activate the exact prepared candidate only after a matching caller
    /// acknowledgement. A lost activation reply remains unresolved and keeps
    /// the seal closed until an exact known observation is supplied.
    pub fn activate(
        &mut self,
        prepared: &PreparedEndpoint,
        observation: CommitObservation<ActivationAcknowledgement>,
    ) -> Result<EndpointRevision, TransitionError> {
        let staged = self
            .staged
            .as_mut()
            .filter(|stage| {
                stage.operation_id == prepared.operation_id
                    && stage.revision == prepared.staged_revision
                    && stage.candidate == prepared.candidate
            })
            .ok_or(TransitionError::StaleOperation)?;
        if staged.unknown_commit == Some(UnknownCommit::Preparation) {
            return Err(TransitionError::CommitUnknown);
        }
        if staged.prepared.is_none() {
            return Err(TransitionError::NotPrepared);
        }
        let acknowledgement = match observation {
            CommitObservation::Unknown { operation_id } => {
                if operation_id != staged.operation_id {
                    return Err(TransitionError::EvidenceMismatch);
                }
                staged.unknown_commit = Some(UnknownCommit::Activation);
                self.operations.insert(
                    operation_id,
                    OperationDisposition::ActivationUnresolved(staged.revision),
                );
                return Err(TransitionError::CommitUnknown);
            }
            CommitObservation::Known(acknowledgement) => acknowledgement,
        };
        if acknowledgement.operation_id != staged.operation_id
            || acknowledgement.staged_revision != staged.revision
            || acknowledgement.binding != staged.candidate.binding
        {
            return Err(TransitionError::EvidenceMismatch);
        }
        let next = self
            .revision
            .next()
            .ok_or(TransitionError::RevisionExhausted)?;
        self.state = EndpointState::Attached(staged.candidate.binding.clone());
        self.staged = None;
        self.revision = next;
        self.operations
            .insert(prepared.operation_id, OperationDisposition::Activated(next));
        Ok(next)
    }

    /// Clear only this still-staged operation. Unknown commits or predecessor
    /// custody keep the seal closed. A prepared operation cannot be abandoned:
    /// candidate cleanup and a committed SM handoff need a separate contract.
    pub fn compensate(
        &mut self,
        operation_id: EndpointOperationId,
        staged_revision: EndpointRevision,
        predecessor: PredecessorOnCompensation,
    ) -> Result<EndpointRevision, TransitionError> {
        let staged = self
            .staged
            .as_ref()
            .filter(|stage| stage.operation_id == operation_id && stage.revision == staged_revision)
            .ok_or(TransitionError::StaleOperation)?;
        if staged.unknown_commit.is_some() {
            return Err(TransitionError::CommitUnknown);
        }
        if staged.prepared.is_some() {
            return Err(TransitionError::PreparedCannotCompensate);
        }
        let preserve = match (&self.state, predecessor) {
            (EndpointState::Empty, PredecessorOnCompensation::NoPredecessor) => false,
            (
                EndpointState::Attached(current),
                PredecessorOnCompensation::StillLiveAndUnchanged { binding_ref },
            ) if binding_ref == current.binding_ref => true,
            (
                EndpointState::Detached(current),
                PredecessorOnCompensation::DetachedAndUnchanged { binding_ref },
            ) if binding_ref == current.prior_binding_ref => true,
            (state, PredecessorOnCompensation::RetiredWithCustody(retirement))
                if state
                    .prior_binding_ref()
                    .is_some_and(|binding_ref| retirement.matches(binding_ref)) =>
            {
                false
            }
            _ => return Err(TransitionError::EvidenceMismatch),
        };
        let next = self
            .revision
            .next()
            .ok_or(TransitionError::RevisionExhausted)?;
        if !preserve {
            self.state = EndpointState::Empty;
        }
        self.staged = None;
        self.revision = next;
        self.operations
            .insert(operation_id, OperationDisposition::Compensated(next));
        Ok(next)
    }

    /// Read the operation journal after an unknown commit reply. Missing is
    /// not rollback evidence while storage itself is unavailable.
    pub fn disposition(&self, operation_id: EndpointOperationId) -> Option<OperationDisposition> {
        self.operations.get(&operation_id).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ownership::{ConnectionRef, NodeIdentity, NodeLeaseIdentity};

    fn jid() -> FullJid {
        "alice@example.test/phone".parse().expect("valid full JID")
    }

    fn new_connection() -> ConnectionRef {
        ConnectionRef::fresh(NodeLeaseIdentity::local_process())
    }

    fn binding(
        connection_ref: ConnectionRef,
        logical_generation: LogicalResourceGeneration,
    ) -> EndpointBinding {
        EndpointBinding {
            binding_ref: BindingRef::fresh(connection_ref),
            logical_generation,
        }
    }

    fn retirement(binding_ref: &BindingRef) -> RetiredPredecessor {
        RetiredPredecessor {
            binding_ref: binding_ref.clone(),
            pump_quiescence: PumpQuiescence {
                binding_ref: binding_ref.clone(),
            },
            custody: RetiredCustody::AcceptedWorkHasRecoveryOwner,
        }
    }

    fn evidence(
        operation_id: EndpointOperationId,
        staged_revision: EndpointRevision,
        candidate: &EndpointCandidate,
        predecessor: Option<&BindingRef>,
    ) -> PreparationEvidence {
        PreparationEvidence {
            operation_id,
            staged_revision,
            non_admitting_mirror: candidate.binding.clone(),
            retired_predecessor: predecessor.map(retirement),
            resume_handoff: match candidate.kind {
                BindKind::Fresh => None,
                BindKind::Resume => predecessor.map(|prior| ResumeHandoff {
                    prior_binding_ref: prior.clone(),
                    candidate_binding_ref: candidate.binding.binding_ref.clone(),
                    logical_generation: candidate.binding.logical_generation,
                }),
            },
        }
    }

    fn acknowledgement(prepared: &PreparedEndpoint) -> ActivationAcknowledgement {
        ActivationAcknowledgement {
            operation_id: prepared.operation_id,
            staged_revision: prepared.staged_revision,
            binding: prepared.candidate.binding.clone(),
        }
    }

    fn prepare(
        slot: &mut EndpointSlot,
        operation: EndpointOperationId,
        revision: EndpointRevision,
        candidate: &EndpointCandidate,
        predecessor: Option<&BindingRef>,
    ) -> PreparedEndpoint {
        slot.prepare(
            operation,
            revision,
            CommitObservation::Known(evidence(operation, revision, candidate, predecessor)),
        )
        .expect("prepare exact candidate")
    }

    fn activate(slot: &mut EndpointSlot, prepared: &PreparedEndpoint) -> EndpointRevision {
        slot.activate(
            prepared,
            CommitObservation::Known(acknowledgement(prepared)),
        )
        .expect("activate exact candidate")
    }

    fn publish_fresh(slot: &mut EndpointSlot, binding: EndpointBinding) {
        let operation = EndpointOperationId::fresh();
        let candidate = EndpointCandidate {
            binding,
            kind: BindKind::Fresh,
        };
        let revision = slot
            .stage(slot.revision(), operation, candidate.clone())
            .expect("stage fresh bind");
        let prepared = prepare(slot, operation, revision, &candidate, None);
        assert_eq!(slot.active(), None);
        activate(slot, &prepared);
    }

    #[test]
    fn prepared_fresh_binding_is_non_routable_until_exact_activation() {
        let mut slot = EndpointSlot::new(jid());
        let first = binding(new_connection(), LogicalResourceGeneration::fresh());
        let candidate = EndpointCandidate {
            binding: first.clone(),
            kind: BindKind::Fresh,
        };
        let operation = EndpointOperationId::fresh();
        let staged = slot
            .stage(slot.revision(), operation, candidate.clone())
            .expect("stage");
        assert_eq!(slot.active(), None);
        assert_eq!(
            slot.with_admission(&first, || panic!("staged endpoint enqueued")),
            EnqueueOutcome::RetryBeforeEnqueue
        );
        let prepared = prepare(&mut slot, operation, staged, &candidate, None);
        assert_eq!(slot.active(), None);
        assert_eq!(
            slot.disposition(operation),
            Some(OperationDisposition::Prepared(staged))
        );
        let mut wrong = acknowledgement(&prepared);
        wrong.binding.binding_ref = BindingRef::fresh(new_connection());
        assert_eq!(
            slot.activate(&prepared, CommitObservation::Known(wrong)),
            Err(TransitionError::EvidenceMismatch)
        );
        assert_eq!(slot.active(), None);
        activate(&mut slot, &prepared);
        assert_eq!(slot.active(), Some(&first));
        assert_eq!(
            slot.with_admission(&first, || 7),
            EnqueueOutcome::Committed(7)
        );
    }

    #[test]
    fn detached_resource_has_no_active_endpoint_and_resumes_same_logical_state() {
        let mut slot = EndpointSlot::new(jid());
        let logical = LogicalResourceGeneration::fresh();
        let old = binding(new_connection(), logical);
        publish_fresh(&mut slot, old.clone());

        let mut wrong_pump = retirement(&old.binding_ref);
        wrong_pump.pump_quiescence.binding_ref = BindingRef::fresh(new_connection());
        assert_eq!(
            slot.detach(slot.revision(), &old, &wrong_pump),
            Err(TransitionError::EvidenceMismatch)
        );
        assert_eq!(slot.active(), Some(&old));
        slot.detach(slot.revision(), &old, &retirement(&old.binding_ref))
            .expect("detach exact old pump");
        assert_eq!(slot.active(), None);
        assert_eq!(
            slot.detached(),
            Some(&DetachedResource {
                logical_generation: logical,
                prior_binding_ref: old.binding_ref.clone(),
            })
        );
        assert_eq!(
            slot.with_admission(&old, || panic!("detached endpoint enqueued")),
            EnqueueOutcome::NoEndpoint
        );
        assert_eq!(
            slot.stage(
                slot.revision(),
                EndpointOperationId::fresh(),
                EndpointCandidate {
                    binding: binding(old.binding_ref.connection_ref().clone(), logical),
                    kind: BindKind::Resume,
                },
            ),
            Err(StageError::ResumeOnSameTransport)
        );

        let successor = binding(
            ConnectionRef::fresh(NodeLeaseIdentity::from_node_identity(&NodeIdentity::new(
                "node-b", "epoch-b",
            ))),
            logical,
        );
        let candidate = EndpointCandidate {
            binding: successor.clone(),
            kind: BindKind::Resume,
        };
        let operation = EndpointOperationId::fresh();
        let staged = slot
            .stage(slot.revision(), operation, candidate.clone())
            .expect("stage detached resume");
        let good = evidence(operation, staged, &candidate, Some(&old.binding_ref));
        let mut cases = Vec::new();
        let mut wrong_operation = good.clone();
        wrong_operation.operation_id = EndpointOperationId::fresh();
        cases.push(wrong_operation);
        let mut wrong_revision = good.clone();
        wrong_revision.staged_revision = EndpointRevision::INITIAL;
        cases.push(wrong_revision);
        let mut wrong_mirror = good.clone();
        wrong_mirror.non_admitting_mirror = old.clone();
        cases.push(wrong_mirror);
        let mut missing_retirement = good.clone();
        missing_retirement.retired_predecessor = None;
        cases.push(missing_retirement);
        let mut wrong_retirement = good.clone();
        wrong_retirement.retired_predecessor = Some(retirement(&successor.binding_ref));
        cases.push(wrong_retirement);
        let mut wrong_quiescence = good.clone();
        wrong_quiescence.retired_predecessor = Some(RetiredPredecessor {
            pump_quiescence: PumpQuiescence {
                binding_ref: successor.binding_ref.clone(),
            },
            ..retirement(&old.binding_ref)
        });
        cases.push(wrong_quiescence);
        let mut missing_handoff = good.clone();
        missing_handoff.resume_handoff = None;
        cases.push(missing_handoff);
        let mut wrong_handoff = good.clone();
        wrong_handoff.resume_handoff = Some(ResumeHandoff {
            prior_binding_ref: successor.binding_ref.clone(),
            candidate_binding_ref: successor.binding_ref.clone(),
            logical_generation: logical,
        });
        cases.push(wrong_handoff);
        for wrong in cases {
            assert_eq!(
                slot.prepare(operation, staged, CommitObservation::Known(wrong)),
                Err(TransitionError::EvidenceMismatch)
            );
            assert_eq!(slot.active(), None);
            assert_eq!(
                slot.disposition(operation),
                Some(OperationDisposition::Staged(staged))
            );
        }
        let prepared = slot
            .prepare(operation, staged, CommitObservation::Known(good))
            .expect("all exact assumptions supplied");
        assert_eq!(slot.active(), None);
        activate(&mut slot, &prepared);
        assert_eq!(slot.active(), Some(&successor));
        assert_eq!(slot.detached(), None);
        assert_ne!(old.binding_ref, successor.binding_ref);
        assert_eq!(old.logical_generation, successor.logical_generation);
    }

    #[test]
    fn failed_resume_then_fresh_bind_uses_a_new_attempt_on_the_same_socket() {
        let mut slot = EndpointSlot::new(jid());
        let old = binding(new_connection(), LogicalResourceGeneration::fresh());
        publish_fresh(&mut slot, old.clone());
        slot.detach(slot.revision(), &old, &retirement(&old.binding_ref))
            .expect("detach");
        let socket = new_connection();
        let failed_candidate = EndpointCandidate::new_attempt(
            socket.clone(),
            old.logical_generation,
            BindKind::Resume,
        );
        let failed = failed_candidate.binding().clone();
        assert_eq!(failed_candidate.kind(), BindKind::Resume);
        let failed_op = EndpointOperationId::fresh();
        let failed_revision = slot
            .stage(slot.revision(), failed_op, failed_candidate)
            .expect("stage failed resume");
        slot.compensate(
            failed_op,
            failed_revision,
            PredecessorOnCompensation::DetachedAndUnchanged {
                binding_ref: old.binding_ref.clone(),
            },
        )
        .expect("compensate failed candidate");
        assert_eq!(slot.active(), None);
        assert_eq!(
            slot.detached().map(|value| value.logical_generation),
            Some(old.logical_generation)
        );

        let candidate = EndpointCandidate::new_attempt(
            socket,
            LogicalResourceGeneration::fresh(),
            BindKind::Fresh,
        );
        let fresh = candidate.binding().clone();
        assert_eq!(
            fresh.binding_ref.connection_ref(),
            failed.binding_ref.connection_ref()
        );
        assert_ne!(fresh.binding_ref, failed.binding_ref);
        assert_ne!(fresh.logical_generation, failed.logical_generation);
        assert_eq!(
            slot.stage(
                slot.revision(),
                EndpointOperationId::fresh(),
                EndpointCandidate {
                    binding: EndpointBinding {
                        binding_ref: failed.binding_ref.clone(),
                        logical_generation: LogicalResourceGeneration::fresh(),
                    },
                    kind: BindKind::Fresh,
                },
            ),
            Err(StageError::BindingRefReused)
        );
        let operation = EndpointOperationId::fresh();
        let staged = slot
            .stage(slot.revision(), operation, candidate.clone())
            .expect("stage fresh on the same socket");
        let prepared = prepare(
            &mut slot,
            operation,
            staged,
            &candidate,
            Some(&old.binding_ref),
        );
        activate(&mut slot, &prepared);
        assert_eq!(slot.active(), Some(&fresh));
        assert_eq!(
            slot.compensate(
                failed_op,
                failed_revision,
                PredecessorOnCompensation::RetiredWithCustody(retirement(&old.binding_ref)),
            ),
            Err(TransitionError::StaleOperation)
        );
        assert_eq!(slot.active(), Some(&fresh));
    }

    #[test]
    fn unknown_commit_cannot_activate_compensate_or_fall_through() {
        let mut slot = EndpointSlot::new(jid());
        let candidate = EndpointCandidate {
            binding: binding(new_connection(), LogicalResourceGeneration::fresh()),
            kind: BindKind::Fresh,
        };
        let operation = EndpointOperationId::fresh();
        let staged = slot
            .stage(slot.revision(), operation, candidate.clone())
            .expect("stage");
        let forged = PreparedEndpoint {
            operation_id: operation,
            staged_revision: staged,
            candidate: candidate.clone(),
        };
        assert_eq!(
            slot.activate(&forged, CommitObservation::Known(acknowledgement(&forged))),
            Err(TransitionError::NotPrepared)
        );
        assert_eq!(
            slot.prepare(
                operation,
                staged,
                CommitObservation::Unknown {
                    operation_id: operation
                },
            ),
            Err(TransitionError::CommitUnknown)
        );
        assert_eq!(
            slot.activate(&forged, CommitObservation::Known(acknowledgement(&forged))),
            Err(TransitionError::CommitUnknown)
        );
        assert_eq!(
            slot.compensate(operation, staged, PredecessorOnCompensation::Unknown),
            Err(TransitionError::CommitUnknown)
        );
        assert_eq!(slot.active(), None);
        assert_eq!(
            slot.disposition(operation),
            Some(OperationDisposition::PreparationUnresolved(staged))
        );
        let mut wrong = evidence(operation, staged, &candidate, None);
        wrong.operation_id = EndpointOperationId::fresh();
        assert_eq!(
            slot.prepare(operation, staged, CommitObservation::Known(wrong)),
            Err(TransitionError::EvidenceMismatch)
        );
        assert_eq!(
            slot.compensate(operation, staged, PredecessorOnCompensation::Unknown),
            Err(TransitionError::CommitUnknown)
        );
        let prepared = prepare(&mut slot, operation, staged, &candidate, None);
        assert_eq!(
            slot.activate(
                &prepared,
                CommitObservation::Unknown {
                    operation_id: operation
                },
            ),
            Err(TransitionError::CommitUnknown)
        );
        assert_eq!(
            slot.disposition(operation),
            Some(OperationDisposition::ActivationUnresolved(staged))
        );
        assert_eq!(
            slot.compensate(operation, staged, PredecessorOnCompensation::Unknown),
            Err(TransitionError::CommitUnknown)
        );
        assert_eq!(
            slot.prepare(
                operation,
                staged,
                CommitObservation::Known(evidence(operation, staged, &candidate, None)),
            ),
            Err(TransitionError::CommitUnknown)
        );
        assert_eq!(slot.active(), None);
        assert_eq!(
            slot.with_admission(&candidate.binding, || panic!("unknown commit admitted")),
            EnqueueOutcome::RetryBeforeEnqueue
        );
        let mut wrong_ack = acknowledgement(&prepared);
        wrong_ack.operation_id = EndpointOperationId::fresh();
        assert_eq!(
            slot.activate(&prepared, CommitObservation::Known(wrong_ack)),
            Err(TransitionError::EvidenceMismatch)
        );
        assert_eq!(slot.active(), None);
        let activated = activate(&mut slot, &prepared);
        assert_eq!(slot.active(), Some(&candidate.binding));
        assert_eq!(
            slot.disposition(operation),
            Some(OperationDisposition::Activated(activated))
        );
    }

    #[test]
    fn sealing_blocks_reserved_enqueue_and_retired_predecessor_cannot_reopen() {
        let mut slot = EndpointSlot::new(jid());
        let old = binding(new_connection(), LogicalResourceGeneration::fresh());
        publish_fresh(&mut slot, old.clone());
        let candidate = EndpointCandidate {
            binding: binding(new_connection(), LogicalResourceGeneration::fresh()),
            kind: BindKind::Fresh,
        };
        let operation = EndpointOperationId::fresh();
        let before = slot.revision();
        let staged = slot
            .stage(before, operation, candidate.clone())
            .expect("seal old endpoint");
        assert_eq!(
            slot.with_admission(&old, || panic!("reservation admitted after seal")),
            EnqueueOutcome::RetryBeforeEnqueue
        );
        assert_eq!(
            slot.stage(before, EndpointOperationId::fresh(), candidate.clone()),
            Err(StageError::RevisionConflict)
        );
        let prepared = prepare(
            &mut slot,
            operation,
            staged,
            &candidate,
            Some(&old.binding_ref),
        );
        assert_eq!(
            slot.compensate(
                operation,
                staged,
                PredecessorOnCompensation::StillLiveAndUnchanged {
                    binding_ref: old.binding_ref.clone(),
                },
            ),
            Err(TransitionError::PreparedCannotCompensate)
        );
        activate(&mut slot, &prepared);
        assert_eq!(
            slot.with_admission(&old, || panic!("old binding accepted")),
            EnqueueOutcome::RetryBeforeEnqueue
        );
        assert_eq!(
            slot.compensate(
                operation,
                staged,
                PredecessorOnCompensation::RetiredWithCustody(retirement(&old.binding_ref)),
            ),
            Err(TransitionError::StaleOperation)
        );
        assert_eq!(slot.active(), Some(&candidate.binding));
    }

    #[test]
    fn compensation_requires_exact_predecessor_and_pump_before_forgetting_it() {
        let mut slot = EndpointSlot::new(jid());
        let old = binding(new_connection(), LogicalResourceGeneration::fresh());
        publish_fresh(&mut slot, old.clone());
        let candidate = EndpointCandidate {
            binding: binding(new_connection(), LogicalResourceGeneration::fresh()),
            kind: BindKind::Fresh,
        };
        let operation = EndpointOperationId::fresh();
        let staged = slot
            .stage(slot.revision(), operation, candidate.clone())
            .expect("seal old endpoint");
        let mut wrong_pump = retirement(&old.binding_ref);
        wrong_pump.pump_quiescence.binding_ref = candidate.binding.binding_ref.clone();
        assert_eq!(
            slot.compensate(
                operation,
                staged,
                PredecessorOnCompensation::RetiredWithCustody(wrong_pump),
            ),
            Err(TransitionError::EvidenceMismatch)
        );
        assert_eq!(slot.active(), None);
        assert_eq!(
            slot.compensate(
                operation,
                staged,
                PredecessorOnCompensation::StillLiveAndUnchanged {
                    binding_ref: candidate.binding.binding_ref.clone(),
                },
            ),
            Err(TransitionError::EvidenceMismatch)
        );
        slot.compensate(
            operation,
            staged,
            PredecessorOnCompensation::StillLiveAndUnchanged {
                binding_ref: old.binding_ref.clone(),
            },
        )
        .expect("reopen exact unchanged predecessor");
        assert_eq!(slot.active(), Some(&old));
        let used_logical = candidate.binding.logical_generation;
        assert_eq!(
            slot.stage(slot.revision(), EndpointOperationId::fresh(), candidate),
            Err(StageError::BindingRefReused)
        );
        assert_eq!(
            slot.stage(
                slot.revision(),
                EndpointOperationId::fresh(),
                EndpointCandidate::new_attempt(new_connection(), used_logical, BindKind::Fresh),
            ),
            Err(StageError::LogicalGenerationReused)
        );
        let next = EndpointCandidate {
            binding: binding(new_connection(), LogicalResourceGeneration::fresh()),
            kind: BindKind::Fresh,
        };
        let next_op = EndpointOperationId::fresh();
        let next_stage = slot
            .stage(slot.revision(), next_op, next)
            .expect("stage new attempt");
        slot.compensate(
            next_op,
            next_stage,
            PredecessorOnCompensation::RetiredWithCustody(retirement(&old.binding_ref)),
        )
        .expect("forget only exact quiesced predecessor");
        assert_eq!(slot.active(), None);
        assert_eq!(slot.detached(), None);
    }
}
