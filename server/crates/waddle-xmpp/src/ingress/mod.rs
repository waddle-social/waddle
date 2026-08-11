//! Typed ingress identity domain's dark substrate; see issues #1650 and #1635
//! for the broader identity-domain design.

mod alias;
pub mod digest;
mod epoch;
mod error;
mod generation;
mod keys;
mod ordinal;
mod stream;
mod target;

pub use alias::{resolve_alias, AliasConflict, AliasOutcome, AliasResolution, StoredAlias};
pub use digest::{DigestVersion, SemanticDigest};
pub use epoch::ProtocolEpoch;
pub use error::IngressTypeError;
pub use generation::{ConnectionGeneration, EntityGeneration, RowRevision};
pub use keys::{DeliveryKey, MessageKey};
pub use ordinal::IngressOrdinal;
pub use stream::{IngressStreamId, SmIngressId};
pub use target::NormalizedTarget;
