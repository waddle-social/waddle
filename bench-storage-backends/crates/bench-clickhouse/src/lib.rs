//! ClickHouse backend for the stanza benchmark.

use std::sync::Arc;

use async_trait::async_trait;
use bench_core::message::{ArchivedMessage, MamQuery};
use bench_core::store::{StanzaStore, StoreError};
use clickhouse::Row;
use serde::Deserialize;

pub struct ClickHouseStore {
    client: clickhouse::Client,
}

impl ClickHouseStore {
    pub fn connect(url: &str, database: &str, user: &str, password: &str) -> Arc<Self> {
        let client = clickhouse::Client::default()
            .with_url(url)
            .with_database(database)
            .with_user(user)
            .with_password(password);
        Arc::new(Self { client })
    }
}

#[derive(Debug, Row, Deserialize)]
struct ClickHouseMessageRow {
    id: String,
    room_jid: String,
    timestamp_ms: i64,
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

#[derive(Debug, Row, Deserialize)]
struct CountRow {
    count: u64,
}

#[derive(Debug, Row, Deserialize)]
struct BytesRow {
    bytes: Option<u64>,
}

impl From<ClickHouseMessageRow> for ArchivedMessage {
    fn from(value: ClickHouseMessageRow) -> Self {
        let timestamp = chrono::DateTime::from_timestamp_millis(value.timestamp_ms)
            .unwrap_or_else(|| chrono::DateTime::from_timestamp(0, 0).unwrap())
            .with_timezone(&chrono::Utc);
        Self {
            id: value.id,
            room_jid: value.room_jid,
            timestamp,
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
impl StanzaStore for ClickHouseStore {
    async fn init(&self) -> Result<(), StoreError> {
        self.client
            .query(
                "CREATE TABLE IF NOT EXISTS mam_messages (
                    id String,
                    room_jid String,
                    timestamp_ms Int64,
                    from_jid String,
                    to_jid String,
                    body String,
                    stanza_id Nullable(String),
                    thread_id Nullable(String),
                    reply_to_id Nullable(String),
                    reply_to_jid Nullable(String),
                    origin_id Nullable(String),
                    message_type String,
                    stanza_xml Nullable(String),
                    created_at DateTime DEFAULT now()
                 ) ENGINE = MergeTree()
                 ORDER BY (room_jid, timestamp_ms, id)",
            )
            .execute()
            .await
            .map_err(StoreError::backend)?;
        Ok(())
    }

    async fn store_message(&self, m: &ArchivedMessage) -> Result<(), StoreError> {
        self.client
            .query(
                "INSERT INTO mam_messages
                (id, room_jid, timestamp_ms, from_jid, to_jid, body, stanza_id, thread_id,
                 reply_to_id, reply_to_jid, origin_id, message_type, stanza_xml)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&m.id)
            .bind(&m.room_jid)
            .bind(m.timestamp.timestamp_millis())
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
            .execute()
            .await
            .map_err(StoreError::backend)?;
        Ok(())
    }

    async fn query_messages(&self, q: &MamQuery) -> Result<Vec<ArchivedMessage>, StoreError> {
        let mut sql = String::from(
            "SELECT id, room_jid, timestamp_ms, from_jid, to_jid, body, stanza_id, thread_id, \
             reply_to_id, reply_to_jid, origin_id, message_type, stanza_xml \
             FROM mam_messages WHERE room_jid = ?",
        );
        if q.start.is_some() {
            sql.push_str(" AND timestamp_ms >= ?");
        }
        if q.end.is_some() {
            sql.push_str(" AND timestamp_ms <= ?");
        }
        if q.from_jid.is_some() {
            sql.push_str(" AND from_jid = ?");
        }
        if q.before_id.is_some() {
            sql.push_str(" AND id < ?");
        }
        if q.after_id.is_some() {
            sql.push_str(" AND id > ?");
        }
        sql.push_str(" ORDER BY timestamp_ms DESC LIMIT ?");

        let mut query = self.client.query(&sql).bind(&q.room_jid);
        if let Some(start) = q.start {
            query = query.bind(start.timestamp_millis());
        }
        if let Some(end) = q.end {
            query = query.bind(end.timestamp_millis());
        }
        if let Some(from) = &q.from_jid {
            query = query.bind(from);
        }
        if let Some(before) = &q.before_id {
            query = query.bind(before);
        }
        if let Some(after) = &q.after_id {
            query = query.bind(after);
        }
        query = query.bind(u64::from(q.limit.max(1)));

        let rows = query
            .fetch_all::<ClickHouseMessageRow>()
            .await
            .map_err(StoreError::backend)?;
        Ok(rows.into_iter().map(ArchivedMessage::from).collect())
    }

    async fn count_messages(&self, room_jid: &str) -> Result<u64, StoreError> {
        let row = self
            .client
            .query("SELECT count() AS count FROM mam_messages WHERE room_jid = ?")
            .bind(room_jid)
            .fetch_one::<CountRow>()
            .await
            .map_err(StoreError::backend)?;
        Ok(row.count)
    }

    async fn db_size_bytes(&self) -> Result<u64, StoreError> {
        let row = self
            .client
            .query(
                "SELECT sum(bytes) AS bytes
                 FROM system.parts
                 WHERE active
                   AND database = currentDatabase()
                   AND table = 'mam_messages'",
            )
            .fetch_one::<BytesRow>()
            .await
            .map_err(StoreError::backend)?;
        Ok(row.bytes.unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use testcontainers::runners::AsyncRunner;
    use testcontainers_modules::clickhouse::ClickHouse;

    #[tokio::test]
    async fn insert_and_query_roundtrip_clickhouse() {
        let container = ClickHouse::default().start().await.unwrap();
        let url = format!(
            "http://127.0.0.1:{}",
            container.get_host_port_ipv4(8123).await.unwrap()
        );

        let store = ClickHouseStore::connect(&url, "default", "default", "");
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
