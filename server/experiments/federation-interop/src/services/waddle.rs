//! A native `waddle-server` process for one deployment: its own
//! PostgreSQL database, SpiceDB, session and occupant-ID secrets, and
//! deployment UUID; S2S enabled with a configuration file, or disabled for
//! the first part of step 6.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tokio::process::Command;
use tokio::time::Instant;

use crate::net::http;
use crate::process::{StopSignal, Supervised};
use crate::topology::{Home, Ports};

/// The native accounts every deployment provisions. `admin` is the
/// fixed test account and a server owner.
pub const ACCOUNTS: [&str; 5] = ["admin", "alice", "bob", "carol", "caseprobe"];

/// How S2S is configured for one start.
#[derive(Debug, Clone)]
pub enum S2s {
    /// `WADDLE_S2S_ENABLED` unset; `WADDLE_S2S_CONFIG_FILE` points at
    /// `unread`, a path that does not exist, so a read would fail startup.
    Disabled {
        unread: PathBuf,
    },
    Enabled {
        config: PathBuf,
    },
}

/// Everything one deployment's process is started with.
#[derive(Debug, Clone)]
pub struct Spec {
    pub home: Home,
    pub binary: PathBuf,
    pub database_url: String,
    pub spicedb_endpoint: String,
    pub spicedb_key: String,
    pub passwords: BTreeMap<String, String>,
    pub deployment_uuid: String,
    pub session_key: String,
    pub occupant_secret: String,
    pub upload_dir: PathBuf,
    pub git_sha: String,
}

impl Spec {
    pub fn password(&self, account: &str) -> String {
        self.passwords.get(account).cloned().unwrap_or_default()
    }
}

pub struct Waddle {
    pub spec: Spec,
    pub s2s: S2s,
    process: Option<Supervised>,
    logs: PathBuf,
    starts: usize,
}

impl Waddle {
    pub fn new(spec: Spec, logs: &Path) -> Self {
        Self {
            spec,
            s2s: S2s::Disabled {
                unread: PathBuf::new(),
            },
            process: None,
            logs: logs.to_owned(),
            starts: 0,
        }
    }

