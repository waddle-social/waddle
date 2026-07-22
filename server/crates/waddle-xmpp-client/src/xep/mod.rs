//! Typed XEP modules.
//!
//! Each submodule owns one XEP: namespace constants, typed value structs,
//! parse helpers, and (when needed) element builders. No string literals for
//! namespaces or attribute names at call sites — everything flows through the
//! typed module.

pub mod call_thread;
pub mod encrypted_file;
pub mod fallback;
pub mod reply;
pub mod thread;
pub mod threads;
pub mod xep0045_owner;
pub mod xep0050;
pub mod xep0215;
pub mod xep0292;
pub mod xep0357;
pub mod xep0402;
pub mod xep0492;
