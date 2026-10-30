use anyhow::{ensure, Result};
use sqlx::Connection;

use super::contender::observe;
use super::graph;
use super::model::{Action, Control, Effect, Outcome, Pause, Work};
use super::store;
use crate::authority::client;
use crate::model::barrier;

pub async fn execute(work: &Work) -> Result<()> {
    // Includes barrier waits, graph calls and SQL transport, not just server-side
    // statement timeouts. Cancellation drops owned connections/transactions.
    tokio::time::timeout(std::time::Duration::from_secs(30), execute_bounded(work))
        .await
        .map_err(|_| anyhow::anyhow!("multi-edge worker action deadline"))?
}
async fn execute_bounded(work: &Work) -> Result<()> {
    work.settings.validate()?;
    ensure!(
        work.barriers.starts_with(&work.settings.root),
        "barriers must be fixture owned"
    );
    let lock_ms = match &work.action {
        Action::Admit { lock_ms, .. } => *lock_ms,
        _ => 3000,
    };
    let mut db = work
        .settings
        .connect("fence_app", "fence_app", "multi-edge-worker", lock_ms)
        .await?;
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut db)
        .await?;
    let result = match &work.action {
        Action::Admit {
            effect,
            omit,
            pause,
            ..
        } => admit(work, &mut db, pid, effect, *omit, *pause).await,
        Action::Mutate {
            previous,
            next,
            principal,
            pause,
        } => {
            let mut tx = db.begin().await?;
            observe(work, "attempting", None, pid)?;
            if !store::close(&mut tx, previous, next).await? {
                tx.rollback().await?;
                return observe(work, "done", Some(Outcome::Superseded), pid);
            }
            pause_at(work, *pause, Pause::MutationLocked, "held", pid).await?;
            tx.commit().await?;
            pause_at(work, *pause, Pause::Closed, "closed", pid).await?;
            let graph = client(&work.settings).await?;
            graph::publish(&graph, Some(previous), next, *principal).await?;
            pause_at(work, *pause, Pause::Published, "published", pid).await?;
            if graph::verify_mutation(&graph, next, *principal).await?
                && store::activate(&mut db, next).await?
            {
                Ok(Outcome::Activated)
            } else {
                Ok(Outcome::Superseded)
            }
        }
    };
    let outcome = match result {
        Ok(outcome) => outcome,
        Err(error) if error.chain().any(|cause|cause.downcast_ref::<sqlx::Error>().is_some_and(|error|matches!(error,sqlx::Error::Database(db) if db.code().as_deref() == Some("55P03")))) => Outcome::LockTimeout,
        Err(error) => return Err(error),
    };
    observe(work, "done", Some(outcome), pid)
}
async fn admit(
    work: &Work,
    db: &mut sqlx::PgConnection,
    pid: i32,
    effect: &Effect,
    omit: Control,
    pause: Pause,
) -> Result<Outcome> {
    graph::validate(&effect.witness)?;
    // Serialize on the unique effect identity before dependency locks. This is
    // provisional inside this transaction, not committed authorization.
    let mut tx = db.begin().await?;
    observe(work, "attempting", None, pid)?;
    let descriptor = serde_json::to_string(effect)?;
    let inserted = sqlx::query(
        "INSERT INTO multi_effect(effect,descriptor) VALUES($1,$2) ON CONFLICT(effect) DO NOTHING",
    )
    .bind(effect.key)
    .bind(&descriptor)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        == 1;
    if !inserted {
        let prior: String =
            sqlx::query_scalar("SELECT descriptor FROM multi_effect WHERE effect=$1")
                .bind(effect.key)
                .fetch_one(&mut *tx)
                .await?;
        tx.rollback().await?;
        return Ok(if prior == descriptor {
            Outcome::AlreadyCommitted
        } else {
            Outcome::Conflict
        });
    }
    pause_at(work, pause, Pause::Reserved, "reserved", pid).await?;
    let graph_client = client(&work.settings).await?;
    let Some(revision) = tokio::time::timeout(
        crate::model::RPC_BUDGET,
        graph::anchor(&graph_client, &effect.witness.guards[0]),
    )
    .await??
    else {
        tx.rollback().await?;
        return Ok(Outcome::DeniedGraph);
    };
    pause_at(work, pause, Pause::Anchored, "anchored", pid).await?;
    let checked = graph::witness(&graph_client, &effect.witness, revision).await;
    if !matches!(checked, Ok(true)) {
        tx.rollback().await?;
        return Ok(Outcome::DeniedGraph);
    }
    pause_at(work, pause, Pause::Checked, "checked", pid).await?;
    if !store::lock_validate(&mut tx, &effect.witness.guards, omit).await? {
        tx.rollback().await?;
        return Ok(Outcome::DeniedFence);
    }
    pause_at(work, pause, Pause::Locked, "held", pid).await?;
    tx.commit().await?;
    // Internal COMMIT acknowledgement; deliberately not the caller result.
    pause_at(work, pause, Pause::Committed, "committed", pid).await?;
    Ok(Outcome::Admitted)
}
async fn pause_at(
    work: &Work,
    configured: Pause,
    actual: Pause,
    name: &str,
    pid: i32,
) -> Result<()> {
    observe(work, name, None, pid)?;
    if configured == actual {
        barrier(&work.barriers, "release").await?;
    }
    Ok(())
}
