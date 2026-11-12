//! The typed transaction entry for ingress correctness writes.
//!
//! Every ingress transaction that attests its physical PostgreSQL connection
//! starts in [`begin_ingress_entry`]. The entry acquires the transaction,
//! fixes its isolation, installs the caller's typed wait bounds, takes the
//! epoch relation prelock, runs the physical attestor on that same
//! connection, proves the ingress protocol epoch, and refuses an epoch this
//! binary does not implement. Only then does it hand back an
//! [`IngressEntryTransaction`].
//!
//! Two types make the entry unavoidable rather than conventional:
//!
//! - [`PreparedAttestedReceiptWriter`] is the witness ingress repositories
//!   need to address schema-qualified relations on an attested pool. Its
//!   fields are private to this module and nothing else constructs it.
//! - [`IngressEntryTransaction`] owns the transaction together with that
//!   witness. The ingress unit of work stores this value, so a unit-of-work
//!   transaction cannot be assembled around a transaction that skipped the
//!   entry.
//!
//! SQLite and PostgreSQL pools without physical attestation use the same
//! entry with [`IngressEntryAuthority::Unattested`]: there is no attestor to
//! run, so the entry takes the V1009 epoch row lock first and, on
//! PostgreSQL, installs the epoch proof settings the guard triggers read.
//!
//! The entry runs on the serving pool, whose attestation covers the physical
//! connection identity and lineage. It does not attest the migration ledger
//! or an epoch manifest per transaction, and the pool type still exposes raw
//! `begin`/`execute`/`query` to other subsystems.

use std::time::Duration;

use waddle_xmpp::ingress::ProtocolEpoch;

use super::substrate::discard_database_error;
use super::{
    acquire_epoch_lock_first, quote_postgres_identifier, set_local_transaction_timeouts,
    supported_protocol_epoch, tighten_local_transaction_timeouts, IngressSubstrateError,
};
use crate::db::physical_attestation::AttestedPostgresWriterCapability;
use crate::db::{Database, DatabaseDriver, DatabaseError, Transaction};
use crate::ingress_uow::{DbRetryClass, RelationGuardCapability};
use waddle_xmpp::telemetry::attributes::{IngressEntryAuthorityKind, IngressEntryOutcome};
use waddle_xmpp::telemetry::authority::record_ingress_entry;

/// How the entry obtains its transaction from the admission lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IngressEntryAcquisition {
    /// The driver's write-intent begin on both backends.
    Immediate,
    /// SQLite takes its write lock up front; PostgreSQL uses a plain begin.
    WriterDefault,
    /// A plain begin on both backends, for transactions that only read.
    Deferred,
}

/// The wait-bound clamp an attested transaction installs before its first
/// relation lock. A clamp never loosens a tighter bound already in effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IngressEntryClamp {
    /// At most 100 ms of lock wait and 250 ms per statement.
    Writer,
    /// At most 3 s of lock wait and 5 s per statement, for the candidate
    /// relation guard whose preparation inspects catalog state.
    RelationGuard,
    /// No clamp: the request's explicit timeouts are the transaction's
    /// bounds, exactly as given. For maintenance writers that budget their
    /// own waits; the entry refuses the request if it carries no explicit
    /// timeouts.
    CallerBounds,
}

/// The authority under which the entry proves the protocol epoch.
#[derive(Clone, Copy)]
pub(crate) enum IngressEntryAuthority<'c> {
    /// SQLite, or a PostgreSQL pool without physical attestation.
    Unattested,
    /// An attested pool using the registered V1009 row-lock guard.
    InstalledGuard(&'c AttestedPostgresWriterCapability),
    /// An attested pool using the verifier-issued candidate relation guard.
    RelationGuard(&'c RelationGuardCapability),
}

/// Everything in which ingress transaction openers differ.
#[derive(Clone, Copy)]
pub(crate) struct IngressEntryRequest<'c> {
    pub(crate) authority: IngressEntryAuthority<'c>,
    pub(crate) acquisition: IngressEntryAcquisition,
    /// Deadline for acquiring the transaction, including pool checkout.
    pub(crate) acquisition_deadline: Option<Duration>,
    /// Caller-chosen `(lock, statement)` timeouts installed before the clamp.
    pub(crate) explicit_timeouts: Option<(Duration, Duration)>,
    /// Applied only under an attested authority.
    pub(crate) clamp: IngressEntryClamp,
}

