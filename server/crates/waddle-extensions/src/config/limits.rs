//! Resource, network, observation, and execution limits applied to extension invocations.

use crate::types::{ObservationGeneration, RoomObservationScope};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Explicit operator opt-in to a generated, scoped, concurrency-bounded room observer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoomObservationConfig {
    /// Positive operator generation used to fence stale observation configuration.
    pub generation: ObservationGeneration,
    /// Scope in which the route, command, or observation applies.
    pub scope: RoomObservationScope,
    /// Per-node limit; each replica has its own bounded executor.
    #[serde(default = "default_concurrency")]
    pub max_concurrent: u32,
}
fn default_concurrency() -> u32 {
    4
}
impl RoomObservationConfig {
    /// Validate positive generation, per-node concurrency bounds, and uniqueness of
    /// explicitly selected rooms.
    ///
    /// # Errors
    ///
    /// Returns an explanatory error for a zero generation, concurrency outside 1..=64, or
    /// duplicate explicit room selections.
    pub fn validate(&self) -> Result<(), String> {
        if self.generation.get() == 0 {
            return Err("observation generation must be positive".into());
        }
        if !(1..=64).contains(&self.max_concurrent) {
            return Err("observation max_concurrent must be 1..=64".into());
        }
        if let RoomObservationScope::Rooms(rooms) = &self.scope {
            let distinct: HashSet<_> = rooms.iter().collect();
            if distinct.len() != rooms.len() {
                return Err("observation room scope contains duplicate rooms".into());
            }
        }
        Ok(())
    }
}

/// Per-invocation deadlines, Wasm resource budgets, and outbound HTTP bounds.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RuntimeLimits {
    /// Maximum elapsed time for one guest initialization or event call, in milliseconds.
    pub invocation_timeout_ms: u32,
    /// Wasmtime instruction-fuel budget for one invocation.
    pub wasm_fuel: u64,
    /// Per linear memory, with at most four memories per store.
    pub memory_bytes: usize,
    /// Maximum elapsed time for each outbound HTTP request, in milliseconds.
    pub http_timeout_ms: u32,
    /// Maximum body size in bytes for one outbound HTTP request.
    pub http_max_request_bytes: u32,
    /// Maximum response body bytes accepted from one outbound HTTP request.
    pub http_max_response_bytes: u32,
    /// Maximum outbound HTTP requests admitted during one invocation.
    pub http_max_requests: u32,
}
impl Default for RuntimeLimits {
    fn default() -> Self {
        Self {
            invocation_timeout_ms: 45_000,
            wasm_fuel: 50_000_000,
            memory_bytes: 64 * 1024 * 1024,
            http_timeout_ms: 30_000,
            http_max_request_bytes: 256 * 1024,
            http_max_response_bytes: 1024 * 1024,
            http_max_requests: 32,
        }
    }
}
impl RuntimeLimits {
    /// Validate bounded deadlines, fuel, memory, HTTP body sizes, and request counts.
    ///
    /// # Errors
    ///
    /// Returns an explanatory error if deadlines, Wasm fuel/memory, HTTP body-size
    /// limits, or request count exceed the host bounds, or if the HTTP timeout exceeds
    /// the invocation timeout.
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=120_000).contains(&self.invocation_timeout_ms)
            || !(1..=self.invocation_timeout_ms).contains(&self.http_timeout_ms)
            || !(1..=1_000_000_000).contains(&self.wasm_fuel)
            || !(1024 * 1024..=256 * 1024 * 1024).contains(&self.memory_bytes)
            || !(1..=1024 * 1024).contains(&self.http_max_request_bytes)
            || !(1..=4 * 1024 * 1024).contains(&self.http_max_response_bytes)
            || !(1..=64).contains(&self.http_max_requests)
        {
            return Err("extension runtime limits are outside host bounds".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_unbounded_and_inconsistent_limits() {
        let mut limits = RuntimeLimits::default();
        assert!(limits.validate().is_ok());
        limits.http_max_requests = 0;
        assert!(limits.validate().is_err());
        limits.http_max_requests = 1;
        limits.http_timeout_ms = limits.invocation_timeout_ms + 1;
        assert!(limits.validate().is_err());
    }
    #[test]
    fn empty_scope_is_an_explicit_revocation_generation() {
        let config: RoomObservationConfig = serde_json::from_value(
            serde_json::json!({"generation":2,"scope":{"kind":"rooms","rooms":[]}}),
        )
        .expect("revocation config");
        assert!(config.validate().is_ok());
        assert!(!config
            .scope
            .includes(&"room@rooms.example.test".parse().unwrap()));
    }
    #[test]
    fn all_rooms_requires_an_explicit_selector() {
        assert!(serde_json::from_value::<RoomObservationConfig>(
            serde_json::json!({"generation":1})
        )
        .is_err());
        let config: RoomObservationConfig = serde_json::from_value(
            serde_json::json!({"generation":1,"scope":{"kind":"all-hosted-rooms"}}),
        )
        .expect("explicit scope");
        assert!(config.validate().is_ok());
    }
}
