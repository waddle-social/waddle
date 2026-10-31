//! Fixed-command source provenance probe; never a general child-spawn API.
use crate::process::Process;
use anyhow::Result;
use std::path::Path;
use std::process::Command;
use std::time::Duration;
pub async fn head(root: &Path) -> Result<String> {
    let mut command = Command::new("git");
    command
        .env_clear()
        .args(["rev-parse", "HEAD"])
        .current_dir(root);
    let mut process = Process::spawn_probe(&mut command, None, true)?;
    process.finish(Duration::from_secs(5)).await?;
    Ok(process.output()?.trim_end().to_owned())
}
