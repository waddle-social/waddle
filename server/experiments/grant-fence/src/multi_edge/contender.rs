use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use sqlx::PgConnection;
use uuid::Uuid;

use super::model::{Action, Observation, Outcome, Work};
use crate::fixture::Fixture;
use crate::model::{write_private, WorkerRequest, BARRIER_BUDGET};
use crate::process::Process;

pub fn observe(work: &Work, name: &str, outcome: Option<Outcome>, pid: i32) -> Result<()> {
    let temp = work.barriers.join(format!("{name}.writing"));
    write_private(&temp, &serde_json::to_vec(&Observation { outcome, pid })?)?;
    fs::rename(temp, work.barriers.join(name))?;
    Ok(())
}
pub struct Contender {
    pub process: Process,
    pub work: Work,
}
impl Contender {
    pub fn spawn(fixture: &Fixture, binary: &Path, action: Action) -> Result<Self> {
        let barriers = fixture
            .settings
            .root
            .join(Uuid::new_v4().simple().to_string());
        fs::create_dir(&barriers)?;
        fs::set_permissions(&barriers, fs::Permissions::from_mode(0o700))?;
        let work = Work {
            settings: fixture.settings.clone(),
            barriers,
            action,
        };
        let bytes = serde_json::to_vec(&WorkerRequest::MultiEdge(work.clone()))?;
        ensure!(bytes.len() <= 4096, "worker input exceeds bounded pipe");
        let mut command = Command::new(binary);
        command.env_clear();
        let process = fixture.spawn_worker(&mut command, Some(&bytes))?;
        Ok(Self { process, work })
    }
    async fn read(
        &self,
        name: &str,
        deadline: tokio::time::Instant,
    ) -> Result<Option<Observation>> {
        match tokio::time::timeout_at(deadline, tokio::fs::read(self.work.barriers.join(name)))
            .await
            .context("barrier read timeout")?
        {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
    pub async fn wait(&mut self, name: &str) -> Result<Observation> {
        let deadline = tokio::time::Instant::now() + BARRIER_BUDGET;
        loop {
            if let Some(event) = self.read(name, deadline).await? {
                return Ok(event);
            }
            if self.process.status()?.is_some() {
                return self
                    .read(name, deadline)
                    .await?
                    .with_context(|| format!("multi-edge worker exited before {name}"));
            }
            ensure!(
                tokio::time::Instant::now() < deadline,
                "multi-edge barrier timeout: {name}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    pub fn release(&self) -> Result<()> {
        write_private(&self.work.barriers.join("release"), b"release")
    }
    pub async fn done(&mut self, expected: Outcome) -> Result<()> {
        let event = self.wait("done").await?;
        ensure!(
            event.outcome == Some(expected),
            "multi-edge outcome {:?} expected {expected:?}",
            event.outcome
        );
        self.process.finish(BARRIER_BUDGET).await
    }
}
pub async fn blocked(connection: &mut PgConnection, waiter: i32, blocker: i32) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let observed: bool =
                sqlx::query_scalar("SELECT $2::integer = ANY(pg_blocking_pids($1::integer))")
                    .bind(waiter)
                    .bind(blocker)
                    .fetch_one(&mut *connection)
                    .await?;
            if observed {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("actual PostgreSQL blocker absent")?
}
