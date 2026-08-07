//! Postgres-authoritative entity ownership claims (ADR-0017 Phase 3 Slice 1,
//! element 4).
//!
//! Implements `waddle_xmpp::ownership::ClaimStore` — defined upstream in
//! `waddle-xmpp` (Q1: the trait must be unconditionally compiled, since
//! ordinary `UserActor`/`RoomActor`/SM-session code has no `clustering`
//! feature) — for a `waddle-server`-local `PostgresClaimStore`, legal under
//! Rust's orphan rule (local type, foreign trait) without the trait itself
//! needing to live downstream.
//!
//! Unlike `lease.rs`/`allowlist.rs`, this store cannot define its own
//! associated error enum: the `ClaimStore` trait fixes the error type to
//! `waddle_xmpp::ownership::ClaimError`. Every fallible `Database` call is
//! mapped to [`ClaimError::Backend`] at the boundary — a human-facing
//! `Display` conversion, not a structured payload, exactly as that variant's
//! doc comment explains.
//!
//! **Schema** (PROPOSED — no ADR-locked DDL exists for `clustering_nodes`/
//! `clustering_claims`, only the column shapes in element 4):
//! - `clustering_nodes` — one liveness row per replica. Only its DDL lands
//!   in this slice; the heartbeat/expire CAS and demotion reconciliation
//!   that actually populate/renew it are ADR-0017 Phase 3 Slice 2
//!   (`NodeLeaseStore`). Tests in this file seed/mutate rows directly,
//!   playing the future `NodeLeaseStore`'s part.
//! - `clustering_claims` — which node owns a given entity, under which
//!   fencing epoch. The `entity` primary key is not the bare
//!   [`Entity::id`](waddle_xmpp::ownership::Entity), but
//!   [`entity_key`]'s `<entity_type_tag>:<id>` encoding — the type must be
//!   folded into the key itself, not just written to the (otherwise
//!   read-only-in-effect) `entity_type` column, or two distinct entities of
//!   different types sharing the same id would collide on one row.
//!
//! **CAS/fencing SQL** (element 4, quoted verbatim in the Phase 3 plan;
//! these shapes are locked, not improvised): *Acquire* is
//! `INSERT ... ON CONFLICT (entity) DO NOTHING` + `rows_affected == 1`.
//! *Steal (stale owner)* realizes the ADR's "owner-stale LEFT-JOIN
//! predicate" as a `NOT EXISTS` correlated subquery over `clustering_nodes`
//! (Postgres `UPDATE` has no `LEFT JOIN` clause, and `UPDATE ... FROM` is
//! inner-join semantics, which would silently drop the "owner row missing
//! entirely" case) — reading only the **committed** `expired` flag, never a
//! raw `heartbeat < now() - ttl` comparison. *Steal (consent/epoch-only)* is
//! the same CAS with no staleness predicate at all, reachable only via
//! [`waddle_xmpp::ownership::ClaimStore::steal_for_resume`], which requires
//! a `ResumeIdentityProof` no code outside the identity-checked resume path
//! can mint. `StalePredicate::StealIntentExpired` (ADR-0017 Phase 3 Slice 3)
//! substitutes an `EXISTS` probe over `clustering_steal_intents` for the
//! owner-stale predicate — see [`NodeLeaseStore::report_steal_intent`]/
//! [`NodeLeaseStore::owner_steal_intents`]/
//! [`NodeLeaseStore::clear_steal_intent`] for the intent CRUD + owner-veto
//! path. `EntityType::SmSession` is rejected for this variant (the
//! three-rule steal-variant block: steal-intents never touch SM-session
//! claims).
//!
//! All CAS statements run on the dedicated control-plane pool
//! ([`Database::control_plane_guard`], ADR-0017 element 4/12, Slice 0):
//! this per-node/per-entity liveness traffic must never queue behind fenced
//! bulk writes, backstop fencing SELECTs, or claims-read storms on the main
//! pool. `ensure_schema` is one-time startup DDL, not hot-path liveness
//! traffic, and runs on the main pool — mirroring `lease.rs`.

use std::time::Duration;

use async_trait::async_trait;
use waddle_xmpp::ownership::{
    ClaimEpoch, ClaimError, ClaimStore, Entity, EntityType, NodeIdentity, ResumeIdentityProof,
    StalePredicate,
};
use waddle_xmpp::pending_delivery::SmSessionId;

use crate::db::{Database, DatabaseError};

/// Convert a backend database failure into the upstream `ClaimError`. The
/// concrete diagnostic (`DatabaseError`'s `Display`) is preserved as
/// human-facing text; see [`ClaimError::Backend`]'s doc comment for why a
/// richer, `waddle-server`-local error type can't cross this boundary.
fn db_err(error: DatabaseError) -> ClaimError {
    ClaimError::Backend(error.to_string())
}

/// True iff `error` is a Postgres `40P01 deadlock_detected` failure.
///
/// FIX 1(c): `steal_stale`'s `StealIntentExpired` CAS and
/// `clear_steal_intent`'s epoch-fenced veto DELETE deliberately acquire
/// their `clustering_claims`/`clustering_steal_intents` row locks in
/// opposite orders (see both statements' doc comments for the full
/// reasoning) — a documented design choice, not an oversight, because it is
/// exactly what lets the two statements serialize *on the intent rows
/// themselves* rather than racing an unlocked `EXISTS` read. The
/// consequence is that Postgres may occasionally abort one side of a
/// contended pair with `40P01` instead of blocking it. That outcome is
/// always safe (a typed [`ClaimError::Backend`], never a panic; the loser
/// retries on its own next tick/scan) — this helper only distinguishes it
/// so callers can log it at `debug` instead of `warn`/`error`, since it is
/// an expected consequence of the lock-order design, not a fault.
fn is_postgres_deadlock(error: &DatabaseError) -> bool {
    matches!(
        error,
        DatabaseError::Internal(sqlx::Error::Database(inner))
            if inner.code().as_deref() == Some("40P01")
    )
}

/// Log a `debug`-level note when `error` is the expected FIX 1(c) deadlock
/// outcome; a no-op for every other error (those are logged by the normal
/// `tracing::warn!` call sites that already wrap this store's callers).
fn log_if_postgres_deadlock(error: &DatabaseError, entity_key: &str, statement: &'static str) {
    if is_postgres_deadlock(error) {
        tracing::debug!(
            entity_key = %entity_key,
            statement,
            "clustering claim CAS: deadlock detected against the opposite-lock-order \
             counterpart statement (FIX 1(c), expected under contention); retrying next \
             tick/scan is the correct response"
        );
    }
}

/// Injective `(entity_type, id) -> TEXT` encoding for the
/// `clustering_claims.entity` primary key (fix: the PK previously bound
/// `entity.id` alone, so `(UserActor, "42")` and `(RoomActor, "42")` — two
/// genuinely distinct claimable entities — collided on the same row;
/// `entity_type` was write-only, never folded into the key that actually
/// disambiguates rows). Every statement that binds the `entity` column
/// MUST go through this function, never `entity.id` directly.
///
/// The tag set (`EntityType::as_db_str`) is closed and pairwise
/// prefix-free — `"user_actor"`, `"room_actor"`, `"sm_session"` each start
/// with a distinct first character, so no tag is a prefix of another —
/// which keeps the `tag:id` encoding unambiguous (each key decomposes into
/// exactly one `(tag, id)` pair by splitting on the first `:`) even though
/// `id` itself is attacker/caller-controlled and may contain `:` — nothing
/// in this slice decodes the key back, but a future slice safely could.
fn entity_key(entity: &Entity) -> String {
    format!("{}:{}", entity.entity_type.as_db_str(), entity.id)
}

/// The inverse of [`entity_key`]: reconstruct an [`Entity`] from an encoded
/// `clustering_claims.entity` key plus its separately-stored, already-typed
/// `entity_type` column. Slice 1's doc comment noted that nothing in that
/// slice decoded the key back — [`NodeLeaseStore::owner_steal_intents`]
/// (Slice 3) is the first caller that needs to, since it returns typed
/// [`Entity`] values to callers outside this module. Stripping the known
/// `"{tag}:"` prefix (rather than a generic first-`:`-split decode) is safe
/// even when `id` itself contains further colons, because the caller
/// already knows the exact tag for each row from the `entity_type` column.
///
/// **FIX 6**: returns `None` on a prefix mismatch instead of silently
/// falling back to the raw encoded string as the id (`unwrap_or(encoded)`)
/// — a row whose `entity` key does not actually start with
/// `entity_type`'s tag (a data-integrity anomaly: `entity_type` and the
/// key's own tag prefix disagreeing about what type this row is) must
/// never be allowed to mangle an id silently; the caller decides whether
/// to skip and log the row (see
/// [`NodeLeaseStore::owner_steal_intents`]'s handling).
fn decode_entity(encoded: &str, entity_type: EntityType) -> Option<Entity> {
    let prefix = format!("{}:", entity_type.as_db_str());
    let id = encoded.strip_prefix(&prefix)?;
    if id.len() > waddle_xmpp::ownership::ENTITY_ID_MAX_LEN {
        return None;
    }
    Some(Entity::new(entity_type, id))
}

/// Decode a persisted SM claim and enforce the SM protocol boundary's
/// narrower wire limit. [`Entity`] deliberately permits longer ids for room
/// JIDs and other entity kinds, so `decode_entity` alone cannot establish
/// that a database value is a valid [`SmSessionId`].
fn decode_sm_session_entity(encoded: &str) -> Option<Entity> {
    let entity = decode_entity(encoded, EntityType::SmSession)?;
    SmSessionId::try_from_wire(entity.id.clone()).ok()?;
    Some(entity)
}

#[cfg(test)]
mod entity_key_tests {
    use super::{decode_sm_session_entity, entity_key};
    use waddle_xmpp::ownership::{Entity, EntityType};

    /// Pure unit test (no Postgres required): `entity_key` must be
    /// injective even across entity types sharing the same `id`, and even
    /// when `id` itself contains the `:` separator or looks like one of
    /// the tag strings — the whole point of prefixing with a
    /// pairwise-prefix-free tag set.
    #[test]
    fn entity_key_is_injective_even_with_colons_in_ids() {
        let cases = [
            Entity::new(EntityType::UserActor, "42"),
            Entity::new(EntityType::RoomActor, "42"),
            Entity::new(EntityType::SmSession, "42"),
            Entity::new(EntityType::UserActor, "room_actor:42"),
            Entity::new(EntityType::UserActor, "user_actor:99"),
            Entity::new(EntityType::RoomActor, "user_actor:42"),
            Entity::new(EntityType::SmSession, "sm_session:sm_session:x"),
            Entity::new(EntityType::UserActor, ""),
            Entity::new(EntityType::RoomActor, ""),
            Entity::new(EntityType::UserActor, ":"),
            Entity::new(EntityType::RoomActor, ":"),
        ];
        let mut keys = std::collections::HashSet::new();
        for entity in &cases {
            let key = entity_key(entity);
            assert!(
                keys.insert(key.clone()),
                "entity_key collision for {entity:?}: key {key:?} already produced by \
                 a different (entity_type, id) pair"
            );
        }
    }

    #[test]
    fn decode_entity_round_trips_entity_key_including_ids_with_colons() {
        let long_valid_room: jid::BareJid = format!("{}@muc.example.com", "a".repeat(129))
            .parse()
            .expect("valid room JID longer than the old 128-byte wire bound");
        let cases = vec![
            Entity::new(EntityType::UserActor, "42"),
            Entity::new(EntityType::RoomActor, "room_actor:42"),
            Entity::new(EntityType::SmSession, "sm_session:sm_session:x"),
            Entity::new(EntityType::UserActor, ""),
            Entity::new(EntityType::RoomActor, ":"),
            Entity::new(EntityType::RoomActor, long_valid_room.to_string()),
        ];
        for entity in cases {
            let encoded = entity_key(&entity);
            let decoded = super::decode_entity(&encoded, entity.entity_type)
                .expect("decode_entity must succeed for a key it encoded itself");
            assert_eq!(decoded, entity, "decode_entity must invert entity_key");
        }
    }

    /// FIX 6: a row whose encoded key does not actually start with the
    /// claimed `entity_type`'s tag prefix (a data-integrity anomaly, not a
    /// shape `entity_key` itself ever produces) must be rejected rather
    /// than silently decoded with the whole encoded string mangled in as
    /// the id.
    #[test]
    fn decode_entity_returns_none_on_a_mismatched_prefix() {
        let encoded = entity_key(&Entity::new(EntityType::UserActor, "42"));
        assert_eq!(super::decode_entity(&encoded, EntityType::RoomActor), None);
        assert_eq!(super::decode_entity(&encoded, EntityType::SmSession), None);
        assert_eq!(
            super::decode_entity("not-even-tagged", EntityType::UserActor),
            None
        );
    }

    #[test]
    fn sm_session_decode_enforces_the_sm_specific_wire_bound() {
        let valid = Entity::new(
            EntityType::SmSession,
            "s".repeat(waddle_xmpp::pending_delivery::SM_SESSION_ID_MAX_LEN),
        );
        assert_eq!(decode_sm_session_entity(&entity_key(&valid)), Some(valid));

        let overlong = Entity::new(
            EntityType::SmSession,
            "s".repeat(waddle_xmpp::pending_delivery::SM_SESSION_ID_MAX_LEN + 1),
        );
        assert_eq!(decode_sm_session_entity(&entity_key(&overlong)), None);
    }
}

/// Postgres implementation of `ClaimStore`, backing `UserActor`/`RoomActor`/
/// SM-session ownership.
pub struct PostgresClaimStore {
    db: Database,
}

struct OrphanedSmClaimCleanup {
    encoded: String,
    owner: NodeIdentity,
    claim_epoch: ClaimEpoch,
    durable_stream_id: Option<String>,
    malformed: bool,
}

impl PostgresClaimStore {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    async fn load_sm_orphan_reaper_cursor(&self) -> Result<Option<SmOrphanScanCursor>, ClaimError> {
        let conn = self.db.control_plane_guard().await.map_err(db_err)?;
        let mut rows = conn
            .query(
                "SELECT cursor_entity FROM clustering_orphan_reaper_cursors WHERE lane = ?",
                crate::db_params![EntityType::SmSession.as_db_str().to_string()],
            )
            .await
            .map_err(db_err)?;
        match rows.next().await.map_err(db_err)? {
            Some(row) => row
                .get::<Option<String>>(0)
                .map(|cursor| cursor.map(SmOrphanScanCursor::from_raw))
                .map_err(db_err),
            None => Ok(None),
        }
    }

    async fn load_room_orphan_reaper_cursor(
        &self,
    ) -> Result<Option<RoomOrphanScanCursor>, ClaimError> {
        let conn = self.db.control_plane_guard().await.map_err(db_err)?;
        let mut rows = conn
            .query(
                "SELECT cursor_entity FROM clustering_orphan_reaper_cursors WHERE lane = ?",
                crate::db_params![EntityType::RoomActor.as_db_str().to_string()],
            )
            .await
            .map_err(db_err)?;
        match rows.next().await.map_err(db_err)? {
            Some(row) => row
                .get::<Option<String>>(0)
                .map(|cursor| cursor.map(RoomOrphanScanCursor::from_raw))
                .map_err(db_err),
            None => Ok(None),
        }
    }

    /// Remove one exact stale SM claim discovered by the advisory orphan
    /// scan. A claim classified as claim-only is deleted only if its durable
    /// row is still absent in this transaction. That current-state predicate
    /// closes the scan/delete window in which a paused owner can finish a
    /// fenced detach write before this DELETE obtains the claim-row lock.
    async fn cleanup_orphaned_sm_claim(
        &self,
        cleanup: OrphanedSmClaimCleanup,
    ) -> Result<bool, ClaimError> {
        let mut tx = self.db.begin().await.map_err(db_err)?;
        // Serialize with every fenced SM writer before evaluating durable-row
        // absence. Under Postgres READ COMMITTED, putting `NOT EXISTS` in the
        // same DELETE that waits on a writer's `FOR SHARE` lock is not enough:
        // that statement's snapshot can predate the writer's commit. Taking
        // the exact claim lock in this first statement makes the DELETE below
        // start with a fresh statement snapshot after any in-flight writer has
        // committed or rolled back.
        let mut locked = tx
            .query(
                r#"
                /* orphan_sm_cleanup_exact_lock */
                SELECT 1 FROM clustering_claims
                WHERE entity = ? AND entity_type = ? AND node_id = ? AND node_epoch = ?
                  AND claim_epoch = ?
                FOR UPDATE
                "#,
                crate::db_params![
                    cleanup.encoded.clone(),
                    EntityType::SmSession.as_db_str().to_string(),
                    cleanup.owner.node_id.clone(),
                    cleanup.owner.node_epoch.clone(),
                    cleanup.claim_epoch.0,
                ],
            )
            .await
            .map_err(db_err)?;
        let exact_claim_locked = locked.next().await.map_err(db_err)?.is_some();
        drop(locked);
        if !exact_claim_locked {
            tx.commit().await.map_err(db_err)?;
            return Ok(false);
        }
        let params = crate::db_params![
            cleanup.encoded.clone(),
            EntityType::SmSession.as_db_str().to_string(),
            cleanup.owner.node_id,
            cleanup.owner.node_epoch,
            cleanup.claim_epoch.0,
        ];
        let affected = if cleanup.durable_stream_id.is_some() {
            tx.execute(
                r#"
                DELETE FROM clustering_claims
                WHERE entity = ? AND entity_type = ? AND node_id = ? AND node_epoch = ?
                  AND claim_epoch = ?
                  AND NOT EXISTS (
                    SELECT 1 FROM clustering_nodes n
                    WHERE n.node_id = clustering_claims.node_id
                      AND NOT n.expired
                      AND n.node_epoch = clustering_claims.node_epoch
                  )
                "#,
                params,
            )
            .await
            .map_err(db_err)?
        } else {
            tx.execute(
                r#"
                DELETE FROM clustering_claims
                WHERE entity = ? AND entity_type = ? AND node_id = ? AND node_epoch = ?
                  AND claim_epoch = ?
                  AND NOT EXISTS (
                    SELECT 1 FROM clustering_nodes n
                    WHERE n.node_id = clustering_claims.node_id
                      AND NOT n.expired
                      AND n.node_epoch = clustering_claims.node_epoch
                  )
                  AND NOT EXISTS (
                    SELECT 1 FROM sm_sessions s
                    WHERE clustering_claims.entity = ('sm_session:' || s.stream_id)
                  )
                "#,
                params,
            )
            .await
            .map_err(db_err)?
        };
        if affected == 1 {
            if let Some(stream_id) = cleanup.durable_stream_id {
                tx.execute(
                    "DELETE FROM sm_unacked WHERE stream_id = ?",
                    crate::db_params![stream_id.clone()],
                )
                .await
                .map_err(db_err)?;
                tx.execute(
                    "DELETE FROM sm_sessions WHERE stream_id = ?",
                    crate::db_params![stream_id],
                )
                .await
                .map_err(db_err)?;
            }
        }
        tx.commit().await.map_err(db_err)?;
        Ok(affected == 1 && cleanup.malformed)
    }
}

