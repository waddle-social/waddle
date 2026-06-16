//! In-memory [`SpacesMetadataStore`] for tests and as the placeholder
//! `AppState` dependency in unit-test constructors.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use jid::BareJid;

use super::{SpaceMetadata, SpacesMetadataError, SpacesMetadataStore};
use crate::space_identity::SpaceNode;

#[derive(Debug, Default)]
pub struct InMemorySpacesMetadataStore {
    rows: Mutex<HashMap<String, SpaceMetadata>>,
}

impl InMemorySpacesMetadataStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl SpacesMetadataStore for InMemorySpacesMetadataStore {
    async fn get(&self, space_jid: &BareJid) -> Result<Option<SpaceMetadata>, SpacesMetadataError> {
        let guard = self
            .rows
            .lock()
            .map_err(|error| SpacesMetadataError::Storage(error.to_string()))?;
        Ok(guard
            .values()
            .find(|row| &row.space_jid == space_jid)
            .cloned())
    }

    async fn get_by_node(
        &self,
        space_node: &SpaceNode,
    ) -> Result<Option<SpaceMetadata>, SpacesMetadataError> {
        let guard = self
            .rows
            .lock()
            .map_err(|error| SpacesMetadataError::Storage(error.to_string()))?;
        Ok(guard.get(space_node.as_str()).cloned())
    }

    async fn upsert(&self, metadata: &SpaceMetadata) -> Result<(), SpacesMetadataError> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|error| SpacesMetadataError::Storage(error.to_string()))?;
        // Preserve created_at on an existing row so callers can supply
        // it freely on `upsert`; the trait contract says created_at is
        // assigned at first insert.
        match guard.get(metadata.space_node.as_str()) {
            Some(existing) => {
                let preserved_created_at = existing.created_at;
                guard.insert(
                    metadata.space_node.to_string(),
                    SpaceMetadata {
                        created_at: preserved_created_at,
                        ..metadata.clone()
                    },
                );
            }
            None => {
                guard.insert(metadata.space_node.to_string(), metadata.clone());
            }
        }
        Ok(())
    }

    async fn delete(&self, space_jid: &BareJid) -> Result<bool, SpacesMetadataError> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|error| SpacesMetadataError::Storage(error.to_string()))?;
        let key = guard
            .iter()
            .find_map(|(key, row)| (&row.space_jid == space_jid).then(|| key.clone()));
        Ok(key.and_then(|key| guard.remove(&key)).is_some())
    }

    async fn delete_by_node(&self, space_node: &SpaceNode) -> Result<bool, SpacesMetadataError> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|error| SpacesMetadataError::Storage(error.to_string()))?;
        Ok(guard.remove(space_node.as_str()).is_some())
    }

    async fn list_all(&self) -> Result<Vec<SpaceMetadata>, SpacesMetadataError> {
        let guard = self
            .rows
            .lock()
            .map_err(|error| SpacesMetadataError::Storage(error.to_string()))?;
        let mut rows: Vec<SpaceMetadata> = guard.values().cloned().collect();
        rows.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.space_node.cmp(&b.space_node))
        });
        Ok(rows)
    }
}
