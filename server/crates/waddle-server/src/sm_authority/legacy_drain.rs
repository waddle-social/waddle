//! One-time transfer of stream-management sessions that have no durable
//! authority record to offline storage.
//!
//! A session enabled before the durable authority was the only one has a
//! `sm_sessions` row, `sm_unacked` frames, or both, and no
//! `sm_durable_sessions` row. Nothing in a serving process resumes, expires
//! or releases such a session, so the owner drains it once before the first
//! serving process starts: its client cannot resume it and binds afresh, and
//! every retained message reaches that account through offline delivery.
//!
//! Each session is drained in one guarded write transaction:
//!
//! 1. Recorded acknowledgement windows delete the offline rows the client
//!    already acknowledged in that session, as the session's own settlement
//!    would have.
//! 2. Offline rows the session had claimed are released. A retained frame
//!    whose sequence is one of those rows' outbound sequences is that row's
//!    replay copy; the released row is delivered and the copy is not stored
//!    a second time.
//! 3. A retained frame that an earlier promotion already transferred (it has
//!    a `pending_delivery_sm_promotions` receipt) is not stored again.
//! 4. Every other retained frame is classified as an offline-storage
//!    candidate with no resource online. A storable message is inserted into
//!    `pending_delivery` without the per-recipient quota, so a cutover never
//!    rejects a message the session had already accepted. A message the
//!    classifier would not store offline, and every `<iq/>` and `<presence/>`
//!    frame, is dropped, which is what the expiry of an unresumed session
//!    does to them when no other resource of the account is online.
//! 5. The ingress custody of each processed frame is marked promoted, and the
//!    session's rows, receipts and acknowledgement windows are deleted.
//!
//! The transaction commits all of it or none of it, so every frame is stored
//! at most once, and a rerun finds only sessions that are still undrained.

use std::collections::{HashMap, HashSet};

use jid::BareJid;
use thiserror::Error;
use waddle_xmpp::pending_delivery::{InsertOutcome, SmSessionId};
use waddle_xmpp::pending_delivery::{PendingPayload, PendingRow, PendingRowId, QuotaPolicy};
use waddle_xmpp::protocol::dm_routing::{classify_dm_intake, OnlineResources, PendingDecision};
use waddle_xmpp::protocol::session_state::Blocklist;
use waddle_xmpp::stream_management::persistence::{PersistedUnackedStanza, SmPersistenceError};
use waddle_xmpp::Stanza;

use crate::db::blocking::{BlockingStorageError, DatabaseBlockingStorage};
use crate::db::{Database, DatabaseError};
use crate::ingress_substrate::{begin_guarded_store_writer, EnteredTransaction};
use crate::sm_persistence::durable_rows;

/// Sessions listed per scan. Each listed session is drained before the next
/// scan, so the scan always starts from the remaining sessions.
const SCAN_LIMIT: i64 = 64;
const LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const STATEMENT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// What one drain committed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LegacyDrainReport {
    /// Sessions whose rows were removed.
    pub sessions: u64,
    /// Retained messages inserted into offline storage.
    pub stored: u64,
    /// Claimed offline rows released for ordinary offline delivery.
    pub released: u64,
    /// Claimed offline rows deleted because the client acknowledged them.
    pub acknowledged: u64,
    /// Retained frames that offline storage does not hold.
    pub dropped: u64,
}

