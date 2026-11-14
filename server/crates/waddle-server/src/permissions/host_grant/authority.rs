//! Projection, verification and activation of canonical host grants.
//!
//! [`HostGrantAuthority::reconcile`] is the only way a recorded increase of
//! access becomes effective, and it is the recovery path as well: it holds
//! no state of its own, so any replica can run it at any time for any scope,
//! after a crash, a cancelled caller or a lost reply. One pass:
//!
//! 1. reads the scope's current canonical intent in one PostgreSQL snapshot
//!    (only the current operation has an intent; a superseded one has none);
//! 2. reads the scope's projection marker;
//! 3. decides from the marker:
//!    - it is the current operation's marker: the write was applied, whether
//!      or not its reply arrived;
//!    - it is the marker the journal records for an older generation: the
//!      current intent is written, conditioned on exactly that marker;
//!    - there is none: the current intent is written if no projection of
//!      this lifecycle was ever activated, conditioned on the namespace being
//!      fresh, and the scope is otherwise left as it is;
//!    - anything else (a malformed marker, another incarnation, a generation
//!      the journal does not hold or that is not older): nothing is written;
//! 4. reads the projection back at the marker's revision and compares the
//!    affiliation family with the canonical rows exactly, which yields the
//!    [`VerifiedProjection`] witness;
//! 5. activates the exact operation in PostgreSQL, which accepts nothing
//!    but that witness.
//!
//! The pass reads PostgreSQL before SpiceDB, so another replica can record
//! and project a newer operation in between. A marker that is not older
//! than the plan is therefore compared with the canonical state again: the
//! pass reports a contradiction only if the canonical state is still the
//! one it read, and [`Reconciliation::Superseded`] otherwise.
//!
//! A pass never repeats a write whose precondition failed and never writes
//! an intent older than the marker it observes: a writer that lost a race
//! reports [`Reconciliation::Superseded`], and the next pass starts from the
//! canonical state again. While SpiceDB does not answer, the pass reports
//! [`Reconciliation::ProjectionUnknown`] and the grant stays recorded and
//! not effective; admission, which reads PostgreSQL only, is unaffected.
//!
//! A marker that the canonical journal contradicts (`AheadOfCanonical`,
//! `UnknownMarker`, or a `ForeignIncarnation` the journal never held) is
//! evidence that the database lost operations: besides reporting the
//! contradiction, the pass closes the scope as `fenced`
//! (`HostGrantStore::fence_scope`), as the restore fence does.
//!
//! A scope a restore transition closed is reconstructed by the same pass.
//! When its marker was written under another authority incarnation, the
//! pass first records what the projection and the journal witness prove
//! the restored database lost (`restore::reconstruction_targets`: only
//! lowerings, and the subjects only the projection holds, at `none` or
//! `outcast`), then takes the projection over: it writes the current intent
//! conditioned on exactly that marker, reads it back and activates it. The
//! scope is then `reconstructed`, and admits again once an operator reopens
//! it. A marker that is missing after an activation or malformed, a family
//! that holds what the projection never writes, and changes beyond the
//! scope's row bound leave the scope closed.
//!
//! Every committed mutation brings the scope's journal witness and the
//! position witness up to date on a task of its own
//! (`witness::sync_witnesses_after_commit`), after the commit and without
//! the caller waiting for SpiceDB.

use std::collections::BTreeSet;

use kameo::actor::ActorRef;
use kameo::error::SendError;

use super::error::HostGrantError;
use super::issuance::{IssuanceBasis, IssuancePolicy};
use super::marker::ProjectionMarker;
use super::model::{
    EnrollScope, GrantGeneration, GrantOperationId, GrantSubject, MutationReceipt, RoomAffiliation,
    ScopeLifecycleId, SetAffiliation,
};
use super::projection::{
    MarkerObservation, ProjectedRelationship, ProjectionObservation, ProjectionPlan,
    ProjectionWrite, RestoredPlan,
};
use super::restore::{
    reconstruction_targets, FenceEvidence, ReconstructScope, ReconstructionChange,
};
use super::store::{Activation, HostGrantStore};
use super::witness::{
    head_witness_resource, sync_journal_witness, sync_position_witness,
    sync_witnesses_after_commit, WitnessSync,
};
use crate::permissions::{
    ObserveHostGrantProjection, PermissionActor, PermissionError, ReadHostGrantWitness,
    TakeOverHostGrantProjection, WriteHostGrantProjection,
};

