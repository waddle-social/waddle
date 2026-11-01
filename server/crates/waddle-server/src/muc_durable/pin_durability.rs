//! Dark, claim-fenced storage for exact room-pin operations.
//!
//! This schema and API are deliberately not registered by
//! [`super::PostgresMucRoomStore::open`] or an application migration. The
//! migration-owner/runtime-role split is not provisioned yet, and this module
//! does not create a complete privilege boundary while the application role
//! owns the tables. The explicit schema bootstrap exists for isolated tests
//! and lab work only. No actor, ingress, archive, or wire path calls this API.
//!
//! The write lock order is room claim, active lifecycle, current snapshot,
//! then immutable operation outcome. Pin operations never lock or reference a
//! canonical message row. Archive custody uses canonical-message-before-room-
//! claim ordering, so this path must not add a canonical message lock or
//! foreign key after taking the room claim.

use std::fmt;

use jid::BareJid;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use waddle_xmpp::muc::durable::RoomCommitError;
use waddle_xmpp::muc::pin::{PinStateChange, PinnedEntry, MAX_PINNED_ENTRIES};
use waddle_xmpp::muc::{RoomClaimFenceContext, RoomLifecycleId};
use waddle_xmpp::ownership::{Entity, EntityType, NodeIdentity};
use waddle_xmpp_core::xep0359::StanzaId;

use crate::db::{Database, DatabaseDriver, DatabaseError, Transaction};

use super::implementation::remove_room_claim_fence_if;
use super::PostgresMucRoomStore;

const PIN_SCHEMA_ADVISORY_LOCK_KEY: i64 = 6_841_445_497_037_937_995;

/// Stable, globally unique identity for one accepted pin mutation.
///
/// Callers should mint one ID at the trusted acceptance edge and reuse it for
/// every retry. This slice defines the durable identity and operation API but
/// does not connect an ingress authority to the minting step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PinOperationId(uuid::Uuid);

impl PinOperationId {
    /// Wrap an ID minted by the trusted acceptance boundary.
    pub(crate) const fn from_uuid(id: uuid::Uuid) -> Self {
        Self(id)
    }

    /// Return the underlying UUID for persistence or correlation.
    pub const fn as_uuid(self) -> uuid::Uuid {
        self.0
    }
}

impl fmt::Display for PinOperationId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// Full ordered room-pin state restored under a currently held room claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinStateSnapshot {
    /// Room incarnation this snapshot belongs to.
    pub lifecycle: RoomLifecycleId,
    /// Monotonic snapshot revision within this lifecycle, starting at one.
    pub revision: i64,
    /// Most recent operation represented by the snapshot.
    pub last_operation_id: Option<PinOperationId>,
    /// Newest pin first, including the complete frozen preview of each entry.
    pub entries: Vec<PinnedEntry>,
}

/// Immutable result for one exact pin operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinOperationOutcome {
    /// The operation identity supplied for this commit.
    pub operation_id: PinOperationId,
    /// Room incarnation in which the operation committed.
    pub lifecycle: RoomLifecycleId,
    /// Exact typed mutation bound to `operation_id`.
    pub mutation: PinStateChange,
    /// Snapshot revision atomically committed with this operation.
    pub snapshot_revision: i64,
    /// Exact room claim that committed this outcome. A replay under a
    /// successor claim returns this original provenance.
    pub committed_fence: RoomClaimFenceContext,
}

/// Result of one commit attempt, separating historical operation evidence
/// from the current snapshot returned to the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinOperationCommit {
    /// Exact immutable evidence for the requested operation.
    pub outcome: PinOperationOutcome,
    /// Current full ordered state after this transaction.
    pub snapshot: PinStateSnapshot,
    /// True when this call returned an earlier outcome without reapplying it.
    pub was_replay: bool,
}