    fn command(&self, ports: Ports) -> Command {
        let spec = &self.spec;
        let mut command = Command::new(&spec.binary);
        command.env_clear();
        for key in ["PATH", "HOME", "SSL_CERT_FILE", "NIX_SSL_CERT_FILE"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        let extra: Vec<String> = spec
            .passwords
            .iter()
            .filter(|(account, _)| account.as_str() != "admin")
            .map(|(account, password)| [account.as_str(), ":", password.as_str()].concat())
            .collect();
        command
            .env("WADDLE_LOG_LEVEL", "info,waddle_server::s2s=debug")
            .env("WADDLE_XMPP_DOMAIN", spec.home.domain())
            .env(
                "WADDLE_HTTP_ADDR",
                ["127.0.0.1:", &ports.waddle_http(spec.home).to_string()].concat(),
            )
            .env(
                "WADDLE_XMPP_PUBLIC_WEBSOCKET_URL",
                [
                    "wss://",
                    spec.home.domain(),
                    ":",
                    &ports.waddle_wss(spec.home).to_string(),
                    "/ws",
                ]
                .concat(),
            )
            .env("WADDLE_DB_DRIVER", "postgres")
            .env("WADDLE_DATABASE_URL", &spec.database_url)
            .env("WADDLE_DEPLOYMENT_UUID", &spec.deployment_uuid)
            .env("WADDLE_DB_LINEAGE_ACTION", "enroll")
            .env("WADDLE_SPICEDB_ENDPOINT", &spec.spicedb_endpoint)
            .env("WADDLE_SPICEDB_PRESHARED_KEY", &spec.spicedb_key)
            .env("WADDLE_CERTS_EPHEMERAL", "true")
            .env("WADDLE_TEST_FIXED_ACCOUNT_ENABLED", "true")
            .env("WADDLE_TEST_FIXED_ACCOUNT_PASSWORD", spec.password("admin"))
            .env("WADDLE_TEST_EXTRA_FIXED_ACCOUNTS", extra.join(","))
            .env("WADDLE_SERVER_OWNER_LOCALPARTS", "admin")
            .env("WADDLE_SESSION_KEY", &spec.session_key)
            .env("WADDLE_OCCUPANT_ID_SECRET", &spec.occupant_secret)
            .env("WADDLE_UPLOAD_DIR", &spec.upload_dir)
            .env("WADDLE_GIT_SHA", &spec.git_sha);
        match &self.s2s {
            S2s::Disabled { unread } => {
                command.env("WADDLE_S2S_CONFIG_FILE", unread);
            }
            S2s::Enabled { config } => {
                command
                    .env("WADDLE_S2S_ENABLED", "true")
                    .env("WADDLE_S2S_CONFIG_FILE", config);
            }
        }
        command
    }

    /// Start the process and wait until `/readyz` answers 200.
    pub async fn start(&mut self, ports: Ports, s2s: S2s) -> Result<()> {
        anyhow::ensure!(
            self.process.is_none(),
            "{} is already running",
            self.spec.home.label()
        );
        self.s2s = s2s;
        self.starts += 1;
        let label = [self.spec.home.label(), "-", &self.starts.to_string()].concat();
        let process = Supervised::spawn(
            &label,
            self.command(ports),
            &self.logs,
            StopSignal::TermGroup,
        )?;
        self.process = Some(process);
        self.wait_ready(ports, Duration::from_secs(240)).await
    }

    /// Start with a configuration that must refuse to load: the process
    /// must exit unsuccessfully without ever becoming ready.
    pub async fn start_expecting_failure(
        &mut self,
        ports: Ports,
        config: PathBuf,
    ) -> Result<String> {
        anyhow::ensure!(
            self.process.is_none(),
            "{} is already running",
            self.spec.home.label()
        );
        self.s2s = S2s::Enabled { config };
        self.starts += 1;
        let label = [
            self.spec.home.label(),
            "-refused-",
            &self.starts.to_string(),
        ]
        .concat();
        let mut process = Supervised::spawn(
            &label,
            self.command(ports),
            &self.logs,
            StopSignal::TermGroup,
        )?;
        let deadline = Instant::now() + Duration::from_secs(120);
        while Instant::now() < deadline {
            if !process.running() {
                return Ok(process.log_tail(60));
            }
            if http::get(ports.waddle_http(self.spec.home), "/readyz")
                .await
                .is_ok_and(|response| response.status == 200)
            {
                process.stop(Duration::from_secs(30)).await?;
                bail!("the deployment became ready with a configuration that must not load");
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        process.stop(Duration::from_secs(30)).await?;
        bail!("the deployment neither failed nor became ready")
    }

    async fn wait_ready(&mut self, ports: Ports, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(process) = self.process.as_mut() {
                if !process.running() {
                    let tail = process.log_tail(40);
                    self.process = None;
                    bail!("{} exited during startup:\n{tail}", self.spec.home.label());
                }
            }
            if http::get(ports.waddle_http(self.spec.home), "/readyz")
                .await
                .is_ok_and(|response| response.status == 200)
            {
                return Ok(());
            }
            if Instant::now() >= deadline {
                bail!("{} not ready within {timeout:?}", self.spec.home.label());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    pub async fn stop(&mut self) -> Result<()> {
        if let Some(mut process) = self.process.take() {
            process
                .stop(Duration::from_secs(60))
                .await
                .with_context(|| format!("stop {}", self.spec.home.label()))?;
        }
        Ok(())
    }

    /// Kill without a graceful shutdown.
    pub async fn crash(&mut self) -> Result<()> {
        if let Some(mut process) = self.process.take() {
            process.kill().await?;
        }
        Ok(())
    }

    pub async fn restart(&mut self, ports: Ports, s2s: S2s) -> Result<()> {
        self.stop().await?;
        self.start(ports, s2s).await
    }

    pub fn running(&mut self) -> bool {
        self.process.as_mut().is_some_and(Supervised::running)
    }

    pub fn log_path(&self) -> Option<PathBuf> {
        self.process.as_ref().map(|process| process.log.clone())
    }

    /// The deployment's Prometheus metrics.
    pub async fn metrics(&self, ports: Ports) -> Result<String> {
        let response = http::get(ports.waddle_http(self.spec.home), "/metrics").await?;
        anyhow::ensure!(response.status == 200, "metrics status {}", response.status);
        Ok(response.body)
    }
}
