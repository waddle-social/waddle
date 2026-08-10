use std::{fmt, str::FromStr};

use uuid::Uuid;

use super::LineageError;

/// The UUID minted for a durable database boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LineageUuid(pub Uuid);

impl LineageUuid {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl fmt::Display for LineageUuid {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for LineageUuid {
    type Err = LineageError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(value)
            .map(Self)
            .map_err(|_| LineageError::InvalidUuid {
                field: "lineage_uuid",
                value: value.to_string(),
            })
    }
}

/// The operator-provided UUID identifying one Waddle deployment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeploymentUuid(pub Uuid);

impl fmt::Display for DeploymentUuid {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for DeploymentUuid {
    type Err = LineageError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(value)
            .map(Self)
            .map_err(|_| LineageError::InvalidUuid {
                field: "deployment_uuid",
                value: value.to_string(),
            })
    }
}

/// PostgreSQL's stable cluster system identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PgSystemIdentifier(pub u64);

impl fmt::Display for PgSystemIdentifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for PgSystemIdentifier {
    type Err = LineageError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value
            .parse()
            .map(Self)
            .map_err(|_| LineageError::InvalidPostgresIdentity {
                field: "pg_system_identifier",
                value: value.to_string(),
            })
    }
}

/// PostgreSQL database OID and name, captured as one indivisible identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PgDatabaseIdentity {
    pub oid: u32,
    pub name: String,
}

/// PostgreSQL schema OID and name, captured as one indivisible identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PgSchemaIdentity {
    pub oid: u32,
    pub name: String,
}

/// The complete PostgreSQL identity of a durable schema boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PgIdentity {
    pub system_identifier: PgSystemIdentifier,
    pub database: PgDatabaseIdentity,
    pub schema: PgSchemaIdentity,
}

/// The persisted `_lineage` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineageRecord {
    pub format: i64,
    pub lineage_uuid: LineageUuid,
    pub deployment_uuid: DeploymentUuid,
    pub postgres_identity: Option<PgIdentity>,
}

/// Explicit operator action requested through `WADDLE_DB_LINEAGE_ACTION`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineageAction {
    Enroll,
    Adopt(LineageUuid),
}
