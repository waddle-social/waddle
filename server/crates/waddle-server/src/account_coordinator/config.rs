use std::num::NonZeroUsize;
use std::time::Duration;

/// Bounds of one account coordinator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountCoordinatorConfig {
    /// Resource slots one account may hold, bound or free.
    pub max_resources: NonZeroUsize,
    /// Messages a coordinator mailbox holds before it refuses one.
    pub mailbox_capacity: usize,
    /// Longest a caller waits for mailbox room.
    pub mailbox_timeout: Duration,
    /// Longest a caller waits for a reply once its message is queued.
    pub reply_timeout: Duration,
    /// How long a started retirement may wait for its endpoint evidence
    /// before it is reported for repair.
    pub retirement_deadline: Duration,
    /// Started retirements one account may have awaiting evidence.
    pub max_pending_retirements: usize,
    /// How long a pending replacement reserves its resource.
    pub replacement_deadline: Duration,
    /// How long an account with no binding stays loaded.
    pub idle_after: Duration,
}

impl Default for AccountCoordinatorConfig {
    fn default() -> Self {
        Self {
            max_resources: NonZeroUsize::MIN.saturating_add(31),
            mailbox_capacity: 256,
            mailbox_timeout: Duration::from_secs(2),
            reply_timeout: Duration::from_secs(10),
            retirement_deadline: Duration::from_secs(30),
            max_pending_retirements: 32,
            replacement_deadline: Duration::from_secs(30),
            idle_after: Duration::from_secs(300),
        }
    }
}
