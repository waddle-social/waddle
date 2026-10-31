//! Implementation behind the parent module facade.

use super::admin_receipts;
use super::AffiliationEntry;
use super::BareJid;
use super::CancellationToken;
use super::DashMap;
use super::MucDestroyCompletionOutboxStore;
use super::NodeId;
use super::RelayHandle;
use super::RoomEffectEnqueue;
use super::RoomEffectOutboxStore;
use super::RoomEffectProducingNode;
use super::{Affiliation, XmppError};
use super::{ClaimEpoch, CurrentNodeIdentityGuard, Entity, EntityType, SharedNodeIdentity};
use super::{Database, DatabaseError, Transaction};
use super::{
    DestroyAttemptId, DurableRoomState, MucDurableFuture, MucDurableStore, RoomClaimFenceContext,
    RoomConfig, RoomLifecycleId, RoomLifecycleState, RoomRevision, SubjectState,
};
use super::{
    DurableAffiliationEntry, RoomCommitDatabaseError, RoomCommitError, RoomCommitFuture,
    RoomCommitOutcome, RoomCommittedCoordinates, RoomDurableMutation, RoomMutationEffects,
    RoomPinProjection, RoomProjection,
};

/// Dedicated transaction-scoped Postgres advisory lock for MUC store schema
/// bootstrap. It is distinct from the clustering claims lock
/// (`6_841_445_497_037_937_991`), migration-ledger lock
/// (`6_841_445_497_037_937_992`), and lineage lock
/// (`6_841_445_497_037_937_993`) because each protects a separate bootstrap
/// invariant and should not serialize unrelated startup work.
pub(in crate::muc_durable) const MUC_SCHEMA_ADVISORY_LOCK_KEY: i64 = 6_841_445_497_037_937_994;
pub(in crate::muc_durable) const ROOM_COMMIT_RETRY_ATTEMPTS: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::muc_durable) enum CommitReconciliation {
    Committed,
    NotCommitted,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::muc_durable) enum DestroyCompletionAttemptProof {
    Missing,
    Inert { lifecycle: Option<RoomLifecycleId> },
    Armed { lifecycle: Option<RoomLifecycleId> },
}

#[derive(serde::Deserialize)]
pub(in crate::muc_durable) struct PersistedDestroyCompletionRoom {
    pub(in crate::muc_durable) room_jid: BareJid,
}

pub(in crate::muc_durable) fn persisted_affiliation_fingerprint(
    entry: &DurableAffiliationEntry,
) -> serde_json::Value {
    serde_json::json!({
        "jid": entry.jid.clone(),
        "affiliation": entry.affiliation.map(affiliation_to_db_str),
    })
}

