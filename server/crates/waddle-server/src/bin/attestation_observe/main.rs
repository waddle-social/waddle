//! One-shot read-only operator observation, never writer activation.

use std::process::ExitCode;
use std::str::FromStr;

use clap::error::ErrorKind;
use clap::Parser;
use serde_json::json;
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use uuid::Uuid;
use waddle_server::db::lineage::{
    AttestedLineage, DeploymentUuid, LineageUuid, PgDatabaseIdentity, PgDatabaseName, PgIdentity,
    PgSchemaIdentity, PgSchemaName, PgSystemIdentifier,
};
use waddle_server::db::physical_attestation::{
    observe_installation, InstallationObservation, PgRoleName, PhysicalAttestationConfig,
};

#[derive(Parser)]
#[command(
    name = "waddle-attestation-observe",
    about = "Read-only, point-in-time installation observation; never writer activation"
)]
struct Args {
    /// Assert that the endpoint is direct and fenced by the operator.
    #[arg(long, required = true)]
    direct_endpoint: bool,
    #[arg(long)]
    operation_id: Uuid,
    #[arg(long)]
    expected_deployment_uuid: Uuid,
    #[arg(long)]
    expected_lineage_uuid: Uuid,
    #[arg(long)]
    expected_system_identifier: PgSystemIdentifier,
    #[arg(long)]
    expected_database_oid: u32,
    #[arg(long)]
    expected_database_name: String,
    #[arg(long)]
    expected_schema_oid: u32,
    #[arg(long)]
    expected_schema_name: String,
    #[arg(long)]
    installer_login: String,
    #[arg(long)]
    runtime_login: String,
    #[arg(long)]
    definer_role: String,
    #[arg(long)]
    trusted_schema: String,
}

#[tokio::main]
async fn main() -> ExitCode {
    let started_at = chrono::Utc::now();
    let args = match Args::try_parse() {
        Ok(args) => args,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            let _ = error.print();
            return ExitCode::SUCCESS;
        }
        Err(_) => {
            return print_result(
                started_at,
                None,
                InstallationObservation::Indeterminate {
                    reason: "configuration",
                },
            )
        }
    };
    let (scope, result) = match config_and_options(&args) {
        Some((config, options)) => {
            let scope = observation_scope(&args, &options);
            (
                Some(scope),
                observe_installation(&options, &config, args.operation_id).await,
            )
        }
        None => (
            None,
            InstallationObservation::Indeterminate {
                reason: "configuration",
            },
        ),
    };
    print_result(started_at, scope, result)
}

fn observation_scope(args: &Args, options: &PgConnectOptions) -> serde_json::Value {
    let tls_mode = match options.get_ssl_mode() {
        PgSslMode::Disable => "disable",
        PgSslMode::Allow => "allow",
        PgSslMode::Prefer => "prefer",
        PgSslMode::Require => "require",
        PgSslMode::VerifyCa => "verify-ca",
        PgSslMode::VerifyFull => "verify-full",
    };
    json!({
        "kind": "one_direct_session_catalog_and_lineage_snapshot",
        "operation_id": args.operation_id,
        "expected": {
            "deployment_uuid": args.expected_deployment_uuid,
            "lineage_uuid": args.expected_lineage_uuid,
            "system_identifier": args.expected_system_identifier.to_string(),
            "database_oid": args.expected_database_oid,
            "database_name": args.expected_database_name,
            "schema_oid": args.expected_schema_oid,
            "schema_name": args.expected_schema_name,
            "installer_login": args.installer_login,
            "runtime_login": args.runtime_login,
            "definer_role": args.definer_role,
            "trusted_schema": args.trusted_schema,
        },
        "endpoint": {
            "host": options.get_host(),
            "port": options.get_port(),
            "database": options.get_database(),
            "tls_mode": tls_mode,
            "direct_and_fenced": "operator_assertion",
        },
    })
}

fn config_and_options(args: &Args) -> Option<(PhysicalAttestationConfig, PgConnectOptions)> {
    if !args.direct_endpoint
        || args.expected_database_oid == 0
        || args.expected_schema_oid == 0
        || args.expected_system_identifier.0 == 0
        || args.expected_database_name.is_empty()
        || args.expected_database_name.len() > 63
        || args.expected_database_name.contains('\0')
    {
        return None;
    }
    let config = PhysicalAttestationConfig::new(
        AttestedLineage {
            lineage_uuid: LineageUuid(args.expected_lineage_uuid),
            deployment_uuid: DeploymentUuid(args.expected_deployment_uuid),
            postgres_identity: Some(PgIdentity {
                system_identifier: args.expected_system_identifier,
                database: PgDatabaseIdentity {
                    oid: args.expected_database_oid,
                    name: PgDatabaseName(args.expected_database_name.clone()),
                },
                schema: PgSchemaIdentity {
                    oid: args.expected_schema_oid,
                    name: PgSchemaName(args.expected_schema_name.clone()),
                },
            }),
        },
        PgRoleName::new(args.installer_login.clone()).ok()?,
        PgRoleName::new(args.runtime_login.clone()).ok()?,
        PgRoleName::new(args.definer_role.clone()).ok()?,
        PgSchemaName(args.trusted_schema.clone()),
    )
    .ok()?;
    let url = std::env::var("WADDLE_ATTESTATION_OBSERVE_DATABASE_URL").ok()?;
    let options = PgConnectOptions::from_str(&url).ok()?;
    Some((config, options))
}

fn print_result(
    started_at: chrono::DateTime<chrono::Utc>,
    scope: Option<serde_json::Value>,
    result: InstallationObservation,
) -> ExitCode {
    let (status, reason, code) = match result {
        InstallationObservation::Observed => ("observed", "matching_catalog_and_identity", 0),
        InstallationObservation::Conflict { reason } => ("conflict", reason, 1),
        InstallationObservation::Indeterminate { reason } => ("indeterminate", reason, 2),
    };
    println!(
        "{}",
        json!({
            "version": 1,
            "kind": "physical_attestation_installation_observation",
            "scope": scope,
            "started_at": started_at.to_rfc3339(),
            "finished_at": chrono::Utc::now().to_rfc3339(),
            "status": status,
            "reason": reason,
            "writer_activation_authority": false,
        })
    );
    ExitCode::from(code)
}
