//! Inert experiment for typed, injective encoding of identifier representations.
//!
//! This is not a migration or an authorization decision. It must not read or
//! write the legacy permission store: a version prefix cannot isolate unknown
//! legacy grants. See the accompanying README for the store-isolation gate.
//! The JID parser does not establish principal equivalence: distinct accepted
//! representations can refer to the same logical user. Authoritative principal
//! canonicalization is a separate unresolved prerequisite for any integration.

mod codec;
mod opaque;
mod user;

pub use codec::{CodecError, MAX_ID_BYTES, MAX_WIRE_BYTES};
pub use opaque::{EncodedOpaqueId, OpaqueId};
pub use user::{EncodedUserId, ValidatedUserJid};
