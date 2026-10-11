use uuid::Uuid;
use waddle_extensions::{ExtensionPayload, RoomMessageSource, RoomObservationSubscription};
use waddle_xmpp::ingress::MessageKey;

use crate::db::Row;
use crate::ingress_uow::{EffectDescendantRepository, IngressUowTransaction};

use super::sources::{active_subscription, load_source, locked};
use super::{ObservationError, RoomPublication};

struct StoredPublication {
    id: Uuid,
    source_key: MessageKey,
    subscription: RoomObservationSubscription,
    source: RoomMessageSource,
    payload: ExtensionPayload,
    status: String,
}

fn decode_publication(
    row: &Row,
    subscription: &RoomObservationSubscription,
) -> Result<StoredPublication, ObservationError> {
    let id: String = row.get(0)?;
    let source_key: String = row.get(1)?;
    let plugin: String = row.get(2)?;
    let generation: i64 = row.get(3)?;
    let identity: String = row.get(4)?;
    let room: String = row.get(5)?;
    let revision: i64 = row.get(6)?;
    let source_json: String = row.get(7)?;
    let payload_json: String = row.get(8)?;
    let status: String = row.get(9)?;
    let source: RoomMessageSource = serde_json::from_str(&source_json)?;
    if plugin != subscription.plugin.as_str()
        || u64::try_from(generation).ok() != Some(subscription.generation.get())
        || identity != subscription.identity.as_str()
        || room != subscription.room.to_string()
        || u64::try_from(revision).ok() != Some(source.revision.get())
    {
        return Err(ObservationError::PublicationConflict);
    }
    Ok(StoredPublication {
        id: Uuid::parse_str(&id).map_err(|_| ObservationError::Codec)?,
        source_key: MessageKey::from_storage(
            Uuid::parse_str(&source_key).map_err(|_| ObservationError::Codec)?,
        ),
        subscription: subscription.clone(),
        source,
        payload: serde_json::from_str(&payload_json)?,
        status,
    })
}

fn current(stored: &StoredPublication, source: &super::sources::StoredSource) -> bool {
    stored.status == "pending"
        && !source.retracted
        && source.key == stored.source_key
        && source.source == stored.source
}

async fn load_locked(
    tx: &mut IngressUowTransaction<'_>,
    id: &Uuid,
    subscription: &RoomObservationSubscription,
) -> Result<Option<StoredPublication>, ObservationError> {
    let sql = locked(
        "SELECT id, source_key, plugin_id, generation, identity, room_jid, revision, source_json, payload_json, status FROM extension_room_publications WHERE id = ?",
        tx.transaction_mut().driver(),
    );
    let mut rows = tx
        .transaction_mut()
        .query(&sql, crate::db_params![id.to_string()])
        .await?;
    rows.next()
        .await?
        .as_ref()
        .map(|row| decode_publication(row, subscription))
        .transpose()
}

