//! Characterizes inherited graph access during a Space parent move.

mod harness;
mod storage;

#[tokio::test]
#[ignore = "requires mandatory pinned native PostgreSQL and SpiceDB runner"]
async fn production_parent_move_revoke_before_cleanup() {
    harness::characterize(true).await;
}
#[tokio::test]
#[ignore = "requires mandatory pinned native PostgreSQL and SpiceDB runner"]
async fn production_parent_move_revoke_during_cleanup() {
    harness::characterize(false).await;
}
