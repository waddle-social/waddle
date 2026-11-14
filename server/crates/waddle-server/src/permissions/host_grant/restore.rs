//! The restore incarnation of the host-grant authority (ADR-0020 D5
//! condition B7, gate G13).
//!
//! Restoring PostgreSQL from a backup, or recovering it to an earlier point
//! in time, brings back every scope's older generation together with its
//! grants, its `active` flag and the authority incarnation that state was
//! bound to. Nothing inside the restored database can tell it from the live
//! one, so the incarnation is bound to three witnesses that a restore of the
//! database does not bring back:
//!
//! - the declared restore epoch ([`RestoreEpoch`]): an operator-supplied
//!   counter in the deployment configuration of every replica and of the
//!   owner phase, raised once for each restore. The database records the
//!   epoch its incarnation was established under, and a replica serves only
//!   a database that records its own configured epoch, so a database
//!   restored to before the latest restore transition is refused;
//! - the database lineage (`_lineage`), which a restore into another cluster
//!   or database rotates through adoption. The owner phase binds the
//!   incarnation to the lineage and refuses an adopted database until the
//!   restore is declared;
//! - the SpiceDB projection markers, which name the incarnation and the
//!   generation each scope's projection was last written for. A replica
//!   refuses to start while a marker names a scope, a generation or an
//!   incarnation the canonical journal does not record: the database lost
//!   operations that were projected, and the restore was not declared.
//!
//! Declaring a restore is an owner-phase act
//! ([`HostGrantStore::prepare_restore_incarnation`](super::HostGrantStore::prepare_restore_incarnation)).
//! When the configured epoch exceeds the stored one it mints a new
//! incarnation, records the epoch and the live lineage, and closes every
//! scope that is not retired or exhausted as `restored`, bound to the new
//! incarnation at its next generation, in one transaction. From that commit:
//!
//! - no admission, mutation or activation can use the earlier incarnation or
//!   any generation before the transition, so old queued and in-flight work
//!   (a command decided under an older generation, a prepared admission
//!   witness, a reconciliation pass that read an older plan) is refused or
//!   superseded;
//! - a replica attached to the earlier incarnation admits nothing, because
//!   every acceptance compares the incarnation, the epoch and the start of
//!   the PostgreSQL server it reads with those its attachment verified
//!   ([`AuthorityBinding`]);
//! - a restored scope admits nothing until reconciliation has rebuilt its
//!   projection from the canonical rows, read it back and activated it
//!   under the new incarnation. Reconstruction never grants from the
//!   projection: where the projection shows that the restored database lost
//!   projected operations, it lowers the canonical grants to what the
//!   projection holds, so a revoke or a ban the restore lost stays in force
//!   ([`reconstruction_targets`]).
//!
//! The decisions and their limits are recorded in
//! `server/docs/planning/host-grant-authority-2047.md`, "Restore incarnation
//! decisions", and the operator procedure in
//! `docs/operations/authz-host-grant-restore.md` at the repository root.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::time::Duration;

use kameo::actor::ActorRef;
use thiserror::Error;

use super::authority::projection_store_error;
use super::error::HostGrantError;
use super::issuance::IssuanceBasis;
use super::marker::ProjectionMarker;
use super::model::{
    AuthorityIncarnation, ClosedReason, Fingerprint, GrantGeneration, GrantOperationId,
    GrantSubject, RoomAffiliation, ScopeLifecycleId,
};
use super::projection::{MarkerObservation, ProjectedRelationship, ProjectionIntent};
use super::store::HostGrantStore;
use crate::permissions::{ListHostGrantProjectionMarkers, PermissionActor};

/// Deadline of the startup read of every projection marker.
pub const HOST_GRANT_RESTORE_SCAN_TIMEOUT: Duration = Duration::from_secs(60);

/// The deployment's declared restore epoch: zero until the first restore,
/// raised by one (or more) for each restore the operator declares. It is
/// configuration, held outside the database it fences.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RestoreEpoch(u32);

impl RestoreEpoch {
    /// The epoch of a deployment that has never declared a restore.
    pub const INITIAL: Self = Self(0);

    pub fn new(value: u32) -> Self {
        Self(value)
    }

    pub fn value(self) -> u32 {
        self.0
    }

    /// Parse the configured spelling: decimal digits only.
    pub fn parse(text: &str) -> Option<Self> {
        if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        text.parse::<u32>().ok().map(Self)
    }

