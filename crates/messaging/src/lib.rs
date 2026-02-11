use std::sync::Arc;

use chrono::{DateTime, Utc};
use tracing::{debug, error, warn};
use uuid::Uuid;

use waddle_core::event::{ChatMessage, ChatState, Event, EventPayload, MessageType};
use waddle_storage::{Database, FromRow, Row, SqlValue, StorageError};
use waddle_xmpp::Stanza;

#[cfg(feature = "native")]
use waddle_core::event::{Channel, EventBus, EventSource};

#[derive(Debug, thiserror::Error)]
pub enum MessagingError {
    #[error("failed to send message: {0}")]
    SendFailed(String),

    #[error("storage error: {0}")]
    Storage(#[from] StorageError),

    #[error("event bus error: {0}")]
    EventBus(String),

    #[error("invalid JID: {0}")]
    InvalidJid(String),
}

struct StoredMessage {
    id: String,
    from_jid: String,
    to_jid: String,
    body: String,
    timestamp: String,
    message_type: String,
    thread: Option<String>,
}

impl FromRow for StoredMessage {
    fn from_row(row: &Row) -> Result<Self, StorageError> {
        let id = match row.get(0) {
            Some(SqlValue::Text(s)) => s.clone(),
            _ => return Err(StorageError::QueryFailed("missing id column".to_string())),
        };
        let from_jid = match row.get(1) {
            Some(SqlValue::Text(s)) => s.clone(),
            _ => {
                return Err(StorageError::QueryFailed(
                    "missing from_jid column".to_string(),
                ));
            }
        };
        let to_jid = match row.get(2) {
            Some(SqlValue::Text(s)) => s.clone(),
            _ => {
                return Err(StorageError::QueryFailed(
                    "missing to_jid column".to_string(),
                ));
            }
        };
        let body = match row.get(3) {
            Some(SqlValue::Text(s)) => s.clone(),
            _ => return Err(StorageError::QueryFailed("missing body column".to_string())),
        };
        let timestamp = match row.get(4) {
            Some(SqlValue::Text(s)) => s.clone(),
            _ => {
                return Err(StorageError::QueryFailed(
                    "missing timestamp column".to_string(),
                ));
            }
        };
        let message_type = match row.get(5) {
            Some(SqlValue::Text(s)) => s.clone(),
            _ => {
                return Err(StorageError::QueryFailed(
                    "missing message_type column".to_string(),
                ));
            }
        };
        let thread = match row.get(6) {
            Some(SqlValue::Text(s)) => Some(s.clone()),
            Some(SqlValue::Null) | None => None,
            _ => None,
        };
        Ok(StoredMessage {
            id,
            from_jid,
            to_jid,
            body,
            timestamp,
            message_type,
            thread,
        })
    }
}

impl StoredMessage {
    fn into_chat_message(self) -> ChatMessage {
        let message_type = match self.message_type.as_str() {
            "chat" => MessageType::Chat,
            "groupchat" => MessageType::Groupchat,
            "normal" => MessageType::Normal,
            "headline" => MessageType::Headline,
            "error" => MessageType::Error,
            _ => MessageType::Chat,
        };
        let timestamp = self
            .timestamp
            .parse::<DateTime<Utc>>()
            .unwrap_or_else(|_| Utc::now());
        ChatMessage {
            id: self.id,
            from: self.from_jid,
            to: self.to_jid,
            body: self.body,
            timestamp,
            message_type,
            thread: self.thread,
        }
    }
}

fn message_type_to_str(mt: &MessageType) -> &'static str {
    match mt {
        MessageType::Chat => "chat",
        MessageType::Groupchat => "groupchat",
        MessageType::Normal => "normal",
        MessageType::Headline => "headline",
        MessageType::Error => "error",
    }
}

pub struct MessageManager<D: Database> {
    db: Arc<D>,
    #[cfg(feature = "native")]
    event_bus: Arc<dyn EventBus>,
}

impl<D: Database> MessageManager<D> {
    #[cfg(feature = "native")]
    pub fn new(db: Arc<D>, event_bus: Arc<dyn EventBus>) -> Self {
        Self { db, event_bus }
    }

