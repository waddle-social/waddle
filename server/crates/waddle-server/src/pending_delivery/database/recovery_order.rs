//! Monotone physical insertion metadata for bounded notification recovery laps.

use super::*;

const INITIALIZATION_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);
const BACKFILL_BATCH: i64 = 128;

pub(super) async fn initialize(
    storage: &DatabasePendingDeliveryStorage,
) -> Result<(), PendingStorageError> {
    tokio::time::timeout(INITIALIZATION_BUDGET, initialize_inner(storage))
        .await
        .map_err(|_| {
            PendingStorageError::Other(
                "pending recovery metadata initialization timed out".to_owned(),
            )
        })?
}

async fn begin(
    storage: &DatabasePendingDeliveryStorage,
) -> Result<crate::db::Transaction<'_>, PendingStorageError> {
    let mut tx = storage.db.begin_immediate().await.map_err(other)?;
    if tx.driver() == DatabaseDriver::Postgres {
        // Fresh catalog reads are required after waiting for another initializer.
        tx.execute("SET TRANSACTION ISOLATION LEVEL READ COMMITTED", ())
            .await
            .map_err(other)?;
        tx.execute("SET LOCAL lock_timeout = '5s'", ())
            .await
            .map_err(other)?;
        tx.execute("SET LOCAL statement_timeout = '25s'", ())
            .await
            .map_err(other)?;
        tx.execute("SELECT pg_advisory_xact_lock(hashtext(current_schema()), hashtext('pending_notification_recovery_ordinal'))", ()).await.map_err(other)?;
    }
    Ok(tx)
}

fn other(error: crate::db::DatabaseError) -> PendingStorageError {
    PendingStorageError::Other(error.to_string())
}

async fn initialize_inner(
    storage: &DatabasePendingDeliveryStorage,
) -> Result<(), PendingStorageError> {
    let mut tx = begin(storage).await?;
    if tx.driver() == DatabaseDriver::Postgres {
        initialize_postgres(&mut tx).await?;
    } else {
        initialize_sqlite(&mut tx).await?;
    }
    tx.execute("CREATE UNIQUE INDEX IF NOT EXISTS idx_pending_notification_recovery_ordinal ON pending_delivery (notification_recovery_ordinal)", ()).await.map_err(other)?;
    tx.execute("CREATE INDEX IF NOT EXISTS idx_pending_notification_recovery_due ON pending_delivery (notification_recovery_ordinal) WHERE payload_kind = 'archived' AND flushed_in_session IS NULL AND notification_outboxed_at_ms IS NULL", ()).await.map_err(other)?;
    tx.commit().await.map_err(other)
}

async fn initialize_postgres(
    tx: &mut crate::db::Transaction<'_>,
) -> Result<(), PendingStorageError> {
    let identity = {
        let mut rows = tx.query("SELECT attidentity::text FROM pg_attribute WHERE attrelid = 'pending_delivery'::regclass AND attname = 'notification_recovery_ordinal' AND NOT attisdropped", ()).await.map_err(other)?;
        rows.next()
            .await
            .map_err(other)?
            .map(|row| row.get::<String>(0))
            .transpose()
            .map_err(other)?
    };
    match identity.as_deref() {
        None => {
            // Adding the new identity column populates existing rows atomically.
            // Its native sequence survives deletion and is never reset here.
            tx.execute("ALTER TABLE pending_delivery ADD COLUMN notification_recovery_ordinal BIGINT GENERATED ALWAYS AS IDENTITY (CACHE 1 NO CYCLE)", ()).await.map_err(other)?;
        }
        Some("a") => {}
        Some(_) => {
            return Err(PendingStorageError::Other(
                "pending recovery ordinal is not an always-generated identity".to_owned(),
            ))
        }
    }
    tx.execute("CREATE OR REPLACE FUNCTION pending_notification_recovery_ordinal_immutable() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.notification_recovery_ordinal IS DISTINCT FROM OLD.notification_recovery_ordinal THEN RAISE EXCEPTION 'pending recovery ordinal is immutable'; END IF; RETURN NEW; END $$", ()).await.map_err(other)?;
    tx.execute("DROP TRIGGER IF EXISTS pending_notification_recovery_ordinal_immutable ON pending_delivery", ()).await.map_err(other)?;
    tx.execute("CREATE TRIGGER pending_notification_recovery_ordinal_immutable BEFORE UPDATE OF notification_recovery_ordinal ON pending_delivery FOR EACH ROW EXECUTE FUNCTION pending_notification_recovery_ordinal_immutable()", ()).await.map_err(other)?;
    Ok(())
}

