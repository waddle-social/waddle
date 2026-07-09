//! Native, privacy-minimized XEP-0030 Gate 0 evidence collection.

mod args;
mod collect;
mod contract;
mod model;
mod output;

pub use args::{run_capability_collector, CapabilityCollectorArgs};
pub use model::{CapabilityEvidenceError, CapabilityTarget};

#[cfg(test)]
mod tests;
