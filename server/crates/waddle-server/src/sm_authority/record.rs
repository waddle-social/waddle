//! The stored authority record and its row codec.

use std::time::Duration;

use chrono::{DateTime, Utc};
use jid::FullJid;
use uuid::Uuid;
use waddle_xmpp::auth::{
    AuthContextId, AuthContextVersion, AuthenticatedPrincipalRef, PrincipalAuthEpoch,
};
use waddle_xmpp::ownership::{BindingRef, SessionGeneration, SmSessionKey};
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::durable::{
    DurableSmState, OutboundWindow, OwnerFence, SmOperationId, SmOrdinal, SmResumeTokenDigest,
    TerminalReason,
};
use waddle_xmpp::stream_management::persistence::SmClaimFence;

use super::error::DurableSmError;
use crate::db::{DatabaseDriver, Row, Transaction, Value};

pub(super) const COLUMNS: &str = "session_id, full_jid, state, terminal_reason, generation, \
    binding, bare_jid, auth_context_id, auth_context_version, principal_auth_epoch, \
    owner_node_id, owner_node_epoch, claim_epoch, inbound_handled, outbound_acked, \
    outbound_exposure_ceiling, outbound_appended, outbound_unacked_bytes, max_resume_secs, \
    detached_at_ms, enable_operation, last_operation";

/// The claim a generation was committed under on a clustered node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedClaim {
    pub node_id: String,
    pub node_epoch: String,
    pub claim_epoch: i64,
}

/// One `sm_durable_sessions` row as typed values.
///
/// A record is evidence read from storage. Holding or constructing one
/// authorizes nothing: every store operation reloads the row under its lock
/// and compares the caller's fence there. It holds no resume token; the row
/// stores only the token's digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableSessionRecord {
    pub session: SmSessionKey,
    pub full_jid: FullJid,
    pub state: DurableSmState,
    pub generation: SessionGeneration,
    /// The bind or resume attempt that owns `generation`.
    pub binding: BindingRef,
    pub principal: AuthenticatedPrincipalRef,
    pub claim: Option<RecordedClaim>,
    /// Handled count of client stanzas, as last reported by the owner.
    pub inbound_handled: u64,
    pub window: OutboundWindow,
    /// Payload bytes of the retained frames.
    pub unacked_bytes: u64,
    pub max_resume: Duration,
    /// When the session first lost its transport, if it has.
    pub detached_at: Option<DateTime<Utc>>,
    pub enable_operation: SmOperationId,
    pub last_operation: SmOperationId,
}

impl RecordedClaim {
    pub(super) fn from_fence(claim: &SmClaimFence) -> Self {
        Self {
            node_id: claim.owner().node_id.clone(),
            node_epoch: claim.owner().node_epoch.clone(),
            claim_epoch: claim.epoch().0,
        }
    }
}

impl DurableSessionRecord {
    /// The owner fence the stored generation requires.
    pub fn fence(&self) -> OwnerFence {
        OwnerFence {
            session: self.session,
            generation: self.generation,
            binding: self.binding.clone(),
        }
    }
}

fn decode_state(state: i64, reason: Option<i64>) -> Result<DurableSmState, DurableSmError> {
    Ok(match (state, reason) {
        (1, None) => DurableSmState::Live,
        (2, None) => DurableSmState::Detached,
        (3, None) => DurableSmState::TransferHydrating,
        (4, Some(1)) => DurableSmState::Terminal(TerminalReason::EnableAbandoned),
        (4, Some(2)) => DurableSmState::Terminal(TerminalReason::Expired),
        (4, Some(3)) => DurableSmState::Terminal(TerminalReason::Replaced),
        (4, Some(4)) => DurableSmState::Terminal(TerminalReason::Closed),
        _ => return Err(DurableSmError::CorruptRecord { field: "state" }),
    })
}

fn corrupt(field: &'static str) -> DurableSmError {
    DurableSmError::CorruptRecord { field }
}

fn unsigned(value: i64, field: &'static str) -> Result<u64, DurableSmError> {
    u64::try_from(value).map_err(|_| corrupt(field))
}

fn ordinal(value: i64, field: &'static str) -> Result<SmOrdinal, DurableSmError> {
    SmOrdinal::new(unsigned(value, field)?).ok_or(corrupt(field))
}

fn uuid(value: &str, field: &'static str) -> Result<Uuid, DurableSmError> {
    Uuid::parse_str(value).map_err(|_| corrupt(field))
}