    pub async fn send_message(&self, to: &str, body: &str) -> Result<ChatMessage, MessagingError> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let message = ChatMessage {
            id: id.clone(),
            from: String::new(), // filled by outbound router with our JID
            to: to.to_string(),
            body: body.to_string(),
            timestamp: now,
            message_type: MessageType::Chat,
            thread: None,
        };

        self.persist_message(&message).await?;

        #[cfg(feature = "native")]
        {
            let _ = self.event_bus.publish(Event::new(
                Channel::new("ui.message.send").unwrap(),
                EventSource::System("messaging".into()),
                EventPayload::MessageSendRequested {
                    to: to.to_string(),
                    body: body.to_string(),
                    message_type: MessageType::Chat,
                },
            ));
        }

        Ok(message)
    }

    pub async fn send_chat_state(&self, to: &str, state: ChatState) -> Result<(), MessagingError> {
        #[cfg(feature = "native")]
        {
            let _ = self.event_bus.publish(Event::new(
                Channel::new("ui.chatstate.send").unwrap(),
                EventSource::System("messaging".into()),
                EventPayload::ChatStateSendRequested {
                    to: to.to_string(),
                    state,
                },
            ));
        }
        Ok(())
    }

    pub async fn get_messages(
        &self,
        jid: &str,
        limit: u32,
        before: Option<&str>,
    ) -> Result<Vec<ChatMessage>, MessagingError> {
        let jid_s = jid.to_string();
        let limit_i = i64::from(limit);

        let rows: Vec<StoredMessage> = if let Some(before_ts) = before {
            let before_s = before_ts.to_string();
            self.db
                .query(
                    "SELECT id, from_jid, to_jid, body, timestamp, message_type, thread \
                     FROM messages \
                     WHERE (from_jid = ?1 OR to_jid = ?1) AND message_type = 'chat' AND timestamp < ?2 \
                     ORDER BY timestamp DESC \
                     LIMIT ?3",
                    &[&jid_s, &before_s, &limit_i],
                )
                .await?
        } else {
            self.db
                .query(
                    "SELECT id, from_jid, to_jid, body, timestamp, message_type, thread \
                     FROM messages \
                     WHERE (from_jid = ?1 OR to_jid = ?1) AND message_type = 'chat' \
                     ORDER BY timestamp DESC \
                     LIMIT ?2",
                    &[&jid_s, &limit_i],
                )
                .await?
        };

        Ok(rows.into_iter().map(|r| r.into_chat_message()).collect())
    }

    pub async fn mark_read(&self, jid: &str) -> Result<(), MessagingError> {
        let jid_s = jid.to_string();
        let read_val = 1_i64;
        self.db
            .execute(
                "UPDATE messages SET read = ?1 WHERE from_jid = ?2 AND read = 0",
                &[&read_val, &jid_s],
            )
            .await?;
        Ok(())
    }

    async fn persist_message(&self, message: &ChatMessage) -> Result<(), MessagingError> {
        let id = message.id.clone();
        let from = message.from.clone();
        let to = message.to.clone();
        let body = message.body.clone();
        let ts = message.timestamp.to_rfc3339();
        let mt = message_type_to_str(&message.message_type).to_string();
        let thread = message.thread.clone();
        let read = 0_i64;

        self.db
            .execute(
                "INSERT OR IGNORE INTO messages (id, from_jid, to_jid, body, timestamp, message_type, thread, read) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                &[&id, &from, &to, &body, &ts, &mt, &thread, &read],
            )
            .await?;
        Ok(())
    }

    #[cfg(feature = "native")]
    pub async fn handle_event(&self, event: &Event) {
        match &event.payload {
            EventPayload::MessageReceived { message } => {
                debug!(
                    id = %message.id,
                    from = %message.from,
                    "message received, persisting"
                );
                if let Err(e) = self.persist_message(message).await {
                    error!(error = %e, "failed to persist received message");
                }
            }
            EventPayload::MessageSent { message } => {
                debug!(
                    id = %message.id,
                    to = %message.to,
                    "message sent, persisting"
                );
                if let Err(e) = self.persist_message(message).await {
                    error!(error = %e, "failed to persist sent message");
                }
            }
            EventPayload::MessageDelivered { id, to } => {
                debug!(id = %id, to = %to, "delivery receipt received");
            }
            EventPayload::ChatStateReceived { from, state } => {
                debug!(from = %from, ?state, "chat state received");
            }
            _ => {}
        }
    }

    #[cfg(feature = "native")]
    pub async fn run(self: Arc<Self>) -> Result<(), MessagingError> {
        let mut sub = self
            .event_bus
            .subscribe("{system,xmpp}.**")
            .map_err(|e| MessagingError::EventBus(e.to_string()))?;

        loop {
            match sub.recv().await {
                Ok(event) => {
                    self.handle_event(&event).await;
                }
                Err(waddle_core::error::EventBusError::ChannelClosed) => {
                    debug!("event bus closed, message manager stopping");
                    return Ok(());
                }
                Err(waddle_core::error::EventBusError::Lagged(count)) => {
                    warn!(count, "message manager lagged, some events dropped");
                }
                Err(e) => {
                    error!(error = %e, "message manager subscription error");
                    return Err(MessagingError::EventBus(e.to_string()));
                }
            }
        }
    }

    pub fn handle_stanza(&self, _stanza: &Stanza) {}
}