pub(super) async fn publication(
    tx: &mut IngressUowTransaction<'_>,
    subscription: &RoomObservationSubscription,
    now_ms: i64,
) -> Result<Option<RoomPublication>, ObservationError> {
    let generation = i64::try_from(subscription.generation.get())
        .map_err(|_| ObservationError::GenerationOutOfRange)?;
    let mut rows = tx.transaction_mut().query(
        "SELECT p.id, p.source_key, w.message_key FROM extension_room_publications p LEFT JOIN extension_room_observation_work w ON w.id = p.work_id WHERE p.plugin_id = ? AND p.generation = ? AND p.identity = ? AND p.room_jid = ? AND p.status = 'pending' ORDER BY p.id LIMIT 32",
        crate::db_params![subscription.plugin.as_str(), generation, subscription.identity.as_str(), subscription.room.to_string()],
    ).await?;
    let mut candidates = Vec::new();
    while let Some(row) = rows.next().await? {
        let id: String = row.get(0)?;
        let source_key: String = row.get(1)?;
        let message_key: Option<String> = row.get(2)?;
        candidates.push((id, source_key, message_key));
    }
    drop(rows);
    let mut keys = Vec::new();
    for (_, source, message) in &candidates {
        for key in std::iter::once(source).chain(message.iter()) {
            keys.push(MessageKey::from_storage(
                Uuid::parse_str(key).map_err(|_| ObservationError::Codec)?,
            ));
        }
    }
    super::work::lock_canonical_keys(tx, &keys).await?;
    for (id, _, _) in &candidates {
        EffectDescendantRepository::lock_all_nowait_raw(
            tx.transaction_mut(),
            Uuid::parse_str(id).map_err(|_| ObservationError::Codec)?,
        )
        .await
        .map_err(ObservationError::from)?;
    }
    if !active_subscription(tx, subscription).await? {
        return Ok(None);
    }
    for (id, source_key, _) in candidates {
        let id = Uuid::parse_str(&id).map_err(|_| ObservationError::Codec)?;
        let source_key = MessageKey::from_storage(
            Uuid::parse_str(&source_key).map_err(|_| ObservationError::Codec)?,
        );
        let Some(source) = load_source(tx, source_key).await? else {
            continue;
        };
        let Some(stored) = load_locked(tx, &id, subscription).await? else {
            continue;
        };
        if !current(&stored, &source) {
            tx.transaction_mut().execute(
                "UPDATE extension_room_publications SET status = 'stale', settled_at_ms = ? WHERE id = ? AND status = 'pending'",
                crate::db_params![now_ms, id.to_string()],
            ).await?;
            let now =
                chrono::DateTime::from_timestamp_millis(now_ms).ok_or(ObservationError::Codec)?;
            EffectDescendantRepository::settle_all_raw(tx.transaction_mut(), id, now)
                .await
                .map_err(ObservationError::from)?;
            continue;
        }
        return Ok(Some(RoomPublication {
            id: stored.id,
            subscription: stored.subscription,
            source: stored.source,
            payload: stored.payload,
        }));
    }
    Ok(None)
}

pub(super) async fn assert_publication(
    tx: &mut IngressUowTransaction<'_>,
    publication: &RoomPublication,
) -> Result<bool, ObservationError> {
    let mut rows = tx
        .transaction_mut()
        .query(
            "SELECT source_key FROM extension_room_publications WHERE id = ?",
            crate::db_params![publication.id.to_string()],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(false);
    };
    let source_key: String = row.get(0)?;
    drop(rows);
    let key = MessageKey::from_storage(
        Uuid::parse_str(&source_key).map_err(|_| ObservationError::Codec)?,
    );
    super::work::lock_canonical_keys(tx, &[key]).await?;
    EffectDescendantRepository::lock_all_nowait_raw(tx.transaction_mut(), publication.id)
        .await
        .map_err(ObservationError::from)?;
    if !active_subscription(tx, &publication.subscription).await? {
        return Ok(false);
    }
    let Some(source) = load_source(tx, key).await? else {
        return Ok(false);
    };
    let Some(stored) = load_locked(tx, &publication.id, &publication.subscription).await? else {
        return Ok(false);
    };
    Ok(current(&stored, &source)
        && stored.source == publication.source
        && stored.payload == publication.payload)
}

/// Must be called in the same ingress transaction that archives the result,
/// after `assert_publication` has held the config/source/publication locks.
pub(super) async fn mark_published(
    tx: &mut IngressUowTransaction<'_>,
    id: &Uuid,
    now_ms: i64,
) -> Result<bool, ObservationError> {
    EffectDescendantRepository::lock_all_nowait_raw(tx.transaction_mut(), *id)
        .await
        .map_err(ObservationError::from)?;
    let changed = tx.transaction_mut().execute(
        "UPDATE extension_room_publications SET status = 'published', settled_at_ms = ? WHERE id = ? AND status = 'pending'",
        crate::db_params![now_ms, id.to_string()],
    ).await?;
    if changed == 1 {
        let now = chrono::DateTime::from_timestamp_millis(now_ms).ok_or(ObservationError::Codec)?;
        EffectDescendantRepository::settle_all_raw(tx.transaction_mut(), *id, now)
            .await
            .map_err(ObservationError::from)?;
    }
    Ok(changed == 1)
}
