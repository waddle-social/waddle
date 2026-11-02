use super::quote_postgres_identifier;
use super::substrate::acquire_supported_epoch_lock_first;
use super::substrate::{
    acquire_supported_epoch_in_prepared_transaction,
    lock_message_for_child_in_prepared_transaction, prepared_ingress_relation,
    prepared_ingress_sql, PreparedAttestedReceiptWriter, PreparedIngressRelation,
};
use super::{
    acquire_epoch_lock_first, decode_stored_alias, discard_database_error,
    supported_protocol_epoch, Database, DatabaseDriver, IngressOrdinal, IngressSubstrateError,
    MessageKey, SemanticDigest, SmIngressId, Transaction, Uuid, WireHandledCount,
};
use crate::db::physical_attestation::AttestedPostgresWriterCapability;
use crate::db::DatabaseError;
use crate::ingress::room_manifest::{FrozenRoomManifest, StoredRoomManifest};
use crate::ingress_substrate::prepare_attested_receipt_transaction;
use xmpp_parsers::message::Message;

#[cfg(feature = "test-support")]
tokio::task_local! {
    static LOSE_POOLED_RECEIPT_COMMIT_ACK: std::cell::Cell<bool>;
}

#[cfg(feature = "test-support")]
pub(crate) async fn with_pooled_receipt_commit_ack_lost<T>(
    future: impl std::future::Future<Output = T>,
) -> T {
    LOSE_POOLED_RECEIPT_COMMIT_ACK
        .scope(std::cell::Cell::new(true), future)
        .await
}

#[cfg(feature = "test-support")]
fn take_pooled_receipt_commit_ack_loss() -> bool {
    LOSE_POOLED_RECEIPT_COMMIT_ACK
        .try_with(|lost| lost.replace(false))
        .unwrap_or(false)
}

/// Archive-only room metadata, never exposed through the live message accessor.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct MucArchiveContext {
    pub stanza_id: waddle_xmpp_core::xep0359::StanzaId,
    pub sender: Option<waddle_xmpp_core::mam::ArchivedMucSender>,
    pub nickname_generation: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvelopeVersion {
    V1,
    V2,
}

impl EnvelopeVersion {
    pub const fn to_storage(self) -> i32 {
        match self {
            Self::V1 => 1,
            Self::V2 => 2,
        }
    }
}

/// The typed message an accepted or rejected canonical row preserves for
/// post-commit reconstruction (RFC 0018 §3.3).
///
/// Serialization happens only in the substrate's storage-write adapter;
/// callers carry the typed message through planning, retries and replay.
#[derive(Debug, Clone, PartialEq)]
pub struct MessageEnvelope {
    version: EnvelopeVersion,
    message: Message,
    observer_request: Option<Message>,
    archive_contexts: Vec<MucArchiveContext>,
    room_manifest: Option<FrozenRoomManifest>,
}

impl MessageEnvelope {
    pub fn new(message: Message) -> Self {
        Self {
            version: EnvelopeVersion::V1,
            message,
            observer_request: None,
            archive_contexts: Vec::new(),
            room_manifest: None,
        }
    }

    pub fn version(&self) -> EnvelopeVersion {
        self.version
    }

    pub fn message(&self) -> &Message {
        &self.message
    }

    /// Preserve the original request shape while sharing its body and subjects
    /// with the canonical observed message. Room enrichment does not change either.
    pub fn with_room_observer(message: Message, mut request: Message) -> Self {
        request.bodies.clear();
        request.subjects.clear();
        Self {
            version: EnvelopeVersion::V1,
            message,
            observer_request: Some(request),
            archive_contexts: Vec::new(),
            room_manifest: None,
        }
    }

    pub fn room_observer_request(&self) -> Option<Message> {
        self.observer_request.as_ref().map(|request| {
            let mut request = request.clone();
            request.bodies = self.message.bodies.clone();
            request.subjects = self.message.subjects.clone();
            request
        })
    }

    pub fn room_manifest(&self) -> Option<&FrozenRoomManifest> {
        self.room_manifest.as_ref()
    }

    /// Once sealed, retries may only preserve the exact original manifest.
    pub fn with_room_manifest(
        mut self,
        manifest: FrozenRoomManifest,
    ) -> Result<Self, IngressSubstrateError> {
        match &self.room_manifest {
            Some(stored) if stored != &manifest => {
                return Err(IngressSubstrateError::MessageContentConflict)
            }
            Some(_) => {}
            None => self.room_manifest = Some(manifest),
        }
        self.version = EnvelopeVersion::V2;
        Ok(self)
    }

    fn preserve_room_manifest(&mut self, stored: &Self) -> Result<(), IngressSubstrateError> {
        if let Some(manifest) = &stored.room_manifest {
            match &self.room_manifest {
                Some(candidate) if candidate != manifest => {
                    return Err(IngressSubstrateError::MessageContentConflict)
                }
                Some(_) => {}
                None => self.room_manifest = Some(manifest.clone()),
            }
            self.version = EnvelopeVersion::V2;
        }
        Ok(())
    }

