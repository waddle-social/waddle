use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
#[cfg(feature = "native")]
use serde::{Deserialize, Serialize};
use std::sync::RwLock;
use tracing::{debug, error, warn};
use uuid::Uuid;

use waddle_core::event::{
    ChatMessage, ChatState, Event, EventPayload, MessageType, MucOccupant, MucRole,
};
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

#[cfg(feature = "native")]
const OFFLINE_STATUS_PENDING: &str = "pending";
#[cfg(feature = "native")]
const OFFLINE_STATUS_SENT: &str = "sent";
#[cfg(feature = "native")]
const OFFLINE_STATUS_CONFIRMED: &str = "confirmed";
#[cfg(feature = "native")]
const OFFLINE_STATUS_FAILED: &str = "failed";
#[cfg(feature = "native")]
const OFFLINE_SOURCE: &str = "offline";

#[cfg(feature = "native")]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct QueuedOutboundEvent {
    channel: String,
    payload: EventPayload,
    correlation_id: Option<Uuid>,
}

#[cfg(feature = "native")]
struct StoredOfflineQueueItem {
    id: i64,
    stanza_type: String,
    payload: String,
    status: String,
}

#[cfg(feature = "native")]
impl FromRow for StoredOfflineQueueItem {
    fn from_row(row: &Row) -> Result<Self, StorageError> {
        let id = match row.get(0) {
            Some(SqlValue::Integer(v)) => *v,
            _ => return Err(StorageError::QueryFailed("missing id column".to_string())),
        };
        let stanza_type = match row.get(1) {
            Some(SqlValue::Text(v)) => v.clone(),
            _ => {
                return Err(StorageError::QueryFailed(
                    "missing stanza_type column".to_string(),
                ));
            }
        };
        let payload = match row.get(2) {
            Some(SqlValue::Text(v)) => v.clone(),
            _ => {
                return Err(StorageError::QueryFailed(
                    "missing payload column".to_string(),
                ));
            }
        };
        let status = match row.get(3) {
            Some(SqlValue::Text(v)) => v.clone(),
            _ => {
                return Err(StorageError::QueryFailed(
                    "missing status column".to_string(),
                ));
            }
        };
        Ok(Self {
            id,
            stanza_type,
            payload,
            status,
        })
    }
}

#[cfg(feature = "native")]
fn command_stanza_type(payload: &EventPayload) -> Option<&'static str> {
    match payload {
        EventPayload::MessageSendRequested { .. }
        | EventPayload::MucSendRequested { .. }
        | EventPayload::ChatStateSendRequested { .. } => Some("message"),
        EventPayload::PresenceSetRequested { .. }
        | EventPayload::SubscriptionRespondRequested { .. }
        | EventPayload::SubscriptionSendRequested { .. }
        | EventPayload::MucJoinRequested { .. }
        | EventPayload::MucLeaveRequested { .. } => Some("presence"),
        EventPayload::RosterAddRequested { .. }
        | EventPayload::RosterUpdateRequested { .. }
        | EventPayload::RosterRemoveRequested { .. }
        | EventPayload::RosterFetchRequested => Some("iq"),
        _ => None,
    }
}

pub struct MessageManager<D: Database> {
    db: Arc<D>,
    #[cfg(feature = "native")]
    event_bus: Arc<dyn EventBus>,
    #[cfg(feature = "native")]
    is_online: RwLock<bool>,
}

impl<D: Database> MessageManager<D> {
    #[cfg(feature = "native")]
    pub fn new(db: Arc<D>, event_bus: Arc<dyn EventBus>) -> Self {
        Self {
            db,
            event_bus,
            is_online: RwLock::new(false),
        }
    }

    pub async fn send_message(&self, to: &str, body: &str) -> Result<ChatMessage, MessagingError> {
        let id = Uuid::new_v4();
        let now = Utc::now();
        let message = ChatMessage {
            id: id.to_string(),
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
            let payload = EventPayload::MessageSendRequested {
                to: to.to_string(),
                body: body.to_string(),
                message_type: MessageType::Chat,
            };

            if self.is_online() {
                let _ = self.event_bus.publish(Event::with_correlation(
                    Channel::new("ui.message.send").unwrap(),
                    EventSource::System("messaging".into()),
                    payload,
                    id,
                ));
            } else {
                self.enqueue_command_event("ui.message.send", payload, Some(id))
                    .await?;
            }
        }

        Ok(message)
    }

