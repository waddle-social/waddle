use jid::BareJid;
use uuid::Uuid;
use waddle_extensions::{
    ConfiguredRoomObserver, DisplayText, ObservationFailure, ObservationSkip, RoomMessageSource,
    RoomObservationOutcome, RoomObservationScope, RoomObservationSubscription,
};
use waddle_xmpp::ingress::MessageKey;

use crate::db::Row;
use crate::ingress_uow::IngressUowTransaction;

use super::sources::{active_subscription, load_source, locked, terminal_receipt};
use super::{ObservationError, ObservationWork};

const LEASE_MS: i64 = 180_000;
const MAX_ATTEMPTS: u32 = 20;

pub(super) fn retry_delay_ms(attempt: u32) -> i64 {
    (1_000_i64.saturating_mul(1_i64 << attempt.saturating_sub(1).min(6))).min(60_000)
}

pub(super) fn eligible_for_claim(
    status: &str,
    due_at_ms: i64,
    lease_until_ms: Option<i64>,
    now_ms: i64,
) -> bool {
    match status {
        "pending" => due_at_ms <= now_ms,
        "leased" => lease_until_ms.is_some_and(|until| until <= now_ms),
        _ => false,
    }
}

fn decode_work(
    row: &Row,
    subscription: &RoomObservationSubscription,
    lease: Uuid,
) -> Result<ObservationWork, ObservationError> {
    let id: String = row.get(0)?;
    let message_key: String = row.get(1)?;
    let source_json: String = row.get(2)?;
    let body: String = row.get(3)?;
    let attempt: i64 = row.get(4)?;
    Ok(ObservationWork {
        id: Uuid::parse_str(&id).map_err(|_| ObservationError::Codec)?,
        lease,
        message_key: MessageKey::from_storage(
            Uuid::parse_str(&message_key).map_err(|_| ObservationError::Codec)?,
        ),
        subscription: subscription.clone(),
        source: serde_json::from_str(&source_json)?,
        body: DisplayText::new(body).map_err(|_| ObservationError::Codec)?,
        attempt: u32::try_from(attempt).map_err(|_| ObservationError::Codec)?,
    })
}

