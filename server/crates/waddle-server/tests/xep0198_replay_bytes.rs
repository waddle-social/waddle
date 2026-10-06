//! Literal replay bytes across SQLite/Postgres persistence and fresh registries.
//! XEP-0198 requires retransmission; #1658 additionally promises these bytes.

pub mod ingress_support;
mod replay_bytes_support;

use ingress_support::IngressFixture;
use replay_bytes_support::ReplayBytesFixture;
use std::sync::Arc;
use waddle_server::sm_persistence::DatabaseSmPersistence;
use waddle_xmpp::stream_management::InMemorySmSessionRegistry;

async fn persisted_replay_bytes(fixture: IngressFixture) {
    let storage = Arc::new(
        DatabaseSmPersistence::open(Some(fixture.db.database_url()))
            .await
            .expect("SM storage"),
    );
    let payloads =
        ReplayBytesFixture::new("exact-replay", "alice@example.com/web".parse().unwrap());
    let registry = Arc::new(InMemorySmSessionRegistry::new().with_persistence(storage.clone()));
    payloads.store_and_append(&registry).await;
    payloads.retry_appends(&registry).await;
    drop(registry);
    for _ in 0..2 {
        let restored = Arc::new(InMemorySmSessionRegistry::new().with_persistence(storage.clone()));
        assert_eq!(
            restored
                .restore_from_persistence()
                .await
                .expect("fresh registry restore"),
            1
        );
        payloads.retry_appends(&restored).await;
        let claimed = restored
            .claim_session("exact-replay")
            .await
            .expect("resume claim")
            .expect("persisted session");
        payloads.assert_replay(&claimed);
        payloads.complete_resume_and_detach(&restored).await;
        drop(restored);
    }
    drop(storage);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_replay_preserves_production_bytes_and_original_delay() {
    persisted_replay_bytes(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_replay_preserves_production_bytes_and_original_delay() {
    let Some(fixture) = IngressFixture::postgres("replay_bytes").await else {
        return;
    };
    persisted_replay_bytes(fixture).await;
}
