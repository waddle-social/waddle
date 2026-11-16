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
}

fn signal(target: i32, signal: libc::c_int) {
    // SAFETY: `target` is a process or process group this harness spawned
    // and has not reaped, so it cannot name a recycled process.
    unsafe {
        libc::kill(target, signal);
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
        })
    }

    pub fn running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Stop the service: the configured signal, then SIGKILL to the group
    /// after `grace`.
    pub async fn stop(&mut self, grace: Duration) -> Result<Option<ExitStatus>> {
        if !self.running() {
            return Ok(self.child.try_wait()?);
        }
        match self.stop {
            StopSignal::TermGroup => signal(-self.pid, libc::SIGTERM),
            StopSignal::InterruptProcess => signal(self.pid, libc::SIGINT),
        }
        match tokio::time::timeout(grace, self.child.wait()).await {
            Ok(status) => {
                // Descendants that stayed in the group do not outlive it.
                signal(-self.pid, libc::SIGKILL);
                Ok(Some(status?))
            }
            Err(_) => {
                signal(-self.pid, libc::SIGKILL);
                Ok(Some(self.child.wait().await?))
            }
        }
    }

    /// Kill the whole group at once, as a crash would.
    pub async fn kill(&mut self) -> Result<()> {
        signal(-self.pid, libc::SIGKILL);
        let _ = self.child.wait().await?;
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
        if self.running() {
            signal(-self.pid, libc::SIGKILL);
        }
    }
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
