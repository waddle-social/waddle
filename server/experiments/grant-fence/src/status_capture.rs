//! Bounded streaming status fingerprint. Paths are never retained or emitted.
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

use crate::process::Process;

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct StatusCapture {
    pub bytes: u64,
    pub sha256: String,
}

pub async fn capture(root: &Path) -> Result<StatusCapture> {
    capture_bounded(root, 16 * 1024 * 1024).await
}
async fn capture_bounded(root: &Path, limit: u64) -> Result<StatusCapture> {
    let mut command = Command::new("git");
    command
        .env_clear()
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .current_dir(root);
    let mut process = Process::spawn(&mut command, None, true)?;
    let mut output = process.async_output()?;
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut hash = Sha256::new();
        let mut bytes = 0_u64;
        let mut buffer = [0; 8192];
        loop {
            let count = output.read(&mut buffer).await?;
            if count == 0 {
                break;
            }
            bytes = bytes
                .checked_add(u64::try_from(count)?)
                .context("status byte count overflow")?;
            ensure!(bytes <= limit, "git status exceeds bounded stream capacity");
            hash.update(&buffer[..count]);
        }
        process.finish(Duration::from_secs(5)).await?;
        Ok(StatusCapture {
            bytes,
            sha256: hex::encode(hash.finalize()),
        })
    })
    .await
    .context("git status fingerprint deadline")?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn large_dirty_status_is_streamed_without_retaining_paths() -> Result<()> {
        let root = tempfile::tempdir()?;
        let mut init = Command::new("git");
        init.env_clear()
            .args(["init", "--quiet", "--template="])
            .current_dir(root.path());
        Process::spawn(&mut init, None, false)?
            .finish(Duration::from_secs(5))
            .await?;
        let clean = capture(root.path()).await?;
        ensure!(clean.bytes == 0, "fresh repository must be clean");
        for index in 0..1024 {
            let name = format!("sensitive_path_marker_{index:04}_{}", "x".repeat(64));
            tokio::fs::write(root.path().join(name), b"fixture").await?;
        }
        let first = capture(root.path()).await?;
        ensure!(
            first.bytes > 65536 && first == capture(root.path()).await?,
            "large dirty fingerprint must be complete and stable beyond pipe capacity"
        );
        ensure!(
            !serde_json::to_string(&first)?.contains("sensitive_path_marker"),
            "status paths must not be retained in evidence"
        );
        ensure!(
            capture_bounded(root.path(), 16).await.is_err(),
            "oversized status must fail closed"
        );
        tokio::fs::write(root.path().join("changed"), b"fixture").await?;
        ensure!(
            capture(root.path()).await? != first,
            "dirty changes must alter full status fingerprint"
        );
        Ok(())
    }
}
