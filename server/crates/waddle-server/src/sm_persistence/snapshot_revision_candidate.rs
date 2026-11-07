//! Dark snapshot-only revision authority. No production writer selects it.
//!
//! A capture reads the durable snapshot and its revision under the same route
//! and stream locks. Only this module's snapshots and retirement advance that
//! authority. Legacy, append, ACK, claim and other writers do not participate:
//! their writes can therefore invalidate a capture without advancing it. A
//! database capture cannot certify a process-local snapshot captured earlier,
//! and neither a principal reference nor this generation verifies a live claim
//! or binding. This is not a serving fence or a complete session lifecycle.

use sha2::{Digest, Sha256};
use uuid::Uuid;
use waddle_xmpp::auth::AuthenticatedPrincipalRef;
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::persistence::{PersistedSession, PersistedUnackedStanza};

use super::recipient_route_interlock_candidate::{
    lock_route_set, lock_stream_gate, read_snapshot_inventory, storage, validated_schema,
    CandidateRouteError, CandidateSnapshotLedger, CandidateSnapshotOutcome,
};
use crate::db::{DatabaseDriver, Transaction};

/// Owner-installed only in a disposable candidate schema. Neither table has a
/// foreign key to `sm_sessions`: deletion must not erase authority or receipts.
pub const CANDIDATE_DDL: &str = r#"
CREATE TABLE __SCHEMA__.sm_snapshot_candidate_generations (
    generation UUID PRIMARY KEY,
    stream_id TEXT NOT NULL,
    occupancy_session TEXT NOT NULL,
    UNIQUE (stream_id, generation),
    UNIQUE (stream_id, occupancy_session)
);
CREATE TABLE __SCHEMA__.sm_snapshot_candidate_authority (
    stream_id TEXT PRIMARY KEY,
    generation UUID NOT NULL,
    occupancy_session TEXT NOT NULL,
    full_jid TEXT NOT NULL,
    revision BIGINT NOT NULL CHECK (revision >= 0),
    terminal BOOLEAN NOT NULL,
    FOREIGN KEY (stream_id, generation)
      REFERENCES __SCHEMA__.sm_snapshot_candidate_generations (stream_id, generation)
);
CREATE TABLE __SCHEMA__.sm_snapshot_candidate_operations (
    operation_id UUID PRIMARY KEY,
    stream_id TEXT NOT NULL,
    generation UUID NOT NULL,
    expected_revision BIGINT NOT NULL CHECK (expected_revision >= 0),
    input_digest BYTEA NOT NULL CHECK (pg_catalog.octet_length(input_digest) = 32),
    FOREIGN KEY (stream_id, generation)
      REFERENCES __SCHEMA__.sm_snapshot_candidate_generations (stream_id, generation)
);
"#;

/// An opaque revision bound to the durable session state returned by capture.
#[derive(Clone, Debug)]
pub struct Revision {
    stream: SmSessionId,
    generation: Uuid,
    revision: i64,
    principal: Option<AuthenticatedPrincipalRef>,
}

/// Input state read together with the revision. A caller must derive its new
/// snapshot from this capture, and commit the capture transaction before use.
/// The token must never be combined with an older process-local snapshot.
pub struct Capture {
    pub revision: Revision,
    pub principal: Option<AuthenticatedPrincipalRef>,
    pub session: PersistedSession,
    pub unacked: Vec<PersistedUnackedStanza>,
}

/// Whether a terminal stream may be opened as an explicitly new generation.
#[derive(Clone, Copy)]
pub enum CaptureMode {
    Current,
    ReopenTerminal,
}

/// Capture actual portable rows and revision while holding the ordered locks.
/// Reopening requires a distinct persisted occupancy generation; every prior
/// generation remains recorded even after the session row is deleted.
pub async fn capture<'a>(
    mut tx: Transaction<'a>,
    schema: &str,
    stream: &SmSessionId,
    mode: CaptureMode,
) -> Result<(Transaction<'a>, Capture), CandidateRouteError> {
    match capture_in_transaction(&mut tx, schema, stream, mode).await {
        Ok(capture) => Ok((tx, capture)),
        Err(error) => {
            tx.rollback().await.map_err(storage)?;
            Err(error)
        }
    }
}

