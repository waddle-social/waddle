//! Incremental queue writes retain the caller's exact ownership fence.

use super::*;

impl PostgresFencedSmPersistence {
    pub(super) async fn append_detached_under(
        &self,
        append: PersistedDetachedAppend,
        fence: &SmClaimFence,
    ) -> Result<KeyedSnapshotOutcome, SmPersistenceError> {
        append.validate()?;
        let stream_id = &append.session.stream_id;
        let mut tx = self
            .db
            .begin()
            .await
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
        let _identity_guard = self.assert_fenced(&mut tx, stream_id, fence).await?;

        if append.new_delivery {
            let proof = append.ingress_append.as_ref().ok_or_else(|| {
                SmPersistenceError::Other("new delivery requires an ingress append".into())
            })?;
            if crate::sm_persistence::ingress_append::authorize_new_delivery(&mut tx, &proof.key)
                .await?
                == crate::sm_persistence::ingress_append::NewDeliveryAuthorization::AlreadyResolved
            {
                return Ok(KeyedSnapshotOutcome::ObligationAlreadyResolved);
            }
        }

        // Never upsert here: a deleted source must stay deleted, and the first
        // detach timestamp, principal, presence and expiry window stay untouched.
        // Materialize a decoded legacy NULL generation without replacing a concrete one.
        let updated = tx.execute(
            "UPDATE sm_sessions SET inbound_count = ?, outbound_count = ?, last_acked = ?, replay_gap_through = ?, occupancy_session = COALESCE(occupancy_session, ?) WHERE stream_id = ?",
            crate::db_params![
                i64::from(append.session.inbound_count),
                i64::from(append.session.outbound_count),
                i64::from(append.session.last_acked),
                append.session.replay_gap_through.map(i64::from),
                append.session.occupancy_session.to_string(),
                stream_id.as_str().to_string(),
            ],
        ).await.map_err(|error| SmPersistenceError::Other(error.to_string()))?;
        if updated != 1 {
            return Err(SmPersistenceError::Other(
                "detached append source is missing".into(),
            ));
        }

        // Claim custody before inserting the replay row: a retry of the same
        // allocation must report the ledger winner, even at the same sequence.
        if let Some(proof) = &append.ingress_append {
            if let Err(error) = crate::sm_persistence::ingress_append::insert(&mut tx, proof).await
            {
                let conflict = crate::sm_persistence::ingress_append::is_ledger_conflict(&error);
                tx.rollback()
                    .await
                    .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
                if !conflict {
                    return Err(SmPersistenceError::Other(error.to_string()));
                }
                let winner = crate::sm_persistence::ingress_append::get(&self.db, &proof.key)
                    .await?
                    .ok_or_else(|| {
                        SmPersistenceError::Other(
                            "ingress append conflict winner missing after rollback".into(),
                        )
                    })?;
                return Ok(KeyedSnapshotOutcome::ObligationAlreadyAllocated {
                    accepting_stream: winner.accepting_stream,
                });
            }
        }

        tx.execute(
            "INSERT INTO sm_unacked (stream_id, sequence, stanza_xml, original_receipt_at_ms, ingress_receipts) VALUES (?, ?, ?, ?, ?)",
            crate::db_params![
                stream_id.as_str().to_string(),
                i64::from(append.stanza.sequence),
                serialize_stanza(&append.stanza.stanza)?,
                append.stanza.original_receipt_at.timestamp_millis(),
                crate::sm_persistence::codec::encode_ingress_receipts(&append.stanza.ingress_receipts),
            ],
        ).await.map_err(|error| SmPersistenceError::Other(error.to_string()))?;
        // Explicit sequences are wrap-safe and bounded by this buffer push. The
        // newly inserted entry may itself have been immediately evicted.
        for sequence in append.evicted_sequences {
            tx.execute(
                "DELETE FROM sm_unacked WHERE stream_id = ? AND sequence = ?",
                crate::db_params![stream_id.as_str().to_string(), i64::from(sequence)],
            )
            .await
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
        }
        tx.commit()
            .await
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
        Ok(KeyedSnapshotOutcome::Committed)
    }
}
