use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use uuid::Uuid;
use waddle_xmpp::auth::AuthenticatedPrincipalRef;
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::persistence::{
    IngressCustodyDisposition, PersistedDetachedAppend, PersistedIngressAppend, PersistedSession,
    PersistedUnackedStanza,
};
use waddle_xmpp::stream_management::SmIngressAppendKey;

use crate::db::{Database, DatabaseDriver, Transaction};

use super::super::atomic_store::{
    apply_snapshot_in_transaction, Ledger, PreparedSnapshot, SnapshotApplyError, StoreOutcome,
};
use super::super::{codec, ingress_append, promotion_inventory};
use super::digest::digest;
use super::frontier::ExposureFrontier;
use super::types::{
    storage, ApplyResult, Capture, Effect, Mutation, Readback, Request, Revision, RevisionError,
};

/// Database handle for the unselected revision protocol. It never exposes a
/// transaction or invokes the legacy persistence trait's fallback methods.
#[derive(Clone)]
pub struct RevisionedSmStorage {
    db: Database,
}

#[derive(Serialize, Deserialize)]
struct StoredResult {
    revision: i64,
    effect: StoredEffect,
}

#[derive(Serialize, Deserialize)]
enum StoredEffect {
    Snapshot { withheld: Vec<usize> },
    Appended,
    AlreadyAllocated { accepting_stream: String },
    Exposed { ordinal: u64 },
    Acknowledged,
    Retired,
}

impl RevisionedSmStorage {
    /// Create a handle for an owner-prepared database. No startup path selects
    /// this handle, and constructing it performs no DDL or authority check.
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Capture the session, queue, principal and revision in one gated
    /// transaction. Callers must derive any subsequent snapshot from these
    /// rows and independently serialize process-local publication with capture.
    pub async fn capture(&self, stream: &SmSessionId) -> Result<Capture, RevisionError> {
        let mut tx = self.db.begin_immediate().await.map_err(storage)?;
        lock_stream(&mut tx, stream).await?;
        let capture = capture_locked(&mut tx, stream).await?;
        tx.commit().await.map_err(storage)?;
        Ok(capture)
    }

