//! Postgres-authoritative, epoch-fenced XEP-0397 ISR token storage
//! (ADR-0017 Phase 3 Slice 8, element 10, Q8's revised decision).
//!
//! Implements `waddle_xmpp::isr::IsrTokenStore` — defined upstream in
//! `waddle-xmpp` (mirroring the `ClaimStore` split, Q1) — for a
//! `waddle-server`-local `PostgresIsrTokenStore`, legal under Rust's orphan
//! rule exactly like [`super::claims::PostgresClaimStore`].
//!
//! **Schema** (`clustering_isr_tokens`, PROPOSED — no ADR-locked DDL exists
//! for this table; see the phase plan's Slice 8): keyed by `sm_id`, the
//! same non-secret key as the `sm_session` claim's `entity`. A dedicated
//! table, not columns bolted onto `sm_sessions`/`sm_unacked` — see Q8's
//! full rationale in the phase plan (ISR has a distinct, single-use/rotated
//! lifecycle orthogonal to the SM session's own).
//!
//! **Locked consume spec** (element 10, quoted verbatim in the phase plan):
//! token consume *"fetches the token row by the non-secret key (the
//! SM-ID/claim), compares the stored token against the presented token in
//! Rust with a constant-time primitive (`subtle`/`constant_time_eq`), and
//! only then performs the delete — all inside one epoch-fenced, `FOR
//! SHARE`-locked transaction preserving single-use atomicity, bound to the
//! same authenticated-identity check as resume (element 8)."* Matching the
//! token in a SQL `WHERE` clause is explicitly banned as a timing oracle —
//! [`PostgresIsrTokenStore::consume`] never does this: the `SELECT` is
//! keyed by `sm_id` only, and the comparison happens in Rust via
//! [`subtle::ConstantTimeEq`].
//!
//! **Pool assignment** (Slice 0's rule, restated for this slice): the
//! consume transaction's fencing `SELECT ... FOR SHARE` against
//! `clustering_claims` and its `clustering_isr_tokens` reads/writes all run
//! on the **main pool** ([`Database::begin`]) — a fencing lock and the
//! write it guards must share one connection/one transaction, exactly the
//! rule Slices 4 and 7 already follow. `ensure_schema`/`issue` are
//! single-statement, non-fenced operations and run on the main pool via
//! [`Database::guard`] (mirroring `ensure_schema`'s own placement in
//! `claims.rs`/`lease.rs`/`allowlist.rs`).
//!
//! **Identity binding**: this store does *not* itself call
//! `ownership::resume::verify_resume_identity` — that check happens in the
//! caller (the WebSocket ISR-resume handler), *before* `consume` is ever
//! invoked, exactly mirroring the resume path's own "identity check before
//! any write" rule (element 8). `consume`'s only job is the token
//! compare/rotate/destroy under the SM-session claim's fence.
//!
//! **Council-adjudicated FIX 1 (deviation from the locked spec's literal
//! delete ordering, verified against a live Postgres)**: [`consume`] no
//! longer issues an unconditional `DELETE` immediately after the `SELECT
//! ... FOR UPDATE`. It first branches on whether that `SELECT` found a
//! row at all — a concurrent loser's blocked `FOR UPDATE` read observes
//! **no row** once the winner's transaction commits its `DELETE`
//! (Postgres's row-lock semantics for a deleted-not-updated row), so
//! gating the `DELETE`/comparison on `stored.is_some()` is what prevents
//! the loser from deleting the winner's freshly-rotated replacement row —
//! see [`PostgresIsrTokenStore::consume`]'s own doc comment for the full
//! trace. This is also a partial reconciliation with element 10's literal
//! "compare... and only then performs the delete" text (conformance
//! finding 8): the delete is now conditioned on a compare-worthy
//! precondition (a locked row existing) rather than running completely
//! unconditionally as before, though the actual DELETE statement still
//! precedes the Rust-side token comparison in program order (kept that
//! way deliberately, matching deviation 77's original reasoning: the same
//! delete must happen whether the subsequent compare matches or not, so
//! splitting it into two separate DELETE call sites gains nothing and
//! only risks the two branches drifting). Recorded as a deviation, not
//! silently claimed as full literal-order conformance.
//!
//! **Council-adjudicated FIX 3**: the no-row branch above returns a
//! distinct [`IsrConsumeOutcome::NoSuchToken`], not `Mismatched` — the
//! caller (`isr_resume.rs`) destroys SM session state only for a genuine
//! `Mismatched` (a row existed, the presented token didn't match it),
//! never for `NoSuchToken` (never opted into ISR, or a legitimate
//! concurrent loser).

use async_trait::async_trait;
use subtle::ConstantTimeEq;
use waddle_xmpp::isr::{
    generate_isr_token, IsrConsumeOutcome, IsrTokenStore, IsrTokenStoreError, IssuedIsrToken,
};
use waddle_xmpp::ownership::{ClaimEpoch, ClaimGrant, EntityType, NodeIdentity};

use crate::db::{Database, DatabaseError};

/// Convert a backend database failure into the upstream
/// [`IsrTokenStoreError`]. Mirrors [`super::claims::db_err`] one type over
/// — a human-facing `Display` conversion, not a structured payload (see
/// [`IsrTokenStoreError::Backend`]'s doc comment).
fn db_err(error: DatabaseError) -> IsrTokenStoreError {
    IsrTokenStoreError::Backend(error.to_string())
}

