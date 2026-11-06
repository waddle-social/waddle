//! Recipient-wide route interlock for the unregistered SM custody candidate.
//!
//! This guard only coordinates candidate transactions. Guard rows have no
//! quota or retirement protocol, and the disposable runtime fixture has direct
//! DML grants that can seed or close them. The production SM persistence
//! writers do not use this schema, so it cannot authorize real pending-delivery
//! acceptance or claim to fence legacy writers.

use jid::BareJid;
use sqlx::Row;

use super::repository::pg;
use super::types::db;
use super::CustodyError;
use crate::ingress_uow::IngressUowTransaction;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecipientRouteState {
    Open,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LockedRecipientRoute {
    recipient: BareJid,
    state: RecipientRouteState,
    revision: i64,
}

/// Acquires the durable recipient row after canonical-source locks, then
/// rechecks its current state while retaining the lock through transaction
/// completion. READ COMMITTED plus `FOR UPDATE` observes a transition that
/// committed while this operation waited.
pub(super) async fn acquire(
    tx: &mut IngressUowTransaction<'_>,
    schema: &str,
    recipient: &BareJid,
    require_open: bool,
) -> Result<(), CustodyError> {
    let route = lock(tx, schema, recipient).await?;
    recheck(tx, schema, &route, require_open).await
}

async fn lock(
    tx: &mut IngressUowTransaction<'_>,
    schema: &str,
    recipient: &BareJid,
) -> Result<LockedRecipientRoute, CustodyError> {
    sqlx::query(&format!(
        "INSERT INTO {schema}.sm_custody_recipient_route_guards (recipient) VALUES ($1) ON CONFLICT DO NOTHING"
    ))
    .bind(recipient.to_string())
    .execute(pg(tx)?)
    .await
    .map_err(db)?;

    let row = sqlx::query(&format!(
        "SELECT state, revision FROM {schema}.sm_custody_recipient_route_guards WHERE recipient = $1 FOR UPDATE"
    ))
    .bind(recipient.to_string())
    .fetch_optional(pg(tx)?)
    .await
    .map_err(db)?
    .ok_or(CustodyError::Rejected)?;
    let state = decode_state(row.try_get(0).map_err(db)?)?;
    let revision = row.try_get(1).map_err(db)?;
    Ok(LockedRecipientRoute {
        recipient: recipient.clone(),
        state,
        revision,
    })
}

async fn recheck(
    tx: &mut IngressUowTransaction<'_>,
    schema: &str,
    locked: &LockedRecipientRoute,
    require_open: bool,
) -> Result<(), CustodyError> {
    let row = sqlx::query(&format!(
        "SELECT state, revision FROM {schema}.sm_custody_recipient_route_guards WHERE recipient = $1"
    ))
    .bind(locked.recipient.to_string())
    .fetch_optional(pg(tx)?)
    .await
    .map_err(db)?
    .ok_or(CustodyError::Rejected)?;
    let current = LockedRecipientRoute {
        recipient: locked.recipient.clone(),
        state: decode_state(row.try_get(0).map_err(db)?)?,
        revision: row.try_get(1).map_err(db)?,
    };
    if current != *locked || (require_open && current.state != RecipientRouteState::Open) {
        return Err(CustodyError::Rejected);
    }
    Ok(())
}

fn decode_state(state: i16) -> Result<RecipientRouteState, CustodyError> {
    match state {
        0 => Ok(RecipientRouteState::Open),
        1 => Ok(RecipientRouteState::Closed),
        _ => Err(CustodyError::InvalidStored),
    }
}
