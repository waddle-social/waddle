//! The restore incarnation of the host-grant authority (ADR-0020 D5
//! condition B7, gate G13).
//!
//! Restoring PostgreSQL from a backup, recovering it to an earlier point
//! in time, or failing over to a replica that missed commits brings back
//! every scope's older generation together with its grants, its `active`
//! flag and the authority incarnation that state was bound to. Nothing
//! inside the restored database can tell it from the live one, so the
//! incarnation is bound to what a restore of the database does not bring
//! back:
//!
//! - the declared restore epoch ([`RestoreEpoch`]): an operator-supplied
//!   counter in the deployment configuration of every replica and of the
//!   owner phase, raised once for each restore. The database records the
//!   epoch its incarnation was established under, and a replica serves only
//!   a database that records its own configured epoch;
//! - the database lineage (`_lineage`), which a logical restore into another
//!   database rotates through adoption. A physical copy keeps it;
//! - what SpiceDB holds (`witness`): the projection markers, the journal
//!   head of each scope, the room bindings of each projected scope and the
//!   PostgreSQL position of the authority.
//!
//! Declaring a restore is an owner-phase act
//! ([`HostGrantStore::prepare_restore_incarnation`](super::HostGrantStore::prepare_restore_incarnation)).
//! When the configured epoch exceeds the stored one it mints a new
//! incarnation, records the epoch and the live lineage, and closes every
//! scope that is not retired or exhausted as `restored`, bound to the new
//! incarnation at its next generation and with no effective standing above
//! `none`. From that commit:
//!
//! - no admission, mutation or activation can use the earlier incarnation or
//!   any generation before the transition, so old queued and in-flight work
//!   is refused or superseded;
//! - a replica attached to the earlier incarnation admits nothing: every
//!   acceptance compares the authority it reads with the one its attachment
//!   verified ([`AuthorityBinding`]), and a replica that finds it changed
//!   verifies the fence again before it admits again;
//! - a restored scope admits nothing, and honours no initiator, until
//!   reconciliation has rebuilt its projection from the canonical rows,
//!   read it back and activated it (it is then `reconstructed`) and an
//!   operator has reopened it. Reconstruction never grants from the
//!   projection: where the projection or the journal witness shows that the
//!   restored database lost operations, it lowers the canonical grants to
//!   what the projection holds ([`reconstruction_targets`]).
//!
//! An undeclared restore is found by the fence every replica passes before
//! it admits a managed room ([`verify_restore_fence`]). A restore that
//! affects the whole authority (another epoch, lineage, or PostgreSQL
//! position) refuses the attachment; evidence about one scope closes that
//! scope as `fenced`, and a lifecycle only SpiceDB knows refuses its rooms.
//!
//! The decisions and their limits are recorded in
//! `server/docs/planning/host-grant-authority-2047.md`, "Restore incarnation
//! decisions", and the operator procedure in
//! `docs/operations/authz-host-grant-restore.md` at the repository root.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::future::Future;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use kameo::actor::ActorRef;
use thiserror::Error;

use super::authority::projection_store_error;
use super::error::HostGrantError;
use super::issuance::IssuanceBasis;
use super::marker::ProjectionMarker;
use super::model::{
    AuthorityIncarnation, ClosedReason, Fingerprint, GrantGeneration, GrantOperationId,
    GrantSubject, RoomAffiliation, ScopeLifecycleId, ScopeState,
};
use super::projection::{
    MarkerObservation, ProjectedRelationship, ProjectionIntent, ProjectionWrite,
};
use super::store::{FenceSnapshot, HostGrantStore};
use super::witness::{
    apply_witness_changes, clear_witnesses, effective_latch, epoch_witness_resource, latch_changes,
    latch_witness_resource, parse_epoch_stamp, position_standing, position_witness_resource,
    regression, witness_write_failed, ListedWitness, PositionLatch, PositionStanding,
    PositionWitness, WitnessChange, WitnessResource,
};
use crate::permissions::{ListHostGrantWitnesses, PermissionActor};

/// Deadline of the read of every projection marker and witness.
pub const HOST_GRANT_RESTORE_SCAN_TIMEOUT: Duration = Duration::from_secs(60);

/// Budget of the PostgreSQL reads and fence closures of one fence pass,
/// besides [`HOST_GRANT_RESTORE_FENCE_SCOPE_ALLOWANCE`] for each scope the
/// pass compares. The read of SpiceDB has its own deadline
/// ([`HOST_GRANT_RESTORE_SCAN_TIMEOUT`]) and does not draw on it.
pub const HOST_GRANT_RESTORE_FENCE_DEADLINE: Duration = Duration::from_secs(60);

/// What a fence pass's PostgreSQL budget grows by for each scope SpiceDB
/// names and the store holds, so that a deployment with many scopes is
/// compared within a budget that grows with it rather than refused as busy.
pub const HOST_GRANT_RESTORE_FENCE_SCOPE_ALLOWANCE: Duration = Duration::from_millis(100);

