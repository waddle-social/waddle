//! Per-JID rate limit on Jingle `session-initiate`.
//!
//! Without a limit, a misbehaving (or compromised) client can spam
//! `session-initiate` and force the server to mint LiveKit JWTs +
//! register call participants for every one of them. Even though
//! each JWT-mint is sub-millisecond, a flood would still
//! (a) saturate the SFU registry with garbage entries that no
//! `session-terminate` ever clears (DoS the registry's memory),
//! (b) churn the inbox of every recipient resource.
//!
//! Bucket-style limiter: keep a `VecDeque<Instant>` per bare JID
//! holding the timestamps of recent initiates; on each new
//! initiate, drop entries older than `window` and reject the call
//! if the remaining count is `>= max_initiates`.
//!
//! Keyed by bare JID (not full): rate-limiting per-resource would
//! let a single user blast initiates from many resources in
//! parallel.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use jid::BareJid;

/// Default policy: at most 5 session-initiates per 30s per bare JID.
/// A real-world conversation rarely needs more than 1–2 in that
/// window; the headroom covers a click-then-immediately-cancel-then-
/// retry pattern without locking the user out.
pub const DEFAULT_MAX_INITIATES: usize = 5;
pub const DEFAULT_WINDOW: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub struct SessionInitiateRateLimit {
    max_initiates: usize,
    window: Duration,
    buckets: Mutex<HashMap<BareJid, VecDeque<Instant>>>,
}

impl SessionInitiateRateLimit {
    pub fn new(max_initiates: usize, window: Duration) -> Self {
        Self {
            max_initiates,
            window,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(DEFAULT_MAX_INITIATES, DEFAULT_WINDOW)
    }

    /// Returns `Err(())` if the JID has exceeded the per-window
    /// initiate budget. Otherwise records the attempt and returns
    /// `Ok(())`.
    pub fn check_and_record(&self, jid: &BareJid) -> Result<(), RateLimitExceeded> {
        self.check_and_record_at(jid, Instant::now())
    }

    /// Test-only entry point that lets us pass a controlled clock.
    pub(crate) fn check_and_record_at(
        &self,
        jid: &BareJid,
        now: Instant,
    ) -> Result<(), RateLimitExceeded> {
        let mut buckets = self.buckets.lock().expect("rate-limit mutex poisoned");
        let bucket = buckets.entry(jid.clone()).or_default();

        // Drop expired timestamps from the head — `VecDeque` keeps
        // them in arrival order so we can stop at the first
        // still-fresh entry.
        while let Some(&front) = bucket.front() {
            if now.duration_since(front) > self.window {
                bucket.pop_front();
            } else {
                break;
            }
        }

        if bucket.len() >= self.max_initiates {
            // Don't record the rejected attempt — the limiter
            // resets cleanly once the window passes; recording
            // would extend the lockout indefinitely under sustained
            // pressure.
            return Err(RateLimitExceeded {
                window: self.window,
                max_initiates: self.max_initiates,
            });
        }

        bucket.push_back(now);
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RateLimitExceeded {
    pub window: Duration,
    pub max_initiates: usize,
}

impl std::fmt::Display for RateLimitExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "rate limit exceeded: more than {} session-initiates within {:?}",
            self.max_initiates, self.window
        )
    }
}

impl std::error::Error for RateLimitExceeded {}

#[cfg(test)]
mod tests {
    use super::*;

    fn jid(s: &str) -> BareJid {
        s.parse().unwrap()
    }

    #[test]
    fn under_budget_allows_initiates() {
        let limiter = SessionInitiateRateLimit::new(3, Duration::from_secs(30));
        let alice = jid("alice@waddle.test");
        let now = Instant::now();
        assert!(limiter.check_and_record_at(&alice, now).is_ok());
        assert!(limiter
            .check_and_record_at(&alice, now + Duration::from_millis(10))
            .is_ok());
        assert!(limiter
            .check_and_record_at(&alice, now + Duration::from_millis(20))
            .is_ok());
    }

    #[test]
    fn over_budget_rejects() {
        let limiter = SessionInitiateRateLimit::new(2, Duration::from_secs(30));
        let alice = jid("alice@waddle.test");
        let now = Instant::now();
        assert!(limiter.check_and_record_at(&alice, now).is_ok());
        assert!(limiter
            .check_and_record_at(&alice, now + Duration::from_millis(10))
            .is_ok());
        // Third attempt within window — denied.
        assert!(limiter
            .check_and_record_at(&alice, now + Duration::from_millis(20))
            .is_err());
    }

    #[test]
    fn rejected_attempt_does_not_extend_lockout() {
        let limiter = SessionInitiateRateLimit::new(2, Duration::from_secs(30));
        let alice = jid("alice@waddle.test");
        let now = Instant::now();
        limiter.check_and_record_at(&alice, now).unwrap();
        limiter
            .check_and_record_at(&alice, now + Duration::from_millis(10))
            .unwrap();
        // Hammer the limiter while denied.
        for i in 0..10 {
            assert!(limiter
                .check_and_record_at(&alice, now + Duration::from_millis(100 + i))
                .is_err());
        }
        // Once the window passes, the bucket is clean again.
        let after = now + Duration::from_secs(31);
        assert!(limiter.check_and_record_at(&alice, after).is_ok());
    }

    #[test]
    fn different_jids_have_independent_buckets() {
        let limiter = SessionInitiateRateLimit::new(1, Duration::from_secs(30));
        let alice = jid("alice@waddle.test");
        let bob = jid("bob@waddle.test");
        let now = Instant::now();
        assert!(limiter.check_and_record_at(&alice, now).is_ok());
        assert!(limiter.check_and_record_at(&bob, now).is_ok());
        // Both filled their bucket — third attempt from either is denied.
        assert!(limiter.check_and_record_at(&alice, now).is_err());
        assert!(limiter.check_and_record_at(&bob, now).is_err());
    }

    #[test]
    fn window_drops_expired_entries() {
        let limiter = SessionInitiateRateLimit::new(2, Duration::from_secs(1));
        let alice = jid("alice@waddle.test");
        let now = Instant::now();
        limiter.check_and_record_at(&alice, now).unwrap();
        limiter
            .check_and_record_at(&alice, now + Duration::from_millis(100))
            .unwrap();
        // The first entry is now older than the 1s window.
        assert!(limiter
            .check_and_record_at(&alice, now + Duration::from_millis(1500))
            .is_ok());
    }
}
