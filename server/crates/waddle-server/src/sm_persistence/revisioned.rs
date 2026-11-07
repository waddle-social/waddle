//! Unselected revision participation for portable SM persistence.
//!
//! This API serializes only operations performed through it. Legacy writers,
//! actor-local outbound frames, claims, recipient routes, pending delivery and
//! garbage collection do not participate. It is not a serving or authorization
//! fence, and must not be selected until those boundaries are composed and old
//! writers are excluded. In particular, a revision read from the database must
//! never be paired with an older process-local snapshot.
//!
//! The owner explicitly installs the schema. Each operation owns its transaction,
//! including its revision advance and immutable result receipt. A missing receipt
//! after an uncertain commit is never evidence that the commit rolled back.

mod digest;
mod implementation;
mod schema;
mod types;

pub use implementation::RevisionedSmStorage;
pub use schema::bootstrap_revisioned_sm_owner_schema;
pub use types::{
    ApplyResult, Capture, Effect, Mutation, Readback, Request, Revision, RevisionError,
};