/// Projection state that no reconciliation pass may write over. Nothing is
/// written; a recorded increase stays not effective until an operator
/// resolves the contradiction. A contradiction that shows the database lost
/// operations also closes the scope as `fenced`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectionContradiction {
    /// More than one marker, or one that is not a canonical stamp.
    MalformedMarker,
    /// A projection of this lifecycle was activated and its marker is gone.
    MarkerMissing,
    /// The marker belongs to another authority incarnation.
    ForeignIncarnation,
    /// The marker's generation is not older than the canonical one, and the
    /// marker is not the canonical one.
    AheadOfCanonical,
    /// The journal records no such operation for the marker's generation.
    UnknownMarker,
    /// The marker is the canonical one and the affiliation family differs
    /// from the canonical rows.
    ContentMismatch,
    /// The lifecycle has no marker and its rooms' affiliation family already
    /// holds relationships: the namespace a first projection requires to be
    /// empty is not.
    NamespaceNotFresh,
    /// The projection a reconstruction would replace holds a relationship
    /// the projection never writes, so what it records cannot be read.
    MalformedFamily,
    /// Recording every subject the projection a reconstruction would replace
    /// holds would exceed the scope's grant-row bound.
    ReconstructionTooLarge,
}

/// The result of one reconciliation pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reconciliation {
    /// The current operation's projection was verified and activated.
    Activated(GrantGeneration),
    /// The current operation of a scope a restore transition closed was
    /// verified and activated; the scope stays closed until an operator
    /// reopens it.
    Reconstructed(GrantGeneration),
    /// Nothing was pending.
    AlreadyActive(GrantGeneration),
    /// Another operation or another writer got ahead of this pass. Nothing
    /// stale was written or activated; a new pass starts from the canonical
    /// state.
    Superseded,
    /// SpiceDB did not give an acknowledged answer. The write may or may not
    /// have been applied; the next pass reads the marker to find out.
    ProjectionUnknown,
    Contradiction(ProjectionContradiction),
}

/// Evidence that a scope's projection was read back and holds exactly the
/// canonical intent of one operation: its marker, and the affiliation family
/// of its rooms equal to the canonical grant rows.
///
/// Verification is the only code that constructs it, and
/// `HostGrantStore::activate` accepts nothing else, so an increase of access
/// cannot become effective without a projection that was read back.
#[derive(Debug)]
pub struct VerifiedProjection {
    marker: ProjectionMarker,
}

impl VerifiedProjection {
    /// The witness for `plan`, if `observation` shows exactly its intent.
    fn of(plan: &ProjectionPlan, observation: &ProjectionObservation) -> Option<Self> {
        let exact = observation.marker == MarkerObservation::Present(plan.marker().clone())
            && !observation.malformed_family
            && observation.relationships == plan.intent.expected_relationships();
        exact.then(|| Self {
            marker: plan.marker().clone(),
        })
    }

    /// The marker of the operation whose projection was verified.
    pub(crate) fn marker(&self) -> &ProjectionMarker {
        &self.marker
    }

    /// A witness for `marker` without a projection behind it, for fixtures
    /// of suites that exercise the canonical store without SpiceDB and for
    /// cases that replay a delayed activation.
    #[cfg(feature = "test-support")]
    pub(crate) fn without_projection_for_test(marker: ProjectionMarker) -> Self {
        Self { marker }
    }
}

/// The canonical store together with its projection and issuance policy.
#[derive(Clone)]
pub struct HostGrantAuthority {
    store: HostGrantStore,
    permissions: ActorRef<PermissionActor>,
    issuance: IssuancePolicy,
}

impl HostGrantAuthority {
    /// `issuance` carries the trusted-home set this authority issues under.
    pub fn new(
        store: HostGrantStore,
        permissions: ActorRef<PermissionActor>,
        issuance: IssuancePolicy,
    ) -> Self {
        Self {
            store,
            permissions,
            issuance,
        }
    }