async fn capture_in_transaction(
    tx: &mut Transaction<'_>,
    schema: &str,
    stream: &SmSessionId,
    mode: CaptureMode,
) -> Result<Capture, CandidateRouteError> {
    let old = read_snapshot_inventory(tx, schema, stream, false).await?;
    let (bare, _, _) = old.session.as_ref().ok_or(CandidateRouteError::Identity)?;
    let recipient = bare.parse().map_err(|_| CandidateRouteError::Identity)?;
    if !lock_route_set(tx, schema, &old.routes()?, &[recipient]).await? {
        return Err(CandidateRouteError::Closed);
    }
    lock_stream_gate(tx, stream).await?;
    let current = read_snapshot_inventory(tx, schema, stream, true).await?;
    if current != old {
        return Err(CandidateRouteError::Retry);
    }
    let schema = validated_schema(schema)?;
    let mut rows = tx.query(
        &format!("SELECT stream_id, user_id, full_jid, occupancy_session, inbound_count, outbound_count, last_acked, max_resume_secs, detached_at_ms, max_resume_duration_ms, carbons_enabled, roster_interested, blocklist_interested, presence_available, presence_show, presence_status, presence_priority, replay_gap_through, presence_payloads FROM {schema}.sm_sessions WHERE stream_id = ?"),
        crate::db_params![stream.as_str()],
    ).await.map_err(storage)?;
    let row = rows
        .next()
        .await
        .map_err(storage)?
        .ok_or(CandidateRouteError::Identity)?;
    let occupancy: Option<String> = row.get(3).map_err(storage)?;
    let occupancy = occupancy.ok_or(CandidateRouteError::GenerationChanged)?;
    let session = super::codec::decode_session(&row).map_err(storage)?;
    let mut rows = tx.query(
        &format!("SELECT bare_jid, auth_context_id, auth_context_version, principal_auth_epoch FROM {schema}.sm_sessions WHERE stream_id = ?"),
        crate::db_params![stream.as_str()],
    ).await.map_err(storage)?;
    let row = rows
        .next()
        .await
        .map_err(storage)?
        .ok_or(CandidateRouteError::Identity)?;
    let principal = super::codec::decode_session_principal(&row).map_err(storage)?;
    let projection =
        super::codec::bounded_provenance_projection(DatabaseDriver::Postgres, "provenance");
    let mut rows = tx.query(
        &format!("SELECT stream_id, sequence, stanza_xml, original_receipt_at_ms, ingress_receipts, {projection} FROM {schema}.sm_unacked WHERE stream_id = ? ORDER BY sequence"),
        crate::db_params![stream.as_str()],
    ).await.map_err(storage)?;
    let mut unacked = Vec::new();
    while let Some(row) = rows.next().await.map_err(storage)? {
        unacked.push(super::codec::decode_unacked(&row).map_err(storage)?);
    }
    let mut rows = tx.query(
        &format!("SELECT generation::text, occupancy_session, full_jid, revision, terminal FROM {schema}.sm_snapshot_candidate_authority WHERE stream_id = ? FOR UPDATE"),
        crate::db_params![stream.as_str()],
    ).await.map_err(storage)?;
    let authority = rows.next().await.map_err(storage)?;
    let (generation, revision) = if let Some(row) = authority {
        let generation: String = row.get(0).map_err(storage)?;
        let saved_occupancy: String = row.get(1).map_err(storage)?;
        let full_jid: String = row.get(2).map_err(storage)?;
        let revision: i64 = row.get(3).map_err(storage)?;
        let terminal: bool = row.get(4).map_err(storage)?;
        if terminal {
            if !matches!(mode, CaptureMode::ReopenTerminal) {
                return Err(CandidateRouteError::Terminal);
            }
            let generation = insert_generation(tx, &schema, stream, &occupancy).await?;
            tx.execute(
                &format!("UPDATE {schema}.sm_snapshot_candidate_authority SET generation = ?::uuid, occupancy_session = ?, full_jid = ?, revision = 0, terminal = FALSE WHERE stream_id = ?"),
                crate::db_params![generation.to_string(), occupancy, session.jid.to_string(), stream.as_str()],
            ).await.map_err(storage)?;
            (generation, 0)
        } else {
            if matches!(mode, CaptureMode::ReopenTerminal)
                || saved_occupancy != occupancy
                || full_jid != session.jid.to_string()
            {
                return Err(CandidateRouteError::GenerationChanged);
            }
            (generation.parse().map_err(storage)?, revision)
        }
    } else {
        if matches!(mode, CaptureMode::ReopenTerminal) {
            return Err(CandidateRouteError::GenerationChanged);
        }
        let generation = insert_generation(tx, &schema, stream, &occupancy).await?;
        tx.execute(
            &format!("INSERT INTO {schema}.sm_snapshot_candidate_authority (stream_id, generation, occupancy_session, full_jid, revision, terminal) VALUES (?, ?::uuid, ?, ?, 0, FALSE)"),
            crate::db_params![stream.as_str(), generation.to_string(), occupancy, session.jid.to_string()],
        ).await.map_err(storage)?;
        (generation, 0)
    };
    Ok(Capture {
        revision: Revision {
            stream: stream.clone(),
            generation,
            revision,
            principal: principal.clone(),
        },
        principal,
        session,
        unacked,
    })
}

