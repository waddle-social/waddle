//! Inactive model of a proposed P3 endpoint transition.
//!
//! This is not a repository, a distributed lock, or publication authority.
//! In particular, the evidence passed to `activate` is an assumption made by a
//! test: this module cannot prove predecessor custody, SM persistence, node
//! claims, or a remote mirror. No production registration or send path uses it.
//! `with_admission` models a synchronous commit action only: Rust permits its
//! closure to return a future without polling it, so this type does not prove
//! that a real enqueue completed. Future wiring must require a completed
//! enqueue or reservation transfer before its guard releases. Sealing and
//! that action must be atomic after capacity reservation, without awaiting a
//! socket while holding an endpoint lock.

use std::collections::{HashMap, HashSet};

use jid::FullJid;
use uuid::Uuid;

use super::ConnectionRef;

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

/// Proposed exact fence for one published resource binding. Its relationship
/// to durable SM and account-resource rows remains a design gate. It is not
/// the node-local `LocalConnectionBindingId` used by today's registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ResourceBindingGeneration(Uuid);

impl ResourceBindingGeneration {
    pub fn fresh() -> Self {
        Self(Uuid::new_v4())
    }
}

/// Exact destination of a published endpoint. Neither identity alone grants
/// node publication authority or durable delivery custody.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointBinding {
    pub connection_ref: ConnectionRef,
    pub binding_generation: ResourceBindingGeneration,
    pub logical_generation: LogicalResourceGeneration,
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
    pub binding: EndpointBinding,
    pub kind: BindKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageError {
    RevisionConflict,
    AlreadyStaged,
    OperationReused,
    LogicalGenerationReused,
    LogicalGenerationMismatch,
    BindingGenerationReused,
    ResumeWithoutPredecessor,
    ResumeOnSameTransport,
    RevisionExhausted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionError {
    StaleOperation,
    EvidenceMissing,
    RevisionExhausted,
}

/// Assumptions supplied by a future caller, never proofs issued by this model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActivationEvidence {
    pub predecessor_retired_with_custody: bool,
    pub mirror_prepared_non_admitting: bool,
    pub resumed_sm_handoff_committed: bool,
}

/// What is known about the predecessor when abandoning a staged candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PredecessorOnCompensation {
    /// The old endpoint and its queue were never detached or modified.
    StillLiveAndUnchanged,
    /// Its accepted tail has an independently confirmed recovery owner.
    RetiredWithCustody,
    /// A timeout or cancellation did not establish either fact.
    Unknown,
}

/// Durable-journal disposition proposed for unknown-result reconciliation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationDisposition {
    Staged(EndpointRevision),
    Activated(EndpointRevision),
    Compensated(EndpointRevision),
}

/// A lost commit reply is explicitly unknown, not a conflict or rollback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
struct Staged {
    operation_id: EndpointOperationId,
    revision: EndpointRevision,
    candidate: EndpointCandidate,
    had_predecessor: bool,
}

/// A single full-JID state-machine fixture. All methods are synchronous so a
/// test can serialize a capacity-reserved enqueue with a stage transition.
/// That serialization is a model condition, not an implementation mechanism.
#[derive(Debug)]
pub struct EndpointSlot {
    full_jid: FullJid,
    revision: EndpointRevision,
    active: Option<EndpointBinding>,
    staged: Option<Staged>,
    used_logical_generations: HashSet<LogicalResourceGeneration>,
    used_binding_generations: HashSet<ResourceBindingGeneration>,
    operations: HashMap<EndpointOperationId, OperationDisposition>,
}

