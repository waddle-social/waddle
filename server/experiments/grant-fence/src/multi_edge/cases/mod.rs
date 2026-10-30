use crate::fixture::Fixture;
use crate::multi_edge::model::Case;
use anyhow::{ensure, Result};
use std::path::Path;
mod races;
mod retries;
mod stale;
mod support;
pub async fn run(fixture: &Fixture, binary: &Path) -> Result<Vec<Case>> {
    tokio::time::timeout(
        std::time::Duration::from_secs(180),
        run_bounded(fixture, binary),
    )
    .await
    .map_err(|_| anyhow::anyhow!("multi-edge stage deadline"))?
}
async fn run_bounded(fixture: &Fixture, binary: &Path) -> Result<Vec<Case>> {
    let mut cases = Vec::new();
    stale::stale_vectors(fixture, binary, &mut cases).await?;
    stale::aba(fixture, binary, &mut cases).await?;
    stale::alternatives(fixture, binary, &mut cases).await?;
    stale::graph_error(fixture, binary, &mut cases).await?;
    stale::snapshot(fixture, binary, &mut cases).await?;
    races::run(fixture, binary, &mut cases).await?;
    retries::run(fixture, binary, &mut cases).await?;
    ensure!(
        cases.len() == 31,
        "multi-edge inventory changed: {}",
        cases.len()
    );
    Ok(cases)
}