/// Passes a fence makes before it gives up on an authority that changes
/// while it is verified.
const FENCE_ATTEMPTS: usize = 3;

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
    /// microseconds since the Unix epoch. A restore restarts the server,
    /// and so does a failover; either changes it.
    pub server_started_at_us: i64,
    /// The OID of the database the binding was read from. A logical restore
    /// into a new database changes it; a physical copy keeps it.
    pub database_oid: i64,
    /// The name of that database.
    pub database_name: String,
    /// The OID of the authority relation. A logical restore that recreates
    /// the host-grant relations (`pg_restore --clean`, a dump loaded into a
    /// recreated schema) changes it without restarting the server.
    pub authority_relation_oid: i64,
}

/// Why the authority as a whole is not served: a restore that was not
/// declared, or a configuration that does not match the declared one. A
/// refusal names no scope, room or account.
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
         database was adopted after a restore or a clone; declare the restore by raising the \
         restore epoch"
    )]
    LineageChanged,
    #[error(
        "the PostgreSQL position is behind the one SpiceDB witnessed: the database was rolled \
         back or recovered to an earlier point and the restore was not declared"
    )]
    PositionRegressed,
    #[error(
        "the PostgreSQL system or timeline changed since SpiceDB witnessed its position (a \
         promotion, a point-in-time recovery or another cluster); acknowledge it with \
         `waddle-server host-grant acknowledge-position` or declare a restore"
    )]
    PositionUnverified,
    #[error(
        "the PostgreSQL position cannot be read by this role: pg_current_wal_flush_lsn() and \
         pg_walfile_name() must be executable, and outside the attested pool \
         pg_control_system() as well"
    )]
    PositionUnreadable,
    #[error(
        "scopes admit and SpiceDB holds no projection marker, room witness or position witness \
         of this authority: the replica is connected to a SpiceDB that is empty or another \
         deployment's"
    )]
    ProjectionAbsent,
    #[error("the host-grant authority changed while its restore fence was verified")]
    AuthorityMoved,
}

impl RestoreFenceRefusal {
    /// A stable name of the refusal for logs.
    pub fn kind(self) -> &'static str {
        match self {
            Self::EpochMismatch { .. } => "epoch_mismatch",
            Self::LineageChanged => "lineage_changed",
            Self::PositionRegressed => "position_regressed",
            Self::PositionUnverified => "position_unverified",
            Self::PositionUnreadable => "position_unreadable",
            Self::ProjectionAbsent => "projection_absent",
            Self::AuthorityMoved => "authority_moved",
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
    /// A transition an earlier run recorded and did not finish was finished:
    /// the scopes still bound to an earlier incarnation were closed.
    Resumed { restored_scopes: usize },
}

/// What the fence found that one scope's canonical state contradicts. The
/// scope is closed as `fenced` until a restore is declared.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FenceEvidence {
    /// The projection marker names an operation of the scope's incarnation
    /// that the journal does not record.
    ProjectionAhead(ProjectionMarker),
    /// The projection marker names an incarnation the scope's journal does
    /// not record.
    UnknownIncarnation(ProjectionMarker),
    /// The journal witness names an operation the journal does not record.
    JournalAhead(ProjectionMarker),
    /// The scope admitted, so its projection was verified, and SpiceDB
    /// holds no marker for it: the projection, and with it every witness
    /// of the scope, was lost.
    MarkerMissing,
}

impl FenceEvidence {
    /// Whether the evidence shows that the database lost operations of the
    /// scope, rather than that SpiceDB lost its projection.
    pub fn shows_lost_operations(&self) -> bool {
        match self {
            Self::ProjectionAhead(_) | Self::UnknownIncarnation(_) | Self::JournalAhead(_) => true,
            Self::MarkerMissing => false,
        }
    }

    /// A stable name of the evidence for logs.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::ProjectionAhead(_) => "projection_ahead",
            Self::UnknownIncarnation(_) => "unknown_incarnation",
            Self::JournalAhead(_) => "journal_ahead",
            Self::MarkerMissing => "marker_missing",
        }
    }
}

/// What a fence pass found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RestoreFenceReport {
    /// The authority the pass verified, read before the projection and
    /// again after it; `None` when the database holds no host-grant
    /// relations.
    pub binding: Option<AuthorityBinding>,
    /// The projection markers compared with the canonical store.
    pub markers: usize,
    /// Scopes a restore transition closed whose reconstruction is not yet
    /// activated.
    pub awaiting_reconstruction: usize,
    /// Scopes whose reconstruction was activated and which wait for an
    /// operator to reopen them.
    pub awaiting_reopen: usize,
    /// Scopes this pass closed as `fenced`.
    pub fenced: usize,
    /// Lifecycles SpiceDB holds and the canonical store does not.
    pub orphaned: usize,
    /// The room witness digests of those lifecycles: their rooms are refused
    /// rather than served as rooms no scope manages.
    pub refused_rooms: BTreeSet<String>,
    /// How the PostgreSQL position relates to its witness; `None` when the
    /// backend holds no projection or the pass did not reach it.
    pub position: Option<PositionStanding>,
    /// Whether the permission backend holds a projection whose markers and
    /// witnesses the pass read.
    pub projection_listed: bool,
    /// The journal witness of each scope, as the pass read it, for the
    /// witness writers that follow a pass that passed.
    pub witnessed_heads: BTreeMap<ScopeLifecycleId, String>,
    /// Whether SpiceDB holds a marker or room witness of a lifecycle the
    /// store holds, or a position witness, latch or witnessed epoch filed
    /// under this authority. Only then does the witness pass after it write
    /// the first position witness.
    pub holds_authority: bool,
}

