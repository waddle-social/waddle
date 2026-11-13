//! The account binding store over the server database.

use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use jid::{BareJid, FullJid};
use waddle_xmpp::ownership::account_coordinator::Presentation;
use waddle_xmpp::ownership::account_resource_transition::{
    AccountResourceExecution, AccountResourceOperation, AccountResourceOperationOutcome,
    AccountResourceRevision, ACCOUNT_RESOURCE_OPERATION_CAPACITY,
};
use waddle_xmpp::ownership::joint_identity::SmBinding;
use waddle_xmpp::ownership::{BindingRef, Entity, EntityType, SmSessionKey, UserClaimIdentity};

use super::decision::{
    check_account, decide, decide_presentation, operation_key, Decision, MutationKind,
    OperationFingerprint, RecordedOperation,
};
use super::error::AccountBindingStoreError;
use super::row::{self, StoredResource};
use super::schema::bootstrap_schema_owner;
use super::store::{AccountBindingStore, PresentationMark, StoredExecution};
use crate::db::{Database, DatabaseDriver, Transaction};
use crate::ingress_substrate::EnteredTransaction;

/// Longest a store transaction waits for a row or relation lock.
const LOCK_TIMEOUT: Duration = Duration::from_secs(3);
/// Longest any one statement of a store transaction runs.
const STATEMENT_TIMEOUT: Duration = Duration::from_secs(5);
/// `sm_durable_sessions.state` of a session that has ended and cannot
/// resume; every other state can.
const SESSION_STATE_TERMINAL: i64 = 4;

/// Account resource rows in SQLite (single node) or PostgreSQL.
///
/// The store holds no resource state in memory. On a clustered store every
/// write also proves, in its own transaction, that its authority is the
/// account's row in `clustering_claims`, so a node that lost the claim
/// cannot write the account's rows.
#[derive(Clone)]
pub struct DatabaseAccountBindingStore {
    db: Database,
    clustered: bool,
}

impl DatabaseAccountBindingStore {
    /// Open the store. On the attested runtime pool the owner-prepared
    /// relations and the runtime role's exact privileges are verified in one
    /// read-only snapshot and nothing is created. On SQLite, or PostgreSQL
    /// without a restricted runtime role, the relations are created if
    /// absent.
    pub async fn open(db: Database, clustered: bool) -> Result<Self, AccountBindingStoreError> {
        if clustered && db.driver() != DatabaseDriver::Postgres {
            return Err(AccountBindingStoreError::ClusterRequiresPostgres);
        }
        if db.uses_attested_postgres_pool() {
            super::runtime_attach::verify(&db).await?;
        } else {
            bootstrap_schema_owner(&db).await?;
        }
        Ok(Self { db, clustered })
    }

    /// Whether every write asserts the account's claim row.
    pub fn is_clustered(&self) -> bool {
        self.clustered
    }

    /// Begin a write transaction. SQLite takes its write lock immediately,
    /// which serializes every store transaction. PostgreSQL runs READ
    /// COMMITTED with bounded lock and statement waits, so a transaction
    /// that meets a held row lock fails instead of waiting without limit.
    ///
    /// The resource relations are under the ingress epoch guard, so the
    /// transaction proves the live protocol epoch before its first
    /// statement: a write this binary's epoch does not cover is refused
    /// here, and the epoch row lock precedes the claim and resource locks.
    async fn begin_write(&self) -> Result<EnteredTransaction<'_>, AccountBindingStoreError> {
        Ok(crate::ingress_substrate::begin_guarded_store_writer(
            &self.db,
            LOCK_TIMEOUT,
            STATEMENT_TIMEOUT,
        )
        .await?)
    }

    /// Begin a write under `authority`. On a clustered store the exact
    /// claim row `(user_actor:<bare JID>, node, node epoch, claim epoch)`
    /// must exist. The row is share-locked for the rest of the transaction,
    /// so the claim cannot be stolen or released before the write it
    /// authorizes commits. It is taken before the resource row lock.
    async fn begin_authorized(
        &self,
        authority: &UserClaimIdentity,
        full_jid: &FullJid,
    ) -> Result<EnteredTransaction<'_>, AccountBindingStoreError> {
        check_account(authority, full_jid)?;
        let mut tx = self.begin_write().await?;
        if self.clustered && !claim_is_held(&mut tx, authority).await? {
            tx.rollback().await?;
            return Err(AccountBindingStoreError::ClaimNotHeld);
        }
        Ok(tx)
    }
}

