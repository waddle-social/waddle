use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use jid::BareJid;
use tokio::sync::RwLock;
use uuid::Uuid;
use waddle_xmpp_core::mam::{ArchivedMessage, MamQuery, MamResult};
use waddle_xmpp_core::xep0359::OriginId;

use crate::xep::matches_fulltext;

use super::origin_dedup::{origin_id_dedup_match, origin_id_tombstone_match};
use super::query_semantics::{
    archive_order_after, archive_order_before, matches_thread_filter, message_matches_with_filter,
    missing_requested_id, uses_backward_pagination,
};
use super::tombstone::apply_tombstone;
use super::{MamStorage, MamStorageError, StoreOutcome, TerminalTombstoneOutcome};

#[derive(Clone, Default)]
pub struct InMemoryMamStorage {
    entries: Arc<RwLock<Vec<(BareJid, ArchivedMessage)>>>,
}

impl InMemoryMamStorage {
    pub fn new() -> Self {
        Self::default()
    }

    fn generate_archive_id() -> String {
        Uuid::now_v7().to_string()
    }
}

#[async_trait]
impl MamStorage for InMemoryMamStorage {
    async fn store_message(
        &self,
        archive_jid: &BareJid,
        message: &ArchivedMessage,
    ) -> Result<StoreOutcome, MamStorageError> {
        let mut entries = self.entries.write().await;
        if message.origin_id.is_some() {
            if let Some((_, existing)) = entries.iter().find(|(jid, existing)| {
                jid == archive_jid && origin_id_dedup_match(existing, message)
            }) {
                return Ok(StoreOutcome::Deduplicated(existing.id.clone()));
            }
            if let Some((_, existing)) = entries.iter().find(|(jid, existing)| {
                jid == archive_jid && origin_id_tombstone_match(existing, message)
            }) {
                return Ok(StoreOutcome::TombstoneHit(existing.id.clone()));
            }
        }

        let archive_id = if message.id.is_empty() {
            Self::generate_archive_id()
        } else {
            message.id.clone()
        };

        let mut stored = message.clone();
        stored.id = archive_id.clone();

        entries.push((archive_jid.clone(), stored));
        Ok(StoreOutcome::Stored(archive_id))
    }

