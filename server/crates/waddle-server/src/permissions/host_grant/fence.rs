//! The acknowledged revocation fence of the host-grant authority.
//!
//! A change of a scope's grants takes effect at its PostgreSQL commit: an
//! acceptance transaction takes the scope's guard, and every other decision
//! reads the canonical grants from PostgreSQL when it is made. What the
//! commit alone does not cover is a decision read before it whose effect has
//! not happened yet, on any replica. The fence covers that:
//!
//! - A replica decides for a managed room only while it holds a *lease*
//!   ([`ReplicaFence`]): a row of `host_grant_fence_replicas` it renews every
//!   [`FENCE_TICK`]. It measures the lease on its own boot-time clock, which
//!   counts time the host spent suspended (`boot_clock`), from before the
//!   renewal was sent, and stops deciding [`FENCE_LEASE_MARGIN`] before the
//!   lease PostgreSQL recorded runs out. A renewal that comes back after the
//!   lease it renewed ran out here does not revive it: the replica treats it
//!   as a lapse and registers again.
//! - Each tick the replica reads every scope's generation and *installs* the
//!   ones that moved, in memory and under the lock every decision's validity
//!   check takes, and only then records them in `host_grant_fence_acks`.
//! - A decision carries a [`FenceTicket`]: the registration and the scope
//!   generation it was read under. [`FenceTicket::is_current`] and
//!   [`FenceTicket::admit`] refuse it once the replica has installed a newer
//!   generation of its scope, registered again, or let its lease lapse.
//!   A check never waits. A surface that holds a decision across an await
//!   asks it again before the effect, and decides again once its ticket is
//!   no longer current ([`super::HostGrantGate::still_allows`]).
//! - A revoke is *acknowledged* ([`HostGrantStore::await_revocation_fence`])
//!   once every replica whose lease has not been expired for
//!   [`FENCE_EXPIRY_GRACE`] has recorded the revoke's generation. The set of
//!   replicas is read after the revoke committed.
//!
//! So after the acknowledgement no replica passes a validity check with a
//! decision read before the revoke: a replica that acknowledged installed the
//! generation first, and one that did not has stopped deciding. A replica
//! whose lease lapsed registers again under a new identity and starts from
//! nothing it decided before.
//!
//! The lease's end is PostgreSQL's clock and the replica's deadline its own:
//! the margin absorbs a difference in their rates, not in their readings.
//! The leases of every replica are compared with one database clock, so a
//! failover to a PostgreSQL host whose clock runs ahead ends every lease
//! early by the difference. PostgreSQL hosts are therefore kept in sync
//! (NTP, skew well under a second), and each registration and renewal
//! compares the replica's wall clock with PostgreSQL's `clock_timestamp()`
//! and warns when they differ by more than [`FENCE_CLOCK_SKEW_WARNING`].
//!
//! The acknowledgement depends on PostgreSQL alone. It does not wait for the
//! projection, for the room actors that seat occupants or for the media
//! server: ejecting a seated occupant the grants no longer admit is work the
//! replica that hosts the room starts when it installs the generation (the
//! sweep a server installs with [`ReplicaFence::install_sweep`]).
//!
//! Log lines carry no JID, room, Space or scope identifier.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, PoisonError, RwLock, Weak};
use std::time::{Duration, Instant};

use super::boot_clock::{suspend_counted, BootInstant};

use tokio::sync::{watch, Notify};
use uuid::Uuid;

use super::enrolled::EnrolledScopes;
use super::error::HostGrantError;
use super::model::{GrantGeneration, ScopeLifecycleId};
use super::rate_limited::RateLimitedWarning;
use super::runtime_attach::HostGrantAttachmentGate;
use super::store::{corrupt, HostGrantStore};
use crate::db::Transaction;
use crate::ingress_substrate::set_local_transaction_timeouts;

/// How long a lease lasts in PostgreSQL after the renewal that set it.
pub const FENCE_LEASE_TTL: Duration = Duration::from_secs(5);
/// How long before the recorded end of its lease a replica stops deciding.
/// It absorbs the difference in rate between the replica's clock and the
/// database's over one lease.
pub const FENCE_LEASE_MARGIN: Duration = Duration::from_secs(1);
/// How long after a lease's recorded end a revoke still waits for its
/// replica. It covers the step from a validity check to the effect it
/// guards.
pub const FENCE_EXPIRY_GRACE: Duration = Duration::from_secs(1);
/// The difference between this replica's wall clock and PostgreSQL's above
/// which a registration or renewal warns. The fence measures leases on the
/// database's clock and the replica's own deadline on a relative one, so a
/// difference in readings alone does not void a lease; it shows a host that
/// is not kept in sync, whose clock can also step or drift.
pub const FENCE_CLOCK_SKEW_WARNING: Duration = Duration::from_millis(500);
/// How often a replica renews its lease and reads the scopes' generations.
pub const FENCE_TICK: Duration = Duration::from_millis(250);
/// Longest a caller waits for a revoke's acknowledgement: a replica that
/// stopped without releasing its lease is waited for until the lease and
/// the grace have run out, and a live one records the generation within a
/// tick.
pub const FENCE_ACKNOWLEDGEMENT_DEADLINE: Duration = Duration::from_secs(10);