    /// Apply an immutable request and record its exact result in the same
    /// transaction. Repeating the same request returns the recorded result;
    /// reusing its UUID with different input is rejected.
    pub async fn apply(&self, request: &Request<'_>) -> Result<ApplyResult, RevisionError> {
        let hash = digest(request)?;
        let mut tx = self.db.begin_immediate().await.map_err(storage)?;
        lock_stream(&mut tx, request.mutation.stream()).await?;
        if let Some(result) = lookup(&mut tx, request, &hash).await? {
            tx.commit().await.map_err(storage)?;
            return Ok(result);
        }
        let (effect, frontier) = match &request.mutation {
            Mutation::Create {
                generation,
                principal,
                session,
                unacked,
                drained,
            } => {
                validate_snapshot(principal, session, unacked, drained)?;
                let frontier = ExposureFrontier::initial(session)?;
                create_generation(&mut tx, *generation, session, frontier).await?;
                let effect = snapshot(&mut tx, principal, session, unacked, drained).await?;
                (effect, frontier)
            }
            Mutation::Snapshot {
                revision,
                session,
                unacked,
                drained,
            } => {
                let current = check_revision(&mut tx, revision).await?;
                validate_session(revision, session)?;
                validate_snapshot(&revision.principal, session, unacked, drained)?;
                validate_snapshot_extension(&current, session, unacked)?;
                let advanced = session
                    .outbound_count
                    .wrapping_sub(current.session.outbound_count);
                let frontier = current.frontier.extend(advanced)?;
                let effect =
                    snapshot(&mut tx, &revision.principal, session, unacked, drained).await?;
                (effect, frontier)
            }
            Mutation::Append { revision, append } => {
                let current = check_revision(&mut tx, revision).await?;
                let effect = append_detached(&mut tx, revision, &current.session, append).await?;
                let frontier = if matches!(&effect, StoredEffect::Appended) {
                    current.frontier.extend(1)?
                } else {
                    current.frontier
                };
                (effect, frontier)
            }
            Mutation::Expose { revision, ordinal } => {
                let current = check_revision(&mut tx, revision).await?;
                let frontier = current.frontier.expose(*ordinal, &current.unacked)?;
                (StoredEffect::Exposed { ordinal: *ordinal }, frontier)
            }
            Mutation::Acknowledge { revision, h, mode } => {
                let current = check_revision(&mut tx, revision).await?;
                let frontier = current.frontier.acknowledge(*h)?;
                reserve_acknowledgement_window(&mut tx, &current, *h).await?;
                let driver = tx.driver();
                promotion_inventory::acknowledge(&mut tx, &revision.stream, *h, driver, *mode)
                    .await
                    .map_err(storage)?;
                clear_acknowledged_gap(&mut tx, &current.session, *h).await?;
                (StoredEffect::Acknowledged, frontier)
            }
            Mutation::ConfirmAcknowledgement { .. } => return Err(RevisionError::Unsupported),
            Mutation::Tombstone { .. } => return Err(RevisionError::Unsupported),
            Mutation::Prune { .. } => return Err(RevisionError::Unsupported),
            Mutation::Retire { revision } => {
                let current = check_revision(&mut tx, revision).await?;
                // A replay gap may account for an empty queue while counted
                // frames remain unacknowledged. No terminal transition may
                // discard that interval until an ACK has covered it.
                if current.session.last_acked != current.session.outbound_count
                    || current.session.replay_gap_through.is_some()
                {
                    return Err(RevisionError::OutstandingCustody);
                }
                let driver = tx.driver();
                promotion_inventory::require_settled(&mut tx, &revision.stream, driver)
                    .await
                    .map_err(storage)?;
                let mut rows = tx.query(
                    "SELECT 1 WHERE EXISTS (SELECT 1 FROM sm_unacked WHERE stream_id = ?) OR EXISTS (SELECT 1 FROM sm_ingress_appends WHERE accepting_stream_id = ? AND disposition = 0)",
                    crate::db_params![revision.stream.as_str(), revision.stream.as_str()],
                ).await.map_err(storage)?;
                if rows.next().await.map_err(storage)?.is_some() {
                    return Err(RevisionError::OutstandingCustody);
                }
                drop(rows);
                tx.execute(
                    "DELETE FROM sm_sessions WHERE stream_id = ?",
                    crate::db_params![revision.stream.as_str()],
                )
                .await
                .map_err(storage)?;
                (StoredEffect::Retired, current.frontier)
            }
        };
        let expected = match &request.mutation {
            Mutation::Create { .. } => 0,
            Mutation::Snapshot { revision, .. }
            | Mutation::Append { revision, .. }
            | Mutation::Expose { revision, .. }
            | Mutation::Acknowledge { revision, .. }
            | Mutation::ConfirmAcknowledgement { revision, .. }
            | Mutation::Tombstone { revision, .. }
            | Mutation::Prune { revision, .. }
            | Mutation::Retire { revision } => revision.value,
        };
        let next = expected
            .checked_add(1)
            .ok_or(RevisionError::RevisionExhausted)?;
        let terminal = i64::from(matches!(&request.mutation, Mutation::Retire { .. }));
        let (acknowledged, exposed, appended) = frontier.sql_values()?;
        let changed = tx.execute(
            "UPDATE sm_revision_authority SET revision = ?, terminal = ?, acknowledged_ordinal = ?, exposed_ordinal = ?, appended_ordinal = ? WHERE stream_id = ? AND generation = ? AND revision = ? AND terminal = 0",
            crate::db_params![next, terminal, acknowledged, exposed, appended, request.mutation.stream().as_str(), request.mutation.generation().to_string(), expected],
        ).await.map_err(storage)?;
        if changed != 1 {
            return Err(RevisionError::StaleRevision);
        }
        let stored = StoredResult {
            revision: next,
            effect,
        };
        let result = decode_result(request, &stored)?;
        let encoded = serde_json::to_string(&stored).map_err(storage)?;
        tx.execute(
            "INSERT INTO sm_revision_operations (operation_id, stream_id, generation, input_digest, result) VALUES (?, ?, ?, ?, ?)",
            crate::db_params![request.id.to_string(), request.mutation.stream().as_str(), request.mutation.generation().to_string(), hash, encoded],
        ).await.map_err(storage)?;
        tx.commit().await.map_err(storage)?;
        Ok(result)
    }

