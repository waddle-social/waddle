use std::path::{Path, PathBuf};

use anyhow::{ensure, Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Provenance {
    pub git_head: String,
    pub git_status_bytes: u64,
    pub git_status_sha256: String,
    pub clean: bool,
    pub experiment_source_sha256: String,
}

impl Provenance {
    pub async fn capture() -> Result<Self> {
        let git_head =
            waddle_native_fixture::git_probe::head(Path::new(env!("CARGO_MANIFEST_DIR"))).await?;
        ensure!(
            git_head.len() == 40 && git_head.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "exact repository revision unavailable"
        );
        let git_status =
            waddle_native_fixture::status_capture::capture(Path::new(env!("CARGO_MANIFEST_DIR")))
                .await?;
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let repository = root.join("../../..").canonicalize()?;
        let shared = root.join("../native-fixture").canonicalize()?;
        let mut files = vec![
            shared.join("Cargo.toml"),
            shared.join("README.md"),
            repository.join("flake.lock"),
            repository.join("flake.nix"),
            repository.join("server/rust-toolchain.toml"),
            root.join("Cargo.toml"),
            root.join("Cargo.lock"),
            root.join("README.md"),
            root.join("native.nix"),
            root.join("run-native.sh"),
        ];
        for directory in ["src", "schema", "tests"] {
            collect(&root.join(directory), &mut files).await?;
        }
        collect(&shared.join("src"), &mut files).await?;
        files.sort();
        let mut hash = Sha256::new();
        for path in files {
            let relative = path
                .strip_prefix(&repository)?
                .to_str()
                .context("UTF-8 experiment source path")?;
            let kind = tokio::fs::symlink_metadata(&path).await?.file_type();
            ensure!(
                kind.is_file() && !kind.is_symlink(),
                "required source must be a regular file"
            );
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
            clean: git_status.bytes == 0,
            git_status_bytes: git_status.bytes,
            git_status_sha256: git_status.sha256,
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
