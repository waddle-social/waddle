//! Dark #2244 seam: one portable SM append and pending acceptance use the
//! same durable canonical-source -> source-guard -> recipient-guard order.
//!
//! This is deliberately not wired into normal writers or startup. It proves a
//! transaction-local composition point only; batch, detached, relay, drain,
//! ACK, transfer, tombstone, and GC paths remain outside this interlock.
//! The quiescence probe is conservative for frames without a candidate owner
//! ledger row, but it is not a complete inventory of process-local or legacy
//! producers. This candidate table is neither selected nor role-attested.

use jid::BareJid;
use thiserror::Error;
use uuid::Uuid;
use waddle_xmpp::pending_delivery::{InsertOutcome, PendingRow, QuotaPolicy};
use waddle_xmpp::stream_management::persistence::PersistedUnackedStanza;
use waddle_xmpp::Stanza;

use crate::db::{DatabaseDriver, Rows, Transaction};
use crate::pending_delivery::database;

/// Owner-installed only in a disposable candidate schema.
pub const CANDIDATE_DDL: &str = r#"
CREATE TABLE __SCHEMA__.sm_recipient_route_source_guards (
    message_key UUID NOT NULL,
    recipient TEXT NOT NULL CHECK (recipient <> ''),
    PRIMARY KEY (message_key, recipient)
);
CREATE TABLE __SCHEMA__.sm_recipient_route_guards (
    recipient TEXT PRIMARY KEY CHECK (recipient <> '' AND pg_catalog.strpos(recipient, '/') = 0),
    closed BOOLEAN NOT NULL DEFAULT FALSE
);
CREATE TABLE __SCHEMA__.sm_recipient_route_frame_owners (
    stream_id TEXT NOT NULL,
    sequence BIGINT NOT NULL,
    recipient TEXT NOT NULL,
    PRIMARY KEY (stream_id, sequence),
    FOREIGN KEY (stream_id, sequence)
        REFERENCES __SCHEMA__.sm_unacked (stream_id, sequence) ON DELETE CASCADE
);
"#;

#[derive(Debug, Error)]
pub enum CandidateRouteError {
    #[error("recipient route is closed")]
    Closed,
    #[error("recipient still has a counted SM frame")]
    OutstandingSm,
    #[error("candidate recipient and payload identities differ")]
    Identity,
    #[error("frame provenance or owner is unsupported")]
    UnsupportedProvenance,
    #[error("snapshot inventory changed; roll back and retry in a new transaction")]
    Retry,
    #[error("snapshot ledger conflict; roll back before looking up the winner")]
    LedgerConflict,
    #[error("snapshot revision is stale")]
    StaleRevision,
    #[error("snapshot session authority is terminal")]
    Terminal,
    #[error("snapshot session generation changed")]
    GenerationChanged,
    #[error("snapshot operation UUID was reused with different input")]
    OperationIdentity,
    #[error("snapshot revision is exhausted")]
    RevisionExhausted,
    #[error("snapshot revision laboratory does not support this ledger mode")]
    UnsupportedLedger,
    #[error("recipient route candidate requires PostgreSQL")]
    PostgresRequired,
    #[error("recipient route candidate requires READ COMMITTED")]
    ReadCommittedRequired,
    #[error("candidate transaction failed: {0}")]
    Storage(String),
}

/// Lock the complete route set before any session gate. Ordering is global:
/// all distinct sources, then all (source, recipient) pairs, then recipients.
/// A caller must never add another source to this transaction after the gate.
pub async fn lock_source_recipient_set(
    tx: &mut Transaction<'_>,
    schema: &str,
    routes: &[(Uuid, BareJid)],
) -> Result<bool, CandidateRouteError> {
    lock_route_set(tx, schema, routes, &[]).await
}

