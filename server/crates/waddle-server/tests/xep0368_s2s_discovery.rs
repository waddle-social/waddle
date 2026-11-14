//! S2S endpoint discovery (XEP-0368, RFC 6120 §3.2; ADR-0020 D3, gate
//! G6): merged `_xmpps-server` and `_xmpp-server` SRV ordering, "."
//! targets, A/AAAA fallback only after negative answers, lookup failures
//! that never fall back, lookups of fully qualified names without a search
//! list, refused domain forms, caps, the blocked address classes, the
//! peer-scope and discovery configuration, and outbound XEP-0368 direct TLS
//! with its Server Name Indication and ALPN protocol.
//!
//! DNS is a stub resolver; no lookup leaves the process. Service cases run
//! in-process deployments on real loopback sockets with the shared test CA.
//! No PostgreSQL, SpiceDB or second process is involved.

// The connection-manager support module is shared by several suites; this
// one uses only part of it.
#[allow(dead_code)]
#[path = "s2s_connection_manager/support.rs"]
mod support;

#[path = "xep0368_s2s_discovery/dns.rs"]
mod dns;

#[path = "xep0368_s2s_discovery/addresses.rs"]
mod addresses;

#[path = "xep0368_s2s_discovery/config.rs"]
mod config;

#[path = "xep0368_s2s_discovery/dialling.rs"]
mod dialling;
