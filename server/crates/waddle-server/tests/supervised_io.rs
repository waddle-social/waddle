//! Real loopback transport tests for the test-gated supervised I/O core.

#![cfg(feature = "test-support")]

#[path = "supervised_io/kameo_control.rs"]
mod kameo_control;
#[path = "supervised_io/pumps.rs"]
mod pumps;
#[path = "supervised_io/reserved_start.rs"]
mod reserved_start;