/// Why the entry refused to hand out a transaction. Database and substrate
/// failures stay unconverted so each caller keeps its own classification.
#[derive(Debug)]
pub(crate) enum IngressEntryError {
    AcquisitionTimeout,
    Database(DatabaseError),
    BoundsUnproven,
    /// A database failure while the registered guard prepared the
    /// transaction, with its driver diagnostics, so a caller that
    /// distinguishes lock from statement timeouts still can.
    PreparationDatabase(DatabaseError),
    Preparation(IngressSubstrateError),
    EpochUnsupported {
        live: ProtocolEpoch,
        supported: ProtocolEpoch,
    },
    EpochProofMissing,
}

impl From<DatabaseError> for IngressEntryError {
    fn from(error: DatabaseError) -> Self {
        Self::Database(error)
    }
}

impl IngressSubstrateError {
    /// A typed refusal raised while the entry prepared a transaction.
    fn into_preparation(self) -> IngressEntryError {
        IngressEntryError::Preparation(self)
    }
}

impl IngressEntryError {
    /// The substrate's classification of an entry refusal: database errors
    /// lose their diagnostics, and an unproven bound or missing proof row is
    /// a non-retryable database failure. This is a method, not a `From`
    /// impl, so `?` on substrate results elsewhere keeps one inferable
    /// conversion.
    pub(crate) fn into_substrate_error(self) -> IngressSubstrateError {
        match self {
            Self::AcquisitionTimeout => IngressSubstrateError::Timeout,
            Self::Database(error) | Self::PreparationDatabase(error) => {
                discard_database_error(error)
            }
            Self::BoundsUnproven | Self::EpochProofMissing => IngressSubstrateError::Database {
                retry_class: DbRetryClass::NotRetryable,
            },
            Self::Preparation(error) => error,
            Self::EpochUnsupported { .. } => IngressSubstrateError::UnsupportedLiveEpoch,
        }
    }
}

/// Evidence that the transaction was prepared on the attested PostgreSQL
/// connection before application locks or writes. Only this module's entry
/// constructs it, and it stays inside the [`IngressEntryTransaction`] that
/// owns the prepared transaction.
pub(crate) struct PreparedAttestedReceiptWriter {
    protocol_epoch: ProtocolEpoch,
    schema: String,
    relation_guard: bool,
    definer_api_candidate: bool,
}

impl PreparedAttestedReceiptWriter {
    pub(crate) fn protocol_epoch(&self) -> ProtocolEpoch {
        self.protocol_epoch
    }

    pub(crate) fn schema(&self) -> &str {
        &self.schema
    }

    pub(crate) fn uses_relation_guard(&self) -> bool {
        self.relation_guard
    }

    pub(crate) fn uses_definer_api_candidate(&self) -> bool {
        self.definer_api_candidate
    }

    #[cfg(feature = "test-support")]
    pub(crate) fn enable_definer_api_candidate_for_test(&mut self) {
        self.definer_api_candidate = true;
    }
}

/// A transaction that came through [`begin_ingress_entry`], with the epoch
/// it proved and, on an attested pool, the prepared-writer witness.
///
/// Dropping the value without [`Self::commit`] rolls the transaction back.
pub(crate) struct IngressEntryTransaction<'a> {
    transaction: EnteredTransaction<'a>,
    protocol_epoch: ProtocolEpoch,
    prepared: Option<PreparedAttestedReceiptWriter>,
}

impl<'a> IngressEntryTransaction<'a> {
    /// The protocol epoch proven while only the epoch lock was held.
    pub(crate) fn protocol_epoch(&self) -> ProtocolEpoch {
        self.protocol_epoch
    }

    pub(crate) fn prepared(&self) -> Option<&PreparedAttestedReceiptWriter> {
        self.prepared.as_ref()
    }

