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
//! including its revision advance and immutable result receipt. A newly created
//! generation trusts its supplied acknowledged floor but treats its outstanding
//! frames as unexposed. Exposure records an ordered write-attempt fence, not
//! actual transport delivery or a claim to the socket. Tombstones remain
//! unsupported until a durable terminal fences later generations and appends.
//! A missing receipt after an uncertain commit is never evidence of rollback.
//! A positive exposure receipt is historical and cannot authorize another
//! attempt; a socket owner and its current claim must be fenced separately.
//! Storage errors do not classify whether COMMIT succeeded; selection also
//! needs a typed unknown-commit and original-backend reconciliation protocol.
//! Colliding wrapping ACK windows fail closed until the earlier window is
//! settled. ACK-window confirmation is not supported by this candidate.

mod digest;
mod frontier;
mod implementation;
mod schema;
mod types;

pub use frontier::ExposureFrontier;
pub use implementation::RevisionedSmStorage;
pub use schema::bootstrap_revisioned_sm_owner_schema;
pub use types::{
    ApplyResult, Capture, Effect, Mutation, Readback, Request, Revision, RevisionError,
};
