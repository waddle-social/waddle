#[tokio::test]
async fn postgres_guard_first_stage() -> anyhow::Result<()> {
    waddle_grant_fence_experiment::run(env!("CARGO_BIN_EXE_waddle-fence-worker")).await
}
