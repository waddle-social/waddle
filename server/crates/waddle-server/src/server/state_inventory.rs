//! Shared collection of the server's long-lived in-memory map sizes.
//!
//! The same data feeds two surfaces:
//!
//! - [`crate::server::state_inventory_route`] — the operator-side
//!   `/debug/state-inventory` JSON endpoint, gated on a token. Useful
//!   when Grafana isn't reachable from where the operator is sitting.
//! - [`crate::server::state_inventory_metrics`] — a periodic
//!   publisher that records each field as an OTel gauge. This is the
//!   primary surface in production: the values flow through Alloy
//!   into Grafana Cloud and chart against `process_resident_memory_bytes`
//!   without any port-forward or per-pod auth dance.

use serde::Serialize;

use crate::server::routes::websocket::WebSocketState;

/// Typed snapshot of every long-lived map's `.len()`. Field grouping
/// mirrors the layers of the server (auth → profile → sessions →
/// caps → connections → rooms) so the Grafana panels can be read in
/// order from "is this a login storm?" to "is this an MUC growth
/// problem?".
#[derive(Debug, Clone, Serialize)]
pub struct StateInventorySnapshot {
    pub auth: AuthInventory,
    pub profile: ProfileInventory,
    pub sessions: SessionInventory,
    pub caps: CapsInventory,
    pub connections: ConnectionInventory,
    pub rooms: RoomInventory,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthInventory {
    pub pending_auth: usize,
    pub device_auth: usize,
    pub xmpp_auth_codes: usize,
    pub dynamic_oidc_clients: usize,
    pub dynamic_oidc_client_locks: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProfileInventory {
    pub avatar_source_locks: usize,
    pub profile_publish_tracker_in_flight: usize,
    pub provider_dispatch_tasks_in_flight: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionInventory {
    /// `None` only when the SM-session registry's internal locks are
    /// poisoned — observed as a gap in the Grafana series. We keep
    /// it nullable here rather than collapsing to `0` so the gap is
    /// visually distinct from "zero sessions live".
    pub sm_live_sessions: Option<usize>,
    /// Durable detached XEP-0198 snapshots, including a resume claim that is
    /// temporarily frozen in the registry. This is not an authorization
    /// sidecar; every resumable snapshot carries its principal in persistence.
    pub resumable_sessions: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CapsInventory {
    pub caps_cache: usize,
    pub pending_resolutions: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConnectionInventory {
    pub full_jid_connections: usize,
    pub pending_subscription_stanzas: usize,
    pub presence_states: usize,
    pub last_activity: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct RoomInventory {
    /// Total `RoomActor` instances tracked by `RoomRegistryActor`.
    pub total: usize,
    /// Rooms that report `is_dormant() == true` — zero occupants AND
    /// no subject AND no pinned entries AND no in-memory affiliations.
    /// Reclaimable by the room dormancy janitor on its next pass.
    pub dormant: usize,
}

/// Collect a snapshot of every state-inventory field. Best-effort:
/// any individual actor `ask` that fails falls through as 0 (or
/// `None` for `sm_live_sessions`) so the publisher / endpoint never
/// fail hard on a single hung actor.
pub async fn collect_snapshot(ws: &WebSocketState) -> StateInventorySnapshot {
    use waddle_xmpp::muc::room_actor::IsDormant;
    use waddle_xmpp::muc::room_registry_actor::{GetRoom, ListRooms};
    use waddle_xmpp::muc::RoomRegistry;

    let deps = &ws.deps;
    let auth_state = &deps.auth_state;
    let protocol = &deps.protocol;

    let sm_live_sessions = protocol
        .sm_session_registry
        .live_session_ids()
        .map(|ids| ids.len());

    let rooms_total: usize = RoomRegistry::wrap(protocol.room_registry.clone())
        .room_count()
        .await
        .unwrap_or(0);
    let room_list = protocol
        .room_registry
        .ask(ListRooms)
        .await
        .unwrap_or_default();
    let mut rooms_dormant = 0usize;
    for room_jid in room_list {
        let Ok(Some(actor)) = protocol
            .room_registry
            .ask(GetRoom {
                room_jid: room_jid.clone(),
            })
            .await
        else {
            continue;
        };
        if actor
            .ask(IsDormant)
            .await
            .map(|status| status.dormant)
            .unwrap_or(false)
        {
            rooms_dormant += 1;
        }
    }

    // Handshake entries live in the shared database (#1336), so the
    // counts are DB-wide rather than per-process; a query failure is
    // reported as zeros rather than dropping the whole snapshot.
    let handshake_counts = auth_state
        .auth_handshake
        .counts()
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(error = %error, "state inventory: auth handshake counts unavailable");
            Default::default()
        });

    StateInventorySnapshot {
        auth: AuthInventory {
            pending_auth: handshake_counts.pending_auth,
            device_auth: handshake_counts.device_auth,
            xmpp_auth_codes: handshake_counts.xmpp_auth_codes,
            dynamic_oidc_clients: auth_state.dynamic_oidc_clients.len(),
            dynamic_oidc_client_locks: auth_state.dynamic_oidc_client_locks.len(),
        },
        profile: ProfileInventory {
            avatar_source_locks: protocol.avatar_source_locks.len(),
            profile_publish_tracker_in_flight: protocol.profile_publish_tracker.len(),
            provider_dispatch_tasks_in_flight: deps.provider_dispatch_tasks.len(),
        },
        sessions: SessionInventory {
            sm_live_sessions,
            resumable_sessions: protocol
                .sm_session_registry
                .live_session_ids()
                .map(|ids| ids.len()),
        },
        caps: CapsInventory {
            caps_cache: protocol.caps_resolver.cache().len(),
            pending_resolutions: protocol.caps_resolver.pending_len(),
        },
        connections: ConnectionInventory {
            full_jid_connections: protocol.connection_registry.connection_count(),
            pending_subscription_stanzas: protocol.connection_registry.pending_subscription_count(),
            presence_states: protocol.connection_registry.presence_state_count(),
            last_activity: protocol.connection_registry.last_activity_count(),
        },
        rooms: RoomInventory {
            total: rooms_total,
            dormant: rooms_dormant,
        },
    }
}
