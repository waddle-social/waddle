//! Construction of the durable session store and its transaction entry.

use waddle_xmpp::ownership::SmSessionKey;
use waddle_xmpp::stream_management::durable::ExposureBounds;

use super::error::DurableSmError;
use super::record::{self, DurableSessionRecord};
use super::schema::bootstrap_schema_owner;
use crate::db::{Database, DatabaseDriver};

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
