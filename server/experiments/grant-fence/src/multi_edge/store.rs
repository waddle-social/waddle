use anyhow::{ensure, Result};
use sqlx::{PgConnection, Postgres, Row, Transaction};

use super::model::{Control, Guard};

pub fn matches(row: &sqlx::postgres::PgRow, guard: &Guard) -> Result<bool> {
    Ok(row.try_get::<i64, _>("generation")? == guard.generation
        && row.try_get::<uuid::Uuid, _>("operation")? == guard.operation
        && row.try_get::<String, _>("payload")? == serde_json::to_string(&guard.payload)?)
}
pub async fn insert(connection: &mut PgConnection, guard: &Guard) -> Result<()> {
    sqlx::query("INSERT INTO multi_guard(kind,id,generation,operation,payload,active) VALUES($1,$2,$3,$4,$5,false)").bind(guard.kind().sql()).bind(guard.id).bind(guard.generation).bind(guard.operation).bind(serde_json::to_string(&guard.payload)?).execute(connection).await?;
    Ok(())
}
pub async fn activate(connection: &mut PgConnection, guard: &Guard) -> Result<bool> {
    Ok(sqlx::query("UPDATE multi_guard SET active=true WHERE kind=$1 AND id=$2 AND generation=$3 AND operation=$4 AND payload=$5 AND active=false").bind(guard.kind().sql()).bind(guard.id).bind(guard.generation).bind(guard.operation).bind(serde_json::to_string(&guard.payload)?).execute(connection).await?.rows_affected() == 1)
}
pub async fn close(
    tx: &mut Transaction<'_, Postgres>,
    previous: &Guard,
    next: &Guard,
) -> Result<bool> {
    ensure!(
        previous.id == next.id
            && previous.kind() == next.kind()
            && next.generation
                == previous
                    .generation
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("guard overflow"))?
            && next.operation != previous.operation,
        "mutation identity does not advance"
    );
    let row = sqlx::query("SELECT * FROM multi_guard WHERE kind=$1 AND id=$2 FOR UPDATE")
        .bind(previous.kind().sql())
        .bind(previous.id)
        .fetch_optional(&mut **tx)
        .await?;
    if !row
        .as_ref()
        .map(|row| matches(row, previous))
        .transpose()?
        .unwrap_or(false)
    {
        return Ok(false);
    }
    sqlx::query("UPDATE multi_guard SET generation=$3,operation=$4,payload=$5,active=false WHERE kind=$1 AND id=$2").bind(next.kind().sql()).bind(next.id).bind(next.generation).bind(next.operation).bind(serde_json::to_string(&next.payload)?).execute(&mut **tx).await?;
    Ok(true)
}
pub async fn lock_validate(
    tx: &mut Transaction<'_, Postgres>,
    guards: &[Guard],
    omit: Control,
) -> Result<bool> {
    let mut ordered: Vec<_> = guards
        .iter()
        .filter(|guard| omit != Control::OmitGuard(guard.kind()))
        .collect();
    ordered.sort_by_key(|guard| (guard.kind(), guard.id));
    for guard in ordered {
        let row = sqlx::query("SELECT * FROM multi_guard WHERE kind=$1 AND id=$2 FOR SHARE")
            .bind(guard.kind().sql())
            .bind(guard.id)
            .fetch_optional(&mut **tx)
            .await?;
        let Some(row) = row else {
            return Ok(false);
        };
        let identity_matches = if omit == Control::OmitVersionStamp(guard.kind()) {
            // Still lock the stable keyed row and check current target/polarity.
            // Only generation + operation version identity is deliberately ignored.
            row.try_get::<String, _>("payload")? == serde_json::to_string(&guard.payload)?
        } else {
            matches(&row, guard)?
        };
        if !identity_matches || !row.try_get::<bool, _>("active")? {
            return Ok(false);
        }
    }
    Ok(true)
}
pub async fn active(connection: &mut PgConnection, guard: &Guard) -> Result<bool> {
    let row = sqlx::query("SELECT * FROM multi_guard WHERE kind=$1 AND id=$2")
        .bind(guard.kind().sql())
        .bind(guard.id)
        .fetch_one(connection)
        .await?;
    Ok(matches(&row, guard)? && row.try_get::<bool, _>("active")?)
}