#[async_trait]
impl ClaimStore for PostgresClaimStore {
    async fn ensure_schema(&self) -> Result<(), ClaimError> {
        let conn = self.db.guard().await.map_err(db_err)?;
        conn.execute(
            r#"
            CREATE TABLE IF NOT EXISTS clustering_nodes (
                node_id           TEXT PRIMARY KEY,
                node_epoch        TEXT NOT NULL,
                heartbeat         TIMESTAMPTZ NOT NULL DEFAULT now(),
                expired           BOOLEAN NOT NULL DEFAULT FALSE,
                pod_template_hash TEXT,
                -- ADR-0017 Phase 3 Slice 2: set by `NodeLeaseStore::mark_draining`
                -- (Slice 10 consumes it — stop acquiring new claims, keep
                -- serving owned ones). No production deployments predate this
                -- column (pre-launch project), so it is added directly to the
                -- `CREATE TABLE IF NOT EXISTS` rather than a separate `ALTER
                -- TABLE` migration.
                draining          BOOLEAN NOT NULL DEFAULT FALSE,
                -- ADR-0017 Phase 4: libp2p PeerId bound to this exact
                -- node_id/node_epoch registration. Ordered relay validates
                -- signed origin envelopes against this value before applying
                -- delivery effects.
                peer_id           TEXT,
                -- ADR-0017 Phase 3 Slice 10 (Q5's operational definition of
                -- "the current deployment generation" — realized here):
                -- stamped once, at this row's FIRST registration, and never
                -- refreshed by a later re-registration under the SAME
                -- `node_id` (`register`'s upsert explicitly preserves it —
                -- see that method). "The current generation" for the
                -- rollout-aware acquire-backoff heuristic is the
                -- `pod_template_hash` of the row with the greatest
                -- `first_seen` among non-expired rows
                -- (`current_generation`, below): a re-registration after a
                -- self-fence must NOT make an old-generation pod look newer
                -- than a genuinely newer-generation pod that registered
                -- later, or the backoff heuristic would misclassify who
                -- "tries first" during a rollout (never who wins — the CAS
                -- remains authoritative either way, per Q5).
                first_seen        TIMESTAMPTZ NOT NULL DEFAULT now()
            )
            "#,
            (),
        )
        .await
        .map_err(db_err)?;
        conn.execute(
            "ALTER TABLE clustering_nodes ADD COLUMN IF NOT EXISTS peer_id TEXT",
            (),
        )
        .await
        .map_err(db_err)?;
        // Claim generations must never repeat after a row is deleted and
        // recreated. Bootstrap the sequence under a transaction-scoped
        // advisory lock so concurrent startup migrations cannot race their
        // `setval`, then lock the claims table while advancing the sequence
        // past every generation already present in a pre-sequence schema.
        // The table lock also excludes concurrent INSERT/UPDATE statements
        // until the new default and sequence floor commit together.
        let mut tx = self.db.begin().await.map_err(db_err)?;
        tx.query("SELECT pg_advisory_xact_lock(6841445497037937991)", ())
            .await
            .map_err(db_err)?;
        tx.execute(
            r#"
            CREATE SEQUENCE IF NOT EXISTS clustering_claim_epoch_seq
                AS BIGINT MINVALUE 0 START WITH 1
            "#,
            (),
        )
        .await
        .map_err(db_err)?;
        tx.execute(
            r#"
            CREATE TABLE IF NOT EXISTS clustering_claims (
                entity      TEXT PRIMARY KEY,
                entity_type TEXT NOT NULL,
                node_id     TEXT NOT NULL,
                node_epoch  TEXT NOT NULL,
                claim_epoch BIGINT NOT NULL
                    DEFAULT nextval('clustering_claim_epoch_seq'::regclass)
            )
            "#,
            (),
        )
        .await
        .map_err(db_err)?;
        tx.execute("LOCK TABLE clustering_claims IN ACCESS EXCLUSIVE MODE", ())
            .await
            .map_err(db_err)?;
        tx.execute(
            r#"
            ALTER TABLE clustering_claims
            ALTER COLUMN claim_epoch
            SET DEFAULT nextval('clustering_claim_epoch_seq'::regclass)
            "#,
            (),
        )
        .await
        .map_err(db_err)?;
        tx.query(
            r#"
            SELECT setval(
                'clustering_claim_epoch_seq',
                GREATEST(
                    (SELECT COALESCE(MAX(claim_epoch), 0) FROM clustering_claims),
                    (SELECT last_value FROM clustering_claim_epoch_seq)
                ),
                true
            )
            "#,
            (),
        )
        .await
        .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        conn.execute(
            r#"
            CREATE INDEX IF NOT EXISTS clustering_claims_node_id_node_epoch
                ON clustering_claims (node_id, node_epoch)
            "#,
            (),
        )
        .await
        .map_err(db_err)?;
        conn.execute(
            r#"
            CREATE INDEX IF NOT EXISTS clustering_claims_owner_type_entity
                ON clustering_claims (node_id, node_epoch, entity_type, entity)
            "#,
            (),
        )
        .await
        .map_err(db_err)?;
        // Supports bounded per-entity-type orphan scans without walking the
        // much larger mixed claim table (SM sessions dominate in modeled
        // deployments). `entity` also satisfies the room scan's stable
        // ordering before its LIMIT.
        conn.execute(
            r#"
            CREATE INDEX IF NOT EXISTS clustering_claims_entity_type_entity
                ON clustering_claims (entity_type, entity)
            "#,
            (),
        )
        .await
        .map_err(db_err)?;
        conn.execute(
            r#"
            CREATE TABLE IF NOT EXISTS clustering_orphan_reaper_cursors (
                lane          TEXT PRIMARY KEY,
                cursor_entity TEXT
            )
            "#,
            (),
        )
        .await
        .map_err(db_err)?;
        for lane in [EntityType::SmSession, EntityType::RoomActor] {
            conn.execute(
                "INSERT INTO clustering_orphan_reaper_cursors (lane) VALUES (?) \
                 ON CONFLICT (lane) DO NOTHING",
                crate::db_params![lane.as_db_str().to_string()],
            )
            .await
            .map_err(db_err)?;
        }
        // ADR-0017 Phase 3 Slice 3: steal-intents unwedge/owner-veto path
        // (element 4's "Unwedge" text, quoted verbatim in the phase plan).
        // `UNIQUE (entity, reporter_node)` + the upsert in
        // `report_steal_intent` collapses repeated failures from one
        // reporter against one entity into a single refreshed row rather
        // than growing unbounded during a sustained relay fault.
        conn.execute(
            r#"
            CREATE TABLE IF NOT EXISTS clustering_steal_intents (
                entity        TEXT NOT NULL,
                reporter_node TEXT NOT NULL,
                created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
                UNIQUE (entity, reporter_node)
            )
            "#,
            (),
        )
        .await
        .map_err(db_err)?;
        // Backs both the steal CAS's per-entity `EXISTS` probe (hot path)
        // and `owner_steal_intents`'s per-owner read. The abandoned-intent
        // sweep (a future orphan-reaper concern, Slice 5) is deliberately
        // NOT served by this index — it is keyed by bare `created_at` with
        // no entity filter, a full-table scan on a table that stays small
        // by construction (minor fix 20 in the phase plan).
        conn.execute(
            r#"
            CREATE INDEX IF NOT EXISTS clustering_steal_intents_entity_created_at
                ON clustering_steal_intents (entity, created_at)
            "#,
            (),
        )
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn acquire(&self, entity: &Entity, me: &NodeIdentity) -> Result<ClaimEpoch, ClaimError> {
        if !me.is_active() {
            return Err(ClaimError::AuthorityDisabled);
        }
        let conn = self.db.control_plane_guard().await.map_err(db_err)?;
        // Acquire CAS (element 4): a fresh claim only inserts; a
        // still-live claim on the same entity leaves the row untouched and
        // affects zero rows.
        //
        // ADR-0017 Phase 3 Slice 10: the `INSERT ... SELECT ... WHERE NOT
        // EXISTS (draining)` guard makes "a draining node never acquires a
        // NEW claim" atomic with the CAS itself — never a separate
        // check-then-act read, which would leave a TOCTOU window between
        // observing "not draining" and the INSERT actually landing. A
        // draining node's own `mark_draining` UPDATE (issued once, at the
        // start of its shutdown drain sequence) is a single autocommit
        // statement on the same control-plane pool, so it is visible to
        // every subsequent `acquire`/`steal_stale` call under ordinary
        // READ COMMITTED semantics by the time this node's drain loop
        // itself proceeds to iterate owned entities.
        let mut inserted = conn
            .query(
                r#"
                INSERT INTO clustering_claims (entity, entity_type, node_id, node_epoch, claim_epoch)
                SELECT ?, ?, ?, ?, nextval('clustering_claim_epoch_seq'::regclass)
                WHERE NOT EXISTS (
                    SELECT 1 FROM clustering_nodes
                    WHERE node_id = ? AND node_epoch = ? AND draining
                )
                ON CONFLICT (entity) DO NOTHING
                RETURNING claim_epoch
                "#,
                crate::db_params![
                    entity_key(entity),
                    entity.entity_type.as_db_str().to_string(),
                    me.node_id.clone(),
                    me.node_epoch.clone(),
                    me.node_id.clone(),
                    me.node_epoch.clone(),
                ],
            )
            .await
            .map_err(db_err)?;
        if let Some(row) = inserted.next().await.map_err(db_err)? {
            return Ok(ClaimEpoch(row.get::<i64>(0).map_err(db_err)?));
        }
        // Zero rows affected: either a genuine conflict (someone already
        // holds this entity) or this node's own draining gate blocked the
        // INSERT before it ever reached `ON CONFLICT`. Distinguish with one
        // follow-up read — only ever taken on this already-cold "lost the
        // race" path, never the common uncontended-acquire case above.
        let mut rows = conn
            .query(
                r#"
                SELECT
                    EXISTS (SELECT 1 FROM clustering_claims WHERE entity = ?) AS claimed,
                    COALESCE(
                        (SELECT draining FROM clustering_nodes WHERE node_id = ? AND node_epoch = ?),
                        false
                    ) AS draining
                "#,
                crate::db_params![entity_key(entity), me.node_id.clone(), me.node_epoch.clone()],
            )
            .await
            .map_err(db_err)?;
        match rows.next().await.map_err(db_err)? {
            Some(row) => {
                let claimed: bool = row.get(0).map_err(db_err)?;
                let draining: bool = row.get(1).map_err(db_err)?;
                if claimed {
                    Err(ClaimError::AlreadyClaimed)
                } else if draining {
                    Err(ClaimError::Draining)
                } else {
                    // A genuine race: the entity was momentarily claimed by
                    // someone else and released again between the INSERT
                    // and this read. Conservative fallback — the caller
                    // retries.
                    Err(ClaimError::AlreadyClaimed)
                }
            }
            None => Err(ClaimError::AlreadyClaimed),
        }
    }

    async fn ensure_claimed(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        if !me.is_active() {
            return Err(ClaimError::AuthorityDisabled);
        }
        // FIX 1: try the real CAS first — the common, uncontended case (a
        // brand-new entity, or a genuine conflict with another node) needs
        // nothing beyond `acquire` itself.
        //
        // ADR-0017 Phase 3 Slice 10: `acquire` can now also fail with
        // `ClaimError::Draining` (this node refused a NEW claim while
        // marked draining). The self-reacquire fallback below still MUST
        // run in that case too — "already-owned entities keep being
        // served" while draining (element 4's drain sequence) — so both
        // errors take the same fallback path; only the *fallback's own*
        // outcome decides which error (if any) ultimately surfaces.
        match self.acquire(entity, me).await {
            Ok(epoch) => Ok(epoch),
            Err(err @ (ClaimError::AlreadyClaimed | ClaimError::Draining)) => {
                // `acquire` lost the CAS: read the row it lost to. This read
                // is deliberately **unlocked** (no `FOR SHARE`) — safe
                // because it never authorizes a write itself; the
                // authoritative gate over any actual write stays the
                // per-write `FOR SHARE` fence inside that write's own
                // transaction (`sm_persistence_fenced::assert_fenced`). This
                // method only decides which `ClaimEpoch` value the caller
                // should cache to bind into that later fence.
                let conn = self.db.control_plane_guard().await.map_err(db_err)?;
                let mut rows = conn
                    .query(
                        "SELECT node_id, node_epoch, claim_epoch FROM clustering_claims WHERE entity = ?",
                        crate::db_params![entity_key(entity)],
                    )
                    .await
                    .map_err(db_err)?;
                match rows.next().await.map_err(db_err)? {
                    Some(row) => {
                        let node_id: String = row.get(0).map_err(db_err)?;
                        let node_epoch: String = row.get(1).map_err(db_err)?;
                        let claim_epoch: i64 = row.get(2).map_err(db_err)?;
                        if node_id == me.node_id && node_epoch == me.node_epoch {
                            // Self-reacquire: this exact node/epoch already
                            // holds the claim (either the losing side of a
                            // concurrent first-write race against itself, or
                            // a later slice's `<enable/>`-time acquire under
                            // this same identity) — idempotent, not a
                            // conflict.
                            Ok(ClaimEpoch(claim_epoch))
                        } else {
                            Err(ClaimError::AlreadyClaimed)
                        }
                    }
                    // No row on file at all: cannot confirm self-ownership.
                    // Propagate `acquire`'s own original error faithfully —
                    // `ClaimError::Draining` when this node's own draining
                    // gate is what blocked the INSERT (there is nothing to
                    // self-reacquire), or `ClaimError::AlreadyClaimed` for
                    // the momentary-race case `acquire` itself already
                    // distinguished (the row vanished between the failed
                    // INSERT and this read — released concurrently).
                    None => Err(err),
                }
            }
            Err(other) => Err(other),
        }
    }

    async fn steal_stale(
        &self,
        entity: &Entity,
        observed: ClaimEpoch,
        staleness: StalePredicate,
        me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        if !me.is_active() {
            return Err(ClaimError::AuthorityDisabled);
        }
        // FIX 5: an exhaustive `match`, not an `if let ... else` fallthrough
        // — so the compiler forces this function to be revisited the
        // moment a third `StalePredicate` variant is added, rather than
        // silently routing an unrecognised future variant into the
        // `OwnerStale` arm below.
        match staleness {
            StalePredicate::StealIntentExpired { intent_ttl } => {
                // Rule 1 of the three-rule steal-variant block (ADR-0017
                // Phase 3 Slice 3): steal-intents never touch SM-session
                // claims. Only `UserActor`/`RoomActor` claims accumulate
                // `steal_intents` rows or get stolen through them —
                // enforced here defensively (in addition to
                // `report_steal_intent` never letting a `SmSession` row
                // exist in the first place), so this CAS itself can never
                // be misused to displace an SM-session owner even if a row
                // somehow existed.
                if entity.entity_type == EntityType::SmSession {
                    return Err(ClaimError::SmSessionExcludedFromStealIntent);
                }
                let conn = self.db.control_plane_guard().await.map_err(db_err)?;
                // FIX 1(a) — council-adjudicated redesign closing the veto
                // race (write skew under READ COMMITTED between this CAS
                // and `clear_steal_intent`'s DELETE, both previously gated
                // on an *unlocked* `EXISTS` read over the other table): a
                // single data-modifying CTE that **consumes** the
                // authorizing intents. The `DELETE` runs first (as the
                // WITH-clause's data-modifying CTE), row-locking and
                // removing every steal-intent row for this entity aged
                // past `intent_ttl`, *before* the outer `UPDATE` even
                // evaluates its own `WHERE` clause. A concurrent
                // `clear_steal_intent` racing to delete those exact same
                // rows under the owner's live epoch therefore serializes
                // on the intent rows themselves: whichever transaction's
                // `DELETE` commits first physically removes the rows, and
                // the loser — blocked on the row lock, then unblocked by
                // the winner's commit — has its own predicate re-evaluated
                // against the now-committed, already-emptied state
                // (Postgres's EvalPlanQual re-check), so it finds nothing
                // left to act on. A healthy owner's veto and a stealer's
                // claim can therefore never both succeed against the same
                // intent rows.
                //
                // This closes the instant-re-steal hole for free: a
                // successful steal deletes exactly the intents that
                // authorized it, so the new owner starts under a clean
                // slate — no leftover aged intent row to immediately
                // re-trigger another steal — with the full `intent_ttl`
                // window of protection any freshly-acquired claim gets.
                //
                // FIX 1(c) — DEADLOCK NOTE: this statement acquires locks
                // in the order {`clustering_steal_intents` rows (the
                // DELETE), then the `clustering_claims` row (the UPDATE)}.
                // `clear_steal_intent` below acquires them in the *opposite*
                // order ({`clustering_claims` row via `FOR SHARE`, then
                // `clustering_steal_intents` rows via DELETE}) — a
                // deliberate, documented lock-order inversion, not an
                // oversight. Under sustained contention Postgres may abort
                // either statement with a `40P01 deadlock_detected` error;
                // this is SAFE and expected: it surfaces as an ordinary
                // typed [`ClaimError::Backend`] (never a panic), logged at
                // `debug` (see [`is_postgres_deadlock`]), and the loser
                // simply retries on its own next tick/scan.
                // The DELETE is additionally gated on the caller's observed
                // epoch still being current (an unlocked read — the
                // authoritative gate remains the epoch-CAS on the UPDATE):
                // a data-modifying CTE runs to completion even when the
                // outer UPDATE matches nothing, so without this gate a
                // caller holding an already-stale epoch would burn the
                // expired intents a concurrent, correctly-epoched stealer
                // needed, delaying the unwedge by one intent_ttl cycle.
                // With it, a stale-epoch caller's CTE deletes nothing. The
                // residual window (epoch bumped by a third writer between
                // the CTE's read and the UPDATE's lock) burns intents
                // without a steal — bounded, self-healing via the
                // reporter's next threshold crossing.
                //
                // ADR-0017 Phase 3 Slice 10: `AND NOT EXISTS (... draining
                // ...)` on the outer UPDATE stops a draining node from
                // winning a NEW claim via the steal-intent path too — a
                // steal is, from this node's perspective, exactly the kind
                // of new acquisition `mark_draining` exists to refuse. Zero
                // rows affected here already means `ClaimError::Conflict`
                // under the pre-existing contract (stale epoch, predicate
                // unsatisfied, or claim gone) — draining is simply another
                // reason folded into that same catch-all, not a new error
                // variant.
                //
                // ADR-0017 Phase 4 Slice 1a hardening: the stealer row must
                // also exist under the same node epoch and be non-expired. A
                // missing, expired, or draining local lease must not be able
                // to acquire a new claim merely because it won this CAS after
                // another node's watchdog expired it.
                let mut rows = conn
                    .query(
                        r#"
                        WITH live_stealer AS (
                            SELECT 1 FROM clustering_nodes
                            WHERE node_id = ?
                              AND node_epoch = ?
                              AND NOT expired
                              AND NOT draining
                        ),
                        consumed AS (
                            DELETE FROM clustering_steal_intents
                            WHERE entity = ?
                              AND created_at < now() - (? || ' milliseconds')::interval
                              AND EXISTS (SELECT 1 FROM live_stealer)
                              AND EXISTS (
                                  SELECT 1 FROM clustering_claims
                                  WHERE entity = ? AND claim_epoch = ?
                              )
                            RETURNING 1
                        )
                        UPDATE clustering_claims
                        SET node_id = ?, node_epoch = ?,
                            claim_epoch = nextval('clustering_claim_epoch_seq'::regclass)
                        WHERE entity = ?
                          AND claim_epoch = ?
                          AND EXISTS (SELECT 1 FROM consumed)
                          AND EXISTS (SELECT 1 FROM live_stealer)
                        RETURNING claim_epoch
                        "#,
                        crate::db_params![
                            me.node_id.clone(),
                            me.node_epoch.clone(),
                            entity_key(entity),
                            intent_ttl.as_millis().to_string(),
                            entity_key(entity),
                            observed.0,
                            me.node_id.clone(),
                            me.node_epoch.clone(),
                            entity_key(entity),
                            observed.0,
                        ],
                    )
                    .await
                    .map_err(|error| {
                        log_if_postgres_deadlock(
                            &error,
                            &entity_key(entity),
                            "steal_stale(StealIntentExpired)",
                        );
                        db_err(error)
                    })?;
                match rows.next().await.map_err(db_err)? {
                    Some(row) => Ok(ClaimEpoch(row.get::<i64>(0).map_err(db_err)?)),
                    None => Err(ClaimError::Conflict),
                }
            }
            StalePredicate::OwnerStale => {
                let conn = self.db.control_plane_guard().await.map_err(db_err)?;
                // Owner-stale steal CAS (element 4): the `NOT EXISTS`
                // correlated subquery realizes the ADR's "LEFT JOIN"
                // predicate (`nodes.node_id IS NULL OR nodes.expired OR
                // node_epoch mismatch`) — a claim is stale iff no
                // `clustering_nodes` row matches its current owner under a
                // fresh (non-expired), current epoch. Reads only the
                // committed `expired` flag, never a raw heartbeat
                // comparison.
                //
                // ADR-0017 Phase 3 Slice 10: the second `NOT EXISTS`
                // refuses the steal when the STEALER (`me`) is itself
                // marked draining — a draining node must not win a dead
                // node's orphaned claim either; that is still a NEW
                // acquisition from this node's point of view. Zero rows
                // affected already means `ClaimError::Conflict` under this
                // method's pre-existing catch-all contract.
                //
                // ADR-0017 Phase 4 Slice 1a hardening: this is now a positive
                // live-stealer predicate, not just "not draining". A node
                // whose own row is missing or committed expired is not allowed
                // to win orphaned claims under its dead identity.
                let mut rows = conn
                    .query(
                        r#"
                        UPDATE clustering_claims
                        SET node_id = ?, node_epoch = ?,
                            claim_epoch = nextval('clustering_claim_epoch_seq'::regclass)
                        WHERE entity = ?
                          AND claim_epoch = ?
                          AND NOT EXISTS (
                            SELECT 1 FROM clustering_nodes n
                            WHERE n.node_id = clustering_claims.node_id
                              AND NOT n.expired
                              AND n.node_epoch = clustering_claims.node_epoch
                          )
                          AND EXISTS (
                            SELECT 1 FROM clustering_nodes
                            WHERE node_id = ?
                              AND node_epoch = ?
                              AND NOT expired
                              AND NOT draining
                          )
                        RETURNING claim_epoch
                        "#,
                        crate::db_params![
                            me.node_id.clone(),
                            me.node_epoch.clone(),
                            entity_key(entity),
                            observed.0,
                            me.node_id.clone(),
                            me.node_epoch.clone(),
                        ],
                    )
                    .await
                    .map_err(db_err)?;
                match rows.next().await.map_err(db_err)? {
                    Some(row) => Ok(ClaimEpoch(row.get::<i64>(0).map_err(db_err)?)),
                    None => Err(ClaimError::Conflict),
                }
            }
        }
    }

    async fn steal_for_resume(
        &self,
        entity: &Entity,
        observed: ClaimEpoch,
        _witness: ResumeIdentityProof,
        me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        if !me.is_active() {
            return Err(ClaimError::AuthorityDisabled);
        }
        let conn = self.db.control_plane_guard().await.map_err(db_err)?;
        // Consent/epoch-only steal CAS (element 4's third variant): no
        // staleness predicate at all — authorized exclusively by the
        // caller already holding a `ResumeIdentityProof`, which only
        // `ownership::resume::verify_resume_identity` can mint.
        let mut rows = conn
            .query(
                r#"
                UPDATE clustering_claims
                SET node_id = ?, node_epoch = ?,
                    claim_epoch = nextval('clustering_claim_epoch_seq'::regclass)
                WHERE entity = ? AND claim_epoch = ?
                RETURNING claim_epoch
                "#,
                crate::db_params![
                    me.node_id.clone(),
                    me.node_epoch.clone(),
                    entity_key(entity),
                    observed.0,
                ],
            )
            .await
            .map_err(db_err)?;
        match rows.next().await.map_err(db_err)? {
            Some(row) => Ok(ClaimEpoch(row.get::<i64>(0).map_err(db_err)?)),
            None => Err(ClaimError::Conflict),
        }
    }

    async fn current_claim(
        &self,
        entity: &Entity,
    ) -> Result<Option<waddle_xmpp::ownership::ClaimSnapshot>, ClaimError> {
        // ADR-0017 Phase 3 Slice 6 addition: read-only, deliberately
        // unlocked — same shape and same "never itself an authority" caveat
        // as `ensure_claimed`'s own conflict-path read above. The cross-node
        // XEP-0198 resume path uses this to decide which resume branch
        // applies and to learn the observed epoch to bind into a later
        // `steal_for_resume` call.
        let conn = self.db.control_plane_guard().await.map_err(db_err)?;
        // `owner_lease_fresh` (council-adjudicated fix, Slice 6): the exact
        // same `NOT EXISTS` owner-stale predicate `steal_stale`'s
        // `OwnerStale` arm uses, read here read-only/unlocked alongside the
        // claim row itself — never a raw heartbeat comparison, only the
        // committed `expired` flag (see that predicate's own doc comment).
        let mut rows = conn
            .query(
                r#"
                SELECT
                    c.node_id,
                    c.node_epoch,
                    c.claim_epoch,
                    EXISTS (
                        SELECT 1 FROM clustering_nodes n
                        WHERE n.node_id = c.node_id
                          AND NOT n.expired
                          AND n.node_epoch = c.node_epoch
                    ) AS owner_lease_fresh
                FROM clustering_claims c
                WHERE c.entity = ?
                "#,
                crate::db_params![entity_key(entity)],
            )
            .await
            .map_err(db_err)?;
        match rows.next().await.map_err(db_err)? {
            Some(row) => {
                let node_id: String = row.get(0).map_err(db_err)?;
                let node_epoch: String = row.get(1).map_err(db_err)?;
                let claim_epoch: i64 = row.get(2).map_err(db_err)?;
                let owner_lease_fresh: bool = row.get(3).map_err(db_err)?;
                Ok(Some(waddle_xmpp::ownership::ClaimSnapshot {
                    owner: NodeIdentity::new(node_id, node_epoch),
                    claim_epoch: ClaimEpoch(claim_epoch),
                    owner_lease_fresh,
                }))
            }
            None => Ok(None),
        }
    }

    async fn current_claim_after_pending_writes(
        &self,
        entity: &Entity,
    ) -> Result<Option<waddle_xmpp::ownership::ClaimSnapshot>, ClaimError> {
        // Terminal orphan recovery can arrive here after its caller dropped
        // a steal future. Locking the claim row makes this SELECT wait behind
        // that detached UPDATE, so the returned version is post-commit (or
        // post-rollback) rather than the unlocked pre-CAS snapshot. A stale
        // room steal only updates an existing row, so the absent-row gap does
        // not need predicate/range locking.
        let conn = self.db.control_plane_guard().await.map_err(db_err)?;
        let mut rows = conn
            .query(
                r#"
                /* terminal_claim_reconciliation_lock */
                SELECT
                    c.node_id,
                    c.node_epoch,
                    c.claim_epoch,
                    EXISTS (
                        SELECT 1 FROM clustering_nodes n
                        WHERE n.node_id = c.node_id
                          AND NOT n.expired
                          AND n.node_epoch = c.node_epoch
                    ) AS owner_lease_fresh
                FROM clustering_claims c
                WHERE c.entity = ?
                FOR UPDATE
                "#,
                crate::db_params![entity_key(entity)],
            )
            .await
            .map_err(db_err)?;
        match rows.next().await.map_err(db_err)? {
            Some(row) => {
                let node_id: String = row.get(0).map_err(db_err)?;
                let node_epoch: String = row.get(1).map_err(db_err)?;
                let claim_epoch: i64 = row.get(2).map_err(db_err)?;
                let owner_lease_fresh: bool = row.get(3).map_err(db_err)?;
                Ok(Some(waddle_xmpp::ownership::ClaimSnapshot {
                    owner: NodeIdentity::new(node_id, node_epoch),
                    claim_epoch: ClaimEpoch(claim_epoch),
                    owner_lease_fresh,
                }))
            }
            None => Ok(None),
        }
    }

    async fn fence(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
        mine: ClaimEpoch,
    ) -> Result<bool, ClaimError> {
        // Advisory-only point read (never the write-path fencing
        // mechanism — see the trait doc comment): a claims-table point
        // read, so it rides the control-plane pool alongside the CAS
        // statements, per the ADR's pool-isolation rule.
        let conn = self.db.control_plane_guard().await.map_err(db_err)?;
        let mut rows = conn
            .query(
                r#"
                SELECT 1 FROM clustering_claims
                WHERE entity = ? AND node_id = ? AND node_epoch = ? AND claim_epoch = ?
                "#,
                crate::db_params![
                    entity_key(entity),
                    me.node_id.clone(),
                    me.node_epoch.clone(),
                    mine.0,
                ],
            )
            .await
            .map_err(db_err)?;
        Ok(rows.next().await.map_err(db_err)?.is_some())
    }

    async fn release(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
        mine: ClaimEpoch,
    ) -> Result<(), ClaimError> {
        let conn = self.db.control_plane_guard().await.map_err(db_err)?;
        conn.execute(
            r#"
                DELETE FROM clustering_claims
                WHERE entity = ? AND node_id = ? AND node_epoch = ? AND claim_epoch = ?
                "#,
            crate::db_params![
                entity_key(entity),
                me.node_id.clone(),
                me.node_epoch.clone(),
                mine.0,
            ],
        )
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn release_exact(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
        mine: ClaimEpoch,
    ) -> Result<waddle_xmpp::ownership::ExactReleaseOutcome, ClaimError> {
        let conn = self.db.control_plane_guard().await.map_err(db_err)?;
        let affected = conn
            .execute(
                r#"
                DELETE FROM clustering_claims
                WHERE entity = ? AND node_id = ? AND node_epoch = ? AND claim_epoch = ?
                "#,
                crate::db_params![
                    entity_key(entity),
                    me.node_id.clone(),
                    me.node_epoch.clone(),
                    mine.0,
                ],
            )
            .await
            .map_err(db_err)?;
        if affected == 1 {
            Ok(waddle_xmpp::ownership::ExactReleaseOutcome::Released)
        } else {
            Ok(waddle_xmpp::ownership::ExactReleaseOutcome::NotOwned)
        }
    }

    async fn release_many(&self, entities: &[Entity], me: &NodeIdentity) -> Result<(), ClaimError> {
        if entities.is_empty() {
            return Ok(());
        }
        let conn = self.db.control_plane_guard().await.map_err(db_err)?;
        // One round trip for the whole batch (Slice 10's ~18k modeled
        // drain claims): release does not pin a per-entity epoch, only
        // "still owned by me, under my current node identity, whatever
        // the epoch." This is exactly the epoch-blind ABA window documented
        // on `ClaimStore::release_many`'s trait doc comment — see there for
        // the mitigations (Slice 2 draining marker, Slice 10 batch
        // ordering).
        // `db_params!` only accepts a fixed, comma-separated literal list,
        // not a runtime-length one, so the dynamically-sized `IN (...)`
        // parameter list is built as a plain `Vec<Value>` directly (`Vec<Value>`
        // implements `IntoParams`).
        let placeholders = entities.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
        let sql = format!(
            "DELETE FROM clustering_claims WHERE node_id = ? AND node_epoch = ? AND entity IN ({placeholders})"
        );
        let mut params: Vec<crate::db::Value> = vec![
            crate::db::Value::from(me.node_id.clone()),
            crate::db::Value::from(me.node_epoch.clone()),
        ];
        params.extend(
            entities
                .iter()
                .map(|entity| crate::db::Value::from(entity_key(entity))),
        );
        conn.execute(&sql, params).await.map_err(db_err)?;
        Ok(())
    }
}

/// Per-node liveness: registration, heartbeat renewal, node-side expiry, and
/// the demotion-reconciliation query (ADR-0017 Phase 3 Slice 2, element 4).
/// Deliberately **not** part of [`ClaimStore`] (major fix 9 in the phase
/// plan): heartbeats are per node, not per entity, matching the ADR's own
/// framing. Unlike `ClaimStore`, no ordinary single-node `waddle-xmpp` code
/// ever needs a `NodeLeaseStore` — every caller is clustering-internal
/// (`self_fence.rs`, already `#[cfg(feature = "clustering")]`) — so the
/// trait lives here in `waddle-server` rather than being split across
/// crates the way `ClaimStore` is (Q1's unconditional-compilation
/// rationale does not apply to a store nothing outside clustering calls).
///
/// [`count_other_live_nodes`](NodeLeaseStore::count_other_live_nodes) and
/// [`reconcile`](NodeLeaseStore::reconcile) are not part of the ADR's
/// literal sketch (which named only `register`/`heartbeat`/`expire`/
/// `mark_draining`) — they are added here because the isolation rule and
/// the demotion-reconciliation query both need a `clustering_nodes`/
/// `clustering_claims` read this store already owns the pool/table for,
/// and inventing a third store for two read-only queries would be
/// gratuitous.
#[async_trait]
pub trait NodeLeaseStore: Send + Sync {
    /// Register this node's liveness row: fresh startup, or re-registration
    /// under a new `node_id`/`node_epoch` after a self-fence (Q7/element 4).
    /// Idempotent (`ON CONFLICT` upsert) so a retried registration is safe.
    async fn register(
        &self,
        me: &NodeIdentity,
        pod_template_hash: Option<String>,
    ) -> Result<(), ClaimError>;

    /// Register this node and bind the claim node identity to the libp2p
    /// PeerId currently holding the leased swarm keypair. The default keeps
    /// existing fakes/no-op stores simple; production Postgres overrides it
    /// so inbound ordered-relay delivery can verify signed origin envelopes
    /// against a registry-bound PeerId, not a sender-provided node string.
    async fn register_with_peer_id(
        &self,
        me: &NodeIdentity,
        pod_template_hash: Option<String>,
        _peer_id: Option<String>,
    ) -> Result<(), ClaimError> {
        self.register(me, pod_template_hash).await
    }

    /// Renew this node's heartbeat. `Ok(false)` (zero rows affected) is
    /// **fencing loss** — the lease lapsed, was expired by a
    /// stealer/reaper, or the epoch was superseded — not an error: the
    /// caller demotes all local claims and self-fences immediately.
    async fn heartbeat(&self, me: &NodeIdentity, lease_ttl: Duration) -> Result<bool, ClaimError>;

    /// The Expire CAS (element 4's single serialized ordering point):
    /// commits `expired = true` for `owner` if its heartbeat has lapsed
    /// past `lease_ttl` and it is not already expired. Returns `true` if
    /// the row is now (or was already) committed-expired — i.e. a
    /// subsequent `steal_stale` may proceed — `false` if the row is fresh.
    async fn expire(&self, owner: &NodeIdentity, lease_ttl: Duration) -> Result<bool, ClaimError>;

    /// Bounded advisory scan for non-expired nodes whose raw heartbeat has
    /// lapsed past `lease_ttl`.
    ///
    /// This method deliberately does **not** mark rows expired. It is only a
    /// watchdog candidate list: callers must feed each returned identity
    /// through [`Self::expire`], whose CAS remains the sole authoritative
    /// transition from heartbeat-stale to committed `expired = true`.
    ///
    /// The default supports test/no-op lease stores; production
    /// [`PostgresClaimStore`] overrides it.
    async fn list_heartbeat_stale_nodes(
        &self,
        _lease_ttl: Duration,
        _limit: usize,
    ) -> Result<Vec<NodeIdentity>, ClaimError> {
        Ok(Vec::new())
    }

    /// Read-only self-liveness proof for background workers that are about to
    /// acquire new ownership. Mirrors [`Self::heartbeat`]'s freshness
    /// predicate without mutating the heartbeat: same node id, same epoch,
    /// not expired, not draining, and heartbeat still inside `lease_ttl`.
    ///
    /// The default supports fakes/no-op stores that have no lease table.
    async fn is_fresh(&self, _me: &NodeIdentity, _lease_ttl: Duration) -> Result<bool, ClaimError> {
        Ok(true)
    }

    /// Read the PeerId currently bound to this exact `node_id`/`node_epoch`
    /// row. `None` means the node row is absent or predates the binding and
    /// must not authenticate relay delivery effects.
    async fn peer_id_for_node(&self, _node: &NodeIdentity) -> Result<Option<String>, ClaimError> {
        Ok(None)
    }

    /// Mark this node draining (Slice 10: stop acquiring new claims, keep
    /// serving already-owned ones). Best-effort, idempotent.
    async fn mark_draining(&self, me: &NodeIdentity) -> Result<(), ClaimError>;

    /// Count other (not `me`) live rows in `clustering_nodes` right now —
    /// feeds the isolation rule's "`clustering_nodes` shows two or more
    /// other live nodes" condition (element 4). "Live" means: not
    /// committed-`expired`, not `draining` (Slice 10 — a draining node is
    /// on its way out and must not inflate another node's isolation
    /// carve-out count), **and** heartbeat-fresh (`heartbeat >= now() -
    /// lease_ttl`).
    ///
    /// **The heartbeat-freshness read does not violate the "never infer
    /// expiry from raw heartbeat" rule** (`steal_stale`'s doc comment
    /// above, element 4): that ban is scoped to the *fencing CAS
    /// predicates that decide whether another node's claim may be taken*
    /// (`steal_stale`'s `OwnerStale` predicate) — those must read only the
    /// committed `expired` flag, because a race between "read a stale
    /// heartbeat" and "the owner's own renewal lands a moment later" would
    /// let two nodes believe they both hold the same claim. This method
    /// makes no fencing decision over anyone else's claims at all: it is a
    /// purely advisory count feeding *this* node's own isolation heuristic
    /// (whether to refuse its *own* renewal), never a commitment about
    /// another node's liveness that any other transaction relies on for
    /// correctness. A stale-but-not-yet-`expired` peer under-counted here
    /// costs nothing worse than a delayed or skipped self-fence decision on
    /// this node — reading the raw heartbeat is safe precisely because
    /// nothing downstream treats this count as authoritative the way the
    /// steal CAS treats the `expired` flag.
    ///
    /// Without the heartbeat-freshness filter, a node whose process is
    /// hard-killed (SIGKILL, OOM) leaves a `clustering_nodes` row that is
    /// never explicitly expired — nothing in production calls
    /// [`expire`](Self::expire) this slice (`ClaimStore::steal_stale`'s
    /// `OwnerStale` path is the only caller, and it has no production
    /// caller yet either) — so a bare `NOT expired` filter would count a
    /// long-dead node as "live" forever, permanently and wrongly inflating
    /// every other node's isolation count.
    async fn count_other_live_nodes(
        &self,
        me: &NodeIdentity,
        lease_ttl: Duration,
    ) -> Result<usize, ClaimError>;

    /// The demotion-reconciliation query (element 4/Slice 2): read every
    /// entity currently on file as owned by `(me.node_id, me.node_epoch)`
    /// via the `clustering_claims_node_id_node_epoch` index, and return
    /// every entry of `locally_owned` that is absent from that
    /// authoritative set — claims this process must demote/tombstone
    /// locally. Performs no local-state mutation itself; the caller
    /// demotes each returned entity.
    async fn reconcile(
        &self,
        me: &NodeIdentity,
        locally_owned: &[Entity],
    ) -> Result<Vec<Entity>, ClaimError>;

    /// Report a steal intent against `entity` (ADR-0017 Phase 3 Slice 3,
    /// element 4's "Unwedge" text): "after N consecutive failed/NACKed
    /// remote deliveries to a fresh-lease owner, the frustrated node writes
    /// a `steal_intents` row." Refresh-not-accumulate: the `UNIQUE (entity,
    /// reporter_node)` upsert collapses repeated reports from the same
    /// reporter against the same entity into one row with a freshened
    /// `created_at`, so a sustained relay fault never grows the table
    /// unbounded.
    ///
    /// **FIX 4 — reporter calling convention (binding on the Slice 5+
    /// cross-node reporter)**: a caller MUST report on **crossing a failure
    /// threshold**, not on every individual failed/NACKed delivery attempt.
    /// Because this upsert refreshes `created_at` on every call, a reporter
    /// that calls this once per failed attempt — faster than `intent_ttl`
    /// elapses between attempts — perpetually resets the row's age and the
    /// intent can never clear `intent_ttl`, permanently starving the steal
    /// this mechanism exists to unwedge. The correct shape: count
    /// consecutive failures locally, call `report_steal_intent` once when
    /// the count first crosses the configured threshold (N), and do not
    /// call it again until either the intent clears (owner vetoed) or the
    /// steal succeeds — never on a per-attempt cadence. See the Phase 3
    /// plan's Slice 5 reporter text for the forward reference to the actual
    /// cross-node reporter this constrains.
    ///
    /// Rejects [`EntityType::SmSession`](waddle_xmpp::ownership::EntityType)
    /// with [`ClaimError::SmSessionExcludedFromStealIntent`] — rule 1 of the
    /// three-rule steal-variant block: SM-session claims are never stolen
    /// via the steal-intent path.
    async fn report_steal_intent(
        &self,
        entity: &Entity,
        reporter: &NodeIdentity,
    ) -> Result<(), ClaimError>;

    /// Every entity currently claimed by `me` (under its current
    /// `node_id`/`node_epoch`) that has at least one outstanding
    /// steal-intent row — the owner-veto loop's own read ("every owner's
    /// heartbeat loop reads intents against its own claims," element 4).
    /// Returns each entity alongside the [`ClaimEpoch`] `me` currently holds
    /// it under, ready to feed [`Self::clear_steal_intent`]'s epoch-fenced
    /// DELETE. `SmSession` claims are excluded defensively (mirroring
    /// [`Self::report_steal_intent`]'s rejection — no row should exist for
    /// one, but this filter means a stray row can never surface here
    /// either).
    async fn owner_steal_intents(
        &self,
        me: &NodeIdentity,
    ) -> Result<Vec<(Entity, ClaimEpoch)>, ClaimError>;

    /// Epoch-fenced veto: delete every steal-intent row for `entity` iff
    /// `me` still holds it under `mine` right now (element 4's owner-veto
    /// text). **FIX 1(e), corrected guarantee**: the earlier doc claimed
    /// this uses "the same single-statement CAS discipline
    /// `steal_stale`/`steal_for_resume` already use" — that was imprecise.
    /// Those two are a **self-CAS on their own row** (`clustering_claims`):
    /// the row they gate is the exact row they write. This method gates a
    /// write on a *different* table (`clustering_steal_intents`) by a
    /// `FOR SHARE` lock taken on `clustering_claims` — a cross-table
    /// fencing discipline, not a self-CAS. The actual guarantee, precisely:
    /// this DELETE and `steal_stale`'s `StealIntentExpired` CTE (FIX 1(a))
    /// acquire their `clustering_claims`/`clustering_steal_intents` locks in
    /// **opposite order** by design, so a concurrent veto-clear and a
    /// concurrent steal **serialize on the intent rows themselves**
    /// (whichever statement's row-locking `DELETE` half commits first
    /// wins; the loser's own predicate re-check, evaluated against the
    /// now-committed state, observes nothing left to act on) — never on an
    /// unlocked, racy `EXISTS` read of a table neither statement is
    /// otherwise touching. Under contention this lock-order inversion can
    /// surface as a Postgres `40P01` deadlock on either side (FIX 1(c)) —
    /// safe, typed, logged at `debug`, retried by the loser next
    /// tick/scan — never a correctness hazard.
    ///
    /// **FIX 1(b)**: returns the number of rows actually deleted (`0` or
    /// more; realistically `0` or `1` per reporter row, so a caller with
    /// exactly one outstanding intent for `entity` sees `0` or `1`), not a
    /// bare `Result<(), _>`. A deposed owner's stale-epoch call observes
    /// `Ok(0)` — a no-op, not an error, exactly like every other
    /// epoch-gated CAS in this store — but unlike a bare unit return, the
    /// caller can now distinguish "the veto genuinely landed" from
    /// "nothing happened, and I no longer hold this claim under `mine`."
    /// [`self_fence::run_node_lease`](super::self_fence::run_node_lease)
    /// treats `Ok(0)` after a nonzero
    /// [`owner_steal_intents`](Self::owner_steal_intents) entry as
    /// "possibly deposed" and demotes the entity immediately rather than
    /// believing the veto succeeded.
    async fn clear_steal_intent(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
        mine: ClaimEpoch,
    ) -> Result<u64, ClaimError>;

    /// Candidate `EntityType::SmSession` claims owned by a stale node
    /// (ADR-0017 Phase 3 Slice 5's orphan reaper, element 9): "any node may
    /// steal such claims (fenced CAS) and then expire or promote them,
    /// after first committing the expire CAS on the owner's `nodes` row."
    /// This is the scan half — read-only and **unlocked** (an advisory
    /// candidate list only; the reaper's subsequent `expire` +
    /// `ClaimStore::steal_stale(OwnerStale)` calls are the actual
    /// authority, exactly like every other candidate-then-CAS pattern in
    /// this store). Scoped to `sm_session` only because RoomActor recovery
    /// has its own bounded scan/adoption path below, while UserActor claims
    /// remain demand-created and carry no durable actor state to hydrate.
    ///
    /// A row whose `entity` key does not decode cleanly against its own
    /// `entity_type` column (the same data-integrity anomaly
    /// [`decode_entity`] defensively rejects elsewhere) is skipped and
    /// logged rather than silently mangled — mirrors
    /// [`Self::owner_steal_intents`]'s handling.
    async fn list_orphaned_sm_session_claims(
        &self,
    ) -> Result<Vec<OrphanedSmSessionClaim>, ClaimError>;

    /// Ordered, bounded cursor page used by the periodic orphan reaper. The
    /// default keeps test/dummy stores source-compatible; durable stores
    /// should override this so the bound is enforced by the query itself.
    async fn list_orphaned_sm_session_claims_page(
        &self,
        after: Option<SmOrphanScanCursor>,
        limit: usize,
    ) -> Result<OrphanedSmSessionClaimPage, ClaimError> {
        let mut candidates = self.list_orphaned_sm_session_claims().await?;
        candidates.sort_by_key(|candidate| entity_key(&candidate.entity));
        if let Some(after) = after.as_ref() {
            candidates.retain(|candidate| {
                entity_key(&candidate.entity).as_str() > after.raw_key.as_str()
            });
        }
        let has_more = candidates.len() > limit;
        candidates.truncate(limit);
        let next_cursor = candidates
            .last()
            .map(|candidate| SmOrphanScanCursor::from_raw(entity_key(&candidate.entity)));
        Ok(OrphanedSmSessionClaimPage {
            candidates,
            next_cursor,
            has_more,
            quarantined: 0,
        })
    }

    /// Persist one lane-typed advisory cursor after a complete page. The
    /// update enum carries its lane even when resetting to `None`, making
    /// cross-lane cursor writes unrepresentable at this public boundary.
    async fn persist_orphan_reaper_cursor(
        &self,
        _update: OrphanReaperCursorUpdate,
    ) -> Result<(), ClaimError> {
        Ok(())
    }

    /// Bounded inline-recovery scan for one identity this process has just
    /// superseded. Production stores override with an owner-indexed query;
    /// the default keeps fakes source-compatible.
    async fn list_orphaned_sm_session_claims_for_owner(
        &self,
        owner: &NodeIdentity,
        limit: usize,
    ) -> Result<Vec<OrphanedSmSessionClaim>, ClaimError> {
        let mut candidates = self.list_orphaned_sm_session_claims().await?;
        candidates.retain(|candidate| candidate.owner == *owner);
        candidates.truncate(limit);
        Ok(candidates)
    }

    /// Reaper-only stale-owner steal for detached SM-session claims. Unlike
    /// [`ClaimStore::steal_stale`]'s generic owner-stale CAS, this binds the
    /// stealer's own heartbeat freshness into the same SQL statement because
    /// the orphan reaper has the node `lease_ttl` available and must not win
    /// new work after its local lease has lapsed.
    async fn steal_orphaned_sm_session_claim(
        &self,
        _entity: &Entity,
        _observed: ClaimEpoch,
        _me: &NodeIdentity,
        _lease_ttl: Duration,
    ) -> Result<ClaimEpoch, ClaimError> {
        Err(ClaimError::Conflict)
    }

    /// Inline counterpart used immediately after re-registration. Durable
    /// Postgres stores override this to bind the fresh stealer's heartbeat
    /// into the CAS; fakes delegate to their supplied claim store.
    async fn steal_own_expired_sm_session_claim(
        &self,
        claim_store: &dyn ClaimStore,
        entity: &Entity,
        observed: ClaimEpoch,
        me: &NodeIdentity,
        _lease_ttl: Duration,
    ) -> Result<ClaimEpoch, ClaimError> {
        claim_store
            .steal_stale(entity, observed, StalePredicate::OwnerStale, me)
            .await
    }

    /// Bounded advisory scan for `RoomActor` claims whose recorded owner
    /// lease is committed-stale. The returned snapshot never authorizes a
    /// takeover: callers must still expire the owner and win
    /// [`Self::steal_orphaned_room_actor_claim`]'s epoch-fenced CAS.
    async fn list_orphaned_room_actor_claims(
        &self,
        _limit: usize,
    ) -> Result<Vec<OrphanedRoomActorClaim>, ClaimError> {
        Ok(Vec::new())
    }

    /// Raw-key cursor page for RoomActor recovery. Durable stores override
    /// this so live-owner and malformed rows still consume the fixed scan
    /// budget instead of forcing an unbounded predicate scan before LIMIT.
    async fn list_orphaned_room_actor_claims_page(
        &self,
        after: Option<RoomOrphanScanCursor>,
        limit: usize,
    ) -> Result<OrphanedRoomActorClaimPage, ClaimError>;

    /// Reaper-only stale-owner steal for a `RoomActor`. The production
    /// implementation binds both the observed claim epoch and the sweeping
    /// node's own fresh, non-draining lease into one statement, so neither a
    /// renewed owner nor a stale sweeper can win a candidate-scan race.
    async fn steal_orphaned_room_actor_claim(
        &self,
        _entity: &Entity,
        _observed: ClaimEpoch,
        _me: &NodeIdentity,
        _lease_ttl: Duration,
    ) -> Result<ClaimEpoch, ClaimError> {
        Err(ClaimError::Conflict)
    }

    /// The current deployment generation's `pod_template_hash` (ADR-0017
    /// Phase 3 Slice 10, Q5's operational definition): the hash stamped on
    /// the non-expired `clustering_nodes` row with the greatest
    /// `first_seen`. `None` when there are no live rows at all, or when
    /// that freshest row's own `pod_template_hash` is `None` (outside
    /// Kubernetes, or a Deployment that omits the downward-API env var) —
    /// both treated identically by every acquire-backoff call site as "no
    /// generation to compare against," never a parse failure. Read-only,
    /// advisory-only: this is a placement heuristic (decides who tries
    /// first), never an ownership authority — the claims CAS remains the
    /// sole source of truth over who actually wins any given claim.
    async fn current_generation(&self) -> Result<Option<String>, ClaimError>;
}

/// A candidate orphaned `sm_session` claim: its entity/epoch plus the
/// (stale) owner currently on file for it. See
/// [`NodeLeaseStore::list_orphaned_sm_session_claims`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanedSmSessionClaim {
    pub entity: Entity,
    pub epoch: ClaimEpoch,
    pub owner: NodeIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanedSmSessionClaimPage {
    pub candidates: Vec<OrphanedSmSessionClaim>,
    pub next_cursor: Option<SmOrphanScanCursor>,
    pub has_more: bool,
    pub quarantined: usize,
}

/// Opaque ordering checkpoint for the raw `sm_session` claim-key lane.
///
/// The private value intentionally remains the database key rather than a
/// parsed `SmSessionId`: malformed keys must still advance the integrity
/// scan so one poison row cannot pin every later claim forever.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmOrphanScanCursor {
    raw_key: String,
}

impl SmOrphanScanCursor {
    pub(crate) fn from_raw(raw_key: String) -> Self {
        Self { raw_key }
    }

