//! Waddle community module.
//!
//! Each waddle is a community with its own set of channels, configuration,
//! and database. The `WaddleActor` supervises a single community.

pub mod actor;

pub use actor::{WaddleActor, WaddleConfig};
