//! The federation interoperability harness of issue #2047 (plan slice
//! W10): two native Waddle deployments, the pinned Prosody and a two-node
//! ejabberd cluster on loopback, a `.test` CA, recording relays on every
//! S2S path, and a driver that walks the verification phase's flow table
//! and negative matrix, recording PASS, FAIL or UNVERIFIED per row.
//!
//! See `README.md` beside this crate for what each row drives and how to
//! run it; the harness is run only in the verification phase.

pub mod client;
pub mod dns;
pub mod evidence;
pub mod manifest;
pub mod net;
pub mod pki;
pub mod process;
pub mod s2s;
pub mod scenarios;
pub mod services;
pub mod topology;
pub mod watch;
pub mod xml;
