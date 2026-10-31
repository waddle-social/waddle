use std::fs;
use std::io::{Read, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, ChildStdout, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use anyhow::{ensure, Context, Result};

use crate::cleanup::{CleanupFailure, CleanupLatch, CleanupService, ProcessFault, StopPhase};

/// Engineering margin for native filesystem flushes, not a proven upper bound.
pub const POSTGRES_SHUTDOWN_BUDGET: Duration = Duration::from_secs(30);

/// Linux-only disposable process supervisor. WNOWAIT retains the leader PID
/// until its original process group has been terminated; it never signals a
/// group after reaping its leader. Native diagnostics are disabled at source.
pub struct Process {
    child: Child,
    reaped: bool,
    observed: Option<ExitStatus>,
    output: Option<ChildStdout>,
    postgres: bool,
    measurement_armed: bool,
    measurement_start: Option<Instant>,
    measurement_elapsed_ms: Option<u64>,
    cleanup: Option<(CleanupLatch, CleanupService)>,
    #[cfg(test)]
    inject_stop_failure: bool,
}

impl Process {
    pub fn spawn(
        command: &mut Command,
        input: Option<&[u8]>,
        capture_version: bool,
    ) -> Result<Self> {
        Self::spawn_inner(command, input, capture_version, None, |_| Ok(()))
    }

    pub fn spawn_tracked(
        command: &mut Command,
        input: Option<&[u8]>,
        capture_version: bool,
        cleanup: &CleanupLatch,
        service: CleanupService,
    ) -> Result<Self> {
        Self::spawn_inner(
            command,
            input,
            capture_version,
            Some((cleanup.clone(), service)),
            |_| Ok(()),
        )
    }

    fn spawn_inner(
        command: &mut Command,
        input: Option<&[u8]>,
        capture_version: bool,
        cleanup: Option<(CleanupLatch, CleanupService)>,
        before_input: impl FnOnce(&mut Self) -> Result<()>,
    ) -> Result<Self> {
        // Empty anonymous pipe + at most PIPE_BUF bytes: the initial write is
        // bounded and cannot wait for an unresponsive child's reads.
        ensure!(
            input.is_none_or(|bytes| bytes.len() <= 4096),
            "fixture stdin exceeds PIPE_BUF"
        );
        command
            .process_group(0)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(if capture_version {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stderr(Stdio::null());
        let mut child = command.spawn().context("spawn fixture process")?;
        let output = child.stdout.take();
        let mut process = Self {
            child,
            reaped: false,
            observed: None,
            output,
            postgres: false,
            measurement_armed: false,
            measurement_start: None,
            measurement_elapsed_ms: None,
            cleanup,
            #[cfg(test)]
            inject_stop_failure: false,
        };
        // The fixture latch was attached in infallible construction above,
        // before this test seam, stdin writes or any other fallible operation.
        before_input(&mut process)?;
        if let Some(bytes) = input {
            let mut pipe = process
                .child
                .stdin
                .take()
                .context("anonymous child stdin")?;
            pipe.write_all(bytes)?;
            drop(pipe);
        }
        Ok(process)
    }

    pub fn postgres(command: &mut Command, cleanup: &CleanupLatch) -> Result<Self> {
        let mut process =
            Self::spawn_tracked(command, None, false, cleanup, CleanupService::Postgres)?;
        process.postgres = true;
        Ok(process)
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }

    pub fn stop_postgres(&mut self, budget: Duration) -> Result<()> {
        let result = self.stop_postgres_inner(budget);
        self.record_cleanup(result)
    }

    fn stop_postgres_inner(&mut self, budget: Duration) -> Result<()> {
        #[cfg(test)]
        if self.inject_stop_failure {
            return Err(ProcessFault::InjectedStop.into());
        }
        ensure!(
            self.postgres,
            "PostgreSQL shutdown requires PostgreSQL supervisor"
        );
        if self.reaped {
            return Ok(());
        }
        let pid = i32::try_from(self.child.id()).context("postmaster PID fits pid_t")?;
        if self.status()?.is_none() {
            // SAFETY: the owned unreaped postmaster reserves this PID. Fast
            // shutdown coordinates and reaps PostgreSQL's setsid backends.
            let immediately_before_signal = Instant::now();
            if unsafe { libc::kill(pid, libc::SIGINT) } != 0 {
                return Err(ProcessFault::Signal {
                    errno: std::io::Error::last_os_error().raw_os_error(),
                }
                .into());
            }
            if self.measurement_armed && self.measurement_start.is_none() {
                self.measurement_start = Some(immediately_before_signal);
            }
        }
        let deadline = Instant::now() + budget;
        loop {
            if let Some(status) = self.status()? {
                // Abnormal exit does not establish that escaped backends died.
                // Retain the handle and private data; never SIGKILL fallback.
                if !status.success() {
                    return Err(ProcessFault::PostgresExit {
                        code: status.code(),
                        signal: status.signal(),
                    }
                    .into());
                }
                if let Some(start) = self.measurement_start {
                    self.measurement_elapsed_ms
                        .get_or_insert(u64::try_from(start.elapsed().as_millis())?);
                }
                let status = self.child.wait()?;
                self.observed = Some(status);
                self.reaped = true;
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(ProcessFault::Deadline {
                    phase: StopPhase::Postgres,
                    postgres: Some(crate::postgres_diagnostics::capture(self.child.id())),
                }
                .into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Diagnostic-only controller: invoke the existing five-second stop once,
    /// then observe without signaling up to thirty seconds TOTAL from its first
    /// successful SIGINT. A late exit never clears the fixture failure latch.
    pub async fn measure_postgres_shutdown(
        &mut self,
    ) -> Result<crate::shutdown_measurement::ShutdownTiming> {
        use crate::shutdown_measurement::{ObservedExit, ShutdownTiming};
        ensure!(
            self.postgres && !self.reaped && !self.measurement_armed,
            "measurement requires a fresh owned postmaster"
        );
        self.measurement_armed = true;
        let first = self.stop_postgres(Duration::from_secs(5));
        let exceeded = match first {
            Ok(()) => false,
            Err(error)
                if matches!(
                    error.downcast_ref::<ProcessFault>(),
                    Some(ProcessFault::Deadline {
                        phase: StopPhase::Postgres,
                        ..
                    })
                ) =>
            {
                true
            }
            Err(error) => return Err(error),
        };
        let start = self
            .measurement_start
            .context("measurement signal was not acknowledged by kernel")?;
        if !exceeded {
            return Ok(ShutdownTiming {
                elapsed_ms: self
                    .measurement_elapsed_ms
                    .context("normal exit observation timing")?,
                exceeded_five_seconds: false,
                observed: ObservedExit::Normal,
                final_snapshot: None,
            });
        }
        loop {
            if start.elapsed() >= Duration::from_secs(30) {
                return Ok(ShutdownTiming {
                    elapsed_ms: u64::try_from(start.elapsed().as_millis())?,
                    exceeded_five_seconds: true,
                    observed: ObservedExit::StillRunning,
                    final_snapshot: Some(crate::postgres_diagnostics::capture(self.child.id())),
                });
            }
            if let Some(status) = self.status()? {
                let elapsed_ms = u64::try_from(start.elapsed().as_millis())?;
                let final_snapshot = Some(crate::postgres_diagnostics::capture(self.child.id()));
                let observed = if status.success() {
                    self.reap_measured_exit()?;
                    self.measurement_elapsed_ms = Some(elapsed_ms);
                    ObservedExit::Normal
                } else {
                    ObservedExit::Abnormal {
                        code: status.code(),
                        signal: status.signal(),
                    }
                };
                return Ok(ShutdownTiming {
                    elapsed_ms,
                    exceeded_five_seconds: true,
                    observed,
                    final_snapshot,
                });
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// The measurement already observed normal exit with WNOWAIT. Reaping
    /// must still be nonblocking; unexpected uncertainty keeps ownership and
    /// permanently prevents fixture cleanup from claiming success.
    fn reap_measured_exit(&mut self) -> Result<()> {
        let result = match self.child.try_wait() {
            Ok(Some(status)) => {
                self.observed = Some(status);
                self.reaped = true;
                if status.success() {
                    Ok(())
                } else {
                    Err(ProcessFault::PostgresExit {
                        code: status.code(),
                        signal: status.signal(),
                    }
                    .into())
                }
            }
            Ok(None) => Err(ProcessFault::ReapNotReady.into()),
            Err(error) => Err(error.into()),
        };
        self.record_cleanup(result)
    }

    pub fn status(&mut self) -> Result<Option<ExitStatus>> {
        if self.observed.is_some() {
            return Ok(self.observed);
        }
        ensure!(!self.reaped, "cannot observe an already reaped child");
        let mut information = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        // SAFETY: waitid writes one initialized siginfo_t. WNOWAIT explicitly
        // observes without reaping, retaining the leader's PID reservation.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                self.child.id(),
                information.as_mut_ptr(),
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result != 0 {
            return Err(ProcessFault::ObserveExit {
                errno: std::io::Error::last_os_error().raw_os_error(),
            }
            .into());
        }
        // SAFETY: successful waitid initialized the zeroed structure.
        let information = unsafe { information.assume_init() };
        // SAFETY: these siginfo accessors are valid for waitid's SIGCHLD data.
        if unsafe { information.si_pid() } == 0 {
            return Ok(None);
        }
        let status = unsafe { information.si_status() };
        let raw = match information.si_code {
            libc::CLD_EXITED => status << 8,
            libc::CLD_KILLED => status,
            libc::CLD_DUMPED => status | 0x80,
            _ => anyhow::bail!("unexpected child exit observation"),
        };
        self.observed = Some(ExitStatus::from_raw(raw));
        Ok(self.observed)
    }

    pub async fn finish(&mut self, budget: Duration) -> Result<()> {
        let deadline = tokio::time::Instant::now() + budget;
        loop {
            if let Some(status) = self.status()? {
                // Kill remaining descendants even when the leader exited
                // successfully. Its PID is still unreaped and cannot be reused.
                self.stop()?;
                ensure!(status.success(), "fixture child exited unsuccessfully");
                return Ok(());
            }
            ensure!(
                tokio::time::Instant::now() < deadline,
                "fixture child deadline"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    pub fn async_output(&mut self) -> Result<tokio::process::ChildStdout> {
        Ok(tokio::process::ChildStdout::from_std(
            self.output.take().context("captured child stdout")?,
        )?)
    }

    pub fn output(&mut self) -> Result<String> {
        ensure!(self.reaped, "capture requires completed process cleanup");
        let mut bytes = Vec::new();
        if let Some(pipe) = self.output.take() {
            pipe.take(4097).read_to_end(&mut bytes)?;
            ensure!(
                bytes.len() <= 4096,
                "fixture captured output exceeds 4096 bytes"
            );
        }
        Ok(String::from_utf8(bytes)?)
    }

    pub fn stop(&mut self) -> Result<()> {
        let result = self.stop_inner();
        self.record_cleanup(result)
    }

    fn record_cleanup(&self, result: Result<()>) -> Result<()> {
        if let (Err(error), Some((cleanup, service))) = (&result, &self.cleanup) {
            cleanup.record(CleanupFailure::capture(*service, error));
        }
        result
    }

    fn stop_inner(&mut self) -> Result<()> {
        #[cfg(test)]
        if self.inject_stop_failure {
            return Err(ProcessFault::InjectedStop.into());
        }
        if self.postgres {
            return self.stop_postgres_inner(POSTGRES_SHUTDOWN_BUDGET);
        }
        if self.reaped {
            return Ok(());
        }
        let pid = i32::try_from(self.child.id()).context("child PID fits pid_t")?;
        // SAFETY: spawn established this child's process group. WNOWAIT has
        // retained the unreaped leader, so this PGID cannot identify a reused
        // process group. No signal is ever sent after reaping.
        let result = unsafe { libc::kill(-pid, libc::SIGKILL) };
        if result != 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
            return Err(ProcessFault::Signal {
                errno: std::io::Error::last_os_error().raw_os_error(),
            }
            .into());
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while live_group_members(pid)? {
            if Instant::now() >= deadline {
                return Err(ProcessFault::Deadline {
                    phase: StopPhase::ProcessGroup,
                    postgres: None,
                }
                .into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        loop {
            if let Some(status) = self.child.try_wait()? {
                self.observed.get_or_insert(status);
                self.reaped = true;
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(ProcessFault::Deadline {
                    phase: StopPhase::LeaderReap,
                    postgres: None,
                }
                .into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn live_group_members(group: i32) -> Result<bool> {
    for entry in fs::read_dir("/proc").context("Linux /proc required for fixture group cleanup")? {
        let entry = entry?;
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Some(text) = process_stat_result(fs::read_to_string(entry.path().join("stat")), pid)?
        else {
            continue;
        };
        let fields: Vec<_> = text
            .rsplit_once(") ")
            .context("process stat layout")?
            .1
            .split_whitespace()
            .collect();
        if fields.get(2).and_then(|value| value.parse::<i32>().ok()) == Some(group)
            && !matches!(fields.first(), Some(&"Z" | &"X"))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn process_stat_result(result: std::io::Result<String>, pid: u32) -> Result<Option<String>> {
    match result {
        Ok(text) => Ok(Some(text)),
        // proc_single_show can return ESRCH after open succeeded if the task
        // exits before seq_read. That task is gone, just as with ENOENT.
        Err(error)
            if error.raw_os_error() == Some(libc::ESRCH)
                || error.kind() == std::io::ErrorKind::NotFound =>
        {
            Ok(None)
        }
        Err(error) => Err(ProcessFault::ProcessStat {
            pid,
            errno: error.raw_os_error(),
        }
        .into()),
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            eprintln!("fixture cleanup failed: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_binary(name: &str) -> Result<std::path::PathBuf> {
        let path = std::path::PathBuf::from(
            std::env::var_os(name).with_context(|| format!("required {name} is unset"))?,
        );
        ensure!(
            path.is_absolute() && path.is_file(),
            "{name} requires an absolute existing binary"
        );
        Ok(path)
    }

    struct ReapProbe(std::sync::Arc<std::sync::atomic::AtomicU32>);

    impl ReapProbe {
        fn finish(&mut self) -> Result<()> {
            use std::sync::atomic::Ordering;
            let pid = self.0.load(Ordering::SeqCst);
            if pid == 0 {
                return Ok(());
            }
            let mut information = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
            // SAFETY: a known owned child is observed without reaping before
            // any signal. ECHILD refuses to signal a potentially reused PID.
            let observed = unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid,
                    information.as_mut_ptr(),
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if observed != 0 {
                self.0.store(0, Ordering::SeqCst);
                anyhow::bail!("probe child ownership could not be verified");
            }
            // SAFETY: the unreaped owned child reserves this PGID. The probe
            // command is one sleep process and has no descendants.
            ensure!(
                unsafe { libc::kill(-i32::try_from(pid)?, libc::SIGKILL) } == 0,
                "kill retained probe group"
            );
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                let mut status = 0;
                // SAFETY: waitpid reaps only this exact owned probe child.
                let result =
                    unsafe { libc::waitpid(i32::try_from(pid)?, &mut status, libc::WNOHANG) };
                if result == i32::try_from(pid)? {
                    self.0.store(0, Ordering::SeqCst);
                    return Ok(());
                }
                ensure!(
                    result == 0 && Instant::now() < deadline,
                    "retained probe cleanup deadline"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    impl Drop for ReapProbe {
        fn drop(&mut self) {
            if self.finish().is_err() {
                eprintln!("retained probe cleanup unverified");
            }
        }
    }

    #[tokio::test]
    async fn measurement_reap_uncertainty_retains_fixture_and_child_ownership() -> Result<()> {
        let mut fixture = crate::fixture::Fixture::new()?;
        let root = fixture.settings.root.clone();
        let mut command = Command::new(test_binary("FENCE_TEST_SLEEP")?);
        command.env_clear().current_dir(&root).arg("30");
        let mut child = Process::spawn_tracked(
            &mut command,
            None,
            false,
            fixture.cleanup_latch(),
            CleanupService::Postgres,
        )?;
        // Model an unexpectedly stale normal WNOWAIT observation while the
        // real owned child is still live. try_wait must return immediately.
        child.observed = Some(ExitStatus::from_raw(0));
        let before = Instant::now();
        let error = child
            .reap_measured_exit()
            .expect_err("live child is not reapable");
        ensure!(
            before.elapsed() < Duration::from_secs(1)
                && error.downcast_ref::<ProcessFault>() == Some(&ProcessFault::ReapNotReady)
                && !child.reaped,
            "nonblocking reap must preserve owned live child on uncertainty"
        );
        let first = fixture
            .cleanup_failure()
            .context("measurement reap failure latch")?;
        ensure!(
            first.service == CleanupService::Postgres
                && first.cause == ProcessFault::ReapNotReady
                && fixture.shutdown().is_err(),
            "measurement reap uncertainty must forbid cleanup success"
        );
        child.stop()?;
        ensure!(
            child.reaped,
            "intentional probe child cleanup must complete"
        );
        drop(child);
        drop(fixture);
        ensure!(
            tokio::fs::try_exists(root.join("fixture-id")).await?,
            "successful retry and Drop must retain failed measurement data"
        );
        tokio::fs::remove_dir_all(root).await?;

        let mut command = Command::new(test_binary("FENCE_TEST_SLEEP")?);
        command.env_clear().arg("0");
        let mut completed = Process::spawn(&mut command, None, false)?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while completed.status()?.is_none() {
            ensure!(
                tokio::time::Instant::now() < deadline,
                "probe exit deadline"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        completed.reap_measured_exit()?;
        ensure!(completed.reaped, "observed normal child must be reaped");
        Ok(())
    }

    #[tokio::test]
    async fn worker_drop_failure_reaches_fixture_before_data_removal() -> Result<()> {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;
        let mut fixture = crate::fixture::Fixture::new()?;
        fixture.start().await?;
        let root = fixture.settings.root.clone();
        let pid = Arc::new(AtomicU32::new(0));
        let mut reap = ReapProbe(pid.clone());
        let mut command = Command::new(test_binary("FENCE_TEST_SLEEP")?);
        command.env_clear().current_dir(&root).arg("30");
        let mut worker = Process::spawn_tracked(
            &mut command,
            None,
            false,
            fixture.cleanup_latch(),
            CleanupService::Worker,
        )?;
        worker.inject_stop_failure = true;
        pid.store(worker.id(), Ordering::SeqCst);
        drop(worker);
        let first = fixture
            .cleanup_failure()
            .context("worker Drop failure did not reach fixture")?;
        ensure!(
            first.service == CleanupService::Worker && first.cause == ProcessFault::InjectedStop,
            "worker cleanup cause lost"
        );
        let cleanup_verified = fixture.shutdown().is_ok();
        ensure!(
            !cleanup_verified && fixture.services_stopped(),
            "later successful service stops must not mask an unverified worker"
        );
        ensure!(
            tokio::fs::try_exists(root.join("fixture-id")).await?,
            "worker cleanup failure removed private root"
        );
        reap.finish()?;
        ensure!(
            fixture.shutdown().is_err() && fixture.cleanup_failure().as_ref() == Some(&first),
            "successful later cleanup cleared first failure"
        );
        drop(fixture);
        ensure!(
            tokio::fs::try_exists(root.join("fixture-id")).await?,
            "Drop removed sticky retained worker data"
        );
        tokio::fs::remove_dir_all(root).await?;
        Ok(())
    }

    #[test]
    fn postspawn_failure_is_tracked_before_stdin_or_caller_ownership() -> Result<()> {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;
        let mut fixture = crate::fixture::Fixture::new()?;
        let root = fixture.settings.root.clone();
        let pid = Arc::new(AtomicU32::new(0));
        let mut reap = ReapProbe(pid.clone());
        let mut command = Command::new(test_binary("FENCE_TEST_SLEEP")?);
        command.env_clear().current_dir(&root).arg("30");
        let result = Process::spawn_inner(
            &mut command,
            Some(b"bounded input"),
            false,
            Some((fixture.cleanup_latch().clone(), CleanupService::Initdb)),
            |child| {
                child.inject_stop_failure = true;
                pid.store(child.id(), Ordering::SeqCst);
                Err(ProcessFault::InjectedPostSpawn.into())
            },
        );
        ensure!(
            result.is_err() && pid.load(Ordering::SeqCst) != 0,
            "probe must fail after child actually starts"
        );
        let first = fixture
            .cleanup_failure()
            .context("postspawn cleanup failure did not reach fixture")?;
        ensure!(
            first.service == CleanupService::Initdb && first.cause == ProcessFault::InjectedStop,
            "temporary process cleanup cause lost"
        );
        let cleanup_verified = fixture.shutdown().is_ok();
        ensure!(
            !cleanup_verified && root.join("fixture-id").is_file(),
            "postspawn uncertainty must retain private root"
        );
        reap.finish()?;
        drop(fixture);
        ensure!(
            root.join("fixture-id").is_file(),
            "Drop removed retained temporary-process data"
        );
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn opened_proc_stat_of_reaped_task_is_treated_as_vanished() -> Result<()> {
        let mut command = Command::new(test_binary("FENCE_TEST_SLEEP")?);
        command.env_clear().arg("30");
        let mut child = Process::spawn(&mut command, None, false)?;
        let pid = child.id();
        let mut stat = fs::File::open(format!("/proc/{pid}/stat"))?;
        child.stop()?;
        let error = stat
            .read_to_string(&mut String::new())
            .expect_err("Linux read of opened proc stat after task reap must fail");
        ensure!(
            error.raw_os_error() == Some(libc::ESRCH),
            "deterministic open/read race did not return ESRCH"
        );
        ensure!(
            process_stat_result(Err(error), pid)?.is_none(),
            "vanished task must not fail group cleanup"
        );
        for errno in [libc::EIO, libc::EACCES, libc::EPERM] {
            let error = process_stat_result(Err(std::io::Error::from_raw_os_error(errno)), pid)
                .expect_err("unknown or inaccessible proc tasks must fail closed");
            ensure!(
                error.downcast_ref::<ProcessFault>()
                    == Some(&ProcessFault::ProcessStat {
                        pid,
                        errno: Some(errno)
                    }),
                "unknown or permission-denied proc error lost its typed cause"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn exited_leader_keeps_descendants_owned_until_group_cleanup() -> Result<()> {
        let shell = test_binary("FENCE_TEST_SHELL")?;
        let sleep = test_binary("FENCE_TEST_SLEEP")?;
        let mut command = Command::new(shell);
        command
            .env_clear()
            .args(["-c", "\"$1\" 30 & echo $!; exit 0", "cleanup-probe"])
            .arg(sleep);
        let mut process = Process::spawn(&mut command, None, true)?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while process.status()?.is_none() {
            ensure!(
                tokio::time::Instant::now() < deadline,
                "cleanup probe leader deadline"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        process.finish(Duration::from_secs(1)).await?;
        let descendant: i32 = process.output()?.trim().parse()?;
        if let Ok(stat) = tokio::fs::read_to_string(format!("/proc/{descendant}/stat")).await {
            let state = stat
                .rsplit_once(") ")
                .context("descendant stat")?
                .1
                .split_whitespace()
                .next();
            ensure!(
                matches!(state, Some("Z" | "X")),
                "descendant remained running after group cleanup"
            );
        }
        Ok(())
    }
}