    /// Read only the immutable operation receipt. `Unconfirmed` is not proof
    /// of rollback: the original backend may still commit after this read.
    pub async fn readback(&self, request: &Request<'_>) -> Result<Readback, RevisionError> {
        let hash = digest(request)?;
        let mut tx = self.db.begin().await.map_err(storage)?;
        let result = lookup(&mut tx, request, &hash).await?;
        tx.commit().await.map_err(storage)?;
        Ok(match result {
            Some(result) => Readback::Applied(result),
            None => Readback::Unconfirmed,
        })
    }
}

/// Reserve the wrapping settlement key before deleting any acknowledged
/// frames. A previous full counter cycle can leave the same key unsettled;
/// the shared promotion helper's conflict-ignoring insert cannot distinguish
/// that older occurrence from this one. Lock the session row first to keep
/// its existing session -> window lock order, and reject a collision rather
/// than losing settlement evidence. A retry of the exact committed operation
/// is resolved from its receipt before reaching this reservation.
async fn reserve_acknowledgement_window(
    tx: &mut Transaction<'_>,
    current: &Capture,
    h: u32,
) -> Result<(), RevisionError> {
    let sql = if tx.driver() == DatabaseDriver::Postgres {
        "SELECT outbound_count, last_acked FROM sm_sessions WHERE stream_id = ? FOR UPDATE"
    } else {
        "SELECT outbound_count, last_acked FROM sm_sessions WHERE stream_id = ?"
    };
    let mut rows = tx
        .query(sql, crate::db_params![current.revision.stream.as_str()])
        .await
        .map_err(storage)?;
    let row = rows
        .next()
        .await
        .map_err(storage)?
        .ok_or(RevisionError::GenerationChanged)?;
    let outbound: i64 = row.get(0).map_err(storage)?;
    let acknowledged: i64 = row.get(1).map_err(storage)?;
    if outbound != i64::from(current.session.outbound_count)
        || acknowledged != i64::from(current.session.last_acked)
    {
        return Err(RevisionError::StaleRevision);
    }
    drop(rows);
    if h == current.session.last_acked {
        return Ok(());
    }
    let inserted = tx.execute(
        "INSERT INTO sm_acknowledged_windows (stream_id, from_exclusive, to_inclusive) VALUES (?, ?, ?) ON CONFLICT (stream_id, from_exclusive, to_inclusive) DO NOTHING",
        crate::db_params![current.revision.stream.as_str(), i64::from(current.session.last_acked), i64::from(h)],
    ).await.map_err(storage)?;
    if inserted != 1 {
        return Err(RevisionError::AcknowledgementWindowConflict);
    }
    Ok(())
}

async fn lock_stream(tx: &mut Transaction<'_>, stream: &SmSessionId) -> Result<(), RevisionError> {
    if tx.driver() == DatabaseDriver::Postgres {
        let mut rows = tx
            .query(
                "SELECT pg_catalog.current_setting('transaction_isolation')",
                (),
            )
            .await
            .map_err(storage)?;
        let row = rows
            .next()
            .await
            .map_err(storage)?
            .ok_or(RevisionError::Unsupported)?;
        let isolation: String = row.get(0).map_err(storage)?;
        if isolation != "read committed" {
            return Err(RevisionError::Unsupported);
        }
        tx.execute(
            "SELECT pg_catalog.pg_advisory_xact_lock(1463895108, pg_catalog.hashtext(?))",
            crate::db_params![stream.as_str()],
        )
        .await
        .map_err(storage)?;
    }
    Ok(())
}

async fn lookup(
    tx: &mut Transaction<'_>,
    request: &Request<'_>,
    hash: &[u8],
) -> Result<Option<ApplyResult>, RevisionError> {
    let mut rows = tx
        .query(
            "SELECT stream_id, generation, input_digest, result FROM sm_revision_operations WHERE operation_id = ?",
            crate::db_params![request.id.to_string()],
        )
        .await
        .map_err(storage)?;
    let Some(row) = rows.next().await.map_err(storage)? else {
        return Ok(None);
    };
    let stream: String = row.get(0).map_err(storage)?;
    let generation: String = row.get(1).map_err(storage)?;
    let stored_hash: Vec<u8> = row.get(2).map_err(storage)?;
    let encoded: String = row.get(3).map_err(storage)?;
    if stream != request.mutation.stream().as_str()
        || generation != request.mutation.generation().to_string()
        || stored_hash != hash
    {
        return Err(RevisionError::OperationIdentity);
    }
    let stored: StoredResult = serde_json::from_str(&encoded).map_err(storage)?;
    Ok(Some(decode_result(request, &stored)?))
}

