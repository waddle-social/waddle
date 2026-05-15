//! OTel publisher for the long-lived in-memory map sizes.
//!
//! Spawns a single tokio task that polls
//! [`crate::server::state_inventory::collect_snapshot`] every
//! [`STATE_INVENTORY_PUBLISH_INTERVAL`] and records each field as an
//! i64 gauge under `waddle.state.*`. The values flow through the
//! global OTel meter provider — wired to Alloy in
//! `crate::telemetry::init` — and land in Grafana Cloud Mimir
//! without any per-pod auth or port-forward.
//!
//! All gauges carry zero labels: the structure of interest is
//! "which map", not "which user/room/etc". Bounded cardinality
//! means OTel SDK aggregation is a no-op and there's no risk of a
//! per-JID cardinality blowup.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use opentelemetry::metrics::Gauge;
use tracing::debug;

use crate::server::routes::websocket::WebSocketState;
use crate::server::state_inventory::collect_snapshot;

/// How often the publisher polls and records. Chosen well under the
/// OTel periodic-reader default (60 s) so each export contains a
/// fresh value rather than a stale one.
pub const STATE_INVENTORY_PUBLISH_INTERVAL: Duration = Duration::from_secs(15);

fn meter() -> &'static opentelemetry::metrics::Meter {
    static METER: OnceLock<opentelemetry::metrics::Meter> = OnceLock::new();
    METER.get_or_init(|| opentelemetry::global::meter("waddle-server"))
}

macro_rules! gauge {
    ($name:literal, $description:literal) => {{
        static G: OnceLock<Gauge<i64>> = OnceLock::new();
        G.get_or_init(|| {
            meter()
                .i64_gauge($name)
                .with_description($description)
                .with_unit("entry")
                .build()
        })
    }};
}

/// Cast a `usize` length to `i64` for OTel without panicking on
/// (effectively impossible) overflow. Saturates at `i64::MAX` —
/// in production any `.len()` near that value is already a fatal bug
/// long before the metric matters.
fn as_i64(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// Spawn the periodic publisher. Wired alongside the existing
/// janitors at HTTP bootstrap.
pub(crate) fn spawn_state_inventory_publisher(websocket_state: &Arc<WebSocketState>) {
    let weak_state = Arc::downgrade(websocket_state);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(STATE_INVENTORY_PUBLISH_INTERVAL);
        // Skip the first immediate tick — a fresh process shouldn't
        // emit before any traffic has touched the maps.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let Some(state) = weak_state.upgrade() else {
                break;
            };
            publish_once(&state).await;
        }
    });
}

