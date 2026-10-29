//! Disposable grant-fence experiment. No production Waddle API is implemented.

mod authority;
mod binaries;
mod fixture;
mod model;
mod process;
mod proof;
mod provenance;
mod worker;

pub use proof::run;
pub use worker::run_worker;
