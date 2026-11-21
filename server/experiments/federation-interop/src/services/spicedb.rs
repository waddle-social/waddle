//! One SpiceDB 1.56.1 per Waddle deployment, on its deployment's
//! PostgreSQL (`--datastore-engine postgres`), with a preshared key
//! generated per run and passed only through the child's environment.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use tokio::process::Command;

use crate::process::{self, StopSignal, Supervised};

pub struct SpiceDb {
    pub label: String,
    pub port: u16,
    pub preshared_key: String,
    bin: PathBuf,
    datastore: String,
    logs: PathBuf,
    process: Option<Supervised>,
}

impl SpiceDb {
    pub async fn start(
        bin: &Path,
        logs: &Path,
        label: &str,
        port: u16,
        datastore: String,
    ) -> Result<Self> {
        let mut spicedb = Self {
            label: label.to_owned(),
            port,
            preshared_key: uuid::Uuid::new_v4().simple().to_string(),
            bin: bin.to_owned(),
            datastore,
            logs: logs.to_owned(),
            process: None,
        };
        let mut migrate = spicedb.command();
        migrate
            .arg("migrate")
            .arg("head")
            .arg("--migration-timeout=60s");
        process::run("spicedb migrate", migrate, Duration::from_secs(120)).await?;
        spicedb.launch().await?;
        Ok(spicedb)
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.bin);
        command
            .env_clear()
            .env("SPICEDB_DATASTORE_ENGINE", "postgres")
            .env("SPICEDB_DATASTORE_CONN_URI", &self.datastore)
            .env("SPICEDB_GRPC_PRESHARED_KEY", &self.preshared_key)
            .arg("--skip-release-check")
            .arg("--log-format=json")
            .arg("--log-level=warn");
        command
    }

    async fn launch(&mut self) -> Result<()> {
        let mut command = self.command();
        command
            .arg("serve")
            .arg(["--grpc-addr=127.0.0.1:", &self.port.to_string()].concat())
            .arg("--dispatch-cluster-enabled=false")
            .arg("--http-enabled=false")
            .arg("--metrics-enabled=false")
            .arg("--telemetry-endpoint=");
        self.process = Some(Supervised::spawn(
            &self.label,
            command,
            &self.logs,
            StopSignal::TermGroup,
        )?);
        process::wait_tcp(&self.label, self.port, Duration::from_secs(60)).await
    }

    pub fn endpoint(&self) -> String {
        ["http://127.0.0.1:", &self.port.to_string()].concat()
    }

    pub async fn stop(&mut self) -> Result<()> {
        if let Some(mut process) = self.process.take() {
            process.stop(Duration::from_secs(20)).await?;
        }
        Ok(())
    }
}
