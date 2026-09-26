//! Typed CRUD on top of [`super::schema`]'s `extension_job_outbox` table.
//!
//! Three guardrails both adversarial reviews of #1855 flagged as
//! launch-blocking (see that PR's description for the full reasoning) are
//! implemented here, not left to the drain worker to get right by
//! convention:
//!
//! 1. **Attempt-on-claim, not attempt-on-return**
//!    ([`claim_due_batch`]). `runtime::loader::WasmRuntime` builds its
//!    wasmtime engine with no epoch interruption or fuel limit, so a
//!    CPU-wedged (or just very slow) guest can block its own invocation
//!    indefinitely. [`claim_due_batch`]'s single `UPDATE` increments
//!    `attempt_count` as part of the atomic claim itself, *before* the guest
//!    is ever invoked — so a wedged job still marches toward
//!    [`dead_letter`] purely from repeated claim/reclaim cycles (once its
//!    lease goes stale), even if the guest call that first claimed it never
//!    returns.
//! 2. **Lease-checked finalize writes** ([`mark_done`], [`record_failure`],
//!    [`dead_letter`]). Each conditions its `UPDATE` on the caller still
//!    holding the row's *current* `lease_token` — not just `WHERE NOT
//!    done`. This was safe in `message_judgment_outbox` because Jev's HTTP
//!    timeout was a few seconds; here a job's real execution time is
//!    guest-controlled and can exceed [`CLAIM_TIMEOUT_MS`], so a lease can
//!    go stale mid-job, get reclaimed by another replica, and both workers
//!    could otherwise try to finalize the same row.
//! 3. **Fair per-extension claiming** ([`claim_due_batch`]). Rows are
//!    claimed round-robin across `extension_id` (via `ROW_NUMBER() OVER
//!    (PARTITION BY extension_id ...)`), not a single global `ORDER BY
//!    available_at_ms LIMIT N` — otherwise one backlogged or failing
//!    extension's jobs could starve every other extension's due jobs.
//!
//! The claim SQL shape (an outer `UPDATE ... WHERE <eligibility> AND id IN
//! (SELECT ...)` re-checking the same eligibility the inner `SELECT` used)
//! is carried over from `message_judgment_outbox::store::claim_due_batch`
//! verbatim except for the fairness ranking and the attempt-on-claim
//! increment — see that function's original doc comment (in the deleted
//! module's git history) for why the double eligibility check is what
//! makes two concurrent claimers on the same row race-safe on both Postgres
//! and SQLite without `SKIP LOCKED` (which SQLite does not support).

use jid::Jid;
use waddle_extensions::{JobKind, PluginId, RoomJid, WaddleId};
use waddle_xmpp_core::xep0359::StanzaId;

use super::ExtensionJobOutboxError;
use crate::db::{Database, Row};

/// Cap on the persisted body snapshot, mirroring
/// `message_judgment_outbox::store::MAX_BODY_SNAPSHOT_CHARS`: this table is
/// not the canonical message store, just enough context for a job handler.
pub const MAX_BODY_SNAPSHOT_CHARS: usize = 4_000;

/// A due row is considered abandoned (reclaimable by another node, or by
/// this same node's next poll tick) once its lease is older than this, in
/// milliseconds. Copied from `message_judgment_outbox`'s
/// `CLAIM_TIMEOUT_MS` (5 minutes) — see [`claim_due_batch`]'s doc comment
/// for why a job's real duration is no longer host-bounded the way that
/// value's original justification assumed, which is exactly why guardrail
/// 2 (lease-checked finalize) exists.
pub const CLAIM_TIMEOUT_MS: i64 = 300_000;

/// Typed id for one `extension_job_outbox` row. A UUID (v4), matching this
/// codebase's outbox-row-id convention.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExtensionJobOutboxId(String);

