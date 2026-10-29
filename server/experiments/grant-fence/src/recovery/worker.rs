use anyhow::{ensure, Context, Result};
use sqlx::{Connection, Row};

use super::contender::observe;
use super::graph;
use super::model::{
    Candidate, Fact, Outcome, Pause, Polarity, RecoveryAction, RecoveryWork, Signal, WriteFailure,
};
use super::store;
use crate::authority::client;
use crate::model::{barrier, BARRIER_BUDGET, RPC_BUDGET};

pub async fn execute(work: &RecoveryWork) -> Result<()> {
    work.settings.validate()?;
    ensure!(
        work.barriers.starts_with(&work.settings.root),
        "recovery barriers must be fixture-owned"
    );
    match &work.action {
        RecoveryAction::Mutate {
            intent,
            previous,
            pause,
            write_proxy_port,
        } => {
            let mut connection = work
                .settings
                .connect("fence_app", "fence_app", "recovery-mutation", 3000)
                .await?;
            let pid = sqlx::query_scalar("SELECT pg_backend_pid()")
                .fetch_one(&mut connection)
                .await?;
            let tx =
                store::prepare(&mut connection, &work.identity, intent, previous.as_ref()).await?;
            if *pause == Pause::BeforeFenceCommit {
                observe(work, "held", Signal::FenceHeld, Some(pid))?;
                barrier(&work.barriers, "release").await?;
            }
            tx.commit().await?;
            observe(work, "closed", Signal::AdmissionClosed, Some(pid))?;
            // Any acknowledged external write below begins with a committed
            // closed fence. It never holds a PostgreSQL lock across the RPC.
            ensure!(
                store::current(&mut connection, &work.identity, false).await?,
                "mutation fence no longer current before external write"
            );
            let write = async {
                let endpoint = crate::authority::client_at_port(
                    &work.settings,
                    write_proxy_port.unwrap_or(work.settings.grpc_port),
                    if write_proxy_port.is_some() {
                        BARRIER_BUDGET
                    } else {
                        RPC_BUDGET
                    },
                )
                .await?;
                if write_proxy_port.is_some() {
                    // This local deadline expires before prescience/tonic's
                    // longer endpoint deadline. Do not fabricate a gRPC code.
                    match tokio::time::timeout(
                        RPC_BUDGET,
                        graph::publish(&endpoint, &work.identity, intent),
                    )
                    .await
                    {
                        Ok(result) => result,
                        Err(_) => {
                            observe(
                                work,
                                "write_failed",
                                Signal::WriteFailed(WriteFailure::CallerDeadlineExceeded),
                                None,
                            )?;
                            return Ok(false);
                        }
                    }?;
                } else {
                    graph::publish(&endpoint, &work.identity, intent).await?;
                }
                Ok::<bool, anyhow::Error>(true)
            }
            .await;
            match write {
                Ok(true) => {}
                Ok(false) => return done(work, Outcome::AuthorityUnavailable),
                Err(error) => {
                    if write_proxy_port.is_some() {
                        let failure = error
                            .downcast_ref::<prescience::Error>()
                            .and_then(prescience::Error::code)
                            .map(|code| WriteFailure::GrpcStatus(code as i32))
                            .unwrap_or(WriteFailure::Other);
                        observe(work, "write_failed", Signal::WriteFailed(failure), None)?;
                    }
                    return done(work, Outcome::AuthorityUnavailable);
                }
            }
            if work.identity.candidate == Candidate::SpiceDb {
                store::fact(
                    &mut connection,
                    &work.identity,
                    Fact::CanonicalMutationCommitted,
                )
                .await?;
            }
            observe(work, "acknowledged", Signal::AcknowledgedWrite, None)?;
            if *pause == Pause::AfterAcknowledgedWrite {
                barrier(&work.barriers, "release").await?;
            }
            finish(work, *pause).await
        }
        RecoveryAction::Recover { pause } => {
            let mut connection = work
                .settings
                .connect("fence_app", "fence_app", "fresh-recovery", 3000)
                .await?;
            if !store::current(&mut connection, &work.identity, false).await? {
                return done(work, Outcome::Stale);
            }
            if work.identity.candidate == Candidate::Postgres {
                let intent = store::canonical_intent(&mut connection, &work.identity).await?;
                let write = async {
                    graph::publish(&client(&work.settings).await?, &work.identity, &intent).await
                }
                .await;
                if write.is_err() {
                    return done(work, Outcome::AuthorityUnavailable);
                }
            }
            finish(work, *pause).await
        }
        RecoveryAction::Admit { pause_after_check } => admit(work, *pause_after_check).await,
    }
}

