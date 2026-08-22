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
//!   closed `state` vocabulary is `preparing` | `active` | `dormant` |
//!   `tombstoned`; the partial unique index permits at most one live
//!   (`preparing`, `active`, or `dormant`)
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
    RoomCommitFuture, RoomCommitOutcome, RoomCommittedCoordinates, RoomDurableMutation,
    RoomMutationEffects, RoomPinProjection, RoomProjection,
};
use waddle_xmpp::muc::{
    DestroyAttemptId, DurableRoomState, MucDurableFuture, MucDurableStore, RoomClaimFenceContext,
    RoomConfig, RoomLifecycleId, RoomLifecycleState, RoomRevision, SubjectState,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommitReconciliation {
    Committed,
    NotCommitted,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DestroyCompletionAttemptProof {
    Missing,
    Inert { lifecycle: Option<RoomLifecycleId> },
    Armed { lifecycle: Option<RoomLifecycleId> },
}

#[derive(serde::Deserialize)]
struct PersistedDestroyCompletionRoom {
    room_jid: BareJid,
}

fn persisted_affiliation_fingerprint(entry: &DurableAffiliationEntry) -> serde_json::Value {
    serde_json::json!({
        "jid": entry.jid.clone(),
        "affiliation": entry.affiliation.map(affiliation_to_db_str),
    })
}

fn projection_fingerprint(projection: &RoomProjection) -> serde_json::Value {
    match projection {
        RoomProjection::OccupancyJoin { occupant, nick } => serde_json::json!({
            "kind": "occupancy_join",
            "occupant": occupant.to_string(),
            "nick": nick.as_str(),
        }),
        RoomProjection::OccupancyLeave {
            occupant,
            nick,
            cause,
        } => serde_json::json!({
            "kind": "occupancy_leave",
            "occupant": occupant.to_string(),
            "nick": nick.as_str(),
            "cause": match cause {
                waddle_xmpp::muc::durable::OccupancyLeaveCause::Explicit => "explicit",
                waddle_xmpp::muc::durable::OccupancyLeaveCause::Disconnect => "disconnect",
                waddle_xmpp::muc::durable::OccupancyLeaveCause::Administrative => "administrative",
            },
        }),
        RoomProjection::Pin(RoomPinProjection::Pin { target }) => serde_json::json!({
            "kind": "pin",
            "target": target.id.as_str(),
        }),
        RoomProjection::Pin(RoomPinProjection::Unpin { target }) => serde_json::json!({
            "kind": "unpin",
            "target": target.id.as_str(),
        }),
    }
}

/// The fingerprint binds the payload to the COMMITTING fence (owner identity
/// and claim epoch), not just the operation shape: two nodes can legitimately
/// commit byte-identical projections at the same next revision across an
/// ownership transfer, and ambiguous-commit reconciliation must never mistake
/// the other owner's row for its own (#1647).
fn mutation_fingerprint(
    fence: &RoomClaimFenceContext,
    intent: &RoomDurableMutation,
) -> Result<String, RoomCommitError> {
    let intent_value = match intent {
        RoomDurableMutation::Create {
            waddle_id,
            channel_id,
            config,
            initial_affiliations,
        } => serde_json::json!({
            "kind": "create",
            "waddle_id": waddle_id.as_str(),
            "channel_id": channel_id.as_str(),
            "config": config,
            "initial_affiliations": initial_affiliations
                .iter()
                .map(persisted_affiliation_fingerprint)
                .collect::<Vec<_>>(),
        }),
        RoomDurableMutation::Publish => serde_json::json!({ "kind": "publish" }),
        RoomDurableMutation::MarkUnpublishedCleanup => {
            serde_json::json!({ "kind": "mark_unpublished_cleanup" })
        }
        RoomDurableMutation::Config {
            config,
            waddle_id,
            channel_id,
        } => serde_json::json!({
            "kind": "config",
            "waddle_id": waddle_id.as_str(),
            "channel_id": channel_id.as_str(),
            "config": config,
        }),
        RoomDurableMutation::Subject(subject) => {
            serde_json::json!({ "kind": "subject", "subject": subject })
        }
        RoomDurableMutation::Affiliation(entry) => serde_json::json!({
            "kind": "affiliation",
            "entry": persisted_affiliation_fingerprint(entry),
        }),
        RoomDurableMutation::AffiliationBatch(entries) => serde_json::json!({
            "kind": "affiliation_batch",
            "entries": entries
                .iter()
                .map(persisted_affiliation_fingerprint)
                .collect::<Vec<_>>(),
        }),
        RoomDurableMutation::MembersOnlyEnforcement {
            config,
            affiliations,
        } => serde_json::json!({
            "kind": "members_only_enforcement",
            "config": config,
            "affiliations": affiliations
                .iter()
                .map(persisted_affiliation_fingerprint)
                .collect::<Vec<_>>(),
        }),
        RoomDurableMutation::MediatedInviteGrant(entry) => serde_json::json!({
            "kind": "mediated_invite_grant",
            "entry": persisted_affiliation_fingerprint(entry),
        }),
        RoomDurableMutation::MediatedInviteRollback(entry) => serde_json::json!({
            "kind": "mediated_invite_rollback",
            "entry": persisted_affiliation_fingerprint(entry),
        }),
        RoomDurableMutation::Destroy { completion_attempt } => serde_json::json!({
            "kind": "destroy",
            "completion_attempt": completion_attempt
                .as_ref()
                .map(|attempt| attempt.as_uuid()),
        }),
        RoomDurableMutation::DestroyAndReleaseClaim { completion_attempt } => serde_json::json!({
            "kind": "destroy_and_release_claim",
            "completion_attempt": completion_attempt
                .as_ref()
                .map(|attempt| attempt.as_uuid()),
        }),
        RoomDurableMutation::Dormancy => serde_json::json!({ "kind": "dormancy" }),
        RoomDurableMutation::Activate => serde_json::json!({ "kind": "activate" }),
        RoomDurableMutation::Projection(projection) => serde_json::json!({
            "kind": "projection",
            "projection": projection_fingerprint(projection),
        }),
    };
    let value = serde_json::json!({
        "fence": {
            "node_id": fence.owner.node_id,
            "node_epoch": fence.owner.node_epoch,
            "claim_epoch": fence.epoch.0,
        },
        "intent": intent_value,
    });
    serde_json::to_string(&value).map_err(|_| commit_database_error())
}

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
    room_effect_outbox: crate::room_effect_outbox::RoomEffectOutboxStore,
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
        Self::ensure_schema(&db).await.map_err(db_err)?;
        crate::muc_destroy_completion_outbox::MucDestroyCompletionOutboxStore::new(db.clone())
            .await
            .map_err(db_err)?;
        let room_effect_outbox = crate::room_effect_outbox::RoomEffectOutboxStore::new(db.clone())
            .await
            .map_err(|error| {
                crate::telemetry::mark_span_error("MUC durable storage operation failed");
                XmppError::internal(format!("room effect outbox bootstrap error: {error}"))
            })?;
        let store = Self {
            db,
            room_effect_outbox,
            node_identity,
            stop_token,
            exact_claim_fences: DashMap::new(),
            published_claim_fences: DashMap::new(),
        };
        Ok(store)
    }

    async fn ensure_schema(db: &Database) -> Result<(), DatabaseError> {
        let mut tx = db.begin().await?;
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
                config_lifecycle_id TEXT,
                config_revision BIGINT,
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
            DO $$
            BEGIN
                IF NOT EXISTS (
                    SELECT 1
                    FROM pg_attribute
                    WHERE attrelid = 'clustering_muc_rooms'::regclass
                      AND attname = 'config_lifecycle_id'
                      AND NOT attisdropped
                ) THEN
                    ALTER TABLE clustering_muc_rooms ADD COLUMN config_lifecycle_id TEXT;
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
                      AND attname = 'config_revision'
                      AND NOT attisdropped
                ) THEN
                    ALTER TABLE clustering_muc_rooms ADD COLUMN config_revision BIGINT;
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
                    WHERE conname = 'clustering_muc_rooms_config_lifecycle_pairing'
                      AND conrelid = 'clustering_muc_rooms'::regclass
                ) THEN
                    ALTER TABLE clustering_muc_rooms
                        ADD CONSTRAINT clustering_muc_rooms_config_lifecycle_pairing
                        CHECK ((config_lifecycle_id IS NULL) = (config_revision IS NULL));
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
                    WHERE conname = 'clustering_muc_rooms_config_revision_min'
                      AND conrelid = 'clustering_muc_rooms'::regclass
                ) THEN
                    ALTER TABLE clustering_muc_rooms
                        ADD CONSTRAINT clustering_muc_rooms_config_revision_min
                        CHECK (config_revision IS NULL OR config_revision >= 1);
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
                state        TEXT NOT NULL CONSTRAINT clustering_muc_room_lifecycles_state_closed CHECK (state IN ('preparing','active','dormant','tombstoned')),
                mutation_fingerprint TEXT,
                created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
                updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
            )
            "#,
            (),
        )
        .await?;
        // Upgrade the closed state vocabulary only when this deployment
        // predates the durable preparing phase. The catalog probe avoids an
        // ACCESS EXCLUSIVE lock on every steady-state startup.
        tx.execute(
            r#"
            DO $$
            BEGIN
                IF EXISTS (
                    SELECT 1
                    FROM pg_constraint
                    WHERE conname = 'clustering_muc_room_lifecycles_state_closed'
                      AND conrelid = 'clustering_muc_room_lifecycles'::regclass
                      AND pg_get_constraintdef(oid) NOT LIKE '%preparing%'
                ) THEN
                    ALTER TABLE clustering_muc_room_lifecycles
                        DROP CONSTRAINT clustering_muc_room_lifecycles_state_closed;
                    ALTER TABLE clustering_muc_room_lifecycles
                        ADD CONSTRAINT clustering_muc_room_lifecycles_state_closed
                        CHECK (state IN ('preparing','active','dormant','tombstoned'));
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
                IF EXISTS (
                    SELECT 1
                    FROM pg_index i
                    JOIN pg_class c ON c.oid = i.indexrelid
                    WHERE i.indrelid = 'clustering_muc_room_lifecycles'::regclass
                      AND c.relname = 'clustering_muc_room_lifecycles_live_room_idx'
                      AND pg_get_indexdef(i.indexrelid) NOT LIKE '%preparing%'
                ) THEN
                    DROP INDEX clustering_muc_room_lifecycles_live_room_idx;
                END IF;
                IF NOT EXISTS (
                    SELECT 1
                    FROM pg_index i
                    JOIN pg_class c ON c.oid = i.indexrelid
                    WHERE i.indrelid = 'clustering_muc_room_lifecycles'::regclass
                      AND c.relname = 'clustering_muc_room_lifecycles_live_room_idx'
                ) THEN
                    CREATE UNIQUE INDEX clustering_muc_room_lifecycles_live_room_idx
                        ON clustering_muc_room_lifecycles (room_jid) WHERE state IN ('preparing','active','dormant');
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
                    WHERE attrelid = 'clustering_muc_room_lifecycles'::regclass
                      AND attname = 'mutation_fingerprint'
                      AND NOT attisdropped
                ) THEN
                    ALTER TABLE clustering_muc_room_lifecycles
                        ADD COLUMN mutation_fingerprint TEXT;
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
            "FOR UPDATE" => {
                "SELECT 1 FROM clustering_claims WHERE entity = ? AND node_id = ? AND node_epoch = ? AND claim_epoch = ? FOR UPDATE"
            }
            _ => {
                "SELECT 1 FROM clustering_claims WHERE entity = ? AND node_id = ? AND node_epoch = ? AND claim_epoch = ? FOR SHARE"
            }
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

    async fn release_claim_in_tx(
        &self,
        tx: &mut Transaction<'_>,
        room_jid: &BareJid,
        fence: &RoomClaimFenceContext,
    ) -> Result<(), RoomCommitError> {
        let released = tx
            .execute(
                "DELETE FROM clustering_claims WHERE entity = ? AND node_id = ? AND node_epoch = ? AND claim_epoch = ?",
                crate::db_params![
                    room_entity_key(room_jid),
                    fence.owner.node_id.clone(),
                    fence.owner.node_epoch.clone(),
                    fence.epoch.0,
                ],
            )
            .await
            .map_err(Self::commit_error)?;
        if released == 1 {
            Ok(())
        } else {
            remove_room_claim_fence_if(&self.exact_claim_fences, room_jid, fence);
            remove_room_claim_fence_if(&self.published_claim_fences, room_jid, fence);
            Err(RoomCommitError::NotOwner)
        }
    }

    fn commit_error(error: DatabaseError) -> RoomCommitError {
        if is_retryable_tx_error(&error) {
            RoomCommitError::RetryExhausted
        } else {
            tracing::warn!(
                error = ?error,
                "MUC durable commit failed with a non-retryable database error; returning sanitized RoomCommitError"
            );
            commit_database_error()
        }
    }

    fn commit_fence_error(error: DatabaseError) -> RoomCommitError {
        if is_retryable_tx_error(&error) {
            RoomCommitError::RetryExhausted
        } else {
            tracing::warn!(
                error = ?error,
                "MUC durable ownership fence query failed with a non-retryable database error; returning OwnershipUnavailable"
            );
            RoomCommitError::OwnershipUnavailable
        }
    }

    async fn load_destroy_completion_attempt_proof(
        &self,
        attempt: &DestroyAttemptId,
    ) -> Result<DestroyCompletionAttemptProof, RoomCommitError> {
        let conn = self.db.guard().await.map_err(Self::commit_error)?;
        let mut rows = conn
            .query(
                "SELECT lifecycle_id, available_at_ms \
                 FROM clustering_muc_destroy_outbox \
                 WHERE attempt_id = ?",
                crate::db_params![attempt.as_uuid().to_string()],
            )
            .await
            .map_err(Self::commit_error)?;
        let Some(row) = rows.next().await.map_err(Self::commit_error)? else {
            return Ok(DestroyCompletionAttemptProof::Missing);
        };
        let lifecycle: Option<String> = row.get(0).map_err(Self::commit_error)?;
        let lifecycle = lifecycle
            .map(|value| {
                uuid::Uuid::parse_str(&value)
                    .map(RoomLifecycleId::from_uuid)
                    .map_err(|_| commit_database_error())
            })
            .transpose()?;
        let available_at_ms: i64 = row.get(1).map_err(Self::commit_error)?;
        Ok(if available_at_ms == i64::MAX {
            DestroyCompletionAttemptProof::Inert { lifecycle }
        } else {
            DestroyCompletionAttemptProof::Armed { lifecycle }
        })
    }

    async fn reconcile_ambiguous_commit(
        &self,
        room_jid: &BareJid,
        fence: &RoomClaimFenceContext,
        intent: &RoomDurableMutation,
        coordinates: RoomCommittedCoordinates,
    ) -> Result<CommitReconciliation, RoomCommitError> {
        let expected_fingerprint = mutation_fingerprint(fence, intent)?;
        let conn = self.db.guard().await.map_err(Self::commit_error)?;
        let mut reconcile_rows = conn
            .query(
                "SELECT lifecycles.state, lifecycles.mutation_fingerprint, lifecycles.revision, CASE WHEN EXISTS(SELECT 1 FROM clustering_muc_rooms WHERE room_jid = ? AND lifecycle_id = ? AND revision = ?) THEN 1 ELSE 0 END AS room_exists FROM clustering_muc_room_lifecycles lifecycles WHERE lifecycles.room_jid = ? AND lifecycles.lifecycle_id = ?",
                crate::db_params![
                    room_jid.to_string(),
                    coordinates.lifecycle.to_string(),
                    coordinates.revision.as_i64(),
                    room_jid.to_string(),
                    coordinates.lifecycle.to_string(),
                ],
            )
            .await
            .map_err(Self::commit_error)?;
        let (lifecycle_state, room_matches) = reconcile_rows
            .next()
            .await
            .map_err(Self::commit_error)?
            .map(|row| {
                Ok::<_, RoomCommitError>((
                    Some((
                        row.get::<String>(0).map_err(Self::commit_error)?,
                        row.get::<Option<String>>(1).map_err(Self::commit_error)?,
                        row.get::<i64>(2).map_err(Self::commit_error)?,
                    )),
                    row.get::<i64>(3).map_err(Self::commit_error)? != 0,
                ))
            })
            .transpose()?
            .unwrap_or((None, false));
        if lifecycle_state
            .as_ref()
            .is_some_and(|(_, _, revision)| *revision > coordinates.revision.as_i64())
        {
            return Ok(CommitReconciliation::Unknown);
        }
        let destroy_attempt_proof = match intent {
            RoomDurableMutation::Destroy {
                completion_attempt: Some(attempt),
            }
            | RoomDurableMutation::DestroyAndReleaseClaim {
                completion_attempt: Some(attempt),
            } => Some(self.load_destroy_completion_attempt_proof(attempt).await?),
            _ => None,
        };
        let exact_intent_reconciled = lifecycle_state
            .as_ref()
            .and_then(|(_, fingerprint, revision)| {
                (*revision == coordinates.revision.as_i64()).then_some(fingerprint)
            })
            .and_then(|fingerprint| {
                fingerprint
                    .as_deref()
                    .map(|fingerprint| fingerprint == expected_fingerprint)
            });
        let terminal_coordinates_committed =
            lifecycle_state.as_ref().and_then(|(state, _, revision)| {
                (*revision == coordinates.revision.as_i64()).then_some(state.as_str())
            }) == Some(RoomLifecycleState::Tombstoned.as_db_str())
                && !room_matches;

        match intent {
            RoomDurableMutation::Destroy {
                completion_attempt: Some(_),
            } => Ok(match destroy_attempt_proof {
                Some(DestroyCompletionAttemptProof::Armed {
                    lifecycle: Some(lifecycle),
                }) if terminal_coordinates_committed
                    && lifecycle == coordinates.lifecycle
                    && exact_intent_reconciled == Some(true) =>
                {
                    CommitReconciliation::Committed
                }
                Some(DestroyCompletionAttemptProof::Armed {
                    lifecycle: Some(lifecycle),
                }) if terminal_coordinates_committed
                    && lifecycle == coordinates.lifecycle
                    && exact_intent_reconciled == Some(false) =>
                {
                    CommitReconciliation::NotCommitted
                }
                Some(DestroyCompletionAttemptProof::Missing)
                | Some(DestroyCompletionAttemptProof::Inert { .. })
                | Some(DestroyCompletionAttemptProof::Armed { .. })
                    if terminal_coordinates_committed =>
                {
                    CommitReconciliation::Unknown
                }
                _ => CommitReconciliation::NotCommitted,
            }),
            RoomDurableMutation::Destroy { .. } => Ok(if terminal_coordinates_committed {
                match exact_intent_reconciled {
                    Some(true) => CommitReconciliation::Committed,
                    Some(false) => CommitReconciliation::NotCommitted,
                    None => CommitReconciliation::Unknown,
                }
            } else {
                CommitReconciliation::NotCommitted
            }),
            RoomDurableMutation::DestroyAndReleaseClaim {
                completion_attempt: Some(_),
            } => Ok(match destroy_attempt_proof {
                Some(DestroyCompletionAttemptProof::Armed {
                    lifecycle: Some(lifecycle),
                }) if terminal_coordinates_committed
                    && lifecycle == coordinates.lifecycle
                    && exact_intent_reconciled == Some(true) =>
                {
                    CommitReconciliation::Committed
                }
                Some(DestroyCompletionAttemptProof::Armed {
                    lifecycle: Some(lifecycle),
                }) if terminal_coordinates_committed
                    && lifecycle == coordinates.lifecycle
                    && exact_intent_reconciled == Some(false) =>
                {
                    CommitReconciliation::NotCommitted
                }
                Some(DestroyCompletionAttemptProof::Missing)
                | Some(DestroyCompletionAttemptProof::Inert { .. })
                | Some(DestroyCompletionAttemptProof::Armed { .. }) => {
                    CommitReconciliation::Unknown
                }
                None => CommitReconciliation::Unknown,
            }),
            RoomDurableMutation::DestroyAndReleaseClaim { .. } => {
                if terminal_coordinates_committed {
                    return Ok(match exact_intent_reconciled {
                        Some(true) => CommitReconciliation::Committed,
                        Some(false) => CommitReconciliation::NotCommitted,
                        None => CommitReconciliation::Unknown,
                    });
                }

                // Coordinate-less unpublished cleanup only releases a
                // claim, leaving no durable lifecycle record to reconcile.
                // A missing original claim is ambiguous: it may mean this
                // transaction committed, or that a foreign owner replaced
                // it. Never let a stale owner turn that ambiguity into a
                // successful cleanup acknowledgement.
                Ok(CommitReconciliation::Unknown)
            }
            // An `Activate` is state-idempotent: when the lifecycle already
            // sits in `active` at the SAME coordinates, the caller's target
            // state holds whether its own transaction landed or was a no-op.
            // The stored proof may legitimately belong to a PREDECESSOR
            // owner's activation (an idempotent re-activate never overwrites
            // it), so a fence-bound fingerprint mismatch must not report
            // this success as NotCommitted and refuse a valid active room
            // (#1647, codex final round).
            RoomDurableMutation::Activate => Ok(
                if room_matches
                    && lifecycle_state
                        .as_ref()
                        .is_some_and(|(state, _, revision)| {
                            *revision == coordinates.revision.as_i64()
                                && state.as_str() == RoomLifecycleState::Active.as_db_str()
                        })
                {
                    CommitReconciliation::Committed
                } else if lifecycle_state.is_some() && room_matches {
                    match exact_intent_reconciled {
                        Some(true) => CommitReconciliation::Committed,
                        Some(false) => CommitReconciliation::NotCommitted,
                        None => CommitReconciliation::Unknown,
                    }
                } else {
                    CommitReconciliation::NotCommitted
                },
            ),
            _ => Ok(if lifecycle_state.is_some() && room_matches {
                match exact_intent_reconciled {
                    Some(true) => CommitReconciliation::Committed,
                    Some(false) => CommitReconciliation::NotCommitted,
                    None => CommitReconciliation::Unknown,
                }
            } else {
                CommitReconciliation::NotCommitted
            }),
        }
    }

    async fn commit_or_reconcile(
        &self,
        tx: Transaction<'_>,
        room_jid: &BareJid,
        fence: &RoomClaimFenceContext,
        intent: &RoomDurableMutation,
        coordinates: RoomCommittedCoordinates,
        reservation: Option<waddle_xmpp::muc::RoomEffectReservation>,
    ) -> Result<RoomCommitOutcome, RoomCommitError> {
        match tx.commit().await {
            Ok(()) => Ok(RoomCommitOutcome {
                coordinates,
                reservation,
            }),
            Err(error) => {
                if is_retryable_tx_error(&error) {
                    return Err(Self::commit_error(error));
                }
                let commit_error = Self::commit_error(error);
                match self
                    .reconcile_ambiguous_commit(room_jid, fence, intent, coordinates)
                    .await
                {
                    Ok(CommitReconciliation::Committed) => {
                        tracing::warn!(
                            room = %room_jid,
                            lifecycle = %coordinates.lifecycle,
                            revision = coordinates.revision.as_i64(),
                            "MUC durable commit acknowledgement was lost after durable coordinates committed"
                        );
                        Ok(RoomCommitOutcome {
                            coordinates,
                            reservation,
                        })
                    }
                    Ok(CommitReconciliation::NotCommitted) => Err(commit_error),
                    Ok(CommitReconciliation::Unknown) => {
                        tracing::warn!(
                            room = %room_jid,
                            lifecycle = %coordinates.lifecycle,
                            revision = coordinates.revision.as_i64(),
                            "MUC coordinate-less destroy cleanup has no durable commit acknowledgement proof"
                        );
                        Err(RoomCommitError::CommitOutcomeUnknown)
                    }
                    Err(reconciliation_error) => {
                        tracing::warn!(
                            room = %room_jid,
                            lifecycle = %coordinates.lifecycle,
                            revision = coordinates.revision.as_i64(),
                            error = ?reconciliation_error,
                            "MUC durable commit outcome is unknown because acknowledgement reconciliation failed"
                        );
                        Err(RoomCommitError::CommitOutcomeUnknown)
                    }
                }
            }
        }
    }

    async fn destroy_completion_blocks_create_in_tx(
        tx: &mut Transaction<'_>,
        room_jid: &BareJid,
    ) -> Result<bool, RoomCommitError> {
        let room_jid = room_jid.to_string();
        let mut rows = tx
            .query(
                "SELECT payload_json, available_at_ms FROM clustering_muc_destroy_outbox",
                (),
            )
            .await
            .map_err(Self::commit_error)?;
        while let Some(row) = rows.next().await.map_err(Self::commit_error)? {
            let payload: String = row.get(0).map_err(Self::commit_error)?;
            let available_at_ms: i64 = row.get(1).map_err(Self::commit_error)?;
            // `i64::MAX` is an inert, pre-commit reservation. It has no
            // durable proof that its destroy completed, so treating it as a
            // recreation fence would strand a newly-creatable room after a
            // creator crash. Only a durably armed completion can block.
            if available_at_ms == i64::MAX {
                continue;
            }
            let payload: serde_json::Value =
                serde_json::from_str(&payload).map_err(|_| commit_database_error())?;
            if payload.get("room_jid").and_then(serde_json::Value::as_str)
                == Some(room_jid.as_str())
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn persist_idempotent_mutation_fingerprint_in_tx(
        tx: &mut Transaction<'_>,
        lifecycle: &RoomLifecycleId,
        current_fingerprint: Option<&str>,
        intent_fingerprint: &str,
    ) -> Result<(), RoomCommitError> {
        // A lifecycle revision carries one durable proof.  An idempotent
        // transition must never replace the proof left by the transition that
        // created these coordinates: an older acknowledgement may still need
        // to reconcile its exact intent.  Only legacy NULL rows need a
        // backfill, which remains safe because they have no prior proof.
        if current_fingerprint.is_some() {
            return Ok(());
        }
        tx.execute(
            "UPDATE clustering_muc_room_lifecycles \
             SET mutation_fingerprint = ?, updated_at = now() \
             WHERE lifecycle_id = ?",
            crate::db_params![intent_fingerprint.to_string(), lifecycle.to_string()],
        )
        .await
        .map_err(Self::commit_error)?;
        Ok(())
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

    async fn enqueue_room_effects_in_tx(
        &self,
        tx: &mut Transaction<'_>,
        room_jid: &BareJid,
        intent: &RoomDurableMutation,
        coordinates: waddle_xmpp::muc::RoomCommittedCoordinates,
        effects: &RoomMutationEffects,
        producing_node: &crate::room_effect_outbox::RoomEffectProducingNode,
    ) -> Result<Option<waddle_xmpp::muc::RoomEffectReservation>, RoomCommitError> {
        let waddle_xmpp::muc::RoomCommittedCoordinates {
            lifecycle,
            revision,
        } = coordinates;
        let now_ms = crate::time::now_ms();
        if matches!(
            intent,
            RoomDurableMutation::Destroy { .. }
                | RoomDurableMutation::DestroyAndReleaseClaim { .. }
        ) {
            self.room_effect_outbox
                .supersede_non_terminal_in_tx(tx, lifecycle, now_ms)
                .await
                .map_err(|_| commit_database_error())?;
        }
        if let Some(reservation) = effects.superseding_reservation() {
            self.room_effect_outbox
                .supersede_reservation_in_tx(tx, reservation)
                .await
                .map_err(|_| commit_database_error())?;
        }
        let successor_config_codes = effects.effects().iter().find_map(|effect| match effect {
            waddle_xmpp::muc::RoomEffect::ConfigChanged { status_codes, .. } => {
                Some(status_codes.as_slice())
            }
            _ => None,
        });
        if matches!(intent, RoomDurableMutation::Config { .. }) {
            if let Some(successor_codes) = successor_config_codes {
                self.room_effect_outbox
                    .supersede_idempotent_config_in_tx(tx, lifecycle, successor_codes)
                    .await
                    .map_err(|_| commit_database_error())?;
            }
        }
        if effects.effects().is_empty() {
            return Ok(None);
        }
        let effects_room_jid = effects.room_jid().cloned();
        if effects_room_jid.as_ref() != Some(room_jid) {
            return Err(RoomCommitError::EffectRoomJidMismatch {
                mutation_room_jid: room_jid.clone(),
                effects_room_jid,
            });
        }
        let origin = crate::room_effect_outbox::room_effect_origin_instance_id();
        self.room_effect_outbox
            .enqueue_in_tx(
                tx,
                crate::room_effect_outbox::RoomEffectEnqueue {
                    lifecycle,
                    revision,
                    effects,
                    origin: &origin,
                    producing_node,
                    now_ms,
                },
            )
            .await
            .map(Some)
            .map_err(|_| commit_database_error())
    }

    async fn commit_room_mutation_once(
        &self,
        room_jid: &BareJid,
        fence: &RoomClaimFenceContext,
        intent: &RoomDurableMutation,
        effects: &RoomMutationEffects,
        authority: Option<&waddle_xmpp::ownership::CurrentNodeIdentityGuard>,
    ) -> Result<RoomCommitOutcome, RoomCommitError> {
        let identity_guard = if let Some(authority) = authority {
            if !self.node_identity.owns_guard(authority) || authority.identity() != &fence.owner {
                remove_room_claim_fence_if(&self.exact_claim_fences, room_jid, fence);
                remove_room_claim_fence_if(&self.published_claim_fences, room_jid, fence);
                return Err(RoomCommitError::NotOwner);
            }
            None
        } else {
            self.node_identity.guard_if_current(&fence.owner).await
        };
        if authority.is_none() && identity_guard.is_none() {
            remove_room_claim_fence_if(&self.exact_claim_fences, room_jid, fence);
            remove_room_claim_fence_if(&self.published_claim_fences, room_jid, fence);
            return Err(RoomCommitError::NotOwner);
        }
        let producing_node = if let Some(authority) = authority {
            crate::room_effect_outbox::RoomEffectProducingNode::from_node_identity(
                authority.identity().clone(),
            )
        } else if let Some(identity_guard) = identity_guard.as_ref() {
            crate::room_effect_outbox::RoomEffectProducingNode::from_node_identity(
                identity_guard.identity().clone(),
            )
        } else {
            return Err(RoomCommitError::NotOwner);
        };
        let mut tx = self
            .db
            .begin()
            .await
            .map_err(|_| RoomCommitError::OwnershipUnavailable)?;
        let exclusive_claim = matches!(
            intent,
            RoomDurableMutation::Destroy { .. }
                | RoomDurableMutation::DestroyAndReleaseClaim { .. }
                | RoomDurableMutation::Dormancy
                | RoomDurableMutation::Activate
                | RoomDurableMutation::MarkUnpublishedCleanup
        );
        if exclusive_claim {
            self.assert_fenced_for_update(&mut tx, room_jid, fence)
                .await?;
        } else {
            self.assert_commit_fenced(&mut tx, room_jid, fence, "FOR SHARE")
                .await?;
        }
        // The claim lock serializes this predicate with a destroy's
        // exclusive fenced transaction: once this create has proved no
        // completion exists, a matching destroy cannot commit its tombstone
        // and arm its completion until this transaction either commits or
        // rejects. This closes the actor-side pre-check TOCTOU window.
        if matches!(intent, RoomDurableMutation::Create { .. })
            && (Self::destroy_completion_blocks_create_in_tx(&mut tx, room_jid).await?
                || self
                    .room_effect_outbox
                    .has_pending_terminal_for_room_in_tx(&mut tx, room_jid)
                    .await
                    .map_err(|_| commit_database_error())?)
        {
            return Err(RoomCommitError::RecreationBlocked);
        }
        let intent_fingerprint = mutation_fingerprint(fence, intent)?;

        let mut rows = tx
            .query(
                "SELECT lifecycle_id, revision, state, mutation_fingerprint \
                 FROM clustering_muc_room_lifecycles \
                 WHERE room_jid = ? AND state IN ('preparing', 'active', 'dormant') FOR UPDATE",
                crate::db_params![room_jid.to_string()],
            )
            .await
            .map_err(Self::commit_error)?;
        let existing = rows.next().await.map_err(Self::commit_error)?;
        drop(rows);
        let lifecycle_row_exists = existing.is_some();

        let (lifecycle, revision, state, current_fingerprint) = if let Some(row) = existing {
            let lifecycle: String = row.get(0).map_err(Self::commit_error)?;
            let revision: i64 = row.get(1).map_err(Self::commit_error)?;
            let state: String = row.get(2).map_err(Self::commit_error)?;
            let current_fingerprint: Option<String> = row.get(3).map_err(Self::commit_error)?;
            let lifecycle = uuid::Uuid::parse_str(&lifecycle)
                .map(RoomLifecycleId::from_uuid)
                .map_err(|_| commit_database_error())?;
            let revision = RoomRevision::from_stored(revision).ok_or_else(commit_database_error)?;
            let state =
                RoomLifecycleState::from_db_str(&state).ok_or_else(commit_database_error)?;
            (lifecycle, revision, state, current_fingerprint)
        } else if matches!(intent, RoomDurableMutation::Create { .. }) {
            let lifecycle = RoomLifecycleId::generate();
            let revision = RoomRevision::initial();
            match tx
                .execute(
                    "INSERT INTO clustering_muc_room_lifecycles \
                     (lifecycle_id, room_jid, revision, state, mutation_fingerprint) \
                     VALUES (?, ?, ?, ?, ?)",
                    crate::db_params![
                        lifecycle.to_string(),
                        room_jid.to_string(),
                        revision.as_i64(),
                        RoomLifecycleState::Preparing.as_db_str(),
                        intent_fingerprint.clone(),
                    ],
                )
                .await
            {
                Ok(_) => (lifecycle, revision, RoomLifecycleState::Preparing, None),
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
                "INSERT INTO clustering_muc_room_lifecycles \
                 (lifecycle_id, room_jid, revision, state, mutation_fingerprint) \
                 VALUES (?, ?, ?, ?, ?)",
                crate::db_params![
                    lifecycle.to_string(),
                    room_jid.to_string(),
                    revision.as_i64(),
                    RoomLifecycleState::Active.as_db_str(),
                    intent_fingerprint.clone(),
                ],
            )
            .await
            .map_err(Self::commit_error)?;
            tx.execute(
                "UPDATE clustering_muc_rooms SET lifecycle_id = ?, revision = ?, updated_at = now() WHERE room_jid = ?",
                crate::db_params![lifecycle.to_string(), revision.as_i64(), room_jid.to_string()],
            )
            .await
            .map_err(Self::commit_error)?;
            (lifecycle, revision, RoomLifecycleState::Active, None)
        } else if matches!(intent, RoomDurableMutation::DestroyAndReleaseClaim { .. }) {
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
                drop(room_rows);
                let lifecycle = RoomLifecycleId::generate();
                let revision = RoomRevision::initial();
                tx.execute(
                    "INSERT INTO clustering_muc_room_lifecycles \
                     (lifecycle_id, room_jid, revision, state, mutation_fingerprint) \
                     VALUES (?, ?, ?, ?, ?)",
                    crate::db_params![
                        lifecycle.to_string(),
                        room_jid.to_string(),
                        revision.as_i64(),
                        RoomLifecycleState::Tombstoned.as_db_str(),
                        intent_fingerprint.clone(),
                    ],
                )
                .await
                .map_err(Self::commit_error)?;
                let reservation = self
                    .enqueue_room_effects_in_tx(
                        &mut tx,
                        room_jid,
                        intent,
                        waddle_xmpp::muc::RoomCommittedCoordinates {
                            lifecycle,
                            revision,
                        },
                        effects,
                        &producing_node,
                    )
                    .await?;
                self.release_claim_in_tx(&mut tx, room_jid, fence).await?;
                if let RoomDurableMutation::DestroyAndReleaseClaim {
                    completion_attempt: Some(attempt),
                } = intent
                {
                    let armed = tx
                        .execute(
                            "UPDATE clustering_muc_destroy_outbox \
                             SET lifecycle_id = ?, available_at_ms = ?, \
                                 lease_token = NULL, leased_at_ms = NULL \
                             WHERE attempt_id = ?",
                            crate::db_params![
                                lifecycle.to_string(),
                                crate::time::now_ms(),
                                attempt.as_uuid().to_string(),
                            ],
                        )
                        .await
                        .map_err(Self::commit_error)?;
                    if armed != 1 {
                        return Err(commit_database_error());
                    }
                }
                return self
                    .commit_or_reconcile(
                        tx,
                        room_jid,
                        fence,
                        intent,
                        RoomCommittedCoordinates {
                            lifecycle,
                            revision,
                        },
                        reservation,
                    )
                    .await;
            }
            drop(room_rows);
            let lifecycle = RoomLifecycleId::generate();
            let revision = RoomRevision::initial();
            tx.execute(
                "INSERT INTO clustering_muc_room_lifecycles \
                 (lifecycle_id, room_jid, revision, state, mutation_fingerprint) \
                 VALUES (?, ?, ?, ?, ?)",
                crate::db_params![
                    lifecycle.to_string(),
                    room_jid.to_string(),
                    revision.as_i64(),
                    RoomLifecycleState::Active.as_db_str(),
                    intent_fingerprint.clone(),
                ],
            )
            .await
            .map_err(Self::commit_error)?;
            (lifecycle, revision, RoomLifecycleState::Active, None)
        } else if matches!(intent, RoomDurableMutation::Destroy { .. }) {
            // A pre-lifecycle room row is a valid legacy state.  Destroy is
            // terminal, so it must claim that legacy incarnation and wipe it
            // while the exclusive claim lock is held; otherwise an Activate
            // serialized behind us could adopt and republish the row.
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
                "INSERT INTO clustering_muc_room_lifecycles \
                 (lifecycle_id, room_jid, revision, state, mutation_fingerprint) \
                 VALUES (?, ?, ?, ?, ?)",
                crate::db_params![
                    lifecycle.to_string(),
                    room_jid.to_string(),
                    revision.as_i64(),
                    RoomLifecycleState::Active.as_db_str(),
                    intent_fingerprint.clone(),
                ],
            )
            .await
            .map_err(Self::commit_error)?;
            (lifecycle, revision, RoomLifecycleState::Active, None)
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
            return if stored_waddle == waddle_id.as_str() && stored_channel == channel_id.as_str() {
                if !effects.effects().is_empty() {
                    return Err(RoomCommitError::EffectsRequireRevision);
                }
                self.commit_or_reconcile(
                    tx,
                    room_jid,
                    fence,
                    intent,
                    RoomCommittedCoordinates {
                        lifecycle,
                        revision,
                    },
                    None,
                )
                .await
            } else {
                Err(RoomCommitError::CreateConflict)
            };
        }
        if state == RoomLifecycleState::Dormant
            && !matches!(
                intent,
                RoomDurableMutation::Activate
                    | RoomDurableMutation::Destroy { .. }
                    | RoomDurableMutation::DestroyAndReleaseClaim { .. }
                    | RoomDurableMutation::Dormancy
            )
        {
            return Err(RoomCommitError::StateMissing);
        }
        if matches!(intent, RoomDurableMutation::Activate) && state == RoomLifecycleState::Active {
            // Idempotent: restoring an already-active lifecycle is not a state
            // transition, so it keeps its coordinates and bumps nothing. A
            // missing lifecycle stays a hard `StateMissing` above — callers
            // must not conflate the two.
            if !effects.effects().is_empty() {
                return Err(RoomCommitError::EffectsRequireRevision);
            }
            Self::persist_idempotent_mutation_fingerprint_in_tx(
                &mut tx,
                &lifecycle,
                current_fingerprint.as_deref(),
                &intent_fingerprint,
            )
            .await?;
            return self
                .commit_or_reconcile(
                    tx,
                    room_jid,
                    fence,
                    intent,
                    RoomCommittedCoordinates {
                        lifecycle,
                        revision,
                    },
                    None,
                )
                .await;
        }
        if matches!(intent, RoomDurableMutation::Dormancy) && state == RoomLifecycleState::Dormant {
            // An acknowledgement can be lost after the dormancy transaction
            // commits. Repeating the same terminal transition must converge
            // without bumping the durable coordinates.
            if !effects.effects().is_empty() {
                return Err(RoomCommitError::EffectsRequireRevision);
            }
            Self::persist_idempotent_mutation_fingerprint_in_tx(
                &mut tx,
                &lifecycle,
                current_fingerprint.as_deref(),
                &intent_fingerprint,
            )
            .await?;
            return self
                .commit_or_reconcile(
                    tx,
                    room_jid,
                    fence,
                    intent,
                    RoomCommittedCoordinates {
                        lifecycle,
                        revision,
                    },
                    None,
                )
                .await;
        }
        if matches!(intent, RoomDurableMutation::Publish) && state == RoomLifecycleState::Active {
            // Publishing is idempotent after an acknowledgement loss: once
            // the durable lifecycle is active, retrying must not advance it.
            if !effects.effects().is_empty() {
                return Err(RoomCommitError::EffectsRequireRevision);
            }
            Self::persist_idempotent_mutation_fingerprint_in_tx(
                &mut tx,
                &lifecycle,
                current_fingerprint.as_deref(),
                &intent_fingerprint,
            )
            .await?;
            return self
                .commit_or_reconcile(
                    tx,
                    room_jid,
                    fence,
                    intent,
                    RoomCommittedCoordinates {
                        lifecycle,
                        revision,
                    },
                    None,
                )
                .await;
        }
        if matches!(intent, RoomDurableMutation::MarkUnpublishedCleanup)
            && state == RoomLifecycleState::Preparing
        {
            // The handoff cleanup marker is itself acknowledgement-safe: a
            // retry after an ambiguous commit keeps the same coordinates and
            // leaves restart recovery able to find the room.
            if !effects.effects().is_empty() {
                return Err(RoomCommitError::EffectsRequireRevision);
            }
            Self::persist_idempotent_mutation_fingerprint_in_tx(
                &mut tx,
                &lifecycle,
                current_fingerprint.as_deref(),
                &intent_fingerprint,
            )
            .await?;
            return self
                .commit_or_reconcile(
                    tx,
                    room_jid,
                    fence,
                    intent,
                    RoomCommittedCoordinates {
                        lifecycle,
                        revision,
                    },
                    None,
                )
                .await;
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
                    "INSERT INTO clustering_muc_rooms (room_jid, waddle_id, channel_id, config_json, lifecycle_id, revision, config_lifecycle_id, config_revision) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                    crate::db_params![room_jid.to_string(), waddle_id.as_str().to_string(), channel_id.as_str().to_string(), config_json, lifecycle.to_string(), next_revision.as_i64(), lifecycle.to_string(), next_revision.as_i64()],
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
                    "UPDATE clustering_muc_rooms SET waddle_id = ?, channel_id = ?, config_json = ?, lifecycle_id = ?, revision = ?, config_lifecycle_id = ?, config_revision = ?, updated_at = now() WHERE room_jid = ?",
                    crate::db_params![waddle_id.as_str().to_string(), channel_id.as_str().to_string(), config_json, lifecycle.to_string(), next_revision.as_i64(), lifecycle.to_string(), next_revision.as_i64(), room_jid.to_string()],
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
                let affected = tx.execute("UPDATE clustering_muc_rooms SET config_json = ?, lifecycle_id = ?, revision = ?, config_lifecycle_id = ?, config_revision = ?, updated_at = now() WHERE room_jid = ?", crate::db_params![config_json, lifecycle.to_string(), next_revision.as_i64(), lifecycle.to_string(), next_revision.as_i64(), room_jid.to_string()]).await.map_err(Self::commit_error)?;
                if affected == 0 {
                    return Err(RoomCommitError::StateMissing);
                }
                for entry in affiliations {
                    Self::write_commit_affiliation(&mut tx, room_jid, entry).await?;
                }
            }
            RoomDurableMutation::Projection(_) => {
                let affected = tx.execute("UPDATE clustering_muc_rooms SET lifecycle_id = ?, revision = ?, updated_at = now() WHERE room_jid = ?", crate::db_params![lifecycle.to_string(), next_revision.as_i64(), room_jid.to_string()]).await.map_err(Self::commit_error)?;
                if affected == 0 {
                    return Err(RoomCommitError::StateMissing);
                }
            }
            RoomDurableMutation::Destroy { .. }
            | RoomDurableMutation::DestroyAndReleaseClaim { .. } => {
                tx.execute(
                    "DELETE FROM clustering_muc_room_affiliations WHERE room_jid = ?",
                    crate::db_params![room_jid.to_string()],
                )
                .await
                .map_err(Self::commit_error)?;
                if matches!(intent, RoomDurableMutation::DestroyAndReleaseClaim { .. }) {
                    self.release_claim_in_tx(&mut tx, room_jid, fence).await?;
                }
                tx.execute(
                    "DELETE FROM clustering_muc_rooms WHERE room_jid = ?",
                    crate::db_params![room_jid.to_string()],
                )
                .await
                .map_err(Self::commit_error)?;
            }
            RoomDurableMutation::Dormancy
            | RoomDurableMutation::Activate
            | RoomDurableMutation::Publish
            | RoomDurableMutation::MarkUnpublishedCleanup => {
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
        if let Some(attempt) = match intent {
            RoomDurableMutation::Destroy {
                completion_attempt: Some(attempt),
            }
            | RoomDurableMutation::DestroyAndReleaseClaim {
                completion_attempt: Some(attempt),
            } => Some(attempt),
            _ => None,
        } {
            let armed = tx
                .execute(
                    "UPDATE clustering_muc_destroy_outbox \
                     SET lifecycle_id = ?, available_at_ms = ?, lease_token = NULL, leased_at_ms = NULL \
                     WHERE attempt_id = ?",
                    crate::db_params![
                        lifecycle.to_string(),
                        crate::time::now_ms(),
                        attempt.as_uuid().to_string()
                    ],
                )
                .await
                .map_err(Self::commit_error)?;
            if armed != 1 {
                return Err(commit_database_error());
            }
        }
        let final_state = match intent {
            RoomDurableMutation::Destroy { .. }
            | RoomDurableMutation::DestroyAndReleaseClaim { .. } => RoomLifecycleState::Tombstoned,
            RoomDurableMutation::Dormancy => RoomLifecycleState::Dormant,
            RoomDurableMutation::Activate | RoomDurableMutation::Publish => {
                RoomLifecycleState::Active
            }
            RoomDurableMutation::MarkUnpublishedCleanup => RoomLifecycleState::Preparing,
            _ => state,
        };
        tx.execute(
            "UPDATE clustering_muc_room_lifecycles \
             SET revision = ?, state = ?, mutation_fingerprint = ?, updated_at = now() \
             WHERE lifecycle_id = ?",
            crate::db_params![
                next_revision.as_i64(),
                final_state.as_db_str(),
                intent_fingerprint,
                lifecycle.to_string()
            ],
        )
        .await
        .map_err(Self::commit_error)?;
        let reservation = self
            .enqueue_room_effects_in_tx(
                &mut tx,
                room_jid,
                intent,
                waddle_xmpp::muc::RoomCommittedCoordinates {
                    lifecycle,
                    revision: next_revision,
                },
                effects,
                &producing_node,
            )
            .await?;
        self.commit_or_reconcile(
            tx,
            room_jid,
            fence,
            intent,
            RoomCommittedCoordinates {
                lifecycle,
                revision: next_revision,
            },
            reservation,
        )
        .await
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
                "SELECT waddle_id, channel_id, config_json, subject_json, lifecycle_id, revision, config_lifecycle_id, config_revision FROM \
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
        let lifecycle_id: Option<String> = row.get(4).map_err(db_err)?;
        let revision: Option<i64> = row.get(5).map_err(db_err)?;
        let config_lifecycle_id: Option<String> = row.get(6).map_err(db_err)?;
        let config_revision: Option<i64> = row.get(7).map_err(db_err)?;
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
            coordinates: lifecycle_id
                .zip(revision)
                .map(|(lifecycle_id, revision)| {
                    let lifecycle = uuid::Uuid::parse_str(&lifecycle_id)
                        .map(RoomLifecycleId::from_uuid)
                        .map_err(|_| {
                            XmppError::internal("durable room lifecycle decode failed".to_string())
                        })?;
                    let revision = RoomRevision::from_stored(revision).ok_or_else(|| {
                        XmppError::internal("durable room revision decode failed".to_string())
                    })?;
                    Ok::<RoomCommittedCoordinates, XmppError>(RoomCommittedCoordinates {
                        lifecycle,
                        revision,
                    })
                })
                .transpose()?,
            config_coordinates: config_lifecycle_id
                .zip(config_revision)
                .map(|(lifecycle_id, revision)| {
                    let lifecycle = uuid::Uuid::parse_str(&lifecycle_id)
                        .map(RoomLifecycleId::from_uuid)
                        .map_err(|_| {
                            XmppError::internal(
                                "durable room config lifecycle decode failed".to_string(),
                            )
                        })?;
                    let revision = RoomRevision::from_stored(revision).ok_or_else(|| {
                        XmppError::internal(
                            "durable room config revision decode failed".to_string(),
                        )
                    })?;
                    Ok::<RoomCommittedCoordinates, XmppError>(RoomCommittedCoordinates {
                        lifecycle,
                        revision,
                    })
                })
                .transpose()?,
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
        effects: RoomMutationEffects,
    ) -> RoomCommitFuture<'a> {
        Box::pin(async move {
            for attempt in 0..ROOM_COMMIT_RETRY_ATTEMPTS {
                match self
                    .commit_room_mutation_once(room_jid, fence, &intent, &effects, None)
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

    fn commit_room_mutation_with_authority<'a>(
        &'a self,
        room_jid: &'a BareJid,
        fence: &'a RoomClaimFenceContext,
        intent: RoomDurableMutation,
        effects: RoomMutationEffects,
        authority: &'a waddle_xmpp::ownership::CurrentNodeIdentityGuard,
    ) -> RoomCommitFuture<'a> {
        Box::pin(async move {
            for attempt in 0..ROOM_COMMIT_RETRY_ATTEMPTS {
                match self
                    .commit_room_mutation_once(room_jid, fence, &intent, &effects, Some(authority))
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

    fn destroy_completion_blocks_recreation<'a>(
        &'a self,
        room_jid: &'a BareJid,
    ) -> MucDurableFuture<'a, bool> {
        Box::pin(async move {
            let conn = self.db.guard().await.map_err(db_err)?;
            let mut rows = conn
                .query(
                    "SELECT payload_json, available_at_ms FROM clustering_muc_destroy_outbox",
                    (),
                )
                .await
                .map_err(db_err)?;
            while let Some(row) = rows.next().await.map_err(db_err)? {
                let payload: String = row.get(0).map_err(db_err)?;
                let available_at_ms: i64 = row.get(1).map_err(db_err)?;
                if available_at_ms == i64::MAX {
                    continue;
                }
                let payload: PersistedDestroyCompletionRoom = serde_json::from_str(&payload)
                    .map_err(|error| {
                        XmppError::internal(format!(
                            "durable destroy completion payload decode failed: {error}"
                        ))
                    })?;
                if payload.room_jid == *room_jid {
                    return Ok(true);
                }
            }
            Ok(false)
        })
    }

    fn recover_inert_destroy_completion<'a>(
        &'a self,
        attempt: &'a DestroyAttemptId,
    ) -> MucDurableFuture<'a, bool> {
        Box::pin(async move {
            let conn = self.db.guard().await.map_err(db_err)?;
            let mut rows = conn
                .query(
                    "SELECT EXISTS ( \
                     SELECT 1 \
                     FROM clustering_muc_destroy_outbox AS outbox \
                     JOIN clustering_muc_room_lifecycles AS lifecycle \
                       ON lifecycle.lifecycle_id = outbox.lifecycle_id \
                     WHERE outbox.attempt_id = ? \
                       AND outbox.available_at_ms = ? \
                       AND lifecycle.state = ? \
                       AND NOT EXISTS ( \
                         SELECT 1 \
                         FROM clustering_muc_rooms AS room \
                         WHERE room.room_jid = lifecycle.room_jid \
                       ) \
                     )",
                    crate::db_params![
                        attempt.as_uuid().to_string(),
                        i64::MAX,
                        RoomLifecycleState::Tombstoned.as_db_str(),
                    ],
                )
                .await
                .map_err(db_err)?;
            let row = rows.next().await.map_err(db_err)?.ok_or_else(|| {
                XmppError::internal("durable inert completion recovery returned no result")
            })?;
            row.get(0).map_err(db_err)
        })
    }

    fn find_preparing_room<'a>(
        &'a self,
        room_jid: &'a BareJid,
    ) -> MucDurableFuture<'a, Option<RoomCommittedCoordinates>> {
        Box::pin(async move {
            let conn = self.db.guard().await.map_err(db_err)?;
            let mut rows = conn
                .query(
                    "SELECT lifecycle_id, revision FROM clustering_muc_room_lifecycles \
                     WHERE room_jid = ? AND state = ?",
                    crate::db_params![
                        room_jid.to_string(),
                        RoomLifecycleState::Preparing.as_db_str(),
                    ],
                )
                .await
                .map_err(db_err)?;
            let Some(row) = rows.next().await.map_err(db_err)? else {
                return Ok(None);
            };
            let lifecycle: String = row.get(0).map_err(db_err)?;
            let revision: i64 = row.get(1).map_err(db_err)?;
            let lifecycle = uuid::Uuid::parse_str(&lifecycle)
                .map(RoomLifecycleId::from_uuid)
                .map_err(|error| {
                    XmppError::internal(format!(
                        "durable preparing room lifecycle decode failed: {error}"
                    ))
                })?;
            let revision = RoomRevision::from_stored(revision).ok_or_else(|| {
                XmppError::internal("durable preparing room revision is outside its valid range")
            })?;
            Ok(Some(RoomCommittedCoordinates {
                lifecycle,
                revision,
            }))
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
    use std::io;
    use std::sync::Arc;
    use waddle_xmpp::muc::durable::{
        ChannelId, MucOccupantNick, OccupancyLeaveCause, RoomPinProjection, RoomProjection,
        WaddleId,
    };
    use waddle_xmpp::muc::room_actor::{
        GetSnapshot, RestoreDurableRoomState, RoomActor, SetSubject, SetSubjectError,
    };
    use waddle_xmpp::muc::RoomSubjectTexts;
    use waddle_xmpp::muc::{
        MucConfigStatusCode, MucRoom, RoomConfig, RoomLifecycleState, RoomMutationEffects,
        RoomRevision,
    };
    use waddle_xmpp::ownership::{ClaimStore, NodeIdentity, StalePredicate};
    use waddle_xmpp::xep::xep0421::OccupantIdSecret;

    fn node_identity() -> NodeIdentity {
        NodeIdentity::new(
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
        )
    }

    fn occupancy_join_projection() -> RoomDurableMutation {
        RoomDurableMutation::Projection(RoomProjection::OccupancyJoin {
            occupant: "alice@example.com/desktop".parse().expect("valid full JID"),
            nick: MucOccupantNick::new("alice".to_string()).expect("valid nick"),
        })
    }

    struct ProjectionTestRoom {
        store: PostgresMucRoomStore,
        db: Database,
        room_jid: BareJid,
        fence: RoomClaimFenceContext,
        create: RoomCommitOutcome,
    }

    async fn projection_test_room(prefix: &str) -> Option<ProjectionTestRoom> {
        let (store, claim_store, db, me) = clean_store().await?;
        let room_jid = unique_room_jid(prefix);
        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        let epoch = claim_store
            .ensure_claimed(&entity, &me)
            .await
            .expect("claim room");
        let fence = RoomClaimFenceContext::new(entity, me.clone(), epoch);
        store.record_claim_fence(&room_jid, fence.clone());
        let create = store
            .commit_room_mutation(
                &room_jid,
                &fence,
                RoomDurableMutation::Create {
                    waddle_id: waddle_id("waddle-projection-test"),
                    channel_id: channel_id("channel-projection-test"),
                    config: RoomConfig::default(),
                    initial_affiliations: vec![],
                },
                RoomMutationEffects::none(),
            )
            .await
            .expect("create room");
        Some(ProjectionTestRoom {
            store,
            db,
            room_jid,
            fence,
            create,
        })
    }

    fn projection_config_intent(name: &str) -> RoomDurableMutation {
        RoomDurableMutation::Config {
            config: RoomConfig {
                name: name.to_string(),
                ..RoomConfig::default()
            },
            waddle_id: waddle_id("waddle-projection-test"),
            channel_id: channel_id("channel-projection-test"),
        }
    }

    fn projection_config_effects(room_jid: BareJid) -> RoomMutationEffects {
        RoomMutationEffects::config(
            room_jid,
            vec![MucConfigStatusCode::NonPrivacyConfigurationChange],
            vec![],
        )
    }

    #[test]
    fn projection_fingerprints_are_stable_and_distinct() {
        let occupant: jid::FullJid = "alice@example.com/desktop".parse().expect("valid full JID");
        let nick = MucOccupantNick::new("alice".to_string()).expect("valid nick");
        let stanza_id = waddle_xmpp_core::xep0359::StanzaId::new(
            "archive-1",
            "muc.example.com".parse().expect("valid assigning JID"),
        );
        let projections = [
            RoomProjection::OccupancyJoin {
                occupant: occupant.clone(),
                nick: nick.clone(),
            },
            RoomProjection::OccupancyLeave {
                occupant,
                nick,
                cause: OccupancyLeaveCause::Explicit,
            },
            RoomProjection::Pin(RoomPinProjection::Pin {
                target: stanza_id.clone(),
            }),
            RoomProjection::Pin(RoomPinProjection::Unpin { target: stanza_id }),
        ];
        let fence = RoomClaimFenceContext::new(
            Entity::new(
                EntityType::RoomActor,
                "projection-shape@muc.example.com".to_string(),
            ),
            node_identity(),
            ClaimEpoch(1),
        );
        let fingerprints = projections.map(|projection| {
            mutation_fingerprint(&fence, &RoomDurableMutation::Projection(projection))
                .expect("projection fingerprint")
        });
        assert_eq!(
            fingerprints[0],
            mutation_fingerprint(&fence, &occupancy_join_projection())
                .expect("repeat join fingerprint"),
            "the same typed projection must retain its acknowledgement fingerprint"
        );
        let kinds = fingerprints
            .iter()
            .map(|fingerprint| {
                serde_json::from_str::<serde_json::Value>(fingerprint).expect("fingerprint JSON")
                    ["intent"]["projection"]["kind"]
                    .as_str()
                    .expect("projection kind")
                    .to_string()
            })
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(
            kinds.len(),
            4,
            "each projection kind needs its own fingerprint"
        );
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

    fn waddle_id(value: &str) -> WaddleId {
        WaddleId::new(value.to_string())
    }

    fn channel_id(value: &str) -> ChannelId {
        ChannelId::new(value.to_string())
    }

    async fn insert_tombstoned_lifecycle(
        db: &Database,
        room_jid: &BareJid,
        lifecycle: RoomLifecycleId,
        revision: RoomRevision,
    ) {
        db.guard()
            .await
            .expect("guard")
            .execute(
                "INSERT INTO clustering_muc_room_lifecycles \
                 (lifecycle_id, room_jid, revision, state, mutation_fingerprint) \
                 VALUES (?, ?, ?, ?, ?)",
                crate::db_params![
                    lifecycle.to_string(),
                    room_jid.to_string(),
                    revision.as_i64(),
                    RoomLifecycleState::Tombstoned.as_db_str(),
                    format!("test-tombstone-{lifecycle}"),
                ],
            )
            .await
            .expect("insert tombstoned lifecycle");
    }

    async fn enqueue_terminal_effect(
        db: &Database,
        room_jid: &BareJid,
        lifecycle: RoomLifecycleId,
        revision: RoomRevision,
    ) -> waddle_xmpp::muc::RoomEffectReservation {
        let outbox = crate::room_effect_outbox::RoomEffectOutboxStore::new(db.clone())
            .await
            .expect("open room effect outbox");
        let mut tx = outbox.database().begin().await.expect("begin outbox tx");
        let reservation = outbox
            .enqueue_in_tx(
                &mut tx,
                crate::room_effect_outbox::RoomEffectEnqueue {
                    lifecycle,
                    revision,
                    effects: &RoomMutationEffects::destroy(
                        room_jid.clone(),
                        None,
                        None,
                        None,
                        Vec::new(),
                    ),
                    origin: &crate::room_effect_outbox::RoomEffectOriginInstanceId::new(
                        "muc-durable-test-origin".to_string(),
                    )
                    .expect("valid room effect origin"),
                    producing_node:
                        &crate::room_effect_outbox::RoomEffectProducingNode::from_node_identity(
                            node_identity(),
                        ),
                    now_ms: crate::time::now_ms(),
                },
            )
            .await
            .expect("enqueue terminal effect");
        tx.commit().await.expect("commit terminal effect");
        reservation
    }

    #[derive(Clone, Default)]
    struct CaptureWriter(Arc<std::sync::Mutex<Vec<u8>>>);

    impl io::Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().expect("capture buffer lock").extend(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
        type Writer = CaptureWriter;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    fn captured_logs(buffer: &Arc<std::sync::Mutex<Vec<u8>>>) -> String {
        String::from_utf8(buffer.lock().expect("capture buffer lock").clone())
            .expect("captured logs are valid UTF-8")
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

    #[test]
    fn non_retryable_commit_errors_are_logged_before_sanitizing() {
        let buffer = Arc::new(std::sync::Mutex::new(Vec::new()));
        let writer = CaptureWriter(Arc::clone(&buffer));
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .with_target(false)
            .with_writer(writer)
            .with_max_level(tracing::Level::WARN)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let result = PostgresMucRoomStore::commit_error(DatabaseError::ConnectionFailed(
            "main pool unavailable".to_string(),
        ));

        assert!(
            matches!(result, RoomCommitError::Database(_)),
            "expected commit_error to keep returning the sanitized database marker"
        );
        let logs = captured_logs(&buffer);
        assert!(
            logs.contains(
                "MUC durable commit failed with a non-retryable database error; returning sanitized RoomCommitError"
            ),
            "expected the non-retryable commit warning in logs, got:\n{logs}"
        );
        assert!(
            logs.contains("error=ConnectionFailed(\"main pool unavailable\")"),
            "expected the structured DatabaseError in logs, got:\n{logs}"
        );
    }

    #[test]
    fn non_retryable_fence_query_errors_are_logged_before_sanitizing() {
        let buffer = Arc::new(std::sync::Mutex::new(Vec::new()));
        let writer = CaptureWriter(Arc::clone(&buffer));
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .with_target(false)
            .with_writer(writer)
            .with_max_level(tracing::Level::WARN)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let result = PostgresMucRoomStore::commit_fence_error(DatabaseError::ConnectionFailed(
            "main pool unavailable".to_string(),
        ));

        assert!(
            matches!(result, RoomCommitError::OwnershipUnavailable),
            "expected fence failures to retain their typed unavailable outcome"
        );
        let logs = captured_logs(&buffer);
        assert!(
            logs.contains(
                "MUC durable ownership fence query failed with a non-retryable database error; returning OwnershipUnavailable"
            ),
            "expected the non-retryable fence-query warning in logs, got:\n{logs}"
        );
        assert!(
            logs.contains("error=ConnectionFailed(\"main pool unavailable\")"),
            "expected the structured DatabaseError in logs, got:\n{logs}"
        );
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
        let room_effect_outbox = crate::room_effect_outbox::RoomEffectOutboxStore::new(db.clone())
            .await
            .expect("open room effect outbox");
        let store = PostgresMucRoomStore {
            db,
            room_effect_outbox,
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
                        waddle_id: waddle_id("waddle"),
                        channel_id: channel_id("channel"),
                        config: config.clone(),
                        initial_affiliations: Vec::new(),
                    },
                    waddle_xmpp::muc::RoomMutationEffects::none()
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
        conn.execute("DELETE FROM clustering_muc_destroy_outbox", ())
            .await
            .expect("clean destroy completion outbox");
        conn.execute("DELETE FROM clustering_muc_room_effects", ())
            .await
            .expect("clean room effect outbox");
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
        conn.execute("DROP TABLE IF EXISTS clustering_muc_destroy_outbox", ())
            .await
            .expect("drop destroy completion outbox");
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
                    waddle_id: waddle_id("waddle-first"),
                    channel_id: channel_id("channel-first"),
                    config: config.clone(),
                    initial_affiliations: vec![waddle_xmpp::muc::durable::AffiliationEntry::new(
                        owner.clone(),
                        Some(Affiliation::Owner),
                    )],
                },
                waddle_xmpp::muc::RoomMutationEffects::none(),
            )
            .await
            .expect("first write must commit");

        assert_eq!(coords.coordinates.revision, RoomRevision::initial());

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
        assert_eq!(coords.coordinates.lifecycle.to_string(), lifecycle_id);
        assert_eq!(revision, RoomRevision::initial().as_i64());
        assert_eq!(state, "preparing");

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
        assert_eq!(room_lifecycle_id, coords.coordinates.lifecycle.to_string());
        let room_revision =
            RoomRevision::from_stored(room_revision).expect("room revision must be decodable");
        assert_eq!(room_revision, coords.coordinates.revision);

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
    async fn create_stays_preparing_until_exact_fenced_publish_commits() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, db, me)) = clean_store().await else {
            return;
        };
        let room_jid = unique_room_jid("durable-preparing-publish");
        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        let epoch = claim_store
            .ensure_claimed(&entity, &me)
            .await
            .expect("claim room");
        let fence = RoomClaimFenceContext::new(entity, me, epoch);
        store.record_claim_fence(&room_jid, fence.clone());
        let prepared = store
            .commit_room_mutation(
                &room_jid,
                &fence,
                RoomDurableMutation::Create {
                    waddle_id: waddle_id("waddle-preparing"),
                    channel_id: channel_id("channel-preparing"),
                    config: RoomConfig::default(),
                    initial_affiliations: vec![],
                },
                waddle_xmpp::muc::RoomMutationEffects::none(),
            )
            .await
            .expect("create durable preparing room");
        assert_eq!(
            store
                .find_preparing_room(&room_jid)
                .await
                .expect("find preparing room"),
            Some(prepared.coordinates)
        );

        // A Publish acknowledgement can be lost before any transition was
        // committed. Cleanup must converge on the still-preparing durable
        // state so its exact-fenced terminal destroy remains restart-safe.
        assert_eq!(
            store
                .commit_room_mutation(
                    &room_jid,
                    &fence,
                    RoomDurableMutation::MarkUnpublishedCleanup,
                    waddle_xmpp::muc::RoomMutationEffects::none()
                )
                .await
                .expect("mark cleanup after an uncommitted publish"),
            prepared
        );

        let published = store
            .commit_room_mutation(
                &room_jid,
                &fence,
                RoomDurableMutation::Publish,
                waddle_xmpp::muc::RoomMutationEffects::none(),
            )
            .await
            .expect("publish preparing room");
        assert_eq!(
            published.coordinates.lifecycle,
            prepared.coordinates.lifecycle
        );
        assert_eq!(
            published.coordinates.revision.as_i64(),
            prepared.coordinates.revision.as_i64() + 1
        );
        assert!(store
            .find_preparing_room(&room_jid)
            .await
            .expect("published room no longer preparing")
            .is_none());
        assert_eq!(
            store
                .commit_room_mutation(
                    &room_jid,
                    &fence,
                    RoomDurableMutation::Publish,
                    waddle_xmpp::muc::RoomMutationEffects::none()
                )
                .await
                .expect("publish acknowledgement retry converges"),
            published
        );
        assert_eq!(
            store
                .commit_room_mutation(
                    &room_jid,
                    &fence,
                    RoomDurableMutation::Activate,
                    waddle_xmpp::muc::RoomMutationEffects::none()
                )
                .await
                .expect("an active room activation retry converges"),
            published,
            "a differently-shaped idempotent retry must keep the published coordinates"
        );
        let publish_fingerprint = mutation_fingerprint(&fence, &RoomDurableMutation::Publish)
            .expect("publish fingerprint");
        let mut fingerprint_rows = db
            .guard()
            .await
            .expect("guard")
            .query(
                "SELECT mutation_fingerprint FROM clustering_muc_room_lifecycles WHERE lifecycle_id = ?",
                crate::db_params![published.coordinates.lifecycle.to_string()],
            )
            .await
            .expect("query published lifecycle fingerprint");
        let fingerprint_row = fingerprint_rows
            .next()
            .await
            .expect("published lifecycle fingerprint row")
            .expect("published lifecycle exists");
        assert_eq!(
            fingerprint_row
                .get::<Option<String>>(0)
                .expect("published fingerprint"),
            Some(publish_fingerprint),
            "idempotent activation must preserve the prior publish proof"
        );
        assert_eq!(
            store
                .reconcile_ambiguous_commit(
                    &room_jid,
                    &fence,
                    &RoomDurableMutation::Publish,
                    published.coordinates,
                )
                .await
                .expect("reconcile original publish"),
            CommitReconciliation::Committed,
            "a lost publish acknowledgement must remain reconcilable after an idempotent activation"
        );

        let cleanup_marked = store
            .commit_room_mutation(
                &room_jid,
                &fence,
                RoomDurableMutation::MarkUnpublishedCleanup,
                waddle_xmpp::muc::RoomMutationEffects::none(),
            )
            .await
            .expect("mark handoff cancellation before terminal cleanup");
        assert_eq!(
            cleanup_marked.coordinates.lifecycle,
            published.coordinates.lifecycle
        );
        assert_eq!(
            cleanup_marked.coordinates.revision.as_i64(),
            published.coordinates.revision.as_i64() + 1
        );
        assert_eq!(
            store
                .find_preparing_room(&room_jid)
                .await
                .expect("marked cleanup remains recoverable after restart"),
            Some(cleanup_marked.coordinates)
        );
        assert_eq!(
            store
                .commit_room_mutation(
                    &room_jid,
                    &fence,
                    RoomDurableMutation::MarkUnpublishedCleanup,
                    waddle_xmpp::muc::RoomMutationEffects::none()
                )
                .await
                .expect("cleanup marker acknowledgement retry converges"),
            cleanup_marked
        );
        store
            .commit_room_mutation(
                &room_jid,
                &fence,
                RoomDurableMutation::DestroyAndReleaseClaim {
                    completion_attempt: None,
                },
                waddle_xmpp::muc::RoomMutationEffects::none(),
            )
            .await
            .expect("terminal cleanup consumes the durable marker and claim");
        assert!(store
            .find_preparing_room(&room_jid)
            .await
            .expect("terminal cleanup removes preparing recovery marker")
            .is_none());
    }

    #[tokio::test]
    async fn destroy_commit_arms_its_completion_outbox_atomically() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, db, me)) = clean_store().await else {
            return;
        };
        let conn = db.guard().await.expect("guard");

        let room_jid = unique_room_jid("destroy-completion-transaction");
        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        let epoch = claim_store
            .ensure_claimed(&entity, &me)
            .await
            .expect("claim");
        let fence = RoomClaimFenceContext::new(entity, me, epoch);
        store.record_claim_fence(&room_jid, fence.clone());
        let created = store
            .commit_room_mutation(
                &room_jid,
                &fence,
                RoomDurableMutation::Create {
                    waddle_id: waddle_id("waddle"),
                    channel_id: channel_id("channel"),
                    config: RoomConfig::default(),
                    initial_affiliations: vec![],
                },
                waddle_xmpp::muc::RoomMutationEffects::none(),
            )
            .await
            .expect("create room before destroy");

        let attempt = waddle_xmpp::muc::DestroyAttemptId::generate();
        assert!(matches!(
            store
                .commit_room_mutation(
                    &room_jid,
                    &fence,
                    RoomDurableMutation::Destroy {
                        completion_attempt: Some(attempt),
                    },
                    waddle_xmpp::muc::RoomMutationEffects::none()
                )
                .await,
            Err(RoomCommitError::Database(_))
        ));

        let mut room_rows = conn
            .query(
                "SELECT COUNT(*) FROM clustering_muc_rooms WHERE room_jid = ?",
                crate::db_params![room_jid.to_string()],
            )
            .await
            .expect("query rolled-back room");
        assert_eq!(
            room_rows
                .next()
                .await
                .expect("read rolled-back room")
                .expect("rolled-back room count")
                .get::<i64>(0)
                .expect("decode rolled-back room count"),
            1,
            "a missing completion record must roll the terminal destroy back"
        );
        drop(room_rows);

        conn.execute(
            "INSERT INTO clustering_muc_destroy_outbox \
             (attempt_id, payload_json, available_at_ms, lease_token, leased_at_ms) \
             VALUES (?, '{}', ?, NULL, NULL)",
            crate::db_params![attempt.as_uuid().to_string(), i64::MAX],
        )
        .await
        .expect("persist inert completion before destroy");
        store
            .commit_room_mutation(
                &room_jid,
                &fence,
                RoomDurableMutation::Destroy {
                    completion_attempt: Some(attempt),
                },
                waddle_xmpp::muc::RoomMutationEffects::none(),
            )
            .await
            .expect("destroy commits with its completion record");

        let mut outbox_rows = conn
            .query(
                "SELECT available_at_ms, lifecycle_id FROM clustering_muc_destroy_outbox WHERE attempt_id = ?",
                crate::db_params![attempt.as_uuid().to_string()],
            )
            .await
            .expect("query armed completion");
        let outbox_row = outbox_rows
            .next()
            .await
            .expect("read armed completion")
            .expect("armed completion row");
        let available_at_ms: i64 = outbox_row.get(0).expect("decode completion availability");
        assert_ne!(
            available_at_ms,
            i64::MAX,
            "the completion must become recoverable with the committed tombstone"
        );
        let lifecycle_id: String = outbox_row.get(1).expect("decode completion lifecycle");
        assert_eq!(
            lifecycle_id,
            created.coordinates.lifecycle.to_string(),
            "the completion must be fenced to the lifecycle its destroy tombstoned"
        );
        drop(outbox_rows);

        conn.execute(
            "UPDATE clustering_muc_destroy_outbox SET available_at_ms = ? WHERE attempt_id = ?",
            crate::db_params![i64::MAX, attempt.as_uuid().to_string()],
        )
        .await
        .expect("restore inert completion after simulated crash");
        assert!(store
            .recover_inert_destroy_completion(&attempt)
            .await
            .expect("proven terminal destroy can re-arm its inert completion"));

        let unproven_attempt = DestroyAttemptId::generate();
        conn.execute(
            "INSERT INTO clustering_muc_destroy_outbox \
             (attempt_id, payload_json, available_at_ms, lease_token, leased_at_ms) \
             VALUES (?, '{}', ?, NULL, NULL)",
            crate::db_params![unproven_attempt.as_uuid().to_string(), i64::MAX],
        )
        .await
        .expect("persist unproven inert completion");
        assert!(!store
            .recover_inert_destroy_completion(&unproven_attempt)
            .await
            .expect("unproven inert completion cannot be armed"));
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
                waddle_id: waddle_id("waddle"),
                channel_id: channel_id("channel"),
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
            RoomDurableMutation::Destroy {
                completion_attempt: None,
            },
        ] {
            assert!(
                matches!(
                    store
                        .commit_room_mutation(
                            &room_jid,
                            &fence,
                            intent,
                            waddle_xmpp::muc::RoomMutationEffects::none()
                        )
                        .await,
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
                    waddle_id: waddle_id("waddle"),
                    channel_id: channel_id("channel"),
                    config: RoomConfig::default(),
                    initial_affiliations: vec![],
                },
                waddle_xmpp::muc::RoomMutationEffects::none(),
            )
            .await
            .expect("create seeds lifecycle");
        store
            .commit_room_mutation(
                &room_jid,
                &fence,
                RoomDurableMutation::Dormancy,
                waddle_xmpp::muc::RoomMutationEffects::none(),
            )
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
                            waddle_id: waddle_id("waddle"),
                            channel_id: channel_id("channel"),
                        },
                        waddle_xmpp::muc::RoomMutationEffects::none()
                    )
                    .await,
                Err(RoomCommitError::StateMissing)
            ),
            "ordinary mutation must not mutate a dormant room"
        );
    }

    #[tokio::test]
    async fn pending_destroy_releases_its_claim_before_a_late_create_can_commit() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, db, me)) = clean_store().await else {
            return;
        };
        let room_jid = unique_room_jid("lane-c7-pending-destroy");
        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        let epoch = claim_store
            .ensure_claimed(&entity, &me)
            .await
            .expect("claim");
        let fence = RoomClaimFenceContext::new(entity.clone(), me, epoch);
        store.record_claim_fence(&room_jid, fence.clone());

        store
            .commit_room_mutation(
                &room_jid,
                &fence,
                RoomDurableMutation::DestroyAndReleaseClaim {
                    completion_attempt: None,
                },
                waddle_xmpp::muc::RoomMutationEffects::none(),
            )
            .await
            .expect("pending destroy atomically consumes its claim");
        assert!(claim_store
            .current_claim(&entity)
            .await
            .expect("claim lookup")
            .is_none());
        assert!(matches!(
            store
                .commit_room_mutation(
                    &room_jid,
                    &fence,
                    RoomDurableMutation::Create {
                        waddle_id: waddle_id("waddle-late-create"),
                        channel_id: channel_id("channel-late-create"),
                        config: RoomConfig::default(),
                        initial_affiliations: vec![],
                    },
                    waddle_xmpp::muc::RoomMutationEffects::none()
                )
                .await,
            Err(RoomCommitError::NotOwner)
        ));
        let mut rows = db
            .guard()
            .await
            .expect("guard")
            .query(
                "SELECT count(*) FROM clustering_muc_rooms WHERE room_jid = ?",
                crate::db_params![room_jid.to_string()],
            )
            .await
            .expect("query room rows");
        let count: i64 = rows
            .next()
            .await
            .expect("room count")
            .expect("room count row")
            .get(0)
            .expect("decode room count");
        assert_eq!(count, 0, "late Create cannot resurrect durable room state");
    }

    #[tokio::test]
    async fn activate_transitions_dormant_to_active_adopts_pre_lifecycle_rooms_and_rejects_missing_state(
    ) {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, db, me)) = clean_store().await else {
            return;
        };
        let store = std::sync::Arc::new(store);
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
                    waddle_id: waddle_id("waddle-activate"),
                    channel_id: channel_id("channel-activate"),
                    config: RoomConfig::default(),
                    initial_affiliations: vec![],
                },
                waddle_xmpp::muc::RoomMutationEffects::none(),
            )
            .await
            .expect("seed room");
        let dormant = store
            .commit_room_mutation(
                &room_jid,
                &fence,
                RoomDurableMutation::Dormancy,
                waddle_xmpp::muc::RoomMutationEffects::none(),
            )
            .await
            .expect("dormancy transition");
        assert_eq!(dormant.coordinates.revision.as_i64(), 2);

        let redormant = store
            .commit_room_mutation(
                &room_jid,
                &fence,
                RoomDurableMutation::Dormancy,
                waddle_xmpp::muc::RoomMutationEffects::none(),
            )
            .await
            .expect("repeating dormancy converges after an ambiguous acknowledgement");
        assert_eq!(
            redormant, dormant,
            "idempotent dormancy must not bump revision"
        );

        let active = store
            .commit_room_mutation(
                &room_jid,
                &fence,
                RoomDurableMutation::Activate,
                waddle_xmpp::muc::RoomMutationEffects::none(),
            )
            .await
            .expect("activate transition");
        assert_eq!(active.coordinates.revision.as_i64(), 3);

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
            active.coordinates.lifecycle.to_string()
        );
        assert_eq!(room_row.get::<i64>(1).expect("room revision"), 3);

        let reactivated = store
            .commit_room_mutation(
                &room_jid,
                &fence,
                RoomDurableMutation::Activate,
                waddle_xmpp::muc::RoomMutationEffects::none(),
            )
            .await
            .expect("re-activating an active lifecycle is idempotent");
        assert_eq!(
            reactivated.coordinates.lifecycle,
            active.coordinates.lifecycle
        );
        assert_eq!(
            reactivated.coordinates.revision.as_i64(),
            3,
            "idempotent activation must not bump the revision"
        );
        let activate_fingerprint = mutation_fingerprint(&fence, &RoomDurableMutation::Activate)
            .expect("activate fingerprint");
        let mut reactivated_rows = db
            .guard()
            .await
            .expect("guard")
            .query(
                "SELECT mutation_fingerprint FROM clustering_muc_room_lifecycles WHERE lifecycle_id = ?",
                crate::db_params![active.coordinates.lifecycle.to_string()],
            )
            .await
            .expect("query reactivated lifecycle fingerprint");
        let reactivated_row = reactivated_rows
            .next()
            .await
            .expect("reactivated lifecycle row")
            .expect("reactivated lifecycle row exists");
        assert_eq!(
            reactivated_row
                .get::<Option<String>>(0)
                .expect("reactivated fingerprint"),
            Some(activate_fingerprint.clone()),
            "idempotent activation should retain exact-intent proof for future ambiguous reconciliation"
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
            .commit_room_mutation(
                &adoptable,
                &adoptable_fence,
                RoomDurableMutation::Activate,
                waddle_xmpp::muc::RoomMutationEffects::none(),
            )
            .await
            .expect("activate adopts a pre-lifecycle room row");
        assert_eq!(adopted.coordinates.revision.as_i64(), 1);

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
            adopted.coordinates.lifecycle.to_string()
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
            adopted.coordinates.lifecycle.to_string()
        );
        assert_eq!(adoption_room_row.get::<i64>(1).expect("room revision"), 1);

        let legacy_null = unique_room_jid("lane-c3-legacy-null-fingerprint-activate");
        let legacy_null_entity = Entity::new(EntityType::RoomActor, legacy_null.to_string());
        let legacy_null_epoch = claim_store
            .ensure_claimed(&legacy_null_entity, &me)
            .await
            .expect("legacy active room claim");
        let legacy_null_fence =
            RoomClaimFenceContext::new(legacy_null_entity, me.clone(), legacy_null_epoch);
        store.record_claim_fence(&legacy_null, legacy_null_fence.clone());
        let legacy_lifecycle = RoomLifecycleId::generate();
        let legacy_revision = RoomRevision::initial();
        db.guard()
            .await
            .expect("guard")
            .execute(
                "INSERT INTO clustering_muc_room_lifecycles \
                 (lifecycle_id, room_jid, revision, state, mutation_fingerprint) \
                 VALUES (?, ?, ?, ?, NULL)",
                crate::db_params![
                    legacy_lifecycle.to_string(),
                    legacy_null.to_string(),
                    legacy_revision.as_i64(),
                    RoomLifecycleState::Active.as_db_str(),
                ],
            )
            .await
            .expect("persist legacy active lifecycle");
        db.guard()
            .await
            .expect("guard")
            .execute(
                "INSERT INTO clustering_muc_rooms \
                 (room_jid, waddle_id, channel_id, config_json, lifecycle_id, revision) \
                 VALUES (?, ?, ?, ?, ?, ?)",
                crate::db_params![
                    legacy_null.to_string(),
                    "legacy-waddle",
                    "legacy-channel",
                    "{}",
                    legacy_lifecycle.to_string(),
                    legacy_revision.as_i64(),
                ],
            )
            .await
            .expect("persist legacy active room row");

        let legacy_activated = store
            .commit_room_mutation(
                &legacy_null,
                &legacy_null_fence,
                RoomDurableMutation::Activate,
                waddle_xmpp::muc::RoomMutationEffects::none(),
            )
            .await
            .expect("idempotent activate backfills fingerprint");
        assert_eq!(
            legacy_activated.coordinates,
            RoomCommittedCoordinates {
                lifecycle: legacy_lifecycle,
                revision: legacy_revision,
            },
            "legacy idempotent activate must not bump coordinates"
        );
        let mut legacy_rows = db
            .guard()
            .await
            .expect("guard")
            .query(
                "SELECT mutation_fingerprint FROM clustering_muc_room_lifecycles WHERE lifecycle_id = ?",
                crate::db_params![legacy_lifecycle.to_string()],
            )
            .await
            .expect("query legacy activate fingerprint");
        let legacy_row = legacy_rows
            .next()
            .await
            .expect("legacy activate fingerprint row")
            .expect("legacy activate fingerprint row exists");
        assert_eq!(
            legacy_row
                .get::<Option<String>>(0)
                .expect("legacy activate fingerprint"),
            Some(
                mutation_fingerprint(&legacy_null_fence, &RoomDurableMutation::Activate)
                    .expect("legacy activate fingerprint under its own fence")
            ),
            "legacy active lifecycle must gain exact-intent proof before an ambiguous activate acknowledgement"
        );
        assert_eq!(
            store
                .reconcile_ambiguous_commit(
                    &legacy_null,
                    &legacy_null_fence,
                    &RoomDurableMutation::Activate,
                    RoomCommittedCoordinates {
                        lifecycle: legacy_lifecycle,
                        revision: legacy_revision,
                    },
                )
                .await
                .expect("reconcile legacy idempotent activate"),
            CommitReconciliation::Committed,
            "backfilled fingerprint must let ambiguous idempotent activate reconcile as committed"
        );

        // Destroy and Activate both take the exclusive claim lock. A legacy
        // row must therefore be destroyed into a tombstone, not rejected as
        // StateMissing and left available for a later activation to adopt.
        let legacy_destroy = unique_room_jid("lane-c4-legacy-destroy");
        let legacy_destroy_entity = Entity::new(EntityType::RoomActor, legacy_destroy.to_string());
        let legacy_destroy_epoch = claim_store
            .ensure_claimed(&legacy_destroy_entity, &me)
            .await
            .expect("legacy destroy room claim");
        let legacy_destroy_fence =
            RoomClaimFenceContext::new(legacy_destroy_entity, me.clone(), legacy_destroy_epoch);
        store.record_claim_fence(&legacy_destroy, legacy_destroy_fence.clone());
        db.guard()
            .await
            .expect("guard")
            .execute(
                "INSERT INTO clustering_muc_rooms (room_jid, waddle_id, channel_id, config_json) VALUES (?, ?, ?, ?)",
                crate::db_params![legacy_destroy.to_string(), "legacy", "legacy", "{}"],
            )
            .await
            .expect("seed destroyable legacy room row");
        let destroyed_legacy = store
            .commit_room_mutation(
                &legacy_destroy,
                &legacy_destroy_fence,
                RoomDurableMutation::Destroy {
                    completion_attempt: None,
                },
                waddle_xmpp::muc::RoomMutationEffects::none(),
            )
            .await
            .expect("destroy adopts and removes legacy state atomically");
        assert!(matches!(
            store
                .commit_room_mutation(
                    &legacy_destroy,
                    &legacy_destroy_fence,
                    RoomDurableMutation::Activate,
                    waddle_xmpp::muc::RoomMutationEffects::none()
                )
                .await,
            Err(RoomCommitError::StateMissing)
        ));
        let mut destroyed_legacy_rows = db
            .guard()
            .await
            .expect("guard")
            .query(
                "SELECT state, revision FROM clustering_muc_room_lifecycles WHERE lifecycle_id = ?",
                crate::db_params![destroyed_legacy.coordinates.lifecycle.to_string()],
            )
            .await
            .expect("query legacy tombstone");
        let destroyed_legacy_row = destroyed_legacy_rows
            .next()
            .await
            .expect("read legacy tombstone")
            .expect("legacy tombstone exists");
        assert_eq!(
            destroyed_legacy_row.get::<String>(0).expect("state"),
            "tombstoned"
        );
        assert_eq!(destroyed_legacy_row.get::<i64>(1).expect("revision"), 2);

        // Exercise the actual race: legacy adoption and terminal cleanup
        // contend on the exclusive claim lock. Destroy must leave no row
        // that a concurrently queued activation can resurrect.
        let raced_legacy = unique_room_jid("lane-c4-activate-destroy-race");
        let raced_entity = Entity::new(EntityType::RoomActor, raced_legacy.to_string());
        let raced_epoch = claim_store
            .ensure_claimed(&raced_entity, &me)
            .await
            .expect("raced legacy room claim");
        let raced_fence = RoomClaimFenceContext::new(raced_entity, me.clone(), raced_epoch);
        store.record_claim_fence(&raced_legacy, raced_fence.clone());
        db.guard()
            .await
            .expect("guard")
            .execute(
                "INSERT INTO clustering_muc_rooms (room_jid, waddle_id, channel_id, config_json) VALUES (?, ?, ?, ?)",
                crate::db_params![raced_legacy.to_string(), "legacy", "legacy", "{}"],
            )
            .await
            .expect("seed raced legacy room row");
        let activate_store = store.clone();
        let activate_jid = raced_legacy.clone();
        let activate_fence = raced_fence.clone();
        let activate = tokio::spawn(async move {
            activate_store
                .commit_room_mutation(
                    &activate_jid,
                    &activate_fence,
                    RoomDurableMutation::Activate,
                    waddle_xmpp::muc::RoomMutationEffects::none(),
                )
                .await
        });
        let destroy_store = store.clone();
        let destroy_jid = raced_legacy.clone();
        let destroy_fence = raced_fence.clone();
        let destroy = tokio::spawn(async move {
            destroy_store
                .commit_room_mutation(
                    &destroy_jid,
                    &destroy_fence,
                    RoomDurableMutation::Destroy {
                        completion_attempt: None,
                    },
                    waddle_xmpp::muc::RoomMutationEffects::none(),
                )
                .await
        });
        let (_activate, destroyed) = tokio::join!(activate, destroy);
        destroyed
            .expect("destroy task")
            .expect("destroy completes terminally after legacy activation race");
        let mut raced_rows = db
            .guard()
            .await
            .expect("guard")
            .query(
                "SELECT count(*) FROM clustering_muc_rooms WHERE room_jid = ?",
                crate::db_params![raced_legacy.to_string()],
            )
            .await
            .expect("query raced room rows");
        let raced_room_count: i64 = raced_rows
            .next()
            .await
            .expect("read raced room count")
            .expect("raced room count")
            .get(0)
            .expect("raced room count value");
        assert_eq!(
            raced_room_count, 0,
            "destroy leaves no legacy row to revive"
        );

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
                    .commit_room_mutation(
                        &missing,
                        &missing_fence,
                        RoomDurableMutation::Activate,
                        waddle_xmpp::muc::RoomMutationEffects::none()
                    )
                    .await,
                Err(RoomCommitError::StateMissing)
            ),
            "activating a missing lifecycle must remain StateMissing"
        );
    }

    #[tokio::test]
    async fn coordinate_less_destroy_reconciliation_does_not_accept_a_foreign_claim_takeover() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, db, me)) = clean_store().await else {
            return;
        };
        let room_jid = unique_room_jid("pending-destroy-foreign-takeover");
        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        let epoch = claim_store
            .ensure_claimed(&entity, &me)
            .await
            .expect("claim room");
        let fence = RoomClaimFenceContext::new(entity.clone(), me, epoch);

        // Model the lost-ack window: the old claim is absent, but a foreign
        // owner now holds the room. This must not be accepted as evidence
        // that the old coordinate-less release committed.
        db.guard()
            .await
            .expect("guard")
            .execute(
                "UPDATE clustering_claims \
                 SET node_id = ?, node_epoch = ?, claim_epoch = claim_epoch + 1 \
                 WHERE entity = ?",
                crate::db_params![
                    NodeId::generate().to_string(),
                    NodeId::generate().to_string(),
                    room_entity_key(&room_jid),
                ],
            )
            .await
            .expect("replace claim with foreign owner");

        assert_eq!(
            store
                .reconcile_ambiguous_commit(
                    &room_jid,
                    &fence,
                    &RoomDurableMutation::DestroyAndReleaseClaim {
                        completion_attempt: None,
                    },
                    RoomCommittedCoordinates {
                        lifecycle: RoomLifecycleId::generate(),
                        revision: RoomRevision::initial(),
                    },
                )
                .await
                .expect("reconcile foreign takeover"),
            CommitReconciliation::Unknown,
            "an absent original fence has no durable proof that coordinate-less cleanup committed"
        );
    }

    #[tokio::test]
    async fn coordinate_less_destroy_reconciliation_accepts_a_matching_completion_attempt_proof() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, db, me)) = clean_store().await else {
            return;
        };
        let room_jid = unique_room_jid("pending-destroy-armed-proof");
        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        let epoch = claim_store
            .ensure_claimed(&entity, &me)
            .await
            .expect("claim room");
        let fence = RoomClaimFenceContext::new(entity, me, epoch);
        store.record_claim_fence(&room_jid, fence.clone());
        let attempt = DestroyAttemptId::generate();

        db.guard()
            .await
            .expect("guard")
            .execute(
                "INSERT INTO clustering_muc_destroy_outbox \
                 (attempt_id, payload_json, available_at_ms, lease_token, leased_at_ms) \
                 VALUES (?, '{}', ?, NULL, NULL)",
                crate::db_params![attempt.as_uuid().to_string(), i64::MAX],
            )
            .await
            .expect("persist inert completion reservation");

        let coordinates = store
            .commit_room_mutation(
                &room_jid,
                &fence,
                RoomDurableMutation::DestroyAndReleaseClaim {
                    completion_attempt: Some(attempt),
                },
                waddle_xmpp::muc::RoomMutationEffects::none(),
            )
            .await
            .expect("pending destroy release commits");
        let mut outbox_rows = db
            .guard()
            .await
            .expect("guard")
            .query(
                "SELECT available_at_ms, lifecycle_id FROM clustering_muc_destroy_outbox WHERE attempt_id = ?",
                crate::db_params![attempt.as_uuid().to_string()],
            )
            .await
            .expect("query fenced coordinate-less completion");
        let outbox_row = outbox_rows
            .next()
            .await
            .expect("read fenced coordinate-less completion")
            .expect("fenced coordinate-less completion row");
        let lifecycle_id: String = outbox_row.get(1).expect("decode fenced lifecycle id");
        assert_eq!(
            lifecycle_id,
            coordinates.coordinates.lifecycle.to_string(),
            "coordinate-less destroy must persist the tombstoned lifecycle fence into its outbox row"
        );
        drop(outbox_rows);
        let mut lifecycle_rows = db
            .guard()
            .await
            .expect("guard")
            .query(
                "SELECT state FROM clustering_muc_room_lifecycles WHERE lifecycle_id = ?",
                crate::db_params![coordinates.coordinates.lifecycle.to_string()],
            )
            .await
            .expect("query coordinate-less destroy lifecycle");
        let lifecycle_row = lifecycle_rows
            .next()
            .await
            .expect("read coordinate-less destroy lifecycle")
            .expect("coordinate-less destroy lifecycle row");
        let lifecycle_state: String = lifecycle_row.get(0).expect("decode lifecycle state");
        assert_eq!(
            lifecycle_state,
            RoomLifecycleState::Tombstoned.as_db_str(),
            "coordinate-less destroy must leave a tombstoned lifecycle proof behind"
        );
        drop(lifecycle_rows);

        assert_eq!(
            store
                .reconcile_ambiguous_commit(
                    &room_jid,
                    &fence,
                    &RoomDurableMutation::DestroyAndReleaseClaim {
                        completion_attempt: Some(attempt),
                    },
                    coordinates.coordinates,
                )
                .await
                .expect("reconcile exact coordinate-less destroy attempt"),
            CommitReconciliation::Committed,
            "an armed matching destroy completion proves the exact coordinate-less cleanup committed"
        );
    }

    #[tokio::test]
    async fn reconciliation_rejects_a_different_mutation_at_matching_coordinates() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, db, me)) = clean_store().await else {
            return;
        };
        let room_jid = unique_room_jid("ambiguous-different-intent");
        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        let epoch = claim_store
            .ensure_claimed(&entity, &me)
            .await
            .expect("claim room");
        let fence = RoomClaimFenceContext::new(entity, me, epoch);
        store.record_claim_fence(&room_jid, fence.clone());
        let stale_intent = RoomDurableMutation::Create {
            waddle_id: waddle_id("waddle-stale"),
            channel_id: channel_id("channel-stale"),
            config: RoomConfig::default(),
            initial_affiliations: vec![],
        };
        let foreign_intent = RoomDurableMutation::Create {
            waddle_id: waddle_id("waddle-foreign"),
            channel_id: channel_id("channel-foreign"),
            config: RoomConfig {
                members_only: true,
                ..RoomConfig::default()
            },
            initial_affiliations: vec![],
        };
        let lifecycle = RoomLifecycleId::generate();
        let revision = RoomRevision::initial();
        let foreign_fingerprint =
            mutation_fingerprint(&fence, &foreign_intent).expect("fingerprint foreign intent");
        let foreign_config = serde_json::to_string(match &foreign_intent {
            RoomDurableMutation::Create { config, .. } => config,
            _ => unreachable!("foreign intent is a create"),
        })
        .expect("serialize foreign config");
        db.guard()
            .await
            .expect("guard")
            .execute(
                "INSERT INTO clustering_muc_room_lifecycles \
                 (lifecycle_id, room_jid, revision, state, mutation_fingerprint) \
                 VALUES (?, ?, ?, ?, ?)",
                crate::db_params![
                    lifecycle.to_string(),
                    room_jid.to_string(),
                    revision.as_i64(),
                    RoomLifecycleState::Preparing.as_db_str(),
                    foreign_fingerprint,
                ],
            )
            .await
            .expect("persist foreign lifecycle");
        db.guard()
            .await
            .expect("guard")
            .execute(
                "INSERT INTO clustering_muc_rooms \
                 (room_jid, waddle_id, channel_id, config_json, lifecycle_id, revision) \
                 VALUES (?, ?, ?, ?, ?, ?)",
                crate::db_params![
                    room_jid.to_string(),
                    "waddle-foreign",
                    "channel-foreign",
                    foreign_config,
                    lifecycle.to_string(),
                    revision.as_i64(),
                ],
            )
            .await
            .expect("persist foreign room row");

        assert_eq!(
            store
                .reconcile_ambiguous_commit(
                    &room_jid,
                    &fence,
                    &stale_intent,
                    RoomCommittedCoordinates {
                        lifecycle,
                        revision
                    },
                )
                .await
                .expect("reconcile mismatched intent"),
            CommitReconciliation::NotCommitted,
            "read-back must reject a different committed mutation at the same durable coordinates"
        );
    }

    /// #1647 (codex final round): `Activate` is state-idempotent — when the
    /// lifecycle already sits in `active` at the same coordinates, an
    /// ambiguous acknowledgement reconciles as Committed even though the
    /// stored proof belongs to a predecessor owner's fence (an idempotent
    /// re-activate never overwrites it).
    #[tokio::test]
    async fn ambiguous_idempotent_activate_reconciles_against_a_predecessors_proof() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, db, me)) = clean_store().await else {
            return;
        };
        let room_jid = unique_room_jid("ambiguous-idempotent-activate");
        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        let epoch = claim_store
            .ensure_claimed(&entity, &me)
            .await
            .expect("claim room");
        let fence = RoomClaimFenceContext::new(entity.clone(), me, epoch);
        store.record_claim_fence(&room_jid, fence.clone());
        // The PREDECESSOR owner activated this lifecycle and left its proof.
        let predecessor_fence =
            RoomClaimFenceContext::new(entity, node_identity(), ClaimEpoch(epoch.0 + 1));
        let predecessor_proof =
            mutation_fingerprint(&predecessor_fence, &RoomDurableMutation::Activate)
                .expect("predecessor activate proof");
        let lifecycle = RoomLifecycleId::generate();
        let revision = RoomRevision::initial();
        db.guard()
            .await
            .expect("guard")
            .execute(
                "INSERT INTO clustering_muc_room_lifecycles \
                 (lifecycle_id, room_jid, revision, state, mutation_fingerprint) \
                 VALUES (?, ?, ?, ?, ?)",
                crate::db_params![
                    lifecycle.to_string(),
                    room_jid.to_string(),
                    revision.as_i64(),
                    RoomLifecycleState::Active.as_db_str(),
                    predecessor_proof,
                ],
            )
            .await
            .expect("persist predecessor-activated lifecycle");
        db.guard()
            .await
            .expect("guard")
            .execute(
                "INSERT INTO clustering_muc_rooms \
                 (room_jid, waddle_id, channel_id, config_json, lifecycle_id, revision) \
                 VALUES (?, ?, ?, ?, ?, ?)",
                crate::db_params![
                    room_jid.to_string(),
                    "w",
                    "c",
                    "{}",
                    lifecycle.to_string(),
                    revision.as_i64(),
                ],
            )
            .await
            .expect("persist active room row");

        assert_eq!(
            store
                .reconcile_ambiguous_commit(
                    &room_jid,
                    &fence,
                    &RoomDurableMutation::Activate,
                    RoomCommittedCoordinates {
                        lifecycle,
                        revision
                    },
                )
                .await
                .expect("reconcile idempotent activate"),
            CommitReconciliation::Committed,
            "the already-active target state reconciles as committed under any owner's proof"
        );
    }

    /// #1647 (codex round 22): two owners can commit byte-identical
    /// projections at the same next revision across an ownership transfer.
    /// Reconciliation must never mistake the other owner's row for its own.
    #[tokio::test]
    async fn reconciliation_rejects_the_same_intent_committed_under_a_different_fence() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, db, me)) = clean_store().await else {
            return;
        };
        let room_jid = unique_room_jid("ambiguous-foreign-fence");
        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        let epoch = claim_store
            .ensure_claimed(&entity, &me)
            .await
            .expect("claim room");
        let fence = RoomClaimFenceContext::new(entity.clone(), me, epoch);
        store.record_claim_fence(&room_jid, fence.clone());
        let intent = occupancy_join_projection();
        let foreign_fence =
            RoomClaimFenceContext::new(entity, node_identity(), ClaimEpoch(epoch.0 + 1));
        let foreign_fingerprint = mutation_fingerprint(&foreign_fence, &intent)
            .expect("fingerprint the foreign owner's identical commit");
        let lifecycle = RoomLifecycleId::generate();
        let revision = RoomRevision::initial();
        db.guard()
            .await
            .expect("guard")
            .execute(
                "INSERT INTO clustering_muc_room_lifecycles \
                 (lifecycle_id, room_jid, revision, state, mutation_fingerprint) \
                 VALUES (?, ?, ?, ?, ?)",
                crate::db_params![
                    lifecycle.to_string(),
                    room_jid.to_string(),
                    revision.as_i64(),
                    RoomLifecycleState::Active.as_db_str(),
                    foreign_fingerprint,
                ],
            )
            .await
            .expect("persist the foreign owner's lifecycle row");

        assert_eq!(
            store
                .reconcile_ambiguous_commit(
                    &room_jid,
                    &fence,
                    &intent,
                    RoomCommittedCoordinates {
                        lifecycle,
                        revision
                    },
                )
                .await
                .expect("reconcile against the foreign owner's identical commit"),
            CommitReconciliation::NotCommitted,
            "an identical payload committed under another owner's fence is NOT this owner's commit"
        );
    }

    #[tokio::test]
    async fn destroy_reconciliation_requires_the_exact_completion_attempt_proof() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, db, me)) = clean_store().await else {
            return;
        };
        let room_jid = unique_room_jid("destroy-ambiguous-wrong-attempt");
        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        let epoch = claim_store
            .ensure_claimed(&entity, &me)
            .await
            .expect("claim room");
        let fence = RoomClaimFenceContext::new(entity, me, epoch);
        store.record_claim_fence(&room_jid, fence.clone());

        let created = store
            .commit_room_mutation(
                &room_jid,
                &fence,
                RoomDurableMutation::Create {
                    waddle_id: waddle_id("waddle-ambiguous-destroy"),
                    channel_id: channel_id("channel-ambiguous-destroy"),
                    config: RoomConfig::default(),
                    initial_affiliations: vec![],
                },
                waddle_xmpp::muc::RoomMutationEffects::none(),
            )
            .await
            .expect("seed room");
        let attempt = DestroyAttemptId::generate();
        db.guard()
            .await
            .expect("guard")
            .execute(
                "INSERT INTO clustering_muc_destroy_outbox \
                 (attempt_id, payload_json, available_at_ms, lease_token, leased_at_ms) \
                 VALUES (?, '{}', ?, NULL, NULL)",
                crate::db_params![attempt.as_uuid().to_string(), i64::MAX],
            )
            .await
            .expect("persist inert completion reservation");
        store
            .commit_room_mutation(
                &room_jid,
                &fence,
                RoomDurableMutation::Destroy {
                    completion_attempt: Some(attempt),
                },
                waddle_xmpp::muc::RoomMutationEffects::none(),
            )
            .await
            .expect("destroy commits");

        db.guard()
            .await
            .expect("guard")
            .execute(
                "DELETE FROM clustering_muc_destroy_outbox WHERE attempt_id = ?",
                crate::db_params![attempt.as_uuid().to_string()],
            )
            .await
            .expect("remove exact completion proof");
        let foreign_attempt = DestroyAttemptId::generate();
        db.guard()
            .await
            .expect("guard")
            .execute(
                "INSERT INTO clustering_muc_destroy_outbox \
                 (attempt_id, payload_json, lifecycle_id, available_at_ms, lease_token, leased_at_ms) \
                 VALUES (?, '{}', ?, ?, NULL, NULL)",
                crate::db_params![
                    foreign_attempt.as_uuid().to_string(),
                    created.coordinates.lifecycle.to_string(),
                    crate::time::now_ms(),
                ],
            )
            .await
            .expect("persist foreign armed completion");

        assert_eq!(
            store
                .reconcile_ambiguous_commit(
                    &room_jid,
                    &fence,
                    &RoomDurableMutation::Destroy {
                        completion_attempt: Some(attempt),
                    },
                    RoomCommittedCoordinates {
                        lifecycle: created.coordinates.lifecycle,
                        revision: RoomRevision::from_stored(2).expect("destroy revision"),
                    },
                )
                .await
                .expect("reconcile destroy without exact attempt proof"),
            CommitReconciliation::Unknown,
            "tombstoned coordinates alone must not prove a different completion attempt committed"
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
            waddle_id: waddle_id("waddle-concurrent"),
            channel_id: channel_id("channel-concurrent"),
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
                .commit_room_mutation(
                    &jid_a,
                    &fence_a,
                    intent_a,
                    waddle_xmpp::muc::RoomMutationEffects::none(),
                )
                .await
                .expect("first create")
        });
        let second = tokio::spawn(async move {
            barrier_b.wait().await;
            store_b
                .commit_room_mutation(
                    &jid_b,
                    &fence_b,
                    intent_b,
                    waddle_xmpp::muc::RoomMutationEffects::none(),
                )
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
                        waddle_id: waddle_id("waddle-mismatch"),
                        channel_id: channel_id("channel-mismatch"),
                        config: RoomConfig::default(),
                        initial_affiliations: vec![],
                    },
                    waddle_xmpp::muc::RoomMutationEffects::none()
                )
                .await,
            Err(RoomCommitError::CreateConflict)
        ));

        store
            .commit_room_mutation(
                &room_jid,
                &fence,
                RoomDurableMutation::Dormancy,
                waddle_xmpp::muc::RoomMutationEffects::none(),
            )
            .await
            .expect("make the lifecycle dormant");
        let dormant_coordinates = store
            .commit_room_mutation(
                &room_jid,
                &fence,
                intent,
                waddle_xmpp::muc::RoomMutationEffects::none(),
            )
            .await
            .expect("matching create is idempotent for a dormant live lifecycle");
        assert_eq!(dormant_coordinates.coordinates.revision.as_i64(), 2);
        assert!(matches!(
            store
                .commit_room_mutation(
                    &room_jid,
                    &fence,
                    RoomDurableMutation::Create {
                        waddle_id: waddle_id("waddle-mismatch"),
                        channel_id: channel_id("channel-mismatch"),
                        config: RoomConfig::default(),
                        initial_affiliations: vec![],
                    },
                    waddle_xmpp::muc::RoomMutationEffects::none()
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
                            waddle_id: waddle_id("waddle-overflow"),
                            channel_id: channel_id("channel-overflow"),
                        },
                        waddle_xmpp::muc::RoomMutationEffects::none()
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
                    waddle_id: waddle_id("waddle-lock"),
                    channel_id: channel_id("channel-lock"),
                    config: RoomConfig::default(),
                    initial_affiliations: vec![],
                },
                waddle_xmpp::muc::RoomMutationEffects::none(),
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
                            waddle_id: waddle_id("waddle-lock"),
                            channel_id: channel_id("channel-lock"),
                        },
                        waddle_xmpp::muc::RoomMutationEffects::none(),
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
            "SELECT lifecycle_id, revision, state, mutation_fingerprint",
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
                    .commit_room_mutation(
                        &room_jid,
                        &fence,
                        RoomDurableMutation::Dormancy,
                        waddle_xmpp::muc::RoomMutationEffects::none(),
                    )
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
                    .commit_room_mutation(
                        &room_jid,
                        &fence,
                        RoomDurableMutation::Destroy {
                            completion_attempt: None,
                        },
                        waddle_xmpp::muc::RoomMutationEffects::none(),
                    )
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
                    waddle_id: waddle_id("waddle-serialized"),
                    channel_id: channel_id("channel-serialized"),
                    config: RoomConfig::default(),
                    initial_affiliations: vec![],
                },
                waddle_xmpp::muc::RoomMutationEffects::none(),
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
                                waddle_id: waddle_id("waddle-serialized"),
                                channel_id: channel_id("channel-serialized"),
                            },
                            waddle_xmpp::muc::RoomMutationEffects::none(),
                        )
                        .await
                        .expect("first concurrent mutation")
                        .coordinates
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
                                waddle_id: waddle_id("waddle-serialized"),
                                channel_id: channel_id("channel-serialized"),
                            },
                            waddle_xmpp::muc::RoomMutationEffects::none(),
                        )
                        .await
                        .expect("second concurrent mutation")
                        .coordinates
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
                            waddle_id: waddle_id("waddle-lost"),
                            channel_id: channel_id("channel-lost"),
                            config: RoomConfig::default(),
                            initial_affiliations: vec![],
                        },
                        waddle_xmpp::muc::RoomMutationEffects::none()
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
                    waddle_id: waddle_id("waddle-mono"),
                    channel_id: channel_id("channel-mono"),
                    config: RoomConfig {
                        name: "first".to_string(),
                        ..RoomConfig::default()
                    },
                    initial_affiliations: vec![],
                },
                waddle_xmpp::muc::RoomMutationEffects::none(),
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
                    waddle_id: waddle_id("waddle-mono"),
                    channel_id: channel_id("channel-mono"),
                },
                waddle_xmpp::muc::RoomMutationEffects::none(),
            )
            .await
            .expect("config revision bump");
        assert_eq!(config.coordinates.revision.as_i64(), 2);

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
                waddle_xmpp::muc::RoomMutationEffects::none(),
            )
            .await
            .expect("subject revision bump");
        assert_eq!(subject.coordinates.revision.as_i64(), 3);

        let affiliation = store
            .commit_room_mutation(
                &room_jid,
                &fence,
                RoomDurableMutation::Affiliation(waddle_xmpp::muc::durable::AffiliationEntry::new(
                    "alice@example.com".parse().expect("valid jid"),
                    Some(Affiliation::Owner),
                )),
                waddle_xmpp::muc::RoomMutationEffects::none(),
            )
            .await
            .expect("affiliation revision bump");
        assert_eq!(affiliation.coordinates.revision.as_i64(), 4);

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
        assert_eq!(lifecycle_id, affiliation.coordinates.lifecycle.to_string());
    }

    #[tokio::test]
    async fn projection_commit_bumps_head_without_advancing_config_coordinates() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, _db, me)) = clean_store().await else {
            return;
        };
        let room_jid = unique_room_jid("lane-1647-projection");
        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        let epoch = claim_store
            .ensure_claimed(&entity, &me)
            .await
            .expect("claim");
        let fence = RoomClaimFenceContext::new(entity, me, epoch);
        store.record_claim_fence(&room_jid, fence.clone());

        let create = store
            .commit_room_mutation(
                &room_jid,
                &fence,
                RoomDurableMutation::Create {
                    waddle_id: waddle_id("waddle-projection"),
                    channel_id: channel_id("channel-projection"),
                    config: RoomConfig::default(),
                    initial_affiliations: vec![],
                },
                RoomMutationEffects::none(),
            )
            .await
            .expect("create room");
        let projection = store
            .commit_room_mutation(
                &room_jid,
                &fence,
                occupancy_join_projection(),
                RoomMutationEffects::none(),
            )
            .await
            .expect("commit projection");

        assert_eq!(projection.coordinates.revision.as_i64(), 2);
        let state = store
            .load_room_state_fenced(&room_jid, &fence)
            .await
            .expect("load state")
            .expect("stored room");
        assert_eq!(state.coordinates, Some(projection.coordinates));
        assert_eq!(state.config_coordinates, Some(create.coordinates));
    }

    #[tokio::test]
    async fn projection_commit_on_dormant_lifecycle_is_state_missing() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, _db, me)) = clean_store().await else {
            return;
        };
        let room_jid = unique_room_jid("lane-1647-projection-dormant");
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
                    waddle_id: waddle_id("waddle-projection-dormant"),
                    channel_id: channel_id("channel-projection-dormant"),
                    config: RoomConfig::default(),
                    initial_affiliations: vec![],
                },
                RoomMutationEffects::none(),
            )
            .await
            .expect("create room");
        store
            .commit_room_mutation(
                &room_jid,
                &fence,
                RoomDurableMutation::Dormancy,
                RoomMutationEffects::none(),
            )
            .await
            .expect("make room dormant");

        assert!(matches!(
            store
                .commit_room_mutation(
                    &room_jid,
                    &fence,
                    occupancy_join_projection(),
                    RoomMutationEffects::none(),
                )
                .await,
            Err(RoomCommitError::StateMissing)
        ));
    }

    #[tokio::test]
    async fn projection_commit_is_rejected_when_claim_is_lost() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, _db, me)) = clean_store().await else {
            return;
        };
        let room_jid = unique_room_jid("lane-1647-projection-lost-claim");
        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        let epoch = claim_store
            .ensure_claimed(&entity, &me)
            .await
            .expect("claim");
        let fence = RoomClaimFenceContext::new(entity.clone(), me.clone(), epoch);
        store.record_claim_fence(&room_jid, fence.clone());
        claim_store
            .release(&entity, &me, epoch)
            .await
            .expect("release claim");

        assert!(matches!(
            store
                .commit_room_mutation(
                    &room_jid,
                    &fence,
                    occupancy_join_projection(),
                    RoomMutationEffects::none(),
                )
                .await,
            Err(RoomCommitError::NotOwner)
        ));
    }

    #[tokio::test]
    async fn members_only_enforcement_updates_config_coordinates() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, _db, me)) = clean_store().await else {
            return;
        };
        let room_jid = unique_room_jid("lane-1647-members-only-coordinates");
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
                    waddle_id: waddle_id("waddle-members-only"),
                    channel_id: channel_id("channel-members-only"),
                    config: RoomConfig::default(),
                    initial_affiliations: vec![],
                },
                RoomMutationEffects::none(),
            )
            .await
            .expect("create room");
        let config = RoomConfig {
            members_only: true,
            ..Default::default()
        };
        let enforcement = store
            .commit_room_mutation(
                &room_jid,
                &fence,
                RoomDurableMutation::MembersOnlyEnforcement {
                    config,
                    affiliations: vec![],
                },
                RoomMutationEffects::none(),
            )
            .await
            .expect("enforce members-only");

        let state = store
            .load_room_state_fenced(&room_jid, &fence)
            .await
            .expect("load state")
            .expect("stored room");
        assert_eq!(state.config_coordinates, Some(enforcement.coordinates));
    }

    #[tokio::test]
    async fn projection_commit_follows_claim_then_lifecycle_lock_order_and_bumps_revision() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(room) = projection_test_room("lane-1647-projection-lock-order").await else {
            return;
        };
        let ProjectionTestRoom {
            store,
            db,
            room_jid,
            fence,
            create,
            ..
        } = room;
        let store = Arc::new(store);
        let mut claim_blocker = db.begin().await.expect("begin claim blocker");
        let mut claim_rows = claim_blocker
            .query(
                "SELECT 1 FROM clustering_claims WHERE entity = ? FOR UPDATE",
                crate::db_params![room_entity_key(&room_jid)],
            )
            .await
            .expect("lock claim");
        assert!(claim_rows.next().await.expect("claim row").is_some());
        drop(claim_rows);
        let mut lifecycle_blocker = db.begin().await.expect("begin lifecycle blocker");
        let mut lifecycle_rows = lifecycle_blocker
            .query(
                "SELECT 1 FROM clustering_muc_room_lifecycles WHERE room_jid = ? FOR UPDATE",
                crate::db_params![room_jid.to_string()],
            )
            .await
            .expect("lock lifecycle");
        assert!(lifecycle_rows
            .next()
            .await
            .expect("lifecycle row")
            .is_some());
        drop(lifecycle_rows);

        let projection = tokio::spawn({
            let store = Arc::clone(&store);
            let room_jid = room_jid.clone();
            let fence = fence.clone();
            async move {
                store
                    .commit_room_mutation(
                        &room_jid,
                        &fence,
                        occupancy_join_projection(),
                        RoomMutationEffects::none(),
                    )
                    .await
            }
        });
        wait_for_lock_waiter(&db, "FOR SHARE").await;
        assert!(
            !projection.is_finished(),
            "projection must acquire its claim share lock before touching the lifecycle"
        );
        claim_blocker.commit().await.expect("release claim blocker");
        wait_for_lock_waiter(
            &db,
            "SELECT lifecycle_id, revision, state, mutation_fingerprint",
        )
        .await;
        assert!(
            !projection.is_finished(),
            "projection must block on the lifecycle only after its claim share lock succeeds"
        );
        lifecycle_blocker
            .commit()
            .await
            .expect("release lifecycle blocker");
        let projection = projection
            .await
            .expect("projection task")
            .expect("projection commit");
        assert_eq!(
            projection.coordinates.revision,
            create.coordinates.revision.next().expect("second revision")
        );
    }

    #[tokio::test]
    async fn projection_commit_on_tombstoned_lifecycle_is_rejected() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(room) = projection_test_room("lane-1647-projection-tombstone").await else {
            return;
        };
        let destroy = room
            .store
            .commit_room_mutation(
                &room.room_jid,
                &room.fence,
                RoomDurableMutation::Destroy {
                    completion_attempt: None,
                },
                RoomMutationEffects::none(),
            )
            .await
            .expect("destroy room");
        assert!(matches!(
            room.store
                .commit_room_mutation(
                    &room.room_jid,
                    &room.fence,
                    occupancy_join_projection(),
                    RoomMutationEffects::none(),
                )
                .await,
            Err(RoomCommitError::StateMissing)
        ));
        let conn = room.db.guard().await.expect("guard");
        let mut rows = conn
            .query(
                "SELECT revision FROM clustering_muc_room_lifecycles WHERE lifecycle_id = ?",
                crate::db_params![destroy.coordinates.lifecycle.to_string()],
            )
            .await
            .expect("query tombstone revision");
        let revision: i64 = rows
            .next()
            .await
            .expect("read tombstone revision")
            .expect("tombstone row")
            .get(0)
            .expect("decode tombstone revision");
        assert_eq!(revision, destroy.coordinates.revision.as_i64());
    }

    #[tokio::test]
    async fn projection_fingerprint_reconciles_lost_ack() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(room) = projection_test_room("lane-1647-projection-lost-ack").await else {
            return;
        };
        let intent = occupancy_join_projection();
        let committed = room
            .store
            .commit_room_mutation(
                &room.room_jid,
                &room.fence,
                intent.clone(),
                RoomMutationEffects::none(),
            )
            .await
            .expect("commit projection before simulated acknowledgement loss");
        assert_eq!(
            room.store
                .reconcile_ambiguous_commit(
                    &room.room_jid,
                    &room.fence,
                    &intent,
                    committed.coordinates,
                )
                .await
                .expect("reconcile lost projection acknowledgement"),
            CommitReconciliation::Committed
        );
    }

    #[tokio::test]
    async fn projection_bump_does_not_strand_pending_config_effect_rows() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(room) = projection_test_room("lane-1647-projection-outbox").await else {
            return;
        };
        let config = room
            .store
            .commit_room_mutation(
                &room.room_jid,
                &room.fence,
                projection_config_intent("with pending effect"),
                projection_config_effects(room.room_jid.clone()),
            )
            .await
            .expect("commit config with effects");
        let reservation = config.reservation.clone().expect("config reservation");
        let projection = room
            .store
            .commit_room_mutation(
                &room.room_jid,
                &room.fence,
                occupancy_join_projection(),
                RoomMutationEffects::none(),
            )
            .await
            .expect("projection after config");
        let state = room
            .store
            .load_room_state_fenced(&room.room_jid, &room.fence)
            .await
            .expect("load room")
            .expect("stored room");
        assert_eq!(state.coordinates, Some(projection.coordinates));
        assert_eq!(state.config_coordinates, Some(config.coordinates));
        assert_eq!(
            room.store
                .room_effect_outbox
                .staged_reservation_for(config.coordinates.lifecycle, config.coordinates.revision)
                .await
                .expect("find staged config reservation"),
            Some(reservation.clone())
        );
        room.store
            .room_effect_outbox
            .arm_reservation(&reservation, crate::time::now_ms())
            .await
            .expect("arm config reservation");
        assert!(
            room.store
                .room_effect_outbox
                .claim_due_head(crate::time::now_ms(), 8)
                .await
                .expect("claim due config effect")
                .into_iter()
                .any(
                    |claim| claim.row.key.lifecycle == config.coordinates.lifecycle
                        && claim.row.key.revision == config.coordinates.revision
                ),
            "a projection head bump must not make an earlier config effect unclaimable"
        );
    }

    #[tokio::test]
    async fn config_coordinates_persist_and_restore_across_retirement() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(room) = projection_test_room("lane-1647-config-coordinates").await else {
            return;
        };
        let config = room
            .store
            .commit_room_mutation(
                &room.room_jid,
                &room.fence,
                projection_config_intent("durable config coordinates"),
                RoomMutationEffects::none(),
            )
            .await
            .expect("commit config");
        let projection = room
            .store
            .commit_room_mutation(
                &room.room_jid,
                &room.fence,
                occupancy_join_projection(),
                RoomMutationEffects::none(),
            )
            .await
            .expect("commit projection");
        let restored = room
            .store
            .load_room_state_fenced(&room.room_jid, &room.fence)
            .await
            .expect("restore durable room")
            .expect("durable room exists");
        assert_eq!(restored.config_coordinates, Some(config.coordinates));
        assert_eq!(restored.coordinates, Some(projection.coordinates));
    }

    #[tokio::test]
    async fn create_seeds_config_coordinates() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(room) = projection_test_room("lane-1647-create-config-coordinates").await else {
            return;
        };
        let state = room
            .store
            .load_room_state_fenced(&room.room_jid, &room.fence)
            .await
            .expect("load created room")
            .expect("created room");
        assert_eq!(state.coordinates, Some(room.create.coordinates));
        assert_eq!(state.config_coordinates, Some(room.create.coordinates));
    }

    #[tokio::test]
    async fn concurrent_same_room_projections_serialize_into_distinct_revisions() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(room) = projection_test_room("lane-1647-projection-fan-in").await else {
            return;
        };
        let store = Arc::new(room.store);
        let barrier = Arc::new(tokio::sync::Barrier::new(16));
        let mut tasks = Vec::new();
        for _ in 0..16 {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            let room_jid = room.room_jid.clone();
            let fence = room.fence.clone();
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                tokio::time::timeout(std::time::Duration::from_secs(30), async move {
                    store
                        .commit_room_mutation(
                            &room_jid,
                            &fence,
                            occupancy_join_projection(),
                            RoomMutationEffects::none(),
                        )
                        .await
                })
                .await
            }));
        }
        let mut revisions = Vec::new();
        for task in tasks {
            let committed = task
                .await
                .expect("projection task")
                .expect("projection must not time out")
                .expect("projection commit");
            revisions.push(committed.coordinates.revision.as_i64());
        }
        revisions.sort_unstable();
        assert_eq!(revisions, (2..=17).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn projection_commit_racing_destroy_either_orders_cleanly() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(room) = projection_test_room("lane-1647-projection-destroy-race").await else {
            return;
        };
        let store = Arc::new(room.store);
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let projection = tokio::spawn({
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            let room_jid = room.room_jid.clone();
            let fence = room.fence.clone();
            async move {
                barrier.wait().await;
                tokio::time::timeout(std::time::Duration::from_secs(30), async move {
                    store
                        .commit_room_mutation(
                            &room_jid,
                            &fence,
                            occupancy_join_projection(),
                            RoomMutationEffects::none(),
                        )
                        .await
                })
                .await
            }
        });
        let destroy = tokio::spawn({
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            let room_jid = room.room_jid.clone();
            let fence = room.fence.clone();
            async move {
                barrier.wait().await;
                tokio::time::timeout(std::time::Duration::from_secs(30), async move {
                    store
                        .commit_room_mutation(
                            &room_jid,
                            &fence,
                            RoomDurableMutation::Destroy {
                                completion_attempt: None,
                            },
                            RoomMutationEffects::none(),
                        )
                        .await
                })
                .await
            }
        });
        let projection = projection
            .await
            .expect("projection task")
            .expect("projection must not time out");
        let destroy = destroy
            .await
            .expect("destroy task")
            .expect("destroy must not time out")
            .expect("destroy commit");
        match projection {
            Ok(projection) => assert!(
                projection.coordinates.revision < destroy.coordinates.revision,
                "a successful projection must precede the destroy tombstone"
            ),
            Err(RoomCommitError::StateMissing) => {}
            Err(error) => panic!("projection race returned unexpected error: {error:?}"),
        }
    }

    #[tokio::test]
    async fn recreated_lifecycle_rejects_projection_under_old_lifecycle_coordinates() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(room) = projection_test_room("lane-1647-old-projection-coordinates").await else {
            return;
        };
        let old_projection = room
            .store
            .commit_room_mutation(
                &room.room_jid,
                &room.fence,
                occupancy_join_projection(),
                RoomMutationEffects::none(),
            )
            .await
            .expect("commit old lifecycle projection");
        room.store
            .commit_room_mutation(
                &room.room_jid,
                &room.fence,
                RoomDurableMutation::Destroy {
                    completion_attempt: None,
                },
                RoomMutationEffects::none(),
            )
            .await
            .expect("destroy old lifecycle");
        let recreated = room
            .store
            .commit_room_mutation(
                &room.room_jid,
                &room.fence,
                RoomDurableMutation::Create {
                    waddle_id: waddle_id("waddle-projection-test"),
                    channel_id: channel_id("channel-projection-test"),
                    config: RoomConfig::default(),
                    initial_affiliations: vec![],
                },
                RoomMutationEffects::none(),
            )
            .await
            .expect("recreate room");
        assert_ne!(
            recreated.coordinates.lifecycle, old_projection.coordinates.lifecycle,
            "recreation must mint a new lifecycle"
        );
        assert_ne!(
            room.store
                .reconcile_ambiguous_commit(
                    &room.room_jid,
                    &room.fence,
                    &occupancy_join_projection(),
                    old_projection.coordinates,
                )
                .await
                .expect("reconcile old lifecycle projection"),
            CommitReconciliation::Committed,
            "an old lifecycle coordinate never reconciles as committed against the recreated lifecycle (Unknown/NotCommitted both fail closed; the actor-level ForeignCapability check refuses the capability)"
        );
    }

    #[tokio::test]
    async fn destroy_supersedes_pending_effects_after_projection_gaps() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(room) = projection_test_room("lane-1647-projection-destroy-effects").await else {
            return;
        };
        let config = room
            .store
            .commit_room_mutation(
                &room.room_jid,
                &room.fence,
                projection_config_intent("pending before destroy"),
                projection_config_effects(room.room_jid.clone()),
            )
            .await
            .expect("commit config effect");
        for _ in 0..3 {
            room.store
                .commit_room_mutation(
                    &room.room_jid,
                    &room.fence,
                    occupancy_join_projection(),
                    RoomMutationEffects::none(),
                )
                .await
                .expect("commit projection gap");
        }
        let destroy = room
            .store
            .commit_room_mutation(
                &room.room_jid,
                &room.fence,
                RoomDurableMutation::Destroy {
                    completion_attempt: None,
                },
                RoomMutationEffects::none(),
            )
            .await
            .expect("destroy after projection gaps");
        assert_eq!(
            destroy.coordinates.revision.as_i64(),
            config.coordinates.revision.as_i64() + 4
        );
        assert!(
            room.store
                .room_effect_outbox
                .reservation_for_revision(config.coordinates.lifecycle, config.coordinates.revision)
                .await
                .expect("query pending config effect")
                .is_none(),
            "destroy must remove unleased non-terminal effects despite intervening projections"
        );
    }

    #[tokio::test]
    async fn members_only_fused_path_recovers_across_projection_bump() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(room) = projection_test_room("lane-1647-members-only-projection").await else {
            return;
        };
        let config = RoomConfig {
            members_only: true,
            ..Default::default()
        };
        let enforcement = room
            .store
            .commit_room_mutation(
                &room.room_jid,
                &room.fence,
                RoomDurableMutation::MembersOnlyEnforcement {
                    config,
                    affiliations: vec![],
                },
                RoomMutationEffects::members_only_enforcement(
                    room.room_jid.clone(),
                    vec![],
                    vec![],
                    vec![],
                    vec![],
                    vec![MucConfigStatusCode::NonPrivacyConfigurationChange],
                    vec![],
                ),
            )
            .await
            .expect("commit members-only enforcement");
        let projection = room
            .store
            .commit_room_mutation(
                &room.room_jid,
                &room.fence,
                occupancy_join_projection(),
                RoomMutationEffects::none(),
            )
            .await
            .expect("projection after enforcement");
        let state = room
            .store
            .load_room_state_fenced(&room.room_jid, &room.fence)
            .await
            .expect("load room")
            .expect("stored room");
        assert_eq!(state.coordinates, Some(projection.coordinates));
        assert_eq!(state.config_coordinates, Some(enforcement.coordinates));
        assert_eq!(
            room.store
                .room_effect_outbox
                .reservation_for_revision(
                    enforcement.coordinates.lifecycle,
                    state
                        .config_coordinates
                        .expect("config coordinates")
                        .revision,
                )
                .await
                .expect("recover members-only reservation"),
            enforcement.reservation,
            "members-only recovery must look up effects by config coordinates, not the head"
        );
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
            ("mutation_fingerprint", "YES"),
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
        for column in [
            "lifecycle_id",
            "revision",
            "config_lifecycle_id",
            "config_revision",
        ] {
            assert_eq!(
                column_nullability(&db, "clustering_muc_rooms", column).await,
                Some("YES".to_string()),
                "room snapshot back-link {column} must remain nullable"
            );
        }
        for constraint in [
            "clustering_muc_rooms_lifecycle_pairing",
            "clustering_muc_rooms_revision_min",
            "clustering_muc_rooms_config_lifecycle_pairing",
            "clustering_muc_rooms_config_revision_min",
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
        for (lifecycle_id, state) in [
            ("second-preparing", "preparing"),
            ("second-active", "active"),
            ("second-dormant", "dormant"),
        ] {
            assert!(conn
                .execute(
                    "INSERT INTO clustering_muc_room_lifecycles (lifecycle_id, room_jid, revision, state) VALUES (?, ?, ?, ?)",
                    crate::db_params![lifecycle_id, "one-live@muc.example.com", 2_i64, state],
                )
                .await
                .is_err(), "a second {state} lifecycle must be rejected");
        }

        // Prove the state CHECK actually admits each live state on its own: the
        // rejections above would also fire from the live-room unique index,
        // so without this insert a vocabulary typo in the constraint would
        // pass the suite while breaking #1646's dormancy snapshot.
        for (lifecycle_id, room_jid, state) in [
            (
                "fresh-preparing",
                "preparing-only@muc.example.com",
                "preparing",
            ),
            ("fresh-dormant", "dormant-only@muc.example.com", "dormant"),
        ] {
            conn.execute(
                "INSERT INTO clustering_muc_room_lifecycles (lifecycle_id, room_jid, revision, state) VALUES (?, ?, ?, ?)",
                crate::db_params![lifecycle_id, room_jid, 1_i64, state],
            )
            .await
            .expect("a live lifecycle on a fresh room must satisfy the state vocabulary");
        }

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
            RoomLifecycleState::Preparing,
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
    async fn durable_commit_holds_identity_guard_while_waiting_for_the_claim_lock() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, db, me)) = clean_store().await else {
            return;
        };
        let room_jid = unique_room_jid("commit-identity-guard");
        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        let epoch = claim_store
            .ensure_claimed(&entity, &me)
            .await
            .expect("claim room");
        let fence = RoomClaimFenceContext::new(entity, me, epoch);
        store.record_claim_fence(&room_jid, fence.clone());
        store
            .commit_room_mutation(
                &room_jid,
                &fence,
                RoomDurableMutation::Create {
                    waddle_id: waddle_id("waddle-identity-guard"),
                    channel_id: channel_id("channel-identity-guard"),
                    config: RoomConfig::default(),
                    initial_affiliations: vec![],
                },
                waddle_xmpp::muc::RoomMutationEffects::none(),
            )
            .await
            .expect("seed room");

        let mut blocker = db.begin().await.expect("begin claim-row blocker");
        let mut rows = blocker
            .query(
                "SELECT 1 FROM clustering_claims WHERE entity = ? FOR UPDATE",
                crate::db_params![room_entity_key(&room_jid)],
            )
            .await
            .expect("lock claim row");
        assert!(rows.next().await.expect("read locked claim row").is_some());
        drop(rows);

        let store = Arc::new(store);
        let commit = tokio::spawn({
            let store = Arc::clone(&store);
            let room_jid = room_jid.clone();
            let fence = fence.clone();
            async move {
                store
                    .commit_room_mutation(
                        &room_jid,
                        &fence,
                        RoomDurableMutation::Config {
                            config: RoomConfig {
                                name: "committed-under-original-incarnation".to_string(),
                                ..RoomConfig::default()
                            },
                            waddle_id: waddle_id("waddle-identity-guard"),
                            channel_id: channel_id("channel-identity-guard"),
                        },
                        waddle_xmpp::muc::RoomMutationEffects::none(),
                    )
                    .await
            }
        });
        wait_for_lock_waiter(&db, "FOR SHARE").await;

        let shared_identity = store.node_identity.clone();
        let rotation = tokio::spawn(async move { shared_identity.rotate(node_identity()).await });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            !rotation.is_finished(),
            "identity rotation must wait until the durable transaction commits"
        );

        blocker.commit().await.expect("release claim-row blocker");
        assert_eq!(
            commit
                .await
                .expect("commit task")
                .expect("commit succeeds before rotation")
                .coordinates
                .revision
                .as_i64(),
            2
        );
        tokio::time::timeout(std::time::Duration::from_secs(1), rotation)
            .await
            .expect("identity rotation completes after commit")
            .expect("identity rotation task");
    }

    #[tokio::test]
    async fn persisted_destroy_completion_blocks_recreation_for_its_room_only() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, _claim_store, db, _me)) = clean_store().await else {
            return;
        };
        let blocked_room = unique_room_jid("destroy-completion-blocked");
        let other_room = unique_room_jid("destroy-completion-other");
        assert!(!store
            .destroy_completion_blocks_recreation(&blocked_room)
            .await
            .expect("empty outbox does not block recreation"));

        let payload = serde_json::json!({ "room_jid": blocked_room.to_string() }).to_string();
        let attempt_id = uuid::Uuid::new_v4().to_string();
        db.guard()
            .await
            .expect("guard")
            .execute(
                "INSERT INTO clustering_muc_destroy_outbox \
                 (attempt_id, payload_json, available_at_ms, lease_token, leased_at_ms) \
                 VALUES (?, ?, ?, NULL, NULL)",
                crate::db_params![attempt_id.clone(), payload, i64::MAX],
            )
            .await
            .expect("persist inert destroy completion");

        assert!(!store
            .destroy_completion_blocks_recreation(&blocked_room)
            .await
            .expect("inert completion reservation does not block recreation"));

        db.guard()
            .await
            .expect("guard")
            .execute(
                "UPDATE clustering_muc_destroy_outbox SET available_at_ms = ? WHERE attempt_id = ?",
                crate::db_params![crate::time::now_ms(), attempt_id],
            )
            .await
            .expect("durably arm destroy completion");

        assert!(store
            .destroy_completion_blocks_recreation(&blocked_room)
            .await
            .expect("matching persisted completion blocks recreation"));
        assert!(!store
            .destroy_completion_blocks_recreation(&other_room)
            .await
            .expect("a different room's completion does not block recreation"));
    }

    #[tokio::test]
    async fn tombstoned_terminal_effect_blocks_recreation_without_destroy_completion_until_completed(
    ) {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, db, me)) = clean_store().await else {
            return;
        };
        let room_jid = unique_room_jid("pending-terminal-recreation");
        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        let epoch = claim_store
            .ensure_claimed(&entity, &me)
            .await
            .expect("claim room");
        let fence = RoomClaimFenceContext::new(entity, me, epoch);
        store.record_claim_fence(&room_jid, fence.clone());

        let lifecycle = RoomLifecycleId::generate();
        let revision = RoomRevision::initial();
        insert_tombstoned_lifecycle(&db, &room_jid, lifecycle, revision).await;
        let reservation = enqueue_terminal_effect(&db, &room_jid, lifecycle, revision).await;
        let outbox = crate::room_effect_outbox::RoomEffectOutboxStore::new(db.clone())
            .await
            .expect("open room effect outbox");
        let row = outbox
            .find(&crate::room_effect_outbox::RoomEffectKey {
                lifecycle,
                revision,
                ordinal: reservation.ordinals[0],
            })
            .await
            .expect("load terminal row")
            .expect("terminal row exists");
        assert!(
            outbox
                .lifecycle_is_executable(&row)
                .await
                .expect("tombstoned lifecycle remains executable"),
            "the exact tombstone must remain readable while a terminal effect is pending"
        );

        let mut destroy_rows = db
            .guard()
            .await
            .expect("guard")
            .query("SELECT COUNT(*) FROM clustering_muc_destroy_outbox", ())
            .await
            .expect("query destroy outbox");
        assert_eq!(
            destroy_rows
                .next()
                .await
                .expect("read destroy outbox count")
                .expect("destroy outbox count row")
                .get::<i64>(0)
                .expect("decode destroy outbox count"),
            0,
            "this fence must come solely from the pending terminal effect row"
        );
        drop(destroy_rows);

        assert!(
            matches!(
                store
                    .commit_room_mutation(
                        &room_jid,
                        &fence,
                        RoomDurableMutation::Create {
                            waddle_id: waddle_id("waddle-terminal-recreation-blocked"),
                            channel_id: channel_id("channel-terminal-recreation-blocked"),
                            config: RoomConfig::default(),
                            initial_affiliations: vec![],
                        },
                        RoomMutationEffects::none()
                    )
                    .await,
                Err(RoomCommitError::RecreationBlocked)
            ),
            "a pending terminal effect must fence recreation even without any destroy-completion row"
        );

        db.guard()
            .await
            .expect("guard")
            .execute(
                "DELETE FROM clustering_muc_room_effects WHERE lifecycle_id = ? AND revision = ?",
                crate::db_params![lifecycle.to_string(), revision.as_i64()],
            )
            .await
            .expect("complete terminal effect");

        store
            .commit_room_mutation(
                &room_jid,
                &fence,
                RoomDurableMutation::Create {
                    waddle_id: waddle_id("waddle-terminal-recreation-unblocked"),
                    channel_id: channel_id("channel-terminal-recreation-unblocked"),
                    config: RoomConfig::default(),
                    initial_affiliations: vec![],
                },
                RoomMutationEffects::none(),
            )
            .await
            .expect("completed terminal effect must unblock recreation");
    }

    #[tokio::test]
    async fn tombstoned_lifecycle_proves_destroy_completion_before_it_blocks_recreation() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, db, me)) = clean_store().await else {
            return;
        };
        let room_jid = unique_room_jid("destroy-completion-tombstone-fence");
        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        let epoch = claim_store
            .ensure_claimed(&entity, &me)
            .await
            .expect("claim room");
        let fence = RoomClaimFenceContext::new(entity, me, epoch);
        store.record_claim_fence(&room_jid, fence.clone());

        let lifecycle = RoomLifecycleId::generate();
        insert_tombstoned_lifecycle(&db, &room_jid, lifecycle, RoomRevision::initial()).await;
        let attempt = DestroyAttemptId::generate();
        let payload = serde_json::json!({ "room_jid": room_jid.to_string() }).to_string();
        db.guard()
            .await
            .expect("guard")
            .execute(
                "INSERT INTO clustering_muc_destroy_outbox \
                 (attempt_id, payload_json, lifecycle_id, available_at_ms, lease_token, leased_at_ms) \
                 VALUES (?, ?, ?, ?, NULL, NULL)",
                crate::db_params![
                    attempt.as_uuid().to_string(),
                    payload,
                    lifecycle.to_string(),
                    i64::MAX,
                ],
            )
            .await
            .expect("persist inert destroy completion with tombstone proof");

        assert!(
            store
                .recover_inert_destroy_completion(&attempt)
                .await
                .expect("prove inert completion from tombstone"),
            "the exact tombstone must remain readable while a destroy completion row is pending"
        );

        db.guard()
            .await
            .expect("guard")
            .execute(
                "UPDATE clustering_muc_destroy_outbox SET available_at_ms = ? WHERE attempt_id = ?",
                crate::db_params![crate::time::now_ms(), attempt.as_uuid().to_string()],
            )
            .await
            .expect("arm destroy completion");

        assert!(
            matches!(
                store
                    .commit_room_mutation(
                        &room_jid,
                        &fence,
                        RoomDurableMutation::Create {
                            waddle_id: waddle_id("waddle-destroy-completion-tombstone"),
                            channel_id: channel_id("channel-destroy-completion-tombstone"),
                            config: RoomConfig::default(),
                            initial_affiliations: vec![],
                        },
                        RoomMutationEffects::none()
                    )
                    .await,
                Err(RoomCommitError::RecreationBlocked)
            ),
            "an armed destroy completion must continue fencing recreation while its tombstone proof exists"
        );
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
                    waddle_id: waddle_id("waddle-1"),
                    channel_id: channel_id("chan-1"),
                    config: config.clone(),
                    initial_affiliations: Vec::new(),
                },
                waddle_xmpp::muc::RoomMutationEffects::none(),
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
                waddle_xmpp::muc::RoomMutationEffects::none(),
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
                waddle_xmpp::muc::RoomMutationEffects::none(),
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
                    waddle_id: waddle_id("waddle-1"),
                    channel_id: channel_id("chan-1"),
                    config: RoomConfig::default(),
                    initial_affiliations: Vec::new(),
                },
                waddle_xmpp::muc::RoomMutationEffects::none(),
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
                waddle_xmpp::muc::RoomMutationEffects::none(),
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
                waddle_xmpp::muc::RoomMutationEffects::none(),
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
            .commit_room_mutation(
                &jid,
                &fence,
                RoomDurableMutation::Subject(Some(subject)),
                waddle_xmpp::muc::RoomMutationEffects::none(),
            )
            .await;
        assert!(
            matches!(&result, Err(RoomCommitError::StateMissing)),
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
    async fn create_rejects_a_pending_destroy_completion_inside_its_fenced_transaction() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, db, me)) = clean_store().await else {
            return;
        };
        let room_jid = unique_room_jid("create-pending-destroy-completion");
        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        let epoch = claim_store
            .ensure_claimed(&entity, &me)
            .await
            .expect("claim room");
        let fence = RoomClaimFenceContext::new(entity, me, epoch);
        store.record_claim_fence(&room_jid, fence.clone());
        let payload = serde_json::json!({ "room_jid": room_jid.to_string() }).to_string();
        db.guard()
            .await
            .expect("guard")
            .execute(
                "INSERT INTO clustering_muc_destroy_outbox \
                 (attempt_id, payload_json, available_at_ms, lease_token, leased_at_ms) \
                 VALUES (?, ?, ?, NULL, NULL)",
                crate::db_params![
                    uuid::Uuid::new_v4().to_string(),
                    payload,
                    crate::time::now_ms()
                ],
            )
            .await
            .expect("persist armed destroy completion");

        assert!(
            matches!(
                store
                    .commit_room_mutation(
                        &room_jid,
                        &fence,
                        RoomDurableMutation::Create {
                            waddle_id: waddle_id("waddle-recreation-blocked"),
                            channel_id: channel_id("channel-recreation-blocked"),
                            config: RoomConfig::default(),
                            initial_affiliations: vec![],
                        },
                        waddle_xmpp::muc::RoomMutationEffects::none()
                    )
                    .await,
                Err(RoomCommitError::RecreationBlocked)
            ),
            "a pending destroy completion must reject create in the durable fenced transaction"
        );
        let mut rows = db
            .guard()
            .await
            .expect("guard")
            .query(
                "SELECT 1 FROM clustering_muc_rooms WHERE room_jid = ?",
                crate::db_params![room_jid.to_string()],
            )
            .await
            .expect("query room row");
        assert!(
            rows.next().await.expect("read room row").is_none(),
            "the rejected create must not write room state"
        );
    }

    #[tokio::test]
    async fn create_ignores_an_inert_destroy_completion_inside_its_fenced_transaction() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, db, me)) = clean_store().await else {
            return;
        };
        let room_jid = unique_room_jid("create-inert-destroy-completion");
        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        let epoch = claim_store
            .ensure_claimed(&entity, &me)
            .await
            .expect("claim room");
        let fence = RoomClaimFenceContext::new(entity, me, epoch);
        store.record_claim_fence(&room_jid, fence.clone());
        let payload = serde_json::json!({ "room_jid": room_jid.to_string() }).to_string();
        db.guard()
            .await
            .expect("guard")
            .execute(
                "INSERT INTO clustering_muc_destroy_outbox \
                 (attempt_id, payload_json, available_at_ms, lease_token, leased_at_ms) \
                 VALUES (?, ?, ?, NULL, NULL)",
                crate::db_params![uuid::Uuid::new_v4().to_string(), payload, i64::MAX],
            )
            .await
            .expect("persist inert destroy completion reservation");

        store
            .commit_room_mutation(
                &room_jid,
                &fence,
                RoomDurableMutation::Create {
                    waddle_id: waddle_id("waddle-recreation-inert"),
                    channel_id: channel_id("channel-recreation-inert"),
                    config: RoomConfig::default(),
                    initial_affiliations: vec![],
                },
                waddle_xmpp::muc::RoomMutationEffects::none(),
            )
            .await
            .expect("an inert completion reservation does not reject create");
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