    pub async fn send_chat_state(&self, to: &str, state: ChatState) -> Result<(), MessagingError> {
        #[cfg(feature = "native")]
        {
            let payload = EventPayload::ChatStateSendRequested {
                to: to.to_string(),
                state,
            };

            if self.is_online() {
                let _ = self.event_bus.publish(Event::new(
                    Channel::new("ui.chatstate.send").unwrap(),
                    EventSource::System("messaging".into()),
                    payload,
                ));
            } else {
                self.enqueue_command_event("ui.chatstate.send", payload, None)
                    .await?;
            }
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
    fn is_online(&self) -> bool {
        *self.is_online.read().unwrap()
    }

    #[cfg(feature = "native")]
    fn set_online(&self, online: bool) -> bool {
        let mut state = self.is_online.write().unwrap();
        let previous = *state;
        *state = online;
        previous
    }

    #[cfg(feature = "native")]
    async fn enqueue_command_event(
        &self,
        channel: &str,
        payload: EventPayload,
        correlation_id: Option<Uuid>,
    ) -> Result<(), MessagingError> {
        let Some(stanza_type) = command_stanza_type(&payload) else {
            return Ok(());
        };

        let resolved_correlation = if matches!(&payload, EventPayload::MessageSendRequested { .. })
            && correlation_id.is_none()
        {
            Some(Uuid::new_v4())
        } else {
            correlation_id
        };

        if let EventPayload::MessageSendRequested {
            to,
            body,
            message_type,
        } = &payload
        {
            let message = ChatMessage {
                id: resolved_correlation
                    .unwrap_or_else(Uuid::new_v4)
                    .to_string(),
                from: String::new(),
                to: to.clone(),
                body: body.clone(),
                timestamp: Utc::now(),
                message_type: message_type.clone(),
                thread: None,
            };
            self.persist_message(&message).await?;
        }

        let queued = QueuedOutboundEvent {
            channel: channel.to_string(),
            payload,
            correlation_id: resolved_correlation,
        };
        let payload_json = serde_json::to_string(&queued)
            .map_err(|e| MessagingError::SendFailed(format!("queue serialization failed: {e}")))?;
        let created_at = Utc::now().to_rfc3339();
        let status = OFFLINE_STATUS_PENDING.to_string();
        let stanza_type_s = stanza_type.to_string();

        self.db
            .execute(
                "INSERT INTO offline_queue (stanza_type, payload, created_at, status) \
                 VALUES (?1, ?2, ?3, ?4)",
                &[&stanza_type_s, &payload_json, &created_at, &status],
            )
            .await?;

        Ok(())
    }

    #[cfg(feature = "native")]
    async fn load_offline_queue_by_status(
        &self,
        status: &str,
    ) -> Result<Vec<StoredOfflineQueueItem>, MessagingError> {
        let status_s = status.to_string();
        self.db
            .query(
                "SELECT id, stanza_type, payload, status \
                 FROM offline_queue \
                 WHERE status = ?1 \
                 ORDER BY id ASC",
                &[&status_s],
            )
            .await
            .map_err(Into::into)
    }

    #[cfg(feature = "native")]
    async fn load_message_queue_candidates(
        &self,
    ) -> Result<Vec<StoredOfflineQueueItem>, MessagingError> {
        self.db
            .query(
                "SELECT id, stanza_type, payload, status \
                 FROM offline_queue \
                 WHERE stanza_type = 'message' AND status != 'confirmed' \
                 ORDER BY id ASC",
                &[],
            )
            .await
            .map_err(Into::into)
    }

    #[cfg(feature = "native")]
    async fn update_queue_status(&self, id: i64, status: &str) -> Result<(), MessagingError> {
        let status_s = status.to_string();
        self.db
            .execute(
                "UPDATE offline_queue SET status = ?1 WHERE id = ?2",
                &[&status_s, &id],
            )
            .await?;
        Ok(())
    }

    #[cfg(feature = "native")]
    async fn drain_offline_queue(&self) -> Result<(), MessagingError> {
        let pending_items = self
            .load_offline_queue_by_status(OFFLINE_STATUS_PENDING)
            .await?;

        for item in pending_items {
            let queued: QueuedOutboundEvent = match serde_json::from_str(&item.payload) {
                Ok(parsed) => parsed,
                Err(error) => {
                    error!(
                        queue_id = item.id,
                        error = %error,
                        "failed to deserialize offline queue item"
                    );
                    let _ = self
                        .update_queue_status(item.id, OFFLINE_STATUS_FAILED)
                        .await;
                    continue;
                }
            };

            let channel = match Channel::new(&queued.channel) {
                Ok(channel) => channel,
                Err(error) => {
                    error!(
                        queue_id = item.id,
                        channel = %queued.channel,
                        error = %error,
                        "invalid queued channel"
                    );
                    let _ = self
                        .update_queue_status(item.id, OFFLINE_STATUS_FAILED)
                        .await;
                    continue;
                }
            };

            let source = EventSource::System(OFFLINE_SOURCE.to_string());
            let event = if let Some(correlation_id) = queued.correlation_id {
                Event::with_correlation(channel, source, queued.payload, correlation_id)
            } else {
                Event::new(channel, source, queued.payload)
            };

            if let Err(error) = self.event_bus.publish(event) {
                error!(
                    queue_id = item.id,
                    error = %error,
                    "failed to publish queued offline command"
                );
                let _ = self
                    .update_queue_status(item.id, OFFLINE_STATUS_FAILED)
                    .await;
                continue;
            }

            if item.stanza_type != "message" {
                if let Err(error) = self.update_queue_status(item.id, OFFLINE_STATUS_SENT).await {
                    error!(
                        queue_id = item.id,
                        error = %error,
                        "failed to update queued command status to sent"
                    );
                } else if let Err(error) = self
                    .update_queue_status(item.id, OFFLINE_STATUS_CONFIRMED)
                    .await
                {
                    error!(
                        queue_id = item.id,
                        error = %error,
                        "failed to update queued command status to confirmed"
                    );
                }
            }
        }

        Ok(())
    }

    #[cfg(feature = "native")]
    async fn update_message_queue_status_by_id(
        &self,
        message_id: &str,
        from_statuses: &[&str],
        to_status: &str,
    ) -> Result<bool, MessagingError> {
        let candidates = self.load_message_queue_candidates().await?;

        for item in candidates {
            if !from_statuses.contains(&item.status.as_str()) {
                continue;
            }

            let Ok(queued) = serde_json::from_str::<QueuedOutboundEvent>(&item.payload) else {
                continue;
            };

            let queued_id = queued.correlation_id.map(|id| id.to_string());
            if queued_id.as_deref() == Some(message_id) {
                self.update_queue_status(item.id, to_status).await?;
                return Ok(true);
            }
        }

        Ok(false)
    }

    #[cfg(feature = "native")]
    async fn update_message_queue_status_by_content(
        &self,
        to: &str,
        body: &str,
        from_statuses: &[&str],
        to_status: &str,
    ) -> Result<bool, MessagingError> {
        let candidates = self.load_message_queue_candidates().await?;

        for item in candidates {
            if !from_statuses.contains(&item.status.as_str()) {
                continue;
            }

            let Ok(queued) = serde_json::from_str::<QueuedOutboundEvent>(&item.payload) else {
                continue;
            };

            if let EventPayload::MessageSendRequested {
                to: queued_to,
                body: queued_body,
                ..
            } = queued.payload
            {
                if queued_to == to && queued_body == body {
                    self.update_queue_status(item.id, to_status).await?;
                    return Ok(true);
                }
            }
        }

        Ok(false)
    }

    #[cfg(feature = "native")]
    fn emit_system_transition(&self, channel: &str, payload: EventPayload) {
        let Ok(channel) = Channel::new(channel) else {
            return;
        };
        let _ = self.event_bus.publish(Event::new(
            channel,
            EventSource::System(OFFLINE_SOURCE.to_string()),
            payload,
        ));
    }

    #[cfg(feature = "native")]
    pub async fn handle_event(&self, event: &Event) {
        match &event.payload {
            EventPayload::ConnectionEstablished { .. } => {
                let was_online = self.set_online(true);
                if !was_online {
                    self.emit_system_transition("system.coming_online", EventPayload::ComingOnline);
                }
                if let Err(error) = self.drain_offline_queue().await {
                    error!(error = %error, "failed to drain offline queue");
                }
            }
            EventPayload::ConnectionLost { .. } => {
                let was_online = self.set_online(false);
                if was_online {
                    self.emit_system_transition("system.going_offline", EventPayload::GoingOffline);
                }
            }
            EventPayload::MessageSendRequested { .. }
            | EventPayload::PresenceSetRequested { .. }
            | EventPayload::RosterAddRequested { .. }
            | EventPayload::RosterUpdateRequested { .. }
            | EventPayload::RosterRemoveRequested { .. }
            | EventPayload::RosterFetchRequested
            | EventPayload::SubscriptionRespondRequested { .. }
            | EventPayload::SubscriptionSendRequested { .. }
            | EventPayload::MucJoinRequested { .. }
            | EventPayload::MucLeaveRequested { .. }
            | EventPayload::MucSendRequested { .. }
            | EventPayload::ChatStateSendRequested { .. } => {
                if self.is_online() {
                    return;
                }

                if matches!(event.source, EventSource::System(ref source) if source == OFFLINE_SOURCE)
                {
                    return;
                }

                if let Err(error) = self
                    .enqueue_command_event(
                        event.channel.as_str(),
                        event.payload.clone(),
                        event.correlation_id,
                    )
                    .await
                {
                    error!(error = %error, "failed to enqueue offline command event");
                }
            }
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
                if let Err(error) = self
                    .update_message_queue_status_by_id(
                        &message.id,
                        &[OFFLINE_STATUS_PENDING],
                        OFFLINE_STATUS_SENT,
                    )
                    .await
                {
                    error!(error = %error, "failed to update queued message to sent");
                }
            }
            EventPayload::MessageDelivered { id, to } => {
                debug!(id = %id, to = %to, "delivery receipt received");
                if let Err(error) = self
                    .update_message_queue_status_by_id(
                        id,
                        &[OFFLINE_STATUS_PENDING, OFFLINE_STATUS_SENT],
                        OFFLINE_STATUS_CONFIRMED,
                    )
                    .await
                {
                    error!(error = %error, "failed to update queued message to confirmed");
                }
            }
            EventPayload::MamResultReceived { messages, .. } => {
                for message in messages {
                    let confirmed_by_id = match self
                        .update_message_queue_status_by_id(
                            &message.id,
                            &[OFFLINE_STATUS_PENDING, OFFLINE_STATUS_SENT],
                            OFFLINE_STATUS_CONFIRMED,
                        )
                        .await
                    {
                        Ok(updated) => updated,
                        Err(error) => {
                            error!(
                                error = %error,
                                message_id = %message.id,
                                "failed to reconcile queued message by id"
                            );
                            false
                        }
                    };

                    if confirmed_by_id {
                        continue;
                    }

                    if let Err(error) = self
                        .update_message_queue_status_by_content(
                            &message.to,
                            &message.body,
                            &[OFFLINE_STATUS_SENT],
                            OFFLINE_STATUS_CONFIRMED,
                        )
                        .await
                    {
                        error!(
                            error = %error,
                            to = %message.to,
                            "failed to reconcile queued message with MAM content"
                        );
                    }
                }
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
            .subscribe("{system,xmpp,ui}.**")
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

#[derive(Debug, Clone)]
pub struct MucRoom {
    pub room_jid: String,
    pub nick: String,
    pub joined: bool,
    pub subject: Option<String>,
}

struct StoredRoom {
    room_jid: String,
    nick: String,
    joined: i64,
    subject: Option<String>,
}

impl FromRow for StoredRoom {
    fn from_row(row: &Row) -> Result<Self, StorageError> {
        let room_jid = match row.get(0) {
            Some(SqlValue::Text(s)) => s.clone(),
            _ => {
                return Err(StorageError::QueryFailed(
                    "missing room_jid column".to_string(),
                ));
            }
        };
        let nick = match row.get(1) {
            Some(SqlValue::Text(s)) => s.clone(),
            _ => return Err(StorageError::QueryFailed("missing nick column".to_string())),
        };
        let joined = match row.get(2) {
            Some(SqlValue::Integer(i)) => *i,
            _ => {
                return Err(StorageError::QueryFailed(
                    "missing joined column".to_string(),
                ));
            }
        };
        let subject = match row.get(3) {
            Some(SqlValue::Text(s)) => Some(s.clone()),
            Some(SqlValue::Null) | None => None,
            _ => None,
        };
        Ok(StoredRoom {
            room_jid,
            nick,
            joined,
            subject,
        })
    }
}

impl StoredRoom {
    fn into_muc_room(self) -> MucRoom {
        MucRoom {
            room_jid: self.room_jid,
            nick: self.nick,
            joined: self.joined != 0,
            subject: self.subject,
        }
    }
}

/// Per-room occupant map: nick -> MucOccupant
type OccupantMap = HashMap<String, MucOccupant>;

pub struct MucManager<D: Database> {
    db: Arc<D>,
    occupants: RwLock<HashMap<String, OccupantMap>>,
    #[cfg(feature = "native")]
    event_bus: Arc<dyn EventBus>,
}

impl<D: Database> MucManager<D> {
    #[cfg(feature = "native")]
    pub fn new(db: Arc<D>, event_bus: Arc<dyn EventBus>) -> Self {
        Self {
            db,
            occupants: RwLock::new(HashMap::new()),
            event_bus,
        }
    }

    pub async fn join_room(&self, room: &str, nick: &str) -> Result<(), MessagingError> {
        let room_s = room.to_string();
        let nick_s = nick.to_string();
        let joined = 0_i64;
        let subject: Option<String> = None;

        self.db
            .execute(
                "INSERT OR REPLACE INTO muc_rooms (room_jid, nick, joined, subject) \
                 VALUES (?1, ?2, ?3, ?4)",
                &[&room_s, &nick_s, &joined, &subject],
            )
            .await?;

        #[cfg(feature = "native")]
        {
            let _ = self.event_bus.publish(Event::new(
                Channel::new("ui.muc.join").unwrap(),
                EventSource::System("muc".into()),
                EventPayload::MucJoinRequested {
                    room: room.to_string(),
                    nick: nick.to_string(),
                },
            ));
        }

        Ok(())
    }

    pub async fn leave_room(&self, room: &str) -> Result<(), MessagingError> {
        #[cfg(feature = "native")]
        {
            let _ = self.event_bus.publish(Event::new(
                Channel::new("ui.muc.leave").unwrap(),
                EventSource::System("muc".into()),
                EventPayload::MucLeaveRequested {
                    room: room.to_string(),
                },
            ));
        }

        Ok(())
    }

    pub async fn send_message(&self, room: &str, body: &str) -> Result<(), MessagingError> {
        #[cfg(feature = "native")]
        {
            let _ = self.event_bus.publish(Event::new(
                Channel::new("ui.muc.send").unwrap(),
                EventSource::System("muc".into()),
                EventPayload::MucSendRequested {
                    room: room.to_string(),
                    body: body.to_string(),
                },
            ));
        }

        Ok(())
    }

    pub async fn get_rooms(&self) -> Result<Vec<MucRoom>, MessagingError> {
        let rows: Vec<StoredRoom> = self
            .db
            .query(
                "SELECT room_jid, nick, joined, subject FROM muc_rooms ORDER BY room_jid",
                &[],
            )
            .await?;

        Ok(rows.into_iter().map(|r| r.into_muc_room()).collect())
    }

    pub async fn get_joined_rooms(&self) -> Result<Vec<MucRoom>, MessagingError> {
        let joined = 1_i64;
        let rows: Vec<StoredRoom> = self
            .db
            .query(
                "SELECT room_jid, nick, joined, subject FROM muc_rooms \
                 WHERE joined = ?1 ORDER BY room_jid",
                &[&joined],
            )
            .await?;

        Ok(rows.into_iter().map(|r| r.into_muc_room()).collect())
    }

    pub async fn get_room_messages(
        &self,
        room: &str,
        limit: u32,
        before: Option<&str>,
    ) -> Result<Vec<ChatMessage>, MessagingError> {
        let room_s = room.to_string();
        let limit_i = i64::from(limit);

        let rows: Vec<StoredMessage> = if let Some(before_ts) = before {
            let before_s = before_ts.to_string();
            self.db
                .query(
                    "SELECT id, from_jid, to_jid, body, timestamp, message_type, thread \
                     FROM messages \
                     WHERE to_jid = ?1 AND message_type = 'groupchat' AND timestamp < ?2 \
                     ORDER BY timestamp DESC \
                     LIMIT ?3",
                    &[&room_s, &before_s, &limit_i],
                )
                .await?
        } else {
            self.db
                .query(
                    "SELECT id, from_jid, to_jid, body, timestamp, message_type, thread \
                     FROM messages \
                     WHERE to_jid = ?1 AND message_type = 'groupchat' \
                     ORDER BY timestamp DESC \
                     LIMIT ?2",
                    &[&room_s, &limit_i],
                )
                .await?
        };

        Ok(rows.into_iter().map(|r| r.into_chat_message()).collect())
    }

    pub fn get_occupants(&self, room: &str) -> Vec<MucOccupant> {
        let occupants = self.occupants.read().unwrap();
        match occupants.get(room) {
            Some(map) => map.values().cloned().collect(),
            None => Vec::new(),
        }
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

    async fn persist_room_message(
        &self,
        room: &str,
        message: &ChatMessage,
    ) -> Result<(), MessagingError> {
        let mut normalized = message.clone();
        normalized.to = room.to_string();
        normalized.message_type = MessageType::Groupchat;
        self.persist_message(&normalized).await
    }

    async fn mark_room_joined(&self, room: &str, nick: &str) -> Result<(), MessagingError> {
        let room_s = room.to_string();
        let nick_s = nick.to_string();
        let joined = 1_i64;
        let subject: Option<String> = None;

        self.db
            .execute(
                "INSERT OR REPLACE INTO muc_rooms (room_jid, nick, joined, subject) \
                 VALUES (?1, ?2, ?3, \
                 COALESCE((SELECT subject FROM muc_rooms WHERE room_jid = ?1), ?4))",
                &[&room_s, &nick_s, &joined, &subject],
            )
            .await?;
        Ok(())
    }

    async fn mark_room_left(&self, room: &str) -> Result<(), MessagingError> {
        let room_s = room.to_string();
        let joined = 0_i64;

        self.db
            .execute(
                "UPDATE muc_rooms SET joined = ?1 WHERE room_jid = ?2",
                &[&joined, &room_s],
            )
            .await?;

        self.occupants.write().unwrap().remove(room);
        Ok(())
    }

    async fn update_subject(&self, room: &str, subject: &str) -> Result<(), MessagingError> {
        let room_s = room.to_string();
        let subject_s = subject.to_string();

        self.db
            .execute(
                "UPDATE muc_rooms SET subject = ?1 WHERE room_jid = ?2",
                &[&subject_s, &room_s],
            )
            .await?;
        Ok(())
    }

    fn track_occupant(&self, room: &str, occupant: &MucOccupant) {
        let mut occupants = self.occupants.write().unwrap();
        let room_occupants = occupants.entry(room.to_string()).or_default();

        if matches!(occupant.role, MucRole::None) {
            room_occupants.remove(&occupant.nick);
        } else {
            room_occupants.insert(occupant.nick.clone(), occupant.clone());
        }
    }

    #[cfg(feature = "native")]
    pub async fn handle_event(&self, event: &Event) {
        match &event.payload {
            EventPayload::MucJoined { room, nick } => {
                debug!(room = %room, nick = %nick, "joined MUC room");
                if let Err(e) = self.mark_room_joined(room, nick).await {
                    error!(error = %e, room = %room, "failed to persist room join");
                }
            }
            EventPayload::MucLeft { room } => {
                debug!(room = %room, "left MUC room");
                if let Err(e) = self.mark_room_left(room).await {
                    error!(error = %e, room = %room, "failed to persist room leave");
                }
            }
            EventPayload::MucMessageReceived { room, message } => {
                debug!(
                    room = %room,
                    id = %message.id,
                    from = %message.from,
                    "MUC message received, persisting"
                );
                if let Err(e) = self.persist_room_message(room, message).await {
                    error!(error = %e, room = %room, "failed to persist MUC message");
                }
            }
            EventPayload::MucSubjectChanged { room, subject } => {
                debug!(room = %room, subject = %subject, "MUC subject changed");
                if let Err(e) = self.update_subject(room, subject).await {
                    error!(error = %e, room = %room, "failed to persist subject change");
                }
            }
            EventPayload::MucOccupantChanged { room, occupant } => {
                debug!(
                    room = %room,
                    nick = %occupant.nick,
                    "MUC occupant changed"
                );
                self.track_occupant(room, occupant);
            }
            _ => {}
        }
    }

    #[cfg(feature = "native")]
    pub async fn run(self: Arc<Self>) -> Result<(), MessagingError> {
        let mut sub = self
            .event_bus
            .subscribe("xmpp.muc.**")
            .map_err(|e| MessagingError::EventBus(e.to_string()))?;

        loop {
            match sub.recv().await {
                Ok(event) => {
                    self.handle_event(&event).await;
                }
                Err(waddle_core::error::EventBusError::ChannelClosed) => {
                    debug!("event bus closed, MUC manager stopping");
                    return Ok(());
                }
                Err(waddle_core::error::EventBusError::Lagged(count)) => {
                    warn!(count, "MUC manager lagged, some events dropped");
                }
                Err(e) => {
                    error!(error = %e, "MUC manager subscription error");
                    return Err(MessagingError::EventBus(e.to_string()));
                }
            }
        }
    }

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

    async fn set_connection_online<D: Database>(manager: &MessageManager<D>) {
        let event = make_event(
            "system.connection.established",
            EventPayload::ConnectionEstablished {
                jid: "alice@example.com".to_string(),
            },
        );
        manager.handle_event(&event).await;
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
        set_connection_online(manager.as_ref()).await;

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
        assert_eq!(
            received.correlation_id,
            Some(Uuid::parse_str(&msg.id).expect("message id should be a UUID"))
        );
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
        set_connection_online(manager.as_ref()).await;

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

    #[tokio::test]
    async fn send_message_offline_enqueues_without_emitting_ui_event() {
        let (manager, event_bus, _dir) = setup().await;
        let mut sub = event_bus.subscribe("ui.message.send").unwrap();

        let message = manager
            .send_message("bob@example.com", "queued while offline")
            .await
            .unwrap();

        let ui_event =
            tokio::time::timeout(std::time::Duration::from_millis(100), sub.recv()).await;
        assert!(
            ui_event.is_err(),
            "offline send should not publish ui.message.send"
        );

        let rows: Vec<Row> = manager
            .db
            .query(
                "SELECT stanza_type, status FROM offline_queue ORDER BY id ASC",
                &[],
            )
            .await
            .unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get(0), Some(&SqlValue::Text("message".to_string())));
        assert_eq!(rows[0].get(1), Some(&SqlValue::Text("pending".to_string())));

        let stored = manager
            .get_messages("bob@example.com", 50, None)
            .await
            .unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].id, message.id);
    }

    #[tokio::test]
    async fn reconnect_drains_offline_queue_fifo_and_marks_sent() {
        let (manager, event_bus, _dir) = setup().await;

        let first = manager
            .send_message("bob@example.com", "first queued")
            .await
            .unwrap();
        let second = manager
            .send_message("carol@example.com", "second queued")
            .await
            .unwrap();

        let mut sub = event_bus.subscribe("ui.message.send").unwrap();
        set_connection_online(manager.as_ref()).await;

        let first_event = tokio::time::timeout(std::time::Duration::from_millis(100), sub.recv())
            .await
            .expect("timed out waiting for first drained item")
            .expect("expected first drained item");
        let second_event = tokio::time::timeout(std::time::Duration::from_millis(100), sub.recv())
            .await
            .expect("timed out waiting for second drained item")
            .expect("expected second drained item");

        assert!(matches!(
            first_event.payload,
            EventPayload::MessageSendRequested { ref body, .. } if body == "first queued"
        ));
        assert!(matches!(
            second_event.payload,
            EventPayload::MessageSendRequested { ref body, .. } if body == "second queued"
        ));

        manager
            .handle_event(&make_event(
                "xmpp.message.sent",
                EventPayload::MessageSent {
                    message: make_chat_message(
                        &first.id,
                        "alice@example.com",
                        "bob@example.com",
                        "first queued",
                    ),
                },
            ))
            .await;

        manager
            .handle_event(&make_event(
                "xmpp.message.sent",
                EventPayload::MessageSent {
                    message: make_chat_message(
                        &second.id,
                        "alice@example.com",
                        "carol@example.com",
                        "second queued",
                    ),
                },
            ))
            .await;

        let rows: Vec<Row> = manager
            .db
            .query("SELECT status FROM offline_queue ORDER BY id ASC", &[])
            .await
            .unwrap();

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get(0), Some(&SqlValue::Text("sent".to_string())));
        assert_eq!(rows[1].get(0), Some(&SqlValue::Text("sent".to_string())));
    }

    #[tokio::test]
    async fn delivery_receipt_marks_queued_message_confirmed() {
        let (manager, _event_bus, _dir) = setup().await;

        let queued = manager
            .send_message("bob@example.com", "needs confirmation")
            .await
            .unwrap();
        set_connection_online(manager.as_ref()).await;

        manager
            .handle_event(&make_event(
                "xmpp.message.sent",
                EventPayload::MessageSent {
                    message: make_chat_message(
                        &queued.id,
                        "alice@example.com",
                        "bob@example.com",
                        "needs confirmation",
                    ),
                },
            ))
            .await;

        manager
            .handle_event(&make_event(
                "xmpp.message.delivered",
                EventPayload::MessageDelivered {
                    id: queued.id.clone(),
                    to: "bob@example.com".to_string(),
                },
            ))
            .await;

        let row: Row = manager
            .db
            .query_one(
                "SELECT status FROM offline_queue ORDER BY id ASC LIMIT 1",
                &[],
            )
            .await
            .unwrap();
        assert_eq!(row.get(0), Some(&SqlValue::Text("confirmed".to_string())));
    }

    #[tokio::test]
    async fn mam_result_reconciles_sent_queue_item_by_content() {
        let (manager, _event_bus, _dir) = setup().await;

        let queued = manager
            .send_message("bob@example.com", "reconcile me")
            .await
            .unwrap();
        set_connection_online(manager.as_ref()).await;

        manager
            .handle_event(&make_event(
                "xmpp.message.sent",
                EventPayload::MessageSent {
                    message: make_chat_message(
                        &queued.id,
                        "alice@example.com",
                        "bob@example.com",
                        "reconcile me",
                    ),
                },
            ))
            .await;

        let mam_message = ChatMessage {
            id: "archive-id-42".to_string(),
            from: "alice@example.com".to_string(),
            to: "bob@example.com".to_string(),
            body: "reconcile me".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Chat,
            thread: None,
        };

        manager
            .handle_event(&make_event(
                "xmpp.mam.result.received",
                EventPayload::MamResultReceived {
                    query_id: "q1".to_string(),
                    messages: vec![mam_message],
                    complete: true,
                },
            ))
            .await;

        let row: Row = manager
            .db
            .query_one(
                "SELECT status FROM offline_queue ORDER BY id ASC LIMIT 1",
                &[],
            )
            .await
            .unwrap();
        assert_eq!(row.get(0), Some(&SqlValue::Text("confirmed".to_string())));
    }
}

#[cfg(all(test, feature = "native"))]
mod muc_tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::TempDir;
    use waddle_core::event::{BroadcastEventBus, Channel, EventBus, EventSource, MucAffiliation};

    async fn setup_muc() -> (Arc<MucManager<impl Database>>, Arc<dyn EventBus>, TempDir) {
        let dir = TempDir::new().expect("failed to create temp dir");
        let db_path = dir.path().join("test.db");
        let db = waddle_storage::open_database(&db_path)
            .await
            .expect("failed to open database");
        let db = Arc::new(db);
        let event_bus: Arc<dyn EventBus> = Arc::new(BroadcastEventBus::default());
        let manager = Arc::new(MucManager::new(db, event_bus.clone()));
        (manager, event_bus, dir)
    }

    fn make_event(channel: &str, payload: EventPayload) -> Event {
        Event::new(
            Channel::new(channel).unwrap(),
            EventSource::System("test".into()),
            payload,
        )
    }

    fn make_muc_message(id: &str, from: &str, room: &str, body: &str) -> ChatMessage {
        ChatMessage {
            id: id.to_string(),
            from: from.to_string(),
            to: room.to_string(),
            body: body.to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Groupchat,
            thread: None,
        }
    }

    fn make_occupant(nick: &str, role: MucRole, affiliation: MucAffiliation) -> MucOccupant {
        MucOccupant {
            nick: nick.to_string(),
            jid: None,
            affiliation,
            role,
        }
    }

    #[tokio::test]
    async fn join_room_emits_event_and_persists() {
        let (manager, event_bus, _dir) = setup_muc().await;
        let mut sub = event_bus.subscribe("ui.**").unwrap();

        manager
            .join_room("room@conference.example.com", "Alice")
            .await
            .unwrap();

        let received = tokio::time::timeout(std::time::Duration::from_millis(100), sub.recv())
            .await
            .expect("timed out")
            .expect("should receive event");

        assert!(matches!(
            received.payload,
            EventPayload::MucJoinRequested {
                ref room,
                ref nick,
            } if room == "room@conference.example.com" && nick == "Alice"
        ));

        let rooms = manager.get_rooms().await.unwrap();
        assert_eq!(rooms.len(), 1);
        assert_eq!(rooms[0].room_jid, "room@conference.example.com");
        assert_eq!(rooms[0].nick, "Alice");
        assert!(!rooms[0].joined);
    }

    #[tokio::test]
    async fn handle_muc_joined_marks_room_joined() {
        let (manager, _, _dir) = setup_muc().await;

        manager
            .join_room("room@conference.example.com", "Alice")
            .await
            .unwrap();

        let event = make_event(
            "xmpp.muc.joined",
            EventPayload::MucJoined {
                room: "room@conference.example.com".to_string(),
                nick: "Alice".to_string(),
            },
        );
        manager.handle_event(&event).await;

        let rooms = manager.get_joined_rooms().await.unwrap();
        assert_eq!(rooms.len(), 1);
        assert!(rooms[0].joined);
    }

    #[tokio::test]
    async fn handle_muc_left_marks_room_not_joined() {
        let (manager, _, _dir) = setup_muc().await;

        manager
            .join_room("room@conference.example.com", "Alice")
            .await
            .unwrap();

        let join_event = make_event(
            "xmpp.muc.joined",
            EventPayload::MucJoined {
                room: "room@conference.example.com".to_string(),
                nick: "Alice".to_string(),
            },
        );
        manager.handle_event(&join_event).await;

        let leave_event = make_event(
            "xmpp.muc.left",
            EventPayload::MucLeft {
                room: "room@conference.example.com".to_string(),
            },
        );
        manager.handle_event(&leave_event).await;

        let joined_rooms = manager.get_joined_rooms().await.unwrap();
        assert!(joined_rooms.is_empty());

        let all_rooms = manager.get_rooms().await.unwrap();
        assert_eq!(all_rooms.len(), 1);
        assert!(!all_rooms[0].joined);
    }

    #[tokio::test]
    async fn leave_room_emits_event() {
        let (manager, event_bus, _dir) = setup_muc().await;
        let mut sub = event_bus.subscribe("ui.**").unwrap();

        manager
            .leave_room("room@conference.example.com")
            .await
            .unwrap();

        let received = tokio::time::timeout(std::time::Duration::from_millis(100), sub.recv())
            .await
            .expect("timed out")
            .expect("should receive event");

        assert!(matches!(
            received.payload,
            EventPayload::MucLeaveRequested {
                ref room,
            } if room == "room@conference.example.com"
        ));
    }

    #[tokio::test]
    async fn send_muc_message_emits_event() {
        let (manager, event_bus, _dir) = setup_muc().await;
        let mut sub = event_bus.subscribe("ui.**").unwrap();

        manager
            .send_message("room@conference.example.com", "Hello everyone!")
            .await
            .unwrap();

        let received = tokio::time::timeout(std::time::Duration::from_millis(100), sub.recv())
            .await
            .expect("timed out")
            .expect("should receive event");

        assert!(matches!(
            received.payload,
            EventPayload::MucSendRequested {
                ref room,
                ref body,
            } if room == "room@conference.example.com" && body == "Hello everyone!"
        ));
    }

    #[tokio::test]
    async fn handle_muc_message_received_persists() {
        let (manager, _, _dir) = setup_muc().await;

        let msg = make_muc_message(
            "muc-msg-1",
            "room@conference.example.com/Bob",
            "room@conference.example.com",
            "Hi all!",
        );

        let event = make_event(
            "xmpp.muc.message.received",
            EventPayload::MucMessageReceived {
                room: "room@conference.example.com".to_string(),
                message: msg,
            },
        );
        manager.handle_event(&event).await;

        let messages = manager
            .get_room_messages("room@conference.example.com", 50, None)
            .await
            .unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id, "muc-msg-1");
        assert_eq!(messages[0].body, "Hi all!");
        assert!(matches!(messages[0].message_type, MessageType::Groupchat));
    }

    #[tokio::test]
    async fn handle_muc_message_received_persists_using_room_jid() {
        let (manager, _, _dir) = setup_muc().await;

        let msg = ChatMessage {
            id: "muc-msg-user-to".to_string(),
            from: "room@conference.example.com/Bob".to_string(),
            to: "alice@example.com".to_string(),
            body: "Directly addressed stanza".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Groupchat,
            thread: None,
        };

        let event = make_event(
            "xmpp.muc.message.received",
            EventPayload::MucMessageReceived {
                room: "room@conference.example.com".to_string(),
                message: msg,
            },
        );
        manager.handle_event(&event).await;

        let messages = manager
            .get_room_messages("room@conference.example.com", 50, None)
            .await
            .unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id, "muc-msg-user-to");
        assert_eq!(messages[0].to, "room@conference.example.com");
    }

