//! Transfer independent ingress custody into pending delivery in one transaction.

use super::*;
use waddle_xmpp::pending_delivery::storage::CustodyInsertOutcome;
use waddle_xmpp::stream_management::persistence::PersistedIngressAppend;

fn storage_error(error: crate::db::DatabaseError) -> PendingStorageError {
    PendingStorageError::Other(error.to_string())
}

pub(super) async fn insert(
    storage: &DatabasePendingDeliveryStorage,
    row: PendingRow,
    append: &PersistedIngressAppend,
) -> Result<CustodyInsertOutcome, PendingStorageError> {
    if row.recipient != append.key.resource.to_bare()
        || row.original_receipt_at != append.original_receipt_at
    {
        return Err(PendingStorageError::Other(
            "pending row does not match ingress custody recipient and receipt time".to_owned(),
        ));
    }
    let Some(fencing) = &storage.fencing else {
        let mut tx = storage.db.begin_immediate().await.map_err(storage_error)?;
        let outcome = transfer(&mut tx, &row, append, storage.quota).await?;
        tx.commit().await.map_err(storage_error)?;
        return Ok(outcome);
    };

    let entity = Entity::new(
        EntityType::SmSession,
        append.accepting_stream.as_str().to_owned(),
    );
    let identity = fencing.node_identity.current();
    let epoch = fencing
        .claim_store
        .ensure_claimed(&entity, &identity)
        .await
        .map_err(|error| claim_error_to_pending_storage_error(error, entity.clone()))?;
    let claim_fence = SmClaimFence::new(identity, epoch);
    let Some(identity_guard) = fencing
        .node_identity
        .guard_if_current(claim_fence.owner())
        .await
    else {
        return Err(PendingStorageError::NotOwner { entity });
    };
    let mut tx = storage.db.begin().await.map_err(storage_error)?;
    // All transfer paths lock in this order: claim, custody, recipient quota.
    assert_insert_fence(
        &mut tx,
        PendingInsertFence::new(
            &entity,
            &claim_fence,
            &fencing.node_identity,
            &identity_guard,
        ),
    )
    .await?;
    let outcome = transfer(&mut tx, &row, append, storage.quota).await?;
    tx.commit().await.map_err(storage_error)?;
    drop(identity_guard);
    Ok(outcome)
}

async fn transfer(
    tx: &mut crate::db::Transaction<'_>,
    row: &PendingRow,
    append: &PersistedIngressAppend,
    quota: QuotaPolicy,
) -> Result<CustodyInsertOutcome, PendingStorageError> {
    // The no-op UPDATE takes an exclusive row lock on both SQL backends.
    // A concurrent tombstone must either win before this lock (no insert),
    // or wait for the pending row and then scrub it after this commit.
    let held = tx.execute(
        "UPDATE sm_ingress_appends SET disposition = disposition WHERE message_key = ? AND receipt_kind = ? AND semantic_identity_hash = ? AND resource = ? AND accepting_stream_id = ? AND sequence = ?",
        crate::db_params![append.key.message_key.to_storage().to_string(), i64::from(append.key.kind.to_storage()), append.key.semantic_identity_hash.to_vec(), append.key.resource.to_string(), append.accepting_stream.as_str().to_string(), i64::from(append.sequence)],
    ).await.map_err(storage_error)?;
    if held != 1 {
        return Err(PendingStorageError::Other(
            "exact ingress custody allocation is absent from pending storage database".to_owned(),
        ));
    }
    let mut rows = tx.query(
        "SELECT disposition FROM sm_ingress_appends WHERE message_key = ? AND receipt_kind = ? AND semantic_identity_hash = ? AND resource = ? AND accepting_stream_id = ? AND sequence = ?",
        crate::db_params![append.key.message_key.to_storage().to_string(), i64::from(append.key.kind.to_storage()), append.key.semantic_identity_hash.to_vec(), append.key.resource.to_string(), append.accepting_stream.as_str().to_string(), i64::from(append.sequence)],
    ).await.map_err(storage_error)?;
    let current = rows.next().await.map_err(storage_error)?.ok_or_else(|| {
        PendingStorageError::Other("locked ingress custody allocation disappeared".to_owned())
    })?;
    match current.get::<i64>(0).map_err(storage_error)? {
        0 => {}
        1..=3 => return Ok(CustodyInsertOutcome::AlreadyCompleted),
        _ => {
            return Err(PendingStorageError::Other(
                "invalid ingress custody disposition".to_owned(),
            ))
        }
    }
    if insert_in_transaction(tx, row, quota, None).await? == InsertOutcome::QuotaExceeded {
        return Ok(CustodyInsertOutcome::QuotaExceeded);
    }
    let completed = tx.execute(
        "UPDATE sm_ingress_appends SET disposition = 2 WHERE message_key = ? AND receipt_kind = ? AND semantic_identity_hash = ? AND resource = ? AND accepting_stream_id = ? AND sequence = ? AND disposition = 0",
        crate::db_params![append.key.message_key.to_storage().to_string(), i64::from(append.key.kind.to_storage()), append.key.semantic_identity_hash.to_vec(), append.key.resource.to_string(), append.accepting_stream.as_str().to_string(), i64::from(append.sequence)],
    ).await.map_err(storage_error)?;
    if completed != 1 {
        return Err(PendingStorageError::Other(
            "locked ingress custody transition failed".to_owned(),
        ));
    }
    Ok(CustodyInsertOutcome::Inserted)
}

#[cfg(test)]
mod tests;
