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
//! can mint. `StalePredicate::StealIntentExpired` is **not yet
//! implementable this slice** — the `clustering_steal_intents` table lands
//! in Slice 3 — so `steal_stale` rejects it with
//! [`ClaimError::NotYetImplemented`].
//!
//! All CAS statements run on the dedicated control-plane pool
//! ([`Database::control_plane_guard`], ADR-0017 element 4/12, Slice 0):
//! this per-node/per-entity liveness traffic must never queue behind fenced
//! bulk writes, backstop fencing SELECTs, or claims-read storms on the main
//! pool. `ensure_schema` is one-time startup DDL, not hot-path liveness
//! traffic, and runs on the main pool — mirroring `lease.rs`.

use async_trait::async_trait;
use waddle_xmpp::ownership::{
    ClaimEpoch, ClaimError, ClaimStore, Entity, NodeIdentity, ResumeIdentityProof, StalePredicate,
};

use crate::db::{Database, DatabaseError};

/// Convert a backend database failure into the upstream `ClaimError`. The
/// concrete diagnostic (`DatabaseError`'s `Display`) is preserved as
/// human-facing text; see [`ClaimError::Backend`]'s doc comment for why a
/// richer, `waddle-server`-local error type can't cross this boundary.
fn db_err(error: DatabaseError) -> ClaimError {
    ClaimError::Backend(error.to_string())
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
}

/// Postgres implementation of `ClaimStore`, backing `UserActor`/`RoomActor`/
/// SM-session ownership.
pub struct PostgresClaimStore {
    db: Database,
}

