//! Foundation references for scheduled descendants, never completion receipts.

use crate::db::DatabaseError;
use crate::ingress_substrate::IngressSubstrateError;
use crate::ingress_substrate::MessageWriteOutcome;
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use waddle_xmpp::ingress::{DeliveryKey, IngressEffectKey, MessageKey};

#[cfg(feature = "test-support")]
use super::RelationGuardCapability;
use super::{IngressUowError, IngressUowTransaction};
#[cfg(feature = "test-support")]
use crate::db::physical_attestation::AttestedPostgresWriterCapability;
use crate::db::{Database, DatabaseDriver, IntoParams, Transaction};
use crate::ingress_substrate::DefinerApiCandidateContext;
use crate::ingress_substrate::{
    acquire_epoch_lock_first, prepared_ingress_relation, prepared_ingress_sql,
    supported_protocol_epoch, PreparedAttestedReceiptWriter, PreparedIngressRelation,
};
#[cfg(feature = "test-support")]
#[cfg(feature = "test-support")]
use crate::ingress_substrate::{
    begin_ingress_entry, IngressEntryAcquisition, IngressEntryAuthority, IngressEntryClamp,
    IngressEntryRequest,
};

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
    begin_raw_ingress_writer_inner(db).await
}

/// Begin a dark raw writer using the full identity capability retained by an
/// independently attested PostgreSQL pool. This path is not wired into normal
/// startup and verifies the same physical transaction connection before any
/// application lock or DML.
#[cfg(feature = "test-support")]
pub(crate) async fn begin_raw_ingress_writer_with_capability<'a>(
    db: &'a Database,
    capability: &AttestedPostgresWriterCapability,
) -> Result<Transaction<'a>, IngressUowError> {
    if db.driver() != DatabaseDriver::Postgres {
        return Err(IngressUowError::PostgresRequired);
    }
    let entry = begin_ingress_entry(
        db,
        IngressEntryRequest {
            authority: IngressEntryAuthority::InstalledGuard(capability),
            acquisition: IngressEntryAcquisition::Immediate,
            acquisition_deadline: None,
            explicit_timeouts: None,
            clamp: IngressEntryClamp::Writer,
        },
    )
    .await?;
    Ok(entry.into_parts().0)
}

/// Begin a raw writer prepared for the dark SECURITY DEFINER API experiment.
/// The returned writer and prepared identity refer to this exact caller-owned
/// transaction; callers must retain both through commit/rollback.
#[cfg(feature = "test-support")]
pub(crate) async fn begin_raw_ingress_writer_with_definer_api_candidate<'a>(
    db: &'a Database,
    capability: &AttestedPostgresWriterCapability,
    relation_guard: &RelationGuardCapability,
) -> Result<(Transaction<'a>, PreparedAttestedReceiptWriter), IngressUowError> {
    if db.driver() != DatabaseDriver::Postgres {
        return Err(IngressUowError::PostgresRequired);
    }
    db.verify_attested_pg_writer_capability(capability)
        .map_err(DatabaseError::from)?;
    relation_guard.verify_for_database(db, capability)?;
    let entry = begin_ingress_entry(
        db,
        IngressEntryRequest {
            authority: IngressEntryAuthority::RelationGuard(relation_guard),
            acquisition: IngressEntryAcquisition::Immediate,
            acquisition_deadline: None,
            explicit_timeouts: None,
            clamp: IngressEntryClamp::Writer,
        },
    )
    .await?;
    let (tx, prepared) = entry.into_parts();
    let mut prepared = prepared.ok_or(IngressUowError::AttestedPoolRequired)?;
    prepared.enable_definer_api_candidate_for_test();
    Ok((tx, prepared))
}

