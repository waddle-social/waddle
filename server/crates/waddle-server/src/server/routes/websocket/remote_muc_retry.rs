use jid::FullJid;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use waddle_xmpp::ownership::{ClaimSnapshot, NodeIdentity};

const MAX_ACTIVE_FAILURES: u8 = 5;
const INITIAL_RETRY_DELAY: Duration = Duration::from_secs(30);
const RECONCILIATION_INTERVAL: Duration = Duration::from_secs(15 * 60);
const WARNING_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Shares the warning budget across all rooms and replacements of a full JID.
#[derive(Debug, Default)]
pub(super) struct RemoteMucCleanupWarnings {
    last_warnings: Mutex<HashMap<FullJid, Instant>>,
}

impl RemoteMucCleanupWarnings {
    pub(super) fn should_warn(&self, jid: &FullJid, now: Instant) -> bool {
        let mut warnings = self
            .last_warnings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        warnings.retain(|_, last| now.saturating_duration_since(*last) < WARNING_INTERVAL);
        if let std::collections::hash_map::Entry::Vacant(entry) = warnings.entry(jid.clone()) {
            entry.insert(now);
            true
        } else {
            false
        }
    }
}

/// Advisory retry eligibility; the cleanup path must still acquire a fenced claim.
pub(super) fn cleanup_claim_is_available(
    claim: Option<&ClaimSnapshot>,
    local: &NodeIdentity,
) -> bool {
    claim.is_none_or(|claim| claim.owner == *local || !claim.owner_lease_fresh)
}

/// Caps active retries while retaining responsibility for slow reconciliation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct RemoteMucCleanupRetry {
    failures: u8,
    next_attempt: Option<Instant>,
}

impl RemoteMucCleanupRetry {
    pub(super) fn ready(&self, now: Instant) -> bool {
        self.next_attempt.is_none_or(|deadline| now >= deadline)
    }

    /// Returns true once, when the active retry budget becomes exhausted.
    pub(super) fn failed(&mut self, now: Instant) -> bool {
        let previously_exhausted = self.exhausted();
        self.failures = self.failures.saturating_add(1);
        let delay = if self.exhausted() {
            RECONCILIATION_INTERVAL
        } else {
            INITIAL_RETRY_DELAY * (1 << (self.failures - 1))
        };
        self.next_attempt = Some(now + delay);
        !previously_exhausted && self.exhausted()
    }

    pub(super) fn exhausted(&self) -> bool {
        self.failures >= MAX_ACTIVE_FAILURES
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use waddle_xmpp::ownership::{ClaimEpoch, ClaimSnapshot, NodeIdentity};

    #[test]
    fn cleanup_is_available_for_missing_local_or_stale_claims() {
        let local = NodeIdentity::new("local", "current");
        assert!(cleanup_claim_is_available(None, &local));
        for owner in [local.clone(), NodeIdentity::new("foreign", "current")] {
            for owner_lease_fresh in [true, false] {
                let claim = ClaimSnapshot {
                    owner: owner.clone(),
                    claim_epoch: ClaimEpoch(7),
                    owner_lease_fresh,
                };
                assert_eq!(
                    cleanup_claim_is_available(Some(&claim), &local),
                    owner == local || !owner_lease_fresh
                );
            }
        }
    }

    #[test]
    fn a_fresh_claim_from_another_incarnation_is_foreign() {
        let local = NodeIdentity::new("local", "current");
        let claim = ClaimSnapshot {
            owner: NodeIdentity::new("local", "previous"),
            claim_epoch: ClaimEpoch(7),
            owner_lease_fresh: true,
        };
        assert!(!cleanup_claim_is_available(Some(&claim), &local));
    }

    #[test]
    fn fresh_state_is_ready() {
        let retry = RemoteMucCleanupRetry::default();
        let now = Instant::now();
        assert!(retry.ready(now));
        assert!(!retry.exhausted());
    }

    #[test]
    fn active_retries_use_exponential_backoff() {
        let mut retry = RemoteMucCleanupRetry::default();
        let mut now = Instant::now();
        for seconds in [30, 60, 120, 240] {
            assert!(!retry.failed(now));
            assert!(!retry.exhausted());
            assert!(!retry.ready(now + Duration::from_secs(seconds - 1)));
            now += Duration::from_secs(seconds);
            assert!(retry.ready(now));
        }
    }

    #[test]
    fn exhausted_retries_continue_slow_reconciliation_without_repeated_transition() {
        let mut retry = RemoteMucCleanupRetry::default();
        let mut now = Instant::now();
        for seconds in [30, 60, 120, 240] {
            assert!(!retry.failed(now));
            now += Duration::from_secs(seconds);
        }
        assert!(retry.failed(now));
        assert!(retry.exhausted());
        for _ in 0..300 {
            assert!(!retry.ready(now + Duration::from_secs(899)));
            now += Duration::from_secs(900);
            assert!(retry.ready(now));
            assert!(!retry.failed(now));
            assert!(retry.exhausted());
        }
    }

    #[test]
    fn warnings_are_limited_independently_of_retry_failures() {
        let warnings = RemoteMucCleanupWarnings::default();
        let jid = "user@example.test/resource".parse::<FullJid>().unwrap();
        let mut retry = RemoteMucCleanupRetry::default();
        let now = Instant::now();
        assert!(warnings.should_warn(&jid, now));
        assert!(!warnings.should_warn(&jid, now));
        assert!(!retry.failed(now));
        assert!(!warnings.should_warn(&jid, now + Duration::from_secs(299)));
        assert!(warnings.should_warn(&jid, now + Duration::from_secs(300)));
        assert!(!warnings.should_warn(&jid, now + Duration::from_secs(599)));
        assert!(warnings.should_warn(&jid, now + Duration::from_secs(600)));
        assert!(!retry.ready(now + Duration::from_secs(29)));
        assert!(retry.ready(now + Duration::from_secs(30)));
    }

    #[test]
    fn distinct_full_jids_have_independent_warning_budgets() {
        let warnings = RemoteMucCleanupWarnings::default();
        let first = "user@example.test/first".parse::<FullJid>().unwrap();
        let second = "user@example.test/second".parse::<FullJid>().unwrap();
        let now = Instant::now();
        assert!(warnings.should_warn(&first, now));
        assert!(warnings.should_warn(&second, now));
        assert!(!warnings.should_warn(&first, now));
        assert!(!warnings.should_warn(&second, now));
    }

    #[test]
    fn checking_warnings_prunes_expired_sessions() {
        let warnings = RemoteMucCleanupWarnings::default();
        let expired = "user@example.test/expired".parse::<FullJid>().unwrap();
        let current = "user@example.test/current".parse::<FullJid>().unwrap();
        let now = Instant::now();
        assert!(warnings.should_warn(&expired, now));
        assert!(warnings.should_warn(&current, now + Duration::from_secs(300)));
        let records = warnings.last_warnings.lock().unwrap();
        assert_eq!(records.len(), 1);
        assert!(records.contains_key(&current));
    }
}