    #[cfg(test)]
    pub(crate) fn as_raw(&self) -> &str {
        &self.raw_key
    }
}

/// A bounded-scan candidate for proactive `RoomActor` reconciliation.
/// Its fields are only an observation; the reaper's subsequent CAS is the
/// authority over whether the claim can move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanedRoomActorClaim {
    pub entity: Entity,
    pub epoch: ClaimEpoch,
    pub owner: NodeIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanedRoomActorClaimPage {
    pub candidates: Vec<OrphanedRoomActorClaim>,
    pub next_cursor: Option<RoomOrphanScanCursor>,
    pub has_more: bool,
    pub quarantined: usize,
}

/// Opaque ordering checkpoint for the raw `room_actor` claim-key lane.
/// The raw key is private because this is a storage cursor, not a room JID;
/// malformed keys must remain representable for forward progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomOrphanScanCursor {
    raw_key: String,
}

impl RoomOrphanScanCursor {
    pub(crate) fn from_raw(raw_key: String) -> Self {
        Self { raw_key }
    }

    #[cfg(test)]
    pub(crate) fn as_raw(&self) -> &str {
        &self.raw_key
    }
}

/// Lane-safe durable cursor write. `None` is a typed reset for exactly one
/// lane rather than an untyped `(EntityType, Option<String>)` pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrphanReaperCursorUpdate {
    SmSession(Option<SmOrphanScanCursor>),
    RoomActor(Option<RoomOrphanScanCursor>),
}

impl OrphanReaperCursorUpdate {
    fn into_db_parts(self) -> (EntityType, Option<String>) {
        match self {
            Self::SmSession(cursor) => (EntityType::SmSession, cursor.map(|cursor| cursor.raw_key)),
            Self::RoomActor(cursor) => (EntityType::RoomActor, cursor.map(|cursor| cursor.raw_key)),
        }
    }
}

async fn register_node(
    store: &PostgresClaimStore,
    me: &NodeIdentity,
    pod_template_hash: Option<String>,
    peer_id: Option<String>,
) -> Result<(), ClaimError> {
    // Runs on the control-plane pool (element 4/12, Slice 0): node
    // registration is liveness-control-plane traffic, never the main pool.
    let conn = store.db.control_plane_guard().await.map_err(db_err)?;
    conn.execute(
        r#"
        INSERT INTO clustering_nodes (node_id, node_epoch, heartbeat, expired, pod_template_hash, draining, peer_id)
        VALUES (?, ?, now(), false, ?, false, ?)
        ON CONFLICT (node_id) DO UPDATE SET
            node_epoch = EXCLUDED.node_epoch,
            heartbeat = now(),
            expired = false,
            pod_template_hash = EXCLUDED.pod_template_hash,
            draining = false,
            peer_id = EXCLUDED.peer_id
        "#,
        crate::db_params![
            me.node_id.clone(),
            me.node_epoch.clone(),
            pod_template_hash,
            peer_id,
        ],
    )
    .await
    .map_err(db_err)?;
    Ok(())
}

#[async_trait]
impl NodeLeaseStore for PostgresClaimStore {
    async fn register(
        &self,
        me: &NodeIdentity,
        pod_template_hash: Option<String>,
    ) -> Result<(), ClaimError> {
        register_node(self, me, pod_template_hash, None).await
    }

    async fn register_with_peer_id(
        &self,
        me: &NodeIdentity,
        pod_template_hash: Option<String>,
        peer_id: Option<String>,
    ) -> Result<(), ClaimError> {
        register_node(self, me, pod_template_hash, peer_id).await
    }

    async fn peer_id_for_node(&self, node: &NodeIdentity) -> Result<Option<String>, ClaimError> {
        let conn = self.db.control_plane_guard().await.map_err(db_err)?;
        let mut rows = conn
            .query(
                r#"
                SELECT peer_id
                FROM clustering_nodes
                WHERE node_id = ?
                  AND node_epoch = ?
                  AND NOT expired
                "#,
                crate::db_params![node.node_id.clone(), node.node_epoch.clone()],
            )
            .await
            .map_err(db_err)?;
        let Some(row) = rows.next().await.map_err(db_err)? else {
            return Ok(None);
        };
        row.get::<Option<String>>(0).map_err(db_err)
    }

    async fn heartbeat(&self, me: &NodeIdentity, lease_ttl: Duration) -> Result<bool, ClaimError> {
        let conn = self.db.control_plane_guard().await.map_err(db_err)?;
        // Heartbeat CAS (element 4, locked verbatim): renew only while the
        // lease is still fresh under our own identity.
        let affected = conn
            .execute(
                r#"
                UPDATE clustering_nodes
                SET heartbeat = now()
                WHERE node_id = ?
                  AND node_epoch = ?
                  AND NOT expired
                  AND heartbeat >= now() - (? || ' milliseconds')::interval
                "#,
                crate::db_params![
                    me.node_id.clone(),
                    me.node_epoch.clone(),
                    lease_ttl.as_millis().to_string(),
                ],
            )
            .await
            .map_err(db_err)?;
        Ok(affected == 1)
    }

    async fn expire(&self, owner: &NodeIdentity, lease_ttl: Duration) -> Result<bool, ClaimError> {
        let conn = self.db.control_plane_guard().await.map_err(db_err)?;
        // Expire CAS (element 4, locked verbatim): the single serialized
        // ordering point that makes lease expiry monotone — see this
        // module's own doc comment and `steal_stale`'s NOT EXISTS predicate,
        // which reads only the committed flag this statement sets.
        let affected = conn
            .execute(
                r#"
                UPDATE clustering_nodes
                SET expired = true
                WHERE node_id = ?
                  AND node_epoch = ?
                  AND NOT expired
                  AND heartbeat < now() - (? || ' milliseconds')::interval
                "#,
                crate::db_params![
                    owner.node_id.clone(),
                    owner.node_epoch.clone(),
                    lease_ttl.as_millis().to_string(),
                ],
            )
            .await
            .map_err(db_err)?;
        if affected == 1 {
            return Ok(true);
        }
        // We did not flip the flag ourselves — either it is already
        // committed true, the row is still fresh, or the row is gone
        // entirely. Distinguish those (a missing/expired row means "proceed,
        // this owner is stale" — the same vacuous-stale treatment
        // `steal_stale`'s NOT EXISTS predicate gives a vanished node).
        let mut rows = conn
            .query(
                "SELECT expired FROM clustering_nodes WHERE node_id = ? AND node_epoch = ?",
                crate::db_params![owner.node_id.clone(), owner.node_epoch.clone()],
            )
            .await
            .map_err(db_err)?;
        match rows.next().await.map_err(db_err)? {
            Some(row) => row.get::<bool>(0).map_err(db_err),
            None => Ok(true),
        }
    }

