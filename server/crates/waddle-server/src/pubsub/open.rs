use std::path::Path;
use std::sync::Arc;

use tracing::info;
use waddle_xmpp::pubsub::PubSubStorage;
use waddle_xmpp::XmppError;

use super::DatabasePubSubStorage;
use crate::db::lineage::PgIdentity;
use crate::db::{Database, DatabaseConfig, DatabaseDriver};

impl DatabasePubSubStorage {
    /// Run the existing legacy owner initializer on the supplied shared main
    /// handle, without opening a child pool or claiming restricted authority.
    pub async fn initialize_shared_owner(db: Database) -> Result<Self, XmppError> {
        if db.driver() != DatabaseDriver::Postgres
            || db.uses_attested_postgres_pool()
            || !db.is_main_lane()
            || db.ingress_pool_size().is_none()
        {
            return Err(XmppError::internal(
                "shared PubSub owner initialization requires an owner main lane".to_owned(),
            ));
        }
        let storage = Self { db };
        storage.initialize().await?;
        Ok(storage)
    }

    pub async fn open(database_url: Option<&str>) -> Result<Self, XmppError> {
        let db = match database_url {
            Some(database_url) => open_database(database_url).await?,
            None => Database::in_memory("pubsub")
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?,
        };
        let storage = Self { db };
        storage.initialize().await?;
        info!(driver = ?storage.db.driver(), "PubSub storage initialized");
        Ok(storage)
    }

    /// Prepare the PubSub schema on this exact owner database handle.
    ///
    /// The caller must fence old writers and other privileged schema or role
    /// changes. PostgreSQL and the exact pre-enrolled physical identity are
    /// required; the current role must own the schema and not be a login role.
    /// An attested runtime handle is never accepted for owner DDL.
    /// The caller supplies the trusted identity and verifies logical lineage;
    /// the owner role needs EXECUTE on `pg_catalog.pg_control_system()`.
    /// A populated current store must have every expected table and is left
    /// unchanged; restricted attachment separately checks its full shape.
    /// Incompatible or unversioned stores are reset only when all managed
    /// data and push watermarks are empty.
    pub async fn initialize_owner(db: Database, identity: &PgIdentity) -> Result<(), XmppError> {
        if db.driver() != DatabaseDriver::Postgres || db.uses_attested_postgres_pool() {
            return Err(XmppError::internal(
                "PubSub owner initialization requires a PostgreSQL owner database handle"
                    .to_string(),
            ));
        }
        let storage = Self { db };
        storage.initialize_guarded_owner(identity).await?;
        info!(driver = ?storage.db.driver(), "PubSub storage initialized");
        Ok(())
    }

    /// Attach the exact attested PostgreSQL main-lane database after a
    /// bounded read-only verification. This never initializes or repairs it.
    /// A trusted bootstrap must grant EXECUTE on the owner-created attach-lock
    /// function only to the attested runtime role; PgIdentity does not carry a
    /// runtime role name, so owner preparation deliberately does not infer it.
    /// The application is trusted to issue its normal DML: table grants alone
    /// cannot prevent arbitrary SQL from choosing identity values or changing
    /// projection counters. Old writers and privileged DDL must remain fenced.
    pub async fn attach_existing(db: Database) -> Result<Self, super::PubSubRuntimeAttachError> {
        super::runtime_attach::verify(&db).await?;
        Ok(Self { db })
    }

    pub fn database(&self) -> Database {
        self.db.clone()
    }
}

pub async fn build_pubsub_storage(
    database_url: Option<String>,
) -> Result<Arc<dyn PubSubStorage>, XmppError> {
    Ok(build_database_pubsub_storage(database_url).await?)
}

pub async fn build_database_pubsub_storage(
    database_url: Option<String>,
) -> Result<Arc<DatabasePubSubStorage>, XmppError> {
    if let Some(url) = database_url {
        return Ok(Arc::new(DatabasePubSubStorage::open(Some(&url)).await?));
    }
    if std::env::var("WADDLE_PUBSUB_INMEMORY").is_ok_and(|v| v == "1") {
        return Ok(Arc::new(DatabasePubSubStorage::open(None).await?));
    }
    Err(XmppError::config(
        "WADDLE_XMPP_PUBSUB_DATABASE_URL is required for production durability; \
         set WADDLE_PUBSUB_INMEMORY=1 to opt into ephemeral storage for dev/test"
            .to_string(),
    ))
}

async fn open_database(database_url: &str) -> Result<Database, XmppError> {
    ensure_sqlite_parent_dir(database_url)?;
    let driver = infer_database_driver(database_url)?;
    Database::from_config(
        "pubsub",
        &DatabaseConfig::new(driver, database_url.to_string()),
    )
    .await
    .map_err(|error| XmppError::internal(error.to_string()))
}

fn infer_database_driver(database_url: &str) -> Result<DatabaseDriver, XmppError> {
    let lower = database_url.to_ascii_lowercase();
    if lower.starts_with("postgres://") || lower.starts_with("postgresql://") {
        return Ok(DatabaseDriver::Postgres);
    }
    if lower.starts_with("sqlite:") {
        return Ok(DatabaseDriver::Sqlite);
    }

    Err(XmppError::config(format!(
        "unsupported PubSub database URL '{database_url}': expected sqlite: or postgres://"
    )))
}

fn ensure_sqlite_parent_dir(database_url: &str) -> Result<(), XmppError> {
    let Some(path) = sqlite_database_path(database_url) else {
        return Ok(());
    };

    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(|error| XmppError::internal(error.to_string()))?;
    }

    Ok(())
}

fn sqlite_database_path(database_url: &str) -> Option<&Path> {
    let path = database_url
        .strip_prefix("sqlite://")
        .or_else(|| database_url.strip_prefix("sqlite:"))?;
    if path.is_empty() || path.starts_with(":memory:") || path.starts_with("file:") {
        return None;
    }
    Some(Path::new(path))
}
