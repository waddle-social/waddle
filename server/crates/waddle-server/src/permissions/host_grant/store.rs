//! The canonical PostgreSQL store of the host-grant authority.
//!
//! Lock order of a store transaction: the ingress protocol epoch row first
//! (the guarded-store entry takes it), then the scope's state row
//! (`host_grant_scopes`) `FOR UPDATE`, then grant and journal rows. A store
//! transaction takes no ownership claim and no ingress row, performs no RPC
//! and waits on nothing but PostgreSQL, under bounded lock and statement
//! timeouts. An acceptance transaction takes the same scope row `FOR SHARE`
//! after its own claims (`ingress_uow::HostGrantAdmissionRepository`), so a
//! mutation waits for an earlier acceptance to resolve and a later acceptance
//! reads the mutated state.
//!
//! Each mutation runs on a task of its own: a caller that stops waiting
//! does not abandon the transaction, which resolves either way and is found
//! again by its operation identity.
//!
//! Enrollment and retirement are owner-phase operations. Both write the
//! lifecycle ledger, which the runtime role may only read, so PostgreSQL
//! refuses either on a runtime handle and the store reports
//! [`HostGrantError::OwnerPhaseRequired`]. So is a restore transition, which
//! changes the authority incarnation: the owner phase's first transaction
//! takes the authority row `FOR UPDATE` and replaces the incarnation, and
//! each later one takes a batch of scope state rows `FOR UPDATE` in
//! lifecycle order and closes them (see `restore`). An operator's reopen,
//! position acknowledgement and revoke are owner-phase acts too; the
//! runtime role may close a scope on fence evidence, which takes the
//! scope's state row like any mutation.

use std::collections::BTreeSet;
use std::future::Future;
use std::time::Duration;

use chrono::Utc;
use jid::BareJid;

use super::authority::VerifiedProjection;
use super::error::HostGrantError;
use super::issuance::{IssuanceBasis, IssuancePolicy, ServedAccountDomains, SubjectHome};
use super::marker::ProjectionMarker;
use super::model::{
    AdmissionState, AuthorityIncarnation, CandidateAdmission, ClosedReason, EnrollScope,
    GrantGeneration, GrantOperationId, GrantRow, GrantSubject, ManagedRoom, MutationReceipt,
    OperationKind, OperationOutcome, OperationRecord, ProjectionDigest, RetireScope,
    RoomAffiliation, ScopeLifecycleId, ScopeState, SetAffiliation, HOST_GRANT_POLICY_VERSION,
    HOST_GRANT_SCOPE_MAX_GRANT_ROWS, HOST_GRANT_SCOPE_MAX_ROOMS,
};
use super::policy::{grant_allows, honoured_affiliation, initiator_may_change, HostGrantAction};
use super::projection::{projection_digest, ProjectedGrant, ProjectionIntent, ProjectionPlan};
use super::restore::{
    fence_fingerprint, reopen_fingerprint, restore_fingerprint, AuthorityBinding, FenceEvidence,
    OperatorRevoke, ReconstructScope, ReopenReport, RestoreDeclaration, RestoreEpoch,
    RestoreFenceRefusal,
};
use super::witness::PostgresPosition;
use crate::db::read_only_timeout::{bound_postgres_waits, ReadOnlyTimeoutError};
use crate::db::{Database, DatabaseDriver, DatabaseError, Row, Transaction};
use crate::ingress_substrate::{
    begin_guarded_store_writer, set_local_transaction_timeouts, EnteredTransaction,
};
use crate::permissions::{Object, ObjectType};

/// Longest a store transaction waits for a row lock.
const LOCK_TIMEOUT: Duration = Duration::from_secs(3);
/// Longest any one statement of a store transaction runs.
const STATEMENT_TIMEOUT: Duration = Duration::from_secs(5);
/// Most scopes one page of a recovery pass lists.
const PENDING_SCOPE_PAGE: i64 = 256;
/// Most scopes one transaction of a restore transition closes.
pub const RESTORE_TRANSITION_BATCH: usize = 256;
/// Longest the lookup behind a legacy permission write waits for a lock, and
/// longest it runs. The lookup reads one indexed row and takes no row lock;
/// the bounds keep a stalled database from holding the permission actor.
const ENROLLED_LOOKUP_LOCK_TIMEOUT: Duration = Duration::from_millis(500);
const ENROLLED_LOOKUP_STATEMENT_TIMEOUT: Duration = Duration::from_secs(1);

const SCOPE_COLUMNS: &str = "lifecycle_id, authority_incarnation, policy_version, generation, \
     operation_id, projected_generation, admission_state, closed_reason";
const GRANT_COLUMNS: &str = "room_jid, subject_jid, affiliation, effective_affiliation, \
     changed_generation, operation_id, issuance_basis";
const OPERATION_COLUMNS: &str =
    "operation_id, lifecycle_id, generation, fingerprint, kind, authority_incarnation, \
     projection_digest";

/// A commit fault injected by a test.
#[cfg(feature = "test-support")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostGrantCommitFault {
    /// PostgreSQL commits the transaction and the acknowledgement does not
    /// reach the caller.
    ReplyLostAfterCommit,
    /// The transaction ends without a commit and the caller learns nothing.
    LostBeforeCommit,
}

#[cfg(feature = "test-support")]
tokio::task_local! {
    static COMMIT_FAULT: HostGrantCommitFault;
}

/// Run `future` so that the first host-grant mutation it awaits ends with
/// `fault` instead of an acknowledged commit.
#[cfg(feature = "test-support")]
pub async fn with_host_grant_commit_fault<F: Future>(
    fault: HostGrantCommitFault,
    future: F,
) -> F::Output {
    COMMIT_FAULT.scope(fault, future).await
}

/// How a mutation's commit ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CommitMode {
    Acknowledged,
    #[cfg(feature = "test-support")]
    Fault(HostGrantCommitFault),
}

fn commit_mode() -> CommitMode {
    #[cfg(feature = "test-support")]
    if let Ok(fault) = COMMIT_FAULT.try_with(|fault| *fault) {
        return CommitMode::Fault(fault);
    }
    CommitMode::Acknowledged
}

/// The outcome of activating a verified projection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activation {
    /// The exact operation was still current: its grants are now effective
    /// and the scope admits.
    Activated,
    /// The operation's projection was already activated.
    AlreadyActive,
    /// Another operation has been recorded since. Nothing was activated.
    Superseded,
    /// The exact operation of a scope a restore transition closed was still
    /// current: its grants are now effective, and the scope stays closed as
    /// `reconstructed` until an operator reopens it.
    Reconstructed,
}

/// What one pass of the restore fence reads from the canonical store before
/// it reads SpiceDB, in one snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FenceSnapshot {
    pub(crate) binding: AuthorityBinding,
    /// The fixed identity the position witness is filed under.
    pub(crate) authority_id: String,
    /// The database lineage the incarnation is bound to, once the owner
    /// phase has bound it.
    pub(crate) bound_lineage: Option<String>,
    /// The database's own lineage, if it records one.
    pub(crate) live_lineage: Option<String>,
    /// Whether any scope is enrolled, retired or not.
    pub(crate) holds_scope: bool,
    /// The scopes that admit, in lifecycle order.
    pub(crate) admitting: Vec<ScopeLifecycleId>,
    /// Scopes closed as `restored`.
    pub(crate) restored: usize,
    /// Scopes closed as `reconstructed`.
    pub(crate) reconstructed: usize,
    /// The system identifier and timeline an operator acknowledged.
    pub(crate) position_acknowledgement: Option<(i64, u32)>,
}

/// What a position witness is filed and compared under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PositionAuthority {
    pub(crate) authority_id: String,
    pub(crate) restore_epoch: RestoreEpoch,
    pub(crate) position_acknowledgement: Option<(i64, u32)>,
}

/// A scope's latest recorded operation, as a journal witness states it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct JournalHead {
    pub(crate) marker: ProjectionMarker,
    /// Whether an operator reopened the scope under the operation's
    /// incarnation.
    pub(crate) reopened: bool,
}

/// The canonical grant store of enrolled scopes.
#[derive(Clone)]
pub struct HostGrantStore {
    db: Database,
}

impl HostGrantStore {
    /// Attach to the host-grant relations of a PostgreSQL database. Nothing
    /// is created: the relations come from the migration catalog, and the
    /// runtime role's privileges on them from the owner phase.
    pub fn open(db: Database) -> Result<Self, HostGrantError> {
        if db.driver() != DatabaseDriver::Postgres {
            return Err(HostGrantError::PostgresRequired);
        }
        Ok(Self { db })
    }

    /// Verify that this handle's role is a restricted runtime role holding
    /// exactly the store's privileges: it reads every host-grant relation,
    /// updates scope state, inserts and updates grant rows, appends to the
    /// operation journal, and can delete or truncate nothing.
    pub async fn verify_runtime_privileges(&self) -> Result<(), HostGrantError> {
        super::runtime_attach::verify_current_role(&self.db).await
    }

    /// Whether any scope is enrolled and not retired, read in a bounded
    /// read-only snapshot. A database without the host-grant relations
    /// holds none. It tells an operator whether a replica that cannot admit
    /// managed rooms leaves a live scope unserved.
    pub async fn holds_unretired_scope(&self) -> Result<bool, HostGrantError> {
        let mut tx = self.begin_read().await?;
        let mut rows = tx
            .query(
                "SELECT pg_catalog.to_regclass('host_grant_scope_lifecycles') IS NOT NULL",
                (),
            )
            .await?;
        let present = match rows.next().await? {
            Some(row) => row.get::<bool>(0)?,
            None => false,
        };
        drop(rows);
        let unretired = if present {
            let mut rows = tx
                .query(
                    "SELECT EXISTS (SELECT 1 FROM host_grant_scope_lifecycles \
                     WHERE retired_at_ms IS NULL)",
                    (),
                )
                .await?;
            let unretired = match rows.next().await? {
                Some(row) => row.get::<bool>(0)?,
                None => false,
            };
            drop(rows);
            unretired
        } else {
            false
        };
        tx.rollback().await?;
        Ok(unretired)
    }

