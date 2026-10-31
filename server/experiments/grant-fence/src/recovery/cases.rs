use std::path::Path;
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use serde::Serialize;
use sqlx::PgConnection;

use super::contender::Contender;
use super::graph;
use super::model::{
    Candidate, Component, Fact, Identity, Intent, Outcome, Pause, Polarity, RecoveryAction,
};
use super::store;
use crate::authority::client;
use crate::fixture::Fixture;

#[derive(Serialize)]
pub enum Scenario {
    Outage,
    ClosedTupleLoss,
    ClosedMarkerLoss,
    AcknowledgedWriteDeath,
    ActivationWaitsForRevokeCommit,
    ActivationAfterRevokeCommit,
    ActivationBeforeRevokeBegins,
    ActiveLossAfterAllowedCheckNegativeControl,
}
#[derive(Serialize)]
pub enum Stage {
    Unavailable,
    ClosedBeforeFault,
    Corrupted,
    Recovered,
    Superseded,
    IndependentScope,
    NegativeControl,
}
#[derive(Serialize)]
pub enum GuardState {
    Closed,
    Active,
    Superseded,
}
#[derive(Serialize)]
pub struct StageEvidence {
    stage: Stage,
    identity: Identity,
    facts: Vec<Fact>,
    guard: GuardState,
    canonical_pair_present: Option<bool>,
    outcome: Option<Outcome>,
}
#[derive(Serialize)]
pub struct RecoveryCase {
    scenario: Scenario,
    polarity: Polarity,
    stages: Vec<StageEvidence>,
    notes: Vec<&'static str>,
}

async fn connection(fixture: &Fixture) -> Result<PgConnection> {
    fixture
        .settings
        .connect("fence_app", "fence_app", "recovery-observer", 3000)
        .await
}
async fn closed(fixture: &Fixture, identity: &Identity) -> Result<()> {
    let mut connection = connection(fixture).await?;
    ensure!(
        store::current(&mut connection, identity, false).await?,
        "fault injection requires exact committed closed guard"
    );
    ensure!(
        store::evidence(&mut connection, identity)
            .await?
            .iter()
            .any(|e| e.fact == Fact::AdmissionClosed),
        "closed fence evidence missing"
    );
    Ok(())
}
async fn stage(
    fixture: &Fixture,
    identity: &Identity,
    point: Stage,
    outcome: Option<Outcome>,
    service_healthy: bool,
) -> Result<StageEvidence> {
    let mut connection = connection(fixture).await?;
    let guard = if store::current(&mut connection, identity, false).await? {
        GuardState::Closed
    } else if store::current(&mut connection, identity, true).await? {
        GuardState::Active
    } else {
        GuardState::Superseded
    };
    let evidence = store::evidence(&mut connection, identity).await?;
    ensure!(
        evidence.iter().all(|e| &e.identity == identity),
        "evidence must bind complete exact identity"
    );
    if identity.candidate == Candidate::SpiceDb {
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM recovery_intent WHERE candidate=$1 AND scope=$2",
        )
        .bind(identity.candidate.storage())
        .bind(identity.scope)
        .fetch_one(&mut connection)
        .await?;
        ensure!(
            count == 0,
            "SpiceDB SQL metadata must carry no canonical payload"
        );
    }
    let canonical_pair_present = if service_healthy {
        Some(
            graph::verify(&client(&fixture.settings).await?, identity)
                .await?
                .is_some(),
        )
    } else {
        None
    };
    Ok(StageEvidence {
        stage: point,
        identity: identity.clone(),
        facts: evidence.into_iter().map(|e| e.fact).collect(),
        guard,
        canonical_pair_present,
        outcome,
    })
}
fn completed(intent: &Intent) -> Outcome {
    if intent.polarity == Polarity::Grant {
        Outcome::Activated
    } else {
        Outcome::VerifiedRevoke
    }
}
async fn mutate(
    binary: &Path,
    fixture: &Fixture,
    identity: &Identity,
    intent: &Intent,
    previous: Option<&Identity>,
) -> Result<()> {
    let mut worker = Contender::spawn(
        binary,
        fixture,
        identity,
        RecoveryAction::Mutate {
            intent: intent.clone(),
            previous: previous.cloned(),
            pause: Pause::None,
        },
    )?;
    worker.done(completed(intent)).await
}
async fn plan(
    binary: &Path,
    fixture: &Fixture,
    candidate: Candidate,
    polarity: Polarity,
) -> Result<(Identity, Intent, Option<Identity>)> {
    let grant = Intent::grant();
    let first = Identity::fresh(candidate, &grant);
    if polarity == Polarity::Grant {
        return Ok((first, grant, None));
    }
    mutate(binary, fixture, &first, &grant, None).await?;
    let revoke = grant.revoke();
    Ok((first.next(&revoke)?, revoke, Some(first)))
}
async fn recover(
    binary: &Path,
    fixture: &Fixture,
    identity: &Identity,
    expected: Outcome,
) -> Result<()> {
    let mut recovery = Contender::spawn(
        binary,
        fixture,
        identity,
        RecoveryAction::Recover { pause: Pause::None },
    )?;
    recovery.done(expected).await
}
async fn assert_no_admission(binary: &Path, fixture: &Fixture, identity: &Identity) -> Result<()> {
    let mut worker = Contender::spawn(
        binary,
        fixture,
        identity,
        RecoveryAction::Admit {
            pause_after_check: false,
        },
    )?;
    worker.done(Outcome::Denied).await?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM recovery_admission WHERE attempt=$1")
        .bind(worker.work.attempt)
        .fetch_one(&mut connection(fixture).await?)
        .await?;
    ensure!(
        count == 0,
        "closed or missing canonical authority must admit nothing"
    );
    Ok(())
}

