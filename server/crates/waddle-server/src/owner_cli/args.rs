//! Command-line surface of the operator subcommands.
//!
//! Role, schema and plan inputs are flags with an environment fallback, so
//! one environment block can configure a job's steps and the server. The
//! database URL, the session key and the keypair pool are secrets and have
//! no flag.

use std::path::PathBuf;
#[cfg(feature = "clustering")]
use std::time::Duration;

#[cfg(feature = "clustering")]
use clap::ValueEnum;
use clap::{Args, Parser, Subcommand};
use thiserror::Error;
use uuid::Uuid;

use crate::db::lineage::PgSchemaName;
use crate::db::physical_attestation::{validate_identifier, PgRoleName, PhysicalAttestationError};
use crate::db::runtime_startup::valid_git_sha;

const ABOUT: &str = "Waddle XMPP server";

const LONG_ABOUT: &str = "\
Waddle XMPP server.

Without a subcommand the server starts and takes its whole configuration from
WADDLE_* environment variables.

The subcommands prepare a PostgreSQL database for a server that serves under a
restricted runtime role (WADDLE_DB_RUNTIME_ATTESTATION). Each connects with
the login named in WADDLE_DATABASE_URL, runs to completion and exits. They do
not start the server, bind a socket or export telemetry.

Connection URLs and other secrets are taken from the environment, never as
arguments, and are never printed. PGOPTIONS and PGAPPNAME must be unset.

Order for one database:
  1. owner-phase       as the migrator login
  2. install-attestor  as the attestor installer login
  3. emit-handoff      as the migrator login

Exit codes:
  0  completed
  1  the connection or the database returned an error, a lock stayed contended
     or timed out, or an outcome is unknown; a rerun may succeed
  2  invalid arguments or environment; nothing was sent to the database
  3  owner-phase only: prepared, keypair-pool generation pending; run it again
     after the printed activation time
  4  refused: a verifier rejected the database, the plan or the document; a
     rerun refuses again until one of them changes";

const OWNER_PHASE_ENVIRONMENT: &str = "\
Environment (no flag; secret values are never printed):
  WADDLE_DATABASE_URL             PostgreSQL URL of the migrator login. It must
                                  not carry `options` or `application_name`:
                                  the subcommand sets the role, the search
                                  path and a session name unique to the run.
  WADDLE_DEPLOYMENT_UUID          Deployment identity the lineage row names.
  WADDLE_DB_LINEAGE_ACTION        `enroll` or `adopt=<lineage-uuid>[,...]` for
                                  a database without a lineage row for this
                                  deployment; unset otherwise.
  WADDLE_SESSION_KEY              The server's session key. The VAPID signing
                                  key is sealed under it.
  WADDLE_DB_BASELINE_ACTION       Unset or `stamp`: an empty database is built
                                  from the schema baseline, and a database
                                  that holds the complete retired migration
                                  history has its ledger rewritten to the
                                  baseline, without DDL. `apply`: the same,
                                  except that a retired ledger is refused
                                  instead of stamped.
  WADDLE_CLUSTERING_ENABLED       `true` prepares the clustering stores and
                                  requires a server built with clustering.
  WADDLE_CLUSTERING_KEYPAIR_POOL  The server's keypair pool. Its PeerIds, in
                                  pool order, become the enrollment and the
                                  peer allowlist.
  WADDLE_CLUSTERING_LEASE_TTL_MS  Keypair-slot lease lifetime the servers run
                                  with (default 30000).

On success the database identity (lineage and physical identity) is printed
to standard output as JSON; install-attestor takes it as --identity.

Without clustering the phase does not check that it has the database to
itself: stop every server before it runs. --identity-output replaces an
existing file atomically and refuses a symbolic link.";

const INSTALL_ATTESTOR_ENVIRONMENT: &str = "\
Environment (no flag; secret values are never printed):
  WADDLE_DATABASE_URL  PostgreSQL URL of the attestor installer login.

The installation is safe to repeat with the same --operation-id. If the exit
code is 1 the outcome may be unknown: run it again with the same
--operation-id before anything else. On success the operation ID is printed
to standard output; emit-handoff takes it as --installation-operation-id.";

