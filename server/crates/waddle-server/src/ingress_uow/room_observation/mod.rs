//! Durable, per-room observation work and result publication.
//!
//! Every mutation here runs inside the caller's ingress unit of work. Source
//! revisions, leased work, receipts, and outbound publications share one
//! commit boundary with the room archive and never depend on its GC lifetime.

mod observation_body;
mod publications;
mod retention;
mod schema;
mod sources;
mod work;

#[cfg(test)]
mod tests;

use uuid::Uuid;
use waddle_extensions::{
    ConfiguredRoomObserver, DisplayText, ExtensionPayload, RoomMessageSource,
    RoomObservationSubscription,
};
use waddle_xmpp::ingress::{DeliveryKey, IngressEffectKey, MessageKey};

pub(crate) use retention::ObserverRetentionBatch;
pub use schema::initialize_room_observations;

/// No provider body, message text, key, JID, or raw SQL diagnostic is exposed
/// through this error's `Display` or `Debug` representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ObservationError {
    #[error("room observation database operation failed")]
    Database,
    #[error("room observation transaction requires a retry")]
    RetryableDatabase(crate::ingress_uow::DbRetryClass),
    #[error("room observation database operation timed out")]
    Timeout,
    #[error("room observation node authority is no longer current")]
    AuthorityStopped,
    #[error("stored room observation data is malformed")]
    Codec,
    #[error("room observation generation has conflicting identity")]
    IdentityConflict,
    #[error("room observation generation has conflicting configuration")]
    ConfigurationConflict,
    #[error("room observation generation is out of range")]
    GenerationOutOfRange,
    #[error("room observation source identity conflicts with a stored source")]
    SourceConflict,
    #[error("room observation publication identity conflicts with a stored result")]
    PublicationConflict,
}

impl From<crate::db::DatabaseError> for ObservationError {
    fn from(error: crate::db::DatabaseError) -> Self {
        let class = crate::ingress_uow::DbRetryClass::from_database_error(&error);
        if class == crate::ingress_uow::DbRetryClass::NotRetryable {
            Self::Database
        } else {
            Self::RetryableDatabase(class)
        }
    }
}

impl From<crate::ingress_uow::IngressUowError> for ObservationError {
    fn from(error: crate::ingress_uow::IngressUowError) -> Self {
        let class = error.retry_class();
        if class != crate::ingress_uow::DbRetryClass::NotRetryable {
            Self::RetryableDatabase(class)
        } else if matches!(error, crate::ingress_uow::IngressUowError::Timeout) {
            Self::Timeout
        } else {
            Self::Database
        }
    }
}

impl From<crate::ingress_substrate::IngressSubstrateError> for ObservationError {
    fn from(error: crate::ingress_substrate::IngressSubstrateError) -> Self {
        crate::ingress_uow::IngressUowError::from(error).into()
    }
}

impl From<serde_json::Error> for ObservationError {
    fn from(_: serde_json::Error) -> Self {
        Self::Codec
    }
}

#[derive(Clone)]
pub struct ObservationWork {
    pub id: Uuid,
    pub lease: Uuid,
    pub message_key: MessageKey,
    pub subscription: RoomObservationSubscription,
    pub source: RoomMessageSource,
    pub body: DisplayText,
    pub attempt: u32,
    owner: waddle_xmpp::ownership::NodeIdentity,
}

impl ObservationWork {
    pub fn effect_key(&self) -> IngressEffectKey {
        IngressEffectKey::RoomObserver(
            self.subscription.room.clone(),
            self.subscription.plugin.clone(),
            self.subscription.generation,
            self.subscription.identity.clone(),
        )
    }

    pub fn delivery_key(&self) -> DeliveryKey {
        DeliveryKey::effect(self.message_key, &self.effect_key())
    }
}

pub(crate) struct CapturedRoomSource<'a> {
    pub key: MessageKey,
    pub room: &'a jid::BareJid,
    pub message: &'a xmpp_parsers::message::Message,
    pub sender: &'a jid::BareJid,
    pub intents: &'a [waddle_xmpp::ingress::IngressEffectIntent],
    pub observed_at: chrono::DateTime<chrono::Utc>,
    pub correction_target: Option<&'a waddle_xmpp_core::xep0359::StanzaId>,
}

