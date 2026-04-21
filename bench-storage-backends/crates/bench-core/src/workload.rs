//! Sustained-session workload generator.
//!
//! Design points (see `/Users/rawkode/.claude/plans/polymorphic-questing-waffle.md`):
//!
//! * One tokio task per session would cost ~1 GiB at 1 M sessions — instead we
//!   use a fixed worker pool draining a shared op queue.
//! * Per-session inter-op delay is drawn from an exponential distribution so
//!   the aggregate rate matches target without thundering-herd wakes.
//! * Each scheduled op is stamped `Write` with probability `p_write` (default
//!   0.2) and `Read` otherwise.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{Duration as ChronoDuration, Utc};
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};
use rand_distr::Distribution;
use tokio::sync::{mpsc, Semaphore};
use tokio::task::JoinSet;

use crate::message::{ArchivedMessage, MamQuery};
use crate::metrics::{LatencyRecorder, OpKind};
use crate::store::{StanzaStore, StoreError};

/// Number of distinct rooms — one room per 50 users, min 4.
fn rooms_for_scale(sessions: u64) -> u64 {
    (sessions / 50).max(4)
}

/// Named `--scale` presets.
///
/// `Write250k` is sized so that with the default mix (80/20) and the default
/// per-user rate (1 op/user/min) the aggregate write rate lands on
/// 250_000/sec:
///
/// ```text
///   250_000 writes/sec
///     / 0.2  (p_write)            =  1_250_000 ops/sec
///     * 60   (sec/min)            = 75_000_000 ops/min
///     / 1    (op/user/min)        = 75_000_000 users
/// ```
#[derive(Debug, Clone, Copy)]
pub enum Scale {
    Small,     // 10
    Medium,    // 10_000
    Large,     // 1_000_000
    Write250k, // 75_000_000  — targets 250k writes/sec under default mix
    Custom(u64),
}

impl Scale {
    pub fn sessions(self) -> u64 {
        match self {
            Self::Small => 10,
            Self::Medium => 10_000,
            Self::Large => 1_000_000,
            Self::Write250k => 75_000_000,
            Self::Custom(n) => n,
        }
    }
}

#[derive(Debug, Clone)]
pub struct WorkloadConfig {
    pub sessions: u64,
    /// Target operations per user per minute. Realistic XMPP default ≈ 1/min.
    pub ops_per_user_per_min: f64,
    /// Probability an op is a write (0.2 = 80/20 read-heavy).
    pub p_write: f64,
    pub warmup: Duration,
    pub duration: Duration,
    /// Upper bound on concurrent in-flight ops against the store.
    pub max_in_flight: usize,
}

impl WorkloadConfig {
    pub fn new(sessions: u64, duration: Duration) -> Self {
        Self {
            sessions,
            ops_per_user_per_min: 1.0,
            p_write: 0.2,
            warmup: Duration::from_secs(0),
            duration,
            max_in_flight: 1024,
        }
    }
}

/// Counters tracked during a run.
#[derive(Default)]
pub struct RunCounters {
    pub writes: AtomicU64,
    pub reads: AtomicU64,
}

pub struct WorkloadRunner<S: StanzaStore> {
    store: Arc<S>,
    cfg: WorkloadConfig,
    metrics: Arc<LatencyRecorder>,
    counters: Arc<RunCounters>,
}

impl<S: StanzaStore> WorkloadRunner<S> {
    pub fn new(store: Arc<S>, cfg: WorkloadConfig, metrics: Arc<LatencyRecorder>) -> Self {
        Self {
            store,
            cfg,
            metrics,
            counters: Arc::new(RunCounters::default()),
        }
    }

    pub fn counters(&self) -> Arc<RunCounters> {
        self.counters.clone()
    }

    /// Warmup: pre-populate ~100 messages per session, up to a cap, in parallel.
    /// Writes during warmup are *not* measured.
    pub async fn warmup(&self) -> Result<(), StoreError> {
        let rooms = rooms_for_scale(self.cfg.sessions);
        // Cap seed volume so 1 M sessions don't try to write 100 M rows on warmup.
        let target = (self.cfg.sessions.saturating_mul(100)).min(200_000);
        let concurrency = 64usize;
        let sem = Arc::new(Semaphore::new(concurrency));
        let mut set = JoinSet::new();
        let store = self.store.clone();

        for i in 0..target {
            let permit = sem.clone().acquire_owned().await.unwrap();
            let store = store.clone();
            let room = i % rooms;
            let user = i % self.cfg.sessions;
            set.spawn(async move {
                let _p = permit;
                let msg = build_message(room, user, "warmup");
                store.store_message(&msg).await
            });
            // Reap occasionally to keep the JoinSet bounded.
            if set.len() > concurrency * 4 {
                if let Some(res) = set.join_next().await {
                    res.map_err(StoreError::backend)??;
                }
            }
        }
        while let Some(res) = set.join_next().await {
            res.map_err(StoreError::backend)??;
        }
        Ok(())
    }