    #[tokio::test]
    async fn handle_muc_subject_changed_persists() {
        let (manager, _, _dir) = setup_muc().await;

        manager
            .join_room("room@conference.example.com", "Alice")
            .await
            .unwrap();

        let join_event = make_event(
            "xmpp.muc.joined",
            EventPayload::MucJoined {
                room: "room@conference.example.com".to_string(),
                nick: "Alice".to_string(),
            },
        );
        manager.handle_event(&join_event).await;

        let event = make_event(
            "xmpp.muc.subject.changed",
            EventPayload::MucSubjectChanged {
                room: "room@conference.example.com".to_string(),
                subject: "Sprint Planning - Week 7".to_string(),
            },
        );
        manager.handle_event(&event).await;

        let rooms = manager.get_joined_rooms().await.unwrap();
        assert_eq!(rooms.len(), 1);
        assert_eq!(
            rooms[0].subject,
            Some("Sprint Planning - Week 7".to_string())
        );
    }

    #[tokio::test]
    async fn occupant_tracking_add_and_remove() {
        let (manager, _, _dir) = setup_muc().await;

        let occupant = make_occupant("Bob", MucRole::Participant, MucAffiliation::Member);
        let event = make_event(
            "xmpp.muc.occupant.changed",
            EventPayload::MucOccupantChanged {
                room: "room@conference.example.com".to_string(),
                occupant,
            },
        );
        manager.handle_event(&event).await;

        let occupants = manager.get_occupants("room@conference.example.com");
        assert_eq!(occupants.len(), 1);
        assert_eq!(occupants[0].nick, "Bob");
        assert!(matches!(occupants[0].role, MucRole::Participant));
        assert!(matches!(occupants[0].affiliation, MucAffiliation::Member));

        // Occupant leaves (role = None)
        let left_occupant = make_occupant("Bob", MucRole::None, MucAffiliation::Member);
        let leave_event = make_event(
            "xmpp.muc.occupant.changed",
            EventPayload::MucOccupantChanged {
                room: "room@conference.example.com".to_string(),
                occupant: left_occupant,
            },
        );
        manager.handle_event(&leave_event).await;

        let occupants = manager.get_occupants("room@conference.example.com");
        assert!(occupants.is_empty());
    }