/// Take a snapshot and record every field as an OTel gauge sample.
/// Pure helper so unit tests can drive a single publish without the
/// long-running ticker.
pub(crate) async fn publish_once(websocket_state: &WebSocketState) {
    let snapshot = collect_snapshot(websocket_state).await;
    let labels: &[opentelemetry::KeyValue] = &[];

    // Auth state — the three DashMaps swept by spawn_auth_state_janitor,
    // plus the OIDC dynamic-client cache they share state with.
    gauge!(
        "waddle.state.auth.pending_auth",
        "Live PendingAuthorization entries awaiting OAuth callback."
    )
    .record(as_i64(snapshot.auth.pending_auth), labels);
    gauge!(
        "waddle.state.auth.device_auth",
        "Live DeviceAuthorization entries awaiting device-flow approval."
    )
    .record(as_i64(snapshot.auth.device_auth), labels);
    gauge!(
        "waddle.state.auth.xmpp_auth_codes",
        "Live XmppAuthCode entries awaiting XMPP OAuth token exchange."
    )
    .record(as_i64(snapshot.auth.xmpp_auth_codes), labels);
    gauge!(
        "waddle.state.auth.dynamic_oidc_clients",
        "Cached dynamic OIDC client registrations."
    )
    .record(as_i64(snapshot.auth.dynamic_oidc_clients), labels);
    gauge!(
        "waddle.state.auth.dynamic_oidc_client_locks",
        "Locks guarding dynamic OIDC client registration races."
    )
    .record(as_i64(snapshot.auth.dynamic_oidc_client_locks), labels);

    // Profile / publish trackers — the avatar-source lock map and the
    // two TaskTrackers for OIDC profile publishes and provider webhook
    // dispatches.
    gauge!(
        "waddle.state.profile.avatar_source_locks",
        "Per-BareJid avatar-publish mutexes currently held or waiting."
    )
    .record(as_i64(snapshot.profile.avatar_source_locks), labels);
    gauge!(
        "waddle.state.profile.publish_tracker_in_flight",
        "OIDC profile-publish background tasks currently in flight."
    )
    .record(
        as_i64(snapshot.profile.profile_publish_tracker_in_flight),
        labels,
    );
    gauge!(
        "waddle.state.profile.provider_dispatch_in_flight",
        "Provider webhook dispatch tasks currently in flight."
    )
    .record(
        as_i64(snapshot.profile.provider_dispatch_tasks_in_flight),
        labels,
    );

    // Stream management — detached XEP-0198 sessions and their
    // sidecar resumable-session map.
    gauge!(
        "waddle.state.sessions.sm_live_sessions",
        "Detached XEP-0198 sessions in the SM session registry."
    )
    .record(
        as_i64(snapshot.sessions.sm_live_sessions.unwrap_or(0)),
        labels,
    );
    gauge!(
        "waddle.state.sessions.resumable_sessions",
        "Sidecar resumable_sessions entries awaiting take or expiry."
    )
    .record(as_i64(snapshot.sessions.resumable_sessions), labels);

    // XEP-0115 entity capabilities — the LRU cache and the in-flight
    // pending-resolution table.
    gauge!(
        "waddle.state.caps.caps_cache",
        "Cached XEP-0115 disco#info bodies (LRU-bounded)."
    )
    .record(as_i64(snapshot.caps.caps_cache), labels);
    gauge!(
        "waddle.state.caps.pending_resolutions",
        "Outstanding XEP-0115 disco#info resolutions awaiting client reply."
    )
    .record(as_i64(snapshot.caps.pending_resolutions), labels);

    // Connection registry — the four DashMaps inside ConnectionRegistry.
    gauge!(
        "waddle.state.connections.full_jid_connections",
        "Live full-JID WebSocket connections registered with the server."
    )
    .record(as_i64(snapshot.connections.full_jid_connections), labels);
    gauge!(
        "waddle.state.connections.pending_subscription_stanzas",
        "Bare JIDs holding queued XEP-0162 subscription stanzas for offline peers."
    )
    .record(
        as_i64(snapshot.connections.pending_subscription_stanzas),
        labels,
    );
    gauge!(
        "waddle.state.connections.presence_states",
        "Bare JIDs with tracked presence state."
    )
    .record(as_i64(snapshot.connections.presence_states), labels);
    gauge!(
        "waddle.state.connections.last_activity",
        "Bare JIDs with recorded XEP-0012 last-activity timestamps."
    )
    .record(as_i64(snapshot.connections.last_activity), labels);

    // MUC rooms — total and the sub-count that the dormancy janitor
    // will reclaim on its next pass.
    gauge!(
        "waddle.state.rooms.total",
        "Total RoomActor instances tracked by RoomRegistryActor."
    )
    .record(as_i64(snapshot.rooms.total), labels);
    gauge!(
        "waddle.state.rooms.dormant",
        "Rooms reported by IsDormant — reclaimable on the next dormancy janitor tick."
    )
    .record(as_i64(snapshot.rooms.dormant), labels);

    debug!(
        rooms_total = snapshot.rooms.total,
        rooms_dormant = snapshot.rooms.dormant,
        sm_live = ?snapshot.sessions.sm_live_sessions,
        full_jid_conns = snapshot.connections.full_jid_connections,
        "state-inventory publisher: recorded gauge sample"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::routes::websocket::tests::create_test_websocket_state;

    #[tokio::test]
    async fn publish_once_does_not_panic_on_fresh_state() {
        let state = create_test_websocket_state().await;
        // Smoke test — the test harness's WebSocketState has every
        // map at zero; we just want to confirm the publisher walks
        // them all without panicking.
        publish_once(&state).await;
    }

    #[test]
    fn as_i64_saturates_at_max() {
        assert_eq!(super::as_i64(0), 0);
        assert_eq!(super::as_i64(42), 42);
        assert_eq!(super::as_i64(usize::MAX), i64::MAX);
    }
}
