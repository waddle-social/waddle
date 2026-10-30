use std::io::Read;

use anyhow::{ensure, Context, Result};
use sqlx::Connection;
use uuid::Uuid;

use crate::authority::{activate, check, client, delete_projection, guard_matches, publish};
use crate::model::{barrier, event, Action, Work};

pub async fn run_worker() -> Result<()> {
    let mut bytes = Vec::new();
    std::io::stdin().take(4097).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 4096, "fixture worker input exceeds PIPE_BUF");
    let work: Work = serde_json::from_slice(&bytes)?;
    work.settings.validate()?;
    ensure!(
        work.barriers.starts_with(&work.settings.root),
        "barriers must be fixture-owned"
    );
    match work.action {
        Action::Admit {
            guarded,
            pause_after_check,
            hold_lock,
            lock_ms,
        } => admit(&work, guarded, pause_after_check, hold_lock, lock_ms).await,
        Action::Revoke { hold_lock } => revoke(&work, hold_lock).await,
        Action::Publish { pause_before_write } => stale_publish(&work, pause_before_write).await,
    }
}

async fn admit(
    work: &Work,
    guarded: bool,
    pause_after_check: bool,
    hold_lock: bool,
    lock_ms: u32,
) -> Result<()> {
    let authority_result = async { check(&client(&work.settings).await?, &work.guard).await }.await;
    match authority_result {
        Ok(true) => {}
        Ok(false) => return event(&work.barriers, "done", "denied_spicedb", None),
        Err(_) => return event(&work.barriers, "done", "denied_authority_unavailable", None),
    }
    event(&work.barriers, "checked", "allowed", None)?;
    if pause_after_check {
        barrier(&work.barriers, "after-check").await?;
    }
    let mut connection = work
        .settings
        .connect("fence_app", "fence_app", "fence-admission", lock_ms)
        .await?;
    let backend_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut connection)
        .await?;
    let mut tx = connection.begin().await?;
    event(
        &work.barriers,
        "attempting",
        "admission_lock",
        Some(backend_pid),
    )?;
    if guarded {
        let row = match sqlx::query("SELECT * FROM scope_guard WHERE scope=$1 FOR SHARE")
            .bind(work.guard.scope)
            .fetch_optional(&mut *tx)
            .await
        {
            Ok(row) => row,
            Err(sqlx::Error::Database(error)) if error.code().as_deref() == Some("55P03") => {
                return event(
                    &work.barriers,
                    "done",
                    "denied_lock_timeout",
                    Some(backend_pid),
                );
            }
            Err(error) => return Err(error).context("guard admission query"),
        };
        if !row
            .as_ref()
            .map(|row| guard_matches(row, &work.guard))
            .transpose()?
            .unwrap_or(false)
        {
            tx.rollback().await?;
            return event(&work.barriers, "done", "denied_fence", Some(backend_pid));
        }
    }
    sqlx::query(
        "INSERT INTO admitted_effect(attempt,scope,incarnation,generation) VALUES($1,$2,$3,$4)",
    )
    .bind(work.attempt)
    .bind(work.guard.scope)
    .bind(work.guard.incarnation)
    .bind(work.guard.generation)
    .execute(&mut *tx)
    .await?;
    if hold_lock {
        event(
            &work.barriers,
            "held",
            "admission_uncommitted",
            Some(backend_pid),
        )?;
        barrier(&work.barriers, "release").await?;
    }
    tx.commit().await?;
    event(&work.barriers, "done", "admitted", Some(backend_pid))
}

async fn revoke(work: &Work, hold_lock: bool) -> Result<()> {
    let mut connection = work
        .settings
        .connect("fence_app", "fence_app", "fence-revoke", 3000)
        .await?;
    let backend_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut connection)
        .await?;
    let mut tx = connection.begin().await?;
    event(
        &work.barriers,
        "attempting",
        "revoke_lock",
        Some(backend_pid),
    )?;
    let row = sqlx::query("SELECT * FROM scope_guard WHERE scope=$1 FOR UPDATE")
        .bind(work.guard.scope)
        .fetch_one(&mut *tx)
        .await?;
    ensure!(
        guard_matches(&row, &work.guard)?,
        "revoker must hold exact active guard"
    );
    let next = work
        .guard
        .generation
        .checked_add(1)
        .context("generation overflow")?;
    sqlx::query("UPDATE scope_guard SET generation=$2,operation=$3,active=false,desired=false WHERE scope=$1")
        .bind(work.guard.scope).bind(next).bind(work.attempt).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO revocation(operation,scope,incarnation,previous_generation,closed_generation) VALUES($1,$2,$3,$4,$5)")
        .bind(work.attempt).bind(work.guard.scope).bind(work.guard.incarnation).bind(work.guard.generation).bind(next).execute(&mut *tx).await?;
    if hold_lock {
        event(
            &work.barriers,
            "held",
            "revoke_uncommitted",
            Some(backend_pid),
        )?;
        barrier(&work.barriers, "release").await?;
    }
    tx.commit().await?;
    event(&work.barriers, "closed", "durable_fence", Some(backend_pid))?;
    delete_projection(&client(&work.settings).await?, &work.guard).await?;
    event(
        &work.barriers,
        "done",
        "revoked_and_projection_deleted",
        Some(backend_pid),
    )
}

async fn stale_publish(work: &Work, pause_before_write: bool) -> Result<()> {
    event(&work.barriers, "ready", "old_projection_captured", None)?;
    if pause_before_write {
        barrier(&work.barriers, "publish").await?;
    }
    publish(&client(&work.settings).await?, &work.guard).await?;
    let mut connection = work
        .settings
        .connect("fence_app", "fence_app", "fence-stale-publisher", 3000)
        .await?;
    let activated = activate(&mut connection, &work.guard).await?;
    event(
        &work.barriers,
        "done",
        if activated {
            "activated"
        } else {
            "stale_publication_rejected"
        },
        None,
    )
}

pub fn fresh_guard() -> crate::model::Guard {
    crate::model::Guard {
        scope: Uuid::new_v4(),
        incarnation: Uuid::new_v4(),
        generation: 1,
        operation: Uuid::new_v4(),
    }
}