    #[tokio::test]
    async fn multiple_occupants_tracked() {
        let (manager, _, _dir) = setup_muc().await;

        let occupants_data = vec![
            ("Alice", MucRole::Moderator, MucAffiliation::Owner),
            ("Bob", MucRole::Participant, MucAffiliation::Member),
            ("Carol", MucRole::Visitor, MucAffiliation::None),
        ];

        for (nick, role, affiliation) in &occupants_data {
            let occupant = make_occupant(nick, role.clone(), affiliation.clone());
            let event = make_event(
                "xmpp.muc.occupant.changed",
                EventPayload::MucOccupantChanged {
                    room: "room@conference.example.com".to_string(),
                    occupant,
                },
            );
            manager.handle_event(&event).await;
        }

        let occupants = manager.get_occupants("room@conference.example.com");
        assert_eq!(occupants.len(), 3);
    }

    #[tokio::test]
    async fn occupants_cleared_on_room_leave() {
        let (manager, _, _dir) = setup_muc().await;

        manager
            .join_room("room@conference.example.com", "Alice")
            .await
            .unwrap();

        let join_event = make_event(
            "xmpp.muc.joined",
            EventPayload::MucJoined {
                room: "room@conference.example.com".to_string(),
                nick: "Alice".to_string(),
            },
        );
        manager.handle_event(&join_event).await;

        let occupant = make_occupant("Bob", MucRole::Participant, MucAffiliation::Member);
        let occ_event = make_event(
            "xmpp.muc.occupant.changed",
            EventPayload::MucOccupantChanged {
                room: "room@conference.example.com".to_string(),
                occupant,
            },
        );
        manager.handle_event(&occ_event).await;
        assert_eq!(
            manager.get_occupants("room@conference.example.com").len(),
            1
        );

        let leave_event = make_event(
            "xmpp.muc.left",
            EventPayload::MucLeft {
                room: "room@conference.example.com".to_string(),
            },
        );
        manager.handle_event(&leave_event).await;

        assert!(
            manager
                .get_occupants("room@conference.example.com")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn get_room_messages_empty() {
        let (manager, _, _dir) = setup_muc().await;

        let messages = manager
            .get_room_messages("room@conference.example.com", 50, None)
            .await
            .unwrap();

        assert!(messages.is_empty());
    }

    #[tokio::test]
    async fn get_room_messages_only_returns_groupchat() {
        let (manager, _, _dir) = setup_muc().await;

        let gc_msg = make_muc_message(
            "muc-msg-1",
            "room@conference.example.com/Bob",
            "room@conference.example.com",
            "Group message",
        );
        let event = make_event(
            "xmpp.muc.message.received",
            EventPayload::MucMessageReceived {
                room: "room@conference.example.com".to_string(),
                message: gc_msg,
            },
        );
        manager.handle_event(&event).await;

        let messages = manager
            .get_room_messages("room@conference.example.com", 50, None)
            .await
            .unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].body, "Group message");
    }

