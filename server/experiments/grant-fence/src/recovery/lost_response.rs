//! The writer never sees a successful response; fresh processes recover only
//! through the candidate's authority. Proxy observations are separate evidence.
use std::path::Path;

use anyhow::{ensure, Result};
use serde::Serialize;

use super::cases::{assert_no_admission, closed, completed, connection, mutate, plan, recover};
use super::contender::Contender;
use super::graph;
use super::model::{
    Candidate, Fact, Identity, Intent, Outcome, Pause, Polarity, RecoveryAction, Signal,
    WriteFailure,
};
use super::store;
use super::write_proxy::{Fault, Observation, Proxy};
use crate::authority::client;
use crate::fixture::Fixture;
use crate::model::RPC_BUDGET;

#[derive(Serialize)]
pub struct LostResponseCase {
    identity: Identity,
    fault: Fault,
    proxy_observation: Observation,
    writer_outcome: Outcome,
    caller_failure: WriteFailure,
    facts_before_recovery: Vec<Fact>,
    recovery_outcome: Outcome,
    facts_after_recovery: Vec<Fact>,
    newer_revoke: Option<Identity>,
    independent_scope_admitted: bool,
}

async fn facts(fixture: &Fixture, identity: &Identity) -> Result<Vec<Fact>> {
    let evidence = store::evidence(&mut connection(fixture).await?, identity).await?;
    ensure!(
        evidence.iter().all(|value| value.identity == *identity),
        "lost-response fact identity"
    );
    Ok(evidence.into_iter().map(|value| value.fact).collect())
}

async fn independent(binary: &Path, fixture: &Fixture, candidate: Candidate) -> Result<()> {
    let intent = Intent::grant();
    let identity = Identity::fresh(candidate, &intent);
    mutate(binary, fixture, &identity, &intent, None).await?;
    let mut admission = Contender::spawn(
        binary,
        fixture,
        &identity,
        RecoveryAction::Admit {
            pause_after_check: false,
        },
    )?;
    admission.done(Outcome::Admitted).await?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM recovery_admission WHERE attempt=$1")
        .bind(admission.work.attempt)
        .fetch_one(&mut connection(fixture).await?)
        .await?;
    ensure!(count == 1, "independent scope lacks committed admission");
    Ok(())
}

async fn no_writer_success(worker: &Contender) -> Result<()> {
    for name in ["acknowledged", "verified", "attempting"] {
        let exists = tokio::time::timeout(
            RPC_BUDGET,
            tokio::fs::try_exists(worker.work.barriers.join(name)),
        )
        .await??;
        ensure!(!exists, "unacknowledged writer recorded {name}");
    }
    Ok(())
}

pub async fn run(fixture: &Fixture, binary: &Path) -> Result<Vec<LostResponseCase>> {
    let mut cases = Vec::new();
    for candidate in [Candidate::Postgres, Candidate::SpiceDb] {
        for polarity in [Polarity::Grant, Polarity::Revoke] {
            for fault in [Fault::DropSuccess, Fault::NeverForward] {
                cases.push(exercise(fixture, binary, candidate, polarity, fault, false).await?);
            }
            cases.push(
                exercise(
                    fixture,
                    binary,
                    candidate,
                    polarity,
                    Fault::DropSuccess,
                    true,
                )
                .await?,
            );
        }
    }
    ensure!(cases.len() == 12, "lost-response scenario inventory");
    Ok(cases)
}

