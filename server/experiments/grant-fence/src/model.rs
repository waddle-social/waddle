use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub use waddle_native_fixture::model::{write_private, Settings, RPC_BUDGET};
pub const BARRIER_BUDGET: Duration = Duration::from_secs(10);
pub const APP_SCHEMA: &str = include_str!("../schema/app.sql");
pub const SPICE_SCHEMA: &str = include_str!("../schema/spicedb.zed");

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Guard {
    pub scope: Uuid,
    pub incarnation: Uuid,
    pub generation: i64,
    pub operation: Uuid,
}

impl Guard {
    pub fn graph_id(&self) -> String {
        format!(
            "v1_{}_{}_{}",
            self.scope.simple(),
            self.incarnation.simple(),
            self.generation
        )
    }

    pub fn digest(&self) -> String {
        hex::encode(Sha256::digest(
            format!("{}#reader@fence_user:v1_alice", self.graph_id()).as_bytes(),
        ))
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub enum Action {
    Admit {
        guarded: bool,
        pause_after_check: bool,
        hold_lock: bool,
        lock_ms: u32,
    },
    Revoke {
        hold_lock: bool,
    },
    Publish {
        pause_before_write: bool,
    },
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Work {
    pub settings: Settings,
    pub guard: Guard,
    pub attempt: Uuid,
    pub action: Action,
    pub barriers: PathBuf,
}

#[derive(Clone, Serialize, Deserialize)]
pub enum WorkerRequest {
    Guard(Work),
    MultiEdge(crate::multi_edge::Work),
    Recovery(crate::recovery::RecoveryWork),
    WriteProxy(crate::recovery::ProxyWork),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    pub outcome: String,
    pub backend_pid: Option<i32>,
}

pub fn event(directory: &Path, name: &str, outcome: &str, backend_pid: Option<i32>) -> Result<()> {
    let temporary = directory.join(format!("{name}.writing"));
    write_private(
        &temporary,
        &serde_json::to_vec(&Event {
            outcome: outcome.to_owned(),
            backend_pid,
        })?,
    )?;
    fs::rename(temporary, directory.join(name))?;
    Ok(())
}

pub async fn barrier(directory: &Path, name: &str) -> Result<()> {
    let deadline = tokio::time::Instant::now() + BARRIER_BUDGET;
    loop {
        if tokio::time::timeout_at(deadline, tokio::fs::try_exists(directory.join(name)))
            .await
            .context("worker barrier lookup deadline")??
        {
            return Ok(());
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            "worker barrier deadline: {name}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
