//! PubSub storage trait and types.
//!
//! Defines the storage interface for PubSub nodes and items.

mod memory;
mod traits;
mod types;

pub use memory::InMemoryPubSubStorage;
pub use traits::PubSubStorage;
pub use types::{
    PubSubNode, PublicationError, PublicationFingerprint, PublicationNode, PublicationVersion,
    PublishResult, StoredItem, VersionedPublishResult,
};

#[cfg(test)]
mod tests;