    async fn begin_write(&self) -> Result<EnteredTransaction<'_>, HostGrantError> {
        Ok(begin_guarded_store_writer(&self.db, LOCK_TIMEOUT, STATEMENT_TIMEOUT).await?)
    }

    /// A bounded read-only snapshot.
    async fn begin_read(&self) -> Result<Transaction<'_>, HostGrantError> {
        let mut tx = self.db.begin().await?;
        tx.execute(
            "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY",
            (),
        )
        .await?;
        bound_postgres_waits(&mut tx)
            .await
            .map_err(|error| match error {
                ReadOnlyTimeoutError::InvalidSetting { setting } => {
                    HostGrantError::InvalidTimeoutSetting { setting }
                }
                ReadOnlyTimeoutError::Database(error) => HostGrantError::Database(error),
            })?;
        Ok(tx)
    }

    /// The deployment's current authority incarnation.
    pub async fn authority_incarnation(&self) -> Result<AuthorityIncarnation, HostGrantError> {
        let mut tx = self.begin_read().await?;
        let incarnation = load_incarnation(&mut tx).await?;
        tx.rollback().await?;
        Ok(incarnation)
    }

    /// The current incarnation, the restore epoch it was established under,
    /// the start of the PostgreSQL server that answered and the database it
    /// answered for, read in one snapshot. A replica's attachment keeps it,
    /// and every acceptance the replica makes compares it with what it
    /// reads.
    pub async fn authority_binding(&self) -> Result<AuthorityBinding, HostGrantError> {
        let mut tx = self.begin_read().await?;
        let binding = load_binding(&mut tx, "host_grant_authority").await?;
        tx.rollback().await?;
        Ok(binding)
    }

    /// What one pass of the restore fence reads before SpiceDB, in one
    /// snapshot. A database without the host-grant relations has no
    /// authority to fence.
    pub(crate) async fn fence_snapshot(&self) -> Result<Option<FenceSnapshot>, HostGrantError> {
        let mut tx = self.begin_read().await?;
        if !query_flag(
            &mut tx,
            "SELECT pg_catalog.to_regclass('host_grant_authority') IS NOT NULL",
        )
        .await?
        {
            tx.rollback().await?;
            return Ok(None);
        }
        let binding = load_binding(&mut tx, "host_grant_authority").await?;
        let mut rows = tx
            .query(
                "SELECT lineage_uuid, authority_id, position_acknowledgement \
                 FROM host_grant_authority WHERE singleton",
                (),
            )
            .await?;
        let row = rows.next().await?.ok_or(corrupt("authority incarnation"))?;
        let bound_lineage = row.get::<Option<String>>(0)?;
        let authority_id = row.get::<String>(1)?;
        let position_acknowledgement = row
            .get::<Option<String>>(2)?
            .map(|text| parse_acknowledgement(&text).ok_or(corrupt("position_acknowledgement")))
            .transpose()?;
        drop(rows);
        let live_lineage = load_live_lineage(&mut tx).await?;
        let mut rows = tx
            .query(
                "SELECT count(*), \
                 count(*) FILTER (WHERE closed_reason = 'restored'), \
                 count(*) FILTER (WHERE closed_reason = 'reconstructed') \
                 FROM host_grant_scopes",
                (),
            )
            .await?;
        let row = rows.next().await?.ok_or(corrupt("scope count"))?;
        let count = |index| {
            row.get::<i64>(index)
                .ok()
                .and_then(|value| usize::try_from(value).ok())
                .ok_or(corrupt("scope count"))
        };
        let (scopes, restored, reconstructed) = (count(0)?, count(1)?, count(2)?);
        drop(rows);
        let mut rows = tx
            .query(
                "SELECT lifecycle_id FROM host_grant_scopes WHERE admission_state = 'active' \
                 ORDER BY lifecycle_id",
                (),
            )
            .await?;
        let mut admitting = Vec::new();
        while let Some(row) = rows.next().await? {
            admitting.push(
                ScopeLifecycleId::from_storage(&row.get::<String>(0)?)
                    .ok_or(corrupt("lifecycle_id"))?,
            );
        }
        drop(rows);
        tx.rollback().await?;
        Ok(Some(FenceSnapshot {
            binding,
            authority_id,
            bound_lineage,
            live_lineage,
            holds_scope: scopes > 0,
            admitting,
            restored,
            reconstructed,
            position_acknowledgement,
        }))
    }

    /// Where the PostgreSQL server stands: its system identifier, the
    /// timeline it writes on and its flushed WAL position. A role that may
    /// not run the functions is refused with
    /// [`RestoreFenceRefusal::PositionUnreadable`].
    pub async fn postgres_position(&self) -> Result<PostgresPosition, HostGrantError> {
        let mut tx = self.begin_read().await?;
        let mut rows = tx
            .query(
                "SELECT (SELECT system_identifier FROM pg_catalog.pg_control_system()), \
                 pg_catalog.pg_walfile_name(pg_catalog.pg_current_wal_flush_lsn()), \
                 (pg_catalog.pg_current_wal_flush_lsn() - '0/0'::pg_lsn)::text",
                (),
            )
            .await
            .map_err(|error| match HostGrantError::from_database(error) {
                HostGrantError::OwnerPhaseRequired => {
                    RestoreFenceRefusal::PositionUnreadable.into()
                }
                other => other,
            })?;
        let row = rows.next().await?.ok_or(corrupt("postgres position"))?;
        let system_identifier = row.get::<i64>(0)?;
        let walfile = row.get::<String>(1)?;
        let lsn = row.get::<String>(2)?;
        drop(rows);
        tx.rollback().await?;
        let timeline = walfile
            .get(..8)
            .and_then(|timeline| u32::from_str_radix(timeline, 16).ok())
            .ok_or(corrupt("postgres timeline"))?;
        let lsn = lsn
            .parse::<u64>()
            .map_err(|_| corrupt("postgres position"))?;
        Ok(PostgresPosition {
            system_identifier,
            timeline,
            lsn,
        })
    }

    /// The authority identity, its restore epoch and the acknowledged
    /// PostgreSQL system and timeline, which the position witness is filed
    /// and compared under. `None` without the host-grant relations.
    pub(crate) async fn position_authority(
        &self,
    ) -> Result<Option<PositionAuthority>, HostGrantError> {
        let mut tx = self.begin_read().await?;
        if !query_flag(
            &mut tx,
            "SELECT pg_catalog.to_regclass('host_grant_authority') IS NOT NULL",
        )
        .await?
        {
            tx.rollback().await?;
            return Ok(None);
        }
        let mut rows = tx
            .query(
                "SELECT authority_id, restore_epoch, position_acknowledgement, \
                 EXISTS (SELECT 1 FROM host_grant_scopes) \
                 FROM host_grant_authority WHERE singleton",
                (),
            )
            .await?;
        let row = rows.next().await?.ok_or(corrupt("authority incarnation"))?;
        let authority = PositionAuthority {
            authority_id: row.get::<String>(0)?,
            restore_epoch: RestoreEpoch::from_storage(row.get::<i64>(1)?)
                .ok_or(corrupt("restore_epoch"))?,
            position_acknowledgement: row
                .get::<Option<String>>(2)?
                .map(|text| parse_acknowledgement(&text).ok_or(corrupt("position_acknowledgement")))
                .transpose()?,
        };
        let holds_scope = row.get::<bool>(3)?;
        drop(rows);
        tx.rollback().await?;
        // Without a scope there is no authority a position could protect.
        Ok(holds_scope.then_some(authority))
    }

    /// Whether a scope's journal records an operation under `incarnation`:
    /// the scope was bound to it at some generation.
    pub(crate) async fn journal_holds_incarnation(
        &self,
        lifecycle: ScopeLifecycleId,
        incarnation: AuthorityIncarnation,
    ) -> Result<bool, HostGrantError> {
        let mut tx = self.begin_read().await?;
        let held = holds_incarnation(&mut tx, lifecycle, incarnation).await?;
        tx.rollback().await?;
        Ok(held)
    }

    /// Whether an operator reopened the scope under `incarnation`: its
    /// journal records a `reopen` operation of that incarnation.
    pub(crate) async fn journal_holds_reopen(
        &self,
        lifecycle: ScopeLifecycleId,
        incarnation: AuthorityIncarnation,
    ) -> Result<bool, HostGrantError> {
        let mut tx = self.begin_read().await?;
        let held = holds_reopen(&mut tx, lifecycle, incarnation).await?;
        tx.rollback().await?;
        Ok(held)
    }

    /// The scope's latest recorded operation as a marker, and whether an
    /// operator reopened the scope under that operation's incarnation, read
    /// in one snapshot. `None` for a scope the store does not hold.
    pub(crate) async fn journal_head(
        &self,
        lifecycle: ScopeLifecycleId,
    ) -> Result<Option<JournalHead>, HostGrantError> {
        let mut tx = self.begin_read().await?;
        let Some(scope) = load_scope(&mut tx, lifecycle, false).await? else {
            tx.rollback().await?;
            return Ok(None);
        };
        let recorded = load_operation(&mut tx, scope.operation)
            .await?
            .ok_or(corrupt("current operation"))?
            .record;
        let reopened = holds_reopen(&mut tx, lifecycle, recorded.authority_incarnation).await?;
        tx.rollback().await?;
        Ok(Some(JournalHead {
            marker: ProjectionMarker {
                lifecycle,
                incarnation: recorded.authority_incarnation,
                generation: recorded.generation,
                operation: recorded.operation,
                digest: recorded.projection_digest,
            },
            reopened,
        }))
    }

    /// Close a scope on `evidence` that the database lost operations, until
    /// a restore is declared: its next generation is recorded under a
    /// `fence` operation, it admits nothing, its effective standing is
    /// lowered to `none` at most, and it is not projected again.
    ///
    /// The evidence is compared again under the scope's guard: a marker or
    /// witness the journal now records, or a scope that no longer admits
    /// when the evidence is a missing marker, closes nothing. A scope that is
    /// retired, exhausted or fenced already is left as it is. The answer is
    /// whether this call closed the scope. The runtime role may run it.
    pub(crate) async fn fence_scope(
        &self,
        lifecycle: ScopeLifecycleId,
        evidence: &FenceEvidence,
    ) -> Result<bool, HostGrantError> {
        let store = self.clone();
        let evidence = evidence.clone();
        let mode = commit_mode();
        own(async move { store.fence_owned(lifecycle, &evidence, mode).await }).await
    }

    /// Bind the authority to the declared restore epoch and the database
    /// lineage, declaring a restore when `configured` exceeds the stored
    /// epoch.
    ///
    /// - `configured` below the stored epoch: refused. The configuration
    ///   predates the latest restore transition.
    /// - `configured` equal to it: the lineage is bound if it is not yet.
    ///   A database that holds a scope and whose lineage differs from the
    ///   bound one was adopted after a restore or a clone, and is refused
    ///   until the restore is declared; one that holds no scope is bound to
    ///   its new lineage. A transition an earlier run recorded and did not
    ///   finish is finished.
    /// - `configured` above it: a restore transition. One transaction mints
    ///   a new incarnation and records it with the epoch and the live
    ///   lineage; from its commit no scope bound to an earlier incarnation
    ///   admits. Then, in transactions of at most
    ///   [`RESTORE_TRANSITION_BATCH`] scopes, every scope that is not
    ///   retired or exhausted is bound to it at its next generation, closed
    ///   as `restored` with a `restore` operation in its journal, and its
    ///   effective standing lowered to `none` at most. A scope with no
    ///   generation left is closed as exhausted instead.
    ///
    /// Owner phase only: the runtime role reads the authority row and cannot
    /// update it, so PostgreSQL refuses the transaction on a runtime handle
    /// ([`HostGrantError::OwnerPhaseRequired`]) and nothing changes. A rerun
    /// with the same epoch after a run that stopped finds the epoch recorded
    /// and finishes what is left.
    pub async fn prepare_restore_incarnation(
        &self,
        configured: RestoreEpoch,
    ) -> Result<RestoreDeclaration, HostGrantError> {
        let store = self.clone();
        let mode = commit_mode();
        own(async move { store.prepare_restore_owned(configured, mode).await }).await
    }

    /// Reopen the scopes whose reconstruction after a declared restore was
    /// verified and activated: `lifecycle`, or every such scope. Each is
    /// reopened in a transaction of its own, under a `reopen` operation at
    /// its next generation, and admits from that commit; its projection
    /// follows with the next reconciliation pass. A scope that is not
    /// `reconstructed`, or whose latest operation is not yet verified, is
    /// left closed and counted.
    ///
    /// Owner phase only by convention: it is the operator's
    /// acknowledgement of the restore, run as the migrator login.
    pub async fn reopen_scopes(
        &self,
        lifecycle: Option<ScopeLifecycleId>,
    ) -> Result<ReopenReport, HostGrantError> {
        let store = self.clone();
        let mode = commit_mode();
        own(async move { store.reopen_owned(lifecycle, mode).await }).await
    }

    /// Record the PostgreSQL system and timeline the server now writes on
    /// as acknowledged by an operator, after a promotion or a move to
    /// another cluster that the position witness cannot verify. The next
    /// fence pass moves the witness to the current position. Returns the
    /// acknowledged system identifier and timeline.
    ///
    /// Owner phase only: the runtime role cannot update the authority row.
    pub async fn acknowledge_position(&self) -> Result<(i64, u32), HostGrantError> {
        let position = self.postgres_position().await?;
        let store = self.clone();
        let mode = commit_mode();
        own(async move { store.acknowledge_position_owned(position, mode).await }).await
    }

    /// Record an operator's revoke or ban (see [`OperatorRevoke`]). The
    /// subject's recorded and effective affiliation are lowered at the
    /// commit, whatever the scope's state, unless the scope is retired or
    /// exhausted or the subject is the room's last owner of a served
    /// account domain. `served` decides the issuance basis recorded with a
    /// row and which owners count.
    pub async fn operator_revoke(
        &self,
        command: &OperatorRevoke,
        served: &ServedAccountDomains,
    ) -> Result<MutationReceipt, HostGrantError> {
        let store = self.clone();
        let command = command.clone();
        let served = served.clone();
        let mode = commit_mode();
        own(async move { store.operator_revoke_owned(&command, &served, mode).await }).await
    }
    /// Enroll a fresh scope: its lifecycle, its rooms and its bootstrap
    /// grants, closed until the first projection is verified and activated.
    ///
    /// Every bootstrap grant passes `policy`, so a privileged or moderator
    /// grant to a remote JID is refused unless its home is a trusted home,
    /// and every room starts with an owner of a served account domain.
    ///
    /// Owner phase only: the transaction inserts into relations the runtime
    /// role may only read. On a runtime handle it is refused with
    /// [`HostGrantError::OwnerPhaseRequired`] and changes nothing.
    pub async fn enroll_scope(
        &self,
        command: &EnrollScope,
        policy: &IssuancePolicy,
    ) -> Result<MutationReceipt, HostGrantError> {
        let bases = validate_enrollment(command, policy)?;
        let store = self.clone();
        let command = command.clone();
        let mode = commit_mode();
        own(async move { store.enroll_owned(&command, &bases, mode).await }).await
    }

    /// Record one affiliation change.
    ///
    /// The scope must be at `expected_generation`, and the initiator's
    /// authority is evaluated again from the state locked by this
    /// transaction. A reduction of the subject's access is effective when
    /// the transaction commits; an increase is recorded and not effective
    /// until its projection is verified and activated.
    pub async fn set_affiliation(
        &self,
        command: &SetAffiliation,
        policy: &IssuancePolicy,
    ) -> Result<MutationReceipt, HostGrantError> {
        let basis = policy.check(&command.subject, command.affiliation)?;
        let served = policy.served_account_domains().clone();
        let store = self.clone();
        let command = command.clone();
        let mode = commit_mode();
        own(async move {
            store
                .set_affiliation_owned(&command, basis, &served, mode)
                .await
        })
        .await
    }

    /// Close a scope for good. Its grants stop admitting at the commit.
    ///
    /// Owner phase only: retirement stamps the scope's row of the lifecycle
    /// ledger, which the runtime role may only read. On a runtime handle it
    /// is refused with [`HostGrantError::OwnerPhaseRequired`] and changes
    /// nothing.
    pub async fn retire_scope(
        &self,
        command: &RetireScope,
    ) -> Result<MutationReceipt, HostGrantError> {
        let store = self.clone();
        let command = command.clone();
        let mode = commit_mode();
        own(async move { store.retire_owned(&command, mode).await }).await
    }

    /// Read back what is recorded under an operation identity. This is how
    /// an unacknowledged commit is resolved: `Committed` or `Absent` is the
    /// primary's answer, and a failure of this read leaves it unknown.
    pub async fn operation_outcome(
        &self,
        operation: GrantOperationId,
    ) -> Result<OperationOutcome, HostGrantError> {
        let mut tx = self.begin_read().await?;
        let outcome = match load_operation(&mut tx, operation).await? {
            Some(recorded) => OperationOutcome::Committed(recorded.record),
            None => OperationOutcome::Absent,
        };
        tx.rollback().await?;
        Ok(outcome)
    }

    /// The latest state of a scope.
    pub async fn scope(
        &self,
        lifecycle: ScopeLifecycleId,
    ) -> Result<Option<ScopeState>, HostGrantError> {
        let mut tx = self.begin_read().await?;
        let scope = load_scope(&mut tx, lifecycle, false).await?;
        tx.rollback().await?;
        Ok(scope)
    }

    /// Every grant row of a scope.
    pub async fn grants(
        &self,
        lifecycle: ScopeLifecycleId,
    ) -> Result<Vec<GrantRow>, HostGrantError> {
        let mut tx = self.begin_read().await?;
        let grants = load_grants(&mut tx, lifecycle).await?;
        tx.rollback().await?;
        Ok(grants)
    }

    /// One page of the scopes that hold an operation whose projection is
    /// not yet verified and activated, in lifecycle order, starting after
    /// `after`. A caller that follows the pages to an empty one has seen
    /// every pending scope, so a scope that stays pending (a contradiction,
    /// an unreachable projection) never hides another. A scope a restore
    /// transition closed is pending until its reconstruction is activated;
    /// a fenced scope is not projected again and is not listed.
    pub async fn pending_scopes(
        &self,
        after: Option<ScopeLifecycleId>,
    ) -> Result<Vec<ScopeLifecycleId>, HostGrantError> {
        let after = after.map_or_else(String::new, ScopeLifecycleId::to_storage);
        let mut tx = self.begin_read().await?;
        let mut rows = tx
            .query(
                "SELECT lifecycle_id FROM host_grant_scopes \
                 WHERE projected_generation < generation \
                   AND (closed_reason IS NULL \
                        OR closed_reason IN ('enrolled', 'restored', 'reconstructed')) \
                   AND lifecycle_id > ? \
                 ORDER BY lifecycle_id LIMIT ?",
                crate::db_params![after, PENDING_SCOPE_PAGE],
            )
            .await?;
        let mut scopes = Vec::new();
        while let Some(row) = rows.next().await? {
            scopes.push(
                ScopeLifecycleId::from_storage(&row.get::<String>(0)?)
                    .ok_or(corrupt("lifecycle_id"))?,
            );
        }
        drop(rows);
        tx.rollback().await?;
        Ok(scopes)
    }

    /// One page of the scopes that are not retired or exhausted, in
    /// lifecycle order, starting after `after`.
    pub(crate) async fn scope_page(
        &self,
        after: Option<ScopeLifecycleId>,
    ) -> Result<Vec<ScopeLifecycleId>, HostGrantError> {
        let after = after.map_or_else(String::new, ScopeLifecycleId::to_storage);
        let mut tx = self.begin_read().await?;
        let mut rows = tx
            .query(
                "SELECT lifecycle_id FROM host_grant_scopes \
                 WHERE (closed_reason IS NULL OR closed_reason NOT IN ('retired', 'exhausted')) \
                   AND lifecycle_id > ? \
                 ORDER BY lifecycle_id LIMIT ?",
                crate::db_params![after, PENDING_SCOPE_PAGE],
            )
            .await?;
        let mut scopes = Vec::new();
        while let Some(row) = rows.next().await? {
            scopes.push(
                ScopeLifecycleId::from_storage(&row.get::<String>(0)?)
                    .ok_or(corrupt("lifecycle_id"))?,
            );
        }
        drop(rows);
        tx.rollback().await?;
        Ok(scopes)
    }

    /// Read the policy state for one subject and action outside any
    /// acceptance transaction, honouring grants as admission does for a
    /// deployment that serves `served`. The answer is a candidate: it holds
    /// no lock and acceptance decides again.
    pub async fn candidate_admission(
        &self,
        room: &BareJid,
        subject: &GrantSubject,
        action: HostGrantAction,
        served: &ServedAccountDomains,
    ) -> Result<CandidateAdmission, HostGrantError> {
        let mut tx = self.begin_read().await?;
        let mut rows = tx
            .query(
                "SELECT lifecycle_id FROM host_grant_rooms WHERE room_jid = ?",
                crate::db_params![room.as_str()],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            drop(rows);
            tx.rollback().await?;
            return Ok(CandidateAdmission {
                admitted: false,
                lifecycle: None,
                generation: None,
            });
        };
        let lifecycle = ScopeLifecycleId::from_storage(&row.get::<String>(0)?)
            .ok_or(corrupt("lifecycle_id"))?;
        drop(rows);
        let scope = load_scope(&mut tx, lifecycle, false)
            .await?
            .ok_or(corrupt("room scope"))?;
        let current = load_incarnation(&mut tx).await?;
        let mut rows = tx
            .query(
                "SELECT affiliation, effective_affiliation FROM host_grants \
                 WHERE lifecycle_id = ? AND room_jid = ? AND subject_jid = ?",
                crate::db_params![lifecycle.to_storage(), room.as_str(), subject.as_str()],
            )
            .await?;
        let held = match rows.next().await? {
            Some(row) => Some((
                affiliation_column(&row, 0, "affiliation")?,
                affiliation_column(&row, 1, "effective_affiliation")?,
            )),
            None => None,
        };
        drop(rows);
        tx.rollback().await?;
        let admitted = scope_admits(&scope, current)
            && held.is_some_and(|(recorded, effective)| {
                grant_allows(recorded, effective, served.home_of(subject.jid()), action)
            });
        Ok(CandidateAdmission {
            admitted,
            lifecycle: Some(lifecycle),
            generation: Some(scope.generation),
        })
    }

    /// Whether `object` is an authorization object of an enrolled scope: the
    /// channel of a managed room or the Space of a scope. Legacy permission
    /// writes to such an object are refused. A retired scope still counts:
    /// retiring a scope does not hand its objects back to legacy writers.
    pub async fn enrolled_object(&self, object: &Object) -> Result<bool, HostGrantError> {
        let (sql, id) = match object.object_type {
            ObjectType::Channel => (
                "SELECT 1 FROM host_grant_rooms WHERE channel_id = ? LIMIT 1",
                object.id.as_str(),
            ),
            ObjectType::Space => (
                "SELECT 1 FROM host_grant_scope_lifecycles WHERE space_node = ? LIMIT 1",
                object.id.as_str(),
            ),
            ObjectType::Server | ObjectType::Message | ObjectType::Dm | ObjectType::Role => {
                return Ok(false)
            }
        };
        let mut tx = self.db.begin().await?;
        if !set_local_transaction_timeouts(
            &mut tx,
            ENROLLED_LOOKUP_LOCK_TIMEOUT,
            ENROLLED_LOOKUP_STATEMENT_TIMEOUT,
        )
        .await?
        {
            return Err(HostGrantError::BoundsUnproven);
        }
        let mut rows = tx
            .query(sql, crate::db_params![id])
            .await
            .map_err(HostGrantError::from_database)?;
        let enrolled = rows.next().await?.is_some();
        drop(rows);
        tx.rollback().await?;
        Ok(enrolled)
    }

    /// The canonical state a reconciliation projects, read in one snapshot.
    ///
    /// Only the current operation's intent is ever returned: a superseded
    /// operation has no plan. A retired or exhausted scope, or one bound to
    /// another incarnation or policy version, has none either.
    pub async fn projection_plan(
        &self,
        lifecycle: ScopeLifecycleId,
    ) -> Result<ProjectionPlan, HostGrantError> {
        let mut tx = self.begin_read().await?;
        let scope = load_scope(&mut tx, lifecycle, false)
            .await?
            .ok_or(HostGrantError::UnknownScope)?;
        let current = load_incarnation(&mut tx).await?;
        refuse_unusable_scope(&scope, current)?;
        let rooms = load_rooms(&mut tx, lifecycle).await?;
        let grants = load_grants(&mut tx, lifecycle).await?;
        let recorded = load_operation(&mut tx, scope.operation)
            .await?
            .ok_or(corrupt("current operation"))?;
        tx.rollback().await?;
        let record = recorded.record;
        if record.lifecycle != lifecycle
            || record.generation != scope.generation
            || record.authority_incarnation != scope.authority_incarnation
        {
            return Err(corrupt("current operation"));
        }
        let projected = projected_grants(&rooms, &grants)?;
        let digest = projection_digest(scope.policy_version, lifecycle, &projected)
            .ok_or(corrupt("projection digest"))?;
        if digest != record.projection_digest {
            return Err(corrupt("projection digest"));
        }
        Ok(ProjectionPlan {
            intent: ProjectionIntent {
                marker: ProjectionMarker {
                    lifecycle,
                    incarnation: scope.authority_incarnation,
                    generation: scope.generation,
                    operation: scope.operation,
                    digest,
                },
                channels: rooms
                    .iter()
                    .map(|room| room.channel_id().to_owned())
                    .collect(),
                grants: projected,
            },
            projected_generation: scope.projected_generation,
            restored: scope.closed_reason == Some(ClosedReason::Restored),
        })
    }

    /// The marker the journal records for one generation of a lifecycle, if
    /// that generation was recorded. A marker read from the projection is
    /// valid only if it equals one of these.
    pub async fn recorded_marker(
        &self,
        lifecycle: ScopeLifecycleId,
        generation: GrantGeneration,
    ) -> Result<Option<ProjectionMarker>, HostGrantError> {
        let mut tx = self.begin_read().await?;
        let recorded = load_recorded_marker(&mut tx, lifecycle, generation).await?;
        tx.rollback().await?;
        Ok(recorded)
    }

    /// Make the grants of a verified projection effective, if the operation
    /// it projected is still the scope's current one.
    ///
    /// The witness is what verification produces after it read the
    /// projection back and found exactly the canonical intent
    /// (`HostGrantAuthority`); nothing else can ask for an activation. The
    /// comparison covers the lifecycle, the authority incarnation, the
    /// generation, the operation and the policy version, under the scope's
    /// guard. An activation prepared for an operation that has since been
    /// superseded changes nothing.
    pub(crate) async fn activate(
        &self,
        verified: &VerifiedProjection,
    ) -> Result<Activation, HostGrantError> {
        let store = self.clone();
        let marker = verified.marker().clone();
        own(async move { store.activate_owned(&marker).await }).await
    }

    /// Record the changes a reconstruction found before it rebuilds the
    /// projection of a scope a restore transition closed.
    ///
    /// The scope must still be closed as `restored` and at
    /// `expected_generation`. A change that would not lower the subject's
    /// recorded affiliation is left out, a lowering that would raise what
    /// admission honours for an account `served` does not hold (a remote
    /// privileged grant, honoured as none, lowered to `member`) is recorded
    /// at `none` instead, and a change to a subject without a row records
    /// one at `none` or `outcast`, within the scope's row bound
    /// ([`HostGrantError::GrantLimit`] otherwise). The scope stays closed.
    /// `None` when nothing is left to change.
    pub(crate) async fn record_reconstruction(
        &self,
        command: &ReconstructScope,
        served: &ServedAccountDomains,
    ) -> Result<Option<OperationRecord>, HostGrantError> {
        let store = self.clone();
        let command = command.clone();
        let served = served.clone();
        let mode = commit_mode();
        own(async move { store.reconstruct_owned(&command, &served, mode).await }).await
    }

    /// Enroll, and answer a conflict with what the journal holds by then.
    ///
    /// An enrollment has no guard to wait on before its first insert: a
    /// concurrent submission of the same operation meets the first one at a
    /// unique key instead, after the first committed. The journal is read
    /// again at that point, so an exact retry returns the recorded outcome
    /// and the same identity with other input is a conflict, whichever of
    /// the two submissions reached the key first.
    async fn enroll_owned(
        &self,
        command: &EnrollScope,
        bases: &[IssuanceBasis],
        mode: CommitMode,
    ) -> Result<MutationReceipt, HostGrantError> {
        let fingerprint = command.fingerprint();
        match self.enroll_once(command, bases, &fingerprint, mode).await {
            Err(
                conflict @ (HostGrantError::ScopeAlreadyEnrolled
                | HostGrantError::RoomAlreadyBound
                | HostGrantError::Database(_)),
            ) => {
                let mut tx = self.begin_read().await?;
                let recorded = load_operation(&mut tx, command.operation).await?;
                tx.rollback().await?;
                match recorded {
                    Some(recorded) => recorded.replay(&fingerprint),
                    None => Err(conflict),
                }
            }
            outcome => outcome,
        }
    }

    async fn enroll_once(
        &self,
        command: &EnrollScope,
        bases: &[IssuanceBasis],
        fingerprint: &str,
        mode: CommitMode,
    ) -> Result<MutationReceipt, HostGrantError> {
        let lifecycle = command.lifecycle.to_storage();
        let operation = command.operation.to_storage();
        let now = Utc::now().timestamp_millis();
        let mut tx = self.begin_write().await?;
        if let Some(recorded) = load_operation(&mut tx, command.operation)
            .await
            .map_err(statement_failure)?
        {
            tx.rollback().await?;
            return recorded.replay(fingerprint);
        }
        let incarnation = load_incarnation(&mut tx).await.map_err(statement_failure)?;
        let inserted = tx
            .execute(
                "INSERT INTO host_grant_scope_lifecycles \
                 (lifecycle_id, service_jid, space_node, enrolled_at_ms) VALUES (?, ?, ?, ?) \
                 ON CONFLICT DO NOTHING",
                crate::db_params![
                    lifecycle.as_str(),
                    command.key.service().as_str(),
                    command.key.node(),
                    now
                ],
            )
            .await
            .map_err(HostGrantError::from_database)?;
        if inserted != 1 {
            tx.rollback().await?;
            return Err(HostGrantError::ScopeAlreadyEnrolled);
        }
        tx.execute(
            "INSERT INTO host_grant_scopes (lifecycle_id, authority_incarnation, policy_version, \
             generation, operation_id, projected_generation, admission_state, closed_reason, \
             updated_at_ms) VALUES (?, ?, ?, ?, ?, 0, 'closed', 'enrolled', ?)",
            crate::db_params![
                lifecycle.as_str(),
                incarnation.to_storage(),
                i64::from(HOST_GRANT_POLICY_VERSION),
                GrantGeneration::FIRST.value(),
                operation.as_str(),
                now
            ],
        )
        .await
        .map_err(HostGrantError::from_database)?;
        for room in &command.rooms {
            let bound = tx
                .execute(
                    "INSERT INTO host_grant_rooms \
                     (room_jid, channel_id, lifecycle_id, access_model, bound_at_ms) \
                     VALUES (?, ?, ?, 'members_only', ?) ON CONFLICT DO NOTHING",
                    crate::db_params![
                        room.room().as_str(),
                        room.channel_id(),
                        lifecycle.as_str(),
                        now
                    ],
                )
                .await
                .map_err(HostGrantError::from_database)?;
            if bound != 1 {
                tx.rollback().await?;
                return Err(HostGrantError::RoomAlreadyBound);
            }
        }
        let mut projected = Vec::with_capacity(command.bootstrap.len());
        for (ordinal, (grant, basis)) in command.bootstrap.iter().zip(bases).enumerate() {
            let channel_id = command
                .rooms
                .iter()
                .find(|room| room.room() == &grant.room)
                .map(ManagedRoom::channel_id)
                .ok_or(HostGrantError::UnknownRoom)?;
            // A bootstrap grant is an increase like any other: it is
            // recorded and becomes effective with the first activation.
            let effective = grant.affiliation.min(RoomAffiliation::None);
            tx.execute(
                "INSERT INTO host_grants (lifecycle_id, room_jid, subject_jid, affiliation, \
                 effective_affiliation, changed_generation, operation_id, issuance_basis) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                crate::db_params![
                    lifecycle.as_str(),
                    grant.room.as_str(),
                    grant.subject.as_str(),
                    grant.affiliation.as_storage(),
                    effective.as_storage(),
                    GrantGeneration::FIRST.value(),
                    operation.as_str(),
                    basis.as_storage()
                ],
            )
            .await
            .map_err(HostGrantError::from_database)?;
            insert_change(
                &mut tx,
                RecordedChange {
                    operation: &operation,
                    ordinal,
                    room: &grant.room,
                    subject: &grant.subject,
                    affiliation: grant.affiliation,
                    previous: RoomAffiliation::None,
                    basis: *basis,
                },
            )
            .await?;
            projected.push(ProjectedGrant {
                channel_id: channel_id.to_owned(),
                subject: grant.subject.clone(),
                affiliation: grant.affiliation,
            });
        }
        let digest = projection_digest(HOST_GRANT_POLICY_VERSION, command.lifecycle, &projected)
            .ok_or(corrupt("projection digest"))?;
        let record = OperationRecord {
            operation: command.operation,
            lifecycle: command.lifecycle,
            generation: GrantGeneration::FIRST,
            kind: OperationKind::Enroll,
            authority_incarnation: incarnation,
            projection_digest: digest,
        };
        insert_operation(&mut tx, &record, fingerprint, None, now).await?;
        commit(tx, mode).await?;
        Ok(MutationReceipt::Recorded(record))
    }

    async fn set_affiliation_owned(
        &self,
        command: &SetAffiliation,
        basis: IssuanceBasis,
        served: &ServedAccountDomains,
        mode: CommitMode,
    ) -> Result<MutationReceipt, HostGrantError> {
        let fingerprint = command.fingerprint();
        let lifecycle = command.lifecycle.to_storage();
        let operation = command.operation.to_storage();
        let now = Utc::now().timestamp_millis();
        let mut tx = self.begin_write().await?;
        // The guard comes first: a concurrent submission of the same
        // operation waits here and then finds the recorded outcome.
        let Some(scope) = load_scope(&mut tx, command.lifecycle, true)
            .await
            .map_err(statement_failure)?
        else {
            tx.rollback().await?;
            return Err(HostGrantError::UnknownScope);
        };
        if let Some(recorded) = load_operation(&mut tx, command.operation)
            .await
            .map_err(statement_failure)?
        {
            tx.rollback().await?;
            return recorded.replay(&fingerprint);
        }
        let current = load_incarnation(&mut tx).await.map_err(statement_failure)?;
        let decision = async {
            refuse_unusable_scope(&scope, current)?;
            if scope.generation != command.expected_generation {
                return Err(HostGrantError::StaleGeneration {
                    current: scope.generation,
                });
            }
            let rooms = load_rooms(&mut tx, command.lifecycle).await?;
            let room = rooms
                .iter()
                .find(|room| room.room() == &command.room)
                .ok_or(HostGrantError::UnknownRoom)?;
            let grants = load_grants(&mut tx, command.lifecycle).await?;
            let in_room = |subject: &GrantSubject| {
                grants
                    .iter()
                    .find(|grant| grant.room == command.room && &grant.subject == subject)
            };
            let held = in_room(&command.subject);
            let recorded = held.map_or(RoomAffiliation::None, |grant| grant.affiliation);
            let effective = held.map_or(RoomAffiliation::None, |grant| grant.effective_affiliation);
            // The initiator's standing is what is effective and honoured
            // now, under the guard: a grant that was revoked, demoted or
            // never activated authorizes nothing, whatever the initiator saw
            // earlier, and neither does a privileged grant of an account
            // this deployment does not serve. While a restore is handled no
            // standing is honoured: the grant rows are those of the restored
            // database until an operator reopens the scope.
            let restore_handling = scope
                .closed_reason
                .is_some_and(ClosedReason::awaits_restore_handling);
            let initiator = in_room(&command.initiator)
                .filter(|grant| grant.affiliation != RoomAffiliation::Outcast)
                .filter(|_| !restore_handling)
                .map_or(RoomAffiliation::None, |grant| {
                    honoured_affiliation(
                        grant.effective_affiliation,
                        served.home_of(grant.subject.jid()),
                    )
                });
            if !initiator_may_change(initiator, recorded.max(effective), command.affiliation) {
                return Err(HostGrantError::InitiatorNotAuthorized);
            }
            // The room keeps an owner who can administer it: one whose
            // standing is recorded, effective and honoured. An owner that
            // still waits for its projection cannot yet administer the room,
            // and an owner of an account this deployment does not serve is
            // not honoured as one.
            let administers = |grant: &GrantRow| {
                grant.affiliation == RoomAffiliation::Owner
                    && grant.effective_affiliation == RoomAffiliation::Owner
                    && served.home_of(grant.subject.jid()) == SubjectHome::Served
            };
            let other_owners = grants
                .iter()
                .filter(|grant| grant.room == command.room && grant.subject != command.subject)
                .any(administers);
            let stays_owner =
                command.affiliation == RoomAffiliation::Owner && held.is_some_and(administers);
            if !other_owners && !stays_owner {
                return Err(HostGrantError::LastOwner);
            }
            if held.is_none() && grants.len() >= HOST_GRANT_SCOPE_MAX_GRANT_ROWS {
                return Err(HostGrantError::GrantLimit);
            }
            let mut projected = projected_grants(&rooms, &grants)?;
            match projected.iter_mut().find(|grant| {
                grant.channel_id == room.channel_id() && grant.subject == command.subject
            }) {
                Some(grant) => grant.affiliation = command.affiliation,
                None => projected.push(ProjectedGrant {
                    channel_id: room.channel_id().to_owned(),
                    subject: command.subject.clone(),
                    affiliation: command.affiliation,
                }),
            }
            let digest = projection_digest(scope.policy_version, command.lifecycle, &projected)
                .ok_or(corrupt("projection digest"))?;
            Ok((recorded, effective, digest))
        }
        .await;
        let (recorded, effective, digest) = match decision {
            Ok(decision) => decision,
            Err(error) => {
                tx.rollback().await?;
                return Err(statement_failure(error));
            }
        };
        let Some(generation) = scope.generation.next() else {
            return self
                .exhaust(tx, &scope, command.operation, &fingerprint, mode)
                .await;
        };
        // A reduction takes effect with this commit. An increase keeps the
        // subject's present effective standing until it is activated.
        let effective_after = effective.min(command.affiliation);
        tx.execute(
            "INSERT INTO host_grants (lifecycle_id, room_jid, subject_jid, affiliation, \
             effective_affiliation, changed_generation, operation_id, issuance_basis) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (lifecycle_id, room_jid, subject_jid) DO UPDATE SET \
             affiliation = EXCLUDED.affiliation, \
             effective_affiliation = EXCLUDED.effective_affiliation, \
             changed_generation = EXCLUDED.changed_generation, \
             operation_id = EXCLUDED.operation_id, \
             issuance_basis = EXCLUDED.issuance_basis",
            crate::db_params![
                lifecycle.as_str(),
                command.room.as_str(),
                command.subject.as_str(),
                command.affiliation.as_storage(),
                effective_after.as_storage(),
                generation.value(),
                operation.as_str(),
                basis.as_storage()
            ],
        )
        .await
        .map_err(HostGrantError::from_database)?;
        advance_scope(&mut tx, &scope, generation, &operation, now).await?;
        insert_change(
            &mut tx,
            RecordedChange {
                operation: &operation,
                ordinal: 0,
                room: &command.room,
                subject: &command.subject,
                affiliation: command.affiliation,
                previous: recorded,
                basis,
            },
        )
        .await?;
        let record = OperationRecord {
            operation: command.operation,
            lifecycle: command.lifecycle,
            generation,
            kind: OperationKind::SetAffiliation,
            authority_incarnation: scope.authority_incarnation,
            projection_digest: digest,
        };
        insert_operation(
            &mut tx,
            &record,
            &fingerprint,
            Some(command.initiator.as_str()),
            now,
        )
        .await?;
        commit(tx, mode).await?;
        Ok(MutationReceipt::Recorded(record))
    }

    async fn retire_owned(
        &self,
        command: &RetireScope,
        mode: CommitMode,
    ) -> Result<MutationReceipt, HostGrantError> {
        let fingerprint = command.fingerprint();
        let operation = command.operation.to_storage();
        let now = Utc::now().timestamp_millis();
        let mut tx = self.begin_write().await?;
        let Some(scope) = load_scope(&mut tx, command.lifecycle, true)
            .await
            .map_err(statement_failure)?
        else {
            tx.rollback().await?;
            return Err(HostGrantError::UnknownScope);
        };
        if let Some(recorded) = load_operation(&mut tx, command.operation)
            .await
            .map_err(statement_failure)?
        {
            tx.rollback().await?;
            return recorded.replay(&fingerprint);
        }
        let refusal = match scope.closed_reason {
            Some(ClosedReason::Retired) => Some(HostGrantError::ScopeRetired),
            Some(ClosedReason::Exhausted) => Some(HostGrantError::ScopeExhausted),
            Some(
                ClosedReason::Enrolled
                | ClosedReason::Restored
                | ClosedReason::Reconstructed
                | ClosedReason::Fenced,
            )
            | None => (scope.generation != command.expected_generation).then_some(
                HostGrantError::StaleGeneration {
                    current: scope.generation,
                },
            ),
        };
        if let Some(refusal) = refusal {
            tx.rollback().await?;
            return Err(refusal);
        }
        // The retirement stamp on the lifecycle ledger is what makes this an
        // owner-phase operation: the runtime role cannot update the ledger,
        // so PostgreSQL refuses the statement on a runtime handle before the
        // scope's state is touched.
        let stamped = tx
            .execute(
                "UPDATE host_grant_scope_lifecycles SET retired_at_ms = ? \
                 WHERE lifecycle_id = ? AND retired_at_ms IS NULL",
                crate::db_params![now, scope.lifecycle.to_storage()],
            )
            .await
            .map_err(HostGrantError::from_database)?;
        if stamped != 1 {
            return Err(corrupt("lifecycle retirement"));
        }
        let Some(generation) = scope.generation.next() else {
            return self
                .exhaust(tx, &scope, command.operation, &fingerprint, mode)
                .await;
        };
        let digest = current_digest(&mut tx, &scope).await?;
        let updated = tx
            .execute(
                "UPDATE host_grant_scopes SET generation = ?, operation_id = ?, \
                 admission_state = 'closed', closed_reason = 'retired', updated_at_ms = ? \
                 WHERE lifecycle_id = ? AND generation = ?",
                crate::db_params![
                    generation.value(),
                    operation.as_str(),
                    now,
                    scope.lifecycle.to_storage(),
                    scope.generation.value()
                ],
            )
            .await
            .map_err(HostGrantError::from_database)?;
        if updated != 1 {
            return Err(corrupt("scope generation"));
        }
        let record = OperationRecord {
            operation: command.operation,
            lifecycle: command.lifecycle,
            generation,
            kind: OperationKind::Retire,
            authority_incarnation: scope.authority_incarnation,
            projection_digest: digest,
        };
        insert_operation(&mut tx, &record, &fingerprint, None, now).await?;
        commit(tx, mode).await?;
        Ok(MutationReceipt::Recorded(record))
    }

    /// Close a scope whose next operation has no working generation left.
    /// The scope moves to the terminal generation, closed, and the command
    /// that met the limit is recorded as the operation that exhausted it;
    /// the change it asked for is not made. The recorded digest is therefore
    /// that of the grant rows as they stand, whatever the command intended.
    async fn exhaust(
        &self,
        mut tx: EnteredTransaction<'_>,
        scope: &ScopeState,
        operation: GrantOperationId,
        fingerprint: &str,
        mode: CommitMode,
    ) -> Result<MutationReceipt, HostGrantError> {
        let now = Utc::now().timestamp_millis();
        let digest = current_digest(&mut tx, scope).await?;
        let updated = tx
            .execute(
                "UPDATE host_grant_scopes SET generation = ?, operation_id = ?, \
                 admission_state = 'closed', closed_reason = 'exhausted', updated_at_ms = ? \
                 WHERE lifecycle_id = ? AND generation = ?",
                crate::db_params![
                    GrantGeneration::TERMINAL.value(),
                    operation.to_storage(),
                    now,
                    scope.lifecycle.to_storage(),
                    scope.generation.value()
                ],
            )
            .await
            .map_err(HostGrantError::from_database)?;
        if updated != 1 {
            return Err(corrupt("scope generation"));
        }
        let record = OperationRecord {
            operation,
            lifecycle: scope.lifecycle,
            generation: GrantGeneration::TERMINAL,
            kind: OperationKind::Exhaust,
            authority_incarnation: scope.authority_incarnation,
            projection_digest: digest,
        };
        insert_operation(&mut tx, &record, fingerprint, None, now).await?;
        commit(tx, mode).await?;
        Err(HostGrantError::ScopeExhausted)
    }

    async fn activate_owned(
        &self,
        marker: &ProjectionMarker,
    ) -> Result<Activation, HostGrantError> {
        let now = Utc::now().timestamp_millis();
        let mut tx = self.begin_write().await?;
        let Some(scope) = load_scope(&mut tx, marker.lifecycle, true)
            .await
            .map_err(statement_failure)?
        else {
            tx.rollback().await?;
            return Err(HostGrantError::UnknownScope);
        };
        let current = load_incarnation(&mut tx).await.map_err(statement_failure)?;
        if let Err(refusal) = refuse_unusable_scope(&scope, current) {
            tx.rollback().await?;
            return Err(refusal);
        }
        if scope.generation != marker.generation
            || scope.operation != marker.operation
            || scope.authority_incarnation != marker.incarnation
        {
            tx.rollback().await?;
            return Ok(Activation::Superseded);
        }
        if scope.projected_generation == Some(scope.generation) {
            tx.rollback().await?;
            return Ok(Activation::AlreadyActive);
        }
        tx.execute(
            "UPDATE host_grants SET effective_affiliation = affiliation \
             WHERE lifecycle_id = ? AND effective_affiliation <> affiliation",
            crate::db_params![scope.lifecycle.to_storage()],
        )
        .await
        .map_err(HostGrantError::from_database)?;
        // A scope a restore transition closed stays closed once its
        // reconstruction is verified: an operator reopens it.
        let reconstructed = matches!(
            scope.closed_reason,
            Some(ClosedReason::Restored | ClosedReason::Reconstructed)
        );
        let (state, reason) = if reconstructed {
            ("closed", Some(ClosedReason::Reconstructed.as_storage()))
        } else {
            ("active", None)
        };
        let updated = tx
            .execute(
                "UPDATE host_grant_scopes SET projected_generation = generation, \
                 admission_state = ?, closed_reason = ?, updated_at_ms = ? \
                 WHERE lifecycle_id = ? AND generation = ? AND operation_id = ?",
                crate::db_params![
                    state,
                    reason,
                    now,
                    scope.lifecycle.to_storage(),
                    scope.generation.value(),
                    scope.operation.to_storage()
                ],
            )
            .await
            .map_err(HostGrantError::from_database)?;
        if updated != 1 {
            return Err(corrupt("scope generation"));
        }
        commit(tx, CommitMode::Acknowledged).await?;
        Ok(if reconstructed {
            Activation::Reconstructed
        } else {
            Activation::Activated
        })
    }

    async fn prepare_restore_owned(
        &self,
        configured: RestoreEpoch,
        mode: CommitMode,
    ) -> Result<RestoreDeclaration, HostGrantError> {
        let mut tx = self.begin_write().await?;
        // Locking the authority row needs UPDATE on it, which the runtime
        // role does not hold: on a runtime handle PostgreSQL refuses this
        // first statement and nothing changes.
        let mut rows = tx
            .query(
                "SELECT incarnation, restore_epoch, lineage_uuid FROM host_grant_authority \
                 WHERE singleton FOR UPDATE",
                (),
            )
            .await
            .map_err(HostGrantError::from_database)?;
        let row = rows.next().await?.ok_or(corrupt("authority incarnation"))?;
        let previous = AuthorityIncarnation::from_storage(&row.get::<String>(0)?)
            .ok_or(corrupt("authority incarnation"))?;
        let stored =
            RestoreEpoch::from_storage(row.get::<i64>(1)?).ok_or(corrupt("restore_epoch"))?;
        let bound = row.get::<Option<String>>(2)?;
        drop(rows);
        let live = load_live_lineage(&mut tx)
            .await
            .map_err(statement_failure)?;
        if configured < stored {
            tx.rollback().await?;
            return Err(RestoreFenceRefusal::EpochMismatch { configured, stored }.into());
        }
        if configured == stored {
            if bound.is_some() && bound != live {
                // A database that holds no scope has no authority a restore
                // could reopen, and its new lineage is simply bound.
                let holds_scope =
                    query_flag(&mut tx, "SELECT EXISTS (SELECT 1 FROM host_grant_scopes)")
                        .await
                        .map_err(statement_failure)?;
                if holds_scope {
                    tx.rollback().await?;
                    return Err(RestoreFenceRefusal::LineageChanged.into());
                }
            }
            let declaration = match live.filter(|live| bound.as_ref() != Some(live)) {
                Some(live) => {
                    tx.execute(
                        "UPDATE host_grant_authority SET lineage_uuid = ? WHERE singleton",
                        crate::db_params![live],
                    )
                    .await
                    .map_err(HostGrantError::from_database)?;
                    commit(tx, mode).await?;
                    RestoreDeclaration::LineageBound
                }
                None => {
                    tx.rollback().await?;
                    RestoreDeclaration::Unchanged
                }
            };
            // A transition whose first commit landed and whose scopes were
            // not all closed is finished now.
            let resumed = self.close_for_restore(previous, configured).await?;
            return Ok(if resumed.closed > 0 {
                RestoreDeclaration::Resumed {
                    restored_scopes: resumed.restored,
                }
            } else {
                declaration
            });
        }
        let incarnation = AuthorityIncarnation::generate();
        tx.execute(
            "UPDATE host_grant_authority SET incarnation = ?, restore_epoch = ?, \
             lineage_uuid = ? WHERE singleton",
            crate::db_params![incarnation.to_storage(), configured.to_storage(), live],
        )
        .await
        .map_err(HostGrantError::from_database)?;
        // From this commit no scope bound to the earlier incarnation admits,
        // mutates or activates; the batches below close each one.
        commit(tx, mode).await?;
        let closed = self.close_for_restore(incarnation, configured).await?;
        Ok(RestoreDeclaration::Transitioned {
            previous,
            incarnation,
            epoch: configured,
            restored_scopes: closed.restored,
        })
    }

    /// Close every scope that is not retired or exhausted and is bound to
    /// another incarnation than `incarnation`, the current one, in
    /// transactions of at most [`RESTORE_TRANSITION_BATCH`] scopes.
    async fn close_for_restore(
        &self,
        incarnation: AuthorityIncarnation,
        epoch: RestoreEpoch,
    ) -> Result<TransitionCount, HostGrantError> {
        let mut count = TransitionCount::default();
        loop {
            let now = Utc::now().timestamp_millis();
            let mut tx = self.begin_write().await?;
            if load_incarnation(&mut tx).await.map_err(statement_failure)? != incarnation {
                tx.rollback().await?;
                return Err(HostGrantError::IncarnationChanged);
            }
            let batch = i64::try_from(RESTORE_TRANSITION_BATCH).unwrap_or(i64::MAX);
            let mut rows = tx
                .query(
                    &format!(
                        "SELECT {SCOPE_COLUMNS} FROM host_grant_scopes \
                         WHERE authority_incarnation <> ? \
                           AND (closed_reason IS NULL \
                                OR closed_reason NOT IN ('retired', 'exhausted')) \
                         ORDER BY lifecycle_id LIMIT ? FOR UPDATE"
                    ),
                    crate::db_params![incarnation.to_storage(), batch],
                )
                .await
                .map_err(HostGrantError::from_database)?;
            let mut scopes = Vec::new();
            while let Some(row) = rows.next().await? {
                scopes.push(decode_scope(&row)?);
            }
            drop(rows);
            if scopes.is_empty() {
                tx.rollback().await?;
                return Ok(count);
            }
            for scope in &scopes {
                if close_one_for_restore(&mut tx, scope, incarnation, epoch, now).await? {
                    count.restored += 1;
                }
                count.closed += 1;
            }
            commit(tx, CommitMode::Acknowledged).await?;
        }
    }

    async fn fence_owned(
        &self,
        lifecycle: ScopeLifecycleId,
        evidence: &FenceEvidence,
        mode: CommitMode,
    ) -> Result<bool, HostGrantError> {
        let now = Utc::now().timestamp_millis();
        let mut tx = self.begin_write().await?;
        let Some(scope) = load_scope(&mut tx, lifecycle, true)
            .await
            .map_err(statement_failure)?
        else {
            tx.rollback().await?;
            return Ok(false);
        };
        let evidence_holds = async {
            if matches!(
                scope.closed_reason,
                Some(ClosedReason::Retired | ClosedReason::Exhausted | ClosedReason::Fenced)
            ) {
                return Ok(false);
            }
            Ok::<_, HostGrantError>(match evidence {
                FenceEvidence::ProjectionAhead(marker) => {
                    load_recorded_marker(&mut tx, lifecycle, marker.generation)
                        .await?
                        .as_ref()
                        != Some(marker)
                }
                FenceEvidence::UnknownIncarnation(marker) => {
                    !holds_incarnation(&mut tx, lifecycle, marker.incarnation).await?
                }
                FenceEvidence::JournalAhead(head) => {
                    let recorded = load_recorded_marker(&mut tx, lifecycle, head.generation)
                        .await?
                        .as_ref()
                        == Some(head);
                    let restore_handling = scope
                        .closed_reason
                        .is_some_and(ClosedReason::awaits_restore_handling);
                    let superseded = head.incarnation != scope.authority_incarnation
                        && (restore_handling
                            || holds_reopen(&mut tx, lifecycle, scope.authority_incarnation)
                                .await?);
                    !recorded && !superseded
                }
                FenceEvidence::MarkerMissing => scope.admission == AdmissionState::Active,
            })
        }
        .await
        .map_err(statement_failure);
        match evidence_holds {
            Ok(true) => {}
            Ok(false) => {
                tx.rollback().await?;
                return Ok(false);
            }
            Err(error) => {
                tx.rollback().await?;
                return Err(error);
            }
        }
        let operation = GrantOperationId::generate();
        let fingerprint = fence_fingerprint(lifecycle, scope.generation, evidence);
        let Some(generation) = scope.generation.next() else {
            return match self
                .exhaust(tx, &scope, operation, &fingerprint, mode)
                .await
            {
                Err(HostGrantError::ScopeExhausted) => Ok(true),
                Err(error) => Err(error),
                Ok(_) => Ok(true),
            };
        };
        let digest = current_digest(&mut tx, &scope).await?;
        close_scope(
            &mut tx,
            &scope,
            scope.authority_incarnation,
            generation,
            operation,
            ClosedReason::Fenced,
            now,
        )
        .await?;
        lower_effective_standing(&mut tx, lifecycle).await?;
        let record = OperationRecord {
            operation,
            lifecycle,
            generation,
            kind: OperationKind::Fence,
            authority_incarnation: scope.authority_incarnation,
            projection_digest: digest,
        };
        insert_operation(&mut tx, &record, &fingerprint, None, now).await?;
        commit(tx, mode).await?;
        Ok(true)
    }

    async fn reopen_owned(
        &self,
        selection: Option<ScopeLifecycleId>,
        mode: CommitMode,
    ) -> Result<ReopenReport, HostGrantError> {
        let candidates = match selection {
            Some(lifecycle) => vec![lifecycle],
            None => {
                let mut tx = self.begin_read().await?;
                let mut rows = tx
                    .query(
                        "SELECT lifecycle_id FROM host_grant_scopes \
                         WHERE closed_reason IN ('restored', 'reconstructed') \
                         ORDER BY lifecycle_id",
                        (),
                    )
                    .await?;
                let mut candidates = Vec::new();
                while let Some(row) = rows.next().await? {
                    candidates.push(
                        ScopeLifecycleId::from_storage(&row.get::<String>(0)?)
                            .ok_or(corrupt("lifecycle_id"))?,
                    );
                }
                drop(rows);
                tx.rollback().await?;
                candidates
            }
        };
        let mut report = ReopenReport::default();
        for lifecycle in candidates {
            let now = Utc::now().timestamp_millis();
            let mut tx = self.begin_write().await?;
            let Some(scope) = load_scope(&mut tx, lifecycle, true)
                .await
                .map_err(statement_failure)?
            else {
                tx.rollback().await?;
                return Err(HostGrantError::UnknownScope);
            };
            let current = load_incarnation(&mut tx).await.map_err(statement_failure)?;
            let verified = scope.closed_reason == Some(ClosedReason::Reconstructed)
                && scope.projected_generation == Some(scope.generation)
                && scope.authority_incarnation == current
                && scope.policy_version == HOST_GRANT_POLICY_VERSION;
            let Some(generation) = scope.generation.next().filter(|_| verified) else {
                tx.rollback().await?;
                report.not_reconstructed += 1;
                continue;
            };
            let operation = GrantOperationId::generate();
            let digest = current_digest(&mut tx, &scope).await?;
            let updated = tx
                .execute(
                    "UPDATE host_grant_scopes SET generation = ?, operation_id = ?, \
                     admission_state = 'active', closed_reason = NULL, updated_at_ms = ? \
                     WHERE lifecycle_id = ? AND generation = ?",
                    crate::db_params![
                        generation.value(),
                        operation.to_storage(),
                        now,
                        lifecycle.to_storage(),
                        scope.generation.value()
                    ],
                )
                .await
                .map_err(HostGrantError::from_database)?;
            if updated != 1 {
                return Err(corrupt("scope generation"));
            }
            let record = OperationRecord {
                operation,
                lifecycle,
                generation,
                kind: OperationKind::Reopen,
                authority_incarnation: scope.authority_incarnation,
                projection_digest: digest,
            };
            insert_operation(
                &mut tx,
                &record,
                &reopen_fingerprint(lifecycle, scope.generation),
                None,
                now,
            )
            .await?;
            commit(tx, mode).await?;
            report.reopened += 1;
        }
        Ok(report)
    }

    async fn acknowledge_position_owned(
        &self,
        position: PostgresPosition,
        mode: CommitMode,
    ) -> Result<(i64, u32), HostGrantError> {
        let mut tx = self.begin_write().await?;
        let acknowledged = (position.system_identifier, position.timeline);
        tx.execute(
            "UPDATE host_grant_authority SET position_acknowledgement = ? WHERE singleton",
            crate::db_params![format!("{}_{}", acknowledged.0, acknowledged.1)],
        )
        .await
        .map_err(HostGrantError::from_database)?;
        commit(tx, mode).await?;
        Ok(acknowledged)
    }

    async fn operator_revoke_owned(
        &self,
        command: &OperatorRevoke,
        served: &ServedAccountDomains,
        mode: CommitMode,
    ) -> Result<MutationReceipt, HostGrantError> {
        let fingerprint = command.fingerprint();
        let lifecycle = command.lifecycle.to_storage();
        let operation = command.operation.to_storage();
        let now = Utc::now().timestamp_millis();
        let mut tx = self.begin_write().await?;
        let Some(scope) = load_scope(&mut tx, command.lifecycle, true)
            .await
            .map_err(statement_failure)?
        else {
            tx.rollback().await?;
            return Err(HostGrantError::UnknownScope);
        };
        if let Some(recorded) = load_operation(&mut tx, command.operation)
            .await
            .map_err(statement_failure)?
        {
            tx.rollback().await?;
            return recorded.replay(&fingerprint);
        }
        let current = load_incarnation(&mut tx).await.map_err(statement_failure)?;
        let decision = async {
            match scope.closed_reason {
                Some(ClosedReason::Retired) => return Err(HostGrantError::ScopeRetired),
                Some(ClosedReason::Exhausted) => return Err(HostGrantError::ScopeExhausted),
                _ => {}
            }
            if scope.authority_incarnation != current {
                return Err(HostGrantError::IncarnationChanged);
            }
            let rooms = load_rooms(&mut tx, command.lifecycle).await?;
            if !rooms.iter().any(|room| room.room() == &command.room) {
                return Err(HostGrantError::UnknownRoom);
            }
            let grants = load_grants(&mut tx, command.lifecycle).await?;
            let held = grants
                .iter()
                .find(|grant| grant.room == command.room && grant.subject == command.subject);
            // A revoke never raises: a banned subject stays banned.
            let target = held.map_or(command.affiliation(), |grant| {
                grant.affiliation.min(command.affiliation())
            });
            let recorded = held.map_or(RoomAffiliation::None, |grant| grant.affiliation);
            let effective = held.map_or(RoomAffiliation::None, |grant| grant.effective_affiliation);
            // The room keeps a recorded owner of a served account, so that it
            // can be administered once it is reopened.
            if recorded == RoomAffiliation::Owner && target != RoomAffiliation::Owner {
                let other_owner = grants.iter().any(|grant| {
                    grant.room == command.room
                        && grant.subject != command.subject
                        && grant.affiliation == RoomAffiliation::Owner
                        && served.home_of(grant.subject.jid()) == SubjectHome::Served
                });
                if !other_owner {
                    return Err(HostGrantError::LastOwner);
                }
            }
            if held.is_none() && grants.len() >= HOST_GRANT_SCOPE_MAX_GRANT_ROWS {
                return Err(HostGrantError::GrantLimit);
            }
            Ok((target, recorded, effective))
        }
        .await;
        let (target, recorded, effective) = match decision {
            Ok(decision) => decision,
            Err(error) => {
                tx.rollback().await?;
                return Err(statement_failure(error));
            }
        };
        let Some(generation) = scope.generation.next() else {
            return self
                .exhaust(tx, &scope, command.operation, &fingerprint, mode)
                .await;
        };
        let basis = match served.home_of(command.subject.jid()) {
            SubjectHome::Served => IssuanceBasis::Local,
            SubjectHome::Remote => IssuanceBasis::Unprivileged,
        };
        tx.execute(
            "INSERT INTO host_grants (lifecycle_id, room_jid, subject_jid, affiliation, \
             effective_affiliation, changed_generation, operation_id, issuance_basis) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (lifecycle_id, room_jid, subject_jid) DO UPDATE SET \
             affiliation = EXCLUDED.affiliation, \
             effective_affiliation = EXCLUDED.effective_affiliation, \
             changed_generation = EXCLUDED.changed_generation, \
             operation_id = EXCLUDED.operation_id, \
             issuance_basis = EXCLUDED.issuance_basis",
            crate::db_params![
                lifecycle.as_str(),
                command.room.as_str(),
                command.subject.as_str(),
                target.as_storage(),
                effective.min(target).as_storage(),
                generation.value(),
                operation.as_str(),
                basis.as_storage()
            ],
        )
        .await
        .map_err(HostGrantError::from_database)?;
        advance_scope(&mut tx, &scope, generation, &operation, now).await?;
        insert_change(
            &mut tx,
            RecordedChange {
                operation: &operation,
                ordinal: 0,
                room: &command.room,
                subject: &command.subject,
                affiliation: target,
                previous: recorded,
                basis,
            },
        )
        .await?;
        let digest = current_digest(&mut tx, &scope).await?;
        let record = OperationRecord {
            operation: command.operation,
            lifecycle: command.lifecycle,
            generation,
            kind: OperationKind::SetAffiliation,
            authority_incarnation: scope.authority_incarnation,
            projection_digest: digest,
        };
        insert_operation(&mut tx, &record, &fingerprint, None, now).await?;
        commit(tx, mode).await?;
        Ok(MutationReceipt::Recorded(record))
    }

    async fn reconstruct_owned(
        &self,
        command: &ReconstructScope,
        served: &ServedAccountDomains,
        mode: CommitMode,
    ) -> Result<Option<OperationRecord>, HostGrantError> {
        let fingerprint = command.fingerprint();
        let lifecycle = command.lifecycle.to_storage();
        let operation = command.operation.to_storage();
        let now = Utc::now().timestamp_millis();
        let mut tx = self.begin_write().await?;
        let Some(scope) = load_scope(&mut tx, command.lifecycle, true)
            .await
            .map_err(statement_failure)?
        else {
            tx.rollback().await?;
            return Err(HostGrantError::UnknownScope);
        };
        if let Some(recorded) = load_operation(&mut tx, command.operation)
            .await
            .map_err(statement_failure)?
        {
            tx.rollback().await?;
            return recorded
                .replay(&fingerprint)
                .map(|receipt| Some(receipt.record().clone()));
        }
        let current = load_incarnation(&mut tx).await.map_err(statement_failure)?;
        let decision = async {
            refuse_unusable_scope(&scope, current)?;
            if scope.closed_reason != Some(ClosedReason::Restored) {
                return Err(HostGrantError::ScopeNotRestored);
            }
            if scope.generation != command.expected_generation {
                return Err(HostGrantError::StaleGeneration {
                    current: scope.generation,
                });
            }
            let rooms = load_rooms(&mut tx, command.lifecycle).await?;
            let grants = load_grants(&mut tx, command.lifecycle).await?;
            let mut writes = Vec::new();
            let mut new_rows = 0usize;
            for change in &command.changes {
                let room = rooms
                    .iter()
                    .find(|room| room.channel_id() == change.channel_id)
                    .ok_or(HostGrantError::UnknownRoom)?;
                let held = grants
                    .iter()
                    .find(|grant| &grant.room == room.room() && grant.subject == change.subject);
                let home = served.home_of(change.subject.jid());
                let write = match held {
                    // Only a lowering is recorded for a subject that holds a
                    // row, and only one that lowers what admission honours:
                    // a privileged grant of an account this deployment does
                    // not serve is honoured as none, so it is lowered to
                    // `none` or `outcast`, never to `member`.
                    Some(grant) if change.affiliation < grant.affiliation => {
                        let honoured = |affiliation| honoured_affiliation(affiliation, home);
                        let target = if honoured(change.affiliation) > honoured(grant.affiliation) {
                            change.affiliation.min(RoomAffiliation::None)
                        } else {
                            change.affiliation
                        };
                        ReconstructionWrite {
                            room: room.room().clone(),
                            subject: change.subject.clone(),
                            target,
                            basis: change.basis,
                            recorded: grant.affiliation,
                            effective: grant.effective_affiliation,
                        }
                    }
                    Some(_) => continue,
                    // A subject only the projection held is recorded at
                    // `none` or `outcast`, never above.
                    None if change.affiliation <= RoomAffiliation::None => {
                        new_rows += 1;
                        ReconstructionWrite {
                            room: room.room().clone(),
                            subject: change.subject.clone(),
                            target: change.affiliation,
                            basis: change.basis,
                            recorded: RoomAffiliation::None,
                            effective: RoomAffiliation::None,
                        }
                    }
                    None => continue,
                };
                writes.push(write);
            }
            if grants.len() + new_rows > HOST_GRANT_SCOPE_MAX_GRANT_ROWS {
                return Err(HostGrantError::GrantLimit);
            }
            Ok(writes)
        }
        .await;
        let writes = match decision {
            Ok(writes) if writes.is_empty() => {
                tx.rollback().await?;
                return Ok(None);
            }
            Ok(writes) => writes,
            Err(error) => {
                tx.rollback().await?;
                return Err(statement_failure(error));
            }
        };
        let Some(generation) = scope.generation.next() else {
            return self
                .exhaust(tx, &scope, command.operation, &fingerprint, mode)
                .await
                .map(|_| None);
        };
        for (ordinal, write) in writes.iter().enumerate() {
            // A lowering takes effect with this commit, like any reduction.
            let effective_after = write.effective.min(write.target);
            tx.execute(
                "INSERT INTO host_grants (lifecycle_id, room_jid, subject_jid, affiliation, \
                 effective_affiliation, changed_generation, operation_id, issuance_basis) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
                 ON CONFLICT (lifecycle_id, room_jid, subject_jid) DO UPDATE SET \
                 affiliation = EXCLUDED.affiliation, \
                 effective_affiliation = EXCLUDED.effective_affiliation, \
                 changed_generation = EXCLUDED.changed_generation, \
                 operation_id = EXCLUDED.operation_id, \
                 issuance_basis = EXCLUDED.issuance_basis",
                crate::db_params![
                    lifecycle.as_str(),
                    write.room.as_str(),
                    write.subject.as_str(),
                    write.target.as_storage(),
                    effective_after.as_storage(),
                    generation.value(),
                    operation.as_str(),
                    write.basis.as_storage()
                ],
            )
            .await
            .map_err(HostGrantError::from_database)?;
            insert_change(
                &mut tx,
                RecordedChange {
                    operation: &operation,
                    ordinal,
                    room: &write.room,
                    subject: &write.subject,
                    affiliation: write.target,
                    previous: write.recorded,
                    basis: write.basis,
                },
            )
            .await?;
        }
        advance_scope(&mut tx, &scope, generation, &operation, now).await?;
        let digest = current_digest(&mut tx, &scope).await?;
        let record = OperationRecord {
            operation: command.operation,
            lifecycle: command.lifecycle,
            generation,
            kind: OperationKind::Reconstruct,
            authority_incarnation: scope.authority_incarnation,
            projection_digest: digest,
        };
        insert_operation(&mut tx, &record, &fingerprint, None, now).await?;
        commit(tx, mode).await?;
        Ok(Some(record))
    }
}