/// Failures from the dark room-pin durability API.
#[derive(Debug, Error)]
pub enum PinDurabilityError {
    /// The supplied room claim could not authorize this operation.
    #[error(transparent)]
    Claim(#[from] RoomCommitError),
    /// The room has no active lifecycle to receive pin operations.
    #[error("room has no active lifecycle for pin operations")]
    RoomNotActive,
    /// The same operation ID is already bound to another room, lifecycle, or
    /// exact mutation payload.
    #[error("pin operation ID is already bound to a different payload")]
    OperationIdConflict,
    /// The clustered room-claim authority is unavailable on this backend.
    #[error("room pin claim fencing is unsupported on this database backend")]
    UnsupportedBackend,
    /// Persisted JSON or row shape is invalid.
    #[error("stored room-pin state is invalid")]
    InvalidStoredState,
    /// The per-lifecycle snapshot revision cannot advance without overflow.
    #[error("room pin snapshot revision is exhausted")]
    RevisionExhausted,
    /// A database operation failed. The caller must resolve an uncertain
    /// commit by querying the exact operation ID under a current claim.
    #[error("room pin durability database operation failed")]
    Database(#[source] DatabaseError),
    /// A typed mutation or snapshot could not be encoded.
    #[error("room pin durability payload encoding failed")]
    Serialization(#[source] serde_json::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum StoredPinMutation {
    Pin { entry: PinnedEntry },
    Unpin { target_stanza_id: StanzaId },
}

impl From<&PinStateChange> for StoredPinMutation {
    fn from(value: &PinStateChange) -> Self {
        match value {
            PinStateChange::Pin(entry) => Self::Pin {
                entry: entry.clone(),
            },
            PinStateChange::Unpin { target_stanza_id } => Self::Unpin {
                target_stanza_id: target_stanza_id.clone(),
            },
        }
    }
}

impl From<StoredPinMutation> for PinStateChange {
    fn from(value: StoredPinMutation) -> Self {
        match value {
            StoredPinMutation::Pin { entry } => Self::Pin(entry),
            StoredPinMutation::Unpin { target_stanza_id } => Self::Unpin { target_stanza_id },
        }
    }
}

/// Apply a pin mutation using the room actor's replacement, ordering, and cap
/// rules. The helper is exposed for the later actor-wiring slice; this module
/// remains unused by runtime paths.
pub fn apply_pin_mutation(entries: &mut Vec<PinnedEntry>, change: &PinStateChange) {
    match change {
        PinStateChange::Pin(entry) => {
            remove_by_target(entries, &entry.target_stanza_id);
            entries.insert(0, entry.clone());
            entries.truncate(MAX_PINNED_ENTRIES);
        }
        PinStateChange::Unpin { target_stanza_id } => {
            remove_by_target(entries, target_stanza_id);
        }
    }
}

fn remove_by_target(entries: &mut Vec<PinnedEntry>, target: &StanzaId) {
    if let Some(position) = entries
        .iter()
        .position(|entry| entry.target_stanza_id.id == target.id)
    {
        entries.remove(position);
    }
}

fn validate_snapshot(entries: &[PinnedEntry]) -> Result<(), PinDurabilityError> {
    if entries.len() > MAX_PINNED_ENTRIES {
        return Err(PinDurabilityError::InvalidStoredState);
    }
    for (index, entry) in entries.iter().enumerate() {
        if entries[..index]
            .iter()
            .any(|previous| previous.target_stanza_id.id == entry.target_stanza_id.id)
        {
            return Err(PinDurabilityError::InvalidStoredState);
        }
    }
    Ok(())
}

/// Install the pin-operation lab schema for explicit tests or experiments.
///
/// This is intentionally not called during server startup and is absent from
/// the append-only migration ledger until the runtime-writer role policy and
/// retention owner are settled.
pub async fn ensure_schema(db: &Database) -> Result<(), DatabaseError> {
    let mut tx = db.begin().await?;
    match tx.driver() {
        DatabaseDriver::Postgres => {
            tx.query(
                "SELECT pg_advisory_xact_lock(?)",
                crate::db_params![PIN_SCHEMA_ADVISORY_LOCK_KEY],
            )
            .await?;
            ensure_postgres_schema(&mut tx).await?;
        }
        DatabaseDriver::Sqlite => ensure_sqlite_schema(&mut tx).await?,
    }
    tx.commit().await
}

async fn ensure_postgres_schema(tx: &mut Transaction<'_>) -> Result<(), DatabaseError> {
    tx.execute(
        "CREATE TABLE IF NOT EXISTS clustering_muc_room_pin_snapshots (\
         lifecycle_id TEXT PRIMARY KEY, room_jid TEXT NOT NULL, \
         pin_revision BIGINT NOT NULL CHECK (pin_revision >= 1), \
         last_operation_id TEXT NOT NULL, ordered_entries_json TEXT NOT NULL, \
         updated_at_ms BIGINT NOT NULL)",
        (),
    )
    .await?;
    tx.execute(
        "CREATE TABLE IF NOT EXISTS clustering_muc_room_pin_outcomes (\
         operation_id TEXT PRIMARY KEY, room_jid TEXT NOT NULL, lifecycle_id TEXT NOT NULL, \
         owner_node_id TEXT NOT NULL, owner_node_epoch TEXT NOT NULL, claim_epoch BIGINT NOT NULL CHECK (claim_epoch >= 1), \
         snapshot_revision BIGINT NOT NULL CHECK (snapshot_revision >= 1), \
         mutation_json TEXT NOT NULL, created_at_ms BIGINT NOT NULL)",
        (),
    )
    .await?;
    tx.execute(
        "CREATE INDEX IF NOT EXISTS clustering_muc_room_pin_outcomes_room_lifecycle_idx \
         ON clustering_muc_room_pin_outcomes (room_jid, lifecycle_id)",
        (),
    )
    .await?;
    tx.execute(
        "CREATE OR REPLACE FUNCTION waddle_muc_room_pin_outcomes_append_only() \
         RETURNS trigger LANGUAGE plpgsql AS $$ \
         BEGIN RAISE EXCEPTION 'waddle: room pin outcomes are append-only'; END; $$",
        (),
    )
    .await?;
    tx.execute(
        "DROP TRIGGER IF EXISTS clustering_muc_room_pin_outcomes_append_only_row \
         ON clustering_muc_room_pin_outcomes",
        (),
    )
    .await?;
    tx.execute(
        "CREATE TRIGGER clustering_muc_room_pin_outcomes_append_only_row \
         BEFORE UPDATE OR DELETE ON clustering_muc_room_pin_outcomes \
         FOR EACH ROW EXECUTE FUNCTION waddle_muc_room_pin_outcomes_append_only()",
        (),
    )
    .await?;
    tx.execute(
        "DROP TRIGGER IF EXISTS clustering_muc_room_pin_outcomes_append_only_truncate \
         ON clustering_muc_room_pin_outcomes",
        (),
    )
    .await?;
    tx.execute(
        "CREATE TRIGGER clustering_muc_room_pin_outcomes_append_only_truncate \
         BEFORE TRUNCATE ON clustering_muc_room_pin_outcomes \
         FOR EACH STATEMENT EXECUTE FUNCTION waddle_muc_room_pin_outcomes_append_only()",
        (),
    )
    .await?;
    tx.execute(
        "ALTER TABLE clustering_muc_room_pin_outcomes \
         ENABLE ALWAYS TRIGGER clustering_muc_room_pin_outcomes_append_only_row",
        (),
    )
    .await?;
    tx.execute(
        "ALTER TABLE clustering_muc_room_pin_outcomes \
         ENABLE ALWAYS TRIGGER clustering_muc_room_pin_outcomes_append_only_truncate",
        (),
    )
    .await?;
    Ok(())
}

async fn ensure_sqlite_schema(tx: &mut Transaction<'_>) -> Result<(), DatabaseError> {
    tx.execute(
        "CREATE TABLE IF NOT EXISTS clustering_muc_room_pin_snapshots (\
         lifecycle_id TEXT PRIMARY KEY, room_jid TEXT NOT NULL, \
         pin_revision BIGINT NOT NULL CHECK (pin_revision >= 1), \
         last_operation_id TEXT NOT NULL, ordered_entries_json TEXT NOT NULL, \
         updated_at_ms BIGINT NOT NULL)",
        (),
    )
    .await?;
    tx.execute(
        "CREATE TABLE IF NOT EXISTS clustering_muc_room_pin_outcomes (\
         operation_id TEXT PRIMARY KEY, room_jid TEXT NOT NULL, lifecycle_id TEXT NOT NULL, \
         owner_node_id TEXT NOT NULL, owner_node_epoch TEXT NOT NULL, claim_epoch BIGINT NOT NULL CHECK (claim_epoch >= 1), \
         snapshot_revision BIGINT NOT NULL CHECK (snapshot_revision >= 1), \
         mutation_json TEXT NOT NULL, created_at_ms BIGINT NOT NULL)",
        (),
    )
    .await?;
    tx.execute(
        "CREATE INDEX IF NOT EXISTS clustering_muc_room_pin_outcomes_room_lifecycle_idx \
         ON clustering_muc_room_pin_outcomes (room_jid, lifecycle_id)",
        (),
    )
    .await?;
    tx.execute(
        "CREATE TRIGGER IF NOT EXISTS clustering_muc_room_pin_outcomes_append_only_update \
         BEFORE UPDATE ON clustering_muc_room_pin_outcomes \
         BEGIN SELECT RAISE(ABORT, 'waddle: room pin outcomes are append-only'); END",
        (),
    )
    .await?;
    tx.execute(
        "CREATE TRIGGER IF NOT EXISTS clustering_muc_room_pin_outcomes_append_only_delete \
         BEFORE DELETE ON clustering_muc_room_pin_outcomes \
         BEGIN SELECT RAISE(ABORT, 'waddle: room pin outcomes are append-only'); END",
        (),
    )
    .await?;
    Ok(())
}

impl PostgresMucRoomStore {
    /// Atomically apply one exact pin operation, persist the full ordered
    /// snapshot, and append its immutable outcome under the current exact room
    /// claim. This API is dark until the pin schema is explicitly provisioned.
    pub async fn commit_pin_operation(
        &self,
        room_jid: &BareJid,
        fence: &RoomClaimFenceContext,
        operation_id: PinOperationId,
        mutation: PinStateChange,
    ) -> Result<PinOperationCommit, PinDurabilityError> {
        let stored_mutation = StoredPinMutation::from(&mutation);
        let mutation_json =
            serde_json::to_string(&stored_mutation).map_err(PinDurabilityError::Serialization)?;
        let Some(identity_guard) = self.node_identity.guard_if_current(&fence.owner).await else {
            remove_room_claim_fence_if(&self.exact_claim_fences, room_jid, fence);
            remove_room_claim_fence_if(&self.published_claim_fences, room_jid, fence);
            return Err(PinDurabilityError::Claim(RoomCommitError::NotOwner));
        };
        let mut tx = self
            .db
            .begin()
            .await
            .map_err(PinDurabilityError::Database)?;
        assert_current_claim(self, &mut tx, room_jid, fence).await?;
        let lifecycle = lock_active_lifecycle(&mut tx, room_jid).await?;
        let operation_id_text = operation_id.to_string();
        if let Some(outcome) =
            load_existing_outcome(&mut tx, room_jid, lifecycle, operation_id, &mutation_json)
                .await?
        {
            let snapshot = lock_snapshot(&mut tx, room_jid, lifecycle).await?;
            tx.commit().await.map_err(PinDurabilityError::Database)?;
            drop(identity_guard);
            return Ok(PinOperationCommit {
                outcome,
                snapshot,
                was_replay: true,
            });
        }

        let mut snapshot = lock_snapshot(&mut tx, room_jid, lifecycle).await?;
        apply_pin_mutation(&mut snapshot.entries, &mutation);
        validate_snapshot(&snapshot.entries)?;
        snapshot.revision = snapshot
            .revision
            .checked_add(1)
            .ok_or(PinDurabilityError::RevisionExhausted)?;
        snapshot.last_operation_id = Some(operation_id);
        let entries_json =
            serde_json::to_string(&snapshot.entries).map_err(PinDurabilityError::Serialization)?;
        let now_ms = crate::time::now_ms();
        let snapshot_rows = tx
            .execute(
                "INSERT INTO clustering_muc_room_pin_snapshots \
                 (lifecycle_id, room_jid, pin_revision, last_operation_id, ordered_entries_json, updated_at_ms) \
                 VALUES (?, ?, ?, ?, ?, ?) \
                 ON CONFLICT (lifecycle_id) DO UPDATE SET \
                   room_jid = excluded.room_jid, pin_revision = excluded.pin_revision, \
                   last_operation_id = excluded.last_operation_id, \
                   ordered_entries_json = excluded.ordered_entries_json, updated_at_ms = excluded.updated_at_ms \
                 WHERE clustering_muc_room_pin_snapshots.room_jid = excluded.room_jid",
                crate::db_params![
                    lifecycle.to_string(),
                    room_jid.to_string(),
                    snapshot.revision,
                    operation_id_text.clone(),
                    entries_json.clone(),
                    now_ms,
                ],
            )
            .await
            .map_err(PinDurabilityError::Database)?;
        if snapshot_rows != 1 {
            return Err(PinDurabilityError::InvalidStoredState);
        }
        let inserted = tx
            .execute(
                "INSERT INTO clustering_muc_room_pin_outcomes \
                 (operation_id, room_jid, lifecycle_id, owner_node_id, owner_node_epoch, claim_epoch, \
                  snapshot_revision, mutation_json, created_at_ms) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT (operation_id) DO NOTHING",
                crate::db_params![
                    operation_id_text,
                    room_jid.to_string(),
                    lifecycle.to_string(),
                    fence.owner.node_id.clone(),
                    fence.owner.node_epoch.clone(),
                    fence.epoch.0,
                    snapshot.revision,
                    mutation_json,
                    now_ms,
                ],
            )
            .await;
        let inserted = match inserted {
            Ok(rows) => rows,
            Err(error) => {
                let _ = tx.rollback().await;
                return Err(PinDurabilityError::Database(error));
            }
        };
        if inserted != 1 {
            let _ = tx.rollback().await;
            return Err(PinDurabilityError::OperationIdConflict);
        }
        tx.commit().await.map_err(PinDurabilityError::Database)?;
        drop(identity_guard);
        Ok(PinOperationCommit {
            outcome: PinOperationOutcome {
                operation_id,
                lifecycle,
                mutation,
                snapshot_revision: snapshot.revision,
                committed_fence: fence.clone(),
            },
            snapshot,
            was_replay: false,
        })
    }

    /// Restore the latest full ordered pin snapshot under the current exact
    /// room claim. No snapshot means the active lifecycle has no pins yet.
    pub async fn load_pin_snapshot_fenced(
        &self,
        room_jid: &BareJid,
        fence: &RoomClaimFenceContext,
    ) -> Result<Option<PinStateSnapshot>, PinDurabilityError> {
        let Some(identity_guard) = self.node_identity.guard_if_current(&fence.owner).await else {
            remove_room_claim_fence_if(&self.exact_claim_fences, room_jid, fence);
            remove_room_claim_fence_if(&self.published_claim_fences, room_jid, fence);
            return Err(PinDurabilityError::Claim(RoomCommitError::NotOwner));
        };
        let mut tx = self
            .db
            .begin()
            .await
            .map_err(PinDurabilityError::Database)?;
        assert_current_claim(self, &mut tx, room_jid, fence).await?;
        let lifecycle = read_active_lifecycle(&mut tx, room_jid).await?;
        let mut rows = tx
            .query(
                "SELECT pin_revision, last_operation_id, ordered_entries_json \
                 FROM clustering_muc_room_pin_snapshots WHERE room_jid = ? AND lifecycle_id = ?",
                crate::db_params![room_jid.to_string(), lifecycle.to_string()],
            )
            .await
            .map_err(PinDurabilityError::Database)?;
        let Some(row) = rows.next().await.map_err(PinDurabilityError::Database)? else {
            drop(rows);
            tx.commit().await.map_err(PinDurabilityError::Database)?;
            drop(identity_guard);
            return Ok(None);
        };
        let revision: i64 = row.get(0).map_err(PinDurabilityError::Database)?;
        let last_operation_id: String = row.get(1).map_err(PinDurabilityError::Database)?;
        let entries_json: String = row.get(2).map_err(PinDurabilityError::Database)?;
        drop(rows);
        let entries: Vec<PinnedEntry> = serde_json::from_str(&entries_json)
            .map_err(|_| PinDurabilityError::InvalidStoredState)?;
        validate_snapshot(&entries)?;
        if revision < 1 {
            return Err(PinDurabilityError::InvalidStoredState);
        }
        let last_operation_id = parse_operation_id(&last_operation_id)?;
        tx.commit().await.map_err(PinDurabilityError::Database)?;
        drop(identity_guard);
        Ok(Some(PinStateSnapshot {
            lifecycle,
            revision,
            last_operation_id: Some(last_operation_id),
            entries,
        }))
    }

    /// Resolve one retained operation outcome by exact ID while proving the
    /// caller holds the current exact room claim. The returned lifecycle is
    /// the one that committed the operation, so an accepted-edge caller can
    /// compare historical evidence after the room has been recreated.
    pub async fn load_pin_operation_outcome_fenced(
        &self,
        room_jid: &BareJid,
        fence: &RoomClaimFenceContext,
        operation_id: PinOperationId,
    ) -> Result<Option<PinOperationOutcome>, PinDurabilityError> {
        let Some(identity_guard) = self.node_identity.guard_if_current(&fence.owner).await else {
            remove_room_claim_fence_if(&self.exact_claim_fences, room_jid, fence);
            remove_room_claim_fence_if(&self.published_claim_fences, room_jid, fence);
            return Err(PinDurabilityError::Claim(RoomCommitError::NotOwner));
        };
        let mut tx = self
            .db
            .begin()
            .await
            .map_err(PinDurabilityError::Database)?;
        assert_current_claim(self, &mut tx, room_jid, fence).await?;
        let outcome = load_outcome_by_id(&mut tx, room_jid, operation_id).await?;
        tx.commit().await.map_err(PinDurabilityError::Database)?;
        drop(identity_guard);
        Ok(outcome)
    }
}

async fn assert_current_claim(
    store: &PostgresMucRoomStore,
    tx: &mut Transaction<'_>,
    room_jid: &BareJid,
    fence: &RoomClaimFenceContext,
) -> Result<(), PinDurabilityError> {
    if tx.driver() == DatabaseDriver::Sqlite {
        // SQLite is supported for the inert schema and mutation algorithm,
        // but this repository has no SQLite room-claim authority. Do not
        // emulate or invent one in this storage slice.
        return Err(PinDurabilityError::UnsupportedBackend);
    }
    if !store.exact_fence_is_established(room_jid, fence) {
        return Err(PinDurabilityError::Claim(
            RoomCommitError::OwnershipUnavailable,
        ));
    }
    store
        .assert_commit_fenced(tx, room_jid, fence, "FOR SHARE")
        .await
        .map_err(PinDurabilityError::Claim)
}

async fn lock_active_lifecycle(
    tx: &mut Transaction<'_>,
    room_jid: &BareJid,
) -> Result<RoomLifecycleId, PinDurabilityError> {
    let mut rows = tx
        .query(
            "SELECT lifecycle_id FROM clustering_muc_room_lifecycles \
             WHERE room_jid = ? AND state = 'active' FOR UPDATE",
            crate::db_params![room_jid.to_string()],
        )
        .await
        .map_err(PinDurabilityError::Database)?;
    let Some(row) = rows.next().await.map_err(PinDurabilityError::Database)? else {
        return Err(PinDurabilityError::RoomNotActive);
    };
    let lifecycle: String = row.get(0).map_err(PinDurabilityError::Database)?;
    drop(rows);
    parse_lifecycle_id(&lifecycle)
}

async fn read_active_lifecycle(
    tx: &mut Transaction<'_>,
    room_jid: &BareJid,
) -> Result<RoomLifecycleId, PinDurabilityError> {
    let mut rows = tx
        .query(
            "SELECT lifecycle_id FROM clustering_muc_room_lifecycles \
             WHERE room_jid = ? AND state = 'active' FOR SHARE",
            crate::db_params![room_jid.to_string()],
        )
        .await
        .map_err(PinDurabilityError::Database)?;
    let Some(row) = rows.next().await.map_err(PinDurabilityError::Database)? else {
        return Err(PinDurabilityError::RoomNotActive);
    };
    let lifecycle: String = row.get(0).map_err(PinDurabilityError::Database)?;
    drop(rows);
    parse_lifecycle_id(&lifecycle)
}

async fn lock_snapshot(
    tx: &mut Transaction<'_>,
    room_jid: &BareJid,
    lifecycle: RoomLifecycleId,
) -> Result<PinStateSnapshot, PinDurabilityError> {
    let mut rows = tx
        .query(
            "SELECT pin_revision, last_operation_id, ordered_entries_json \
             FROM clustering_muc_room_pin_snapshots \
             WHERE room_jid = ? AND lifecycle_id = ? FOR UPDATE",
            crate::db_params![room_jid.to_string(), lifecycle.to_string()],
        )
        .await
        .map_err(PinDurabilityError::Database)?;
    let Some(row) = rows.next().await.map_err(PinDurabilityError::Database)? else {
        return Ok(PinStateSnapshot {
            lifecycle,
            revision: 0,
            last_operation_id: None,
            entries: Vec::new(),
        });
    };
    let revision: i64 = row.get(0).map_err(PinDurabilityError::Database)?;
    let last_operation_id: String = row.get(1).map_err(PinDurabilityError::Database)?;
    let entries_json: String = row.get(2).map_err(PinDurabilityError::Database)?;
    drop(rows);
    let entries: Vec<PinnedEntry> =
        serde_json::from_str(&entries_json).map_err(|_| PinDurabilityError::InvalidStoredState)?;
    validate_snapshot(&entries)?;
    if revision < 1 {
        return Err(PinDurabilityError::InvalidStoredState);
    }
    Ok(PinStateSnapshot {
        lifecycle,
        revision,
        last_operation_id: Some(parse_operation_id(&last_operation_id)?),
        entries,
    })
}

async fn load_existing_outcome(
    tx: &mut Transaction<'_>,
    room_jid: &BareJid,
    lifecycle: RoomLifecycleId,
    operation_id: PinOperationId,
    mutation_json: &str,
) -> Result<Option<PinOperationOutcome>, PinDurabilityError> {
    let mut rows = tx
        .query(
            "SELECT room_jid, lifecycle_id, owner_node_id, owner_node_epoch, claim_epoch, \
                    snapshot_revision, mutation_json \
             FROM clustering_muc_room_pin_outcomes WHERE operation_id = ?",
            crate::db_params![operation_id.to_string()],
        )
        .await
        .map_err(PinDurabilityError::Database)?;
    let Some(row) = rows.next().await.map_err(PinDurabilityError::Database)? else {
        return Ok(None);
    };
    let stored_room: String = row.get(0).map_err(PinDurabilityError::Database)?;
    let stored_lifecycle: String = row.get(1).map_err(PinDurabilityError::Database)?;
    let owner_node_id: String = row.get(2).map_err(PinDurabilityError::Database)?;
    let owner_node_epoch: String = row.get(3).map_err(PinDurabilityError::Database)?;
    let claim_epoch: i64 = row.get(4).map_err(PinDurabilityError::Database)?;
    let snapshot_revision: i64 = row.get(5).map_err(PinDurabilityError::Database)?;
    let stored_mutation: String = row.get(6).map_err(PinDurabilityError::Database)?;
    drop(rows);
    if stored_room != room_jid.to_string()
        || stored_lifecycle != lifecycle.to_string()
        || stored_mutation != mutation_json
    {
        return Err(PinDurabilityError::OperationIdConflict);
    }
    let mutation = serde_json::from_str::<StoredPinMutation>(&stored_mutation)
        .map_err(|_| PinDurabilityError::InvalidStoredState)?
        .into();
    if claim_epoch < 1 || snapshot_revision < 1 {
        return Err(PinDurabilityError::InvalidStoredState);
    }
    Ok(Some(PinOperationOutcome {
        operation_id,
        lifecycle,
        mutation,
        snapshot_revision,
        committed_fence: RoomClaimFenceContext::new(
            Entity::new(EntityType::RoomActor, room_jid.to_string()),
            NodeIdentity::new(owner_node_id, owner_node_epoch),
            waddle_xmpp::ownership::ClaimEpoch(claim_epoch),
        ),
    }))
}

async fn load_outcome_by_id(
    tx: &mut Transaction<'_>,
    room_jid: &BareJid,
    operation_id: PinOperationId,
) -> Result<Option<PinOperationOutcome>, PinDurabilityError> {
    let mut rows = tx
        .query(
            "SELECT lifecycle_id, owner_node_id, owner_node_epoch, claim_epoch, \
                    snapshot_revision, mutation_json \
             FROM clustering_muc_room_pin_outcomes \
             WHERE operation_id = ? AND room_jid = ?",
            crate::db_params![operation_id.to_string(), room_jid.to_string()],
        )
        .await
        .map_err(PinDurabilityError::Database)?;
    let Some(row) = rows.next().await.map_err(PinDurabilityError::Database)? else {
        return Ok(None);
    };
    let lifecycle_text: String = row.get(0).map_err(PinDurabilityError::Database)?;
    let owner_node_id: String = row.get(1).map_err(PinDurabilityError::Database)?;
    let owner_node_epoch: String = row.get(2).map_err(PinDurabilityError::Database)?;
    let claim_epoch: i64 = row.get(3).map_err(PinDurabilityError::Database)?;
    let snapshot_revision: i64 = row.get(4).map_err(PinDurabilityError::Database)?;
    let mutation_json: String = row.get(5).map_err(PinDurabilityError::Database)?;
    drop(rows);
    let lifecycle = parse_lifecycle_id(&lifecycle_text)?;
    let mutation = serde_json::from_str::<StoredPinMutation>(&mutation_json)
        .map_err(|_| PinDurabilityError::InvalidStoredState)?
        .into();
    if claim_epoch < 1 || snapshot_revision < 1 {
        return Err(PinDurabilityError::InvalidStoredState);
    }
    Ok(Some(PinOperationOutcome {
        operation_id,
        lifecycle,
        mutation,
        snapshot_revision,
        committed_fence: RoomClaimFenceContext::new(
            Entity::new(EntityType::RoomActor, room_jid.to_string()),
            NodeIdentity::new(owner_node_id, owner_node_epoch),
            waddle_xmpp::ownership::ClaimEpoch(claim_epoch),
        ),
    }))
}

fn parse_operation_id(value: &str) -> Result<PinOperationId, PinDurabilityError> {
    uuid::Uuid::parse_str(value)
        .map(PinOperationId::from_uuid)
        .map_err(|_| PinDurabilityError::InvalidStoredState)
}

fn parse_lifecycle_id(value: &str) -> Result<RoomLifecycleId, PinDurabilityError> {
    uuid::Uuid::parse_str(value)
        .map(RoomLifecycleId::from_uuid)
        .map_err(|_| PinDurabilityError::InvalidStoredState)
}

#[cfg(test)]
mod tests;
