use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use serde::Serialize;
use sqlx::{Connection, PgConnection};
use uuid::Uuid;

use crate::authority::{check, client, grant, publish};
use crate::fixture::Fixture;
use crate::model::{write_private, Action, Event, Guard, Work, BARRIER_BUDGET};
use crate::process::Process;
use crate::provenance::Provenance;
use crate::worker::fresh_guard;

#[derive(Serialize)]
struct Case {
    name: &'static str,
    observations: Vec<String>,
    workers: Vec<BTreeMap<String, Event>>,
}

#[derive(Serialize)]
struct Report {
    provenance: Provenance,
    formal_evidence: bool,
    cleanup_verified: bool,
    versions: Vec<String>,
    authority: &'static str,
    outcome: String,
    cases: Vec<Case>,
    pending: Vec<&'static str>,
}

#[derive(Serialize)]
struct EvidenceSummary<'a> {
    git_head: &'a str,
    experiment_source_sha256: &'a str,
    source_clean: bool,
    formal_evidence: bool,
    cleanup_verified: bool,
    outcome: &'static str,
    versions: &'a [String],
    authority: &'static str,
    cases: &'a [Case],
    pending: &'a [&'static str],
}

struct Worker {
    process: Process,
    work: Work,
}

impl Worker {
    fn spawn(binary: &Path, fixture: &Fixture, guard: &Guard, action: Action) -> Result<Self> {
        let attempt = Uuid::new_v4();
        let barriers = fixture.settings.root.join(attempt.simple().to_string());
        fs::create_dir(&barriers)?;
        fs::set_permissions(&barriers, fs::Permissions::from_mode(0o700))?;
        let work = Work {
            settings: fixture.settings.clone(),
            guard: guard.clone(),
            attempt,
            action,
            barriers,
        };
        let mut command = Command::new(binary);
        command.env_clear();
        let process = Process::spawn(&mut command, Some(&serde_json::to_vec(&work)?), false)?;
        Ok(Self { process, work })
    }

    async fn read_event(
        &self,
        name: &str,
        deadline: tokio::time::Instant,
    ) -> Result<Option<Event>> {
        let path = self.work.barriers.join(name);
        match tokio::time::timeout_at(deadline, tokio::fs::read(path))
            .await
            .context("parent barrier read deadline")?
        {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(anyhow::Error::new(error).context("read worker barrier")),
        }
    }

