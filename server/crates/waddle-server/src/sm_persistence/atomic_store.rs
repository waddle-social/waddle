use super::{
    ingress_append, serialize_presence_payloads, serialize_stanza, show_wire_str,
    DatabaseSmPersistence, KeyedSnapshotOutcome, PersistedIngressAppend, PersistedSession,
    PersistedUnackedStanza, SmPersistenceError, SmSessionId,
};
use crate::sm_persistence::codec::encode_session_principal;

pub(super) async fn store_session_atomic(
    storage: &DatabaseSmPersistence,
    session: PersistedSession,
    unacked: Vec<PersistedUnackedStanza>,
) -> Result<(), SmPersistenceError> {
    store_session_atomic_inner(storage, None, session, unacked, Ledger::Untouched)
        .await
        .map(|_| ())
}

pub(super) async fn store_session_atomic_with_principal(
    storage: &DatabaseSmPersistence,
    principal: &waddle_xmpp::auth::AuthenticatedPrincipalRef,
    session: PersistedSession,
    unacked: Vec<PersistedUnackedStanza>,
) -> Result<(), SmPersistenceError> {
    store_session_atomic_with_principal_and_ingress_appends(
        storage,
        principal,
        session,
        unacked,
        Vec::new(),
    )
    .await
    .map(drop)
}

pub(super) async fn store_session_atomic_with_principal_and_ingress_appends(
    storage: &DatabaseSmPersistence,
    principal: &waddle_xmpp::auth::AuthenticatedPrincipalRef,
    session: PersistedSession,
    unacked: Vec<PersistedUnackedStanza>,
    appends: Vec<PersistedIngressAppend>,
) -> Result<Vec<waddle_xmpp::stream_management::SmIngressAppendKey>, SmPersistenceError> {
    match store_session_atomic_inner(
        storage,
        Some(principal),
        session,
        unacked,
        Ledger::Drained(appends),
    )
    .await?
    {
        StoreOutcome::Committed { withheld } => Ok(withheld),
        StoreOutcome::ObligationAlreadyAllocated { .. }
        | StoreOutcome::ObligationAlreadyResolved => Err(SmPersistenceError::Other(
            "a drained batch never aborts on a ledger conflict".into(),
        )),
    }
}

pub(super) async fn store_session_atomic_with_ingress_append(
    storage: &DatabaseSmPersistence,
    session: PersistedSession,
    unacked: Vec<PersistedUnackedStanza>,
    append: PersistedIngressAppend,
) -> Result<KeyedSnapshotOutcome, SmPersistenceError> {
    Ok(
        match store_session_atomic_inner(
            storage,
            None,
            session,
            unacked,
            Ledger::Exclusive(Box::new(append)),
        )
        .await?
        {
            StoreOutcome::Committed { .. } => KeyedSnapshotOutcome::Committed,
            StoreOutcome::ObligationAlreadyResolved => {
                KeyedSnapshotOutcome::ObligationAlreadyResolved
            }
            StoreOutcome::ObligationAlreadyAllocated { accepting_stream } => {
                KeyedSnapshotOutcome::ObligationAlreadyAllocated { accepting_stream }
            }
        },
    )
}

pub(super) async fn store_session_atomic_with_ingress_delivery(
    storage: &DatabaseSmPersistence,
    session: PersistedSession,
    unacked: Vec<PersistedUnackedStanza>,
    append: PersistedIngressAppend,
) -> Result<KeyedSnapshotOutcome, SmPersistenceError> {
    Ok(
        match store_session_atomic_inner(
            storage,
            None,
            session,
            unacked,
            Ledger::NewDelivery(Box::new(append)),
        )
        .await?
        {
            StoreOutcome::Committed { .. } => KeyedSnapshotOutcome::Committed,
            StoreOutcome::ObligationAlreadyResolved => {
                KeyedSnapshotOutcome::ObligationAlreadyResolved
            }
            StoreOutcome::ObligationAlreadyAllocated { accepting_stream } => {
                KeyedSnapshotOutcome::ObligationAlreadyAllocated { accepting_stream }
            }
        },
    )
}

/// How a snapshot write touches the ingress allocation ledger.
pub(super) enum Ledger {
    Untouched,
    /// One obligation gates the whole write: a conflict commits nothing.
    Exclusive(Box<PersistedIngressAppend>),
    /// A new delivery additionally excludes a standing live queue attempt.
    NewDelivery(Box<PersistedIngressAppend>),
    /// Drained entries already hold counted sequences: a conflict withholds only its proof.
    Drained(Vec<PersistedIngressAppend>),
}

pub(super) enum StoreOutcome {
    ObligationAlreadyResolved,
    Committed {
        withheld: Vec<waddle_xmpp::stream_management::SmIngressAppendKey>,
    },
    ObligationAlreadyAllocated {
        accepting_stream: SmSessionId,
    },
}

