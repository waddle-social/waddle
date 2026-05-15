use std::path::Path;
use std::sync::Arc;

use tracing::info;
use waddle_xmpp::pubsub::PubSubStorage;
use waddle_xmpp::XmppError;

use super::DatabasePubSubStorage;
use crate::db::{Database, DatabaseConfig, DatabaseDriver};

impl DatabasePubSubStorage {
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