pub(super) async fn lock_route_set(
    tx: &mut Transaction<'_>,
    schema: &str,
    routes: &[(Uuid, BareJid)],
    extra_recipients: &[BareJid],
) -> Result<bool, CandidateRouteError> {
    use std::collections::BTreeSet;

    if tx.driver() != DatabaseDriver::Postgres {
        return Err(CandidateRouteError::PostgresRequired);
    }
    let mut isolation = tx
        .query(
            "SELECT pg_catalog.current_setting('transaction_isolation')",
            (),
        )
        .await
        .map_err(storage)?;
    let Some(isolation) = isolation.next().await.map_err(storage)? else {
        return Err(CandidateRouteError::Storage(
            "transaction isolation returned no row".into(),
        ));
    };
    let isolation: String = isolation.get(0).map_err(storage)?;
    if isolation != "read committed" {
        return Err(CandidateRouteError::ReadCommittedRequired);
    }
    let schema = validated_schema(schema)?;
    tx.execute(
        &format!("SET LOCAL search_path TO pg_catalog, {schema}, pg_temp"),
        (),
    )
    .await
    .map_err(storage)?;
    let routes: BTreeSet<(Uuid, String)> = routes
        .iter()
        .map(|(source, recipient)| (*source, recipient.to_string()))
        .collect();
    let sources: BTreeSet<Uuid> = routes.iter().map(|(source, _)| *source).collect();
    let recipients: BTreeSet<String> = routes
        .iter()
        .map(|(_, recipient)| recipient.clone())
        .chain(extra_recipients.iter().map(ToString::to_string))
        .collect();

    for source in sources {
        let rows = tx
            .query(
                &format!("SELECT 1 FROM {schema}.ingress_messages WHERE message_key = ?::uuid FOR UPDATE"),
                crate::db_params![source.to_string()],
            )
            .await
            .map_err(storage)?;
        if !has_row(rows).await.map_err(storage)? {
            return Err(CandidateRouteError::Storage(
                "canonical source is missing".into(),
            ));
        }
    }
    for (source, recipient) in &routes {
        tx.execute(
            &format!("INSERT INTO {schema}.sm_recipient_route_source_guards (message_key, recipient) VALUES (?::uuid, ?) ON CONFLICT DO NOTHING"),
            crate::db_params![source.to_string(), recipient.clone()],
        )
        .await
        .map_err(storage)?;
        let rows = tx
            .query(
                &format!("SELECT 1 FROM {schema}.sm_recipient_route_source_guards WHERE message_key = ?::uuid AND recipient = ? FOR UPDATE"),
                crate::db_params![source.to_string(), recipient.clone()],
            )
            .await
            .map_err(storage)?;
        if !has_row(rows).await.map_err(storage)? {
            return Err(CandidateRouteError::Storage(
                "source guard disappeared".into(),
            ));
        }
    }
    let mut all_open = true;
    for recipient in recipients {
        tx.execute(
            &format!("INSERT INTO {schema}.sm_recipient_route_guards (recipient) VALUES (?) ON CONFLICT DO NOTHING"),
            crate::db_params![recipient.clone()],
        )
        .await
        .map_err(storage)?;
        let mut rows = tx
            .query(
                &format!("SELECT closed FROM {schema}.sm_recipient_route_guards WHERE recipient = ? FOR UPDATE"),
                crate::db_params![recipient],
            )
            .await
            .map_err(storage)?;
        let Some(row) = rows.next().await.map_err(storage)? else {
            return Err(CandidateRouteError::Storage(
                "recipient guard disappeared".into(),
            ));
        };
        let closed: bool = row.get(0).map_err(storage)?;
        all_open &= !closed;
    }
    Ok(all_open)
}

/// Compatibility wrapper for the original single-recipient laboratory.
pub async fn lock_sources_then_recipient(
    tx: &mut Transaction<'_>,
    schema: &str,
    sources: &[Uuid],
    recipient: &BareJid,
) -> Result<bool, CandidateRouteError> {
    let routes: Vec<_> = sources
        .iter()
        .map(|source| (*source, recipient.clone()))
        .collect();
    if routes.is_empty() {
        return Err(CandidateRouteError::Identity);
    }
    lock_source_recipient_set(tx, schema, &routes).await
}

/// The old laboratory API accepts a single canonical source only.
pub async fn append_unacked_if_open(
    tx: &mut Transaction<'_>,
    schema: &str,
    source: Uuid,
    recipient: &BareJid,
    stanza: &PersistedUnackedStanza,
) -> Result<(), CandidateRouteError> {
    if !stanza.provenance.claims().is_some_and(|claims| {
        !claims.is_empty()
            && claims
                .iter()
                .all(|claim| claim.message_key.to_storage() == source)
    }) {
        return Err(CandidateRouteError::Identity);
    }
    append_complete_sources_if_open(tx, schema, recipient, stanza).await
}

