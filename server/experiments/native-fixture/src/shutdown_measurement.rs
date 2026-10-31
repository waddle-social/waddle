//! Optional diagnostic evidence, never a formal proof or shutdown budget change.
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use anyhow::{ensure, Context, Result};
use serde::Serialize;
use sqlx::{Connection, Row};

use crate::model::{Settings, RPC_BUDGET};
use crate::postgres_diagnostics::PostgresSnapshot;

#[derive(Clone, Debug, Serialize)]
pub enum ObservedExit {
    Normal,
    Abnormal {
        code: Option<i32>,
        signal: Option<i32>,
    },
    StillRunning,
}
#[derive(Clone, Debug, Serialize)]
pub struct ShutdownTiming {
    pub elapsed_ms: u64,
    pub exceeded_five_seconds: bool,
    pub observed: ObservedExit,
    pub final_snapshot: Option<PostgresSnapshot>,
}
#[derive(Clone, Debug, Serialize)]
pub struct DurabilitySettings {
    pub fsync: bool,
    pub synchronous_commit: bool,
    pub full_page_writes: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct Measurement {
    pub formal_proof: bool,
    pub app_session: DurabilitySettings,
    pub spice_role_session: DurabilitySettings,
    pub filesystem_magic: i64,
    pub timing: ShutdownTiming,
}

pub async fn durability(settings: &Settings, role: &str) -> Result<DurabilitySettings> {
    let connection = settings
        .connect(role, role, "shutdown-durability-observer", 3000)
        .await?;
    read_and_close(connection).await
}

async fn read_and_close(mut connection: sqlx::PgConnection) -> Result<DurabilitySettings> {
    tokio::time::timeout(RPC_BUDGET, async move {
        let row = sqlx::query("SELECT current_setting('fsync')::bool AS fsync, current_setting('synchronous_commit')='on' AS synchronous_commit, current_setting('full_page_writes')::bool AS full_page_writes").fetch_one(&mut connection).await?;
        let settings = DurabilitySettings { fsync: row.try_get("fsync")?, synchronous_commit: row.try_get("synchronous_commit")?, full_page_writes: row.try_get("full_page_writes")? };
        ensure!(settings.fsync && settings.synchronous_commit && settings.full_page_writes, "measurement requires unchanged enabled PostgreSQL durability settings");
        connection.close().await?;
        Ok(settings)
    }).await.context("durability query/close client deadline")?
}

pub fn filesystem_magic(root: &Path) -> Result<i64> {
    let path = CString::new(root.as_os_str().as_bytes())?;
    let mut information = std::mem::MaybeUninit::<libc::statfs>::zeroed();
    // SAFETY: the owned fixture path is NUL-terminated and statfs initializes
    // this structure on success. Only the numeric filesystem kind is retained.
    ensure!(
        unsafe { libc::statfs(path.as_ptr(), information.as_mut_ptr()) } == 0,
        "owned filesystem metadata unavailable"
    );
    let information = unsafe { information.assume_init() };
    Ok(information.f_type)
}

#[cfg(test)]
mod tests {
    use super::read_and_close;
    use crate::fixture::Fixture;
    use crate::model::RPC_BUDGET;
    use anyhow::{ensure, Context, Result};
    use std::time::Duration;

    struct ResumeBackend(i32);
    impl Drop for ResumeBackend {
        fn drop(&mut self) {
            // SAFETY: SIGSTOP keeps this exact owned backend PID reserved.
            unsafe { libc::kill(self.0, libc::SIGCONT) };
        }
    }

    #[tokio::test]
    async fn durability_deadline_bounds_a_connected_unresponsive_backend() -> Result<()> {
        let mut fixture = Fixture::new()?;
        fixture.start("", crate::TEST_SCHEMA).await?;
        let mut connection = fixture
            .settings
            .connect(
                "fence_app",
                "fence_app",
                "stopped-durability-observer",
                3000,
            )
            .await?;
        let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut connection)
            .await?;
        // SAFETY: the PID belongs to our authenticated fixture connection.
        ensure!(
            unsafe { libc::kill(pid, libc::SIGSTOP) } == 0,
            "stop connected fixture backend"
        );
        let resume = ResumeBackend(pid);
        let outcome = tokio::time::timeout(
            RPC_BUDGET + Duration::from_secs(1),
            read_and_close(connection),
        )
        .await
        .context("outer durability regression watchdog")?;
        let error = outcome.expect_err("unresponsive connected backend must hit client deadline");
        ensure!(
            error
                .downcast_ref::<tokio::time::error::Elapsed>()
                .is_some(),
            "expected actual client-side query/close timeout"
        );
        ensure!(
            fixture.cleanup_failure().is_none(),
            "query timeout must not fabricate or clear cleanup evidence"
        );
        drop(resume);
        fixture.shutdown()?;
        Ok(())
    }
}
