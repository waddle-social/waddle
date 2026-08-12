use chrono::{DateTime, Utc};
use jid::BareJid;
use uuid::Uuid;
use waddle_xmpp::ingress::{
    AliasResolution, DeliveryKey, IngressOrdinal, MessageKey, NormalizedTarget, SemanticDigest,
    SmIngressId,
};
use waddle_xmpp_core::xep0359::OriginId;

use crate::{
    ingress_substrate::{self, MessageWriteOutcome, TerminalizeOutcome},
    ingress_uow::{IngressUowError, IngressUowTransaction},
};

/// Repository for canonical ingress messages and their origin-id aliases.
#[derive(Debug, Default, Clone, Copy)]
pub struct CanonicalMessageRepository;

impl CanonicalMessageRepository {
    pub async fn record(
        transaction: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
        digest: &SemanticDigest,
    ) -> Result<(), IngressUowError> {
        ingress_substrate::record_message(transaction.transaction_mut(), message_key, digest)
            .await
            .map_err(Into::into)
    }

    pub async fn resolve_and_record_alias(
        transaction: &mut IngressUowTransaction<'_>,
        sender: &BareJid,
        target: &NormalizedTarget,
        origin_id: &OriginId,
        digest: &SemanticDigest,
        mint: impl FnOnce() -> MessageKey,
    ) -> Result<AliasResolution, IngressUowError> {
        ingress_substrate::resolve_and_record_alias(
            transaction.transaction_mut(),
            sender,
            target,
            origin_id,
            digest,
            mint,
        )
        .await
        .map_err(Into::into)
    }

    pub async fn terminalize(
        transaction: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
        proven_terminal_at: DateTime<Utc>,
    ) -> Result<TerminalizeOutcome, IngressUowError> {
        ingress_substrate::terminalize_message(
            transaction.transaction_mut(),
            message_key,
            proven_terminal_at,
        )
        .await
        .map_err(Into::into)
    }
}

/// Repository for exact stream-management ingress references.
#[derive(Debug, Default, Clone, Copy)]
pub struct SmIngressRepository;

impl SmIngressRepository {
    pub async fn insert(
        transaction: &mut IngressUowTransaction<'_>,
        sm_ingress_id: SmIngressId,
        ordinal: IngressOrdinal,
        message_key: MessageKey,
    ) -> Result<MessageWriteOutcome, IngressUowError> {
        ingress_substrate::insert_sm_ref(
            transaction.transaction_mut(),
            sm_ingress_id,
            ordinal,
            message_key,
        )
        .await
        .map_err(Into::into)
    }

    pub async fn lookup(
        transaction: &mut IngressUowTransaction<'_>,
        sm_ingress_id: SmIngressId,
        ordinal: IngressOrdinal,
    ) -> Result<Option<MessageKey>, IngressUowError> {
        lookup_message_key(
            transaction,
            r#"
            SELECT message_key::text FROM ingress_sm_refs
            WHERE sm_ingress_id = ?::uuid AND ingress_ordinal = ?::numeric
            "#,
            crate::db_params![
                sm_ingress_id.to_storage().to_string(),
                ordinal.to_storage().to_string(),
            ],
        )
        .await
    }
}

/// Repository for durable delivery/effect identities.
#[derive(Debug, Default, Clone, Copy)]
pub struct DeliveryEffectRepository;

impl DeliveryEffectRepository {
    pub async fn record(
        transaction: &mut IngressUowTransaction<'_>,
        delivery_key: DeliveryKey,
        message_key: MessageKey,
    ) -> Result<MessageWriteOutcome, IngressUowError> {
        ingress_substrate::record_delivery(transaction.transaction_mut(), delivery_key, message_key)
            .await
            .map_err(Into::into)
    }

    pub async fn lookup(
        transaction: &mut IngressUowTransaction<'_>,
        delivery_key: DeliveryKey,
    ) -> Result<Option<MessageKey>, IngressUowError> {
        lookup_message_key(
            transaction,
            "SELECT message_key::text FROM ingress_deliveries WHERE delivery_key = ?::uuid",
            crate::db_params![delivery_key.to_storage().to_string()],
        )
        .await
    }
}

