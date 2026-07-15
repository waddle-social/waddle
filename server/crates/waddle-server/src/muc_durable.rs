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
//! `clustering_steal_intents` convention (ensure-schema-on-the-store, not a
//! versioned app migration, since these tables exist purely to back the
//! clustering-durability subsystem):
//! - `clustering_muc_rooms` — one row per durably-written room: `waddle_id`/
//!   `channel_id` plus the JSON-serialized `RoomConfig`/`SubjectState`
//!   (both already `Serialize`/`Deserialize` for exactly this purpose).
//! - `clustering_muc_room_affiliations` — one row per `(room_jid, member_jid)`
//!   affiliation grant, foreign-keyed to the parent room with cascading
//!   deletion so a grant cannot outlive its room. `affiliation` is stored via
//!   a small
//!   `affiliation_to_db_str`/`affiliation_from_db_str` pair, mirroring
//!   `EntityType::as_db_str`/`from_db_str`'s exact convention, rather than a
//!   JSON blob for a five-variant closed enum.
//!
//! **Fencing**: every `save_*` write runs [`PostgresMucRoomStore::assert_fenced`]
//! — the exact `SELECT ... FOR SHARE` shape `sm_persistence_fenced::
//! assert_fenced` already established — as the first statement inside the
//! same [`crate::db::Database::begin`] transaction as the write it guards,
//! on the **main pool**, never the control-plane pool (the Slice 0/4/7
//! pool-assignment rule). The epoch bound into that check comes from a
//! per-room cache ([`PostgresMucRoomStore::claim_fences`]) populated by the
//! room registry calling [`Self::record_claim_fence`] immediately after a
//! successful claim acquire/steal — never re-derived here, mirroring
//! `PostgresFencedSmPersistence`'s own "epoch side channel" design note.
//!
//! **Corrected code-research finding (this slice's own, not a plan
//! misattribution)**: element 7's "the MAM archive insert doubles as the
//! backstop when archiving is on" is not achievable within this slice's
//! Files list. MAM storage (`waddle_xmpp::mam::storage::sqlx_store`) issues
//! a plain `sqlx::QueryBuilder` insert directly against its own raw
//! `sqlx::PgPool`/`SqlitePool`, in `waddle-xmpp` — it has no access to
//! `waddle-server`'s `Database`/`Transaction` types (the crate-dependency
//! direction only runs `waddle-server -> waddle-xmpp`) and making the MAM
//! insert itself fenced would require restructuring the MAM storage
//! boundary, out of scope here. [`Self::check_fenced_fanout`] is therefore
//! the **sole** backstop, run unconditionally (both archiving-on and
//! archiving-off), as a standalone autocommit statement — the ADR's own
//! "otherwise the standalone autocommit fencing SELECT itself is the one
//! write-adjacent statement" text for the non-archiving case, generalized
//! to always apply. See the phase plan's deviation log for the full note.

use dashmap::DashMap;
use jid::BareJid;
use std::collections::HashSet;
use tokio_util::sync::CancellationToken;
use waddle_xmpp::muc::affiliation::AffiliationEntry;
use waddle_xmpp::muc::{
    DurableRoomState, MucDurableFuture, MucDurableStore, RoomClaimFenceContext, RoomConfig,
    SubjectState,
};
use waddle_xmpp::ownership::{
    ClaimEpoch, CurrentNodeIdentityGuard, Entity, EntityType, SharedNodeIdentity,
};
use waddle_xmpp::{Affiliation, XmppError};

use crate::clustering::relay::RelayHandle;
use crate::clustering::NodeId;
use crate::db::{ConnectionGuard, Database, DatabaseError, Transaction};

const AFFILIATION_CHECK_CONSTRAINT: &str = "clustering_muc_room_affiliations_value_check";
const AFFILIATION_ROOM_FK_CONSTRAINT: &str = "clustering_muc_room_affiliations_room_fk";
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoomDeleteStep {
    Parent,
    LegacyOrphanAffiliations,
}

impl RoomDeleteStep {
    const fn sql(self) -> &'static str {
        match self {
            Self::Parent => "DELETE FROM clustering_muc_rooms WHERE room_jid = ?",
            Self::LegacyOrphanAffiliations => {
                "DELETE FROM clustering_muc_room_affiliations WHERE room_jid = ?"
            }
        }
    }
}

const ROOM_DELETE_STEPS: [RoomDeleteStep; 2] = [
    RoomDeleteStep::Parent,
    RoomDeleteStep::LegacyOrphanAffiliations,
];

