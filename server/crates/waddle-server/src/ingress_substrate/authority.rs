use super::*;
use std::str::FromStr;
use xmpp_parsers::message::Message;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvelopeVersion {
    V1,
}

impl EnvelopeVersion {
    pub const fn to_storage(self) -> i32 {
        match self {
            Self::V1 => 1,
        }
    }
}

/// The typed message an accepted or rejected canonical row preserves for
/// post-commit reconstruction (RFC 0018 §3.3).
///
/// The wire form is private to this storage adapter: it is produced once at
/// construction (the canonical serialization the equality check relies on)
/// and parsed back into a typed [`Message`] when loaded, so no caller ever
/// sees or supplies raw bytes.
#[derive(Debug, Clone)]
pub struct MessageEnvelope {
    version: EnvelopeVersion,
    message: Message,
    canonical: Vec<u8>,
}

impl MessageEnvelope {
    pub fn new(message: Message) -> Result<Self, IngressSubstrateError> {
        let canonical = waddle_xmpp::parser::message_to_string(&message)
            .map_err(|_| IngressSubstrateError::InvalidStoredEnvelope)?
            .into_bytes();
        Ok(Self {
            version: EnvelopeVersion::V1,
            message,
            canonical,
        })
    }

    pub fn version(&self) -> EnvelopeVersion {
        self.version
    }

    pub fn message(&self) -> &Message {
        &self.message
    }

    pub(super) fn storage_bytes(&self) -> &[u8] {
        &self.canonical
    }

    pub(super) fn from_storage(
        version: i64,
        bytes: Vec<u8>,
    ) -> Result<Self, IngressSubstrateError> {
        if version != i64::from(EnvelopeVersion::V1.to_storage()) {
            return Err(IngressSubstrateError::InvalidStoredEnvelope);
        }
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| IngressSubstrateError::InvalidStoredEnvelope)?;
        let element = minidom::Element::from_str(text)
            .map_err(|_| IngressSubstrateError::InvalidStoredEnvelope)?;
        let stanza_ns = element.ns().to_string();
        let thread_parent = waddle_xmpp_core::parser_utils::extract_thread_parent(&element);
        let mut message =
            Message::try_from(element).map_err(|_| IngressSubstrateError::InvalidStoredEnvelope)?;
        if let Some(parent) = thread_parent {
            waddle_xmpp_core::parser_utils::reattach_thread_parent(
                &mut message,
                parent,
                &stanza_ns,
            );
        }
        Ok(Self {
            version: EnvelopeVersion::V1,
            message,
            canonical: bytes,
        })
    }
}

impl PartialEq for MessageEnvelope {
    fn eq(&self, other: &Self) -> bool {
        self.version == other.version && self.canonical == other.canonical
    }
}

impl Eq for MessageEnvelope {}

/// Stable effect codec discriminator carried by receipt identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectReceiptKind(i32);

