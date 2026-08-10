/// Physical PostgreSQL database boundary identity used for clustered
/// co-location checks across independently-opened storage pools.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostgresBoundaryIdentity {
    pub system_identifier: u64,
    pub database: PostgresDatabaseIdentity,
    pub schema: PostgresSchemaIdentity,
}

/// A PostgreSQL database name, distinct from arbitrary strings at every
/// boundary that carries identity (typed-payloads rule).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PostgresDatabaseName(pub String);

impl std::fmt::Display for PostgresDatabaseName {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A PostgreSQL schema (namespace) name, distinct from arbitrary strings.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PostgresSchemaName(pub String);

impl std::fmt::Display for PostgresSchemaName {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostgresDatabaseIdentity {
    pub oid: u32,
    pub name: PostgresDatabaseName,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostgresSchemaIdentity {
    pub oid: u32,
    pub name: PostgresSchemaName,
}

/// The two typed PostgreSQL boundaries compared by a clustered storage
/// co-location check. Boxed by error variants so ordinary storage errors stay
/// small on hot decode and persistence paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterColocationIdentities {
    pub store: PostgresBoundaryIdentity,
    pub global: PostgresBoundaryIdentity,
}
