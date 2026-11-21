//! Server Dialback (XEP-0220) with XEP-0185 keys under ADR-0020 D2 (gate
//! G5): the key vectors of both specifications; the codec's Receiving,
//! Authoritative and Initiating Server roles and verification connections,
//! including one domain pair per stream and the refusal of unsolicited and
//! uncorrelated `db:result` and `db:verify`; the dialback configuration;
//! and two in-process deployments on loopback, with hand-written peers,
//! authenticating streams by dialback in both directions.
//!
//! Every certificate comes from the shared test CA and the clock is fixed.
//! No PostgreSQL, SpiceDB, Prosody, ejabberd or second process is involved.

// The connection-manager support module is shared by several suites; this
// one uses only part of it.
#[allow(dead_code)]
#[path = "s2s_connection_manager/support.rs"]
mod support;

#[path = "xep0220_s2s_dialback/keys.rs"]
mod keys;

#[path = "xep0220_s2s_dialback/codec.rs"]
mod codec;

#[path = "xep0220_s2s_dialback/config.rs"]
mod config;

#[path = "xep0220_s2s_dialback/loopback.rs"]
mod loopback;
