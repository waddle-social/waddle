use std::path::Path;

use anyhow::Result;
use uuid::Uuid;

use super::support::{admit, change, connect, count, passed, World};
use crate::fixture::Fixture;
use crate::multi_edge::contender::blocked;
use crate::multi_edge::model::{Branch, Case, Outcome, Pause, Payload};

pub(super) async fn run(fixture: &Fixture, binary: &Path, cases: &mut Vec<Case>) -> Result<()> {
    duplicate(fixture, binary, cases, false).await?;
    duplicate(fixture, binary, cases, true).await?;
    unresolved(fixture, binary, cases, false, false).await?;
    unresolved(fixture, binary, cases, true, false).await?;
    unresolved(fixture, binary, cases, false, true).await?;
    committed_death(fixture, binary, cases).await
}
async fn duplicate(
    fixture: &Fixture,
    binary: &Path,
    cases: &mut Vec<Case>,
    altered: bool,
) -> Result<()> {
    let world = World::new(fixture).await?;
    let effect = world.effect(Branch::Inherited);
    admit(fixture, binary, &effect, None, Pause::None, 3000)?
        .done(Outcome::Admitted)
        .await?;
    change(
        fixture,
        binary,
        &world.ancestor,
        Payload::Ancestor(false),
        world.user,
    )
    .await?;
    let mut retry = effect.clone();
    if altered {
        retry.payload = Uuid::new_v4();
    }
    admit(fixture, binary, &retry, None, Pause::None, 3000)?
        .done(if altered {
            Outcome::Conflict
        } else {
            Outcome::AlreadyCommitted
        })
        .await?;
    count(fixture, &effect, 1).await?;
    passed(
        cases,
        if altered {
            "altered_descriptor_rejected_after_revoke"
        } else {
            "exact_committed_retry_reconciles_after_revoke"
        },
    );
    Ok(())
}
async fn unresolved(
    fixture: &Fixture,
    binary: &Path,
    cases: &mut Vec<Case>,
    kill_first: bool,
    altered: bool,
) -> Result<()> {
    let world = World::new(fixture).await?;
    let effect = world.effect(Branch::Inherited);
    let mut first = admit(fixture, binary, &effect, None, Pause::Locked, 3000)?;
    let holder = first.wait("held").await?.pid;
    count(fixture, &effect, 0).await?;
    let mut retry_effect = effect.clone();
    if altered {
        retry_effect.payload = Uuid::new_v4();
    }
    let mut retry = admit(fixture, binary, &retry_effect, None, Pause::None, 3000)?;
    let waiter = retry.wait("attempting").await?.pid;
    blocked(&mut connect(fixture).await?, waiter, holder).await?;
    if kill_first {
        first.process.stop()?;
        retry.done(Outcome::Admitted).await?;
    } else {
        first.release()?;
        first.done(Outcome::Admitted).await?;
        retry
            .done(if altered {
                Outcome::Conflict
            } else {
                Outcome::AlreadyCommitted
            })
            .await?;
    }
    count(fixture, &effect, 1).await?;
    passed(
        cases,
        if kill_first {
            "precommit_death_rolls_back_and_waiting_fresh_worker_admits"
        } else if altered {
            "unique_insert_wait_then_altered_descriptor_conflict"
        } else {
            "unique_insert_wait_then_exact_duplicate"
        },
    );
    Ok(())
}
async fn committed_death(fixture: &Fixture, binary: &Path, cases: &mut Vec<Case>) -> Result<()> {
    let world = World::new(fixture).await?;
    let effect = world.effect(Branch::Inherited);
    let mut first = admit(fixture, binary, &effect, None, Pause::Committed, 3000)?;
    first.wait("committed").await?;
    count(fixture, &effect, 1).await?;
    // Internal COMMIT acknowledged, caller outcome still withheld. This is NOT
    // a dropped PostgreSQL wire response.
    first.process.stop()?;
    change(
        fixture,
        binary,
        &world.ancestor,
        Payload::Ancestor(false),
        world.user,
    )
    .await?;
    admit(fixture, binary, &effect, None, Pause::None, 3000)?
        .done(Outcome::AlreadyCommitted)
        .await?;
    count(fixture, &effect, 1).await?;
    passed(
        cases,
        "death_after_acknowledged_commit_before_caller_result_reconciles",
    );
    Ok(())
}
