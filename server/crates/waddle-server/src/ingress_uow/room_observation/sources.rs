use chrono::{DateTime, Utc};
use jid::BareJid;
use sha2::{Digest, Sha256};
use uuid::Uuid;
use waddle_extensions::{
    ConfiguredRoomObserver, DisplayText, MessageRevision, ObservationGeneration, OriginId,
    PluginId, RoomMessageSource, RoomObservationScope, RoomObservationSubscription, Sha256Digest,
    StanzaId, Timestamp,
};
use waddle_xmpp::ingress::{IngressEffectIntent, IngressEffectKey, IngressEffectKind, MessageKey};
use waddle_xmpp::xep::xep0308;
use waddle_xmpp_core::xep0359;
use xmpp_parsers::message::Message;

use crate::db::{DatabaseDriver, Row};
use crate::ingress_substrate::EffectReceiptKind;
use crate::ingress_uow::{EffectReceiptRepository, IngressUowTransaction};

use super::{CapturedRoomSource, ObservationError};

pub(super) struct StoredSource {
    pub key: MessageKey,
    pub source: RoomMessageSource,
    pub retracted: bool,
}

pub(super) fn locked(sql: &'static str, driver: DatabaseDriver) -> String {
    if driver == DatabaseDriver::Postgres {
        format!("{sql} FOR UPDATE")
    } else {
        sql.to_string()
    }
}

fn canonical_scope(scope: &RoomObservationScope) -> RoomObservationScope {
    match scope {
        RoomObservationScope::AllHostedRooms => RoomObservationScope::AllHostedRooms,
        RoomObservationScope::Rooms(rooms) => {
            let mut rooms = rooms.clone();
            rooms.sort_by_key(ToString::to_string);
            rooms.dedup();
            RoomObservationScope::Rooms(rooms)
        }
    }
}

pub(super) async fn sync_configured(
    tx: &mut IngressUowTransaction<'_>,
    configured: &[ConfiguredRoomObserver],
) -> Result<(), ObservationError> {
    let mut seen = std::collections::HashSet::new();
    for observer in configured {
        if !seen.insert(observer.plugin.clone()) {
            return Err(ObservationError::ConfigurationConflict);
        }
        let generation = i64::try_from(observer.generation.get())
            .map_err(|_| ObservationError::GenerationOutOfRange)?;
        let scope_json = serde_json::to_string(&canonical_scope(&observer.scope))?;
        let max_concurrent = i64::from(observer.max_concurrent);
        tx.transaction_mut()
            .execute(
                "INSERT INTO extension_room_observers (plugin_id, generation, identity, scope_json, max_concurrent) VALUES (?, ?, ?, ?, ?) ON CONFLICT (plugin_id) DO NOTHING",
                crate::db_params![observer.plugin.as_str(), generation, observer.identity.as_str(), &scope_json, max_concurrent],
            )
            .await?;
        let sql = locked(
            "SELECT generation, identity, scope_json, max_concurrent FROM extension_room_observers WHERE plugin_id = ?",
            tx.transaction_mut().driver(),
        );
        let mut rows = tx
            .transaction_mut()
            .query(&sql, crate::db_params![observer.plugin.as_str()])
            .await?;
        let row = rows.next().await?.ok_or(ObservationError::Database)?;
        drop(rows);
        let stored_generation: i64 = row.get(0)?;
        let stored_identity: String = row.get(1)?;
        let stored_scope: String = row.get(2)?;
        let stored_concurrency: i64 = row.get(3)?;
        if stored_generation > generation {
            continue;
        }
        if stored_generation == generation {
            if stored_identity != observer.identity.as_str() {
                return Err(ObservationError::IdentityConflict);
            }
            if stored_scope != scope_json || stored_concurrency != max_concurrent {
                return Err(ObservationError::ConfigurationConflict);
            }
            continue;
        }
        tx.transaction_mut()
            .execute(
                "UPDATE extension_room_observers SET generation = ?, identity = ?, scope_json = ?, max_concurrent = ? WHERE plugin_id = ? AND generation < ?",
                crate::db_params![generation, observer.identity.as_str(), &scope_json, max_concurrent, observer.plugin.as_str(), generation],
            )
            .await?;
        stale_generation(tx, &observer.plugin, generation).await?;
    }
    Ok(())
}