async fn insert_generation(
    tx: &mut Transaction<'_>,
    schema: &str,
    stream: &SmSessionId,
    occupancy: &str,
) -> Result<Uuid, CandidateRouteError> {
    let generation = Uuid::new_v4();
    let inserted = tx.execute(
        &format!("INSERT INTO {schema}.sm_snapshot_candidate_generations (generation, stream_id, occupancy_session) VALUES (?::uuid, ?, ?) ON CONFLICT DO NOTHING"),
        crate::db_params![generation.to_string(), stream.as_str(), occupancy],
    ).await.map_err(storage)?;
    if inserted != 1 {
        return Err(CandidateRouteError::GenerationChanged);
    }
    Ok(generation)
}

pub(super) struct Operation {
    id: Uuid,
    revision: Revision,
    digest: Vec<u8>,
}

/// Positive-only operation lookup. `Unconfirmed` does not authorize a blind
/// retry after an uncertain commit; retrying this API still performs the CAS.
#[derive(Debug, PartialEq, Eq)]
pub enum Readback {
    Applied,
    Unconfirmed,
}

/// One caller snapshot and its separately captured revision. The caller must
/// derive the snapshot from its capture. Under the stream gate, the store
/// checks revision, generation, and principal, but cannot prove derivation.
pub struct SnapshotInput<'a> {
    pub operation_id: Uuid,
    pub revision: &'a Revision,
    pub principal: Option<&'a AuthenticatedPrincipalRef>,
    pub session: &'a PersistedSession,
    pub unacked: &'a [PersistedUnackedStanza],
}

/// Verify the immutable operation and its complete input on a fresh transaction.
pub async fn readback(
    tx: &mut Transaction<'_>,
    schema: &str,
    input: SnapshotInput<'_>,
    ledger: &CandidateSnapshotLedger,
) -> Result<Readback, CandidateRouteError> {
    let op = operation(
        input.operation_id,
        input.revision,
        input.principal,
        input.session,
        input.unacked,
        ledger,
    )?;
    Ok(if operation_applied(tx, schema, &op).await? {
        Readback::Applied
    } else {
        Readback::Unconfirmed
    })
}