const EMIT_HANDOFF_ENVIRONMENT: &str = "\
Environment (no flag; secret values are never printed):
  WADDLE_DATABASE_URL          PostgreSQL URL of the migrator login, as for
                               owner-phase.
  WADDLE_DEPLOYMENT_UUID       Deployment identity; the server must be started
                               with the same value.
  WADDLE_RUNTIME_DATABASE_URL  Optional. The runtime login's URL exactly as the
                               server will use it. When set, the composed
                               handoff must also pass the server's pinned
                               read-only preflight on that connection.

The document is the server's WADDLE_DB_RUNTIME_ATTESTATION. Its database pins
are read on this connection and stay valid until a pinned object is
recreated. The deployment UUID, the role names, the installation operation ID
and the cutover record are the same for every release. Only `release_git_sha`
changes per release, so a deployment composes the handoff for a new image by
replacing that field; --verify checks a stored document against the database
and the release it is about to start.";

#[derive(Debug, Parser)]
#[command(name = "waddle-server", version, about = ABOUT, long_about = LONG_ABOUT)]
pub(super) struct Cli {
    #[command(subcommand)]
    pub(super) command: Command,
}

#[derive(Debug, Subcommand)]
pub(super) enum Command {
    /// Run migrations, every store's owner preparation and the runtime grants
    #[command(after_help = OWNER_PHASE_ENVIRONMENT)]
    OwnerPhase(OwnerPhaseArgs),
    /// Install the physical attestor function under the installer login
    #[command(after_help = INSTALL_ATTESTOR_ENVIRONMENT)]
    InstallAttestor(InstallAttestorArgs),
    /// Compose or verify the handoff document the restricted server starts with
    #[command(after_help = EMIT_HANDOFF_ENVIRONMENT)]
    EmitHandoff(EmitHandoffArgs),
}

/// The owner connection: the migrator login acting as the schema owner.
#[derive(Debug, Args)]
pub(super) struct OwnerConnectionArgs {
    /// Application schema that holds the server's relations
    #[arg(
        long,
        env = "WADDLE_XMPP_MAM_CLAIM_SCHEMA",
        hide_env_values = true,
        value_parser = parse_schema
    )]
    pub(super) schema: PgSchemaName,
    /// Login that runs the owner phase; the only login able to act as the schema owner
    #[arg(
        long,
        env = "WADDLE_DB_MIGRATOR_LOGIN",
        hide_env_values = true,
        value_parser = parse_role
    )]
    pub(super) migrator_login: PgRoleName,
    /// NOLOGIN role that owns the application schema and every relation in it
    #[arg(
        long,
        env = "WADDLE_XMPP_MAM_SCHEMA_OWNER",
        hide_env_values = true,
        value_parser = parse_role
    )]
    pub(super) schema_owner: PgRoleName,
}

/// Roles the owner phase grants to and the attestor is bound to.
#[derive(Debug, Args)]
pub(super) struct RuntimeRoleArgs {
    /// Restricted login the server serves under
    #[arg(
        long,
        env = "WADDLE_DB_RUNTIME_LOGIN",
        hide_env_values = true,
        value_parser = parse_role
    )]
    pub(super) runtime_login: PgRoleName,
    /// NOLOGIN role that owns the attestor schema and the attestor function
    #[arg(
        long,
        env = "WADDLE_DB_ATTESTOR_DEFINER_ROLE",
        hide_env_values = true,
        value_parser = parse_role
    )]
    pub(super) attestor_definer_role: PgRoleName,
}

/// The attestor installation's own login and schema.
#[derive(Debug, Args)]
pub(super) struct AttestorArgs {
    /// Login that installs the attestor; the only login able to act as the definer role
    #[arg(
        long,
        env = "WADDLE_DB_ATTESTOR_INSTALLER_LOGIN",
        hide_env_values = true,
        value_parser = parse_role
    )]
    pub(super) attestor_installer_login: PgRoleName,
    /// Schema, owned by the definer role, that holds the attestor function
    #[arg(
        long,
        env = "WADDLE_DB_ATTESTOR_SCHEMA",
        hide_env_values = true,
        value_parser = parse_schema
    )]
    pub(super) attestor_schema: PgSchemaName,
}

