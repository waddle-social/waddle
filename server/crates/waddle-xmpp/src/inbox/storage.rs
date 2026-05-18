//! Persistence trait for the inbox.
//!
//! The actual libSQL-backed implementation lives in `waddle-server`; this
//! crate only defines the contract so tests and handlers can work against
//! an in-memory fake.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use jid::{BareJid, Jid};
use waddle_xmpp_core::xep0359::StanzaId;

use super::{InboxEntry, InboxKey, InboxView};

/// Durable key for one groupchat notification recovery item.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GroupchatNotificationRecoveryKey {
    pub recipient: BareJid,
    pub room: BareJid,
    pub thread_id: Option<String>,
    pub archive_stanza_id: StanzaId,
}

/// Durable retry item created alongside a committed groupchat inbox row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupchatNotificationRecovery {
    pub key: GroupchatNotificationRecoveryKey,
    pub sender_jid: Jid,
    pub is_live_occupant: bool,
    pub room_members_only: bool,
    pub sender_role: crate::Role,
    pub mention_permissions: crate::xep::xep0513::MentionPermissions,
    pub occupant_id_bare_jids: Vec<(crate::xep::xep0421::OccupantId, BareJid)>,
    pub created_at_ms: i64,
}

/// Errors returned by [`InboxStorage`] implementations.
#[derive(Debug, thiserror::Error)]
pub enum InboxStorageError {
    #[error("invalid groupchat notification sender role: {value}")]
    InvalidGroupchatNotificationSenderRole { value: String },
    #[error("invalid groupchat notification mention permission: {value}")]
    InvalidGroupchatNotificationMentionPermission { value: String },
    #[error("invalid groupchat notification mention count: {value}")]
    InvalidGroupchatNotificationMentionCount { value: i64 },
    #[error("invalid groupchat occupant-id map JSON")]
    InvalidGroupchatOccupantIdMapJson {
        #[source]
        source: serde_json::Error,
    },
    #[error("invalid groupchat occupant-id map bare JID: {value}")]
    InvalidGroupchatOccupantIdMapBareJid {
        value: String,
        #[source]
        source: jid::Error,
    },
    #[error(
        "groupchat_notification_recovery schema is missing required columns {missing_columns:?}; apply a versioned schema migration or recreate the inbox database"
    )]
    InvalidGroupchatNotificationRecoverySchema { missing_columns: Vec<String> },
    #[error("inbox storage error: {0}")]
    Other(String),
}

/// Storage contract for the per-user inbox projection.
#[async_trait]
pub trait InboxStorage: Send + Sync {
    /// Fetch every channel-level inbox entry for `user`, newest first.
    async fn list(&self, user: &BareJid) -> Result<Vec<InboxEntry>, InboxStorageError>;

    /// Fetch thread-level inbox entries for `user` within a specific room.
    async fn list_threads(
        &self,
        user: &BareJid,
        room: &BareJid,
    ) -> Result<Vec<InboxEntry>, InboxStorageError>;

    /// Fetch every thread-level inbox entry for `user` across all rooms,
    /// newest first. Used by the global `urn:waddle:threads:0` view.
    ///
    /// Default implementation walks `list` over the user's channel
    /// conversations and aggregates `list_threads` per room — backends
    /// SHOULD override with a single-query implementation when one is
    /// available (the SQL backend does).
    async fn list_all_threads(&self, user: &BareJid) -> Result<Vec<InboxEntry>, InboxStorageError> {
        let channels = self.list(user).await?;
        let mut all = Vec::new();
        for channel in &channels {
            let partner = channel.partner.clone();
            let threads = self.list_threads(user, &partner).await?;
            all.extend(threads);
        }
        all.sort_by_key(|entry| std::cmp::Reverse(entry.last_updated));
        Ok(all)
    }

    /// Upsert an entry, incrementing unread unless `increment_unread` is
    /// false. Returns the post-upsert state of the entry.
    async fn upsert(
        &self,
        user: &BareJid,
        entry: InboxEntry,
        increment_unread: bool,
    ) -> Result<InboxEntry, InboxStorageError>;

    /// Upsert an inbox row and, when present, durably record the
    /// notification recovery item that corresponds to that committed
    /// projection.
    async fn upsert_with_groupchat_notification_recovery(
        &self,
        user: &BareJid,
        entry: InboxEntry,
        increment_unread: bool,
        recovery: Option<GroupchatNotificationRecovery>,
    ) -> Result<InboxEntry, InboxStorageError> {
        let updated = self.upsert(user, entry, increment_unread).await?;
        if let Some(recovery) = recovery {
            self.insert_groupchat_notification_recovery(recovery)
                .await?;
        }
        Ok(updated)
    }