fn decode_result(
    request: &Request<'_>,
    stored: &StoredResult,
) -> Result<ApplyResult, RevisionError> {
    let effect = match (&stored.effect, &request.mutation) {
        (StoredEffect::Snapshot { withheld }, Mutation::Create { drained, .. })
        | (StoredEffect::Snapshot { withheld }, Mutation::Snapshot { drained, .. }) => {
            let keys = withheld
                .iter()
                .map(|index| {
                    drained
                        .get(*index)
                        .map(|append| append.key.clone())
                        .ok_or(RevisionError::OperationIdentity)
                })
                .collect::<Result<Vec<_>, _>>()?;
            Effect::Snapshot { withheld: keys }
        }
        (StoredEffect::Appended, Mutation::Append { .. }) => Effect::Appended,
        (StoredEffect::AlreadyAllocated { accepting_stream }, Mutation::Append { .. }) => {
            Effect::AlreadyAllocated {
                accepting_stream: SmSessionId::new(accepting_stream),
            }
        }
        (
            StoredEffect::Exposed { ordinal },
            Mutation::Expose {
                ordinal: requested, ..
            },
        ) if ordinal == requested => Effect::Exposed { ordinal: *ordinal },
        (StoredEffect::Acknowledged, Mutation::Acknowledge { .. }) => Effect::Acknowledged,
        (StoredEffect::Retired, Mutation::Retire { .. }) => Effect::Retired,
        _ => return Err(RevisionError::OperationIdentity),
    };
    Ok(ApplyResult {
        revision: stored.revision,
        effect,
    })
}

async fn capture_locked(
    tx: &mut Transaction<'_>,
    stream: &SmSessionId,
) -> Result<Capture, RevisionError> {
    let mut rows = tx
        .query(
            "SELECT generation, occupancy_session, full_jid, revision, terminal, acknowledged_ordinal, exposed_ordinal, appended_ordinal FROM sm_revision_authority WHERE stream_id = ?",
            crate::db_params![stream.as_str()],
        )
        .await
        .map_err(storage)?;
    let row = rows
        .next()
        .await
        .map_err(storage)?
        .ok_or(RevisionError::GenerationChanged)?;
    let generation: String = row.get(0).map_err(storage)?;
    let occupancy: String = row.get(1).map_err(storage)?;
    let full_jid: String = row.get(2).map_err(storage)?;
    let value: i64 = row.get(3).map_err(storage)?;
    let terminal: i64 = row.get(4).map_err(storage)?;
    let acknowledged: i64 = row.get(5).map_err(storage)?;
    let exposed: i64 = row.get(6).map_err(storage)?;
    let appended: i64 = row.get(7).map_err(storage)?;
    if terminal != 0 {
        return Err(RevisionError::Terminal);
    }
    drop(rows);
    let mut rows = tx.query(
        "SELECT stream_id, user_id, full_jid, occupancy_session, inbound_count, outbound_count, last_acked, max_resume_secs, detached_at_ms, max_resume_duration_ms, carbons_enabled, roster_interested, blocklist_interested, presence_available, presence_show, presence_status, presence_priority, replay_gap_through, presence_payloads FROM sm_sessions WHERE stream_id = ?",
        crate::db_params![stream.as_str()],
    ).await.map_err(storage)?;
    let row = rows
        .next()
        .await
        .map_err(storage)?
        .ok_or(RevisionError::GenerationChanged)?;
    let stored_occupancy: Option<String> = row.get(3).map_err(storage)?;
    for column in [4, 5, 6] {
        let value: i64 = row.get(column).map_err(storage)?;
        u32::try_from(value).map_err(|_| RevisionError::InvalidInput)?;
    }
    for column in [7, 17] {
        let value: Option<i64> = row.get(column).map_err(storage)?;
        if value.is_some_and(|value| u32::try_from(value).is_err()) {
            return Err(RevisionError::InvalidInput);
        }
    }
    if row.get::<i64>(9).map_err(storage)? < 0
        || i8::try_from(row.get::<i64>(16).map_err(storage)?).is_err()
    {
        return Err(RevisionError::InvalidInput);
    }
    for column in [10, 11, 12, 13] {
        if !matches!(row.get::<i64>(column).map_err(storage)?, 0 | 1) {
            return Err(RevisionError::InvalidInput);
        }
    }
    let raw_presence: Option<String> = row.get(18).map_err(storage)?;
    let session = codec::decode_session(&row).map_err(storage)?;
    let frontier = ExposureFrontier::restored(acknowledged, exposed, appended, &session)?;
    if stored_occupancy.as_deref() != Some(occupancy.as_str())
        || session.occupancy_session.to_string() != occupancy
        || session.jid.to_string() != full_jid
        || codec::serialize_presence_payloads(&session.presence_payloads).map_err(storage)?
            != raw_presence
    {
        return Err(RevisionError::GenerationChanged);
    }
    drop(rows);
    let mut rows = tx.query(
        "SELECT bare_jid, auth_context_id, auth_context_version, principal_auth_epoch FROM sm_sessions WHERE stream_id = ?",
        crate::db_params![stream.as_str()],
    ).await.map_err(storage)?;
    let row = rows
        .next()
        .await
        .map_err(storage)?
        .ok_or(RevisionError::GenerationChanged)?;
    let principal = codec::decode_session_principal(&row)
        .map_err(storage)?
        .ok_or(RevisionError::GenerationChanged)?;
    if principal.bare_jid() != &session.jid.to_bare() {
        return Err(RevisionError::GenerationChanged);
    }
    drop(rows);
    let projection = codec::bounded_provenance_projection(tx.driver(), "provenance");
    let sql = format!("SELECT stream_id, sequence, stanza_xml, original_receipt_at_ms, ingress_receipts, {projection} FROM sm_unacked WHERE stream_id = ? ORDER BY sequence");
    let mut rows = tx
        .query(&sql, crate::db_params![stream.as_str()])
        .await
        .map_err(storage)?;
    let mut unacked = Vec::new();
    while let Some(row) = rows.next().await.map_err(storage)? {
        let raw_provenance: Option<Vec<u8>> = row.get(5).map_err(storage)?;
        let frame = codec::decode_unacked(&row).map_err(storage)?;
        if raw_provenance.as_deref() != Some(frame.provenance.encode().as_slice()) {
            return Err(RevisionError::InvalidInput);
        }
        unacked.push(frame);
    }
    validate_snapshot(&principal, &session, &unacked, &[])?;
    let revision = Revision {
        stream: stream.clone(),
        generation: generation.parse().map_err(storage)?,
        value,
        principal: principal.clone(),
        jid: session.jid.clone(),
        occupancy: session.occupancy_session,
    };
    Ok(Capture {
        revision,
        principal,
        session,
        unacked,
        frontier,
    })
}

