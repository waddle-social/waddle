//! Disposable restricted-role PostgreSQL validation of the closed writer.
#![cfg(all(feature = "test-support", feature = "clustering"))]

#[tokio::test]
async fn recorded_room_archive_owns_its_transaction() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    waddle_server::test_support::verify_recorded_room_archive(&url)
        .await
        .expect("recorded room archive fixture");
}
