//! Outcome ledger for enable, resume and admission operations.
//!
//! One row per caller-minted operation id records whether the operation
//! committed. The primary key lets exactly one of a commit and an
//! abandonment marker exist, which is what resolves an unknown commit.

use chrono::{DateTime, Utc};
use uuid::Uuid;
use waddle_xmpp::ownership::{SessionGeneration, SmSessionKey};
use waddle_xmpp::stream_management::durable::SmOperationId;

use super::error::DurableSmError;
use crate::db::Transaction;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OperationKind {
    Enable,
    Resume,
    Admission,
}

impl OperationKind {
    const fn to_storage(self) -> i64 {
        match self {
            Self::Enable => 1,
            Self::Resume => 2,
            Self::Admission => 3,
        }
    }

    fn from_storage(value: i64) -> Result<Self, DurableSmError> {
        match value {
            1 => Ok(Self::Enable),
            2 => Ok(Self::Resume),
            3 => Ok(Self::Admission),
            _ => Err(DurableSmError::CorruptRecord {
                field: "operation kind",
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OperationOutcome {
    /// The operation's transaction committed its effects with this row.
    Committed,
    /// A resolver recorded that the operation will never commit.
    Abandoned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct OperationRow {
    pub(super) session: SmSessionKey,
    pub(super) kind: OperationKind,
    pub(super) outcome: OperationOutcome,
    /// The generation the operation committed, or the generation it was
    /// abandoned against.
    pub(super) generation: SessionGeneration,
}

/// Record a committed operation in the transaction that carries its effects.
pub(super) async fn record_committed(
    tx: &mut Transaction<'_>,
    operation: SmOperationId,
    session: SmSessionKey,
    kind: OperationKind,
    generation: SessionGeneration,
    now: DateTime<Utc>,
) -> Result<(), DurableSmError> {
    tx.execute(
        "INSERT INTO sm_durable_operations \
         (operation_id, session_id, kind, outcome, generation, ordinal, recorded_at_ms) \
         VALUES (?, ?, ?, 1, ?, NULL, ?)",
        crate::db_params![
            operation.as_uuid().to_string(),
            session.as_uuid().to_string(),
            kind.to_storage(),
            generation_column(generation),
            now.timestamp_millis(),
        ],
    )
    .await?;
    Ok(())
}

/// Insert an abandonment marker unless the operation already has a row, and
/// return the row that exists afterwards. While the operation's own
/// transaction still holds the key uncommitted, PostgreSQL blocks this insert
/// until that transaction ends or the lock wait bound fails the statement.
pub(super) async fn abandon(
    tx: &mut Transaction<'_>,
    operation: SmOperationId,
    session: SmSessionKey,
    kind: OperationKind,
    generation: SessionGeneration,
    now: DateTime<Utc>,
) -> Result<OperationRow, DurableSmError> {
    tx.execute(
        "INSERT INTO sm_durable_operations \
         (operation_id, session_id, kind, outcome, generation, ordinal, recorded_at_ms) \
         VALUES (?, ?, ?, 2, ?, NULL, ?) ON CONFLICT (operation_id) DO NOTHING",
        crate::db_params![
            operation.as_uuid().to_string(),
            session.as_uuid().to_string(),
            kind.to_storage(),
            generation_column(generation),
            now.timestamp_millis(),
        ],
    )
    .await?;
    find(tx, operation)
        .await?
        .ok_or(DurableSmError::CorruptRecord {
            field: "operation outcome",
        })
}

pub(super) async fn find(
    tx: &mut Transaction<'_>,
    operation: SmOperationId,
) -> Result<Option<OperationRow>, DurableSmError> {
    let mut rows = tx
        .query(
            "SELECT session_id, kind, outcome, generation FROM sm_durable_operations \
             WHERE operation_id = ?",
            crate::db_params![operation.as_uuid().to_string()],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let session =
        Uuid::parse_str(&row.get::<String>(0)?).map_err(|_| DurableSmError::CorruptRecord {
            field: "operation session",
        })?;
    let outcome = match row.get::<i64>(2)? {
        1 => OperationOutcome::Committed,
        2 => OperationOutcome::Abandoned,
        _ => {
            return Err(DurableSmError::CorruptRecord {
                field: "operation outcome",
            })
        }
    };
    let generation = u64::try_from(row.get::<i64>(3)?)
        .ok()
        .and_then(SessionGeneration::new)
        .ok_or(DurableSmError::CorruptRecord {
            field: "operation generation",
        })?;
    Ok(Some(OperationRow {
        session: SmSessionKey::from_uuid(session),
        kind: OperationKind::from_storage(row.get(1)?)?,
        outcome,
        generation,
    }))
}

/// Generations are at most `i64::MAX`, so the conversion cannot truncate.
pub(super) fn generation_column(generation: SessionGeneration) -> i64 {
    generation.value() as i64
}