    pub(crate) fn from_storage(value: i64) -> Option<Self> {
        u32::try_from(value).ok().map(Self)
    }

    pub(crate) fn to_storage(self) -> i64 {
        i64::from(self.0)
    }
}

impl fmt::Display for RestoreEpoch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// What a replica's attachment verified about the authority, and what every
/// acceptance it makes compares with the database it reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorityBinding {
    pub incarnation: AuthorityIncarnation,
    pub restore_epoch: RestoreEpoch,
    /// When the PostgreSQL server the binding was read from started, in
    /// microseconds since the Unix epoch. A restore restarts the server, so
    /// a restore under a running replica changes it; so does any other
    /// restart, after which the replica admits no managed room until it is
    /// restarted and has verified the restore fence again.
    pub server_started_at_us: i64,
}

/// Why the authority is not served: a restore that was not declared, or a
/// configuration that does not match the declared one. A refusal names no
/// scope, room or account.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum RestoreFenceRefusal {
    #[error(
        "the configured host-grant restore epoch {configured} differs from the epoch {stored} \
         the authority records; see docs/operations/authz-host-grant-restore.md"
    )]
    EpochMismatch {
        configured: RestoreEpoch,
        stored: RestoreEpoch,
    },
    #[error(
        "the database lineage changed since the host-grant incarnation was established: the \
         database was restored or cloned; declare the restore by raising the restore epoch"
    )]
    LineageChanged,
    #[error(
        "the host-grant projection records an operation the canonical journal does not: the \
         database lost operations and the restore was not declared"
    )]
    ProjectionAhead,
    #[error(
        "the host-grant projection was written by an authority incarnation the canonical \
         journal does not record: the database is older than the projection"
    )]
    UnknownIncarnation,
    #[error(
        "the host-grant projection holds a scope the canonical store does not: the database \
         lost an enrollment"
    )]
    OrphanedProjection,
}

impl RestoreFenceRefusal {
    /// A stable name of the refusal for logs.
    pub fn kind(self) -> &'static str {
        match self {
            Self::EpochMismatch { .. } => "epoch_mismatch",
            Self::LineageChanged => "lineage_changed",
            Self::ProjectionAhead => "projection_ahead",
            Self::UnknownIncarnation => "unknown_incarnation",
            Self::OrphanedProjection => "orphaned_projection",
        }
    }
}

/// What the owner phase did with the authority incarnation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RestoreDeclaration {
    /// The configured epoch is the stored one and the database lineage is
    /// the one the incarnation is bound to.
    Unchanged,
    /// The incarnation was bound to the database lineage for the first time.
    LineageBound,
    /// A restore was declared: a new incarnation was established under the
    /// configured epoch and every scope that was not retired or exhausted
    /// was closed as `restored`.
    Transitioned {
        previous: AuthorityIncarnation,
        incarnation: AuthorityIncarnation,
        epoch: RestoreEpoch,
        restored_scopes: usize,
    },
}

/// A marker read from the whole projection store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListedProjectionMarker {
    /// The lifecycle the marker resource names; `None` when the resource ID
    /// is not a lifecycle identity.
    pub lifecycle: Option<ScopeLifecycleId>,
    pub marker: MarkerObservation,
}

/// What the startup fence found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RestoreFenceReport {
    /// The projection markers compared with the canonical store.
    pub markers: usize,
    /// Markers of scopes a restore transition closed, which reconstruction
    /// replaces.
    pub awaiting_reconstruction: usize,
}

