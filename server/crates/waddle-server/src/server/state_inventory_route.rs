//! `/debug/state-inventory` — JSON snapshot of every long-lived
//! in-memory map's `.len()`.
//!
//! Purpose: the server's process RSS has grown faster than the
//! observable workload would explain, and the existing `/metrics`
//! Prometheus surface tracks rates / cumulative counters but not the
//! instantaneous size of the long-lived maps that are the most
//! likely culprits (room actors, avatar locks, auth flows, SM
//! sessions, caps cache, etc.). With this endpoint scraped at
//! 30 s and charted against RSS, the structure responsible for the
//! climb falls out of the data within minutes.
//!
//! Hardening:
//!
//! 1. Mounted only when a non-empty `WADDLE_DEBUG_STATE_TOKEN` env
//!    var is set at HTTP bootstrap. If unset, the route does not
//!    exist (404).
//! 2. Every request MUST carry `X-Waddle-Debug-Token` matching that
//!    token. Missing or wrong → 401.
//! 3. The response only reports map sizes — never keys, values, or
//!    user-identifying data. Safe to log.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use crate::server::routes::websocket::WebSocketState;

const AUTH_HEADER: &str = "x-waddle-debug-token";
const TOKEN_ENV: &str = "WADDLE_DEBUG_STATE_TOKEN";

#[derive(Debug, Clone)]
pub struct DebugStateAuth {
    pub token: String,
}

/// Read the debug token from the environment at HTTP bootstrap. Returns
/// `None` when the env var is missing or blank, in which case the
/// caller MUST NOT mount the route.
pub fn debug_state_auth_from_env() -> Option<DebugStateAuth> {
    std::env::var(TOKEN_ENV)
        .ok()
        .map(|raw| raw.trim().to_string())
        .filter(|raw| !raw.is_empty())
        .map(|token| DebugStateAuth { token })
}

#[derive(Clone)]
struct RouteState {
    websocket: Arc<WebSocketState>,
    auth: Arc<DebugStateAuth>,
}

#[derive(Debug, Serialize)]
struct InventoryResponse {
    auth: AuthInventory,
    profile: ProfileInventory,
    sessions: SessionInventory,
    caps: CapsInventory,
    connections: ConnectionInventory,
    rooms: RoomInventory,
}

#[derive(Debug, Serialize)]
struct RoomInventory {
    /// Total `RoomActor` instances tracked by `RoomRegistryActor`.
    /// Errors fall through as 0 to keep the endpoint best-effort.
    total: usize,
    /// Rooms that report `is_dormant() == true` — zero occupants AND
    /// no subject AND no pinned entries AND no in-memory affiliations.
    /// Reclaimable by the room dormancy janitor on its next pass; a
    /// growing value here is the canary signal that the janitor
    /// interval should be tightened.
    dormant: usize,
}

#[derive(Debug, Serialize)]
struct AuthInventory {
    pending_auth: usize,
    device_auth: usize,
    xmpp_auth_codes: usize,
    dynamic_oidc_clients: usize,
    dynamic_oidc_client_locks: usize,
}

#[derive(Debug, Serialize)]
struct ProfileInventory {
    avatar_source_locks: usize,
    profile_publish_tracker_in_flight: usize,
    provider_dispatch_tasks_in_flight: usize,
}

#[derive(Debug, Serialize)]
struct SessionInventory {
    sm_live_sessions: Option<usize>,
    resumable_sessions: usize,
}

#[derive(Debug, Serialize)]
struct CapsInventory {
    caps_cache: usize,
    pending_resolutions: usize,
}

#[derive(Debug, Serialize)]
struct ConnectionInventory {
    full_jid_connections: usize,
    pending_subscription_stanzas: usize,
    presence_states: usize,
    last_activity: usize,
}

pub fn router(websocket: Arc<WebSocketState>, auth: DebugStateAuth) -> Router {
    Router::new()
        .route("/debug/state-inventory", get(handler))
        .with_state(RouteState {
            websocket,
            auth: Arc::new(auth),
        })
}