/// What a replica keeps between passes of its restore fence.
///
/// - The rooms of lifecycles only SpiceDB holds (lost enrollments). Every
///   pass that reads SpiceDB and the store's scopes replaces the set, before
///   it decides anything about the whole authority, so the set stays known
///   while the authority is refused. `None` until a pass got that far.
/// - A position regression the replica saw. It counts like a stored latch
///   naming the same witness, so the replica keeps refusing it when the
///   latch could not be written to SpiceDB, and every later pass tries the
///   write again. Until a write succeeds the latch is this replica's alone:
///   it is lost when the replica restarts, and it stops counting once the
///   position witness it names is replaced, which a writer that does not
///   hold it does as soon as the server has written past the witnessed
///   position again (another replica's fence, a committed mutation, an
///   operator's act).
#[derive(Debug, Default)]
pub struct FenceMemory {
    refused_rooms: RwLock<Option<Arc<BTreeSet<String>>>>,
    latch: Mutex<Option<PositionLatch>>,
}

impl FenceMemory {
    /// The room witness digests a replica refuses, if a pass read them.
    pub fn refused_rooms(&self) -> Option<Arc<BTreeSet<String>>> {
        self.refused_rooms
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn publish_rooms(&self, rooms: BTreeSet<String>) {
        *self
            .refused_rooms
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::new(rooms));
    }

    /// The regression this replica latched, if any.
    pub fn latch(&self) -> Option<PositionLatch> {
        *self
            .latch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn set_latch(&self, latch: Option<PositionLatch>) {
        *self
            .latch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = latch;
    }
}

/// The scopes an operator's reopen considers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReopenSelection {
    /// Every reconstructed scope the restore lost no operations in.
    All,
    /// One scope. One the restore lost operations in reopens only when
    /// `lost_operations_reapplied` confirms that the operator has applied
    /// again what it lost.
    Scope {
        lifecycle: ScopeLifecycleId,
        lost_operations_reapplied: bool,
    },
}

/// Verify, before a replica admits a managed room, that the canonical store
/// is the authority the deployment declared and not an undeclared restore
/// of it.
///
/// One pass reads the authority binding, the lineage, the scopes that admit
/// and the scope counts in one PostgreSQL snapshot, then every marker and
/// witness SpiceDB holds, then every scope the store holds. From those it
/// first finds the lifecycles only SpiceDB holds and the rooms their room
/// witnesses name, which stay refused whatever the pass decides next. It
/// then refuses the whole authority when:
///
/// - the database lineage differs from the one the incarnation is bound to;
/// - the configured restore epoch is not the stored one;
/// - scopes admit and SpiceDB holds no marker, room witness or position
///   witness of this authority (a SpiceDB that is empty, or that holds only
///   another deployment's markers); nothing is closed for it;
/// - the PostgreSQL position regressed, a regression is latched, a latch
///   is held without its witness, or the system or timeline changed
///   without an acknowledgement ([`position_standing`]);
/// - the position witness, the latch or the witnessed epoch cannot be read
///   and no restore was declared since they were written (once one was,
///   the pass removes them).
///
/// About each scope it decides alone: a marker or journal witness the
/// journal contradicts, or a scope that admits without a marker, closes that
/// scope as `fenced` in PostgreSQL. A database that holds no scope, beside a
/// projection that names none, passes. A replica whose permission backend
/// holds no projection compares only the epoch and the lineage.
///
/// Canonical state is read after what SpiceDB holds, so an operation another
/// replica recorded and projected meanwhile is found in the journal. The
/// binding is read again at the end of the pass, and the pass is repeated
/// when it changed: the report's binding is the one the whole pass saw. The
/// PostgreSQL reads of a pass share one budget
/// ([`HOST_GRANT_RESTORE_FENCE_DEADLINE`]).
pub async fn verify_restore_fence(
    store: &HostGrantStore,
    permissions: &ActorRef<PermissionActor>,
    configured: RestoreEpoch,
) -> Result<RestoreFenceReport, HostGrantError> {
    verify_restore_fence_with(store, permissions, configured, &FenceMemory::default()).await
}

