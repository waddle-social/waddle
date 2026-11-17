//! Ending a retired account's open sessions on every replica (ADR-0020 D8).
//!
//! Retirement ([`super::retirement::retire_account`]) removes the account's
//! rows. The streams the account has open at that moment, and the
//! stream-management sessions waiting for it to resume, are held by the
//! replicas that serve them. `waddle-server account retire` ends them in a
//! second step, after the retirement committed:
//!
//! - It records a *request* that names the account
//!   ([`request_session_ending`]).
//! - Every serving replica holds a *lease*: a row it registers when it
//!   starts and renews every [`SESSION_ENDING_TICK`] for
//!   [`SESSION_ENDING_LEASE_TTL`]. Each tick it reads the requests of the
//!   last [`SESSION_ENDING_WINDOW`] that it has not completed and whose
//!   account the ledger of retired JIDs holds ([`requests_to_complete`]),
//!   refuses the account's bindings from then on, ends the account's
//!   streams and detached sessions it holds, and records the request as
//!   completed with what it ended (`crate::server::account_session_endings`).
//! - The command waits ([`await_session_endings`]) until every replica whose
//!   lease counts has completed the request or has stopped renewing its
//!   lease, and reports each replica.
//!
//! The command reads no replica's clock to decide whether that replica
//! serves. It sees a lease renewed while it waits, and decides that a
//! replica stopped when its lease has not been renewed for
//! [`SESSION_ENDING_LEASE_TTL`] plus [`SESSION_ENDING_LAPSE_GRACE`], both
//! measured on its own monotonic clock. Only which leases it looks at
//! depends on the clocks: those whose recorded end is no earlier than
//! [`SESSION_ENDING_CLOCK_ALLOWANCE`] before the request.
//!
//! A replica that starts later reads the requests of the window too and
//! completes them: it refuses the account's bindings and ends the detached
//! sessions an earlier process left.
//!
//! Times are milliseconds since the Unix epoch on the clock of the process
//! that writes them, on both engines. The runtime role registers and renews
//! leases and records acknowledgements; it reads requests and deletes
//! nothing. [`prune_session_endings`] is the owner's. Nothing here logs a
//! JID.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use jid::BareJid;
use uuid::Uuid;

use super::retirement::retirement_key_of;
use crate::db::{Database, DatabaseError, Transaction};
use crate::ingress_substrate::set_local_transaction_timeouts;

/// How long a lease lasts after the registration or renewal that set it.
pub const SESSION_ENDING_LEASE_TTL: Duration = Duration::from_secs(5);
/// How often a replica renews its lease and reads the requests.
pub const SESSION_ENDING_TICK: Duration = Duration::from_millis(500);
/// How long beyond [`SESSION_ENDING_LEASE_TTL`] the command waits for a
/// lease to be renewed before it decides that its replica stopped.
pub const SESSION_ENDING_LAPSE_GRACE: Duration = Duration::from_secs(2);
/// How far before the request a lease's recorded end may lie for the
/// command to look at its replica. It absorbs a difference between the
/// command's clock and a replica's.
pub const SESSION_ENDING_CLOCK_ALLOWANCE: Duration = Duration::from_secs(10);
/// How old a request may be for a replica to act on it. A replica that
/// starts within the window after a retirement still ends the account's
/// detached sessions and refuses its bindings.
pub const SESSION_ENDING_WINDOW: Duration = Duration::from_secs(24 * 60 * 60);
/// How long after its end a lease is kept before the owner removes it.
const LEASE_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);
/// How long a request and its acknowledgements are kept: past the window,
/// so that no replica still reads a request the owner removes.
const REQUEST_RETENTION: Duration = Duration::from_secs(2 * 24 * 60 * 60);
/// The shortest the command observes the leases before it reports that no
/// replica is left to complete the request: a replica registers its lease
/// within its first tick.
const MINIMUM_OBSERVATION: Duration = Duration::from_secs(1);
/// How often the command reads the acknowledgements again.
const AWAIT_POLL: Duration = Duration::from_millis(100);
/// How many requests a replica reads in one tick.
const REQUESTS_PER_TICK: i64 = 64;
/// Longest a statement here waits for a lock, and longest it runs.
const LOCK_TIMEOUT: Duration = Duration::from_millis(500);
const STATEMENT_TIMEOUT: Duration = Duration::from_secs(2);