pub(super) fn decode(row: &Row) -> Result<DurableSessionRecord, DurableSmError> {
    let session = SmSessionKey::from_uuid(uuid(&row.get::<String>(0)?, "session_id")?);
    let full_jid = row
        .get::<String>(1)?
        .parse::<FullJid>()
        .map_err(|_| corrupt("full_jid"))?;
    let state = decode_state(row.get(2)?, row.get(3)?)?;
    let generation = SessionGeneration::new(unsigned(row.get(4)?, "generation")?)
        .ok_or(corrupt("generation"))?;
    let binding = serde_json::from_str::<BindingRef>(&row.get::<String>(5)?)
        .map_err(|_| corrupt("binding"))?;
    let principal = AuthenticatedPrincipalRef::new(
        row.get::<String>(6)?
            .parse()
            .map_err(|_| corrupt("bare_jid"))?,
        AuthContextId::new(uuid(&row.get::<String>(7)?, "auth_context_id")?),
        AuthContextVersion::new(unsigned(row.get(8)?, "auth_context_version")?),
        PrincipalAuthEpoch::new(unsigned(row.get(9)?, "principal_auth_epoch")?),
    );
    let claim = match (
        row.get::<Option<String>>(10)?,
        row.get::<Option<String>>(11)?,
        row.get::<Option<i64>>(12)?,
    ) {
        (None, None, None) => None,
        (Some(node_id), Some(node_epoch), Some(claim_epoch)) => Some(RecordedClaim {
            node_id,
            node_epoch,
            claim_epoch,
        }),
        _ => return Err(corrupt("claim")),
    };
    let window = OutboundWindow::new(
        ordinal(row.get(14)?, "outbound_acked")?,
        ordinal(row.get(15)?, "outbound_exposure_ceiling")?,
        ordinal(row.get(16)?, "outbound_appended")?,
    )
    .map_err(|_| corrupt("outbound window"))?;
    let detached_at = row
        .get::<Option<i64>>(19)?
        .map(|millis| {
            DateTime::<Utc>::from_timestamp_millis(millis).ok_or(corrupt("detached_at_ms"))
        })
        .transpose()?;
    Ok(DurableSessionRecord {
        session,
        full_jid,
        state,
        generation,
        binding,
        principal,
        claim,
        inbound_handled: unsigned(row.get(13)?, "inbound_handled")?,
        window,
        unacked_bytes: unsigned(row.get(17)?, "outbound_unacked_bytes")?,
        max_resume: Duration::from_secs(unsigned(row.get(18)?, "max_resume_secs")?),
        detached_at,
        enable_operation: SmOperationId::from_uuid(uuid(
            &row.get::<String>(20)?,
            "enable_operation",
        )?),
        last_operation: SmOperationId::from_uuid(uuid(&row.get::<String>(21)?, "last_operation")?),
    })
}

/// Load a session without locking it.
pub(super) async fn load(
    tx: &mut Transaction<'_>,
    session: SmSessionKey,
) -> Result<Option<DurableSessionRecord>, DurableSmError> {
    let mut rows = tx
        .query(
            &format!("SELECT {COLUMNS} FROM sm_durable_sessions WHERE session_id = ?"),
            crate::db_params![session.as_uuid().to_string()],
        )
        .await?;
    rows.next().await?.as_ref().map(decode).transpose()
}

/// The `stream_id` under which a session's `sm_sessions` and `sm_unacked`
/// rows are stored, and the id of its `sm_session` claim.
pub(super) fn stream_id(session: SmSessionKey) -> SmSessionId {
    SmSessionId::new(session.as_uuid().to_string())
}

fn digest_column(digest: &SmResumeTokenDigest) -> String {
    hex::encode(digest.as_bytes())
}

fn state_columns(state: DurableSmState) -> Result<(i64, Option<i64>), DurableSmError> {
    Ok(match state {
        // `Enabling` precedes the commit that inserts the row.
        DurableSmState::Enabling => return Err(corrupt("state")),
        DurableSmState::Live => (1, None),
        DurableSmState::Detached => (2, None),
        DurableSmState::TransferHydrating => (3, None),
        DurableSmState::Terminal(reason) => (
            4,
            Some(match reason {
                TerminalReason::EnableAbandoned => 1,
                TerminalReason::Expired => 2,
                TerminalReason::Replaced => 3,
                TerminalReason::Closed => 4,
            }),
        ),
    })
}

fn signed(value: u64, field: &'static str) -> Result<i64, DurableSmError> {
    i64::try_from(value).map_err(|_| corrupt(field))
}