/// One grant row a reconstruction writes.
struct ReconstructionWrite {
    room: BareJid,
    subject: GrantSubject,
    target: RoomAffiliation,
    basis: IssuanceBasis,
    recorded: RoomAffiliation,
    effective: RoomAffiliation,
}

/// Scopes a restore transition closed.
#[derive(Clone, Copy, Debug, Default)]
struct TransitionCount {
    /// Every scope bound to the new incarnation.
    closed: usize,
    /// Of those, the scopes closed as `restored`; the rest had no
    /// generation left and were closed as exhausted.
    restored: usize,
}

/// Bind one scope to `incarnation` at its next generation and close it as
/// `restored` under a `restore` operation, or as exhausted when no
/// generation is left. Its effective standing is lowered to `none` at most,
/// so nothing it held before the restore authorizes an initiator while it
/// is closed. Answers whether it was closed as `restored`.
async fn close_one_for_restore(
    tx: &mut Transaction<'_>,
    scope: &ScopeState,
    incarnation: AuthorityIncarnation,
    epoch: RestoreEpoch,
    now: i64,
) -> Result<bool, HostGrantError> {
    let operation = GrantOperationId::generate();
    let digest = current_digest(tx, scope).await?;
    let (generation, kind, reason) = match scope.generation.next() {
        Some(generation) => (generation, OperationKind::Restore, ClosedReason::Restored),
        None => (
            GrantGeneration::TERMINAL,
            OperationKind::Exhaust,
            ClosedReason::Exhausted,
        ),
    };
    close_scope(tx, scope, incarnation, generation, operation, reason, now).await?;
    lower_effective_standing(tx, scope.lifecycle).await?;
    let record = OperationRecord {
        operation,
        lifecycle: scope.lifecycle,
        generation,
        kind,
        authority_incarnation: incarnation,
        projection_digest: digest,
    };
    insert_operation(
        tx,
        &record,
        &restore_fingerprint(scope.lifecycle, epoch, incarnation),
        None,
        now,
    )
    .await?;
    Ok(kind == OperationKind::Restore)
}

