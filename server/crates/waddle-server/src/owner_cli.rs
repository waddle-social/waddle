//! Operator subcommands of the `waddle-server` binary.
//!
//! A server that serves under the restricted PostgreSQL runtime role holds
//! no DDL privilege, so its database is prepared by separate, short-lived
//! invocations of the same binary, each under its own login:
//!
//! - `owner-phase` runs the migrations, every store's owner preparation and
//!   the runtime grants as the migrator login acting as the schema owner;
//! - `install-attestor` installs the physical attestor as the installer
//!   login;
//! - `emit-handoff` composes, or verifies, the handoff document the server
//!   is started with;
//! - `host-grant` records an operator's act on the host-grant authority:
//!   after a restore, reopening reconstructed scopes, acknowledging the
//!   PostgreSQL position or revoking a grant the restore lost; and at any
//!   time, listing an account's grants and revoking them through the
//!   revocation fence;
//! - `account retire` retires an account, so that its JID is never issued
//!   again.
//!
//! None of them adopts listeners, binds a socket, starts a telemetry
//! exporter or reads serving configuration. Secrets (the database URL, the
//! session key, the keypair pool) are taken from the environment only and
//! are never printed. Results go to standard output and diagnostics to
//! standard error.

mod account;
mod args;
mod connection;
mod emit_handoff;
mod error;
mod host_grant;
mod install_attestor;
mod output;
mod owner_phase;

use clap::Parser;

use crate::db::select_schema_baseline_from_env;
use args::{Cli, Command};
pub use error::{ExitStatus, OwnerCliError};

/// Parse the process arguments, run the named subcommand to completion and
/// return the process exit code. Usage errors and `--help` are printed by
/// the parser.
pub fn run_from_args() -> i32 {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            // Printing fails only when the standard streams are closed.
            let _ = error.print();
            return error.exit_code();
        }
    };
    // What migrations do with a database the retired migration history
    // built is chosen once, before any step connects, so every step of
    // every subcommand agrees.
    match select_schema_baseline_from_env() {
        Ok(()) => {}
        Err(message) => {
            eprintln!("error: {message}");
            return ExitStatus::Usage.code();
        }
    }
    // Database connections that verify a server certificate need a process
    // default; a second installation attempt is refused and harmless.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(8 * 1024 * 1024)
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("error: could not build the async runtime: {error}");
            return ExitStatus::Failed.code();
        }
    };
    let outcome = runtime.block_on(async {
        match cli.command {
            Command::OwnerPhase(args) => owner_phase::run(args).await,
            Command::InstallAttestor(args) => install_attestor::run(args).await,
            Command::EmitHandoff(args) => emit_handoff::run(args).await,
            Command::HostGrant(args) => host_grant::run(args).await,
            Command::Account(args) => account::run(args).await,
        }
    });
    match outcome {
        Ok(()) => ExitStatus::Success.code(),
        Err(error) => {
            error.report();
            error.exit_status().code()
        }
    }
}