pub(super) async fn claim(
    tx: &mut IngressUowTransaction<'_>,
    subscription: &RoomObservationSubscription,
    now_ms: i64,
) -> Result<Option<ObservationWork>, ObservationError> {
    if !active_subscription(tx, subscription).await? {
        return Ok(None);
    }
    let generation = i64::try_from(subscription.generation.get())
        .map_err(|_| ObservationError::GenerationOutOfRange)?;
    // Query candidates without a row lock, then lock the source before the
    // work row. The per-room actor applies the configured concurrency cap;
    // distinct source jobs may have independent active leases.
    let mut rows = tx.transaction_mut().query(
        "SELECT id, source_key FROM extension_room_observation_work WHERE plugin_id = ? AND generation = ? AND identity = ? AND room_jid = ? AND ((status = 'pending' AND due_at_ms <= ?) OR (status = 'leased' AND lease_until_ms <= ?)) ORDER BY due_at_ms, id LIMIT 32",
        crate::db_params![subscription.plugin.as_str(), generation, subscription.identity.as_str(), subscription.room.to_string(), now_ms, now_ms],
    ).await?;
    let mut candidates = Vec::new();
    while let Some(row) = rows.next().await? {
        let id: String = row.get(0)?;
        let source_key: String = row.get(1)?;
        candidates.push((id, source_key));
    }
    drop(rows);
    for (id, source_key) in candidates {
        let source_key = MessageKey::from_storage(
            Uuid::parse_str(&source_key).map_err(|_| ObservationError::Codec)?,
        );
        let source = load_source(tx, source_key).await?;
        let sql = locked(
            "SELECT id, message_key, source_json, body, attempt, revision, status, due_at_ms, lease_until_ms FROM extension_room_observation_work WHERE id = ?",
            tx.transaction_mut().driver(),
        );
        let mut work_rows = tx
            .transaction_mut()
            .query(&sql, crate::db_params![&id])
            .await?;
        let Some(row) = work_rows.next().await? else {
            continue;
        };
        let revision: i64 = row.get(5)?;
        let message_key: String = row.get(1)?;
        let source_json: String = row.get(2)?;
        let attempt: i64 = row.get(4)?;
        let status: String = row.get(6)?;
        let due_at_ms: i64 = row.get(7)?;
        let lease_until_ms: Option<i64> = row.get(8)?;
        drop(work_rows);
        // Candidate selection is deliberately unlocked so the source can be
        // locked first. Recheck the due predicate after both locks: another
        // worker may have just leased its final attempt.
        if !eligible_for_claim(&status, due_at_ms, lease_until_ms, now_ms) {
            continue;
        }
        let source_snapshot: RoomMessageSource = serde_json::from_str(&source_json)?;
        let current = source.as_ref().is_some_and(|source| {
            !source.retracted
                && u64::try_from(revision).ok() == Some(source.source.revision.get())
                && source_snapshot == source.source
        });
        if !current {
            tx.transaction_mut().execute(
                "UPDATE extension_room_observation_work SET status = 'stale', terminal_category = 'source_changed', body = '', lease_id = NULL, lease_until_ms = NULL WHERE id = ? AND status IN ('pending', 'leased')",
                crate::db_params![&id],
            ).await?;
            let message_key = MessageKey::from_storage(
                Uuid::parse_str(&message_key).map_err(|_| ObservationError::Codec)?,
            );
            terminal_receipt(tx, subscription, message_key, "source_changed").await?;
            continue;
        }
        if u32::try_from(attempt)
            .ok()
            .is_none_or(|attempt| attempt >= MAX_ATTEMPTS)
        {
            tx.transaction_mut().execute(
                "UPDATE extension_room_observation_work SET status = 'terminal', terminal_category = 'retry_exhausted', body = '', lease_id = NULL, lease_until_ms = NULL WHERE id = ? AND status IN ('pending', 'leased')",
                crate::db_params![&id],
            ).await?;
            let mut rows = tx
                .transaction_mut()
                .query(
                    "SELECT message_key FROM extension_room_observation_work WHERE id = ?",
                    crate::db_params![&id],
                )
                .await?;
            let message_key: String = rows
                .next()
                .await?
                .ok_or(ObservationError::Database)?
                .get(0)?;
            let message_key = MessageKey::from_storage(
                Uuid::parse_str(&message_key).map_err(|_| ObservationError::Codec)?,
            );
            terminal_receipt(tx, subscription, message_key, "retry_exhausted").await?;
            continue;
        }
        let lease = Uuid::now_v7();
        let changed = tx.transaction_mut().execute(
            "UPDATE extension_room_observation_work SET status = 'leased', lease_id = ?, lease_until_ms = ?, attempt = attempt + 1 WHERE id = ? AND ((status = 'pending' AND due_at_ms <= ?) OR (status = 'leased' AND lease_until_ms <= ?))",
            crate::db_params![lease.to_string(), now_ms.saturating_add(LEASE_MS), &id, now_ms, now_ms],
        ).await?;
        if changed == 1 {
            let mut rows = tx.transaction_mut().query(
                "SELECT id, message_key, source_json, body, attempt FROM extension_room_observation_work WHERE id = ?",
                crate::db_params![&id],
            ).await?;
            let row = rows.next().await?.ok_or(ObservationError::Database)?;
            return decode_work(&row, subscription, lease).map(Some);
        }
    }
    Ok(None)
}

fn failure_category(failure: ObservationFailure) -> &'static str {
    match failure {
        ObservationFailure::TemporaryFailure => "temporary_failure",
        ObservationFailure::InvalidRequest => "invalid_request",
        ObservationFailure::Denied => "denied",
        ObservationFailure::UnsupportedEvent => "unsupported_event",
        ObservationFailure::RuntimeFailure => "runtime_failure",
        ObservationFailure::ResourceLimit => "resource_limit",
        ObservationFailure::DeadlineExceeded => "deadline_exceeded",
        ObservationFailure::InvalidResult => "invalid_result",
        ObservationFailure::SourceMismatch => "source_mismatch",
    }
}

fn skip_category(skip: ObservationSkip) -> &'static str {
    match skip {
        ObservationSkip::MissingOriginId => "missing_origin_id",
        ObservationSkip::SubscriptionUnavailable => "subscription_unavailable",
        ObservationSkip::NoResult => "no_result",
    }
}