/// The same `<entity_type_tag>:<id>` encoding
/// `waddle-server::clustering::claims::entity_key` and
/// `sm_persistence_fenced::sm_session_entity_key` use for the
/// `clustering_claims.entity` primary key — duplicated here rather than
/// imported for the same reason `sm_persistence_fenced.rs` duplicates it:
/// this store owns its own inline fencing SQL, so it also owns its own copy
/// of the key encoding it binds into that SQL.
fn sm_session_entity_key(sm_id: &str) -> String {
    format!("{}:{}", EntityType::SmSession.as_db_str(), sm_id)
}

/// Postgres-only, `clustering`-fenced [`IsrTokenStore`].
pub struct PostgresIsrTokenStore {
    db: Database,
}

impl PostgresIsrTokenStore {
    /// Construct against an already-opened `Database` handle. Callers MUST
    /// pass the same handle `clustering::start_if_enabled` uses for
    /// `PostgresClaimStore` — the consume transaction's fencing `SELECT`
    /// targets `clustering_claims`, which lives in that same database.
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Wait for the exact token row (or serialize its absence) before a
    /// caller computes a database-owned effect timestamp. PostgreSQL may
    /// evaluate an `INSERT .. ON CONFLICT` VALUES expression before it
    /// waits on the conflicting row, so the explicit lock must precede
    /// `clock_timestamp()`.
    async fn lock_token_effect_row(
        tx: &mut crate::db::Transaction<'_>,
        sm_id: &str,
    ) -> Result<(), IsrTokenStoreError> {
        tx.execute(
            "SELECT pg_advisory_xact_lock(hashtextextended(?, 8721))",
            crate::db_params![sm_id.to_string()],
        )
        .await
        .map_err(db_err)?;
        let mut rows = tx
            .query(
                "SELECT 1 FROM clustering_isr_tokens WHERE sm_id = ? FOR UPDATE",
                crate::db_params![sm_id.to_string()],
            )
            .await
            .map_err(db_err)?;
        let _ = rows.next().await.map_err(db_err)?;
        Ok(())
    }
}

