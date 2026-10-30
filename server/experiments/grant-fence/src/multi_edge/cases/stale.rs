use super::support::{admit, admit_version_control, change, count, passed, World};
use crate::authority::client;
use crate::fixture::Fixture;
use crate::multi_edge::graph;
use crate::multi_edge::model::{Branch, Case, Kind, Outcome, Pause, Payload};
use anyhow::{ensure, Result};
use std::path::Path;
use uuid::Uuid;
pub(super) async fn stale_vectors(
    fixture: &Fixture,
    binary: &Path,
    cases: &mut Vec<Case>,
) -> Result<()> {
    for (kind, positive, negative) in [
        (
            Kind::Lifecycle,
            "lifecycle_recreate_fenced",
            "unsafe_omitted_lifecycle",
        ),
        (
            Kind::Binding,
            "parent_move_fenced",
            "unsafe_omitted_binding",
        ),
        (
            Kind::Ancestor,
            "ancestor_revoke_fenced",
            "unsafe_omitted_ancestor",
        ),
        (
            Kind::Direct,
            "direct_revoke_fenced",
            "unsafe_omitted_direct",
        ),
    ] {
        for unsafe_control in [false, true] {
            let mut world = World::new(fixture).await?;
            let branch = if kind == Kind::Direct {
                world.direct = change(
                    fixture,
                    binary,
                    &world.direct,
                    Payload::Direct(true),
                    world.user,
                )
                .await?;
                Branch::Direct
            } else {
                Branch::Inherited
            };
            if branch == Branch::Direct {
                world.ancestor = change(
                    fixture,
                    binary,
                    &world.ancestor,
                    Payload::Ancestor(false),
                    world.user,
                )
                .await?;
            }
            let effect = world.effect(branch);
            let mut reader = admit(
                fixture,
                binary,
                &effect,
                unsafe_control.then_some(kind),
                Pause::Checked,
                3000,
            )?;
            reader.wait("checked").await?;
            match kind {
                Kind::Lifecycle => {
                    world.lifecycle = change(
                        fixture,
                        binary,
                        &world.lifecycle,
                        Payload::Lifecycle(Uuid::new_v4()),
                        world.user,
                    )
                    .await?;
                }
                Kind::Binding => {
                    change(
                        fixture,
                        binary,
                        &world.binding,
                        Payload::Binding(world.other_parent.id),
                        world.user,
                    )
                    .await?;
                }
                Kind::Ancestor => {
                    change(
                        fixture,
                        binary,
                        &world.ancestor,
                        Payload::Ancestor(false),
                        world.user,
                    )
                    .await?;
                }
                Kind::Direct => {
                    change(
                        fixture,
                        binary,
                        &world.direct,
                        Payload::Direct(false),
                        world.user,
                    )
                    .await?;
                }
            }
            if kind == Kind::Lifecycle {
                ensure!(
                    graph::current_permissions(
                        &client(&fixture.settings).await?,
                        &world.lifecycle,
                        world.user
                    )
                    .await?
                        == (true, false),
                    "recreation control must retain only inherited graph access"
                );
            } else {
                let graph_client = client(&fixture.settings).await?;
                ensure!(
                    graph::current_permissions(&graph_client, &world.lifecycle, world.user).await?
                        == (false, false),
                    "negative control retains a current authorization path"
                );
            }
            reader.release()?;
            reader
                .done(if unsafe_control {
                    Outcome::Admitted
                } else {
                    Outcome::DeniedFence
                })
                .await?;
            count(fixture, &effect, i64::from(unsafe_control)).await?;
            passed(cases, if unsafe_control { negative } else { positive });
        }
    }
    // The direct witness also requires the stable lifecycle row.
    let mut world = World::new(fixture).await?;
    world.direct = change(
        fixture,
        binary,
        &world.direct,
        Payload::Direct(true),
        world.user,
    )
    .await?;
    let effect = world.effect(Branch::Direct);
    let mut reader = admit(fixture, binary, &effect, None, Pause::Checked, 3000)?;
    reader.wait("checked").await?;
    world.lifecycle = change(
        fixture,
        binary,
        &world.lifecycle,
        Payload::Lifecycle(Uuid::new_v4()),
        world.user,
    )
    .await?;
    reader.release()?;
    reader.done(Outcome::DeniedFence).await?;
    count(fixture, &effect, 0).await?;
    let fresh = world.effect(Branch::Direct);
    admit(fixture, binary, &fresh, None, Pause::None, 3000)?
        .done(Outcome::Admitted)
        .await?;
    count(fixture, &fresh, 1).await?;
    passed(cases, "direct_lifecycle_recreate_fenced");
    Ok(())
}
pub(super) async fn aba(fixture: &Fixture, binary: &Path, cases: &mut Vec<Case>) -> Result<()> {
    for (kind, name, negative) in [
        (
            Kind::Binding,
            "binding_a_b_a",
            "unsafe_binding_a_b_a_omitted_version_stamp",
        ),
        (
            Kind::Ancestor,
            "ancestor_revoke_regrant",
            "unsafe_ancestor_aba_omitted_version_stamp",
        ),
        (
            Kind::Direct,
            "direct_revoke_regrant",
            "unsafe_direct_aba_omitted_version_stamp",
        ),
    ] {
        for unsafe_control in [false, true] {
            let mut world = World::new(fixture).await?;
            let branch = if kind == Kind::Direct {
                world.direct = change(
                    fixture,
                    binary,
                    &world.direct,
                    Payload::Direct(true),
                    world.user,
                )
                .await?;
                Branch::Direct
            } else {
                Branch::Inherited
            };
            if branch == Branch::Direct {
                world.ancestor = change(
                    fixture,
                    binary,
                    &world.ancestor,
                    Payload::Ancestor(false),
                    world.user,
                )
                .await?;
            }
            let effect = world.effect(branch);
            let mut reader = if unsafe_control {
                admit_version_control(fixture, binary, &effect, kind)?
            } else {
                admit(fixture, binary, &effect, None, Pause::Checked, 3000)?
            };
            reader.wait("checked").await?;
            let (guard, intermediate) = match kind {
                Kind::Binding => (&mut world.binding, Payload::Binding(world.other_parent.id)),
                Kind::Ancestor => (&mut world.ancestor, Payload::Ancestor(false)),
                Kind::Direct => (&mut world.direct, Payload::Direct(false)),
                Kind::Lifecycle => unreachable!("bounded ABA inventory"),
            };
            let initial = guard.clone();
            *guard = change(fixture, binary, guard, intermediate, world.user).await?;
            *guard = change(fixture, binary, guard, initial.payload.clone(), world.user).await?;
            ensure!(
                initial.id == guard.id
                    && initial.payload == guard.payload
                    && initial.generation + 2 == guard.generation,
                "ABA identity precondition"
            );
            let current = graph::current_permissions(
                &client(&fixture.settings).await?,
                &world.lifecycle,
                world.user,
            )
            .await?;
            ensure!(
                current
                    == if branch == Branch::Inherited {
                        (true, false)
                    } else {
                        (false, true)
                    },
                "ABA must restore exactly the chosen access branch"
            );
            reader.release()?;
            reader
                .done(if unsafe_control {
                    Outcome::Admitted
                } else {
                    Outcome::DeniedFence
                })
                .await?;
            count(fixture, &effect, i64::from(unsafe_control)).await?;
            let fresh = world.effect(branch);
            admit(fixture, binary, &fresh, None, Pause::None, 3000)?
                .done(Outcome::Admitted)
                .await?;
            count(fixture, &fresh, 1).await?;
            passed(cases, if unsafe_control { negative } else { name });
        }
    }
    Ok(())
}
pub(super) async fn alternatives(
    fixture: &Fixture,
    binary: &Path,
    cases: &mut Vec<Case>,
) -> Result<()> {
    let mut world = World::new(fixture).await?;
    world.direct = change(
        fixture,
        binary,
        &world.direct,
        Payload::Direct(true),
        world.user,
    )
    .await?;
    let inherited = world.effect(Branch::Inherited);
    let mut old = admit(fixture, binary, &inherited, None, Pause::Checked, 3000)?;
    old.wait("checked").await?;
    world.ancestor = change(
        fixture,
        binary,
        &world.ancestor,
        Payload::Ancestor(false),
        world.user,
    )
    .await?;
    old.release()?;
    old.done(Outcome::DeniedFence).await?;
    count(fixture, &inherited, 0).await?;
    let direct = world.effect(Branch::Direct);
    admit(fixture, binary, &direct, None, Pause::None, 3000)?
        .done(Outcome::Admitted)
        .await?;
    count(fixture, &direct, 1).await?;
    let forged_inherited = world.effect(Branch::Inherited);
    // Keep a structurally positive witness but use the actual denied version.
    let mut denied = forged_inherited;
    denied.witness.guards[2].payload = Payload::Ancestor(true);
    admit(fixture, binary, &denied, None, Pause::None, 3000)?
        .done(Outcome::DeniedGraph)
        .await?;
    count(fixture, &denied, 0).await?;
    passed(
        cases,
        "direct_alternative_does_not_rescue_inherited_witness",
    );
    Ok(())
}
pub(super) async fn snapshot(
    fixture: &Fixture,
    binary: &Path,
    cases: &mut Vec<Case>,
) -> Result<()> {
    let world = World::new(fixture).await?;
    let effect = world.effect(Branch::Inherited);
    let mut reader = admit(fixture, binary, &effect, None, Pause::Anchored, 3000)?;
    reader.wait("anchored").await?;
    change(
        fixture,
        binary,
        &world.ancestor,
        Payload::Ancestor(false),
        world.user,
    )
    .await?;
    reader.release()?;
    // This proves reads/check used the old anchored snapshot, not individually
    // fully-consistent reads that would deny before the final SQL fence.
    reader.wait("checked").await?;
    reader.done(Outcome::DeniedFence).await?;
    count(fixture, &effect, 0).await?;
    passed(
        cases,
        "anchored_snapshot_survives_mid_read_revoke_then_sql_fences",
    );
    Ok(())
}