    async fn list_heartbeat_stale_nodes(
        &self,
        lease_ttl: Duration,
        limit: usize,
    ) -> Result<Vec<NodeIdentity>, ClaimError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.db.control_plane_guard().await.map_err(db_err)?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut rows = conn
            .query(
                r#"
                SELECT node_id, node_epoch
                FROM clustering_nodes
                WHERE NOT expired
                  AND heartbeat < now() - (? || ' milliseconds')::interval
                ORDER BY heartbeat ASC, node_id ASC, node_epoch ASC
                LIMIT ?
                "#,
                crate::db_params![lease_ttl.as_millis().to_string(), limit],
            )
            .await
            .map_err(db_err)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.map_err(db_err)? {
            let node_id: String = row.get(0).map_err(db_err)?;
            let node_epoch: String = row.get(1).map_err(db_err)?;
            out.push(NodeIdentity::new(node_id, node_epoch));
        }
        Ok(out)
    }

    async fn is_fresh(&self, me: &NodeIdentity, lease_ttl: Duration) -> Result<bool, ClaimError> {
        let conn = self.db.control_plane_guard().await.map_err(db_err)?;
        let mut rows = conn
            .query(
                r#"
                SELECT 1 FROM clustering_nodes
                WHERE node_id = ?
                  AND node_epoch = ?
                  AND NOT expired
                  AND NOT draining
                  AND heartbeat >= now() - (? || ' milliseconds')::interval
                "#,
                crate::db_params![
                    me.node_id.clone(),
                    me.node_epoch.clone(),
                    lease_ttl.as_millis().to_string(),
                ],
            )
            .await
            .map_err(db_err)?;
        Ok(rows.next().await.map_err(db_err)?.is_some())
    }

    async fn mark_draining(&self, me: &NodeIdentity) -> Result<(), ClaimError> {
        let conn = self.db.control_plane_guard().await.map_err(db_err)?;
        conn.execute(
            "UPDATE clustering_nodes SET draining = true WHERE node_id = ? AND node_epoch = ?",
            crate::db_params![me.node_id.clone(), me.node_epoch.clone()],
        )
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn count_other_live_nodes(
        &self,
        me: &NodeIdentity,
        lease_ttl: Duration,
    ) -> Result<usize, ClaimError> {
        let conn = self.db.control_plane_guard().await.map_err(db_err)?;
        // FIX 1(c): "live" requires NOT expired, NOT draining, AND a fresh
        // heartbeat — see the trait doc comment for why reading the raw
        // heartbeat here is safe (advisory isolation heuristic, never a
        // fencing decision over another node's claims).
        let mut rows = conn
            .query(
                r#"
                SELECT COUNT(*) FROM clustering_nodes
                WHERE node_id != ?
                  AND NOT expired
                  AND NOT draining
                  AND heartbeat >= now() - (? || ' milliseconds')::interval
                "#,
                crate::db_params![me.node_id.clone(), lease_ttl.as_millis().to_string()],
            )
            .await
            .map_err(db_err)?;
        let count = match rows.next().await.map_err(db_err)? {
            Some(row) => row.get::<i64>(0).map_err(db_err)?,
            None => 0,
        };
        Ok(usize::try_from(count).unwrap_or(0))
    }

    async fn reconcile(
        &self,
        me: &NodeIdentity,
        locally_owned: &[Entity],
    ) -> Result<Vec<Entity>, ClaimError> {
        if locally_owned.is_empty() {
            return Ok(Vec::new());
        }
        // Indexed by `clustering_claims_node_id_node_epoch` (Slice 1's
        // DDL) — the ADR's "one indexed reconciliation query" per node per
        // interval.
        let conn = self.db.control_plane_guard().await.map_err(db_err)?;
        let mut rows = conn
            .query(
                "SELECT entity FROM clustering_claims WHERE node_id = ? AND node_epoch = ?",
                crate::db_params![me.node_id.clone(), me.node_epoch.clone()],
            )
            .await
            .map_err(db_err)?;
        let mut owned_keys = std::collections::HashSet::new();
        while let Some(row) = rows.next().await.map_err(db_err)? {
            owned_keys.insert(row.get::<String>(0).map_err(db_err)?);
        }
        Ok(locally_owned
            .iter()
            .filter(|entity| !owned_keys.contains(&entity_key(entity)))
            .cloned()
            .collect())
    }

    async fn report_steal_intent(
        &self,
        entity: &Entity,
        reporter: &NodeIdentity,
    ) -> Result<(), ClaimError> {
        if entity.entity_type == EntityType::SmSession {
            return Err(ClaimError::SmSessionExcludedFromStealIntent);
        }
        let conn = self.db.control_plane_guard().await.map_err(db_err)?;
        conn.execute(
            r#"
            INSERT INTO clustering_steal_intents (entity, reporter_node, created_at)
            VALUES (?, ?, now())
            ON CONFLICT (entity, reporter_node) DO UPDATE SET created_at = EXCLUDED.created_at
            "#,
            crate::db_params![entity_key(entity), reporter.node_id.clone()],
        )
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn owner_steal_intents(
        &self,
        me: &NodeIdentity,
    ) -> Result<Vec<(Entity, ClaimEpoch)>, ClaimError> {
        let conn = self.db.control_plane_guard().await.map_err(db_err)?;
        let sm_session_tag = EntityType::SmSession.as_db_str();
        let mut rows = conn
            .query(
                r#"
                SELECT DISTINCT c.entity, c.entity_type, c.claim_epoch
                FROM clustering_claims c
                JOIN clustering_steal_intents si ON si.entity = c.entity
                WHERE c.node_id = ? AND c.node_epoch = ? AND c.entity_type != ?
                "#,
                crate::db_params![
                    me.node_id.clone(),
                    me.node_epoch.clone(),
                    sm_session_tag.to_string()
                ],
            )
            .await
            .map_err(db_err)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.map_err(db_err)? {
            let encoded = row.get::<String>(0).map_err(db_err)?;
            let entity_type_str = row.get::<String>(1).map_err(db_err)?;
            let claim_epoch = row.get::<i64>(2).map_err(db_err)?;
            let Some(entity_type) = EntityType::from_db_str(&entity_type_str) else {
                tracing::warn!(
                    entity_type = %entity_type_str,
                    "owner_steal_intents: unrecognised entity_type in clustering_claims row; skipping"
                );
                continue;
            };
            // FIX 6: `decode_entity` now typed-rejects a key whose prefix
            // disagrees with the row's own `entity_type` column instead of
            // silently mangling the id — treat it exactly like the
            // unrecognised-entity_type case above: log and skip the row
            // rather than surfacing a corrupted `Entity`.
            let Some(entity) = decode_entity(&encoded, entity_type) else {
                tracing::warn!(
                    encoded = %encoded,
                    entity_type = %entity_type_str,
                    "owner_steal_intents: entity key prefix does not match entity_type; skipping"
                );
                continue;
            };
            out.push((entity, ClaimEpoch(claim_epoch)));
        }
        Ok(out)
    }

    async fn clear_steal_intent(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
        mine: ClaimEpoch,
    ) -> Result<u64, ClaimError> {
        let conn = self.db.control_plane_guard().await.map_err(db_err)?;
        // FIX 1(b) — council-adjudicated redesign closing the veto race:
        // the previous shape gated the DELETE on an *unlocked* `EXISTS`
        // read over `clustering_claims`, which could observe "still owned"
        // and commit the veto even while a concurrent `steal_stale`
        // (`StealIntentExpired`) was in the middle of winning the same
        // race — a write-skew hazard under READ COMMITTED. This version
        // takes a real `FOR SHARE` lock on the owning claim row as a
        // data-modifying CTE input, so the DELETE below only proceeds once
        // that lock is held and the row is confirmed live under `mine`.
        //
        // FIX 1(c) — DEADLOCK NOTE: this statement acquires locks in the
        // order {`clustering_claims` row (`FOR SHARE`), then
        // `clustering_steal_intents` rows (the DELETE)} — the *opposite*
        // order from `steal_stale`'s `StealIntentExpired` CTE (FIX 1(a)),
        // which acquires {`clustering_steal_intents` rows first, then the
        // `clustering_claims` row}. This is deliberate: it is exactly what
        // makes a concurrent veto-clear and a concurrent steal serialize
        // on the intent rows themselves rather than race an unlocked read
        // — see `steal_stale`'s doc comment for the full argument. Under
        // contention Postgres may abort either side with a `40P01`
        // deadlock instead of blocking it; that is SAFE (a typed
        // [`ClaimError::Backend`], never a panic — logged at `debug` via
        // [`log_if_postgres_deadlock`] — the loser retries next
        // tick/scan).
        //
        // A deposed owner's stale `(node_id, mine)` pair matches no
        // `clustering_claims` row, so `fenced` is empty, the DELETE's
        // `EXISTS (SELECT 1 FROM fenced)` is false, and it affects zero
        // rows — a silent no-op, exactly like `release`'s epoch-gated
        // semantics, but now observable by the caller via the returned
        // count (FIX 1(b): see the trait doc for why `run_node_lease`
        // needs to distinguish this from a genuine veto).
        let affected = conn
            .execute(
                r#"
                WITH fenced AS (
                    SELECT 1 FROM clustering_claims
                    WHERE entity = ? AND node_id = ? AND node_epoch = ? AND claim_epoch = ?
                    FOR SHARE
                )
                DELETE FROM clustering_steal_intents
                WHERE entity = ?
                  AND EXISTS (SELECT 1 FROM fenced)
                "#,
                crate::db_params![
                    entity_key(entity),
                    me.node_id.clone(),
                    me.node_epoch.clone(),
                    mine.0,
                    entity_key(entity),
                ],
            )
            .await
            .map_err(|error| {
                log_if_postgres_deadlock(&error, &entity_key(entity), "clear_steal_intent");
                db_err(error)
            })?;
        Ok(affected)
    }

    async fn list_orphaned_sm_session_claims(
        &self,
    ) -> Result<Vec<OrphanedSmSessionClaim>, ClaimError> {
        let conn = self.db.control_plane_guard().await.map_err(db_err)?;
        // Same owner-stale `NOT EXISTS` predicate as `steal_stale`'s
        // `OwnerStale` arm (element 4's "LEFT JOIN" predicate, realized as
        // a correlated subquery) — read-only and unlocked here: this is a
        // candidate scan, never itself the authority (see this method's
        // trait doc comment).
        //
        // ADR-0017 Phase 4 Slice 1a: orphan reaping may only surface detached
        // SM sessions that already have a durable `sm_sessions` row. `<enable
        // resume='true'/>` creates the claim while the socket is still live;
        // stealing such claim-only rows would leave this node owning an
        // unhydratable session and break later XEP-0198 resume paths.
        let mut rows = conn
            .query(
                r#"
                SELECT entity, node_id, node_epoch, claim_epoch
                FROM clustering_claims
                WHERE entity_type = ?
                  AND NOT EXISTS (
                    SELECT 1 FROM clustering_nodes n
                    WHERE n.node_id = clustering_claims.node_id
                      AND NOT n.expired
                      AND n.node_epoch = clustering_claims.node_epoch
                  )
                  AND EXISTS (
                    SELECT 1 FROM sm_sessions s
                    WHERE clustering_claims.entity = (? || ':' || s.stream_id)
                  )
                "#,
                crate::db_params![
                    EntityType::SmSession.as_db_str().to_string(),
                    EntityType::SmSession.as_db_str().to_string(),
                ],
            )
            .await
            .map_err(db_err)?;
        let mut out = Vec::new();
        loop {
            let Some(row) = rows.next().await.map_err(db_err)? else {
                break;
            };
            let encoded: String = row.get(0).map_err(db_err)?;
            let node_id: String = row.get(1).map_err(db_err)?;
            let node_epoch: String = row.get(2).map_err(db_err)?;
            let claim_epoch: i64 = row.get(3).map_err(db_err)?;
            let Some(entity) = decode_sm_session_entity(&encoded) else {
                tracing::warn!(
                    encoded_entity = %encoded,
                    "list_orphaned_sm_session_claims: row's entity key is not a valid \
                     bounded SM-session id; skipping (data-integrity anomaly)"
                );
                continue;
            };
            out.push(OrphanedSmSessionClaim {
                entity,
                epoch: ClaimEpoch(claim_epoch),
                owner: NodeIdentity::new(node_id, node_epoch),
            });
        }
        Ok(out)
    }

    async fn list_orphaned_sm_session_claims_page(
        &self,
        after: Option<SmOrphanScanCursor>,
        limit: usize,
    ) -> Result<OrphanedSmSessionClaimPage, ClaimError> {
        if limit == 0 {
            return Ok(OrphanedSmSessionClaimPage {
                candidates: Vec::new(),
                next_cursor: after,
                has_more: false,
                quarantined: 0,
            });
        }
        let scan_limit = limit.saturating_add(1);
        let cursor = match after {
            Some(cursor) => cursor.raw_key,
            None => self
                .load_sm_orphan_reaper_cursor()
                .await?
                .map(|cursor| cursor.raw_key)
                .unwrap_or_default(),
        };
        let conn = self.db.control_plane_guard().await.map_err(db_err)?;
        let mut rows = conn
            .query(
                r#"
                WITH raw_claims AS (
                    SELECT entity, node_id, node_epoch, claim_epoch
                    FROM clustering_claims
                    WHERE entity_type = ? AND entity > ?
                    ORDER BY entity
                    LIMIT ?
                )
                SELECT entity, node_id, node_epoch, claim_epoch,
                       CASE WHEN NOT EXISTS (
                         SELECT 1 FROM clustering_nodes n
                         WHERE n.node_id = raw_claims.node_id
                           AND NOT n.expired
                           AND n.node_epoch = raw_claims.node_epoch
                       ) THEN 1 ELSE 0 END AS owner_stale,
                       CASE WHEN EXISTS (
                         SELECT 1 FROM sm_sessions s
                         WHERE raw_claims.entity = ('sm_session:' || s.stream_id)
                       ) THEN 1 ELSE 0 END AS has_durable
                FROM raw_claims
                ORDER BY entity
                "#,
                crate::db_params![
                    EntityType::SmSession.as_db_str().to_string(),
                    cursor,
                    i64::try_from(scan_limit).unwrap_or(i64::MAX),
                ],
            )
            .await
            .map_err(db_err)?;
        let mut out = Vec::new();
        // Exact stale-owner snapshot plus an optional durable suffix. The
        // cleanup transaction revalidates claim-only absence; this scan-time
        // classification is never destructive authority by itself.
        let mut cleanup = Vec::new();
        let mut next_cursor = None;
        let mut observed_extra = false;
        let mut scanned = 0usize;
        while let Some(row) = rows.next().await.map_err(db_err)? {
            if scanned == limit {
                observed_extra = true;
                break;
            }
            scanned += 1;
            let encoded: String = row.get(0).map_err(db_err)?;
            let node_id: String = row.get(1).map_err(db_err)?;
            let node_epoch: String = row.get(2).map_err(db_err)?;
            let claim_epoch: i64 = row.get(3).map_err(db_err)?;
            let owner_stale: i64 = row.get(4).map_err(db_err)?;
            let has_durable: i64 = row.get(5).map_err(db_err)?;
            next_cursor = Some(SmOrphanScanCursor::from_raw(encoded.clone()));
            if owner_stale != 1 {
                continue;
            }
            let Some(entity) = decode_sm_session_entity(&encoded) else {
                // Only malformed keys in the canonical namespace may name
                // matching poison durable rows. Wrong-prefix keys can never
                // authorize deleting session state.
                let durable_suffix = encoded
                    .strip_prefix("sm_session:")
                    .filter(|_| has_durable == 1)
                    .map(str::to_string);
                cleanup.push(OrphanedSmClaimCleanup {
                    encoded,
                    owner: NodeIdentity::new(node_id, node_epoch),
                    claim_epoch: ClaimEpoch(claim_epoch),
                    durable_stream_id: durable_suffix,
                    malformed: true,
                });
                continue;
            };
            if has_durable != 1 {
                // A committed stale owner cannot still be in the live
                // claim-before-detach window. Remove only the exact stale
                // claim; never manufacture a durable session or steal it.
                cleanup.push(OrphanedSmClaimCleanup {
                    encoded,
                    owner: NodeIdentity::new(node_id, node_epoch),
                    claim_epoch: ClaimEpoch(claim_epoch),
                    durable_stream_id: None,
                    malformed: false,
                });
                continue;
            }
            out.push(OrphanedSmSessionClaim {
                entity,
                epoch: ClaimEpoch(claim_epoch),
                owner: NodeIdentity::new(node_id, node_epoch),
            });
        }
        drop(rows);
        drop(conn);
        let mut quarantined = 0usize;
        for cleanup in cleanup {
            quarantined += usize::from(self.cleanup_orphaned_sm_claim(cleanup).await?);
        }
        Ok(OrphanedSmSessionClaimPage {
            has_more: observed_extra,
            candidates: out,
            next_cursor,
            quarantined,
        })
    }

    async fn persist_orphan_reaper_cursor(
        &self,
        update: OrphanReaperCursorUpdate,
    ) -> Result<(), ClaimError> {
        let (lane, cursor) = update.into_db_parts();
        let conn = self.db.control_plane_guard().await.map_err(db_err)?;
        conn.execute(
            "INSERT INTO clustering_orphan_reaper_cursors (lane, cursor_entity) VALUES (?, ?) \
             ON CONFLICT (lane) DO UPDATE SET cursor_entity = EXCLUDED.cursor_entity",
            crate::db_params![lane.as_db_str().to_string(), cursor],
        )
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn list_orphaned_sm_session_claims_for_owner(
        &self,
        owner: &NodeIdentity,
        limit: usize,
    ) -> Result<Vec<OrphanedSmSessionClaim>, ClaimError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.db.control_plane_guard().await.map_err(db_err)?;
        let mut rows = conn
            .query(
                r#"
                WITH raw_claims AS (
                    SELECT entity, claim_epoch
                    FROM clustering_claims
                    WHERE node_id = ? AND node_epoch = ? AND entity_type = ?
                    ORDER BY entity
                    LIMIT ?
                )
                SELECT entity, claim_epoch,
                       CASE WHEN EXISTS (
                         SELECT 1 FROM sm_sessions s
                         WHERE raw_claims.entity = ('sm_session:' || s.stream_id)
                       ) THEN 1 ELSE 0 END AS has_durable
                FROM raw_claims
                ORDER BY entity
                "#,
                crate::db_params![
                    owner.node_id.clone(),
                    owner.node_epoch.clone(),
                    EntityType::SmSession.as_db_str().to_string(),
                    i64::try_from(limit).unwrap_or(i64::MAX),
                ],
            )
            .await
            .map_err(db_err)?;
        let mut candidates = Vec::new();
        while let Some(row) = rows.next().await.map_err(db_err)? {
            let encoded: String = row.get(0).map_err(db_err)?;
            let claim_epoch: i64 = row.get(1).map_err(db_err)?;
            let has_durable: i64 = row.get(2).map_err(db_err)?;
            if has_durable != 1 {
                continue;
            }
            let Some(entity) = decode_sm_session_entity(&encoded) else {
                continue;
            };
            candidates.push(OrphanedSmSessionClaim {
                entity,
                epoch: ClaimEpoch(claim_epoch),
                owner: owner.clone(),
            });
        }
        Ok(candidates)
    }

    async fn steal_orphaned_sm_session_claim(
        &self,
        entity: &Entity,
        observed: ClaimEpoch,
        me: &NodeIdentity,
        lease_ttl: Duration,
    ) -> Result<ClaimEpoch, ClaimError> {
        let conn = self.db.control_plane_guard().await.map_err(db_err)?;
        let mut rows = conn
            .query(
                r#"
                UPDATE clustering_claims
                SET node_id = ?, node_epoch = ?,
                    claim_epoch = nextval('clustering_claim_epoch_seq'::regclass)
                WHERE entity = ?
                  AND entity_type = ?
                  AND claim_epoch = ?
                  AND NOT EXISTS (
                    SELECT 1 FROM clustering_nodes n
                    WHERE n.node_id = clustering_claims.node_id
                      AND NOT n.expired
                      AND n.node_epoch = clustering_claims.node_epoch
                  )
                  AND EXISTS (
                    SELECT 1 FROM sm_sessions s
                    WHERE clustering_claims.entity = (? || ':' || s.stream_id)
                  )
                  AND EXISTS (
                    SELECT 1 FROM clustering_nodes
                    WHERE node_id = ?
                      AND node_epoch = ?
                      AND NOT expired
                      AND NOT draining
                      AND heartbeat >= now() - (? || ' milliseconds')::interval
                  )
                RETURNING claim_epoch
                "#,
                crate::db_params![
                    me.node_id.clone(),
                    me.node_epoch.clone(),
                    entity_key(entity),
                    EntityType::SmSession.as_db_str().to_string(),
                    observed.0,
                    EntityType::SmSession.as_db_str().to_string(),
                    me.node_id.clone(),
                    me.node_epoch.clone(),
                    lease_ttl.as_millis().to_string(),
                ],
            )
            .await
            .map_err(db_err)?;
        match rows.next().await.map_err(db_err)? {
            Some(row) => Ok(ClaimEpoch(row.get::<i64>(0).map_err(db_err)?)),
            None => Err(ClaimError::Conflict),
        }
    }

    async fn steal_own_expired_sm_session_claim(
        &self,
        _claim_store: &dyn ClaimStore,
        entity: &Entity,
        observed: ClaimEpoch,
        me: &NodeIdentity,
        lease_ttl: Duration,
    ) -> Result<ClaimEpoch, ClaimError> {
        self.steal_orphaned_sm_session_claim(entity, observed, me, lease_ttl)
            .await
    }

    async fn list_orphaned_room_actor_claims(
        &self,
        limit: usize,
    ) -> Result<Vec<OrphanedRoomActorClaim>, ClaimError> {
        Ok(self
            .list_orphaned_room_actor_claims_page(None, limit)
            .await?
            .candidates)
    }

    async fn list_orphaned_room_actor_claims_page(
        &self,
        after: Option<RoomOrphanScanCursor>,
        limit: usize,
    ) -> Result<OrphanedRoomActorClaimPage, ClaimError> {
        if limit == 0 {
            return Ok(OrphanedRoomActorClaimPage {
                candidates: Vec::new(),
                next_cursor: after,
                has_more: false,
                quarantined: 0,
            });
        }
        let scan_limit = limit.saturating_add(1);
        let cursor = match after {
            Some(cursor) => cursor.raw_key,
            None => self
                .load_room_orphan_reaper_cursor()
                .await?
                .map(|cursor| cursor.raw_key)
                .unwrap_or_default(),
        };
        let conn = self.db.control_plane_guard().await.map_err(db_err)?;
        let mut rows = conn
            .query(
                r#"
                WITH raw_claims AS (
                    SELECT entity, node_id, node_epoch, claim_epoch
                    FROM clustering_claims
                    WHERE entity_type = ? AND entity > ?
                    ORDER BY entity
                    LIMIT ?
                )
                SELECT entity, node_id, node_epoch, claim_epoch,
                       CASE WHEN NOT EXISTS (
                         SELECT 1 FROM clustering_nodes n
                         WHERE n.node_id = raw_claims.node_id
                           AND NOT n.expired
                           AND n.node_epoch = raw_claims.node_epoch
                       ) THEN 1 ELSE 0 END AS owner_stale
                FROM raw_claims
                ORDER BY entity
                "#,
                crate::db_params![
                    EntityType::RoomActor.as_db_str().to_string(),
                    cursor,
                    i64::try_from(scan_limit).unwrap_or(i64::MAX),
                ],
            )
            .await
            .map_err(db_err)?;
        let mut out = Vec::new();
        let mut malformed = Vec::new();
        let mut next_cursor = None;
        let mut scanned = 0usize;
        let mut has_more = false;
        while let Some(row) = rows.next().await.map_err(db_err)? {
            if scanned == limit {
                has_more = true;
                break;
            }
            scanned += 1;
            let encoded: String = row.get(0).map_err(db_err)?;
            let node_id: String = row.get(1).map_err(db_err)?;
            let node_epoch: String = row.get(2).map_err(db_err)?;
            let claim_epoch: i64 = row.get(3).map_err(db_err)?;
            let owner_stale: i64 = row.get(4).map_err(db_err)?;
            next_cursor = Some(RoomOrphanScanCursor::from_raw(encoded.clone()));
            if owner_stale != 1 {
                continue;
            }
            let Some(entity) = decode_entity(&encoded, EntityType::RoomActor) else {
                malformed.push((encoded, node_id, node_epoch, claim_epoch));
                continue;
            };
            // XEP-0045 Business Rules / Addresses requires a non-empty Room
            // ID (the node portion); a domain-only BareJid is not a room JID.
            if !matches!(
                entity.id.parse::<jid::BareJid>(),
                Ok(room_jid) if room_jid.node().is_some()
            ) {
                malformed.push((encoded, node_id, node_epoch, claim_epoch));
                continue;
            }
            out.push(OrphanedRoomActorClaim {
                entity,
                epoch: ClaimEpoch(claim_epoch),
                owner: NodeIdentity::new(node_id, node_epoch),
            });
        }
        drop(rows);
        let mut quarantined = 0usize;
        for (encoded, node_id, node_epoch, claim_epoch) in malformed {
            let affected = conn
                .execute(
                    r#"
                    DELETE FROM clustering_claims
                    WHERE entity = ? AND entity_type = ? AND node_id = ? AND node_epoch = ?
                      AND claim_epoch = ?
                      AND NOT EXISTS (
                        SELECT 1 FROM clustering_nodes n
                        WHERE n.node_id = clustering_claims.node_id
                          AND NOT n.expired
                          AND n.node_epoch = clustering_claims.node_epoch
                      )
                    "#,
                    crate::db_params![
                        encoded,
                        EntityType::RoomActor.as_db_str().to_string(),
                        node_id,
                        node_epoch,
                        claim_epoch,
                    ],
                )
                .await
                .map_err(db_err)?;
            quarantined += usize::from(affected == 1);
        }
        if quarantined > 0 {
            tracing::debug!(
                quarantined,
                "quarantined malformed stale RoomActor claim rows"
            );
        }
        Ok(OrphanedRoomActorClaimPage {
            candidates: out,
            next_cursor,
            has_more,
            quarantined,
        })
    }

    async fn steal_orphaned_room_actor_claim(
        &self,
        entity: &Entity,
        observed: ClaimEpoch,
        me: &NodeIdentity,
        lease_ttl: Duration,
    ) -> Result<ClaimEpoch, ClaimError> {
        if entity.entity_type != EntityType::RoomActor {
            return Err(ClaimError::Conflict);
        }
        let conn = self.db.control_plane_guard().await.map_err(db_err)?;
        let mut rows = conn
            .query(
                r#"
                UPDATE clustering_claims
                SET node_id = ?, node_epoch = ?,
                    claim_epoch = nextval('clustering_claim_epoch_seq'::regclass)
                WHERE entity = ?
                  AND entity_type = ?
                  AND claim_epoch = ?
                  AND NOT EXISTS (
                    SELECT 1 FROM clustering_nodes n
                    WHERE n.node_id = clustering_claims.node_id
                      AND NOT n.expired
                      AND n.node_epoch = clustering_claims.node_epoch
                  )
                  AND EXISTS (
                    SELECT 1 FROM clustering_nodes
                    WHERE node_id = ?
                      AND node_epoch = ?
                      AND NOT expired
                      AND NOT draining
                      AND heartbeat >= now() - (? || ' milliseconds')::interval
                  )
                RETURNING claim_epoch
                "#,
                crate::db_params![
                    me.node_id.clone(),
                    me.node_epoch.clone(),
                    entity_key(entity),
                    EntityType::RoomActor.as_db_str().to_string(),
                    observed.0,
                    me.node_id.clone(),
                    me.node_epoch.clone(),
                    lease_ttl.as_millis().to_string(),
                ],
            )
            .await
            .map_err(db_err)?;
        match rows.next().await.map_err(db_err)? {
            Some(row) => Ok(ClaimEpoch(row.get::<i64>(0).map_err(db_err)?)),
            None => Err(ClaimError::Conflict),
        }
    }

    async fn current_generation(&self) -> Result<Option<String>, ClaimError> {
        let conn = self.db.control_plane_guard().await.map_err(db_err)?;
        // Q5's operational definition: the `pod_template_hash` of the
        // non-expired row with the greatest `first_seen`. Draining rows are
        // deliberately NOT excluded here (unlike
        // `count_other_live_nodes`'s isolation-heuristic filter) — a
        // draining node's own registration event is still a real, valid
        // signal about which generation most recently rolled in; excluding
        // it would let a fully-rolled-out new generation whose pods have
        // all since started draining again (e.g. immediately followed by
        // another rollout) fall back to reporting a stale prior
        // generation as "current."
        let mut rows = conn
            .query(
                r#"
                SELECT pod_template_hash
                FROM clustering_nodes
                WHERE NOT expired
                ORDER BY first_seen DESC
                LIMIT 1
                "#,
                (),
            )
            .await
            .map_err(db_err)?;
        match rows.next().await.map_err(db_err)? {
            Some(row) => row.get::<Option<String>>(0).map_err(db_err),
            None => Ok(None),
        }
    }
}

/// Serializes every test that touches the shared `clustering_nodes`/
/// `clustering_claims` tables so a concurrently seeded/wiped row from one
/// test cannot leak into another — mirrors [`super::allowlist_table_lock`]
/// for the same reason, one table pair over. Module-scope (not nested in
/// `mod tests`) and `pub(crate)` so `self_fence.rs`'s Postgres-gated
/// lifecycle test (FIX 1(d)) can serialize against these tests too: both
/// modules' tests share the same two tables in the same test binary, and
/// `cargo test` runs tests within a binary concurrently by default.
#[cfg(test)]
pub(crate) fn clustering_control_plane_table_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{DatabaseConfig, DatabaseDriver};
    use std::sync::Arc;
    use waddle_xmpp::ownership::SharedNodeIdentity;

    fn node_identity() -> NodeIdentity {
        NodeIdentity::new(
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
        )
    }

    fn sm_entity(id: &str) -> Entity {
        Entity::new(EntityType::SmSession, id.to_string())
    }

    #[test]
    fn orphan_reaper_cursor_updates_preserve_their_lane_when_set_or_reset() {
        let sm = SmOrphanScanCursor::from_raw("sm_session:page-064".to_string());
        let room =
            RoomOrphanScanCursor::from_raw("room_actor:page-064@muc.example.com".to_string());
        assert_eq!(
            OrphanReaperCursorUpdate::SmSession(Some(sm)).into_db_parts(),
            (
                EntityType::SmSession,
                Some("sm_session:page-064".to_string())
            )
        );
        assert_eq!(
            OrphanReaperCursorUpdate::RoomActor(Some(room)).into_db_parts(),
            (
                EntityType::RoomActor,
                Some("room_actor:page-064@muc.example.com".to_string())
            )
        );
        assert_eq!(
            OrphanReaperCursorUpdate::SmSession(None).into_db_parts(),
            (EntityType::SmSession, None)
        );
        assert_eq!(
            OrphanReaperCursorUpdate::RoomActor(None).into_db_parts(),
            (EntityType::RoomActor, None)
        );
    }

    async fn clean_store() -> Option<PostgresClaimStore> {
        let url = std::env::var("WADDLE_TEST_POSTGRES_URL").ok()?;
        let db = Database::from_config(
            "clustering-claims-test",
            &DatabaseConfig::new(DatabaseDriver::Postgres, url)
                .with_control_plane_pool(crate::db::DEFAULT_CONTROL_PLANE_POOL_SIZE),
        )
        .await
        .expect("open test postgres");
        let store = PostgresClaimStore::new(db.clone());
        store.ensure_schema().await.expect("ensure schema");
        crate::sm_persistence_fenced::PostgresFencedSmPersistence::open(
            db.clone(),
            Arc::new(PostgresClaimStore::new(db.clone())),
            SharedNodeIdentity::new(node_identity()),
        )
        .await
        .expect("ensure SM persistence schema");
        let conn = db.guard().await.expect("guard");
        conn.execute("DELETE FROM sm_unacked", ())
            .await
            .expect("clean sm_unacked");
        conn.execute("DELETE FROM sm_sessions", ())
            .await
            .expect("clean sm_sessions");
        conn.execute("DELETE FROM clustering_claims", ())
            .await
            .expect("clean claims");
        conn.execute("DELETE FROM clustering_orphan_reaper_cursors", ())
            .await
            .expect("clean orphan reaper cursors");
        conn.execute("DELETE FROM clustering_nodes", ())
            .await
            .expect("clean nodes");
        conn.execute("DELETE FROM clustering_steal_intents", ())
            .await
            .expect("clean steal intents");
        Some(store)
    }

    #[tokio::test]
    async fn durable_orphan_reaper_cursors_are_lane_typed_and_reset_independently() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let sm = SmOrphanScanCursor::from_raw("sm_session:durable-064".to_string());
        let room =
            RoomOrphanScanCursor::from_raw("room_actor:durable-064@muc.example.com".to_string());
        store
            .persist_orphan_reaper_cursor(OrphanReaperCursorUpdate::SmSession(Some(sm.clone())))
            .await
            .expect("persist SM cursor");
        store
            .persist_orphan_reaper_cursor(OrphanReaperCursorUpdate::RoomActor(Some(room.clone())))
            .await
            .expect("persist room cursor");

        assert_eq!(
            store
                .load_sm_orphan_reaper_cursor()
                .await
                .expect("load SM cursor"),
            Some(sm)
        );
        assert_eq!(
            store
                .load_room_orphan_reaper_cursor()
                .await
                .expect("load room cursor"),
            Some(room.clone())
        );

        store
            .persist_orphan_reaper_cursor(OrphanReaperCursorUpdate::SmSession(None))
            .await
            .expect("reset SM cursor");
        assert_eq!(
            store
                .load_sm_orphan_reaper_cursor()
                .await
                .expect("load reset SM cursor"),
            None
        );
        assert_eq!(
            store
                .load_room_orphan_reaper_cursor()
                .await
                .expect("room cursor survives SM reset"),
            Some(room)
        );
    }

    /// Seed a `clustering_nodes` row directly with a caller-chosen `expired`
    /// flag. `NodeLeaseStore` (below, this same file) exists, but its own
    /// `register`/`heartbeat` API always stamps `heartbeat = now()` and has
    /// no way to synthesize an already-`expired` — or already-stale — row
    /// without a test actually waiting out a real TTL window; this helper
    /// is the direct-SQL shortcut for the "owner already committed-expired"
    /// fixture shape the steal-CAS tests need, mirroring how
    /// `allowlist.rs`'s tests seed rows directly for shapes its own store
    /// API has no fast path for.
    ///
    /// `expired` is spliced into the SQL text as a literal, not bound as a
    /// parameter: Postgres types every bind parameter by its Rust source
    /// type (`bool` binds as `bigint` through this crate's `Value`
    /// abstraction, per `db/value.rs`'s `From<bool>`), and a `bigint`
    /// parameter cannot implicitly cast to a `boolean` column — the same
    /// reason `lease.rs`'s own CAS statements always write `expired`/`true`/
    /// `false` as SQL literals, never as bound parameters.
    async fn seed_node(db: &Database, identity: &NodeIdentity, expired: bool) {
        let conn = db.guard().await.expect("guard");
        let expired_literal = if expired { "true" } else { "false" };
        conn.execute(
            &format!(
                "INSERT INTO clustering_nodes (node_id, node_epoch, expired) VALUES (?, ?, {expired_literal})"
            ),
            crate::db_params![identity.node_id.clone(), identity.node_epoch.clone()],
        )
        .await
        .expect("seed node");
    }

    async fn backdate_heartbeat(db: &Database, identity: &NodeIdentity) {
        let conn = db.guard().await.expect("guard");
        conn.execute(
            r#"
            UPDATE clustering_nodes
            SET heartbeat = now() - interval '1 hour'
            WHERE node_id = ? AND node_epoch = ?
            "#,
            crate::db_params![identity.node_id.clone(), identity.node_epoch.clone()],
        )
        .await
        .expect("backdate heartbeat");
    }

    async fn seed_sm_session_row(db: &Database, stream_id: &str) {
        let conn = db.guard().await.expect("guard");
        conn.execute(
            r#"
            INSERT INTO sm_sessions (
                stream_id, user_id, full_jid, inbound_count, outbound_count,
                last_acked, max_resume_secs, detached_at_ms, max_resume_duration_ms,
                carbons_enabled, roster_interested, blocklist_interested,
                presence_available, presence_priority
            ) VALUES (?, ?, ?, 0, 0, 0, NULL, 0, 60000, 0, 0, 0, 0, 0)
            ON CONFLICT (stream_id) DO NOTHING
            "#,
            crate::db_params![
                stream_id.to_string(),
                "alice".to_string(),
                "alice@example.com/web".to_string(),
            ],
        )
        .await
        .expect("seed sm_sessions row");
    }

    #[tokio::test]
    async fn acquire_succeeds_once_then_conflicts() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let me = node_identity();
        let entity = sm_entity("stream-1");
        let epoch = store.acquire(&entity, &me).await.expect("first acquire");
        assert!(store.fence(&entity, &me, epoch).await.expect("fence"));

        let other = node_identity();
        let err = store
            .acquire(&entity, &other)
            .await
            .expect_err("second acquire loses the race");
        assert!(matches!(err, ClaimError::AlreadyClaimed));
    }

    /// FIX 1: `ensure_claimed` on a not-yet-claimed entity is a plain fresh
    /// acquire.
    #[tokio::test]
    async fn ensure_claimed_acquires_fresh_when_unclaimed() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let me = node_identity();
        let entity = sm_entity("ensure-claimed-fresh");
        let epoch = store
            .ensure_claimed(&entity, &me)
            .await
            .expect("ensure_claimed acquires fresh");
        assert!(store.fence(&entity, &me, epoch).await.expect("fence"));
    }

    /// FIX 1: a second `ensure_claimed` call under the exact same
    /// `NodeIdentity` that already holds the claim (e.g. two concurrent
    /// first-writes for the same not-yet-claimed stream_id) must observe the
    /// existing epoch rather than `AlreadyClaimed` — this is the whole point
    /// of the fix, closing the spurious-conflict failure mode the original
    /// bare-`acquire` design hit.
    #[tokio::test]
    async fn ensure_claimed_is_idempotent_for_the_same_node_and_epoch() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let me = node_identity();
        let entity = sm_entity("ensure-claimed-idempotent");
        let first = store
            .ensure_claimed(&entity, &me)
            .await
            .expect("first ensure_claimed acquires");
        let second = store
            .ensure_claimed(&entity, &me)
            .await
            .expect("second ensure_claimed under the same identity self-reacquires");
        assert_eq!(first, second);
    }

    /// FIX 1: `ensure_claimed` under a genuinely different node/epoch than
    /// the current owner must still reject exactly like `acquire` does — the
    /// self-reacquire path is scoped to the *exact same* identity, never a
    /// blanket idempotent success.
    #[tokio::test]
    async fn ensure_claimed_rejects_a_foreign_owner() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let owner = node_identity();
        let entity = sm_entity("ensure-claimed-foreign");
        store
            .ensure_claimed(&entity, &owner)
            .await
            .expect("owner's ensure_claimed acquires");

        let foreign = node_identity();
        let err = store
            .ensure_claimed(&entity, &foreign)
            .await
            .expect_err("a different node/epoch must not self-reacquire");
        assert!(matches!(err, ClaimError::AlreadyClaimed));
    }

    /// FIX 1: two concurrent first-writes racing `ensure_claimed` for the
    /// same fresh entity, under the same `NodeIdentity` (the fenced SM
    /// persistence's own use case — one process, one identity, two
    /// concurrent tasks writing the same not-yet-claimed stream_id), must
    /// both succeed, and exactly one `clustering_claims` row must exist for
    /// the entity afterward.
    #[tokio::test]
    async fn ensure_claimed_concurrent_first_writes_both_succeed_exactly_one_row() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(url) = std::env::var("WADDLE_TEST_POSTGRES_URL").ok() else {
            return;
        };
        let db = Database::from_config(
            "clustering-claims-test-concurrent",
            &DatabaseConfig::new(DatabaseDriver::Postgres, url)
                .with_control_plane_pool(crate::db::DEFAULT_CONTROL_PLANE_POOL_SIZE),
        )
        .await
        .expect("open test postgres");
        let store = PostgresClaimStore::new(db.clone());
        store.ensure_schema().await.expect("ensure schema");
        let conn = db.guard().await.expect("guard");
        conn.execute("DELETE FROM clustering_claims", ())
            .await
            .expect("clean claims");
        conn.execute("DELETE FROM clustering_nodes", ())
            .await
            .expect("clean nodes");

        let me = node_identity();
        let entity = sm_entity("ensure-claimed-concurrent");
        let store = std::sync::Arc::new(store);

        let (store_a, entity_a, me_a) = (store.clone(), entity.clone(), me.clone());
        let (store_b, entity_b, me_b) = (store.clone(), entity.clone(), me.clone());
        let task_a = tokio::spawn(async move { store_a.ensure_claimed(&entity_a, &me_a).await });
        let task_b = tokio::spawn(async move { store_b.ensure_claimed(&entity_b, &me_b).await });
        let (result_a, result_b) = tokio::join!(task_a, task_b);
        let epoch_a = result_a
            .expect("task a join")
            .expect("task a ensure_claimed");
        let epoch_b = result_b
            .expect("task b join")
            .expect("task b ensure_claimed");
        assert_eq!(epoch_a, epoch_b);

        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM clustering_claims WHERE entity = ?",
                crate::db_params![entity_key(&entity)],
            )
            .await
            .expect("count query");
        let count: i64 = rows
            .next()
            .await
            .expect("row")
            .expect("row present")
            .get(0)
            .expect("count column");
        assert_eq!(count, 1, "exactly one claims row must exist for the entity");
    }

    #[tokio::test]
    async fn acquire_does_not_collide_across_entity_types_sharing_the_same_id() {
        // Fix: the `entity` primary key previously bound `entity.id` alone
        // (entity_type was write-only), so a `UserActor` and a `RoomActor`
        // sharing the same id would collide on one row. `entity_key`'s
        // type-tag prefix must keep them as two distinct claims.
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let me = node_identity();
        let user_entity = Entity::new(EntityType::UserActor, "42");
        let room_entity = Entity::new(EntityType::RoomActor, "42");
        let sm_entity_same_id = Entity::new(EntityType::SmSession, "42");

        let user_epoch = store
            .acquire(&user_entity, &me)
            .await
            .expect("acquire user_actor:42");
        let room_epoch = store
            .acquire(&room_entity, &me)
            .await
            .expect("acquire room_actor:42 must not collide with user_actor:42");
        let sm_epoch = store
            .acquire(&sm_entity_same_id, &me)
            .await
            .expect("acquire sm_session:42 must not collide with either of the above");

        assert!(store
            .fence(&user_entity, &me, user_epoch)
            .await
            .expect("fence user_actor:42"));
        assert!(store
            .fence(&room_entity, &me, room_epoch)
            .await
            .expect("fence room_actor:42"));
        assert!(store
            .fence(&sm_entity_same_id, &me, sm_epoch)
            .await
            .expect("fence sm_session:42"));

        // Releasing one must not affect the others.
        store
            .release(&user_entity, &me, user_epoch)
            .await
            .expect("release user_actor:42");
        assert!(!store
            .fence(&user_entity, &me, user_epoch)
            .await
            .expect("fence user_actor:42 after release"));
        assert!(store
            .fence(&room_entity, &me, room_epoch)
            .await
            .expect("room_actor:42 untouched by user_actor:42's release"));
        assert!(store
            .fence(&sm_entity_same_id, &me, sm_epoch)
            .await
            .expect("sm_session:42 untouched by user_actor:42's release"));
    }

    #[tokio::test]
    async fn steal_stale_ignores_raw_heartbeat_only_committed_expired_flag_matters() {
        // Named test: proves the owner-stale predicate reads only the
        // committed `expired` flag, never a raw `heartbeat < now() - ttl`
        // comparison — an old heartbeat with `expired = false` must NOT be
        // stealable.
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let owner = node_identity();
        let entity = sm_entity("stream-1");
        let epoch0 = store.acquire(&entity, &owner).await.expect("acquire");

        // The owner's node row is fresh (expired = false); its heartbeat
        // is irrelevant because the predicate never inspects it directly.
        seed_node(&store.db, &owner, false).await;

        let stealer = node_identity();
        seed_node(&store.db, &stealer, false).await;
        let conflict = store
            .steal_stale(&entity, epoch0, StalePredicate::OwnerStale, &stealer)
            .await
            .expect_err("fresh (non-expired) owner cannot be stolen from");
        assert!(matches!(conflict, ClaimError::Conflict));

        // Only once the owner's row is committed `expired = true` does the
        // steal succeed.
        store
            .db
            .guard()
            .await
            .expect("guard")
            .execute(
                "UPDATE clustering_nodes SET expired = true WHERE node_id = ?",
                crate::db_params![owner.node_id.clone()],
            )
            .await
            .expect("mark owner expired");

        let epoch1 = store
            .steal_stale(&entity, epoch0, StalePredicate::OwnerStale, &stealer)
            .await
            .expect("steal succeeds once the owner is committed-expired");
        assert!(epoch1.0 > epoch0.0);
        assert!(store.fence(&entity, &stealer, epoch1).await.expect("fence"));
    }

    #[tokio::test]
    async fn steal_from_vanished_node_succeeds_via_not_exists() {
        // No `clustering_nodes` row at all for the owner: the NOT EXISTS
        // predicate is vacuously true, so the claim is stealable exactly
        // as if the owner were committed-expired.
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let owner = node_identity();
        let entity = sm_entity("stream-1");
        let epoch0 = store.acquire(&entity, &owner).await.expect("acquire");

        let stealer = node_identity();
        seed_node(&store.db, &stealer, false).await;
        let epoch1 = store
            .steal_stale(&entity, epoch0, StalePredicate::OwnerStale, &stealer)
            .await
            .expect("steal from a node with no nodes-row succeeds");
        assert!(epoch1.0 > epoch0.0);
    }

    #[tokio::test]
    async fn steal_stale_with_stale_epoch_loses_the_race() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let owner = node_identity();
        let entity = sm_entity("stream-1");
        store.acquire(&entity, &owner).await.expect("acquire");

        let stealer = node_identity();
        seed_node(&store.db, &stealer, false).await;
        let err = store
            .steal_stale(
                &entity,
                ClaimEpoch(99),
                StalePredicate::OwnerStale,
                &stealer,
            )
            .await
            .expect_err("wrong observed epoch loses");
        assert!(matches!(err, ClaimError::Conflict));
    }

    fn user_actor_entity(id: &str) -> Entity {
        Entity::new(EntityType::UserActor, id.to_string())
    }

    #[tokio::test]
    async fn steal_stale_rejects_steal_intent_expired_for_sm_session() {
        // Rule 1 of the three-rule steal-variant block: steal-intents never
        // touch SM-session claims, enforced defensively inside `steal_stale`
        // itself (belt-and-suspenders alongside `report_steal_intent` never
        // letting such a row exist).
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let owner = node_identity();
        let entity = sm_entity("stream-1");
        let epoch0 = store.acquire(&entity, &owner).await.expect("acquire");

        let stealer = node_identity();
        let err = store
            .steal_stale(
                &entity,
                epoch0,
                StalePredicate::StealIntentExpired {
                    intent_ttl: std::time::Duration::from_millis(1),
                },
                &stealer,
            )
            .await
            .expect_err("SM-session claims are excluded from the steal-intent path");
        assert!(matches!(err, ClaimError::SmSessionExcludedFromStealIntent));
    }

    #[tokio::test]
    async fn steal_for_resume_succeeds_against_a_fresh_owner() {
        // The whole point of the consent/epoch-only CAS: it steals from a
        // perfectly fresh (non-expired) owner, which `steal_stale` never
        // could.
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let owner = node_identity();
        let entity = sm_entity("stream-1");
        let epoch0 = store.acquire(&entity, &owner).await.expect("acquire");
        seed_node(&store.db, &owner, false).await;

        let stealer = node_identity();
        let jid: jid::BareJid = "alice@example.com".parse().expect("valid jid");
        let proof =
            waddle_xmpp::ownership::verify_resume_identity(&jid, &jid).expect("identity match");
        let epoch1 = store
            .steal_for_resume(&entity, epoch0, proof, &stealer)
            .await
            .expect("consent CAS steals from a fresh owner");
        assert!(epoch1.0 > epoch0.0);
    }

    #[tokio::test]
    async fn steal_for_resume_with_stale_epoch_loses_the_race() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let owner = node_identity();
        let entity = sm_entity("stream-1");
        store.acquire(&entity, &owner).await.expect("acquire");

        let stealer = node_identity();
        let jid: jid::BareJid = "alice@example.com".parse().expect("valid jid");
        let proof =
            waddle_xmpp::ownership::verify_resume_identity(&jid, &jid).expect("identity match");
        let err = store
            .steal_for_resume(&entity, ClaimEpoch(42), proof, &stealer)
            .await
            .expect_err("wrong observed epoch loses");
        assert!(matches!(err, ClaimError::Conflict));
    }

    #[tokio::test]
    async fn fence_reflects_current_owner_and_epoch_only() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let owner = node_identity();
        let entity = sm_entity("stream-1");
        let epoch0 = store.acquire(&entity, &owner).await.expect("acquire");
        assert!(store.fence(&entity, &owner, epoch0).await.expect("fence"));
        assert!(!store
            .fence(&entity, &owner, ClaimEpoch(7))
            .await
            .expect("fence wrong epoch"));

        let stranger = node_identity();
        assert!(!store
            .fence(&entity, &stranger, epoch0)
            .await
            .expect("fence wrong node"));
    }

    #[tokio::test]
    async fn release_is_idempotent_and_exact_release_requires_owner_incarnation_and_epoch() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let owner = node_identity();
        let entity = sm_entity("stream-1");
        let epoch0 = store.acquire(&entity, &owner).await.expect("acquire");

        store
            .release(&entity, &owner, ClaimEpoch(99))
            .await
            .expect("idempotent stale release");
        assert_eq!(
            store
                .release_exact(&entity, &owner, ClaimEpoch(99))
                .await
                .expect("exact stale release"),
            waddle_xmpp::ownership::ExactReleaseOutcome::NotOwned
        );
        assert!(store.fence(&entity, &owner, epoch0).await.expect("fence"));

        let replacement = NodeIdentity::new(owner.node_id.clone(), "replacement-incarnation");
        assert!(!store
            .fence(&entity, &replacement, epoch0)
            .await
            .expect("replacement fence"));
        assert_eq!(
            store
                .release_exact(&entity, &replacement, epoch0)
                .await
                .expect("exact release"),
            waddle_xmpp::ownership::ExactReleaseOutcome::NotOwned
        );
        assert!(store.fence(&entity, &owner, epoch0).await.expect("fence"));

        store
            .release(&entity, &owner, epoch0)
            .await
            .expect("release under the right epoch");
        assert!(!store
            .fence(&entity, &owner, epoch0)
            .await
            .expect("fence after release"));

        store
            .release(&entity, &owner, epoch0)
            .await
            .expect("repeat release is idempotent");
    }

    #[tokio::test]
    async fn delete_and_recreate_never_reuses_a_claim_generation() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let owner = node_identity();
        let entity = room_entity("generation-recreate@muc.example.com");
        let first = store.acquire(&entity, &owner).await.expect("first acquire");
        assert_eq!(
            store
                .release_exact(&entity, &owner, first)
                .await
                .expect("exact release"),
            waddle_xmpp::ownership::ExactReleaseOutcome::Released
        );

        let recreated = store
            .acquire(&entity, &owner)
            .await
            .expect("recreate under the same node incarnation");
        assert!(recreated.0 > first.0);
        assert_eq!(
            store
                .release_exact(&entity, &owner, first)
                .await
                .expect("stale exact release"),
            waddle_xmpp::ownership::ExactReleaseOutcome::NotOwned
        );
        assert!(store
            .fence(&entity, &owner, recreated)
            .await
            .expect("recreated claim remains held"));
    }

    #[tokio::test]
    async fn concurrent_schema_setup_advances_sequence_past_existing_generations() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let conn = store.db.guard().await.expect("guard");
        let mut rows = conn
            .query("SELECT last_value FROM clustering_claim_epoch_seq", ())
            .await
            .expect("sequence value");
        let last = rows
            .next()
            .await
            .expect("sequence row")
            .expect("sequence row present")
            .get::<i64>(0)
            .expect("last_value");
        let legacy_epoch = last.checked_add(1_000).expect("test sequence headroom");
        let legacy = room_entity("legacy-generation@muc.example.com");
        let owner = node_identity();
        conn.execute(
            r#"
            INSERT INTO clustering_claims
                (entity, entity_type, node_id, node_epoch, claim_epoch)
            VALUES (?, ?, ?, ?, ?)
            "#,
            crate::db_params![
                entity_key(&legacy),
                EntityType::RoomActor.as_db_str().to_string(),
                owner.node_id.clone(),
                owner.node_epoch.clone(),
                legacy_epoch,
            ],
        )
        .await
        .expect("seed pre-sequence generation");

        let store_a = PostgresClaimStore::new(store.db.clone());
        let store_b = PostgresClaimStore::new(store.db.clone());
        let (a, b) = tokio::join!(store_a.ensure_schema(), store_b.ensure_schema());
        a.expect("first concurrent schema setup");
        b.expect("second concurrent schema setup");

        conn.execute(
            "DELETE FROM clustering_claims WHERE entity = ?",
            crate::db_params![entity_key(&legacy)],
        )
        .await
        .expect("remove legacy row");
        let fresh = room_entity("post-migration-generation@muc.example.com");
        let fresh_epoch = store
            .acquire(&fresh, &owner)
            .await
            .expect("acquire after schema migration");
        assert!(fresh_epoch.0 > legacy_epoch);
    }

    #[tokio::test]
    async fn release_many_clears_every_owned_entity_in_one_round_trip() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let owner = node_identity();
        let a = sm_entity("stream-a");
        let b = sm_entity("stream-b");
        let c = sm_entity("stream-c");
        let a_epoch = store.acquire(&a, &owner).await.expect("acquire a");
        let b_epoch = store.acquire(&b, &owner).await.expect("acquire b");
        // c is owned by someone else — release_many must not touch it.
        let other = node_identity();
        let c_epoch = store.acquire(&c, &other).await.expect("acquire c");

        store
            .release_many(&[a.clone(), b.clone(), c.clone()], &owner)
            .await
            .expect("release_many");

        assert!(!store.fence(&a, &owner, a_epoch).await.expect("fence a"));
        assert!(!store.fence(&b, &owner, b_epoch).await.expect("fence b"));
        assert!(store
            .fence(&c, &other, c_epoch)
            .await
            .expect("fence c: untouched, still owned by other"));
    }

    #[tokio::test]
    async fn release_many_on_an_empty_slice_is_a_no_op() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        store
            .release_many(&[], &node_identity())
            .await
            .expect("empty release_many does not error");
    }

    // ADR-0017 Phase 3 Slice 10, `ClaimStore::release_many`'s own
    // "plan-sanctioned ABA window" doc comment: literal proof, at the SQL
    // level, that `release_many` is blind to an entity's individual
    // `claim_epoch` and matches only `(node_id, node_epoch)` — so a batch
    // entry queued for release, then legitimately re-claimed by the SAME
    // node/epoch at a HIGHER epoch before the batched DELETE actually
    // runs (the doc comment's own example: "a resumed XEP-0198 session
    // legitimately steals back onto this node via `steal_for_resume`"),
    // is deleted by the stale batch entry regardless. This is the
    // documented, accepted risk — not a bug — and Slice 10's own
    // `crate::clustering::drain::tests::
    // drain_seals_then_releases_only_room_entities_skipping_sm_sessions`
    // proves the mitigation: `SmSession` entities (the only ones reachable
    // via `steal_for_resume`) never enter a `release_many` batch in the
    // first place, so this window is real at the store level but
    // unreachable through the production drain path.
    #[tokio::test]
    async fn release_many_epoch_blind_window_deletes_a_fresh_same_node_resume() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let me = node_identity();
        let entity = sm_entity("stream-aba");
        let epoch0 = store.acquire(&entity, &me).await.expect("initial acquire");

        // Drain decides to release this entity (its final write already
        // committed) and queues it for the batch...
        let batch = vec![entity.clone()];

        // ...but before `release_many` actually runs, the SAME node
        // legitimately re-wins this exact entity at a HIGHER epoch via the
        // one CAS variant the draining gate does not cover
        // (`steal_for_resume` — a resumed session landing back on this
        // node while it drains but has not yet exited).
        let jid: jid::BareJid = "resuming@example.com".parse().expect("valid jid");
        let proof =
            waddle_xmpp::ownership::verify_resume_identity(&jid, &jid).expect("identity match");
        let epoch1 = store
            .steal_for_resume(&entity, epoch0, proof, &me)
            .await
            .expect("same-node resume steal succeeds (no staleness required)");
        assert!(epoch1.0 > epoch0.0, "epoch bumped by the resume steal");
        assert!(
            store
                .fence(&entity, &me, epoch1)
                .await
                .expect("fence check"),
            "the entity is genuinely, freshly re-claimed under a new generation"
        );

        // The stale batch entry's `release_many` call is blind to that
        // epoch bump — it deletes the fresh claim anyway.
        store.release_many(&batch, &me).await.expect("release_many");

        assert!(
            !store
                .fence(&entity, &me, epoch1)
                .await
                .expect("fence check"),
            "release_many's epoch-blind DELETE removes the fresh, genuinely-live claim too — \
             the documented ABA window, reproduced here at the SQL level"
        );
    }

    // The ADR-named interleaving race: a steal CAS commits while a
    // concurrent transaction holds the claims row's `FOR SHARE` lock (the
    // exact fencing-transaction shape later slices use to guard durable
    // writes). The steal's `UPDATE` must block until the `FOR SHARE`
    // transaction commits, then re-evaluate its predicate against the
    // latest committed row (EvalPlanQual) — proving the locking model
    // later fenced-write slices (4/7/8) depend on actually holds here.
    #[tokio::test]
    async fn steal_commit_interleaved_inside_a_fenced_transaction() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let owner = node_identity();
        let entity = sm_entity("stream-1");
        let epoch0 = store.acquire(&entity, &owner).await.expect("acquire");
        seed_node(&store.db, &owner, true).await; // owner already stale

        // Open a fencing-shaped transaction and take the `FOR SHARE` lock
        // on the claims row, but do not commit yet.
        let mut fencing_tx = store.db.begin().await.expect("begin fencing tx");
        let held = fencing_tx
            .query(
                "SELECT 1 FROM clustering_claims WHERE entity = ? AND node_id = ? AND node_epoch = ? AND claim_epoch = ? FOR SHARE",
                crate::db_params![
                    entity_key(&entity),
                    owner.node_id.clone(),
                    owner.node_epoch.clone(),
                    epoch0.0,
                ],
            )
            .await
            .expect("fencing select")
            .next()
            .await
            .expect("row present")
            .is_some();
        assert!(held, "fencing SELECT must observe the still-fresh claim");

        // Race a steal against the locked row on a separate connection.
        let stealer = node_identity();
        seed_node(&store.db, &stealer, false).await;
        let store_db = store.db.clone();
        let entity_clone = entity.clone();
        let steal_task = tokio::spawn(async move {
            let store = PostgresClaimStore::new(store_db);
            store
                .steal_stale(&entity_clone, epoch0, StalePredicate::OwnerStale, &stealer)
                .await
        });

        // Give the steal a moment to actually block on the row lock before
        // releasing it.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        fencing_tx.commit().await.expect("commit fencing tx");

        let stolen_epoch = steal_task
            .await
            .expect("steal task join")
            .expect("steal succeeds once the FOR SHARE lock is released");
        assert!(stolen_epoch.0 > epoch0.0);

        // The original owner's fencing check against its old epoch now
        // observes zero rows — it has been fenced out.
        assert!(!store
            .fence(&entity, &owner, epoch0)
            .await
            .expect("owner fenced out after the steal"));
    }

    // --- NodeLeaseStore (ADR-0017 Phase 3 Slice 2) ---

    const NODE_LEASE_TTL: Duration = Duration::from_secs(30);

    #[tokio::test]
    async fn register_then_heartbeat_renews_the_lease() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let me = node_identity();
        store
            .register(&me, Some("gen-1".to_string()))
            .await
            .expect("register");
        let renewed = store
            .heartbeat(&me, NODE_LEASE_TTL)
            .await
            .expect("heartbeat call succeeds");
        assert!(renewed, "a freshly registered node must renew successfully");
    }

    #[tokio::test]
    async fn heartbeat_returns_false_not_err_for_an_unregistered_node() {
        // Fencing loss is a normal `Ok(false)` outcome, never an error —
        // the trait's own doc comment: "false ⇒ fencing loss".
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let stranger = node_identity();
        let renewed = store
            .heartbeat(&stranger, NODE_LEASE_TTL)
            .await
            .expect("heartbeat against an unknown node is not an error");
        assert!(!renewed);
    }

    #[tokio::test]
    async fn register_is_idempotent_and_refreshes_the_epoch() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let node_id = uuid::Uuid::new_v4().to_string();
        let first = NodeIdentity::new(node_id.clone(), uuid::Uuid::new_v4().to_string());
        store
            .register(&first, None)
            .await
            .expect("first registration");

        // Re-registration under the SAME node_id but a fresh epoch (the
        // post-fence re-registration shape) must supersede the old epoch:
        // the old epoch's heartbeat must now fail.
        let second = NodeIdentity::new(node_id, uuid::Uuid::new_v4().to_string());
        store
            .register(&second, None)
            .await
            .expect("re-registration");

        assert!(!store
            .heartbeat(&first, NODE_LEASE_TTL)
            .await
            .expect("heartbeat call succeeds"));
        assert!(store
            .heartbeat(&second, NODE_LEASE_TTL)
            .await
            .expect("heartbeat call succeeds"));
    }

    #[tokio::test]
    async fn expire_commits_the_flag_and_is_idempotent_once_true() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let owner = node_identity();
        let short_ttl = Duration::from_millis(200);
        store.register(&owner, None).await.expect("register");

        // Not yet lapsed: expire must not flip the flag.
        assert!(!store
            .expire(&owner, short_ttl)
            .await
            .expect("expire call succeeds"));

        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(
            store
                .expire(&owner, short_ttl)
                .await
                .expect("expire call succeeds"),
            "a lapsed heartbeat must commit expired = true"
        );
        // Idempotent: calling again observes the already-committed flag.
        assert!(store
            .expire(&owner, short_ttl)
            .await
            .expect("expire call succeeds"));
    }

    #[tokio::test]
    async fn expire_of_an_unknown_node_is_vacuously_true() {
        // Mirrors `steal_stale`'s NOT EXISTS treatment of a vanished node:
        // nothing to expire, so the caller may proceed as if it already
        // observed a committed-expired row.
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let stranger = node_identity();
        assert!(store
            .expire(&stranger, NODE_LEASE_TTL)
            .await
            .expect("expire call succeeds"));
    }

    #[tokio::test]
    async fn list_heartbeat_stale_nodes_is_bounded_and_defers_authority_to_expire() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let fresh = node_identity();
        let stale_a = node_identity();
        let stale_b = node_identity();
        let stale_c = node_identity();
        let already_expired = node_identity();

        for identity in [&fresh, &stale_a, &stale_b, &stale_c] {
            store.register(identity, None).await.expect("register node");
        }
        seed_node(&store.db, &already_expired, true).await;

        for identity in [&stale_a, &stale_b, &stale_c, &already_expired] {
            backdate_heartbeat(&store.db, identity).await;
        }

        let bounded = store
            .list_heartbeat_stale_nodes(NODE_LEASE_TTL, 2)
            .await
            .expect("list heartbeat-stale nodes with bound");
        assert_eq!(
            bounded.len(),
            2,
            "the watchdog candidate scan must respect its per-sweep bound"
        );

        let candidates = store
            .list_heartbeat_stale_nodes(NODE_LEASE_TTL, 10)
            .await
            .expect("list all heartbeat-stale candidates");
        assert!(
            candidates.contains(&stale_a)
                && candidates.contains(&stale_b)
                && candidates.contains(&stale_c),
            "non-expired heartbeat-stale rows must be listed as watchdog candidates"
        );
        assert!(
            !candidates.contains(&fresh),
            "fresh heartbeat rows must not be listed"
        );
        assert!(
            !candidates.contains(&already_expired),
            "already-expired rows must not be listed"
        );

        assert!(
            store
                .expire(&stale_a, NODE_LEASE_TTL)
                .await
                .expect("expire stale candidate"),
            "expire remains the authoritative false-to-true transition"
        );
        let after_expire = store
            .list_heartbeat_stale_nodes(NODE_LEASE_TTL, 10)
            .await
            .expect("list heartbeat-stale candidates after expire");
        assert!(
            !after_expire.contains(&stale_a),
            "a node committed expired by expire() must disappear from the candidate list"
        );
    }

    #[tokio::test]
    async fn node_lease_is_fresh_requires_matching_fresh_non_draining_row() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let fresh = node_identity();
        store
            .register(&fresh, None)
            .await
            .expect("register fresh node");
        assert!(
            store
                .is_fresh(&fresh, NODE_LEASE_TTL)
                .await
                .expect("freshness query"),
            "a newly registered node must be fresh"
        );

        let wrong_epoch =
            NodeIdentity::new(fresh.node_id.clone(), uuid::Uuid::new_v4().to_string());
        assert!(
            !store
                .is_fresh(&wrong_epoch, NODE_LEASE_TTL)
                .await
                .expect("wrong-epoch freshness query"),
            "the node epoch must match"
        );

        let stale = node_identity();
        store
            .register(&stale, None)
            .await
            .expect("register stale node");
        backdate_heartbeat(&store.db, &stale).await;
        assert!(
            !store
                .is_fresh(&stale, NODE_LEASE_TTL)
                .await
                .expect("heartbeat-stale freshness query"),
            "a heartbeat-stale node must not be fresh before another sweep commits expired"
        );

        let expired = node_identity();
        seed_node(&store.db, &expired, true).await;
        assert!(
            !store
                .is_fresh(&expired, NODE_LEASE_TTL)
                .await
                .expect("expired freshness query"),
            "a committed-expired node must not be fresh"
        );

        let draining = node_identity();
        store
            .register(&draining, None)
            .await
            .expect("register draining node");
        store.mark_draining(&draining).await.expect("mark draining");
        assert!(
            !store
                .is_fresh(&draining, NODE_LEASE_TTL)
                .await
                .expect("draining freshness query"),
            "a draining node must not acquire new orphaned work"
        );
    }

    // FIX 7(b): the ADR-named interleaving race, made genuinely concurrent
    // (not merely sequential) — mirrors
    // `steal_commit_interleaved_inside_a_fenced_transaction` above's
    // hold-a-lock-then-race-a-concurrent-statement pattern, applied to the
    // node-lease heartbeat/expire pair. A hand-rolled transaction performs
    // the expire CAS's exact UPDATE shape and holds the row lock
    // (uncommitted) while a REAL `heartbeat()` call is raced against it on
    // a separate connection via `tokio::spawn` — genuine concurrency, not
    // two sequential calls with a real-time `sleep` between them. Under
    // READ COMMITTED, `heartbeat`'s UPDATE blocks on the row lock; once
    // unblocked by the expire transaction's commit, it re-evaluates its
    // `AND NOT expired` predicate against the now-committed row and must
    // observe zero rows — proving the committed `expired` flag (not a raw
    // heartbeat-freshness read) is the single serialized ordering point,
    // even though the row's heartbeat is otherwise still well within the
    // TTL window at the moment `heartbeat()` was issued.
    #[tokio::test]
    async fn renewal_vs_expire_interleaving_renewal_committed_post_expire_returns_zero_rows() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let owner = node_identity();
        let short_ttl = Duration::from_millis(200);
        store.register(&owner, None).await.expect("register");

        let mut expire_tx = store.db.begin().await.expect("begin expire tx");
        let affected = expire_tx
            .execute(
                r#"
                UPDATE clustering_nodes
                SET expired = true
                WHERE node_id = ? AND node_epoch = ? AND NOT expired
                "#,
                crate::db_params![owner.node_id.clone(), owner.node_epoch.clone()],
            )
            .await
            .expect("expire update takes the row lock");
        assert_eq!(affected, 1, "expire's UPDATE must affect the owner's row");

        let store_db = store.db.clone();
        let owner_clone = owner.clone();
        let heartbeat_task = tokio::spawn(async move {
            let store = PostgresClaimStore::new(store_db);
            store.heartbeat(&owner_clone, short_ttl).await
        });

        // Give the concurrent heartbeat a moment to actually block on the
        // row lock before the expire transaction commits.
        tokio::time::sleep(Duration::from_millis(200)).await;
        expire_tx.commit().await.expect("commit expire tx");

        let renewed = heartbeat_task
            .await
            .expect("heartbeat task join")
            .expect("heartbeat call succeeds");
        assert!(
            !renewed,
            "a renewal blocked behind a concurrently-committing expire must \
             re-evaluate against the committed expired=true row and return \
             zero rows affected — expire-commits-first wins the race even \
             though the row's raw heartbeat is still fresh"
        );
    }

    // NB on scope: the test above proves genuine lock-based interleaving (a
    // real blocked `UPDATE`, unblocked only by a concurrent transaction's
    // commit) — it does NOT prove anything about which of two
    // truly-simultaneous callers "wins" when neither blocks the other
    // (e.g. two heartbeats from two different, legitimately-live
    // connections racing with no shared lock to serialize them); that
    // scenario cannot arise for a single node's own heartbeat in
    // production (a node has exactly one heartbeat loop), so it is out of
    // scope here.

    // Lapsed-lease heartbeat CAS: a node paused (GC/VM stall, or simply not
    // renewing) longer than the TTL observes fencing loss immediately on
    // wake — no grace period, no retry-until-success.
    #[tokio::test]
    async fn lapsed_lease_heartbeat_cas_fences_a_paused_node_on_wake() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let owner = node_identity();
        let short_ttl = Duration::from_millis(150);
        store.register(&owner, None).await.expect("register");
        assert!(store
            .heartbeat(&owner, short_ttl)
            .await
            .expect("initial renewal succeeds"));

        // Simulate a pause longer than the TTL, with nobody explicitly
        // calling `expire` — the heartbeat CAS's own `heartbeat >= now() -
        // ttl` predicate must fail on its own, since the row's heartbeat
        // column is now stale relative to the TTL window.
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(
            !store
                .heartbeat(&owner, short_ttl)
                .await
                .expect("heartbeat call succeeds"),
            "a heartbeat CAS evaluated after the TTL has lapsed must fence, \
             even with no separate expire() call in between"
        );
    }

    #[tokio::test]
    async fn count_other_live_nodes_excludes_self_and_expired_rows() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let me = node_identity();
        let live_peer = node_identity();
        let expired_peer = node_identity();
        store.register(&me, None).await.expect("register me");
        store
            .register(&live_peer, None)
            .await
            .expect("register live peer");
        seed_node(&store.db, &expired_peer, true).await;

        let count = store
            .count_other_live_nodes(&me, NODE_LEASE_TTL)
            .await
            .expect("count call succeeds");
        assert_eq!(
            count, 1,
            "must count the live peer but not self or the expired row"
        );
    }

    // FIX 1(c): a node whose process died without anyone ever calling
    // `expire` on its row (nothing does, in production, this slice) must
    // eventually stop counting as "live" once its heartbeat goes stale —
    // otherwise a hard-killed peer inflates this count forever.
    #[tokio::test]
    async fn count_other_live_nodes_excludes_heartbeat_stale_rows_even_when_not_expired() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let me = node_identity();
        let dead_peer = node_identity();
        let short_ttl = Duration::from_millis(150);
        store.register(&me, None).await.expect("register me");
        store
            .register(&dead_peer, None)
            .await
            .expect("register dead peer");

        assert_eq!(
            store
                .count_other_live_nodes(&me, short_ttl)
                .await
                .expect("count call succeeds"),
            1,
            "freshly registered, not-expired row counts as live"
        );

        // The dead peer's process is gone and its heartbeat simply stops
        // advancing. This advisory count must stop treating that row as
        // live even before a separate watchdog sweep commits `expired`.
        tokio::time::sleep(Duration::from_millis(350)).await;
        assert_eq!(
            store
                .count_other_live_nodes(&me, short_ttl)
                .await
                .expect("count call succeeds"),
            0,
            "a heartbeat-stale row must stop counting as live even though \
             `expired` was never explicitly committed"
        );
    }

    // FIX 1(c): a draining row (Slice 10 marker, also set by FIX 1(b)'s
    // just-fenced-identity handling in `self_fence::run_node_lease`) must
    // not inflate another node's isolation count even while its heartbeat
    // is still technically fresh.
    #[tokio::test]
    async fn count_other_live_nodes_excludes_draining_rows() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let me = node_identity();
        let draining_peer = node_identity();
        store.register(&me, None).await.expect("register me");
        store
            .register(&draining_peer, None)
            .await
            .expect("register draining peer");
        store
            .mark_draining(&draining_peer)
            .await
            .expect("mark draining");

        let count = store
            .count_other_live_nodes(&me, NODE_LEASE_TTL)
            .await
            .expect("count call succeeds");
        assert_eq!(
            count, 0,
            "a draining-but-heartbeat-fresh row must not count as live"
        );
    }

    #[tokio::test]
    async fn mark_draining_sets_the_flag() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let me = node_identity();
        store.register(&me, None).await.expect("register");
        store.mark_draining(&me).await.expect("mark draining");

        let conn = store.db.guard().await.expect("guard");
        let mut rows = conn
            .query(
                "SELECT draining FROM clustering_nodes WHERE node_id = ?",
                crate::db_params![me.node_id.clone()],
            )
            .await
            .expect("query");
        let draining: bool = rows
            .next()
            .await
            .expect("row present")
            .expect("row present")
            .get(0)
            .expect("column present");
        assert!(draining);
    }

    fn room_entity(id: &str) -> Entity {
        Entity::new(EntityType::RoomActor, id.to_string())
    }

    #[tokio::test]
    async fn terminal_claim_lookup_waits_for_an_in_flight_steal_update() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let previous_owner = node_identity();
        let new_owner = node_identity();
        seed_node(&store.db, &previous_owner, true).await;
        seed_node(&store.db, &new_owner, false).await;
        let entity = room_entity("terminal-barrier@muc.example.com");
        store
            .acquire(&entity, &previous_owner)
            .await
            .expect("seed previous claim");

        let mut steal = store.db.begin().await.expect("begin detached steal");
        let mut rows = steal
            .query(
                r#"
                UPDATE clustering_claims
                SET node_id = ?, node_epoch = ?,
                    claim_epoch = nextval('clustering_claim_epoch_seq'::regclass)
                WHERE entity = ?
                RETURNING claim_epoch
                "#,
                crate::db_params![
                    new_owner.node_id.clone(),
                    new_owner.node_epoch.clone(),
                    entity_key(&entity),
                ],
            )
            .await
            .expect("stage steal update");
        let committed_epoch = ClaimEpoch(
            rows.next()
                .await
                .expect("read staged update")
                .expect("updated claim")
                .get::<i64>(0)
                .expect("claim epoch"),
        );
        drop(rows);

        let barrier_store = PostgresClaimStore::new(store.db.clone());
        let barrier_entity = entity.clone();
        let barrier = tokio::spawn(async move {
            barrier_store
                .current_claim_after_pending_writes(&barrier_entity)
                .await
        });

        let monitor = store.db.guard().await.expect("monitor guard");
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let mut rows = monitor
                    .query(
                        r#"
                        SELECT COUNT(*) FROM pg_stat_activity
                        WHERE pid <> pg_backend_pid()
                          AND query LIKE '%terminal_claim_reconciliation_lock%'
                          AND wait_event_type = 'Lock'
                        "#,
                        (),
                    )
                    .await
                    .expect("inspect blocked terminal lookup");
                let blocked = rows
                    .next()
                    .await
                    .expect("read blocked lookup count")
                    .expect("blocked lookup count row")
                    .get::<i64>(0)
                    .expect("blocked lookup count");
                if blocked > 0 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("terminal lookup reached the claim-row barrier");
        drop(monitor);

        steal.commit().await.expect("commit detached steal");
        let snapshot = barrier
            .await
            .expect("terminal lookup joined")
            .expect("terminal lookup")
            .expect("claim remains present");
        assert_eq!(snapshot.owner, new_owner);
        assert_eq!(snapshot.claim_epoch, committed_epoch);
    }

    // --- ADR-0017 Phase 3 Slice 10: the acquire-side draining gate -------

    #[tokio::test]
    async fn acquire_refuses_a_new_claim_once_the_caller_is_marked_draining() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let me = node_identity();
        store.register(&me, None).await.expect("register");
        store.mark_draining(&me).await.expect("mark draining");

        let entity = room_entity("fresh@muc.example.com");
        let error = store
            .acquire(&entity, &me)
            .await
            .expect_err("a draining node must refuse a brand-new acquire");
        assert!(
            matches!(error, ClaimError::Draining),
            "expected ClaimError::Draining, got {error:?}"
        );

        // The entity must genuinely remain unclaimed — a non-draining node
        // (or this same node, once done draining) can still acquire it.
        let other = node_identity();
        store
            .acquire(&entity, &other)
            .await
            .expect("a non-draining node can still acquire the untouched entity");
    }

    #[tokio::test]
    async fn ensure_claimed_self_reacquire_still_succeeds_while_draining() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let me = node_identity();
        store.register(&me, None).await.expect("register");
        let entity = room_entity("already-owned@muc.example.com");
        let epoch = store
            .ensure_claimed(&entity, &me)
            .await
            .expect("initial acquire, not yet draining");

        store.mark_draining(&me).await.expect("mark draining");

        // Element 4's drain sequence: "keep serving already-owned draining
        // entities." A draining node re-observing a claim it already holds
        // (e.g. a retried first-fenced-write path) must self-reacquire
        // idempotently, never error.
        let reacquired = store
            .ensure_claimed(&entity, &me)
            .await
            .expect("self-reacquire must still succeed while draining");
        assert_eq!(reacquired, epoch);
    }

    #[tokio::test]
    async fn ensure_claimed_refuses_a_genuinely_new_entity_while_draining() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let me = node_identity();
        store.register(&me, None).await.expect("register");
        store.mark_draining(&me).await.expect("mark draining");

        let entity = room_entity("never-owned@muc.example.com");
        let error = store
            .ensure_claimed(&entity, &me)
            .await
            .expect_err("a draining node must refuse a genuinely new entity");
        assert!(
            matches!(error, ClaimError::Draining),
            "expected ClaimError::Draining, got {error:?}"
        );
    }

    #[tokio::test]
    async fn steal_stale_owner_stale_refuses_the_stealer_while_draining() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let dead_owner = node_identity();
        let entity = room_entity("orphaned@muc.example.com");
        let epoch0 = store.acquire(&entity, &dead_owner).await.expect("acquire");
        seed_node(&store.db, &dead_owner, true).await; // dead owner: expired

        let stealer = node_identity();
        store
            .register(&stealer, None)
            .await
            .expect("register stealer");
        store.mark_draining(&stealer).await.expect("mark draining");

        let error = store
            .steal_stale(&entity, epoch0, StalePredicate::OwnerStale, &stealer)
            .await
            .expect_err("a draining node must not win a dead owner's claim either");
        assert!(matches!(error, ClaimError::Conflict));

        // The entity must genuinely remain stealable — a non-draining node
        // can still win it.
        let other = node_identity();
        store
            .register(&other, None)
            .await
            .expect("register non-draining stealer");
        store
            .steal_stale(&entity, epoch0, StalePredicate::OwnerStale, &other)
            .await
            .expect("a non-draining node can still steal the dead owner's claim");
    }

    #[tokio::test]
    async fn steal_stale_owner_stale_requires_a_live_stealer_lease() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let dead_owner = node_identity();
        let entity = room_entity("orphaned-live-stealer@muc.example.com");
        let epoch0 = store.acquire(&entity, &dead_owner).await.expect("acquire");
        seed_node(&store.db, &dead_owner, true).await;

        let missing_stealer = node_identity();
        let missing = store
            .steal_stale(
                &entity,
                epoch0,
                StalePredicate::OwnerStale,
                &missing_stealer,
            )
            .await
            .expect_err("a missing stealer lease row must not win a dead owner's claim");
        assert!(matches!(missing, ClaimError::Conflict));

        let expired_stealer = node_identity();
        seed_node(&store.db, &expired_stealer, true).await;
        let expired = store
            .steal_stale(
                &entity,
                epoch0,
                StalePredicate::OwnerStale,
                &expired_stealer,
            )
            .await
            .expect_err("an expired stealer lease row must not win a dead owner's claim");
        assert!(matches!(expired, ClaimError::Conflict));

        let live_stealer = node_identity();
        seed_node(&store.db, &live_stealer, false).await;
        let epoch1 = store
            .steal_stale(&entity, epoch0, StalePredicate::OwnerStale, &live_stealer)
            .await
            .expect("a live stealer can still reclaim the dead owner's claim");
        assert!(epoch1.0 > epoch0.0);
    }

    #[tokio::test]
    async fn steal_orphaned_sm_session_claim_requires_heartbeat_fresh_stealer_in_cas() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let dead_owner = node_identity();
        let entity = sm_entity("orphaned-sm-heartbeat-fresh-stealer");
        let epoch0 = store.acquire(&entity, &dead_owner).await.expect("acquire");
        seed_node(&store.db, &dead_owner, true).await;

        let live_stealer = node_identity();
        store
            .register(&live_stealer, None)
            .await
            .expect("register live stealer");
        let claim_only = store
            .steal_orphaned_sm_session_claim(&entity, epoch0, &live_stealer, NODE_LEASE_TTL)
            .await
            .expect_err("a claim-only SM session must not win the reaper CAS");
        assert!(matches!(claim_only, ClaimError::Conflict));

        seed_sm_session_row(&store.db, &entity.id).await;

        let heartbeat_stale_stealer = node_identity();
        store
            .register(&heartbeat_stale_stealer, None)
            .await
            .expect("register heartbeat-stale stealer");
        backdate_heartbeat(&store.db, &heartbeat_stale_stealer).await;
        let stale = store
            .steal_orphaned_sm_session_claim(
                &entity,
                epoch0,
                &heartbeat_stale_stealer,
                NODE_LEASE_TTL,
            )
            .await
            .expect_err("a heartbeat-stale-but-not-expired stealer must not win the reaper CAS");
        assert!(matches!(stale, ClaimError::Conflict));

        let epoch1 = store
            .steal_orphaned_sm_session_claim(&entity, epoch0, &live_stealer, NODE_LEASE_TTL)
            .await
            .expect("a heartbeat-fresh stealer can reclaim the orphaned SM session claim");
        assert!(epoch1.0 > epoch0.0);
    }

    #[tokio::test]
    async fn room_orphan_scan_and_steal_are_bounded_and_fenced_against_races() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let store = Arc::new(store);
        let dead_owner = node_identity();
        seed_node(&store.db, &dead_owner, true).await;
        let first = room_entity("a-orphan@muc.example.com");
        let renewed = room_entity("b-renewed@muc.example.com");
        let concurrent = room_entity("c-concurrent@muc.example.com");
        let first_epoch = store
            .acquire(&first, &dead_owner)
            .await
            .expect("first claim");
        let renewed_epoch = store
            .acquire(&renewed, &dead_owner)
            .await
            .expect("renewed claim");
        let concurrent_epoch = store
            .acquire(&concurrent, &dead_owner)
            .await
            .expect("concurrent claim");

        let bounded = store
            .list_orphaned_room_actor_claims(2)
            .await
            .expect("bounded room scan");
        assert_eq!(bounded.len(), 2, "the SQL LIMIT must bound each sweep");
        assert_eq!(bounded[0].entity, first);
        assert_eq!(bounded[1].entity, renewed);

        let live_stealer = node_identity();
        store
            .register(&live_stealer, None)
            .await
            .expect("register live stealer");
        let won = store
            .steal_orphaned_room_actor_claim(&first, first_epoch, &live_stealer, NODE_LEASE_TTL)
            .await
            .expect("fresh sweeper wins");
        assert!(
            won > concurrent_epoch,
            "a room-claim steal must allocate a generation newer than every earlier claim"
        );
        let claimed = store
            .current_claim(&first)
            .await
            .expect("read stolen room claim")
            .expect("stolen room remains claimed");
        assert_eq!(claimed.owner, live_stealer);
        assert_eq!(claimed.claim_epoch, won);

        // Candidate discovery was advisory. If the owner renews before the
        // CAS, the stale snapshot cannot displace it.
        store
            .register(&dead_owner, None)
            .await
            .expect("owner renews its exact lease identity");
        assert!(matches!(
            store
                .steal_orphaned_room_actor_claim(
                    &renewed,
                    renewed_epoch,
                    &live_stealer,
                    NODE_LEASE_TTL,
                )
                .await,
            Err(ClaimError::Conflict)
        ));

        // Make the owner stale again, then race two sweepers against the
        // same observed epoch. Exactly one can bump it once.
        backdate_heartbeat(&store.db, &dead_owner).await;
        assert!(store
            .expire(&dead_owner, NODE_LEASE_TTL)
            .await
            .expect("commit the renewed owner as expired"));
        let second_stealer = node_identity();
        store
            .register(&second_stealer, None)
            .await
            .expect("register second stealer");
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let task_a = {
            let store = Arc::clone(&store);
            let entity = concurrent.clone();
            let stealer = live_stealer.clone();
            let barrier = Arc::clone(&barrier);
            tokio::spawn(async move {
                barrier.wait().await;
                store
                    .steal_orphaned_room_actor_claim(
                        &entity,
                        concurrent_epoch,
                        &stealer,
                        NODE_LEASE_TTL,
                    )
                    .await
            })
        };
        let task_b = {
            let store = Arc::clone(&store);
            let entity = concurrent.clone();
            let stealer = second_stealer.clone();
            let barrier = Arc::clone(&barrier);
            tokio::spawn(async move {
                barrier.wait().await;
                store
                    .steal_orphaned_room_actor_claim(
                        &entity,
                        concurrent_epoch,
                        &stealer,
                        NODE_LEASE_TTL,
                    )
                    .await
            })
        };
        let (a, b) = tokio::join!(task_a, task_b);
        let results = [a.expect("task a"), b.expect("task b")];
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(ClaimError::Conflict)))
                .count(),
            1
        );

        // A heartbeat-stale (but not yet committed-expired) sweeper is
        // fenced inside the same CAS statement.
        let stale_sweeper = node_identity();
        store
            .register(&stale_sweeper, None)
            .await
            .expect("register stale sweeper");
        backdate_heartbeat(&store.db, &stale_sweeper).await;
        let another = room_entity("d-stale-sweeper@muc.example.com");
        let another_epoch = store
            .acquire(&another, &dead_owner)
            .await
            .expect("another orphan");
        assert!(matches!(
            store
                .steal_orphaned_room_actor_claim(
                    &another,
                    another_epoch,
                    &stale_sweeper,
                    NODE_LEASE_TTL,
                )
                .await,
            Err(ClaimError::Conflict)
        ));
    }

    #[tokio::test]
    async fn orphan_scan_preserves_valid_room_jids_longer_than_128_bytes() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let dead_owner = node_identity();
        seed_node(&store.db, &dead_owner, true).await;
        let long_room_jid: jid::BareJid = format!("{}@muc.example.com", "a".repeat(129))
            .parse()
            .expect("valid long room JID");
        let entity = room_entity(long_room_jid.as_str());
        store
            .acquire(&entity, &dead_owner)
            .await
            .expect("long room claim");

        let candidates = store
            .list_orphaned_room_actor_claims(1)
            .await
            .expect("orphan scan");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].entity, entity);
        assert!(
            store
                .current_claim(&entity)
                .await
                .expect("claim lookup")
                .is_some(),
            "a valid long room claim must be returned for adoption, not quarantined"
        );
    }

    #[tokio::test]
    async fn malformed_room_claims_are_quarantined_without_starving_valid_page() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let dead_owner = node_identity();
        seed_node(&store.db, &dead_owner, true).await;
        let mut malformed = Vec::new();
        for index in 0..65 {
            let entity = Entity::new(EntityType::RoomActor, format!("000-invalid-{index:02}"));
            store
                .acquire(&entity, &dead_owner)
                .await
                .expect("malformed claim");
            malformed.push(entity);
        }
        let valid = room_entity("zzz-valid@muc.example.com");
        store
            .acquire(&valid, &dead_owner)
            .await
            .expect("valid claim");

        let first = store
            .list_orphaned_room_actor_claims_page(None, 64)
            .await
            .expect("first bounded scan");
        assert!(first.candidates.is_empty());
        assert!(first.has_more);
        assert_eq!(first.quarantined, 64);
        let second = store
            .list_orphaned_room_actor_claims_page(first.next_cursor, 64)
            .await
            .expect("second bounded scan");
        assert!(second
            .candidates
            .iter()
            .any(|candidate| candidate.entity == valid));
        assert_eq!(second.quarantined, 1);
        assert!(!second.has_more);
        for entity in [
            malformed.first().expect("first"),
            malformed.last().expect("last"),
        ] {
            assert!(store
                .current_claim(entity)
                .await
                .expect("claim lookup")
                .is_none());
        }
    }

    // --- ADR-0017 Phase 3 Slice 10: current_generation (Q5's mechanism) --

    #[tokio::test]
    async fn current_generation_is_none_with_no_live_nodes() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        assert_eq!(store.current_generation().await.expect("query"), None);
    }

    #[tokio::test]
    async fn current_generation_is_the_most_recently_registered_live_nodes_hash() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let old = node_identity();
        store
            .register(&old, Some("old-gen-hash".to_string()))
            .await
            .expect("register old");
        // Ensure a strictly later `first_seen` than `old`'s — Postgres
        // `now()` resolution is high enough that two `register` calls in
        // the same test could otherwise tie.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let new = node_identity();
        store
            .register(&new, Some("new-gen-hash".to_string()))
            .await
            .expect("register new");

        assert_eq!(
            store.current_generation().await.expect("query"),
            Some("new-gen-hash".to_string())
        );
    }

    #[tokio::test]
    async fn current_generation_ignores_expired_rows() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let old_live = node_identity();
        store
            .register(&old_live, Some("still-live-hash".to_string()))
            .await
            .expect("register old_live");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let newer_but_dead = node_identity();
        store
            .register(&newer_but_dead, Some("dead-gen-hash".to_string()))
            .await
            .expect("register newer_but_dead");
        // Force this row `expired` directly: `seed_node` INSERTs a fresh
        // row (it would conflict on `node_id`'s PRIMARY KEY against the
        // one `register` just created), so an `UPDATE` is the correct
        // fixture shape here, not `seed_node`.
        let conn = store.db.guard().await.expect("guard");
        conn.execute(
            "UPDATE clustering_nodes SET expired = true WHERE node_id = ?",
            crate::db_params![newer_but_dead.node_id.clone()],
        )
        .await
        .expect("force expire newer_but_dead");

        assert_eq!(
            store.current_generation().await.expect("query"),
            Some("still-live-hash".to_string()),
            "the most-recently-registered row is expired, so the next-freshest LIVE row wins"
        );
    }

    #[tokio::test]
    async fn current_generation_preserves_first_seen_across_re_registration() {
        // Register/re-register under the SAME node_id must NOT refresh
        // `first_seen` — `register`'s `ON CONFLICT (node_id) DO UPDATE`
        // deliberately omits `first_seen` from its SET list.
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let node = NodeIdentity::new("stable-node-id", "epoch-0");
        store
            .register(&node, Some("gen-a".to_string()))
            .await
            .expect("first register");
        // `DbDecode` (this crate's row-decoding trait) has no
        // `chrono::DateTime` impl, so `first_seen` is read as its Postgres
        // text representation — a plain string-equality check on the exact
        // same stored value is exactly what "must not have changed" needs,
        // with no timestamp-parsing machinery required.
        let conn = store.db.guard().await.expect("guard");
        let mut rows = conn
            .query(
                "SELECT first_seen::text FROM clustering_nodes WHERE node_id = ?",
                crate::db_params![node.node_id.clone()],
            )
            .await
            .expect("query");
        let first_seen_initial: String = rows
            .next()
            .await
            .expect("row present")
            .expect("row present")
            .get(0)
            .expect("column present");

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let node_reregistered = NodeIdentity::new("stable-node-id", "epoch-1");
        store
            .register(&node_reregistered, Some("gen-b".to_string()))
            .await
            .expect("re-register under the same node_id, new node_epoch");

        let mut rows = conn
            .query(
                "SELECT first_seen::text FROM clustering_nodes WHERE node_id = ?",
                crate::db_params![node.node_id.clone()],
            )
            .await
            .expect("query");
        let first_seen_after: String = rows
            .next()
            .await
            .expect("row present")
            .expect("row present")
            .get(0)
            .expect("column present");
        assert_eq!(
            first_seen_initial, first_seen_after,
            "re-registration under the same node_id must not refresh first_seen"
        );
    }

    // FIX 7(c): renamed from `..._under_a_live_local_actor` — this is a pure
    // store-level test with no actor in play (`NoLocallyClaimedEntities`
    // per Slice 2's own doc comment; the query is exercised directly here,
    // not through any actor).
    #[tokio::test]
    async fn reconcile_returns_entities_stolen_out_from_under_this_nodes_claim() {
        // The reconciliation query's core promise: an entity a node
        // believes it locally owns, but whose Postgres claim has since
        // moved to a different node, must show up as "lost" so the caller
        // can demote/tombstone it.
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let me = node_identity();
        let other = node_identity();
        store.register(&me, None).await.expect("register me");

        let still_owned = sm_entity("stream-still-owned");
        let stolen = sm_entity("stream-stolen");
        store
            .acquire(&still_owned, &me)
            .await
            .expect("acquire still_owned");
        let stolen_epoch = store.acquire(&stolen, &me).await.expect("acquire stolen");

        // Simulate a steal out from under `me` via the consent CAS (no
        // staleness required — proves reconciliation does not depend on how
        // the claim moved, only that Postgres no longer attributes it to
        // this node/epoch).
        let jid: jid::BareJid = "alice@example.com".parse().expect("valid jid");
        let proof =
            waddle_xmpp::ownership::verify_resume_identity(&jid, &jid).expect("identity match");
        store
            .steal_for_resume(&stolen, stolen_epoch, proof, &other)
            .await
            .expect("steal succeeds");

        let locally_owned = [still_owned.clone(), stolen.clone()];
        let lost = store
            .reconcile(&me, &locally_owned)
            .await
            .expect("reconcile call succeeds");
        assert_eq!(lost, vec![stolen], "only the stolen entity is lost");
    }

    #[tokio::test]
    async fn reconcile_on_an_empty_local_set_is_a_no_op() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let me = node_identity();
        let lost = store
            .reconcile(&me, &[])
            .await
            .expect("reconcile call succeeds");
        assert!(lost.is_empty());
    }

    // --- Steal-intents unwedge/owner-veto path (ADR-0017 Phase 3 Slice 3) ---

    #[tokio::test]
    async fn report_steal_intent_rejects_sm_session() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let reporter = node_identity();
        let entity = sm_entity("stream-1");
        let err = store
            .report_steal_intent(&entity, &reporter)
            .await
            .expect_err("SM-session claims are excluded from the steal-intent path");
        assert!(matches!(err, ClaimError::SmSessionExcludedFromStealIntent));
    }

    #[tokio::test]
    async fn steal_intent_veto_clears_before_expiry_blocks_the_steal() {
        // The owner-veto fast path: a reported intent, cleared by the owner
        // (an epoch-fenced DELETE proving it is still alive) before it ages
        // past `intent_ttl`, must make the steal CAS lose the race.
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let owner = node_identity();
        let entity = user_actor_entity("room-1");
        let epoch0 = store.acquire(&entity, &owner).await.expect("acquire");

        let stealer = node_identity();
        store
            .register(&stealer, None)
            .await
            .expect("register live stealer");
        store
            .report_steal_intent(&entity, &stealer)
            .await
            .expect("report intent");

        let intent_ttl = std::time::Duration::from_millis(50);

        // Too fresh to steal yet.
        let err = store
            .steal_stale(
                &entity,
                epoch0,
                StalePredicate::StealIntentExpired { intent_ttl },
                &stealer,
            )
            .await
            .expect_err("a freshly reported intent has not aged past intent_ttl yet");
        assert!(matches!(err, ClaimError::Conflict));

        // Owner scans its own claims, finds the intent, health-asks
        // healthily, and clears it — FIX 1(e): the veto is enforced by
        // serializing on the intent rows against a concurrent steal
        // (deadlock-abort-safe, FIX 1(c)), not by any inherent
        // "unforgeability" of the write itself.
        let intents = store
            .owner_steal_intents(&owner)
            .await
            .expect("owner_steal_intents");
        assert_eq!(intents, vec![(entity.clone(), epoch0)]);
        let cleared = store
            .clear_steal_intent(&entity, &owner, epoch0)
            .await
            .expect("clear_steal_intent");
        assert_eq!(
            cleared, 1,
            "FIX 1(b): a genuine veto reports the row it deleted"
        );

        // No outstanding intent left: the steal must lose even after
        // `intent_ttl` has elapsed.
        tokio::time::sleep(intent_ttl * 2).await;
        let err = store
            .steal_stale(
                &entity,
                epoch0,
                StalePredicate::StealIntentExpired { intent_ttl },
                &stealer,
            )
            .await
            .expect_err("a vetoed (cleared) intent must not make the entity stealable");
        assert!(matches!(err, ClaimError::Conflict));
    }

    #[tokio::test]
    async fn steal_intent_unanswered_past_ttl_allows_the_steal() {
        // The unwedge path: an intent the owner never clears (wedged, or
        // genuinely gone but still heartbeating) becomes stealable once it
        // ages past `intent_ttl` — the whole point of this CAS variant.
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let owner = node_identity();
        let entity = user_actor_entity("room-2");
        let epoch0 = store.acquire(&entity, &owner).await.expect("acquire");
        // The owner's node lease is perfectly fresh — this CAS variant must
        // steal regardless, unlike `OwnerStale`.
        seed_node(&store.db, &owner, false).await;

        let stealer = node_identity();
        store
            .register(&stealer, None)
            .await
            .expect("register live stealer");
        store
            .report_steal_intent(&entity, &stealer)
            .await
            .expect("report intent");

        let intent_ttl = std::time::Duration::from_millis(100);
        tokio::time::sleep(intent_ttl * 2).await;

        let epoch1 = store
            .steal_stale(
                &entity,
                epoch0,
                StalePredicate::StealIntentExpired { intent_ttl },
                &stealer,
            )
            .await
            .expect("an uncleared, aged-out intent makes the entity stealable");
        assert!(epoch1.0 > epoch0.0);
    }

    #[tokio::test]
    async fn steal_intent_expired_requires_live_stealer_without_burning_intents() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let owner = node_identity();
        let entity = user_actor_entity("room-live-intent-stealer");
        let epoch0 = store.acquire(&entity, &owner).await.expect("acquire");
        seed_node(&store.db, &owner, false).await;

        let missing_stealer = node_identity();
        store
            .report_steal_intent(&entity, &missing_stealer)
            .await
            .expect("report intent");
        let intent_ttl = std::time::Duration::from_millis(50);
        tokio::time::sleep(intent_ttl * 2).await;

        let missing = store
            .steal_stale(
                &entity,
                epoch0,
                StalePredicate::StealIntentExpired { intent_ttl },
                &missing_stealer,
            )
            .await
            .expect_err("a missing stealer lease row must not win via steal-intent");
        assert!(matches!(missing, ClaimError::Conflict));
        assert_eq!(
            store
                .owner_steal_intents(&owner)
                .await
                .expect("owner_steal_intents after missing stealer"),
            vec![(entity.clone(), epoch0)],
            "a missing stealer must not consume the aged intent it failed to use"
        );

        let expired_stealer = node_identity();
        seed_node(&store.db, &expired_stealer, true).await;
        let expired = store
            .steal_stale(
                &entity,
                epoch0,
                StalePredicate::StealIntentExpired { intent_ttl },
                &expired_stealer,
            )
            .await
            .expect_err("an expired stealer lease row must not win via steal-intent");
        assert!(matches!(expired, ClaimError::Conflict));
        assert_eq!(
            store
                .owner_steal_intents(&owner)
                .await
                .expect("owner_steal_intents after expired stealer"),
            vec![(entity.clone(), epoch0)],
            "an expired stealer must not consume the aged intent it failed to use"
        );

        let live_stealer = node_identity();
        store
            .register(&live_stealer, None)
            .await
            .expect("register live stealer");
        let epoch1 = store
            .steal_stale(
                &entity,
                epoch0,
                StalePredicate::StealIntentExpired { intent_ttl },
                &live_stealer,
            )
            .await
            .expect("a live stealer can consume and win the aged intent");
        assert!(epoch1.0 > epoch0.0);
    }

    #[tokio::test]
    async fn clear_steal_intent_by_a_deposed_owner_is_a_no_op() {
        // Epoch-fenced clear: once the claim has moved to a new owner, the
        // old owner's clear call (under its now-stale epoch) must not
        // delete the intent row — only the current owner can veto.
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let old_owner = node_identity();
        let entity = user_actor_entity("room-3");
        let epoch0 = store.acquire(&entity, &old_owner).await.expect("acquire");

        let reporter = node_identity();
        store
            .report_steal_intent(&entity, &reporter)
            .await
            .expect("report intent");

        // Simulate the claim moving to a new owner via the consent CAS —
        // any CAS variant works here; reconciliation and clear_steal_intent
        // only care that Postgres no longer attributes the claim to
        // old_owner/epoch0.
        let new_owner = node_identity();
        let jid: jid::BareJid = "alice@example.com".parse().expect("valid jid");
        let proof =
            waddle_xmpp::ownership::verify_resume_identity(&jid, &jid).expect("identity match");
        let epoch1 = store
            .steal_for_resume(&entity, epoch0, proof, &new_owner)
            .await
            .expect("steal succeeds");

        // The deposed owner's clear call, still under its old epoch, is a
        // silent no-op: the intent row must survive.
        let cleared_by_deposed_owner = store
            .clear_steal_intent(&entity, &old_owner, epoch0)
            .await
            .expect("deposed owner's clear call does not error, but is a no-op");
        assert_eq!(
            cleared_by_deposed_owner, 0,
            "FIX 1(b): a deposed owner's stale-epoch clear must report zero rows affected, \
             so `run_node_lease` can tell it apart from a genuine veto"
        );
        let survives = store
            .owner_steal_intents(&new_owner)
            .await
            .expect("owner_steal_intents");
        assert_eq!(
            survives,
            vec![(entity.clone(), epoch1)],
            "the intent row must survive a deposed owner's stale-epoch clear attempt"
        );

        // The new, current owner's clear call succeeds.
        let cleared_by_new_owner = store
            .clear_steal_intent(&entity, &new_owner, epoch1)
            .await
            .expect("current owner's clear_steal_intent succeeds");
        assert_eq!(
            cleared_by_new_owner, 1,
            "FIX 1(b): the current owner's veto reports the row it deleted"
        );
        let cleared = store
            .owner_steal_intents(&new_owner)
            .await
            .expect("owner_steal_intents");
        assert!(cleared.is_empty());
    }

    #[tokio::test]
    async fn stale_epoch_steal_attempt_does_not_burn_the_intents() {
        // A data-modifying CTE runs to completion even when the outer
        // UPDATE matches nothing, so without the epoch gate on the
        // consuming DELETE a caller holding an already-stale observed
        // epoch would delete the aged-out intents a concurrent,
        // correctly-epoched stealer needed — delaying the unwedge by a
        // full intent_ttl. The gate makes the stale caller's CTE a no-op.
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let owner = node_identity();
        let entity = user_actor_entity("room-stale-burn");
        let epoch0 = store.acquire(&entity, &owner).await.expect("acquire");
        seed_node(&store.db, &owner, false).await;

        // Bump the claim epoch so epoch0 is stale.
        let jid: jid::BareJid = "alice@example.com".parse().expect("valid jid");
        let proof =
            waddle_xmpp::ownership::verify_resume_identity(&jid, &jid).expect("identity match");
        let current_owner = node_identity();
        let epoch1 = store
            .steal_for_resume(&entity, epoch0, proof, &current_owner)
            .await
            .expect("epoch bump");

        let reporter = node_identity();
        store
            .report_steal_intent(&entity, &reporter)
            .await
            .expect("report intent");
        let intent_ttl = std::time::Duration::from_millis(100);
        tokio::time::sleep(intent_ttl * 2).await;

        // The stale-epoch caller loses its CAS AND must not consume the
        // intent.
        let stale_caller = node_identity();
        store
            .register(&stale_caller, None)
            .await
            .expect("register stale-epoch caller");
        let err = store
            .steal_stale(
                &entity,
                epoch0,
                StalePredicate::StealIntentExpired { intent_ttl },
                &stale_caller,
            )
            .await
            .expect_err("stale observed epoch loses the CAS");
        assert!(matches!(err, ClaimError::Conflict));
        let survives = store
            .owner_steal_intents(&current_owner)
            .await
            .expect("owner_steal_intents");
        assert_eq!(
            survives,
            vec![(entity.clone(), epoch1)],
            "a stale-epoch steal attempt must not burn the expired intent"
        );

        // A correctly-epoched stealer still wins on the surviving intent.
        let real_stealer = node_identity();
        store
            .register(&real_stealer, None)
            .await
            .expect("register real stealer");
        let epoch2 = store
            .steal_stale(
                &entity,
                epoch1,
                StalePredicate::StealIntentExpired { intent_ttl },
                &real_stealer,
            )
            .await
            .expect("current-epoch stealer wins on the surviving intent");
        assert!(epoch2.0 > epoch1.0);
        let consumed = store
            .owner_steal_intents(&real_stealer)
            .await
            .expect("owner_steal_intents");
        assert!(
            consumed.is_empty(),
            "the winning steal consumes the intents that authorized it"
        );
    }

    #[tokio::test]
    async fn owner_steal_intents_only_returns_entities_with_outstanding_intents() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let owner = node_identity();
        let with_intent = user_actor_entity("room-with-intent");
        let without_intent = Entity::new(EntityType::RoomActor, "room-without-intent".to_string());
        let epoch_with_intent = store
            .acquire(&with_intent, &owner)
            .await
            .expect("acquire with_intent");
        store
            .acquire(&without_intent, &owner)
            .await
            .expect("acquire without_intent");

        let reporter = node_identity();
        store
            .report_steal_intent(&with_intent, &reporter)
            .await
            .expect("report intent");

        let intents = store
            .owner_steal_intents(&owner)
            .await
            .expect("owner_steal_intents");
        assert_eq!(intents, vec![(with_intent, epoch_with_intent)]);
    }

    #[tokio::test]
    async fn report_steal_intent_refreshes_rather_than_accumulates() {
        // `UNIQUE (entity, reporter_node)` + the upsert: repeated reports
        // from the same reporter against the same entity must collapse to
        // one row, refreshing `created_at` each time (so the steal CAS's
        // aged-out check restarts from the latest report).
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let owner = node_identity();
        let entity = user_actor_entity("room-4");
        let epoch0 = store.acquire(&entity, &owner).await.expect("acquire");
        let reporter = node_identity();

        store
            .report_steal_intent(&entity, &reporter)
            .await
            .expect("first report");
        let intent_ttl = std::time::Duration::from_millis(150);
        tokio::time::sleep(intent_ttl / 2).await;
        // Refresh before the first report ages out.
        store
            .report_steal_intent(&entity, &reporter)
            .await
            .expect("refreshed report");
        tokio::time::sleep(intent_ttl / 2 + std::time::Duration::from_millis(20)).await;

        // If the refresh had not landed, the first report would already be
        // stale enough to steal by now; it must not be.
        let stealer = node_identity();
        store
            .register(&stealer, None)
            .await
            .expect("register live stealer");
        let err = store
            .steal_stale(
                &entity,
                epoch0,
                StalePredicate::StealIntentExpired { intent_ttl },
                &stealer,
            )
            .await
            .expect_err("a refreshed intent must not have aged out yet");
        assert!(matches!(err, ClaimError::Conflict));

        let conn = store.db.guard().await.expect("guard");
        let mut rows = conn
            .query("SELECT COUNT(*) FROM clustering_steal_intents", ())
            .await
            .expect("count query");
        let count: i64 = rows
            .next()
            .await
            .expect("row present")
            .expect("row present")
            .get(0)
            .expect("column present");
        assert_eq!(count, 1, "repeated reports must refresh, not accumulate");
    }

    // FIX 1(d): a genuinely concurrent stress test proving the veto race is
    // closed. Two real Postgres connections hammer `clear_steal_intent`
    // (the owner's veto) against `steal_stale(StealIntentExpired)` (a
    // stealer's attempt) on the SAME entity, at the SAME observed epoch,
    // across many rounds — re-seeding an already-aged intent each round
    // (backdated directly via SQL rather than a real-time sleep, so the
    // stress loop runs fast) and racing the two calls off a `Barrier` so
    // both fire as close to simultaneously as async scheduling allows.
    // Modeled on `steal_commit_interleaved_inside_a_fenced_transaction`'s
    // concurrency style (hold-a-lock-then-race pattern), generalized here
    // to genuinely-concurrent racing rather than a controlled hold/release.
    //
    // The invariant under test: a clear reporting `rows_affected > 0` (a
    // real veto) and a steal succeeding against the SAME observed epoch
    // must never both happen in one round — write skew would mean a
    // "vetoed" owner was simultaneously deposed. FIX 1(c)'s deliberate
    // opposite lock-acquisition order means Postgres may occasionally
    // abort one side of a round with `40P01 deadlock_detected`; this test
    // asserts that outcome — when it occurs — surfaces as an ordinary typed
    // `ClaimError::Backend`, never a panic, and counts how often it
    // happened for the test's own report (not an assertion, since a
    // deadlock is a possible-but-not-guaranteed outcome of any given round).
    #[tokio::test]
    async fn steal_intent_veto_vs_steal_stress_never_both_succeed_same_round() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let store = std::sync::Arc::new(store);

        let mut owner = node_identity();
        let mut stealer = node_identity();
        store
            .register(&owner, None)
            .await
            .expect("register initial owner");
        store
            .register(&stealer, None)
            .await
            .expect("register initial stealer");
        let entity = user_actor_entity("stress-veto-race");
        let mut epoch = store.acquire(&entity, &owner).await.expect("acquire");

        let intent_ttl = std::time::Duration::from_millis(30);
        const ROUNDS: usize = 200;
        let mut clears_won = 0usize;
        let mut steals_won = 0usize;
        let mut deadlocks_observed = 0usize;

        fn mentions_deadlock(err: &ClaimError) -> bool {
            matches!(err, ClaimError::Backend(msg) if msg.to_lowercase().contains("deadlock"))
        }

        for round in 0..ROUNDS {
            // Re-seed an already-aged intent directly: `report_steal_intent`
            // always stamps `created_at = now()`, so backdate it here
            // instead of sleeping out a real `intent_ttl` every round (200
            // rounds of real sleeps would make this test unreasonably
            // slow).
            {
                let conn = store.db.guard().await.expect("guard");
                conn.execute(
                    r#"
                    INSERT INTO clustering_steal_intents (entity, reporter_node, created_at)
                    VALUES (?, ?, now() - (? || ' milliseconds')::interval)
                    ON CONFLICT (entity, reporter_node)
                        DO UPDATE SET created_at = EXCLUDED.created_at
                    "#,
                    crate::db_params![
                        entity_key(&entity),
                        stealer.node_id.clone(),
                        (intent_ttl.as_millis() * 10).to_string(),
                    ],
                )
                .await
                .expect("seed an already-aged intent");
            }

            let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));

            let clear_task = {
                let store = std::sync::Arc::clone(&store);
                let owner = owner.clone();
                let entity = entity.clone();
                let barrier = std::sync::Arc::clone(&barrier);
                tokio::spawn(async move {
                    barrier.wait().await;
                    store.clear_steal_intent(&entity, &owner, epoch).await
                })
            };
            let steal_task = {
                let store = std::sync::Arc::clone(&store);
                let stealer = stealer.clone();
                let entity = entity.clone();
                let barrier = std::sync::Arc::clone(&barrier);
                tokio::spawn(async move {
                    barrier.wait().await;
                    store
                        .steal_stale(
                            &entity,
                            epoch,
                            StalePredicate::StealIntentExpired { intent_ttl },
                            &stealer,
                        )
                        .await
                })
            };

            let clear_result = clear_task.await.expect("clear task must not panic");
            let steal_result = steal_task.await.expect("steal task must not panic");

            let clear_won = matches!(clear_result, Ok(rows) if rows > 0);
            let steal_won = steal_result.is_ok();
            assert!(
                !(clear_won && steal_won),
                "round {round}: both a veto-clear ({clear_result:?}) and a steal \
                 ({steal_result:?}) succeeded against the same observed epoch {epoch:?} — \
                 write skew between clear_steal_intent and steal_stale"
            );

            // Both results are already typed `Result<_, ClaimError>` at this
            // point (propagated cleanly through the `?` operator inside
            // `clear_steal_intent`/`steal_stale` — a panic would have
            // already failed the `expect("... task must not panic")` calls
            // above). This only distinguishes the FIX 1(c) deadlock outcome
            // for the test's own report.
            if let Err(error) = &clear_result {
                if mentions_deadlock(error) {
                    deadlocks_observed += 1;
                }
            }
            if let Err(error) = &steal_result {
                if mentions_deadlock(error) {
                    deadlocks_observed += 1;
                }
            }

            if steal_won {
                steals_won += 1;
                epoch = steal_result.expect("checked Ok above");
                std::mem::swap(&mut owner, &mut stealer);
            } else if clear_won {
                clears_won += 1;
                // Ownership/epoch unchanged: the veto held.
            }
            // Neither winning (both sides lost, e.g. one aborted on a
            // deadlock while the other had already been overtaken by the
            // seed step's own timing) is possible but rare; nothing to
            // update in that case — the next round simply re-seeds and
            // retries under the same owner/epoch.
        }

        eprintln!(
            "steal_intent_veto_vs_steal_stress: {ROUNDS} rounds, clears_won={clears_won}, \
             steals_won={steals_won}, deadlocks_observed={deadlocks_observed}"
        );
        assert!(
            clears_won + steals_won <= ROUNDS,
            "sanity: cannot win more rounds than were run"
        );
    }
    #[tokio::test]
    async fn list_orphaned_sm_session_claims_scopes_to_sm_session_and_stale_owners() {
        // ADR-0017 Phase 3 Slice 5, element 9: the orphan-reaper scan finds
        // detached `sm_session` claims owned by a stale node, ignores fresh
        // owners, and never surfaces a `UserActor`/`RoomActor` claim even
        // under the exact same stale owner.
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };

        let fresh_owner = node_identity();
        seed_node(&store.db, &fresh_owner, false).await;
        let fresh_entity = sm_entity("fresh-owner-stream");
        seed_sm_session_row(&store.db, &fresh_entity.id).await;
        store
            .acquire(&fresh_entity, &fresh_owner)
            .await
            .expect("acquire under fresh owner");

        let stale_owner = node_identity();
        seed_node(&store.db, &stale_owner, true).await;
        let orphaned_entity = sm_entity("orphaned-stream");
        let orphaned_epoch = store
            .acquire(&orphaned_entity, &stale_owner)
            .await
            .expect("acquire under stale owner");
        seed_sm_session_row(&store.db, &orphaned_entity.id).await;
        let claim_only_entity = sm_entity("claim-only-stream");
        store
            .acquire(&claim_only_entity, &stale_owner)
            .await
            .expect("acquire claim-only SM session under stale owner");
        let non_sm_entity = Entity::new(EntityType::UserActor, "orphaned-user-actor".to_string());
        store
            .acquire(&non_sm_entity, &stale_owner)
            .await
            .expect("acquire a non-sm_session entity under the same stale owner");

        let candidates = store
            .list_orphaned_sm_session_claims()
            .await
            .expect("list_orphaned_sm_session_claims");
        assert_eq!(
            candidates.len(),
            1,
            "exactly one candidate: the sm_session claim under the stale owner, \
             excluding the fresh-owner claim, the claim-only live SM session, \
             and the non-sm_session claim"
        );
        assert_eq!(candidates[0].entity, orphaned_entity);
        assert_eq!(candidates[0].epoch, orphaned_epoch);
        assert_eq!(candidates[0].owner, stale_owner);

        // Steal it, register the new owner as live, and confirm the scan
        // no longer reports it.
        let new_owner = node_identity();
        seed_node(&store.db, &new_owner, false).await;
        store
            .steal_stale(
                &orphaned_entity,
                orphaned_epoch,
                StalePredicate::OwnerStale,
                &new_owner,
            )
            .await
            .expect("steal_stale(OwnerStale) reclaims the orphaned claim");

        let candidates_after = store
            .list_orphaned_sm_session_claims()
            .await
            .expect("list_orphaned_sm_session_claims after steal");
        assert!(
            candidates_after.is_empty(),
            "the reclaimed claim (now owned by a fresh, registered node) must no longer \
             be reported as orphaned"
        );
    }

    #[tokio::test]
    async fn legacy_orphaned_sm_scan_skips_ids_above_the_sm_wire_limit() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let stale_owner = node_identity();
        seed_node(&store.db, &stale_owner, true).await;
        let malformed_id = "z".repeat(waddle_xmpp::pending_delivery::SM_SESSION_ID_MAX_LEN + 1);
        let malformed = sm_entity(&malformed_id);
        seed_sm_session_row(&store.db, &malformed_id).await;
        store
            .acquire(&malformed, &stale_owner)
            .await
            .expect("seed overlong SM claim");

        let candidates = store
            .list_orphaned_sm_session_claims()
            .await
            .expect("legacy orphan scan");
        assert!(
            candidates.is_empty(),
            "the legacy scan must not construct a typed orphan candidate from an overlong SM id"
        );
    }

    #[tokio::test]
    async fn orphaned_sm_pages_advance_past_sixty_four_and_quarantine_malformed_rows() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let stale_owner = node_identity();
        seed_node(&store.db, &stale_owner, true).await;
        for index in 0..70 {
            let entity = sm_entity(&format!("paged-{index:03}"));
            seed_sm_session_row(&store.db, &entity.id).await;
            store
                .acquire(&entity, &stale_owner)
                .await
                .expect("acquire paged claim");
        }

        let first = store
            .list_orphaned_sm_session_claims_page(None, 64)
            .await
            .expect("first page");
        assert_eq!(first.candidates.len(), 64);
        assert!(first.has_more);
        let second = store
            .list_orphaned_sm_session_claims_page(first.next_cursor, 64)
            .await
            .expect("second page");
        assert_eq!(
            second.candidates.len(),
            6,
            "cursor must expose rows beyond the first 64"
        );

        // An id above the SM wire limit can still fit the deliberately wider
        // generic Entity bound, but it is not a valid typed SmSessionId. It
        // satisfies the durable-row join, so the scan must exact-owner/epoch
        // quarantine it instead of re-WARN forever.
        let malformed_id = "z".repeat(waddle_xmpp::pending_delivery::SM_SESSION_ID_MAX_LEN + 1);
        seed_sm_session_row(&store.db, &malformed_id).await;
        let encoded = format!("{}:{malformed_id}", EntityType::SmSession.as_db_str());
        let conn = store.db.guard().await.expect("guard");
        conn.execute(
            r#"INSERT INTO clustering_claims
               (entity, entity_type, node_id, node_epoch, claim_epoch)
               VALUES (?, ?, ?, ?, 0)"#,
            crate::db_params![
                encoded.clone(),
                EntityType::SmSession.as_db_str().to_string(),
                stale_owner.node_id.clone(),
                stale_owner.node_epoch.clone(),
            ],
        )
        .await
        .expect("seed malformed claim");
        drop(conn);
        let malformed_page = store
            .list_orphaned_sm_session_claims_page(
                Some(SmOrphanScanCursor::from_raw(
                    "sm_session:paged-999".to_string(),
                )),
                64,
            )
            .await
            .expect("malformed page");
        assert_eq!(malformed_page.quarantined, 1);
        let conn = store.db.guard().await.expect("guard");
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM clustering_claims WHERE entity = ?",
                crate::db_params![encoded],
            )
            .await
            .expect("count quarantined row");
        let count: i64 = rows
            .next()
            .await
            .expect("row")
            .expect("row")
            .get(0)
            .expect("count");
        assert_eq!(count, 0);
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM sm_sessions WHERE stream_id = ?",
                crate::db_params![malformed_id],
            )
            .await
            .expect("count quarantined durable row");
        let durable_count: i64 = rows
            .next()
            .await
            .expect("row")
            .expect("row")
            .get(0)
            .expect("count");
        assert_eq!(durable_count, 0);
    }

    #[tokio::test]
    async fn orphaned_sm_page_classifies_claim_only_and_wrong_prefix_rows_safely() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let stale_owner = node_identity();
        seed_node(&store.db, &stale_owner, true).await;

        let claim_only = sm_entity("claim-only-cleanup");
        store
            .acquire(&claim_only, &stale_owner)
            .await
            .expect("seed valid claim without durable state");
        let wrong_prefix = "wrong-prefix:must-not-touch-durable";
        let conn = store.db.guard().await.expect("guard");
        conn.execute(
            r#"INSERT INTO clustering_claims
               (entity, entity_type, node_id, node_epoch, claim_epoch)
               VALUES (?, ?, ?, ?, 0)"#,
            crate::db_params![
                wrong_prefix,
                EntityType::SmSession.as_db_str().to_string(),
                stale_owner.node_id.clone(),
                stale_owner.node_epoch.clone(),
            ],
        )
        .await
        .expect("seed wrong-prefix claim");
        drop(conn);
        // A similarly named durable row proves wrong-prefix cleanup cannot
        // derive a suffix and delete persistence state.
        seed_sm_session_row(&store.db, "must-not-touch-durable").await;

        let page = store
            .list_orphaned_sm_session_claims_page(None, 64)
            .await
            .expect("classify stale rows");
        assert!(page.candidates.is_empty());
        assert_eq!(page.quarantined, 1, "only the malformed key is quarantine");

        let conn = store.db.guard().await.expect("guard");
        for encoded in [claim_only.to_string(), wrong_prefix.to_string()] {
            let mut rows = conn
                .query(
                    "SELECT COUNT(*) FROM clustering_claims WHERE entity = ?",
                    crate::db_params![encoded],
                )
                .await
                .expect("count claim");
            let count: i64 = rows
                .next()
                .await
                .expect("row")
                .expect("row")
                .get(0)
                .expect("count");
            assert_eq!(count, 0);
        }
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM sm_sessions WHERE stream_id = ?",
                crate::db_params!["must-not-touch-durable"],
            )
            .await
            .expect("count protected durable row");
        let durable_count: i64 = rows
            .next()
            .await
            .expect("row")
            .expect("row")
            .get(0)
            .expect("count");
        assert_eq!(durable_count, 1);
    }

    #[tokio::test]
    async fn claim_only_cleanup_rechecks_durable_absence_after_scan_classification() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let stale_owner = node_identity();
        seed_node(&store.db, &stale_owner, true).await;
        let entity = sm_entity("claim-only-raced-by-detach");
        let claim_epoch = store
            .acquire(&entity, &stale_owner)
            .await
            .expect("seed stale claim without durable state");

        // This cleanup value is the exact classification the unlocked page
        // scan produced while no sm_sessions row existed. A paused owner can
        // finish its already-fenced detach before cleanup locks the claim.
        let cleanup = OrphanedSmClaimCleanup {
            encoded: entity_key(&entity),
            owner: stale_owner.clone(),
            claim_epoch,
            durable_stream_id: None,
            malformed: false,
        };
        seed_sm_session_row(&store.db, &entity.id).await;

        assert!(!store
            .cleanup_orphaned_sm_claim(cleanup)
            .await
            .expect("cleanup transaction"));
        let snapshot = store
            .current_claim(&entity)
            .await
            .expect("read claim after cleanup");
        assert_eq!(
            snapshot.as_ref().map(|claim| (&claim.owner, claim.claim_epoch)),
            Some((&stale_owner, claim_epoch)),
            "a durable row appearing after scan classification must retain its exact claim for a later steal"
        );
        let conn = store.db.guard().await.expect("guard");
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM sm_sessions WHERE stream_id = ?",
                crate::db_params![entity.id],
            )
            .await
            .expect("count durable row");
        let durable_count: i64 = rows
            .next()
            .await
            .expect("row")
            .expect("row")
            .get(0)
            .expect("count");
        assert_eq!(durable_count, 1);
    }

    #[tokio::test]
    async fn claim_only_cleanup_observes_detach_committed_while_waiting_for_claim_lock() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let store = Arc::new(store);
        let stale_owner = node_identity();
        seed_node(&store.db, &stale_owner, true).await;
        let entity = sm_entity("claim-only-in-flight-detach");
        let claim_epoch = store
            .acquire(&entity, &stale_owner)
            .await
            .expect("seed stale claim without durable state");

        // Model a real fenced detach: hold FOR SHARE on the exact claim and
        // stage the durable row without committing it yet.
        let mut writer = store.db.begin().await.expect("begin detach writer");
        let mut fenced = writer
            .query(
                "SELECT 1 FROM clustering_claims WHERE entity = ? AND node_id = ? AND node_epoch = ? AND claim_epoch = ? FOR SHARE",
                crate::db_params![
                    entity_key(&entity),
                    stale_owner.node_id.clone(),
                    stale_owner.node_epoch.clone(),
                    claim_epoch.0,
                ],
            )
            .await
            .expect("lock exact claim for detach");
        assert!(fenced.next().await.expect("read fence").is_some());
        drop(fenced);
        writer
            .execute(
                r#"
                INSERT INTO sm_sessions (
                    stream_id, user_id, full_jid, inbound_count, outbound_count,
                    last_acked, max_resume_secs, detached_at_ms, max_resume_duration_ms,
                    carbons_enabled, roster_interested, blocklist_interested,
                    presence_available, presence_priority
                ) VALUES (?, ?, ?, 0, 0, 0, NULL, 0, 60000, 0, 0, 0, 0, 0)
                "#,
                crate::db_params![
                    entity.id.clone(),
                    "alice".to_string(),
                    "alice@example.com/web".to_string(),
                ],
            )
            .await
            .expect("stage durable detach row");

        let cleanup = OrphanedSmClaimCleanup {
            encoded: entity_key(&entity),
            owner: stale_owner.clone(),
            claim_epoch,
            durable_stream_id: None,
            malformed: false,
        };
        let cleanup_store = Arc::clone(&store);
        let cleanup_task =
            tokio::spawn(async move { cleanup_store.cleanup_orphaned_sm_claim(cleanup).await });

        // Do not release the writer until Postgres confirms the cleanup's
        // exact FOR UPDATE statement is waiting on that writer's share lock.
        let monitor = store.db.guard().await.expect("monitor guard");
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let mut rows = monitor
                    .query(
                        r#"
                        SELECT COUNT(*) FROM pg_stat_activity
                        WHERE pid <> pg_backend_pid()
                          AND query LIKE '%orphan_sm_cleanup_exact_lock%'
                          AND wait_event_type = 'Lock'
                        "#,
                        (),
                    )
                    .await
                    .expect("inspect blocked cleanup");
                let blocked = rows
                    .next()
                    .await
                    .expect("read blocked cleanup count")
                    .expect("blocked cleanup count row")
                    .get::<i64>(0)
                    .expect("blocked cleanup count");
                if blocked > 0 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("cleanup reached exact claim lock");
        drop(monitor);

        writer.commit().await.expect("commit durable detach");
        cleanup_task
            .await
            .expect("cleanup task joined")
            .expect("cleanup transaction");

        let snapshot = store
            .current_claim(&entity)
            .await
            .expect("read claim after cleanup");
        assert_eq!(
            snapshot
                .as_ref()
                .map(|claim| (&claim.owner, claim.claim_epoch)),
            Some((&stale_owner, claim_epoch)),
            "cleanup must preserve the exact claim once the blocked detach commits durable state"
        );
        let conn = store.db.guard().await.expect("guard");
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM sm_sessions WHERE stream_id = ?",
                crate::db_params![entity.id],
            )
            .await
            .expect("count durable row");
        assert_eq!(
            rows.next()
                .await
                .expect("row")
                .expect("row")
                .get::<i64>(0)
                .expect("count"),
            1
        );
    }
}