/// Exercise the production ordinary append after every claimed source and the
/// recipient route. The caller owns commit and must roll back on every error
/// or cancellation. Committing after interruption can leave an ownerless frame
/// that conservatively blocks pending acceptance; this remains a dark lab.
pub async fn append_complete_sources_if_open(
    tx: &mut Transaction<'_>,
    schema: &str,
    recipient: &BareJid,
    stanza: &PersistedUnackedStanza,
) -> Result<(), CandidateRouteError> {
    let full = frame_resource(stanza, recipient)?;
    let routes = frame_routes(stanza, &full)?;
    if !lock_source_recipient_set(tx, schema, &routes).await? {
        return Err(CandidateRouteError::Closed);
    }
    lock_stream_gate(tx, &stanza.stream_id).await?;
    require_session(tx, schema, &stanza.stream_id, &full).await?;
    super::implementation::append_unacked_in_transaction(tx, stanza)
        .await
        .map_err(storage)?;
    insert_owner(tx, schema, stanza, recipient).await
}

/// Exercise the real promotion suffix allocator under the same route locks.
/// The caller must roll back after any error or cancellation, including a SQL
/// conflict. Committing an interrupted append can leave owner or watermark
/// state incomplete; this borrowed transaction interface remains unselected.
pub async fn append_promotion_if_open(
    tx: &mut Transaction<'_>,
    schema: &str,
    recipient: &BareJid,
    stanza: PersistedUnackedStanza,
) -> Result<waddle_xmpp::stream_management::persistence::PromotionAppendOutcome, CandidateRouteError>
{
    use waddle_xmpp::stream_management::persistence::PromotionAppendOutcome;

    let full = frame_resource(&stanza, recipient)?;
    let routes = frame_routes(&stanza, &full)?;
    if !lock_source_recipient_set(tx, schema, &routes).await? {
        return Err(CandidateRouteError::Closed);
    }
    lock_stream_gate(tx, &stanza.stream_id).await?;
    require_session(tx, schema, &stanza.stream_id, &full).await?;
    let outcome = super::promotion_inventory::append(tx, stanza.clone(), DatabaseDriver::Postgres)
        .await
        .map_err(storage)?;
    match outcome {
        PromotionAppendOutcome::Appended => insert_owner(tx, schema, &stanza, recipient).await?,
        PromotionAppendOutcome::AlreadyRecorded => {
            let schema = validated_schema(schema)?;
            let mut rows = tx.query(
                &format!("SELECT recipient FROM {schema}.sm_recipient_route_frame_owners WHERE stream_id = ? AND sequence = ?"),
                crate::db_params![stanza.stream_id.as_str(), i64::from(stanza.sequence)],
            ).await.map_err(storage)?;
            let owner = rows
                .next()
                .await
                .map_err(storage)?
                .ok_or(CandidateRouteError::UnsupportedProvenance)?;
            let owner: String = owner.get(0).map_err(storage)?;
            if owner != recipient.to_string() {
                return Err(CandidateRouteError::Identity);
            }
        }
        PromotionAppendOutcome::Retired => {}
    }
    Ok(outcome)
}

pub(super) async fn lock_stream_gate(
    tx: &mut Transaction<'_>,
    stream: &waddle_xmpp::pending_delivery::SmSessionId,
) -> Result<(), CandidateRouteError> {
    // This candidate-only two-key advisory namespace also serializes creation
    // when no sm_sessions row exists. Hash collisions only serialize streams.
    tx.execute(
        "SELECT pg_catalog.pg_advisory_xact_lock(1463895108, pg_catalog.hashtext(?))",
        crate::db_params![stream.as_str()],
    )
    .await
    .map_err(storage)?;
    Ok(())
}

async fn require_session(
    tx: &mut Transaction<'_>,
    schema: &str,
    stream: &waddle_xmpp::pending_delivery::SmSessionId,
    full: &jid::FullJid,
) -> Result<(), CandidateRouteError> {
    let schema = validated_schema(schema)?;
    let mut sessions = tx.query(
        &format!("SELECT bare_jid, full_jid FROM {schema}.sm_sessions WHERE stream_id = ? FOR UPDATE"),
        crate::db_params![stream.as_str()],
    ).await.map_err(storage)?;
    let Some(session) = sessions.next().await.map_err(storage)? else {
        return Err(CandidateRouteError::Identity);
    };
    let session_bare: Option<String> = session.get(0).map_err(storage)?;
    let session_full: String = session.get(1).map_err(storage)?;
    if session_bare.as_deref() != Some(full.to_bare().to_string().as_str())
        || session_full != full.to_string()
    {
        return Err(CandidateRouteError::Identity);
    }
    Ok(())
}

