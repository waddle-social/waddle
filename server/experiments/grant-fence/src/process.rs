use std::fs;
use std::io::{Read, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, ChildStdout, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use anyhow::{ensure, Context, Result};

/// Linux-only disposable process supervisor. WNOWAIT retains the leader PID
/// until its original process group has been terminated; it never signals a
/// group after reaping its leader. Native diagnostics are disabled at source.
pub struct Process {
    child: Child,
    reaped: bool,
    observed: Option<ExitStatus>,
    output: Option<ChildStdout>,
    postgres: bool,
}

impl Process {
    pub fn spawn(
        command: &mut Command,
        input: Option<&[u8]>,
        capture_version: bool,
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
        };
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

    pub fn postgres(command: &mut Command) -> Result<Self> {
        let mut process = Self::spawn(command, None, false)?;
        process.postgres = true;
        Ok(process)
    }

    #[cfg(test)]
    pub fn id(&self) -> u32 {
        self.child.id()
    }

    pub fn stop_postgres(&mut self, budget: Duration) -> Result<()> {
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
            ensure!(
                unsafe { libc::kill(pid, libc::SIGINT) } == 0,
                "signal owned postmaster failed"
            );
        }
        let deadline = Instant::now() + budget;
        loop {
            if let Some(status) = self.status()? {
                // Abnormal exit does not establish that escaped backends died.
                // Retain the handle and private data; never SIGKILL fallback.
                ensure!(
                    status.success(),
                    "postmaster did not complete coordinated shutdown"
                );
                let status = self.child.wait()?;
                self.observed = Some(status);
                self.reaped = true;
                return Ok(());
            }
            ensure!(
                Instant::now() < deadline,
                "PostgreSQL coordinated shutdown deadline"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
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
        ensure!(result == 0, "waitid child observation failed");
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
        if self.postgres {
            return self.stop_postgres(Duration::from_secs(5));
        }
        if self.reaped {
            return Ok(());
        }
        let pid = i32::try_from(self.child.id()).context("child PID fits pid_t")?;
        // SAFETY: spawn established this child's process group. WNOWAIT has
        // retained the unreaped leader, so this PGID cannot identify a reused
        // process group. No signal is ever sent after reaping.
        let result = unsafe { libc::kill(-pid, libc::SIGKILL) };
        ensure!(
            result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH),
            "kill owned fixture group failed"
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while live_group_members(pid)? {
            ensure!(
                Instant::now() < deadline,
                "owned group still has live members after SIGKILL"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        loop {
            if let Some(status) = self.child.try_wait()? {
                self.observed.get_or_insert(status);
                self.reaped = true;
                return Ok(());
            }
            ensure!(
                Instant::now() < deadline,
                "fixture leader did not reap after SIGKILL"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn live_group_members(group: i32) -> Result<bool> {
    for entry in fs::read_dir("/proc").context("Linux /proc required for fixture group cleanup")? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().parse::<u32>().is_err() {
            continue;
        }
        let text = match fs::read_to_string(entry.path().join("stat")) {
            Ok(text) => text,
            Err(error)
                if [
                    std::io::ErrorKind::NotFound,
                    std::io::ErrorKind::PermissionDenied,
                ]
                .contains(&error.kind()) =>
            {
                continue
            }
            Err(error) => return Err(error.into()),
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
        if let Ok(stat) = fs::read_to_string(format!("/proc/{descendant}/stat")) {
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