    pub(crate) fn archive_context(
        &self,
        stanza_id: &waddle_xmpp_core::xep0359::StanzaId,
    ) -> Option<&MucArchiveContext> {
        self.archive_contexts
            .iter()
            .find(|context| &context.stanza_id == stanza_id)
    }

    pub(crate) fn with_archive_context(mut self, context: MucArchiveContext) -> Self {
        self.archive_contexts.push(context);
        self
    }

    fn preserve_archive_contexts(&mut self, stored: &Self) -> Result<(), IngressSubstrateError> {
        for context in &stored.archive_contexts {
            match self.archive_context(&context.stanza_id) {
                Some(candidate) if candidate != context => {
                    return Err(IngressSubstrateError::MessageContentConflict)
                }
                Some(_) => {}
                None => self.archive_contexts.push(context.clone()),
            }
        }
        Ok(())
    }

    pub(super) fn from_storage(
        version: i64,
        bytes: Vec<u8>,
    ) -> Result<Self, IngressSubstrateError> {
        let (version, stored, room_manifest) = match version {
            1 => {
                let stored: StoredEnvelopeV1 = serde_json::from_slice(&bytes)
                    .map_err(|_| IngressSubstrateError::InvalidStoredEnvelope)?;
                (EnvelopeVersion::V1, stored, None)
            }
            2 => {
                let stored: StoredEnvelopeV2 = serde_json::from_slice(&bytes)
                    .map_err(|_| IngressSubstrateError::InvalidStoredEnvelope)?;
                let manifest = stored.room_manifest.into_domain()?;
                (
                    EnvelopeVersion::V2,
                    StoredEnvelopeV1 {
                        message: stored.message,
                        observer_request: stored.observer_request,
                        archive_contexts: stored.archive_contexts,
                    },
                    Some(manifest),
                )
            }
            _ => return Err(IngressSubstrateError::InvalidStoredEnvelope),
        };
        if stored
            .archive_contexts
            .iter()
            .enumerate()
            .any(|(index, context)| {
                stored.archive_contexts[index + 1..]
                    .iter()
                    .any(|other| other.stanza_id == context.stanza_id)
            })
        {
            return Err(IngressSubstrateError::InvalidStoredEnvelope);
        }
        let observer_request = stored
            .observer_request
            .as_deref()
            .map(parse_envelope_message)
            .transpose()?;
        if observer_request
            .as_ref()
            .is_some_and(|request| !request.bodies.is_empty() || !request.subjects.is_empty())
        {
            return Err(IngressSubstrateError::InvalidStoredEnvelope);
        }
        Ok(Self {
            version,
            message: parse_envelope_message(&stored.message)?,
            observer_request,
            archive_contexts: stored.archive_contexts,
            room_manifest,
        })
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredEnvelopeV1 {
    message: String,
    observer_request: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    archive_contexts: Vec<MucArchiveContext>,
}

/// Sealed room authority is a separate format. V1 stays readable by legacy
/// readers; V2 requires the seal rather than accepting an unsealed fallback.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredEnvelopeV2 {
    message: String,
    observer_request: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    archive_contexts: Vec<MucArchiveContext>,
    room_manifest: StoredRoomManifest,
}

fn parse_envelope_message(text: &str) -> Result<Message, IngressSubstrateError> {
    waddle_xmpp::parser::message_from_string(text)
        .map_err(|_| IngressSubstrateError::InvalidStoredEnvelope)
}

/// Encode only where the typed envelope crosses into database storage.
pub(super) fn serialize_envelope(
    envelope: &MessageEnvelope,
) -> Result<Vec<u8>, IngressSubstrateError> {
    let encode = |message: &Message| {
        waddle_xmpp::parser::message_to_string(message)
            .map_err(|_| IngressSubstrateError::InvalidStoredEnvelope)
    };
    let message = encode(envelope.message())?;
    let observer_request = envelope.observer_request.as_ref().map(encode).transpose()?;
    let archive_contexts = envelope.archive_contexts.clone();
    match (envelope.version, envelope.room_manifest.as_ref()) {
        (EnvelopeVersion::V1, None) => serde_json::to_vec(&StoredEnvelopeV1 {
            message,
            observer_request,
            archive_contexts,
        }),
        (EnvelopeVersion::V2, Some(manifest)) => serde_json::to_vec(&StoredEnvelopeV2 {
            message,
            observer_request,
            archive_contexts,
            room_manifest: StoredRoomManifest::from_domain(manifest)?,
        }),
        _ => return Err(IngressSubstrateError::InvalidStoredEnvelope),
    }
    .map_err(|_| IngressSubstrateError::InvalidStoredEnvelope)
}

/// Attach the owner's canonical room payload under the canonical row lock.
/// The caller proves first MUC acceptance from recorded authority. Existing
/// observer-only context remains immutable across retries.
pub async fn record_room_canonical_envelope(
    tx: &mut Transaction<'_>,
    key: MessageKey,
    envelope: &MessageEnvelope,
) -> Result<(), IngressSubstrateError> {
    record_room_envelope_in_prepared_transaction(tx, None, key, envelope).await
}

pub(crate) async fn record_room_envelope_in_prepared_transaction(
    tx: &mut Transaction<'_>,
    prepared: Option<&PreparedAttestedReceiptWriter>,
    key: MessageKey,
    envelope: &MessageEnvelope,
) -> Result<(), IngressSubstrateError> {
    acquire_supported_epoch_in_prepared_transaction(tx, prepared).await?;
    if !lock_message_for_child_in_prepared_transaction(tx, prepared, key).await? {
        return Err(IngressSubstrateError::MessageContentConflict);
    }
    let stored = load_envelope_in_prepared_transaction(tx, prepared, key)
        .await?
        .ok_or(IngressSubstrateError::MessageContentConflict)?;
    if stored.room_manifest().is_some() && envelope.message() != stored.message() {
        return Err(IngressSubstrateError::MessageContentConflict);
    }
    if envelope
        .room_manifest()
        .is_some_and(|manifest| manifest.message_key() != key)
    {
        return Err(IngressSubstrateError::MessageContentConflict);
    }
    let mut updated = if stored.observer_request.is_some() {
        let mut original = stored.clone();
        original.preserve_archive_contexts(envelope)?;
        original.preserve_room_manifest(envelope)?;
        original
    } else {
        envelope.clone()
    };
    updated.preserve_archive_contexts(&stored)?;
    updated.preserve_room_manifest(&stored)?;
    if updated == stored {
        return Ok(());
    }
    const POSTGRES: &str = "UPDATE ingress_messages SET envelope_version = ?, envelope = ? WHERE message_key = ?::uuid";
    const SQLITE: &str =
        "UPDATE ingress_messages SET envelope_version = ?, envelope = ? WHERE message_key = ?";
    let qualified;
    let postgres = if prepared.is_some() {
        qualified = format!(
            "UPDATE {} SET envelope_version = ?, envelope = ? WHERE message_key = ?::uuid",
            prepared_ingress_relation(prepared, PreparedIngressRelation::Messages)
        );
        qualified.as_str()
    } else {
        POSTGRES
    };
    tx.execute(
        if tx.driver() == DatabaseDriver::Postgres {
            postgres
        } else {
            SQLITE
        },
        crate::db_params![
            updated.version().to_storage(),
            serialize_envelope(&updated)?,
            key.to_storage().to_string()
        ],
    )
    .await
    .map_err(discard_database_error)?;
    Ok(())
}

#[cfg(test)]
mod room_manifest_tests {
    use super::*;
    use crate::ingress::RoomManifestSnapshot;
    use waddle_xmpp::ingress::{EffectMessageIdentity, IngressEffectIntent};
    use waddle_xmpp::muc::durable::AdminMutationId;
    use waddle_xmpp::muc::room_actor::OccupancyWatermark;