async fn check_revision(
    tx: &mut Transaction<'_>,
    revision: &Revision,
) -> Result<Capture, RevisionError> {
    let current = capture_locked(tx, &revision.stream).await?;
    if current.revision.generation != revision.generation
        || current.revision.jid != revision.jid
        || current.revision.occupancy != revision.occupancy
        || current.principal != revision.principal
    {
        return Err(RevisionError::GenerationChanged);
    }
    if current.revision.value != revision.value {
        return Err(RevisionError::StaleRevision);
    }
    if revision.value == i64::MAX {
        return Err(RevisionError::RevisionExhausted);
    }
    Ok(current)
}

async fn create_generation(
    tx: &mut Transaction<'_>,
    generation: Uuid,
    session: &PersistedSession,
    frontier: ExposureFrontier,
) -> Result<(), RevisionError> {
    let stream = &session.stream_id;
    let mut rows = tx
        .query(
            "SELECT terminal FROM sm_revision_authority WHERE stream_id = ?",
            crate::db_params![stream.as_str()],
        )
        .await
        .map_err(storage)?;
    if let Some(row) = rows.next().await.map_err(storage)? {
        let terminal: i64 = row.get(0).map_err(storage)?;
        if terminal != 1 {
            return Err(RevisionError::GenerationChanged);
        }
    }
    drop(rows);
    let mut rows = tx.query(
        "SELECT 1 WHERE EXISTS (SELECT 1 FROM sm_sessions WHERE stream_id = ?) OR EXISTS (SELECT 1 FROM sm_unacked WHERE stream_id = ?) OR EXISTS (SELECT 1 FROM sm_ingress_appends WHERE accepting_stream_id = ?) OR EXISTS (SELECT 1 FROM sm_acknowledged_windows WHERE stream_id = ?)",
        crate::db_params![stream.as_str(), stream.as_str(), stream.as_str(), stream.as_str()],
    ).await.map_err(storage)?;
    if rows.next().await.map_err(storage)?.is_some() {
        return Err(RevisionError::GenerationChanged);
    }
    drop(rows);
    let inserted = tx.execute(
        "INSERT INTO sm_revision_generations (generation, stream_id, occupancy_session) VALUES (?, ?, ?) ON CONFLICT DO NOTHING",
        crate::db_params![generation.to_string(), stream.as_str(), session.occupancy_session.to_string()],
    ).await.map_err(storage)?;
    if inserted != 1 {
        return Err(RevisionError::GenerationChanged);
    }
    let (acknowledged, exposed, appended) = frontier.sql_values()?;
    let changed = tx.execute(
        "INSERT INTO sm_revision_authority (stream_id, generation, occupancy_session, full_jid, revision, terminal, acknowledged_ordinal, exposed_ordinal, appended_ordinal) VALUES (?, ?, ?, ?, 0, 0, ?, ?, ?) ON CONFLICT (stream_id) DO UPDATE SET generation = excluded.generation, occupancy_session = excluded.occupancy_session, full_jid = excluded.full_jid, revision = 0, terminal = 0, acknowledged_ordinal = excluded.acknowledged_ordinal, exposed_ordinal = excluded.exposed_ordinal, appended_ordinal = excluded.appended_ordinal WHERE sm_revision_authority.terminal = 1",
        crate::db_params![stream.as_str(), generation.to_string(), session.occupancy_session.to_string(), session.jid.to_string(), acknowledged, exposed, appended],
    ).await.map_err(storage)?;
    if changed != 1 {
        return Err(RevisionError::GenerationChanged);
    }
    Ok(())
}

