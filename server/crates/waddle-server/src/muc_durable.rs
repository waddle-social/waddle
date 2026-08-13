//! Postgres-backed durable MUC room ownership state (ADR-0017 Phase 3
//! Slice 7, element 7).
//!
//! Implements `waddle_xmpp::muc::MucDurableStore` for [`PostgresMucRoomStore`]
//! — the concrete, clustering-only backing store a freshly spawned/claimed
//! `RoomActor` restores configuration, affiliations, and subject from
//! before accepting any join, and persists the same three pieces to on
//! every change. `None` (no `MucDurableStore` configured at all) in
//! single-node/non-clustering deployments, matching today's purely
//! in-memory room behavior exactly — this module is only ever constructed
//! when clustering is enabled against a Postgres backend, mirroring
//! `sm_persistence_fenced::PostgresFencedSmPersistence`'s identical gating.
//!
//! **Schema** (PROPOSED — no ADR-locked DDL exists for MUC room durability,
//! only the column shapes element 7's text names): `clustering_` prefixed,
//! following the `clustering_nodes`/`clustering_claims`/
//! `clustering_steal_intents` convention (store-owned ensure-schema, not a
//! versioned app migration, since these tables exist purely to back the
//! clustering-durability subsystem and the migration-version freeze in
//! #1651 deliberately excludes them):
//! - `clustering_muc_rooms` — one row per durably-written room: `waddle_id`/
//!   `channel_id` plus the JSON-serialized `RoomConfig`/`SubjectState`
//!   (both already `Serialize`/`Deserialize` for exactly this purpose).
//! - `clustering_muc_room_affiliations` — one row per `(room_jid, member_jid)`
//!   affiliation grant. `affiliation` is stored via a small
//!   `affiliation_to_db_str`/`affiliation_from_db_str` pair, mirroring
//!   `EntityType::as_db_str`/`from_db_str`'s exact convention, rather than a
//!   JSON blob for a five-variant closed enum.
//! - `clustering_muc_room_lifecycles` — one row per room incarnation. Its
//!   closed `state` vocabulary is `active` | `dormant` | `tombstoned`; the
//!   partial unique index permits at most one live (`active` or `dormant`)
//!   incarnation per room while retaining tombstones for #1646's effect drain.
//!   The nullable `clustering_muc_rooms.lifecycle_id`/`revision` snapshot
//!   back-link is deliberately nullable forever: `NULL` is a legitimate
//!   pre-lifecycle row. All lifecycle schema is inert in this slice; #1645 is
//!   its first writer.
//!
//! **Fencing**: every `save_*` write runs [`PostgresMucRoomStore::assert_fenced`]
//! — the exact `SELECT ... FOR SHARE` shape `sm_persistence_fenced::
//! assert_fenced` already established — as the first statement inside the
//! same [`crate::db::Database::begin`] transaction as the write it guards,
//! on the **main pool**, never the control-plane pool (the Slice 0/4/7
//! pool-assignment rule). Actor-owned loads, saves, and deletes receive their
//! immutable incarnation fence explicitly. The unpublished exact-fence
//! bookkeeping and the published room-keyed fan-out cache are tracked
//! separately: actor-owned durable work may establish its exact fence during
//! preparation, while the legacy pre-fanout groupchat dispatch/MAM cache is
//! still published only with the matching ready registry entry.

use dashmap::DashMap;
use jid::BareJid;
use tokio_util::sync::CancellationToken;
use waddle_xmpp::muc::affiliation::AffiliationEntry;
use waddle_xmpp::muc::durable::{
    AffiliationEntry as DurableAffiliationEntry, RoomCommitDatabaseError, RoomCommitError,
    RoomCommitFuture, RoomCommittedCoordinates, RoomDurableMutation,
};
use waddle_xmpp::muc::{
    DurableRoomState, MucDurableFuture, MucDurableStore, RoomClaimFenceContext, RoomConfig,
    RoomLifecycleId, RoomLifecycleState, RoomRevision, SubjectState,
};
use waddle_xmpp::ownership::{
    ClaimEpoch, CurrentNodeIdentityGuard, Entity, EntityType, SharedNodeIdentity,
};
use waddle_xmpp::{Affiliation, XmppError};

use crate::clustering::relay::RelayHandle;
use crate::clustering::NodeId;
use crate::db::{Database, DatabaseError, Transaction};

/// Dedicated transaction-scoped Postgres advisory lock for MUC store schema
/// bootstrap. It is distinct from the clustering claims lock
/// (`6_841_445_497_037_937_991`), migration-ledger lock
/// (`6_841_445_497_037_937_992`), and lineage lock
/// (`6_841_445_497_037_937_993`) because each protects a separate bootstrap
/// invariant and should not serialize unrelated startup work.
const MUC_SCHEMA_ADVISORY_LOCK_KEY: i64 = 6_841_445_497_037_937_994;
const ROOM_COMMIT_RETRY_ATTEMPTS: usize = 5;

fn is_retryable_tx_error(error: &DatabaseError) -> bool {
    matches!(
        error,
        DatabaseError::Internal(sqlx::Error::Database(inner))
            if matches!(inner.code().as_deref(), Some("40001" | "40P01"))
                || (inner.code().as_deref() == Some("23505")
                    && inner.constraint()
                        == Some("clustering_muc_room_lifecycles_live_room_idx"))
    )
}

fn commit_database_error() -> RoomCommitError {
    RoomCommitError::Database(RoomCommitDatabaseError::sanitized())
}

fn db_err(error: DatabaseError) -> XmppError {
    // Durable MUC operations translate database failures into typed XMPP
    // errors, so record the internal failure before returning the protocol
    // response to the caller.
    crate::telemetry::mark_span_error("MUC durable storage operation failed");
    XmppError::internal(format!("MUC durable store backend error: {error}"))
}

fn fence_db_unavailable(error: DatabaseError, fence: &RoomClaimFenceContext) -> XmppError {
    // An unavailable fence proof is a backend failure, unlike a successful
    // proof that reports ownership was lost.
    crate::telemetry::mark_span_error("MUC durable ownership fence check failed");
    tracing::warn!(
        %error,
        entity = %fence.entity,
        "MUC durable store could not prove the exact ownership fence"
    );
    XmppError::OwnershipUnavailable {
        entity: fence.entity.clone(),
    }
}

/// Injective `(entity_type, id) -> TEXT` encoding for the
/// `clustering_claims.entity` primary key, mirroring
/// `clustering::claims::entity_key`/`sm_persistence_fenced::
/// sm_session_entity_key` exactly. Duplicated here rather than imported
/// for the same accepted-tradeoff reason those two already duplicate it
/// from each other: this impl owns its own inline fencing SQL rather than
/// delegating to `ClaimStore`.
fn room_entity_key(room_jid: &BareJid) -> String {
    format!("{}:{}", EntityType::RoomActor.as_db_str(), room_jid)
}

/// `EntityType::as_db_str`/`from_db_str`'s exact convention, applied to
/// `Affiliation` (a closed, five-variant enum) for the
/// `clustering_muc_room_affiliations.affiliation` column — a small typed
/// mapping rather than a JSON blob for a value this simple.
fn affiliation_to_db_str(affiliation: Affiliation) -> &'static str {
    match affiliation {
        Affiliation::Outcast => "outcast",
        Affiliation::None => "none",
        Affiliation::Member => "member",
        Affiliation::Admin => "admin",
        Affiliation::Owner => "owner",
    }
}

fn affiliation_from_db_str(value: &str) -> Option<Affiliation> {
    match value {
        "outcast" => Some(Affiliation::Outcast),
        "none" => Some(Affiliation::None),
        "member" => Some(Affiliation::Member),
        "admin" => Some(Affiliation::Admin),
        "owner" => Some(Affiliation::Owner),
        _ => None,
    }
}

/// Postgres-backed [`MucDurableStore`] (ADR-0017 Phase 3 Slice 7). See the
/// module doc for the schema and fencing design.
pub struct PostgresMucRoomStore {
    db: Database,
    /// Live process incarnation. Cached room fences remain immutable; every
    /// use must still match this handle so a self-fenced old incarnation
    /// cannot write merely because its old claim row remains in Postgres.
    node_identity: SharedNodeIdentity,
    /// This node's clustering-scope cancellation token, threaded into the
    /// `RelayHandle` [`Self::notify_previous_owner_demoted`] constructs
    /// per-call (mirroring `resume_asker::SwarmRemoteResumeAsker`'s
    /// identical "fresh `RelayHandle` per ask" pattern).
    stop_token: CancellationToken,
    /// Exact claim fences established for actor-owned durable work.
    exact_claim_fences: DashMap<BareJid, RoomClaimFenceContext>,
    /// Fence for the currently published local room actor. Retained for
    /// publication observability and legacy pre-fanout groupchat dispatch/MAM
    /// consumers; actor-derived durable work carries its immutable fence.
    published_claim_fences: DashMap<BareJid, RoomClaimFenceContext>,
}

fn remove_room_claim_fence_if(
    claim_fences: &DashMap<BareJid, RoomClaimFenceContext>,
    room_jid: &BareJid,
    expected: &RoomClaimFenceContext,
) {
    claim_fences.remove_if(room_jid, |_, current| current == expected);
}

impl PostgresMucRoomStore {
    /// Open against an already-opened Postgres [`Database`] handle — the
    /// SAME global handle `clustering::start_if_enabled` gives the claims
    /// store, never a second, independently-resolved database (the fencing
    /// `SELECT ... FOR SHARE` this impl issues targets `clustering_claims`,
    /// which lives there).
    pub async fn open(
        db: Database,
        stop_token: CancellationToken,
        node_identity: SharedNodeIdentity,
    ) -> Result<Self, XmppError> {
        let store = Self {
            db,
            node_identity,
            stop_token,
            exact_claim_fences: DashMap::new(),
            published_claim_fences: DashMap::new(),
        };
        store.ensure_schema().await.map_err(db_err)?;
        Ok(store)
    }