async fn initialize_sqlite(tx: &mut crate::db::Transaction<'_>) -> Result<(), PendingStorageError> {
    let has_ordinal = {
        let mut rows = tx
            .query("PRAGMA table_info(pending_delivery)", ())
            .await
            .map_err(other)?;
        let mut found = false;
        while let Some(row) = rows.next().await.map_err(other)? {
            found |= row.get::<String>(1).map_err(other)? == "notification_recovery_ordinal";
        }
        found
    };
    let has_counter = {
        let mut rows = tx.query("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'pending_notification_recovery_counter'", ()).await.map_err(other)?;
        rows.next().await.map_err(other)?.is_some()
    };
    if has_ordinal && !has_counter {
        return Err(PendingStorageError::Other(
            "pending recovery counter missing for existing ordinal metadata".to_owned(),
        ));
    }
    if !has_ordinal {
        tx.execute("ALTER TABLE pending_delivery ADD COLUMN notification_recovery_ordinal INTEGER CHECK (notification_recovery_ordinal IS NULL OR (typeof(notification_recovery_ordinal) = 'integer' AND notification_recovery_ordinal > 0))", ()).await.map_err(other)?;
    }
    if !has_counter {
        tx.execute("CREATE TABLE pending_notification_recovery_counter (singleton INTEGER PRIMARY KEY CHECK (singleton = 1), last_ordinal INTEGER NOT NULL CHECK (typeof(last_ordinal) = 'integer' AND last_ordinal >= 0))", ()).await.map_err(other)?;
        tx.execute("INSERT INTO pending_notification_recovery_counter (singleton, last_ordinal) VALUES (1, 0)", ()).await.map_err(other)?;
    }
    let mut counter = {
        let mut rows = tx.query("SELECT last_ordinal FROM pending_notification_recovery_counter WHERE singleton = 1", ()).await.map_err(other)?;
        rows.next()
            .await
            .map_err(other)?
            .ok_or_else(|| {
                PendingStorageError::Other("pending recovery counter row missing".to_owned())
            })?
            .get::<i64>(0)
            .map_err(other)?
    };
    let maximum = {
        let mut rows = tx
            .query(
                "SELECT COALESCE(MAX(notification_recovery_ordinal), 0) FROM pending_delivery",
                (),
            )
            .await
            .map_err(other)?;
        rows.next()
            .await
            .map_err(other)?
            .ok_or_else(|| {
                PendingStorageError::Other("pending recovery maximum missing".to_owned())
            })?
            .get::<i64>(0)
            .map_err(other)?
    };
    counter = counter.max(maximum);
    let mut after = None::<PendingRowId>;
    loop {
        let ids = {
            let mut rows = match &after {
                Some(after) => tx.query("SELECT row_id FROM pending_delivery WHERE notification_recovery_ordinal IS NULL AND row_id > ? ORDER BY row_id LIMIT ?", crate::db_params![after.as_str(), BACKFILL_BATCH]).await.map_err(other)?,
                None => tx.query("SELECT row_id FROM pending_delivery WHERE notification_recovery_ordinal IS NULL ORDER BY row_id LIMIT ?", crate::db_params![BACKFILL_BATCH]).await.map_err(other)?,
            };
            let mut ids = Vec::new();
            while let Some(row) = rows.next().await.map_err(other)? {
                ids.push(PendingRowId::new(row.get::<String>(0).map_err(other)?));
            }
            ids
        };
        if ids.is_empty() {
            break;
        }
        for id in &ids {
            counter = counter.checked_add(1).ok_or_else(|| {
                PendingStorageError::Other("pending recovery ordinal exhausted".to_owned())
            })?;
            if tx.execute("UPDATE pending_delivery SET notification_recovery_ordinal = ? WHERE row_id = ? AND notification_recovery_ordinal IS NULL", crate::db_params![counter, id.as_str()]).await.map_err(other)? != 1 {
                return Err(PendingStorageError::Other("pending recovery backfill lost a physical row".to_owned()));
            }
        }
        after = ids.last().cloned();
    }
    tx.execute(
        "UPDATE pending_notification_recovery_counter SET last_ordinal = ? WHERE singleton = 1",
        crate::db_params![counter],
    )
    .await
    .map_err(other)?;
    for name in [
        "pending_notification_recovery_allocate",
        "pending_notification_recovery_ordinal_immutable",
        "pending_notification_recovery_counter_monotone",
    ] {
        tx.execute(&format!("DROP TRIGGER IF EXISTS {name}"), ())
            .await
            .map_err(other)?;
    }
    tx.execute("CREATE TRIGGER pending_notification_recovery_allocate AFTER INSERT ON pending_delivery BEGIN SELECT CASE WHEN NOT EXISTS (SELECT 1 FROM pending_notification_recovery_counter WHERE singleton = 1 AND last_ordinal < 9223372036854775807) THEN RAISE(ABORT, 'pending recovery ordinal exhausted or counter missing') END; UPDATE pending_notification_recovery_counter SET last_ordinal = last_ordinal + 1 WHERE singleton = 1; UPDATE pending_delivery SET notification_recovery_ordinal = (SELECT last_ordinal FROM pending_notification_recovery_counter WHERE singleton = 1) WHERE row_id = NEW.row_id; END", ()).await.map_err(other)?;
    tx.execute("CREATE TRIGGER pending_notification_recovery_ordinal_immutable BEFORE UPDATE OF notification_recovery_ordinal ON pending_delivery WHEN OLD.notification_recovery_ordinal IS NOT NULL AND NEW.notification_recovery_ordinal IS NOT OLD.notification_recovery_ordinal BEGIN SELECT RAISE(ABORT, 'pending recovery ordinal is immutable'); END", ()).await.map_err(other)?;
    tx.execute("CREATE TRIGGER pending_notification_recovery_counter_monotone BEFORE UPDATE OF last_ordinal ON pending_notification_recovery_counter WHEN NEW.last_ordinal < OLD.last_ordinal BEGIN SELECT RAISE(ABORT, 'pending recovery counter cannot rewind'); END", ()).await.map_err(other)?;
    Ok(())
}

#[cfg(test)]
#[path = "recovery_order_tests.rs"]
mod tests;
