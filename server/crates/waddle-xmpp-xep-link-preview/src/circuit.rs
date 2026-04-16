//! Per-host circuit breaker.
//!
//! Keeps one [`Circuit`] per hostname. The circuit opens after
//! [`CircuitConfig::failure_threshold`] consecutive failures within a
//! [`CircuitConfig::window`] and stays open for
//! [`CircuitConfig::open_duration`]. After that, the next request is a
//! half-open probe: success closes the circuit, failure re-opens it.
//!
//! All clock interactions accept an explicit [`Instant`] so tests can
//! advance time deterministically.

use std::time::{Duration, Instant};

use dashmap::DashMap;

#[derive(Debug, Clone)]
pub struct CircuitConfig {
    pub failure_threshold: u32,
    pub window: Duration,
    pub open_duration: Duration,
}

impl Default for CircuitConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            window: Duration::from_secs(60),
            open_duration: Duration::from_secs(5 * 60),
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct Circuit {
    consecutive_failures: u32,
    first_failure_in_window: Option<Instant>,
    opened_at: Option<Instant>,
}

pub struct CircuitBreaker {
    config: CircuitConfig,
    hosts: DashMap<String, Circuit>,
}

impl CircuitBreaker {
    pub fn new(config: CircuitConfig) -> Self {
        Self {
            config,
            hosts: DashMap::new(),
        }
    }

    /// Is the host allowed to be tried right now?
    ///
    /// Returns `true` if the circuit is closed *or* the open-duration
    /// has elapsed (granting a half-open probe).
    pub fn should_allow(&self, host: &str, now: Instant) -> bool {
        match self.hosts.get(host).as_deref().copied() {
            None => true,
            Some(circuit) => match circuit.opened_at {
                Some(opened_at) => now.duration_since(opened_at) >= self.config.open_duration,
                None => true,
            },
        }
    }

    pub fn record_success(&self, host: &str) {
        self.hosts.remove(host);
    }

    pub fn record_failure(&self, host: &str, now: Instant) {
        let mut entry = self.hosts.entry(host.to_owned()).or_default();

        // If a previous open period elapsed, this failure is the half-open
        // probe outcome. Treat it as a fresh window.
        if let Some(opened_at) = entry.opened_at {
            if now.duration_since(opened_at) >= self.config.open_duration {
                entry.opened_at = Some(now);
                entry.consecutive_failures = 1;
                entry.first_failure_in_window = Some(now);
                return;
            }
            // Still open — nothing to do.
            return;
        }

        let start = entry.first_failure_in_window.unwrap_or(now);
        if now.duration_since(start) > self.config.window {
            // Window elapsed without crossing threshold — reset.
            entry.consecutive_failures = 1;
            entry.first_failure_in_window = Some(now);
            return;
        }

        entry.first_failure_in_window.get_or_insert(now);
        entry.consecutive_failures = entry.consecutive_failures.saturating_add(1);

        if entry.consecutive_failures >= self.config.failure_threshold {
            entry.opened_at = Some(now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn breaker() -> CircuitBreaker {
        CircuitBreaker::new(CircuitConfig {
            failure_threshold: 3,
            window: Duration::from_secs(60),
            open_duration: Duration::from_secs(300),
        })
    }

    #[test]
    fn new_host_is_allowed() {
        let cb = breaker();
        assert!(cb.should_allow("example.com", Instant::now()));
    }

    #[test]
    fn failures_below_threshold_keep_circuit_closed() {
        let cb = breaker();
        let now = Instant::now();
        cb.record_failure("example.com", now);
        cb.record_failure("example.com", now + Duration::from_secs(1));
        assert!(cb.should_allow("example.com", now + Duration::from_secs(2)));
    }

    #[test]
    fn reaching_threshold_opens_circuit() {
        let cb = breaker();
        let now = Instant::now();
        for i in 0..3 {
            cb.record_failure("example.com", now + Duration::from_secs(i));
        }
        assert!(!cb.should_allow("example.com", now + Duration::from_secs(4)));
    }

    #[test]
    fn success_resets_consecutive_failures() {
        let cb = breaker();
        let now = Instant::now();
        cb.record_failure("example.com", now);
        cb.record_failure("example.com", now + Duration::from_secs(1));
        cb.record_success("example.com");
        // Two more failures must not trip because the counter reset.
        cb.record_failure("example.com", now + Duration::from_secs(2));
        cb.record_failure("example.com", now + Duration::from_secs(3));
        assert!(cb.should_allow("example.com", now + Duration::from_secs(4)));
    }

    #[test]
    fn open_circuit_half_opens_after_duration() {
        let cb = breaker();
        let now = Instant::now();
        for i in 0..3 {
            cb.record_failure("example.com", now + Duration::from_secs(i));
        }
        assert!(!cb.should_allow("example.com", now + Duration::from_secs(60)));
        assert!(cb.should_allow(
            "example.com",
            now + Duration::from_secs(60 + 300)
        ));
    }

    #[test]
    fn failure_during_half_open_reopens_circuit() {
        let cb = breaker();
        let t0 = Instant::now();
        for i in 0..3 {
            cb.record_failure("example.com", t0 + Duration::from_secs(i));
        }
        let after_open = t0 + Duration::from_secs(305);
        // Probe: should be allowed.
        assert!(cb.should_allow("example.com", after_open));
        // Probe fails.
        cb.record_failure("example.com", after_open);
        // Now open again for another 5 minutes.
        assert!(!cb.should_allow(
            "example.com",
            after_open + Duration::from_secs(1)
        ));
        assert!(cb.should_allow(
            "example.com",
            after_open + Duration::from_secs(301)
        ));
    }

    #[test]
    fn window_rollover_resets_counter() {
        let cb = breaker();
        let t0 = Instant::now();
        // Two failures, then long pause, then two more — must not trip.
        cb.record_failure("example.com", t0);
        cb.record_failure("example.com", t0 + Duration::from_secs(10));
        let after_window = t0 + Duration::from_secs(120);
        cb.record_failure("example.com", after_window);
        cb.record_failure("example.com", after_window + Duration::from_secs(1));
        assert!(cb.should_allow(
            "example.com",
            after_window + Duration::from_secs(2)
        ));
    }

    #[test]
    fn hosts_are_isolated() {
        let cb = breaker();
        let now = Instant::now();
        for i in 0..3 {
            cb.record_failure("broken.example", now + Duration::from_secs(i));
        }
        assert!(!cb.should_allow("broken.example", now + Duration::from_secs(3)));
        assert!(cb.should_allow("ok.example", now + Duration::from_secs(3)));
    }
}