    fn envelope() -> MessageEnvelope {
        let room = "room@conference.example.com"
            .parse::<jid::BareJid>()
            .expect("room");
        let target = "alice@example.com/phone"
            .parse::<jid::FullJid>()
            .expect("target");
        let intents = vec![IngressEffectIntent::RouteDirect {
            recipient: target.to_bare(),
            fanout: vec![target.clone()],
            route_identity: EffectMessageIdentity::capture_ordinal(0),
            prepared: None,
        }];
        let snapshot = RoomManifestSnapshot {
            host_no_transport: Vec::new(),
            coordinates: None,
            occupancy_incarnation: AdminMutationId::generate(),
            occupants: vec![(target, OccupancyWatermark::from_revision(7))],
        };
        let manifest = FrozenRoomManifest::build(
            MessageKey::new(),
            room.clone(),
            waddle_xmpp_core::xep0359::StanzaId::new("room-stanza", room.into()),
            &snapshot,
            None,
            &intents,
        )
        .expect("freeze manifest");
        MessageEnvelope::new(Message::new(None))
            .with_room_manifest(manifest)
            .expect("seal")
    }

    #[test]
    fn manifest_codec_preserves_children_and_rejects_changed_keys_or_duplicate_targets() {
        let envelope = envelope();
        assert_eq!(envelope.version().to_storage(), 2);
        let bytes = serialize_envelope(&envelope).expect("serialize manifest");
        assert_eq!(
            MessageEnvelope::from_storage(2, bytes.clone()).expect("restore"),
            envelope
        );
        let stored: serde_json::Value = serde_json::from_slice(&bytes).expect("stored envelope");
        for mutate in ["key", "duplicate", "occupancy", "canonical-key"] {
            let mut changed = stored.clone();
            let manifest = &mut changed["room_manifest"];
            match mutate {
                "key" => {
                    manifest["children"][1]["delivery_key"] =
                        serde_json::json!(uuid::Uuid::new_v4())
                }
                "duplicate" => {
                    let child = manifest["children"][1].clone();
                    manifest["children"]
                        .as_array_mut()
                        .expect("children")
                        .push(child);
                }
                "occupancy" => manifest["children"][0]["occupancy"] = serde_json::json!(7),
                "canonical-key" => {
                    manifest["message_key"] = serde_json::json!(uuid::Uuid::new_v4())
                }
                _ => unreachable!(),
            }
            assert!(
                matches!(
                    MessageEnvelope::from_storage(
                        2,
                        serde_json::to_vec(&changed).expect("encode changed")
                    ),
                    Err(IngressSubstrateError::InvalidStoredEnvelope)
                ),
                "{mutate}"
            );
        }
    }