/// Why a statement about session endings did not complete.
#[derive(Debug, thiserror::Error)]
pub enum SessionEndingError {
    #[error("the JID names no account: it needs a localpart and a canonical form")]
    NotAnAccountJid,
    #[error("the statement bounds could not be installed")]
    BoundsUnproven,
    #[error("a stored session-ending row is malformed: {0}")]
    Corrupt(&'static str),
    #[error("a session-ending statement failed")]
    Database(#[from] DatabaseError),
}

/// A request to end the open sessions of a retired account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionEndingRequest {
    pub id: Uuid,
    /// The account's bare JID in canonical form.
    pub account: BareJid,
    /// When the request was recorded, on the recording process's clock.
    pub requested_at_ms: i64,
}

/// What a replica ended for one request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EndedSessions {
    /// Live streams of the account the replica closed.
    pub streams_closed: u64,
    /// Detached stream-management sessions of the account the replica
    /// ended.
    pub detached_sessions_ended: u64,
}

impl EndedSessions {
    pub fn add(&mut self, other: EndedSessions) {
        self.streams_closed = self.streams_closed.saturating_add(other.streams_closed);
        self.detached_sessions_ended = self
            .detached_sessions_ended
            .saturating_add(other.detached_sessions_ended);
    }
}

/// Where one replica stands with a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplicaEnding {
    /// The replica ended every stream and detached session of the account
    /// it held.
    Completed(EndedSessions),
    /// The replica stopped renewing its lease, or released it, without
    /// completing the request. A replica that serves again completes it
    /// within the window.
    Lapsed,
    /// The replica renews its lease and has not completed the request yet.
    Pending,
}

impl ReplicaEnding {
    /// The state's name in the command's report.
    pub fn label(self) -> &'static str {
        match self {
            Self::Completed(_) => "completed",
            Self::Lapsed => "lapsed",
            Self::Pending => "pending",
        }
    }
}

/// One replica's line in the report of a request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplicaReport {
    pub replica: Uuid,
    /// The replica's clustering node identity, or its host and process.
    pub node: String,
    pub ending: ReplicaEnding,
}

/// Where every replica that counts stands with a request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionEndingReport {
    pub request: Uuid,
    pub replicas: Vec<ReplicaReport>,
}

impl SessionEndingReport {
    /// Whether no serving replica is left to complete the request: each
    /// one completed it or stopped renewing its lease.
    pub fn confirmed(&self) -> bool {
        self.replicas
            .iter()
            .all(|replica| replica.ending != ReplicaEnding::Pending)
    }
}