fn validate_session(revision: &Revision, session: &PersistedSession) -> Result<(), RevisionError> {
    if session.stream_id != revision.stream
        || session.jid != revision.jid
        || session.occupancy_session != revision.occupancy
    {
        return Err(RevisionError::GenerationChanged);
    }
    Ok(())
}

fn validate_snapshot(
    principal: &AuthenticatedPrincipalRef,
    session: &PersistedSession,
    unacked: &[PersistedUnackedStanza],
    drained: &[PersistedIngressAppend],
) -> Result<(), RevisionError> {
    if session.stream_id.as_str().is_empty()
        || principal.bare_jid() != &session.jid.to_bare()
        || session.occupancy_session.as_uuid().is_nil()
        || session.outbound_count.wrapping_sub(session.last_acked) >= (1_u32 << 31)
        || unacked
            .iter()
            .any(|frame| frame.stream_id != session.stream_id)
        || drained.iter().any(|append| {
            append.accepting_stream != session.stream_id
                || append.key.resource != session.jid
                || append.disposition != IngressCustodyDisposition::Pending
        })
    {
        return Err(RevisionError::InvalidInput);
    }
    let outstanding = session.outbound_count.wrapping_sub(session.last_acked);
    let gap_distance = replay_gap_distance(session)?;
    let mut seen = HashSet::with_capacity(unacked.len());
    let mut suffix_entries = 0_u32;
    for frame in unacked {
        let distance = frame.sequence.wrapping_sub(session.last_acked);
        if !seen.insert(frame.sequence) || distance <= gap_distance || distance > outstanding {
            return Err(RevisionError::InvalidInput);
        }
        suffix_entries = suffix_entries
            .checked_add(1)
            .ok_or(RevisionError::InvalidInput)?;
    }
    if suffix_entries != outstanding - gap_distance {
        return Err(RevisionError::InvalidInput);
    }
    for append in drained {
        let frame = unacked
            .iter()
            .find(|frame| frame.sequence == append.sequence)
            .ok_or(RevisionError::Unsupported)?;
        if codec::serialize_stanza(&append.payload).map_err(storage)?
            != codec::serialize_stanza(&frame.stanza).map_err(storage)?
            || append.original_receipt_at.timestamp_millis()
                != frame.original_receipt_at.timestamp_millis()
        {
            return Err(RevisionError::InvalidInput);
        }
    }
    Ok(())
}

fn replay_gap_distance(session: &PersistedSession) -> Result<u32, RevisionError> {
    let outstanding = session.outbound_count.wrapping_sub(session.last_acked);
    match session.replay_gap_through {
        Some(gap) => {
            let distance = gap.wrapping_sub(session.last_acked);
            if distance == 0 || distance > outstanding || outstanding >= (1_u32 << 31) {
                return Err(RevisionError::InvalidInput);
            }
            Ok(distance)
        }
        None => Ok(0),
    }
}