#[derive(Debug, Clone)]
pub struct RoomPublication {
    pub id: Uuid,
    pub subscription: RoomObservationSubscription,
    pub source: RoomMessageSource,
    pub payload: ExtensionPayload,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct RoomObservationRepository;

impl RoomObservationRepository {
    pub async fn sync_configured(
        tx: &mut super::IngressUowTransaction<'_>,
        configured: &[ConfiguredRoomObserver],
        now_ms: i64,
    ) -> Result<(), ObservationError> {
        sources::sync_configured(tx, configured, now_ms).await
    }

    pub(crate) async fn capture(
        tx: &mut super::IngressUowTransaction<'_>,
        source: CapturedRoomSource<'_>,
    ) -> Result<(), ObservationError> {
        sources::capture(tx, source).await
    }

    pub async fn retract(
        tx: &mut super::IngressUowTransaction<'_>,
        room: &jid::BareJid,
        target: &waddle_xmpp_core::xep0359::StanzaId,
        now_ms: i64,
    ) -> Result<(), ObservationError> {
        sources::retract(tx, room, target, now_ms).await
    }

    pub async fn claim(
        tx: &mut super::IngressUowTransaction<'_>,
        subscription: &RoomObservationSubscription,
        now_ms: i64,
    ) -> Result<Option<ObservationWork>, ObservationError> {
        work::claim(tx, subscription, now_ms).await
    }

    /// Commit a successful start before invoking the callback. Started work
    /// may be reclaimed after lease expiry with a fresh token. A repeated
    /// guest effect is possible; stale tokens cannot publish results.
    pub async fn start(
        tx: &mut super::IngressUowTransaction<'_>,
        work: &ObservationWork,
        now_ms: i64,
    ) -> Result<bool, ObservationError> {
        work::start(tx, work, now_ms).await
    }

    /// Every guest operation and its returned result rechecks the exact started
    /// lease, current node incarnation, configured subscription, and source.
    pub async fn validate_started(
        tx: &mut super::IngressUowTransaction<'_>,
        work: &ObservationWork,
        now_ms: i64,
    ) -> Result<bool, ObservationError> {
        work::validate_started(tx, work, now_ms).await
    }

    pub async fn finish(
        tx: &mut super::IngressUowTransaction<'_>,
        work: &ObservationWork,
        outcome: &waddle_extensions::RoomObservationOutcome,
        now_ms: i64,
    ) -> Result<bool, ObservationError> {
        work::finish(tx, work, outcome, now_ms).await
    }

    pub async fn due_rooms(
        tx: &mut super::IngressUowTransaction<'_>,
        observer: &ConfiguredRoomObserver,
        after: Option<&jid::BareJid>,
        now_ms: i64,
        limit: u32,
    ) -> Result<Vec<jid::BareJid>, ObservationError> {
        work::due_rooms(tx, observer, after, now_ms, limit).await
    }

    pub async fn publication(
        tx: &mut super::IngressUowTransaction<'_>,
        subscription: &RoomObservationSubscription,
        now_ms: i64,
    ) -> Result<Option<RoomPublication>, ObservationError> {
        publications::publication(tx, subscription, now_ms).await
    }

    pub async fn assert_publication(
        tx: &mut super::IngressUowTransaction<'_>,
        publication: &RoomPublication,
    ) -> Result<bool, ObservationError> {
        publications::assert_publication(tx, publication).await
    }

    pub async fn mark_published(
        tx: &mut super::IngressUowTransaction<'_>,
        id: &Uuid,
        now_ms: i64,
    ) -> Result<bool, ObservationError> {
        publications::mark_published(tx, id, now_ms).await
    }

    /// Delete one bounded batch of settled observer history older than
    /// [`retention::OBSERVER_HISTORY_RETENTION`] (#1901). `limit` bounds physical row
    /// deletions across every table. Active work, pending publications, and
    /// anything still evidence for a non-terminal canonical row survive.
    pub(crate) async fn collect_expired(
        tx: &mut crate::db::Transaction<'_>,
        now_ms: i64,
        limit: u32,
    ) -> Result<ObserverRetentionBatch, ObservationError> {
        retention::collect_expired(tx, now_ms, limit).await
    }
}