#[derive(Debug, Args)]
pub(super) struct OwnerPhaseArgs {
    #[command(flatten)]
    pub(super) connection: OwnerConnectionArgs,
    #[command(flatten)]
    pub(super) roles: RuntimeRoleArgs,
    /// Also write the database identity JSON to this file
    #[arg(long, value_name = "FILE")]
    pub(super) identity_output: Option<PathBuf>,
    #[cfg(feature = "clustering")]
    #[command(flatten)]
    pub(super) clustering: ClusteringArgs,
}

/// Clustering plan inputs. They apply when `WADDLE_CLUSTERING_ENABLED` is
/// true.
#[cfg(feature = "clustering")]
#[derive(Debug, Args)]
#[command(next_help_heading = "Clustering plan (applies when WADDLE_CLUSTERING_ENABLED is true)")]
pub(super) struct ClusteringArgs {
    /// Generation the keypair-pool enrollment is recorded under; required with
    /// clustering, and the same value on every run for one keypair pool
    #[arg(
        long,
        env = "WADDLE_CLUSTERING_KEYPOOL_GENERATION",
        hide_env_values = true,
        value_name = "UUID"
    )]
    pub(super) keypool_generation: Option<Uuid>,
    /// How long a newly enrolled generation stays pending, e.g. 30s or 500ms
    /// [default: the --legacy-ttl when retiring, otherwise 0s]
    #[arg(long, value_name = "DURATION", value_parser = parse_duration)]
    pub(super) keypool_activation_delay: Option<Duration>,
    /// What to do with a keypair-slot table an unattested server created
    #[arg(long, value_enum, default_value_t = LegacyKeypairSlotsArg::Refuse)]
    pub(super) legacy_keypair_slots: LegacyKeypairSlotsArg,
    /// Lease lifetime (WADDLE_CLUSTERING_LEASE_TTL_MS) the retired servers ran
    /// with; required with --legacy-keypair-slots retire
    #[arg(
        long,
        value_name = "DURATION",
        value_parser = parse_duration,
        required_if_eq("legacy_keypair_slots", "retire")
    )]
    pub(super) legacy_ttl: Option<Duration>,
    /// What to do with enrolled PeerIds the keypair pool does not contain
    #[arg(long, value_enum, default_value_t = AllowlistArg::Refuse)]
    pub(super) allowlist: AllowlistArg,
    /// Whether the phase requires that no other session is connected to the
    /// database and no prepared transaction is pending in it. `not-checked`
    /// disables the only check that no server or operator session is still
    /// connected; never use it during a cutover
    #[arg(long, value_enum, default_value_t = ExclusiveDatabaseArg::Required)]
    pub(super) exclusive_database: ExclusiveDatabaseArg,
}

#[cfg(feature = "clustering")]
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(super) enum LegacyKeypairSlotsArg {
    /// Fail and leave the table untouched
    Refuse,
    /// Drop and recreate the table once no row in it is leased
    Retire,
}

#[cfg(feature = "clustering")]
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(super) enum AllowlistArg {
    /// Fail and change nothing
    Refuse,
    /// Revoke them, making the allowlist exactly the keypair pool
    Reconcile,
}

#[cfg(feature = "clustering")]
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(super) enum ExclusiveDatabaseArg {
    /// Fail while another session or a prepared transaction exists
    Required,
    /// Do not check; a connected server can then write while the phase runs
    NotChecked,
}

#[derive(Debug, Args)]
pub(super) struct InstallAttestorArgs {
    /// Database identity JSON that owner-phase printed
    #[arg(long, value_name = "FILE")]
    pub(super) identity: PathBuf,
    /// Operation ID of this installation; generate one UUID and keep it
    #[arg(long, value_name = "UUID")]
    pub(super) operation_id: Uuid,
    #[command(flatten)]
    pub(super) roles: RuntimeRoleArgs,
    #[command(flatten)]
    pub(super) attestor: AttestorArgs,
}