    #[cfg(feature = "test-support")]
    pub(crate) fn prepared_mut(&mut self) -> Option<&mut PreparedAttestedReceiptWriter> {
        self.prepared.as_mut()
    }

    pub(crate) fn transaction_mut(&mut self) -> &mut Transaction<'a> {
        &mut self.transaction
    }

    /// The transaction as the typed handle raw descendant repositories
    /// require, together with its witness.
    pub(crate) fn entered_parts_mut(
        &mut self,
    ) -> (
        &mut EnteredTransaction<'a>,
        Option<&PreparedAttestedReceiptWriter>,
    ) {
        (&mut self.transaction, self.prepared.as_ref())
    }

    /// The transaction together with its witness, for repositories that
    /// choose schema-qualified statements from the prepared identity.
    pub(crate) fn parts_mut(
        &mut self,
    ) -> (&mut Transaction<'a>, Option<&PreparedAttestedReceiptWriter>) {
        (&mut self.transaction.transaction, self.prepared.as_ref())
    }

    /// Release the transaction to a caller that owns its remaining lifetime
    /// itself. The witness travels with it, never without it.
    pub(crate) fn into_parts(
        self,
    ) -> (
        EnteredTransaction<'a>,
        Option<PreparedAttestedReceiptWriter>,
    ) {
        (self.transaction, self.prepared)
    }

    /// Commit. An error here does not establish that the transaction rolled
    /// back; callers keep their own policy for an unacknowledged commit.
    pub(crate) async fn commit(self) -> Result<(), DatabaseError> {
        self.transaction.commit().await
    }
}

/// Begin an ingress correctness transaction.
///
/// On PostgreSQL the statements run in a fixed order: transaction begin,
/// `READ COMMITTED` isolation, the request's explicit timeouts, the attested
/// clamp, then the authority's epoch preparation. Under an attested authority
/// that preparation takes the epoch relation prelock before the physical
/// attestor runs, so no application lock or write can precede either.
pub(crate) async fn begin_ingress_entry<'a>(
    db: &'a Database,
    request: IngressEntryRequest<'_>,
) -> Result<IngressEntryTransaction<'a>, IngressEntryError> {
    let authority = match request.authority {
        IngressEntryAuthority::Unattested => IngressEntryAuthorityKind::Unattested,
        IngressEntryAuthority::InstalledGuard(_) => IngressEntryAuthorityKind::InstalledGuard,
        IngressEntryAuthority::RelationGuard(_) => IngressEntryAuthorityKind::RelationGuard,
    };
    let requested = std::time::Instant::now();
    let entered = enter_ingress_transaction(db, request).await;
    let outcome = match &entered {
        Ok(_) => IngressEntryOutcome::Entered,
        Err(IngressEntryError::AcquisitionTimeout) => IngressEntryOutcome::AcquisitionTimeout,
        Err(IngressEntryError::Database(_)) => IngressEntryOutcome::Failed,
        Err(IngressEntryError::Preparation(IngressSubstrateError::Timeout)) => {
            IngressEntryOutcome::PreparationTimeout
        }
        Err(IngressEntryError::PreparationDatabase(_) | IngressEntryError::Preparation(_)) => {
            IngressEntryOutcome::PreparationFailed
        }
        Err(IngressEntryError::EpochUnsupported { .. }) => IngressEntryOutcome::EpochUnsupported,
        Err(IngressEntryError::BoundsUnproven | IngressEntryError::EpochProofMissing) => {
            IngressEntryOutcome::Refused
        }
    };
    record_ingress_entry(authority, outcome, requested.elapsed());
    entered
}

