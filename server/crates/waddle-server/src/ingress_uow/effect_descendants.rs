//! Foundation references for scheduled descendants, never completion receipts.

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use waddle_xmpp::ingress::{DeliveryKey, IngressEffectKey, MessageKey};

use super::{IngressUowError, IngressUowTransaction};
use crate::db::{DatabaseDriver, Transaction};
use crate::ingress_substrate::{acquire_epoch_lock_first, supported_protocol_epoch};

#[derive(Debug, Default, Clone, Copy)]
pub struct EffectDescendantRepository;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectDeliveryBinding {
    Bound(DeliveryKey),
    AwaitingDurableOwner,
}

impl EffectDescendantRepository {
    pub async fn lock_nowait(
        transaction: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
    ) -> Result<(), IngressUowError> {
        Self::lock_nowait_raw(transaction.transaction_mut(), message_key).await
    }

    pub async fn copy(
        transaction: &mut IngressUowTransaction<'_>,
        source: Uuid,
        target: Uuid,
    ) -> Result<(), IngressUowError> {
        Self::copy_raw(transaction.transaction_mut(), source, target).await
    }

    pub async fn settle_all(
        transaction: &mut IngressUowTransaction<'_>,
        descendant: Uuid,
        now: DateTime<Utc>,
    ) -> Result<(), IngressUowError> {
        Self::settle_all_raw(transaction.transaction_mut(), descendant, now).await
    }

    /// New parents acquired after pre-existing work must never wait: abort the
    /// whole UoW so its complete lock set can be retried from the beginning.
    pub async fn lock_nowait_raw(
        tx: &mut Transaction<'_>,
        message_key: MessageKey,
    ) -> Result<(), IngressUowError> {
        install_epoch_proof(tx).await?;
        let query = sql(
            tx,
            "SELECT 1 FROM ingress_messages WHERE message_key = ?::uuid FOR UPDATE NOWAIT",
            "SELECT 1 FROM ingress_messages WHERE message_key = ?",
        );
        let mut rows = tx
            .query(
                query,
                crate::db_params![message_key.to_storage().to_string()],
            )
            .await
            .map_err(canonical_nowait_error)?;
        if rows.next().await.map_err(canonical_nowait_error)?.is_none() {
            return Err(IngressUowError::EffectIntentMessageMissing);
        }
        Ok(())
    }

    pub async fn lock_all_nowait_raw(
        tx: &mut Transaction<'_>,
        descendant: Uuid,
    ) -> Result<(), IngressUowError> {
        if !table_exists(tx).await? {
            return Ok(());
        }
        install_epoch_proof(tx).await?;
        for key in ancestor_keys(tx, descendant).await? {
            Self::lock_nowait_raw(tx, key).await?;
        }
        Ok(())
    }
    /// Take Foundation locks before scheduler/config/source row locks.
    pub async fn lock_raw(
        tx: &mut Transaction<'_>,
        message_key: MessageKey,
    ) -> Result<(), IngressUowError> {
        Self::lock_nowait_raw(tx, message_key).await
    }

    pub async fn bind_effect_raw(
        tx: &mut Transaction<'_>,
        message_key: MessageKey,
        effect: &IngressEffectKey,
    ) -> Result<EffectDeliveryBinding, IngressUowError> {
        Self::lock_raw(tx, message_key).await?;
        ensure_intent(tx, message_key, effect).await?;
        if matches!(
            effect,
            IngressEffectKey::CallSignal(..)
                | IngressEffectKey::Pin(..)
                | IngressEffectKey::DmPinMutation(..)
                | IngressEffectKey::DmCallThreadState(..)
        ) {
            return Ok(EffectDeliveryBinding::AwaitingDurableOwner);
        }
        let key = DeliveryKey::effect(message_key, effect);
        let outcome = crate::ingress_substrate::record_delivery(tx, key, message_key).await?;
        if outcome == crate::ingress_substrate::MessageWriteOutcome::MessageVanished {
            return Err(IngressUowError::EffectIntentMessageMissing);
        }
        Ok(EffectDeliveryBinding::Bound(key))
    }

    pub async fn attach(
        transaction: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
        effect: &IngressEffectKey,
        descendant: Uuid,
    ) -> Result<(), IngressUowError> {
        Self::attach_raw(
            transaction.transaction_mut(),
            message_key,
            effect,
            descendant,
        )
        .await
    }

