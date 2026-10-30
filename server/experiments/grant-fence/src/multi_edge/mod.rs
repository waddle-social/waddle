//! Finite dependency-fence proof; no production policy or canonical recovery.
mod cases;
mod contender;
mod graph;
mod model;
mod store;
mod worker;

pub use cases::run;
pub use model::{Case, Work};
pub use worker::execute;
