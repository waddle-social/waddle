//! Immutable source identity compiled into the shipped server binary.
//!
//! The implementation lives in `waddle-xmpp-core` so diagnostics, telemetry,
//! metrics, and XEP-0092 all expose the same non-spoofable value.

pub use waddle_xmpp_core::build_identity::{
    embedded_git_sha, printable_git_sha, UNKNOWN_BUILD_COMMIT,
};