pub(super) async fn active_subscription(
    tx: &mut IngressUowTransaction<'_>,
    subscription: &RoomObservationSubscription,
) -> Result<bool, ObservationError> {
    let sql = if tx.transaction_mut().driver() == DatabaseDriver::Postgres {
        "SELECT generation, identity, scope_json FROM extension_room_observers WHERE plugin_id = ? FOR SHARE"
    } else {
        "SELECT generation, identity, scope_json FROM extension_room_observers WHERE plugin_id = ?"
    };
    let mut rows = tx
        .transaction_mut()
        .query(sql, crate::db_params![subscription.plugin.as_str()])
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(false);
    };
    let generation: i64 = row.get(0)?;
    let identity: String = row.get(1)?;
    let scope_json: String = row.get(2)?;
    let scope: RoomObservationScope = serde_json::from_str(&scope_json)?;
    Ok(
        u64::try_from(generation).ok() == Some(subscription.generation.get())
            && identity == subscription.identity.as_str()
            && scope.includes(&subscription.room),
    )
}

fn decode_source(row: &Row) -> Result<StoredSource, ObservationError> {
    let key: String = row.get(0)?;
    let source_json: String = row.get(1)?;
    let retracted: i64 = row.get(2)?;
    Ok(StoredSource {
        key: MessageKey::from_storage(Uuid::parse_str(&key).map_err(|_| ObservationError::Codec)?),
        source: serde_json::from_str(&source_json)?,
        retracted: retracted != 0,
    })
}

pub(super) async fn load_source(
    tx: &mut IngressUowTransaction<'_>,
    key: MessageKey,
) -> Result<Option<StoredSource>, ObservationError> {
    let sql = locked(
        "SELECT source_key, source_json, retracted FROM extension_room_sources WHERE source_key = ?",
        tx.transaction_mut().driver(),
    );
    let mut rows = tx
        .transaction_mut()
        .query(&sql, crate::db_params![key.to_storage().to_string()])
        .await?;
    rows.next().await?.as_ref().map(decode_source).transpose()
}

async fn source_for_authoritative_target(
    tx: &mut IngressUowTransaction<'_>,
    room: &BareJid,
    sender: &BareJid,
    target: &xep0359::StanzaId,
) -> Result<Option<StoredSource>, ObservationError> {
    if target.by != *room {
        return Ok(None);
    }
    let mut rows = tx.transaction_mut().query(
        "SELECT source_key FROM extension_room_source_revisions WHERE room_jid = ? AND room_stanza_id = ?",
        crate::db_params![room.to_string(), target.id.as_str()],
    ).await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let key: String = row.get(0)?;
    drop(rows);
    let key = MessageKey::from_storage(Uuid::parse_str(&key).map_err(|_| ObservationError::Codec)?);
    Ok(load_source(tx, key)
        .await?
        .filter(|stored| &stored.source.sender == sender))
}

async fn record_revision_mapping(
    tx: &mut IngressUowTransaction<'_>,
    room: &BareJid,
    stanza_id: &StanzaId,
    source_key: MessageKey,
) -> Result<(), ObservationError> {
    let key = source_key.to_storage().to_string();
    tx.transaction_mut().execute(
        "INSERT INTO extension_room_source_revisions (room_jid, room_stanza_id, source_key) VALUES (?, ?, ?) ON CONFLICT (room_jid, room_stanza_id) DO NOTHING",
        crate::db_params![room.to_string(), stanza_id.as_str(), &key],
    ).await?;
    let mut rows = tx.transaction_mut().query(
        "SELECT source_key FROM extension_room_source_revisions WHERE room_jid = ? AND room_stanza_id = ?",
        crate::db_params![room.to_string(), stanza_id.as_str()],
    ).await?;
    let stored: String = rows
        .next()
        .await?
        .ok_or(ObservationError::Database)?
        .get(0)?;
    if stored != key {
        return Err(ObservationError::SourceConflict);
    }
    Ok(())
}