pub(in crate::muc_durable) fn projection_fingerprint(
    projection: &RoomProjection,
) -> serde_json::Value {
    match projection {
        RoomProjection::OccupancyJoin {
            occupant,
            nick,
            session,
        } => serde_json::json!({
            "kind": "occupancy_join",
            "occupant": occupant.to_string(),
            "nick": nick.as_str(),
            "session": session.to_string(),
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
pub(in crate::muc_durable) fn mutation_fingerprint(
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

pub(in crate::muc_durable) fn is_retryable_tx_error(error: &DatabaseError) -> bool {
    matches!(
        error,
        DatabaseError::Internal(sqlx::Error::Database(inner))
            if matches!(inner.code().as_deref(), Some("40001" | "40P01"))
                || (inner.code().as_deref() == Some("23505")
                    && inner.constraint()
                        == Some("clustering_muc_room_lifecycles_live_room_idx"))
    )
}

pub(in crate::muc_durable) fn commit_database_error() -> RoomCommitError {
    RoomCommitError::Database(RoomCommitDatabaseError::sanitized())
}

pub(in crate::muc_durable) fn db_err(error: DatabaseError) -> XmppError {
    // Durable MUC operations translate database failures into typed XMPP
    // errors, so record the internal failure before returning the protocol
    // response to the caller.
    crate::telemetry::mark_span_error("MUC durable storage operation failed");
    XmppError::internal(format!("MUC durable store backend error: {error}"))
}

pub(in crate::muc_durable) fn fence_db_unavailable(
    error: DatabaseError,
    fence: &RoomClaimFenceContext,
) -> XmppError {
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
pub(in crate::muc_durable) fn room_entity_key(room_jid: &BareJid) -> String {
    format!("{}:{}", EntityType::RoomActor.as_db_str(), room_jid)
}

/// `EntityType::as_db_str`/`from_db_str`'s exact convention, applied to
/// `Affiliation` (a closed, five-variant enum) for the
/// `clustering_muc_room_affiliations.affiliation` column — a small typed
/// mapping rather than a JSON blob for a value this simple.
pub(in crate::muc_durable) fn affiliation_to_db_str(affiliation: Affiliation) -> &'static str {
    match affiliation {
        Affiliation::Outcast => "outcast",
        Affiliation::None => "none",
        Affiliation::Member => "member",
        Affiliation::Admin => "admin",
        Affiliation::Owner => "owner",
    }
}

pub(in crate::muc_durable) fn affiliation_from_db_str(value: &str) -> Option<Affiliation> {
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
    pub(in crate::muc_durable) db: Database,
    pub(in crate::muc_durable) room_effect_outbox: RoomEffectOutboxStore,
    /// Live process incarnation. Cached room fences remain immutable; every
    /// use must still match this handle so a self-fenced old incarnation
    /// cannot write merely because its old claim row remains in Postgres.
    pub(in crate::muc_durable) node_identity: SharedNodeIdentity,
    /// This node's clustering-scope cancellation token, threaded into the
    /// `RelayHandle` [`Self::notify_previous_owner_demoted`] constructs
    /// per-call (mirroring `resume_asker::SwarmRemoteResumeAsker`'s
    /// identical "fresh `RelayHandle` per ask" pattern).
    pub(in crate::muc_durable) stop_token: CancellationToken,
    /// Exact claim fences established for actor-owned durable work.
    pub(in crate::muc_durable) exact_claim_fences: DashMap<BareJid, RoomClaimFenceContext>,
    /// Fence for the currently published local room actor. Retained for
    /// publication observability and legacy pre-fanout groupchat dispatch/MAM
    /// consumers; actor-derived durable work carries its immutable fence.
    pub(in crate::muc_durable) published_claim_fences: DashMap<BareJid, RoomClaimFenceContext>,
}

pub(in crate::muc_durable) fn remove_room_claim_fence_if(
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
        MucDestroyCompletionOutboxStore::try_new(db.clone())
            .await
            .map_err(db_err)?;
        let room_effect_outbox =
            RoomEffectOutboxStore::try_new(db.clone())
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

    pub(in crate::muc_durable) async fn ensure_schema(db: &Database) -> Result<(), DatabaseError> {
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
        admin_receipts::ensure_schema(&mut tx).await?;
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

    pub(in crate::muc_durable) fn fence_for(
        &self,
        room_jid: &BareJid,
    ) -> Result<RoomClaimFenceContext, XmppError> {
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

    pub(in crate::muc_durable) fn exact_fence_is_established(
        &self,
        room_jid: &BareJid,
        fence: &RoomClaimFenceContext,
    ) -> bool {
        self.exact_claim_fences.get(room_jid).as_deref() == Some(fence)
    }

    pub(in crate::muc_durable) async fn guard_fence_identity(
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
    pub(in crate::muc_durable) async fn assert_fenced(
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

    pub(in crate::muc_durable) async fn assert_fenced_for_update(
        &self,
        tx: &mut Transaction<'_>,
        room_jid: &BareJid,
        fence: &RoomClaimFenceContext,
    ) -> Result<(), RoomCommitError> {
        self.assert_commit_fenced(tx, room_jid, fence, "FOR UPDATE")
            .await
    }

    pub(in crate::muc_durable) async fn assert_commit_fenced(
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

    pub(in crate::muc_durable) async fn release_claim_in_tx(
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

    pub(in crate::muc_durable) fn commit_error(error: DatabaseError) -> RoomCommitError {
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

    pub(in crate::muc_durable) fn commit_fence_error(error: DatabaseError) -> RoomCommitError {
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

    pub(in crate::muc_durable) async fn load_destroy_completion_attempt_proof(
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

    pub(in crate::muc_durable) async fn reconcile_ambiguous_commit(
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

    pub(in crate::muc_durable) async fn commit_or_reconcile(
        &self,
        tx: Transaction<'_>,
        room_jid: &BareJid,
        fence: &RoomClaimFenceContext,
        intent: &RoomDurableMutation,
        outcome: RoomCommitOutcome,
        admin_mutation_id: Option<waddle_xmpp::muc::AdminMutationId>,
    ) -> Result<RoomCommitOutcome, RoomCommitError> {
        let RoomCommitOutcome {
            coordinates,
            reservation,
        } = outcome;
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
                let reconciliation = if let Some(attempt) = admin_mutation_id {
                    admin_receipts::load(&self.db, room_jid, attempt)
                        .await
                        .map(|receipt| match receipt {
                            Some(receipt) if receipt.coordinates == coordinates => {
                                CommitReconciliation::Committed
                            }
                            // A disconnected COMMIT may still be finishing.
                            // Only the later fenced successor can treat a
                            // missing receipt as proof to skip the projection.
                            Some(_) | None => CommitReconciliation::Unknown,
                        })
                        .map_err(|_| commit_database_error())
                } else {
                    self.reconcile_ambiguous_commit(room_jid, fence, intent, coordinates)
                        .await
                };
                match reconciliation {
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

    pub(in crate::muc_durable) async fn destroy_completion_blocks_create_in_tx(
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

    pub(in crate::muc_durable) async fn persist_idempotent_mutation_fingerprint_in_tx(
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

    pub(in crate::muc_durable) async fn write_commit_affiliation(
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

    pub(in crate::muc_durable) async fn enqueue_room_effects_in_tx(
        &self,
        tx: &mut Transaction<'_>,
        room_jid: &BareJid,
        intent: &RoomDurableMutation,
        coordinates: waddle_xmpp::muc::RoomCommittedCoordinates,
        effects: &RoomMutationEffects,
        producing_node: &RoomEffectProducingNode,
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
                RoomEffectEnqueue {
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

    pub(in crate::muc_durable) async fn commit_room_mutation_once(
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
            RoomEffectProducingNode::from_node_identity(authority.identity().clone())
        } else if let Some(identity_guard) = identity_guard.as_ref() {
            RoomEffectProducingNode::from_node_identity(identity_guard.identity().clone())
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
        // Lock order is room claim, connection generation, then room lifecycle.
        // Fresh bind publication only locks the generation row and releases it
        // before dispatching cleanup, so there is no reverse lock dependency.
        if let RoomDurableMutation::Projection(RoomProjection::OccupancyJoin {
            occupant,
            session,
            ..
        }) = intent
        {
            if !crate::occupancy_authority::lock_current(&mut tx, occupant, *session)
                .await
                .map_err(Self::commit_error)?
            {
                return Err(RoomCommitError::StaleOccupancyGeneration);
            }
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
                        RoomCommitOutcome {
                            coordinates: RoomCommittedCoordinates {
                                lifecycle,
                                revision,
                            },
                            reservation,
                        },
                        effects.admin_mutation_id(),
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
                    RoomCommitOutcome {
                        coordinates: RoomCommittedCoordinates {
                            lifecycle,
                            revision,
                        },
                        reservation: None,
                    },
                    effects.admin_mutation_id(),
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
                    RoomCommitOutcome {
                        coordinates: RoomCommittedCoordinates {
                            lifecycle,
                            revision,
                        },
                        reservation: None,
                    },
                    effects.admin_mutation_id(),
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
                    RoomCommitOutcome {
                        coordinates: RoomCommittedCoordinates {
                            lifecycle,
                            revision,
                        },
                        reservation: None,
                    },
                    effects.admin_mutation_id(),
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
                    RoomCommitOutcome {
                        coordinates: RoomCommittedCoordinates {
                            lifecycle,
                            revision,
                        },
                        reservation: None,
                    },
                    effects.admin_mutation_id(),
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
                    RoomCommitOutcome {
                        coordinates: RoomCommittedCoordinates {
                            lifecycle,
                            revision,
                        },
                        reservation: None,
                    },
                    effects.admin_mutation_id(),
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
                admin_receipts::delete_lifecycle_in_tx(&mut tx, lifecycle)
                    .await
                    .map_err(Self::commit_error)?;
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
        if let Some(attempt) = effects.admin_mutation_id() {
            admin_receipts::insert_in_tx(
                &mut tx,
                room_jid,
                attempt,
                &waddle_xmpp::muc::AdminMutationReceipt::from_effects(
                    RoomCommittedCoordinates {
                        lifecycle,
                        revision: next_revision,
                    },
                    effects,
                ),
            )
            .await
            .map_err(Self::commit_error)?;
        }
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
            RoomCommitOutcome {
                coordinates: RoomCommittedCoordinates {
                    lifecycle,
                    revision: next_revision,
                },
                reservation,
            },
            effects.admin_mutation_id(),
        )
        .await
    }

    pub(in crate::muc_durable) async fn exact_claim_is_held(
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

    pub(in crate::muc_durable) async fn load_room_state_in_tx(
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
    fn load_admin_mutation_receipt<'a>(
        &'a self,
        room_jid: &'a BareJid,
        attempt: waddle_xmpp::muc::AdminMutationId,
    ) -> waddle_xmpp::muc::MucDurableFuture<'a, Option<waddle_xmpp::muc::AdminMutationReceipt>>
    {
        Box::pin(admin_receipts::load(&self.db, room_jid, attempt))
    }

    fn delete_admin_mutation_receipt<'a>(
        &'a self,
        room_jid: &'a BareJid,
        attempt: waddle_xmpp::muc::AdminMutationId,
    ) -> waddle_xmpp::muc::MucDurableFuture<'a, ()> {
        Box::pin(admin_receipts::delete(&self.db, room_jid, attempt))
    }

    fn prune_admin_mutation_receipts(
        &self,
        retention: std::time::Duration,
        limit: usize,
    ) -> waddle_xmpp::muc::MucDurableFuture<'_, u64> {
        let cutoff_ms = crate::time::now_ms().saturating_sub(retention.as_millis() as i64);
        Box::pin(admin_receipts::prune_older_than(&self.db, cutoff_ms, limit))
    }

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
    /// standalone-autocommit `SELECT ... FOR SHARE` on the main pool. Only
    /// `dispatch_to_room` on the immediate (non-planning) interpreter path
    /// calls it, which in production means the non-ingress
    /// `handlers::message::handle_message` fallback. Authenticated client
    /// groupchat and relayed room submissions are planned instead, and their
    /// room fence is asserted inside the ingress transaction
    /// (`ingress/commit_room.rs`, `ClaimRepository::assert_room_claim`).
    /// Actor-owned durable operations use their immutable fence. #1283 owns
    /// this boundary's replacement.
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
                // dispatch's retry-then-bounce error branch.
                return Ok(false);
            };
            // Dispatch retries a backend error a bounded number of times and
            // then bounces the sender without demoting (#2046 W1.4), so `Err`
            // must mean uncertainty only, never a definitive answer. The
            // exact check keeps backend uncertainty typed while classifying
            // local identity rotation as definitively non-serving, without
            // holding the rotation gate across database I/O.
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
