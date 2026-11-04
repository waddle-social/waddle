//! Durable per-(user, conversation) activity projection backing the
//! XEP-0513 `<active/>` push filter.
//!
//! The projection records, for each conversation a user participates
//! in, the most recent typed signal that the user was *currently
//! active*: a XEP-0085 chat-state update, a XEP-0490 read-marker
//! advance, an outbound message commit, or a XEP-0045 presence
//! join/leave. The T1 push-gate evaluator consults this projection to
//! suppress `ActiveChannelMention` candidates whose recipient is no
//! longer recent enough to satisfy the XEP-0513 `<active/>` filter.
//!
//! Slice 2b — wires
//! [`crate::notification_outbox::SuppressedReason::Xep0513ActiveMiss`]
//! (the reserved variant from slice 2a) to the evaluator and lands the
//! projection store + reader trait + writer surface used by the
//! ingestion sites (XEP-0085 / XEP-0490 / outbound commit / XEP-0045).
//!
//! Cold-start expectation: on first deploy the projection table is
//! empty. Every [`crate::notification_outbox::NotificationClass::ActiveChannelMention`]
//! candidate that reaches the T1 drain will suppress with
//! [`crate::notification_outbox::SuppressedReason::Xep0513ActiveMiss`]
//! until users start sending chat-states, advancing read markers,
//! committing outbound messages, or emitting MUC presence. The metric
//! `waddle_push_suppressed_total{reason="xep0513_active_miss"}` will
//! therefore ramp up from zero to a baseline as the projection fills.
//! That is expected behavior, not a regression.

use crate::db::Rows;
use async_trait::async_trait;
use jid::BareJid;
use thiserror::Error;

use crate::db::{Database, DatabaseError, IntoParams};
pub(crate) mod runtime_attach;
mod schema;
mod store;
mod types;

pub use runtime_attach::DurableStoreAttachError;

#[cfg(all(test, feature = "test-support"))]
#[path = "notification_activity/runtime_attach_tests.rs"]
mod runtime_attach_tests;

pub use types::{
    NoopActivityReader, NotificationActivity, NotificationActivityError,
    NotificationActivityReader, NotificationChatState, NotificationPresenceShow,
};

#[derive(Clone)]
pub struct NotificationActivityStore {
    db: Database,
}

impl NotificationActivityStore {
    pub async fn try_new(db: Database) -> Result<Self, NotificationActivityError> {
        Self::build_initialized(db).await
    }

    /// Initialize the activity projection through this exact owner database
    /// handle. Existing callers retain the historical bootstrap behavior via
    /// [`Self::try_new`].
    pub async fn initialize_owner(db: Database) -> Result<Self, NotificationActivityError> {
        if db.attested_pg_writer_capability().is_some() {
            return Err(NotificationActivityError::OwnerBootstrapRequiresOwnerHandle);
        }
        Self::build_initialized(db).await
    }

    async fn build_initialized(db: Database) -> Result<Self, NotificationActivityError> {
        let store = Self { db };
        store.initialize().await?;
        Ok(store)
    }

    /// Attach to an owner-prepared PostgreSQL table through the exact
    /// physically attested main-pool handle. This path is read-only and does
    /// not reopen a URL or apply schema DDL.
    pub async fn attach_existing(db: Database) -> Result<Self, DurableStoreAttachError> {
        runtime_attach::verify_attested_table(&db, &runtime_attach::ACTIVITY_CONTRACT).await?;
        Ok(Self { db })
    }

    async fn execute(
        &self,
        sql: &str,
        params: impl IntoParams,
    ) -> Result<u64, NotificationActivityError> {
        let conn = self.db.guard().await?;
        Ok(conn.execute(sql, params).await?)
    }

    async fn query(
        &self,
        sql: &str,
        params: impl IntoParams,
    ) -> Result<Rows, NotificationActivityError> {
        let conn = self.db.guard().await?;
        Ok(conn.query(sql, params).await?)
    }
}