impl EffectReceiptKind {
    pub const fn from_storage(value: i32) -> Self {
        Self(value)
    }
    pub const fn to_storage(self) -> i32 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrontierOutcome {
    Advanced,
    Idempotent,
    Stale { stored: u64 },
}

pub async fn load_envelope(
    tx: &mut Transaction<'_>,
    key: MessageKey,
) -> Result<Option<MessageEnvelope>, IngressSubstrateError> {
    const POSTGRES: &str =
        "SELECT envelope_version::int, envelope FROM ingress_messages WHERE message_key = ?::uuid";
    const SQLITE: &str =
        "SELECT envelope_version, envelope FROM ingress_messages WHERE message_key = ?";
    let mut rows = tx
        .query(
            dialect_sql(tx.driver(), POSTGRES, SQLITE),
            crate::db_params![key.to_storage().to_string()],
        )
        .await
        .map_err(discard_database_error)?;
    let Some(row) = rows.next().await.map_err(discard_database_error)? else {
        return Ok(None);
    };
    let version: Option<i64> = row.get(0).map_err(discard_database_error)?;
    let bytes: Option<Vec<u8>> = row.get(1).map_err(discard_database_error)?;
    match (version, bytes) {
        (None, None) => Ok(None),
        (Some(version), Some(bytes)) => MessageEnvelope::from_storage(version, bytes).map(Some),
        _ => Err(IngressSubstrateError::InvalidStoredEnvelope),
    }
}

pub async fn lookup_wire_binding(
    tx: &mut Transaction<'_>,
    id: SmIngressId,
    h: WireHandledCount,
) -> Result<Option<(MessageKey, IngressOrdinal)>, IngressSubstrateError> {
    const POSTGRES: &str = "SELECT message_key::text, ingress_ordinal::text FROM ingress_sm_refs WHERE sm_ingress_id = ?::uuid AND wire_h = ?";
    const SQLITE: &str = "SELECT message_key, ingress_ordinal FROM ingress_sm_refs WHERE sm_ingress_id = ? AND wire_h = ?";
    let mut rows = tx
        .query(
            dialect_sql(tx.driver(), POSTGRES, SQLITE),
            crate::db_params![id.to_storage().to_string(), i64::from(h.to_storage())],
        )
        .await
        .map_err(discard_database_error)?;
    let Some(row) = rows.next().await.map_err(discard_database_error)? else {
        return Ok(None);
    };
    let key: String = row.get(0).map_err(discard_database_error)?;
    let ordinal: String = row.get(1).map_err(discard_database_error)?;
    let key = key
        .parse::<Uuid>()
        .map(MessageKey::from_storage)
        .map_err(|_| IngressSubstrateError::InvalidStoredMessageKey)?;
    let ordinal = ordinal
        .parse::<u64>()
        .ok()
        .and_then(|value| IngressOrdinal::from_storage(value).ok())
        .ok_or(IngressSubstrateError::InvalidStoredStream)?;
    Ok(Some((key, ordinal)))
}

pub async fn advance_frontier(
    tx: &mut Transaction<'_>,
    id: SmIngressId,
    offered: IngressOrdinal,
    checkpoint_h: WireHandledCount,
) -> Result<FrontierOutcome, IngressSubstrateError> {
    const READ_POSTGRES: &str = "SELECT handled_ordinal::text FROM ingress_sm_streams WHERE sm_ingress_id = ?::uuid FOR UPDATE";
    const READ_SQLITE: &str =
        "SELECT handled_ordinal FROM ingress_sm_streams WHERE sm_ingress_id = ?";
    acquire_epoch_lock_first(tx).await?;
    let mut rows = tx
        .query(
            dialect_sql(tx.driver(), READ_POSTGRES, READ_SQLITE),
            crate::db_params![id.to_storage().to_string()],
        )
        .await
        .map_err(discard_database_error)?;
    let stored: String = rows
        .next()
        .await
        .map_err(discard_database_error)?
        .ok_or(IngressSubstrateError::StreamMissing)?
        .get(0)
        .map_err(discard_database_error)?;
    drop(rows);
    let stored = stored
        .parse::<u64>()
        .map_err(|_| IngressSubstrateError::InvalidStoredStream)?;
    let offered = offered.to_storage();
    if stored >= offered {
        return Ok(FrontierOutcome::Idempotent);
    }
    if stored != offered - 1 {
        return Ok(FrontierOutcome::Stale { stored });
    }
    const WRITE_POSTGRES: &str = "UPDATE ingress_sm_streams SET handled_ordinal = ?::numeric, checkpoint_h = ?, row_revision = row_revision + 1, updated_at = now() WHERE sm_ingress_id = ?::uuid AND handled_ordinal = ?::numeric";
    const WRITE_SQLITE: &str = "UPDATE ingress_sm_streams SET handled_ordinal = ?, checkpoint_h = ?, row_revision = row_revision + 1, updated_at = CURRENT_TIMESTAMP WHERE sm_ingress_id = ? AND handled_ordinal = ?";
    let changed = tx
        .execute(
            dialect_sql(tx.driver(), WRITE_POSTGRES, WRITE_SQLITE),
            crate::db_params![
                offered.to_string(),
                i64::from(checkpoint_h.to_storage()),
                id.to_storage().to_string(),
                stored.to_string()
            ],
        )
        .await
        .map_err(discard_database_error)?;
    Ok(if changed == 1 {
        FrontierOutcome::Advanced
    } else {
        FrontierOutcome::Stale { stored }
    })
}

pub async fn flush_checkpoint(
    tx: &mut Transaction<'_>,
    id: SmIngressId,
    h: WireHandledCount,
) -> Result<(), IngressSubstrateError> {
    const POSTGRES: &str =
        "UPDATE ingress_sm_streams SET checkpoint_h = ? WHERE sm_ingress_id = ?::uuid";
    const SQLITE: &str = "UPDATE ingress_sm_streams SET checkpoint_h = ? WHERE sm_ingress_id = ?";
    tx.execute(
        dialect_sql(tx.driver(), POSTGRES, SQLITE),
        crate::db_params![i64::from(h.to_storage()), id.to_storage().to_string()],
    )
    .await
    .map_err(discard_database_error)?;
    Ok(())
}

pub async fn load_stream_checkpoint(
    tx: &mut Transaction<'_>,
    id: SmIngressId,
) -> Result<Option<WireHandledCount>, IngressSubstrateError> {
    const POSTGRES: &str =
        "SELECT checkpoint_h FROM ingress_sm_streams WHERE sm_ingress_id = ?::uuid";
    const SQLITE: &str = "SELECT checkpoint_h FROM ingress_sm_streams WHERE sm_ingress_id = ?";
    let mut rows = tx
        .query(
            dialect_sql(tx.driver(), POSTGRES, SQLITE),
            crate::db_params![id.to_storage().to_string()],
        )
        .await
        .map_err(discard_database_error)?;
    let Some(row) = rows.next().await.map_err(discard_database_error)? else {
        return Ok(None);
    };
    let value: i64 = row.get(0).map_err(discard_database_error)?;
    u32::try_from(value)
        .map(WireHandledCount::from_storage)
        .map(Some)
        .map_err(|_| IngressSubstrateError::InvalidStoredStream)
}

pub async fn record_receipt(
    tx: &mut Transaction<'_>,
    key: MessageKey,
    kind: EffectReceiptKind,
    hash: &[u8; 32],
) -> Result<(), IngressSubstrateError> {
    const POSTGRES: &str = "INSERT INTO ingress_effect_receipts (message_key, kind, semantic_identity_hash) VALUES (?::uuid, ?, ?) ON CONFLICT (message_key, kind, semantic_identity_hash) DO NOTHING";
    const SQLITE: &str = "INSERT INTO ingress_effect_receipts (message_key, kind, semantic_identity_hash) VALUES (?, ?, ?) ON CONFLICT (message_key, kind, semantic_identity_hash) DO NOTHING";
    tx.execute(
        dialect_sql(tx.driver(), POSTGRES, SQLITE),
        crate::db_params![
            key.to_storage().to_string(),
            kind.to_storage(),
            hash.to_vec()
        ],
    )
    .await
    .map_err(discard_database_error)?;
    Ok(())
}

pub async fn receipts_complete(
    tx: &mut Transaction<'_>,
    key: MessageKey,
) -> Result<bool, IngressSubstrateError> {
    const POSTGRES: &str = "SELECT NOT EXISTS (SELECT 1 FROM ingress_effect_intents i WHERE i.message_key = ?::uuid AND NOT EXISTS (SELECT 1 FROM ingress_effect_receipts r WHERE r.message_key = i.message_key AND r.kind = i.kind AND r.semantic_identity_hash = i.semantic_identity_hash))";
    const SQLITE: &str = "SELECT NOT EXISTS (SELECT 1 FROM ingress_effect_intents i WHERE i.message_key = ? AND NOT EXISTS (SELECT 1 FROM ingress_effect_receipts r WHERE r.message_key = i.message_key AND r.kind = i.kind AND r.semantic_identity_hash = i.semantic_identity_hash))";
    let mut rows = tx
        .query(
            dialect_sql(tx.driver(), POSTGRES, SQLITE),
            crate::db_params![key.to_storage().to_string()],
        )
        .await
        .map_err(discard_database_error)?;
    rows.next()
        .await
        .map_err(discard_database_error)?
        .ok_or(IngressSubstrateError::InvalidStoredStream)?
        .get(0)
        .map_err(discard_database_error)
}

pub async fn record_receipt_pooled(
    db: &Database,
    key: MessageKey,
    kind: EffectReceiptKind,
    hash: &[u8; 32],
) -> Result<(), IngressSubstrateError> {
    let mut tx = db.begin_immediate().await.map_err(discard_database_error)?;
    let epoch = acquire_epoch_lock_first(&mut tx).await?;
    if epoch > supported_protocol_epoch() {
        return Err(IngressSubstrateError::UnsupportedLiveEpoch);
    }
    if tx.driver() == DatabaseDriver::Postgres {
        tx.execute("SELECT set_config('waddle.protocol_epoch', ?, true), set_config('waddle.protocol_epoch_xid', pg_current_xact_id()::text, true)", crate::db_params![epoch.to_storage().to_string()]).await.map_err(discard_database_error)?;
    }
    record_receipt(&mut tx, key, kind, hash).await?;
    tx.commit().await.map_err(discard_database_error)
}

/// The canonical lock makes envelope completion and concurrent retries atomic.
pub(super) async fn complete_message_envelope(
    tx: &mut Transaction<'_>,
    key: MessageKey,
    digest: &SemanticDigest,
    envelope: Option<&MessageEnvelope>,
) -> Result<(), IngressSubstrateError> {
    const POSTGRES: &str = "SELECT message_key::text, digest_version, digest FROM ingress_messages WHERE message_key = ?::uuid FOR UPDATE";
    const SQLITE: &str =
        "SELECT message_key, digest_version, digest FROM ingress_messages WHERE message_key = ?";
    let mut rows = tx
        .query(
            dialect_sql(tx.driver(), POSTGRES, SQLITE),
            crate::db_params![key.to_storage().to_string()],
        )
        .await
        .map_err(discard_database_error)?;
    let row = rows
        .next()
        .await
        .map_err(discard_database_error)?
        .ok_or(IngressSubstrateError::MessageContentConflict)?;
    drop(rows);
    if &decode_stored_alias(&row)?.digest != digest {
        return Err(IngressSubstrateError::MessageContentConflict);
    }
    let Some(envelope) = envelope else {
        return Ok(());
    };
    if let Some(stored) = load_envelope(tx, key).await? {
        return if &stored == envelope {
            Ok(())
        } else {
            Err(IngressSubstrateError::MessageContentConflict)
        };
    }
    const WRITE_POSTGRES: &str = "UPDATE ingress_messages SET envelope_version = ?, envelope = ? WHERE message_key = ?::uuid AND envelope IS NULL";
    const WRITE_SQLITE: &str = "UPDATE ingress_messages SET envelope_version = ?, envelope = ? WHERE message_key = ? AND envelope IS NULL";
    tx.execute(
        dialect_sql(tx.driver(), WRITE_POSTGRES, WRITE_SQLITE),
        crate::db_params![
            envelope.version().to_storage(),
            envelope.storage_bytes().to_vec(),
            key.to_storage().to_string()
        ],
    )
    .await
    .map_err(discard_database_error)?;
    Ok(())
}
