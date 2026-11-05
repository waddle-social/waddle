//! Durable user-server notification candidates and XEP-0357 outbox.
//!
//! This is scheduling/coalescing state. The canonical first-party XEP-0357
//! payload still becomes a XEP-0060 PubSub item on the Push Service boundary.

use crate::db::Rows;
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
mod direct_envelope;
pub(crate) use direct_envelope::direct_candidate_from_envelope;
mod envelope;
pub(crate) mod groupchat_class;
pub use envelope::{candidate_from_envelope, GroupchatCandidateIdentity};
mod codec;
mod deps;
mod drain;
mod enqueue;
mod gate;
mod legacy_ancestry;
mod observability;
mod payload;
mod prune;
mod publish;
mod runtime_attach;
#[cfg(all(test, feature = "test-support"))]
#[path = "notification_outbox/runtime_attach_tests.rs"]
mod runtime_attach_tests;
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
pub use observability::PushOutboxQueueObservation;
pub use payload::{
    build_xep0357_notification_payload, publish_options_form_type_is_xep0060,
    target_from_subscription, RichSummary, WADDLE_PUSH_CONTEXT_NS, XEP0357_SUMMARY_FORM_TYPE,
};
pub(crate) use publish::retire_registration_outbox_tx;
pub use runtime_attach::NotificationOutboxRuntimeAttachError;
pub(crate) use types::SuppressedReason;
pub use types::{
    NotificationCandidateInsertOutcome, NotificationClass, NotificationOutboxError,
    NotificationOutboxJob, NotificationOutboxJobId, NotificationOutboxPruneOutcome,
    NotificationOutboxPublishOutcome, NotificationOutboxStatus, NotificationOutboxTarget,
    NotificationReason, NotificationThreadId, PushServiceNodeName,
};

// Internal wiring: children import shared sibling items through this facade.
use candidate::{
    require_full_sender_jid, require_full_sender_jid_set, require_sender_matches_conversation,
    require_sender_set_contains_scalar, require_sender_set_matches_conversation,
};
use codec::{decode_candidate, decode_outbox_job, decode_sender_jids, encode_sender_jids};
use gate::{xep0191_blocks_notification_candidate, xep0191_blocks_notification_job};
use payload::build_waddle_context;
use publish::{lock_outbox_ancestry_tx, policy_retry_delay_ms, settle_outbox_if_unowned_tx};
use types::{STATUS_FAILED, STATUS_IN_PROGRESS, STATUS_PUBLISHED, STATUS_QUEUED};

#[derive(Clone)]
pub struct NotificationOutboxStore {
    db: Database,
    owner_preparation: Option<OwnerPreparationProof>,
}

#[derive(Clone)]
struct OwnerPreparationProof {
    identity: crate::db::lineage::PgIdentity,
    probe_key: (i32, i32),
}

impl NotificationOutboxStore {
    pub async fn try_new(db: Database) -> Result<Self, NotificationOutboxError> {
        let store = Self {
            db,
            owner_preparation: None,
        };
        store.initialize().await?;
        Ok(store)
    }

    /// Bootstrap or upgrade the notification outbox with a trusted owner
    /// handle. PostgreSQL preparation serializes schema changes, scheduler-ID
    /// assignment, and legacy ancestry adoption under the outbox owner gate.
    /// Restricted runtime handles must use [`Self::attach_existing`].
    pub async fn prepare_owner(
        db: &Database,
        identity: &crate::db::lineage::PgIdentity,
    ) -> Result<Self, NotificationOutboxError> {
        if db.driver() != crate::db::DatabaseDriver::Postgres {
            return Self::try_new(db.clone()).await;
        }
        if db.uses_attested_postgres_pool() {
            return Err(NotificationOutboxError::OwnerPreparationRequiresTrustedHandle);
        }
        let store = Self {
            db: db.clone(),
            owner_preparation: None,
        };
        store.validate_typed_schema_contract()?;
        let mut preparing = store.clone();
        preparing.owner_preparation = Some(OwnerPreparationProof {
            identity: identity.clone(),
            probe_key: fresh_owner_probe_key(),
        });
        tokio::spawn(async move {
            preparing
                .initialize_postgres_prepared_owner(schema::OUTBOX_SCHEMA_GATE_WAIT)
                .await
        })
        .await
        .map_err(|error| {
            DatabaseError::QueryFailed(format!("outbox owner preparation task failed: {error}"))
        })??;
        Ok(store)
    }

    /// Attach a restricted, physically attested PostgreSQL main-pool handle
    /// to an owner-prepared outbox. This performs read-only validation only.
    pub async fn attach_existing(
        db: Database,
    ) -> Result<Self, NotificationOutboxRuntimeAttachError> {
        runtime_attach::attach_existing(db).await
    }

    async fn execute(
        &self,
        sql: &str,
        params: impl IntoParams,
    ) -> Result<u64, NotificationOutboxError> {
        if self.owner_preparation.is_some() {
            let mut tx = self.db.begin().await?;
            self.verify_owner_transaction(&mut tx).await?;
            let affected = tx.execute(sql, params).await?;
            tx.commit().await?;
            return Ok(affected);
        }
        let conn = self.db.guard().await?;
        Ok(conn.execute(sql, params).await?)
    }

    async fn query(
        &self,
        sql: &str,
        params: impl IntoParams,
    ) -> Result<Rows, NotificationOutboxError> {
        if self.owner_preparation.is_some() {
            let mut tx = self.db.begin().await?;
            self.verify_owner_transaction(&mut tx).await?;
            let rows = tx.query(sql, params).await?;
            tx.commit().await?;
            return Ok(rows);
        }
        let conn = self.db.guard().await?;
        Ok(conn.query(sql, params).await?)
    }
}

fn fresh_owner_probe_key() -> (i32, i32) {
    let bytes = uuid::Uuid::new_v4().into_bytes();
    let key = (
        i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        i32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
    );
    if key == schema::OUTBOX_SCHEMA_GATE {
        fresh_owner_probe_key()
    } else {
        key
    }
}
