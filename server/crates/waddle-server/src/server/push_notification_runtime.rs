//! Diagnostic attachment of the Push and notification store family.
//!
//! This composes the existing read-only PostgreSQL attachment checks on one
//! supplied, physically attested main database handle. It is deliberately not
//! wired into server startup: a successful result is diagnostic evidence only,
//! not a serving-readiness certificate. In particular, this module does not
//! start workers, register commands, load provider credentials, adopt ancestry,
//! initialize schemas, or provision keys.

use std::sync::Arc;

use jid::BareJid;
use thiserror::Error;
use waddle_xmpp::pubsub::PubSubStorage;

use crate::db::{Database, DatabaseDriver, DatabaseError};
use crate::dnd_projection::DndProjectionStore;
use crate::dnd_reader::PepDndReader;
use crate::notification_activity::{DurableStoreAttachError, NotificationActivityStore};
use crate::notification_outbox::{NotificationOutboxRuntimeAttachError, NotificationOutboxStore};
use crate::notification_settings_projection::NotificationSettingsProjectionStore;
use crate::pubsub::{DatabasePubSubStorage, PubSubRuntimeAttachError};
use crate::push_registrations::DatabasePushRegistrationStore;
use crate::push_service::vapid_storage::{VapidRuntimeAttachError, VapidStorage};
use crate::push_service::{DatabasePushServiceStore, PushServiceAttachError};

/// Failure to inspect one member of the Push and notification store family.
#[derive(Debug, Error)]
pub enum PushNotificationRuntimeAttachError {
    #[error("Push and notification attachment requires PostgreSQL")]
    PostgresRequired,
    #[error("Push and notification attachment requires the main database lane")]
    MainLaneRequired,
    #[error("Push and notification attachment requires a physically attested database")]
    AttestedPoolRequired,
    #[error("Push and notification attachment received an invalid pool capability")]
    InvalidPoolCapability(#[source] DatabaseError),
    #[error("failed to attach PubSub for Push and notification inspection: {0}")]
    PubSub(#[source] PubSubRuntimeAttachError),
    #[error("failed to attach XEP-0357 registrations: {0}")]
    PushRegistrations(#[source] DurableStoreAttachError),
    #[error("failed to attach Push Service storage: {0}")]
    PushService(#[source] PushServiceAttachError),
    #[error("failed to attach the notification outbox: {0}")]
    NotificationOutbox(#[source] NotificationOutboxRuntimeAttachError),
    #[error("failed to attach notification activity: {0}")]
    NotificationActivity(#[source] DurableStoreAttachError),
    #[error("failed to attach the prepared VAPID signer: {0}")]
    Vapid(#[source] VapidRuntimeAttachError),
}

/// Opaque result for a dark, read-only attachment pass.
///
/// The store handles are intentionally not returned: this check does not
/// establish the joint owner/runtime ACL and old-writer conditions required
/// before serving can be enabled.
pub struct PushNotificationRuntimeAttachEvidence {
    _database: Database,
}

/// Read-only attachment check for already prepared Push and notification
/// stores. Each store is attached sequentially through a clone of `database`,
/// so every component verifies the same exact attested main pool. PubSub is
/// attached from that handle here; the settings and DND projections are then
/// constructed from PubSub's database handle rather than opening another pool.
///
/// The prepared VAPID signer is also loaded through its read-only attachment
/// API. The existing `load_or_provision` and `load_or_ephemeral` paths are
/// intentionally excluded because both initialize the VAPID schema, and the
/// former can also provision a key.
pub async fn verify_attested_push_notification_stores(
    database: Database,
    secret_key: &[u8],
    push_service_jid: BareJid,
) -> Result<PushNotificationRuntimeAttachEvidence, PushNotificationRuntimeAttachError> {
    if database.driver() != DatabaseDriver::Postgres {
        return Err(PushNotificationRuntimeAttachError::PostgresRequired);
    }
    if !database.is_main_lane() {
        return Err(PushNotificationRuntimeAttachError::MainLaneRequired);
    }
    let capability = database
        .attested_pg_writer_capability()
        .ok_or(PushNotificationRuntimeAttachError::AttestedPoolRequired)?;
    database
        .verify_attested_pg_writer_capability(&capability)
        .map_err(DatabaseError::from)
        .map_err(PushNotificationRuntimeAttachError::InvalidPoolCapability)?;

    // PubSub's attachment validates the settings and DND projection tables as
    // part of its owner-prepared schema contract.
    let pubsub = Arc::new(
        DatabasePubSubStorage::attach_existing(database.clone())
            .await
            .map_err(PushNotificationRuntimeAttachError::PubSub)?,
    );
    let pubsub_boundary: Arc<dyn PubSubStorage> = pubsub.clone();
    let projection_database = pubsub.database();
    let _notification_settings =
        NotificationSettingsProjectionStore::new(projection_database.clone());
    let dnd_projection = Arc::new(DndProjectionStore::new(projection_database));
    let _dnd_reader = Arc::new(PepDndReader::with_system_clock(Arc::clone(&dnd_projection)));

    let _push_registrations = DatabasePushRegistrationStore::attach_existing(database.clone())
        .await
        .map_err(PushNotificationRuntimeAttachError::PushRegistrations)?;
    let _notification_outbox = NotificationOutboxStore::attach_existing(database.clone())
        .await
        .map_err(PushNotificationRuntimeAttachError::NotificationOutbox)?;
    let _notification_activity = NotificationActivityStore::attach_existing(database.clone())
        .await
        .map_err(PushNotificationRuntimeAttachError::NotificationActivity)?;
    let _push_service = DatabasePushServiceStore::attach_existing(
        database.clone(),
        secret_key,
        push_service_jid,
        pubsub_boundary,
    )
    .await
    .map_err(PushNotificationRuntimeAttachError::PushService)?;
    let _vapid_signer = VapidStorage::load_attested(database.clone(), secret_key)
        .await
        .map_err(PushNotificationRuntimeAttachError::Vapid)?;

    Ok(PushNotificationRuntimeAttachEvidence {
        _database: database,
    })
}