#[derive(Debug, Error)]
pub enum LegacyDrainError {
    #[error(transparent)]
    Database(#[from] DatabaseError),
    #[error("a legacy stream-management row could not be decoded")]
    Rows(#[from] SmPersistenceError),
    #[error("the blocklist of {recipient} could not be read")]
    Blocklist {
        recipient: BareJid,
        #[source]
        source: BlockingStorageError,
    },
    #[error("the drain transaction for {stream} could not be opened")]
    Begin {
        stream: String,
        #[source]
        source: Box<super::DurableSmError>,
    },
    #[error("an offline row for {stream} could not be inserted")]
    Offline {
        stream: String,
        #[source]
        source: waddle_xmpp::pending_delivery::storage::PendingStorageError,
    },
    /// Offline storage refused a row although the drain inserts without a
    /// quota. Nothing of the session was committed.
    #[error("offline storage refused a retained message of {stream}")]
    OfflineRefused { stream: String },
    /// The session's retained frames changed between the blocklist read and
    /// the drain transaction. Nothing of the session was committed.
    #[error("the retained frames of {stream} changed during the drain")]
    Changed { stream: String },
    /// A drained session was still listed afterwards.
    #[error("the legacy session {stream} was not removed")]
    NotRemoved { stream: String },
}

/// The offline-storage outcome of one retained frame.
#[derive(Debug, Clone)]
pub(crate) enum FrameDisposition {
    Store {
        recipient: BareJid,
        payload: PendingPayload,
    },
    Drop,
}

/// Drain every session without a durable authority record. Safe to run
/// again: a drained session leaves no rows behind to find.
pub async fn drain_legacy_sm_sessions(
    db: &Database,
) -> Result<LegacyDrainReport, LegacyDrainError> {
    let mut report = LegacyDrainReport::default();
    let mut drained = HashSet::new();
    loop {
        let streams = legacy_streams(db).await?;
        if streams.is_empty() {
            return Ok(report);
        }
        for stream in streams {
            if !drained.insert(stream.clone()) {
                return Err(LegacyDrainError::NotRemoved { stream });
            }
            drain_one(db, &SmSessionId::new(stream), &mut report).await?;
        }
    }
}

async fn legacy_streams(db: &Database) -> Result<Vec<String>, LegacyDrainError> {
    let mut tx = db.begin().await?;
    let mut rows = tx
        .query(
            "SELECT l.stream_id FROM \
                 (SELECT stream_id FROM sm_sessions UNION SELECT stream_id FROM sm_unacked) l \
             WHERE NOT EXISTS \
                 (SELECT 1 FROM sm_durable_sessions d WHERE d.session_id = l.stream_id) \
             ORDER BY l.stream_id LIMIT ?",
            crate::db_params![SCAN_LIMIT],
        )
        .await?;
    let mut streams = Vec::new();
    while let Some(row) = rows.next().await? {
        streams.push(row.get::<String>(0)?);
    }
    drop(rows);
    tx.rollback().await?;
    Ok(streams)
}

async fn drain_one(
    db: &Database,
    stream: &SmSessionId,
    report: &mut LegacyDrainReport,
) -> Result<(), LegacyDrainError> {
    // The blocklists are read on their own connections before the write
    // transaction, which on SQLite holds the only writer.
    let preview = {
        let mut tx = db.begin().await?;
        let frames = durable_rows::load_unacked(&mut tx, stream).await?;
        tx.rollback().await?;
        frames
    };
    let blocking = DatabaseBlockingStorage::new(db.clone());
    let mut blocklists = HashMap::new();
    for recipient in preview.iter().filter_map(frame_recipient) {
        if blocklists.contains_key(&recipient) {
            continue;
        }
        let entries = blocking
            .list_blocked_jid_entries(&recipient)
            .await
            .map_err(|source| LegacyDrainError::Blocklist {
                recipient: recipient.clone(),
                source,
            })?;
        blocklists.insert(recipient, Blocklist::new(entries));
    }

    let mut tx = begin_guarded_store_writer(db, LOCK_TIMEOUT, STATEMENT_TIMEOUT)
        .await
        .map_err(|source| LegacyDrainError::Begin {
            stream: stream.as_str().to_owned(),
            source: Box::new(source.into()),
        })?;
    if has_durable_record(&mut tx, stream).await? {
        // The authority adopted the stream id after it was listed; its rows
        // are the authority's.
        tx.rollback().await?;
        return Ok(());
    }
    let mut frames = durable_rows::load_unacked(&mut tx, stream).await?;
    if frames
        .iter()
        .filter_map(frame_recipient)
        .any(|recipient| !blocklists.contains_key(&recipient))
    {
        tx.rollback().await?;
        return Err(LegacyDrainError::Changed {
            stream: stream.as_str().to_owned(),
        });
    }
    // Stored sequences wrap; receipt time is the order the frames were
    // accepted in, and the sequence breaks ties.
    frames.sort_by_key(|frame| (frame.original_receipt_at, frame.sequence));

    let mut committed = LegacyDrainReport {
        sessions: 1,
        ..LegacyDrainReport::default()
    };
    committed.acknowledged = settle_acknowledged(&mut tx, stream).await?;
    let replay_copies = claimed_sequences(&mut tx, stream).await?;
    committed.released = release_claims(&mut tx, stream).await?;
    let transferred = promotion_receipts(&mut tx, stream).await?;

    for frame in &frames {
        if replay_copies.contains(&frame.sequence) || transferred.contains(&frame.sequence) {
            continue;
        }
        match frame_disposition(frame.stanza.wire(), |recipient| blocklists.get(recipient)) {
            FrameDisposition::Store { recipient, payload } => {
                let row = PendingRow {
                    id: PendingRowId::fresh(),
                    recipient,
                    original_receipt_at: frame.original_receipt_at,
                    payload,
                    flushed_in_session: None,
                    outbound_sequence: None,
                };
                let outcome = crate::pending_delivery::database::insert_in_transaction(
                    &mut tx,
                    &row,
                    QuotaPolicy::Unlimited,
                    None,
                )
                .await
                .map_err(|source| LegacyDrainError::Offline {
                    stream: stream.as_str().to_owned(),
                    source,
                })?;
                if outcome != InsertOutcome::Inserted {
                    return Err(LegacyDrainError::OfflineRefused {
                        stream: stream.as_str().to_owned(),
                    });
                }
                committed.stored += 1;
            }
            FrameDisposition::Drop => committed.dropped += 1,
        }
    }

    let stream_id = stream.as_str().to_owned();
    // Custody that this drain discharged; custody of a frame the session
    // no longer retains stays pending for the runtime's custody recovery.
    for frame in &frames {
        tx.execute(
            "UPDATE sm_ingress_appends SET disposition = 2 \
             WHERE accepting_stream_id = ? AND sequence = ? AND disposition = 0",
            crate::db_params![stream_id.clone(), i64::from(frame.sequence)],
        )
        .await?;
    }
    for statement in [
        "DELETE FROM sm_unacked WHERE stream_id = ?",
        "DELETE FROM sm_sessions WHERE stream_id = ?",
        "DELETE FROM sm_acknowledged_windows WHERE stream_id = ?",
        "DELETE FROM pending_delivery_sm_promotions WHERE origin_stream_id = ?",
    ] {
        tx.execute(statement, crate::db_params![stream_id.clone()])
            .await?;
    }
    tx.commit().await?;

    report.sessions += committed.sessions;
    report.stored += committed.stored;
    report.released += committed.released;
    report.acknowledged += committed.acknowledged;
    report.dropped += committed.dropped;
    Ok(())
}

async fn has_durable_record(
    tx: &mut EnteredTransaction<'_>,
    stream: &SmSessionId,
) -> Result<bool, DatabaseError> {
    let mut rows = tx
        .query(
            "SELECT 1 FROM sm_durable_sessions WHERE session_id = ?",
            crate::db_params![stream.as_str().to_owned()],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

/// Delete the claimed offline rows inside each acknowledgement window the
/// session recorded, and return how many were deleted.
async fn settle_acknowledged(
    tx: &mut EnteredTransaction<'_>,
    stream: &SmSessionId,
) -> Result<u64, DatabaseError> {
    let mut windows = Vec::new();
    {
        let mut rows = tx
            .query(
                "SELECT from_exclusive, to_inclusive FROM sm_acknowledged_windows \
                 WHERE stream_id = ?",
                crate::db_params![stream.as_str().to_owned()],
            )
            .await?;
        while let Some(row) = rows.next().await? {
            windows.push((row.get::<i64>(0)?, row.get::<i64>(1)?));
        }
    }
    let mut removed = 0;
    for (from_exclusive, to_inclusive) in windows {
        let sql = if from_exclusive <= to_inclusive {
            "DELETE FROM pending_delivery WHERE flushed_in_session = ? \
             AND outbound_sequence IS NOT NULL \
             AND outbound_sequence > ? AND outbound_sequence <= ?"
        } else {
            // The window crosses the counter wrap.
            "DELETE FROM pending_delivery WHERE flushed_in_session = ? \
             AND outbound_sequence IS NOT NULL \
             AND (outbound_sequence > ? OR outbound_sequence <= ?)"
        };
        removed += tx
            .execute(
                sql,
                crate::db_params![stream.as_str().to_owned(), from_exclusive, to_inclusive],
            )
            .await?;
    }
    Ok(removed)
}

/// Outbound sequences of the offline rows the session claimed: the retained
/// frames at these sequences are replay copies of those rows.
async fn claimed_sequences(
    tx: &mut EnteredTransaction<'_>,
    stream: &SmSessionId,
) -> Result<HashSet<u32>, DatabaseError> {
    let mut rows = tx
        .query(
            "SELECT outbound_sequence FROM pending_delivery \
             WHERE flushed_in_session = ? AND outbound_sequence IS NOT NULL",
            crate::db_params![stream.as_str().to_owned()],
        )
        .await?;
    let mut sequences = HashSet::new();
    while let Some(row) = rows.next().await? {
        if let Ok(sequence) = u32::try_from(row.get::<i64>(0)?) {
            sequences.insert(sequence);
        }
    }
    Ok(sequences)
}

async fn release_claims(
    tx: &mut EnteredTransaction<'_>,
    stream: &SmSessionId,
) -> Result<u64, DatabaseError> {
    tx.execute(
        "UPDATE pending_delivery SET flushed_in_session = NULL, outbound_sequence = NULL, \
                                     claimed_at_ms = NULL, claim_token = NULL, claim_offered = 0 \
         WHERE flushed_in_session = ?",
        crate::db_params![stream.as_str().to_owned()],
    )
    .await
}

async fn promotion_receipts(
    tx: &mut EnteredTransaction<'_>,
    stream: &SmSessionId,
) -> Result<HashSet<u32>, DatabaseError> {
    let mut rows = tx
        .query(
            "SELECT sequence FROM pending_delivery_sm_promotions WHERE origin_stream_id = ?",
            crate::db_params![stream.as_str().to_owned()],
        )
        .await?;
    let mut sequences = HashSet::new();
    while let Some(row) = rows.next().await? {
        if let Ok(sequence) = u32::try_from(row.get::<i64>(0)?) {
            sequences.insert(sequence);
        }
    }
    Ok(sequences)
}

/// The bare recipient of a retained message, whose blocklist classifies it.
fn frame_recipient(frame: &PersistedUnackedStanza) -> Option<BareJid> {
    match crate::sm_promotion::parse_stanza(frame.stanza.wire()) {
        Some(Stanza::Message(message)) => message.to.map(|to| to.to_bare()),
        _ => None,
    }
}

/// Classify one retained frame for offline storage with no resource of its
/// recipient online. `blocklist_of` returns the recipient's blocklist; a
/// message whose recipient has none listed is not stored.
pub(crate) fn frame_disposition<'b>(
    wire: &str,
    blocklist_of: impl Fn(&BareJid) -> Option<&'b Blocklist>,
) -> FrameDisposition {
    let Some(Stanza::Message(message)) = crate::sm_promotion::parse_stanza(wire) else {
        return FrameDisposition::Drop;
    };
    if waddle_xmpp_core::mam::is_mam_query_response_message(&message)
        || crate::sm_promotion::is_senderless_inbox_projection(&message)
    {
        return FrameDisposition::Drop;
    }
    let Some(recipient) = message.to.as_ref().map(|to| to.to_bare()) else {
        return FrameDisposition::Drop;
    };
    let Some(blocklist) = blocklist_of(&recipient) else {
        return FrameDisposition::Drop;
    };
    let routing = classify_dm_intake(&message, &OnlineResources::empty(), blocklist);
    let payload = match routing.pending {
        PendingDecision::None => return FrameDisposition::Drop,
        PendingDecision::Archived => {
            let by = jid::Jid::from(recipient.clone());
            match waddle_xmpp_core::xep0359::extract_stanza_id_by(&message, &by) {
                Some(id) => {
                    PendingPayload::Archived(waddle_xmpp_core::xep0359::StanzaId::new(id, by))
                }
                // Without the recipient's archive id the message is kept
                // inline, so it is not lost.
                None => PendingPayload::Transient(Box::new(message)),
            }
        }
        PendingDecision::Transient => PendingPayload::Transient(Box::new(message)),
    };
    FrameDisposition::Store { recipient, payload }
}

#[cfg(test)]
mod tests;