/// Whether the claim row the identity names exists, share-locked for the
/// rest of the transaction. A standalone process identity names no row.
async fn claim_is_held(
    tx: &mut Transaction<'_>,
    claim: &UserClaimIdentity,
) -> Result<bool, AccountBindingStoreError> {
    let node = claim
        .owner()
        .clustered_node()
        .ok_or(AccountBindingStoreError::ClaimRequired)?;
    let mut rows = tx
        .query(
            "SELECT 1 FROM clustering_claims WHERE entity = ? AND node_id = ? \
             AND node_epoch = ? AND claim_epoch = ? FOR SHARE",
            crate::db_params![
                Entity::new(EntityType::UserActor, claim.bare_jid().to_string()).to_string(),
                node.node_id,
                node.node_epoch,
                claim.claim_epoch().0,
            ],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

/// Row lock suffix: PostgreSQL locks the selected row; SQLite's immediate
/// transaction already excludes every other writer.
fn for_update(driver: DatabaseDriver) -> &'static str {
    match driver {
        DatabaseDriver::Postgres => " FOR UPDATE",
        DatabaseDriver::Sqlite => "",
    }
}

fn session_reference(stored: &StoredResource) -> Option<SmBinding> {
    stored
        .image
        .binding
        .as_ref()
        .and_then(|binding| binding.snapshot.stream_management_ref)
}

/// Require the durable session row a new reference names, bound at the
/// generation the reference names. The row is read in the write's
/// transaction; the foreign key keeps it from being deleted afterwards.
async fn check_session(
    tx: &mut Transaction<'_>,
    reference: SmBinding,
) -> Result<(), AccountBindingStoreError> {
    let mut rows = tx
        .query(
            "SELECT generation FROM sm_durable_sessions WHERE session_id = ?",
            crate::db_params![reference.session.as_uuid().to_string()],
        )
        .await?;
    let Some(found) = rows.next().await? else {
        return Err(AccountBindingStoreError::SessionUnknown);
    };
    let generation = row::unsigned(found.get(0)?, "session generation")?;
    if generation == reference.generation.value() {
        Ok(())
    } else {
        Err(AccountBindingStoreError::SessionGenerationMismatch)
    }
}

async fn load_resource(
    tx: &mut Transaction<'_>,
    full_jid: &FullJid,
    lock: bool,
) -> Result<Option<(StoredResource, u64)>, AccountBindingStoreError> {
    let suffix = if lock { for_update(tx.driver()) } else { "" };
    let mut rows = tx
        .query(
            &format!(
                "SELECT {} FROM account_resource_bindings WHERE full_jid = ?{suffix}",
                row::SELECTED
            ),
            crate::db_params![full_jid.to_string()],
        )
        .await?;
    rows.next().await?.as_ref().map(row::decode).transpose()
}

async fn load_operation(
    tx: &mut Transaction<'_>,
    operation: &AccountResourceOperation,
) -> Result<Option<RecordedOperation>, AccountBindingStoreError> {
    let mut rows = tx
        .query(
            "SELECT full_jid, kind, binding_ref, expected_revision, committed_revision \
             FROM account_resource_operations WHERE operation_id = ?",
            crate::db_params![operation_key(operation).to_string()],
        )
        .await?;
    let Some(found) = rows.next().await? else {
        return Ok(None);
    };
    let corrupt = |field| AccountBindingStoreError::CorruptRow { field };
    Ok(Some(RecordedOperation {
        fingerprint: OperationFingerprint {
            full_jid: found
                .get::<String>(0)?
                .parse()
                .map_err(|_| corrupt("operation full_jid"))?,
            kind: MutationKind::from_storage(found.get(1)?)?,
            binding_ref: row::from_json(&found.get::<String>(2)?, "operation binding_ref")?,
            expected_revision: AccountResourceRevision::new(row::unsigned(
                found.get(3)?,
                "expected_revision",
            )?),
        },
        committed_revision: AccountResourceRevision::new(row::unsigned(
            found.get(4)?,
            "committed_revision",
        )?),
    }))
}

async fn write_resource(
    tx: &mut Transaction<'_>,
    stored: &StoredResource,
    operation_seq: u64,
    exists: bool,
) -> Result<(), AccountBindingStoreError> {
    let params = row::write_params(stored, operation_seq, Utc::now().timestamp_millis())?;
    let sql = if exists {
        row::update_sql()
    } else {
        row::insert_sql()
    };
    tx.execute(&sql, params).await?;
    Ok(())
}

#[async_trait]
impl AccountBindingStore for DatabaseAccountBindingStore {
    async fn execute(
        &self,
        authority: &UserClaimIdentity,
        full_jid: &FullJid,
        operation: &AccountResourceOperation,
    ) -> Result<StoredExecution, AccountBindingStoreError> {
        let fingerprint = OperationFingerprint::of(full_jid, operation);
        let mut tx = self.begin_authorized(authority, full_jid).await?;
        if let Some(recorded) = load_operation(&mut tx, operation).await? {
            tx.rollback().await?;
            return Ok(StoredExecution {
                execution: recorded.replay(&fingerprint)?,
                stored: None,
            });
        }
        let existing = load_resource(&mut tx, full_jid, true).await?;
        let exists = existing.is_some();
        let previous_seq = existing.as_ref().map_or(0, |(_, seq)| *seq);
        let previous_session = existing
            .as_ref()
            .and_then(|(stored, _)| session_reference(stored));
        let decision = match decide(existing.map(|(stored, _)| stored), full_jid, operation) {
            Ok(decision) => decision,
            Err(error) => {
                tx.rollback().await?;
                return Err(error);
            }
        };
        let (revision, stored) = match decision {
            Decision::Rejected(error) => {
                tx.rollback().await?;
                return Ok(StoredExecution {
                    execution: AccountResourceExecution::Applied(
                        AccountResourceOperationOutcome::Rejected(error),
                    ),
                    stored: None,
                });
            }
            Decision::Committed { revision, stored } => (revision, stored),
        };
        if let Some(reference) =
            session_reference(&stored).filter(|reference| Some(*reference) != previous_session)
        {
            if let Err(error) = check_session(&mut tx, reference).await {
                tx.rollback().await?;
                return Err(error);
            }
        }
        let operation_seq = previous_seq
            .checked_add(1)
            .ok_or(AccountBindingStoreError::CounterOutOfRange)?;
        write_resource(&mut tx, &stored, operation_seq, exists).await?;
        tx.execute(
            "INSERT INTO account_resource_operations \
             (operation_id, full_jid, kind, binding_ref, expected_revision, \
              committed_revision, operation_seq, recorded_at_ms) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            crate::db_params![
                operation_key(operation).to_string(),
                full_jid.to_string(),
                fingerprint.kind.to_storage(),
                row::to_json(&fingerprint.binding_ref, "operation binding_ref")?,
                row::signed(fingerprint.expected_revision.value())?,
                row::signed(revision.value())?,
                row::signed(operation_seq)?,
                Utc::now().timestamp_millis(),
            ],
        )
        .await?;
        // Keep as many recorded operations per resource as the slot's own
        // journal holds.
        let capacity = ACCOUNT_RESOURCE_OPERATION_CAPACITY as u64;
        if operation_seq > capacity {
            tx.execute(
                "DELETE FROM account_resource_operations \
                 WHERE full_jid = ? AND operation_seq <= ?",
                crate::db_params![full_jid.to_string(), row::signed(operation_seq - capacity)?],
            )
            .await?;
        }
        tx.commit().await?;
        Ok(StoredExecution {
            execution: AccountResourceExecution::Applied(
                AccountResourceOperationOutcome::Committed { revision },
            ),
            stored: Some(*stored),
        })
    }

    async fn mark_presented(
        &self,
        authority: &UserClaimIdentity,
        full_jid: &FullJid,
        binding_ref: &BindingRef,
        presentation: Presentation,
    ) -> Result<PresentationMark, AccountBindingStoreError> {
        let mut tx = self.begin_authorized(authority, full_jid).await?;
        let existing = load_resource(&mut tx, full_jid, true).await?;
        let operation_seq = existing.as_ref().map_or(0, |(_, seq)| *seq);
        let (mark, write) = decide_presentation(
            existing.map(|(stored, _)| stored),
            binding_ref,
            presentation,
        );
        match write {
            Some(stored) => {
                write_resource(&mut tx, &stored, operation_seq, true).await?;
                tx.commit().await?;
            }
            None => tx.rollback().await?,
        }
        Ok(mark)
    }

    async fn forget(
        &self,
        authority: &UserClaimIdentity,
        full_jid: &FullJid,
        expected_revision: AccountResourceRevision,
    ) -> Result<bool, AccountBindingStoreError> {
        let mut tx = self.begin_authorized(authority, full_jid).await?;
        let free = load_resource(&mut tx, full_jid, true)
            .await?
            .is_some_and(|(stored, _)| {
                stored.image.binding.is_none() && stored.image.revision == expected_revision
            });
        if !free {
            tx.rollback().await?;
            return Ok(false);
        }
        tx.execute(
            "DELETE FROM account_resource_operations WHERE full_jid = ?",
            crate::db_params![full_jid.to_string()],
        )
        .await?;
        tx.execute(
            "DELETE FROM account_resource_bindings WHERE full_jid = ?",
            crate::db_params![full_jid.to_string()],
        )
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    async fn load(
        &self,
        full_jid: &FullJid,
    ) -> Result<Option<StoredResource>, AccountBindingStoreError> {
        let mut tx = self.db.begin().await?;
        let stored = load_resource(&mut tx, full_jid, false).await?;
        tx.rollback().await?;
        Ok(stored.map(|(stored, _)| stored))
    }

    async fn resources_for_session(
        &self,
        session: SmSessionKey,
    ) -> Result<Vec<FullJid>, AccountBindingStoreError> {
        let mut tx = self.db.begin().await?;
        let mut resources = Vec::new();
        {
            let mut rows = tx
                .query(
                    "SELECT full_jid FROM account_resource_bindings \
                     WHERE sm_session_id = ? ORDER BY full_jid",
                    crate::db_params![session.as_uuid().to_string()],
                )
                .await?;
            while let Some(found) = rows.next().await? {
                resources.push(
                    found
                        .get::<String>(0)?
                        .parse()
                        .map_err(|_| AccountBindingStoreError::CorruptRow { field: "full_jid" })?,
                );
            }
        }
        tx.rollback().await?;
        Ok(resources)
    }

    async fn session_resumable(
        &self,
        session: SmSessionKey,
    ) -> Result<bool, AccountBindingStoreError> {
        let mut tx = self.db.begin().await?;
        let resumable = {
            let mut rows = tx
                .query(
                    "SELECT state FROM sm_durable_sessions WHERE session_id = ?",
                    crate::db_params![session.as_uuid().to_string()],
                )
                .await?;
            match rows.next().await? {
                Some(found) => found.get::<i64>(0)? != SESSION_STATE_TERMINAL,
                None => false,
            }
        };
        tx.rollback().await?;
        Ok(resumable)
    }

    async fn bound_accounts(
        &self,
        after: Option<&BareJid>,
        limit: u32,
    ) -> Result<Vec<BareJid>, AccountBindingStoreError> {
        let mut tx = self.db.begin().await?;
        let mut accounts = Vec::new();
        {
            let mut rows = tx
                .query(
                    "SELECT DISTINCT bare_jid FROM account_resource_bindings \
                     WHERE binding_ref IS NOT NULL AND bare_jid > ? \
                     ORDER BY bare_jid LIMIT ?",
                    crate::db_params![
                        after.map(ToString::to_string).unwrap_or_default(),
                        i64::from(limit)
                    ],
                )
                .await?;
            while let Some(found) = rows.next().await? {
                accounts.push(
                    found
                        .get::<String>(0)?
                        .parse()
                        .map_err(|_| AccountBindingStoreError::CorruptRow { field: "bare_jid" })?,
                );
            }
        }
        tx.rollback().await?;
        Ok(accounts)
    }

    async fn load_account(
        &self,
        account: &BareJid,
    ) -> Result<Vec<StoredResource>, AccountBindingStoreError> {
        let mut tx = self.db.begin().await?;
        let mut resources = Vec::new();
        {
            let mut rows = tx
                .query(
                    &format!(
                        "SELECT {} FROM account_resource_bindings \
                         WHERE bare_jid = ? ORDER BY full_jid",
                        row::SELECTED
                    ),
                    crate::db_params![account.to_string()],
                )
                .await?;
            while let Some(found) = rows.next().await? {
                resources.push(row::decode(&found)?.0);
            }
        }
        tx.rollback().await?;
        Ok(resources)
    }
}