async fn finish(work: &RecoveryWork, pause: Pause) -> Result<()> {
    let verified =
        async { graph::verify(&client(&work.settings).await?, &work.identity).await }.await;
    let snapshot = match verified {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => return done(work, Outcome::CanonicalMissing),
        Err(_) => return done(work, Outcome::AuthorityUnavailable),
    };
    let mut connection = work
        .settings
        .connect("fence_app", "fence_app", "recovery-activation", 3000)
        .await?;
    if work.identity.candidate == Candidate::SpiceDb {
        store::fact(
            &mut connection,
            &work.identity,
            Fact::CanonicalMutationCommitted,
        )
        .await?;
    }
    store::fact(&mut connection, &work.identity, Fact::ProjectionVerified).await?;
    observe(work, "verified", Signal::ProjectionVerified, None)?;
    if pause == Pause::BeforeActivation {
        observe(work, "ready", Signal::BeforeActivation, None)?;
        barrier(&work.barriers, "release").await?;
    }
    let pid = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut connection)
        .await?;
    observe(work, "attempting", Signal::ActivationAttempt, Some(pid))?;
    let current = if snapshot.intent.polarity == Polarity::Grant {
        store::activate(&mut connection, &work.identity).await?
    } else {
        store::current(&mut connection, &work.identity, false).await?
    };
    done(
        work,
        if !current {
            Outcome::Stale
        } else if snapshot.intent.polarity == Polarity::Grant {
            Outcome::Activated
        } else {
            Outcome::VerifiedRevoke
        },
    )
}

async fn admit(work: &RecoveryWork, pause: bool) -> Result<()> {
    let allowed =
        async { graph::allowed(&client(&work.settings).await?, &work.identity).await }.await;
    if !matches!(allowed, Ok(true)) {
        return done(work, Outcome::Denied);
    }
    observe(work, "checked", Signal::Checked, None)?;
    if pause {
        barrier(&work.barriers, "release").await?;
    }
    let mut connection = work
        .settings
        .connect("fence_app", "fence_app", "recovery-admission", 3000)
        .await?;
    let mut tx = connection.begin().await?;
    let row = sqlx::query("SELECT * FROM recovery_guard WHERE candidate=$1 AND scope=$2 FOR SHARE")
        .bind(work.identity.candidate.storage())
        .bind(work.identity.scope)
        .fetch_optional(&mut *tx)
        .await?;
    let allowed = row
        .as_ref()
        .map(|row| -> Result<bool> {
            Ok(store::matches(row, &work.identity)? && row.try_get::<bool, _>("active")?)
        })
        .transpose()?
        .unwrap_or(false);
    if !allowed {
        tx.rollback().await?;
        return done(work, Outcome::Denied);
    }
    sqlx::query("INSERT INTO recovery_admission(attempt,candidate,scope,incarnation,generation,operation,digest) VALUES($1,$2,$3,$4,$5,$6,$7)")
        .bind(work.attempt).bind(work.identity.candidate.storage()).bind(work.identity.scope).bind(work.identity.incarnation).bind(work.identity.generation).bind(work.identity.operation).bind(work.identity.digest_hex()).execute(&mut *tx).await?;
    tx.commit().await.context("recovery admission commit")?;
    done(work, Outcome::Admitted)
}

fn done(work: &RecoveryWork, outcome: Outcome) -> Result<()> {
    observe(work, "done", Signal::Done(outcome), None)
}