    async fn wait(&mut self, name: &str) -> Result<Event> {
        let deadline = tokio::time::Instant::now() + BARRIER_BUDGET;
        loop {
            if let Some(event) = self.read_event(name, deadline).await? {
                return Ok(event);
            }
            if self.process.status()?.is_some() {
                // The process may have published and exited while the first
                // asynchronous read was pending. Exit orders the final read
                // after all its writes; a genuinely missing barrier fails.
                return self
                    .read_event(name, deadline)
                    .await?
                    .with_context(|| format!("worker exited before barrier {name}"));
            }
            ensure!(
                tokio::time::Instant::now() < deadline,
                "parent barrier deadline: {name}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn release(&self, name: &str) -> Result<()> {
        write_private(&self.work.barriers.join(name), b"release")
    }

    async fn done(&mut self, expected: &str) -> Result<()> {
        ensure!(
            self.wait("done").await?.outcome == expected,
            "worker outcome differed from {expected}"
        );
        self.process.finish(BARRIER_BUDGET).await
    }

    fn events(&self) -> Result<BTreeMap<String, Event>> {
        let mut events = BTreeMap::new();
        for name in ["ready", "checked", "attempting", "held", "closed", "done"] {
            if let Ok(bytes) = fs::read(self.work.barriers.join(name)) {
                events.insert(name.to_owned(), serde_json::from_slice(&bytes)?);
            }
        }
        Ok(events)
    }
}

fn admission(pause_after_check: bool, hold_lock: bool) -> Action {
    Action::Admit {
        guarded: true,
        pause_after_check,
        hold_lock,
        lock_ms: 3000,
    }
}

async fn count(connection: &mut PgConnection, attempt: Uuid) -> Result<i64> {
    Ok(
        sqlx::query_scalar("SELECT count(*) FROM admitted_effect WHERE attempt=$1")
            .bind(attempt)
            .fetch_one(connection)
            .await?,
    )
}

async fn blocked(connection: &mut PgConnection, waiter: i32, blocker: i32) -> Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let blocked: bool =
            sqlx::query_scalar("SELECT $2::integer = ANY(pg_blocking_pids($1::integer))")
                .bind(waiter)
                .bind(blocker)
                .fetch_one(&mut *connection)
                .await?;
        if blocked {
            return Ok(());
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            "expected PostgreSQL lock dependency was not observed"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn record(
    report: &mut Report,
    name: &'static str,
    observations: &[&str],
    workers: &[&Worker],
) -> Result<()> {
    report.cases.push(Case {
        name,
        observations: observations.iter().map(|note| (*note).to_owned()).collect(),
        workers: workers
            .iter()
            .map(|worker| worker.events())
            .collect::<Result<_>>()?,
    });
    Ok(())
}

pub async fn run(worker_binary: &str) -> Result<()> {
    let mut fixture = Fixture::new()?;
    let provenance = Provenance::capture().await?;
    let require_pinned = std::env::var("FENCE_REQUIRE_PINNED").as_deref() == Ok("1");
    let mut report = Report {
        formal_evidence: provenance.clean, provenance: provenance.clone(), cleanup_verified: false, versions: Vec::new(),
        authority: "PostgreSQL canonical grant, immutable generation-qualified single-edge SpiceDB projection",
        outcome: "running".to_owned(), cases: Vec::new(),
        pending: vec![
            "SpiceDB-canonical authority variant and P/S comparison",
            "unknown accepted external-write outcomes and durable recovery protocol",
            "PostgreSQL process restart and physical/logical restore",
            "graph loss/rebuild and multi-edge dependency closure",
            "schema/dependency drift and immutable schema enforcement",
            "cross-scope moves and complete delete/recreate lifecycle writer closure",
            "production writer inventory, codecs, actors, cluster claims, ingress frozen-child contract",
            "sink/byte-level drain after acknowledgement and end-to-end federation",
        ],
    };
    let mut outcome = async {
        ensure!(
            !require_pinned || provenance.clean,
            "formal evidence requires a clean committed experiment source"
        );
        report.versions = fixture.versions().await?;
        fixture.start().await?;
        exercise(&mut fixture, Path::new(worker_binary), &mut report).await
    }
    .await;
    if let Err(error) = fixture.shutdown() {
        outcome = Err(error.context("fixture cleanup"));
    } else {
        report.cleanup_verified = true;
    }
    match Provenance::capture().await {
        Ok(after) if after == provenance => {}
        Ok(_) => outcome = Err(anyhow::anyhow!("source/revision changed during experiment")),
        Err(error) => outcome = Err(error.context("final source provenance")),
    }
    report.outcome = match &outcome {
        Ok(()) => "passed".to_owned(),
        Err(error) => format!("failed: {error:#}"),
    };
    fixture.save("report.json", &report)?;
    eprintln!("grant-fence evidence: {}", fixture.evidence.display());
    let summary = EvidenceSummary {
        git_head: &report.provenance.git_head,
        experiment_source_sha256: &report.provenance.experiment_source_sha256,
        source_clean: report.provenance.clean,
        formal_evidence: report.formal_evidence,
        cleanup_verified: report.cleanup_verified,
        outcome: if outcome.is_ok() { "passed" } else { "failed" },
        versions: &report.versions,
        authority: report.authority,
        cases: &report.cases,
        pending: &report.pending,
    };
    let summary = serde_json::to_vec(&summary)?;
    ensure!(summary.len() <= 65536, "CI evidence summary exceeds 64KiB");
    fixture.assert_public_evidence(&summary)?;
    println!("grant-fence CI evidence: {}", String::from_utf8(summary)?);
    outcome
}

async fn exercise(fixture: &mut Fixture, binary: &Path, report: &mut Report) -> Result<()> {
    let mut app = fixture
        .settings
        .connect("fence_app", "fence_app", "proof-observer", 3000)
        .await?;
    let mut observer = fixture
        .settings
        .connect("fence_admin", "postgres", "lock-observer", 3000)
        .await?;

    // Negative control: the same real permission check, deliberately no final guard.
    let guard = fresh_guard();
    grant(&fixture.settings, &guard, false).await?;
    let mut unsafe_reader = Worker::spawn(
        binary,
        fixture,
        &guard,
        Action::Admit {
            guarded: false,
            pause_after_check: true,
            hold_lock: false,
            lock_ms: 3000,
        },
    )?;
    unsafe_reader.wait("checked").await?;
    let mut revoker = Worker::spawn(binary, fixture, &guard, Action::Revoke { hold_lock: false })?;
    revoker.done("revoked_and_projection_deleted").await?;
    ensure!(
        !check(&client(&fixture.settings).await?, &guard).await?,
        "negative-control revoke must delete real SpiceDB grant"
    );
    ensure!(
        count(&mut app, unsafe_reader.work.attempt).await? == 0,
        "negative control was released prematurely"
    );
    unsafe_reader.release("after-check")?;
    unsafe_reader.done("admitted").await?;
    ensure!(
        count(&mut app, unsafe_reader.work.attempt).await? == 1,
        "check-then-effect control should expose race"
    );
    record(report, "unsafe_check_then_revoke_control", &["real FullyConsistent allow preceded revoke; release happened only after durable fence and confirmed SpiceDB deletion; unguarded effect then committed"], &[&unsafe_reader, &revoker])?;

    let guard = fresh_guard();
    grant(&fixture.settings, &guard, false).await?;
    let mut reader = Worker::spawn(binary, fixture, &guard, admission(true, false))?;
    reader.wait("checked").await?;
    let mut revoker = Worker::spawn(binary, fixture, &guard, Action::Revoke { hold_lock: false })?;
    revoker.done("revoked_and_projection_deleted").await?;
    reader.release("after-check")?;
    reader.done("denied_fence").await?;
    ensure!(
        count(&mut app, reader.work.attempt).await? == 0,
        "old checked generation must not admit after revoke"
    );
    record(
        report,
        "guarded_check_then_revoke",
        &["same allow-before-revoke schedule; final exact-generation guard denied admission"],
        &[&reader, &revoker],
    )?;

    // Admission wins: prove the exact lock dependency, never infer it from time.
    let guard = fresh_guard();
    grant(&fixture.settings, &guard, false).await?;
    let mut reader = Worker::spawn(binary, fixture, &guard, admission(false, true))?;
    let holder = reader
        .wait("held")
        .await?
        .backend_pid
        .context("admission backend PID")?;
    let mut revoker = Worker::spawn(binary, fixture, &guard, Action::Revoke { hold_lock: false })?;
    let waiter = revoker
        .wait("attempting")
        .await?
        .backend_pid
        .context("revoke backend PID")?;
    blocked(&mut observer, waiter, holder).await?;
    reader.release("release")?;
    reader.done("admitted").await?;
    revoker.done("revoked_and_projection_deleted").await?;
    ensure!(
        count(&mut app, reader.work.attempt).await? == 1,
        "pre-fence admitted effect should survive"
    );
    record(report, "admission_lock_precedes_revoke", &["pg_blocking_pids proved revoker blocked on admission transaction; admission committed before revoker could close guard; pre-admitted effect remains"], &[&reader, &revoker])?;

    // Revoke wins: the permission graph still allows while the SQL fence is held.
    let guard = fresh_guard();
    grant(&fixture.settings, &guard, false).await?;
    let mut revoker = Worker::spawn(binary, fixture, &guard, Action::Revoke { hold_lock: true })?;
    let holder = revoker
        .wait("held")
        .await?
        .backend_pid
        .context("revoke backend PID")?;
    let mut reader = Worker::spawn(binary, fixture, &guard, admission(false, false))?;
    reader.wait("checked").await?;
    let waiter = reader
        .wait("attempting")
        .await?
        .backend_pid
        .context("admission backend PID")?;
    blocked(&mut observer, waiter, holder).await?;
    revoker.release("release")?;
    revoker.done("revoked_and_projection_deleted").await?;
    reader.done("denied_fence").await?;
    ensure!(
        count(&mut app, reader.work.attempt).await? == 0,
        "revocation-first ordering must deny"
    );
    record(report, "revoke_lock_precedes_admission", &["real permission check allowed; pg_blocking_pids proved admission waited on revoker; committed closed guard denied it"], &[&reader, &revoker])?;

    stale_versions(fixture, binary, report, &mut app).await?;
    failures(fixture, binary, report, &mut app, &mut observer).await
}

async fn stale_versions(
    fixture: &Fixture,
    binary: &Path,
    report: &mut Report,
    app: &mut PgConnection,
) -> Result<()> {
    for recreate in [false, true] {
        let old = fresh_guard();
        grant(&fixture.settings, &old, false).await?;
        let mut publisher = Worker::spawn(
            binary,
            fixture,
            &old,
            Action::Publish {
                pause_before_write: true,
            },
        )?;
        publisher.wait("ready").await?;
        let mut revoker =
            Worker::spawn(binary, fixture, &old, Action::Revoke { hold_lock: false })?;
        revoker.done("revoked_and_projection_deleted").await?;
        let current = Guard {
            scope: old.scope,
            incarnation: if recreate {
                Uuid::new_v4()
            } else {
                old.incarnation
            },
            generation: if recreate { 1 } else { 3 },
            operation: Uuid::new_v4(),
        };
        grant(&fixture.settings, &current, recreate).await?;
        publisher.release("publish")?;
        publisher.done("stale_publication_rejected").await?;
        ensure!(
            check(&client(&fixture.settings).await?, &old).await?,
            "stale write must really reach SpiceDB"
        );
        let mut old_reader = Worker::spawn(binary, fixture, &old, admission(false, false))?;
        old_reader.done("denied_fence").await?;
        let mut new_reader = Worker::spawn(binary, fixture, &current, admission(false, false))?;
        new_reader.done("admitted").await?;
        ensure!(
            count(app, old_reader.work.attempt).await? == 0
                && count(app, new_reader.work.attempt).await? == 1,
            "ABA admission isolation"
        );
        record(report, if recreate { "stale_projection_after_new_incarnation_same_generation" } else { "stale_projection_after_revoke_regrant" }, &["stale old-generation grant was republished and allowed in real SpiceDB; exact operation/incarnation/generation activation lost; old admission denied while current admission succeeded"], &[&publisher, &revoker, &old_reader, &new_reader])?;
    }
    Ok(())
}

async fn failures(
    fixture: &mut Fixture,
    binary: &Path,
    report: &mut Report,
    app: &mut PgConnection,
    observer: &mut PgConnection,
) -> Result<()> {
    let guard = fresh_guard();
    let independent = fresh_guard();
    grant(&fixture.settings, &guard, false).await?;
    grant(&fixture.settings, &independent, false).await?;
    let mut reader = Worker::spawn(binary, fixture, &guard, admission(false, true))?;
    let holder = reader
        .wait("held")
        .await?
        .backend_pid
        .context("held admission backend PID")?;
    let mut revoker = Worker::spawn(binary, fixture, &guard, Action::Revoke { hold_lock: false })?;
    let waiter = revoker
        .wait("attempting")
        .await?
        .backend_pid
        .context("revoker PID")?;
    blocked(observer, waiter, holder).await?;
    let mut other = Worker::spawn(binary, fixture, &independent, admission(false, false))?;
    other.done("admitted").await?;
    reader.process.stop()?;
    revoker.done("revoked_and_projection_deleted").await?;
    ensure!(
        count(app, reader.work.attempt).await? == 0,
        "dead holder transaction must roll back"
    );
    publish(&client(&fixture.settings).await?, &guard).await?;
    let mut restarted = Worker::spawn(binary, fixture, &guard, admission(false, false))?;
    restarted.done("denied_fence").await?;
    record(report, "holder_process_death_and_fresh_worker", &["exact SQL blocker observed; independent scope admitted while lock held; killed admission process rolled back; revoker completed; fresh process with old real allow was fenced"], &[&reader, &revoker, &other, &restarted])?;

    let mut locker = fixture
        .settings
        .connect("fence_app", "fence_app", "bounded-lock-holder", 3000)
        .await?;
    let mut tx = locker.begin().await?;
    sqlx::query("SELECT scope FROM scope_guard WHERE scope=$1 FOR UPDATE")
        .bind(independent.scope)
        .fetch_one(&mut *tx)
        .await?;
    let mut timed = Worker::spawn(
        binary,
        fixture,
        &independent,
        Action::Admit {
            guarded: true,
            pause_after_check: false,
            hold_lock: false,
            lock_ms: 200,
        },
    )?;
    timed.done("denied_lock_timeout").await?;
    ensure!(
        count(app, timed.work.attempt).await? == 0,
        "lock timeout must not admit"
    );
    tx.rollback().await?;
    record(report, "bounded_postgres_lock_failure", &["PostgreSQL returned lock_timeout SQLSTATE55P03; no effect committed; parent transaction released"], &[&timed])?;

    fixture.stop_spice()?;
    let mut unavailable = Worker::spawn(binary, fixture, &independent, admission(false, false))?;
    unavailable.done("denied_authority_unavailable").await?;
    ensure!(
        count(app, unavailable.work.attempt).await? == 0,
        "SpiceDB outage must not admit"
    );
    fixture.start_spice().await?;
    ensure!(
        check(&client(&fixture.settings).await?, &independent).await?,
        "Postgres-backed SpiceDB grant survived restart"
    );
    let mut recovered = Worker::spawn(binary, fixture, &independent, admission(false, false))?;
    recovered.done("admitted").await?;
    let mut old = Worker::spawn(binary, fixture, &guard, admission(false, false))?;
    old.done("denied_fence").await?;
    record(report, "spicedb_outage_and_same_datastore_restart", &["authority connection/RPC failed within explicit budgets and admitted nothing; restarted native SpiceDB on same dedicated database preserved graph; active scope admitted; revoked guard remained closed despite stale allowed graph"], &[&unavailable, &recovered, &old])?;
    Ok(())
}
