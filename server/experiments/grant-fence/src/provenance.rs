use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::process::Process;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Provenance {
    pub git_head: String,
    pub git_status: String,
    pub clean: bool,
    pub experiment_source_sha256: String,
}

async fn git(arguments: &[&str]) -> Result<String> {
    let mut command = Command::new("git");
    command
        .env_clear()
        .args(arguments)
        .current_dir(env!("CARGO_MANIFEST_DIR"));
    let mut process = Process::spawn(&mut command, None, true)?;
    process.finish(Duration::from_secs(5)).await?;
    Ok(process.output()?.trim_end().to_owned())
}

impl Provenance {
    pub async fn capture() -> Result<Self> {
        let git_head = git(&["rev-parse", "HEAD"]).await?;
        ensure!(
            git_head.len() == 40 && git_head.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "exact repository revision unavailable"
        );
        let git_status = git(&["status", "--porcelain=v1", "--untracked-files=all"]).await?;
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files = vec![
            root.join("Cargo.toml"),
            root.join("Cargo.lock"),
            root.join("README.md"),
            root.join("native.nix"),
            root.join("run-native.sh"),
        ];
        for directory in ["src", "schema", "tests"] {
            collect(&root.join(directory), &mut files).await?;
        }
        files.sort();
        let mut hash = Sha256::new();
        for path in files {
            let relative = path
                .strip_prefix(root)?
                .to_str()
                .context("UTF-8 experiment source path")?;
            let bytes = tokio::fs::read(&path)
                .await
                .with_context(|| format!("required experiment source missing: {relative}"))?;
            hash.update((relative.len() as u64).to_be_bytes());
            hash.update(relative.as_bytes());
            hash.update((bytes.len() as u64).to_be_bytes());
            hash.update(bytes);
        }
        Ok(Self {
            git_head,
            clean: git_status.is_empty(),
            git_status,
            experiment_source_sha256: hex::encode(hash.finalize()),
        })
    }
}

async fn collect(directory: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    let mut pending = vec![directory.to_owned()];
    while let Some(directory) = pending.pop() {
        let mut entries = tokio::fs::read_dir(directory).await?;
        while let Some(entry) = entries.next_entry().await? {
            let kind = entry.file_type().await?;
            ensure!(
                !kind.is_symlink(),
                "experiment source symlinks are not accepted"
            );
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                files.push(entry.path());
            }
        }
    }
    Ok(())
}
