//! Entity ownership claims (ADR-0017 Phase 3 Slice 1, element 4).
//!
//! Every claimed entity — a `UserActor`, a `RoomActor`, or a XEP-0198 SM
//! session — needs a real, epoch-fenced owner record so a cluster of nodes
//! can agree on exactly one live owner at a time. This module defines that
//! abstraction (the [`ClaimStore`] trait) plus the typed value types every
//! implementation and caller shares.
//!
//! **Unconditionally compiled — no `clustering` Cargo feature gate.**
//! `UserActor`/`RoomActor`/SM session code in this crate has no `clustering`
//! feature and must keep working (and keep needing *some* `ClaimStore`) in
//! every build, including a plain single-node SQLite deployment that never
//! touches Postgres. Gating the trait itself behind `clustering` would force
//! ordinary session/registry code to grow two code paths for "clustering
//! compiled" vs. not — exactly the kind of scattered conditional the ADR's
//! element-1 text forbids.
//!
//! - [`InProcessClaimStore`] (in [`in_process`]) is the trivial single-node
//!   implementation: there is only one node, so the contract is identical
//!   to the Postgres impl's — acquire succeeds iff the entity is
//!   currently unclaimed — but same-node contention (two connections
//!   racing to claim the same entity) is real and enforced, not a
//!   no-op.
//! - The Postgres CAS implementation lives downstream, in
//!   `waddle-server::clustering::claims::PostgresClaimStore`
//!   (`#[cfg(feature = "clustering")]`), implementing this crate's
//!   [`ClaimStore`] trait for a `waddle-server`-local type — legal under
//!   Rust's orphan rule (local type, foreign trait) without the trait
//!   itself needing to live downstream.
//! - [`resume`] defines [`ResumeIdentityProof`] and hosts its sole
//!   constructor — see that type's docs for why the consent-CAS steal
//!   path is compiler-enforced, not merely conventional. The type is
//!   deliberately declared inside `resume.rs`, not here: Rust's privacy
//!   rule extends a private field's visibility to the defining module's
//!   *descendants*, never to its ancestors or siblings, so defining it in
//!   `mod.rs` itself would let every sibling submodule (e.g.
//!   [`in_process`]) construct one directly — exactly the loophole moving
//!   the type down into `resume` closes.

mod connection_identity;
pub mod in_process;
mod observe;
pub mod resume;

pub use connection_identity::{ConnectionRef, NodeLeaseIdentity, PhysicalTransportGeneration};
pub use in_process::InProcessClaimStore;
pub use observe::{observed_claim_store, ObservedClaimStore};
pub use resume::{verify_resume_identity, ResumeIdentityProof};

use async_trait::async_trait;
use std::time::Duration;

#[cfg(test)]
mod tests;

mod claims;
pub use claims::ClaimEpoch;
pub use claims::ClaimError;
pub use claims::ClaimSnapshot;
pub use claims::ClaimStore;
pub use claims::CurrentNodeIdentityGuard;
pub use claims::Entity;
pub use claims::EntityIdTooLong;
pub use claims::EntityType;
pub use claims::ExactReleaseOutcome;
use claims::NodeAuthority;
pub use claims::NodeIdentity;
use claims::RawEntity;
pub use claims::RolloutBackoff;
pub use claims::SharedNodeIdentity;
pub use claims::StalePredicate;
pub use claims::ENTITY_ID_MAX_LEN;