/// How often a caller that waits for an acknowledgement reads it again.
const ACKNOWLEDGEMENT_POLL: Duration = Duration::from_millis(40);
/// How long a decision waits for the replica's first lease.
const FIRST_LEASE_WAIT: Duration = Duration::from_secs(2);
/// How long a decision waits for a replica whose lease lapsed to register
/// again: two ticks.
const LAPSED_LEASE_WAIT: Duration = Duration::from_millis(500);
/// Longest a fence statement waits for a lock, and longest it runs.
const FENCE_LOCK_TIMEOUT: Duration = Duration::from_millis(500);
const FENCE_STATEMENT_TIMEOUT: Duration = Duration::from_secs(1);
/// What the tick deadline leaves of the margin and the grace together.
const FENCE_TICK_SLACK: Duration = Duration::from_millis(500);
/// Longest each of one tick's transactions takes, connection checkout
/// included: a transaction that overruns it is abandoned, and the lease
/// then runs out on its own.
///
/// It is shorter than the margin and the grace together, with
/// [`FENCE_TICK_SLACK`] to spare. A renewal sent before the replica's own
/// deadline therefore commits before a revoke can stop waiting for the
/// lease it renews: that lease ends no earlier than the margin after the
/// replica's deadline, and is waited for the grace beyond. A renewal whose
/// generations were read before such a revoke committed would otherwise
/// extend a lease the acknowledgement no longer counts.
const FENCE_TICK_DEADLINE: Duration = Duration::from_millis(1_500);
const _: () = assert!(
    FENCE_TICK_SLACK.as_millis() > 0
        && FENCE_TICK_DEADLINE.as_millis() + FENCE_TICK_SLACK.as_millis()
            <= FENCE_LEASE_MARGIN.as_millis() + FENCE_EXPIRY_GRACE.as_millis()
);

/// At most one warning a minute that the replica's lease lapsed.
static LAPSED: RateLimitedWarning = RateLimitedWarning::new();
/// At most one warning a minute that a tick failed.
static TICK_FAILED: RateLimitedWarning = RateLimitedWarning::new();
/// At most one warning a minute that PostgreSQL's clock and this replica's
/// differ.
static CLOCK_SKEW: RateLimitedWarning = RateLimitedWarning::new();

/// Whether every replica that may still decide has recorded a generation of
/// a scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FenceAcknowledgement {
    /// Every replica whose lease counts has recorded the generation.
    /// `replicas` is how many there are.
    Acknowledged { replicas: u32 },
    /// `lagging` replicas hold a lease that counts and have not recorded
    /// the generation.
    Pending { replicas: u32, lagging: u32 },
}

impl FenceAcknowledgement {
    pub fn acknowledged(self) -> bool {
        matches!(self, Self::Acknowledged { .. })
    }
}

