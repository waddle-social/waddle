use chrono::{DateTime, Utc};
use jid::BareJid;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use uuid::Uuid;
use waddle_xmpp::auth::AuthenticatedPrincipalRef;
use waddle_xmpp::inbox::storage::GroupchatNotificationRecovery;
use waddle_xmpp::inbox::InboxEntry;
use waddle_xmpp::ingress::{
    AliasResolution, DeliveryKey, IngressEffectIntent, IngressOrdinal, MessageKey,
    NormalizedTarget, SemanticDigest, SmIngressId,
};
use waddle_xmpp::mam::{ArchivedMessage, MamTxStoreOutcome};
#[cfg(feature = "clustering")]
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp_core::xep0359::OriginId;

use crate::{
    ingress_substrate::{self, MessageWriteOutcome, TerminalizeOutcome},
    ingress_uow::{IngressUowError, IngressUowTransaction},
};

/// Repository for MAM archive rows written inside the ingress transaction.
#[derive(Debug, Default, Clone, Copy)]
pub struct MamArchiveRepository;

impl MamArchiveRepository {
    pub async fn store(
        transaction: &mut IngressUowTransaction<'_>,
        archive_jid: &BareJid,
        message: &ArchivedMessage,
    ) -> Result<MamTxStoreOutcome, IngressUowError> {
        let outcome = {
            let connection = transaction
                .transaction_mut()
                .postgres_connection()
                .ok_or(IngressUowError::PostgresRequired)?;
            waddle_xmpp::mam::store_archived_message_on_connection(connection, archive_jid, message)
                .await?
        };
        Ok(outcome)
    }

    /// Store a room archive row only after the exact room claim has been
    /// asserted in this transaction.
    #[cfg(feature = "clustering")]
    pub async fn store_fenced(
        transaction: &mut IngressUowTransaction<'_>,
        fence: &RoomClaimFence<'_>,
        archive_jid: &BareJid,
        message: &ArchivedMessage,
    ) -> Result<MamTxStoreOutcome, IngressUowError> {
        if fence.transaction_identity != transaction.identity() || fence.room != *archive_jid {
            return Err(IngressUowError::ClaimFenceMissing);
        }
        Self::store(transaction, archive_jid, message).await
    }
}

/// Repository for inbox projections written inside the ingress transaction.
#[derive(Debug, Default, Clone, Copy)]
pub struct InboxRepository;

impl InboxRepository {
    pub async fn upsert(
        transaction: &mut IngressUowTransaction<'_>,
        user: &BareJid,
        entry: InboxEntry,
        increment_unread: bool,
    ) -> Result<InboxEntry, IngressUowError> {
        crate::inbox::upsert_in_transaction(
            transaction.transaction_mut(),
            user,
            entry,
            increment_unread,
        )
        .await
        .map_err(Into::into)
    }

    /// Upsert an inbox row and its groupchat notification recovery item in
    /// the same ingress transaction.
    pub async fn upsert_with_groupchat_notification_recovery(
        transaction: &mut IngressUowTransaction<'_>,
        user: &BareJid,
        entry: InboxEntry,
        increment_unread: bool,
        recovery: GroupchatNotificationRecovery,
    ) -> Result<InboxEntry, IngressUowError> {
        let entry = Self::upsert(transaction, user, entry, increment_unread).await?;
        crate::inbox::insert_groupchat_notification_recovery_in_transaction(
            transaction.transaction_mut(),
            recovery,
        )
        .await?;
        Ok(entry)
    }
}

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

    #[cfg(feature = "clustering")]
    pub async fn message_keys_for_stream(
        transaction: &mut IngressUowTransaction<'_>,
        sm_ingress_id: SmIngressId,
    ) -> Result<Vec<MessageKey>, IngressUowError> {
        let mut rows = transaction
            .transaction_mut()
            .query(
                "SELECT DISTINCT message_key::text FROM ingress_sm_refs WHERE sm_ingress_id = ?::uuid",
                crate::db_params![sm_ingress_id.to_storage().to_string()],
            )
            .await?;
        let mut keys = Vec::new();
        while let Some(row) = rows.next().await? {
            let key: String = row.get(0)?;
            keys.push(
                key.parse::<Uuid>()
                    .map(MessageKey::from_storage)
                    .map_err(|_| {
                        IngressUowError::Substrate(
                            ingress_substrate::IngressSubstrateError::InvalidStoredMessageKey,
                        )
                    })?,
            );
        }
        Ok(keys)
    }

    #[cfg(feature = "clustering")]
    pub async fn delete_for_stream(
        transaction: &mut IngressUowTransaction<'_>,
        sm_ingress_id: SmIngressId,
    ) -> Result<u64, IngressUowError> {
        transaction
            .transaction_mut()
            .execute(
                "DELETE FROM ingress_sm_refs WHERE sm_ingress_id = ?::uuid",
                crate::db_params![sm_ingress_id.to_storage().to_string()],
            )
            .await
            .map_err(Into::into)
    }
}

