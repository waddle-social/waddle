use std::path::Path;

use anyhow::{ensure, Result};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::authority::client;
use crate::fixture::Fixture;
use crate::multi_edge::contender::Contender;
use crate::multi_edge::graph;
use crate::multi_edge::model::{
    Action, Branch, Case, Control, Effect, EffectAction, Guard, Kind, Outcome, Pause, Payload,
    Witness,
};
use crate::multi_edge::store;

pub(super) struct World {
    pub(super) user: Uuid,
    pub(super) lifecycle: Guard,
    pub(super) binding: Guard,
    pub(super) ancestor: Guard,
    pub(super) other_parent: Guard,
    pub(super) direct: Guard,
}
impl World {
    pub(super) async fn new(fixture: &Fixture) -> Result<Self> {
        let channel = Uuid::new_v4();
        let parent = Uuid::new_v4();
        let make = |id, payload| Guard {
            id,
            generation: 1,
            operation: Uuid::new_v4(),
            payload,
        };
        let world = Self {
            user: Uuid::new_v4(),
            lifecycle: make(channel, Payload::Lifecycle(Uuid::new_v4())),
            binding: make(channel, Payload::Binding(parent)),
            ancestor: make(parent, Payload::Ancestor(true)),
            other_parent: make(Uuid::new_v4(), Payload::Ancestor(false)),
            direct: make(channel, Payload::Direct(false)),
        };
        let mut db = connect(fixture).await?;
        let graph_client = client(&fixture.settings).await?;
        for guard in [
            &world.lifecycle,
            &world.binding,
            &world.ancestor,
            &world.other_parent,
            &world.direct,
        ] {
            store::insert(&mut db, guard).await?;
            graph::publish(&graph_client, None, guard, world.user).await?;
            ensure!(
                graph::verify_mutation(&graph_client, guard, world.user).await?,
                "setup graph mismatch"
            );
            ensure!(
                store::activate(&mut db, guard).await?,
                "setup activation mismatch"
            );
        }
        Ok(world)
    }
    pub(super) fn effect(&self, branch: Branch) -> Effect {
        let guards = match branch {
            Branch::Inherited => vec![
                self.lifecycle.clone(),
                self.binding.clone(),
                self.ancestor.clone(),
            ],
            Branch::Direct => vec![self.lifecycle.clone(), self.direct.clone()],
        };
        Effect {
            key: Uuid::new_v4(),
            action: EffectAction::InspectSyntheticResource,
            payload: Uuid::new_v4(),
            witness: Witness {
                channel: self.lifecycle.id,
                principal: self.user,
                branch,
                guards,
            },
        }
    }
}
pub(super) async fn connect(fixture: &Fixture) -> Result<PgConnection> {
    fixture
        .settings
        .connect("fence_app", "fence_app", "multi-edge-controller", 3000)
        .await
}
pub(super) fn admit(
    fixture: &Fixture,
    binary: &Path,
    effect: &Effect,
    omit: Option<Kind>,
    pause: Pause,
    lock_ms: u32,
) -> Result<Contender> {
    Contender::spawn(
        fixture,
        binary,
        Action::Admit {
            effect: effect.clone(),
            omit: omit.map_or(Control::Full, Control::OmitGuard),
            pause,
            lock_ms,
        },
    )
}
pub(super) fn admit_version_control(
    fixture: &Fixture,
    binary: &Path,
    effect: &Effect,
    kind: Kind,
) -> Result<Contender> {
    Contender::spawn(
        fixture,
        binary,
        Action::Admit {
            effect: effect.clone(),
            omit: Control::OmitVersionStamp(kind),
            pause: Pause::Checked,
            lock_ms: 3000,
        },
    )
}
pub(super) fn mutator(
    fixture: &Fixture,
    binary: &Path,
    previous: &Guard,
    next: &Guard,
    user: Uuid,
    pause: Pause,
) -> Result<Contender> {
    Contender::spawn(
        fixture,
        binary,
        Action::Mutate {
            previous: previous.clone(),
            next: next.clone(),
            principal: user,
            pause,
        },
    )
}
pub(super) async fn change(
    fixture: &Fixture,
    binary: &Path,
    guard: &Guard,
    payload: Payload,
    user: Uuid,
) -> Result<Guard> {
    let next = guard.next(payload)?;
    mutator(fixture, binary, guard, &next, user, Pause::None)?
        .done(Outcome::Activated)
        .await?;
    Ok(next)
}
pub(super) async fn count(fixture: &Fixture, effect: &Effect, expected: i64) -> Result<()> {
    let mut db = connect(fixture).await?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM multi_effect WHERE effect=$1")
        .bind(effect.key)
        .fetch_one(&mut db)
        .await?;
    ensure!(
        count == expected,
        "durable effect count {count}, expected {expected}"
    );
    if count == 1 {
        let descriptor: String =
            sqlx::query_scalar("SELECT descriptor FROM multi_effect WHERE effect=$1")
                .bind(effect.key)
                .fetch_one(&mut db)
                .await?;
        ensure!(
            descriptor == serde_json::to_string(effect)?,
            "durable effect identity mismatch"
        );
    }
    Ok(())
}
pub(super) fn passed(cases: &mut Vec<Case>, name: &'static str) {
    cases.push(Case { name, passed: true });
}
