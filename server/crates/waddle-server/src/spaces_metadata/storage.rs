//! In-memory [`SpacesMetadataStore`] for tests and as the placeholder
//! `AppState` dependency in unit-test constructors.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use jid::BareJid;

use super::{SpaceMetadata, SpacesMetadataError, SpacesMetadataStore};

#[derive(Debug, Default)]
pub struct InMemorySpacesMetadataStore {
    rows: Mutex<HashMap<BareJid, SpaceMetadata>>,
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
        Ok(guard.get(space_jid).cloned())
    }

    async fn upsert(&self, metadata: &SpaceMetadata) -> Result<(), SpacesMetadataError> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|error| SpacesMetadataError::Storage(error.to_string()))?;
        // Preserve created_at on an existing row so callers can supply
        // it freely on `upsert`; the trait contract says created_at is
        // assigned at first insert.
        match guard.get(&metadata.space_jid) {
            Some(existing) => {
                let preserved_created_at = existing.created_at;
                guard.insert(
                    metadata.space_jid.clone(),
                    SpaceMetadata {
                        created_at: preserved_created_at,
                        ..metadata.clone()
                    },
                );
            }
            None => {
                guard.insert(metadata.space_jid.clone(), metadata.clone());
            }
        }
        Ok(())
    }

    async fn delete(&self, space_jid: &BareJid) -> Result<bool, SpacesMetadataError> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|error| SpacesMetadataError::Storage(error.to_string()))?;
        Ok(guard.remove(space_jid).is_some())
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
                .then_with(|| a.space_jid.cmp(&b.space_jid))
        });
        Ok(rows)
    }
}
