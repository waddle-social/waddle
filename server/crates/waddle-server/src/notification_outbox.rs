//! Durable user-server notification candidates and XEP-0357 outbox.
//!
//! This is scheduling/coalescing state. The canonical first-party XEP-0357
//! payload still becomes a XEP-0060 PubSub item on the Push Service boundary.

use jid::{BareJid, Jid};
use minidom::Element;
use thiserror::Error;
use waddle_xmpp::inbox::storage::InboxStorage;
use waddle_xmpp::pubsub::PubSubItem;
use waddle_xmpp::push::PushSubscriptionStore;
use waddle_xmpp::xep::xep0191::{BlockingStorage, BlockingStorageError};
use waddle_xmpp::xep::{NS_DATA_FORMS, NS_PUBSUB_PUBLISH_OPTIONS};
use waddle_xmpp_core::xep0359::StanzaId;

use crate::db::{Database, DatabaseError, IntoParams, Row};
use crate::notification_activity::{
    NotificationActivity, NotificationActivityError, NotificationActivityReader,
};
mod candidate;
mod codec;
mod deps;
mod drain;
mod enqueue;
mod gate;
mod payload;
mod prune;
mod publish;
mod schema;
#[cfg(test)]
mod test_support;
mod types;

pub use candidate::{NotificationCandidate, NotificationMessageHints};
pub use deps::{
    active_mention_ttl_ms_from_env, DndReader, DndState, NoopDndReader, NoopRoomPolicy,
    NotificationDrainDeps, RoomPolicyStore, DEFAULT_ACTIVE_MENTION_TTL_SECONDS,
    MAX_ACTIVE_MENTION_TTL_SECONDS, MIN_ACTIVE_MENTION_TTL_SECONDS,
    WADDLE_PUSH_ACTIVE_MENTION_TTL_ENV,
};
pub(crate) use gate::{
    evaluate_push_gate_at_dispatch, PushEvalCaches, PushEvalDeps, PushEvalStage,
    RoomPolicyCacheEntry, T1PushDispatchOutcome,
};
pub use payload::{
    build_xep0357_notification_payload, publish_options_form_type_is_xep0060,
    target_from_subscription, RichSummary, WADDLE_PUSH_CONTEXT_NS, XEP0357_SUMMARY_FORM_TYPE,
};
pub(crate) use types::SuppressedReason;
pub use types::{
    NotificationCandidateInsertOutcome, NotificationClass, NotificationOutboxError,
    NotificationOutboxJob, NotificationOutboxJobId, NotificationOutboxPruneOutcome,
    NotificationOutboxPublishOutcome, NotificationOutboxStatus, NotificationOutboxTarget,
    NotificationReason, NotificationThreadId, PushServiceNodeName,
};

// Internal wiring: children reference sibling items through `use super::*`,
// which resolves via these module-private glob imports.
use candidate::*;
use codec::*;
use gate::*;
use payload::*;
use publish::*;
use types::*;

#[derive(Clone)]
pub struct NotificationOutboxStore {
    db: Database,
}

impl NotificationOutboxStore {
    pub async fn new(db: Database) -> Result<Self, NotificationOutboxError> {
        let store = Self { db };
        store.initialize().await?;
        Ok(store)
    }

    async fn execute(
        &self,
        sql: &str,
        params: impl IntoParams,
    ) -> Result<u64, NotificationOutboxError> {
        let conn = self.db.guard().await?;
        Ok(conn.execute(sql, params).await?)
    }

    async fn query(
        &self,
        sql: &str,
        params: impl IntoParams,
    ) -> Result<crate::db::Rows, NotificationOutboxError> {
        let conn = self.db.guard().await?;
        Ok(conn.query(sql, params).await?)
    }
}