/// Move a scope to `generation` under `operation`, bound to `incarnation`
/// and closed for `reason`, under the guard its caller holds.
async fn close_scope(
    tx: &mut Transaction<'_>,
    scope: &ScopeState,
    incarnation: AuthorityIncarnation,
    generation: GrantGeneration,
    operation: GrantOperationId,
    reason: ClosedReason,
    now: i64,
) -> Result<(), HostGrantError> {
    let updated = tx
        .execute(
            "UPDATE host_grant_scopes SET authority_incarnation = ?, generation = ?, \
             operation_id = ?, admission_state = 'closed', closed_reason = ?, \
             updated_at_ms = ? WHERE lifecycle_id = ? AND generation = ?",
            crate::db_params![
                incarnation.to_storage(),
                generation.value(),
                operation.to_storage(),
                reason.as_storage(),
                now,
                scope.lifecycle.to_storage(),
                scope.generation.value()
            ],
        )
        .await
        .map_err(HostGrantError::from_database)?;
    if updated == 1 {
        Ok(())
    } else {
        Err(corrupt("scope generation"))
    }
}

/// Lower every effective standing of a scope above `none` to `none`. The
/// recorded affiliations are kept, and activation of a verified projection
/// makes them effective again.
async fn lower_effective_standing(
    tx: &mut Transaction<'_>,
    lifecycle: ScopeLifecycleId,
) -> Result<(), HostGrantError> {
    tx.execute(
        "UPDATE host_grants SET effective_affiliation = 'none' \
         WHERE lifecycle_id = ? AND effective_affiliation IN ('owner', 'admin', 'member')",
        crate::db_params![lifecycle.to_storage()],
    )
    .await
    .map_err(HostGrantError::from_database)?;
    Ok(())
}