    pub fn store(&self) -> &HostGrantStore {
        &self.store
    }

    /// Enroll a scope under this authority's issuance policy.
    pub async fn enroll_scope(
        &self,
        command: &EnrollScope,
    ) -> Result<MutationReceipt, HostGrantError> {
        let receipt = self.store.enroll_scope(command, &self.issuance).await?;
        self.after_commit(command.lifecycle);
        Ok(receipt)
    }

    /// Record an affiliation change under this authority's issuance policy.
    /// A receipt means the change is recorded, not that an increase is
    /// effective: [`Self::reconcile`] makes it so. The witnesses follow the
    /// commit on a task of their own; the receipt does not wait for them.
    pub async fn set_affiliation(
        &self,
        command: &SetAffiliation,
    ) -> Result<MutationReceipt, HostGrantError> {
        let receipt = self.store.set_affiliation(command, &self.issuance).await?;
        self.after_commit(command.lifecycle);
        Ok(receipt)
    }

    /// Bring a scope's journal witness and the position witness up to date
    /// now, and answer what the journal witness writer did. A committed
    /// mutation does this on a task of its own; this is for a caller that
    /// wants to wait for it.
    pub async fn sync_witnesses(
        &self,
        lifecycle: ScopeLifecycleId,
    ) -> Result<WitnessSync, HostGrantError> {
        let journal = sync_journal_witness(&self.store, &self.permissions, lifecycle).await?;
        sync_position_witness(&self.store, &self.permissions).await?;
        Ok(journal)
    }

    /// Spawn the witness writers for a scope after a commit, so that SpiceDB
    /// is never on the commit's path.
    fn after_commit(&self, lifecycle: ScopeLifecycleId) {
        tokio::spawn(sync_witnesses_after_commit(
            self.store.clone(),
            self.permissions.clone(),
            lifecycle,
        ));
    }

    /// Read the projection of the scope `plan` describes.
    pub async fn observe(
        &self,
        plan: &ProjectionPlan,
    ) -> Result<ProjectionObservation, HostGrantError> {
        self.permissions
            .ask(ObserveHostGrantProjection {
                lifecycle: plan.marker().lifecycle,
                channels: plan.intent.channels.clone(),
            })
            .await
            .map_err(projection_store_error)
    }

    /// Write `plan`'s intent, conditioned on exactly `previous`. An error
    /// other than a refusal leaves the outcome unknown.
    pub async fn write(
        &self,
        plan: &ProjectionPlan,
        previous: Option<&ProjectionMarker>,
    ) -> Result<ProjectionWrite, HostGrantError> {
        plan.intent.check_replaces(previous)?;
        self.permissions
            .ask(WriteHostGrantProjection {
                previous: previous.cloned(),
                intent: plan.intent.clone(),
            })
            .await
            .map_err(projection_store_error)
    }

    /// Replace the projection of a scope a restore transition closed:
    /// write `plan`'s intent conditioned on exactly `previous`, a marker of
    /// another authority incarnation. Only the store produces a
    /// [`RestoredPlan`]. An error other than a refusal leaves the outcome
    /// unknown.
    pub async fn take_over(
        &self,
        plan: &RestoredPlan,
        previous: &ProjectionMarker,
    ) -> Result<ProjectionWrite, HostGrantError> {
        plan.plan().intent.check_takes_over(previous)?;
        self.permissions
            .ask(TakeOverHostGrantProjection {
                previous: previous.clone(),
                plan: plan.clone(),
            })
            .await
            .map_err(projection_store_error)
    }

    /// Whether `observation` shows exactly `plan`'s intent.
    pub fn verified(plan: &ProjectionPlan, observation: &ProjectionObservation) -> bool {
        VerifiedProjection::of(plan, observation).is_some()
    }