pub async fn run(
    fixture: &mut Fixture,
    binary: &Path,
    cases: &mut Vec<RecoveryCase>,
) -> Result<()> {
    outage(fixture, binary, cases).await?;
    for candidate in [Candidate::Postgres, Candidate::SpiceDb] {
        for polarity in [Polarity::Grant, Polarity::Revoke] {
            for marker in [false, true] {
                loss(fixture, binary, candidate, polarity, marker, cases).await?;
            }
            acknowledged_death(fixture, binary, candidate, polarity, cases).await?;
        }
        for order in 0..3 {
            stale(fixture, binary, candidate, order, cases).await?;
        }
    }
    active_loss_control(fixture, binary, cases).await?;
    Ok(())
}

async fn outage(fixture: &mut Fixture, binary: &Path, cases: &mut Vec<RecoveryCase>) -> Result<()> {
    for polarity in [Polarity::Grant, Polarity::Revoke] {
        let mut planned = Vec::new();
        for candidate in [Candidate::Postgres, Candidate::SpiceDb] {
            planned.push(plan(binary, fixture, candidate, polarity).await?);
        }
        fixture.stop_spice()?;
        let mut before = Vec::new();
        for (identity, intent, previous) in &planned {
            let mut worker = Contender::spawn(
                binary,
                fixture,
                identity,
                RecoveryAction::Mutate {
                    intent: intent.clone(),
                    previous: previous.clone(),
                    pause: Pause::None,
                },
            )?;
            worker.done(Outcome::AuthorityUnavailable).await?;
            closed(fixture, identity).await?;
            recover(binary, fixture, identity, Outcome::AuthorityUnavailable).await?;
            assert_no_admission(binary, fixture, identity).await?;
            let snapshot = stage(
                fixture,
                identity,
                Stage::Unavailable,
                Some(Outcome::AuthorityUnavailable),
                false,
            )
            .await?;
            ensure!(
                snapshot.facts.contains(&Fact::CanonicalMutationCommitted)
                    == (identity.candidate == Candidate::Postgres)
                    && !snapshot.facts.contains(&Fact::ProjectionVerified),
                "outage must distinguish canonical commitment from projection verification"
            );
            before.push(snapshot);
        }
        fixture.start_spice().await?;
        for ((identity, intent, previous), before) in planned.iter().zip(before) {
            let expected = if identity.candidate == Candidate::Postgres {
                completed(intent)
            } else {
                Outcome::CanonicalMissing
            };
            recover(binary, fixture, identity, expected).await?;
            if identity.candidate == Candidate::SpiceDb || polarity == Polarity::Revoke {
                closed(fixture, identity).await?;
                assert_no_admission(binary, fixture, identity).await?;
            }
            if let Some(previous) = previous {
                ensure!(
                    graph::allowed(&client(&fixture.settings).await?, previous).await?,
                    "outage left historical grant in actual SpiceDB"
                );
                assert_no_admission(binary, fixture, previous).await?;
            }
            cases.push(RecoveryCase { scenario: Scenario::Outage, polarity, stages: vec![before, stage(fixture, identity, Stage::Recovered, Some(expected), true).await?], notes: vec!["Global SpiceDB outage denies protected reads for both candidates; SQL closure is distinct from canonical commitment.", "Recovery is a fresh identity-only process; no mutation is inferred from a timeout."] });
        }
    }
    Ok(())
}