pub(super) async fn finish(
    tx: &mut IngressUowTransaction<'_>,
    work: &ObservationWork,
    outcome: &RoomObservationOutcome,
    now_ms: i64,
) -> Result<bool, ObservationError> {
    if !active_subscription(tx, &work.subscription).await? {
        return Ok(false);
    }
    let mut rows = tx
        .transaction_mut()
        .query(
            "SELECT source_key FROM extension_room_observation_work WHERE id = ?",
            crate::db_params![work.id.to_string()],
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
    let Some(source) = load_source(tx, key).await? else {
        return Ok(false);
    };
    if source.retracted || source.source != work.source {
        return Ok(false);
    }
    let sql = locked(
        "SELECT lease_id, status, source_json, attempt, message_key, generation, identity, room_jid FROM extension_room_observation_work WHERE id = ?",
        tx.transaction_mut().driver(),
    );
    let mut rows = tx
        .transaction_mut()
        .query(&sql, crate::db_params![work.id.to_string()])
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(false);
    };
    let lease_id: Option<String> = row.get(0)?;
    let status: String = row.get(1)?;
    let source_json: String = row.get(2)?;
    let attempt: i64 = row.get(3)?;
    let message_key: String = row.get(4)?;
    let generation: i64 = row.get(5)?;
    let identity: String = row.get(6)?;
    let room: String = row.get(7)?;
    drop(rows);
    if lease_id.as_deref() != Some(work.lease.to_string().as_str())
        || status != "leased"
        || serde_json::from_str::<RoomMessageSource>(&source_json)? != work.source
        || message_key != work.message_key.to_storage().to_string()
        || u64::try_from(generation).ok() != Some(work.subscription.generation.get())
        || identity != work.subscription.identity.as_str()
        || room != work.subscription.room.to_string()
    {
        return Ok(false);
    }
    let (status, category, due_at_ms, usage) = match outcome {
        RoomObservationOutcome::Completed(result) => {
            for (index, payload) in result.payloads.iter().enumerate() {
                tx.transaction_mut().execute(
                    "INSERT INTO extension_room_publications (id, work_id, output_index, source_key, plugin_id, generation, identity, room_jid, revision, source_json, payload_json, status) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'pending') ON CONFLICT (work_id, output_index) DO NOTHING",
                    crate::db_params![Uuid::now_v7().to_string(), work.id.to_string(), i64::try_from(index).map_err(|_| ObservationError::Codec)?, &source_key, work.subscription.plugin.as_str(), generation, work.subscription.identity.as_str(), work.subscription.room.to_string(), i64::try_from(work.source.revision.get()).map_err(|_| ObservationError::Codec)?, &source_json, serde_json::to_string(payload)?],
                ).await?;
            }
            (
                "completed",
                "completed",
                now_ms,
                result
                    .usage
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?,
            )
        }
        RoomObservationOutcome::RetryableFailure(failure)
            if u32::try_from(attempt)
                .ok()
                .is_some_and(|n| n < MAX_ATTEMPTS) =>
        {
            (
                "pending",
                failure_category(*failure),
                now_ms.saturating_add(retry_delay_ms(
                    u32::try_from(attempt).map_err(|_| ObservationError::Codec)?,
                )),
                None,
            )
        }
        RoomObservationOutcome::RetryableFailure(_) => {
            ("terminal", "retry_exhausted", now_ms, None)
        }
        RoomObservationOutcome::PermanentFailure(failure) => {
            ("terminal", failure_category(*failure), now_ms, None)
        }
        RoomObservationOutcome::NotApplicable(skip) => {
            ("terminal", skip_category(*skip), now_ms, None)
        }
    };
    tx.transaction_mut().execute(
        "UPDATE extension_room_observation_work SET status = ?, terminal_category = ?, due_at_ms = ?, body = CASE WHEN ? = 'pending' THEN body ELSE '' END, lease_id = NULL, lease_until_ms = NULL, usage_json = ? WHERE id = ? AND lease_id = ? AND status = 'leased'",
        crate::db_params![status, category, due_at_ms, status, usage, work.id.to_string(), work.lease.to_string()],
    ).await?;
    if status != "pending" {
        terminal_receipt(tx, &work.subscription, work.message_key, category).await?;
    }
    Ok(true)
}

