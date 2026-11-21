//! Focused suite of the S2S certificate verifier (ADR-0020 D2 and D9): the
//! rows marked W4b in the matrix of
//! `server/docs/planning/s2s-verifier-2047.md`.
//!
//! Every case runs in this process against `waddle_server::s2s::verify`:
//! the startup trust inventory, the per-connection verifier and in-memory
//! rustls handshakes in both TLS roles. The PKI is generated in the test,
//! the clock is injected, and nothing opens a socket, reads a file or
//! reaches the network. The public-WebPKI profile is exercised with a
//! substituted root set.

#[path = "s2s_verifier/pki.rs"]
mod pki;

#[path = "s2s_verifier/harness.rs"]
mod harness;

#[path = "s2s_verifier/signed_path.rs"]
mod signed_path;

#[path = "s2s_verifier/leaf_profile.rs"]
mod leaf_profile;

#[path = "s2s_verifier/tls_profile.rs"]
mod tls_profile;

#[path = "s2s_verifier/trust_profile.rs"]
mod trust_profile;

#[path = "s2s_verifier/identity.rs"]
mod identity;

#[path = "s2s_verifier/constraints.rs"]
mod constraints;

#[path = "s2s_verifier/inventory.rs"]
mod inventory;

#[path = "s2s_verifier/crls.rs"]
mod crls;

#[path = "s2s_verifier/time.rs"]
mod time;

#[path = "s2s_verifier/resources.rs"]
mod resources;

#[path = "s2s_verifier/workers.rs"]
mod workers;
