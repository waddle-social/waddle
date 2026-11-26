//! Unpublished Linux-only native PostgreSQL/SpiceDB test support.
//! No production schema, protocol authority or worker is implemented here.
#[cfg(not(target_os = "linux"))]
compile_error!("native fixture requires Linux process ownership and /proc");
mod binaries;
pub mod cleanup;
pub mod client;
pub mod fixture;
pub mod git_probe;
pub mod model;
pub mod postgres_diagnostics;
pub mod process;
pub mod shutdown_measurement;
pub mod status_capture;
#[cfg(test)]
const TEST_SCHEMA: &str = "definition fixture_probe {}";
