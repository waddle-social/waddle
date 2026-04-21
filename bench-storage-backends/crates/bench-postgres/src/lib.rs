//! PostgreSQL backend for the stanza benchmark.

use std::sync::Arc;

use async_trait::async_trait;
use bench_core::message::{ArchivedMessage, MamQuery};
use bench_core::store::{StanzaStore, StoreError};
use sqlx::postgres::PgPoolOptions;
use sqlx::{FromRow, QueryBuilder};

const MAM_SCHEMA_STATEMENTS: &[&str] = &[
    r#"CREATE TABLE IF NOT EXISTS mam_messages (
        id TEXT PRIMARY KEY,
        room_jid TEXT NOT NULL,
        timestamp TIMESTAMPTZ NOT NULL,
        from_jid TEXT NOT NULL,
        to_jid TEXT NOT NULL,
        body TEXT NOT NULL,
        stanza_id TEXT,
        thread_id TEXT,
        reply_to_id TEXT,
        reply_to_jid TEXT,
        origin_id TEXT,
        message_type TEXT NOT NULL DEFAULT 'chat',
        stanza_xml TEXT,
        created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
    )"#,
    "CREATE INDEX IF NOT EXISTS idx_mam_room_timestamp ON mam_messages(room_jid, timestamp DESC)",
    "CREATE INDEX IF NOT EXISTS idx_mam_room_sender ON mam_messages(room_jid, from_jid, timestamp DESC)",
    "CREATE INDEX IF NOT EXISTS idx_mam_room_id ON mam_messages(room_jid, id)",
];

pub struct PostgresStore {
    pool: sqlx::PgPool,
}

impl PostgresStore {
    pub async fn connect(
        database_url: &str,
        max_connections: u32,
    ) -> Result<Arc<Self>, StoreError> {
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .connect(database_url)
            .await
            .map_err(StoreError::backend)?;
        Ok(Arc::new(Self { pool }))
    }
}

#[derive(Debug, FromRow)]
struct PgArchivedMessageRow {
    id: String,
    room_jid: String,
    timestamp: chrono::DateTime<chrono::Utc>,
    from_jid: String,
    to_jid: String,
    body: String,
    stanza_id: Option<String>,
    thread_id: Option<String>,
    reply_to_id: Option<String>,
    reply_to_jid: Option<String>,
    origin_id: Option<String>,
    message_type: String,
    stanza_xml: Option<String>,
}

impl From<PgArchivedMessageRow> for ArchivedMessage {
    fn from(value: PgArchivedMessageRow) -> Self {
        Self {
            id: value.id,
            room_jid: value.room_jid,
            timestamp: value.timestamp,
            from: value.from_jid,
            to: value.to_jid,
            body: value.body,
            stanza_id: value.stanza_id,
            thread_id: value.thread_id,
            reply_to_id: value.reply_to_id,
            reply_to_jid: value.reply_to_jid,
            origin_id: value.origin_id,
            message_type: value.message_type,
            stanza_xml: value.stanza_xml,
        }
    }
}

#[async_trait]
impl StanzaStore for PostgresStore {
    async fn init(&self) -> Result<(), StoreError> {
        for statement in MAM_SCHEMA_STATEMENTS {
            sqlx::query(statement)
                .execute(&self.pool)
                .await
                .map_err(StoreError::backend)?;
        }
        Ok(())
    }

    async fn store_message(&self, m: &ArchivedMessage) -> Result<(), StoreError> {
        sqlx::query(
            r#"INSERT INTO mam_messages
               (id, room_jid, timestamp, from_jid, to_jid, body, stanza_id,
                thread_id, reply_to_id, reply_to_jid, origin_id, message_type, stanza_xml)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)"#,
        )
        .bind(&m.id)
        .bind(&m.room_jid)
        .bind(m.timestamp)
        .bind(&m.from)
        .bind(&m.to)
        .bind(&m.body)
        .bind(&m.stanza_id)
        .bind(&m.thread_id)
        .bind(&m.reply_to_id)
        .bind(&m.reply_to_jid)
        .bind(&m.origin_id)
        .bind(&m.message_type)
        .bind(&m.stanza_xml)
        .execute(&self.pool)
        .await
        .map_err(StoreError::backend)?;
        Ok(())
    }

    async fn query_messages(&self, q: &MamQuery) -> Result<Vec<ArchivedMessage>, StoreError> {
        let mut qb = QueryBuilder::new(
            "SELECT id, room_jid, timestamp, from_jid, to_jid, body, stanza_id, thread_id, \
             reply_to_id, reply_to_jid, origin_id, message_type, stanza_xml \
             FROM mam_messages WHERE room_jid = ",
        );
        qb.push_bind(&q.room_jid);
        if let Some(start) = q.start {
            qb.push(" AND timestamp >= ");
            qb.push_bind(start);
        }
        if let Some(end) = q.end {
            qb.push(" AND timestamp <= ");
            qb.push_bind(end);
        }
        if let Some(from) = &q.from_jid {
            qb.push(" AND from_jid = ");
            qb.push_bind(from);
        }
        if let Some(before) = &q.before_id {
            qb.push(" AND id < ");
            qb.push_bind(before);
        }
        if let Some(after) = &q.after_id {
            qb.push(" AND id > ");
            qb.push_bind(after);
        }
        qb.push(" ORDER BY timestamp DESC LIMIT ");
        qb.push_bind(i64::from(q.limit.max(1)));

        let rows: Vec<PgArchivedMessageRow> = qb
            .build_query_as()
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::backend)?;
        Ok(rows.into_iter().map(ArchivedMessage::from).collect())
    }

    async fn count_messages(&self, room_jid: &str) -> Result<u64, StoreError> {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM mam_messages WHERE room_jid = $1")
                .bind(room_jid)
                .fetch_one(&self.pool)
                .await
                .map_err(StoreError::backend)?;
        Ok(count as u64)
    }

    async fn db_size_bytes(&self) -> Result<u64, StoreError> {
        let bytes: i64 = sqlx::query_scalar("SELECT pg_database_size(current_database())")
            .fetch_one(&self.pool)
            .await
            .map_err(StoreError::backend)?;
        Ok(bytes as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use testcontainers::runners::AsyncRunner;
    use testcontainers_modules::postgres::Postgres;

    #[tokio::test]
    async fn insert_and_query_roundtrip_postgres() {
        let container = Postgres::default().start().await.unwrap();
        let db_url = format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            container.get_host_port_ipv4(5432).await.unwrap()
        );

        let store = PostgresStore::connect(&db_url, 16).await.unwrap();
        store.init().await.unwrap();

        for i in 0..1_000 {
            let mut m = ArchivedMessage::new_chat(
                "room1@conference.bench.local",
                &format!("user{i}@bench.local/c"),
                "room1@conference.bench.local",
                &format!("body {i}"),
            );
            m.message_type = "groupchat".into();
            store.store_message(&m).await.unwrap();
        }

        let count = store
            .count_messages("room1@conference.bench.local")
            .await
            .unwrap();
        assert_eq!(count, 1_000);

        let rows = store
            .query_messages(&MamQuery {
                room_jid: "room1@conference.bench.local".into(),
                limit: 50,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(rows.len(), 50);
    }
}