pub(super) async fn terminal_receipt(
    tx: &mut IngressUowTransaction<'_>,
    subscription: &RoomObservationSubscription,
    key: MessageKey,
    category: &'static str,
) -> Result<(), ObservationError> {
    // This is the exact semantic receipt identity used by ingress::receipt_key
    // for a frozen RoomObserver intent. The sender/requester fields are not
    // part of that identity; the generation and artifact digest are.
    let semantic_key = IngressEffectKey::RoomObserver(
        subscription.room.clone(),
        subscription.plugin.clone(),
        subscription.generation,
        subscription.identity.clone(),
    );
    let hash: [u8; 32] = Sha256::digest(semantic_key.storage_identity().as_bytes()).into();
    EffectReceiptRepository::record_receipt(
        tx,
        key,
        EffectReceiptKind::from_storage(IngressEffectKind::RoomObserver.storage_tag()),
        &hash,
    )
    .await
    .map_err(|_| ObservationError::Database)?;
    tx.transaction_mut().execute(
        "INSERT INTO extension_room_observation_receipts (plugin_id, generation, room_jid, message_key, category) VALUES (?, ?, ?, ?, ?) ON CONFLICT (plugin_id, generation, room_jid, message_key) DO NOTHING",
        crate::db_params![subscription.plugin.as_str(), i64::try_from(subscription.generation.get()).map_err(|_| ObservationError::GenerationOutOfRange)?, subscription.room.to_string(), key.to_storage().to_string(), category],
    ).await?;
    Ok(())
}

pub(super) async fn stale_source_work(
    tx: &mut IngressUowTransaction<'_>,
    source_key: MessageKey,
    before_revision: Option<u64>,
    category: &'static str,
) -> Result<(), ObservationError> {
    let key = source_key.to_storage().to_string();
    let revision = before_revision
        .map(i64::try_from)
        .transpose()
        .map_err(|_| ObservationError::Codec)?;
    let mut rows = tx.transaction_mut().query(
        "SELECT plugin_id, generation, identity, room_jid, message_key FROM extension_room_observation_work WHERE source_key = ? AND status IN ('pending', 'leased') AND (? IS NULL OR revision < ?)",
        crate::db_params![&key, revision, revision],
    ).await?;
    let mut pending = Vec::new();
    while let Some(row) = rows.next().await? {
        let plugin: String = row.get(0)?;
        let generation: i64 = row.get(1)?;
        let identity: String = row.get(2)?;
        let room: String = row.get(3)?;
        let message_key: String = row.get(4)?;
        pending.push((plugin, generation, identity, room, message_key));
    }
    drop(rows);
    tx.transaction_mut().execute(
        "UPDATE extension_room_observation_work SET status = 'stale', terminal_category = ?, body = '', lease_id = NULL, lease_until_ms = NULL WHERE source_key = ? AND status IN ('pending', 'leased') AND (? IS NULL OR revision < ?)",
        crate::db_params![category, &key, revision, revision],
    ).await?;
    tx.transaction_mut().execute(
        "UPDATE extension_room_publications SET status = 'stale' WHERE source_key = ? AND status = 'pending' AND (? IS NULL OR revision < ?)",
        crate::db_params![&key, revision, revision],
    ).await?;
    for (plugin, generation, identity, room, message_key) in pending {
        let subscription = RoomObservationSubscription {
            plugin: PluginId::new(plugin).map_err(|_| ObservationError::Codec)?,
            generation: ObservationGeneration::new(
                u64::try_from(generation).map_err(|_| ObservationError::Codec)?,
            )
            .map_err(|_| ObservationError::Codec)?,
            identity: Sha256Digest::new(identity).map_err(|_| ObservationError::Codec)?,
            room: room.parse().map_err(|_| ObservationError::Codec)?,
        };
        let message_key = MessageKey::from_storage(
            Uuid::parse_str(&message_key).map_err(|_| ObservationError::Codec)?,
        );
        terminal_receipt(tx, &subscription, message_key, category).await?;
    }
    Ok(())
}