    #[test]
    fn envelope_versions_keep_unsealed_v1_and_require_a_manifest_for_v2() {
        let unsealed = MessageEnvelope::new(Message::new(None));
        let bytes = serialize_envelope(&unsealed).expect("serialize V1");
        assert_eq!(unsealed.version().to_storage(), 1);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).expect("JSON"),
            serde_json::json!({
                "message": waddle_xmpp::parser::message_to_string(unsealed.message()).expect("XML"),
                "observer_request": null
            })
        );
        assert_eq!(
            MessageEnvelope::from_storage(1, bytes.clone()).expect("V1"),
            unsealed
        );
        assert!(matches!(
            MessageEnvelope::from_storage(2, bytes),
            Err(IngressSubstrateError::InvalidStoredEnvelope)
        ));
        let sealed = envelope();
        let sealed_bytes = serialize_envelope(&sealed).expect("serialize V2");
        assert!(matches!(
            MessageEnvelope::from_storage(1, sealed_bytes.clone()),
            Err(IngressSubstrateError::InvalidStoredEnvelope)
        ));
        assert!(matches!(
            MessageEnvelope::from_storage(3, sealed_bytes.clone()),
            Err(IngressSubstrateError::InvalidStoredEnvelope)
        ));
        let mut null_manifest: serde_json::Value =
            serde_json::from_slice(&sealed_bytes).expect("V2 JSON");
        null_manifest["room_manifest"] = serde_json::Value::Null;
        assert!(matches!(
            MessageEnvelope::from_storage(2, serde_json::to_vec(&null_manifest).expect("JSON")),
            Err(IngressSubstrateError::InvalidStoredEnvelope)
        ));
        assert!(matches!(
            MessageEnvelope::from_storage(1, serde_json::to_vec(&null_manifest).expect("JSON")),
            Err(IngressSubstrateError::InvalidStoredEnvelope)
        ));
    }

    #[test]
    fn observer_enrichment_preserves_seal_and_rejects_replacement_manifest() {
        let original = envelope();
        let mut observer = MessageEnvelope::with_room_observer(
            original.message().clone(),
            original.message().clone(),
        );
        observer
            .preserve_room_manifest(&original)
            .expect("preserve seal");
        assert_eq!(observer.room_manifest(), original.room_manifest());
        assert_eq!(observer.version().to_storage(), 2);
        assert_eq!(
            MessageEnvelope::from_storage(2, serialize_envelope(&observer).expect("serialize"))
                .expect("restore observer"),
            observer
        );
        assert!(matches!(
            observer.preserve_room_manifest(&envelope()),
            Err(IngressSubstrateError::MessageContentConflict)
        ));
    }
}

/// Stable effect codec discriminator carried by receipt identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    load_envelope_in_prepared_transaction(tx, None, key).await
}

pub(crate) async fn load_envelope_in_prepared_transaction(
    tx: &mut Transaction<'_>,
    prepared: Option<&PreparedAttestedReceiptWriter>,
    key: MessageKey,
) -> Result<Option<MessageEnvelope>, IngressSubstrateError> {
    const POSTGRES: &str =
        "SELECT envelope_version::int, envelope FROM ingress_messages WHERE message_key = ?::uuid";
    const SQLITE: &str =
        "SELECT envelope_version, envelope FROM ingress_messages WHERE message_key = ?";
    let qualified;
    let postgres = if prepared.is_some() {
        qualified = format!(
            "SELECT envelope_version::int, envelope FROM {} WHERE message_key = ?::uuid",
            prepared_ingress_relation(prepared, PreparedIngressRelation::Messages)
        );
        qualified.as_str()
    } else {
        POSTGRES
    };
    let mut rows = tx
        .query(
            if tx.driver() == DatabaseDriver::Postgres {
                postgres
            } else {
                SQLITE
            },
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
        (Some(version), Some(bytes)) => {
            let envelope = MessageEnvelope::from_storage(version, bytes)?;
            if envelope
                .room_manifest()
                .is_some_and(|manifest| manifest.message_key() != key)
            {
                return Err(IngressSubstrateError::InvalidStoredEnvelope);
            }
            Ok(Some(envelope))
        }
        _ => Err(IngressSubstrateError::InvalidStoredEnvelope),
    }
}

