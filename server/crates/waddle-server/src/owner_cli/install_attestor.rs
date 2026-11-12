//! `install-attestor`: install the physical attestor under its own login.

use crate::db::physical_attestation::{
    install, PhysicalAttestationConfig, PhysicalAttestationError,
};
use crate::db::runtime_startup::database_identity_from_json;

use super::args::InstallAttestorArgs;
use super::connection::{connect_options, database_url};
use super::error::OwnerCliError;

pub(super) async fn run(args: InstallAttestorArgs) -> Result<(), OwnerCliError> {
    let identity =
        std::fs::read_to_string(&args.identity).map_err(|source| OwnerCliError::Read {
            path: args.identity.clone(),
            source,
        })?;
    // The expected identity is the one the owner phase attested. The
    // installer compares the live database with it; it is not read back
    // through the installer login.
    let lineage = database_identity_from_json(&identity).map_err(OwnerCliError::Handoff)?;
    let schema = lineage
        .postgres_identity
        .as_ref()
        .map(|postgres| postgres.schema.name.0.clone())
        .ok_or(OwnerCliError::Attestor(
            PhysicalAttestationError::MissingPostgresIdentity,
        ))?;
    let config = PhysicalAttestationConfig::new(
        lineage,
        args.attestor.attestor_installer_login.clone(),
        args.roles.runtime_login.clone(),
        args.roles.attestor_definer_role.clone(),
        args.attestor.attestor_schema.clone(),
    )
    .map_err(OwnerCliError::Attestor)?;
    let options = connect_options(
        "WADDLE_DATABASE_URL",
        &database_url()?,
        &args.attestor.attestor_installer_login,
    )?
    .options([("search_path", schema.as_str())]);
    install(&options, config, args.operation_id)
        .await
        .map_err(OwnerCliError::Attestor)?;
    println!("{}", args.operation_id);
    eprintln!(
        "attestor installed in schema {} by operation {}",
        args.attestor.attestor_schema.0, args.operation_id
    );
    Ok(())
}