/// Apply real portable snapshot SQL, frame ownership, revision advance, and
/// operation receipt in one transaction. Only the two modes whose result can
/// be represented by this receipt are admitted in this bounded candidate.
pub async fn store<'a>(
    mut tx: Transaction<'a>,
    schema: &str,
    input: SnapshotInput<'_>,
    ledger: CandidateSnapshotLedger,
) -> Result<(Transaction<'a>, CandidateSnapshotOutcome), CandidateRouteError> {
    let result = async {
        let op = operation(
            input.operation_id,
            input.revision,
            input.principal,
            input.session,
            input.unacked,
            &ledger,
        )?;
        if operation_applied(&mut tx, schema, &op).await? {
            return Ok(CandidateSnapshotOutcome::AlreadyApplied);
        }
        super::recipient_route_interlock_candidate::store_snapshot_in_transaction(
            &mut tx,
            schema,
            input.principal,
            input.session,
            input.unacked,
            ledger,
            Some(&op),
        )
        .await
    }
    .await;
    match result {
        Ok(outcome) => Ok((tx, outcome)),
        Err(error) => {
            tx.rollback().await.map_err(storage)?;
            Err(error)
        }
    }
}

pub(super) async fn operation_applied(
    tx: &mut Transaction<'_>,
    schema: &str,
    op: &Operation,
) -> Result<bool, CandidateRouteError> {
    if tx.driver() != DatabaseDriver::Postgres {
        return Err(CandidateRouteError::PostgresRequired);
    }
    let schema = validated_schema(schema)?;
    let mut rows = tx.query(
        &format!("SELECT stream_id, generation::text, expected_revision, input_digest FROM {schema}.sm_snapshot_candidate_operations WHERE operation_id = ?::uuid"),
        crate::db_params![op.id.to_string()],
    ).await.map_err(storage)?;
    let Some(row) = rows.next().await.map_err(storage)? else {
        return Ok(false);
    };
    let stream: String = row.get(0).map_err(storage)?;
    let generation: String = row.get(1).map_err(storage)?;
    let revision: i64 = row.get(2).map_err(storage)?;
    let digest: Vec<u8> = row.get(3).map_err(storage)?;
    if stream != op.revision.stream.as_str()
        || generation != op.revision.generation.to_string()
        || revision != op.revision.revision
        || digest != op.digest
    {
        return Err(CandidateRouteError::OperationIdentity);
    }
    Ok(true)
}

pub(super) async fn check_revision(
    tx: &mut Transaction<'_>,
    schema: &str,
    session: &PersistedSession,
    op: &Operation,
) -> Result<(), CandidateRouteError> {
    let schema = validated_schema(schema)?;
    let mut rows = tx.query(
        &format!("SELECT a.generation::text, a.occupancy_session, a.full_jid, a.revision, a.terminal, s.occupancy_session, s.full_jid FROM {schema}.sm_snapshot_candidate_authority a LEFT JOIN {schema}.sm_sessions s ON s.stream_id = a.stream_id WHERE a.stream_id = ? FOR UPDATE OF a"),
        crate::db_params![op.revision.stream.as_str()],
    ).await.map_err(storage)?;
    let row = rows
        .next()
        .await
        .map_err(storage)?
        .ok_or(CandidateRouteError::GenerationChanged)?;
    let generation: String = row.get(0).map_err(storage)?;
    let occupancy: String = row.get(1).map_err(storage)?;
    let full: String = row.get(2).map_err(storage)?;
    let revision: i64 = row.get(3).map_err(storage)?;
    let terminal: bool = row.get(4).map_err(storage)?;
    let current_occupancy: Option<String> = row.get(5).map_err(storage)?;
    let current_full: Option<String> = row.get(6).map_err(storage)?;
    if terminal {
        return Err(CandidateRouteError::Terminal);
    }
    if generation != op.revision.generation.to_string()
        || occupancy != session.occupancy_session.to_string()
        || full != session.jid.to_string()
        || current_occupancy.as_deref() != Some(occupancy.as_str())
        || current_full.as_deref() != Some(full.as_str())
    {
        return Err(CandidateRouteError::GenerationChanged);
    }
    if revision != op.revision.revision {
        return Err(CandidateRouteError::StaleRevision);
    }
    if revision == i64::MAX {
        return Err(CandidateRouteError::RevisionExhausted);
    }
    let mut rows = tx.query(
        &format!("SELECT bare_jid, auth_context_id, auth_context_version, principal_auth_epoch FROM {schema}.sm_sessions WHERE stream_id = ?"),
        crate::db_params![op.revision.stream.as_str()],
    ).await.map_err(storage)?;
    let row = rows
        .next()
        .await
        .map_err(storage)?
        .ok_or(CandidateRouteError::GenerationChanged)?;
    let current_principal = super::codec::decode_session_principal(&row).map_err(storage)?;
    if current_principal != op.revision.principal {
        return Err(CandidateRouteError::GenerationChanged);
    }
    Ok(())
}