/// Resolve a reserved wire count within the checkpoint's unambiguous SM window.
/// Pending messages may already belong to the next wrap while an earlier IQ
/// keeps the checkpoint behind; replays can likewise belong to the prior wrap.
pub(super) async fn wire_generation(
    tx: &mut Transaction<'_>,
    prepared: Option<&PreparedAttestedReceiptWriter>,
    id: SmIngressId,
    h: WireHandledCount,
) -> Result<i64, IngressSubstrateError> {
    const POSTGRES: (&str, &str) = (
        "SELECT checkpoint_h, wire_generation FROM ",
        " WHERE sm_ingress_id = ?::uuid FOR UPDATE",
    );
    const SQLITE: &str =
        "SELECT checkpoint_h, wire_generation FROM ingress_sm_streams WHERE sm_ingress_id = ?";
    let sql = prepared_ingress_sql(
        tx.driver(),
        prepared,
        PreparedIngressRelation::SmStreams,
        POSTGRES,
        SQLITE,
    );
    let mut rows = tx
        .query(&sql, crate::db_params![id.to_storage().to_string()])
        .await
        .map_err(discard_database_error)?;
    let row = rows
        .next()
        .await
        .map_err(discard_database_error)?
        .ok_or(IngressSubstrateError::StreamMissing)?;
    let checkpoint: i64 = row.get(0).map_err(discard_database_error)?;
    let generation: i64 = row.get(1).map_err(discard_database_error)?;
    let checkpoint =
        u32::try_from(checkpoint).map_err(|_| IngressSubstrateError::InvalidStoredStream)?;
    let h = h.to_storage();
    let latest = waddle_xmpp::stream_management::sequence::max_in_window(checkpoint, h);
    let generation = if h < checkpoint && latest == h {
        generation.checked_add(1)
    } else if h > checkpoint && latest == checkpoint {
        generation.checked_sub(1)
    } else {
        Some(generation)
    };
    generation
        .filter(|value| *value >= 0)
        .ok_or(IngressSubstrateError::InvalidStoredStream)
}

pub async fn lookup_wire_binding(
    tx: &mut Transaction<'_>,
    id: SmIngressId,
    h: WireHandledCount,
) -> Result<Option<(MessageKey, IngressOrdinal)>, IngressSubstrateError> {
    lookup_wire_binding_in_prepared_transaction(tx, None, id, h).await
}