    #[tokio::test]
    async fn get_room_messages_with_pagination() {
        let (manager, _, _dir) = setup_muc().await;

        let base = Utc::now();
        for i in 0..5 {
            let msg = ChatMessage {
                id: format!("muc-msg-{i}"),
                from: "room@conference.example.com/Bob".to_string(),
                to: "room@conference.example.com".to_string(),
                body: format!("Message {i}"),
                timestamp: base + chrono::Duration::seconds(i),
                message_type: MessageType::Groupchat,
                thread: None,
            };
            let event = make_event(
                "xmpp.muc.message.received",
                EventPayload::MucMessageReceived {
                    room: "room@conference.example.com".to_string(),
                    message: msg,
                },
            );
            manager.handle_event(&event).await;
        }

        let cutoff = (base + chrono::Duration::seconds(3)).to_rfc3339();
        let messages = manager
            .get_room_messages("room@conference.example.com", 50, Some(&cutoff))
            .await
            .unwrap();

        assert_eq!(messages.len(), 3);
    }

    #[tokio::test]
    async fn get_occupants_unknown_room_returns_empty() {
        let (manager, _, _dir) = setup_muc().await;

        let occupants = manager.get_occupants("unknown@conference.example.com");
        assert!(occupants.is_empty());
    }

    #[tokio::test]
    async fn duplicate_muc_message_not_inserted_twice() {
        let (manager, _, _dir) = setup_muc().await;

        let msg = make_muc_message(
            "muc-dup",
            "room@conference.example.com/Bob",
            "room@conference.example.com",
            "Hello",
        );

        for _ in 0..2 {
            let event = make_event(
                "xmpp.muc.message.received",
                EventPayload::MucMessageReceived {
                    room: "room@conference.example.com".to_string(),
                    message: msg.clone(),
                },
            );
            manager.handle_event(&event).await;
        }

        let messages = manager
            .get_room_messages("room@conference.example.com", 50, None)
            .await
            .unwrap();

        assert_eq!(messages.len(), 1);
    }