async fn insert_owner(
    tx: &mut Transaction<'_>,
    schema: &str,
    stanza: &PersistedUnackedStanza,
    recipient: &BareJid,
) -> Result<(), CandidateRouteError> {
    let schema = validated_schema(schema)?;
    tx.execute(
        &format!("INSERT INTO {schema}.sm_recipient_route_frame_owners (stream_id, sequence, recipient) VALUES (?, ?, ?)"),
        crate::db_params![stanza.stream_id.as_str(), i64::from(stanza.sequence), recipient.to_string()],
    ).await.map_err(storage)?;
    Ok(())
}

fn frame_resource(
    stanza: &PersistedUnackedStanza,
    recipient: &BareJid,
) -> Result<jid::FullJid, CandidateRouteError> {
    if stanza_recipient(stanza) != Some(recipient.clone()) {
        return Err(CandidateRouteError::Identity);
    }
    let claims = stanza
        .provenance
        .claims()
        .ok_or(CandidateRouteError::UnsupportedProvenance)?;
    let first = claims
        .first()
        .ok_or(CandidateRouteError::UnsupportedProvenance)?;
    if first.resource.to_bare() != *recipient
        || claims.iter().any(|claim| claim.resource != first.resource)
    {
        return Err(CandidateRouteError::Identity);
    }
    Ok(first.resource.clone())
}

fn frame_routes(
    stanza: &PersistedUnackedStanza,
    full: &jid::FullJid,
) -> Result<Vec<(Uuid, BareJid)>, CandidateRouteError> {
    let claims = stanza
        .provenance
        .claims()
        .ok_or(CandidateRouteError::UnsupportedProvenance)?;
    if claims.is_empty() || claims.iter().any(|claim| claim.resource != *full) {
        return Err(CandidateRouteError::UnsupportedProvenance);
    }
    Ok(claims
        .iter()
        .map(|claim| (claim.message_key.to_storage(), full.to_bare()))
        .collect())
}

/// Which existing snapshot ledger behavior to exercise in the dark candidate.
#[derive(Clone)]
pub enum CandidateSnapshotLedger {
    Untouched,
    Exclusive(waddle_xmpp::stream_management::persistence::PersistedIngressAppend),
    NewDelivery(waddle_xmpp::stream_management::persistence::PersistedIngressAppend),
    Drained(Vec<waddle_xmpp::stream_management::persistence::PersistedIngressAppend>),
}

#[derive(Debug, PartialEq, Eq)]
pub enum CandidateSnapshotOutcome {
    Applied {
        withheld: Vec<waddle_xmpp::stream_management::SmIngressAppendKey>,
    },
    AlreadyResolved,
    /// The same immutable operation already committed, as proven by its row.
    AlreadyApplied,
}

/// Replace a portable snapshot under its complete old/new route inventory.
///
/// The operation owns its transaction until the complete mutation succeeds.
/// Cancellation drops that transaction; an error explicitly rolls it back.
/// A conflict winner may be read only after the returned error, using a fresh
/// transaction. Success returns the transaction and is not a commit; an
/// uncertain COMMIT requires positive reconciliation by the owner. This dark
/// laboratory assumes every competing SM writer uses the same gates. The
/// inventory recheck does not prove that the caller's snapshot includes writes
/// already committed before discovery; selecting this path requires a separate
/// captured-occurrence or generation authority for that input.
pub async fn store_snapshot_if_open<'a>(
    mut tx: Transaction<'a>,
    schema: &str,
    principal: Option<&waddle_xmpp::auth::AuthenticatedPrincipalRef>,
    session: &waddle_xmpp::stream_management::persistence::PersistedSession,
    unacked: &[PersistedUnackedStanza],
    ledger: CandidateSnapshotLedger,
) -> Result<(Transaction<'a>, CandidateSnapshotOutcome), CandidateRouteError> {
    match store_snapshot_in_transaction(&mut tx, schema, principal, session, unacked, ledger, None)
        .await
    {
        Ok(outcome) => Ok((tx, outcome)),
        Err(error) => {
            tx.rollback().await.map_err(storage)?;
            Err(error)
        }
    }
}

