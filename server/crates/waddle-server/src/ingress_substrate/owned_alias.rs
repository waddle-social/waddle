//! Prepared PostgreSQL origin-alias resolution inside an owned ingress UOW.
//!
//! The caller retains the complete transaction while this module uses a
//! savepoint. Alias UPDATE/DELETE and candidate-message DELETE are not needed.
//! Privileged alias removal must follow canonical-message-first lock order.

use jid::BareJid;
use uuid::Uuid;
use waddle_xmpp::ingress::{
    AliasOutcome, AliasResolution, MessageKey, NormalizedTarget, SemanticDigest,
};
use waddle_xmpp_core::xep0359::OriginId;

use super::substrate::{
    decode_stored_alias, discard_database_error, insert_message,
    lock_message_for_child_in_prepared_transaction, prepared_ingress_relation,
    validate_stored_alias_identity, validate_stored_alias_identity_at, AliasStorageKey,
    IngressSubstrateError, PreparedAttestedReceiptWriter, PreparedIngressRelation,
};
use crate::db::{DatabaseDriver, Transaction};
use crate::ingress_uow::OwnedAliasScope;

#[cfg(test)]
#[derive(Clone)]
pub(crate) struct AliasGate {
    pub entered: std::sync::Arc<tokio::sync::Notify>,
    pub proceed: std::sync::Arc<tokio::sync::Notify>,
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SavepointPhase {
    Begin,
    Rollback,
    Release,
}

#[cfg(test)]
tokio::task_local! {
    pub(crate) static AFTER_MISS: AliasGate;
    pub(crate) static AFTER_INSERT: AliasGate;
    pub(crate) static AFTER_PROBE: AliasGate;
    pub(crate) static AFTER_LOCK: AliasGate;
    pub(crate) static STALL_SAVEPOINT: SavepointPhase;
}

#[cfg(test)]
async fn wait(gate: Option<AliasGate>) {
    if let Some(gate) = gate {
        gate.entered.notify_one();
        gate.proceed.notified().await;
    }
}

async fn savepoint(
    tx: &mut Transaction<'_>,
    statement: &'static str,
    #[cfg(test)] phase: SavepointPhase,
) -> Result<(), IngressSubstrateError> {
    tx.execute(statement, ())
        .await
        .map_err(discard_database_error)?;
    #[cfg(test)]
    if STALL_SAVEPOINT
        .try_with(|selected| *selected == phase)
        .unwrap_or(false)
    {
        // Hold the owned UOW after the savepoint command has completed.
        return tx
            .execute("SELECT pg_catalog.pg_sleep(3)", ())
            .await
            .map(|_| ())
            .map_err(discard_database_error);
    }
    Ok(())
}

/// Reachable only while the repository owns the complete ingress transaction.
/// A failure leaves its caller responsible for dropping that transaction.
pub(crate) async fn resolve_prepared_origin_in_scope(
    tx: &mut Transaction<'_>,
    prepared: &PreparedAttestedReceiptWriter,
    _scope: &OwnedAliasScope,
    sender: &BareJid,
    target: &NormalizedTarget,
    origin: &OriginId,
    digest: &SemanticDigest,
) -> Result<AliasResolution, IngressSubstrateError> {
    if tx.driver() != DatabaseDriver::Postgres {
        return Err(IngressSubstrateError::SchemaIdentityMismatch);
    }
    if prepared.uses_definer_api_candidate() {
        return super::substrate::resolve_alias_via_definer_api(
            tx,
            prepared,
            sender,
            target,
            origin,
            digest,
            None,
            MessageKey::new,
        )
        .await;
    }
    let alias = AliasStorageKey::new(sender, target, origin);
    if let Some(key) = probe_alias(tx, prepared, &alias).await? {
        #[cfg(test)]
        wait(AFTER_PROBE.try_with(Clone::clone).ok()).await;
        return locked_resolution(tx, prepared, &alias, key, digest).await;
    }

    #[cfg(test)]
    wait(AFTER_MISS.try_with(Clone::clone).ok()).await;

    let candidate = MessageKey::new();
    savepoint(
        tx,
        "SAVEPOINT waddle_ingress_origin_candidate",
        #[cfg(test)]
        SavepointPhase::Begin,
    )
    .await?;
    // The live caller records the envelope after resolving the alias; this
    // candidate must not assert a source binding on its behalf.
    if !insert_message(tx, Some(prepared), candidate, digest, None, None).await? {
        return Err(IngressSubstrateError::MessageContentConflict);
    }
    let sql = format!(
        "INSERT INTO {} (alias_key_hash, sender_bare_jid, target_kind, target_jid, origin_id, message_key) VALUES (?, ?, ?, ?, ?, ?::uuid) ON CONFLICT (alias_key_hash) DO NOTHING",
        prepared_ingress_relation(Some(prepared), PreparedIngressRelation::Aliases)
    );
    let inserted = tx
        .execute(
            &sql,
            crate::db_params![
                alias.hash.to_vec(),
                alias.sender.to_string(),
                alias.target.kind(),
                alias.target.jid(),
                alias.origin_id.as_str(),
                candidate.to_storage().to_string(),
            ],
        )
        .await
        .map_err(discard_database_error)?;
    if inserted == 1 {
        #[cfg(test)]
        wait(AFTER_INSERT.try_with(Clone::clone).ok()).await;
        savepoint(
            tx,
            "RELEASE SAVEPOINT waddle_ingress_origin_candidate",
            #[cfg(test)]
            SavepointPhase::Release,
        )
        .await?;
        return Ok(AliasResolution::Aliased(AliasOutcome::Inserted(candidate)));
    }

    savepoint(
        tx,
        "ROLLBACK TO SAVEPOINT waddle_ingress_origin_candidate",
        #[cfg(test)]
        SavepointPhase::Rollback,
    )
    .await?;
    savepoint(
        tx,
        "RELEASE SAVEPOINT waddle_ingress_origin_candidate",
        #[cfg(test)]
        SavepointPhase::Release,
    )
    .await?;
    // READ COMMITTED gives this lookup a fresh snapshot after the winning
    // transaction resolves. Never keep, delete, or publish the lost candidate.
    let key = probe_alias(tx, prepared, &alias)
        .await?
        .ok_or(IngressSubstrateError::AliasMissingAfterConflict)?;
    locked_resolution(tx, prepared, &alias, key, digest).await
}

async fn probe_alias(
    tx: &mut Transaction<'_>,
    prepared: &PreparedAttestedReceiptWriter,
    alias: &AliasStorageKey<'_>,
) -> Result<Option<MessageKey>, IngressSubstrateError> {
    let sql = format!(
        "SELECT message_key::text, sender_bare_jid, target_kind, target_jid, origin_id FROM {} WHERE alias_key_hash = ?",
        prepared_ingress_relation(Some(prepared), PreparedIngressRelation::Aliases),
    );
    let mut rows = tx
        .query(&sql, crate::db_params![alias.hash.to_vec()])
        .await
        .map_err(discard_database_error)?;
    let Some(row) = rows.next().await.map_err(discard_database_error)? else {
        return Ok(None);
    };
    validate_stored_alias_identity_at(&row, alias, 1)?;
    let key: String = row.get(0).map_err(discard_database_error)?;
    key.parse::<Uuid>()
        .map(MessageKey::from_storage)
        .map(Some)
        .map_err(|_| IngressSubstrateError::InvalidStoredMessageKey)
}

async fn locked_resolution(
    tx: &mut Transaction<'_>,
    prepared: &PreparedAttestedReceiptWriter,
    alias: &AliasStorageKey<'_>,
    key: MessageKey,
    digest: &SemanticDigest,
) -> Result<AliasResolution, IngressSubstrateError> {
    if !lock_message_for_child_in_prepared_transaction(tx, Some(prepared), key).await? {
        return Err(IngressSubstrateError::AliasMissingAfterConflict);
    }
    let sql = format!(
        "SELECT m.message_key::text, m.digest_version, m.digest, a.sender_bare_jid, a.target_kind, a.target_jid, a.origin_id FROM {} a JOIN {} m USING (message_key) WHERE a.alias_key_hash = ? AND m.message_key = ?::uuid",
        prepared_ingress_relation(Some(prepared), PreparedIngressRelation::Aliases),
        prepared_ingress_relation(Some(prepared), PreparedIngressRelation::Messages),
    );
    let mut rows = tx
        .query(
            &sql,
            crate::db_params![alias.hash.to_vec(), key.to_storage().to_string()],
        )
        .await
        .map_err(discard_database_error)?;
    let row = rows
        .next()
        .await
        .map_err(discard_database_error)?
        .ok_or(IngressSubstrateError::AliasMissingAfterConflict)?;
    validate_stored_alias_identity(&row, alias)?;
    let stored = decode_stored_alias(&row)?;
    if stored.key != key {
        return Err(IngressSubstrateError::AliasMissingAfterConflict);
    }
    #[cfg(test)]
    wait(AFTER_LOCK.try_with(Clone::clone).ok()).await;
    Ok(super::resolve_alias(
        true,
        digest,
        Some(&stored),
        MessageKey::new,
    ))
}
