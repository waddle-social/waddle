use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use anyhow::{ensure, Context, Result};
use uuid::Uuid;

use super::model::{Identity, Observation, Outcome, RecoveryAction, RecoveryWork, Signal};
use crate::fixture::Fixture;
use crate::model::{write_private, WorkerRequest, BARRIER_BUDGET};
use crate::process::Process;

pub fn observe(
    work: &RecoveryWork,
    name: &str,
    signal: Signal,
    backend_pid: Option<i32>,
) -> Result<()> {
    let temporary = work.barriers.join(format!("{name}.writing"));
    write_private(
        &temporary,
        &serde_json::to_vec(&Observation {
            identity: work.identity.clone(),
            signal,
            backend_pid,
        })?,
    )?;
    fs::rename(temporary, work.barriers.join(name))?;
    Ok(())
}

pub struct Contender {
    pub process: Process,
    pub work: RecoveryWork,
}

impl Contender {
    pub fn spawn(
        binary: &Path,
        fixture: &Fixture,
        identity: &Identity,
        action: RecoveryAction,
    ) -> Result<Self> {
        let attempt = Uuid::new_v4();
        let barriers = fixture.settings.root.join(attempt.simple().to_string());
        fs::create_dir(&barriers)?;
        fs::set_permissions(&barriers, fs::Permissions::from_mode(0o700))?;
        let work = RecoveryWork {
            settings: fixture.settings.clone(),
            identity: identity.clone(),
            action,
            attempt,
            barriers,
        };
        let request = WorkerRequest::Recovery(work.clone());
        let bytes = serde_json::to_vec(&request)?;
        if matches!(work.action, RecoveryAction::Recover { .. }) {
            let wire = serde_json::to_value(&request)?;
            let recovery = &wire["Recovery"]["action"]["Recover"];
            ensure!(
                recovery.as_object().context("typed recovery action")?.len() == 1
                    && recovery.get("pause").is_some(),
                "fresh recovery action must contain no canonical payload"
            );
        }
        let mut command = Command::new(binary);
        command.env_clear();
        let process = Process::spawn_tracked(
            &mut command,
            Some(&bytes),
            false,
            fixture.cleanup_latch(),
            crate::cleanup::CleanupService::Worker,
        )?;
        Ok(Self { process, work })
    }

    async fn read(
        &self,
        name: &str,
        deadline: tokio::time::Instant,
    ) -> Result<Option<Observation>> {
        match tokio::time::timeout_at(deadline, tokio::fs::read(self.work.barriers.join(name)))
            .await
            .context("recovery barrier read deadline")?
        {
            Ok(bytes) => {
                let observation: Observation = serde_json::from_slice(&bytes)?;
                ensure!(
                    observation.identity == self.work.identity,
                    "recovery observation identity mismatch"
                );
                Ok(Some(observation))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(anyhow::Error::new(error).context("read recovery barrier")),
        }
    }
    pub async fn wait(&mut self, name: &str) -> Result<Observation> {
        let deadline = tokio::time::Instant::now() + BARRIER_BUDGET;
        loop {
            if let Some(observation) = self.read(name, deadline).await? {
                return Ok(observation);
            }
            if self.process.status()?.is_some() {
                return self
                    .read(name, deadline)
                    .await?
                    .with_context(|| format!("recovery worker exited before {name}"));
            }
            ensure!(
                tokio::time::Instant::now() < deadline,
                "recovery barrier deadline: {name}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
    pub fn release(&self) -> Result<()> {
        write_private(&self.work.barriers.join("release"), b"release")
    }
    pub async fn done(&mut self, outcome: Outcome) -> Result<()> {
        ensure!(
            self.wait("done").await?.signal == Signal::Done(outcome),
            "recovery outcome differs from {outcome:?}"
        );
        self.process.finish(BARRIER_BUDGET).await
    }
}
