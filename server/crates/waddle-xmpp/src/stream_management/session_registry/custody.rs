//! Durable ingress custody is independent of the bounded SM replay cache.

use crate::stream_management::persistence::{
    IngressCustodyDisposition, PersistedIngressAppend, SmPersistenceStorage,
};
use crate::{pending_delivery::SmSessionId, stream_management::SmIngressAppendKey};

use super::{InMemorySmSessionRegistry, SmRegistryError};

impl InMemorySmSessionRegistry {
    fn custody_storage(&self) -> Result<&dyn SmPersistenceStorage, SmRegistryError> {
        self.persistence
            .as_deref()
            .ok_or(SmRegistryError::StorageUnavailable(
                super::traits::StorageOutageCause::Backend,
            ))
    }

    /// Check immutable custody before dispatching a retry to a live resource.
    pub async fn get_ingress_append(
        &self,
        key: &SmIngressAppendKey,
    ) -> Result<Option<PersistedIngressAppend>, SmRegistryError> {
        let Some(storage) = self.persistence.as_deref() else {
            return Ok(None);
        };
        storage
            .get_ingress_append(key)
            .await
            .map_err(|error| SmRegistryError::Internal(error.to_string()))
    }

    pub async fn get_ingress_appends_for_sequence(
        &self,
        stream: &SmSessionId,
        sequence: u32,
    ) -> Result<Vec<PersistedIngressAppend>, SmRegistryError> {
        let Some(storage) = self.persistence.as_deref() else {
            return Ok(Vec::new());
        };
        storage
            .get_ingress_appends_for_sequence(stream, sequence)
            .await
            .map_err(|error| SmRegistryError::Internal(error.to_string()))
    }

    pub async fn list_pending_ingress_appends(
        &self,
        limit: usize,
    ) -> Result<Vec<PersistedIngressAppend>, SmRegistryError> {
        let Some(storage) = self.persistence.as_deref() else {
            return Ok(Vec::new());
        };
        storage
            .list_pending_ingress_appends(limit)
            .await
            .map_err(|error| SmRegistryError::Internal(error.to_string()))
    }

    pub async fn list_pending_ingress_appends_after(
        &self,
        after: Option<&SmIngressAppendKey>,
        limit: usize,
    ) -> Result<Vec<PersistedIngressAppend>, SmRegistryError> {
        let Some(storage) = self.persistence.as_deref() else {
            return Ok(Vec::new());
        };
        storage
            .list_pending_ingress_appends_after(after, limit)
            .await
            .map_err(|error| SmRegistryError::Internal(error.to_string()))
    }

    pub async fn complete_ingress_append(
        &self,
        append: &PersistedIngressAppend,
        disposition: IngressCustodyDisposition,
    ) -> Result<bool, SmRegistryError> {
        self.custody_storage()?
            .complete_ingress_append(
                &append.key,
                &append.accepting_stream,
                append.sequence,
                disposition,
            )
            .await
            .map_err(|error| SmRegistryError::Internal(error.to_string()))
    }

    pub async fn complete_ingress_appends_through(
        &self,
        stream: &SmSessionId,
        from_exclusive: u32,
        h: u32,
    ) -> Result<(), SmRegistryError> {
        let Some(storage) = self.persistence.as_deref() else {
            return Ok(());
        };
        storage
            .complete_ingress_appends_through(stream, from_exclusive, h)
            .await
            .map_err(|error| SmRegistryError::Internal(error.to_string()))
    }
}
