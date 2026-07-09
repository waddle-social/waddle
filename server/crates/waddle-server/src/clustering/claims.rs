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
    ClaimEpoch, ClaimError, ClaimGrant, ClaimStore, Entity, EntityType, NodeIdentity,
    ResumeIdentityProof, StalePredicate,
};

use crate::db::{Database, DatabaseError, Transaction};

/// Convert a backend database failure into the upstream `ClaimError`. The
/// concrete diagnostic (`DatabaseError`'s `Display`) is preserved as
/// human-facing text; see [`ClaimError::Backend`]'s doc comment for why a
/// richer, `waddle-server`-local error type can't cross this boundary.
fn db_err(error: DatabaseError) -> ClaimError {
    if matches!(
        &error,
        DatabaseError::Internal(sqlx::Error::Database(inner))
            if inner.code().as_deref() == Some("2200H")
    ) {
        return ClaimError::EpochExhausted;
    }
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
    Some(Entity::new(entity_type, id))
}

#[cfg(test)]
mod entity_key_tests {
    use super::entity_key;
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
        let cases = [
            Entity::new(EntityType::UserActor, "42"),
            Entity::new(EntityType::RoomActor, "room_actor:42"),
            Entity::new(EntityType::SmSession, "sm_session:sm_session:x"),
            Entity::new(EntityType::UserActor, ""),
            Entity::new(EntityType::RoomActor, ":"),
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
}

/// Postgres implementation of `ClaimStore`, backing `UserActor`/`RoomActor`/
/// SM-session ownership.
pub struct PostgresClaimStore {
    db: Database,
    /// Lease deadline stamped on every node incarnation registered through
    /// this handle. Persisting the configured value with the row lets every
    /// ownership and durable-write fence reject a process that wakes after
    /// its deadline but before a watchdog has committed `expired = true`.
    lease_ttl: Duration,
}

impl PostgresClaimStore {
    const DEFAULT_LEASE_TTL: Duration = Duration::from_secs(30);

    pub fn new(db: Database) -> Self {
        Self::with_lease_ttl(db, Self::DEFAULT_LEASE_TTL)
    }

    /// Construct a store whose node registrations carry the exact configured
    /// node-lease deadline. Production must use this constructor; `new` keeps
    /// legacy test fixtures concise at the shipped 30-second default.
    pub fn with_lease_ttl(db: Database, lease_ttl: Duration) -> Self {
        Self { db, lease_ttl }
    }