    /// Insert a pending groupchat notification recovery item.
    async fn insert_groupchat_notification_recovery(
        &self,
        recovery: GroupchatNotificationRecovery,
    ) -> Result<(), InboxStorageError> {
        let _ = recovery;
        Ok(())
    }

    /// List a bounded global page of uncompleted groupchat notification
    /// recovery items.
    async fn list_pending_groupchat_notification_recoveries(
        &self,
        limit: usize,
    ) -> Result<Vec<GroupchatNotificationRecovery>, InboxStorageError> {
        let _ = limit;
        Ok(Vec::new())
    }

    /// Mark a groupchat notification recovery item as complete.
    async fn mark_groupchat_notification_recovery_completed(
        &self,
        key: &GroupchatNotificationRecoveryKey,
    ) -> Result<u64, InboxStorageError> {
        let _ = key;
        Ok(0)
    }

    /// Delete completed groupchat notification recovery items older than
    /// `cutoff_ms`, bounded by `limit`.
    async fn prune_completed_groupchat_notification_recoveries(
        &self,
        cutoff_ms: i64,
        limit: usize,
    ) -> Result<u64, InboxStorageError> {
        let _ = (cutoff_ms, limit);
        Ok(0)
    }

    /// Mark a conversation as read. When `thread_id` is `Some`, only the
    /// specific thread is marked; otherwise the channel-level entry is.
    ///
    /// Returns the post-update entry so the caller can fan it out to the
    /// user's other resources (cross-device sync); `None` when no row
    /// matched the `(user, partner, thread_id)` triple.
    async fn mark_read(
        &self,
        user: &BareJid,
        partner: &BareJid,
        thread_id: Option<&str>,
    ) -> Result<Option<InboxEntry>, InboxStorageError>;

    /// Return the total unread count for the user (channel-level only).
    async fn total_unread(&self, user: &BareJid) -> Result<u64, InboxStorageError>;
}

/// In-memory implementation used for tests and as the storage fake for
/// handler unit tests.
#[derive(Debug, Default)]
pub struct InMemoryInboxStorage {
    per_user: Mutex<HashMap<BareJid, InboxView>>,
    groupchat_notification_recoveries:
        Mutex<HashMap<GroupchatNotificationRecoveryKey, GroupchatNotificationRecovery>>,
    completed_groupchat_notification_recoveries:
        Mutex<HashMap<GroupchatNotificationRecoveryKey, i64>>,
}

impl InMemoryInboxStorage {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl InboxStorage for InMemoryInboxStorage {
    async fn list(&self, user: &BareJid) -> Result<Vec<InboxEntry>, InboxStorageError> {
        let guard = self
            .per_user
            .lock()
            .map_err(|e| InboxStorageError::Other(e.to_string()))?;
        Ok(guard.get(user).map(|v| v.snapshot()).unwrap_or_default())
    }

    async fn list_threads(
        &self,
        user: &BareJid,
        room: &BareJid,
    ) -> Result<Vec<InboxEntry>, InboxStorageError> {
        let guard = self
            .per_user
            .lock()
            .map_err(|e| InboxStorageError::Other(e.to_string()))?;
        Ok(guard
            .get(user)
            .map(|v| v.threads_for_room(room))
            .unwrap_or_default())
    }

    async fn upsert(
        &self,
        user: &BareJid,
        entry: InboxEntry,
        increment_unread: bool,
    ) -> Result<InboxEntry, InboxStorageError> {
        let mut guard = self
            .per_user
            .lock()
            .map_err(|e| InboxStorageError::Other(e.to_string()))?;
        let view = guard.entry(user.clone()).or_default();
        Ok(view.observe_message(entry, increment_unread))
    }

    async fn upsert_with_groupchat_notification_recovery(
        &self,
        user: &BareJid,
        entry: InboxEntry,
        increment_unread: bool,
        recovery: Option<GroupchatNotificationRecovery>,
    ) -> Result<InboxEntry, InboxStorageError> {
        let updated = {
            let mut guard = self
                .per_user
                .lock()
                .map_err(|e| InboxStorageError::Other(e.to_string()))?;
            let view = guard.entry(user.clone()).or_default();
            view.observe_message(entry, increment_unread)
        };

        if let Some(recovery) = recovery {
            self.insert_groupchat_notification_recovery(recovery)
                .await?;
        }
        Ok(updated)
    }

    async fn insert_groupchat_notification_recovery(
        &self,
        recovery: GroupchatNotificationRecovery,
    ) -> Result<(), InboxStorageError> {
        let mut recoveries = self
            .groupchat_notification_recoveries
            .lock()
            .map_err(|e| InboxStorageError::Other(e.to_string()))?;
        let mut completed = self
            .completed_groupchat_notification_recoveries
            .lock()
            .map_err(|e| InboxStorageError::Other(e.to_string()))?;
        completed.remove(&recovery.key);
        recoveries.insert(recovery.key.clone(), recovery);
        Ok(())
    }