async fn store_session_atomic_inner(
    storage: &DatabaseSmPersistence,
    principal: Option<&waddle_xmpp::auth::AuthenticatedPrincipalRef>,
    session: PersistedSession,
    unacked: Vec<PersistedUnackedStanza>,
    ledger: Ledger,
) -> Result<StoreOutcome, SmPersistenceError> {
    let lock = storage.lock_for(&session.stream_id);
    let _guard = lock.lock().await;

    let prepared = PreparedSnapshot::new(principal, &session, &unacked)?;

    let mut tx = storage
        .db
        .begin_immediate()
        .await
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;

    if let Ledger::NewDelivery(append) = &ledger {
        if ingress_append::authorize_new_delivery(&mut tx, &append.key).await?
            == ingress_append::NewDeliveryAuthorization::AlreadyResolved
        {
            return Ok(StoreOutcome::ObligationAlreadyResolved);
        }
    }

    let outcome = match apply_snapshot_in_transaction(&mut tx, &prepared, &ledger).await {
        Ok(outcome) => outcome,
        Err(SnapshotApplyError::Storage(error)) => return Err(error),
        Err(SnapshotApplyError::LedgerError { error, conflict }) => {
            let append = match &ledger {
                Ledger::Exclusive(append) | Ledger::NewDelivery(append) => append,
                Ledger::Untouched | Ledger::Drained(_) => {
                    return Err(SmPersistenceError::Other(
                        "unexpected snapshot ledger conflict".into(),
                    ));
                }
            };
            tx.rollback()
                .await
                .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
            if !conflict {
                return Err(SmPersistenceError::Other(error.to_string()));
            }
            let winner = ingress_append::get(&storage.db, &append.key)
                .await?
                .ok_or_else(|| {
                    SmPersistenceError::Other(
                        "ingress append conflict winner missing after rollback".into(),
                    )
                })?;
            return Ok(StoreOutcome::ObligationAlreadyAllocated {
                accepting_stream: winner.accepting_stream,
            });
        }
    };
    tx.commit()
        .await
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
    Ok(outcome)
}

/// A snapshot's encoding is prepared before the existing wrapper opens a
/// transaction, preserving its failure and lock behavior.
pub(super) struct PreparedSnapshot<'a> {
    session: &'a PersistedSession,
    unacked: &'a [PersistedUnackedStanza],
    max_resume_duration_ms: i64,
    detached_at_ms: i64,
    presence_show_str: Option<&'static str>,
    presence_payloads_xml: Option<String>,
    encoded_principal: Option<crate::sm_persistence::codec::EncodedSessionPrincipal>,
}

impl<'a> PreparedSnapshot<'a> {
    pub(super) fn new(
        principal: Option<&waddle_xmpp::auth::AuthenticatedPrincipalRef>,
        session: &'a PersistedSession,
        unacked: &'a [PersistedUnackedStanza],
    ) -> Result<Self, SmPersistenceError> {
        Ok(Self {
            session,
            unacked,
            max_resume_duration_ms: i64::try_from(session.max_resume_duration.as_millis())
                .map_err(|_| {
                    SmPersistenceError::Other("max_resume_duration overflows i64".into())
                })?,
            detached_at_ms: session.detached_at.timestamp_millis(),
            presence_show_str: session.presence_show.as_ref().map(show_wire_str),
            presence_payloads_xml: serialize_presence_payloads(&session.presence_payloads)?,
            encoded_principal: principal.map(encode_session_principal).transpose()?,
        })
    }
}

pub(super) enum SnapshotApplyError {
    Storage(SmPersistenceError),
    /// PostgreSQL has aborted this transaction. Roll it back before reading
    /// the winning allocation on another connection.
    LedgerError {
        error: crate::db::DatabaseError,
        conflict: bool,
    },
}

