use std::fs;
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use serde::Serialize;
use sqlx::{Executor, Row};
use tempfile::TempDir;
use uuid::Uuid;

use crate::authority::client;
use crate::binaries::Binaries;
use crate::model::{write_private, Settings, APP_SCHEMA, SPICE_SCHEMA};
use crate::process::Process;

pub struct Fixture {
    root: Option<TempDir>,
    binaries: Binaries,
    pub settings: Settings,
    pub evidence: PathBuf,
    postgres: Option<Process>,
    spice: Option<Process>,
    spice_run: u32,
    cleanup_failed: bool,
    #[cfg(test)]
    inject_shutdown_failure: bool,
    #[cfg(test)]
    postgres_shutdown_budget: Duration,
}

impl Fixture {
    pub fn new() -> Result<Self> {
        let binaries = Binaries::from_env()?;
        let root = tempfile::Builder::new()
            .prefix("waddle-fence-private-")
            .tempdir_in("/tmp")?;
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700))?;
        let fixture = Uuid::new_v4();
        write_private(
            &root.path().join("fixture-id"),
            fixture.to_string().as_bytes(),
        )?;
        let evidence = PathBuf::from(format!("/tmp/waddle-fence-evidence-{fixture}"));
        fs::create_dir(&evidence)?;
        fs::set_permissions(&evidence, fs::Permissions::from_mode(0o700))?;
        let settings = Settings {
            root: root.path().to_owned(),
            fixture,
            grpc_port: 0,
            admin_password: Uuid::new_v4().simple().to_string(),
            app_password: Uuid::new_v4().simple().to_string(),
            spice_password: Uuid::new_v4().simple().to_string(),
            preshared_key: Uuid::new_v4().simple().to_string(),
        };
        Ok(Self {
            root: Some(root),
            binaries,
            settings,
            evidence,
            postgres: None,
            spice: None,
            spice_run: 0,
            cleanup_failed: false,
            #[cfg(test)]
            inject_shutdown_failure: false,
            #[cfg(test)]
            postgres_shutdown_budget: Duration::from_secs(5),
        })
    }

    pub async fn start(&mut self) -> Result<()> {
        self.binaries.attest().await?;
        let listener = TcpListener::bind("127.0.0.1:0").context("reserve loopback SpiceDB port")?;
        self.settings.grpc_port = listener.local_addr()?.port();
        drop(listener);
        fs::create_dir(self.settings.root.as_path().join("socket"))?;
        fs::set_permissions(
            self.settings.root.as_path().join("socket"),
            fs::Permissions::from_mode(0o700),
        )?;
        let mut init = Command::new(&self.binaries.initdb);
        init.env_clear()
            .env(
                "PATH",
                self.binaries
                    .postgres
                    .parent()
                    .context("PostgreSQL binary directory")?,
            )
            .env("LC_ALL", "C")
            .arg("-D")
            .arg(self.settings.root.as_path().join("pgdata"))
            .arg("--username=fence_admin")
            .arg("--no-locale")
            .arg("--encoding=UTF8")
            .arg("--auth-local=scram-sha-256")
            .arg("--auth-host=scram-sha-256")
            .arg("--pwfile=/dev/stdin");
        Process::spawn(
            &mut init,
            Some(self.settings.admin_password.as_bytes()),
            false,
        )?
        .finish(Duration::from_secs(30))
        .await?;
        let mut postgres = Command::new(&self.binaries.postgres);
        postgres
            .env_clear()
            .env(
                "PATH",
                self.binaries
                    .postgres
                    .parent()
                    .context("PostgreSQL binary directory")?,
            )
            .env("LC_ALL", "C")
            .arg("-D")
            .arg(self.settings.root.as_path().join("pgdata"))
            .arg("-h")
            .arg("")
            .arg("-k")
            .arg(self.settings.root.as_path().join("socket"))
            .arg("-p")
            .arg("5432")
            .arg("-c")
            .arg("max_connections=30")
            .arg("-c")
            .arg("logging_collector=off")
            .arg("-c")
            .arg("log_statement=none")
            .arg("-c")
            .arg("log_min_error_statement=panic")
            .arg("-c")
            .arg("log_min_messages=panic");
        self.postgres = Some(Process::postgres(&mut postgres)?);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        let mut admin = loop {
            if let Ok(connection) = self
                .settings
                .connect("fence_admin", "postgres", "fixture-bootstrap", 3000)
                .await
            {
                break connection;
            }
            ensure!(
                self.postgres
                    .as_mut()
                    .context("Postgres child")?
                    .status()?
                    .is_none(),
                "fixture PostgreSQL exited during startup"
            );
            ensure!(
                tokio::time::Instant::now() < deadline,
                "fixture PostgreSQL readiness deadline"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        // Only generated lowercase UUID hex enters these constant DDL templates.
        admin
            .execute(
                format!(
                    "CREATE ROLE fence_app LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE PASSWORD '{}'",
                    self.settings.app_password
                )
                .as_str(),
            )
            .await?;
        admin.execute(format!("CREATE ROLE fence_spice LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE PASSWORD '{}'", self.settings.spice_password).as_str()).await?;
        for (database, owner) in [("fence_app", "fence_app"), ("fence_spice", "fence_spice")] {
            admin
                .execute(format!("CREATE DATABASE {database} OWNER {owner}").as_str())
                .await?;
            admin
                .execute(format!("REVOKE ALL ON DATABASE {database} FROM PUBLIC").as_str())
                .await?;
        }
        drop(admin);
        for role in ["fence_app", "fence_spice"] {
            let mut connection = self
                .settings
                .connect(role, role, "fixture-attestation", 3000)
                .await?;
            let row = sqlx::query("SELECT current_database() AS db,current_user AS role,inet_server_addr() IS NULL AS unix_only,rolsuper,rolcreatedb,rolcreaterole FROM pg_roles WHERE rolname=current_user").fetch_one(&mut connection).await?;
            ensure!(
                row.try_get::<String, _>("db")? == role
                    && row.try_get::<String, _>("role")? == role,
                "fixture database/role mismatch"
            );
            ensure!(
                row.try_get::<bool, _>("unix_only")?
                    && !row.try_get::<bool, _>("rolsuper")?
                    && !row.try_get::<bool, _>("rolcreatedb")?
                    && !row.try_get::<bool, _>("rolcreaterole")?,
                "fixture requires Unix-only non-superuser roles"
            );
        }
        ensure!(
            self.settings
                .connect("fence_app", "fence_spice", "cross-database-denied", 3000)
                .await
                .is_err(),
            "application role must not enter SpiceDB database"
        );
        ensure!(
            self.settings
                .connect(
                    "fence_spice",
                    "fence_app",
                    "reverse-cross-database-denied",
                    3000
                )
                .await
                .is_err(),
            "SpiceDB role must not enter application database"
        );
        let mut app = self
            .settings
            .connect("fence_app", "fence_app", "fixture-schema", 3000)
            .await?;
        sqlx::raw_sql(APP_SCHEMA).execute(&mut app).await?;
        let mut migrate = self.spice_command()?;
        migrate
            .arg("migrate")
            .arg("head")
            .arg("--migration-timeout=30s");
        Process::spawn(&mut migrate, None, false)?
            .finish(Duration::from_secs(40))
            .await?;
        self.start_spice().await?;
        client(&self.settings)
            .await?
            .write_schema(SPICE_SCHEMA)
            .await?;
        Ok(())
    }

    fn spice_command(&self) -> Result<Command> {
        let mut command = Command::new(&self.binaries.spicedb);
        command
            .env_clear()
            .env("SPICEDB_DATASTORE_ENGINE", "postgres")
            .env("SPICEDB_DATASTORE_CONN_URI", self.settings.spice_uri()?)
            .env("SPICEDB_GRPC_PRESHARED_KEY", &self.settings.preshared_key)
            .arg("--skip-release-check")
            .arg("--log-format=json")
            .arg("--log-level=warn");
        Ok(command)
    }

    pub async fn start_spice(&mut self) -> Result<()> {
        ensure!(self.spice.is_none(), "SpiceDB already running");
        self.spice_run += 1;
        let mut command = self.spice_command()?;
        command
            .arg("serve")
            .arg(format!("--grpc-addr=127.0.0.1:{}", self.settings.grpc_port))
            .arg("--dispatch-cluster-enabled=false")
            .arg("--http-enabled=false")
            .arg("--metrics-enabled=false")
            .arg("--telemetry-endpoint=")
            .arg("--datastore-conn-pool-read-min-open=0")
            .arg("--datastore-conn-pool-read-max-open=4")
            .arg("--datastore-conn-pool-write-min-open=0")
            .arg("--datastore-conn-pool-write-max-open=4");
        self.spice = Some(Process::spawn(&mut command, None, false)?);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            if let Ok(client) = client(&self.settings).await {
                // A real authenticated schema RPC establishes ready service,
                // including empty schema before the initial test-only install.
                let ready = if self.spice_run == 1 {
                    client.write_schema(SPICE_SCHEMA).await.map(|_| ())
                } else {
                    client.read_schema().await.map(|_| ())
                };
                if ready.is_ok() {
                    return Ok(());
                }
            }
            ensure!(
                self.spice
                    .as_mut()
                    .context("SpiceDB child")?
                    .status()?
                    .is_none(),
                "fixture SpiceDB exited during startup"
            );
            ensure!(
                tokio::time::Instant::now() < deadline,
                "fixture SpiceDB readiness deadline"
            );
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    }

    pub fn stop_spice(&mut self) -> Result<()> {
        self.spice.as_mut().context("SpiceDB was running")?.stop()?;
        self.spice = None;
        Ok(())
    }

    pub fn shutdown(&mut self) -> Result<()> {
        let result = self.stop_services();
        if result.is_err() || self.cleanup_failed {
            self.cleanup_failed = true;
            if let Some(root) = self.root.as_mut() {
                root.disable_cleanup(true);
            }
            anyhow::bail!(
                "process cleanup unverified; private fixture retained at {}",
                self.settings.root.display()
            );
        }
        let credential_check = if self.root.is_some() {
            self.assert_no_plaintext_credentials(self.settings.root.as_path())
        } else {
            Ok(())
        };
        if let Some(root) = self.root.take() {
            root.close().context("remove private fixture data")?;
        }
        credential_check
    }

    fn stop_services(&mut self) -> Result<()> {
        #[cfg(test)]
        ensure!(!self.inject_shutdown_failure, "injected supervisor failure");
        // Retain each handle until its own stop verifies completion. In
        // particular, PostgreSQL backends deliberately escape the parent PGID.
        let spice_result = if let Some(process) = self.spice.as_mut() {
            process.stop()
        } else {
            Ok(())
        };
        if spice_result.is_ok() {
            self.spice = None;
        }
        #[cfg(test)]
        let budget = self.postgres_shutdown_budget;
        #[cfg(not(test))]
        let budget = Duration::from_secs(5);
        let postgres_result = if let Some(process) = self.postgres.as_mut() {
            process.stop_postgres(budget)
        } else {
            Ok(())
        };
        if postgres_result.is_ok() {
            self.postgres = None;
        }
        spice_result?;
        postgres_result
    }

    fn assert_no_plaintext_credentials(&self, directory: &Path) -> Result<()> {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            ensure!(!kind.is_symlink(), "unexpected symlink in private fixture");
            if kind.is_dir() {
                self.assert_no_plaintext_credentials(&entry.path())?;
            } else if kind.is_file() {
                let bytes = fs::read(entry.path())?;
                for secret in [
                    &self.settings.admin_password,
                    &self.settings.app_password,
                    &self.settings.spice_password,
                    &self.settings.preshared_key,
                ] {
                    ensure!(
                        !bytes
                            .windows(secret.len())
                            .any(|window| window == secret.as_bytes()),
                        "plaintext credential found on disk in fixture file {}",
                        entry.file_name().to_string_lossy()
                    );
                }
            }
        }
        Ok(())
    }

    pub fn assert_public_evidence(&self, bytes: &[u8]) -> Result<()> {
        for secret in [
            &self.settings.admin_password,
            &self.settings.app_password,
            &self.settings.spice_password,
            &self.settings.preshared_key,
        ] {
            ensure!(
                !bytes
                    .windows(secret.len())
                    .any(|window| window == secret.as_bytes()),
                "credential material is forbidden in evidence"
            );
        }
        Ok(())
    }

    pub fn save<T: Serialize>(&self, name: &str, value: &T) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(value)?;
        self.assert_public_evidence(&bytes)?;
        write_private(&self.evidence.join(name), &bytes)
    }

    pub async fn versions(&self) -> Result<Vec<String>> {
        self.binaries.attest().await
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Err(error) = self.shutdown() {
            eprintln!("fixture cleanup failed: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use anyhow::{ensure, Context, Result};

    use super::Fixture;

    #[test]
    fn shutdown_error_then_drop_retains_private_data() -> Result<()> {
        let mut fixture = Fixture::new()?;
        let root = fixture.settings.root.clone();
        fixture.inject_shutdown_failure = true;
        ensure!(fixture.shutdown().is_err(), "injected shutdown must fail");
        drop(fixture);
        ensure!(
            root.join("fixture-id").is_file(),
            "Drop removed retained fixture"
        );
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    struct StoppedBackend(i32);

    impl Drop for StoppedBackend {
        fn drop(&mut self) {
            // SAFETY: this backend remains stopped and hence retains its PID
            // until resumed; guard construction immediately follows SIGSTOP.
            unsafe { libc::kill(self.0, libc::SIGCONT) };
        }
    }

    #[tokio::test]
    async fn stopped_postgres_backend_prevents_data_cleanup() -> Result<()> {
        let mut fixture = Fixture::new()?;
        fixture.start().await?;
        let root = fixture.settings.root.clone();
        let mut backend = fixture
            .settings
            .connect("fence_app", "fence_app", "shutdown-regression", 3000)
            .await?;
        let backend_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut backend)
            .await?;
        let postmaster_pid = i32::try_from(fixture.postgres.as_ref().context("postmaster")?.id())?;
        // SAFETY: getpgid only observes the connected fixture backend and owned
        // postmaster; neither PID has been reaped by this fixture.
        let backend_group = unsafe { libc::getpgid(backend_pid) };
        let postmaster_group = unsafe { libc::getpgid(postmaster_pid) };
        ensure!(
            backend_group > 0 && backend_group != postmaster_group,
            "native PostgreSQL backend must escape postmaster group for this regression"
        );
        // SAFETY: this PID came from our authenticated fixture connection.
        ensure!(
            unsafe { libc::kill(backend_pid, libc::SIGSTOP) } == 0,
            "stop fixture backend"
        );
        let stopped = StoppedBackend(backend_pid);
        fixture.postgres_shutdown_budget = std::time::Duration::from_millis(200);
        ensure!(
            fixture.shutdown().is_err(),
            "stopped backend must prevent verified shutdown"
        );
        ensure!(
            root.join("pgdata/PG_VERSION").is_file(),
            "failed shutdown deleted PGDATA"
        );
        ensure!(
            fixture.postgres.is_some(),
            "failed shutdown lost process ownership"
        );
        drop(stopped);
        fixture.postgres_shutdown_budget = std::time::Duration::from_secs(5);
        fixture.stop_services()?;
        ensure!(
            fixture.postgres.is_none() && fixture.spice.is_none(),
            "service ownership not discharged"
        );
        drop(backend);
        drop(fixture);
        ensure!(
            root.join("pgdata/PG_VERSION").is_file(),
            "Drop removed sticky retained data"
        );
        // Both services have now completed verified shutdown; only this test's
        // marked, private root is removed after checking the retention contract.
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