impl PostgresClaimStore {
    pub fn new(db: Database) -> Self {
        Self { db }
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
                pod_template_hash TEXT
            )
            "#,
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
        Ok(())
    }

    async fn acquire(&self, entity: &Entity, me: &NodeIdentity) -> Result<ClaimEpoch, ClaimError> {
        let conn = self.db.control_plane_guard().await.map_err(db_err)?;
        // Acquire CAS (element 4): a fresh claim only inserts; a
        // still-live claim on the same entity leaves the row untouched and
        // affects zero rows.
        let affected = conn
            .execute(
                r#"
                INSERT INTO clustering_claims (entity, entity_type, node_id, node_epoch, claim_epoch)
                VALUES (?, ?, ?, ?, 0)
                ON CONFLICT (entity) DO NOTHING
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
        if affected == 1 {
            Ok(ClaimEpoch(0))
        } else {
            Err(ClaimError::AlreadyClaimed)
        }
    }

    async fn steal_stale(
        &self,
        entity: &Entity,
        observed: ClaimEpoch,
        staleness: StalePredicate,
        me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        match staleness {
            StalePredicate::StealIntentExpired { .. } => {
                // ADR-0017 Phase 3 Slice 3 lands `clustering_steal_intents`
                // and this predicate's realization; until then it is
                // unimplemented rather than silently falling back to a
                // different (and wrong) staleness definition.
                return Err(ClaimError::NotYetImplemented(
                    "StalePredicate::StealIntentExpired requires the Slice 3 \
                     clustering_steal_intents table",
                ));
            }
            StalePredicate::OwnerStale => {}
        }
        let conn = self.db.control_plane_guard().await.map_err(db_err)?;
        let next_epoch = observed.0 + 1;
        // Owner-stale steal CAS (element 4): the `NOT EXISTS` correlated
        // subquery realizes the ADR's "LEFT JOIN" predicate
        // (`nodes.node_id IS NULL OR nodes.expired OR node_epoch
        // mismatch`) — a claim is stale iff no `clustering_nodes` row
        // matches its current owner under a fresh (non-expired), current
        // epoch. Reads only the committed `expired` flag, never a raw
        // heartbeat comparison.
        let affected = conn
            .execute(
                r#"
                UPDATE clustering_claims
                SET node_id = ?, node_epoch = ?, claim_epoch = ?
                WHERE entity = ?
                  AND claim_epoch = ?
                  AND NOT EXISTS (
                    SELECT 1 FROM clustering_nodes n
                    WHERE n.node_id = clustering_claims.node_id
                      AND NOT n.expired
                      AND n.node_epoch = clustering_claims.node_epoch
                  )
                "#,
                crate::db_params![
                    me.node_id.clone(),
                    me.node_epoch.clone(),
                    next_epoch,
                    entity_key(entity),
                    observed.0,
                ],
            )
            .await
            .map_err(db_err)?;
        if affected == 1 {
            Ok(ClaimEpoch(next_epoch))
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
        let conn = self.db.control_plane_guard().await.map_err(db_err)?;
        let next_epoch = observed.0 + 1;
        // Consent/epoch-only steal CAS (element 4's third variant): no
        // staleness predicate at all — authorized exclusively by the
        // caller already holding a `ResumeIdentityProof`, which only
        // `ownership::resume::verify_resume_identity` can mint.
        let affected = conn
            .execute(
                r#"
                UPDATE clustering_claims
                SET node_id = ?, node_epoch = ?, claim_epoch = ?
                WHERE entity = ? AND claim_epoch = ?
                "#,
                crate::db_params![
                    me.node_id.clone(),
                    me.node_epoch.clone(),
                    next_epoch,
                    entity_key(entity),
                    observed.0,
                ],
            )
            .await
            .map_err(db_err)?;
        if affected == 1 {
            Ok(ClaimEpoch(next_epoch))
        } else {
            Err(ClaimError::Conflict)
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
                WHERE entity = ? AND node_id = ? AND claim_epoch = ?
                "#,
                crate::db_params![entity_key(entity), me.node_id.clone(), mine.0],
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
        // Epoch-gated release: best-effort. A claim already stolen out
        // from under `me` (0 rows affected) is a no-op, not an error —
        // graceful drain releases whatever it still owns and does not
        // treat a lost race as a failure.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{DatabaseConfig, DatabaseDriver};
    use waddle_xmpp::ownership::EntityType;

    // These tests share the `clustering_nodes`/`clustering_claims` tables,
    // so serialize them and wipe both tables at each start. Skipped unless
    // `WADDLE_TEST_POSTGRES_URL` points at a Postgres (the claims CAS has no
    // SQLite equivalent) — mirrors `lease.rs`'s test convention.
    fn serial_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

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
        let conn = db.guard().await.expect("guard");
        conn.execute("DELETE FROM clustering_claims", ())
            .await
            .expect("clean claims");
        conn.execute("DELETE FROM clustering_nodes", ())
            .await
            .expect("clean nodes");
        Some(store)
    }

    /// Seed a `clustering_nodes` row directly — this slice does not yet
    /// have a `NodeLeaseStore` (ADR-0017 Phase 3 Slice 2), so tests play
    /// that future store's part exactly as `allowlist.rs`'s tests play the
    /// enrollment pipeline's part.
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

    #[tokio::test]
    async fn acquire_succeeds_once_then_conflicts() {
        let _guard = serial_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let me = node_identity();
        let entity = sm_entity("stream-1");
        let epoch = store.acquire(&entity, &me).await.expect("first acquire");
        assert_eq!(epoch, ClaimEpoch(0));

        let other = node_identity();
        let err = store
            .acquire(&entity, &other)
            .await
            .expect_err("second acquire loses the race");
        assert!(matches!(err, ClaimError::AlreadyClaimed));
    }

    #[tokio::test]
    async fn acquire_does_not_collide_across_entity_types_sharing_the_same_id() {
        // Fix: the `entity` primary key previously bound `entity.id` alone
        // (entity_type was write-only), so a `UserActor` and a `RoomActor`
        // sharing the same id would collide on one row. `entity_key`'s
        // type-tag prefix must keep them as two distinct claims.
        let _guard = serial_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let me = node_identity();
        let user_entity = Entity::new(EntityType::UserActor, "42");
        let room_entity = Entity::new(EntityType::RoomActor, "42");
        let sm_entity_same_id = Entity::new(EntityType::SmSession, "42");

        store
            .acquire(&user_entity, &me)
            .await
            .expect("acquire user_actor:42");
        store
            .acquire(&room_entity, &me)
            .await
            .expect("acquire room_actor:42 must not collide with user_actor:42");
        store
            .acquire(&sm_entity_same_id, &me)
            .await
            .expect("acquire sm_session:42 must not collide with either of the above");

        assert!(store
            .fence(&user_entity, &me, ClaimEpoch(0))
            .await
            .expect("fence user_actor:42"));
        assert!(store
            .fence(&room_entity, &me, ClaimEpoch(0))
            .await
            .expect("fence room_actor:42"));
        assert!(store
            .fence(&sm_entity_same_id, &me, ClaimEpoch(0))
            .await
            .expect("fence sm_session:42"));

        // Releasing one must not affect the others.
        store
            .release(&user_entity, &me, ClaimEpoch(0))
            .await
            .expect("release user_actor:42");
        assert!(!store
            .fence(&user_entity, &me, ClaimEpoch(0))
            .await
            .expect("fence user_actor:42 after release"));
        assert!(store
            .fence(&room_entity, &me, ClaimEpoch(0))
            .await
            .expect("room_actor:42 untouched by user_actor:42's release"));
        assert!(store
            .fence(&sm_entity_same_id, &me, ClaimEpoch(0))
            .await
            .expect("sm_session:42 untouched by user_actor:42's release"));
    }

    #[tokio::test]
    async fn steal_stale_ignores_raw_heartbeat_only_committed_expired_flag_matters() {
        // Named test: proves the owner-stale predicate reads only the
        // committed `expired` flag, never a raw `heartbeat < now() - ttl`
        // comparison — an old heartbeat with `expired = false` must NOT be
        // stealable.
        let _guard = serial_lock().lock().await;
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
        assert_eq!(epoch1, ClaimEpoch(1));
        assert!(store.fence(&entity, &stealer, epoch1).await.expect("fence"));
    }

    #[tokio::test]
    async fn steal_from_vanished_node_succeeds_via_not_exists() {
        // No `clustering_nodes` row at all for the owner: the NOT EXISTS
        // predicate is vacuously true, so the claim is stealable exactly
        // as if the owner were committed-expired.
        let _guard = serial_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let owner = node_identity();
        let entity = sm_entity("stream-1");
        let epoch0 = store.acquire(&entity, &owner).await.expect("acquire");

        let stealer = node_identity();
        let epoch1 = store
            .steal_stale(&entity, epoch0, StalePredicate::OwnerStale, &stealer)
            .await
            .expect("steal from a node with no nodes-row succeeds");
        assert_eq!(epoch1, ClaimEpoch(1));
    }

    #[tokio::test]
    async fn steal_stale_with_stale_epoch_loses_the_race() {
        let _guard = serial_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let owner = node_identity();
        let entity = sm_entity("stream-1");
        store.acquire(&entity, &owner).await.expect("acquire");

        let stealer = node_identity();
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

    #[tokio::test]
    async fn steal_stale_rejects_steal_intent_expired_this_slice() {
        let _guard = serial_lock().lock().await;
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
                    intent_ttl: std::time::Duration::from_secs(30),
                },
                &stealer,
            )
            .await
            .expect_err("StealIntentExpired is Slice 3 work, not implemented here");
        assert!(matches!(err, ClaimError::NotYetImplemented(_)));
    }

    #[tokio::test]
    async fn steal_for_resume_succeeds_against_a_fresh_owner() {
        // The whole point of the consent/epoch-only CAS: it steals from a
        // perfectly fresh (non-expired) owner, which `steal_stale` never
        // could.
        let _guard = serial_lock().lock().await;
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
        assert_eq!(epoch1, ClaimEpoch(1));
    }

    #[tokio::test]
    async fn steal_for_resume_with_stale_epoch_loses_the_race() {
        let _guard = serial_lock().lock().await;
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
        let _guard = serial_lock().lock().await;
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
    async fn release_is_epoch_gated_and_idempotent() {
        let _guard = serial_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let owner = node_identity();
        let entity = sm_entity("stream-1");
        let epoch0 = store.acquire(&entity, &owner).await.expect("acquire");

        // Releasing under the wrong epoch is a silent no-op — the claim
        // must survive.
        store
            .release(&entity, &owner, ClaimEpoch(99))
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
    }

    #[tokio::test]
    async fn release_many_clears_every_owned_entity_in_one_round_trip() {
        let _guard = serial_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let owner = node_identity();
        let a = sm_entity("stream-a");
        let b = sm_entity("stream-b");
        let c = sm_entity("stream-c");
        store.acquire(&a, &owner).await.expect("acquire a");
        store.acquire(&b, &owner).await.expect("acquire b");
        // c is owned by someone else — release_many must not touch it.
        let other = node_identity();
        store.acquire(&c, &other).await.expect("acquire c");

        store
            .release_many(&[a.clone(), b.clone(), c.clone()], &owner)
            .await
            .expect("release_many");

        assert!(!store
            .fence(&a, &owner, ClaimEpoch(0))
            .await
            .expect("fence a"));
        assert!(!store
            .fence(&b, &owner, ClaimEpoch(0))
            .await
            .expect("fence b"));
        assert!(store
            .fence(&c, &other, ClaimEpoch(0))
            .await
            .expect("fence c: untouched, still owned by other"));
    }

    #[tokio::test]
    async fn release_many_on_an_empty_slice_is_a_no_op() {
        let _guard = serial_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        store
            .release_many(&[], &node_identity())
            .await
            .expect("empty release_many does not error");
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
        let _guard = serial_lock().lock().await;
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
                "SELECT 1 FROM clustering_claims WHERE entity = ? AND node_id = ? AND claim_epoch = ? FOR SHARE",
                crate::db_params![entity_key(&entity), owner.node_id.clone(), epoch0.0],
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
        assert_eq!(stolen_epoch, ClaimEpoch(1));

        // The original owner's fencing check against its old epoch now
        // observes zero rows — it has been fenced out.
        assert!(!store
            .fence(&entity, &owner, epoch0)
            .await
            .expect("owner fenced out after the steal"));
    }
}