    /// Exact incarnation currently occupying a stable node id, including an
    /// expired row. Startup uses this only to build an expected-old CAS; the
    /// row is never replaced from an unlocked observation alone.
    pub(crate) async fn registered_identity(
        &self,
        node_id: &str,
    ) -> Result<Option<NodeIdentity>, ClaimError> {
        let conn = self.db.control_plane_guard().await.map_err(db_err)?;
        let mut rows = conn
            .query(
                "SELECT node_epoch FROM clustering_nodes WHERE node_id = ?",
                crate::db_params![node_id.to_string()],
            )
            .await
            .map_err(db_err)?;
        Ok(rows
            .next()
            .await
            .map_err(db_err)?
            .map(|row| row.get::<String>(0))
            .transpose()
            .map_err(db_err)?
            .map(|epoch| NodeIdentity::new(node_id, epoch)))
    }
}

#[derive(Clone, Copy)]
enum GrantDestinationPolicy {
    Active,
    DrainingRecovery,
    ActiveOrDraining,
}

/// Lock and validate the destination incarnation after the claim mutation has
/// obtained the entity row lock. The mutation is still uncommitted, so a
/// failed proof rolls it back. This ordering matters: validating the node
/// first lets a claim-row wait consume the whole lease while the early proof
/// remains materialized as true.
async fn validate_grant_destination(
    tx: &mut Transaction<'_>,
    me: &NodeIdentity,
    policy: GrantDestinationPolicy,
    caller_ttl: Option<Duration>,
) -> Result<bool, ClaimError> {
    let policy = match policy {
        GrantDestinationPolicy::Active => "active",
        GrantDestinationPolicy::DrainingRecovery => "draining",
        GrantDestinationPolicy::ActiveOrDraining => "either",
    };
    let (ttl_mode, ttl_ms) = caller_ttl
        .map(|ttl| ("bounded", ttl.as_millis().to_string()))
        .unwrap_or(("row", "0".to_string()));
    let mut rows = tx
        .query(
            r#"
            WITH locked AS MATERIALIZED (
                SELECT node_id, node_epoch, heartbeat, expired, draining, lease_ttl_ms
                FROM clustering_nodes
                WHERE node_id = ? AND node_epoch = ?
                FOR SHARE
            )
            SELECT EXISTS (
                SELECT 1 FROM locked
                WHERE NOT expired
                  AND (? = 'either'
                       OR (? = 'active' AND NOT draining)
                       OR (? = 'draining' AND draining))
                  AND heartbeat >= clock_timestamp()
                      - (lease_ttl_ms::text || ' milliseconds')::interval
                  AND (? = 'row' OR heartbeat >= clock_timestamp()
                      - (? || ' milliseconds')::interval)
            )
            "#,
            crate::db_params![
                me.node_id.clone(),
                me.node_epoch.clone(),
                policy.to_string(),
                policy.to_string(),
                policy.to_string(),
                ttl_mode.to_string(),
                ttl_ms,
            ],
        )
        .await
        .map_err(db_err)?;
    rows.next()
        .await
        .map_err(db_err)?
        .map(|row| row.get::<bool>(0).map_err(db_err))
        .transpose()
        .map(|eligible| eligible.unwrap_or(false))
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
                -- Exact deadline for this incarnation. Authority checks use
                -- this row-local value so mixed/rolling configuration cannot
                -- accidentally grant a longer lease than the owner registered.
                lease_ttl_ms      BIGINT NOT NULL DEFAULT 30000,
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
        conn.execute(
            "ALTER TABLE clustering_nodes ADD COLUMN IF NOT EXISTS lease_ttl_ms BIGINT NOT NULL DEFAULT 30000",
            (),
        )
        .await
        .map_err(db_err)?;
        conn.execute(
            r#"
            CREATE TABLE IF NOT EXISTS clustering_claims (
                entity      TEXT PRIMARY KEY,
                entity_type TEXT NOT NULL,
                node_id     TEXT NOT NULL,
                node_epoch  TEXT NOT NULL,
                claim_epoch BIGINT NOT NULL DEFAULT 0
            )
            "#,
            (),
        )
        .await
        .map_err(db_err)?;
        conn.execute(
            r#"
            CREATE INDEX IF NOT EXISTS clustering_claims_node_id_node_epoch
                ON clustering_claims (node_id, node_epoch)
            "#,
            (),
        )
        .await
        .map_err(db_err)?;
        // Global, never-reset claim generations close release/reacquire ABA:
        // deleting a claim row must not make a later grant reuse epoch 0 (or
        // any earlier epoch). The singleton seed row is published in the
        // same statement that advances the sequence above every legacy row;
        // grant statements take a share lock on it before calling `nextval`,
        // so concurrent startup cannot allocate before seeding commits.
        conn.execute(
            "CREATE SEQUENCE IF NOT EXISTS clustering_claim_epoch_seq AS BIGINT",
            (),
        )
        .await
        .map_err(db_err)?;
        conn.execute(
            r#"
            CREATE TABLE IF NOT EXISTS clustering_claim_epoch_seed (
                singleton BOOLEAN PRIMARY KEY CHECK (singleton)
            )
            "#,
            (),
        )
        .await
        .map_err(db_err)?;
        conn.execute(
            r#"
            WITH won AS (
                INSERT INTO clustering_claim_epoch_seed (singleton)
                VALUES (TRUE)
                ON CONFLICT (singleton) DO NOTHING
                RETURNING 1
            )
            SELECT CASE
                WHEN EXISTS (SELECT 1 FROM won) THEN setval(
                    'clustering_claim_epoch_seq',
                    GREATEST(
                        (SELECT COALESCE(MAX(claim_epoch), 0) FROM clustering_claims),
                        nextval('clustering_claim_epoch_seq')
                    )
                )
                ELSE NULL
            END
            "#,
            (),
        )
        .await
        .map_err(db_err)?;
        // ADR-0017 Phase 3 Slice 3: steal-intents unwedge/owner-veto path
        // (element 4's "Unwedge" text, quoted verbatim in the phase plan).
        // `UNIQUE (entity, reporter_node)` + the upsert in
        // `report_steal_intent` collapses repeated failures from one
        // reporter against one entity into a single refreshed row rather
        // than growing unbounded during a sustained relay fault.
        conn.execute(
            r#"
            CREATE TABLE IF NOT EXISTS clustering_steal_intents (
                entity             TEXT NOT NULL,
                reporter_node      TEXT NOT NULL,
                reporter_epoch     TEXT NOT NULL,
                target_node        TEXT NOT NULL,
                target_node_epoch  TEXT NOT NULL,
                target_claim_epoch BIGINT NOT NULL,
                created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
                UNIQUE (entity, reporter_node)
            )
            "#,
            (),
        )
        .await
        .map_err(db_err)?;
        for ddl in [
            "ALTER TABLE clustering_steal_intents ADD COLUMN IF NOT EXISTS reporter_epoch TEXT",
            "ALTER TABLE clustering_steal_intents ADD COLUMN IF NOT EXISTS target_node TEXT",
            "ALTER TABLE clustering_steal_intents ADD COLUMN IF NOT EXISTS target_node_epoch TEXT",
            "ALTER TABLE clustering_steal_intents ADD COLUMN IF NOT EXISTS target_claim_epoch BIGINT",
        ] {
            conn.execute(ddl, ()).await.map_err(db_err)?;
        }
        // Legacy intent rows cannot be made safe: they identify neither the
        // reporter incarnation nor the target grant. Drop only those
        // unbound rows during the one-time schema transition, then make the
        // exact bindings mandatory for every future report.
        conn.execute(
            "DELETE FROM clustering_steal_intents WHERE reporter_epoch IS NULL OR target_node IS NULL OR target_node_epoch IS NULL OR target_claim_epoch IS NULL",
            (),
        )
        .await
        .map_err(db_err)?;
        for ddl in [
            "ALTER TABLE clustering_steal_intents ALTER COLUMN reporter_epoch SET NOT NULL",
            "ALTER TABLE clustering_steal_intents ALTER COLUMN target_node SET NOT NULL",
            "ALTER TABLE clustering_steal_intents ALTER COLUMN target_node_epoch SET NOT NULL",
            "ALTER TABLE clustering_steal_intents ALTER COLUMN target_claim_epoch SET NOT NULL",
        ] {
            conn.execute(ddl, ()).await.map_err(db_err)?;
        }
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
        let mut tx = self.db.control_plane_begin_fenced().await.map_err(db_err)?;
        // Acquire CAS (element 4): a fresh claim only inserts; a
        // still-live claim on the same entity leaves the row untouched and
        // affects zero rows.
        //
        // ADR-0017 Phase 3 Slice 10 + stable-node-id hardening: the `INSERT
        // ... SELECT ... WHERE EXISTS (exact live incarnation)` guard makes
        // "only the current non-expired, non-draining epoch acquires a NEW
        // claim" atomic with the CAS itself — never a separate
        // check-then-act read, which would leave a TOCTOU window between
        // observing "not draining" and the INSERT actually landing. A
        // draining node's own `mark_draining` UPDATE (issued once, at the
        // start of its shutdown drain sequence) is a single autocommit
        // statement on the same control-plane pool, so it is visible to
        // every subsequent `acquire`/`steal_stale` call under ordinary
        // READ COMMITTED semantics by the time this node's drain loop
        // itself proceeds to iterate owned entities.
        // A missing claim row cannot itself be locked. Serialize the absence
        // with a transaction-scoped advisory lock derived from the injective
        // entity key; hash collisions only reduce concurrency, never safety.
        tx.execute(
            "SELECT pg_advisory_xact_lock(hashtextextended(?, 8717))",
            crate::db_params![entity_key(entity)],
        )
        .await
        .map_err(db_err)?;

        let mut granted_rows = tx
            .query(
                r#"
                WITH epoch_seed AS MATERIALIZED (
                    SELECT 1 FROM clustering_claim_epoch_seed
                    WHERE singleton
                    FOR SHARE
                ),
                granted AS (
                    INSERT INTO clustering_claims (entity, entity_type, node_id, node_epoch, claim_epoch)
                    SELECT ?, ?, ?, ?, nextval('clustering_claim_epoch_seq')
                    FROM epoch_seed
                    ON CONFLICT (entity) DO NOTHING
                    RETURNING claim_epoch
                )
                SELECT claim_epoch FROM granted
                "#,
                crate::db_params![
                    entity_key(entity),
                    entity.entity_type.as_db_str().to_string(),
                    me.node_id.clone(),
                    me.node_epoch.clone(),
                ],
            )
            .await
            .map_err(db_err)?;
        if let Some(row) = granted_rows.next().await.map_err(db_err)? {
            let epoch = ClaimEpoch(row.get::<i64>(0).map_err(db_err)?);
            drop(granted_rows);
            if !validate_grant_destination(&mut tx, me, GrantDestinationPolicy::Active, None)
                .await?
            {
                return Err(ClaimError::Draining);
            }
            tx.execute(
                "DELETE FROM clustering_steal_intents WHERE entity = ?",
                crate::db_params![entity_key(entity)],
            )
            .await
            .map_err(db_err)?;
            tx.commit().await.map_err(db_err)?;
            return Ok(epoch);
        }
        drop(granted_rows);
        // Zero rows affected: either a genuine conflict (someone already
        // holds this entity) or this exact node incarnation is not eligible
        // to acquire. Distinguish with one follow-up read — only ever taken
        // on this already-cold "lost the race" path.
        let mut rows = tx
            .query(
                "SELECT EXISTS (SELECT 1 FROM clustering_claims WHERE entity = ?)",
                crate::db_params![entity_key(entity)],
            )
            .await
            .map_err(db_err)?;
        match rows.next().await.map_err(db_err)? {
            Some(row) => {
                let claimed: bool = row.get(0).map_err(db_err)?;
                if claimed {
                    Err(ClaimError::AlreadyClaimed)
                } else if !validate_grant_destination(
                    &mut tx,
                    me,
                    GrantDestinationPolicy::Active,
                    None,
                )
                .await?
                {
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
                        r#"
                        SELECT
                            c.node_id,
                            c.node_epoch,
                            c.claim_epoch,
                            EXISTS (
                                SELECT 1 FROM clustering_nodes n
                                WHERE n.node_id = c.node_id
                                  AND n.node_epoch = c.node_epoch
                                  AND NOT n.expired
                                  AND n.heartbeat >= now() - (n.lease_ttl_ms::text || ' milliseconds')::interval
                            ) AS owner_incarnation_live
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
                        let owner_incarnation_live: bool = row.get(3).map_err(db_err)?;
                        if node_id == me.node_id
                            && node_epoch == me.node_epoch
                            && owner_incarnation_live
                        {
                            // Self-reacquire: this exact node/epoch already
                            // holds the claim (either the losing side of a
                            // concurrent first-write race against itself, or
                            // a later slice's `<enable/>`-time acquire under
                            // this same identity) — idempotent, not a
                            // conflict.
                            Ok(ClaimEpoch(claim_epoch))
                        } else if node_id == me.node_id && node_epoch == me.node_epoch {
                            // The claim still names this caller's stale epoch,
                            // but the corresponding node incarnation is gone
                            // or expired. Never resurrect it through the
                            // idempotent self-reacquire path.
                            Err(err)
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
                let mut tx = self.db.control_plane_begin_fenced().await.map_err(db_err)?;
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
                // Acquire every existing intent-row lock before deciding
                // whether its age has crossed the threshold. PostgreSQL may
                // evaluate a DELETE predicate before waiting on a row lock;
                // the separate lock phase makes `clock_timestamp()` below a
                // genuinely post-wait freshness decision.
                let mut intent_rows = tx
                    .query(
                        "SELECT 1 FROM clustering_steal_intents WHERE entity = ? ORDER BY reporter_node FOR UPDATE",
                        crate::db_params![entity_key(entity)],
                    )
                    .await
                    .map_err(db_err)?;
                while intent_rows.next().await.map_err(db_err)?.is_some() {}
                drop(intent_rows);

                let mut granted_rows = tx
                    .query(
                        r#"
                        WITH epoch_seed AS MATERIALIZED (
                            SELECT 1 FROM clustering_claim_epoch_seed
                            WHERE singleton
                            FOR SHARE
                        ),
                        consumed AS (
                            DELETE FROM clustering_steal_intents si
                            USING clustering_claims c
                            WHERE si.entity = ?
                              AND c.entity = si.entity
                              AND c.claim_epoch = ?
                              AND si.target_node = c.node_id
                              AND si.target_node_epoch = c.node_epoch
                              AND si.target_claim_epoch = c.claim_epoch
                              AND si.created_at < clock_timestamp() - (? || ' milliseconds')::interval
                            RETURNING 1
                        ),
                        granted AS (
                            UPDATE clustering_claims
                            SET node_id = ?,
                                node_epoch = ?,
                                claim_epoch = nextval('clustering_claim_epoch_seq')
                            WHERE entity = ?
                              AND claim_epoch = ?
                              AND EXISTS (SELECT 1 FROM consumed)
                              AND EXISTS (SELECT 1 FROM epoch_seed)
                            RETURNING claim_epoch
                        ),
                        cleared AS (
                            DELETE FROM clustering_steal_intents
                            WHERE entity = ?
                              AND EXISTS (SELECT 1 FROM granted)
                        )
                        SELECT claim_epoch FROM granted
                        "#,
                        crate::db_params![
                            entity_key(entity),
                            observed.0,
                            intent_ttl.as_millis().to_string(),
                            me.node_id.clone(),
                            me.node_epoch.clone(),
                            entity_key(entity),
                            observed.0,
                            entity_key(entity),
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
                if let Some(row) = granted_rows.next().await.map_err(db_err)? {
                    let epoch = ClaimEpoch(row.get::<i64>(0).map_err(db_err)?);
                    drop(granted_rows);
                    if !validate_grant_destination(
                        &mut tx,
                        me,
                        GrantDestinationPolicy::Active,
                        None,
                    )
                    .await?
                    {
                        return Err(ClaimError::Conflict);
                    }
                    tx.commit().await.map_err(db_err)?;
                    Ok(epoch)
                } else {
                    Err(ClaimError::Conflict)
                }
            }
            StalePredicate::OwnerStale => {
                let mut tx = self.db.control_plane_begin_fenced().await.map_err(db_err)?;
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
                let mut granted_rows = tx
                    .query(
                        r#"
                        WITH epoch_seed AS MATERIALIZED (
                            SELECT 1 FROM clustering_claim_epoch_seed
                            WHERE singleton
                            FOR SHARE
                        ),
                        granted AS (
                            UPDATE clustering_claims
                            SET node_id = ?,
                                node_epoch = ?,
                                claim_epoch = nextval('clustering_claim_epoch_seq')
                            WHERE entity = ?
                              AND claim_epoch = ?
                              AND NOT EXISTS (
                                SELECT 1 FROM clustering_nodes n
                                WHERE n.node_id = clustering_claims.node_id
                                  AND NOT n.expired
                                  AND n.node_epoch = clustering_claims.node_epoch
                              )
                              AND EXISTS (SELECT 1 FROM epoch_seed)
                            RETURNING claim_epoch
                        ),
                        cleared AS (
                            DELETE FROM clustering_steal_intents
                            WHERE entity = ?
                              AND EXISTS (SELECT 1 FROM granted)
                        )
                        SELECT claim_epoch FROM granted
                        "#,
                        crate::db_params![
                            me.node_id.clone(),
                            me.node_epoch.clone(),
                            entity_key(entity),
                            observed.0,
                            entity_key(entity),
                        ],
                    )
                    .await
                    .map_err(db_err)?;
                if let Some(row) = granted_rows.next().await.map_err(db_err)? {
                    let epoch = ClaimEpoch(row.get::<i64>(0).map_err(db_err)?);
                    drop(granted_rows);
                    if !validate_grant_destination(
                        &mut tx,
                        me,
                        GrantDestinationPolicy::Active,
                        None,
                    )
                    .await?
                    {
                        return Err(ClaimError::Conflict);
                    }
                    tx.commit().await.map_err(db_err)?;
                    Ok(epoch)
                } else {
                    Err(ClaimError::Conflict)
                }
            }
        }
    }

    async fn reclaim_after_self_fence(
        &self,
        entity: &Entity,
        observed: ClaimEpoch,
        expected_owner: &NodeIdentity,
        me: &NodeIdentity,
        lease_ttl: Duration,
    ) -> Result<ClaimEpoch, ClaimError> {
        if expected_owner.node_id != me.node_id {
            return Err(ClaimError::Conflict);
        }
        let mut tx = self.db.control_plane_begin_fenced().await.map_err(db_err)?;
        let mut granted_rows = tx
            .query(
                r#"
                WITH epoch_seed AS MATERIALIZED (
                    SELECT 1 FROM clustering_claim_epoch_seed
                    WHERE singleton
                    FOR SHARE
                ),
                granted AS (
                    UPDATE clustering_claims
                    SET node_id = ?,
                        node_epoch = ?,
                        claim_epoch = nextval('clustering_claim_epoch_seq')
                    WHERE entity = ?
                      AND node_id = ?
                      AND node_epoch = ?
                      AND claim_epoch = ?
                      AND NOT EXISTS (
                        SELECT 1 FROM clustering_nodes n
                        WHERE n.node_id = clustering_claims.node_id
                          AND n.node_epoch = clustering_claims.node_epoch
                          AND NOT n.expired
                      )
                      AND EXISTS (SELECT 1 FROM epoch_seed)
                    RETURNING claim_epoch
                ),
                cleared AS (
                    DELETE FROM clustering_steal_intents
                    WHERE entity = ?
                      AND EXISTS (SELECT 1 FROM granted)
                )
                SELECT claim_epoch FROM granted
                "#,
                crate::db_params![
                    me.node_id.clone(),
                    me.node_epoch.clone(),
                    entity_key(entity),
                    expected_owner.node_id.clone(),
                    expected_owner.node_epoch.clone(),
                    observed.0,
                    entity_key(entity),
                ],
            )
            .await
            .map_err(db_err)?;
        if let Some(row) = granted_rows.next().await.map_err(db_err)? {
            let epoch = ClaimEpoch(row.get::<i64>(0).map_err(db_err)?);
            drop(granted_rows);
            if !validate_grant_destination(
                &mut tx,
                me,
                GrantDestinationPolicy::DrainingRecovery,
                Some(lease_ttl),
            )
            .await?
            {
                return Err(ClaimError::Conflict);
            }
            tx.commit().await.map_err(db_err)?;
            Ok(epoch)
        } else {
            Err(ClaimError::Conflict)
        }
    }

    async fn steal_for_resume(
        &self,
        entity: &Entity,
        observed: ClaimEpoch,
        _witness: ResumeIdentityProof,
        me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        let mut tx = self.db.control_plane_begin_fenced().await.map_err(db_err)?;
        // Consent/epoch-only steal CAS (element 4's third variant): no
        // OWNER-staleness predicate — authorized by the caller already
        // holding a `ResumeIdentityProof`. The destination incarnation must
        // nevertheless still have an exact, non-expired, heartbeat-fresh
        // node row; otherwise
        // a task carrying a pre-fence SharedNodeIdentity could resurrect an
        // immediately orphaned claim after this process rotates its epoch.
        // Draining remains allowed for this consent path so resumable
        // sessions already in flight can finish during an orderly drain.
        let mut granted_rows = tx
            .query(
                r#"
                WITH epoch_seed AS MATERIALIZED (
                    SELECT 1 FROM clustering_claim_epoch_seed
                    WHERE singleton
                    FOR SHARE
                ),
                granted AS (
                    UPDATE clustering_claims
                    SET node_id = ?,
                        node_epoch = ?,
                        claim_epoch = nextval('clustering_claim_epoch_seq')
                    WHERE entity = ?
                      AND claim_epoch = ?
                      AND EXISTS (SELECT 1 FROM epoch_seed)
                    RETURNING claim_epoch
                ),
                cleared AS (
                    DELETE FROM clustering_steal_intents
                    WHERE entity = ?
                      AND EXISTS (SELECT 1 FROM granted)
                )
                SELECT claim_epoch FROM granted
                "#,
                crate::db_params![
                    me.node_id.clone(),
                    me.node_epoch.clone(),
                    entity_key(entity),
                    observed.0,
                    entity_key(entity),
                ],
            )
            .await
            .map_err(db_err)?;
        if let Some(row) = granted_rows.next().await.map_err(db_err)? {
            let epoch = ClaimEpoch(row.get::<i64>(0).map_err(db_err)?);
            drop(granted_rows);
            if !validate_grant_destination(
                &mut tx,
                me,
                GrantDestinationPolicy::ActiveOrDraining,
                None,
            )
            .await?
            {
                return Err(ClaimError::Conflict);
            }
            tx.commit().await.map_err(db_err)?;
            Ok(epoch)
        } else {
            Err(ClaimError::Conflict)
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
        // `owner_lease_fresh` is serving authority, not steal authority: it
        // requires the exact non-expired incarnation and its row-local
        // heartbeat deadline. `steal_stale(OwnerStale)` deliberately remains
        // stricter and consumes only committed `expired`; a raw deadline
        // lapse can fail closed without letting another node steal.
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
                          AND n.heartbeat >= now() - (n.lease_ttl_ms::text || ' milliseconds')::interval
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

    async fn owned_claims(
        &self,
        entities: &[Entity],
        me: &NodeIdentity,
    ) -> Result<Vec<ClaimGrant>, ClaimError> {
        if entities.is_empty() {
            return Ok(Vec::new());
        }
        let by_key = entities
            .iter()
            .cloned()
            .map(|entity| (entity_key(&entity), entity))
            .collect::<std::collections::HashMap<_, _>>();
        let requested = serde_json::to_string(&by_key.keys().collect::<Vec<_>>())
            .map_err(|error| ClaimError::Backend(error.to_string()))?;
        let conn = self.db.control_plane_guard().await.map_err(db_err)?;
        let mut rows = conn
            .query(
                r#"
                WITH requested AS MATERIALIZED (
                    SELECT jsonb_array_elements_text(?::jsonb) AS entity
                )
                SELECT c.entity, c.claim_epoch
                FROM clustering_claims c
                JOIN requested r ON r.entity = c.entity
                JOIN clustering_nodes n
                  ON n.node_id = c.node_id
                 AND n.node_epoch = c.node_epoch
                WHERE c.node_id = ?
                  AND c.node_epoch = ?
                  AND NOT n.expired
                  AND n.heartbeat >= now() - (n.lease_ttl_ms::text || ' milliseconds')::interval
                "#,
                crate::db_params![requested, me.node_id.clone(), me.node_epoch.clone(),],
            )
            .await
            .map_err(db_err)?;
        let mut grants = Vec::new();
        while let Some(row) = rows.next().await.map_err(db_err)? {
            let key = row.get::<String>(0).map_err(db_err)?;
            let epoch = ClaimEpoch(row.get::<i64>(1).map_err(db_err)?);
            if let Some(entity) = by_key.get(&key) {
                grants.push(ClaimGrant::new(entity.clone(), me.clone(), epoch));
            }
        }
        Ok(grants)
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
                SELECT 1
                FROM clustering_claims c
                JOIN clustering_nodes n
                  ON n.node_id = c.node_id
                 AND n.node_epoch = c.node_epoch
                WHERE c.entity = ?
                  AND c.node_id = ?
                  AND c.node_epoch = ?
                  AND c.claim_epoch = ?
                  AND NOT n.expired
                  AND n.heartbeat >= now() - (n.lease_ttl_ms::text || ' milliseconds')::interval
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
        let mut tx = self.db.control_plane_begin_fenced().await.map_err(db_err)?;
        // Epoch-gated release: best-effort. A claim already stolen out
        // from under `me` (0 rows affected) is a no-op, not an error —
        // graceful drain releases whatever it still owns and does not
        // treat a lost race as a failure.
        let affected = tx
            .execute(
                r#"
                WITH live_owner AS MATERIALIZED (
                    SELECT 1 FROM clustering_nodes
                    WHERE node_id = ?
                      AND node_epoch = ?
                      AND NOT expired
                    FOR SHARE
                )
                DELETE FROM clustering_claims
                WHERE entity = ? AND node_id = ? AND node_epoch = ? AND claim_epoch = ?
                  AND EXISTS (SELECT 1 FROM live_owner)
                "#,
                crate::db_params![
                    me.node_id.clone(),
                    me.node_epoch.clone(),
                    entity_key(entity),
                    me.node_id.clone(),
                    me.node_epoch.clone(),
                    mine.0,
                ],
            )
            .await
            .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        if affected == 0 {
            tracing::debug!(
                entity = %entity.id,
                node_id = %me.node_id,
                claim_epoch = mine.0,
                "release: claim already gone (stolen or already released)"
            );
        }
        Ok(())
    }

    async fn release_many(&self, grants: &[ClaimGrant]) -> Result<(), ClaimError> {
        if grants.is_empty() {
            return Ok(());
        }
        let mut tx = self.db.control_plane_begin_fenced().await.map_err(db_err)?;
        // One round trip for the whole ~18k-claim modeled drain, with every
        // row matched by its exact grant. A JSON recordset uses one bind
        // parameter instead of four per grant; the latter would exceed
        // PostgreSQL's 65,535-parameter protocol limit at modeled scale.
        let requested = grants
            .iter()
            .map(|grant| {
                serde_json::json!({
                    "entity": entity_key(&grant.entity),
                    "node_id": grant.owner.node_id,
                    "node_epoch": grant.owner.node_epoch,
                    "claim_epoch": grant.epoch.0,
                })
            })
            .collect::<Vec<_>>();
        let requested = serde_json::to_string(&requested)
            .map_err(|error| ClaimError::Backend(error.to_string()))?;
        let sql = "WITH requested AS MATERIALIZED (\
                 SELECT entity, node_id, node_epoch, claim_epoch \
                 FROM jsonb_to_recordset(?::jsonb) AS r(\
                     entity TEXT, node_id TEXT, node_epoch TEXT, claim_epoch BIGINT\
                 )\
             ), \
             live_owners AS MATERIALIZED (\
                 SELECT n.node_id, n.node_epoch \
                 FROM clustering_nodes n \
                 WHERE NOT n.expired \
                   AND EXISTS (\
                       SELECT 1 FROM requested r \
                       WHERE r.node_id = n.node_id AND r.node_epoch = n.node_epoch\
                   ) \
                 FOR SHARE OF n\
             ) \
             DELETE FROM clustering_claims c \
             USING requested r, live_owners l \
             WHERE c.entity = r.entity \
               AND c.node_id = r.node_id \
               AND c.node_epoch = r.node_epoch \
               AND c.claim_epoch = r.claim_epoch \
               AND l.node_id = r.node_id \
               AND l.node_epoch = r.node_epoch";
        tx.execute(sql, crate::db_params![requested])
            .await
            .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
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
    /// under a new `node_epoch` after a self-fence (Q7/element 4).
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

    /// Initial process registration after the caller has proved any
    /// pre-existing exact incarnation stale. `expected_previous = None`
    /// means the stable node id must be absent; `Some` is an exact
    /// compare-and-swap source and may not match any later incarnation.
    async fn register_initial_with_peer_id(
        &self,
        expected_previous: Option<&NodeIdentity>,
        me: &NodeIdentity,
        pod_template_hash: Option<String>,
        peer_id: Option<String>,
    ) -> Result<(), ClaimError> {
        let _ = expected_previous;
        self.register_with_peer_id(me, pod_template_hash, peer_id)
            .await
    }

    /// Publish a post-fence incarnation as draining while recovery admission
    /// is still in progress. Production implementations must make the epoch
    /// upsert and `draining = true` one atomic statement: a candidate that has
    /// not passed hysteresis and terminal teardown must never be observable as
    /// a serving node.
    async fn register_draining_with_peer_id(
        &self,
        expected_previous: &NodeIdentity,
        me: &NodeIdentity,
        pod_template_hash: Option<String>,
        peer_id: Option<String>,
        _lease_ttl: Duration,
    ) -> Result<(), ClaimError> {
        let _ = expected_previous;
        self.register_with_peer_id(me, pod_template_hash, peer_id)
            .await?;
        self.mark_draining(me).await
    }

    /// Commit a draining post-fence incarnation to serving state. Returns
    /// `false` if the exact node/epoch row no longer exists or was expired.
    async fn activate(&self, _me: &NodeIdentity, _lease_ttl: Duration) -> Result<bool, ClaimError> {
        Ok(true)
    }

    /// Exact incarnation currently occupying `node_id`, including an
    /// expired row. Recovery uses this only to reconcile an ambiguous
    /// registration result; it never authorizes claim writes by itself.
    async fn registered_identity(
        &self,
        _node_id: &str,
    ) -> Result<Option<NodeIdentity>, ClaimError> {
        Ok(None)
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

    /// Refresh and prove a recovery candidate is serving-eligible in one
    /// exact-incarnation CAS. Unlike [`Self::heartbeat`], this rejects a
    /// draining row; recovery calls it immediately before publishing
    /// readiness after potentially long-running state hydration.
    async fn renew_active(
        &self,
        me: &NodeIdentity,
        lease_ttl: Duration,
    ) -> Result<bool, ClaimError> {
        self.is_fresh(me, lease_ttl).await
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
        target_owner: &NodeIdentity,
        target_epoch: ClaimEpoch,
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
    /// this store). Scoped to `sm_session` only: `UserActor`/`RoomActor`
    /// claim acquisition is out of this slice's scope (see
    /// `clustering::local_claims`'s module doc), so there is nothing of
    /// either other type to scan for yet.
    ///
    /// A row whose `entity` key does not decode cleanly against its own
    /// `entity_type` column (the same data-integrity anomaly
    /// [`decode_entity`] defensively rejects elsewhere) is skipped and
    /// logged rather than silently mangled — mirrors
    /// [`Self::owner_steal_intents`]'s handling.
    async fn list_orphaned_sm_session_claims(
        &self,
    ) -> Result<Vec<OrphanedSmSessionClaim>, ClaimError>;

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

#[derive(Clone, Copy)]
enum RegistrationPrecondition<'a> {
    Bootstrap,
    InitialReplacement(&'a NodeIdentity),
    Recovery(&'a NodeIdentity),
}

async fn register_node(
    store: &PostgresClaimStore,
    me: &NodeIdentity,
    pod_template_hash: Option<String>,
    peer_id: Option<String>,
    draining: bool,
    precondition: RegistrationPrecondition<'_>,
    exact_retry_ttl: Option<Duration>,
) -> Result<(), ClaimError> {
    // Runs on the control-plane pool (element 4/12, Slice 0): node
    // registration is liveness-control-plane traffic, never the main pool.
    if matches!(
        precondition,
        RegistrationPrecondition::InitialReplacement(previous)
            | RegistrationPrecondition::Recovery(previous)
            if previous.node_id != me.node_id
    ) {
        return Err(ClaimError::Conflict);
    }
    let mut tx = store
        .db
        .control_plane_begin_fenced()
        .await
        .map_err(db_err)?;
    tx.execute(
        "SELECT pg_advisory_xact_lock(hashtextextended(?, 8718))",
        crate::db_params![me.node_id.clone()],
    )
    .await
    .map_err(db_err)?;
    let mut locked = tx
        .query(
            "SELECT node_id FROM clustering_nodes WHERE node_id = ? FOR UPDATE",
            crate::db_params![me.node_id.clone()],
        )
        .await
        .map_err(db_err)?;
    let _ = locked.next().await.map_err(db_err)?;
    drop(locked);
    let values = if draining { "true" } else { "false" };
    let sql = format!(
        r#"
        INSERT INTO clustering_nodes (
            node_id, node_epoch, heartbeat, expired, pod_template_hash,
            draining, peer_id, lease_ttl_ms
        )
        VALUES (?, ?, clock_timestamp(), false, ?, {values}, ?, CAST(? AS BIGINT))
        ON CONFLICT (node_id) DO UPDATE SET
            node_epoch = EXCLUDED.node_epoch,
            heartbeat = clock_timestamp(),
            expired = false,
            pod_template_hash = EXCLUDED.pod_template_hash,
            lease_ttl_ms = EXCLUDED.lease_ttl_ms,
            -- An exact-identity retry is idempotent with respect to the
            -- lifecycle state. In particular, a delayed recovery-register
            -- retry must not flip an already-activated incarnation back to
            -- draining (and a delayed initial-register retry must not
            -- activate a recovery candidate).
            draining = CASE
                WHEN clustering_nodes.node_epoch = EXCLUDED.node_epoch
                    THEN clustering_nodes.draining
                ELSE EXCLUDED.draining
            END,
            peer_id = EXCLUDED.peer_id
        WHERE (clustering_nodes.node_epoch = EXCLUDED.node_epoch
               AND NOT clustering_nodes.expired
               AND clustering_nodes.heartbeat >= clock_timestamp() - (
                   clustering_nodes.lease_ttl_ms::text || ' milliseconds'
               )::interval
               AND (? = 'unbounded'
                    OR clustering_nodes.heartbeat >= clock_timestamp() - (? || ' milliseconds')::interval))
           OR (? = 'initial'
               AND clustering_nodes.node_epoch = ?
               AND clustering_nodes.expired)
           OR (? = 'recovery'
               AND clustering_nodes.node_epoch = ?)
        "#
    );
    let (mode, expected_epoch) = match precondition {
        RegistrationPrecondition::Bootstrap => ("bootstrap", String::new()),
        RegistrationPrecondition::InitialReplacement(previous) => {
            ("initial", previous.node_epoch.clone())
        }
        RegistrationPrecondition::Recovery(previous) => ("recovery", previous.node_epoch.clone()),
    };
    let (retry_mode, retry_ttl_ms) = match exact_retry_ttl {
        Some(ttl) => ("bounded", ttl.as_millis().to_string()),
        None => ("unbounded", "0".to_string()),
    };
    let affected = tx
        .execute(
            &sql,
            crate::db_params![
                me.node_id.clone(),
                me.node_epoch.clone(),
                pod_template_hash,
                peer_id,
                store.lease_ttl.as_millis().to_string(),
                retry_mode.to_string(),
                retry_ttl_ms,
                mode.to_string(),
                expected_epoch.clone(),
                mode.to_string(),
                expected_epoch,
            ],
        )
        .await
        .map_err(db_err)?;
    if affected == 1 {
        tx.commit().await.map_err(db_err)?;
        Ok(())
    } else {
        Err(ClaimError::Conflict)
    }
}

#[async_trait]
impl NodeLeaseStore for PostgresClaimStore {
    async fn register(
        &self,
        me: &NodeIdentity,
        pod_template_hash: Option<String>,
    ) -> Result<(), ClaimError> {
        register_node(
            self,
            me,
            pod_template_hash,
            None,
            false,
            RegistrationPrecondition::Bootstrap,
            Some(self.lease_ttl),
        )
        .await
    }

    async fn register_with_peer_id(
        &self,
        me: &NodeIdentity,
        pod_template_hash: Option<String>,
        peer_id: Option<String>,
    ) -> Result<(), ClaimError> {
        register_node(
            self,
            me,
            pod_template_hash,
            peer_id,
            false,
            RegistrationPrecondition::Bootstrap,
            Some(self.lease_ttl),
        )
        .await
    }

    async fn register_initial_with_peer_id(
        &self,
        expected_previous: Option<&NodeIdentity>,
        me: &NodeIdentity,
        pod_template_hash: Option<String>,
        peer_id: Option<String>,
    ) -> Result<(), ClaimError> {
        let precondition = expected_previous
            .map(RegistrationPrecondition::InitialReplacement)
            .unwrap_or(RegistrationPrecondition::Bootstrap);
        register_node(
            self,
            me,
            pod_template_hash,
            peer_id,
            false,
            precondition,
            Some(self.lease_ttl),
        )
        .await
    }

    async fn register_draining_with_peer_id(
        &self,
        expected_previous: &NodeIdentity,
        me: &NodeIdentity,
        pod_template_hash: Option<String>,
        peer_id: Option<String>,
        lease_ttl: Duration,
    ) -> Result<(), ClaimError> {
        register_node(
            self,
            me,
            pod_template_hash,
            peer_id,
            true,
            RegistrationPrecondition::Recovery(expected_previous),
            Some(lease_ttl),
        )
        .await
    }

    async fn activate(&self, me: &NodeIdentity, lease_ttl: Duration) -> Result<bool, ClaimError> {
        let mut tx = self.db.control_plane_begin_fenced().await.map_err(db_err)?;
        let affected = tx
            .execute(
                r#"
                WITH locked AS MATERIALIZED (
                    SELECT node_id, node_epoch
                    FROM clustering_nodes
                    WHERE node_id = ? AND node_epoch = ?
                    FOR UPDATE
                )
                UPDATE clustering_nodes AS n
                SET draining = false, heartbeat = clock_timestamp()
                FROM locked
                WHERE n.node_id = locked.node_id
                  AND n.node_epoch = locked.node_epoch
                  AND NOT n.expired
                  AND n.draining
                  AND n.heartbeat >= clock_timestamp() - (? || ' milliseconds')::interval
                  AND n.heartbeat >= clock_timestamp() - (n.lease_ttl_ms::text || ' milliseconds')::interval
                "#,
                crate::db_params![
                    me.node_id.clone(),
                    me.node_epoch.clone(),
                    lease_ttl.as_millis().to_string(),
                ],
            )
            .await
            .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        Ok(affected == 1)
    }

    async fn registered_identity(&self, node_id: &str) -> Result<Option<NodeIdentity>, ClaimError> {
        PostgresClaimStore::registered_identity(self, node_id).await
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
                  AND heartbeat >= now() - (lease_ttl_ms::text || ' milliseconds')::interval
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
        let mut tx = self.db.control_plane_begin_fenced().await.map_err(db_err)?;
        // Heartbeat CAS (element 4, locked verbatim): renew only while the
        // lease is still fresh under our own identity.
        let affected = tx
            .execute(
                r#"
                WITH locked AS MATERIALIZED (
                    SELECT node_id, node_epoch
                    FROM clustering_nodes
                    WHERE node_id = ? AND node_epoch = ?
                    FOR UPDATE
                )
                UPDATE clustering_nodes AS n
                SET heartbeat = clock_timestamp()
                FROM locked
                WHERE n.node_id = locked.node_id
                  AND n.node_epoch = locked.node_epoch
                  AND NOT n.expired
                  AND n.heartbeat >= clock_timestamp() - (? || ' milliseconds')::interval
                  AND n.heartbeat >= clock_timestamp() - (n.lease_ttl_ms::text || ' milliseconds')::interval
                "#,
                crate::db_params![
                    me.node_id.clone(),
                    me.node_epoch.clone(),
                    lease_ttl.as_millis().to_string(),
                ],
            )
            .await
            .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        Ok(affected == 1)
    }

    async fn expire(&self, owner: &NodeIdentity, lease_ttl: Duration) -> Result<bool, ClaimError> {
        let mut tx = self.db.control_plane_begin_fenced().await.map_err(db_err)?;
        // Expire CAS (element 4, locked verbatim): the single serialized
        // ordering point that makes lease expiry monotone — see this
        // module's own doc comment and `steal_stale`'s NOT EXISTS predicate,
        // which reads only the committed flag this statement sets.
        let affected = tx
            .execute(
                r#"
                WITH locked AS MATERIALIZED (
                    SELECT node_id, node_epoch
                    FROM clustering_nodes
                    WHERE node_id = ? AND node_epoch = ?
                    FOR UPDATE
                )
                UPDATE clustering_nodes AS n
                SET expired = true
                FROM locked
                WHERE n.node_id = locked.node_id
                  AND n.node_epoch = locked.node_epoch
                  AND NOT n.expired
                  AND n.heartbeat < clock_timestamp() - (? || ' milliseconds')::interval
                  AND n.heartbeat < clock_timestamp() - (n.lease_ttl_ms::text || ' milliseconds')::interval
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
            tx.commit().await.map_err(db_err)?;
            return Ok(true);
        }
        // We did not flip the flag ourselves — either it is already
        // committed true, the row is still fresh, or the row is gone
        // entirely. Distinguish those (a missing/expired row means "proceed,
        // this owner is stale" — the same vacuous-stale treatment
        // `steal_stale`'s NOT EXISTS predicate gives a vanished node).
        let mut rows = tx
            .query(
                "SELECT expired FROM clustering_nodes WHERE node_id = ? AND node_epoch = ?",
                crate::db_params![owner.node_id.clone(), owner.node_epoch.clone()],
            )
            .await
            .map_err(db_err)?;
        let result = match rows.next().await.map_err(db_err)? {
            Some(row) => row.get::<bool>(0).map_err(db_err),
            None => Ok(true),
        }?;
        drop(rows);
        tx.commit().await.map_err(db_err)?;
        Ok(result)
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
                  AND heartbeat < now() - (lease_ttl_ms::text || ' milliseconds')::interval
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
                  AND heartbeat >= now() - (lease_ttl_ms::text || ' milliseconds')::interval
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

    async fn renew_active(
        &self,
        me: &NodeIdentity,
        lease_ttl: Duration,
    ) -> Result<bool, ClaimError> {
        let mut tx = self.db.control_plane_begin_fenced().await.map_err(db_err)?;
        let affected = tx
            .execute(
                r#"
                WITH locked AS MATERIALIZED (
                    SELECT node_id, node_epoch
                    FROM clustering_nodes
                    WHERE node_id = ? AND node_epoch = ?
                    FOR UPDATE
                )
                UPDATE clustering_nodes AS n
                SET heartbeat = clock_timestamp()
                FROM locked
                WHERE n.node_id = locked.node_id
                  AND n.node_epoch = locked.node_epoch
                  AND NOT n.expired
                  AND NOT n.draining
                  AND n.heartbeat >= clock_timestamp() - (? || ' milliseconds')::interval
                  AND n.heartbeat >= clock_timestamp() - (n.lease_ttl_ms::text || ' milliseconds')::interval
                "#,
                crate::db_params![
                    me.node_id.clone(),
                    me.node_epoch.clone(),
                    lease_ttl.as_millis().to_string(),
                ],
            )
            .await
            .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        Ok(affected == 1)
    }

    async fn mark_draining(&self, me: &NodeIdentity) -> Result<(), ClaimError> {
        let mut tx = self.db.control_plane_begin_fenced().await.map_err(db_err)?;
        tx.execute(
            "UPDATE clustering_nodes SET draining = true WHERE node_id = ? AND node_epoch = ?",
            crate::db_params![me.node_id.clone(), me.node_epoch.clone()],
        )
        .await
        .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
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
                  AND heartbeat >= now() - (lease_ttl_ms::text || ' milliseconds')::interval
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
                r#"
                SELECT c.entity
                FROM clustering_claims c
                JOIN clustering_nodes n
                  ON n.node_id = c.node_id
                 AND n.node_epoch = c.node_epoch
                WHERE c.node_id = ?
                  AND c.node_epoch = ?
                  AND NOT n.expired
                  AND n.heartbeat >= now() - (n.lease_ttl_ms::text || ' milliseconds')::interval
                "#,
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
        target_owner: &NodeIdentity,
        target_epoch: ClaimEpoch,
        reporter: &NodeIdentity,
    ) -> Result<(), ClaimError> {
        if entity.entity_type == EntityType::SmSession {
            return Err(ClaimError::SmSessionExcludedFromStealIntent);
        }
        let mut tx = self.db.control_plane_begin_fenced().await.map_err(db_err)?;
        // Serialize reports even while the `(entity, reporter)` row is absent;
        // otherwise two first reports can both reach the unique insert and
        // make one statement's pre-conflict timestamp stale while it waits.
        tx.execute(
            "SELECT pg_advisory_xact_lock(hashtextextended(?, 8719))",
            crate::db_params![format!("{}:{}", entity_key(entity), reporter.node_id)],
        )
        .await
        .map_err(db_err)?;

        // Lock the exact reporter incarnation and target grant first, without
        // materializing a freshness decision before a later intent-row wait.
        let mut exact_rows = tx
            .query(
                r#"
                WITH locked_reporter AS MATERIALIZED (
                    SELECT 1
                    FROM clustering_nodes
                    WHERE node_id = ? AND node_epoch = ?
                    FOR SHARE
                ),
                locked_target AS MATERIALIZED (
                    SELECT 1
                    FROM clustering_claims c
                    JOIN clustering_nodes n
                      ON n.node_id = c.node_id
                     AND n.node_epoch = c.node_epoch
                    WHERE c.entity = ?
                      AND c.node_id = ?
                      AND c.node_epoch = ?
                      AND c.claim_epoch = ?
                    FOR SHARE OF c, n
                )
                SELECT EXISTS (SELECT 1 FROM locked_reporter)
                   AND EXISTS (SELECT 1 FROM locked_target)
                "#,
                crate::db_params![
                    reporter.node_id.clone(),
                    reporter.node_epoch.clone(),
                    entity_key(entity),
                    target_owner.node_id.clone(),
                    target_owner.node_epoch.clone(),
                    target_epoch.0,
                ],
            )
            .await
            .map_err(db_err)?;
        let exact = exact_rows
            .next()
            .await
            .map_err(db_err)?
            .map(|row| row.get::<bool>(0).map_err(db_err))
            .transpose()?
            .unwrap_or(false);
        drop(exact_rows);
        if !exact {
            return Err(ClaimError::Conflict);
        }

        // An existing row may be held by clear/steal. Wait for it explicitly;
        // only after this lock resolves do we evaluate wall-clock freshness
        // and choose the new intent age.
        let mut intent_rows = tx
            .query(
                "SELECT 1 FROM clustering_steal_intents WHERE entity = ? AND reporter_node = ? FOR UPDATE",
                crate::db_params![entity_key(entity), reporter.node_id.clone()],
            )
            .await
            .map_err(db_err)?;
        let _ = intent_rows.next().await.map_err(db_err)?;
        drop(intent_rows);

        let mut fresh_rows = tx
            .query(
                r#"
                SELECT
                    EXISTS (
                        SELECT 1 FROM clustering_nodes n
                        WHERE n.node_id = ? AND n.node_epoch = ?
                          AND NOT n.expired AND NOT n.draining
                          AND n.heartbeat >= clock_timestamp() - (
                              n.lease_ttl_ms::text || ' milliseconds'
                          )::interval
                    )
                    AND EXISTS (
                        SELECT 1
                        FROM clustering_claims c
                        JOIN clustering_nodes n
                          ON n.node_id = c.node_id AND n.node_epoch = c.node_epoch
                        WHERE c.entity = ?
                          AND c.node_id = ? AND c.node_epoch = ? AND c.claim_epoch = ?
                          AND NOT n.expired
                          AND n.heartbeat >= clock_timestamp() - (
                              n.lease_ttl_ms::text || ' milliseconds'
                          )::interval
                    )
                "#,
                crate::db_params![
                    reporter.node_id.clone(),
                    reporter.node_epoch.clone(),
                    entity_key(entity),
                    target_owner.node_id.clone(),
                    target_owner.node_epoch.clone(),
                    target_epoch.0,
                ],
            )
            .await
            .map_err(db_err)?;
        let fresh = fresh_rows
            .next()
            .await
            .map_err(db_err)?
            .map(|row| row.get::<bool>(0).map_err(db_err))
            .transpose()?
            .unwrap_or(false);
        drop(fresh_rows);
        if !fresh {
            return Err(ClaimError::Conflict);
        }

        tx.execute(
            r#"
            INSERT INTO clustering_steal_intents (
                entity, reporter_node, reporter_epoch, target_node,
                target_node_epoch, target_claim_epoch, created_at
            ) VALUES (?, ?, ?, ?, ?, ?, clock_timestamp())
            ON CONFLICT (entity, reporter_node) DO UPDATE SET
                reporter_epoch = EXCLUDED.reporter_epoch,
                target_node = EXCLUDED.target_node,
                target_node_epoch = EXCLUDED.target_node_epoch,
                target_claim_epoch = EXCLUDED.target_claim_epoch,
                created_at = EXCLUDED.created_at
            "#,
            crate::db_params![
                entity_key(entity),
                reporter.node_id.clone(),
                reporter.node_epoch.clone(),
                target_owner.node_id.clone(),
                target_owner.node_epoch.clone(),
                target_epoch.0,
            ],
        )
        .await
        .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
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
                JOIN clustering_steal_intents si
                  ON si.entity = c.entity
                 AND si.target_node = c.node_id
                 AND si.target_node_epoch = c.node_epoch
                 AND si.target_claim_epoch = c.claim_epoch
                JOIN clustering_nodes n
                  ON n.node_id = c.node_id
                 AND n.node_epoch = c.node_epoch
                WHERE c.node_id = ? AND c.node_epoch = ? AND c.entity_type != ?
                  AND NOT n.expired
                  AND n.heartbeat >= now() - (n.lease_ttl_ms::text || ' milliseconds')::interval
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
        let mut tx = self.db.control_plane_begin_fenced().await.map_err(db_err)?;
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
        // A deposed owner's stale `(node_id, node_epoch, mine)` tuple matches no
        // `clustering_claims` row, so `fenced` is empty, the DELETE's
        // `EXISTS (SELECT 1 FROM fenced)` is false, and it affects zero
        // rows — a silent no-op, exactly like `release`'s epoch-gated
        // semantics, but now observable by the caller via the returned
        // count (FIX 1(b): see the trait doc for why `run_node_lease`
        // needs to distinguish this from a genuine veto).
        let affected = tx
            .execute(
                r#"
                WITH locked_owner AS MATERIALIZED (
                    SELECT heartbeat, expired, lease_ttl_ms FROM clustering_nodes
                    WHERE node_id = ?
                      AND node_epoch = ?
                    FOR SHARE
                ),
                live_owner AS MATERIALIZED (
                    SELECT 1 FROM locked_owner
                    WHERE NOT expired
                      AND heartbeat >= clock_timestamp() - (lease_ttl_ms::text || ' milliseconds')::interval
                ),
                fenced AS MATERIALIZED (
                    SELECT 1 FROM clustering_claims
                    WHERE entity = ? AND node_id = ? AND node_epoch = ? AND claim_epoch = ?
                      AND EXISTS (SELECT 1 FROM live_owner)
                    FOR SHARE
                )
                DELETE FROM clustering_steal_intents
                WHERE entity = ?
                  AND target_node = ?
                  AND target_node_epoch = ?
                  AND target_claim_epoch = ?
                  AND EXISTS (SELECT 1 FROM fenced)
                "#,
                crate::db_params![
                    me.node_id.clone(),
                    me.node_epoch.clone(),
                    entity_key(entity),
                    me.node_id.clone(),
                    me.node_epoch.clone(),
                    mine.0,
                    entity_key(entity),
                    me.node_id.clone(),
                    me.node_epoch.clone(),
                    mine.0,
                ],
            )
            .await
            .map_err(|error| {
                log_if_postgres_deadlock(&error, &entity_key(entity), "clear_steal_intent");
                db_err(error)
            })?;
        tx.commit().await.map_err(db_err)?;
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
        // unhydratable session and break later XEP-0198 resume/ISR paths.
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
            let Some(entity) = decode_entity(&encoded, EntityType::SmSession) else {
                tracing::warn!(
                    encoded_entity = %encoded,
                    "list_orphaned_sm_session_claims: row's entity key does not decode \
                     against the sm_session tag; skipping (data-integrity anomaly)"
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

    async fn steal_orphaned_sm_session_claim(
        &self,
        entity: &Entity,
        observed: ClaimEpoch,
        me: &NodeIdentity,
        lease_ttl: Duration,
    ) -> Result<ClaimEpoch, ClaimError> {
        let mut tx = self.db.control_plane_begin_fenced().await.map_err(db_err)?;
        let mut granted_rows = tx
            .query(
                r#"
                WITH epoch_seed AS MATERIALIZED (
                    SELECT 1 FROM clustering_claim_epoch_seed
                    WHERE singleton
                    FOR SHARE
                ),
                granted AS (
                    UPDATE clustering_claims
                    SET node_id = ?,
                        node_epoch = ?,
                        claim_epoch = nextval('clustering_claim_epoch_seq')
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
                      AND EXISTS (SELECT 1 FROM epoch_seed)
                    RETURNING claim_epoch
                ),
                cleared AS (
                    DELETE FROM clustering_steal_intents
                    WHERE entity = ?
                      AND EXISTS (SELECT 1 FROM granted)
                )
                SELECT claim_epoch FROM granted
                "#,
                crate::db_params![
                    me.node_id.clone(),
                    me.node_epoch.clone(),
                    entity_key(entity),
                    EntityType::SmSession.as_db_str().to_string(),
                    observed.0,
                    EntityType::SmSession.as_db_str().to_string(),
                    entity_key(entity),
                ],
            )
            .await
            .map_err(db_err)?;
        if let Some(row) = granted_rows.next().await.map_err(db_err)? {
            let epoch = ClaimEpoch(row.get::<i64>(0).map_err(db_err)?);
            drop(granted_rows);
            if !validate_grant_destination(
                &mut tx,
                me,
                GrantDestinationPolicy::Active,
                Some(lease_ttl),
            )
            .await?
            {
                return Err(ClaimError::Conflict);
            }
            tx.commit().await.map_err(db_err)?;
            Ok(epoch)
        } else {
            Err(ClaimError::Conflict)
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
                  AND heartbeat >= now() - (lease_ttl_ms::text || ' milliseconds')::interval
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
        conn.execute("DELETE FROM clustering_nodes", ())
            .await
            .expect("clean nodes");
        conn.execute("DELETE FROM clustering_steal_intents", ())
            .await
            .expect("clean steal intents");
        Some(store)
    }

    async fn register_live_node(store: &PostgresClaimStore, identity: &NodeIdentity) {
        store
            .register(identity, None)
            .await
            .expect("register live test node");
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
    /// Several fixtures acquire a claim through the production API first,
    /// which now requires and therefore registers the exact node incarnation.
    /// Treat that same-incarnation row as an idempotent fixture update instead
    /// of issuing a second bare INSERT. A different epoch under the same stable
    /// node id remains a hard fixture error: silently replacing it here would
    /// bypass the production registration CAS this test suite is meant to
    /// exercise.
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
        let affected = conn
            .execute(
                &format!(
                    r#"
                INSERT INTO clustering_nodes (
                    node_id, node_epoch, heartbeat, expired, draining
                )
                VALUES (?, ?, clock_timestamp(), {expired_literal}, false)
                ON CONFLICT (node_id) DO UPDATE SET
                    heartbeat = clock_timestamp(),
                    expired = EXCLUDED.expired,
                    draining = false
                WHERE clustering_nodes.node_epoch = EXCLUDED.node_epoch
                "#
                ),
                crate::db_params![identity.node_id.clone(), identity.node_epoch.clone()],
            )
            .await
            .expect("seed node");
        assert_eq!(
            affected, 1,
            "seed_node must only insert or update the exact requested node incarnation"
        );
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

    async fn while_claim_is_locked_past_ttl<F, T>(
        db: &Database,
        entity: &Entity,
        ttl: Duration,
        operation: F,
    ) -> T
    where
        F: std::future::Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let mut blocker = db.begin().await.expect("begin claim blocker");
        let mut rows = blocker
            .query(
                "SELECT 1 FROM clustering_claims WHERE entity = ? FOR SHARE",
                crate::db_params![entity_key(entity)],
            )
            .await
            .expect("lock claim row");
        assert!(rows.next().await.expect("read lock row").is_some());
        drop(rows);
        let task = tokio::spawn(operation);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            !task.is_finished(),
            "claim operation must wait on the held row"
        );
        tokio::time::sleep(ttl * 3).await;
        blocker.commit().await.expect("release claim blocker");
        task.await.expect("join blocked claim operation")
    }

    async fn while_node_is_locked_past_ttl<F, T>(
        db: &Database,
        identity: &NodeIdentity,
        ttl: Duration,
        operation: F,
    ) -> T
    where
        F: std::future::Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let mut blocker = db.begin().await.expect("begin node blocker");
        let mut rows = blocker
            .query(
                "SELECT 1 FROM clustering_nodes WHERE node_id = ? AND node_epoch = ? FOR SHARE",
                crate::db_params![identity.node_id.clone(), identity.node_epoch.clone()],
            )
            .await
            .expect("lock node row");
        assert!(rows.next().await.expect("read lock row").is_some());
        drop(rows);
        let task = tokio::spawn(operation);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            !task.is_finished(),
            "node operation must wait on the held row"
        );
        tokio::time::sleep(ttl * 3).await;
        blocker.commit().await.expect("release node blocker");
        task.await.expect("join blocked node operation")
    }

    #[tokio::test]
    async fn acquire_succeeds_once_then_conflicts() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let me = node_identity();
        register_live_node(&store, &me).await;
        let entity = sm_entity("stream-1");
        let epoch = store.acquire(&entity, &me).await.expect("first acquire");
        assert!(epoch.0 >= 0, "the global claim generation must not wrap");

        let other = node_identity();
        register_live_node(&store, &other).await;
        let err = store
            .acquire(&entity, &other)
            .await
            .expect_err("second acquire loses the race");
        assert!(matches!(err, ClaimError::AlreadyClaimed));
    }

    #[tokio::test]
    async fn registration_persists_the_exact_configured_lease_ttl() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(clean) = clean_store().await else {
            return;
        };
        let configured_ttl = Duration::from_millis(12_345);
        let store = PostgresClaimStore::with_lease_ttl(clean.db.clone(), configured_ttl);
        let me = node_identity();
        store.register(&me, None).await.expect("register node");

        let conn = store.db.guard().await.expect("guard");
        let mut rows = conn
            .query(
                "SELECT lease_ttl_ms FROM clustering_nodes WHERE node_id = ? AND node_epoch = ?",
                crate::db_params![me.node_id.clone(), me.node_epoch.clone()],
            )
            .await
            .expect("query registered TTL");
        let persisted = rows
            .next()
            .await
            .expect("row read")
            .expect("registered row")
            .get::<i64>(0)
            .expect("lease_ttl_ms");
        assert_eq!(persisted, 12_345);
    }

    #[tokio::test]
    async fn lapsed_but_not_expired_incarnation_has_no_claim_or_relay_authority() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let me = node_identity();
        store
            .register_with_peer_id(&me, None, Some("peer-before-lapse".to_owned()))
            .await
            .expect("register live node");
        let owned = room_entity("lapsed-owned@muc.example.com");
        let epoch = store.acquire(&owned, &me).await.expect("initial acquire");

        // Leave the terminal bit deliberately untouched. Every authority
        // check must enforce the row-local deadline itself during the gap
        // before a watchdog commits `expired = true`.
        backdate_heartbeat(&store.db, &me).await;
        let conn = store.db.guard().await.expect("guard");
        let mut rows = conn
            .query(
                "SELECT expired FROM clustering_nodes WHERE node_id = ? AND node_epoch = ?",
                crate::db_params![me.node_id.clone(), me.node_epoch.clone()],
            )
            .await
            .expect("query terminal bit");
        assert!(!rows
            .next()
            .await
            .expect("row read")
            .expect("node row")
            .get::<bool>(0)
            .expect("expired"));
        drop(rows);
        drop(conn);

        let fresh = room_entity("lapsed-new@muc.example.com");
        assert!(matches!(
            store.acquire(&fresh, &me).await,
            Err(ClaimError::Draining)
        ));
        assert!(store.ensure_claimed(&owned, &me).await.is_err());
        let snapshot = store
            .current_claim(&owned)
            .await
            .expect("read claim")
            .expect("claim remains on file");
        assert_eq!(snapshot.owner, me);
        assert!(!snapshot.owner_lease_fresh);
        assert!(!store.fence(&owned, &me, epoch).await.expect("fence"));
        assert!(store
            .owned_claims(std::slice::from_ref(&owned), &me)
            .await
            .expect("owned snapshot")
            .is_empty());
        assert_eq!(
            store
                .reconcile(&me, std::slice::from_ref(&owned))
                .await
                .expect("reconcile"),
            vec![owned]
        );
        assert_eq!(
            store.peer_id_for_node(&me).await.expect("peer lookup"),
            None
        );
    }

    #[tokio::test]
    async fn every_claim_grant_cas_rejects_a_lapsed_nonexpired_destination() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };

        let stale_owner = node_identity();
        register_live_node(&store, &stale_owner).await;
        let stale_entity = room_entity("lapsed-owner-stale-steal@muc.example.com");
        let stale_epoch = store
            .acquire(&stale_entity, &stale_owner)
            .await
            .expect("owner acquires stale-steal fixture");
        store
            .db
            .guard()
            .await
            .expect("guard")
            .execute(
                "UPDATE clustering_nodes SET expired = true WHERE node_id = ? AND node_epoch = ?",
                crate::db_params![stale_owner.node_id.clone(), stale_owner.node_epoch.clone()],
            )
            .await
            .expect("commit source expiry");

        let resume_owner = node_identity();
        register_live_node(&store, &resume_owner).await;
        let resume_entity = sm_entity("lapsed-resume-destination");
        let resume_epoch = store
            .acquire(&resume_entity, &resume_owner)
            .await
            .expect("owner acquires resume fixture");

        let destination = node_identity();
        register_live_node(&store, &destination).await;
        backdate_heartbeat(&store.db, &destination).await;
        assert!(matches!(
            store
                .steal_stale(
                    &stale_entity,
                    stale_epoch,
                    StalePredicate::OwnerStale,
                    &destination,
                )
                .await,
            Err(ClaimError::Conflict)
        ));
        let jid: jid::BareJid = "alice@example.com".parse().expect("valid jid");
        let proof = waddle_xmpp::ownership::verify_resume_identity(&jid, &jid)
            .expect("matching resume identity");
        assert!(matches!(
            store
                .steal_for_resume(&resume_entity, resume_epoch, proof, &destination)
                .await,
            Err(ClaimError::Conflict)
        ));

        // Recovery has the one deliberate draining-destination exception,
        // but it is still deadline-fenced inside the same reclaim CAS.
        let predecessor = NodeIdentity::new("lapsed-recovery-node", "old");
        register_live_node(&store, &predecessor).await;
        let recovery_entity = sm_entity("lapsed-recovery-destination");
        let recovery_epoch = store
            .acquire(&recovery_entity, &predecessor)
            .await
            .expect("predecessor acquires recovery fixture");
        let recovery = NodeIdentity::new(predecessor.node_id.clone(), "candidate");
        store
            .register_draining_with_peer_id(&predecessor, &recovery, None, None, NODE_LEASE_TTL)
            .await
            .expect("register draining recovery candidate");
        backdate_heartbeat(&store.db, &recovery).await;
        assert!(matches!(
            store
                .reclaim_after_self_fence(
                    &recovery_entity,
                    recovery_epoch,
                    &predecessor,
                    &recovery,
                    NODE_LEASE_TTL,
                )
                .await,
            Err(ClaimError::Conflict)
        ));
    }

    #[tokio::test]
    async fn lapsed_steal_intent_destination_cannot_consume_the_intent() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let owner = node_identity();
        register_live_node(&store, &owner).await;
        let entity = user_actor_entity("lapsed-intent-destination");
        let epoch = store
            .acquire(&entity, &owner)
            .await
            .expect("owner acquires");
        let destination = node_identity();
        register_live_node(&store, &destination).await;
        store
            .report_steal_intent(&entity, &owner, epoch, &destination)
            .await
            .expect("report intent while destination is fresh");
        let conn = store.db.guard().await.expect("guard");
        conn.execute(
            "UPDATE clustering_steal_intents SET created_at = now() - interval '1 hour' WHERE entity = ?",
            crate::db_params![entity_key(&entity)],
        )
        .await
        .expect("age intent deterministically");
        drop(conn);
        backdate_heartbeat(&store.db, &destination).await;

        let intent_ttl = Duration::from_secs(1);
        assert!(matches!(
            store
                .steal_stale(
                    &entity,
                    epoch,
                    StalePredicate::StealIntentExpired { intent_ttl },
                    &destination,
                )
                .await,
            Err(ClaimError::Conflict)
        ));
        assert_eq!(
            store
                .owner_steal_intents(&owner)
                .await
                .expect("intent remains visible to owner"),
            vec![(entity.clone(), epoch)]
        );

        let fresh_destination = node_identity();
        register_live_node(&store, &fresh_destination).await;
        store
            .steal_stale(
                &entity,
                epoch,
                StalePredicate::StealIntentExpired { intent_ttl },
                &fresh_destination,
            )
            .await
            .expect("fresh destination consumes the surviving intent");
    }

    #[tokio::test]
    async fn acquire_serializes_with_same_node_id_epoch_rotation() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let old = node_identity();
        register_live_node(&store, &old).await;
        let fresh = NodeIdentity::new(old.node_id.clone(), uuid::Uuid::new_v4().to_string());
        let entity = sm_entity("acquire-vs-node-epoch-rotation");

        let mut rotation = store.db.begin().await.expect("begin node epoch rotation");
        assert_eq!(
            rotation
                .execute(
                    "UPDATE clustering_nodes SET node_epoch = ? WHERE node_id = ? AND node_epoch = ?",
                    crate::db_params![
                        fresh.node_epoch.clone(),
                        old.node_id.clone(),
                        old.node_epoch.clone(),
                    ],
                )
                .await
                .expect("rotate node epoch while holding row lock"),
            1
        );

        let store_db = store.db.clone();
        let old_claimant = old.clone();
        let claim_entity = entity.clone();
        let acquire = tokio::spawn(async move {
            PostgresClaimStore::new(store_db)
                .acquire(&claim_entity, &old_claimant)
                .await
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !acquire.is_finished(),
            "claim acquire must wait on the exact node-row share lock"
        );

        rotation.commit().await.expect("commit node epoch rotation");
        assert!(matches!(
            acquire.await.expect("acquire task"),
            Err(ClaimError::Draining)
        ));
        assert!(
            store
                .current_claim(&entity)
                .await
                .expect("read claim")
                .is_none(),
            "the superseded epoch must not publish a claim after rotation commits"
        );
    }

    #[tokio::test]
    async fn owner_stale_steal_serializes_with_same_node_id_epoch_rotation() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let dead_owner = node_identity();
        register_live_node(&store, &dead_owner).await;
        let entity = sm_entity("steal-vs-node-epoch-rotation");
        let observed = store
            .acquire(&entity, &dead_owner)
            .await
            .expect("dead owner acquires claim");
        store
            .db
            .guard()
            .await
            .expect("guard")
            .execute(
                "UPDATE clustering_nodes SET expired = true WHERE node_id = ? AND node_epoch = ?",
                crate::db_params![dead_owner.node_id.clone(), dead_owner.node_epoch.clone()],
            )
            .await
            .expect("expire old owner");

        let old_stealer = node_identity();
        register_live_node(&store, &old_stealer).await;
        let fresh_stealer = NodeIdentity::new(
            old_stealer.node_id.clone(),
            uuid::Uuid::new_v4().to_string(),
        );
        let mut rotation = store.db.begin().await.expect("begin node epoch rotation");
        assert_eq!(
            rotation
                .execute(
                    "UPDATE clustering_nodes SET node_epoch = ? WHERE node_id = ? AND node_epoch = ?",
                    crate::db_params![
                        fresh_stealer.node_epoch.clone(),
                        old_stealer.node_id.clone(),
                        old_stealer.node_epoch.clone(),
                    ],
                )
                .await
                .expect("rotate stealer epoch while holding row lock"),
            1
        );

        let store_db = store.db.clone();
        let stale_stealer = old_stealer.clone();
        let steal_entity = entity.clone();
        let steal = tokio::spawn(async move {
            PostgresClaimStore::new(store_db)
                .steal_stale(
                    &steal_entity,
                    observed,
                    StalePredicate::OwnerStale,
                    &stale_stealer,
                )
                .await
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !steal.is_finished(),
            "claim steal must wait on the exact destination node-row share lock"
        );

        rotation
            .commit()
            .await
            .expect("commit stealer epoch rotation");
        assert!(matches!(
            steal.await.expect("steal task"),
            Err(ClaimError::Conflict)
        ));
        let current = store
            .current_claim(&entity)
            .await
            .expect("read claim")
            .expect("claim remains");
        assert_eq!(current.owner, dead_owner);
        assert_eq!(current.claim_epoch, observed);
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
        register_live_node(&store, &me).await;
        let entity = sm_entity("ensure-claimed-fresh");
        let epoch = store
            .ensure_claimed(&entity, &me)
            .await
            .expect("ensure_claimed acquires fresh");
        assert!(epoch.0 >= 0, "the global claim generation must not wrap");
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
        register_live_node(&store, &me).await;
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
        register_live_node(&store, &owner).await;
        let entity = sm_entity("ensure-claimed-foreign");
        store
            .ensure_claimed(&entity, &owner)
            .await
            .expect("owner's ensure_claimed acquires");

        let foreign = node_identity();
        register_live_node(&store, &foreign).await;
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
        register_live_node(&store, &me).await;
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
        register_live_node(&store, &me).await;
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
        register_live_node(&store, &owner).await;
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
        assert!(epoch1 > epoch0);
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
        register_live_node(&store, &owner).await;
        let entity = sm_entity("stream-1");
        let epoch0 = store.acquire(&entity, &owner).await.expect("acquire");

        store
            .db
            .guard()
            .await
            .expect("guard")
            .execute(
                "DELETE FROM clustering_nodes WHERE node_id = ? AND node_epoch = ?",
                crate::db_params![owner.node_id.clone(), owner.node_epoch.clone()],
            )
            .await
            .expect("remove vanished owner node row");

        let stealer = node_identity();
        seed_node(&store.db, &stealer, false).await;
        let epoch1 = store
            .steal_stale(&entity, epoch0, StalePredicate::OwnerStale, &stealer)
            .await
            .expect("steal from a node with no nodes-row succeeds");
        assert!(epoch1 > epoch0);
    }

    #[tokio::test]
    async fn steal_stale_with_stale_epoch_loses_the_race() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let owner = node_identity();
        register_live_node(&store, &owner).await;
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
        register_live_node(&store, &owner).await;
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
        register_live_node(&store, &owner).await;
        let entity = sm_entity("stream-1");
        let epoch0 = store.acquire(&entity, &owner).await.expect("acquire");
        seed_node(&store.db, &owner, false).await;

        let stealer = node_identity();
        register_live_node(&store, &stealer).await;
        let jid: jid::BareJid = "alice@example.com".parse().expect("valid jid");
        let proof =
            waddle_xmpp::ownership::verify_resume_identity(&jid, &jid).expect("identity match");
        let epoch1 = store
            .steal_for_resume(&entity, epoch0, proof, &stealer)
            .await
            .expect("consent CAS steals from a fresh owner");
        assert!(epoch1 > epoch0);
    }

    #[tokio::test]
    async fn steal_for_resume_with_stale_epoch_loses_the_race() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let owner = node_identity();
        register_live_node(&store, &owner).await;
        let entity = sm_entity("stream-1");
        store.acquire(&entity, &owner).await.expect("acquire");

        let stealer = node_identity();
        register_live_node(&store, &stealer).await;
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
        register_live_node(&store, &owner).await;
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

        let recovered_incarnation =
            NodeIdentity::new(owner.node_id.clone(), uuid::Uuid::new_v4().to_string());
        assert!(!store
            .fence(&entity, &recovered_incarnation, epoch0)
            .await
            .expect("fence wrong node epoch"));
    }

    #[tokio::test]
    async fn release_is_epoch_gated_and_idempotent() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let owner = node_identity();
        register_live_node(&store, &owner).await;
        let entity = sm_entity("stream-1");
        let epoch0 = store.acquire(&entity, &owner).await.expect("acquire");

        // Releasing under the wrong epoch is a silent no-op — the claim
        // must survive.
        let wrong_epoch = ClaimEpoch(epoch0.0 + 1);
        store
            .release(&entity, &owner, wrong_epoch)
            .await
            .expect("release under wrong epoch is a no-op");
        assert!(store.fence(&entity, &owner, epoch0).await.expect("fence"));

        store
            .release(&entity, &owner, epoch0)
            .await
            .expect("release under the right epoch");
        assert!(!store
            .fence(&entity, &owner, epoch0)
            .await
            .expect("fence after release"));

        // Releasing again (already gone) is still not an error.
        store
            .release(&entity, &owner, epoch0)
            .await
            .expect("re-release is a no-op, not an error");

        let epoch1 = store
            .acquire(&entity, &owner)
            .await
            .expect("reacquire after release");
        assert!(
            epoch1 > epoch0,
            "the global allocator must not reuse a deleted row's generation"
        );
        store
            .release(&entity, &owner, epoch0)
            .await
            .expect("delayed old release remains a no-op");
        assert!(store
            .fence(&entity, &owner, epoch1)
            .await
            .expect("new grant survives old release"));
    }

    #[tokio::test]
    async fn release_many_clears_every_owned_entity_in_one_round_trip() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let owner = node_identity();
        register_live_node(&store, &owner).await;
        let a = sm_entity("stream-a");
        let b = sm_entity("stream-b");
        let c = sm_entity("stream-c");
        let a_epoch = store.acquire(&a, &owner).await.expect("acquire a");
        let b_epoch = store.acquire(&b, &owner).await.expect("acquire b");
        // c is owned by someone else — release_many must not touch it.
        let other = node_identity();
        register_live_node(&store, &other).await;
        let c_epoch = store.acquire(&c, &other).await.expect("acquire c");

        store
            .release_many(&[
                ClaimGrant::new(a.clone(), owner.clone(), a_epoch),
                ClaimGrant::new(b.clone(), owner.clone(), b_epoch),
                // Deliberately carry the wrong owner for c: exact matching
                // must leave the other node's claim untouched.
                ClaimGrant::new(c.clone(), owner.clone(), c_epoch),
            ])
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
            .release_many(&[])
            .await
            .expect("empty release_many does not error");
    }

    #[tokio::test]
    async fn stale_epoch_cleanup_cannot_delete_claims_after_node_incarnation_rotation() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let old = NodeIdentity::new("stable-cleanup-node", uuid::Uuid::new_v4().to_string());
        register_live_node(&store, &old).await;
        let single = sm_entity("stale-single-release");
        let batched = sm_entity("stale-batched-release");
        let single_epoch = store.acquire(&single, &old).await.expect("single acquire");
        let batched_epoch = store.acquire(&batched, &old).await.expect("batch acquire");

        let fresh = NodeIdentity::new(old.node_id.clone(), uuid::Uuid::new_v4().to_string());
        store
            .register_draining_with_peer_id(&old, &fresh, None, None, Duration::from_secs(30))
            .await
            .expect("rotate exact node incarnation");

        store
            .release(&single, &old, single_epoch)
            .await
            .expect("stale single cleanup is a no-op");
        store
            .release_many(&[ClaimGrant::new(batched.clone(), old.clone(), batched_epoch)])
            .await
            .expect("stale batch cleanup is a no-op");

        assert_eq!(
            store
                .current_claim(&single)
                .await
                .expect("single claim read")
                .expect("single claim remains")
                .claim_epoch,
            single_epoch
        );
        assert_eq!(
            store
                .current_claim(&batched)
                .await
                .expect("batch claim read")
                .expect("batch claim remains")
                .claim_epoch,
            batched_epoch
        );
    }

    // Exact-grant regression: a batch captured before a same-node regrant
    // must not delete the newer claim when it finally executes.
    #[tokio::test]
    async fn delayed_release_many_is_a_no_op_after_same_node_regrant() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let me = node_identity();
        register_live_node(&store, &me).await;
        let entity = sm_entity("stream-aba");
        let epoch0 = store.acquire(&entity, &me).await.expect("initial acquire");

        // Drain decides to release this entity (its final write already
        // committed) and queues it for the batch...
        let batch = vec![ClaimGrant::new(entity.clone(), me.clone(), epoch0)];

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
        assert_ne!(epoch1, epoch0, "every grant receives a new global epoch");
        assert!(
            store
                .fence(&entity, &me, epoch1)
                .await
                .expect("fence check"),
            "the entity is genuinely, freshly re-claimed under epoch 1"
        );

        store.release_many(&batch).await.expect("release_many");

        assert!(
            store
                .fence(&entity, &me, epoch1)
                .await
                .expect("fence check"),
            "the delayed old batch must not delete the newer exact grant"
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
        register_live_node(&store, &owner).await;
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
        assert!(stolen_epoch > epoch0);

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
    async fn register_is_idempotent_only_for_the_exact_incarnation() {
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
        store
            .register(&first, None)
            .await
            .expect("exact registration retry is idempotent");

        // A caller with no expected-old proof cannot overwrite the row with
        // a fresh epoch. Recovery and startup replacement use their explicit
        // expected-old CAS methods instead.
        let second = NodeIdentity::new(node_id, uuid::Uuid::new_v4().to_string());
        let delayed = store
            .register(&second, None)
            .await
            .expect_err("bootstrap registration cannot replace an incarnation");
        assert!(matches!(delayed, ClaimError::Conflict));

        assert!(store
            .heartbeat(&first, NODE_LEASE_TTL)
            .await
            .expect("heartbeat call succeeds"));
        assert!(!store
            .heartbeat(&second, NODE_LEASE_TTL)
            .await
            .expect("heartbeat call succeeds"));
    }

    #[tokio::test]
    async fn initial_registration_replacement_is_an_exact_expected_old_cas() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let stable_id = uuid::Uuid::new_v4().to_string();
        let old = NodeIdentity::new(stable_id.clone(), uuid::Uuid::new_v4().to_string());
        store.register(&old, None).await.expect("register old");
        backdate_heartbeat(&store.db, &old).await;
        assert!(store
            .expire(&old, NODE_LEASE_TTL)
            .await
            .expect("commit old incarnation expired"));

        let fresh = NodeIdentity::new(stable_id.clone(), uuid::Uuid::new_v4().to_string());
        store
            .register_initial_with_peer_id(Some(&old), &fresh, None, None)
            .await
            .expect("exact expired predecessor may be replaced");
        assert_eq!(
            store
                .registered_identity(&stable_id)
                .await
                .expect("read current identity"),
            Some(fresh.clone())
        );

        let delayed = NodeIdentity::new(stable_id.clone(), uuid::Uuid::new_v4().to_string());
        let stale_replacement = store
            .register_initial_with_peer_id(Some(&old), &delayed, None, None)
            .await
            .expect_err("the old predecessor proof cannot replace a later incarnation");
        assert!(matches!(stale_replacement, ClaimError::Conflict));
        let stale_bootstrap = store
            .register(&old, None)
            .await
            .expect_err("a delayed bootstrap cannot overwrite the later incarnation");
        assert!(matches!(stale_bootstrap, ClaimError::Conflict));
        assert_eq!(
            store
                .registered_identity(&stable_id)
                .await
                .expect("read identity after delayed writes"),
            Some(fresh),
            "neither stale registration may overwrite the current epoch"
        );
    }

    #[tokio::test]
    async fn expire_commits_the_flag_and_is_idempotent_once_true() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(clean) = clean_store().await else {
            return;
        };
        let owner = node_identity();
        let short_ttl = Duration::from_millis(200);
        let store = PostgresClaimStore::with_lease_ttl(clean.db.clone(), short_ttl);
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

    #[tokio::test]
    async fn post_fence_registration_stays_draining_until_exact_epoch_activation() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let old = NodeIdentity::new("stable-relay-node", "old-epoch");
        store
            .register(&old, None)
            .await
            .expect("register old epoch");
        let fresh = NodeIdentity::new(old.node_id.clone(), "fresh-epoch");

        store
            .register_draining_with_peer_id(
                &old,
                &fresh,
                Some("test-generation".to_string()),
                Some("test-peer".to_string()),
                NODE_LEASE_TTL,
            )
            .await
            .expect("register recovery candidate");

        let conn = store.db.guard().await.expect("guard");
        let mut rows = conn
            .query(
                "SELECT node_epoch, draining FROM clustering_nodes WHERE node_id = ?",
                crate::db_params![fresh.node_id.clone()],
            )
            .await
            .expect("query candidate row");
        let row = rows
            .next()
            .await
            .expect("row")
            .expect("candidate row present");
        assert_eq!(row.get::<String>(0).expect("node_epoch"), fresh.node_epoch);
        assert!(row.get::<bool>(1).expect("draining"));
        drop(rows);
        drop(conn);

        assert!(
            !store
                .activate(&old, NODE_LEASE_TTL)
                .await
                .expect("stale activation CAS"),
            "the superseded epoch must not activate the recovery row"
        );
        assert!(
            store
                .activate(&fresh, NODE_LEASE_TTL)
                .await
                .expect("fresh activation CAS"),
            "the exact recovery epoch must activate once"
        );
        store
            .register_draining_with_peer_id(
                &old,
                &fresh,
                Some("test-generation".to_string()),
                Some("test-peer".to_string()),
                NODE_LEASE_TTL,
            )
            .await
            .expect("delayed exact recovery registration is an idempotent retry");
        assert!(
            store
                .is_fresh(&fresh, NODE_LEASE_TTL)
                .await
                .expect("freshness check"),
            "a delayed exact registration retry must not put an activated node back into draining"
        );
    }

    #[tokio::test]
    async fn elapsed_recovery_candidate_cannot_be_revived_and_can_rotate_forward() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let ttl = Duration::from_millis(100);
        let old = NodeIdentity::new("stable-expired-recovery", "old");
        store.register(&old, None).await.expect("register old");
        let elapsed = NodeIdentity::new(old.node_id.clone(), "elapsed");
        store
            .register_draining_with_peer_id(&old, &elapsed, None, None, ttl)
            .await
            .expect("register draining candidate");
        store
            .db
            .guard()
            .await
            .expect("guard")
            .execute(
                "UPDATE clustering_nodes SET heartbeat = now() - interval '1 second' WHERE node_id = ?",
                crate::db_params![old.node_id.clone()],
            )
            .await
            .expect("backdate candidate");

        assert!(
            !store
                .activate(&elapsed, ttl)
                .await
                .expect("elapsed activation CAS"),
            "activation must not refresh an epoch whose lease deadline elapsed"
        );
        assert!(matches!(
            store
                .register_draining_with_peer_id(&old, &elapsed, None, None, ttl)
                .await,
            Err(ClaimError::Conflict)
        ));

        store
            .db
            .guard()
            .await
            .expect("guard")
            .execute(
                "UPDATE clustering_nodes SET expired = true WHERE node_id = ? AND node_epoch = ?",
                crate::db_params![elapsed.node_id.clone(), elapsed.node_epoch.clone()],
            )
            .await
            .expect("expire candidate");
        let replacement = NodeIdentity::new(old.node_id.clone(), "replacement");
        store
            .register_draining_with_peer_id(&elapsed, &replacement, None, None, ttl)
            .await
            .expect("an exact expired candidate can be superseded by a new epoch");
        assert!(store
            .activate(&replacement, ttl)
            .await
            .expect("activate replacement"));
    }

