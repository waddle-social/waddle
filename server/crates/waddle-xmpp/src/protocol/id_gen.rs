//! Pure-handler entropy source for XEP-0359 stanza-id stamping.
//!
//! Handlers must be pure (no `rand::thread_rng()` calls — that's hidden I/O
//! for tests). [`IdGenerator`] is the injection point: production wires
//! [`UuidV4Generator`] (UUIDv4 satisfies XEP-0359 §6 — opaque,
//! collision-resistant, unique within `by=` scope, and the de-facto XMPP
//! standard); tests wire [`FixedIdGenerator`] or [`CounterIdGenerator`]
//! for deterministic assertions on emitted stanza-ids.
//!
//! The trait carries `Send + Sync` because the dispatcher and its
//! `MessageContext` snapshot may be shared across threads.

use std::sync::atomic::{AtomicU64, Ordering};

/// Source of fresh, opaque stanza-id values for handler stamping.
///
/// Per XEP-0359 §6 the only requirements on the value are:
///
/// 1. unique within the scope of a single `by=` attribute,
/// 2. opaque (no externally-visible structure),
/// 3. collision-resistant in practice.
///
/// UUIDv4 satisfies all three.
pub trait IdGenerator: Send + Sync {
    /// Return a fresh, opaque, collision-resistant id string.
    fn fresh_stanza_id(&self) -> String;
}

/// Production implementation: random UUIDv4 strings (with hyphens).
///
/// Same shape as the ids stamped by Prosody, ejabberd, and Openfire, so
/// any cross-server tooling that grew up around those servers stays
/// compatible.
#[derive(Debug, Default, Clone, Copy)]
pub struct UuidV4Generator;

impl IdGenerator for UuidV4Generator {
    fn fresh_stanza_id(&self) -> String {
        uuid::Uuid::new_v4().to_string()
    }
}

/// Test impl that returns the same id every call. Useful for snapshot
/// assertions on a single dispatch.
#[derive(Debug, Clone)]
pub struct FixedIdGenerator(pub String);

impl IdGenerator for FixedIdGenerator {
    fn fresh_stanza_id(&self) -> String {
        self.0.clone()
    }
}

/// Test impl that returns `prefix-1`, `prefix-2`, … in call order. Useful
/// for asserting on multiple stamp events within a single test scenario.
#[derive(Debug)]
pub struct CounterIdGenerator {
    prefix: String,
    next: AtomicU64,
}

impl CounterIdGenerator {
    /// Construct a counter starting at 1.
    pub fn new(prefix: impl Into<String>) -> Self {
        Self {
            prefix: prefix.into(),
            next: AtomicU64::new(1),
        }
    }
}

impl IdGenerator for CounterIdGenerator {
    fn fresh_stanza_id(&self) -> String {
        let n = self.next.fetch_add(1, Ordering::SeqCst);
        format!("{}-{n}", self.prefix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_v4_generator_produces_distinct_ids() {
        let gen = UuidV4Generator;
        let a = gen.fresh_stanza_id();
        let b = gen.fresh_stanza_id();
        assert_ne!(a, b);
        // UUIDv4 string form: 8-4-4-4-12 with hyphens.
        assert_eq!(a.len(), 36);
        assert_eq!(a.matches('-').count(), 4);
    }

    #[test]
    fn fixed_generator_returns_same_id_each_call() {
        let gen = FixedIdGenerator("stable-1".to_string());
        assert_eq!(gen.fresh_stanza_id(), "stable-1");
        assert_eq!(gen.fresh_stanza_id(), "stable-1");
    }

    #[test]
    fn counter_generator_returns_sequential_ids() {
        let gen = CounterIdGenerator::new("test");
        assert_eq!(gen.fresh_stanza_id(), "test-1");
        assert_eq!(gen.fresh_stanza_id(), "test-2");
        assert_eq!(gen.fresh_stanza_id(), "test-3");
    }
}
