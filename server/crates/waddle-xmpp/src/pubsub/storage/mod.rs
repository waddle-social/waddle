//! PubSub storage trait and types.
//!
//! Defines the storage interface for PubSub nodes and items.

mod memory;
mod traits;
mod types;

pub use memory::InMemoryPubSubStorage;
pub use traits::PubSubStorage;
pub use types::{PubSubNode, PublishResult, StoredItem};

#[cfg(test)]
mod tests;