/// The entry itself; [`begin_ingress_entry`] records its outcome.
async fn enter_ingress_transaction<'a>(
    db: &'a Database,
    request: IngressEntryRequest<'_>,
) -> Result<IngressEntryTransaction<'a>, IngressEntryError> {
    let attested_writer = match request.authority {
        IngressEntryAuthority::Unattested => None,
        IngressEntryAuthority::InstalledGuard(capability) => Some(capability),
        IngressEntryAuthority::RelationGuard(guard) => Some(guard.writer()),
    };
    if let Some(capability) = attested_writer {
        db.verify_attested_pg_writer_capability(capability)
            .map_err(DatabaseError::from)?;
    }

    let acquisition = async {
        match (request.acquisition, db.driver()) {
            (IngressEntryAcquisition::Immediate, _)
            | (IngressEntryAcquisition::WriterDefault, DatabaseDriver::Sqlite) => {
                db.begin_immediate().await
            }
            (IngressEntryAcquisition::Deferred, _)
            | (IngressEntryAcquisition::WriterDefault, DatabaseDriver::Postgres) => {
                db.begin().await
            }
        }
    };
    let mut transaction =
        acquire_within_deadline(request.acquisition_deadline, acquisition).await?;

    if db.driver() == DatabaseDriver::Postgres {
        // Isolation is set before any timeout, epoch, or lineage SELECT can
        // pin the transaction's first snapshot.
        transaction
            .execute("SET TRANSACTION ISOLATION LEVEL READ COMMITTED", ())
            .await?;
    }
    if let Some((lock, statement)) = request.explicit_timeouts {
        if !set_local_transaction_timeouts(&mut transaction, lock, statement).await? {
            return Err(IngressEntryError::BoundsUnproven);
        }
    }
    if attested_writer.is_some() {
        let bounded = match request.clamp {
            IngressEntryClamp::Writer => {
                tighten_local_transaction_timeouts(&mut transaction).await?
            }
            IngressEntryClamp::RelationGuard => {
                bound_relation_guard_transaction(&mut transaction).await?
            }
            IngressEntryClamp::CallerBounds => request.explicit_timeouts.is_some(),
        };
        if !bounded {
            return Err(IngressEntryError::BoundsUnproven);
        }
    }

    let (protocol_epoch, prepared) = match request.authority {
        IngressEntryAuthority::Unattested => {
            let epoch = acquire_epoch_lock_first(&mut transaction)
                .await
                .map_err(IngressEntryError::Preparation)?;
            (epoch, None)
        }
        IngressEntryAuthority::InstalledGuard(capability) => {
            let prepared = prepare_installed_guard(&mut transaction, capability).await?;
            (prepared.protocol_epoch, Some(prepared))
        }
        IngressEntryAuthority::RelationGuard(guard) => {
            let prepared = prepare_relation_guard(&mut transaction, guard).await?;
            (prepared.protocol_epoch, Some(prepared))
        }
    };
    let supported = supported_protocol_epoch();
    if protocol_epoch > supported {
        return Err(IngressEntryError::EpochUnsupported {
            live: protocol_epoch,
            supported,
        });
    }

    if db.driver() == DatabaseDriver::Postgres && prepared.is_none() {
        // The attested preparations install this proof themselves; an
        // unattested PostgreSQL transaction installs it once its epoch row
        // lock is held, so the V1009 guard triggers accept its writes.
        let mut proof = transaction
            .query(
                r#"
                SELECT
                    set_config('waddle.protocol_epoch', ?, true),
                    set_config('waddle.protocol_epoch_xid', pg_current_xact_id()::text, true)
                "#,
                crate::db_params![protocol_epoch.to_storage().to_string()],
            )
            .await?;
        proof
            .next()
            .await?
            .ok_or(IngressEntryError::EpochProofMissing)?;
        drop(proof);
    }

    Ok(IngressEntryTransaction {
        transaction: EnteredTransaction { transaction },
        protocol_epoch,
        prepared,
    })
}

/// A transaction that one of this module's begin functions opened.
///
/// Repositories that lock or write ingress descendants on a caller's own
/// statements take this type instead of a bare transaction. Only
/// [`begin_ingress_entry`] and [`begin_unattested_raw_writer`] construct it,
/// so such a write cannot be issued on a transaction that was opened
/// directly on a database handle. It dereferences to the transaction for the
/// caller's own statements; dropping it without [`Self::commit`] rolls back.
pub(crate) struct EnteredTransaction<'a> {
    transaction: Transaction<'a>,
}

impl<'a> EnteredTransaction<'a> {
    pub(crate) async fn commit(self) -> Result<(), DatabaseError> {
        self.transaction.commit().await
    }