async fn loss(
    fixture: &Fixture,
    binary: &Path,
    candidate: Candidate,
    polarity: Polarity,
    marker: bool,
    cases: &mut Vec<RecoveryCase>,
) -> Result<()> {
    let (identity, intent, previous) = plan(binary, fixture, candidate, polarity).await?;
    let mut worker = Contender::spawn(
        binary,
        fixture,
        &identity,
        RecoveryAction::Mutate {
            intent: intent.clone(),
            previous,
            pause: Pause::AfterAcknowledgedWrite,
        },
    )?;
    worker.wait("acknowledged").await?;
    closed(fixture, &identity).await?;
    let before = stage(fixture, &identity, Stage::ClosedBeforeFault, None, true).await?;
    ensure!(
        before.canonical_pair_present == Some(true),
        "atomic acknowledged write must contain marker and positive leaf"
    );
    graph::remove(
        &client(&fixture.settings).await?,
        &identity,
        if marker {
            Component::Marker
        } else {
            Component::Leaf(intent.polarity)
        },
    )
    .await?;
    closed(fixture, &identity).await?;
    worker.process.stop()?;
    let corrupted = stage(fixture, &identity, Stage::Corrupted, None, true).await?;
    ensure!(
        corrupted.canonical_pair_present == Some(false),
        "independently removed canonical component must make verification fail"
    );
    assert_no_admission(binary, fixture, &identity).await?;
    let expected = if candidate == Candidate::Postgres {
        completed(&intent)
    } else {
        Outcome::CanonicalMissing
    };
    recover(binary, fixture, &identity, expected).await?;
    if candidate == Candidate::SpiceDb || polarity == Polarity::Revoke {
        closed(fixture, &identity).await?;
    }
    cases.push(RecoveryCase { scenario: if marker { Scenario::ClosedMarkerLoss } else { Scenario::ClosedTupleLoss }, polarity, stages: vec![before, corrupted, stage(fixture, &identity, Stage::Recovered, Some(expected), true).await?], notes: vec!["Loss was injected only after observing the exact committed closed fence; tuple and marker are removed separately.", "Canonical commitment facts are historical observations, not a claim that canonical data still exists after corruption."] });
    Ok(())
}