/// Verify, before a replica serves anything, that the canonical store is the
/// authority the deployment declared and not an undeclared restore of it.
///
/// A database that holds no scope, beside a projection that holds no
/// marker, passes: there is no authority a restore could reopen. Otherwise
/// it is refused, without reading further, when:
///
/// - the database lineage differs from the one the incarnation is bound to;
/// - the configured restore epoch is not the stored one;
/// - a projection marker names a scope the store does not hold, an
///   operation of the scope's own incarnation that its journal does not
///   record, or an incarnation its journal does not record. A scope a
///   restore transition closed accepts a marker of any other incarnation:
///   reconstruction replaces it.
///
/// A malformed marker is not evidence of a restore and is left to
/// reconciliation, which reports it. Each marker is compared with canonical
/// state read after the marker, so an operation another replica recorded
/// and projected meanwhile is found in the journal. A replica whose
/// permission backend holds no projection compares only the epoch and the
/// lineage.
pub async fn verify_restore_fence(
    store: &HostGrantStore,
    permissions: &ActorRef<PermissionActor>,
    configured: RestoreEpoch,
) -> Result<RestoreFenceReport, HostGrantError> {
    let Some(authority) = store.fence_authority().await? else {
        return Ok(RestoreFenceReport::default());
    };
    let listed = permissions
        .ask(ListHostGrantProjectionMarkers)
        .await
        .map_err(projection_store_error)?
        .unwrap_or_default();
    // A database that holds no scope, beside a projection that holds no
    // marker, has no authority a restore could reopen.
    if !authority.holds_scope && listed.is_empty() {
        return Ok(RestoreFenceReport::default());
    }
    if let Some(bound) = &authority.bound_lineage {
        if authority.live_lineage.as_ref() != Some(bound) {
            return Err(RestoreFenceRefusal::LineageChanged.into());
        }
    }
    if authority.restore_epoch != configured {
        return Err(RestoreFenceRefusal::EpochMismatch {
            configured,
            stored: authority.restore_epoch,
        }
        .into());
    }
    let mut report = RestoreFenceReport {
        markers: listed.len(),
        awaiting_reconstruction: 0,
    };
    for entry in listed {
        let (Some(lifecycle), MarkerObservation::Present(marker)) = (entry.lifecycle, entry.marker)
        else {
            continue;
        };
        match marker_standing(store, lifecycle, &marker).await? {
            MarkerStanding::Consistent => {}
            MarkerStanding::AwaitingReconstruction => report.awaiting_reconstruction += 1,
            MarkerStanding::Refused(refusal) => return Err(refusal.into()),
        }
    }
    Ok(report)
}

/// How one projection marker relates to the canonical store.
enum MarkerStanding {
    Consistent,
    AwaitingReconstruction,
    Refused(RestoreFenceRefusal),
}

async fn marker_standing(
    store: &HostGrantStore,
    lifecycle: ScopeLifecycleId,
    marker: &ProjectionMarker,
) -> Result<MarkerStanding, HostGrantError> {
    let Some(scope) = store.scope(lifecycle).await? else {
        return Ok(MarkerStanding::Refused(
            RestoreFenceRefusal::OrphanedProjection,
        ));
    };
    let restored = match scope.closed_reason {
        // A retired or exhausted scope admits nothing, and its projection
        // is never written again.
        Some(ClosedReason::Retired | ClosedReason::Exhausted) => {
            return Ok(MarkerStanding::Consistent)
        }
        Some(ClosedReason::Restored) => true,
        Some(ClosedReason::Enrolled) | None => false,
    };
    if marker.incarnation == scope.authority_incarnation {
        let recorded = store.recorded_marker(lifecycle, marker.generation).await?;
        return Ok(if recorded.as_ref() == Some(marker) {
            MarkerStanding::Consistent
        } else {
            MarkerStanding::Refused(RestoreFenceRefusal::ProjectionAhead)
        });
    }
    if restored {
        return Ok(MarkerStanding::AwaitingReconstruction);
    }
    Ok(
        if store
            .journal_holds_incarnation(lifecycle, marker.incarnation)
            .await?
        {
            // The projection store is older than the canonical one: it was
            // restored or lost writes. Admission does not read it, and
            // reconciliation reports the marker as a contradiction.
            MarkerStanding::Consistent
        } else {
            MarkerStanding::Refused(RestoreFenceRefusal::UnknownIncarnation)
        },
    )
}

/// What a reconstruction records for one subject in one managed room before
/// the projection is rebuilt.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReconstructionTarget {
    pub channel_id: String,
    /// The subject as the projection or the canonical row spells it.
    pub subject: String,
    pub affiliation: RoomAffiliation,
}