/// Outcome of advancing the shadow stream's non-wrapping handled frontier.
#[cfg(feature = "clustering")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShadowFrontierOutcome {
    Advanced,
    Idempotent,
    Stale { stored: u64 },
}

/// Repository for shadow ingress-stream enrollment and its contiguous frontier.
#[cfg(feature = "clustering")]
#[derive(Debug, Default, Clone, Copy)]
pub struct SmIngressStreamRepository;

#[cfg(feature = "clustering")]
impl SmIngressStreamRepository {
    /// Mint the one durable shadow row for a freshly SM-enabled stream.
    pub async fn mint(
        transaction: &mut IngressUowTransaction<'_>,
        stream_id: &SmSessionId,
    ) -> Result<SmIngressId, IngressUowError> {
        let minted = SmIngressId::new();
        let inserted = transaction
            .transaction_mut()
            .execute(
                "INSERT INTO ingress_sm_streams (sm_ingress_id, stream_id) VALUES (?::uuid, ?) ON CONFLICT (stream_id) DO NOTHING",
                crate::db_params![minted.to_storage().to_string(), stream_id.as_str().to_string()],
            )
            .await?;
        if inserted == 1 {
            return Ok(minted);
        }
        let mut rows = transaction
            .transaction_mut()
            .query(
                "SELECT sm_ingress_id::text FROM ingress_sm_streams WHERE stream_id = ?",
                crate::db_params![stream_id.as_str().to_string()],
            )
            .await?;
        let stored: String = rows
            .next()
            .await?
            .ok_or(IngressUowError::SmIngressStreamMissing)?
            .get(0)?;
        stored
            .parse::<Uuid>()
            .map(SmIngressId::from_storage)
            .map_err(|_| IngressUowError::InvalidStoredSmIngressId)
    }