/// What [`prune_session_endings`] removed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SessionEndingPrune {
    pub leases: u64,
    pub requests: u64,
    pub acknowledgements: u64,
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn millis(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

fn count(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn stored_count(value: i64) -> Result<u64, SessionEndingError> {
    u64::try_from(value).map_err(|_| SessionEndingError::Corrupt("count"))
}

fn stored_id(value: &str, what: &'static str) -> Result<Uuid, SessionEndingError> {
    Uuid::try_parse(value).map_err(|_| SessionEndingError::Corrupt(what))
}

async fn begin_bounded(db: &Database) -> Result<Transaction<'_>, SessionEndingError> {
    let mut tx = db.begin().await?;
    if !set_local_transaction_timeouts(&mut tx, LOCK_TIMEOUT, STATEMENT_TIMEOUT).await? {
        return Err(SessionEndingError::BoundsUnproven);
    }
    Ok(tx)
}

async fn execute_one(
    db: &Database,
    sql: &str,
    params: Vec<crate::db::Value>,
) -> Result<u64, SessionEndingError> {
    let mut tx = begin_bounded(db).await?;
    let affected = tx.execute(sql, params).await?;
    tx.commit().await?;
    Ok(affected)
}

/// Record a request to end the open sessions of the retired account
/// `account`. The owner's command calls it after the retirement committed.
pub async fn request_session_ending(
    db: &Database,
    account: &BareJid,
) -> Result<SessionEndingRequest, SessionEndingError> {
    let key = retirement_key_of(account).ok_or(SessionEndingError::NotAnAccountJid)?;
    let canonical: BareJid = key
        .parse()
        .map_err(|_| SessionEndingError::NotAnAccountJid)?;
    let request = SessionEndingRequest {
        id: Uuid::new_v4(),
        account: canonical,
        requested_at_ms: now_ms(),
    };
    execute_one(
        db,
        "INSERT INTO account_session_ending_requests (request_id, account_jid, requested_at_ms) \
         VALUES (?, ?, ?)",
        crate::db_params![
            request.id.simple().to_string(),
            key,
            request.requested_at_ms
        ],
    )
    .await?;
    Ok(request)
}

/// One replica's lease and acknowledgement as one reading shows them.
struct ReplicaReading {
    replica: Uuid,
    node: String,
    lease_expires_at_ms: i64,
    completed: Option<EndedSessions>,
}

async fn read_replicas(
    db: &Database,
    request: &SessionEndingRequest,
) -> Result<Vec<ReplicaReading>, SessionEndingError> {
    let since = request
        .requested_at_ms
        .saturating_sub(millis(SESSION_ENDING_CLOCK_ALLOWANCE));
    let mut tx = begin_bounded(db).await?;
    let mut rows = tx
        .query(
            "SELECT r.replica_id, r.node, r.lease_expires_at_ms, a.completed_at_ms, \
             a.streams_closed, a.detached_sessions_ended \
             FROM account_session_ending_replicas r \
             LEFT JOIN account_session_ending_acks a \
             ON a.replica_id = r.replica_id AND a.request_id = ? \
             WHERE r.lease_expires_at_ms > ? OR a.completed_at_ms IS NOT NULL",
            crate::db_params![request.id.simple().to_string(), since],
        )
        .await?;
    let mut readings = Vec::new();
    while let Some(row) = rows.next().await? {
        let replica = stored_id(&row.get::<String>(0)?, "replica_id")?;
        let completed = match row.get::<Option<i64>>(3)? {
            Some(_) => Some(EndedSessions {
                streams_closed: stored_count(row.get::<Option<i64>>(4)?.unwrap_or(0))?,
                detached_sessions_ended: stored_count(row.get::<Option<i64>>(5)?.unwrap_or(0))?,
            }),
            None => None,
        };
        readings.push(ReplicaReading {
            replica,
            node: row.get::<String>(1)?,
            lease_expires_at_ms: row.get::<i64>(2)?,
            completed,
        });
    }
    drop(rows);
    tx.rollback().await?;
    Ok(readings)
}

/// What the command has seen of one replica's lease.
struct ObservedLease {
    node: String,
    lease_expires_at_ms: i64,
    /// When, on the command's clock, the lease was first read or last seen
    /// renewed.
    renewed_at: Instant,
    ending: ReplicaEnding,
}

/// Fold one reading into what the command has observed, at `now`.
fn observe(
    observed: &mut BTreeMap<Uuid, ObservedLease>,
    readings: Vec<ReplicaReading>,
    now: Instant,
) {
    let lapse_after = SESSION_ENDING_LEASE_TTL + SESSION_ENDING_LAPSE_GRACE;
    let mut read = BTreeSet::new();
    for reading in readings {
        read.insert(reading.replica);
        let lease = observed
            .entry(reading.replica)
            .or_insert_with(|| ObservedLease {
                node: reading.node.clone(),
                lease_expires_at_ms: reading.lease_expires_at_ms,
                renewed_at: now,
                ending: ReplicaEnding::Pending,
            });
        if lease.lease_expires_at_ms != reading.lease_expires_at_ms {
            lease.lease_expires_at_ms = reading.lease_expires_at_ms;
            lease.renewed_at = now;
        }
        lease.ending = match reading.completed {
            Some(ended) => ReplicaEnding::Completed(ended),
            None if now.saturating_duration_since(lease.renewed_at) >= lapse_after => {
                ReplicaEnding::Lapsed
            }
            None => ReplicaEnding::Pending,
        };
    }
    // A lease no longer read was released, or ended long enough before the
    // request no longer to count.
    for (replica, lease) in observed.iter_mut() {
        if !read.contains(replica) && !matches!(lease.ending, ReplicaEnding::Completed(_)) {
            lease.ending = ReplicaEnding::Lapsed;
        }
    }
}

fn report_of(
    request: &SessionEndingRequest,
    observed: &BTreeMap<Uuid, ObservedLease>,
) -> SessionEndingReport {
    SessionEndingReport {
        request: request.id,
        replicas: observed
            .iter()
            .map(|(replica, lease)| ReplicaReport {
                replica: *replica,
                node: lease.node.clone(),
                ending: lease.ending,
            })
            .collect(),
    }
}

/// Wait, for at most `limit`, until every replica whose lease counts has
/// completed `request` or has stopped renewing its lease. The last reading
/// is returned either way; a reading that fails is returned at once, and
/// the request stays recorded.
pub async fn await_session_endings(
    db: &Database,
    request: &SessionEndingRequest,
    limit: Duration,
) -> Result<SessionEndingReport, SessionEndingError> {
    let started = Instant::now();
    let deadline = started + limit;
    let mut observed = BTreeMap::new();
    loop {
        let readings = read_replicas(db, request).await?;
        let now = Instant::now();
        observe(&mut observed, readings, now);
        let report = report_of(request, &observed);
        let observed_long_enough = now.saturating_duration_since(started) >= MINIMUM_OBSERVATION;
        if (report.confirmed() && observed_long_enough) || now >= deadline {
            return Ok(report);
        }
        tokio::time::sleep(AWAIT_POLL.min(deadline.saturating_duration_since(now))).await;
    }
}

/// Remove the leases that ended more than a day ago, the requests older
/// than twice the window, and the acknowledgements of both. Owner only: the
/// runtime role may not delete.
pub async fn prune_session_endings(
    db: &Database,
) -> Result<SessionEndingPrune, SessionEndingError> {
    let now = now_ms();
    let lease_cutoff = now.saturating_sub(millis(LEASE_RETENTION));
    let request_cutoff = now.saturating_sub(millis(REQUEST_RETENTION));
    let mut tx = begin_bounded(db).await?;
    let mut acknowledgements = tx
        .execute(
            "DELETE FROM account_session_ending_acks WHERE request_id IN \
             (SELECT request_id FROM account_session_ending_requests WHERE requested_at_ms < ?)",
            crate::db_params![request_cutoff],
        )
        .await?;
    acknowledgements += tx
        .execute(
            "DELETE FROM account_session_ending_acks WHERE replica_id IN \
             (SELECT replica_id FROM account_session_ending_replicas \
             WHERE lease_expires_at_ms < ?)",
            crate::db_params![lease_cutoff],
        )
        .await?;
    let requests = tx
        .execute(
            "DELETE FROM account_session_ending_requests WHERE requested_at_ms < ?",
            crate::db_params![request_cutoff],
        )
        .await?;
    let leases = tx
        .execute(
            "DELETE FROM account_session_ending_replicas WHERE lease_expires_at_ms < ?",
            crate::db_params![lease_cutoff],
        )
        .await?;
    tx.commit().await?;
    Ok(SessionEndingPrune {
        leases,
        requests,
        acknowledgements,
    })
}

/// Register the lease of `replica`, named `node` in reports, for
/// [`SESSION_ENDING_LEASE_TTL`]. Registering again after an unknown outcome
/// renews the row the first attempt wrote.
pub async fn register_replica(
    db: &Database,
    replica: Uuid,
    node: &str,
) -> Result<(), SessionEndingError> {
    let now = now_ms();
    execute_one(
        db,
        "INSERT INTO account_session_ending_replicas \
         (replica_id, node, registered_at_ms, lease_expires_at_ms) VALUES (?, ?, ?, ?) \
         ON CONFLICT (replica_id) DO UPDATE SET \
         lease_expires_at_ms = excluded.lease_expires_at_ms",
        crate::db_params![
            replica.simple().to_string(),
            node,
            now,
            now.saturating_add(millis(SESSION_ENDING_LEASE_TTL))
        ],
    )
    .await?;
    Ok(())
}

/// Renew the lease of `replica` for [`SESSION_ENDING_LEASE_TTL`]. `false`
/// when no row of it is left: the owner removed a lease that had ended.
pub async fn renew_replica(db: &Database, replica: Uuid) -> Result<bool, SessionEndingError> {
    let renewed = execute_one(
        db,
        "UPDATE account_session_ending_replicas SET lease_expires_at_ms = ? \
         WHERE replica_id = ?",
        crate::db_params![
            now_ms().saturating_add(millis(SESSION_ENDING_LEASE_TTL)),
            replica.simple().to_string()
        ],
    )
    .await?;
    Ok(renewed > 0)
}

/// End the lease of `replica` now, so that no command waits for it. The
/// replica renews it no more.
pub async fn release_replica(db: &Database, replica: Uuid) -> Result<(), SessionEndingError> {
    execute_one(
        db,
        "UPDATE account_session_ending_replicas SET lease_expires_at_ms = 0 \
         WHERE replica_id = ?",
        crate::db_params![replica.simple().to_string()],
    )
    .await?;
    Ok(())
}

/// The requests of the last [`SESSION_ENDING_WINDOW`] that `replica` has not
/// completed and whose account the ledger of retired JIDs holds, oldest
/// first.
pub async fn requests_to_complete(
    db: &Database,
    replica: Uuid,
) -> Result<Vec<SessionEndingRequest>, SessionEndingError> {
    let since = now_ms().saturating_sub(millis(SESSION_ENDING_WINDOW));
    let mut tx = begin_bounded(db).await?;
    let mut rows = tx
        .query(
            "SELECT q.request_id, q.account_jid, q.requested_at_ms \
             FROM account_session_ending_requests q \
             JOIN retired_account_jids l ON l.jid_key = q.account_jid \
             WHERE q.requested_at_ms > ? AND NOT EXISTS ( \
             SELECT 1 FROM account_session_ending_acks a \
             WHERE a.request_id = q.request_id AND a.replica_id = ? \
             AND a.completed_at_ms IS NOT NULL) \
             ORDER BY q.requested_at_ms, q.request_id LIMIT ?",
            crate::db_params![since, replica.simple().to_string(), REQUESTS_PER_TICK],
        )
        .await?;
    let mut requests = Vec::new();
    while let Some(row) = rows.next().await? {
        let id = stored_id(&row.get::<String>(0)?, "request_id")?;
        let account = row
            .get::<String>(1)?
            .parse::<BareJid>()
            .map_err(|_| SessionEndingError::Corrupt("account_jid"))?;
        requests.push(SessionEndingRequest {
            id,
            account,
            requested_at_ms: row.get::<i64>(2)?,
        });
    }
    drop(rows);
    tx.rollback().await?;
    Ok(requests)
}

/// Record that `replica` has taken up `request`: the account's bindings are
/// refused there from now on.
pub async fn record_observed(
    db: &Database,
    request: Uuid,
    replica: Uuid,
) -> Result<(), SessionEndingError> {
    execute_one(
        db,
        "INSERT INTO account_session_ending_acks (request_id, replica_id, observed_at_ms) \
         VALUES (?, ?, ?) ON CONFLICT (request_id, replica_id) DO NOTHING",
        crate::db_params![
            request.simple().to_string(),
            replica.simple().to_string(),
            now_ms()
        ],
    )
    .await?;
    Ok(())
}

/// Record that `replica` has completed `request`, having ended `ended`.
pub async fn record_completed(
    db: &Database,
    request: Uuid,
    replica: Uuid,
    ended: EndedSessions,
) -> Result<(), SessionEndingError> {
    let now = now_ms();
    execute_one(
        db,
        "INSERT INTO account_session_ending_acks (request_id, replica_id, observed_at_ms, \
         completed_at_ms, streams_closed, detached_sessions_ended) VALUES (?, ?, ?, ?, ?, ?) \
         ON CONFLICT (request_id, replica_id) DO UPDATE SET \
         completed_at_ms = excluded.completed_at_ms, \
         streams_closed = excluded.streams_closed, \
         detached_sessions_ended = excluded.detached_sessions_ended",
        crate::db_params![
            request.simple().to_string(),
            replica.simple().to_string(),
            now,
            now,
            count(ended.streams_closed),
            count(ended.detached_sessions_ended)
        ],
    )
    .await?;
    Ok(())
}
