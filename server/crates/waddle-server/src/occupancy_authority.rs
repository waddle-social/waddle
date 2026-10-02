//! Durable authority for the connection generation currently bound to a full JID.
//!
//! Only a fresh bind publishes a generation. Resume and remote-resource refresh
//! may verify an existing generation, but must never make an old one current.
//! Room commits hold the matching row lock until their projection transaction
//! commits, so a bind replacement cannot overtake an authorized old join.

use jid::FullJid;
use waddle_xmpp_core::OccupancySessionGeneration;

use crate::db::{Database, DatabaseDriver, DatabaseError, Transaction};

/// Held only through synchronous registry publication, never actor or relay
/// awaits. The caller retains its per-JID bind gate and rechecks durable
/// authority after asynchronous admission. SQLite uses that gate without a
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

/// Return the full JID's current generation, publishing `candidate` only when
/// none exists. Host-owned occupants (extension bots) have no bind to mint a
/// generation, so they keep one stable generation that is never rotated.
pub async fn ensure(
    db: &Database,
    jid: &FullJid,
    candidate: OccupancySessionGeneration,
) -> Result<OccupancySessionGeneration, DatabaseError> {
    let mut tx = db.begin_immediate().await?;
    tx.execute(
        "INSERT INTO xmpp_occupancy_authority (full_jid, generation) VALUES (?, ?) \
         ON CONFLICT (full_jid) DO NOTHING",
        crate::db_params![jid.to_string(), candidate.to_string()],
    )
    .await?;
    let mut rows = tx
        .query(
            "SELECT generation FROM xmpp_occupancy_authority WHERE full_jid = ?",
            crate::db_params![jid.to_string()],
        )
        .await?;
    let row = rows.next().await?.ok_or_else(|| {
        DatabaseError::QueryFailed("occupancy authority disappeared during ensure".to_owned())
    })?;
    let current: String = row.get(0)?;
    let current = current.parse().map_err(|_| {
        DatabaseError::QueryFailed("invalid persisted occupancy generation".to_owned())
    })?;
    drop(rows);
    tx.commit().await?;
    Ok(current)
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
    async fn ensure_publishes_once_and_never_rotates() {
        let db = Database::in_memory("occupancy-authority-ensure")
            .await
            .unwrap();
        crate::db::MigrationRunner::global().run(&db).await.unwrap();
        let bot: FullJid = "stargate@extensions.example.com/bot".parse().unwrap();
        let first = OccupancySessionGeneration::mint();
        assert_eq!(ensure(&db, &bot, first).await.unwrap(), first);
        let later = OccupancySessionGeneration::mint();
        assert_eq!(ensure(&db, &bot, later).await.unwrap(), first);
        assert!(is_current(&db, &bot, first).await.unwrap());
        assert!(!is_current(&db, &bot, later).await.unwrap());
    }

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