    /// Run the measured window. Returns per-second throughput samples.
    pub async fn run(&self) -> Result<Vec<u64>, StoreError> {
        let cfg = self.cfg.clone();
        let rooms = rooms_for_scale(cfg.sessions);

        // Poisson process per session → aggregate rate = sessions * ops/sec/user.
        let ops_per_user_per_sec = cfg.ops_per_user_per_min / 60.0;
        let agg_per_sec = ops_per_user_per_sec * (cfg.sessions as f64);

        // Op queue. Small buffer: we'd rather the driver block than buffer GB.
        let (tx, rx) = mpsc::channel::<ScheduledOp>(cfg.max_in_flight);

        // ---- driver: schedules ops at the aggregate rate ----
        //
        // We don't sleep per-op: tokio sleep precision is ~1ms on macOS and
        // ~50us on Linux, which would cap us at ~1-20k ops/sec regardless of
        // how fast the store is. Instead we tick every `TICK`, draw a Poisson
        // count for the interval (mean = agg_per_sec * TICK), and enqueue that
        // many ops in a batch. Aggregate rate stays exact over seconds of
        // wall time; per-tick variance is Poisson as intended.
        let driver = {
            let tx = tx.clone();
            let sessions = cfg.sessions;
            let duration = cfg.duration;
            let p_write = cfg.p_write;
            tokio::spawn(async move {
                use rand_distr::Poisson;
                const TICK: Duration = Duration::from_millis(10);
                let mut rng = SmallRng::from_entropy();
                let mean_per_tick = (agg_per_sec * TICK.as_secs_f64()).max(0.0001);
                let pois = Poisson::new(mean_per_tick).unwrap();
                let deadline = Instant::now() + duration;
                let mut interval = tokio::time::interval(TICK);
                interval.tick().await; // discard the immediate first tick
                while Instant::now() < deadline {
                    interval.tick().await;
                    let n: u64 = pois.sample(&mut rng) as u64;
                    for _ in 0..n {
                        let user = rng.gen_range(0..sessions);
                        let room = user % rooms;
                        let is_write = rng.gen::<f64>() < p_write;
                        let op = if is_write {
                            ScheduledOp::Write { room, user }
                        } else {
                            ScheduledOp::Read {
                                room,
                                kind: pick_read_kind(&mut rng),
                            }
                        };
                        if tx.try_send(op).is_err() {
                            // Queue full: fall back to a blocking send so we
                            // observe real backpressure instead of dropping.
                            if tx.send(op).await.is_err() {
                                return;
                            }
                        }
                    }
                }
            })
        };

        // ---- workers: drain the queue and hit the store ----
        let workers_n = (num_cpus() * 4).max(8);
        let mut workers = JoinSet::new();
        let store = self.store.clone();
        let metrics = self.metrics.clone();
        let counters = self.counters.clone();

        let rx = Arc::new(tokio::sync::Mutex::new(rx));
        for _ in 0..workers_n {
            let store = store.clone();
            let metrics = metrics.clone();
            let counters = counters.clone();
            let rx = rx.clone();
            workers.spawn(async move {
                loop {
                    let op = {
                        let mut guard = rx.lock().await;
                        match guard.recv().await {
                            Some(op) => op,
                            None => break,
                        }
                    };
                    execute_op(&store, op, &metrics, &counters).await;
                }
            });
        }

        // ---- sampler: records per-second cumulative throughput ----
        let throughput_counters = self.counters.clone();
        let sampler_duration = cfg.duration;
        let sampler = tokio::spawn(async move {
            let mut samples = Vec::with_capacity(sampler_duration.as_secs() as usize + 1);
            let mut prev: u64 = 0;
            let deadline = Instant::now() + sampler_duration;
            while Instant::now() < deadline {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let w = throughput_counters.writes.load(Ordering::Relaxed);
                let r = throughput_counters.reads.load(Ordering::Relaxed);
                let total = w + r;
                samples.push(total.saturating_sub(prev));
                prev = total;
            }
            samples
        });

        // Wait for driver to finish, then close channel and drain workers.
        driver.await.map_err(StoreError::backend)?;
        drop(tx);
        // The rx inside Arc still lives via workers; when they see Empty+closed they'll exit.
        while let Some(res) = workers.join_next().await {
            let _ = res; // worker errors are per-op, already counted
        }
        let samples = sampler.await.map_err(StoreError::backend)?;
        Ok(samples)
    }
}

#[derive(Debug, Clone, Copy)]
enum ScheduledOp {
    Write { room: u64, user: u64 },
    Read { room: u64, kind: ReadKind },
}

#[derive(Debug, Clone, Copy)]
enum ReadKind {
    RangeAll,
    RangeSender,
    Pagination,
}

fn pick_read_kind(rng: &mut impl Rng) -> ReadKind {
    // Roughly: range-all is the common case (a client opening a room),
    // sender filter is rarer, pagination is mid-frequency.
    let r: f64 = rng.gen();
    if r < 0.6 {
        ReadKind::RangeAll
    } else if r < 0.85 {
        ReadKind::Pagination
    } else {
        ReadKind::RangeSender
    }
}