impl HostGrantStore {
    async fn begin_fence(&self) -> Result<Transaction<'_>, HostGrantError> {
        let mut tx = self.database().begin().await?;
        if !set_local_transaction_timeouts(&mut tx, FENCE_LOCK_TIMEOUT, FENCE_STATEMENT_TIMEOUT)
            .await?
        {
            return Err(HostGrantError::BoundsUnproven);
        }
        Ok(tx)
    }

    /// Record a new replica with a lease of [`FENCE_LEASE_TTL`] and read
    /// every scope's generation, in that order: the generations are read
    /// after the registration is visible to the statement, and the caller
    /// decides nothing before this returns.
    async fn fence_register(
        &self,
        replica: Uuid,
    ) -> Result<Vec<(ScopeLifecycleId, GrantGeneration)>, HostGrantError> {
        let mut tx = self.begin_fence().await?;
        let sent_ms = chrono::Utc::now().timestamp_millis();
        let mut rows = tx
            .query(
                "INSERT INTO host_grant_fence_replicas (replica_id, registered_at_ms, \
                 lease_expires_at) VALUES (?, ?, now() + (? * interval '1 millisecond')) \
                 RETURNING (extract(epoch FROM clock_timestamp()) * 1000)::bigint",
                crate::db_params![replica.simple().to_string(), sent_ms, lease_ttl_ms()],
            )
            .await?;
        let database_ms = match rows.next().await? {
            Some(row) => Some(row.get::<i64>(0)?),
            None => None,
        };
        drop(rows);
        observe_clock_skew(database_ms, sent_ms, chrono::Utc::now().timestamp_millis());
        tx.commit().await?;
        // A snapshot of its own, taken after the registration committed.
        let mut tx = self.begin_fence().await?;
        let generations = scope_generations(&mut tx).await?;
        tx.rollback().await?;
        Ok(generations)
    }

    /// Renew a replica's lease and read every scope's generation. `None`
    /// when PostgreSQL holds no lease of the replica that has not run out:
    /// a revoke may already have stopped waiting for it.
    async fn fence_renew(
        &self,
        replica: Uuid,
    ) -> Result<Option<Vec<(ScopeLifecycleId, GrantGeneration)>>, HostGrantError> {
        let mut tx = self.begin_fence().await?;
        let sent_ms = chrono::Utc::now().timestamp_millis();
        let mut rows = tx
            .query(
                "UPDATE host_grant_fence_replicas \
                 SET lease_expires_at = now() + (? * interval '1 millisecond') \
                 WHERE replica_id = ? AND lease_expires_at > now() \
                 RETURNING (extract(epoch FROM clock_timestamp()) * 1000)::bigint",
                crate::db_params![lease_ttl_ms(), replica.simple().to_string()],
            )
            .await?;
        let renewed = match rows.next().await? {
            Some(row) => Some(row.get::<i64>(0)?),
            None => None,
        };
        drop(rows);
        let Some(database_ms) = renewed else {
            tx.rollback().await?;
            return Ok(None);
        };
        observe_clock_skew(
            Some(database_ms),
            sent_ms,
            chrono::Utc::now().timestamp_millis(),
        );
        let generations = scope_generations(&mut tx).await?;
        tx.commit().await?;
        Ok(Some(generations))
    }

    /// Record the generations a replica has installed. A recorded
    /// generation never goes back.
    async fn fence_acknowledge(
        &self,
        replica: Uuid,
        installed: &[(ScopeLifecycleId, GrantGeneration)],
    ) -> Result<(), HostGrantError> {
        if installed.is_empty() {
            return Ok(());
        }
        let now = chrono::Utc::now().timestamp_millis();
        let replica = replica.simple().to_string();
        let mut tx = self.begin_fence().await?;
        for (lifecycle, generation) in installed {
            tx.execute(
                "INSERT INTO host_grant_fence_acks (replica_id, lifecycle_id, generation, \
                 acknowledged_at_ms) VALUES (?, ?, ?, ?) \
                 ON CONFLICT (replica_id, lifecycle_id) DO UPDATE SET \
                 generation = EXCLUDED.generation, \
                 acknowledged_at_ms = EXCLUDED.acknowledged_at_ms \
                 WHERE host_grant_fence_acks.generation < EXCLUDED.generation",
                crate::db_params![
                    replica.as_str(),
                    lifecycle.to_storage(),
                    generation.value(),
                    now
                ],
            )
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// End a replica's lease now, so that no revoke waits for it. The
    /// caller has stopped deciding under the lease before it calls this.
    async fn fence_release(&self, replica: Uuid) -> Result<(), HostGrantError> {
        let mut tx = self.begin_fence().await?;
        tx.execute(
            "UPDATE host_grant_fence_replicas \
             SET lease_expires_at = now() - (? * interval '1 millisecond') \
             WHERE replica_id = ? \
             AND lease_expires_at > now() - (? * interval '1 millisecond')",
            crate::db_params![
                expiry_grace_ms(),
                replica.simple().to_string(),
                expiry_grace_ms()
            ],
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Whether every replica that may still decide has recorded `generation`
    /// of `lifecycle`, as of now. A replica counts until its lease has been
    /// expired for [`FENCE_EXPIRY_GRACE`]. The caller reads this after the
    /// operation that recorded `generation` committed, so every replica that
    /// decided anything before that commit is among those counted or has
    /// stopped deciding.
    pub async fn revocation_fence(
        &self,
        lifecycle: ScopeLifecycleId,
        generation: GrantGeneration,
    ) -> Result<FenceAcknowledgement, HostGrantError> {
        let mut tx = self.begin_fence().await?;
        let mut rows = tx
            .query(
                "SELECT count(*), \
                 count(*) FILTER (WHERE a.generation IS NULL OR a.generation < ?) \
                 FROM host_grant_fence_replicas r \
                 LEFT JOIN host_grant_fence_acks a \
                 ON a.replica_id = r.replica_id AND a.lifecycle_id = ? \
                 WHERE r.lease_expires_at + (? * interval '1 millisecond') > now()",
                crate::db_params![
                    generation.value(),
                    lifecycle.to_storage(),
                    expiry_grace_ms()
                ],
            )
            .await?;
        let (replicas, lagging) = match rows.next().await? {
            Some(row) => (row.get::<i64>(0)?, row.get::<i64>(1)?),
            None => (0, 0),
        };
        drop(rows);
        tx.rollback().await?;
        let replicas = u32::try_from(replicas).map_err(|_| corrupt("fence replicas"))?;
        let lagging = u32::try_from(lagging).map_err(|_| corrupt("fence replicas"))?;
        Ok(if lagging == 0 {
            FenceAcknowledgement::Acknowledged { replicas }
        } else {
            FenceAcknowledgement::Pending { replicas, lagging }
        })
    }

    /// Wait until `generation` of `lifecycle` is acknowledged, for at most
    /// `limit`. The last reading is returned either way; a reading that
    /// fails is returned at once, and the operation it follows stays
    /// committed.
    pub async fn await_revocation_fence(
        &self,
        lifecycle: ScopeLifecycleId,
        generation: GrantGeneration,
        limit: Duration,
    ) -> Result<FenceAcknowledgement, HostGrantError> {
        let deadline = Instant::now() + limit;
        loop {
            let reading = self.revocation_fence(lifecycle, generation).await?;
            if reading.acknowledged() || Instant::now() >= deadline {
                return Ok(reading);
            }
            tokio::time::sleep(ACKNOWLEDGEMENT_POLL).await;
        }
    }

    /// Remove the leases that ran out more than `older_than` ago, with their
    /// acknowledgements. Owner phase only: the runtime role may not delete.
    pub async fn prune_fence_replicas(&self, older_than: Duration) -> Result<u64, HostGrantError> {
        let age = i64::try_from(older_than.as_millis()).unwrap_or(i64::MAX);
        let mut tx = self.begin_fence().await?;
        tx.execute(
            "DELETE FROM host_grant_fence_acks a USING host_grant_fence_replicas r \
             WHERE a.replica_id = r.replica_id \
             AND r.lease_expires_at < now() - (? * interval '1 millisecond')",
            crate::db_params![age],
        )
        .await
        .map_err(HostGrantError::from_database)?;
        let pruned = tx
            .execute(
                "DELETE FROM host_grant_fence_replicas \
                 WHERE lease_expires_at < now() - (? * interval '1 millisecond')",
                crate::db_params![age],
            )
            .await
            .map_err(HostGrantError::from_database)?;
        tx.commit().await?;
        Ok(pruned)
    }
}

fn lease_ttl_ms() -> i64 {
    i64::try_from(FENCE_LEASE_TTL.as_millis()).unwrap_or(i64::MAX)
}

/// Record how far PostgreSQL's clock is from this replica's wall clock, from
/// one statement that read `clock_timestamp()` (`database_ms`) between the
/// replica's readings `sent_ms` and `received_ms`. The difference is
/// estimated from the middle of that round trip; a warning needs it to
/// exceed [`FENCE_CLOCK_SKEW_WARNING`] by more than half the round trip, so a
/// slow round trip alone does not warn.
fn observe_clock_skew(database_ms: Option<i64>, sent_ms: i64, received_ms: i64) {
    let Some(database_ms) = database_ms else {
        return;
    };
    let round_trip = received_ms.saturating_sub(sent_ms).unsigned_abs();
    let midpoint = sent_ms.saturating_add(received_ms.saturating_sub(sent_ms) / 2);
    let skew = database_ms.saturating_sub(midpoint).unsigned_abs();
    waddle_xmpp::gauge_record!(
        "waddle.host_grant.fence.clock_skew",
        "ms",
        "Difference between this replica's wall clock and PostgreSQL's clock_timestamp(), \
         measured at its last host-grant revocation fence registration or renewal",
        skew,
    );
    let threshold = u64::try_from(FENCE_CLOCK_SKEW_WARNING.as_millis()).unwrap_or(u64::MAX);
    if skew.saturating_sub(round_trip / 2) > threshold && CLOCK_SKEW.due() {
        tracing::warn!(
            kind = "clock_skew",
            skew_ms = skew,
            round_trip_ms = round_trip,
            "PostgreSQL's clock and this replica's differ by more than the host-grant \
             revocation fence allows for; keep the database hosts in sync"
        );
    }
}

fn expiry_grace_ms() -> i64 {
    i64::try_from(FENCE_EXPIRY_GRACE.as_millis()).unwrap_or(i64::MAX)
}

async fn scope_generations(
    tx: &mut Transaction<'_>,
) -> Result<Vec<(ScopeLifecycleId, GrantGeneration)>, HostGrantError> {
    let mut rows = tx
        .query("SELECT lifecycle_id, generation FROM host_grant_scopes", ())
        .await?;
    let mut generations = Vec::new();
    while let Some(row) = rows.next().await? {
        let lifecycle = ScopeLifecycleId::from_storage(&row.get::<String>(0)?)
            .ok_or(corrupt("lifecycle_id"))?;
        let generation = GrantGeneration::new(row.get::<i64>(1)?).ok_or(corrupt("generation"))?;
        generations.push((lifecycle, generation));
    }
    Ok(generations)
}

/// What a replica asks of its server once it has installed a scope's new
/// generation: put what the server derived from the scope's grants (seated
/// occupants, their roles, their media sessions) back in line with them.
/// The call must not block; the work is the server's own, with its own
/// retries, and the acknowledgement does not wait for it.
pub type RevocationSweep = Arc<dyn Fn(ScopeLifecycleId) + Send + Sync>;

/// The lease a replica holds.
#[derive(Clone, Copy, Debug)]
struct Lease {
    replica: Uuid,
    /// Until when, on the replica's boot-time clock, it may decide.
    valid_until: BootInstant,
}

impl Lease {
    /// The lease of `replica` renewed by a statement sent at `sent`: it may
    /// decide until [`FENCE_LEASE_MARGIN`] before the end PostgreSQL
    /// records, measured from `sent`.
    fn from(replica: Uuid, sent: BootInstant) -> Option<Self> {
        let valid_until = sent.checked_add(FENCE_LEASE_TTL.saturating_sub(FENCE_LEASE_MARGIN))?;
        Some(Self {
            replica,
            valid_until,
        })
    }

    /// Whether the replica may still decide under the lease; never when
    /// the clock cannot be read.
    fn valid(&self) -> bool {
        BootInstant::now().is_some_and(|now| now < self.valid_until)
    }
}

#[derive(Debug, Default)]
struct FenceState {
    /// Counts registrations: a decision read under an earlier one is void.
    registration: u64,
    lease: Option<Lease>,
    /// The generation installed for each scope under this registration.
    installed: HashMap<ScopeLifecycleId, GrantGeneration>,
    /// The generation recorded in PostgreSQL for each scope under this
    /// registration.
    recorded: HashMap<ScopeLifecycleId, GrantGeneration>,
}

impl FenceState {
    fn lease_valid(&self) -> bool {
        self.lease.is_some_and(|lease| lease.valid())
    }
}

struct Shared {
    store: HostGrantStore,
    enrolled: EnrolledScopes,
    attachment: Arc<OnceLock<Arc<HostGrantAttachmentGate>>>,
    /// Set when another attachment gate than the bound one was offered: the
    /// replica's surfaces then decide no managed room, so neither does the
    /// fence.
    conflicting: Arc<AtomicBool>,
    /// The lock every validity check reads under and every installation
    /// writes under. It is never held across an await.
    state: RwLock<FenceState>,
    /// Counts ticks, so a decision can wait for the first lease.
    ticks: watch::Sender<u64>,
    wake: Arc<Notify>,
    started: AtomicBool,
    /// Whether the background task skips its ticks (test support).
    suspended: AtomicBool,
    sweep: OnceLock<RevocationSweep>,
    /// Holds the answer of the next renewal until the sender fires or is
    /// dropped, as a slow network would (test support).
    #[cfg(feature = "test-support")]
    held_renewal: std::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
}

impl Shared {
    fn read(&self) -> std::sync::RwLockReadGuard<'_, FenceState> {
        self.state.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, FenceState> {
        self.state.write().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether this replica decides for managed rooms at all: its bound
    /// attachment gate is attached, as the surfaces require
    /// ([`HostGrantAttachmentGate::verified_attachment`]), and the enrolled
    /// set names a scope. While the gate verifies a changed authority again,
    /// or after that verification failed, the lease is released, so every
    /// ticket read before it stops being current with the surfaces.
    fn serves(&self) -> bool {
        let attached = !self.conflicting.load(Ordering::Acquire)
            && self
                .attachment
                .get()
                .is_some_and(|gate| gate.verified_attachment().is_some());
        attached && self.enrolled.snapshot().is_some_and(|set| !set.is_empty())
    }

    /// Stop deciding under the current lease, if any, and say which replica
    /// held it.
    fn lapse(&self) -> Option<Uuid> {
        let mut state = self.write();
        let lease = state.lease.take()?;
        state.installed.clear();
        state.recorded.clear();
        Some(lease.replica)
    }

    fn request_sweep(&self, lifecycle: ScopeLifecycleId) {
        if let Some(sweep) = self.sweep.get() {
            sweep(lifecycle);
        }
    }

    async fn tick(&self) {
        if !self.serves() {
            if let Some(replica) = self.lapse() {
                // Nothing is decided here any more, so no revoke needs to
                // wait for this replica.
                let _ = self.store.fence_release(replica).await;
            }
            return;
        }
        let lease = self.read().lease;
        let Some(sent) = BootInstant::now() else {
            // Without the clock no lease can be measured: nothing is
            // decided here until it can be read again.
            if let Some(replica) = self.lapse() {
                self.note_lapse("clock_unreadable");
                let _ = self.store.fence_release(replica).await;
            }
            return;
        };
        let outcome = match lease {
            Some(lease) if sent < lease.valid_until => self.renew(lease.replica, sent).await,
            Some(lease) => {
                // The lease ran out here before it could be renewed. A
                // revoke may stop waiting for it at any moment, so nothing
                // decided under it is kept, and none needs to wait for it.
                self.lapse();
                self.note_lapse("expired");
                let _ = self.store.fence_release(lease.replica).await;
                self.register(sent).await
            }
            None => self.register(sent).await,
        };
        match outcome {
            Ok(()) => {}
            Err(error) => {
                if TICK_FAILED.due() {
                    tracing::warn!(
                        kind = error.kind(),
                        "The host-grant revocation fence could not renew this replica's lease"
                    );
                }
            }
        }
    }

    fn note_lapse(&self, kind: &'static str) {
        waddle_xmpp::counter_add!(
            "waddle.host_grant.fence.lease_lapsed",
            "1",
            "Times this replica's host-grant revocation fence lease ran out; rooms of \
             enrolled scopes are refused here until it registers again",
            1,
        );
        if LAPSED.due() {
            tracing::warn!(
                kind,
                "This replica's host-grant revocation fence lease lapsed; rooms of enrolled \
                 scopes are unavailable here until it registers again"
            );
        }
    }

    /// Wait for the sender of a held renewal, if a test holds one.
    #[cfg(feature = "test-support")]
    async fn hold_renewal(&self) {
        let held = self
            .held_renewal
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(held) = held {
            let _ = held.await;
        }
    }

    async fn register(&self, sent: BootInstant) -> Result<(), HostGrantError> {
        let replica = Uuid::new_v4();
        let generations =
            tokio::time::timeout(FENCE_TICK_DEADLINE, self.store.fence_register(replica))
                .await
                .map_err(|_| HostGrantError::Busy)??;
        let Some(lease) = Lease::from(replica, sent) else {
            let _ = self.store.fence_release(replica).await;
            return Err(HostGrantError::Busy);
        };
        {
            let mut state = self.write();
            state.registration = state.registration.saturating_add(1);
            state.installed = generations.iter().copied().collect();
            state.recorded.clear();
            state.lease = Some(lease);
        }
        // Whatever this replica derived from the grants before it held this
        // lease was derived without the fence.
        for (lifecycle, _) in &generations {
            self.request_sweep(*lifecycle);
        }
        self.record(replica).await
    }

    async fn renew(&self, replica: Uuid, sent: BootInstant) -> Result<(), HostGrantError> {
        let renewed = tokio::time::timeout(FENCE_TICK_DEADLINE, self.store.fence_renew(replica))
            .await
            .map_err(|_| HostGrantError::Busy)??;
        #[cfg(feature = "test-support")]
        self.hold_renewal().await;
        let Some(generations) = renewed else {
            self.lapse();
            self.note_lapse("not_renewed");
            return Ok(());
        };
        let Some(renewed_lease) = Lease::from(replica, sent) else {
            return Err(HostGrantError::Busy);
        };
        let mut moved = Vec::new();
        let renewed_in_time = {
            let mut state = self.write();
            // Another tick may have replaced the lease meanwhile.
            let Some(lease) = state.lease.filter(|lease| lease.replica == replica) else {
                return Ok(());
            };
            let in_time = lease.valid();
            if in_time {
                for (lifecycle, generation) in generations {
                    if state
                        .installed
                        .get(&lifecycle)
                        .is_none_or(|installed| *installed < generation)
                    {
                        state.installed.insert(lifecycle, generation);
                        moved.push(lifecycle);
                    }
                }
                state.lease = Some(renewed_lease);
            }
            in_time
        };
        if !renewed_in_time {
            // The answer came back after the lease it renews ran out here.
            // Decisions under that lease have been void since, and the
            // generations this answer read may predate a revoke whose
            // acknowledgement stopped counting the lease before the renewal
            // committed: they are no ground to make those decisions current
            // again. The replica lapses and registers under a new
            // registration, which voids them for good.
            self.lapse();
            self.note_lapse("renewed_late");
            let _ = self.store.fence_release(replica).await;
            let sent = BootInstant::now().ok_or(HostGrantError::Busy)?;
            return self.register(sent).await;
        }
        for lifecycle in moved {
            self.request_sweep(lifecycle);
        }
        self.record(replica).await
    }

    /// Record in PostgreSQL every generation installed and not yet
    /// recorded. The installation comes first: a recorded generation is one
    /// no validity check passes an older decision against.
    async fn record(&self, replica: Uuid) -> Result<(), HostGrantError> {
        let pending: Vec<(ScopeLifecycleId, GrantGeneration)> = {
            let state = self.read();
            if state.lease.map(|lease| lease.replica) != Some(replica) {
                return Ok(());
            }
            state
                .installed
                .iter()
                .filter(|(lifecycle, generation)| {
                    state
                        .recorded
                        .get(*lifecycle)
                        .is_none_or(|recorded| recorded < *generation)
                })
                .map(|(lifecycle, generation)| (*lifecycle, *generation))
                .collect()
        };
        if pending.is_empty() {
            return Ok(());
        }
        tokio::time::timeout(
            FENCE_TICK_DEADLINE,
            self.store.fence_acknowledge(replica, &pending),
        )
        .await
        .map_err(|_| HostGrantError::Busy)??;
        let mut state = self.write();
        if state.lease.map(|lease| lease.replica) == Some(replica) {
            for (lifecycle, generation) in pending {
                let recorded = state.recorded.entry(lifecycle).or_insert(generation);
                if *recorded < generation {
                    *recorded = generation;
                }
            }
        }
        Ok(())
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        let lease = self
            .state
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .lease
            .take();
        let (Some(lease), Ok(runtime)) = (lease, tokio::runtime::Handle::try_current()) else {
            return;
        };
        let store = self.store.clone();
        runtime.spawn(async move {
            let _ = store.fence_release(lease.replica).await;
        });
    }
}

/// A replica's part of the revocation fence: its lease, the generations it
/// has installed and the validity check of every decision it makes for a
/// managed room. Cloning shares it; a replica has one, held by its
/// [`super::HostGrantGate`].
#[derive(Clone)]
pub struct ReplicaFence {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for ReplicaFence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.shared.read();
        f.debug_struct("ReplicaFence")
            .field("registration", &state.registration)
            .field("leased", &state.lease_valid())
            .finish_non_exhaustive()
    }
}

impl ReplicaFence {
    /// The fence of the replica whose canonical store is `store`, whose
    /// enrolled set is `enrolled` and whose attachment gate is the one
    /// `attachment` holds once bound; `conflicting` is the gate's flag that
    /// another attachment gate was offered. Nothing runs until
    /// [`Self::start`] or the first decision.
    pub(super) fn new(
        store: HostGrantStore,
        enrolled: EnrolledScopes,
        attachment: Arc<OnceLock<Arc<HostGrantAttachmentGate>>>,
        conflicting: Arc<AtomicBool>,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                store,
                enrolled,
                attachment,
                conflicting,
                state: RwLock::default(),
                ticks: watch::Sender::new(0),
                wake: Arc::new(Notify::new()),
                started: AtomicBool::new(false),
                suspended: AtomicBool::new(false),
                sweep: OnceLock::new(),
                #[cfg(feature = "test-support")]
                held_renewal: std::sync::Mutex::new(None),
            }),
        }
    }

    /// Keep the replica's lease from a background task of the current Tokio
    /// runtime: register when the replica serves an enrolled scope, renew
    /// every [`FENCE_TICK`], install the generations that moved and record
    /// them. The task stops once every handle is dropped. Outside a runtime
    /// nothing runs and the replica decides nothing for a managed room.
    pub fn start(&self) {
        if self.shared.started.swap(true, Ordering::AcqRel) {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            self.shared.started.store(false, Ordering::Release);
            return;
        };
        if !suspend_counted() {
            tracing::warn!(
                kind = "lease_clock",
                "The host-grant revocation fence measures leases on a clock that stops while \
                 this host is suspended; production replicas run on Linux, where it does not"
            );
        }
        runtime.spawn(keep_lease(
            Arc::downgrade(&self.shared),
            Arc::clone(&self.shared.wake),
        ));
    }

    /// Have the server put what it derived from a scope's grants back in
    /// line with them whenever this replica installs a new generation of
    /// the scope, and for every scope when it registers. Only the first
    /// sweep installed is kept.
    pub fn install_sweep(&self, sweep: RevocationSweep) -> bool {
        self.shared.sweep.set(sweep).is_ok()
    }

    /// Ask the server's sweep for one scope now.
    pub fn request_sweep(&self, lifecycle: ScopeLifecycleId) {
        self.shared.request_sweep(lifecycle);
    }

    /// Run a tick now instead of at the next interval: a replica that has
    /// just recorded a change installs and records it at once.
    pub fn wake(&self) {
        self.shared.wake.notify_one();
    }

    /// The registration a decision begins under, or `None` when the replica
    /// holds no lease it may decide under. The decision reads the grants
    /// after this returns, so everything it reads was read by a registered
    /// replica.
    pub(super) async fn begin_decision(&self) -> Option<u64> {
        if let Some(registration) = self.registration() {
            return Some(registration);
        }
        self.start();
        self.wake();
        // A replica that never held a lease is given time to register. One
        // whose lease lapsed registers again within a tick when it can; a
        // request does not wait longer than that for a database that does
        // not answer.
        let limit = if self.shared.read().registration == 0 {
            FIRST_LEASE_WAIT
        } else {
            LAPSED_LEASE_WAIT
        };
        let mut ticks = self.shared.ticks.subscribe();
        let leased = tokio::time::timeout(limit, async {
            loop {
                if let Some(registration) = self.registration() {
                    return Some(registration);
                }
                if ticks.changed().await.is_err() {
                    return None;
                }
            }
        })
        .await;
        leased.ok().flatten()
    }

    fn registration(&self) -> Option<u64> {
        let state = self.shared.read();
        state.lease_valid().then_some(state.registration)
    }

    /// The ticket of a decision that began under `registration` and read
    /// `lifecycle` at `generation`.
    pub(super) fn ticket(
        &self,
        registration: u64,
        lifecycle: ScopeLifecycleId,
        generation: GrantGeneration,
    ) -> FenceTicket {
        FenceTicket {
            fence: Some(Arc::clone(&self.shared)),
            registration,
            lifecycle,
            generation,
        }
    }

    /// Whether the replica holds a lease it may decide under.
    pub fn leased(&self) -> bool {
        self.shared.read().lease_valid()
    }

    /// The generation this replica has installed for a scope, if any.
    pub fn installed(&self, lifecycle: ScopeLifecycleId) -> Option<GrantGeneration> {
        self.shared.read().installed.get(&lifecycle).copied()
    }

    /// Stop or resume renewing. While renewing is stopped the lease runs
    /// out as that of a replica that stopped or lost its database would.
    #[cfg(feature = "test-support")]
    pub fn suspend_for_test(&self, suspended: bool) {
        self.shared.suspended.store(suspended, Ordering::Release);
    }

    /// Hold the answer of the next renewal, after PostgreSQL committed it,
    /// until the returned sender fires or is dropped: a renewal whose answer
    /// arrives late, as over a slow network.
    #[cfg(feature = "test-support")]
    pub fn hold_next_renewal_for_test(&self) -> tokio::sync::oneshot::Sender<()> {
        let (release, held) = tokio::sync::oneshot::channel();
        *self
            .shared
            .held_renewal
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(held);
        release
    }
}