    /// Lock an existing enrolled stream. This path never enrolls a stream.
    pub async fn lock(
        transaction: &mut IngressUowTransaction<'_>,
        fence: &SmClaimFence<'_>,
        stream_id: &SmSessionId,
    ) -> Result<Option<(SmIngressId, u64)>, IngressUowError> {
        if fence.transaction_identity != transaction.identity() || fence.stream_id != *stream_id {
            return Err(IngressUowError::ClaimFenceMissing);
        }
        let mut rows = transaction
            .transaction_mut()
            .query(
                "SELECT sm_ingress_id::text, handled_ordinal::text FROM ingress_sm_streams WHERE stream_id = ? FOR UPDATE",
                crate::db_params![stream_id.as_str().to_string()],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        let id: String = row.get(0)?;
        let frontier: String = row.get(1)?;
        let id = id
            .parse::<Uuid>()
            .map(SmIngressId::from_storage)
            .map_err(|_| IngressUowError::InvalidStoredSmIngressId)?;
        let frontier = frontier
            .parse::<u64>()
            .map_err(|_| IngressUowError::InvalidStoredShadowFrontier)?;
        Ok(Some((id, frontier)))
    }

    pub async fn lookup_unclaimed(
        transaction: &mut IngressUowTransaction<'_>,
        stream_id: &SmSessionId,
    ) -> Result<Option<SmIngressId>, IngressUowError> {
        let mut rows = transaction
            .transaction_mut()
            .query(
                "SELECT sm_ingress_id::text FROM ingress_sm_streams WHERE stream_id = ? FOR UPDATE",
                crate::db_params![stream_id.as_str().to_string()],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        let id: String = row.get(0)?;
        id.parse::<Uuid>()
            .map(SmIngressId::from_storage)
            .map(Some)
            .map_err(|_| IngressUowError::InvalidStoredSmIngressId)
    }

    /// Fence a retirement's unclaimed check against concurrent claim writes.
    /// A missing row cannot be protected by a row lock, so this rare cleanup
    /// path holds the claims table's SHARE lock through its shadow deletion.
    #[cfg(feature = "clustering")]
    pub async fn fence_claim_absence_for_retirement(
        transaction: &mut IngressUowTransaction<'_>,
        stream_id: &SmSessionId,
    ) -> Result<bool, IngressUowError> {
        transaction
            .transaction_mut()
            .execute("LOCK TABLE clustering_claims IN SHARE MODE", ())
            .await?;
        let mut rows = transaction
            .transaction_mut()
            .query(
                "SELECT 1 FROM clustering_claims WHERE entity = ? AND entity_type = ?",
                crate::db_params![
                    format!("sm_session:{}", stream_id.as_str()),
                    "sm_session".to_string(),
                ],
            )
            .await?;
        Ok(rows.next().await?.is_none())
    }

    /// Advance only the next contiguous shadow ordinal for the locked stream.
    pub async fn advance_frontier(
        transaction: &mut IngressUowTransaction<'_>,
        fence: &SmClaimFence<'_>,
        sm_ingress_id: SmIngressId,
        allocated: IngressOrdinal,
    ) -> Result<ShadowFrontierOutcome, IngressUowError> {
        if fence.transaction_identity != transaction.identity() {
            return Err(IngressUowError::ClaimFenceMissing);
        }
        let mut rows = transaction
            .transaction_mut()
            .query(
                "SELECT handled_ordinal::text FROM ingress_sm_streams WHERE sm_ingress_id = ?::uuid AND stream_id = ? FOR UPDATE",
                crate::db_params![
                    sm_ingress_id.to_storage().to_string(),
                    fence.stream_id.as_str().to_string(),
                ],
            )
            .await?;
        let stored: String = rows
            .next()
            .await?
            .ok_or(IngressUowError::SmIngressStreamMissing)?
            .get(0)?;
        drop(rows);
        let stored = stored
            .parse::<u64>()
            .map_err(|_| IngressUowError::InvalidStoredShadowFrontier)?;
        let allocated = allocated.to_storage();
        if stored >= allocated {
            return Ok(ShadowFrontierOutcome::Idempotent);
        }
        if stored != allocated - 1 {
            return Ok(ShadowFrontierOutcome::Stale { stored });
        }
        let updated = transaction
            .transaction_mut()
            .execute(
                "UPDATE ingress_sm_streams SET handled_ordinal = ?::numeric, row_revision = row_revision + 1, updated_at = now() WHERE sm_ingress_id = ?::uuid AND stream_id = ? AND handled_ordinal = ?::numeric",
                crate::db_params![
                    allocated.to_string(),
                    sm_ingress_id.to_storage().to_string(),
                    fence.stream_id.as_str().to_string(),
                    stored.to_string(),
                ],
            )
            .await?;
        if updated == 1 {
            Ok(ShadowFrontierOutcome::Advanced)
        } else {
            Ok(ShadowFrontierOutcome::Stale { stored })
        }
    }

    pub async fn delete_unclaimed(
        transaction: &mut IngressUowTransaction<'_>,
        stream_id: &SmSessionId,
    ) -> Result<u64, IngressUowError> {
        transaction
            .transaction_mut()
            .execute(
                "DELETE FROM ingress_sm_streams WHERE stream_id = ?",
                crate::db_params![stream_id.as_str().to_string()],
            )
            .await
            .map_err(Into::into)
    }
}

/// Outcome of writing the immutable logical effects required by a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectIntentWriteOutcome {
    Recorded,
    AlreadyRecorded,
    IntentDivergence,
}

/// Repository for inert, deterministic effect-intent rows.
#[derive(Debug, Default, Clone, Copy)]
pub struct EffectIntentRepository;

impl EffectIntentRepository {
    pub async fn record_all(
        transaction: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
        intents: &[IngressEffectIntent],
    ) -> Result<EffectIntentWriteOutcome, IngressUowError> {
        Self::record_all_inner(transaction, message_key, intents, false).await
    }

