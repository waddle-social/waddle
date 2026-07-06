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

use std::time::Duration;

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
                pod_template_hash TEXT,
                -- ADR-0017 Phase 3 Slice 2: set by `NodeLeaseStore::mark_draining`
                -- (Slice 10 consumes it — stop acquiring new claims, keep
                -- serving owned ones). No production deployments predate this
                -- column (pre-launch project), so it is added directly to the
                -- `CREATE TABLE IF NOT EXISTS` rather than a separate `ALTER
                -- TABLE` migration.
                draining          BOOLEAN NOT NULL DEFAULT FALSE
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
}

#[async_trait]
impl NodeLeaseStore for PostgresClaimStore {
    async fn register(
        &self,
        me: &NodeIdentity,
        pod_template_hash: Option<String>,
    ) -> Result<(), ClaimError> {
        // Runs on the control-plane pool (element 4/12, Slice 0): node
        // registration is liveness-control-plane traffic, never the main
        // pool.
        let conn = self.db.control_plane_guard().await.map_err(db_err)?;
        conn.execute(
            r#"
            INSERT INTO clustering_nodes (node_id, node_epoch, heartbeat, expired, pod_template_hash, draining)
            VALUES (?, ?, now(), false, ?, false)
            ON CONFLICT (node_id) DO UPDATE SET
                node_epoch = EXCLUDED.node_epoch,
                heartbeat = now(),
                expired = false,
                pod_template_hash = EXCLUDED.pod_template_hash,
                draining = false
            "#,
            crate::db_params![
                me.node_id.clone(),
                me.node_epoch.clone(),
                pod_template_hash,
            ],
        )
        .await
        .map_err(db_err)?;
        Ok(())
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
    use waddle_xmpp::ownership::EntityType;

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

    #[tokio::test]
    async fn acquire_succeeds_once_then_conflicts() {
        let _guard = clustering_control_plane_table_lock().lock().await;
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
        let _guard = clustering_control_plane_table_lock().lock().await;
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
        let _guard = clustering_control_plane_table_lock().lock().await;
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
        let _guard = clustering_control_plane_table_lock().lock().await;
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
        assert_eq!(epoch1, ClaimEpoch(1));
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
    async fn release_is_epoch_gated_and_idempotent() {
        let _guard = clustering_control_plane_table_lock().lock().await;
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
        let _guard = clustering_control_plane_table_lock().lock().await;
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
        let _guard = clustering_control_plane_table_lock().lock().await;
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

        // The dead peer's process is gone; nobody calls `expire` on its
        // row (no production caller this slice), but its heartbeat simply
        // stops advancing.
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
}