/// [`verify_restore_fence`] for a replica that keeps `memory` between
/// passes: the pass publishes the refused rooms into it and counts the
/// regression it latched.
pub async fn verify_restore_fence_with(
    store: &HostGrantStore,
    permissions: &ActorRef<PermissionActor>,
    configured: RestoreEpoch,
    memory: &FenceMemory,
) -> Result<RestoreFenceReport, HostGrantError> {
    for _ in 0..FENCE_ATTEMPTS {
        let mut budget = PostgresBudget::new(HOST_GRANT_RESTORE_FENCE_DEADLINE);
        let Some(snapshot) = budget.bounded(store.fence_snapshot()).await? else {
            memory.publish_rooms(BTreeSet::new());
            return Ok(RestoreFenceReport::default());
        };
        let report = fence_pass(
            store,
            permissions,
            configured,
            &snapshot,
            &mut budget,
            memory,
        )
        .await?;
        let reread = budget.bounded(store.authority_binding()).await?;
        if reread == snapshot.binding {
            return Ok(report);
        }
    }
    Err(RestoreFenceRefusal::AuthorityMoved.into())
}

/// Read every marker and witness SpiceDB holds and the store's scopes, and
/// publish the rooms of lifecycles only SpiceDB holds into `memory`. For a
/// replica that cannot run the fence (its runtime role does not match the
/// profile) and still has to refuse those rooms.
pub(crate) async fn refresh_refused_rooms(
    store: &HostGrantStore,
    permissions: &ActorRef<PermissionActor>,
    memory: &FenceMemory,
) -> Result<(), HostGrantError> {
    let listed = permissions
        .ask(ListHostGrantWitnesses)
        .await
        .map_err(projection_store_error)?;
    let Some(listed) = listed else {
        memory.publish_rooms(BTreeSet::new());
        return Ok(());
    };
    let witnesses = Witnesses::from_listing(&listed);
    let mut budget = PostgresBudget::new(HOST_GRANT_RESTORE_FENCE_DEADLINE);
    let scopes = budget.bounded(store.scopes()).await?;
    memory.publish_rooms(witnesses.orphaned(&scopes).1);
    Ok(())
}

/// The PostgreSQL time one fence pass may spend. Each step draws what it
/// took; a step that would overrun what is left is cut off as
/// [`HostGrantError::Busy`].
struct PostgresBudget {
    left: Duration,
}

impl PostgresBudget {
    fn new(budget: Duration) -> Self {
        Self { left: budget }
    }

    fn grant(&mut self, extra: Duration) {
        self.left = self.left.saturating_add(extra);
    }

    async fn bounded<T>(
        &mut self,
        step: impl Future<Output = Result<T, HostGrantError>>,
    ) -> Result<T, HostGrantError> {
        let started = tokio::time::Instant::now();
        let outcome = tokio::time::timeout(self.left, step)
            .await
            .unwrap_or(Err(HostGrantError::Busy));
        self.left = self.left.saturating_sub(started.elapsed());
        outcome
    }
}

