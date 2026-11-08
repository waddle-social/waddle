//! Standalone, post-effect receipt writes on one previously selected lane.

use std::sync::Arc;

use waddle_xmpp::ingress::MessageKey;

use crate::db::Database;
use crate::ingress::EffectReceiptKey;
use crate::ingress_uow::{EffectReceiptRepository, IngressUowError};

/// A single-receipt writer bound to the exact existing database handle.
/// Clones share that handle; they do not open or select another pool.
#[derive(Clone)]
pub(crate) struct StandaloneReceiptWriter {
    database: Arc<Database>,
}

impl StandaloneReceiptWriter {
    pub(crate) fn new(database: Database) -> Self {
        Self {
            database: Arc::new(database),
        }
    }

    pub(crate) fn from_shared(database: Arc<Database>) -> Self {
        Self { database }
    }

    pub(crate) async fn record(
        &self,
        message: MessageKey,
        receipt: &EffectReceiptKey,
    ) -> Result<(), IngressUowError> {
        EffectReceiptRepository::record_receipt_pooled(
            &self.database,
            message,
            receipt.kind,
            &receipt.semantic_identity_hash,
        )
        .await
    }

    pub(crate) fn has_attested_writer(&self) -> bool {
        self.database.attested_pg_writer_capability().is_some()
    }

    #[cfg(feature = "test-support")]
    pub(crate) fn shares_ingress_lane(&self, database: &Database) -> bool {
        self.database.shares_ingress_lane(database)
    }

    #[cfg(feature = "test-support")]
    pub(crate) fn same_writer(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.database, &other.database)
    }
}