    async fn ensure_schema(&self) -> Result<(), DatabaseError> {
        let mut tx = self.db.begin().await?;
        // The advisory-lock loser must observe the winner's committed DDL,
        // even when the deployment's session default is stricter.
        tx.execute("SET TRANSACTION ISOLATION LEVEL READ COMMITTED", ())
            .await?;
        tx.query(
            "SELECT pg_advisory_xact_lock(?)",
            crate::db_params![MUC_SCHEMA_ADVISORY_LOCK_KEY],
        )
        .await?;
        tx.execute(
            r#"
            CREATE TABLE IF NOT EXISTS clustering_muc_rooms (
                room_jid     TEXT PRIMARY KEY,
                waddle_id    TEXT NOT NULL,
                channel_id   TEXT NOT NULL,
                config_json  TEXT NOT NULL,
                subject_json TEXT,
                updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
            )
            "#,
            (),
        )
        .await?;
        tx.execute(
            r#"
            CREATE TABLE IF NOT EXISTS clustering_muc_room_affiliations (
                room_jid    TEXT NOT NULL,
                member_jid  TEXT NOT NULL,
                affiliation TEXT NOT NULL,
                reason      TEXT,
                granted_at  TIMESTAMPTZ,
                PRIMARY KEY (room_jid, member_jid)
            )
            "#,
            (),
        )
        .await?;
        // Both index creations are catalog-guarded rather than
        // `CREATE INDEX IF NOT EXISTS`: non-concurrent CREATE INDEX takes
        // SHARE on the target table BEFORE evaluating IF NOT EXISTS and, in
        // the already-exists case, retains it to end of transaction — which,
        // now that this bootstrap is one advisory-locked transaction, would
        // block every fenced affiliation write cluster-wide (SHARE conflicts
        // with ROW EXCLUSIVE) on every steady-state pod start, with no
        // lock_timeout. The pg_index probe takes no lock on the relation and
        // is bound to the target table's own regclass — an unqualified
        // to_regclass('<index name>') would resolve across the whole
        // search_path, so a same-named index in another schema could falsely
        // suppress creation on the table this transaction just created.
        // `'<table>'::regclass` here and the unqualified CREATE INDEX target
        // resolve through the same search_path rules, so the probe and the
        // DDL always agree on one relation.
        tx.execute(
            r#"
            DO $$
            BEGIN
                IF NOT EXISTS (
                    SELECT 1
                    FROM pg_index i
                    JOIN pg_class c ON c.oid = i.indexrelid
                    WHERE i.indrelid = 'clustering_muc_room_affiliations'::regclass
                      AND c.relname = 'clustering_muc_room_affiliations_room_jid_idx'
                ) THEN
                    CREATE INDEX clustering_muc_room_affiliations_room_jid_idx
                        ON clustering_muc_room_affiliations (room_jid);
                END IF;
            END $$
            "#,
            (),
        )
        .await?;
        tx.execute(
            r#"
            CREATE TABLE IF NOT EXISTS clustering_muc_room_lifecycles (
                lifecycle_id TEXT PRIMARY KEY,
                room_jid     TEXT NOT NULL,
                revision     BIGINT NOT NULL CONSTRAINT clustering_muc_room_lifecycles_revision_min CHECK (revision >= 1),
                state        TEXT NOT NULL CONSTRAINT clustering_muc_room_lifecycles_state_closed CHECK (state IN ('active','dormant','tombstoned')),
                created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
                updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
            )
            "#,
            (),
        )
        .await?;
        tx.execute(
            r#"
            DO $$
            BEGIN
                IF NOT EXISTS (
                    SELECT 1
                    FROM pg_index i
                    JOIN pg_class c ON c.oid = i.indexrelid
                    WHERE i.indrelid = 'clustering_muc_room_lifecycles'::regclass
                      AND c.relname = 'clustering_muc_room_lifecycles_live_room_idx'
                ) THEN
                    CREATE UNIQUE INDEX clustering_muc_room_lifecycles_live_room_idx
                        ON clustering_muc_room_lifecycles (room_jid) WHERE state IN ('active','dormant');
                END IF;
            END $$
            "#,
            (),
        )
        .await?;
        // Catalog-guarded instead of `ADD COLUMN IF NOT EXISTS`: Postgres
        // acquires ACCESS EXCLUSIVE on the relation before it evaluates the
        // per-column IF NOT EXISTS clause, so the bare form would queue every
        // cluster-wide room read and write behind each pod start — held for
        // the rest of this transaction, with no lock_timeout, while the
        // advisory lock above additionally serializes every other starting
        // pod behind the wait. The pg_attribute probe takes no lock on the
        // table (and, unlike information_schema.columns, is not filtered by
        // column privileges); only the one bootstrap that actually adds a
        // column pays for the relation lock.
        tx.execute(
            r#"
            DO $$
            BEGIN
                IF NOT EXISTS (
                    SELECT 1
                    FROM pg_attribute
                    WHERE attrelid = 'clustering_muc_rooms'::regclass
                      AND attname = 'lifecycle_id'
                      AND NOT attisdropped
                ) THEN
                    ALTER TABLE clustering_muc_rooms ADD COLUMN lifecycle_id TEXT;
                END IF;
            END $$
            "#,
            (),
        )
        .await?;
        tx.execute(
            r#"
            DO $$
            BEGIN
                IF NOT EXISTS (
                    SELECT 1
                    FROM pg_attribute
                    WHERE attrelid = 'clustering_muc_rooms'::regclass
                      AND attname = 'revision'
                      AND NOT attisdropped
                ) THEN
                    ALTER TABLE clustering_muc_rooms ADD COLUMN revision BIGINT;
                END IF;
            END $$
            "#,
            (),
        )
        .await?;
        tx.execute(
            r#"
            DO $$
            BEGIN
                IF NOT EXISTS (
                    SELECT 1
                    FROM pg_constraint
                    WHERE conname = 'clustering_muc_rooms_lifecycle_pairing'
                      AND conrelid = 'clustering_muc_rooms'::regclass
                ) THEN
                    ALTER TABLE clustering_muc_rooms
                        ADD CONSTRAINT clustering_muc_rooms_lifecycle_pairing
                        CHECK ((lifecycle_id IS NULL) = (revision IS NULL));
                END IF;
            END $$
            "#,
            (),
        )
        .await?;
        tx.execute(
            r#"
            DO $$
            BEGIN
                IF NOT EXISTS (
                    SELECT 1
                    FROM pg_constraint
                    WHERE conname = 'clustering_muc_rooms_revision_min'
                      AND conrelid = 'clustering_muc_rooms'::regclass
                ) THEN
                    ALTER TABLE clustering_muc_rooms
                        ADD CONSTRAINT clustering_muc_rooms_revision_min
                        CHECK (revision IS NULL OR revision >= 1);
                END IF;
            END $$
            "#,
            (),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    fn fence_for(&self, room_jid: &BareJid) -> Result<RoomClaimFenceContext, XmppError> {
        let fence = self
            .published_claim_fences
            .get(room_jid)
            .map(|entry| entry.clone())
            .ok_or_else(|| {
                XmppError::internal(format!(
                    "no published claim fence recorded for room {room_jid}; fenced fan-out skipped"
                ))
            })?;
        if self.node_identity.current() != fence.owner {
            remove_room_claim_fence_if(&self.exact_claim_fences, room_jid, &fence);
            remove_room_claim_fence_if(&self.published_claim_fences, room_jid, &fence);
            return Err(XmppError::internal(format!(
                "fenced fan-out for room {room_jid} aborted: cached claim belongs to a stale node incarnation"
            )));
        }
        Ok(fence)
    }

    fn exact_fence_is_established(
        &self,
        room_jid: &BareJid,
        fence: &RoomClaimFenceContext,
    ) -> bool {
        self.exact_claim_fences.get(room_jid).as_deref() == Some(fence)
    }

    async fn guard_fence_identity(
        &self,
        room_jid: &BareJid,
        fence: &RoomClaimFenceContext,
    ) -> Result<CurrentNodeIdentityGuard, XmppError> {
        let Some(identity_guard) = self.node_identity.guard_if_current(&fence.owner).await else {
            remove_room_claim_fence_if(&self.exact_claim_fences, room_jid, fence);
            remove_room_claim_fence_if(&self.published_claim_fences, room_jid, fence);
            return Err(XmppError::OwnershipLost {
                entity: fence.entity.clone(),
            });
        };
        Ok(identity_guard)
    }

    /// Take the fencing lock inside `tx` — the exact `SELECT ... FOR SHARE`
    /// shape `sm_persistence_fenced::assert_fenced` already established —
    /// for `room_jid` at `epoch`. See that function's doc comment for the
    /// full contract this mirrors.
    async fn assert_fenced(
        &self,
        tx: &mut Transaction<'_>,
        room_jid: &BareJid,
        fence: &RoomClaimFenceContext,
    ) -> Result<(), XmppError> {
        let expected_entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        if fence.entity != expected_entity {
            return Err(XmppError::OwnershipLost {
                entity: fence.entity.clone(),
            });
        }
        if self.node_identity.current() != fence.owner {
            remove_room_claim_fence_if(&self.exact_claim_fences, room_jid, fence);
            remove_room_claim_fence_if(&self.published_claim_fences, room_jid, fence);
            return Err(XmppError::OwnershipLost {
                entity: expected_entity,
            });
        }
        let key = room_entity_key(room_jid);
        let mut rows = tx
            .query(
                "SELECT 1 FROM clustering_claims WHERE entity = ? AND node_id = ? AND node_epoch = ? AND claim_epoch = ? FOR SHARE",
                crate::db_params![
                    key,
                    fence.owner.node_id.clone(),
                    fence.owner.node_epoch.clone(),
                    fence.epoch.0
                ],
            )
            .await
            .map_err(|error| fence_db_unavailable(error, fence))?;
        let held = rows
            .next()
            .await
            .map_err(|error| fence_db_unavailable(error, fence))?
            .is_some();
        if !held {
            remove_room_claim_fence_if(&self.exact_claim_fences, room_jid, fence);
            remove_room_claim_fence_if(&self.published_claim_fences, room_jid, fence);
            return Err(XmppError::OwnershipLost {
                entity: expected_entity,
            });
        }
        debug_assert_eq!(self.node_identity.current(), fence.owner);
        Ok(())
    }

    async fn assert_fenced_for_update(
        &self,
        tx: &mut Transaction<'_>,
        room_jid: &BareJid,
        fence: &RoomClaimFenceContext,
    ) -> Result<(), RoomCommitError> {
        self.assert_commit_fenced(tx, room_jid, fence, "FOR UPDATE")
            .await
    }

    async fn assert_commit_fenced(
        &self,
        tx: &mut Transaction<'_>,
        room_jid: &BareJid,
        fence: &RoomClaimFenceContext,
        lock_mode: &'static str,
    ) -> Result<(), RoomCommitError> {
        if !self.exact_fence_is_established(room_jid, fence) {
            return Err(RoomCommitError::OwnershipUnavailable);
        }
        let expected_entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        if fence.entity != expected_entity || self.node_identity.current() != fence.owner {
            remove_room_claim_fence_if(&self.exact_claim_fences, room_jid, fence);
            remove_room_claim_fence_if(&self.published_claim_fences, room_jid, fence);
            return Err(RoomCommitError::NotOwner);
        }
        let query = match lock_mode {
            "FOR UPDATE" => "SELECT 1 FROM clustering_claims WHERE entity = ? AND node_id = ? AND node_epoch = ? AND claim_epoch = ? FOR UPDATE",
            _ => "SELECT 1 FROM clustering_claims WHERE entity = ? AND node_id = ? AND node_epoch = ? AND claim_epoch = ? FOR SHARE",
        };
        let mut rows = tx
            .query(
                query,
                crate::db_params![
                    room_entity_key(room_jid),
                    fence.owner.node_id.clone(),
                    fence.owner.node_epoch.clone(),
                    fence.epoch.0,
                ],
            )
            .await
            .map_err(Self::commit_fence_error)?;
        let held = rows
            .next()
            .await
            .map_err(Self::commit_fence_error)?
            .is_some();
        if !held {
            remove_room_claim_fence_if(&self.exact_claim_fences, room_jid, fence);
            remove_room_claim_fence_if(&self.published_claim_fences, room_jid, fence);
            return Err(RoomCommitError::NotOwner);
        }
        Ok(())
    }

    fn commit_error(error: DatabaseError) -> RoomCommitError {
        if is_retryable_tx_error(&error) {
            RoomCommitError::RetryExhausted
        } else {
            commit_database_error()
        }
    }

    fn commit_fence_error(error: DatabaseError) -> RoomCommitError {
        if is_retryable_tx_error(&error) {
            RoomCommitError::RetryExhausted
        } else {
            RoomCommitError::OwnershipUnavailable
        }
    }

    async fn write_commit_affiliation(
        tx: &mut Transaction<'_>,
        room_jid: &BareJid,
        entry: &DurableAffiliationEntry,
    ) -> Result<(), RoomCommitError> {
        if let Some(affiliation) = entry.affiliation {
            tx.execute(
                r#"
                INSERT INTO clustering_muc_room_affiliations (room_jid, member_jid, affiliation, reason)
                VALUES (?, ?, ?, NULL)
                ON CONFLICT (room_jid, member_jid) DO UPDATE SET
                    affiliation = excluded.affiliation,
                    reason = NULL
                "#,
                crate::db_params![
                    room_jid.to_string(),
                    entry.jid.to_string(),
                    affiliation_to_db_str(affiliation).to_string(),
                ],
            )
            .await
            .map_err(Self::commit_error)?;
        } else {
            tx.execute(
                "DELETE FROM clustering_muc_room_affiliations WHERE room_jid = ? AND member_jid = ?",
                crate::db_params![room_jid.to_string(), entry.jid.to_string()],
            )
            .await
            .map_err(Self::commit_error)?;
        }
        Ok(())
    }

    async fn commit_room_mutation_once(
        &self,
        room_jid: &BareJid,
        fence: &RoomClaimFenceContext,
        intent: &RoomDurableMutation,
    ) -> Result<RoomCommittedCoordinates, RoomCommitError> {
        if self.node_identity.current() != fence.owner {
            remove_room_claim_fence_if(&self.exact_claim_fences, room_jid, fence);
            remove_room_claim_fence_if(&self.published_claim_fences, room_jid, fence);
            return Err(RoomCommitError::NotOwner);
        }
        let mut tx = self
            .db
            .begin()
            .await
            .map_err(|_| RoomCommitError::OwnershipUnavailable)?;
        let exclusive_claim = matches!(
            intent,
            RoomDurableMutation::Destroy
                | RoomDurableMutation::Dormancy
                | RoomDurableMutation::Activate
        );
        if exclusive_claim {
            self.assert_fenced_for_update(&mut tx, room_jid, fence)
                .await?;
        } else {
            self.assert_commit_fenced(&mut tx, room_jid, fence, "FOR SHARE")
                .await?;
        }

        let mut rows = tx
            .query(
                "SELECT lifecycle_id, revision, state FROM clustering_muc_room_lifecycles WHERE room_jid = ? AND state IN ('active', 'dormant') FOR UPDATE",
                crate::db_params![room_jid.to_string()],
            )
            .await
            .map_err(Self::commit_error)?;
        let existing = rows.next().await.map_err(Self::commit_error)?;
        drop(rows);
        let lifecycle_row_exists = existing.is_some();

        let (lifecycle, revision, state) = if let Some(row) = existing {
            let lifecycle: String = row.get(0).map_err(Self::commit_error)?;
            let revision: i64 = row.get(1).map_err(Self::commit_error)?;
            let state: String = row.get(2).map_err(Self::commit_error)?;
            let lifecycle = uuid::Uuid::parse_str(&lifecycle)
                .map(RoomLifecycleId::from_uuid)
                .map_err(|_| commit_database_error())?;
            let revision = RoomRevision::from_stored(revision).ok_or_else(commit_database_error)?;
            let state =
                RoomLifecycleState::from_db_str(&state).ok_or_else(commit_database_error)?;
            (lifecycle, revision, state)
        } else if matches!(intent, RoomDurableMutation::Create { .. }) {
            let lifecycle = RoomLifecycleId::generate();
            let revision = RoomRevision::initial();
            match tx
                .execute(
                    "INSERT INTO clustering_muc_room_lifecycles (lifecycle_id, room_jid, revision, state) VALUES (?, ?, ?, ?)",
                    crate::db_params![lifecycle.to_string(), room_jid.to_string(), revision.as_i64(), RoomLifecycleState::Active.as_db_str()],
                )
                .await
            {
                Ok(_) => (lifecycle, revision, RoomLifecycleState::Active),
                Err(error) => return Err(Self::commit_error(error)),
            }
        } else if matches!(intent, RoomDurableMutation::Activate) {
            // Lifecycle adoption: durable room state written before the
            // lifecycle table existed has no live lifecycle row. Activation
            // adopts it by minting one — but only when the room row itself
            // exists. Destroy wipes room rows, so adoption can never
            // resurrect a destroyed room; a true miss stays StateMissing.
            let mut room_rows = tx
                .query(
                    "SELECT 1 FROM clustering_muc_rooms WHERE room_jid = ?",
                    crate::db_params![room_jid.to_string()],
                )
                .await
                .map_err(Self::commit_error)?;
            if room_rows
                .next()
                .await
                .map_err(Self::commit_error)?
                .is_none()
            {
                return Err(RoomCommitError::StateMissing);
            }
            drop(room_rows);
            let lifecycle = RoomLifecycleId::generate();
            let revision = RoomRevision::initial();
            tx.execute(
                "INSERT INTO clustering_muc_room_lifecycles (lifecycle_id, room_jid, revision, state) VALUES (?, ?, ?, ?)",
                crate::db_params![lifecycle.to_string(), room_jid.to_string(), revision.as_i64(), RoomLifecycleState::Active.as_db_str()],
            )
            .await
            .map_err(Self::commit_error)?;
            tx.execute(
                "UPDATE clustering_muc_rooms SET lifecycle_id = ?, revision = ?, updated_at = now() WHERE room_jid = ?",
                crate::db_params![lifecycle.to_string(), revision.as_i64(), room_jid.to_string()],
            )
            .await
            .map_err(Self::commit_error)?;
            (lifecycle, revision, RoomLifecycleState::Active)
        } else {
            return Err(RoomCommitError::StateMissing);
        };

        if matches!(intent, RoomDurableMutation::Create { .. }) && lifecycle_row_exists {
            let RoomDurableMutation::Create {
                waddle_id,
                channel_id,
                ..
            } = intent
            else {
                unreachable!()
            };
            let mut room_rows = tx
                .query(
                    "SELECT waddle_id, channel_id FROM clustering_muc_rooms WHERE room_jid = ?",
                    crate::db_params![room_jid.to_string()],
                )
                .await
                .map_err(Self::commit_error)?;
            let Some(room_row) = room_rows.next().await.map_err(Self::commit_error)? else {
                return Err(RoomCommitError::StateMissing);
            };
            let stored_waddle: String = room_row.get(0).map_err(Self::commit_error)?;
            let stored_channel: String = room_row.get(1).map_err(Self::commit_error)?;
            drop(room_rows);
            return if &stored_waddle == waddle_id && &stored_channel == channel_id {
                tx.commit().await.map_err(Self::commit_error)?;
                Ok(RoomCommittedCoordinates {
                    lifecycle,
                    revision,
                })
            } else {
                Err(RoomCommitError::CreateConflict)
            };
        }
        if state == RoomLifecycleState::Dormant
            && !matches!(
                intent,
                RoomDurableMutation::Activate | RoomDurableMutation::Destroy
            )
        {
            return Err(RoomCommitError::StateMissing);
        }
        if matches!(intent, RoomDurableMutation::Activate) && state == RoomLifecycleState::Active {
            // Idempotent: restoring an already-active lifecycle is not a state
            // transition, so it keeps its coordinates and bumps nothing. A
            // missing lifecycle stays a hard `StateMissing` above — callers
            // must not conflate the two.
            tx.commit().await.map_err(Self::commit_error)?;
            return Ok(RoomCommittedCoordinates {
                lifecycle,
                revision,
            });
        }

        let next_revision = if matches!(intent, RoomDurableMutation::Create { .. }) {
            revision
        } else {
            revision.next().ok_or(RoomCommitError::RevisionOverflow)?
        };
        match intent {
            RoomDurableMutation::Create {
                waddle_id,
                channel_id,
                config,
                initial_affiliations,
            } => {
                let config_json =
                    serde_json::to_string(config).map_err(|_| commit_database_error())?;
                tx.execute(
                    "INSERT INTO clustering_muc_rooms (room_jid, waddle_id, channel_id, config_json, lifecycle_id, revision) VALUES (?, ?, ?, ?, ?, ?)",
                    crate::db_params![room_jid.to_string(), waddle_id.clone(), channel_id.clone(), config_json, lifecycle.to_string(), next_revision.as_i64()],
                ).await.map_err(Self::commit_error)?;
                for entry in initial_affiliations {
                    Self::write_commit_affiliation(&mut tx, room_jid, entry).await?;
                }
            }
            RoomDurableMutation::Config {
                config,
                waddle_id,
                channel_id,
            } => {
                let config_json =
                    serde_json::to_string(config).map_err(|_| commit_database_error())?;
                let affected = tx.execute(
                    "UPDATE clustering_muc_rooms SET waddle_id = ?, channel_id = ?, config_json = ?, lifecycle_id = ?, revision = ?, updated_at = now() WHERE room_jid = ?",
                    crate::db_params![waddle_id.clone(), channel_id.clone(), config_json, lifecycle.to_string(), next_revision.as_i64(), room_jid.to_string()],
                ).await.map_err(Self::commit_error)?;
                if affected == 0 {
                    return Err(RoomCommitError::StateMissing);
                }
            }
            RoomDurableMutation::Subject(subject) => {
                let subject_json = subject
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()
                    .map_err(|_| commit_database_error())?;
                let affected = tx.execute("UPDATE clustering_muc_rooms SET subject_json = ?, lifecycle_id = ?, revision = ?, updated_at = now() WHERE room_jid = ?", crate::db_params![subject_json, lifecycle.to_string(), next_revision.as_i64(), room_jid.to_string()]).await.map_err(Self::commit_error)?;
                if affected == 0 {
                    return Err(RoomCommitError::StateMissing);
                }
            }
            RoomDurableMutation::Affiliation(entry)
            | RoomDurableMutation::MediatedInviteGrant(entry)
            | RoomDurableMutation::MediatedInviteRollback(entry) => {
                Self::write_commit_affiliation(&mut tx, room_jid, entry).await?;
                tx.execute("UPDATE clustering_muc_rooms SET lifecycle_id = ?, revision = ?, updated_at = now() WHERE room_jid = ?", crate::db_params![lifecycle.to_string(), next_revision.as_i64(), room_jid.to_string()]).await.map_err(Self::commit_error)?;
            }
            RoomDurableMutation::AffiliationBatch(entries) => {
                for entry in entries {
                    Self::write_commit_affiliation(&mut tx, room_jid, entry).await?;
                }
                tx.execute("UPDATE clustering_muc_rooms SET lifecycle_id = ?, revision = ?, updated_at = now() WHERE room_jid = ?", crate::db_params![lifecycle.to_string(), next_revision.as_i64(), room_jid.to_string()]).await.map_err(Self::commit_error)?;
            }
            RoomDurableMutation::MembersOnlyEnforcement {
                config,
                affiliations,
            } => {
                let config_json =
                    serde_json::to_string(config).map_err(|_| commit_database_error())?;
                tx.execute("UPDATE clustering_muc_rooms SET config_json = ?, lifecycle_id = ?, revision = ?, updated_at = now() WHERE room_jid = ?", crate::db_params![config_json, lifecycle.to_string(), next_revision.as_i64(), room_jid.to_string()]).await.map_err(Self::commit_error)?;
                for entry in affiliations {
                    Self::write_commit_affiliation(&mut tx, room_jid, entry).await?;
                }
            }
            RoomDurableMutation::Destroy => {
                tx.execute(
                    "DELETE FROM clustering_muc_room_affiliations WHERE room_jid = ?",
                    crate::db_params![room_jid.to_string()],
                )
                .await
                .map_err(Self::commit_error)?;
                tx.execute(
                    "DELETE FROM clustering_muc_rooms WHERE room_jid = ?",
                    crate::db_params![room_jid.to_string()],
                )
                .await
                .map_err(Self::commit_error)?;
            }
            RoomDurableMutation::Dormancy | RoomDurableMutation::Activate => {
                let affected = tx
                    .execute(
                        "UPDATE clustering_muc_rooms SET lifecycle_id = ?, revision = ?, updated_at = now() WHERE room_jid = ?",
                        crate::db_params![
                            lifecycle.to_string(),
                            next_revision.as_i64(),
                            room_jid.to_string()
                        ],
                    )
                    .await
                    .map_err(Self::commit_error)?;
                if affected == 0 {
                    return Err(RoomCommitError::StateMissing);
                }
            }
        }
        let final_state = match intent {
            RoomDurableMutation::Destroy => RoomLifecycleState::Tombstoned,
            RoomDurableMutation::Dormancy => RoomLifecycleState::Dormant,
            RoomDurableMutation::Activate => RoomLifecycleState::Active,
            _ => state,
        };
        tx.execute(
            "UPDATE clustering_muc_room_lifecycles SET revision = ?, state = ?, updated_at = now() WHERE lifecycle_id = ?",
            crate::db_params![next_revision.as_i64(), final_state.as_db_str(), lifecycle.to_string()],
        ).await.map_err(Self::commit_error)?;
        tx.commit().await.map_err(Self::commit_error)?;
        Ok(RoomCommittedCoordinates {
            lifecycle,
            revision: next_revision,
        })
    }

    async fn exact_claim_is_held(
        &self,
        room_jid: &BareJid,
        fence: &RoomClaimFenceContext,
    ) -> Result<bool, XmppError> {
        let expected_entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        if fence.entity != expected_entity {
            return Ok(false);
        }
        if self.node_identity.current() != fence.owner {
            remove_room_claim_fence_if(&self.exact_claim_fences, room_jid, fence);
            remove_room_claim_fence_if(&self.published_claim_fences, room_jid, fence);
            return Ok(false);
        }
        let conn = self
            .db
            .guard()
            .await
            .map_err(|error| fence_db_unavailable(error, fence))?;
        let mut rows = conn
            .query(
                "/* muc_exact_claim_check */ \
                 SELECT 1 FROM clustering_claims WHERE entity = ? AND node_id = ? \
                 AND node_epoch = ? AND claim_epoch = ? FOR SHARE",
                crate::db_params![
                    room_entity_key(room_jid),
                    fence.owner.node_id.clone(),
                    fence.owner.node_epoch.clone(),
                    fence.epoch.0,
                ],
            )
            .await
            .map_err(|error| fence_db_unavailable(error, fence))?;
        let held = rows
            .next()
            .await
            .map_err(|error| fence_db_unavailable(error, fence))?
            .is_some();
        drop(rows);
        if !held {
            remove_room_claim_fence_if(&self.exact_claim_fences, room_jid, fence);
            remove_room_claim_fence_if(&self.published_claim_fences, room_jid, fence);
            return Ok(false);
        }
        // The database proof may block, so it deliberately runs without a
        // rotation guard. Confirm the local incarnation only after all
        // backend I/O; this short guard is dropped before returning and can
        // never delay identity rotation behind a stalled database call.
        let Some(identity_guard) = self.node_identity.guard_if_current(&fence.owner).await else {
            remove_room_claim_fence_if(&self.exact_claim_fences, room_jid, fence);
            remove_room_claim_fence_if(&self.published_claim_fences, room_jid, fence);
            return Ok(false);
        };
        drop(identity_guard);
        Ok(true)
    }

    async fn load_room_state_in_tx(
        tx: &mut Transaction<'_>,
        room_jid: &BareJid,
    ) -> Result<Option<DurableRoomState>, XmppError> {
        let mut room_rows = tx
            .query(
                "SELECT waddle_id, channel_id, config_json, subject_json FROM \
                 clustering_muc_rooms WHERE room_jid = ?",
                crate::db_params![room_jid.to_string()],
            )
            .await
            .map_err(db_err)?;
        let Some(row) = room_rows.next().await.map_err(db_err)? else {
            return Ok(None);
        };
        let waddle_id: String = row.get(0).map_err(db_err)?;
        let channel_id: String = row.get(1).map_err(db_err)?;
        let config_json: String = row.get(2).map_err(db_err)?;
        let subject_json: Option<String> = row.get(3).map_err(db_err)?;
        let config: RoomConfig = serde_json::from_str(&config_json).map_err(|error| {
            XmppError::internal(format!("durable room config decode failed: {error}"))
        })?;
        let subject: Option<SubjectState> = subject_json
            .map(|json| serde_json::from_str(&json))
            .transpose()
            .map_err(|error| {
                XmppError::internal(format!("durable room subject decode failed: {error}"))
            })?;

        let mut affiliation_rows = tx
            .query(
                "SELECT member_jid, affiliation, reason FROM \
                 clustering_muc_room_affiliations WHERE room_jid = ?",
                crate::db_params![room_jid.to_string()],
            )
            .await
            .map_err(db_err)?;
        let mut affiliations = Vec::new();
        while let Some(row) = affiliation_rows.next().await.map_err(db_err)? {
            let member_jid: String = row.get(0).map_err(db_err)?;
            let affiliation_str: String = row.get(1).map_err(db_err)?;
            let reason: Option<String> = row.get(2).map_err(db_err)?;
            let Ok(jid) = member_jid.parse::<BareJid>() else {
                tracing::warn!(
                    room = %room_jid,
                    member_jid,
                    "durable affiliation row has an unparseable member JID; skipping"
                );
                continue;
            };
            let Some(affiliation) = affiliation_from_db_str(&affiliation_str) else {
                tracing::warn!(
                    room = %room_jid,
                    affiliation = affiliation_str,
                    "durable affiliation row has an unrecognized affiliation tag; skipping"
                );
                continue;
            };
            affiliations.push(AffiliationEntry {
                jid,
                affiliation,
                granted_at: None,
                reason,
            });
        }

        Ok(Some(DurableRoomState {
            waddle_id,
            channel_id,
            config,
            subject,
            affiliations,
        }))
    }
}

impl MucDurableStore for PostgresMucRoomStore {
    fn commit_room_mutation<'a>(
        &'a self,
        room_jid: &'a BareJid,
        fence: &'a RoomClaimFenceContext,
        intent: RoomDurableMutation,
    ) -> RoomCommitFuture<'a> {
        Box::pin(async move {
            for attempt in 0..ROOM_COMMIT_RETRY_ATTEMPTS {
                match self
                    .commit_room_mutation_once(room_jid, fence, &intent)
                    .await
                {
                    Ok(coordinates) => return Ok(coordinates),
                    Err(RoomCommitError::RetryExhausted)
                        if attempt + 1 < ROOM_COMMIT_RETRY_ATTEMPTS =>
                    {
                        let jitter = (uuid::Uuid::now_v7().as_u128() % 4) as u64;
                        let delay =
                            std::time::Duration::from_millis(2 + attempt as u64 * 3 + jitter);
                        tokio::time::sleep(delay).await;
                    }
                    Err(error) => return Err(error),
                }
            }
            Err(RoomCommitError::RetryExhausted)
        })
    }