async fn exercise(
    fixture: &Fixture,
    binary: &Path,
    candidate: Candidate,
    polarity: Polarity,
    fault: Fault,
    supersede: bool,
) -> Result<LostResponseCase> {
    let (identity, intent, previous) = plan(binary, fixture, candidate, polarity).await?;
    let mut proxy = Proxy::spawn(binary, fixture, &identity, fault).await?;
    proxy.assert_rejected_requests().await?;
    let mut writer = Contender::spawn(
        binary,
        fixture,
        &identity,
        RecoveryAction::Mutate {
            intent: intent.clone(),
            previous,
            pause: Pause::None,
            write_proxy_port: Some(proxy.port),
        },
    )?;
    writer.wait("closed").await?;
    closed(fixture, &identity).await?;
    let observation = proxy.wait("observed").await?;
    ensure!(
        !tokio::time::timeout(
            RPC_BUDGET,
            tokio::fs::try_exists(writer.work.barriers.join("done"))
        )
        .await??,
        "proxy must observe its fault boundary before the caller deadline completes"
    );
    ensure!(
        observation
            == match fault {
                Fault::DropSuccess => Observation::BackendAcknowledged,
                Fault::NeverForward => Observation::RequestNotForwarded,
            },
        "wrong fault boundary observed"
    );
    ensure!(
        graph::verify(&client(&fixture.settings).await?, &identity)
            .await?
            .is_some()
            == (fault == Fault::DropSuccess),
        "actual canonical pair differs from proxy observation"
    );
    closed(fixture, &identity).await?;
    // No success reply has been released. The writer may time out, but neither
    // that timeout nor another scope's activity can activate this operation.
    no_writer_success(&writer).await?;
    independent(binary, fixture, candidate).await?;
    let newer_revoke = if supersede {
        let revoked = intent.revoke();
        let newer = identity.next(&revoked)?;
        mutate(binary, fixture, &newer, &revoked, Some(&identity)).await?;
        closed(fixture, &newer).await?;
        Some(newer)
    } else {
        None
    };
    writer.done(Outcome::AuthorityUnavailable).await?;
    ensure!(
        writer.wait("write_failed").await?.signal
            == Signal::WriteFailed(WriteFailure::CallerDeadlineExceeded),
        "writer must reach its own local deadline before the proxy releases anything"
    );
    proxy.release()?;
    no_writer_success(&writer).await?;
    proxy.stop()?;
    let before = facts(fixture, &identity).await?;
    ensure!(
        before.contains(&Fact::AdmissionClosed),
        "uncertain operation lost fence fact"
    );
    ensure!(
        !before.contains(&Fact::ProjectionVerified),
        "writer inferred verification from failure"
    );
    ensure!(
        before.contains(&Fact::CanonicalMutationCommitted) == (candidate == Candidate::Postgres),
        "writer inferred canonical commitment from lost response"
    );
    assert_no_admission(binary, fixture, &identity).await?;

    let expected = if supersede {
        // A delayed retry really republishes the old exact marker/leaf in the
        // backend. It must not make recovery of that operation current again.
        graph::publish(&client(&fixture.settings).await?, &identity, &intent).await?;
        Outcome::Stale
    } else if fault == Fault::NeverForward && candidate == Candidate::SpiceDb {
        Outcome::CanonicalMissing
    } else {
        completed(&intent)
    };
    recover(binary, fixture, &identity, expected).await?;
    if let Some(newer) = &newer_revoke {
        closed(fixture, newer).await?;
        assert_no_admission(binary, fixture, newer).await?;
        assert_no_admission(binary, fixture, &identity).await?;
    } else if expected == Outcome::Activated {
        let mut admitted = Contender::spawn(
            binary,
            fixture,
            &identity,
            RecoveryAction::Admit {
                pause_after_check: false,
            },
        )?;
        admitted.done(Outcome::Admitted).await?;
    } else {
        closed(fixture, &identity).await?;
        assert_no_admission(binary, fixture, &identity).await?;
    }
    let after = facts(fixture, &identity).await?;
    if matches!(expected, Outcome::Activated | Outcome::VerifiedRevoke) {
        ensure!(
            after.contains(&Fact::CanonicalMutationCommitted)
                && after.contains(&Fact::ProjectionVerified),
            "fresh recovery lacks independent commitment and verification"
        );
    } else {
        ensure!(
            before == after,
            "failed or stale recovery changed historical facts"
        );
    }
    Ok(LostResponseCase {
        identity,
        fault,
        proxy_observation: observation,
        writer_outcome: Outcome::AuthorityUnavailable,
        caller_failure: WriteFailure::CallerDeadlineExceeded,
        facts_before_recovery: before,
        recovery_outcome: expected,
        facts_after_recovery: after,
        newer_revoke,
        independent_scope_admitted: true,
    })
}