async fn acknowledged_death(
    fixture: &Fixture,
    binary: &Path,
    candidate: Candidate,
    polarity: Polarity,
    cases: &mut Vec<RecoveryCase>,
) -> Result<()> {
    let (identity, intent, previous) = plan(binary, fixture, candidate, polarity).await?;
    let mut worker = Contender::spawn(
        binary,
        fixture,
        &identity,
        RecoveryAction::Mutate {
            intent: intent.clone(),
            previous,
            pause: Pause::AfterAcknowledgedWrite,
        },
    )?;
    worker.wait("acknowledged").await?;
    closed(fixture, &identity).await?;
    let before = stage(fixture, &identity, Stage::ClosedBeforeFault, None, true).await?;
    ensure!(
        before.canonical_pair_present == Some(true)
            && !before.facts.contains(&Fact::ProjectionVerified),
        "kill point must follow acknowledged atomic write and precede verification/activation"
    );
    let predecessor = worker.process.id();
    worker.process.stop()?;
    let mut recovery = Contender::spawn(
        binary,
        fixture,
        &identity,
        RecoveryAction::Recover { pause: Pause::None },
    )?;
    ensure!(
        recovery.process.id() != predecessor,
        "recovery requires a fresh native process"
    );
    recovery.done(completed(&intent)).await?;
    cases.push(RecoveryCase { scenario: Scenario::AcknowledgedWriteDeath, polarity, stages: vec![before, stage(fixture, &identity, Stage::Recovered, Some(completed(&intent)), true).await?], notes: vec!["The external RPC was acknowledged before worker death; this is not a lost RPC response experiment.", "Single-revision canonical verification establishes a separate fact and does not itself reopen admission."] });
    Ok(())
}

