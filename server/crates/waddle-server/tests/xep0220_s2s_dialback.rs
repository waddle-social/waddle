//! Server Dialback (XEP-0220) with XEP-0185 keys under ADR-0020 D2 (gate
//! G5): the key vectors of both specifications; the codec's Receiving,
//! Authoritative and Initiating Server roles and verification connections,
//! including one domain pair per stream and the refusal of unsolicited and
//! uncorrelated `db:result` and `db:verify`; the dialback configuration;
//! the router's answer to the senders of stanzas queued behind a refused
//! key, one stream carrying several verified domain pairs, and two in-process deployments on loopback, with hand-written peers,
//! authenticating streams by dialback in both directions.
//!
//! Every certificate comes from the shared test CA and the clock is fixed.
//! No PostgreSQL, SpiceDB, Prosody, ejabberd or second process is involved.

// The routing support module and, below it, the connection-manager support
// module are shared by several suites; this one uses only part of them.
#[allow(dead_code, unused_imports)]
#[path = "s2s_routing_support/mod.rs"]
mod routing_support;

use routing_support::transport as support;

#[path = "xep0220_s2s_dialback/keys.rs"]
mod keys;

#[path = "xep0220_s2s_dialback/codec.rs"]
mod codec;

#[path = "xep0220_s2s_dialback/config.rs"]
mod config;

#[path = "xep0220_s2s_dialback/loopback.rs"]
mod loopback;
