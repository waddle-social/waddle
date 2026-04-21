//! Latency + throughput recording, backed by `hdrhistogram`.

use std::sync::Mutex;
use std::time::Duration;

use hdrhistogram::Histogram;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OpKind {
    Write,
    ReadRangeAll,
    ReadRangeSender,
    ReadPagination,
}

impl OpKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Write => "write",
            Self::ReadRangeAll => "read_range",
            Self::ReadRangeSender => "read_sender",
            Self::ReadPagination => "read_page",
        }
    }

    pub fn is_read(self) -> bool {
        !matches!(self, Self::Write)
    }
}

/// Thread-safe, per-op-kind latency recorder.
/// `record_ns` is the hot path; contention is acceptable because individual
/// measurements are pushed from worker tasks which already cross an async
/// boundary per op.
pub struct LatencyRecorder {
    hists: [Mutex<Histogram<u64>>; 4],
    errors: Mutex<u64>,
    backpressure: Mutex<u64>,
}

impl Default for LatencyRecorder {
    fn default() -> Self {
        // 1ns .. 60s with 3 sig figs
        let new =
            || Mutex::new(Histogram::<u64>::new_with_bounds(1, 60 * 1_000_000_000, 3).unwrap());
        Self {
            hists: [new(), new(), new(), new()],
            errors: Mutex::new(0),
            backpressure: Mutex::new(0),
        }
    }
}

impl LatencyRecorder {
    pub fn record(&self, kind: OpKind, elapsed: Duration) {
        let ns = elapsed.as_nanos().min(u64::MAX as u128) as u64;
        if let Ok(mut h) = self.hists[Self::idx(kind)].lock() {
            let _ = h.record(ns);
        }
    }

    pub fn record_error(&self) {
        if let Ok(mut n) = self.errors.lock() {
            *n += 1;
        }
    }

    pub fn record_backpressure(&self) {
        if let Ok(mut n) = self.backpressure.lock() {
            *n += 1;
        }
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        let mut ops = Vec::new();
        for kind in [
            OpKind::Write,
            OpKind::ReadRangeAll,
            OpKind::ReadRangeSender,
            OpKind::ReadPagination,
        ] {
            let h = self.hists[Self::idx(kind)].lock().unwrap();
            if h.len() == 0 {
                continue;
            }
            ops.push(OpStats {
                kind: kind.as_str(),
                count: h.len(),
                p50_us: ns_to_us(h.value_at_quantile(0.50)),
                p95_us: ns_to_us(h.value_at_quantile(0.95)),
                p99_us: ns_to_us(h.value_at_quantile(0.99)),
                p999_us: ns_to_us(h.value_at_quantile(0.999)),
                max_us: ns_to_us(h.max()),
            });
        }
        MetricsSnapshot {
            ops,
            errors: *self.errors.lock().unwrap(),
            backpressure: *self.backpressure.lock().unwrap(),
        }
    }

    const fn idx(kind: OpKind) -> usize {
        match kind {
            OpKind::Write => 0,
            OpKind::ReadRangeAll => 1,
            OpKind::ReadRangeSender => 2,
            OpKind::ReadPagination => 3,
        }
    }
}

fn ns_to_us(ns: u64) -> f64 {
    ns as f64 / 1_000.0
}

/// Build an `OpStats` row from a raw histogram. Backends expose internal
/// histograms (queue-wait, sql-exec, ...) via this.
pub fn op_stats_from_hist(kind: &'static str, hist: &Histogram<u64>) -> Option<OpStats> {
    if hist.len() == 0 {
        return None;
    }
    Some(OpStats {
        kind,
        count: hist.len(),
        p50_us: ns_to_us(hist.value_at_quantile(0.50)),
        p95_us: ns_to_us(hist.value_at_quantile(0.95)),
        p99_us: ns_to_us(hist.value_at_quantile(0.99)),
        p999_us: ns_to_us(hist.value_at_quantile(0.999)),
        max_us: ns_to_us(hist.max()),
    })
}

#[derive(Debug, Serialize)]
pub struct MetricsSnapshot {
    pub ops: Vec<OpStats>,
    pub errors: u64,
    pub backpressure: u64,
}

#[derive(Debug, Serialize)]
pub struct OpStats {
    pub kind: &'static str,
    pub count: u64,
    pub p50_us: f64,
    pub p95_us: f64,
    pub p99_us: f64,
    pub p999_us: f64,
    pub max_us: f64,
}
