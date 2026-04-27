//! XMPP Extension Protocol (XEP) value objects shared by the core stack.
//!
//! These modules host typed protocol values that need to be visible to the
//! storage layer (`mam`) and other low-level boundaries — they cannot live in
//! the upper `waddle-xmpp` crate without inducing a dependency cycle.

pub mod xep0359;