impl EndpointSlot {
    pub fn new(full_jid: FullJid) -> Self {
        Self {
            full_jid,
            revision: EndpointRevision::INITIAL,
            active: None,
            staged: None,
            used_logical_generations: HashSet::new(),
            used_binding_generations: HashSet::new(),
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
        self.active.as_ref().filter(|_| self.staged.is_none())
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
            .used_binding_generations
            .contains(&candidate.binding.binding_generation)
        {
            return Err(StageError::BindingGenerationReused);
        }
        match (candidate.kind, self.active.as_ref()) {
            (BindKind::Fresh, _) => {
                if self
                    .used_logical_generations
                    .contains(&candidate.binding.logical_generation)
                {
                    return Err(StageError::LogicalGenerationReused);
                }
            }
            (BindKind::Resume, Some(current)) => {
                if current.logical_generation != candidate.binding.logical_generation {
                    return Err(StageError::LogicalGenerationMismatch);
                }
                if current.connection_ref == candidate.binding.connection_ref {
                    return Err(StageError::ResumeOnSameTransport);
                }
            }
            (BindKind::Resume, None) => return Err(StageError::ResumeWithoutPredecessor),
        }
        let next = self.revision.next().ok_or(StageError::RevisionExhausted)?;
        if candidate.kind == BindKind::Fresh {
            self.used_logical_generations
                .insert(candidate.binding.logical_generation);
        }
        self.used_binding_generations
            .insert(candidate.binding.binding_generation);
        self.revision = next;
        self.staged = Some(Staged {
            operation_id,
            revision: next,
            candidate,
            had_predecessor: self.active.is_some(),
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
        match self.active.as_ref() {
            Some(current) if current == expected => EnqueueOutcome::Committed(enqueue()),
            Some(_) => EnqueueOutcome::RetryBeforeEnqueue,
            None => EnqueueOutcome::NoEndpoint,
        }
    }

    /// Activate only after external retirement, mirror and SM handoff facts
    /// are established. No prior endpoint is restored by this operation.
    pub fn activate(
        &mut self,
        operation_id: EndpointOperationId,
        staged_revision: EndpointRevision,
        evidence: ActivationEvidence,
    ) -> Result<EndpointRevision, TransitionError> {
        let staged = self
            .staged
            .as_ref()
            .filter(|stage| stage.operation_id == operation_id && stage.revision == staged_revision)
            .ok_or(TransitionError::StaleOperation)?;
        if !evidence.mirror_prepared_non_admitting
            || (staged.had_predecessor && !evidence.predecessor_retired_with_custody)
            || (staged.candidate.kind == BindKind::Resume && !evidence.resumed_sm_handoff_committed)
        {
            return Err(TransitionError::EvidenceMissing);
        }
        let next = self
            .revision
            .next()
            .ok_or(TransitionError::RevisionExhausted)?;
        self.active = Some(staged.candidate.binding.clone());
        self.staged = None;
        self.revision = next;
        self.operations
            .insert(operation_id, OperationDisposition::Activated(next));
        Ok(next)
    }

    /// Clear only this still-staged operation. Unknown predecessor custody
    /// keeps the seal closed. An unchanged live predecessor can be reopened;
    /// a retired predecessor leaves no route and is never revived by JID.
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
        if staged.had_predecessor && predecessor == PredecessorOnCompensation::Unknown {
            return Err(TransitionError::EvidenceMissing);
        }
        let next = self
            .revision
            .next()
            .ok_or(TransitionError::RevisionExhausted)?;
        if predecessor != PredecessorOnCompensation::StillLiveAndUnchanged {
            self.active = None;
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
    use crate::ownership::NodeLeaseIdentity;

    fn jid() -> FullJid {
        "alice@example.test/phone".parse().expect("valid full JID")
    }

    fn binding(
        connection_ref: ConnectionRef,
        logical_generation: LogicalResourceGeneration,
    ) -> EndpointBinding {
        EndpointBinding {
            connection_ref,
            binding_generation: ResourceBindingGeneration::fresh(),
            logical_generation,
        }
    }

    fn new_connection() -> ConnectionRef {
        ConnectionRef::fresh(NodeLeaseIdentity::local_process())
    }

    fn ready_for_fresh() -> ActivationEvidence {
        ActivationEvidence {
            predecessor_retired_with_custody: true,
            mirror_prepared_non_admitting: true,
            resumed_sm_handoff_committed: false,
        }
    }

    fn publish_fresh(slot: &mut EndpointSlot, binding: EndpointBinding) {
        let operation = EndpointOperationId::fresh();
        let staged = slot
            .stage(
                slot.revision(),
                operation,
                EndpointCandidate {
                    binding,
                    kind: BindKind::Fresh,
                },
            )
            .expect("stage fresh bind");
        slot.activate(operation, staged, ready_for_fresh())
            .expect("activate fresh bind");
    }

    #[test]
    fn fresh_bind_mints_logical_generation_and_seals_before_activation() {
        let mut slot = EndpointSlot::new(jid());
        let first = binding(new_connection(), LogicalResourceGeneration::fresh());
        let operation = EndpointOperationId::fresh();
        let staged = slot
            .stage(
                slot.revision(),
                operation,
                EndpointCandidate {
                    binding: first.clone(),
                    kind: BindKind::Fresh,
                },
            )
            .expect("stage");
        assert_eq!(slot.active(), None);
        assert_eq!(
            slot.with_admission(&first, || panic!("sealed endpoint enqueued")),
            EnqueueOutcome::RetryBeforeEnqueue
        );
        assert_eq!(
            slot.activate(
                operation,
                staged,
                ActivationEvidence {
                    mirror_prepared_non_admitting: false,
                    ..ready_for_fresh()
                }
            ),
            Err(TransitionError::EvidenceMissing)
        );
        slot.activate(operation, staged, ready_for_fresh())
            .expect("activate");
        assert_eq!(slot.active(), Some(&first));
        assert_eq!(
            slot.with_admission(&first, || 7),
            EnqueueOutcome::Committed(7)
        );
    }

    #[test]
    fn successful_cross_node_resume_keeps_logical_generation_and_changes_transport() {
        let mut slot = EndpointSlot::new(jid());
        let logical = LogicalResourceGeneration::fresh();
        let old = binding(new_connection(), logical);
        publish_fresh(&mut slot, old.clone());
        assert_eq!(
            slot.stage(
                slot.revision(),
                EndpointOperationId::fresh(),
                EndpointCandidate {
                    binding: binding(old.connection_ref.clone(), logical),
                    kind: BindKind::Resume,
                }
            ),
            Err(StageError::ResumeOnSameTransport)
        );
        let next = binding(
            ConnectionRef::fresh(NodeLeaseIdentity::from_node_identity(
                &crate::ownership::NodeIdentity::new("node-b", "epoch-b"),
            )),
            logical,
        );
        let operation = EndpointOperationId::fresh();
        let staged = slot
            .stage(
                slot.revision(),
                operation,
                EndpointCandidate {
                    binding: next.clone(),
                    kind: BindKind::Resume,
                },
            )
            .expect("stage resume");
        assert_eq!(
            slot.with_admission(&old, || panic!("old endpoint enqueued after seal")),
            EnqueueOutcome::RetryBeforeEnqueue
        );
        assert_eq!(
            slot.activate(operation, staged, ready_for_fresh()),
            Err(TransitionError::EvidenceMissing)
        );
        slot.activate(
            operation,
            staged,
            ActivationEvidence {
                resumed_sm_handoff_committed: true,
                ..ready_for_fresh()
            },
        )
        .expect("activate resume");
        assert_eq!(slot.active(), Some(&next));
        assert_eq!(
            slot.with_admission(&old, || panic!("old socket enqueued")),
            EnqueueOutcome::RetryBeforeEnqueue
        );
    }

    #[test]
    fn failed_resume_then_fresh_bind_reuses_physical_socket_not_binding_or_logical_state() {
        let mut slot = EndpointSlot::new(jid());
        let old = binding(new_connection(), LogicalResourceGeneration::fresh());
        publish_fresh(&mut slot, old.clone());
        let socket = new_connection();
        let failed = binding(socket.clone(), old.logical_generation);
        let failed_op = EndpointOperationId::fresh();
        let failed_revision = slot
            .stage(
                slot.revision(),
                failed_op,
                EndpointCandidate {
                    binding: failed.clone(),
                    kind: BindKind::Resume,
                },
            )
            .expect("stage failed resume");
        slot.compensate(
            failed_op,
            failed_revision,
            PredecessorOnCompensation::RetiredWithCustody,
        )
        .expect("compensate exact failed bind");
        assert_eq!(slot.active(), None);
        let fresh = binding(socket, LogicalResourceGeneration::fresh());
        publish_fresh(&mut slot, fresh.clone());
        assert_eq!(fresh.connection_ref, failed.connection_ref);
        assert_ne!(fresh.binding_generation, failed.binding_generation);
        assert_ne!(fresh.logical_generation, failed.logical_generation);
        assert_eq!(
            slot.compensate(
                failed_op,
                failed_revision,
                PredecessorOnCompensation::RetiredWithCustody
            ),
            Err(TransitionError::StaleOperation)
        );
        assert_eq!(slot.active(), Some(&fresh));
        // The compensated resume's binding fence cannot be minted again for a
        // later successor, even though it is no longer the active binding.
        let mut reused_failed_binding =
            binding(new_connection(), LogicalResourceGeneration::fresh());
        reused_failed_binding.binding_generation = failed.binding_generation;
        assert_eq!(
            slot.stage(
                slot.revision(),
                EndpointOperationId::fresh(),
                EndpointCandidate {
                    binding: reused_failed_binding,
                    kind: BindKind::Fresh,
                }
            ),
            Err(StageError::BindingGenerationReused)
        );
        assert_eq!(slot.active(), Some(&fresh));
        assert_eq!(
            slot.stage(
                slot.revision(),
                EndpointOperationId::fresh(),
                EndpointCandidate {
                    binding: binding(new_connection(), fresh.logical_generation),
                    kind: BindKind::Fresh,
                }
            ),
            Err(StageError::LogicalGenerationReused)
        );
    }

    #[test]
    fn slow_reserved_enqueue_rechecks_after_seal_and_never_runs() {
        let mut slot = EndpointSlot::new(jid());
        let old = binding(new_connection(), LogicalResourceGeneration::fresh());
        publish_fresh(&mut slot, old.clone());
        // The sender reserved capacity while the old endpoint was active.
        let reserved_for = old.clone();
        let successor = binding(new_connection(), LogicalResourceGeneration::fresh());
        let operation = EndpointOperationId::fresh();
        slot.stage(
            slot.revision(),
            operation,
            EndpointCandidate {
                binding: successor,
                kind: BindKind::Fresh,
            },
        )
        .expect("seal before commit");
        assert_eq!(
            slot.with_admission(&reserved_for, || panic!("reservation admitted after seal")),
            EnqueueOutcome::RetryBeforeEnqueue
        );
    }

    #[test]
    fn old_operation_and_revision_cannot_retire_successor() {
        let mut slot = EndpointSlot::new(jid());
        let old = binding(new_connection(), LogicalResourceGeneration::fresh());
        publish_fresh(&mut slot, old);
        let next = binding(new_connection(), LogicalResourceGeneration::fresh());
        let operation = EndpointOperationId::fresh();
        let before = slot.revision();
        let staged = slot
            .stage(
                before,
                operation,
                EndpointCandidate {
                    binding: next.clone(),
                    kind: BindKind::Fresh,
                },
            )
            .expect("stage successor");
        assert_eq!(
            slot.stage(
                before,
                EndpointOperationId::fresh(),
                EndpointCandidate {
                    binding: next.clone(),
                    kind: BindKind::Fresh,
                }
            ),
            Err(StageError::RevisionConflict)
        );
        assert_eq!(
            slot.compensate(
                EndpointOperationId::fresh(),
                staged,
                PredecessorOnCompensation::RetiredWithCustody
            ),
            Err(TransitionError::StaleOperation)
        );
        assert_eq!(
            slot.compensate(
                operation,
                before,
                PredecessorOnCompensation::RetiredWithCustody
            ),
            Err(TransitionError::StaleOperation)
        );
        slot.activate(operation, staged, ready_for_fresh())
            .expect("activate successor");
        assert_eq!(
            slot.compensate(
                operation,
                staged,
                PredecessorOnCompensation::RetiredWithCustody
            ),
            Err(TransitionError::StaleOperation)
        );
        assert_eq!(slot.active(), Some(&next));
    }

    #[test]
    fn unknown_reply_requires_journal_resolution_not_assumed_rollback() {
        let mut slot = EndpointSlot::new(jid());
        let operation = EndpointOperationId::fresh();
        let candidate = binding(new_connection(), LogicalResourceGeneration::fresh());
        let staged = slot
            .stage(
                slot.revision(),
                operation,
                EndpointCandidate {
                    binding: candidate.clone(),
                    kind: BindKind::Fresh,
                },
            )
            .expect("commit happened but reply was lost");
        let observation: CommitObservation<EndpointRevision> = CommitObservation::Unknown {
            operation_id: operation,
        };
        assert_eq!(
            observation,
            CommitObservation::Unknown {
                operation_id: operation
            }
        );
        assert_eq!(
            slot.disposition(operation),
            Some(OperationDisposition::Staged(staged))
        );
        assert_eq!(slot.active(), None);
        let activated = slot
            .activate(operation, staged, ready_for_fresh())
            .expect("finish known stage");
        assert_eq!(
            slot.disposition(operation),
            Some(OperationDisposition::Activated(activated))
        );
        assert_eq!(slot.active(), Some(&candidate));
    }

    #[test]
    fn cancelled_prepare_stays_sealed_until_exact_compensation() {
        let mut slot = EndpointSlot::new(jid());
        let old = binding(new_connection(), LogicalResourceGeneration::fresh());
        publish_fresh(&mut slot, old.clone());
        let operation = EndpointOperationId::fresh();
        let staged = slot
            .stage(
                slot.revision(),
                operation,
                EndpointCandidate {
                    binding: binding(new_connection(), LogicalResourceGeneration::fresh()),
                    kind: BindKind::Fresh,
                },
            )
            .expect("stage before cancellation");
        assert_eq!(
            slot.with_admission(&old, || panic!("old endpoint reopened")),
            EnqueueOutcome::RetryBeforeEnqueue
        );
        assert_eq!(
            slot.disposition(operation),
            Some(OperationDisposition::Staged(staged))
        );
        assert_eq!(
            slot.compensate(operation, staged, PredecessorOnCompensation::Unknown),
            Err(TransitionError::EvidenceMissing)
        );
        assert_eq!(
            slot.with_admission(&old, || panic!("unknown custody reopened old endpoint")),
            EnqueueOutcome::RetryBeforeEnqueue
        );
        let compensated = slot
            .compensate(
                operation,
                staged,
                PredecessorOnCompensation::RetiredWithCustody,
            )
            .expect("exact cleanup");
        assert_eq!(
            slot.disposition(operation),
            Some(OperationDisposition::Compensated(compensated))
        );
        assert_eq!(slot.active(), None);
        assert_eq!(
            slot.with_admission(&old, || panic!("retired predecessor reopened")),
            EnqueueOutcome::NoEndpoint
        );
    }

    #[test]
    fn untouched_predecessor_can_reopen_but_staged_generation_cannot_be_reused() {
        let mut slot = EndpointSlot::new(jid());
        let old = binding(new_connection(), LogicalResourceGeneration::fresh());
        publish_fresh(&mut slot, old.clone());
        let candidate = binding(new_connection(), LogicalResourceGeneration::fresh());
        let operation = EndpointOperationId::fresh();
        let staged = slot
            .stage(
                slot.revision(),
                operation,
                EndpointCandidate {
                    binding: candidate.clone(),
                    kind: BindKind::Fresh,
                },
            )
            .expect("stage new candidate");
        slot.compensate(
            operation,
            staged,
            PredecessorOnCompensation::StillLiveAndUnchanged,
        )
        .expect("reopen untouched predecessor");
        assert_eq!(slot.active(), Some(&old));
        assert_eq!(
            slot.stage(
                slot.revision(),
                EndpointOperationId::fresh(),
                EndpointCandidate {
                    binding: binding(new_connection(), candidate.logical_generation),
                    kind: BindKind::Fresh,
                }
            ),
            Err(StageError::LogicalGenerationReused)
        );
    }
}