async fn fence_pass(
    store: &HostGrantStore,
    permissions: &ActorRef<PermissionActor>,
    configured: RestoreEpoch,
    snapshot: &FenceSnapshot,
    budget: &mut PostgresBudget,
    memory: &FenceMemory,
) -> Result<RestoreFenceReport, HostGrantError> {
    let listed = permissions
        .ask(ListHostGrantWitnesses)
        .await
        .map_err(projection_store_error)?;
    let mut report = RestoreFenceReport {
        binding: Some(snapshot.binding.clone()),
        awaiting_reconstruction: snapshot.restored,
        awaiting_reopen: snapshot.reconstructed,
        ..RestoreFenceReport::default()
    };
    let Some(listed) = listed else {
        // No projection, so no witness either: no room can belong to a lost
        // enrollment.
        memory.publish_rooms(BTreeSet::new());
        if snapshot.holds_scope {
            check_lineage_and_epoch(snapshot, configured)?;
        }
        return Ok(report);
    };
    let witnesses = Witnesses::from_listing(&listed);
    report.projection_listed = true;
    report.markers = witnesses.markers.len();
    report.witnessed_heads = witnesses
        .heads
        .iter()
        .filter_map(|(lifecycle, head)| match head {
            MarkerObservation::Present(head) => Some((*lifecycle, head.stamp())),
            MarkerObservation::Absent | MarkerObservation::Malformed => None,
        })
        .collect();

    // The store's scopes, read after SpiceDB. The rooms of lifecycles only
    // SpiceDB holds are refused before anything is decided about the whole
    // authority, so they stay refused when it is refused.
    let scopes = budget.bounded(store.scopes()).await?;
    let (orphaned, refused_rooms) = witnesses.orphaned(&scopes);
    memory.publish_rooms(refused_rooms.clone());
    report.orphaned = orphaned.len();
    report.refused_rooms = refused_rooms;

    // A database that holds no scope, beside a projection that names none,
    // has no authority a restore could reopen.
    if !snapshot.holds_scope && !witnesses.names_scope() {
        return Ok(report);
    }
    check_lineage_and_epoch(snapshot, configured)?;

    let authority_id = &snapshot.authority_id;
    report.holds_authority = witnesses.holds_authority(&scopes, authority_id);
    // Scopes admit, so their markers were written; a SpiceDB that holds no
    // marker, room witness or position witness of this authority (none at
    // all, or only another deployment's) is not this deployment's
    // projection. Nothing is closed
    // for it and no position witness is written into it: the refusal
    // clears when the replica reaches the SpiceDB it belongs beside.
    if !snapshot.admitting.is_empty() && !report.holds_authority {
        return Err(RestoreFenceRefusal::ProjectionAbsent.into());
    }
    let (witnessed, stored_latch) = match witnesses.readable_position(authority_id) {
        Some(readable) => readable,
        None => {
            clear_unreadable_position(
                permissions,
                &witnesses,
                authority_id,
                snapshot.binding.restore_epoch,
            )
            .await?;
            (None, None)
        }
    };
    let current = PositionWitness {
        epoch: snapshot.binding.restore_epoch,
        position: budget.bounded(store.postgres_position()).await?,
    };
    let latch = effective_latch(
        witnessed.as_ref(),
        stored_latch.as_ref(),
        memory.latch().as_ref(),
    );
    let standing = position_standing(
        witnessed.as_ref(),
        &current,
        snapshot.position_acknowledgement.as_ref(),
        latch.as_ref(),
    );
    match standing {
        PositionStanding::Regressed => {
            latch_regression(
                permissions,
                &snapshot.authority_id,
                witnessed,
                &current,
                latch,
                stored_latch,
                memory,
            )
            .await;
            return Err(RestoreFenceRefusal::PositionRegressed.into());
        }
        PositionStanding::Unverified => return Err(RestoreFenceRefusal::PositionUnverified.into()),
        PositionStanding::Rebased | PositionStanding::Acknowledged => {
            memory.set_latch(None);
            report.position = Some(standing);
        }
        PositionStanding::Unrecorded | PositionStanding::Current | PositionStanding::Advanced => {
            report.position = Some(standing);
        }
    }

    for lifecycle in witnesses.lifecycles() {
        let Some(scope) = scopes.get(&lifecycle) else {
            continue;
        };
        budget.grant(HOST_GRANT_RESTORE_FENCE_SCOPE_ALLOWANCE);
        let evidence = budget
            .bounded(scope_evidence(
                store,
                scope,
                witnesses.markers.get(&lifecycle),
                witnesses.heads.get(&lifecycle),
            ))
            .await?;
        if let Some(evidence) = evidence {
            if budget
                .bounded(store.fence_scope(lifecycle, &evidence))
                .await?
            {
                tracing::error!(
                    evidence = evidence.kind(),
                    "A host-grant scope was closed: SpiceDB records what its canonical store \
                     does not (docs/operations/authz-host-grant-restore.md)"
                );
                report.fenced += 1;
            }
        }
    }
    // A scope that admitted when the pass began had its marker written
    // before then, and a marker is only ever replaced, never removed.
    for lifecycle in &snapshot.admitting {
        if witnesses.markers.contains_key(lifecycle) {
            continue;
        }
        let evidence = FenceEvidence::MarkerMissing;
        if budget
            .bounded(store.fence_scope(*lifecycle, &evidence))
            .await?
        {
            tracing::error!(
                evidence = evidence.kind(),
                "A host-grant scope was closed: SpiceDB holds no projection of it \
                 (docs/operations/authz-host-grant-restore.md)"
            );
            report.fenced += 1;
        }
    }
    Ok(report)
}

/// Refuse a database adopted without a declared restore, and a configured
/// epoch the database does not record.
fn check_lineage_and_epoch(
    snapshot: &FenceSnapshot,
    configured: RestoreEpoch,
) -> Result<(), HostGrantError> {
    if let Some(bound) = &snapshot.bound_lineage {
        if snapshot.live_lineage.as_ref() != Some(bound) {
            return Err(RestoreFenceRefusal::LineageChanged.into());
        }
    }
    if snapshot.binding.restore_epoch != configured {
        return Err(RestoreFenceRefusal::EpochMismatch {
            configured,
            stored: snapshot.binding.restore_epoch,
        }
        .into());
    }
    Ok(())
}

/// Keep a regression the fence saw: in `memory` at once, and in SpiceDB as
/// the authority's latch, beside the exact position witness it names. The
/// write is best effort; a failure is counted and logged by kind.
async fn latch_regression(
    permissions: &ActorRef<PermissionActor>,
    authority_id: &str,
    witnessed: Option<PositionWitness>,
    current: &PositionWitness,
    latch: Option<PositionLatch>,
    stored: Option<PositionLatch>,
    memory: &FenceMemory,
) {
    let Some(seen) = witnessed else {
        return;
    };
    let Some(latch) = latch.or_else(|| {
        regression(&seen, current).map(|kind| PositionLatch {
            kind,
            witness: seen,
        })
    }) else {
        return;
    };
    if memory.latch() != Some(latch) {
        tracing::error!(
            latch = ?latch.kind,
            "A regression of the PostgreSQL position was latched: the host-grant authority stays \
             refused until a restore is declared (docs/operations/authz-host-grant-restore.md)"
        );
        memory.set_latch(Some(latch));
    }
    if stored == Some(latch) {
        return;
    }
    let stored = stored.map(|stored| stored.stamp());
    let changes = latch_changes(
        &position_witness_resource(authority_id),
        &latch_witness_resource(authority_id),
        &latch,
        stored.as_deref(),
    );
    // A latch that is not written is kept in this replica's memory only;
    // every way it fails is counted like any failed witness write.
    match apply_witness_changes(permissions, changes).await {
        Ok(Some(ProjectionWrite::Applied)) => {}
        Ok(Some(ProjectionWrite::PreconditionFailed)) => {
            witness_write_failed("precondition_failed", "latch");
        }
        Ok(None) => witness_write_failed("no_projection", "latch"),
        Err(error) => witness_write_failed(error.kind(), "latch"),
    }
}

