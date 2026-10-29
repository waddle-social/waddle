use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use anyhow::{ensure, Context, Result};

use crate::process::Process;

pub struct Binaries {
    pub postgres: PathBuf,
    pub initdb: PathBuf,
    pub spicedb: PathBuf,
}

impl Binaries {
    pub fn from_env() -> Result<Self> {
        let postgres = required("FENCE_POSTGRES_BIN")?;
        let spicedb = required("FENCE_SPICEDB_BIN")?;
        let initdb = postgres.join("initdb");
        let postgres = postgres.join("postgres");
        for binary in [&postgres, &initdb, &spicedb] {
            ensure!(
                binary.is_file(),
                "required native binary missing: {}",
                binary.display()
            );
        }
        Ok(Self {
            postgres,
            initdb,
            spicedb,
        })
    }

    pub async fn attest(&self) -> Result<Vec<String>> {
        let mut versions = Vec::new();
        for (binary, argument, expected) in [
            (&self.postgres, "--version", "postgres (PostgreSQL) 17.11"),
            (&self.initdb, "--version", "initdb (PostgreSQL) 17.11"),
            (&self.spicedb, "version", "spicedb v1.56.1"),
        ] {
            let mut command = Command::new(binary);
            command.env_clear().arg(argument);
            let mut process = Process::spawn(&mut command, None, true)?;
            process.finish(Duration::from_secs(3)).await?;
            let version = process.output()?.trim().to_owned();
            ensure!(
                version == expected,
                "native test binary version mismatch: expected {expected}"
            );
            versions.push(version);
        }
        versions.push("prescience 0.1.0; PostgreSQL-canonical leaf-grant prototype".to_owned());
        Ok(versions)
    }
}

fn required(name: &str) -> Result<PathBuf> {
    let path =
        PathBuf::from(std::env::var_os(name).with_context(|| format!("required {name} is unset"))?);
    ensure!(path.is_absolute(), "{name} must be an absolute path");
    Ok(path)
}