    /// One reconciliation pass for a scope. See the module documentation.
    pub async fn reconcile(
        &self,
        lifecycle: ScopeLifecycleId,
    ) -> Result<Reconciliation, HostGrantError> {
        let plan = self.store.projection_plan(lifecycle).await?;
        let generation = plan.marker().generation;
        if !plan.pending() {
            return Ok(Reconciliation::AlreadyActive(generation));
        }
        let Ok(observation) = self.observe(&plan).await else {
            return Ok(Reconciliation::ProjectionUnknown);
        };
        let previous = match observation.marker {
            MarkerObservation::Malformed => {
                return Ok(Reconciliation::Contradiction(
                    ProjectionContradiction::MalformedMarker,
                ))
            }
            MarkerObservation::Absent if plan.projected_generation.is_some() => {
                return Ok(Reconciliation::Contradiction(
                    ProjectionContradiction::MarkerMissing,
                ))
            }
            MarkerObservation::Absent => None,
            MarkerObservation::Present(ref marker) if marker == plan.marker() => {
                return self.verify_and_activate(&plan, Some(observation)).await;
            }
            MarkerObservation::Present(marker) => {
                if marker.incarnation != plan.marker().incarnation {
                    if let Some(restored) = plan.into_restored() {
                        return self
                            .reconstruct(
                                restored,
                                &observation.relationships,
                                observation.malformed_family,
                                marker,
                            )
                            .await;
                    }
                    // A marker of an incarnation this journal never held
                    // was written by an authority the database lost.
                    if !self
                        .store
                        .journal_holds_incarnation(lifecycle, marker.incarnation)
                        .await?
                    {
                        self.fence(lifecycle, FenceEvidence::UnknownIncarnation(marker))
                            .await?;
                    }
                    return Ok(Reconciliation::Contradiction(
                        ProjectionContradiction::ForeignIncarnation,
                    ));
                }
                if marker.generation >= generation {
                    // The marker was read after the plan. If the canonical
                    // state has moved on since, the marker may be the newer
                    // operation's and this pass's plan is the stale one.
                    let current = self.store.projection_plan(lifecycle).await?;
                    if current.marker() != plan.marker() {
                        return Ok(Reconciliation::Superseded);
                    }
                    self.fence(lifecycle, FenceEvidence::ProjectionAhead(marker))
                        .await?;
                    return Ok(Reconciliation::Contradiction(
                        ProjectionContradiction::AheadOfCanonical,
                    ));
                }
                let recorded = self
                    .store
                    .recorded_marker(lifecycle, marker.generation)
                    .await?;
                if recorded.as_ref() != Some(&marker) {
                    self.fence(lifecycle, FenceEvidence::ProjectionAhead(marker))
                        .await?;
                    return Ok(Reconciliation::Contradiction(
                        ProjectionContradiction::UnknownMarker,
                    ));
                }
                Some(marker)
            }
        };
        match self.write(&plan, previous.as_ref()).await {
            Ok(ProjectionWrite::Applied) => self.verify_and_activate(&plan, None).await,
            // The first write of a lifecycle found its namespace not fresh:
            // another replica wrote the marker meanwhile, or the rooms'
            // affiliation family already holds relationships that this
            // authority did not write.
            Ok(ProjectionWrite::PreconditionFailed) if previous.is_none() => {
                Ok(match self.observe(&plan).await {
                    Ok(ProjectionObservation {
                        marker: MarkerObservation::Absent,
                        ..
                    }) => Reconciliation::Contradiction(ProjectionContradiction::NamespaceNotFresh),
                    Ok(_) => Reconciliation::Superseded,
                    Err(_) => Reconciliation::ProjectionUnknown,
                })
            }
            // This pass's view of the marker is out of date. It stops here:
            // retrying its intent against a newer marker could apply an
            // older grant after a newer revoke.
            Ok(ProjectionWrite::PreconditionFailed) => Ok(Reconciliation::Superseded),
            Err(HostGrantError::Projection(refusal)) => Err(refusal.into()),
            Err(_) => Ok(Reconciliation::ProjectionUnknown),
        }
    }

