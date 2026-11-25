//! XEP-0060 §16.4.4 `pubsub#item_expire`: the janitor that purges items past
//! their node's retention and tells subscribers.
//!
//! The storage backend removes the items; this module sends the retraction
//! notification for each one (XEP-0060 §7.2: a node with
//! `pubsub#notify_retract` notifies subscribers of every retraction, expiry
//! included) and drops the XEP-0470 attachment state hanging off an expired
//! story.

use std::sync::Arc;
use std::time::Duration;

use tracing::{debug, warn};
use waddle_xmpp::pubsub::ExpiredItem;
use waddle_xmpp::telemetry::attributes::{Janitor, MetricAttribute, SweepOutcome};

use crate::server::routes::websocket::handlers::iq::cleanup_story_attachment_state;
use crate::server::routes::websocket::handlers::pubsub_fanout;
use crate::server::routes::websocket::WebSocketState;

/// Environment variable overriding the sweep period, in seconds.
const SWEEP_INTERVAL_ENV: &str = "WADDLE_PUBSUB_ITEM_EXPIRY_SWEEP_SECS";
const DEFAULT_SWEEP_INTERVAL_SECS: u64 = 30;
const MIN_SWEEP_INTERVAL_SECS: u64 = 1;
const MAX_SWEEP_INTERVAL_SECS: u64 = 3_600;

fn sweep_interval() -> Duration {
    let secs = std::env::var(SWEEP_INTERVAL_ENV)
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .map_or(DEFAULT_SWEEP_INTERVAL_SECS, |secs| {
            secs.clamp(MIN_SWEEP_INTERVAL_SECS, MAX_SWEEP_INTERVAL_SECS)
        });
    Duration::from_secs(secs)
}

/// Start the item-expiry janitor. The task ends when the server state drops.
pub(crate) fn spawn_pubsub_item_expiry_janitor(websocket_state: &Arc<WebSocketState>) {
    let weak_state = Arc::downgrade(websocket_state);
    let interval = sweep_interval();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let Some(state) = weak_state.upgrade() else {
                break;
            };
            run_item_expiry_sweep(&state).await;
        }
    });
}

async fn run_item_expiry_sweep(state: &WebSocketState) {
    use tracing::Instrument as _;
    let outcome = async {
        match state
            .deps
            .protocol
            .pubsub_storage
            .purge_expired_items(chrono::Utc::now())
            .await
        {
            Ok(expired) => {
                if !expired.is_empty() {
                    debug!(count = expired.len(), "pubsub item expiry: purged items");
                }
                for item in &expired {
                    announce_expired_item(state, item).await;
                }
                SweepOutcome::Completed
            }
            Err(error) => {
                warn!(error = %error, "pubsub item expiry sweep failed; will retry next tick");
                SweepOutcome::Failed
            }
        }
    }
    .instrument(tracing::info_span!(
        parent: None,
        "janitor.sweep",
        janitor = Janitor::PubsubItemExpiry.value()
    ))
    .await;
    waddle_xmpp::telemetry::reliability::record_janitor_sweep(Janitor::PubsubItemExpiry, outcome);
}

async fn announce_expired_item(state: &WebSocketState, item: &ExpiredItem) {
    let is_pep = item.owner.node().is_some();
    let request = pubsub_fanout::FanOutRetractRequest {
        owner: &item.owner,
        node: &item.node_name,
        item_id: &item.item_id,
    };
    if is_pep {
        pubsub_fanout::fan_out_pep_retract(state, request, false).await;
    } else {
        pubsub_fanout::fan_out_retract(state, request).await;
    }
    if is_pep && item.node_name == waddle_xmpp_core::xep0501::PUBSUB_NODE_STORIES {
        if let Err(error) = cleanup_story_attachment_state(state, &item.owner, &item.item_id).await
        {
            warn!(
                item_id = %item.item_id,
                error = ?error,
                "pubsub item expiry: failed to clean story attachment state"
            );
        }
    }
}