#[derive(Debug, Args)]
pub(super) struct EmitHandoffArgs {
    #[command(flatten)]
    pub(super) connection: OwnerConnectionArgs,
    #[command(flatten)]
    pub(super) roles: RuntimeRoleArgs,
    #[command(flatten)]
    pub(super) attestor: AttestorArgs,
    /// Git SHA of the release the handoff is for: 40 lowercase hex characters,
    /// equal to the server's WADDLE_GIT_SHA
    #[arg(
        long,
        env = "WADDLE_GIT_SHA",
        hide_env_values = true,
        value_name = "SHA",
        value_parser = parse_git_sha
    )]
    pub(super) release_git_sha: String,
    /// Operation ID the attestor was installed under
    #[arg(
        long,
        value_name = "UUID",
        required_unless_present = "verify",
        conflicts_with = "verify"
    )]
    pub(super) installation_operation_id: Option<Uuid>,
    /// JSON file holding the handoff's `cutover` object: the operator's record
    /// of the transition, the primary endpoint the runtime connects to, the
    /// retired login and the cutoff references
    #[arg(
        long,
        value_name = "FILE",
        required_unless_present = "verify",
        conflicts_with = "verify"
    )]
    pub(super) cutover: Option<PathBuf>,
    /// Write the document to this file instead of standard output; an existing
    /// file is replaced atomically and a symbolic link is refused
    #[arg(long, value_name = "FILE", conflicts_with = "verify")]
    pub(super) output: Option<PathBuf>,
    /// Emit nothing; check that this stored handoff is what the database and
    /// these inputs produce now, taking its cutover record and installation
    /// operation ID as given, and name every field that differs
    #[arg(long, value_name = "FILE")]
    pub(super) verify: Option<PathBuf>,
}

fn parse_role(value: &str) -> Result<PgRoleName, PhysicalAttestationError> {
    PgRoleName::new(value)
}

fn parse_schema(value: &str) -> Result<PgSchemaName, PhysicalAttestationError> {
    validate_identifier(value, "schema")?;
    Ok(PgSchemaName(value.to_owned()))
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(super) enum ArgumentError {
    #[error("expected 40 lowercase hexadecimal characters")]
    GitSha,
    #[cfg(feature = "clustering")]
    #[error("expected a whole number followed by ms, s, m or h, such as 30s")]
    Duration,
}

fn parse_git_sha(value: &str) -> Result<String, ArgumentError> {
    if valid_git_sha(value) {
        Ok(value.to_owned())
    } else {
        Err(ArgumentError::GitSha)
    }
}

#[cfg(feature = "clustering")]
fn parse_duration(value: &str) -> Result<Duration, ArgumentError> {
    let digits = value
        .find(|character: char| !character.is_ascii_digit())
        .ok_or(ArgumentError::Duration)?;
    let (amount, unit) = value.split_at(digits);
    let amount: u64 = amount.parse().map_err(|_| ArgumentError::Duration)?;
    let millis = match unit {
        "ms" => Some(amount),
        "s" => amount.checked_mul(1_000),
        "m" => amount.checked_mul(60_000),
        "h" => amount.checked_mul(3_600_000),
        _ => None,
    };
    millis
        .map(Duration::from_millis)
        .ok_or(ArgumentError::Duration)
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "clustering")]
    use std::time::Duration;

    use clap::CommandFactory;

    #[cfg(feature = "clustering")]
    use super::parse_duration;
    use super::{parse_git_sha, ArgumentError, Cli};

    #[test]
    fn command_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[cfg(feature = "clustering")]
    #[test]
    fn durations_take_a_unit() {
        assert_eq!(parse_duration("500ms"), Ok(Duration::from_millis(500)));
        assert_eq!(parse_duration("30s"), Ok(Duration::from_secs(30)));
        assert_eq!(parse_duration("2m"), Ok(Duration::from_secs(120)));
        assert_eq!(parse_duration("1h"), Ok(Duration::from_secs(3600)));
        for invalid in ["", "30", "s", "1.5s", "-1s", "30 s", "30d"] {
            assert_eq!(parse_duration(invalid), Err(ArgumentError::Duration));
        }
    }

    #[test]
    fn release_identity_is_a_full_lowercase_sha() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(parse_git_sha(sha), Ok(sha.to_owned()));
        assert_eq!(parse_git_sha("0123abc"), Err(ArgumentError::GitSha));
        assert_eq!(
            parse_git_sha(&sha.to_uppercase()),
            Err(ArgumentError::GitSha)
        );
    }
}