    pub(crate) async fn rollback(self) -> Result<(), DatabaseError> {
        self.transaction.rollback().await
    }

    /// Give up the typed handle. The plain transaction cannot be turned
    /// back into one.
    pub(crate) fn into_transaction(self) -> Transaction<'a> {
        self.transaction
    }

    /// Wrap a transaction a fixture opened itself, for tests that hold a
    /// canonical lock or seed rows from an arbitrary connection.
    #[cfg(test)]
    pub(crate) fn from_fixture_transaction(transaction: Transaction<'a>) -> Self {
        Self { transaction }
    }
}

impl<'a> std::ops::Deref for EnteredTransaction<'a> {
    type Target = Transaction<'a>;

    fn deref(&self) -> &Self::Target {
        &self.transaction
    }
}

impl std::ops::DerefMut for EnteredTransaction<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.transaction
    }
}

/// Begin a scheduler transaction on a handle without physical attestation.
///
/// Stores that schedule work for ingress descendants also run standalone,
/// against a database that has no ingress foundation. On PostgreSQL the
/// transaction therefore probes for `ingress_effect_descendants` under the
/// writer clamp. With the foundation it takes the epoch row lock and
/// installs the epoch proof, keeping the clamp; without it the session's
/// own timeouts are restored and no epoch is read. SQLite only takes its
/// write lock.
pub(crate) async fn begin_unattested_raw_writer(
    db: &Database,
) -> Result<EnteredTransaction<'_>, IngressEntryError> {
    let mut tx = db.begin_immediate().await?;
    if tx.driver() == DatabaseDriver::Postgres {
        tx.execute("SET TRANSACTION ISOLATION LEVEL READ COMMITTED", ())
            .await?;
        // Bound the catalog probe too, without changing standalone stores'
        // timeout policy after a foundation-free probe.
        let mut rows = tx
            .query(
                "SELECT current_setting('lock_timeout'), current_setting('statement_timeout')",
                (),
            )
            .await?;
        let row = rows
            .next()
            .await?
            .ok_or(IngressEntryError::BoundsUnproven)?;
        let lock_timeout: String = row.get(0)?;
        let statement_timeout: String = row.get(1)?;
        drop(rows);
        if !tighten_local_transaction_timeouts(&mut tx).await? {
            return Err(IngressEntryError::BoundsUnproven);
        }
        let mut foundation = tx
            .query(
                "SELECT 1 WHERE to_regclass('ingress_effect_descendants') IS NOT NULL",
                (),
            )
            .await?;
        let has_foundation = foundation.next().await?.is_some();
        drop(foundation);
        if has_foundation {
            if !tighten_local_transaction_timeouts(&mut tx).await? {
                return Err(IngressEntryError::BoundsUnproven);
            }
            let live = acquire_epoch_lock_first(&mut tx)
                .await
                .map_err(IngressEntryError::Preparation)?;
            let supported = supported_protocol_epoch();
            if live > supported {
                return Err(IngressEntryError::EpochUnsupported { live, supported });
            }
            let mut proof = tx.query("SELECT set_config('waddle.protocol_epoch', ?, true), set_config('waddle.protocol_epoch_xid', pg_current_xact_id()::text, true)", crate::db_params![live.to_storage().to_string()]).await?;
            proof
                .next()
                .await?
                .ok_or(IngressEntryError::EpochProofMissing)?;
            drop(proof);
        } else {
            tx.execute(
                "SELECT set_config('lock_timeout', ?, true), set_config('statement_timeout', ?, true)",
                crate::db_params![lock_timeout, statement_timeout],
            )
            .await?;
        }
    }
    Ok(EnteredTransaction { transaction: tx })
}

/// Bound transaction acquisition, including pool checkout and SQLite's
/// `BEGIN IMMEDIATE`, by the request's deadline. PostgreSQL's transaction
/// timeouts apply only after checkout, so expiry here has its own variant.
pub(crate) async fn acquire_within_deadline<'a>(
    deadline: Option<Duration>,
    acquisition: impl std::future::Future<Output = Result<Transaction<'a>, DatabaseError>>,
) -> Result<Transaction<'a>, IngressEntryError> {
    match deadline {
        Some(deadline) => Ok(tokio::time::timeout(deadline, acquisition)
            .await
            .map_err(|_| IngressEntryError::AcquisitionTimeout)??),
        None => Ok(acquisition.await?),
    }
}