/// Apply the existing portable snapshot SQL in the caller's transaction.
/// The caller must authorize any new delivery before entering this function;
/// this body never acquires a canonical-source lock after the session gate.
pub(super) async fn apply_snapshot_in_transaction(
    tx: &mut crate::db::Transaction<'_>,
    prepared: &PreparedSnapshot<'_>,
    ledger: &Ledger,
) -> Result<StoreOutcome, SnapshotApplyError> {
    let session = prepared.session;
    // Drop any pre-existing unacked rows for this stream_id
    // BEFORE the inserts so a previous `persist_delete_session`
    // failure (eviction path logs-and-swallows) doesn't trip the
    // `(stream_id, sequence)` PRIMARY KEY constraint on the new
    // inserts and roll the whole transaction back — including
    // the session row update we WANT to commit. (Greptile P1
    // review on PR #405.) The session row's `ON CONFLICT DO
    // UPDATE` already handles its own duplicate; the unacked
    // table has no upsert path because it normally only sees
    // appends. The DELETE here makes the atomic-store call
    // idempotent against partial-prior-state on the same
    // stream_id.
    tx.execute(
        "DELETE FROM sm_unacked WHERE stream_id = ?",
        crate::db_params![session.stream_id.as_str().to_string()],
    )
    .await
    .map_err(|e| SnapshotApplyError::Storage(SmPersistenceError::Other(e.to_string())))?;

    // Refreshes preserve the first durable detach timestamp: traffic cannot
    // renew the existing session's resume window. A later detach after source
    // deletion inserts its own timestamp.
    tx.execute(
        r#"
        INSERT INTO sm_sessions (
            stream_id, user_id, full_jid, occupancy_session, inbound_count, outbound_count,
            last_acked, max_resume_secs, detached_at_ms, max_resume_duration_ms,
            carbons_enabled, roster_interested, blocklist_interested, presence_available,
            presence_show, presence_status, presence_priority, replay_gap_through,
            presence_payloads, bare_jid, auth_context_id, auth_context_version,
            principal_auth_epoch
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT (stream_id) DO UPDATE SET
            user_id = excluded.user_id,
            full_jid = excluded.full_jid,
            occupancy_session = excluded.occupancy_session,
            inbound_count = excluded.inbound_count,
            outbound_count = excluded.outbound_count,
            last_acked = excluded.last_acked,
            max_resume_secs = excluded.max_resume_secs,
            max_resume_duration_ms = excluded.max_resume_duration_ms,
            carbons_enabled = excluded.carbons_enabled,
            roster_interested = excluded.roster_interested,
            blocklist_interested = excluded.blocklist_interested,
            presence_available = excluded.presence_available,
            presence_show = excluded.presence_show,
            presence_status = excluded.presence_status,
            presence_priority = excluded.presence_priority,
            replay_gap_through = excluded.replay_gap_through,
            presence_payloads = excluded.presence_payloads,
            bare_jid = COALESCE(excluded.bare_jid, sm_sessions.bare_jid),
            auth_context_id = COALESCE(excluded.auth_context_id, sm_sessions.auth_context_id),
            auth_context_version = COALESCE(excluded.auth_context_version, sm_sessions.auth_context_version),
            principal_auth_epoch = COALESCE(excluded.principal_auth_epoch, sm_sessions.principal_auth_epoch)
        "#,
        crate::db_params![
            session.stream_id.as_str().to_string(),
            session.user_id.clone(),
            session.jid.to_string(),
            Some(session.occupancy_session.to_string()),
            i64::from(session.inbound_count),
            i64::from(session.outbound_count),
            i64::from(session.last_acked),
            session.max_resume_time.map(i64::from),
            prepared.detached_at_ms,
            prepared.max_resume_duration_ms,
            i64::from(session.carbons_enabled),
            i64::from(session.roster_interested),
            i64::from(session.blocklist_interested),
            i64::from(session.presence_available),
            prepared.presence_show_str.map(str::to_string),
            session.presence_status.clone(),
            i64::from(session.presence_priority),
            session.replay_gap_through.map(i64::from),
            prepared.presence_payloads_xml.clone(),
            prepared.encoded_principal
                .as_ref()
                .map(|principal| principal.bare_jid.clone()),
            prepared.encoded_principal
                .as_ref()
                .map(|principal| principal.auth_context_id.clone()),
            prepared.encoded_principal
                .as_ref()
                .map(|principal| principal.auth_context_version),
            prepared.encoded_principal
                .as_ref()
                .map(|principal| principal.principal_auth_epoch),
        ],
    )
    .await
    .map_err(|e| SnapshotApplyError::Storage(SmPersistenceError::Other(e.to_string())))?;

    for stanza in prepared.unacked {
        let xml = serialize_stanza(&stanza.stanza).map_err(SnapshotApplyError::Storage)?;
        let receipt_ms = stanza.original_receipt_at.timestamp_millis();
        tx.execute(
            "INSERT INTO sm_unacked (stream_id, sequence, stanza_xml, original_receipt_at_ms, ingress_receipts, provenance) \
             VALUES (?, ?, ?, ?, ?, ?)",
            crate::db_params![
                stanza.stream_id.as_str().to_string(),
                i64::from(stanza.sequence),
                xml,
                receipt_ms,
                crate::sm_persistence::codec::encode_ingress_receipts(&stanza.ingress_receipts),
                stanza.provenance.encode(),
            ],
        )
        .await
        .map_err(|e| SnapshotApplyError::Storage(SmPersistenceError::Other(e.to_string())))?;
    }

    let mut withheld = Vec::new();
    if let Ledger::Drained(appends) = &ledger {
        withheld = ingress_append::insert_or_withhold(tx, appends)
            .await
            .map_err(|error| {
                SnapshotApplyError::Storage(SmPersistenceError::Other(error.to_string()))
            })?;
    }
    if let Ledger::Exclusive(append) | Ledger::NewDelivery(append) = ledger {
        if let Err(error) = ingress_append::insert(tx, append).await {
            let conflict = ingress_append::is_ledger_conflict(&error);
            return Err(SnapshotApplyError::LedgerError { error, conflict });
        }
    }
    Ok(StoreOutcome::Committed { withheld })
}
