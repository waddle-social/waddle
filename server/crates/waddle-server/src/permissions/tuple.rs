//! Permission tuple types and storage.
//!
//! Tuples are the fundamental unit of the permission system.
//! Format: `<object>#<relation>@<subject>`

mod store;
#[cfg(test)]
mod tests;
mod types;

pub use store::TupleStore;
pub use types::{Object, ObjectType, Relation, Subject, SubjectType, Tuple};
