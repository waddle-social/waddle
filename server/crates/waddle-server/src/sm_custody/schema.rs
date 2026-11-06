use super::CustodyError;
use crate::db::physical_attestation::PhysicalAttestationConfig;

/// Unregistered PostgreSQL 17 candidate. Install only in a disposable schema.
/// No grants, migration registration, or production selector are provided.
pub fn candidate_postgres_ddl(config: &PhysicalAttestationConfig) -> Result<String, CustodyError> {
    let postgres = config
        .expected_lineage()
        .postgres_identity
        .as_ref()
        .ok_or(CustodyError::Rejected)?;
    Ok(include_str!("schema.sql").replace(
        "__SCHEMA__",
        &format!("\"{}\"", postgres.schema.name.0.replace('"', "\"\"")),
    ))
}