    async fn query_messages(
        &self,
        archive_jid: &BareJid,
        archive_kind: super::MamArchiveKind,
        query: &MamQuery,
    ) -> Result<MamResult, MamStorageError> {
        let entries = self.entries.read().await;
        let mut messages: Vec<ArchivedMessage> = entries
            .iter()
            .filter(|(jid, _)| jid == archive_jid)
            .map(|(_, message)| message.clone())
            .collect();
        let filter_before_cursor = match query.filter_before_id.as_deref() {
            Some(before_id) => Some(
                messages
                    .iter()
                    .find(|message| message.id == before_id)
                    .cloned()
                    .ok_or_else(|| MamStorageError::NotFound(before_id.to_string()))?,
            ),
            None => None,
        };
        let filter_after_cursor = match query.filter_after_id.as_deref() {
            Some(after_id) => Some(
                messages
                    .iter()
                    .find(|message| message.id == after_id)
                    .cloned()
                    .ok_or_else(|| MamStorageError::NotFound(after_id.to_string()))?,
            ),
            None => None,
        };
        if let Some(missing_id) = missing_requested_id(&messages, &query.ids) {
            return Err(MamStorageError::NotFound(missing_id));
        }
        let before_cursor = match query.before_id.as_deref().filter(|id| !id.is_empty()) {
            Some(before_id) => Some(
                messages
                    .iter()
                    .find(|message| message.id == before_id)
                    .cloned()
                    .ok_or_else(|| MamStorageError::NotFound(before_id.to_string()))?,
            ),
            None => None,
        };
        let after_cursor = match query.after_id.as_deref() {
            Some(after_id) => Some(
                messages
                    .iter()
                    .find(|message| message.id == after_id)
                    .cloned()
                    .ok_or_else(|| MamStorageError::NotFound(after_id.to_string()))?,
            ),
            None => None,
        };

        if let Some(start) = query.start {
            messages.retain(|message| message.timestamp >= start);
        }
        if let Some(end) = query.end {
            messages.retain(|message| message.timestamp <= end);
        }
        if let Some(with) = query.with.as_ref() {
            messages.retain(|message| {
                message_matches_with_filter(
                    archive_jid,
                    archive_kind,
                    &message.from,
                    &message.to,
                    with,
                )
            });
        }
        if !query.ids.is_empty() {
            let requested_ids = query.ids.iter().map(String::as_str).collect::<HashSet<_>>();
            messages.retain(|message| requested_ids.contains(message.id.as_str()));
        }
        if let Some(thread_id) = query.thread_id.as_ref() {
            messages.retain(|message| matches_thread_filter(message, thread_id.as_str()));
        }
        if let Some(fulltext) = query.fulltext.as_ref() {
            // None body matches no fulltext query — there's no text to
            // search. Treat absent body as empty for the matcher's
            // purposes; the matcher's existing semantics for "" are
            // unchanged.
            messages.retain(|message| {
                matches_fulltext(message.body.as_deref().unwrap_or(""), fulltext.as_str())
            });
        }
        if !query.stanza_ids.is_empty() {
            // Room pins pass archive-primary IDs. DM pins pass the pair-stable
            // logical message ID, stored as the wire `<message id>` on each
            // participant's personal archive row. Match both so the same MAM
            // filter hydrates pinned bodies for rooms and DMs.
            let allowed: HashSet<&str> = query.stanza_ids.iter().map(|id| id.as_str()).collect();
            messages.retain(|m| {
                allowed.contains(m.id.as_str())
                    || m.stanza_id
                        .as_ref()
                        .is_some_and(|stanza_id| allowed.contains(stanza_id.id.as_str()))
            });
        }
        if let Some(cursor) = filter_before_cursor.as_ref() {
            messages.retain(|message| archive_order_before(message, cursor));
        }
        if let Some(cursor) = filter_after_cursor.as_ref() {
            messages.retain(|message| archive_order_after(message, cursor));
        }
        let count = Some(u32::try_from(messages.len()).unwrap_or(u32::MAX));

        if let Some(cursor) = before_cursor {
            messages.retain(|message| archive_order_before(message, &cursor));
        }
        if let Some(cursor) = after_cursor {
            messages.retain(|message| archive_order_after(message, &cursor));
        }

        // XEP-0313 §archive_order: results MUST be in chronological (received)
        // order. Order by timestamp first; archive id is the tiebreak for
        // messages that share a timestamp.
        if uses_backward_pagination(query) {
            messages.sort_by(|a, b| b.timestamp.cmp(&a.timestamp).then_with(|| b.id.cmp(&a.id)));
        } else {
            messages.sort_by(|a, b| a.timestamp.cmp(&b.timestamp).then_with(|| a.id.cmp(&b.id)));
        }

        let actual_limit = query.max.unwrap_or(100).min(500) as usize;
        let mut complete = true;
        if messages.len() > actual_limit {
            messages.truncate(actual_limit);
            complete = actual_limit == 0;
        }

        if uses_backward_pagination(query) {
            messages.reverse();
        }

        let first_id = messages.first().map(|message| message.id.clone());
        let last_id = messages.last().map(|message| message.id.clone());
        Ok(MamResult {
            messages,
            complete,
            first_id,
            last_id,
            count,
        })
    }

    async fn get_message(
        &self,
        archive_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        let entries = self.entries.read().await;
        Ok(entries
            .iter()
            .find(|(_, message)| message.id == archive_id)
            .map(|(_, message)| message.clone()))
    }

    async fn get_message_by_stanza_id(
        &self,
        archive_jid: &BareJid,
        stanza_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        let entries = self.entries.read().await;
        Ok(entries
            .iter()
            .find(|(jid, message)| {
                jid == archive_jid
                    && (message.stanza_id.as_ref().map(|s| s.id.as_str()) == Some(stanza_id)
                        || message.origin_id.as_ref().map(|o| o.id.as_str()) == Some(stanza_id))
            })
            .map(|(_, message)| message.clone()))
    }

