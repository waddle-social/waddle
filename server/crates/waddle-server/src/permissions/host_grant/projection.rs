//! The intended SpiceDB projection of a scope's canonical grants.
//!
//! A scope's projection is the affiliation family (`owner`, `admin`,
//! `member`, `outcast`) of every subject in every managed room, plus the
//! scope's [`ProjectionMarker`]. One `WriteRelationships` request writes all
//! of it:
//!
//! - for every canonical grant row, the relation of its affiliation is
//!   touched and the other three are deleted, so the request states the whole
//!   family of every subject the scope has ever granted;
//! - the previous marker is deleted and the next one touched;
//! - the request is conditioned on the exact previous marker. The first
//!   write of a lifecycle is instead conditioned on the lifecycle holding no
//!   marker and on the affiliation family of every managed room being empty,
//!   which is the evidence that the namespace is fresh.
//!
//! A request whose precondition fails writes nothing. Its writer stops: it
//! may not read the marker again and retry the same intent, because that
//! intent may be older than what replaced the marker. Reconciliation starts
//! again from the canonical state. A write also refuses, before any RPC, to
//! replace a marker whose generation is not older than its own.
//!
//! Any other failure of the request leaves its outcome unknown; the marker
//! is read back to learn whether it was applied.

use std::collections::BTreeSet;
use std::time::Duration;

use thiserror::Error;

use super::marker::ProjectionMarker;
use super::model::{
    Fingerprint, GrantGeneration, GrantSubject, ProjectionDigest, RoomAffiliation, ScopeLifecycleId,
};

/// Deadline of each projection RPC.
pub const HOST_GRANT_PROJECTION_RPC_TIMEOUT: Duration = Duration::from_secs(5);

/// One canonical grant row as it is projected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectedGrant {
    pub channel_id: String,
    pub subject: GrantSubject,
    pub affiliation: RoomAffiliation,
}

/// A relationship of the affiliation family: `channel:<channel_id>#<relation>@user:<subject>`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProjectedRelationship {
    pub channel_id: String,
    pub relation: String,
    pub subject: String,
}

/// The complete intended projection of one scope at one generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectionIntent {
    pub marker: ProjectionMarker,
    /// The authorization object of every managed room.
    pub channels: Vec<String>,
    /// Every canonical grant row, including revoked subjects.
    pub grants: Vec<ProjectedGrant>,
}

impl ProjectionIntent {
    /// The relationships the affiliation family holds once this intent is
    /// applied.
    pub fn expected_relationships(&self) -> BTreeSet<ProjectedRelationship> {
        self.grants
            .iter()
            .filter_map(|grant| {
                grant
                    .affiliation
                    .relation()
                    .map(|relation| ProjectedRelationship {
                        channel_id: grant.channel_id.clone(),
                        relation: relation.to_owned(),
                        subject: grant.subject.as_str().to_owned(),
                    })
            })
            .collect()
    }
}

/// The digest a marker binds: the policy version, the lifecycle and every
/// relationship of the intended projection, in a canonical order.
pub(crate) fn projection_digest(
    policy_version: u32,
    lifecycle: ScopeLifecycleId,
    grants: &[ProjectedGrant],
) -> Option<ProjectionDigest> {
    let mut relationships: Vec<(&str, &str, &str)> = grants
        .iter()
        .filter_map(|grant| {
            grant
                .affiliation
                .relation()
                .map(|relation| (grant.channel_id.as_str(), grant.subject.as_str(), relation))
        })
        .collect();
    relationships.sort_unstable();
    let mut fingerprint = Fingerprint::new("waddle:host-grant:projection:v1");
    fingerprint.number(i64::from(policy_version));
    fingerprint.text(&lifecycle.to_storage());
    fingerprint.number(relationships.len() as i64);
    for (channel_id, subject, relation) in relationships {
        fingerprint.text(channel_id);
        fingerprint.text(subject);
        fingerprint.text(relation);
    }
    ProjectionDigest::from_storage(&fingerprint.finish())
}

/// The marker relationships a lifecycle holds in the projection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MarkerObservation {
    Absent,
    Present(ProjectionMarker),
    /// More than one marker, or one that is not a canonical stamp.
    Malformed,
}

/// The projection of one scope, read at one SpiceDB revision when it holds a
/// marker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectionObservation {
    pub marker: MarkerObservation,
    /// The affiliation-family relationships of the scope's rooms whose
    /// subject is a plain account.
    pub relationships: BTreeSet<ProjectedRelationship>,
    /// The family holds a relationship this projection never writes (a
    /// subject that is not a plain account, or a caveat), or more rows than
    /// a scope can hold.
    pub malformed_family: bool,
}

/// The acknowledged outcome of a projection write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectionWrite {
    /// The marker and the relationships were written in one transaction.
    Applied,
    /// The projection no longer held the marker the write was conditioned
    /// on. Nothing was written, and this writer's intent may be stale.
    PreconditionFailed,
}

/// A projection write refused before any RPC.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum ProjectionRefusal {
    #[error("a projection write may only replace a marker of an older generation")]
    GenerationRegression,
    #[error("a projection write may only replace a marker of its own lifecycle")]
    LifecycleMismatch,
    #[error("a projection write may only replace a marker of its own authority incarnation")]
    IncarnationMismatch,
    #[error("the projection does not fit one bounded write request")]
    TooLarge,
}

impl ProjectionIntent {
    /// Refuse a write that would move the marker to an older or equal
    /// generation, to another lifecycle or to another incarnation.
    pub fn check_replaces(
        &self,
        previous: Option<&ProjectionMarker>,
    ) -> Result<(), ProjectionRefusal> {
        let Some(previous) = previous else {
            return Ok(());
        };
        if previous.lifecycle != self.marker.lifecycle {
            return Err(ProjectionRefusal::LifecycleMismatch);
        }
        if previous.incarnation != self.marker.incarnation {
            return Err(ProjectionRefusal::IncarnationMismatch);
        }
        if previous.generation >= self.marker.generation {
            return Err(ProjectionRefusal::GenerationRegression);
        }
        Ok(())
    }
}

/// The canonical state of a scope that a reconciliation projects, read in
/// one PostgreSQL snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectionPlan {
    pub intent: ProjectionIntent,
    /// The latest generation whose projection was verified and activated.
    pub projected_generation: Option<GrantGeneration>,
}

impl ProjectionPlan {
    pub fn marker(&self) -> &ProjectionMarker {
        &self.intent.marker
    }

    /// Whether the current generation still waits for its projection.
    pub fn pending(&self) -> bool {
        self.projected_generation != Some(self.intent.marker.generation)
    }
}
