/// Physical PostgreSQL database boundary identity used for clustered
/// co-location checks across independently-opened storage pools.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostgresBoundaryIdentity {
    pub system_identifier: u64,
    pub database: PostgresDatabaseIdentity,
    pub schema: PostgresSchemaIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostgresDatabaseIdentity {
    pub oid: u32,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostgresSchemaIdentity {
    pub oid: u32,
    pub name: String,
}

/// The two typed PostgreSQL boundaries compared by a clustered storage
/// co-location check. Boxed by error variants so ordinary storage errors stay
/// small on hot decode and persistence paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterColocationIdentities {
    pub store: PostgresBoundaryIdentity,
    pub global: PostgresBoundaryIdentity,
}
