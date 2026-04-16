//! Per-sender fixed-window rate limiter.
//!
//! One bucket per bare JID. Up to [`RateConfig::capacity`] enrichments
//! allowed in any rolling [`RateConfig::window`]. Exceeding the budget
//! silently drops enrichment for that message.
//!
//! The limiter uses a fixed-window counter instead of a leaky-bucket
//! because enrichment is a coarse, message-level operation — sub-second
//! smoothing adds no value and complicates testing.

use std::time::{Duration, Instant};

use dashmap::DashMap;
use jid::BareJid;

#[derive(Debug, Clone)]
pub struct RateConfig {
    pub capacity: u32,
    pub window: Duration,
}

impl Default for RateConfig {
    fn default() -> Self {
        Self {
            capacity: 30,
            window: Duration::from_secs(60),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Bucket {
    count: u32,
    window_start: Instant,
}

pub struct RateLimiter {
    config: RateConfig,
    buckets: DashMap<BareJid, Bucket>,
}

impl RateLimiter {
    pub fn new(config: RateConfig) -> Self {
        Self {
            config,
            buckets: DashMap::new(),
        }
    }

    /// Try to consume a slot for `jid`; returns `true` if allowed,
    /// `false` if the bucket is full.
    pub fn try_acquire(&self, jid: &BareJid, now: Instant) -> bool {
        let mut entry = self.buckets.entry(jid.clone()).or_insert(Bucket {
            count: 0,
            window_start: now,
        });

        if now.duration_since(entry.window_start) >= self.config.window {
            entry.count = 0;
            entry.window_start = now;
        }

        if entry.count < self.config.capacity {
            entry.count += 1;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn alice() -> BareJid {
        BareJid::from_str("alice@example.com").unwrap()
    }
    fn bob() -> BareJid {
        BareJid::from_str("bob@example.com").unwrap()
    }

    fn limiter(capacity: u32, window_secs: u64) -> RateLimiter {
        RateLimiter::new(RateConfig {
            capacity,
            window: Duration::from_secs(window_secs),
        })
    }

    #[test]
    fn allows_up_to_capacity_in_window() {
        let rl = limiter(3, 60);
        let now = Instant::now();
        assert!(rl.try_acquire(&alice(), now));
        assert!(rl.try_acquire(&alice(), now));
        assert!(rl.try_acquire(&alice(), now));
        assert!(!rl.try_acquire(&alice(), now));
    }

    #[test]
    fn refills_after_window() {
        let rl = limiter(2, 60);
        let t0 = Instant::now();
        assert!(rl.try_acquire(&alice(), t0));
        assert!(rl.try_acquire(&alice(), t0));
        assert!(!rl.try_acquire(&alice(), t0 + Duration::from_secs(30)));
        // After window elapses, reset.
        assert!(rl.try_acquire(&alice(), t0 + Duration::from_secs(60)));
        assert!(rl.try_acquire(&alice(), t0 + Duration::from_secs(60)));
        assert!(!rl.try_acquire(&alice(), t0 + Duration::from_secs(60)));
    }

    #[test]
    fn buckets_are_per_jid() {
        let rl = limiter(1, 60);
        let now = Instant::now();
        assert!(rl.try_acquire(&alice(), now));
        assert!(!rl.try_acquire(&alice(), now));
        assert!(rl.try_acquire(&bob(), now));
    }

    #[test]
    fn default_config_is_30_per_60s() {
        let cfg = RateConfig::default();
        assert_eq!(cfg.capacity, 30);
        assert_eq!(cfg.window, Duration::from_secs(60));
    }

    #[test]
    fn boundary_exact_window_duration_resets() {
        // Requests at exactly t=0 and t=window should both succeed in a
        // 1-capacity bucket, because the >= window check resets the bucket.
        let rl = limiter(1, 60);
        let t0 = Instant::now();
        assert!(rl.try_acquire(&alice(), t0));
        assert!(!rl.try_acquire(&alice(), t0 + Duration::from_secs(59)));
        assert!(rl.try_acquire(&alice(), t0 + Duration::from_secs(60)));
    }
}
