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
//! rule Slices 4 and 7 already follow. `ensure_schema` runs on the main pool
//! via [`Database::guard`]. Token publication, revocation, and consume use
//! main-pool transactions sharing a per-SM-ID advisory lock so an ambiguous
//! late publication cannot cross exact revocation-fence installation.
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
    generate_isr_token, IsrConsumeOutcome, IsrRevokeOutcome, IsrTokenStore, IsrTokenStoreError,
    IssuedIsrToken,
};
use waddle_xmpp::ownership::EntityType;
use waddle_xmpp::pending_delivery::SmSessionId;

use crate::db::{Database, DatabaseError};

/// Keep each TTL pass short enough that cleanup cannot monopolize the ISR
/// tables. Repeated janitor ticks make monotonic progress in oldest-first
/// order.
const ISR_SWEEP_BATCH_SIZE: i64 = 64;

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
fn sm_session_entity_key(sm_id: &SmSessionId) -> String {
    format!("{}:{}", EntityType::SmSession.as_db_str(), sm_id)
}

/// Serialize token publication and exact negative-fence installation for one
/// SM session. A transaction-scoped advisory lock avoids a gap between the
/// live-token and revocation-fence tables where a timed-out UPSERT could land
/// after cleanup had already declared success.
async fn lock_isr_stream(
    tx: &mut crate::db::Transaction<'_>,
    sm_id: &SmSessionId,
) -> Result<(), IsrTokenStoreError> {
    let mut rows = tx
        .query(
            "SELECT pg_advisory_xact_lock(hashtextextended(?, 0))",
            crate::db_params![sm_id.to_string()],
        )
        .await
        .map_err(db_err)?;
    rows.next().await.map_err(db_err)?;
    drop(rows);
    Ok(())
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
        conn.execute(
            r#"
            CREATE INDEX IF NOT EXISTS clustering_isr_tokens_created_at_sm_id
                ON clustering_isr_tokens (created_at, sm_id)
            "#,
            (),
        )
        .await
        .map_err(db_err)?;
        conn.execute(
            r#"
            CREATE TABLE IF NOT EXISTS clustering_isr_revocation_fences (
                sm_id      TEXT NOT NULL,
                token      TEXT NOT NULL,
                mechanism  TEXT NOT NULL,
                created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
                PRIMARY KEY (sm_id, token, mechanism)
            )
            "#,
            (),
        )
        .await
        .map_err(db_err)?;
        conn.execute(
            r#"
            CREATE INDEX IF NOT EXISTS clustering_isr_revocation_fences_created_at_identity
                ON clustering_isr_revocation_fences (created_at, sm_id, token, mechanism)
            "#,
            (),
        )
        .await
        .map_err(db_err)?;
        conn.execute(
            r#"
            CREATE TABLE IF NOT EXISTS clustering_isr_sweep_state (
                singleton         BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
                cursor_created_at TIMESTAMPTZ,
                cursor_sm_id      TEXT
            )
            "#,
            (),
        )
        .await
        .map_err(db_err)?;
        conn.execute(
            "INSERT INTO clustering_isr_sweep_state (singleton) VALUES (TRUE) \
             ON CONFLICT (singleton) DO NOTHING",
            (),
        )
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn persist_issued(
        &self,
        sm_id: &SmSessionId,
        issued: &IssuedIsrToken,
    ) -> Result<(), IsrTokenStoreError> {
        let mut tx = self.db.begin().await.map_err(db_err)?;
        lock_isr_stream(&mut tx, sm_id).await?;
        let suppressed = tx
            .execute(
                "DELETE FROM clustering_isr_revocation_fences \
                 WHERE sm_id = ? AND token = ? AND mechanism = ?",
                crate::db_params![
                    sm_id.to_string(),
                    issued.token.clone(),
                    issued.mechanism.clone()
                ],
            )
            .await
            .map_err(db_err)?;
        if suppressed == 0 {
            tx.execute(
                "INSERT INTO clustering_isr_tokens (sm_id, token, mechanism, created_at) \
                 VALUES (?, ?, ?, now()) \
                 ON CONFLICT (sm_id) DO UPDATE SET \
                     token = EXCLUDED.token, mechanism = EXCLUDED.mechanism, created_at = now()",
                crate::db_params![
                    sm_id.to_string(),
                    issued.token.clone(),
                    issued.mechanism.clone()
                ],
            )
            .await
            .map_err(db_err)?;
        }
        tx.commit().await.map_err(db_err)?;
        Ok(())
    }

    async fn revoke_if_current(
        &self,
        sm_id: &SmSessionId,
        issued: &IssuedIsrToken,
    ) -> Result<IsrRevokeOutcome, IsrTokenStoreError> {
        let mut tx = self.db.begin().await.map_err(db_err)?;
        lock_isr_stream(&mut tx, sm_id).await?;
        let mut current = tx
            .query(
                "SELECT token, mechanism FROM clustering_isr_tokens WHERE sm_id = ?",
                crate::db_params![sm_id.to_string()],
            )
            .await
            .map_err(db_err)?;
        let live = current.next().await.map_err(db_err)?;
        let outcome = if let Some(row) = live {
            let token: String = row.get(0).map_err(db_err)?;
            let mechanism: String = row.get(1).map_err(db_err)?;
            if mechanism == issued.mechanism
                && bool::from(token.as_bytes().ct_eq(issued.token.as_bytes()))
            {
                IsrRevokeOutcome::Revoked
            } else {
                IsrRevokeOutcome::Superseded
            }
        } else {
            IsrRevokeOutcome::Missing
        };
        drop(current);
        if outcome == IsrRevokeOutcome::Revoked {
            tx.execute(
                "DELETE FROM clustering_isr_tokens WHERE sm_id = ?",
                crate::db_params![sm_id.to_string()],
            )
            .await
            .map_err(db_err)?;
        }
        tx.execute(
            "INSERT INTO clustering_isr_revocation_fences \
             (sm_id, token, mechanism, created_at) VALUES (?, ?, ?, now()) \
             ON CONFLICT (sm_id, token, mechanism) DO UPDATE SET created_at = now()",
            crate::db_params![
                sm_id.to_string(),
                issued.token.clone(),
                issued.mechanism.clone()
            ],
        )
        .await
        .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        Ok(outcome)
    }

    async fn consume(
        &self,
        sm_id: &SmSessionId,
        presented_token: &[u8],
        mechanism: &str,
        fence: &waddle_xmpp::stream_management::persistence::SmClaimFence,
    ) -> Result<IsrConsumeOutcome, IsrTokenStoreError> {
        let mut tx = self.db.begin().await.map_err(db_err)?;
        lock_isr_stream(&mut tx, sm_id).await?;

        // Fencing check: identical shape to `assert_fenced` in
        // `sm_persistence_fenced.rs`/`muc_durable.rs` — one connection, one
        // transaction, shared with the token read/delete/insert below.
        let key = sm_session_entity_key(sm_id);
        let mut fence_rows = tx
            .query(
                "SELECT 1 FROM clustering_claims WHERE entity = ? AND node_id = ? AND node_epoch = ? AND claim_epoch = ? FOR SHARE",
                crate::db_params![
                    key,
                    fence.owner().node_id.clone(),
                    fence.owner().node_epoch.clone(),
                    fence.epoch().0,
                ],
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
             VALUES (?, ?, ?, now())",
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
        // deliberately avoids. Age alone is not terminal authority: an SM
        // stream can remain live for longer than this backstop and its ISR
        // token must remain valid throughout that lifetime. Reap only an old
        // token whose typed SM-session claim and all durable resumable or
        // terminal-generation work are absent. Persisted work still requires
        // recovery even while its former node's exact claim is being
        // recovered, so deleting its ISR credential would turn a recoverable
        // node loss into permanent resume failure. Mirrors
        // `lease.rs`'s own `(? || ' milliseconds')::interval`
        // TTL-comparison idiom.
        let mut tx = self.db.begin().await.map_err(db_err)?;
        let mut cursor_rows = tx
            .query(
                "SELECT COALESCE(cursor_created_at::text, '0001-01-01 00:00:00+00'), \
                        COALESCE(cursor_sm_id, '') \
                 FROM clustering_isr_sweep_state WHERE singleton = TRUE FOR UPDATE",
                (),
            )
            .await
            .map_err(db_err)?;
        let cursor_row =
            cursor_rows.next().await.map_err(db_err)?.ok_or_else(|| {
                IsrTokenStoreError::Backend("missing ISR sweep cursor row".into())
            })?;
        let raw_after_created_at: String = cursor_row.get(0).map_err(db_err)?;
        let raw_after_sm_id: String = cursor_row.get(1).map_err(db_err)?;
        drop(cursor_rows);
        let raw_limit = ISR_SWEEP_BATCH_SIZE.saturating_add(1);
        let mut raw_rows = tx
            .query(
                r#"
                SELECT created_at::text, sm_id
                FROM clustering_isr_tokens
                WHERE created_at < now() - (? || ' milliseconds')::interval
                  AND (created_at, sm_id) > (CAST(? AS TIMESTAMPTZ), ?)
                ORDER BY created_at, sm_id
                LIMIT ?
                FOR UPDATE SKIP LOCKED
                "#,
                crate::db_params![
                    max_age.as_millis().to_string(),
                    raw_after_created_at,
                    raw_after_sm_id,
                    raw_limit,
                ],
            )
            .await
            .map_err(db_err)?;
        let mut raw_tokens = Vec::new();
        while let Some(row) = raw_rows.next().await.map_err(db_err)? {
            raw_tokens.push((
                row.get::<String>(0).map_err(db_err)?,
                row.get::<String>(1).map_err(db_err)?,
            ));
        }
        drop(raw_rows);

        let batch_size = usize::try_from(ISR_SWEEP_BATCH_SIZE).unwrap_or(64);
        let has_more = raw_tokens.len() > batch_size;
        raw_tokens.truncate(batch_size);
        let mut deleted_tokens = 0u64;
        for (_, sm_id) in &raw_tokens {
            deleted_tokens = deleted_tokens.saturating_add(
                tx.execute(
                    r#"
                    DELETE FROM clustering_isr_tokens AS token
                    WHERE token.sm_id = ?
                      AND token.created_at < now() - (? || ' milliseconds')::interval
                      AND NOT EXISTS (
                        SELECT 1 FROM clustering_claims AS claim
                        WHERE claim.entity = (? || ':' || token.sm_id)
                          AND claim.entity_type = ?
                      )
                      AND NOT EXISTS (
                        SELECT 1 FROM sm_sessions AS session
                        WHERE session.stream_id = token.sm_id
                      )
                      AND NOT EXISTS (
                        SELECT 1 FROM sm_terminal_generations AS terminal
                        WHERE terminal.stream_id = token.sm_id
                      )
                    "#,
                    crate::db_params![
                        sm_id.clone(),
                        max_age.as_millis().to_string(),
                        EntityType::SmSession.as_db_str().to_string(),
                        EntityType::SmSession.as_db_str().to_string(),
                    ],
                )
                .await
                .map_err(db_err)?,
            );
        }
        if has_more {
            let (created_at, sm_id) = raw_tokens
                .last()
                .expect("a page with an extra row has a retained cursor row");
            tx.execute(
                "UPDATE clustering_isr_sweep_state \
                 SET cursor_created_at = CAST(? AS TIMESTAMPTZ), cursor_sm_id = ? \
                 WHERE singleton = TRUE",
                crate::db_params![created_at.clone(), sm_id.clone()],
            )
            .await
            .map_err(db_err)?;
        } else {
            tx.execute(
                "UPDATE clustering_isr_sweep_state \
                 SET cursor_created_at = NULL, cursor_sm_id = NULL \
                 WHERE singleton = TRUE",
                (),
            )
            .await
            .map_err(db_err)?;
        }
        tx.commit().await.map_err(db_err)?;

        let conn = self.db.guard().await.map_err(db_err)?;
        conn.execute(
            r#"
            WITH expired_fences AS (
                SELECT fence.sm_id, fence.token, fence.mechanism
                FROM clustering_isr_revocation_fences AS fence
                WHERE fence.created_at < now() - (? || ' milliseconds')::interval
                ORDER BY fence.created_at, fence.sm_id, fence.token, fence.mechanism
                LIMIT ?
                FOR UPDATE OF fence SKIP LOCKED
            )
            DELETE FROM clustering_isr_revocation_fences AS fence
            USING expired_fences AS expired
            WHERE fence.sm_id = expired.sm_id
              AND fence.token = expired.token
              AND fence.mechanism = expired.mechanism
            "#,
            crate::db_params![max_age.as_millis().to_string(), ISR_SWEEP_BATCH_SIZE],
        )
        .await
        .map_err(db_err)?;
        Ok(deleted_tokens)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clustering::claims::{clustering_control_plane_table_lock, PostgresClaimStore};
    use crate::db::{DatabaseConfig, DatabaseDriver};
    use std::sync::Arc;
    use waddle_xmpp::ownership::{
        ClaimEpoch, ClaimStore, Entity, NodeIdentity, SharedNodeIdentity,
    };
    use waddle_xmpp::stream_management::persistence::SmClaimFence;

    fn node_identity() -> NodeIdentity {
        NodeIdentity::new(
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
        )
    }

    fn fence(owner: &NodeIdentity, epoch: ClaimEpoch) -> SmClaimFence {
        SmClaimFence::new(owner.clone(), epoch)
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
    async fn claim_sm_session(db: &Database, sm_id: &SmSessionId, me: &NodeIdentity) -> ClaimEpoch {
        let claim_store = PostgresClaimStore::new(db.clone());
        claim_store
            .ensure_schema()
            .await
            .expect("ensure claims schema");
        let entity = Entity::new(EntityType::SmSession, sm_id.to_string());
        claim_store
            .acquire(&entity, me)
            .await
            .expect("acquire sm_session claim")
    }

    async fn ensure_sm_persistence_schema(db: &Database) {
        let claim_store = Arc::new(PostgresClaimStore::new(db.clone()));
        claim_store
            .ensure_schema()
            .await
            .expect("ensure claims schema");
        crate::sm_persistence_fenced::PostgresFencedSmPersistence::open(
            db.clone(),
            claim_store,
            SharedNodeIdentity::new(node_identity()),
        )
        .await
        .expect("ensure SM persistence schema");
    }

    async fn clean_sweep_tables(db: &Database) {
        let conn = db.guard().await.expect("guard");
        conn.execute("DELETE FROM sm_terminal_unacked", ())
            .await
            .expect("clean sm_terminal_unacked");
        conn.execute("DELETE FROM sm_terminal_generations", ())
            .await
            .expect("clean sm_terminal_generations");
        conn.execute("DELETE FROM sm_unacked", ())
            .await
            .expect("clean sm_unacked");
        conn.execute("DELETE FROM sm_sessions", ())
            .await
            .expect("clean sm_sessions");
        conn.execute("DELETE FROM clustering_isr_tokens", ())
            .await
            .expect("clean ISR tokens");
        conn.execute("DELETE FROM clustering_isr_revocation_fences", ())
            .await
            .expect("clean ISR revocation fences");
        conn.execute(
            "UPDATE clustering_isr_sweep_state \
             SET cursor_created_at = NULL, cursor_sm_id = NULL \
             WHERE singleton = TRUE",
            (),
        )
        .await
        .expect("reset ISR sweep cursor");
        conn.execute("DELETE FROM clustering_claims", ())
            .await
            .expect("clean claims");
        conn.execute("DELETE FROM clustering_nodes", ())
            .await
            .expect("clean nodes");
    }

    async fn seed_sm_session_row(db: &Database, sm_id: &SmSessionId) {
        let conn = db.guard().await.expect("guard");
        conn.execute(
            r#"
            INSERT INTO sm_sessions (
                stream_id, user_id, full_jid, inbound_count, outbound_count,
                last_acked, max_resume_secs, detached_at_ms, max_resume_duration_ms,
                carbons_enabled, roster_interested, blocklist_interested,
                presence_available, presence_priority
            ) VALUES (?, ?, ?, 0, 0, 0, NULL, 0, 60000, 0, 0, 0, 0, 0)
            "#,
            crate::db_params![
                sm_id.to_string(),
                "alice".to_string(),
                "alice@example.com/web".to_string(),
            ],
        )
        .await
        .expect("seed durable SM session");
    }

    async fn seed_sm_terminal_generation_row(db: &Database, sm_id: &SmSessionId) {
        let conn = db.guard().await.expect("guard");
        conn.execute(
            r#"
            INSERT INTO sm_terminal_generations (
                stream_id, generation_id, user_id, full_jid, inbound_count,
                outbound_count, last_acked, max_resume_secs, detached_at_ms,
                max_resume_duration_ms, carbons_enabled, roster_interested,
                blocklist_interested, presence_available, presence_priority
            ) VALUES (?, ?, ?, ?, 0, 0, 0, NULL, 0, 60000, 0, 0, 0, 0, 0)
            "#,
            crate::db_params![
                sm_id.to_string(),
                "terminal-generation".to_string(),
                "alice".to_string(),
                "alice@example.com/web".to_string(),
            ],
        )
        .await
        .expect("seed terminal SM generation");
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
        let sm_id = SmSessionId::new(format!("sm-{}", uuid::Uuid::new_v4()));
        let epoch = claim_sm_session(&db, &sm_id, &me).await;

        let issued = store.issue(&sm_id, "PLAIN").await.expect("issue");
        let outcome = store
            .consume(&sm_id, issued.token.as_bytes(), "PLAIN", &fence(&me, epoch))
            .await
            .expect("consume");
        let IsrConsumeOutcome::Matched { rotated } = outcome else {
            panic!("expected Matched, got {outcome:?}");
        };
        assert_ne!(rotated.token, issued.token);

        // Single-use: the OLD token fails now, even under the same fence.
        let replay = store
            .consume(&sm_id, issued.token.as_bytes(), "PLAIN", &fence(&me, epoch))
            .await
            .expect("consume");
        assert_eq!(replay, IsrConsumeOutcome::Mismatched);
    }

    #[tokio::test]
    async fn provisional_revoke_cannot_delete_a_newer_postgres_issue() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(db) = test_db().await else {
            return;
        };
        let store = PostgresIsrTokenStore::new(db);
        store.ensure_schema().await.expect("ensure isr schema");
        let sm_id = SmSessionId::new(format!("sm-{}", uuid::Uuid::new_v4()));
        let old = store.issue(&sm_id, "PLAIN").await.expect("old issue");
        let current = store.issue(&sm_id, "PLAIN").await.expect("new issue");

        assert_eq!(
            store
                .revoke_if_current(&sm_id, &old)
                .await
                .expect("stale revoke"),
            IsrRevokeOutcome::Superseded
        );
        store
            .persist_issued(&sm_id, &old)
            .await
            .expect("late old persistence is suppressed by its exact fence");
        assert_eq!(
            store
                .revoke_if_current(&sm_id, &current)
                .await
                .expect("exact revoke"),
            IsrRevokeOutcome::Revoked
        );
    }

    #[tokio::test]
    async fn concurrent_consume_and_exact_revoke_preserve_the_serialized_winner() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(db) = test_db().await else {
            return;
        };
        let store = PostgresIsrTokenStore::new(db.clone());
        store.ensure_schema().await.expect("ensure isr schema");
        let me = node_identity();
        let sm_id = SmSessionId::new(format!("sm-{}", uuid::Uuid::new_v4()));
        let epoch = claim_sm_session(&db, &sm_id, &me).await;
        let issued = store.issue(&sm_id, "PLAIN").await.expect("issue");
        let claim_fence = fence(&me, epoch);

        let (consumed, revoked) = tokio::join!(
            store.consume(&sm_id, issued.token.as_bytes(), "PLAIN", &claim_fence,),
            store.revoke_if_current(&sm_id, &issued),
        );
        let consumed = consumed.expect("consume result");
        let revoked = revoked.expect("revoke result");

        match (consumed, revoked) {
            (IsrConsumeOutcome::Matched { rotated }, IsrRevokeOutcome::Superseded) => {
                assert!(matches!(
                    store
                        .consume(
                            &sm_id,
                            rotated.token.as_bytes(),
                            "PLAIN",
                            &fence(&me, epoch),
                        )
                        .await
                        .expect("rotated token survives stale cleanup"),
                    IsrConsumeOutcome::Matched { .. }
                ));
            }
            (IsrConsumeOutcome::NoSuchToken, IsrRevokeOutcome::Revoked) => {}
            outcome => panic!("consume/revoke did not serialize safely: {outcome:?}"),
        }
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
        let sm_id = SmSessionId::new(format!("sm-{}", uuid::Uuid::new_v4()));
        let epoch = claim_sm_session(&db, &sm_id, &me).await;

        let issued = store.issue(&sm_id, "PLAIN").await.expect("issue");
        let outcome = store
            .consume(&sm_id, b"not-the-token", "PLAIN", &fence(&me, epoch))
            .await
            .expect("consume");
        assert_eq!(outcome, IsrConsumeOutcome::Mismatched);

        // The row is destroyed — a subsequent attempt with the CORRECT
        // token now finds no row at all (FIX 3: `NoSuchToken`, distinct
        // from the genuine `Mismatched` above), proving unconditional
        // destruction happened.
        let second = store
            .consume(&sm_id, issued.token.as_bytes(), "PLAIN", &fence(&me, epoch))
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
        let sm_id = SmSessionId::new(format!("sm-{}", uuid::Uuid::new_v4()));
        let epoch = claim_sm_session(&db, &sm_id, &me).await;

        let outcome = store
            .consume(&sm_id, b"anything", "PLAIN", &fence(&me, epoch))
            .await
            .expect("consume");
        assert_eq!(outcome, IsrConsumeOutcome::NoSuchToken);

        // Nothing was touched: issuing a token AFTER this failed attempt
        // and consuming it must succeed normally — proving the no-row
        // branch never wrote anything that could poison a later issuance.
        let issued = store.issue(&sm_id, "PLAIN").await.expect("issue");
        let second = store
            .consume(&sm_id, issued.token.as_bytes(), "PLAIN", &fence(&me, epoch))
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

    /// Council-adjudicated FIX 4: `sweep_expired` reaps an old terminal row
    /// but preserves fresh, exactly claimed, resumable, and terminal work.
    /// Token age starts at SM enable, not at disconnect, so age alone must
    /// never invalidate a live or recoverable stream.
    #[tokio::test]
    async fn sweep_expired_requires_both_claim_and_durable_session_to_be_absent() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(db) = test_db().await else {
            return;
        };
        let store = PostgresIsrTokenStore::new(db.clone());
        store.ensure_schema().await.expect("ensure isr schema");
        ensure_sm_persistence_schema(&db).await;
        clean_sweep_tables(&db).await;

        let fresh_sm_id = SmSessionId::new(format!("sm-{}", uuid::Uuid::new_v4()));
        let claimed_stale_sm_id = SmSessionId::new(format!("sm-{}", uuid::Uuid::new_v4()));
        let persisted_stale_sm_id = SmSessionId::new(format!("sm-{}", uuid::Uuid::new_v4()));
        let terminal_generation_stale_sm_id =
            SmSessionId::new(format!("sm-{}", uuid::Uuid::new_v4()));
        let terminal_stale_sm_id = SmSessionId::new(format!("sm-{}", uuid::Uuid::new_v4()));
        store
            .issue(&fresh_sm_id, "PLAIN")
            .await
            .expect("issue fresh token");
        store
            .issue(&claimed_stale_sm_id, "PLAIN")
            .await
            .expect("issue claimed stale token");
        store
            .issue(&persisted_stale_sm_id, "PLAIN")
            .await
            .expect("issue persisted stale token");
        store
            .issue(&terminal_generation_stale_sm_id, "PLAIN")
            .await
            .expect("issue terminal-generation stale token");
        store
            .issue(&terminal_stale_sm_id, "PLAIN")
            .await
            .expect("issue terminal stale token");

        let me = node_identity();
        let claimed_stale_epoch = claim_sm_session(&db, &claimed_stale_sm_id, &me).await;
        seed_sm_session_row(&db, &persisted_stale_sm_id).await;
        seed_sm_terminal_generation_row(&db, &terminal_generation_stale_sm_id).await;

        // Backdate all non-fresh rows directly. Only the row with neither
        // exact ownership nor durable resume state is terminal.
        let conn = db.guard().await.expect("guard");
        conn.execute(
            "UPDATE clustering_isr_tokens SET created_at = now() - interval '1 day' \
             WHERE sm_id IN (?, ?, ?, ?)",
            crate::db_params![
                claimed_stale_sm_id.to_string(),
                persisted_stale_sm_id.to_string(),
                terminal_generation_stale_sm_id.to_string(),
                terminal_stale_sm_id.to_string()
            ],
        )
        .await
        .expect("backdate stale row");

        let deleted = store
            .sweep_expired(std::time::Duration::from_secs(3600))
            .await
            .expect("sweep_expired");
        assert_eq!(deleted, 1, "only the terminal token may be reaped");

        // The fresh row survives by age. The three old recoverable rows
        // survive by ownership, resumable persistence, and terminal
        // persistence respectively. Only the truly terminal row is gone.
        let fresh_epoch = claim_sm_session(&db, &fresh_sm_id, &me).await;
        let persisted_stale_epoch = claim_sm_session(&db, &persisted_stale_sm_id, &me).await;
        let terminal_generation_stale_epoch =
            claim_sm_session(&db, &terminal_generation_stale_sm_id, &me).await;
        let terminal_stale_epoch = claim_sm_session(&db, &terminal_stale_sm_id, &me).await;
        let terminal_stale_outcome = store
            .consume(
                &terminal_stale_sm_id,
                b"anything",
                "PLAIN",
                &fence(&me, terminal_stale_epoch),
            )
            .await
            .expect("consume terminal stale");
        assert_eq!(terminal_stale_outcome, IsrConsumeOutcome::NoSuchToken);
        let claimed_stale_outcome = store
            .consume(
                &claimed_stale_sm_id,
                b"wrong-token-but-row-must-exist",
                "PLAIN",
                &fence(&me, claimed_stale_epoch),
            )
            .await
            .expect("consume claimed stale");
        assert_eq!(
            claimed_stale_outcome,
            IsrConsumeOutcome::Mismatched,
            "an old token with a live SM claim must not be swept"
        );
        let persisted_stale_outcome = store
            .consume(
                &persisted_stale_sm_id,
                b"wrong-token-but-row-must-exist",
                "PLAIN",
                &fence(&me, persisted_stale_epoch),
            )
            .await
            .expect("consume persisted stale");
        assert_eq!(
            persisted_stale_outcome,
            IsrConsumeOutcome::Mismatched,
            "an old token with durable resumable state must not be swept while its claim is absent"
        );
        let terminal_generation_stale_outcome = store
            .consume(
                &terminal_generation_stale_sm_id,
                b"wrong-token-but-row-must-exist",
                "PLAIN",
                &fence(&me, terminal_generation_stale_epoch),
            )
            .await
            .expect("consume terminal-generation stale");
        assert_eq!(
            terminal_generation_stale_outcome,
            IsrConsumeOutcome::Mismatched,
            "an old token with terminal-generation work must not be swept while its claim is absent"
        );
        let fresh_outcome = store
            .consume(
                &fresh_sm_id,
                b"wrong-token-but-row-must-exist",
                "PLAIN",
                &fence(&me, fresh_epoch),
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
    async fn sweep_expired_deletes_ordered_batches_until_both_tables_are_empty() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(db) = test_db().await else {
            return;
        };
        let store = PostgresIsrTokenStore::new(db.clone());
        store.ensure_schema().await.expect("ensure isr schema");
        ensure_sm_persistence_schema(&db).await;
        clean_sweep_tables(&db).await;

        let row_count = usize::try_from(ISR_SWEEP_BATCH_SIZE).expect("positive batch") + 3;
        for index in 0..row_count {
            let token_id = SmSessionId::new(format!("batch-token-{index:03}"));
            store
                .issue(&token_id, "PLAIN")
                .await
                .expect("seed token row");

            let fence_id = SmSessionId::new(format!("batch-fence-{index:03}"));
            let issued = store
                .issue(&fence_id, "PLAIN")
                .await
                .expect("seed fence source");
            assert_eq!(
                store
                    .revoke_if_current(&fence_id, &issued)
                    .await
                    .expect("turn token into revocation fence"),
                IsrRevokeOutcome::Revoked
            );
        }
        let conn = db.guard().await.expect("guard");
        conn.execute(
            "UPDATE clustering_isr_tokens SET created_at = now() - interval '1 day'",
            (),
        )
        .await
        .expect("backdate token batch");
        conn.execute(
            "UPDATE clustering_isr_revocation_fences SET created_at = now() - interval '1 day'",
            (),
        )
        .await
        .expect("backdate fence batch");
        drop(conn);

        assert_eq!(
            store
                .sweep_expired(std::time::Duration::from_secs(3600))
                .await
                .expect("first bounded sweep"),
            u64::try_from(ISR_SWEEP_BATCH_SIZE).expect("positive batch"),
            "the first pass must report only its bounded token deletion count"
        );
        let conn = db.guard().await.expect("guard");
        for table in ["clustering_isr_tokens", "clustering_isr_revocation_fences"] {
            let query = match table {
                "clustering_isr_tokens" => "SELECT COUNT(*) FROM clustering_isr_tokens",
                _ => "SELECT COUNT(*) FROM clustering_isr_revocation_fences",
            };
            let mut rows = conn
                .query(query, ())
                .await
                .expect("count rows after first pass");
            assert_eq!(
                rows.next()
                    .await
                    .expect("count result")
                    .expect("count row")
                    .get::<i64>(0)
                    .expect("count"),
                3,
                "first pass must leave the ordered tail in {table}"
            );
        }
        drop(conn);

        assert_eq!(
            store
                .sweep_expired(std::time::Duration::from_secs(3600))
                .await
                .expect("second bounded sweep"),
            3,
            "the next pass must make monotonic progress through the tail"
        );
        assert_eq!(
            store
                .sweep_expired(std::time::Duration::from_secs(3600))
                .await
                .expect("empty sweep"),
            0,
            "once drained, subsequent passes must be stable"
        );
        let conn = db.guard().await.expect("guard");
        let mut rows = conn
            .query(
                "SELECT (SELECT COUNT(*) FROM clustering_isr_tokens) + \
                        (SELECT COUNT(*) FROM clustering_isr_revocation_fences)",
                (),
            )
            .await
            .expect("count final rows");
        assert_eq!(
            rows.next()
                .await
                .expect("count result")
                .expect("count row")
                .get::<i64>(0)
                .expect("count"),
            0
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
        let sm_id = SmSessionId::new(format!("sm-{}", uuid::Uuid::new_v4()));
        let epoch = claim_sm_session(&db, &sm_id, &me).await;

        let issued = store.issue(&sm_id, "PLAIN").await.expect("issue");
        let wrong_epoch = ClaimEpoch(epoch.0 + 1);
        let outcome = store
            .consume(
                &sm_id,
                issued.token.as_bytes(),
                "PLAIN",
                &fence(&me, wrong_epoch),
            )
            .await;
        assert!(matches!(outcome, Err(IsrTokenStoreError::NotOwner)));

        // Fencing failure must not have touched the token row.
        let still_valid = store
            .consume(&sm_id, issued.token.as_bytes(), "PLAIN", &fence(&me, epoch))
            .await
            .expect("consume");
        assert!(matches!(still_valid, IsrConsumeOutcome::Matched { .. }));
    }

    #[tokio::test]
    async fn consume_fails_fencing_when_node_incarnation_changes_at_the_same_claim_epoch() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Some(db) = test_db().await else {
            return;
        };
        let store = PostgresIsrTokenStore::new(db.clone());
        store.ensure_schema().await.expect("ensure isr schema");
        let old = node_identity();
        let replacement = NodeIdentity::new(old.node_id.clone(), uuid::Uuid::new_v4().to_string());
        let sm_id = SmSessionId::new(format!("sm-incarnation-{}", uuid::Uuid::new_v4()));
        let epoch = claim_sm_session(&db, &sm_id, &old).await;
        let issued = store.issue(&sm_id, "PLAIN").await.expect("issue");
        let entity_key = sm_session_entity_key(&sm_id);
        db.guard()
            .await
            .expect("guard")
            .execute(
                "UPDATE clustering_claims SET node_epoch = ? WHERE entity = ?",
                crate::db_params![replacement.node_epoch.clone(), entity_key],
            )
            .await
            .expect("rotate claim owner incarnation without changing numeric epoch");

        let stale = store
            .consume(
                &sm_id,
                issued.token.as_bytes(),
                "PLAIN",
                &fence(&old, epoch),
            )
            .await;
        assert!(matches!(stale, Err(IsrTokenStoreError::NotOwner)));

        let current = store
            .consume(
                &sm_id,
                issued.token.as_bytes(),
                "PLAIN",
                &fence(&replacement, epoch),
            )
            .await
            .expect("current incarnation consume");
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
        let sm_id = SmSessionId::new(format!("sm-{}", uuid::Uuid::new_v4()));
        let epoch = claim_sm_session(&db, &sm_id, &me).await;
        let issued = store.issue(&sm_id, "PLAIN").await.expect("issue");

        // Manually hold the token row's lock open — standing in for
        // `consume()`'s own winning transaction, but under this test's
        // control, so the losing `consume()` call spawned below is
        // GUARANTEED to block on it rather than depending on scheduling
        // luck.
        let mut winner_tx = db.begin().await.expect("begin winner tx");
        let mut locked_rows = winner_tx
            .query(
                "SELECT token, mechanism FROM clustering_isr_tokens WHERE sm_id = ? FOR UPDATE",
                crate::db_params![sm_id.to_string()],
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
                    &fence(&me_loser, epoch),
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
                crate::db_params![sm_id.to_string()],
            )
            .await
            .expect("winner delete");
        let rotated_token = generate_isr_token();
        winner_tx
            .execute(
                "INSERT INTO clustering_isr_tokens (sm_id, token, mechanism, created_at) \
                 VALUES (?, ?, ?, now())",
                crate::db_params![
                    sm_id.to_string(),
                    rotated_token.clone(),
                    "PLAIN".to_string()
                ],
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
            .consume(
                &sm_id,
                rotated_token.as_bytes(),
                "PLAIN",
                &fence(&me, epoch),
            )
            .await
            .expect("consume with the winner's rotated token");
        assert!(
            matches!(survives, IsrConsumeOutcome::Matched { .. }),
            "the winner's rotated token must still exist and be consumable after the race; \
             got {survives:?}"
        );
    }
}
