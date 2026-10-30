use std::path::Path;

use anyhow::{ensure, Result};

use super::support::{admit, change, connect, count, mutator, passed, World};
use crate::fixture::Fixture;
use crate::multi_edge::contender::blocked;
use crate::multi_edge::model::{Branch, Case, Kind, Outcome, Pause, Payload};
use crate::multi_edge::store;

pub(super) async fn run(fixture: &Fixture, binary: &Path, cases: &mut Vec<Case>) -> Result<()> {
    for kind in [Kind::Binding, Kind::Ancestor] {
        admission_first(fixture, binary, cases, kind).await?;
        mutation_first(fixture, binary, cases, kind).await?;
    }
    timeout(fixture, binary, cases).await?;
    closed_death(fixture, binary, cases).await?;
    stale_activation(fixture, binary, cases).await
}
async fn admission_first(
    fixture: &Fixture,
    binary: &Path,
    cases: &mut Vec<Case>,
    kind: Kind,
) -> Result<()> {
    let world = World::new(fixture).await?;
    let effect = world.effect(Branch::Inherited);
    let mut reader = admit(fixture, binary, &effect, None, Pause::Locked, 3000)?;
    let holder = reader.wait("held").await?.pid;
    let (guard, payload) = if kind == Kind::Binding {
        (&world.binding, Payload::Binding(world.other_parent.id))
    } else {
        (&world.ancestor, Payload::Ancestor(false))
    };
    let next = guard.next(payload)?;
    let mut mutation = mutator(fixture, binary, guard, &next, world.user, Pause::None)?;
    let waiter = mutation.wait("attempting").await?.pid;
    blocked(&mut connect(fixture).await?, waiter, holder).await?;
    let separate = World::new(fixture).await?;
    change(
        fixture,
        binary,
        &separate.direct,
        Payload::Direct(true),
        separate.user,
    )
    .await?;
    let independent = separate.effect(Branch::Inherited);
    admit(fixture, binary, &independent, None, Pause::None, 3000)?
        .done(Outcome::Admitted)
        .await?;
    count(fixture, &independent, 1).await?;
    reader.release()?;
    reader.done(Outcome::Admitted).await?;
    mutation.done(Outcome::Activated).await?;
    count(fixture, &effect, 1).await?;
    passed(
        cases,
        if kind == Kind::Binding {
            "admission_before_binding_move_with_independent_progress"
        } else {
            "admission_before_ancestor_revoke_with_independent_progress"
        },
    );
    Ok(())
}
async fn mutation_first(
    fixture: &Fixture,
    binary: &Path,
    cases: &mut Vec<Case>,
    kind: Kind,
) -> Result<()> {
    let world = World::new(fixture).await?;
    let effect = world.effect(Branch::Inherited);
    let mut reader = admit(fixture, binary, &effect, None, Pause::Checked, 3000)?;
    let waiter = reader.wait("checked").await?.pid;
    let (guard, payload) = if kind == Kind::Binding {
        (&world.binding, Payload::Binding(world.other_parent.id))
    } else {
        (&world.ancestor, Payload::Ancestor(false))
    };
    let next = guard.next(payload)?;
    let mut mutation = mutator(
        fixture,
        binary,
        guard,
        &next,
        world.user,
        Pause::MutationLocked,
    )?;
    let holder = mutation.wait("held").await?.pid;
    reader.release()?;
    blocked(&mut connect(fixture).await?, waiter, holder).await?;
    mutation.release()?;
    mutation.done(Outcome::Activated).await?;
    reader.done(Outcome::DeniedFence).await?;
    count(fixture, &effect, 0).await?;
    passed(
        cases,
        if kind == Kind::Binding {
            "binding_move_before_admission"
        } else {
            "ancestor_revoke_before_admission"
        },
    );
    Ok(())
}
async fn timeout(fixture: &Fixture, binary: &Path, cases: &mut Vec<Case>) -> Result<()> {
    let world = World::new(fixture).await?;
    let effect = world.effect(Branch::Inherited);
    let mut reader = admit(fixture, binary, &effect, None, Pause::Checked, 600)?;
    let waiter = reader.wait("checked").await?.pid;
    let next = world.ancestor.next(Payload::Ancestor(false))?;
    let mut mutation = mutator(
        fixture,
        binary,
        &world.ancestor,
        &next,
        world.user,
        Pause::MutationLocked,
    )?;
    let holder = mutation.wait("held").await?.pid;
    reader.release()?;
    blocked(&mut connect(fixture).await?, waiter, holder).await?;
    reader.done(Outcome::LockTimeout).await?;
    count(fixture, &effect, 0).await?;
    mutation.release()?;
    mutation.done(Outcome::Activated).await?;
    passed(cases, "multi_guard_lock_timeout_rolls_back_reservation");
    Ok(())
}
async fn closed_death(fixture: &Fixture, binary: &Path, cases: &mut Vec<Case>) -> Result<()> {
    let world = World::new(fixture).await?;
    let next = world.ancestor.next(Payload::Ancestor(false))?;
    let mut mutation = mutator(
        fixture,
        binary,
        &world.ancestor,
        &next,
        world.user,
        Pause::Closed,
    )?;
    mutation.wait("closed").await?;
    mutation.process.stop()?;
    ensure!(
        !store::active(&mut connect(fixture).await?, &next).await?,
        "dead worker activated closed operation"
    );
    let effect = world.effect(Branch::Inherited);
    admit(fixture, binary, &effect, None, Pause::None, 3000)?
        .done(Outcome::DeniedFence)
        .await?;
    count(fixture, &effect, 0).await?;
    passed(
        cases,
        "death_after_durable_close_stays_closed_without_reconstruction",
    );
    Ok(())
}
async fn stale_activation(fixture: &Fixture, binary: &Path, cases: &mut Vec<Case>) -> Result<()> {
    let world = World::new(fixture).await?;
    let pending = world.ancestor.next(Payload::Ancestor(true))?;
    let mut old = mutator(
        fixture,
        binary,
        &world.ancestor,
        &pending,
        world.user,
        Pause::Published,
    )?;
    old.wait("published").await?;
    ensure!(
        !store::active(&mut connect(fixture).await?, &pending).await?,
        "publication prematurely activated"
    );
    let newer = change(
        fixture,
        binary,
        &pending,
        Payload::Ancestor(false),
        world.user,
    )
    .await?;
    old.release()?;
    old.done(Outcome::Superseded).await?;
    ensure!(
        !store::activate(&mut connect(fixture).await?, &pending).await?,
        "stale exact operation CAS reopened"
    );
    ensure!(
        store::active(&mut connect(fixture).await?, &newer).await?,
        "newer operation changed"
    );
    passed(
        cases,
        "stale_acknowledged_graph_completion_cannot_reopen_newer_revoke",
    );
    Ok(())
}