    /// Reconcile every scope that holds a recorded operation whose
    /// projection is not yet activated: the pass a replica runs when it
    /// starts, and whenever it wants pending grants to progress.
    ///
    /// The pass follows the pending scopes page by page to the end, so a
    /// scope that cannot progress (a contradiction, a projection that does
    /// not answer) is visited once per pass and never keeps a later scope
    /// from being reconciled.
    pub async fn recover_pending(
        &self,
    ) -> Result<Vec<(ScopeLifecycleId, Result<Reconciliation, HostGrantError>)>, HostGrantError>
    {
        let mut outcomes = Vec::new();
        let mut after = None;
        loop {
            let page = self.store.pending_scopes(after).await?;
            let Some(last) = page.last().copied() else {
                break;
            };
            for lifecycle in page {
                outcomes.push((lifecycle, self.reconcile(lifecycle).await));
            }
            after = Some(last);
        }
        Ok(outcomes)
    }

    /// Reconstruct a scope a restore transition closed whose projection
    /// holds `previous`, a marker of another authority incarnation.
    ///
    /// The restored database lost operations when the journal does not
    /// record `previous`, or does not record the scope's journal witness:
    /// the canonical grants are then first lowered to what the projection
    /// holds; a subject only the projection holds is recorded at `none` or
    /// `outcast` either way. Those changes are one `reconstruct` operation,
    /// after which the pass reads the plan again, takes the projection
    /// over, reads it back and activates it, which leaves the scope
    /// `reconstructed`. A change issuance refuses at its affiliation is
    /// recorded lower, at `none` or `outcast`; one it refuses even there (a
    /// service identity) is left out, and verification then reports the
    /// relationship it leaves as a contradiction.
    async fn reconstruct(
        &self,
        plan: RestoredPlan,
        observed: &BTreeSet<ProjectedRelationship>,
        malformed_family: bool,
        previous: ProjectionMarker,
    ) -> Result<Reconciliation, HostGrantError> {
        if malformed_family {
            return Ok(Reconciliation::Contradiction(
                ProjectionContradiction::MalformedFamily,
            ));
        }
        let lifecycle = plan.plan().marker().lifecycle;
        let projection_lost = self
            .store
            .recorded_marker(lifecycle, previous.generation)
            .await?
            .as_ref()
            != Some(&previous);
        let Ok(journal_lost) = self.journal_witness_lost(lifecycle).await else {
            return Ok(Reconciliation::ProjectionUnknown);
        };
        let lost = projection_lost || journal_lost;
        let changes: Vec<ReconstructionChange> =
            reconstruction_targets(&plan.plan().intent, observed, lost)
                .into_iter()
                .filter_map(|target| {
                    let subject = GrantSubject::from_storage(&target.subject)?;
                    let (affiliation, basis) = self.issuable(&subject, target.affiliation)?;
                    Some(ReconstructionChange {
                        channel_id: target.channel_id,
                        subject,
                        affiliation,
                        basis,
                    })
                })
                .collect();
        let mut plan = plan;
        if !changes.is_empty() {
            let command = ReconstructScope {
                operation: GrantOperationId::generate(),
                lifecycle,
                expected_generation: plan.plan().marker().generation,
                replaced: previous.clone(),
                changes,
            };
            match self
                .store
                .record_reconstruction(&command, self.issuance.served_account_domains())
                .await
            {
                Ok(_) => {}
                Err(HostGrantError::StaleGeneration { .. } | HostGrantError::ScopeNotRestored) => {
                    return Ok(Reconciliation::Superseded)
                }
                Err(HostGrantError::GrantLimit) => {
                    return Ok(Reconciliation::Contradiction(
                        ProjectionContradiction::ReconstructionTooLarge,
                    ))
                }
                Err(error) => return Err(error),
            }
            self.after_commit(lifecycle);
            plan = match self.store.projection_plan(lifecycle).await?.into_restored() {
                Some(plan) => plan,
                None => return Ok(Reconciliation::Superseded),
            };
        }
        match self.take_over(&plan, &previous).await {
            Ok(ProjectionWrite::Applied) => self.verify_and_activate(plan.plan(), None).await,
            // The marker changed since it was read: another pass took the
            // projection over, or a writer replaced it. This pass stops.
            Ok(ProjectionWrite::PreconditionFailed) => Ok(Reconciliation::Superseded),
            Err(HostGrantError::Projection(refusal)) => Err(refusal.into()),
            Err(_) => Ok(Reconciliation::ProjectionUnknown),
        }
    }

