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

use kameo::actor::ActorRef;
use kameo::error::SendError;

use super::error::HostGrantError;
use super::issuance::IssuancePolicy;
use super::marker::ProjectionMarker;
use super::model::{
    EnrollScope, GrantGeneration, MutationReceipt, ScopeLifecycleId, SetAffiliation,
};
use super::projection::{
    MarkerObservation, ProjectionObservation, ProjectionPlan, ProjectionWrite,
};
use super::store::{Activation, HostGrantStore};
use crate::permissions::{
    ObserveHostGrantProjection, PermissionActor, PermissionError, WriteHostGrantProjection,
};

/// Projection state that no reconciliation pass may write over. The scope
/// keeps admitting from its effective grants; the recorded increase stays
/// not effective until an operator resolves the contradiction.
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
}

/// The result of one reconciliation pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reconciliation {
    /// The current operation's projection was verified and activated.
    Activated(GrantGeneration),
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
        self.store.enroll_scope(command, &self.issuance).await
    }

    /// Record an affiliation change under this authority's issuance policy.
    /// A receipt means the change is recorded, not that an increase is
    /// effective: [`Self::reconcile`] makes it so.
    pub async fn set_affiliation(
        &self,
        command: &SetAffiliation,
    ) -> Result<MutationReceipt, HostGrantError> {
        self.store.set_affiliation(command, &self.issuance).await
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
                    return Ok(Reconciliation::Contradiction(
                        ProjectionContradiction::ForeignIncarnation,
                    ));
                }
                if marker.generation >= generation {
                    // The marker was read after the plan. If the canonical
                    // state has moved on since, the marker may be the newer
                    // operation's and this pass's plan is the stale one.
                    let current = self.store.projection_plan(lifecycle).await?;
                    return Ok(if current.marker() == plan.marker() {
                        Reconciliation::Contradiction(ProjectionContradiction::AheadOfCanonical)
                    } else {
                        Reconciliation::Superseded
                    });
                }
                let recorded = self
                    .store
                    .recorded_marker(lifecycle, marker.generation)
                    .await?;
                if recorded.as_ref() != Some(&marker) {
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
            Activation::AlreadyActive => Reconciliation::AlreadyActive(plan.marker().generation),
            Activation::Superseded => Reconciliation::Superseded,
        })
    }
}

/// A failed ask of the permission actor: the projection's answer is unknown.
fn projection_store_error<M>(error: SendError<M, PermissionError>) -> HostGrantError {
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