    async fn list_pending_groupchat_notification_recoveries(
        &self,
        limit: usize,
    ) -> Result<Vec<GroupchatNotificationRecovery>, InboxStorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let recoveries = self
            .groupchat_notification_recoveries
            .lock()
            .map_err(|e| InboxStorageError::Other(e.to_string()))?;
        let completed = self
            .completed_groupchat_notification_recoveries
            .lock()
            .map_err(|e| InboxStorageError::Other(e.to_string()))?;
        let mut pending = recoveries
            .iter()
            .filter(|(key, _)| !completed.contains_key(*key))
            .map(|(_, recovery)| recovery.clone())
            .collect::<Vec<_>>();
        pending.sort_by(|a, b| {
            a.created_at_ms
                .cmp(&b.created_at_ms)
                .then_with(|| a.key.recipient.cmp(&b.key.recipient))
                .then_with(|| a.key.room.cmp(&b.key.room))
                .then_with(|| a.key.thread_id.cmp(&b.key.thread_id))
                .then_with(|| a.key.archive_stanza_id.by.cmp(&b.key.archive_stanza_id.by))
                .then_with(|| a.key.archive_stanza_id.id.cmp(&b.key.archive_stanza_id.id))
        });
        pending.truncate(limit);
        Ok(pending)
    }

    async fn mark_groupchat_notification_recovery_completed(
        &self,
        key: &GroupchatNotificationRecoveryKey,
    ) -> Result<u64, InboxStorageError> {
        let recoveries = self
            .groupchat_notification_recoveries
            .lock()
            .map_err(|e| InboxStorageError::Other(e.to_string()))?;
        if !recoveries.contains_key(key) {
            return Ok(0);
        }
        drop(recoveries);
        let mut completed = self
            .completed_groupchat_notification_recoveries
            .lock()
            .map_err(|e| InboxStorageError::Other(e.to_string()))?;
        let was_pending = !completed.contains_key(key);
        completed.insert(key.clone(), chrono::Utc::now().timestamp_millis());
        Ok(u64::from(was_pending))
    }

    async fn prune_completed_groupchat_notification_recoveries(
        &self,
        cutoff_ms: i64,
        limit: usize,
    ) -> Result<u64, InboxStorageError> {
        if limit == 0 {
            return Ok(0);
        }
        let mut recoveries = self
            .groupchat_notification_recoveries
            .lock()
            .map_err(|e| InboxStorageError::Other(e.to_string()))?;
        let mut completed = self
            .completed_groupchat_notification_recoveries
            .lock()
            .map_err(|e| InboxStorageError::Other(e.to_string()))?;
        let mut prune_keys = completed
            .iter()
            .filter(|(_, completed_at_ms)| **completed_at_ms < cutoff_ms)
            .map(|(key, completed_at_ms)| (key.clone(), *completed_at_ms))
            .collect::<Vec<_>>();
        prune_keys.sort_by(|(a_key, a_completed_at), (b_key, b_completed_at)| {
            a_completed_at
                .cmp(b_completed_at)
                .then_with(|| a_key.recipient.cmp(&b_key.recipient))
                .then_with(|| a_key.room.cmp(&b_key.room))
                .then_with(|| a_key.thread_id.cmp(&b_key.thread_id))
                .then_with(|| a_key.archive_stanza_id.by.cmp(&b_key.archive_stanza_id.by))
                .then_with(|| a_key.archive_stanza_id.id.cmp(&b_key.archive_stanza_id.id))
        });
        prune_keys.truncate(limit);
        let deleted = prune_keys.len();
        for (key, _) in prune_keys {
            completed.remove(&key);
            recoveries.remove(&key);
        }
        Ok(deleted as u64)
    }

    async fn mark_read(
        &self,
        user: &BareJid,
        partner: &BareJid,
        thread_id: Option<&str>,
    ) -> Result<Option<InboxEntry>, InboxStorageError> {
        let mut guard = self
            .per_user
            .lock()
            .map_err(|e| InboxStorageError::Other(e.to_string()))?;
        let Some(view) = guard.get_mut(user) else {
            return Ok(None);
        };
        let key = match thread_id {
            Some(tid) => InboxKey::thread(partner.clone(), tid),
            None => InboxKey::channel(partner.clone()),
        };
        Ok(view.mark_read_by_key(&key))
    }

