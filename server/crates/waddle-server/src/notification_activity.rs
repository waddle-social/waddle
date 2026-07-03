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

use async_trait::async_trait;
use jid::BareJid;
use thiserror::Error;

use crate::db::{Database, DatabaseError, IntoParams};
mod schema;
mod store;
mod types;

pub use types::{
    NoopActivityReader, NotificationActivity, NotificationActivityError,
    NotificationActivityReader, NotificationChatState, NotificationPresenceShow,
};

#[derive(Clone)]
pub struct NotificationActivityStore {
    db: Database,
}

impl NotificationActivityStore {
    pub async fn new(db: Database) -> Result<Self, NotificationActivityError> {
        let store = Self { db };
        store.initialize().await?;
        Ok(store)
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
    ) -> Result<crate::db::Rows, NotificationActivityError> {
        let conn = self.db.guard().await?;
        Ok(conn.query(sql, params).await?)
    }
}