async fn begin_raw_ingress_writer_inner<'a>(
    db: &'a Database,
) -> Result<Transaction<'a>, IngressUowError> {
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
        let (tx, prepared) = transaction.prepared_parts();
        match prepared {
            Some(prepared) if prepared.uses_relation_guard() => {
                lock_nowait_prepared(tx, prepared, message_key).await
            }
            _ => Self::lock_nowait_raw(tx, message_key).await,
        }
    }

    pub async fn lock_all_nowait(
        transaction: &mut IngressUowTransaction<'_>,
        descendant: Uuid,
    ) -> Result<(), IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        match prepared {
            Some(prepared) if prepared.uses_relation_guard() => {
                lock_all_nowait_prepared(tx, prepared, descendant).await
            }
            _ => Self::lock_all_nowait_raw(tx, descendant).await,
        }
    }

    pub async fn copy(
        transaction: &mut IngressUowTransaction<'_>,
        source: Uuid,
        target: Uuid,
    ) -> Result<(), IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        match prepared {
            Some(prepared) if prepared.uses_relation_guard() => {
                copy_prepared(tx, prepared, source, target).await
            }
            _ => Self::copy_raw(tx, source, target).await,
        }
    }

    pub async fn settle_all(
        transaction: &mut IngressUowTransaction<'_>,
        descendant: Uuid,
        now: DateTime<Utc>,
    ) -> Result<(), IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        match prepared {
            Some(prepared) if prepared.uses_relation_guard() => {
                settle_all_prepared(tx, prepared, descendant, now).await
            }
            _ => Self::settle_all_raw(tx, descendant, now).await,
        }
    }

    /// Fixed API variants for raw callers that retain the same prepared
    /// relation-guard transaction. These never begin or commit transactions.
    pub(crate) async fn lock_message_with_candidate_api(
        tx: &mut Transaction<'_>,
        api: &DefinerApiCandidateContext<'_>,
        message_key: MessageKey,
    ) -> Result<(), IngressUowError> {
        let query = format!(
            "SELECT {}(?::uuid)",
            api.qualified_function("waddle_ingress_lock_message_nowait"),
        );
        let mut rows = tx
            .query(
                &query,
                crate::db_params![message_key.to_storage().to_string()],
            )
            .await
            .map_err(canonical_nowait_error)?;
        let row = rows
            .next()
            .await
            .map_err(canonical_nowait_error)?
            .ok_or(IngressSubstrateError::InvalidDefinerApiResult)?;
        let locked: bool = row.get(0).map_err(canonical_nowait_error)?;
        if rows.next().await.map_err(canonical_nowait_error)?.is_some() {
            return Err(IngressSubstrateError::InvalidDefinerApiResult.into());
        }
        if !locked {
            return Err(IngressUowError::EffectIntentMessageMissing);
        }
        Ok(())
    }

    pub(crate) async fn lock_all_with_candidate_api(
        tx: &mut Transaction<'_>,
        api: &DefinerApiCandidateContext<'_>,
        descendant: Uuid,
    ) -> Result<(), IngressUowError> {
        let query = format!(
            "SELECT {}(?::uuid)",
            api.qualified_function("waddle_ingress_lock_descendant_ancestors_nowait"),
        );
        let mut rows = tx
            .query(&query, crate::db_params![descendant.to_string()])
            .await
            .map_err(canonical_nowait_error)?;
        let row = rows
            .next()
            .await
            .map_err(canonical_nowait_error)?
            .ok_or(IngressSubstrateError::InvalidDefinerApiResult)?;
        let locked: bool = row.get(0).map_err(canonical_nowait_error)?;
        if rows.next().await.map_err(canonical_nowait_error)?.is_some() {
            return Err(IngressSubstrateError::InvalidDefinerApiResult.into());
        }
        if !locked {
            return Err(IngressUowError::EffectIntentMessageMissing);
        }
        Ok(())
    }

    pub(crate) async fn attach_with_candidate_api(
        tx: &mut Transaction<'_>,
        api: &DefinerApiCandidateContext<'_>,
        message_key: MessageKey,
        kind: i32,
        semantic_identity_hash: &[u8],
        descendant: Uuid,
    ) -> Result<bool, IngressUowError> {
        let query = format!(
            "SELECT {}(?::uuid, ?::integer, ?::bytea, ?::uuid)",
            api.qualified_function("waddle_ingress_attach_descendant"),
        );
        candidate_boolean(
            tx,
            &query,
            crate::db_params![
                message_key.to_storage().to_string(),
                kind,
                semantic_identity_hash.to_vec(),
                descendant.to_string()
            ],
        )
        .await
    }

    pub(crate) async fn copy_with_candidate_api(
        tx: &mut Transaction<'_>,
        api: &DefinerApiCandidateContext<'_>,
        source: Uuid,
        target: Uuid,
    ) -> Result<(), IngressUowError> {
        let query = format!(
            "SELECT {}(?::uuid, ?::uuid)",
            api.qualified_function("waddle_ingress_copy_descendant"),
        );
        let copied = candidate_boolean(
            tx,
            &query,
            crate::db_params![source.to_string(), target.to_string()],
        )
        .await?;
        if copied {
            Ok(())
        } else {
            Err(IngressSubstrateError::InvalidDefinerApiResult.into())
        }
    }

    pub(crate) async fn settle_with_candidate_api(
        tx: &mut Transaction<'_>,
        api: &DefinerApiCandidateContext<'_>,
        descendant: Uuid,
        _now: DateTime<Utc>,
    ) -> Result<(), IngressUowError> {
        let query = format!(
            "SELECT {}(?::uuid)",
            api.qualified_function("waddle_ingress_settle_descendant"),
        );
        let settled =
            candidate_boolean(tx, &query, crate::db_params![descendant.to_string()]).await?;
        if settled {
            Ok(())
        } else {
            Err(IngressSubstrateError::InvalidDefinerApiResult.into())
        }
    }

    pub(crate) async fn bind_effect_with_candidate_api(
        tx: &mut Transaction<'_>,
        api: &DefinerApiCandidateContext<'_>,
        message_key: MessageKey,
        effect: &IngressEffectKey,
    ) -> Result<EffectDeliveryBinding, IngressUowError> {
        Self::lock_message_with_candidate_api(tx, api, message_key).await?;
        ensure_intent_with_candidate_api(tx, api, message_key, effect).await?;
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
        let outcome = crate::ingress_substrate::record_delivery_in_prepared_transaction(
            tx,
            Some(api.prepared()),
            key,
            message_key,
        )
        .await?;
        if outcome == MessageWriteOutcome::MessageVanished {
            return Err(IngressUowError::EffectIntentMessageMissing);
        }
        Ok(EffectDeliveryBinding::Bound(key))
    }

    pub(crate) async fn attach_effect_with_candidate_api(
        tx: &mut Transaction<'_>,
        api: &DefinerApiCandidateContext<'_>,
        message_key: MessageKey,
        effect: &IngressEffectKey,
        descendant: Uuid,
    ) -> Result<(), IngressUowError> {
        if Self::bind_effect_with_candidate_api(tx, api, message_key, effect).await?
            == EffectDeliveryBinding::AwaitingDurableOwner
        {
            return Err(IngressUowError::EffectIntentConflict);
        }
        Self::attach_with_candidate_api(
            tx,
            api,
            message_key,
            effect.storage_kind(),
            &identity_hash(effect),
            descendant,
        )
        .await?;
        Ok(())
    }

    #[expect(
        dead_code,
        reason = "the restricted definer API candidate is not selected by production ingress yet"
    )]
    pub(crate) async fn descendant_exists_with_candidate_api(
        tx: &mut Transaction<'_>,
        api: &DefinerApiCandidateContext<'_>,
        descendant_key: &str,
        pending_only: bool,
    ) -> Result<bool, IngressUowError> {
        let query = format!(
            "SELECT {}(?::text, ?::boolean)",
            api.qualified_function("waddle_ingress_descendant_exists"),
        );
        candidate_boolean(tx, &query, crate::db_params![descendant_key, pending_only]).await
    }

    pub(crate) async fn intent_exists_with_candidate_api(
        tx: &mut Transaction<'_>,
        api: &DefinerApiCandidateContext<'_>,
        message_key: MessageKey,
        effect: &IngressEffectKey,
    ) -> Result<bool, IngressUowError> {
        let query = format!(
            "SELECT {}(?::uuid, ?::integer, ?::bytea)",
            api.qualified_function("waddle_ingress_intent_exists"),
        );
        candidate_boolean(
            tx,
            &query,
            crate::db_params![
                message_key.to_storage().to_string(),
                effect.storage_kind(),
                identity_hash(effect).to_vec()
            ],
        )
        .await
    }

    #[expect(
        dead_code,
        reason = "the restricted definer API candidate is not selected by production ingress yet"
    )]
    pub(crate) async fn descendant_settled_with_candidate_api(
        tx: &mut Transaction<'_>,
        api: &DefinerApiCandidateContext<'_>,
        message_key: MessageKey,
        kind: i32,
        semantic_identity_hash: &[u8],
        descendant_key: &str,
    ) -> Result<bool, IngressUowError> {
        let query = format!(
            "SELECT {}(?::uuid, ?::integer, ?::bytea, ?::text)",
            api.qualified_function("waddle_ingress_descendant_settled"),
        );
        candidate_boolean(
            tx,
            &query,
            crate::db_params![
                message_key.to_storage().to_string(),
                kind,
                semantic_identity_hash.to_vec(),
                descendant_key
            ],
        )
        .await
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

    pub async fn bind_effect(
        transaction: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
        effect: &IngressEffectKey,
    ) -> Result<EffectDeliveryBinding, IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        match prepared {
            Some(prepared) if prepared.uses_relation_guard() => {
                bind_effect_prepared(tx, prepared, message_key, effect).await
            }
            _ => Self::bind_effect_raw(tx, message_key, effect).await,
        }
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
        let (tx, prepared) = transaction.prepared_parts();
        match prepared {
            Some(prepared) if prepared.uses_relation_guard() => {
                attach_prepared(tx, prepared, message_key, effect, descendant).await
            }
            _ => Self::attach_raw(tx, message_key, effect, descendant).await,
        }
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

async fn lock_nowait_prepared(
    tx: &mut Transaction<'_>,
    prepared: &PreparedAttestedReceiptWriter,
    message_key: MessageKey,
) -> Result<(), IngressUowError> {
    if prepared.uses_definer_api_candidate() {
        let api = DefinerApiCandidateContext::from_prepared(prepared)
            .ok_or(IngressSubstrateError::InvalidDefinerApiResult)?;
        return EffectDescendantRepository::lock_message_with_candidate_api(tx, &api, message_key)
            .await;
    }
    let query = prepared_ingress_sql(
        tx.driver(),
        Some(prepared),
        PreparedIngressRelation::Messages,
        (
            "SELECT 1 FROM ",
            " WHERE message_key = ?::uuid FOR UPDATE NOWAIT",
        ),
        "SELECT 1 FROM ingress_messages WHERE message_key = ?",
    );
    let mut rows = tx
        .query(
            &query,
            crate::db_params![message_key.to_storage().to_string()],
        )
        .await
        .map_err(canonical_nowait_error)?;
    if rows.next().await.map_err(canonical_nowait_error)?.is_none() {
        return Err(IngressUowError::EffectIntentMessageMissing);
    }
    Ok(())
}

async fn lock_all_nowait_prepared(
    tx: &mut Transaction<'_>,
    prepared: &PreparedAttestedReceiptWriter,
    descendant: Uuid,
) -> Result<(), IngressUowError> {
    if prepared.uses_definer_api_candidate() {
        let api = DefinerApiCandidateContext::from_prepared(prepared)
            .ok_or(IngressSubstrateError::InvalidDefinerApiResult)?;
        return EffectDescendantRepository::lock_all_with_candidate_api(tx, &api, descendant).await;
    }
    for key in ancestor_keys_in_prepared(tx, Some(prepared), descendant).await? {
        lock_nowait_prepared(tx, prepared, key).await?;
    }
    Ok(())
}

async fn copy_prepared(
    tx: &mut Transaction<'_>,
    prepared: &PreparedAttestedReceiptWriter,
    source: Uuid,
    target: Uuid,
) -> Result<(), IngressUowError> {
    if prepared.uses_definer_api_candidate() {
        let api = DefinerApiCandidateContext::from_prepared(prepared)
            .ok_or(IngressSubstrateError::InvalidDefinerApiResult)?;
        return EffectDescendantRepository::copy_with_candidate_api(tx, &api, source, target).await;
    }
    lock_all_nowait_prepared(tx, prepared, source).await?;
    let keys = ancestor_keys_in_prepared(tx, Some(prepared), source).await?;
    let relation =
        prepared_ingress_relation(Some(prepared), PreparedIngressRelation::EffectDescendants);
    let query = format!(
        "INSERT INTO {relation} (message_key, kind, semantic_identity_hash, descendant_key) \
         SELECT message_key, kind, semantic_identity_hash, ? FROM {relation} \
         WHERE descendant_key = ? AND settled_at IS NULL ON CONFLICT DO NOTHING"
    );
    tx.execute(
        &query,
        crate::db_params![target.to_string(), source.to_string()],
    )
    .await?;
    for key in keys {
        crate::ingress_substrate::invalidate_retention_in_prepared_transaction(
            tx,
            Some(prepared),
            key,
        )
        .await?;
    }
    Ok(())
}

async fn settle_all_prepared(
    tx: &mut Transaction<'_>,
    prepared: &PreparedAttestedReceiptWriter,
    descendant: Uuid,
    now: DateTime<Utc>,
) -> Result<(), IngressUowError> {
    if prepared.uses_definer_api_candidate() {
        let api = DefinerApiCandidateContext::from_prepared(prepared)
            .ok_or(IngressSubstrateError::InvalidDefinerApiResult)?;
        return EffectDescendantRepository::settle_with_candidate_api(tx, &api, descendant, now)
            .await;
    }
    lock_all_nowait_prepared(tx, prepared, descendant).await?;
    let keys = ancestor_keys_in_prepared(tx, Some(prepared), descendant).await?;
    let query = prepared_ingress_sql(
        tx.driver(),
        Some(prepared),
        PreparedIngressRelation::EffectDescendants,
        (
            "UPDATE ",
            " SET settled_at = ?::timestamptz WHERE descendant_key = ? AND settled_at IS NULL",
        ),
        "UPDATE ingress_effect_descendants SET settled_at = ? WHERE descendant_key = ? AND settled_at IS NULL",
    );
    tx.execute(
        &query,
        crate::db_params![now.to_rfc3339(), descendant.to_string()],
    )
    .await?;
    for key in keys {
        crate::ingress_substrate::refresh_retention_in_prepared_transaction(
            tx,
            Some(prepared),
            key,
            now,
        )
        .await?;
    }
    Ok(())
}

async fn bind_effect_prepared(
    tx: &mut Transaction<'_>,
    prepared: &PreparedAttestedReceiptWriter,
    message_key: MessageKey,
    effect: &IngressEffectKey,
) -> Result<EffectDeliveryBinding, IngressUowError> {
    lock_nowait_prepared(tx, prepared, message_key).await?;
    ensure_intent_in_prepared(tx, Some(prepared), message_key, effect).await?;
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
    let outcome = crate::ingress_substrate::record_delivery_in_prepared_transaction(
        tx,
        Some(prepared),
        key,
        message_key,
    )
    .await?;
    if outcome == MessageWriteOutcome::MessageVanished {
        return Err(IngressUowError::EffectIntentMessageMissing);
    }
    Ok(EffectDeliveryBinding::Bound(key))
}

async fn attach_prepared(
    tx: &mut Transaction<'_>,
    prepared: &crate::ingress_substrate::PreparedAttestedReceiptWriter,
    message_key: MessageKey,
    effect: &IngressEffectKey,
    descendant: Uuid,
) -> Result<(), IngressUowError> {
    if bind_effect_prepared(tx, prepared, message_key, effect).await?
        == EffectDeliveryBinding::AwaitingDurableOwner
    {
        return Err(IngressUowError::EffectIntentConflict);
    }
    if prepared.uses_definer_api_candidate() {
        let api = DefinerApiCandidateContext::from_prepared(prepared)
            .ok_or(IngressSubstrateError::InvalidDefinerApiResult)?;
        let _inserted = EffectDescendantRepository::attach_with_candidate_api(
            tx,
            &api,
            message_key,
            effect.storage_kind(),
            &identity_hash(effect),
            descendant,
        )
        .await?;
        return Ok(());
    }
    let query = prepared_ingress_sql(
        tx.driver(),
        Some(prepared),
        PreparedIngressRelation::EffectDescendants,
        ("INSERT INTO ", " (message_key, kind, semantic_identity_hash, descendant_key) VALUES (?::uuid, ?, ?, ?) ON CONFLICT DO NOTHING"),
        "INSERT INTO ingress_effect_descendants (message_key, kind, semantic_identity_hash, descendant_key) VALUES (?, ?, ?, ?) ON CONFLICT DO NOTHING",
    );
    let inserted = tx
        .execute(
            &query,
            crate::db_params![
                message_key.to_storage().to_string(),
                effect.storage_kind(),
                identity_hash(effect).to_vec(),
                descendant.to_string()
            ],
        )
        .await?;
    if inserted > 0 {
        crate::ingress_substrate::invalidate_retention_in_prepared_transaction(
            tx,
            Some(prepared),
            message_key,
        )
        .await?;
    }
    Ok(())
}

async fn ensure_intent(
    tx: &mut Transaction<'_>,
    message_key: MessageKey,
    effect: &IngressEffectKey,
) -> Result<(), IngressUowError> {
    ensure_intent_in_prepared(tx, None, message_key, effect).await
}

async fn ensure_intent_in_prepared(
    tx: &mut Transaction<'_>,
    prepared: Option<&crate::ingress_substrate::PreparedAttestedReceiptWriter>,
    message_key: MessageKey,
    effect: &IngressEffectKey,
) -> Result<(), IngressUowError> {
    if let Some(prepared) = prepared.filter(|prepared| prepared.uses_definer_api_candidate()) {
        let api = DefinerApiCandidateContext::from_prepared(prepared)
            .ok_or(IngressSubstrateError::InvalidDefinerApiResult)?;
        return ensure_intent_with_candidate_api(tx, &api, message_key, effect).await;
    }
    let query = crate::ingress_substrate::prepared_ingress_sql(tx.driver(), prepared,
        crate::ingress_substrate::PreparedIngressRelation::Effects,
        ("SELECT 1 FROM ", " WHERE message_key = ?::uuid AND kind = ? AND semantic_identity_hash = ?"),
        "SELECT 1 FROM ingress_effect_intents WHERE message_key = ? AND kind = ? AND semantic_identity_hash = ?");
    let mut rows = tx
        .query(
            &query,
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
    ancestor_keys_in_prepared(tx, None, descendant).await
}

async fn ancestor_keys_in_prepared(
    tx: &mut Transaction<'_>,
    prepared: Option<&PreparedAttestedReceiptWriter>,
    descendant: Uuid,
) -> Result<Vec<MessageKey>, IngressUowError> {
    let query = prepared_ingress_sql(
        tx.driver(),
        prepared,
        PreparedIngressRelation::EffectDescendants,
        (
            "SELECT DISTINCT message_key::text FROM ",
            " WHERE descendant_key = ? AND settled_at IS NULL ORDER BY message_key::text",
        ),
        "SELECT DISTINCT message_key FROM ingress_effect_descendants WHERE descendant_key = ? AND settled_at IS NULL ORDER BY message_key",
    );
    let mut rows = tx
        .query(&query, crate::db_params![descendant.to_string()])
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

async fn candidate_boolean(
    tx: &mut Transaction<'_>,
    query: &str,
    params: impl IntoParams,
) -> Result<bool, IngressUowError> {
    let mut rows = tx.query(query, params).await?;
    let row = rows
        .next()
        .await?
        .ok_or(IngressSubstrateError::InvalidDefinerApiResult)?;
    let result: bool = row.get(0)?;
    if rows.next().await?.is_some() {
        return Err(IngressSubstrateError::InvalidDefinerApiResult.into());
    }
    Ok(result)
}

async fn ensure_intent_with_candidate_api(
    tx: &mut Transaction<'_>,
    api: &DefinerApiCandidateContext<'_>,
    message_key: MessageKey,
    effect: &IngressEffectKey,
) -> Result<(), IngressUowError> {
    if EffectDescendantRepository::intent_exists_with_candidate_api(tx, api, message_key, effect)
        .await?
    {
        Ok(())
    } else {
        Err(IngressUowError::EffectIntentConflict)
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
