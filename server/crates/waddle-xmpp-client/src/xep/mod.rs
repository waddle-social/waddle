//! Typed XEP modules.
//!
//! Each submodule owns one XEP: namespace constants, typed value structs,
//! parse helpers, and (when needed) element builders. No string literals for
//! namespaces or attribute names at call sites — everything flows through the
//! typed module.

pub mod reply;
pub mod thread;
