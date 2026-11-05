//! Sequential owner-only preparation for the core Push/outbox/VAPID stores.
//!
//! This is a dark maintenance helper for the core Push Service schema,
//! notification outbox, notification ancestry, and VAPID owner phases.
//! PubSub, PushRegistration, and notification Activity owner phases remain
//! separate prerequisites. Each included phase commits independently, so a
//! later failure leaves earlier phases available for an idempotent retry. The
//! result is evidence of this sequence only; it does not establish complete
//! Push/notification preparation, runtime attachment, or serving readiness.

use thiserror::Error;

use crate::db::lineage::PgIdentity;
use crate::db::{Database, DatabaseDriver};
use crate::notification_outbox::{NotificationOutboxError, NotificationOutboxStore};
use crate::push_service::vapid_storage::{VapidOwnerError, VapidStorage};
use crate::push_service::{DatabasePushServiceStore, PushServiceOwnerError};

/// Failure while preparing one core Push/outbox/VAPID owner phase.
#[derive(Debug, Error)]
pub enum CorePushOutboxVapidOwnerPreparationError {
    #[error("core Push/outbox/VAPID owner preparation requires PostgreSQL")]
    PostgresRequired,
    #[error("core Push/outbox/VAPID owner preparation requires the main database lane")]
    MainLaneRequired,
    #[error("core Push/outbox/VAPID owner preparation requires a non-attested owner handle")]
    OwnerHandleRequired,
    #[error("Push Service schema owner preparation failed: {0}")]
    PushServiceSchema(#[source] PushServiceOwnerError),
    #[error("notification outbox owner preparation failed: {0}")]
    NotificationOutbox(#[source] NotificationOutboxError),
    #[error("Push notification-ancestry adoption failed: {0}")]
    NotificationAncestry(#[source] PushServiceOwnerError),
    #[error("VAPID owner preparation failed: {0}")]
    Vapid(#[source] VapidOwnerError),
}

/// Opaque evidence that the core owner-preparation sequence completed.
///
/// This carries no store handles and makes no claim about runtime attachment,
/// complete Push/notification preparation, functional serving, or a global
/// transaction across the owner phases.
#[must_use]
pub struct CorePushOutboxVapidOwnerPreparationEvidence {
    _physical_identity: PgIdentity,
}

/// Prepare Push Service schema, notification outbox, canonical notification
/// ancestry, and VAPID storage in order using one supplied owner database
/// handle and its independently supplied physical identity.
///
/// The exact same database handle and physical identity are passed to each
/// existing owner API. Each phase call is awaited before the next begins; the
/// outbox's bounded preparation task uses the same pool and is awaited on
/// success. This opens no second pool or database URL, starts no runtime or
/// delivery worker, and returns no functional store. The four owner phases
/// commit independently; failure or cancellation can leave a completed prefix
/// that is safe to retry through these idempotent owner APIs. PubSub,
/// PushRegistration, and Activity must be prepared through their separate
/// owner APIs when those stores are prerequisites for the caller.
pub async fn prepare_core_push_outbox_vapid_owner(
    owner_database: &Database,
    physical_identity: &PgIdentity,
    vapid_root_key: &[u8],
) -> Result<CorePushOutboxVapidOwnerPreparationEvidence, CorePushOutboxVapidOwnerPreparationError> {
    if owner_database.driver() != DatabaseDriver::Postgres {
        return Err(CorePushOutboxVapidOwnerPreparationError::PostgresRequired);
    }
    if !owner_database.is_main_lane() {
        return Err(CorePushOutboxVapidOwnerPreparationError::MainLaneRequired);
    }
    if owner_database.uses_attested_postgres_pool() {
        return Err(CorePushOutboxVapidOwnerPreparationError::OwnerHandleRequired);
    }

    DatabasePushServiceStore::initialize_owner(owner_database, physical_identity)
        .await
        .map_err(CorePushOutboxVapidOwnerPreparationError::PushServiceSchema)?;

    NotificationOutboxStore::prepare_owner(owner_database, physical_identity)
        .await
        .map_err(CorePushOutboxVapidOwnerPreparationError::NotificationOutbox)?;

    DatabasePushServiceStore::adopt_notification_ancestry_owner(owner_database, physical_identity)
        .await
        .map_err(CorePushOutboxVapidOwnerPreparationError::NotificationAncestry)?;

    VapidStorage::prepare_owner(owner_database, physical_identity, vapid_root_key)
        .await
        .map_err(CorePushOutboxVapidOwnerPreparationError::Vapid)?;

    Ok(CorePushOutboxVapidOwnerPreparationEvidence {
        _physical_identity: physical_identity.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::{prepare_core_push_outbox_vapid_owner, CorePushOutboxVapidOwnerPreparationError};
    use crate::db::lineage::{
        PgDatabaseIdentity, PgDatabaseName, PgIdentity, PgSchemaIdentity, PgSchemaName,
        PgSystemIdentifier,
    };
    use crate::db::Database;

    #[tokio::test]
    async fn sqlite_owner_preparation_rejects_without_schema_side_effects() {
        let database = Database::in_memory("push-owner-sqlite-rejection")
            .await
            .expect("in-memory SQLite database");
        let identity = PgIdentity {
            system_identifier: PgSystemIdentifier(1),
            database: PgDatabaseIdentity {
                oid: 1,
                name: PgDatabaseName("not-postgres".to_owned()),
            },
            schema: PgSchemaIdentity {
                oid: 1,
                name: PgSchemaName("main".to_owned()),
            },
        };
        let tables_before = sqlite_user_table_count(&database).await;

        let result = prepare_core_push_outbox_vapid_owner(&database, &identity, &[7; 32]).await;

        assert!(matches!(
            result,
            Err(CorePushOutboxVapidOwnerPreparationError::PostgresRequired)
        ));
        assert_eq!(sqlite_user_table_count(&database).await, tables_before);
    }

    async fn sqlite_user_table_count(database: &Database) -> i64 {
        let connection = database.guard().await.expect("SQLite connection");
        let mut rows = connection
            .query(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' \
                 AND name NOT LIKE 'sqlite_%'",
                (),
            )
            .await
            .expect("query SQLite table catalog");
        rows.next()
            .await
            .expect("SQLite count row result")
            .expect("SQLite count row")
            .get(0)
            .expect("SQLite table count")
    }
}