/// Run a transaction on a task of its own, so that a caller that stops
/// waiting does not abandon it.
///
/// A task that ends without reporting (it panicked, or the runtime stopped
/// it) may have committed before it did, so its outcome is unknown, like an
/// unacknowledged commit: the caller reads the operation back.
async fn own<T: Send + 'static>(
    work: impl Future<Output = Result<T, HostGrantError>> + Send + 'static,
) -> Result<T, HostGrantError> {
    tokio::spawn(work)
        .await
        .map_err(|_| HostGrantError::CommitOutcomeUnknown)?
}

/// End a mutation's transaction. A commit that is not acknowledged is
/// unknown, not rolled back.
async fn commit(tx: EnteredTransaction<'_>, mode: CommitMode) -> Result<(), HostGrantError> {
    match mode {
        CommitMode::Acknowledged => tx
            .commit()
            .await
            .map_err(|_| HostGrantError::CommitOutcomeUnknown),
        #[cfg(feature = "test-support")]
        CommitMode::Fault(HostGrantCommitFault::ReplyLostAfterCommit) => {
            tx.commit().await?;
            Err(HostGrantError::CommitOutcomeUnknown)
        }
        #[cfg(feature = "test-support")]
        CommitMode::Fault(HostGrantCommitFault::LostBeforeCommit) => {
            tx.rollback().await?;
            Err(HostGrantError::CommitOutcomeUnknown)
        }
    }
}