pub(crate) async fn lookup_wire_binding_in_prepared_transaction(
    tx: &mut Transaction<'_>,
    prepared: Option<&PreparedAttestedReceiptWriter>,
    id: SmIngressId,
    h: WireHandledCount,
) -> Result<Option<(MessageKey, IngressOrdinal)>, IngressSubstrateError> {
    const POSTGRES: (&str, &str) = (
        "SELECT message_key::text, ingress_ordinal::text FROM ",
        " WHERE sm_ingress_id = ?::uuid AND wire_generation = ? AND wire_h = ?",
    );
    const SQLITE: &str = "SELECT message_key, ingress_ordinal FROM ingress_sm_refs WHERE sm_ingress_id = ? AND wire_generation = ? AND wire_h = ?";
    let generation = wire_generation(tx, prepared, id, h).await?;
    let sql = prepared_ingress_sql(
        tx.driver(),
        prepared,
        PreparedIngressRelation::SmRefs,
        POSTGRES,
        SQLITE,
    );
    let mut rows = tx
        .query(
            &sql,
            crate::db_params![
                id.to_storage().to_string(),
                generation,
                i64::from(h.to_storage())
            ],
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
    advance_frontier_in_prepared_transaction(tx, None, id, offered, checkpoint_h).await
}

pub(crate) async fn advance_frontier_in_prepared_transaction(
    tx: &mut Transaction<'_>,
    prepared: Option<&PreparedAttestedReceiptWriter>,
    id: SmIngressId,
    offered: IngressOrdinal,
    checkpoint_h: WireHandledCount,
) -> Result<FrontierOutcome, IngressSubstrateError> {
    const READ_POSTGRES: (&str, &str) = (
        "SELECT handled_ordinal::text FROM ",
        " WHERE sm_ingress_id = ?::uuid FOR UPDATE",
    );
    const READ_SQLITE: &str =
        "SELECT handled_ordinal FROM ingress_sm_streams WHERE sm_ingress_id = ?";
    acquire_supported_epoch_in_prepared_transaction(tx, prepared).await?;
    let read = prepared_ingress_sql(
        tx.driver(),
        prepared,
        PreparedIngressRelation::SmStreams,
        READ_POSTGRES,
        READ_SQLITE,
    );
    let mut rows = tx
        .query(&read, crate::db_params![id.to_storage().to_string()])
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
    const WRITE_POSTGRES: (&str, &str) = ("UPDATE ", " SET handled_ordinal = ?::numeric, row_revision = row_revision + 1, updated_at = now() WHERE sm_ingress_id = ?::uuid AND handled_ordinal = ?::numeric");
    const WRITE_SQLITE: &str = "UPDATE ingress_sm_streams SET handled_ordinal = ?, row_revision = row_revision + 1, updated_at = CURRENT_TIMESTAMP WHERE sm_ingress_id = ? AND handled_ordinal = ?";
    let write = prepared_ingress_sql(
        tx.driver(),
        prepared,
        PreparedIngressRelation::SmStreams,
        WRITE_POSTGRES,
        WRITE_SQLITE,
    );
    let changed = tx
        .execute(
            &write,
            crate::db_params![
                offered.to_string(),
                id.to_storage().to_string(),
                stored.to_string()
            ],
        )
        .await
        .map_err(discard_database_error)?;
    Ok(if changed == 1 {
        flush_checkpoint_in_prepared_transaction(tx, prepared, id, checkpoint_h).await?;
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
    flush_checkpoint_in_prepared_transaction(tx, None, id, h).await
}

pub(crate) async fn flush_checkpoint_in_prepared_transaction(
    tx: &mut Transaction<'_>,
    prepared: Option<&PreparedAttestedReceiptWriter>,
    id: SmIngressId,
    h: WireHandledCount,
) -> Result<(), IngressSubstrateError> {
    const READ_POSTGRES: (&str, &str) = (
        "SELECT checkpoint_h FROM ",
        " WHERE sm_ingress_id = ?::uuid FOR UPDATE",
    );
    const READ_SQLITE: &str = "SELECT checkpoint_h FROM ingress_sm_streams WHERE sm_ingress_id = ?";
    acquire_supported_epoch_in_prepared_transaction(tx, prepared).await?;
    let read = prepared_ingress_sql(
        tx.driver(),
        prepared,
        PreparedIngressRelation::SmStreams,
        READ_POSTGRES,
        READ_SQLITE,
    );
    let mut rows = tx
        .query(&read, crate::db_params![id.to_storage().to_string()])
        .await
        .map_err(discard_database_error)?;
    let row = rows
        .next()
        .await
        .map_err(discard_database_error)?
        .ok_or(IngressSubstrateError::StreamMissing)?;
    let stored: i64 = row.get(0).map_err(discard_database_error)?;
    let stored = u32::try_from(stored).map_err(|_| IngressSubstrateError::InvalidStoredStream)?;
    drop(rows);
    // A delayed ACK flush from a displaced connection must never rewind the
    // checkpoint committed by its replacement, including across counter wrap.
    let offered = waddle_xmpp::stream_management::sequence::max_in_window(stored, h.to_storage());
    if offered == stored {
        return Ok(());
    }
    const POSTGRES: (&str, &str) = ("UPDATE ", " SET checkpoint_h = ?, wire_generation = wire_generation + ? WHERE sm_ingress_id = ?::uuid");
    const SQLITE: &str = "UPDATE ingress_sm_streams SET checkpoint_h = ?, wire_generation = wire_generation + ? WHERE sm_ingress_id = ?";
    let write = prepared_ingress_sql(
        tx.driver(),
        prepared,
        PreparedIngressRelation::SmStreams,
        POSTGRES,
        SQLITE,
    );
    tx.execute(
        &write,
        crate::db_params![
            i64::from(offered),
            i64::from(offered < stored),
            id.to_storage().to_string()
        ],
    )
    .await
    .map_err(discard_database_error)?;
    Ok(())
}

pub async fn load_stream_checkpoint(
    tx: &mut Transaction<'_>,
    id: SmIngressId,
) -> Result<Option<WireHandledCount>, IngressSubstrateError> {
    load_stream_checkpoint_in_prepared_transaction(tx, None, id).await
}

pub(crate) async fn load_stream_checkpoint_in_prepared_transaction(
    tx: &mut Transaction<'_>,
    prepared: Option<&PreparedAttestedReceiptWriter>,
    id: SmIngressId,
) -> Result<Option<WireHandledCount>, IngressSubstrateError> {
    const POSTGRES: (&str, &str) = (
        "SELECT checkpoint_h FROM ",
        " WHERE sm_ingress_id = ?::uuid",
    );
    const SQLITE: &str = "SELECT checkpoint_h FROM ingress_sm_streams WHERE sm_ingress_id = ?";
    let sql = prepared_ingress_sql(
        tx.driver(),
        prepared,
        PreparedIngressRelation::SmStreams,
        POSTGRES,
        SQLITE,
    );
    let mut rows = tx
        .query(&sql, crate::db_params![id.to_storage().to_string()])
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
    acquire_supported_epoch_lock_first(tx).await?;
    record_receipt_in_prepared_transaction(tx, key, kind, hash, None).await
}

/// Insert a receipt after the caller has already acquired the epoch lock.
/// UoW receipts use this so a receipt written after application row locks
/// never issues another epoch prelock. A schema is supplied only from the
/// opaque attested preparation carried by the same UoW transaction.
pub(crate) async fn record_receipt_in_prepared_transaction(
    tx: &mut Transaction<'_>,
    key: MessageKey,
    kind: EffectReceiptKind,
    hash: &[u8; 32],
    attested_schema: Option<&str>,
) -> Result<(), IngressSubstrateError> {
    const POSTGRES: &str = "INSERT INTO ingress_effect_receipts (message_key, kind, semantic_identity_hash) VALUES (?::uuid, ?, ?) ON CONFLICT (message_key, kind, semantic_identity_hash) DO NOTHING";
    const SQLITE: &str = "INSERT INTO ingress_effect_receipts (message_key, kind, semantic_identity_hash) VALUES (?, ?, ?) ON CONFLICT (message_key, kind, semantic_identity_hash) DO NOTHING";
    let qualified_postgres;
    let sql = if tx.driver() == DatabaseDriver::Postgres {
        match attested_schema {
            Some(schema) => {
                qualified_postgres = format!(
                    "INSERT INTO {}.ingress_effect_receipts \
                     (message_key, kind, semantic_identity_hash) VALUES (?::uuid, ?, ?) \
                     ON CONFLICT (message_key, kind, semantic_identity_hash) DO NOTHING",
                    quote_postgres_identifier(schema)
                );
                qualified_postgres.as_str()
            }
            None => POSTGRES,
        }
    } else {
        SQLITE
    };
    tx.execute(
        sql,
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

/// Number of completion receipts recorded for one canonical row, read on a
/// plain pooled connection so maintenance accounting never waits on a lock.
pub async fn receipts_complete(
    tx: &mut Transaction<'_>,
    key: MessageKey,
) -> Result<bool, IngressSubstrateError> {
    receipts_complete_in_schema(tx, key, None).await
}

pub(crate) async fn receipts_complete_in_schema(
    tx: &mut Transaction<'_>,
    key: MessageKey,
    attested_schema: Option<&str>,
) -> Result<bool, IngressSubstrateError> {
    const POSTGRES: &str = "SELECT NOT EXISTS (SELECT 1 FROM ingress_effect_intents i WHERE i.message_key = ?::uuid AND NOT EXISTS (SELECT 1 FROM ingress_effect_receipts r WHERE r.message_key = i.message_key AND r.kind = i.kind AND r.semantic_identity_hash = i.semantic_identity_hash))";
    const SQLITE: &str = "SELECT NOT EXISTS (SELECT 1 FROM ingress_effect_intents i WHERE i.message_key = ? AND NOT EXISTS (SELECT 1 FROM ingress_effect_receipts r WHERE r.message_key = i.message_key AND r.kind = i.kind AND r.semantic_identity_hash = i.semantic_identity_hash))";
    let qualified_postgres;
    let sql = if tx.driver() == DatabaseDriver::Postgres {
        match attested_schema {
            Some(schema) => {
                let schema = quote_postgres_identifier(schema);
                qualified_postgres = format!(
                    "SELECT NOT EXISTS (SELECT 1 FROM {schema}.ingress_effect_intents i \
                     WHERE i.message_key = ?::uuid AND NOT EXISTS (SELECT 1 \
                     FROM {schema}.ingress_effect_receipts r WHERE r.message_key = i.message_key \
                     AND r.kind = i.kind AND r.semantic_identity_hash = i.semantic_identity_hash))"
                );
                qualified_postgres.as_str()
            }
            None => POSTGRES,
        }
    } else {
        SQLITE
    };
    let mut rows = tx
        .query(sql, crate::db_params![key.to_storage().to_string()])
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
    if let Some(capability) = db.attested_pg_writer_capability() {
        return record_receipt_pooled_attested(db, &capability, key, kind, hash).await;
    }

    let mut tx = db.begin_immediate().await.map_err(discard_database_error)?;
    if tx.driver() == DatabaseDriver::Postgres {
        tx.execute("SET TRANSACTION ISOLATION LEVEL READ COMMITTED", ())
            .await
            .map_err(discard_database_error)?;
    }
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

async fn record_receipt_pooled_attested(
    db: &Database,
    capability: &AttestedPostgresWriterCapability,
    key: MessageKey,
    kind: EffectReceiptKind,
    hash: &[u8; 32],
) -> Result<(), IngressSubstrateError> {
    db.verify_attested_pg_writer_capability(capability)
        .map_err(DatabaseError::from)
        .map_err(discard_database_error)?;
    let mut tx = db.begin_immediate().await.map_err(discard_database_error)?;
    tx.execute("SET TRANSACTION ISOLATION LEVEL READ COMMITTED", ())
        .await
        .map_err(discard_database_error)?;
    if !super::tighten_local_transaction_timeouts(&mut tx)
        .await
        .map_err(discard_database_error)?
    {
        return Err(IngressSubstrateError::Database {
            retry_class: crate::ingress_uow::DbRetryClass::NotRetryable,
        });
    }
    let prepared = prepare_attested_receipt_transaction(&mut tx, capability).await?;
    if prepared.protocol_epoch() > supported_protocol_epoch() {
        return Err(IngressSubstrateError::UnsupportedLiveEpoch);
    }
    record_receipt_in_prepared_transaction(&mut tx, key, kind, hash, Some(prepared.schema()))
        .await?;
    let commit = tx.commit().await;
    #[cfg(feature = "test-support")]
    let acknowledgement_lost = take_pooled_receipt_commit_ack_loss();
    #[cfg(not(feature = "test-support"))]
    let acknowledgement_lost = false;
    if commit.is_ok() && !acknowledgement_lost {
        return Ok(());
    }
    if exact_receipt_exists_after_commit_error(db, capability, key, kind, hash)
        .await
        .unwrap_or(false)
    {
        Ok(())
    } else {
        Err(IngressSubstrateError::ReceiptCommitIndeterminate)
    }
}

async fn exact_receipt_exists_after_commit_error(
    db: &Database,
    capability: &AttestedPostgresWriterCapability,
    key: MessageKey,
    kind: EffectReceiptKind,
    hash: &[u8; 32],
) -> Result<bool, IngressSubstrateError> {
    let mut tx = db.begin().await.map_err(discard_database_error)?;
    db.verify_attested_pg_writer_capability(capability)
        .map_err(DatabaseError::from)
        .map_err(discard_database_error)?;
    tx.execute("SET TRANSACTION ISOLATION LEVEL READ COMMITTED", ())
        .await
        .map_err(discard_database_error)?;
    if !super::tighten_local_transaction_timeouts(&mut tx)
        .await
        .map_err(discard_database_error)?
    {
        return Err(IngressSubstrateError::Database {
            retry_class: crate::ingress_uow::DbRetryClass::NotRetryable,
        });
    }
    let prepared = prepare_attested_receipt_transaction(&mut tx, capability).await?;
    if prepared.protocol_epoch() > supported_protocol_epoch() {
        return Err(IngressSubstrateError::UnsupportedLiveEpoch);
    }
    exact_receipt_exists(&mut tx, key, kind, hash, prepared.schema()).await
}

async fn exact_receipt_exists(
    tx: &mut Transaction<'_>,
    key: MessageKey,
    kind: EffectReceiptKind,
    hash: &[u8; 32],
    attested_schema: &str,
) -> Result<bool, IngressSubstrateError> {
    let sql = format!(
        "SELECT EXISTS (SELECT 1 FROM {}.ingress_effect_receipts \
         WHERE message_key = ?::uuid AND kind = ? AND semantic_identity_hash = ?)",
        quote_postgres_identifier(attested_schema)
    );
    let mut rows = tx
        .query(
            &sql,
            crate::db_params![
                key.to_storage().to_string(),
                kind.to_storage(),
                hash.to_vec()
            ],
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

/// The canonical lock makes envelope completion and concurrent retries atomic.
pub(super) async fn complete_message_envelope(
    tx: &mut Transaction<'_>,
    prepared: Option<&PreparedAttestedReceiptWriter>,
    key: MessageKey,
    digest: &SemanticDigest,
    envelope: Option<&MessageEnvelope>,
) -> Result<(), IngressSubstrateError> {
    const POSTGRES: &str = "SELECT message_key::text, digest_version, digest FROM ingress_messages WHERE message_key = ?::uuid FOR UPDATE";
    const SQLITE: &str =
        "SELECT message_key, digest_version, digest FROM ingress_messages WHERE message_key = ?";
    let qualified;
    let postgres = if prepared.is_some() {
        qualified = format!(
            "SELECT message_key::text, digest_version, digest FROM {} WHERE message_key = ?::uuid FOR UPDATE",
            prepared_ingress_relation(prepared, PreparedIngressRelation::Messages)
        );
        qualified.as_str()
    } else {
        POSTGRES
    };
    let mut rows = tx
        .query(
            if tx.driver() == DatabaseDriver::Postgres {
                postgres
            } else {
                SQLITE
            },
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
    if let Some(stored) = load_envelope_in_prepared_transaction(tx, prepared, key).await? {
        return if stored.version() == envelope.version()
            && serialize_envelope(&stored)? == serialize_envelope(envelope)?
        {
            Ok(())
        } else {
            Err(IngressSubstrateError::MessageContentConflict)
        };
    }
    const WRITE_POSTGRES: &str = "UPDATE ingress_messages SET envelope_version = ?, envelope = ? WHERE message_key = ?::uuid AND envelope IS NULL";
    const WRITE_SQLITE: &str = "UPDATE ingress_messages SET envelope_version = ?, envelope = ? WHERE message_key = ? AND envelope IS NULL";
    let qualified;
    let postgres = if prepared.is_some() {
        qualified = format!(
            "UPDATE {} SET envelope_version = ?, envelope = ? WHERE message_key = ?::uuid AND envelope IS NULL",
            prepared_ingress_relation(prepared, PreparedIngressRelation::Messages)
        );
        qualified.as_str()
    } else {
        WRITE_POSTGRES
    };
    tx.execute(
        if tx.driver() == DatabaseDriver::Postgres {
            postgres
        } else {
            WRITE_SQLITE
        },
        crate::db_params![
            envelope.version().to_storage(),
            serialize_envelope(envelope)?,
            key.to_storage().to_string()
        ],
    )
    .await
    .map_err(discard_database_error)?;
    Ok(())
}