#[async_trait]
impl IsrTokenStore for PostgresIsrTokenStore {
    async fn ensure_schema(&self) -> Result<(), IsrTokenStoreError> {
        let conn = self.db.guard().await.map_err(db_err)?;
        conn.execute(
            r#"
            CREATE TABLE IF NOT EXISTS clustering_isr_tokens (
                sm_id      TEXT PRIMARY KEY,
                token      TEXT NOT NULL,
                mechanism  TEXT NOT NULL,
                created_at TIMESTAMPTZ NOT NULL DEFAULT now()
            )
            "#,
            (),
        )
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn issue(
        &self,
        sm_id: &str,
        mechanism: &str,
        grant: &ClaimGrant,
    ) -> Result<IssuedIsrToken, IsrTokenStoreError> {
        let token = generate_isr_token();
        if grant.entity.entity_type != EntityType::SmSession || grant.entity.id != sm_id {
            return Err(IsrTokenStoreError::NotOwner);
        }
        let mut tx = self.db.begin_fenced().await.map_err(db_err)?;
        let mut fence_rows = tx
            .query(
                "WITH locked AS MATERIALIZED ( \
                     SELECT n.heartbeat, n.expired, n.lease_ttl_ms \
                     FROM clustering_claims c \
                     JOIN clustering_nodes n ON n.node_id = c.node_id AND n.node_epoch = c.node_epoch \
                     WHERE c.entity = ? AND c.node_id = ? AND c.node_epoch = ? AND c.claim_epoch = ? \
                     FOR SHARE OF c, n \
                 ) \
                 SELECT 1 FROM locked WHERE NOT expired \
                   AND heartbeat >= clock_timestamp() - (lease_ttl_ms::text || ' milliseconds')::interval",
                crate::db_params![
                    sm_session_entity_key(sm_id),
                    grant.owner.node_id.clone(),
                    grant.owner.node_epoch.clone(),
                    grant.epoch.0,
                ],
            )
            .await
            .map_err(db_err)?;
        let fenced = fence_rows.next().await.map_err(db_err)?.is_some();
        drop(fence_rows);
        if !fenced {
            return Err(IsrTokenStoreError::NotOwner);
        }
        Self::lock_token_effect_row(&mut tx, sm_id).await?;
        tx.execute(
            "INSERT INTO clustering_isr_tokens (sm_id, token, mechanism, created_at) \
             VALUES (?, ?, ?, clock_timestamp()) \
             ON CONFLICT (sm_id) DO UPDATE SET \
                 token = EXCLUDED.token, mechanism = EXCLUDED.mechanism, created_at = clock_timestamp()",
            crate::db_params![sm_id.to_string(), token.clone(), mechanism.to_string()],
        )
        .await
        .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        Ok(IssuedIsrToken {
            token,
            mechanism: mechanism.to_string(),
        })
    }

    async fn consume(
        &self,
        sm_id: &str,
        presented_token: &[u8],
        mechanism: &str,
        me: &NodeIdentity,
        mine: ClaimEpoch,
    ) -> Result<IsrConsumeOutcome, IsrTokenStoreError> {
        let mut tx = self.db.begin_fenced().await.map_err(db_err)?;

        // Fencing check: identical shape to `assert_fenced` in
        // `sm_persistence_fenced.rs`/`muc_durable.rs` — one connection, one
        // transaction, shared with the token read/delete/insert below.
        let key = sm_session_entity_key(sm_id);
        let mut fence_rows = tx
            .query(
                "WITH locked AS MATERIALIZED ( \
                     SELECT n.heartbeat, n.expired, n.lease_ttl_ms \
                     FROM clustering_claims c \
                     JOIN clustering_nodes n ON n.node_id = c.node_id AND n.node_epoch = c.node_epoch \
                     WHERE c.entity = ? AND c.node_id = ? AND c.node_epoch = ? AND c.claim_epoch = ? \
                     FOR SHARE OF c, n \
                 ) \
                 SELECT 1 FROM locked WHERE NOT expired \
                   AND heartbeat >= clock_timestamp() - (lease_ttl_ms::text || ' milliseconds')::interval",
                crate::db_params![key, me.node_id.clone(), me.node_epoch.clone(), mine.0],
            )
            .await
            .map_err(db_err)?;
        let fenced = fence_rows.next().await.map_err(db_err)?.is_some();
        drop(fence_rows);
        if !fenced {
            // Dropping `tx` here rolls back — no token row is touched.
            return Err(IsrTokenStoreError::NotOwner);
        }

        // Fetch by the non-secret key (`sm_id`) only — NEVER by token. The
        // comparison happens in Rust below, not in this WHERE clause.
        //
        // `FOR UPDATE` (not a bare `SELECT`) closes a single-use-atomicity
        // race a plain read would leave open: without it, two concurrent
        // `consume` calls for the same `sm_id` could both read the same
        // not-yet-deleted row before either commits, both conclude "match",
        // and both rotate — the exact double-spend single-use atomicity
        // exists to prevent. `FOR UPDATE` forces the second transaction to
        // block until the first commits (or rolls back) and then re-reads
        // the row's post-commit state (empty, once the first transaction's
        // `DELETE` has committed), so only the winner ever observes a match.
        let mut token_rows = tx
            .query(
                "SELECT token, mechanism FROM clustering_isr_tokens WHERE sm_id = ? FOR UPDATE",
                crate::db_params![sm_id.to_string()],
            )
            .await
            .map_err(db_err)?;
        let stored_row = token_rows.next().await.map_err(db_err)?;
        let stored = match stored_row {
            Some(row) => {
                let token: String = row.get(0).map_err(db_err)?;
                let stored_mechanism: String = row.get(1).map_err(db_err)?;
                Some((token, stored_mechanism))
            }
            None => None,
        };
        drop(token_rows);

        // Council-adjudicated FIX 1 (critical — verified against a live
        // Postgres): if this transaction's own `FOR UPDATE` found NO row,
        // commit read-only and return immediately — WITHOUT issuing any
        // DELETE/INSERT. The pre-fix code ran an unconditional
        // `DELETE ... WHERE sm_id = ?` here regardless of whether `stored`
        // was `Some`, reasoning (deviation 77) that "the delete must happen
        // either way" (destroy-on-mismatch, destroy-then-rotate-on-match).
        // That reasoning missed a concurrent-consumer race: when two
        // `consume` calls race for the same `sm_id`, the loser's
        // `SELECT ... FOR UPDATE` blocks on the winner's row lock. Once the
        // winner's transaction COMMITS a `DELETE` (rotation is
        // delete-then-insert, never an `UPDATE` of the same row), Postgres
        // does not re-run the loser's query against the winner's freshly
        // INSERTed replacement row — per Postgres's own documented
        // row-locking semantics (`SELECT ... FOR UPDATE`), a transaction
        // blocked on a row that a concurrent committer DELETEs simply
        // "ignores the row" once unblocked, even if a new row sharing the
        // same primary key value was subsequently inserted by that same
        // committer. So the loser's `stored` here is genuinely `None` — not
        // a stale read of a row that still exists. The pre-fix code's
        // unconditional delete, reached via this same `None` path, would
        // still issue `DELETE FROM clustering_isr_tokens WHERE sm_id = ?`
        // — which, since the winner's rotated row already matches that
        // predicate, deletes the WINNER'S freshly-rotated row. The loser's
        // caller (`isr_resume.rs`) would then treat its own (formerly)
        // `Mismatched` outcome as a wrong-token attack and destroy the
        // winner's just-resumed session — reproduced against a live
        // Postgres before this fix, fixed by the early return below.
        //
        // Council-adjudicated FIX 3 threads this as a distinct
        // `NoSuchToken` outcome (never opted into ISR, or already
        // consumed/reaped by a genuine loser) rather than folding it into
        // `Mismatched` (a real ISR-enabled session whose presented token
        // was wrong) — `isr_resume.rs` destroys session state only for the
        // latter.
        let Some((stored_token, stored_mechanism)) = stored else {
            tx.commit().await.map_err(db_err)?;
            return Ok(IsrConsumeOutcome::NoSuchToken);
        };

        // This transaction's own `FOR UPDATE` holds the lock on this exact
        // row — safe to destroy it unconditionally now. Per XEP-0397's
        // anti-brute-force MUST, a failed-token attempt against a valid
        // SM-ID destroys the token outright; a successful attempt
        // destroys-then-rotates in the same statement group. Either way
        // the DELETE runs exactly once, right here — never before this
        // tx has confirmed (via the `FOR UPDATE` above) that it, not some
        // other transaction, owns the row about to be destroyed
        // (conformance finding 8's compare/delete-ordering nuance vs.
        // element 10's literal "compare, and only then delete" text is
        // recorded as a deviation — see this module's doc comment).
        tx.execute(
            "DELETE FROM clustering_isr_tokens WHERE sm_id = ?",
            crate::db_params![sm_id.to_string()],
        )
        .await
        .map_err(db_err)?;

        // Constant-time comparison (element 10): never `==` on the secret
        // token bytes. The mechanism pin is non-secret metadata, compared
        // plainly — a mismatch here means the client is presenting the
        // token under a different mechanism than it was pinned to, which
        // XEP-0397 treats as invalid use of the token.
        let matched = stored_mechanism == mechanism
            && bool::from(stored_token.as_bytes().ct_eq(presented_token));

        if !matched {
            tx.commit().await.map_err(db_err)?;
            return Ok(IsrConsumeOutcome::Mismatched);
        }

        // Rotation: delete-then-insert is one atomic unit inside this same
        // transaction (minor fix 21) — a crash/rollback between them can
        // never leave a deleted-but-unrotated state, and the reply the
        // caller builds is constructed only from `rotated` below, which is
        // only returned after `commit()` succeeds.
        let rotated_token = generate_isr_token();
        tx.execute(
            "INSERT INTO clustering_isr_tokens (sm_id, token, mechanism, created_at) \
             VALUES (?, ?, ?, clock_timestamp())",
            crate::db_params![
                sm_id.to_string(),
                rotated_token.clone(),
                mechanism.to_string()
            ],
        )
        .await
        .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;

        Ok(IsrConsumeOutcome::Matched {
            rotated: IssuedIsrToken {
                token: rotated_token,
                mechanism: mechanism.to_string(),
            },
        })
    }

    async fn sweep_expired(&self, max_age: std::time::Duration) -> Result<u64, IsrTokenStoreError> {
        // Council-adjudicated FIX 4: TTL backstop, not a cascade off the SM
        // session claim's own release/reap paths. A cascade would need the
        // SM session registry (`waddle-xmpp`, cross-node-generic) to know
        // about ISR (`waddle-server`-local, Postgres-only) at all — the
        // exact coupling `ClaimStore`/`IsrTokenStore`'s trait split
        // deliberately avoids. `created_at` already exists on this table
        // (the schema this module's own `ensure_schema` creates), so a
        // bounded sweep over it, run at the same cadence as
        // `session_janitors.rs`'s orphan-reaper sweep, is the smaller
        // correct option. Mirrors `lease.rs`'s own
        // `(? || ' milliseconds')::interval` TTL-comparison idiom.
        let conn = self.db.guard().await.map_err(db_err)?;
        let deleted = conn
            .execute(
                "DELETE FROM clustering_isr_tokens \
                 WHERE created_at < now() - (? || ' milliseconds')::interval",
                crate::db_params![max_age.as_millis().to_string()],
            )
            .await
            .map_err(db_err)?;
        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clustering::claims::{
        clustering_control_plane_table_lock, NodeLeaseStore, PostgresClaimStore,
    };
    use crate::db::{DatabaseConfig, DatabaseDriver};
    use waddle_xmpp::ownership::{ClaimStore, Entity};

    fn node_identity() -> NodeIdentity {
        NodeIdentity::new(
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
        )
    }

    async fn test_db() -> Option<Database> {
        let url = std::env::var("WADDLE_TEST_POSTGRES_URL").ok()?;
        // `PostgresClaimStore`'s CAS statements run on the control-plane
        // pool (its own module doc) — `claim_sm_session` below needs one
        // configured, mirroring `claims.rs`'s own `clean_store()` test
        // helper exactly.
        let db = Database::from_config(
            "clustering-isr-test",
            &DatabaseConfig::new(DatabaseDriver::Postgres, url)
                .with_control_plane_pool(crate::db::DEFAULT_CONTROL_PLANE_POOL_SIZE),
        )
        .await
        .expect("open test postgres");
        Some(db)
    }

    /// Acquire a real SM-session claim (via `PostgresClaimStore`) so a
    /// `consume` test can present a genuinely fenced `(me, epoch)` pair —
    /// mirrors how `sm_persistence_fenced.rs`'s own Postgres-gated tests
    /// establish a fenceable claim before exercising a fenced write.
    async fn claim_sm_session(db: &Database, sm_id: &str, me: &NodeIdentity) -> ClaimGrant {
        let claim_store = PostgresClaimStore::new(db.clone());
        claim_store
            .ensure_schema()
            .await
            .expect("ensure claims schema");
        claim_store
            .register(me, None)
            .await
            .expect("register live claimant");
        let entity = Entity::new(EntityType::SmSession, sm_id.to_string());
        let epoch = claim_store
            .acquire(&entity, me)
            .await
            .expect("acquire sm_session claim");
        ClaimGrant::new(entity, me.clone(), epoch)
    }

    #[tokio::test]
    async fn consume_matches_and_rotates_inside_the_fenced_transaction() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(db) = test_db().await else {
            return;
        };
        let store = PostgresIsrTokenStore::new(db.clone());
        store.ensure_schema().await.expect("ensure isr schema");
        let me = node_identity();
        let sm_id = format!("sm-{}", uuid::Uuid::new_v4());
        let grant = claim_sm_session(&db, &sm_id, &me).await;

        let issued = store.issue(&sm_id, "PLAIN", &grant).await.expect("issue");
        let outcome = store
            .consume(&sm_id, issued.token.as_bytes(), "PLAIN", &me, grant.epoch)
            .await
            .expect("consume");
        let IsrConsumeOutcome::Matched { rotated } = outcome else {
            panic!("expected Matched, got {outcome:?}");
        };
        assert_ne!(rotated.token, issued.token);

        // Single-use: the OLD token fails now, even under the same fence.
        let replay = store
            .consume(&sm_id, issued.token.as_bytes(), "PLAIN", &me, grant.epoch)
            .await
            .expect("consume");
        assert_eq!(replay, IsrConsumeOutcome::Mismatched);
    }

    #[tokio::test]
    async fn consume_with_wrong_token_destroys_the_row_without_sql_where_comparison() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(db) = test_db().await else {
            return;
        };
        let store = PostgresIsrTokenStore::new(db.clone());
        store.ensure_schema().await.expect("ensure isr schema");
        let me = node_identity();
        let sm_id = format!("sm-{}", uuid::Uuid::new_v4());
        let grant = claim_sm_session(&db, &sm_id, &me).await;

        let issued = store.issue(&sm_id, "PLAIN", &grant).await.expect("issue");
        let outcome = store
            .consume(&sm_id, b"not-the-token", "PLAIN", &me, grant.epoch)
            .await
            .expect("consume");
        assert_eq!(outcome, IsrConsumeOutcome::Mismatched);

        // The row is destroyed — a subsequent attempt with the CORRECT
        // token now finds no row at all (FIX 3: `NoSuchToken`, distinct
        // from the genuine `Mismatched` above), proving unconditional
        // destruction happened.
        let second = store
            .consume(&sm_id, issued.token.as_bytes(), "PLAIN", &me, grant.epoch)
            .await
            .expect("consume");
        assert_eq!(second, IsrConsumeOutcome::NoSuchToken);
    }