impl ExtensionJobOutboxId {
    pub fn generate() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    fn from_stored(value: String) -> Self {
        Self(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Opaque claim token for one [`claim_due_batch`] winner. A caller may only
/// finalize ([`mark_done`]/[`record_failure`]/[`dead_letter`]) a row while
/// presenting the exact token it was handed at claim time (guardrail 2).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExtensionJobOutboxLeaseToken(String);

impl ExtensionJobOutboxLeaseToken {
    pub(crate) fn generate() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    fn from_stored(value: String) -> Self {
        Self(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Input to [`enqueue_pending_in_tx`]/[`enqueue_pending`]. `body` is the
/// raw, untruncated message body; truncation happens inside the enqueue
/// call, not at the call site.
pub struct PendingJobInput {
    /// The plugin currently holding the `durable-job` grant for `job_kind`
    /// — resolved by the caller via
    /// `ExtensionManager::durable_job_grant_holder` at enqueue time (the
    /// grant-derived enqueue gate; see `ingress::durable`'s call site).
    pub extension_id: PluginId,
    pub job_kind: JobKind,
    pub waddle_id: WaddleId,
    pub room: Option<RoomJid>,
    pub target_stanza_id: StanzaId,
    pub body: String,
    pub now_ms: i64,
}

/// One claimed row from `extension_job_outbox`, ready to hand to the
/// extension manager as a `DurableJob` event.
#[derive(Debug, Clone)]
pub struct ClaimedJob {
    pub id: ExtensionJobOutboxId,
    pub lease_token: ExtensionJobOutboxLeaseToken,
    pub extension_id: PluginId,
    pub job_kind: JobKind,
    pub waddle_id: WaddleId,
    pub room: Option<RoomJid>,
    pub target_stanza_id: StanzaId,
    pub body_snapshot: String,
    /// Reflects the attempt-on-claim increment: 1 on a row's first ever
    /// claim, incremented again on every subsequent reclaim (including a
    /// reclaim of a row whose previous claimant's guest invocation never
    /// returned).
    pub attempt_count: i64,
    pub last_error: Option<String>,
    pub created_at_ms: i64,
}

const CLAIMED_SELECT_COLUMNS: &str = "id, lease_token, extension_id, job_kind, waddle_id, room, \
     target_stanza_id, target_stanza_by, body_snapshot, attempt_count, last_error, created_at_ms";

const ENQUEUE_SQL: &str = "INSERT INTO extension_job_outbox \
     (id, extension_id, job_kind, waddle_id, room, target_stanza_id, target_stanza_by, \
      body_snapshot, available_at_ms, attempt_count, last_error, done, created_at_ms) \
     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 0, NULL, FALSE, ?)";

fn enqueue_params(
    id: &ExtensionJobOutboxId,
    input: &PendingJobInput,
    body_snapshot: &str,
) -> Vec<crate::db::Value> {
    crate::db_params![
        id.as_str(),
        input.extension_id.as_str(),
        input.job_kind.as_str(),
        input.waddle_id.as_str(),
        input.room.as_ref().map(|room| room.as_str().to_string()),
        input.target_stanza_id.id.as_str(),
        input.target_stanza_id.by.to_string(),
        body_snapshot,
        input.now_ms,
        input.now_ms,
    ]
}

/// Enqueue one durable job. Used by this module's own tests and any future
/// non-transactional caller. The real production enqueue site
/// (`ingress::durable::apply_durable`) uses [`enqueue_pending_in_tx`]
/// instead, so the row and the archive write it accompanies commit or roll
/// back together — the same transactional-outbox property
/// `message_judgment_outbox` had.
pub async fn enqueue_pending(
    db: &Database,
    input: PendingJobInput,
) -> Result<(), ExtensionJobOutboxError> {
    let id = ExtensionJobOutboxId::generate();
    let body_snapshot: String = input.body.chars().take(MAX_BODY_SNAPSHOT_CHARS).collect();
    let connection = db.guard().await?;
    connection
        .execute(ENQUEUE_SQL, enqueue_params(&id, &input, &body_snapshot))
        .await?;
    Ok(())
}

/// Enqueue one durable job inside the caller's own transaction — the real
/// transactional-outbox seam. See [`enqueue_pending`]'s doc comment.
pub async fn enqueue_pending_in_tx(
    tx: &mut crate::db::Transaction<'_>,
    input: PendingJobInput,
) -> Result<(), ExtensionJobOutboxError> {
    let id = ExtensionJobOutboxId::generate();
    let body_snapshot: String = input.body.chars().take(MAX_BODY_SNAPSHOT_CHARS).collect();
    tx.execute(ENQUEUE_SQL, enqueue_params(&id, &input, &body_snapshot))
        .await?;
    Ok(())
}

/// Claim up to `limit` due rows for this drain pass, fairly across
/// extensions, incrementing each claimed row's `attempt_count` as part of
/// the same atomic `UPDATE` (guardrails 1 and 3 — see the module docs).
///
/// Fairness: due rows are ranked per-`extension_id` by `available_at_ms`
/// (`ROW_NUMBER() OVER (PARTITION BY extension_id ORDER BY
/// available_at_ms, id)`), then the batch is filled by ascending rank
/// first — every extension's oldest due row before any extension's second
/// oldest, and so on — so one extension with a large backlog (or one stuck
/// in a failure loop, whose rows keep coming back due after backoff) can
/// claim at most a proportional share of any single batch instead of
/// crowding out every other extension's due jobs.
///
/// Race-safety: mirrors `message_judgment_outbox::store::claim_due_batch`'s
/// exclusivity proof exactly — the outer `UPDATE`'s `WHERE` re-checks the
/// same due/unleased-or-stale eligibility the inner ranked `SELECT` used,
/// so a concurrent claimer that already won one of this call's
/// preselected ids makes this call's `UPDATE` a no-op for that id.
pub async fn claim_due_batch(
    db: &Database,
    limit: i64,
    now_ms: i64,
) -> Result<Vec<ClaimedJob>, ExtensionJobOutboxError> {
    let token = ExtensionJobOutboxLeaseToken::generate();
    let stale = now_ms.saturating_sub(CLAIM_TIMEOUT_MS);
    let bounded_limit = limit.clamp(1, 1_000);
    let connection = db.guard().await?;
    let mut rows = connection
        .query(
            &format!(
                "UPDATE extension_job_outbox \
                 SET lease_token = ?, leased_at_ms = ?, attempt_count = attempt_count + 1 \
                 WHERE NOT done AND available_at_ms <= ? \
                   AND (lease_token IS NULL OR leased_at_ms <= ?) \
                   AND id IN ( \
                     SELECT id FROM ( \
                       SELECT id, \
                              ROW_NUMBER() OVER ( \
                                PARTITION BY extension_id ORDER BY available_at_ms, id \
                              ) AS rn \
                       FROM extension_job_outbox \
                       WHERE NOT done AND available_at_ms <= ? \
                         AND (lease_token IS NULL OR leased_at_ms <= ?) \
                     ) ranked \
                     ORDER BY rn, id \
                     LIMIT ? \
                   ) \
                 RETURNING {CLAIMED_SELECT_COLUMNS}"
            ),
            crate::db_params![
                token.as_str(),
                now_ms,
                now_ms,
                stale,
                now_ms,
                stale,
                bounded_limit,
            ],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(decode_claimed_row(&row)?);
    }
    Ok(out)
}

/// Mark a row done after a successful job — permanently removing it from
/// the due set. Lease-checked (guardrail 2): a no-op, returning `false`,
/// once another worker has already reclaimed this row (its lease token no
/// longer matches) or already finalized it.
pub async fn mark_done(
    db: &Database,
    id: &ExtensionJobOutboxId,
    lease_token: &ExtensionJobOutboxLeaseToken,
) -> Result<bool, ExtensionJobOutboxError> {
    let connection = db.guard().await?;
    let affected = connection
        .execute(
            "UPDATE extension_job_outbox SET done = TRUE \
             WHERE id = ? AND lease_token = ? AND NOT done",
            crate::db_params![id.as_str(), lease_token.as_str()],
        )
        .await?;
    Ok(affected > 0)
}

/// Dead-letter a row that exceeded the retry budget: stores the terminal
/// `error` and marks the row done. Lease-checked (guardrail 2), like
/// [`mark_done`]. Does not touch `attempt_count` — it was already
/// incremented at claim time (guardrail 1).
pub async fn dead_letter(
    db: &Database,
    id: &ExtensionJobOutboxId,
    lease_token: &ExtensionJobOutboxLeaseToken,
    error: &str,
) -> Result<bool, ExtensionJobOutboxError> {
    let connection = db.guard().await?;
    let affected = connection
        .execute(
            "UPDATE extension_job_outbox SET last_error = ?, done = TRUE \
             WHERE id = ? AND lease_token = ? AND NOT done",
            crate::db_params![error, id.as_str(), lease_token.as_str()],
        )
        .await?;
    Ok(affected > 0)
}

/// Record one failed job attempt: stores `error` and reschedules
/// `available_at_ms` with backoff, clearing the lease so the row is
/// reclaimable as soon as it is due again. Lease-checked (guardrail 2).
/// Does not touch `attempt_count` (see [`dead_letter`]'s doc comment) — the
/// caller (`drain::drain_once`) decides whether to call this or
/// [`dead_letter`] based on the `attempt_count` [`claim_due_batch`] already
/// returned.
pub async fn record_failure(
    db: &Database,
    id: &ExtensionJobOutboxId,
    lease_token: &ExtensionJobOutboxLeaseToken,
    error: &str,
    next_attempt_at_ms: i64,
) -> Result<bool, ExtensionJobOutboxError> {
    let connection = db.guard().await?;
    let affected = connection
        .execute(
            // Clears `lease_token`/`leased_at_ms`: the retry backoff delay
            // for early attempts is far shorter than `CLAIM_TIMEOUT_MS`, so
            // a row that kept its old claim would sit unreclaimable long
            // after it becomes due again.
            "UPDATE extension_job_outbox \
             SET last_error = ?, available_at_ms = ?, lease_token = NULL, leased_at_ms = NULL \
             WHERE id = ? AND lease_token = ? AND NOT done",
            crate::db_params![error, next_attempt_at_ms, id.as_str(), lease_token.as_str()],
        )
        .await?;
    Ok(affected > 0)
}

/// Fetch up to `limit` not-yet-done due rows, oldest first, ignoring
/// `extension_id` fairness and never touching the lease. Read-only
/// inspection only (tests and future metrics/backfill tooling) — never
/// safe for the drain worker itself; see [`claim_due_batch`].
#[cfg(test)]
pub async fn fetch_due_batch(
    db: &Database,
    limit: i64,
    now_ms: i64,
) -> Result<Vec<ClaimedJob>, ExtensionJobOutboxError> {
    let connection = db.guard().await?;
    let mut rows = connection
        .query(
            "SELECT id, lease_token, extension_id, job_kind, waddle_id, room, \
             target_stanza_id, target_stanza_by, body_snapshot, attempt_count, \
             last_error, created_at_ms \
             FROM extension_job_outbox \
             WHERE NOT done AND available_at_ms <= ? \
             ORDER BY available_at_ms, id \
             LIMIT ?",
            crate::db_params![now_ms, limit.clamp(1, 1_000)],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(decode_claimed_row_allow_unleased(&row)?);
    }
    Ok(out)
}

fn decode_claimed_row(row: &Row) -> Result<ClaimedJob, ExtensionJobOutboxError> {
    let lease_token: Option<String> = row.get(1)?;
    let lease_token = lease_token
        .map(ExtensionJobOutboxLeaseToken::from_stored)
        .ok_or(ExtensionJobOutboxError::MissingLeaseToken)?;
    decode_claimed_row_with_lease(row, lease_token)
}

#[cfg(test)]
fn decode_claimed_row_allow_unleased(row: &Row) -> Result<ClaimedJob, ExtensionJobOutboxError> {
    let lease_token: Option<String> = row.get(1)?;
    decode_claimed_row_with_lease(
        row,
        ExtensionJobOutboxLeaseToken::from_stored(lease_token.unwrap_or_default()),
    )
}

fn decode_claimed_row_with_lease(
    row: &Row,
    lease_token: ExtensionJobOutboxLeaseToken,
) -> Result<ClaimedJob, ExtensionJobOutboxError> {
    let id: String = row.get(0)?;
    let extension_id: String = row.get(2)?;
    let job_kind: String = row.get(3)?;
    let waddle_id: String = row.get(4)?;
    let room: Option<String> = row.get(5)?;
    let target_stanza_id: String = row.get(6)?;
    let target_stanza_by: String = row.get(7)?;
    let by: Jid = target_stanza_by
        .parse()
        .map_err(|_| ExtensionJobOutboxError::InvalidStanzaByJid(target_stanza_by.clone()))?;
    Ok(ClaimedJob {
        id: ExtensionJobOutboxId::from_stored(id),
        lease_token,
        extension_id: PluginId::new(extension_id.clone())
            .map_err(|_| ExtensionJobOutboxError::InvalidPluginId(extension_id))?,
        job_kind: JobKind::new(job_kind.clone())
            .map_err(|_| ExtensionJobOutboxError::InvalidJobKind(job_kind))?,
        waddle_id: WaddleId::new(waddle_id.clone())
            .map_err(|_| ExtensionJobOutboxError::InvalidWaddleId(waddle_id))?,
        room: room
            .map(|room| {
                RoomJid::new(room.clone())
                    .map_err(|_| ExtensionJobOutboxError::InvalidRoomJid(room))
            })
            .transpose()?,
        target_stanza_id: StanzaId::new(target_stanza_id, by),
        body_snapshot: row.get(8)?,
        attempt_count: row.get(9)?,
        last_error: row.get(10)?,
        created_at_ms: row.get(11)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    fn extension(name: &str) -> PluginId {
        PluginId::new(name).expect("plugin id")
    }

    fn kind() -> JobKind {
        JobKind::new("message-judge").expect("job kind")
    }

    fn waddle() -> WaddleId {
        WaddleId::new("default").expect("waddle id")
    }

    fn stanza(id: &str) -> StanzaId {
        StanzaId::new(
            id.to_string(),
            "room@conference.example.test".parse().expect("room jid"),
        )
    }

    fn input(extension_id: PluginId, stanza_id: &str, now_ms: i64) -> PendingJobInput {
        PendingJobInput {
            extension_id,
            job_kind: kind(),
            waddle_id: waddle(),
            room: None,
            target_stanza_id: stanza(stanza_id),
            body: "body".to_string(),
            now_ms,
        }
    }

    async fn test_db() -> Database {
        let db = Database::in_memory(&format!(
            "extension-job-outbox-store-{}",
            uuid::Uuid::new_v4()
        ))
        .await
        .expect("in-memory database");
        super::super::schema::initialize(&db)
            .await
            .expect("initialize");
        db
    }

    #[tokio::test]
    async fn schema_initializes_cleanly_and_is_idempotent() {
        let db = test_db().await;
        super::super::schema::initialize(&db)
            .await
            .expect("reinitialize");
    }

    #[tokio::test]
    async fn enqueue_then_claim_round_trips_every_field() {
        let db = test_db().await;
        enqueue_pending(&db, input(extension("ext-a"), "stanza-1", 1_000))
            .await
            .expect("enqueue");

        let none_due = claim_due_batch(&db, 10, 500).await.expect("claim");
        assert!(none_due.is_empty(), "not yet due");

        let claimed = claim_due_batch(&db, 10, 1_000).await.expect("claim");
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].extension_id, extension("ext-a"));
        assert_eq!(claimed[0].job_kind, kind());
        assert_eq!(claimed[0].waddle_id, waddle());
        assert!(claimed[0].room.is_none());
        assert_eq!(claimed[0].target_stanza_id.id.as_str(), "stanza-1");
        assert_eq!(claimed[0].body_snapshot, "body");
        assert!(claimed[0].last_error.is_none());
    }

    #[tokio::test]
    async fn enqueue_truncates_oversized_body() {
        let db = test_db().await;
        let mut long_input = input(extension("ext-a"), "stanza-long", 1_000);
        long_input.body = "x".repeat(MAX_BODY_SNAPSHOT_CHARS + 500);
        enqueue_pending(&db, long_input).await.expect("enqueue");

        let claimed = claim_due_batch(&db, 10, 1_000).await.expect("claim");
        assert_eq!(
            claimed[0].body_snapshot.chars().count(),
            MAX_BODY_SNAPSHOT_CHARS
        );
    }

    // Guardrail 1: attempt-on-claim, not attempt-on-return.

    #[tokio::test]
    async fn claim_increments_attempt_count_before_any_invocation() {
        let db = test_db().await;
        enqueue_pending(&db, input(extension("ext-a"), "stanza-attempt", 1_000))
            .await
            .expect("enqueue");

        let first = claim_due_batch(&db, 10, 1_000).await.expect("claim");
        assert_eq!(
            first[0].attempt_count, 1,
            "the very first claim must already report attempt 1 — before any guest is invoked"
        );

        // Simulate the first claimant's guest call never returning: its
        // lease goes stale and a later pass reclaims the same row. Each
        // reclaim bumps attempt_count again, entirely independent of
        // whether the original invocation ever completes.
        let reclaimed = claim_due_batch(&db, 10, 1_000 + CLAIM_TIMEOUT_MS + 1)
            .await
            .expect("reclaim");
        assert_eq!(reclaimed.len(), 1);
        assert_eq!(
            reclaimed[0].attempt_count, 2,
            "a reclaim of a wedged job's row must bump attempt_count again on its own"
        );
    }

    // Guardrail 2: lease-checked finalize writes.

    #[tokio::test]
    async fn mark_done_is_rejected_once_another_worker_has_reclaimed_the_row() {
        let db = test_db().await;
        enqueue_pending(
            &db,
            input(extension("ext-a"), "stanza-stale-finalize", 1_000),
        )
        .await
        .expect("enqueue");

        let original = claim_due_batch(&db, 10, 1_000).await.expect("first claim");
        let original_lease = original[0].lease_token.clone();

        // The original worker's lease goes stale (its guest call is still
        // running past CLAIM_TIMEOUT_MS) and a second worker reclaims it.
        let reclaimer = claim_due_batch(&db, 10, 1_000 + CLAIM_TIMEOUT_MS + 1)
            .await
            .expect("reclaim");
        assert_eq!(reclaimer.len(), 1);
        assert_ne!(
            reclaimer[0].lease_token, original_lease,
            "reclaiming must mint a fresh lease token"
        );

        // The original (stale) worker's guest call finally returns and
        // tries to finalize using its now-superseded lease token. This
        // must be rejected, not silently applied over the reclaimer's
        // ongoing work.
        let applied = mark_done(&db, &original[0].id, &original_lease)
            .await
            .expect("mark_done must not error");
        assert!(
            !applied,
            "a stale-leased worker's mark_done must be rejected once reclaimed"
        );

        // The reclaimer's own finalize, using its current lease, must
        // still succeed.
        let applied = mark_done(&db, &reclaimer[0].id, &reclaimer[0].lease_token)
            .await
            .expect("mark_done must not error");
        assert!(applied, "the current lease holder's finalize must succeed");
    }

    #[tokio::test]
    async fn record_failure_is_rejected_once_another_worker_has_reclaimed_the_row() {
        let db = test_db().await;
        enqueue_pending(&db, input(extension("ext-a"), "stanza-stale-retry", 1_000))
            .await
            .expect("enqueue");

        let original = claim_due_batch(&db, 10, 1_000).await.expect("first claim");
        let original_lease = original[0].lease_token.clone();
        claim_due_batch(&db, 10, 1_000 + CLAIM_TIMEOUT_MS + 1)
            .await
            .expect("reclaim");

        let applied = record_failure(
            &db,
            &original[0].id,
            &original_lease,
            "stale failure",
            50_000,
        )
        .await
        .expect("record_failure must not error");
        assert!(
            !applied,
            "a stale-leased worker's record_failure must be rejected once reclaimed"
        );
    }

    #[tokio::test]
    async fn dead_letter_is_rejected_once_another_worker_has_reclaimed_the_row() {
        let db = test_db().await;
        enqueue_pending(
            &db,
            input(extension("ext-a"), "stanza-stale-dead-letter", 1_000),
        )
        .await
        .expect("enqueue");

        let original = claim_due_batch(&db, 10, 1_000).await.expect("first claim");
        let original_lease = original[0].lease_token.clone();
        claim_due_batch(&db, 10, 1_000 + CLAIM_TIMEOUT_MS + 1)
            .await
            .expect("reclaim");

        let applied = dead_letter(&db, &original[0].id, &original_lease, "stale failure")
            .await
            .expect("dead_letter must not error");
        assert!(
            !applied,
            "a stale-leased worker's dead_letter must be rejected once reclaimed"
        );
    }

    #[tokio::test]
    async fn mark_done_is_a_no_op_once_the_row_is_already_done() {
        let db = test_db().await;
        enqueue_pending(&db, input(extension("ext-a"), "stanza-already-done", 1_000))
            .await
            .expect("enqueue");
        let claimed = claim_due_batch(&db, 10, 1_000).await.expect("claim");
        assert!(mark_done(&db, &claimed[0].id, &claimed[0].lease_token)
            .await
            .expect("first mark_done"));

        let applied = mark_done(&db, &claimed[0].id, &claimed[0].lease_token)
            .await
            .expect("second mark_done must not error");
        assert!(
            !applied,
            "marking an already-done row done again must be a no-op"
        );
    }

    // Guardrail 3: fair per-extension claiming.

    #[tokio::test]
    async fn claim_due_batch_gives_every_extension_a_row_before_any_extension_a_second() {
        let db = test_db().await;
        for index in 0..5 {
            enqueue_pending(
                &db,
                input(extension("busy"), &format!("busy-{index}"), 1_000),
            )
            .await
            .expect("enqueue busy");
        }
        enqueue_pending(&db, input(extension("quiet"), "quiet-1", 1_000))
            .await
            .expect("enqueue quiet");

        let claimed = claim_due_batch(&db, 3, 1_000).await.expect("claim");
        assert_eq!(claimed.len(), 3);
        assert!(
            claimed.iter().any(|row| row.extension_id == extension("quiet")),
            "the quiet extension's only due row must not be starved by the busy extension's backlog"
        );
    }

    // Concurrency (ported from `message_judgment_outbox::store`'s own race
    // tests — the same claim-exclusivity property, now over the
    // fairness-ranked query).

    #[tokio::test]
    async fn concurrent_claim_due_batch_calls_never_both_win_the_same_row() {
        let db = test_db().await;
        enqueue_pending(&db, input(extension("ext-a"), "stanza-race", 1_000))
            .await
            .expect("enqueue");

        let (first, second) = tokio::join!(
            claim_due_batch(&db, 10, 1_000),
            claim_due_batch(&db, 10, 1_000),
        );
        let claimed = first.expect("first claim").len() + second.expect("second claim").len();
        assert_eq!(
            claimed, 1,
            "exactly one concurrent caller may claim the single due row"
        );
    }

    #[tokio::test]
    async fn claim_due_batch_reclaims_a_stale_lease_after_a_crashed_node() {
        let db = test_db().await;
        enqueue_pending(&db, input(extension("ext-a"), "stanza-stale-lease", 1_000))
            .await
            .expect("enqueue");

        let first = claim_due_batch(&db, 10, 1_000).await.expect("first claim");
        assert_eq!(first.len(), 1);
        assert!(claim_due_batch(&db, 10, 1_000)
            .await
            .expect("still leased")
            .is_empty());

        let after_timeout = 1_000 + CLAIM_TIMEOUT_MS + 1;
        let reclaimed = claim_due_batch(&db, 10, after_timeout)
            .await
            .expect("reclaim after stale lease");
        assert_eq!(reclaimed.len(), 1);
    }

    // Transactional enqueue (the real production seam:
    // `ingress::durable::apply_durable` via
    // `ingress_uow::ExtensionJobOutboxRepository`).

    #[tokio::test]
    async fn enqueue_pending_in_tx_is_visible_once_committed() {
        let db = test_db().await;
        let mut tx = db.begin().await.expect("begin");
        enqueue_pending_in_tx(
            &mut tx,
            input(extension("ext-a"), "stanza-tx-commit", 1_000),
        )
        .await
        .expect("enqueue in tx");
        tx.commit().await.expect("commit");

        let claimed = claim_due_batch(&db, 10, 1_000).await.expect("claim");
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].target_stanza_id.id.as_str(), "stanza-tx-commit");
    }

    #[tokio::test]
    async fn enqueue_pending_in_tx_is_absent_when_the_transaction_rolls_back() {
        let db = test_db().await;
        let mut tx = db.begin().await.expect("begin");
        enqueue_pending_in_tx(
            &mut tx,
            input(extension("ext-a"), "stanza-tx-rollback", 1_000),
        )
        .await
        .expect("enqueue in tx");
        drop(tx);

        let claimed = claim_due_batch(&db, 10, 1_000).await.expect("claim");
        assert!(claimed.is_empty(), "a rolled-back enqueue must not persist");
    }
}