    async fn total_unread(&self, user: &BareJid) -> Result<u64, InboxStorageError> {
        let guard = self
            .per_user
            .lock()
            .map_err(|e| InboxStorageError::Other(e.to_string()))?;
        Ok(guard.get(user).map(|v| v.total_unread()).unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::super::ConversationKind;
    use super::*;

    fn jid(s: &str) -> BareJid {
        s.parse().unwrap()
    }

    #[tokio::test]
    async fn test_in_memory_round_trip() {
        let store = InMemoryInboxStorage::new();
        let user = jid("me@example.com");
        store
            .upsert(
                &user,
                InboxEntry::new(
                    jid("alice@example.com"),
                    ConversationKind::Direct,
                    "sid-1",
                    100,
                )
                .with_preview("yo"),
                true,
            )
            .await
            .unwrap();
        store
            .upsert(
                &user,
                InboxEntry::new(
                    jid("general@conference.example.com"),
                    ConversationKind::MucRoom,
                    "sid-2",
                    200,
                ),
                true,
            )
            .await
            .unwrap();
        let snapshot = store.list(&user).await.unwrap();
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot[0].last_stanza_id, "sid-2");
        assert_eq!(store.total_unread(&user).await.unwrap(), 2);

        let updated = store
            .mark_read(&user, &jid("alice@example.com"), None)
            .await
            .unwrap()
            .expect("mark_read returns the post-update entry for fan-out");
        assert_eq!(updated.unread, 0);
        assert_eq!(updated.partner, jid("alice@example.com"));
        assert_eq!(store.total_unread(&user).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn test_mark_read_returns_none_when_no_row_matches() {
        let store = InMemoryInboxStorage::new();
        let user = jid("me@example.com");
        let result = store
            .mark_read(&user, &jid("ghost@example.com"), None)
            .await
            .unwrap();
        assert!(
            result.is_none(),
            "mark_read must signal no-op so the IQ handler skips fan-out"
        );
    }

    #[tokio::test]
    async fn test_thread_entries() {
        let store = InMemoryInboxStorage::new();
        let user = jid("me@example.com");
        let room = jid("room@muc.example.com");

        // Channel-level entry
        store
            .upsert(
                &user,
                InboxEntry::new(room.clone(), ConversationKind::MucRoom, "s1", 100),
                true,
            )
            .await
            .unwrap();

        // Thread entry
        store
            .upsert(
                &user,
                InboxEntry::new(room.clone(), ConversationKind::MucRoom, "s2", 200)
                    .with_thread("t1")
                    .with_thread_title("Discussion"),
                true,
            )
            .await
            .unwrap();

        // Channel list excludes threads
        let channels = store.list(&user).await.unwrap();
        assert_eq!(channels.len(), 1);
        assert!(channels[0].thread_id.is_none());

        // Thread list for room
        let threads = store.list_threads(&user, &room).await.unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].thread_id.as_deref(), Some("t1"));

        // Mark thread read
        let updated = store
            .mark_read(&user, &room, Some("t1"))
            .await
            .unwrap()
            .expect("thread entry returned for fan-out");
        assert_eq!(updated.unread, 0);
        assert_eq!(updated.thread_id.as_deref(), Some("t1"));
        let threads = store.list_threads(&user, &room).await.unwrap();
        assert_eq!(threads[0].unread, 0);

        // Channel unread unaffected
        assert_eq!(store.total_unread(&user).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn test_groupchat_notification_recovery_prune() {
        let store = InMemoryInboxStorage::new();
        let user = jid("me@example.com");
        let room = jid("room@muc.example.com");
        let recovery = GroupchatNotificationRecovery {
            key: GroupchatNotificationRecoveryKey {
                recipient: user,
                room,
                thread_id: Some("t1".to_string()),
                archive_stanza_id: StanzaId::new(
                    "groupchat-recovery-1",
                    "room@muc.example.com".parse().unwrap(),
                ),
            },
            sender_jid: "room@muc.example.com/alice".parse().unwrap(),
            is_live_occupant: true,
            room_members_only: false,
            sender_role: crate::Role::Participant,
            mention_permissions: crate::xep::xep0513::MentionPermissions::default(),
            occupant_id_bare_jids: vec![(
                crate::xep::xep0421::OccupantId::new("room-stable-me"),
                "me@example.com".parse().unwrap(),
            )],
            created_at_ms: 42,
        };

        store
            .insert_groupchat_notification_recovery(recovery.clone())
            .await
            .unwrap();
        assert_eq!(
            store
                .list_pending_groupchat_notification_recoveries(16)
                .await
                .unwrap(),
            vec![recovery.clone()]
        );
        assert_eq!(
            store
                .mark_groupchat_notification_recovery_completed(&recovery.key)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .prune_completed_groupchat_notification_recoveries(0, 16)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            store
                .prune_completed_groupchat_notification_recoveries(
                    chrono::Utc::now().timestamp_millis().saturating_add(1_000),
                    16,
                )
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .prune_completed_groupchat_notification_recoveries(
                    chrono::Utc::now().timestamp_millis().saturating_add(1_000),
                    16,
                )
                .await
                .unwrap(),
            0
        );
    }
}