/// Remove the authority's position witness, latch and witnessed epoch when
/// one of them cannot be read. An unreadable witness carries no epoch, so it
/// is removed only when what can be read of the three shows a restore was
/// declared since they were written: an epoch below `declared`. Otherwise,
/// or when the removal does not complete, the authority is refused as
/// unverified, and nothing is removed without a declared restore.
///
/// Each resource is removed in a request of its own conditioned on the
/// resource that shows the earlier epoch, and that resource is removed
/// last, so a pass that stops in between leaves the evidence for the next
/// pass to finish. Removed, the three are as a declared restore leaves a
/// readable witness: the next pass that passes writes the first witness of
/// the declared epoch.
async fn clear_unreadable_position(
    permissions: &ActorRef<PermissionActor>,
    witnesses: &Witnesses,
    authority_id: &str,
    declared: RestoreEpoch,
) -> Result<(), HostGrantError> {
    let resources = [
        (
            epoch_witness_resource(authority_id),
            witnesses
                .epochs
                .get(authority_id)
                .copied()
                .flatten()
                .map(|epoch| (epoch, epoch.to_string())),
        ),
        (
            position_witness_resource(authority_id),
            witnesses
                .positions
                .get(authority_id)
                .copied()
                .flatten()
                .map(|witness| (witness.epoch, witness.stamp())),
        ),
        (
            latch_witness_resource(authority_id),
            witnesses
                .latches
                .get(authority_id)
                .copied()
                .flatten()
                .map(|latch| (latch.witness.epoch, latch.stamp())),
        ),
    ];
    let Some((proving, stamp)) = resources.iter().find_map(|(resource, read)| {
        read.as_ref()
            .filter(|(epoch, _)| *epoch < declared)
            .map(|(_, stamp)| (resource.clone(), stamp.clone()))
    }) else {
        return Err(RestoreFenceRefusal::PositionUnverified.into());
    };
    let mut order: Vec<String> = resources
        .iter()
        .map(|(resource, _)| resource.clone())
        .filter(|resource| *resource != proving)
        .collect();
    order.push(proving.clone());
    let condition = WitnessChange {
        resource: proving,
        previous: Some(stamp.clone()),
        next: Some(stamp),
    };
    match clear_witnesses(permissions, order, condition).await {
        Ok(Some(ProjectionWrite::Applied)) => {
            tracing::warn!(
                "A host-grant position witness that could not be read was removed after a \
                 declared restore (docs/operations/authz-host-grant-restore.md)"
            );
            Ok(())
        }
        Ok(Some(ProjectionWrite::PreconditionFailed)) => {
            witness_write_failed("precondition_failed", "position");
            Err(RestoreFenceRefusal::PositionUnverified.into())
        }
        Ok(None) => Err(RestoreFenceRefusal::PositionUnverified.into()),
        Err(error) => {
            witness_write_failed(error.kind(), "position");
            Err(RestoreFenceRefusal::PositionUnverified.into())
        }
    }
}