/// The changes a reconstruction records before it replaces the projection
/// of an earlier incarnation with the canonical rows. Nothing is granted
/// from the projection.
///
/// - A subject the projection holds in a managed room that no canonical row
///   names is recorded at `outcast` when the projection bans it and at
///   `none` otherwise, so that the rebuilt projection states it and keeps
///   none of what it held.
/// - When `lost` is set, the canonical journal does not record the marker
///   the projection holds: the projection reflects operations the restored
///   database lost. Every canonical grant is then lowered to what the
///   projection holds for its subject, and to `none` where it holds
///   nothing, so a revoke or a ban the restore lost stays in force. A grant
///   the restore lost is not recreated.
/// - Otherwise the canonical rows are at least as new as the projection and
///   are kept as they are, including increases recorded and not yet
///   activated.
///
/// The targets are sorted, so equal inputs give equal operations.
pub fn reconstruction_targets(
    intent: &ProjectionIntent,
    observed: &BTreeSet<ProjectedRelationship>,
    lost: bool,
) -> Vec<ReconstructionTarget> {
    let mut projected: BTreeMap<(&str, &str), RoomAffiliation> = BTreeMap::new();
    for relationship in observed {
        let Some(affiliation) = RoomAffiliation::from_storage(&relationship.relation) else {
            continue;
        };
        projected
            .entry((
                relationship.channel_id.as_str(),
                relationship.subject.as_str(),
            ))
            .and_modify(|held| *held = (*held).min(affiliation))
            .or_insert(affiliation);
    }
    let canonical: BTreeMap<(&str, &str), RoomAffiliation> = intent
        .grants
        .iter()
        .map(|grant| {
            (
                (grant.channel_id.as_str(), grant.subject.as_str()),
                grant.affiliation,
            )
        })
        .collect();
    let target = |(channel_id, subject): (&str, &str), affiliation| ReconstructionTarget {
        channel_id: channel_id.to_owned(),
        subject: subject.to_owned(),
        affiliation,
    };
    let mut targets = Vec::new();
    for (key, held) in &projected {
        match canonical.get(key) {
            None => targets.push(target(*key, (*held).min(RoomAffiliation::None))),
            Some(recorded) if lost && held < recorded => targets.push(target(*key, *held)),
            Some(_) => {}
        }
    }
    if lost {
        for (key, recorded) in &canonical {
            if !projected.contains_key(key) && RoomAffiliation::None < *recorded {
                targets.push(target(*key, RoomAffiliation::None));
            }
        }
    }
    targets.sort();
    targets
}

/// One change of a reconstruction, issuable as recorded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReconstructionChange {
    pub(crate) channel_id: String,
    pub(crate) subject: GrantSubject,
    pub(crate) affiliation: RoomAffiliation,
    pub(crate) basis: IssuanceBasis,
}

/// Record the changes of a reconstruction for a scope a restore transition
/// closed. Every change lowers a grant or records a subject at `none` or
/// `outcast`; none raises one.
#[derive(Clone, Debug)]
pub(crate) struct ReconstructScope {
    pub(crate) operation: GrantOperationId,
    pub(crate) lifecycle: ScopeLifecycleId,
    pub(crate) expected_generation: GrantGeneration,
    /// The marker of the projection whose relationships the changes were
    /// read from.
    pub(crate) replaced: ProjectionMarker,
    pub(crate) changes: Vec<ReconstructionChange>,
}

impl ReconstructScope {
    pub(crate) fn fingerprint(&self) -> String {
        let mut fingerprint = Fingerprint::new("waddle:host-grant:reconstruct:v1");
        fingerprint.text(&self.lifecycle.to_storage());
        fingerprint.number(self.expected_generation.value());
        fingerprint.text(&self.replaced.stamp());
        let mut changes: Vec<(&str, &str, &str)> = self
            .changes
            .iter()
            .map(|change| {
                (
                    change.channel_id.as_str(),
                    change.subject.as_str(),
                    change.affiliation.as_storage(),
                )
            })
            .collect();
        changes.sort_unstable();
        fingerprint.number(changes.len() as i64);
        for (channel_id, subject, affiliation) in changes {
            fingerprint.text(channel_id);
            fingerprint.text(subject);
            fingerprint.text(affiliation);
        }
        fingerprint.finish()
    }
}

/// The fingerprint of the operation that closes one scope in a restore
/// transition.
pub(crate) fn restore_fingerprint(
    lifecycle: ScopeLifecycleId,
    epoch: RestoreEpoch,
    incarnation: AuthorityIncarnation,
) -> String {
    let mut fingerprint = Fingerprint::new("waddle:host-grant:restore:v1");
    fingerprint.text(&lifecycle.to_storage());
    fingerprint.number(epoch.to_storage());
    fingerprint.text(&incarnation.to_storage());
    fingerprint.finish()
}
