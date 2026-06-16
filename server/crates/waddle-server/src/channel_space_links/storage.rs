//! In-memory [`ChannelSpaceLinkStore`] for tests and as the placeholder
//! `AppState` dependency in unit-test constructors.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use jid::BareJid;

use super::{ChannelSpaceLink, ChannelSpaceLinkError, ChannelSpaceLinkStore};
use crate::space_identity::SpaceNode;

#[derive(Debug, Default)]
pub struct InMemoryChannelSpaceLinkStore {
    rows: Mutex<HashMap<BareJid, ChannelSpaceLink>>,
}

impl InMemoryChannelSpaceLinkStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ChannelSpaceLinkStore for InMemoryChannelSpaceLinkStore {
    async fn set(&self, link: &ChannelSpaceLink) -> Result<(), ChannelSpaceLinkError> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|error| ChannelSpaceLinkError::Storage(error.to_string()))?;
        // Preserve `created_at` on an existing row to keep list
        // ordering stable across overwrites, matching the SQLite impl.
        match guard.get(&link.channel_jid) {
            Some(existing) => {
                let preserved_created_at = existing.created_at;
                guard.insert(
                    link.channel_jid.clone(),
                    ChannelSpaceLink {
                        created_at: preserved_created_at,
                        ..link.clone()
                    },
                );
            }
            None => {
                guard.insert(link.channel_jid.clone(), link.clone());
            }
        }
        Ok(())
    }

    async fn clear(&self, channel_jid: &BareJid) -> Result<bool, ChannelSpaceLinkError> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|error| ChannelSpaceLinkError::Storage(error.to_string()))?;
        Ok(guard.remove(channel_jid).is_some())
    }

    async fn get(
        &self,
        channel_jid: &BareJid,
    ) -> Result<Option<ChannelSpaceLink>, ChannelSpaceLinkError> {
        let guard = self
            .rows
            .lock()
            .map_err(|error| ChannelSpaceLinkError::Storage(error.to_string()))?;
        Ok(guard.get(channel_jid).cloned())
    }

    async fn list_channels_in_space(
        &self,
        space_jid: &BareJid,
    ) -> Result<Vec<BareJid>, ChannelSpaceLinkError> {
        let guard = self
            .rows
            .lock()
            .map_err(|error| ChannelSpaceLinkError::Storage(error.to_string()))?;
        let mut rows: Vec<ChannelSpaceLink> = guard
            .values()
            .filter(|row| &row.space_jid == space_jid)
            .cloned()
            .collect();
        rows.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.channel_jid.cmp(&b.channel_jid))
        });
        Ok(rows.into_iter().map(|row| row.channel_jid).collect())
    }

    async fn list_channels_in_space_node(
        &self,
        space_node: &SpaceNode,
    ) -> Result<Vec<BareJid>, ChannelSpaceLinkError> {
        let guard = self
            .rows
            .lock()
            .map_err(|error| ChannelSpaceLinkError::Storage(error.to_string()))?;
        let mut rows: Vec<ChannelSpaceLink> = guard
            .values()
            .filter(|row| &row.space_node == space_node)
            .cloned()
            .collect();
        rows.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.channel_jid.cmp(&b.channel_jid))
        });
        Ok(rows.into_iter().map(|row| row.channel_jid).collect())
    }

    async fn list_all(&self) -> Result<Vec<ChannelSpaceLink>, ChannelSpaceLinkError> {
        let guard = self
            .rows
            .lock()
            .map_err(|error| ChannelSpaceLinkError::Storage(error.to_string()))?;
        let mut rows: Vec<ChannelSpaceLink> = guard.values().cloned().collect();
        rows.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.channel_jid.cmp(&b.channel_jid))
        });
        Ok(rows)
    }
}