pub(super) async fn store_snapshot_in_transaction(
    tx: &mut Transaction<'_>,
    schema: &str,
    principal: Option<&waddle_xmpp::auth::AuthenticatedPrincipalRef>,
    session: &waddle_xmpp::stream_management::persistence::PersistedSession,
    unacked: &[PersistedUnackedStanza],
    ledger: CandidateSnapshotLedger,
    revision: Option<&super::snapshot_revision_candidate::Operation>,
) -> Result<CandidateSnapshotOutcome, CandidateRouteError> {
    use super::atomic_store::{Ledger, PreparedSnapshot, SnapshotApplyError, StoreOutcome};

    if principal.is_some_and(|principal| principal.bare_jid() != &session.jid.to_bare()) {
        return Err(CandidateRouteError::Identity);
    }
    let prepared = PreparedSnapshot::new(principal, session, unacked).map_err(storage)?;
    let mut routes = Vec::new();
    for frame in unacked {
        if frame.stream_id != session.stream_id
            || frame_resource(frame, &session.jid.to_bare())? != session.jid
        {
            return Err(CandidateRouteError::Identity);
        }
        routes.extend(frame_routes(frame, &session.jid)?);
    }
    let ledger = match ledger {
        CandidateSnapshotLedger::Untouched => Ledger::Untouched,
        CandidateSnapshotLedger::Exclusive(append) => Ledger::Exclusive(Box::new(append)),
        CandidateSnapshotLedger::NewDelivery(append) => Ledger::NewDelivery(Box::new(append)),
        CandidateSnapshotLedger::Drained(appends) => Ledger::Drained(appends),
    };
    let appends: Vec<_> = match &ledger {
        Ledger::Untouched => Vec::new(),
        Ledger::Exclusive(append) | Ledger::NewDelivery(append) => vec![append.as_ref()],
        Ledger::Drained(appends) => appends.iter().collect(),
    };
    for append in appends {
        if append.accepting_stream != session.stream_id
            || append.key.resource != session.jid
            || append.disposition
                != waddle_xmpp::stream_management::persistence::IngressCustodyDisposition::Pending
        {
            return Err(CandidateRouteError::Identity);
        }
        routes.push((append.key.message_key.to_storage(), session.jid.to_bare()));
    }

    let old = read_snapshot_inventory(tx, schema, &session.stream_id, false).await?;
    if let Some((bare, full, _)) = &old.session {
        if *bare != session.jid.to_bare().to_string() || *full != session.jid.to_string() {
            return Err(CandidateRouteError::Identity);
        }
    } else if principal.is_none() {
        return Err(CandidateRouteError::Identity);
    }
    routes.extend(old.routes()?);
    let mut recipients = vec![session.jid.to_bare()];
    if let Some((bare, _, _)) = &old.session {
        recipients.push(bare.parse().map_err(|_| CandidateRouteError::Identity)?);
    }
    if !lock_route_set(tx, schema, &routes, &recipients).await? {
        return Err(CandidateRouteError::Closed);
    }
    // New-delivery authorization's source is already in `routes`. This call
    // deliberately precedes the gate; no helper below may discover a source.
    if let Ledger::NewDelivery(append) = &ledger {
        if super::ingress_append::authorize_new_delivery(tx, &append.key)
            .await
            .map_err(storage)?
            == super::ingress_append::NewDeliveryAuthorization::AlreadyResolved
        {
            return Ok(CandidateSnapshotOutcome::AlreadyResolved);
        }
    }
    lock_stream_gate(tx, &session.stream_id).await?;
    if let Some(operation) = revision {
        if super::snapshot_revision_candidate::operation_applied(tx, schema, operation).await? {
            return Ok(CandidateSnapshotOutcome::AlreadyApplied);
        }
        super::snapshot_revision_candidate::check_revision(tx, schema, session, operation).await?;
    }
    let current = read_snapshot_inventory(tx, schema, &session.stream_id, true).await?;
    if current != old {
        return Err(CandidateRouteError::Retry);
    }

    let outcome = match super::atomic_store::apply_snapshot_in_transaction(tx, &prepared, &ledger)
        .await
    {
        Ok(StoreOutcome::Committed { withheld }) => CandidateSnapshotOutcome::Applied { withheld },
        Ok(
            StoreOutcome::ObligationAlreadyResolved
            | StoreOutcome::ObligationAlreadyAllocated { .. },
        ) => {
            return Err(CandidateRouteError::Storage(
                "unexpected snapshot apply outcome".into(),
            ));
        }
        Err(SnapshotApplyError::LedgerError { conflict: true, .. }) => {
            return Err(CandidateRouteError::LedgerConflict)
        }
        Err(SnapshotApplyError::LedgerError {
            error,
            conflict: false,
        }) => return Err(storage(error)),
        Err(SnapshotApplyError::Storage(error)) => return Err(storage(error)),
    };
    for frame in unacked {
        insert_owner(tx, schema, frame, &session.jid.to_bare()).await?;
    }
    if let Some(operation) = revision {
        super::snapshot_revision_candidate::record_operation(tx, schema, operation).await?;
    }
    Ok(outcome)
}