async fn lookup_message_key(
    transaction: &mut IngressUowTransaction<'_>,
    sql: &str,
    params: impl crate::db::IntoParams,
) -> Result<Option<MessageKey>, IngressUowError> {
    let mut rows = transaction.transaction_mut().query(sql, params).await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let stored: String = row.get(0)?;
    let key = stored
        .parse::<Uuid>()
        .map(MessageKey::from_storage)
        .map_err(|_| ingress_substrate::IngressSubstrateError::InvalidStoredMessageKey)?;
    Ok(Some(key))
}

#[cfg(feature = "clustering")]
use std::marker::PhantomData;
#[cfg(feature = "clustering")]
use waddle_xmpp::{
    ownership::{ClaimEpoch, EntityType, NodeIdentity},
    pending_delivery::SmSessionId,
};

/// Repository that proves exact SM ownership before a fenced SM write.
#[cfg(feature = "clustering")]
#[derive(Debug, Default, Clone, Copy)]
pub struct ClaimRepository;

/// Non-forgeable proof that this transaction holds one exact SM claim.
///
/// It can only be minted after the `FOR SHARE` claim assertion. Its lifetime
/// is tied to the unit of work so a caller cannot carry it into another one.
#[cfg(feature = "clustering")]
#[derive(Debug)]
pub struct SmClaimFence<'transaction> {
    stream_id: SmSessionId,
    transaction_identity: Uuid,
    _transaction: PhantomData<&'transaction IngressUowTransaction<'transaction>>,
}

#[cfg(feature = "clustering")]
impl ClaimRepository {
    pub async fn assert_sm_claim<'transaction>(
        transaction: &mut IngressUowTransaction<'transaction>,
        stream_id: &SmSessionId,
        owner: &NodeIdentity,
        claim_epoch: ClaimEpoch,
    ) -> Result<SmClaimFence<'transaction>, IngressUowError> {
        let entity = format!(
            "{}:{}",
            EntityType::SmSession.as_db_str(),
            stream_id.as_str()
        );
        let mut rows = transaction
            .transaction_mut()
            .query(
                "SELECT 1 FROM clustering_claims WHERE entity = ? AND node_id = ? AND node_epoch = ? AND claim_epoch = ? FOR SHARE",
                crate::db_params![
                    entity,
                    owner.node_id.clone(),
                    owner.node_epoch.clone(),
                    claim_epoch.0,
                ],
            )
            .await?;
        if rows.next().await?.is_none() {
            return Err(IngressUowError::ClaimFenceMissing);
        }
        Ok(SmClaimFence {
            stream_id: stream_id.clone(),
            transaction_identity: transaction.identity(),
            _transaction: PhantomData,
        })
    }
}

/// Result of offering a handled XEP-0198 frontier to an SM session.
#[cfg(feature = "clustering")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandledFrontierOutcome {
    Idempotent,
    Advanced,
}

/// Repository that advances the fenced, wrapping SM handled frontier.
#[cfg(feature = "clustering")]
#[derive(Debug, Default, Clone, Copy)]
pub struct HandledFrontierRepository;

#[cfg(feature = "clustering")]
impl HandledFrontierRepository {
    pub async fn advance(
        transaction: &mut IngressUowTransaction<'_>,
        fence: &SmClaimFence<'_>,
        stream_id: &SmSessionId,
        offered: u32,
    ) -> Result<HandledFrontierOutcome, IngressUowError> {
        if fence.transaction_identity != transaction.identity() || fence.stream_id != *stream_id {
            return Err(IngressUowError::ClaimFenceMissing);
        }
        let mut rows = transaction
            .transaction_mut()
            .query(
                "SELECT inbound_count FROM sm_sessions WHERE stream_id = ? FOR UPDATE",
                crate::db_params![stream_id.as_str().to_string()],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Err(IngressUowError::StreamMissing);
        };
        let stored: i64 = row.get(0)?;
        let stored = stored as u32;
        drop(rows);

        if offered == stored {
            return Ok(HandledFrontierOutcome::Idempotent);
        }
        if offered != stored.wrapping_add(1) {
            return Err(IngressUowError::FrontierStale { stored, offered });
        }
        let advanced = transaction
            .transaction_mut()
            .execute(
                "UPDATE sm_sessions SET inbound_count = ? WHERE stream_id = ? AND inbound_count = ?",
                crate::db_params![
                    i64::from(offered),
                    stream_id.as_str().to_string(),
                    i64::from(stored),
                ],
            )
            .await?;
        if advanced != 1 {
            return Err(IngressUowError::FrontierStale { stored, offered });
        }
        Ok(HandledFrontierOutcome::Advanced)
    }
}
