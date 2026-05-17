//! Persistence trait for the inbox.
//!
//! The actual libSQL-backed implementation lives in `waddle-server`; this
//! crate only defines the contract so tests and handlers can work against
//! an in-memory fake.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use jid::BareJid;

use super::{InboxEntry, InboxKey, InboxView};

/// Errors returned by [`InboxStorage`] implementations.
#[derive(Debug, thiserror::Error)]
pub enum InboxStorageError {
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
}