fn db_err(error: DatabaseError) -> XmppError {
    XmppError::internal(format!("MUC durable store backend error: {error}"))
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

fn affiliation_check_matches_expected(definition: &str) -> bool {
    const EXPECTED: &str = "check((affiliation=any(array['outcast'::text,'member'::text,'admin'::text,'owner'::text])))";

    let mut compact = String::with_capacity(definition.len());
    let mut characters = definition.chars().peekable();
    let mut in_literal = false;
    while let Some(character) = characters.next() {
        if character == '\'' {
            compact.push(character);
            if in_literal && characters.peek() == Some(&'\'') {
                compact.push(characters.next().expect("peeked SQL quote"));
            } else {
                in_literal = !in_literal;
            }
        } else if in_literal {
            compact.push(character);
        } else if !character.is_whitespace() {
            compact.push(character.to_ascii_lowercase());
        }
    }
    compact.strip_suffix("notvalid").unwrap_or(&compact) == EXPECTED
}

/// Postgres-backed [`MucDurableStore`] (ADR-0017 Phase 3 Slice 7). See the
/// module doc for the schema, fencing design, and the MAM-backstop
/// code-research correction.
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
    /// Per-room claim epoch cache — see the module doc's fencing section.
    claim_fences: DashMap<BareJid, RoomClaimFenceContext>,
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
            claim_fences: DashMap::new(),
        };
        store.ensure_schema().await.map_err(db_err)?;
        Ok(store)
    }

    async fn ensure_schema(&self) -> Result<(), DatabaseError> {
        let conn = self.db.guard().await?;
        conn.execute(
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
        conn.execute(
            r#"
            CREATE TABLE IF NOT EXISTS clustering_muc_room_affiliations (
                room_jid    TEXT NOT NULL,
                member_jid  TEXT NOT NULL,
                affiliation TEXT NOT NULL,
                reason      TEXT,
                granted_at  TIMESTAMPTZ,
                PRIMARY KEY (room_jid, member_jid),
                CONSTRAINT clustering_muc_room_affiliations_room_fk
                    FOREIGN KEY (room_jid)
                    REFERENCES clustering_muc_rooms (room_jid)
                    ON DELETE CASCADE,
                CONSTRAINT clustering_muc_room_affiliations_value_check
                    CHECK (affiliation IN ('outcast', 'member', 'admin', 'owner'))
            )
            "#,
            (),
        )
        .await?;
        conn.execute(
            r#"
            DO $muc_schema$
            BEGIN
                ALTER TABLE clustering_muc_room_affiliations
                    ADD CONSTRAINT clustering_muc_room_affiliations_value_check
                    CHECK (affiliation IN ('outcast', 'member', 'admin', 'owner'))
                    NOT VALID;
            EXCEPTION
                WHEN duplicate_object THEN NULL;
            END
            $muc_schema$
            "#,
            (),
        )
        .await?;
        // Existing deployments created the affiliation table before it had a
        // parent-room foreign key. `NOT VALID` leaves any legacy corruption
        // available for the fail-closed loader to diagnose while PostgreSQL
        // still enforces the relationship for every new insert/update. The
        // duplicate-object handler makes concurrent/idempotent startup safe;
        // new installs already received the same named constraint inline.
        conn.execute(
            r#"
            DO $muc_schema$
            BEGIN
                ALTER TABLE clustering_muc_room_affiliations
                    ADD CONSTRAINT clustering_muc_room_affiliations_room_fk
                    FOREIGN KEY (room_jid)
                    REFERENCES clustering_muc_rooms (room_jid)
                    ON DELETE CASCADE
                    NOT VALID;
            EXCEPTION
                WHEN duplicate_object THEN NULL;
            END
            $muc_schema$
            "#,
            (),
        )
        .await?;
        Self::verify_affiliation_constraints(&conn).await?;
        conn.execute(
            r#"
            CREATE INDEX IF NOT EXISTS clustering_muc_room_affiliations_room_jid_idx
                ON clustering_muc_room_affiliations (room_jid)
            "#,
            (),
        )
        .await?;
        Ok(())
    }

    async fn verify_affiliation_constraints(conn: &ConnectionGuard) -> Result<(), DatabaseError> {
        let mut check_rows = conn
            .query(
                r#"
                SELECT pg_get_constraintdef(c.oid)
                FROM pg_constraint AS c
                JOIN pg_attribute AS a
                  ON a.attrelid = c.conrelid
                 AND a.attname = 'affiliation'
                WHERE c.conrelid = 'clustering_muc_room_affiliations'::regclass
                  AND c.conname = ?
                  AND c.contype = 'c'
                  AND c.conkey = ARRAY[a.attnum]::int2[]
                "#,
                crate::db_params![AFFILIATION_CHECK_CONSTRAINT],
            )
            .await?;
        let definition = check_rows
            .next()
            .await?
            .ok_or_else(|| {
                DatabaseError::QueryFailed(format!(
                    "MUC affiliation schema is missing the required {AFFILIATION_CHECK_CONSTRAINT} constraint"
                ))
            })?
            .get::<String>(0)?;
        if check_rows.next().await?.is_some() || !affiliation_check_matches_expected(&definition) {
            return Err(DatabaseError::QueryFailed(format!(
                "MUC affiliation constraint {AFFILIATION_CHECK_CONSTRAINT} has an unexpected definition: {definition}"
            )));
        }

        let mut fk_rows = conn
            .query(
                r#"
                SELECT count(*)
                FROM pg_constraint AS c
                JOIN pg_attribute AS child_column
                  ON child_column.attrelid = c.conrelid
                 AND child_column.attname = 'room_jid'
                JOIN pg_attribute AS parent_column
                  ON parent_column.attrelid = c.confrelid
                 AND parent_column.attname = 'room_jid'
                WHERE c.conrelid = 'clustering_muc_room_affiliations'::regclass
                  AND c.conname = ?
                  AND c.contype = 'f'
                  AND c.confrelid = 'clustering_muc_rooms'::regclass
                  AND c.conkey = ARRAY[child_column.attnum]::int2[]
                  AND c.confkey = ARRAY[parent_column.attnum]::int2[]
                  AND c.confdeltype = 'c'
                  AND NOT c.condeferrable
                "#,
                crate::db_params![AFFILIATION_ROOM_FK_CONSTRAINT],
            )
            .await?;
        let matching_fk_count = fk_rows
            .next()
            .await?
            .ok_or_else(|| {
                DatabaseError::QueryFailed(
                    "MUC affiliation FK verification returned no count row".to_string(),
                )
            })?
            .get::<i64>(0)?;
        if matching_fk_count != 1 {
            return Err(DatabaseError::QueryFailed(format!(
                "MUC affiliation constraint {AFFILIATION_ROOM_FK_CONSTRAINT} must be a non-deferrable room_jid FK with ON DELETE CASCADE"
            )));
        }
        Ok(())
    }

    fn fence_for(&self, room_jid: &BareJid) -> Result<RoomClaimFenceContext, XmppError> {
        let fence = self
            .claim_fences
            .get(room_jid)
            .map(|entry| entry.clone())
            .ok_or_else(|| {
                XmppError::internal(format!(
                    "no claim epoch recorded for room {room_jid}; durable write skipped \
                 (the room registry must call record_claim_fence before any write)"
                ))
            })?;
        if self.node_identity.current() != fence.owner {
            remove_room_claim_fence_if(&self.claim_fences, room_jid, &fence);
            return Err(XmppError::internal(format!(
                "durable write for room {room_jid} aborted: cached claim belongs to a stale node incarnation"
            )));
        }
        Ok(fence)
    }

    async fn guard_fence_identity(
        &self,
        room_jid: &BareJid,
        fence: &RoomClaimFenceContext,
    ) -> Result<CurrentNodeIdentityGuard, XmppError> {
        self.node_identity
            .guard_if_current(&fence.owner)
            .await
            .ok_or_else(|| {
                XmppError::internal(format!(
                    "durable write for room {room_jid} aborted during node identity rotation"
                ))
            })
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
        if self.node_identity.current() != fence.owner {
            remove_room_claim_fence_if(&self.claim_fences, room_jid, fence);
            return Err(XmppError::internal(format!(
                "durable write for room {room_jid} aborted: claim fence belongs to a stale node incarnation"
            )));
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
        .map_err(db_err)?;
        let held = rows.next().await.map_err(db_err)?.is_some();
        if held && self.node_identity.current() == fence.owner {
            Ok(())
        } else {
            remove_room_claim_fence_if(&self.claim_fences, room_jid, fence);
            Err(XmppError::internal(format!(
                "durable write for room {room_jid} aborted: this node no longer holds the \
                 room's ownership claim (0 rows from the fencing SELECT)"
            )))
        }
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
            let mut orphan_rows = tx
                .query(
                    "SELECT member_jid FROM clustering_muc_room_affiliations \
                     WHERE room_jid = ? LIMIT 1",
                    crate::db_params![room_jid.to_string()],
                )
                .await
                .map_err(db_err)?;
            if let Some(orphan) = orphan_rows.next().await.map_err(db_err)? {
                let member_jid: String = orphan.get(0).map_err(db_err)?;
                return Err(XmppError::internal(format!(
                    "durable room state is corrupt: affiliation for {member_jid} references \
                     missing room {room_jid}"
                )));
            }
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
        let mut affiliation_jids = HashSet::new();
        while let Some(row) = affiliation_rows.next().await.map_err(db_err)? {
            let member_jid: String = row.get(0).map_err(db_err)?;
            let affiliation_str: String = row.get(1).map_err(db_err)?;
            let reason: Option<String> = row.get(2).map_err(db_err)?;
            let jid = member_jid.parse::<BareJid>().map_err(|error| {
                XmppError::internal(format!(
                    "durable room state is corrupt: affiliation member JID {member_jid:?} \
                     for room {room_jid} is invalid: {error}"
                ))
            })?;
            let canonical_jid = jid.to_string();
            if member_jid != canonical_jid {
                return Err(XmppError::internal(format!(
                    "durable room state is corrupt: affiliation member JID {member_jid:?} \
                     for room {room_jid} is non-canonical; canonical form is {canonical_jid:?}"
                )));
            }
            if !affiliation_jids.insert(jid.clone()) {
                return Err(XmppError::internal(format!(
                    "durable room state is corrupt: duplicate normalized affiliation member \
                     JID {canonical_jid} for room {room_jid}"
                )));
            }
            let affiliation = match affiliation_from_db_str(&affiliation_str) {
                Some(Affiliation::None) => {
                    return Err(XmppError::internal(format!(
                        "durable room state is corrupt: affiliation tag 'none' for member \
                         {member_jid} in room {room_jid} must be represented by no row"
                    )));
                }
                Some(affiliation) => affiliation,
                None => {
                    return Err(XmppError::internal(format!(
                        "durable room state is corrupt: affiliation tag {affiliation_str:?} \
                         for member {member_jid} in room {room_jid} is invalid"
                    )));
                }
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
    fn load_room_state<'a>(
        &'a self,
        room_jid: &'a BareJid,
    ) -> MucDurableFuture<'a, Option<DurableRoomState>> {
        Box::pin(async move {
            let mut tx = self.db.begin().await.map_err(db_err)?;
            tx.execute(
                "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY",
                (),
            )
            .await
            .map_err(db_err)?;
            let state = Self::load_room_state_in_tx(&mut tx, room_jid).await?;
            tx.commit().await.map_err(db_err)?;
            Ok(state)
        })
    }

    fn save_config<'a>(
        &'a self,
        room_jid: &'a BareJid,
        waddle_id: &'a str,
        channel_id: &'a str,
        config: &'a RoomConfig,
    ) -> MucDurableFuture<'a, ()> {
        Box::pin(async move {
            let fence = self.fence_for(room_jid)?;
            let _identity_guard = self.guard_fence_identity(room_jid, &fence).await?;
            let config_json = serde_json::to_string(config).map_err(|error| {
                XmppError::internal(format!("durable room config encode failed: {error}"))
            })?;
            let mut tx = self.db.begin().await.map_err(db_err)?;
            self.assert_fenced(&mut tx, room_jid, &fence).await?;
            let affected = tx.execute(
                r#"
                INSERT INTO clustering_muc_rooms (room_jid, waddle_id, channel_id, config_json, updated_at)
                VALUES (?, ?, ?, ?, now())
                ON CONFLICT (room_jid) DO UPDATE SET
                    waddle_id = excluded.waddle_id,
                    channel_id = excluded.channel_id,
                    config_json = excluded.config_json,
                    updated_at = excluded.updated_at
                "#,
                crate::db_params![
                    room_jid.to_string(),
                    waddle_id.to_string(),
                    channel_id.to_string(),
                    config_json,
                ],
            )
            .await
            .map_err(db_err)?;
            if affected != 1 {
                return Err(XmppError::internal(format!(
                    "durable room config persist for {room_jid} affected {affected} rows; expected 1"
                )));
            }
            tx.commit().await.map_err(db_err)?;
            Ok(())
        })
    }

    fn save_subject<'a>(
        &'a self,
        room_jid: &'a BareJid,
        subject: Option<&'a SubjectState>,
    ) -> MucDurableFuture<'a, ()> {
        Box::pin(async move {
            let fence = self.fence_for(room_jid)?;
            let _identity_guard = self.guard_fence_identity(room_jid, &fence).await?;
            let subject_json = subject
                .map(serde_json::to_string)
                .transpose()
                .map_err(|error| {
                    XmppError::internal(format!("durable room subject encode failed: {error}"))
                })?;
            let mut tx = self.db.begin().await.map_err(db_err)?;
            self.assert_fenced(&mut tx, room_jid, &fence).await?;
            // The room row must already exist (a subject can only be set on
            // an already-spawned room, which always durably writes its
            // config at spawn-adjacent points first) — an UPDATE-only
            // statement here, not an upsert, so a missing row is a loud
            // 0-rows-affected rather than a silently-incomplete insert
            // missing `waddle_id`/`channel_id`.
            let affected = tx
                .execute(
                    "UPDATE clustering_muc_rooms SET subject_json = ?, updated_at = now() WHERE room_jid = ?",
                    crate::db_params![subject_json, room_jid.to_string()],
                )
                .await
                .map_err(db_err)?;
            if affected == 0 {
                return Err(XmppError::internal(format!(
                    "durable subject persist refused: room {room_jid} has no durable config row"
                )));
            }
            tx.commit().await.map_err(db_err)?;
            Ok(())
        })
    }

    fn save_affiliation<'a>(
        &'a self,
        room_jid: &'a BareJid,
        entry: &'a AffiliationEntry,
    ) -> MucDurableFuture<'a, ()> {
        Box::pin(async move {
            let fence = self.fence_for(room_jid)?;
            let _identity_guard = self.guard_fence_identity(room_jid, &fence).await?;
            let mut tx = self.db.begin().await.map_err(db_err)?;
            self.assert_fenced(&mut tx, room_jid, &fence).await?;
            let mut parent_rows = tx
                .query(
                    "SELECT 1 FROM clustering_muc_rooms WHERE room_jid = ? FOR KEY SHARE",
                    crate::db_params![room_jid.to_string()],
                )
                .await
                .map_err(db_err)?;
            if parent_rows.next().await.map_err(db_err)?.is_none() {
                return Err(XmppError::internal(format!(
                    "durable affiliation persist refused: room {room_jid} has no durable config row"
                )));
            }
            if entry.affiliation == Affiliation::None {
                tx.execute(
                    "DELETE FROM clustering_muc_room_affiliations WHERE room_jid = ? AND member_jid = ?",
                    crate::db_params![room_jid.to_string(), entry.jid.to_string()],
                )
                .await
                .map_err(db_err)?;
            } else {
                let affected = tx
                    .execute(
                        r#"
                        INSERT INTO clustering_muc_room_affiliations
                            (room_jid, member_jid, affiliation, reason)
                        VALUES (?, ?, ?, ?)
                        ON CONFLICT (room_jid, member_jid) DO UPDATE SET
                            affiliation = excluded.affiliation,
                            reason = excluded.reason
                        "#,
                        crate::db_params![
                            room_jid.to_string(),
                            entry.jid.to_string(),
                            affiliation_to_db_str(entry.affiliation).to_string(),
                            entry.reason.clone(),
                        ],
                    )
                    .await
                    .map_err(db_err)?;
                if affected != 1 {
                    return Err(XmppError::internal(format!(
                        "durable affiliation persist refused: room {room_jid} has no durable config row"
                    )));
                }
            }
            tx.commit().await.map_err(db_err)?;
            Ok(())
        })
    }

    /// XEP-0045 §10.9 (#1261): a destroyed room must not resurrect from
    /// durable storage. Epoch-fenced like every other write — a node
    /// that lost the claim mid-destroy must not wipe the new owner's
    /// rows.
    fn delete_room_state<'a>(&'a self, room_jid: &'a BareJid) -> MucDurableFuture<'a, ()> {
        Box::pin(async move {
            let fence = self.fence_for(room_jid)?;
            let _identity_guard = self.guard_fence_identity(room_jid, &fence).await?;
            let mut tx = self.db.begin().await.map_err(db_err)?;
            self.assert_fenced(&mut tx, room_jid, &fence).await?;
            // Parent-first ordering matches `save_affiliation`, which locks
            // the parent before touching a child row. The enforced FK owns
            // child deletion through ON DELETE CASCADE; manually deleting a
            // child first would invert that lock order and deadlock against a
            // concurrent affiliation update/removal.
            // The ordered step list is also the regression seam: production
            // executes exactly this parent-first sequence, while the unit test
            // pins the order. The second step removes legacy NOT VALID orphans;
            // for valid rooms the FK cascade already removed every child.
            for step in ROOM_DELETE_STEPS {
                tx.execute(step.sql(), crate::db_params![room_jid.to_string()])
                    .await
                    .map_err(db_err)?;
            }
            tx.commit().await.map_err(db_err)?;
            Ok(())
        })
    }

    fn record_claim_fence(&self, room_jid: &BareJid, fence: RoomClaimFenceContext) {
        self.claim_fences.insert(room_jid.clone(), fence);
    }

    fn forget_claim_fence(&self, room_jid: &BareJid, expected: &RoomClaimFenceContext) {
        remove_room_claim_fence_if(&self.claim_fences, room_jid, expected);
    }

    /// The guaranteed demotion backstop (element 7): a fenced,
    /// standalone-autocommit `SELECT ... FOR SHARE` on the main pool, run
    /// before every local fan-out. See the module doc's MAM-backstop
    /// correction for why this is the sole backstop mechanism this slice
    /// lands, unconditionally (both archiving-on and archiving-off).
    fn check_fenced_fanout<'a>(&'a self, room_jid: &'a BareJid) -> MucDurableFuture<'a, bool> {
        Box::pin(async move {
            let fence = self
                .claim_fences
                .get(room_jid)
                .map(|entry| entry.clone())
                .ok_or_else(|| {
                    XmppError::internal(format!(
                        "no claim epoch recorded for room {room_jid}; fenced fan-out skipped"
                    ))
                })?;
            // Identity rotation is a definitive local ownership loss, not a
            // transient storage failure. Return `Ok(false)` so fan-out and
            // mutation gates fail closed instead of taking their transient
            // error path (which intentionally fails open).
            if self.node_identity.current() != fence.owner {
                remove_room_claim_fence_if(&self.claim_fences, room_jid, &fence);
                return Ok(false);
            }
            let key = room_entity_key(room_jid);
            let conn = self.db.guard().await.map_err(db_err)?;
            let mut rows = conn
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
                .map_err(db_err)?;
            let held = rows.next().await.map_err(db_err)?.is_some();
            if held && self.node_identity.current() == fence.owner {
                Ok(true)
            } else {
                remove_room_claim_fence_if(&self.claim_fences, room_jid, &fence);
                Ok(false)
            }
        })
    }

    /// ADR-0017 Phase 3 Slice 7 FIX 1: exposes the exact `(Entity,
    /// ClaimEpoch, node_id)` triple [`Self::check_fenced_fanout`]/
    /// [`Self::assert_fenced`] already resolve from `self.claim_fences`, so
    /// `groupchat_archive.rs`'s MAM fenced write can bind the identical
    /// typed context rather than re-deriving it from a second mechanism.
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
    use std::sync::Arc;
    use waddle_xmpp::muc::RoomSubjectTexts;
    use waddle_xmpp::ownership::{ClaimStore, NodeIdentity, StalePredicate};

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

    #[test]
    fn affiliation_check_matcher_requires_the_exact_closed_set_and_shape() {
        let expected = "CHECK ((affiliation = ANY (ARRAY['outcast'::text, 'member'::text, 'admin'::text, 'owner'::text]))) NOT VALID";
        assert!(affiliation_check_matches_expected(expected));
        assert!(!affiliation_check_matches_expected(
            "CHECK ((affiliation = ANY (ARRAY['outcast'::text, 'member'::text, 'admin'::text, 'owner'::text, 'superowner'::text])))"
        ));
        assert!(!affiliation_check_matches_expected(
            "CHECK (((affiliation = ANY (ARRAY['outcast'::text, 'member'::text, 'admin'::text, 'owner'::text])) OR true))"
        ));
        assert!(!affiliation_check_matches_expected(
            "CHECK ((affiliation = ANY (ARRAY['outcast'::text, 'member'::text, 'admin'::text, 'owner'::text, NULL::text])))"
        ));
        assert!(!affiliation_check_matches_expected(
            "CHECK ((affiliation = ANY (ARRAY['OUTCAST'::text, 'MEMBER'::text, 'ADMIN'::text, 'OWNER'::text])))"
        ));
    }

    #[test]
    fn room_delete_keeps_parent_before_legacy_orphan_cleanup() {
        assert_eq!(
            ROOM_DELETE_STEPS,
            [
                RoomDeleteStep::Parent,
                RoomDeleteStep::LegacyOrphanAffiliations,
            ]
        );
        assert_eq!(
            ROOM_DELETE_STEPS.map(RoomDeleteStep::sql),
            [
                "DELETE FROM clustering_muc_rooms WHERE room_jid = ?",
                "DELETE FROM clustering_muc_room_affiliations WHERE room_jid = ?",
            ]
        );
    }

    async fn legacy_corruption_db(name: &str) -> Database {
        let mut config = DatabaseConfig::new(DatabaseDriver::Sqlite, "sqlite::memory:");
        // An in-memory SQLite database is per connection. Pinning this test
        // fixture to one pooled connection keeps the deliberately legacy
        // schema visible to the later transaction deterministically.
        config.pool_size = 1;
        let db = Database::from_config(name, &config)
            .await
            .expect("open in-memory database");
        let conn = db.guard().await.expect("guard");
        conn.execute(
            r#"
            CREATE TABLE clustering_muc_rooms (
                room_jid TEXT PRIMARY KEY,
                waddle_id TEXT NOT NULL,
                channel_id TEXT NOT NULL,
                config_json TEXT NOT NULL,
                subject_json TEXT
            )
            "#,
            (),
        )
        .await
        .expect("create rooms table");
        // Deliberately model the pre-hardening schema so corruption can be
        // seeded without the new FK rejecting it at write time.
        conn.execute(
            r#"
            CREATE TABLE clustering_muc_room_affiliations (
                room_jid TEXT NOT NULL,
                member_jid TEXT NOT NULL,
                affiliation TEXT NOT NULL,
                reason TEXT,
                PRIMARY KEY (room_jid, member_jid)
            )
            "#,
            (),
        )
        .await
        .expect("create legacy affiliations table");
        drop(conn);
        db
    }

    async fn seed_room_and_affiliation(
        db: &Database,
        jid: &BareJid,
        member_jid: &str,
        affiliation: &str,
    ) {
        let config_json = serde_json::to_string(&RoomConfig::default()).expect("encode config");
        let conn = db.guard().await.expect("guard");
        conn.execute(
            "INSERT INTO clustering_muc_rooms \
             (room_jid, waddle_id, channel_id, config_json) VALUES (?, ?, ?, ?)",
            crate::db_params![
                jid.to_string(),
                "waddle-1".to_string(),
                "channel-1".to_string(),
                config_json,
            ],
        )
        .await
        .expect("seed room");
        conn.execute(
            "INSERT INTO clustering_muc_room_affiliations \
             (room_jid, member_jid, affiliation) VALUES (?, ?, ?)",
            crate::db_params![
                jid.to_string(),
                member_jid.to_string(),
                affiliation.to_string(),
            ],
        )
        .await
        .expect("seed affiliation");
    }

    #[tokio::test]
    async fn load_errors_on_legacy_orphan_affiliation_row() {
        let db = legacy_corruption_db("muc-orphan-affiliation-test").await;
        let jid = room_jid("orphan");
        let conn = db.guard().await.expect("guard");
        conn.execute(
            "INSERT INTO clustering_muc_room_affiliations \
             (room_jid, member_jid, affiliation) VALUES (?, ?, ?)",
            crate::db_params![
                jid.to_string(),
                "alice@example.com".to_string(),
                "member".to_string(),
            ],
        )
        .await
        .expect("seed orphan affiliation");
        drop(conn);

        let mut tx = db.begin().await.expect("begin");
        let error = PostgresMucRoomStore::load_room_state_in_tx(&mut tx, &jid)
            .await
            .expect_err("orphan affiliation must fail closed");
        assert!(
            error.to_string().contains("references missing room"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn load_errors_on_malformed_affiliation_member_jid() {
        let db = legacy_corruption_db("muc-malformed-affiliation-jid-test").await;
        let jid = room_jid("malformed-member");
        seed_room_and_affiliation(&db, &jid, "@broken", "member").await;

        let mut tx = db.begin().await.expect("begin");
        let error = PostgresMucRoomStore::load_room_state_in_tx(&mut tx, &jid)
            .await
            .expect_err("malformed member JID must fail closed");
        assert!(
            error.to_string().contains("member JID") && error.to_string().contains("is invalid"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn load_errors_on_unknown_affiliation_tag() {
        let db = legacy_corruption_db("muc-malformed-affiliation-tag-test").await;
        let jid = room_jid("malformed-tag");
        seed_room_and_affiliation(&db, &jid, "alice@example.com", "superowner").await;

        let mut tx = db.begin().await.expect("begin");
        let error = PostgresMucRoomStore::load_room_state_in_tx(&mut tx, &jid)
            .await
            .expect_err("unknown affiliation tag must fail closed");
        assert!(
            error.to_string().contains("affiliation tag")
                && error.to_string().contains("is invalid"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn load_errors_on_conflicting_canonical_affiliation_aliases() {
        let db = legacy_corruption_db("muc-canonical-affiliation-alias-test").await;
        let jid = room_jid("canonical-alias");
        seed_room_and_affiliation(&db, &jid, "ßA@IX.test", "owner").await;
        let conn = db.guard().await.expect("guard");
        conn.execute(
            "INSERT INTO clustering_muc_room_affiliations \
             (room_jid, member_jid, affiliation) VALUES (?, ?, ?)",
            crate::db_params![
                jid.to_string(),
                "ssa@ix.test".to_string(),
                "outcast".to_string(),
            ],
        )
        .await
        .expect("seed conflicting canonical alias");
        drop(conn);

        let mut tx = db.begin().await.expect("begin");
        let error = PostgresMucRoomStore::load_room_state_in_tx(&mut tx, &jid)
            .await
            .expect_err("canonical aliases must fail closed instead of resolving by row order");
        assert!(
            error.to_string().contains("non-canonical")
                && error.to_string().contains("ssa@ix.test"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn load_errors_on_persisted_none_affiliation() {
        let db = legacy_corruption_db("muc-persisted-none-affiliation-test").await;
        let jid = room_jid("persisted-none");
        seed_room_and_affiliation(&db, &jid, "alice@example.com", "none").await;

        let mut tx = db.begin().await.expect("begin");
        let error = PostgresMucRoomStore::load_room_state_in_tx(&mut tx, &jid)
            .await
            .expect_err("a persisted none row must fail closed");
        assert!(
            error.to_string().contains("must be represented by no row"),
            "unexpected error: {error}"
        );
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
            claim_fences: DashMap::new(),
        };
        let jid = room_jid("identity-rotation");
        let entity = Entity::new(EntityType::RoomActor, jid.to_string());
        store.record_claim_fence(
            &jid,
            RoomClaimFenceContext::new(entity, original, ClaimEpoch(7)),
        );
        assert!(store.current_claim_fence(&jid).is_some());

        live_identity.rotate(node_identity()).await;

        assert!(
            !store
                .check_fenced_fanout(&jid)
                .await
                .expect("identity rotation is definitive ownership loss"),
            "a rotated cached fence must return false, never a transient error that callers fail open"
        );
        assert!(store.current_claim_fence(&jid).is_none());
        assert!(store.claim_fences.get(&jid).is_none());
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
        conn.execute("DELETE FROM clustering_muc_room_affiliations", ())
            .await
            .expect("clean affiliations");
        conn.execute("DELETE FROM clustering_muc_rooms", ())
            .await
            .expect("clean rooms");
        Some((store, claim_store, db, me))
    }

    #[tokio::test]
    async fn ensure_schema_upgrades_legacy_affiliation_constraints_idempotently() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, _claim_store, db, _me)) = clean_store().await else {
            return;
        };
        let legacy_room = room_jid("legacy-orphan");
        let conn = db.guard().await.expect("guard");
        conn.execute(
            "ALTER TABLE clustering_muc_room_affiliations \
             DROP CONSTRAINT IF EXISTS clustering_muc_room_affiliations_room_fk",
            (),
        )
        .await
        .expect("drop room FK to model the legacy schema");
        conn.execute(
            "ALTER TABLE clustering_muc_room_affiliations \
             DROP CONSTRAINT IF EXISTS clustering_muc_room_affiliations_value_check",
            (),
        )
        .await
        .expect("drop affiliation check to model the legacy schema");
        conn.execute(
            "INSERT INTO clustering_muc_room_affiliations \
             (room_jid, member_jid, affiliation) VALUES (?, ?, ?)",
            crate::db_params![
                legacy_room.to_string(),
                "legacy@example.com".to_string(),
                "none".to_string(),
            ],
        )
        .await
        .expect("seed legacy corruption before NOT VALID constraints exist");
        drop(conn);

        store.ensure_schema().await.expect("upgrade legacy schema");
        store
            .ensure_schema()
            .await
            .expect("schema upgrade is idempotent");

        let conn = db.guard().await.expect("guard");
        for constraint in [
            "clustering_muc_room_affiliations_room_fk",
            "clustering_muc_room_affiliations_value_check",
        ] {
            let mut rows = conn
                .query(
                    "SELECT convalidated FROM pg_constraint \
                     WHERE conrelid = 'clustering_muc_room_affiliations'::regclass \
                       AND conname = ?",
                    crate::db_params![constraint.to_string()],
                )
                .await
                .expect("inspect upgraded constraint");
            let row = rows
                .next()
                .await
                .expect("read constraint row")
                .expect("upgraded constraint exists");
            let validated: bool = row.get(0).expect("read validation state");
            assert!(
                !validated,
                "legacy constraint {constraint} must remain NOT VALID so strict loading can diagnose existing corruption"
            );
        }

        let new_orphan = room_jid("new-orphan");
        assert!(
            conn.execute(
                "INSERT INTO clustering_muc_room_affiliations \
                 (room_jid, member_jid, affiliation) VALUES (?, ?, ?)",
                crate::db_params![
                    new_orphan.to_string(),
                    "alice@example.com".to_string(),
                    "member".to_string(),
                ],
            )
            .await
            .is_err(),
            "the upgraded FK must reject new orphan rows"
        );

        let parent = room_jid("constraint-parent");
        let config_json = serde_json::to_string(&RoomConfig::default()).expect("encode config");
        conn.execute(
            "INSERT INTO clustering_muc_rooms \
             (room_jid, waddle_id, channel_id, config_json) VALUES (?, ?, ?, ?)",
            crate::db_params![
                parent.to_string(),
                "waddle-1".to_string(),
                "channel-1".to_string(),
                config_json,
            ],
        )
        .await
        .expect("seed parent room");
        assert!(
            conn.execute(
                "INSERT INTO clustering_muc_room_affiliations \
                 (room_jid, member_jid, affiliation) VALUES (?, ?, ?)",
                crate::db_params![
                    parent.to_string(),
                    "invalid@example.com".to_string(),
                    "none".to_string(),
                ],
            )
            .await
            .is_err(),
            "the upgraded check must reject new none rows"
        );
        conn.execute(
            "INSERT INTO clustering_muc_room_affiliations \
             (room_jid, member_jid, affiliation) VALUES (?, ?, ?)",
            crate::db_params![
                parent.to_string(),
                "member@example.com".to_string(),
                "member".to_string(),
            ],
        )
        .await
        .expect("seed valid child row");
        conn.execute(
            "DELETE FROM clustering_muc_rooms WHERE room_jid = ?",
            crate::db_params![parent.to_string()],
        )
        .await
        .expect("delete parent room");
        let mut child_rows = conn
            .query(
                "SELECT 1 FROM clustering_muc_room_affiliations WHERE room_jid = ?",
                crate::db_params![parent.to_string()],
            )
            .await
            .expect("query cascaded child");
        assert!(
            child_rows.next().await.expect("read child row").is_none(),
            "deleting a parent must cascade to its affiliations"
        );
    }

    #[tokio::test]
    async fn ensure_schema_rejects_same_named_wrong_affiliation_constraints() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, _claim_store, db, _me)) = clean_store().await else {
            return;
        };
        let conn = db.guard().await.expect("guard");
        conn.execute(
            "ALTER TABLE clustering_muc_room_affiliations \
             DROP CONSTRAINT clustering_muc_room_affiliations_value_check",
            (),
        )
        .await
        .expect("drop canonical CHECK");
        conn.execute(
            "ALTER TABLE clustering_muc_room_affiliations \
             ADD CONSTRAINT clustering_muc_room_affiliations_value_check \
             CHECK (affiliation <> '') NOT VALID",
            (),
        )
        .await
        .expect("install same-named wrong CHECK");
        drop(conn);

        let check_error = store
            .ensure_schema()
            .await
            .expect_err("same-named wrong CHECK must fail startup");
        assert!(
            check_error.to_string().contains("unexpected definition"),
            "unexpected error: {check_error}"
        );

        let conn = db.guard().await.expect("guard");
        conn.execute(
            "ALTER TABLE clustering_muc_room_affiliations \
             DROP CONSTRAINT clustering_muc_room_affiliations_value_check",
            (),
        )
        .await
        .expect("drop wrong CHECK");
        drop(conn);
        store
            .ensure_schema()
            .await
            .expect("restore canonical CHECK");

        let conn = db.guard().await.expect("guard");
        conn.execute(
            "ALTER TABLE clustering_muc_room_affiliations \
             DROP CONSTRAINT clustering_muc_room_affiliations_value_check",
            (),
        )
        .await
        .expect("drop canonical CHECK before uppercase fixture");
        conn.execute(
            "ALTER TABLE clustering_muc_room_affiliations \
             ADD CONSTRAINT clustering_muc_room_affiliations_value_check \
             CHECK (affiliation IN ('OUTCAST', 'MEMBER', 'ADMIN', 'OWNER')) NOT VALID",
            (),
        )
        .await
        .expect("install same-named uppercase CHECK");
        drop(conn);

        let uppercase_error = store
            .ensure_schema()
            .await
            .expect_err("uppercase affiliation literals must fail startup");
        assert!(
            uppercase_error
                .to_string()
                .contains("unexpected definition"),
            "unexpected error: {uppercase_error}"
        );

        let conn = db.guard().await.expect("guard");
        conn.execute(
            "ALTER TABLE clustering_muc_room_affiliations \
             DROP CONSTRAINT clustering_muc_room_affiliations_value_check",
            (),
        )
        .await
        .expect("drop uppercase CHECK");
        drop(conn);
        store
            .ensure_schema()
            .await
            .expect("restore canonical CHECK after uppercase fixture");

        let conn = db.guard().await.expect("guard");
        conn.execute(
            "ALTER TABLE clustering_muc_room_affiliations \
             DROP CONSTRAINT clustering_muc_room_affiliations_value_check",
            (),
        )
        .await
        .expect("drop canonical CHECK before nullable fixture");
        conn.execute(
            "ALTER TABLE clustering_muc_room_affiliations \
             ADD CONSTRAINT clustering_muc_room_affiliations_value_check \
             CHECK (affiliation = ANY (ARRAY[\
                 'outcast'::text, 'member'::text, 'admin'::text, 'owner'::text, NULL::text\
             ])) NOT VALID",
            (),
        )
        .await
        .expect("install same-named nullable CHECK");
        drop(conn);

        let nullable_error = store
            .ensure_schema()
            .await
            .expect_err("nullable affiliation CHECK must fail startup");
        assert!(
            nullable_error.to_string().contains("unexpected definition"),
            "unexpected error: {nullable_error}"
        );

        let conn = db.guard().await.expect("guard");
        conn.execute(
            "ALTER TABLE clustering_muc_room_affiliations \
             DROP CONSTRAINT clustering_muc_room_affiliations_value_check",
            (),
        )
        .await
        .expect("drop nullable CHECK");
        drop(conn);
        store
            .ensure_schema()
            .await
            .expect("restore canonical CHECK after nullable fixture");

        let conn = db.guard().await.expect("guard");
        conn.execute(
            "ALTER TABLE clustering_muc_room_affiliations \
             DROP CONSTRAINT clustering_muc_room_affiliations_room_fk",
            (),
        )
        .await
        .expect("drop canonical FK");
        conn.execute(
            "ALTER TABLE clustering_muc_room_affiliations \
             ADD CONSTRAINT clustering_muc_room_affiliations_room_fk \
             FOREIGN KEY (room_jid) REFERENCES clustering_muc_rooms (room_jid) \
             ON DELETE RESTRICT NOT VALID",
            (),
        )
        .await
        .expect("install same-named wrong FK");
        drop(conn);

        let fk_error = store
            .ensure_schema()
            .await
            .expect_err("same-named wrong FK must fail startup");
        assert!(
            fk_error.to_string().contains("ON DELETE CASCADE"),
            "unexpected error: {fk_error}"
        );

        let conn = db.guard().await.expect("guard");
        conn.execute(
            "ALTER TABLE clustering_muc_room_affiliations \
             DROP CONSTRAINT clustering_muc_room_affiliations_room_fk",
            (),
        )
        .await
        .expect("drop wrong FK");
        drop(conn);
        store.ensure_schema().await.expect("restore canonical FK");
    }

    #[tokio::test]
    async fn room_delete_removes_legacy_orphans_before_jid_recreation() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, db, me)) = clean_store().await else {
            return;
        };
        let jid = room_jid("legacy-orphan-delete");
        let conn = db.guard().await.expect("guard");
        conn.execute(
            "ALTER TABLE clustering_muc_room_affiliations \
             DROP CONSTRAINT clustering_muc_room_affiliations_room_fk",
            (),
        )
        .await
        .expect("drop FK to seed a legacy orphan");
        conn.execute(
            "INSERT INTO clustering_muc_room_affiliations \
             (room_jid, member_jid, affiliation) VALUES (?, ?, ?)",
            crate::db_params![
                jid.to_string(),
                "legacy-owner@example.com".to_string(),
                "owner".to_string(),
            ],
        )
        .await
        .expect("seed legacy orphan");
        drop(conn);
        store
            .ensure_schema()
            .await
            .expect("install NOT VALID FK over legacy orphan");

        let entity = Entity::new(EntityType::RoomActor, jid.to_string());
        let epoch = claim_store
            .ensure_claimed(&entity, &me)
            .await
            .expect("claim");
        store.record_claim_fence(&jid, RoomClaimFenceContext::new(entity, me, epoch));
        store
            .delete_room_state(&jid)
            .await
            .expect("delete absent parent and legacy child");
        store
            .save_config(&jid, "waddle-1", "chan-1", &RoomConfig::default())
            .await
            .expect("recreate room JID");

        let restored = store
            .load_room_state(&jid)
            .await
            .expect("load recreated room")
            .expect("recreated room exists");
        assert!(
            restored.affiliations.is_empty(),
            "a legacy owner grant must not attach to a recreated room"
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
        store.record_claim_fence(
            &jid,
            RoomClaimFenceContext::new(entity.clone(), me.clone(), epoch),
        );

        let config = RoomConfig {
            name: "test room".to_string(),
            members_only: true,
            ..RoomConfig::default()
        };
        store
            .save_config(&jid, "waddle-1", "chan-1", &config)
            .await
            .expect("save config");

        let subject = SubjectState {
            texts: RoomSubjectTexts::from_iter([(String::new(), "hello".to_string())]),
            setter: "alice@example.com".parse().expect("valid jid"),
            setter_nick: "alice".to_string(),
            set_at: chrono::Utc::now(),
        };
        store
            .save_subject(&jid, Some(&subject))
            .await
            .expect("save subject");

        let bob: BareJid = "bob@example.com".parse().expect("valid jid");
        let entry = AffiliationEntry::new(bob.clone(), Affiliation::Owner);
        store
            .save_affiliation(&jid, &entry)
            .await
            .expect("save affiliation");

        let loaded = store
            .load_room_state(&jid)
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
        store.record_claim_fence(
            &jid,
            RoomClaimFenceContext::new(entity.clone(), me.clone(), epoch),
        );
        store
            .save_config(&jid, "waddle-1", "chan-1", &RoomConfig::default())
            .await
            .expect("save config");

        let carol: BareJid = "carol@example.com".parse().expect("valid jid");
        store
            .save_affiliation(
                &jid,
                &AffiliationEntry::new(carol.clone(), Affiliation::Member),
            )
            .await
            .expect("save member");
        let loaded = store
            .load_room_state(&jid)
            .await
            .expect("load")
            .expect("row exists");
        assert_eq!(loaded.affiliations.len(), 1);

        store
            .save_affiliation(&jid, &AffiliationEntry::new(carol, Affiliation::None))
            .await
            .expect("save none removes the row");
        let loaded = store
            .load_room_state(&jid)
            .await
            .expect("load")
            .expect("row exists");
        assert!(loaded.affiliations.is_empty());
    }

    #[tokio::test]
    async fn affiliation_writes_and_room_delete_share_parent_first_lock_order() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, db, me)) = clean_store().await else {
            return;
        };
        let store = Arc::new(store);

        for (suffix, affiliation) in [
            ("upsert", Affiliation::Admin),
            ("removal", Affiliation::None),
        ] {
            let jid = room_jid(&format!("delete-race-{suffix}"));
            let entity = Entity::new(EntityType::RoomActor, jid.to_string());
            let epoch = claim_store
                .ensure_claimed(&entity, &me)
                .await
                .expect("claim");
            store.record_claim_fence(&jid, RoomClaimFenceContext::new(entity, me.clone(), epoch));
            store
                .save_config(&jid, "waddle-1", "chan-1", &RoomConfig::default())
                .await
                .expect("seed parent room");
            let member: BareJid = "alice@example.com".parse().expect("valid JID");
            store
                .save_affiliation(
                    &jid,
                    &AffiliationEntry::new(member.clone(), Affiliation::Member),
                )
                .await
                .expect("seed child affiliation");

            let entry = AffiliationEntry::new(member, affiliation);
            let barrier = Arc::new(tokio::sync::Barrier::new(3));
            let start = {
                let barrier = Arc::clone(&barrier);
                async move { barrier.wait().await }
            };
            let save = {
                let barrier = Arc::clone(&barrier);
                let store = Arc::clone(&store);
                let jid = jid.clone();
                async move {
                    barrier.wait().await;
                    store.save_affiliation(&jid, &entry).await
                }
            };
            let delete = {
                let barrier = Arc::clone(&barrier);
                let store = Arc::clone(&store);
                let jid = jid.clone();
                async move {
                    barrier.wait().await;
                    store.delete_room_state(&jid).await
                }
            };
            let (_, save_result, delete_result) =
                tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    tokio::join!(start, save, delete)
                })
                .await
                .expect("parent-first operations must not deadlock");
            delete_result.expect("room deletion must complete");
            if let Err(error) = save_result {
                assert!(
                    error.to_string().contains("has no durable config row"),
                    "only a delete-won missing-parent race is acceptable, got: {error}"
                );
            }
            let conn = db.guard().await.expect("guard");
            let mut parent_rows = conn
                .query(
                    "SELECT count(*) FROM clustering_muc_rooms WHERE room_jid = ?",
                    crate::db_params![jid.to_string()],
                )
                .await
                .expect("query parent state");
            let parent_count: i64 = parent_rows
                .next()
                .await
                .expect("read parent count")
                .expect("parent count row")
                .get(0)
                .expect("decode parent count");
            let mut child_rows = conn
                .query(
                    "SELECT count(*) FROM clustering_muc_room_affiliations WHERE room_jid = ?",
                    crate::db_params![jid.to_string()],
                )
                .await
                .expect("query child state");
            let child_count: i64 = child_rows
                .next()
                .await
                .expect("read child count")
                .expect("child count row")
                .get(0)
                .expect("decode child count");
            assert_eq!((parent_count, child_count), (0, 0));
        }
    }

    #[tokio::test]
    async fn affiliation_and_subject_writes_require_a_durable_room_row() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some((store, claim_store, _db, me)) = clean_store().await else {
            return;
        };
        let jid = room_jid("missing-parent");
        let entity = Entity::new(EntityType::RoomActor, jid.to_string());
        let epoch = claim_store
            .ensure_claimed(&entity, &me)
            .await
            .expect("claim");
        store.record_claim_fence(&jid, RoomClaimFenceContext::new(entity, me, epoch));

        let affiliation_error = store
            .save_affiliation(
                &jid,
                &AffiliationEntry::new(
                    "alice@example.com".parse().expect("valid JID"),
                    Affiliation::Member,
                ),
            )
            .await
            .expect_err("affiliation without parent room must be rejected");
        assert!(
            affiliation_error
                .to_string()
                .contains("has no durable config row"),
            "unexpected error: {affiliation_error}"
        );

        let removal_error = store
            .save_affiliation(
                &jid,
                &AffiliationEntry::new(
                    "alice@example.com".parse().expect("valid JID"),
                    Affiliation::None,
                ),
            )
            .await
            .expect_err("affiliation removal without parent room must be rejected");
        assert!(
            removal_error
                .to_string()
                .contains("has no durable config row"),
            "unexpected error: {removal_error}"
        );

        let subject = SubjectState {
            texts: RoomSubjectTexts::from_iter([(String::new(), "hello".to_string())]),
            setter: "alice@example.com".parse().expect("valid JID"),
            setter_nick: "alice".to_string(),
            set_at: chrono::Utc::now(),
        };
        let subject_error = store
            .save_subject(&jid, Some(&subject))
            .await
            .expect_err("subject without parent room must be rejected");
        assert!(
            subject_error
                .to_string()
                .contains("has no durable config row"),
            "unexpected error: {subject_error}"
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
        store.record_claim_fence(
            &jid,
            RoomClaimFenceContext::new(entity.clone(), me.clone(), epoch),
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
    }

    /// ADR-0017 Phase 3 Slice 7 FIX 1 (council-adjudicated): the MAM
    /// fenced-archive-write backstop. Exercises the full path this fix
    /// adds — `current_claim_fence` resolving the same typed context
    /// `check_fenced_fanout` uses, `SqlxMamStorage::store_message_fenced`
    /// running the fencing `SELECT ... FOR SHARE` inside the same
    /// transaction as the archive insert, and the deposed owner's very
    /// next fenced write observing `NotOwner` rather than silently
    /// archiving under a claim it no longer holds.
    #[tokio::test]
    async fn mam_store_message_fenced_blocks_the_deposed_owners_next_archive_write() {
        use waddle_xmpp::mam::{MamStorage, MamStorageError, SqlxMamStorage};
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
        let archive_id = mam_storage
            .store_message_fenced(&jid, &message, &fence)
            .await
            .expect("the current owner's fenced write must succeed");
        assert_eq!(archive_id, first_id);
        let stored = mam_storage
            .get_message(&archive_id)
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