fn corrupt(field: &'static str) -> HostGrantError {
    HostGrantError::CorruptRow { field }
}

/// A read inside a write transaction that failed before anything was
/// written: an expired bound is `Busy`.
fn statement_failure(error: HostGrantError) -> HostGrantError {
    match error {
        HostGrantError::Database(error) => HostGrantError::from_database(error),
        other => other,
    }
}

/// Check an enrollment's shape and every bootstrap grant's issuance.
fn validate_enrollment(
    command: &EnrollScope,
    policy: &IssuancePolicy,
) -> Result<Vec<IssuanceBasis>, HostGrantError> {
    let rooms: BTreeSet<&str> = command
        .rooms
        .iter()
        .map(|room| room.room().as_str())
        .collect();
    let channels: BTreeSet<&str> = command.rooms.iter().map(ManagedRoom::channel_id).collect();
    if command.rooms.is_empty()
        || command.rooms.len() > HOST_GRANT_SCOPE_MAX_ROOMS
        || rooms.len() != command.rooms.len()
        || channels.len() != command.rooms.len()
    {
        return Err(HostGrantError::InvalidEnrollment);
    }
    if command.bootstrap.len() > HOST_GRANT_SCOPE_MAX_GRANT_ROWS {
        return Err(HostGrantError::GrantLimit);
    }
    let mut seen = BTreeSet::new();
    let mut bases = Vec::with_capacity(command.bootstrap.len());
    for grant in &command.bootstrap {
        if !rooms.contains(grant.room.as_str()) {
            return Err(HostGrantError::UnknownRoom);
        }
        if grant.affiliation == RoomAffiliation::None
            || !seen.insert((grant.room.as_str(), grant.subject.as_str()))
        {
            return Err(HostGrantError::InvalidEnrollment);
        }
        bases.push(policy.check(&grant.subject, grant.affiliation)?);
    }
    // Every room starts with an owner who can administer it, one of a
    // served account domain: no later operation can add the first.
    for room in &command.rooms {
        let owned = command.bootstrap.iter().zip(&bases).any(|(grant, basis)| {
            &grant.room == room.room()
                && grant.affiliation == RoomAffiliation::Owner
                && *basis == IssuanceBasis::Local
        });
        if !owned {
            return Err(HostGrantError::LastOwner);
        }
    }
    Ok(bases)
}