    async fn get_message_by_message_id(
        &self,
        archive_jid: &BareJid,
        message_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        let entries = self.entries.read().await;
        Ok(entries
            .iter()
            .find(|(jid, message)| {
                jid == archive_jid
                    && message.stanza_id.as_ref().map(|s| s.id.as_str()) == Some(message_id)
            })
            .map(|(_, message)| message.clone()))
    }

    async fn get_message_by_sender_and_origin_id(
        &self,
        archive_jid: &BareJid,
        archive_kind: super::MamArchiveKind,
        sender: &jid::Jid,
        origin_id: &OriginId,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        let entries = self.entries.read().await;
        Ok(entries
            .iter()
            .filter_map(|(stored_archive, message)| {
                let sender_matches = match archive_kind {
                    super::MamArchiveKind::Personal => message.from.to_bare() == sender.to_bare(),
                    super::MamArchiveKind::Room => &message.from == sender,
                };
                if stored_archive != archive_jid || !sender_matches {
                    return None;
                }
                let match_priority = if message
                    .origin_id
                    .as_ref()
                    .is_some_and(|candidate| candidate == origin_id)
                {
                    0
                } else if message
                    .stanza_id
                    .as_ref()
                    .is_some_and(|candidate| candidate.id.as_str() == origin_id.as_str())
                {
                    1
                } else {
                    return None;
                };
                Some((match_priority, message))
            })
            .min_by(|(left_priority, left), (right_priority, right)| {
                left_priority.cmp(right_priority).then_with(|| {
                    left.timestamp
                        .cmp(&right.timestamp)
                        .then_with(|| left.id.cmp(&right.id))
                })
            })
            .map(|(_, message)| message)
            .cloned())
    }

    async fn get_message_by_archive_or_stanza_id(
        &self,
        archive_jid: &BareJid,
        stanza_id: &str,
    ) -> Result<Option<ArchivedMessage>, MamStorageError> {
        let entries = self.entries.read().await;
        Ok(entries
            .iter()
            .find(|(jid, message)| {
                jid == archive_jid
                    && (message.id == stanza_id
                        || message.stanza_id.as_ref().map(|s| s.id.as_str()) == Some(stanza_id))
            })
            .map(|(_, message)| message.clone()))
    }

    async fn count_messages(&self, room_jid: &BareJid) -> Result<u32, MamStorageError> {
        let entries = self.entries.read().await;
        Ok(entries.iter().filter(|(jid, _)| jid == room_jid).count() as u32)
    }

    async fn delete_before(
        &self,
        room_jid: &BareJid,
        before: DateTime<Utc>,
    ) -> Result<u64, MamStorageError> {
        let mut entries = self.entries.write().await;
        let previous_len = entries.len();
        entries.retain(|(jid, message)| !(jid == room_jid && message.timestamp < before));
        Ok((previous_len - entries.len()) as u64)
    }

    async fn replace_with_tombstone(
        &self,
        archive_id: &str,
        tombstone: waddle_xmpp_core::mam::ArchivedTombstone,
    ) -> Result<bool, MamStorageError> {
        let mut entries = self.entries.write().await;
        for (_jid, message) in entries.iter_mut() {
            if message.id == archive_id {
                apply_tombstone(message, tombstone);
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn replace_with_terminal_tombstone(
        &self,
        archive_id: &str,
        tombstone: waddle_xmpp_core::mam::ArchivedTombstone,
    ) -> Result<TerminalTombstoneOutcome, MamStorageError> {
        let mut entries = self.entries.write().await;
        let Some((_, message)) = entries
            .iter_mut()
            .find(|(_, message)| message.id == archive_id)
        else {
            return Ok(TerminalTombstoneOutcome::NotFound);
        };
        if message.rich.as_ref().is_some_and(|rich| {
            matches!(
                rich.payload.as_ref(),
                Some(waddle_xmpp_core::mam::ArchivedRichPayload::Tombstone(_))
            )
        }) {
            return Ok(TerminalTombstoneOutcome::AlreadyTombstoned);
        }
        apply_tombstone(message, tombstone);
        Ok(TerminalTombstoneOutcome::Replaced)
    }
}