    fn room_entity(id: &str) -> Entity {
        Entity::new(EntityType::RoomActor, id.to_string())
    }

    // --- ADR-0017 Phase 3 Slice 10: the acquire-side draining gate -------

    #[tokio::test]
    async fn acquire_and_self_reacquire_reject_a_superseded_node_epoch() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let old = NodeIdentity::new("stable-claim-node", "old-epoch");
        register_live_node(&store, &old).await;
        let owned = room_entity("old-epoch-owned@muc.example.com");
        let owned_epoch = store
            .ensure_claimed(&owned, &old)
            .await
            .expect("old epoch initially owns claim");

        let fresh = NodeIdentity::new(old.node_id.clone(), "fresh-epoch");
        store
            .register_draining_with_peer_id(&old, &fresh, None, None, NODE_LEASE_TTL)
            .await
            .expect("rotate stable node to fresh epoch");
        assert!(store
            .activate(&fresh, NODE_LEASE_TTL)
            .await
            .expect("activate fresh epoch"));

        let new_entity = room_entity("old-epoch-new@muc.example.com");
        assert!(matches!(
            store.acquire(&new_entity, &old).await,
            Err(ClaimError::Draining)
        ));
        assert!(
            store.ensure_claimed(&owned, &old).await.is_err(),
            "the idempotent path must not resurrect a claim whose exact owner incarnation is gone"
        );
        let jid: jid::BareJid = "old-epoch@example.com".parse().expect("valid jid");
        let proof =
            waddle_xmpp::ownership::verify_resume_identity(&jid, &jid).expect("identity match");
        assert!(matches!(
            store
                .steal_for_resume(&owned, owned_epoch, proof, &old)
                .await,
            Err(ClaimError::Conflict)
        ));
    }

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
        register_live_node(&store, &other).await;
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
        register_live_node(&store, &dead_owner).await;
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
        register_live_node(&store, &dead_owner).await;
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
        assert!(epoch1 > epoch0);
    }

    #[tokio::test]
    async fn steal_orphaned_sm_session_claim_requires_heartbeat_fresh_stealer_in_cas() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let dead_owner = node_identity();
        register_live_node(&store, &dead_owner).await;
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
        assert!(epoch1 > epoch0);
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
        // A proven replacement under the SAME node_id must NOT refresh
        // `first_seen` — the registration upsert deliberately omits
        // `first_seen` from its SET list.
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
        drop(rows);
        drop(conn);

        backdate_heartbeat(&store.db, &node).await;
        assert!(store
            .expire(&node, NODE_LEASE_TTL)
            .await
            .expect("commit predecessor expiry"));

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let node_reregistered = NodeIdentity::new("stable-node-id", "epoch-1");
        store
            .register_initial_with_peer_id(
                Some(&node),
                &node_reregistered,
                Some("gen-b".to_string()),
                None,
            )
            .await
            .expect("replace the exact expired incarnation under the same node_id");

        let conn = store.db.guard().await.expect("guard");
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
        store
            .register(&other, None)
            .await
            .expect("register resume destination");

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
            .report_steal_intent(&entity, &reporter, ClaimEpoch(0), &reporter)
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
        register_live_node(&store, &owner).await;
        let entity = user_actor_entity("room-1");
        let epoch0 = store.acquire(&entity, &owner).await.expect("acquire");

        let stealer = node_identity();
        store
            .register(&stealer, None)
            .await
            .expect("register live stealer");
        store
            .report_steal_intent(&entity, &owner, epoch0, &stealer)
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
        register_live_node(&store, &owner).await;
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
            .report_steal_intent(&entity, &owner, epoch0, &stealer)
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
        assert!(epoch1 > epoch0);
    }

    #[tokio::test]
    async fn steal_intent_expired_requires_live_stealer_without_burning_intents() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let owner = node_identity();
        register_live_node(&store, &owner).await;
        let entity = user_actor_entity("room-live-intent-stealer");
        let epoch0 = store.acquire(&entity, &owner).await.expect("acquire");
        seed_node(&store.db, &owner, false).await;

        let reporter = node_identity();
        register_live_node(&store, &reporter).await;
        store
            .report_steal_intent(&entity, &owner, epoch0, &reporter)
            .await
            .expect("report intent");
        let missing_stealer = node_identity();
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
        assert!(epoch1 > epoch0);
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
        register_live_node(&store, &old_owner).await;
        let entity = user_actor_entity("room-3");
        let epoch0 = store.acquire(&entity, &old_owner).await.expect("acquire");

        let reporter = node_identity();
        register_live_node(&store, &reporter).await;
        store
            .report_steal_intent(&entity, &old_owner, epoch0, &reporter)
            .await
            .expect("report intent");

        // Simulate the claim moving to a new owner via the consent CAS —
        // any CAS variant works here; reconciliation and clear_steal_intent
        // only care that Postgres no longer attributes the claim to
        // old_owner/epoch0.
        let new_owner = node_identity();
        register_live_node(&store, &new_owner).await;
        let jid: jid::BareJid = "alice@example.com".parse().expect("valid jid");
        let proof =
            waddle_xmpp::ownership::verify_resume_identity(&jid, &jid).expect("identity match");
        let epoch1 = store
            .steal_for_resume(&entity, epoch0, proof, &new_owner)
            .await
            .expect("steal succeeds");

        // Every successful grant clears intents bound to the displaced
        // generation. Report a fresh intent against the new exact grant so
        // the stale owner's delayed veto cannot erase it.
        store
            .report_steal_intent(&entity, &new_owner, epoch1, &reporter)
            .await
            .expect("report intent against new grant");

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
            "the new grant's exact intent must survive a deposed owner's stale-epoch clear attempt"
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
    async fn report_steal_intent_rejects_a_delayed_target_grant_and_stale_reporter_incarnation() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let old_owner = node_identity();
        register_live_node(&store, &old_owner).await;
        let entity = user_actor_entity("intent-exact-grant");
        let old_epoch = store
            .acquire(&entity, &old_owner)
            .await
            .expect("old owner acquires");
        let reporter = node_identity();
        register_live_node(&store, &reporter).await;

        let new_owner = node_identity();
        register_live_node(&store, &new_owner).await;
        let jid: jid::BareJid = "alice@example.com".parse().expect("valid jid");
        let proof =
            waddle_xmpp::ownership::verify_resume_identity(&jid, &jid).expect("identity match");
        let new_epoch = store
            .steal_for_resume(&entity, old_epoch, proof, &new_owner)
            .await
            .expect("move claim to a new exact grant");

        let delayed_old_target = store
            .report_steal_intent(&entity, &old_owner, old_epoch, &reporter)
            .await
            .expect_err("a delayed report must not bind itself to a replacement grant");
        assert!(matches!(delayed_old_target, ClaimError::Conflict));

        let recovered_reporter =
            NodeIdentity::new(reporter.node_id.clone(), uuid::Uuid::new_v4().to_string());
        store
            .register_draining_with_peer_id(
                &reporter,
                &recovered_reporter,
                None,
                None,
                NODE_LEASE_TTL,
            )
            .await
            .expect("rotate reporter node row");
        let delayed_old_reporter = store
            .report_steal_intent(&entity, &new_owner, new_epoch, &reporter)
            .await
            .expect_err("a deposed reporter incarnation must not create an intent");
        assert!(matches!(delayed_old_reporter, ClaimError::Conflict));
        assert!(
            store
                .owner_steal_intents(&new_owner)
                .await
                .expect("scan after rejected reports")
                .is_empty(),
            "neither stale input may leave an intent attached to the current grant"
        );

        assert!(store
            .activate(&recovered_reporter, NODE_LEASE_TTL)
            .await
            .expect("activate recovered reporter"));
        store
            .report_steal_intent(&entity, &new_owner, new_epoch, &recovered_reporter)
            .await
            .expect("the exact live reporter and target grant may report");
        assert_eq!(
            store
                .owner_steal_intents(&new_owner)
                .await
                .expect("scan current exact intent"),
            vec![(entity, new_epoch)]
        );
    }

    #[tokio::test]
    async fn clear_steal_intent_rejects_an_old_epoch_of_the_same_node_id() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let old_owner = node_identity();
        store
            .register(&old_owner, None)
            .await
            .expect("register old owner");
        let entity = user_actor_entity("same-node-new-epoch");
        let epoch = store.acquire(&entity, &old_owner).await.expect("acquire");
        let reporter = node_identity();
        register_live_node(&store, &reporter).await;
        store
            .report_steal_intent(&entity, &old_owner, epoch, &reporter)
            .await
            .expect("report intent");

        let recovered_owner =
            NodeIdentity::new(old_owner.node_id.clone(), uuid::Uuid::new_v4().to_string());
        store
            .register_draining_with_peer_id(
                &old_owner,
                &recovered_owner,
                None,
                None,
                NODE_LEASE_TTL,
            )
            .await
            .expect("rotate node row without moving the claim");

        let stale_clear = store
            .clear_steal_intent(&entity, &old_owner, epoch)
            .await
            .expect("stale clear is a no-op");
        assert_eq!(stale_clear, 0);
        let current_clear = store
            .clear_steal_intent(&entity, &recovered_owner, epoch)
            .await
            .expect("recovered incarnation does not own the old claim");
        assert_eq!(
            current_clear, 0,
            "cleanup requires both the exact live node incarnation and exact claim tuple"
        );
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
        register_live_node(&store, &owner).await;
        let entity = user_actor_entity("room-stale-burn");
        let epoch0 = store.acquire(&entity, &owner).await.expect("acquire");
        seed_node(&store.db, &owner, false).await;

        // Bump the claim epoch so epoch0 is stale.
        let jid: jid::BareJid = "alice@example.com".parse().expect("valid jid");
        let proof =
            waddle_xmpp::ownership::verify_resume_identity(&jid, &jid).expect("identity match");
        let current_owner = node_identity();
        register_live_node(&store, &current_owner).await;
        let epoch1 = store
            .steal_for_resume(&entity, epoch0, proof, &current_owner)
            .await
            .expect("epoch bump");

        let reporter = node_identity();
        register_live_node(&store, &reporter).await;
        store
            .report_steal_intent(&entity, &current_owner, epoch1, &reporter)
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
        assert!(epoch2 > epoch1);
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
        register_live_node(&store, &owner).await;
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
        register_live_node(&store, &reporter).await;
        store
            .report_steal_intent(&with_intent, &owner, epoch_with_intent, &reporter)
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
        register_live_node(&store, &owner).await;
        let entity = user_actor_entity("room-4");
        let epoch0 = store.acquire(&entity, &owner).await.expect("acquire");
        let reporter = node_identity();
        register_live_node(&store, &reporter).await;

        store
            .report_steal_intent(&entity, &owner, epoch0, &reporter)
            .await
            .expect("first report");
        let intent_ttl = std::time::Duration::from_millis(150);
        tokio::time::sleep(intent_ttl / 2).await;
        // Refresh before the first report ages out.
        store
            .report_steal_intent(&entity, &owner, epoch0, &reporter)
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

    #[tokio::test]
    async fn report_steal_intent_starts_its_full_age_after_intent_lock_wait() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let owner = node_identity();
        let reporter = node_identity();
        register_live_node(&store, &owner).await;
        register_live_node(&store, &reporter).await;
        let entity = user_actor_entity("intent-post-lock-clock");
        let epoch = store.acquire(&entity, &owner).await.unwrap();
        store
            .report_steal_intent(&entity, &owner, epoch, &reporter)
            .await
            .unwrap();

        let mut blocker = store.db.begin().await.unwrap();
        let mut rows = blocker
            .query(
                "SELECT 1 FROM clustering_steal_intents WHERE entity = ? AND reporter_node = ? FOR UPDATE",
                crate::db_params![entity_key(&entity), reporter.node_id.clone()],
            )
            .await
            .unwrap();
        assert!(rows.next().await.unwrap().is_some());
        drop(rows);

        let reporter_store = PostgresClaimStore::new(store.db.clone());
        let task_entity = entity.clone();
        let task_owner = owner.clone();
        let task_reporter = reporter.clone();
        let refresh = tokio::spawn(async move {
            reporter_store
                .report_steal_intent(&task_entity, &task_owner, epoch, &task_reporter)
                .await
        });
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(
            !refresh.is_finished(),
            "refresh must wait on the intent row"
        );
        tokio::time::sleep(Duration::from_millis(360)).await;
        blocker.commit().await.unwrap();
        refresh.await.unwrap().unwrap();

        let mut rows = store
            .db
            .guard()
            .await
            .unwrap()
            .query(
                "SELECT (EXTRACT(EPOCH FROM clock_timestamp() - created_at) * 1000)::double precision FROM clustering_steal_intents WHERE entity = ? AND reporter_node = ?",
                crate::db_params![entity_key(&entity), reporter.node_id],
            )
            .await
            .unwrap();
        let age_ms = rows.next().await.unwrap().unwrap().get::<f64>(0).unwrap();
        assert!(
            age_ms < 200.0,
            "the refreshed intent age must start after the 400ms lock wait, got {age_ms}ms"
        );
    }

    #[tokio::test]
    async fn report_steal_intent_rechecks_leases_after_intent_lock_wait() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let owner = node_identity();
        let reporter = node_identity();
        register_live_node(&store, &owner).await;
        register_live_node(&store, &reporter).await;
        let entity = user_actor_entity("intent-post-lock-lease");
        let epoch = store.acquire(&entity, &owner).await.unwrap();
        store
            .report_steal_intent(&entity, &owner, epoch, &reporter)
            .await
            .unwrap();
        store
            .db
            .guard()
            .await
            .unwrap()
            .execute(
                "UPDATE clustering_steal_intents SET created_at = clock_timestamp() - interval '1 hour' WHERE entity = ? AND reporter_node = ?",
                crate::db_params![entity_key(&entity), reporter.node_id.clone()],
            )
            .await
            .unwrap();
        store
            .db
            .guard()
            .await
            .unwrap()
            .execute(
                "UPDATE clustering_nodes SET heartbeat = clock_timestamp(), lease_ttl_ms = 100 WHERE (node_id = ? AND node_epoch = ?) OR (node_id = ? AND node_epoch = ?)",
                crate::db_params![
                    owner.node_id.clone(),
                    owner.node_epoch.clone(),
                    reporter.node_id.clone(),
                    reporter.node_epoch.clone(),
                ],
            )
            .await
            .unwrap();

        let mut blocker = store.db.begin().await.unwrap();
        let mut rows = blocker
            .query(
                "SELECT 1 FROM clustering_steal_intents WHERE entity = ? AND reporter_node = ? FOR UPDATE",
                crate::db_params![entity_key(&entity), reporter.node_id.clone()],
            )
            .await
            .unwrap();
        assert!(rows.next().await.unwrap().is_some());
        drop(rows);

        let reporter_store = PostgresClaimStore::new(store.db.clone());
        let task_entity = entity.clone();
        let task_owner = owner.clone();
        let task_reporter = reporter.clone();
        let refresh = tokio::spawn(async move {
            reporter_store
                .report_steal_intent(&task_entity, &task_owner, epoch, &task_reporter)
                .await
        });
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(
            !refresh.is_finished(),
            "refresh must wait on the intent row"
        );
        tokio::time::sleep(Duration::from_millis(360)).await;
        blocker.commit().await.unwrap();
        assert!(matches!(refresh.await.unwrap(), Err(ClaimError::Conflict)));

        let mut rows = store
            .db
            .guard()
            .await
            .unwrap()
            .query(
                "SELECT created_at < clock_timestamp() - interval '30 minutes' FROM clustering_steal_intents WHERE entity = ? AND reporter_node = ?",
                crate::db_params![entity_key(&entity), reporter.node_id],
            )
            .await
            .unwrap();
        assert!(
            rows.next().await.unwrap().unwrap().get::<bool>(0).unwrap(),
            "a report rejected after the wait must not refresh the intent age"
        );
    }

    #[tokio::test]
    async fn steal_intent_age_is_evaluated_after_intent_row_lock_wait() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let owner = node_identity();
        let stealer = node_identity();
        register_live_node(&store, &owner).await;
        register_live_node(&store, &stealer).await;
        let entity = user_actor_entity("steal-intent-post-lock-age");
        let epoch = store.acquire(&entity, &owner).await.unwrap();
        store
            .report_steal_intent(&entity, &owner, epoch, &stealer)
            .await
            .unwrap();

        let mut blocker = store.db.begin().await.unwrap();
        let mut rows = blocker
            .query(
                "SELECT 1 FROM clustering_steal_intents WHERE entity = ? FOR UPDATE",
                crate::db_params![entity_key(&entity)],
            )
            .await
            .unwrap();
        assert!(rows.next().await.unwrap().is_some());
        drop(rows);

        let task_store = PostgresClaimStore::new(store.db.clone());
        let task_entity = entity.clone();
        let task_stealer = stealer.clone();
        let steal = tokio::spawn(async move {
            task_store
                .steal_stale(
                    &task_entity,
                    epoch,
                    StalePredicate::StealIntentExpired {
                        intent_ttl: Duration::from_millis(100),
                    },
                    &task_stealer,
                )
                .await
        });
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(!steal.is_finished(), "steal must lock before deciding age");
        tokio::time::sleep(Duration::from_millis(360)).await;
        blocker.commit().await.unwrap();
        assert!(steal.await.unwrap().unwrap() > epoch);
    }

    #[tokio::test]
    async fn clear_steal_intent_rechecks_owner_lease_after_node_lock_wait() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let owner = node_identity();
        let reporter = node_identity();
        register_live_node(&store, &owner).await;
        register_live_node(&store, &reporter).await;
        let entity = user_actor_entity("clear-intent-post-lock-lease");
        let epoch = store.acquire(&entity, &owner).await.unwrap();
        store
            .report_steal_intent(&entity, &owner, epoch, &reporter)
            .await
            .unwrap();
        store
            .db
            .guard()
            .await
            .unwrap()
            .execute(
                "UPDATE clustering_nodes SET heartbeat = clock_timestamp(), lease_ttl_ms = 100 WHERE node_id = ? AND node_epoch = ?",
                crate::db_params![owner.node_id.clone(), owner.node_epoch.clone()],
            )
            .await
            .unwrap();

        let mut blocker = store.db.begin().await.unwrap();
        let mut rows = blocker
            .query(
                "SELECT 1 FROM clustering_nodes WHERE node_id = ? AND node_epoch = ? FOR UPDATE",
                crate::db_params![owner.node_id.clone(), owner.node_epoch.clone()],
            )
            .await
            .unwrap();
        assert!(rows.next().await.unwrap().is_some());
        drop(rows);

        let task_store = PostgresClaimStore::new(store.db.clone());
        let task_entity = entity.clone();
        let task_owner = owner.clone();
        let clear = tokio::spawn(async move {
            task_store
                .clear_steal_intent(&task_entity, &task_owner, epoch)
                .await
        });
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(
            !clear.is_finished(),
            "clear must wait on the owner node row"
        );
        tokio::time::sleep(Duration::from_millis(360)).await;
        blocker.commit().await.unwrap();
        assert_eq!(clear.await.unwrap().unwrap(), 0);

        let mut rows = store
            .db
            .guard()
            .await
            .unwrap()
            .query(
                "SELECT COUNT(*) FROM clustering_steal_intents WHERE entity = ?",
                crate::db_params![entity_key(&entity)],
            )
            .await
            .unwrap();
        assert_eq!(
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            1,
            "a stale owner cannot clear the reporter's intent after the wait"
        );
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
        let Some(clean) = clean_store().await else {
            return;
        };
        // Keep liveness out of this deliberately long Postgres stress loop.
        // The invariant under test is the intent-veto/steal row-lock race;
        // allowing the default node lease to lapse partway through 200 CI
        // rounds turns `report_steal_intent` into an unrelated liveness test.
        let store = PostgresClaimStore::with_lease_ttl(
            clean.db.clone(),
            std::time::Duration::from_secs(5 * 60),
        );
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
            store
                .report_steal_intent(&entity, &owner, epoch, &stealer)
                .await
                .expect("seed an exact current-grant intent");
            {
                let conn = store.db.guard().await.expect("guard");
                conn.execute(
                    r#"
                    UPDATE clustering_steal_intents
                    SET created_at = now() - (? || ' milliseconds')::interval
                    WHERE entity = ?
                      AND reporter_node = ?
                      AND reporter_epoch = ?
                      AND target_node = ?
                      AND target_node_epoch = ?
                      AND target_claim_epoch = ?
                    "#,
                    crate::db_params![
                        (intent_ttl.as_millis() * 10).to_string(),
                        entity_key(&entity),
                        stealer.node_id.clone(),
                        stealer.node_epoch.clone(),
                        owner.node_id.clone(),
                        owner.node_epoch.clone(),
                        epoch.0,
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
        seed_node(&store.db, &stale_owner, false).await;
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
        seed_node(&store.db, &stale_owner, true).await;

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
    async fn resume_grant_revalidates_destination_after_claim_lock_waits_past_ttl() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(clean) = clean_store().await else {
            return;
        };
        let ttl = Duration::from_millis(80);
        let store = PostgresClaimStore::with_lease_ttl(clean.db.clone(), ttl);
        let owner = node_identity();
        let destination = node_identity();
        register_live_node(&store, &owner).await;
        register_live_node(&store, &destination).await;
        let entity = sm_entity("post-lock-resume");
        let epoch = store.acquire(&entity, &owner).await.expect("acquire");
        let contender = PostgresClaimStore::with_lease_ttl(clean.db.clone(), ttl);
        let task_entity = entity.clone();
        let task_destination = destination.clone();
        let jid: jid::BareJid = "alice@example.com".parse().unwrap();
        let proof = waddle_xmpp::ownership::verify_resume_identity(&jid, &jid).unwrap();
        let result = while_claim_is_locked_past_ttl(&clean.db, &entity, ttl, async move {
            contender
                .steal_for_resume(&task_entity, epoch, proof, &task_destination)
                .await
        })
        .await;
        assert!(matches!(result, Err(ClaimError::Conflict)));
        assert_eq!(
            store.current_claim(&entity).await.unwrap().unwrap().owner,
            owner
        );
    }

    #[tokio::test]
    async fn acquire_revalidates_after_unique_conflict_waits_past_ttl() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(clean) = clean_store().await else {
            return;
        };
        let ttl = Duration::from_millis(80);
        let store = PostgresClaimStore::with_lease_ttl(clean.db.clone(), ttl);
        let destination = node_identity();
        register_live_node(&store, &destination).await;
        let entity = sm_entity("post-lock-acquire");
        let mut blocker = clean.db.begin().await.unwrap();
        blocker
            .execute(
                "INSERT INTO clustering_claims (entity, entity_type, node_id, node_epoch, claim_epoch) VALUES (?, ?, ?, ?, -1)",
                crate::db_params![
                    entity_key(&entity),
                    EntityType::SmSession.as_db_str().to_string(),
                    "temporary".to_string(),
                    "temporary".to_string(),
                ],
            )
            .await
            .unwrap();
        let contender = PostgresClaimStore::with_lease_ttl(clean.db.clone(), ttl);
        let task_entity = entity.clone();
        let task_destination = destination.clone();
        let task =
            tokio::spawn(async move { contender.acquire(&task_entity, &task_destination).await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            !task.is_finished(),
            "acquire must wait on the unique conflict"
        );
        tokio::time::sleep(ttl * 3).await;
        blocker.rollback().await.unwrap();
        assert!(matches!(task.await.unwrap(), Err(ClaimError::Draining)));
        assert!(store.current_claim(&entity).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn intent_recovery_and_orphan_grants_revalidate_after_claim_lock_wait() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(clean) = clean_store().await else {
            return;
        };
        let ttl = Duration::from_millis(80);
        let store = PostgresClaimStore::with_lease_ttl(clean.db.clone(), ttl);

        let intent_owner = node_identity();
        let reporter = node_identity();
        register_live_node(&store, &intent_owner).await;
        register_live_node(&store, &reporter).await;
        let intent_entity = room_entity("post-lock-intent@muc.example.com");
        let intent_epoch = store.acquire(&intent_entity, &intent_owner).await.unwrap();
        store
            .report_steal_intent(&intent_entity, &intent_owner, intent_epoch, &reporter)
            .await
            .unwrap();
        store
            .db
            .guard()
            .await
            .unwrap()
            .execute(
                "UPDATE clustering_steal_intents SET created_at = clock_timestamp() - interval '1 hour' WHERE entity = ?",
                crate::db_params![entity_key(&intent_entity)],
            )
            .await
            .unwrap();
        let intent_store = PostgresClaimStore::with_lease_ttl(clean.db.clone(), ttl);
        let intent_task_entity = intent_entity.clone();
        let intent_task_reporter = reporter.clone();
        let intent_result =
            while_claim_is_locked_past_ttl(&clean.db, &intent_entity, ttl, async move {
                intent_store
                    .steal_stale(
                        &intent_task_entity,
                        intent_epoch,
                        StalePredicate::StealIntentExpired {
                            intent_ttl: Duration::from_millis(1),
                        },
                        &intent_task_reporter,
                    )
                    .await
            })
            .await;
        assert!(matches!(intent_result, Err(ClaimError::Conflict)));
        let mut intent_rows = store
            .db
            .guard()
            .await
            .unwrap()
            .query(
                "SELECT 1 FROM clustering_steal_intents WHERE entity = ?",
                crate::db_params![entity_key(&intent_entity)],
            )
            .await
            .unwrap();
        assert!(intent_rows.next().await.unwrap().is_some());

        let predecessor = NodeIdentity::new("post-lock-recovery", "old");
        register_live_node(&store, &predecessor).await;
        let recovery_entity = sm_entity("post-lock-recovery-session");
        let recovery_epoch = store.acquire(&recovery_entity, &predecessor).await.unwrap();
        let candidate = NodeIdentity::new(predecessor.node_id.clone(), "candidate");
        store
            .register_draining_with_peer_id(&predecessor, &candidate, None, None, ttl)
            .await
            .unwrap();
        let recovery_store = PostgresClaimStore::with_lease_ttl(clean.db.clone(), ttl);
        let recovery_task_entity = recovery_entity.clone();
        let recovery_task_source = predecessor.clone();
        let recovery_task_candidate = candidate.clone();
        let recovery_result =
            while_claim_is_locked_past_ttl(&clean.db, &recovery_entity, ttl, async move {
                recovery_store
                    .reclaim_after_self_fence(
                        &recovery_task_entity,
                        recovery_epoch,
                        &recovery_task_source,
                        &recovery_task_candidate,
                        ttl,
                    )
                    .await
            })
            .await;
        assert!(matches!(recovery_result, Err(ClaimError::Conflict)));

        let orphan_owner = node_identity();
        let orphan_destination = node_identity();
        register_live_node(&store, &orphan_owner).await;
        let orphan_entity = sm_entity("post-lock-orphan-session");
        let orphan_epoch = store.acquire(&orphan_entity, &orphan_owner).await.unwrap();
        seed_sm_session_row(&store.db, &orphan_entity.id).await;
        store
            .db
            .guard()
            .await
            .unwrap()
            .execute(
                "UPDATE clustering_nodes SET expired = true WHERE node_id = ? AND node_epoch = ?",
                crate::db_params![
                    orphan_owner.node_id.clone(),
                    orphan_owner.node_epoch.clone()
                ],
            )
            .await
            .unwrap();
        register_live_node(&store, &orphan_destination).await;
        let orphan_store = PostgresClaimStore::with_lease_ttl(clean.db.clone(), ttl);
        let orphan_task_entity = orphan_entity.clone();
        let orphan_task_destination = orphan_destination.clone();
        let orphan_result =
            while_claim_is_locked_past_ttl(&clean.db, &orphan_entity, ttl, async move {
                orphan_store
                    .steal_orphaned_sm_session_claim(
                        &orphan_task_entity,
                        orphan_epoch,
                        &orphan_task_destination,
                        ttl,
                    )
                    .await
            })
            .await;
        assert!(matches!(orphan_result, Err(ClaimError::Conflict)));
        assert_eq!(
            store
                .current_claim(&orphan_entity)
                .await
                .unwrap()
                .unwrap()
                .owner,
            orphan_owner
        );
    }

    #[tokio::test]
    async fn lifecycle_updates_revalidate_after_node_lock_waits_past_ttl() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(clean) = clean_store().await else {
            return;
        };
        let ttl = Duration::from_millis(80);
        let store = PostgresClaimStore::with_lease_ttl(clean.db.clone(), ttl);

        let heartbeat_node = node_identity();
        register_live_node(&store, &heartbeat_node).await;
        let heartbeat_store = PostgresClaimStore::with_lease_ttl(clean.db.clone(), ttl);
        let heartbeat_task_node = heartbeat_node.clone();
        let renewed = while_node_is_locked_past_ttl(&clean.db, &heartbeat_node, ttl, async move {
            heartbeat_store.heartbeat(&heartbeat_task_node, ttl).await
        })
        .await
        .unwrap();
        assert!(!renewed);

        let predecessor = NodeIdentity::new("post-lock-activation", "old");
        register_live_node(&store, &predecessor).await;
        let candidate = NodeIdentity::new(predecessor.node_id.clone(), "candidate");
        store
            .register_draining_with_peer_id(&predecessor, &candidate, None, None, ttl)
            .await
            .unwrap();
        let activation_store = PostgresClaimStore::with_lease_ttl(clean.db.clone(), ttl);
        let activation_task_candidate = candidate.clone();
        let activated = while_node_is_locked_past_ttl(&clean.db, &candidate, ttl, async move {
            activation_store
                .activate(&activation_task_candidate, ttl)
                .await
        })
        .await
        .unwrap();
        assert!(!activated);

        let retry_node = node_identity();
        register_live_node(&store, &retry_node).await;
        let retry_store = PostgresClaimStore::with_lease_ttl(clean.db.clone(), ttl);
        let retry_task_node = retry_node.clone();
        let retry = while_node_is_locked_past_ttl(&clean.db, &retry_node, ttl, async move {
            retry_store.register(&retry_task_node, None).await
        })
        .await;
        assert!(matches!(retry, Err(ClaimError::Conflict)));

        let stable_id = format!("absent-register-{}", uuid::Uuid::new_v4());
        let first = NodeIdentity::new(stable_id.clone(), "first");
        let second = NodeIdentity::new(stable_id.clone(), "second");
        let first_store = PostgresClaimStore::with_lease_ttl(clean.db.clone(), ttl);
        let second_store = PostgresClaimStore::with_lease_ttl(clean.db.clone(), ttl);
        let (first_result, second_result) = tokio::join!(
            first_store.register(&first, None),
            second_store.register(&second, None),
        );
        assert_ne!(first_result.is_ok(), second_result.is_ok());
        let current = store
            .registered_identity(&stable_id)
            .await
            .unwrap()
            .unwrap();
        assert!(current == first || current == second);
    }

    #[tokio::test]
    async fn fenced_transaction_budget_releases_claim_and_node_locks() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let owner = node_identity();
        register_live_node(&store, &owner).await;
        let entity = sm_entity("bounded-fenced-transaction");
        let epoch = store.acquire(&entity, &owner).await.unwrap();

        let budget = waddle_xmpp::ownership::FencedTransactionBudget::from_millis(200);
        let mut stalled = store.db.begin().await.unwrap();
        stalled.configure_fenced(budget).await.unwrap();
        let mut rows = stalled
            .query(
                r#"
                SELECT 1
                FROM clustering_claims c
                JOIN clustering_nodes n
                  ON n.node_id = c.node_id AND n.node_epoch = c.node_epoch
                WHERE c.entity = ?
                  AND c.node_id = ?
                  AND c.node_epoch = ?
                  AND c.claim_epoch = ?
                FOR SHARE OF c, n
                "#,
                crate::db_params![
                    entity_key(&entity),
                    owner.node_id.clone(),
                    owner.node_epoch.clone(),
                    epoch.0,
                ],
            )
            .await
            .unwrap();
        assert!(rows.next().await.unwrap().is_some());
        drop(rows);

        let contender_db = store.db.clone();
        let contender_owner = owner.clone();
        let contender = tokio::spawn(async move {
            contender_db
                .control_plane_guard()
                .await
                .unwrap()
                .execute(
                    "UPDATE clustering_nodes SET draining = true WHERE node_id = ? AND node_epoch = ?",
                    crate::db_params![
                        contender_owner.node_id,
                        contender_owner.node_epoch,
                    ],
                )
                .await
        });
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(
            !contender.is_finished(),
            "the exact node fence must initially block the lifecycle update"
        );
        let affected = tokio::time::timeout(Duration::from_secs(3), contender)
            .await
            .expect("database-side fenced transaction timeout must release locks")
            .unwrap()
            .unwrap();
        assert_eq!(affected, 1);
        assert!(
            stalled.execute("SELECT 1", ()).await.is_err(),
            "the transaction itself must be terminated, not merely its waiting statement"
        );
    }
}