async fn handler(
    State(state): State<RouteState>,
    headers: HeaderMap,
) -> Result<Json<InventoryResponse>, StatusCode> {
    let Some(provided) = headers
        .get(AUTH_HEADER)
        .and_then(|value| value.to_str().ok())
    else {
        return Err(StatusCode::UNAUTHORIZED);
    };
    if !constant_time_eq(provided.as_bytes(), state.auth.token.as_bytes()) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let ws = &state.websocket;
    let deps = &ws.deps;
    let auth_state = &deps.auth_state;
    let protocol = &deps.protocol;

    let sm_live_sessions = protocol
        .sm_session_registry
        .live_session_ids()
        .map(|ids| ids.len());

    let rooms = collect_room_inventory(ws).await;

    let response = InventoryResponse {
        auth: AuthInventory {
            pending_auth: auth_state.pending_auth.len(),
            device_auth: auth_state.device_auth.len(),
            xmpp_auth_codes: auth_state.xmpp_auth_codes.len(),
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
            resumable_sessions: protocol.resumable_sessions.len(),
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
        rooms,
    };
    Ok(Json(response))
}

/// Best-effort population of `RoomInventory`. Errors fall through as
/// zeros so the inventory endpoint stays operational even when the
/// room registry or a per-room actor is overloaded.
async fn collect_room_inventory(ws: &WebSocketState) -> RoomInventory {
    use waddle_xmpp::muc::room_actor::IsDormant;
    use waddle_xmpp::muc::room_registry_actor::{GetRoom, ListRooms, RoomCount};
    let total: usize = ws
        .deps
        .protocol
        .room_registry
        .ask(RoomCount)
        .await
        .unwrap_or(0);
    let room_list = match ws.deps.protocol.room_registry.ask(ListRooms).await {
        Ok(list) => list,
        Err(_) => {
            return RoomInventory { total, dormant: 0 };
        }
    };
    let mut dormant = 0usize;
    for room_jid in room_list {
        let Ok(Some(actor)) = ws
            .deps
            .protocol
            .room_registry
            .ask(GetRoom {
                room_jid: room_jid.clone(),
            })
            .await
        else {
            continue;
        };
        if matches!(actor.ask(IsDormant).await, Ok(true)) {
            dormant += 1;
        }
    }
    RoomInventory { total, dormant }
}

/// Constant-time byte slice comparison so the auth check cannot leak
/// the token via timing.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (lhs, rhs) in a.iter().zip(b.iter()) {
        diff |= lhs ^ rhs;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_state_auth_from_env_returns_none_when_unset() {
        // Use a known-untouched name so we don't disturb live env in
        // case another test runs in the same process.
        let prev = std::env::var(TOKEN_ENV).ok();
        std::env::remove_var(TOKEN_ENV);
        assert!(debug_state_auth_from_env().is_none());
        if let Some(prev) = prev {
            std::env::set_var(TOKEN_ENV, prev);
        }
    }

    #[test]
    fn debug_state_auth_from_env_returns_none_when_blank() {
        let prev = std::env::var(TOKEN_ENV).ok();
        std::env::set_var(TOKEN_ENV, "   ");
        assert!(debug_state_auth_from_env().is_none());
        match prev {
            Some(prev) => std::env::set_var(TOKEN_ENV, prev),
            None => std::env::remove_var(TOKEN_ENV),
        }
    }

    #[test]
    fn debug_state_auth_from_env_returns_some_when_set() {
        let prev = std::env::var(TOKEN_ENV).ok();
        std::env::set_var(TOKEN_ENV, "  shhh-secret  ");
        let auth = debug_state_auth_from_env().expect("non-blank value parses");
        assert_eq!(auth.token, "shhh-secret");
        match prev {
            Some(prev) => std::env::set_var(TOKEN_ENV, prev),
            None => std::env::remove_var(TOKEN_ENV),
        }
    }

    #[test]
    fn constant_time_eq_matches_only_identical_byte_strings() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(!constant_time_eq(b"abcd", b"abc"));
    }
}
