//! Serializable per-run report written to `results/*.json`.

use serde::Serialize;

use crate::metrics::{MetricsSnapshot, OpStats};

#[derive(Debug, Serialize)]
pub struct RunReport {
    pub backend: String,
    pub scale: u64,
    pub warmup_seconds: u64,
    pub duration_seconds: u64,
    pub target_ops_per_user_per_min: f64,
    pub total_writes: u64,
    pub total_reads: u64,
    pub db_size_bytes: u64,
    pub peak_rss_kib: u64,
    pub metrics: MetricsSnapshot,
    /// Backend-internal breakdowns (e.g. queue-wait vs. sql-exec for stores
    /// that queue writes). Empty for backends that don't surface any.
    pub diagnostics: Vec<OpStats>,
    /// Per-second throughput samples (ops/sec, total writes+reads).
    pub throughput_samples: Vec<u64>,
}

/// Best-effort peak RSS query. Returns 0 if the platform isn't handled.
pub fn peak_rss_kib() -> u64 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
            for line in status.lines() {
                if let Some(rest) = line.strip_prefix("VmHWM:") {
                    if let Some(tok) = rest.split_whitespace().next() {
                        if let Ok(kib) = tok.parse::<u64>() {
                            return kib;
                        }
                    }
                }
            }
        }
        0
    }
    #[cfg(target_os = "macos")]
    {
        // `ps -o rss= -p <pid>` returns KiB on darwin. Best-effort: skip on error.
        use std::process::Command;
        let pid = std::process::id();
        let out = Command::new("ps")
            .args(["-o", "rss=", "-p", &pid.to_string()])
            .output();
        if let Ok(o) = out {
            if let Ok(s) = String::from_utf8(o.stdout) {
                if let Ok(kib) = s.trim().parse::<u64>() {
                    return kib;
                }
            }
        }
        0
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        0
    }
}