async fn execute_op<S: StanzaStore>(
    store: &Arc<S>,
    op: ScheduledOp,
    metrics: &LatencyRecorder,
    counters: &RunCounters,
) {
    match op {
        ScheduledOp::Write { room, user } => {
            let msg = build_message(room, user, "live");
            let t0 = Instant::now();
            match store.store_message(&msg).await {
                Ok(()) => {
                    metrics.record(OpKind::Write, t0.elapsed());
                    counters.writes.fetch_add(1, Ordering::Relaxed);
                }
                Err(StoreError::Backpressure(_)) => metrics.record_backpressure(),
                Err(e) => {
                    // Sample-rate error logging so we can diagnose without
                    // drowning stderr on high-error runs.
                    static ERR_SAMPLE: std::sync::atomic::AtomicU64 =
                        std::sync::atomic::AtomicU64::new(0);
                    let n = ERR_SAMPLE.fetch_add(1, Ordering::Relaxed);
                    if n < 5 || n.is_power_of_two() {
                        tracing::warn!(%e, "write error sample");
                    }
                    metrics.record_error();
                }
            }
        }
        ScheduledOp::Read { room, kind } => {
            let room_jid = room_jid(room);
            let now = Utc::now();
            let query = match kind {
                ReadKind::RangeAll => MamQuery {
                    room_jid,
                    start: Some(now - ChronoDuration::days(7)),
                    end: Some(now),
                    limit: 100,
                    ..Default::default()
                },
                ReadKind::RangeSender => MamQuery {
                    room_jid,
                    start: Some(now - ChronoDuration::days(7)),
                    end: Some(now),
                    from_jid: Some(user_jid(room * 13)), // deterministic sampler
                    limit: 100,
                    ..Default::default()
                },
                ReadKind::Pagination => MamQuery {
                    room_jid,
                    limit: 50,
                    // empty after_id = start-from-oldest; good enough for measurement
                    ..Default::default()
                },
            };
            let t0 = Instant::now();
            match store.query_messages(&query).await {
                Ok(_rows) => {
                    let op_kind = match kind {
                        ReadKind::RangeAll => OpKind::ReadRangeAll,
                        ReadKind::RangeSender => OpKind::ReadRangeSender,
                        ReadKind::Pagination => OpKind::ReadPagination,
                    };
                    metrics.record(op_kind, t0.elapsed());
                    counters.reads.fetch_add(1, Ordering::Relaxed);
                }
                Err(StoreError::Backpressure(_)) => metrics.record_backpressure(),
                Err(_) => metrics.record_error(),
            }
        }
    }
}

fn build_message(room: u64, user: u64, tag: &str) -> ArchivedMessage {
    let room_jid = room_jid(room);
    let from = user_jid(user);
    let mut m = ArchivedMessage::new_chat(
        &room_jid,
        &from,
        &room_jid,
        &format!("hello from user {user} in {tag}"),
    );
    m.thread_id = Some(format!("thr-{}", room));
    m.stanza_id = Some(m.id.clone());
    m.origin_id = Some(m.id.clone());
    m.message_type = "groupchat".to_string();
    m
}

fn room_jid(room: u64) -> String {
    format!("room{room}@conference.bench.local")
}

fn user_jid(user: u64) -> String {
    format!("user{user}@bench.local/client")
}

fn num_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockStore;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn workload_respects_80_20_mix() {
        let store = Arc::new(MockStore::new());
        store.init().await.unwrap();
        let cfg = WorkloadConfig {
            sessions: 100,
            ops_per_user_per_min: 600.0, // 10 ops/sec/user → aggregate 1000 ops/sec
            p_write: 0.2,
            warmup: Duration::ZERO,
            duration: Duration::from_secs(2),
            max_in_flight: 512,
        };
        let metrics = Arc::new(LatencyRecorder::default());
        let runner = WorkloadRunner::new(store.clone(), cfg, metrics.clone());
        let _samples = runner.run().await.unwrap();

        let w = store.writes();
        let r = store.reads();
        let total = w + r;
        assert!(total > 100, "expected some ops, got {total}");
        let write_ratio = w as f64 / total as f64;
        // Poisson-scheduled, so allow a wide window.
        assert!(
            (0.1..=0.35).contains(&write_ratio),
            "write ratio {write_ratio} outside 0.1..=0.35"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn metrics_snapshot_is_populated() {
        let store = Arc::new(MockStore::new());
        store.init().await.unwrap();
        let cfg = WorkloadConfig {
            sessions: 20,
            ops_per_user_per_min: 600.0,
            p_write: 0.2,
            warmup: Duration::ZERO,
            duration: Duration::from_secs(1),
            max_in_flight: 64,
        };
        let metrics = Arc::new(LatencyRecorder::default());
        let runner = WorkloadRunner::new(store.clone(), cfg, metrics.clone());
        runner.run().await.unwrap();
        let snap = metrics.snapshot();
        assert!(!snap.ops.is_empty());
    }
}
