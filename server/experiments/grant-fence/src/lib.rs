//! Disposable grant-fence experiment. No production Waddle API is implemented.

mod authority;
mod binaries;
mod cleanup;
mod fixture;
mod model;
mod postgres_diagnostics;
mod process;
mod proof;
mod provenance;
mod recovery;
mod shutdown_measurement;
mod status_capture;
mod worker;

pub use proof::run;
pub use worker::run_worker;