async fn keep_lease(shared: Weak<Shared>, wake: Arc<Notify>) {
    loop {
        {
            let Some(shared) = shared.upgrade() else {
                return;
            };
            if !shared.suspended.load(Ordering::Acquire) {
                shared.tick().await;
            }
            shared
                .ticks
                .send_modify(|ticks| *ticks = ticks.wrapping_add(1));
        }
        tokio::select! {
            () = tokio::time::sleep(FENCE_TICK) => {}
            () = wake.notified() => {}
        }
    }
}

/// What a decision for a managed room was read under: the replica's
/// registration and the scope's generation. A decision is acted on only
/// while its ticket is current.
#[derive(Clone)]
pub struct FenceTicket {
    /// `None` for a decision no fence covers: a store read directly, never
    /// through a replica's gate.
    fence: Option<Arc<Shared>>,
    registration: u64,
    lifecycle: ScopeLifecycleId,
    generation: GrantGeneration,
}

impl std::fmt::Debug for FenceTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FenceTicket")
            .field("fenced", &self.fence.is_some())
            .field("registration", &self.registration)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl FenceTicket {
    /// The ticket of grants read from a store directly, outside any
    /// replica's gate. It is always current: nothing acknowledges for it.
    pub fn unfenced(lifecycle: ScopeLifecycleId, generation: GrantGeneration) -> Self {
        Self {
            fence: None,
            registration: 0,
            lifecycle,
            generation,
        }
    }

    /// The scope the decision was read for.
    pub fn lifecycle(&self) -> ScopeLifecycleId {
        self.lifecycle
    }

    fn current_in(&self, state: &FenceState) -> bool {
        state.lease_valid()
            && state.registration == self.registration
            && state
                .installed
                .get(&self.lifecycle)
                .is_none_or(|installed| *installed <= self.generation)
    }

    /// Whether the decision may still be acted on: the replica holds the
    /// lease it was read under and has installed no newer generation of its
    /// scope.
    pub fn is_current(&self) -> bool {
        match &self.fence {
            None => true,
            Some(shared) => self.current_in(&shared.read()),
        }
    }

    /// Run `effect` if the decision is still current, under the lock an
    /// installation takes: the replica records a newer generation only
    /// after `effect` has returned, or `effect` does not run. `effect`
    /// must not block.
    pub fn admit<R>(&self, effect: impl FnOnce() -> R) -> Option<R> {
        match &self.fence {
            None => Some(effect()),
            Some(shared) => {
                let state = shared.read();
                self.current_in(&state).then(effect)
            }
        }
    }

    /// Ask the replica's server to sweep the decision's scope: an effect
    /// that began under this ticket finished after the ticket stopped being
    /// current.
    pub fn request_sweep(&self) {
        if let Some(shared) = &self.fence {
            shared.request_sweep(self.lifecycle);
        }
    }
}