    pub async fn record_all_existing_alias(
        transaction: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
        intents: &[IngressEffectIntent],
    ) -> Result<EffectIntentWriteOutcome, IngressUowError> {
        Self::record_all_inner(transaction, message_key, intents, true).await
    }

    async fn record_all_inner(
        transaction: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
        intents: &[IngressEffectIntent],
        allow_existing_alias_divergence: bool,
    ) -> Result<EffectIntentWriteOutcome, IngressUowError> {
        let mut message = transaction
            .transaction_mut()
            .query(
                "SELECT 1 FROM ingress_messages WHERE message_key = ?::uuid FOR UPDATE",
                crate::db_params![message_key.to_storage().to_string()],
            )
            .await?;
        if message.next().await?.is_none() {
            return Err(IngressUowError::EffectIntentMessageMissing);
        }
        drop(message);

        let mut ordered = intents
            .iter()
            .map(|intent| {
                let semantic_key = intent.semantic_key();
                intent
                    .with_encoded_v1(|kind, payload| {
                        (
                            semantic_key,
                            CanonicalEffectIntentRow {
                                kind,
                                semantic_identity_hash: semantic_identity_hash(intent),
                                effect_ordinal: 0,
                                payload: payload.to_vec(),
                            },
                        )
                    })
                    .map_err(IngressUowError::from)
            })
            .collect::<Result<Vec<_>, IngressUowError>>()?;
        ordered.sort_by(|left, right| left.0.cmp(&right.0));
        let mut canonical = Vec::with_capacity(ordered.len());
        for (key, row) in ordered {
            if let Some((previous_key, previous_row)) = canonical.last() {
                if *previous_key == key {
                    if previous_row != &row {
                        return Err(IngressUowError::EffectIntentConflict);
                    }
                    continue;
                }
            }
            canonical.push((key, row));
        }
        for (ordinal, (_, row)) in canonical.iter_mut().enumerate() {
            row.effect_ordinal =
                u64::try_from(ordinal).map_err(|_| IngressUowError::EffectIntentOrdinalOverflow)?;
        }

        let mut existing_rows = transaction
            .transaction_mut()
            .query(
                "SELECT kind::int, semantic_identity_hash, effect_ordinal::text, payload_version::int, payload FROM ingress_effect_intents WHERE message_key = ?::uuid ORDER BY effect_ordinal FOR SHARE",
                crate::db_params![message_key.to_storage().to_string()],
            )
            .await?;
        let mut existing = BTreeMap::new();
        while let Some(row) = existing_rows.next().await? {
            let kind: i64 = row.get(0)?;
            let semantic_identity_hash: Vec<u8> = row.get(1)?;
            let effect_ordinal: String = row.get(2)?;
            let payload_version: i64 = row.get(3)?;
            let payload: Vec<u8> = row.get(4)?;
            existing.insert(
                (
                    i32::try_from(kind).map_err(|_| IngressUowError::EffectIntentConflict)?,
                    storage_hash(&semantic_identity_hash)?,
                ),
                StoredEffectIntentRow {
                    effect_ordinal: effect_ordinal
                        .parse::<u64>()
                        .map_err(|_| IngressUowError::EffectIntentConflict)?,
                    payload_version,
                    payload,
                },
            );
        }

        if existing.is_empty() && allow_existing_alias_divergence {
            return Ok(if canonical.is_empty() {
                EffectIntentWriteOutcome::AlreadyRecorded
            } else {
                EffectIntentWriteOutcome::IntentDivergence
            });
        }

        if !existing.is_empty() {
            let mut matches_existing = true;
            for (_, row) in &canonical {
                let key = (row.kind, row.semantic_identity_hash);
                let Some(stored) = existing.remove(&key) else {
                    matches_existing = false;
                    break;
                };
                if stored.effect_ordinal != row.effect_ordinal
                    || stored.payload_version != 1
                    || stored.payload != row.payload
                {
                    matches_existing = false;
                    break;
                }
            }
            if matches_existing && existing.is_empty() {
                return Ok(EffectIntentWriteOutcome::AlreadyRecorded);
            }
            return if allow_existing_alias_divergence {
                Ok(EffectIntentWriteOutcome::IntentDivergence)
            } else {
                Err(IngressUowError::EffectIntentConflict)
            };
        }

        for (_, row) in canonical {
            transaction
                .transaction_mut()
                .execute(
                    "INSERT INTO ingress_effect_intents (message_key, effect_ordinal, kind, semantic_identity_hash, payload_version, payload) VALUES (?::uuid, ?::numeric, ?, ?, 1, ?)",
                    crate::db_params![
                        message_key.to_storage().to_string(),
                        row.effect_ordinal.to_string(),
                        i64::from(row.kind),
                        row.semantic_identity_hash.to_vec(),
                        row.payload,
                    ],
                )
                .await?;
        }

        Ok(EffectIntentWriteOutcome::Recorded)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CanonicalEffectIntentRow {
    kind: i32,
    semantic_identity_hash: [u8; 32],
    effect_ordinal: u64,
    payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredEffectIntentRow {
    effect_ordinal: u64,
    payload_version: i64,
    payload: Vec<u8>,
}

fn semantic_identity_hash(intent: &IngressEffectIntent) -> [u8; 32] {
    let identity = intent.semantic_key().storage_identity();
    Sha256::digest(identity.as_bytes()).into()
}

fn storage_hash(value: &[u8]) -> Result<[u8; 32], IngressUowError> {
    <[u8; 32]>::try_from(value).map_err(|_| IngressUowError::EffectIntentConflict)
}

/// Result of checking a persisted authenticated principal under a share lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrincipalAssertion {
    Asserted,
    PrincipalAssertionFailed,
}

/// Repository for the durable authority check of an authenticated principal.
#[derive(Debug, Default, Clone, Copy)]
pub struct PrincipalRepository;

impl PrincipalRepository {
    pub async fn assert_principal(
        transaction: &mut IngressUowTransaction<'_>,
        principal: &AuthenticatedPrincipalRef,
    ) -> Result<PrincipalAssertion, IngressUowError> {
        let mut rows = transaction
            .transaction_mut()
            .query(
                r#"
                SELECT expires_at FROM sessions
                WHERE user_jid = ?
                  AND auth_context_id = ?
                  AND auth_context_version = ?
                  AND principal_auth_epoch = ?
                FOR SHARE
                "#,
                crate::db_params![
                    principal.bare_jid().to_string(),
                    principal.auth_context_id().as_uuid().to_string(),
                    i64::try_from(principal.auth_context_version().get())
                        .map_err(|_| IngressUowError::PrincipalReferenceOutOfRange)?,
                    i64::try_from(principal.auth_epoch().get())
                        .map_err(|_| IngressUowError::PrincipalReferenceOutOfRange)?,
                ],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(PrincipalAssertion::PrincipalAssertionFailed);
        };
        let expires_at: Option<String> = row.get(0)?;
        let expired = expires_at
            .map(|raw| {
                DateTime::parse_from_rfc3339(&raw)
                    .map(|expires_at| Utc::now() >= expires_at.with_timezone(&Utc))
                    .map_err(|_| IngressUowError::InvalidStoredPrincipalExpiry)
            })
            .transpose()?
            .unwrap_or(false);
        Ok(if expired {
            PrincipalAssertion::PrincipalAssertionFailed
        } else {
            PrincipalAssertion::Asserted
        })
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
use waddle_xmpp::ownership::{ClaimEpoch, EntityType, NodeIdentity};

/// Repository that proves exact SM ownership before a fenced SM write.
#[cfg(feature = "clustering")]
#[derive(Debug, Default, Clone, Copy)]
pub struct ClaimRepository;

/// Non-forgeable proof that this transaction holds one exact SM claim under
/// current local node authority.
///
/// It can only be minted after both the node-authority currency check
/// (against the canonical `SharedNodeIdentity` bound at
/// [`super::PostgresIngressUnitOfWork::open_with_node_identity`]) and the
/// `FOR SHARE` claim assertion. The minted `CurrentNodeIdentityGuard` is
/// retained by the transaction itself — not this independently droppable
/// value — so identity rotation or terminal disable cannot complete until
/// the transaction commits or rolls back, the same
/// publication-after-revocation gate the fenced SM persistence path holds
/// through its transactions. Its lifetime is tied to the unit of work so a
/// caller cannot carry it into another one.
#[cfg(feature = "clustering")]
#[derive(Debug)]
pub struct SmClaimFence<'transaction> {
    stream_id: SmSessionId,
    transaction_identity: Uuid,
    _transaction: PhantomData<&'transaction IngressUowTransaction<'transaction>>,
}

/// Non-forgeable proof that this transaction holds one exact room claim under
/// current local node authority.
///
/// It can only be minted after both the node-authority currency check
/// (against the canonical `SharedNodeIdentity` bound at
/// [`super::PostgresIngressUnitOfWork::open_with_node_identity`]) and the
/// `FOR SHARE` claim assertion. The minted `CurrentNodeIdentityGuard` is
/// retained by the transaction itself — not this independently droppable
/// value — so identity rotation or terminal disable cannot complete until
/// the transaction commits or rolls back, the same
/// publication-after-revocation gate the fenced room archive path holds
/// through its transactions. Its lifetime is tied to the unit of work so a
/// caller cannot carry it into another one.
#[cfg(feature = "clustering")]
#[derive(Debug)]
pub struct RoomClaimFence<'transaction> {
    room: BareJid,
    transaction_identity: Uuid,
    _transaction: PhantomData<&'transaction IngressUowTransaction<'transaction>>,
}

#[cfg(feature = "clustering")]
impl ClaimRepository {
    /// Assert the exact `(entity, node id, node incarnation, claim epoch)`
    /// row under `FOR SHARE`, after proving `owner` is still this node's
    /// current, active identity per the transaction's bound canonical
    /// identity source. Locks are taken in the fixed order
    /// epoch (at [`super::PostgresIngressUnitOfWork::begin`]) → exact claim
    /// (here) → `sm_sessions`/child rows (fenced repositories).
    pub async fn assert_sm_claim<'transaction>(
        transaction: &mut IngressUowTransaction<'transaction>,
        stream_id: &SmSessionId,
        owner: &NodeIdentity,
        claim_epoch: ClaimEpoch,
    ) -> Result<SmClaimFence<'transaction>, IngressUowError> {
        let Some(node_identity) = transaction.bound_node_identity().cloned() else {
            return Err(IngressUowError::NodeIdentityUnbound);
        };
        let Some(authority) = node_identity.guard_if_current(owner).await else {
            return Err(IngressUowError::ClaimFenceMissing);
        };
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
                    authority.identity().node_id.clone(),
                    authority.identity().node_epoch.clone(),
                    claim_epoch.0,
                ],
            )
            .await?;
        if rows.next().await?.is_none() {
            return Err(IngressUowError::ClaimFenceMissing);
        }
        drop(rows);
        transaction.retain_authority(authority);
        Ok(SmClaimFence {
            stream_id: stream_id.clone(),
            transaction_identity: transaction.identity(),
            _transaction: PhantomData,
        })
    }

    /// Assert the exact `(entity, node id, node incarnation, claim epoch)`
    /// row under `FOR SHARE`, after proving `owner` is still this node's
    /// current, active identity per the transaction's bound canonical
    /// identity source. Locks are taken in the fixed order
    /// epoch (at [`super::PostgresIngressUnitOfWork::begin`]) → exact claim
    /// (here) → room archive/child rows (fenced repositories).
    pub async fn assert_room_claim<'transaction>(
        transaction: &mut IngressUowTransaction<'transaction>,
        room: &BareJid,
        owner: &NodeIdentity,
        claim_epoch: ClaimEpoch,
    ) -> Result<RoomClaimFence<'transaction>, IngressUowError> {
        let Some(node_identity) = transaction.bound_node_identity().cloned() else {
            return Err(IngressUowError::NodeIdentityUnbound);
        };
        let Some(authority) = node_identity.guard_if_current(owner).await else {
            return Err(IngressUowError::ClaimFenceMissing);
        };
        let entity = format!("{}:{}", EntityType::RoomActor.as_db_str(), room);
        let mut rows = transaction
            .transaction_mut()
            .query(
                "SELECT 1 FROM clustering_claims WHERE entity = ? AND node_id = ? AND node_epoch = ? AND claim_epoch = ? FOR SHARE",
                crate::db_params![
                    entity,
                    authority.identity().node_id.clone(),
                    authority.identity().node_epoch.clone(),
                    claim_epoch.0,
                ],
            )
            .await?;
        if rows.next().await?.is_none() {
            return Err(IngressUowError::ClaimFenceMissing);
        }
        drop(rows);
        transaction.retain_authority(authority);
        Ok(RoomClaimFence {
            room: room.clone(),
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
