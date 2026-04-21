//! In-memory `StanzaStore` used by unit tests.

use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;

use crate::message::{ArchivedMessage, MamQuery};
use crate::store::{StanzaStore, StoreError};

#[derive(Default)]
pub struct MockStore {
    pub writes: AtomicU64,
    pub reads: AtomicU64,
}

impl MockStore {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn writes(&self) -> u64 {
        self.writes.load(Ordering::Relaxed)
    }
    pub fn reads(&self) -> u64 {
        self.reads.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl StanzaStore for MockStore {
    async fn init(&self) -> Result<(), StoreError> {
        Ok(())
    }
    async fn store_message(&self, _m: &ArchivedMessage) -> Result<(), StoreError> {
        self.writes.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
    async fn query_messages(&self, _q: &MamQuery) -> Result<Vec<ArchivedMessage>, StoreError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        Ok(Vec::new())
    }
    async fn count_messages(&self, _room_jid: &str) -> Result<u64, StoreError> {
        Ok(self.writes.load(Ordering::Relaxed))
    }
}