    /// Bind scheduling custody to one recorded canonical effect and its target.
    pub async fn attach_raw(
        tx: &mut Transaction<'_>,
        message_key: MessageKey,
        effect: &IngressEffectKey,
        descendant: Uuid,
    ) -> Result<(), IngressUowError> {
        if Self::bind_effect_raw(tx, message_key, effect).await?
            == EffectDeliveryBinding::AwaitingDurableOwner
        {
            return Err(IngressUowError::EffectIntentConflict);
        }
        let query = sql(tx,
            "INSERT INTO ingress_effect_descendants (message_key, kind, semantic_identity_hash, descendant_key) VALUES (?::uuid, ?, ?, ?) ON CONFLICT DO NOTHING",
            "INSERT INTO ingress_effect_descendants (message_key, kind, semantic_identity_hash, descendant_key) VALUES (?, ?, ?, ?) ON CONFLICT DO NOTHING");
        let inserted = tx
            .execute(
                query,
                crate::db_params![
                    message_key.to_storage().to_string(),
                    effect.storage_kind(),
                    identity_hash(effect).to_vec(),
                    descendant.to_string()
                ],
            )
            .await?;
        if inserted > 0 {
            crate::ingress_substrate::invalidate_retention(tx, message_key).await?;
        }
        Ok(())
    }

    /// Lock all ancestors in canonical order before touching a scheduler row.
    pub async fn lock_all_raw(
        tx: &mut Transaction<'_>,
        descendant: Uuid,
    ) -> Result<(), IngressUowError> {
        if !table_exists(tx).await? {
            return Ok(());
        }
        install_epoch_proof(tx).await?;
        for key in ancestor_keys(tx, descendant).await? {
            Self::lock_raw(tx, key).await?;
        }
        Ok(())
    }

    /// Copy ancestry during candidate fanout/coalescing; the source stays live
    /// until its caller has copied every target and explicitly settles it.
    pub async fn copy_raw(
        tx: &mut Transaction<'_>,
        source: Uuid,
        target: Uuid,
    ) -> Result<(), IngressUowError> {
        if !table_exists(tx).await? {
            return Ok(());
        }
        Self::lock_all_nowait_raw(tx, source).await?;
        let keys = ancestor_keys(tx, source).await?;
        tx.execute(
            "INSERT INTO ingress_effect_descendants (message_key, kind, semantic_identity_hash, descendant_key) SELECT message_key, kind, semantic_identity_hash, ? FROM ingress_effect_descendants WHERE descendant_key = ? AND settled_at IS NULL ON CONFLICT DO NOTHING",
            crate::db_params![target.to_string(), source.to_string()],
        ).await?;
        for key in keys {
            crate::ingress_substrate::invalidate_retention(tx, key).await?;
        }
        Ok(())
    }

    /// Explicit durable scheduler/provider settlement, not transport ACK.
    pub async fn settle_all_raw(
        tx: &mut Transaction<'_>,
        descendant: Uuid,
        now: DateTime<Utc>,
    ) -> Result<(), IngressUowError> {
        if !table_exists(tx).await? {
            return Ok(());
        }
        Self::lock_all_raw(tx, descendant).await?;
        let keys = ancestor_keys(tx, descendant).await?;
        let query = sql(tx,
            "UPDATE ingress_effect_descendants SET settled_at = ?::timestamptz WHERE descendant_key = ? AND settled_at IS NULL",
            "UPDATE ingress_effect_descendants SET settled_at = ? WHERE descendant_key = ? AND settled_at IS NULL");
        tx.execute(
            query,
            crate::db_params![now.to_rfc3339(), descendant.to_string()],
        )
        .await?;
        for key in keys {
            crate::ingress_substrate::refresh_retention(tx, key, now).await?;
        }
        Ok(())
    }
}

async fn ensure_intent(
    tx: &mut Transaction<'_>,
    message_key: MessageKey,
    effect: &IngressEffectKey,
) -> Result<(), IngressUowError> {
    let query = sql(tx,
        "SELECT 1 FROM ingress_effect_intents WHERE message_key = ?::uuid AND kind = ? AND semantic_identity_hash = ?",
        "SELECT 1 FROM ingress_effect_intents WHERE message_key = ? AND kind = ? AND semantic_identity_hash = ?");
    let mut rows = tx
        .query(
            query,
            crate::db_params![
                message_key.to_storage().to_string(),
                effect.storage_kind(),
                identity_hash(effect).to_vec()
            ],
        )
        .await?;
    if rows.next().await?.is_none() {
        return Err(IngressUowError::EffectIntentConflict);
    }
    Ok(())
}