/// Snapshot and append do not acknowledge an evicted frame or reopen an
/// expired source. Only the checked ACK transaction can clear a covered gap.
fn validate_session_watermarks(
    current: &PersistedSession,
    proposed: &PersistedSession,
) -> Result<(), RevisionError> {
    if proposed.last_acked != current.last_acked
        || proposed.max_resume_duration > current.max_resume_duration
    {
        return Err(RevisionError::InvalidInput);
    }
    match (current.max_resume_time, proposed.max_resume_time) {
        (None, None) => {}
        (Some(old), Some(new)) if new <= old => {}
        _ => return Err(RevisionError::InvalidInput),
    }
    let old_gap = replay_gap_distance(current)?;
    let new_gap = replay_gap_distance(proposed)?;
    if new_gap < old_gap {
        return Err(RevisionError::InvalidInput);
    }
    Ok(())
}

async fn clear_acknowledged_gap(
    tx: &mut Transaction<'_>,
    current: &PersistedSession,
    h: u32,
) -> Result<(), RevisionError> {
    if current.replay_gap_through.is_none() {
        return Ok(());
    }
    let gap = replay_gap_distance(current)?;
    if h.wrapping_sub(current.last_acked) >= gap {
        let updated = tx
            .execute(
                "UPDATE sm_sessions SET replay_gap_through = NULL WHERE stream_id = ?",
                crate::db_params![current.stream_id.as_str()],
            )
            .await
            .map_err(storage)?;
        if updated != 1 {
            return Err(RevisionError::GenerationChanged);
        }
    }
    Ok(())
}

/// Replacement may add captured suffix entries but cannot erase or rewrite an
/// existing durable frame. ACK is a separate checked transition; tombstone
/// deletion remains unsupported until its terminal fence is available.
fn validate_snapshot_extension(
    current: &Capture,
    proposed: &PersistedSession,
    unacked: &[PersistedUnackedStanza],
) -> Result<(), RevisionError> {
    validate_session_watermarks(&current.session, proposed)?;
    let advanced = proposed
        .outbound_count
        .wrapping_sub(current.session.outbound_count);
    if proposed.last_acked != current.session.last_acked || advanced >= (1_u32 << 31) {
        return Err(RevisionError::InvalidInput);
    }
    let mut proposed_frames = HashMap::with_capacity(unacked.len());
    for frame in unacked {
        if proposed_frames.insert(frame.sequence, frame).is_some() {
            return Err(RevisionError::InvalidInput);
        }
    }
    for old in &current.unacked {
        let new = proposed_frames
            .remove(&old.sequence)
            .ok_or(RevisionError::InvalidInput)?;
        if codec::serialize_stanza(&new.stanza).map_err(storage)?
            != codec::serialize_stanza(&old.stanza).map_err(storage)?
            || new.original_receipt_at.timestamp_millis()
                != old.original_receipt_at.timestamp_millis()
            || new.ingress_receipts != old.ingress_receipts
            || new.provenance != old.provenance
        {
            return Err(RevisionError::InvalidInput);
        }
    }
    if u32::try_from(proposed_frames.len()).map_err(|_| RevisionError::InvalidInput)? != advanced {
        return Err(RevisionError::InvalidInput);
    }
    for sequence in proposed_frames.keys() {
        let distance = sequence.wrapping_sub(current.session.outbound_count);
        if distance == 0 || distance > advanced {
            return Err(RevisionError::InvalidInput);
        }
    }
    Ok(())
}

