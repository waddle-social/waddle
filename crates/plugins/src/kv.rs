use std::sync::Arc;

use waddle_storage::{Database, Row, SqlValue, StorageError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KvQuota {
    pub max_keys: u64,
    pub max_value_bytes: u64,
}

impl Default for KvQuota {
    fn default() -> Self {
        Self {
            max_keys: 10_000,
            max_value_bytes: 1_048_576,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct KvUsage {
    pub key_count: u64,
    pub total_bytes: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum KvError {
    #[error("value too large: {size} bytes exceeds limit of {limit} bytes")]
    ValueTooLarge { size: u64, limit: u64 },

    #[error("quota exceeded: plugin has {current} keys, limit is {limit}")]
    QuotaExceeded { current: u64, limit: u64 },

    #[error("storage error: {0}")]
    Storage(#[from] StorageError),
}

pub struct PluginKvStore<D: Database> {
    plugin_id: String,
    db: Arc<D>,
    quota: KvQuota,
}

impl<D: Database> PluginKvStore<D> {
    pub fn new(plugin_id: String, db: Arc<D>, quota: KvQuota) -> Self {
        Self {
            plugin_id,
            db,
            quota,
        }
    }

    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    pub fn quota(&self) -> &KvQuota {
        &self.quota
    }

    pub fn database(&self) -> &Arc<D> {
        &self.db
    }

    pub async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, KvError> {
        let pid = self.plugin_id.clone();
        let k = key.to_string();
        let rows: Vec<Row> = self
            .db
            .query(
                "SELECT value FROM plugin_kv WHERE plugin_id = ?1 AND key = ?2",
                &[&pid, &k],
            )
            .await?;

        match rows.first().and_then(|row| row.get(0)) {
            Some(SqlValue::Blob(bytes)) => Ok(Some(bytes.clone())),
            _ => Ok(None),
        }
    }

    pub async fn set(&self, key: &str, value: &[u8]) -> Result<(), KvError> {
        let size = value.len() as u64;
        if size > self.quota.max_value_bytes {
            return Err(KvError::ValueTooLarge {
                size,
                limit: self.quota.max_value_bytes,
            });
        }

        let pid = self.plugin_id.clone();
        let k = key.to_string();

        // Check if this key already exists (an overwrite doesn't increase key count)
        let existing: Vec<Row> = self
            .db
            .query(
                "SELECT 1 FROM plugin_kv WHERE plugin_id = ?1 AND key = ?2",
                &[&pid, &k],
            )
            .await?;
        let is_new = existing.is_empty();

        if is_new {
            let usage = self.usage().await?;
            if usage.key_count >= self.quota.max_keys {
                return Err(KvError::QuotaExceeded {
                    current: usage.key_count,
                    limit: self.quota.max_keys,
                });
            }
        }

        let val = value.to_vec();
        self.db
            .execute(
                "INSERT INTO plugin_kv (plugin_id, key, value) VALUES (?1, ?2, ?3) \
                 ON CONFLICT (plugin_id, key) DO UPDATE SET value = excluded.value",
                &[&pid, &k, &val],
            )
            .await?;

        Ok(())
    }

    pub async fn delete(&self, key: &str) -> Result<(), KvError> {
        let pid = self.plugin_id.clone();
        let k = key.to_string();
        self.db
            .execute(
                "DELETE FROM plugin_kv WHERE plugin_id = ?1 AND key = ?2",
                &[&pid, &k],
            )
            .await?;
        Ok(())
    }

    pub async fn list_keys(&self, prefix: &str) -> Result<Vec<String>, KvError> {
        let pid = self.plugin_id.clone();

        let rows: Vec<Row> = if prefix.is_empty() {
            self.db
                .query("SELECT key FROM plugin_kv WHERE plugin_id = ?1", &[&pid])
                .await?
        } else {
            let pattern = format!("{prefix}%");
            self.db
                .query(
                    "SELECT key FROM plugin_kv WHERE plugin_id = ?1 AND key LIKE ?2",
                    &[&pid, &pattern],
                )
                .await?
        };

        Ok(rows
            .iter()
            .filter_map(|row| match row.get(0) {
                Some(SqlValue::Text(key)) => Some(key.clone()),
                _ => None,
            })
            .collect())
    }

    pub async fn usage(&self) -> Result<KvUsage, KvError> {
        let pid = self.plugin_id.clone();
        let row: Row = self
            .db
            .query_one(
                "SELECT COUNT(*), COALESCE(SUM(LENGTH(value)), 0) FROM plugin_kv WHERE plugin_id = ?1",
                &[&pid],
            )
            .await?;

        let key_count = match row.get(0) {
            Some(SqlValue::Integer(n)) => *n as u64,
            _ => 0,
        };
        let total_bytes = match row.get(1) {
            Some(SqlValue::Integer(n)) => *n as u64,
            _ => 0,
        };

        Ok(KvUsage {
            key_count,
            total_bytes,
        })
    }

    pub async fn clear_all(&self) -> Result<(), KvError> {
        let pid = self.plugin_id.clone();
        self.db
            .execute("DELETE FROM plugin_kv WHERE plugin_id = ?1", &[&pid])
            .await?;
        Ok(())
    }
}

#[cfg(all(test, feature = "native"))]
mod tests {
    use super::*;
    use std::path::Path;
    use waddle_storage::open_database;

    async fn open_temp_store(plugin_id: &str) -> (PluginKvStore<impl Database>, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().expect("failed to create temp dir");
        let db_path = dir.path().join("test.db");
        let db = open_database(Path::new(&db_path))
            .await
            .expect("failed to open database");
        let store = PluginKvStore::new(plugin_id.to_string(), Arc::new(db), KvQuota::default());
        (store, dir)
    }

    async fn open_temp_store_with_quota(
        plugin_id: &str,
        quota: KvQuota,
    ) -> (PluginKvStore<impl Database>, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().expect("failed to create temp dir");
        let db_path = dir.path().join("test.db");
        let db = open_database(Path::new(&db_path))
            .await
            .expect("failed to open database");
        let store = PluginKvStore::new(plugin_id.to_string(), Arc::new(db), quota);
        (store, dir)
    }

    // ---- CRUD operations ----

    #[tokio::test]
    async fn get_missing_key_returns_none() {
        let (store, _dir) = open_temp_store("test-plugin").await;
        let result = store.get("nonexistent").await.expect("get failed");
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn set_and_get_round_trip() {
        let (store, _dir) = open_temp_store("test-plugin").await;
        store.set("key1", b"hello world").await.expect("set failed");
        let value = store.get("key1").await.expect("get failed");
        assert_eq!(value, Some(b"hello world".to_vec()));
    }

    #[tokio::test]
    async fn set_overwrites_existing_value() {
        let (store, _dir) = open_temp_store("test-plugin").await;
        store.set("key1", b"first").await.expect("set failed");
        store.set("key1", b"second").await.expect("set failed");
        let value = store.get("key1").await.expect("get failed");
        assert_eq!(value, Some(b"second".to_vec()));
    }

    #[tokio::test]
    async fn delete_removes_key() {
        let (store, _dir) = open_temp_store("test-plugin").await;
        store.set("key1", b"value").await.expect("set failed");
        store.delete("key1").await.expect("delete failed");
        let value = store.get("key1").await.expect("get failed");
        assert_eq!(value, None);
    }

    #[tokio::test]
    async fn delete_nonexistent_key_is_ok() {
        let (store, _dir) = open_temp_store("test-plugin").await;
        store
            .delete("nonexistent")
            .await
            .expect("delete should not error on missing key");
    }

    #[tokio::test]
    async fn list_keys_all() {
        let (store, _dir) = open_temp_store("test-plugin").await;
        store.set("alpha", b"1").await.expect("set failed");
        store.set("beta", b"2").await.expect("set failed");
        store.set("gamma", b"3").await.expect("set failed");

        let mut keys = store.list_keys("").await.expect("list_keys failed");
        keys.sort();
        assert_eq!(keys, vec!["alpha", "beta", "gamma"]);
    }

    #[tokio::test]
    async fn list_keys_with_prefix() {
        let (store, _dir) = open_temp_store("test-plugin").await;
        store.set("session.a", b"1").await.expect("set failed");
        store.set("session.b", b"2").await.expect("set failed");
        store.set("config.x", b"3").await.expect("set failed");

        let mut keys = store.list_keys("session.").await.expect("list_keys failed");
        keys.sort();
        assert_eq!(keys, vec!["session.a", "session.b"]);
    }

    #[tokio::test]
    async fn usage_reflects_stored_data() {
        let (store, _dir) = open_temp_store("test-plugin").await;
        let usage = store.usage().await.expect("usage failed");
        assert_eq!(usage.key_count, 0);
        assert_eq!(usage.total_bytes, 0);

        store.set("k1", b"abc").await.expect("set failed");
        store.set("k2", b"defgh").await.expect("set failed");

        let usage = store.usage().await.expect("usage failed");
        assert_eq!(usage.key_count, 2);
        assert_eq!(usage.total_bytes, 8); // 3 + 5
    }

    #[tokio::test]
    async fn clear_all_removes_all_keys() {
        let (store, _dir) = open_temp_store("test-plugin").await;
        store.set("k1", b"a").await.expect("set failed");
        store.set("k2", b"b").await.expect("set failed");

        store.clear_all().await.expect("clear_all failed");

        let usage = store.usage().await.expect("usage failed");
        assert_eq!(usage.key_count, 0);
        assert_eq!(usage.total_bytes, 0);
    }

    // ---- Namespace isolation ----

    #[tokio::test]
    async fn namespace_isolation_between_plugins() {
        let dir = tempfile::TempDir::new().expect("failed to create temp dir");
        let db_path = dir.path().join("test.db");
        let db = Arc::new(
            open_database(Path::new(&db_path))
                .await
                .expect("failed to open database"),
        );

        let store_a = PluginKvStore::new("plugin-a".to_string(), db.clone(), KvQuota::default());
        let store_b = PluginKvStore::new("plugin-b".to_string(), db.clone(), KvQuota::default());

        store_a
            .set("shared-key", b"value-a")
            .await
            .expect("set failed");
        store_b
            .set("shared-key", b"value-b")
            .await
            .expect("set failed");

        let val_a = store_a.get("shared-key").await.expect("get failed");
        let val_b = store_b.get("shared-key").await.expect("get failed");

        assert_eq!(val_a, Some(b"value-a".to_vec()));
        assert_eq!(val_b, Some(b"value-b".to_vec()));
    }

    #[tokio::test]
    async fn clear_all_does_not_affect_other_plugin() {
        let dir = tempfile::TempDir::new().expect("failed to create temp dir");
        let db_path = dir.path().join("test.db");
        let db = Arc::new(
            open_database(Path::new(&db_path))
                .await
                .expect("failed to open database"),
        );

        let store_a = PluginKvStore::new("plugin-a".to_string(), db.clone(), KvQuota::default());
        let store_b = PluginKvStore::new("plugin-b".to_string(), db.clone(), KvQuota::default());

        store_a.set("key", b"a").await.expect("set failed");
        store_b.set("key", b"b").await.expect("set failed");

        store_a.clear_all().await.expect("clear_all failed");

        let val_a = store_a.get("key").await.expect("get failed");
        let val_b = store_b.get("key").await.expect("get failed");

        assert_eq!(val_a, None);
        assert_eq!(val_b, Some(b"b".to_vec()));
    }

    #[tokio::test]
    async fn list_keys_isolated_between_plugins() {
        let dir = tempfile::TempDir::new().expect("failed to create temp dir");
        let db_path = dir.path().join("test.db");
        let db = Arc::new(
            open_database(Path::new(&db_path))
                .await
                .expect("failed to open database"),
        );

        let store_a = PluginKvStore::new("plugin-a".to_string(), db.clone(), KvQuota::default());
        let store_b = PluginKvStore::new("plugin-b".to_string(), db.clone(), KvQuota::default());

        store_a.set("k1", b"a").await.expect("set failed");
        store_a.set("k2", b"a").await.expect("set failed");
        store_b.set("k3", b"b").await.expect("set failed");

        let keys_a = store_a.list_keys("").await.expect("list_keys failed");
        let keys_b = store_b.list_keys("").await.expect("list_keys failed");

        assert_eq!(keys_a.len(), 2);
        assert_eq!(keys_b.len(), 1);
        assert!(keys_a.contains(&"k1".to_string()));
        assert!(keys_a.contains(&"k2".to_string()));
        assert!(keys_b.contains(&"k3".to_string()));
    }

    #[tokio::test]
    async fn delete_isolated_between_plugins() {
        let dir = tempfile::TempDir::new().expect("failed to create temp dir");
        let db_path = dir.path().join("test.db");
        let db = Arc::new(
            open_database(Path::new(&db_path))
                .await
                .expect("failed to open database"),
        );

        let store_a = PluginKvStore::new("plugin-a".to_string(), db.clone(), KvQuota::default());
        let store_b = PluginKvStore::new("plugin-b".to_string(), db.clone(), KvQuota::default());

        store_a.set("key", b"a").await.expect("set failed");
        store_b.set("key", b"b").await.expect("set failed");

        store_a.delete("key").await.expect("delete failed");

        let val_b = store_b.get("key").await.expect("get failed");
        assert_eq!(val_b, Some(b"b".to_vec()));
    }

    // ---- Quota enforcement ----

    #[tokio::test]
    async fn value_too_large_rejected() {
        let quota = KvQuota {
            max_keys: 100,
            max_value_bytes: 10,
        };
        let (store, _dir) = open_temp_store_with_quota("test-plugin", quota).await;

        let large_value = vec![0u8; 11];
        let result = store.set("key", &large_value).await;

        assert!(matches!(
            result,
            Err(KvError::ValueTooLarge {
                size: 11,
                limit: 10
            })
        ));
    }

    #[tokio::test]
    async fn value_at_exact_limit_accepted() {
        let quota = KvQuota {
            max_keys: 100,
            max_value_bytes: 10,
        };
        let (store, _dir) = open_temp_store_with_quota("test-plugin", quota).await;

        let exact_value = vec![0u8; 10];
        store
            .set("key", &exact_value)
            .await
            .expect("value at exact limit should be accepted");
    }

    #[tokio::test]
    async fn key_quota_exceeded() {
        let quota = KvQuota {
            max_keys: 3,
            max_value_bytes: 1_048_576,
        };
        let (store, _dir) = open_temp_store_with_quota("test-plugin", quota).await;

        store.set("k1", b"v").await.expect("set failed");
        store.set("k2", b"v").await.expect("set failed");
        store.set("k3", b"v").await.expect("set failed");

        let result = store.set("k4", b"v").await;
        assert!(matches!(
            result,
            Err(KvError::QuotaExceeded {
                current: 3,
                limit: 3
            })
        ));
    }

    #[tokio::test]
    async fn overwrite_does_not_count_as_new_key() {
        let quota = KvQuota {
            max_keys: 2,
            max_value_bytes: 1_048_576,
        };
        let (store, _dir) = open_temp_store_with_quota("test-plugin", quota).await;

        store.set("k1", b"v1").await.expect("set failed");
        store.set("k2", b"v2").await.expect("set failed");

        // Overwriting k1 should succeed even though we're at max_keys
        store
            .set("k1", b"updated")
            .await
            .expect("overwrite should not be blocked by quota");

        let value = store.get("k1").await.expect("get failed");
        assert_eq!(value, Some(b"updated".to_vec()));
    }

    #[tokio::test]
    async fn quota_freed_after_delete() {
        let quota = KvQuota {
            max_keys: 2,
            max_value_bytes: 1_048_576,
        };
        let (store, _dir) = open_temp_store_with_quota("test-plugin", quota).await;

        store.set("k1", b"v").await.expect("set failed");
        store.set("k2", b"v").await.expect("set failed");

        store.delete("k1").await.expect("delete failed");

        store
            .set("k3", b"v")
            .await
            .expect("should succeed after freeing a slot");
    }

    #[tokio::test]
    async fn usage_isolated_between_plugins() {
        let dir = tempfile::TempDir::new().expect("failed to create temp dir");
        let db_path = dir.path().join("test.db");
        let db = Arc::new(
            open_database(Path::new(&db_path))
                .await
                .expect("failed to open database"),
        );

        let store_a = PluginKvStore::new("plugin-a".to_string(), db.clone(), KvQuota::default());
        let store_b = PluginKvStore::new("plugin-b".to_string(), db.clone(), KvQuota::default());

        store_a.set("k1", b"aaa").await.expect("set failed");
        store_a.set("k2", b"bbb").await.expect("set failed");
        store_b.set("k1", b"cc").await.expect("set failed");

        let usage_a = store_a.usage().await.expect("usage failed");
        let usage_b = store_b.usage().await.expect("usage failed");

        assert_eq!(usage_a.key_count, 2);
        assert_eq!(usage_a.total_bytes, 6);
        assert_eq!(usage_b.key_count, 1);
        assert_eq!(usage_b.total_bytes, 2);
    }

    #[tokio::test]
    async fn binary_data_round_trip() {
        let (store, _dir) = open_temp_store("test-plugin").await;
        let binary = vec![0u8, 1, 2, 255, 254, 253, 0, 128];
        store.set("bin", &binary).await.expect("set failed");
        let value = store.get("bin").await.expect("get failed");
        assert_eq!(value, Some(binary));
    }

    #[tokio::test]
    async fn empty_value_round_trip() {
        let (store, _dir) = open_temp_store("test-plugin").await;
        store.set("empty", b"").await.expect("set failed");
        let value = store.get("empty").await.expect("get failed");
        assert_eq!(value, Some(vec![]));
    }
}