    fn load_room_state_fenced<'a>(
        &'a self,
        room_jid: &'a BareJid,
        fence: &'a RoomClaimFenceContext,
    ) -> MucDurableFuture<'a, Option<DurableRoomState>> {
        Box::pin(async move {
            let _identity_guard = self.guard_fence_identity(room_jid, fence).await?;
            let mut tx = self
                .db
                .begin()
                .await
                .map_err(|error| fence_db_unavailable(error, fence))?;
            tx.execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ", ())
                .await
                .map_err(db_err)?;
            self.assert_fenced(&mut tx, room_jid, fence).await?;
            let state = Self::load_room_state_in_tx(&mut tx, room_jid).await?;
            tx.commit().await.map_err(db_err)?;
            Ok(state)
        })
    }

    fn establish_claim_fence(&self, room_jid: &BareJid, fence: RoomClaimFenceContext) {
        let expected = Entity::new(EntityType::RoomActor, room_jid.to_string());
        if fence.entity != expected {
            tracing::warn!(
                room = %room_jid,
                entity = %fence.entity,
                "refusing to establish a MUC exact claim fence for a different entity"
            );
            return;
        }
        self.exact_claim_fences.insert(room_jid.clone(), fence);
    }

    fn record_claim_fence(&self, room_jid: &BareJid, fence: RoomClaimFenceContext) {
        let expected = Entity::new(EntityType::RoomActor, room_jid.to_string());
        if fence.entity != expected {
            tracing::warn!(
                room = %room_jid,
                entity = %fence.entity,
                "refusing to cache a MUC claim fence for a different entity"
            );
            return;
        }
        self.exact_claim_fences
            .insert(room_jid.clone(), fence.clone());
        self.published_claim_fences.insert(room_jid.clone(), fence);
    }

    fn forget_claim_fence(&self, room_jid: &BareJid, expected: &RoomClaimFenceContext) {
        remove_room_claim_fence_if(&self.exact_claim_fences, room_jid, expected);
        remove_room_claim_fence_if(&self.published_claim_fences, room_jid, expected);
    }

    /// Cache-backed legacy pre-fanout backstop: a fenced,
    /// standalone-autocommit `SELECT ... FOR SHARE` on the main pool. This
    /// remains in production for groupchat dispatch/MAM until #1283 replaces
    /// it; actor-owned durable operations use their immutable fence instead.
    fn check_fenced_fanout<'a>(&'a self, room_jid: &'a BareJid) -> MucDurableFuture<'a, bool> {
        Box::pin(async move {
            let Some(fence) = self
                .published_claim_fences
                .get(room_jid)
                .map(|entry| entry.clone())
            else {
                // Ready clustered rooms publish this cache entry before the
                // registry exposes their actor. Absence therefore means this
                // process cannot serve the retained room incarnation. It is
                // local state, not backend uncertainty, and must not enter
                // dispatch's legacy fail-open error branch.
                return Ok(false);
            };
            // Legacy dispatch treats backend errors as fail-open until #1283
            // replaces this cache-backed boundary. The exact check keeps
            // backend uncertainty typed while classifying local identity
            // rotation as definitively non-serving, without holding the
            // rotation gate across database I/O.
            self.exact_claim_is_held(room_jid, &fence).await
        })
    }

    fn check_exact_claim_fence<'a>(
        &'a self,
        room_jid: &'a BareJid,
        fence: &'a RoomClaimFenceContext,
    ) -> MucDurableFuture<'a, bool> {
        Box::pin(async move { self.exact_claim_is_held(room_jid, fence).await })
    }

    /// Exposes the typed `(Entity, ClaimEpoch, node_id)` triple published for
    /// the current live registry entry. Actor-derived work must instead use
    /// the immutable fence from its own actor snapshot.
    fn current_claim_fence(&self, room_jid: &BareJid) -> Option<RoomClaimFenceContext> {
        self.fence_for(room_jid).ok()
    }

    fn notify_previous_owner_demoted<'a>(
        &'a self,
        room_jid: &'a BareJid,
        previous_owner_node_id: &'a str,
        _previous_owner_node_epoch: &'a str,
        new_epoch: ClaimEpoch,
    ) -> MucDurableFuture<'a, ()> {
        Box::pin(async move {
            let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
            let mut relay_handle = RelayHandle::new(
                NodeId::new(previous_owner_node_id.to_string()),
                self.stop_token.clone(),
            );
            relay_handle
                .demote(entity, new_epoch)
                .await
                .map(|_reply| ())
                .map_err(|error| {
                    XmppError::internal(format!(
                        "best-effort Demote relay ask to the previous owner failed: {error}"
                    ))
                })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clustering::claims::{clustering_control_plane_table_lock, PostgresClaimStore};
    use crate::db::{DatabaseConfig, DatabaseDriver, DEFAULT_CONTROL_PLANE_POOL_SIZE};
    use kameo::{actor::Spawn, error::SendError};
    use std::sync::Arc;
    use waddle_xmpp::muc::room_actor::{
        GetSnapshot, RestoreDurableRoomState, RoomActor, SetSubject, SetSubjectError,
    };
    use waddle_xmpp::muc::RoomSubjectTexts;
    use waddle_xmpp::muc::{MucRoom, RoomConfig};
    use waddle_xmpp::ownership::{ClaimStore, NodeIdentity, StalePredicate};
    use waddle_xmpp::xep::xep0421::OccupantIdSecret;

    fn node_identity() -> NodeIdentity {
        NodeIdentity::new(
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
        )
    }

    async fn live_stealer(db: &crate::db::Database) -> NodeIdentity {
        let stealer = node_identity();
        let conn = db.guard().await.expect("guard");
        conn.execute(
            "INSERT INTO clustering_nodes (node_id, node_epoch, expired) VALUES (?, ?, false)",
            crate::db_params![stealer.node_id.clone(), stealer.node_epoch.clone()],
        )
        .await
        .expect("seed live stealer node");
        stealer
    }

    fn room_jid(name: &str) -> BareJid {
        format!("{name}@muc.example.com")
            .parse()
            .expect("valid test room JID")
    }

    fn unique_room_jid(prefix: &str) -> BareJid {
        format!("{prefix}-{}@muc.example.com", uuid::Uuid::new_v4())
            .parse()
            .expect("valid test room JID")
    }

    async fn wait_for_lock_waiter(db: &Database, query_fragment: &str) {
        let waiter = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let conn = db.guard().await.expect("monitor guard");
                let mut rows = conn
                    .query(
                        "SELECT COUNT(*) FROM pg_stat_activity WHERE wait_event_type = 'Lock' AND query LIKE ?",
                        crate::db_params![format!("%{query_fragment}%")],
                    )
                    .await
                    .expect("poll lock waiters");
                let count = rows
                    .next()
                    .await
                    .expect("read lock-waiter count")
                    .expect("lock-waiter count row")
                    .get::<i64>(0)
                    .expect("decode lock-waiter count");
                if count > 0 {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        })
        .await;
        assert!(
            waiter.is_ok(),
            "no blocked backend appeared for query fragment: {query_fragment:?}"
        );
    }

    #[test]
    fn fence_database_failures_preserve_the_exact_fence_entity() {
        let jid = room_jid("fence-db-unavailable");
        let entity = Entity::new(EntityType::RoomActor, jid.to_string());
        let fence = RoomClaimFenceContext::new(entity.clone(), node_identity(), ClaimEpoch(7));

        for error in [
            DatabaseError::ConnectionFailed("main pool unavailable".to_string()),
            DatabaseError::QueryFailed("claim-row iteration failed".to_string()),
        ] {
            assert!(
                matches!(
                    fence_db_unavailable(error, &fence),
                    XmppError::OwnershipUnavailable { entity: unavailable_entity }
                        if unavailable_entity == entity
                ),
                "database uncertainty must remain typed for the exact fence entity"
            );
        }
    }

    #[derive(Debug)]
    struct RetryablePgError {
        code: &'static str,
        constraint: Option<&'static str>,
    }

    impl std::fmt::Display for RetryablePgError {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(formatter, "test postgres error")
        }
    }

    impl std::error::Error for RetryablePgError {}

    impl sqlx::error::DatabaseError for RetryablePgError {
        fn message(&self) -> &str {
            "test postgres error"
        }

        fn code(&self) -> Option<std::borrow::Cow<'_, str>> {
            Some(std::borrow::Cow::Borrowed(self.code))
        }

        fn constraint(&self) -> Option<&str> {
            self.constraint
        }

        fn kind(&self) -> sqlx::error::ErrorKind {
            sqlx::error::ErrorKind::Other
        }

        fn as_error(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
            self
        }

        fn as_error_mut(&mut self) -> &mut (dyn std::error::Error + Send + Sync + 'static) {
            self
        }

        fn into_error(self: Box<Self>) -> Box<dyn std::error::Error + Send + Sync + 'static> {
            self
        }
    }

    fn retryable_database_error(
        code: &'static str,
        constraint: Option<&'static str>,
    ) -> DatabaseError {
        DatabaseError::Internal(sqlx::Error::Database(Box::new(RetryablePgError {
            code,
            constraint,
        })))
    }

    #[test]
    fn retry_classifier_accepts_transaction_retries_and_the_lifecycle_arbiter_only() {
        assert!(is_retryable_tx_error(&retryable_database_error(
            "40001", None
        )));
        assert!(is_retryable_tx_error(&retryable_database_error(
            "40P01", None
        )));
        assert!(is_retryable_tx_error(&retryable_database_error(
            "23505",
            Some("clustering_muc_room_lifecycles_live_room_idx"),
        )));
        assert!(!is_retryable_tx_error(&retryable_database_error(
            "23505",
            Some("other_unique")
        )));
        assert!(!is_retryable_tx_error(&retryable_database_error(
            "23503", None
        )));
    }

    #[tokio::test]
    async fn identity_rotation_invalidates_cached_room_fence_before_database_access() {
        let original = node_identity();
        let live_identity = SharedNodeIdentity::new(original.clone());
        let db = Database::from_config(
            "muc-identity-rotation-test",
            &DatabaseConfig::new(DatabaseDriver::Sqlite, "sqlite::memory:"),
        )
        .await
        .expect("open in-memory database");
        let store = PostgresMucRoomStore {
            db,
            node_identity: live_identity.clone(),
            stop_token: CancellationToken::new(),
            exact_claim_fences: DashMap::new(),
            published_claim_fences: DashMap::new(),
        };
        let jid = room_jid("identity-rotation");
        let entity = Entity::new(EntityType::RoomActor, jid.to_string());
        let fence = RoomClaimFenceContext::new(entity.clone(), original, ClaimEpoch(7));
        store.record_claim_fence(&jid, fence.clone());
        assert!(store.current_claim_fence(&jid).is_some());

        live_identity.rotate(node_identity()).await;
        let config = RoomConfig::default();

        assert!(matches!(
            store
                .commit_room_mutation(
                    &jid,
                    &fence,
                    RoomDurableMutation::Create {
                        waddle_id: "waddle".to_string(),
                        channel_id: "channel".to_string(),
                        config: config.clone(),
                        initial_affiliations: Vec::new(),
                    },
                )
                .await,
            Err(RoomCommitError::NotOwner)
        ));
        assert!(
            store.exact_claim_fences.get(&jid).is_none(),
            "the first fenced actor operation must evict the stale exact fence"
        );
        assert!(
            store.published_claim_fences.get(&jid).is_none(),
            "the first fenced actor operation must evict the stale published fence"
        );

        let mut tx = store.db.begin().await.expect("begin mismatch assertion");
        assert!(matches!(
            store.assert_fenced(&mut tx, &jid, &fence).await,
            Err(XmppError::OwnershipLost { entity: lost_entity })
                if lost_entity == entity
        ));

        assert!(!store
            .check_fenced_fanout(&jid)
            .await
            .expect("identity rotation is a non-serving fanout result"));
        assert!(!store
            .check_exact_claim_fence(&jid, &fence)
            .await
            .expect("identity rotation makes the actor fence non-serving"));
        assert!(store.current_claim_fence(&jid).is_none());
        assert!(store.exact_claim_fences.get(&jid).is_none());
        assert!(store.published_claim_fences.get(&jid).is_none());
        assert!(!store
            .check_fenced_fanout(&jid)
            .await
            .expect("an absent published fence remains non-serving"));
    }

    /// Open a clean `PostgresMucRoomStore` alongside a `PostgresClaimStore`
    /// sharing the same `Database`/tables, wiping every row this module's
    /// tests touch first. `None` (test skipped) when
    /// `WADDLE_TEST_POSTGRES_URL` is unset, mirroring every other
    /// Postgres-gated test in this workspace.
    async fn clean_store() -> Option<(
        PostgresMucRoomStore,
        Arc<PostgresClaimStore>,
        Database,
        NodeIdentity,
    )> {
        let url = std::env::var("WADDLE_TEST_POSTGRES_URL").ok()?;
        let db = Database::from_config(
            "muc-durable-test",
            &DatabaseConfig::new(DatabaseDriver::Postgres, url)
                .with_control_plane_pool(DEFAULT_CONTROL_PLANE_POOL_SIZE),
        )
        .await
        .expect("open test postgres");
        let claim_store = Arc::new(PostgresClaimStore::new(db.clone()));
        claim_store
            .ensure_schema()
            .await
            .expect("ensure claim schema");
        let me = node_identity();
        let store = PostgresMucRoomStore::open(
            db.clone(),
            CancellationToken::new(),
            SharedNodeIdentity::new(me.clone()),
        )
        .await
        .expect("open muc durable store");
        let conn = db.guard().await.expect("guard");
        conn.execute("DELETE FROM clustering_claims", ())
            .await
            .expect("clean claims");
        conn.execute("DELETE FROM clustering_nodes", ())
            .await
            .expect("clean nodes");
        conn.execute("DELETE FROM clustering_muc_rooms", ())
            .await
            .expect("clean rooms");
        conn.execute("DELETE FROM clustering_muc_room_affiliations", ())
            .await
            .expect("clean affiliations");
        conn.execute("DELETE FROM clustering_muc_room_lifecycles", ())
            .await
            .expect("clean room lifecycles");
        Some((store, claim_store, db, me))
    }

    /// Creates a shared Postgres handle after removing only the MUC durable
    /// schema, so concurrent `open` calls must bootstrap it from scratch.
    /// Callers hold `clustering_control_plane_table_lock` to avoid racing the
    /// existing Postgres-gated MUC tests that share this opt-in database.
    async fn fresh_muc_schema_database() -> Option<Database> {
        let url = std::env::var("WADDLE_TEST_POSTGRES_URL").ok()?;
        let db = Database::from_config(
            "muc-durable-fresh-schema-test",
            &DatabaseConfig::new(DatabaseDriver::Postgres, url)
                .with_control_plane_pool(DEFAULT_CONTROL_PLANE_POOL_SIZE),
        )
        .await
        .expect("open test postgres");
        let conn = db.guard().await.expect("guard");
        conn.execute("DROP TABLE IF EXISTS clustering_muc_room_lifecycles", ())
            .await
            .expect("drop room lifecycles");
        conn.execute("DROP TABLE IF EXISTS clustering_muc_room_affiliations", ())
            .await
            .expect("drop room affiliations");
        conn.execute("DROP TABLE IF EXISTS clustering_muc_rooms", ())
            .await
            .expect("drop rooms");
        Some(db)
    }

    async fn column_nullability(db: &Database, table: &str, column: &str) -> Option<String> {
        let conn = db.guard().await.expect("guard");
        let mut rows = conn
            .query(
                "SELECT is_nullable FROM information_schema.columns \
                 WHERE table_schema = current_schema() AND table_name = ? AND column_name = ?",
                crate::db_params![table, column],
            )
            .await
            .expect("query column nullability");
        rows.next()
            .await
            .expect("read column nullability")
            .map(|row| row.get(0).expect("decode column nullability"))
    }

    async fn named_check_exists(db: &Database, table: &str, constraint: &str) -> bool {
        let conn = db.guard().await.expect("guard");
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM pg_constraint \
                 WHERE conrelid = to_regclass(?) AND conname = ? AND contype = 'c'",
                crate::db_params![table, constraint],
            )
            .await
            .expect("query named check constraint");
        rows.next()
            .await
            .expect("read named check constraint")
            .expect("named check constraint count row")
            .get::<i64>(0)
            .expect("decode named check constraint count")
            == 1
    }

    #[tokio::test]
    async fn concurrent_open_serializes_muc_schema_bootstrap() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(db) = fresh_muc_schema_database().await else {
            return;
        };

        let (first, second) = tokio::join!(
            PostgresMucRoomStore::open(
                db.clone(),
                CancellationToken::new(),
                SharedNodeIdentity::new(node_identity()),
            ),
            PostgresMucRoomStore::open(
                db,
                CancellationToken::new(),
                SharedNodeIdentity::new(node_identity()),
            )
        );

        assert!(
            first.is_ok(),
            "first concurrent MUC schema open must succeed"
        );
        assert!(
            second.is_ok(),
            "second concurrent MUC schema open must succeed"
        );
    }

    /// Poll `pg_locks` until a transaction is queued (ungranted) on the MUC
    /// schema advisory lock, mirroring
    /// `db::migrations::tests::wait_for_postgres_advisory_waiter`: Postgres
    /// exposes a single-bigint advisory key with its high 32 bits in
    /// `classid`, low 32 bits in `objid`, and `objsubid = 1`.
    async fn wait_for_muc_schema_advisory_waiter(db: &Database) {
        let waiter = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let conn = db.guard().await.expect("guard");
                let mut rows = conn
                    .query(
                        "SELECT COUNT(*) \
                         FROM pg_locks \
                         WHERE locktype = 'advisory' \
                           AND granted = false \
                           AND objsubid = 1 \
                           AND ((classid::bigint << 32) | objid::bigint) = ?",
                        crate::db_params![MUC_SCHEMA_ADVISORY_LOCK_KEY],
                    )
                    .await
                    .expect("query MUC schema advisory lock waiters");
                let count = rows
                    .next()
                    .await
                    .expect("read MUC schema advisory lock waiters")
                    .expect("advisory waiter count row")
                    .get::<i64>(0)
                    .expect("decode advisory waiter count");
                if count > 0 {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        })
        .await;
        assert!(
            waiter.is_ok(),
            "MUC schema bootstrap must show an ungranted advisory waiter in pg_locks"
        );
    }

    #[tokio::test]
    async fn first_writer_create_mints_revision_one_and_back_links() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, _db, me)) = clean_store().await else {
            return;
        };
        let room_jid = unique_room_jid("lane-a2-first-write");
        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        let epoch = claim_store
            .ensure_claimed(&entity, &me)
            .await
            .expect("claim");
        let fence = RoomClaimFenceContext::new(entity, me, epoch);
        store.record_claim_fence(&room_jid, fence.clone());

        let config = RoomConfig {
            name: "first-writer".to_string(),
            ..RoomConfig::default()
        };
        let owner: BareJid = "owner@example.com".parse().expect("valid jid");
        let coords = store
            .commit_room_mutation(
                &room_jid,
                &fence,
                RoomDurableMutation::Create {
                    waddle_id: "waddle-first".to_string(),
                    channel_id: "channel-first".to_string(),
                    config: config.clone(),
                    initial_affiliations: vec![waddle_xmpp::muc::durable::AffiliationEntry::new(
                        owner.clone(),
                        Some(Affiliation::Owner),
                    )],
                },
            )
            .await
            .expect("first write must commit");

        assert_eq!(coords.revision, RoomRevision::initial());

        let conn = store.db.guard().await.expect("guard");
        let mut lifecycle_rows = conn
            .query(
                "SELECT lifecycle_id, revision, state FROM clustering_muc_room_lifecycles WHERE room_jid = ?",
                crate::db_params![room_jid.to_string()],
            )
            .await
            .expect("query lifecycle row");
        let lifecycle_row = lifecycle_rows
            .next()
            .await
            .expect("lifecycle row exists")
            .expect("lifecycle row");
        let lifecycle_id: String = lifecycle_row.get(0).expect("lifecycle id");
        let revision: i64 = lifecycle_row.get(1).expect("lifecycle revision");
        let state: String = lifecycle_row.get(2).expect("lifecycle state");
        assert_eq!(coords.lifecycle.to_string(), lifecycle_id);
        assert_eq!(revision, RoomRevision::initial().as_i64());
        assert_eq!(state, "active");

        let mut room_rows = conn
            .query(
                "SELECT lifecycle_id, revision FROM clustering_muc_rooms WHERE room_jid = ?",
                crate::db_params![room_jid.to_string()],
            )
            .await
            .expect("query room row");
        let room_row = room_rows
            .next()
            .await
            .expect("room row exists")
            .expect("room row");
        let room_lifecycle_id: String = room_row.get(0).expect("room lifecycle id");
        let room_revision: i64 = room_row.get(1).expect("room revision");
        assert_eq!(room_lifecycle_id, coords.lifecycle.to_string());
        let room_revision =
            RoomRevision::from_stored(room_revision).expect("room revision must be decodable");
        assert_eq!(room_revision, coords.revision);

        let mut affiliation_rows = conn
            .query(
                "SELECT count(*) FROM clustering_muc_room_affiliations WHERE room_jid = ? AND member_jid = ? AND affiliation = ?",
                crate::db_params![room_jid.to_string(), owner.to_string(), "owner"],
            )
            .await
            .expect("query affiliation rows");
        let affiliation_count: i64 = affiliation_rows
            .next()
            .await
            .expect("affiliation row count")
            .expect("affiliation row count row")
            .get(0)
            .expect("decode affiliation row count");
        assert_eq!(affiliation_count, 1);
    }

    #[tokio::test]
    async fn non_create_mutations_require_matching_lifecycle_and_dormant_state_is_not_mutable() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, _db, me)) = clean_store().await else {
            return;
        };
        let room_jid = unique_room_jid("lane-a2-state-missing");
        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        let epoch = claim_store
            .ensure_claimed(&entity, &me)
            .await
            .expect("claim");
        let fence = RoomClaimFenceContext::new(entity, me, epoch);
        store.record_claim_fence(&room_jid, fence.clone());

        for intent in [
            RoomDurableMutation::Config {
                config: RoomConfig::default(),
                waddle_id: "waddle".to_string(),
                channel_id: "channel".to_string(),
            },
            RoomDurableMutation::Subject(Some(SubjectState {
                texts: RoomSubjectTexts::from_iter([(String::new(), "subject".to_string())]),
                setter: "alice@example.com".parse().expect("valid jid"),
                setter_nick: "alice".to_string(),
                set_at: chrono::Utc::now(),
            })),
            RoomDurableMutation::Affiliation(waddle_xmpp::muc::durable::AffiliationEntry::new(
                "bob@example.com".parse().expect("valid jid"),
                Some(Affiliation::Member),
            )),
            RoomDurableMutation::AffiliationBatch(vec![
                waddle_xmpp::muc::durable::AffiliationEntry::new(
                    "carol@example.com".parse().expect("valid jid"),
                    Some(Affiliation::Member),
                ),
            ]),
            RoomDurableMutation::MembersOnlyEnforcement {
                config: RoomConfig::default(),
                affiliations: vec![waddle_xmpp::muc::durable::AffiliationEntry::new(
                    "dave@example.com".parse().expect("valid jid"),
                    Some(Affiliation::Member),
                )],
            },
            RoomDurableMutation::MediatedInviteGrant(
                waddle_xmpp::muc::durable::AffiliationEntry::new(
                    "erin@example.com".parse().expect("valid jid"),
                    Some(Affiliation::Member),
                ),
            ),
            RoomDurableMutation::MediatedInviteRollback(
                waddle_xmpp::muc::durable::AffiliationEntry::new(
                    "frank@example.com".parse().expect("valid jid"),
                    Some(Affiliation::Outcast),
                ),
            ),
            RoomDurableMutation::Dormancy,
            RoomDurableMutation::Activate,
            RoomDurableMutation::Destroy,
        ] {
            assert!(
                matches!(
                    store.commit_room_mutation(&room_jid, &fence, intent).await,
                    Err(RoomCommitError::StateMissing)
                ),
                "non-create mutation on a missing lifecycle must return StateMissing"
            );
        }

        store
            .commit_room_mutation(
                &room_jid,
                &fence,
                RoomDurableMutation::Create {
                    waddle_id: "waddle".to_string(),
                    channel_id: "channel".to_string(),
                    config: RoomConfig::default(),
                    initial_affiliations: vec![],
                },
            )
            .await
            .expect("create seeds lifecycle");
        store
            .commit_room_mutation(&room_jid, &fence, RoomDurableMutation::Dormancy)
            .await
            .expect("dormancy transition");

        assert!(
            matches!(
                store
                    .commit_room_mutation(
                        &room_jid,
                        &fence,
                        RoomDurableMutation::Config {
                            config: RoomConfig::default(),
                            waddle_id: "waddle".to_string(),
                            channel_id: "channel".to_string(),
                        },
                    )
                    .await,
                Err(RoomCommitError::StateMissing)
            ),
            "ordinary mutation must not mutate a dormant room"
        );
    }

    #[tokio::test]
    async fn activate_transitions_dormant_to_active_adopts_pre_lifecycle_rooms_and_rejects_missing_state(
    ) {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, db, me)) = clean_store().await else {
            return;
        };
        let room_jid = unique_room_jid("lane-a2-activate");
        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        let epoch = claim_store
            .ensure_claimed(&entity, &me)
            .await
            .expect("claim");
        let fence = RoomClaimFenceContext::new(entity, me.clone(), epoch);
        store.record_claim_fence(&room_jid, fence.clone());

        store
            .commit_room_mutation(
                &room_jid,
                &fence,
                RoomDurableMutation::Create {
                    waddle_id: "waddle-activate".to_string(),
                    channel_id: "channel-activate".to_string(),
                    config: RoomConfig::default(),
                    initial_affiliations: vec![],
                },
            )
            .await
            .expect("seed room");
        let dormant = store
            .commit_room_mutation(&room_jid, &fence, RoomDurableMutation::Dormancy)
            .await
            .expect("dormancy transition");
        assert_eq!(dormant.revision.as_i64(), 2);

        let active = store
            .commit_room_mutation(&room_jid, &fence, RoomDurableMutation::Activate)
            .await
            .expect("activate transition");
        assert_eq!(active.revision.as_i64(), 3);

        let mut rows = db
            .guard()
            .await
            .expect("guard")
            .query(
                "SELECT revision, state FROM clustering_muc_room_lifecycles WHERE room_jid = ?",
                crate::db_params![room_jid.to_string()],
            )
            .await
            .expect("query lifecycle row");
        let row = rows
            .next()
            .await
            .expect("lifecycle row")
            .expect("lifecycle row");
        let revision: i64 = row.get(0).expect("revision");
        let state: String = row.get(1).expect("state");
        assert_eq!(revision, 3);
        assert_eq!(state, "active");

        let mut room_rows = db
            .guard()
            .await
            .expect("guard")
            .query(
                "SELECT lifecycle_id, revision FROM clustering_muc_rooms WHERE room_jid = ?",
                crate::db_params![room_jid.to_string()],
            )
            .await
            .expect("query activated room back-link");
        let room_row = room_rows
            .next()
            .await
            .expect("activated room row exists")
            .expect("activated room row");
        assert_eq!(
            room_row.get::<String>(0).expect("room lifecycle id"),
            active.lifecycle.to_string()
        );
        assert_eq!(room_row.get::<i64>(1).expect("room revision"), 3);

        let reactivated = store
            .commit_room_mutation(&room_jid, &fence, RoomDurableMutation::Activate)
            .await
            .expect("re-activating an active lifecycle is idempotent");
        assert_eq!(reactivated.lifecycle, active.lifecycle);
        assert_eq!(
            reactivated.revision.as_i64(),
            3,
            "idempotent activation must not bump the revision"
        );

        let adoptable = unique_room_jid("lane-c3-activate-adoption");
        let adoptable_entity = Entity::new(EntityType::RoomActor, adoptable.to_string());
        let adoptable_epoch = claim_store
            .ensure_claimed(&adoptable_entity, &me)
            .await
            .expect("adoptable room claim");
        let adoptable_fence =
            RoomClaimFenceContext::new(adoptable_entity, me.clone(), adoptable_epoch);
        store.record_claim_fence(&adoptable, adoptable_fence.clone());
        db.guard()
            .await
            .expect("guard")
            .execute(
                "INSERT INTO clustering_muc_rooms (room_jid, waddle_id, channel_id, config_json) VALUES (?, ?, ?, ?)",
                crate::db_params![
                    adoptable.to_string(),
                    "waddle-adopt",
                    "channel-adopt",
                    "{}"
                ],
            )
            .await
            .expect("seed pre-lifecycle room row");
        let adopted = store
            .commit_room_mutation(&adoptable, &adoptable_fence, RoomDurableMutation::Activate)
            .await
            .expect("activate adopts a pre-lifecycle room row");
        assert_eq!(adopted.revision.as_i64(), 1);

        let mut adoption_rows = db
            .guard()
            .await
            .expect("guard")
            .query(
                "SELECT lifecycle_id, revision, state FROM clustering_muc_room_lifecycles WHERE room_jid = ?",
                crate::db_params![adoptable.to_string()],
            )
            .await
            .expect("query adopted lifecycle row");
        let adoption_row = adoption_rows
            .next()
            .await
            .expect("read adopted lifecycle row")
            .expect("adopted lifecycle row");
        assert_eq!(
            adoption_row.get::<String>(0).expect("lifecycle id"),
            adopted.lifecycle.to_string()
        );
        assert_eq!(adoption_row.get::<i64>(1).expect("revision"), 1);
        assert_eq!(adoption_row.get::<String>(2).expect("state"), "active");

        let mut adoption_room_rows = db
            .guard()
            .await
            .expect("guard")
            .query(
                "SELECT lifecycle_id, revision FROM clustering_muc_rooms WHERE room_jid = ?",
                crate::db_params![adoptable.to_string()],
            )
            .await
            .expect("query adopted room back-links");
        let adoption_room_row = adoption_room_rows
            .next()
            .await
            .expect("read adopted room back-links")
            .expect("adopted room row");
        assert_eq!(
            adoption_room_row
                .get::<String>(0)
                .expect("room lifecycle id"),
            adopted.lifecycle.to_string()
        );
        assert_eq!(adoption_room_row.get::<i64>(1).expect("room revision"), 1);

        let missing = unique_room_jid("lane-a2-activate-missing");
        let missing_entity = Entity::new(EntityType::RoomActor, missing.to_string());
        let missing_epoch = claim_store
            .ensure_claimed(&missing_entity, &me)
            .await
            .expect("missing room claim");
        let missing_fence = RoomClaimFenceContext::new(missing_entity, me, missing_epoch);
        store.record_claim_fence(&missing, missing_fence.clone());
        assert!(
            matches!(
                store
                    .commit_room_mutation(&missing, &missing_fence, RoomDurableMutation::Activate)
                    .await,
                Err(RoomCommitError::StateMissing)
            ),
            "activating a missing lifecycle must remain StateMissing"
        );
    }

    #[tokio::test]
    async fn concurrent_create_is_idempotent_for_matching_life_identity_and_conflicts_on_different_ids(
    ) {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, db, me)) = clean_store().await else {
            return;
        };
        let room_jid = unique_room_jid("lane-a2-concurrent-create");
        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        let epoch = claim_store
            .ensure_claimed(&entity, &me)
            .await
            .expect("claim");
        let fence = RoomClaimFenceContext::new(entity.clone(), me.clone(), epoch);
        let store = Arc::new(store);
        store.record_claim_fence(&room_jid, fence.clone());
        let intent = RoomDurableMutation::Create {
            waddle_id: "waddle-concurrent".to_string(),
            channel_id: "channel-concurrent".to_string(),
            config: RoomConfig::default(),
            initial_affiliations: vec![],
        };
        let conn = db.guard().await.expect("guard");
        conn.execute(
            "DROP TRIGGER IF EXISTS lane_a2_delay_lifecycle_insert ON clustering_muc_room_lifecycles",
            (),
        )
        .await
        .expect("remove stale concurrent-create delay trigger");
        conn.execute(
            "DROP FUNCTION IF EXISTS lane_a2_delay_lifecycle_insert()",
            (),
        )
        .await
        .expect("remove stale concurrent-create delay function");
        conn.execute(
            r#"
            CREATE OR REPLACE FUNCTION lane_a2_delay_lifecycle_insert()
            RETURNS trigger AS $$
            BEGIN
                PERFORM pg_sleep(0.2);
                RETURN NEW;
            END;
            $$ LANGUAGE plpgsql
            "#,
            (),
        )
        .await
        .expect("create concurrent-create delay function");
        conn.execute(
            "CREATE TRIGGER lane_a2_delay_lifecycle_insert BEFORE INSERT ON clustering_muc_room_lifecycles FOR EACH ROW EXECUTE FUNCTION lane_a2_delay_lifecycle_insert()",
            (),
        )
        .await
        .expect("create concurrent-create delay trigger");
        let store_a = Arc::clone(&store);
        let store_b = Arc::clone(&store);
        let jid_a = room_jid.clone();
        let jid_b = room_jid.clone();
        let fence_a = fence.clone();
        let fence_b = fence.clone();
        let intent_a = intent.clone();
        let intent_b = intent.clone();
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let barrier_a = Arc::clone(&barrier);
        let barrier_b = Arc::clone(&barrier);

        let first = tokio::spawn(async move {
            barrier_a.wait().await;
            store_a
                .commit_room_mutation(&jid_a, &fence_a, intent_a)
                .await
                .expect("first create")
        });
        let second = tokio::spawn(async move {
            barrier_b.wait().await;
            store_b
                .commit_room_mutation(&jid_b, &fence_b, intent_b)
                .await
                .expect("second create")
        });

        let first_result = first.await;
        let second_result = second.await;
        conn.execute(
            "DROP TRIGGER lane_a2_delay_lifecycle_insert ON clustering_muc_room_lifecycles",
            (),
        )
        .await
        .expect("drop concurrent-create delay trigger");
        conn.execute("DROP FUNCTION lane_a2_delay_lifecycle_insert()", ())
            .await
            .expect("drop concurrent-create delay function");
        let first_coords = first_result.expect("first create task");
        let second_coords = second_result.expect("second create task");
        assert_eq!(
            first_coords, second_coords,
            "concurrent creates with same ids must return idempotent winner coordinates"
        );

        assert!(matches!(
            store
                .commit_room_mutation(
                    &room_jid,
                    &fence,
                    RoomDurableMutation::Create {
                        waddle_id: "waddle-mismatch".to_string(),
                        channel_id: "channel-mismatch".to_string(),
                        config: RoomConfig::default(),
                        initial_affiliations: vec![],
                    },
                )
                .await,
            Err(RoomCommitError::CreateConflict)
        ));

        store
            .commit_room_mutation(&room_jid, &fence, RoomDurableMutation::Dormancy)
            .await
            .expect("make the lifecycle dormant");
        let dormant_coordinates = store
            .commit_room_mutation(&room_jid, &fence, intent)
            .await
            .expect("matching create is idempotent for a dormant live lifecycle");
        assert_eq!(dormant_coordinates.revision.as_i64(), 2);
        assert!(matches!(
            store
                .commit_room_mutation(
                    &room_jid,
                    &fence,
                    RoomDurableMutation::Create {
                        waddle_id: "waddle-mismatch".to_string(),
                        channel_id: "channel-mismatch".to_string(),
                        config: RoomConfig::default(),
                        initial_affiliations: vec![],
                    },
                )
                .await,
            Err(RoomCommitError::CreateConflict)
        ));
    }

    #[tokio::test]
    async fn revision_overflow_does_not_apply_any_commit_data() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, db, me)) = clean_store().await else {
            return;
        };
        let room_jid = unique_room_jid("lane-a2-revision-overflow");
        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        let epoch = claim_store
            .ensure_claimed(&entity, &me)
            .await
            .expect("claim");
        let fence = RoomClaimFenceContext::new(entity.clone(), me.clone(), epoch);
        store.record_claim_fence(&room_jid, fence.clone());
        let lifecycle_id = uuid::Uuid::new_v4().to_string();
        let overflow = i64::MAX;
        let original_config =
            serde_json::to_string(&RoomConfig::default()).expect("encode default config");
        let conn = db.guard().await.expect("guard");
        conn.execute(
            "INSERT INTO clustering_muc_room_lifecycles (lifecycle_id, room_jid, revision, state) VALUES (?, ?, ?, ?)",
            crate::db_params![lifecycle_id.clone(), room_jid.to_string(), overflow, "active"],
        )
        .await
        .expect("seed max revision lifecycle");
        conn.execute(
            "INSERT INTO clustering_muc_rooms (room_jid, waddle_id, channel_id, config_json, lifecycle_id, revision) VALUES (?, ?, ?, ?, ?, ?)",
            crate::db_params![
                room_jid.to_string(),
                "waddle-overflow",
                "channel-overflow",
                original_config.clone(),
                lifecycle_id.clone(),
                overflow
            ],
        )
        .await
        .expect("seed room row");

        assert!(
            matches!(
                store
                    .commit_room_mutation(
                        &room_jid,
                        &fence,
                        RoomDurableMutation::Config {
                            config: RoomConfig {
                                name: "updated".to_string(),
                                ..RoomConfig::default()
                            },
                            waddle_id: "waddle-overflow".to_string(),
                            channel_id: "channel-overflow".to_string(),
                        },
                    )
                    .await,
                Err(RoomCommitError::RevisionOverflow)
            ),
            "max revision must overflow before any state write"
        );

        let mut room_rows = conn
            .query(
                "SELECT revision, lifecycle_id, config_json FROM clustering_muc_rooms WHERE room_jid = ?",
                crate::db_params![room_jid.to_string()],
            )
            .await
            .expect("query room row after overflow");
        let room_row = room_rows
            .next()
            .await
            .expect("room row exists")
            .expect("room row");
        let room_revision: i64 = room_row.get(0).expect("room revision");
        let room_lifecycle_id: String = room_row.get(1).expect("room lifecycle id");
        let room_config: String = room_row.get(2).expect("room config");
        assert_eq!(room_revision, overflow);
        assert_eq!(room_lifecycle_id, lifecycle_id);
        assert_eq!(room_config, original_config);

        let mut lifecycle_rows = conn
            .query(
                "SELECT revision, state FROM clustering_muc_room_lifecycles WHERE room_jid = ?",
                crate::db_params![room_jid.to_string()],
            )
            .await
            .expect("query lifecycle row after overflow");
        let lifecycle_row = lifecycle_rows
            .next()
            .await
            .expect("lifecycle row exists")
            .expect("lifecycle row");
        assert_eq!(
            lifecycle_row.get::<i64>(0).expect("lifecycle revision"),
            overflow
        );
        assert_eq!(
            lifecycle_row.get::<String>(1).expect("lifecycle state"),
            "active"
        );
    }

    #[tokio::test]
    async fn lock_ordering_uses_claim_then_lifecycle_locking_and_serializes_concurrent_mutations() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, db, me)) = clean_store().await else {
            return;
        };
        let store = Arc::new(store);
        let room_jid = unique_room_jid("lane-a2-lock-order");
        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        let epoch = claim_store
            .ensure_claimed(&entity, &me)
            .await
            .expect("claim");
        let fence = RoomClaimFenceContext::new(entity, me.clone(), epoch);
        store.record_claim_fence(&room_jid, fence.clone());
        store
            .commit_room_mutation(
                &room_jid,
                &fence,
                RoomDurableMutation::Create {
                    waddle_id: "waddle-lock".to_string(),
                    channel_id: "channel-lock".to_string(),
                    config: RoomConfig::default(),
                    initial_affiliations: vec![],
                },
            )
            .await
            .expect("seed room");

        // normal mutation blocks behind a FOR UPDATE claim holder while it takes FOR SHARE
        let mut blocker = db.begin().await.expect("begin blocker transaction");
        let mut lock_rows = blocker
            .query(
                "SELECT 1 FROM clustering_claims WHERE entity = ? FOR UPDATE",
                crate::db_params![room_entity_key(&room_jid)],
            )
            .await
            .expect("acquire claim lock");
        assert!(lock_rows.next().await.expect("claim lock row").is_some());
        drop(lock_rows);
        let mut lifecycle_blocker = db
            .begin()
            .await
            .expect("begin lifecycle blocker transaction");
        let mut lifecycle_lock_rows = lifecycle_blocker
            .query(
                "SELECT 1 FROM clustering_muc_room_lifecycles WHERE room_jid = ? FOR UPDATE",
                crate::db_params![room_jid.to_string()],
            )
            .await
            .expect("acquire lifecycle lock");
        assert!(lifecycle_lock_rows
            .next()
            .await
            .expect("lifecycle lock row")
            .is_some());
        drop(lifecycle_lock_rows);

        let share_blocker = tokio::spawn({
            let room_jid = room_jid.clone();
            let fence = fence.clone();
            let store = Arc::clone(&store);
            async move {
                store
                    .commit_room_mutation(
                        &room_jid,
                        &fence,
                        RoomDurableMutation::Config {
                            config: RoomConfig {
                                name: "blocked".to_string(),
                                ..RoomConfig::default()
                            },
                            waddle_id: "waddle-lock".to_string(),
                            channel_id: "channel-lock".to_string(),
                        },
                    )
                    .await
                    .expect("blocked mutation completes after release")
            }
        });
        wait_for_lock_waiter(&db, "FOR SHARE").await;
        assert!(
            !share_blocker.is_finished(),
            "normal mutation must wait on the claim before it can lock the lifecycle"
        );
        blocker
            .commit()
            .await
            .expect("release claim FOR UPDATE holder");
        wait_for_lock_waiter(
            &db,
            "SELECT lifecycle_id, revision, state FROM clustering_muc_room_lifecycles",
        )
        .await;
        assert!(
            !share_blocker.is_finished(),
            "normal mutation must reach the lifecycle lock only after the claim is released"
        );
        lifecycle_blocker
            .commit()
            .await
            .expect("release lifecycle blocker");
        share_blocker.await.expect("blocked commit task");

        // destroy/dormancy path blocks behind FOR SHARE claim holders (FOR UPDATE assert)
        let mut blocker = db.begin().await.expect("begin blocker transaction");
        let mut share_lock_rows = blocker
            .query(
                "SELECT 1 FROM clustering_claims WHERE entity = ? FOR SHARE",
                crate::db_params![room_entity_key(&room_jid)],
            )
            .await
            .expect("acquire claim SHARE lock");
        assert!(share_lock_rows
            .next()
            .await
            .expect("claim share lock row")
            .is_some());
        drop(share_lock_rows);
        let update = tokio::spawn({
            let room_jid = room_jid.clone();
            let fence = fence.clone();
            let store = Arc::clone(&store);
            async move {
                store
                    .commit_room_mutation(&room_jid, &fence, RoomDurableMutation::Dormancy)
                    .await
                    .expect("dormancy follows lock order")
            }
        });
        wait_for_lock_waiter(&db, "FOR UPDATE").await;
        blocker.commit().await.expect("release share lock holder");
        update.await.expect("dormancy task finishes");

        let mut blocker = db.begin().await.expect("begin destroy blocker transaction");
        let mut share_lock_rows = blocker
            .query(
                "SELECT 1 FROM clustering_claims WHERE entity = ? FOR SHARE",
                crate::db_params![room_entity_key(&room_jid)],
            )
            .await
            .expect("acquire destroy claim SHARE lock");
        assert!(share_lock_rows
            .next()
            .await
            .expect("destroy claim share lock row")
            .is_some());
        drop(share_lock_rows);
        let destroy = tokio::spawn({
            let room_jid = room_jid.clone();
            let fence = fence.clone();
            let store = Arc::clone(&store);
            async move {
                store
                    .commit_room_mutation(&room_jid, &fence, RoomDurableMutation::Destroy)
                    .await
                    .expect("destroy follows lock order")
            }
        });
        wait_for_lock_waiter(&db, "FOR UPDATE").await;
        assert!(
            !destroy.is_finished(),
            "destroy must wait behind the shared claim holder"
        );
        blocker
            .commit()
            .await
            .expect("release destroy share lock holder");
        destroy.await.expect("destroy task finishes");

        let serialized_jid = unique_room_jid("lane-a2-lifecycle-serialization");
        let serialized_entity = Entity::new(EntityType::RoomActor, serialized_jid.to_string());
        let serialized_epoch = claim_store
            .ensure_claimed(&serialized_entity, &me)
            .await
            .expect("serialized room claim");
        let serialized_fence =
            RoomClaimFenceContext::new(serialized_entity, me.clone(), serialized_epoch);
        store.record_claim_fence(&serialized_jid, serialized_fence.clone());
        store
            .commit_room_mutation(
                &serialized_jid,
                &serialized_fence,
                RoomDurableMutation::Create {
                    waddle_id: "waddle-serialized".to_string(),
                    channel_id: "channel-serialized".to_string(),
                    config: RoomConfig::default(),
                    initial_affiliations: vec![],
                },
            )
            .await
            .expect("seed serialized room");

        let (start_a, start_b) = {
            let barrier = Arc::new(tokio::sync::Barrier::new(2));
            let store_a = Arc::clone(&store);
            let store_b = Arc::clone(&store);
            let barrier_a = Arc::clone(&barrier);
            let barrier_b = Arc::clone(&barrier);
            let room_a = serialized_jid.clone();
            let room_b = serialized_jid.clone();
            let fence_a = serialized_fence.clone();
            let fence_b = serialized_fence.clone();
            (
                tokio::spawn(async move {
                    barrier_a.wait().await;
                    store_a
                        .commit_room_mutation(
                            &room_a,
                            &fence_a,
                            RoomDurableMutation::Config {
                                config: RoomConfig {
                                    name: "first".to_string(),
                                    ..RoomConfig::default()
                                },
                                waddle_id: "waddle-serialized".to_string(),
                                channel_id: "channel-serialized".to_string(),
                            },
                        )
                        .await
                        .expect("first concurrent mutation")
                        .revision
                        .as_i64()
                }),
                tokio::spawn(async move {
                    barrier_b.wait().await;
                    store_b
                        .commit_room_mutation(
                            &room_b,
                            &fence_b,
                            RoomDurableMutation::Config {
                                config: RoomConfig {
                                    name: "second".to_string(),
                                    ..RoomConfig::default()
                                },
                                waddle_id: "waddle-serialized".to_string(),
                                channel_id: "channel-serialized".to_string(),
                            },
                        )
                        .await
                        .expect("second concurrent mutation")
                        .revision
                        .as_i64()
                }),
            )
        };
        let first_revision = start_a.await.expect("first concurrent mutation join");
        let second_revision = start_b.await.expect("second concurrent mutation join");
        let mut revisions = [first_revision, second_revision];
        revisions.sort_unstable();
        assert_eq!(
            revisions,
            [
                RoomRevision::from_stored(2).expect("revision 2").as_i64(),
                3
            ]
        );
    }

    #[tokio::test]
    async fn claim_lost_mid_mutation_is_not_owner_and_leaves_no_new_state() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, db, me)) = clean_store().await else {
            return;
        };
        let room_jid = unique_room_jid("lane-a2-claim-lost");
        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        let epoch = claim_store
            .ensure_claimed(&entity, &me)
            .await
            .expect("claim");
        let fence = RoomClaimFenceContext::new(entity, me, epoch);
        store.record_claim_fence(&room_jid, fence.clone());

        db.guard()
            .await
            .expect("guard")
            .execute(
                "DELETE FROM clustering_claims WHERE entity = ?",
                crate::db_params![room_entity_key(&room_jid)],
            )
            .await
            .expect("delete claim row");

        assert!(
            matches!(
                store
                    .commit_room_mutation(
                        &room_jid,
                        &fence,
                        RoomDurableMutation::Create {
                            waddle_id: "waddle-lost".to_string(),
                            channel_id: "channel-lost".to_string(),
                            config: RoomConfig::default(),
                            initial_affiliations: vec![],
                        },
                    )
                    .await,
                Err(RoomCommitError::NotOwner)
            ),
            "a deleted claim row must be treated as not owner"
        );

        let mut room_rows = db
            .guard()
            .await
            .expect("guard")
            .query(
                "SELECT count(*) FROM clustering_muc_rooms WHERE room_jid = ?",
                crate::db_params![room_jid.to_string()],
            )
            .await
            .expect("query room rows");
        let room_count: i64 = room_rows
            .next()
            .await
            .expect("room row count")
            .expect("room row count row")
            .get(0)
            .expect("decode room row count");
        assert_eq!(room_count, 0);
        let mut lifecycle_rows = db
            .guard()
            .await
            .expect("guard")
            .query(
                "SELECT count(*) FROM clustering_muc_room_lifecycles WHERE room_jid = ?",
                crate::db_params![room_jid.to_string()],
            )
            .await
            .expect("query lifecycle rows");
        let lifecycle_count: i64 = lifecycle_rows
            .next()
            .await
            .expect("lifecycle row count")
            .expect("lifecycle row count row")
            .get(0)
            .expect("decode lifecycle row count");
        assert_eq!(lifecycle_count, 0);
    }

    #[tokio::test]
    async fn revision_monotonicity_across_intents_tracks_coordinates_and_back_links() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, _db, me)) = clean_store().await else {
            return;
        };
        let room_jid = unique_room_jid("lane-a2-revision-mono");
        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        let epoch = claim_store
            .ensure_claimed(&entity, &me)
            .await
            .expect("claim");
        let fence = RoomClaimFenceContext::new(entity, me, epoch);
        store.record_claim_fence(&room_jid, fence.clone());

        store
            .commit_room_mutation(
                &room_jid,
                &fence,
                RoomDurableMutation::Create {
                    waddle_id: "waddle-mono".to_string(),
                    channel_id: "channel-mono".to_string(),
                    config: RoomConfig {
                        name: "first".to_string(),
                        ..RoomConfig::default()
                    },
                    initial_affiliations: vec![],
                },
            )
            .await
            .expect("seed room");

        let config = store
            .commit_room_mutation(
                &room_jid,
                &fence,
                RoomDurableMutation::Config {
                    config: RoomConfig {
                        name: "second".to_string(),
                        ..RoomConfig::default()
                    },
                    waddle_id: "waddle-mono".to_string(),
                    channel_id: "channel-mono".to_string(),
                },
            )
            .await
            .expect("config revision bump");
        assert_eq!(config.revision.as_i64(), 2);

        let subject = store
            .commit_room_mutation(
                &room_jid,
                &fence,
                RoomDurableMutation::Subject(Some(SubjectState {
                    texts: RoomSubjectTexts::from_iter([(String::new(), "hello".to_string())]),
                    setter: "alice@example.com".parse().expect("valid jid"),
                    setter_nick: "alice".to_string(),
                    set_at: chrono::Utc::now(),
                })),
            )
            .await
            .expect("subject revision bump");
        assert_eq!(subject.revision.as_i64(), 3);

        let affiliation = store
            .commit_room_mutation(
                &room_jid,
                &fence,
                RoomDurableMutation::Affiliation(waddle_xmpp::muc::durable::AffiliationEntry::new(
                    "alice@example.com".parse().expect("valid jid"),
                    Some(Affiliation::Owner),
                )),
            )
            .await
            .expect("affiliation revision bump");
        assert_eq!(affiliation.revision.as_i64(), 4);

        let coords = room_jid;
        let conn = store.db.guard().await.expect("guard");
        let mut rows = conn
            .query(
                "SELECT lifecycle_id, revision FROM clustering_muc_rooms WHERE room_jid = ?",
                crate::db_params![coords.to_string()],
            )
            .await
            .expect("query back-link");
        let row = rows
            .next()
            .await
            .expect("room row exists")
            .expect("room row");
        let row_revision: i64 = row.get(1).expect("room revision");
        assert_eq!(row_revision, 4);
        let lifecycle_id: String = row.get(0).expect("room lifecycle id");
        assert_eq!(lifecycle_id, affiliation.lifecycle.to_string());
    }

    #[tokio::test]
    async fn open_blocks_until_the_muc_schema_advisory_lock_is_released() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(db) = fresh_muc_schema_database().await else {
            return;
        };

        let mut lock_tx = db.begin().await.expect("start lock transaction");
        lock_tx
            .query(
                "SELECT pg_advisory_xact_lock(?)",
                crate::db_params![MUC_SCHEMA_ADVISORY_LOCK_KEY],
            )
            .await
            .expect("take MUC schema advisory lock");

        let open_db = db.clone();
        let mut open = tokio::spawn(async move {
            PostgresMucRoomStore::open(
                open_db,
                CancellationToken::new(),
                SharedNodeIdentity::new(node_identity()),
            )
            .await
        });

        wait_for_muc_schema_advisory_waiter(&db).await;
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut open)
                .await
                .is_err(),
            "MUC schema bootstrap must block while another transaction holds its advisory lock"
        );

        lock_tx
            .commit()
            .await
            .expect("release MUC schema advisory lock");
        open.await
            .expect("MUC schema open task")
            .expect("MUC schema open must succeed once the advisory lock is released");
    }

    #[tokio::test]
    async fn repeated_open_is_idempotent() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(db) = fresh_muc_schema_database().await else {
            return;
        };

        PostgresMucRoomStore::open(
            db.clone(),
            CancellationToken::new(),
            SharedNodeIdentity::new(node_identity()),
        )
        .await
        .expect("first MUC schema open");
        PostgresMucRoomStore::open(
            db,
            CancellationToken::new(),
            SharedNodeIdentity::new(node_identity()),
        )
        .await
        .expect("second MUC schema open");
    }

    #[tokio::test]
    async fn lifecycle_schema_has_expected_catalog_shape() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((_store, _claim_store, db, _me)) = clean_store().await else {
            return;
        };

        for (column, nullable) in [
            ("lifecycle_id", "NO"),
            ("room_jid", "NO"),
            ("revision", "NO"),
            ("state", "NO"),
            ("created_at", "NO"),
            ("updated_at", "NO"),
        ] {
            assert_eq!(
                column_nullability(&db, "clustering_muc_room_lifecycles", column).await,
                Some(nullable.to_string()),
                "lifecycle column {column} must have expected nullability"
            );
        }
        for constraint in [
            "clustering_muc_room_lifecycles_revision_min",
            "clustering_muc_room_lifecycles_state_closed",
        ] {
            assert!(
                named_check_exists(&db, "clustering_muc_room_lifecycles", constraint).await,
                "lifecycle check constraint {constraint} must exist"
            );
        }
        for column in ["lifecycle_id", "revision"] {
            assert_eq!(
                column_nullability(&db, "clustering_muc_rooms", column).await,
                Some("YES".to_string()),
                "room snapshot back-link {column} must remain nullable"
            );
        }
        for constraint in [
            "clustering_muc_rooms_lifecycle_pairing",
            "clustering_muc_rooms_revision_min",
        ] {
            assert!(
                named_check_exists(&db, "clustering_muc_rooms", constraint).await,
                "room check constraint {constraint} must exist"
            );
        }

        let conn = db.guard().await.expect("guard");
        let mut rows = conn
            .query(
                "SELECT i.indisunique, i.indpred IS NOT NULL \
                 FROM pg_index i \
                 JOIN pg_class index_relation ON index_relation.oid = i.indexrelid \
                 WHERE index_relation.relname = ? \
                   AND i.indrelid = to_regclass('clustering_muc_room_lifecycles')",
                crate::db_params!["clustering_muc_room_lifecycles_live_room_idx"],
            )
            .await
            .expect("query lifecycle index catalog");
        let index = rows
            .next()
            .await
            .expect("read lifecycle index catalog")
            .expect("lifecycle partial unique index exists");
        assert!(index
            .get::<bool>(0)
            .expect("decode lifecycle index uniqueness"));
        assert!(index
            .get::<bool>(1)
            .expect("decode lifecycle index predicate"));
    }

    #[tokio::test]
    async fn lifecycle_schema_rejects_invalid_invariants() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((_store, _claim_store, db, _me)) = clean_store().await else {
            return;
        };
        let conn = db.guard().await.expect("guard");

        assert!(conn
            .execute(
                "INSERT INTO clustering_muc_room_lifecycles (lifecycle_id, room_jid, revision, state) VALUES (?, ?, ?, ?)",
                crate::db_params!["lifecycle-revision-zero", "revision-zero@muc.example.com", 0_i64, "active"],
            )
            .await
            .is_err());
        assert!(conn
            .execute(
                "INSERT INTO clustering_muc_room_lifecycles (lifecycle_id, room_jid, revision, state) VALUES (?, ?, ?, ?)",
                crate::db_params!["lifecycle-bogus-state", "bogus-state@muc.example.com", 1_i64, "bogus"],
            )
            .await
            .is_err());

        conn.execute(
            "INSERT INTO clustering_muc_room_lifecycles (lifecycle_id, room_jid, revision, state) VALUES (?, ?, ?, ?)",
            crate::db_params!["live-active", "one-live@muc.example.com", 1_i64, "active"],
        )
        .await
        .expect("insert first active lifecycle");
        for (lifecycle_id, state) in [("second-active", "active"), ("second-dormant", "dormant")] {
            assert!(conn
                .execute(
                    "INSERT INTO clustering_muc_room_lifecycles (lifecycle_id, room_jid, revision, state) VALUES (?, ?, ?, ?)",
                    crate::db_params![lifecycle_id, "one-live@muc.example.com", 2_i64, state],
                )
                .await
                .is_err(), "a second {state} lifecycle must be rejected");
        }

        // Prove the state CHECK actually admits 'dormant' on its own: the
        // rejections above would also fire from the live-room unique index,
        // so without this insert a vocabulary typo in the constraint would
        // pass the suite while breaking #1646's dormancy snapshot.
        conn.execute(
            "INSERT INTO clustering_muc_room_lifecycles (lifecycle_id, room_jid, revision, state) VALUES (?, ?, ?, ?)",
            crate::db_params![
                "fresh-dormant",
                "dormant-only@muc.example.com",
                1_i64,
                "dormant"
            ],
        )
        .await
        .expect("a dormant lifecycle on a fresh room must satisfy the state vocabulary");

        for (lifecycle_id, room_jid, revision, state) in [
            (
                "tombstone-first",
                "tombstone-and-live@muc.example.com",
                1_i64,
                "tombstoned",
            ),
            (
                "live-after-tombstone",
                "tombstone-and-live@muc.example.com",
                2_i64,
                "active",
            ),
            (
                "tombstone-second",
                "tombstone-and-live@muc.example.com",
                3_i64,
                "tombstoned",
            ),
        ] {
            conn.execute(
                "INSERT INTO clustering_muc_room_lifecycles (lifecycle_id, room_jid, revision, state) VALUES (?, ?, ?, ?)",
                crate::db_params![lifecycle_id, room_jid, revision, state],
            )
            .await
            .expect("allowed lifecycle state combination");
        }

        for (room_jid, lifecycle_id, revision) in [
            (
                "missing-revision@muc.example.com",
                Some("lifecycle-missing-revision"),
                None,
            ),
            ("missing-lifecycle@muc.example.com", None, Some(1_i64)),
            (
                "room-revision-zero@muc.example.com",
                Some("lifecycle-revision-zero"),
                Some(0_i64),
            ),
        ] {
            assert!(conn
                .execute(
                    "INSERT INTO clustering_muc_rooms (room_jid, waddle_id, channel_id, config_json, lifecycle_id, revision) VALUES (?, ?, ?, ?, ?, ?)",
                    crate::db_params![room_jid, "waddle", "channel", "{}", lifecycle_id, revision],
                )
                .await
                .is_err(), "invalid room lifecycle pairing or revision must be rejected");
        }
        conn.execute(
            "INSERT INTO clustering_muc_rooms (room_jid, waddle_id, channel_id, config_json) VALUES (?, ?, ?, ?)",
            crate::db_params!["pre-lifecycle@muc.example.com", "waddle", "channel", "{}"],
        )
        .await
        .expect("a pre-lifecycle room row with both back-link columns NULL is valid");
    }

    #[tokio::test]
    async fn lifecycle_state_db_mapping_matches_the_schema_check() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((_store, _claim_store, db, _me)) = clean_store().await else {
            return;
        };
        let conn = db.guard().await.expect("guard");
        for (index, state) in [
            RoomLifecycleState::Active,
            RoomLifecycleState::Dormant,
            RoomLifecycleState::Tombstoned,
        ]
        .into_iter()
        .enumerate()
        {
            conn.execute(
                "INSERT INTO clustering_muc_room_lifecycles (lifecycle_id, room_jid, revision, state) VALUES (?, ?, ?, ?)",
                crate::db_params![
                    format!("lifecycle-state-{index}"),
                    format!("state-{index}@muc.example.com"),
                    1_i64,
                    state.as_db_str(),
                ],
            )
            .await
            .expect("every Rust lifecycle state must satisfy the database CHECK");
        }
        assert!(conn
            .execute(
                "INSERT INTO clustering_muc_room_lifecycles (lifecycle_id, room_jid, revision, state) VALUES (?, ?, ?, ?)",
                crate::db_params!["lifecycle-state-invalid", "invalid@muc.example.com", 1_i64, "invalid"],
            )
            .await
            .is_err());
    }

    #[tokio::test]
    async fn exact_claim_check_does_not_block_identity_rotation_on_database_io() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, db, me)) = clean_store().await else {
            return;
        };
        let jid = room_jid("rotation-during-exact-check");
        let entity = Entity::new(EntityType::RoomActor, jid.to_string());
        let epoch = claim_store
            .ensure_claimed(&entity, &me)
            .await
            .expect("claim");
        let fence = RoomClaimFenceContext::new(entity, me, epoch);
        store.record_claim_fence(&jid, fence.clone());

        let mut blocker = db.begin().await.expect("begin claim-row blocker");
        let mut rows = blocker
            .query(
                "SELECT 1 FROM clustering_claims WHERE entity = ? FOR UPDATE",
                crate::db_params![room_entity_key(&jid)],
            )
            .await
            .expect("lock exact claim row");
        assert!(rows.next().await.expect("locked row result").is_some());
        drop(rows);

        let store = Arc::new(store);
        let check_store = Arc::clone(&store);
        let check_jid = jid.clone();
        let check_fence = fence.clone();
        let check = tokio::spawn(async move {
            check_store
                .exact_claim_is_held(&check_jid, &check_fence)
                .await
        });

        // Prove the exact check reached Postgres and is blocked on the row
        // lock before rotating. A task-start notification or sleep can pass
        // without the query ever being polled, which would exercise only the
        // early identity-mismatch branch.
        let monitor = db.guard().await.expect("monitor guard");
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let mut rows = monitor
                    .query(
                        r#"
                        SELECT COUNT(*) FROM pg_stat_activity
                        WHERE pid <> pg_backend_pid()
                          AND query LIKE '%muc_exact_claim_check%'
                          AND wait_event_type = 'Lock'
                        "#,
                        (),
                    )
                    .await
                    .expect("inspect blocked exact claim check");
                let blocked = rows
                    .next()
                    .await
                    .expect("read blocked exact-check count")
                    .expect("blocked exact-check count row")
                    .get::<i64>(0)
                    .expect("blocked exact-check count");
                if blocked > 0 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("exact claim check reached the claim-row lock");
        drop(monitor);
        assert!(
            !check.is_finished(),
            "the exact check must be waiting behind the held database row lock"
        );

        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            store.node_identity.rotate(node_identity()),
        )
        .await
        .expect("database I/O must not retain the identity rotation guard");
        blocker.commit().await.expect("release claim-row blocker");

        assert!(!check
            .await
            .expect("exact-check task")
            .expect("database proof completes after the row lock is released"));
    }

    #[tokio::test]
    async fn save_and_load_round_trips_config_subject_and_affiliations() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, _db, me)) = clean_store().await else {
            return;
        };
        let jid = room_jid("round-trip");
        let entity = Entity::new(EntityType::RoomActor, jid.to_string());
        let epoch = claim_store
            .ensure_claimed(&entity, &me)
            .await
            .expect("claim");
        let fence = RoomClaimFenceContext::new(entity.clone(), me.clone(), epoch);
        store.record_claim_fence(&jid, fence.clone());

        let config = RoomConfig {
            name: "test room".to_string(),
            members_only: true,
            ..RoomConfig::default()
        };
        store
            .commit_room_mutation(
                &jid,
                &fence,
                RoomDurableMutation::Create {
                    waddle_id: "waddle-1".to_string(),
                    channel_id: "chan-1".to_string(),
                    config: config.clone(),
                    initial_affiliations: Vec::new(),
                },
            )
            .await
            .expect("create room");

        let subject = SubjectState {
            texts: RoomSubjectTexts::from_iter([(String::new(), "hello".to_string())]),
            setter: "alice@example.com".parse().expect("valid jid"),
            setter_nick: "alice".to_string(),
            set_at: chrono::Utc::now(),
        };
        store
            .commit_room_mutation(
                &jid,
                &fence,
                RoomDurableMutation::Subject(Some(subject.clone())),
            )
            .await
            .expect("save subject");

        let bob: BareJid = "bob@example.com".parse().expect("valid jid");
        store
            .commit_room_mutation(
                &jid,
                &fence,
                RoomDurableMutation::Affiliation(DurableAffiliationEntry::new(
                    bob.clone(),
                    Some(Affiliation::Owner),
                )),
            )
            .await
            .expect("save affiliation");

        let loaded = store
            .load_room_state_fenced(&jid, &fence)
            .await
            .expect("load")
            .expect("row exists");
        assert_eq!(loaded.waddle_id, "waddle-1");
        assert_eq!(loaded.channel_id, "chan-1");
        assert_eq!(loaded.config.name, "test room");
        assert!(loaded.config.members_only);
        assert_eq!(
            loaded.subject.expect("subject persisted").texts.get(""),
            Some("hello")
        );
        assert_eq!(loaded.affiliations.len(), 1);
        assert_eq!(loaded.affiliations[0].jid, bob);
        assert_eq!(loaded.affiliations[0].affiliation, Affiliation::Owner);
    }

    #[tokio::test]
    async fn save_affiliation_none_deletes_the_row() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, _db, me)) = clean_store().await else {
            return;
        };
        let jid = room_jid("affiliation-removal");
        let entity = Entity::new(EntityType::RoomActor, jid.to_string());
        let epoch = claim_store
            .ensure_claimed(&entity, &me)
            .await
            .expect("claim");
        let fence = RoomClaimFenceContext::new(entity.clone(), me.clone(), epoch);
        store.record_claim_fence(&jid, fence.clone());
        store
            .commit_room_mutation(
                &jid,
                &fence,
                RoomDurableMutation::Create {
                    waddle_id: "waddle-1".to_string(),
                    channel_id: "chan-1".to_string(),
                    config: RoomConfig::default(),
                    initial_affiliations: Vec::new(),
                },
            )
            .await
            .expect("create room");

        let carol: BareJid = "carol@example.com".parse().expect("valid jid");
        store
            .commit_room_mutation(
                &jid,
                &fence,
                RoomDurableMutation::Affiliation(DurableAffiliationEntry::new(
                    carol.clone(),
                    Some(Affiliation::Member),
                )),
            )
            .await
            .expect("save member");
        let loaded = store
            .load_room_state_fenced(&jid, &fence)
            .await
            .expect("load")
            .expect("row exists");
        assert_eq!(loaded.affiliations.len(), 1);

        store
            .commit_room_mutation(
                &jid,
                &fence,
                RoomDurableMutation::Affiliation(DurableAffiliationEntry::new(carol, None)),
            )
            .await
            .expect("save none removes the row");
        let loaded = store
            .load_room_state_fenced(&jid, &fence)
            .await
            .expect("load")
            .expect("row exists");
        assert!(loaded.affiliations.is_empty());
    }

    #[tokio::test]
    async fn commit_subject_rejects_a_claimed_room_without_durable_state() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, _db, me)) = clean_store().await else {
            return;
        };
        let jid = room_jid("subject-without-durable-state");
        let entity = Entity::new(EntityType::RoomActor, jid.to_string());
        let epoch = claim_store
            .ensure_claimed(&entity, &me)
            .await
            .expect("claim");
        let fence = RoomClaimFenceContext::new(entity, me, epoch);
        store.record_claim_fence(&jid, fence.clone());
        let subject = SubjectState {
            texts: RoomSubjectTexts::from_iter([(String::new(), "hello".to_string())]),
            setter: "alice@example.com".parse().expect("valid jid"),
            setter_nick: "alice".to_string(),
            set_at: chrono::Utc::now(),
        };

        let result = store
            .commit_room_mutation(&jid, &fence, RoomDurableMutation::Subject(Some(subject)))
            .await;
        assert!(
            matches!(
                &result,
                Err(RoomCommitError::StateMissing)
            ),
            "a subject commit must fail typed rather than acknowledge non-durable state: {result:?}"
        );
        assert!(
            store
                .load_room_state_fenced(&jid, &fence)
                .await
                .expect("load after rejected subject write")
                .is_none(),
            "the rejected subject write must not create a partial durable room state"
        );
    }

    #[tokio::test]
    async fn fresh_clustered_room_rejects_its_first_subject_before_applying_memory() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, _db, me)) = clean_store().await else {
            return;
        };
        let room_jid = room_jid("fresh-first-subject");
        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        let epoch = claim_store
            .ensure_claimed(&entity, &me)
            .await
            .expect("claim");
        let fence = RoomClaimFenceContext::new(entity, me, epoch);
        let store: Arc<dyn MucDurableStore> = Arc::new(store);
        // Mirror registry preparation: the exact fence must be established
        // with the store before any actor-owned durable commit can pass.
        store.establish_claim_fence(&room_jid, fence.clone());
        let actor = RoomActor::spawn(RoomActor::new(
            MucRoom::new(
                room_jid.clone(),
                "waddle-fresh".to_string(),
                "channel-fresh".to_string(),
                RoomConfig::default(),
            ),
            OccupantIdSecret::new(b"muc-durable-first-subject-test-secret".to_vec())
                .expect("valid test secret"),
        ));

        actor
            .ask(RestoreDurableRoomState {
                store: store.clone(),
                claim_fence: fence.clone(),
            })
            .await
            .expect("restore handler");
        let result = actor
            .ask(SetSubject {
                texts: RoomSubjectTexts::from_iter([(String::new(), "first subject".to_string())]),
                setter: "alice@example.com".parse().expect("valid jid"),
                setter_nick: "alice".to_string(),
                set_at: chrono::Utc::now(),
            })
            .await;
        assert!(
            matches!(
                result,
                Err(SendError::HandlerError(
                    SetSubjectError::PersistFailedBeforeApply
                ))
            ),
            "unexpected first-subject outcome: {result:?}"
        );
        assert!(
            actor
                .ask(GetSnapshot)
                .await
                .expect("room snapshot")
                .room
                .subject
                .is_none(),
            "the failed first subject must not be installed in actor memory"
        );
        assert!(
            store
                .load_room_state_fenced(&room_jid, &fence)
                .await
                .expect("load after rejected first subject")
                .is_none(),
            "the failed first subject must not manufacture a partial durable parent"
        );
    }

    /// The plan's own Slice 7 Tests entry: "fenced pre-fan-out SELECT
    /// returns 0 rows immediately after a steal commits (the deposed
    /// owner's very next broadcast is blocked)."
    #[tokio::test]
    async fn check_fenced_fanout_returns_false_immediately_after_a_steal_commits() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, db, me)) = clean_store().await else {
            return;
        };
        let jid = room_jid("deposed");
        let entity = Entity::new(EntityType::RoomActor, jid.to_string());
        let epoch = claim_store
            .ensure_claimed(&entity, &me)
            .await
            .expect("claim");
        let fence = RoomClaimFenceContext::new(entity.clone(), me.clone(), epoch);
        store.record_claim_fence(&jid, fence.clone());

        let sibling_jid = room_jid("different-room");
        let sibling_entity = Entity::new(EntityType::RoomActor, sibling_jid.to_string());
        let sibling_fence = RoomClaimFenceContext::new(sibling_entity, me.clone(), epoch);
        assert!(
            !store
                .check_exact_claim_fence(&jid, &sibling_fence)
                .await
                .expect("cross-room exact check"),
            "a different room entity must not authorize this room, regardless of its epoch"
        );
        assert_eq!(
            store.current_claim_fence(&jid),
            Some(fence.clone()),
            "a mismatched entity must not evict the retained room fence"
        );
        assert!(
            store
                .check_exact_claim_fence(&jid, &fence)
                .await
                .expect("exact room check"),
            "the matching room incarnation must remain authorized"
        );

        assert!(
            store
                .check_fenced_fanout(&jid)
                .await
                .expect("check_fenced_fanout"),
            "the current owner's own fenced check must pass"
        );

        // Simulate another node stealing via steal_stale(OwnerStale): `me`
        // has no `clustering_nodes` liveness row, so the owner-stale
        // predicate is true, while the stealer has a fresh live row as
        // required by Slice 1a's hardened steal CAS.
        let stealer = live_stealer(&db).await;
        claim_store
            .steal_stale(&entity, epoch, StalePredicate::OwnerStale, &stealer)
            .await
            .expect("steal succeeds against a dead-owner claim");

        assert!(
            !store
                .check_fenced_fanout(&jid)
                .await
                .expect("check_fenced_fanout"),
            "the deposed owner's very next fenced check must observe 0 rows"
        );
        assert!(
            !store
                .check_fenced_fanout(&jid)
                .await
                .expect("a removed stale cache fence is still non-serving"),
            "concurrent stale dispatches must not fail open after the first check clears the cache"
        );
    }

    /// Lower-level MAM storage regression coverage. It is not a #1355
    /// general-dispatch or archive-fence contract; the production pre-fanout
    /// path remains cache-backed until #1283 replaces it.
    #[tokio::test]
    async fn mam_store_message_fenced_blocks_the_deposed_owners_next_archive_write() {
        use waddle_xmpp::mam::{MamStorage, MamStorageError, SqlxMamStorage, StoreOutcome};
        use waddle_xmpp_core::mam::ArchivedMessage;

        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, db, me)) = clean_store().await else {
            return;
        };
        let jid = room_jid("mam-fenced");
        let entity = Entity::new(EntityType::RoomActor, jid.to_string());
        let epoch = claim_store
            .ensure_claimed(&entity, &me)
            .await
            .expect("claim");
        store.record_claim_fence(
            &jid,
            RoomClaimFenceContext::new(entity.clone(), me.clone(), epoch),
        );

        let mam_storage = SqlxMamStorage::open(db.database_url())
            .await
            .expect("open mam storage against the same postgres database")
            .with_cluster_fencing(true);
        // This test's own room JID is unique per run (`room_jid` embeds no
        // randomness itself, but the shared Postgres instance persists
        // `mam_messages` rows across test runs within the same process —
        // unlike `clean_store()`'s clustering tables, nothing here wipes
        // MAM rows). Scope every assertion to a fresh archive id (UUID) so
        // repeated runs against the same long-lived database never
        // collide on a fixed literal id.
        let first_id = uuid::Uuid::new_v4().to_string();
        let second_id = uuid::Uuid::new_v4().to_string();

        let fence = store
            .current_claim_fence(&jid)
            .expect("current_claim_fence must resolve immediately after record_claim_fence");
        assert_eq!(fence.entity, entity);
        assert_eq!(fence.epoch, epoch);
        assert_eq!(fence.owner, me);

        let message = ArchivedMessage {
            id: first_id.clone(),
            body: Some("hello, fenced world".to_string()),
            origin_id: Some(waddle_xmpp_core::xep0359::OriginId::new(
                uuid::Uuid::new_v4().to_string(),
            )),
            message_type: xmpp_parsers::message::MessageType::Groupchat,
            ..ArchivedMessage::for_test(
                format!("{jid}/alice").parse().expect("valid full jid"),
                jid::Jid::from(jid.clone()),
            )
        };
        let archive_outcome = mam_storage
            .store_message_fenced(&jid, &message, &fence)
            .await
            .expect("the current owner's fenced write must succeed");
        assert_eq!(archive_outcome, StoreOutcome::Stored(first_id.clone()));
        let stored = mam_storage
            .get_message(&first_id)
            .await
            .expect("get_message")
            .expect("row exists");
        assert_eq!(stored.body.as_deref(), Some("hello, fenced world"));

        // Steal the claim exactly like `check_fenced_fanout`'s own test:
        // the old owner is missing from `clustering_nodes`, and the stealer
        // is explicitly live.
        let stealer = live_stealer(&db).await;
        claim_store
            .steal_stale(&entity, epoch, StalePredicate::OwnerStale, &stealer)
            .await
            .expect("steal succeeds against a dead-owner claim");

        let duplicate_result = mam_storage
            .store_message_fenced(&jid, &message, &fence)
            .await;
        assert!(
            matches!(duplicate_result, Err(MamStorageError::NotOwner { .. })),
            "origin-id dedup must not bypass the deposed owner's fence: {duplicate_result:?}"
        );

        let second_message = ArchivedMessage {
            id: second_id.clone(),
            body: Some("should never be archived".to_string()),
            message_type: xmpp_parsers::message::MessageType::Groupchat,
            ..ArchivedMessage::for_test(
                format!("{jid}/alice").parse().expect("valid full jid"),
                jid::Jid::from(jid.clone()),
            )
        };
        // The deposed owner's cached `fence` still carries the now-stolen
        // epoch — exactly the "believed was current" scenario the fencing
        // check exists to catch.
        let result = mam_storage
            .store_message_fenced(&jid, &second_message, &fence)
            .await;
        assert!(
            matches!(result, Err(MamStorageError::NotOwner { .. })),
            "the deposed owner's next fenced archive write must be rejected, got: {result:?}"
        );
        // And the message must genuinely not have landed in the archive.
        let should_be_absent = mam_storage
            .get_message(&second_id)
            .await
            .expect("get_message");
        assert!(
            should_be_absent.is_none(),
            "a rejected fenced write must not have committed any row"
        );
    }

    #[test]
    fn stale_muc_cache_failure_does_not_evict_new_generation() {
        let jid = room_jid("old-a-new-b");
        let entity = Entity::new(EntityType::RoomActor, jid.to_string());
        let owner = node_identity();
        let fence_a = RoomClaimFenceContext::new(entity.clone(), owner.clone(), ClaimEpoch(1));
        let fence_b = RoomClaimFenceContext::new(entity, owner, ClaimEpoch(2));
        let cache = DashMap::new();
        cache.insert(jid.clone(), fence_b.clone());

        remove_room_claim_fence_if(&cache, &jid, &fence_a);

        assert_eq!(cache.get(&jid).as_deref(), Some(&fence_b));
    }
}
