//! Shared native/WASM timeline adapter for the independent Flutter application.
//!
//! DTOs exist only at the application binding boundary. The store validates
//! archive authorities and converts them to the shared sans-IO timeline's typed
//! inputs before retaining message state.

mod adapter;
mod dto;
mod store;

pub use dto::{TimelineCommand, TimelineSnapshot};
pub use store::{TimelineError, TimelineStore};

#[cfg(test)]
mod tests;
