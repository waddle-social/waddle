//! Construction of the durable session store and its transaction entry.

use std::time::Duration;

use waddle_xmpp::ownership::{Entity, EntityType, SmSessionKey};
use waddle_xmpp::stream_management::durable::ExposureBounds;
use waddle_xmpp::stream_management::persistence::SmClaimFence;

use super::error::DurableSmError;
use super::record::{self, DurableSessionRecord};
use super::schema::bootstrap_schema_owner;
use crate::db::{Database, DatabaseDriver, Transaction};

/// Longest a store transaction waits for a row or relation lock.
const LOCK_TIMEOUT: Duration = Duration::from_secs(3);
/// Longest any one statement of a store transaction runs.
const STATEMENT_TIMEOUT: Duration = Duration::from_secs(5);

/// The durable XEP-0198 session authority over one database.
///
/// Every mutation is one transaction that locks the session's authority row
/// first and compares the caller's fence under that lock. The store holds no
/// session state in memory, so any node attached to the database can serve
/// any session it holds the claim for.
#[derive(Clone)]
pub struct DurableSmStore {
    pub(super) db: Database,
    pub(super) bounds: ExposureBounds,
    /// Whether writes assert the session's `clustering_claims` row.
    pub(super) clustered: bool,
}

impl DurableSmStore {
    /// Open the store on a database this process may run DDL on: SQLite, or
    /// PostgreSQL without a restricted runtime role. The authority relations
    /// are created if absent.
    pub async fn open(
        db: Database,
        bounds: ExposureBounds,
        clustered: bool,
    ) -> Result<Self, DurableSmError> {
        if db.uses_attested_postgres_pool() {
            return Err(DurableSmError::AttestedPoolRequiresAttach);
        }
        if clustered && db.driver() != DatabaseDriver::Postgres {
            return Err(DurableSmError::ClusterRequiresPostgres);
        }
        bootstrap_schema_owner(&db).await?;
        Ok(Self {
            db,
            bounds,
            clustered,
        })
    }

    /// Attach to the owner-prepared relations on the attested runtime pool.
    /// The schema and the runtime role's exact privileges are verified in one
    /// read-only snapshot; nothing is created.
    pub(crate) async fn attach_runtime(
        db: Database,
        bounds: ExposureBounds,
        clustered: bool,
    ) -> Result<Self, DurableSmError> {
        crate::sm_persistence::verify_durable_runtime_postgres_schema(&db).await?;
        Ok(Self {
            db,
            bounds,
            clustered,
        })
    }

    pub fn bounds(&self) -> ExposureBounds {
        self.bounds
    }

    /// Whether every write asserts the session's claim row.
    pub fn is_clustered(&self) -> bool {
        self.clustered
    }

    /// Begin a write transaction. SQLite takes its write lock immediately,
    /// which serializes every store transaction. PostgreSQL runs READ
    /// COMMITTED with bounded lock and statement waits, so a transaction that
    /// meets a held row lock fails as a known rejection instead of waiting
    /// without limit.
    pub(super) async fn begin_write(&self) -> Result<Transaction<'_>, DurableSmError> {
        let mut tx = match self.db.driver() {
            DatabaseDriver::Sqlite => self.db.begin_immediate().await?,
            DatabaseDriver::Postgres => self.db.begin().await?,
        };
        if self.db.driver() == DatabaseDriver::Postgres {
            tx.execute("SET TRANSACTION ISOLATION LEVEL READ COMMITTED", ())
                .await?;
            if !crate::ingress_substrate::set_local_transaction_timeouts(
                &mut tx,
                LOCK_TIMEOUT,
                STATEMENT_TIMEOUT,
            )
            .await?
            {
                return Err(DurableSmError::BoundsUnproven);
            }
        }
        Ok(tx)
    }

    /// The claim a write must assert: the one supplied on a clustered store,
    /// none otherwise. A clustered write without a claim is refused.
    pub(super) fn write_claim<'c>(
        &self,
        claim: Option<&'c SmClaimFence>,
    ) -> Result<Option<&'c SmClaimFence>, DurableSmError> {
        match (self.clustered, claim) {
            (false, _) => Ok(None),
            (true, Some(claim)) => Ok(Some(claim)),
            (true, None) => Err(DurableSmError::ClaimRequired),
        }
    }

    /// Whether the exact claim row `(sm_session:<session>, node, node epoch,
    /// claim epoch)` exists. The row is share-locked for the rest of the
    /// transaction, so the claim cannot be stolen or released before the
    /// write it authorizes commits. It is taken before the session row lock.
    pub(super) async fn claim_is_held(
        &self,
        tx: &mut Transaction<'_>,
        session: SmSessionKey,
        claim: &SmClaimFence,
    ) -> Result<bool, DurableSmError> {
        let mut rows = tx
            .query(
                "SELECT 1 FROM clustering_claims WHERE entity = ? AND node_id = ? \
                 AND node_epoch = ? AND claim_epoch = ? FOR SHARE",
                crate::db_params![
                    Entity::new(EntityType::SmSession, record::stream_id(session).as_str())
                        .to_string(),
                    claim.owner().node_id.clone(),
                    claim.owner().node_epoch.clone(),
                    claim.epoch().0,
                ],
            )
            .await?;
        Ok(rows.next().await?.is_some())
    }

    /// Read a session's stored record without locking it.
    pub async fn session(
        &self,
        session: SmSessionKey,
    ) -> Result<Option<DurableSessionRecord>, DurableSmError> {
        let mut tx = self.db.begin().await?;
        let record = record::load(&mut tx, session).await?;
        tx.rollback().await?;
        Ok(record)
    }
}
