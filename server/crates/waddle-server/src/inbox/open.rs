use super::*;

pub async fn build_inbox_storage(
    database_url: Option<String>,
) -> Result<Arc<dyn InboxStorage>, InboxStorageError> {
    Ok(Arc::new(
        DatabaseInboxStorage::open(database_url.as_deref()).await?,
    ))
}

pub(super) async fn open_database(database_url: &str) -> Result<Database, InboxStorageError> {
    ensure_sqlite_parent_dir(database_url)?;
    let driver = infer_database_driver(database_url)?;
    Database::from_config(
        "inbox",
        &DatabaseConfig::new(driver, database_url.to_string()),
    )
    .await
    .map_err(|error| InboxStorageError::Other(error.to_string()))
}

fn infer_database_driver(database_url: &str) -> Result<DatabaseDriver, InboxStorageError> {
    let lower = database_url.to_ascii_lowercase();
    if lower.starts_with("postgres://") || lower.starts_with("postgresql://") {
        return Ok(DatabaseDriver::Postgres);
    }
    if lower.starts_with("sqlite:") {
        return Ok(DatabaseDriver::Sqlite);
    }

    Err(InboxStorageError::Other(format!(
        "unsupported inbox database URL '{database_url}': expected sqlite: or postgres://"
    )))
}

fn ensure_sqlite_parent_dir(database_url: &str) -> Result<(), InboxStorageError> {
    let Some(path) = sqlite_database_path(database_url) else {
        return Ok(());
    };

    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| InboxStorageError::Other(error.to_string()))?;
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