#[derive(PartialEq, Eq)]
pub(super) struct SnapshotInventory {
    pub(super) session: Option<(String, String, String)>,
    frames: Vec<SnapshotFrame>,
}

#[derive(PartialEq, Eq)]
struct SnapshotFrame {
    sequence: i64,
    stanza_xml: String,
    receipt_ms: i64,
    ingress_receipts: Option<Vec<u8>>,
    provenance: Option<Vec<u8>>,
    owner: Option<String>,
}

impl SnapshotInventory {
    pub(super) fn routes(&self) -> Result<Vec<(Uuid, BareJid)>, CandidateRouteError> {
        let Some((bare, full, _)) = &self.session else {
            return if self.frames.is_empty() {
                Ok(Vec::new())
            } else {
                Err(CandidateRouteError::UnsupportedProvenance)
            };
        };
        let full: jid::FullJid = full.parse().map_err(|_| CandidateRouteError::Identity)?;
        let bare: BareJid = bare.parse().map_err(|_| CandidateRouteError::Identity)?;
        if full.to_bare() != bare {
            return Err(CandidateRouteError::Identity);
        }
        let mut routes = Vec::new();
        for frame in &self.frames {
            if frame.owner.as_deref() != Some(bare.to_string().as_str()) {
                return Err(CandidateRouteError::UnsupportedProvenance);
            }
            let provenance = frame
                .provenance
                .as_deref()
                .map(waddle_xmpp::stream_management::SmFrameProvenance::decode)
                .ok_or(CandidateRouteError::UnsupportedProvenance)?;
            let claims = provenance
                .claims()
                .ok_or(CandidateRouteError::UnsupportedProvenance)?;
            if claims.is_empty() || claims.iter().any(|claim| claim.resource != full) {
                return Err(CandidateRouteError::UnsupportedProvenance);
            }
            routes.extend(
                claims
                    .iter()
                    .map(|claim| (claim.message_key.to_storage(), bare.clone())),
            );
        }
        Ok(routes)
    }
}

pub(super) async fn read_snapshot_inventory(
    tx: &mut Transaction<'_>,
    schema: &str,
    stream: &waddle_xmpp::pending_delivery::SmSessionId,
    lock_session: bool,
) -> Result<SnapshotInventory, CandidateRouteError> {
    let schema = validated_schema(schema)?;
    let suffix = if lock_session { " FOR UPDATE" } else { "" };
    let mut sessions = tx.query(
        &format!("SELECT s.bare_jid, s.full_jid, pg_catalog.row_to_json(s)::text FROM {schema}.sm_sessions s WHERE s.stream_id = ?{suffix}"),
        crate::db_params![stream.as_str()],
    ).await.map_err(storage)?;
    let session = match sessions.next().await.map_err(storage)? {
        Some(row) => {
            let bare: Option<String> = row.get(0).map_err(storage)?;
            let full: String = row.get(1).map_err(storage)?;
            let state: String = row.get(2).map_err(storage)?;
            Some((
                bare.ok_or(CandidateRouteError::UnsupportedProvenance)?,
                full,
                state,
            ))
        }
        None => None,
    };
    let bounded =
        super::codec::bounded_provenance_projection(DatabaseDriver::Postgres, "u.provenance");
    let mut rows = tx.query(
        &format!("SELECT u.sequence, u.stanza_xml, u.original_receipt_at_ms, u.ingress_receipts, {bounded}, f.recipient FROM {schema}.sm_unacked u LEFT JOIN {schema}.sm_recipient_route_frame_owners f ON f.stream_id = u.stream_id AND f.sequence = u.sequence WHERE u.stream_id = ? ORDER BY u.sequence"),
        crate::db_params![stream.as_str()],
    ).await.map_err(storage)?;
    let mut frames = Vec::new();
    while let Some(row) = rows.next().await.map_err(storage)? {
        frames.push(SnapshotFrame {
            sequence: row.get(0).map_err(storage)?,
            stanza_xml: row.get(1).map_err(storage)?,
            receipt_ms: row.get(2).map_err(storage)?,
            ingress_receipts: row.get(3).map_err(storage)?,
            provenance: row.get(4).map_err(storage)?,
            owner: row.get(5).map_err(storage)?,
        });
    }
    let inventory = SnapshotInventory { session, frames };
    // Malformed or untracked inventory never authorizes destructive replacement.
    inventory.routes()?;
    Ok(inventory)
}