/// Whether a scope admits anything at all.
fn scope_admits(scope: &ScopeState, current: AuthorityIncarnation) -> bool {
    scope.admission == AdmissionState::Active
        && scope.authority_incarnation == current
        && scope.policy_version == HOST_GRANT_POLICY_VERSION
}

/// Refuse a scope no operation may change and no projection may serve.
///
/// A scope a restore transition closed stays usable: its projection is
/// rebuilt and activated by reconciliation, and a mutation is recorded,
/// though no initiator's standing is honoured until it is reopened. A
/// fenced scope is neither changed nor projected until a restore is
/// declared; only an operator's revoke is recorded in it.
fn refuse_unusable_scope(
    scope: &ScopeState,
    current: AuthorityIncarnation,
) -> Result<(), HostGrantError> {
    match scope.closed_reason {
        Some(ClosedReason::Retired) => return Err(HostGrantError::ScopeRetired),
        Some(ClosedReason::Exhausted) => return Err(HostGrantError::ScopeExhausted),
        Some(ClosedReason::Fenced) => return Err(HostGrantError::ScopeFenced),
        Some(ClosedReason::Enrolled | ClosedReason::Restored | ClosedReason::Reconstructed)
        | None => {}
    }
    if scope.authority_incarnation != current {
        return Err(HostGrantError::IncarnationChanged);
    }
    if scope.policy_version != HOST_GRANT_POLICY_VERSION {
        return Err(HostGrantError::PolicyVersionUnsupported);
    }
    Ok(())
}

/// One boolean a statement answers.
async fn query_flag(tx: &mut Transaction<'_>, sql: &str) -> Result<bool, HostGrantError> {
    let mut rows = tx.query(sql, ()).await?;
    let flag = match rows.next().await? {
        Some(row) => row.get::<bool>(0)?,
        None => false,
    };
    drop(rows);
    Ok(flag)
}

/// The database's own lineage UUID, as `_lineage` spells it, if the
/// database records one.
async fn load_live_lineage(tx: &mut Transaction<'_>) -> Result<Option<String>, HostGrantError> {
    if !query_flag(tx, "SELECT pg_catalog.to_regclass('_lineage') IS NOT NULL").await? {
        return Ok(None);
    }
    let mut rows = tx
        .query("SELECT lineage_uuid FROM _lineage WHERE id = 1", ())
        .await?;
    let lineage = match rows.next().await? {
        Some(row) => Some(row.get::<String>(0)?),
        None => None,
    };
    drop(rows);
    Ok(lineage)
}

/// The statement that reads an [`AuthorityBinding`] from the authority row
/// `authority` names, in the column order [`decode_binding`] reads.
pub(crate) fn binding_query(authority: &str) -> String {
    format!(
        "SELECT incarnation, restore_epoch, \
         (extract(epoch FROM pg_catalog.pg_postmaster_start_time()) * 1000000)::bigint, \
         (SELECT d.oid::bigint FROM pg_catalog.pg_database d \
          WHERE d.datname = pg_catalog.current_database()), \
         pg_catalog.current_database()::text, tableoid::bigint \
         FROM {authority} WHERE singleton"
    )
}

/// The binding a row of [`binding_query`] holds; `None` when a stored value
/// is not valid.
pub(crate) fn decode_binding(row: &Row) -> Result<Option<AuthorityBinding>, DatabaseError> {
    let incarnation = AuthorityIncarnation::from_storage(&row.get::<String>(0)?);
    let restore_epoch = RestoreEpoch::from_storage(row.get::<i64>(1)?);
    let (Some(incarnation), Some(restore_epoch)) = (incarnation, restore_epoch) else {
        return Ok(None);
    };
    Ok(Some(AuthorityBinding {
        incarnation,
        restore_epoch,
        server_started_at_us: row.get::<i64>(2)?,
        database_oid: row.get::<i64>(3)?,
        database_name: row.get::<String>(4)?,
        authority_relation_oid: row.get::<i64>(5)?,
    }))
}

async fn load_binding(
    tx: &mut Transaction<'_>,
    authority: &str,
) -> Result<AuthorityBinding, HostGrantError> {
    let mut rows = tx.query(&binding_query(authority), ()).await?;
    let row = rows.next().await?.ok_or(corrupt("authority incarnation"))?;
    let binding = decode_binding(&row)?.ok_or(corrupt("authority incarnation"))?;
    drop(rows);
    Ok(binding)
}

/// `<system identifier>_<timeline>`, as `position_acknowledgement` holds it.
fn parse_acknowledgement(text: &str) -> Option<(i64, u32)> {
    let (system, timeline) = text.split_once('_')?;
    let parsed = (system.parse::<i64>().ok()?, timeline.parse::<u32>().ok()?);
    (format!("{}_{}", parsed.0, parsed.1) == text).then_some(parsed)
}

