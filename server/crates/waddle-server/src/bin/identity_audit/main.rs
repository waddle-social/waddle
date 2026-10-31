//! Offline, read-only account preparation audit. Never starts server services.
mod analysis;
mod cli;
mod model;
mod profile;
mod report;
mod source;

use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    cli::run().await
}