/// The mutable columns of a record, in the order `insert` and `update` bind
/// them.
fn mutable_params(
    stored: &DurableSessionRecord,
    now: DateTime<Utc>,
) -> Result<Vec<Value>, DurableSmError> {
    let (state, terminal_reason) = state_columns(stored.state)?;
    let binding = serde_json::to_string(&stored.binding).map_err(|_| corrupt("binding"))?;
    Ok(crate::db_params![
        state,
        terminal_reason,
        signed(stored.generation.value(), "generation")?,
        binding,
        stored.principal.bare_jid().to_string(),
        stored.principal.auth_context_id().as_uuid().to_string(),
        signed(
            stored.principal.auth_context_version().get(),
            "auth_context_version"
        )?,
        signed(stored.principal.auth_epoch().get(), "principal_auth_epoch")?,
        stored.claim.as_ref().map(|claim| claim.node_id.clone()),
        stored.claim.as_ref().map(|claim| claim.node_epoch.clone()),
        stored.claim.as_ref().map(|claim| claim.claim_epoch),
        signed(stored.inbound_handled, "inbound_handled")?,
        signed(stored.window.acked().value(), "outbound_acked")?,
        signed(
            stored.window.exposure_ceiling().value(),
            "outbound_exposure_ceiling"
        )?,
        signed(stored.window.appended().value(), "outbound_appended")?,
        signed(stored.unacked_bytes, "outbound_unacked_bytes")?,
        signed(stored.max_resume.as_secs(), "max_resume_secs")?,
        stored.detached_at.map(|at| at.timestamp_millis()),
        stored.last_operation.as_uuid().to_string(),
        now.timestamp_millis(),
    ])
}

/// Insert a new session row. The unique digest index rejects a token that
/// is already stored.
pub(super) async fn insert(
    tx: &mut Transaction<'_>,
    stored: &DurableSessionRecord,
    digest: &SmResumeTokenDigest,
    now: DateTime<Utc>,
) -> Result<(), DurableSmError> {
    let mut params = crate::db_params![
        stored.session.as_uuid().to_string(),
        digest_column(digest),
        stored.full_jid.to_string(),
        stored.enable_operation.as_uuid().to_string(),
    ];
    params.extend(mutable_params(stored, now)?);
    tx.execute(
        "INSERT INTO sm_durable_sessions (session_id, resume_token_sha256, full_jid, \
         enable_operation, state, terminal_reason, generation, binding, bare_jid, \
         auth_context_id, auth_context_version, principal_auth_epoch, owner_node_id, \
         owner_node_epoch, claim_epoch, inbound_handled, outbound_acked, \
         outbound_exposure_ceiling, outbound_appended, outbound_unacked_bytes, \
         max_resume_secs, detached_at_ms, last_operation, updated_at_ms) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params,
    )
    .await?;
    Ok(())
}

pub(super) async fn token_exists(
    tx: &mut Transaction<'_>,
    digest: &SmResumeTokenDigest,
) -> Result<bool, DurableSmError> {
    let mut rows = tx
        .query(
            "SELECT 1 FROM sm_durable_sessions WHERE resume_token_sha256 = ?",
            crate::db_params![digest_column(digest)],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

/// Row lock suffix: PostgreSQL locks the selected row; SQLite's immediate
/// transaction already excludes every other writer.
fn for_update(driver: DatabaseDriver) -> &'static str {
    match driver {
        DatabaseDriver::Postgres => " FOR UPDATE",
        DatabaseDriver::Sqlite => "",
    }
}

/// Load a session and lock its row for the rest of the transaction.
pub(super) async fn load_for_update(
    tx: &mut Transaction<'_>,
    session: SmSessionKey,
) -> Result<Option<DurableSessionRecord>, DurableSmError> {
    let lock = for_update(tx.driver());
    let mut rows = tx
        .query(
            &format!("SELECT {COLUMNS} FROM sm_durable_sessions WHERE session_id = ?{lock}"),
            crate::db_params![session.as_uuid().to_string()],
        )
        .await?;
    rows.next().await?.as_ref().map(decode).transpose()
}

/// Write every mutable column of a locked session row.
pub(super) async fn update(
    tx: &mut Transaction<'_>,
    stored: &DurableSessionRecord,
    now: DateTime<Utc>,
) -> Result<(), DurableSmError> {
    let mut params = mutable_params(stored, now)?;
    params.push(Value::from(stored.session.as_uuid().to_string()));
    let changed = tx
        .execute(
            "UPDATE sm_durable_sessions SET state = ?, terminal_reason = ?, generation = ?, \
             binding = ?, bare_jid = ?, auth_context_id = ?, auth_context_version = ?, \
             principal_auth_epoch = ?, owner_node_id = ?, owner_node_epoch = ?, \
             claim_epoch = ?, inbound_handled = ?, outbound_acked = ?, \
             outbound_exposure_ceiling = ?, outbound_appended = ?, outbound_unacked_bytes = ?, \
             max_resume_secs = ?, detached_at_ms = ?, last_operation = ?, updated_at_ms = ? \
             WHERE session_id = ?",
            params,
        )
        .await?;
    if changed != 1 {
        return Err(corrupt("session row"));
    }
    Ok(())
}