pub(super) async fn due_rooms(
    tx: &mut IngressUowTransaction<'_>,
    observer: &ConfiguredRoomObserver,
    after: Option<&BareJid>,
    now_ms: i64,
    limit: u32,
) -> Result<Vec<BareJid>, ObservationError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    match &observer.scope {
        RoomObservationScope::Rooms(configured) => {
            let mut configured = configured.clone();
            configured.sort_by_key(ToString::to_string);
            configured.dedup();
            let mut rooms = Vec::new();
            for room in configured
                .iter()
                .filter(|room| after.is_none_or(|cursor| room.to_string() > cursor.to_string()))
            {
                if room_due(tx, observer, room, now_ms).await? {
                    rooms.push(room.clone());
                    if rooms.len() >= limit as usize {
                        return Ok(rooms);
                    }
                }
            }
            if rooms.is_empty() && after.is_some() {
                for room in &configured {
                    if room_due(tx, observer, room, now_ms).await? {
                        rooms.push(room.clone());
                        if rooms.len() >= limit as usize {
                            break;
                        }
                    }
                }
            }
            Ok(rooms)
        }
        RoomObservationScope::AllHostedRooms => {
            let rooms = all_rooms_page(tx, observer, after, now_ms, limit).await?;
            if rooms.is_empty() && after.is_some() {
                all_rooms_page(tx, observer, None, now_ms, limit).await
            } else {
                Ok(rooms)
            }
        }
    }
}

async fn room_due(
    tx: &mut IngressUowTransaction<'_>,
    observer: &ConfiguredRoomObserver,
    room: &BareJid,
    now_ms: i64,
) -> Result<bool, ObservationError> {
    let subscription = RoomObservationSubscription {
        plugin: observer.plugin.clone(),
        generation: observer.generation,
        identity: observer.identity.clone(),
        room: room.clone(),
    };
    if !active_subscription(tx, &subscription).await? {
        return Ok(false);
    }
    let generation = i64::try_from(observer.generation.get())
        .map_err(|_| ObservationError::GenerationOutOfRange)?;
    let mut rows = tx.transaction_mut().query(
        "SELECT 1 FROM extension_room_observation_work WHERE plugin_id = ? AND generation = ? AND identity = ? AND room_jid = ? AND ((status = 'pending' AND due_at_ms <= ?) OR (status = 'leased' AND lease_until_ms <= ?)) LIMIT 1",
        crate::db_params![observer.plugin.as_str(), generation, observer.identity.as_str(), room.to_string(), now_ms, now_ms],
    ).await?;
    if rows.next().await?.is_some() {
        return Ok(true);
    }
    drop(rows);
    let mut rows = tx.transaction_mut().query(
        "SELECT 1 FROM extension_room_publications WHERE plugin_id = ? AND generation = ? AND identity = ? AND room_jid = ? AND status = 'pending' LIMIT 1",
        crate::db_params![observer.plugin.as_str(), generation, observer.identity.as_str(), room.to_string()],
    ).await?;
    Ok(rows.next().await?.is_some())
}

async fn all_rooms_page(
    tx: &mut IngressUowTransaction<'_>,
    observer: &ConfiguredRoomObserver,
    after: Option<&BareJid>,
    now_ms: i64,
    limit: u32,
) -> Result<Vec<BareJid>, ObservationError> {
    let generation = i64::try_from(observer.generation.get())
        .map_err(|_| ObservationError::GenerationOutOfRange)?;
    let cursor = after.map(ToString::to_string).unwrap_or_default();
    let mut rows = tx.transaction_mut().query(
        "SELECT room_jid FROM extension_room_observation_work WHERE plugin_id = ? AND generation = ? AND identity = ? AND room_jid > ? AND ((status = 'pending' AND due_at_ms <= ?) OR (status = 'leased' AND lease_until_ms <= ?)) UNION SELECT room_jid FROM extension_room_publications WHERE plugin_id = ? AND generation = ? AND identity = ? AND room_jid > ? AND status = 'pending' ORDER BY room_jid LIMIT ?",
        crate::db_params![observer.plugin.as_str(), generation, observer.identity.as_str(), &cursor, now_ms, now_ms, observer.plugin.as_str(), generation, observer.identity.as_str(), &cursor, i64::from(limit)],
    ).await?;
    let mut rooms = Vec::new();
    while let Some(row) = rows.next().await? {
        let value: String = row.get(0)?;
        let room: BareJid = value.parse().map_err(|_| ObservationError::Codec)?;
        rooms.push(room);
    }
    drop(rows);
    // The configured scope covers every hosted room, but still prove that
    // this exact generation remains active before returning scheduler hints.
    if let Some(room) = rooms.first() {
        let subscription = RoomObservationSubscription {
            plugin: observer.plugin.clone(),
            generation: observer.generation,
            identity: observer.identity.clone(),
            room: room.clone(),
        };
        if !active_subscription(tx, &subscription).await? {
            return Ok(Vec::new());
        }
    }
    Ok(rooms)
}
