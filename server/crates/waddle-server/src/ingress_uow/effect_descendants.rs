//! Foundation references for scheduled descendants, never completion receipts.

use crate::db::DatabaseError;
use crate::ingress_substrate::IngressSubstrateError;
use crate::ingress_substrate::MessageWriteOutcome;
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use waddle_xmpp::ingress::{DeliveryKey, IngressEffectKey, MessageKey};

use super::{IngressUowError, IngressUowTransaction};
use crate::db::{Database, DatabaseDriver, Transaction};
use crate::ingress_substrate::{acquire_epoch_lock_first, supported_protocol_epoch};

#[derive(Debug, Default, Clone, Copy)]
pub struct EffectDescendantRepository;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectDeliveryBinding {
    Bound(DeliveryKey),
    AwaitingDurableOwner,
}

/// Begin a scheduler transaction that may write canonical descendants.
///
/// This must happen at the actual transaction boundary: PostgreSQL cannot
/// change isolation after a snapshot has been taken, and an epoch request
/// made after scheduler locks can deadlock with epoch activation. Standalone
/// stores without the descendant foundation remain usable.
pub(crate) async fn begin_raw_ingress_writer(
    db: &Database,
) -> Result<Transaction<'_>, IngressUowError> {
    let mut tx = db.begin_immediate().await?;
    if tx.driver() == DatabaseDriver::Postgres {
        tx.execute("SET TRANSACTION ISOLATION LEVEL READ COMMITTED", ())
            .await?;
        // Bound the catalog probe too, without changing standalone stores'
        // timeout policy after a foundation-free probe.
        let mut rows = tx
            .query(
                "SELECT current_setting('lock_timeout'), current_setting('statement_timeout')",
                (),
            )
            .await?;
        let row = rows
            .next()
            .await?
            .ok_or(IngressUowError::TransactionBoundsUnproven)?;
        let lock_timeout: String = row.get(0)?;
        let statement_timeout: String = row.get(1)?;
        drop(rows);
        bound_raw_transaction(&mut tx).await?;
        if descendant_foundation_exists(&mut tx).await? {
            install_epoch_proof(&mut tx).await?;
        } else {
            tx.execute(
                "SELECT set_config('lock_timeout', ?, true), set_config('statement_timeout', ?, true)",
                crate::db_params![lock_timeout, statement_timeout],
            )
            .await?;
        }
    }
    Ok(tx)
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
    pub(crate) async fn lock_nowait_raw(
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

    pub(crate) async fn lock_all_nowait_raw(
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
    pub(crate) async fn lock_raw(
        tx: &mut Transaction<'_>,
        message_key: MessageKey,
    ) -> Result<(), IngressUowError> {
        Self::lock_nowait_raw(tx, message_key).await
    }

    pub(crate) async fn bind_effect_raw(
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
        if outcome == MessageWriteOutcome::MessageVanished {
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
    pub(crate) async fn attach_raw(
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
    pub(crate) async fn lock_all_raw(
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
    pub(crate) async fn copy_raw(
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
    pub(crate) async fn settle_all_raw(
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
    descendant_foundation_exists(tx).await
}

async fn descendant_foundation_exists(tx: &mut Transaction<'_>) -> Result<bool, IngressUowError> {
    let query = sql(
        tx,
        "SELECT 1 WHERE to_regclass('ingress_effect_descendants') IS NOT NULL",
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'ingress_effect_descendants'",
    );
    let mut rows = tx.query(query, ()).await?;
    Ok(rows.next().await?.is_some())
}

/// Bound epoch and catalog waits, retaining any tighter transaction bounds.
pub(crate) async fn bound_raw_transaction(tx: &mut Transaction<'_>) -> Result<(), IngressUowError> {
    if !crate::ingress_substrate::tighten_local_transaction_timeouts(tx).await? {
        return Err(IngressUowError::TransactionBoundsUnproven);
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
        keys.push(MessageKey::from_storage(
            value
                .parse()
                .map_err(|_| IngressSubstrateError::InvalidStoredMessageKey)?,
        ));
    }
    Ok(keys)
}

fn sql<'a>(tx: &Transaction<'_>, postgres: &'a str, sqlite: &'a str) -> &'a str {
    match tx.driver() {
        DatabaseDriver::Postgres => postgres,
        DatabaseDriver::Sqlite => sqlite,
    }
}

pub(crate) fn canonical_nowait_error(error: DatabaseError) -> IngressUowError {
    if let crate::db::DatabaseError::Internal(sqlx::Error::Database(database)) = &error {
        if database.code().as_deref() == Some("55P03") {
            return IngressUowError::Database {
                retry_class: super::DbRetryClass::CanonicalLockContention,
            };
        }
    }
    error.into()
}
