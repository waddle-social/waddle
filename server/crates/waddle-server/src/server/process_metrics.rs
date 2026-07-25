//! Process-level OTel instruments: start time, CPU time, open FDs (#1435).
//!
//! Since the `/metrics` contract phase (#1330/#1426) reduced the
//! Prometheus text endpoint to a liveness stub, waddle-server exported
//! no restart/uptime signal at all — crash-loops were only inferable
//! from ReplicaSet-hash churn. These instruments restore that signal
//! through the OTLP pipeline:
//!
//! - `waddle.process.start_time` (unit `s`): unix-epoch seconds
//!   captured once at telemetry init. A restart is detectable from
//!   metrics alone via `changes(waddle_process_start_time_seconds[1h]) > 0`.
//! - `waddle.process.cpu.time` (unit `s`, monotonic sum): user+system
//!   CPU seconds from `/proc/self/stat`.
//! - `waddle.process.open_file_descriptors` (unit `{fd}`): entry count
//!   of `/proc/self/fd`.
//!
//! Observable instruments with callbacks (the `init_pod_gauges`
//! pattern) rather than the periodic publisher: values are read at
//! every reader collection, so they export from the first OTLP push
//! with no sidecar task. The `/proc` readers are Linux-only and degrade
//! to "no series" elsewhere, mirroring
//! `state_inventory_metrics::read_process_rss_bytes`.

use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

fn meter() -> &'static opentelemetry::metrics::Meter {
    static METER: OnceLock<opentelemetry::metrics::Meter> = OnceLock::new();
    METER.get_or_init(|| opentelemetry::global::meter("waddle-server"))
}

/// Unix-epoch seconds captured on first use — telemetry init runs
/// within milliseconds of exec, so this is the process start time for
/// every restart-detection purpose. Reading the kernel's exact value
/// (`/proc/self/stat` field 22 + boot time) would buy sub-second
/// precision at the cost of a Linux-only start-time signal.
fn start_time_unix_seconds() -> i64 {
    static START: OnceLock<i64> = OnceLock::new();
    *START.get_or_init(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|since_epoch| i64::try_from(since_epoch.as_secs()).unwrap_or(i64::MAX))
            .unwrap_or(0)
    })
}

/// Parse total CPU seconds (utime + stime) from a `/proc/<pid>/stat`
/// line. The second field, `comm`, is parenthesized and may itself
/// contain spaces and `)` — fields are therefore indexed after the
/// *last* `)`. After it, `state` is overall field 3, so `utime`
/// (overall field 14) sits at index 11 and `stime` (15) follows.
fn cpu_seconds_from_stat(stat: &str, clock_ticks_per_second: u64) -> Option<f64> {
    if clock_ticks_per_second == 0 {
        return None;
    }
    let (_, after_comm) = stat.rsplit_once(')')?;
    let mut fields = after_comm.split_whitespace();
    let utime: u64 = fields.nth(11)?.parse().ok()?;
    let stime: u64 = fields.next()?.parse().ok()?;
    Some((utime.saturating_add(stime)) as f64 / clock_ticks_per_second as f64)
}