fn identity_hash(effect: &IngressEffectKey) -> [u8; 32] {
    Sha256::digest(effect.storage_identity().as_bytes()).into()
}

async fn install_epoch_proof(tx: &mut Transaction<'_>) -> Result<(), IngressUowError> {
    bound_raw_transaction(tx).await?;
    let live = acquire_epoch_lock_first(tx).await?;
    let supported = supported_protocol_epoch();
    if live > supported {
        return Err(IngressUowError::EpochUnsupported { live, supported });
    }
    if tx.driver() == DatabaseDriver::Postgres {
        let mut rows = tx.query("SELECT set_config('waddle.protocol_epoch', ?, true), set_config('waddle.protocol_epoch_xid', pg_current_xact_id()::text, true)", crate::db_params![live.to_storage().to_string()]).await?;
        rows.next()
            .await?
            .ok_or(IngressUowError::EpochProofMissing)?;
    }
    Ok(())
}

async fn table_exists(tx: &mut Transaction<'_>) -> Result<bool, IngressUowError> {
    bound_raw_transaction(tx).await?;
    let query = sql(
        tx,
        "SELECT 1 WHERE to_regclass('ingress_effect_descendants') IS NOT NULL",
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'ingress_effect_descendants'",
    );
    let mut rows = tx.query(query, ()).await?;
    Ok(rows.next().await?.is_some())
}

/// Scheduler transactions also enter through plain Database::begin. Bound
/// even the first epoch/catalog wait, retaining any tighter ingress UoW bounds.
async fn bound_raw_transaction(tx: &mut Transaction<'_>) -> Result<(), IngressUowError> {
    if tx.driver() == DatabaseDriver::Postgres {
        let mut rows = tx
            .query(
                r#"
            SELECT
              set_config('lock_timeout', CASE
                WHEN current_setting('lock_timeout')::interval = interval '0 ms'
                  OR current_setting('lock_timeout')::interval > interval '100 ms'
                THEN '100ms' ELSE current_setting('lock_timeout') END, true),
              set_config('statement_timeout', CASE
                WHEN current_setting('statement_timeout')::interval = interval '0 ms'
                  OR current_setting('statement_timeout')::interval > interval '250 ms'
                THEN '250ms' ELSE current_setting('statement_timeout') END, true)
        "#,
                (),
            )
            .await?;
        rows.next()
            .await?
            .ok_or(IngressUowError::TransactionBoundsUnproven)?;
    }
    Ok(())
}

async fn ancestor_keys(
    tx: &mut Transaction<'_>,
    descendant: Uuid,
) -> Result<Vec<MessageKey>, IngressUowError> {
    let query = sql(tx, "SELECT DISTINCT message_key::text FROM ingress_effect_descendants WHERE descendant_key = ? AND settled_at IS NULL ORDER BY message_key::text", "SELECT DISTINCT message_key FROM ingress_effect_descendants WHERE descendant_key = ? AND settled_at IS NULL ORDER BY message_key");
    let mut rows = tx
        .query(query, crate::db_params![descendant.to_string()])
        .await?;
    let mut keys = Vec::new();
    while let Some(row) = rows.next().await? {
        let value: String = row.get(0)?;
        keys.push(MessageKey::from_storage(value.parse().map_err(|_| {
            crate::ingress_substrate::IngressSubstrateError::InvalidStoredMessageKey
        })?));
    }
    Ok(keys)
}

fn sql<'a>(tx: &Transaction<'_>, postgres: &'a str, sqlite: &'a str) -> &'a str {
    match tx.driver() {
        DatabaseDriver::Postgres => postgres,
        DatabaseDriver::Sqlite => sqlite,
    }
}

fn canonical_nowait_error(error: crate::db::DatabaseError) -> IngressUowError {
    if let crate::db::DatabaseError::Internal(sqlx::Error::Database(database)) = &error {
        if database.code().as_deref() == Some("55P03") {
            return IngressUowError::Database {
                retry_class: super::DbRetryClass::CanonicalLockContention,
            };
        }
    }
    error.into()
}
