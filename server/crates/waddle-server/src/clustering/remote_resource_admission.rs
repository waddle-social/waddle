//! Durable, cluster-wide ordering for physical full-JID socket admissions.
//!
//! A per-process socket generation cannot order two concurrent binds hosted by
//! different nodes. This store gives each bind a Postgres-sequenced epoch and
//! keeps only the newest exact reservation for a full JID. Owner-side relay
//! handlers prove that reservation before every effect, so a delayed register
//! or cleanup cannot displace a newer socket.

use std::collections::HashMap;

use async_trait::async_trait;
use jid::FullJid;
use tokio::sync::Mutex;
use waddle_xmpp::ownership::NodeIdentity;

use crate::db::{Database, DatabaseError, Transaction};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct RemoteResourceRegistrationId(uuid::Uuid);

impl RemoteResourceRegistrationId {
    pub(crate) fn fresh() -> Self {
        Self(uuid::Uuid::new_v4())
    }

    fn as_db_string(self) -> String {
        self.0.to_string()
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct RemoteResourceAdmissionEpoch(pub(crate) i64);

#[derive(Debug, thiserror::Error)]
pub enum RemoteResourceAdmissionError {
    #[error("remote-resource admission backend failed: {0}")]
    Backend(String),
    #[error("remote-resource admission epoch space is exhausted")]
    EpochExhausted,
    #[error("physical socket node is not serving-eligible")]
    StaleSocketNode,
}

fn db_error(error: DatabaseError) -> RemoteResourceAdmissionError {
    if matches!(
        &error,
        DatabaseError::Internal(sqlx::Error::Database(inner))
            if inner.code().as_deref() == Some("2200H")
    ) {
        return RemoteResourceAdmissionError::EpochExhausted;
    }
    RemoteResourceAdmissionError::Backend(error.to_string())
}

async fn lock_full_jid(
    tx: &mut Transaction<'_>,
    jid: &FullJid,
) -> Result<(), RemoteResourceAdmissionError> {
    let mut rows = tx
        .query(
            "SELECT pg_advisory_xact_lock(hashtextextended(?, 8718))",
            crate::db_params![jid.to_string()],
        )
        .await
        .map_err(db_error)?;
    let _ = rows.next().await.map_err(db_error)?;
    Ok(())
}

#[async_trait]
pub trait RemoteResourceAdmissionStore: Send + Sync {
    async fn ensure_schema(&self) -> Result<(), RemoteResourceAdmissionError>;

    async fn reserve(
        &self,
        jid: &FullJid,
        registration_id: RemoteResourceRegistrationId,
        socket_node: &NodeIdentity,
    ) -> Result<RemoteResourceAdmissionEpoch, RemoteResourceAdmissionError>;

    async fn is_current(
        &self,
        jid: &FullJid,
        registration_id: RemoteResourceRegistrationId,
        admission_epoch: RemoteResourceAdmissionEpoch,
        socket_node: &NodeIdentity,
    ) -> Result<bool, RemoteResourceAdmissionError>;

    async fn cancel(
        &self,
        jid: &FullJid,
        registration_id: RemoteResourceRegistrationId,
        admission_epoch: RemoteResourceAdmissionEpoch,
        socket_node: &NodeIdentity,
    ) -> Result<bool, RemoteResourceAdmissionError>;

    /// Bounded cleanup for admissions whose exact socket-node incarnation is
    /// absent or committed expired. Raw heartbeat age is deliberately not an
    /// authority for deleting another node's reservation.
    async fn prune_stale(&self, limit: usize) -> Result<u64, RemoteResourceAdmissionError>;
}

pub struct PostgresRemoteResourceAdmissionStore {
    db: Database,
}

impl PostgresRemoteResourceAdmissionStore {
    pub fn new(db: Database) -> Self {
        Self { db }
    }
}

#[async_trait]
impl RemoteResourceAdmissionStore for PostgresRemoteResourceAdmissionStore {
    async fn ensure_schema(&self) -> Result<(), RemoteResourceAdmissionError> {
        let conn = self.db.guard().await.map_err(db_error)?;
        conn.execute(
            "CREATE SEQUENCE IF NOT EXISTS clustering_remote_resource_admission_epoch_seq AS BIGINT",
            (),
        )
        .await
        .map_err(db_error)?;
        conn.execute(
            r#"
            CREATE TABLE IF NOT EXISTS clustering_remote_resource_admissions (
                full_jid        TEXT PRIMARY KEY,
                registration_id TEXT NOT NULL,
                admission_epoch BIGINT NOT NULL,
                socket_node_id  TEXT NOT NULL,
                socket_epoch    TEXT NOT NULL
            )
            "#,
            (),
        )
        .await
        .map_err(db_error)?;
        conn.execute(
            r#"
            CREATE INDEX IF NOT EXISTS clustering_remote_resource_admissions_socket_identity
                ON clustering_remote_resource_admissions (
                    socket_node_id, socket_epoch, full_jid
                )
            "#,
            (),
        )
        .await
        .map_err(db_error)?;
        Ok(())
    }

    async fn reserve(
        &self,
        jid: &FullJid,
        registration_id: RemoteResourceRegistrationId,
        socket_node: &NodeIdentity,
    ) -> Result<RemoteResourceAdmissionEpoch, RemoteResourceAdmissionError> {
        let mut tx = self
            .db
            .control_plane_begin_fenced()
            .await
            .map_err(db_error)?;
        lock_full_jid(&mut tx, jid).await?;
        let mut live_rows = tx
            .query(
                r#"
                WITH locked AS MATERIALIZED (
                    SELECT n.heartbeat, n.expired, n.draining, n.lease_ttl_ms
                    FROM clustering_nodes n
                    WHERE n.node_id = ?
                      AND n.node_epoch = ?
                    FOR SHARE OF n
                )
                SELECT 1 FROM locked
                WHERE NOT expired
                  AND NOT draining
                  AND heartbeat >= clock_timestamp() - (
                      lease_ttl_ms::text || ' milliseconds'
                  )::interval
                "#,
                crate::db_params![socket_node.node_id.clone(), socket_node.node_epoch.clone(),],
            )
            .await
            .map_err(db_error)?;
        let socket_is_live = live_rows.next().await.map_err(db_error)?.is_some();
        drop(live_rows);
        if !socket_is_live {
            return Err(RemoteResourceAdmissionError::StaleSocketNode);
        }
        let mut rows = tx
            .query(
                r#"
                INSERT INTO clustering_remote_resource_admissions (
                    full_jid, registration_id, admission_epoch,
                    socket_node_id, socket_epoch
                )
                VALUES (?, ?, nextval('clustering_remote_resource_admission_epoch_seq'), ?, ?)
                ON CONFLICT (full_jid) DO UPDATE SET
                    registration_id = EXCLUDED.registration_id,
                    admission_epoch = EXCLUDED.admission_epoch,
                    socket_node_id = EXCLUDED.socket_node_id,
                    socket_epoch = EXCLUDED.socket_epoch
                RETURNING admission_epoch
                "#,
                crate::db_params![
                    jid.to_string(),
                    registration_id.as_db_string(),
                    socket_node.node_id.clone(),
                    socket_node.node_epoch.clone(),
                ],
            )
            .await
            .map_err(db_error)?;
        let Some(row) = rows.next().await.map_err(db_error)? else {
            return Err(RemoteResourceAdmissionError::Backend(
                "remote-resource admission upsert returned no epoch".to_string(),
            ));
        };
        let epoch = row
            .get::<i64>(0)
            .map(RemoteResourceAdmissionEpoch)
            .map_err(db_error)?;
        drop(rows);
        tx.commit().await.map_err(db_error)?;
        Ok(epoch)
    }

    async fn is_current(
        &self,
        jid: &FullJid,
        registration_id: RemoteResourceRegistrationId,
        admission_epoch: RemoteResourceAdmissionEpoch,
        socket_node: &NodeIdentity,
    ) -> Result<bool, RemoteResourceAdmissionError> {
        let mut tx = self
            .db
            .control_plane_begin_fenced()
            .await
            .map_err(db_error)?;
        lock_full_jid(&mut tx, jid).await?;
        let mut rows = tx
            .query(
                r#"
                WITH locked AS MATERIALIZED (
                    SELECT n.heartbeat, n.expired, n.lease_ttl_ms
                    FROM clustering_remote_resource_admissions a
                    JOIN clustering_nodes n
                      ON n.node_id = a.socket_node_id
                     AND n.node_epoch = a.socket_epoch
                    WHERE a.full_jid = ?
                      AND a.registration_id = ?
                      AND a.admission_epoch = ?
                      AND a.socket_node_id = ?
                      AND a.socket_epoch = ?
                    FOR SHARE OF n
                )
                SELECT 1 FROM locked
                WHERE NOT expired
                  AND heartbeat >= clock_timestamp() - (
                      lease_ttl_ms::text || ' milliseconds'
                  )::interval
                "#,
                crate::db_params![
                    jid.to_string(),
                    registration_id.as_db_string(),
                    admission_epoch.0,
                    socket_node.node_id.clone(),
                    socket_node.node_epoch.clone(),
                ],
            )
            .await
            .map_err(db_error)?;
        let current = rows.next().await.map_err(db_error)?.is_some();
        drop(rows);
        tx.commit().await.map_err(db_error)?;
        Ok(current)
    }

    async fn cancel(
        &self,
        jid: &FullJid,
        registration_id: RemoteResourceRegistrationId,
        admission_epoch: RemoteResourceAdmissionEpoch,
        socket_node: &NodeIdentity,
    ) -> Result<bool, RemoteResourceAdmissionError> {
        let mut tx = self
            .db
            .control_plane_begin_fenced()
            .await
            .map_err(db_error)?;
        lock_full_jid(&mut tx, jid).await?;
        let affected = tx
            .execute(
                r#"
                DELETE FROM clustering_remote_resource_admissions
                WHERE full_jid = ?
                  AND registration_id = ?
                  AND admission_epoch = ?
                  AND socket_node_id = ?
                  AND socket_epoch = ?
                "#,
                crate::db_params![
                    jid.to_string(),
                    registration_id.as_db_string(),
                    admission_epoch.0,
                    socket_node.node_id.clone(),
                    socket_node.node_epoch.clone(),
                ],
            )
            .await
            .map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        Ok(affected == 1)
    }

    async fn prune_stale(&self, limit: usize) -> Result<u64, RemoteResourceAdmissionError> {
        let mut tx = self
            .db
            .control_plane_begin_fenced()
            .await
            .map_err(db_error)?;
        let affected = tx
            .execute(
                r#"
            WITH candidates AS MATERIALIZED (
                SELECT
                    a.full_jid,
                    a.registration_id,
                    a.admission_epoch,
                    a.socket_node_id,
                    a.socket_epoch
                FROM clustering_remote_resource_admissions a
                LEFT JOIN clustering_nodes n
                  ON n.node_id = a.socket_node_id
                 AND n.node_epoch = a.socket_epoch
                WHERE n.node_id IS NULL OR n.expired
                ORDER BY a.full_jid
                LIMIT ?
            )
            DELETE FROM clustering_remote_resource_admissions a
            USING candidates c
            WHERE a.full_jid = c.full_jid
              AND a.registration_id = c.registration_id
              AND a.admission_epoch = c.admission_epoch
              AND a.socket_node_id = c.socket_node_id
              AND a.socket_epoch = c.socket_epoch
            "#,
                crate::db_params![i64::try_from(limit).unwrap_or(i64::MAX)],
            )
            .await
            .map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        Ok(affected)
    }
}

#[derive(Clone)]
struct InMemoryAdmission {
    registration_id: RemoteResourceRegistrationId,
    admission_epoch: RemoteResourceAdmissionEpoch,
    socket_node: NodeIdentity,
}

/// Exact in-memory implementation used by deterministic relay unit tests.
#[derive(Default)]
pub struct InMemoryRemoteResourceAdmissionStore {
    next_epoch: std::sync::atomic::AtomicI64,
    rows: Mutex<HashMap<FullJid, InMemoryAdmission>>,
}

#[async_trait]
impl RemoteResourceAdmissionStore for InMemoryRemoteResourceAdmissionStore {
    async fn ensure_schema(&self) -> Result<(), RemoteResourceAdmissionError> {
        Ok(())
    }

    async fn reserve(
        &self,
        jid: &FullJid,
        registration_id: RemoteResourceRegistrationId,
        socket_node: &NodeIdentity,
    ) -> Result<RemoteResourceAdmissionEpoch, RemoteResourceAdmissionError> {
        let raw = self
            .next_epoch
            .fetch_update(
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
                |current| current.checked_add(1),
            )
            .map_err(|_| RemoteResourceAdmissionError::EpochExhausted)?
            .checked_add(1)
            .ok_or(RemoteResourceAdmissionError::EpochExhausted)?;
        let admission_epoch = RemoteResourceAdmissionEpoch(raw);
        self.rows.lock().await.insert(
            jid.clone(),
            InMemoryAdmission {
                registration_id,
                admission_epoch,
                socket_node: socket_node.clone(),
            },
        );
        Ok(admission_epoch)
    }

    async fn is_current(
        &self,
        jid: &FullJid,
        registration_id: RemoteResourceRegistrationId,
        admission_epoch: RemoteResourceAdmissionEpoch,
        socket_node: &NodeIdentity,
    ) -> Result<bool, RemoteResourceAdmissionError> {
        Ok(self.rows.lock().await.get(jid).is_some_and(|current| {
            current.registration_id == registration_id
                && current.admission_epoch == admission_epoch
                && current.socket_node == *socket_node
        }))
    }

    async fn cancel(
        &self,
        jid: &FullJid,
        registration_id: RemoteResourceRegistrationId,
        admission_epoch: RemoteResourceAdmissionEpoch,
        socket_node: &NodeIdentity,
    ) -> Result<bool, RemoteResourceAdmissionError> {
        let mut rows = self.rows.lock().await;
        let current = rows.get(jid).is_some_and(|current| {
            current.registration_id == registration_id
                && current.admission_epoch == admission_epoch
                && current.socket_node == *socket_node
        });
        if current {
            rows.remove(jid);
        }
        Ok(current)
    }

    async fn prune_stale(&self, _limit: usize) -> Result<u64, RemoteResourceAdmissionError> {
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clustering::claims::PostgresClaimStore;
    use crate::db::{DatabaseConfig, DatabaseDriver};
    use waddle_xmpp::ownership::ClaimStore as _;

    fn socket_node(epoch: &str) -> NodeIdentity {
        NodeIdentity::new("socket-node", epoch)
    }

    fn full_jid(resource: &str) -> FullJid {
        format!("juliet@example.test/{resource}")
            .parse()
            .expect("test full JID")
    }

    #[tokio::test]
    async fn newer_reservation_wins_and_exact_old_cancel_cannot_delete_it() {
        let store = InMemoryRemoteResourceAdmissionStore::default();
        let jid = full_jid("phone");
        let node = socket_node("epoch-1");
        let first_id = RemoteResourceRegistrationId::fresh();
        let first_epoch = store
            .reserve(&jid, first_id, &node)
            .await
            .expect("first admission");
        let second_id = RemoteResourceRegistrationId::fresh();
        let second_epoch = store
            .reserve(&jid, second_id, &node)
            .await
            .expect("second admission");

        assert!(second_epoch > first_epoch);
        assert!(!store
            .is_current(&jid, first_id, first_epoch, &node)
            .await
            .expect("check first admission"));
        assert!(!store
            .cancel(&jid, first_id, first_epoch, &node)
            .await
            .expect("cancel superseded admission"));
        assert!(store
            .is_current(&jid, second_id, second_epoch, &node)
            .await
            .expect("check second admission"));
    }

    #[tokio::test]
    async fn unregister_before_delayed_register_makes_that_register_terminally_stale() {
        let store = InMemoryRemoteResourceAdmissionStore::default();
        let jid = full_jid("laptop");
        let node = socket_node("epoch-1");
        let registration_id = RemoteResourceRegistrationId::fresh();
        let admission_epoch = store
            .reserve(&jid, registration_id, &node)
            .await
            .expect("reserve admission");

        assert!(store
            .cancel(&jid, registration_id, admission_epoch, &node)
            .await
            .expect("unregister cancels admission"));
        assert!(!store
            .is_current(&jid, registration_id, admission_epoch, &node)
            .await
            .expect("delayed register re-proves admission"));
    }

    async fn postgres_store() -> Option<(Database, PostgresRemoteResourceAdmissionStore)> {
        let url = std::env::var("WADDLE_TEST_POSTGRES_URL").ok()?;
        let db = Database::from_config(
            "remote-resource-admission-test",
            &DatabaseConfig::new(DatabaseDriver::Postgres, url)
                .with_control_plane_pool(crate::db::DEFAULT_CONTROL_PLANE_POOL_SIZE),
        )
        .await
        .expect("open test Postgres");
        PostgresClaimStore::new(db.clone())
            .ensure_schema()
            .await
            .expect("ensure clustering node schema");
        let store = PostgresRemoteResourceAdmissionStore::new(db.clone());
        store
            .ensure_schema()
            .await
            .expect("ensure admission schema");
        let conn = db.guard().await.expect("test database guard");
        conn.execute("DELETE FROM clustering_remote_resource_admissions", ())
            .await
            .expect("clean admissions");
        conn.execute("DELETE FROM clustering_nodes", ())
            .await
            .expect("clean nodes");
        Some((db, store))
    }

    async fn insert_live_node(db: &Database, node: &NodeIdentity) {
        db.guard()
            .await
            .expect("test database guard")
            .execute(
                r#"
                INSERT INTO clustering_nodes (
                    node_id, node_epoch, heartbeat, expired, draining, lease_ttl_ms
                ) VALUES (?, ?, clock_timestamp(), false, false, 30000)
                "#,
                crate::db_params![node.node_id.clone(), node.node_epoch.clone()],
            )
            .await
            .expect("insert live socket node");
    }

    async fn admission_count(db: &Database) -> i64 {
        let mut rows = db
            .guard()
            .await
            .expect("test database guard")
            .query(
                "SELECT COUNT(*) FROM clustering_remote_resource_admissions",
                (),
            )
            .await
            .expect("count admissions");
        rows.next()
            .await
            .expect("read admission count")
            .expect("admission count row")
            .get::<i64>(0)
            .expect("decode admission count")
    }

    async fn socket_identity_index_exists(db: &Database) -> bool {
        let mut rows = db
            .guard()
            .await
            .expect("test database guard")
            .query(
                r#"
                SELECT 1
                FROM pg_indexes
                WHERE schemaname = current_schema()
                  AND tablename = 'clustering_remote_resource_admissions'
                  AND indexname = 'clustering_remote_resource_admissions_socket_identity'
                "#,
                (),
            )
            .await
            .expect("query admission cleanup index");
        rows.next()
            .await
            .expect("read admission cleanup index query")
            .is_some()
    }

    async fn waiting_advisory_lock_exists(db: &Database) -> bool {
        let mut rows = db
            .guard()
            .await
            .expect("test database guard")
            .query(
                "SELECT EXISTS (SELECT 1 FROM pg_locks WHERE locktype = 'advisory' AND NOT granted)",
                (),
            )
            .await
            .expect("query waiting advisory locks");
        rows.next()
            .await
            .expect("read waiting advisory lock query")
            .expect("waiting advisory lock row")
            .get::<bool>(0)
            .expect("decode waiting advisory lock flag")
    }

    #[tokio::test]
    async fn postgres_reserve_and_cancel_are_exact_and_globally_ordered() {
        let _table_guard = super::super::claims::clustering_control_plane_table_lock()
            .lock()
            .await;
        let Some((db, store)) = postgres_store().await else {
            return;
        };
        let jid = full_jid("phone");
        let node = socket_node("epoch-1");
        insert_live_node(&db, &node).await;
        let first_id = RemoteResourceRegistrationId::fresh();
        let first_epoch = store
            .reserve(&jid, first_id, &node)
            .await
            .expect("first Postgres admission");
        let second_id = RemoteResourceRegistrationId::fresh();
        let second_epoch = store
            .reserve(&jid, second_id, &node)
            .await
            .expect("second Postgres admission");

        assert!(second_epoch > first_epoch);
        assert!(!store
            .cancel(&jid, first_id, first_epoch, &node)
            .await
            .expect("cancel old exact tuple"));
        assert!(store
            .is_current(&jid, second_id, second_epoch, &node)
            .await
            .expect("new admission remains current"));
        db.guard()
            .await
            .expect("test database guard")
            .execute(
                "UPDATE clustering_nodes SET draining = true WHERE node_id = ? AND node_epoch = ?",
                crate::db_params![node.node_id.clone(), node.node_epoch.clone()],
            )
            .await
            .expect("mark socket node draining");
        assert!(
            store
                .is_current(&jid, second_id, second_epoch, &node)
                .await
                .expect("existing admission remains current while draining"),
            "draining refuses new sockets but does not retroactively revoke an existing one"
        );
        assert!(matches!(
            store
                .reserve(
                    &full_jid("new-while-draining"),
                    RemoteResourceRegistrationId::fresh(),
                    &node,
                )
                .await,
            Err(RemoteResourceAdmissionError::StaleSocketNode)
        ));
        assert!(store
            .cancel(&jid, second_id, second_epoch, &node)
            .await
            .expect("cancel current exact tuple"));
        assert_eq!(admission_count(&db).await, 0);
    }

    #[tokio::test]
    async fn postgres_normal_cancel_and_bounded_expired_node_pruning_bound_cardinality() {
        let _table_guard = super::super::claims::clustering_control_plane_table_lock()
            .lock()
            .await;
        let Some((db, store)) = postgres_store().await else {
            return;
        };
        assert!(socket_identity_index_exists(&db).await);
        let node = socket_node("epoch-1");
        insert_live_node(&db, &node).await;
        let mut admissions = Vec::new();
        for index in 0..8 {
            let jid = full_jid(&format!("resource-{index}"));
            let registration_id = RemoteResourceRegistrationId::fresh();
            let admission_epoch = store
                .reserve(&jid, registration_id, &node)
                .await
                .expect("reserve Postgres admission");
            admissions.push((jid, registration_id, admission_epoch));
        }
        for (jid, registration_id, admission_epoch) in admissions.iter().take(2) {
            assert!(store
                .cancel(jid, *registration_id, *admission_epoch, &node)
                .await
                .expect("normal exact cancellation"));
        }
        assert_eq!(admission_count(&db).await, 6);

        db.guard()
            .await
            .expect("test database guard")
            .execute(
                "UPDATE clustering_nodes SET expired = true WHERE node_id = ? AND node_epoch = ?",
                crate::db_params![node.node_id.clone(), node.node_epoch.clone()],
            )
            .await
            .expect("commit socket-node expiry");
        assert_eq!(store.prune_stale(2).await.expect("first bounded prune"), 2);
        assert_eq!(admission_count(&db).await, 4);
        assert_eq!(store.prune_stale(2).await.expect("second bounded prune"), 2);
        assert_eq!(admission_count(&db).await, 2);
        assert_eq!(store.prune_stale(2).await.expect("final bounded prune"), 2);
        assert_eq!(admission_count(&db).await, 0);
        assert_eq!(store.prune_stale(2).await.expect("empty bounded prune"), 0);
    }

    #[tokio::test]
    async fn postgres_reserve_validates_socket_lease_after_waiting_for_full_jid_lock() {
        let _table_guard = super::super::claims::clustering_control_plane_table_lock()
            .lock()
            .await;
        let Some((db, _store)) = postgres_store().await else {
            return;
        };
        let jid = full_jid("locked");
        let node = socket_node("epoch-1");
        insert_live_node(&db, &node).await;

        let mut blocker = db
            .control_plane_begin()
            .await
            .expect("begin advisory-lock blocker");
        lock_full_jid(&mut blocker, &jid)
            .await
            .expect("hold full-JID advisory lock");
        let reserve = tokio::spawn({
            let db = db.clone();
            let jid = jid.clone();
            let node = node.clone();
            async move {
                PostgresRemoteResourceAdmissionStore::new(db)
                    .reserve(&jid, RemoteResourceRegistrationId::fresh(), &node)
                    .await
            }
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !waiting_advisory_lock_exists(&db).await {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("reserve waits on the per-full-JID advisory lock");

        db.guard()
            .await
            .expect("test database guard")
            .execute(
                r#"
                UPDATE clustering_nodes
                SET heartbeat = clock_timestamp() - interval '1 hour'
                WHERE node_id = ? AND node_epoch = ?
                "#,
                crate::db_params![node.node_id.clone(), node.node_epoch.clone()],
            )
            .await
            .expect("expire socket lease while reserve is waiting");
        blocker.commit().await.expect("release advisory lock");

        assert!(matches!(
            reserve.await.expect("join blocked reserve"),
            Err(RemoteResourceAdmissionError::StaleSocketNode)
        ));
        assert_eq!(admission_count(&db).await, 0);
    }

    #[tokio::test]
    async fn postgres_reserve_validates_socket_lease_after_node_row_lock_wait() {
        let _table_guard = super::super::claims::clustering_control_plane_table_lock()
            .lock()
            .await;
        let Some((db, _store)) = postgres_store().await else {
            return;
        };
        let jid = full_jid("node-row-locked");
        let node = socket_node("epoch-1");
        insert_live_node(&db, &node).await;
        db.guard()
            .await
            .unwrap()
            .execute(
                "UPDATE clustering_nodes SET heartbeat = clock_timestamp(), lease_ttl_ms = 100 WHERE node_id = ? AND node_epoch = ?",
                crate::db_params![node.node_id.clone(), node.node_epoch.clone()],
            )
            .await
            .unwrap();

        let mut blocker = db.begin().await.unwrap();
        let mut rows = blocker
            .query(
                "SELECT 1 FROM clustering_nodes WHERE node_id = ? AND node_epoch = ? FOR UPDATE",
                crate::db_params![node.node_id.clone(), node.node_epoch.clone()],
            )
            .await
            .unwrap();
        assert!(rows.next().await.unwrap().is_some());
        drop(rows);

        let reserve = tokio::spawn({
            let db = db.clone();
            let jid = jid.clone();
            let node = node.clone();
            async move {
                PostgresRemoteResourceAdmissionStore::new(db)
                    .reserve(&jid, RemoteResourceRegistrationId::fresh(), &node)
                    .await
            }
        });
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        assert!(!reserve.is_finished(), "reserve must wait on the node row");
        tokio::time::sleep(std::time::Duration::from_millis(360)).await;
        blocker.commit().await.unwrap();

        assert!(matches!(
            reserve.await.unwrap(),
            Err(RemoteResourceAdmissionError::StaleSocketNode)
        ));
        assert_eq!(admission_count(&db).await, 0);
    }

    #[tokio::test]
    async fn postgres_is_current_validates_socket_lease_after_node_row_lock_wait() {
        let _table_guard = super::super::claims::clustering_control_plane_table_lock()
            .lock()
            .await;
        let Some((db, store)) = postgres_store().await else {
            return;
        };
        let jid = full_jid("current-node-row-locked");
        let node = socket_node("epoch-1");
        insert_live_node(&db, &node).await;
        let registration_id = RemoteResourceRegistrationId::fresh();
        let admission_epoch = store.reserve(&jid, registration_id, &node).await.unwrap();
        db.guard()
            .await
            .unwrap()
            .execute(
                "UPDATE clustering_nodes SET heartbeat = clock_timestamp(), lease_ttl_ms = 100 WHERE node_id = ? AND node_epoch = ?",
                crate::db_params![node.node_id.clone(), node.node_epoch.clone()],
            )
            .await
            .unwrap();

        let mut blocker = db.begin().await.unwrap();
        let mut rows = blocker
            .query(
                "SELECT 1 FROM clustering_nodes WHERE node_id = ? AND node_epoch = ? FOR UPDATE",
                crate::db_params![node.node_id.clone(), node.node_epoch.clone()],
            )
            .await
            .unwrap();
        assert!(rows.next().await.unwrap().is_some());
        drop(rows);

        let current = tokio::spawn({
            let db = db.clone();
            let jid = jid.clone();
            let node = node.clone();
            async move {
                PostgresRemoteResourceAdmissionStore::new(db)
                    .is_current(&jid, registration_id, admission_epoch, &node)
                    .await
            }
        });
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        assert!(
            !current.is_finished(),
            "is_current must wait on the node row"
        );
        tokio::time::sleep(std::time::Duration::from_millis(360)).await;
        blocker.commit().await.unwrap();

        assert!(!current.await.unwrap().unwrap());
    }
}