/// The kernel's ticks-per-second constant. `None` when unavailable or
/// nonsensical (≤ 0) rather than dividing by a bogus value.
fn clock_ticks_per_second() -> Option<u64> {
    #[cfg(unix)]
    {
        // SAFETY: `sysconf(_SC_CLK_TCK)` is signal-safe and has no
        // preconditions.
        let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        u64::try_from(ticks).ok().filter(|ticks| *ticks > 0)
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// Read total process CPU seconds. `None` off Linux (`/proc` does not
/// exist, so the read fails) or when `/proc/self/stat` is malformed;
/// the callback then records no sample instead of a wrong one.
fn read_cpu_seconds() -> Option<f64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    cpu_seconds_from_stat(&stat, clock_ticks_per_second()?)
}

/// Count this process's open file descriptors via `/proc/self/fd`.
/// The count includes the directory handle the read itself opens —
/// a constant +1 that never matters for the leak trends the metric
/// exists to show. `None` off Linux.
fn read_open_fd_count() -> Option<i64> {
    let entries = std::fs::read_dir("/proc/self/fd").ok()?;
    i64::try_from(entries.count()).ok()
}

/// Register the process instruments eagerly so a fresh pod exports its
/// start time on the very first OTLP push (a restart signal that only
/// appears after traffic would miss the crash-loops it exists to
/// catch). Called from `telemetry::init` next to
/// `waddle_xmpp::metrics::init_pod_gauges`; the instruments are held
/// in the `OnceLock` so their callbacks stay alive for the process
/// lifetime.
pub(crate) fn init_process_instruments() {
    struct ProcessInstruments {
        _start_time: opentelemetry::metrics::ObservableGauge<i64>,
        _cpu_time: opentelemetry::metrics::ObservableCounter<f64>,
        _open_fds: opentelemetry::metrics::ObservableGauge<i64>,
    }
    static INSTRUMENTS: OnceLock<ProcessInstruments> = OnceLock::new();
    INSTRUMENTS.get_or_init(|| ProcessInstruments {
        _start_time: meter()
            .i64_observable_gauge("waddle.process.start_time")
            .with_description(
                "Unix time the server process started, captured at telemetry init. \
                 changes(...) > 0 detects a restart from metrics alone (#1435).",
            )
            .with_unit("s")
            .with_callback(|observer| observer.observe(start_time_unix_seconds(), &[]))
            .build(),
        _cpu_time: meter()
            .f64_observable_counter("waddle.process.cpu.time")
            .with_description(
                "Total user+system CPU seconds consumed by the server process, \
                 from /proc/self/stat.",
            )
            .with_unit("s")
            .with_callback(|observer| {
                if let Some(seconds) = read_cpu_seconds() {
                    observer.observe(seconds, &[]);
                }
            })
            .build(),
        _open_fds: meter()
            .i64_observable_gauge("waddle.process.open_file_descriptors")
            // UCUM annotation: dropped by Prometheus name normalization,
            // so the backend series has no unit suffix.
            .with_unit("{fd}")
            .with_description(
                "Open file descriptors held by the server process, from /proc/self/fd.",
            )
            .with_callback(|observer| {
                if let Some(count) = read_open_fd_count() {
                    observer.observe(count, &[]);
                }
            })
            .build(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_time_is_positive_and_stable() {
        let first = start_time_unix_seconds();
        assert!(first > 0, "start time must be a positive unix timestamp");
        assert_eq!(
            first,
            start_time_unix_seconds(),
            "start time must never move within a process lifetime"
        );
    }

    #[test]
    fn cpu_seconds_parses_a_realistic_stat_line() {
        // utime = 250 ticks (overall field 14), stime = 150 ticks (15),
        // 100 ticks/s → 4.0 CPU seconds.
        let stat = "12345 (waddle-server) S 1 12345 12345 0 -1 4194560 1000 0 5 0 250 150 0 0 20 0 8 0 100000 1000000 500 18446744073709551615 1 1 0 0 0 0 0 0 0 0 0 0 17 3 0 0 0 0 0";
        assert_eq!(cpu_seconds_from_stat(stat, 100), Some(4.0));
    }

    #[test]
    fn cpu_seconds_survives_comm_with_spaces_and_parens() {
        // comm is attacker/operator-controlled (`exec -a`); parsing must
        // index after the LAST ')'.
        let stat = "1 (evil ) comm) S 1 1 1 0 -1 0 0 0 0 0 300 100 0 0 20 0 1 0 1 1 1 1";
        assert_eq!(cpu_seconds_from_stat(stat, 100), Some(4.0));
    }

    #[test]
    fn cpu_seconds_rejects_malformed_input_and_zero_ticks() {
        assert_eq!(cpu_seconds_from_stat("garbage", 100), None);
        assert_eq!(cpu_seconds_from_stat("1 (a) S 1 2", 100), None);
        assert_eq!(cpu_seconds_from_stat("", 100), None);
        let valid = "1 (a) S 1 1 1 0 -1 0 0 0 0 0 300 100 0 0 20 0 1 0 1 1 1 1";
        assert_eq!(cpu_seconds_from_stat(valid, 0), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn proc_readers_return_values_on_linux() {
        assert!(
            read_cpu_seconds().is_some_and(|seconds| seconds >= 0.0),
            "a live Linux process must report CPU time"
        );
        assert!(
            read_open_fd_count().is_some_and(|count| count > 0),
            "a live Linux process must hold open file descriptors"
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn proc_readers_are_none_off_linux() {
        assert!(read_cpu_seconds().is_none());
        assert!(read_open_fd_count().is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn init_exports_the_process_instruments_through_the_reader_seam() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        init_process_instruments();

        let names = metrics.metric_names();
        assert!(
            names.contains(&"waddle.process.start_time".to_string()),
            "start time must export eagerly: {names:?}"
        );
        assert_eq!(
            metrics.metric_unit("waddle.process.start_time"),
            Some("s".to_string()),
        );

        // The /proc-backed instruments only produce series on Linux.
        #[cfg(target_os = "linux")]
        {
            assert!(
                names.contains(&"waddle.process.cpu.time".to_string()),
                "CPU time must export on Linux: {names:?}"
            );
            assert!(
                names.contains(&"waddle.process.open_file_descriptors".to_string()),
                "open-FD count must export on Linux: {names:?}"
            );
        }
    }
}
