//! Durable authority for the connection generation currently bound to a full JID.
//!
//! Only a fresh bind publishes a generation. Resume and remote-resource refresh
//! may verify an existing generation, but must never make an old one current.
//! Room commits hold the matching row lock until their projection transaction
//! commits, so a bind replacement cannot overtake an authorized old join.

use jid::FullJid;
use waddle_xmpp_core::OccupancySessionGeneration;

use crate::db::{Database, DatabaseDriver, DatabaseError, Transaction};

/// Held through local and remote registry publication. SQLite cannot cluster;
/// its caller's per-JID bind mutex supplies serialization without holding a
/// database writer lock across nested actor calls.
pub struct CurrentGenerationGuard<'a> {
    _transaction: Option<Transaction<'a>>,
}

pub async fn acquire_current<'a>(
    db: &'a Database,
    jid: &FullJid,
    generation: OccupancySessionGeneration,
) -> Result<Option<CurrentGenerationGuard<'a>>, DatabaseError> {
    if db.driver() == DatabaseDriver::Sqlite {
        return Ok(is_current(db, jid, generation)
            .await?
            .then_some(CurrentGenerationGuard { _transaction: None }));
    }
    let mut transaction = db.begin().await?;
    Ok(lock_current(&mut transaction, jid, generation)
        .await?
        .then_some(CurrentGenerationGuard {
            _transaction: Some(transaction),
        }))
}

pub async fn publish(
    db: &Database,
    jid: &FullJid,
    generation: OccupancySessionGeneration,
) -> Result<Option<OccupancySessionGeneration>, DatabaseError> {
    let mut tx = db.begin_immediate().await?;
    tx.execute(
        "INSERT INTO xmpp_occupancy_authority (full_jid, generation) VALUES (?, ?) \
         ON CONFLICT (full_jid) DO NOTHING",
        crate::db_params![jid.to_string(), generation.to_string()],
    )
    .await?;
    let sql = match tx.driver() {
        DatabaseDriver::Postgres => {
            "SELECT generation FROM xmpp_occupancy_authority WHERE full_jid = ? FOR UPDATE"
        }
        DatabaseDriver::Sqlite => {
            "SELECT generation FROM xmpp_occupancy_authority WHERE full_jid = ?"
        }
    };
    let mut rows = tx.query(sql, crate::db_params![jid.to_string()]).await?;
    let row = rows.next().await?.ok_or_else(|| {
        DatabaseError::QueryFailed("occupancy authority disappeared during bind".to_owned())
    })?;
    let previous: String = row.get(0)?;
    let previous: OccupancySessionGeneration = previous.parse().map_err(|_| {
        DatabaseError::QueryFailed("invalid persisted occupancy generation".to_owned())
    })?;
    drop(rows);
    if previous != generation {
        tx.execute(
            "UPDATE xmpp_occupancy_authority SET generation = ? WHERE full_jid = ?",
            crate::db_params![generation.to_string(), jid.to_string()],
        )
        .await?;
    }
    tx.commit().await?;
    Ok((previous != generation).then_some(previous))
}

pub async fn is_current(
    db: &Database,
    jid: &FullJid,
    generation: OccupancySessionGeneration,
) -> Result<bool, DatabaseError> {
    let conn = db.guard().await?;
    let mut rows = conn
        .query(
            "SELECT 1 FROM xmpp_occupancy_authority WHERE full_jid = ? AND generation = ?",
            crate::db_params![jid.to_string(), generation.to_string()],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

/// Verify under the caller's transaction. SQLite callers must begin with
/// `begin_immediate`; PostgreSQL holds a row SHARE lock through commit.
pub(crate) async fn lock_current(
    tx: &mut Transaction<'_>,
    jid: &FullJid,
    generation: OccupancySessionGeneration,
) -> Result<bool, DatabaseError> {
    let sql = match tx.driver() {
        DatabaseDriver::Postgres => {
            "SELECT 1 FROM xmpp_occupancy_authority WHERE full_jid = ? AND generation = ? FOR SHARE"
        }
        DatabaseDriver::Sqlite => {
            "SELECT 1 FROM xmpp_occupancy_authority WHERE full_jid = ? AND generation = ?"
        }
    };
    let mut rows = tx
        .query(
            sql,
            crate::db_params![jid.to_string(), generation.to_string()],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn replacement_rejects_departed_generation_and_missing_authority() {
        let db = Database::in_memory("occupancy-authority").await.unwrap();
        crate::db::MigrationRunner::global().run(&db).await.unwrap();
        let jid: FullJid = "alice@example.com/desktop".parse().unwrap();
        let departed = OccupancySessionGeneration::mint();
        let replacement = OccupancySessionGeneration::mint();
        assert!(!is_current(&db, &jid, departed).await.unwrap());
        assert_eq!(publish(&db, &jid, departed).await.unwrap(), None);
        assert!(is_current(&db, &jid, departed).await.unwrap());
        assert_eq!(
            publish(&db, &jid, replacement).await.unwrap(),
            Some(departed)
        );
        assert!(!is_current(&db, &jid, departed).await.unwrap());
        assert!(is_current(&db, &jid, replacement).await.unwrap());
        let mut tx = db.begin_immediate().await.unwrap();
        assert!(!lock_current(&mut tx, &jid, departed).await.unwrap());
        assert!(lock_current(&mut tx, &jid, replacement).await.unwrap());
        tx.commit().await.unwrap();
    }
}