/// What SpiceDB holds that one known scope's canonical state contradicts,
/// if anything. Malformed markers and witnesses are not evidence: the
/// projection pass reports a malformed marker, and a malformed witness
/// writer stops.
async fn scope_evidence(
    store: &HostGrantStore,
    scope: &ScopeState,
    marker: Option<&MarkerObservation>,
    head: Option<&MarkerObservation>,
) -> Result<Option<FenceEvidence>, HostGrantError> {
    let restore_handling = match scope.closed_reason {
        // A retired or exhausted scope admits nothing and is never written
        // again; a fenced one is closed already.
        Some(ClosedReason::Retired | ClosedReason::Exhausted | ClosedReason::Fenced) => {
            return Ok(None)
        }
        Some(ClosedReason::Restored | ClosedReason::Reconstructed) => true,
        Some(ClosedReason::Enrolled) | None => false,
    };
    let lifecycle = scope.lifecycle;
    if let Some(MarkerObservation::Present(marker)) = marker {
        if marker.incarnation == scope.authority_incarnation {
            if store
                .recorded_marker(lifecycle, marker.generation)
                .await?
                .as_ref()
                != Some(marker)
            {
                return Ok(Some(FenceEvidence::ProjectionAhead(marker.clone())));
            }
        } else if !restore_handling
            && !store
                .journal_holds_incarnation(lifecycle, marker.incarnation)
                .await?
        {
            return Ok(Some(FenceEvidence::UnknownIncarnation(marker.clone())));
        }
        // Otherwise a marker of an earlier incarnation of this journal: SpiceDB
        // is older than PostgreSQL, or a restored scope's reconstruction
        // replaces it.
    }
    if let Some(MarkerObservation::Present(head)) = head {
        let recorded = store
            .recorded_marker(lifecycle, head.generation)
            .await?
            .as_ref()
            == Some(head);
        // A head of another incarnation is explained while a restore is
        // handled, or once a declared restore that superseded an incarnation
        // this journal held was reopened under the current one.
        let explained = recorded
            || (head.incarnation != scope.authority_incarnation
                && (restore_handling
                    || (store
                        .journal_holds_reopen(lifecycle, scope.authority_incarnation)
                        .await?
                        && store
                            .journal_holds_incarnation(lifecycle, head.incarnation)
                            .await?)));
        if !explained {
            return Ok(Some(FenceEvidence::JournalAhead(head.clone())));
        }
    }
    Ok(None)
}

/// Every marker and witness SpiceDB holds, by kind.
#[derive(Debug, Default)]
pub(crate) struct Witnesses {
    pub(crate) markers: BTreeMap<ScopeLifecycleId, MarkerObservation>,
    pub(crate) heads: BTreeMap<ScopeLifecycleId, MarkerObservation>,
    /// The lifecycles each room witness names.
    pub(crate) rooms: BTreeMap<String, BTreeSet<ScopeLifecycleId>>,
    /// Each authority's position witness; `None` when it is malformed.
    pub(crate) positions: BTreeMap<String, Option<PositionWitness>>,
    /// Each authority's regression latch; `None` when it is malformed.
    pub(crate) latches: BTreeMap<String, Option<PositionLatch>>,
    /// The epoch each authority's position witness was last written under;
    /// `None` when it is malformed.
    pub(crate) epochs: BTreeMap<String, Option<RestoreEpoch>>,
}

impl Witnesses {
    pub(crate) fn from_listing(listing: &[ListedWitness]) -> Self {
        let mut witnesses = Self::default();
        for listed in listing {
            match WitnessResource::classify(&listed.resource) {
                WitnessResource::Marker(lifecycle) => {
                    witnesses
                        .markers
                        .insert(lifecycle, observe_marker(lifecycle, listed));
                }
                WitnessResource::Head(lifecycle) => {
                    witnesses
                        .heads
                        .insert(lifecycle, observe_marker(lifecycle, listed));
                }
                WitnessResource::Room(digest) => {
                    if listed.well_formed {
                        let named = listed
                            .stamps
                            .iter()
                            .filter_map(|stamp| ScopeLifecycleId::from_storage(stamp));
                        witnesses.rooms.entry(digest).or_default().extend(named);
                    }
                }
                WitnessResource::Position(authority) => {
                    let witness = match listed.single_stamp() {
                        Ok(Some(stamp)) => PositionWitness::parse(stamp),
                        Ok(None) | Err(_) => None,
                    };
                    witnesses.positions.insert(authority, witness);
                }
                WitnessResource::Latch(authority) => {
                    let latch = match listed.single_stamp() {
                        Ok(Some(stamp)) => PositionLatch::parse(stamp),
                        Ok(None) | Err(_) => None,
                    };
                    witnesses.latches.insert(authority, latch);
                }
                WitnessResource::Epoch(authority) => {
                    let epoch = match listed.single_stamp() {
                        Ok(Some(stamp)) => parse_epoch_stamp(stamp),
                        Ok(None) | Err(_) => None,
                    };
                    witnesses.epochs.insert(authority, epoch);
                }
                WitnessResource::Unknown => {}
            }
        }
        witnesses
    }

    /// Whether any marker or witness names a scope.
    fn names_scope(&self) -> bool {
        !self.markers.is_empty() || !self.heads.is_empty() || !self.rooms.is_empty()
    }

    /// Whether SpiceDB holds what only this authority's projection writes: a
    /// marker or room witness of a lifecycle `scopes` holds, or a position
    /// witness, latch or witnessed epoch filed under `authority_id`. A
    /// journal witness does not count: a committed mutation writes it into
    /// whatever SpiceDB its replica reaches, so it shows nothing about whose
    /// projection SpiceDB holds.
    fn holds_authority(
        &self,
        scopes: &BTreeMap<ScopeLifecycleId, ScopeState>,
        authority_id: &str,
    ) -> bool {
        self.markers
            .keys()
            .chain(self.rooms.values().flatten())
            .any(|lifecycle| scopes.contains_key(lifecycle))
            || self.positions.contains_key(authority_id)
            || self.latches.contains_key(authority_id)
            || self.epochs.contains_key(authority_id)
    }

