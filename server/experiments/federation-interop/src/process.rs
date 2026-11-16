//! Process supervision: every long-running service is a child in its own
//! process group with its output in a log file, stopped by a signal to the
//! group (or to the process alone, for PostgreSQL, whose backends leave
//! the group) and killed with the group if it outlives its grace period
//! or the harness.

use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio::time::Instant;

/// How a service is asked to stop.
#[derive(Debug, Clone, Copy)]
pub enum StopSignal {
    /// SIGTERM to the whole process group.
    TermGroup,
    /// SIGINT to the process alone (PostgreSQL's fast shutdown).
    InterruptProcess,
}

pub struct Supervised {
    pub label: String,
    pub log: PathBuf,
    child: Child,
    pid: i32,
    stop: StopSignal,
    /// Set once the leader has been reaped; its pid and group id may then
    /// name another process and are never signalled again.
    reaped: Option<ExitStatus>,
}

fn signal(target: i32, signal: libc::c_int) {
    // SAFETY: `kill` takes no pointers. Callers name a process this harness
    // spawned and has not reaped, or that process's group while its leader
    // is unreaped (running or a zombie), so the id cannot have been reused.
    unsafe {
        libc::kill(target, signal);
    }
}

/// Whether the process `pid` has exited, observed without reaping it: an
/// exited child stays a zombie, so its pid and process group id are not
/// reused until [`Child::wait`] reaps it.
fn exited_unreaped(pid: i32) -> bool {
    let Ok(pid) = libc::id_t::try_from(pid) else {
        return true;
    };
    // SAFETY: `info` is a zeroed `siginfo_t` that `waitid` writes into, and
    // `WNOWAIT` leaves the child unreaped for `Child::wait`.
    unsafe {
        let mut info: libc::siginfo_t = std::mem::zeroed();
        let status = libc::waitid(
            libc::P_PID,
            pid,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        );
        // With WNOHANG, a child that has not exited leaves `si_pid` zero;
        // an error means there is no such unreaped child.
        status != 0 || info.si_pid() != 0
    }
}

impl Supervised {
    pub fn spawn(label: &str, mut command: Command, logs: &Path, stop: StopSignal) -> Result<Self> {
        std::fs::create_dir_all(logs)?;
        let log = logs.join([label, "log"].join("."));
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
            .with_context(|| format!("open {}", log.display()))?;
        command
            .stdin(Stdio::null())
            .stdout(file.try_clone()?)
            .stderr(file)
            .process_group(0)
            .kill_on_drop(true);
        let child = command.spawn().with_context(|| format!("spawn {label}"))?;
        let pid = child
            .id()
            .and_then(|pid| i32::try_from(pid).ok())
            .context("a spawned child has a pid")?;
        Ok(Self {
            label: label.to_owned(),
            log,
            child,
            pid,
            stop,
            reaped: None,
        })
    }

    /// Whether the service is still running. An exited service is not
    /// reaped here, so its process group can still be signalled safely.
    pub fn running(&mut self) -> bool {
        self.reaped.is_none() && !exited_unreaped(self.pid)
    }

    /// Stop the service: the configured signal, then SIGKILL to the group
    /// after `grace`. The leader is reaped only after the group has been
    /// killed, so the group id still names this service's group.
    pub async fn stop(&mut self, grace: Duration) -> Result<Option<ExitStatus>> {
        if let Some(status) = self.reaped {
            return Ok(Some(status));
        }
        if self.running() {
            match self.stop {
                StopSignal::TermGroup => signal(-self.pid, libc::SIGTERM),
                StopSignal::InterruptProcess => signal(self.pid, libc::SIGINT),
            }
            let deadline = Instant::now() + grace;
            while self.running() && Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        // Descendants that stayed in the group do not outlive it.
        signal(-self.pid, libc::SIGKILL);
        let status = self.child.wait().await?;
        self.reaped = Some(status);
        Ok(Some(status))
    }

    /// Kill the whole group at once, as a crash would.
    pub async fn kill(&mut self) -> Result<()> {
        if self.reaped.is_none() {
            signal(-self.pid, libc::SIGKILL);
            self.reaped = Some(self.child.wait().await?);
        }
        Ok(())
    }

    /// The last `lines` lines of the log.
    pub fn log_tail(&self, lines: usize) -> String {
        let text = std::fs::read_to_string(&self.log).unwrap_or_default();
        let all: Vec<&str> = text.lines().collect();
        all.get(all.len().saturating_sub(lines)..)
            .unwrap_or_default()
            .join("\n")
    }
}

impl Drop for Supervised {
    fn drop(&mut self) {
        // Unreaped until here, so the group is still this service's; the
        // runtime reaps the dropped child afterwards.
        if self.reaped.is_none() {
            signal(-self.pid, libc::SIGKILL);
        }
    }
}

/// Write `contents` to a new file only its owner can read, for keys,
/// secrets and passwords.
pub fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("create {}", path.display()))?;
    // `mode` applies only to a file this call creates.
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(contents)
        .with_context(|| format!("write {}", path.display()))
}

/// Run a one-shot command to completion within `timeout`.
pub async fn run(label: &str, mut command: Command, timeout: Duration) -> Result<String> {
    command.stdin(Stdio::null()).kill_on_drop(true);
    let output = tokio::time::timeout(timeout, command.output())
        .await
        .with_context(|| format!("{label} timed out"))?
        .with_context(|| format!("run {label}"))?;
    let text = [
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    ]
    .concat();
    if !output.status.success() {
        bail!("{label} failed ({}): {text}", output.status);
    }
    Ok(text)
}

/// Wait until something accepts TCP connections on `port`.
pub async fn wait_tcp(label: &str, port: u16, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("{label} did not listen on {port} within {timeout:?}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Whether nothing accepts TCP connections on `port`.
pub async fn port_closed(port: u16) -> bool {
    TcpStream::connect(("127.0.0.1", port)).await.is_err()
}

/// Wait until `check` succeeds, polling every 200 ms.
pub async fn eventually<F, Fut>(timeout: Duration, mut check: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + timeout;
    loop {
        if check().await {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
