//! Per-JID sliding-window rate limits for call-control surfaces.
//!
//! `session-initiate` is the original caller and still the highest-cost
//! surface here: a burst forces JWT minting and SFU registry writes. The
//! same per-bare-JID sliding-window shape also now protects:
//!
//! - `session-terminate`, so a compromised client cannot spam teardown
//!   requests into the control plane.
//! - Muji `session-terminate` plus other non-initiate actions, which can
//!   fan out into room-locality checks, membership asks, or cross-node
//!   relay attempts before they are rejected.
//! - TURN credential issuance (`extdisco` requests that actually mint a
//!   credential-bearing response).
//!
//! Bucket-style limiter: keep a `VecDeque<Instant>` per bare JID
//! holding the timestamps of recent events; on each new event, drop
//! entries older than `window` and reject the request if the remaining
//! count is `>= max_events`.
//!
//! Keyed by bare JID (not full): rate-limiting per-resource would
//! let a single user blast requests from many resources in
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
/// A first legitimate hangup must not be blocked, so the terminate
/// budget is deliberately generous.
pub const DEFAULT_MAX_TERMINATES: usize = 20;
/// Unsupported Muji actions still receive protocol error replies. This
/// budget absorbs ordinary client retry bursts while bounding the
/// room-locality, membership, and relay work performed before rejection.
pub const DEFAULT_MAX_MUJI_ACTIONS: usize = 60;
/// TURN credentials are request-shaped and should stay comfortably
/// available during login / reconnect loops.
pub const DEFAULT_MAX_TURN_CREDENTIALS: usize = 10;
pub const DEFAULT_WINDOW: Duration = Duration::from_secs(30);

/// Opaque receipt for one successful `check_and_record`. Refunds are
/// keyed on it so a caller can only ever remove ITS OWN recorded
/// event — a bare `pop_back` could refund a concurrent request's
/// charge and skew the sliding window (#1612 review round 10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChargeToken(u64);

#[derive(Debug)]
struct PerBareJidSlidingWindowRateLimit {
    max_events: usize,
    window: Duration,
    next_charge_seq: std::sync::atomic::AtomicU64,
    buckets: Mutex<HashMap<BareJid, VecDeque<(u64, Instant)>>>,
}

impl PerBareJidSlidingWindowRateLimit {
    fn new(max_events: usize, window: Duration) -> Self {
        Self {
            max_events,
            window,
            next_charge_seq: std::sync::atomic::AtomicU64::new(0),
            buckets: Mutex::new(HashMap::new()),
        }
    }

    fn check_and_record(&self, jid: &BareJid) -> Result<ChargeToken, RateLimitExceeded> {
        self.check_and_record_at(jid, Instant::now())
    }

    /// Test-only entry point that lets us pass a controlled clock.
    fn check_and_record_at(
        &self,
        jid: &BareJid,
        now: Instant,
    ) -> Result<ChargeToken, RateLimitExceeded> {
        let mut buckets = self.buckets.lock().expect("rate-limit mutex poisoned");
        let bucket = buckets.entry(jid.clone()).or_default();

        // Drop expired timestamps from the head — `VecDeque` keeps
        // them in arrival order so we can stop at the first
        // still-fresh entry.
        while let Some(&(_, front)) = bucket.front() {
            if now.duration_since(front) > self.window {
                bucket.pop_front();
            } else {
                break;
            }
        }

        if bucket.len() >= self.max_events {
            // Don't record the rejected attempt — the limiter
            // resets cleanly once the window passes; recording
            // would extend the lockout indefinitely under sustained
            // pressure.
            return Err(RateLimitExceeded {
                window: self.window,
                max_events: self.max_events,
            });
        }

        let seq = self
            .next_charge_seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        bucket.push_back((seq, now));
        Ok(ChargeToken(seq))
    }