async fn stale_generation(
    tx: &mut IngressUowTransaction<'_>,
    plugin: &PluginId,
    generation: i64,
) -> Result<(), ObservationError> {
    let mut rows = tx.transaction_mut().query(
        "SELECT source_key FROM extension_room_observation_work WHERE plugin_id = ? AND generation < ? AND status IN ('pending', 'leased') UNION SELECT source_key FROM extension_room_publications WHERE plugin_id = ? AND generation < ? AND status = 'pending' ORDER BY source_key",
        crate::db_params![plugin.as_str(), generation, plugin.as_str(), generation],
    ).await?;
    let mut source_keys = Vec::new();
    while let Some(row) = rows.next().await? {
        let source_key: String = row.get(0)?;
        source_keys.push(source_key);
    }
    drop(rows);
    for source_key in source_keys {
        let key = MessageKey::from_storage(
            Uuid::parse_str(&source_key).map_err(|_| ObservationError::Codec)?,
        );
        let Some(_) = load_source(tx, key).await? else {
            continue;
        };
        let mut rows = tx.transaction_mut().query(
            "SELECT generation, identity, room_jid, message_key FROM extension_room_observation_work WHERE source_key = ? AND plugin_id = ? AND generation < ? AND status IN ('pending', 'leased')",
            crate::db_params![&source_key, plugin.as_str(), generation],
        ).await?;
        let mut pending = Vec::new();
        while let Some(row) = rows.next().await? {
            let old_generation: i64 = row.get(0)?;
            let identity: String = row.get(1)?;
            let room: String = row.get(2)?;
            let message_key: String = row.get(3)?;
            pending.push((old_generation, identity, room, message_key));
        }
        drop(rows);
        tx.transaction_mut().execute(
            "UPDATE extension_room_observation_work SET status = 'stale', terminal_category = 'generation_changed', body = '', lease_id = NULL, lease_until_ms = NULL WHERE source_key = ? AND plugin_id = ? AND generation < ? AND status IN ('pending', 'leased')",
            crate::db_params![&source_key, plugin.as_str(), generation],
        ).await?;
        tx.transaction_mut().execute(
            "UPDATE extension_room_publications SET status = 'stale' WHERE source_key = ? AND plugin_id = ? AND generation < ? AND status = 'pending'",
            crate::db_params![&source_key, plugin.as_str(), generation],
        ).await?;
        for (old_generation, identity, room, message_key) in pending {
            let subscription = RoomObservationSubscription {
                plugin: plugin.clone(),
                generation: ObservationGeneration::new(
                    u64::try_from(old_generation).map_err(|_| ObservationError::Codec)?,
                )
                .map_err(|_| ObservationError::Codec)?,
                identity: Sha256Digest::new(identity).map_err(|_| ObservationError::Codec)?,
                room: room.parse().map_err(|_| ObservationError::Codec)?,
            };
            let message_key = MessageKey::from_storage(
                Uuid::parse_str(&message_key).map_err(|_| ObservationError::Codec)?,
            );
            terminal_receipt(tx, &subscription, message_key, "generation_changed").await?;
        }
    }
    Ok(())
}

fn current_stanza_id(message: &Message, room: &BareJid) -> Option<StanzaId> {
    xep0359::extract_stanza_ids(message)
        .into_iter()
        .find(|id| id.by == *room)
        .and_then(|id| StanzaId::new(id.id).ok())
}

fn source_for_new_message(
    room: &BareJid,
    sender: &BareJid,
    stanza_id: StanzaId,
    origin_id: OriginId,
    body: &str,
    now: DateTime<Utc>,
) -> Result<RoomMessageSource, ObservationError> {
    let digest = Sha256Digest::new(hex::encode(Sha256::digest(body.as_bytes())))
        .map_err(|_| ObservationError::Codec)?;
    let observed_at = Timestamp::new(now.to_rfc3339()).map_err(|_| ObservationError::Codec)?;
    Ok(RoomMessageSource {
        room: room.clone(),
        revision_stanza_id: stanza_id.clone(),
        stanza_id,
        origin_id: Some(origin_id),
        sender: sender.clone(),
        revision: MessageRevision::new(0),
        body_digest: digest,
        observed_at,
    })
}

