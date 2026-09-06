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
    NormalizedTarget, SemanticDigest, SmIngressId, WireHandledCount,
};
use waddle_xmpp::mam::{ArchivedMessage, MamTxStoreOutcome};
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp_core::xep0359::OriginId;

use crate::{
    db::{Database, DatabaseDriver},
    ingress_substrate::{
        self, EffectReceiptKind, MessageEnvelope, MessageWriteOutcome, TerminalizeOutcome,
    },
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
    pub async fn record_message(
        transaction: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
        digest: &SemanticDigest,
        envelope: Option<&MessageEnvelope>,
    ) -> Result<(), IngressUowError> {
        ingress_substrate::record_message(
            transaction.transaction_mut(),
            message_key,
            digest,
            envelope,
        )
        .await
        .map_err(Into::into)
    }

    pub async fn load_envelope(
        transaction: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
    ) -> Result<Option<MessageEnvelope>, IngressUowError> {
        ingress_substrate::load_envelope(transaction.transaction_mut(), message_key)
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
    pub async fn insert_sm_ref(
        transaction: &mut IngressUowTransaction<'_>,
        sm_ingress_id: SmIngressId,
        ordinal: IngressOrdinal,
        wire_h: WireHandledCount,
        message_key: MessageKey,
    ) -> Result<MessageWriteOutcome, IngressUowError> {
        ingress_substrate::insert_sm_ref(
            transaction.transaction_mut(),
            sm_ingress_id,
            ordinal,
            wire_h,
            message_key,
        )
        .await
        .map_err(Into::into)
    }

    pub async fn lookup_wire_binding(
        transaction: &mut IngressUowTransaction<'_>,
        sm_ingress_id: SmIngressId,
        wire_h: WireHandledCount,
    ) -> Result<Option<(MessageKey, IngressOrdinal)>, IngressUowError> {
        ingress_substrate::lookup_wire_binding(transaction.transaction_mut(), sm_ingress_id, wire_h)
            .await
            .map_err(Into::into)
    }

    pub async fn lookup(
        transaction: &mut IngressUowTransaction<'_>,
        sm_ingress_id: SmIngressId,
        ordinal: IngressOrdinal,
    ) -> Result<Option<MessageKey>, IngressUowError> {
        const POSTGRES: &str = "SELECT message_key::text FROM ingress_sm_refs WHERE sm_ingress_id = ?::uuid AND ingress_ordinal = ?::numeric";
        const SQLITE: &str = "SELECT message_key FROM ingress_sm_refs WHERE sm_ingress_id = ? AND ingress_ordinal = ?";
        let sql = dialect_sql(transaction, POSTGRES, SQLITE);
        lookup_message_key(
            transaction,
            sql,
            crate::db_params![
                sm_ingress_id.to_storage().to_string(),
                ordinal.to_storage().to_string(),
            ],
        )
        .await
    }

    pub async fn message_keys_for_stream(
        transaction: &mut IngressUowTransaction<'_>,
        sm_ingress_id: SmIngressId,
    ) -> Result<Vec<MessageKey>, IngressUowError> {
        const POSTGRES: &str =
            "SELECT DISTINCT message_key::text FROM ingress_sm_refs WHERE sm_ingress_id = ?::uuid";
        const SQLITE: &str =
            "SELECT DISTINCT message_key FROM ingress_sm_refs WHERE sm_ingress_id = ?";
        let sql = dialect_sql(transaction, POSTGRES, SQLITE);
        let mut rows = transaction
            .transaction_mut()
            .query(
                sql,
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

    pub async fn delete_for_stream(
        transaction: &mut IngressUowTransaction<'_>,
        sm_ingress_id: SmIngressId,
    ) -> Result<u64, IngressUowError> {
        const POSTGRES: &str = "DELETE FROM ingress_sm_refs WHERE sm_ingress_id = ?::uuid";
        const SQLITE: &str = "DELETE FROM ingress_sm_refs WHERE sm_ingress_id = ?";
        let sql = dialect_sql(transaction, POSTGRES, SQLITE);
        transaction
            .transaction_mut()
            .execute(
                sql,
                crate::db_params![sm_ingress_id.to_storage().to_string()],
            )
            .await
            .map_err(Into::into)
    }
}

pub use ingress_substrate::FrontierOutcome;

/// Repository for ingress-stream enrollment and its contiguous frontier.
#[derive(Debug, Default, Clone, Copy)]
pub struct SmIngressStreamRepository;

impl SmIngressStreamRepository {
    /// Mint the one durable ingress row for a freshly SM-enabled stream.
    pub async fn mint(
        transaction: &mut IngressUowTransaction<'_>,
        stream_id: &SmSessionId,
    ) -> Result<SmIngressId, IngressUowError> {
        const POSTGRES: &str = "INSERT INTO ingress_sm_streams (sm_ingress_id, stream_id) VALUES (?::uuid, ?) ON CONFLICT (stream_id) DO NOTHING";
        const SQLITE: &str = "INSERT INTO ingress_sm_streams (sm_ingress_id, stream_id) VALUES (?, ?) ON CONFLICT (stream_id) DO NOTHING";
        let sql = dialect_sql(transaction, POSTGRES, SQLITE);
        let minted = SmIngressId::new();
        let inserted = transaction
            .transaction_mut()
            .execute(
                sql,
                crate::db_params![
                    minted.to_storage().to_string(),
                    stream_id.as_str().to_string()
                ],
            )
            .await?;
        if inserted == 1 {
            return Ok(minted);
        }
        const SELECT_POSTGRES: &str =
            "SELECT sm_ingress_id::text FROM ingress_sm_streams WHERE stream_id = ?";
        const SELECT_SQLITE: &str =
            "SELECT sm_ingress_id FROM ingress_sm_streams WHERE stream_id = ?";
        let sql = dialect_sql(transaction, SELECT_POSTGRES, SELECT_SQLITE);
        let mut rows = transaction
            .transaction_mut()
            .query(sql, crate::db_params![stream_id.as_str().to_string()])
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
    #[cfg(feature = "clustering")]
    pub async fn lock(
        transaction: &mut IngressUowTransaction<'_>,
        fence: &SmClaimFence<'_>,
        stream_id: &SmSessionId,
    ) -> Result<Option<(SmIngressId, u64)>, IngressUowError> {
        if fence.transaction_identity != transaction.identity() || fence.stream_id != *stream_id {
            return Err(IngressUowError::ClaimFenceMissing);
        }
        Self::lock_stream(transaction, stream_id).await
    }

    /// Lock a stream protected by this database's single-node write transaction.
    pub async fn lock_single_node(
        transaction: &mut IngressUowTransaction<'_>,
        stream_id: &SmSessionId,
    ) -> Result<Option<(SmIngressId, u64)>, IngressUowError> {
        if !matches!(transaction.fencing(), super::IngressFencing::SingleNode) {
            return Err(IngressUowError::SingleNodeFencingRequired);
        }
        Self::lock_stream(transaction, stream_id).await
    }

    async fn lock_stream(
        transaction: &mut IngressUowTransaction<'_>,
        stream_id: &SmSessionId,
    ) -> Result<Option<(SmIngressId, u64)>, IngressUowError> {
        const POSTGRES: &str = "SELECT sm_ingress_id::text, handled_ordinal::text FROM ingress_sm_streams WHERE stream_id = ? FOR UPDATE";
        const SQLITE: &str = "SELECT sm_ingress_id, CAST(handled_ordinal AS TEXT) FROM ingress_sm_streams WHERE stream_id = ?";
        let sql = dialect_sql(transaction, POSTGRES, SQLITE);
        let mut rows = transaction
            .transaction_mut()
            .query(sql, crate::db_params![stream_id.as_str().to_string()])
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
            .map_err(|_| IngressUowError::InvalidStoredFrontier)?;
        Ok(Some((id, frontier)))
    }

    pub async fn lookup_unclaimed(
        transaction: &mut IngressUowTransaction<'_>,
        stream_id: &SmSessionId,
    ) -> Result<Option<SmIngressId>, IngressUowError> {
        const POSTGRES: &str =
            "SELECT sm_ingress_id::text FROM ingress_sm_streams WHERE stream_id = ? FOR UPDATE";
        const SQLITE: &str = "SELECT sm_ingress_id FROM ingress_sm_streams WHERE stream_id = ?";
        let sql = dialect_sql(transaction, POSTGRES, SQLITE);
        let mut rows = transaction
            .transaction_mut()
            .query(sql, crate::db_params![stream_id.as_str().to_string()])
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
    /// path holds the claims table's SHARE lock through its ingress deletion.
    #[cfg(feature = "clustering")]
    pub async fn fence_claim_absence_for_retirement(
        transaction: &mut IngressUowTransaction<'_>,
        stream_id: &SmSessionId,
    ) -> Result<bool, IngressUowError> {
        if transaction.bound_node_identity().is_none() {
            return Err(IngressUowError::NodeIdentityUnbound);
        }
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

    /// Advance the contiguous ordinal and its exposable wire checkpoint atomically.
    pub async fn advance_frontier(
        transaction: &mut IngressUowTransaction<'_>,
        sm_ingress_id: SmIngressId,
        offered: IngressOrdinal,
        checkpoint_h: WireHandledCount,
    ) -> Result<FrontierOutcome, IngressUowError> {
        ingress_substrate::advance_frontier(
            transaction.transaction_mut(),
            sm_ingress_id,
            offered,
            checkpoint_h,
        )
        .await
        .map_err(Into::into)
    }

    pub async fn flush_checkpoint(
        transaction: &mut IngressUowTransaction<'_>,
        sm_ingress_id: SmIngressId,
        h: WireHandledCount,
    ) -> Result<(), IngressUowError> {
        ingress_substrate::flush_checkpoint(transaction.transaction_mut(), sm_ingress_id, h)
            .await
            .map_err(Into::into)
    }

    pub async fn load_stream_checkpoint(
        transaction: &mut IngressUowTransaction<'_>,
        sm_ingress_id: SmIngressId,
    ) -> Result<Option<WireHandledCount>, IngressUowError> {
        ingress_substrate::load_stream_checkpoint(transaction.transaction_mut(), sm_ingress_id)
            .await
            .map_err(Into::into)
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

/// Repository for completed durable and post-commit ingress effects.
#[derive(Debug, Default, Clone, Copy)]
pub struct EffectReceiptRepository;

impl EffectReceiptRepository {
    pub async fn record_receipt(
        transaction: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
        kind: EffectReceiptKind,
        hash: &[u8; 32],
    ) -> Result<(), IngressUowError> {
        ingress_substrate::record_receipt(transaction.transaction_mut(), message_key, kind, hash)
            .await
            .map_err(Into::into)
    }

    pub async fn receipts_complete(
        transaction: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
    ) -> Result<bool, IngressUowError> {
        ingress_substrate::receipts_complete(transaction.transaction_mut(), message_key)
            .await
            .map_err(Into::into)
    }

    pub async fn record_receipt_pooled(
        db: &Database,
        message_key: MessageKey,
        kind: EffectReceiptKind,
        hash: &[u8; 32],
    ) -> Result<(), IngressUowError> {
        ingress_substrate::record_receipt_pooled(db, message_key, kind, hash)
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
        const POSTGRES: &str = "SELECT expires_at FROM sessions WHERE user_jid = ? AND auth_context_id = ? AND auth_context_version = ? AND principal_auth_epoch = ? FOR SHARE";
        const SQLITE: &str = "SELECT expires_at FROM sessions WHERE user_jid = ? AND auth_context_id = ? AND auth_context_version = ? AND principal_auth_epoch = ?";
        let sql = dialect_sql(transaction, POSTGRES, SQLITE);
        let mut rows = transaction
            .transaction_mut()
            .query(
                sql,
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
        const POSTGRES: &str =
            "SELECT message_key::text FROM ingress_deliveries WHERE delivery_key = ?::uuid";
        const SQLITE: &str = "SELECT message_key FROM ingress_deliveries WHERE delivery_key = ?";
        let sql = dialect_sql(transaction, POSTGRES, SQLITE);
        lookup_message_key(
            transaction,
            sql,
            crate::db_params![delivery_key.to_storage().to_string()],
        )
        .await
    }
}

fn dialect_sql(
    transaction: &mut IngressUowTransaction<'_>,
    postgres: &'static str,
    sqlite: &'static str,
) -> &'static str {
    match transaction.transaction_mut().driver() {
        DatabaseDriver::Postgres => postgres,
        DatabaseDriver::Sqlite => sqlite,
    }
}

async fn lookup_message_key(
    transaction: &mut IngressUowTransaction<'_>,
    sql: &'static str,
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
/// [`super::IngressUnitOfWork::open_with_node_identity`]) and the
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
/// [`super::IngressUnitOfWork::open_with_node_identity`]) and the
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
    /// epoch (at [`super::IngressUnitOfWork::begin`]) → exact claim
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
    /// epoch (at [`super::IngressUnitOfWork::begin`]) → exact claim
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