pub(super) async fn record_operation(
    tx: &mut Transaction<'_>,
    schema: &str,
    op: &Operation,
) -> Result<(), CandidateRouteError> {
    let schema = validated_schema(schema)?;
    let changed = tx.execute(
        &format!("UPDATE {schema}.sm_snapshot_candidate_authority SET revision = revision + 1 WHERE stream_id = ? AND generation = ?::uuid AND revision = ? AND NOT terminal"),
        crate::db_params![op.revision.stream.as_str(), op.revision.generation.to_string(), op.revision.revision],
    ).await.map_err(storage)?;
    if changed != 1 {
        return Err(CandidateRouteError::StaleRevision);
    }
    tx.execute(
        &format!("INSERT INTO {schema}.sm_snapshot_candidate_operations (operation_id, stream_id, generation, expected_revision, input_digest) VALUES (?::uuid, ?, ?::uuid, ?, ?)"),
        crate::db_params![op.id.to_string(), op.revision.stream.as_str(), op.revision.generation.to_string(), op.revision.revision, op.digest.clone()],
    ).await.map_err(storage)?;
    Ok(())
}

fn operation(
    id: Uuid,
    revision: &Revision,
    principal: Option<&AuthenticatedPrincipalRef>,
    session: &PersistedSession,
    unacked: &[PersistedUnackedStanza],
    ledger: &CandidateSnapshotLedger,
) -> Result<Operation, CandidateRouteError> {
    if id.is_nil() {
        return Err(CandidateRouteError::OperationIdentity);
    }
    if revision.stream != session.stream_id {
        return Err(CandidateRouteError::Identity);
    }
    if principal != revision.principal.as_ref() {
        return Err(CandidateRouteError::GenerationChanged);
    }
    let principal = principal.map(|p| {
        serde_json::json!([
            p.bare_jid().to_string(),
            p.auth_context_id().as_uuid().to_string(),
            p.auth_context_version().get(),
            p.auth_epoch().get()
        ])
    });
    let session_value = serde_json::json!([
        session.stream_id.as_str(),
        session.user_id,
        session.jid.to_string(),
        session.occupancy_session.to_string(),
        session.inbound_count,
        session.outbound_count,
        session.last_acked,
        session.replay_gap_through,
        session.max_resume_time,
        session.detached_at.timestamp(),
        session.detached_at.timestamp_subsec_nanos(),
        session.max_resume_duration.as_secs(),
        session.max_resume_duration.subsec_nanos(),
        session.carbons_enabled,
        session.roster_interested,
        session.blocklist_interested,
        session.presence_available,
        session
            .presence_show
            .as_ref()
            .map(super::codec::show_wire_str),
        session.presence_status,
        session.presence_priority,
        super::codec::serialize_presence_payloads(&session.presence_payloads).map_err(storage)?
    ]);
    let frames: Vec<_> = unacked
        .iter()
        .map(|frame| {
            Ok(serde_json::json!([
                frame.stream_id.as_str(),
                frame.sequence,
                frame.stanza.wire(),
                frame.original_receipt_at.timestamp(),
                frame.original_receipt_at.timestamp_subsec_nanos(),
                super::codec::encode_ingress_receipts(&frame.ingress_receipts),
                frame.provenance.encode()
            ]))
        })
        .collect::<Result<_, CandidateRouteError>>()?;
    let ledger = match ledger {
        CandidateSnapshotLedger::Untouched => serde_json::json!(["untouched"]),
        CandidateSnapshotLedger::Exclusive(append) => serde_json::json!([
            "exclusive", append.key.message_key.to_storage().to_string(), append.key.kind.to_storage(),
            append.key.semantic_identity_hash, append.key.resource.to_string(), append.accepting_stream.as_str(),
            append.sequence, append.appended_at.timestamp(), append.appended_at.timestamp_subsec_nanos(),
            super::codec::serialize_stanza(&append.payload).map_err(storage)?,
            append.original_receipt_at.timestamp(), append.original_receipt_at.timestamp_subsec_nanos(),
            match append.disposition {
                waddle_xmpp::stream_management::persistence::IngressCustodyDisposition::Pending => 0,
                waddle_xmpp::stream_management::persistence::IngressCustodyDisposition::Acknowledged => 1,
                waddle_xmpp::stream_management::persistence::IngressCustodyDisposition::Promoted => 2,
                waddle_xmpp::stream_management::persistence::IngressCustodyDisposition::Tombstoned => 3,
            }
        ]),
        CandidateSnapshotLedger::NewDelivery(_) | CandidateSnapshotLedger::Drained(_) => return Err(CandidateRouteError::UnsupportedLedger),
    };
    let encoded = serde_json::to_vec(&serde_json::json!([
        "waddle-dark-snapshot-input-v1",
        principal,
        session_value,
        frames,
        ledger
    ]))
    .map_err(storage)?;
    Ok(Operation {
        id,
        revision: revision.clone(),
        digest: Sha256::digest(encoded).to_vec(),
    })
}

