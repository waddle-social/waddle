#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    waddle_grant_fence_experiment::run_worker().await
}
