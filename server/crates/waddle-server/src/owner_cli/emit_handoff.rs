//! `emit-handoff`: compose or verify the restricted server's handoff.

use std::time::Duration;

use uuid::Uuid;

use crate::config::LineageConfig;
use crate::db::bootstrap::RuntimePoolLimits;
use crate::db::runtime_startup::{
    compose_handoff, verify_handoff, ComposedHandoff, HandoffInputs, OperatorRecord,
};

use super::args::EmitHandoffArgs;
use super::connection::{open_owner, secret_env};
use super::error::OwnerCliError;

const RUNTIME_DATABASE_URL: &str = "WADDLE_RUNTIME_DATABASE_URL";

pub(super) async fn run(args: EmitHandoffArgs) -> Result<(), OwnerCliError> {
    let deployment_uuid = LineageConfig::from_env()
        .map_err(|error| OwnerCliError::Configuration(error.to_string()))?
        .deployment_uuid
        .ok_or_else(|| {
            OwnerCliError::Configuration("WADDLE_DEPLOYMENT_UUID is required".to_owned())
        })?;
    let runtime_url = secret_env(RUNTIME_DATABASE_URL)?;
    let read = |path: &std::path::PathBuf| {
        std::fs::read_to_string(path).map_err(|source| OwnerCliError::Read {
            path: path.clone(),
            source,
        })
    };
    let inputs = HandoffInputs {
        release_git_sha: &args.release_git_sha,
        deployment_uuid,
        installer_login: &args.attestor.attestor_installer_login,
        runtime_login: &args.roles.runtime_login,
        definer_role: &args.roles.attestor_definer_role,
        trusted_schema: &args.attestor.attestor_schema,
    };
    let application_name = format!("waddle-handoff-{}", Uuid::new_v4().simple());

    let handoff = match (&args.verify, args.installation_operation_id, &args.cutover) {
        (Some(existing), _, _) => {
            let existing = read(existing)?;
            let owner = open_owner(&args.connection, &application_name).await?;
            verify_handoff(&owner, &inputs, &existing)
                .await
                .map_err(OwnerCliError::Handoff)?
        }
        (None, Some(installation_operation_id), Some(cutover)) => {
            let cutover = read(cutover)?;
            let owner = open_owner(&args.connection, &application_name).await?;
            compose_handoff(
                &owner,
                &inputs,
                &OperatorRecord {
                    installation_operation_id,
                    cutover: &cutover,
                },
            )
            .await
            .map_err(OwnerCliError::Handoff)?
        }
        (None, _, _) => {
            return Err(OwnerCliError::Configuration(
                "--installation-operation-id and --cutover are required unless --verify is given"
                    .to_owned(),
            ));
        }
    };
    if let Some(runtime_url) = &runtime_url {
        preflight(&handoff, runtime_url).await?;
    }

    if args.verify.is_some() {
        eprintln!(
            "the handoff matches the database and release {}",
            args.release_git_sha
        );
        return Ok(());
    }
    match &args.output {
        Some(path) => {
            std::fs::write(path, handoff.document()).map_err(|source| OwnerCliError::Write {
                path: path.clone(),
                source,
            })?;
        }
        None => println!("{}", handoff.document()),
    }
    eprintln!("handoff composed for release {}", args.release_git_sha);
    Ok(())
}

/// Run the server's pinned read-only preflight on the runtime connection,
/// so a handoff the server would refuse for its endpoint, transport or
/// pinned catalog is refused here.
async fn preflight(handoff: &ComposedHandoff, runtime_url: &str) -> Result<(), OwnerCliError> {
    let limits = RuntimePoolLimits {
        main_connections: 1,
        ingress_connections: 1,
        acquire_timeout: Duration::from_secs(30),
    };
    handoff
        .identity()
        .preflight(runtime_url, limits)
        .await
        .map(|_| ())
        .map_err(OwnerCliError::Preflight)
}