async fn blocked(observer: &mut PgConnection, waiter: i32, holder: i32) -> Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let blockers: Vec<i32> = sqlx::query_scalar("SELECT pg_blocking_pids($1)")
            .bind(waiter)
            .fetch_one(&mut *observer)
            .await?;
        if blockers.contains(&holder) {
            return Ok(());
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            "exact recovery PostgreSQL blocker not observed"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn stale(
    fixture: &Fixture,
    binary: &Path,
    candidate: Candidate,
    order: u8,
    cases: &mut Vec<RecoveryCase>,
) -> Result<()> {
    let grant = Intent::grant();
    let identity = Identity::fresh(candidate, &grant);
    let mut initial = Contender::spawn(
        binary,
        fixture,
        &identity,
        RecoveryAction::Mutate {
            intent: grant.clone(),
            previous: None,
            pause: Pause::AfterAcknowledgedWrite,
        },
    )?;
    initial.wait("acknowledged").await?;
    closed(fixture, &identity).await?;
    initial.process.stop()?;
    let mut stale = Contender::spawn(
        binary,
        fixture,
        &identity,
        RecoveryAction::Recover {
            pause: Pause::BeforeActivation,
        },
    )?;
    stale.wait("ready").await?;
    closed(fixture, &identity).await?;
    let verified = stage(fixture, &identity, Stage::ClosedBeforeFault, None, true).await?;
    ensure!(
        verified.facts.contains(&Fact::ProjectionVerified),
        "verification must be independent of activation"
    );
    let revoke = grant.revoke();
    let next = identity.next(&revoke)?;
    let mut independent_stage = None;
    if order == 2 {
        stale.release()?;
        stale.done(Outcome::Activated).await?;
        mutate(binary, fixture, &next, &revoke, Some(&identity)).await?;
    } else {
        let mut revoker = Contender::spawn(
            binary,
            fixture,
            &next,
            RecoveryAction::Mutate {
                intent: revoke,
                previous: Some(identity.clone()),
                pause: Pause::BeforeFenceCommit,
            },
        )?;
        let holder = revoker
            .wait("held")
            .await?
            .backend_pid
            .context("revoker backend")?;
        // The previous operation is still committed closed until this transaction
        // commits, even though it is being replaced by an exact newer revoke.
        closed(fixture, &identity).await?;
        let independent_intent = Intent::grant();
        let independent = Identity::fresh(candidate, &independent_intent);
        mutate(binary, fixture, &independent, &independent_intent, None).await?;
        let mut other = Contender::spawn(
            binary,
            fixture,
            &independent,
            RecoveryAction::Admit {
                pause_after_check: false,
            },
        )?;
        other.done(Outcome::Admitted).await?;
        independent_stage = Some(
            stage(
                fixture,
                &independent,
                Stage::IndependentScope,
                Some(Outcome::Admitted),
                true,
            )
            .await?,
        );
        if order == 0 {
            stale.release()?;
            let waiter = stale
                .wait("attempting")
                .await?
                .backend_pid
                .context("activation backend")?;
            let mut observer = fixture
                .settings
                .connect(
                    "fence_admin",
                    "postgres",
                    "recovery-blocking-observer",
                    3000,
                )
                .await?;
            blocked(&mut observer, waiter, holder).await?;
            revoker.release()?;
            revoker.done(Outcome::VerifiedRevoke).await?;
            stale.done(Outcome::Stale).await?;
        } else {
            revoker.release()?;
            revoker.done(Outcome::VerifiedRevoke).await?;
            stale.release()?;
            stale.done(Outcome::Stale).await?;
        }
    }
    closed(fixture, &next).await?;
    assert_no_admission(binary, fixture, &identity).await?;
    ensure!(
        graph::allowed(&client(&fixture.settings).await?, &identity).await?,
        "historical canonical grant remains allowed, so the new SQL fence must deny it"
    );
    let mut stages = vec![
        verified,
        stage(
            fixture,
            &identity,
            Stage::Superseded,
            Some(if order == 2 {
                Outcome::Activated
            } else {
                Outcome::Stale
            }),
            true,
        )
        .await?,
        stage(
            fixture,
            &next,
            Stage::Recovered,
            Some(Outcome::VerifiedRevoke),
            true,
        )
        .await?,
    ];
    stages.extend(independent_stage);
    cases.push(RecoveryCase { scenario: match order { 0 => Scenario::ActivationWaitsForRevokeCommit, 1 => Scenario::ActivationAfterRevokeCommit, _ => Scenario::ActivationBeforeRevokeBegins }, polarity: Polarity::Grant, stages, notes: vec!["A newer revoke supersedes the exact pending operation, advances generation and leaves admission closed.", "Precommit activation is proven blocked by pg_blocking_pids; the other schedule attempts activation only after revoke commit. A third schedule permits activation before revoke starts.", "With healthy SpiceDB an unrelated scope both mutates and admits while this scope's exclusive mutation lock is held (first two schedules)."] });
    Ok(())
}

async fn active_loss_control(
    fixture: &Fixture,
    binary: &Path,
    cases: &mut Vec<RecoveryCase>,
) -> Result<()> {
    let intent = Intent::grant();
    let identity = Identity::fresh(Candidate::SpiceDb, &intent);
    mutate(binary, fixture, &identity, &intent, None).await?;
    let mut admission = Contender::spawn(
        binary,
        fixture,
        &identity,
        RecoveryAction::Admit {
            pause_after_check: true,
        },
    )?;
    admission.wait("checked").await?;
    ensure!(
        store::current(&mut connection(fixture).await?, &identity, true).await?,
        "negative control deliberately begins active"
    );
    graph::remove(
        &client(&fixture.settings).await?,
        &identity,
        Component::Marker,
    )
    .await?;
    graph::remove(
        &client(&fixture.settings).await?,
        &identity,
        Component::Leaf(Polarity::Grant),
    )
    .await?;
    ensure!(
        !graph::allowed(&client(&fixture.settings).await?, &identity).await?,
        "canonical authority really disappeared after the check"
    );
    admission.release()?;
    admission.done(Outcome::Admitted).await?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM recovery_admission WHERE attempt=$1")
        .bind(admission.work.attempt)
        .fetch_one(&mut connection(fixture).await?)
        .await?;
    ensure!(
        count == 1,
        "negative control must demonstrate the unsupported active-corruption hole"
    );
    cases.push(RecoveryCase { scenario: Scenario::ActiveLossAfterAllowedCheckNegativeControl, polarity: Polarity::Grant, stages: vec![stage(fixture, &identity, Stage::NegativeControl, Some(Outcome::Admitted), true).await?], notes: vec!["Expected unsafe control: deleting ACTIVE canonical state after a successful check without closing the PostgreSQL guard permits admission.", "This experiment does not prove arbitrary active datastore corruption detection or production writer closure."] });
    Ok(())
}
