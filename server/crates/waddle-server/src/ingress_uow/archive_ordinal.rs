//! Finalize derived archive authority under the canonical message lock.
use sha2::{Digest, Sha256};
use waddle_xmpp::{
    ingress::{IngressEffectIntent, IngressEffectKey, MessageKey},
    mam::ArchiveOrdinal,
};

use super::{EffectIntentRepository, IngressUowError, IngressUowTransaction};
use crate::db::DatabaseDriver;

/// A planned archive intent whose ordinal is still unfinalized (`None`) matches
/// the recorded intent that carries the committed ordinal: the position is
/// derived state, never identity, so it cannot make two otherwise identical
/// authorities contradict.
pub(super) fn archive_intent_matches(
    recorded: &IngressEffectIntent,
    planned: &IngressEffectIntent,
) -> bool {
    if recorded == planned {
        return true;
    }
    let recorded_ordinal = match recorded {
        IngressEffectIntent::ArchiveAuthoritative { ordinal, .. }
        | IngressEffectIntent::SystemMessageArchive { ordinal, .. } => *ordinal,
        _ => return false,
    };
    let mut planned = planned.clone();
    match &mut planned {
        IngressEffectIntent::ArchiveAuthoritative { ordinal, .. }
        | IngressEffectIntent::SystemMessageArchive { ordinal, .. }
            if ordinal.is_none() =>
        {
            *ordinal = recorded_ordinal;
        }
        _ => return false,
    }
    *recorded == planned
}

impl EffectIntentRepository {
    /// Persist the committed position without changing the effect's identity or version.
    pub async fn record_archive_ordinal(
        tx: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
        key: &IngressEffectKey,
        ordinal: ArchiveOrdinal,
    ) -> Result<(), IngressUowError> {
        if !matches!(key, IngressEffectKey::ArchiveAuthoritative(..)) {
            return Err(IngressUowError::EffectIntentConflict);
        }
        let mut intent = Self::load(tx, message_key)
            .await?
            .into_iter()
            .find(|intent| intent.semantic_key() == *key)
            .ok_or(IngressUowError::EffectIntentConflict)?;
        let (IngressEffectIntent::ArchiveAuthoritative {
            ordinal: recorded, ..
        }
        | IngressEffectIntent::SystemMessageArchive {
            ordinal: recorded, ..
        }) = &mut intent
        else {
            return Err(IngressUowError::EffectIntentConflict);
        };
        if let Some(recorded) = *recorded {
            if recorded != ordinal {
                return Err(IngressUowError::ArchiveOrdinalConflict {
                    recorded,
                    stored: ordinal,
                });
            }
            return Ok(());
        }
        *recorded = Some(ordinal);
        let hash = Sha256::digest(key.storage_identity().as_bytes());
        let (kind, payload) = intent.with_encoded_v1(|kind, payload| (kind, payload.to_vec()))?;
        let sql = if tx.transaction_mut().driver() == DatabaseDriver::Postgres {
            "UPDATE ingress_effect_intents SET payload = ? WHERE message_key = ?::uuid AND kind = ? AND semantic_identity_hash = ? AND payload_version = 1"
        } else {
            "UPDATE ingress_effect_intents SET payload = ? WHERE message_key = ? AND kind = ? AND semantic_identity_hash = ? AND payload_version = 1"
        };
        let changed = tx
            .transaction_mut()
            .execute(
                sql,
                crate::db_params![
                    payload,
                    message_key.to_storage().to_string(),
                    i64::from(kind),
                    hash.to_vec()
                ],
            )
            .await?;
        if changed != 1 {
            return Err(IngressUowError::EffectIntentConflict);
        }
        Ok(())
    }
}