async fn snapshot(
    tx: &mut Transaction<'_>,
    principal: &AuthenticatedPrincipalRef,
    session: &PersistedSession,
    unacked: &[PersistedUnackedStanza],
    drained: &[PersistedIngressAppend],
) -> Result<StoredEffect, RevisionError> {
    let prepared = PreparedSnapshot::new(Some(principal), session, unacked).map_err(storage)?;
    let ledger = Ledger::Drained(drained.to_vec());
    let withheld = match apply_snapshot_in_transaction(tx, &prepared, &ledger).await {
        Ok(StoreOutcome::Committed { withheld }) => withheld,
        Ok(
            StoreOutcome::ObligationAlreadyResolved
            | StoreOutcome::ObligationAlreadyAllocated { .. },
        ) => {
            return Err(RevisionError::Unsupported);
        }
        Err(SnapshotApplyError::Storage(error)) => return Err(storage(error)),
        Err(SnapshotApplyError::LedgerError { error, .. }) => return Err(storage(error)),
    };
    let withheld = withheld
        .iter()
        .map(|key| {
            drained
                .iter()
                .position(|append| append.key == *key)
                .ok_or(RevisionError::InvalidInput)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(StoredEffect::Snapshot { withheld })
}

async fn append_detached(
    tx: &mut Transaction<'_>,
    revision: &Revision,
    current: &PersistedSession,
    append: &PersistedDetachedAppend,
) -> Result<StoredEffect, RevisionError> {
    append.validate().map_err(storage)?;
    validate_session(revision, &append.session)?;
    validate_session_watermarks(current, &append.session)?;
    if append.new_delivery || !append.evicted_sequences.is_empty() {
        return Err(RevisionError::Unsupported);
    }
    let distance = append.stanza.sequence.wrapping_sub(current.outbound_count);
    if append.session.outbound_count != append.stanza.sequence
        || append.session.last_acked != current.last_acked
        || append.session.replay_gap_through != current.replay_gap_through
        || distance != 1
        || append
            .session
            .outbound_count
            .wrapping_sub(append.session.last_acked)
            >= (1_u32 << 31)
    {
        return Err(RevisionError::InvalidInput);
    }
    if let Some(proof) = &append.ingress_append {
        if proof.key.resource != revision.jid
            || codec::serialize_stanza(&proof.payload).map_err(storage)?
                != codec::serialize_stanza(&append.stanza.stanza).map_err(storage)?
            || proof.original_receipt_at.timestamp_millis()
                != append.stanza.original_receipt_at.timestamp_millis()
        {
            return Err(RevisionError::InvalidInput);
        }
        let withheld = ingress_append::insert_or_withhold(tx, std::slice::from_ref(proof))
            .await
            .map_err(storage)?;
        if !withheld.is_empty() {
            let stream = allocation_stream(tx, &proof.key).await?;
            return Ok(StoredEffect::AlreadyAllocated {
                accepting_stream: stream.as_str().to_owned(),
            });
        }
    }
    let changed = tx.execute(
        "UPDATE sm_sessions SET inbound_count = ?, outbound_count = ?, last_acked = ?, replay_gap_through = ? WHERE stream_id = ?",
        crate::db_params![i64::from(append.session.inbound_count), i64::from(append.session.outbound_count), i64::from(append.session.last_acked), append.session.replay_gap_through.map(i64::from), revision.stream.as_str()],
    ).await.map_err(storage)?;
    if changed != 1 {
        return Err(RevisionError::GenerationChanged);
    }
    tx.execute(
        "INSERT INTO sm_unacked (stream_id, sequence, stanza_xml, original_receipt_at_ms, ingress_receipts, provenance) VALUES (?, ?, ?, ?, ?, ?)",
        crate::db_params![revision.stream.as_str(), i64::from(append.stanza.sequence), codec::serialize_stanza(&append.stanza.stanza).map_err(storage)?, append.stanza.original_receipt_at.timestamp_millis(), codec::encode_ingress_receipts(&append.stanza.ingress_receipts), append.stanza.provenance.encode()],
    ).await.map_err(storage)?;
    for sequence in &append.evicted_sequences {
        tx.execute(
            "DELETE FROM sm_unacked WHERE stream_id = ? AND sequence = ?",
            crate::db_params![revision.stream.as_str(), i64::from(*sequence)],
        )
        .await
        .map_err(storage)?;
    }
    Ok(StoredEffect::Appended)
}

async fn allocation_stream(
    tx: &mut Transaction<'_>,
    key: &SmIngressAppendKey,
) -> Result<SmSessionId, RevisionError> {
    let mut rows = tx.query(
        "SELECT accepting_stream_id FROM sm_ingress_appends WHERE message_key = ? AND receipt_kind = ? AND semantic_identity_hash = ? AND resource = ?",
        crate::db_params![key.message_key.to_storage().to_string(), i64::from(key.kind.to_storage()), key.semantic_identity_hash.to_vec(), key.resource.to_string()],
    ).await.map_err(storage)?;
    let row = rows
        .next()
        .await
        .map_err(storage)?
        .ok_or(RevisionError::InvalidInput)?;
    let stream: String = row.get(0).map_err(storage)?;
    Ok(SmSessionId::new(stream))
}