    /// Whether the scope's journal witness names an operation the journal
    /// does not record: the restored database lost it, whether or not its
    /// projection was written. An absent or malformed witness shows
    /// nothing.
    async fn journal_witness_lost(
        &self,
        lifecycle: ScopeLifecycleId,
    ) -> Result<bool, HostGrantError> {
        let listed = self
            .permissions
            .ask(ReadHostGrantWitness {
                resource: head_witness_resource(lifecycle),
            })
            .await
            .map_err(projection_store_error)?;
        let Some(Ok(Some(stamp))) = listed.as_ref().map(|listed| listed.single_stamp()) else {
            return Ok(false);
        };
        let Some(head) = ProjectionMarker::parse(lifecycle, stamp) else {
            return Ok(false);
        };
        Ok(self
            .store
            .recorded_marker(lifecycle, head.generation)
            .await?
            .as_ref()
            != Some(&head))
    }

    /// Close a scope on evidence a pass found, as the restore fence does.
    async fn fence(
        &self,
        lifecycle: ScopeLifecycleId,
        evidence: FenceEvidence,
    ) -> Result<(), HostGrantError> {
        if self.store.fence_scope(lifecycle, &evidence).await? {
            tracing::error!(
                evidence = evidence.kind(),
                "A host-grant scope was closed: its projection records what its canonical store \
                 does not (docs/operations/authz-host-grant-restore.md)"
            );
            self.after_commit(lifecycle);
        }
        Ok(())
    }

    /// The affiliation and basis a reconstruction records for a subject: the
    /// target when issuance accepts it, else no higher than `none`.
    fn issuable(
        &self,
        subject: &GrantSubject,
        target: RoomAffiliation,
    ) -> Option<(RoomAffiliation, IssuanceBasis)> {
        if let Ok(basis) = self.issuance.check(subject, target) {
            return Some((target, basis));
        }
        let lowered = target.min(RoomAffiliation::None);
        self.issuance
            .check(subject, lowered)
            .ok()
            .map(|basis| (lowered, basis))
    }

    /// Compare the projection with the plan and activate the exact
    /// operation. `observation` is reused when the caller has just read the
    /// projection and found the plan's own marker.
    async fn verify_and_activate(
        &self,
        plan: &ProjectionPlan,
        observation: Option<ProjectionObservation>,
    ) -> Result<Reconciliation, HostGrantError> {
        let observation = match observation {
            Some(observation) => observation,
            None => match self.observe(plan).await {
                Ok(observation) => observation,
                Err(_) => return Ok(Reconciliation::ProjectionUnknown),
            },
        };
        let Some(verified) = VerifiedProjection::of(plan, &observation) else {
            return Ok(match observation.marker {
                MarkerObservation::Present(ref marker) if marker == plan.marker() => {
                    Reconciliation::Contradiction(ProjectionContradiction::ContentMismatch)
                }
                MarkerObservation::Malformed => {
                    Reconciliation::Contradiction(ProjectionContradiction::MalformedMarker)
                }
                // Another writer has replaced the marker since.
                MarkerObservation::Present(_) | MarkerObservation::Absent => {
                    Reconciliation::Superseded
                }
            });
        };
        Ok(match self.store.activate(&verified).await? {
            Activation::Activated => Reconciliation::Activated(plan.marker().generation),
            Activation::Reconstructed => Reconciliation::Reconstructed(plan.marker().generation),
            Activation::AlreadyActive => Reconciliation::AlreadyActive(plan.marker().generation),
            Activation::Superseded => Reconciliation::Superseded,
        })
    }
}

/// A failed ask of the permission actor: the projection's answer is unknown.
pub(super) fn projection_store_error<M>(error: SendError<M, PermissionError>) -> HostGrantError {
    match error {
        SendError::HandlerError(PermissionError::HostGrantProjection(refusal)) => {
            HostGrantError::Projection(refusal)
        }
        SendError::HandlerError(error) => HostGrantError::ProjectionStore(error),
        _ => HostGrantError::ProjectionStore(PermissionError::SpiceDbError(
            "the permission actor did not answer".to_owned(),
        )),
    }
}