    /// Remove exactly the event recorded under `token`, if it is still
    /// in the window. Used to refund a charge once the request is known
    /// to have performed none of the work the limiter exists to bound
    /// (#1612 review rounds 9-10).
    fn refund(&self, jid: &BareJid, token: ChargeToken) {
        let mut buckets = self.buckets.lock().expect("rate-limit mutex poisoned");
        if let Some(bucket) = buckets.get_mut(jid) {
            if let Some(position) = bucket.iter().position(|(seq, _)| *seq == token.0) {
                bucket.remove(position);
            }
            if bucket.is_empty() {
                buckets.remove(jid);
            }
        }
    }
}

macro_rules! define_rate_limit {
    ($name:ident, $default_max:expr) => {
        #[derive(Debug)]
        pub struct $name {
            inner: PerBareJidSlidingWindowRateLimit,
        }

        impl $name {
            pub fn new(max_events: usize, window: Duration) -> Self {
                Self {
                    inner: PerBareJidSlidingWindowRateLimit::new(max_events, window),
                }
            }

            pub fn with_defaults() -> Self {
                Self::new($default_max, DEFAULT_WINDOW)
            }

            pub fn check_and_record(
                &self,
                jid: &BareJid,
            ) -> Result<ChargeToken, RateLimitExceeded> {
                self.inner.check_and_record(jid)
            }

            /// Refund exactly the charge identified by `token` — see
            /// [`PerBareJidSlidingWindowRateLimit::refund`].
            pub fn refund(&self, jid: &BareJid, token: ChargeToken) {
                self.inner.refund(jid, token)
            }

            #[cfg(test)]
            pub(crate) fn check_and_record_at(
                &self,
                jid: &BareJid,
                now: Instant,
            ) -> Result<ChargeToken, RateLimitExceeded> {
                self.inner.check_and_record_at(jid, now)
            }
        }
    };
}

define_rate_limit!(SessionInitiateRateLimit, DEFAULT_MAX_INITIATES);
define_rate_limit!(TerminateRateLimit, DEFAULT_MAX_TERMINATES);
define_rate_limit!(MujiActionRateLimit, DEFAULT_MAX_MUJI_ACTIONS);
define_rate_limit!(TurnCredentialRateLimit, DEFAULT_MAX_TURN_CREDENTIALS);

#[derive(Debug, Clone, Copy)]
pub struct RateLimitExceeded {
    pub window: Duration,
    pub max_events: usize,
}