    #[tokio::test]
    async fn muc_run_loop_processes_events() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (manager, event_bus, _dir) = setup_muc().await;

                // Pre-create the room so we can verify join
                manager
                    .join_room("room@conference.example.com", "Alice")
                    .await
                    .unwrap();

                let manager_clone = manager.clone();
                let handle = tokio::task::spawn_local(async move { manager_clone.run().await });

                tokio::task::yield_now().await;
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;

                event_bus
                    .publish(Event::new(
                        Channel::new("xmpp.muc.joined").unwrap(),
                        EventSource::Xmpp,
                        EventPayload::MucJoined {
                            room: "room@conference.example.com".to_string(),
                            nick: "Alice".to_string(),
                        },
                    ))
                    .unwrap();

                tokio::time::sleep(std::time::Duration::from_millis(50)).await;

                let rooms = manager.get_joined_rooms().await.unwrap();
                assert_eq!(rooms.len(), 1);
                assert!(rooms[0].joined);

                handle.abort();
            })
            .await;
    }

    #[tokio::test]
    async fn occupant_role_update_tracked() {
        let (manager, _, _dir) = setup_muc().await;

        let occupant = make_occupant("Bob", MucRole::Participant, MucAffiliation::Member);
        let event = make_event(
            "xmpp.muc.occupant.changed",
            EventPayload::MucOccupantChanged {
                room: "room@conference.example.com".to_string(),
                occupant,
            },
        );
        manager.handle_event(&event).await;

        let occupant_updated = make_occupant("Bob", MucRole::Moderator, MucAffiliation::Admin);
        let update_event = make_event(
            "xmpp.muc.occupant.changed",
            EventPayload::MucOccupantChanged {
                room: "room@conference.example.com".to_string(),
                occupant: occupant_updated,
            },
        );
        manager.handle_event(&update_event).await;

        let occupants = manager.get_occupants("room@conference.example.com");
        assert_eq!(occupants.len(), 1);
        assert_eq!(occupants[0].nick, "Bob");
        assert!(matches!(occupants[0].role, MucRole::Moderator));
        assert!(matches!(occupants[0].affiliation, MucAffiliation::Admin));
    }
}
