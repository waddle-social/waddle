//! Periodic mailbox-depth gauge for the MUC `RoomRegistry` (#807).
//!
//! The #757 incident was a wedged registry actor with no actionable signal.
//! Besides the per-request reply timeout in
//! [`waddle_xmpp::muc::RoomRegistry`], this task emits the registry's mailbox
//! depth on a fixed interval so a backlog (the leading edge of a wedge) is
//! visible on dashboards/alerts before requests start timing out.

use std::time::Duration;

use tokio::time::interval;
use tokio_util::sync::CancellationToken;
use tracing::debug;
use waddle_xmpp::metrics;
use waddle_xmpp::muc::RoomRegistry;

/// How often the registry mailbox depth is sampled and recorded.
///
/// Matches the cadence of the existing state-inventory publisher (15s): fast
/// enough to catch a forming backlog, slow enough to be negligible overhead.
const GAUGE_INTERVAL: Duration = Duration::from_secs(15);

const ACTOR_LABEL: &str = "room_registry";

/// Spawn the mailbox-depth gauge task. It records
/// [`metrics::record_actor_mailbox_depth`] every [`GAUGE_INTERVAL`] until either
/// `stop_token` is cancelled or the registry actor stops.
pub(crate) fn spawn(registry: RoomRegistry, stop_token: CancellationToken) {
    tokio::spawn(async move {
        let mut ticker = interval(GAUGE_INTERVAL);
        let max_capacity = registry.max_capacity();
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    if !registry.is_alive() {
                        debug!("RoomRegistry actor stopped; ending mailbox-depth gauge");
                        break;
                    }
                    if let Some(depth) = registry.mailbox_depth() {
                        metrics::record_actor_mailbox_depth(
                            ACTOR_LABEL,
                            "all",
                            depth,
                            max_capacity,
                        );
                    }
                }
                _ = stop_token.cancelled() => break,
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use waddle_xmpp::xep::xep0421::OccupantIdSecret;

    fn test_registry() -> RoomRegistry {
        let secret = OccupantIdSecret::new(b"test-occupant-id-secret-32-bytes-long".to_vec())
            .expect("test occupant-id secret meets length floor");
        RoomRegistry::spawn("muc.example.com".to_string(), secret, None)
    }

    #[tokio::test]
    async fn gauge_task_runs_then_stops_on_cancel() {
        let registry = test_registry();
        let token = CancellationToken::new();
        spawn(registry.clone(), token.clone());
        // Let the task reach its select! and register the interval.
        tokio::task::yield_now().await;
        token.cancel();
        // The registry is still alive and usable after the gauge stops.
        assert!(registry.is_alive());
    }

    #[tokio::test]
    async fn gauge_reads_depth_zero_for_idle_registry() {
        let registry = test_registry();
        assert_eq!(registry.mailbox_depth(), Some(0));
        assert_eq!(registry.max_capacity(), 128);
    }
}
