use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::postgres::PgConnectOptions;
use sqlx::{Connection, PgConnection};
use uuid::Uuid;

pub const RPC_BUDGET: Duration = Duration::from_secs(2);
pub const BARRIER_BUDGET: Duration = Duration::from_secs(10);
pub const APP_SCHEMA: &str = include_str!("../schema/app.sql");
pub const SPICE_SCHEMA: &str = include_str!("../schema/spicedb.zed");

#[derive(Clone, Serialize, Deserialize)]
pub struct Settings {
    pub root: PathBuf,
    pub fixture: Uuid,
    pub app_password: String,
    #[serde(skip)]
    pub spice_password: String,
    #[serde(skip)]
    pub admin_password: String,
    pub preshared_key: String,
    pub grpc_port: u16,
}

impl Settings {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.root.starts_with("/tmp"),
            "fixture must live under /tmp"
        );
        ensure!(
            fs::metadata(&self.root)?.permissions().mode() & 0o777 == 0o700,
            "fixture directory must be 0700"
        );
        ensure!(
            fs::read_to_string(self.root.join("fixture-id"))? == self.fixture.to_string(),
            "fixture identity mismatch"
        );
        Ok(())
    }

    pub fn options(
        &self,
        role: &str,
        database: &str,
        name: &str,
        lock_ms: u32,
    ) -> Result<PgConnectOptions> {
        self.validate()?;
        let password = match role {
            "fence_admin" => &self.admin_password,
            "fence_app" => &self.app_password,
            "fence_spice" => &self.spice_password,
            _ => anyhow::bail!("unknown fixture role"),
        };
        ensure!(
            ["postgres", "fence_app", "fence_spice"].contains(&database),
            "unknown fixture database"
        );
        Ok(PgConnectOptions::new()
            .socket(self.root.join("socket"))
            .port(5432)
            .username(role)
            .password(password)
            .database(database)
            .application_name(name)
            .options([
                ("statement_timeout", "5000".to_owned()),
                ("lock_timeout", lock_ms.to_string()),
            ]))
    }

    pub async fn connect(
        &self,
        role: &str,
        database: &str,
        name: &str,
        lock_ms: u32,
    ) -> Result<PgConnection> {
        let options = self.options(role, database, name, lock_ms)?;
        tokio::time::timeout(RPC_BUDGET, PgConnection::connect_with(&options))
            .await
            .context("fixture PostgreSQL connection deadline")?
            .context("fixture PostgreSQL connection failed")
    }

    pub fn spice_uri(&self) -> Result<String> {
        self.validate()?;
        let mut uri = url::Url::parse("postgresql://fence_spice@localhost/fence_spice")?;
        uri.set_password(Some(&self.spice_password))
            .map_err(|_| anyhow::anyhow!("fixture URI password"))?;
        uri.query_pairs_mut()
            .append_pair(
                "host",
                self.root
                    .join("socket")
                    .to_str()
                    .context("UTF-8 fixture path")?,
            )
            .append_pair("sslmode", "disable");
        Ok(uri.into())
    }
}

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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    pub outcome: String,
    pub backend_pid: Option<i32>,
}

pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
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