/// Close the recipient route and exercise the real pending-delivery insert in
/// the same transaction. Refusal leaves the already-counted SM row untouched.
pub async fn accept_pending_if_quiescent(
    tx: &mut Transaction<'_>,
    schema: &str,
    sources: &[Uuid],
    recipient: &BareJid,
    row: &PendingRow,
    quota: QuotaPolicy,
) -> Result<InsertOutcome, CandidateRouteError> {
    if row.recipient != *recipient {
        return Err(CandidateRouteError::Identity);
    }
    let qualified_schema = validated_schema(schema)?;
    if sources.is_empty() {
        return Err(CandidateRouteError::Identity);
    }
    if !lock_sources_then_recipient(tx, schema, sources, recipient).await? {
        return Err(CandidateRouteError::Closed);
    }
    let mut inventory = tx
        .query(
            &format!("SELECT EXISTS (SELECT 1 FROM {qualified_schema}.sm_unacked u LEFT JOIN {qualified_schema}.sm_recipient_route_frame_owners f ON f.stream_id = u.stream_id AND f.sequence = u.sequence WHERE f.recipient IS NULL OR f.recipient = ?)"),
            crate::db_params![recipient.to_string()],
        )
        .await
        .map_err(storage)?;
    let row_inventory = inventory.next().await.map_err(storage)?;
    let Some(row_inventory) = row_inventory else {
        return Err(CandidateRouteError::Storage(
            "sibling inventory returned no row".into(),
        ));
    };
    let has_counted_sm: bool = row_inventory.get(0).map_err(storage)?;
    if has_counted_sm {
        return Err(CandidateRouteError::OutstandingSm);
    }
    // Close before the insert. A cancelled caller can commit a closed route,
    // but cannot commit accepted pending with the route still open.
    tx.execute(
        &format!("UPDATE {qualified_schema}.sm_recipient_route_guards SET closed = TRUE WHERE recipient = ? AND closed = FALSE"),
        crate::db_params![recipient.to_string()],
    )
    .await
    .map_err(storage)?;
    let outcome = database::insert_in_transaction(tx, row, quota, None)
        .await
        .map_err(|error| CandidateRouteError::Storage(error.to_string()))?;
    if outcome == InsertOutcome::QuotaExceeded {
        tx.execute(
            &format!("UPDATE {qualified_schema}.sm_recipient_route_guards SET closed = FALSE WHERE recipient = ? AND closed = TRUE"),
            crate::db_params![recipient.to_string()],
        )
        .await
        .map_err(storage)?;
    }
    Ok(outcome)
}

async fn has_row(mut rows: Rows) -> Result<bool, crate::db::DatabaseError> {
    Ok(rows.next().await?.is_some())
}

pub(super) fn validated_schema(schema: &str) -> Result<String, CandidateRouteError> {
    if schema.is_empty()
        || schema.len() > 63
        || !schema
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(CandidateRouteError::Storage(
            "candidate schema name is invalid".into(),
        ));
    }
    Ok(format!("\"{schema}\""))
}

fn stanza_recipient(stanza: &PersistedUnackedStanza) -> Option<BareJid> {
    let Stanza::Message(message) = stanza.stanza.as_ref() else {
        return None;
    };
    message.to.as_ref().map(jid::Jid::to_bare)
}

pub(super) fn storage(error: impl std::fmt::Display) -> CandidateRouteError {
    CandidateRouteError::Storage(error.to_string())
}
