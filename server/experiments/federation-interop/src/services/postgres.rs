//! One PostgreSQL 17 cluster per Waddle deployment, listening on loopback
//! TCP with SCRAM authentication and a password generated per run. Each
//! cluster holds that deployment's Waddle database and its SpiceDB
//! datastore; nothing is shared between the deployments.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::process::Command;

use crate::process::{self, StopSignal, Supervised};

pub const USER: &str = "waddle_harness";

pub struct Postgres {
    pub label: String,
    pub port: u16,
    password: String,
    bin: PathBuf,
    data: PathBuf,
    owner_socket: Option<PathBuf>,
    logs: PathBuf,
    process: Option<Supervised>,
}

impl Postgres {
    /// Initialize a cluster under `root` and start it.
    pub async fn start(
        bin: &Path,
        root: &Path,
        logs: &Path,
        label: &str,
        port: u16,
        owner_fixture: bool,
    ) -> Result<Self> {
        let cluster = root.join(label);
        tokio::fs::create_dir_all(&cluster).await?;
        let data = cluster.join("data");
        let owner_socket = owner_fixture.then(|| cluster.join("owner-socket"));
        if let Some(socket) = &owner_socket {
            tokio::fs::DirBuilder::new()
                .mode(0o700)
                .create(socket)
                .await?;
            tokio::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o700)).await?;
        }
        let password = uuid::Uuid::new_v4().simple().to_string();
        let password_file = cluster.join("initdb-password");
        crate::process::write_private(&password_file, password.as_bytes())?;
        let mut initdb = Command::new(bin.join("initdb"));
        initdb
            .arg("-D")
            .arg(&data)
            .arg("-U")
            .arg(USER)
            .arg(if owner_socket.is_some() {
                "--auth-local=trust"
            } else {
                "--auth-local=scram-sha-256"
            })
            .arg("--auth-host=scram-sha-256")
            .arg("--no-locale")
            .arg("-E")
            .arg("UTF8")
            .arg("--pwfile")
            .arg(&password_file);
        let result = process::run("initdb", initdb, Duration::from_secs(120)).await;
        tokio::fs::remove_file(&password_file).await?;
        result?;
        let mut postgres = Self {
            label: label.to_owned(),
            port,
            password,
            bin: bin.to_owned(),
            data,
            owner_socket,
            logs: logs.to_owned(),
            process: None,
        };
        postgres.launch().await?;
        Ok(postgres)
    }

    async fn launch(&mut self) -> Result<()> {
        let mut command = Command::new(self.bin.join("postgres"));
        command
            .arg("-D")
            .arg(&self.data)
            .arg("-p")
            .arg(self.port.to_string())
            .arg("-c")
            .arg("listen_addresses=127.0.0.1")
            .arg("-c")
            .arg(format!(
                "unix_socket_directories={}",
                self.owner_socket
                    .as_ref()
                    .map_or_else(String::new, |socket| socket.display().to_string(),)
            ))
            .arg("-c")
            .arg("max_connections=200")
            .arg("-c")
            .arg("log_min_messages=warning");
        self.process = Some(Supervised::spawn(
            &self.label,
            command,
            &self.logs,
            StopSignal::InterruptProcess,
        )?);
        process::wait_tcp(&self.label, self.port, Duration::from_secs(60)).await
    }

    pub async fn stop(&mut self) -> Result<()> {
        if let Some(mut process) = self.process.take() {
            process.stop(Duration::from_secs(30)).await?;
        }
        Ok(())
    }

    /// A connection URL for `database`.
    pub fn url(&self, database: &str) -> String {
        [
            "postgres://",
            USER,
            ":",
            &self.password,
            "@127.0.0.1:",
            &self.port.to_string(),
            "/",
            database,
        ]
        .concat()
    }

    /// Private local endpoint for the explicit enrolled Space fixture.
    pub fn owner_fixture_url(&self) -> Option<String> {
        let socket = self.owner_socket.as_ref()?;
        let encoded: String = socket
            .as_os_str()
            .as_encoded_bytes()
            .iter()
            .map(|byte| format!("%{byte:02X}"))
            .collect();
        Some(format!(
            "postgresql:///waddle?host={encoded}&port={}&user={USER}",
            self.port,
        ))
    }

    pub async fn create_database(&self, name: &str) -> Result<()> {
        let mut command = Command::new(self.bin.join("createdb"));
        command
            .env("PGPASSWORD", &self.password)
            .arg("-h")
            .arg("127.0.0.1")
            .arg("-p")
            .arg(self.port.to_string())
            .arg("-U")
            .arg(USER)
            .arg(name);
        process::run("createdb", command, Duration::from_secs(60))
            .await
            .with_context(|| format!("create database {name} in {}", self.label))?;
        Ok(())
    }
}
