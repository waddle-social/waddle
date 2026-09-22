//! Prompt mirror retirement is independent of the five-minute empty-actor reaper.
use super::{janitor_sweep_span, Janitor, WebSocketState};
use std::{sync::Arc, time::Duration};
use tracing::Instrument;

const REMOTE_OWNER_MIRROR_INTERVAL: Duration = Duration::from_secs(1);

pub(crate) fn remote_owner_mirror_ticker() -> tokio::time::Interval {
    let mut ticker = tokio::time::interval(REMOTE_OWNER_MIRROR_INTERVAL);
    // A slow backend must not cause a burst of catch-up pages once it recovers.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ticker
}

pub(crate) fn spawn_remote_owner_mirror_janitor(websocket_state: &Arc<WebSocketState>) {
    let Some(bridge) = websocket_state
        .deps
        .app_state
        .clustering_claims
        .ordered_relay_delivery_bridge
        .as_ref()
    else {
        return;
    };
    let weak_bridge = Arc::downgrade(bridge);
    tokio::spawn(async move {
        let mut ticker = remote_owner_mirror_ticker();
        loop {
            ticker.tick().await;
            let Some(bridge) = weak_bridge.upgrade() else {
                break;
            };
            let outcome = bridge
                .sweep_remote_owner_resources()
                .instrument(janitor_sweep_span(Janitor::RemoteOwnerMirror))
                .await;
            waddle_xmpp::telemetry::reliability::record_janitor_sweep(
                Janitor::RemoteOwnerMirror,
                outcome,
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn slow_mirror_sweep_skips_missed_ticks_instead_of_bursting() {
        let mut ticker = remote_owner_mirror_ticker();
        ticker.tick().await;
        tokio::time::advance(Duration::from_secs(30)).await;
        ticker.tick().await;
        assert!(
            tokio::time::timeout(Duration::from_millis(1), ticker.tick())
                .await
                .is_err()
        );
    }
}
