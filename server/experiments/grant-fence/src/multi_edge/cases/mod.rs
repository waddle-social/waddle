use crate::fixture::Fixture;
use crate::multi_edge::model::Case;
use anyhow::{ensure, Result};
use std::path::Path;
mod races;
mod retries;
mod stale;
mod support;
pub async fn run(fixture: &Fixture, binary: &Path, cases: &mut Vec<Case>) -> Result<()> {
    tokio::time::timeout(std::time::Duration::from_secs(180), async {
        // Inject a controlled failure after one real completed scenario and
        // check the caller-owned evidence survives the failed stage.
        let mut partial = Vec::new();
        let error = run_bounded(fixture, binary, &mut partial, true)
            .await
            .expect_err("injected failure must stop the stage");
        ensure!(
            error.to_string() == "injected failure after completed multi-edge case"
                && partial.len() == 1
                && partial[0].name == "lifecycle_recreate_fenced"
                && partial[0].passed,
            "partial multi-edge evidence was not retained: {error:#}"
        );
        support::passed(cases, "completed_case_survives_stage_failure");
        run_bounded(fixture, binary, cases, false).await?;
        ensure!(
            cases.len() == 32,
            "multi-edge inventory changed: {}",
            cases.len()
        );
        Ok(())
    })
    .await
    .map_err(|_| anyhow::anyhow!("multi-edge stage deadline"))?
}
async fn run_bounded(
    fixture: &Fixture,
    binary: &Path,
    cases: &mut Vec<Case>,
    fail_after_first: bool,
) -> Result<()> {
    stale::stale_vectors(fixture, binary, cases, fail_after_first).await?;
    stale::aba(fixture, binary, cases).await?;
    stale::alternatives(fixture, binary, cases).await?;
    stale::graph_error(fixture, binary, cases).await?;
    stale::snapshot(fixture, binary, cases).await?;
    races::run(fixture, binary, cases).await?;
    retries::run(fixture, binary, cases).await?;
    Ok(())
}
