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

impl Default for LineageUuid {
    fn default() -> Self {
        Self::new()
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
        // `pg_control_system().system_identifier` is a signed bigint on the
        // wire (initdb packs seconds-since-epoch into the high word, so from
        // 2038 PostgreSQL renders it as a negative decimal). Accept both
        // renderings and normalize to the u64 bit pattern.
        value
            .parse::<u64>()
            .or_else(|_| value.parse::<i64>().map(|signed| signed as u64))
            .map(Self)
            .map_err(|_| LineageError::InvalidPostgresIdentity {
                field: "pg_system_identifier",
                value: value.to_string(),
            })
    }
}

/// A PostgreSQL database name, typed so identity values cannot be confused
/// with arbitrary strings at storage and error boundaries.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PgDatabaseName(pub String);

impl fmt::Display for PgDatabaseName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A PostgreSQL schema (namespace) name, typed like [`PgDatabaseName`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PgSchemaName(pub String);

impl fmt::Display for PgSchemaName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// PostgreSQL database OID and name, captured as one indivisible identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PgDatabaseIdentity {
    pub oid: u32,
    pub name: PgDatabaseName,
}

/// PostgreSQL schema OID and name, captured as one indivisible identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PgSchemaIdentity {
    pub oid: u32,
    pub name: PgSchemaName,
}

/// The complete PostgreSQL identity of a durable schema boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PgIdentity {
    pub system_identifier: PgSystemIdentifier,
    pub database: PgDatabaseIdentity,
    pub schema: PgSchemaIdentity,
}

impl From<&PgIdentity> for waddle_xmpp::PostgresBoundaryIdentity {
    fn from(identity: &PgIdentity) -> Self {
        Self {
            system_identifier: identity.system_identifier.0,
            database: waddle_xmpp::PostgresDatabaseIdentity {
                oid: identity.database.oid,
                name: waddle_xmpp::PostgresDatabaseName(identity.database.name.0.clone()),
            },
            schema: waddle_xmpp::PostgresSchemaIdentity {
                oid: identity.schema.oid,
                name: waddle_xmpp::PostgresSchemaName(identity.schema.name.0.clone()),
            },
        }
    }
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
///
/// `Adopt` carries a list because one deployment can span several durable
/// boundaries (each with its own lineage UUID); a post-restore adoption
/// names every boundary's expected old UUID in one one-shot action:
/// `adopt=<uuid>[,<uuid>...]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineageAction {
    Enroll,
    Adopt(Vec<LineageUuid>),
}
