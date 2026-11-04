//! The candidate UoW is executable only inside its closed disposable fixture.
#![cfg(feature = "test-support")]

#[tokio::test]
async fn restricted_relation_guarded_chain_uses_one_attested_transaction() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    waddle_server::test_support::verify_disposable_relation_guard_chain(&url)
        .await
        .expect("disposable relation-guarded chain");
}
