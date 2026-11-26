//! Re-export of the promoted suspend-aware boot-time clock from `waddle-broker-core`.

pub(super) use waddle_broker_core::boot_clock::{suspend_counted, BootInstant};