impl std::fmt::Display for RateLimitExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "rate limit exceeded: more than {} requests within {:?}",
            self.max_events, self.window
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

    fn assert_under_budget<L>(limiter: &L)
    where
        L: CheckAt,
    {
        let alice = jid("alice@waddle.test");
        let now = Instant::now();
        assert!(limiter.check_at(&alice, now).is_ok());
        assert!(limiter
            .check_at(&alice, now + Duration::from_millis(10))
            .is_ok());
        assert!(limiter
            .check_at(&alice, now + Duration::from_millis(20))
            .is_ok());
    }

    fn assert_over_budget_rejects<L>(limiter: &L)
    where
        L: CheckAt,
    {
        let alice = jid("alice@waddle.test");
        let now = Instant::now();
        assert!(limiter.check_at(&alice, now).is_ok());
        assert!(limiter
            .check_at(&alice, now + Duration::from_millis(10))
            .is_ok());
        assert!(limiter
            .check_at(&alice, now + Duration::from_millis(20))
            .is_err());
    }

    fn assert_rejected_attempt_does_not_extend_lockout<L>(limiter: &L)
    where
        L: CheckAt,
    {
        let alice = jid("alice@waddle.test");
        let now = Instant::now();
        limiter.check_at(&alice, now).unwrap();
        limiter
            .check_at(&alice, now + Duration::from_millis(10))
            .unwrap();
        for i in 0..10 {
            assert!(limiter
                .check_at(&alice, now + Duration::from_millis(100 + i))
                .is_err());
        }
        let after = now + Duration::from_secs(31);
        assert!(limiter.check_at(&alice, after).is_ok());
    }

    fn assert_different_jids_have_independent_buckets<L>(limiter: &L)
    where
        L: CheckAt,
    {
        let alice = jid("alice@waddle.test");
        let bob = jid("bob@waddle.test");
        let now = Instant::now();
        assert!(limiter.check_at(&alice, now).is_ok());
        assert!(limiter.check_at(&bob, now).is_ok());
        assert!(limiter.check_at(&alice, now).is_err());
        assert!(limiter.check_at(&bob, now).is_err());
    }

    fn assert_window_drops_expired_entries<L>(limiter: &L)
    where
        L: CheckAt,
    {
        let alice = jid("alice@waddle.test");
        let now = Instant::now();
        limiter.check_at(&alice, now).unwrap();
        limiter
            .check_at(&alice, now + Duration::from_millis(100))
            .unwrap();
        assert!(limiter
            .check_at(&alice, now + Duration::from_millis(1500))
            .is_ok());
    }

    trait CheckAt {
        fn check_at(&self, jid: &BareJid, now: Instant) -> Result<(), RateLimitExceeded>;
    }

    impl CheckAt for SessionInitiateRateLimit {
        fn check_at(&self, jid: &BareJid, now: Instant) -> Result<(), RateLimitExceeded> {
            self.check_and_record_at(jid, now).map(|_| ())
        }
    }

    impl CheckAt for TerminateRateLimit {
        fn check_at(&self, jid: &BareJid, now: Instant) -> Result<(), RateLimitExceeded> {
            self.check_and_record_at(jid, now).map(|_| ())
        }
    }

    impl CheckAt for MujiActionRateLimit {
        fn check_at(&self, jid: &BareJid, now: Instant) -> Result<(), RateLimitExceeded> {
            self.check_and_record_at(jid, now).map(|_| ())
        }
    }

    impl CheckAt for TurnCredentialRateLimit {
        fn check_at(&self, jid: &BareJid, now: Instant) -> Result<(), RateLimitExceeded> {
            self.check_and_record_at(jid, now).map(|_| ())
        }
    }

    #[test]
    fn session_initiate_under_budget_allows_requests() {
        assert_under_budget(&SessionInitiateRateLimit::new(3, Duration::from_secs(30)));
    }

    #[test]
    fn session_initiate_over_budget_rejects() {
        assert_over_budget_rejects(&SessionInitiateRateLimit::new(2, Duration::from_secs(30)));
    }

    #[test]
    fn session_initiate_rejected_attempt_does_not_extend_lockout() {
        assert_rejected_attempt_does_not_extend_lockout(&SessionInitiateRateLimit::new(
            2,
            Duration::from_secs(30),
        ));
    }

    #[test]
    fn session_initiate_tracks_buckets_per_bare_jid() {
        assert_different_jids_have_independent_buckets(&SessionInitiateRateLimit::new(
            1,
            Duration::from_secs(30),
        ));
    }

    #[test]
    fn session_initiate_window_drops_expired_entries() {
        assert_window_drops_expired_entries(&SessionInitiateRateLimit::new(
            2,
            Duration::from_secs(1),
        ));
    }

    #[test]
    fn terminate_limiter_rejects_at_budget_and_recovers() {
        assert_over_budget_rejects(&TerminateRateLimit::new(2, Duration::from_secs(1)));
        assert_window_drops_expired_entries(&TerminateRateLimit::new(2, Duration::from_secs(1)));
    }

    #[test]
    fn muji_action_limiter_rejects_at_budget_and_recovers() {
        assert_over_budget_rejects(&MujiActionRateLimit::new(2, Duration::from_secs(1)));
        assert_window_drops_expired_entries(&MujiActionRateLimit::new(2, Duration::from_secs(1)));
    }

    #[test]
    fn turn_credential_limiter_rejects_at_budget_and_recovers() {
        assert_over_budget_rejects(&TurnCredentialRateLimit::new(2, Duration::from_secs(1)));
        assert_window_drops_expired_entries(&TurnCredentialRateLimit::new(
            2,
            Duration::from_secs(1),
        ));
    }
}
