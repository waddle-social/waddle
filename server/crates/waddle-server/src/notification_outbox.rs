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
pub(crate) mod runtime_attach;
#[cfg(all(test, feature = "test-support"))]
#[path = "notification_outbox/runtime_attach_tests.rs"]
mod runtime_attach_tests;
mod schema;
pub(crate) const fn outbox_schema_gate() -> (i32, i32) {
    schema::OUTBOX_SCHEMA_GATE
}
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

/// Proof that this exact URL-opened owner pool completed the legacy outbox
/// initializer before its admission lanes were sealed. It holds no pool.
#[derive(Debug)]
pub(crate) struct PreparedLegacyOwnerOutbox {
    pool_identity: std::sync::Weak<()>,
}

#[derive(Clone)]
struct OwnerSchemaIdentity {
    oid: i64,
    name: String,
    search_path_json: String,
    temp_schema_oid: i64,
}

#[derive(Clone)]
struct OwnerPreparationProof {
    identity: Option<crate::db::lineage::PgIdentity>,
    probe_key: (i32, i32),
    schema_identity: std::sync::Arc<tokio::sync::Mutex<Option<OwnerSchemaIdentity>>>,
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

    pub(crate) async fn prepare_legacy_owner_shared(
        db: &Database,
    ) -> Result<PreparedLegacyOwnerOutbox, NotificationOutboxError> {
        if db.driver() != crate::db::DatabaseDriver::Postgres
            || !db.is_main_lane()
            || db.ingress_pool_size().is_some()
            || db.uses_attested_postgres_pool()
        {
            return Err(NotificationOutboxError::SharedOwnerMainLaneRequired);
        }
        let identity = db
            .owner_startup_identity()
            .ok_or(NotificationOutboxError::SharedOwnerMainLaneRequired)?;
        let temporary = Self {
            db: db.clone(),
            owner_preparation: None,
        };
        temporary.initialize_legacy_postgres_inline().await?;
        drop(temporary);
        Ok(PreparedLegacyOwnerOutbox {
            pool_identity: identity,
        })
    }

    /// Bind the already prepared legacy outbox to sealed owner admission.
    /// The only production sealing path completes the legacy initializer before it
    /// publishes the pool. This check performs no owner DDL or adoption.
    pub(crate) async fn attach_prepared_shared_owner(
        db: Database,
        prepared: PreparedLegacyOwnerOutbox,
    ) -> Result<Self, NotificationOutboxError> {
        if db.driver() != crate::db::DatabaseDriver::Postgres
            || !db.is_main_lane()
            || db.ingress_pool_size().is_none()
            || db.uses_attested_postgres_pool()
        {
            return Err(NotificationOutboxError::SharedOwnerMainLaneRequired);
        }
        let identity = db
            .owner_startup_identity()
            .ok_or(NotificationOutboxError::SharedOwnerMainLaneRequired)?;
        if !std::sync::Weak::ptr_eq(&identity, &prepared.pool_identity) {
            return Err(NotificationOutboxError::SharedOwnerMainLaneRequired);
        }
        let store = Self {
            db,
            owner_preparation: None,
        };
        store.validate_typed_schema_contract()?;
        let mut tx = store.db.begin().await?;
        tx.execute(
            "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ ONLY",
            (),
        )
        .await?;
        let mut rows = tx
            .query("SELECT pg_catalog.current_schema()::text", ())
            .await?;
        let schema = rows
            .next()
            .await?
            .ok_or(NotificationOutboxError::SharedOwnerMainLaneRequired)?
            .get::<String>(0)?;
        drop(rows);
        crate::db::physical_attestation::validate_identifier(
            &schema,
            "legacy notification outbox schema",
        )
        .map_err(|_| NotificationOutboxError::UnsafeOwnerBoundary)?;
        let qualified = crate::db::physical_attestation::quote_identifier(&schema);
        for table in [
            "notification_candidates",
            "notification_outbox_lineage",
            "notification_outbox",
        ] {
            let sql = format!("SELECT 1 FROM {qualified}.\"{table}\" LIMIT 0");
            let rows = tx.query(&sql, ()).await?;
            drop(rows);
        }
        tx.rollback().await?;
        Ok(store)
    }

    pub(crate) fn belongs_to_shared_owner_main(&self, main: &Database) -> bool {
        self.db.is_main_lane()
            && self.db.ingress_pool_size().is_some()
            && self
                .db
                .owner_startup_identity()
                .zip(main.owner_startup_identity())
                .is_some_and(|(left, right)| std::sync::Weak::ptr_eq(&left, &right))
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
            identity: Some(identity.clone()),
            probe_key: fresh_owner_probe_key(),
            schema_identity: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
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

    /// Hold the outbox owner gate while a second owner phase relies on the
    /// completed preparation marker. The caller must keep `tx` open through
    /// its dependent writes so legacy outbox initialization cannot invalidate
    /// the marker between validation and adoption.
    pub(crate) async fn verify_owner_preparation_complete_tx(
        tx: &mut crate::db::Transaction<'_>,
        identity: &crate::db::lineage::PgIdentity,
    ) -> Result<(), NotificationOutboxRuntimeAttachError> {
        runtime_attach::verify_owner_preparation_complete_tx(tx, identity).await
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