    /// Council-adjudicated FIX 1/FIX 3: a `consume` attempt against an
    /// SM-ID that never had a token issued at all (no `<isr-enable/>` ever
    /// ran for it) must find no row under its `FOR UPDATE` lock, commit
    /// read-only, and return `NoSuchToken` — never `Mismatched` (which
    /// would tell the caller to destroy session state for a session that
    /// never opted into ISR).
    #[tokio::test]
    async fn consume_with_no_token_ever_issued_returns_no_such_token_and_touches_nothing() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(db) = test_db().await else {
            return;
        };
        let store = PostgresIsrTokenStore::new(db.clone());
        store.ensure_schema().await.expect("ensure isr schema");
        let me = node_identity();
        let sm_id = format!("sm-{}", uuid::Uuid::new_v4());
        let grant = claim_sm_session(&db, &sm_id, &me).await;

        let outcome = store
            .consume(&sm_id, b"anything", "PLAIN", &me, grant.epoch)
            .await
            .expect("consume");
        assert_eq!(outcome, IsrConsumeOutcome::NoSuchToken);

        // Nothing was touched: issuing a token AFTER this failed attempt
        // and consuming it must succeed normally — proving the no-row
        // branch never wrote anything that could poison a later issuance.
        let issued = store.issue(&sm_id, "PLAIN", &grant).await.expect("issue");
        let second = store
            .consume(&sm_id, issued.token.as_bytes(), "PLAIN", &me, grant.epoch)
            .await
            .expect("consume");
        assert!(matches!(second, IsrConsumeOutcome::Matched { .. }));
    }

    /// Council-adjudicated FIX 5: pins the actual comparison primitive
    /// `consume()` uses on the secret token bytes
    /// (`subtle::ConstantTimeEq::ct_eq`, never `==`). A true timing-safety
    /// property needs dedicated side-channel measurement infrastructure
    /// this suite does not have (the phase plan's own text: "at minimum
    /// asserting the primitive is used") — this instead proves the
    /// primitive itself returns identical (non-early-exit-observable)
    /// boolean results regardless of where in the byte string a mismatch
    /// occurs, and that equal inputs of equal length compare equal.
    #[test]
    fn consume_uses_constant_time_equality_not_eq() {
        let stored: &[u8] = b"AAAAAAAAAAAAAAAAAAAA";
        let mismatch_at_first_byte: &[u8] = b"BAAAAAAAAAAAAAAAAAAA";
        let mismatch_at_last_byte: &[u8] = b"AAAAAAAAAAAAAAAAAAAB";
        assert!(bool::from(stored.ct_eq(stored)));
        assert!(!bool::from(stored.ct_eq(mismatch_at_first_byte)));
        assert!(!bool::from(stored.ct_eq(mismatch_at_last_byte)));
    }

    /// Council-adjudicated FIX 4: `sweep_expired` reaps only rows whose
    /// `created_at` predates the TTL — a token issued moments ago survives
    /// a short-TTL sweep; a token backdated (via a direct UPDATE, standing
    /// in for one that has genuinely aged past the TTL) is reaped.
    #[tokio::test]
    async fn sweep_expired_reaps_only_rows_older_than_max_age() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(db) = test_db().await else {
            return;
        };
        let store = PostgresIsrTokenStore::new(db.clone());
        store.ensure_schema().await.expect("ensure isr schema");

        let fresh_sm_id = format!("sm-{}", uuid::Uuid::new_v4());
        let stale_sm_id = format!("sm-{}", uuid::Uuid::new_v4());
        let me = node_identity();
        let fresh_grant = claim_sm_session(&db, &fresh_sm_id, &me).await;
        let stale_grant = claim_sm_session(&db, &stale_sm_id, &me).await;
        store
            .issue(&fresh_sm_id, "PLAIN", &fresh_grant)
            .await
            .expect("issue fresh token");
        store
            .issue(&stale_sm_id, "PLAIN", &stale_grant)
            .await
            .expect("issue stale token");

        // Backdate the "stale" row directly — standing in for a token that
        // has genuinely aged past the TTL without needing the test to
        // actually sleep.
        let conn = db.guard().await.expect("guard");
        conn.execute(
            "UPDATE clustering_isr_tokens SET created_at = now() - interval '1 day' \
             WHERE sm_id = ?",
            crate::db_params![stale_sm_id.clone()],
        )
        .await
        .expect("backdate stale row");

        let deleted = store
            .sweep_expired(std::time::Duration::from_secs(3600))
            .await
            .expect("sweep_expired");
        assert_eq!(deleted, 1, "exactly the backdated row must be reaped");

        // The fresh row survives; the stale one is gone (a subsequent
        // consume for it finds no row at all).
        let stale_outcome = store
            .consume(&stale_sm_id, b"anything", "PLAIN", &me, stale_grant.epoch)
            .await
            .expect("consume stale");
        assert_eq!(stale_outcome, IsrConsumeOutcome::NoSuchToken);
        let fresh_outcome = store
            .consume(
                &fresh_sm_id,
                b"wrong-token-but-row-must-exist",
                "PLAIN",
                &me,
                fresh_grant.epoch,
            )
            .await
            .expect("consume fresh");
        assert_eq!(
            fresh_outcome,
            IsrConsumeOutcome::Mismatched,
            "fresh row must still exist (Mismatched, not NoSuchToken) — only wrong-token, \
             not swept"
        );
    }

    #[tokio::test]
    async fn consume_fails_fencing_when_the_epoch_does_not_match() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(db) = test_db().await else {
            return;
        };
        let store = PostgresIsrTokenStore::new(db.clone());
        store.ensure_schema().await.expect("ensure isr schema");
        let me = node_identity();
        let sm_id = format!("sm-{}", uuid::Uuid::new_v4());
        let grant = claim_sm_session(&db, &sm_id, &me).await;

        let issued = store.issue(&sm_id, "PLAIN", &grant).await.expect("issue");
        let wrong_epoch = ClaimEpoch(grant.epoch.0 + 1);
        let outcome = store
            .consume(&sm_id, issued.token.as_bytes(), "PLAIN", &me, wrong_epoch)
            .await;
        assert!(matches!(outcome, Err(IsrTokenStoreError::NotOwner)));

        // Fencing failure must not have touched the token row.
        let still_valid = store
            .consume(&sm_id, issued.token.as_bytes(), "PLAIN", &me, grant.epoch)
            .await
            .expect("consume");
        assert!(matches!(still_valid, IsrConsumeOutcome::Matched { .. }));
    }

    #[tokio::test]
    async fn issue_and_consume_reject_a_lapsed_owner_before_committed_expiry() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(db) = test_db().await else {
            return;
        };
        let store = PostgresIsrTokenStore::new(db.clone());
        store.ensure_schema().await.expect("ensure isr schema");
        let me = node_identity();
        let sm_id = format!("sm-lapsed-{}", uuid::Uuid::new_v4());
        let grant = claim_sm_session(&db, &sm_id, &me).await;
        let issued = store.issue(&sm_id, "PLAIN", &grant).await.expect("issue");

        let conn = db.guard().await.expect("guard");
        conn.execute(
            "UPDATE clustering_nodes SET heartbeat = now() - interval '1 hour', expired = false \
             WHERE node_id = ? AND node_epoch = ?",
            crate::db_params![me.node_id.clone(), me.node_epoch.clone()],
        )
        .await
        .expect("lapse owner heartbeat");
        drop(conn);

        assert!(matches!(
            store.issue(&sm_id, "PLAIN", &grant).await,
            Err(IsrTokenStoreError::NotOwner)
        ));
        assert!(matches!(
            store
                .consume(&sm_id, issued.token.as_bytes(), "PLAIN", &me, grant.epoch)
                .await,
            Err(IsrTokenStoreError::NotOwner)
        ));

        let conn = db.guard().await.expect("guard");
        let mut rows = conn
            .query(
                "SELECT token FROM clustering_isr_tokens WHERE sm_id = ?",
                crate::db_params![sm_id],
            )
            .await
            .expect("query untouched token");
        assert_eq!(
            rows.next()
                .await
                .expect("row read")
                .expect("token remains")
                .get::<String>(0)
                .expect("token"),
            issued.token
        );
    }

    #[tokio::test]
    async fn issue_revalidates_wall_clock_after_node_lock_wait() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(db) = test_db().await else {
            return;
        };
        let store = PostgresIsrTokenStore::new(db.clone());
        store.ensure_schema().await.unwrap();
        let me = node_identity();
        let sm_id = format!("sm-clock-{}", uuid::Uuid::new_v4());
        let grant = claim_sm_session(&db, &sm_id, &me).await;
        db.guard()
            .await
            .unwrap()
            .execute(
                "UPDATE clustering_nodes SET heartbeat = clock_timestamp(), lease_ttl_ms = 50 WHERE node_id = ? AND node_epoch = ?",
                crate::db_params![me.node_id.clone(), me.node_epoch.clone()],
            )
            .await
            .unwrap();
        let mut blocker = db.begin().await.unwrap();
        let mut rows = blocker
            .query(
                "SELECT 1 FROM clustering_nodes WHERE node_id = ? AND node_epoch = ? FOR UPDATE",
                crate::db_params![me.node_id.clone(), me.node_epoch.clone()],
            )
            .await
            .unwrap();
        assert!(rows.next().await.unwrap().is_some());
        drop(rows);
        let task_store = PostgresIsrTokenStore::new(db.clone());
        let task = tokio::spawn(async move { task_store.issue(&sm_id, "PLAIN", &grant).await });
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(
            !task.is_finished(),
            "ISR issue must wait on the held node row"
        );
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        blocker.commit().await.unwrap();
        assert!(matches!(
            task.await.unwrap(),
            Err(IsrTokenStoreError::NotOwner)
        ));
    }

    #[tokio::test]
    async fn issue_stamps_created_at_after_token_row_lock_wait() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(db) = test_db().await else {
            return;
        };
        let store = PostgresIsrTokenStore::new(db.clone());
        store.ensure_schema().await.unwrap();
        let me = node_identity();
        let sm_id = format!("sm-created-at-{}", uuid::Uuid::new_v4());
        let grant = claim_sm_session(&db, &sm_id, &me).await;
        store.issue(&sm_id, "PLAIN", &grant).await.unwrap();

        let mut blocker = db.begin().await.unwrap();
        let mut rows = blocker
            .query(
                "SELECT 1 FROM clustering_isr_tokens WHERE sm_id = ? FOR UPDATE",
                crate::db_params![sm_id.clone()],
            )
            .await
            .unwrap();
        assert!(rows.next().await.unwrap().is_some());
        drop(rows);

        let task_store = PostgresIsrTokenStore::new(db.clone());
        let task_sm_id = sm_id.clone();
        let task =
            tokio::spawn(async move { task_store.issue(&task_sm_id, "PLAIN", &grant).await });
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        assert!(!task.is_finished(), "issue must wait on the token row");
        tokio::time::sleep(std::time::Duration::from_millis(360)).await;
        blocker.commit().await.unwrap();
        task.await.unwrap().unwrap();

        let mut rows = db
            .guard()
            .await
            .unwrap()
            .query(
                "SELECT (EXTRACT(EPOCH FROM clock_timestamp() - created_at) * 1000)::double precision FROM clustering_isr_tokens WHERE sm_id = ?",
                crate::db_params![sm_id],
            )
            .await
            .unwrap();
        let age_ms = rows.next().await.unwrap().unwrap().get::<f64>(0).unwrap();
        assert!(
            age_ms < 200.0,
            "created_at must start after the 400ms token-row wait, got {age_ms}ms"
        );
    }

    #[tokio::test]
    async fn issue_rejects_the_old_grant_after_same_node_id_recovery() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(db) = test_db().await else {
            return;
        };
        let store = PostgresIsrTokenStore::new(db.clone());
        store.ensure_schema().await.expect("ensure isr schema");
        let old = node_identity();
        let sm_id = format!("sm-issue-same-node-{}", uuid::Uuid::new_v4());
        let grant = claim_sm_session(&db, &sm_id, &old).await;
        let original = store
            .issue(&sm_id, "PLAIN", &grant)
            .await
            .expect("initial issue");

        // Rotate only the node-liveness row. The claim intentionally still
        // names the old incarnation, so a fence that checked claim columns
        // alone would incorrectly authorize this stale issuer.
        let recovered = NodeIdentity::new(old.node_id.clone(), uuid::Uuid::new_v4().to_string());
        let claim_store = PostgresClaimStore::new(db.clone());
        claim_store
            .register_draining_with_peer_id(
                &old,
                &recovered,
                None,
                None,
                std::time::Duration::from_secs(30),
            )
            .await
            .expect("rotate node row without moving claim");

        let stale_issue = store.issue(&sm_id, "PLAIN", &grant).await;
        assert!(
            matches!(stale_issue, Err(IsrTokenStoreError::NotOwner)),
            "the deposed node incarnation must not rotate the token: {stale_issue:?}"
        );

        let recovered_epoch = claim_store
            .reclaim_after_self_fence(
                &grant.entity,
                grant.epoch,
                &old,
                &recovered,
                std::time::Duration::from_secs(30),
            )
            .await
            .expect("reclaim exact old claim");
        assert!(claim_store
            .activate(&recovered, std::time::Duration::from_secs(30))
            .await
            .expect("activate recovered incarnation"));
        let current = store
            .consume(
                &sm_id,
                original.token.as_bytes(),
                "PLAIN",
                &recovered,
                recovered_epoch,
            )
            .await
            .expect("current incarnation consumes original token");
        assert!(
            matches!(current, IsrConsumeOutcome::Matched { .. }),
            "stale issuance failure must leave the original token untouched"
        );
    }

    #[tokio::test]
    async fn consume_rejects_the_old_epoch_after_same_node_id_recovery() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(db) = test_db().await else {
            return;
        };
        let store = PostgresIsrTokenStore::new(db.clone());
        store.ensure_schema().await.expect("ensure isr schema");
        let old = node_identity();
        let sm_id = format!("sm-same-node-{}", uuid::Uuid::new_v4());
        let grant = claim_sm_session(&db, &sm_id, &old).await;
        let issued = store.issue(&sm_id, "PLAIN", &grant).await.expect("issue");

        let recovered = NodeIdentity::new(old.node_id.clone(), uuid::Uuid::new_v4().to_string());
        let claim_store = PostgresClaimStore::new(db.clone());
        claim_store
            .register_draining_with_peer_id(
                &old,
                &recovered,
                None,
                None,
                std::time::Duration::from_secs(30),
            )
            .await
            .expect("rotate node row without moving claim");

        let stale = store
            .consume(&sm_id, issued.token.as_bytes(), "PLAIN", &old, grant.epoch)
            .await;
        assert!(
            matches!(stale, Err(IsrTokenStoreError::NotOwner)),
            "the old node epoch must fail before consuming the token: {stale:?}"
        );

        let recovered_epoch = claim_store
            .reclaim_after_self_fence(
                &grant.entity,
                grant.epoch,
                &old,
                &recovered,
                std::time::Duration::from_secs(30),
            )
            .await
            .expect("reclaim exact old claim");
        assert!(claim_store
            .activate(&recovered, std::time::Duration::from_secs(30))
            .await
            .expect("activate recovered incarnation"));

        let current = store
            .consume(
                &sm_id,
                issued.token.as_bytes(),
                "PLAIN",
                &recovered,
                recovered_epoch,
            )
            .await
            .expect("current incarnation consumes untouched token");
        assert!(matches!(current, IsrConsumeOutcome::Matched { .. }));
    }

    /// Council-adjudicated FIX 1: strengthens the pre-existing exactly-once
    /// assertion with the invariant the original test missed — the
    /// WINNER'S rotated token must survive the race (not be
    /// deleted-out-from-under-it by the loser) AND must still be
    /// consumable afterward, and the LOSER's outcome must be the
    /// non-destructive `NoSuchToken` (FIX 3), never a `Mismatched` that
    /// would tell `isr_resume.rs` to destroy the winner's just-resumed
    /// session.
    ///
    /// **Deterministic, not scheduling-dependent** (implementation-time
    /// finding): spawning two ordinary `consume()` calls with
    /// `tokio::spawn` alone does not reliably produce genuine row-lock
    /// contention — under a busy test suite (many concurrent OS threads
    /// competing for CPU), one task's entire transaction can complete
    /// before the other's very first query ever reaches Postgres, which
    /// is an ordinary *sequential* (not racing) outcome that legitimately
    /// returns `Mismatched` for the second call, not the phantom-delete
    /// race this fix targets — observed directly while writing this test
    /// (flaky between `NoSuchToken` and `Mismatched` depending on system
    /// load). This test instead manually holds the winning transaction's
    /// row lock open (bypassing `consume()`'s own transaction for this one
    /// side only, replicating exactly what its `Matched` path does) for a
    /// fixed window, GUARANTEEING the second (real `consume()`) call
    /// genuinely blocks on it before the first commits — proving the exact
    /// scenario FIX 1 fixes, on every run, regardless of system load.
    #[tokio::test]
    async fn concurrent_double_consume_exactly_one_wins() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(db) = test_db().await else {
            return;
        };
        let store = std::sync::Arc::new(PostgresIsrTokenStore::new(db.clone()));
        store.ensure_schema().await.expect("ensure isr schema");
        let me = node_identity();
        let sm_id = format!("sm-{}", uuid::Uuid::new_v4());
        let grant = claim_sm_session(&db, &sm_id, &me).await;
        let issued = store.issue(&sm_id, "PLAIN", &grant).await.expect("issue");
        let epoch = grant.epoch;

        // Manually hold the token row's lock open — standing in for
        // `consume()`'s own winning transaction, but under this test's
        // control, so the losing `consume()` call spawned below is
        // GUARANTEED to block on it rather than depending on scheduling
        // luck.
        let mut winner_tx = db.begin().await.expect("begin winner tx");
        let mut locked_rows = winner_tx
            .query(
                "SELECT token, mechanism FROM clustering_isr_tokens WHERE sm_id = ? FOR UPDATE",
                crate::db_params![sm_id.clone()],
            )
            .await
            .expect("lock the token row");
        let locked_row = locked_rows
            .next()
            .await
            .expect("row read")
            .expect("token row exists");
        let locked_token: String = locked_row.get(0).expect("token column");
        assert_eq!(
            locked_token, issued.token,
            "sanity: this test locked the freshly-issued row"
        );
        drop(locked_rows);

        // Spawn the loser: a real `consume()` call on the SAME sm_id,
        // presenting the SAME (from its own perspective, still valid)
        // token. Its own `SELECT ... FOR UPDATE` genuinely blocks behind
        // `winner_tx`'s still-open lock above.
        let store_loser = store.clone();
        let (sm_id_loser, me_loser, token_loser) =
            (sm_id.clone(), me.clone(), issued.token.clone());
        let loser_handle = tokio::spawn(async move {
            store_loser
                .consume(
                    &sm_id_loser,
                    token_loser.as_bytes(),
                    "PLAIN",
                    &me_loser,
                    epoch,
                )
                .await
        });

        // Give the loser's request ample time to actually reach Postgres
        // and enter the lock wait queue before this test commits — 200ms
        // is an enormous margin for a local round trip, even under a
        // heavily loaded test suite.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Complete the winning transaction exactly as `consume()`'s own
        // `Matched` path would: delete-then-insert-then-commit, all in one
        // transaction (minor fix 21's atomicity rule, preserved here).
        winner_tx
            .execute(
                "DELETE FROM clustering_isr_tokens WHERE sm_id = ?",
                crate::db_params![sm_id.clone()],
            )
            .await
            .expect("winner delete");
        let rotated_token = generate_isr_token();
        winner_tx
            .execute(
                "INSERT INTO clustering_isr_tokens (sm_id, token, mechanism, created_at) \
                 VALUES (?, ?, ?, now())",
                crate::db_params![sm_id.clone(), rotated_token.clone(), "PLAIN".to_string()],
            )
            .await
            .expect("winner rotate insert");
        winner_tx.commit().await.expect("winner commit");

        // The loser was genuinely blocked the entire time above — its
        // outcome now reflects real Postgres row-lock semantics, not
        // scheduling luck.
        let loser = loser_handle.await.expect("join loser");

        // FIX 1/FIX 3: the loser's outcome must be the non-destructive
        // `NoSuchToken` — its blocked `FOR UPDATE` read observed the
        // winner's row already deleted (Postgres's delete-not-update
        // row-lock semantics: a transaction blocked on a row that gets
        // DELETED — never UPDATED — ignores it once unblocked, even
        // though a new row sharing the same primary key now exists) —
        // never a destructive `Mismatched` that would tell
        // `isr_resume.rs` to destroy the winner's just-resumed session.
        assert!(
            matches!(loser, Ok(IsrConsumeOutcome::NoSuchToken)),
            "the losing consumer must observe no row at all, never a destructive Mismatched; \
             got {loser:?}"
        );

        // Token survival + winner-can-still-resume invariant (FIX 1): the
        // winner's rotated token was NOT destroyed by the loser's
        // (correctly no-op) attempt — a subsequent consume with it still
        // succeeds.
        let survives = store
            .consume(&sm_id, rotated_token.as_bytes(), "PLAIN", &me, epoch)
            .await
            .expect("consume with the winner's rotated token");
        assert!(
            matches!(survives, IsrConsumeOutcome::Matched { .. }),
            "the winner's rotated token must still exist and be consumable after the race; \
             got {survives:?}"
        );
    }
}
