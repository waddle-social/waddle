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
use waddle_xmpp::stream_management::SmFrameProvenance;
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
    #[error("recipient route candidate requires PostgreSQL")]
    PostgresRequired,
    #[error("recipient route candidate requires READ COMMITTED")]
    ReadCommittedRequired,
    #[error("candidate transaction failed: {0}")]
    Storage(String),
}

/// Acquire canonical sources in UUID order, then source guards in that order,
/// then the bare-recipient route row. Keep the caller transaction open through
/// the selected append or pending insert.
pub async fn lock_sources_then_recipient(
    tx: &mut Transaction<'_>,
    schema: &str,
    sources: &[Uuid],
    recipient: &BareJid,
) -> Result<bool, CandidateRouteError> {
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
    let mut sources = sources.to_vec();
    sources.sort_unstable();
    sources.dedup();

    for source in &sources {
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

    let recipient = recipient.to_string();
    for source in sources {
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
    Ok(!closed)
}

/// Exercise the existing portable SM INSERT after the shared interlock.
pub async fn append_unacked_if_open(
    tx: &mut Transaction<'_>,
    schema: &str,
    source: Uuid,
    recipient: &BareJid,
    stanza: &PersistedUnackedStanza,
) -> Result<(), CandidateRouteError> {
    if stanza_recipient(stanza) != Some(recipient.clone()) {
        return Err(CandidateRouteError::Identity);
    }
    if !lock_sources_then_recipient(tx, schema, &[source], recipient).await? {
        return Err(CandidateRouteError::Closed);
    }
    let qualified_schema = validated_schema(schema)?;
    let mut sessions = tx
        .query(
            &format!("SELECT bare_jid, full_jid FROM {qualified_schema}.sm_sessions WHERE stream_id = ? FOR UPDATE"),
            crate::db_params![stanza.stream_id.as_str().to_string()],
        )
        .await
        .map_err(storage)?;
    let Some(session) = sessions.next().await.map_err(storage)? else {
        return Err(CandidateRouteError::Identity);
    };
    let session_bare: Option<String> = session.get(0).map_err(storage)?;
    let session_full: String = session.get(1).map_err(storage)?;
    let session_full = session_full
        .parse::<jid::FullJid>()
        .map_err(|_| CandidateRouteError::Identity)?;
    if session_bare.as_deref() != Some(recipient.to_string().as_str())
        || session_full.to_bare() != *recipient
        || !provenance_matches_source(&stanza.provenance, source, &session_full)
    {
        return Err(CandidateRouteError::Identity);
    }
    super::implementation::append_unacked_in_transaction(tx, stanza)
        .await
        .map_err(|error| CandidateRouteError::Storage(error.to_string()))?;
    tx.execute(
        &format!("INSERT INTO {qualified_schema}.sm_recipient_route_frame_owners (stream_id, sequence, recipient) VALUES (?, ?, ?)"),
        crate::db_params![stanza.stream_id.as_str().to_string(), i64::from(stanza.sequence), recipient.to_string()],
    )
    .await
    .map_err(storage)?;
    Ok(())
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

fn validated_schema(schema: &str) -> Result<String, CandidateRouteError> {
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

fn provenance_matches_source(
    provenance: &SmFrameProvenance,
    source: Uuid,
    session_full: &jid::FullJid,
) -> bool {
    provenance.claims().is_some_and(|claims| {
        !claims.is_empty()
            && claims.iter().all(|claim| {
                claim.message_key.to_storage() == source && claim.resource == *session_full
            })
    })
}

fn storage(error: impl std::fmt::Display) -> CandidateRouteError {
    CandidateRouteError::Storage(error.to_string())
}
