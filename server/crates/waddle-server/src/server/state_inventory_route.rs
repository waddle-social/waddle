//! `/debug/state-inventory` — JSON snapshot of every long-lived
//! in-memory map's `.len()`.
//!
//! In production, the primary surface for these values is the OTel
//! gauge stream published by
//! [`crate::server::state_inventory_metrics`] — those values flow
//! through Alloy into Grafana Cloud without any port-forward or
//! per-pod auth. This route is the operator backstop: a single
//! `curl` from inside the cluster (or via port-forward) returns the
//! same shape as the gauges, useful when Grafana isn't reachable.
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

use crate::server::routes::websocket::WebSocketState;
use crate::server::state_inventory::{collect_snapshot, StateInventorySnapshot};

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
) -> Result<Json<StateInventorySnapshot>, StatusCode> {
    let Some(provided) = headers
        .get(AUTH_HEADER)
        .and_then(|value| value.to_str().ok())
    else {
        return Err(StatusCode::UNAUTHORIZED);
    };
    if !constant_time_eq(provided.as_bytes(), state.auth.token.as_bytes()) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    Ok(Json(collect_snapshot(&state.websocket).await))
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
    use std::sync::{Mutex, OnceLock};

    /// Serialise the env-var tests against each other so parallel
    /// `cargo test` execution can't race them on a single
    /// process-global env slot.
    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn debug_state_auth_from_env_returns_none_when_unset() {
        let _guard = env_lock().lock().expect("lock should not be poisoned");
        let prev = std::env::var(TOKEN_ENV).ok();
        std::env::remove_var(TOKEN_ENV);
        assert!(debug_state_auth_from_env().is_none());
        if let Some(prev) = prev {
            std::env::set_var(TOKEN_ENV, prev);
        }
    }

    #[test]
    fn debug_state_auth_from_env_returns_none_when_blank() {
        let _guard = env_lock().lock().expect("lock should not be poisoned");
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
        let _guard = env_lock().lock().expect("lock should not be poisoned");
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
