//! Exact admin commit evidence that survives later owners and effect delivery.

use jid::BareJid;
use waddle_xmpp::muc::{AdminMutationId, RoomCommittedCoordinates, RoomLifecycleId, RoomRevision};
use waddle_xmpp::XmppError;

use crate::db::{Database, DatabaseError, Transaction};

pub(super) async fn ensure_schema(tx: &mut Transaction<'_>) -> Result<(), DatabaseError> {
    tx.execute(
        "CREATE TABLE IF NOT EXISTS clustering_muc_admin_receipts (\
         attempt_id TEXT PRIMARY KEY, room_jid TEXT NOT NULL, \
         lifecycle_id TEXT NOT NULL REFERENCES clustering_muc_room_lifecycles(lifecycle_id) ON DELETE CASCADE, \
         revision BIGINT NOT NULL CHECK (revision >= 1), \
         created_at_ms BIGINT NOT NULL)",
        (),
    )
    .await?;
    tx.execute(
        "CREATE INDEX IF NOT EXISTS clustering_muc_admin_receipts_lifecycle_idx \
         ON clustering_muc_admin_receipts (lifecycle_id)",
        (),
    )
    .await?;
    tx.execute(
        "CREATE INDEX IF NOT EXISTS clustering_muc_admin_receipts_created_at_idx \
         ON clustering_muc_admin_receipts (created_at_ms)",
        (),
    )
    .await?;
    Ok(())
}

pub(super) async fn insert_in_tx(
    tx: &mut Transaction<'_>,
    room: &BareJid,
    attempt: AdminMutationId,
    coordinates: RoomCommittedCoordinates,
) -> Result<(), DatabaseError> {
    tx.execute(
        "INSERT INTO clustering_muc_admin_receipts \
         (attempt_id, room_jid, lifecycle_id, revision, created_at_ms) VALUES (?, ?, ?, ?, ?)",
        crate::db_params![
            attempt.as_uuid().to_string(),
            room.to_string(),
            coordinates.lifecycle.to_string(),
            coordinates.revision.as_i64(),
            crate::time::now_ms()
        ],
    )
    .await?;
    Ok(())
}

pub(super) async fn load(
    db: &Database,
    room: &BareJid,
    attempt: AdminMutationId,
) -> Result<Option<RoomCommittedCoordinates>, XmppError> {
    let connection = db.guard().await.map_err(super::db_err)?;
    let mut rows = connection
        .query(
            "SELECT lifecycle_id, revision FROM clustering_muc_admin_receipts \
             WHERE room_jid = ? AND attempt_id = ?",
            crate::db_params![room.to_string(), attempt.as_uuid().to_string()],
        )
        .await
        .map_err(super::db_err)?;
    let Some(row) = rows.next().await.map_err(super::db_err)? else {
        return Ok(None);
    };
    let lifecycle: String = row.get(0).map_err(super::db_err)?;
    let lifecycle = uuid::Uuid::parse_str(&lifecycle)
        .map(RoomLifecycleId::from_uuid)
        .map_err(|_| XmppError::internal("invalid admin receipt lifecycle"))?;
    let revision = RoomRevision::from_stored(row.get(1).map_err(super::db_err)?)
        .ok_or_else(|| XmppError::internal("invalid admin receipt revision"))?;
    Ok(Some(RoomCommittedCoordinates {
        lifecycle,
        revision,
    }))
}

pub(super) async fn delete(
    db: &Database,
    room: &BareJid,
    attempt: AdminMutationId,
) -> Result<(), XmppError> {
    db.guard()
        .await
        .map_err(super::db_err)?
        .execute(
            "DELETE FROM clustering_muc_admin_receipts WHERE room_jid = ? AND attempt_id = ?",
            crate::db_params![room.to_string(), attempt.as_uuid().to_string()],
        )
        .await
        .map_err(super::db_err)?;
    Ok(())
}

/// Remove receipts whose projection cleanup never landed. Every reader of a
/// receipt is a bounded recovery of the attempt that minted it, so anything
/// older than the retention window is stranded, not pending. Bounded so one
/// sweep cannot hold a long delete lock.
pub(super) async fn prune_older_than(
    db: &Database,
    cutoff_ms: i64,
    limit: usize,
) -> Result<u64, XmppError> {
    db.guard()
        .await
        .map_err(super::db_err)?
        .execute(
            "DELETE FROM clustering_muc_admin_receipts WHERE attempt_id IN (\
             SELECT attempt_id FROM clustering_muc_admin_receipts \
             WHERE created_at_ms < ? ORDER BY created_at_ms LIMIT ?)",
            crate::db_params![cutoff_ms, limit as i64],
        )
        .await
        .map_err(super::db_err)
}