/// Clamp relation-guard waits to 3 s of lock wait and 5 s per statement,
/// retaining any tighter transaction setting.
async fn bound_relation_guard_transaction(
    transaction: &mut Transaction<'_>,
) -> Result<bool, DatabaseError> {
    let mut rows = transaction
        .query(
            "SELECT pg_catalog.set_config('lock_timeout', CASE \
         WHEN pg_catalog.current_setting('lock_timeout')::interval = interval '0 ms' \
           OR pg_catalog.current_setting('lock_timeout')::interval > interval '3 s' \
         THEN '3s' ELSE pg_catalog.current_setting('lock_timeout') END, true), \
         pg_catalog.set_config('statement_timeout', CASE \
         WHEN pg_catalog.current_setting('statement_timeout')::interval = interval '0 ms' \
           OR pg_catalog.current_setting('statement_timeout')::interval > interval '5 s' \
         THEN '5s' ELSE pg_catalog.current_setting('statement_timeout') END, true)",
            (),
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

/// Prepare under the candidate relation guard: its relation prelocks, the
/// physical attestor on the same connection, then the guard's marker and
/// epoch proof. The guard's own writer capability is the one attested, so
/// the witness names the schema that capability was verified against.
async fn prepare_relation_guard(
    transaction: &mut Transaction<'_>,
    guard: &RelationGuardCapability,
) -> Result<PreparedAttestedReceiptWriter, DatabaseError> {
    guard.prelock_relations(transaction).await?;
    guard.writer().attest_transaction(transaction).await?;
    let protocol_epoch = guard.prove_epoch_after_attestation(transaction).await?;
    Ok(PreparedAttestedReceiptWriter {
        protocol_epoch,
        schema: guard.writer().identity().schema.name.0.clone(),
        relation_guard: true,
        definer_api_candidate: false,
    })
}

/// Prepare a writer from the physical attestor's complete identity under the
/// registered V1009 guard.
///
/// The schema-qualified epoch relation lock is the first SQL operation after
/// transaction isolation and timeout setup. Only then does the writer run
/// the trusted physical attestor on this same transaction connection, before
/// reading the epoch or taking application locks. This path never derives
/// identity from the active search path or DSN.
async fn prepare_installed_guard(
    tx: &mut Transaction<'_>,
    capability: &AttestedPostgresWriterCapability,
) -> Result<PreparedAttestedReceiptWriter, IngressEntryError> {
    if tx.driver() != DatabaseDriver::Postgres {
        return Err(IngressSubstrateError::Database {
            retry_class: DbRetryClass::NotRetryable,
        }
        .into_preparation());
    }

    let expected = &capability.identity().schema;
    let relation = format!(
        "{}.ingress_protocol_epoch",
        quote_postgres_identifier(&expected.name.0)
    );
    tx.execute(
        &format!("LOCK TABLE ONLY {relation} IN ACCESS SHARE MODE"),
        (),
    )
    .await
    .map_err(IngressEntryError::PreparationDatabase)?;

    capability
        .attest_transaction(tx)
        .await
        .map_err(IngressEntryError::PreparationDatabase)?;

    let mut schema_rows = tx
        .query(
            "SELECT oid::text FROM pg_catalog.pg_namespace WHERE nspname = ?",
            crate::db_params![expected.name.0.clone()],
        )
        .await
        .map_err(IngressEntryError::PreparationDatabase)?;
    let actual_schema_oid = schema_rows
        .next()
        .await
        .map_err(IngressEntryError::PreparationDatabase)?
        .ok_or(IngressSubstrateError::SchemaIdentityMismatch.into_preparation())?
        .get::<String>(0)
        .map_err(IngressEntryError::PreparationDatabase)?
        .parse::<u32>()
        .map_err(|_| IngressSubstrateError::SchemaIdentityMismatch.into_preparation())?;
    drop(schema_rows);
    if actual_schema_oid != expected.oid {
        return Err(IngressSubstrateError::SchemaIdentityMismatch.into_preparation());
    }

    let mut isolation_rows = tx
        .query("SELECT current_setting('transaction_isolation')", ())
        .await
        .map_err(IngressEntryError::PreparationDatabase)?;
    let isolation = isolation_rows
        .next()
        .await
        .map_err(IngressEntryError::PreparationDatabase)?
        .ok_or(IngressSubstrateError::ReadCommittedRequired.into_preparation())?
        .get::<String>(0)
        .map_err(IngressEntryError::PreparationDatabase)?;
    if isolation != "read committed" {
        return Err(IngressSubstrateError::ReadCommittedRequired.into_preparation());
    }
    drop(isolation_rows);

    // PostgreSQL searches the temporary schema before ordinary schemas unless
    // it is explicitly ordered. Keep the attested schema first and pg_temp
    // last for the remainder of this transaction; receipt SQL is also
    // explicitly schema-qualified at its repository boundary.
    let search_path = format!("{}, pg_temp", quote_postgres_identifier(&expected.name.0));
    let mut search_path_rows = tx
        .query(
            "SELECT pg_catalog.set_config('search_path', ?, true)",
            crate::db_params![search_path],
        )
        .await
        .map_err(IngressEntryError::PreparationDatabase)?;
    search_path_rows
        .next()
        .await
        .map_err(IngressEntryError::PreparationDatabase)?
        .ok_or(
            IngressSubstrateError::Database {
                retry_class: DbRetryClass::NotRetryable,
            }
            .into_preparation(),
        )?;
    drop(search_path_rows);

    let fresh_epoch = read_epoch_in_relation(tx, &relation, false).await?;
    let mut proof = tx
        .query(
            "SELECT set_config('waddle.protocol_epoch', ?, true), set_config('waddle.protocol_epoch_xid', pg_current_xact_id()::text, true)",
            crate::db_params![fresh_epoch.to_storage().to_string()],
        )
        .await
        .map_err(IngressEntryError::PreparationDatabase)?;
    proof
        .next()
        .await
        .map_err(IngressEntryError::PreparationDatabase)?
        .ok_or(
            IngressSubstrateError::Database {
                retry_class: DbRetryClass::NotRetryable,
            }
            .into_preparation(),
        )?;
    drop(proof);

    // The V1009 guard trigger requests the epoch row `FOR SHARE` on every
    // protected write. Taking it here keeps that request already granted and
    // fails the transaction if an activation landed since the proof above.
    let locked_epoch = read_epoch_in_relation(tx, &relation, true).await?;
    if locked_epoch != fresh_epoch {
        return Err(IngressSubstrateError::EpochChangedDuringPreparation.into_preparation());
    }
    Ok(PreparedAttestedReceiptWriter {
        protocol_epoch: locked_epoch,
        schema: expected.name.0.clone(),
        relation_guard: false,
        definer_api_candidate: false,
    })
}

/// Read the epoch row through its schema-qualified name, optionally taking
/// the `FOR SHARE` row lock the V1009 guard trigger requests.
async fn read_epoch_in_relation(
    tx: &mut Transaction<'_>,
    relation: &str,
    for_share: bool,
) -> Result<ProtocolEpoch, IngressEntryError> {
    let lock_clause = if for_share { " FOR SHARE" } else { "" };
    let mut rows = tx
        .query(
            &format!("SELECT epoch FROM {relation} WHERE id = 1{lock_clause}"),
            (),
        )
        .await
        .map_err(IngressEntryError::PreparationDatabase)?;
    let epoch = rows
        .next()
        .await
        .map_err(IngressEntryError::PreparationDatabase)?
        .ok_or(
            IngressSubstrateError::Database {
                retry_class: DbRetryClass::NotRetryable,
            }
            .into_preparation(),
        )?
        .get::<i64>(0)
        .map_err(IngressEntryError::PreparationDatabase)?;
    u32::try_from(epoch)
        .map(ProtocolEpoch::from_storage)
        .map_err(|_| IngressSubstrateError::UnsupportedLiveEpoch.into_preparation())
}