#[derive(Debug)]
pub struct MucManager<D>
where
    D: Database,
{
    _database: std::marker::PhantomData<D>,
}

impl<D> Default for MucManager<D>
where
    D: Database,
{
    fn default() -> Self {
        Self {
            _database: std::marker::PhantomData,
        }
    }
}

impl<D> MucManager<D>
where
    D: Database,
{
    pub fn new() -> Self {
        Self::default()
    }

    pub fn handle_event(&self, _event: &Event) {}

    pub fn handle_stanza(&self, _stanza: &Stanza) {}
}

#[cfg(all(test, feature = "native"))]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::TempDir;
    use waddle_core::event::{BroadcastEventBus, EventBus};

    async fn setup() -> (
        Arc<MessageManager<impl Database>>,
        Arc<dyn EventBus>,
        TempDir,
    ) {
        let dir = TempDir::new().expect("failed to create temp dir");
        let db_path = dir.path().join("test.db");
        let db = waddle_storage::open_database(&db_path)
            .await
            .expect("failed to open database");
        let db = Arc::new(db);
        let event_bus: Arc<dyn EventBus> = Arc::new(BroadcastEventBus::default());
        let manager = Arc::new(MessageManager::new(db, event_bus.clone()));
        (manager, event_bus, dir)
    }

    fn make_event(channel: &str, payload: EventPayload) -> Event {
        Event::new(
            Channel::new(channel).unwrap(),
            EventSource::System("test".into()),
            payload,
        )
    }

    fn make_chat_message(id: &str, from: &str, to: &str, body: &str) -> ChatMessage {
        ChatMessage {
            id: id.to_string(),
            from: from.to_string(),
            to: to.to_string(),
            body: body.to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Chat,
            thread: None,
        }
    }

    #[tokio::test]
    async fn get_messages_empty() {
        let (manager, _, _dir) = setup().await;
        let messages = manager
            .get_messages("alice@example.com", 50, None)
            .await
            .unwrap();
        assert!(messages.is_empty());
    }

    #[tokio::test]
    async fn send_message_persists_and_emits_event() {
        let (manager, event_bus, _dir) = setup().await;
        let mut sub = event_bus.subscribe("ui.**").unwrap();

        let msg = manager
            .send_message("bob@example.com", "Hello Bob!")
            .await
            .unwrap();

        assert_eq!(msg.to, "bob@example.com");
        assert_eq!(msg.body, "Hello Bob!");
        assert!(!msg.id.is_empty());

        let received = tokio::time::timeout(std::time::Duration::from_millis(100), sub.recv())
            .await
            .expect("timed out")
            .expect("should receive event");

        assert!(matches!(
            received.payload,
            EventPayload::MessageSendRequested {
                ref to,
                ref body,
                ..
            } if to == "bob@example.com" && body == "Hello Bob!"
        ));
    }

    #[tokio::test]
    async fn send_message_then_retrieve() {
        let (manager, _, _dir) = setup().await;

        manager
            .send_message("bob@example.com", "Hello!")
            .await
            .unwrap();

        let messages = manager
            .get_messages("bob@example.com", 50, None)
            .await
            .unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].body, "Hello!");
        assert_eq!(messages[0].to, "bob@example.com");
    }

    #[tokio::test]
    async fn handle_message_received_persists() {
        let (manager, _, _dir) = setup().await;
        let msg = make_chat_message("msg-1", "alice@example.com", "me@example.com", "Hi there!");

        let event = make_event(
            "xmpp.message.received",
            EventPayload::MessageReceived {
                message: msg.clone(),
            },
        );
        manager.handle_event(&event).await;

        let messages = manager
            .get_messages("alice@example.com", 50, None)
            .await
            .unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id, "msg-1");
        assert_eq!(messages[0].body, "Hi there!");
        assert_eq!(messages[0].from, "alice@example.com");
    }

    #[tokio::test]
    async fn handle_message_sent_persists() {
        let (manager, _, _dir) = setup().await;
        let msg = make_chat_message("msg-2", "me@example.com", "bob@example.com", "Hey Bob");

        let event = make_event(
            "xmpp.message.sent",
            EventPayload::MessageSent {
                message: msg.clone(),
            },
        );
        manager.handle_event(&event).await;

        let messages = manager
            .get_messages("bob@example.com", 50, None)
            .await
            .unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id, "msg-2");
        assert_eq!(messages[0].body, "Hey Bob");
    }

    #[tokio::test]
    async fn duplicate_message_not_inserted_twice() {
        let (manager, _, _dir) = setup().await;
        let msg = make_chat_message("msg-dup", "alice@example.com", "me@example.com", "Hello");

        manager.persist_message(&msg).await.unwrap();
        manager.persist_message(&msg).await.unwrap();

        let messages = manager
            .get_messages("alice@example.com", 50, None)
            .await
            .unwrap();

        assert_eq!(messages.len(), 1);
    }

    #[tokio::test]
    async fn get_messages_with_limit() {
        let (manager, _, _dir) = setup().await;

        for i in 0..5 {
            let msg = ChatMessage {
                id: format!("msg-{i}"),
                from: "alice@example.com".to_string(),
                to: "me@example.com".to_string(),
                body: format!("Message {i}"),
                timestamp: Utc::now() + chrono::Duration::seconds(i),
                message_type: MessageType::Chat,
                thread: None,
            };
            manager.persist_message(&msg).await.unwrap();
        }

        let messages = manager
            .get_messages("alice@example.com", 3, None)
            .await
            .unwrap();

        assert_eq!(messages.len(), 3);
    }

    #[tokio::test]
    async fn get_messages_with_before_pagination() {
        let (manager, _, _dir) = setup().await;

        let base = Utc::now();
        for i in 0..5 {
            let msg = ChatMessage {
                id: format!("msg-{i}"),
                from: "alice@example.com".to_string(),
                to: "me@example.com".to_string(),
                body: format!("Message {i}"),
                timestamp: base + chrono::Duration::seconds(i),
                message_type: MessageType::Chat,
                thread: None,
            };
            manager.persist_message(&msg).await.unwrap();
        }

        let cutoff = (base + chrono::Duration::seconds(3)).to_rfc3339();
        let messages = manager
            .get_messages("alice@example.com", 50, Some(&cutoff))
            .await
            .unwrap();

        assert_eq!(messages.len(), 3);
    }

    #[tokio::test]
    async fn mark_read_updates_messages() {
        let (manager, _, _dir) = setup().await;

        let msg = make_chat_message("msg-r", "alice@example.com", "me@example.com", "Read me");
        manager.persist_message(&msg).await.unwrap();

        manager.mark_read("alice@example.com").await.unwrap();

        // Verify read flag was updated by querying raw
        let rows: Vec<Row> = manager
            .db
            .query(
                "SELECT read FROM messages WHERE from_jid = ?1",
                &[&"alice@example.com".to_string()],
            )
            .await
            .unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get(0), Some(&SqlValue::Integer(1)));
    }

    #[tokio::test]
    async fn send_chat_state_emits_event() {
        let (manager, event_bus, _dir) = setup().await;
        let mut sub = event_bus.subscribe("ui.**").unwrap();

        manager
            .send_chat_state("bob@example.com", ChatState::Composing)
            .await
            .unwrap();

        let received = tokio::time::timeout(std::time::Duration::from_millis(100), sub.recv())
            .await
            .expect("timed out")
            .expect("should receive event");

        assert!(matches!(
            received.payload,
            EventPayload::ChatStateSendRequested {
                ref to,
                state: ChatState::Composing,
            } if to == "bob@example.com"
        ));
    }

    #[tokio::test]
    async fn handle_delivery_receipt_does_not_error() {
        let (manager, _, _dir) = setup().await;

        let event = make_event(
            "xmpp.message.delivered",
            EventPayload::MessageDelivered {
                id: "msg-1".to_string(),
                to: "bob@example.com".to_string(),
            },
        );
        manager.handle_event(&event).await;
    }

    #[tokio::test]
    async fn handle_chat_state_received_does_not_error() {
        let (manager, _, _dir) = setup().await;

        let event = make_event(
            "xmpp.chatstate.received",
            EventPayload::ChatStateReceived {
                from: "alice@example.com".to_string(),
                state: ChatState::Composing,
            },
        );
        manager.handle_event(&event).await;
    }

    #[tokio::test]
    async fn messages_with_thread_round_trip() {
        let (manager, _, _dir) = setup().await;

        let msg = ChatMessage {
            id: "msg-thread".to_string(),
            from: "alice@example.com".to_string(),
            to: "me@example.com".to_string(),
            body: "threaded message".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Chat,
            thread: Some("thread-123".to_string()),
        };
        manager.persist_message(&msg).await.unwrap();

        let messages = manager
            .get_messages("alice@example.com", 50, None)
            .await
            .unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].thread, Some("thread-123".to_string()));
    }

    #[tokio::test]
    async fn groupchat_messages_excluded_from_get_messages() {
        let (manager, _, _dir) = setup().await;

        let chat_msg = ChatMessage {
            id: "msg-chat".to_string(),
            from: "alice@example.com".to_string(),
            to: "me@example.com".to_string(),
            body: "chat message".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Chat,
            thread: None,
        };
        manager.persist_message(&chat_msg).await.unwrap();

        let gc_msg = ChatMessage {
            id: "msg-gc".to_string(),
            from: "room@muc.example.com".to_string(),
            to: "me@example.com".to_string(),
            body: "group message".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Groupchat,
            thread: None,
        };
        manager.persist_message(&gc_msg).await.unwrap();

        let messages = manager
            .get_messages("alice@example.com", 50, None)
            .await
            .unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id, "msg-chat");
    }

    #[tokio::test]
    async fn run_loop_processes_events() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (manager, event_bus, _dir) = setup().await;

                let manager_clone = manager.clone();
                let handle = tokio::task::spawn_local(async move { manager_clone.run().await });

                tokio::task::yield_now().await;
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;

                let msg = make_chat_message(
                    "msg-run",
                    "alice@example.com",
                    "me@example.com",
                    "via run loop",
                );

                event_bus
                    .publish(Event::new(
                        Channel::new("xmpp.message.received").unwrap(),
                        EventSource::Xmpp,
                        EventPayload::MessageReceived { message: msg },
                    ))
                    .unwrap();

                tokio::time::sleep(std::time::Duration::from_millis(50)).await;

                let messages = manager
                    .get_messages("alice@example.com", 50, None)
                    .await
                    .unwrap();

                assert_eq!(messages.len(), 1);
                assert_eq!(messages[0].body, "via run loop");

                handle.abort();
            })
            .await;
    }

    #[tokio::test]
    async fn bidirectional_conversation_retrieval() {
        let (manager, _, _dir) = setup().await;

        let msg1 = make_chat_message(
            "msg-from-alice",
            "alice@example.com",
            "me@example.com",
            "Hello from Alice",
        );
        manager.persist_message(&msg1).await.unwrap();

        let msg2 = make_chat_message(
            "msg-to-alice",
            "me@example.com",
            "alice@example.com",
            "Hello back",
        );
        manager.persist_message(&msg2).await.unwrap();

        let messages = manager
            .get_messages("alice@example.com", 50, None)
            .await
            .unwrap();

        assert_eq!(messages.len(), 2);
    }
}