pub(super) async fn delete_lifecycle_in_tx(
    tx: &mut Transaction<'_>,
    lifecycle: RoomLifecycleId,
) -> Result<(), DatabaseError> {
    tx.execute(
        "DELETE FROM clustering_muc_admin_receipts WHERE lifecycle_id = ?",
        crate::db_params![lifecycle.to_string()],
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{DatabaseConfig, DatabaseDriver};

    #[tokio::test]
    async fn admin_receipts_are_atomic_exact_and_survive_later_mutations() {
        let db = Database::from_config(
            "admin-receipt-test",
            &DatabaseConfig::new(DatabaseDriver::Sqlite, "sqlite::memory:"),
        )
        .await
        .expect("database");
        let room: BareJid = "room@muc.example.com".parse().expect("room");
        let coordinates = RoomCommittedCoordinates {
            lifecycle: RoomLifecycleId::generate(),
            revision: RoomRevision::initial(),
        };
        let mut tx = db.begin().await.expect("schema transaction");
        tx.execute(
            "CREATE TABLE clustering_muc_room_lifecycles (lifecycle_id TEXT PRIMARY KEY)",
            (),
        )
        .await
        .expect("lifecycle schema");
        tx.execute(
            "INSERT INTO clustering_muc_room_lifecycles VALUES (?)",
            crate::db_params![coordinates.lifecycle.to_string()],
        )
        .await
        .expect("lifecycle");
        ensure_schema(&mut tx).await.expect("receipt schema");
        tx.commit().await.expect("schema commit");

        let failed_attempt = AdminMutationId::generate();
        let mut tx = db.begin().await.expect("rolled-back transaction");
        insert_in_tx(&mut tx, &room, failed_attempt, coordinates)
            .await
            .expect("stage failed receipt");
        tx.rollback().await.expect("rollback");
        assert_eq!(load(&db, &room, failed_attempt).await.expect("read"), None);

        let committed_attempt = AdminMutationId::generate();
        let mut tx = db.begin().await.expect("committed transaction");
        insert_in_tx(&mut tx, &room, committed_attempt, coordinates)
            .await
            .expect("stage committed receipt");
        tx.commit().await.expect("commit");
        let later_attempt = AdminMutationId::generate();
        let mut tx = db.begin().await.expect("later owner transaction");
        insert_in_tx(
            &mut tx,
            &room,
            later_attempt,
            RoomCommittedCoordinates {
                revision: coordinates.revision.next().expect("next revision"),
                ..coordinates
            },
        )
        .await
        .expect("later owner receipt");
        tx.commit().await.expect("later commit");
        assert_eq!(load(&db, &room, failed_attempt).await.expect("read"), None);
        assert_eq!(
            load(&db, &room, committed_attempt).await.expect("read"),
            Some(coordinates)
        );
        let other_room: BareJid = "other@muc.example.com".parse().expect("other room");
        assert_eq!(
            load(&db, &other_room, committed_attempt)
                .await
                .expect("read"),
            None
        );
        delete(&db, &other_room, committed_attempt)
            .await
            .expect("wrong-room cleanup");
        assert_eq!(
            load(&db, &room, committed_attempt).await.expect("read"),
            Some(coordinates)
        );
        delete(&db, &room, committed_attempt)
            .await
            .expect("projected cleanup");
        assert_eq!(
            load(&db, &room, committed_attempt).await.expect("read"),
            None
        );
        let mut tx = db.begin().await.expect("destroy transaction");
        delete_lifecycle_in_tx(&mut tx, coordinates.lifecycle)
            .await
            .expect("destroy cleanup");
        tx.commit().await.expect("destroy commit");
        assert_eq!(load(&db, &room, later_attempt).await.expect("read"), None);
    }

    #[tokio::test]
    async fn stranded_receipts_are_pruned_by_age_in_bounded_batches() {
        let db = Database::from_config(
            "admin-receipt-prune-test",
            &DatabaseConfig::new(DatabaseDriver::Sqlite, "sqlite::memory:"),
        )
        .await
        .expect("database");
        let room: BareJid = "room@muc.example.com".parse().expect("room");
        let coordinates = RoomCommittedCoordinates {
            lifecycle: RoomLifecycleId::generate(),
            revision: RoomRevision::initial(),
        };
        let mut tx = db.begin().await.expect("schema transaction");
        tx.execute(
            "CREATE TABLE clustering_muc_room_lifecycles (lifecycle_id TEXT PRIMARY KEY)",
            (),
        )
        .await
        .expect("lifecycle schema");
        tx.execute(
            "INSERT INTO clustering_muc_room_lifecycles VALUES (?)",
            crate::db_params![coordinates.lifecycle.to_string()],
        )
        .await
        .expect("lifecycle");
        ensure_schema(&mut tx).await.expect("receipt schema");
        tx.commit().await.expect("schema commit");

        let stranded: Vec<AdminMutationId> = (0..3).map(|_| AdminMutationId::generate()).collect();
        let fresh = AdminMutationId::generate();
        let mut tx = db.begin().await.expect("insert transaction");
        for attempt in stranded.iter().chain(std::iter::once(&fresh)) {
            insert_in_tx(&mut tx, &room, *attempt, coordinates)
                .await
                .expect("insert receipt");
        }
        tx.commit().await.expect("insert commit");
        // Backdate the stranded rows past the retention window.
        for (index, attempt) in stranded.iter().enumerate() {
            db.guard()
                .await
                .expect("connection")
                .execute(
                    "UPDATE clustering_muc_admin_receipts SET created_at_ms = ? WHERE attempt_id = ?",
                    crate::db_params![index as i64, attempt.as_uuid().to_string()],
                )
                .await
                .expect("backdate");
        }
        let cutoff = crate::time::now_ms() - 1_000;

        assert_eq!(
            prune_older_than(&db, cutoff, 2).await.expect("first page"),
            2
        );
        assert_eq!(load(&db, &room, stranded[0]).await.expect("read"), None);
        assert_eq!(load(&db, &room, stranded[1]).await.expect("read"), None);
        assert_eq!(
            load(&db, &room, stranded[2]).await.expect("read"),
            Some(coordinates),
            "the page limit bounds one sweep"
        );
        assert_eq!(
            prune_older_than(&db, cutoff, 2).await.expect("second page"),
            1
        );
        assert_eq!(load(&db, &room, stranded[2]).await.expect("read"), None);
        assert_eq!(
            load(&db, &room, fresh).await.expect("read"),
            Some(coordinates),
            "a receipt inside the retention window is never pruned"
        );
        assert_eq!(prune_older_than(&db, cutoff, 2).await.expect("idle"), 0);
    }
}
