//! Disposable grant-fence experiment. No production Waddle API is implemented.

mod authority;
mod model;
mod multi_edge;
mod proof;
mod provenance;
mod recovery;
mod worker;

pub use proof::run;
use waddle_native_fixture::{cleanup, fixture, process};
pub use worker::run_worker;