pub(super) async fn capture(
    tx: &mut IngressUowTransaction<'_>,
    source: CapturedRoomSource<'_>,
) -> Result<(), ObservationError> {
    let CapturedRoomSource {
        key,
        room,
        message,
        sender,
        intents,
        observed_at: now,
        correction_target,
    } = source;
    // Match the core room validator: malformed replace payloads are ordinary
    // messages there and must not make optional observation abort the archive.
    let correction = xep0308::extract_correction_from_message(message);
    let mut subscriptions: Vec<_> = intents
        .iter()
        .filter_map(|intent| match intent {
            IngressEffectIntent::RoomObserver {
                room: intent_room,
                plugin,
                generation,
                identity,
                ..
            } if intent_room == room => Some(RoomObservationSubscription {
                plugin: plugin.clone(),
                generation: *generation,
                identity: identity.clone(),
                room: room.clone(),
            }),
            _ => None,
        })
        .collect();
    if subscriptions.is_empty() && correction.is_none() {
        return Ok(());
    }
    subscriptions.sort_by_key(|subscription| subscription.plugin.as_str().to_string());
    let mut active = Vec::new();
    for subscription in subscriptions {
        if active_subscription(tx, &subscription).await? {
            active.push(subscription);
        } else {
            terminal_receipt(tx, &subscription, key, "subscription_unavailable").await?;
        }
    }
    // A stale replica's frozen observer is no longer allowed to schedule
    // provider work, but its accepted correction still advances the tracked
    // source so a newer generation cannot publish a score for old content.
    if active.is_empty() && correction.is_none() {
        return Ok(());
    }
    let subscriptions = active;

    let body = message
        .get_best_body(vec![])
        .map(|(_, body)| body.as_str())
        .unwrap_or("");
    let Some(stanza_id) = current_stanza_id(message, room) else {
        for subscription in &subscriptions {
            terminal_receipt(tx, subscription, key, "missing_room_stanza_id").await?;
        }
        return Ok(());
    };
    if body.trim().is_empty() && correction.is_none() {
        for subscription in &subscriptions {
            terminal_receipt(tx, subscription, key, "empty_body").await?;
        }
        return Ok(());
    }

    let (source_key, source) = if correction.is_some() {
        let Some(target) = correction_target else {
            for subscription in &subscriptions {
                terminal_receipt(
                    tx,
                    subscription,
                    key,
                    "missing_authoritative_correction_target",
                )
                .await?;
            }
            return Ok(());
        };
        let Some(mut stored) = source_for_authoritative_target(tx, room, sender, target).await?
        else {
            for subscription in &subscriptions {
                terminal_receipt(tx, subscription, key, "unknown_correction_target").await?;
            }
            return Ok(());
        };
        if stored.retracted {
            for subscription in &subscriptions {
                terminal_receipt(tx, subscription, key, "retracted_source").await?;
            }
            return Ok(());
        }
        if stored.source.revision_stanza_id != stanza_id {
            stored.source.revision = MessageRevision::new(
                stored
                    .source
                    .revision
                    .get()
                    .checked_add(1)
                    .ok_or(ObservationError::Codec)?,
            );
            stored.source.revision_stanza_id = stanza_id.clone();
            stored.source.body_digest =
                Sha256Digest::new(hex::encode(Sha256::digest(body.as_bytes())))
                    .map_err(|_| ObservationError::Codec)?;
            stored.source.observed_at =
                Timestamp::new(now.to_rfc3339()).map_err(|_| ObservationError::Codec)?;
            tx.transaction_mut().execute(
                "UPDATE extension_room_sources SET revision = ?, revision_stanza_id = ?, source_json = ? WHERE source_key = ? AND retracted = 0",
                crate::db_params![i64::try_from(stored.source.revision.get()).map_err(|_| ObservationError::Codec)?, stanza_id.as_str(), serde_json::to_string(&stored.source)?, stored.key.to_storage().to_string()],
            ).await?;
            stale_source_work(
                tx,
                stored.key,
                Some(stored.source.revision.get()),
                "superseded",
            )
            .await?;
        }
        record_revision_mapping(tx, room, &stanza_id, stored.key).await?;
        (stored.key, stored.source)
    } else {
        let Some(origin) =
            xep0359::extract_origin_id(message).and_then(|id| OriginId::new(id.id).ok())
        else {
            for subscription in &subscriptions {
                terminal_receipt(tx, subscription, key, "missing_origin_id").await?;
            }
            return Ok(());
        };
        let source = source_for_new_message(room, sender, stanza_id.clone(), origin, body, now)?;
        tx.transaction_mut().execute(
            "INSERT INTO extension_room_sources (source_key, room_jid, sender_jid, root_stanza_id, revision_stanza_id, root_origin_id, revision, source_json, retracted) VALUES (?, ?, ?, ?, ?, ?, 0, ?, 0) ON CONFLICT (source_key) DO NOTHING",
            crate::db_params![key.to_storage().to_string(), room.to_string(), sender.to_string(), source.stanza_id.as_str(), source.revision_stanza_id.as_str(), source.origin_id.as_ref().map(OriginId::as_str), serde_json::to_string(&source)?],
        ).await?;
        let stored = load_source(tx, key)
            .await?
            .ok_or(ObservationError::Database)?;
        if stored.source.room != source.room
            || stored.source.sender != source.sender
            || stored.source.stanza_id != source.stanza_id
            || stored.source.origin_id != source.origin_id
            || (stored.source.revision.get() == 0
                && stored.source.body_digest != source.body_digest)
        {
            return Err(ObservationError::SourceConflict);
        }
        if stored.retracted || stored.source.revision.get() != 0 {
            return Ok(());
        }
        record_revision_mapping(tx, room, &stanza_id, key).await?;
        (key, stored.source)
    };

    if body.trim().is_empty() {
        for subscription in &subscriptions {
            terminal_receipt(tx, subscription, key, "empty_body").await?;
        }
        return Ok(());
    }

    let body = DisplayText::new(body.to_string()).map_err(|_| ObservationError::Codec)?;
    for subscription in &subscriptions {
        let generation = i64::try_from(subscription.generation.get())
            .map_err(|_| ObservationError::GenerationOutOfRange)?;
        tx.transaction_mut().execute(
            "INSERT INTO extension_room_observation_work (id, source_key, message_key, plugin_id, generation, identity, room_jid, revision, source_json, body, status, due_at_ms) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'pending', ?) ON CONFLICT (plugin_id, generation, room_jid, source_key, revision) DO NOTHING",
            crate::db_params![Uuid::now_v7().to_string(), source_key.to_storage().to_string(), key.to_storage().to_string(), subscription.plugin.as_str(), generation, subscription.identity.as_str(), room.to_string(), i64::try_from(source.revision.get()).map_err(|_| ObservationError::Codec)?, serde_json::to_string(&source)?, body.as_str(), now.timestamp_millis()],
        ).await?;
    }
    Ok(())
}

pub(super) async fn retract(
    tx: &mut IngressUowTransaction<'_>,
    room: &BareJid,
    target: &xep0359::StanzaId,
) -> Result<(), ObservationError> {
    if target.by != *room {
        return Ok(());
    }
    let sql = locked(
        "SELECT source_key, source_json, retracted FROM extension_room_sources WHERE room_jid = ? AND (root_stanza_id = ? OR revision_stanza_id = ?)",
        tx.transaction_mut().driver(),
    );
    let mut rows = tx
        .transaction_mut()
        .query(
            &sql,
            crate::db_params![room.to_string(), target.id.as_str(), target.id.as_str()],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(());
    };
    let source = decode_source(&row)?;
    drop(rows);
    if source.retracted {
        return Ok(());
    }
    let key = source.key.to_storage().to_string();
    tx.transaction_mut()
        .execute(
            "UPDATE extension_room_sources SET retracted = 1 WHERE source_key = ?",
            crate::db_params![&key],
        )
        .await?;
    stale_source_work(tx, source.key, None, "retracted").await?;
    Ok(())
}
