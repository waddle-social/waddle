//! Durable account-resource rows.
//!
//! One row per full JID records the facts of that resource's slot, as fixed
//! by `docs/planning/2138-account-coordinator.md`: the complete
//! `UserClaimIdentity` the slot is held under, the exact `BindingRef` of its
//! current binding with that binding's lifecycle state and install
//! generation, the selection metadata routing reads, and the one-time
//! presentation flags. A second relation records the committed outcome of
//! each operation identity, so a lost reply is resolved without applying the
//! operation twice.
//!
//! A store does not decide transitions itself. Under the row lock it
//! rebuilds the slot from the stored image, applies the operation through
//! `waddle_xmpp::ownership::account_resource_transition`, and writes the new
//! image, so the claim, revision, binding and lifecycle fences are the ones
//! the pure model defines. The ten binding operations of the decision record
//! (prepare, publish, register, update snapshot, detach, resume, begin and
//! complete retirement, replace claim, adopt) are the ten
//! `AccountResourceMutation` variants passed to
//! [`AccountBindingStore::execute`].

mod database;
mod decision;
mod error;
#[cfg(any(test, feature = "test-support"))]
mod memory;
mod row;
mod runtime_attach;
mod schema;
mod store;

pub use database::DatabaseAccountBindingStore;
pub use error::AccountBindingStoreError;
#[cfg(any(test, feature = "test-support"))]
pub use memory::InMemoryAccountBindingStore;
pub use row::StoredResource;
pub use runtime_attach::AccountBindingAttachError;
pub use schema::bootstrap_schema_owner as bootstrap_account_binding_owner_schema;
pub use store::{AccountBindingStore, PresentationMark, StoredExecution};