    /// The authority's position witness and latch, when each is absent or
    /// readable and so is the witnessed epoch; `None` when one of them
    /// cannot be read.
    fn readable_position(
        &self,
        authority_id: &str,
    ) -> Option<(Option<PositionWitness>, Option<PositionLatch>)> {
        let witness = match self.positions.get(authority_id) {
            None => None,
            Some(witness) => Some((*witness)?),
        };
        let latch = match self.latches.get(authority_id) {
            None => None,
            Some(latch) => Some((*latch)?),
        };
        if matches!(self.epochs.get(authority_id), Some(None)) {
            return None;
        }
        Some((witness, latch))
    }

    /// Every lifecycle a marker, a journal witness or a room witness names.
    fn lifecycles(&self) -> BTreeSet<ScopeLifecycleId> {
        self.markers
            .keys()
            .chain(self.heads.keys())
            .copied()
            .chain(self.rooms.values().flatten().copied())
            .collect()
    }

    /// The lifecycles a marker or witness names that `scopes` does not
    /// hold, and the room witness digests that name any of them.
    fn orphaned(
        &self,
        scopes: &BTreeMap<ScopeLifecycleId, ScopeState>,
    ) -> (BTreeSet<ScopeLifecycleId>, BTreeSet<String>) {
        let orphaned: BTreeSet<ScopeLifecycleId> = self
            .lifecycles()
            .into_iter()
            .filter(|lifecycle| !scopes.contains_key(lifecycle))
            .collect();
        let rooms = self
            .rooms
            .iter()
            .filter(|(_, named)| named.iter().any(|lifecycle| orphaned.contains(lifecycle)))
            .map(|(digest, _)| digest.clone())
            .collect();
        (orphaned, rooms)
    }
}

fn observe_marker(lifecycle: ScopeLifecycleId, listed: &ListedWitness) -> MarkerObservation {
    match listed.single_stamp() {
        Ok(Some(stamp)) => ProjectionMarker::parse(lifecycle, stamp)
            .map_or(MarkerObservation::Malformed, MarkerObservation::Present),
        Ok(None) => MarkerObservation::Absent,
        Err(_) => MarkerObservation::Malformed,
    }
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

/// The fingerprint of the operation that closes one scope on fence
/// evidence. Two passes that close the same scope on the same evidence at
/// the same generation record one operation's input.
pub(crate) fn fence_fingerprint(
    lifecycle: ScopeLifecycleId,
    generation: GrantGeneration,
    evidence: &FenceEvidence,
) -> String {
    let mut fingerprint = Fingerprint::new("waddle:host-grant:fence:v1");
    fingerprint.text(&lifecycle.to_storage());
    fingerprint.number(generation.value());
    fingerprint.text(evidence.kind());
    match evidence {
        FenceEvidence::ProjectionAhead(marker)
        | FenceEvidence::UnknownIncarnation(marker)
        | FenceEvidence::JournalAhead(marker) => fingerprint.text(&marker.stamp()),
        FenceEvidence::MarkerMissing => {}
    }
    fingerprint.finish()
}

/// The fingerprint of the operation that reopens one scope.
pub(crate) fn reopen_fingerprint(
    lifecycle: ScopeLifecycleId,
    generation: GrantGeneration,
) -> String {
    let mut fingerprint = Fingerprint::new("waddle:host-grant:reopen:v1");
    fingerprint.text(&lifecycle.to_storage());
    fingerprint.number(generation.value());
    fingerprint.finish()
}

/// An operator's revoke or ban of one subject in one managed room, recorded
/// in any scope that is not retired or exhausted, closed ones included, and
/// with no initiator: the operator acts through the owner phase. It is how a
/// revoke the restored database lost is applied again.
#[derive(Clone, Debug)]
pub struct OperatorRevoke {
    pub operation: GrantOperationId,
    pub lifecycle: ScopeLifecycleId,
    pub room: jid::BareJid,
    pub subject: GrantSubject,
    /// `none` revokes, `outcast` bans.
    pub ban: bool,
}

impl OperatorRevoke {
    pub fn affiliation(&self) -> RoomAffiliation {
        if self.ban {
            RoomAffiliation::Outcast
        } else {
            RoomAffiliation::None
        }
    }

    pub(crate) fn fingerprint(&self) -> String {
        let mut fingerprint = Fingerprint::new("waddle:host-grant:operator-revoke:v1");
        fingerprint.text(&self.lifecycle.to_storage());
        fingerprint.text(self.room.as_str());
        fingerprint.text(self.subject.as_str());
        fingerprint.text(self.affiliation().as_storage());
        fingerprint.finish()
    }
}

/// What an operator's reopen did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReopenReport {
    /// Scopes reopened.
    pub reopened: usize,
    /// Scopes closed by a restore whose reconstruction is not yet verified
    /// and activated, or which a fence closed since: left closed.
    pub not_reconstructed: usize,
    /// Reconstructed scopes the restore lost operations in, left closed:
    /// each reopens only by name, once the operator confirms that what was
    /// lost has been applied again.
    pub lost_operations: usize,
}