/// The marker the journal records for one generation of a lifecycle.
async fn load_recorded_marker(
    tx: &mut Transaction<'_>,
    lifecycle: ScopeLifecycleId,
    generation: GrantGeneration,
) -> Result<Option<ProjectionMarker>, HostGrantError> {
    let mut rows = tx
        .query(
            &format!(
                "SELECT {OPERATION_COLUMNS} FROM host_grant_operations \
                 WHERE lifecycle_id = ? AND generation = ?"
            ),
            crate::db_params![lifecycle.to_storage(), generation.value()],
        )
        .await?;
    let recorded = match rows.next().await? {
        Some(row) => Some(decode_operation(&row)?),
        None => None,
    };
    drop(rows);
    Ok(recorded.map(|recorded| ProjectionMarker {
        lifecycle,
        incarnation: recorded.record.authority_incarnation,
        generation: recorded.record.generation,
        operation: recorded.record.operation,
        digest: recorded.record.projection_digest,
    }))
}

async fn holds_incarnation(
    tx: &mut Transaction<'_>,
    lifecycle: ScopeLifecycleId,
    incarnation: AuthorityIncarnation,
) -> Result<bool, HostGrantError> {
    let mut rows = tx
        .query(
            "SELECT EXISTS (SELECT 1 FROM host_grant_operations \
             WHERE lifecycle_id = ? AND authority_incarnation = ?)",
            crate::db_params![lifecycle.to_storage(), incarnation.to_storage()],
        )
        .await?;
    let held = match rows.next().await? {
        Some(row) => row.get::<bool>(0)?,
        None => false,
    };
    drop(rows);
    Ok(held)
}

async fn holds_reopen(
    tx: &mut Transaction<'_>,
    lifecycle: ScopeLifecycleId,
    incarnation: AuthorityIncarnation,
) -> Result<bool, HostGrantError> {
    let mut rows = tx
        .query(
            "SELECT EXISTS (SELECT 1 FROM host_grant_operations \
             WHERE lifecycle_id = ? AND authority_incarnation = ? AND kind = 'reopen')",
            crate::db_params![lifecycle.to_storage(), incarnation.to_storage()],
        )
        .await?;
    let held = match rows.next().await? {
        Some(row) => row.get::<bool>(0)?,
        None => false,
    };
    drop(rows);
    Ok(held)
}

async fn load_incarnation(
    tx: &mut Transaction<'_>,
) -> Result<AuthorityIncarnation, HostGrantError> {
    let mut rows = tx
        .query(
            "SELECT incarnation FROM host_grant_authority WHERE singleton",
            (),
        )
        .await?;
    let row = rows.next().await?.ok_or(corrupt("authority incarnation"))?;
    AuthorityIncarnation::from_storage(&row.get::<String>(0)?)
        .ok_or(corrupt("authority incarnation"))
}

async fn load_scope(
    tx: &mut Transaction<'_>,
    lifecycle: ScopeLifecycleId,
    lock: bool,
) -> Result<Option<ScopeState>, HostGrantError> {
    let suffix = if lock { " FOR UPDATE" } else { "" };
    let mut rows = tx
        .query(
            &format!(
                "SELECT {SCOPE_COLUMNS} FROM host_grant_scopes WHERE lifecycle_id = ?{suffix}"
            ),
            crate::db_params![lifecycle.to_storage()],
        )
        .await?;
    rows.next().await?.as_ref().map(decode_scope).transpose()
}

fn decode_scope(row: &Row) -> Result<ScopeState, HostGrantError> {
    let generation = GrantGeneration::new(row.get::<i64>(3)?).ok_or(corrupt("generation"))?;
    let projected = row.get::<i64>(5)?;
    let projected_generation = match projected {
        0 => None,
        value => Some(GrantGeneration::new(value).ok_or(corrupt("projected_generation"))?),
    };
    let admission = match row.get::<String>(6)?.as_str() {
        "active" => AdmissionState::Active,
        "closed" => AdmissionState::Closed,
        _ => return Err(corrupt("admission_state")),
    };
    let closed_reason = row
        .get::<Option<String>>(7)?
        .map(|reason| ClosedReason::from_storage(&reason).ok_or(corrupt("closed_reason")))
        .transpose()?;
    if (admission == AdmissionState::Active) != closed_reason.is_none() {
        return Err(corrupt("closed_reason"));
    }
    Ok(ScopeState {
        lifecycle: ScopeLifecycleId::from_storage(&row.get::<String>(0)?)
            .ok_or(corrupt("lifecycle_id"))?,
        authority_incarnation: AuthorityIncarnation::from_storage(&row.get::<String>(1)?)
            .ok_or(corrupt("authority_incarnation"))?,
        policy_version: u32::try_from(row.get::<i64>(2)?).map_err(|_| corrupt("policy_version"))?,
        generation,
        operation: GrantOperationId::from_storage(&row.get::<String>(4)?)
            .ok_or(corrupt("operation_id"))?,
        projected_generation,
        admission,
        closed_reason,
    })
}

async fn load_rooms(
    tx: &mut Transaction<'_>,
    lifecycle: ScopeLifecycleId,
) -> Result<Vec<ManagedRoom>, HostGrantError> {
    let mut rows = tx
        .query(
            "SELECT room_jid, channel_id FROM host_grant_rooms WHERE lifecycle_id = ? \
             ORDER BY room_jid",
            crate::db_params![lifecycle.to_storage()],
        )
        .await?;
    let mut rooms = Vec::new();
    while let Some(row) = rows.next().await? {
        rooms.push(
            ManagedRoom::from_storage(&row.get::<String>(0)?, &row.get::<String>(1)?)
                .ok_or(corrupt("room"))?,
        );
    }
    Ok(rooms)
}

async fn load_grants(
    tx: &mut Transaction<'_>,
    lifecycle: ScopeLifecycleId,
) -> Result<Vec<GrantRow>, HostGrantError> {
    let mut rows = tx
        .query(
            &format!(
                "SELECT {GRANT_COLUMNS} FROM host_grants WHERE lifecycle_id = ? \
                 ORDER BY room_jid, subject_jid"
            ),
            crate::db_params![lifecycle.to_storage()],
        )
        .await?;
    let mut grants = Vec::new();
    while let Some(row) = rows.next().await? {
        grants.push(GrantRow {
            room: row
                .get::<String>(0)?
                .parse()
                .map_err(|_| corrupt("room_jid"))?,
            subject: GrantSubject::from_storage(&row.get::<String>(1)?)
                .ok_or(corrupt("subject_jid"))?,
            affiliation: affiliation_column(&row, 2, "affiliation")?,
            effective_affiliation: affiliation_column(&row, 3, "effective_affiliation")?,
            changed_generation: GrantGeneration::new(row.get::<i64>(4)?)
                .ok_or(corrupt("changed_generation"))?,
            operation: GrantOperationId::from_storage(&row.get::<String>(5)?)
                .ok_or(corrupt("operation_id"))?,
            issuance_basis: IssuanceBasis::from_storage(&row.get::<String>(6)?)
                .ok_or(corrupt("issuance_basis"))?,
        });
    }
    Ok(grants)
}

fn affiliation_column(
    row: &Row,
    index: usize,
    field: &'static str,
) -> Result<RoomAffiliation, HostGrantError> {
    RoomAffiliation::from_storage(&row.get::<String>(index)?).ok_or(corrupt(field))
}

/// The grant rows as the projection states them, with each room's
/// authorization object.
fn projected_grants(
    rooms: &[ManagedRoom],
    grants: &[GrantRow],
) -> Result<Vec<ProjectedGrant>, HostGrantError> {
    grants
        .iter()
        .map(|grant| {
            let room = rooms
                .iter()
                .find(|room| room.room() == &grant.room)
                .ok_or(corrupt("grant room"))?;
            Ok(ProjectedGrant {
                channel_id: room.channel_id().to_owned(),
                subject: grant.subject.clone(),
                affiliation: grant.affiliation,
            })
        })
        .collect()
}

/// The digest of a scope's present canonical state.
async fn current_digest(
    tx: &mut Transaction<'_>,
    scope: &ScopeState,
) -> Result<ProjectionDigest, HostGrantError> {
    let rooms = load_rooms(tx, scope.lifecycle)
        .await
        .map_err(statement_failure)?;
    let grants = load_grants(tx, scope.lifecycle)
        .await
        .map_err(statement_failure)?;
    let projected = projected_grants(&rooms, &grants)?;
    projection_digest(scope.policy_version, scope.lifecycle, &projected)
        .ok_or(corrupt("projection digest"))
}

/// An operation as the journal holds it.
struct RecordedOperation {
    record: OperationRecord,
    fingerprint: String,
}

impl RecordedOperation {
    /// The original outcome for an exact retry, or a conflict when the
    /// identity was recorded for other input. Nothing is authorized again.
    fn replay(self, fingerprint: &str) -> Result<MutationReceipt, HostGrantError> {
        if self.fingerprint != fingerprint {
            return Err(HostGrantError::OperationConflict);
        }
        match self.record.kind {
            OperationKind::Exhaust => Err(HostGrantError::ScopeExhausted),
            OperationKind::Enroll
            | OperationKind::SetAffiliation
            | OperationKind::Retire
            | OperationKind::Restore
            | OperationKind::Reconstruct
            | OperationKind::Fence
            | OperationKind::Reopen => Ok(MutationReceipt::Replayed(self.record)),
        }
    }
}

async fn load_operation(
    tx: &mut Transaction<'_>,
    operation: GrantOperationId,
) -> Result<Option<RecordedOperation>, HostGrantError> {
    let mut rows = tx
        .query(
            &format!(
                "SELECT {OPERATION_COLUMNS} FROM host_grant_operations WHERE operation_id = ?"
            ),
            crate::db_params![operation.to_storage()],
        )
        .await?;
    rows.next()
        .await?
        .as_ref()
        .map(decode_operation)
        .transpose()
}

fn decode_operation(row: &Row) -> Result<RecordedOperation, HostGrantError> {
    Ok(RecordedOperation {
        record: OperationRecord {
            operation: GrantOperationId::from_storage(&row.get::<String>(0)?)
                .ok_or(corrupt("operation_id"))?,
            lifecycle: ScopeLifecycleId::from_storage(&row.get::<String>(1)?)
                .ok_or(corrupt("lifecycle_id"))?,
            generation: GrantGeneration::new(row.get::<i64>(2)?).ok_or(corrupt("generation"))?,
            kind: OperationKind::from_storage(&row.get::<String>(4)?).ok_or(corrupt("kind"))?,
            authority_incarnation: AuthorityIncarnation::from_storage(&row.get::<String>(5)?)
                .ok_or(corrupt("authority_incarnation"))?,
            projection_digest: ProjectionDigest::from_storage(&row.get::<String>(6)?)
                .ok_or(corrupt("projection_digest"))?,
        },
        fingerprint: row.get::<String>(3)?,
    })
}

async fn insert_operation(
    tx: &mut Transaction<'_>,
    record: &OperationRecord,
    fingerprint: &str,
    initiator: Option<&str>,
    now: i64,
) -> Result<(), HostGrantError> {
    tx.execute(
        "INSERT INTO host_grant_operations (operation_id, lifecycle_id, generation, fingerprint, \
         kind, initiator_jid, authority_incarnation, projection_digest, recorded_at_ms) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        crate::db_params![
            record.operation.to_storage(),
            record.lifecycle.to_storage(),
            record.generation.value(),
            fingerprint,
            record.kind.as_storage(),
            initiator,
            record.authority_incarnation.to_storage(),
            record.projection_digest.as_str(),
            now
        ],
    )
    .await
    .map_err(HostGrantError::from_database)?;
    Ok(())
}

/// One grant change of an operation, as the journal records it.
struct RecordedChange<'a> {
    operation: &'a str,
    ordinal: usize,
    room: &'a BareJid,
    subject: &'a GrantSubject,
    affiliation: RoomAffiliation,
    previous: RoomAffiliation,
    basis: IssuanceBasis,
}

async fn insert_change(
    tx: &mut Transaction<'_>,
    change: RecordedChange<'_>,
) -> Result<(), HostGrantError> {
    tx.execute(
        "INSERT INTO host_grant_operation_changes (operation_id, ordinal, room_jid, subject_jid, \
         affiliation, previous_affiliation, issuance_basis) VALUES (?, ?, ?, ?, ?, ?, ?)",
        crate::db_params![
            change.operation,
            i64::try_from(change.ordinal).map_err(|_| corrupt("change ordinal"))?,
            change.room.as_str(),
            change.subject.as_str(),
            change.affiliation.as_storage(),
            change.previous.as_storage(),
            change.basis.as_storage()
        ],
    )
    .await
    .map_err(HostGrantError::from_database)?;
    Ok(())
}

/// Move a scope to its next generation under the guard its caller holds.
async fn advance_scope(
    tx: &mut Transaction<'_>,
    scope: &ScopeState,
    generation: GrantGeneration,
    operation: &str,
    now: i64,
) -> Result<(), HostGrantError> {
    let updated = tx
        .execute(
            "UPDATE host_grant_scopes SET generation = ?, operation_id = ?, updated_at_ms = ? \
             WHERE lifecycle_id = ? AND generation = ?",
            crate::db_params![
                generation.value(),
                operation,
                now,
                scope.lifecycle.to_storage(),
                scope.generation.value()
            ],
        )
        .await
        .map_err(HostGrantError::from_database)?;
    if updated == 1 {
        Ok(())
    } else {
        Err(corrupt("scope generation"))
    }
}