/// Retire a participating generation and delete its portable session and queue
/// atomically. Other deletion/claim/ACK/append paths do not use this function.
pub async fn retire<'a>(
    mut tx: Transaction<'a>,
    schema: &str,
    captured: &Capture,
) -> Result<Transaction<'a>, CandidateRouteError> {
    let result = async {
        let old = read_snapshot_inventory(&mut tx, schema, &captured.revision.stream, false).await?;
        let (bare, _, _) = old.session.as_ref().ok_or(CandidateRouteError::GenerationChanged)?;
        let recipient = bare.parse().map_err(|_| CandidateRouteError::Identity)?;
        if !lock_route_set(&mut tx, schema, &old.routes()?, &[recipient]).await? { return Err(CandidateRouteError::Closed); }
        lock_stream_gate(&mut tx, &captured.revision.stream).await?;
        let op = Operation { id: Uuid::nil(), revision: captured.revision.clone(), digest: Vec::new() };
        check_revision(&mut tx, schema, &captured.session, &op).await?;
        let current = read_snapshot_inventory(&mut tx, schema, &captured.revision.stream, true).await?;
        if current != old { return Err(CandidateRouteError::Retry); }
        let schema = validated_schema(schema)?;
        tx.execute(&format!("DELETE FROM {schema}.sm_unacked WHERE stream_id = ?"), crate::db_params![captured.revision.stream.as_str()]).await.map_err(storage)?;
        tx.execute(&format!("DELETE FROM {schema}.sm_sessions WHERE stream_id = ?"), crate::db_params![captured.revision.stream.as_str()]).await.map_err(storage)?;
        tx.execute(&format!("UPDATE {schema}.sm_snapshot_candidate_authority SET terminal = TRUE, revision = revision + 1 WHERE stream_id = ?"), crate::db_params![captured.revision.stream.as_str()]).await.map_err(storage)?;
        Ok(())
    }.await;
    match result {
        Ok(()) => Ok(tx),
        Err(error) => {
            tx.rollback().await.map_err(storage)?;
            Err(error)
        }
    }
}
