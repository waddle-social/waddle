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
//!   [`FENCE_TICK`]. It measures the lease on its own monotonic clock from
//!   before the renewal was sent and stops deciding [`FENCE_LEASE_MARGIN`]
//!   before the lease PostgreSQL recorded runs out.
//! - Each tick the replica reads every scope's generation and *installs* the
//!   ones that moved, in memory and under the lock every decision's validity
//!   check takes, and only then records them in `host_grant_fence_acks`.
//! - A decision carries a [`FenceTicket`]: the registration and the scope
//!   generation it was read under. [`FenceTicket::is_current`] and
//!   [`FenceTicket::admit`] refuse it once the replica has installed a newer
//!   generation of its scope, registered again, or let its lease lapse.
//!   A check never waits and no ticket is held across an await.
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
/// Longest one tick's reads and writes take, connection checkout included:
/// a tick that overruns it is abandoned, and the lease then runs out on its
/// own.
const FENCE_TICK_DEADLINE: Duration = Duration::from_secs(2);

/// At most one warning a minute that the replica's lease lapsed.
static LAPSED: RateLimitedWarning = RateLimitedWarning::new();
/// At most one warning a minute that a tick failed.
static TICK_FAILED: RateLimitedWarning = RateLimitedWarning::new();

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
        tx.execute(
            "INSERT INTO host_grant_fence_replicas (replica_id, registered_at_ms, \
             lease_expires_at) VALUES (?, ?, now() + (? * interval '1 millisecond'))",
            crate::db_params![
                replica.simple().to_string(),
                chrono::Utc::now().timestamp_millis(),
                lease_ttl_ms()
            ],
        )
        .await?;
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
        let renewed = tx
            .execute(
                "UPDATE host_grant_fence_replicas \
                 SET lease_expires_at = now() + (? * interval '1 millisecond') \
                 WHERE replica_id = ? AND lease_expires_at > now()",
                crate::db_params![lease_ttl_ms(), replica.simple().to_string()],
            )
            .await?;
        if renewed == 0 {
            tx.rollback().await?;
            return Ok(None);
        }
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
    /// Until when, on the replica's monotonic clock, it may decide.
    valid_until: Instant,
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
        self.lease
            .is_some_and(|lease| Instant::now() < lease.valid_until)
    }
}

struct Shared {
    store: HostGrantStore,
    enrolled: EnrolledScopes,
    attachment: Arc<OnceLock<Arc<HostGrantAttachmentGate>>>,
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
}

impl Shared {
    fn read(&self) -> std::sync::RwLockReadGuard<'_, FenceState> {
        self.state.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, FenceState> {
        self.state.write().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether this replica decides for managed rooms at all: it holds a
    /// verified attachment and the enrolled set names a scope.
    fn serves(&self) -> bool {
        let attached = self
            .attachment
            .get()
            .is_some_and(|gate| gate.current().is_some());
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
        let sent = Instant::now();
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

    async fn register(&self, sent: Instant) -> Result<(), HostGrantError> {
        let replica = Uuid::new_v4();
        let generations =
            tokio::time::timeout(FENCE_TICK_DEADLINE, self.store.fence_register(replica))
                .await
                .map_err(|_| HostGrantError::Busy)??;
        {
            let mut state = self.write();
            state.registration = state.registration.saturating_add(1);
            state.installed = generations.iter().copied().collect();
            state.recorded.clear();
            state.lease = Some(Lease {
                replica,
                valid_until: sent + (FENCE_LEASE_TTL - FENCE_LEASE_MARGIN),
            });
        }
        // Whatever this replica derived from the grants before it held this
        // lease was derived without the fence.
        for (lifecycle, _) in &generations {
            self.request_sweep(*lifecycle);
        }
        self.record(replica).await
    }

    async fn renew(&self, replica: Uuid, sent: Instant) -> Result<(), HostGrantError> {
        let renewed = tokio::time::timeout(FENCE_TICK_DEADLINE, self.store.fence_renew(replica))
            .await
            .map_err(|_| HostGrantError::Busy)??;
        let Some(generations) = renewed else {
            self.lapse();
            self.note_lapse("not_renewed");
            return Ok(());
        };
        let mut moved = Vec::new();
        {
            let mut state = self.write();
            // Another tick may have replaced the lease meanwhile.
            if state.lease.map(|lease| lease.replica) != Some(replica) {
                return Ok(());
            }
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
            state.lease = Some(Lease {
                replica,
                valid_until: sent + (FENCE_LEASE_TTL - FENCE_LEASE_MARGIN),
            });
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
    /// `attachment` holds once bound. Nothing runs until
    /// [`Self::start`] or the first decision.
    pub(super) fn new(
        store: HostGrantStore,
        enrolled: EnrolledScopes,
        attachment: Arc<OnceLock<Arc<HostGrantAttachmentGate>>>,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                store,
                enrolled,
                attachment,
                state: RwLock::default(),
                ticks: watch::Sender::new(0),
                wake: Arc::new(Notify::new()),
                started: AtomicBool::new(false),
                suspended: AtomicBool::new(false),
                sweep: OnceLock::new(),
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
